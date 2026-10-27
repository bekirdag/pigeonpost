// Pigeonpost calls — live streams in groups (Phase 6, SDS §14A, docs/streaming.md).
//
// A stream is a room on the media server (LiveKit) that is separate from every call room: its
// LiveKit room name is `stream_<id>` and it never mixes with a call. The postbox decides who may
// take part and in which role (`POST /internal/v1/stream-admission`, this service's token); this
// service turns that into media-server tickets, keeps the media server within its measured budget,
// and relays the stream key that the streamer's browser wraps to each authorised viewer's key.
//
//   * host (the streamer) and speakers (invited and accepted): may publish, microphone, camera and
//     — for the host, if they chose it — screen;
//   * viewers (and invited people who have not accepted yet): subscribe only, hidden from the
//     other participants on the media server, never able to publish or send data.
//
// The media server enforces those grants: a viewer's ticket carries no publish right, and a stage
// change is applied to the participant on the media server (UpdateParticipant), not only in a UI.
//
// Every STREAM_TICK_MS this service reports the viewer count and every participant's mailbox to the
// postbox (`/internal/v1/stream-check`). Anyone whose role disappeared (removed from the group,
// banned) is removed from the media server at once and the streamer is told, so their browser
// rotates the stream key without them. If the postbox cannot be asked for STREAM_BLIND_MS, the stream
// stops: this service may not guess who is still allowed in.
//
// What this service sees: who watches (mailboxes), when, roughly how much media flows, wrapped keys
// it cannot open and sealed reactions it cannot read.

export const STREAM_ID = /^str_[a-z2-7]{26}$/;
const PUBLIC_KEY = /^B[A-Za-z0-9_-]{86}$/;
const B64U = (n) => new RegExp(`^[A-Za-z0-9_-]{${n}}$`);
const WRAP_IV = B64U(16);
const WRAP_KEY = B64U(64);
const SEALED = /^[A-Za-z0-9_-]{24,400}$/;

export const STREAM_LIMITS = {
  tickMs: 10_000,
  blindMs: 90_000,
  hostGraceMs: 30_000,
  viewerGraceMs: 15_000,
  maxStreamMs: 4 * 3600_000,
  ticketsPerMinute: 20,
  keyBatchesPerMinute: 60,
  wrapsPerBatch: 1_000,
  reactionsPer10s: 6,
  reactionFlushMs: 500,
  reactionsPerFlush: 30,
  viewerCountMs: 2_000,
  streamsTotal: 200,
};

export function streamConfigFromEnv(env = process.env) {
  return {
    // The most viewers one stream may have (SDS §14A.5); the postbox may say fewer, the budget too.
    maxViewers: Number(env.CALLS_STREAM_MAX_VIEWERS || 200),
    // All viewers of all streams together, kept well inside the media host's CPU (measured
    // 2026-10-27: ~0.9% of one core per viewer, 155 viewers = 1.4 of 5 vCPUs; docs/streaming.md).
    totalViewers: Number(env.CALLS_STREAM_TOTAL_VIEWERS || 400),
    // What one viewer costs the media host's upload, in Mbit/s, on the wire: a camera stream (720p
    // capped at 1.7 Mbit/s plus voice, +18% measured overhead), and one with a shared screen (2.5
    // Mbit/s plus voice and a small camera). Measured on the production media host, 2026-10-27.
    viewerMbps: Number(env.CALLS_STREAM_VIEWER_MBPS || 2.1),
    screenViewerMbps: Number(env.CALLS_STREAM_SCREEN_VIEWER_MBPS || 3.2),
    // How many viewers a new stream must have room for before it may start.
    startReserve: Number(env.CALLS_STREAM_START_RESERVE || 10),
  };
}

export function createStreams(ctx) {
  const { cfg, internal, HttpError, now, liveSession, mailboxFor, limited, send, sessions, signSfuToken, sfuAdmin, sfuEnabled, otherLoadMbps } = ctx;
  const sfuCall = ctx.sfuCall || (async () => 200);
  const L = { ...STREAM_LIMITS, ...(ctx.limits || {}) };
  const sc = { ...streamConfigFromEnv({}), ...(cfg.streams || {}) };
  const streams = new Map(); // stream id -> stream room
  const newId = ctx.newId;

  // ---- capacity ----

  const rate = (sources) => (sources.includes("screen") ? sc.screenViewerMbps : sc.viewerMbps);
  function viewersOf(st) {
    let n = 0;
    for (const p of st.participants.values()) if (p.role !== "host") n++;
    return n;
  }
  function totalViewers(except) {
    let n = 0;
    for (const st of streams.values()) if (st !== except && !st.ended) n += viewersOf(st);
    return n;
  }
  // What the streams plan to cost the media host's upload. A stream always holds room for its
  // start reserve, so an idle new stream cannot be starved by the next one.
  function loadMbps(except) {
    let total = 0;
    for (const st of streams.values()) {
      if (st === except || st.ended) continue;
      total += Math.max(viewersOf(st), sc.startReserve) * rate(st.sources);
    }
    return total;
  }
  // How many viewers this stream could have right now, given the budget and every other room.
  function effectiveMax(st, postboxMax) {
    const budget = cfg.sfu ? cfg.sfu.budgetMbps : 0;
    const room = Math.floor((budget - otherLoadMbps() - loadMbps(st)) / rate(st.sources));
    const cpu = sc.totalViewers - totalViewers(st);
    return Math.max(0, Math.min(postboxMax || sc.maxViewers, sc.maxViewers, room, cpu));
  }
  function capacityError() {
    const err = new HttpError(503, "stream_capacity");
    err.extra = { message: "Live streams are very busy right now. Please try again in a few minutes." };
    return err;
  }

  // ---- tickets ----

  const LK_SOURCES = { camera: "camera", screen: "screen_share" };
  function grantFor(st, p) {
    const publish = p.role === "host" || p.role === "speaker";
    const sources = p.role === "host" ? ["microphone", ...st.sources.map((s) => LK_SOURCES[s])] : ["microphone", "camera"];
    return {
      room: st.lkRoom, roomJoin: true, canSubscribe: true, canPublishData: false, canUpdateOwnMetadata: false,
      canPublish: publish, canPublishSources: publish ? sources : [],
      // Viewers are invisible to everyone else on the media server: nobody's browser learns who
      // else is watching from it, and 200 viewers cost the others no signaling.
      hidden: !publish,
    };
  }
  function ticket(st, p) {
    const t = Math.floor(now() / 1000);
    const token = signSfuToken(cfg.sfu, { sub: p.id, nbf: t - 5, exp: t + cfg.sfu.tokenTtlSeconds, video: grantFor(st, p) });
    return { url: cfg.sfu.url, token, ttl: cfg.sfu.tokenTtlSeconds };
  }
  // A role change applied on the media server itself: publishing rights appear or disappear for a
  // participant who is already connected.
  function applyRole(st, p) {
    const g = grantFor(st, p);
    const source = { microphone: "MICROPHONE", camera: "CAMERA", screen_share: "SCREEN_SHARE" };
    sfuAdmin("UpdateParticipant", {
      room: st.lkRoom, identity: p.id,
      permission: { can_subscribe: true, can_publish: g.canPublish, can_publish_data: false, can_publish_sources: g.canPublishSources.map((s) => source[s]), hidden: g.hidden },
    }, { roomAdmin: true, room: st.lkRoom });
  }

  // ---- views and events ----

  function participantView(p) {
    return { participant: p.id, mailbox: p.mailbox, role: p.role, key: p.key };
  }
  const publishers = (st) => [...st.participants.values()].filter((p) => p.role === "host" || p.role === "speaker").map(participantView);
  function streamView(st, you) {
    const host = [...st.participants.values()].find((p) => p.role === "host") || null;
    return {
      stream: st.id, you: you ? you.id : undefined, role: you ? you.role : undefined,
      host: host ? participantView(host) : null, host_mailbox: st.hostMailbox, host_key: st.hostKey,
      publishers: publishers(st), viewers: viewersOf(st), max_viewers: st.maxViewers, ends_at: st.endsAt,
    };
  }
  function toAll(st, event, data, except) {
    for (const p of st.participants.values()) if (p !== except) send(sessions.get(p.sessionId), event, data);
  }
  function toHost(st, event, data, except) {
    for (const p of st.participants.values()) if (p.role === "host" && p !== except) send(sessions.get(p.sessionId), event, data);
  }
  // Viewers' comings and goings concern only the streamer (the key holder); publishers' concern all.
  function participantChanged(st, p, change, reason) {
    const data = { stream: st.id, change, reason: reason || null, ...participantView(p) };
    if (p.role === "host" || p.role === "speaker" || change === "role") toAll(st, "stream_participant", data, p);
    else toHost(st, "stream_participant", data, p);
    countSoon(st);
  }
  function countSoon(st) {
    if (st.countTimer) return;
    st.countTimer = setTimeout(() => {
      st.countTimer = null;
      if (!st.ended) toAll(st, "stream_viewers", { stream: st.id, count: viewersOf(st) });
    }, L.viewerCountMs);
    st.countTimer.unref?.();
  }

  // ---- lifecycle ----

  function makeStream(sid, verdict) {
    if (streams.size >= L.streamsTotal) throw capacityError();
    const st = {
      id: sid, lkRoom: "stream_" + sid, groupId: verdict.group_id, hostMailbox: verdict.host, hostKey: verdict.host_key,
      sources: verdict.sources.length ? verdict.sources : ["camera"], participants: new Map(), ended: false,
      createdAt: now(), endsAt: Math.min(verdict.ends_by ? verdict.ends_by * 1000 : Infinity, now() + L.maxStreamMs),
      maxViewers: verdict.max_viewers, lastCheck: now(), reactions: [], reactionTimer: null, countTimer: null,
      hostGrace: null, timers: [],
    };
    streams.set(sid, st);
    const tick = setInterval(() => { check(st).catch(() => {}); }, L.tickMs);
    tick.unref?.();
    const cap = setTimeout(() => end(st, "max_duration", true), Math.max(0, st.endsAt - now()));
    cap.unref?.();
    st.timers.push(tick, cap);
    console.log(`calls: stream=${sid.slice(4, 10)} started`);
    return st;
  }

  function end(st, reason, tellPostbox) {
    if (st.ended) return;
    st.ended = true;
    for (const t of st.timers) { clearInterval(t); clearTimeout(t); }
    clearTimeout(st.reactionTimer); clearTimeout(st.countTimer); clearTimeout(st.hostGrace);
    for (const p of st.participants.values()) { clearTimeout(p.grace); send(sessions.get(p.sessionId), "stream_ended", { stream: st.id, reason }); }
    st.participants.clear();
    if (cfg.sfu && cfg.sfu.apiUrl) sfuAdmin("DeleteRoom", { room: st.lkRoom }, { roomCreate: true });
    if (tellPostbox && internal.streamEnd) internal.streamEnd({ stream_id: st.id, reason: ["host_left", "media_failed", "max_duration", "capacity"].includes(reason) ? reason : "media_failed" });
    console.log(`calls: stream=${st.id.slice(4, 10)} ended (${reason})`);
    const cleanup = setTimeout(() => { if (streams.get(st.id) === st) streams.delete(st.id); }, 60_000);
    cleanup.unref?.();
  }

  function removeParticipant(st, p, reason) {
    if (!st.participants.has(p.id)) return;
    clearTimeout(p.grace);
    st.participants.delete(p.id);
    if (cfg.sfu && cfg.sfu.apiUrl) sfuAdmin("RemoveParticipant", { room: st.lkRoom, identity: p.id }, { roomAdmin: true, room: st.lkRoom });
    participantChanged(st, p, "left", reason);
    if (reason === "revoked") send(sessions.get(p.sessionId), "stream_ended", { stream: st.id, reason: "revoked" });
    if (p.role === "host" && !st.ended) {
      // The streamer is gone: give them a moment to come back (a reload), then end the stream.
      clearTimeout(st.hostGrace);
      st.hostGrace = setTimeout(() => {
        if (![...st.participants.values()].some((x) => x.role === "host")) end(st, "host_left", true);
      }, reason === "left" ? 0 : L.hostGraceMs);
      st.hostGrace.unref?.();
    }
  }

  // The heartbeat: report, learn the state and everyone's current role, act on it.
  async function check(st) {
    if (st.ended) return;
    const members = [...new Set([...st.participants.values()].map((p) => p.mailbox))];
    const verdict = await internal.streamCheck({ stream_id: st.id, viewers: viewersOf(st), members });
    if (st.ended) return;
    if (!verdict) {
      if (now() - st.lastCheck > L.blindMs) end(st, "media_failed", false);
      return;
    }
    st.lastCheck = now();
    if (verdict.state !== "live") { end(st, verdict.reason || "ended", false); return; }
    if (verdict.host_key) st.hostKey = verdict.host_key;
    for (const p of [...st.participants.values()]) {
      if (!verdict.roles.has(p.mailbox)) continue;
      const role = verdict.roles.get(p.mailbox);
      if (role === null) { removeParticipant(st, p, "revoked"); continue; }
      if (role !== p.role) setRole(st, p, role);
    }
  }

  function setRole(st, p, role) {
    if (p.role === role) return;
    // A host is the host from the first ticket; nobody becomes one later.
    if (role === "host" || p.role === "host") return;
    const wasPublisher = p.role === "speaker";
    p.role = role;
    applyRole(st, p);
    send(sessions.get(p.sessionId), "stream_role", { stream: st.id, role });
    participantChanged(st, p, "role", wasPublisher ? "stage_off" : null);
  }

  // ---- routes ----

  function streamFor(sid) {
    const st = streams.get(sid);
    if (!st || st.ended) throw new HttpError(404, "no_stream");
    return st;
  }
  function participantFor(st, auth, sessionId) {
    const session = liveSession(auth, sessionId);
    for (const p of st.participants.values()) if (p.sessionId === session.id) return p;
    throw new HttpError(403, "not_in_stream");
  }

  async function admit(sid, from, publicKey) {
    if (!internal.configured || typeof internal.streamAdmit !== "function") throw new HttpError(503, "streams_unavailable");
    const verdict = await internal.streamAdmit({ stream_id: sid, from, public_key: publicKey });
    if (!verdict) throw new HttpError(503, "streams_unavailable");
    if (!verdict.allowed) {
      if (verdict.reason === "stream_ended") throw new HttpError(410, "stream_ended");
      if (verdict.reason === "no_stream") throw new HttpError(404, "no_stream");
      throw new HttpError(403, "not_member");
    }
    return verdict;
  }

  async function join(auth, sid, body) {
    if (auth.guest) throw new HttpError(403, "members_only");
    if (!sfuEnabled()) throw new HttpError(503, "streams_unavailable");
    const session = liveSession(auth, body.session);
    const from = mailboxFor(auth, body.from || "");
    if (!from) throw new HttpError(403, "not_your_mailbox");
    if (typeof body.public_key !== "string" || !PUBLIC_KEY.test(body.public_key)) throw new HttpError(400, "bad_key");
    // There is no unencrypted stream: a browser that cannot encrypt or decrypt frames is refused.
    if (body.e2ee !== true) throw new HttpError(409, "needs_e2ee");
    const verdict = await admit(sid, from, body.public_key);
    // Only a key the member registered with the postbox (the key directory) is taken: this service
    // cannot put a listener of its own into the stream.
    if (!verdict.key_attested) throw new HttpError(409, "key_not_registered");
    if (session.closed) throw new HttpError(409, "no_session");
    let st = streams.get(sid);
    if (st && st.ended) st = null;
    const isHost = verdict.role === "host";
    if (!st) {
      if (!isHost) throw new HttpError(409, "not_started");
      const budget = cfg.sfu.budgetMbps;
      if (otherLoadMbps() + loadMbps() + sc.startReserve * rate(verdict.sources) > budget || totalViewers() + sc.startReserve > sc.totalViewers) throw capacityError();
      st = makeStream(sid, verdict);
      // The media room is made here, sized for this stream (the media server's default room size
      // is the one for calls), before anyone connects to it.
      const size = Math.min(verdict.max_viewers, sc.maxViewers) + 10;
      let status = 0;
      try { status = await sfuCall("CreateRoom", { name: st.lkRoom, max_participants: size, empty_timeout: 300, departure_timeout: 30 }, { roomCreate: true }); } catch (_) { status = 0; }
      if (status !== 200) { end(st, "media_failed", false); throw new HttpError(503, "streams_unavailable"); }
    }
    for (const p of st.participants.values()) if (p.sessionId === session.id) return { ...streamView(st, p), sfu: ticket(st, p) };
    st.maxViewers = verdict.max_viewers;
    if (!isHost) {
      const max = effectiveMax(st, verdict.max_viewers);
      if (viewersOf(st) >= max) {
        if (viewersOf(st) >= Math.min(verdict.max_viewers, sc.maxViewers)) {
          const err = new HttpError(409, "stream_full");
          err.extra = { message: "This stream has reached its viewer limit.", max_viewers: Math.min(verdict.max_viewers, sc.maxViewers) };
          throw err;
        }
        throw capacityError();
      }
    }
    const p = {
      id: newId(), sessionId: session.id, account: auth.account, mailbox: from.address,
      role: verdict.role, key: body.public_key, joinedAt: now(), grace: null,
    };
    if (isHost) {
      // A reload: the previous host participant is replaced, not kept beside the new one.
      for (const old of [...st.participants.values()]) if (old.role === "host") { st.participants.delete(old.id); clearTimeout(old.grace); if (cfg.sfu.apiUrl) sfuAdmin("RemoveParticipant", { room: st.lkRoom, identity: old.id }, { roomAdmin: true, room: st.lkRoom }); }
      clearTimeout(st.hostGrace);
      st.hostKey = body.public_key;
    }
    st.participants.set(p.id, p);
    participantChanged(st, p, "joined");
    return { ...streamView(st, p), sfu: ticket(st, p) };
  }

  function sfuTicket(auth, sid, body) {
    const st = streamFor(sid);
    const p = participantFor(st, auth, body.session);
    if (limited("stream-sfu:" + p.id, L.ticketsPerMinute, 60_000)) throw new HttpError(429, "rate_limited");
    return ticket(st, p);
  }

  // The streamer hands the wrapped stream key to participants. Only the streamer: everyone else in a
  // stream receives keys, never sends them.
  function keys(auth, sid, body) {
    const st = streamFor(sid);
    const p = participantFor(st, auth, body.session);
    if (p.role !== "host") throw new HttpError(403, "not_host");
    if (limited("stream-keys:" + p.id, L.keyBatchesPerMinute, 60_000)) throw new HttpError(429, "rate_limited");
    const epoch = body.epoch;
    if (!Number.isInteger(epoch) || epoch < 1 || epoch > 1e9 || !Array.isArray(body.wraps) || body.wraps.length > L.wrapsPerBatch) throw new HttpError(400, "bad_keys");
    let delivered = 0;
    for (const w of body.wraps) {
      if (!w || typeof w.iv !== "string" || !WRAP_IV.test(w.iv) || typeof w.key !== "string" || !WRAP_KEY.test(w.key)) throw new HttpError(400, "bad_keys");
      const to = st.participants.get(String(w.to || ""));
      if (!to || to.id === p.id) continue;
      if (send(sessions.get(to.sessionId), "stream_key", { stream: st.id, from: p.id, epoch, iv: w.iv, key: w.key })) delivered++;
    }
    return { ok: true, delivered };
  }

  // A reaction, sealed with the stream key by the viewer's browser (this service cannot read it),
  // passed on to everyone in small batches.
  function react(auth, sid, body) {
    const st = streamFor(sid);
    const p = participantFor(st, auth, body.session);
    if (typeof body.sealed !== "string" || !SEALED.test(body.sealed)) throw new HttpError(400, "bad_reaction");
    if (limited("stream-react:" + p.id, L.reactionsPer10s, 10_000)) throw new HttpError(429, "rate_limited");
    if (st.reactions.length < L.reactionsPerFlush) st.reactions.push({ from: p.id, sealed: body.sealed });
    if (!st.reactionTimer) {
      st.reactionTimer = setTimeout(() => {
        st.reactionTimer = null;
        const items = st.reactions.splice(0);
        if (items.length && !st.ended) toAll(st, "stream_reactions", { stream: st.id, items });
      }, L.reactionFlushMs);
      st.reactionTimer.unref?.();
    }
    return { ok: true };
  }

  // After accepting or leaving the stage: ask the postbox again and apply the answer.
  async function role(auth, sid, body) {
    const st = streamFor(sid);
    const p = participantFor(st, auth, body.session);
    if (limited("stream-role:" + p.id, 20, 60_000)) throw new HttpError(429, "rate_limited");
    const verdict = await admit(sid, { address: p.mailbox }, p.key);
    if (!st.participants.has(p.id)) throw new HttpError(403, "not_in_stream");
    setRole(st, p, verdict.role);
    return { role: p.role, publishers: publishers(st) };
  }

  async function recheck(auth, sid, body) {
    const st = streamFor(sid);
    const p = participantFor(st, auth, body.session);
    if (p.role !== "host") throw new HttpError(403, "not_host");
    if (limited("stream-recheck:" + p.id, 12, 60_000)) throw new HttpError(429, "rate_limited");
    await check(st);
    return { ok: true, viewers: viewersOf(st) };
  }

  function leave(auth, sid, body) {
    const st = streamFor(sid);
    const p = participantFor(st, auth, body.session);
    removeParticipant(st, p, "left");
    return { ok: true };
  }

  // The streamer stops (after `DELETE /v1/streams/{id}` on the postbox, which records it).
  function stop(auth, sid, body) {
    const st = streamFor(sid);
    const p = participantFor(st, auth, body.session);
    if (p.role !== "host") throw new HttpError(403, "not_host");
    end(st, "stopped", false);
    return { ok: true };
  }

  // An event stream closed: viewers get a short grace period (a reload rejoins anyway), the host a
  // longer one before the stream ends.
  function sessionGone(session) {
    for (const st of streams.values()) {
      if (st.ended) continue;
      for (const p of st.participants.values()) {
        if (p.sessionId !== session.id) continue;
        clearTimeout(p.grace);
        p.grace = setTimeout(() => {
          if (!sessions.has(p.sessionId)) removeParticipant(st, p, "disconnected");
        }, p.role === "host" ? L.hostGraceMs : L.viewerGraceMs);
        p.grace.unref?.();
      }
    }
  }

  async function handle(req, path, auth, readJson) {
    const m = /^\/v1\/streams\/(str_[a-z2-7]{26})\/(join|sfu|keys|react|role|recheck|leave|stop)$/.exec(path);
    if (!m || req.method !== "POST") return undefined;
    const body = await readJson(req);
    const action = { join, sfu: sfuTicket, keys, react, role, recheck, leave, stop }[m[2]];
    return action(auth, m[1], body);
  }

  function health() {
    let live = 0, viewers = 0;
    for (const st of streams.values()) if (!st.ended) { live++; viewers += viewersOf(st); }
    return { live, viewers, load_mbps: Math.round(loadMbps()), viewer_mbps: sc.viewerMbps, screen_viewer_mbps: sc.screenViewerMbps, max_viewers: sc.maxViewers, total_viewers: sc.totalViewers };
  }

  return { handle, sessionGone, loadMbps, health, streams, check, _end: end };
}
