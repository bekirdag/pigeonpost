// Windows app ⇄ browser call test. Runs a stand-in postbox, the real call service from this
// repository, and the inbox site locally; the browser peer is "bob" (headless Chrome with synthetic
// media), the Windows app is "alice" (its WebView2 with Chromium's fake devices). Proves:
//   1. an outgoing video call from the app connects with audio and video both ways, the call covers
//      the window, minimises to the strip, (optionally) shares the screen, and hangs up;
//   2. an incoming call rings in the idle, invisible call view, which comes forward and connects.
//
//   node drive.mjs <repo> <out-dir> <Pigeonpost.exe>       (Windows runner)
//   node drive.mjs <repo> <out-dir> --simulate             (any OS: a browser stands in for the app)
import { spawn, execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { randomBytes } from "node:crypto";
import { launch, attach, sleep, until } from "./cdp.mjs";

const [repo, out, app] = process.argv.slice(2);
const simulate = app === "--simulate";
mkdirSync(out, { recursive: true });
const here = (name) => fileURLToPath(new URL("./" + name, import.meta.url));
const SITE = 8460, CALLS = 8791, POSTBOX = 8792, DEVTOOLS = 9333;
const ALICE = { token: "tok-alice", address: "/k/ec55xq18rbwdnwpsvkht07az40", handle: "/alice" };
const BOB = { address: "/k/rywv63mb6h0hhrpfjewrrttspw", handle: "/bob" };
const result = { checks: [], passed: false };
const check = (name, ok, detail) => { result.checks.push({ name, ok: Boolean(ok), detail }); console.log(`${ok ? "PASS" : "FAIL"} ${name}${detail === undefined ? "" : " " + JSON.stringify(detail)}`); if (!ok) throw new Error(name); };
const note = (name, detail) => { result.checks.push({ name, ok: null, detail }); console.log(`NOTE ${name} ${JSON.stringify(detail)}`); };

const procs = [];
const run = (args, env = {}) => { const p = spawn(process.execPath, args, { env: { ...process.env, ...env }, stdio: ["ignore", "inherit", "inherit"] }); procs.push(p); return p; };
const serviceToken = "svc_" + randomBytes(24).toString("hex");
run([here("mockpostbox.mjs"), String(POSTBOX)], { PIGEONPOST_TEST_INTERNAL_TOKEN: serviceToken });
run([here("calls.mjs"), path.join(repo, "calls-service/src/server.js")], { CALLS_TURN_SECRET: "", CALLS_PORT: String(CALLS), CALLS_POSTBOX_URL: `http://127.0.0.1:${POSTBOX}`, CALLS_ALLOWED_ORIGINS: `http://127.0.0.1:${SITE}`, CALLS_TRUST_PROXY: "0", POSTBOX_INTERNAL_URL: `http://127.0.0.1:${POSTBOX}`, POSTBOX_SERVICE_TOKEN: serviceToken });
run([here("serve.mjs"), path.join(repo, "site-inbox"), String(SITE), String(CALLS)]);
await sleep(1500);

// ---- The app side ---------------------------------------------------------------------------
let appProcess = null, A = null, B = null;
const logFile = path.join(process.env.LOCALAPPDATA || "", "Pigeonpost", "logs", "desktop.log");
const appLog = () => {
  if (simulate || !appProcess || !existsSync(logFile)) return [];
  return readFileSync(logFile, "utf8").split(/\r?\n/).filter((l) => l.includes(`[${appProcess.pid}]`));
};
const logged = (text) => appLog().filter((l) => l.includes(text)).length;
const pwsh = (...args) => !appProcess ? "" : execFileSync("pwsh", ["-NoProfile", "-File", here("window.ps1"), "-ProcessId", String(appProcess.pid), ...args], { encoding: "utf8" }).trim();
const capture = (name) => { if (!simulate) pwsh("-Action", "capture", "-Path", path.join(out, name)); };
const buttonEnabled = (name) => simulate ? null : pwsh("-Action", "enabled", "-Name", name) === "True";

// In simulation a browser page stands in for the app: it answers the page exactly as the app does.
const nativeShim = `window.__native = []; window.chrome = window.chrome || {}; window.chrome.webview = { postMessage: (m) => window.__native.push(m) };`;
let simMode = "hidden";
async function simulatePump() {
  for (;;) {
    let messages = [];
    try { messages = await A.evaluate("window.__native.splice(0)"); } catch { return; }
    for (const m of messages) {
      if (m.type === "ready") await A.evaluate(`PigeonpostNative.configure(${JSON.stringify({ token: ALICE.token, mailbox: { address: ALICE.address, handle: ALICE.handle }, known: { "/bob": "Bob" } })})`);
      if (m.type === "renew") await A.evaluate(`PigeonpostNative.renewed(${JSON.stringify(ALICE.token)})`);
      if (m.type === "mode") simMode = m.mode;
    }
    await sleep(150);
  }
}
const mode = () => simulate ? simMode : (() => {
  const lines = appLog().filter((l) => / Call view (Hidden|Mini|Full)$/.test(l));
  return lines.length ? lines.at(-1).split(" ").at(-1).toLowerCase() : "hidden";
})();

const inspect = (P) => P.evaluate("PigeonpostCalls.inspect()");
const audioIn = (i) => i.peers[0]?.stats?.receiving?.audio;
const videoIn = (i) => i.peers[0]?.stats?.receiving?.video;

try {
  B = await launch();
  await B.send("Page.navigate", { url: `http://127.0.0.1:${SITE}/?as=bob` });
  await until(() => B.evaluate("Boolean(window.PigeonpostCalls && PigeonpostCalls.inspect().session)"), 30000, 250, "browser peer signed in");
  check("Browser peer (bob) is signed in to the local call service", true);

  if (simulate) {
    A = await launch([], nativeShim);
    await A.send("Page.navigate", { url: `http://127.0.0.1:${SITE}/native-call.html` });
    simulatePump();
  } else {
    appProcess = spawn(app, [], {
      stdio: "ignore",
      env: {
        ...process.env,
        PIGEONPOST_CALLS_PAGE: `http://127.0.0.1:${SITE}/native-call.html`,
        PIGEONPOST_CALLS_TOKEN: ALICE.token, PIGEONPOST_CALLS_MAILBOX: ALICE.address, PIGEONPOST_CALLS_HANDLE: ALICE.handle,
        PIGEONPOST_CALLS_CALL: "/bob:video",
        PIGEONPOST_CALLS_BROWSER_ARGS: `--use-fake-device-for-media-stream --remote-debugging-port=${DEVTOOLS} --auto-select-desktop-capture-source=Entire screen`,
      },
    });
    A = await attach(DEVTOOLS, (t) => t.type === "page" && t.url.includes("native-call.html"), 60000);
    await until(() => logged("Call page ready"), 30000, 250, "the app's call page to be ready");
    await until(() => { try { return pwsh("-Action", "size") === "sized"; } catch { return false; } }, 20000, 500, "the app window");
    check("The app loaded the call page in WebView2 and configured it", true, { log: appLog().filter((l) => l.includes("Call ")).map((l) => l.replace(/^\S+ \[\d+\] /, "")) });
  }
  await until(() => A.evaluate("Boolean(window.PigeonpostCalls && PigeonpostCalls.inspect().session)"), 30000, 250, "the app's call session");
  check("The app's call client is signed in as alice", true);
  // (The app places PIGEONPOST_CALLS_CALL 1.5s after the page is ready; the idle state is checked
  // after the first call ends.)

  // ---- 1. Outgoing video call from the app ----
  if (simulate) await A.evaluate(`PigeonpostNative.call("/bob", "video")`); // The app places PIGEONPOST_CALLS_CALL itself.
  await until(() => B.evaluate("!document.getElementById('call-ring').hidden"), 40000, 250, "the browser to ring");
  check("The app's outgoing call rings the browser peer", true);
  await until(() => mode() === "full", 10000, 200, "the call view to cover the window");
  check("The call covers the app window (mode full)", true);
  await B.evaluate("document.getElementById('call-ring-accept').click()");
  await until(async () => (await inspect(B)).peers[0]?.connected && (await inspect(A)).peers[0]?.connected, 40000, 250, "both ends connected");
  check("Outgoing call connected at both ends", true);
  await sleep(8000);
  let a = await inspect(A), b = await inspect(B);
  result.outgoing = { app: a.peers.map((p) => ({ connected: p.connected, route: p.stats?.route, receiving: p.stats?.receiving, sas: p.security?.code })), browser: b.peers.map((p) => ({ connected: p.connected, route: p.stats?.route, receiving: p.stats?.receiving, sas: p.security?.code })) };
  check("The app receives the browser's audio", audioIn(a)?.bps > 0 || audioIn(a)?.received > 0, audioIn(a));
  check("The browser receives the app's audio", audioIn(b)?.bps > 0 || audioIn(b)?.received > 0, audioIn(b));
  check("The app receives the browser's video", videoIn(a)?.decoded > 0, videoIn(a));
  check("The browser receives the app's camera", videoIn(b)?.decoded > 0, videoIn(b));
  check("Both ends show the same security code", a.peers[0]?.security?.code && a.peers[0].security.code === b.peers[0]?.security?.code, a.peers[0]?.security?.code);
  if (!simulate) {
    check("The microphone and camera were granted by the app's permission handler", logged("Call permission Microphone: allowed") > 0 && logged("Call permission Camera: allowed") > 0);
    check("Call buttons are disabled while a call is showing", buttonEnabled("Video call") === false && buttonEnabled("Voice call") === false);
  }
  capture("call-video-full.png");
  writeFileSync(path.join(out, "call-browser-peer.png"), Buffer.from((await B.send("Page.captureScreenshot", { format: "png" })).data, "base64"));

  // Screen sharing: WebView2 supports getDisplayMedia; the runner auto-selects the screen.
  try {
    const offered = await A.evaluate("!document.getElementById('call-share').hidden");
    if (!offered) note("Screen sharing", "share button hidden (getDisplayMedia unavailable)");
    else {
      await A.evaluate("document.getElementById('call-share').click()");
      const shared = await until(async () => (await inspect(B)).peers[0]?.remoteScreen > 0, 15000, 250, "the shared screen").catch(() => false);
      note("Screen sharing", { offered, receivedByBrowser: Boolean(shared), log: logged("Call screen sharing started") });
      if (shared) { await sleep(1500); capture("call-sharing.png"); }
      if (await A.evaluate("Boolean(PigeonpostCalls.inspect().local?.screen)").catch(() => false) || shared) await A.evaluate("document.getElementById('call-share').click()");
    }
  } catch (e) { note("Screen sharing", String(e.message || e)); }

  await A.evaluate("document.getElementById('call-minimize').click()");
  await until(() => mode() === "mini", 10000, 200, "the strip");
  check("Minimising the call leaves a strip over the inbox (mode mini)", true);
  await sleep(800);
  capture("call-mini-strip.png");
  if (!simulate) check("Call buttons stay disabled while the strip shows", buttonEnabled("Voice call") === false);
  await B.evaluate("document.getElementById('call-hangup').click()");
  await until(() => mode() === "hidden", 15000, 200, "the call view to hide");
  check("Hanging up hides the call view again", true);
  if (!simulate) { await sleep(500); check("Call buttons are enabled again", buttonEnabled("Video call") && buttonEnabled("Voice call")); }

  // ---- 2. Incoming audio call to the idle app ----
  await sleep(3000);
  await B.evaluate("document.querySelector('.thread-row')?.click()");
  await sleep(600);
  await B.evaluate("document.getElementById('call-audio-btn').click()");
  await until(async () => await A.evaluate("!document.getElementById('call-ring').hidden") && mode() === "full", 40000, 250, "the app to ring");
  check("An incoming call rings in the app and comes forward (mode full)", true);
  await sleep(800);
  capture("call-incoming-ring.png");
  if (!simulate) {
    const colours = Number(pwsh("-Action", "colors"));
    check("The ring is a card over the inbox, which stays visible", logged("Call view ring card") > 0 && colours > 12, { distinctColours: colours });
  }
  await A.evaluate("document.getElementById('call-ring-accept').click()");
  await until(async () => (await inspect(B)).peers[0]?.connected && (await inspect(A)).peers[0]?.connected, 40000, 250, "incoming call connected");
  if (!simulate) check("Answering turns the ring card into the full-window call", logged("Call view ring card closed") > 0 && mode() === "full");
  await sleep(7000);
  a = await inspect(A); b = await inspect(B);
  result.incoming = { app: a.peers.map((p) => ({ connected: p.connected, route: p.stats?.route, receiving: p.stats?.receiving })), browser: b.peers.map((p) => ({ connected: p.connected, route: p.stats?.route, receiving: p.stats?.receiving })) };
  check("Incoming call: the app receives audio", audioIn(a)?.bps > 0 || audioIn(a)?.received > 0, audioIn(a));
  check("Incoming call: the browser receives the app's audio", audioIn(b)?.bps > 0 || audioIn(b)?.received > 0, audioIn(b));
  capture("call-voice-full.png");
  await A.evaluate("document.getElementById('call-hangup').click()");
  await until(() => mode() === "hidden", 15000, 200, "the call view to hide after the incoming call");
  check("The app hangs up and returns to the inbox", true);

  // ---- 3. Scheduled meeting: exact occurrence, service window and lobby, then native media ----
  await until(async () => !(await inspect(A)).room && !(await inspect(B)).room, 15000, 200, "both earlier calls to finish");
  const occurrenceId = "occ_nativeCalendar_first";
  const nativeJoin = (code, options) => A.evaluate(`PigeonpostNative.joinScheduled(${JSON.stringify(code)}, ${JSON.stringify(options)})`);
  const wrongInbox = await nativeJoin("ABCDEFGHJK", { as: BOB.address, occurrenceId, kind: "video" });
  check("The native scheduled bridge refuses another inbox", wrongInbox?.state === "error" && wrongInbox?.code === "identity_changed");
  const closed = await nativeJoin("BCDEFGHJKM", { as: ALICE.address, occurrenceId, kind: "video" });
  check("The service refuses a closed scheduled window", closed?.state === "error" && closed?.code === "window_closed");
  const choose = await nativeJoin("ABCDEFGHJK", { as: ALICE.address, occurrenceId: "occ_notSelected", kind: "video" });
  check("A changed occurrence requires explicit selection instead of joining the first window", choose?.state === "choose_occurrence");
  const waiting = await B.evaluate(`PigeonpostCalls.joinScheduled("ABCDEFGHJK", ${JSON.stringify({ as: BOB.address, occurrenceId, kind: "video", title: "Synthetic calendar meeting", role: "host" })})`);
  check("The first attendee waits in the scheduled lobby and does not become host", waiting?.state === "waiting");
  if (simulate) {
    const joined = await nativeJoin("ABCDEFGHJK", { as: ALICE.address, occurrenceId, kind: "video", title: "Synthetic calendar meeting" });
    check("The native member joins the exact scheduled occurrence", joined?.state === "joined");
  } else {
    // A separate disposable test process supplies one shared synthetic account to the preview,
    // HTTP client and call host. Its real CalendarStore prepares the selected occurrence and
    // the production host dispatches/receives the operation-tagged native bridge unchanged.
    result.adHocAppLog = appLog().map(line => line.replace(/^\S+ \[\d+\] /, ""));
    A.close(); const earlier = appProcess; earlier.kill();
    await until(() => earlier.exitCode !== null, 10000, 100, "the earlier app process to exit");
    const scheduledPort = DEVTOOLS + 1;
    appProcess = spawn(app, [], { stdio: "ignore", env: {
      ...process.env, PIGEONPOST_CALLS_PAGE: `http://127.0.0.1:${SITE}/native-call.html`,
      PIGEONPOST_CALLS_TOKEN: ALICE.token, PIGEONPOST_CALLS_MAILBOX: ALICE.address, PIGEONPOST_CALLS_HANDLE: ALICE.handle,
      PIGEONPOST_CALLS_CALL: "", PIGEONPOST_CALLS_POSTBOX: `http://127.0.0.1:${POSTBOX}/`,
      PIGEONPOST_CALLS_SCHEDULED: JSON.stringify({ code: "ABCDEFGHJK", event_id: "evt_nativeCalendar", occurrence_id: occurrenceId, video: true }),
      PIGEONPOST_CALLS_BROWSER_ARGS: `--use-fake-device-for-media-stream --remote-debugging-port=${scheduledPort}`,
    } });
    A = await attach(scheduledPort, target => target.type === "page" && target.url.includes("native-call.html"), 60000);
    await until(() => logged("Scheduled call result: joined"), 45000, 200, "the original native scheduled operation result");
    check("The typed calendar request completes through the real native host and operation result", true);
    const nativeRoom = (await inspect(A)).room, waitingRoom = (await inspect(B)).room;
    check("The native member joins the exact selected occurrence's room", nativeRoom?.code === "ABCDEFGHJK" && nativeRoom?.id === waitingRoom?.id);
  }
  await until(() => A.evaluate("Boolean(document.querySelector('#call-knocks .call-pill-primary'))"), 15000, 200, "the host's lobby controls");
  check("Only the scheduled host sees end/extend controls", await A.evaluate("!document.getElementById('call-host-tools').hidden") && await B.evaluate("document.getElementById('call-host-tools').hidden"));
  capture("scheduled-call-lobby.png");
  await A.evaluate("document.querySelector('#call-knocks .call-pill-primary').click()");
  await until(async () => (await inspect(A)).peers[0]?.connected && (await inspect(B)).peers[0]?.connected, 40000, 250, "scheduled participants connected after admission");
  await sleep(7000); a = await inspect(A); b = await inspect(B);
  result.scheduled = { app: a.peers.map(p => ({ connected: p.connected, receiving: p.stats?.receiving, sas: p.security?.code })), browser: b.peers.map(p => ({ connected: p.connected, receiving: p.stats?.receiving, sas: p.security?.code })) };
  check("Scheduled native and browser participants receive audio both ways", (audioIn(a)?.bps > 0 || audioIn(a)?.received > 0) && (audioIn(b)?.bps > 0 || audioIn(b)?.received > 0));
  check("Scheduled native and browser participants receive video both ways", videoIn(a)?.decoded > 0 && videoIn(b)?.decoded > 0);
  check("Scheduled participants show the same security code", a.peers[0]?.security?.code && a.peers[0].security.code === b.peers[0]?.security?.code);
  capture("scheduled-call-connected.png");
  await A.evaluate("document.getElementById('call-end-all').click()");
  await until(async () => mode() === "hidden" && !(await inspect(B)).room, 15000, 200, "the host to end the scheduled meeting");
  check("The scheduled host ends the meeting for both participants", true);
  result.passed = true;
} catch (e) {
  result.error = String(e.stack || e);
  result.appConsole = A?.logs.slice(-15); result.browserConsole = B?.logs.slice(-15);
  console.error(result.error);
  try { capture("call-failure.png"); } catch {}
} finally {
  result.appExit = appProcess ? { exitCode: appProcess.exitCode, signal: appProcess.signalCode } : null;
  result.appLog = appLog().map((l) => l.replace(/^\S+ \[\d+\] /, ""));
  writeFileSync(path.join(out, "calls.json"), JSON.stringify(result, null, 1));
  A?.close?.(); B?.close?.();
  if (appProcess) try { appProcess.kill(); } catch {}
  for (const p of procs) p.kill();
  process.exit(result.passed ? 0 : 1);
}
