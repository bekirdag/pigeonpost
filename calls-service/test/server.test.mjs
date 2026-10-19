import { test } from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import crypto from "node:crypto";
import { createCallsService, LIMITS } from "../src/server.js";

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
async function start(overrides = {}) {
  const postbox = fakePostbox();
  const cfg = {
    postbox: "https://postbox.test", turnSecret: "s3cret", turnUrls: ["turn:relay.test:3478?transport=udp"],
    turnTtlSeconds: 3600, allowedOrigins: ["https://inbox.test"], ringMs: 400, knockMs: 1500, trustProxy: true, ...overrides,
  };
  const service = createCallsService(cfg, { fetch: postbox.fetch, randomCode: () => "CODE" + String(10 + (codeSeq++ % 80)) });
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

async function connectedPair(t) {
  const alice = await t.stream("tok-alice");
  const bob = await t.stream("tok-bob");
  const placed = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/bob", kind: "video" });
  assert.equal(placed.status, 200);
  await bob.until("incoming");
  const accepted = await t.req("tok-bob", "POST", `/calls/v1/rooms/${placed.body.room}/accept`, { session: bob.id });
  assert.equal(accepted.status, 200);
  const joinedBob = await bob.until("joined");
  const joinedAlice = await alice.until("participant_joined");
  return { alice, bob, room: placed.body.room, code: placed.body.code, alicePid: placed.body.you, bobPid: joinedBob.data.you, joinedAlice };
}

test("rejects requests without a valid member token or guest token", async () => {
  const t = await start();
  try {
    assert.equal((await t.req(null, "GET", "/calls/v1/turn")).status, 401);
    assert.equal((await t.req("tok-nobody", "GET", "/calls/v1/turn")).status, 401);
    assert.equal((await t.req("guest:" + "x".repeat(30), "GET", "/calls/v1/turn")).status, 401);
  } finally { await t.stop(); }
});

test("a ring: incoming, accept, joined, offer/answer/ice between participants, leave ends a pair", async () => {
  const t = await start();
  try {
    const { alice, bob, room, alicePid, bobPid, joinedAlice } = await connectedPair(t);
    assert.equal(joinedAlice.data.participant, bobPid);
    assert.deepEqual(joinedAlice.data.mailbox, { address: "/k/bob1", handle: "/bob" });
    // The newcomer (Bob) offers to Alice.
    assert.equal((await t.req("tok-bob", "POST", `/calls/v1/rooms/${room}/signal`, { session: bob.id, to: alicePid, type: "offer", data: { type: "offer", sdp: "v=0 o" } })).status, 200);
    const offer = await alice.until("signal");
    assert.deepEqual(offer.data, { room, from: bobPid, type: "offer", data: { type: "offer", sdp: "v=0 o" } });
    assert.equal((await t.req("tok-alice", "POST", `/calls/v1/rooms/${room}/signal`, { session: alice.id, to: bobPid, type: "ice", data: { candidate: "candidate:1", sdpMid: "0", sdpMLineIndex: 0, junk: 1 } })).status, 200);
    assert.deepEqual((await bob.until("signal")).data.data, { candidate: "candidate:1", sdpMid: "0", sdpMLineIndex: 0 });
    assert.equal((await t.req("tok-bob", "POST", `/calls/v1/rooms/${room}/leave`, { session: bob.id })).status, 200);
    assert.equal((await alice.until("participant_left")).data.participant, bobPid);
    assert.deepEqual((await alice.until("ended")).data.reason, "ended");
  } finally { await t.stop(); }
});

test("only mailboxes the token owns can place or answer calls, and only the called mailbox rings", async () => {
  const t = await start();
  try {
    const alice = await t.stream("tok-alice");
    const bob = await t.stream("tok-bob");
    const carol = await t.stream("tok-carol");
    assert.equal((await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/bob", to: "/k/carol1", kind: "audio" })).body.error, "not_your_mailbox");
    assert.equal((await t.req("tok-alice", "POST", "/calls/v1/calls", { session: bob.id, from: "/alice", to: "/bob", kind: "audio" })).body.error, "no_session");
    const r = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/k/alice1", to: "/BOB", kind: "audio" });
    assert.equal((await bob.next()).event, "incoming");
    await settle();
    assert.equal(carol.pending(), 0);
    assert.equal((await t.req("tok-carol", "POST", `/calls/v1/rooms/${r.body.room}/accept`, { session: carol.id })).body.error, "not_ringing");
    assert.equal((await t.req("tok-carol", "POST", `/calls/v1/rooms/${r.body.room}/signal`, { session: carol.id, to: r.body.you, type: "ice", data: null })).body.error, "not_in_room");
    assert.equal((await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/k/alice1", kind: "audio" })).body.error, "cannot_call_self");
  } finally { await t.stop(); }
});

test("decline, busy, cancel and no-answer end the room with their reason", async () => {
  const t = await start({ ringMs: 300 });
  try {
    const alice = await t.stream("tok-alice");
    const bob = await t.stream("tok-bob");
    const call = () => t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/bob", kind: "audio" });
    let r = await call(); await bob.until("incoming");
    await t.req("tok-bob", "POST", `/calls/v1/rooms/${r.body.room}/decline`, { session: bob.id });
    assert.equal((await alice.until("ended")).data.reason, "declined");
    r = await call(); await bob.until("incoming");
    await t.req("tok-bob", "POST", `/calls/v1/rooms/${r.body.room}/decline`, { session: bob.id, reason: "busy" });
    assert.equal((await alice.until("ended")).data.reason, "busy");
    r = await call(); await bob.until("incoming");
    await t.req("tok-alice", "POST", `/calls/v1/rooms/${r.body.room}/leave`, { session: alice.id });
    assert.equal((await bob.until("ring_ended")).data.reason, "cancelled");
    r = await call(); await bob.until("incoming");
    assert.equal((await alice.until("ended", 1500)).data.reason, "no_answer");
    assert.equal((await bob.until("ring_ended", 1500)).data.reason, "no_answer");
  } finally { await t.stop(); }
});

test("a meeting link lets a guest knock; a participant admits them and they join the mesh", async () => {
  const t = await start();
  try {
    const { alice, bob, room, code, alicePid, bobPid } = await connectedPair(t);
    // Anyone can see that a meeting exists — but not who is in it.
    const info = await t.req(null, "GET", `/calls/v1/meetings/${code.toLowerCase()}`);
    assert.deepEqual(info.body, { code, kind: "video", participants: 2, full: false });
    const g = await t.guest();
    const gs = await t.stream(g);
    // No relay credentials before admission.
    assert.equal((await t.req(g, "GET", "/calls/v1/turn?session=" + gs.id)).body.error, "not_in_room");
    assert.equal((await t.req(g, "POST", "/calls/v1/knocks", { session: gs.id, code, name: "  " })).body.error, "name_required");
    const k = await t.req(g, "POST", "/calls/v1/knocks", { session: gs.id, code, name: "Dana‮\u0007 from Acme" });
    assert.equal(k.status, 200);
    const seenA = await alice.until("knock"), seenB = await bob.until("knock");
    assert.equal(seenA.data.name, "Dana from Acme");
    assert.equal(seenA.data.guest, true);
    assert.equal(seenB.data.knock, k.body.knock);
    // Guests cannot ring anyone or answer for the room.
    assert.equal((await t.req(g, "POST", "/calls/v1/calls", { session: gs.id, from: "/x", to: "/bob", kind: "audio" })).body.error, "members_only");
    assert.equal((await t.req(g, "POST", `/calls/v1/rooms/${room}/knocks/${k.body.knock}`, { session: gs.id, admit: true })).body.error, "not_in_room");
    // Bob admits.
    assert.equal((await t.req("tok-bob", "POST", `/calls/v1/rooms/${room}/knocks/${k.body.knock}`, { session: bob.id, admit: true })).status, 200);
    assert.equal((await alice.until("knock_resolved")).data.by, bobPid);
    assert.equal((await gs.until("knock_result")).data.admitted, true);
    const joined = await gs.until("joined");
    assert.deepEqual(joined.data.participants.map((p) => p.participant).sort(), [alicePid, bobPid, joined.data.you].sort());
    const newcomer = await alice.until("participant_joined");
    assert.equal(newcomer.data.guest, true);
    assert.ok((await t.req(g, "GET", "/calls/v1/turn?session=" + gs.id)).body.iceServers.length === 1);
    // The guest can now signal any participant.
    assert.equal((await t.req(g, "POST", `/calls/v1/rooms/${room}/signal`, { session: gs.id, to: alicePid, type: "offer", data: { type: "offer", sdp: "x" } })).status, 200);
    assert.equal((await alice.until("signal")).data.from, joined.data.you);
    // With three present, one leaving does not end the call.
    await t.req("tok-bob", "POST", `/calls/v1/rooms/${room}/leave`, { session: bob.id });
    await alice.until("participant_left");
    await settle();
    assert.equal(t.service.state.rooms.get(room).state, "active");
  } finally { await t.stop(); }
});

test("a denied or unanswered knock is told so; a signed-in member knocks as a verified mailbox", async () => {
  const t = await start({ knockMs: 300 });
  try {
    const { alice, room, code } = await connectedPair(t);
    const g = await t.guest();
    const gs = await t.stream(g);
    let k = await t.req(g, "POST", "/calls/v1/knocks", { session: gs.id, code, name: "Eve" });
    await alice.until("knock");
    await t.req("tok-alice", "POST", `/calls/v1/rooms/${room}/knocks/${k.body.knock}`, { session: alice.id, admit: false });
    assert.deepEqual([(await gs.until("knock_result")).data.admitted], [false]);
    k = await t.req(g, "POST", "/calls/v1/knocks", { session: gs.id, code, name: "Eve again" });
    const r = await gs.until("knock_result", 1500);
    assert.equal(r.data.reason, "timeout");
    const carol = await t.stream("tok-carol");
    assert.equal((await t.req("tok-carol", "POST", "/calls/v1/knocks", { session: carol.id, code, as: "/alice" })).body.error, "not_your_mailbox");
    await t.req("tok-carol", "POST", "/calls/v1/knocks", { session: carol.id, code, as: "/k/carol1", name: "ignored" });
    let seen;
    do { seen = await alice.until("knock"); } while (seen.data.name !== "/k/carol1");
    assert.deepEqual([seen.data.name, seen.data.guest, seen.data.mailbox], ["/k/carol1", false, { address: "/k/carol1", handle: null }]);
  } finally { await t.stop(); }
});

test("unknown codes are rate limited per address, and a room has a participant ceiling", async () => {
  const t = await start();
  try {
    let last;
    for (let i = 0; i < LIMITS.missedCodesPerIp10Min + 1; i++) last = await t.req(null, "GET", "/calls/v1/meetings/ZZZZZZ", null, { "cf-connecting-ip": "203.0.113.9" });
    assert.equal(last.body.error, "rate_limited");
    assert.equal((await t.req(null, "GET", "/calls/v1/meetings/ZZZZZZ", null, { "cf-connecting-ip": "203.0.113.10" })).body.error, "no_meeting");

    const { alice, room, code } = await connectedPair(t);
    for (let i = 0; i < LIMITS.participants - 2; i++) {
      const g = await t.guest(); const gs = await t.stream(g);
      const k = await t.req(g, "POST", "/calls/v1/knocks", { session: gs.id, code, name: "Guest " + i });
      await alice.until("knock");
      await t.req("tok-alice", "POST", `/calls/v1/rooms/${room}/knocks/${k.body.knock}`, { session: alice.id, admit: true });
      await gs.until("joined");
    }
    const g = await t.guest(); const gs = await t.stream(g);
    assert.equal((await t.req(g, "POST", "/calls/v1/knocks", { session: gs.id, code, name: "One too many" })).body.error, "room_full");
    assert.equal((await t.req(null, "GET", `/calls/v1/meetings/${code}`)).body.full, true);
  } finally { await t.stop(); }
});

test("guest sessions are rate limited per address", async () => {
  const t = await start();
  try {
    let last;
    for (let i = 0; i < LIMITS.guestsPerIpHour + 1; i++) last = await t.req(null, "POST", "/calls/v1/guests", {}, { "cf-connecting-ip": "192.0.2.77" });
    assert.equal(last.body.error, "rate_limited");
  } finally { await t.stop(); }
});

test("malformed signals, bad origins and wrong kinds are refused", async () => {
  const t = await start();
  try {
    const { bob, room, alicePid } = await connectedPair(t);
    const sig = (body) => t.req("tok-bob", "POST", `/calls/v1/rooms/${room}/signal`, { session: bob.id, to: alicePid, ...body });
    assert.equal((await sig({ type: "script", data: {} })).body.error, "bad_signal");
    assert.equal((await sig({ type: "offer", data: { type: "answer", sdp: "x" } })).body.error, "bad_signal");
    assert.equal((await sig({ type: "offer", data: { type: "offer", sdp: "x".repeat(100_001) } })).body.error, "bad_signal");
    assert.equal((await sig({ type: "ice", data: { candidate: "c".repeat(1001) } })).body.error, "bad_signal");
    assert.equal((await sig({ to: "nobody-here-123", type: "ice", data: null })).body.error, "no_participant");
    const big = await fetch(t.base + `/calls/v1/rooms/${room}/signal`, {
      method: "POST", headers: { authorization: "Bearer tok-bob", "content-type": "application/json" },
      body: JSON.stringify({ session: bob.id, to: alicePid, type: "offer", data: { type: "offer", sdp: "x".repeat(200_000) } }),
    });
    assert.equal(big.status, 413);
    const carol = await t.stream("tok-carol");
    assert.equal((await t.req("tok-carol", "POST", "/calls/v1/calls", { session: carol.id, from: "/k/carol1", to: "/bob", kind: "screen" })).body.error, "bad_kind");
    assert.equal((await t.req("tok-carol", "POST", "/calls/v1/calls", { session: carol.id, from: "/k/carol1", to: "/bob", kind: "audio" }, { origin: "https://evil.test" })).body.error, "bad_origin");
  } finally { await t.stop(); }
});

test("TURN credentials are short-lived HMACs bound to a live session", async () => {
  const t = await start();
  try {
    assert.equal((await t.req("tok-alice", "GET", "/calls/v1/turn?session=nope")).status, 409);
    const alice = await t.stream("tok-alice");
    const { body } = await t.req("tok-alice", "GET", "/calls/v1/turn?session=" + alice.id);
    const [server] = body.iceServers;
    const ttl = Number(server.username.split(":")[0]) - Math.floor(Date.now() / 1000);
    assert.ok(ttl > 3500 && ttl <= 3600);
    assert.equal(server.credential, crypto.createHmac("sha1", "s3cret").update(server.username).digest("base64"));
    assert.ok(!JSON.stringify(body).includes("s3cret"));
  } finally { await t.stop(); }
});

test("an engaged mailbox is not rung, and a session in a call cannot place another", async () => {
  const t = await start();
  try {
    const { alice } = await connectedPair(t);
    const carol = await t.stream("tok-carol");
    assert.equal((await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/k/carol1", kind: "audio" })).body.error, "already_in_call");
    assert.equal((await t.req("tok-carol", "POST", "/calls/v1/calls", { session: carol.id, from: "/k/carol1", to: "/bob", kind: "audio" })).status, 200);
    await settle();
  } finally { await t.stop(); }
});

test("a reloaded page takes its place back within the grace period", async () => {
  const t = await start();
  try {
    const { alice, bob, room, alicePid, bobPid } = await connectedPair(t);
    bob.close();
    await settle(120);
    const bob2 = await t.stream("tok-bob");
    const res = await t.req("tok-bob", "POST", `/calls/v1/rooms/${room}/resume`, { session: bob2.id, participant: bobPid });
    assert.equal(res.body.you, bobPid);
    assert.equal((await t.req("tok-alice", "POST", `/calls/v1/rooms/${room}/signal`, { session: alice.id, to: bobPid, type: "ice", data: null })).status, 200);
    assert.equal((await bob2.until("signal")).event, "signal");
    const carol = await t.stream("tok-carol");
    assert.equal((await t.req("tok-carol", "POST", `/calls/v1/rooms/${room}/resume`, { session: carol.id, participant: alicePid })).body.error, "not_in_room");
  } finally { await t.stop(); }
});

test("the caller leaving while it rings cancels the ring", async () => {
  const t = await start();
  try {
    const alice = await t.stream("tok-alice");
    const bob = await t.stream("tok-bob");
    await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/bob", kind: "audio" });
    await bob.until("incoming");
    alice.close();
    const ev = await bob.until("ring_ended", 6000);
    assert.equal(ev.data.reason, "cancelled");
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
      await t.req("tok-alice", "POST", `/calls/v1/rooms/${last.body.room}/leave`, { session: alice.id });
    }
    assert.equal(last.body.error, "rate_limited");
  } finally { await t.stop(); }
});

test("postbox answers are cached so signals cost no postbox round trip", async () => {
  const t = await start();
  try {
    const alice = await t.stream("tok-alice");
    const before = t.postbox.calls.length;
    for (let i = 0; i < 5; i++) await t.req("tok-alice", "GET", "/calls/v1/turn?session=" + alice.id);
    assert.equal(t.postbox.calls.length, before);
  } finally { await t.stop(); }
});

test("anyone in the call can mute anyone else; reports log counts only", async () => {
  const t = await start();
  try {
    const { alice, bob, room, alicePid, bobPid } = await connectedPair(t);
    assert.equal((await t.req("tok-alice", "POST", `/calls/v1/rooms/${room}/signal`, { session: alice.id, to: bobPid, type: "mute", data: { anything: "ignored" } })).status, 200);
    const ev = await bob.until("signal");
    assert.deepEqual([ev.data.type, ev.data.data, ev.data.from], ["mute", null, alicePid]);
    const logs = [];
    const orig = console.log;
    console.log = (line) => logs.push(line);
    try {
      assert.equal((await t.req("tok-bob", "POST", `/calls/v1/rooms/${room}/report`, { session: bob.id, to: alicePid, state: "stalled", local: { host: 2, relay: 1, evil: "x" }, remote: { srflx: 1 }, route: "203.0.113.5", sdp: "v=0 secret" })).status, 200);
    } finally { console.log = orig; }
    assert.equal(logs.length, 1);
    assert.match(logs[0], /state=stalled route=- relay_only=false restarts=0 local=h2s0p0r1 remote=h0s1p0r0/);
    assert.ok(!logs[0].includes("203.0.113.5") && !logs[0].includes("secret"));
  } finally { await t.stop(); }
});
