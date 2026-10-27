// Phase 7: the in-call notes client and the Meeting notes reader, in jsdom with Node's WebCrypto,
// a fake MediaRecorder and a tiny in-memory IndexedDB. No microphone, network or real person.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { JSDOM, VirtualConsole } from "jsdom";
import crypto from "node:crypto";
import { b64, generateSigningKeyPem, loadSigningKey, newSessionKey, openChunk, sealBrief, signSessionKey } from "../../meeting-intel/src/crypto.js";

const read = (file) => readFileSync(new URL("../" + file, import.meta.url), "utf8");
const tick = (ms = 5) => new Promise((r) => setTimeout(r, ms));
async function until(predicate, what = "condition") {
  for (let n = 0; n < 400; n++) { if (await predicate()) return; await tick(); }
  assert.fail("timed out waiting for " + what);
}
const signing = loadSigningKey(generateSigningKeyPem());
const MEETING = "mtg_" + "q".repeat(26);

function fakeIndexedDB() {
  const stores = new Map();
  return {
    open() {
      const req = {};
      setTimeout(() => {
        const db = {
          createObjectStore(n) { stores.set(n, new Map()); },
          transaction(n) {
            const tx = {
              objectStore() {
                const m = stores.get(n);
                return {
                  get(k) { const r = {}; setTimeout(() => { r.result = m.get(k); r.onsuccess && r.onsuccess(); }); return r; },
                  put(v, k) { m.set(k, v); setTimeout(() => tx.oncomplete && tx.oncomplete()); },
                };
              },
            };
            return tx;
          },
        };
        req.result = db;
        if (!stores.size) req.onupgradeneeded && req.onupgradeneeded();
        req.onsuccess && req.onsuccess();
      });
      return req;
    },
  };
}

function page(t, { pinned = signing, guest = false } = {}) {
  const errors = [], vc = new VirtualConsole();
  vc.on("jsdomError", (e) => errors.push(e.message));
  const dom = new JSDOM(`<!doctype html><body data-page="inbox"><nav id="side-nav"></nav>
    <div id="call-ui"><div class="call-main"><footer id="call-controls"><button id="call-hangup"></button></footer></div></div></body>`, {
    url: "https://inbox.pigeonpost.dev/", runScripts: "outside-only", pretendToBeVisual: true, virtualConsole: vc,
  });
  const w = dom.window;
  Object.defineProperty(w, "crypto", { value: globalThis.crypto, configurable: true });
  w.TextEncoder = TextEncoder;
  w.TextDecoder = TextDecoder;
  w.indexedDB = fakeIndexedDB();
  const recorders = [];
  w.MediaStream = class { constructor(tracks) { this.tracks = tracks; } getTracks() { return this.tracks; } };
  w.MediaRecorder = class {
    static isTypeSupported(m) { return m === "audio/webm;codecs=opus"; }
    constructor(stream, opts) { this.stream = stream; this.opts = opts; this.state = "inactive"; this.mimeType = opts.mimeType; recorders.push(this); }
    start() { this.state = "recording"; }
    stop() { if (this.state === "inactive") return; this.state = "inactive"; this.ondataavailable({ data: new w.Blob([this.payload || "OPUS-FRAMES-" + recorders.indexOf(this)], { type: this.mimeType }) }); this.onstop(); }
  };
  const posts = [], uploads = [];
  let uploadStatus = 200;
  w.fetch = async (url, opts = {}) => {
    uploads.push({ url, headers: opts.headers, body: JSON.parse(opts.body) });
    return { ok: uploadStatus === 200, status: uploadStatus, json: async () => ({}) };
  };
  const mic = { kind: "audio", readyState: "live", id: "own-processed-mic" };
  let muted = false;
  const toasts = [];
  const el = (tag, attrs, ...children) => {
    const node = w.document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v == null || v === false) continue;
      if (k === "class") node.className = v; else if (k === "text") node.textContent = v;
      else if (k.startsWith("on")) node.addEventListener(k.slice(2), v); else node.setAttribute(k, v === true ? "" : v);
    }
    for (const c of children.flat()) if (c != null) node.append(c);
    return node;
  };
  const room = { id: "r".repeat(22), you: "pid_aaaaaaaaaaaa" };
  let reply = null;
  let source = read("calls-summary.js");
  source = source.replace(/const PINNED_KEYS = \[[^\]]*\];/, `const PINNED_KEYS = [${JSON.stringify(b64(pinned.publicRaw))}];`);
  w.eval(source);
  w.eval(read("meeting-notes.js"));
  w.PigeonpostCallsSummary.attach({
    room: () => room, session: () => "session-1", mic: () => mic, muted: () => muted,
    post: async (path, body) => { posts.push({ path, body }); if (reply) return reply(path, body); return {}; },
    base: "/calls/v1", authHeader: () => (guest ? "Guest g" : "Bearer member"), toast: (m) => toasts.push(m), el,
    svg: () => w.document.createElementNS("http://www.w3.org/2000/svg", "svg"), isGuest: () => guest,
  });
  t.after(async () => { await tick(20); w.close(); assert.deepEqual(errors, []); });
  return {
    w, room, mic, posts, uploads, recorders, toasts,
    setMuted: (v) => { muted = v; w.PigeonpostCallsSummary.muteChanged(); },
    setUploadStatus: (s) => { uploadStatus = s; },
    onReply: (fn) => { reply = fn; },
    $: (id) => w.document.getElementById(id),
  };
}

function serviceKey(signer = signing) {
  const key = newSessionKey();
  const session = "ses_" + crypto.randomBytes(12).toString("hex");
  const expires = Math.floor(Date.now() / 1000) + 3600;
  const keyB64 = b64(key.publicRaw);
  return { key, wire: { session, key: keyB64, key_id: signer.keyId, expires_at: expires, signature: signSessionKey(signer, session, MEETING, keyB64, expires) } };
}

function view(p, sk, over = {}) {
  return {
    room: p.room.id, state: "waiting", meeting: MEETING, version: 3, host: "/alice", retention_days: 90,
    disclosure: { v: 1, processor: "Pigeonpost's own notes service", capture: "Only your own microphone", audio: "Encrypted on your device", audience: "Members in this call who agree. Guests can agree but don't get a copy.", retention: "90 days", limits: "Use headphones", control: "Anyone can stop" },
    disclosure_hash: "HASH", service_key: sk.wire, waiting_for: 1, agreed: [],
    you: { participant: p.room.you, consent: null, can_control: false, can_request: false }, capture: null, ...over,
  };
}

test("everyone is asked; Agree carries the device's notes key; capture records only the own track, encrypted", async (t) => {
  const p = page(t);
  const sk = serviceKey();
  p.w.PigeonpostCallsSummary.onEvent(view(p, sk));
  await until(() => p.$("call-notes-sheet") && !p.$("call-notes-sheet").hidden, "consent sheet");
  assert.match(p.$("call-notes-title").textContent, /\/alice asked to take meeting notes/);
  assert.match(p.$("call-notes-list").textContent, /Only your own microphone/);
  assert.equal(p.$("call-notes-banner").hidden, false, "the indicator shows before anyone agrees");
  assert.match(p.$("call-notes-text").textContent, /paused until everyone agrees/);
  assert.equal(p.recorders.length, 0, "nothing is captured before consent");

  p.$("call-notes-agree").click();
  await until(() => p.posts.length === 1, "consent post");
  const consent = p.posts[0];
  assert.equal(consent.path, `/rooms/${p.room.id}/summary/consent`);
  assert.equal(consent.body.consent, true);
  assert.equal(consent.body.version, 3);
  assert.equal(consent.body.disclosure_hash, "HASH");
  assert.match(consent.body.notes_key.key_id, /^nk[A-Za-z0-9_-]{16}$/);
  assert.equal(Buffer.from(consent.body.notes_key.public_key, "base64").length, 65);

  // Everyone agreed: the service issues a credential and capture starts on the own processed track.
  p.w.PigeonpostCallsSummary.onEvent(view(p, sk, { state: "active", version: 4, you: { participant: p.room.you, consent: "yes", can_control: false }, capture: { token: "cap-token", version: 4 } }));
  await until(() => p.recorders.length === 1, "recorder");
  assert.equal(p.recorders[0].stream.tracks.length, 1);
  assert.equal(p.recorders[0].stream.tracks[0], p.mic, "only this device's own microphone track");
  assert.equal(p.recorders[0].opts.mimeType, "audio/webm;codecs=opus");
  assert.match(p.$("call-notes-text").textContent, /your microphone is being captured/);

  // A chunk completes (the mic track changed): it is encrypted to the session key and uploaded.
  p.w.PigeonpostCallsSummary.micChanged();
  await until(() => p.uploads.length === 1, "upload");
  const up = p.uploads[0];
  assert.match(up.url, new RegExp(`/calls/v1/rooms/${p.room.id}/summary/audio\\?session=session-1&seq=0$`));
  assert.equal(up.headers["x-capture-token"], "cap-token");
  assert.equal(up.headers.authorization, "Bearer member");
  assert.ok(!JSON.stringify(up.body).includes("OPUS-FRAMES"), "the upload is ciphertext");
  const plain = openChunk(sk.key, sk.wire.session, p.room.you, 0, up.body);
  assert.equal(plain.toString(), "OPUS-FRAMES-0");
  assert.equal(p.recorders.length, 2, "recording continues");

  // Stop for me: this device stops at once, then tells the service.
  p.$("call-notes-stopme").click();
  await until(() => p.posts.length === 2, "withdrawal");
  assert.deepEqual({ ...p.posts[1].body }, { session: "session-1", consent: false });
  assert.equal(p.recorders[1].state, "inactive");
  assert.equal(p.w.PigeonpostCallsSummary.inspect().capturing, false);
  await tick(20);
  assert.equal(p.uploads.length, 1, "nothing recorded after stopping is uploaded");
});

test("a session key not signed by the pinned notes key is never used", async (t) => {
  const p = page(t);
  const forged = serviceKey(loadSigningKey(generateSigningKeyPem()));
  p.w.PigeonpostCallsSummary.onEvent(view(p, forged));
  await until(() => p.$("call-notes-sheet") && !p.$("call-notes-sheet").hidden);
  assert.equal(p.$("call-notes-agree").disabled, true);
  assert.match(p.$("call-notes-warn").textContent, /couldn't verify/);
  p.w.PigeonpostCallsSummary.onEvent(view(p, forged, { state: "active", you: { participant: p.room.you, consent: "yes" }, capture: { token: "x", version: 4 } }));
  await tick(30);
  assert.equal(p.recorders.length, 0);
  assert.match(p.$("call-notes-text").textContent, /can't be verified/);
});

test("guests are told they get no copy and send no notes key", async (t) => {
  const p = page(t, { guest: true });
  const sk = serviceKey();
  p.w.PigeonpostCallsSummary.onEvent(view(p, sk));
  await until(() => p.$("call-notes-sheet") && !p.$("call-notes-sheet").hidden);
  assert.match(p.$("call-notes-warn").textContent, /guest: you can agree, but you won't receive a copy/);
  p.$("call-notes-agree").click();
  await until(() => p.posts.length === 1);
  assert.equal(p.posts[0].body.notes_key, undefined);
  // Declining is always possible.
  p.w.PigeonpostCallsSummary.onEvent(view(p, sk, { you: { participant: p.room.you, consent: "yes" } }));
  await tick();
  p.$("call-notes-stopme").click();
  await until(() => p.posts.length === 2);
  assert.equal(p.posts[1].body.consent, false);
});

test("muted chunks are not uploaded; a paused service drops the backlog; the host can turn notes off", async (t) => {
  const p = page(t);
  const sk = serviceKey();
  const active = view(p, sk, { state: "active", version: 5, you: { participant: p.room.you, consent: "yes", can_control: true }, capture: { token: "tok", version: 5 } });
  p.w.PigeonpostCallsSummary.onEvent(active);
  await until(() => p.recorders.length === 1);
  p.setMuted(true);
  p.w.PigeonpostCallsSummary.micChanged(); // chunk 1 started unmuted: uploaded
  await until(() => p.uploads.length === 1);
  p.w.PigeonpostCallsSummary.micChanged(); // chunk 2 was muted throughout: not uploaded
  await tick(20);
  assert.equal(p.uploads.length, 1);
  p.setMuted(false);
  p.setUploadStatus(409);
  p.w.PigeonpostCallsSummary.micChanged();
  await until(() => p.uploads.length === 2);
  assert.equal(p.w.PigeonpostCallsSummary.inspect().queued, 0, "a paused interval's audio is dropped");
  // The host's banner offers "Turn off notes".
  assert.equal(p.$("call-notes-off").hidden, false);
  p.onReply((path, body) => ({ ...active, state: "stopped", reason: "host_stopped", capture: null }));
  p.$("call-notes-off").click();
  await until(() => p.posts.some((x) => x.path.endsWith("/summary") && x.body.enabled === false));
  await until(() => p.$("call-notes-banner").hidden);
  assert.equal(p.w.PigeonpostCallsSummary.inspect().capturing, false);
  assert.match(p.toasts.at(-1), /summary will appear in Meeting notes/);
  // Leaving the call stops everything.
  p.w.PigeonpostCallsSummary.left();
  assert.equal(p.$("call-notes-btn").hidden, true);
});

test("the notes reader opens a brief encrypted for this device's key, and only that", async (t) => {
  const p = page(t);
  const k = await p.w.PigeonpostNotesKeys.get();
  const other = crypto.createECDH("prime256v1");
  other.generateKeys();
  const brief = { v: 1, meeting_id: MEETING, title: "Lighthouse release", language: "en", started_at: "2026-10-27T10:00:00Z", duration_s: 49,
    participants: ["/ada/main", "Guest Deniz"], summary: "Launch set for 12 November.", decisions: ["Launch on Thursday 12 November"],
    action_items: [{ owner: "Guest Deniz", task: "Prepare release notes", due: "Friday", quote_ts: "00:21" }], open_questions: ["Second billing review?"],
    draft: false, transcript: [{ at: "00:00", speaker: "/ada/main", text: "Good morning everyone." }], generated_by: { stt: "whisper.cpp v1.9.5 small", llm: "qwen" } };
  const sealed = sealBrief(MEETING, Buffer.from(JSON.stringify(brief)), [
    { account_id: "acct_me", keys: [{ key_id: k.key_id, public_key: k.public_key }] },
    { account_id: "acct_other", keys: [{ key_id: "nkotherdevice00000", public_key: b64(other.getPublicKey(null, "uncompressed")) }] },
  ]);
  const mine = { id: MEETING, state: "ready", iv: sealed.iv, ciphertext: sealed.ciphertext, keys: sealed.wraps.filter((x) => x.key_id === k.key_id) };
  const opened = await p.w.PigeonpostNotesKeys.open(mine);
  assert.equal(opened.summary, "Launch set for 12 November.");
  const node = p.w.PigeonpostNotes._renderBrief(opened);
  assert.match(node.textContent, /Guest Deniz: Prepare release notes \(due Friday\)/);
  assert.match(node.textContent, /00:00\/ada\/main:Good morning everyone\./);
  assert.match(p.w.PigeonpostNotes._asText(opened), /\[00:00\] \/ada\/main: Good morning everyone\./);
  // Only another device's wrap: this device cannot open it.
  const theirs = { ...mine, keys: sealed.wraps.filter((x) => x.key_id !== k.key_id) };
  await assert.rejects(p.w.PigeonpostNotesKeys.open(theirs), (e) => e.code === "no_key_for_device");
  // A tampered ciphertext fails.
  const bad = { ...mine, ciphertext: Buffer.from(Buffer.from(mine.ciphertext, "base64").map((x, i) => (i === 3 ? x ^ 1 : x))).toString("base64") };
  await assert.rejects(p.w.PigeonpostNotesKeys.open(bad));
  // The key is stable for this browser.
  assert.equal((await p.w.PigeonpostNotesKeys.get()).key_id, k.key_id);
});

test("calls.js hands summary events to the notes module and stops it when the call ends", () => {
  const src = read("calls.js");
  assert.match(src, /case "summary": window\.PigeonpostCallsSummary\?\.onEvent\(data\)/);
  assert.match(src, /window\.PigeonpostCallsSummary\?\.left\(\); \/\/ Phase 7/);
  assert.match(src, /window\.PigeonpostCallsSummary\?\.micChanged\(\)/);
  assert.match(src, /mic: \(\) => local\.mic/, "the notes module gets the sent (own) microphone track only");
  for (const f of ["index.html", "join.html", "m.html", "native-call.html"]) {
    const html = read(f);
    assert.ok(html.indexOf("calls-summary.js") > 0 && html.indexOf("calls-summary.js") < html.indexOf("calls.js?v="), f + " loads the notes module before calls.js");
    assert.match(html, /calls-summary\.css/);
  }
  // Every page script stays same-origin under the inbox CSP (no inline handlers or blobs).
  assert.doesNotMatch(read("calls-summary.js"), /new Worker|blob:|eval\(/);
});
