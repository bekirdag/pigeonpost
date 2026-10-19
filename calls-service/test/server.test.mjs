import { test } from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import crypto from "node:crypto";
import { createCallsService } from "../src/server.js";

// Three accounts. Alice owns two mailboxes, Bob one, Carol one. Tokens are opaque strings; the
// fake postbox below is the only thing that knows which mailboxes a token may act as.
const ACCOUNTS = {
  "tok-alice": [{ address: "/k/alice1", handle: "/alice" }, { address: "/k/alice2", handle: "/alice/agent" }],
  "tok-bob": [{ address: "/k/bob1", handle: "/bob" }],
  "tok-carol": [{ address: "/k/carol1", handle: null }],
};

function fakePostbox() {
  const calls = [];
  const fetch = async (url, opts) => {
    calls.push(url);
    const token = (opts.headers.authorization || "").replace(/^Bearer /, "");
    const owned = ACCOUNTS[token];
    if (!owned) return { ok: false, status: 401, json: async () => ({}) };
    const u = new URL(url);
    if (u.pathname === "/v1/identities") {
      return { ok: true, status: 200, json: async () => ({ identities: owned.map((m) => ({ address: m.address, label: null })) }) };
    }
    if (u.pathname === "/v1/whoami") {
      const m = owned.find((x) => x.address === u.searchParams.get("identity"));
      if (!m) return { ok: false, status: 403, json: async () => ({}) };
      return { ok: true, status: 200, json: async () => ({ address: m.address, handle: m.handle }) };
    }
    return { ok: false, status: 404, json: async () => ({}) };
  };
  return { fetch, calls };
}

async function start(overrides = {}) {
  const postbox = fakePostbox();
  const cfg = {
    postbox: "https://postbox.test", turnSecret: "s3cret", turnUrls: ["turn:relay.test:3478?transport=udp"],
    turnTtlSeconds: 3600, allowedOrigins: ["https://inbox.test"], ringMs: 400, ...overrides,
  };
  const service = createCallsService(cfg, { fetch: postbox.fetch });
  const server = http.createServer((req, res) => service.handle(req, res));
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const base = `http://127.0.0.1:${server.address().port}`;
  const open = [];
  const t = {
    service, postbox, base,
    async req(token, method, path, body, headers = {}) {
      const res = await fetch(base + path, {
        method,
        headers: { ...(token ? { authorization: "Bearer " + token } : {}), ...(body ? { "content-type": "application/json" } : {}), ...headers },
        body: body ? JSON.stringify(body) : undefined,
      });
      return { status: res.status, body: await res.json().catch(() => null) };
    },
    async stream(token) {
      const ac = new AbortController();
      const res = await fetch(base + "/calls/v1/events", { headers: { authorization: "Bearer " + token }, signal: ac.signal });
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
        next: (ms = 1500) => queue.length ? Promise.resolve(queue.shift())
          : new Promise((resolve, reject) => {
            const timer = setTimeout(() => reject(new Error("no event")), ms);
            waiters.push((x) => { clearTimeout(timer); resolve(x); });
          }),
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

const settle = (ms = 60) => new Promise((r) => setTimeout(r, ms));

test("rejects requests without a valid member token", async () => {
  const t = await start();
  try {
    assert.equal((await t.req(null, "GET", "/calls/v1/turn")).status, 401);
    assert.equal((await t.req("tok-nobody", "GET", "/calls/v1/turn")).status, 401);
    assert.equal((await t.req("bad token!", "GET", "/calls/v1/turn")).status, 401);
  } finally { await t.stop(); }
});

test("a full call: ring, accept, offer/answer/ice, end", async () => {
  const t = await start();
  try {
    const alice = await t.stream("tok-alice");
    const bob = await t.stream("tok-bob");
    const started = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/bob", kind: "video" });
    assert.equal(started.status, 200);
    assert.equal(started.body.state, "ringing");
    const incoming = await bob.next();
    assert.equal(incoming.event, "incoming");
    assert.equal(incoming.data.kind, "video");
    assert.deepEqual(incoming.data.from, { address: "/k/alice1", handle: "/alice" });
    const id = started.body.call;

    // Signals are refused until the call is accepted.
    assert.equal((await t.req("tok-alice", "POST", `/calls/v1/calls/${id}/signal`, { session: alice.id, type: "offer", data: { type: "offer", sdp: "v=0" } })).status, 409);

    const acc = await t.req("tok-bob", "POST", `/calls/v1/calls/${id}/accept`, { session: bob.id });
    assert.equal(acc.status, 200);
    const accepted = await alice.next();
    assert.equal(accepted.event, "accepted");
    assert.deepEqual(accepted.data.callee, { address: "/k/bob1", handle: "/bob" });

    assert.equal((await t.req("tok-alice", "POST", `/calls/v1/calls/${id}/signal`, { session: alice.id, type: "offer", data: { type: "offer", sdp: "v=0 offer" } })).status, 200);
    const offer = await bob.next();
    assert.deepEqual(offer.data, { call: id, type: "offer", data: { type: "offer", sdp: "v=0 offer" } });
    assert.equal((await t.req("tok-bob", "POST", `/calls/v1/calls/${id}/signal`, { session: bob.id, type: "answer", data: { type: "answer", sdp: "v=0 answer" } })).status, 200);
    assert.equal((await alice.next()).data.type, "answer");
    assert.equal((await t.req("tok-bob", "POST", `/calls/v1/calls/${id}/signal`, { session: bob.id, type: "ice", data: { candidate: "candidate:1 1 udp 1 1.2.3.4 5 typ host", sdpMid: "0", sdpMLineIndex: 0, extra: "dropped" } })).status, 200);
    const ice = await alice.next();
    assert.deepEqual(ice.data.data, { candidate: "candidate:1 1 udp 1 1.2.3.4 5 typ host", sdpMid: "0", sdpMLineIndex: 0 });
    assert.equal(t.service.state.calls.get(id).state, "active");

    assert.equal((await t.req("tok-bob", "POST", `/calls/v1/calls/${id}/end`, { session: bob.id })).status, 200);
    const ended = await alice.next();
    assert.deepEqual([ended.event, ended.data.reason], ["ended", "ended"]);
    assert.equal(bob.pending(), 0, "the party that hung up is not told its own hang-up");
  } finally { await t.stop(); }
});

test("only mailboxes the token owns can place or answer calls", async () => {
  const t = await start();
  try {
    const alice = await t.stream("tok-alice");
    const bob = await t.stream("tok-bob");
    const carol = await t.stream("tok-carol");
    // Alice cannot call as Bob.
    assert.equal((await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/bob", to: "/k/carol1", kind: "audio" })).body.error, "not_your_mailbox");
    // Alice cannot use Bob's session.
    assert.equal((await t.req("tok-alice", "POST", "/calls/v1/calls", { session: bob.id, from: "/alice", to: "/bob", kind: "audio" })).body.error, "no_session");
    // A call to Bob rings Bob only; Carol neither hears it nor can accept it.
    const started = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/k/alice1", to: "/BOB", kind: "audio" });
    assert.equal((await bob.next()).event, "incoming");
    await settle();
    assert.equal(carol.pending(), 0);
    const id = started.body.call;
    assert.equal((await t.req("tok-carol", "POST", `/calls/v1/calls/${id}/accept`, { session: carol.id })).body.error, "not_your_call");
    assert.equal((await t.req("tok-carol", "POST", `/calls/v1/calls/${id}/end`, { session: carol.id })).body.error, "not_your_call");
    assert.equal((await t.req("tok-carol", "POST", `/calls/v1/calls/${id}/signal`, { session: carol.id, type: "ice", data: null })).status, 409);
  } finally { await t.stop(); }
});

test("a call to one of an account's mailboxes rings only sessions of that account, not the caller's own session", async () => {
  const t = await start();
  try {
    const aliceA = await t.stream("tok-alice");
    const aliceB = await t.stream("tok-alice");
    const bob = await t.stream("tok-bob");
    // Alice calling her own second mailbox: her other session rings, her calling session does not.
    const started = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: aliceA.id, from: "/alice", to: "/alice/agent", kind: "audio" });
    assert.equal(started.status, 200);
    assert.equal((await aliceB.next()).event, "incoming");
    await settle();
    assert.equal(aliceA.pending(), 0);
    assert.equal(bob.pending(), 0);
    // Calling the very mailbox you are calling from is refused.
    assert.equal((await t.req("tok-alice", "POST", "/calls/v1/calls", { session: aliceB.id, from: "/alice", to: "/k/alice1", kind: "audio" })).body.error, "cannot_call_self");
  } finally { await t.stop(); }
});

test("accepting on one device stops the others ringing", async () => {
  const t = await start();
  try {
    const alice = await t.stream("tok-alice");
    const bob1 = await t.stream("tok-bob");
    const bob2 = await t.stream("tok-bob");
    const { body } = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/bob", kind: "audio" });
    await bob1.next(); await bob2.next();
    await t.req("tok-bob", "POST", `/calls/v1/calls/${body.call}/accept`, { session: bob1.id });
    const other = await bob2.next();
    assert.deepEqual([other.event, other.data.reason], ["ended", "answered_elsewhere"]);
    assert.equal((await t.req("tok-bob", "POST", `/calls/v1/calls/${body.call}/accept`, { session: bob2.id })).body.error, "not_ringing");
  } finally { await t.stop(); }
});

test("decline, busy, cancel and no-answer each end the call with their reason", async () => {
  const t = await start({ ringMs: 300 });
  try {
    const alice = await t.stream("tok-alice");
    const bob = await t.stream("tok-bob");
    let r = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/bob", kind: "audio" });
    await bob.next();
    await t.req("tok-bob", "POST", `/calls/v1/calls/${r.body.call}/decline`, { session: bob.id });
    assert.deepEqual((await alice.next()).data.reason, "declined");

    r = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/bob", kind: "audio" });
    await bob.next();
    await t.req("tok-bob", "POST", `/calls/v1/calls/${r.body.call}/decline`, { session: bob.id, reason: "busy" });
    assert.deepEqual((await alice.next()).data.reason, "busy");

    r = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/bob", kind: "audio" });
    await bob.next();
    await t.req("tok-alice", "POST", `/calls/v1/calls/${r.body.call}/end`, { session: alice.id });
    assert.deepEqual((await bob.next()).data.reason, "cancelled");

    r = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/bob", kind: "audio" });
    await bob.next();
    const a = await alice.next(1000), b = await bob.next(1000);
    assert.deepEqual([a.data.reason, b.data.reason], ["no_answer", "no_answer"]);
  } finally { await t.stop(); }
});

test("an offline or engaged recipient looks like an unanswered call", async () => {
  const t = await start({ ringMs: 200 });
  try {
    const alice = await t.stream("tok-alice");
    const r = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/nobody/here", kind: "audio" });
    assert.equal(r.status, 200);
    assert.equal(r.body.state, "ringing");
    assert.equal((await alice.next(1000)).data.reason, "no_answer");
  } finally { await t.stop(); }
});

test("a session already in a call cannot place a second one; a new call to an engaged mailbox does not ring it", async () => {
  const t = await start();
  try {
    const alice = await t.stream("tok-alice");
    const bob = await t.stream("tok-bob");
    const carol = await t.stream("tok-carol");
    const r = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/bob", kind: "audio" });
    await bob.next();
    await t.req("tok-bob", "POST", `/calls/v1/calls/${r.body.call}/accept`, { session: bob.id });
    await alice.next();
    assert.equal((await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/k/carol1", kind: "audio" })).body.error, "already_in_call");
    assert.equal((await t.req("tok-carol", "POST", "/calls/v1/calls", { session: carol.id, from: "/k/carol1", to: "/bob", kind: "audio" })).status, 200);
    await settle();
    assert.equal(bob.pending(), 0);
  } finally { await t.stop(); }
});

test("malformed and oversized signals are refused", async () => {
  const t = await start();
  try {
    const alice = await t.stream("tok-alice");
    const bob = await t.stream("tok-bob");
    const r = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/bob", kind: "audio" });
    await bob.next();
    await t.req("tok-bob", "POST", `/calls/v1/calls/${r.body.call}/accept`, { session: bob.id });
    const sig = (body) => t.req("tok-alice", "POST", `/calls/v1/calls/${r.body.call}/signal`, { session: alice.id, ...body });
    assert.equal((await sig({ type: "script", data: {} })).body.error, "bad_signal");
    assert.equal((await sig({ type: "offer", data: { type: "answer", sdp: "x" } })).body.error, "bad_signal");
    assert.equal((await sig({ type: "offer", data: { type: "offer", sdp: "x".repeat(100_001) } })).body.error, "bad_signal");
    assert.equal((await sig({ type: "ice", data: { candidate: "c".repeat(1001) } })).body.error, "bad_signal");
    const big = await fetch(t.base + `/calls/v1/calls/${r.body.call}/signal`, {
      method: "POST", headers: { authorization: "Bearer tok-alice", "content-type": "application/json" },
      body: JSON.stringify({ session: alice.id, type: "offer", data: { type: "offer", sdp: "x".repeat(200_000) } }),
    });
    assert.equal(big.status, 413);
    assert.equal((await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "bob", kind: "audio" })).body.error, "bad_recipient");
    assert.equal((await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/bob", kind: "screen" })).body.error, "bad_kind");
    assert.equal((await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/bob", kind: "audio" }, { origin: "https://evil.test" })).body.error, "bad_origin");
  } finally { await t.stop(); }
});

test("TURN credentials are short-lived HMACs bound to a live session", async () => {
  const t = await start();
  try {
    assert.equal((await t.req("tok-alice", "GET", "/calls/v1/turn?session=nope")).status, 409);
    const alice = await t.stream("tok-alice");
    const { body } = await t.req("tok-alice", "GET", "/calls/v1/turn?session=" + alice.id);
    const [server] = body.iceServers;
    const [expires] = server.username.split(":");
    const ttl = Number(expires) - Math.floor(Date.now() / 1000);
    assert.ok(ttl > 3500 && ttl <= 3600, "expires within the configured lifetime");
    assert.equal(server.credential, crypto.createHmac("sha1", "s3cret").update(server.username).digest("base64"));
    assert.ok(!JSON.stringify(body).includes("s3cret"));
  } finally { await t.stop(); }
});

test("rate limits new calls per account", async () => {
  const t = await start({ ringMs: 50 });
  try {
    const alice = await t.stream("tok-alice");
    let last;
    for (let i = 0; i < 7; i++) {
      last = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/nobody", kind: "audio" });
      if (last.status === 429) break;
      await t.req("tok-alice", "POST", `/calls/v1/calls/${last.body.call}/end`, { session: alice.id });
    }
    assert.equal(last.body.error, "rate_limited");
  } finally { await t.stop(); }
});

test("a reloaded page can take a connected call back over within the grace period", async () => {
  const t = await start();
  try {
    const alice = await t.stream("tok-alice");
    const bob = await t.stream("tok-bob");
    const r = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/bob", kind: "audio" });
    await bob.next();
    await t.req("tok-bob", "POST", `/calls/v1/calls/${r.body.call}/accept`, { session: bob.id });
    await alice.next();
    bob.close();
    await settle(100);
    const bob2 = await t.stream("tok-bob");
    const res = await t.req("tok-bob", "POST", `/calls/v1/calls/${r.body.call}/resume`, { session: bob2.id });
    assert.equal(res.body.role, "callee");
    assert.equal((await t.req("tok-alice", "POST", `/calls/v1/calls/${r.body.call}/signal`, { session: alice.id, type: "ice", data: null })).status, 200);
    assert.equal((await bob2.next()).event, "signal");
    // Another account cannot take the call over.
    const carol = await t.stream("tok-carol");
    assert.equal((await t.req("tok-carol", "POST", `/calls/v1/calls/${r.body.call}/resume`, { session: carol.id })).body.error, "not_your_call");
  } finally { await t.stop(); }
});

test("the caller leaving while it rings cancels the call", async () => {
  const t = await start();
  try {
    const alice = await t.stream("tok-alice");
    const bob = await t.stream("tok-bob");
    await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/bob", kind: "audio" });
    await bob.next();
    alice.close();
    const ev = await bob.next();
    assert.deepEqual([ev.event, ev.data.reason], ["ended", "cancelled"]);
  } finally { await t.stop(); }
});

test("postbox answers are cached briefly so a call does not cost a postbox round trip per signal", async () => {
  const t = await start();
  try {
    const alice = await t.stream("tok-alice");
    const before = t.postbox.calls.length;
    for (let i = 0; i < 5; i++) await t.req("tok-alice", "GET", "/calls/v1/turn?session=" + alice.id);
    assert.equal(t.postbox.calls.length, before);
  } finally { await t.stop(); }
});
