// Phase 3B: scheduled meeting rooms (docs/calendar.md §7–§8, SDS §14B.7).
import { test } from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import { createCallsService, sfuConfigFromEnv } from "../src/server.js";
import { createPostboxInternal } from "../src/postbox-internal.js";

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
const CODE = "ABCDEFGHJK";
const RANK = { host: 4, cohost: 3, attendee: 2, guest: 1, none: 0 };
const nowS = () => Math.floor(Date.now() / 1000);

function occ(id, extra = {}) {
  const t = nowS();
  return { occurrence_id: id, starts_at: t - 60, ends_at: t + 3600, opens_at: t - 600, closes_at: t + 5400, ...extra };
}

// A postbox that knows one meeting series. Tests change `m` to change its answers.
function fakePostbox() {
  const m = {
    open: [occ("occ_a")], closed: false, down: false, delayMs: 0,
    members: {
      "/k/alice1": { role: "host", admission: "direct", can_admit: true },
      "/k/bob1": { role: "cohost", admission: "direct", can_admit: true },
      "/k/carol1": { role: "attendee", admission: "direct", can_admit: false },
      "/k/dave1": { role: "attendee", admission: "knock", can_admit: false },
      "/k/fred1": { role: "attendee", admission: "direct", can_admit: false },
      "/k/gina1": { role: "attendee", admission: "direct", can_admit: false },
      "/k/hank1": { role: "attendee", admission: "direct", can_admit: false },
    },
    declined: new Set(),
    tickets: { "tkt-grace-000001": { attendee_id: "att_grace", name: "Grace", verified: true, admission: "direct" } },
    attendees: new Set(["att_grace"]),
    anonymous: "deny",
    policy: { waiting: "guests", admit: "hosts", allow_knock: false, participant_limit: 6, transport: "mesh" },
    admitters: ["/k/alice1", "/k/bob1"],
    extra: 0,
  };
  const accesses = [], events = [];
  function decide(body) {
    const base = { meeting_id: "mtg_series1", code: CODE, event_id: "evt_e1", organizer: { address: "/k/alice1", handle: "/alice" }, policy: m.policy, admitters: m.admitters, policy_version: "3.0", occurrences: [] };
    if (body.code !== CODE) return { allowed: false, reason: "unavailable" };
    const actor = body.actor;
    let who = null;
    if (actor.kind === "member") {
      for (const mb of actor.mailboxes) {
        const r = m.members[mb.address];
        if (r && (!who || RANK[r.role] > RANK[who.role])) who = { ...r, address: mb.address, handle: mb.handle };
      }
      if (!who) return { allowed: false, reason: "not_invited" };
      if (m.declined.has(who.address)) return { allowed: false, reason: "declined" };
    } else if (actor.kind === "guest") {
      if (actor.ticket) {
        const tk = m.tickets[actor.ticket];
        delete m.tickets[actor.ticket]; // single use
        if (!tk) return { allowed: false, reason: "ticket_invalid" };
        who = { role: "guest", admission: tk.admission, attendee_id: tk.attendee_id, name: tk.name, verified: tk.verified };
      } else {
        if (!m.attendees.has(actor.attendee_id)) return { allowed: false, reason: "not_invited" };
        who = { role: "guest", admission: "direct", attendee_id: actor.attendee_id };
      }
    } else {
      if (m.anonymous !== "knock" || m.closed || !m.open.length) return { allowed: false, reason: "unavailable" };
      who = { role: "none", admission: "knock" };
    }
    if (m.closed || !m.open.length) return { ...base, allowed: false, reason: "window_closed", occurrences: [occ("occ_next")] };
    let o;
    if (body.occurrence_id) {
      o = m.open.find((x) => x.occurrence_id === body.occurrence_id);
      if (!o) return { ...base, allowed: false, reason: "window_closed", occurrences: m.open };
    } else if (m.open.length > 1) {
      return { ...base, allowed: false, reason: "choose_occurrence", occurrences: m.open };
    } else o = m.open[0];
    if (body.purpose === "extend") {
      if (who.role !== "host") return { allowed: false, reason: "not_invited" };
      m.extra += body.extend_minutes * 60;
    }
    return {
      ...base, allowed: true, reason: null, ...o, closes_at: o.closes_at + m.extra,
      role: who.role, admission: who.admission, can_admit: Boolean(who.can_admit),
      participant: { address: who.address, handle: who.handle, attendee_id: who.attendee_id || null, name: who.name || who.handle || who.address, verified: who.verified !== false },
      unexpected: "dropped",
    };
  }
  const fetch = async (url, opts = {}) => {
    const u = new URL(url);
    if (u.pathname.startsWith("/internal/v1/")) {
      assert.equal(opts.headers.authorization, "Bearer " + TOKEN);
      const body = JSON.parse(opts.body);
      if (u.pathname === "/internal/v1/calendar/meeting-access") {
        accesses.push(body);
        if (m.delayMs) await new Promise((r) => setTimeout(r, m.delayMs));
        if (m.down) return { ok: false, status: 503, json: async () => ({}) };
        const v = decide(body);
        return { ok: true, status: 200, json: async () => v };
      }
      if (u.pathname === "/internal/v1/call-events") {
        events.push(body);
        return { ok: true, status: 200, json: async () => ({ accepted: true }) };
      }
      return { ok: false, status: 404, json: async () => ({}) };
    }
    const owned = ACCOUNTS[(opts.headers.authorization || "").replace(/^Bearer /, "")];
    if (!owned) return { ok: false, status: 401, json: async () => ({}) };
    if (u.pathname === "/v1/identities") return { ok: true, status: 200, json: async () => ({ identities: owned.map((x) => ({ address: x.address })) }) };
    if (u.pathname === "/v1/whoami") {
      const x = owned.find((y) => y.address === u.searchParams.get("identity"));
      return x ? { ok: true, status: 200, json: async () => x } : { ok: false, status: 403, json: async () => ({}) };
    }
    return { ok: false, status: 404, json: async () => ({}) };
  };
  return { fetch, m, accesses, events };
}

const SFU_SECRET = "s".repeat(40);
let codeSeq = 0;
async function start(pb, overrides = {}) {
  const cfg = {
    postbox: "https://postbox.test", turnSecret: "", turnUrls: [], turnTtlSeconds: 3600,
    allowedOrigins: ["https://inbox.test"], ringMs: 400, knockMs: 1500, trustProxy: true,
    postboxInternalUrl: "http://10.0.0.1:18991", postboxServiceToken: TOKEN, ...overrides,
  };
  const used = [];
  const service = createCallsService(cfg, {
    fetch: pb.fetch,
    randomCode: () => { const c = "SCHD" + String(10 + (codeSeq++ % 80)); used.push(c); return c; },
    sfuApi: async () => 200,
  });
  const server = http.createServer((req, res) => service.handle(req, res));
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const base = `http://127.0.0.1:${server.address().port}`;
  const open = [];
  let ipSeq = 0;
  const t = {
    service, base, used,
    async req(auth, method, path, body, headers = {}) {
      const authHeader = !auth ? {} : auth.startsWith("guest:") ? { authorization: "Guest " + auth.slice(6) } : { authorization: "Bearer " + auth };
      const res = await fetch(base + path, {
        method, headers: { ...authHeader, ...(body ? { "content-type": "application/json" } : {}), ...headers },
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
        auth, seen,
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
    join(s, body = {}, headers = {}) {
      return t.req(s.auth, "POST", `/calls/v1/meetings/scheduled/${body.code || CODE}/join`, { session: s.id, e2ee: true, ...body, code: undefined }, headers);
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

// ---- postbox-internal ------------------------------------------------------------------------------

test("meetingAccess posts with the service token, validates strictly and fails closed", async () => {
  const seen = [];
  let answer = { allowed: true, meeting_id: "mtg_x", event_id: "evt_x", occurrence_id: "occ_x", code: "abcdefghjk", closes_at: 100, role: "boss", admission: "maybe",
    participant: { address: "not an address", attendee_id: "att_1", name: "x".repeat(500), verified: "yes" }, organizer: { address: "/k/o" },
    policy: { participant_limit: 99, transport: "carrier-pigeon", waiting: "Guests!" }, admitters: ["/k/a", "bad", 3], secret_field: 1 };
  const fetch = async (url, opts) => {
    seen.push({ url, opts });
    return { ok: true, status: 200, json: async () => answer };
  };
  const pi = createPostboxInternal({ url: "http://10.0.0.1:1/", token: TOKEN, fetch });
  const v = await pi.meetingAccess({ code: CODE, actor: { kind: "anonymous" }, purpose: "status" });
  assert.equal(seen[0].url, "http://10.0.0.1:1/internal/v1/calendar/meeting-access");
  assert.equal(seen[0].opts.headers.authorization, "Bearer " + TOKEN);
  assert.equal(v.role, "none");
  assert.equal(v.admission, "deny");
  assert.equal(v.code, "ABCDEFGHJK");
  assert.equal(v.participant.address, null);
  assert.equal(v.participant.verified, false);
  assert.equal(v.participant.name.length, 200);
  assert.deepEqual(v.policy, { waiting: null, admit: null, allow_knock: false, participant_limit: 6, transport: "mesh" });
  assert.deepEqual(v.admitters, ["/k/a"]);
  assert.equal(v.secret_field, undefined);
  answer = { allowed: true, role: "host", admission: "direct" }; // no room identity
  assert.equal(await pi.meetingAccess({}), null);
  answer = { allowed: false, reason: "something_new" };
  assert.equal((await pi.meetingAccess({})).reason, "unavailable");
  const down = createPostboxInternal({ url: "http://10.0.0.1:1", token: TOKEN, fetch: async () => { throw new Error("down"); } });
  assert.equal(await down.meetingAccess({}), null);
  assert.equal(await createPostboxInternal({ url: "", token: "" }).meetingAccess({}), null);
});

// ---- joining ---------------------------------------------------------------------------------------

test("roles come from the postbox only: host, cohost, attendee, guest; anonymous knockers have none", async () => {
  const pb = fakePostbox();
  pb.m.anonymous = "knock";
  const t = await start(pb);
  try {
    const carol = await t.stream("tok-carol");
    const c = await t.join(carol);
    assert.equal(c.status, 200);
    assert.equal(c.body.state, "joined");
    assert.equal(c.body.scheduled.role, "attendee"); // first in the room, still only an attendee
    assert.equal(c.body.scheduled.can_admit, false);
    assert.equal(c.body.scheduled.event_id, "evt_e1");
    assert.equal(c.body.scheduled.occurrence_id, "occ_a");
    assert.ok(c.body.scheduled.closes_at > nowS());
    assert.equal(c.body.code, CODE);
    const alice = await t.stream("tok-alice");
    const a = await t.join(alice);
    assert.equal(a.body.scheduled.role, "host");
    assert.equal(a.body.scheduled.can_admit, true);
    assert.equal(a.body.room, c.body.room);
    const bob = await t.stream("tok-bob");
    assert.equal((await t.join(bob, { as: "/bob" })).body.scheduled.role, "cohost");
    assert.deepEqual(pb.accesses.at(-1).actor, { kind: "member", mailboxes: [{ address: "/k/bob1", handle: "/bob" }] });
    assert.equal(pb.accesses.at(-1).purpose, "join");
    const g = await t.guest();
    const grace = await t.stream(g);
    const gj = await t.join(grace, { ticket: "tkt-grace-000001" });
    assert.equal(gj.status, 200);
    assert.equal(gj.body.scheduled.role, "guest");
    assert.equal(gj.body.scheduled.can_admit, false);
    assert.equal(gj.body.participants.find((p) => p.participant === gj.body.you).verified, true);
    assert.equal(gj.body.participants.find((p) => p.participant === gj.body.you).name, "Grace");
    // A ticket is single use.
    const g2 = await t.stream(await t.guest());
    assert.equal((await t.join(g2, { ticket: "tkt-grace-000001" })).body.error, "ticket_invalid");
    const anon = await t.stream(await t.guest());
    const an = await t.join(anon, { name: "Visitor" });
    assert.equal(an.body.state, "waiting");
    const k = await alice.until("knock");
    assert.equal(k.data.verified, false);
    assert.equal((await t.req("tok-alice", "POST", `/calls/v1/rooms/${a.body.room}/knocks/${k.data.knock}`, { session: alice.id, admit: true })).status, 200);
    const joined = await anon.until("joined");
    assert.equal(joined.data.scheduled.role, "none");
  } finally { await t.stop(); }
});

test("direct members join; knock members wait until an admitter lets them in; no auto-admit", async () => {
  const pb = fakePostbox();
  const t = await start(pb, { knockMs: 400 });
  try {
    const dave = await t.stream("tok-dave");
    // Nobody is in yet: the knock waits and then times out rather than letting anyone in.
    const w = await t.join(dave);
    assert.equal(w.body.state, "waiting");
    const r = await dave.until("knock_result", 2000);
    assert.equal(r.data.admitted, false);
    assert.equal(r.data.reason, "timeout");
    const alice = await t.stream("tok-alice");
    const a = await t.join(alice);
    const w2 = await t.join(dave);
    assert.equal(w2.body.room, a.body.room);
    const k = await alice.until("knock");
    assert.equal(k.data.verified, true);
    await t.req("tok-alice", "POST", `/calls/v1/rooms/${a.body.room}/knocks/${k.data.knock}`, { session: alice.id, admit: true });
    assert.equal((await dave.until("knock_result")).data.admitted, true);
    const j = await dave.until("joined");
    assert.equal(j.data.scheduled.role, "attendee");
  } finally { await t.stop(); }
});

test("an admitter arriving sees who already waits", async () => {
  const pb = fakePostbox();
  const t = await start(pb);
  try {
    const carol = await t.stream("tok-carol");
    const c = await t.join(carol);
    const dave = await t.stream("tok-dave");
    await t.join(dave);
    const alice = await t.stream("tok-alice");
    const a = await t.join(alice);
    assert.equal(a.body.scheduled.knocks.length, 1);
    assert.equal(c.body.scheduled.knocks, undefined);
  } finally { await t.stop(); }
});

test("anonymous link holders may only knock when the postbox allows it, and learn nothing else", async () => {
  const pb = fakePostbox();
  const t = await start(pb);
  try {
    const st = await t.req(null, "GET", `/calls/v1/meetings/scheduled/${CODE}`);
    assert.deepEqual(st.body, { state: "unavailable", can_knock: false });
    const bogus = await t.req(null, "GET", "/calls/v1/meetings/scheduled/ZZZZZZZZZZ");
    assert.deepEqual(bogus.body, st.body); // no existence oracle
    const anon = await t.stream(await t.guest());
    const r = await t.join(anon, { name: "Visitor" });
    assert.equal(r.status, 404);
    assert.equal(r.body.error, "unavailable");
    pb.m.anonymous = "knock";
    const open = await t.req(null, "GET", `/calls/v1/meetings/scheduled/${CODE.toLowerCase().slice(0, 5)}-${CODE.slice(5).toLowerCase()}`);
    assert.deepEqual(open.body, { state: "open", can_knock: true });
    assert.equal((await t.join(anon, {})).body.error, "name_required");
    const r2 = await t.join(anon, { name: "Visitor" });
    assert.equal(r2.body.state, "waiting");
    assert.deepEqual(pb.accesses.at(-1).actor, { kind: "anonymous" });
  } finally { await t.stop(); }
});

test("code guessing is bounded per IP", async () => {
  const pb = fakePostbox();
  const t = await start(pb);
  try {
    const h = { "cf-connecting-ip": "203.0.113.77" };
    for (let i = 0; i < 10; i++) assert.equal((await t.req(null, "GET", "/calls/v1/meetings/scheduled/ZZZZZZZZZ" + "23456789"[i % 8], undefined, h)).status, 200);
    const n = pb.accesses.length;
    assert.equal((await t.req(null, "GET", `/calls/v1/meetings/scheduled/${CODE}`, undefined, h)).status, 429);
    assert.equal(pb.accesses.length, n); // the postbox is not asked once the budget is spent
    assert.equal((await t.req(null, "GET", `/calls/v1/meetings/scheduled/${CODE}`, undefined, { "cf-connecting-ip": "203.0.113.78" })).status, 200);
  } finally { await t.stop(); }
});

test("members see the summary; strangers, decliners and closed windows are refused", async () => {
  const pb = fakePostbox();
  const t = await start(pb);
  try {
    const st = await t.req("tok-alice", "GET", `/calls/v1/meetings/scheduled/${CODE}`);
    assert.equal(st.body.state, "open");
    assert.equal(st.body.role, "host");
    assert.equal(st.body.occurrences[0].occurrence_id, "occ_a");
    assert.equal(st.body.room, null);
    const alice = await t.stream("tok-alice");
    await t.join(alice);
    assert.deepEqual((await t.req("tok-carol", "GET", `/calls/v1/meetings/scheduled/${CODE}`)).body.room, { participants: 1 });
    assert.equal((await t.req("tok-erin", "GET", `/calls/v1/meetings/scheduled/${CODE}`)).body.state, "unavailable");
    const erin = await t.stream("tok-erin");
    const e = await t.join(erin);
    assert.equal(e.status, 403);
    assert.equal(e.body.error, "not_invited");
    pb.m.declined.add("/k/carol1");
    const carol = await t.stream("tok-carol");
    assert.equal((await t.join(carol)).body.error, "declined");
    pb.m.closed = true;
    const bob = await t.stream("tok-bob");
    const b = await t.join(bob);
    assert.equal(b.status, 403);
    assert.equal(b.body.error, "window_closed");
    assert.equal(b.body.occurrences[0].occurrence_id, "occ_next");
    assert.equal((await t.req("tok-bob", "GET", `/calls/v1/meetings/scheduled/${CODE}`)).body.state, "not_open");
  } finally { await t.stop(); }
});

test("overlapping windows: choose_occurrence; one code, different occurrences → different rooms", async () => {
  const pb = fakePostbox();
  const t = await start(pb);
  try {
    const alice = await t.stream("tok-alice");
    const a = await t.join(alice);
    pb.m.open = [occ("occ_a"), occ("occ_b")];
    const bob = await t.stream("tok-bob");
    const choose = await t.join(bob);
    assert.equal(choose.status, 409);
    assert.equal(choose.body.error, "choose_occurrence");
    assert.deepEqual(choose.body.occurrences.map((o) => o.occurrence_id), ["occ_a", "occ_b"]);
    assert.equal((await t.join(bob, { occurrence_id: "bad id" })).status, 400);
    const b = await t.join(bob, { occurrence_id: "occ_b" });
    assert.equal(b.status, 200);
    assert.notEqual(b.body.room, a.body.room);
    assert.equal(b.body.scheduled.occurrence_id, "occ_b");
    const carol = await t.stream("tok-carol");
    const c = await t.join(carol, { occurrence_id: "occ_a" });
    assert.equal(c.body.room, a.body.room);
  } finally { await t.stop(); }
});

test("concurrent first joins create exactly one room", async () => {
  const pb = fakePostbox();
  pb.m.delayMs = 40;
  const t = await start(pb);
  try {
    const s = await Promise.all(["tok-alice", "tok-bob", "tok-carol", "tok-fred"].map((x) => t.stream(x)));
    const r = await Promise.all(s.map((x) => t.join(x)));
    for (const x of r) assert.equal(x.status, 200);
    assert.equal(new Set(r.map((x) => x.body.room)).size, 1);
    assert.equal(t.service.scheduled.live.size, 1);
  } finally { await t.stop(); }
});

// ---- the live authorization gate ---------------------------------------------------------------------

test("only admitters answer knocks; only hosts and co-hosts mute; only the host ends or extends", async () => {
  const pb = fakePostbox();
  const t = await start(pb);
  try {
    const alice = await t.stream("tok-alice"), bob = await t.stream("tok-bob"), carol = await t.stream("tok-carol"), dave = await t.stream("tok-dave");
    const a = await t.join(alice);
    const room = a.body.room;
    const b = await t.join(bob);
    const c = await t.join(carol);
    await t.join(dave);
    const k = await carol.until("knock");
    const no = await t.req("tok-carol", "POST", `/calls/v1/rooms/${room}/knocks/${k.data.knock}`, { session: carol.id, admit: true });
    assert.equal(no.status, 403);
    assert.equal(no.body.error, "not_allowed");
    assert.equal((await t.req("tok-alice", "POST", `/calls/v1/rooms/${room}/knocks/${k.data.knock}`, { session: alice.id, admit: true })).status, 200);
    await dave.until("joined");

    const mute = (tok, s, to) => t.req(tok, "POST", `/calls/v1/rooms/${room}/signal`, { session: s.id, to, type: "mute" });
    assert.equal((await mute("tok-carol", carol, a.body.you)).body.error, "not_allowed");
    assert.equal((await mute("tok-bob", bob, c.body.you)).status, 200);
    assert.equal((await mute("tok-alice", alice, b.body.you)).status, 200);

    const ext = (tok, s, minutes) => t.req(tok, "POST", `/calls/v1/rooms/${room}/extend`, { session: s.id, minutes });
    assert.equal((await ext("tok-bob", bob, 15)).body.error, "not_allowed");
    assert.equal((await ext("tok-alice", alice, 20)).body.error, "bad_minutes");
    const before = a.body.scheduled.closes_at;
    const ok = await ext("tok-alice", alice, 15);
    assert.equal(ok.status, 200);
    assert.equal(ok.body.closes_at, before + 900);
    assert.equal(pb.accesses.at(-1).purpose, "extend");
    assert.equal(pb.accesses.at(-1).extend_minutes, 15);
    assert.equal((await bob.until("scheduled_window")).data.closes_at, before + 900);

    assert.equal((await t.req("tok-carol", "POST", `/calls/v1/rooms/${room}/end`, { session: carol.id })).body.error, "not_allowed");
    assert.equal((await t.req("tok-bob", "POST", `/calls/v1/rooms/${room}/end`, { session: bob.id })).body.error, "not_allowed");
    assert.equal((await t.req("tok-alice", "POST", `/calls/v1/rooms/${room}/end`, { session: alice.id })).status, 200);
    assert.equal((await carol.until("ended")).data.reason, "ended_by_host");
  } finally { await t.stop(); }
});

test("end and extend do not exist for ad-hoc rooms", async () => {
  const pb = fakePostbox();
  const t = await start(pb);
  try {
    const alice = await t.stream("tok-alice");
    const placed = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/bob", kind: "audio" });
    assert.equal((await t.req("tok-alice", "POST", `/calls/v1/rooms/${placed.body.room}/end`, { session: alice.id })).status, 404);
  } finally { await t.stop(); }
});

// ---- capacity ----------------------------------------------------------------------------------------

test("a mesh meeting never grows past six or onto the media server; the participant limit holds", async () => {
  const pb = fakePostbox();
  pb.m.policy = { ...pb.m.policy, participant_limit: 30, transport: "mesh" };
  const sfu = sfuConfigFromEnv({ CALLS_SFU_URL: "wss://inbox.test/sfu", CALLS_SFU_API_URL: "https://media.test:7443", CALLS_SFU_KEY: "APIkey1", CALLS_SFU_SECRET: SFU_SECRET, CALLS_SFU_BUDGET_MBPS: "200" });
  const t = await start(pb, { meshMax: 6, sfu });
  try {
    const toks = ["tok-alice", "tok-bob", "tok-carol", "tok-fred", "tok-gina", "tok-hank"];
    let room;
    for (const tok of toks) {
      const r = await t.join(await t.stream(tok));
      assert.equal(r.status, 200);
      room = r.body.room;
    }
    pb.m.members["/k/erin1"] = { role: "attendee", admission: "direct" };
    const seventh = await t.join(await t.stream("tok-erin"));
    assert.equal(seventh.status, 409);
    assert.equal(seventh.body.error, "room_full");
    assert.equal(t.service.state.rooms.get(room).topology, "mesh");
  } finally { await t.stop(); }

  const pb2 = fakePostbox();
  pb2.m.policy = { ...pb2.m.policy, participant_limit: 2 };
  const t2 = await start(pb2);
  try {
    assert.equal((await t2.join(await t2.stream("tok-alice"))).status, 200);
    assert.equal((await t2.join(await t2.stream("tok-bob"))).status, 200);
    assert.equal((await t2.join(await t2.stream("tok-carol"))).body.error, "room_full");
  } finally { await t2.stop(); }
});

// ---- revalidation, failing closed, the window -------------------------------------------------------

test("recheck removes a revoked participant; an unreachable postbox keeps people in", async () => {
  const pb = fakePostbox();
  const t = await start(pb, { scheduledRecheckMs: 150 });
  try {
    const alice = await t.stream("tok-alice"), bob = await t.stream("tok-bob");
    const a = await t.join(alice);
    const b = await t.join(bob);
    const g = await t.stream(await t.guest());
    const gj = await t.join(g, { ticket: "tkt-grace-000001" });
    pb.m.down = true;
    await settle(400);
    assert.equal(t.service.state.rooms.get(a.body.room).participants.size, 3);
    pb.m.down = false;
    delete pb.m.members["/k/bob1"];
    pb.m.attendees.delete("att_grace");
    const left = await alice.until("participant_left", 2000);
    assert.equal(left.data.reason, "revoked");
    assert.equal((await bob.until("ended")).data.reason, "revoked");
    assert.equal((await g.until("ended")).data.reason, "revoked");
    await settle(50);
    const ids = [...t.service.state.rooms.get(a.body.room).participants.keys()];
    assert.deepEqual(ids, [a.body.you]);
    assert.ok(!ids.includes(b.body.you) && !ids.includes(gj.body.you));
    const rechecks = pb.accesses.filter((x) => x.purpose === "recheck");
    assert.ok(rechecks.some((x) => x.actor.kind === "guest" && x.actor.attendee_id === "att_grace" && !x.actor.ticket));
    assert.ok(rechecks.every((x) => x.occurrence_id === "occ_a"));
  } finally { await t.stop(); }
});

test("joining fails closed when the postbox cannot be asked", async () => {
  const pb = fakePostbox();
  pb.m.down = true;
  const t = await start(pb);
  try {
    const alice = await t.stream("tok-alice");
    const r = await t.join(alice);
    assert.equal(r.status, 503);
    assert.equal(r.body.error, "calendar_unavailable");
    assert.equal(t.service.scheduled.live.size, 0);
    assert.equal((await t.req(null, "GET", `/calls/v1/meetings/scheduled/${CODE}`)).status, 503);
  } finally { await t.stop(); }
  const unconfigured = await start(fakePostbox(), { postboxInternalUrl: "", postboxServiceToken: "" });
  try {
    const alice = await unconfigured.stream("tok-alice");
    assert.equal((await unconfigured.join(alice)).status, 503);
  } finally { await unconfigured.stop(); }
});

test("the room closes when the window does", async () => {
  const pb = fakePostbox();
  pb.m.open = [occ("occ_a", { closes_at: nowS() + 1 })];
  const t = await start(pb);
  try {
    const alice = await t.stream("tok-alice");
    await t.join(alice);
    assert.equal((await alice.until("ended", 3000)).data.reason, "window_closed");
  } finally { await t.stop(); }
});

// ---- call history and isolation ---------------------------------------------------------------------

test("call events come from the organizer and carry calendar_event_id and occurrence_id", async () => {
  const pb = fakePostbox();
  const t = await start(pb);
  try {
    const carol = await t.stream("tok-carol"), bob = await t.stream("tok-bob");
    const c = await t.join(carol);
    await t.join(bob);
    const g = await t.stream(await t.guest());
    await t.join(g, { ticket: "tkt-grace-000001" });
    await t.req("tok-carol", "POST", `/calls/v1/rooms/${c.body.room}/leave`, { session: carol.id });
    await t.req("tok-bob", "POST", `/calls/v1/rooms/${c.body.room}/leave`, { session: bob.id });
    await t.req(g.auth, "POST", `/calls/v1/rooms/${c.body.room}/leave`, { session: g.id });
    await settle(200);
    const evs = pb.events.filter((e) => e.room_id === c.body.room);
    assert.deepEqual(evs.map((e) => e.event), ["join", "join", "end"]);
    assert.deepEqual(evs.slice(0, 2).map((e) => e.by), ["/k/carol1", "/k/bob1"]);
    for (const e of evs) {
      assert.equal(e.calendar_event_id, "evt_e1");
      assert.equal(e.occurrence_id, "occ_a");
      assert.equal(e.from.address, "/k/alice1");
    }
    // The next room for the same occurrence is a new room with its own id.
    const bob2 = await t.join(bob);
    assert.notEqual(bob2.body.room, c.body.room);
  } finally { await t.stop(); }
});

test("a scheduled room is never reachable through the ad-hoc 6-character routes", async () => {
  const pb = fakePostbox();
  const t = await start(pb);
  try {
    const alice = await t.stream("tok-alice");
    const a = await t.join(alice);
    assert.equal(a.body.code, CODE);
    assert.equal(t.used.length, 1);
    const sixChar = t.used[0];
    assert.ok(!t.service.state.roomsByCode.has(sixChar));
    assert.equal((await t.req(null, "GET", `/calls/v1/meetings/${sixChar}`)).status, 404);
    const g = await t.stream(await t.guest());
    assert.equal((await t.req(g.auth, "POST", "/calls/v1/knocks", { session: g.id, code: sixChar, name: "X" })).status, 404);
    assert.equal((await t.req(g.auth, "POST", "/calls/v1/knocks", { session: g.id, code: CODE, name: "X" })).status, 404);
    assert.notEqual((await t.req(null, "GET", `/calls/v1/meetings/${CODE}`)).status, 200);
  } finally { await t.stop(); }
});
