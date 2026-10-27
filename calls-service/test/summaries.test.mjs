// Phase 7: meeting notes — the consent matrix, capture credentials, encrypted uploads relayed
// without a key, and the hand-off to the postbox and meeting-intel.
import { test } from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import crypto from "node:crypto";
import { createCallsService } from "../src/server.js";
import { disclosure, disclosureHash } from "../src/summaries.js";
import { createIntel } from "../../meeting-intel/src/service.js";
import { createHandler } from "../../meeting-intel/src/server.js";
import { b64, generateSigningKeyPem, loadSigningKey, openBrief, sealChunk, unb64, verifySessionKey } from "../../meeting-intel/src/crypto.js";

const ACCOUNTS = {
  "tok-alice": [{ address: "/k/alice1", handle: "/alice" }],
  "tok-bob": [{ address: "/k/bob1", handle: "/bob" }],
  "tok-carol": [{ address: "/k/carol1", handle: "/carol" }],
};
const SVC = "svc_calls_0123456789abcdef0123456789abcdef";
const INTEL_TOKEN = "intel-token-0123456789abcdef0123456789";
const signing = loadSigningKey(generateSigningKeyPem());

// The postbox's internal meeting routes, in memory: who may host, who answered what.
function fakePostboxInternal({ allowed = new Set(["/k/alice1"]) } = {}) {
  const meetings = new Map();
  const log = [];
  return {
    meetings, log,
    handle(path, body) {
      log.push({ path, body });
      if (path === "/internal/v1/call-admission") return { decision: "ring", targets: [body.to] };
      if (path === "/internal/v1/call-events") return { accepted: true };
      if (path === "/internal/v1/meetings") {
        if (!allowed.has(body.host)) return { allowed: false, reason: "feature_disabled" };
        if (body.check_only) return { allowed: true, retention_days: 90 };
        const id = "mtg_" + crypto.randomBytes(13).toString("hex");
        meetings.set(id, { host: body.host, kind: body.kind, room_ref: body.room_ref, attendees: new Map(), ended: null, brief: null });
        return { allowed: true, meeting_id: id, retention_days: 90 };
      }
      const m = /^\/internal\/v1\/meetings\/(mtg_[a-z0-9]+)\/(attendees|end|recipients|brief)$/.exec(path);
      const meeting = m && meetings.get(m[1]);
      if (!meeting) return { __status: 404 };
      if (m[2] === "attendees") { meeting.attendees.set(body.address, { consent: body.consent, key: body.notes_key || meeting.attendees.get(body.address)?.key || null }); return { recorded: true }; }
      if (m[2] === "end") { meeting.ended = body.reason; return { ended: true }; }
      if (m[2] === "recipients") {
        return { recipients: [...meeting.attendees].filter(([, a]) => a.key && (a.consent === "yes" || a.consent === "withdrawn")).map(([address, a]) => ({ account_id: "acct_" + address.slice(3), address, keys: [a.key] })) };
      }
      meeting.brief = body;
      return { stored: true };
    },
  };
}

async function start({ allowed } = {}) {
  const pb = fakePostboxInternal({ allowed });
  const transcribed = [];
  const intel = createIntel({ sttName: "test-stt", llmName: "test-llm" }, {
    signing,
    decode: async (plain) => { transcribed.push(plain.toString()); return Buffer.from(plain.toString().padEnd(64000, "x")); },
    transcribe: async (pcm) => ({ language: "english", segments: [{ start: 0, end: 2, text: pcm.toString().replace(/x+$/, "") }] }),
    summarizer: { summarize: async () => ({ notes: { title: "T", summary: "S", decisions: [], action_items: [], open_questions: [] }, draft: false }) },
    postbox: async (method, path, body) => ({ status: 200, body: pb.handle(path, body) }),
    log: () => {},
  });
  const intelServer = http.createServer(createHandler({ serviceToken: INTEL_TOKEN }, intel));
  await new Promise((r) => intelServer.listen(0, "127.0.0.1", r));
  const intelBase = `http://127.0.0.1:${intelServer.address().port}`;
  const forwarded = []; // everything the calls service sent to meeting-intel
  const fetchImpl = async (url, opts = {}) => {
    const u = new URL(url);
    if (u.host === "postbox-internal.test") {
      assert.equal(opts.headers.authorization, "Bearer " + SVC);
      const out = pb.handle(u.pathname, opts.body ? JSON.parse(opts.body) : undefined);
      const status = out.__status || 200;
      return { ok: status === 200, status, json: async () => out };
    }
    if (u.host === "intel.test") {
      forwarded.push({ path: u.pathname, body: opts.body || "" });
      return fetch(intelBase + u.pathname, opts);
    }
    const owned = ACCOUNTS[(opts.headers.authorization || "").replace(/^Bearer /, "")];
    if (!owned) return { ok: false, status: 401, json: async () => ({}) };
    if (u.pathname === "/v1/identities") return { ok: true, status: 200, json: async () => ({ identities: owned.map((m) => ({ address: m.address })) }) };
    if (u.pathname === "/v1/whoami") return { ok: true, status: 200, json: async () => owned.find((x) => x.address === u.searchParams.get("identity")) };
    return { ok: false, status: 404, json: async () => ({}) };
  };
  let n = 0;
  const service = createCallsService({
    postbox: "https://postbox.test", turnSecret: "", turnUrls: [], turnTtlSeconds: 60, allowedOrigins: ["https://inbox.test"],
    ringMs: 2000, knockMs: 3000, trustProxy: true,
    postboxInternalUrl: "http://postbox-internal.test", postboxServiceToken: SVC,
    intelUrl: "http://intel.test", intelToken: INTEL_TOKEN,
  }, { fetch: fetchImpl, randomCode: () => "SUMM" + String(10 + n++) });
  const server = http.createServer((req, res) => service.handle(req, res));
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const base = `http://127.0.0.1:${server.address().port}`;
  const streams = [];
  const t = {
    service, pb, intel, forwarded, transcribed,
    async req(auth, method, path, body, headers = {}) {
      const a = !auth ? {} : auth.startsWith("guest:") ? { authorization: "Guest " + auth.slice(6) } : { authorization: "Bearer " + auth };
      const res = await fetch(base + path, { method, headers: { ...a, ...(body ? { "content-type": "application/json" } : {}), ...headers }, body: body ? JSON.stringify(body) : undefined });
      return { status: res.status, body: await res.json().catch(() => null) };
    },
    async guest() {
      const r = await t.req(null, "POST", "/calls/v1/guests", {}, { "cf-connecting-ip": "203.0.113." + (10 + n++) });
      return "guest:" + r.body.guest_token;
    },
    async stream(auth) {
      const ac = new AbortController();
      const res = await fetch(base + "/calls/v1/events", { headers: { authorization: auth.startsWith("guest:") ? "Guest " + auth.slice(6) : "Bearer " + auth }, signal: ac.signal });
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
        next: (ms = 2000) => queue.length ? Promise.resolve(queue.shift()) : new Promise((resolve, reject) => {
          const timer = setTimeout(() => reject(new Error("no event")), ms);
          waiters.push((x) => { clearTimeout(timer); resolve(x); });
        }),
        async until(name, pred = () => true, ms = 2000) { for (;;) { const e = await s.next(ms); if (e.event === name && pred(e.data)) return e; } },
        // The newest `summary` state available now (draining the queue), or wait for one.
        async summary(pred = () => true) { return (await s.until("summary", pred)).data; },
        close: () => ac.abort(),
      };
      s.id = (await s.next()).data.session;
      streams.push(s);
      return s;
    },
    async stop() {
      for (const s of streams) s.close();
      await new Promise((r) => server.close(r));
      server.closeAllConnections?.();
      await new Promise((r) => intelServer.close(r));
      intelServer.closeAllConnections?.();
    },
  };
  return t;
}

const settle = (ms = 60) => new Promise((r) => setTimeout(r, ms));

async function call(t) {
  const alice = await t.stream("tok-alice");
  const bob = await t.stream("tok-bob");
  const placed = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/bob", kind: "audio" });
  await bob.until("incoming");
  await t.req("tok-bob", "POST", `/calls/v1/rooms/${placed.body.room}/accept`, { session: bob.id });
  const joined = await bob.until("joined");
  return { alice, bob, room: placed.body.room, code: placed.body.code, alicePid: placed.body.you, bobPid: joined.data.you };
}

function notesKey(id) {
  const e = crypto.createECDH("prime256v1");
  e.generateKeys();
  return { ecdh: e, wire: { key_id: id, public_key: b64(e.getPublicKey(null, "uncompressed")), label: "Test" } };
}

// What a browser does with the service key it was given: verify it against the pinned key, then seal.
function seal(state, pid, seq, text) {
  const k = state.service_key;
  assert.ok(verifySessionKey(signing.publicRaw, k.session, state.meeting, k.key, k.expires_at, k.signature), "service key is signed");
  return sealChunk(unb64(k.key, 65), k.session, pid, seq, Buffer.from(text), { codec: "audio/webm;codecs=opus", t: Date.now(), dur: 10_000 });
}

async function upload(t, auth, room, session, seq, envelope, token) {
  return t.req(auth, "POST", `/calls/v1/rooms/${room}/summary/audio?session=${session}&seq=${seq}`, envelope, { "x-capture-token": token || "" });
}

test("only the host can turn notes on; capture waits until everyone present has agreed", async () => {
  const t = await start();
  try {
    const c = await call(t);
    // The host learns they may ask; nobody else does.
    const offer = await c.alice.summary((d) => d.you.can_request);
    assert.equal(offer.state, "off");
    assert.equal((await t.req("tok-bob", "POST", `/calls/v1/rooms/${c.room}/summary`, { session: c.bob.id, enabled: true })).body.error, "host_only");
    const on = await t.req("tok-alice", "POST", `/calls/v1/rooms/${c.room}/summary`, { session: c.alice.id, enabled: true });
    assert.equal(on.status, 200, JSON.stringify(on.body));
    assert.equal(on.body.state, "waiting");
    assert.equal(on.body.disclosure_hash, disclosureHash(disclosure(90)));
    assert.equal((await t.req("tok-alice", "POST", `/calls/v1/rooms/${c.room}/summary`, { session: c.alice.id, enabled: true })).body.error, "summary_active");
    const bobView = await c.bob.summary((d) => d.state === "waiting");
    assert.equal(bobView.you.consent, null);
    assert.equal(bobView.capture, null);
    assert.ok(bobView.disclosure.audience.includes("Guests"));

    // Alice agrees: still waiting for Bob, and no credential yet, so nothing can be uploaded.
    const ak = notesKey("alicebrowser1");
    let r = await t.req("tok-alice", "POST", `/calls/v1/rooms/${c.room}/summary/consent`, { session: c.alice.id, consent: true, version: on.body.version, disclosure_hash: on.body.disclosure_hash, notes_key: ak.wire });
    assert.equal(r.body.state, "waiting");
    assert.equal(r.body.capture, null);
    assert.equal((await upload(t, "tok-alice", c.room, c.alice.id, 0, seal(on.body, c.alicePid, 0, "early"), "x")).body.error, "capture_paused");

    // Bob declines: still waiting, and his audio is refused even with a guessed credential.
    r = await t.req("tok-bob", "POST", `/calls/v1/rooms/${c.room}/summary/consent`, { session: c.bob.id, consent: false });
    assert.equal(r.body.state, "waiting");
    assert.equal(r.body.you.consent, "no");
    const meeting = t.pb.meetings.get(on.body.meeting);
    assert.equal(meeting.attendees.get("/k/bob1").consent, "no");
    assert.equal(meeting.attendees.get("/k/alice1").key.key_id, "alicebrowser1");

    // A stale or altered consent cannot start capture.
    assert.equal((await t.req("tok-bob", "POST", `/calls/v1/rooms/${c.room}/summary/consent`, { session: c.bob.id, consent: true, version: 999, disclosure_hash: on.body.disclosure_hash })).body.error, "consent_stale");
    assert.equal((await t.req("tok-bob", "POST", `/calls/v1/rooms/${c.room}/summary/consent`, { session: c.bob.id, consent: true, version: r.body.version, disclosure_hash: "nope" })).body.error, "consent_stale");

    // Bob changes his mind: everyone has agreed, capture starts, each gets their own credential.
    const bk = notesKey("bobbrowser01");
    r = await t.req("tok-bob", "POST", `/calls/v1/rooms/${c.room}/summary/consent`, { session: c.bob.id, consent: true, version: r.body.version, disclosure_hash: r.body.disclosure_hash, notes_key: bk.wire });
    assert.equal(r.body.state, "active");
    const bobState = r.body;
    const aliceState = await c.alice.summary((d) => d.state === "active");
    assert.ok(aliceState.capture.token && bobState.capture.token && aliceState.capture.token !== bobState.capture.token);

    // Uploads: own credential only; the calls service relays ciphertext it cannot open.
    const env = seal(aliceState, c.alicePid, 0, "SECRET-ALICE-AUDIO");
    assert.equal((await upload(t, "tok-bob", c.room, c.bob.id, 0, seal(aliceState, c.bobPid, 0, "x"), aliceState.capture.token)).body.error, "capture_paused");
    r = await upload(t, "tok-alice", c.room, c.alice.id, 0, env, aliceState.capture.token);
    assert.equal(r.status, 200, JSON.stringify(r.body));
    assert.deepEqual(await upload(t, "tok-alice", c.room, c.alice.id, 0, env, aliceState.capture.token).then((x) => x.body), { accepted: true, duplicate: true });
    // A chunk sealed for Bob's identity but sent by Alice does not decrypt at meeting-intel.
    const forged = seal(aliceState, c.bobPid, 1, "forged");
    assert.equal((await upload(t, "tok-alice", c.room, c.alice.id, 1, forged, aliceState.capture.token)).body.error, "bad_chunk");
    const relayed = t.forwarded.filter((f) => f.path.endsWith("/chunks"));
    assert.ok(relayed.length >= 1);
    for (const f of relayed) assert.ok(!f.body.includes("SECRET-ALICE-AUDIO"), "plaintext never crossed the calls service");
    assert.ok(!JSON.stringify([...t.service.state.rooms.values()].map((x) => x.summary)).includes("SECRET"));
    assert.equal(t.transcribed[0], "SECRET-ALICE-AUDIO");
    assert.equal(JSON.parse(relayed[0].body).speaker, "/alice");

    // The host stops notes: the brief is made and stored for the two members who agreed.
    await t.req("tok-alice", "POST", `/calls/v1/rooms/${c.room}/summary`, { session: c.alice.id, enabled: false });
    assert.equal((await c.bob.summary((d) => d.state === "stopped")).reason, "host_stopped");
    for (let i = 0; i < 50 && !meeting.brief; i++) await settle(20);
    assert.equal(meeting.ended, "host_stopped");
    assert.equal(meeting.brief.status, "ready");
    const wrap = meeting.brief.wraps.find((w) => w.key_id === "bobbrowser01");
    const plain = JSON.parse(openBrief(on.body.meeting, meeting.brief, wrap, bk.ecdh).toString());
    assert.deepEqual(plain.transcript.map((x) => [x.speaker, x.text]), [["/alice", "SECRET-ALICE-AUDIO"]]);
    assert.equal((await upload(t, "tok-alice", c.room, c.alice.id, 2, env, aliceState.capture.token)).body.error, "capture_paused");
  } finally { await t.stop(); }
});

test("a late joiner pauses capture before joining; a guest is told and can agree but gets no copy", async () => {
  const t = await start();
  try {
    const c = await call(t);
    await c.alice.summary((d) => d.you.can_request);
    const on = (await t.req("tok-alice", "POST", `/calls/v1/rooms/${c.room}/summary`, { session: c.alice.id, enabled: true })).body;
    for (const [auth, s, id] of [["tok-alice", c.alice, "alicebrowser1"], ["tok-bob", c.bob, "bobbrowser01"]]) {
      const cur = await s.summary((d) => d.state === "waiting");
      await t.req(auth, "POST", `/calls/v1/rooms/${c.room}/summary/consent`, { session: s.id, consent: true, version: cur.version, disclosure_hash: cur.disclosure_hash, notes_key: notesKey(id).wire });
    }
    const active = await c.alice.summary((d) => d.state === "active");

    // A guest knocks and Bob lets them in: before the guest's `joined`, everyone is paused.
    const g = await t.guest();
    const gs = await t.stream(g);
    const k = await t.req(g, "POST", "/calls/v1/knocks", { session: gs.id, code: c.code, name: "Dana" });
    await c.bob.until("knock");
    await t.req("tok-bob", "POST", `/calls/v1/rooms/${c.room}/knocks/${k.body.knock}`, { session: c.bob.id, admit: true });
    const paused = await c.alice.summary((d) => d.state === "waiting");
    assert.equal(paused.waiting_for, 1);
    assert.ok(paused.version > active.version);
    // The guest hears about notes (the state arrives no later than their `joined`).
    const first = await gs.next();
    const second = first.event === "summary" ? first : await gs.until("summary");
    assert.equal(second.data.state, "waiting");
    assert.ok(second.data.disclosure.audience.includes("Guests can agree but don't get a copy"));
    // The old credential is dead.
    assert.equal((await upload(t, "tok-alice", c.room, c.alice.id, 0, seal(active, c.alicePid, 0, "after join"), active.capture.token)).body.error, "capture_paused");
    // The guest agrees (a notes key from a guest is ignored); capture resumes with new credentials.
    const cur = second.data;
    const r = await t.req(g, "POST", `/calls/v1/rooms/${c.room}/summary/consent`, { session: gs.id, consent: true, version: cur.version, disclosure_hash: cur.disclosure_hash, notes_key: notesKey("guestkey0001").wire });
    assert.equal(r.body.state, "active", JSON.stringify(r.body));
    const meeting = t.pb.meetings.get(on.meeting);
    assert.deepEqual([...meeting.attendees.keys()].sort(), ["/k/alice1", "/k/bob1"]);
    const resumed = await c.alice.summary((d) => d.state === "active");
    assert.notEqual(resumed.capture.token, active.capture.token);
    assert.equal((await upload(t, "tok-alice", c.room, c.alice.id, 0, seal(resumed, c.alicePid, 0, "resumed"), resumed.capture.token)).status, 200);
    // A guest's upload is labelled as a guest.
    const gview = await gs.summary((d) => d.state === "active");
    assert.equal((await upload(t, g, c.room, gs.id, 0, seal(gview, gview.you.participant, 0, "guest words"), gview.capture.token)).status, 200);
    assert.equal(JSON.parse(t.forwarded.filter((f) => f.path.endsWith("/chunks")).at(-1).body).speaker, "Guest Dana");
  } finally { await t.stop(); }
});

test("stop for me pauses everyone and discards my queued audio; the host leaving ends notes", async () => {
  const t = await start();
  try {
    const alice = await t.stream("tok-alice");
    const bob = await t.stream("tok-bob");
    const carol = await t.stream("tok-carol");
    const placed = await t.req("tok-alice", "POST", "/calls/v1/calls", { session: alice.id, from: "/alice", to: "/bob", kind: "audio" });
    await alice.summary((d) => d.you.can_request);
    await bob.until("incoming");
    await t.req("tok-bob", "POST", `/calls/v1/rooms/${placed.body.room}/accept`, { session: bob.id });
    await bob.until("joined");
    const room = placed.body.room;
    const k = await t.req("tok-carol", "POST", "/calls/v1/knocks", { session: carol.id, code: placed.body.code, as: "/k/carol1" });
    await alice.until("knock");
    await t.req("tok-alice", "POST", `/calls/v1/rooms/${room}/knocks/${k.body.knock}`, { session: alice.id, admit: true });
    await carol.until("joined");
    const on = (await t.req("tok-alice", "POST", `/calls/v1/rooms/${room}/summary`, { session: alice.id, enabled: true })).body;
    for (const [auth, s, id] of [["tok-alice", alice, "alicebrowser1"], ["tok-bob", bob, "bobbrowser01"], ["tok-carol", carol, "carolphone01"]]) {
      const cur = await s.summary((d) => d.state === "waiting" && d.meeting === on.meeting);
      await t.req(auth, "POST", `/calls/v1/rooms/${room}/summary/consent`, { session: s.id, consent: true, version: cur.version, disclosure_hash: cur.disclosure_hash, notes_key: notesKey(id).wire });
    }
    const bobActive = await bob.summary((d) => d.state === "active");
    const bobPid = bobActive.you.participant;
    // Bob stops notes for himself.
    const r = await t.req("tok-bob", "POST", `/calls/v1/rooms/${room}/summary/consent`, { session: bob.id, consent: false });
    assert.equal(r.body.state, "waiting");
    await settle();
    assert.ok(t.forwarded.some((f) => f.path.endsWith("/discard")), "meeting-intel was told to discard");
    assert.equal(t.pb.meetings.get(on.meeting).attendees.get("/k/bob1").consent, "withdrawn");
    assert.equal((await upload(t, "tok-bob", room, bob.id, 0, seal(bobActive, bobPid, 0, "late"), bobActive.capture.token)).body.error, "capture_paused");
    // The host leaves: notes stop for everyone (the call goes on between Bob and Carol).
    await t.req("tok-alice", "POST", `/calls/v1/rooms/${room}/leave`, { session: alice.id });
    const stopped = await carol.summary((d) => d.state === "stopped");
    assert.equal(stopped.reason, "host_left");
    await settle(150);
    assert.equal(t.pb.meetings.get(on.meeting).ended, "host_left");
    // No audio was ever captured, so the result is a failure, not an empty brief.
    assert.deepEqual(t.pb.meetings.get(on.meeting).brief, { status: "failed", reason: "no_speech" });
  } finally { await t.stop(); }
});

test("without the feature the host is not offered notes and a request is refused", async () => {
  const t = await start({ allowed: new Set() });
  try {
    const c = await call(t);
    await settle(150);
    const r = await t.req("tok-alice", "POST", `/calls/v1/rooms/${c.room}/summary`, { session: c.alice.id, enabled: true });
    assert.equal(r.status, 403);
    assert.equal(r.body.error, "notes_disabled");
    assert.equal(t.pb.meetings.size, 0);
    // Uploads with no notes running are refused.
    assert.equal((await t.req("tok-alice", "POST", `/calls/v1/rooms/${c.room}/summary/audio?session=${c.alice.id}&seq=0`, { v: 1 }, { "x-capture-token": "x" })).body.error, "capture_paused");
  } finally { await t.stop(); }
});
