// Pigeonpost — the frame-encryption worker for large calls.
//
// This is LiveKit's own worker (vendor/livekit-client, unmodified), with one difference: room keys
// arrive here as raw bytes and are turned into a CryptoKey inside the worker. LiveKit itself posts
// a CryptoKey, and some WebKit web views (the iOS app's, for one) cannot copy a CryptoKey into a
// worker ("The object can not be cloned"), which would leave those devices without media. The key
// stays in this origin's memory either way; it is never sent anywhere else.
"use strict";

let handler = null;
// LiveKit's worker installs `onmessage`; keep it, and hand it every message in arrival order.
Object.defineProperty(self, "onmessage", { configurable: true, get() { return handler; }, set(fn) { handler = fn; } });

let queue = Promise.resolve();
self.addEventListener("message", (event) => {
  const message = event.data;
  queue = queue.then(async () => {
    if (message && message.kind === "setKey" && message.data && message.data.key instanceof ArrayBuffer) {
      message.data.key = await crypto.subtle.importKey("raw", message.data.key, "HKDF", false, ["deriveBits", "deriveKey"]);
    }
    if (handler) handler.call(self, { data: message });
  }).catch((err) => console.warn("calls: frame-encryption worker message failed", err && err.name));
});

importScripts("/vendor/livekit-client/livekit-client.e2ee.worker.js");
