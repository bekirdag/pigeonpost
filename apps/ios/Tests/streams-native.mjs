#!/usr/bin/env node
// Live streams in the iOS app's own call host (Phase 6, native round three), with synthetic media
// only — never this Mac's camera or microphone.
//
// The local stack from tests/e2e-streams (a local LiveKit on PATH, the stand-in postbox, the real
// calls service, the real inbox pages); a streamer in headless Chrome goes live; then the app, in a
// simulator this script is given, watches through the Swift bridge (`CallHost.watchStream`, started
// by the debug `-calls-live` argument exactly as the LIVE banner's Watch starts it).
//
// Checks: the app's WKWebView decodes the end-to-end encrypted stream; the streamer's stage
// invitation reaches the app's page through the app (the app has no group event stream: CallHost
// forwards the role change, see `watchLive`); a person accepting it (the page's own "Join the stage"
// button, pressed by the test page) publishes the app's synthetic microphone and camera; the stream
// ending closes the app's stream view.
//
//   node apps/ios/Tests/streams-native.mjs <simulator-udid>
//
// Ports 7980/7981, 50100-50400/udp, 8560, 8561 and 8891 on 127.0.0.1.
import { spawn, execFileSync } from "node:child_process";
import { readFileSync, writeFileSync, mkdtempSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createMockPostbox } from "../../../tests/e2e-streams/mock-postbox.mjs";
import { createStatic } from "../../../tests/e2e-streams/serve.mjs";
import { launchChrome } from "../../../tests/e2e-meeting-summaries/rig.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HERE, "../../..");
const SIM = process.argv[2];
if (!SIM) { console.error("usage: streams-native.mjs <simulator-udid>"); process.exit(2); }
const SECRET = "x".repeat(48);
const P = { lk: 7980, calls: 8560, pb: 8891, web: 8561 };
const ORIGIN = `http://127.0.0.1:${P.web}`;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const failures = [];
const check = (ok, what, detail = "") => { if (!ok) failures.push(what); console.log((ok ? "ok   " : "FAIL ") + what + (detail ? "  — " + detail : "")); };
async function until(fn, what, ms = 60000) {
  const start = Date.now();
  for (;;) {
    let v; try { v = await fn(); } catch (_) { v = false; }
    if (v) return v;
    if (Date.now() - start > ms) throw new Error("timed out waiting for " + what);
    await sleep(500);
  }
}

const procs = [];
const run = (cmd, args, env) => { const p = spawn(cmd, args, { env: { ...process.env, ...env }, stdio: process.env.VERBOSE ? "inherit" : "ignore" }); procs.push(p); return p; };
const lkYaml = path.join(mkdtempSync(path.join(os.tmpdir(), "pp-native-streams-")), "livekit.yaml");
writeFileSync(lkYaml, `port: ${P.lk}\nbind_addresses: ["127.0.0.1"]\nrtc:\n  tcp_port: 7981\n  port_range_start: 50100\n  port_range_end: 50400\n  use_external_ip: false\n  node_ip: 127.0.0.1\nkeys:\n  APIlocal: ${SECRET}\nlogging:\n  level: warn\n`);
run("livekit-server", ["--config", lkYaml]);
const pb = createMockPostbox({ members: { "/k/host": "owner", "/k/app": "member" } });
await new Promise((r) => pb.server.listen(P.pb, "127.0.0.1", r));
run("node", [path.join(REPO, "calls-service/src/server.js")], {
  CALLS_PORT: String(P.calls), CALLS_POSTBOX_URL: `http://127.0.0.1:${P.pb}`, CALLS_ALLOWED_ORIGINS: ORIGIN,
  POSTBOX_INTERNAL_URL: `http://127.0.0.1:${P.pb}`, POSTBOX_SERVICE_TOKEN: "svc_calls_test",
  CALLS_SFU_URL: `ws://127.0.0.1:${P.lk}`, CALLS_SFU_API_URL: `http://127.0.0.1:${P.lk}`, CALLS_SFU_KEY: "APIlocal", CALLS_SFU_SECRET: SECRET,
  CALLS_SFU_BUDGET_MBPS: "400", CALLS_STREAM_START_RESERVE: "1",
});

// The pages: the real native-call.html with synthetic media (inject.js) ahead of everything. The
// app's copy also carries an observer that reports what the page shows and, like a person, presses
// "Join the stage" when (and only when) the page offers it.
const inject = readFileSync(path.join(REPO, "tests/e2e-streams/inject.js"), "utf8");
const page = readFileSync(path.join(REPO, "site-inbox/native-call.html"), "utf8");
const autopilot = `
(() => {
  const report = (body) => fetch("/__report", { method: "POST", body: JSON.stringify(body) }).catch(() => {});
  let pressed = false;
  setInterval(async () => {
    const L = window.PigeonpostLive; if (!L) return;
    const offer = document.getElementById("live-stage-offer");
    const offered = Boolean(offer && !offer.hidden);
    if (offered && !pressed) { pressed = true; report({ phase: "offer" }); setTimeout(() => L.acceptStage(), 800); }
    const ui = document.getElementById("live-ui");
    report({ phase: "state", ua: navigator.userAgent, current: L.current(), inspect: L.inspect(), stats: await L.stats(), shown: Boolean(ui && !ui.hidden), offered });
  }, 2000);
})();`;
const withInject = (extra) => page.replace("<head>", `<head>\n<script>${inject}</script>`).replace("</body>", `<script>${extra}</script>\n</body>`);
const reports = [];
const web = createStatic(path.join(REPO, "site-inbox"), P.calls, { routes: {
  "/native-call.html": (req, res) => { res.writeHead(200, { "content-type": "text/html", "cache-control": "no-store" }); res.end(withInject(autopilot)); },
  "/__test/host.html": (req, res) => { res.writeHead(200, { "content-type": "text/html", "cache-control": "no-store" }); res.end(withInject("")); },
  "/__report": (req, res) => { let d = ""; req.on("data", (c) => (d += c)); req.on("end", () => { try { reports.push(JSON.parse(d)); } catch (_) {} res.writeHead(204); res.end(); }); },
} });
await new Promise((r) => web.listen(P.web, "127.0.0.1", r));
for (let i = 0; i < 40; i++) { try { await fetch(`http://127.0.0.1:${P.lk}/`); break; } catch { await sleep(500); } }

let browser;
const appOut = path.join(mkdtempSync(path.join(os.tmpdir(), "pp-native-streams-app-")), "stdout.txt");
try {
  browser = await launchChrome();
  const host = await browser.page(`${ORIGIN}/__test/host.html`);
  await until(() => host.evaluate(`Boolean(window.PigeonpostNative && window.PigeonpostLive)`), "the streamer's page");
  await host.evaluate(`PigeonpostNative.configure({ token: "tok-host", mailbox: { address: "/k/host", handle: null } })`);
  const live = await host.evaluate(`PigeonpostNative.goLive({ id: "${pb.GID}", name: "Crew" }, { title: "Native check", camera: true, screen: false })`);
  check(live && live.ok, "the streamer goes live in Chrome", JSON.stringify(live));

  // The app, watching through its own bridge.
  try { execFileSync("xcrun", ["simctl", "terminate", SIM, "dev.pigeonpost.inbox"], { stdio: "ignore" }); } catch (_) {}
  execFileSync("xcrun", ["simctl", "launch", `--stdout=${appOut}`, `--stderr=${appOut}.err`, SIM, "dev.pigeonpost.inbox",
    "-fixtures", "-calls-page", `${ORIGIN}/native-call.html`, "-calls-token", "tok-app", "-calls-mailbox", "/k/app",
    "-calls-handle", "/app", "-calls-live", `watch:${pb.GID}:${live.stream}`]);
  // The bridge's own answer, from the app's log (NSLog "pigeonpost: calls-live watch ok").
  const appLog = () => { try { return execFileSync("xcrun", ["simctl", "spawn", SIM, "log", "show", "--last", "4m", "--style", "compact",
    "--predicate", 'eventMessage CONTAINS "pigeonpost: calls-live"'], { encoding: "utf8", timeout: 30000 }); } catch (_) { return ""; } };
  const answered = await until(() => /calls-live watch (\S+)/.exec(appLog()), "the app's watch to answer", 90000);
  check(answered[1] === "ok", "the app's Watch is accepted by the bridge", answered[1]);
  const decoding = await until(() => {
    const r = reports.filter((x) => x.phase === "state" && x.inspect).at(-1);
    const frames = r && (r.stats || []).filter((s) => s.source === "camera").reduce((a, s) => a + (s.frames || 0), 0);
    return frames > 30 ? r : false;
  }, "the app to decode the stream", 90000);
  console.log("     app user agent: " + decoding.ua);
  check(decoding.shown, "the app shows the stream view full screen");
  check(decoding.inspect.decryptErrors === 0 && decoding.inspect.unencrypted === 0, "end-to-end encrypted frames decode with no errors",
    `epoch ${decoding.inspect.epoch}, frames ${(decoding.stats || []).filter((s) => s.source === "camera").map((s) => s.frames).join(",")}`);
  check(decoding.current && decoding.current.role === "viewer", "the app watches as a viewer");

  // The stage: the streamer invites; only the app's forwarding can make the page offer it.
  await host.evaluate(`PigeonpostLive.setStage("/k/app", true)`);
  await until(() => reports.some((r) => r.phase === "offer"), "the stage invitation to be offered in the app", 45000);
  check(true, "the stage invitation reached the app's page (forwarded by the app)");
  const speaking = await until(() => {
    const r = reports.filter((x) => x.phase === "state" && x.current).at(-1);
    return r && r.current.role === "speaker" && (r.inspect.published || []).includes("mic") ? r : false;
  }, "the app to join the stage", 45000);
  check(speaking.inspect.published.includes("cam"), "after accepting, the app publishes its synthetic microphone and camera", speaking.inspect.published.join(","));
  const heard = await until(async () => {
    const s = (await host.evaluate(`PigeonpostLive.stats()`)) || [];
    const fromApp = s.filter((x) => x.source === "microphone" && x.bytes > 2000);
    return fromApp.length ? fromApp : false;
  }, "the streamer to receive the app's audio", 45000);
  check(heard.every((x) => x.encrypted !== false), "the app's stage audio arrives encrypted at the streamer");

  // The end.
  await host.evaluate(`PigeonpostLive.stop()`);
  await until(() => { const r = reports.filter((x) => x.phase === "state").at(-1); return r && !r.current && !r.shown; }, "the app's stream view to close", 45000);
  check(true, "the stream ending closes the app's stream view");

  // Going live from the app (the Go live sheet's bridge call), watched in Chrome.
  execFileSync("xcrun", ["simctl", "terminate", SIM, "dev.pigeonpost.inbox"]);
  await sleep(1000);
  execFileSync("xcrun", ["simctl", "launch", SIM, "dev.pigeonpost.inbox",
    "-fixtures", "-calls-page", `${ORIGIN}/native-call.html`, "-calls-token", "tok-app", "-calls-mailbox", "/k/app",
    "-calls-handle", "/app", "-calls-live", `golive:${pb.GID}:From the app`]);
  const hosting = await until(() => {
    const r = reports.filter((x) => x.phase === "state" && x.current).at(-1);
    return r && r.current.role === "host" && (r.inspect.published || []).includes("cam") ? r : false;
  }, "the app to go live", 60000);
  check(hosting.shown, "the app goes live through its bridge and publishes its synthetic camera and microphone", hosting.inspect.published.join(","));
  const appStream = hosting.current.id;
  const viewer = await browser.page(`${ORIGIN}/__test/host.html`);
  await until(() => viewer.evaluate(`Boolean(window.PigeonpostNative && window.PigeonpostLive)`), "the viewer's page");
  await viewer.evaluate(`PigeonpostNative.configure({ token: "tok-host", mailbox: { address: "/k/host", handle: null } })`);
  const watched = await viewer.evaluate(`PigeonpostNative.watchStream({ id: "${pb.GID}", name: "Crew" }, "${appStream}")`);
  check(watched && watched.ok, "Chrome watches the app's stream", JSON.stringify(watched));
  const seen = await until(async () => {
    const st = (await viewer.evaluate(`PigeonpostLive.stats()`)) || [];
    const frames = st.filter((x) => x.source === "camera").reduce((a, x) => a + (x.frames || 0), 0);
    return frames > 30 ? await viewer.evaluate(`PigeonpostLive.inspect()`) : false;
  }, "Chrome to decode the app's stream", 60000);
  check(seen.decryptErrors === 0 && seen.unencrypted === 0, "the app's stream is end-to-end encrypted and decodes in Chrome");
  execFileSync("xcrun", ["simctl", "terminate", SIM, "dev.pigeonpost.inbox"]);
  await until(() => viewer.evaluate(`!window.PigeonpostLive.current()`), "the stream to end when the app's streamer leaves", 90000);
  check(true, "the stream ends for viewers when the app's streamer goes away");
} catch (error) {
  failures.push("harness: " + (error && error.stack || error));
  console.log(String(error && error.stack || error));
  try { console.log("app stdout:\n" + readFileSync(appOut, "utf8").slice(-3000)); } catch (_) {}
  try { console.log("app stderr:\n" + readFileSync(appOut + ".err", "utf8").split("\n").filter((l) => /pigeonpost|calls/i.test(l)).slice(-30).join("\n")); } catch (_) {}
  console.log("reports: " + reports.length + " last: " + JSON.stringify(reports.at(-1) || null).slice(0, 1500));
} finally {
  try { execFileSync("xcrun", ["simctl", "terminate", SIM, "dev.pigeonpost.inbox"], { stdio: "ignore" }); } catch (_) {}
  browser?.close();
  web.close(); pb.server.close();
  for (const p of procs) p.kill("SIGTERM");
}
if (failures.length) { console.log(`\n${failures.length} problem(s):\n  ` + failures.join("\n  ")); process.exit(1); }
console.log("live streams in the iOS app's call host: OK");
process.exit(0);
