#!/usr/bin/env node
// Meeting notes end to end in real headless Chrome, under the inbox's production CSP, with synthetic
// media only (a different tone per person; nothing real is captured):
//
//   Alice  native-call.html (the native apps' call host), the host
//   Bob    index.html (the browser inbox), a member
//   Dana   join.html (a meeting link), a guest
//
// The real calls service and the real meeting-intel code run in this process; real ffmpeg decodes the
// browsers' MediaRecorder chunks. The postbox is an in-memory stand-in, and speech-to-text is a stand-in
// that reports each window's dominant tone instead of words, which proves whose microphone a chunk
// carried. Checks:
//   - nobody is captured until everyone agreed; Bob's decline keeps everyone paused; a late guest pauses
//     capture and is asked; nobody's chunk ever carries anyone else's tone;
//   - uploads cross the calls service only as ciphertext;
//   - the host stops notes; the brief is stored for Alice and Bob only (not the guest) and Bob opens and
//     reads it in the inbox's Meeting notes with his own device key.
//
//   node tests/e2e-meeting-summaries/run.mjs        (CHROME=/path/to/chrome, FFMPEG=ffmpeg)
import { startRig, launchChrome } from "./rig.mjs";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn, what, ms = 40000) {
  const start = Date.now();
  for (;;) {
    let v;
    try { v = await fn(); } catch (_) { v = false; }
    if (v) return v;
    if (Date.now() - start > ms) throw new Error("timed out waiting for " + what);
    await sleep(200);
  }
}
const failures = [];
const rig = await startRig();
const { ORIGIN, meetings, relayed, windows, log } = rig;
const check = (ok, what) => { if (!ok) failures.push(what); log((ok ? "ok   " : "FAIL ") + what); };
const near = (hz, f) => Math.abs(hz - f) < f * 0.06;

const NOTES = `(window.PigeonpostCallsSummary && PigeonpostCallsSummary.inspect()) || {}`;
let browser;
try {
  browser = await launchChrome();
  const alice = await browser.page(`${ORIGIN}/native-call.html?as=alice`);
  const bob = await browser.page(`${ORIGIN}/?as=bob`);
  await until(() => alice.evaluate(`Boolean(window.PigeonpostNative && window.PigeonpostCalls && window.PigeonpostCallsSummary)`), "Alice's call page");
  await until(() => bob.evaluate(`Boolean(window.PigeonpostCalls && PigeonpostCalls.inspect().session)`), "Bob's inbox");
  await alice.evaluate(`PigeonpostNative.configure({ token: "tok-alice", mailbox: { address: "/k/alice0000000000000000000a", handle: "/alice" }, known: { "/bob": "/bob" } })`);
  await until(() => alice.evaluate(`Boolean(PigeonpostCalls.inspect().session)`), "Alice to sign in to calls");
  await alice.evaluate(`PigeonpostNative.call("/bob", "audio")`);
  await until(() => bob.evaluate(`!document.getElementById("call-ring").hidden`), "Bob's ring");
  await bob.evaluate(`document.getElementById("call-ring-accept").click()`);
  await until(async () => (await alice.evaluate(`(PigeonpostCalls.inspect().peers || []).some((x) => x.connected)`)), "the call to connect");

  // The host is offered notes; Bob is not.
  await until(() => alice.evaluate(`!document.getElementById("call-notes-btn").hidden`), "Alice's Notes button");
  check(await bob.evaluate(`!document.getElementById("call-notes-btn") || document.getElementById("call-notes-btn").hidden`), "only the host is offered notes");
  await alice.evaluate(`document.getElementById("call-notes-btn").click()`);
  await until(() => alice.evaluate(`!document.getElementById("call-notes-ask").hidden`), "the host's request sheet");
  await alice.evaluate(`document.getElementById("call-notes-ask-go").click()`);
  for (const [name, p] of [["Alice", alice], ["Bob", bob]]) {
    await until(() => p.evaluate(`!document.getElementById("call-notes-sheet").hidden`), name + "'s consent sheet");
    await until(() => p.evaluate(`!document.getElementById("call-notes-banner").hidden`), name + "'s notes indicator");
  }
  check(await bob.evaluate(`/asked to take meeting notes/.test(document.getElementById("call-notes-title").textContent) && /Only your own microphone/.test(document.getElementById("call-notes-list").textContent)`), "the consent sheet discloses what is captured");

  // Alice agrees, Bob declines: still paused, nobody captured.
  await alice.evaluate(`document.getElementById("call-notes-agree").click()`);
  await bob.evaluate(`document.getElementById("call-notes-decline").click()`);
  await sleep(12000);
  check((await alice.evaluate(NOTES)).state === "waiting" && !(await alice.evaluate(NOTES)).capturing && !(await bob.evaluate(NOTES)).capturing, "a decline keeps capture paused for everyone");
  check(relayed.filter((r) => r.path.endsWith("/chunks")).length === 0, "no audio was uploaded before everyone agreed");

  // Bob changes his mind: capture runs on both.
  await bob.evaluate(`document.getElementById("call-notes-review").click()`);
  await until(() => bob.evaluate(`!document.getElementById("call-notes-sheet").hidden`), "Bob's sheet again");
  await bob.evaluate(`document.getElementById("call-notes-agree").click()`);
  await until(async () => (await alice.evaluate(NOTES)).capturing && (await bob.evaluate(NOTES)).capturing, "both to capture");
  check(/being captured/.test(await bob.evaluate(`document.getElementById("call-notes-text").textContent`)), "the indicator says the microphone is captured");
  await until(() => relayed.filter((r) => r.path.endsWith("/chunks")).length >= 2, "chunks from both", 30000);

  // A guest joins by the meeting link: capture pauses before they join and they are asked.
  const code = await alice.evaluate(`PigeonpostCalls.inspect().room && PigeonpostCalls.inspect().room.code`);
  const dana = await browser.page(`${ORIGIN}/${code}`);
  await until(() => dana.evaluate(`!document.getElementById("join-form").hidden`), "the join form");
  await dana.evaluate(`(() => { const n = document.getElementById("join-name"); n.value = "Dana"; n.dispatchEvent(new Event("input", { bubbles: true })); document.getElementById("join-submit").disabled = false; document.getElementById("join-form").requestSubmit(); })()`);
  await until(() => alice.evaluate(`Boolean(document.querySelector("#call-knocks .call-pill-primary"))`), "Dana's knock");
  const beforeJoin = relayed.filter((r) => r.path.endsWith("/chunks")).length;
  await alice.evaluate(`document.querySelector("#call-knocks .call-pill-primary").click()`);
  await until(() => dana.evaluate(`Boolean(document.getElementById("call-notes-sheet")) && !document.getElementById("call-notes-sheet").hidden`), "Dana's consent sheet");
  check((await alice.evaluate(NOTES)).state === "waiting", "a newcomer pauses capture");
  check(/guest: you can agree, but you won't receive a copy/.test(await dana.evaluate(`document.getElementById("call-notes-warn").textContent`)), "the guest is told they get no copy");
  await dana.evaluate(`document.getElementById("call-notes-agree").click()`);
  await until(async () => (await dana.evaluate(NOTES)).capturing && (await alice.evaluate(NOTES)).capturing, "capture to resume with the guest");
  await sleep(13000);
  check(relayed.filter((r) => r.path.endsWith("/chunks")).length > beforeJoin, "chunks after the guest agreed");

  // Bob stops for himself, then agrees again; the host stops notes.
  await bob.evaluate(`document.getElementById("call-notes-stopme").click()`);
  await until(async () => !(await bob.evaluate(NOTES)).capturing && (await alice.evaluate(NOTES)).state === "waiting", "Stop for me to pause everyone");
  await alice.evaluate(`document.getElementById("call-notes-off").click()`);
  await until(() => [...meetings.values()][0].brief, "the brief", 60000);

  // What crossed the calls service: ciphertext only. What meeting-intel heard: each window one tone.
  const chunkBodies = relayed.filter((r) => r.path.endsWith("/chunks")).map((r) => JSON.parse(r.body));
  check(chunkBodies.length >= 4 && chunkBodies.every((c) => typeof c.envelope.ct === "string" && !("audio" in c)), `${chunkBodies.length} chunks relayed as ciphertext`);
  const speakersSeen = new Set(chunkBodies.map((c) => c.speaker));
  check(speakersSeen.has("/alice") && speakersSeen.has("/bob") && speakersSeen.has("Guest Dana"), "chunks from all three, labelled by their own identity: " + [...speakersSeen].join(", "));
  const tones = windows.filter((w) => w.peak > 0.01).map((w) => w.hz);
  check(tones.length >= 2 && tones.every((hz) => near(hz, 440) || near(hz, 660) || near(hz, 880)), "every window carried exactly one person's tone: " + tones.join(", "));
  // Bob's audio was still waiting for transcription when he chose Stop for me: it was discarded.
  check(!tones.some((hz) => near(hz, 660)), "Stop for me discarded Bob's untranscribed audio");
  const brief = [...meetings.values()][0].brief;
  const accounts = new Set(brief.wraps.map((w) => w.account_id));
  check(brief.status === "ready" && accounts.has("acct_alice") && accounts.has("acct_bob") && accounts.size === 2, "the brief is wrapped for Alice and Bob only");

  // Bob reads it in the inbox with his own device key.
  try {
    await until(() => bob.evaluate(`(() => { const b = document.getElementById("notes-btn"); if (b && !b.hidden) b.click(); return Boolean(b && !b.hidden); })()`), "Bob's Meeting notes button", 20000);
  } catch (e) {
    log("diagnostic: " + JSON.stringify(await bob.evaluate(`({ host: Boolean(window.PigeonpostNotesHost), features: window.PigeonpostNotesHost ? [...PigeonpostNotesHost.features()] : null, nav: Boolean(document.getElementById("side-nav")), me: window.PigeonpostNotesHost && PigeonpostNotesHost.mailbox(), notes: Boolean(window.PigeonpostNotes) })`)));
    throw e;
  }
  await until(() => bob.evaluate(`(() => { const b = document.querySelector(".notes-list button"); if (b) b.click(); return Boolean(b); })()`), "the notes list");
  await until(() => bob.evaluate(`/Three synthetic tones were heard/.test(document.getElementById("notes-body").textContent)`), "Bob's decrypted brief");
  const text = await bob.evaluate(`document.getElementById("notes-body").textContent`);
  check(/\/alice:tone 4[0-9]{2} Hz/.test(text) && /Guest Dana:tone 8[0-9]{2} Hz/.test(text), "the transcript shows each speaker's own tone");
  check(browser.consoleErrors.length === 0, "no page errors or CSP refusals" + (browser.consoleErrors.length ? ": " + browser.consoleErrors.join(" | ") : ""));
  await bob.evaluate(`document.getElementById("call-hangup")?.click()`);
} catch (error) {
  failures.push("harness: " + (error && error.stack || error));
} finally {
  browser?.close();
  rig.close();
}
if (failures.length) { log("\n" + failures.length + " problem(s):\n  " + failures.join("\n  ")); process.exit(1); }
log("meeting notes end to end: OK");
process.exit(0);
