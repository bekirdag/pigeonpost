// Pigeonpost calls — the postbox's internal API (Phase 1b, SDS §12.2, §13.3).
//
// Two things this service now asks of the postbox, over a private path and with its own service
// token (`svc_…`), never with a member's token:
//
//   * call admission — before ringing anyone, whether the callee's settings say ring, silence or
//     block. Cached for 30 s per (caller, callee, kind). If the postbox is configured but cannot be
//     asked, admission fails closed: the call behaves exactly like an unanswered one.
//   * lifecycle events — ring, answer, decline, missed, end … so each participant's call history can
//     be written. Sent through a bounded in-memory retry queue with stable event ids, so a retried
//     event is deduplicated by the postbox. Nothing is written to disk; a restart loses what is
//     queued, and the health counters say how much was dropped.
//
// Logs carry counts and outcomes only: no tokens, no addresses.

import crypto from "node:crypto";

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

export function createPostboxInternal({ url, token, fetch, now, setTimeout: setTimer, limits } = {}) {
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
    }
  }

  function health() {
    return { configured, queued: queue.length, ...counters };
  }

  return { configured, admit, groupAdmit, report, health, flush, _queue: queue, _cache: cache };
}
