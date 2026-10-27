// Other engines watching one stream (docs/streaming.md, "Browser and app support"): starts the same
// local stack as e2e.mjs with a streamer in headless Chrome, then waits while viewers in other
// engines (iOS Simulator Safari, an Android emulator's Chrome, a macOS WKWebView) open
//   http://<host>:8561/__test/viewer.html?engine=<name>&who=v<n>&stream=<id>&group=<gid>&pb=http://<host>:8891
// and report what they decoded. Synthetic media only.
//   node engines.mjs [minutes=6]
import { spawn } from "node:child_process";
import { writeFileSync, readFileSync } from "node:fs";
import path from "node:path";
import puppeteer from "puppeteer-core";
import { createMockPostbox } from "./mock-postbox.mjs";
import { createStatic } from "./serve.mjs";

const REPO = process.env.REPO || path.resolve(path.dirname(new URL(import.meta.url).pathname), "../..");
const HERE = path.dirname(new URL(import.meta.url).pathname);
const MINUTES = Number(process.argv[2] || 6);
const SECRET = "x".repeat(48);
const P = { lk: 7980, calls: 8560, pb: 8891, web: 8561 };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const procs = [];
const lkYaml = path.join(process.env.TMPDIR || "/tmp", "pp-streams-livekit-engines.yaml");
writeFileSync(lkYaml, `port: ${P.lk}\nbind_addresses: ["127.0.0.1"]\nrtc:\n  tcp_port: 7981\n  port_range_start: 50100\n  port_range_end: 50400\n  use_external_ip: false\n  node_ip: ${process.env.NODE_IP || "127.0.0.1"}\nkeys:\n  APIlocal: ${SECRET}\nlogging:\n  level: warn\n`);
procs.push(spawn("livekit-server", ["--config", lkYaml], { stdio: "ignore" }));
const members = { "/k/host": "owner" };
for (let i = 0; i < 20; i++) members["/k/v" + i] = "member";
const pb = createMockPostbox({ members });
await new Promise((r) => pb.server.listen(P.pb, "0.0.0.0", r));
const origins = ["127.0.0.1", "localhost", "10.0.2.2", process.env.NODE_IP].filter(Boolean).map((h) => `http://${h}:${P.web}`).join(",");
procs.push(spawn("node", [path.join(REPO, "calls-service/src/server.js")], { stdio: "inherit", env: { ...process.env,
  CALLS_PORT: String(P.calls), CALLS_POSTBOX_URL: `http://127.0.0.1:${P.pb}`, CALLS_ALLOWED_ORIGINS: origins,
  POSTBOX_INTERNAL_URL: `http://127.0.0.1:${P.pb}`, POSTBOX_SERVICE_TOKEN: "svc_calls_test",
  CALLS_SFU_URL: `ws://${process.env.SFU_HOST || "127.0.0.1"}:${P.lk}`, CALLS_SFU_API_URL: `http://127.0.0.1:${P.lk}`, CALLS_SFU_KEY: "APIlocal", CALLS_SFU_SECRET: SECRET,
  CALLS_SFU_BUDGET_MBPS: "400", CALLS_STREAM_START_RESERVE: "1" } }));
const reports = [];
const viewerHtml = readFileSync(path.join(HERE, "viewer.html"));
const web = createStatic(path.join(REPO, "site-inbox"), P.calls, { routes: {
  "/__test/viewer.html": (req, res) => { res.writeHead(200, { "content-type": "text/html", "cache-control": "no-store" }); res.end(viewerHtml); },
  "/__report": (req, res) => { let d = ""; req.on("data", (c) => (d += c)); req.on("end", () => { try { const r = JSON.parse(d); reports.push(r); console.log("REPORT", r.engine, r.phase, JSON.stringify(r.inspect ? { epoch: r.inspect.epoch, decryptErrors: r.inspect.decryptErrors, viewers: r.inspect.viewers } : r.result || r.error || r.e2ee), JSON.stringify((r.stats || []).map((s) => [s.source, s.encrypted, s.frames, s.width, s.height]))); } catch {} res.writeHead(204); res.end(); }); },
} });
await new Promise((r) => web.listen(P.web, "0.0.0.0", r));
for (let i = 0; i < 40; i++) { try { await fetch(`http://127.0.0.1:${P.lk}/`); break; } catch { await sleep(500); } }
await sleep(1000);
const browser = await puppeteer.launch({ executablePath: process.env.CHROME || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", headless: true, args: ["--autoplay-policy=no-user-gesture-required", "--disable-background-timer-throttling", "--disable-renderer-backgrounding", "--use-fake-ui-for-media-stream"] });
const host = await browser.newPage();
host.on("console", (m) => { if (process.env.VERBOSE) console.log("[host]", m.text()); });
await host.evaluateOnNewDocument(readFileSync(path.join(HERE, "inject.js"), "utf8"));
await host.goto(`http://127.0.0.1:${P.web}/native-call.html`);
await host.evaluate(() => window.PigeonpostNative.configure({ token: "tok-host", mailbox: { address: "/k/host", handle: null } }));
const live = await host.evaluate((g) => window.PigeonpostNative.goLive(g, { title: "Engines", camera: true }), { id: pb.GID, name: "Crew" });
console.log("LIVE", JSON.stringify(live));
console.log(`VIEWER URL: http://HOST:${P.web}/__test/viewer.html?engine=NAME&who=v1&stream=${live.stream}&group=${pb.GID}&pb=http://HOST:${P.pb}`);
writeFileSync(path.join(process.env.TMPDIR || "/tmp", "pp-streams-engines-url.txt"), `stream=${live.stream}&group=${pb.GID}`);
const end = Date.now() + MINUTES * 60_000;
while (Date.now() < end) { await sleep(5000); if (process.env.STOP_AFTER && reports.filter((r) => r.phase === "stats" && r.i === 5).length >= Number(process.env.STOP_AFTER)) break; }
writeFileSync(path.join(process.env.TMPDIR || "/tmp", "pp-streams-engines.json"), JSON.stringify(reports, null, 2));
await browser.close(); web.close(); pb.server.close();
for (const p of procs) p.kill("SIGTERM");
process.exit(0);
