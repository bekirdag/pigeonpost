// Pigeonpost calls — signaling and relay credentials for web calls.
//
// What this service is: a rendezvous. It rings a mailbox, admits people who knock on a meeting link,
// carries WebRTC setup messages between browsers and mints short-lived TURN credentials. What it is
// not: a media path. Every pair of browsers in a call holds its own DTLS-SRTP keys; audio, video,
// screens and in-call chat travel between them directly or through the TURN relay, which forwards
// ciphertext. Each screen shows, per person, a security code derived from both browsers' DTLS
// fingerprints, so people can confirm that this service did not put itself in the middle.
//
// Members are authorised by the postbox, not here: every member request carries the member's own
// token, and this service asks the postbox which mailboxes that token's account owns. Guests — people
// opening a meeting link without a Pigeonpost account — get an anonymous session that can do exactly
// one thing: ask to join. Somebody already in the call decides.
//
// Everything is in memory for the life of a call. Dependency-free on purpose.

import http from "node:http";
import crypto from "node:crypto";
import { createPostboxInternal } from "./postbox-internal.js"; // Phase 1b

export function configFromEnv(env = process.env) {
  const turnUrls = (env.CALLS_TURN_URLS || "").split(/[\s,]+/).filter(Boolean);
  return {
    port: Number(env.CALLS_PORT || 8795),
    host: env.CALLS_HOST || "127.0.0.1",
    postbox: (env.CALLS_POSTBOX_URL || "https://postbox.pigeonpost.dev").replace(/\/+$/, ""),
    turnSecret: env.CALLS_TURN_SECRET || "",
    turnUrls,
    // Long enough to outlast a long call: coturn re-checks the credential on every allocation
    // refresh, so a credential that expires mid-call drops relayed media with it.
    turnTtlSeconds: Number(env.CALLS_TURN_TTL_SECONDS || 6 * 3600),
    allowedOrigins: (env.CALLS_ALLOWED_ORIGINS || "https://inbox.pigeonpost.dev").split(/[\s,]+/).filter(Boolean),
    ringMs: Number(env.CALLS_RING_MS || 45_000),
    knockMs: Number(env.CALLS_KNOCK_MS || 120_000),
    // Behind nginx (and Cloudflare in front of that), the socket address is the proxy's. Only then
    // is a forwarded client address worth believing.
    trustProxy: (env.CALLS_TRUST_PROXY || "1") === "1",
    // Phase 1b: the postbox's internal API (call admission, call history), over a private path.
    // Unset = ring without asking and report nothing, as before.
    postboxInternalUrl: env.POSTBOX_INTERNAL_URL || "",
    postboxServiceToken: env.POSTBOX_SERVICE_TOKEN || "",
  };
}

export const LIMITS = {
  bodyBytes: 128 * 1024,
  sdpChars: 100_000,
  candidateChars: 1_000,
  tokenChars: 8_192,
  nameChars: 60,
  participants: 6,
  sessionsPerAccount: 12,
  sessionsTotal: 5_000,
  roomsTotal: 2_000,
  callsPerMinute: 6,
  callsPerHour: 60,
  guestsPerIpHour: 30,
  knocksPerIpHour: 40,
  missedCodesPerIp10Min: 10,
  pendingKnocksPerRoom: 10,
  signalsPerMinute: 600,
  authCacheEntries: 10_000,
  authCacheMs: 60_000,
  heartbeatMs: 20_000,
  resumeGraceMs: 20_000,
  maxRoomMs: 8 * 3600_000,
  endedRetentionMs: 60_000,
};

const ADDRESS = /^\/[A-Za-z0-9._~\-/]{1,200}$/;
// No 0/O, 1/I/L: a meeting code is read aloud and typed from memory.
const CODE_ALPHABET = "ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const CODE = /^[A-Za-z0-9]{6}$/;
const KINDS = new Set(["audio", "video"]);
const SIGNALS = new Set(["offer", "answer", "ice", "mute"]);

class HttpError extends Error {
  constructor(status, code) {
    super(code);
    this.status = status;
    this.code = code;
  }
}

const sha256 = (s) => crypto.createHash("sha256").update(s).digest("hex");
const newId = () => crypto.randomBytes(16).toString("base64url");
const norm = (address) => String(address).toLowerCase();

function cleanName(value) {
  const name = String(value || "").normalize("NFC").replace(/[\u0000-\u001f\u007f​-‏‪-‮⁦-⁩]/g, "").replace(/\s+/g, " ").trim();
  return name.slice(0, LIMITS.nameChars);
}

export function createCallsService(cfg, deps = {}) {
  const fetchImpl = deps.fetch || globalThis.fetch;
  const now = deps.now || (() => Date.now());
  const randomCode = deps.randomCode || (() => {
    const bytes = crypto.randomBytes(6);
    return [...bytes].map((b) => CODE_ALPHABET[b % CODE_ALPHABET.length]).join("");
  });

  const authCache = new Map(); // sha256(token) -> member auth
  const guests = new Map(); // sha256(guest token) -> guest auth
  const sessions = new Map(); // session id -> session
  const sessionsByName = new Map(); // normalized address/handle -> Set<session>
  const rooms = new Map(); // room id -> room
  const roomsByCode = new Map(); // code -> room
  const buckets = new Map(); // rate-limit key -> number[]

  // ---- Phase 1b: postbox admission + call history reporting ---------------------------------------
  const internal = deps.postboxInternal || createPostboxInternal({
    url: cfg.postboxInternalUrl, token: cfg.postboxServiceToken, fetch: fetchImpl, now,
  });
  function participantName(p) {
    return p.mailbox ? p.mailbox.address : "guest:" + p.name;
  }
  // One lifecycle event for the room, with a per-room increasing sequence number.
  function reportCall(room, event, extra = {}) {
    if (!room.caller) return;
    room.seq = (room.seq || 0) + 1;
    internal.report({
      room_id: room.id, seq: room.seq, kind: room.kind, event,
      from: room.caller, callee: (room.targets && room.targets[0]) || room.dialed || undefined,
      to: room.targets || [], at: Math.floor(now() / 1000), ...extra,
    });
  }

  // ---- rate limits ------------------------------------------------------------------------------

  function limited(key, max, windowMs) {
    const t = now();
    const recent = (buckets.get(key) || []).filter((x) => t - x < windowMs);
    if (recent.length >= max) { buckets.set(key, recent); return true; }
    recent.push(t);
    buckets.set(key, recent);
    if (buckets.size > 50_000) buckets.delete(buckets.keys().next().value);
    return false;
  }

  function clientIp(req) {
    if (cfg.trustProxy) {
      const cf = req.headers["cf-connecting-ip"];
      if (typeof cf === "string" && cf.length < 64) return cf;
      const xff = req.headers["x-forwarded-for"];
      if (typeof xff === "string") return xff.split(",")[0].trim().slice(0, 64);
    }
    return req.socket.remoteAddress || "unknown";
  }

  // ---- authentication ---------------------------------------------------------------------------

  async function postboxJson(path, token) {
    const res = await fetchImpl(cfg.postbox + path, {
      headers: { authorization: "Bearer " + token, accept: "application/json" },
    });
    if (res.status === 401 || res.status === 403) throw new HttpError(401, "unauthorized");
    if (!res.ok) throw new HttpError(502, "postbox_unavailable");
    return res.json();
  }

  async function authenticate(req) {
    const header = req.headers.authorization || "";
    const guest = /^Guest ([A-Za-z0-9_-]{20,100})$/.exec(header);
    if (guest) {
      const entry = guests.get(sha256(guest[1]));
      if (!entry) throw new HttpError(401, "unauthorized");
      return entry;
    }
    const match = /^Bearer ([A-Za-z0-9._~+/=-]+)$/.exec(header);
    if (!match || match[1].length > LIMITS.tokenChars) throw new HttpError(401, "unauthorized");
    const token = match[1];
    const key = sha256(token);
    const hit = authCache.get(key);
    if (hit && hit.expires > now()) return hit;

    const { identities } = await postboxJson("/v1/identities", token);
    const list = Array.isArray(identities) ? identities.slice(0, 200) : [];
    const mailboxes = await Promise.all(list.map(async (identity) => {
      const address = String(identity.address || "");
      if (!ADDRESS.test(address)) return null;
      try {
        const who = await postboxJson("/v1/whoami?identity=" + encodeURIComponent(address), token);
        const handle = typeof who.handle === "string" && ADDRESS.test(who.handle) ? who.handle : null;
        return { address, handle };
      } catch (err) {
        if (err.code === "unauthorized") throw err;
        return { address, handle: null };
      }
    }));
    const owned = mailboxes.filter(Boolean);
    if (!owned.length) throw new HttpError(403, "no_mailbox");
    const names = new Set();
    for (const m of owned) {
      names.add(norm(m.address));
      if (m.handle) names.add(norm(m.handle));
    }
    const account = sha256(owned.map((m) => m.address).sort().join("\n")).slice(0, 32);
    const entry = { guest: false, account, mailboxes: owned, names, expires: now() + LIMITS.authCacheMs };
    if (authCache.size >= LIMITS.authCacheEntries) authCache.delete(authCache.keys().next().value);
    authCache.set(key, entry);
    return entry;
  }

  function createGuest(req) {
    const ip = clientIp(req);
    if (limited("guest:" + ip, LIMITS.guestsPerIpHour, 3600_000)) throw new HttpError(429, "rate_limited");
    const token = crypto.randomBytes(24).toString("base64url");
    const id = newId();
    guests.set(sha256(token), { guest: true, account: "guest:" + id, mailboxes: [], names: new Set(), ip, createdAt: now() });
    return { guest_token: token };
  }

  function mailboxFor(auth, address) {
    const n = norm(address);
    return auth.mailboxes.find((m) => norm(m.address) === n || (m.handle && norm(m.handle) === n)) || null;
  }

  // ---- event streams ----------------------------------------------------------------------------

  function send(session, event, data) {
    if (!session || session.closed) return false;
    try {
      session.res.write(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`);
      return true;
    } catch (_) {
      return false;
    }
  }

  function openStream(req, res, auth) {
    let accountSessions = 0;
    for (const s of sessions.values()) if (s.account === auth.account) accountSessions++;
    if (accountSessions >= LIMITS.sessionsPerAccount) throw new HttpError(429, "too_many_sessions");
    if (sessions.size >= LIMITS.sessionsTotal) throw new HttpError(503, "busy");

    res.writeHead(200, {
      "content-type": "text/event-stream; charset=utf-8",
      "cache-control": "no-store",
      "x-accel-buffering": "no",
      connection: "keep-alive",
    });
    const session = { id: newId(), account: auth.account, guest: auth.guest, names: auth.names, mailboxes: auth.mailboxes, res, closed: false };
    sessions.set(session.id, session);
    for (const name of session.names) {
      if (!sessionsByName.has(name)) sessionsByName.set(name, new Set());
      sessionsByName.get(name).add(session);
    }
    send(session, "ready", { session: session.id, mailboxes: auth.mailboxes, guest: auth.guest });
    const beat = setInterval(() => {
      try { res.write(": keepalive\n\n"); } catch (_) { /* closed below */ }
    }, LIMITS.heartbeatMs);
    beat.unref?.();
    const close = () => {
      if (session.closed) return;
      session.closed = true;
      clearInterval(beat);
      sessions.delete(session.id);
      for (const name of session.names) {
        const set = sessionsByName.get(name);
        if (set) { set.delete(session); if (!set.size) sessionsByName.delete(name); }
      }
      sessionGone(session);
    };
    req.on("close", close);
    res.on("error", close);
  }

  function liveSession(auth, id) {
    const session = sessions.get(String(id || ""));
    if (!session || session.closed || session.account !== auth.account) throw new HttpError(409, "no_session");
    return session;
  }

  // ---- rooms ------------------------------------------------------------------------------------

  function participantView(p) {
    return { participant: p.id, name: p.name, mailbox: p.mailbox, guest: p.guest };
  }

  function roomView(room, you) {
    return {
      room: room.id, code: room.code, kind: room.kind, state: room.state,
      you: you ? you.id : undefined,
      participants: [...room.participants.values()].map(participantView),
    };
  }

  function broadcast(room, event, data, exceptPid) {
    for (const p of room.participants.values()) {
      if (p.id !== exceptPid) send(sessions.get(p.sessionId), event, data);
    }
  }

  function newCode() {
    for (let i = 0; i < 20; i++) {
      const code = randomCode();
      if (!roomsByCode.has(code)) return code;
    }
    throw new HttpError(503, "busy");
  }

  function makeRoom(kind) {
    if (rooms.size >= LIMITS.roomsTotal) throw new HttpError(503, "busy");
    const room = {
      id: newId(), code: newCode(), kind, state: "ringing", createdAt: now(),
      participants: new Map(), knocks: new Map(), ring: null, timers: [],
    };
    rooms.set(room.id, room);
    roomsByCode.set(room.code, room);
    const cap = setTimeout(() => endRoom(room, "max_duration"), LIMITS.maxRoomMs);
    cap.unref?.();
    room.timers.push(cap);
    return room;
  }

  function addParticipant(room, session, { name, mailbox, guest }) {
    if (room.participants.size >= LIMITS.participants) throw new HttpError(409, "room_full");
    const p = { id: newId(), sessionId: session.id, account: session.account, name, mailbox: mailbox || null, guest: Boolean(guest), joinedAt: now(), grace: null };
    room.participants.set(p.id, p);
    return p;
  }

  function participantFor(room, auth, sessionId) {
    const session = liveSession(auth, sessionId);
    for (const p of room.participants.values()) if (p.sessionId === session.id) return p;
    throw new HttpError(403, "not_in_room");
  }

  function sessionInRoom(session) {
    for (const room of rooms.values()) {
      if (room.state === "ended") continue;
      for (const p of room.participants.values()) if (p.sessionId === session.id) return room;
    }
    return null;
  }

  function mailboxEngaged(name) {
    for (const room of rooms.values()) {
      if (room.state === "ended") continue;
      for (const p of room.participants.values()) {
        if (p.mailbox && (norm(p.mailbox.address) === name || (p.mailbox.handle && norm(p.mailbox.handle) === name))) return true;
      }
    }
    return false;
  }

  function roomFor(id) {
    const room = rooms.get(String(id || ""));
    if (!room || room.state === "ended") throw new HttpError(404, "no_call");
    return room;
  }

  // A ring: a member calls one mailbox from one of theirs. The room exists from the first ring, so
  // its meeting link can be shared before anybody answers.
  async function startCall(auth, body) {
    if (auth.guest) throw new HttpError(403, "members_only");
    const session = liveSession(auth, body.session);
    const kind = KINDS.has(body.kind) ? body.kind : null;
    if (!kind) throw new HttpError(400, "bad_kind");
    const from = mailboxFor(auth, body.from || "");
    if (!from) throw new HttpError(403, "not_your_mailbox");
    const to = String(body.to || "");
    if (!ADDRESS.test(to)) throw new HttpError(400, "bad_recipient");
    const own = mailboxFor(auth, to);
    if (own && norm(own.address) === norm(from.address)) throw new HttpError(400, "cannot_call_self");
    if (sessionInRoom(session)) throw new HttpError(409, "already_in_call");
    if (limited("calls-min:" + auth.account, LIMITS.callsPerMinute, 60_000) || limited("calls-hour:" + auth.account, LIMITS.callsPerHour, 3600_000)) {
      throw new HttpError(429, "rate_limited");
    }

    // Phase 1b: ask the postbox before anything rings. block and silence look to the caller exactly
    // like nobody answering.
    const admission = await internal.admit({ from, to, kind });
    if (session.closed || sessionInRoom(session)) throw new HttpError(409, "already_in_call");

    const room = makeRoom(kind);
    room.caller = { address: from.address, ...(from.handle ? { handle: from.handle } : {}) };
    room.targets = admission.targets.length ? admission.targets : [to];
    const caller = addParticipant(room, session, { name: from.handle || from.address, mailbox: from });
    ring(room, caller, to, admission.decision);
    return roomView(room, caller);
  }

  // Busy and offline both look like an unanswered call from the outside: the caller learns nothing
  // about whether the other mailbox is online until somebody there picks up.
  function ring(room, caller, to, decision = "ring") {
    const toName = norm(to);
    room.ring = { to, toName, from: caller.mailbox, ringing: new Set(), decision: decision === "ring" ? "ring" : "quiet" };
    // Phase 1b: `silence` files a filtered entry for the callee; `block` records nothing about them.
    // Either way nothing rings and the timer below gives the caller the normal no-answer.
    // The caller's own entry is written the same way in all three cases (ring, then missed), so
    // their history cannot tell a block or a silence from nobody answering.
    if (decision === "ring") reportCall(room, "ring");
    else reportCall(room, "ring", { to: [] });
    if (decision === "silence") reportCall(room, "filtered");
    if (decision === "ring" && !mailboxEngaged(toName)) {
      for (const target of sessionsByName.get(toName) || []) {
        if (target.id === caller.sessionId || sessionInRoom(target)) continue;
        room.ring.ringing.add(target.id);
        send(target, "incoming", { room: room.id, code: room.code, kind: room.kind, from: caller.mailbox, to });
      }
    }
    const t = setTimeout(() => {
      if (room.state !== "ringing") return;
      if (room.ring) reportCall(room, "missed", room.ring.decision === "quiet" ? { to: [] } : {}); // Phase 1b
      stopRinging(room, "no_answer");
      if (room.participants.size < 2) endRoom(room, "no_answer");
    }, cfg.ringMs);
    t.unref?.();
    room.ring.timer = t;
  }

  function stopRinging(room, reason, exceptSession) {
    if (!room.ring) return;
    clearTimeout(room.ring.timer);
    for (const id of room.ring.ringing) if (id !== exceptSession) send(sessions.get(id), "ring_ended", { room: room.id, reason });
    room.ring = null;
  }

  function accept(auth, room, body) {
    if (auth.guest) throw new HttpError(403, "members_only");
    const session = liveSession(auth, body.session);
    if (!room.ring || !room.ring.ringing.has(session.id)) throw new HttpError(409, "not_ringing");
    const callee = mailboxFor(auth, room.ring.to);
    if (!callee) throw new HttpError(403, "not_your_call");
    if (sessionInRoom(session)) throw new HttpError(409, "already_in_call");
    stopRinging(room, "answered_elsewhere", session.id);
    const p = addParticipant(room, session, { name: callee.handle || callee.address, mailbox: callee });
    room.state = "active";
    reportCall(room, "answer", { by: callee.address, participants: [...room.participants.values()].map(participantName) }); // Phase 1b
    admitted(room, p);
    return roomView(room, p);
  }

  function decline(auth, room, body) {
    const session = liveSession(auth, body.session);
    if (!room.ring || !room.ring.ringing.has(session.id)) throw new HttpError(409, "not_ringing");
    const reason = body.reason === "busy" ? "busy" : "declined";
    room.ring.ringing.delete(session.id);
    const decliner = mailboxFor(auth, room.ring.to);
    reportCall(room, reason === "busy" ? "busy" : "decline", decliner ? { by: decliner.address } : {}); // Phase 1b
    // Another device of the same mailbox may still answer; the call only ends when nothing rings.
    if (!room.ring.ringing.size || reason === "declined") {
      stopRinging(room, reason, session.id);
      if (room.participants.size < 2) endRoom(room, reason, null);
      else broadcast(room, "ring_ended", { room: room.id, reason });
    }
    return { room: room.id, state: room.state };
  }

  function admitted(room, p) {
    // The newcomer offers to everyone already present; everyone else just answers. One rule, so
    // no pair ever negotiates twice at the same time.
    broadcast(room, "participant_joined", { room: room.id, ...participantView(p) }, p.id);
    send(sessions.get(p.sessionId), "joined", roomView(room, p));
  }

  function leave(auth, room, body) {
    const session = liveSession(auth, body.session);
    let found = null;
    for (const p of room.participants.values()) if (p.sessionId === session.id) found = p;
    if (!found) throw new HttpError(403, "not_in_room");
    removeParticipant(room, found, "left");
    return { room: room.id, state: room.state };
  }

  function removeParticipant(room, p, reason) {
    clearTimeout(p.grace);
    room.participants.delete(p.id);
    broadcast(room, "participant_left", { room: room.id, participant: p.id, reason });
    if (room.state === "ringing" && room.participants.size === 0) {
      reportCall(room, "cancelled", room.ring && room.ring.decision === "quiet" ? { to: [] } : {}); // Phase 1b
      stopRinging(room, "cancelled");
      endRoom(room, "cancelled");
    } else if (room.state !== "ringing" && room.participants.size < 2 && !room.knocks.size) {
      endRoom(room, "ended");
    }
  }

  function endRoom(room, reason, exceptSession) {
    if (room.state === "ended") return;
    // Phase 1b: an answered call's end; unanswered outcomes were reported where they happened.
    if (room.state === "active") reportCall(room, "end", { participants: [...room.participants.values()].map(participantName) });
    room.state = "ended";
    for (const t of room.timers) clearTimeout(t);
    stopRinging(room, reason);
    for (const p of room.participants.values()) {
      clearTimeout(p.grace);
      if (p.sessionId !== exceptSession) send(sessions.get(p.sessionId), "ended", { room: room.id, reason });
    }
    for (const k of room.knocks.values()) {
      clearTimeout(k.timer);
      send(sessions.get(k.sessionId), "knock_result", { room: room.id, knock: k.id, admitted: false, reason: "ended" });
    }
    room.knocks.clear();
    roomsByCode.delete(room.code);
    const cleanup = setTimeout(() => rooms.delete(room.id), LIMITS.endedRetentionMs);
    cleanup.unref?.();
  }

  // ---- meeting links ----------------------------------------------------------------------------

  function lookup(req, codeValue) {
    const code = String(codeValue || "").toUpperCase();
    const room = CODE.test(code) ? roomsByCode.get(code) : null;
    if (!room || room.state === "ended") {
      if (limited("miss:" + clientIp(req), LIMITS.missedCodesPerIp10Min, 600_000)) throw new HttpError(429, "rate_limited");
      throw new HttpError(404, "no_meeting");
    }
    return room;
  }

  function meetingInfo(req, code) {
    const room = lookup(req, code);
    return { code: room.code, kind: room.kind, participants: room.participants.size, full: room.participants.size >= LIMITS.participants };
  }

  function knock(req, auth, body) {
    const session = liveSession(auth, body.session);
    const room = lookup(req, body.code);
    if (limited("knock:" + clientIp(req), LIMITS.knocksPerIpHour, 3600_000)) throw new HttpError(429, "rate_limited");
    if (sessionInRoom(session)) throw new HttpError(409, "already_in_call");
    if (room.participants.size >= LIMITS.participants) throw new HttpError(409, "room_full");
    if (room.knocks.size >= LIMITS.pendingKnocksPerRoom) throw new HttpError(429, "too_many_requests");
    for (const k of room.knocks.values()) if (k.sessionId === session.id) throw new HttpError(409, "already_waiting");
    // A signed-in member knocks as one of their own mailboxes, which everyone inside sees as
    // verified. A guest's name is only what they typed, and is shown as such.
    let mailbox = null, name;
    if (!auth.guest && body.as) {
      mailbox = mailboxFor(auth, body.as);
      if (!mailbox) throw new HttpError(403, "not_your_mailbox");
      name = mailbox.handle || mailbox.address;
    } else {
      name = cleanName(body.name);
      if (!name) throw new HttpError(400, "name_required");
    }
    const k = { id: newId(), sessionId: session.id, account: auth.account, name, mailbox, guest: auth.guest, at: now(), timer: null };
    room.knocks.set(k.id, k);
    k.timer = setTimeout(() => resolveKnock(room, k, false, "timeout"), cfg.knockMs);
    k.timer.unref?.();
    broadcast(room, "knock", { room: room.id, knock: k.id, name: k.name, mailbox: k.mailbox, guest: k.guest });
    return { room: room.id, knock: k.id, state: "waiting" };
  }

  function answerKnock(auth, room, knockId, body) {
    const decider = participantFor(room, auth, body.session);
    const k = room.knocks.get(String(knockId || ""));
    if (!k) throw new HttpError(404, "no_request");
    resolveKnock(room, k, body.admit === true, body.admit === true ? "admitted" : "denied", decider);
    return { ok: true };
  }

  function resolveKnock(room, k, admit, reason, decider) {
    if (!room.knocks.has(k.id)) return;
    room.knocks.delete(k.id);
    clearTimeout(k.timer);
    broadcast(room, "knock_resolved", { room: room.id, knock: k.id, admitted: admit, by: decider ? decider.id : null, reason });
    const knocker = sessions.get(k.sessionId);
    if (!admit || !knocker || room.state === "ended") {
      send(knocker, "knock_result", { room: room.id, knock: k.id, admitted: false, reason });
      if (room.state !== "ringing" && room.participants.size < 2 && !room.knocks.size) endRoom(room, "ended");
      return;
    }
    if (room.participants.size >= LIMITS.participants) {
      send(knocker, "knock_result", { room: room.id, knock: k.id, admitted: false, reason: "room_full" });
      return;
    }
    const p = addParticipant(room, knocker, { name: k.name, mailbox: k.mailbox, guest: k.guest });
    if (room.state === "ringing" && room.participants.size >= 2) room.state = "active";
    if (k.mailbox) reportCall(room, "join", { by: k.mailbox.address, participants: [...room.participants.values()].map(participantName) }); // Phase 1b
    send(knocker, "knock_result", { room: room.id, knock: k.id, admitted: true });
    admitted(room, p);
  }

  function withdrawKnock(auth, body) {
    const session = liveSession(auth, body.session);
    for (const room of rooms.values()) {
      for (const k of room.knocks.values()) {
        if (k.sessionId === session.id && k.id === body.knock) {
          resolveKnock(room, k, false, "withdrawn");
          return { ok: true };
        }
      }
    }
    throw new HttpError(404, "no_request");
  }

  // ---- signals between two participants ---------------------------------------------------------

  function signal(auth, room, body) {
    const from = participantFor(room, auth, body.session);
    const to = room.participants.get(String(body.to || ""));
    if (!to || to.id === from.id) throw new HttpError(404, "no_participant");
    if (!SIGNALS.has(body.type)) throw new HttpError(400, "bad_signal");
    const data = validSignal(body.type, body.data);
    if (limited("sig:" + from.id, LIMITS.signalsPerMinute, 60_000)) throw new HttpError(429, "rate_limited");
    if (!send(sessions.get(to.sessionId), "signal", { room: room.id, from: from.id, type: body.type, data })) {
      throw new HttpError(409, "peer_unreachable");
    }
    return { ok: true };
  }

  function validSignal(type, data) {
    // Anyone in a call may mute anyone else, as in every meeting app. Only the person can unmute.
    if (type === "mute") return null;
    if (type === "offer" || type === "answer") {
      if (!data || data.type !== type || typeof data.sdp !== "string" || data.sdp.length > LIMITS.sdpChars) {
        throw new HttpError(400, "bad_signal");
      }
      return { type, sdp: data.sdp };
    }
    if (data === null) return null;
    if (!data || typeof data.candidate !== "string" || data.candidate.length > LIMITS.candidateChars) {
      throw new HttpError(400, "bad_signal");
    }
    const out = { candidate: data.candidate };
    if (typeof data.sdpMid === "string" && data.sdpMid.length <= 64) out.sdpMid = data.sdpMid;
    if (Number.isInteger(data.sdpMLineIndex) && data.sdpMLineIndex >= 0 && data.sdpMLineIndex < 64) out.sdpMLineIndex = data.sdpMLineIndex;
    return out;
  }

  // ---- connection reports -----------------------------------------------------------------------

  // How a pair of browsers fared, for the service log: an outcome and candidate counts. No
  // addresses or descriptions are accepted, so none can be logged.
  function report(auth, room, body) {
    const from = participantFor(room, auth, body.session);
    const to = room.participants.get(String(body.to || ""));
    if (!to) throw new HttpError(404, "no_participant");
    if (limited("report:" + from.id, 30, 60_000)) throw new HttpError(429, "rate_limited");
    const counts = (o) => ["host", "srflx", "prflx", "relay"].map((k) => k[0] + Math.min(99, Number((o || {})[k]) || 0)).join("");
    const state = ["connected", "stalled", "failed"].includes(body.state) ? body.state : "other";
    const route = ["relay", "direct"].includes(body.route) ? body.route : "-";
    console.log(`calls: pair room=${room.id.slice(0, 6)} ${from.id.slice(0, 6)}>${to.id.slice(0, 6)} size=${room.participants.size} state=${state} route=${route} relay_only=${body.relay_only === true} restarts=${Math.min(9, Number(body.restarts) || 0)} local=${counts(body.local)} remote=${counts(body.remote)}`);
    return { ok: true };
  }

  // ---- reconnecting -------------------------------------------------------------------------------

  // A page reload or a dropped event stream gives the browser a new session. Media between browsers
  // does not depend on this service, so a participant stays in the room for a grace period and the
  // same account may take its place back.
  function resume(auth, room, body) {
    const session = liveSession(auth, body.session);
    const p = room.participants.get(String(body.participant || ""));
    if (!p || p.account !== auth.account) throw new HttpError(403, "not_in_room");
    p.sessionId = session.id;
    clearTimeout(p.grace);
    p.grace = null;
    return roomView(room, p);
  }

  function sessionGone(session) {
    for (const room of rooms.values()) {
      if (room.state === "ended") continue;
      if (room.ring && room.ring.ringing.delete(session.id) && !room.ring.ringing.size && room.state === "ringing") {
        // Every ringing device went away: let it time out as an unanswered call.
      }
      for (const k of [...room.knocks.values()]) if (k.sessionId === session.id) resolveKnock(room, k, false, "withdrawn");
      for (const p of [...room.participants.values()]) {
        if (p.sessionId !== session.id) continue;
        if (room.state === "ringing") { removeParticipant(room, p, "left"); continue; }
        clearTimeout(p.grace);
        p.grace = setTimeout(() => {
          if (!sessions.has(p.sessionId) && room.participants.has(p.id)) removeParticipant(room, p, "disconnected");
        }, LIMITS.resumeGraceMs);
        p.grace.unref?.();
      }
    }
  }

  // ---- TURN credentials (coturn "use-auth-secret": HMAC-SHA1 over "<expiry>:<user>") -------------

  function turnCredentials(auth, sessionId) {
    const session = liveSession(auth, sessionId);
    // A guest gets relay credentials only once somebody has let them in.
    if (auth.guest && !sessionInRoom(session)) throw new HttpError(403, "not_in_room");
    if (!cfg.turnSecret || !cfg.turnUrls.length) return { iceServers: [], ttl: 0 };
    const expires = Math.floor(now() / 1000) + cfg.turnTtlSeconds;
    // Unique per issue: coturn's per-user allocation quota then applies per connection set-up rather
    // than per account, so one person in a six-way call cannot exhaust it on their own.
    const username = `${expires}:${sha256(auth.account).slice(0, 12)}.${crypto.randomBytes(4).toString("hex")}`;
    const credential = crypto.createHmac("sha1", cfg.turnSecret).update(username).digest("base64");
    return { iceServers: [{ urls: cfg.turnUrls, username, credential }], ttl: cfg.turnTtlSeconds };
  }

  // ---- HTTP ---------------------------------------------------------------------------------------

  async function readJson(req) {
    const type = req.headers["content-type"] || "";
    if (!type.startsWith("application/json")) throw new HttpError(415, "json_required");
    let size = 0;
    const chunks = [];
    for await (const chunk of req) {
      size += chunk.length;
      if (size > LIMITS.bodyBytes) throw new HttpError(413, "too_large");
      chunks.push(chunk);
    }
    try {
      const value = JSON.parse(Buffer.concat(chunks).toString("utf8") || "{}");
      if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error();
      return value;
    } catch (_) {
      throw new HttpError(400, "bad_json");
    }
  }

  function json(res, status, value) {
    res.writeHead(status, { "content-type": "application/json", "cache-control": "no-store" });
    res.end(JSON.stringify(value));
  }

  async function handle(req, res) {
    const url = new URL(req.url, "http://calls.local");
    const path = url.pathname.replace(/^\/calls(?=\/)/, "");
    try {
      if (req.method === "GET" && path === "/v1/health") {
        return json(res, 200, { ok: true, sessions: sessions.size, rooms: rooms.size, postbox: internal.health() });
      }
      if (req.method === "POST") {
        const origin = req.headers.origin;
        if (origin && !cfg.allowedOrigins.includes(origin)) throw new HttpError(403, "bad_origin");
      }
      if (req.method === "POST" && path === "/v1/guests") {
        await readJson(req);
        return json(res, 200, createGuest(req));
      }
      const meeting = /^\/v1\/meetings\/([A-Za-z0-9]{6})$/.exec(path);
      if (req.method === "GET" && meeting) return json(res, 200, meetingInfo(req, meeting[1]));

      const auth = await authenticate(req);
      if (req.method === "GET" && path === "/v1/events") return openStream(req, res, auth);
      if (req.method === "GET" && path === "/v1/turn") return json(res, 200, turnCredentials(auth, url.searchParams.get("session")));
      if (req.method === "POST" && path === "/v1/calls") return json(res, 200, await startCall(auth, await readJson(req)));
      if (req.method === "POST" && path === "/v1/knocks") return json(res, 200, knock(req, auth, await readJson(req)));
      if (req.method === "POST" && path === "/v1/knocks/withdraw") return json(res, 200, withdrawKnock(auth, await readJson(req)));
      const knockAnswer = /^\/v1\/rooms\/([A-Za-z0-9_-]{10,64})\/knocks\/([A-Za-z0-9_-]{10,64})$/.exec(path);
      if (req.method === "POST" && knockAnswer) {
        const body = await readJson(req);
        return json(res, 200, answerKnock(auth, roomFor(knockAnswer[1]), knockAnswer[2], body));
      }
      const m = /^\/v1\/rooms\/([A-Za-z0-9_-]{10,64})\/(accept|decline|leave|signal|resume|report)$/.exec(path);
      if (req.method === "POST" && m) {
        const body = await readJson(req);
        const room = roomFor(m[1]);
        const action = { accept, decline, leave, signal, resume, report }[m[2]];
        return json(res, 200, action(auth, room, body));
      }
      throw new HttpError(404, "not_found");
    } catch (err) {
      if (!(err instanceof HttpError)) {
        console.error("calls: unexpected error", err && err.name);
        err = new HttpError(500, "internal");
      }
      if (!res.headersSent) json(res, err.status, { error: err.code });
      else res.end();
    }
  }

  return { handle, state: { sessions, rooms, roomsByCode, authCache, guests }, internal };
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const cfg = configFromEnv();
  if (!cfg.turnSecret || !cfg.turnUrls.length) console.warn("calls: TURN is not configured; calls will be direct only");
  const service = createCallsService(cfg);
  const server = http.createServer((req, res) => service.handle(req, res));
  server.headersTimeout = 15_000;
  server.requestTimeout = 0; // event streams stay open
  server.listen(cfg.port, cfg.host, () => console.log(`calls: listening on ${cfg.host}:${cfg.port}`));
  const stop = () => server.close(() => process.exit(0));
  process.on("SIGTERM", stop);
  process.on("SIGINT", stop);
}
