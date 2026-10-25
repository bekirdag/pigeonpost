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
function page(t) {
  const errors = [], console = new VirtualConsole();
  console.on("jsdomError", (e) => errors.push(e.message));
  const dom = new JSDOM('<!doctype html><body data-page="native"><button id="call-audio-btn"></button><button id="call-video-btn"></button></body>', {
    url: "https://inbox.pigeonpost.dev/native-call.html", runScripts: "outside-only", pretendToBeVisual: true, virtualConsole: console,
  });
  const w = dom.window, captures = [], requests = [], responses = [], eventReaders = [];
  w.HTMLMediaElement.prototype.play = async () => {};
  w.HTMLMediaElement.prototype.pause = () => {};
  w.RTCPeerConnection = class {};
  w.MediaStream = Stream;
  w.TextDecoder = TextDecoder;
  w.ResizeObserver = class { observe() {} disconnect() {} };
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
      let first = true, done;
      const stopped = new Promise((resolve) => { done = resolve; });
      options.signal.addEventListener("abort", () => done({ done: true }));
      eventReaders.push(done);
      const identity = options.headers.authorization.endsWith("synthetic-b") ? "b" : "a";
      return { ok: true, status: 200, body: { getReader: () => ({
        read: () => first ? (first = false, Promise.resolve({ value: new TextEncoder().encode(`event: ready\ndata: ${JSON.stringify({ session: "session-" + identity, mailboxes: [] })}\n\n`), done: false })) : stopped,
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
  return { w, captures, requests, responses, configure, join };
}

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
