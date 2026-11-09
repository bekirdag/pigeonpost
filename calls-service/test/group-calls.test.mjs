// Phase 3a: calls from persistent groups (docs/groups.md §6).
import { test } from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import { createRequire } from "node:module";
import { createCallsService, LIMITS, sfuConfigFromEnv } from "../src/server.js";

const E = createRequire(import.meta.url)("../../site-inbox/calls-e2ee.js");

const TOKEN = "svc_calls_test_token_0123456789abcdef";
const ACCOUNTS = {
  "tok-alice": [{ address: "/k/alice1", handle: "/alice" }],
  "tok-bob": [{ address: "/k/bob1", handle: "/bob" }],
  "tok-carol": [{ address: "/k/carol1", handle: null }],
  "tok-dave": [{ address: "/k/dave1", handle: "/dave" }],
  "tok-erin": [{ address: "/k/erin1", handle: null }],
  "tok-fred": [{ address: "/k/fred1", handle: null }],
  "tok-gina": [{ address: "/k/gina1", handle: null }],
  "tok-hank": [{ address: "/k/hank1", handle: null }],
};
const GROUP = "grp_abcdefghijklmnopqrstuvwxyz";

// Members of GROUP and how each wants a call from anybody treated.
function fakePostbox({ members = ["/k/alice1", "/k/bob1", "/k/carol1"], decisions = {}, admissionStatus = 200 } = {}) {
  const admissions = [], events = [];
  const state = { members: new Set(members), decisions, admissionStatus };
  const fetch = async (url, opts = {}) => {
    const u = new URL(url);
    if (u.pathname.startsWith("/internal/v1/")) {
      assert.equal(opts.headers.authorization, "Bearer " + TOKEN);
      const body = JSON.parse(opts.body);
      if (u.pathname === "/internal/v1/group-call-admission") {
        admissions.push(body);
        if (state.admissionStatus !== 200) return { ok: false, status: state.admissionStatus, json: async () => ({}) };
        if (body.group_id !== GROUP || !state.members.has(body.from.mailbox)) {
          return { ok: true, status: 200, json: async () => ({ allowed: false, reason: "not_member", members: [] }) };
        }
        const list = [...state.members].filter((a) => a !== body.from.mailbox).map((address) => {
          const owner = Object.values(ACCOUNTS).flat().find((m) => m.address === address);
          return { address, ...(owner && owner.handle ? { handle: owner.handle } : {}), decision: state.decisions[address] || "ring" };
        });
        return { ok: true, status: 200, json: async () => ({ allowed: true, group_name: "Team", members: body.join ? [] : list }) };
      }
      if (u.pathname === "/internal/v1/call-events") {
        events.push(body);
        return { ok: true, status: 200, json: async () => ({ accepted: true }) };
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
async function start(pb, { internal = true, ringMs = 400, extra = {}, sfuApi, ringAlerts } = {}) {
  const cfg = {
    postbox: "https://postbox.test", turnSecret: "", turnUrls: [], turnTtlSeconds: 3600,
    allowedOrigins: ["https://inbox.test"], ringMs, knockMs: 1500, trustProxy: true,
    postboxInternalUrl: internal ? "http://10.0.0.1:18991" : "", postboxServiceToken: internal ? TOKEN : "",
    groupCheckMs: 60_000, ...extra,
  };
  const service = createCallsService(cfg, { fetch: pb.fetch, randomCode: () => "GRPC" + String(10 + (codeSeq++ % 80)), ...(sfuApi ? { sfuApi } : {}), ...(ringAlerts ? { ringAlerts } : {}) });
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
const rang = (s) => s.seen.some((e) => e.event === "incoming");

test("a group call rings every member whose settings say ring, and nobody else", async () => {
  const pb = fakePostbox({ members: ["/k/alice1", "/k/bob1", "/k/carol1", "/k/dave1"], decisions: { "/k/carol1": "silence", "/k/dave1": "block" } });
  const t = await start(pb);
  try {
    const alice = await t.stream("tok-alice");
    const bob = await t.stream("tok-bob");
    const bob2 = await t.stream("tok-bob");
    const carol = await t.stream("tok-carol");
    const dave = await t.stream("tok-dave");
    const erin = await t.stream("tok-erin");
    const placed = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", group_id: GROUP, kind: "video" });
    assert.equal(placed.status, 200, JSON.stringify(placed.body));
    const incoming = await bob.until("incoming");
    assert.equal(incoming.data.group_id, GROUP);
    assert.equal(incoming.data.group_name, "Team");
    assert.equal(incoming.data.to, "/k/bob1");
    await bob2.until("incoming");
    await settle();
    assert.equal(rang(carol), false, "silenced member is not rung");
    assert.equal(rang(dave), false, "member who blocked is not rung");
    assert.equal(rang(erin), false, "non-member is not rung");
    assert.deepEqual(pb.admissions[0], { group_id: GROUP, from: { mailbox: "/k/alice1" }, kind: "video", join: false });
    // The first report carries the group and the ring targets only.
    await t.service.internal.flush();
    const ring = pb.events.find((e) => e.event === "ring");
    assert.equal(ring.group_id, GROUP);
    assert.deepEqual(ring.to, ["/k/bob1"]);
    const filtered = pb.events.find((e) => e.event === "filtered");
    assert.deepEqual(filtered.to, ["/k/carol1"]);
    assert.ok(pb.events.every((e) => e.group_id === GROUP));
  } finally { await t.stop(); }
});

test("answering on one device quiets that person's other devices but keeps ringing the rest", async () => {
  const pb = fakePostbox();
  const t = await start(pb, { ringMs: 1500 });
  try {
    const alice = await t.stream("tok-alice");
    const bob = await t.stream("tok-bob");
    const bob2 = await t.stream("tok-bob");
    const carol = await t.stream("tok-carol");
    const placed = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", group_id: GROUP, kind: "audio" });
    await bob.until("incoming");
    await bob2.until("incoming");
    await carol.until("incoming");
    const accepted = await t.req("tok-bob", "POST", `/calls/v1/rooms/${placed.body.room}/accept`, { session: bob.id });
    assert.equal(accepted.status, 200, JSON.stringify(accepted.body));
    assert.equal(accepted.body.participants.length, 2);
    const quiet = await bob2.until("ring_ended");
    assert.equal(quiet.data.reason, "answered_elsewhere");
    await alice.until("participant_joined");
    // Carol is still ringing and can still answer.
    const carolIn = await t.req("tok-carol", "POST", `/calls/v1/rooms/${placed.body.room}/accept`, { session: carol.id });
    assert.equal(carolIn.status, 200);
    assert.equal(carolIn.body.participants.length, 3);
    // One person leaving does not end a group call; the last one leaving does.
    await t.req("tok-alice", "POST", `/calls/v1/rooms/${placed.body.room}/leave`, { session: alice.id });
    await t.req("tok-bob", "POST", `/calls/v1/rooms/${placed.body.room}/leave`, { session: bob.id });
    assert.equal(t.service.state.rooms.get(placed.body.room).state, "active");
    await t.req("tok-carol", "POST", `/calls/v1/rooms/${placed.body.room}/leave`, { session: carol.id });
    assert.equal(t.service.state.rooms.get(placed.body.room).state, "ended");
    await t.service.internal.flush();
    const end = pb.events.find((e) => e.event === "end");
    assert.ok(end, "end reported");
    assert.equal(end.group_id, GROUP);
  } finally { await t.stop(); }
});

test("non-members are refused; unconfigured or unreachable admission fails closed", async () => {
  {
    const pb = fakePostbox({ members: ["/k/bob1", "/k/carol1"] });
    const t = await start(pb);
    try {
      const alice = await t.stream("tok-alice");
      const r = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", group_id: GROUP, kind: "audio" });
      assert.equal(r.status, 403);
      assert.equal(r.body.error, "not_member");
      const st = await t.req("tok-alice", "GET", `/calls/v1/groups/${GROUP}/call?from=/alice`);
      assert.equal(st.status, 403);
      const bad = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", group_id: "nope/../x", kind: "audio" });
      assert.equal(bad.status, 400);
    } finally { await t.stop(); }
  }
  {
    const pb = fakePostbox();
    const t = await start(pb, { internal: false });
    try {
      const alice = await t.stream("tok-alice");
      const bob = await t.stream("tok-bob");
      const r = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", group_id: GROUP, kind: "audio" });
      assert.equal(r.status, 503);
      assert.equal(r.body.error, "group_calls_unavailable");
      await settle();
      assert.equal(rang(bob), false);
      assert.equal(t.service.state.rooms.size, 0);
    } finally { await t.stop(); }
  }
  {
    const pb = fakePostbox({ admissionStatus: 500 });
    const t = await start(pb);
    try {
      const alice = await t.stream("tok-alice");
      const r = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", group_id: GROUP, kind: "audio" });
      assert.equal(r.status, 503);
      assert.equal(t.service.state.rooms.size, 0);
    } finally { await t.stop(); }
  }
});

test("a member joins a running call without knocking; one live room per group; status for members only", async () => {
  const pb = fakePostbox({ members: ["/k/alice1", "/k/bob1", "/k/carol1"] });
  const t = await start(pb, { ringMs: 300 });
  try {
    const alice = await t.stream("tok-alice");
    const bob = await t.stream("tok-bob");
    const carol = await t.stream("tok-carol");
    const placed = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", group_id: GROUP, kind: "video" });
    await t.req("tok-bob", "POST", `/calls/v1/rooms/${placed.body.room}/accept`, { session: (await bob.until("incoming"), bob.id) });
    // The ring times out for Carol; the call goes on.
    await carol.until("ring_ended", 1500);
    assert.equal(t.service.state.rooms.get(placed.body.room).state, "active");

    const status = await t.req("tok-carol", "GET", `/calls/v1/groups/${GROUP}/call?from=/k/carol1`);
    assert.equal(status.status, 200);
    assert.deepEqual(status.body, { active: true, room: placed.body.room, kind: "video", participants: 2, full: false });

    const second = await t.req("tok-carol", "POST", "/calls/v1/calls", { session: carol.id, from: "/k/carol1", group_id: GROUP, kind: "audio" });
    assert.equal(second.status, 409);
    assert.equal(second.body.error, "call_in_progress");
    assert.equal(second.body.room, placed.body.room);

    const joined = await t.req("tok-carol", "POST", `/calls/v1/groups/${GROUP}/join`, { session: carol.id, from: "/k/carol1" });
    assert.equal(joined.status, 200, JSON.stringify(joined.body));
    assert.equal(joined.body.participants.length, 3);
    await alice.until("participant_joined");
    assert.ok(pb.admissions.some((a) => a.join === true && a.from.mailbox === "/k/carol1"));

    // A non-member learns nothing.
    const outsider = await t.req("tok-dave", "GET", `/calls/v1/groups/${GROUP}/call?from=/dave`);
    assert.equal(outsider.status, 403);
    const outsiderJoin = await t.req("tok-dave", "POST", `/calls/v1/groups/${GROUP}/join`, { session: (await t.stream("tok-dave")).id, from: "/dave" });
    assert.equal(outsiderJoin.status, 403);

    // Everyone leaves: no call in progress any more.
    for (const [tok, s] of [["tok-alice", alice], ["tok-bob", bob], ["tok-carol", carol]]) {
      await t.req(tok, "POST", `/calls/v1/rooms/${placed.body.room}/leave`, { session: s.id });
    }
    const after = await t.req("tok-alice", "GET", `/calls/v1/groups/${GROUP}/call?from=/alice`);
    assert.deepEqual(after.body, { active: false });
  } finally { await t.stop(); }
});

test("removed members cannot join or take their place back; the six-person cap holds", async () => {
  const all = ["/k/alice1", "/k/bob1", "/k/carol1", "/k/dave1", "/k/erin1", "/k/fred1", "/k/gina1", "/k/hank1"];
  const pb = fakePostbox({ members: all });
  const t = await start(pb, { ringMs: 3000 });
  try {
    const toks = ["tok-alice", "tok-bob", "tok-carol", "tok-dave", "tok-erin", "tok-fred", "tok-gina"];
    const streams = {};
    for (const tok of toks) streams[tok] = await t.stream(tok);
    const placed = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: streams["tok-alice"].id, from: "/alice", group_id: GROUP, kind: "audio" });
    for (const tok of toks.slice(1, 6)) {
      await streams[tok].until("incoming");
      const r = await t.req(tok, "POST", `/calls/v1/rooms/${placed.body.room}/accept`, { session: streams[tok].id });
      assert.equal(r.status, 200, tok + " " + JSON.stringify(r.body));
    }
    assert.equal(t.service.state.rooms.get(placed.body.room).participants.size, 6, "the mesh cap holds without a media server");
    await streams["tok-gina"].until("incoming");
    const seventh = await t.req("tok-gina", "POST", `/calls/v1/rooms/${placed.body.room}/accept`, { session: streams["tok-gina"].id });
    assert.equal(seventh.status, 409);
    assert.equal(seventh.body.error, "room_full");

    // Bob is removed from the group: his cached admission expires only after 30 s, so drop the cache
    // the way a real expiry would, then a resume (reload) is refused.
    pb.state.members.delete("/k/bob1");
    t.service.internal._cache.clear();
    const bobPid = [...t.service.state.rooms.get(placed.body.room).participants.values()].find((p) => p.mailbox && p.mailbox.address === "/k/bob1").id;
    const fresh = await t.stream("tok-bob");
    const resumed = await t.req("tok-bob", "POST", `/calls/v1/rooms/${placed.body.room}/resume`, { session: fresh.id, participant: bobPid });
    assert.equal(resumed.status, 403);
    assert.equal(resumed.body.error, "not_member");
  } finally { await t.stop(); }
});

// ---- P0-2 (audit 2026-11-03): membership is enforced during the call, SDS §4.7 ------------------

const SEVEN = ["/k/alice1", "/k/bob1", "/k/carol1", "/k/dave1", "/k/erin1", "/k/fred1", "/k/gina1"];
const SEVEN_TOKS = ["tok-alice", "tok-bob", "tok-carol", "tok-dave", "tok-erin", "tok-fred", "tok-gina"];
function sfuExtra(more = {}) {
  const sfu = sfuConfigFromEnv({ CALLS_SFU_URL: "wss://inbox.test/sfu", CALLS_SFU_API_URL: "https://media.test:7443", CALLS_SFU_KEY: "APIkey1", CALLS_SFU_SECRET: "s".repeat(40), CALLS_SFU_BUDGET_MBPS: "200" });
  return { meshMax: 6, sfu, ...more };
}

// Everyone in SEVEN_TOKS (or `toks`) joins one group call with frame E2EE; with seven it is on the media server.
async function groupCall(t, toks = SEVEN_TOKS) {
  const streams = {};
  for (const tok of toks) streams[tok] = await t.stream(tok);
  const placed = await t.req(toks[0], "POST", "/calls/v1/calls", { session: streams[toks[0]].id, from: ACCOUNTS[toks[0]][0].address, group_id: GROUP, kind: "video", e2ee: true });
  assert.equal(placed.status, 200, JSON.stringify(placed.body));
  for (const tok of toks.slice(1)) {
    await streams[tok].until("incoming");
    const r = await t.req(tok, "POST", `/calls/v1/rooms/${placed.body.room}/accept`, { session: streams[tok].id, e2ee: true });
    assert.equal(r.status, 200, tok + " " + JSON.stringify(r.body));
  }
  const room = t.service.state.rooms.get(placed.body.room);
  const pid = (address) => [...room.participants.values()].find((p) => p.mailbox && p.mailbox.address === address)?.id;
  return { streams, roomId: placed.body.room, room, pid };
}

test("a removed member gets no media-server ticket, is taken out of the call and off the media server", async () => {
  const pb = fakePostbox({ members: SEVEN });
  const sfuCalls = [];
  const t = await start(pb, { ringMs: 5000, extra: sfuExtra(), sfuApi: async (sfu, method, body) => { sfuCalls.push({ method, body }); return 200; } });
  try {
    const { streams, roomId, room, pid } = await groupCall(t);
    assert.equal(room.topology, "sfu");
    const bobPid = pid("/k/bob1");
    const ok = await t.req("tok-bob", "POST", `/calls/v1/rooms/${roomId}/sfu`, { session: streams["tok-bob"].id });
    assert.equal(ok.status, 200, "a member gets a ticket");

    // Bob is removed from the group. His "allowed" verdict is still in the 30 s cache: the ticket
    // path must not use it.
    pb.state.members.delete("/k/bob1");
    const asked = pb.admissions.length;
    const refused = await t.req("tok-bob", "POST", `/calls/v1/rooms/${roomId}/sfu`, { session: streams["tok-bob"].id });
    assert.equal(refused.status, 403);
    assert.equal(refused.body.error, "not_member");
    assert.ok(pb.admissions.length > asked, "the postbox was asked again, not the cache");
    assert.equal(room.participants.has(bobPid), false, "taken out of the call");
    const ended = await streams["tok-bob"].until("ended");
    assert.equal(ended.data.reason, "removed");
    const left = await streams["tok-alice"].until("participant_left");
    assert.deepEqual([left.data.participant, left.data.reason], [bobPid, "removed"]);
    await settle();
    assert.ok(sfuCalls.some((c) => c.method === "RemoveParticipant" && c.body.identity === bobPid && c.body.room === roomId), "kicked from the media room");

    // He cannot come back by any door: another ticket, a rejoin, or a resume.
    const again = await t.req("tok-bob", "POST", `/calls/v1/rooms/${roomId}/sfu`, { session: streams["tok-bob"].id });
    assert.equal(again.status, 403);
    const rejoin = await t.req("tok-bob", "POST", `/calls/v1/groups/${GROUP}/join`, { session: streams["tok-bob"].id, from: "/k/bob1", e2ee: true });
    assert.equal(rejoin.status, 403);
    assert.equal(rejoin.body.error, "not_member");
    const resumed = await t.req("tok-bob", "POST", `/calls/v1/rooms/${roomId}/resume`, { session: streams["tok-bob"].id, participant: bobPid });
    assert.equal(resumed.status, 403);
    // Members still get tickets.
    const alice = await t.req("tok-alice", "POST", `/calls/v1/rooms/${roomId}/sfu`, { session: streams["tok-alice"].id });
    assert.equal(alice.status, 200);
  } finally { await t.stop(); }
});

test("removal is noticed within seconds without any request from the removed member (mesh and media server)", async () => {
  for (const toks of [SEVEN_TOKS.slice(0, 3), SEVEN_TOKS]) {
    const pb = fakePostbox({ members: SEVEN });
    const sfuCalls = [];
    const t = await start(pb, { ringMs: 5000, extra: sfuExtra({ groupCheckMs: 150 }), sfuApi: async (sfu, method, body) => { sfuCalls.push({ method, body }); return 200; } });
    try {
      const { streams, roomId, room, pid } = await groupCall(t, toks);
      const carolPid = pid("/k/carol1");
      const topology = room.topology;
      assert.equal(topology, toks.length > 6 ? "sfu" : "mesh");
      if (topology === "sfu") {
        const ticket = await t.req("tok-carol", "POST", `/calls/v1/rooms/${roomId}/sfu`, { session: streams["tok-carol"].id });
        assert.equal(ticket.status, 200);
      }
      const removedAt = Date.now();
      pb.state.members.delete("/k/carol1");
      const ended = await streams["tok-carol"].until("ended", 3000);
      const took = Date.now() - removedAt;
      assert.equal(ended.data.reason, "removed");
      assert.ok(took < 2000, `kicked after ${took} ms`);
      const left = await streams["tok-alice"].until("participant_left", 2000);
      assert.deepEqual([left.data.participant, left.data.reason], [carolPid, "removed"]);
      assert.equal(room.participants.has(carolPid), false);
      assert.notEqual(room.state, "ended", "the call goes on for the others");
      if (topology === "sfu") {
        // Her ticket is still valid for a while: she is removed from the media room again on every
        // sweep until it expires, so it cannot bring her back.
        await settle(400);
        const kicks = sfuCalls.filter((c) => c.method === "RemoveParticipant" && c.body.identity === carolPid).length;
        assert.ok(kicks >= 2, `removed from the media room ${kicks} time(s)`);
      }
      // Others' signals can no longer reach her: no new room key can be handed to her.
      const sig = await t.req("tok-alice", "POST", `/calls/v1/rooms/${roomId}/signal`, { session: streams["tok-alice"].id, to: carolPid, type: "mute" });
      assert.equal(sig.status, 404);
    } finally { await t.stop(); }
  }
});

test("relay credentials for a group call re-check membership", async () => {
  const pb = fakePostbox({ members: SEVEN });
  const t = await start(pb, { ringMs: 5000 });
  try {
    const { streams, roomId, room, pid } = await groupCall(t, SEVEN_TOKS.slice(0, 3));
    const ok = await t.req("tok-bob", "GET", `/calls/v1/turn?session=${streams["tok-bob"].id}`);
    assert.equal(ok.status, 200);
    const bobPid = pid("/k/bob1");
    pb.state.members.delete("/k/bob1");
    const refused = await t.req("tok-bob", "GET", `/calls/v1/turn?session=${streams["tok-bob"].id}`);
    assert.equal(refused.status, 403);
    assert.equal(refused.body.error, "not_member");
    assert.equal(room.participants.has(bobPid), false);
    assert.equal((await streams["tok-bob"].until("ended")).data.reason, "removed");
    assert.equal(roomId, room.id);
  } finally { await t.stop(); }
});

test("a group call ends when membership cannot be verified for longer than the blind window", async () => {
  const pb = fakePostbox({ members: SEVEN });
  const t = await start(pb, { ringMs: 5000, extra: { groupCheckMs: 100, groupBlindMs: 350 } });
  try {
    const { streams, room } = await groupCall(t, SEVEN_TOKS.slice(0, 3));
    pb.state.admissionStatus = 503;
    const ended = await streams["tok-alice"].until("ended", 3000);
    assert.equal(ended.data.reason, "membership_unverifiable");
    assert.equal(room.state, "ended");
  } finally { await t.stop(); }
});

test("after a removal the room key rotates to the remaining members only; the removed member cannot decrypt", async () => {
  const pb = fakePostbox({ members: SEVEN });
  const t = await start(pb, { ringMs: 5000, extra: sfuExtra({ groupCheckMs: 150 }), sfuApi: async () => 200 });
  try {
    const { streams, roomId, room, pid } = await groupCall(t);
    const ids = {};
    for (const tok of SEVEN_TOKS) {
      const me = await E.generateIdentity();
      ids[tok] = { ...me, pid: pid(ACCOUNTS[tok][0].address) };
      const r = await t.req(tok, "POST", `/calls/v1/rooms/${roomId}/e2ee`, { session: streams[tok].id, public_key: me.publicKey });
      assert.equal(r.status, 200);
    }
    // The leader (Alice) hands generation `epoch` to everyone the service lists, as calls.js does,
    // through the service's signal route. Returns what each addressee could unwrap.
    async function rotate(epoch) {
      const roomKey = E.randomRoomKey();
      const roster = [...room.participants.values()];
      const from = { id: ids["tok-alice"].pid, key: ids["tok-alice"].publicKey };
      for (const p of roster) {
        if (p.id === from.id) continue;
        const wrapped = await E.wrapRoomKey({ privateKey: ids["tok-alice"].privateKey, roomId, epoch, from, to: { id: p.id, key: p.key }, roomKey });
        const r = await t.req("tok-alice", "POST", `/calls/v1/rooms/${roomId}/signal`, { session: streams["tok-alice"].id, to: p.id, type: "key", data: wrapped });
        assert.equal(r.status, 200);
      }
      return { roomKey, roster: roster.map((p) => p.id) };
    }
    const first = await rotate(1);
    assert.ok(first.roster.includes(ids["tok-bob"].pid));
    const bobKey1 = await E.dataKey(first.roomKey);

    pb.state.members.delete("/k/bob1");
    const left = await streams["tok-alice"].until("participant_left", 3000);
    assert.equal(left.data.reason, "removed");
    const second = await rotate(2);
    assert.equal(second.roster.includes(ids["tok-bob"].pid), false, "the new generation is not handed to him");

    // Bob's stream carried generation 1 to him, and nothing after his removal.
    const keysToBob = streams["tok-bob"].seen.filter((e) => e.event === "signal" && e.data.type === "key").map((e) => e.data.data.epoch);
    assert.deepEqual(keysToBob, [1]);
    // A wrapped generation-2 key addressed to someone else does not open with his key either.
    const toErin = streams["tok-erin"].seen.filter((e) => e.event === "signal" && e.data.type === "key" && e.data.data.epoch === 2);
    assert.equal(toErin.length, 1);
    const erinWrapped = toErin[0].data.data;
    const erinOpens = await E.unwrapRoomKey({ privateKey: ids["tok-erin"].privateKey, roomId, from: { id: ids["tok-alice"].pid, key: ids["tok-alice"].publicKey }, to: { id: ids["tok-erin"].pid, key: ids["tok-erin"].publicKey }, wrapped: erinWrapped });
    assert.deepEqual(erinOpens, second.roomKey);
    await assert.rejects(E.unwrapRoomKey({ privateKey: ids["tok-bob"].privateKey, roomId, from: { id: ids["tok-alice"].pid, key: ids["tok-alice"].publicKey }, to: { id: ids["tok-erin"].pid, key: ids["tok-erin"].publicKey }, wrapped: erinWrapped }));
    // Data sealed under generation 2 does not open with anything he holds.
    const sealed = await E.sealData(await E.dataKey(second.roomKey), { epoch: 2, roomId, senderId: ids["tok-alice"].pid }, { text: "after removal" });
    await assert.rejects(E.openData(async (e) => (e === 1 ? bobKey1 : null), { roomId, senderId: ids["tok-alice"].pid }, sealed), /no key/);
    const forged = sealed.slice(); new DataView(forged.buffer).setUint32(1, 1);
    await assert.rejects(E.openData(async (e) => (e === 1 ? bobKey1 : null), { roomId, senderId: ids["tok-alice"].pid }, forged));
    const opened = await E.openData(async (e) => (e === 2 ? await E.dataKey(erinOpens) : null), { roomId, senderId: ids["tok-alice"].pid }, sealed);
    assert.equal(opened.value.text, "after removal");
  } finally { await t.stop(); }
});


// ---- P1-11: group calls ring closed apps ------------------------------------------------------

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

async function ringState(t, room, token) {
  const res = await fetch(`${t.base}/calls/v1/rings/${room}`, { headers: { "x-ring-token": token } });
  return { status: res.status, body: await res.json() };
}

async function declineToken(t, room, token, reason) {
  const res = await fetch(`${t.base}/calls/v1/rings/${room}/decline`, {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ ring_token: token, ...(reason ? { reason } : {}) }),
  });
  return { status: res.status, body: await res.json() };
}

const tokenFor = (rec, address) => rec.rings.find((r) => r.to === address).token;

test("every rung member is pushed with their own ring token; silenced members are not", async () => {
  const pb = fakePostbox({ members: ["/k/alice1", "/k/bob1", "/k/carol1", "/k/dave1"], decisions: { "/k/dave1": "silence" } });
  const rec = recorder();
  const t = await start(pb, { ringMs: 3000, ringAlerts: rec.alerts });
  try {
    const alice = await t.stream("tok-alice");
    const placed = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", group_id: GROUP, kind: "video" });
    assert.equal(placed.status, 200, JSON.stringify(placed.body));
    assert.deepEqual(rec.rings.map((r) => r.to).sort(), ["/k/bob1", "/k/carol1"]);
    for (const r of rec.rings) {
      assert.equal(r.room, placed.body.room);
      assert.equal(r.group_id, GROUP);
      assert.equal(r.group_name, "Team");
      assert.equal(r.kind, "video");
      assert.equal(r.from.address, "/k/alice1");
      assert.ok(r.expiresAt > Date.now());
    }
    assert.notEqual(rec.rings[0].token, rec.rings[1].token, "one token per member");
    const st = await ringState(t, placed.body.room, tokenFor(rec, "/k/bob1"));
    assert.deepEqual([st.status, st.body.state, st.body.kind], [200, "ringing", "video"]);
  } finally { await t.stop(); }
});

test("answering cancels only that member's push; the ring's end cancels the rest", async () => {
  const pb = fakePostbox();
  const rec = recorder();
  const t = await start(pb, { ringMs: 400, ringAlerts: rec.alerts });
  try {
    const alice = await t.stream("tok-alice");
    const bob = await t.stream("tok-bob");
    const placed = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", group_id: GROUP, kind: "audio" });
    await bob.until("incoming");
    const ok = await t.req("tok-bob", "POST", `/calls/v1/rooms/${placed.body.room}/accept`, { session: bob.id });
    assert.equal(ok.status, 200);
    assert.deepEqual(rec.cancels.map((c) => [c.to, c.reason]), [["/k/bob1", "answered_elsewhere"]]);
    // Carol's phone still rings until the ring times out.
    assert.equal((await ringState(t, placed.body.room, tokenFor(rec, "/k/carol1"))).body.state, "ringing");
    assert.equal((await ringState(t, placed.body.room, tokenFor(rec, "/k/bob1"))).body.state, "ended");
    await settle(600);
    assert.deepEqual(rec.cancels.map((c) => c.to), ["/k/bob1", "/k/carol1"], "each push is cancelled once");
    assert.equal(rec.cancels[1].reason, "timeout");
    const after = await ringState(t, placed.body.room, tokenFor(rec, "/k/carol1"));
    assert.deepEqual(after.body, { state: "ended", reason: "timeout" });
  } finally { await t.stop(); }
});

test("a member's ring token declines for that member only; the call ends when nobody rings", async () => {
  const pb = fakePostbox();
  const rec = recorder();
  const t = await start(pb, { ringMs: 3000, ringAlerts: rec.alerts });
  try {
    const alice = await t.stream("tok-alice");
    const placed = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", group_id: GROUP, kind: "audio" });
    const room = placed.body.room;
    const d1 = await declineToken(t, room, tokenFor(rec, "/k/bob1"), "busy");
    assert.equal(d1.status, 200);
    assert.equal(d1.body.state, "ringing", "carol is still being rung");
    assert.deepEqual(rec.cancels.map((c) => [c.to, c.reason]), [["/k/bob1", "busy"]]);
    assert.deepEqual((await ringState(t, room, tokenFor(rec, "/k/bob1"))).body, { state: "ended", reason: "busy" });
    // A spent token cannot decline again or touch anyone else.
    assert.equal((await declineToken(t, room, tokenFor(rec, "/k/bob1"))).body.state, "ended");
    assert.equal(rec.cancels.length, 1);
    const d2 = await declineToken(t, room, tokenFor(rec, "/k/carol1"));
    assert.equal(d2.status, 200);
    assert.equal((await alice.until("ended")).data.reason, "declined");
    assert.deepEqual(rec.cancels.map((c) => c.to), ["/k/bob1", "/k/carol1"]);
    await t.service.internal.flush();
    assert.deepEqual(pb.events.filter((e) => e.event === "busy" || e.event === "decline").map((e) => [e.event, e.by]),
      [["busy", "/k/bob1"], ["decline", "/k/carol1"]]);
  } finally { await t.stop(); }
});

test("an app woken by the push rings late, and answers with its member's token only", async () => {
  const pb = fakePostbox();
  const rec = recorder();
  const t = await start(pb, { ringMs: 3000, ringAlerts: rec.alerts });
  try {
    const alice = await t.stream("tok-alice");
    const placed = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", group_id: GROUP, kind: "audio" });
    const room = placed.body.room;
    // Bob's phone opens its stream only now, and is rung like the others.
    const bob = await t.stream("tok-bob");
    const late = await bob.until("incoming");
    assert.deepEqual([late.data.room, late.data.group_id, late.data.to], [room, GROUP, "/k/bob1"]);
    // The caller's own new session is not rung.
    const alice2 = await t.stream("tok-alice");
    await settle();
    assert.equal(rang(alice2), false);
    // A session that is not itself rung (the CallKit answer path) needs Bob's own token: Carol's is
    // not his to use, and a made-up token answers nothing.
    const fresh = await t.stream("tok-bob");
    const r1 = t.service.state.rooms.get(room).ring;
    r1.ringing.delete(fresh.id); r1.ringingFor.delete(fresh.id);
    const wrong = await t.req("tok-bob", "POST", `/calls/v1/rooms/${room}/accept`, { session: fresh.id, ring_token: tokenFor(rec, "/k/carol1") });
    assert.equal(wrong.status, 403);
    const forged = await t.req("tok-bob", "POST", `/calls/v1/rooms/${room}/accept`, { session: fresh.id, ring_token: "x".repeat(43) });
    assert.equal(forged.status, 409);
    const ok = await t.req("tok-bob", "POST", `/calls/v1/rooms/${room}/accept`, { session: fresh.id, ring_token: tokenFor(rec, "/k/bob1") });
    assert.equal(ok.status, 200, JSON.stringify(ok.body));
    assert.equal((await bob.until("ring_ended")).data.reason, "answered_elsewhere");
    assert.deepEqual(rec.cancels.map((c) => [c.to, c.reason]), [["/k/bob1", "answered_elsewhere"]]);
  } finally { await t.stop(); }
});

test("nothing is pushed when ring pushes are off, and the 1:1 behaviour is unchanged", async () => {
  const pb = fakePostbox();
  const rec = recorder();
  rec.alerts.configured = false;
  const t = await start(pb, { ringMs: 3000, ringAlerts: rec.alerts });
  try {
    const alice = await t.stream("tok-alice");
    const bob = await t.stream("tok-bob");
    const placed = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", group_id: GROUP, kind: "audio" });
    await bob.until("incoming");
    await t.req("tok-bob", "POST", `/calls/v1/rooms/${placed.body.room}/decline`, { session: bob.id });
    await t.req("tok-alice", "POST", `/calls/v1/rooms/${placed.body.room}/leave`, { session: alice.id });
    await settle();
    assert.equal(rec.rings.length, 0);
    assert.equal(rec.cancels.length, 0);
  } finally { await t.stop(); }
});
