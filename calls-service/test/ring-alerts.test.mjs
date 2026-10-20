import { test } from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import crypto from "node:crypto";
import { createCallsService, LIMITS } from "../src/server.js";
import { createRingAlerts, ringTokenMatches, newRingToken, RING_TOKEN } from "../src/ring-alerts.js";

// Three accounts. Alice owns two mailboxes, Bob one, Carol one. Tokens are opaque strings; the fake
// postbox below is the only thing that knows which mailboxes a token may act as.
const ACCOUNTS = {
  "tok-alice": [{ address: "/k/alice1", handle: "/alice" }, { address: "/k/alice2", handle: "/alice/agent" }],
  "tok-bob": [{ address: "/k/bob1", handle: "/bob" }],
  "tok-carol": [{ address: "/k/carol1", handle: null }],
};

function fakePostbox() {
  const calls = [];
  const fetch = async (url, opts) => {
    calls.push(url);
    const owned = ACCOUNTS[(opts.headers.authorization || "").replace(/^Bearer /, "")];
    if (!owned) return { ok: false, status: 401, json: async () => ({}) };
    const u = new URL(url);
    if (u.pathname === "/v1/identities") return { ok: true, status: 200, json: async () => ({ identities: owned.map((m) => ({ address: m.address, label: null })) }) };
    if (u.pathname === "/v1/whoami") {
      const m = owned.find((x) => x.address === u.searchParams.get("identity"));
      return m ? { ok: true, status: 200, json: async () => m } : { ok: false, status: 403, json: async () => ({}) };
    }
    return { ok: false, status: 404, json: async () => ({}) };
  };
  return { fetch, calls };
}

let codeSeq = 0;
async function start(overrides = {}, extraDeps = {}) {
  const postbox = fakePostbox();
  const cfg = {
    postbox: "https://postbox.test", turnSecret: "s3cret", turnUrls: ["turn:relay.test:3478?transport=udp"],
    turnTtlSeconds: 3600, allowedOrigins: ["https://inbox.test"], ringMs: 400, knockMs: 1500, trustProxy: true, ...overrides,
  };
  const service = createCallsService(cfg, { fetch: postbox.fetch, randomCode: () => "CODE" + String(10 + (codeSeq++ % 80)), ...extraDeps });
  const server = http.createServer((req, res) => service.handle(req, res));
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const base = `http://127.0.0.1:${server.address().port}`;
  const open = [];
  let ipSeq = 0;
  const t = {
    service, postbox, base,
    async req(auth, method, path, body, headers = {}) {
      const authHeader = !auth ? {} : auth.startsWith("guest:") ? { authorization: "Guest " + auth.slice(6) } : { authorization: "Bearer " + auth };
      const res = await fetch(base + path, {
        method,
        headers: { ...authHeader, ...(body ? { "content-type": "application/json" } : {}), ...headers },
        body: body ? JSON.stringify(body) : undefined,
      });
      return { status: res.status, body: await res.json().catch(() => null) };
    },
    async guest(ip = "198.51.100." + (++ipSeq)) {
      const r = await t.req(null, "POST", "/calls/v1/guests", {}, { "cf-connecting-ip": ip });
      assert.equal(r.status, 200);
      return "guest:" + r.body.guest_token;
    },
    async stream(auth) {
      const ac = new AbortController();
      const authHeader = auth.startsWith("guest:") ? "Guest " + auth.slice(6) : "Bearer " + auth;
      const res = await fetch(base + "/calls/v1/events", { headers: { authorization: authHeader }, signal: ac.signal });
      assert.equal(res.status, 200);
      const reader = res.body.getReader();
      const dec = new TextDecoder();
      let buf = "";
      const queue = [], waiters = [];
      (async () => {
        try {
          for (;;) {
            const { value, done } = await reader.read();
            if (done) break;
            buf += dec.decode(value, { stream: true });
            let i;
            while ((i = buf.indexOf("\n\n")) >= 0) {
              const frame = buf.slice(0, i); buf = buf.slice(i + 2);
              const ev = /^event: (.*)$/m.exec(frame), data = /^data: (.*)$/m.exec(frame);
              if (!ev) continue;
              const item = { event: ev[1], data: JSON.parse(data[1]) };
              const w = waiters.shift();
              if (w) w(item); else queue.push(item);
            }
          }
        } catch (_) { /* aborted */ }
      })();
      const s = {
        auth,
        next: (ms = 2000) => queue.length ? Promise.resolve(queue.shift())
          : new Promise((resolve, reject) => {
            const timer = setTimeout(() => reject(new Error("no event")), ms);
            waiters.push((x) => { clearTimeout(timer); resolve(x); });
          }),
        async until(name, ms = 2000) { for (;;) { const e = await s.next(ms); if (e.event === name) return e; } },
        pending: () => queue.length,
        close: () => ac.abort(),
      };
      const ready = await s.next();
      assert.equal(ready.event, "ready");
      s.id = ready.data.session;
      open.push(s);
      return s;
    },
    async stop() {
      for (const s of open) s.close();
      await new Promise((r) => server.close(r));
      server.closeAllConnections?.();
    },
  };
  return t;
}


const settle = (ms = 80) => new Promise((r) => setTimeout(r, ms));

// A stand-in for the postbox client that records what would have been pushed.
function recorder() {
  const rings = [], cancels = [];
  return {
    rings, cancels,
    alerts: {
      configured: true,
      ring: async (r) => { rings.push(r); return { decision: "ring", devices: 1 }; },
      cancel: async (r, reason) => { cancels.push({ ...r, reason }); return { devices: 1 }; },
      health: () => ({ configured: true }),
    },
  };
}

async function ringBob(t, kind = "audio") {
  const alice = await t.stream("tok-alice");
  const placed = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/bob", kind });
  assert.equal(placed.status, 200);
  return { alice, room: placed.body.room };
}

test("a ring is pushed with a fresh ring token, and its end is pushed with the reason", async () => {
  const rec = recorder();
  const t = await start({}, { ringAlerts: rec.alerts });
  try {
    const { alice, room } = await ringBob(t, "video");
    assert.equal(rec.rings.length, 1);
    const r = rec.rings[0];
    assert.equal(r.room, room);
    assert.equal(r.to, "/bob");
    assert.equal(r.kind, "video");
    assert.equal(r.from.address, "/k/alice1");
    assert.match(r.token, RING_TOKEN);
    assert.ok(r.expiresAt > Date.now());
    // The caller hangs up while it rings.
    await t.req("tok-alice", "POST", `/calls/v1/rooms/${room}/leave`, { session: alice.id });
    await settle();
    assert.deepEqual(rec.cancels.map((c) => [c.room, c.to, c.reason]), [[room, "/bob", "cancelled"]]);
  } finally { await t.stop(); }
});

test("a session opened while its mailbox rings joins the ring and can answer", async () => {
  const rec = recorder();
  const t = await start({ ringMs: 3000 }, { ringAlerts: rec.alerts });
  try {
    const { alice, room } = await ringBob(t);
    // Bob's phone was woken by the push and opens its stream only now.
    const bob = await t.stream("tok-bob");
    const incoming = await bob.until("incoming");
    assert.equal(incoming.data.room, room);
    assert.equal(incoming.data.from.handle, "/alice");
    const ok = await t.req("tok-bob", "POST", `/calls/v1/rooms/${room}/accept`, { session: bob.id });
    assert.equal(ok.status, 200);
    await alice.until("participant_joined");
    assert.equal(rec.cancels.at(-1).reason, "answered_elsewhere");
    // The caller's own late session is never rung.
    const alice2 = await t.stream("tok-alice");
    await settle();
    assert.equal(alice2.pending(), 0);
  } finally { await t.stop(); }
});

test("accepting with the ring token works only with the right token and the callee's account", async () => {
  const rec = recorder();
  const t = await start({ ringMs: 3000 }, { ringAlerts: rec.alerts });
  try {
    const bob1 = await t.stream("tok-bob");
    const bob2 = await t.stream("tok-bob");
    const { room } = await ringBob(t);
    await bob1.until("incoming");
    await bob2.until("incoming");
    // bob1 says busy; bob2 still rings, so the ring lives on, but bob1 is no longer rung.
    await t.req("tok-bob", "POST", `/calls/v1/rooms/${room}/decline`, { session: bob1.id, reason: "busy" });
    const wrong = await t.req("tok-bob", "POST", `/calls/v1/rooms/${room}/accept`, { session: bob1.id, ring_token: newRingToken().token });
    assert.equal(wrong.status, 409);
    const carol = await t.stream("tok-carol");
    const stranger = await t.req("tok-carol", "POST", `/calls/v1/rooms/${room}/accept`, { session: carol.id, ring_token: rec.rings[0].token });
    assert.equal(stranger.status, 403);
    const right = await t.req("tok-bob", "POST", `/calls/v1/rooms/${room}/accept`, { session: bob1.id, ring_token: rec.rings[0].token });
    assert.equal(right.status, 200);
    assert.equal((await bob2.until("ring_ended")).data.reason, "answered_elsewhere");
  } finally { await t.stop(); }
});

test("a ring token reads the ring's state and declines it without a member token", async () => {
  const rec = recorder();
  const t = await start({ ringMs: 3000 }, { ringAlerts: rec.alerts });
  try {
    const { alice, room } = await ringBob(t, "video");
    const token = rec.rings[0].token;
    const state = await t.req(null, "GET", `/calls/v1/rings/${room}`, null, { "x-ring-token": token });
    assert.equal(state.status, 200);
    assert.equal(state.body.state, "ringing");
    assert.equal(state.body.kind, "video");
    for (const bad of ["", newRingToken().token, token.slice(0, 42)]) {
      const r = await t.req(null, "GET", `/calls/v1/rings/${room}`, null, { "x-ring-token": bad });
      assert.equal(r.status, 404);
      assert.equal(r.body.error, "no_ring");
    }
    const unknown = await t.req(null, "GET", `/calls/v1/rings/AAAAAAAAAAAAAAAAAAAAAA`, null, { "x-ring-token": token });
    assert.equal(unknown.status, 404);
    const badDecline = await t.req(null, "POST", `/calls/v1/rings/${room}/decline`, { ring_token: newRingToken().token });
    assert.equal(badDecline.status, 404);
    const declined = await t.req(null, "POST", `/calls/v1/rings/${room}/decline`, { ring_token: token });
    assert.equal(declined.status, 200);
    assert.equal((await alice.until("ended")).data.reason, "declined");
    assert.equal(rec.cancels.at(-1).reason, "declined");
    const after = await t.req(null, "GET", `/calls/v1/rings/${room}`, null, { "x-ring-token": token });
    assert.deepEqual(after.body, { state: "ended", reason: "declined" });
    // Declining twice is harmless.
    const again = await t.req(null, "POST", `/calls/v1/rings/${room}/decline`, { ring_token: token });
    assert.equal(again.body.state, "ended");
  } finally { await t.stop(); }
});

test("an unanswered ring ends on time and its devices hear no_answer", async () => {
  const rec = recorder();
  const t = await start({ ringMs: 150 }, { ringAlerts: rec.alerts });
  try {
    const { room } = await ringBob(t);
    await settle(300);
    assert.equal(rec.cancels.at(-1).reason, "no_answer");
    const after = await t.req(null, "GET", `/calls/v1/rings/${room}`, null, { "x-ring-token": rec.rings[0].token });
    assert.equal(after.body.reason, "no_answer");
  } finally { await t.stop(); }
});

test("nothing is pushed for a callee already in a call", async () => {
  const rec = recorder();
  const t = await start({ ringMs: 3000 }, { ringAlerts: rec.alerts });
  try {
    // Bob is in a call with Carol.
    const bob = await t.stream("tok-bob");
    const carol = await t.stream("tok-carol");
    const c = await t.req("tok-carol", "POST", "/calls/v1/calls", { session: carol.id, from: "/k/carol1", to: "/bob", kind: "audio" });
    await bob.until("incoming");
    await t.req("tok-bob", "POST", `/calls/v1/rooms/${c.body.room}/accept`, { session: bob.id });
    rec.rings.length = 0; rec.cancels.length = 0;
    await ringBob(t);
    assert.equal(rec.rings.length, 0);
    // A late session of Bob's is not rung either.
    const bob2 = await t.stream("tok-bob");
    await settle();
    assert.equal(bob2.pending(), 0);
  } finally { await t.stop(); }
});

test("the postbox client posts the documented bodies with the service token, and is off unless enabled", async () => {
  const seen = [];
  const fetch = async (url, opts) => { seen.push({ url, opts }); return { ok: true, status: 200, json: async () => ({ decision: "ring", devices: 2 }) }; };
  const off = createRingAlerts({ enabled: false, url: "http://127.0.0.1:18991", token: "svc_x", fetch });
  assert.equal(off.configured, false);
  assert.equal(await off.ring({}), null);
  assert.equal(seen.length, 0);
  const on = createRingAlerts({ enabled: true, url: "http://127.0.0.1:18991/", token: "svc_calls", fetch });
  const { token } = newRingToken();
  await on.ring({ room: "r".repeat(22), code: "ABC234", kind: "video", from: { address: "/k/a", handle: "/alice" }, to: "/bob", token, expiresAt: 1_792_500_045_500 });
  await on.cancel({ room: "r".repeat(22), to: "/bob", kind: "video", from: { address: "/k/a", handle: null } }, "declined");
  assert.equal(seen[0].url, "http://127.0.0.1:18991/internal/v1/call-ring");
  assert.equal(seen[0].opts.headers.authorization, "Bearer svc_calls");
  assert.deepEqual(JSON.parse(seen[0].opts.body), {
    room_id: "r".repeat(22), code: "ABC234", kind: "video", ring_token: token,
    from: { address: "/k/a", handle: "/alice" }, to: "/bob", expires_at: 1_792_500_045,
  });
  assert.equal(seen[1].url, "http://127.0.0.1:18991/internal/v1/call-ring-cancel");
  assert.deepEqual(JSON.parse(seen[1].opts.body), { room_id: "r".repeat(22), to: "/bob", kind: "video", from: { address: "/k/a" }, reason: "declined" });
  assert.equal(on.health().devices, 4);
  // A failing postbox never throws into the call path.
  const broken = createRingAlerts({ enabled: true, url: "http://x", token: "svc_y", fetch: async () => { throw new Error("down"); } });
  assert.equal(await broken.ring({ room: "r", to: "/b", from: null, expiresAt: 0 }), null);
  assert.equal(broken.health().failed, 1);
});

test("ring tokens are compared by hash and only in their own format", () => {
  const { token, hash } = newRingToken();
  assert.match(token, RING_TOKEN);
  assert.ok(ringTokenMatches(hash, token));
  assert.ok(!ringTokenMatches(hash, newRingToken().token));
  assert.ok(!ringTokenMatches(hash, token + "x"));
  assert.ok(!ringTokenMatches(null, token));
  assert.ok(!ringTokenMatches(hash, undefined));
});
