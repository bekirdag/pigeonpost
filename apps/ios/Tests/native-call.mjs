#!/usr/bin/env node
// A call answered through the CallKit path in the app's native engine (NativeCall), against the real
// calls service and the real call page in headless Chrome with synthetic media.
//
// The simulator has no call screen, so the stand-in plays the person's tap on the same handler
// CallKit calls. WebRTC's audio stays off in the simulator on purpose: turning it on would capture
// this Mac's microphone. So this proves signaling, the three m-lines, DTLS and the data channel with
// a browser, and that the app receives; that the phone's own microphone is heard needs a real phone.
//
//   node apps/ios/Tests/native-call.mjs <simulator-udid>
//
// Ports 8570, 8571 and 8892 on 127.0.0.1.
import { spawn, execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createMockPostbox } from "../../../tests/e2e-streams/mock-postbox.mjs";
import { createStatic } from "../../../tests/e2e-streams/serve.mjs";
import { launchChrome } from "../../../tests/e2e-meeting-summaries/rig.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HERE, "../../..");
const SIM = process.argv[2];
if (!SIM) { console.error("usage: native-call.mjs <simulator-udid>"); process.exit(2); }
const P = { calls: 8570, pb: 8892, web: 8571 };
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

const pb = createMockPostbox({ members: { "/k/host": "owner", "/k/app": "member" } });
await new Promise((r) => pb.server.listen(P.pb, "127.0.0.1", r));
let log = "";
const calls = spawn("node", [path.join(REPO, "calls-service/src/server.js")], {
  env: { ...process.env, CALLS_PORT: String(P.calls), CALLS_POSTBOX_URL: `http://127.0.0.1:${P.pb}`, CALLS_ALLOWED_ORIGINS: ORIGIN },
  stdio: ["ignore", "pipe", "pipe"],
});
for (const s of [calls.stdout, calls.stderr]) s.on("data", (d) => { log += d; if (process.env.VERBOSE) process.stdout.write(d); });

const inject = readFileSync(path.join(REPO, "tests/e2e-streams/inject.js"), "utf8");
const page = readFileSync(path.join(REPO, "site-inbox/native-call.html"), "utf8");
const synthetic = page.replace("<head>", `<head>\n<script>${inject}</script>`);
const send = (res) => { res.writeHead(200, { "content-type": "text/html", "cache-control": "no-store" }); res.end(synthetic); };
const web = createStatic(path.join(REPO, "site-inbox"), P.calls, { routes: { "/native-call.html": (_, res) => send(res), "/__test/host.html": (_, res) => send(res) } });
await new Promise((r) => web.listen(P.web, "127.0.0.1", r));

let browser;
try {
  browser = await launchChrome();
  const host = await browser.page(`${ORIGIN}/__test/host.html`);
  await until(() => host.evaluate(`Boolean(window.PigeonpostNative)`), "the caller's page");
  await host.evaluate(`PigeonpostNative.configure({ token: "tok-host", mailbox: { address: "/k/host", handle: null } })`);

  try { execFileSync("xcrun", ["simctl", "terminate", SIM, "dev.pigeonpost.inbox"], { stdio: "ignore" }); } catch (_) {}
  execFileSync("xcrun", ["simctl", "launch", SIM, "dev.pigeonpost.inbox", "-fixtures",
    "-calls-page", `${ORIGIN}/native-call.html`, "-calls-token", "tok-app", "-calls-mailbox", "/k/app", "-calls-handle", "/app",
    "-callkit-stand-in", "YES", "-callkit-script", "answer@3,mute@40,unmute@44,end@52"]);
  await sleep(9000);

  await host.evaluate(`PigeonpostNative.call("/k/app", "video")`);
  const pair = await until(() => /calls: pair room=\S+ \S+ size=2 state=connected route=(\S+)/.exec(log), "both ends to connect", 45000);
  check(true, "the app and the browser connect", "route=" + pair[1]);

  const media = (host_) => new RegExp(`calls: media room=\\S+ \\S+ at=6s host=${host_} vis=\\S+ mic=(\\S+) sent=\\S+ cam=(\\S+) out=a(\\d)v(\\d) in=a(\\d)e(\\d)v(\\d)`).exec(log);
  const app = await until(() => media("(?:ios-native)"), "the app's own media report", 30000);
  check(true, "the media report comes from the native engine", app[0].replace(/^calls: media room=\S+ \S+ /, ""));
  check(app[5] === "1", "the app receives the browser's audio");
  check(app[7] === "1", "the app decodes the browser's video");

  const state = await until(async () => {
    const s = await host.evaluate(`(() => { const t = document.querySelector("#call-status"); return { status: t && t.textContent, page: document.body.innerText.slice(0, 200) }; })()`);
    return s;
  }, "the caller's page");
  check(Boolean(state), "the caller's page is still in the call");

  // The script ends the call 52 s after the ring.
  const gone = await until(async () => {
    const st = await host.evaluate(`(() => { const u = document.querySelector("#call-ui"); return { ui: Boolean(u && !u.hidden && u.offsetParent !== null), inCall: Boolean(window.__ppInCall) }; })()`);
    return st.ui ? false : true;
  }, "the caller's call screen to close", 70000).catch(() => false);
  check(Boolean(gone), "hanging up on the system call screen ends the call for the other side");
} finally {
  try { execFileSync("xcrun", ["simctl", "terminate", SIM, "dev.pigeonpost.inbox"], { stdio: "ignore" }); } catch (_) {}
  browser?.close?.();
  calls.kill();
  pb.server.close();
  web.close();
}
console.log(failures.length ? `\n${failures.length} FAILED` : "\nall passed");
process.exit(failures.length ? 1 : 0);
