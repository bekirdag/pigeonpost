import { test } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import crypto from "node:crypto";

const require = createRequire(import.meta.url);
const E = require("../calls-e2ee.js");

async function people(n) {
  const out = [];
  for (let i = 0; i < n; i++) {
    const id = await E.generateIdentity();
    out.push({ id: "p" + String(i).padStart(2, "0") + "xxxxxxxxxxxx", name: "Person " + i, mailbox: i % 2 ? { address: "/k/m" + i } : null, key: id.publicKey, privateKey: id.privateKey });
  }
  return out;
}
const pub = (p) => ({ id: p.id, key: p.key });

test("public keys are uncompressed P-256 points; private halves cannot be exported", async () => {
  const me = await E.generateIdentity();
  assert.ok(E.isPublicKey(me.publicKey));
  assert.equal(E.unb64u(me.publicKey).length, 65);
  assert.equal(E.unb64u(me.publicKey)[0], 4);
  assert.equal(me.privateKey.extractable, false);
  for (const bad of ["", "A".repeat(87), "B".repeat(86), "B" + "/".repeat(86), null, 5]) assert.equal(E.isPublicKey(bad), false);
});

test("a wrapped room key opens only for its addressee, from its sender, in its room and epoch", async () => {
  const [a, b, c] = await people(3);
  const roomKey = E.randomRoomKey();
  const wrapped = await E.wrapRoomKey({ privateKey: a.privateKey, roomId: "room1", epoch: 3, from: pub(a), to: pub(b), roomKey });
  assert.deepEqual(Object.keys(wrapped).sort(), ["epoch", "iv", "key"]);
  assert.match(wrapped.iv, /^[A-Za-z0-9_-]{16}$/);
  assert.match(wrapped.key, /^[A-Za-z0-9_-]{64}$/); // the shapes the call service accepts
  const opened = await E.unwrapRoomKey({ privateKey: b.privateKey, roomId: "room1", from: pub(a), to: pub(b), wrapped });
  assert.deepEqual(opened, roomKey);
  const fails = (args) => assert.rejects(E.unwrapRoomKey({ privateKey: b.privateKey, roomId: "room1", from: pub(a), to: pub(b), wrapped, ...args }));
  await fails({ privateKey: c.privateKey }); // someone else
  await fails({ roomId: "room2" });
  await fails({ wrapped: { ...wrapped, epoch: 4 } }); // replayed as another generation
  await fails({ from: pub(c) }); // believed to come from someone else
  await fails({ from: { id: "other-id", key: a.key } }); // or under another participant id
  await fails({ to: { id: "someone-else", key: b.key } });
  const flipped = E.unb64u(wrapped.key); flipped[0] ^= 1;
  await fails({ wrapped: { ...wrapped, key: E.b64u(flipped) } });
  // A substituted public key (a service in the middle) yields a different wrapping key.
  const [m] = await people(1);
  await assert.rejects(E.unwrapRoomKey({ privateKey: b.privateKey, roomId: "room1", from: { id: a.id, key: m.key }, to: pub(b), wrapped }));
});

test("the security code is the same for everyone who sees the same room, and changes with any substitution", async () => {
  const room = await people(7);
  const view = room.map((p) => ({ id: p.id, name: p.name, mailbox: p.mailbox, key: p.key }));
  const code = await E.securityCode("room1", view);
  assert.match(code, /^\d{4} \d{4} \d{4}$/);
  // Order of arrival does not matter.
  assert.equal(await E.securityCode("room1", [...view].reverse()), code);
  // A substituted key, a relabelled guest, a hidden extra participant, another room: all change it.
  const [m] = await people(1);
  assert.notEqual(await E.securityCode("room1", view.map((p, i) => (i === 3 ? { ...p, key: m.key } : p))), code);
  assert.notEqual(await E.securityCode("room1", view.map((p, i) => (i === 0 ? { ...p, name: "Someone else" } : p))), code);
  assert.notEqual(await E.securityCode("room1", [...view, { id: "zz-ghost", name: "", key: m.key }]), code);
  assert.notEqual(await E.securityCode("room2", view), code);
  // A member's mailbox, not their display name, is what identifies them.
  assert.equal(E.memberLabel({ mailbox: { address: "/K/Alice1" }, name: "anything" }), "mailbox:/k/alice1");
  assert.equal(E.memberLabel({ name: "Dana" }), "guest:Dana");
  // Somebody who has not announced a key yet is not part of it.
  assert.equal(await E.securityCode("room1", [...view, { id: "zz-new", name: "New" }]), code);
});

test("the security code is deliberately slow, so a service in the middle cannot search for matching fakes", async () => {
  const room = await people(5);
  const t0 = performance.now();
  await E.securityCode("room1", room);
  const ms = performance.now() - t0;
  // 100,000 PBKDF2 rounds: comfortably fast once per membership change, ~10^5 times dearer to search.
  assert.ok(ms > 5, "took " + ms + " ms");
  assert.ok(ms < 2000, "took " + ms + " ms");
  const src = readFileSync(new URL("../calls-e2ee.js", import.meta.url), "utf8");
  assert.match(src, /SAS_ROUNDS = 100_000/);
});

test("sealed chat and status messages open only with that epoch's key, for that room and sender", async () => {
  const k1 = await E.dataKey(E.randomRoomKey()), k2 = await E.dataKey(E.randomRoomKey());
  const meta = { epoch: 7, roomId: "room1", senderId: "pA" };
  const sealed = await E.sealData(k1, meta, { t: "chat", text: "hello" });
  assert.equal(sealed[0], 1);
  assert.ok(!Buffer.from(sealed).toString("latin1").includes("hello"));
  const keys = new Map([[7, k1], [8, k2]]);
  const keyFor = async (e) => keys.get(e) || null;
  assert.deepEqual(await E.openData(keyFor, { roomId: "room1", senderId: "pA" }, sealed), { epoch: 7, value: { t: "chat", text: "hello" } });
  await assert.rejects(E.openData(keyFor, { roomId: "room1", senderId: "pB" }, sealed)); // attributed to someone else
  await assert.rejects(E.openData(keyFor, { roomId: "room2", senderId: "pA" }, sealed));
  await assert.rejects(E.openData(async () => k2, { roomId: "room1", senderId: "pA" }, sealed)); // wrong key
  await assert.rejects(E.openData(async () => null, { roomId: "room1", senderId: "pA" }, sealed)); // left before this epoch
  const tampered = sealed.slice(); tampered[tampered.length - 1] ^= 1;
  await assert.rejects(E.openData(keyFor, { roomId: "room1", senderId: "pA" }, tampered));
  await assert.rejects(E.openData(keyFor, { roomId: "room1", senderId: "pA" }, new Uint8Array(20_000)));
});

test("frame encryption support is detected from the two browser mechanisms, not from the browser's name", () => {
  const sender = function () {};
  const w = { crypto: globalThis.crypto, Worker: function () {}, RTCRtpSender: sender };
  assert.equal(E.frameEncryptionSupported(w), false);
  sender.prototype.createEncodedStreams = function () {};
  assert.equal(E.frameEncryptionSupported(w), true);
  assert.equal(E.frameEncryptionSupported({ crypto: globalThis.crypto, Worker: function () {}, RTCRtpScriptTransform: function () {} }), true);
  assert.equal(E.frameEncryptionSupported({ crypto: globalThis.crypto, RTCRtpScriptTransform: function () {} }), false);
  assert.equal(E.slotFor(17), 1);
});

// The vendored LiveKit client must be exactly the pinned, published build.
test("vendored LiveKit files match their pinned hashes", () => {
  const dir = new URL("../vendor/livekit-client/", import.meta.url);
  const readme = readFileSync(new URL("README.md", dir), "utf8");
  const pins = [...readme.matchAll(/^\| `([^`]+)` \| `sha384-([A-Za-z0-9+/=]+)` \|/gm)];
  assert.ok(pins.length >= 2, "pins listed in vendor README");
  for (const [, name, want] of pins) {
    const got = crypto.createHash("sha384").update(readFileSync(new URL(name, dir))).digest("base64");
    assert.equal(got, want, name);
  }
  // calls.js loads the client with the same integrity value.
  const calls = readFileSync(new URL("../calls.js", import.meta.url), "utf8");
  const umd = pins.find(([, n]) => n === "livekit-client.umd.js");
  assert.ok(calls.includes("sha384-" + umd[2]), "calls.js pins the client's hash");
});
