// Pigeonpost calls — waking the callee's devices (Phase 1c, docs/call-alerting.md).
//
// A ring used to reach only the event-stream sessions this service holds, so a phone with the app
// closed never rang. When `CALLS_RING_PUSH=1`, every ring is also handed to the postbox, which owns
// the device registrations and pushes to them (APNs VoIP, APNs alert, FCM, WNS); when the ring ends
// for any reason the postbox is told so it can take the call screen down again.
//
// Each ring carries a ring token: 32 random bytes, held here only as a SHA-256, valid until the ring
// ends. A device woken by a push uses it to ask whether the ring is still live, to decline without a
// member token, and to accept from a session that was opened after the ring started.
//
// Fire and forget, short timeout, no retries: a ring delivered late is worthless, and the ring's own
// expiry is the only retry policy that makes sense. Logs carry counts and outcomes only.

import crypto from "node:crypto";

export const RING_TOKEN = /^[A-Za-z0-9_-]{43}$/;

const sha256 = (s) => crypto.createHash("sha256").update(s).digest("hex");

export function newRingToken() {
  const token = crypto.randomBytes(32).toString("base64url");
  return { token, hash: sha256(token) };
}

// Constant-time comparison of a presented token with the stored hash.
export function ringTokenMatches(hash, presented) {
  if (!hash || typeof presented !== "string" || !RING_TOKEN.test(presented)) return false;
  const a = Buffer.from(sha256(presented), "hex"), b = Buffer.from(hash, "hex");
  return a.length === b.length && crypto.timingSafeEqual(a, b);
}

export function createRingAlerts({ enabled, url, token, fetch, timeoutMs = 3_000 } = {}) {
  const base = (url || "").replace(/\/+$/, "");
  const configured = Boolean(enabled && base && token);
  const fetchImpl = fetch || globalThis.fetch;
  const counters = { rings: 0, cancels: 0, failed: 0, devices: 0 };

  async function post(path, body) {
    const ac = new AbortController();
    const t = setTimeout(() => ac.abort(), timeoutMs);
    t.unref?.();
    try {
      const res = await fetchImpl(base + path, {
        method: "POST",
        headers: { authorization: "Bearer " + token, "content-type": "application/json", accept: "application/json" },
        body: JSON.stringify(body),
        signal: ac.signal,
      });
      if (!res.ok) throw new Error("status " + res.status);
      const value = await res.json().catch(() => ({}));
      if (Number.isInteger(value.devices)) counters.devices += value.devices;
      return value;
    } catch (err) {
      counters.failed++;
      console.warn(`calls: ring push ${path.endsWith("cancel") ? "cancel" : "ring"} failed (${err && err.name === "AbortError" ? "timeout" : "error"}; failures=${counters.failed})`);
      return null;
    } finally {
      clearTimeout(t);
    }
  }

  const who = (m) => (m ? { address: m.address, ...(m.handle ? { handle: m.handle } : {}), ...(m.display ? { display: m.display } : {}) } : null);

  return {
    configured,
    // `ring` = { room, code, kind, from, to, token, expiresAt }
    ring(r) {
      if (!configured) return Promise.resolve(null);
      counters.rings++;
      return post("/internal/v1/call-ring", {
        room_id: r.room, code: r.code, kind: r.kind, ring_token: r.token,
        from: who(r.from), to: r.to, expires_at: Math.floor(r.expiresAt / 1000),
      });
    },
    cancel(r, reason) {
      if (!configured) return Promise.resolve(null);
      counters.cancels++;
      return post("/internal/v1/call-ring-cancel", { room_id: r.room, to: r.to, kind: r.kind, from: who(r.from), reason });
    },
    health: () => ({ configured, ...counters }),
  };
}
