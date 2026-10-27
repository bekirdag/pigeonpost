// The real calls client with delayed media/HTTP, without a camera, microphone or external server.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { JSDOM, VirtualConsole } from "jsdom";

const read = (file) => readFileSync(new URL("../" + file, import.meta.url), "utf8");
const tick = () => new Promise((resolve) => setTimeout(resolve, 5));
async function until(predicate) {
  for (let n = 0; n < 400; n++) { if (predicate()) return; await tick(); }
  assert.fail("Synthetic call did not settle");
}
class Track {
  constructor(kind) { this.kind = kind; this.enabled = true; this.stops = 0; this.id = kind; }
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
function page(t, { strongNoise = false } = {}) {
  const errors = [], console = new VirtualConsole();
  console.on("jsdomError", (e) => errors.push(e.message));
  const dom = new JSDOM('<!doctype html><body data-page="native"><button id="call-audio-btn"></button><button id="call-video-btn"></button></body>', {
    url: "https://inbox.pigeonpost.dev/native-call.html", runScripts: "outside-only", pretendToBeVisual: true, virtualConsole: console,
  });
  const w = dom.window, captures = [], requests = [], responses = [], eventReaders = [], eventSenders = [];
  w.HTMLMediaElement.prototype.play = async () => {};
  w.HTMLMediaElement.prototype.pause = () => {};
  w.RTCPeerConnection = class {};
  w.MediaStream = Stream;
  w.TextDecoder = TextDecoder;
  w.ResizeObserver = class { observe() {} disconnect() {} };
  if (strongNoise) w.AudioWorkletNode = class {};
  w.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
  Object.defineProperty(w.navigator, "mediaDevices", { value: {
    enumerateDevices: async () => [],
    getUserMedia: () => new Promise((resolve, reject) => captures.push({ resolve, reject })),
  } });
  const json = (body, status = 200) => ({ ok: status < 300, status, json: async () => body });
  w.fetch = async (url, options = {}) => {
    const path = new URL(url, w.location.href).pathname;
    requests.push({ path, headers: options.headers, body: options.body ? JSON.parse(options.body) : null });
    if (path.endsWith("/events")) {
      const queued = [], readers = []; let closed = false;
      const done = () => { closed = true; for (const resolve of readers.splice(0)) resolve({ done: true }); };
      const emit = (event, data) => {
        if (closed) return;
        const frame = { value: new TextEncoder().encode(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`), done: false };
        if (readers.length) readers.shift()(frame); else queued.push(frame);
      };
      options.signal.addEventListener("abort", done); eventReaders.push(done); eventSenders.push(emit);
      const identity = options.headers.authorization.endsWith("synthetic-b") ? "b" : "a";
      emit("ready", { session: "session-" + identity, mailboxes: [] });
      return { ok: true, status: 200, body: { getReader: () => ({
        read: () => closed ? Promise.resolve({ done: true }) : queued.length ? Promise.resolve(queued.shift()) : new Promise(resolve => readers.push(resolve)),
      }) } };
    }
    if (path.includes("/meetings/scheduled/")) return new Promise((resolve) => responses.push((body, status) => resolve(json(body, status))));
    return json({ ok: true });
  };
  w.eval(read("native-call.js"));
  w.eval(process.env.PP_CALLS_TEST_SOURCE ? readFileSync(process.env.PP_CALLS_TEST_SOURCE, "utf8") : read("calls.js"));
  t.after(async () => {
    w.PigeonpostCalls?.stop();
    for (const done of eventReaders) done({ done: true });
    for (const respond of responses) respond({ error: "cancelled" }, 403);
    for (const capture of captures) capture.resolve(new Stream());
    await tick();
    w.close();
    assert.deepEqual(errors, [], "no uncaught browser errors");
  });
  const configure = (id) => w.PigeonpostNative.configure({ token: "synthetic-" + id, mailbox: { address: "/k/" + id }, known: {} });
  configure("a");
  const join = (occurrence, id = "a") => w.PigeonpostCalls.joinScheduled("ABCDEFGHJK", { as: "/k/" + id, occurrenceId: occurrence, kind: "audio" });
  return { w, captures, requests, responses, configure, join, event: (name, data) => eventSenders.at(-1)(name, data) };
}

test("a late host sees earlier waiting people and duplicate joined snapshots do not revive resolved requests", { timeout: 6000 }, async (t) => {
  const p = page(t); const joining = p.join("occ_first");
  await until(() => p.captures.length === 1); p.captures[0].resolve(new Stream([new Track("audio")]));
  await until(() => p.responses.length === 1);
  const snapshot = { room: "a".repeat(32), code: "ABCDEFGHJK", you: "host", kind: "audio", participants: [],
    scheduled: { role: "host", can_admit: true, knocks: [{ knock: "b".repeat(32), name: "Earlier guest", guest: true, verified: false }] } };
  p.event("joined", snapshot);
  await until(() => p.w.PigeonpostCalls.inspect().knocks.length === 1);
  assert.match(p.w.document.getElementById("call-knocks").textContent, /Earlier guest/);
  assert.ok(p.w.document.querySelector("#call-knocks .call-pill-primary"), "the host can admit the earlier guest");
  p.event("knock_resolved", { room: snapshot.room, knock: "b".repeat(32) });
  await until(() => p.w.PigeonpostCalls.inspect().knocks.length === 0);
  p.event("knock", { room: snapshot.room, knock: "c".repeat(32), name: "Live guest", guest: true });
  await until(() => p.w.PigeonpostCalls.inspect().knocks.length === 1);
  p.responses[0]({ state: "joined", ...snapshot }); assert.equal((await joining).state, "joined");
  assert.deepEqual(Array.from(p.w.PigeonpostCalls.inspect().knocks), ["c".repeat(32)], "a stale second joined snapshot preserves live requests and keeps resolved ones removed");
});

test("an attendee cannot bootstrap a private host lobby from an unsolicited snapshot", { timeout: 6000 }, async (t) => {
  const p = page(t); const joining = p.join("occ_first");
  await until(() => p.captures.length === 1); p.captures[0].resolve(new Stream([new Track("audio")]));
  await until(() => p.responses.length === 1);
  p.responses[0]({ state: "joined", room: "a".repeat(32), you: "attendee", participants: [], scheduled: { role: "attendee", can_admit: false, knocks: [{ knock: "b".repeat(32), name: "Private guest" }] } });
  assert.equal((await joining).state, "joined");
  assert.equal(p.w.PigeonpostCalls.inspect().knocks.length, 0);
  assert.equal(p.w.document.getElementById("call-host-tools").hidden, true);
});

test("late media from a cancelled attempt is released and cannot replace the next call", { timeout: 6000 }, async (t) => {
  const p = page(t);
  const old = p.join("occ-old");
  await until(() => p.captures.length === 1);
  p.w.PigeonpostCalls.hangUp();
  const next = p.join("occ-next");
  await until(() => p.captures.length === 2);
  const currentTrack = new Track("audio"), oldTrack = new Track("audio");
  p.captures[1].resolve(new Stream([currentTrack]));
  await until(() => p.responses.length === 1);
  p.responses[0]({ state: "waiting", room: "synthetic-room-b", knock: "knock-b" });
  assert.equal((await next).state, "waiting");
  p.captures[0].resolve(new Stream([oldTrack]));
  await until(() => oldTrack.stops > 0 || p.responses.length > 1);
  assert.equal(oldTrack.stops, 1, "late microphone is released");
  assert.equal((await old).state, "cancelled");
  assert.equal(currentTrack.stops, 0, "new call's microphone remains in use");
  const joins = p.requests.filter((r) => r.path.includes("/meetings/scheduled/"));
  assert.equal(joins.length, 1);
  assert.equal(joins[0].body.occurrence_id, "occ-next");
  assert.equal(p.w.document.getElementById("call-lobby").hidden, false);
});

test("a delayed unauthorized join never renews or retries as the replacement account", { timeout: 6000 }, async (t) => {
  const p = page(t);
  let renewals = 0;
  p.w.PigeonpostCallsHost.renew = async () => { renewals++; return true; };
  const old = p.join("occ-old");
  await until(() => p.captures.length === 1);
  p.captures[0].resolve(new Stream([new Track("audio")]));
  await until(() => p.responses.length === 1);
  p.configure("b");
  const next = p.join("occ-next", "b");
  await until(() => p.captures.length === 2);
  p.captures[1].resolve(new Stream([new Track("audio")]));
  await until(() => p.responses.length === 2);
  p.responses[1]({ state: "waiting", room: "synthetic-room-b", knock: "knock-b" });
  assert.equal((await next).state, "waiting");
  p.responses[0]({ error: "unauthorized" }, 401);
  await tick();
  assert.equal(renewals, 0);
  assert.equal((await old).state, "cancelled");
  assert.equal(p.requests.filter((r) => r.path.includes("/meetings/scheduled/")).length, 2);
});

test("a cancelled direct capture cannot place a call over a new scheduled lobby", { timeout: 6000 }, async (t) => {
  const p = page(t);
  await until(() => p.w.PigeonpostCalls.inspect().session);
  p.w.PigeonpostNative.call("/test/peer", "audio");
  await until(() => p.captures.length === 1);
  p.w.PigeonpostCalls.hangUp();
  const next = p.join("occ-next");
  await until(() => p.captures.length === 2);
  const currentTrack = new Track("audio"), oldTrack = new Track("audio");
  p.captures[1].resolve(new Stream([currentTrack]));
  await until(() => p.responses.length === 1);
  p.responses[0]({ state: "waiting", room: "synthetic-room-b", knock: "knock-b" });
  assert.equal((await next).state, "waiting");
  p.captures[0].resolve(new Stream([oldTrack]));
  await until(() => oldTrack.stops > 0);
  await tick();
  assert.equal(p.requests.filter((r) => r.path === "/calls/v1/calls").length, 0);
  assert.equal(currentTrack.stops, 0);
});

test("hang-up immediately stops a captured microphone while asynchronous noise setup is pending", { timeout: 6000 }, async (t) => {
  const p = page(t, { strongNoise: true });
  const joining = p.join("occ-noise");
  await until(() => p.captures.length === 1);
  const track = new Track("audio");
  const stopped = new Promise((resolve) => {
    let queued = false;
    Object.defineProperty(track, "contentHint", { set() {
      if (queued) return;
      queued = true;
      // The client has received the track and starts preparing it for speech. Act in the next
      // microtask, while noise setup is awaiting its module, rather than before capture returns.
      p.w.queueMicrotask(() => { p.w.PigeonpostCalls.hangUp(); resolve(track.stops); });
    } });
    p.captures[0].resolve(new Stream([track]));
  });
  assert.ok(await stopped > 0, "capture is released at hang-up, without waiting for noise setup");
  assert.equal((await joining).state, "cancelled");
  assert.equal(p.responses.length, 0);
});

test("a late refusal for a cancelled join cannot end the replacement call", { timeout: 6000 }, async (t) => {
  const p = page(t);
  const old = p.join("occ-old");
  await until(() => p.captures.length === 1);
  p.captures[0].resolve(new Stream([new Track("audio")]));
  await until(() => p.responses.length === 1);
  p.w.PigeonpostCalls.hangUp();
  const next = p.join("occ-next");
  await until(() => p.captures.length === 2);
  const currentTrack = new Track("audio");
  p.captures[1].resolve(new Stream([currentTrack]));
  await until(() => p.responses.length === 2);
  p.responses[1]({ state: "waiting", room: "synthetic-room-b", knock: "knock-b" });
  assert.equal((await next).state, "waiting");
  p.responses[0]({ error: "window_closed" }, 403);
  assert.equal((await old).state, "cancelled");
  assert.equal(currentTrack.stops, 0);
  assert.equal(p.w.document.getElementById("call-lobby").hidden, false);
});

test("an accepted abandoned join is released with its original identity", { timeout: 6000 }, async (t) => {
  const p = page(t);
  const old = p.join("occ-old");
  await until(() => p.captures.length === 1);
  p.captures[0].resolve(new Stream([new Track("audio")]));
  await until(() => p.responses.length === 1);
  p.configure("b");
  const next = p.join("occ-next", "b");
  await until(() => p.captures.length === 2);
  const currentTrack = new Track("audio");
  p.captures[1].resolve(new Stream([currentTrack]));
  await until(() => p.responses.length === 2);
  p.responses[1]({ state: "waiting", room: "synthetic-room-b", knock: "knock-b" });
  assert.equal((await next).state, "waiting");
  p.responses[0]({ state: "joined", room: "synthetic-room-a" });
  assert.equal((await old).state, "cancelled");
  await until(() => p.requests.some((r) => r.path.endsWith("/synthetic-room-a/leave")));
  const leave = p.requests.find((r) => r.path.endsWith("/synthetic-room-a/leave"));
  assert.equal(leave.headers.authorization, "Bearer synthetic-a");
  assert.equal(leave.body.session, "session-a");
  assert.equal(currentTrack.stops, 0);
});
