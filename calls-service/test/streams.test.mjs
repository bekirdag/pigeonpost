// Live streams in groups (Phase 6): tickets and their scopes, the key directory check, the
// streamer-only key relay, revocation, capacity and the viewer limit.
import { test } from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import crypto from "node:crypto";
import { createCallsService, sfuConfigFromEnv, sfuRoomMbps } from "../src/server.js";

const ACCOUNTS = { "tok-host": "/k/host", "tok-bob": "/k/bob" };
for (let i = 0; i < 40; i++) ACCOUNTS["tok-v" + i] = "/k/v" + i;
const SID = "str_" + "a".repeat(26);
const SID2 = "str_" + "b".repeat(26);
const SECRET = "s".repeat(40);
const pub = (i) => "B" + crypto.createHash("sha256").update("k" + i).digest("base64url").padEnd(86, "A").slice(0, 86);

function fakePostbox() {
  return async (url, opts) => {
    const address = ACCOUNTS[(opts.headers.authorization || "").replace(/^Bearer /, "")];
    if (!address) return { ok: false, status: 401, json: async () => ({}) };
    const u = new URL(url);
    if (u.pathname === "/v1/identities") return { ok: true, status: 200, json: async () => ({ identities: [{ address }] }) };
    if (u.pathname === "/v1/whoami") return { ok: true, status: 200, json: async () => ({ address, handle: null }) };
    return { ok: false, status: 404, json: async () => ({}) };
  };
}

// The postbox's stream decisions, as plain state the tests change.
function fakeInternal() {
  const pb = {
    configured: true,
    streams: new Map(), // sid -> { state, host, roles: Map(mailbox -> role), keys: Map(mailbox -> Set) , max }
    checks: [], ends: [], down: false,
    admit: async () => ({ decision: "ring", targets: [] }), report: () => null, health: () => ({}),
    async streamAdmit({ stream_id, from, public_key }) {
      if (pb.down) return null;
      const s = pb.streams.get(stream_id);
      if (!s) return { allowed: false, reason: "no_stream" };
      if (s.state !== "live") return { allowed: false, reason: "stream_ended" };
      const role = s.roles.get(from.address);
      if (!role) return { allowed: false, reason: "not_member" };
      return { allowed: true, role, group_id: "grp_" + "c".repeat(26), host: s.host, host_key: s.hostKey, sources: ["camera", "screen"], state: "live", ends_by: 0, max_viewers: s.max || 200, key_attested: Boolean(public_key && (s.keys.get(from.address) || new Set()).has(public_key)) };
    },
    async streamCheck({ stream_id, viewers, members }) {
      pb.checks.push({ stream_id, viewers, members });
      if (pb.down) return null;
      const s = pb.streams.get(stream_id);
      const roles = new Map(members.map((m) => [m, s && s.state === "live" ? s.roles.get(m) || null : null]));
      return { state: s ? s.state : "ended", reason: s && s.reason, roles, host_key: s && s.hostKey };
    },
    async streamEnd(x) { pb.ends.push(x); return { ok: true }; },
    live(sid, extra = {}) {
      const s = { state: "live", host: "/k/host", hostKey: pub("host"), roles: new Map([["/k/host", "host"], ["/k/bob", "viewer"]]), keys: new Map([["/k/host", new Set([pub("host")])], ["/k/bob", new Set([pub("bob")])]]), ...extra };
      for (let i = 0; i < 40; i++) { s.roles.set("/k/v" + i, "viewer"); s.keys.set("/k/v" + i, new Set([pub("v" + i)])); }
      pb.streams.set(sid, s);
      return s;
    },
  };
  return pb;
}

async function start({ budget = 400, streams = {}, limits = {} } = {}) {
  const sfu = sfuConfigFromEnv({ CALLS_SFU_URL: "wss://inbox.test/sfu", CALLS_SFU_API_URL: "https://media.test:7443", CALLS_SFU_KEY: "APIkey1", CALLS_SFU_SECRET: SECRET, CALLS_SFU_BUDGET_MBPS: String(budget) });
  const internal = fakeInternal();
  const sfuCalls = [];
  const cfg = { postbox: "https://postbox.test", turnSecret: "", turnUrls: [], allowedOrigins: ["https://inbox.test"], ringMs: 400, knockMs: 1500, trustProxy: true, meshMax: 6, sfu, streams: { viewerMbps: 2, startReserve: 10, maxViewers: 200, totalViewers: 400, ...streams } };
  const service = createCallsService(cfg, { fetch: fakePostbox(), postboxInternal: internal, sfuApi: async (_s, method, body) => { sfuCalls.push({ method, body }); return 200; }, streamLimits: { tickMs: 60_000, viewerCountMs: 20, reactionFlushMs: 20, ...limits } });
  const server = http.createServer((req, res) => service.handle(req, res));
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const base = `http://127.0.0.1:${server.address().port}`;
  const open = [];
  const t = {
    service, internal, sfuCalls,
    async req(auth, path, body) {
      const res = await fetch(base + path, { method: "POST", headers: { authorization: "Bearer " + auth, "content-type": "application/json" }, body: JSON.stringify(body || {}) });
      return { status: res.status, body: await res.json().catch(() => null) };
    },
    async session(auth) {
      const ac = new AbortController();
      const res = await fetch(base + "/calls/v1/events", { headers: { authorization: "Bearer " + auth }, signal: ac.signal });
      const reader = res.body.getReader(); const dec = new TextDecoder();
      let buf = ""; const queue = [], waiters = [];
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
              const w = waiters.shift(); if (w) w(item); else queue.push(item);
            }
          }
        } catch (_) { /* closed */ }
      })();
      const s = {
        auth, close: () => ac.abort(),
        next: (ms = 2000) => queue.length ? Promise.resolve(queue.shift()) : new Promise((resolve, reject) => { const tm = setTimeout(() => reject(new Error("no event")), ms); waiters.push((x) => { clearTimeout(tm); resolve(x); }); }),
        async until(name, ms = 2000) { for (;;) { const e = await s.next(ms); if (e.event === name) return e; } },
        drain() { queue.length = 0; },
      };
      s.id = (await s.next()).data.session;
      open.push(s);
      return s;
    },
    async join(auth, sid = SID, extra = {}) {
      const s = await t.session(auth);
      const who = ACCOUNTS[auth];
      const key = auth === "tok-host" ? pub("host") : pub(who.slice(3));
      const r = await t.req(auth, `/calls/v1/streams/${sid}/join`, { session: s.id, from: who, public_key: key, e2ee: true, ...extra });
      return { s, r };
    },
    async stop() { for (const s of open) s.close(); await new Promise((r) => server.close(r)); server.closeAllConnections?.(); },
  };
  return t;
}

function claims(token) {
  const [h, b, sig] = token.split(".");
  assert.equal(crypto.createHmac("sha256", SECRET).update(h + "." + b).digest("base64url"), sig);
  return JSON.parse(Buffer.from(b, "base64url"));
}
const settle = (ms = 60) => new Promise((r) => setTimeout(r, ms));

test("the streamer gets a publishing ticket, viewers a hidden subscribe-only one, in the stream's own room", async () => {
  const t = await start();
  try {
    t.internal.live(SID);
    const host = await t.join("tok-host");
    assert.equal(host.r.status, 200, JSON.stringify(host.r.body));
    assert.equal(host.r.body.role, "host");
    const h = claims(host.r.body.sfu.token);
    assert.equal(h.video.room, "stream_" + SID);
    assert.equal(h.video.canPublish, true);
    assert.deepEqual(h.video.canPublishSources, ["microphone", "camera", "screen_share"]);
    assert.equal(h.video.canPublishData, false);
    assert.equal(h.sub, host.r.body.you);
    assert.ok(h.exp - h.nbf <= 130);
    const bob = await t.join("tok-bob");
    assert.equal(bob.r.status, 200, JSON.stringify(bob.r.body));
    const v = claims(bob.r.body.sfu.token).video;
    assert.deepEqual({ canPublish: v.canPublish, canPublishData: v.canPublishData, canSubscribe: v.canSubscribe, hidden: v.hidden, sources: v.canPublishSources }, { canPublish: false, canPublishData: false, canSubscribe: true, hidden: true, sources: [] });
    assert.equal(bob.r.body.host.participant, host.r.body.you);
    assert.equal(bob.r.body.host.key, pub("host"));
    // The streamer hears about the viewer (with their key); a refreshed ticket keeps the scope.
    const joined = await host.s.until("stream_participant");
    assert.equal(joined.data.mailbox, "/k/bob");
    assert.equal(joined.data.key, pub("bob"));
    const again = await t.req("tok-bob", `/calls/v1/streams/${SID}/sfu`, { session: bob.s.id });
    assert.equal(claims(again.body.token).video.canPublish, false);
    // Stream rooms are not call rooms.
    assert.equal(t.service.state.rooms.size, 0);
  } finally { await t.stop(); }
});

test("joining needs frame encryption, a key registered with the postbox, a live stream and the streamer first", async () => {
  const t = await start();
  try {
    t.internal.live(SID);
    let { r } = await t.join("tok-bob");
    assert.deepEqual([r.status, r.body.error], [409, "not_started"]);
    ({ r } = await t.join("tok-host", SID, { e2ee: false }));
    assert.deepEqual([r.status, r.body.error], [409, "needs_e2ee"]);
    ({ r } = await t.join("tok-host", SID, { public_key: pub("intruder") }));
    assert.deepEqual([r.status, r.body.error], [409, "key_not_registered"]);
    ({ r } = await t.join("tok-host"));
    assert.equal(r.status, 200);
    ({ r } = await t.join("tok-bob", SID, { public_key: pub("not-bobs") }));
    assert.deepEqual([r.status, r.body.error], [409, "key_not_registered"]);
    ({ r } = await t.join("tok-bob", SID2));
    assert.deepEqual([r.status, r.body.error], [404, "no_stream"]);
    t.internal.streams.get(SID).roles.delete("/k/bob");
    ({ r } = await t.join("tok-bob"));
    assert.deepEqual([r.status, r.body.error], [403, "not_member"]);
    t.internal.down = true;
    ({ r } = await t.join("tok-v1"));
    assert.deepEqual([r.status, r.body.error], [503, "streams_unavailable"]);
  } finally { await t.stop(); }
});

test("only the streamer relays wrapped keys, and only to people in the stream", async () => {
  const t = await start();
  try {
    t.internal.live(SID);
    const host = await t.join("tok-host");
    const bob = await t.join("tok-bob");
    const wrap = { to: bob.r.body.you, iv: "A".repeat(16), key: "B".repeat(64) };
    const r = await t.req("tok-host", `/calls/v1/streams/${SID}/keys`, { session: host.s.id, epoch: 1, wraps: [wrap, { to: "nobody-here-xx", iv: "A".repeat(16), key: "B".repeat(64) }] });
    assert.deepEqual(r.body, { ok: true, delivered: 1 });
    const got = await bob.s.until("stream_key");
    assert.deepEqual(got.data, { stream: SID, from: host.r.body.you, epoch: 1, iv: wrap.iv, key: wrap.key });
    const forged = await t.req("tok-bob", `/calls/v1/streams/${SID}/keys`, { session: bob.s.id, epoch: 2, wraps: [{ ...wrap, to: host.r.body.you }] });
    assert.deepEqual([forged.status, forged.body.error], [403, "not_host"]);
    const bad = await t.req("tok-host", `/calls/v1/streams/${SID}/keys`, { session: host.s.id, epoch: 1, wraps: [{ ...wrap, key: "short" }] });
    assert.equal(bad.status, 400);
  } finally { await t.stop(); }
});

test("a viewer removed from the group is removed from the media server and the streamer is told", async () => {
  const t = await start();
  try {
    t.internal.live(SID);
    const host = await t.join("tok-host");
    const bob = await t.join("tok-bob");
    await host.s.until("stream_participant");
    t.internal.streams.get(SID).roles.delete("/k/bob");
    const r = await t.req("tok-host", `/calls/v1/streams/${SID}/recheck`, { session: host.s.id });
    assert.equal(r.status, 200);
    let left;
    do left = await host.s.until("stream_participant"); while (left.data.change !== "left");
    assert.deepEqual([left.data.change, left.data.reason, left.data.mailbox], ["left", "revoked", "/k/bob"]);
    assert.equal((await bob.s.until("stream_ended")).data.reason, "revoked");
    await settle();
    assert.ok(t.sfuCalls.some((c) => c.method === "RemoveParticipant" && c.body.identity === bob.r.body.you && c.body.room === "stream_" + SID));
    // Their tickets stop too.
    const again = await t.req("tok-bob", `/calls/v1/streams/${SID}/sfu`, { session: bob.s.id });
    assert.equal(again.status, 403);
    assert.equal(t.internal.checks.at(-1).viewers, 1);
  } finally { await t.stop(); }
});

test("accepting the stage grants publishing on the media server; leaving it revokes", async () => {
  const t = await start();
  try {
    const s = t.internal.live(SID);
    const host = await t.join("tok-host");
    const bob = await t.join("tok-bob");
    s.roles.set("/k/bob", "speaker");
    const r = await t.req("tok-bob", `/calls/v1/streams/${SID}/role`, { session: bob.s.id });
    assert.equal(r.body.role, "speaker");
    await settle();
    const up = t.sfuCalls.filter((c) => c.method === "UpdateParticipant").at(-1);
    assert.deepEqual(up.body, { room: "stream_" + SID, identity: bob.r.body.you, permission: { can_subscribe: true, can_publish: true, can_publish_data: false, can_publish_sources: ["MICROPHONE", "CAMERA"], hidden: false } });
    assert.equal(claims((await t.req("tok-bob", `/calls/v1/streams/${SID}/sfu`, { session: bob.s.id })).body.token).video.canPublish, true);
    assert.equal((await host.s.until("stream_participant")).data.change, "joined");
    // The streamer takes them off: the next check revokes on the media server.
    s.roles.set("/k/bob", "viewer");
    await t.req("tok-host", `/calls/v1/streams/${SID}/recheck`, { session: host.s.id });
    await settle();
    const down = t.sfuCalls.filter((c) => c.method === "UpdateParticipant").at(-1);
    assert.equal(down.body.permission.can_publish, false);
    assert.deepEqual(down.body.permission.can_publish_sources, []);
    let role;
    do role = await bob.s.until("stream_role"); while (role.data.role !== "viewer");
  } finally { await t.stop(); }
});

test("starting and watching are refused with stream_capacity beyond the budget, and the viewer limit holds", async () => {
  // Budget 60 Mbit/s at 2 Mbit/s per viewer, a start reserve of 10: one stream fits with 30 viewers.
  const t = await start({ budget: 60, streams: { maxViewers: 200 } });
  try {
    t.internal.live(SID);
    t.internal.live(SID2);
    assert.equal((await t.join("tok-host")).r.status, 200);
    const second = await t.join("tok-host", SID2);
    assert.equal(second.r.status, 200, "20 Mbit/s reserved, 40 left");
    const ok = [];
    for (let i = 0; i < 21; i++) ok.push((await t.join("tok-v" + i)).r);
    // Room for 60 Mbit/s: stream 2 reserves 10 viewers (20), so stream 1 may hold 20.
    assert.ok(ok.slice(0, 20).every((r) => r.status === 200), JSON.stringify(ok.map((r) => r.status)));
    assert.deepEqual([ok[20].status, ok[20].body.error], [503, "stream_capacity"]);
    assert.match(ok[20].body.message, /try again/);
  } finally { await t.stop(); }
  // The postbox's (or the configured) viewer limit gives stream_full, not capacity.
  const t2 = await start({ budget: 1000 });
  try {
    t2.internal.live(SID, { max: 3 });
    await t2.join("tok-host");
    for (let i = 0; i < 3; i++) assert.equal((await t2.join("tok-v" + i)).r.status, 200);
    const r = (await t2.join("tok-v9")).r;
    assert.deepEqual([r.status, r.body.error, r.body.max_viewers], [409, "stream_full", 3]);
  } finally { await t2.stop(); }
  // Calls and streams share one budget: a stream counts against large calls.
  const t3 = await start({ budget: 39 });
  try {
    t3.internal.live(SID);
    await t3.join("tok-host");
    assert.equal(t3.service.sfuLoadMbps(), 20);
    const model = sfuConfigFromEnv({}).model;
    assert.ok(t3.service.sfuLoadMbps() + sfuRoomMbps(7, model) > 39);
    const r = (await t3.join("tok-host", SID2)).r;
    assert.equal(r.status, 404, "an unknown stream is refused before capacity");
    t3.internal.live(SID2);
    const r2 = (await t3.join("tok-host", SID2)).r;
    assert.deepEqual([r2.status, r2.body.error], [503, "stream_capacity"]);
  } finally { await t3.stop(); }
});

test("reactions are sealed blobs, rate limited and batched to everyone", async () => {
  const t = await start();
  try {
    t.internal.live(SID);
    const host = await t.join("tok-host");
    const bob = await t.join("tok-bob");
    const sealed = "x".repeat(40);
    for (let i = 0; i < 6; i++) assert.equal((await t.req("tok-bob", `/calls/v1/streams/${SID}/react`, { session: bob.s.id, sealed })).status, 200);
    assert.equal((await t.req("tok-bob", `/calls/v1/streams/${SID}/react`, { session: bob.s.id, sealed })).status, 429);
    assert.equal((await t.req("tok-bob", `/calls/v1/streams/${SID}/react`, { session: bob.s.id, sealed: "<b>hi</b>" })).status, 400);
    const batch = await host.s.until("stream_reactions");
    assert.ok(batch.data.items.length >= 1 && batch.data.items.every((x) => x.sealed === sealed && x.from === bob.r.body.you));
    const count = await bob.s.until("stream_viewers");
    assert.equal(count.data.count, 1);
  } finally { await t.stop(); }
});

test("the stream ends when the streamer stops, leaves for good, or the postbox says so", async () => {
  const t = await start({ limits: { hostGraceMs: 50, blindMs: -1 } });
  try {
    const s = t.internal.live(SID);
    const host = await t.join("tok-host");
    const bob = await t.join("tok-bob");
    // The postbox ended it (an admin stopped it): the next check closes the room.
    s.state = "ended"; s.reason = "stopped_by_admin";
    await t.req("tok-host", `/calls/v1/streams/${SID}/recheck`, { session: host.s.id }).catch(() => {});
    assert.equal((await bob.s.until("stream_ended")).data.reason, "stopped_by_admin");
    await settle();
    assert.ok(t.sfuCalls.some((c) => c.method === "DeleteRoom" && c.body.room === "stream_" + SID));
    // The streamer's page goes away: after the grace period the stream ends and the postbox hears.
    t.internal.live(SID2);
    const h2 = await t.join("tok-host", SID2);
    h2.s.close();
    await settle(200);
    assert.deepEqual(t.internal.ends.at(-1), { stream_id: SID2, reason: "host_left" });
    // A postbox that cannot be asked stops the stream rather than guessing who may stay.
    const s3 = "str_" + "d".repeat(26);
    t.internal.live(s3);
    const h3 = await t.join("tok-host", s3);
    t.internal.down = true;
    await t.service.streams.check(t.service.streams.streams.get(s3));
    assert.equal((await h3.s.until("stream_ended")).data.reason, "media_failed");
  } finally { await t.stop(); }
});
