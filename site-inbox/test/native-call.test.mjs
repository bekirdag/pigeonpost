// The native apps' call bridge (native-call.js): the faces an app hands over are painted on the
// call's avatars, anything else leaves the initials alone.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { JSDOM } from "jsdom";

function page() {
  const dom = new JSDOM("<!doctype html><body><span class=avatar id=a>AB</span></body>", { runScripts: "outside-only" });
  dom.window.eval(readFileSync(new URL("../native-call.js", import.meta.url), "utf8"));
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

test("an app that sends no avatars leaves every tile on its initials", () => {
  const w = page();
  w.PigeonpostNative.configure({ token: null, mailbox: null, known: {} });
  const node = w.document.getElementById("a");
  w.PigeonpostCallsHost.decorateAvatar(node, "/bob");
  assert.ok(!node.classList.contains("has-photo"));
});
