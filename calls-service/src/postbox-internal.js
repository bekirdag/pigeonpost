// Pigeonpost calls — the postbox's internal API (Phase 1b, SDS §12.2, §13.3).
//
// Two things this service now asks of the postbox, over a private path and with its own service
// token (`svc_…`), never with a member's token:
//
//   * call admission — before ringing anyone, whether the callee's settings say ring, silence or
//     block. Cached for 30 s per (caller, callee, kind). If the postbox is configured but cannot be
//     asked, admission fails closed: the call behaves exactly like an unanswered one.
//   * lifecycle events — ring, answer, decline, missed, end … so each participant's call history can
//     be written. Sent through a bounded retry queue with stable event ids, so a retried event is
//     deduplicated by the postbox. With `spoolPath` (CALLS_EVENT_SPOOL, Phase 9) the queue is also
//     kept in one 0600 JSON file, rewritten shortly after each change and on shutdown, and reloaded
//     at start, so a restart no longer loses call history. Entries older than the give-up window
//     are discarded on load; without a spool path nothing is written to disk.
//
// Logs carry counts and outcomes only: no tokens, no addresses.

import crypto from "node:crypto";
import fs from "node:fs";

export const INTERNAL_LIMITS = {
  admissionTimeoutMs: 3_000,
  admissionCacheMs: 30_000,
  admissionCacheEntries: 5_000,
  queueMax: 1_000,
  reportTimeoutMs: 5_000,
  backoffBaseMs: 1_000,
  backoffMaxMs: 60_000,
  maxAttempts: 12,
  giveUpAfterMs: 30 * 60_000,
};

const DECISIONS = new Set(["ring", "silence", "block"]);
const ADDRESS = /^\/[A-Za-z0-9._~\-/]{1,200}$/;
// Phase 3B: scheduled meetings.
const MEETING_ID = /^mtg_[A-Za-z0-9]{1,64}$/;
const EVENT_ID = /^evt_[A-Za-z0-9]{1,64}$/;
// The postbox's occurrence ids are `occ_<event suffix>_<recurrence key>` (docs/calendar.md §1).
const OCCURRENCE_ID = /^occ_[A-Za-z0-9_]{1,80}$/;
const ATTENDEE_ID = /^att_[A-Za-z0-9]{1,64}$/;
const MEETING_CODE = /^[ABCDEFGHJKMNPQRSTUVWXYZ23456789]{10}$/;
const MEETING_ROLES = new Set(["host", "cohost", "attendee", "guest", "none"]);
const MEETING_ADMISSIONS = new Set(["direct", "knock", "deny"]);
const MEETING_REASONS = new Set(["unavailable", "window_closed", "not_invited", "declined", "cancelled", "ticket_invalid", "choose_occurrence"]);

export function createPostboxInternal({ url, token, fetch, now, setTimeout: setTimer, limits, spoolPath } = {}) {
  const L = { ...INTERNAL_LIMITS, ...(limits || {}) };
  const base = (url || "").replace(/\/+$/, "");
  const configured = Boolean(base && token);
  const fetchImpl = fetch || globalThis.fetch;
  const clock = now || (() => Date.now());
  const schedule = setTimer || globalThis.setTimeout;

  const counters = { sent: 0, retried: 0, dropped: 0, rejected: 0, failed: 0, admission_ok: 0, admission_failed: 0, cache_hits: 0 };
  const cache = new Map(); // key -> { value, expires }
  const queue = []; // pending events: { body, attempts, firstAt, nextAt }
  let timer = null;
  let flushing = false;

  // ---- optional spool (Phase 9) --------------------------------------------------------------------
  const spool = configured && spoolPath ? spoolPath : "";
  let spoolTimer = null;

  function persist() {
    if (!spool) return;
    if (spoolTimer) { globalThis.clearTimeout(spoolTimer); spoolTimer = null; }
    const tmp = `${spool}.tmp`;
    try {
      const data = JSON.stringify(queue.map(({ body, attempts, firstAt }) => ({ body, attempts, firstAt })));
      fs.writeFileSync(tmp, data, { mode: 0o600 });
      fs.renameSync(tmp, spool);
    } catch (e) {
      counters.spool_errors = (counters.spool_errors || 0) + 1;
      console.warn(`calls: call-event spool write failed (${e.code || "error"})`);
    }
  }

  function persistSoon() {
    if (!spool || spoolTimer) return;
    spoolTimer = globalThis.setTimeout(() => { spoolTimer = null; persist(); }, 250);
    spoolTimer.unref?.();
  }

  if (spool) {
    try {
      const saved = JSON.parse(fs.readFileSync(spool, "utf8"));
      const t = clock();
      for (const item of Array.isArray(saved) ? saved : []) {
        const ok = item && typeof item.body === "object" && item.body && typeof item.body.event_id === "string"
          && /^[a-f0-9]{32}$/.test(item.body.event_id) && Number.isFinite(item.firstAt) && Number.isInteger(item.attempts)
          && t - item.firstAt < L.giveUpAfterMs;
        if (ok && queue.length < L.queueMax) queue.push({ body: item.body, attempts: item.attempts, firstAt: item.firstAt, nextAt: t });
      }
      if (queue.length) {
        counters.restored = queue.length;
        console.log(`calls: ${queue.length} call events restored from the spool`);
      }
    } catch (e) {
      if (e.code !== "ENOENT") console.warn(`calls: call-event spool unreadable (${e.code || "parse"}); starting empty`);
    }
  }

  async function post(path, body, timeoutMs) {
    const ac = new AbortController();
    const t = globalThis.setTimeout(() => ac.abort(), timeoutMs);
    t.unref?.();
    try {
      return await fetchImpl(base + path, {
        method: "POST",
        headers: { authorization: "Bearer " + token, "content-type": "application/json", accept: "application/json" },
        body: JSON.stringify(body),
        signal: ac.signal,
      });
    } finally {
      clearTimeout(t);
    }
  }

  // ---- admission --------------------------------------------------------------------------------

  function validAdmission(v) {
    if (!v || typeof v !== "object" || !DECISIONS.has(v.decision) || !Array.isArray(v.targets)) return null;
    const targets = v.targets.filter((x) => typeof x === "string" && ADDRESS.test(x)).slice(0, 50);
    return { decision: v.decision, targets, reason: typeof v.reason === "string" ? v.reason.slice(0, 64) : "" };
  }

  async function admit({ from, to, kind }) {
    if (!configured) return { decision: "ring", targets: [], reason: "unconfigured" };
    const key = [String(from && from.address || "").toLowerCase(), String(to || "").toLowerCase(), kind].join("\n");
    const hit = cache.get(key);
    if (hit && hit.expires > clock()) {
      counters.cache_hits++;
      return hit.value;
    }
    if (hit) cache.delete(key);
    let value = null;
    try {
      const res = await post("/internal/v1/call-admission", {
        from: { mailbox: from.address, ...(from.handle ? { handle: from.handle } : {}) },
        to, kind,
      }, L.admissionTimeoutMs);
      if (res.ok) value = validAdmission(await res.json());
    } catch (_) {
      value = null;
    }
    if (!value) {
      counters.admission_failed++;
      console.warn(`calls: admission unavailable; failing closed (failures=${counters.admission_failed})`);
      return { decision: "block", targets: [], reason: "admission_unavailable" };
    }
    counters.admission_ok++;
    if (cache.size >= L.admissionCacheEntries) cache.delete(cache.keys().next().value);
    cache.set(key, { value, expires: clock() + L.admissionCacheMs });
    return value;
  }

  // ---- Phase 3a: group call admission --------------------------------------------------------------
  //
  // `POST /internal/v1/group-call-admission {group_id, from:{mailbox}, kind, join}` →
  // `{allowed, reason?, group_name, members:[{address, handle?, decision}]}` (members exclude the
  // caller). Returns null when unconfigured or unavailable: group calls then fail closed.

  function validGroupAdmission(v) {
    if (!v || typeof v !== "object" || typeof v.allowed !== "boolean") return null;
    const members = (Array.isArray(v.members) ? v.members : []).slice(0, 10_000).flatMap((m) => {
      if (!m || typeof m !== "object" || typeof m.address !== "string" || !ADDRESS.test(m.address) || !DECISIONS.has(m.decision)) return [];
      const handle = typeof m.handle === "string" && ADDRESS.test(m.handle) ? m.handle : null;
      return [{ address: m.address, handle, decision: m.decision }];
    });
    return {
      allowed: v.allowed,
      reason: typeof v.reason === "string" ? v.reason.slice(0, 64) : "",
      group_name: typeof v.group_name === "string" ? v.group_name.slice(0, 200) : "",
      members,
    };
  }

  async function groupAdmit({ group_id, from, kind, join }) {
    if (!configured) return null;
    const cacheable = Boolean(join);
    const key = ["group", group_id, String(from && from.address || "").toLowerCase(), join ? "join" : "start"].join("\n");
    if (cacheable) {
      const hit = cache.get(key);
      if (hit && hit.expires > clock()) {
        counters.cache_hits++;
        return hit.value;
      }
      if (hit) cache.delete(key);
    }
    let value = null;
    try {
      const res = await post("/internal/v1/group-call-admission", {
        group_id, from: { mailbox: from.address }, kind: kind || "audio", join: Boolean(join),
      }, L.admissionTimeoutMs);
      if (res.ok) value = validGroupAdmission(await res.json());
    } catch (_) {
      value = null;
    }
    if (!value) {
      counters.admission_failed++;
      console.warn(`calls: group admission unavailable; failing closed (failures=${counters.admission_failed})`);
      return null;
    }
    counters.admission_ok++;
    if (cacheable) {
      if (cache.size >= L.admissionCacheEntries) cache.delete(cache.keys().next().value);
      cache.set(key, { value, expires: clock() + L.admissionCacheMs });
    }
    return value;
  }

  // ---- Phase 3B: scheduled meeting access (docs/calendar.md §8) -----------------------------------
  //
  // `POST /internal/v1/calendar/meeting-access {code, occurrence_id?, actor, purpose, extend_minutes?}`.
  // Never cached: every join, recheck and extension asks again. Returns null when unconfigured,
  // unreachable or the answer does not validate; callers fail closed (join) or keep the status quo
  // (recheck).

  function validMeetingAccess(v) {
    if (!v || typeof v !== "object" || typeof v.allowed !== "boolean") return null;
    const time = (x) => (Number.isInteger(x) && x >= 0 && x <= 1e11 ? x : 0);
    const occurrences = (Array.isArray(v.occurrences) ? v.occurrences : []).slice(0, 50).flatMap((o) => {
      if (!o || typeof o !== "object" || !OCCURRENCE_ID.test(String(o.occurrence_id || ""))) return [];
      return [{ occurrence_id: o.occurrence_id, starts_at: time(o.starts_at), ends_at: time(o.ends_at), opens_at: time(o.opens_at), closes_at: time(o.closes_at) }];
    });
    const out = {
      allowed: v.allowed,
      reason: v.allowed ? null : (MEETING_REASONS.has(v.reason) ? v.reason : "unavailable"),
      role: MEETING_ROLES.has(v.role) ? v.role : "none",
      admission: MEETING_ADMISSIONS.has(v.admission) ? v.admission : "deny",
      can_admit: v.can_admit === true,
      occurrences,
    };
    if (typeof v.meeting_id === "string" && MEETING_ID.test(v.meeting_id)) out.meeting_id = v.meeting_id;
    if (typeof v.event_id === "string" && EVENT_ID.test(v.event_id)) out.event_id = v.event_id;
    if (typeof v.occurrence_id === "string" && OCCURRENCE_ID.test(v.occurrence_id)) out.occurrence_id = v.occurrence_id;
    if (typeof v.code === "string" && MEETING_CODE.test(v.code.toUpperCase())) out.code = v.code.toUpperCase();
    for (const k of ["starts_at", "ends_at", "opens_at", "closes_at"]) out[k] = time(v[k]);
    const p = v.participant && typeof v.participant === "object" ? v.participant : null;
    out.participant = p ? {
      address: typeof p.address === "string" && ADDRESS.test(p.address) ? p.address : null,
      handle: typeof p.handle === "string" && ADDRESS.test(p.handle) ? p.handle : null,
      attendee_id: typeof p.attendee_id === "string" && ATTENDEE_ID.test(p.attendee_id) ? p.attendee_id : null,
      name: typeof p.name === "string" ? p.name.slice(0, 200) : "",
      verified: p.verified === true,
    } : null;
    const o = v.organizer && typeof v.organizer === "object" ? v.organizer : null;
    out.organizer = o && typeof o.address === "string" && ADDRESS.test(o.address)
      ? { address: o.address, ...(typeof o.handle === "string" && ADDRESS.test(o.handle) ? { handle: o.handle } : {}) }
      : null;
    const pol = v.policy && typeof v.policy === "object" ? v.policy : {};
    const word = (x) => (typeof x === "string" && /^[a-z_]{1,32}$/.test(x) ? x : null);
    out.policy = {
      waiting: word(pol.waiting), admit: word(pol.admit), allow_knock: pol.allow_knock === true,
      participant_limit: Number.isInteger(pol.participant_limit) && pol.participant_limit >= 1 && pol.participant_limit <= 30 ? pol.participant_limit : 6,
      transport: pol.transport === "sfu" ? "sfu" : "mesh",
    };
    out.admitters = (Array.isArray(v.admitters) ? v.admitters : []).filter((x) => typeof x === "string" && ADDRESS.test(x)).slice(0, 500);
    out.policy_version = typeof v.policy_version === "string" ? v.policy_version.slice(0, 32) : "";
    // An admission without the room's identity cannot be acted on.
    if (out.allowed && (!out.meeting_id || !out.event_id || !out.occurrence_id || !out.closes_at)) return null;
    return out;
  }

  async function meetingAccess(body) {
    if (!configured) return null;
    let value = null;
    try {
      const res = await post("/internal/v1/calendar/meeting-access", body, L.admissionTimeoutMs);
      if (res.ok) value = validMeetingAccess(await res.json());
    } catch (_) {
      value = null;
    }
    if (!value) {
      counters.admission_failed++;
      console.warn(`calls: meeting access unavailable (failures=${counters.admission_failed})`);
      return null;
    }
    counters.admission_ok++;
    return value;
  }

  // ---- Phase 6: live streams in groups (docs/streaming.md) ------------------------------------------
  //
  // Never cached: every join, role refresh and heartbeat asks again, so a removal, a stage change or
  // a policy change reaches the media room at the next check. null = unconfigured, unreachable or
  // an answer that does not validate; callers fail closed.

  const STREAM_ROLES = new Set(["host", "speaker", "invited", "viewer"]);
  const PUBLIC_KEY = /^B[A-Za-z0-9_-]{86}$/;

  function validStreamAdmission(v) {
    if (!v || typeof v !== "object" || typeof v.allowed !== "boolean") return null;
    if (!v.allowed) return { allowed: false, reason: typeof v.reason === "string" && /^[a-z_]{1,32}$/.test(v.reason) ? v.reason : "not_member" };
    if (!STREAM_ROLES.has(v.role) || typeof v.group_id !== "string" || !/^grp_[a-z2-7]{26}$/.test(v.group_id)
      || typeof v.host !== "string" || !ADDRESS.test(v.host) || v.state !== "live") return null;
    const sources = (Array.isArray(v.sources) ? v.sources : []).filter((x) => x === "camera" || x === "screen");
    return {
      allowed: true, role: v.role, group_id: v.group_id, host: v.host,
      host_key: typeof v.host_key === "string" && PUBLIC_KEY.test(v.host_key) ? v.host_key : null,
      sources, state: "live",
      ends_by: Number.isInteger(v.ends_by) ? v.ends_by : 0,
      max_viewers: Number.isInteger(v.max_viewers) && v.max_viewers > 0 ? Math.min(v.max_viewers, 10_000) : 200,
      key_attested: v.key_attested === true,
    };
  }

  async function ask(path, body, validate) {
    if (!configured) return null;
    let value = null;
    try {
      const res = await post(path, body, L.admissionTimeoutMs);
      if (res.ok) value = validate(await res.json());
    } catch (_) {
      value = null;
    }
    if (!value) {
      counters.admission_failed++;
      console.warn(`calls: ${path.slice(13)} unavailable (failures=${counters.admission_failed})`);
      return null;
    }
    counters.admission_ok++;
    return value;
  }

  const streamAdmit = ({ stream_id, from, public_key }) => ask("/internal/v1/stream-admission",
    { stream_id, from: { mailbox: from.address }, ...(public_key ? { public_key } : {}) }, validStreamAdmission);

  function validStreamCheck(v) {
    if (!v || typeof v !== "object" || (v.state !== "live" && v.state !== "ended") || !v.roles || typeof v.roles !== "object") return null;
    const roles = new Map();
    for (const [k, r] of Object.entries(v.roles)) if (ADDRESS.test(k)) roles.set(k, STREAM_ROLES.has(r) ? r : null);
    return { state: v.state, reason: typeof v.reason === "string" ? v.reason.slice(0, 32) : null, roles,
      host_key: typeof v.host_key === "string" && PUBLIC_KEY.test(v.host_key) ? v.host_key : null };
  }
  const streamCheck = ({ stream_id, viewers, members }) => ask("/internal/v1/stream-check",
    { stream_id, viewers, members: members.slice(0, 2_000) }, validStreamCheck);
  const streamEnd = ({ stream_id, reason }) => ask("/internal/v1/stream-end", { stream_id, reason },
    (v) => (v && v.ok === true ? v : null));

  // ---- lifecycle events ---------------------------------------------------------------------------

  function report(event) {
    if (!configured) return null;
    const body = { event_id: crypto.randomBytes(16).toString("hex"), ...event };
    if (queue.length >= L.queueMax) {
      queue.shift();
      counters.dropped++;
    }
    const t = clock();
    queue.push({ body, attempts: 0, firstAt: t, nextAt: t });
    persistSoon();
    arm(0);
    return body.event_id;
  }

  function arm(delay) {
    if (timer) return;
    timer = schedule(() => { timer = null; flush(); }, Math.max(0, delay));
    timer?.unref?.();
  }

  async function flush() {
    if (flushing) return;
    flushing = true;
    try {
      // In order; one event at a time, so a postbox that is down is not hammered.
      while (queue.length) {
        const item = queue[0];
        const wait = item.nextAt - clock();
        if (wait > 0) { arm(wait); return; }
        item.attempts++;
        let status = 0;
        try {
          const res = await post("/internal/v1/call-events", item.body, L.reportTimeoutMs);
          status = res.status;
        } catch (_) {
          status = 0;
        }
        if (queue[0] !== item) continue; // dropped from the front while we waited
        if (status >= 200 && status < 300) {
          queue.shift();
          counters.sent++;
          continue;
        }
        if (status >= 400 && status < 500 && status !== 429) {
          queue.shift();
          counters.rejected++;
          console.warn(`calls: call event rejected by postbox (status=${status}, rejected=${counters.rejected})`);
          continue;
        }
        if (item.attempts >= L.maxAttempts || clock() - item.firstAt >= L.giveUpAfterMs) {
          queue.shift();
          counters.failed++;
          counters.dropped++;
          console.warn(`calls: call event given up after ${item.attempts} attempts (dropped=${counters.dropped})`);
          continue;
        }
        counters.retried++;
        item.nextAt = clock() + Math.min(L.backoffMaxMs, L.backoffBaseMs * 2 ** (item.attempts - 1));
        // Head-of-line: later events wait behind the failing one, which keeps per-room order.
        arm(item.nextAt - clock());
        return;
      }
    } finally {
      flushing = false;
      persistSoon();
    }
  }

  function health() {
    return { configured, queued: queue.length, ...counters };
  }

  if (queue.length) arm(0);

  return { configured, admit, groupAdmit, meetingAccess, streamAdmit, streamCheck, streamEnd, report, health, flush, persist, _queue: queue, _cache: cache };
}
