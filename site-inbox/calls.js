// Pigeonpost — voice and video calls, for one other person or a small group.
//
// The call service (`/calls/v1` on this origin) rings mailboxes, lets people who open a meeting link
// ask to join, and passes WebRTC setup messages between browsers. It checks members against the
// postbox's own answer to "which mailboxes does this token own"; guests can only ask to join, and
// somebody already in the call decides. Media never touches it: every pair of browsers in a call has
// its own encrypted connection (DTLS-SRTP), directly or through the TURN relay, which only forwards
// ciphertext. Chat during a call travels over those same encrypted connections and is not stored.
//
// Each person's tile can show a security code computed from both browsers' DTLS fingerprints. If two
// people read the same code, nothing between them — this service included — is in the middle.
//
// The same file runs inside the inbox (window.PigeonpostCallsHost is set by app.js) and on the
// meeting-link page (join.html), where nobody needs to be signed in.
(function () {
  "use strict";

  const BASE = "/calls/v1";
  const AUDIO_BPS = 128_000;
  // About 4 Mbit/s of media per sender in total. With one other person all of it is theirs; in a
  // group every connection carries its own copy, so the video share is divided between them.
  const SEND_BUDGET_BPS = 4_000_000;
  const MIN_VIDEO_BPS = 450_000;
  const RECONNECT_GRACE_MS = 15_000;
  const CHAT_CHARS = 2_000;
  const PAGE = document.body?.dataset.page === "join" ? "join" : "inbox";

  const supported = Boolean(window.RTCPeerConnection && navigator.mediaDevices && navigator.mediaDevices.getUserMedia);
  const canShareScreen = Boolean(navigator.mediaDevices && navigator.mediaDevices.getDisplayMedia);
  const canPickSpeaker = typeof HTMLMediaElement !== "undefined" && "setSinkId" in HTMLMediaElement.prototype;
  const store = {
    get(k) { try { return localStorage.getItem(k); } catch (_) { return null; } },
    set(k, v) { try { if (v == null) localStorage.removeItem(k); else localStorage.setItem(k, v); } catch (_) { /* private mode */ } },
  };
  const forceRelay = store.get("ppi_calls_relay") === "1";
  const host = () => window.PigeonpostCallsHost || null;

  // ---- tiny DOM helpers ------------------------------------------------------------------------

  const $ = (id) => document.getElementById(id);
  function el(tag, attrs, ...children) {
    const node = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v == null || v === false) continue;
      if (k === "class") node.className = v;
      // The `muted` attribute only mutes an element the HTML parser created. One made here would
      // play its stream out loud — for the self view that is your own microphone, which the
      // microphone then picks up again: an echo for everybody. Set the property.
      else if (k === "muted") { node.muted = true; node.defaultMuted = true; node.setAttribute("muted", ""); }
      else if (k === "text") node.textContent = v;
      else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
      else node.setAttribute(k, v === true ? "" : v);
    }
    for (const c of children.flat()) if (c != null) node.append(c);
    return node;
  }
  function svg(paths, extra = "") {
    const s = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    s.setAttribute("viewBox", "0 0 20 20");
    s.setAttribute("aria-hidden", "true");
    s.innerHTML = paths + extra; // static icon markup defined in this file only
    return s;
  }
  const ICON = {
    mic: '<rect x="7" y="2.5" width="6" height="10" rx="3" fill="none" stroke="currentColor" stroke-width="1.6"/><path d="M4.5 9.5a5.5 5.5 0 0 0 11 0M10 15v2.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>',
    cam: '<rect x="2.5" y="5" width="11" height="10" rx="2" fill="none" stroke="currentColor" stroke-width="1.6"/><path d="m13.5 9 4-2.3v6.6l-4-2.3" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round"/>',
    slash: '<path class="slash" d="M3.5 3.5l13 13" stroke="currentColor" stroke-width="1.7" stroke-linecap="round"/>',
    screen: '<rect x="2.5" y="3.5" width="15" height="10" rx="1.6" fill="none" stroke="currentColor" stroke-width="1.6"/><path d="M7 17h6M10 13.5V17M10 11V6.5M7.8 8.6 10 6.4l2.2 2.2" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/>',
    chat: '<path d="M3.5 5.5a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v6a2 2 0 0 1-2 2H9l-3.5 3v-3h0a2 2 0 0 1-2-2Z" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round"/>',
    people: '<circle cx="7.5" cy="7" r="2.6" fill="none" stroke="currentColor" stroke-width="1.6"/><path d="M2.8 16.5a4.7 4.7 0 0 1 9.4 0M13 4.7a2.6 2.6 0 0 1 0 4.8M14.5 12.3a4.7 4.7 0 0 1 2.7 4.2" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>',
    gear: '<circle cx="10" cy="10" r="2.6" fill="none" stroke="currentColor" stroke-width="1.6"/><path d="M10 2.8v2M10 15.2v2M2.8 10h2M15.2 10h2M4.9 4.9l1.4 1.4M13.7 13.7l1.4 1.4M4.9 15.1l1.4-1.4M13.7 6.3l1.4-1.4" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>',
    end: '<path d="M2.4 11.6c-.4-.5-.4-1.2.1-1.7C5 7.6 7.4 6.8 10 6.8s5 .8 7.5 3.1c.5.5.5 1.2.1 1.7l-1.2 1.4c-.4.4-1 .5-1.5.3l-2-1c-.5-.2-.8-.7-.8-1.2V9.7a8.6 8.6 0 0 0-4.2 0v1.4c0 .5-.3 1-.8 1.2l-2 1c-.5.2-1.1.1-1.5-.3Z" fill="currentColor"/>',
    phone: '<path d="M6.6 2.9 4.5 3.3c-.8.2-1.4.9-1.3 1.7.5 6.1 5.7 11.3 11.8 11.8.8.1 1.5-.5 1.7-1.3l.4-2.1c.1-.6-.2-1.2-.8-1.4l-2.4-1c-.5-.2-1.1-.1-1.5.3l-.9.9a9 9 0 0 1-4-4l.9-.9c.4-.4.5-1 .3-1.5l-1-2.4c-.2-.6-.8-.9-1.4-.8Z" fill="currentColor"/>',
    lock: '<rect x="4" y="9" width="12" height="8.5" rx="2" fill="none" stroke="currentColor" stroke-width="1.5"/><path d="M6.8 9V6.5a3.2 3.2 0 0 1 6.4 0V9" fill="none" stroke="currentColor" stroke-width="1.5"/>',
    close: '<path d="M5 5l10 10M15 5 5 15" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>',
    send: '<path d="M3.5 10 16.5 3.8 13 16.2 9.4 11.1Z M9.4 11.1 16.5 3.8" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/>',
    link: '<path d="M8.5 11.5a3 3 0 0 0 4.2 0l2.6-2.6a3 3 0 0 0-4.2-4.2l-1 1M11.5 8.5a3 3 0 0 0-4.2 0l-2.6 2.6a3 3 0 0 0 4.2 4.2l1-1" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>',
  };

  // Faces: the same two letters and tone the inbox uses, so a person looks the same in both places.
  function initials(name) {
    const base = String(name || "").split("/").filter(Boolean).pop() || String(name || "");
    const letters = base.replace(/[^\p{L}\p{N}]/gu, "");
    return (letters.slice(0, 2) || "··").toUpperCase();
  }
  function faceTone(name) {
    let h = 0;
    const s = String(name || "");
    for (let i = 0; i < s.length; i++) h = (h * 31 + s.charCodeAt(i)) >>> 0;
    return String((h % 6) + 1);
  }
  function paintFace(node, name) {
    node.textContent = initials(name);
    node.dataset.tone = faceTone(name);
  }
  function displayName(m) {
    if (!m) return "Guest";
    if (typeof m === "string") return host()?.displayName ? host().displayName(m) : m;
    if (m.mailbox) return displayName(m.mailbox.handle || m.mailbox.address);
    return m.name || "Guest";
  }

  function toast(message) {
    if (host()?.toast) return host().toast(message);
    const t = $("call-toast");
    if (!t) return;
    t.textContent = message;
    t.hidden = false;
    clearTimeout(toast.timer);
    toast.timer = setTimeout(() => { t.hidden = true; }, 5200);
  }

  // ---- state -----------------------------------------------------------------------------------

  let streamWanted = false, streamGen = 0, streamAbort = null;
  let session = null, myMailboxes = [], iceServers = null;
  let ring = null; // an incoming ring not yet answered
  let room = null; // the call on screen
  let knockState = null; // join page: { code, knock, state }
  const peers = new Map(); // participant id -> peer
  const knocks = new Map(); // knock id -> request waiting inside a call
  const chatLog = [];
  const local = { stream: null, mic: null, cam: null, screen: null, screenStream: null, muted: false, cameraOff: false };
  const prefs = {
    get mic() { return store.get("ppi_call_mic"); }, set mic(v) { store.set("ppi_call_mic", v); },
    get cam() { return store.get("ppi_call_cam"); }, set cam(v) { store.set("ppi_call_cam", v); },
    get speaker() { return store.get("ppi_call_speaker"); }, set speaker(v) { store.set("ppi_call_speaker", v); },
  };

  // ---- auth ------------------------------------------------------------------------------------

  // A member uses the inbox's own token. On the meeting-link page a signed-in member may still have
  // one; anybody else gets an anonymous guest token that is only good for asking to join.
  function memberToken() {
    if (host()) return host().signedIn() ? host().token() : null;
    return store.get("ppi_token");
  }
  let guestToken = null;
  function authHeader() {
    const t = memberToken();
    if (t && !guestToken) return "Bearer " + t;
    return guestToken ? "Guest " + guestToken : null;
  }
  async function ensureGuest() {
    if (guestToken) return;
    try { guestToken = sessionStorage.getItem("ppi_guest"); } catch (_) { guestToken = null; }
    if (guestToken) return;
    const res = await fetch(BASE + "/guests", { method: "POST", headers: { "content-type": "application/json" }, body: "{}" });
    const body = await res.json().catch(() => ({}));
    if (!res.ok || !body.guest_token) throw Object.assign(new Error(body.error || "guest"), { code: body.error });
    guestToken = body.guest_token;
    try { sessionStorage.setItem("ppi_guest", guestToken); } catch (_) { /* fine */ }
  }

  async function request(method, path, body, renewed) {
    const auth = authHeader();
    const res = await fetch(BASE + path, {
      method,
      headers: Object.assign(auth ? { authorization: auth } : {}, body ? { "content-type": "application/json" } : {}),
      body: body ? JSON.stringify(body) : undefined,
    });
    if (res.status === 401 && !renewed && host() && (await host().renew())) return request(method, path, body, true);
    const value = await res.json().catch(() => ({}));
    if (!res.ok) throw Object.assign(new Error(value.error || "http_" + res.status), { status: res.status, code: value.error });
    return value;
  }
  const post = (path, body) => request("POST", path, body);

  // ---- the event stream ------------------------------------------------------------------------

  function start() {
    if (!supported || streamWanted || !authHeader()) return;
    streamWanted = true;
    runStream(++streamGen);
  }

  function stop() {
    streamWanted = false;
    streamGen += 1;
    if (streamAbort) streamAbort.abort();
    streamAbort = null;
    session = null;
    dismissRing();
    if (room) endRoom(null, { notify: true });
    renderHead();
  }

  async function runStream(gen) {
    let backoff = 1000, renewed = false;
    while (streamWanted && gen === streamGen) {
      streamAbort = new AbortController();
      try {
        const auth = authHeader();
        if (!auth) { streamWanted = false; return; }
        const res = await fetch(BASE + "/events", { headers: { authorization: auth, accept: "text/event-stream" }, signal: streamAbort.signal });
        if (res.status === 401) {
          if (!renewed && host() && (await host().renew())) { renewed = true; continue; }
          // A stale member token on the meeting page: carry on as a guest instead.
          if (!host() && !guestToken) { await ensureGuest(); continue; }
          streamWanted = false;
          onStreamClosed("unauthorized");
          return;
        }
        if (res.status === 403 && !host() && !guestToken) { await ensureGuest(); continue; }
        if (res.status === 404 || res.status === 405) { streamWanted = false; renderHead(); return; }
        if (!res.ok || !res.body) throw new Error("stream " + res.status);
        renewed = false;
        backoff = 1000;
        const reader = res.body.getReader();
        const decoder = new TextDecoder();
        let buffer = "";
        for (;;) {
          const { value, done } = await reader.read();
          if (done) break;
          buffer += decoder.decode(value, { stream: true });
          let cut;
          while ((cut = buffer.indexOf("\n\n")) >= 0) {
            const frame = buffer.slice(0, cut);
            buffer = buffer.slice(cut + 2);
            const name = /^event: (.+)$/m.exec(frame), data = /^data: (.+)$/m.exec(frame);
            if (name && data) {
              try { onEvent(name[1], JSON.parse(data[1])); } catch (err) { console.warn("calls: event failed", err && err.name); }
            }
          }
        }
      } catch (_) {
        if (!streamWanted || gen !== streamGen) return;
      }
      session = null;
      renderHead();
      if (!streamWanted || gen !== streamGen) return;
      await new Promise((r) => setTimeout(r, backoff));
      backoff = Math.min(backoff * 2, 30_000);
    }
  }

  const readyWaiters = [];
  function whenReady() {
    if (session) return Promise.resolve(session);
    return new Promise((resolve) => readyWaiters.push(resolve));
  }

  function onEvent(name, data) {
    switch (name) {
      case "ready":
        session = data.session;
        myMailboxes = data.mailboxes || [];
        renderHead();
        while (readyWaiters.length) readyWaiters.shift()(session);
        if (room && room.you) post(`/rooms/${room.id}/resume`, { session, participant: room.you }).catch(() => {});
        if (PAGE === "join") Join.onReady();
        return;
      case "incoming": return onIncoming(data);
      case "ring_ended":
        if (ring && ring.room === data.room) {
          const missed = ring;
          dismissRing();
          if (data.reason !== "answered_elsewhere") toast("Missed " + kindWord(missed.kind) + " call from " + displayName(missed.from.handle || missed.from.address) + ".");
        } else if (room && room.id === data.room && peers.size === 0) {
          endRoom(endedMessage(data.reason), { notify: false });
        }
        return;
      case "joined": return onJoined(data);
      case "participant_joined":
        if (room && room.id === data.room) {
          room.state = "active";
          stopTone();
          // The newcomer offers; this end prepares to answer.
          ensurePeer(data, false);
          renderStage();
        }
        return;
      case "participant_left":
        if (room && room.id === data.room) {
          const p = peers.get(data.participant);
          if (p) {
            closePeer(p);
            if (room.size > 2) toast(displayName(p) + " left the call.");
          }
          renderStage();
        }
        return;
      case "signal":
        if (room && room.id === data.room) onSignal(data.from, data.type, data.data).catch((err) => console.warn("calls: signal", err && err.name));
        return;
      case "knock":
        if (room && room.id === data.room) { knocks.set(data.knock, data); renderKnocks(); startTone("knock", 1); }
        return;
      case "knock_resolved":
        if (room && room.id === data.room) {
          const k = knocks.get(data.knock);
          knocks.delete(data.knock);
          renderKnocks();
          if (k && data.admitted && data.by && data.by !== room.you) toast(displayName(k) + " was let in.");
        }
        return;
      case "knock_result":
        if (PAGE === "join") Join.onResult(data);
        return;
      case "ended":
        if (room && room.id === data.room) endRoom(endedMessage(data.reason), { notify: false });
        return;
      default:
    }
  }

  function onStreamClosed() {
    if (PAGE === "join") Join.fail("Your session ended. Reload the page to try again.");
  }

  // ---- placing, ringing, answering ---------------------------------------------------------------

  async function place(kind) {
    const h = host();
    const me = h && h.mailbox(), peer = h && h.peer();
    if (!me || !peer || !supported) return;
    if (room || ring) { toast("Finish the current call first."); return; }
    if (!session) { toast("Calling isn’t ready yet. Try again in a moment."); start(); return; }
    openRoomUI({ title: displayName(peer), sub: peer, face: peer, status: "Starting " + kindWord(kind) + "…" });
    room = newRoom({ kind, title: displayName(peer), peerKey: peer });
    try {
      await startLocalMedia(kind === "video");
    } catch (err) {
      endRoom(mediaMessage(err, kind), { notify: false });
      return;
    }
    if (!room) return;
    try {
      const placed = await post("/calls", { session, from: me.address, to: peer, kind });
      if (!room) { post(`/rooms/${placed.room}/leave`, { session }).catch(() => {}); return; }
      Object.assign(room, { id: placed.room, code: placed.code, you: placed.you, state: "ringing" });
      setStatus("Ringing…");
      startTone("ringback");
      renderStage();
    } catch (err) {
      endRoom(placeError(err), { notify: false });
    }
  }

  function onIncoming(data) {
    if (room || ring) {
      post(`/rooms/${data.room}/decline`, { session, reason: "busy" }).catch(() => {});
      return;
    }
    ring = data;
    const from = data.from || {};
    const key = from.handle || from.address;
    const h = host();
    const known = h && (h.knownPeer(from.handle) || h.knownPeer(from.address));
    $("call-ring-title").textContent = "Incoming " + kindWord(data.kind) + " call";
    $("call-ring-from").textContent = key || "Someone";
    $("call-ring-note").textContent = "To " + (data.to || "") + (known ? "" : " · not in your contacts");
    paintFace($("call-ring-face"), key);
    $("call-ring").hidden = false;
    $("call-ring-accept").focus({ preventScroll: true });
    startTone("ring");
    if (document.hidden && "Notification" in window && Notification.permission === "granted") {
      try {
        const n = new Notification("Incoming " + kindWord(data.kind) + " call", { body: key, tag: "pp-call-" + data.room });
        n.onclick = () => { window.focus(); n.close(); };
        ring.notification = n;
      } catch (_) { /* a courtesy */ }
    }
  }

  function dismissRing() {
    if (!ring) return;
    try { ring.notification?.close(); } catch (_) { /* gone */ }
    ring = null;
    $("call-ring").hidden = true;
    stopTone();
  }

  async function acceptRing() {
    const r = ring;
    if (!r) return;
    dismissRing();
    const peerKey = r.from.handle || r.from.address;
    openRoomUI({ title: displayName(peerKey), sub: peerKey, face: peerKey, status: "Connecting…" });
    room = newRoom({ kind: r.kind, title: displayName(peerKey), peerKey, id: r.room, code: r.code });
    try {
      await startLocalMedia(r.kind === "video");
    } catch (err) {
      post(`/rooms/${r.room}/decline`, { session }).catch(() => {});
      endRoom(mediaMessage(err, r.kind), { notify: false });
      return;
    }
    if (!room) return;
    try {
      await post(`/rooms/${r.room}/accept`, { session });
    } catch (err) {
      endRoom(err.code === "not_ringing" ? "That call has already ended." : "The call could not be answered.", { notify: false });
    }
  }

  function declineRing() {
    const r = ring;
    if (!r) return;
    dismissRing();
    post(`/rooms/${r.room}/decline`, { session }).catch(() => {});
  }

  function newRoom(fields) {
    return Object.assign({ id: null, code: null, you: null, kind: "audio", state: "starting", startedAt: 0, timer: null, statsTimer: null, size: 1 }, fields);
  }

  // The call service says who is here: this end has just been let in (by answering a ring, or by
  // somebody admitting a knock). The newcomer opens a connection to everyone already present.
  function onJoined(data) {
    if (!room) {
      if (PAGE !== "join") return;
      room = newRoom({ kind: data.kind, title: "Call", id: data.room });
    }
    Object.assign(room, { id: data.room, code: data.code, you: data.you, kind: data.kind || room.kind, state: "active" });
    stopTone();
    if (PAGE === "join") Join.enter();
    for (const p of data.participants || []) {
      if (p.participant === data.you) continue;
      ensurePeer(p, true);
    }
    renderStage();
  }

  // ---- local media -----------------------------------------------------------------------------

  // A chosen device is asked for *exactly*. As a mere preference it competes with the resolution
  // preference, and a 720p webcam loses to a 1080p built-in camera — the choice silently ignored.
  const AUDIO_CONSTRAINTS = (id) => Object.assign({
    echoCancellation: true, noiseSuppression: true, autoGainControl: true,
    channelCount: { ideal: 1 }, sampleRate: { ideal: 48_000 },
  }, id ? { deviceId: { exact: id } } : {});
  const VIDEO_CONSTRAINTS = (id) => Object.assign({
    width: { ideal: 1920 }, height: { ideal: 1080 }, frameRate: { ideal: 30, max: 30 },
  }, id ? { deviceId: { exact: id } } : { facingMode: "user" });

  // The saved choice, if that device is still plugged in.
  async function chosen(kind) {
    const id = kind === "mic" ? prefs.mic : prefs.cam;
    if (!id) return null;
    try {
      const list = await navigator.mediaDevices.enumerateDevices();
      return list.some((d) => d.deviceId === id) ? id : null;
    } catch (_) { return null; }
  }

  // getUserMedia with the chosen devices, falling back to the defaults if a chosen one has gone.
  async function capture({ audio, video }) {
    const [micId, camId] = await Promise.all([audio ? chosen("mic") : null, video ? chosen("cam") : null]);
    const ask = (m, c) => navigator.mediaDevices.getUserMedia({ audio: audio ? AUDIO_CONSTRAINTS(m) : false, video: video ? VIDEO_CONSTRAINTS(c) : false });
    try { return await ask(micId, camId); }
    catch (err) {
      if ((err.name === "OverconstrainedError" || err.name === "NotFoundError") && (micId || camId)) return ask(null, null);
      throw err;
    }
  }

  async function startLocalMedia(withVideo) {
    let media;
    try {
      media = await capture({ audio: true, video: withVideo });
    } catch (err) {
      // No camera, or the camera refused: still a call, just a voice one. A refused microphone ends
      // it — nobody can hear you.
      if (!withVideo) throw err;
      media = await capture({ audio: true, video: false });
      toast("No camera available — joining with audio only.");
    }
    local.stream = new MediaStream();
    local.mic = media.getAudioTracks()[0] || null;
    local.cam = media.getVideoTracks()[0] || null;
    if (local.mic) { local.mic.contentHint = "speech"; local.stream.addTrack(local.mic); }
    if (local.cam) local.stream.addTrack(local.cam);
    local.muted = false;
    local.cameraOff = false;
    showSelf();
    renderControls();
  }

  function stopLocalMedia() {
    for (const t of [local.mic, local.cam, local.screen]) t?.stop();
    Object.assign(local, { stream: null, mic: null, cam: null, screen: null, screenStream: null, muted: false, cameraOff: false });
  }

  // Turning the camera on in a voice call makes it a video call: the track is added to every
  // connection and each one renegotiates.
  async function toggleCamera() {
    if (!room) return;
    if (!local.cam) {
      try {
        const media = await capture({ audio: false, video: true });
        local.cam = media.getVideoTracks()[0];
        local.stream.addTrack(local.cam);
        local.cameraOff = false;
        for (const p of peers.values()) addLocalTrack(p, local.cam, local.stream);
        room.kind = "video";
        broadcastState();
      } catch (err) {
        toast(mediaMessage(err, "video"));
      }
    } else {
      local.cameraOff = !local.cameraOff;
      local.cam.enabled = !local.cameraOff;
      broadcastState();
    }
    showSelf();
    renderControls();
    renderStage();
  }

  function toggleMute() {
    if (!local.mic) return;
    local.muted = !local.muted;
    local.mic.enabled = !local.muted;
    broadcastState();
    renderControls();
  }

  async function toggleScreen() {
    if (!room) return;
    if (local.screen) { stopScreen(); return; }
    try {
      const media = await navigator.mediaDevices.getDisplayMedia({ video: { frameRate: { ideal: 15, max: 30 } }, audio: false });
      const track = media.getVideoTracks()[0];
      if (!track) return;
      track.contentHint = "detail";
      local.screen = track;
      local.screenStream = new MediaStream([track]);
      track.addEventListener("ended", () => { if (local.screen === track) stopScreen(); });
      for (const p of peers.values()) addLocalTrack(p, track, local.screenStream);
      broadcastState();
      applyBitrates();
    } catch (err) {
      if (err && err.name !== "NotAllowedError") toast("Screen sharing couldn’t start.");
    }
    renderControls();
    renderStage();
  }

  function stopScreen() {
    const track = local.screen;
    if (!track) return;
    for (const p of peers.values()) {
      const sender = p.pc.getSenders().find((s) => s.track === track);
      if (sender) { try { p.pc.removeTrack(sender); } catch (_) { /* closed */ } }
    }
    track.stop();
    local.screen = null;
    local.screenStream = null;
    broadcastState();
    applyBitrates();
    renderControls();
    renderStage();
  }

  async function switchDevice(kind, deviceId) {
    if (kind === "speaker") {
      prefs.speaker = deviceId || null;
      for (const v of document.querySelectorAll(".call-ui video, .call-ui audio")) {
        if (v.muted) continue;
        v.setSinkId?.(deviceId || "").catch(() => {});
      }
      return;
    }
    if (kind === "mic") prefs.mic = deviceId || null;
    if (kind === "cam") prefs.cam = deviceId || null;
    if (!local.stream) return;
    const old = kind === "mic" ? local.mic : local.cam;
    if (!old) return;
    const open = (id) => navigator.mediaDevices.getUserMedia(kind === "mic" ? { audio: AUDIO_CONSTRAINTS(id) } : { video: VIDEO_CONSTRAINTS(id) });
    let media;
    // Exactly the device picked. Release the old camera first: many cameras will not open twice,
    // and asking for a second while the first is held is what quietly lands back on the built-in.
    if (kind === "cam") old.stop();
    try {
      media = await open(deviceId || null);
    } catch (err) {
      toast(mediaMessage(err, kind === "mic" ? "audio" : "video"));
      if (kind !== "cam") return;
      try { media = await open(null); } catch (_) { return; }
    }
    const fresh = (kind === "mic" ? media.getAudioTracks() : media.getVideoTracks())[0];
    if (!fresh) return;
    if (kind === "mic") { fresh.contentHint = "speech"; fresh.enabled = !local.muted; } else fresh.enabled = !local.cameraOff;
    for (const p of peers.values()) {
      const sender = p.pc && p.pc.getSenders().find((x) => x.track === old);
      if (sender) await sender.replaceTrack(fresh).catch(() => {});
    }
    local.stream.removeTrack(old);
    local.stream.addTrack(fresh);
    old.stop();
    if (kind === "mic") local.mic = fresh; else local.cam = fresh;
    showSelf();
    fillDevices();
  }

  // ---- connections to the others ---------------------------------------------------------------

  async function turnServers() {
    if (iceServers) return iceServers;
    try { iceServers = (await request("GET", "/turn?session=" + encodeURIComponent(session))).iceServers || []; }
    catch (_) { iceServers = []; }
    return iceServers;
  }

  // Perfect negotiation (W3C WebRTC §10.7): both ends may start a renegotiation — adding a camera,
  // starting a screen share — and collisions are settled by politeness, decided by participant id.
  function ensurePeer(info, initiator) {
    let p = peers.get(info.participant);
    if (p) return p;
    p = {
      id: info.participant, name: info.name, mailbox: info.mailbox || null, guest: Boolean(info.guest),
      pc: null, dc: null, polite: String(room.you) < String(info.participant), makingOffer: false, ignoreOffer: false,
      pendingIce: [], remote: { camera: new MediaStream(), screen: new MediaStream(), cameraId: null, screenId: null, streams: new Map() },
      state: { muted: false, cameraOff: false }, security: null, connected: false, dropTimer: null, tile: null, ready: null,
      initiator, restarts: 0, watchdog: null,
    };
    peers.set(p.id, p);
    room.size = peers.size + 1;
    // Only the newcomer starts: everybody already in the call builds their side when the newcomer's
    // offer arrives. Two ends both offering at once on a slow link is the collision that otherwise
    // leaves a pair stuck half-negotiated.
    if (initiator) p.ready = createConnection(p, true);
    applyBitrates();
    renderStage();
    return p;
  }

  function createConnection(p, initiator) {
    return (async () => {
      const pc = new RTCPeerConnection({
        iceServers: await turnServers(),
        iceTransportPolicy: forceRelay ? "relay" : "all",
        bundlePolicy: "max-bundle",
        rtcpMuxPolicy: "require",
      });
      p.pc = pc;
      // A negotiated channel exists on both ends without either having to announce it.
      p.dc = pc.createDataChannel("pigeonpost", { negotiated: true, id: 0, ordered: true });
      p.dc.onopen = () => sendState(p);
      p.dc.onmessage = (e) => onPeerMessage(p, e.data);
      pc.onicecandidate = (e) => sendSignal(p, "ice", e.candidate ? e.candidate.toJSON() : null);
      pc.ontrack = (e) => onRemoteTrack(p, e);
      pc.onconnectionstatechange = () => onConnectionState(p);
      pc.onnegotiationneeded = () => negotiate(p).catch(() => {});
      if (initiator) {
        for (const t of localTracks()) addLocalTrack(p, t.track, t.stream);
        // A voice-only newcomer still wants to see video from anybody who has it.
        if (!local.cam) pc.addTransceiver("video", { direction: "recvonly" });
      }
      return pc;
    })();
  }

  function localTracks() {
    const out = [];
    if (local.mic) out.push({ track: local.mic, stream: local.stream });
    if (local.cam) out.push({ track: local.cam, stream: local.stream });
    if (local.screen) out.push({ track: local.screen, stream: local.screenStream });
    return out;
  }

  function addLocalTrack(p, track, stream) {
    if (!p.pc || p.pc.getSenders().some((s) => s.track === track)) return;
    // Reuse a transceiver the other side offered (an unused receive-only slot of the same kind)
    // before adding a new one, so a voice call turning into video needs no extra m-line.
    const spare = p.pc.getTransceivers().find((t) => !t.stopped && t.receiver.track.kind === track.kind && !t.sender.track && t.currentDirection !== "stopped" && !p.usedTransceivers?.has(t));
    if (spare && track !== local.screen) {
      (p.usedTransceivers ||= new Set()).add(spare);
      spare.sender.replaceTrack(track);
      spare.sender.setStreams?.(stream);
      spare.direction = "sendrecv";
      preferCodecs(spare, track.kind);
    } else {
      const sender = p.pc.addTrack(track, stream);
      const transceiver = p.pc.getTransceivers().find((t) => t.sender === sender);
      if (transceiver) { (p.usedTransceivers ||= new Set()).add(transceiver); preferCodecs(transceiver, track.kind); }
    }
  }

  async function negotiate(p) {
    const pc = p.pc;
    if (!pc || pc.signalingState === "closed") return;
    try {
      p.makingOffer = true;
      const offer = await pc.createOffer();
      if (pc.signalingState !== "stable") return;
      offer.sdp = tuneOpus(offer.sdp);
      await pc.setLocalDescription(offer);
      await sendSignal(p, "offer", { type: "offer", sdp: pc.localDescription.sdp });
      armWatchdog(p);
    } finally {
      p.makingOffer = false;
    }
    applyBitrates();
  }

  async function onSignal(fromId, type, data) {
    if (!room) return;
    const p = peers.get(fromId) || ensurePeer({ participant: fromId, name: "Someone" }, false);
    if (!p.ready) p.ready = createConnection(p, false);
    const pc = await p.ready;
    if (type === "ice") {
      if (!pc.remoteDescription) { p.pendingIce.push(data); return; }
      try { await pc.addIceCandidate(data || null); } catch (err) { if (!p.ignoreOffer) console.warn("calls: ice", err && err.name); }
      return;
    }
    const collision = type === "offer" && (p.makingOffer || pc.signalingState !== "stable");
    p.ignoreOffer = !p.polite && collision;
    if (p.ignoreOffer) return;
    await pc.setRemoteDescription(data);
    for (const c of p.pendingIce.splice(0)) { try { await pc.addIceCandidate(c || null); } catch (_) { /* stale */ } }
    if (type === "offer") {
      // Answering: put this end's media on the slots the offer opened.
      for (const t of localTracks()) addLocalTrack(p, t.track, t.stream);
      const answer = await pc.createAnswer();
      answer.sdp = tuneOpus(answer.sdp);
      await pc.setLocalDescription(answer);
      await sendSignal(p, "answer", { type: "answer", sdp: pc.localDescription.sdp });
      armWatchdog(p);
      applyBitrates();
    }
  }

  function sendSignal(p, type, data) {
    if (!room || !room.id || !session) return Promise.resolve();
    return post(`/rooms/${room.id}/signal`, { session, to: p.id, type, data }).catch(() => {});
  }

  function onRemoteTrack(p, e) {
    const stream = e.streams[0];
    if (stream) p.remote.streams.set(stream.id, stream);
    placeRemoteTrack(p, e.track, stream ? stream.id : null);
    e.track.addEventListener("ended", () => renderStage());
    e.track.addEventListener("mute", () => renderStage());
    e.track.addEventListener("unmute", () => renderStage());
    renderStage();
  }

  // Which of a person's streams is their camera and which their screen is told over the data
  // channel; until then everything is treated as camera.
  function placeRemoteTrack(p, track, streamId) {
    const isScreen = streamId && p.remote.screenId === streamId;
    const target = isScreen ? p.remote.screen : p.remote.camera;
    const other = isScreen ? p.remote.camera : p.remote.screen;
    if (other.getTracks().includes(track)) other.removeTrack(track);
    if (!target.getTracks().includes(track)) target.addTrack(track);
  }

  // A pair that has not connected after a while gets fresh ICE candidates (and fresh relay
  // allocations) rather than sitting on "connecting" for ever. Only the side that started the
  // connection restarts, so the two never both renegotiate at once.
  function armWatchdog(p) {
    if (p.watchdog || p.connected) return;
    const check = () => {
      p.watchdog = null;
      if (!peers.has(p.id) || p.connected || !p.pc || p.pc.signalingState === "closed") return;
      if (p.initiator && p.restarts < 3) { p.restarts += 1; p.pc.restartIce?.(); }
      p.watchdog = setTimeout(check, 12_000);
    };
    p.watchdog = setTimeout(check, 12_000);
  }

  function onConnectionState(p) {
    if (!p.pc) return;
    const s = p.pc.connectionState;
    if (s === "connected") {
      clearTimeout(p.dropTimer);
      clearTimeout(p.watchdog);
      p.dropTimer = null;
      p.watchdog = null;
      if (!p.connected) {
        p.connected = true;
        if (!room.startedAt) {
          room.startedAt = Date.now();
          room.timer = setInterval(renderClock, 1000);
          room.statsTimer = setInterval(sampleStats, 2000);
        }
        verifySecurity(p).catch(() => {});
      }
      renderClock();
      renderStage();
      return;
    }
    if (s === "disconnected" || s === "failed") {
      if (p.initiator && p.restarts < 3 && (s === "failed" || !p.connected)) { p.restarts += 1; p.pc.restartIce?.(); }
      if (!p.dropTimer) p.dropTimer = setTimeout(() => {
        // Gone for good: drop this connection. The call service decides whether the call goes on.
        if (p.pc && p.pc.connectionState !== "connected") { closePeer(p); renderStage(); if (!peers.size && room?.state === "active") leave(); }
      }, RECONNECT_GRACE_MS);
      renderStage();
    }
  }

  function closePeer(p) {
    clearTimeout(p.dropTimer);
    clearTimeout(p.watchdog);
    try { p.dc?.close(); } catch (_) { /* closed */ }
    try { p.pc?.close(); } catch (_) { /* closed */ }
    for (const t of [...p.remote.camera.getTracks(), ...p.remote.screen.getTracks()]) t.stop();
    p.tile?.remove();
    peers.delete(p.id);
    if (room) room.size = peers.size + 1;
    applyBitrates();
  }

  // ---- what travels on the data channel ---------------------------------------------------------

  function sendState(p) {
    if (!p.dc || p.dc.readyState !== "open") return;
    p.dc.send(JSON.stringify({
      t: "state", muted: local.muted, cameraOff: !local.cam || local.cameraOff,
      camera: local.stream ? local.stream.id : null, screen: local.screenStream ? local.screenStream.id : null,
    }));
  }
  function broadcastState() { for (const p of peers.values()) sendState(p); }

  function onPeerMessage(p, raw) {
    if (typeof raw !== "string" || raw.length > 8_000) return;
    let m;
    try { m = JSON.parse(raw); } catch (_) { return; }
    if (m.t === "state") {
      p.state = { muted: Boolean(m.muted), cameraOff: Boolean(m.cameraOff) };
      p.remote.cameraId = typeof m.camera === "string" ? m.camera : null;
      p.remote.screenId = typeof m.screen === "string" ? m.screen : null;
      // Re-file tracks now that it is known which stream is which.
      for (const [id, stream] of p.remote.streams) for (const track of stream.getTracks()) placeRemoteTrack(p, track, id);
      renderStage();
    } else if (m.t === "chat" && typeof m.text === "string") {
      const text = m.text.slice(0, CHAT_CHARS);
      if (!text.trim()) return;
      chatLog.push({ who: displayName(p), mine: false, text, at: Date.now() });
      renderChat();
      if ($("call-panel").dataset.view !== "chat" || $("call-panel").hidden) {
        room.unread = (room.unread || 0) + 1;
        renderControls();
      }
    }
  }

  function sendChat(text) {
    const value = String(text || "").trim().slice(0, CHAT_CHARS);
    if (!value || !room) return false;
    let delivered = 0;
    for (const p of peers.values()) {
      if (p.dc && p.dc.readyState === "open") { p.dc.send(JSON.stringify({ t: "chat", text: value })); delivered++; }
    }
    if (!delivered) { toast("Nobody else is connected yet."); return false; }
    chatLog.push({ who: "You", mine: true, text: value, at: Date.now() });
    renderChat();
    return true;
  }

  // ---- encoding --------------------------------------------------------------------------------

  function preferCodecs(transceiver, kind) {
    if (!transceiver.setCodecPreferences || !window.RTCRtpReceiver?.getCapabilities) return;
    const caps = RTCRtpReceiver.getCapabilities(kind);
    if (!caps) return;
    const rank = (c) => {
      const t = c.mimeType.toLowerCase();
      if (kind === "audio") return t === "audio/opus" ? 0 : 5;
      // H.264 first: hardware encoders on Macs, iPhones and most PCs make 1080p sustainable.
      if (t === "video/h264") return /profile-level-id=64/.test(c.sdpFmtpLine || "") ? 0 : 1;
      if (t === "video/vp9") return 2;
      if (t === "video/vp8") return 3;
      if (t === "video/av1") return 4;
      return 6;
    };
    try { transceiver.setCodecPreferences(caps.codecs.slice().sort((a, b) => rank(a) - rank(b))); } catch (_) { /* keep defaults */ }
  }

  // Opus as this end wants to receive it: full band, 128 kbit/s ceiling, in-band error correction,
  // no discontinuous transmission (its silence gaps clip soft speech).
  function tuneOpus(sdp) {
    const pt = (/a=rtpmap:(\d+) opus\/48000\/2/i.exec(sdp) || [])[1];
    if (!pt) return sdp;
    const wanted = { minptime: "10", useinbandfec: "1", usedtx: "0", stereo: "0", "sprop-stereo": "0", maxplaybackrate: "48000", maxaveragebitrate: String(AUDIO_BPS) };
    const line = new RegExp(`a=fmtp:${pt} ([^\\r\\n]*)`);
    const found = line.exec(sdp);
    const params = new Map();
    if (found) for (const kv of found[1].split(";")) { const [k, v] = kv.split("="); if (k) params.set(k.trim(), (v || "").trim()); }
    for (const [k, v] of Object.entries(wanted)) params.set(k, v);
    const text = [...params].map(([k, v]) => `${k}=${v}`).join(";");
    return found ? sdp.replace(line, `a=fmtp:${pt} ${text}`) : sdp.replace(new RegExp(`(a=rtpmap:${pt} [^\\r\\n]*\\r?\\n)`), `$1a=fmtp:${pt} ${text}\r\n`);
  }

  // Divide the upload budget: every other person gets their own copy of this end's video.
  function applyBitrates() {
    const others = Math.max(1, peers.size);
    const videoBudget = (SEND_BUDGET_BPS - AUDIO_BPS * others) / others;
    const camera = Math.max(MIN_VIDEO_BPS, Math.floor(local.screen ? videoBudget * 0.35 : videoBudget));
    const screen = Math.max(MIN_VIDEO_BPS, Math.floor(videoBudget * 0.65));
    for (const p of peers.values()) {
      if (!p.pc) continue;
      for (const sender of p.pc.getSenders()) {
        if (!sender.track) continue;
        const params = sender.getParameters();
        if (!params.encodings || !params.encodings.length) continue;
        const enc = params.encodings[0];
        if (sender.track === local.screen) {
          enc.maxBitrate = screen; enc.maxFramerate = 15; params.degradationPreference = "maintain-resolution";
        } else if (sender.track.kind === "video") {
          enc.maxBitrate = camera; enc.maxFramerate = 30; params.degradationPreference = "balanced";
        } else {
          enc.maxBitrate = AUDIO_BPS;
        }
        enc.priority = sender.track.kind === "audio" ? "high" : "medium";
        enc.networkPriority = enc.priority;
        sender.setParameters(params).catch(() => {});
      }
    }
  }

  // ---- security codes --------------------------------------------------------------------------

  async function verifySecurity(p) {
    const pc = p.pc;
    const fingerprint = (sdp) => ((/a=fingerprint:\S+ ([0-9A-F:]+)/i.exec(sdp || "") || [])[1] || "").toUpperCase();
    const mine = fingerprint(pc.localDescription?.sdp), theirs = fingerprint(pc.remoteDescription?.sdp);
    if (!mine || !theirs) return;
    // The certificate the other browser actually used must be the one its description named.
    const report = await pc.getStats();
    let presented = null;
    report.forEach((s) => { if (s.type === "transport" && s.remoteCertificateId) presented = report.get(s.remoteCertificateId)?.fingerprint || null; });
    if (presented && presented.toUpperCase() !== theirs) {
      toast("Disconnected from " + displayName(p) + ": their encryption certificate did not match.");
      closePeer(p);
      renderStage();
      return;
    }
    const [a, b] = [mine, theirs].sort();
    const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode("pigeonpost-call-sas-v1\n" + a + "\n" + b));
    const code = String(new DataView(digest).getUint32(0) % 1_000_000).padStart(6, "0");
    p.security = { code: code.slice(0, 3) + " " + code.slice(3), checked: presented ? true : null };
    renderStage();
    renderPeople();
  }

  // ---- stats -----------------------------------------------------------------------------------

  async function sampleStats() {
    for (const p of peers.values()) {
      if (!p.pc) continue;
      const report = await p.pc.getStats();
      const prev = p.stats || {};
      const out = {}, inb = {};
      let pair = null;
      report.forEach((s) => {
        if (s.type === "transport" && s.selectedCandidatePairId) pair = report.get(s.selectedCandidatePairId);
        if (s.type === "outbound-rtp" && s.kind && !(s.kind in out && (out[s.kind].bytesSent || 0) > (s.bytesSent || 0))) out[s.kind] = s;
        if (s.type === "inbound-rtp" && s.kind && !(s.kind in inb && (inb[s.kind].bytesReceived || 0) > (s.bytesReceived || 0))) inb[s.kind] = s;
      });
      const localCand = pair && report.get(pair.localCandidateId);
      const rate = (now, before) => before && now && now.timestamp > before.timestamp
        ? Math.round(((now.bytesSent ?? now.bytesReceived) - (before.bytesSent ?? before.bytesReceived)) * 8 / ((now.timestamp - before.timestamp) / 1000)) : null;
      p.stats = {
        raw: { out, inb },
        route: localCand ? (localCand.candidateType === "relay" ? "relay" : "direct") : null,
        rtt: pair ? pair.currentRoundTripTime : null,
        sending: { video: out.video ? { width: out.video.frameWidth, height: out.video.frameHeight, fps: out.video.framesPerSecond, bps: rate(out.video, prev.raw?.out.video), limitation: out.video.qualityLimitationReason } : null, audio: out.audio ? { bps: rate(out.audio, prev.raw?.out.audio) } : null },
        receiving: { video: inb.video ? { width: inb.video.frameWidth, height: inb.video.frameHeight, fps: inb.video.framesPerSecond, bps: rate(inb.video, prev.raw?.inb.video), decoded: inb.video.framesDecoded } : null, audio: inb.audio ? { bps: rate(inb.audio, prev.raw?.inb.audio), level: inb.audio.audioLevel, concealed: inb.audio.concealedSamples, received: inb.audio.totalSamplesReceived } : null },
      };
    }
    renderQuality();
  }

  // ---- leaving ---------------------------------------------------------------------------------

  function leave() {
    if (room) endRoom(null, { notify: true });
  }

  function endRoom(message, { notify }) {
    const ending = room;
    if (!ending) return;
    room = null;
    stopTone();
    clearInterval(ending.timer);
    clearInterval(ending.statsTimer);
    if (notify && ending.id && session) post(`/rooms/${ending.id}/leave`, { session }).catch(() => {});
    for (const p of [...peers.values()]) closePeer(p);
    stopLocalMedia();
    knocks.clear();
    chatLog.length = 0;
    iceServers = null;
    const ui = $("call-ui");
    if (ui) {
      for (const v of ui.querySelectorAll("video")) { v.pause?.(); v.srcObject = null; }
      ui.hidden = true;
      $("call-panel").hidden = true;
      $("call-grid").textContent = "";
      $("call-knocks").textContent = "";
    }
    renderHead();
    const last = ending.startedAt ? " · " + clock(Date.now() - ending.startedAt) : "";
    if (message) toast(message);
    else if (notify && ending.startedAt) toast("Call ended" + last + ".");
    if (PAGE === "join") Join.left(message);
  }

  window.addEventListener("pagehide", () => {
    if (room && room.id && session) {
      try {
        const auth = authHeader();
        fetch(BASE + `/rooms/${room.id}/leave`, { method: "POST", keepalive: true, headers: Object.assign({ "content-type": "application/json" }, auth ? { authorization: auth } : {}), body: JSON.stringify({ session }) });
      } catch (_) { /* best effort */ }
    }
  });

  // ---- the call screen -------------------------------------------------------------------------

  function buildUI() {
    if ($("call-ui")) return;
    const btn = (id, label, icon, extraClass = "") => el("button", { id, class: "call-btn " + extraClass, type: "button", "aria-label": label, title: label }, svg(icon));
    const controls = el("footer", { class: "call-controls", id: "call-controls" },
      btn("call-mute", "Mute microphone", ICON.mic + ICON.slash),
      btn("call-camera", "Turn camera on", ICON.cam + ICON.slash),
      btn("call-share", "Share your screen", ICON.screen),
      el("span", { class: "call-chat-wrap" }, btn("call-chat-btn", "Chat", ICON.chat), el("span", { class: "call-badge", id: "call-chat-badge", hidden: true })),
      el("span", { class: "call-chat-wrap" }, btn("call-people-btn", "People and invite link", ICON.people), el("span", { class: "call-badge", id: "call-people-badge", hidden: true })),
      btn("call-settings-btn", "Camera and microphone", ICON.gear),
      btn("call-hangup", "Leave call", ICON.end, "call-end"),
    );
    const ui = el("div", { class: "call-ui", id: "call-ui", hidden: true },
      el("section", { class: "call-stage", id: "call-stage", role: "dialog", "aria-modal": "true", "aria-labelledby": "call-name" },
        el("div", { class: "call-main" },
          el("header", { class: "call-top" },
            el("div", { class: "call-who" },
              el("h2", { id: "call-name" }),
              el("p", { class: "call-handle", id: "call-handle" }),
              el("p", { class: "call-status" }, el("span", { id: "call-status", "aria-live": "polite" }), el("span", { class: "call-quality", id: "call-quality" })),
            ),
            el("p", { class: "call-sas", id: "call-sas", hidden: true, title: "Compare this code with the other person. If it matches on both screens, the call is end-to-end encrypted between your two devices." },
              svg(ICON.lock), el("span", { text: "Security code" }), el("strong", { id: "call-sas-code" })),
          ),
          el("div", { class: "call-knocks", id: "call-knocks", "aria-live": "polite" }),
          el("div", { class: "call-grid", id: "call-grid", "data-count": "0" }),
          el("div", { class: "call-waiting", id: "call-waiting" }, el("span", { class: "avatar call-avatar", id: "call-face", "aria-hidden": "true" })),
          el("div", { class: "call-self", id: "call-self" }, el("video", { id: "call-self-video", autoplay: true, playsinline: true, muted: true }), el("span", { class: "call-self-label", text: "You" })),
          el("div", { class: "call-presenting", id: "call-presenting", hidden: true }, el("span", { text: "You’re sharing your screen" }), el("button", { type: "button", class: "call-pill", text: "Stop sharing", onclick: () => stopScreen() })),
          controls,
        ),
        el("aside", { class: "call-panel", id: "call-panel", hidden: true, "aria-labelledby": "call-panel-title" },
          el("header", { class: "call-panel-head" },
            el("h3", { id: "call-panel-title" }),
            el("button", { class: "call-icon-btn", type: "button", id: "call-panel-close", "aria-label": "Close panel", title: "Close" }, svg(ICON.close)),
          ),
          el("div", { class: "call-panel-body", id: "call-panel-chat" },
            el("ol", { class: "call-chat-log", id: "call-chat-log", "aria-live": "polite" }),
            el("form", { class: "call-chat-form", id: "call-chat-form" },
              el("textarea", { id: "call-chat-input", rows: "1", maxlength: String(CHAT_CHARS), placeholder: "Message everyone in the call", "aria-label": "Message everyone in the call" }),
              el("button", { class: "call-icon-btn call-chat-send", type: "submit", "aria-label": "Send", title: "Send" }, svg(ICON.send)),
            ),
            el("p", { class: "call-panel-note", text: "Messages go straight to the others over the call’s encrypted connections. They aren’t saved." }),
          ),
          el("div", { class: "call-panel-body", id: "call-panel-people" },
            el("section", { class: "call-invite" },
              el("p", { class: "call-panel-label", text: "Invite link" }),
              el("div", { class: "call-link-row" }, el("code", { id: "call-link" }), el("button", { class: "call-pill", type: "button", id: "call-link-copy" }, svg(ICON.link), el("span", { text: "Copy" }))),
              el("p", { class: "call-panel-note", text: "Anyone with the link can ask to join. Someone in the call has to let them in. Up to 6 people." }),
            ),
            el("ul", { class: "call-knock-list", id: "call-knock-list" }),
            el("p", { class: "call-panel-label", text: "In the call" }),
            el("ul", { class: "call-people-list", id: "call-people-list" }),
          ),
          el("div", { class: "call-panel-body", id: "call-panel-settings" },
            deviceField("mic", "Microphone"),
            deviceField("cam", "Camera"),
            canPickSpeaker ? deviceField("speaker", "Speaker") : null,
          ),
        ),
      ),
    );
    const ringCard = el("div", { class: "call-ring", id: "call-ring", role: "alertdialog", "aria-labelledby": "call-ring-title", "aria-describedby": "call-ring-from call-ring-note", hidden: true },
      el("span", { class: "avatar", id: "call-ring-face", "aria-hidden": "true" }),
      el("div", { class: "call-ring-text" },
        el("p", { class: "call-ring-title", id: "call-ring-title" }),
        el("p", { class: "call-ring-from", id: "call-ring-from" }),
        el("p", { class: "call-ring-note", id: "call-ring-note" }),
      ),
      el("div", { class: "call-ring-actions" },
        el("button", { id: "call-ring-decline", class: "call-btn call-end", type: "button", "aria-label": "Decline", title: "Decline", onclick: declineRing }, svg(ICON.end)),
        el("button", { id: "call-ring-accept", class: "call-btn call-accept", type: "button", "aria-label": "Accept", title: "Accept", onclick: acceptRing }, svg(ICON.phone)),
      ),
    );
    document.body.append(ui, ringCard);
    if (!host()) document.body.append(el("div", { class: "toast", id: "call-toast", role: "status", "aria-live": "polite", hidden: true }));

    $("call-mute").addEventListener("click", toggleMute);
    $("call-camera").addEventListener("click", toggleCamera);
    $("call-share").addEventListener("click", toggleScreen);
    $("call-share").hidden = !canShareScreen;
    $("call-chat-btn").addEventListener("click", () => togglePanel("chat"));
    $("call-people-btn").addEventListener("click", () => togglePanel("people"));
    $("call-settings-btn").addEventListener("click", () => togglePanel("settings"));
    $("call-hangup").addEventListener("click", leave);
    $("call-panel-close").addEventListener("click", () => togglePanel(null));
    $("call-link-copy").addEventListener("click", copyLink);
    $("call-chat-form").addEventListener("submit", (e) => {
      e.preventDefault();
      const input = $("call-chat-input");
      if (sendChat(input.value)) input.value = "";
      input.focus();
    });
    $("call-chat-input").addEventListener("keydown", (e) => {
      if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); $("call-chat-form").requestSubmit(); }
    });
    document.addEventListener("keydown", (e) => {
      if (e.key !== "Escape") return;
      if (ring && !$("call-ring").hidden) declineRing();
      else if (room && !$("call-panel").hidden) togglePanel(null);
    });
  }

  function deviceField(kind, label) {
    const id = "call-device-" + kind;
    const select = el("select", { id, class: "call-select" });
    select.addEventListener("change", () => switchDevice(kind, select.value));
    return el("label", { class: "call-field", for: id }, el("span", { class: "call-panel-label", text: label }), select);
  }

  async function fillDevices(prefix = "call-device-") {
    if (!navigator.mediaDevices?.enumerateDevices) return;
    const devices = await navigator.mediaDevices.enumerateDevices();
    const fill = (kind, deviceKind, current, fallback) => {
      const select = $(prefix + kind);
      if (!select) return;
      const list = devices.filter((d) => d.kind === deviceKind);
      select.textContent = "";
      if (!list.length) { select.append(el("option", { value: "", text: "None found" })); select.disabled = true; return; }
      select.disabled = false;
      list.forEach((d, i) => select.append(el("option", { value: d.deviceId, text: d.label || fallback + " " + (i + 1) })));
      const active = current && list.some((d) => d.deviceId === current) ? current : list[0].deviceId;
      select.value = active;
    };
    fill("mic", "audioinput", local.mic?.getSettings().deviceId || prefs.mic, "Microphone");
    fill("cam", "videoinput", local.cam?.getSettings().deviceId || prefs.cam, "Camera");
    fill("speaker", "audiooutput", prefs.speaker, "Speaker");
  }

  function togglePanel(view) {
    const panel = $("call-panel");
    const showing = !panel.hidden && panel.dataset.view;
    if (!view || showing === view) { panel.hidden = true; panel.dataset.view = ""; $("call-stage").dataset.panel = "false"; $("call-stage").dataset.panelView = ""; renderControls(); return; }
    panel.hidden = false;
    panel.dataset.view = view;
    $("call-stage").dataset.panel = "true";
    $("call-stage").dataset.panelView = view;
    $("call-panel-title").textContent = { chat: "Chat", people: "People", settings: "Camera and microphone" }[view];
    for (const v of ["chat", "people", "settings"]) $("call-panel-" + v).hidden = v !== view;
    if (view === "chat") { if (room) room.unread = 0; renderChat(); $("call-chat-input").focus({ preventScroll: true }); }
    if (view === "people") renderPeople();
    if (view === "settings") fillDevices();
    renderControls();
  }

  function meetingLink() {
    return room && room.code ? location.origin + "/" + room.code : "";
  }

  async function copyLink() {
    const link = meetingLink();
    if (!link) return;
    try { await navigator.clipboard.writeText(link); toast("Invite link copied."); }
    catch (_) {
      const range = document.createRange();
      range.selectNodeContents($("call-link"));
      getSelection().removeAllRanges();
      getSelection().addRange(range);
      toast("Select and copy the link.");
    }
  }

  function openRoomUI({ title, sub, face, status }) {
    buildUI();
    $("call-name").textContent = title;
    $("call-handle").textContent = sub || "";
    paintFace($("call-face"), face || title);
    $("call-quality").textContent = "";
    $("call-sas").hidden = true;
    $("call-ui").hidden = false;
    setStatus(status || "");
    renderControls();
    renderHead();
    $("call-hangup").focus({ preventScroll: true });
  }

  function showSelf() {
    const v = $("call-self-video");
    if (!v) return;
    v.muted = true;
    v.srcObject = local.stream;
    v.play().catch(() => {});
    $("call-self").hidden = !(local.cam && !local.cameraOff);
  }

  function renderControls() {
    if (!$("call-ui")) return;
    const set = (id, pressed, label) => { const b = $(id); b.setAttribute("aria-pressed", String(pressed)); b.setAttribute("aria-label", label); b.title = label; };
    set("call-mute", local.muted, local.muted ? "Unmute microphone" : "Mute microphone");
    const camOn = Boolean(local.cam && !local.cameraOff);
    set("call-camera", !camOn, camOn ? "Turn camera off" : (local.cam ? "Turn camera on" : "Turn camera on (switch to video)"));
    set("call-share", Boolean(local.screen), local.screen ? "Stop sharing your screen" : "Share your screen");
    const view = !$("call-panel").hidden && $("call-panel").dataset.view;
    for (const [id, v] of [["call-chat-btn", "chat"], ["call-people-btn", "people"], ["call-settings-btn", "settings"]]) $(id).setAttribute("aria-expanded", String(view === v));
    const unread = room ? room.unread || 0 : 0;
    $("call-chat-badge").hidden = !unread;
    $("call-chat-badge").textContent = unread > 9 ? "9+" : String(unread);
    $("call-people-badge").hidden = !knocks.size;
    $("call-people-badge").textContent = String(knocks.size);
    $("call-presenting").hidden = !local.screen;
    $("call-share").disabled = !room || !room.id || peers.size === 0;
  }

  function setStatus(text) { const s = $("call-status"); if (s) s.textContent = text; }
  function renderClock() { if (room && room.startedAt) setStatus(clock(Date.now() - room.startedAt)); }

  // Tiles: one per person. Whoever is sharing a screen takes the big space; everyone else shrinks
  // into a strip beside it.
  function renderStage() {
    if (!room || !$("call-ui")) return;
    const grid = $("call-grid");
    const list = [...peers.values()];
    const presenter = list.find((p) => p.remote.screen.getVideoTracks().some((t) => t.readyState === "live"));
    grid.dataset.count = String(list.length);
    grid.dataset.presenting = String(Boolean(presenter));
    $("call-waiting").hidden = list.some((p) => p.connected);
    if (list.length === 1 && !list[0].connected) setStatus(room.state === "ringing" ? "Ringing…" : "Connecting…");
    for (const p of list) {
      if (!p.tile) {
        p.tile = el("figure", { class: "call-tile" },
          el("video", { class: "call-tile-video", autoplay: true, playsinline: true }),
          el("video", { class: "call-tile-screen", autoplay: true, playsinline: true, muted: true }),
          el("span", { class: "avatar call-tile-face", "aria-hidden": "true" }),
          el("figcaption", { class: "call-tile-name" }, el("span", { class: "call-tile-label" }), el("span", { class: "call-tile-muted", hidden: true }, svg(ICON.mic + ICON.slash))),
        );
        p.tile.dataset.pid = p.id;
        grid.append(p.tile);
      }
      const video = p.tile.querySelector(".call-tile-video");
      const screen = p.tile.querySelector(".call-tile-screen");
      if (video.srcObject !== p.remote.camera) { video.srcObject = p.remote.camera; video.play().catch(() => {}); if (prefs.speaker) video.setSinkId?.(prefs.speaker).catch(() => {}); }
      if (screen.srcObject !== p.remote.screen) { screen.srcObject = p.remote.screen; screen.play().catch(() => {}); }
      const camLive = p.remote.camera.getVideoTracks().some((t) => t.readyState === "live" && !t.muted) && !p.state.cameraOff;
      const screenLive = p === presenter;
      p.tile.dataset.video = String(camLive);
      p.tile.dataset.screen = String(screenLive);
      p.tile.dataset.connected = String(p.connected);
      p.tile.classList.toggle("call-tile-presenter", screenLive);
      paintFace(p.tile.querySelector(".call-tile-face"), p.mailbox ? (p.mailbox.handle || p.mailbox.address) : p.name);
      p.tile.querySelector(".call-tile-label").textContent = displayName(p) + (p.guest ? " · guest" : "") + (p.connected ? "" : " · connecting");
      p.tile.querySelector(".call-tile-muted").hidden = !p.state.muted;
    }
    for (const tile of [...grid.children]) if (!peers.has(tile.dataset.pid)) tile.remove();
    // The header names the call: the other person, or how many are in it.
    if (list.length > 1) {
      $("call-name").textContent = (list.length + 1) + " people";
      $("call-handle").textContent = list.map((p) => displayName(p)).join(", ");
    } else if (list.length === 1 && PAGE === "join") {
      $("call-name").textContent = displayName(list[0]);
      $("call-handle").textContent = list[0].mailbox ? (list[0].mailbox.handle || list[0].mailbox.address) : "";
    }
    const solo = list.length === 1 ? list[0] : null;
    $("call-sas").hidden = !(solo && solo.security);
    if (solo && solo.security) $("call-sas-code").textContent = solo.security.code;
    $("call-link").textContent = meetingLink();
    renderControls();
    if (!$("call-panel").hidden && $("call-panel").dataset.view === "people") renderPeople();
  }

  function renderKnocks() {
    const box = $("call-knocks");
    if (!box) return;
    box.textContent = "";
    for (const k of knocks.values()) box.append(knockCard(k, "call-knock"));
    const list = $("call-knock-list");
    list.textContent = "";
    for (const k of knocks.values()) list.append(el("li", {}, knockCard(k, "call-knock call-knock-inline")));
    renderControls();
  }

  function knockCard(k, cls) {
    const who = k.mailbox ? (k.mailbox.handle || k.mailbox.address) : k.name;
    const face = el("span", { class: "avatar", "aria-hidden": "true" });
    paintFace(face, who);
    const answer = (admit) => post(`/rooms/${room.id}/knocks/${k.knock}`, { session, admit }).catch((err) => { if (err.code === "room_full") toast("The call is full."); });
    return el("div", { class: cls, role: "group", "aria-label": who + " wants to join" },
      face,
      el("div", { class: "call-knock-text" },
        el("p", { class: "call-knock-name", text: who }),
        el("p", { class: "call-knock-note", text: k.mailbox ? "Pigeonpost member · wants to join" : "Guest · name not verified · wants to join" }),
      ),
      el("button", { class: "call-pill", type: "button", text: "Deny", onclick: () => answer(false) }),
      el("button", { class: "call-pill call-pill-primary", type: "button", text: "Let in", onclick: () => answer(true) }),
    );
  }

  function renderPeople() {
    const list = $("call-people-list");
    if (!list) return;
    list.textContent = "";
    const row = (name, note, code, face) => {
      const f = el("span", { class: "avatar", "aria-hidden": "true" });
      paintFace(f, face || name);
      return el("li", { class: "call-person" }, f,
        el("div", { class: "call-person-text" }, el("p", { class: "call-person-name", text: name }), el("p", { class: "call-person-note", text: note })),
        code ? el("span", { class: "call-person-code", title: "Security code with this person. If they read the same code, your connection is end-to-end encrypted." }, svg(ICON.lock), el("span", { text: code })) : null);
    };
    const me = host()?.mailbox?.();
    list.append(row("You", me ? (me.handle || me.address) : (Join.name || "Guest"), null, me ? (me.handle || me.address) : Join.name));
    for (const p of peers.values()) {
      const key = p.mailbox ? (p.mailbox.handle || p.mailbox.address) : p.name;
      list.append(row(displayName(p), (p.guest ? "Guest · name not verified" : key) + (p.connected ? (p.stats?.route === "relay" ? " · relayed" : "") : " · connecting"), p.security?.code, key));
    }
  }

  function renderChat() {
    const log = $("call-chat-log");
    if (!log) return;
    log.textContent = "";
    for (const m of chatLog) {
      log.append(el("li", { class: "call-chat-item" + (m.mine ? " call-chat-mine" : "") },
        el("p", { class: "call-chat-meta" }, el("span", { class: "call-chat-who", text: m.who }), el("time", { text: new Date(m.at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }) })),
        el("p", { class: "call-chat-text", text: m.text })));
    }
    log.scrollTop = log.scrollHeight;
    renderControls();
  }

  function renderQuality() {
    const list = [...peers.values()];
    const solo = list.length === 1 ? list[0] : null;
    let text = "";
    if (solo && solo.stats) {
      const rx = solo.stats.receiving.video;
      const res = rx && rx.height ? (rx.height >= 1080 ? "1080p" : rx.height >= 720 ? "720p" : rx.height + "p") : "";
      text = [res, solo.stats.route === "relay" ? "Relayed" : solo.stats.route ? "Direct" : ""].filter(Boolean).join(" · ");
    } else if (list.length > 1) {
      text = list.some((p) => p.stats?.route === "relay") ? "Relayed" : "";
    }
    const q = $("call-quality");
    if (q) q.textContent = text;
  }

  function renderHead() {
    const h = host();
    const show = Boolean(supported && h && h.signedIn() && h.peer() && h.mailbox() && session !== undefined);
    for (const id of ["call-audio-btn", "call-video-btn"]) {
      const button = $(id);
      if (!button) continue;
      button.hidden = !show;
      button.disabled = Boolean(room || ring);
    }
  }

  // ---- tones -----------------------------------------------------------------------------------

  let tone = null;
  function startTone(kind, times) {
    stopTone();
    const Ctx = window.AudioContext || window.webkitAudioContext;
    if (!Ctx) return;
    try {
      const ctx = new Ctx();
      const gain = ctx.createGain();
      gain.gain.value = 0;
      gain.connect(ctx.destination);
      const freqs = kind === "ring" ? [523.25, 659.25] : kind === "knock" ? [784] : [440];
      const oscs = freqs.map((f) => { const o = ctx.createOscillator(); o.type = "sine"; o.frequency.value = f; o.connect(gain); o.start(); return o; });
      const level = kind === "ring" ? 0.07 : kind === "knock" ? 0.05 : 0.035;
      const on = kind === "ring" ? 900 : kind === "knock" ? 180 : 1200, period = kind === "ring" ? 2600 : kind === "knock" ? 400 : 4000;
      let count = 0;
      const pulse = () => {
        if (times && count++ >= times * 2) { stopTone(); return; }
        const t = ctx.currentTime;
        gain.gain.cancelScheduledValues(t);
        gain.gain.setTargetAtTime(level, t, 0.02);
        gain.gain.setTargetAtTime(0, t + on / 1000, 0.03);
      };
      pulse();
      const interval = setInterval(pulse, period);
      ctx.resume?.().catch(() => {});
      tone = { ctx, oscs, interval };
    } catch (_) { tone = null; }
  }
  function stopTone() {
    if (!tone) return;
    clearInterval(tone.interval);
    try { tone.oscs.forEach((o) => o.stop()); tone.ctx.close(); } catch (_) { /* closed */ }
    tone = null;
  }

  // ---- words -----------------------------------------------------------------------------------

  const kindWord = (kind) => (kind === "video" ? "video" : "voice");
  const clock = (ms) => {
    const s = Math.max(0, Math.floor(ms / 1000));
    const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), r = s % 60;
    return (h ? h + ":" + String(m).padStart(2, "0") : String(m)) + ":" + String(r).padStart(2, "0");
  };

  function endedMessage(reason) {
    return {
      declined: "The call was declined.",
      busy: "They’re on another call.",
      no_answer: "No answer.",
      cancelled: "The call was cancelled.",
      ended: "Call ended.",
      max_duration: "The call reached its time limit.",
    }[reason] || null;
  }

  function placeError(err) {
    return {
      already_in_call: "You’re already in a call.",
      rate_limited: "Too many calls in a short time. Try again in a minute.",
      cannot_call_self: "That’s the mailbox you’re calling from.",
      not_your_mailbox: "This mailbox can’t place calls.",
    }[err.code] || "The call couldn’t be started.";
  }

  function mediaMessage(err, kind) {
    const what = kind === "video" ? "camera and microphone" : "microphone";
    if (err && (err.name === "NotAllowedError" || err.name === "SecurityError")) return "Pigeonpost needs permission to use your " + what + ". Allow it in the browser and try again.";
    if (err && err.name === "NotFoundError") return "No microphone was found.";
    if (err && err.name === "NotReadableError") return "Your " + what + " is in use by another app.";
    return "Your " + what + " couldn’t be started.";
  }

  // ---- the meeting-link page -------------------------------------------------------------------

  const Join = {
    code: null, name: "", previewStream: null,
    init() {
      this.code = (location.pathname.match(/^\/([A-Za-z0-9]{6})\/?$/) || [])[1]?.toUpperCase() || null;
      buildUI();
      const form = $("join-form");
      $("join-code").textContent = this.code ? this.code.slice(0, 3) + " " + this.code.slice(3) : "";
      if (!supported) return this.fail("This browser can’t make calls. Try a recent Chrome, Edge, Firefox or Safari.");
      if (!this.code) return this.fail("That meeting link isn’t complete. Check it and try again.");
      try { $("join-name").value = sessionStorage.getItem("ppi_guest_name") || ""; } catch (_) { /* fine */ }
      if (!canPickSpeaker) $("join-device-speaker").closest(".call-field").hidden = true;
      form.addEventListener("submit", (e) => { e.preventDefault(); this.ask(); });
      $("join-cancel").addEventListener("click", () => this.cancel());
      $("join-mic").addEventListener("click", () => this.togglePreview("mic"));
      $("join-cam").addEventListener("click", () => this.togglePreview("cam"));
      for (const kind of ["mic", "cam", "speaker"]) {
        const select = $("join-device-" + kind);
        select?.addEventListener("change", async () => {
          if (kind === "speaker") { prefs.speaker = select.value || null; return; }
          prefs[kind] = select.value || null;
          await this.preview();
        });
      }
      this.check();
    },
    async check() {
      try {
        const info = await (await fetch(BASE + "/meetings/" + this.code)).json();
        if (info.error) return this.fail(info.error === "rate_limited" ? "Too many attempts. Wait a few minutes and try again." : "This call has ended, or the link is wrong.");
        if (info.full) return this.fail("This call is full.");
        $("join-meta").textContent = (info.kind === "video" ? "Video call" : "Voice call") + " · " + info.participants + (info.participants === 1 ? " person" : " people") + " in the call";
        $("join-form").hidden = false;
        this.wantCam = info.kind === "video";
        await this.preview();
        if (!authHeader()) await ensureGuest();
        start();
      } catch (_) {
        this.fail("Couldn’t reach the call service. Check your connection and reload.");
      }
    },
    async preview() {
      try {
        this.previewStream?.getTracks().forEach((t) => t.stop());
        const media = await capture({ audio: true, video: this.wantCam });
        this.previewStream = media;
        const v = $("join-preview");
        v.srcObject = media;
        v.play().catch(() => {});
        $("join-preview-wrap").dataset.video = String(media.getVideoTracks().length > 0);
        await fillDevices("join-device-");
      } catch (err) {
        $("join-error").textContent = mediaMessage(err, this.wantCam ? "video" : "audio");
        $("join-error").hidden = false;
      }
      this.renderToggles();
    },
    togglePreview(kind) {
      if (kind === "mic") { this.micOff = !this.micOff; this.previewStream?.getAudioTracks().forEach((t) => { t.enabled = !this.micOff; }); }
      else { this.wantCam = !this.wantCam; this.preview(); }
      this.renderToggles();
    },
    renderToggles() {
      $("join-mic").setAttribute("aria-pressed", String(Boolean(this.micOff)));
      $("join-mic").setAttribute("aria-label", this.micOff ? "Unmute microphone" : "Mute microphone");
      $("join-cam").setAttribute("aria-pressed", String(!this.wantCam));
      $("join-cam").setAttribute("aria-label", this.wantCam ? "Turn camera off" : "Turn camera on");
    },
    onReady() {
      const members = myMailboxes || [];
      const pick = $("join-as");
      if (members.length && !guestToken) {
        pick.textContent = "";
        for (const m of members) pick.append(el("option", { value: m.address, text: m.handle || m.address }));
        $("join-as-row").hidden = false;
        $("join-name-row").hidden = true;
      } else {
        $("join-as-row").hidden = true;
        $("join-name-row").hidden = false;
      }
      $("join-submit").disabled = false;
    },
    async ask() {
      const asMember = !$("join-as-row").hidden;
      const name = $("join-name").value.trim();
      if (!asMember && !name) { $("join-name").focus(); return; }
      this.name = asMember ? $("join-as").selectedOptions[0]?.textContent : name;
      try { if (!asMember) sessionStorage.setItem("ppi_guest_name", name); } catch (_) { /* fine */ }
      $("join-submit").disabled = true;
      $("join-error").hidden = true;
      try {
        await whenReady();
        const body = asMember ? { session, code: this.code, as: $("join-as").value } : { session, code: this.code, name };
        const res = await post("/knocks", body);
        this.knock = res.knock;
        $("join-form").hidden = true;
        $("join-waiting").hidden = false;
        $("join-cancel").focus();
      } catch (err) {
        $("join-submit").disabled = false;
        $("join-error").textContent = { room_full: "This call is full.", no_meeting: "This call has ended.", rate_limited: "Too many attempts. Wait a few minutes and try again.", too_many_requests: "Several people are already waiting. Try again in a moment." }[err.code] || "Couldn’t ask to join. Try again.";
        $("join-error").hidden = false;
      }
    },
    async cancel() {
      if (this.knock) await post("/knocks/withdraw", { session, knock: this.knock }).catch(() => {});
      this.knock = null;
      $("join-waiting").hidden = true;
      $("join-form").hidden = false;
      $("join-submit").disabled = false;
    },
    onResult(data) {
      if (data.knock !== this.knock) return;
      if (data.admitted) {
        $("join-waiting-text").textContent = "You’re in. Connecting…";
        return;
      }
      this.knock = null;
      $("join-waiting").hidden = true;
      $("join-form").hidden = false;
      $("join-submit").disabled = false;
      $("join-error").textContent = { denied: "Someone in the call declined your request.", timeout: "Nobody answered your request. You can ask again.", ended: "This call has ended.", room_full: "This call is full." }[data.reason] || "You weren’t let in.";
      $("join-error").hidden = false;
    },
    enter() {
      // Hand the preview's devices to the call, rather than asking for them a second time.
      const media = this.previewStream;
      this.previewStream = null;
      local.stream = new MediaStream();
      local.mic = media?.getAudioTracks()[0] || null;
      local.cam = media?.getVideoTracks()[0] || null;
      if (local.mic) { local.mic.contentHint = "speech"; local.mic.enabled = !this.micOff; local.stream.addTrack(local.mic); }
      if (local.cam) local.stream.addTrack(local.cam);
      local.muted = Boolean(this.micOff);
      local.cameraOff = false;
      $("join").hidden = true;
      openRoomUI({ title: "Call", sub: "", face: this.name, status: "Connecting…" });
      showSelf();
      renderControls();
    },
    left(message) {
      $("join").hidden = false;
      $("join-form").hidden = true;
      $("join-waiting").hidden = true;
      $("join-done").hidden = false;
      $("join-done-text").textContent = message || "You left the call.";
    },
    fail(message) {
      $("join-form").hidden = true;
      $("join-waiting").hidden = true;
      $("join-error").textContent = message;
      $("join-error").hidden = false;
    },
  };

  // ---- wiring ----------------------------------------------------------------------------------

  function wire() {
    if (PAGE === "join") { Join.init(); return; }
    buildUI();
    $("call-audio-btn")?.addEventListener("click", () => place("audio"));
    $("call-video-btn")?.addEventListener("click", () => place("video"));
    start();
    renderHead();
  }

  window.PigeonpostCalls = {
    start, stop, renderHead,
    // For automated checks: who is connected, by which route, at what resolution and rate, and with
    // which security codes. Contains no media and no message text.
    inspect: () => ({
      session, page: PAGE,
      room: room && { id: room.id, code: room.code, you: room.you, state: room.state, size: room.size },
      call: room && { id: room.id, state: room.state === "active" && [...peers.values()].some((p) => p.connected) ? "active" : room.state },
      ring: ring && { room: ring.room, kind: ring.kind },
      knocks: [...knocks.keys()],
      local: { mic: Boolean(local.mic), cam: Boolean(local.cam), screen: Boolean(local.screen), muted: local.muted, cameraOff: local.cameraOff, micDevice: local.mic?.getSettings().deviceId || null, camDevice: local.cam?.getSettings().deviceId || null },
      chat: chatLog.length,
      peers: [...peers.values()].map((p) => ({ id: p.id, name: displayName(p), guest: p.guest, connected: p.connected, security: p.security, stats: p.stats && { route: p.stats.route, rtt: p.stats.rtt, sending: p.stats.sending, receiving: p.stats.receiving }, pc: p.pc && { signaling: p.pc.signalingState, ice: p.pc.iceConnectionState, gathering: p.pc.iceGatheringState, connection: p.pc.connectionState, local: Boolean(p.pc.localDescription), remote: Boolean(p.pc.remoteDescription), polite: p.polite, pendingIce: p.pendingIce.length }, remoteCamera: p.remote.camera.getVideoTracks().length, remoteScreen: p.remote.screen.getVideoTracks().length })),
    }),
  };

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", wire);
  else wire();
})();
