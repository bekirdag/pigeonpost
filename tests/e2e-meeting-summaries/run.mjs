#!/usr/bin/env node
// Meeting notes end to end in real headless Chrome, under the inbox's production CSP, with synthetic
// media only (a different tone per person; nothing real is captured):
//
//   Alice  native-call.html (the native apps' call host), the host
//   Bob    index.html (the browser inbox), a member
//   Dana   join.html (a meeting link), a guest
//
// The real calls service and the real meeting-intel code run in this process; real ffmpeg decodes the
// browsers' MediaRecorder chunks. The postbox is an in-memory stand-in, and speech-to-text is a stand-in
// that reports each window's dominant tone instead of words, which proves whose microphone a chunk
// carried. Checks:
//   - nobody is captured until everyone agreed; Bob's decline keeps everyone paused; a late guest pauses
//     capture and is asked; nobody's chunk ever carries anyone else's tone;
//   - uploads cross the calls service only as ciphertext;
//   - the host stops notes; the brief is stored for Alice and Bob only (not the guest) and Bob opens and
//     reads it in the inbox's Meeting notes with his own device key.
//
//   node tests/e2e-meeting-summaries/run.mjs        (CHROME=/path/to/chrome, FFMPEG=ffmpeg)
import http from "node:http";
import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { fileURLToPath } from "node:url";
import { configFromEnv, createCallsService } from "../../calls-service/src/server.js";
import { createIntel } from "../../meeting-intel/src/service.js";
import { createHandler } from "../../meeting-intel/src/server.js";
import { createDecoder, peak, RATE } from "../../meeting-intel/src/audio.js";
import { b64, generateSigningKeyPem, loadSigningKey } from "../../meeting-intel/src/crypto.js";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SITE = path.resolve(HERE, "../../site-inbox");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn, what, ms = 40000) {
  const start = Date.now();
  for (;;) {
    let v;
    try { v = await fn(); } catch (_) { v = false; }
    if (v) return v;
    if (Date.now() - start > ms) throw new Error("timed out waiting for " + what);
    await sleep(200);
  }
}
const failures = [];
const check = (ok, what) => { if (!ok) failures.push(what); console.log((ok ? "ok   " : "FAIL ") + what); };

// ---- people and the stand-in postbox ---------------------------------------------------------------
const PEOPLE = {
  "tok-alice": { address: "/k/alice0000000000000000000a", handle: "/alice", account: "acct_alice" },
  "tok-bob": { address: "/k/bob00000000000000000000000b", handle: "/bob", account: "acct_bob" },
};
const byAddress = new Map(Object.values(PEOPLE).map((p) => [p.address, p]));
const meetings = new Map();
function postboxInternal(pathname, body) {
  if (pathname === "/internal/v1/call-admission") return { decision: "ring", targets: [body.to] };
  if (pathname === "/internal/v1/call-events") return { accepted: true };
  if (pathname === "/internal/v1/meetings") {
    if (!byAddress.has(body.host)) return { allowed: false, reason: "unknown_host" };
    if (body.check_only) return { allowed: true, retention_days: 90 };
    const id = "mtg_" + crypto.randomBytes(13).toString("hex");
    meetings.set(id, { attendees: new Map(), brief: null, ended: null });
    return { allowed: true, meeting_id: id, retention_days: 90 };
  }
  const m = /^\/internal\/v1\/meetings\/(mtg_[a-z0-9]+)\/(attendees|end|recipients|brief)$/.exec(pathname);
  const meeting = m && meetings.get(m[1]);
  if (!meeting) return { __status: 404, error: "not_found" };
  if (m[2] === "attendees") {
    const who = byAddress.get(body.address);
    if (!who) return { __status: 400, error: "unknown_member" };
    const prev = meeting.attendees.get(who.account) || { keys: new Map() };
    prev.consent = body.consent;
    if (body.notes_key) prev.keys.set(body.notes_key.key_id, body.notes_key.public_key);
    prev.address = who.address;
    meeting.attendees.set(who.account, prev);
    return { recorded: true };
  }
  if (m[2] === "end") { meeting.ended = body.reason; return { ended: true }; }
  if (m[2] === "recipients") {
    return { recipients: [...meeting.attendees].filter(([, a]) => a.consent === "yes" || a.consent === "withdrawn").map(([account, a]) => ({ account_id: account, address: a.address, keys: [...a.keys].map(([key_id, public_key]) => ({ key_id, public_key })) })) };
  }
  meeting.brief = body;
  return { stored: true };
}
// The member API the inbox's Meeting notes reader calls (through /__postbox on this origin).
function postboxMember(account, method, pathname) {
  if (pathname === "/v1/meetings") {
    return { meetings: [...meetings].filter(([, x]) => x.attendees.get(account)?.consent && ["yes", "withdrawn"].includes(x.attendees.get(account).consent)).map(([id, x]) => ({ id, kind: "direct", state: x.brief ? (x.brief.status === "ready" ? "ready" : "failed") : "processing", created_at: Math.floor(Date.now() / 1000), expires_at: Math.floor(Date.now() / 1000) + 90 * 86400, host: "/k/alice0000000000000000000a", as: "-" })) };
  }
  const m = /^\/v1\/meetings\/(mtg_[a-z0-9]+)$/.exec(pathname);
  if (m && method === "GET") {
    const x = meetings.get(m[1]);
    const a = x && x.attendees.get(account);
    if (!x || !a || !["yes", "withdrawn"].includes(a.consent)) return { __status: 404, error: "not_found" };
    if (!x.brief || x.brief.status !== "ready") return { id: m[1], state: "processing" };
    return { id: m[1], state: "ready", alg: x.brief.alg, iv: x.brief.iv, ciphertext: x.brief.ciphertext, keys: x.brief.wraps.filter((w) => w.account_id === account) };
  }
  if (/^\/v1\/meetings\/keys\//.test(pathname)) return { registered: true };
  return { __status: 404, error: "not_found" };
}

// ---- meeting-intel with real decoding and a tone-reading stand-in for speech-to-text -----------------
function dominantHz(pcm) {
  let crossings = 0;
  let prev = 0;
  for (let i = 0; i + 1 < pcm.length; i += 2) {
    const v = pcm.readInt16LE(i);
    if (Math.abs(v) < 200) continue;
    if ((v > 0) !== (prev > 0) && prev !== 0) crossings++;
    prev = v;
  }
  return Math.round(crossings / 2 / (pcm.length / 2 / RATE));
}
const windows = []; // { participant, hz, peak }
const signing = loadSigningKey(generateSigningKeyPem());
const intel = createIntel({ sttName: "tone stand-in", llmName: "stand-in" }, {
  signing,
  decode: createDecoder({ ffmpeg: process.env.FFMPEG || "ffmpeg" }),
  transcribe: async (pcm) => {
    const hz = dominantHz(pcm);
    windows.push({ hz, peak: peak(pcm) });
    return { language: "english", segments: [{ start: 0, end: 1, text: `tone ${hz} Hz` }] };
  },
  summarizer: { summarize: async (lines, speakers) => ({ notes: { title: "Synthetic tones", summary: "Three synthetic tones were heard.", decisions: [], action_items: [], open_questions: [] }, draft: false }) },
  postbox: async (method, p, body) => { const out = postboxInternal(p, body); return { status: out.__status || 200, body: out }; },
  log: () => {},
});
const relayed = []; // bodies the calls service sent to meeting-intel
const intelHandler = createHandler({ serviceToken: "intel-token-0123456789abcdef0123456789" }, intel);
const intelServer = http.createServer(intelHandler);
await new Promise((r) => intelServer.listen(0, "127.0.0.1", r));
const INTEL = `http://127.0.0.1:${intelServer.address().port}`;

// ---- the calls service ---------------------------------------------------------------------------------
const log = console.log;
console.log = (...a) => { if (!/^(calls|intel):/.test(String(a[0]))) log(...a); };
console.warn = (...a) => { if (!/^(calls|intel):/.test(String(a[0]))) log(...a); };
async function serviceFetch(url, opts = {}) {
  const u = new URL(String(url));
  const reply = (status, body) => new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
  if (u.host === "postbox-internal.test") {
    const out = postboxInternal(u.pathname, opts.body ? JSON.parse(opts.body) : undefined);
    return reply(out.__status || 200, out);
  }
  if (u.host === "intel.test") {
    relayed.push({ path: u.pathname, body: String(opts.body || "") });
    return fetch(INTEL + u.pathname, opts);
  }
  const auth = (opts.headers && (opts.headers.authorization || opts.headers.Authorization)) || "";
  const who = PEOPLE[auth.replace(/^Bearer /, "")];
  if (!who) return reply(401, { error: "unauthorized" });
  if (u.pathname === "/v1/identities") return reply(200, { identities: [{ address: who.address, label: null }] });
  if (u.pathname === "/v1/whoami") return reply(200, { address: who.address, handle: who.handle });
  return reply(404, {});
}

// ---- the site: production CSP, synthetic media, the pinned key replaced by this run's key -----------
const CSP = "default-src 'self'; img-src 'self' data: blob:; media-src 'self' blob:; style-src 'self'; script-src 'self' 'wasm-unsafe-eval'; worker-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'; object-src 'none'";
const TYPES = { ".html": "text/html", ".css": "text/css", ".js": "application/javascript", ".mjs": "application/javascript", ".png": "image/png", ".woff2": "font/woff2", ".wasm": "application/wasm", ".ico": "image/x-icon", ".json": "application/json" };
const MEDIA = `(function () {
  window.__nativeFetch = window.__nativeFetch || window.fetch.bind(window);
  const who = new URLSearchParams(location.search).get("as") || (location.pathname.length === 7 ? "dana" : "x");
  const HZ = { alice: 440, bob: 660, dana: 880 }[who] || 1000;
  try { localStorage.setItem("ppi_call_noise", "standard"); } catch (_) {}
  navigator.mediaDevices.enumerateDevices = async () => [{ kind: "audioinput", deviceId: "mic-a", label: "Test microphone", groupId: "g", toJSON() { return this; } }];
  navigator.mediaDevices.getUserMedia = async (c) => {
    const out = new MediaStream();
    if (c.audio) {
      const ctx = new AudioContext({ sampleRate: 48000 });
      const osc = ctx.createOscillator(); osc.frequency.value = HZ;
      const gain = ctx.createGain(); gain.gain.value = 0.3;
      const dest = ctx.createMediaStreamDestination();
      osc.connect(gain).connect(dest); osc.start();
      const t = dest.stream.getAudioTracks()[0];
      const s = t.getSettings.bind(t); t.getSettings = () => ({ ...s(), deviceId: "mic-a" });
      out.addTrack(t);
    }
    if (c.video) {
      const canvas = document.createElement("canvas"); canvas.width = 320; canvas.height = 180;
      const g = canvas.getContext("2d"); setInterval(() => { g.fillStyle = "#335"; g.fillRect(0, 0, 320, 180); }, 200);
      out.addTrack(canvas.captureStream(5).getVideoTracks()[0]);
    }
    return out;
  };
})();`;
const NOTES_FIXTURE = `(function () {
  const prev = window.fetch;
  window.fetch = function (url, opts = {}) {
    const u = new URL(String(url), location.href);
    if (u.pathname === "/v1/capabilities") return Promise.resolve(new Response(JSON.stringify({ features: ["meeting_summaries"] }), { status: 200 }));
    if (u.pathname.startsWith("/v1/meetings")) return window.__nativeFetch("/__postbox" + u.pathname, opts);
    return prev(url, opts);
  };
})();`;
let calls;
const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, "http://x");
  let p = decodeURIComponent(url.pathname);
  if (p.startsWith("/calls/")) return calls.handle(req, res);
  if (p.startsWith("/__postbox/")) {
    const who = PEOPLE[String(req.headers.authorization || "").replace(/^Bearer /, "")];
    for await (const _ of req) { /* drain */ }
    const out = who ? postboxMember(who.account, req.method, p.slice("/__postbox".length)) : { __status: 401, error: "unauthorized" };
    res.writeHead(out.__status || 200, { "content-type": "application/json" });
    return res.end(JSON.stringify(out));
  }
  const send = (type, body) => { res.writeHead(200, { "content-type": type, "cache-control": "no-store", "content-security-policy": CSP }); res.end(body); };
  if (p === "/__media.js") return send(TYPES[".js"], MEDIA);
  if (p === "/__notes.js") return send(TYPES[".js"], NOTES_FIXTURE);
  if (p === "/__fixture.js") return send(TYPES[".js"], readFileSync(path.join(SITE, "test/call-layout/fixture.js")));
  if (/^\/[A-Z0-9]{6}$/.test(p)) p = "/join.html";
  const file = path.join(SITE, p === "/" ? "index.html" : p);
  if (!file.startsWith(SITE + path.sep) || !existsSync(file)) { res.writeHead(404); return res.end(); }
  let body = readFileSync(file);
  if (p === "/" || p === "/index.html") {
    body = String(body).replace(/<script src="config\.js(?:\?[^"]*)?"><\/script>/, (tag) => '<script src="/__media.js"></script><script src="/__fixture.js"></script><script src="/__notes.js"></script>' + tag);
  } else if (p === "/native-call.html" || p === "/join.html") {
    body = String(body).replace(/<body([^>]*)>/, '<body$1><script src="/__media.js"></script>');
  } else if (p === "/calls-summary.js") {
    body = String(body).replace(/const PINNED_KEYS = \[[^\]]*\];/, `const PINNED_KEYS = [${JSON.stringify(b64(signing.publicRaw))}];`);
  }
  send(TYPES[path.extname(file)] || "application/octet-stream", body);
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const ORIGIN = `http://127.0.0.1:${server.address().port}`;
calls = createCallsService({ ...configFromEnv({ CALLS_POSTBOX_URL: "http://postbox.test", CALLS_ALLOWED_ORIGINS: ORIGIN, CALLS_TRUST_PROXY: "0" }),
  postboxInternalUrl: "http://postbox-internal.test", postboxServiceToken: "svc_calls_" + "0".repeat(32),
  intelUrl: "http://intel.test", intelToken: "intel-token-0123456789abcdef0123456789" }, { fetch: serviceFetch });

// ---- Chrome over the DevTools protocol -----------------------------------------------------------------
function findChrome() {
  return [process.env.CHROME, "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", "/usr/bin/google-chrome", "/usr/bin/google-chrome-stable", "/usr/bin/chromium"].find((c) => c && existsSync(c));
}
async function launchChrome() {
  const chrome = findChrome();
  if (!chrome) throw new Error("no Chrome found; set CHROME");
  const dir = mkdtempSync(path.join(os.tmpdir(), "notes-e2e-"));
  const proc = spawn(chrome, ["--headless=new", "--remote-debugging-port=0", `--user-data-dir=${dir}`, "--no-first-run", "--no-default-browser-check",
    "--use-fake-ui-for-media-stream", "--autoplay-policy=no-user-gesture-required", "--disable-gpu", ...(process.platform === "linux" ? ["--no-sandbox"] : []), "about:blank"], { stdio: ["ignore", "ignore", "pipe"] });
  const wsUrl = await new Promise((resolve, reject) => {
    let err = "";
    proc.stderr.on("data", (d) => { err += d; const m = /DevTools listening on (ws:\S+)/.exec(err); if (m) resolve(m[1]); });
    proc.on("exit", (code) => reject(new Error("Chrome exited " + code)));
  });
  const ws = new WebSocket(wsUrl);
  await new Promise((r, j) => { ws.addEventListener("open", r); ws.addEventListener("error", j); });
  let id = 0; const pending = new Map(); const consoleErrors = [];
  ws.addEventListener("message", (e) => {
    const m = JSON.parse(e.data);
    if (m.id && pending.has(m.id)) { const { res, rej } = pending.get(m.id); pending.delete(m.id); if (m.error) rej(new Error(JSON.stringify(m.error))); else res(m.result); }
    if (m.method === "Runtime.exceptionThrown") consoleErrors.push(JSON.stringify(m.params.exceptionDetails).slice(0, 300));
    if (m.method === "Log.entryAdded" && /Content Security Policy|Refused/.test(m.params.entry.text)) consoleErrors.push(m.params.entry.text.slice(0, 300));
  });
  const sendCmd = (method, params = {}, sessionId) => new Promise((res, rej) => { const i = ++id; pending.set(i, { res, rej }); ws.send(JSON.stringify({ id: i, method, params, ...(sessionId ? { sessionId } : {}) })); });
  async function page(url) {
    const { browserContextId } = await sendCmd("Target.createBrowserContext");
    const { targetId } = await sendCmd("Target.createTarget", { url: "about:blank", browserContextId });
    const { sessionId } = await sendCmd("Target.attachToTarget", { targetId, flatten: true });
    const s = (m, p) => sendCmd(m, p, sessionId);
    await s("Page.enable"); await s("Runtime.enable"); await s("Log.enable");
    await s("Emulation.setDeviceMetricsOverride", { width: 1280, height: 860, deviceScaleFactor: 1, mobile: false });
    await s("Page.navigate", { url });
    const evaluate = async (expr) => {
      const r = await s("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true });
      if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails).slice(0, 400));
      return r.result.value;
    };
    return { evaluate };
  }
  return { page, consoleErrors, close: () => { try { ws.close(); } catch (_) {} proc.kill(); } };
}

const NOTES = `(window.PigeonpostCallsSummary && PigeonpostCallsSummary.inspect()) || {}`;
let browser;
try {
  browser = await launchChrome();
  const alice = await browser.page(`${ORIGIN}/native-call.html?as=alice`);
  const bob = await browser.page(`${ORIGIN}/?as=bob`);
  await until(() => alice.evaluate(`Boolean(window.PigeonpostNative && window.PigeonpostCalls && window.PigeonpostCallsSummary)`), "Alice's call page");
  await until(() => bob.evaluate(`Boolean(window.PigeonpostCalls && PigeonpostCalls.inspect().session)`), "Bob's inbox");
  await alice.evaluate(`PigeonpostNative.configure({ token: "tok-alice", mailbox: { address: "/k/alice0000000000000000000a", handle: "/alice" }, known: { "/bob": "/bob" } })`);
  await until(() => alice.evaluate(`Boolean(PigeonpostCalls.inspect().session)`), "Alice to sign in to calls");
  await alice.evaluate(`PigeonpostNative.call("/bob", "audio")`);
  await until(() => bob.evaluate(`!document.getElementById("call-ring").hidden`), "Bob's ring");
  await bob.evaluate(`document.getElementById("call-ring-accept").click()`);
  await until(async () => (await alice.evaluate(`(PigeonpostCalls.inspect().peers || []).some((x) => x.connected)`)), "the call to connect");

  // The host is offered notes; Bob is not.
  await until(() => alice.evaluate(`!document.getElementById("call-notes-btn").hidden`), "Alice's Notes button");
  check(await bob.evaluate(`!document.getElementById("call-notes-btn") || document.getElementById("call-notes-btn").hidden`), "only the host is offered notes");
  await alice.evaluate(`document.getElementById("call-notes-btn").click()`);
  await until(() => alice.evaluate(`!document.getElementById("call-notes-ask").hidden`), "the host's request sheet");
  await alice.evaluate(`document.getElementById("call-notes-ask-go").click()`);
  for (const [name, p] of [["Alice", alice], ["Bob", bob]]) {
    await until(() => p.evaluate(`!document.getElementById("call-notes-sheet").hidden`), name + "'s consent sheet");
    await until(() => p.evaluate(`!document.getElementById("call-notes-banner").hidden`), name + "'s notes indicator");
  }
  check(await bob.evaluate(`/asked to take meeting notes/.test(document.getElementById("call-notes-title").textContent) && /Only your own microphone/.test(document.getElementById("call-notes-list").textContent)`), "the consent sheet discloses what is captured");

  // Alice agrees, Bob declines: still paused, nobody captured.
  await alice.evaluate(`document.getElementById("call-notes-agree").click()`);
  await bob.evaluate(`document.getElementById("call-notes-decline").click()`);
  await sleep(12000);
  check((await alice.evaluate(NOTES)).state === "waiting" && !(await alice.evaluate(NOTES)).capturing && !(await bob.evaluate(NOTES)).capturing, "a decline keeps capture paused for everyone");
  check(relayed.filter((r) => r.path.endsWith("/chunks")).length === 0, "no audio was uploaded before everyone agreed");

  // Bob changes his mind: capture runs on both.
  await bob.evaluate(`document.getElementById("call-notes-review").click()`);
  await until(() => bob.evaluate(`!document.getElementById("call-notes-sheet").hidden`), "Bob's sheet again");
  await bob.evaluate(`document.getElementById("call-notes-agree").click()`);
  await until(async () => (await alice.evaluate(NOTES)).capturing && (await bob.evaluate(NOTES)).capturing, "both to capture");
  check(/being captured/.test(await bob.evaluate(`document.getElementById("call-notes-text").textContent`)), "the indicator says the microphone is captured");
  await until(() => relayed.filter((r) => r.path.endsWith("/chunks")).length >= 2, "chunks from both", 30000);

  // A guest joins by the meeting link: capture pauses before they join and they are asked.
  const code = await alice.evaluate(`PigeonpostCalls.inspect().room && PigeonpostCalls.inspect().room.code`);
  const dana = await browser.page(`${ORIGIN}/${code}`);
  await until(() => dana.evaluate(`!document.getElementById("join-form").hidden`), "the join form");
  await dana.evaluate(`(() => { const n = document.getElementById("join-name"); n.value = "Dana"; n.dispatchEvent(new Event("input", { bubbles: true })); document.getElementById("join-submit").disabled = false; document.getElementById("join-form").requestSubmit(); })()`);
  await until(() => alice.evaluate(`Boolean(document.querySelector("#call-knocks .call-pill-primary"))`), "Dana's knock");
  const beforeJoin = relayed.filter((r) => r.path.endsWith("/chunks")).length;
  await alice.evaluate(`document.querySelector("#call-knocks .call-pill-primary").click()`);
  await until(() => dana.evaluate(`!document.getElementById("call-notes-sheet")?.hidden`), "Dana's consent sheet");
  check((await alice.evaluate(NOTES)).state === "waiting", "a newcomer pauses capture");
  check(/guest: you can agree, but you won't receive a copy/.test(await dana.evaluate(`document.getElementById("call-notes-warn").textContent`)), "the guest is told they get no copy");
  await dana.evaluate(`document.getElementById("call-notes-agree").click()`);
  await until(async () => (await dana.evaluate(NOTES)).capturing && (await alice.evaluate(NOTES)).capturing, "capture to resume with the guest");
  await sleep(13000);
  check(relayed.filter((r) => r.path.endsWith("/chunks")).length > beforeJoin, "chunks after the guest agreed");

  // Bob stops for himself, then agrees again; the host stops notes.
  await bob.evaluate(`document.getElementById("call-notes-stopme").click()`);
  await until(async () => !(await bob.evaluate(NOTES)).capturing && (await alice.evaluate(NOTES)).state === "waiting", "Stop for me to pause everyone");
  await alice.evaluate(`document.getElementById("call-notes-off").click()`);
  await until(() => [...meetings.values()][0].brief, "the brief", 60000);

  // What crossed the calls service: ciphertext only. What meeting-intel heard: each window one tone.
  const chunkBodies = relayed.filter((r) => r.path.endsWith("/chunks")).map((r) => JSON.parse(r.body));
  check(chunkBodies.length >= 4 && chunkBodies.every((c) => typeof c.envelope.ct === "string" && !("audio" in c)), `${chunkBodies.length} chunks relayed as ciphertext`);
  const speakersSeen = new Set(chunkBodies.map((c) => c.speaker));
  check(speakersSeen.has("/alice") && speakersSeen.has("/bob") && speakersSeen.has("Guest Dana"), "chunks from all three, labelled by their own identity: " + [...speakersSeen].join(", "));
  const tones = windows.filter((w) => w.peak > 0.01).map((w) => w.hz);
  const near = (hz, f) => Math.abs(hz - f) < f * 0.06;
  check(tones.length >= 2 && tones.every((hz) => near(hz, 440) || near(hz, 660) || near(hz, 880)), "every window carried exactly one person's tone: " + tones.join(", "));
  // Bob's audio was still waiting for transcription when he chose Stop for me: it was discarded.
  check(!tones.some((hz) => near(hz, 660)), "Stop for me discarded Bob's untranscribed audio");
  const brief = [...meetings.values()][0].brief;
  const accounts = new Set(brief.wraps.map((w) => w.account_id));
  check(brief.status === "ready" && accounts.has("acct_alice") && accounts.has("acct_bob") && accounts.size === 2, "the brief is wrapped for Alice and Bob only");

  // Bob reads it in the inbox with his own device key.
  await until(() => bob.evaluate(`!document.getElementById("notes-btn")?.hidden`), "Bob's Meeting notes button");
  await bob.evaluate(`document.getElementById("notes-btn").click()`);
  await until(() => bob.evaluate(`(() => { const b = document.querySelector(".notes-list button"); if (b) b.click(); return Boolean(b); })()`), "the notes list");
  await until(() => bob.evaluate(`/Three synthetic tones were heard/.test(document.getElementById("notes-body").textContent)`), "Bob's decrypted brief");
  const text = await bob.evaluate(`document.getElementById("notes-body").textContent`);
  check(/\/alice:tone 4[0-9]{2} Hz/.test(text) && /Guest Dana:tone 8[0-9]{2} Hz/.test(text), "the transcript shows each speaker's own tone");
  check(browser.consoleErrors.length === 0, "no page errors or CSP refusals" + (browser.consoleErrors.length ? ": " + browser.consoleErrors.join(" | ") : ""));
  await bob.evaluate(`document.getElementById("call-hangup")?.click()`);
} catch (error) {
  failures.push("harness: " + (error && error.stack || error));
} finally {
  browser?.close();
  server.close();
  intelServer.close();
}
if (failures.length) { log("\n" + failures.length + " problem(s):\n  " + failures.join("\n  ")); process.exit(1); }
log("meeting notes end to end: OK");
process.exit(0);
