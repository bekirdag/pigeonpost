// The native apps' call bridge (native-call.js): the faces an app hands over are painted on the
// call's avatars, anything else leaves the initials alone.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { JSDOM } from "jsdom";

function page(t) {
  const dom = new JSDOM("<!doctype html><body><span class=avatar id=a>AB</span></body>", { runScripts: "outside-only" });
  dom.window.eval(readFileSync(new URL("../native-call.js", import.meta.url), "utf8"));
  t?.after(() => dom.window.close());
  return dom.window;
}

test("decorateAvatar paints a face the app supplied and clears it otherwise", () => {
  const w = page();
  const face = "data:image/jpeg;base64,/9j/4AAQ";
  w.PigeonpostNative.configure({ token: null, mailbox: null, known: { "/bob": "Bob" }, avatars: { "/bob": face, "/eve": "https://evil.test/x.png" } });
  const node = w.document.getElementById("a");
  w.PigeonpostCallsHost.decorateAvatar(node, "/bob");
  assert.ok(node.classList.contains("has-photo"));
  assert.match(node.style.backgroundImage, /data:image\/jpeg/);
  // Only data: images are painted: a page URL from anywhere else is ignored.
  w.PigeonpostCallsHost.decorateAvatar(node, "/eve");
  assert.ok(!node.classList.contains("has-photo"));
  assert.equal(node.style.backgroundImage, "");
  w.PigeonpostCallsHost.decorateAvatar(node, "/nobody");
  assert.ok(!node.classList.contains("has-photo"));
});

test("scheduled member joins forward the selected occurrence and keep server authority", async (t) => {
  const w = page(t), joins = [];
  w.PigeonpostCalls = { stop() {}, start() {}, joinScheduled: async (...args) => { joins.push(JSON.parse(JSON.stringify(args))); return { state: "waiting", secret: "never returned" }; } };
  w.PigeonpostNative.configure({ token: "synthetic", mailbox: { address: "/k/member" } });
  const result = await w.PigeonpostNative.joinScheduled("ABCDEFGHJK", { as: "/k/member", occurrenceId: "opaque-original-occurrence", kind: "audio", title: "Review", role: "host", ticket: "ignored", guest: true });
  assert.deepEqual(JSON.parse(JSON.stringify(result)), { state: "waiting" });
  assert.deepEqual(joins, [["ABCDEFGHJK", { as: "/k/member", occurrenceId: "opaque-original-occurrence", kind: "audio", title: "Review" }]]);
});

test("invalid or stale member joins never reach the call client", async (t) => {
  const w = page(t), joins = [];
  w.PigeonpostCalls = { stop() {}, start() {}, joinScheduled: (...args) => { joins.push(args); return { state: "joined" }; } };
  const join = w.PigeonpostNative.joinScheduled;
  assert.equal((await join("ABCDEFGHJK", { as: "/k/member", occurrenceId: "occ" })).code, "identity_changed");
  w.PigeonpostNative.configure({ token: "synthetic", mailbox: { address: "/k/member" } });
  for (const [code, options] of [["https://evil.test", { as: "/k/member", occurrenceId: "occ" }], ["ABCDEFGHJK", { as: "/k/member", occurrenceId: "" }], ["ABCDEFGHJK", { as: "/k/other", occurrenceId: "occ" }]]) {
    assert.equal((await join(code, options)).state, "error");
  }
  assert.equal(joins.length, 0);
});

test("sign-out or mailbox switching settles pending renewal and join without restoring the old result", async (t) => {
  const w = page(t);
  let finish;
  w.PigeonpostCalls = { stop() {}, start() {}, joinScheduled: () => new Promise((resolve) => { finish = resolve; }) };
  w.PigeonpostNative.configure({ token: "synthetic", mailbox: { address: "/k/member" } });
  const renewal = w.PigeonpostCallsHost.renew();
  const joined = w.PigeonpostNative.joinScheduled("ABCDEFGHJK", { as: "/k/member", occurrenceId: "occ" });
  await Promise.resolve();
  w.PigeonpostNative.configure({ token: "other", mailbox: { address: "/k/other" } });
  assert.equal(await renewal, false);
  assert.equal((await joined).state, "cancelled");
  finish({ state: "joined" });
  const renewal2 = w.PigeonpostCallsHost.renew();
  w.PigeonpostNative.signOut();
  assert.equal(await renewal2, false);
  assert.equal(w.PigeonpostCallsHost.signedIn(), false);
});

test("an older call page and call-service errors produce explicit bounded results", async (t) => {
  const w = page(t);
  w.PigeonpostNative.configure({ token: "synthetic", mailbox: { address: "/k/member" } });
  const join = () => w.PigeonpostNative.joinScheduled("ABCDEFGHJK", { as: "/k/member", occurrenceId: "occ" });
  assert.equal((await join()).code, "bridge_unavailable");
  w.PigeonpostCalls = { joinScheduled: async () => ({ state: "error", code: "window_closed", message: "Closed" }) };
  assert.deepEqual(JSON.parse(JSON.stringify(await join())), { state: "error", code: "window_closed", message: "Closed" });
  w.PigeonpostCalls.joinScheduled = async () => ({ state: "choose_occurrence", occurrences: [{ private: true }] });
  assert.deepEqual(JSON.parse(JSON.stringify(await join())), { state: "choose_occurrence" });
});

test("a mailbox switch before the queued join starts sends no request as the new identity", async (t) => {
  const w = page(t), joins = [];
  w.PigeonpostCalls = { stop() {}, start() {}, joinScheduled: (...args) => { joins.push(args); return { state: "joined" }; } };
  w.PigeonpostNative.configure({ token: "synthetic", mailbox: { address: "/k/member" } });
  const joining = w.PigeonpostNative.joinScheduled("ABCDEFGHJK", { as: "/k/member", occurrenceId: "old-occurrence" });
  w.PigeonpostNative.configure({ token: "synthetic-other", mailbox: { address: "/k/other" } });
  assert.equal((await joining).state, "cancelled");
  await Promise.resolve();
  assert.equal(joins.length, 0);
});

test("an app that sends no avatars leaves every tile on its initials", () => {
  const w = page();
  w.PigeonpostNative.configure({ token: null, mailbox: null, known: {} });
  const node = w.document.getElementById("a");
  w.PigeonpostCallsHost.decorateAvatar(node, "/bob");
  assert.ok(!node.classList.contains("has-photo"));
});
