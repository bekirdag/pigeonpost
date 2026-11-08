// A call whose media stopped on this end gets it back.
//
// 2026-10-30 and 2026-11-04: iPhone-to-iPhone calls answered from CallKit connected, yet the person
// who answered was not heard or seen, and did not hear the caller (only the caller's picture
// arrived). On that phone WebKit's audio and camera were interrupted — the page answered while the
// app had been woken in the background by a VoIP push, and CallKit holds the app's audio session —
// and calls.js never asked for any of it again: an ended microphone stayed ended, a camera that
// could not open in the background was given up for the call, a suspended noise graph went on
// sending silence, and a paused voice stayed paused. Reproduced in WebKit (two WKWebViews, mock
// capture devices) with exactly that asymmetry; this drives the real calls client through the native
// bridge's answer with synthetic tracks.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { JSDOM, VirtualConsole } from "jsdom";

const read = (file) => readFileSync(new URL("../" + file, import.meta.url), "utf8");
const tick = () => new Promise((resolve) => setTimeout(resolve, 5));
async function until(predicate, ms = 2000) {
  for (let n = 0; n < ms / 5; n++) { if (predicate()) return; await tick(); }
  assert.fail("the synthetic call did not settle");
}
class Track {
  constructor(kind) { this.kind = kind; this.enabled = true; this.stops = 0; this.id = kind + Math.random(); this.readyState = "live"; this.muted = false; this.contentHint = ""; }
  stop() { this.stops++; this.readyState = "ended"; }
  getSettings() { return { echoCancellation: true }; }
}
class Stream {
  constructor(tracks = []) { this.tracks = tracks; }
  getTracks() { return this.tracks; }
  getAudioTracks() { return this.tracks.filter((t) => t.kind === "audio"); }
  getVideoTracks() { return this.tracks.filter((t) => t.kind === "video"); }
  addTrack(t) { this.tracks.push(t); }
  removeTrack(t) { this.tracks = this.tracks.filter((v) => v !== t); }
}

// `audio`: "stalled" (WebKit, audio not allowed: resume() never settles) or "running".
function page(t, { audio, visibility = { state: "visible" } }) {
  const errors = [], vconsole = new VirtualConsole();
  vconsole.on("jsdomError", (e) => errors.push(e.message));
  const dom = new JSDOM('<!doctype html><body data-page="native"><button id="call-audio-btn"></button><button id="call-video-btn"></button></body>', {
    url: "https://inbox.pigeonpost.dev/native-call.html", runScripts: "outside-only", pretendToBeVisual: true, virtualConsole: vconsole,
  });
  Object.defineProperty(dom.window.document, "visibilityState", { configurable: true, get: () => visibility.state });
  const w = dom.window, captures = [], requests = [], senders = [], readers = [], contexts = [];
  w.HTMLMediaElement.prototype.play = async () => {};
  w.HTMLMediaElement.prototype.pause = () => {};
  w.RTCPeerConnection = class {};
  w.MediaStream = Stream;
  w.TextDecoder = TextDecoder;
  w.ResizeObserver = class { observe() {} disconnect() {} };
  w.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
  w.webkit = { messageHandlers: { pigeonpost: { postMessage() {} } } };
  // Strong noise removal, with an audio graph that starts or never does.
  w.AudioWorkletNode = class {};
  w.AudioContext = class {
    constructor() { this.state = "suspended"; this.closed = 0; this.audioWorklet = { addModule: async () => {} }; contexts.push(this); }
    resume() { if (audio === "stalled") return new Promise(() => {}); this.state = "running"; return Promise.resolve(); }
    close() { this.closed++; this.state = "closed"; return Promise.resolve(); }
    createMediaStreamSource() { return { connect: (n) => n, disconnect() {} }; }
    createMediaStreamDestination() { const d = { stream: new Stream([new Track("audio")]) }; this.dest = d; return d; }
    createGain() { return { gain: {}, connect() {}, disconnect() {} }; }
    createOscillator() { return { frequency: {}, connect() {}, start() {}, stop() {} }; }
    get destination() { return {}; }
    get currentTime() { return 0; }
  };
  w.__noiseLib = async () => ({
    loadRnnoise: async () => new ArrayBuffer(8),
    RnnoiseWorkletNode: class { connect(d) { return d; } disconnect() {} destroy() {} },
  });
  Object.defineProperty(w.navigator, "mediaDevices", { value: {
    enumerateDevices: async () => [],
    getUserMedia: (constraints) => new Promise((resolve, reject) => captures.push({ constraints, resolve, reject })),
  } });
  const json = (body, status = 200) => ({ ok: status < 300, status, json: async () => body });
  w.fetch = async (url, options = {}) => {
    const path = new URL(url, w.location.href).pathname;
    requests.push({ path, body: options.body ? JSON.parse(options.body) : null });
    if (path.endsWith("/events")) {
      const queued = [], waiting = []; let closed = false;
      const done = () => { closed = true; for (const resolve of waiting.splice(0)) resolve({ done: true }); };
      const emit = (event, data) => {
        if (closed) return;
        const frame = { value: new TextEncoder().encode(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`), done: false };
        if (waiting.length) waiting.shift()(frame); else queued.push(frame);
      };
      options.signal.addEventListener("abort", done); readers.push(done); senders.push(emit);
      emit("ready", { session: "session-b", mailboxes: [] });
      return { ok: true, status: 200, body: { getReader: () => ({
        read: () => closed ? Promise.resolve({ done: true }) : queued.length ? Promise.resolve(queued.shift()) : new Promise((resolve) => waiting.push(resolve)),
      }) } };
    }
    return json({ ok: true });
  };
  // jsdom runs no module loader: the vendored noise library comes from the stub above instead.
  const src = read("calls.js");
  const load = 'import(NOISE_BASE + "index.js")';
  assert.ok(src.includes(load), "calls.js loads the noise library as this test expects");
  w.eval(read("native-call.js"));
  w.eval(src.replace(load, "window.__noiseLib()"));
  t.after(async () => {
    w.PigeonpostCalls?.stop();
    for (const done of readers) done();
    for (const c of captures) c.resolve(new Stream());
    await tick();
    w.close();
    assert.deepEqual(errors, [], "no uncaught browser errors");
  });
  return { w, captures, requests, contexts, event: (name, data) => senders.at(-1)(name, data) };
}

const ROOM = "R".repeat(22);
async function answerNatively(p, kind = "audio") {
  p.w.PigeonpostNative.configure({ token: "synthetic-b", mailbox: { address: "/k/b" }, known: {}, nativeRing: true });
  p.w.PigeonpostNative.answer(ROOM, null);
  await until(() => p.w.PigeonpostCalls.inspect().session === "session-b");
  p.event("incoming", { room: ROOM, code: "ABC234", kind, from: { address: "/k/a" }, to: "/k/b" });
  await until(() => p.captures.length === 1);
}
const accepted = (p) => p.requests.find((r) => r.path === `/calls/v1/rooms/${ROOM}/accept`);
async function joined(p, kind = "audio") {
  await until(() => accepted(p), 4000);
  p.event("joined", { room: ROOM, code: "ABC234", you: "me", kind, participants: [{ participant: "me", name: "B" }] });
  await until(() => p.w.PigeonpostCalls.inspect().room?.state === "active");
}
const inspect = (p) => p.w.PigeonpostCalls.inspect();
const shown = (p) => p.w.document.dispatchEvent(new p.w.Event("visibilitychange"));

test("a noise graph that stops mid-call gives way to the plain microphone", { timeout: 8000 }, async (t) => {
  const p = page(t, { audio: "running" });
  await answerNatively(p);
  const mic = new Track("audio");
  p.captures[0].resolve(new Stream([mic]));
  await joined(p);
  assert.equal(inspect(p).local.sent, "noise", "the call starts with the noise removal");
  const processed = p.contexts[0].dest.stream.getAudioTracks()[0];

  // WebKit interrupts the graph (an audio interruption): from now on it would send silence.
  p.contexts[0].state = "interrupted";
  p.contexts[0].onstatechange?.();
  await until(() => inspect(p).local.sent === "raw");
  assert.ok(inspect(p).revived.includes("noise"));
  assert.equal(mic.stops, 0, "the microphone itself is kept: it is what is sent now");
  assert.equal(processed.stops, 1, "the silent processed track is released");
  assert.ok(p.contexts[0].closed > 0, "the stopped graph is closed");
});

test("a microphone that ended is captured again when the page is shown", { timeout: 8000 }, async (t) => {
  const p = page(t, { audio: "stalled" });
  await answerNatively(p);
  const mic = new Track("audio");
  p.captures[0].resolve(new Stream([mic]));
  await joined(p);
  assert.equal(inspect(p).local.sent, "raw");

  mic.readyState = "ended"; // WebKit ended the capture (an interruption that did not recover)
  shown(p);
  await until(() => p.captures.length === 2);
  assert.ok(p.captures[1].constraints.audio, "the microphone is asked for again");
  assert.equal(p.captures[1].constraints.video, false, "and only the microphone");
  const fresh = new Track("audio");
  p.captures[1].resolve(new Stream([fresh]));
  await until(() => inspect(p).revived.includes("mic"));
  assert.equal(fresh.stops, 0, "the new microphone is the one in the call");
  assert.ok(mic.stops >= 1, "the dead one is released");
});

test("a camera that could not open in the background is opened when the page is shown", { timeout: 8000 }, async (t) => {
  const visibility = { state: "hidden" }; // the app was woken by a VoIP push, or the phone is locked
  const p = page(t, { audio: "stalled", visibility });
  await answerNatively(p, "video");
  assert.ok(p.captures[0].constraints.video, "a video call asks for the camera");
  p.captures[0].reject(Object.assign(new Error("camera unavailable in the background"), { name: "NotReadableError" }));
  await until(() => p.captures.length === 2);
  assert.equal(p.captures[1].constraints.video, false, "it goes ahead with the microphone");
  p.captures[1].resolve(new Stream([new Track("audio")]));
  await joined(p, "video");
  assert.equal(inspect(p).local.cam, false);
  assert.equal(inspect(p).local.cameraLater, true, "the camera is remembered for when the page is shown");

  shown(p); // still hidden: nothing is asked yet
  await tick(); await tick();
  assert.equal(p.captures.length, 2, "no camera request while the page is hidden");

  visibility.state = "visible"; // the person opens the app
  shown(p);
  await until(() => p.captures.length === 3);
  assert.equal(p.captures[2].constraints.audio, false);
  assert.ok(p.captures[2].constraints.video, "the camera is asked for again");
  p.captures[2].resolve(new Stream([new Track("video")]));
  await until(() => inspect(p).revived.includes("camera"));
  assert.equal(inspect(p).local.cam, true);
  assert.equal(inspect(p).local.cameraLater, false);
});
