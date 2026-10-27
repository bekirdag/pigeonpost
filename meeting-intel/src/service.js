// meeting-intel: turns consented, encrypted microphone chunks into an encrypted meeting brief.
//
// Per meeting session (one per "notes on" in a call):
//   chunk (ciphertext) -> decrypt in memory -> ffmpeg (pipes) -> PCM appended to that speaker's
//   window -> window of ~30 s queued -> whisper.cpp -> transcript segments; the window's PCM is
//   zeroed and dropped as soon as it is transcribed (or discarded, or the session ends).
// finish -> remaining windows transcribed -> local LLM brief -> encrypted to each consenting
//   attendee's notes keys -> stored by the postbox -> the session (keys, transcript) is dropped.
//
// Nothing is written to disk. Bounded: sessions, queued audio seconds (global and per session),
// chunk size, session length. Sessions are isolated: each has its own key, windows and transcript,
// and the transcription queue serves sessions round-robin. Logs carry ids, counts and durations.

import crypto from "node:crypto";
import { b64, newSessionKey, openChunk, sealBrief, signSessionKey } from "./crypto.js";
import { BYTES_PER_SECOND, peak } from "./audio.js";
import { majorityLanguage, mmss, transcriptLines } from "./summary.js";

export const LIMITS = {
  sessions: 6,
  chunkBytes: 192 * 1024,
  chunkSeconds: 30,
  windowSeconds: 28, // whisper encodes 30 s at a time; a window just under that costs one pass
  gapMs: 2_500, // a longer gap between a speaker's chunks starts a new window
  queueSeconds: 1_200, // all sessions: audio waiting for transcription
  sessionQueueSeconds: 600,
  sessionMs: 4 * 3600_000,
  idleMs: 30 * 60_000,
  keyTtlMs: 4 * 3600_000 + 15 * 60_000,
  silencePeak: 0.006, // below this a window is treated as silence and never sent to whisper
};

const WHISPER_LANGS = { english: "en", turkish: "tr", german: "de", french: "fr", spanish: "es", italian: "it", dutch: "nl", portuguese: "pt", russian: "ru", arabic: "ar", japanese: "ja", chinese: "zh", korean: "ko", polish: "pl", ukrainian: "uk", swedish: "sv", greek: "el" };
const langCode = (v) => (v ? WHISPER_LANGS[String(v).toLowerCase()] || (String(v).length === 2 ? String(v).toLowerCase() : null) : null);

export class IntelError extends Error {
  constructor(status, code) { super(code); this.status = status; this.code = code; }
}

export function createIntel(cfg, deps) {
  const { decode, transcribe, summarizer, postbox, signing } = deps;
  const now = deps.now || (() => Date.now());
  const log = deps.log || ((line) => console.log("intel: " + line));
  const sessions = new Map();
  const queue = []; // windows waiting: { session, window }
  const stats = { sessions_opened: 0, chunks: 0, chunks_refused: 0, windows: 0, silent_windows: 0, audio_seconds: 0, stt_seconds: 0, briefs: 0, failures: 0, discarded_windows: 0 };
  let working = false;
  let decoding = 0;

  const queuedSeconds = (s) => queue.reduce((n, q) => n + (!s || q.session === s ? q.window.bytes / BYTES_PER_SECOND : 0), 0)
    + [...sessions.values()].reduce((n, x) => n + (!s || x === s ? [...x.open.values()].reduce((m, w) => m + w.bytes / BYTES_PER_SECOND, 0) : 0), 0);

  function wipeWindow(w) {
    for (const p of w.parts) p.fill(0);
    w.parts.length = 0;
    w.bytes = 0;
  }

  function dropSession(s) {
    for (const w of s.open.values()) wipeWindow(w);
    s.open.clear();
    for (let i = queue.length - 1; i >= 0; i--) if (queue[i].session === s) { wipeWindow(queue[i].window); queue.splice(i, 1); }
    s.key = null;
    s.segments.length = 0;
    clearTimeout(s.idleTimer);
    clearTimeout(s.maxTimer);
    sessions.delete(s.id);
  }

  // ---- sessions -----------------------------------------------------------------------------------

  function open(body) {
    const meeting = String(body.meeting_id || "");
    if (!/^mtg_[a-z0-9]{26}$/.test(meeting)) throw new IntelError(400, "bad_request");
    if (sessions.size >= LIMITS.sessions) throw new IntelError(503, "busy");
    for (const s of sessions.values()) if (s.meeting === meeting) throw new IntelError(409, "exists");
    const id = "ses_" + crypto.randomBytes(12).toString("hex");
    const key = newSessionKey();
    const expiresAt = Math.floor((now() + LIMITS.keyTtlMs) / 1000);
    const keyB64 = b64(key.publicRaw);
    const s = {
      id, meeting, key, createdAt: now(), lastChunkAt: now(), finishing: false, finished: null,
      open: new Map(), segments: [], speakers: new Map(), gen: new Map(), seqs: new Map(),
      pending: 0, idleTimer: null, maxTimer: null,
    };
    s.maxTimer = setTimeout(() => finish(s, "max_duration"), LIMITS.sessionMs);
    s.maxTimer.unref?.();
    armIdle(s);
    sessions.set(id, s);
    stats.sessions_opened++;
    log(`session ${id.slice(0, 10)} opened`);
    return {
      session: id, key: keyB64, key_id: signing.keyId, expires_at: expiresAt,
      signature: signSessionKey(signing, id, meeting, keyB64, expiresAt),
    };
  }

  function armIdle(s) {
    clearTimeout(s.idleTimer);
    s.idleTimer = setTimeout(() => finish(s, "idle"), LIMITS.idleMs);
    s.idleTimer.unref?.();
  }

  function sessionFor(id) {
    const s = sessions.get(String(id || ""));
    if (!s) throw new IntelError(404, "no_session");
    return s;
  }

  // ---- chunks -------------------------------------------------------------------------------------

  async function chunk(s, body) {
    if (s.finishing) throw new IntelError(409, "finished");
    const participant = String(body.participant || "");
    const seq = Number(body.seq);
    const e = body.envelope;
    if (!/^[A-Za-z0-9_-]{8,64}$/.test(participant) || !Number.isInteger(seq) || seq < 0 || !e || typeof e !== "object") {
      throw new IntelError(400, "bad_request");
    }
    if (typeof e.ct !== "string" || e.ct.length > LIMITS.chunkBytes * 1.4) throw new IntelError(400, "bad_request");
    if (!(Number(e.dur) > 0 && Number(e.dur) <= LIMITS.chunkSeconds * 1000)) throw new IntelError(400, "bad_request");
    if ((s.seqs.get(participant) ?? -1) >= seq) return { accepted: true, duplicate: true };
    if (queuedSeconds() + e.dur / 1000 > LIMITS.queueSeconds || queuedSeconds(s) + e.dur / 1000 > LIMITS.sessionQueueSeconds) {
      stats.chunks_refused++;
      throw new IntelError(503, "busy");
    }
    if (decoding >= 4) { stats.chunks_refused++; throw new IntelError(503, "busy"); }
    let plain;
    try {
      plain = openChunk(s.key, s.id, participant, seq, e);
    } catch (_) {
      stats.chunks_refused++;
      throw new IntelError(400, "undecryptable");
    }
    decoding++;
    let pcm;
    try {
      pcm = await decode(plain);
    } catch (_) {
      stats.chunks_refused++;
      throw new IntelError(400, "undecodable");
    } finally {
      plain.fill(0);
      decoding--;
    }
    // The session may have ended or this speaker stopped while ffmpeg ran.
    if (!sessions.has(s.id) || s.finishing) { pcm.fill(0); throw new IntelError(409, "finished"); }
    s.seqs.set(participant, seq);
    s.lastChunkAt = now();
    armIdle(s);
    stats.chunks++;
    const speaker = String(body.speaker || "Speaker").replace(/[\u0000-\u001f]/g, "").slice(0, 80) || "Speaker";
    s.speakers.set(participant, speaker);
    const t = Number(e.t);
    let w = s.open.get(participant);
    if (w && (t - w.end > LIMITS.gapMs || t < w.end - 1000)) { flush(s, participant); w = null; }
    if (!w) {
      w = { participant, gen: s.gen.get(participant) || 0, t0: t, end: t, parts: [], bytes: 0 };
      s.open.set(participant, w);
    } else if (t > w.end) {
      // Short gap: keep the timeline by padding with silence.
      const pad = Buffer.alloc(Math.min(LIMITS.gapMs, t - w.end) * BYTES_PER_SECOND / 1000 & ~1);
      w.parts.push(pad);
      w.bytes += pad.length;
    }
    w.parts.push(pcm);
    w.bytes += pcm.length;
    w.end = t + (pcm.length / BYTES_PER_SECOND) * 1000;
    if (w.bytes >= LIMITS.windowSeconds * BYTES_PER_SECOND) flush(s, participant);
    return { accepted: true };
  }

  function flush(s, participant) {
    const w = s.open.get(participant);
    if (!w) return;
    s.open.delete(participant);
    if (!w.bytes) return;
    queue.push({ session: s, window: w });
    s.pending++;
    pump();
  }

  // A participant stopped notes for themselves: everything of theirs not yet transcribed goes.
  function discard(s, body) {
    const participant = String(body.participant || "");
    // Results of a window already inside whisper are dropped too: its generation is now stale.
    s.gen.set(participant, (s.gen.get(participant) || 0) + 1);
    let dropped = 0;
    const w = s.open.get(participant);
    if (w) { wipeWindow(w); s.open.delete(participant); dropped++; }
    for (let i = queue.length - 1; i >= 0; i--) {
      if (queue[i].session === s && queue[i].window.participant === participant) {
        wipeWindow(queue[i].window);
        queue.splice(i, 1);
        s.pending--;
        dropped++;
      }
    }
    stats.discarded_windows += dropped;
    settle(s);
    return { discarded: dropped };
  }

  // Round-robin across sessions: take the oldest window of the session served longest ago.
  function next() {
    if (!queue.length) return null;
    let pick = 0;
    let best = Infinity;
    for (let i = 0; i < queue.length; i++) {
      const served = queue[i].session.lastServed || 0;
      if (served < best) { best = served; pick = i; }
    }
    return queue.splice(pick, 1)[0];
  }

  async function pump() {
    if (working) return;
    working = true;
    try {
      for (let item = next(); item; item = next()) {
        const { session: s, window: w } = item;
        s.lastServed = now();
        const seconds = w.bytes / BYTES_PER_SECOND;
        try {
          const pcm = Buffer.concat(w.parts);
          wipeWindow(w);
          if (peak(pcm) < LIMITS.silencePeak) {
            stats.silent_windows++;
          } else {
            const started = now();
            const r = await transcribe(pcm);
            stats.stt_seconds += (now() - started) / 1000;
            stats.audio_seconds += seconds;
            stats.windows++;
            const lang = langCode(r.language);
            if (sessions.has(s.id) && w.gen === (s.gen.get(w.participant) || 0)) {
              for (const seg of r.segments) {
                s.segments.push({ t: w.t0 + seg.start * 1000, participant: w.participant, speaker: s.speakers.get(w.participant) || "Speaker", text: seg.text.slice(0, 2000), lang });
              }
            }
          }
          pcm.fill(0);
        } catch (err) {
          stats.failures++;
          s.sttErrors = (s.sttErrors || 0) + 1;
          log(`session ${s.id.slice(0, 10)} window failed: ${err && err.message ? err.message.slice(0, 40) : "error"}`);
        } finally {
          s.pending--;
          settle(s);
        }
      }
    } finally {
      working = false;
    }
  }

  // ---- finishing ----------------------------------------------------------------------------------

  function finish(s, reason) {
    if (s.finishing) return s.finished;
    s.finishing = true;
    s.reason = reason;
    clearTimeout(s.idleTimer);
    // The promise exists before any window is flushed: a silent window settles synchronously.
    s.finished = new Promise((resolve) => { s.done = resolve; });
    for (const participant of [...s.open.keys()]) flush(s, participant);
    settle(s);
    return s.finished;
  }

  function settle(s) {
    if (s.finishing && s.pending <= 0 && !s.summarizing && sessions.has(s.id)) {
      s.summarizing = true;
      deliver(s).then(s.done, s.done);
    }
  }

  async function deliver(s) {
    const tag = s.id.slice(0, 10);
    try {
      const segments = s.segments.slice().sort((a, b) => a.t - b.t);
      if (!segments.length) return await failed(s, s.sttErrors ? "stt_unavailable" : "no_speech");
      const t0 = segments[0].t;
      const lines = transcriptLines(segments, t0);
      const speakers = new Set(segments.map((x) => x.speaker));
      const language = majorityLanguage(segments);
      const started = now();
      const result = await summarizer.summarize(lines, speakers, language);
      const recipients = await postbox("GET", `/internal/v1/meetings/${s.meeting}/recipients`);
      if (recipients.status !== 200 || !recipients.body) return await failed(s, "postbox_unavailable");
      const audience = (recipients.body.recipients || []).filter((r) => Array.isArray(r.keys) && r.keys.length);
      if (!audience.length) return await failed(s, "no_recipients");
      const brief = {
        v: 1, meeting_id: s.meeting,
        title: result.notes?.title || null,
        language, started_at: new Date(t0).toISOString(),
        duration_s: Math.round((segments[segments.length - 1].t - t0) / 1000),
        participants: [...speakers],
        summary: result.notes?.summary || null,
        decisions: result.notes?.decisions || [],
        action_items: result.notes?.action_items || [],
        open_questions: result.notes?.open_questions || [],
        draft: result.draft,
        transcript: segments.map((x) => ({ at: mmss(x.t - t0), speaker: x.speaker, text: x.text, lang: x.lang })),
        generated_by: { stt: cfg.sttName, llm: result.draft ? null : cfg.llmName },
      };
      const plaintext = Buffer.from(JSON.stringify(brief), "utf8");
      const sealed = sealBrief(s.meeting, plaintext, audience);
      plaintext.fill(0);
      const r = await postbox("POST", `/internal/v1/meetings/${s.meeting}/brief`, { status: "ready", ...sealed });
      if (r.status !== 200) {
        log(`session ${tag} brief refused status=${r.status}`);
        return await failed(s, "store_refused");
      }
      stats.briefs++;
      log(`session ${tag} brief stored segments=${segments.length} recipients=${audience.length} draft=${result.draft} llm_s=${Math.round((now() - started) / 1000)}`);
      return { status: "ready" };
    } catch (err) {
      return await failed(s, "error");
    } finally {
      dropSession(s);
    }
  }

  async function failed(s, reason) {
    stats.failures++;
    log(`session ${s.id.slice(0, 10)} failed reason=${reason}`);
    await postbox("POST", `/internal/v1/meetings/${s.meeting}/brief`, { status: "failed", reason }).catch(() => {});
    return { status: "failed", reason };
  }

  function abort(s) {
    log(`session ${s.id.slice(0, 10)} aborted`);
    postbox("POST", `/internal/v1/meetings/${s.meeting}/brief`, { status: "failed", reason: "aborted" }).catch(() => {});
    dropSession(s);
    return { aborted: true };
  }

  function health() {
    const queued = queuedSeconds();
    return {
      ok: true, sessions: sessions.size, queued_seconds: Math.round(queued), queue_windows: queue.length,
      rtf: stats.audio_seconds ? Number((stats.stt_seconds / stats.audio_seconds).toFixed(3)) : null,
      stt: cfg.sttName, llm: cfg.llmName, key_id: signing.keyId, ...stats,
    };
  }

  return { open, chunk, discard, finish, abort, sessionFor, health, _sessions: sessions, _queue: queue };
}
