#!/usr/bin/env node
// Meeting notes in a native app's call host (iOS WKWebView or Android WebView), with synthetic media
// only. Serves the harness on 127.0.0.1:PORT (default 8560); Bob answers in headless Chrome on this
// machine. Launch the native app as Alice with its debug call-test arguments pointing here, calling
// /bob, for example:
//
//   iOS:     xcrun simctl launch <udid> dev.pigeonpost.inbox -calls-page http://127.0.0.1:8560/native-call.html \
//              -calls-token tok-alice -calls-mailbox /k/alice0000000000000000000a -calls-handle /alice -call /bob:audio
//   Android: adb reverse tcp:8560 tcp:8560; adb shell am start -n dev.pigeonpost.inbox/.MainActivity \
//              --es pigeonpost.calls.page http://127.0.0.1:8560/native-call.html --es pigeonpost.calls.token tok-alice \
//              --es pigeonpost.calls.mailbox /k/alice0000000000000000000a --es pigeonpost.calls.handle /alice \
//              --es pigeonpost.call /bob:audio --es pigeonpost.fixtures inbox
//
// The page's test fixture taps Notes, Ask everyone and Agree in the native host (a person would) and
// reports its state. Passes when the native host's own microphone (440 Hz) was captured, encrypted,
// relayed as ciphertext and transcribed, and the brief was wrapped for the native host's notes key.
import { startRig, launchChrome } from "./rig.mjs";

const PORT = Number(process.env.PORT || 8560);
const LABEL = process.env.LABEL || "native";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn, what, ms = 180000) {
  const start = Date.now();
  for (;;) {
    let v;
    try { v = await fn(); } catch (_) { v = false; }
    if (v) return v;
    if (Date.now() - start > ms) throw new Error("timed out waiting for " + what);
    await sleep(500);
  }
}
const rig = await startRig({ port: PORT, nativeFixture: true });
const { ORIGIN, meetings, relayed, windows, reports, log } = rig;
const failures = [];
const check = (ok, what) => { if (!ok) failures.push(what); log((ok ? "ok   " : "FAIL ") + `[${LABEL}] ` + what); };
const near = (hz, f) => Math.abs(hz - f) < f * 0.06;
let browser;
try {
  browser = await launchChrome();
  const bob = await browser.page(`${ORIGIN}/?as=bob`);
  await until(() => bob.evaluate(`Boolean(window.PigeonpostCalls && PigeonpostCalls.inspect().session)`), "Bob's inbox", 60000);
  log(`[${LABEL}] ready on ${ORIGIN}: launch the native app now`);
  await until(() => reports.some((r) => r.session), "the native call host to sign in to calls");
  const ua = reports.find((r) => r.ua).ua;
  log(`[${LABEL}] native user agent: ${ua}`);
  await until(() => reports.some((r) => r.notesKey && r.notesKey !== "pending"), "the notes key probe", 30000);
  log(`[${LABEL}] notes key on this device: ${reports.filter((r) => r.notesKey).at(-1).notesKey} (indexedDB: ${reports.at(-1).idb})`);
  await until(() => bob.evaluate(`!document.getElementById("call-ring").hidden`), "the native app to ring Bob");
  await bob.evaluate(`document.getElementById("call-ring-accept").click()`);
  await until(() => bob.evaluate(`Boolean(document.getElementById("call-notes-sheet")) && !document.getElementById("call-notes-sheet").hidden`), "the native host to request notes");
  check(true, "the native host (host) requested notes");
  await bob.evaluate(`document.getElementById("call-notes-agree").click()`);
  await until(() => reports.some((r) => r.summary && r.summary.capturing), "the native host to capture");
  const capturing = reports.filter((r) => r.summary && r.summary.capturing).at(-1);
  check(/your microphone is being captured/.test(capturing.banner || ""), "the native host shows the capture indicator");
  check(capturing.summary.verified === true, "the native host verified the notes service key");
  log(`[${LABEL}] recorder type: ${capturing.summary.mime || "(browser default)"}`);
  await until(() => relayed.filter((r) => r.path.endsWith("/chunks") && JSON.parse(r.body).speaker === "/alice").length >= 2, "two chunks from the native host", 60000);
  const mine = relayed.filter((r) => r.path.endsWith("/chunks")).map((r) => JSON.parse(r.body)).filter((c) => c.speaker === "/alice");
  check(mine.every((c) => typeof c.envelope.ct === "string" && c.envelope.ct.length > 100), `${mine.length} chunks from the native host relayed as ciphertext (${mine[0].envelope.codec})`);
  // Bob leaves; the 1:1 call ends, notes stop and the brief is made.
  await bob.evaluate(`document.getElementById("call-hangup").click()`);
  await until(() => [...meetings.values()][0]?.brief, "the brief", 90000);
  const tones = windows.filter((w) => w.peak > 0.01).map((w) => w.hz);
  check(tones.some((hz) => near(hz, 440)), "the native host's own tone (440 Hz) was transcribed: " + tones.join(", "));
  check(!tones.some((hz) => !near(hz, 440) && !near(hz, 660)), "no window mixed in anything else");
  const meeting = [...meetings.values()][0];
  const brief = meeting.brief;
  const aliceKeys = [...(meeting.attendees.get("acct_alice")?.keys.keys() || [])];
  check(brief.status === "ready" && aliceKeys.length === 1 && brief.wraps.some((w) => w.account_id === "acct_alice" && w.key_id === aliceKeys[0]), "the brief is wrapped for the native host's own notes key");
} catch (error) {
  failures.push("harness: " + (error && error.stack || error));
} finally {
  browser?.close();
  rig.close();
}
if (failures.length) { log(`\n[${LABEL}] ${failures.length} problem(s):\n  ` + failures.join("\n  ")); process.exit(1); }
log(`[${LABEL}] meeting notes in the native call host: OK`);
process.exit(0);
