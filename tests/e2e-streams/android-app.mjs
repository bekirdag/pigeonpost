// The Android app watching a group live stream (docs/streaming.md) through its own call web view,
// end to end, with synthetic media only: local LiveKit, the real calls service, the mock postbox,
// the real call page (site-inbox/native-call.html + live.js) and a streamer in headless Chrome. The
// app runs its `live` fixture with stream reads proxied to the mock postbox, so a stage invitation
// the streamer makes reaches the app the way it does in production (the app reads the postbox and
// tells the page). Driven by the instrumented LiveStreamE2eTest.
//
//   npm i --no-save puppeteer-core@24.10.0
//   ANDROID_SERIAL=emulator-5580 node android-app.mjs     (debug app + test APK installed)
import { spawn, execFileSync } from "node:child_process";
import { writeFileSync, readFileSync } from "node:fs";
import path from "node:path";
import puppeteer from "puppeteer-core";
import { createMockPostbox } from "./mock-postbox.mjs";
import { createStatic } from "./serve.mjs";

const REPO = process.env.REPO || path.resolve(path.dirname(new URL(import.meta.url).pathname), "../..");
const HERE = path.dirname(new URL(import.meta.url).pathname);
const ADB = process.env.ADB || "adb";
const SECRET = "x".repeat(48);
const P = { lk: 7980, calls: 8560, pb: 8891, web: 8561 };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const procs = [];
const failures = [];
const check = (ok, what) => { if (!ok) failures.push(what); console.log((ok ? "ok   " : "FAIL ") + what); };
const lkYaml = path.join(process.env.TMPDIR || "/tmp", "pp-streams-livekit-android.yaml");
writeFileSync(lkYaml, `port: ${P.lk}\nbind_addresses: ["127.0.0.1"]\nrtc:\n  tcp_port: 7981\n  port_range_start: 50100\n  port_range_end: 50400\n  use_external_ip: false\n  node_ip: 127.0.0.1\nkeys:\n  APIlocal: ${SECRET}\nlogging:\n  level: warn\n`);
procs.push(spawn("livekit-server", ["--config", lkYaml], { stdio: "ignore" }));
const members = { "/k/host": "owner" };
for (let i = 0; i < 4; i++) members["/k/v" + i] = "member";
const pb = createMockPostbox({ members });
await new Promise((r) => pb.server.listen(P.pb, "127.0.0.1", r));
procs.push(spawn("node", [path.join(REPO, "calls-service/src/server.js")], { stdio: process.env.VERBOSE ? "inherit" : "ignore", env: { ...process.env,
  CALLS_PORT: String(P.calls), CALLS_POSTBOX_URL: `http://127.0.0.1:${P.pb}`, CALLS_ALLOWED_ORIGINS: `http://127.0.0.1:${P.web}`,
  POSTBOX_INTERNAL_URL: `http://127.0.0.1:${P.pb}`, POSTBOX_SERVICE_TOKEN: "svc_calls_test",
  CALLS_SFU_URL: `ws://127.0.0.1:${P.lk}`, CALLS_SFU_API_URL: `http://127.0.0.1:${P.lk}`, CALLS_SFU_KEY: "APIlocal", CALLS_SFU_SECRET: SECRET,
  CALLS_SFU_BUDGET_MBPS: "400", CALLS_STREAM_START_RESERVE: "1" } }));
const reports = [];
// The app's call page, told where the (mock) postbox is, as the production page knows its own.
const nativeHtml = readFileSync(path.join(REPO, "site-inbox/native-call.html"), "utf8")
  .replace("<body data-page=\"native\">", `<body data-page="native"><script>window.PIGEONPOST_INBOX={postbox:"http://127.0.0.1:${P.pb}"};</script>`);
const web = createStatic(path.join(REPO, "site-inbox"), P.calls, { routes: {
  "/native-call.html": (req, res) => { res.writeHead(200, { "content-type": "text/html", "cache-control": "no-store" }); res.end(nativeHtml); },
  "/__report": (req, res) => { let d = ""; req.on("data", (c) => (d += c)); req.on("end", () => { try { const r = JSON.parse(d); reports.push(r); console.log("REPORT", r.phase, JSON.stringify(r.inspect ? { epoch: r.inspect.epoch, role: r.inspect.role, decryptErrors: r.inspect.decryptErrors, unencrypted: r.inspect.unencrypted } : r), JSON.stringify((r.stats || []).map((s) => [s.source, s.encrypted, s.frames, s.width, s.height]))); } catch {} res.writeHead(204); res.end(); }); },
} });
await new Promise((r) => web.listen(P.web, "127.0.0.1", r));
for (let i = 0; i < 40; i++) { try { await fetch(`http://127.0.0.1:${P.lk}/`); break; } catch { await sleep(500); } }
await sleep(1000);
for (const port of [P.web, P.pb, P.calls, P.lk, 7981]) execFileSync(ADB, ["reverse", `tcp:${port}`, `tcp:${port}`]);

const browser = await puppeteer.launch({ executablePath: process.env.CHROME || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", headless: true,
  args: ["--autoplay-policy=no-user-gesture-required", "--disable-background-timer-throttling", "--disable-renderer-backgrounding", "--use-fake-ui-for-media-stream", "--use-fake-device-for-media-stream"] });
let instrument;
try {
  const host = await browser.newPage();
  await host.evaluateOnNewDocument(readFileSync(path.join(HERE, "inject.js"), "utf8"));
  await host.goto(`http://127.0.0.1:${P.web}/native-call.html`);
  await host.evaluate(() => window.PigeonpostNative.configure({ token: "tok-host", mailbox: { address: "/k/host", handle: null } }));
  const live = await host.evaluate((g) => window.PigeonpostNative.goLive(g, { title: "Seed swap", camera: true }), { id: pb.GID, name: "Garden club" });
  check(live && live.ok && /^str_/.test(live.stream), "the streamer is live: " + JSON.stringify(live));
  // Start the app's instrumented test against this stream; it runs while this script reacts to it.
  instrument = spawn(ADB, ["shell", "am", "instrument", "-w", "-e", "class", "dev.pigeonpost.inbox.LiveStreamE2eTest",
    "-e", "streamPage", `http://127.0.0.1:${P.web}/native-call.html`, "-e", "stream", live.stream, "-e", "group", pb.GID, "-e", "postbox", `http://127.0.0.1:${P.pb}`,
    "dev.pigeonpost.inbox.debug.test/dev.pigeonpost.inbox.PigeonpostTestRunner"], { stdio: ["ignore", "pipe", "inherit"] });
  let out = ""; instrument.stdout.on("data", (d) => (out += d));
  const done = new Promise((r) => instrument.on("exit", r));
  const until = async (fn, what, ms = 180000) => { const t = Date.now(); while (!fn()) { if (Date.now() - t > ms) throw new Error("timed out: " + what); await sleep(500); } };
  await until(() => reports.filter((r) => r.phase === "stats").length >= 3, "the app's stream statistics");
  const last = reports.filter((r) => r.phase === "stats").at(-1);
  const video = (last.stats || []).find((s) => s.source === "camera");
  check(video && video.encrypted && video.frames > 30 && video.width >= 320, `the app decodes the encrypted camera: ${JSON.stringify(video)}`);
  check(last.inspect.decryptErrors === 0 && last.inspect.unencrypted === 0, "no decryption errors and nothing unencrypted");
  check(last.inspect.role === "viewer", "the app watches as a viewer (it cannot publish)");
  check(last.service === true, "the app's foreground service runs while the stream plays");
  // The streamer invites the app's mailbox onto the stage, at the postbox.
  await host.evaluate(() => window.PigeonpostLive.setStage("/k/v1", true));
  await until(() => reports.some((r) => r.phase === "stage_offer"), "the stage invitation in the app's page", 120000);
  check(/invited you onto the stage/.test(reports.find((r) => r.phase === "stage_offer").text), "the app's page offers the stage (read from the postbox by the app)");
  await host.evaluate(() => window.PigeonpostLive.stop());
  await until(() => reports.some((r) => r.phase === "ended"), "the app to close the ended stream", 120000);
  check(reports.find((r) => r.phase === "ended").service === false, "the foreground service stopped with the stream");
  await done;
  check(/OK \(1 test\)/.test(out), "the instrumented test passed: " + (out.match(/OK \(.*\)|FAILURES!!!.*|Tests run.*/) || [out.slice(-300)])[0]);
} catch (e) {
  failures.push("harness: " + (e && e.stack || e));
} finally {
  instrument?.kill();
  await browser.close(); web.close(); pb.server.close();
  for (const p of procs) p.kill("SIGTERM");
}
if (failures.length) { console.log(`\n${failures.length} problem(s):\n  ` + failures.join("\n  ")); process.exit(1); }
console.log("the Android app watched an end-to-end encrypted stream: OK");
process.exit(0);
