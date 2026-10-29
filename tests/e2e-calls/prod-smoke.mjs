#!/usr/bin/env node
// A synthetic end-to-end check of calling on a live deployment (production by default): throwaway
// accounts call each other in headless Chrome with synthetic media — never a real camera or
// microphone — and every check asserts that audio and video actually arrive, not only that the
// HTTP calls returned 200.
//
//   one-to-one audio   the callee answers on the page's own ring screen
//   one-to-one video   the callee answers the way the iPhone app does after CallKit: nativeRing,
//                      PigeonpostNative.answer(room), on a page that loads only after the ring and
//                      whose AudioContext.resume() never settles (WebKit with audio not allowed).
//                      This is the 2026-10-29 outage: the answer was never sent.
//   group (mesh)       four people: a call plus two guests through the meeting link, admitted
//   group (SFU)        seven people: the seventh moves the call to the media server
//
// The two accounts take turns calling: the postbox silences more than 3 calls from one mailbox to
// another in 2 minutes (10 an hour), so run it at most a few times an hour with the same ACCOUNTS.
//
// Accounts: two proof-of-work accounts with one mailbox each are created and allow each other as
// contacts (an unknown caller is silenced by admission and nothing rings); at the end their
// mailboxes are deleted and their API keys revoked. ACCOUNTS=file.json reuses
// [{api_key, address}, …] instead and leaves them alone.
//
//   node tests/e2e-calls/prod-smoke.mjs                    # all scenarios against production
//   ONLY=video node tests/e2e-calls/prod-smoke.mjs         # audio | video | mesh | sfu (comma list)
//   ORIGIN=https://inbox.example POSTBOX=https://postbox.example node tests/e2e-calls/prod-smoke.mjs
//   RELAY=1 node tests/e2e-calls/prod-smoke.mjs           # every mesh connection through the TURN relay
//   CHROME=/path/to/chrome   KEEP=1 (keep the accounts)   VERBOSE=1 (page console)
//
// Exits 1 on the first failed scenario, after cleaning up.
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";

const ORIGIN = process.env.ORIGIN || "https://inbox.pigeonpost.dev";
const POSTBOX = process.env.POSTBOX || "https://postbox.pigeonpost.dev";
const ONLY = new Set((process.env.ONLY || "audio,video,mesh,sfu").split(",").map((s) => s.trim()));
const VERBOSE = process.env.VERBOSE === "1";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const t0 = Date.now();
const log = (...a) => console.log(((Date.now() - t0) / 1000).toFixed(1).padStart(6), ...a);

// ---- accounts -----------------------------------------------------------------------------------
async function pb(method, p, key, body) {
  const res = await fetch(POSTBOX + p, { method, headers: { ...(key ? { authorization: "Bearer " + key } : {}), ...(body ? { "content-type": "application/json" } : {}) }, body: body ? JSON.stringify(body) : undefined });
  const text = await res.text();
  let json = null; try { json = JSON.parse(text); } catch (_) { /* not json */ }
  if (!res.ok) throw new Error(`${method} ${p} → ${res.status} ${(json && json.error) || text.slice(0, 120)}`);
  return json;
}
function zeroBits(buf) { let n = 0; for (const b of buf) { if (b === 0) { n += 8; continue; } return n + Math.clz32(b) - 24; } return n; }
async function proofOfWork() {
  const { challenge, bits } = await pb("GET", "/v1/pow/challenge");
  for (let i = 0; ; i++) if (zeroBits(createHash("sha256").update(challenge + ":" + i).digest()) >= bits) return { pow_challenge: challenge, pow_solution: String(i) };
}
async function makeAccounts() {
  const out = [];
  for (let i = 0; i < 2; i++) {
    const { api_key } = await pb("POST", "/v1/accounts", null, await proofOfWork());
    const { address } = await pb("POST", "/v1/identities", api_key, { label: "call-smoke-" + i });
    out.push({ api_key, address, created: true });
  }
  for (const me of out) for (const other of out) if (me !== other) await pb("PUT", "/v1/contacts", me.api_key, { identity: me.address, peer: other.address, admission: "allow" });
  return out;
}
async function removeAccounts(accounts) {
  for (const a of accounts.filter((x) => x.created)) {
    try { await pb("DELETE", "/v1/identities?identity=" + encodeURIComponent(a.address), a.api_key); } catch (e) { log("cleanup: mailbox", e.message); }
    try {
      const { keys } = await pb("GET", "/v1/api-keys", a.api_key);
      for (const k of keys || []) await pb("DELETE", "/v1/api-keys/" + encodeURIComponent(k.id), a.api_key);
    } catch (e) { log("cleanup: keys", e.message); }
  }
}

// ---- headless Chrome over the DevTools protocol --------------------------------------------------
// Synthetic camera (a moving canvas) and microphone (a tone); every RTCPeerConnection is recorded so
// the checks can read what actually arrived.
const MEDIA = `(() => {
  const synth = (w, h) => { const c = document.createElement("canvas"); c.width = w; c.height = h; const x = c.getContext("2d"); let f = 0;
    setInterval(() => { f++; x.fillStyle = "hsl(" + (f * 3) % 360 + ",60%,40%)"; x.fillRect(0, 0, w, h); x.fillStyle = "#fff"; x.font = "40px sans-serif"; x.fillText("smoke " + f, 40, h / 2); }, 33);
    return c.captureStream(30).getVideoTracks()[0]; };
  const tone = () => { const ac = new AudioContext(); const o = ac.createOscillator(); const d = ac.createMediaStreamDestination(); o.connect(d); o.start(); return d.stream.getAudioTracks()[0]; };
  const md = navigator.mediaDevices;
  md.getUserMedia = async (c) => new MediaStream([...(c && c.audio ? [tone()] : []), ...(c && c.video ? [synth(640, 360)] : [])]);
  md.enumerateDevices = async () => [];
  const PC = window.RTCPeerConnection; window.__pcs = [];
  window.RTCPeerConnection = class extends PC { constructor(...a) { super(...a); window.__pcs.push(this); } };
  // What arrives on each live connection: one per other person in a mesh, the subscriber's on the media server.
  window.__received = async () => { const out = [];
    for (const pc of window.__pcs) { if (pc.connectionState !== "connected") continue;
      const c = { audio: 0, video: 0, frames: 0 };
      (await pc.getStats()).forEach((r) => { if (r.type !== "inbound-rtp") return; if (r.kind === "audio") c.audio += r.bytesReceived || 0; else { c.video += r.bytesReceived || 0; c.frames += r.framesDecoded || 0; } });
      out.push(c); }
    return out; };
})();`;
// RELAY=1: every connection through the TURN relay (the calls client's own switch), so the relay is
// exercised although both ends run on one machine.
const RELAY = process.env.RELAY === "1" ? `try { localStorage.setItem("ppi_calls_relay", "1"); } catch (_) {}` : "";
const NATIVE_HOST = `window.webkit = { messageHandlers: { pigeonpost: { postMessage: (m) => { (window.__host ||= []).push(m); } } } };`;
const STALLED_AUDIO = `AudioContext.prototype.resume = function () { return new Promise(() => {}); };`;

function findChrome() {
  return [process.env.CHROME, "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/usr/bin/google-chrome", "/usr/bin/google-chrome-stable", "/usr/bin/chromium", "/usr/bin/chromium-browser"].find((c) => c && existsSync(c));
}
async function launchChrome() {
  const chrome = findChrome();
  if (!chrome) throw new Error("no Chrome found; set CHROME=/path/to/chrome");
  const dir = mkdtempSync(path.join(os.tmpdir(), "call-smoke-"));
  const proc = spawn(chrome, ["--headless=new", "--remote-debugging-port=0", `--user-data-dir=${dir}`, "--no-first-run", "--no-default-browser-check",
    "--use-fake-ui-for-media-stream", "--autoplay-policy=no-user-gesture-required", "--disable-quic", "--mute-audio",
    ...(process.platform === "linux" ? ["--no-sandbox"] : []), "about:blank"], { stdio: ["ignore", "ignore", "pipe"] });
  const wsUrl = await new Promise((resolve, reject) => {
    let err = "";
    proc.stderr.on("data", (d) => { err += d; const m = /DevTools listening on (ws:\S+)/.exec(err); if (m) resolve(m[1]); });
    proc.on("exit", (code) => reject(new Error("Chrome exited " + code + ": " + err.slice(-400))));
  });
  const ws = new WebSocket(wsUrl);
  await new Promise((r, j) => { ws.addEventListener("open", r); ws.addEventListener("error", j); });
  let id = 0; const pending = new Map(), listeners = new Map();
  ws.addEventListener("message", (e) => {
    const m = JSON.parse(e.data);
    if (m.id && pending.has(m.id)) { const { res, rej } = pending.get(m.id); pending.delete(m.id); if (m.error) rej(new Error(JSON.stringify(m.error))); else res(m.result); }
    else if (m.method && m.sessionId && listeners.has(m.sessionId)) listeners.get(m.sessionId)(m.method, m.params);
  });
  const send = (method, params = {}, sessionId) => new Promise((res, rej) => {
    const i = ++id; pending.set(i, { res, rej });
    ws.send(JSON.stringify({ id: i, method, params, ...(sessionId ? { sessionId } : {}) }));
  });
  const contexts = [], people = [];
  // Each person in a browser context of their own: separate storage, as separate devices.
  async function person(name, url, { init = "" } = {}) {
    const { browserContextId } = await send("Target.createBrowserContext");
    contexts.push(browserContextId);
    const { targetId } = await send("Target.createTarget", { url: "about:blank", browserContextId });
    const { sessionId } = await send("Target.attachToTarget", { targetId, flatten: true });
    const s = (m, p) => send(m, p, sessionId);
    const errors = [];
    listeners.set(sessionId, (method, p) => {
      if (method === "Runtime.exceptionThrown") errors.push(p.exceptionDetails?.exception?.description || p.exceptionDetails?.text);
      if (VERBOSE && method === "Runtime.consoleAPICalled") log(name, "console", p.type, (p.args || []).map((a) => a.value ?? a.description).join(" ").slice(0, 200));
    });
    await s("Page.enable"); await s("Runtime.enable");
    await s("Emulation.setDeviceMetricsOverride", { width: 900, height: 700, deviceScaleFactor: 1, mobile: false });
    await s("Page.addScriptToEvaluateOnNewDocument", { source: MEDIA + RELAY + init });
    const evaluate = async (expr) => {
      const r = await s("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true });
      if (r.exceptionDetails) throw new Error(name + ": " + (r.exceptionDetails.exception?.description || r.exceptionDetails.text).slice(0, 300));
      return r.result.value;
    };
    const goto = async (u) => {
      await s("Page.navigate", { url: u });
      for (let i = 0; i < 120; i++) { await sleep(250); try { if (await evaluate("document.readyState") === "complete") return; } catch (_) { /* navigating */ } }
      throw new Error(name + ": " + u + " did not load");
    };
    if (url) await goto(url);
    const p = { name, evaluate, goto, errors };
    people.push(p);
    return p;
  }
  return {
    person,
    // Everyone still on a page leaves their call: a participant who just vanishes is held for the
    // resume grace, and their mailbox counts as busy (no ring) until it ends.
    hangUpAll: async () => { await hangUpAll(people.splice(0)); },
    close: async () => { for (const c of contexts) await send("Target.disposeBrowserContext", { browserContextId: c }).catch(() => {}); try { ws.close(); } catch (_) {} proc.kill(); try { rmSync(dir, { recursive: true, force: true }); } catch (_) {} },
  };
}

// ---- the call checks -------------------------------------------------------------------------------
// Waits for fn() to return something truthy; fn may throw to report why it is not there yet.
const not = (detail) => { throw new Error(typeof detail === "string" ? detail : JSON.stringify(detail)); };
async function until(what, fn, ms = 20000) {
  const end = Date.now() + ms; let last;
  while (Date.now() < end) { try { last = await fn(); if (last) return last; } catch (e) { last = e.message; } await sleep(300); }
  throw new Error(what + " (last: " + JSON.stringify(last)?.slice(0, 200) + ")");
}
const configure = (p, acct, nativeRing = false) => p.evaluate(`window.PigeonpostNative.configure(${JSON.stringify({ token: acct.api_key, mailbox: { address: acct.address }, known: {}, nativeRing })}); true`);
const inspect = (p) => p.evaluate("JSON.parse(JSON.stringify(window.PigeonpostCalls.inspect()))");
async function member(browser, name, acct, opts = {}) {
  const p = await browser.person(name, ORIGIN + "/native-call.html?smoke=" + Date.now(), opts);
  await configure(p, acct, opts.nativeRing === true);
  await until(name + " has a calls session", async () => (await inspect(p)).session);
  return p;
}
// Media must keep arriving: received bytes grow over three seconds. In a mesh (`mesh` = the number
// of people) each person must first hold a connection to every other one, and each connection must
// carry the other person's audio (and video); on the media server the totals must grow.
async function flowing(people, opts) {
  // A connection that has only just come up (a newcomer, a subscriber on the media server) can take
  // a few seconds to carry anything: allow several windows before calling it broken.
  for (let attempt = 1; ; attempt++) {
    try { return await flowingOnce(people, opts); } catch (e) { if (attempt >= 5) throw e; log("  (retrying:", e.message + ")"); }
  }
}
async function flowingOnce(people, { video, mesh = 0 }) {
  if (mesh) for (const p of people) await until(p.name + " is connected to all " + (mesh - 1) + " others", async () => (await p.evaluate("window.__received()")).length >= mesh - 1 || not(await p.evaluate("window.__pcs.map((pc) => pc.connectionState + '/' + pc.iceConnectionState)")), 30000);
  const first = await Promise.all(people.map((p) => p.evaluate("window.__received()")));
  await sleep(3000);
  const second = await Promise.all(people.map((p) => p.evaluate("window.__received()")));
  if (VERBOSE) people.forEach((p, i) => log("  ", p.name, JSON.stringify(first[i]), "→", JSON.stringify(second[i])));
  people.forEach((p, i) => {
    const sum = (list, k) => list.reduce((n, c) => n + c[k], 0);
    const links = mesh ? second[i].map((c, j) => [c, first[i][j] || { audio: 0, video: 0, frames: 0 }]) : [[
      { audio: sum(second[i], "audio"), video: sum(second[i], "video"), frames: sum(second[i], "frames") },
      { audio: sum(first[i], "audio"), video: sum(first[i], "video"), frames: sum(first[i], "frames") }]];
    if (!links.length) throw new Error(p.name + " has no live connection");
    links.forEach(([now, before], j) => {
      const who = mesh ? " from connection " + (j + 1) : "";
      if (now.audio <= before.audio) throw new Error(p.name + " receives no audio" + who);
      if (video && (now.video <= before.video || now.frames <= before.frames)) throw new Error(p.name + " receives no video" + who);
    });
    const a = links.reduce((n, [x, y]) => n + x.audio - y.audio, 0), v = links.reduce((n, [x, y]) => n + x.video - y.video, 0);
    log("  ", p.name, "receiving on " + links.length + " connection(s): audio +" + a + " B" + (video ? ", video +" + v + " B" : ""));
  });
}
async function hangUpAll(people) {
  for (const p of people) await p.evaluate("window.PigeonpostNative?.hangUp?.(); window.PigeonpostCalls?.hangUp?.(); true").catch(() => {});
  await sleep(1500);
}
async function call(a, b, A, B, kind, how) {
  await a.evaluate(`window.PigeonpostNative.call(${JSON.stringify(B.address)}, ${JSON.stringify(kind)}); true`);
  const room = await until("the caller is ringing", async () => { const r = (await inspect(a)).room; return r && r.id; });
  if (how === "ring") {
    await until("the callee sees the ring", () => b.evaluate("!document.getElementById('call-ring').hidden"));
    await b.evaluate("document.getElementById('call-ring-accept').click(); true");
  } else {
    await b.evaluate(`window.PigeonpostNative.answer(${JSON.stringify(room)}, null); true`);
  }
  for (const p of [a, b]) await until(p.name + " is in an active call", async () => (await inspect(p)).call?.state === "active", 25000);
  return room;
}
// Guests come in through the meeting link and someone inside admits each of them.
async function guestsJoin(browser, host, code, names) {
  const guests = [];
  for (const name of names) {
    const g = await browser.person(name, ORIGIN + "/" + code);
    await until(name + " sees the join form", () => g.evaluate("!!document.getElementById('join-name') && !document.getElementById('join-name-row').hidden && !document.getElementById('join-submit').disabled"));
    await g.evaluate(`(() => { const i = document.getElementById('join-name'); i.value = ${JSON.stringify(name)}; i.dispatchEvent(new Event('input', { bubbles: true })); document.getElementById('join-submit').click(); return true; })()`);
    const joinState = () => g.evaluate("({ waiting: !document.getElementById('join-waiting').hidden, error: document.getElementById('join-error').textContent, done: document.getElementById('join-done').hidden ? '' : document.getElementById('join-done-text').textContent })");
    if (VERBOSE) for (let i = 0; i < 4; i++) { await sleep(1000); log("  ", name, JSON.stringify(await joinState())); }
    // The knock list re-renders as knocks change: find and press "Admit" in one step.
    await until(host.name + " admits " + name, async () => (await host.evaluate("(() => { const b = document.querySelector('#call-knocks .call-pill-primary'); if (!b) return false; b.click(); return true; })()")) || not({ guest: await joinState() }));
    await until(name + " is in the call", async () => { const i = await inspect(g); return i.call?.state === "active" || not({ call: i.call, room: i.room, join: await joinState() }); }, 30000);
    guests.push(g);
  }
  return guests;
}

const scenarios = {
  async audio(browser, [A, B]) {
    const a = await member(browser, "caller", A), b = await member(browser, "callee", B);
    await call(a, b, A, B, "audio", "ring");
    await flowing([a, b], { video: false, mesh: 2 });
    await hangUpAll([a, b]);
  },
  async video(browser, [A, B]) {
    const a = await member(browser, "caller", A, { init: NATIVE_HOST, nativeRing: true });
    await a.evaluate(`window.PigeonpostNative.call(${JSON.stringify(B.address)}, "video"); true`);
    const room = await until("the caller is ringing", async () => { const r = (await inspect(a)).room; return r && r.id; });
    // The callee's app is woken by the push and only now loads the page; WebKit will not start audio.
    const b = await member(browser, "callee (CallKit, audio not allowed)", B, { init: NATIVE_HOST + STALLED_AUDIO, nativeRing: true });
    await b.evaluate(`window.PigeonpostNative.answer(${JSON.stringify(room)}, null); true`);
    for (const p of [a, b]) await until(p.name + " is in an active call", async () => (await inspect(p)).call?.state === "active", 25000);
    await flowing([a, b], { video: true, mesh: 2 });
    await hangUpAll([a, b]);
  },
  async mesh(browser, [A, B]) {
    const a = await member(browser, "host", A), b = await member(browser, "member", B);
    await call(a, b, A, B, "video", "ring");
    const { code } = (await inspect(a)).room;
    const guests = await guestsJoin(browser, a, code, ["Guest One", "Guest Two"]);
    const everyone = [a, b, ...guests];
    await until("four people in a mesh", async () => { const r = (await inspect(a)).room; return (r.size === 4 && r.topology === "mesh") || not(r); });
    await flowing(everyone, { video: true, mesh: 4 });
    await hangUpAll(everyone);
  },
  async sfu(browser, [A, B]) {
    const a = await member(browser, "host", A), b = await member(browser, "member", B);
    await call(a, b, A, B, "video", "ring");
    const { code } = (await inspect(a)).room;
    const guests = await guestsJoin(browser, a, code, ["Guest 3", "Guest 4", "Guest 5", "Guest 6", "Guest 7"]);
    const everyone = [a, b, ...guests];
    for (const p of everyone) await until(p.name + " is on the media server", async () => (await inspect(p)).room?.topology === "sfu", 30000);
    await sleep(8000);
    await flowing(everyone, { video: true });
    await hangUpAll(everyone);
  },
};

let accounts = [], failed = false;
const browser = await launchChrome();
try {
  accounts = process.env.ACCOUNTS ? JSON.parse(readFileSync(process.env.ACCOUNTS, "utf8")) : await makeAccounts();
  log("accounts", accounts.map((a) => a.address).join(", "));
  let turn = 0;
  for (const [name, run] of Object.entries(scenarios)) {
    if (!ONLY.has(name)) continue;
    // The postbox silences a caller after 3 calls to the same callee in 2 minutes (10 an hour):
    // take turns, so a full run places two calls each way.
    const pair = turn++ % 2 ? [accounts[1], accounts[0]] : accounts;
    log("▶", name);
    try { await run(browser, pair); } finally { await browser.hangUpAll(); }
    log("✔", name);
  }
} catch (e) {
  failed = true;
  log("✖", e.message);
} finally {
  await browser.close();
  if (process.env.KEEP !== "1") await removeAccounts(accounts);
}
process.exit(failed ? 1 : 0);
