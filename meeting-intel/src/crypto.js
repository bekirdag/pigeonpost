// Cryptography shared with the browsers (site-inbox/calls-summary.js, site-inbox/meeting-notes.js).
// Everything here is what WebCrypto offers on every supported engine: ECDH and ECDSA on P-256,
// HKDF-SHA-256 and AES-256-GCM (ciphertext || 16-byte tag). Binary values travel as standard base64.

import crypto from "node:crypto";

export const AUDIO_INFO = "pigeonpost meeting audio v1|";
export const NOTES_INFO = "pigeonpost meeting notes v1|";
export const SIGN_CONTEXT = "pigeonpost-meeting-intel-v1";

export const b64 = (buf) => Buffer.from(buf).toString("base64");
export function unb64(text, expectLen) {
  if (typeof text !== "string" || !/^[A-Za-z0-9+/]*={0,2}$/.test(text)) throw new Error("bad base64");
  const buf = Buffer.from(text, "base64");
  if (expectLen !== undefined && buf.length !== expectLen) throw new Error("bad length");
  return buf;
}

// A per-meeting ECDH key. The private half never leaves this process and is dropped with the session.
export function newSessionKey() {
  const ecdh = crypto.createECDH("prime256v1");
  ecdh.generateKeys();
  return { ecdh, publicRaw: ecdh.getPublicKey(null, "uncompressed") };
}

export function hkdf(secret, salt, info) {
  return Buffer.from(crypto.hkdfSync("sha256", secret, Buffer.from(salt, "utf8"), Buffer.from(info, "utf8"), 32));
}

export function gcmEncrypt(key, plaintext, aad, iv = crypto.randomBytes(12)) {
  const c = crypto.createCipheriv("aes-256-gcm", key, iv);
  c.setAAD(Buffer.from(aad, "utf8"));
  const ct = Buffer.concat([c.update(plaintext), c.final(), c.getAuthTag()]);
  return { iv, ct };
}

export function gcmDecrypt(key, iv, data, aad) {
  if (data.length < 17) throw new Error("short ciphertext");
  const d = crypto.createDecipheriv("aes-256-gcm", key, iv);
  d.setAAD(Buffer.from(aad, "utf8"));
  d.setAuthTag(data.subarray(data.length - 16));
  return Buffer.concat([d.update(data.subarray(0, data.length - 16)), d.final()]);
}

// The additional data that binds one audio chunk to its meeting session, speaker and place.
export function audioAad(session, participant, e) {
  return `${session}|${participant}|${e.seq}|${e.codec}|${e.t}|${e.dur}`;
}

// Decrypt one uploaded chunk with the session's private key.
export function openChunk(sessionKey, session, participant, seq, e) {
  const epk = unb64(e.epk, 65);
  const shared = sessionKey.ecdh.computeSecret(epk);
  const key = hkdf(shared, session, AUDIO_INFO + participant);
  shared.fill(0);
  try {
    return gcmDecrypt(key, unb64(e.iv, 12), unb64(e.ct), audioAad(session, participant, { ...e, seq }));
  } finally {
    key.fill(0);
  }
}

// What a browser does, for tests and the synthetic harness.
export function sealChunk(servicePublicRaw, session, participant, seq, audio, meta) {
  const eph = crypto.createECDH("prime256v1");
  eph.generateKeys();
  const shared = eph.computeSecret(servicePublicRaw);
  const key = hkdf(shared, session, AUDIO_INFO + participant);
  const e = { v: 1, epk: b64(eph.getPublicKey(null, "uncompressed")), seq, codec: meta.codec, t: meta.t, dur: meta.dur };
  const { iv, ct } = gcmEncrypt(key, audio, audioAad(session, participant, e));
  return { v: 1, epk: e.epk, iv: b64(iv), ct: b64(ct), codec: e.codec, t: e.t, dur: e.dur };
}

// The brief: one content key, wrapped for each recipient's notes key.
export function sealBrief(meetingId, plaintext, recipients) {
  const cek = crypto.randomBytes(32);
  const { iv, ct } = gcmEncrypt(cek, plaintext, meetingId);
  const wraps = [];
  for (const r of recipients) {
    for (const k of r.keys) {
      let pub;
      try { pub = unb64(k.public_key, 65); } catch (_) { continue; }
      const eph = crypto.createECDH("prime256v1");
      eph.generateKeys();
      let shared;
      try { shared = eph.computeSecret(pub); } catch (_) { continue; }
      const kek = hkdf(shared, meetingId, NOTES_INFO + k.key_id);
      const w = gcmEncrypt(kek, cek, `${meetingId}|${k.key_id}`);
      wraps.push({ account_id: r.account_id, key_id: k.key_id, epk: b64(eph.getPublicKey(null, "uncompressed")), iv: b64(w.iv), wrapped: b64(w.ct) });
      shared.fill(0);
      kek.fill(0);
    }
  }
  cek.fill(0);
  return { alg: "A256GCM", iv: b64(iv), ciphertext: b64(ct), wraps };
}

// What a browser does to open a brief (tests).
export function openBrief(meetingId, brief, wrap, privateEcdh) {
  const shared = privateEcdh.computeSecret(unb64(wrap.epk, 65));
  const kek = hkdf(shared, meetingId, NOTES_INFO + wrap.key_id);
  const cek = gcmDecrypt(kek, unb64(wrap.iv, 12), unb64(wrap.wrapped), `${meetingId}|${wrap.key_id}`);
  return gcmDecrypt(cek, unb64(brief.iv, 12), unb64(brief.ciphertext), meetingId);
}

// ---- the long-term signing key -------------------------------------------------------------------

export function loadSigningKey(pem) {
  const key = crypto.createPrivateKey(pem);
  if (key.asymmetricKeyType !== "ec" || key.asymmetricKeyDetails?.namedCurve !== "prime256v1") {
    throw new Error("signing key must be an EC P-256 private key");
  }
  const pub = crypto.createPublicKey(key);
  const jwk = pub.export({ format: "jwk" });
  const raw = Buffer.concat([Buffer.from([4]), Buffer.from(jwk.x, "base64url"), Buffer.from(jwk.y, "base64url")]);
  return { key, publicRaw: raw, keyId: crypto.createHash("sha256").update(raw).digest("base64url").slice(0, 16) };
}

export function generateSigningKeyPem() {
  const { privateKey } = crypto.generateKeyPairSync("ec", { namedCurve: "prime256v1" });
  return privateKey.export({ format: "pem", type: "pkcs8" });
}

export function signedMessage(session, meeting, keyB64, expiresAt) {
  return `${SIGN_CONTEXT}\n${session}\n${meeting}\n${keyB64}\n${expiresAt}`;
}

export function signSessionKey(signing, session, meeting, keyB64, expiresAt) {
  return b64(crypto.sign("sha256", Buffer.from(signedMessage(session, meeting, keyB64, expiresAt)), { key: signing.key, dsaEncoding: "ieee-p1363" }));
}

export function verifySessionKey(publicRaw, session, meeting, keyB64, expiresAt, signature) {
  const pub = crypto.createPublicKey({
    key: { kty: "EC", crv: "P-256", x: publicRaw.subarray(1, 33).toString("base64url"), y: publicRaw.subarray(33).toString("base64url") },
    format: "jwk",
  });
  return crypto.verify("sha256", Buffer.from(signedMessage(session, meeting, keyB64, expiresAt)), { key: pub, dsaEncoding: "ieee-p1363" }, unb64(signature));
}
