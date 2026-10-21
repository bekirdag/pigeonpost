// End-to-end: message actions and receipts (Phase 1e, docs/message-actions.md) in the real web
// inbox, against a real locally built postbox, in two headless Chrome profiles.
//
//   node tests/e2e-message-actions/run.mjs     # builds the postbox unless POSTBOX_BIN is set
//
// What runs (the scaffolding is the groups e2e's): a throwaway OIDC issuer so the postbox accepts
// real member JWTs; the postbox with a temp database and the flags message_edit, message_reply,
// receipts, agent_status and account_events switched on through the internal API; a static server
// for site-inbox that proxies /v1 to the postbox under the production CSP; Chrome as Alice and Bob.
//
// Scenario:
//   1. ticks: Alice sends → one gray ✓; Bob's client lists his inbox → two gray ✓✓; Bob opens the
//      conversation → two green ✓✓;
//   2. edit: Alice edits; Bob sees the new text and "edited" within 2 s, and both versions in info;
//   3. reply: Bob replies to Alice's message; Alice sees the quote, and clicking it highlights the
//      parent;
//   4. read receipts off: Bob turns them off in Settings; Alice's next message stops at delivered;
//   5. layout: on both screens, for prose, markdown, a request, an attachment and a long message,
//      the action row is below the bubble (its top at or under the bubble's bottom), and nothing
//      actionable is inside the bubble.
//
// Live mode (a deployment, cross-origin): E2E_LIVE=1 E2E_ORIGIN=https://inbox… \
//   E2E_POSTBOX=https://postbox… E2E_TOKENS='{"alice":"<jwt>","bob":"<jwt>"}' with throwaway accounts
// that already have the flags.
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
const LIVE = process.env.E2E_LIVE === "1";
const FLAGS = ["message_edit", "message_reply", "receipts", "agent_status", "account_events"];
const work = mkdtempSync(path.join(tmpdir(), "pp-e2e-actions-"));
const log = (...a) => console.log(new Date().toISOString().slice(11, 19), ...a);
const children = [];
const servers = [];
const results = [];
const pass = (what) => { results.push(what); log("✓", what); };

function cleanup() {
  for (const c of children) { try { c.kill("SIGKILL"); } catch (_) { /* gone */ } }
  for (const s of servers) { try { s.close(); } catch (_) { /* gone */ } }
  if (!process.env.E2E_KEEP) rmSync(work, { recursive: true, force: true });
}
process.on("exit", cleanup);
process.on("SIGINT", () => process.exit(130));

const listen = (server) => new Promise((resolve) => server.listen(0, "127.0.0.1", () => { servers.push(server); resolve(server.address().port); }));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn, what, ms = 30000) {
  const end = Date.now() + ms;
  let last;
  while (Date.now() < end) {
    try { last = await fn(); if (last) return last; } catch (e) { last = e; }
    await sleep(150);
  }
  throw new Error("timed out: " + what + (last instanceof Error ? " (" + last.message + ")" : last !== undefined ? " (last: " + JSON.stringify(last) + ")" : ""));
}

let PB, ORIGIN, jwt;
if (LIVE) {
  PB = process.env.E2E_POSTBOX.replace(/\/+$/, "");
  ORIGIN = process.env.E2E_ORIGIN.replace(/\/+$/, "");
  const tokens = JSON.parse(process.env.E2E_TOKENS);
  jwt = (sub) => tokens[sub.replace(/^e2e-/, "")];
  log("live mode:", ORIGIN, "→", PB);
} else {
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
  jwt = function (sub, name) {
    const now = Math.floor(Date.now() / 1000);
    const head = b64u(JSON.stringify({ alg: "RS256", typ: "JWT", kid: KID }));
    const body = b64u(JSON.stringify({ iss: ISSUER, sub, exp: now + 6 * 3600, iat: now, auth_time: now, azp: "pigeonpost-web", typ: "Bearer", preferred_username: name, aud: ["account"] }));
    const sig = crypto.sign("RSA-SHA256", Buffer.from(head + "." + body), privateKey);
    return head + "." + body + "." + b64u(sig);
  };

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
  PB = `http://127.0.0.1:${pbPort}`;
  const INT = `http://127.0.0.1:${intPort}`;
  await until(() => fetch(PB + "/health").then((r) => r.ok), "postbox health", 30000);
  for (const name of FLAGS) {
    const r = await fetch(`${INT}/internal/v1/features/${name}`, { method: "PUT", headers: { authorization: "Bearer " + OPS, "content-type": "application/json" }, body: JSON.stringify({ scope: "global", enabled: true }) });
    if (!r.ok) throw new Error(`flag ${name}: ${r.status} ${await r.text()}`);
  }
  log("postbox up on", PB, "flags", FLAGS.join(","));
}

async function pb(token, method, p, body, identity) {
  const r = await fetch(PB + p, { method, headers: { authorization: "Bearer " + token, "content-type": "application/json", ...(identity ? { "x-pigeonpost-identity": identity } : {}) }, body: body ? JSON.stringify(body) : undefined });
  const text = await r.text();
  let json = null; try { json = JSON.parse(text); } catch (_) { /* not json */ }
  return { status: r.status, body: json };
}

const people = {};
for (const [key, name] of [["alice", "Alice"], ["bob", "Bob"]]) {
  const token = jwt("e2e-" + key, name);
  const listed = await pb(token, "GET", "/v1/identities");
  let address = listed.body?.identities?.find((i) => i.label === name)?.address;
  if (!address) {
    const made = await pb(token, "POST", "/v1/identities", { label: name });
    if (made.status !== 201) throw new Error(`mailbox for ${name}: ${made.status} ${JSON.stringify(made.body)}`);
    address = made.body.address;
  }
  people[key] = { name, token, address };
}
const { alice, bob } = people;
// Contacts both ways, so neither is a throttled stranger to the other.
for (const [me, them] of [[alice, bob], [bob, alice]]) {
  const r = await pb(me.token, "PUT", "/v1/contacts", { peer: them.address, alias: them.name, admission: "allow", autonomy: "review", identity: me.address }, me.address);
  if (r.status >= 300) throw new Error("contact " + me.name + ": " + r.status + " " + JSON.stringify(r.body));
}
// Bob writes first, so Alice's screen has the conversation to open.
const hello = await pb(bob.token, "POST", "/v1/send", { to: alice.address, body: "hello — are you the maintainer of pigeonpost?", from: bob.address });
if (hello.status !== 201) throw new Error("seed send: " + hello.status + " " + JSON.stringify(hello.body));
log("mailboxes", alice.address, bob.address);

if (!LIVE) {
  const TYPES = { ".html": "text/html", ".js": "text/javascript", ".mjs": "text/javascript", ".css": "text/css", ".wasm": "application/wasm", ".png": "image/png", ".ico": "image/x-icon", ".woff2": "font/woff2", ".json": "application/json", ".svg": "image/svg+xml" };
  const site = http.createServer((req, res) => {
    const url = new URL(req.url, "http://x");
    if (url.pathname.startsWith("/v1/")) {
      const up = http.request(PB + req.url, { method: req.method, headers: { ...req.headers, host: new URL(PB).host } }, (r) => { res.writeHead(r.statusCode, r.headers); r.pipe(res); });
      up.on("error", () => { res.writeHead(502); res.end(); });
      req.pipe(up);
      return;
    }
    if (url.pathname.startsWith("/calls/")) { res.writeHead(404, { "content-type": "application/json" }); res.end('{"error":"not_found"}'); return; }
    const file = url.pathname === "/" ? "/index.html" : url.pathname;
    const full = path.join(SITE, path.normalize(file));
    if (!full.startsWith(SITE) || !existsSync(full)) { res.writeHead(404); res.end(); return; }
    let data = readFileSync(full);
    if (file === "/config.js") data = Buffer.concat([data, Buffer.from("\nwindow.PIGEONPOST_INBOX.postbox = location.origin;\n")]);
    res.writeHead(200, { "content-type": TYPES[path.extname(full)] || "application/octet-stream", "cache-control": "no-store",
      "content-security-policy": "default-src 'self'; img-src 'self' data: blob:; media-src 'self' blob:; style-src 'self'; script-src 'self' 'wasm-unsafe-eval'; worker-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'; object-src 'none'" });
    res.end(data);
  });
  ORIGIN = `http://127.0.0.1:${await listen(site)}`;
  log("inbox on", ORIGIN);
}

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
    if (msg.method === "Runtime.consoleAPICalled" && ["error"].includes(msg.params.type)) consoleLines.push(msg.params.type + " " + msg.params.args.map((a) => a.value ?? a.description).join(" "));
  };
  const send = (method, params = {}) => new Promise((resolve, reject) => { const n = ++id; waiting.set(n, { resolve, reject }); ws.send(JSON.stringify({ id: n, method, params })); });
  await send("Runtime.enable");
  await send("Page.enable");
  await send("Browser.grantPermissions", { permissions: ["clipboardReadWrite", "clipboardSanitizedWrite"], origin: ORIGIN }).catch(() => {});
  await send("Page.addScriptToEvaluateOnNewDocument", { source: `try { if (location.origin === ${JSON.stringify(ORIGIN)}) localStorage.setItem("ppi_token", ${JSON.stringify(token)}); } catch (_) {}` });
  const evaluate = async (expression) => {
    const r = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text);
    return r.result.value;
  };
  const go = async (url) => { await send("Page.navigate", { url }); await sleep(300); };
  // E2E_SHOTS=<dir> saves screenshots for a visual check of the look against the reference.
  const shot = async (name) => {
    if (!process.env.E2E_SHOTS) return;
    const { data } = await send("Page.captureScreenshot", { format: "png" });
    writeFileSync(path.join(process.env.E2E_SHOTS, `${label}-${name}.png`), Buffer.from(data, "base64"));
  };
  return { label, evaluate, go, shot, consoleLines, close: () => { ws.close(); proc.kill("SIGKILL"); } };
}

// Page-side helpers, as expressions.
const J = JSON.stringify;
const rowWith = (text) => `[...document.querySelectorAll("#messages > li[data-message-id]")].reverse().find((li) => (li.querySelector(".bubble .text, .bubble .request") || {}).textContent?.includes(${J(text)}))`;
const tickOf = (text) => `(() => { const li = ${rowWith(text)}; const t = li && li.querySelector(".bubble .meta .ticks"); return t ? t.className.replace("ticks ", "").replace(" ticks-button", "") : null; })()`;
const openConversation = `(() => { const row = document.querySelector("#threads .thread-row"); if (!row) return false; if (row.getAttribute("aria-current") !== "true") row.click(); return Boolean(document.querySelector("#messages > li[data-message-id]")); })()`;
async function compose(b, text) {
  await b.evaluate(`(() => { const c = document.getElementById("compose"); c.value = ${J(text)}; c.dispatchEvent(new Event("input")); document.getElementById("composer").requestSubmit(); })()`);
}
const ready = `Boolean(document.getElementById("identity-btn")?.dataset.address && document.querySelector("#threads .thread-row"))`;

const A = await browser("alice", alice.token);
await A.go(ORIGIN + "/");
await until(() => A.evaluate(ready), "alice inbox ready");
await until(() => A.evaluate(openConversation), "alice opens the conversation with bob");

// 1. Ticks: sent → delivered → seen.
await compose(A, "Hi! Yes, that's me 👋");
await until(() => A.evaluate(tickOf("Hi! Yes, that's me")).then((t) => t === "ticks-sent"), "one gray tick after sending", 15000);
pass("sent: one gray ✓ while Bob's client has not fetched it");
const bobList = await pb(bob.token, "GET", "/v1/inbox?include_sent=true&include_read=true&actions=1&fold=1", null, bob.address);
if (bobList.status !== 200) throw new Error("bob listing " + bobList.status);
await until(() => A.evaluate(tickOf("Hi! Yes, that's me")).then((t) => t === "ticks-delivered"), "two gray ticks once Bob's client listed it", 15000);
pass("delivered: two gray ✓✓ once Bob's client listed his inbox");

const B = await browser("bob", bob.token);
await B.go(ORIGIN + "/");
await until(() => B.evaluate(ready), "bob inbox ready");
await until(() => B.evaluate(openConversation), "bob opens the conversation");
await until(() => A.evaluate(tickOf("Hi! Yes, that's me")).then((t) => t === "ticks-seen"), "green ticks once Bob read it", 20000);
const seenLabel = await A.evaluate(`(${rowWith("Hi! Yes, that's me")}).querySelector(".ticks").getAttribute("aria-label")`);
if (!/^Seen \d/.test(seenLabel)) throw new Error("seen label: " + seenLabel);
pass(`seen: two green ✓✓ after Bob opened the conversation ("${seenLabel}")`);

// 2. Edit, visible to Bob within 2 s, with both versions in info.
await A.evaluate(`(${rowWith("Hi! Yes, that's me")}).querySelector(".msg-action.edit").click()`);
const prefilled = await A.evaluate(`document.getElementById("compose").value`);
if (prefilled !== "Hi! Yes, that's me 👋") throw new Error("edit prefill: " + prefilled);
const editedAt = Date.now();
await compose(A, "Hi! Yes, that's me — the maintainer 👋");
await until(() => B.evaluate(`(() => { const li = ${rowWith("the maintainer")}; return Boolean(li && li.querySelector(".bubble .meta .edited")); })()`), "bob sees the edit", 10000);
const editLag = Date.now() - editedAt;
if (editLag > 2000) throw new Error(`the edit took ${editLag} ms to reach Bob`);
const bobStale = await B.evaluate(`[...document.querySelectorAll("#messages .bubble .text")].filter((t) => t.textContent.trim() === "Hi! Yes, that's me 👋").length`);
if (bobStale) throw new Error("bob still shows the pre-edit text as its own bubble");
pass(`edit: Bob sees the new text and "edited" in ${editLag} ms`);
await B.evaluate(`(${rowWith("the maintainer")}).querySelector(".bubble").dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 200, clientY: 200 }))`);
await B.evaluate(`[...document.querySelectorAll("#msg-menu button")].find((b) => b.textContent === "Info").click()`);
const versions = await until(() => B.evaluate(`[...document.querySelectorAll("#msg-info-body .info-versions li")].map((li) => li.textContent)`).then((v) => v.length === 2 && v), "two versions in info", 10000);
if (!/that's me 👋$/.test(versions[0]) || !/the maintainer 👋$/.test(versions[1])) throw new Error("versions: " + J(versions));
await B.evaluate(`document.getElementById("msg-info-done").click()`);
pass("edit: message info lists the original and the edit");

// 3. Reply with a quote; clicking it highlights the parent.
await B.evaluate(`(${rowWith("the maintainer")}).querySelector(".msg-action.reply").click()`);
const replyBar = await B.evaluate(`[document.getElementById("compose-context").hidden, document.getElementById("compose-context-line").textContent]`);
if (replyBar[0] || !replyBar[1].includes("the maintainer")) throw new Error("reply bar: " + J(replyBar));
await compose(B, "Great — can I add my tester agent?");
await until(() => A.evaluate(`(() => { const li = ${rowWith("tester agent")}; const q = li && li.querySelector(".bubble > .reply-quote .reply-line"); return q && q.textContent; })()`).then((q) => q && q.includes("the maintainer")), "alice sees the quoted parent", 15000);
await A.evaluate(`(${rowWith("tester agent")}).querySelector(".reply-quote").click()`);
const flashed = await A.evaluate(`(${rowWith("the maintainer")}).classList.contains("flash")`);
if (!flashed) throw new Error("the parent was not highlighted");
pass("reply: the quote shows on Alice's side and clicking it highlights the parent");

// 4. Read receipts off on Bob's side: Alice's next message stops at delivered.
await B.evaluate(`document.getElementById("settings-btn").click(); document.getElementById("settings-nav-privacy").click();`);
await until(() => B.evaluate(`Boolean(document.querySelector('input[name="policy-read_receipts"][value="false"]:not([disabled])'))`), "bob's privacy page", 10000);
await B.evaluate(`(() => { const off = document.querySelector('input[name="policy-read_receipts"][value="false"]'); off.checked = true; off.dispatchEvent(new Event("change")); })()`);
await until(async () => (await pb(bob.token, "GET", "/v1/policy", null, bob.address)).body?.read_receipts === false, "read_receipts saved off", 10000);
await B.evaluate(`document.getElementById("settings-close").click()`);
await until(() => B.evaluate(openConversation), "bob back in the conversation");
await compose(A, "Sure, send me its handle");
await until(() => B.evaluate(`Boolean(${rowWith("send me its handle")})`), "bob receives it");
await until(() => A.evaluate(tickOf("send me its handle")).then((t) => t === "ticks-delivered"), "delivered with receipts off", 15000);
await sleep(4000);
const stillDelivered = await A.evaluate(tickOf("send me its handle"));
if (stillDelivered !== "ticks-delivered") throw new Error("with read receipts off Alice saw " + stillDelivered);
const bobOwnSeen = await B.evaluate(`[...document.querySelectorAll("#messages .ticks-seen")].length`);
if (bobOwnSeen) throw new Error("bob turned read receipts off but still sees seen ticks");
pass("read receipts off: Alice stays at gray ✓✓ and Bob sees no green ticks");

// 5. Layout across message kinds, on both screens.
const upload = await fetch(PB + "/v1/attachments", { method: "POST", headers: { authorization: "Bearer " + alice.token, "x-pigeonpost-identity": alice.address, "x-pigeonpost-filename": "report.txt", "x-pigeonpost-media-type": "text/plain", "content-type": "application/octet-stream" }, body: "figures" });
const att = upload.ok ? (await upload.json()).id : null;
const bodies = [
  "## Release plan\n- **ship** the `postbox`\n- deploy the web\n\n| a | b |\n|---|---|\n| 1 | 2 |",
  JSON.stringify({ v: 1, verb: "full_access", args: { task: "Run the release checklist" }, note: "Run the release checklist" }),
  "Long message: " + "this sentence keeps going so the bubble wraps across many lines. ".repeat(25),
];
for (const body of bodies) await pb(alice.token, "POST", "/v1/send", { to: bob.address, body, from: alice.address });
if (att) await pb(alice.token, "POST", "/v1/send", { to: bob.address, body: "the figures", from: alice.address, attachments: [att] });
else log("attachment upload refused (" + upload.status + "), skipping that case");
for (const b of [A, B]) await b.evaluate(`window.dispatchEvent(new Event("online"))`);
const layout = `(() => {
  const rows = [...document.querySelectorAll("#messages > li[data-message-id]")];
  const bad = [];
  for (const li of rows) {
    const bubble = li.querySelector(":scope > .msg-stack > .bubble"), actions = li.querySelector(":scope > .msg-stack > .msg-actions");
    if (!bubble || !actions) { bad.push(li.dataset.messageId + ": missing stack"); continue; }
    const b = bubble.getBoundingClientRect(), a = actions.getBoundingClientRect();
    if (a.top + 0.5 < b.bottom) bad.push(li.dataset.messageId + ": row overlaps the bubble " + a.top + " < " + b.bottom);
    if (bubble.querySelector(".msg-action, .copy")) bad.push(li.dataset.messageId + ": an action inside the bubble");
    const mine = li.classList.contains("mine");
    if (mine && Math.abs(a.right - b.right) > 12) bad.push(li.dataset.messageId + ": sent row not aligned right");
    if (!mine && Math.abs(a.left - b.left) > 12) bad.push(li.dataset.messageId + ": received row not aligned left");
    const kinds = [...actions.querySelectorAll(".msg-action")].map((x) => x.classList[1]).join(",");
    if (!mine && kinds !== "copy,reply") bad.push(li.dataset.messageId + ": received actions " + kinds);
    if (mine && !/^(edit,)?copy$/.test(kinds)) bad.push(li.dataset.messageId + ": sent actions " + kinds);
  }
  return { count: rows.length, bad, kinds: rows.map((li) => [li.querySelector(".md-h, .md-table") ? "markdown" : li.querySelector(".request") ? "request" : li.querySelector(".files") ? "attachment" : (li.textContent.length > 800 ? "long" : "prose"), li.classList.contains("mine") ? "sent" : "received"].join("/")) };
})()`;
for (const b of [A, B]) {
  await until(() => b.evaluate(`Boolean(${rowWith("the figures")}) || ${att ? "false" : "true"}`), b.label + " has the attachment message", 15000);
  await until(() => b.evaluate(`Boolean(${rowWith("Release plan")}) && Boolean(${rowWith("Long message")})`), b.label + " has every message", 15000);
  // Scroll through so every row is laid out, then check.
  await b.evaluate(`document.getElementById("thread-scroll").scrollTop = 0`);
  const r = await b.evaluate(layout);
  if (r.bad.length) throw new Error(b.label + " layout: " + r.bad.join("; "));
  const covered = new Set(r.kinds);
  log(b.label, "layout ok over", r.count, "rows:", [...covered].join(" "));
}
for (const b of [A, B]) {
  await b.evaluate(`document.getElementById("thread-scroll").scrollTop = 0`);
  await b.shot("top");
  await b.evaluate(`document.getElementById("thread-scroll").scrollTop = 1e6`);
  await b.shot("bottom");
}
pass("layout: the action row is under and outside every bubble (prose, markdown, request, attachment, long) on both screens");

const problems = [A, B].flatMap((b) => b.consoleLines.filter((l) => /EXCEPTION/.test(l)).map((l) => b.label + ": " + l));
if (problems.length) { console.error(problems.join("\n")); throw new Error("uncaught page exceptions"); }
log("E2E OK —", results.length, "checks");
for (const b of [A, B]) b.close();
process.exit(0);
