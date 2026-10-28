// Phase 1b: call admission and call-history reporting through the postbox's internal API.
import { test } from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import { createCallsService } from "../src/server.js";
import { createPostboxInternal } from "../src/postbox-internal.js";

const ACCOUNTS = {
  "tok-alice": [{ address: "/k/alice1", handle: "/alice" }],
  "tok-bob": [{ address: "/k/bob1", handle: "/bob" }],
  "tok-carol": [{ address: "/k/carol1", handle: null }],
};

// The public postbox (member auth) and the internal one, in one fake.
function fakePostbox({ decision = "ring", admissionStatus = 200, eventStatus = () => 200 } = {}) {
  const admissions = [], events = [];
  const state = { decision, admissionStatus };
  const fetch = async (url, opts = {}) => {
    const u = new URL(url);
    if (u.pathname.startsWith("/internal/v1/")) {
      assert.equal(opts.headers.authorization, "Bearer svc_calls_test_token_0123456789abcdef");
      const body = JSON.parse(opts.body);
      if (u.pathname === "/internal/v1/call-admission") {
        admissions.push(body);
        if (state.admissionStatus !== 200) return { ok: false, status: state.admissionStatus, json: async () => ({}) };
        const targets = body.to === "/bob" || body.to === "/k/bob1" ? ["/k/bob1"] : [];
        return { ok: true, status: 200, json: async () => ({ decision: state.decision, targets, reason: "test" }) };
      }
      if (u.pathname === "/internal/v1/call-events") {
        events.push(body);
        const status = eventStatus(body);
        return { ok: status < 300, status, json: async () => ({ accepted: true }) };
      }
      return { ok: false, status: 404, json: async () => ({}) };
    }
    const owned = ACCOUNTS[(opts.headers.authorization || "").replace(/^Bearer /, "")];
    if (!owned) return { ok: false, status: 401, json: async () => ({}) };
    if (u.pathname === "/v1/identities") return { ok: true, status: 200, json: async () => ({ identities: owned.map((m) => ({ address: m.address })) }) };
    if (u.pathname === "/v1/whoami") {
      const m = owned.find((x) => x.address === u.searchParams.get("identity"));
      return m ? { ok: true, status: 200, json: async () => m } : { ok: false, status: 403, json: async () => ({}) };
    }
    return { ok: false, status: 404, json: async () => ({}) };
  };
  return { fetch, admissions, events, state };
}

let codeSeq = 0;
async function start(pb, { internal = true, ringMs = 300 } = {}) {
  const cfg = {
    postbox: "https://postbox.test", turnSecret: "", turnUrls: [], turnTtlSeconds: 3600,
    allowedOrigins: ["https://inbox.test"], ringMs, knockMs: 1500, trustProxy: true,
    postboxInternalUrl: internal ? "http://10.0.0.1:18991" : "", postboxServiceToken: internal ? "svc_calls_test_token_0123456789abcdef" : "",
  };
  const service = createCallsService(cfg, { fetch: pb.fetch, randomCode: () => "CODE" + String(10 + (codeSeq++ % 80)) });
  const server = http.createServer((req, res) => service.handle(req, res));
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const base = `http://127.0.0.1:${server.address().port}`;
  const open = [];
  const t = {
    service, base,
    async req(auth, method, path, body) {
      const res = await fetch(base + path, {
        method, headers: { authorization: "Bearer " + auth, ...(body ? { "content-type": "application/json" } : {}) },
        body: body ? JSON.stringify(body) : undefined,
      });
      return { status: res.status, body: await res.json().catch(() => null) };
    },
    async stream(auth) {
      const ac = new AbortController();
      const res = await fetch(base + "/calls/v1/events", { headers: { authorization: "Bearer " + auth }, signal: ac.signal });
      const reader = res.body.getReader();
      const dec = new TextDecoder();
      let buf = "";
      const seen = [], queue = [], waiters = [];
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
              seen.push(item);
              const w = waiters.shift();
              if (w) w(item); else queue.push(item);
            }
          }
        } catch (_) { /* aborted */ }
      })();
      const s = {
        seen,
        next: (ms = 2000) => queue.length ? Promise.resolve(queue.shift()) : new Promise((resolve, reject) => {
          const timer = setTimeout(() => reject(new Error("no event")), ms);
          waiters.push((x) => { clearTimeout(timer); resolve(x); });
        }),
        async until(name, ms = 2000) { for (;;) { const e = await s.next(ms); if (e.event === name) return e; } },
        pending: () => queue.length,
        close: () => ac.abort(),
      };
      s.id = (await s.next()).data.session;
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
const names = (events) => events.map((e) => e.event);
const strip = (events) => events.map((e) => ({ event: e.event, to: e.to, by: e.by, seq: e.seq }));

// What the caller sees for one call to Bob that nobody answers, under a given admission decision.
async function callerView(decision) {
  const pb = fakePostbox({ decision });
  const t = await start(pb);
  try {
    const alice = await t.stream("tok-alice");
    const bob = await t.stream("tok-bob");
    const placed = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/bob", kind: "audio" });
    const ended = await alice.until("ended", 1500);
    await settle();
    const bobRang = bob.seen.some((e) => e.event === "incoming");
    const response = { status: placed.status, keys: Object.keys(placed.body).sort(), state: placed.body.state, participants: placed.body.participants.length };
    return { response, aliceEvents: names(alice.seen.slice(1)), reason: ended.data.reason, bobRang, events: pb.events };
  } finally { await t.stop(); }
}

test("block and silence look to the caller exactly like a genuine no-answer", async () => {
  const ring = await callerView("ring");
  const block = await callerView("block");
  const silence = await callerView("silence");
  assert.equal(ring.bobRang, true);
  assert.equal(block.bobRang, false);
  assert.equal(silence.bobRang, false);
  for (const v of [block, silence]) {
    assert.deepEqual(v.response, ring.response);
    assert.deepEqual(v.aliceEvents, ring.aliceEvents);
    assert.equal(v.reason, "no_answer");
  }
  // The caller's own history gets ring then missed in all three cases.
  assert.deepEqual(strip(ring.events), [
    { event: "ring", to: ["/k/bob1"], by: undefined, seq: 1 },
    { event: "missed", to: ["/k/bob1"], by: undefined, seq: 2 },
  ]);
  assert.deepEqual(strip(block.events), [
    { event: "ring", to: [], by: undefined, seq: 1 },
    { event: "missed", to: [], by: undefined, seq: 2 },
  ]);
  // Silence files a filtered entry for the callee, and nothing that rings.
  assert.deepEqual(strip(silence.events), [
    { event: "ring", to: [], by: undefined, seq: 1 },
    { event: "filtered", to: ["/k/bob1"], by: undefined, seq: 2 },
    { event: "missed", to: [], by: undefined, seq: 3 },
  ]);
  for (const e of ring.events) {
    assert.deepEqual(e.from, { address: "/k/alice1", handle: "/alice" });
    assert.equal(e.kind, "audio");
    assert.match(e.event_id, /^[0-9a-f]{32}$/);
    assert.ok(Number.isInteger(e.at));
  }
});

test("lifecycle: answer then end, decline, busy and caller cancel are reported in order", async () => {
  const pb = fakePostbox();
  const t = await start(pb, { ringMs: 2000 });
  try {
    const alice = await t.stream("tok-alice");
    const bob = await t.stream("tok-bob");
    const call = () => t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/bob", kind: "video" });

    let r = await call(); await bob.until("incoming");
    await t.req("tok-bob", "POST", `/calls/v1/rooms/${r.body.room}/accept`, { session: bob.id });
    await bob.until("joined");
    await t.req("tok-bob", "POST", `/calls/v1/rooms/${r.body.room}/leave`, { session: bob.id });
    await alice.until("ended");
    await settle();
    let room = pb.events.filter((e) => e.room_id === r.body.room);
    assert.deepEqual(names(room), ["ring", "answer", "end"]);
    assert.equal(room[1].by, "/k/bob1");
    assert.deepEqual(room[1].participants, ["/k/alice1", "/k/bob1"]);
    assert.deepEqual(room[2].participants, ["/k/alice1"]);
    assert.deepEqual(room.map((e) => e.seq), [1, 2, 3]);

    r = await call(); await bob.until("incoming");
    await t.req("tok-bob", "POST", `/calls/v1/rooms/${r.body.room}/decline`, { session: bob.id });
    await alice.until("ended"); await settle();
    room = pb.events.filter((e) => e.room_id === r.body.room);
    assert.deepEqual(names(room), ["ring", "decline"]);
    assert.equal(room[1].by, "/k/bob1");

    r = await call(); await bob.until("incoming");
    await t.req("tok-bob", "POST", `/calls/v1/rooms/${r.body.room}/decline`, { session: bob.id, reason: "busy" });
    await alice.until("ended"); await settle();
    assert.deepEqual(names(pb.events.filter((e) => e.room_id === r.body.room)), ["ring", "busy"]);

    r = await call(); await bob.until("incoming");
    await t.req("tok-alice", "POST", `/calls/v1/rooms/${r.body.room}/leave`, { session: alice.id });
    await bob.until("ring_ended"); await settle();
    assert.deepEqual(names(pb.events.filter((e) => e.room_id === r.body.room)), ["ring", "cancelled"]);
    // Event ids are unique.
    assert.equal(new Set(pb.events.map((e) => e.event_id)).size, pb.events.length);
  } finally { await t.stop(); }
});

test("admission is asked once per 30 s per caller and callee, and fails closed when the postbox is down", async () => {
  const pb = fakePostbox({ decision: "ring" });
  const t = await start(pb, { ringMs: 100 });
  try {
    const alice = await t.stream("tok-alice");
    const bob = await t.stream("tok-bob");
    const call = () => t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/bob", kind: "audio" });
    await call(); await bob.until("incoming"); await alice.until("ended", 1500);
    await call(); await bob.until("incoming"); await alice.until("ended", 1500);
    assert.equal(pb.admissions.length, 1);
    assert.deepEqual(pb.admissions[0], { from: { mailbox: "/k/alice1", handle: "/alice" }, to: "/bob", kind: "audio" });
    assert.equal(t.service.internal.health().cache_hits, 1);
  } finally { await t.stop(); }

  const down = fakePostbox({ admissionStatus: 503 });
  const t2 = await start(down, { ringMs: 100 });
  try {
    const alice = await t2.stream("tok-alice");
    const bob = await t2.stream("tok-bob");
    const placed = await t2.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/bob", kind: "audio" });
    assert.equal(placed.status, 200);
    assert.equal((await alice.until("ended", 1500)).data.reason, "no_answer");
    await settle();
    assert.equal(bob.seen.some((e) => e.event === "incoming"), false);
    assert.equal(t2.service.internal.health().admission_failed, 1);
    const health = await fetch(t2.base + "/calls/v1/health").then((r) => r.json());
    assert.equal(health.postbox.admission_failed, 1);
    assert.equal(health.postbox.configured, true);
    assert.equal(JSON.stringify(health).includes("svc_"), false);
  } finally { await t2.stop(); }
});

test("unconfigured: rings without asking and reports nothing, as before", async () => {
  const pb = fakePostbox({ decision: "block" });
  const t = await start(pb, { internal: false, ringMs: 100 });
  try {
    const alice = await t.stream("tok-alice");
    const bob = await t.stream("tok-bob");
    await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/bob", kind: "audio" });
    await bob.until("incoming");
    await alice.until("ended", 1500);
    await settle();
    assert.equal(pb.admissions.length, 0);
    assert.equal(pb.events.length, 0);
    assert.equal(t.service.internal.health().configured, false);
  } finally { await t.stop(); }
});

// ---- the module on its own, with an injected clock and timers --------------------------------------

function fakeTimers() {
  let t = 1_000_000;
  const timers = [];
  return {
    now: () => t,
    setTimeout: (fn, ms) => { const h = { fn, at: t + ms, unref() {} }; timers.push(h); return h; },
    async advance(ms) {
      t += ms;
      for (;;) {
        timers.sort((a, b) => a.at - b.at);
        const due = timers.findIndex((h) => h.at <= t);
        if (due < 0) break;
        const [h] = timers.splice(due, 1);
        await h.fn();
        await new Promise((r) => setImmediate(r));
      }
    },
  };
}

function scripted(statuses) {
  const posts = [];
  const fetch = async (url, opts) => {
    const body = JSON.parse(opts.body);
    posts.push({ path: new URL(url).pathname, body });
    const s = statuses.length ? statuses.shift() : 200;
    if (s === "throw") throw new Error("ECONNREFUSED");
    if (url.endsWith("/call-admission")) return { ok: s === 200, status: s, json: async () => ({ decision: "silence", targets: ["/k/b"], reason: "unknown" }) };
    return { ok: s < 300, status: s, json: async () => ({}) };
  };
  return { fetch, posts };
}

test("admission cache expires after 30 s; bad responses fail closed", async () => {
  const clock = fakeTimers();
  const net = scripted([200, 200]);
  const pi = createPostboxInternal({ url: "http://x", token: "svc_t", fetch: net.fetch, now: clock.now, setTimeout: clock.setTimeout });
  const ask = () => pi.admit({ from: { address: "/k/a" }, to: "/b", kind: "audio" });
  assert.deepEqual(await ask(), { decision: "silence", targets: ["/k/b"], reason: "unknown" });
  await clock.advance(29_000);
  await ask();
  assert.equal(net.posts.length, 1);
  await clock.advance(2_000);
  await ask();
  assert.equal(net.posts.length, 2);

  const bad = createPostboxInternal({ url: "http://x", token: "svc_t", now: clock.now, setTimeout: clock.setTimeout,
    fetch: async () => ({ ok: true, status: 200, json: async () => ({ decision: "maybe", targets: [] }) }) });
  assert.deepEqual(await bad.admit({ from: { address: "/k/a" }, to: "/b", kind: "audio" }), { decision: "block", targets: [], reason: "admission_unavailable" });
  const unconfigured = createPostboxInternal({});
  assert.deepEqual(await unconfigured.admit({ from: { address: "/k/a" }, to: "/b", kind: "audio" }), { decision: "ring", targets: [], reason: "unconfigured" });
  assert.equal(unconfigured.report({ event: "ring" }), null);
});

test("events retry with backoff and keep their id; 4xx is dropped; the queue is bounded", async () => {
  const clock = fakeTimers();
  const net = scripted(["throw", 503, 429, 200, 400]);
  const pi = createPostboxInternal({ url: "http://x/", token: "svc_t", fetch: net.fetch, now: clock.now, setTimeout: clock.setTimeout });
  const id = pi.report({ room_id: "r1", seq: 1, event: "ring" });
  pi.report({ room_id: "r1", seq: 2, event: "missed" });
  await clock.advance(0);
  assert.equal(net.posts.length, 1); // failed, waits 1 s
  await clock.advance(999);
  assert.equal(net.posts.length, 1);
  await clock.advance(1);
  assert.equal(net.posts.length, 2); // 503, waits 2 s
  await clock.advance(2_000);
  assert.equal(net.posts.length, 3); // 429, waits 4 s
  await clock.advance(4_000);
  // 200 for the first event, then 400 for the second (dropped as rejected).
  assert.equal(net.posts.length, 5);
  assert.ok(net.posts.slice(0, 4).every((p) => p.body.event_id === id && p.path === "/internal/v1/call-events"));
  assert.equal(net.posts[4].body.seq, 2);
  const h = pi.health();
  assert.equal(h.sent, 1);
  assert.equal(h.retried, 3);
  assert.equal(h.rejected, 1);
  assert.equal(h.queued, 0);

  // Give up after the attempt ceiling.
  const net2 = scripted(Array(20).fill(503));
  const pi2 = createPostboxInternal({ url: "http://x", token: "svc_t", fetch: net2.fetch, now: clock.now, setTimeout: clock.setTimeout });
  pi2.report({ event: "end" });
  for (let i = 0; i < 20; i++) await clock.advance(60_000);
  assert.equal(net2.posts.length, 12);
  assert.equal(pi2.health().dropped, 1);
  assert.equal(pi2.health().queued, 0);

  // Bounded: the oldest is dropped when full.
  const pi3 = createPostboxInternal({ url: "http://x", token: "svc_t", fetch: net2.fetch, now: clock.now, setTimeout: () => ({ unref() {} }), limits: { queueMax: 3 } });
  for (let i = 0; i < 5; i++) pi3.report({ seq: i });
  assert.deepEqual(pi3._queue.map((q) => q.body.seq), [2, 3, 4]);
  assert.equal(pi3.health().dropped, 2);
});

// Phase 9: with a spool path, queued call events survive a restart (and stale ones do not).
test("queued call events survive a restart through the spool", async () => {
  const fs = await import("node:fs");
  const os = await import("node:os");
  const path = await import("node:path");
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "pp-calls-spool-"));
  const spoolPath = path.join(dir, "call-events.json");
  const sent = [];
  let up = false;
  const fetch = async (_url, opts) => {
    if (!up) return { ok: false, status: 503, json: async () => ({}) };
    sent.push(JSON.parse(opts.body));
    return { ok: true, status: 200, json: async () => ({}) };
  };
  const opts = { url: "http://postbox.test", token: "svc_calls_test_token_0123456789abcdef", fetch, spoolPath,
    setTimeout: () => null };
  const first = createPostboxInternal(opts);
  const a = first.report({ room_id: "r1", kind: "ring" });
  const b = first.report({ room_id: "r1", kind: "missed" });
  await first.flush(); // postbox down: both stay queued
  first.persist();
  assert.equal((fs.statSync(spoolPath).mode & 0o777), 0o600);

  // A stale entry from long ago is dropped on load.
  const saved = JSON.parse(fs.readFileSync(spoolPath, "utf8"));
  saved.push({ body: { event_id: "f".repeat(32), room_id: "old" }, attempts: 1, firstAt: Date.now() - 31 * 60_000 });
  fs.writeFileSync(spoolPath, JSON.stringify(saved));

  up = true;
  const second = createPostboxInternal(opts);
  assert.equal(second.health().queued, 2);
  await second.flush();
  assert.deepEqual(sent.map((e) => e.event_id), [a, b], "same ids, same order, so the postbox deduplicates");
  assert.equal(second.health().queued, 0);
  second.persist();
  assert.deepEqual(JSON.parse(fs.readFileSync(spoolPath, "utf8")), []);
  fs.rmSync(dir, { recursive: true, force: true });
});

test("without a spool path nothing touches the disk", () => {
  const internal = createPostboxInternal({ url: "http://postbox.test", token: "svc_x_0123456789abcdef0123456789abcdef",
    fetch: async () => ({ ok: false, status: 503, json: async () => ({}) }), setTimeout: () => null });
  internal.report({ room_id: "r1", kind: "ring" });
  internal.persist(); // no-op
  assert.equal(internal.health().queued, 1);
});
