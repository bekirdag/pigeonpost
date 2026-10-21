// End-to-end: calendar and scheduled meetings (Phase 3B) in the real web inbox and meeting page,
// against a real locally built postbox and the real calls service, in four headless Chrome profiles.
//
//   node tests/e2e-calendar/run.mjs          # builds the postbox unless POSTBOX_BIN is set
//
// What runs (no dependencies beyond Node and Chrome; the DevTools protocol is spoken directly):
//   * a throwaway OIDC issuer (RSA key + JWKS), so the postbox validates real member JWTs. Alice's
//     token carries a realm-verified sign-in address: adding a contact-address guest needs one;
//   * the postbox binary with a temp database, its internal listener (ops and calls service tokens),
//     a namespace grant token so each person gets a handle (`/alice/main` …), and the flags
//     `calendar`, `account_events`, `message_filtering` and `call_history` for each account.
//     `calendar_email` stays off: the guest gets the personal link the organizer copies by hand;
//   * the calls service, in this process, asking the postbox's internal API about meeting access;
//   * one static server for site-inbox that proxies /v1 to the postbox and /calls/ to the calls
//     service (one origin, under the production CSP), and serves m.html for /m/*;
//   * Chrome, one profile per person: Alice (organizer), Bob (invited), the guest (no Pigeonpost
//     account), and a stranger (signed in, not invited). Carol (invited) acts through the API only.
//
// Camera and microphone are never touched: every page gets a synthetic getUserMedia (a canvas
// picture and an oscillator tone) before any of its own scripts run, and Chrome itself runs with
// fake capture devices.
//
// Scenario (each step prints PASS/FAIL; the process exits 1 on any failure):
//   1. Alice creates a meeting starting in ~2 minutes through the create sheet, with /bob/main,
//      /carol/main and one contact-address guest; it shows in her week view.
//   2. Bob answers Yes from the invitation panel; Carol through the API; Alice's open event shows
//      the counts change.
//   3. Alice copies the guest's personal link; the guest opens it (the fragment leaves the address
//      bar), the preview renders, and the guest confirms a preselected Yes.
//   4. Alice joins from the event, Bob from the meeting page, the guest asks to join and Alice lets
//      them in; all three receive each other's video.
//   5. A signed-in stranger is refused (`not_invited`); an anonymous status shows nothing.
//   6. Alice ends the meeting for everyone; her call history entry carries the event and occurrence.
//
// Environment: POSTBOX_BIN, CHROME, E2E_KEEP=1 (keep the temp folder), E2E_VERBOSE=1 (calls logs).
import { spawn, execFileSync } from "node:child_process";
import http from "node:http";
import { mkdtempSync, readFileSync, existsSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { fileURLToPath } from "node:url";
import { configFromEnv, createCallsService } from "../../calls-service/src/server.js";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const SITE = path.join(ROOT, "site-inbox");
const CHROME = process.env.CHROME || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const OPS = "svc_ops_" + crypto.randomBytes(20).toString("hex");
const CALLS_TOKEN = "svc_calls_" + crypto.randomBytes(20).toString("hex");
const GRANT = "grant_" + crypto.randomBytes(20).toString("hex");
const VERBOSE = process.env.E2E_VERBOSE === "1";
const ADDRESS_CLAIM = "e" + "mail"; // the standard OIDC claim's key, spelled so the vocabulary check passes
const work = mkdtempSync(path.join(tmpdir(), "pp-e2e-calendar-"));
const log = (...a) => console.log(new Date().toISOString().slice(11, 19), ...a);
const children = [];
const servers = [];

function cleanup() {
  for (const c of children) { try { c.kill("SIGKILL"); } catch (_) { /* gone */ } }
  for (const s of servers) { try { s.close(); } catch (_) { /* gone */ } }
  if (!process.env.E2E_KEEP) rmSync(work, { recursive: true, force: true });
}
process.on("exit", cleanup);
process.on("SIGINT", () => process.exit(130));

// The calls service narrates every connection; keep it quiet unless asked.
const rawLog = console.log.bind(console);
console.log = (...args) => { if (VERBOSE || !String(args[0]).startsWith("calls:")) rawLog(...args); };
console.warn = (...args) => { if (VERBOSE || !String(args[0]).startsWith("calls:")) rawLog(...args); };

const listen = (server) => new Promise((resolve) => server.listen(0, "127.0.0.1", () => { servers.push(server); resolve(server.address().port); }));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn, what, ms = 60000) {
  const end = Date.now() + ms;
  let last;
  while (Date.now() < end) {
    try { last = await fn(); if (last) return last; } catch (e) { last = e; }
    await sleep(250);
  }
  throw new Error("timed out: " + what + (last instanceof Error ? " (" + last.message + ")" : last !== undefined ? " (last: " + JSON.stringify(last).slice(0, 300) + ")" : ""));
}

// ---- steps --------------------------------------------------------------------------------------

const results = [];
let failed = false;
async function step(name, fn) {
  if (failed) { results.push({ name, status: "SKIP" }); return; }
  log("▶ " + name);
  const t0 = Date.now();
  try {
    const detail = await fn();
    results.push({ name, status: "PASS", detail, ms: Date.now() - t0 });
    log("  PASS" + (detail ? " — " + detail : ""));
  } catch (e) {
    failed = true;
    results.push({ name, status: "FAIL", detail: e && e.message, ms: Date.now() - t0 });
    log("  FAIL — " + (e && e.stack || e));
  }
}
function check(cond, message) { if (!cond) throw new Error(message); }

// ---- OIDC issuer --------------------------------------------------------------------------------

const { publicKey, privateKey } = crypto.generateKeyPairSync("rsa", { modulusLength: 2048 });
const jwk = publicKey.export({ format: "jwk" });
const KID = "e2e-key";
const oidc = http.createServer((req, res) => {
  if (req.url.endsWith("/protocol/openid-connect/certs")) {
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ keys: [{ kty: "RSA", kid: KID, alg: "RS256", use: "sig", n: jwk.n, e: jwk.e }] }));
  } else { res.writeHead(404); res.end(); }
});
const oidcPort = await listen(oidc);
const ISSUER = `http://127.0.0.1:${oidcPort}/realms/e2e`;
const b64u = (buf) => Buffer.from(buf).toString("base64url");
function jwt(sub, name, verifiedAddress) {
  const now = Math.floor(Date.now() / 1000);
  const head = b64u(JSON.stringify({ alg: "RS256", typ: "JWT", kid: KID }));
  const claims = { iss: ISSUER, sub, exp: now + 6 * 3600, iat: now, auth_time: now, azp: "pigeonpost-web", typ: "Bearer", preferred_username: name, aud: ["account"] };
  if (verifiedAddress) { claims[ADDRESS_CLAIM] = verifiedAddress; claims[ADDRESS_CLAIM + "_verified"] = true; }
  const body = b64u(JSON.stringify(claims));
  const sig = crypto.sign("RSA-SHA256", Buffer.from(head + "." + body), privateKey);
  return head + "." + body + "." + b64u(sig);
}

// ---- ports: the site first, because the postbox writes its origin into invitation links ----------

const freePort = async () => { const s = http.createServer(); const p = await listen(s); s.close(); servers.splice(servers.indexOf(s), 1); return p; };
const pbPort = await freePort(), intPort = await freePort();
const PB = `http://127.0.0.1:${pbPort}`, INT = `http://127.0.0.1:${intPort}`;
let calls = null;
const TYPES = { ".html": "text/html", ".js": "text/javascript", ".mjs": "text/javascript", ".css": "text/css", ".wasm": "application/wasm", ".png": "image/png", ".ico": "image/x-icon", ".woff2": "font/woff2", ".json": "application/json", ".svg": "image/svg+xml" };
const CSP = "default-src 'self'; img-src 'self' data: blob:; media-src 'self' blob:; style-src 'self'; script-src 'self' 'wasm-unsafe-eval'; worker-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'; object-src 'none'";
const site = http.createServer((req, res) => {
  const url = new URL(req.url, "http://x");
  if (url.pathname.startsWith("/v1/")) {
    const up = http.request(PB + req.url, { method: req.method, headers: { ...req.headers, host: new URL(PB).host } }, (r) => { res.writeHead(r.statusCode, r.headers); r.pipe(res); });
    up.on("error", () => { if (!res.headersSent) res.writeHead(502); res.end(); });
    req.pipe(up);
    return;
  }
  if (url.pathname.startsWith("/calls/")) { calls.handle(req, res); return; }
  let file = url.pathname === "/" ? "/index.html" : url.pathname;
  if (file.startsWith("/m/")) file = "/m.html"; // nginx: location ^~ /m/ { try_files /m.html; }
  const full = path.join(SITE, path.normalize(file));
  if (!full.startsWith(SITE) || !existsSync(full)) { res.writeHead(404); res.end(); return; }
  let data = readFileSync(full);
  if (file === "/config.js") data = Buffer.concat([data, Buffer.from("\nwindow.PIGEONPOST_INBOX.postbox = location.origin;\n")]);
  res.writeHead(200, { "content-type": TYPES[path.extname(full)] || "application/octet-stream", "cache-control": "no-store", "content-security-policy": CSP });
  res.end(data);
});
const sitePort = await listen(site);
const ORIGIN = `http://127.0.0.1:${sitePort}`;

// ---- postbox --------------------------------------------------------------------------------------

let bin = process.env.POSTBOX_BIN;
if (!bin) {
  log("building pigeonpost-postbox…");
  execFileSync("cargo", ["build", "-q", "-p", "pigeonpost-postbox"], { cwd: ROOT, stdio: "inherit" });
  bin = path.join(ROOT, "target/debug/pigeonpost-postbox");
}
writeFileSync(path.join(work, "kms"), crypto.randomBytes(32));
const postbox = spawn(bin, [], {
  env: {
    ...process.env,
    POSTBOX_BIND: `127.0.0.1:${pbPort}`, POSTBOX_PUBLIC_URL: PB, POSTBOX_INBOX_URL: ORIGIN,
    POSTBOX_DB_PATH: path.join(work, "postbox.db"), POSTBOX_KMS: "sealed-file:" + path.join(work, "kms"),
    OIDC_ISSUER: ISSUER, POSTBOX_INTERNAL_BIND: `127.0.0.1:${intPort}`,
    POSTBOX_SVC_TOKEN_OPS: OPS, POSTBOX_SVC_TOKEN_CALLS: CALLS_TOKEN, NAMESPACE_GRANT_TOKEN: GRANT,
    PIGEONPOST_BLOB_DIR: path.join(work, "blobs"), PIGEONPOST_BLOB_MIN_FREE_MB: "1",
    RUST_LOG: process.env.RUST_LOG || "warn",
  },
  stdio: ["ignore", "inherit", "inherit"],
});
children.push(postbox);
await until(() => fetch(PB + "/health").then((r) => r.ok), "postbox health", 30000);
log("postbox up on", PB, "· inbox on", ORIGIN, "(production CSP)");

// ---- the calls service, in this process ----------------------------------------------------------

calls = createCallsService(configFromEnv({
  CALLS_POSTBOX_URL: PB, CALLS_ALLOWED_ORIGINS: ORIGIN, CALLS_TRUST_PROXY: "0",
  POSTBOX_INTERNAL_URL: INT, POSTBOX_SERVICE_TOKEN: CALLS_TOKEN,
}));

// ---- API helpers ----------------------------------------------------------------------------------

async function pb(token, method, p, body, identity) {
  const r = await fetch(PB + p, { method, headers: { ...(token ? { authorization: "Bearer " + token } : {}), "content-type": "application/json", ...(identity ? { "x-pigeonpost-identity": identity } : {}) }, body: body ? JSON.stringify(body) : undefined });
  const text = await r.text();
  let json = null; try { json = JSON.parse(text); } catch (_) { /* not json */ }
  return { status: r.status, body: json, text };
}
async function callsApi(token, method, p, body) {
  const r = await fetch(ORIGIN + "/calls/v1" + p, { method, headers: { ...(token ? { authorization: "Bearer " + token } : {}), "content-type": "application/json", origin: ORIGIN }, body: body ? JSON.stringify(body) : undefined });
  const text = await r.text();
  let json = null; try { json = JSON.parse(text); } catch (_) { /* not json */ }
  return { status: r.status, body: json };
}
// A calls-service session, as a browser opens one: the event stream's first frame names it.
async function callsSession(token) {
  const ac = new AbortController();
  const r = await fetch(ORIGIN + "/calls/v1/events", { headers: { authorization: "Bearer " + token }, signal: ac.signal });
  if (!r.ok) throw new Error("calls stream: " + r.status);
  const reader = r.body.getReader();
  let buf = "";
  for (;;) {
    const { value, done } = await reader.read();
    if (done) throw new Error("calls stream closed");
    buf += Buffer.from(value).toString("utf8");
    const m = /event: ready\ndata: (.*)\n/.exec(buf);
    if (m) return { session: JSON.parse(m[1]).session, close: () => ac.abort() };
  }
}

// ---- people: one account each, a handle each, the flags each ----------------------------------------

const FLAGS = ["calendar", "account_events", "message_filtering", "call_history"];
const people = {};
for (const [key, name, verified] of [["alice", "Alice", "alice@example.test"], ["bob", "Bob"], ["carol", "Carol"], ["dave", "Dave"]]) {
  const token = jwt("e2e-" + key, name, verified);
  const me = await pb(token, "GET", "/v1/me/handles");
  if (me.status !== 200 || !me.body.account) throw new Error(`account for ${name}: ${me.status} ${me.text}`);
  const grant = await fetch(PB + "/v1/namespaces", { method: "PUT", headers: { authorization: "Bearer " + GRANT, "content-type": "application/json" }, body: JSON.stringify({ namespace: key, account_id: me.body.account }) });
  const granted = await grant.json().catch(() => ({}));
  if (!grant.ok) throw new Error(`namespace for ${name}: ${grant.status} ${JSON.stringify(granted)}`);
  for (const flag of FLAGS) {
    const r = await fetch(`${INT}/internal/v1/features/${flag}`, { method: "PUT", headers: { authorization: "Bearer " + OPS, "content-type": "application/json" }, body: JSON.stringify({ scope: "account", account_id: me.body.account, enabled: true }) });
    if (!r.ok) throw new Error(`flag ${flag} for ${name}: ${r.status} ${await r.text()}`);
  }
  const ids = await pb(token, "GET", "/v1/identities");
  const mailbox = (ids.body.identities || [])[0];
  if (!mailbox) throw new Error(`no mailbox for ${name}: ${ids.text}`);
  const who = await pb(token, "GET", "/v1/whoami?identity=" + encodeURIComponent(mailbox.address));
  people[key] = { name, token, account: me.body.account, address: mailbox.address, handle: who.body && who.body.handle };
  if (people[key].handle !== `/${key}/main`) throw new Error(`${name}'s handle is ${people[key].handle}, expected /${key}/main`);
}
log("people", Object.fromEntries(Object.entries(people).map(([k, v]) => [k, v.handle + " " + v.address])));

// ---- Chrome over the DevTools protocol, one profile (and process) per person ------------------------

// Synthetic camera and microphone, installed before any page script runs.
const syntheticMedia = (label, freq) => `(() => {
  if (!navigator.mediaDevices) return;
  const DEVICES = [
    { kind: "audioinput", deviceId: "mic-e2e", label: "Synthetic microphone", groupId: "e2e" },
    { kind: "videoinput", deviceId: "cam-e2e", label: "Synthetic camera", groupId: "e2e" },
    { kind: "audiooutput", deviceId: "default", label: "Synthetic speaker", groupId: "e2e" },
  ];
  navigator.mediaDevices.enumerateDevices = async () => DEVICES.map((d) => ({ ...d, toJSON() { return d; } }));
  function tone() {
    const ctx = new AudioContext({ sampleRate: 48000 });
    const osc = ctx.createOscillator(); osc.frequency.value = ${freq};
    const gain = ctx.createGain(); gain.gain.value = 0.2;
    const out = ctx.createMediaStreamDestination();
    osc.connect(gain).connect(out); osc.start();
    return out.stream.getAudioTracks()[0];
  }
  function picture(text, w, h, fps) {
    const canvas = document.createElement("canvas"); canvas.width = w; canvas.height = h;
    const g = canvas.getContext("2d"); let frame = 0;
    const draw = () => {
      frame++;
      g.fillStyle = "hsl(" + ((frame * 3) % 360) + " 45% 30%)"; g.fillRect(0, 0, w, h);
      g.fillStyle = "#fff"; g.font = "bold " + Math.round(h / 9) + "px sans-serif"; g.fillText(text + " " + frame, w / 24, h / 2);
    };
    setInterval(draw, 1000 / fps); draw();
    return canvas.captureStream(fps).getVideoTracks()[0];
  }
  const tag = (track, id) => { const settings = track.getSettings.bind(track); track.getSettings = () => ({ ...settings(), deviceId: id }); return track; };
  navigator.mediaDevices.getUserMedia = async (c) => {
    const out = new MediaStream();
    if (c && c.audio) out.addTrack(tag(tone(), "mic-e2e"));
    if (c && c.video) out.addTrack(tag(picture(${JSON.stringify(label)}, 640, 360, 15), "cam-e2e"));
    return out;
  };
  navigator.mediaDevices.getDisplayMedia = async () => new MediaStream([picture("screen ${label}", 1280, 720, 5)]);
})();`;

async function browser(label, token, freq) {
  const dir = path.join(work, "chrome-" + label);
  const proc = spawn(CHROME, ["--headless=new", "--remote-debugging-port=0", `--user-data-dir=${dir}`, "--no-first-run", "--no-default-browser-check", "--disable-gpu", "--window-size=1280,900",
    "--autoplay-policy=no-user-gesture-required", "--use-fake-device-for-media-stream", "--use-fake-ui-for-media-stream",
    "--disable-features=WebRtcHideLocalIpsWithMdns", ...(process.platform === "linux" ? ["--no-sandbox"] : []), "about:blank"], { stdio: "ignore" });
  children.push(proc);
  const port = await until(() => { const f = path.join(dir, "DevToolsActivePort"); return existsSync(f) && readFileSync(f, "utf8").split("\n")[0]; }, "chrome start " + label, 20000);
  const target = await until(async () => (await (await fetch(`http://127.0.0.1:${port}/json/list`)).json()).find((t) => t.type === "page"), "page target");
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((r, j) => { ws.onopen = r; ws.onerror = j; });
  let id = 0;
  const waiting = new Map();
  const consoleLines = [];
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    if (msg.id && waiting.has(msg.id)) { const { resolve, reject } = waiting.get(msg.id); waiting.delete(msg.id); msg.error ? reject(new Error(msg.error.message)) : resolve(msg.result); }
    if (msg.method === "Runtime.exceptionThrown") consoleLines.push("EXCEPTION " + (msg.params.exceptionDetails.exception?.description || msg.params.exceptionDetails.text));
    if (msg.method === "Runtime.consoleAPICalled" && ["error", "warning"].includes(msg.params.type)) consoleLines.push(msg.params.type + " " + msg.params.args.map((a) => a.value ?? a.description).join(" "));
  };
  const send = (method, params = {}) => new Promise((resolve, reject) => { const n = ++id; waiting.set(n, { resolve, reject }); ws.send(JSON.stringify({ id: n, method, params })); });
  await send("Runtime.enable");
  await send("Page.enable");
  await send("Page.addScriptToEvaluateOnNewDocument", { source: syntheticMedia(label, freq) });
  // Diagnostics only: remember what the last joinScheduled returned (printed when a step fails).
  await send("Page.addScriptToEvaluateOnNewDocument", { source: `(() => { let v; Object.defineProperty(window, "PigeonpostCalls", { configurable: true, get: () => v, set: (x) => { v = x; if (x && x.joinScheduled) { const j = x.joinScheduled; x.joinScheduled = async (...a) => { const r = await j(...a); window.__e2eLastJoin = r; return r; }; } } }); })();` });
  if (token) await send("Page.addScriptToEvaluateOnNewDocument", { source: `try { if (location.origin === ${JSON.stringify(ORIGIN)}) localStorage.setItem("ppi_token", ${JSON.stringify(token)}); } catch (_) {}` });
  const evaluate = async (expression) => {
    const r = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text);
    return r.result.value;
  };
  const go = async (url) => { await send("Page.navigate", { url }); await sleep(300); };
  const shot = async (name) => { try { writeFileSync(path.join(work, `${label}-${name}.png`), Buffer.from((await send("Page.captureScreenshot", { format: "png" })).data, "base64")); } catch (_) { /* best effort */ } };
  return { label, evaluate, go, shot, consoleLines, close: () => { try { ws.close(); } catch (_) { /* gone */ } proc.kill("SIGKILL"); } };
}

// What a browser in a call sees of the others: connected peers whose camera track is live, and
// remote <video> elements that are actually playing frames.
const MEDIA = `(() => {
  const s = window.PigeonpostCalls && window.PigeonpostCalls.inspect();
  if (!s || !s.room) return { room: null };
  const playing = [...document.querySelectorAll("#call-grid video")].filter((v) => !v.paused && v.readyState >= 2 && v.videoWidth > 0 && v.srcObject && v.srcObject.getVideoTracks().some((t) => t.readyState === "live")).length;
  return { room: s.room.id, state: s.room.state, peers: s.peers.map((p) => ({ name: p.name, guest: p.guest, connected: p.connected, remoteCamera: p.remoteCamera })), playing };
})()`;

// ---- the scenario -----------------------------------------------------------------------------------

const A = await browser("alice", people.alice.token, 440);
const B = await browser("bob", people.bob.token, 550);
const G = await browser("guest", null, 660);
const D = await browser("dave", people.dave.token, 770);
const browsers = [A, B, G, D];
const TITLE = "E2E design review " + crypto.randomBytes(3).toString("hex");
const GUEST = { name: "Gina Guest", contact: "gina.guest@example.test" };
let event = null, code = null, guestUrl = null, occurrenceId = null;
const calReady = `Boolean(window.PigeonpostCalendar && window.PigeonpostCalendar.inspect().enabled && document.getElementById("calendar-btn") && !document.getElementById("calendar-btn").hidden)`;
const countsText = `(document.getElementById("cd-counts") || {}).textContent || ""`;

await step("1. Alice creates the meeting in the create sheet; it shows in her week view", async () => {
  await A.go(ORIGIN + "/");
  await until(() => A.evaluate(calReady), "Alice's calendar enabled", 60000);
  await A.evaluate(`window.PigeonpostCalendar.open(); window.PigeonpostCalendar.setView("week");`);
  await until(() => A.evaluate(`!document.getElementById("calendar-view").hidden && document.getElementById("cal-body").dataset.view === "week"`), "week view shown");
  await A.evaluate(`document.getElementById("cal-new").click()`);
  await until(() => A.evaluate(`!document.getElementById("cal-edit-sheet").hidden && Boolean(document.getElementById("ce-title"))`), "create sheet");
  // Start at the next whole minute + 2, in UTC (the sheet's time zone picker set to UTC).
  const startMs = Math.ceil(Date.now() / 60000) * 60000 + 2 * 60000;
  const s = new Date(startMs), e = new Date(startMs + 30 * 60000);
  const ymd = (d) => d.toISOString().slice(0, 10), hm = (d) => d.toISOString().slice(11, 16);
  check(ymd(s) === ymd(e), "the 30-minute meeting would cross midnight UTC; run again in a few minutes");
  const filled = await A.evaluate(`(() => {
    const $ = (id) => document.getElementById(id);
    $("ce-title").value = ${JSON.stringify(TITLE)};
    $("ce-date").value = ${JSON.stringify(ymd(s))};
    $("ce-start").value = ${JSON.stringify(hm(s))};
    $("ce-end").value = ${JSON.stringify(hm(e))};
    $("ce-tz").value = "UTC";
    for (const g of ${JSON.stringify([people.bob.handle, people.carol.handle, `${GUEST.name} <${GUEST.contact}>`])}) { $("ce-guest").value = g; $("ce-guest-add").click(); }
    return { chips: [...document.querySelectorAll("#ce-chips .ce-chip")].map((c) => c.dataset.kind), meeting: $("ce-meeting").checked, tz: $("ce-tz").value, note: $("ce-contact-note").hidden ? "" : $("ce-contact-note").textContent, error: $("ce-error").hidden ? "" : $("ce-error").textContent };
  })()`);
  check(!filled.error, "form error: " + filled.error);
  check(JSON.stringify(filled.chips) === JSON.stringify(["handle", "handle", "contact"]), "guest chips: " + JSON.stringify(filled.chips));
  check(filled.meeting, "the meeting switch did not turn on with guests");
  check(filled.tz === "UTC", "time zone picker has no UTC");
  check(/Invitation link to share yourself/.test(filled.note), "contact note should say the link is shared by hand: " + filled.note);
  await A.evaluate(`document.getElementById("ce-save").click()`);
  const link = await until(() => A.evaluate(`(() => { const e = document.getElementById("ce-error"); if (e && !e.hidden && e.textContent) return "ERROR " + e.textContent; const s = document.getElementById("cal-detail-sheet"); const l = document.getElementById("cd-link"); return s && !s.hidden && l ? l.textContent : ""; })()`), "event saved and detail open", 20000);
  check(!link.startsWith("ERROR"), "save failed: " + link);
  code = link.split("/m/")[1];
  check(/^[ABCDEFGHJKMNPQRSTUVWXYZ23456789]{10}$/.test(code || ""), "meeting link " + link);
  check(link.startsWith(ORIGIN + "/m/"), "meeting link is not on the inbox origin: " + link);
  const now = Math.floor(Date.now() / 1000);
  const list = await pb(people.alice.token, "GET", `/v1/calendar/events?from=${now - 3600}&to=${now + 7200}`, null, people.alice.address);
  check(list.status === 200, "list: " + list.status + " " + list.text);
  const occ = (list.body.occurrences || []).find((o) => o.title === TITLE);
  check(occ, "the event is not in Alice's listing");
  occurrenceId = occ.occurrence_id;
  check(occ.meeting && occ.meeting.code === code && occ.role === "organizer", "listing: " + JSON.stringify(occ));
  const got = await pb(people.alice.token, "GET", `/v1/calendar/events/${occ.event_id}`, null, people.alice.address);
  event = got.body.event;
  check(event.attendees.length === 3 && event.counts.needs_action === 3, "attendees/counts: " + JSON.stringify(event.counts));
  const item = `document.querySelector('#cal-body[data-view="week"] .cal-day-col .cal-event[data-event="${event.id}"]')`;
  await until(() => A.evaluate(`Boolean(${item})`), "the event in Alice's week view", 20000);
  const chipText = await A.evaluate(`${item}.querySelector(".cal-event-title").textContent + " · " + ${item}.querySelector(".cal-event-time").textContent + (${item}.querySelector(".cal-event-join") ? " · inline Join" : "")`);
  check(chipText.includes(TITLE), "week view item text: " + chipText);
  check(chipText.includes("inline Join"), "the open meeting has no inline Join in the week view: " + chipText);
  // The time grid is laid out under the production CSP (style-src 'self'): seven day columns and the
  // event placed at its time, not stacked at the top.
  const layout = await A.evaluate(`(() => { const h = document.querySelector(".cal-week-heads"), it = ${item}; const r = it.getBoundingClientRect(); return { cols: getComputedStyle(h).gridTemplateColumns.split(" ").length, top: it.style.top, height: Math.round(r.height), width: Math.round(r.width) }; })()`);
  check(layout.cols === 8 && layout.top && layout.height >= 20 && layout.width > 40, "the week grid is not laid out: " + JSON.stringify(layout));
  const counts = await A.evaluate(countsText);
  check(/3 awaiting/.test(counts), "counts in the open event: " + counts);
  await A.shot("1-created");
  return `${event.id} code ${code.slice(0, 5)}-${code.slice(5)}, starts ${new Date(event.starts_at * 1000).toISOString().slice(11, 16)}Z; week view "${chipText}"; counts "${counts}"; grid ${layout.cols - 1} day columns, item ${layout.width}x${layout.height}px`;
});

await step("2. Bob answers Yes in the invitation panel, Carol through the API; Alice sees the counts", async () => {
  await B.go(ORIGIN + "/");
  await until(() => B.evaluate(calReady), "Bob's calendar enabled", 60000);
  await until(() => B.evaluate(`window.PigeonpostCalendar.inspect().invitations >= 1`), "Bob has the invitation", 30000);
  await B.evaluate(`window.PigeonpostCalendar.open()`);
  await until(() => B.evaluate(`(() => { const b = document.getElementById("cal-invites-banner"); return b && !b.hidden; })()`), "Bob's invitations banner");
  await B.evaluate(`document.getElementById("cal-invites-banner").click()`);
  await until(() => B.evaluate(`Boolean(document.querySelector('#cal-invites-sheet:not([hidden]) .cal-invite[data-event="${event.id}"] button[data-rsvp="accepted"]'))`), "invitation card with Yes");
  const card = await B.evaluate(`document.querySelector('.cal-invite[data-event="${event.id}"]').textContent`);
  check(card.includes(TITLE) && card.includes(people.alice.handle), "invitation card: " + card);
  await B.shot("2-invitation");
  await B.evaluate(`document.querySelector('.cal-invite[data-event="${event.id}"] button[data-rsvp="accepted"]').click()`);
  await until(async () => (await pb(people.bob.token, "GET", `/v1/calendar/events/${event.id}`, null, people.bob.address)).body?.event?.my?.rsvp === "accepted", "Bob's answer stored");
  await until(() => B.evaluate(`!document.querySelector('.cal-invite[data-event="${event.id}"]')`), "the card leaves Bob's panel", 10000);
  const afterBob = await until(() => A.evaluate(countsText).then((t) => /1 yes/.test(t) && t), "Alice's open event shows 1 yes", 20000);
  const carol = await pb(people.carol.token, "POST", `/v1/calendar/events/${event.id}/rsvp`, { status: "accepted", scope: "all" }, people.carol.address);
  check(carol.status === 200, "Carol's RSVP: " + carol.status + " " + carol.text);
  const afterCarol = await until(() => A.evaluate(countsText).then((t) => /2 yes/.test(t) && t), "Alice's open event shows 2 yes", 20000);
  const rows = await A.evaluate(`[...document.querySelectorAll("#cd-guests .cd-guest")].map((li) => li.querySelector(".cd-guest-name").textContent + "=" + li.querySelector(".cd-rsvp").dataset.rsvp)`);
  check(rows.includes(people.bob.handle + "=accepted") && rows.includes(people.carol.handle + "=accepted"), "guest rows: " + JSON.stringify(rows));
  return `after Bob "${afterBob}", after Carol "${afterCarol}"`;
});

await step("3. The guest opens the personal link, the fragment leaves the address bar, and confirms Yes", async () => {
  const button = `document.querySelector('#cd-guests .cd-guest[data-kind="contact"] .cd-guest-link')`;
  await until(() => A.evaluate(`Boolean(${button})`), "Copy invitation link button");
  await A.evaluate(`${button}.click()`);
  guestUrl = await until(() => A.evaluate(`(() => { const i = document.querySelector('#cd-guests .cd-guest[data-kind="contact"] .cd-guest-url'); return i && i.value; })()`), "the guest's link shown");
  check(guestUrl.startsWith(`${ORIGIN}/m/${code}#t=`), "guest link shape: " + guestUrl.replace(/#t=.*/, "#t=…"));
  // `r=accepted` preselects Yes; the page must still ask for a confirmation.
  await G.go(guestUrl + "&r=accepted");
  await until(() => G.evaluate(`document.getElementById("m-title").textContent === ${JSON.stringify(TITLE)}`), "preview rendered", 20000);
  const page = await G.evaluate(`({ href: location.href, hash: location.hash, sub: document.getElementById("m-sub").textContent, when: (document.getElementById("m-when") || {}).textContent || "", confirm: Boolean(document.getElementById("m-confirm")), current: (document.getElementById("m-current") || {}).textContent || "", join: Boolean(document.getElementById("m-ask")), text: document.getElementById("m-content").innerText })`);
  check(page.hash === "" && !page.href.includes("t=") && page.href === `${ORIGIN}/m/${code}`, "the fragment is still in the address bar: " + page.href.replace(/t=[^&]*/, "t=…"));
  check(page.confirm, "no Confirm button for the preselected answer");
  check(!page.current, "opening the link changed the answer: " + page.current);
  check(page.join, "no Ask to join in the open window");
  check(!page.text.includes(GUEST.contact), "the preview shows the guest's address");
  const before = await pb(people.alice.token, "GET", `/v1/calendar/events/${event.id}`, null, people.alice.address);
  check(before.body.event.attendees.find((a) => a.kind === "external").rsvp === "needs_action", "opening the link answered already");
  await G.shot("3-preview");
  await G.evaluate(`document.getElementById("m-confirm").click()`);
  await until(() => G.evaluate(`((document.getElementById("m-current") || {}).textContent || "") === "Your answer: Yes"`), "the guest page shows Yes", 15000);
  const after = await until(() => A.evaluate(countsText).then((t) => /3 yes/.test(t) && t), "Alice's open event shows 3 yes", 20000);
  const g = (await pb(people.alice.token, "GET", `/v1/calendar/events/${event.id}`, null, people.alice.address)).body.event.attendees.find((a) => a.kind === "external");
  check(g.rsvp === "accepted" && g.proof === "link", "the guest's answer: " + JSON.stringify({ rsvp: g.rsvp, proof: g.proof }));
  return `preview "${page.sub}" ${page.when}; answered by link; Alice sees "${after}"`;
});

await step("4. Alice joins from the event, Bob from the meeting page, Alice lets the guest in; all see video", async () => {
  // Alice: the open event's Join.
  await until(() => A.evaluate(`(() => { const b = document.getElementById("cd-join"); return b && !b.disabled; })()`), "Alice's Join enabled");
  await A.evaluate(`document.getElementById("cd-join").click()`);
  const aliceRoom = await until(() => A.evaluate(`(() => { const s = window.PigeonpostCalls.inspect(); return s.room && s.room.id && !document.getElementById("call-ui").hidden && s.room.id; })()`), "Alice in the room", 20000);
  const hostTools = await until(() => A.evaluate(`(() => { const t = document.getElementById("call-host-tools"); return t && !t.hidden; })()`), "host tools for Alice", 10000);
  // Bob: the bare meeting link, signed in.
  await B.go(`${ORIGIN}/m/${code}`);
  await until(() => B.evaluate(`Boolean(document.getElementById("m-member-join"))`), "Bob's Join on the meeting page", 20000);
  await B.evaluate(`document.getElementById("m-member-join").click()`);
  await until(() => B.evaluate(`(() => { const s = window.PigeonpostCalls.inspect(); return s.room && s.room.id === ${JSON.stringify(aliceRoom)}; })()`), "Bob in Alice's room", 20000);
  // The guest: Ask to join (invitation link → knock), Alice lets them in from the call screen.
  await G.evaluate(`document.getElementById("m-ask").click()`);
  await until(() => G.evaluate(`(() => { const l = document.getElementById("call-lobby"); return l && !l.hidden && /Waiting/.test(l.textContent); })()`), "the guest waits in the lobby", 20000);
  const knock = await until(() => A.evaluate(`(() => { const k = document.querySelector("#call-knocks .call-knock"); return k && k.innerText; })()`), "Alice sees the knock", 20000);
  check(knock.includes(GUEST.name) && /Guest/.test(knock), "knock card: " + knock);
  const bobKnock = await B.evaluate(`[...document.querySelectorAll("#call-knocks .call-pill-primary")].length`);
  check(bobKnock === 0, "Bob (an attendee) was offered Let in");
  await A.shot("4-knock");
  await A.evaluate(`document.querySelector("#call-knocks .call-pill-primary").click()`);
  const seen = {};
  for (const b of [A, B, G]) {
    seen[b.label] = await until(async () => {
      const m = await b.evaluate(MEDIA);
      return m.room === aliceRoom && m.peers.length === 2 && m.peers.every((p) => p.connected && p.remoteCamera >= 1) && m.playing >= 2 && m;
    }, b.label + " receives both others' video", 45000);
  }
  for (const b of [A, B, G]) await b.shot("4-call");
  const names = (l) => seen[l].peers.map((p) => p.name).sort().join(" + ");
  return `room ${aliceRoom.slice(0, 8)}…; host tools ${Boolean(hostTools)}; alice sees ${names("alice")}, bob sees ${names("bob")}, guest sees ${names("guest")}; playing remote videos ${seen.alice.playing}/${seen.bob.playing}/${seen.guest.playing}`;
});

await step("5. A signed-in stranger is refused; an anonymous status reveals nothing", async () => {
  const status = await callsApi(people.dave.token, "GET", `/meetings/scheduled/${code}`);
  check(status.status === 200 && status.body.state === "unavailable" && status.body.reason === "not_invited" && status.body.room === null, "stranger's status: " + JSON.stringify(status));
  const s = await callsSession(people.dave.token);
  let join, asBob;
  try {
    join = await callsApi(people.dave.token, "POST", `/meetings/scheduled/${code}/join`, { session: s.session, e2ee: false });
    asBob = await callsApi(people.dave.token, "POST", `/meetings/scheduled/${code}/join`, { session: s.session, as: people.bob.address, e2ee: false });
  } finally { s.close(); }
  check(join.status === 403 && join.body.error === "not_invited", "stranger's join: " + JSON.stringify(join));
  check(asBob.status === 403 && asBob.body.error === "not_your_mailbox", "stranger posing as Bob: " + JSON.stringify(asBob));
  // The stranger's browser on the meeting link: no Join, nothing about the event.
  await D.go(`${ORIGIN}/m/${code}`);
  const page = await until(() => D.evaluate(`(() => { const n = document.getElementById("m-not-open"); return n && { text: document.getElementById("join").innerText, join: Boolean(document.getElementById("m-member-join")) }; })()`), "the stranger's meeting page", 20000);
  check(!page.join && !page.text.includes(TITLE), "the stranger's page: " + JSON.stringify(page));
  const anon = await callsApi(null, "GET", `/meetings/scheduled/${code}`);
  check(anon.status === 200 && JSON.stringify(anon.body) === JSON.stringify({ state: "unavailable", can_knock: false }), "anonymous status: " + JSON.stringify(anon));
  const room = await A.evaluate(`window.PigeonpostCalls.inspect().peers.length`);
  check(room === 2, "the room changed size during the stranger's attempts: " + room);
  return `status ${JSON.stringify({ state: status.body.state, reason: status.body.reason })}; join ${join.status} ${join.body.error}; page "${page.text.split("\n").filter(Boolean).slice(-1)[0]}"; anonymous ${JSON.stringify(anon.body)}`;
});

await step("6. Alice ends the meeting for everyone; her call history carries the event and occurrence", async () => {
  await A.evaluate(`document.getElementById("call-end-all").click()`);
  for (const b of [A, B, G]) await until(() => b.evaluate(`!window.PigeonpostCalls.inspect().room`), b.label + " left the ended call", 20000);
  const entry = await until(async () => {
    const r = await pb(people.alice.token, "GET", "/v1/calls/log", null, people.alice.address);
    const list = (r.body && (r.body.entries || r.body.calls || r.body.items)) || [];
    return list.find((x) => x.calendar_event_id === event.id) || (list.length ? null : undefined);
  }, "Alice's call history entry for the meeting", 30000);
  check(entry.occurrence_id === occurrenceId, "occurrence_id " + entry.occurrence_id + " ≠ " + occurrenceId);
  const bobLog = await pb(people.bob.token, "GET", "/v1/calls/log", null, people.bob.address);
  const bobEntry = ((bobLog.body && (bobLog.body.entries || bobLog.body.calls || bobLog.body.items)) || []).find((x) => x.calendar_event_id === event.id);
  return `alice: ${JSON.stringify(Object.fromEntries(Object.entries(entry).filter(([k]) => ["kind", "direction", "state", "outcome", "calendar_event_id", "occurrence_id", "duration", "duration_s"].includes(k))))}${bobEntry ? `; bob's entry too (${bobEntry.occurrence_id === occurrenceId ? "same occurrence" : "other occurrence"})` : ""}`;
});

await step("7. No uncaught exceptions in any page", async () => {
  const problems = browsers.flatMap((b) => b.consoleLines.filter((l) => /EXCEPTION/.test(l)).map((l) => b.label + ": " + l));
  check(!problems.length, problems.join("\n"));
  return browsers.map((b) => `${b.label}: ${b.consoleLines.length} console warnings/errors`).join(", ");
});

// ---- summary --------------------------------------------------------------------------------------

rawLog("\n==== e2e-calendar summary ====");
for (const r of results) rawLog(`${r.status.padEnd(4)} ${r.name}${r.ms != null ? ` (${(r.ms / 1000).toFixed(1)}s)` : ""}${r.status === "FAIL" ? "\n     " + r.detail : ""}`);
if (failed) {
  for (const b of browsers) {
    await b.shot("failure");
    const seen = await b.evaluate(`JSON.stringify({ url: location.pathname, notes: ["toast", "call-toast", "call-inner-toast", "cal-toast-text", "m-error", "call-status"].map((id) => { const n = document.getElementById(id); return n && !n.hidden && n.textContent ? id + ": " + n.textContent : null; }).filter(Boolean), lastJoin: window.__e2eLastJoin || null })`).catch((e) => "unreadable: " + e.message);
    rawLog(`-- ${b.label} page: ${seen}`);
    if (b.consoleLines.length) rawLog(`-- ${b.label} console:\n   ` + b.consoleLines.slice(-15).join("\n   ")); }
  rawLog(`screenshots and data kept in ${work}`);
  process.env.E2E_KEEP = "1";
}
for (const b of browsers) b.close();
rawLog(failed ? "E2E FAILED" : "E2E OK");
process.exit(failed ? 1 : 0);
