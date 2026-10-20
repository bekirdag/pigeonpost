// Pigeonpost — end-to-end encryption for large calls (the ones that run through the media server).
//
// In a call of more than six people every browser encrypts each audio and video frame itself, with a
// room key, before the frame leaves the device (LiveKit's frame encryption, AES-GCM). The media server
// forwards frames it cannot open. This file is how that room key gets from browser to browser
// without the call service or the media server ever holding it:
//
// - Each browser makes a fresh key pair for the call (ECDH P-256; the private half cannot be
//   exported) and announces the public half through the call service.
// - One participant — the earliest one holding the newest room key — makes a new random room key
//   whenever someone joins or leaves, and wraps it to every other participant's public key:
//   ECDH → HKDF-SHA-256 → AES-256-GCM, bound to the room, both participants, both public keys and the
//   key generation ("epoch"). Only the addressee can unwrap it.
// - Chat and status messages in a large call are sealed with a key derived from the same room key.
//
// The call service relays the public keys, so a dishonest one could hand out keys of its own and sit
// in the middle. Everyone therefore sees a security code computed from the whole room — every
// participant's id, name and public key — and the codes only match on everyone's screen if nobody
// substituted anything. The code is long (12 digits) and slow to compute (PBKDF2, 100,000 rounds) so
// that a service in the middle cannot search for fake keys that happen to give matching codes while a
// call is being set up.
//
// Runs in browsers (window.PigeonpostCallsE2EE) and in Node for the tests.
(function (root) {
  "use strict";

  const subtle = (root.crypto || globalThis.crypto).subtle;
  const randomBytes = (n) => (root.crypto || globalThis.crypto).getRandomValues(new Uint8Array(n));
  const te = new TextEncoder(), td = new TextDecoder();
  const ECDH = { name: "ECDH", namedCurve: "P-256" };
  const SAS_ROUNDS = 100_000;
  const DATA_VERSION = 1;
  // LiveKit keeps 16 keys at once; frames name the one they were sealed with by its slot.
  const KEY_SLOTS = 16;

  function b64u(bytes) {
    let s = "";
    for (const b of bytes) s += String.fromCharCode(b);
    return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  }
  function unb64u(text) {
    if (typeof text !== "string" || !/^[A-Za-z0-9_-]*$/.test(text)) throw new Error("bad base64url");
    const s = atob(text.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((text.length + 3) % 4));
    const out = new Uint8Array(s.length);
    for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
    return out;
  }
  const canon = (parts) => te.encode(JSON.stringify(parts));

  // An uncompressed P-256 point: 65 bytes starting with 4.
  function isPublicKey(text) {
    return typeof text === "string" && /^B[A-Za-z0-9_-]{86}$/.test(text);
  }

  async function generateIdentity() {
    const pair = await subtle.generateKey(ECDH, false, ["deriveBits"]);
    const raw = new Uint8Array(await subtle.exportKey("raw", pair.publicKey));
    return { privateKey: pair.privateKey, publicKey: b64u(raw) };
  }

  const randomRoomKey = () => randomBytes(32);
  const slotFor = (epoch) => epoch % KEY_SLOTS;

  // The key that wraps a room key from one participant to another. The same pair derives the same
  // key from either end; everything it is bound to goes into the HKDF info.
  async function wrapKeyFor(privateKey, theirKey, roomId, from, to) {
    if (!isPublicKey(from.key) || !isPublicKey(to.key)) throw new Error("bad public key");
    const peer = await subtle.importKey("raw", unb64u(theirKey), ECDH, false, []);
    const shared = await subtle.deriveBits({ name: "ECDH", public: peer }, privateKey, 256);
    const base = await subtle.importKey("raw", shared, "HKDF", false, ["deriveKey"]);
    return subtle.deriveKey(
      { name: "HKDF", hash: "SHA-256", salt: canon(["pigeonpost-call-wrap-salt-v1", roomId]), info: canon(["pigeonpost-call-wrap-v1", roomId, from.id, from.key, to.id, to.key]) },
      base, { name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]);
  }

  // `from` is this browser ({id, key} with our own public key); `to` the addressee.
  async function wrapRoomKey({ privateKey, roomId, epoch, from, to, roomKey }) {
    if (!Number.isInteger(epoch) || epoch < 1) throw new Error("bad epoch");
    if (!(roomKey instanceof Uint8Array) || roomKey.length !== 32) throw new Error("bad room key");
    const k = await wrapKeyFor(privateKey, to.key, roomId, from, to);
    const iv = randomBytes(12);
    const sealed = new Uint8Array(await subtle.encrypt({ name: "AES-GCM", iv, additionalData: canon(["pigeonpost-call-room-key-v1", roomId, epoch]) }, k, roomKey));
    return { epoch, iv: b64u(iv), key: b64u(sealed) };
  }

  // `from` is the sender as this browser knows them; `to` is this browser. Throws unless the key was
  // wrapped by the holder of `from.key`'s private half, to `to.key`, for this room and epoch.
  async function unwrapRoomKey({ privateKey, roomId, from, to, wrapped }) {
    if (!wrapped || !Number.isInteger(wrapped.epoch) || wrapped.epoch < 1) throw new Error("bad epoch");
    const k = await wrapKeyFor(privateKey, from.key, roomId, from, to);
    const iv = unb64u(wrapped.iv);
    if (iv.length !== 12) throw new Error("bad iv");
    const out = new Uint8Array(await subtle.decrypt({ name: "AES-GCM", iv, additionalData: canon(["pigeonpost-call-room-key-v1", roomId, wrapped.epoch]) }, k, unb64u(wrapped.key)));
    if (out.length !== 32) throw new Error("bad room key");
    return out;
  }

  // Who someone is, as the code covers it: a verified mailbox, or the name a guest typed.
  function memberLabel(p) {
    if (p && p.mailbox && p.mailbox.address) return "mailbox:" + String(p.mailbox.address).toLowerCase();
    return "guest:" + String((p && p.name) || "");
  }

  // The room's security code: twelve digits over everyone's id, name and public key, in a fixed
  // order. Members without a key yet are left out (they cannot receive the room key either).
  async function securityCode(roomId, members) {
    const list = members.filter((m) => isPublicKey(m.key)).map((m) => [String(m.id), memberLabel(m), m.key]);
    list.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
    const base = await subtle.importKey("raw", canon(["pigeonpost-call-security-code-v1", roomId, list]), "PBKDF2", false, ["deriveBits"]);
    const bits = new Uint8Array(await subtle.deriveBits({ name: "PBKDF2", hash: "SHA-256", salt: te.encode("pigeonpost-call-security-code-v1"), iterations: SAS_ROUNDS }, base, 64));
    let n = 0n;
    for (const b of bits) n = (n << 8n) | BigInt(b);
    const digits = String(n % 1_000_000_000_000n).padStart(12, "0");
    return digits.slice(0, 4) + " " + digits.slice(4, 8) + " " + digits.slice(8);
  }

  // Chat and status messages: AES-GCM under a key derived from the room key of one epoch.
  async function dataKey(roomKey) {
    const base = await subtle.importKey("raw", roomKey, "HKDF", false, ["deriveKey"]);
    return subtle.deriveKey({ name: "HKDF", hash: "SHA-256", salt: new Uint8Array(32), info: te.encode("pigeonpost-call-data-v1") }, base, { name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]);
  }

  async function sealData(key, { epoch, roomId, senderId }, value) {
    const iv = randomBytes(12);
    const ct = new Uint8Array(await subtle.encrypt({ name: "AES-GCM", iv, additionalData: canon(["pigeonpost-call-data-v1", roomId, senderId, epoch]) }, key, te.encode(JSON.stringify(value))));
    const out = new Uint8Array(5 + 12 + ct.length);
    out[0] = DATA_VERSION;
    new DataView(out.buffer).setUint32(1, epoch);
    out.set(iv, 5);
    out.set(ct, 17);
    return out;
  }

  // `keyFor(epoch)` returns the data key of that epoch, or null. Throws on anything that does not open.
  async function openData(keyFor, { roomId, senderId }, bytes) {
    if (!(bytes instanceof Uint8Array) || bytes.length < 5 + 12 + 16 || bytes.length > 16_384 || bytes[0] !== DATA_VERSION) throw new Error("bad packet");
    const epoch = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength).getUint32(1);
    const key = await keyFor(epoch);
    if (!key) throw new Error("no key");
    const plain = await subtle.decrypt({ name: "AES-GCM", iv: bytes.slice(5, 17), additionalData: canon(["pigeonpost-call-data-v1", roomId, senderId, epoch]) }, key, bytes.slice(17));
    return { epoch, value: JSON.parse(td.decode(plain)) };
  }

  // Can this browser encrypt its own media frames? Chrome, Edge and their web views have encoded
  // insertable streams; Safari, WebKit web views and Firefox have RTCRtpScriptTransform.
  function frameEncryptionSupported(w) {
    const g = w || root;
    const scriptTransform = typeof g.RTCRtpScriptTransform !== "undefined";
    const insertable = typeof g.RTCRtpSender !== "undefined" && typeof g.RTCRtpSender.prototype.createEncodedStreams !== "undefined";
    return Boolean((scriptTransform || insertable) && g.crypto && g.crypto.subtle && typeof g.Worker !== "undefined");
  }

  const api = { generateIdentity, isPublicKey, randomRoomKey, slotFor, KEY_SLOTS, wrapRoomKey, unwrapRoomKey, memberLabel, securityCode, dataKey, sealData, openData, frameEncryptionSupported, b64u, unb64u };
  if (typeof module === "object" && module.exports) module.exports = api;
  else root.PigeonpostCallsE2EE = api;
})(typeof window !== "undefined" ? window : globalThis);
