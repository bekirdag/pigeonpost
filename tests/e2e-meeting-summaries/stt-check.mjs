// Qualification of the meeting-notes pipeline on its real host: real ffmpeg, whisper.cpp and the local
// model, fed the synthetic three-speaker fixture (make-fixture.sh: macOS text-to-speech voices, nobody's
// real voice). Each speaker's track is cut into ~10 s Opus chunks and encrypted exactly as a browser
// does; the postbox is a stand-in that returns one notes key per speaker and keeps the encrypted brief.
//
//   node tests/e2e-meeting-summaries/stt-check.mjs <fixture-dir>
//     env: WHISPER_URL (default http://127.0.0.1:8798/inference), LLM_URL, LLM_MODEL, FFMPEG, STT_NAME
//
// Exits non-zero unless: every speaker's key phrases are transcribed (English and Turkish), the brief
// decrypts for each recipient key, owners are only people who took tasks, and all audio was wiped.
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import crypto from "node:crypto";
import { b64, generateSigningKeyPem, loadSigningKey, openBrief, sealChunk, unb64 } from "../../meeting-intel/src/crypto.js";
import { createIntel } from "../../meeting-intel/src/service.js";
import { createDecoder, createWhisper, peak } from "../../meeting-intel/src/audio.js";
import { createSummarizer } from "../../meeting-intel/src/summary.js";

const dir = process.argv[2] || "fixture";
const ffmpeg = process.env.FFMPEG || "ffmpeg";
const env = {
  whisper: process.env.WHISPER_URL || "http://127.0.0.1:8798/inference",
  llm: process.env.LLM_URL || "http://127.0.0.1:11452/v1/chat/completions",
  model: process.env.LLM_MODEL || "qwen-latest-flash-next",
  stt: process.env.STT_NAME || "whisper.cpp small",
};
const MEETING = "mtg_" + crypto.randomBytes(13).toString("hex");
const speakers = [
  { pid: "pid_ada_000000000", label: "/ada/main", file: "ada.wav" },
  { pid: "pid_ben_000000000", label: "/ben/main", file: "ben.wav" },
  { pid: "pid_deniz_0000000", label: "Guest Deniz", file: "deniz.wav" },
];
const recipients = speakers.slice(0, 2).map((s) => {
  const e = crypto.createECDH("prime256v1");
  e.generateKeys();
  return { ecdh: e, account_id: "acct_" + s.pid, address: s.label, keys: [{ key_id: "nk" + s.pid.slice(4, 20).padEnd(16, "0"), public_key: b64(e.getPublicKey(null, "uncompressed")) }] };
});

function opusChunk(file, start, seconds) {
  const r = spawnSync(ffmpeg, ["-hide_banner", "-loglevel", "error", "-ss", String(start), "-t", String(seconds), "-i", file, "-c:a", "libopus", "-b:a", "24k", "-f", "webm", "pipe:1"], { maxBuffer: 4 << 20 });
  if (r.status !== 0) throw new Error("ffmpeg failed");
  return r.stdout;
}

const decoded = [];
const realDecode = createDecoder({ ffmpeg });
let stored = null;
const intel = createIntel({ sttName: env.stt, llmName: env.model }, {
  signing: loadSigningKey(generateSigningKeyPem()),
  decode: async (b) => { const p = await realDecode(b); decoded.push(p); return p; },
  transcribe: createWhisper({ url: env.whisper }),
  summarizer: createSummarizer({ url: env.llm, model: env.model }),
  postbox: async (method, path, body) => {
    if (path.endsWith("/recipients")) return { status: 200, body: { recipients: recipients.map(({ ecdh, ...r }) => r) } };
    stored = body;
    return { status: 200, body: { stored: true } };
  },
  log: (l) => console.log("  intel: " + l),
});

const opened = intel.open({ meeting_id: MEETING });
const session = intel.sessionFor(opened.session);
const servicePub = unb64(opened.key, 65);
const t0 = Date.now();
const duration = Number(spawnSync(ffmpeg.replace(/ffmpeg$/, "ffprobe"), ["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", `${dir}/ada.wav`]).stdout.toString()) || 49;
let seqs = new Map();
let audioSeconds = 0;
const wall = Date.now();
for (let start = 0; start < duration; start += 10) {
  for (const s of speakers) {
    const chunk = opusChunk(`${dir}/${s.file}`, start, Math.min(10, duration - start));
    const seq = seqs.get(s.pid) || 0;
    seqs.set(s.pid, seq + 1);
    const envelope = sealChunk(servicePub, opened.session, s.pid, seq, chunk, { codec: "audio/webm;codecs=opus", t: t0 + start * 1000, dur: Math.round(Math.min(10, duration - start) * 1000) });
    await intel.chunk(session, { participant: s.pid, speaker: s.label, seq, envelope });
    audioSeconds += Math.min(10, duration - start);
  }
}
const sttStarted = Date.now();
await intel.finish(session, "host_stopped");
const finishedAt = Date.now();
const health = intel.health();

const fail = [];
if (!stored || stored.status !== "ready") fail.push("no brief stored: " + JSON.stringify(stored));
let brief = null;
for (const r of recipients) {
  const wrap = stored && stored.wraps && stored.wraps.find((w) => w.key_id === r.keys[0].key_id);
  if (!wrap) { fail.push("no wrap for " + r.address); continue; }
  brief = JSON.parse(openBrief(MEETING, stored, wrap, r.ecdh).toString());
}
const text = brief ? brief.transcript.map((x) => `${x.speaker}: ${x.text}`).join("\n") : "";
const expect = [["/ada/main", /lighthouse/i], ["/ada/main", /billing/i], ["/ben/main", /status page/i], ["/ben/main", /november/i], ["Guest Deniz", /sürüm notlar/i], ["Guest Deniz", /çeviri/i]];
for (const [who, re] of expect) if (!brief || !brief.transcript.some((x) => x.speaker === who && re.test(x.text))) fail.push(`missing ${re} from ${who}`);
if (brief && !brief.transcript.some((x) => x.lang === "tr")) fail.push("no Turkish segment detected");
if (brief && !brief.draft) {
  const owners = brief.action_items.map((a) => a.owner).filter(Boolean);
  for (const o of owners) if (!["/ben/main", "Guest Deniz"].includes(o)) fail.push("owner who took no task: " + o);
  if (!owners.includes("Guest Deniz")) fail.push("Deniz's task has no owner");
  if (!brief.decisions.some((d) => /november|12/i.test(d))) fail.push("launch decision missing");
  if (!brief.open_questions.some((q) => /billing/i.test(q))) fail.push("open question missing");
}
for (const p of decoded) if (peak(p) !== 0) fail.push("decoded audio left in memory");
if (intel._sessions.size !== 0) fail.push("session not dropped");

console.log(JSON.stringify({
  audio_seconds: Math.round(audioSeconds), stt_audio_seconds: Math.round(health.audio_seconds), stt_seconds: Number(health.stt_seconds.toFixed(1)),
  rtf: health.rtf, windows: health.windows, silent_windows: health.silent_windows,
  ingest_s: Number(((sttStarted - wall) / 1000).toFixed(1)), finish_s: Number(((finishedAt - sttStarted) / 1000).toFixed(1)),
  model: env.stt, llm: brief && !brief.draft ? env.model : "draft (no model output)",
}, null, 1));
if (brief) {
  console.log("--- brief ---");
  console.log(JSON.stringify({ title: brief.title, summary: brief.summary, decisions: brief.decisions, action_items: brief.action_items, open_questions: brief.open_questions }, null, 1));
  console.log("--- transcript ---\n" + text);
}
if (fail.length) { console.error("FAIL\n- " + fail.join("\n- ")); process.exit(1); }
console.log("OK");
