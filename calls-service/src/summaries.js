// Phase 7: meeting summaries (SDS §14, docs/meeting-summaries.md).
//
// This service holds the consent state of each call's notes and relays audio it cannot read:
//
// - only the host (the person who started an ad-hoc or 1:1 call, the scheduled meeting's host) may
//   turn notes on or off, and only when the postbox says the host's account has the feature;
// - capture runs only while *every* person in the call has explicitly agreed to the current
//   disclosure. A newcomer, a decline or a withdrawal pauses it for everyone until the people
//   present have all agreed again. Declining never removes anyone from the call;
// - each agreeing participant gets a capture credential bound to the meeting, to them and to the
//   consent version. A version change (someone joins, declines, stops for themselves) makes every
//   credential stale, so queued uploads from before the change are refused;
// - each browser encrypts its own microphone's audio to the meeting-intel service's per-meeting key
//   (signed by meeting-intel's long-term key, which the browsers pin). This service forwards the
//   ciphertext with the speaker label it authenticated; it has no key to read it.
//
// Logs carry counts and truncated ids only.

import crypto from "node:crypto";

export const SUMMARY_LIMITS = {
  audioBytes: 192 * 1024, // one encrypted ~10 s chunk is ~40 KB; this leaves room for PCM fallbacks
  chunksPerMinute: 30, // per participant
  consentsPerMinute: 20, // per participant
  requestsPerHour: 20, // per host account
  eligibilityCacheMs: 60_000,
  intelTimeoutMs: 8_000,
};

// What each person agrees to. Changing any of this text changes the hash, and a consent given to an
// older text cannot start capture.
export function disclosure(retentionDays) {
  return {
    v: 1,
    processor: "Pigeonpost's own notes service, on Pigeonpost's server. No outside AI service is used.",
    capture: "Only your own microphone, from your device, and only while everyone in the call has agreed.",
    audio: "Encrypted on your device for the notes service. Deleted as soon as it is transcribed, and never kept longer than 24 hours.",
    audience: "Members in this call who agree, including people who join later and agree. Guests can agree but don't get a copy.",
    retention: `The summary and transcript are encrypted for those members' devices and kept for ${retentionDays} days, or until they delete them.`,
    limits: "A microphone can pick up other voices nearby. Use headphones if people near you haven't agreed.",
    control: "Anyone can decline or stop notes for themselves at any time; that pauses notes for everyone.",
  };
}

export function disclosureHash(d) {
  return crypto.createHash("sha256").update(JSON.stringify(d)).digest("base64url");
}

const KEY_ID = /^[A-Za-z0-9_-]{8,64}$/;
const B64 = /^[A-Za-z0-9+/]+={0,2}$/;

export function createSummaries(ctx) {
  const { cfg, HttpError, now, liveSession, sessions, send, limited, rooms } = ctx;
  const fetchImpl = ctx.fetch || globalThis.fetch;
  const secret = crypto.randomBytes(32); // capture credentials live only as long as this process
  const eligibility = new Map(); // address -> { allowed, retentionDays, at }
  const counters = { requested: 0, refused: 0, chunks: 0, chunks_refused: 0, intel_errors: 0, finished: 0 };

  const configured = () => Boolean(cfg.intelUrl && cfg.intelToken && cfg.postboxInternalUrl && cfg.postboxServiceToken);

  async function call(base, token, method, path, body) {
    const ac = new AbortController();
    const timer = setTimeout(() => ac.abort(), SUMMARY_LIMITS.intelTimeoutMs);
    try {
      const res = await fetchImpl(base + path, {
        method, signal: ac.signal,
        headers: { authorization: "Bearer " + token, "content-type": "application/json", accept: "application/json" },
        body: body === undefined ? undefined : JSON.stringify(body),
      });
      let value = null;
      try { value = await res.json(); } catch (_) { value = null; }
      return { status: res.status, body: value };
    } finally {
      clearTimeout(timer);
    }
  }
  const postbox = (method, path, body) => call(cfg.postboxInternalUrl, cfg.postboxServiceToken, method, path, body);
  const intel = (method, path, body) => call(cfg.intelUrl, cfg.intelToken, method, path, body);

  // ---- who is who ---------------------------------------------------------------------------------

  function isHost(room, p) {
    if (!p || p.guest || !p.mailbox) return false;
    if (room.scheduled) return Boolean(p.sched && p.sched.role === "host");
    return Boolean(room.caller && room.caller.address === p.mailbox.address);
  }

  function speakerLabel(p) {
    if (p.mailbox) return p.mailbox.handle || p.mailbox.address;
    return "Guest " + p.name;
  }

  function kindOf(room) {
    if (room.scheduled) return "scheduled";
    if (room.group_id) return "group";
    if (room.targets && room.targets.length === 1 && room.participants.size <= 2) return "direct";
    return "room";
  }

  function participantOf(room, session) {
    for (const p of room.participants.values()) if (p.sessionId === session.id) return p;
    return null;
  }

  async function eligible(address) {
    const hit = eligibility.get(address);
    if (hit && now() - hit.at < SUMMARY_LIMITS.eligibilityCacheMs) return hit;
    let entry = { allowed: false, retentionDays: 0, at: now() };
    if (configured()) {
      try {
        const r = await postbox("POST", "/internal/v1/meetings", { host: address, check_only: true });
        if (r.status === 200 && r.body && r.body.allowed === true) entry = { allowed: true, retentionDays: r.body.retention_days || 90, at: now() };
      } catch (_) { /* unreachable postbox: not available */ }
    }
    eligibility.set(address, entry);
    if (eligibility.size > 5_000) eligibility.delete(eligibility.keys().next().value);
    return entry;
  }

  // ---- state --------------------------------------------------------------------------------------

  function token(s, pid) {
    return crypto.createHmac("sha256", secret).update(`${s.meeting}|${pid}|${s.version}`).digest("base64url");
  }

  // Everyone present has said yes: capture. Anyone without a yes: wait (a decline and an unanswered
  // prompt look the same to the others).
  function compute(room) {
    const s = room.summary;
    if (!s || s.state === "stopped") return false;
    const present = [...room.participants.values()];
    const all = present.length > 0 && present.every((p) => s.consents.get(p.id) === "yes");
    const next = all ? "active" : "waiting";
    if (next === s.state) return false;
    s.state = next;
    s.version++;
    return true;
  }

  function view(room, p) {
    const s = room.summary;
    const you = { participant: p.id, consent: null, can_control: isHost(room, p), can_request: Boolean(p && p.canRequest) };
    if (!s) return { room: room.id, state: "off", you };
    you.consent = s.consents.get(p.id) || null;
    const out = {
      room: room.id, state: s.state, meeting: s.meeting, version: s.version, host: s.hostLabel,
      disclosure: s.disclosure, disclosure_hash: s.hash, retention_days: s.retentionDays,
      service_key: s.serviceKey, reason: s.reason || null,
      waiting_for: s.state === "waiting" ? [...room.participants.values()].filter((q) => s.consents.get(q.id) !== "yes").length : 0,
      agreed: [...room.participants.values()].filter((q) => s.consents.get(q.id) === "yes").map((q) => q.id),
      you,
      capture: null,
    };
    if (s.state === "active" && you.consent === "yes") out.capture = { token: token(s, p.id), version: s.version };
    return out;
  }

  function publish(room) {
    for (const p of room.participants.values()) send(sessions.get(p.sessionId), "summary", view(room, p));
  }

  // ---- lifecycle hooks (called by server.js) --------------------------------------------------------

  // A person is about to be in the room. Notes pause before they receive anyone's media.
  function memberAdded(room, p) {
    const s = room.summary;
    if (s && s.state !== "stopped") {
      if (compute(room)) console.log(`calls: summary room=${room.id.slice(0, 6)} paused for a newcomer v=${s.version}`);
      publish(room);
    }
    if (!p.guest && p.mailbox && isHost(room, p) && configured()) {
      eligible(p.mailbox.address).then((e) => {
        p.canRequest = e.allowed;
        if (e.allowed && room.participants.has(p.id) && room.state !== "ended") send(sessions.get(p.sessionId), "summary", view(room, p));
      }, () => {});
    }
  }

  function memberRemoved(room, p) {
    const s = room.summary;
    if (!s || s.state === "stopped") return;
    if (isHost(room, p)) return finish(room, "host_left");
    s.consents.delete(p.id);
    compute(room);
    publish(room);
  }

  function roomEnded(room) {
    if (room.summary && room.summary.state !== "stopped") finish(room, "call_ended");
  }

  function finish(room, reason) {
    const s = room.summary;
    if (!s || s.state === "stopped") return;
    s.state = "stopped";
    s.reason = reason;
    s.version++;
    counters.finished++;
    publish(room);
    console.log(`calls: summary room=${room.id.slice(0, 6)} stopped reason=${reason} agreed=${s.ever.size}`);
    intel("POST", `/v1/sessions/${s.session}/finish`, { reason }).catch(() => { counters.intel_errors++; });
    postbox("POST", `/internal/v1/meetings/${s.meeting}/end`, { reason }).catch(() => {});
  }

  // ---- routes ---------------------------------------------------------------------------------------

  async function control(auth, room, body) {
    const session = liveSession(auth, body.session);
    const p = participantOf(room, session);
    if (!p) throw new HttpError(403, "not_in_room");
    if (!isHost(room, p)) throw new HttpError(403, "host_only");
    if (body.enabled === false) {
      if (!room.summary || room.summary.state === "stopped") return view(room, p);
      finish(room, "host_stopped");
      return view(room, p);
    }
    if (body.enabled !== true) throw new HttpError(400, "bad_request");
    if (room.state === "ended") throw new HttpError(409, "ended");
    if (room.summary && room.summary.state !== "stopped") throw new HttpError(409, "summary_active");
    if (!configured()) throw new HttpError(403, "notes_disabled");
    if (limited("summary-req:" + auth.account, SUMMARY_LIMITS.requestsPerHour, 3600_000)) throw new HttpError(429, "rate_limited");
    counters.requested++;
    const opened = await postbox("POST", "/internal/v1/meetings", {
      host: p.mailbox.address, kind: kindOf(room),
      room_ref: crypto.createHash("sha256").update(room.id).digest("hex").slice(0, 32),
      ...(room.group_id ? { group_id: room.group_id } : {}),
      ...(room.scheduled ? { calendar_event_id: room.scheduled.event_id, occurrence_id: room.scheduled.occurrence_id } : {}),
    }).catch(() => ({ status: 0 }));
    if (opened.status !== 200 || !opened.body) throw new HttpError(502, "postbox_unavailable");
    if (opened.body.allowed !== true) { counters.refused++; throw new HttpError(403, "notes_disabled"); }
    const meeting = opened.body.meeting_id;
    const sess = await intel("POST", "/v1/sessions", { meeting_id: meeting }).catch(() => ({ status: 0 }));
    if (sess.status !== 200 || !sess.body || !sess.body.session) {
      counters.intel_errors++;
      postbox("POST", `/internal/v1/meetings/${meeting}/end`, { reason: "unavailable" }).catch(() => {});
      throw new HttpError(503, sess.status === 503 ? "summary_busy" : "summary_unavailable");
    }
    if (room.state === "ended" || !room.participants.has(p.id)) {
      intel("POST", `/v1/sessions/${sess.body.session}/finish`, { reason: "call_ended" }).catch(() => {});
      throw new HttpError(409, "ended");
    }
    const retentionDays = opened.body.retention_days || 90;
    const d = disclosure(retentionDays);
    room.summary = {
      meeting, session: sess.body.session, state: "waiting", version: 1, reason: null,
      hostLabel: speakerLabel(p), disclosure: d, hash: disclosureHash(d), retentionDays,
      serviceKey: {
        session: sess.body.session, key: sess.body.key, key_id: sess.body.key_id,
        signature: sess.body.signature, expires_at: sess.body.expires_at,
      },
      consents: new Map(), ever: new Set(), seqs: new Map(), consentWrites: new Map(), consentRevisions: new Map(),
    };
    console.log(`calls: summary room=${room.id.slice(0, 6)} requested participants=${room.participants.size}`);
    publish(room);
    return view(room, p);
  }

  async function consent(auth, room, body) {
    const session = liveSession(auth, body.session);
    const p = participantOf(room, session);
    if (!p) throw new HttpError(403, "not_in_room");
    const s = room.summary;
    if (!s || s.state === "stopped") throw new HttpError(409, "notes_off");
    if (limited("summary-consent:" + p.id, SUMMARY_LIMITS.consentsPerMinute, 60_000)) throw new HttpError(429, "rate_limited");
    const yes = body.consent === true;
    if (!yes && body.consent !== false) throw new HttpError(400, "bad_request");
    const previous = s.consents.get(p.id) || null;
    if (yes) {
      // A yes counts only for the text and the situation the person actually saw.
      if (body.disclosure_hash !== s.hash) throw new HttpError(409, "consent_stale", { version: s.version });
      if (Number(body.version) !== s.version) throw new HttpError(409, "consent_stale", { version: s.version });
    }
    let key = null;
    if (yes && body.notes_key !== undefined && !p.guest) {
      const k = body.notes_key || {};
      if (!KEY_ID.test(String(k.key_id || "")) || !B64.test(String(k.public_key || "")) || String(k.public_key).length > 120) {
        throw new HttpError(400, "bad_notes_key");
      }
      key = { key_id: k.key_id, public_key: k.public_key, ...(typeof k.label === "string" ? { label: k.label.slice(0, 60) } : {}) };
    }
    // Refusal takes effect before any network wait. A slow postbox must never keep recording
    // after a participant withdraws, nor may an older yes response undo their newer decision.
    const revision = (s.consentRevisions.get(p.id) || 0) + 1;
    s.consentRevisions.set(p.id, revision);
    if (!yes) {
      s.consents.set(p.id, "no");
      compute(room);
      publish(room);
      intel("POST", `/v1/sessions/${s.session}/discard`, { participant: p.id }).catch(() => { counters.intel_errors++; });
    }
    const current = () => room.summary === s && s.state !== "stopped" && room.participants.get(p.id) === p;
    const latest = () => current() && s.consentRevisions.get(p.id) === revision;
    const record = async () => {
      if (yes && (!latest() || Number(body.version) !== s.version)) throw new HttpError(409, "consent_stale", { version: s.version });
      if (!p.guest && p.mailbox) {
        const answer = yes ? "yes" : previous === "yes" ? "withdrawn" : "no";
        const path = `/internal/v1/meetings/${s.meeting}/attendees`;
        const r = await postbox("POST", path, {
          address: p.mailbox.address, consent: answer, ...(key ? { notes_key: key } : {}),
        }).catch(() => ({ status: 0 }));
        if (r.status !== 200) {
          if (yes) throw new HttpError(r.status === 409 ? 409 : 502, r.status === 409 ? "notes_off" : "postbox_unavailable");
          console.warn(`calls: summary room=${room.id.slice(0, 6)} decline not recorded status=${r.status}`);
        }
        if (yes && latest() && Number(body.version) !== s.version) {
          // Membership changed during the write. Do not leave this rejected answer as a new
          // durable permission. This compensation stays in the same ordered write lane.
          s.consents.set(p.id, "no"); compute(room); publish(room);
          const undone = await postbox("POST", path, { address: p.mailbox.address, consent: "no" }).catch(() => ({ status: 0 }));
          if (undone.status !== 200) finish(room, "consent_unavailable");
        }
      }
      if (yes) {
        if (!latest() || Number(body.version) !== s.version) throw new HttpError(409, "consent_stale", { version: s.version });
        s.consents.set(p.id, "yes"); s.ever.add(p.id);
        compute(room); publish(room);
      }
    };
    // Keep durable answers in arrival order. A later no waits for an earlier write to settle,
    // while its local capture pause above is immediate. Failed writes do not block the next one.
    const write = (s.consentWrites.get(p.id) || Promise.resolve()).catch(() => {}).then(record);
    s.consentWrites.set(p.id, write);
    try { await write; }
    finally { if (s.consentWrites.get(p.id) === write) s.consentWrites.delete(p.id); }
    if (!current()) throw new HttpError(409, "notes_off");
    return view(room, p);
  }

  async function readBody(req) {
    const type = req.headers["content-type"] || "";
    if (!type.startsWith("application/json")) throw new HttpError(415, "json_required");
    const declared = Number(req.headers["content-length"] || 0);
    if (declared > SUMMARY_LIMITS.audioBytes) throw new HttpError(413, "too_large");
    let size = 0;
    const chunks = [];
    for await (const chunk of req) {
      size += chunk.length;
      if (size > SUMMARY_LIMITS.audioBytes) throw new HttpError(413, "too_large");
      chunks.push(chunk);
    }
    try {
      const value = JSON.parse(Buffer.concat(chunks).toString("utf8"));
      if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error();
      return value;
    } catch (_) {
      throw new HttpError(400, "bad_json");
    }
  }

  function validEnvelope(e) {
    return e && e.v === 1 && typeof e.epk === "string" && B64.test(e.epk) && e.epk.length <= 120
      && typeof e.iv === "string" && B64.test(e.iv) && e.iv.length <= 24
      && typeof e.ct === "string" && B64.test(e.ct)
      && typeof e.codec === "string" && /^[a-z0-9.;=/ -]{1,60}$/i.test(e.codec)
      && Number.isFinite(e.t) && Number.isFinite(e.dur) && e.dur > 0 && e.dur <= 30_000;
  }

  async function audio(req, auth, room, url) {
    const session = liveSession(auth, url.searchParams.get("session"));
    const p = participantOf(room, session);
    const s = room.summary;
    const seq = Number(url.searchParams.get("seq"));
    const refuse = (status, code) => { counters.chunks_refused++; return new HttpError(status, code); };
    if (!p) throw refuse(403, "not_in_room");
    if (!s || s.state !== "active") throw refuse(409, "capture_paused");
    if (s.consents.get(p.id) !== "yes") throw refuse(403, "consent_required");
    const presented = String(req.headers["x-capture-token"] || "");
    const expected = token(s, p.id);
    if (presented.length !== expected.length || !crypto.timingSafeEqual(Buffer.from(presented), Buffer.from(expected))) {
      throw refuse(409, "capture_paused");
    }
    if (!Number.isInteger(seq) || seq < 0 || seq > 1_000_000) throw refuse(400, "bad_seq");
    const last = s.seqs.has(p.id) ? s.seqs.get(p.id) : -1;
    if (seq <= last) return { accepted: true, duplicate: true };
    if (limited("summary-audio:" + p.id, SUMMARY_LIMITS.chunksPerMinute, 60_000)) throw refuse(429, "rate_limited");
    const envelope = await readBody(req);
    if (!validEnvelope(envelope)) throw refuse(400, "bad_chunk");
    // Re-check after the body arrived: a pause during the upload refuses it.
    if (room.summary !== s || s.state !== "active" || s.consents.get(p.id) !== "yes" || token(s, p.id) !== expected) {
      throw refuse(409, "capture_paused");
    }
    const r = await intel("POST", `/v1/sessions/${s.session}/chunks`, {
      participant: p.id, speaker: speakerLabel(p), guest: p.guest, seq, envelope,
    }).catch(() => ({ status: 0 }));
    if (r.status === 200) {
      s.seqs.set(p.id, seq);
      counters.chunks++;
      return { accepted: true };
    }
    counters.chunks_refused++;
    if (r.status === 503 || r.status === 429) throw new HttpError(503, "summary_busy");
    if (r.status === 409) throw new HttpError(409, "capture_paused");
    if (r.status === 400) throw new HttpError(400, "bad_chunk");
    counters.intel_errors++;
    throw new HttpError(502, "summary_unavailable");
  }

  async function route(req, auth, room, what, url) {
    if (what === "audio") return audio(req, auth, room, url);
    const body = await ctx.readJson(req);
    if (what === "consent") return consent(auth, room, body);
    return control(auth, room, body);
  }

  function health() {
    let active = 0;
    for (const r of rooms.values()) if (r.summary && r.summary.state !== "stopped" && r.state !== "ended") active++;
    return { configured: configured(), active, ...counters };
  }

  return { route, memberAdded, memberRemoved, roomEnded, view, health, isHost, _finish: finish };
}
