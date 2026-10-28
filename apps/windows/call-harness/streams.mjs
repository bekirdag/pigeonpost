// Windows app ⇄ browser live stream test (Phase 6, docs/streaming.md), on a disposable runner:
// a local LiveKit (LIVEKIT_SERVER), the stream stand-in postbox (tests/e2e-streams/mock-postbox.mjs),
// the repository's call service and the inbox pages. Synthetic media only (tests/e2e-streams/inject.js).
//   1. A headless-Chrome streamer goes live; the UI-test app watches through its production bridge
//      (C# WatchStreamScript → native-call.js watchStream) in WebView2 and decodes the encrypted frames.
//   2. The streamer invites the app onto the stage; the app reads that from the postbox and hands it
//      to the page, which offers the stage; joining publishes the app's camera to the stream.
//   3. A new app process goes live through the bridge (GoLiveScript); a browser watches and decodes it.
//   node streams.mjs <repo> <out-dir> <Pigeonpost.exe>
import { spawn, execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { launch, attach, sleep, until } from "./cdp.mjs";

const [repo, out, app] = process.argv.slice(2);
mkdirSync(out, { recursive: true });
const here = (name) => fileURLToPath(new URL("./" + name, import.meta.url));
const P = { lk: 7980, calls: 8560, pb: 8891, web: 8561, devtools: 9335 };
const SECRET = "x".repeat(48);
const result = { checks: [], notes: [], passed: false };
const check = (name, ok, detail) => { result.checks.push({ name, ok: Boolean(ok), detail }); console.log(`${ok ? "PASS" : "FAIL"} ${name}${detail === undefined ? "" : " " + JSON.stringify(detail)}`); if (!ok) throw new Error(name); };
const note = (name, detail) => { result.notes.push({ name, detail }); console.log(`NOTE ${name} ${JSON.stringify(detail)}`); };
const procs = [];

// Media candidates on the runner's own network address: WebView2 does not reach a media server
// that offers only loopback candidates (signalling still uses 127.0.0.1).
const lan = Object.values(os.networkInterfaces()).flat().find((a) => a && a.family === "IPv4" && !a.internal)?.address || "127.0.0.1";
const lkYaml = path.join(os.tmpdir(), "pp-win-streams-livekit.yaml");
writeFileSync(lkYaml, `port: ${P.lk}\nbind_addresses: ["127.0.0.1", "${lan}"]\nrtc:\n  tcp_port: 7981\n  port_range_start: 50100\n  port_range_end: 50400\n  use_external_ip: false\n  node_ip: ${lan}\nkeys:\n  APIlocal: ${SECRET}\nlogging:\n  level: warn\n`);
note("media address", lan);
procs.push(spawn(process.env.LIVEKIT_SERVER || "livekit-server", ["--config", lkYaml], { stdio: "ignore" }));
const { createMockPostbox } = await import(pathToFileURL(path.join(repo, "tests/e2e-streams/mock-postbox.mjs")).href);
const { createStatic } = await import(pathToFileURL(path.join(repo, "tests/e2e-streams/serve.mjs")).href);
const pb = createMockPostbox({ members: { "/k/host": "owner", "/k/win": "member", "/k/peer": "member" } });
await new Promise((r) => pb.server.listen(P.pb, "127.0.0.1", r));
procs.push(spawn(process.execPath, [here("calls.mjs"), path.join(repo, "calls-service/src/server.js")], { stdio: "inherit", env: { ...process.env,
  CALLS_TURN_SECRET: "", CALLS_PORT: String(P.calls), CALLS_POSTBOX_URL: `http://127.0.0.1:${P.pb}`, CALLS_ALLOWED_ORIGINS: `http://127.0.0.1:${P.web}`, CALLS_TRUST_PROXY: "0",
  POSTBOX_INTERNAL_URL: `http://127.0.0.1:${P.pb}`, POSTBOX_SERVICE_TOKEN: "svc_calls_test",
  CALLS_SFU_URL: `ws://127.0.0.1:${P.lk}`, CALLS_SFU_API_URL: `http://127.0.0.1:${P.lk}`, CALLS_SFU_KEY: "APIlocal", CALLS_SFU_SECRET: SECRET,
  CALLS_SFU_BUDGET_MBPS: "400", CALLS_STREAM_START_RESERVE: "1" } }));
// The pages get the stand-in postbox and synthetic media before any of their own scripts run.
const inject = `window.__POSTBOX = "http://127.0.0.1:${P.pb}";\n` + readFileSync(path.join(repo, "tests/e2e-streams/inject.js"), "utf8");
const nativePage = readFileSync(path.join(repo, "site-inbox/native-call.html"), "utf8").replace("<head>", '<head>\n<script src="/__inject.js"></script>');
// The vendored LiveKit client is loaded with a subresource-integrity hash; serve the committed bytes
// (a Windows checkout may have rewritten its line endings).
const committed = (file) => (req, res) => { res.writeHead(200, { "content-type": "text/javascript", "cache-control": "no-store" }); res.end(execFileSync("git", ["-C", repo, "show", "HEAD:site-inbox" + file], { maxBuffer: 64 << 20 })); };
const web = createStatic(path.join(repo, "site-inbox"), P.calls, { routes: {
  "/vendor/livekit-client/livekit-client.umd.js": committed("/vendor/livekit-client/livekit-client.umd.js"),
  "/vendor/livekit-client/livekit-client.e2ee.worker.js": committed("/vendor/livekit-client/livekit-client.e2ee.worker.js"),
  "/__inject.js": (req, res) => { res.writeHead(200, { "content-type": "text/javascript", "cache-control": "no-store" }); res.end(inject); },
  "/native-call.html": (req, res) => { res.writeHead(200, { "content-type": "text/html", "cache-control": "no-store" }); res.end(nativePage); },
} });
await new Promise((r) => web.listen(P.web, "127.0.0.1", r));
await until(async () => { await fetch(`http://127.0.0.1:${P.lk}/`); return true; }, 60000, 500, "LiveKit");
await until(async () => (await fetch(`http://127.0.0.1:${P.calls}/v1/health`)).ok, 60000, 500, "the call service");
await sleep(1000);

let appProcess = null, A = null, H = null, V = null;
const logFile = path.join(process.env.LOCALAPPDATA || "", "Pigeonpost", "logs", "desktop.log");
const appLog = () => !appProcess || !existsSync(logFile) ? [] : readFileSync(logFile, "utf8").split(/\r?\n/).filter((l) => l.includes(`[${appProcess.pid}]`));
const logged = (text) => appLog().filter((l) => l.includes(text)).length;
const capture = (name) => { if (!appProcess) return; try { execFileSync("pwsh", ["-NoProfile", "-File", here("window.ps1"), "-ProcessId", String(appProcess.pid), "-Action", "capture", "-Path", path.join(out, name)]); } catch (e) { note("capture " + name, String(e.message || e).slice(0, 200)); } };
// Browser pages report what the page tells its native host (toasts, modes) to the console log.
const shim = `window.chrome = window.chrome || {}; window.chrome.webview = { postMessage: (m) => console.log("native " + JSON.stringify(m)) };
  window.addEventListener("unhandledrejection", (e) => console.log("unhandled " + String(e.reason && (e.reason.stack || e.reason.message) || e.reason)));`;
const page = (who) => launch([], shim).then(async (p) => {
  await p.send("Page.navigate", { url: `http://127.0.0.1:${P.web}/native-call.html` });
  await until(() => p.evaluate("Boolean(window.PigeonpostNative && window.PigeonpostLive)"), 30000, 250, who + " page");
  await p.evaluate(`PigeonpostNative.configure(${JSON.stringify({ token: "tok-" + who, mailbox: { address: "/k/" + who, handle: null } })})`);
  await sleep(1500);
  return p;
});
const inspect = (p) => p.evaluate("PigeonpostLive.inspect()");
const decoded = async (p) => ((await p.evaluate("PigeonpostLive.stats()")) || []).filter((s) => s.source === "camera").reduce((n, s) => n + (s.frames || 0), 0);
function startApp(stream, port) {
  appProcess = spawn(app, [], { stdio: "ignore", env: { ...process.env,
    PIGEONPOST_CALLS_PAGE: `http://127.0.0.1:${P.web}/native-call.html`, PIGEONPOST_CALLS_TOKEN: "tok-win", PIGEONPOST_CALLS_MAILBOX: "/k/win", PIGEONPOST_CALLS_HANDLE: "/win/main",
    PIGEONPOST_CALLS_CALL: "", PIGEONPOST_CALLS_POSTBOX: `http://127.0.0.1:${P.pb}/`, PIGEONPOST_CALLS_STREAM: JSON.stringify(stream),
    PIGEONPOST_CALLS_BROWSER_ARGS: `--use-fake-device-for-media-stream --remote-debugging-port=${port}`,
  } });
  return attach(port, (t) => t.type === "page" && t.url.includes("native-call.html"), 60000);
}

try {
  // ---- 1. the app watches a browser's stream ----
  H = await page("host");
  const live = await H.evaluate(`PigeonpostNative.goLive({ id: ${JSON.stringify(pb.GID)}, name: "Crew" }, { title: "Native watch", camera: true })`);
  if (!live?.ok) result.hostDiagnosis = await H.evaluate(`(async () => {
    const out = { session: window.PigeonpostCalls && PigeonpostCalls.inspect && PigeonpostCalls.inspect().session || null, frames: Boolean(window.PigeonpostCalls && PigeonpostCalls.inspect && PigeonpostCalls.inspect().canEncryptFrames) };
    try { const s = await navigator.mediaDevices.getUserMedia({ audio: true, video: true }); out.media = s.getTracks().map((t) => t.kind); } catch (e) { out.media = String(e && (e.stack || e)); }
    try { out.inspect = PigeonpostLive.inspect(); } catch (e) { out.inspect = String(e); }
    out.supported = PigeonpostLive.supported();
    return out;
  })()`).catch((e) => String(e));
  check("The browser streamer is live", live?.ok && /^str_[a-z2-7]{26}$/.test(live.stream), { live, diagnosis: result.hostDiagnosis });
  A = await startApp({ action: "watch", group: pb.GID, name: "Crew", stream: live.stream }, P.devtools);
  await until(() => logged("Stream result: ok"), 60000, 250, "the app's watch result");
  check("The app's production bridge watched the stream (operation-bound result)", true);
  await until(() => logged("Call view Full"), 15000, 200, "the stream to cover the window");
  check("The stream covers the app window", true);
  await until(async () => (await inspect(A))?.connected && (await decoded(A)) > 30, 90000, 500, "the app to decode frames");
  await sleep(4000);
  const ai = await inspect(A), frames = await decoded(A);
  check("WebView2 decodes the end-to-end encrypted camera", frames > 60 && ai.decryptErrors === 0 && ai.unencrypted === 0, { frames, epoch: ai.epoch, decryptErrors: ai.decryptErrors });
  check("The app is a viewer and publishes nothing", ai.role === "viewer" && ai.published.length === 0, { role: ai.role, published: ai.published });
  const code = await A.evaluate("document.getElementById('live-ui') && !document.getElementById('live-ui').hidden");
  check("The live player is on screen in the call host", code === true);
  capture("stream-watching.png");

  // ---- 2. stage invitation, forwarded by the app from the postbox ----
  await H.evaluate(`PigeonpostLive.setStage("/k/win", true)`);
  await until(() => logged("Stream stage change forwarded: invited"), 20000, 250, "the app to forward the stage invitation");
  await until(() => A.evaluate("!document.getElementById('live-stage-offer').hidden"), 10000, 200, "the stage offer in the app");
  check("The app hands a stage invitation it read from the postbox to the page, which offers it", true);
  check("Nothing is published before the person accepts", (await inspect(A)).published.length === 0);
  capture("stream-stage-offer.png");
  await A.evaluate("PigeonpostLive.acceptStage()");
  await until(async () => (await inspect(A)).role === "speaker" && (await inspect(A)).published.length > 0, 20000, 300, "the app on stage");
  const heard = await until(() => H.evaluate("(() => { const r = PigeonpostLive._room(); let n = 0; for (const rp of r.remoteParticipants.values()) for (const p of rp.trackPublications.values()) if (p.isSubscribed && p.source === 'camera') n++; return n; })()"), 20000, 500, "the streamer to receive the app's camera");
  check("After accepting, the app's camera reaches the streamer", heard > 0, { heard });
  await H.evaluate("PigeonpostLive.stop()");
  await until(() => logged("Call view Hidden"), 30000, 300, "the app to close the ended stream");
  check("When the stream ends the app returns to the inbox", true);
  A.close(); { const earlier = appProcess; earlier.kill(); await until(() => earlier.exitCode !== null || earlier.signalCode !== null, 10000, 100, "the first app process to exit"); }
  await sleep(2000);

  // ---- 3. the app goes live, a browser watches ----
  A = await startApp({ action: "golive", group: pb.GID, name: "Crew", title: "From Windows" }, P.devtools + 1);
  await until(() => logged("Stream result: ok"), 60000, 250, "the app's go-live result");
  check("The app went live through its production bridge", true);
  const mine = [...pb.streams.values()].find((s) => s.state === "live");
  check("The postbox has the app's stream with its title", mine?.host === "/k/win" && mine.title === "From Windows", mine && { host: mine.host, title: mine.title });
  V = await page("peer");
  const watched = await V.evaluate(`PigeonpostNative.watchStream({ id: ${JSON.stringify(pb.GID)}, name: "Crew" }, ${JSON.stringify(mine.id)})`);
  check("A browser watches the app's stream", watched?.ok, watched);
  await until(async () => (await decoded(V)) > 60, 90000, 500, "the browser to decode the app's frames");
  const vi = await inspect(V), hostView = await inspect(A);
  check("The app's camera arrives end-to-end encrypted and decodes in the browser", vi.decryptErrors === 0 && vi.unencrypted === 0, { decryptErrors: vi.decryptErrors, frames: await decoded(V) });
  check("The app publishes as the streamer and wrapped the key for the viewer", hostView.role === "host" && hostView.wrapped >= 1, { role: hostView.role, wrapped: hostView.wrapped });
  capture("stream-live-from-app.png");
  await A.evaluate("PigeonpostLive.stop()");
  await until(() => [...pb.streams.values()].every((s) => s.state !== "live"), 15000, 300, "the app's stream to end");
  check("Stopping in the app ends the stream", true);
  result.passed = true;
} catch (e) {
  result.error = String(e.stack || e);
  result.appConsole = A?.logs?.slice(-20);
  result.appErrors = A?.logs?.filter((l) => /"type":"(error|warning|assert)"|exceptionDetails|"exception"/.test(l)).slice(-20);
  try { result.appLive = await A?.evaluate("JSON.stringify(PigeonpostLive.inspect())"); } catch (x) { result.appLive = String(x); }
  result.hostConsole = H?.logs?.filter((l) => /"type":"(error|warning)"|exceptionDetails|native /.test(l)).slice(-20);
  result.viewerConsole = V?.logs?.slice(-20);
  try { result.mockPostbox = pb.log.slice(-30); } catch {}
  console.error(result.error);
  try { capture("stream-failure.png"); } catch {}
} finally {
  result.appLog = appLog().map((l) => l.replace(/^\S+ \[\d+\] /, ""));
  writeFileSync(path.join(out, "streams.json"), JSON.stringify(result, null, 1));
  for (const p of [A, H, V]) try { p?.close(); } catch {}
  if (appProcess) try { appProcess.kill(); } catch {}
  web.close(); pb.server.close();
  for (const p of procs) try { p.kill(); } catch {}
  process.exit(result.passed ? 0 : 1);
}
