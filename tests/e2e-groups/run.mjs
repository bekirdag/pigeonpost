// End-to-end: persistent E2EE groups in the real web inbox, against a real locally built postbox,
// in four headless Chrome profiles (Phase 3a).
//
//   node tests/e2e-groups/run.mjs            # builds the postbox unless POSTBOX_BIN is set
//
// What runs:
//   * a throwaway OIDC issuer (RSA key + JWKS) so the postbox validates real member JWTs — the web
//     inbox signs in with those, exactly as with the production realm;
//   * the postbox binary with a temp database and blob dir, flags `groups`, `disappearing` and
//     `account_events` switched on through the internal API;
//   * a static server for site-inbox that also proxies /v1 to the postbox, so the page and the API
//     share one origin (no CORS in the way);
//   * Chrome with four profiles: Alice, Bob, Carol, and Alice's second browser.
//
// Scenario: Alice creates a group with Bob and Carol through the UI; both accept; they chat; Alice
// removes Carol, who stops receiving anything; Alice's second browser joins as a new device and sees
// no earlier messages; a 5-minute disappearing timer runs out and the message vanishes everywhere.
// No Chrome dependency beyond the browser itself: the DevTools protocol is spoken directly.
import { spawn, execFileSync } from "node:child_process";
import http from "node:http";
import { mkdtempSync, readFileSync, existsSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const SITE = path.join(ROOT, "site-inbox");
const CHROME = process.env.CHROME || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const OPS = "svc_ops_" + crypto.randomBytes(20).toString("hex");
const TIMER_WAIT = Number(process.env.E2E_TIMER_WAIT_S || 400);
const SKIP_TIMER = process.env.E2E_SKIP_TIMER === "1";
const work = mkdtempSync(path.join(tmpdir(), "pp-e2e-groups-"));
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

const listen = (server) => new Promise((resolve) => server.listen(0, "127.0.0.1", () => { servers.push(server); resolve(server.address().port); }));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn, what, ms = 60000) {
  const end = Date.now() + ms;
  let last;
  while (Date.now() < end) {
    try { last = await fn(); if (last) return last; } catch (e) { last = e; }
    await sleep(250);
  }
  throw new Error("timed out: " + what + (last instanceof Error ? " (" + last.message + ")" : ""));
}

// ---- OIDC issuer ------------------------------------------------------------------------------

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
function jwt(sub, name) {
  const now = Math.floor(Date.now() / 1000);
  const head = b64u(JSON.stringify({ alg: "RS256", typ: "JWT", kid: KID }));
  const body = b64u(JSON.stringify({ iss: ISSUER, sub, exp: now + 6 * 3600, iat: now, auth_time: now, azp: "pigeonpost-web", typ: "Bearer", preferred_username: name, aud: ["account"] }));
  const sig = crypto.sign("RSA-SHA256", Buffer.from(head + "." + body), privateKey);
  return head + "." + body + "." + b64u(sig);
}

// ---- postbox ----------------------------------------------------------------------------------

let bin = process.env.POSTBOX_BIN;
if (!bin) {
  log("building pigeonpost-postbox…");
  execFileSync("cargo", ["build", "-q", "-p", "pigeonpost-postbox"], { cwd: ROOT, stdio: "inherit" });
  bin = path.join(ROOT, "target/debug/pigeonpost-postbox");
}
const freePort = async () => { const s = http.createServer(); const p = await listen(s); s.close(); servers.splice(servers.indexOf(s), 1); return p; };
const pbPort = await freePort(), intPort = await freePort();
writeFileSync(path.join(work, "kms"), crypto.randomBytes(32));
const postbox = spawn(bin, [], {
  env: {
    ...process.env,
    POSTBOX_BIND: `127.0.0.1:${pbPort}`, POSTBOX_PUBLIC_URL: `http://127.0.0.1:${pbPort}`,
    POSTBOX_DB_PATH: path.join(work, "postbox.db"), POSTBOX_KMS: "sealed-file:" + path.join(work, "kms"),
    OIDC_ISSUER: ISSUER, POSTBOX_INTERNAL_BIND: `127.0.0.1:${intPort}`, POSTBOX_SVC_TOKEN_OPS: OPS,
    PIGEONPOST_BLOB_DIR: path.join(work, "blobs"), PIGEONPOST_BLOB_MIN_FREE_MB: "1",
    RUST_LOG: process.env.RUST_LOG || "warn",
  },
  stdio: ["ignore", "inherit", "inherit"],
});
children.push(postbox);
const PB = `http://127.0.0.1:${pbPort}`, INT = `http://127.0.0.1:${intPort}`;
await until(() => fetch(PB + "/health").then((r) => r.ok), "postbox health", 30000);
for (const name of ["groups", "disappearing", "account_events"]) {
  const r = await fetch(`${INT}/internal/v1/features/${name}`, { method: "PUT", headers: { authorization: "Bearer " + OPS, "content-type": "application/json" }, body: JSON.stringify({ scope: "global", enabled: true }) });
  if (!r.ok) throw new Error(`flag ${name}: ${r.status} ${await r.text()}`);
}
log("postbox up on", PB);

async function pb(token, method, p, body, identity, device) {
  const r = await fetch(PB + p, { method, headers: { authorization: "Bearer " + token, "content-type": "application/json", ...(identity ? { "x-pigeonpost-identity": identity } : {}), ...(device ? { "x-pigeonpost-device": device } : {}) }, body: body ? JSON.stringify(body) : undefined });
  const text = await r.text();
  let json = null; try { json = JSON.parse(text); } catch (_) { /* not json */ }
  return { status: r.status, body: json };
}

// People: one member account each, one mailbox each. Alice's second browser shares her login.
const people = {};
for (const [key, name] of [["alice", "Alice"], ["bob", "Bob"], ["carol", "Carol"]]) {
  const token = jwt("e2e-" + key, name);
  const made = await pb(token, "POST", "/v1/identities", { label: name });
  if (made.status !== 201) throw new Error(`mailbox for ${name}: ${made.status} ${JSON.stringify(made.body)}`);
  people[key] = { name, token, address: made.body.address };
}
log("mailboxes", Object.fromEntries(Object.entries(people).map(([k, v]) => [k, v.address])));

// ---- static inbox + same-origin proxy ---------------------------------------------------------

const TYPES = { ".html": "text/html", ".js": "text/javascript", ".mjs": "text/javascript", ".css": "text/css", ".wasm": "application/wasm", ".png": "image/png", ".ico": "image/x-icon", ".woff2": "font/woff2", ".json": "application/json", ".svg": "image/svg+xml" };
const site = http.createServer((req, res) => {
  const url = new URL(req.url, "http://x");
  if (url.pathname.startsWith("/v1/")) {
    const up = http.request(PB + req.url, { method: req.method, headers: { ...req.headers, host: `127.0.0.1:${pbPort}` } }, (r) => { res.writeHead(r.statusCode, r.headers); r.pipe(res); });
    up.on("error", () => { res.writeHead(502); res.end(); });
    req.pipe(up);
    return;
  }
  if (url.pathname.startsWith("/calls/")) { res.writeHead(404, { "content-type": "application/json" }); res.end('{"error":"not_found"}'); return; }
  let file = url.pathname === "/" ? "/index.html" : url.pathname;
  if (/^\/g\/grp_[A-Za-z0-9]+\/?$/.test(file)) file = "/g/index.html";
  const full = path.join(SITE, path.normalize(file));
  if (!full.startsWith(SITE) || !existsSync(full)) { res.writeHead(404); res.end(); return; }
  let data = readFileSync(full);
  if (file === "/config.js") data = Buffer.concat([data, Buffer.from("\nwindow.PIGEONPOST_INBOX.postbox = location.origin;\n")]);
  res.writeHead(200, { "content-type": TYPES[path.extname(full)] || "application/octet-stream", "cache-control": "no-store",
    "content-security-policy": "default-src 'self'; img-src 'self' data: blob:; media-src 'self' blob:; style-src 'self'; script-src 'self' 'wasm-unsafe-eval'; worker-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'; object-src 'none'" });
  res.end(data);
});
const sitePort = await listen(site);
const ORIGIN = `http://127.0.0.1:${sitePort}`;
log("inbox on", ORIGIN, "(served under the production CSP)");

// ---- Chrome over the DevTools protocol ----------------------------------------------------------

async function browser(label, token) {
  const dir = path.join(work, "chrome-" + label);
  const proc = spawn(CHROME, ["--headless=new", "--remote-debugging-port=0", `--user-data-dir=${dir}`, "--no-first-run", "--no-default-browser-check", "--disable-gpu", "--window-size=1280,900", "about:blank"], { stdio: "ignore" });
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
  await send("Page.addScriptToEvaluateOnNewDocument", { source: `try { if (location.origin === ${JSON.stringify(ORIGIN)}) localStorage.setItem("ppi_token", ${JSON.stringify(token)}); } catch (_) {}` });
  const evaluate = async (expression) => {
    const r = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text);
    return r.result.value;
  };
  const go = async (url) => { await send("Page.navigate", { url }); await sleep(300); };
  return { label, evaluate, go, consoleLines, close: () => { ws.close(); proc.kill("SIGKILL"); } };
}

const UI = {
  ready: `Boolean(window.PigeonpostGroups && window.PigeonpostGroups.inspect().enabled && document.getElementById("new-group-btn") && !document.getElementById("new-group-btn").hidden)`,
  groups: `JSON.stringify(window.PigeonpostGroups.inspect().groups)`,
  messagesText: `(document.getElementById("group-messages") || {}).innerText || ""`,
};

const A = await browser("alice", people.alice.token);
const B = await browser("bob", people.bob.token);
const C = await browser("carol", people.carol.token);
const all = [A, B, C];
for (const b of all) await b.go(ORIGIN + "/");
for (const b of all) await until(() => b.evaluate(UI.ready), b.label + " groups ready", 60000);
log("groups enabled in three browsers; devices:", await Promise.all(all.map((b) => b.evaluate("window.PigeonpostGroups.inspect().deviceId"))));

// 1. Alice creates the group through the sheet.
await A.evaluate(`(() => {
  document.getElementById("new-group-btn").click();
  const who = document.getElementById("gc-who");
  for (const a of ${JSON.stringify([people.bob.address, people.carol.address])}) { who.value = a; who.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true })); }
  document.getElementById("gc-name").value = "E2E garden";
  document.getElementById("gc-create").click();
})()`);
const gid = await until(async () => { const v = await A.evaluate(`window.PigeonpostGroups.inspect().open`); return v; }, "group created and open");
log("created", gid);

// 2. Bob and Carol accept from the invitation row.
for (const b of [B, C]) {
  await until(() => b.evaluate(`(() => { const btn = document.getElementById("group-invitations-btn"); if (!btn) { window.PigeonpostGroups.render(); return false; } if (btn.getAttribute("aria-expanded") !== "true") btn.click(); const join = [...document.querySelectorAll(".invitation-card button")].find((x) => x.textContent === "Join"); if (join) { join.click(); return true; } return false; })()`), b.label + " sees and accepts the invitation", 60000);
}
// Alice's device adds them (crypto_pending), their devices join from the Welcome.
for (const b of [B, C]) {
  await until(() => b.evaluate(`(() => { const c = window.PigeonpostGroups._client(); return Boolean(c && c.inGroup(${JSON.stringify(gid)})); })()`), b.label + " joined the encryption", 90000);
}
log("bob and carol are encrypted members");

async function say(b, text) {
  await b.evaluate(`(() => { window.PigeonpostGroups._open(${JSON.stringify(gid)}); })()`);
  await until(() => b.evaluate(`!document.getElementById("group-composer").hidden`), b.label + " composer");
  await b.evaluate(`(() => { const t = document.getElementById("group-compose"); t.value = ${JSON.stringify(text)}; document.getElementById("group-composer").requestSubmit(); })()`);
}
async function sees(b, text, ms = 60000) {
  await b.evaluate(`window.PigeonpostGroups._open(${JSON.stringify(gid)})`);
  await until(() => b.evaluate(UI.messagesText).then((t) => t.includes(text)), `${b.label} sees "${text}"`, ms);
}

// 3. Chat.
await say(A, "hello from alice");
await sees(B, "hello from alice");
await sees(C, "hello from alice");
await say(B, "bob here");
await sees(A, "bob here");
await sees(C, "bob here");
log("chat round trip ok");

// 4. Alice removes Carol through group info.
await A.evaluate(`window.PigeonpostGroups._open(${JSON.stringify(gid)}); document.getElementById("group-info-btn").click();`);
await until(() => A.evaluate(`[...document.querySelectorAll(".gi-member")].some((li) => [...li.querySelectorAll("button")].some((b) => b.textContent === "Remove"))`), "member actions");
await A.evaluate(`(() => {
  const row = [...document.querySelectorAll(".gi-member")].find((li) => li.textContent.includes(${JSON.stringify(people.carol.address.slice(0, 12))}) || li.textContent.includes("Carol"));
  [...(row || document.querySelectorAll(".gi-member")[2]).querySelectorAll("button")].find((b) => b.textContent === "Remove").click();
})()`);
await until(() => A.evaluate(`!document.getElementById("group-confirm-sheet").hidden`), "confirm");
await A.evaluate(`document.getElementById("gq-ok").click(); document.querySelector("#group-info-sheet .sheet-foot .btn-primary").click();`);
await sleep(1000);
await say(A, "after carol left");
await sees(B, "after carol left");
await sleep(3000);
const carolSees = await C.evaluate(`(async () => { const c = window.PigeonpostGroups._client(); try { await c.sync(${JSON.stringify(gid)}); } catch (e) { return "refused:" + e.code; } return c.history(${JSON.stringify(gid)}).map((x) => x.text || "").join("|"); })()`);
if (String(carolSees).includes("after carol left")) throw new Error("removed member read a later message");
const carolDevice = await C.evaluate("window.PigeonpostGroups.inspect().deviceId");
const carolApi = await pb(people.carol.token, "GET", `/v1/groups/${gid}/messages?after_seq=0`, null, people.carol.address, carolDevice);
if (carolApi.status !== 403) throw new Error("removed member still has server access: " + carolApi.status);
log("carol removed: client", carolSees, "· server", carolApi.status, carolApi.body && carolApi.body.error);

// 5. Alice's second browser: a new device, no history.
const A2 = await browser("alice-2", people.alice.token);
await A2.go(ORIGIN + "/");
await until(() => A2.evaluate(UI.ready), "alice-2 ready", 60000);
await until(() => A2.evaluate(`(() => { const c = window.PigeonpostGroups._client(); return Boolean(c && c.inGroup(${JSON.stringify(gid)})); })()`), "alice-2 added to the group", 120000);
await say(B, "for the new device");
await sees(A2, "for the new device");
const a2Text = await A2.evaluate(UI.messagesText);
if (a2Text.includes("hello from alice") || a2Text.includes("bob here")) throw new Error("new device received pre-join history");
await until(() => B.evaluate(UI.messagesText).then((t) => /added a new device/.test(t)), "bob is told Alice added a device", 30000);
log("second device: no history, change visible to Bob");

// 6. Disappearing messages: 5 minutes, the shortest timer offered.
if (!SKIP_TIMER) {
  const g = await pb(people.alice.token, "GET", `/v1/groups/${gid}`, null, people.alice.address);
  const patched = await A.evaluate(`(async () => {
    window.PigeonpostGroups._open(${JSON.stringify(gid)});
    document.getElementById("group-info-btn").click();
    for (let i = 0; i < 50 && !document.getElementById("gi-timer"); i++) await new Promise((r) => setTimeout(r, 100));
    const s = document.getElementById("gi-timer"); s.value = "300"; s.dispatchEvent(new Event("change"));
    return true;
  })()`);
  void g; void patched;
  await until(async () => (await pb(people.alice.token, "GET", `/v1/groups/${gid}`, null, people.alice.address)).body?.ephemeral_secs === 300, "timer set");
  await A.evaluate(`document.querySelector("#group-info-sheet .sheet-foot .btn-primary").click()`);
  await sleep(1500);
  await say(A, "this will vanish");
  await sees(B, "this will vanish");
  log(`timer set; waiting up to ${TIMER_WAIT}s for expiry…`);
  await until(() => B.evaluate(`(window.PigeonpostGroups.render(), (document.getElementById("group-messages").innerText || ""))`).then((t) => !t.includes("this will vanish")), "message vanished in Bob's browser", TIMER_WAIT * 1000);
  const bobDevice = await B.evaluate("window.PigeonpostGroups.inspect().deviceId");
  const after = await pb(people.bob.token, "GET", `/v1/groups/${gid}/messages?after_seq=0&limit=100`, null, people.bob.address, bobDevice);
  const stillThere = (after.body?.items || []).filter((i) => i.type === "application" && i.ciphertext && i.expires_at);
  if (stillThere.length) throw new Error("server still serves an expired message");
  log("expired everywhere; server listing for bob:", after.status, (after.body?.items || []).length, "items, none expired");
}

const problems = [A, B, C, A2].flatMap((b) => b.consoleLines.filter((l) => /EXCEPTION/.test(l)).map((l) => b.label + ": " + l));
if (problems.length) { console.error(problems.join("\n")); throw new Error("uncaught page exceptions"); }
log("E2E OK");
for (const b of [A, B, C, A2]) b.close();
process.exit(0);
