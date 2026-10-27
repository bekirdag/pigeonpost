// Audio handling without touching disk: ffmpeg reads the decrypted chunk from a pipe and writes
// 16 kHz mono 16-bit PCM to a pipe; WAV files for whisper are built in memory.

import { spawn } from "node:child_process";

export const RATE = 16_000;
export const BYTES_PER_SECOND = RATE * 2;

export function createDecoder({ ffmpeg = "ffmpeg", timeoutMs = 20_000, maxSeconds = 40 } = {}) {
  const maxBytes = maxSeconds * BYTES_PER_SECOND;
  return function decode(input) {
    return new Promise((resolve, reject) => {
      const child = spawn(ffmpeg, ["-hide_banner", "-loglevel", "error", "-nostdin", "-i", "pipe:0", "-vn", "-ac", "1", "-ar", String(RATE), "-f", "s16le", "pipe:1"], {
        stdio: ["pipe", "pipe", "ignore"],
      });
      const out = [];
      let size = 0;
      let done = false;
      const finish = (err, value) => {
        if (done) return;
        done = true;
        clearTimeout(timer);
        if (err) { for (const b of out) b.fill(0); reject(err); } else resolve(value);
      };
      const timer = setTimeout(() => { child.kill("SIGKILL"); finish(new Error("decode_timeout")); }, timeoutMs);
      child.stdout.on("data", (d) => {
        size += d.length;
        if (size > maxBytes) { child.kill("SIGKILL"); finish(new Error("decode_too_long")); return; }
        out.push(d);
      });
      child.on("error", (e) => finish(e));
      child.on("close", (code) => {
        if (code !== 0) return finish(new Error("decode_failed"));
        const pcm = Buffer.concat(out);
        for (const b of out) b.fill(0);
        finish(null, pcm.subarray(0, pcm.length - (pcm.length % 2)));
      });
      child.stdin.on("error", () => {});
      child.stdin.end(input);
    });
  };
}

// Loudest sample, as a fraction of full scale.
export function peak(pcm) {
  let max = 0;
  for (let i = 0; i + 1 < pcm.length; i += 2) {
    const v = Math.abs(pcm.readInt16LE(i));
    if (v > max) max = v;
  }
  return max / 32768;
}

export function wav(pcm) {
  const header = Buffer.alloc(44);
  header.write("RIFF", 0);
  header.writeUInt32LE(36 + pcm.length, 4);
  header.write("WAVE", 8);
  header.write("fmt ", 12);
  header.writeUInt32LE(16, 16);
  header.writeUInt16LE(1, 20);
  header.writeUInt16LE(1, 22);
  header.writeUInt32LE(RATE, 24);
  header.writeUInt32LE(BYTES_PER_SECOND, 28);
  header.writeUInt16LE(2, 32);
  header.writeUInt16LE(16, 34);
  header.write("data", 36);
  header.writeUInt32LE(pcm.length, 40);
  return Buffer.concat([header, pcm]);
}

// whisper.cpp's server: one request at a time, verbose JSON with segment times relative to the file.
export function createWhisper({ url, fetch: fetchImpl = globalThis.fetch, timeoutMs = 300_000, language = "auto" }) {
  return async function transcribe(pcm) {
    const body = new FormData();
    const file = wav(pcm);
    body.append("file", new Blob([file], { type: "audio/wav" }), "chunk.wav");
    body.append("response_format", "verbose_json");
    body.append("language", language);
    body.append("temperature", "0.0");
    const ac = new AbortController();
    const timer = setTimeout(() => ac.abort(), timeoutMs);
    try {
      const res = await fetchImpl(url, { method: "POST", body, signal: ac.signal });
      // whisper-server answers 500 when voice-activity detection finds no speech at all.
      if (res.status === 500) return { language: null, segments: [] };
      if (!res.ok) throw new Error("stt_status_" + res.status);
      const v = await res.json();
      const segments = Array.isArray(v.segments) ? v.segments : [];
      return {
        language: typeof v.language === "string" ? v.language : null,
        segments: segments
          .filter((s) => typeof s.text === "string" && s.text.trim() && !(s.no_speech_prob > 0.6))
          .map((s) => ({ start: Number(s.start) || 0, end: Number(s.end) || 0, text: s.text.trim(), avg_logprob: s.avg_logprob })),
      };
    } finally {
      clearTimeout(timer);
      file.fill(0);
    }
  };
}
