// Phase 3a: calls from persistent groups (docs/groups.md §6).
import { test } from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import { createCallsService, LIMITS } from "../src/server.js";

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
async function start(pb, { internal = true, ringMs = 400 } = {}) {
  const cfg = {
    postbox: "https://postbox.test", turnSecret: "", turnUrls: [], turnTtlSeconds: 3600,
    allowedOrigins: ["https://inbox.test"], ringMs, knockMs: 1500, trustProxy: true,
    postboxInternalUrl: internal ? "http://10.0.0.1:18991" : "", postboxServiceToken: internal ? TOKEN : "",
  };
  const service = createCallsService(cfg, { fetch: pb.fetch, randomCode: () => "GRPC" + String(10 + (codeSeq++ % 80)) });
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
