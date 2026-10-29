import test from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import http from "node:http";
import { EventEmitter } from "node:events";
import {
  b64, generateSigningKeyPem, loadSigningKey, newSessionKey, openBrief, openChunk, sealChunk, unb64, verifySessionKey,
} from "../src/crypto.js";
import { createIntel, LIMITS } from "../src/service.js";
import { createHandler } from "../src/server.js";
import { createSummarizer, validateNotes, parseJsonObject } from "../src/summary.js";
import { BYTES_PER_SECOND, createDecoder, peak, wav } from "../src/audio.js";

const signing = loadSigningKey(generateSigningKeyPem());
const MEETING = "mtg_" + "a".repeat(26);
const tick = () => new Promise((r) => setImmediate(r));
async function until(fn, ms = 2000) {
  const end = Date.now() + ms;
  while (Date.now() < end) { if (await fn()) return; await new Promise((r) => setTimeout(r, 5)); }
  throw new Error("timed out");
}

// A loud (or silent) PCM buffer of `seconds`, tagged in its first sample so tests can tell windows apart.
function pcm(seconds, loud = true, tag = 1) {
  const b = Buffer.alloc(Math.round(seconds * BYTES_PER_SECOND));
  if (loud) for (let i = 0; i < b.length; i += 2) b.writeInt16LE(i % 4 ? 8000 : -8000, i);
  b.writeInt16LE(loud ? 9000 + tag : 0, 0);
  return b;
}

function rig(over = {}) {
  const decoded = []; // every PCM buffer handed out, to check it was wiped
  const posted = [];
  const recipientKeys = new Map(); // key_id -> ECDH private
  const recipients = over.recipients || [{ account_id: "acct_a", address: "a@x", keys: [{ id: "keyAAAAAAAA1" }] }];
  const rec = recipients.map((r) => ({
    account_id: r.account_id, address: r.address,
    keys: r.keys.map((k) => { const e = crypto.createECDH("prime256v1"); e.generateKeys(); recipientKeys.set(k.id, e); return { key_id: k.id, public_key: b64(e.getPublicKey(null, "uncompressed")) }; }),
  }));
  let transcribeGate = null;
  const deps = {
    signing,
    decode: over.decode || (async (plain) => { const p = pcm(Number(plain.toString().split(":")[1] || 10), !plain.toString().startsWith("silent"), Number(plain.toString().split(":")[2] || 1)); decoded.push(p); return p; }),
    transcribe: over.transcribe || (async (p) => {
      if (transcribeGate) await transcribeGate.promise;
      const tag = p.readInt16LE(0) - 9000;
      return { language: tag === 3 ? "turkish" : "english", segments: [{ start: 0.5, end: 3, text: `words from window ${tag}` }] };
    }),
    summarizer: over.summarizer || { summarize: async (lines, speakers) => ({ notes: { title: "Weekly", summary: "We met.", decisions: ["Ship"], action_items: [{ owner: [...speakers][0], task: "Write notes", due: null, quote_ts: null }], open_questions: [] }, draft: false }) },
    postbox: async (method, path, body) => {
      posted.push({ method, path, body });
      if (path.endsWith("/recipients")) return over.recipientsStatus ? { status: over.recipientsStatus, body: null } : { status: 200, body: { recipients: rec } };
      return { status: 200, body: { stored: true } };
    },
    log: () => {},
  };
  const intel = createIntel({ sttName: "whisper.cpp v1.9.5 small", llmName: "qwen-test" }, deps);
  return {
    intel, decoded, posted, recipientKeys,
    gate() { let release; const promise = new Promise((r) => { release = r; }); transcribeGate = { promise }; return () => { transcribeGate = null; release(); }; },
  };
}

function send(intel, s, opened, participant, seq, content, t, dur = 10_000) {
  const env = sealChunk(unb64(opened.key, 65), s.id, participant, seq, Buffer.from(content), { codec: "audio/webm;codecs=opus", t, dur });
  return intel.chunk(s, { participant, speaker: participant === "pid_aaaaaaaaaaaa" ? "/ada/main" : "Guest Deniz", seq, envelope: env });
}

test("crypto: chunks and briefs interoperate, and are bound to their context", () => {
  const key = newSessionKey();
  const env = sealChunk(key.publicRaw, "ses_x", "pid_one_11111", 4, Buffer.from("opus bytes"), { codec: "audio/webm", t: 1000, dur: 9000 });
  assert.equal(openChunk(key, "ses_x", "pid_one_11111", 4, env).toString(), "opus bytes");
  // Another participant, sequence number or session cannot open it.
  assert.throws(() => openChunk(key, "ses_x", "pid_two_22222", 4, env));
  assert.throws(() => openChunk(key, "ses_x", "pid_one_11111", 5, env));
  assert.throws(() => openChunk(key, "ses_y", "pid_one_11111", 4, env));
  assert.throws(() => openChunk(newSessionKey(), "ses_x", "pid_one_11111", 4, env));
  // The session key's signature verifies against the pinned public key, and only for its own values.
  const intel = createIntel({}, { signing, decode: null, transcribe: null, summarizer: null, postbox: async () => ({ status: 200 }), log: () => {} });
  const o = intel.open({ meeting_id: MEETING });
  assert.ok(verifySessionKey(signing.publicRaw, o.session, MEETING, o.key, o.expires_at, o.signature));
  assert.ok(!verifySessionKey(signing.publicRaw, o.session, "mtg_" + "b".repeat(26), o.key, o.expires_at, o.signature));
  assert.ok(!verifySessionKey(loadSigningKey(generateSigningKeyPem()).publicRaw, o.session, MEETING, o.key, o.expires_at, o.signature));
  intel.abort(intel.sessionFor(o.session));
});

test("pipeline: two speakers, windows, brief encrypted to the recipients' keys, everything wiped", async () => {
  const r = rig({ recipients: [{ account_id: "acct_a", address: "a@x", keys: [{ id: "keyAAAAAAAA1" }, { id: "keyAAAAAAAA2" }] }, { account_id: "acct_c", address: "c@x", keys: [{ id: "keyCCCCCCCC1" }] }] });
  const o = r.intel.open({ meeting_id: MEETING });
  const s = r.intel.sessionFor(o.session);
  const t0 = Date.parse("2026-10-27T10:00:00Z");
  // Ada speaks 3 x 10 s (one window fills at 28 s), the guest 1 x 10 s in Turkish.
  await send(r.intel, s, o, "pid_aaaaaaaaaaaa", 0, "loud:10:1", t0);
  await send(r.intel, s, o, "pid_aaaaaaaaaaaa", 1, "loud:10:1", t0 + 10_000);
  await send(r.intel, s, o, "pid_aaaaaaaaaaaa", 2, "loud:10:1", t0 + 20_000);
  await send(r.intel, s, o, "pid_bbbbbbbbbbbb", 0, "loud:10:3", t0 + 5_000);
  // A retried chunk is a duplicate, not a second copy.
  assert.deepEqual(await send(r.intel, s, o, "pid_bbbbbbbbbbbb", 0, "loud:10:3", t0 + 5_000), { accepted: true, duplicate: true });
  await r.intel.finish(s, "host_stopped");
  const brief = r.posted.find((p) => p.path.endsWith("/brief"));
  assert.equal(brief.body.status, "ready");
  assert.equal(brief.body.wraps.length, 3);
  // Each recipient key opens the same brief; it carries the transcript in time order with labels.
  const wrapA = brief.body.wraps.find((w) => w.key_id === "keyAAAAAAAA2");
  const plain = JSON.parse(openBrief(MEETING, brief.body, wrapA, r.recipientKeys.get("keyAAAAAAAA2")).toString());
  assert.equal(plain.summary, "We met.");
  assert.deepEqual(plain.participants.sort(), ["/ada/main", "Guest Deniz"]);
  assert.equal(plain.transcript[0].speaker, "/ada/main");
  assert.equal(plain.transcript[1].speaker, "Guest Deniz");
  assert.equal(plain.transcript[1].lang, "tr");
  assert.equal(plain.generated_by.stt, "whisper.cpp v1.9.5 small");
  // A key of one recipient cannot open another's wrap.
  const wrapC = brief.body.wraps.find((w) => w.key_id === "keyCCCCCCCC1");
  assert.throws(() => openBrief(MEETING, brief.body, wrapC, r.recipientKeys.get("keyAAAAAAAA1")));
  // Audio is gone: every decoded buffer was zeroed, and the session no longer exists.
  assert.ok(r.decoded.length >= 4);
  for (const b of r.decoded) assert.equal(peak(b), 0, "decoded audio was wiped");
  assert.equal(r.intel._sessions.size, 0);
  assert.equal(r.intel._queue.length, 0);
  assert.throws(() => r.intel.sessionFor(o.session), /no_session/);
});

test("queue limits refuse with busy, and a busy session does not starve another", async () => {
  const r = rig();
  const release = r.gate();
  const o = r.intel.open({ meeting_id: MEETING });
  const s = r.intel.sessionFor(o.session);
  let refused = null;
  let seq = 0;
  for (let i = 0; i < 100 && !refused; i++) {
    try { await send(r.intel, s, o, "pid_aaaaaaaaaaaa", seq, "loud:28:1", 1_000_000 + seq * 28_000, 28_000); seq++; } catch (e) { refused = e; }
  }
  assert.equal(refused && refused.code, "busy");
  assert.ok(seq * 28 <= LIMITS.sessionQueueSeconds + 28);
  // Another meeting still gets in until the global bound.
  const o2 = r.intel.open({ meeting_id: "mtg_" + "c".repeat(26) });
  const s2 = r.intel.sessionFor(o2.session);
  await send(r.intel, s2, o2, "pid_aaaaaaaaaaaa", 0, "loud:10:2", 2_000_000);
  // Sessions are bounded too.
  for (let i = r.intel._sessions.size; i < LIMITS.sessions; i++) r.intel.open({ meeting_id: "mtg_" + String(i).repeat(26).slice(0, 26) });
  assert.throws(() => r.intel.open({ meeting_id: "mtg_" + "d".repeat(26) }), /busy/);
  release();
  for (const x of [...r.intel._sessions.values()]) r.intel.abort(x);
  for (const b of r.decoded) assert.equal(peak(b), 0);
});

test("failures clean up: undecryptable, undecodable, STT down, LLM down, no speech, abort", async () => {
  // Undecryptable: refused, nothing kept.
  let r = rig();
  let o = r.intel.open({ meeting_id: MEETING });
  let s = r.intel.sessionFor(o.session);
  const wrongKey = newSessionKey();
  const env = sealChunk(wrongKey.publicRaw, s.id, "pid_aaaaaaaaaaaa", 0, Buffer.from("loud:10:1"), { codec: "audio/webm", t: 1, dur: 10_000 });
  await assert.rejects(r.intel.chunk(s, { participant: "pid_aaaaaaaaaaaa", speaker: "x", seq: 0, envelope: env }), /undecryptable/);
  // Undecodable: the decrypted bytes are zeroed even though ffmpeg failed.
  let seen = null;
  r = rig({ decode: async (plain) => { seen = plain; throw new Error("decode_failed"); } });
  o = r.intel.open({ meeting_id: MEETING });
  s = r.intel.sessionFor(o.session);
  await assert.rejects(send(r.intel, s, o, "pid_aaaaaaaaaaaa", 0, "loud:10:1", 1), /undecodable/);
  assert.ok(seen && seen.every((x) => x === 0));
  r.intel.abort(s);
  // STT down: the meeting fails as stt_unavailable, and audio is wiped.
  r = rig({ transcribe: async () => { throw new Error("stt_status_503"); } });
  o = r.intel.open({ meeting_id: MEETING });
  s = r.intel.sessionFor(o.session);
  await send(r.intel, s, o, "pid_aaaaaaaaaaaa", 0, "loud:10:1", 1);
  await r.intel.finish(s, "call_ended");
  assert.deepEqual(r.posted.at(-1).body, { status: "failed", reason: "stt_unavailable" });
  for (const b of r.decoded) assert.equal(peak(b), 0);
  assert.equal(r.intel._sessions.size, 0);
  // LLM down: a marked draft with the transcript, never an unvalidated summary.
  r = rig({ summarizer: createSummarizer({ url: "http://llm.invalid/v1/chat/completions", model: "m", retries: 1, sleep: async () => {}, fetch: async () => ({ status: 503, ok: false }) }) });
  o = r.intel.open({ meeting_id: MEETING });
  s = r.intel.sessionFor(o.session);
  await send(r.intel, s, o, "pid_aaaaaaaaaaaa", 0, "loud:10:1", 1);
  await r.intel.finish(s, "host_stopped");
  const b = r.posted.find((p) => p.path.endsWith("/brief")).body;
  const plain = JSON.parse(openBrief(MEETING, b, b.wraps[0], r.recipientKeys.get("keyAAAAAAAA1")).toString());
  assert.equal(plain.draft, true);
  assert.equal(plain.summary, null);
  assert.equal(plain.transcript.length, 1);
  // Only silence: no whisper call, failed as no_speech.
  let calls = 0;
  r = rig({ transcribe: async () => { calls++; return { segments: [] }; } });
  o = r.intel.open({ meeting_id: MEETING });
  s = r.intel.sessionFor(o.session);
  await send(r.intel, s, o, "pid_aaaaaaaaaaaa", 0, "silent:10:1", 1);
  await r.intel.finish(s, "host_stopped");
  assert.equal(calls, 0);
  assert.deepEqual(r.posted.at(-1).body, { status: "failed", reason: "no_speech" });
  // Nobody with a key: failed, not stored for nobody.
  r = rig({ recipients: [] });
  o = r.intel.open({ meeting_id: MEETING });
  s = r.intel.sessionFor(o.session);
  await send(r.intel, s, o, "pid_aaaaaaaaaaaa", 0, "loud:10:1", 1);
  await r.intel.finish(s, "host_stopped");
  assert.deepEqual(r.posted.at(-1).body, { status: "failed", reason: "no_recipients" });
  // Abort mid-meeting: queued and open audio are wiped at once.
  r = rig();
  const release = r.gate();
  o = r.intel.open({ meeting_id: MEETING });
  s = r.intel.sessionFor(o.session);
  await send(r.intel, s, o, "pid_aaaaaaaaaaaa", 0, "loud:28:1", 1, 28_000);
  await send(r.intel, s, o, "pid_aaaaaaaaaaaa", 1, "loud:10:1", 29_000);
  r.intel.abort(s);
  for (const x of r.decoded.slice(1)) assert.equal(peak(x), 0);
  release();
  await tick();
  assert.equal(r.intel._sessions.size, 0);
  assert.deepEqual(r.posted.at(-1).body, { status: "failed", reason: "aborted" });
});

test("discard during decode rejects and wipes the in-flight audio", async () => {
  let release;
  const gate = new Promise((r) => { release = r; });
  const decoded = pcm(10);
  let plain;
  let transcriptions = 0;
  const r = rig({ decode: async (p) => { plain = p; await gate; return decoded; }, transcribe: async () => { transcriptions++; return { segments: [] }; } });
  const o = r.intel.open({ meeting_id: MEETING });
  const s = r.intel.sessionFor(o.session);
  try {
    const pending = send(r.intel, s, o, "pid_aaaaaaaaaaaa", 0, "loud:10:1", 1);
    await until(() => plain != null);
    r.intel.discard(s, { participant: "pid_aaaaaaaaaaaa" });
    release();
    await assert.rejects(pending, /discarded/);
    assert.equal(peak(decoded), 0);
    assert.ok(plain.every((x) => x === 0));
    assert.equal(s.open.size, 0);
    assert.equal(transcriptions, 0);
  } finally { release(); r.intel.abort(s); }
});

test("transcription failure wipes the actual joined PCM passed to STT", async () => {
  let submitted;
  const r = rig({ transcribe: async (p) => { submitted = p; throw new Error("stt_unavailable"); } });
  const o = r.intel.open({ meeting_id: MEETING });
  const s = r.intel.sessionFor(o.session);
  await send(r.intel, s, o, "pid_aaaaaaaaaaaa", 0, "loud:10:1", 1);
  await r.intel.finish(s, "call_ended");
  assert.ok(submitted);
  assert.equal(peak(submitted), 0, "the STT copy is zeroed on rejection too");
  assert.equal(r.intel._sessions.size, 0);
});

test("stop for me: a participant's unprocessed audio is discarded and their in-flight window ignored", async () => {
  const r = rig();
  const release = r.gate();
  const o = r.intel.open({ meeting_id: MEETING });
  const s = r.intel.sessionFor(o.session);
  await send(r.intel, s, o, "pid_aaaaaaaaaaaa", 0, "loud:28:1", 1, 28_000); // in whisper now
  await send(r.intel, s, o, "pid_bbbbbbbbbbbb", 0, "loud:28:3", 1, 28_000); // queued
  await send(r.intel, s, o, "pid_bbbbbbbbbbbb", 1, "loud:10:3", 29_000); // open window
  await tick();
  assert.deepEqual(r.intel.discard(s, { participant: "pid_bbbbbbbbbbbb" }), { discarded: 2 });
  // A's window is in whisper; the remainder after its pause cut is still open and goes too.
  assert.deepEqual(r.intel.discard(s, { participant: "pid_aaaaaaaaaaaa" }), { discarded: 1 });
  release();
  await r.intel.finish(s, "host_stopped");
  // Nothing of B made it, and A's in-flight window was dropped too.
  assert.deepEqual(r.posted.at(-1).body, { status: "failed", reason: "no_speech" });
  for (const b of r.decoded) assert.equal(peak(b), 0);
});

test("summary validation keeps owners and timestamps honest", () => {
  const speakers = new Set(["/ada/main", "Guest Deniz"]);
  const stamps = new Set(["00:21"]);
  const v = validateNotes({
    title: "T", summary: "s ".repeat(300), decisions: ["d", 3, ""],
    action_items: [
      { owner: "/ada/main", task: "real", due: "Friday", quote_ts: "[00:21]" },
      { owner: "/mallory/x", task: "made up owner", due: null, quote_ts: "99:99" },
      { owner: null },
    ],
    open_questions: "not an array",
  }, speakers, stamps);
  assert.equal(v.action_items.length, 2);
  assert.equal(v.action_items[0].quote_ts, "00:21");
  assert.equal(v.action_items[1].owner, null);
  assert.equal(v.action_items[1].quote_ts, null);
  assert.deepEqual(v.decisions, ["d"]);
  assert.deepEqual(v.open_questions, []);
  assert.ok(v.summary.split(" ").length <= 121);
  assert.equal(validateNotes({ title: "no summary" }, speakers, stamps), null);
  assert.deepEqual(parseJsonObject('Sure! ```json\n{"a":1}\n```'), { a: 1 });
  assert.equal(parseJsonObject("no json"), null);
  // The WAV header is what whisper.cpp reads.
  const w = wav(Buffer.alloc(32000));
  assert.equal(w.toString("ascii", 0, 4), "RIFF");
  assert.equal(w.readUInt32LE(24), 16000);
});

test("http: the service token is required; health needs none and leaks no content", async () => {
  const r = rig();
  const handler = createHandler({ serviceToken: "t".repeat(40) }, r.intel);
  const server = http.createServer(handler);
  await new Promise((res) => server.listen(0, "127.0.0.1", res));
  const base = `http://127.0.0.1:${server.address().port}`;
  try {
    let res = await fetch(base + "/v1/sessions", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ meeting_id: MEETING }) });
    assert.equal(res.status, 401);
    res = await fetch(base + "/v1/sessions", { method: "POST", headers: { "content-type": "application/json", authorization: "Bearer " + "x".repeat(40) }, body: JSON.stringify({ meeting_id: MEETING }) });
    assert.equal(res.status, 401);
    res = await fetch(base + "/v1/sessions", { method: "POST", headers: { "content-type": "application/json", authorization: "Bearer " + "t".repeat(40) }, body: JSON.stringify({ meeting_id: MEETING }) });
    assert.equal(res.status, 200);
    const o = await res.json();
    res = await fetch(base + `/v1/sessions/${o.session}/chunks`, { method: "POST", headers: { "content-type": "application/json", authorization: "Bearer " + "t".repeat(40) }, body: "x".repeat(400 * 1024) });
    assert.equal(res.status, 413);
    res = await fetch(base + "/v1/health");
    const h = await res.json();
    assert.equal(h.sessions, 1);
    assert.ok(!JSON.stringify(h).includes(o.key));
    r.intel.abort(r.intel.sessionFor(o.session));
  } finally {
    server.close();
  }
});

for (const failure of ["overflow", "timeout"]) {
  test(`decoder wipes buffered and late output after ${failure}`, async () => {
    const child = new EventEmitter();
    child.stdout = new EventEmitter();
    child.stdin = new EventEmitter();
    child.stdin.end = () => {};
    child.kill = () => {};
    const decode = createDecoder({ spawn: () => child, maxSeconds: 1, timeoutMs: failure === "timeout" ? 10 : 1000 });
    const pending = decode(Buffer.from("synthetic audio"));
    const rejected = assert.rejects(pending, failure === "timeout" ? /decode_timeout/ : /decode_too_long/);
    const first = Buffer.alloc(20, 7);
    child.stdout.emit("data", first);
    const excess = Buffer.alloc(BYTES_PER_SECOND, 9);
    if (failure === "overflow") child.stdout.emit("data", excess);
    await rejected;
    assert.ok(first.every((x) => x === 0));
    if (failure === "overflow") assert.ok(excess.every((x) => x === 0), "overflowing buffer is wiped too");
    const late = Buffer.alloc(20, 11);
    child.stdout.emit("data", late);
    assert.ok(late.every((x) => x === 0), "late pipe data must not be retained after termination");
    child.emit("close", 1);
  });
}
