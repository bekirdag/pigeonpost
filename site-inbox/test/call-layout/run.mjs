#!/usr/bin/env node
// The call screen's layout, checked in a real browser: no tile's pin or mute button may sit on a
// header control ("Inbox", the security code, Grid, full screen) or on the call's own controls.
//
// Two people call each other through the real call service (run in this process, against a
// stand-in postbox) in headless Chrome, with synthetic camera, microphone and screen — nothing real
// is ever captured. Alice is the native apps' page (native-call.html, which has the "Inbox" button);
// Bob is the browser inbox (index.html). Each side is measured in three layouts:
//
//   solo       one person on the stage, their tile pinned so its buttons always show
//   spotlight  the other shares their screen: it takes the stage, their camera goes to the strip
//   grid       the same two views side by side ("Grid")
//
// at 1440x900, 1180x820 (iPad landscape) and 390x844 (phone). Screenshots of every state are
// written to OUT (default: a temporary folder) for review.
//
//   cd site-inbox && npm run test:call-layout
//   CHROME=/path/to/chrome OUT=/tmp/call-layout node test/call-layout/run.mjs
//
// Exits 1 and lists every intersecting pair when anything overlaps.
import http from "node:http";
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { configFromEnv, createCallsService } from "../../../calls-service/src/server.js";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(HERE, "../..");
const OUT = process.env.OUT || mkdtempSync(path.join(os.tmpdir(), "call-layout-"));
mkdirSync(OUT, { recursive: true });
const VIEWPORTS = [
  { name: "1440x900", width: 1440, height: 900, mobile: false },
  { name: "1180x820", width: 1180, height: 820, mobile: false },
  { name: "390x844", width: 390, height: 844, mobile: true },
];

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn, what, ms = 30000) {
  const start = Date.now();
  for (;;) {
    let value;
    try { value = await fn(); } catch (_) { value = false; }
    if (value) return value;
    if (Date.now() - start > ms) throw new Error("timed out waiting for " + what);
    await sleep(200);
  }
}

// ---- the stand-in postbox the call service asks who a token belongs to --------------------------
const ACCOUNTS = {
  "tok-alice": { address: "/k/alice0000000000000000000a", handle: "/alice" },
  "tok-bob": { address: "/k/bob00000000000000000000000b", handle: "/bob" },
};
async function postbox(url, opts = {}) {
  const u = new URL(String(url));
  const headers = opts.headers instanceof Headers ? Object.fromEntries(opts.headers) : (opts.headers || {});
  const auth = headers.authorization || headers.Authorization || "";
  const who = ACCOUNTS[auth.replace(/^Bearer /, "")];
  const reply = (status, body) => new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
  if (!who) return reply(401, { error: "unauthorized" });
  if (u.pathname === "/v1/identities") return reply(200, { identities: [{ address: who.address, label: null }] });
  if (u.pathname === "/v1/whoami") return u.searchParams.get("identity") === who.address ? reply(200, who) : reply(403, {});
  return reply(404, {});
}

// ---- the site, with the stand-ins injected, and the call service under /calls/ -----------------
const TYPES = { ".html": "text/html", ".css": "text/css", ".js": "application/javascript", ".mjs": "application/javascript", ".png": "image/png", ".woff2": "font/woff2", ".wasm": "application/wasm", ".ico": "image/x-icon", ".json": "application/json" };
let calls = null;
// The call service narrates every connection; this check only reports layout.
const log = console.log;
console.log = (...args) => { if (!String(args[0]).startsWith("calls:")) log(...args); };
console.warn = (...args) => { if (!String(args[0]).startsWith("calls:")) log(...args); };
const server = http.createServer(async (req, res) => {
  const p = decodeURIComponent(new URL(req.url, "http://x").pathname);
  if (p.startsWith("/calls/")) return calls.handle(req, res);
  try {
    if (p === "/__fixture.js") { res.writeHead(200, { "content-type": TYPES[".js"] }); return res.end(readFileSync(path.join(HERE, "fixture.js"))); }
    if (p === "/__media.js") { res.writeHead(200, { "content-type": TYPES[".js"] }); return res.end(readFileSync(path.join(HERE, "synthetic-media.js"))); }
    const file = path.join(ROOT, p === "/" ? "index.html" : p);
    if (!file.startsWith(ROOT + path.sep)) { res.writeHead(403); return res.end(); }
    let body = readFileSync(file);
    if (p === "/" || p === "/index.html") {
      body = String(body).replace('<script src="config.js"></script>', '<script src="/__fixture.js"></script><script src="/__media.js"></script><script src="config.js"></script>');
    } else if (p === "/native-call.html") {
      body = String(body).replace('<body data-page="native">', '<body data-page="native"><script src="/__media.js"></script>');
    }
    res.writeHead(200, { "content-type": TYPES[path.extname(file)] || "application/octet-stream" });
    res.end(body);
  } catch (_) { res.writeHead(404); res.end(); }
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const ORIGIN = `http://127.0.0.1:${server.address().port}`;
calls = createCallsService(configFromEnv({ CALLS_POSTBOX_URL: "http://postbox.test", CALLS_ALLOWED_ORIGINS: ORIGIN, CALLS_TRUST_PROXY: "0" }), { fetch: postbox });

// ---- headless Chrome over the DevTools protocol ---------------------------------------------------
function findChrome() {
  const candidates = [process.env.CHROME,
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/usr/bin/google-chrome", "/usr/bin/google-chrome-stable", "/usr/bin/chromium", "/usr/bin/chromium-browser"];
  return candidates.find((c) => c && existsSync(c));
}
async function launchChrome() {
  const chrome = findChrome();
  if (!chrome) throw new Error("no Chrome found; set CHROME=/path/to/chrome");
  const dir = mkdtempSync(path.join(os.tmpdir(), "call-layout-profile-"));
  const proc = spawn(chrome, ["--headless=new", "--remote-debugging-port=0", `--user-data-dir=${dir}`, "--no-first-run", "--no-default-browser-check",
    "--use-fake-ui-for-media-stream", "--autoplay-policy=no-user-gesture-required", "--disable-gpu", ...(process.platform === "linux" ? ["--no-sandbox"] : []), "about:blank"],
  { stdio: ["ignore", "ignore", "pipe"] });
  const wsUrl = await new Promise((resolve, reject) => {
    let err = "";
    proc.stderr.on("data", (d) => { err += d; const m = /DevTools listening on (ws:\S+)/.exec(err); if (m) resolve(m[1]); });
    proc.on("exit", (code) => reject(new Error("Chrome exited " + code + ": " + err.slice(-400))));
  });
  const ws = new WebSocket(wsUrl);
  await new Promise((r, j) => { ws.addEventListener("open", r); ws.addEventListener("error", j); });
  let id = 0; const pending = new Map();
  ws.addEventListener("message", (e) => {
    const m = JSON.parse(e.data);
    if (m.id && pending.has(m.id)) { const { res, rej } = pending.get(m.id); pending.delete(m.id); if (m.error) rej(new Error(JSON.stringify(m.error))); else res(m.result); }
  });
  const send = (method, params = {}, sessionId) => new Promise((res, rej) => {
    const i = ++id; pending.set(i, { res, rej });
    ws.send(JSON.stringify({ id: i, method, params, ...(sessionId ? { sessionId } : {}) }));
  });
  // Each person in a browser context of their own: separate storage, as two devices would be.
  async function page(url) {
    const { browserContextId } = await send("Target.createBrowserContext");
    const { targetId } = await send("Target.createTarget", { url: "about:blank", browserContextId });
    const { sessionId } = await send("Target.attachToTarget", { targetId, flatten: true });
    const s = (m, p) => send(m, p, sessionId);
    await s("Page.enable"); await s("Runtime.enable");
    const evaluate = async (expr) => {
      const r = await s("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true });
      if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails).slice(0, 400));
      return r.result.value;
    };
    const viewport = async (v) => {
      await s("Emulation.setDeviceMetricsOverride", { width: v.width, height: v.height, deviceScaleFactor: 1, mobile: v.mobile });
      await s("Emulation.setTouchEmulationEnabled", { enabled: v.mobile, maxTouchPoints: v.mobile ? 5 : 1 });
    };
    const shot = async (file) => writeFileSync(file, Buffer.from((await s("Page.captureScreenshot", { format: "png" })).data, "base64"));
    await viewport(VIEWPORTS[0]);
    await s("Page.navigate", { url });
    return { evaluate, viewport, shot };
  }
  return { page, close: () => { try { ws.close(); } catch (_) {} proc.kill(); } };
}

// Every visible tile button against every header control and call control, in page coordinates.
const MEASURE = `(() => {
  const ui = document.getElementById("call-ui");
  if (!ui || ui.hidden) return { error: "call screen not shown" };
  const box = (e) => {
    const r = e.getBoundingClientRect(), cs = getComputedStyle(e);
    if (r.width < 1 || r.height < 1 || cs.display === "none" || cs.visibility === "hidden") return null;
    return { x: r.left, y: r.top, r: r.right, b: r.bottom };
  };
  const label = (e) => (e.getAttribute("aria-label") || e.id || e.className || e.tagName).toString().slice(0, 48);
  const acts = [...ui.querySelectorAll(".call-tile-act")].map((e) => ({ e, b: box(e), strip: Boolean(e.closest(".call-strip")), tile: e.closest(".call-tile") })).filter((a) => a.b);
  const header = [...ui.querySelectorAll(".call-top button, .call-top .call-sas")].map((e) => ({ e, b: box(e) })).filter((g) => g.b);
  // "You're sharing your screen", while it shows: clear of the header, and the tiles' buttons clear of it.
  const presenting = [...ui.querySelectorAll(".call-presenting")].map((e) => ({ e, b: box(e) })).filter((g) => g.b);
  const guarded = [...header, ...presenting, ...[...ui.querySelectorAll(".call-controls button")].map((e) => ({ e, b: box(e) })).filter((g) => g.b)];
  const hit = (a, b) => a.x < b.r - 0.5 && b.x < a.r - 0.5 && a.y < b.b - 0.5 && b.y < a.b - 0.5;
  const problems = [];
  for (const a of acts) {
    for (const g of guarded) if (hit(a.b, g.b)) problems.push(label(a.e) + " overlaps " + label(g.e));
    // A button must also lie inside its own tile and the window (a strip may scroll; its tile clips).
    const t = box(a.tile);
    if (!a.strip && t && (a.b.x < t.x - 0.5 || a.b.y < t.y - 0.5 || a.b.r > t.r + 0.5 || a.b.b > t.b + 0.5)) problems.push(label(a.e) + " outside its tile");
    if (!a.strip && (a.b.x < 0 || a.b.y < 0 || a.b.r > innerWidth || a.b.b > innerHeight)) problems.push(label(a.e) + " outside the window");
  }
  for (const p of presenting) for (const h of header) if (hit(p.b, h.b)) problems.push("the sharing banner overlaps " + label(h.e));
  const grid = document.getElementById("call-grid");
  return { layout: grid.dataset.layout, count: grid.dataset.count, acts: acts.length, guarded: guarded.length,
    topH: getComputedStyle(ui).getPropertyValue("--call-top-h").trim(), problems };
})()`;

const failures = [];
const report = [];
let browser;
try {
  browser = await launchChrome();
  const alice = await browser.page(`${ORIGIN}/native-call.html?as=alice`);
  const bob = await browser.page(`${ORIGIN}/?as=bob`);
  await until(() => alice.evaluate(`Boolean(window.PigeonpostNative && window.PigeonpostCalls)`), "the native call page");
  await until(() => bob.evaluate(`Boolean(window.PigeonpostCalls && PigeonpostCalls.inspect().session)`), "Bob's inbox to sign in to calls");
  await alice.evaluate(`PigeonpostNative.configure({ token: "tok-alice", mailbox: { address: "/k/alice0000000000000000000a", handle: "/alice" }, known: { "/bob": "/bob" } })`);
  await until(() => alice.evaluate(`Boolean(PigeonpostCalls.inspect().session)`), "Alice's page to sign in to calls");
  await alice.evaluate(`PigeonpostNative.call("/bob", "video")`);
  await until(() => bob.evaluate(`!document.getElementById("call-ring").hidden`), "Bob's phone to ring");
  await bob.evaluate(`document.getElementById("call-ring-accept").click()`);
  const people = { alice, bob };
  const connected = (p) => p.evaluate(`(PigeonpostCalls.inspect().peers || []).some((x) => x.connected)`);
  await until(async () => (await connected(alice)) && (await connected(bob)), "the call to connect");
  await sleep(1500);

  async function check(state, expect) {
    for (const v of VIEWPORTS) {
      for (const [who, p] of Object.entries(people)) {
        await p.viewport(v);
        await sleep(500);
        const m = await p.evaluate(MEASURE);
        const name = `${who}-${state}-${v.name}`;
        await p.shot(path.join(OUT, name + ".png"));
        report.push({ name, ...m });
        if (m.error) { failures.push(`${name}: ${m.error}`); continue; }
        if (m.layout !== expect.layout || m.count !== expect.count) failures.push(`${name}: expected ${expect.layout}/${expect.count}, got ${m.layout}/${m.count}`);
        if (!m.acts) failures.push(`${name}: no tile buttons were measured`);
        for (const problem of m.problems) failures.push(`${name}: ${problem}`);
      }
    }
    for (const p of Object.values(people)) await p.viewport(VIEWPORTS[0]);
  }

  // Solo: the one tile pinned, so its buttons stay shown as they did in the reported screenshot.
  for (const p of Object.values(people)) await p.evaluate(`document.querySelector(".call-tile .call-pin").click()`);
  await check("solo", { layout: "grid", count: "1" });
  for (const p of Object.values(people)) await p.evaluate(`document.querySelector(".call-tile .call-pin").click()`);

  // Spotlight: both share a (synthetic) screen; each sees the other's screen on the stage.
  for (const p of Object.values(people)) await p.evaluate(`document.getElementById("call-share").click()`);
  for (const p of Object.values(people)) await until(() => p.evaluate(`document.getElementById("call-grid").dataset.layout === "spotlight"`), "a shared screen to take the stage");
  await sleep(800);
  await check("spotlight", { layout: "spotlight", count: "2" });

  // Grid: the same two views side by side.
  for (const p of Object.values(people)) await p.evaluate(`document.getElementById("call-layout").click()`);
  await check("grid", { layout: "grid", count: "2" });

  await bob.evaluate(`document.getElementById("call-hangup").click()`);
} catch (error) {
  failures.push("harness: " + (error && error.stack || error));
} finally {
  browser?.close();
  server.close();
}

for (const r of report) console.log(`${r.problems && r.problems.length ? "FAIL" : "ok  "} ${r.name.padEnd(28)} ${r.layout}/${r.count} tile buttons ${r.acts} header ${r.topH}`);
console.log(`screenshots: ${OUT}`);
if (failures.length) {
  console.error(`\n${failures.length} problem(s):\n  ` + failures.join("\n  "));
  process.exit(1);
}
console.log("no overlaps");
process.exit(0);
