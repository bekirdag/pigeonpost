// An answered call is accepted even when the page cannot start audio processing.
//
// 2026-10-29: iPhones answering from CallKit (Pigeonpost 1.1 (50)) never accepted. The microphone
// was captured, then strong noise removal awaited AudioContext.resume(), which WebKit leaves pending
// while the page may not start audio (no tap in the page; the phone locked). The answer was never
// sent: the caller rang out and the callee sat on "Connecting…". This drives the real calls client
// through the native bridge's answer with a resume() that never settles.
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
  constructor(kind) { this.kind = kind; this.enabled = true; this.stops = 0; this.id = kind + Math.random(); }
  stop() { this.stops++; }
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
function page(t, { audio }) {
  const errors = [], vconsole = new VirtualConsole();
  vconsole.on("jsdomError", (e) => errors.push(e.message));
  const dom = new JSDOM('<!doctype html><body data-page="native"><button id="call-audio-btn"></button><button id="call-video-btn"></button></body>', {
    url: "https://inbox.pigeonpost.dev/native-call.html", runScripts: "outside-only", pretendToBeVisual: true, virtualConsole: vconsole,
  });
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
    getUserMedia: () => new Promise((resolve, reject) => captures.push({ resolve, reject })),
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
async function answerNatively(p) {
  p.w.PigeonpostNative.configure({ token: "synthetic-b", mailbox: { address: "/k/b" }, known: {}, nativeRing: true });
  p.w.PigeonpostNative.answer(ROOM, null);
  await until(() => p.w.PigeonpostCalls.inspect().session === "session-b");
  p.event("incoming", { room: ROOM, code: "ABC234", kind: "audio", from: { address: "/k/a" }, to: "/k/b" });
  await until(() => p.captures.length === 1);
  const mic = new Track("audio");
  p.captures[0].resolve(new Stream([mic]));
  return mic;
}
const accepted = (p) => p.requests.find((r) => r.path === `/calls/v1/rooms/${ROOM}/accept`);

test("an answer from CallKit is accepted although WebKit never starts the noise removal's audio", { timeout: 8000 }, async (t) => {
  const p = page(t, { audio: "stalled" });
  const started = Date.now();
  const mic = await answerNatively(p);
  await until(() => accepted(p), 5000);
  assert.ok(Date.now() - started < 4000, "the answer is not held up for long");
  assert.equal(accepted(p).body.session, "session-b");
  assert.equal(mic.stops, 0, "the microphone stays on: it is what the call sends");
  assert.equal(p.contexts.length, 1);
  assert.ok(p.contexts[0].closed > 0, "the audio graph that never started is closed");
});

test("with audio allowed, the same answer goes through the noise removal", { timeout: 8000 }, async (t) => {
  const p = page(t, { audio: "running" });
  await answerNatively(p);
  await until(() => accepted(p), 3000);
  assert.equal(p.contexts.length, 1);
  assert.equal(p.contexts[0].state, "running");
  assert.equal(p.contexts[0].closed, 0, "the running noise graph is kept for the call");
});
