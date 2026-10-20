// Phase 1c: the bridge the native apps use to show incoming calls themselves (docs/call-alerting.md).
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { JSDOM } from "jsdom";

const read = (name) => readFileSync(new URL("../" + name, import.meta.url), "utf8");

function bridge() {
  const dom = new JSDOM(`<!doctype html><body data-page="native"></body>`, { url: "https://inbox.pigeonpost.dev/native-call.html", runScripts: "outside-only" });
  const w = dom.window;
  const told = [], acted = [];
  w.webkit = { messageHandlers: { pigeonpost: { postMessage: (m) => told.push(JSON.parse(JSON.stringify(m))) } } };
  w.PigeonpostCalls = {
    start: () => acted.push(["start"]), stop: () => acted.push(["stop"]),
    answer: (...a) => acted.push(["answer", ...a]), decline: (...a) => acted.push(["decline", ...a]),
    setMuted: (m) => acted.push(["setMuted", m]), hangUp: () => acted.push(["hangUp"]),
  };
  w.eval(read("native-call.js"));
  return { w, told, acted };
}

test("an app that rings natively is told about the ring and the page stays quiet", () => {
  const { w, told } = bridge();
  const ring = { room: "r".repeat(22), code: "ABC234", kind: "video", from: { address: "/k/a", handle: "/alice" }, to: "/bob" };
  w.PigeonpostNative.configure({ token: "t", mailbox: { address: "/k/b", handle: "/bob" }, known: {} });
  assert.equal(w.PigeonpostCallsHost.ring(ring), false, "without nativeRing the page shows its own ring screen");
  w.PigeonpostNative.configure({ token: "t", mailbox: { address: "/k/b", handle: "/bob" }, known: {}, nativeRing: true });
  assert.equal(w.PigeonpostCallsHost.ring(ring), true);
  w.PigeonpostCallsHost.ringEnded({ room: ring.room, reason: "cancelled" });
  w.PigeonpostCallsHost.callState({ state: "active", room: ring.room, kind: "video" });
  w.PigeonpostCallsHost.muted(1);
  const relevant = told.filter((m) => m.type !== "ready" && m.type !== "mode");
  assert.deepEqual(relevant, [
    { type: "ring", ...ring }, { type: "ring", ...ring },
    { type: "ring_ended", room: ring.room, reason: "cancelled" },
    { type: "call", state: "active", room: ring.room, kind: "video" },
    { type: "muted", muted: true },
  ]);
});

test("the app's answer, decline, mute and hang-up reach the call client", () => {
  const { w, acted } = bridge();
  w.PigeonpostNative.answer("room-id-1234", "T".repeat(43));
  w.PigeonpostNative.decline("room-id-1234");
  w.PigeonpostNative.setMuted(1);
  w.PigeonpostNative.hangUp();
  assert.deepEqual(acted, [["answer", "room-id-1234", "T".repeat(43)], ["decline", "room-id-1234", null], ["setMuted", true], ["hangUp"]]);
});

test("the call client exposes the answering API and the link parameter", () => {
  const src = read("calls.js");
  for (const name of ["answer: answerFromOutside", "decline: declineFromOutside", "setMuted:", "hangUp:"]) assert.ok(src.includes(name), name);
  assert.ok(src.includes('searchParams.get("answer")'));
  assert.ok(src.includes("ring_token"));
});
