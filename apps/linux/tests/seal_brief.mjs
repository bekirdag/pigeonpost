// Seal a meeting brief exactly as meeting-intel does (its own crypto.js), for the Linux tests.
//   node seal_brief.mjs < {meeting_id, brief, recipients: [{account_id, keys: [{key_id, public_key}]}]}
//   node seal_brief.mjs --vector   (a fixed-shape test vector with a fresh recipient key)
// Synthetic data only.
import crypto from "node:crypto";
import { sealBrief } from "../../../meeting-intel/src/crypto.js";

if (process.argv[2] === "--vector") {
  const { privateKey, publicKey } = crypto.generateKeyPairSync("ec", { namedCurve: "prime256v1" });
  const jwk = publicKey.export({ format: "jwk" });
  const raw = Buffer.concat([Buffer.from([4]), Buffer.from(jwk.x, "base64url"), Buffer.from(jwk.y, "base64url")]);
  const meetingId = "mtg_" + "a1b2c3d4e5f6g7h8i9j0k1l2m3";
  const brief = {
    title: "Release review", summary: "The team agreed to ship the Linux build on Thursday.",
    decisions: ["Ship 1.0.11 on Thursday"], action_items: [{ owner: "Alex", task: "Write the release notes", due: "Wednesday" }],
    open_questions: ["Do we need a Flathub listing?"], participants: ["Alex", "Sam"], started_at: "2026-10-27T10:00:00Z", duration_s: 1260,
    transcript: [{ at: "00:00:04", speaker: "Alex", text: "Let's ship on Thursday." }, { at: "00:00:09", speaker: "Sam", text: "<b>Agreed</b>" }],
    generated_by: { stt: "whisper.cpp ggml-small", llm: "qwen" }, draft: false,
  };
  const sealed = sealBrief(meetingId, Buffer.from(JSON.stringify(brief)), [
    { account_id: "acct_vector", keys: [{ key_id: "nkVectorKey01", public_key: raw.toString("base64") }, { key_id: "nkOtherDevice1", public_key: raw.toString("base64") }] },
  ]);
  const meeting = { id: meetingId, state: "ready", alg: sealed.alg, iv: sealed.iv, ciphertext: sealed.ciphertext,
    keys: sealed.wraps.map(({ key_id, epk, iv, wrapped }) => ({ key_id, epk, iv, wrapped })) };
  console.log(JSON.stringify({ key: { key_id: "nkVectorKey01", private_pkcs8: privateKey.export({ format: "der", type: "pkcs8" }).toString("base64"), public_key: raw.toString("base64") }, meeting, brief }, null, 1));
} else {
  let input = "";
  for await (const chunk of process.stdin) input += chunk;
  const { meeting_id: meetingId, brief, recipients } = JSON.parse(input);
  console.log(JSON.stringify(sealBrief(meetingId, Buffer.from(JSON.stringify(brief)), recipients)));
}
