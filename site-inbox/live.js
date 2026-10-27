// Pigeonpost — live streams in groups: the call client's stream mode (Phase 6, SDS §14A).
//
// One member of a group goes live with a camera and/or a screen; the group's members watch. Media
// goes through the media server (LiveKit), end-to-end encrypted the same way as large calls
// (calls-e2ee.js): every frame is encrypted in the streamer's browser with a random stream key.
//
// How the stream key reaches viewers, and only them:
//
// - Every watching device makes a fresh P-256 key pair and registers the public half with the
//   POSTBOX (`POST /v1/streams/{id}/watch`) — the key directory. The streamer registered theirs when
//   going live, and every viewer learns the streamer's key from the postbox, never from the calls
//   service.
// - The calls service tells the streamer who joined the media room. The streamer's browser wraps
//   the stream key only to participants whose key the postbox lists for a current member
//   (`GET /v1/streams/{id}/audience`), and the viewer's browser only accepts a key wrapped by the key
//   the postbox named. A calls service that wanted to listen in would have to get a key of its own
//   into the postbox's directory.
// - When someone leaves or is removed from the group, the streamer makes a new key and hands it to
//   everyone still allowed; the media server also drops the removed person at once.
//
// Viewers can only receive: their media-server ticket has no publishing right. A viewer the streamer
// invites onto the stage must accept before any microphone or camera opens; the media server then
// grants the publishing right. Reactions are sealed with the stream key. Browsers that cannot encrypt
// frames are told so; there is no unencrypted stream.
//
// The group chat stays the group's own end-to-end encrypted chat (groups.js), docked beside the
// player while watching.
(function () {
  "use strict";

  const C = () => window.PigeonpostCalls && window.PigeonpostCalls._live;
  const E = () => window.PigeonpostCallsE2EE;
  const LK = () => window.LivekitClient;
  const $ = (id) => document.getElementById(id);
  const KEY_SWITCH_MS = 1500;
  const REACTIONS = ["👍", "👏", "❤️"];
  const REACT_EVERY_MS = 2000;
  const NEEDS_NEWER = "Live streams need a newer browser or app — one that can encrypt and decrypt audio and video itself. Update this browser or app, or use a current Chrome, Edge, Firefox or Safari.";
  const MAX_FLOATING = 12;

  let L = null; // the stream on screen

  const ERRORS = {
    stream_capacity: "Live streams are very busy right now. Please try again in a few minutes.",
    stream_full: "This stream has reached its viewer limit.",
    stream_in_progress: "Someone is already live in this group.",
    stream_restricted: "Only admins can go live in this group.",
    stream_ended: "This stream has ended.",
    not_started: "The stream is still starting. Try again in a moment.",
    not_member: "You are not an active member of this group.",
    feature_disabled: "Live streams are not available for this account yet.",
    needs_e2ee: NEEDS_NEWER,
    human_required: "Going live needs you to be signed in yourself, not an agent key.",
    key_not_registered: "The stream’s security check failed. Try again.",
    streams_unavailable: "Live streams are unavailable right now. Please try again later.",
    rate_limited: "Too many attempts. Wait a minute and try again.",
  };
  const explain = (e) => (e && (ERRORS[e.code] || (e.body && e.body.message))) || "Something went wrong. Please try again.";

  // ---- the postbox (stream state, the key directory) ----

  function postboxBase() {
    const h = C() && C().host();
    return (h && typeof h.postbox === "function" && h.postbox()) || (window.PIGEONPOST_INBOX && window.PIGEONPOST_INBOX.postbox) || "https://postbox.pigeonpost.dev";
  }
  async function pb(method, path, body, renewed) {
    const h = C().host();
    const me = h && h.mailbox();
    if (!h || !me) throw Object.assign(new Error("signed_out"), { code: "signed_out" });
    const res = await fetch(postboxBase() + path, {
      method,
      headers: Object.assign({ authorization: "Bearer " + h.token(), accept: "application/json", "x-pigeonpost-identity": me.address }, body ? { "content-type": "application/json" } : {}),
      body: body ? JSON.stringify(body) : undefined,
    });
    if (res.status === 401 && !renewed && (await h.renew())) return pb(method, path, body, true);
    const value = await res.json().catch(() => ({}));
    if (!res.ok) throw Object.assign(new Error(value.error || "http_" + res.status), { status: res.status, code: value.error, body: value });
    return value;
  }
  const calls = (path, body) => C().post("/streams/" + L.id + path, Object.assign({ session: C().session() }, body || {}));

  // ---- going live, watching ----

  async function ready() {
    const c = C();
    if (!c) throw Object.assign(new Error("unavailable"), { code: "streams_unavailable" });
    if (!c.canEncryptFrames) throw Object.assign(new Error("e2ee"), { code: "needs_e2ee" });
    if (c.busy()) throw Object.assign(new Error("busy"), { code: "busy", message: "Finish the current call first." });
    c.start();
    await Promise.race([c.whenReady(), new Promise((_, rej) => setTimeout(() => rej(Object.assign(new Error("calls"), { code: "streams_unavailable" })), 10_000))]);
    await c.loadLibs();
  }

  function fresh(fields) {
    return Object.assign({
      id: null, groupId: null, groupName: "", title: "", role: null, you: null, me: null,
      identity: null, hostPid: null, hostKey: null, hostMailbox: null, keys: null, worker: null, lk: null,
      epoch: 0, sendEpoch: 0, dataKeys: new Map(), viewers: 0, maxViewers: null, ended: false,
      participants: new Map(), // host only: pid -> {mailbox, key, role, wrapped}
      audience: new Map(), // host only: mailbox -> Set(keys) from the postbox
      publishers: new Map(), // pid -> {mailbox, role, key}
      local: { mic: null, cam: null, screen: null, muted: false }, pubs: {},
      timers: [], rotateTimer: null, audienceTimer: null, lastReact: 0, reactions: 0, decryptErrors: 0, unencrypted: 0,
      rotations: 0, keyErrors: 0, startedAt: Date.now(), code: null, stageInvite: null, chatDocked: false,
    }, fields);
  }

  // `group`: {id, name}; `opts`: {title, camera, screen}.
  async function goLive(group, opts) {
    if (L) { C()?.toast("You are already in a live stream."); return { ok: false, code: "busy" }; }
    const sources = [opts.camera && "camera", opts.screen && "screen"].filter(Boolean);
    try {
      await ready();
      L = fresh({ groupId: group.id, groupName: group.name, title: String(opts.title || "").trim(), role: "host", me: C().host().mailbox().address });
      const mine = L;
      buildUI();
      showUI("Starting your stream…");
      await captureHost(mine, opts);
      if (L !== mine) return { ok: false, code: "cancelled" };
      mine.identity = await E().generateIdentity();
      const started = await pb("POST", `/v1/groups/${encodeURIComponent(group.id)}/streams`, { title: mine.title, sources, public_key: mine.identity.publicKey });
      if (L !== mine) { pb("DELETE", `/v1/streams/${started.stream.id}`).catch(() => {}); return { ok: false, code: "cancelled" }; }
      Object.assign(mine, { id: started.stream.id, hostKey: started.host_key, hostMailbox: mine.me, startedAt: (started.stream.started_at || Date.now() / 1000) * 1000 });
      await joinMedia(mine);
      return { ok: true, stream: mine.id };
    } catch (e) {
      const code = e && e.code;
      finish(explain(e), { stop: false });
      return { ok: false, code: code || "error" };
    }
  }

  async function watch(group, streamId) {
    if (L && L.id === streamId) { showUI(); return { ok: true }; }
    if (L) { C()?.toast("Leave the current stream first."); return { ok: false, code: "busy" }; }
    try {
      await ready();
      L = fresh({ id: streamId, groupId: group.id, groupName: group.name, role: "viewer", me: C().host().mailbox().address });
      const mine = L;
      buildUI();
      showUI("Joining the stream…");
      mine.identity = await E().generateIdentity();
      const w = await pb("POST", `/v1/streams/${encodeURIComponent(streamId)}/watch`, { public_key: mine.identity.publicKey });
      if (L !== mine) return { ok: false, code: "cancelled" };
      Object.assign(mine, { title: w.stream.title, hostKey: w.host_key, hostMailbox: w.stream.by, role: w.role, startedAt: w.stream.started_at * 1000, viewers: w.stream.viewers || 0 });
      if (w.role === "host") await captureHost(mine, { camera: w.stream.sources.includes("camera"), screen: w.stream.sources.includes("screen") });
      await joinMedia(mine);
      if (w.role === "invited") offerStage(mine, w.stream.stage.find((x) => x.address === mine.me));
      return { ok: true };
    } catch (e) {
      finish(explain(e), { stop: false });
      return { ok: false, code: (e && e.code) || "error" };
    }
  }

  async function joinMedia(mine) {
    const joined = await C().post("/streams/" + mine.id + "/join", { session: C().session(), from: mine.me, public_key: mine.identity.publicKey, e2ee: true });
    if (L !== mine) { C().post("/streams/" + mine.id + "/leave", { session: C().session() }).catch(() => {}); return; }
    Object.assign(mine, { you: joined.you, role: joined.role, viewers: joined.viewers, maxViewers: joined.max_viewers });
    const early = mine.pendingWraps || [];
    mine.pendingWraps = null;
    for (const p of joined.publishers || []) mine.publishers.set(p.participant, p);
    if (mine.role === "host") {
      mine.hostPid = joined.you;
      await install(mine, 1, E().randomRoomKey(), true);
      refreshAudience(mine);
      mine.timers.push(setInterval(() => refreshAudience(mine), 30_000));
    } else {
      // The streamer is who the postbox says, with the key the postbox registered for them.
      const host = joined.host;
      if (!host || host.mailbox !== mine.hostMailbox || host.key !== mine.hostKey) {
        throw Object.assign(new Error("host key"), { code: "key_not_registered", message: "The stream’s security check failed: the streamer’s key does not match. Nothing was shown." });
      }
      mine.hostPid = host.participant;
      for (const w of early) onWrappedKey(mine, w);
    }
    await connect(mine, joined.sfu);
    updateCode(mine);
    render();
  }

  // ---- local media (streamer and speakers) ----

  async function captureHost(mine, opts) {
    const md = navigator.mediaDevices;
    if (opts.camera || opts.mic !== false) {
      const s = await md.getUserMedia({ audio: { echoCancellation: true, noiseSuppression: true }, video: opts.camera ? { width: { ideal: 1280 }, height: { ideal: 720 }, frameRate: { ideal: 30 } } : false });
      mine.local.mic = s.getAudioTracks()[0] || null;
      mine.local.cam = s.getVideoTracks()[0] || null;
    }
    if (opts.screen) {
      const s = await md.getDisplayMedia({ video: { frameRate: { ideal: 15, max: 30 } }, audio: false });
      mine.local.screen = s.getVideoTracks()[0] || null;
      if (mine.local.screen) mine.local.screen.addEventListener("ended", () => { if (L === mine) { mine.local.screen = null; publishAll(mine); render(); } });
    }
  }
  function stopLocal(mine) {
    for (const t of [mine.local.mic, mine.local.cam, mine.local.screen]) try { t && t.stop(); } catch (_) { /* gone */ }
    mine.local = { mic: null, cam: null, screen: null, muted: false };
  }

  // ---- the media server ----

  async function connect(mine, ticket) {
    const K = LK();
    class StreamKeys extends K.BaseKeyProvider {
      constructor() { super({ sharedKey: true, ratchetWindowSize: 0, failureTolerance: -1 }); }
      stage(material, slot) { this.emit(K.KeyProviderEvent.SetKey, { key: material, participantIdentity: undefined, keyIndex: slot }, false); }
      use(material, slot) { this.onSetEncryptionKey(material, undefined, slot); }
    }
    mine.keys = new StreamKeys();
    mine.worker = new Worker(C().E2EE_WORKER);
    const publisher = mine.role === "host";
    const lk = new K.Room({
      adaptiveStream: !publisher, dynacast: true, stopLocalTrackOnUnpublish: false, disconnectOnPageLeave: true,
      e2ee: { keyProvider: mine.keys, worker: mine.worker },
      publishDefaults: {
        simulcast: true, videoSimulcastLayers: [K.VideoPresets.h180, K.VideoPresets.h360], videoCodec: "vp8",
        videoEncoding: { maxBitrate: 1_700_000, maxFramerate: 30 },
        screenShareEncoding: { maxBitrate: 2_500_000, maxFramerate: 15 },
        dtx: false, red: false, audioPreset: { maxBitrate: 64_000 },
      },
    });
    mine.lk = lk;
    for (const [epoch, material] of mine.pendingKeys || []) mine.keys.use(material, E().slotFor(epoch));
    const R = K.RoomEvent;
    const own = (fn) => (...a) => { if (L === mine) fn(...a); };
    lk.on(R.TrackPublished, own(() => syncSubscriptions(mine)))
      .on(R.ParticipantConnected, own(() => syncSubscriptions(mine)))
      .on(R.TrackSubscribed, own((track, pub, rp) => onTrack(mine, track, pub, rp, true)))
      .on(R.TrackUnsubscribed, own((track, pub, rp) => onTrack(mine, track, pub, rp, false)))
      .on(R.EncryptionError, own(() => { mine.decryptErrors += 1; }))
      .on(R.ParticipantPermissionsChanged, own((_prev, participant) => { if (participant === lk.localParticipant) publishAll(mine); }))
      .on(R.Reconnecting, own(() => setStatus("Reconnecting…")))
      .on(R.Reconnected, own(() => render()))
      .on(R.Disconnected, own(() => onDisconnected(mine)));
    await lk.setE2EEEnabled(true);
    const ice = await C().turnServers();
    const url = /^wss?:\/\//.test(ticket.url) ? ticket.url : location.origin.replace(/^http/, "ws") + ticket.url;
    mine.url = url;
    mine.rtc = { iceServers: ice, iceTransportPolicy: C().forceRelay ? "relay" : "all" };
    await lk.connect(url, ticket.token, { autoSubscribe: false, rtcConfig: { iceServers: ice, iceTransportPolicy: C().forceRelay ? "relay" : "all" } });
    if (L !== mine) { lk.disconnect(); return; }
    mine.connected = true;
    mine.retries = 0;
    await publishAll(mine);
    syncSubscriptions(mine);
  }

  function onDisconnected(mine) {
    mine.connected = false;
    if (mine.ended) return;
    if ((mine.retries = (mine.retries || 0) + 1) > 4) { finish("Lost the connection to the stream.", { stop: mine.role === "host" }); return; }
    setStatus("Reconnecting…");
    setTimeout(async () => {
      if (L !== mine || mine.ended) return;
      try {
        await mine.lk.connect(mine.url, (await calls("/sfu")).token, { autoSubscribe: false, rtcConfig: mine.rtc });
        mine.connected = true;
        mine.pubs = {};
        await publishAll(mine);
        syncSubscriptions(mine);
        render();
      } catch (_) { onDisconnected(mine); }
    }, 1000 * mine.retries);
  }

  const canPublish = (mine) => mine.role === "host" || mine.role === "speaker";
  async function publishAll(mine) {
    if (!mine.lk || !mine.connected) return;
    const K = LK();
    const lp = mine.lk.localParticipant;
    const want = canPublish(mine) ? mine.local : { mic: null, cam: null, screen: null };
    const single = C().singleLayer();
    for (const slot of ["mic", "cam", "screen"]) {
      const track = want[slot];
      const current = mine.pubs[slot];
      try {
        if (!track || mine.sendEpoch === 0) {
          if (current) { delete mine.pubs[slot]; await lp.unpublishTrack(current.track, false); }
          continue;
        }
        if (current) continue;
        const source = { mic: K.Track.Source.Microphone, cam: K.Track.Source.Camera, screen: K.Track.Source.ScreenShare }[slot];
        const options = { source, name: source };
        if (slot === "cam" && single) Object.assign(options, { simulcast: false, videoEncoding: { maxBitrate: 1_200_000, maxFramerate: 24 } });
        mine.pubs[slot] = await lp.publishTrack(track, options);
      } catch (err) {
        mine.lastError = "publish " + slot + ": " + String((err && (err.message || err.name)) || err).slice(0, 120);
      }
    }
    render();
  }

  // Receive only what the stream's publishers (the streamer, and speakers on the stage) send, and
  // only if it is end-to-end encrypted.
  function syncSubscriptions(mine) {
    if (!mine.lk) return;
    for (const rp of mine.lk.remoteParticipants.values()) {
      const allowed = mine.publishers.has(rp.identity) || rp.identity === mine.hostPid;
      for (const pub of rp.trackPublications.values()) {
        // Nothing is received before this browser holds a stream key: frames it cannot open would
        // only be dropped.
        const want = allowed && pub.isEncrypted && mine.epoch > 0;
        if (!pub.isEncrypted && allowed && !pub.ppRefused) { pub.ppRefused = true; mine.unencrypted += 1; }
        if (pub.isDesired !== want) pub.setSubscribed(want);
      }
    }
  }

  function onTrack(mine, track, pub, rp, on) {
    if (on && !pub.isEncrypted) { pub.setSubscribed(false); return; }
    renderMedia(mine);
  }

  // ---- keys ----

  async function install(mine, epoch, roomKey, first) {
    const material = roomKey.slice().buffer;
    mine.dataKeys.set(epoch, await E().dataKey(roomKey));
    for (const e of mine.dataKeys.keys()) if (e <= epoch - E().KEY_SLOTS) mine.dataKeys.delete(e);
    mine.epoch = epoch;
    mine.currentKey = roomKey;
    const slot = E().slotFor(epoch);
    if (!mine.keys) { (mine.pendingKeys ||= []).push([epoch, material]); mine.sendEpoch = epoch; return; }
    if (first || !mine.sendEpoch) { mine.keys.use(material, slot); mine.sendEpoch = epoch; publishAll(mine); return; }
    mine.keys.stage(material, slot);
    clearTimeout(mine.switchTimer);
    mine.switchTimer = setTimeout(() => { if (L === mine && mine.epoch === epoch) { mine.keys.use(material, slot); mine.sendEpoch = epoch; } }, KEY_SWITCH_MS);
  }

  // The streamer: the postbox's key directory, i.e. who may receive the stream key.
  async function refreshAudience(mine) {
    if (L !== mine || mine.role !== "host") return;
    try {
      const a = await pb("GET", `/v1/streams/${mine.id}/audience`);
      const next = new Map();
      for (const x of a.audience || []) next.set(x.address, new Set(x.keys || []));
      mine.audience = next;
      mine.audienceRev = a.revision;
    } catch (e) {
      if (e && (e.status === 410 || e.status === 404)) { finish("Your stream has ended.", { stop: false }); return; }
      return;
    }
    // Anyone holding the key who is no longer in the directory (removed from the group) forces a new key.
    let revoked = false;
    for (const p of mine.participants.values()) {
      const ok = authorised(mine, p);
      if (p.wrapped && !ok) { p.wrapped = false; revoked = true; }
    }
    if (revoked) scheduleRotate(mine, 0);
    else wrapNewcomers(mine);
  }
  const authorised = (mine, p) => Boolean(p.key && mine.audience.get(p.mailbox) && mine.audience.get(p.mailbox).has(p.key));

  async function wrapTo(mine, list, epoch, roomKey) {
    const from = { id: mine.you, key: mine.identity.publicKey };
    const wraps = [];
    for (const p of list) {
      try {
        const w = await E().wrapRoomKey({ privateKey: mine.identity.privateKey, roomId: mine.id, epoch, from, to: { id: p.pid, key: p.key }, roomKey });
        wraps.push({ to: p.pid, iv: w.iv, key: w.key });
      } catch (_) { mine.keyErrors += 1; }
    }
    for (let i = 0; i < wraps.length; i += 500) {
      await calls("/keys", { epoch, wraps: wraps.slice(i, i + 500) }).catch(() => { mine.keyErrors += 1; });
    }
  }

  let wrapping = null;
  async function wrapNewcomers(mine) {
    if (L !== mine || mine.role !== "host" || !mine.currentKey) return;
    const list = [...mine.participants.values()].filter((p) => !p.wrapped && authorised(mine, p));
    const unknown = [...mine.participants.values()].some((p) => !p.wrapped && !authorised(mine, p));
    if (unknown && !mine.audienceTimer) mine.audienceTimer = setTimeout(() => { mine.audienceTimer = null; refreshAudience(mine); }, 1500);
    if (!list.length) return;
    for (const p of list) p.wrapped = true;
    const epoch = mine.epoch, key = mine.currentKey;
    wrapping = wrapTo(mine, list, epoch, key);
    await wrapping;
  }

  // A new stream key for everyone still allowed: after a removal at once, after people leave a
  // little later (several departures make one new key).
  function scheduleRotate(mine, ms) {
    if (mine.role !== "host") return;
    if (mine.rotateTimer && ms > 0) return;
    clearTimeout(mine.rotateTimer);
    mine.rotateTimer = setTimeout(() => { mine.rotateTimer = null; rotate(mine).catch(() => {}); }, ms);
  }
  async function rotate(mine) {
    if (L !== mine || mine.ended) return;
    try {
      const a = await pb("GET", `/v1/streams/${mine.id}/audience`);
      mine.audience = new Map((a.audience || []).map((x) => [x.address, new Set(x.keys || [])]));
    } catch (_) { /* keep the last directory; only fewer people can be in it */ }
    const epoch = mine.epoch + 1;
    const roomKey = E().randomRoomKey();
    mine.rotations += 1;
    const list = [...mine.participants.values()].filter((p) => authorised(mine, p));
    for (const p of mine.participants.values()) p.wrapped = list.includes(p);
    await install(mine, epoch, roomKey, false);
    await wrapTo(mine, list, epoch, roomKey);
  }

  async function onWrappedKey(mine, data) {
    // Only the streamer's participant, and only with the key the postbox registered for them.
    if (data.from !== mine.hostPid || !mine.hostKey || data.epoch <= mine.epoch) return;
    try {
      const roomKey = await E().unwrapRoomKey({ privateKey: mine.identity.privateKey, roomId: mine.id, from: { id: data.from, key: mine.hostKey }, to: { id: mine.you, key: mine.identity.publicKey }, wrapped: data });
      if (L !== mine || data.epoch <= mine.epoch) return;
      await install(mine, data.epoch, roomKey, !mine.sendEpoch);
      syncSubscriptions(mine);
      render();
    } catch (_) { mine.keyErrors += 1; }
  }

  // The code a viewer can compare with the streamer: both their keys, slowly hashed (calls-e2ee.js).
  async function updateCode(mine) {
    if (!mine.hostKey || !mine.identity) return;
    const pair = mine.role === "host" ? null : [
      { id: mine.hostPid, mailbox: { address: mine.hostMailbox }, key: mine.hostKey },
      { id: mine.you, mailbox: { address: mine.me }, key: mine.identity.publicKey },
    ];
    if (!pair) return;
    try { mine.code = await E().securityCode(mine.id, pair); render(); } catch (_) { /* later */ }
  }
  async function viewerCode(mine, p) {
    return E().securityCode(mine.id, [
      { id: mine.you, mailbox: { address: mine.me }, key: mine.identity.publicKey },
      { id: p.pid, mailbox: { address: p.mailbox }, key: p.key },
    ]);
  }

  // ---- events from the calls service ----

  function onCallsEvent(name, data) {
    const mine = L;
    if (!mine || data.stream !== mine.id) return;
    switch (name) {
      case "stream_participant": {
        const pid = data.participant;
        if (data.role === "host" || data.role === "speaker") {
          if (data.change === "left") mine.publishers.delete(pid); else mine.publishers.set(pid, data);
          syncSubscriptions(mine);
        } else if (data.change === "role") {
          mine.publishers.delete(pid);
          syncSubscriptions(mine);
        }
        if (mine.role === "host" && pid !== mine.you) {
          if (data.change === "left") {
            mine.participants.delete(pid);
            scheduleRotate(mine, data.reason === "revoked" ? 200 : 10_000);
          } else {
            const known = mine.participants.get(pid);
            mine.participants.set(pid, { pid, mailbox: data.mailbox, key: data.key, role: data.role, wrapped: known ? known.wrapped : false });
            wrapNewcomers(mine);
          }
        }
        if (data.reason === "revoked" && mine.role === "host") refreshAudience(mine);
        render();
        return;
      }
      case "stream_key":
        // A key can overtake our own join's answer; keep it until we know who we are.
        if (!mine.you || !mine.hostPid) { (mine.pendingWraps ||= []).push(data); return; }
        onWrappedKey(mine, data);
        return;
      case "stream_viewers": mine.viewers = data.count; render(); return;
      case "stream_reactions": showReactions(mine, data.items || []); return;
      case "stream_role":
        mine.role = data.role;
        if (data.role === "speaker") { publishAll(mine); C().toast("You’re on stage. Everyone watching can hear you."); }
        else { publishAll(mine).then(() => { if (mine.role !== "speaker") stopLocal(mine); }); }
        render();
        return;
      case "stream_ended": {
        const why = data.reason === "revoked" ? "You can no longer watch: you are not an active member of this group." : mine.role === "host" ? "Your stream has ended." : "The stream has ended.";
        finish(why, { stop: false });
        return;
      }
      default:
    }
  }

  // A new event stream (the old one dropped): take our place in the stream again, same key.
  function onReady() {
    const mine = L;
    if (!mine || !mine.you || mine.ended) return;
    C().post("/streams/" + mine.id + "/join", { session: C().session(), from: mine.me, public_key: mine.identity.publicKey, e2ee: true })
      .then((j) => { if (L === mine) { mine.you = j.you; if (mine.role === "host") { mine.hostPid = j.you; for (const p of mine.participants.values()) p.wrapped = false; scheduleRotate(mine, 0); } else mine.hostPid = j.host ? j.host.participant : mine.hostPid; } })
      .catch((e) => finish(explain(e), { stop: false }));
  }

  // From the group's typed events (groups.js): a stage invitation for us, a stream that ended, a
  // member removed (the streamer re-reads the key directory at once).
  function onGroupStream(data) {
    const mine = L;
    if (!mine || data.stream_id !== mine.id) return;
    if (data.state === "ended") { finish(mine.role === "host" ? "Your stream has ended." : "The stream has ended.", { stop: false }); return; }
    if (data.stage && data.stage.address === mine.me) {
      if (data.stage.state === "invited") offerStage(mine, data.stage);
      else if (data.stage.state === "viewer" && (mine.role === "speaker" || mine.role === "invited")) { mine.role = "viewer"; mine.stageInvite = null; calls("/role").catch(() => {}); render(); }
    }
    if (data.host_key_changed && mine.role !== "host") {
      // The streamer reloaded: learn their new key from the postbox, not from the calls service.
      pb("GET", `/v1/streams/${mine.id}`).then(() => pb("POST", `/v1/streams/${mine.id}/watch`, { public_key: mine.identity.publicKey })).then((w) => { if (L === mine) { mine.hostKey = w.host_key; updateCode(mine); } }).catch(() => {});
    }
  }
  function onGroupMember() {
    if (L && L.role === "host") { refreshAudience(L); calls("/recheck").catch(() => {}); }
  }

  // ---- the stage ----

  function offerStage(mine, invite) {
    if (!invite || mine.role === "host") return;
    mine.stageInvite = invite;
    render();
  }
  async function acceptStage() {
    const mine = L;
    if (!mine || !mine.stageInvite) return;
    try {
      // Only now, after the person chose to, are the microphone and camera opened.
      await captureHost(mine, { camera: true });
      await pb("POST", `/v1/streams/${mine.id}/stage`, { address: mine.me, on: true });
      mine.stageInvite = null;
      const r = await calls("/role");
      mine.role = r.role;
      await publishAll(mine);
    } catch (e) {
      stopLocal(mine);
      C().toast(e && e.name === "NotAllowedError" ? "Pigeonpost can’t use your microphone or camera. Allow access and try again." : explain(e));
    }
    render();
  }
  async function leaveStage() {
    const mine = L;
    if (!mine) return;
    mine.stageInvite = null;
    await pb("POST", `/v1/streams/${mine.id}/stage`, { address: mine.me, on: false }).catch(() => {});
    const r = await calls("/role").catch(() => null);
    mine.role = r ? r.role : "viewer";
    await publishAll(mine);
    stopLocal(mine);
    render();
  }
  async function setStage(address, on) {
    const mine = L;
    if (!mine) return;
    try {
      await pb("POST", `/v1/streams/${mine.id}/stage`, { address, on });
      await calls("/recheck").catch(() => {});
      C().toast(on ? "Invited onto the stage. They’ll join when they accept." : "Taken off the stage.");
    } catch (e) { C().toast(explain(e)); }
    renderPanel();
  }

  // ---- reactions ----

  async function react(emoji) {
    const mine = L;
    if (!mine || !mine.sendEpoch || !REACTIONS.includes(emoji)) return;
    const t = Date.now();
    if (t - mine.lastReact < REACT_EVERY_MS) return;
    mine.lastReact = t;
    try {
      const sealed = await E().sealData(mine.dataKeys.get(mine.sendEpoch) || mine.dataKeys.get(mine.epoch), { epoch: mine.sendEpoch, roomId: mine.id, senderId: mine.you }, { r: emoji });
      floatReaction(emoji);
      await calls("/react", { sealed: E().b64u(sealed) });
    } catch (_) { /* dropped */ }
  }
  async function showReactions(mine, items) {
    for (const item of items.slice(0, 30)) {
      if (item.from === mine.you) continue;
      try {
        const { value } = await E().openData(async (epoch) => mine.dataKeys.get(epoch) || null, { roomId: mine.id, senderId: item.from }, E().unb64u(item.sealed));
        if (value && REACTIONS.includes(value.r)) { mine.reactions += 1; floatReaction(value.r); }
      } catch (_) { /* not sealed with a key we hold */ }
    }
  }
  function floatReaction(emoji) {
    const layer = $("live-reactions");
    if (!layer || layer.childElementCount >= MAX_FLOATING) return;
    const n = document.createElement("span");
    n.className = "live-float";
    n.textContent = emoji;
    n.style.left = 10 + Math.random() * 70 + "%";
    layer.append(n);
    setTimeout(() => n.remove(), 2600);
  }

  // ---- leaving and stopping ----

  async function stop() {
    const mine = L;
    if (!mine) return;
    if (mine.role === "host" && mine.id) {
      await pb("DELETE", `/v1/streams/${mine.id}`).catch(() => {});
      await calls("/stop").catch(() => {});
    }
    finish(null, { stop: false });
  }

  function finish(message, { stop: stopToo }) {
    const mine = L;
    if (!mine) { if (message) C()?.toast(message); return; }
    mine.ended = true;
    L = null;
    if (stopToo && mine.id && mine.role === "host") pb("DELETE", `/v1/streams/${mine.id}`).catch(() => {});
    if (mine.id && mine.you && C()) C().post("/streams/" + mine.id + "/leave", { session: C().session() }).catch(() => {});
    for (const t of mine.timers) clearInterval(t);
    clearTimeout(mine.rotateTimer); clearTimeout(mine.switchTimer); clearTimeout(mine.audienceTimer);
    try { mine.lk && mine.lk.disconnect(); } catch (_) { /* gone */ }
    setTimeout(() => { try { mine.worker && mine.worker.terminate(); } catch (_) { /* gone */ } }, 1000);
    stopLocal(mine);
    undockChat();
    const ui = $("live-ui");
    if (ui) ui.hidden = true;
    window.PigeonpostGroups?.render?.();
    if (message) C()?.toast(message);
  }

  // ---- the screen ----

  function buildUI() {
    if ($("live-ui")) return;
    const c = C(), el = c.el, svg = c.svg, I = c.ICON;
    const btn = (id, label, icon, cls = "") => el("button", { id, class: "call-btn " + cls, type: "button", "aria-label": label, title: label }, svg(icon));
    const ui = el("div", { class: "call-ui live-ui", id: "live-ui", hidden: true },
      el("section", { class: "live-main", role: "dialog", "aria-modal": "true", "aria-labelledby": "live-title" },
        el("header", { class: "call-top live-top" },
          el("div", { class: "live-who" },
            el("span", { class: "live-badge", text: "LIVE" }),
            el("div", { class: "live-titles" },
              el("h2", { class: "live-title", id: "live-title" }),
              el("p", { class: "live-sub", id: "live-sub", "aria-live": "polite" }))),
          el("div", { class: "call-top-right" },
            el("span", { class: "live-count", id: "live-count", title: "Watching now" }, svg(I.people), el("span", { id: "live-count-n", text: "0" })),
            el("button", { class: "call-icon-btn call-top-btn", id: "live-info-btn", type: "button", title: "Encryption and security code", "aria-label": "Encryption and security code", onclick: () => togglePanel("info") }, svg(I.lock)),
            (document.fullscreenEnabled || document.webkitFullscreenEnabled) ? el("button", { class: "call-icon-btn call-top-btn", type: "button", "aria-label": "Full screen", title: "Full screen", onclick: () => fullscreen() }, svg(I.fullscreen)) : null)),
        el("div", { class: "live-player", id: "live-player" },
          el("video", { id: "live-video", class: "live-video", autoplay: true, playsinline: true, muted: true }),
          el("video", { id: "live-pip", class: "live-pip", autoplay: true, playsinline: true, muted: true, hidden: true }),
          el("div", { class: "live-audio", id: "live-audio", "aria-hidden": "true" }),
          el("p", { class: "live-waiting", id: "live-waiting", role: "status" }),
          el("div", { class: "live-reactions", id: "live-reactions", "aria-hidden": "true" })),
        el("div", { class: "live-stage-offer", id: "live-stage-offer", role: "alertdialog", hidden: true },
          el("p", { id: "live-stage-text" }),
          el("button", { type: "button", class: "call-pill", text: "Join the stage", onclick: () => acceptStage() }),
          el("button", { type: "button", class: "call-pill", text: "No thanks", onclick: () => leaveStage() })),
        el("div", { class: "call-inner-toast", id: "live-toast", role: "status", "aria-live": "polite", hidden: true }),
        el("footer", { class: "call-controls live-controls", id: "live-controls" },
          ...REACTIONS.map((r, i) => el("button", { class: "call-btn live-react", type: "button", id: "live-react-" + i, "aria-label": "React " + r, title: "React", text: r, onclick: () => react(r) })),
          btn("live-mute", "Mute microphone", I.mic + I.slash),
          btn("live-camera", "Turn camera off", I.cam + I.slash),
          btn("live-chat-btn", "Group chat", I.chat),
          btn("live-people-btn", "Viewers and stage", I.people),
          el("button", { id: "live-leave", class: "call-btn call-end", type: "button", "aria-label": "Leave", title: "Leave", onclick: () => leaveOrStop() }, svg(I.end)))),
      el("aside", { class: "call-panel live-panel", id: "live-panel", hidden: true, "aria-labelledby": "live-panel-title" },
        el("header", { class: "call-panel-head" },
          el("h3", { id: "live-panel-title" }),
          el("button", { class: "call-icon-btn", type: "button", "aria-label": "Close panel", title: "Close", onclick: () => togglePanel(null) }, svg(I.close))),
        el("div", { class: "call-panel-body live-chat-dock", id: "live-panel-chat" }),
        el("div", { class: "call-panel-body", id: "live-panel-people" }),
        el("div", { class: "call-panel-body", id: "live-panel-info" })));
    document.body.append(ui);
    $("live-mute").addEventListener("click", () => toggleMute());
    $("live-camera").addEventListener("click", () => toggleCamera());
    $("live-chat-btn").addEventListener("click", () => togglePanel("chat"));
    $("live-people-btn").addEventListener("click", () => togglePanel("people"));
    document.addEventListener("keydown", (e) => { if (e.key === "Escape" && L && !$("live-panel").hidden) togglePanel(null); });
  }

  function showUI(status) {
    const ui = $("live-ui");
    if (!ui) return;
    ui.hidden = false;
    if (status) setStatus(status);
    // Wide screens: the group chat beside the player from the start.
    if (L && !L.panel && window.matchMedia && window.matchMedia("(min-width: 900px)").matches) togglePanel("chat");
    render();
  }
  function setStatus(text) { const s = $("live-sub"); if (s) s.textContent = text; }

  function leaveOrStop() {
    if (!L) return;
    if (L.role === "host") { if (window.confirm("End your stream for everyone?")) stop(); }
    else finish(null, { stop: false });
  }
  function toggleMute() {
    const mine = L;
    if (!mine || !mine.local.mic) return;
    mine.local.muted = !mine.local.muted;
    mine.local.mic.enabled = !mine.local.muted;
    render();
  }
  function toggleCamera() {
    const mine = L;
    if (!mine || !mine.local.cam) return;
    mine.local.cam.enabled = !mine.local.cam.enabled;
    render();
  }
  function fullscreen() {
    const p = $("live-player");
    if (document.fullscreenElement || document.webkitFullscreenElement) (document.exitFullscreen || document.webkitExitFullscreen).call(document);
    else (p.requestFullscreen || p.webkitRequestFullscreen).call(p);
  }

  function togglePanel(view) {
    const mine = L;
    if (!mine) return;
    mine.panel = mine.panel === view ? null : view;
    const panel = $("live-panel");
    panel.hidden = !mine.panel;
    $("live-ui").dataset.panel = mine.panel || "";
    for (const v of ["chat", "people", "info"]) $("live-panel-" + v).hidden = mine.panel !== v;
    $("live-panel-title").textContent = { chat: mine.groupName || "Group chat", people: "Viewers and stage", info: "Encryption" }[mine.panel] || "";
    if (mine.panel === "chat") dockChat(); else undockChat();
    renderPanel();
  }

  // The group's own chat (groups.js), moved beside the player while watching and back afterwards.
  function dockChat() {
    const mine = L;
    if (!mine || mine.chatDocked) return;
    const g = window.PigeonpostGroups;
    if (!g || typeof g.dock !== "function") { $("live-panel-chat").textContent = "Open the group to chat."; return; }
    mine.chatDocked = g.dock(mine.groupId, $("live-panel-chat"));
  }
  function undockChat() {
    if (window.PigeonpostGroups && typeof window.PigeonpostGroups.undock === "function") window.PigeonpostGroups.undock();
    if (L) L.chatDocked = false;
  }

  function name(address) {
    const h = C() && C().host();
    return (h && typeof h.displayName === "function" && h.displayName(address)) || (window.PigeonpostNames ? window.PigeonpostNames.displayName(address) : address);
  }

  function render() {
    const mine = L;
    if (!mine || !$("live-ui")) return;
    $("live-title").textContent = mine.title || "Live";
    const by = mine.role === "host" ? "You are live" : name(mine.hostMailbox || "");
    const minutes = Math.max(0, Math.floor((Date.now() - mine.startedAt) / 60_000));
    if (mine.sendEpoch || mine.role === "host") setStatus(by + " · " + (minutes ? minutes + " min" : "just started") + " · end-to-end encrypted");
    else setStatus("Waiting for the stream’s key…");
    $("live-count-n").textContent = String(mine.viewers || 0);
    const publishing = canPublish(mine);
    $("live-mute").hidden = !publishing || !mine.local.mic;
    $("live-mute").setAttribute("aria-pressed", String(Boolean(mine.local.muted)));
    $("live-mute").classList.toggle("off", Boolean(mine.local.muted));
    $("live-camera").hidden = !publishing || !mine.local.cam;
    $("live-camera").classList.toggle("off", Boolean(mine.local.cam && !mine.local.cam.enabled));
    for (let i = 0; i < REACTIONS.length; i++) $("live-react-" + i).hidden = mine.role === "host";
    $("live-leave").title = $("live-leave").ariaLabel = mine.role === "host" ? "End stream" : "Leave";
    const offer = $("live-stage-offer");
    offer.hidden = !mine.stageInvite || mine.role === "speaker";
    if (mine.stageInvite) $("live-stage-text").textContent = name(mine.stageInvite.by || mine.hostMailbox) + " invited you onto the stage. Your microphone and camera turn on only if you join.";
    renderMedia(mine);
    renderPanel();
  }

  function renderMedia(mine) {
    const video = $("live-video"), pip = $("live-pip"), audio = $("live-audio");
    if (!video) return;
    let main = null, small = null;
    const audios = [];
    if (mine.role === "host") {
      main = mine.local.screen || mine.local.cam;
      small = mine.local.screen ? mine.local.cam : null;
    }
    if (mine.lk) {
      for (const rp of mine.lk.remoteParticipants.values()) {
        for (const pub of rp.trackPublications.values()) {
          if (!pub.isSubscribed || !pub.track || !pub.isEncrypted) continue;
          const t = pub.track.mediaStreamTrack;
          if (pub.source === "microphone") audios.push(t);
          else if (rp.identity === mine.hostPid && pub.source === "screen_share") { small = main && !small ? main : small; main = t; }
          else if (rp.identity === mine.hostPid && pub.source === "camera") { if (main) small = t; else main = t; }
          else if (pub.source === "camera" && !small) small = t; // a speaker
        }
      }
    }
    setTrack(video, main);
    setTrack(pip, small);
    pip.hidden = !small;
    const wanted = new Set(audios);
    for (const node of [...audio.children]) if (!wanted.has(node.ppTrack)) node.remove();
    for (const t of audios) {
      if ([...audio.children].some((n) => n.ppTrack === t)) continue;
      const a = document.createElement("audio");
      a.autoplay = true;
      a.ppTrack = t;
      a.srcObject = new MediaStream([t]);
      audio.append(a);
      a.play().catch(() => {});
    }
    const waiting = $("live-waiting");
    waiting.hidden = Boolean(main);
    waiting.textContent = mine.role === "host" ? "Your stream is starting…" : mine.sendEpoch ? "Waiting for video…" : "Securing the stream…";
  }
  function setTrack(node, track) {
    const current = node.srcObject && node.srcObject.getTracks()[0];
    if (current === track) return;
    node.srcObject = track ? new MediaStream([track]) : null;
    if (track) node.play().catch(() => {});
  }

  async function renderPanel() {
    const mine = L;
    if (!mine || !mine.panel) return;
    const c = C(), el = c.el;
    if (mine.panel === "people") {
      const body = $("live-panel-people");
      body.replaceChildren();
      body.append(el("p", { class: "call-panel-note", text: `${mine.viewers || 0} watching` + (mine.maxViewers ? ` · up to ${mine.maxViewers}` : "") }));
      if (mine.role === "host") {
        const list = el("ul", { class: "call-people-list" });
        const seen = new Set();
        for (const p of mine.participants.values()) {
          if (seen.has(p.mailbox)) continue;
          seen.add(p.mailbox);
          const onStage = p.role === "speaker" || p.role === "invited";
          list.append(el("li", { class: "live-person" },
            el("span", { class: "live-person-name", text: name(p.mailbox) + (p.role === "speaker" ? " · on stage" : p.role === "invited" ? " · invited" : "") }),
            el("button", { type: "button", class: "call-pill", text: onStage ? "Remove from stage" : "Invite to stage", onclick: () => setStage(p.mailbox, !onStage) }),
            el("button", { type: "button", class: "call-pill", text: "Code", title: "Security code with this viewer", onclick: async (e) => { e.target.textContent = await viewerCode(mine, p); } })));
        }
        body.append(list.childElementCount ? list : el("p", { class: "call-panel-note", text: "Nobody is watching yet." }));
        body.append(el("p", { class: "call-panel-note", text: "People you invite onto the stage choose whether to join; their microphone and camera stay off until they do." }));
      } else if (mine.role === "speaker") {
        body.append(el("button", { type: "button", class: "call-pill", text: "Leave the stage", onclick: () => leaveStage() }));
      }
    }
    if (mine.panel === "info") {
      const body = $("live-panel-info");
      body.replaceChildren(
        el("p", { class: "call-panel-note", text: "This stream is end-to-end encrypted. The streamer’s device encrypts every frame; the media server and the call service cannot decrypt it. The key goes only to current members of this group." }),
        mine.role === "host"
          ? el("p", { class: "call-panel-note", text: "Viewers can compare a security code with you: open Viewers and stage, then Code next to their name." })
          : el("div", { class: "call-sas" },
            el("p", { class: "call-sas-row" }, el("span", { text: "Security code with " + name(mine.hostMailbox || "") }), el("strong", { text: mine.code || "…" })),
            el("p", { class: "call-sas-note", text: "If the streamer sees the same code next to your name, nobody in between can read this stream." })),
        el("p", { class: "call-panel-note", text: "Streams aren’t recorded." }));
    }
  }

  window.PigeonpostLive = {
    goLive, watch, stop, onCallsEvent, onReady, onGroupStream, onGroupMember, acceptStage, leaveStage, setStage, react,
    active: () => Boolean(L),
    current: () => (L ? { id: L.id, groupId: L.groupId, role: L.role } : null),
    show: () => { if (L) showUI(); },
    supported: () => Boolean(C() && C().canEncryptFrames),
    // For automated checks (the media-server room, to prove what a viewer's ticket allows).
    _room: () => (L && L.lk) || null,
    // For automated checks: state without media or keys.
    inspect: () => L && {
      id: L.id, role: L.role, you: L.you, hostPid: L.hostPid, epoch: L.epoch, sendEpoch: L.sendEpoch, viewers: L.viewers,
      connected: Boolean(L.connected), participants: L.participants.size, wrapped: [...L.participants.values()].filter((p) => p.wrapped).length,
      rotations: L.rotations, decryptErrors: L.decryptErrors, unencrypted: L.unencrypted, keyErrors: L.keyErrors, reactions: L.reactions,
      code: L.code, published: Object.keys(L.pubs), lastError: L.lastError || null, publicKey: L.identity && L.identity.publicKey,
    },
    stats: async () => {
      if (!L || !L.lk) return null;
      const out = [];
      for (const rp of L.lk.remoteParticipants.values()) {
        for (const pub of rp.trackPublications.values()) {
          const entry = { from: rp.identity, source: pub.source, encrypted: pub.isEncrypted, subscribed: pub.isSubscribed };
          const receiver = pub.track && pub.track.receiver;
          if (receiver) (await receiver.getStats()).forEach((r) => { if (r.type === "inbound-rtp") Object.assign(entry, { bytes: r.bytesReceived, frames: r.framesDecoded, width: r.frameWidth, height: r.frameHeight, jitterBufferDelay: r.jitterBufferDelay, jitterBufferEmitted: r.jitterBufferEmittedCount }); });
          out.push(entry);
        }
      }
      return out;
    },
  };
})();
