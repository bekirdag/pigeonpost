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
// A call of more than six people (up to 30) runs through a media server instead (a LiveKit SFU). Then
// every browser encrypts each audio and video frame itself before it leaves the device, with a room
// key that the browsers pass to each other wrapped to each one's own key pair (calls-e2ee.js); the
// call service and the media server never hold it. The call shows one security code for the whole
// room. Browsers that cannot encrypt their own frames are told the call needs a newer browser; there
// is no unencrypted fallback. When a seventh person joins a mesh call, everyone moves to the media
// server and keeps their direct connections until the media server is carrying the others.
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
  // Large calls: at most this many views on screen at once, the rest a page further.
  const PAGE_SIZE = 16;
  // A new room key is used for sending this long after it arrives, so that everyone has it by then.
  const KEY_SWITCH_MS = 1_500;
  const LIVEKIT_BASE = "/vendor/livekit-client/";
  const LIVEKIT_INTEGRITY = "sha384-5qI/UmLG0CvuWCt6vqjMW1oRwzXu+MNhbw4x6yo9NARdlqi94LwQfq9wvDTIcfrD";
  const E2EE_SCRIPT = "/calls-e2ee.js?v=20261020-sfu";
  const E2EE_WORKER = "/calls-e2ee-worker.js?v=20261020-sfu";
  // Phase 3B: "meeting" is the scheduled-meeting page (m.html), which joins through joinScheduled.
  const PAGE = document.body?.dataset.page === "join" ? "join" : document.body?.dataset.page === "meeting" ? "meeting" : "inbox";

  const supported = Boolean(window.RTCPeerConnection && navigator.mediaDevices && navigator.mediaDevices.getUserMedia);
  const canShareScreen = Boolean(navigator.mediaDevices && navigator.mediaDevices.getDisplayMedia);
  const canPickSpeaker = typeof HTMLMediaElement !== "undefined" && "setSinkId" in HTMLMediaElement.prototype;
  // Can this browser encrypt its own media frames (needed for calls of more than six)? Chrome, Edge and
  // their web views have encoded insertable streams; Safari, WebKit web views and Firefox have
  // RTCRtpScriptTransform. The same test as PigeonpostCallsE2EE.frameEncryptionSupported.
  const canEncryptFrames = Boolean(supported && (typeof window.RTCRtpScriptTransform !== "undefined"
    || (window.RTCRtpSender && typeof RTCRtpSender.prototype.createEncodedStreams !== "undefined"))
    && window.crypto && window.crypto.subtle && typeof Worker !== "undefined");
  const NEEDS_NEWER = "This call has more than six people, which needs a newer browser or app — one that can encrypt your audio and video itself. Update this browser or app, or use a current Chrome, Edge, Firefox or Safari.";
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
    // Lucide's "settings" cog (ISC licence), drawn on its own 24-unit grid.
    gear: '<g transform="scale(.8333)" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"/><circle cx="12" cy="12" r="3"/></g>',
    fullscreen: '<path d="M3.5 7.5v-4h4M16.5 7.5v-4h-4M3.5 12.5v4h4M16.5 12.5v4h-4" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/>',
    unfullscreen: '<path d="M7.5 3.5v4h-4M12.5 3.5v4h4M7.5 16.5v-4h-4M12.5 16.5v-4h4" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/>',
    pin: '<path d="M7.5 2.5h5l-.8 4.8 2.8 2.7v1.5H5.5V10l2.8-2.7Z M10 11.5v6" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round" stroke-linecap="round"/>',
    end: '<path d="M2.4 11.6c-.4-.5-.4-1.2.1-1.7C5 7.6 7.4 6.8 10 6.8s5 .8 7.5 3.1c.5.5.5 1.2.1 1.7l-1.2 1.4c-.4.4-1 .5-1.5.3l-2-1c-.5-.2-.8-.7-.8-1.2V9.7a8.6 8.6 0 0 0-4.2 0v1.4c0 .5-.3 1-.8 1.2l-2 1c-.5.2-1.1.1-1.5-.3Z" fill="currentColor"/>',
    phone: '<path d="M6.6 2.9 4.5 3.3c-.8.2-1.4.9-1.3 1.7.5 6.1 5.7 11.3 11.8 11.8.8.1 1.5-.5 1.7-1.3l.4-2.1c.1-.6-.2-1.2-.8-1.4l-2.4-1c-.5-.2-1.1-.1-1.5.3l-.9.9a9 9 0 0 1-4-4l.9-.9c.4-.4.5-1 .3-1.5l-1-2.4c-.2-.6-.8-.9-1.4-.8Z" fill="currentColor"/>',
    chevron: '<path d="m5.5 8 4.5 4.5L14.5 8" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"/>',
    back: '<path d="M12.5 4 6 10l6.5 6" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"/>',
    expand: '<path d="M11.5 3.5h5v5M8.5 16.5h-5v-5M16.5 3.5 11 9M3.5 16.5 9 11" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/>',
    lock: '<rect x="4" y="9" width="12" height="8.5" rx="2" fill="none" stroke="currentColor" stroke-width="1.5"/><path d="M6.8 9V6.5a3.2 3.2 0 0 1 6.4 0V9" fill="none" stroke="currentColor" stroke-width="1.5"/>',
    close: '<path d="M5 5l10 10M15 5 5 15" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>',
    send: '<path d="M3.5 10 16.5 3.8 13 16.2 9.4 11.1Z M9.4 11.1 16.5 3.8" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/>',
    link: '<path d="M8.5 11.5a3 3 0 0 0 4.2 0l2.6-2.6a3 3 0 0 0-4.2-4.2l-1 1M11.5 8.5a3 3 0 0 0-4.2 0l-2.6 2.6a3 3 0 0 0 4.2 4.2l1-1" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>',
  };

  // Faces: the same two letters and tone the inbox uses, so a person looks the same in both places.
  // A handle is named first ("/bekir/main" -> "Bekir" -> "BE"), never lettered from its last segment.
  function initials(name) {
    const text = String(name || "");
    const named = text.startsWith("/") ? displayName(text) : text;
    if (window.PigeonpostNames) return window.PigeonpostNames.initials(named);
    const base = named.split("/").filter(Boolean).pop() || named;
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
    // Phase 1b: the inbox may lay a profile picture over the initials (falls back to them).
    host()?.decorateAvatar?.(node, name);
  }
  // The name a call shows for someone: the shared naming rule (names.js, "Bob" not "/bob"),
  // applied through displayName so a contact alias still wins.
  function callName(m) {
    return displayName(m);
  }
  function displayName(m) {
    if (!m) return "Guest";
    if (typeof m === "string") return host()?.displayName ? host().displayName(m) : window.PigeonpostNames ? window.PigeonpostNames.displayName(m) : m;
    if (m.mailbox) return displayName(m.mailbox.handle || m.mailbox.address);
    return m.name || "Guest";
  }

  // While the call screen is up it covers the inbox's own toast, so messages show inside the call.
  function toast(message) {
    const inCall = $("call-ui") && !$("call-ui").hidden;
    if (!inCall && host()?.toast) return host().toast(message);
    const t = inCall ? $("call-inner-toast") : $("call-toast");
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
  // Phase 1c (docs/call-alerting.md): a ring to answer as soon as it is seen — from a device that was
  // woken by a push (`PigeonpostNative.answer`) or a link (`?answer=<room>`).
  let pendingAnswer = null; // { room, ringToken, at }
  const ROOM_ID = /^[A-Za-z0-9_-]{10,64}$/, RING_TOKEN_RE = /^[A-Za-z0-9_-]{43}$/;
  // Tell the app hosting this page (if any) about the ring and the call. A browser has no host.
  function tellHost(name, ...args) {
    const h = host();
    try { return h && typeof h[name] === "function" ? h[name](...args) : undefined; } catch (_) { return undefined; }
  }
  let room = null; // the call on screen
  let knockState = null; // join page: { code, knock, state }
  const peers = new Map(); // participant id -> peer
  const knocks = new Map(); // knock id -> request waiting inside a call
  const chatLog = [];
  // `rawMic` is the microphone as captured (with the browser's echo cancellation); `mic` is what is
  // sent — the same track, or the same track after noise removal.
  const local = { stream: null, rawMic: null, mic: null, cam: null, screen: null, screenStream: null, muted: false, cameraOff: false };
  // Captured tracks waiting for asynchronous noise processing must stop immediately on hang-up.
  const pendingCaptureTracks = new Set();
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
    const generation = streamGen;
    const auth = authHeader();
    const res = await fetch(BASE + path, {
      method,
      headers: Object.assign(auth ? { authorization: auth } : {}, body ? { "content-type": "application/json" } : {}),
      body: body ? JSON.stringify(body) : undefined,
    });
    if (res.status === 401 && !renewed && host()) {
      if (generation !== streamGen) throw Object.assign(new Error("cancelled"), { code: "cancelled" });
      const fresh = await host().renew();
      if (generation !== streamGen) throw Object.assign(new Error("cancelled"), { code: "cancelled" });
      if (fresh) return request(method, path, body, true);
    }
    const value = await res.json().catch(() => ({}));
    if (!res.ok) throw Object.assign(new Error(value.error || "http_" + res.status), { status: res.status, code: value.error, body: value }); // Phase 3B: body
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
    window.PigeonpostLive?.stop?.(); // Phase 6: signing out ends one's own stream too
    streamWanted = false;
    streamGen += 1;
    if (streamAbort) streamAbort.abort();
    streamAbort = null;
    dismissRing();
    if (room) endRoom(null, { notify: true });
    session = null;
    while (readyWaiters.length) readyWaiters.shift()(null);
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
        if (!streamWanted || gen !== streamGen) return;
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
          if (!streamWanted || gen !== streamGen) return;
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
      if (!streamWanted || gen !== streamGen) return;
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
        // Events missed while the stream was down (people leaving, keys announced) come back with the room.
        if (room && room.you) post(`/rooms/${room.id}/resume`, { session, participant: room.you }).then(resync).catch(() => {});
        if (PAGE === "join") Join.onReady();
        window.PigeonpostLive?.onReady(session); // Phase 6: a stream rejoins on the new session
        return;
      case "incoming": return onIncoming(data);
      case "summary": window.PigeonpostCallsSummary?.onEvent(data); return; // Phase 7: meeting notes
      case "ring_ended":
        if (ring && ring.room === data.room) {
          const missed = ring;
          tellHost("ringEnded", { room: data.room, reason: data.reason });
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
          tellHost("callState", { state: "active", room: room.id, kind: room.kind });
          stopTone();
          noteMember(data);
          // The newcomer offers; this end prepares to answer. On the media server nobody offers.
          ensurePeer(data, false);
          if (room.topology === "sfu") Sfu.onMembers();
          renderStage();
        }
        return;
      case "participant_left":
        if (room && room.id === data.room) {
          forgetMember(data.participant);
          const p = peers.get(data.participant);
          if (p) {
            closePeer(p);
            if (room.size > 2) toast(displayName(p) + " left the call.");
          }
          if (room.topology === "sfu") Sfu.onMembers();
          renderStage();
        }
        return;
      case "topology":
        // The seventh person is coming in: this call moves to the media server.
        if (room && room.id === data.room && data.topology === "sfu" && room.topology !== "sfu") {
          room.topology = "sfu";
          Sfu.enter();
          renderStage();
        }
        return;
      case "participant_key":
        if (room && room.id === data.room) {
          const m = room.members && room.members.get(data.participant);
          if (m) Object.assign(m, { key: data.key, epoch: data.epoch || 0 });
          Sfu.onMembers();
        }
        return;
      case "signal":
        if (room && room.id === data.room) onSignal(data.from, data.type, data.data).catch((err) => console.warn("calls: signal", err && err.name));
        return;
      case "knock":
        if (room && room.id === data.room) {
          knocks.set(data.knock, data);
          renderKnocks();
          startTone("knock", 1);
          if (room.minimized) toast(displayName(data) + " wants to join your call. Return to the call to let them in.");
        }
        return;
      case "knock_resolved":
        if (room && room.id === data.room) {
          const k = knocks.get(data.knock);
          knocks.delete(data.knock);
          renderKnocks();
          if (k && data.admitted && data.by && data.by !== room.you) toast(displayName(k) + " was let in.");
          else if (k && !data.admitted && data.by === room.you && admissionWords[data.reason]) toast(displayName(k) + " couldn’t be let in: " + admissionWords[data.reason]);
        }
        return;
      case "knock_result":
        if (PAGE === "join") Join.onResult(data);
        else if (room && room.state === "waiting" && data.knock === room.knock) onScheduledKnock(data); // Phase 3B
        return;
      case "scheduled_window": // Phase 3B: a host extended the meeting
        if (room && room.id === data.room && room.scheduled) { room.scheduled.closes_at = data.closes_at ?? room.scheduled.closes_at; renderScheduled(); }
        return;
      case "ended":
        if (room && room.id === data.room) endRoom(endedMessage(data.reason), { notify: false });
        return;
      default:
        // Phase 6: live streams (live.js) share this event stream.
        if (name.startsWith("stream_")) window.PigeonpostLive?.onCallsEvent(name, data);
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
    if (room || ring || window.PigeonpostLive?.active()) { toast("Finish the current call first."); return; }
    if (!session) { toast("Calling isn’t ready yet. Try again in a moment."); start(); return; }
    openRoomUI({ title: callName(peer), sub: peer, face: peer, status: "Starting " + kindWord(kind) + "…" });
    room = newRoom({ kind, title: displayName(peer), peerKey: peer });
    tellHost("callState", { state: "connecting", room: null, kind, outgoing: true, peer });
    try {
      if (!await startLocalMedia(kind === "video")) return;
    } catch (err) {
      endRoom(mediaMessage(err, kind), { notify: false });
      return;
    }
    if (!room) return;
    try {
      const placed = await post("/calls", { session, from: me.address, to: peer, kind, e2ee: canEncryptFrames });
      if (!room) { post(`/rooms/${placed.room}/leave`, { session }).catch(() => {}); return; }
      Object.assign(room, { id: placed.room, code: placed.code, you: placed.you, state: "ringing", topology: placed.topology || "mesh" });
      window.PigeonpostCallsSummary?.joined(placed.room); // Phase 7: the host's notes offer may have come first
      setMembers(placed.participants);
      setStatus("Ringing…");
      startTone("ringback");
      renderStage();
    } catch (err) {
      endRoom(placeError(err), { notify: false });
    }
  }

  // Phase 3a: group calls. The calls service rings every active member (each filtered by their own
  // call settings) and lets any active member join a call in progress without knocking. Membership
  // is checked by the postbox at create, ring and join.
  async function placeGroup(kind, group, join) {
    const h = host();
    const me = h && h.mailbox();
    if (!me || !group || !supported) return;
    if (room || ring || window.PigeonpostLive?.active()) { toast("Finish the current call first."); return; }
    if (!session) { toast("Calling isn’t ready yet. Try again in a moment."); start(); return; }
    openRoomUI({ title: group.name, sub: join ? "Joining group call" : "Group call", face: group.name, status: (join ? "Joining " : "Starting ") + kindWord(kind) + "…" });
    room = newRoom({ kind, title: group.name, peerKey: group.id, groupId: group.id });
    try {
      if (!await startLocalMedia(kind === "video")) return;
    } catch (err) {
      endRoom(mediaMessage(err, kind), { notify: false });
      return;
    }
    if (!room) return;
    try {
      if (join) {
        const joined = await post(`/groups/${encodeURIComponent(group.id)}/join`, { session, from: me.address });
        if (!room) { if (joined.room) post(`/rooms/${joined.room}/leave`, { session }).catch(() => {}); return; }
        if (joined.participants) onJoined(joined);
        else Object.assign(room, { id: joined.room, code: joined.code, you: joined.you, state: "active" });
        window.PigeonpostCallsSummary?.joined(room.id); // Phase 7
      } else {
        const placed = await post("/calls", { session, from: me.address, group_id: group.id, kind });
        if (!room) { post(`/rooms/${placed.room}/leave`, { session }).catch(() => {}); return; }
        Object.assign(room, { id: placed.room, code: placed.code, you: placed.you, state: "ringing" });
        window.PigeonpostCallsSummary?.joined(room.id); // Phase 7
        setStatus("Ringing the group…");
        startTone("ringback");
      }
      renderStage();
    } catch (err) {
      endRoom(err.code === "not_member" ? "You are not an active member of this group." : err.code === "room_full" ? "This call is full (six people)." : err.code === "no_call" ? "That call has ended." : placeError(err), { notify: false });
    }
  }

  function onIncoming(data) {
    if (room || ring || window.PigeonpostLive?.active()) {
      post(`/rooms/${data.room}/decline`, { session, reason: "busy" }).catch(() => {});
      return;
    }
    ring = data;
    const from = data.from || {};
    const key = from.handle || from.address;
    // Phase 1c: answered already (from a lock screen, a notification, a link)?
    if (pendingAnswer && pendingAnswer.room === data.room) {
      const token = pendingAnswer.ringToken;
      pendingAnswer = null;
      acceptRing(token);
      return;
    }
    // An app that rings natively (CallKit, a system notification) shows it itself.
    if (tellHost("ring", { room: data.room, code: data.code, kind: data.kind, from: { address: from.address || null, handle: from.handle || null }, to: data.to || null }) === true) return;
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
        const n = new Notification("Incoming " + kindWord(data.kind) + " call", { body: key, tag: "pp-call-" + data.room, requireInteraction: true });
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

  async function acceptRing(ringToken) {
    const r = ring;
    if (!r) return;
    dismissRing();
    const peerKey = (r.from && (r.from.handle || r.from.address)) || "Call";
    tellHost("callState", { state: "connecting", room: r.room, kind: r.kind });
    openRoomUI({ title: callName(peerKey), sub: peerKey, face: peerKey, status: "Connecting…" });
    room = newRoom({ kind: r.kind, title: displayName(peerKey), peerKey, id: r.room, code: r.code });
    try {
      if (!await startLocalMedia(r.kind === "video")) return;
    } catch (err) {
      post(`/rooms/${r.room}/decline`, { session }).catch(() => {});
      endRoom(mediaMessage(err, r.kind), { notify: false });
      return;
    }
    if (!room) return;
    try {
      await post(`/rooms/${r.room}/accept`, Object.assign({ session, e2ee: canEncryptFrames }, typeof ringToken === "string" && RING_TOKEN_RE.test(ringToken) ? { ring_token: ringToken } : {}));
    } catch (err) {
      endRoom(err.code === "not_ringing" ? "That call has already ended." : err.code === "needs_e2ee" ? NEEDS_NEWER : err.code === "call_busy" ? admissionWords.call_busy : "The call could not be answered.", { notify: false });
    }
  }

  function declineRing() {
    const r = ring;
    if (!r) return;
    dismissRing();
    post(`/rooms/${r.room}/decline`, { session }).catch(() => {});
  }

  function newRoom(fields) {
    return Object.assign({ id: null, code: null, you: null, kind: "audio", state: "starting", startedAt: 0, timer: null, statsTimer: null, size: 1, topology: "mesh", members: new Map(), order: [], page: 0 }, fields);
  }

  // Who is in the call, as the call service lists them (in the order they came in), with the public
  // keys they announced in a large call. The room's security code is computed over exactly this.
  function noteMember(v) {
    if (!room || !v || !v.participant) return;
    const m = room.members.get(v.participant) || { id: v.participant, key: null, epoch: 0 };
    Object.assign(m, { name: v.name, mailbox: v.mailbox || null, guest: Boolean(v.guest) });
    if (v.key !== undefined && v.participant !== room.you) Object.assign(m, { key: v.key || null, epoch: v.epoch || 0 });
    room.members.set(v.participant, m);
    if (!room.order.includes(v.participant)) room.order.push(v.participant);
  }
  function forgetMember(id) {
    if (!room) return;
    room.members.delete(id);
    room.order = room.order.filter((x) => x !== id);
  }
  function setMembers(list) {
    if (!room || !Array.isArray(list)) return;
    const keep = new Set(list.map((v) => v.participant));
    for (const id of [...room.members.keys()]) if (!keep.has(id)) forgetMember(id);
    for (const v of list) noteMember(v);
    room.order.sort((a, b) => list.findIndex((v) => v.participant === a) - list.findIndex((v) => v.participant === b));
  }
  // After the event stream reconnects: catch up with whoever came, went or announced keys meanwhile.
  function resync(view) {
    if (!room || !view || view.room !== room.id) return;
    setMembers(view.participants);
    for (const p of [...peers.values()]) if (!room.members.has(p.id)) closePeer(p);
    for (const v of view.participants || []) if (v.participant !== room.you) ensurePeer(v, false);
    if (view.topology === "sfu" && room.topology !== "sfu") { room.topology = "sfu"; Sfu.enter(); }
    Sfu.onMembers();
    renderStage();
  }

  const admissionWords = {
    needs_e2ee: "someone’s browser or app can’t encrypt its own audio and video, which a call of more than six needs.",
    call_busy: "Pigeonpost’s media server is at capacity for large calls right now. Try again in a few minutes.",
    room_full: "the call is full.",
  };

  // The call service says who is here: this end has just been let in (by answering a ring, or by
  // somebody admitting a knock). The newcomer opens a connection to everyone already present.
  function onJoined(data) {
    if (!room) {
      if (PAGE !== "join") return;
      room = newRoom({ kind: data.kind, title: "Call", id: data.room });
    }
    Object.assign(room, { id: data.room, code: data.code, you: data.you, kind: data.kind || room.kind, state: "active", topology: data.topology || "mesh" });
    if (data.scheduled) {
      room.scheduled = data.scheduled;
      // A host may arrive after people started waiting. Bootstrap that snapshot once: the
      // joined SSE and HTTP response can both arrive, with live knock/resolution events between.
      if (!room.scheduledKnocksLoaded && data.scheduled.can_admit === true && Array.isArray(data.scheduled.knocks)) {
        room.scheduledKnocksLoaded = true;
        for (const k of data.scheduled.knocks.slice(0, 100)) {
          if (k && typeof k.knock === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(k.knock))
            knocks.set(k.knock, { room: data.room, knock: k.knock, name: k.name, mailbox: k.mailbox, guest: k.guest, verified: k.verified === true });
        }
      }
      $("call-lobby") && ($("call-lobby").hidden = true); renderScheduled();
    }
    setMembers(data.participants);
    tellHost("callState", { state: "active", room: data.room, kind: room.kind });
    stopTone();
    if (PAGE === "join") Join.enter();
    for (const p of data.participants || []) {
      if (p.participant === data.you) continue;
      // In a mesh the newcomer opens a connection to everyone; on the media server nobody does.
      ensurePeer(p, room.topology !== "sfu");
    }
    if (room.topology === "sfu") Sfu.enter();
    renderStage();
    window.PigeonpostCallsSummary?.joined(data.room); // Phase 7
  }

  // ---- local media -----------------------------------------------------------------------------

  // A chosen device is asked for *exactly*. As a mere preference it competes with the resolution
  // preference, and a 720p webcam loses to a 1080p built-in camera — the choice silently ignored.
  // Echo cancellation, noise suppression and gain control, and nothing that pins the capture format:
  // asking for a channel count or sample rate can steer the browser onto a capture path where its
  // echo canceller is bypassed.
  const AUDIO_CONSTRAINTS = (id) => Object.assign({
    echoCancellation: true, noiseSuppression: true, autoGainControl: true,
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

  // ---- background noise removal -----------------------------------------------------------------
  //
  // "Standard" is the browser's own noise suppression, always on. "Strong" adds RNNoise, a small
  // neural noise remover running locally in an AudioWorklet (vendored, MIT): fans, keyboards,
  // traffic and room hum are taken out before the voice leaves this device. If it cannot start, the
  // call carries on with the standard suppression.
  const canStrongNoise = Boolean(window.AudioWorkletNode && window.WebAssembly);
  const NOISE_BASE = "/vendor/web-noise-suppressor/";
  const noise = { mode: store.get("ppi_call_noise") || "strong", ctx: null, node: null, src: null, wasm: null, active: false };

  async function processMic(raw, isCurrent = () => true) {
    if (!isCurrent()) return raw;
    teardownNoise();
    if (noise.mode !== "strong" || !canStrongNoise) return raw;
    let ctx, src, node;
    const dispose = () => {
      try { src?.disconnect(); node?.disconnect(); node?.destroy?.(); ctx?.close().catch(() => {}); } catch (_) { /* closed */ }
    };
    try {
      const lib = await import(NOISE_BASE + "index.js");
      if (!isCurrent()) return raw;
      ctx = new AudioContext({ sampleRate: 48_000 });
      noise.wasm ||= await lib.loadRnnoise({ url: NOISE_BASE + "rnnoise.wasm", simdUrl: NOISE_BASE + "rnnoise_simd.wasm" });
      if (!isCurrent()) { dispose(); return raw; }
      await ctx.audioWorklet.addModule(NOISE_BASE + "rnnoise/workletProcessor.js");
      if (!isCurrent()) { dispose(); return raw; }
      src = ctx.createMediaStreamSource(new MediaStream([raw]));
      node = new lib.RnnoiseWorkletNode(ctx, { maxChannels: 1, wasmBinary: noise.wasm });
      const dest = ctx.createMediaStreamDestination();
      src.connect(node).connect(dest);
      await ctx.resume().catch(() => {});
      if (!isCurrent()) { dispose(); return raw; }
      Object.assign(noise, { ctx, node, src, active: true });
      const out = dest.stream.getAudioTracks()[0];
      out.contentHint = "speech";
      return out;
    } catch (err) {
      dispose();
      console.warn("calls: strong noise removal unavailable", err && err.name);
      if (isCurrent()) teardownNoise();
      return raw;
    }
  }

  function teardownNoise() {
    try { noise.src?.disconnect(); noise.node?.disconnect(); noise.node?.destroy?.(); noise.ctx?.close(); } catch (_) { /* closed */ }
    Object.assign(noise, { ctx: null, node: null, src: null, active: false });
  }

  // Put a (new) microphone into the call: process it, swap it in on every connection, release the
  // old one.
  async function setMicTrack(raw, isCurrent = () => true) {
    const oldRaw = local.rawMic, oldSent = local.mic;
    if (raw) raw.contentHint = "speech";
    const sent = raw ? await processMic(raw, isCurrent) : null;
    if (!isCurrent()) { raw?.stop(); if (sent !== raw) sent?.stop(); return false; }
    if (sent) sent.enabled = !local.muted;
    local.rawMic = raw;
    local.mic = sent;
    if (local.stream) {
      if (oldSent && oldSent !== sent) local.stream.removeTrack(oldSent);
      if (sent && !local.stream.getTracks().includes(sent)) local.stream.addTrack(sent);
    }
    updateSlot("audio");
    window.PigeonpostCallsSummary?.micChanged(); // Phase 7: notes record the track that is sent
    if (oldSent && oldSent !== sent && oldSent !== raw) oldSent.stop();
    if (oldRaw && oldRaw !== raw) oldRaw.stop();
    return true;
  }

  async function setNoiseMode(mode) {
    noise.mode = mode === "standard" ? "standard" : "strong";
    store.set("ppi_call_noise", noise.mode);
    if (local.rawMic) await setMicTrack(local.rawMic);
    renderNoise();
  }

  function renderNoise() {
    const select = $("call-noise");
    if (!select) return;
    select.value = noise.mode;
    $("call-noise-note").textContent = noise.mode === "strong"
      ? (noise.active ? "Removing background noise on this device." : canStrongNoise ? "Starts with your microphone." : "Not available in this browser — standard suppression is on.")
      : "Your browser’s standard noise suppression.";
  }

  async function startLocalMedia(withVideo) {
    const owner = room;
    const isCurrent = () => owner !== null && room === owner;
    let media;
    try {
      media = await capture({ audio: true, video: withVideo });
    } catch (err) {
      if (!isCurrent()) return false;
      // No camera, or the camera refused: still a call, just a voice one. A refused microphone ends
      // it — nobody can hear you.
      if (!withVideo) throw err;
      media = await capture({ audio: true, video: false });
      toast("No camera available — joining with audio only.");
    }
    if (!isCurrent()) { media.getTracks().forEach((track) => track.stop()); return false; }
    const captured = media.getTracks();
    captured.forEach((track) => pendingCaptureTracks.add(track));
    try {
      local.stream = new MediaStream();
      local.muted = false;
      local.cameraOff = false;
      if (!await setMicTrack(media.getAudioTracks()[0] || null, isCurrent) || !isCurrent()) {
        captured.forEach((track) => track.stop());
        return false;
      }
      local.cam = media.getVideoTracks()[0] || null;
      if (local.cam) local.stream.addTrack(local.cam);
      warnIfNoEchoCancellation();
      showSelf();
      renderControls();
      return true;
    } finally {
      captured.forEach((track) => pendingCaptureTracks.delete(track));
    }
  }

  function echoCancelling() {
    const track = local.rawMic || local.mic;
    const settings = track && track.getSettings ? track.getSettings() : {};
    return "echoCancellation" in settings ? Boolean(settings.echoCancellation) : null;
  }
  function warnIfNoEchoCancellation() {
    if (echoCancelling() === false) toast("Your browser isn’t cancelling echo on this microphone. Use headphones so the others don’t hear themselves.");
  }

  function stopLocalMedia() {
    for (const track of pendingCaptureTracks) track.stop();
    pendingCaptureTracks.clear();
    for (const t of [local.rawMic, local.mic, local.cam, local.screen]) t?.stop();
    teardownNoise();
    Object.assign(local, { stream: null, rawMic: null, mic: null, cam: null, screen: null, screenStream: null, muted: false, cameraOff: false });
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
        updateSlot("cam");
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
    window.PigeonpostCallsSummary?.muteChanged(); // Phase 7
    tellHost("muted", local.muted);
    broadcastState();
    showSelf();
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
      updateSlot("screen");
      broadcastState();
      applyBitrates();
      showSelf();
      showShareIndicators(track);
    } catch (err) {
      if (err && err.name !== "NotAllowedError") toast("Screen sharing couldn’t start.");
    }
    renderControls();
    renderStage();
  }

  function stopScreen() {
    const track = local.screen;
    if (!track) return;
    local.screen = null;
    local.screenStream = null;
    updateSlot("screen");
    track.stop();
    hideShareIndicators();
    showSelf();
    broadcastState();
    applyBitrates();
    renderControls();
    renderStage();
  }

  // ---- "you are sharing" indicators ------------------------------------------------------------
  //
  // A web page cannot draw on the screen outside its own window, so the border goes where a page
  // can put it: a red frame round this window, and — when a whole screen or another window is being
  // shared — a small always-on-top floating panel (Document Picture-in-Picture, Chrome and Edge)
  // with its own red frame and a Stop button, visible over every other app. A shared browser tab
  // is already outlined by the browser itself.
  let sharePip = null;

  function shareSurface(track) {
    return (track && track.getSettings && track.getSettings().displaySurface) || "monitor";
  }

  async function showShareIndicators(track) {
    document.documentElement.dataset.sharing = shareSurface(track);
    renderControls();
    if (shareSurface(track) !== "browser") await openSharePip();
  }

  async function openSharePip() {
    if (sharePip || !local.screen || !window.documentPictureInPicture) { renderControls(); return; }
    try {
      const pip = await documentPictureInPicture.requestWindow({ width: 360, height: 72 });
      const doc = pip.document;
      for (const link of document.querySelectorAll('link[rel="stylesheet"]')) {
        const copy = doc.createElement("link");
        copy.rel = "stylesheet";
        copy.href = link.href;
        doc.head.append(copy);
      }
      if (document.documentElement.dataset.theme) doc.documentElement.dataset.theme = document.documentElement.dataset.theme;
      doc.title = "Sharing your screen";
      doc.body.className = "share-pip";
      const stop = doc.createElement("button");
      stop.type = "button";
      stop.className = "call-pill call-pill-danger";
      stop.textContent = "Stop sharing";
      stop.addEventListener("click", () => stopScreen());
      const box = doc.createElement("div");
      box.className = "share-pip-box";
      const dot = doc.createElement("span");
      dot.className = "share-pip-dot";
      const text = doc.createElement("span");
      text.className = "share-pip-text";
      text.textContent = document.documentElement.dataset.sharing === "window" ? "You’re sharing a window" : "You’re sharing your screen";
      box.append(dot, text, stop);
      doc.body.append(box);
      pip.addEventListener("pagehide", () => { if (sharePip === pip) sharePip = null; renderControls(); });
      sharePip = pip;
    } catch (_) {
      // Browsers only open it in direct answer to a click; the banner offers that click.
    }
    renderControls();
  }

  function hideShareIndicators() {
    delete document.documentElement.dataset.sharing;
    const pip = sharePip;
    sharePip = null;
    try { pip?.close(); } catch (_) { /* already closed */ }
  }

  async function switchDevice(kind, deviceId) {
    if (kind === "speaker") {
      prefs.speaker = deviceId || null;
      for (const v of document.querySelectorAll(".call-ui audio")) {
        v.setSinkId?.(deviceId || "").catch(() => {});
      }
      return;
    }
    if (kind === "mic") prefs.mic = deviceId || null;
    if (kind === "cam") prefs.cam = deviceId || null;
    if (!local.stream) return;
    if (kind === "mic") {
      if (!local.rawMic) return;
      try {
        const media = await navigator.mediaDevices.getUserMedia({ audio: AUDIO_CONSTRAINTS(deviceId || null) });
        await setMicTrack(media.getAudioTracks()[0]);
        warnIfNoEchoCancellation();
      } catch (err) { toast(mediaMessage(err, "audio")); }
      fillDevices();
      renderNoise();
      return;
    }
    const old = local.cam;
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
    local.stream.removeTrack(old);
    local.stream.addTrack(fresh);
    old.stop();
    local.cam = fresh;
    updateSlot("cam");
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

  // Every pair of browsers has the same three media slots — voice, camera and screen — opened once,
  // by the side that started the pair (the newcomer). Turning a camera on, sharing a screen,
  // switching microphone or noise mode only swaps what goes into a slot; nothing is renegotiated.
  // And only that starting side ever makes an offer (the first one, and ICE restarts), so the two
  // ends of a pair can never both be offering at once — the collision that left pairs stuck on
  // slow links, where a small ICE message can overtake a large offer.
  const SLOTS = ["audio", "cam", "screen"];

  function ensurePeer(info, initiator) {
    let p = peers.get(info.participant);
    if (p) return p;
    p = {
      id: info.participant, name: info.name, mailbox: info.mailbox || null, guest: Boolean(info.guest),
      pc: null, dc: null, tx: null, makingOffer: false, offerAgain: false,
      pendingIce: [], remote: { camera: new MediaStream(), screen: new MediaStream(), audio: new MediaStream() },
      state: { muted: false, cameraOff: true, screen: false }, security: null, connected: false, dropTimer: null, ready: null,
      initiator, restarts: 0, watchdog: null, relayOnly: forceRelay,
      diag: { local: {}, remote: {}, reported: "" },
    };
    peers.set(p.id, p);
    room.size = peers.size + 1;
    if (initiator) p.ready = createConnection(p, true);
    applyBitrates();
    renderStage();
    return p;
  }

  function createConnection(p, initiator) {
    return (async () => {
      const pc = new RTCPeerConnection({
        iceServers: await turnServers(),
        iceTransportPolicy: p.relayOnly ? "relay" : "all",
        bundlePolicy: "max-bundle",
        rtcpMuxPolicy: "require",
      });
      p.pc = pc;
      // A negotiated channel exists on both ends without either having to announce it.
      p.dc = pc.createDataChannel("pigeonpost", { negotiated: true, id: 0, ordered: true });
      p.dc.onopen = () => sendState(p);
      p.dc.onmessage = (e) => onPeerMessage(p, e.data);
      pc.onicecandidate = (e) => {
        if (e.candidate && e.candidate.type) p.diag.local[e.candidate.type] = (p.diag.local[e.candidate.type] || 0) + 1;
        sendSignal(p, "ice", e.candidate ? e.candidate.toJSON() : null);
      };
      pc.ontrack = (e) => onRemoteTrack(p, e);
      pc.onconnectionstatechange = () => onConnectionState(p);
      pc.onnegotiationneeded = () => { if (p.initiator) negotiate(p).catch(() => {}); };
      if (initiator) {
        p.tx = {
          audio: pc.addTransceiver("audio", { direction: "sendrecv" }),
          cam: pc.addTransceiver("video", { direction: "sendrecv" }),
          screen: pc.addTransceiver("video", { direction: "sendrecv" }),
        };
        for (const slot of SLOTS) preferCodecs(p.tx[slot], slot === "audio" ? "audio" : "video");
        attachSlots(p);
      }
      return pc;
    })();
  }

  function slotTrack(slot) {
    return (slot === "audio" ? local.mic : slot === "cam" ? local.cam : local.screen) || null;
  }

  function attachSlots(p) {
    if (!p.tx) return;
    for (const slot of SLOTS) {
      const sender = p.tx[slot].sender, track = slotTrack(slot);
      if (sender.track !== track) sender.replaceTrack(track).catch(() => {});
    }
  }

  // Swap one slot's content on every connection: no offer, no answer. On the media server the slot
  // is a published track instead.
  function updateSlot(slot) {
    for (const p of peers.values()) {
      if (!p.tx) continue;
      const sender = p.tx[slot].sender, track = slotTrack(slot);
      if (sender.track !== track) sender.replaceTrack(track).catch(() => {});
    }
    Sfu.updateSlot(slot);
  }

  async function negotiate(p) {
    const pc = p.pc;
    if (!pc || pc.signalingState === "closed" || !p.initiator) return;
    // One offer in flight per pair; anything asked for meanwhile goes in the next one.
    if (p.makingOffer || pc.signalingState !== "stable") { p.offerAgain = true; return; }
    try {
      p.makingOffer = true;
      const offer = await pc.createOffer();
      if (pc.signalingState !== "stable") { p.offerAgain = true; return; }
      offer.sdp = tuneOpus(offer.sdp);
      await pc.setLocalDescription(offer);
      await sendSignal(p, "offer", { type: "offer", sdp: pc.localDescription.sdp });
      armWatchdog(p);
    } finally {
      p.makingOffer = false;
    }
  }

  async function onSignal(fromId, type, data) {
    if (!room) return;
    if (type === "key") return Sfu.onWrappedKey(fromId, data);
    const p = peers.get(fromId) || ensurePeer({ participant: fromId, name: "Someone" }, false);
    if (type === "mute") {
      if (!local.muted && local.mic) { toggleMute(); toast(displayName(p) + " muted you. Unmute when you want to speak."); }
      return;
    }
    if (type === "ice") {
      const kind = data && /\btyp (host|srflx|prflx|relay)\b/.exec(data.candidate || "");
      if (kind) p.diag.remote[kind[1]] = (p.diag.remote[kind[1]] || 0) + 1;
      // Candidates that overtook the offer wait for it; they never create the connection.
      if (!p.pc || !p.pc.remoteDescription) { p.pendingIce.push(data); return; }
      try { await p.pc.addIceCandidate(data || null); } catch (_) { /* a candidate for a discarded generation */ }
      return;
    }
    if (type === "offer") {
      if (p.initiator) return; // the starting side never answers
      if (!p.ready) p.ready = createConnection(p, false);
      const pc = await p.ready;
      await pc.setRemoteDescription(data);
      if (!p.tx) {
        const list = pc.getTransceivers();
        p.tx = { audio: list[0], cam: list[1], screen: list[2] };
        for (const slot of SLOTS) {
          if (!p.tx[slot]) continue;
          p.tx[slot].direction = "sendrecv";
          preferCodecs(p.tx[slot], slot === "audio" ? "audio" : "video");
        }
      }
      attachSlots(p);
      await flushIce(p);
      const answer = await pc.createAnswer();
      answer.sdp = tuneOpus(answer.sdp);
      await pc.setLocalDescription(answer);
      await sendSignal(p, "answer", { type: "answer", sdp: pc.localDescription.sdp });
      armWatchdog(p);
      applyBitrates();
      return;
    }
    if (type === "answer") {
      const pc = p.pc;
      if (!p.initiator || !pc || pc.signalingState !== "have-local-offer") return;
      await pc.setRemoteDescription(data);
      await flushIce(p);
      applyBitrates();
      if (p.offerAgain) { p.offerAgain = false; negotiate(p).catch(() => {}); }
    }
  }

  async function flushIce(p) {
    for (const c of p.pendingIce.splice(0)) {
      try { await p.pc.addIceCandidate(c || null); } catch (_) { /* stale */ }
    }
  }

  function sendSignal(p, type, data) {
    if (!room || !room.id || !session) return Promise.resolve();
    return post(`/rooms/${room.id}/signal`, { session, to: p.id, type, data }).catch(() => {});
  }

  // A received track is filed by its slot. Voices go to their own <audio> element; every <video>
  // stays silent, so a person is only ever played once.
  function onRemoteTrack(p, e) {
    const index = p.pc.getTransceivers().indexOf(e.transceiver);
    const slot = SLOTS[index] || (e.track.kind === "audio" ? "audio" : "cam");
    const target = slot === "audio" ? p.remote.audio : slot === "screen" ? p.remote.screen : p.remote.camera;
    if (!target.getTracks().includes(e.track)) target.addTrack(e.track);
    for (const ev of ["ended", "mute", "unmute"]) e.track.addEventListener(ev, () => renderStage());
    renderStage();
  }

  // A pair that has not connected after a while gets fresh ICE candidates (and fresh relay
  // allocations) rather than sitting on "connecting" for ever. Only the side that started the
  // connection restarts, so the two never both renegotiate at once.
  //
  // First a plain ICE restart. If that does not do it either, the pair falls back to the relay
  // only: two people on the same office or home network can often reach neither each other's local
  // address (the router isolates them) nor their shared public address (no hairpinning), and the
  // relay is the one path that always works.
  function armWatchdog(p) {
    if (p.watchdog || p.connected) return;
    const check = () => {
      p.watchdog = null;
      if (!peers.has(p.id) || p.connected || !p.pc || p.pc.signalingState === "closed") return;
      reportPair(p, "stalled");
      if (p.initiator && p.restarts < 4) {
        p.restarts += 1;
        if (p.restarts >= 2 && !p.relayOnly) {
          p.relayOnly = true;
          try { p.pc.setConfiguration(Object.assign(p.pc.getConfiguration(), { iceTransportPolicy: "relay" })); } catch (_) { /* older browsers keep "all" */ }
        }
        p.pc.restartIce?.();
      }
      p.watchdog = setTimeout(check, 10_000);
    };
    p.watchdog = setTimeout(check, 10_000);
  }

  // One line per pair in the call service's log — the outcome and how many candidates of each kind
  // were tried. Never addresses, never descriptions: enough to see why a pair failed to connect.
  function reportPair(p, state) {
    const key = state + JSON.stringify(p.diag.local) + JSON.stringify(p.diag.remote) + (p.stats?.route || "");
    if (!room || !room.id || !session || p.diag.reported === key) return;
    p.diag.reported = key;
    post(`/rooms/${room.id}/report`, { session, to: p.id, state, local: p.diag.local, remote: p.diag.remote, relay_only: Boolean(p.relayOnly), restarts: p.restarts, route: p.stats?.route || null }).catch(() => {});
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
        setTimeout(() => { sampleStats().then(() => reportPair(p, "connected")).catch(() => {}); }, 2500);
      }
      renderClock();
      renderStage();
      return;
    }
    if (s === "disconnected" || s === "failed") {
      // Moving to the media server: the other end closed its side of the direct connection.
      if (room && room.topology === "sfu") { Sfu.closeMesh(p); renderStage(); return; }
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
    clearTimeout(p.meshTimer);
    try { p.dc?.close(); } catch (_) { /* closed */ }
    try { p.pc?.close(); } catch (_) { /* closed */ }
    for (const t of [...p.remote.camera.getTracks(), ...p.remote.screen.getTracks(), ...p.remote.audio.getTracks()]) t.stop();
    if (p.audioEl) { p.audioEl.srcObject = null; p.audioEl.remove(); }
    if (room && room.tiles) for (const [id, tile] of room.tiles) if (id.startsWith(p.id + ":")) { tile.remove(); room.tiles.delete(id); }
    peers.delete(p.id);
    if (room) room.size = peers.size + 1;
    applyBitrates();
  }

  // ---- what travels on the data channel ---------------------------------------------------------

  const stateMessage = () => ({ t: "state", muted: local.muted, cameraOff: !local.cam || local.cameraOff, screen: Boolean(local.screen), aec: echoCancelling() });
  function sendState(p) {
    if (!p.dc || p.dc.readyState !== "open") return;
    p.dc.send(JSON.stringify(stateMessage()));
  }
  function broadcastState() {
    for (const p of peers.values()) sendState(p);
    Sfu.send(stateMessage());
  }

  function onPeerMessage(p, raw) {
    if (typeof raw !== "string" || raw.length > 8_000) return;
    let m;
    try { m = JSON.parse(raw); } catch (_) { return; }
    if (m.t === "state") {
      p.state = { muted: Boolean(m.muted), cameraOff: Boolean(m.cameraOff), screen: Boolean(m.screen), aec: m.aec === false ? false : m.aec === true ? true : null };
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

  function muteOther(p) {
    if (!room || !p) return;
    sendSignal(p, "mute", null);
    p.state.muted = true;
    renderStage();
    renderPeople();
    toast(displayName(p) + " is muted. Only they can unmute themselves.");
  }

  function sendChat(text) {
    const value = String(text || "").trim().slice(0, CHAT_CHARS);
    if (!value || !room) return false;
    let delivered = 0;
    // On the media server, chat goes to everyone at once, sealed with the room key.
    const viaSfu = Sfu.ready();
    if (viaSfu) { Sfu.send({ t: "chat", text: value }); delivered = [...peers.values()].filter((p) => p.lk).length; }
    else for (const p of peers.values()) {
      if (p.dc && p.dc.readyState === "open") { p.dc.send(JSON.stringify({ t: "chat", text: value })); delivered++; }
    }
    if (!delivered) { toast("Nobody else is connected yet."); return false; }
    const missed = [...peers.values()].filter((p) => (viaSfu ? !p.lk : !(p.dc && p.dc.readyState === "open"))).map((p) => displayName(p));
    chatLog.push({ who: "You", mine: true, text: value, at: Date.now(), missed });
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
      if (!p.tx) continue;
      for (const slot of SLOTS) {
        const sender = p.tx[slot] && p.tx[slot].sender;
        if (!sender) continue;
        const params = sender.getParameters();
        if (!params.encodings || !params.encodings.length) continue;
        const enc = params.encodings[0];
        if (slot === "screen") {
          enc.maxBitrate = screen; enc.maxFramerate = 15; params.degradationPreference = "maintain-resolution";
        } else if (slot === "cam") {
          enc.maxBitrate = camera; enc.maxFramerate = 30; params.degradationPreference = "balanced";
        } else {
          enc.maxBitrate = AUDIO_BPS;
        }
        enc.priority = slot === "audio" ? "high" : "medium";
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

  // ---- large calls: the media server ----------------------------------------------------------
  //
  // More than six people: one connection to the media server instead of one per person. Everything
  // sent through it is encrypted in this browser first (LiveKit's frame encryption, in a worker), with
  // a room key from calls-e2ee.js. The earliest participant holding the newest room key makes a fresh
  // key whenever the set of participants changes and wraps it to each of the others; a new key is
  // used for sending KEY_SWITCH_MS after it arrives, so everyone can already open it. Remote tracks
  // that are not end-to-end encrypted are never played.
  const Sfu = (() => {
    let s = null;
    const E = () => window.PigeonpostCallsE2EE;
    const L = () => window.LivekitClient;
    const active = () => Boolean(s && room && room.id === s.roomId && !s.left);
    const me = () => (room && room.members.get(room.you)) || null;

    function loadScript(src, integrity) {
      return new Promise((resolve, reject) => {
        const tag = document.createElement("script");
        tag.src = src;
        if (integrity) { tag.integrity = integrity; tag.crossOrigin = "anonymous"; }
        tag.onload = () => resolve();
        tag.onerror = () => reject(new Error("could not load " + src));
        document.head.append(tag);
      });
    }
    let libs = null;
    function loadLibs() {
      if (window.PigeonpostCallsE2EE && window.LivekitClient) return Promise.resolve();
      libs ||= Promise.all([
        window.PigeonpostCallsE2EE ? null : loadScript(E2EE_SCRIPT),
        window.LivekitClient ? null : loadScript(LIVEKIT_BASE + "livekit-client.umd.js", LIVEKIT_INTEGRITY),
      ]).catch((err) => { libs = null; throw err; });
      return libs;
    }

    async function enter() {
      if (!room || (s && s.roomId === room.id)) return;
      s = {
        roomId: room.id, you: room.you, identity: null, lk: null, keys: null, worker: null,
        epoch: 0, sendEpoch: 0, keyFrom: null, keyed: false, dataKeys: new Map(), lastSet: null,
        announced: { key: null, epoch: -1 }, rotateTimer: null, switchTimer: null, codeTimer: null, syncTimer: null,
        code: null, connecting: null, connected: false, pubs: {}, retries: 0, decryptErrors: 0, unencrypted: 0,
        rotations: 0, meshSweep: null, left: false,
      };
      const mine = s;
      if (!canEncryptFrames) { endRoom(NEEDS_NEWER, { notify: true }); return; }
      setStatus("Securing the call…");
      try {
        await loadLibs();
        if (s !== mine || !active()) return;
        mine.identity = await E().generateIdentity();
      } catch (err) {
        console.warn("calls: large-call encryption unavailable", err && err.name);
        if (s === mine && active()) endRoom("This call couldn’t start its encryption. Check your connection and try again.", { notify: true });
        return;
      }
      if (s !== mine || !active()) return;
      const m = me();
      if (m) Object.assign(m, { key: mine.identity.publicKey, epoch: 0 });
      createRoom(mine);
      await announce();
      onMembers();
    }

    // Tell the others (through the call service) our public key and which room key we hold.
    async function announce() {
      if (!active() || !s.identity || !session) return;
      const want = { key: s.identity.publicKey, epoch: s.epoch };
      if (s.announced.key === want.key && s.announced.epoch === want.epoch) return;
      try {
        await post(`/rooms/${s.roomId}/e2ee`, { session, public_key: want.key, epoch: want.epoch });
        if (active()) s.announced = want;
      } catch (err) {
        console.warn("calls: key announcement failed", err && err.code);
        if (active()) setTimeout(() => announce(), 2000);
      }
    }

    function createRoom(mine) {
      const LK = L();
      class RoomKeys extends LK.BaseKeyProvider {
        constructor() { super({ sharedKey: true, ratchetWindowSize: 0, failureTolerance: -1 }); }
        // Known for opening others' frames, not yet used for sealing ours. `material` is the raw room
        // key; calls-e2ee-worker.js imports it inside the worker.
        stage(material, slot) { this.emit(LK.KeyProviderEvent.SetKey, { key: material, participantIdentity: undefined, keyIndex: slot }, false); }
        use(material, slot) { this.onSetEncryptionKey(material, undefined, slot); }
      }
      mine.keys = new RoomKeys();
      mine.worker = new Worker(E2EE_WORKER);
      const lk = new LK.Room({
        adaptiveStream: false, dynacast: true, stopLocalTrackOnUnpublish: false, disconnectOnPageLeave: true,
        e2ee: { keyProvider: mine.keys, worker: mine.worker },
        publishDefaults: {
          simulcast: true, videoSimulcastLayers: [LK.VideoPresets.h180, LK.VideoPresets.h360], videoCodec: "vp8",
          videoEncoding: { maxBitrate: 1_700_000, maxFramerate: 30 },
          screenShareEncoding: { maxBitrate: 2_500_000, maxFramerate: 15 },
          dtx: false, red: false, audioPreset: { maxBitrate: 48_000 },
        },
      });
      mine.lk = lk;
      const R = LK.RoomEvent;
      const mineOnly = (fn) => (...args) => { if (s === mine && active()) fn(...args); };
      lk.on(R.ParticipantConnected, mineOnly((rp) => { attach(rp); scheduleState(); }))
        .on(R.ParticipantDisconnected, mineOnly((rp) => detach(rp)))
        .on(R.TrackPublished, mineOnly(() => scheduleSync()))
        .on(R.TrackUnpublished, mineOnly(() => scheduleSync()))
        .on(R.TrackSubscribed, mineOnly((track, pub, rp) => onTrack(track, pub, rp, true)))
        .on(R.TrackUnsubscribed, mineOnly((track, pub, rp) => onTrack(track, pub, rp, false)))
        .on(R.ActiveSpeakersChanged, mineOnly((list) => onSpeakers(list)))
        .on(R.DataReceived, mineOnly((payload, rp) => onData(payload, rp)))
        .on(R.EncryptionError, mineOnly(() => { mine.decryptErrors += 1; }))
        .on(R.Reconnecting, mineOnly(() => setStatus("Reconnecting…")))
        .on(R.Reconnected, mineOnly(() => renderClock()))
        .on(R.Disconnected, mineOnly(() => onDisconnected(mine)));
    }

    // ---- room keys ----

    // The earliest participant holding the newest room key generation; before anyone holds one, the
    // earliest with a public key.
    function leader() {
      const list = room.order.map((id) => room.members.get(id)).filter((m) => m && m.key);
      if (!list.length) return null;
      const top = Math.max(...list.map((m) => m.epoch || 0));
      return list.find((m) => (m.epoch || 0) === top);
    }

    function keyedSet() {
      return room.order.filter((id) => room.members.get(id)?.key).map((id) => id + ":" + room.members.get(id).key).join("|");
    }

    function onMembers() {
      if (!active() || !s.identity) return;
      announce();
      updateCode();
      const l = leader();
      if (!l || l.id !== room.you) return;
      if (keyedSet() === s.lastSet) return;
      clearTimeout(s.rotateTimer);
      const mine = s;
      // A moment's grace, so that several arrivals at once make one new key, not several.
      mine.rotateTimer = setTimeout(() => { if (s === mine && active()) rotate(mine).catch((err) => note(mine, "rotate", err)); }, 250);
    }

    // The last thing that went wrong, for inspect(): names and messages only, never keys.
    function note(mine, where, err) {
      mine.lastError = where + ": " + String((err && (err.message || err.name)) || err).slice(0, 160);
      console.warn("calls: " + mine.lastError);
    }

    async function rotate(mine) {
      const l = leader();
      if (!l || l.id !== room.you) return;
      const set = keyedSet();
      if (set === mine.lastSet) return;
      mine.lastSet = set;
      const epoch = mine.epoch + 1;
      const roomKey = E().randomRoomKey();
      const from = { id: room.you, key: mine.identity.publicKey };
      mine.rotations += 1;
      await install(mine, epoch, roomKey, room.you);
      await Promise.all(room.order.filter((id) => id !== room.you).map(async (id) => {
        const m = room.members.get(id);
        if (!m || !m.key) return;
        const wrapped = await E().wrapRoomKey({ privateKey: mine.identity.privateKey, roomId: mine.roomId, epoch, from, to: { id, key: m.key }, roomKey });
        await post(`/rooms/${mine.roomId}/signal`, { session, to: id, type: "key", data: wrapped }).catch(() => {});
      }));
    }

    async function onWrappedKey(fromId, wrapped) {
      if (!active() || !s.identity || !wrapped) return;
      const mine = s;
      const from = room.members.get(fromId);
      if (!from || !from.key) return;
      const rank = (id) => { const i = room.order.indexOf(id); return i < 0 ? Infinity : i; };
      // Newer generations win; two "leaders" briefly making the same generation resolve to the earlier one.
      if (wrapped.epoch < mine.epoch || (wrapped.epoch === mine.epoch && rank(fromId) >= rank(mine.keyFrom))) return;
      let roomKey;
      try {
        roomKey = await E().unwrapRoomKey({ privateKey: mine.identity.privateKey, roomId: mine.roomId, from: { id: fromId, key: from.key }, to: { id: room.you, key: mine.identity.publicKey }, wrapped });
      } catch (err) {
        note(mine, "unwrap", err);
        return;
      }
      if (s !== mine || !active() || wrapped.epoch < mine.epoch) return;
      await install(mine, wrapped.epoch, roomKey, fromId);
      onMembers();
    }

    async function install(mine, epoch, roomKey, fromId) {
      const slot = E().slotFor(epoch);
      const material = roomKey.slice().buffer;
      mine.dataKeys.set(epoch, await E().dataKey(roomKey));
      for (const e of mine.dataKeys.keys()) if (e <= epoch - E().KEY_SLOTS) mine.dataKeys.delete(e);
      const first = !mine.keyed;
      Object.assign(mine, { epoch, keyFrom: fromId, keyed: true });
      const m = me();
      if (m) m.epoch = epoch;
      clearTimeout(mine.switchTimer);
      if (first) {
        mine.keys.use(material, slot);
        mine.sendEpoch = epoch;
      } else {
        mine.keys.stage(material, slot);
        mine.switchTimer = setTimeout(() => {
          if (s !== mine || !active() || mine.epoch !== epoch) return;
          mine.keys.use(material, slot);
          mine.sendEpoch = epoch;
          broadcastState();
        }, KEY_SWITCH_MS);
      }
      announce();
      updateCode();
      if (!mine.connecting) connect(mine);
    }

    // The room's security code, recomputed whenever someone comes, goes or changes key.
    function updateCode() {
      if (!active()) return;
      const mine = s;
      clearTimeout(mine.codeTimer);
      mine.codeTimer = setTimeout(async () => {
        const list = room.order.map((id) => room.members.get(id)).filter(Boolean);
        try {
          const code = await E().securityCode(mine.roomId, list);
          if (s !== mine || !active()) return;
          mine.code = list.filter((m) => m.key).length >= 2 ? code : null;
          renderStage();
          renderPeople();
        } catch (_) { /* recomputed on the next change */ }
      }, 150);
    }

    // ---- the connection ----

    function connect(mine) {
      mine.connecting = (async () => {
        await mine.lk.setE2EEEnabled(true);
        const ticket = await post(`/rooms/${mine.roomId}/sfu`, { session });
        const ice = await turnServers();
        const url = /^wss?:\/\//.test(ticket.url) ? ticket.url : location.origin.replace(/^http/, "ws") + ticket.url;
        await mine.lk.connect(url, ticket.token, { autoSubscribe: false, rtcConfig: { iceServers: ice, iceTransportPolicy: forceRelay ? "relay" : "all" } });
        if (s !== mine || !active()) { mine.lk.disconnect(); return; }
        mine.connected = true;
        mine.retries = 0;
        for (const slot of SLOTS) await publish(mine, slot);
        for (const rp of mine.lk.remoteParticipants.values()) attach(rp);
        if (!room.startedAt) {
          room.startedAt = Date.now();
          room.timer = setInterval(renderClock, 1000);
          room.statsTimer = setInterval(sampleStats, 2000);
        }
        renderClock();
        // Direct connections that the media server has not replaced within this time are closed anyway.
        mine.meshSweep = setTimeout(() => { if (s === mine) for (const p of peers.values()) closeMesh(p); renderStage(); }, 20_000);
        broadcastState();
        scheduleSync();
      })().catch((err) => {
        note(mine, "connect", err);
        mine.connecting = null;
        if (s !== mine || !active()) return;
        if (err && err.code === "not_in_room") return;
        if (++mine.retries > 4) { endRoom("Couldn’t connect to the call’s media server. Check your connection and try again.", { notify: true }); return; }
        setTimeout(() => { if (s === mine && active() && !mine.connecting) connect(mine); }, 1500 * mine.retries);
      });
      return mine.connecting;
    }

    function onDisconnected(mine) {
      mine.connected = false;
      mine.connecting = null;
      mine.pubs = {};
      for (const p of peers.values()) dropSfuTracks(p);
      renderStage();
      // LiveKit has already tried to resume. Rejoin with a fresh ticket (same participant, same key).
      if (++mine.retries > 4) { endRoom("Lost the connection to the call’s media server.", { notify: true }); return; }
      setStatus("Reconnecting…");
      setTimeout(() => { if (s === mine && active() && !mine.connecting) connect(mine); }, 1000 * mine.retries);
    }

    const SOURCE = { audio: "microphone", cam: "camera", screen: "screen_share" };

    // LiveKit publishes encrypted video as simulcast only where it recognises Safari 17.2 or later
    // (older WebKit mixed the layers; https://bugs.webkit.org/show_bug.cgi?id=257803). WebKit web
    // views on macOS carry no Safari version in their user agent, so there it sends one layer. That
    // layer then goes to everyone, so it is kept small: 360p at most, about 0.6 Mbit/s.
    const singleLayer = (() => {
      const ua = navigator.userAgent.toLowerCase();
      if (/firefox|fxios|chrom|crios|crmo/.test(ua) || !/safari|applewebkit/.test(ua)) return false;
      const atLeast172 = (m) => Boolean(m) && (Number(m[1]) > 17 || (Number(m[1]) === 17 && Number(m[2]) >= 2));
      if (ua.includes("mobile/") && atLeast172(/\(.+?(\d+)_(\d+)/.exec(ua))) return false;
      return !atLeast172(/version\/(\d+)\.(\d+)/.exec(ua));
    })();
    const SMALL_VIDEO = { width: { ideal: 640 }, height: { ideal: 360 }, frameRate: { ideal: 24, max: 30 } };

    async function publish(mine, slot) {
      const LK = L();
      const track = slotTrack(slot);
      const current = mine.pubs[slot];
      try {
        if (!track) {
          if (current) { delete mine.pubs[slot]; await mine.lk.localParticipant.unpublishTrack(current.track, false); }
          return;
        }
        if (slot === "cam" && singleLayer) await track.applyConstraints(SMALL_VIDEO).catch(() => {});
        if (current && current.track) {
          if (current.track.mediaStreamTrack !== track) await current.track.replaceTrack(track, true);
          return;
        }
        const source = { audio: LK.Track.Source.Microphone, cam: LK.Track.Source.Camera, screen: LK.Track.Source.ScreenShare }[slot];
        const options = { source, name: SOURCE[slot] };
        if (slot === "cam" && singleLayer) Object.assign(options, { simulcast: false, videoEncoding: { maxBitrate: 600_000, maxFramerate: 24 } });
        mine.pubs[slot] = await mine.lk.localParticipant.publishTrack(track, options);
      } catch (err) {
        note(mine, "publish " + slot, err);
      }
    }

    function updateSlot(slot) {
      if (active() && s.connected) publish(s, slot);
    }

    // ---- the others ----

    function attach(rp) {
      const p = peers.get(rp.identity);
      if (!p) return; // not (yet) in the call service's list: nothing of theirs is shown
      p.lk = rp;
      if (!p.pc) p.connected = true;
      scheduleSync();
      renderStage();
    }

    function detach(rp) {
      const p = peers.get(rp.identity);
      if (!p || p.lk !== rp) return;
      p.lk = null;
      dropSfuTracks(p);
      if (!p.pc) p.connected = false;
      renderStage();
    }

    function dropSfuTracks(p) {
      if (!p.sfuTracks) return;
      for (const [slot, t] of Object.entries(p.sfuTracks)) if (t) { streamFor(p, slot).removeTrack(t); }
      p.sfuTracks = {};
    }

    const streamFor = (p, slot) => (slot === "audio" ? p.remote.audio : slot === "screen" ? p.remote.screen : p.remote.camera);
    const slotOf = (source) => (source === "microphone" ? "audio" : source === "camera" ? "cam" : source === "screen_share" ? "screen" : null);

    function onTrack(track, pub, rp, subscribed) {
      const p = peers.get(rp.identity);
      const slot = slotOf(pub.source);
      if (!p || !slot) return;
      p.sfuTracks ||= {};
      if (subscribed) {
        // Only media this browser had to decrypt is ever played. A track the media server describes
        // as unencrypted is refused, whoever it claims to come from.
        if (!pub.isEncrypted) {
          s.unencrypted += 1;
          pub.setSubscribed(false);
          if (s.unencrypted === 1) toast("A stream that wasn’t end-to-end encrypted was refused.");
          return;
        }
        p.sfuTracks[slot] = track.mediaStreamTrack;
        for (const ev of ["ended", "mute", "unmute"]) track.mediaStreamTrack.addEventListener(ev, () => renderStage());
        if (!p.pc) addSfuTrack(p, slot);
        else if (slot === "audio" && s.keyed) scheduleMeshClose(p);
      } else {
        const t = p.sfuTracks[slot];
        if (t) { streamFor(p, slot).removeTrack(t); delete p.sfuTracks[slot]; }
      }
      renderStage();
    }

    function addSfuTrack(p, slot) {
      const t = p.sfuTracks && p.sfuTracks[slot];
      const stream = streamFor(p, slot);
      if (t && !stream.getTracks().includes(t)) stream.addTrack(t);
    }

    // During the move from a mesh: once someone's voice arrives through the media server, their
    // direct connection is closed (after a moment, so their voice never stops), and the media
    // server's tracks take over.
    function scheduleMeshClose(p) {
      if (p.meshTimer || !p.pc) return;
      p.meshTimer = setTimeout(() => { p.meshTimer = null; closeMesh(p); renderStage(); }, 1500);
    }

    function closeMesh(p) {
      if (!p.pc && !p.dc) return;
      clearTimeout(p.dropTimer); clearTimeout(p.watchdog); clearTimeout(p.meshTimer);
      const pc = p.pc, dc = p.dc;
      Object.assign(p, { pc: null, dc: null, tx: null, dropTimer: null, watchdog: null, meshTimer: null, ready: null });
      for (const stream of [p.remote.audio, p.remote.camera, p.remote.screen]) {
        for (const t of stream.getTracks()) if (!Object.values(p.sfuTracks || {}).includes(t)) { stream.removeTrack(t); t.stop(); }
      }
      try { dc?.close(); } catch (_) { /* closed */ }
      try { pc?.close(); } catch (_) { /* closed */ }
      for (const slot of SLOTS) addSfuTrack(p, slot);
      p.connected = Boolean(p.lk);
    }

    // Which tracks this browser receives: every voice; cameras only on the page being shown (the
    // stage view at a high layer, the rest at the lowest); shared screens always.
    function scheduleSync() {
      if (!active()) return;
      clearTimeout(s.syncTimer);
      s.syncTimer = setTimeout(() => renderStage(), 50);
    }

    function sync(views, stage) {
      if (!active() || !s.connected) return;
      const LK = L();
      const shown = new Set(views.map((v) => v.id));
      const quality = views.length <= 2 ? LK.VideoQuality.HIGH : views.length <= 6 ? LK.VideoQuality.MEDIUM : LK.VideoQuality.LOW;
      let adopted = false;
      for (const rp of s.lk.remoteParticipants.values()) {
        const p = peers.get(rp.identity);
        // Connected to the media server before the call service told us they were in: take them in now.
        if (p && p.lk !== rp) { p.lk = rp; if (!p.pc) p.connected = true; adopted = true; }
        for (const pub of rp.trackPublications.values()) {
          if (!p || !pub.isEncrypted) { if (pub.isDesired) pub.setSubscribed(false); continue; }
          const slot = slotOf(pub.source);
          const viewId = rp.identity + (slot === "screen" ? ":screen" : ":cam");
          const want = slot === "audio" || slot === "screen" || (slot === "cam" && shown.has(viewId));
          if (pub.isDesired !== want) pub.setSubscribed(want);
          const q = stage ? (stage.id === viewId ? LK.VideoQuality.HIGH : LK.VideoQuality.LOW) : quality;
          if (want && slot !== "audio" && pub.ppQuality !== q) { pub.ppQuality = q; pub.setVideoQuality(q); }
        }
      }
      if (adopted) scheduleSync();
    }

    function onSpeakers(list) {
      let moved = false;
      for (const sp of list || []) if (!sp.isLocal && bringOnPage(sp.identity + ":cam")) moved = true;
      if (moved) renderStage();
    }

    // ---- chat and status, sealed with the room key ----

    let stateTimer = null;
    function scheduleState() { clearTimeout(stateTimer); stateTimer = setTimeout(() => broadcastState(), 300); }

    async function send(message) {
      if (!active() || !s.connected || !s.sendEpoch) return false;
      const mine = s;
      try {
        const key = mine.dataKeys.get(mine.sendEpoch);
        const sealed = await E().sealData(key, { epoch: mine.sendEpoch, roomId: mine.roomId, senderId: room.you }, message);
        await mine.lk.localParticipant.publishData(sealed, { reliable: true });
        return true;
      } catch (err) {
        console.warn("calls: sending to the call failed", err && err.name);
        return false;
      }
    }

    async function onData(payload, rp) {
      if (!rp) return;
      const p = peers.get(rp.identity);
      if (!p) return;
      const mine = s;
      try {
        const { value } = await E().openData(async (epoch) => mine.dataKeys.get(epoch) || null, { roomId: mine.roomId, senderId: rp.identity }, payload);
        onPeerMessage(p, JSON.stringify(value));
      } catch (_) { /* not sealed with a key we hold: dropped */ }
    }

    // ---- leaving ----

    function leave(ending) {
      if (!s || !ending || s.roomId !== ending.id) return;
      const mine = s;
      s = null;
      mine.left = true;
      for (const t of [mine.rotateTimer, mine.switchTimer, mine.codeTimer, mine.syncTimer, mine.meshSweep]) clearTimeout(t);
      try { mine.lk?.disconnect(); } catch (_) { /* gone */ }
      setTimeout(() => { try { mine.worker?.terminate(); } catch (_) { /* gone */ } }, 1000);
    }

    async function stats() {
      if (!s || !s.lk) return null;
      const out = [];
      for (const rp of s.lk.remoteParticipants.values()) {
        for (const pub of rp.trackPublications.values()) {
          const entry = { from: rp.identity, source: pub.source, encrypted: pub.isEncrypted, subscribed: pub.isSubscribed };
          const receiver = pub.track && pub.track.receiver;
          if (receiver) {
            const report = await receiver.getStats();
            report.forEach((r) => {
              if (r.type === "inbound-rtp") Object.assign(entry, { bytes: r.bytesReceived, frames: r.framesDecoded, width: r.frameWidth, height: r.frameHeight, audioLevel: r.audioLevel, samples: r.totalSamplesReceived, concealed: r.concealedSamples });
            });
          }
          out.push(entry);
        }
      }
      // What this browser sends: one entry per simulcast layer.
      for (const [slot, pub] of Object.entries(s.pubs)) {
        const sender = pub && pub.track && pub.track.sender;
        if (!sender) continue;
        const report = await sender.getStats();
        report.forEach((r) => {
          if (r.type === "outbound-rtp") out.push({ from: "local", source: slot, rid: r.rid || null, bytes: r.bytesSent, frames: r.framesEncoded, width: r.frameWidth, height: r.frameHeight, fps: r.framesPerSecond, limitation: r.qualityLimitationReason || null, encoder: r.encoderImplementation || null, active: r.active });
        });
      }
      return out;
    }

    function inspect() {
      if (!s) return null;
      return {
        connected: s.connected, epoch: s.epoch, sendEpoch: s.sendEpoch, keyFrom: s.keyFrom, code: s.code, rotations: s.rotations, lastError: s.lastError || null,
        leader: room && leader() ? leader().id : null, decryptErrors: s.decryptErrors, unencrypted: s.unencrypted,
        publicKey: s.identity ? s.identity.publicKey : null,
        published: Object.keys(s.pubs).filter((k) => s.pubs[k]), singleLayer,
        remote: s.lk ? s.lk.remoteParticipants.size : 0,
      };
    }

    return {
      loadLibs, singleLayer, // Phase 6: live streams reuse the vendored client and the WebKit check
      enter, onMembers, onWrappedKey, updateSlot, sync, send, leave, closeMesh, stats, inspect,
      ready: () => Boolean(active() && s.connected && s.sendEpoch),
      code: () => (active() ? s.code : null),
    };
  })();

  // ---- leaving ---------------------------------------------------------------------------------

  function leave() {
    if (room) endRoom(null, { notify: true });
  }

  function endRoom(message, { notify }) {
    const ending = room;
    if (!ending) return;
    room = null;
    tellHost("callState", { state: "ended", room: ending.id, kind: ending.kind });
    stopTone();
    clearInterval(ending.timer);
    clearInterval(ending.statsTimer);
    if (notify && ending.state === "waiting" && ending.knock && session) post("/knocks/withdraw", { session, knock: ending.knock }).catch(() => {}); // Phase 3B
    else if (notify && ending.id && session) post(`/rooms/${ending.id}/leave`, { session }).catch(() => {});
    Sfu.leave(ending);
    window.PigeonpostCallsSummary?.left(); // Phase 7: capture stops with the call
    for (const p of [...peers.values()]) closePeer(p);
    hideShareIndicators();
    stopLocalMedia();
    knocks.clear();
    chatLog.length = 0;
    iceServers = null;
    const ui = $("call-ui");
    if (ui) {
      for (const v of ui.querySelectorAll("video")) { v.pause?.(); v.srcObject = null; }
      if (isFullscreen()) (document.exitFullscreen || document.webkitExitFullscreen).call(document);
      ui.hidden = true;
      if ($("call-mini")) $("call-mini").hidden = true;
      $("call-panel").hidden = true;
      $("call-strip").textContent = "";
      for (const t of [...$("call-grid").children]) if (t.id !== "call-strip") t.remove();
      $("call-knocks").textContent = "";
    }
    renderHead();
    const last = ending.startedAt ? " · " + clock(Date.now() - ending.startedAt) : "";
    if (message) toast(message);
    else if (notify && ending.startedAt) toast("Call ended" + last + ".");
    if (PAGE === "join") Join.left(message);
    if (PAGE === "meeting") window.PigeonpostMeetingPage?.left?.(message); // Phase 3B
    if ($("call-host-tools")) $("call-host-tools").hidden = true; // Phase 3B
    if ($("call-lobby")) $("call-lobby").hidden = true; // Phase 3B
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

  // How tall the header's own content is (its buttons, the name and status) and how tall the
  // controls dock is, as --call-top-content and --call-dock-h. The stylesheet adds the paddings and
  // safe areas to make --call-top-h and --call-controls-h, which the stage, the tiles' pin and mute
  // buttons and your own view keep clear of. Measured rather than assumed: they change with the
  // width (a phone stacks the header), the font and zoom.
  function watchLayout(ui) {
    const top = ui.querySelector(".call-top");
    const dock = ui.querySelector(".call-controls");
    if (!top) return;
    const measure = () => {
      let bottom = 0;
      for (const child of top.children) {
        if (child.offsetHeight) bottom = Math.max(bottom, child.offsetTop + child.offsetHeight);
      }
      const pad = parseFloat(getComputedStyle(top).paddingTop) || 0;
      if (bottom) ui.style.setProperty("--call-top-content", Math.ceil(bottom - pad) + "px");
      if (dock && dock.offsetHeight) ui.style.setProperty("--call-dock-h", Math.ceil(dock.offsetHeight) + "px");
    };
    if (typeof ResizeObserver !== "undefined") {
      const observer = new ResizeObserver(measure);
      for (const node of [top, dock, ...top.children]) if (node) observer.observe(node, { box: "border-box" });
    }
    window.addEventListener("resize", measure);
    measure();
  }

  // Every picture in its real shape. A phone sends portrait while upright and landscape on its
  // side, and switches mid-call when turned; a laptop sends landscape. What arrives is read from the
  // video itself (videoWidth/videoHeight, again on every "resize", which is how a turned phone shows
  // up), never assumed. A tile fills itself (object-fit: cover) only while that costs at most a
  // quarter of the picture — a 16:9 camera in a 4:3 tile — and otherwise shows all of it
  // (data-fit="contain"), so a landscape speaker is never cut down to a portrait strip or a portrait
  // one to a band across their face. Your own view takes your camera's shape (--self-ar).
  const FIT_MAX_CROP = 0.25;
  const fitting = typeof ResizeObserver !== "undefined" ? new ResizeObserver((entries) => {
    // A tile taken off the stage reports once, at zero size: let it go then.
    for (const e of entries) if (e.target.isConnected) fitVideo(e.target); else fitting.unobserve(e.target);
  }) : null;
  function fitVideo(video) {
    const w = video.videoWidth, h = video.videoHeight;
    if (!w || !h) return;
    const shape = w > h ? "landscape" : w < h ? "portrait" : "square";
    if (video.id === "call-self-video") {
      const box = $("call-self");
      if (!box) return;
      box.style.setProperty("--self-ar", w + " / " + h);
      box.dataset.shape = shape;
      return;
    }
    video.dataset.shape = shape;
    video.style.setProperty("--video-ar", w + " / " + h);
    const bw = video.clientWidth, bh = video.clientHeight;
    if (!bw || !bh) return;
    const a = w / h, b = bw / bh;
    video.dataset.fit = 1 - Math.min(a, b) / Math.max(a, b) > FIT_MAX_CROP ? "contain" : "cover";
  }
  function watchShape(video) {
    const fit = () => fitVideo(video);
    video.addEventListener("loadedmetadata", fit);
    video.addEventListener("resize", fit);
    if (fitting) fitting.observe(video);
  }

  // Your own view, in whichever corner you put it. Drag it (finger, pen or mouse) and it snaps to
  // the nearest corner; with it focused, the arrow keys move it between corners. The corner is
  // remembered on this device. The corners themselves are the stylesheet's ([data-corner]): clear
  // of the header, the controls dock and the safe areas.
  const CORNERS = ["tl", "tr", "bl", "br"];
  const SELF_CORNER_KEY = "ppi_call_self_corner";
  function savedCorner() {
    const value = store.get(SELF_CORNER_KEY);
    return CORNERS.includes(value) ? value : "br";
  }
  function placeSelf(box, corner, from) {
    if (!CORNERS.includes(corner)) return;
    const before = from || box.getBoundingClientRect();
    box.dataset.corner = corner;
    store.set(SELF_CORNER_KEY, corner);
    // Slide from where it was to the corner: the corners are different properties (top or bottom,
    // left or right), which cannot be transitioned, so the move is played as a transform.
    const after = box.getBoundingClientRect();
    const dx = before.left - after.left, dy = before.top - after.top;
    box.classList.remove("call-self-dragging");
    const still = window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    if (still || (!dx && !dy)) { box.style.transform = ""; return; }
    box.style.transition = "none";
    box.style.transform = `translate(${dx}px, ${dy}px)`;
    void box.offsetWidth;
    box.style.transition = "transform .18s ease-out";
    box.style.transform = "";
    box.addEventListener("transitionend", () => { box.style.transition = ""; }, { once: true });
  }
  function movableSelf(box) {
    const main = box.parentElement;
    let drag = null;
    box.addEventListener("pointerdown", (e) => {
      if (e.button !== 0 && e.pointerType === "mouse") return;
      drag = { id: e.pointerId, x: e.clientX, y: e.clientY, moved: false };
      try { box.setPointerCapture(e.pointerId); } catch (_) { /* not capturable */ }
    });
    box.addEventListener("pointermove", (e) => {
      if (!drag || e.pointerId !== drag.id) return;
      const dx = e.clientX - drag.x, dy = e.clientY - drag.y;
      if (!drag.moved && Math.hypot(dx, dy) < 6) return;
      drag.moved = true;
      box.classList.add("call-self-dragging");
      box.style.transition = "none";
      box.style.transform = `translate(${dx}px, ${dy}px)`;
      e.preventDefault();
    });
    const end = (e) => {
      if (!drag || e.pointerId !== drag.id) return;
      const moved = drag.moved;
      drag = null;
      try { box.releasePointerCapture(e.pointerId); } catch (_) { /* released */ }
      if (!moved) return; // a tap: nothing more than it ever did
      const at = box.getBoundingClientRect();
      const area = main.getBoundingClientRect();
      const left = at.left + at.width / 2 < area.left + area.width / 2;
      const upper = at.top + at.height / 2 < area.top + area.height / 2;
      placeSelf(box, (upper ? "t" : "b") + (left ? "l" : "r"), at);
    };
    box.addEventListener("pointerup", end);
    box.addEventListener("pointercancel", end);
    box.addEventListener("keydown", (e) => {
      const corner = box.dataset.corner || "br";
      let v = corner[0], h = corner[1];
      if (e.key === "ArrowUp") v = "t";
      else if (e.key === "ArrowDown") v = "b";
      else if (e.key === "ArrowLeft") h = "l";
      else if (e.key === "ArrowRight") h = "r";
      else return;
      e.preventDefault();
      if (v + h !== corner) placeSelf(box, v + h);
    });
  }

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
            host() ? el("button", { class: "call-back", id: "call-minimize", type: "button", title: "Back to the inbox — the call keeps going", "aria-label": "Back to the inbox (the call keeps going)", onclick: () => minimize() }, svg(ICON.back), el("span", { text: "Inbox" })) : null,
            // Who the call is with, as one compact pill: the name, the time, and a chevron. The rest —
            // handle, quality and route, and the security code — opens under it on a tap.
            el("div", { class: "call-who" },
              el("button", { class: "call-who-pill", id: "call-pill", type: "button", "aria-expanded": "false", "aria-controls": "call-details",
                title: "Call details and security code", onclick: () => toggleDetails() },
                el("h2", { class: "call-who-name", id: "call-name" }),
                el("span", { class: "call-who-time", id: "call-status", "aria-live": "polite" }),
                svg(ICON.chevron)),
              el("div", { class: "call-details", id: "call-details", role: "region", "aria-label": "Call details", hidden: true },
                el("p", { class: "call-handle", id: "call-handle" }),
                el("p", { class: "call-quality", id: "call-quality" }),
                el("div", { class: "call-sas", id: "call-sas", hidden: true },
                  el("p", { class: "call-sas-row" }, svg(ICON.lock), el("span", { text: "Security code" }), el("strong", { id: "call-sas-code" })),
                  el("ul", { class: "call-sas-list", id: "call-sas-list" }),
                  el("p", { class: "call-sas-note", id: "call-sas-note", text: "Compare this code with the other person. If it matches on both screens, the call is end-to-end encrypted between your two devices." })),
              ),
            ),
            el("div", { class: "call-top-right" },
              el("div", { class: "call-pager", id: "call-pager", role: "group", "aria-label": "Pages of people", hidden: true },
                el("button", { class: "call-icon-btn call-top-btn", id: "call-page-prev", type: "button", "aria-label": "Previous page of people", title: "Previous page", onclick: () => turnPage(-1) }, svg(ICON.back)),
                el("span", { class: "call-page-label", id: "call-page-label", "aria-live": "polite" }),
                el("button", { class: "call-icon-btn call-top-btn call-page-next", id: "call-page-next", type: "button", "aria-label": "Next page of people", title: "Next page", onclick: () => turnPage(1) }, svg(ICON.back))),
              el("button", { class: "call-back", id: "call-layout", type: "button", hidden: true, onclick: () => toggleLayout() }, svg(ICON.people), el("span", { text: "Grid" })),
              // Phase 3B: a scheduled meeting's host may extend it or end it for everyone.
              el("div", { class: "call-host-tools", id: "call-host-tools", role: "group", "aria-label": "Host controls", hidden: true },
                el("button", { class: "call-pill", id: "call-extend-15", type: "button", text: "+15 min", title: "Keep the meeting open 15 minutes longer", onclick: () => extendScheduled(15) }),
                el("button", { class: "call-pill", id: "call-extend-30", type: "button", text: "+30 min", title: "Keep the meeting open 30 minutes longer", onclick: () => extendScheduled(30) }),
                el("button", { class: "call-pill call-pill-danger", id: "call-end-all", type: "button", text: "End for everyone", onclick: () => endScheduled() })),
              (document.fullscreenEnabled || document.webkitFullscreenEnabled) ? el("button", { class: "call-icon-btn call-top-btn", id: "call-fullscreen", type: "button", "aria-label": "Full screen", title: "Full screen", onclick: () => toggleFullscreen() }, svg(ICON.fullscreen)) : null,
            ),
          ),
          el("div", { class: "call-knocks", id: "call-knocks", "aria-live": "polite" }),
          el("div", { class: "call-grid", id: "call-grid", "data-count": "0", "data-layout": "grid" }, el("div", { class: "call-strip", id: "call-strip" })),
          el("div", { class: "call-waiting", id: "call-waiting" }, el("span", { class: "avatar call-avatar", id: "call-face", "aria-hidden": "true" }),
            el("p", { class: "call-lobby", id: "call-lobby", role: "status", hidden: true, text: "Waiting for the host to let you in…" })), // Phase 3B
          el("div", { class: "call-self", id: "call-self", "data-screen": "false", "data-corner": savedCorner(), tabindex: "0",
            role: "group", "aria-label": "Your video. Drag it, or use the arrow keys, to move it to another corner." },
            el("video", { id: "call-self-screen", class: "call-self-screen", autoplay: true, playsinline: true, muted: true }),
            el("video", { id: "call-self-video", class: "call-self-camera", autoplay: true, playsinline: true, muted: true }),
            el("span", { class: "call-mute-badge call-self-mute", id: "call-self-mute", hidden: true, title: "You’re muted", "aria-label": "You’re muted" }, svg(ICON.mic + ICON.slash)),
            el("span", { class: "call-self-label", id: "call-self-label", text: "You" })),
          el("div", { class: "call-audio", id: "call-audio", "aria-hidden": "true" }),
          el("div", { class: "call-inner-toast", id: "call-inner-toast", role: "status", "aria-live": "polite", hidden: true }),
          el("div", { class: "call-presenting", id: "call-presenting", hidden: true },
            el("span", { class: "share-pip-dot", "aria-hidden": "true" }),
            el("span", { text: "You’re sharing your screen" }),
            el("button", { type: "button", class: "call-pill", id: "call-share-pip", text: "Show floating indicator", hidden: true, onclick: () => openSharePip() }),
            el("button", { type: "button", class: "call-pill call-pill-danger", text: "Stop sharing", onclick: () => stopScreen() })),
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
            el("p", { class: "call-panel-note", text: "Messages are end-to-end encrypted, like the call itself. They aren’t saved." }),
          ),
          el("div", { class: "call-panel-body", id: "call-panel-people" },
            el("section", { class: "call-invite" },
              el("p", { class: "call-panel-label", text: "Invite link" }),
              el("div", { class: "call-link-row" }, el("code", { id: "call-link" }), el("button", { class: "call-pill", type: "button", id: "call-link-copy" }, svg(ICON.link), el("span", { text: "Copy" }))),
              el("p", { class: "call-panel-note", id: "call-invite-note", text: "Anyone with the link can ask to join. Someone in the call has to let them in. Up to 30 people." }),
            ),
            el("ul", { class: "call-knock-list", id: "call-knock-list" }),
            el("p", { class: "call-panel-label", text: "In the call" }),
            el("ul", { class: "call-people-list", id: "call-people-list" }),
          ),
          el("div", { class: "call-panel-body", id: "call-panel-settings" },
            deviceField("mic", "Microphone"),
            el("label", { class: "call-field", for: "call-noise" },
              el("span", { class: "call-panel-label", text: "Noise cancellation" }),
              el("select", { id: "call-noise", class: "call-select", onchange: (e) => setNoiseMode(e.target.value) },
                el("option", { value: "strong", text: "Strong — remove background noise" }),
                el("option", { value: "standard", text: "Standard" })),
              el("span", { class: "call-panel-note", id: "call-noise-note" })),
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
    document.body.append(ui, ringCard, miniBar());
    if (!host()) document.body.append(el("div", { class: "toast", id: "call-toast", role: "status", "aria-live": "polite", hidden: true }));
    watchLayout(ui);
    movableSelf($("call-self"));
    watchShape($("call-self-video"));

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
    if (view === "settings") { fillDevices(); renderNoise(); }
    renderControls();
  }

  function meetingLink() {
    if (room && room.code && room.scheduled) return location.origin + "/m/" + room.code; // Phase 3B
    return room && room.code ? location.origin + "/" + room.code : "";
  }

  async function copyLink() {
    const link = meetingLink();
    if (!link) return;
    try {
      await navigator.clipboard.writeText(link);
      toast("Copied to clipboard.");
      const label = $("call-link-copy").querySelector("span");
      label.textContent = "Copied";
      clearTimeout(copyLink.timer);
      copyLink.timer = setTimeout(() => { label.textContent = "Copy"; }, 2000);
    }
    catch (_) {
      const range = document.createRange();
      range.selectNodeContents($("call-link"));
      getSelection().removeAllRanges();
      getSelection().addRange(range);
      toast("Select and copy the link.");
    }
  }

  // ---- back to the inbox during a call ----------------------------------------------------------

  function miniBar() {
    if (!host()) return null;
    const btn = (id, label, icon, cls = "") => el("button", { id, class: "call-mini-btn " + cls, type: "button", "aria-label": label, title: label }, svg(icon));
    const bar = el("div", { class: "call-mini", id: "call-mini", role: "region", "aria-label": "Call in progress", hidden: true },
      el("button", { class: "call-mini-open", id: "call-mini-open", type: "button", title: "Return to the call", onclick: () => restore() },
        el("span", { class: "avatar", id: "call-mini-face", "aria-hidden": "true" }),
        el("span", { class: "call-mini-text" }, el("span", { class: "call-mini-name", id: "call-mini-name" }), el("span", { class: "call-mini-status", id: "call-mini-status" })),
        el("span", { class: "call-badge", id: "call-mini-badge", hidden: true }),
      ),
      btn("call-mini-mute", "Mute microphone", ICON.mic + ICON.slash),
      btn("call-mini-expand", "Return to the call", ICON.expand),
      btn("call-mini-end", "Leave call", ICON.end, "call-mini-end"),
    );
    bar.querySelector("#call-mini-mute").addEventListener("click", toggleMute);
    bar.querySelector("#call-mini-expand").addEventListener("click", () => restore());
    bar.querySelector("#call-mini-end").addEventListener("click", leave);
    return bar;
  }

  function minimize() {
    if (!room || !host()) return;
    room.minimized = true;
    if (isFullscreen()) (document.exitFullscreen || document.webkitExitFullscreen).call(document);
    $("call-ui").hidden = true;
    $("call-mini").hidden = false;
    renderMini();
    $("call-mini-open").focus({ preventScroll: true });
  }

  function restore() {
    if (!room) return;
    room.minimized = false;
    $("call-mini").hidden = true;
    $("call-ui").hidden = false;
    showSelf();
    renderStage();
    $("call-hangup").focus({ preventScroll: true });
  }

  function renderMini() {
    const bar = $("call-mini");
    if (!bar || !room) return;
    const list = [...peers.values()];
    const title = list.length > 1 ? (list.length + 1) + " people" : list.length === 1 ? displayName(list[0]) : $("call-name").textContent;
    $("call-mini-name").textContent = title;
    $("call-mini-status").textContent = (room.startedAt ? clock(Date.now() - room.startedAt) : $("call-status").textContent) + (local.muted ? " · muted" : "") + (local.screen ? " · sharing" : "");
    paintFace($("call-mini-face"), list.length === 1 ? (list[0].mailbox ? (list[0].mailbox.handle || list[0].mailbox.address) : list[0].name) : title);
    const m = $("call-mini-mute");
    m.setAttribute("aria-pressed", String(local.muted));
    m.setAttribute("aria-label", local.muted ? "Unmute microphone" : "Mute microphone");
    m.title = m.getAttribute("aria-label");
    const waiting = knocks.size + (room.unread || 0);
    $("call-mini-badge").hidden = !waiting;
    $("call-mini-badge").textContent = String(waiting);
  }

  function openRoomUI({ title, sub, face, status }) {
    buildUI();
    $("call-name").textContent = title;
    $("call-handle").textContent = sub || "";
    paintFace($("call-face"), face || title);
    $("call-quality").textContent = "";
    $("call-sas").hidden = true;
    toggleDetails(false);
    $("call-ui").hidden = false;
    if ($("call-mini")) $("call-mini").hidden = true;
    setStatus(status || "");
    renderControls();
    renderHead();
    $("call-hangup").focus({ preventScroll: true });
  }

  // Your own view: your camera, and while you share, the screen the others are seeing.
  function showSelf() {
    const v = $("call-self-video");
    if (!v) return;
    v.muted = true;
    if (v.srcObject !== local.stream) v.srcObject = local.stream;
    if (v.paused) v.play().catch(() => {});
    const sv = $("call-self-screen");
    sv.muted = true;
    const screen = local.screenStream || null;
    if (sv.srcObject !== screen) sv.srcObject = screen;
    if (screen && sv.paused) sv.play().catch(() => {});
    const camOn = Boolean(local.cam && !local.cameraOff);
    const box = $("call-self");
    box.dataset.screen = String(Boolean(screen));
    box.dataset.camera = String(camOn);
    box.hidden = !(camOn || screen);
    $("call-self-label").textContent = screen ? "Your screen — what the others see" : "You";
    $("call-self-mute").hidden = !local.muted;
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
    $("call-share-pip").hidden = !(local.screen && !sharePip && window.documentPictureInPicture && document.documentElement.dataset.sharing !== "browser");
    $("call-share").disabled = !room || !room.id || peers.size === 0;
    if (room && room.minimized) renderMini();
  }

  function setStatus(text) { const s = $("call-status"); if (s) s.textContent = text; }
  function renderClock() { if (room && room.startedAt) setStatus(clock(Date.now() - room.startedAt)); if (room && room.minimized) renderMini(); }

  // Views: each person's camera, plus each shared screen as a view of its own. A pinned view — or,
  // if nothing is pinned, a shared screen — takes the stage and the rest line up in a strip beside
  // it. "Grid" puts everything back side by side.
  function viewsFor(list) {
    const views = [];
    for (const p of list) {
      views.push({ id: p.id + ":cam", kind: "cam", p });
      // The screen slot always exists; it is a view only while that person says they are sharing.
      if (p.state.screen && p.remote.screen.getVideoTracks().some((t) => t.readyState === "live")) views.push({ id: p.id + ":screen", kind: "screen", p });
    }
    return views;
  }

  function tileFor(view) {
    room.tiles ||= new Map();
    let tile = room.tiles.get(view.id);
    if (tile) return tile;
    const pin = el("button", { class: "call-tile-act call-pin", type: "button" }, svg(ICON.pin));
    pin.addEventListener("click", (e) => { e.stopPropagation(); togglePin(view.id); });
    const picture = el("video", { class: "call-tile-video", autoplay: true, playsinline: true, muted: true });
    watchShape(picture);
    const children = [picture];
    if (view.kind === "cam") {
      children.push(el("span", { class: "avatar call-tile-face", "aria-hidden": "true" }));
      children.push(el("span", { class: "call-mute-badge", hidden: true, title: "Muted", "aria-label": "Muted" }, svg(ICON.mic + ICON.slash)));
    }
    const acts = el("div", { class: "call-tile-acts" }, pin);
    if (view.kind === "cam") {
      const mute = el("button", { class: "call-tile-act call-tile-mute", type: "button" }, svg(ICON.mic + ICON.slash));
      mute.addEventListener("click", (e) => { e.stopPropagation(); muteOther(peers.get(view.p.id)); });
      acts.append(mute);
    }
    children.push(acts, el("figcaption", { class: "call-tile-name" }, el("span", { class: "call-tile-label" })));
    tile = el("figure", { class: "call-tile", "data-view": view.id, "data-kind": view.kind }, ...children);
    // A thumbnail in the strip is the quickest way to put someone on the stage.
    tile.addEventListener("click", () => { if (tile.parentElement && tile.parentElement.id === "call-strip") togglePin(view.id); });
    room.tiles.set(view.id, tile);
    return tile;
  }

  // The details under the call's name pill: open on a tap or Enter/Space (it is a button), closed by
  // another tap, Escape or a tap anywhere else.
  function toggleDetails(open) {
    const pill = $("call-pill"), details = $("call-details");
    if (!pill || !details) return;
    const next = open === undefined ? pill.getAttribute("aria-expanded") !== "true" : Boolean(open);
    pill.setAttribute("aria-expanded", String(next));
    details.hidden = !next;
    pill.closest(".call-top").classList.toggle("call-top-open", next);
  }
  document.addEventListener("pointerdown", (e) => {
    const pill = $("call-pill");
    if (!pill || pill.getAttribute("aria-expanded") !== "true") return;
    if (e.target.closest && e.target.closest(".call-who")) return;
    toggleDetails(false);
  });
  document.addEventListener("keydown", (e) => {
    const pill = $("call-pill");
    if (e.key !== "Escape" || !pill || pill.getAttribute("aria-expanded") !== "true") return;
    toggleDetails(false);
    pill.focus();
  });

  function isFullscreen() { return Boolean(document.fullscreenElement || document.webkitFullscreenElement); }
  function toggleFullscreen() {
    const ui = $("call-ui");
    if (isFullscreen()) (document.exitFullscreen || document.webkitExitFullscreen).call(document);
    else (ui.requestFullscreen || ui.webkitRequestFullscreen)?.call(ui)?.catch?.(() => {});
  }
  function renderFullscreen() {
    const b = $("call-fullscreen");
    if (!b) return;
    const on = isFullscreen();
    b.replaceChildren(svg(on ? ICON.unfullscreen : ICON.fullscreen));
    b.setAttribute("aria-label", on ? "Exit full screen" : "Full screen");
    b.title = b.getAttribute("aria-label");
  }
  document.addEventListener("fullscreenchange", renderFullscreen);
  document.addEventListener("webkitfullscreenchange", renderFullscreen);

  function togglePin(id) {
    if (!room) return;
    room.pinned = room.pinned === id ? null : id;
    if (room.pinned) room.layout = "auto";
    renderStage();
  }

  function toggleLayout() {
    if (!room) return;
    room.layout = room.layout === "grid" ? "auto" : "grid";
    if (room.layout === "grid") room.pinned = null;
    renderStage();
  }

  function renderStage() {
    if (!room || !$("call-ui")) return;
    room.tiles ||= new Map();
    const grid = $("call-grid");
    const strip = $("call-strip");
    const list = [...peers.values()];
    const all = viewsFor(list);
    if (room.pinned && !all.some((v) => v.id === room.pinned)) room.pinned = null;
    const spotlightView = room.layout === "grid" ? null
      : all.find((v) => v.id === room.pinned) || all.find((v) => v.kind === "screen") || null;
    // A large call shows one page of views at a time; the stage view is always on it.
    const views = pageOf(all, spotlightView);
    const spotlight = Boolean(spotlightView && views.length > 1);
    grid.dataset.layout = spotlight ? "spotlight" : "grid";
    grid.dataset.count = String(views.length);
    $("call-waiting").hidden = list.some((p) => p.connected);
    if (list.length === 1 && !list[0].connected) setStatus(room.state === "ringing" ? "Ringing…" : "Connecting…");

    const wanted = new Set();
    for (const view of views) {
      const p = view.p;
      const tile = tileFor(view);
      wanted.add(view.id);
      const video = tile.querySelector(".call-tile-video");
      const stream = view.kind === "screen" ? p.remote.screen : p.remote.camera;
      video.muted = true;
      if (video.srcObject !== stream) video.srcObject = stream;
      const isMain = spotlight && view === spotlightView;
      const parent = !spotlight || isMain ? grid : strip;
      if (tile.parentElement !== parent) parent.append(tile);
      if (video.paused) video.play().catch(() => {});
      const live = stream.getVideoTracks().some((t) => t.readyState === "live" && !t.muted) && (view.kind === "screen" || !p.state.cameraOff);
      tile.dataset.video = String(live);
      tile.dataset.connected = String(p.connected);
      tile.dataset.main = String(isMain);
      tile.dataset.pinned = String(room.pinned === view.id);
      const name = displayName(p);
      tile.querySelector(".call-tile-label").textContent = view.kind === "screen" ? name + "’s screen" : name + (p.guest ? " · guest" : "") + (p.connected ? "" : " · connecting");
      const pin = tile.querySelector(".call-pin");
      const pinned = room.pinned === view.id;
      pin.setAttribute("aria-pressed", String(pinned));
      pin.setAttribute("aria-label", (pinned ? "Unpin " : "Pin ") + (view.kind === "screen" ? name + "’s screen" : name));
      pin.title = pin.getAttribute("aria-label");
      if (view.kind === "cam") {
        paintFace(tile.querySelector(".call-tile-face"), p.mailbox ? (p.mailbox.handle || p.mailbox.address) : p.name);
        tile.querySelector(".call-mute-badge").hidden = !p.state.muted;
        const mute = tile.querySelector(".call-tile-mute");
        mute.hidden = p.state.muted;
        mute.setAttribute("aria-label", "Mute " + name);
        mute.title = "Mute " + name;
      }
    }
    // The stage first, the strip in arrival order behind it.
    if (spotlight) {
      const main = room.tiles.get(spotlightView.id);
      if (grid.firstElementChild !== main) grid.prepend(main);
      if (strip.parentElement !== grid) grid.append(strip);
    }
    for (const [id, tile] of room.tiles) if (!wanted.has(id)) { tile.querySelector("video").srcObject = null; tile.remove(); room.tiles.delete(id); }

    // Voices, one <audio> per person, independent of any tile.
    for (const p of list) {
      if (!p.audioEl) {
        p.audioEl = el("audio", { autoplay: true });
        $("call-audio").append(p.audioEl);
      }
      if (p.audioEl.srcObject !== p.remote.audio) {
        p.audioEl.srcObject = p.remote.audio;
        if (prefs.speaker) p.audioEl.setSinkId?.(prefs.speaker).catch(() => {});
      }
      if (p.audioEl.paused) p.audioEl.play().catch(() => {});
    }

    // The header names the call: the other person, or the first two and how many more.
    if (list.length > 1) {
      const names = list.map((p) => callName(p));
      $("call-name").textContent = names.slice(0, 2).join(", ") + (names.length > 2 ? " +" + (names.length - 2) : "");
      $("call-handle").textContent = (list.length + 1) + " people: you, " + names.join(", ");
    } else if (list.length === 1 && PAGE === "join") {
      $("call-name").textContent = callName(list[0]);
      $("call-handle").textContent = list[0].mailbox ? (list[0].mailbox.handle || list[0].mailbox.address) : "";
    }
    const solo = list.length === 1 ? list[0] : null;
    // On the media server one code covers the whole room; in a mesh, the code with the one other
    // person, or (a group in a mesh) one code per person.
    const sfu = room.topology === "sfu";
    const code = sfu ? Sfu.code() : solo && solo.security ? solo.security.code : null;
    const coded = sfu ? [] : list.filter((p) => p.security);
    $("call-sas").hidden = !(code || (!solo && coded.length));
    $("call-sas-code").textContent = code || "";
    const sasList = $("call-sas-list");
    sasList.replaceChildren(...(code ? [] : coded.map((p) => el("li", {}, el("span", { text: callName(p) }), el("strong", { text: p.security.code })))));
    $("call-sas-note").textContent = sfu
      ? "Compare this code with everyone in the call. If it is the same on every screen, the call is end-to-end encrypted between all of you."
      : solo
        ? "Compare this code with the other person. If it matches on both screens, the call is end-to-end encrypted between your two devices."
        : "Each person has their own code. Compare yours with each of them: if it matches on both screens, your connection to them is end-to-end encrypted.";
    renderPager();
    const layoutBtn = $("call-layout");
    layoutBtn.hidden = all.length < 2;
    layoutBtn.setAttribute("aria-pressed", String(room.layout === "grid"));
    layoutBtn.querySelector("span").textContent = room.layout === "grid" ? "Spotlight" : "Grid";
    layoutBtn.title = room.layout === "grid" ? "Put the pinned person or shared screen on the stage" : "Show everyone side by side";
    $("call-link").textContent = meetingLink();
    renderControls();
    if (!$("call-panel").hidden && $("call-panel").dataset.view === "people") renderPeople();
    Sfu.sync(views, spotlight ? spotlightView : null);
  }

  // Large calls: which views are on screen. The order stays put as people come and go; whoever starts
  // speaking off the page takes the place of the last view on the current one.
  function pageOf(all, stage) {
    room.pages = 1;
    if (all.length <= PAGE_SIZE) { room.page = 0; return all; }
    const rest = all.filter((v) => v !== stage);
    const ids = new Set(rest.map((v) => v.id));
    room.viewOrder = (room.viewOrder || []).filter((id) => ids.has(id));
    for (const v of rest) if (!room.viewOrder.includes(v.id)) room.viewOrder.push(v.id);
    const per = PAGE_SIZE - (stage ? 1 : 0);
    room.pages = Math.ceil(rest.length / per);
    room.page = Math.min(Math.max(0, room.page || 0), room.pages - 1);
    const byId = new Map(rest.map((v) => [v.id, v]));
    const page = room.viewOrder.slice(room.page * per, room.page * per + per).map((id) => byId.get(id));
    return stage ? [stage, ...page] : page;
  }

  function bringOnPage(viewId) {
    if (!room || !room.viewOrder || (room.pages || 1) < 2) return false;
    const i = room.viewOrder.indexOf(viewId);
    const per = PAGE_SIZE - (room.layout !== "grid" && (room.pinned || [...peers.values()].some((p) => p.state.screen)) ? 1 : 0);
    const start = room.page * per, end = start + per;
    if (i < 0 || (i >= start && i < end)) return false;
    const last = Math.min(end, room.viewOrder.length) - 1;
    [room.viewOrder[i], room.viewOrder[last]] = [room.viewOrder[last], room.viewOrder[i]];
    return true;
  }

  function renderPager() {
    const pager = $("call-pager");
    if (!pager) return;
    const pages = room ? room.pages || 1 : 1;
    pager.hidden = pages < 2;
    if (pages < 2) return;
    $("call-page-label").textContent = (room.page + 1) + " / " + pages;
    $("call-page-prev").disabled = room.page === 0;
    $("call-page-next").disabled = room.page >= pages - 1;
  }

  function turnPage(delta) {
    if (!room) return;
    room.page = Math.min(Math.max(0, (room.page || 0) + delta), (room.pages || 1) - 1);
    renderStage();
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
    const answer = (admit) => post(`/rooms/${room.id}/knocks/${k.knock}`, { session, admit }).catch((err) => { if (err.code === "room_full") toast("The call is full."); else if (err.code === "not_allowed") toast("Only the people the organizer chose can let others in."); });
    const canAdmit = !room.scheduled || Boolean(room.scheduled.can_admit); // Phase 3B
    // (Whether a person who was let in fits is decided then; `knock_resolved` says why not.)
    return el("div", { class: cls, role: "group", "aria-label": who + " wants to join" },
      face,
      el("div", { class: "call-knock-text" },
        el("p", { class: "call-knock-name", text: who }),
        el("p", { class: "call-knock-note", text: k.mailbox ? "Pigeonpost member · wants to join" : k.verified ? "Invited guest · address confirmed · wants to join" : "Guest · name not verified · wants to join" }),
      ),
      canAdmit ? el("button", { class: "call-pill", type: "button", text: "Deny", onclick: () => answer(false) }) : null,
      canAdmit ? el("button", { class: "call-pill call-pill-primary", type: "button", text: "Let in", onclick: () => answer(true) }) : el("span", { class: "call-knock-note", text: "A host will answer" }),
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
    const you = row("You", (me ? (me.handle || me.address) : (Join.name || "Guest")) + (local.muted ? " · muted" : "") + (echoCancelling() === false ? " · no echo cancellation — use headphones" : ""), room && room.topology === "sfu" ? Sfu.code() : null, me ? (me.handle || me.address) : Join.name);
    if (room && room.topology === "sfu") you.querySelector(".call-person-code")?.setAttribute("title", "Security code for the whole call. If everyone reads the same code, the call is end-to-end encrypted between all of you.");
    list.append(you);
    for (const p of peers.values()) {
      const key = p.mailbox ? (p.mailbox.handle || p.mailbox.address) : p.name;
      const notes = [p.guest ? "Guest · name not verified" : key];
      if (!p.connected) notes.push(room && room.topology === "sfu" && !p.lk && Sfu.ready() ? "not connected yet" : "connecting");
      else if (p.stats?.route === "relay") notes.push("relayed");
      if (p.state.muted) notes.push("muted");
      if (p.state.aec === false) notes.push("no echo cancellation — ask them to use headphones");
      const r = row(displayName(p), notes.join(" · "), p.security?.code, key);
      // Phase 3B: in a scheduled meeting only hosts and co-hosts may mute others.
      if (room && room.scheduled && !["host", "cohost"].includes(room.scheduled.role)) { list.append(r); continue; }
      const mute = el("button", { class: "call-pill call-person-mute", type: "button", text: p.state.muted ? "Muted" : "Mute", disabled: p.state.muted, "aria-label": p.state.muted ? displayName(p) + " is muted" : "Mute " + displayName(p) });
      mute.addEventListener("click", () => muteOther(p));
      r.append(mute);
      list.append(r);
    }
  }

  function renderChat() {
    const log = $("call-chat-log");
    if (!log) return;
    log.textContent = "";
    for (const m of chatLog) {
      log.append(el("li", { class: "call-chat-item" + (m.mine ? " call-chat-mine" : "") },
        el("p", { class: "call-chat-meta" }, el("span", { class: "call-chat-who", text: m.who }), el("time", { text: new Date(m.at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }) })),
        el("p", { class: "call-chat-text", text: m.text }),
        m.missed && m.missed.length ? el("p", { class: "call-chat-missed", text: "Not delivered to " + m.missed.join(", ") + " — not connected yet" }) : null));
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
        if ((info.large || info.e2ee_required) && !canEncryptFrames) return this.fail(NEEDS_NEWER);
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
        const body = asMember ? { session, code: this.code, as: $("join-as").value, e2ee: canEncryptFrames } : { session, code: this.code, name, e2ee: canEncryptFrames };
        const res = await post("/knocks", body);
        this.knock = res.knock;
        $("join-form").hidden = true;
        $("join-waiting").hidden = false;
        $("join-cancel").focus();
      } catch (err) {
        $("join-submit").disabled = false;
        $("join-error").textContent = { room_full: "This call is full.", no_meeting: "This call has ended.", rate_limited: "Too many attempts. Wait a few minutes and try again.", too_many_requests: "Several people are already waiting. Try again in a moment.", needs_e2ee: NEEDS_NEWER, call_busy: "The call is busy: Pigeonpost’s media server is at capacity for large calls right now. Try again in a few minutes." }[err.code] || "Couldn’t ask to join. Try again.";
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
      $("join-error").textContent = { denied: "Someone in the call declined your request.", timeout: "Nobody answered your request. You can ask again.", ended: "This call has ended.", room_full: "This call is full.", call_busy: "The call is busy: Pigeonpost’s media server is at capacity for large calls right now. Try again in a few minutes.", needs_e2ee: "You were let in, but this call can’t grow past six: someone in it uses a browser or app that can’t encrypt its own audio and video." }[data.reason] || "You weren’t let in.";
      $("join-error").hidden = false;
    },
    enter() {
      // Hand the preview's devices to the call, rather than asking for them a second time.
      const media = this.previewStream;
      this.previewStream = null;
      local.stream = new MediaStream();
      local.muted = Boolean(this.micOff);
      local.cam = media?.getVideoTracks()[0] || null;
      if (local.cam) local.stream.addTrack(local.cam);
      const raw = media?.getAudioTracks()[0] || null;
      // Start with the raw microphone so nobody waits, then swap in the cleaned one.
      local.rawMic = raw;
      local.mic = raw;
      if (raw) { raw.enabled = !local.muted; local.stream.addTrack(raw); }
      if (raw) setMicTrack(raw).catch(() => {});
      local.cameraOff = false;
      $("join").hidden = true;
      openRoomUI({ title: "Call", sub: "", face: this.name, status: "Connecting…" });
      warnIfNoEchoCancellation();
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

  // ---- answering from outside the page (Phase 1c) -------------------------------------------------

  // Answer a ring this page may not have seen yet. The session opened by a woken app is added to the
  // ring by the service and told `incoming`; if that does not come, the ring token answers directly.
  async function answerFromOutside(roomId, ringToken) {
    if (!ROOM_ID.test(String(roomId || ""))) return;
    const token = RING_TOKEN_RE.test(String(ringToken || "")) ? ringToken : null;
    if (ring && ring.room === roomId) { acceptRing(token); return; }
    if (room) return;
    pendingAnswer = { room: roomId, ringToken: token, at: Date.now() };
    start();
    await whenReady();
    await new Promise((r) => setTimeout(r, 2500));
    if (!pendingAnswer || pendingAnswer.room !== roomId || ring || room) return;
    pendingAnswer = null;
    if (!token) { tellHost("callState", { state: "ended", room: roomId }); return; }
    try {
      const res = await fetch(BASE + `/rings/${roomId}`, { headers: { "x-ring-token": token } });
      const state = await res.json().catch(() => ({}));
      if (!res.ok || state.state !== "ringing") {
        tellHost("callState", { state: "ended", room: roomId });
        toast("That call has already ended.");
        return;
      }
      ring = { room: roomId, kind: state.kind === "video" ? "video" : "audio", from: {}, code: null };
      acceptRing(token);
    } catch (_) {
      tellHost("callState", { state: "ended", room: roomId });
    }
  }

  function declineFromOutside(roomId, ringToken) {
    if (!ROOM_ID.test(String(roomId || ""))) return;
    if (pendingAnswer && pendingAnswer.room === roomId) pendingAnswer = null;
    if (ring && ring.room === roomId) { declineRing(); return; }
    if (RING_TOKEN_RE.test(String(ringToken || ""))) {
      fetch(BASE + `/rings/${roomId}/decline`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ ring_token: ringToken }) }).catch(() => {});
    }
  }

  // ---- Phase 3B: scheduled meetings ------------------------------------------------------------
  //
  // docs/calendar.md §7. Members join with their own session and `as`; guests with the single-use
  // `ticket` the postbox gave them; uninvited link holders with a `name`, and only when the organizer
  // allows knocking. The result tells the caller what happened, so the calendar or the meeting page
  // can ask which occurrence when two windows overlap.
  const scheduledWords = {
    window_closed: "This meeting isn’t open right now. It opens shortly before it starts and closes 30 minutes after it ends.",
    not_invited: "You aren’t invited to this meeting.",
    declined: "You declined this meeting. Change your answer in the calendar to join.",
    event_cancelled: "This meeting was cancelled.",
    cancelled: "This meeting was cancelled.",
    ticket_invalid: "That join link has expired. Try again.",
    unavailable: "This meeting isn’t open.",
    calendar_unavailable: "The calendar service can’t be reached right now. Try again in a moment.",
    room_full: "This meeting is full.",
    needs_e2ee: NEEDS_NEWER,
    rate_limited: "Too many attempts. Wait a minute and try again.",
  };
  async function joinScheduled(code, opts) {
    const o = opts || {};
    if (!supported) { toast("This browser can’t make calls. Try a recent Chrome, Edge, Firefox or Safari."); return { state: "error", code: "unsupported" }; }
    if (room || ring) { toast("Finish the current call first."); return { state: "error", code: "busy" }; }
    try {
      if (o.guest || !authHeader()) await ensureGuest();
    } catch (_) { toast("Couldn’t reach the call service. Check your connection and try again."); return { state: "error", code: "network" }; }
    start();
    const title = o.title || "Meeting " + (code.length === 10 ? code.slice(0, 5) + "-" + code.slice(5) : code);
    openRoomUI({ title, sub: "", face: title, status: "Joining…" });
    room = newRoom({ kind: o.kind === "audio" ? "audio" : "video", title, code, scheduledCode: code });
    const joining = room, generation = streamGen;
    const isCurrent = () => room === joining && generation === streamGen;
    try {
      if (!await startLocalMedia(room.kind === "video")) return { state: "cancelled" };
    } catch (err) {
      if (!isCurrent()) return { state: "cancelled" };
      endRoom(mediaMessage(err, joining.kind), { notify: false });
      return { state: "error", code: "media" };
    }
    if (!isCurrent()) return { state: "cancelled" };
    const joinSession = await whenReady();
    if (!isCurrent() || !joinSession) return { state: "cancelled" };
    const joinAuth = authHeader();
    let res;
    try {
      const body = { session: joinSession, e2ee: canEncryptFrames };
      if (o.as) body.as = o.as;
      if (o.occurrenceId) body.occurrence_id = o.occurrenceId;
      if (o.ticket) body.ticket = o.ticket;
      if (o.name) body.name = o.name;
      res = await post(`/meetings/scheduled/${encodeURIComponent(code)}/join`, body);
    } catch (err) {
      if (!isCurrent()) return { state: "cancelled" };
      if (err.code === "choose_occurrence") {
        endRoom(null, { notify: false });
        return { state: "choose_occurrence", occurrences: (err.body && err.body.occurrences) || [] };
      }
      const message = scheduledWords[err.code] || "Couldn’t join the meeting.";
      endRoom(message, { notify: false });
      return { state: "error", code: err.code, message };
    }
    if (!isCurrent()) {
      // Release an accepted old join using its original session, never the next account's token.
      const id = typeof res?.room === "string" ? res.room : res?.room?.room;
      if (res?.state === "joined" && ROOM_ID.test(id || "")) fetch(BASE + `/rooms/${encodeURIComponent(id)}/leave`, {
        method: "POST", headers: { authorization: joinAuth, "content-type": "application/json" }, body: JSON.stringify({ session: joinSession }),
      }).catch(() => {});
      return { state: "cancelled" };
    }
    if (res.scheduled) room.scheduled = res.scheduled;
    if (res.state === "waiting") {
      Object.assign(room, { id: res.room && typeof res.room === "object" ? res.room.room : res.room, state: "waiting", knock: res.knock && typeof res.knock === "object" ? res.knock.knock : res.knock });
      setStatus("Waiting to be let in");
      $("call-lobby").textContent = "Waiting for the host to let you in…";
      $("call-lobby").hidden = false;
      $("call-waiting").hidden = false;
      return { state: "waiting" };
    }
    onJoined(res);
    renderScheduled();
    return { state: "joined" };
  }
  function onScheduledKnock(data) {
    if (data.admitted) { $("call-lobby").textContent = "You’re in. Connecting…"; return; }
    const reasons = { denied: "The host didn’t let you in.", timeout: "Nobody answered your request. You can ask again.", ended: "This meeting has ended.", room_full: "This meeting is full." };
    endRoom(reasons[data.reason] || "You weren’t let in.", { notify: false });
  }
  function renderScheduled() {
    const tools = $("call-host-tools");
    if (!tools || !room) return;
    const s = room.scheduled;
    tools.hidden = !(s && s.role === "host");
    const note = $("call-invite-note");
    if (note && s) note.textContent = "Invited people join from their calendar or invitation. Others with the link can only ask to join if the organizer allows it.";
    renderKnocks();
    renderPeople();
  }
  async function extendScheduled(minutes) {
    if (!room || !room.id) return;
    try {
      const res = await post(`/rooms/${room.id}/extend`, { session, minutes });
      if (res && res.closes_at && room.scheduled) room.scheduled.closes_at = res.closes_at;
      toast(res && res.closes_at ? "The meeting stays open until " + new Date(res.closes_at * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }) + "." : "The meeting was extended.");
    } catch (err) {
      toast(err.code === "forbidden" || err.code === "not_allowed" ? "Only the host can extend the meeting." : "The meeting can’t be extended further (at most two hours past its end).");
    }
  }
  async function endScheduled() {
    if (!room || !room.id) return;
    try {
      await post(`/rooms/${room.id}/end`, { session });
      endRoom("You ended the meeting for everyone.", { notify: false });
    } catch (_) { toast("Couldn’t end the meeting."); }
  }

  function wire() {
    // Phase 7: meeting notes (calls-summary.js) see the room, the session and this device's own
    // sent microphone track — nothing of anyone else's media.
    window.PigeonpostCallsSummary?.attach({
      room: () => room, session: () => session, mic: () => local.mic, muted: () => local.muted,
      post, base: BASE, authHeader, toast, el, svg, isGuest: () => !memberToken(),
    });
    if (PAGE === "join") { Join.init(); return; }
    // `?answer=<room>`: opened by an app that cannot hold a call itself (Linux) to answer one ring.
    try {
      const url = new URL(location.href);
      const wanted = url.searchParams.get("answer");
      if (wanted && ROOM_ID.test(wanted)) pendingAnswer = { room: wanted, ringToken: null, at: Date.now() };
      if (url.searchParams.has("answer")) { url.searchParams.delete("answer"); history.replaceState(history.state, "", url.pathname + url.search + url.hash); }
    } catch (_) { /* fine */ }
    if (PAGE === "meeting") { buildUI(); return; } // Phase 3B: m.js decides when to join
    buildUI();
    $("call-audio-btn")?.addEventListener("click", () => place("audio"));
    $("call-video-btn")?.addEventListener("click", () => place("video"));
    start();
    renderHead();
  }

  window.PigeonpostCalls = {
    start, stop, renderHead,
    // Phase 6: what live.js (a group's live stream) borrows from the call client — the same event
    // stream, session, relay credentials and vendored media-server client.
    _live: {
      request, post, whenReady, session: () => session, turnServers, toast, el, svg, ICON, host,
      loadLibs: () => Sfu.loadLibs(), singleLayer: () => Sfu.singleLayer, canEncryptFrames, forceRelay, E2EE_WORKER,
      busy: () => Boolean(room || ring), start,
    },
    // For automated checks of large calls: receive statistics of the media server's tracks.
    sfuStats: () => Sfu.stats(),
    // Phase 3a
    placeGroup: (kind, group) => placeGroup(kind, group, false),
    joinGroup: (kind, group) => placeGroup(kind, group, true),
    // Phase 1c: the native apps' call screens act through these.
    answer: answerFromOutside,
    decline: declineFromOutside,
    setMuted: (muted) => { if (local.mic && Boolean(muted) !== local.muted) toggleMute(); },
    hangUp: () => { if (ring) declineRing(); else leave(); },
    // Phase 3B
    joinScheduled,
    // For automated checks: who is connected, by which route, at what resolution and rate, and with
    // which security codes. Contains no media and no message text.
    inspect: () => ({
      session, page: PAGE,
      room: room && { id: room.id, code: room.code, you: room.you, state: room.state, size: room.size, topology: room.topology, page: room.page || 0, pages: room.pages || 1, tiles: room.tiles ? room.tiles.size : 0 },
      e2eeCapable: canEncryptFrames,
      sfu: Sfu.inspect(),
      call: room && { id: room.id, state: room.state === "active" && [...peers.values()].some((p) => p.connected) ? "active" : room.state },
      ring: ring && { room: ring.room, kind: ring.kind },
      knocks: [...knocks.keys()],
      minimized: Boolean(room && room.minimized), aec: echoCancelling(), noise: { mode: noise.mode, active: noise.active },
      pinned: room && room.pinned, layout: room && (room.layout || "auto"), fullscreen: isFullscreen(),
      local: { mic: Boolean(local.mic), cam: Boolean(local.cam), screen: Boolean(local.screen), muted: local.muted, cameraOff: local.cameraOff, micDevice: (local.rawMic || local.mic)?.getSettings().deviceId || null, camDevice: local.cam?.getSettings().deviceId || null },
      chat: chatLog.length,
      peers: [...peers.values()].map((p) => ({ id: p.id, name: displayName(p), guest: p.guest, connected: p.connected, security: p.security, mesh: Boolean(p.pc), sfu: Boolean(p.lk), stats: p.stats && { route: p.stats.route, rtt: p.stats.rtt, sending: p.stats.sending, receiving: p.stats.receiving }, aec: p.state.aec, muted: p.state.muted, pc: p.pc && { signaling: p.pc.signalingState, ice: p.pc.iceConnectionState, gathering: p.pc.iceGatheringState, connection: p.pc.connectionState, local: Boolean(p.pc.localDescription), remote: Boolean(p.pc.remoteDescription), initiator: p.initiator, pendingIce: p.pendingIce.length }, remoteCamera: p.state.cameraOff ? 0 : p.remote.camera.getVideoTracks().filter((t) => t.readyState === "live").length, remoteScreen: p.state.screen ? p.remote.screen.getVideoTracks().filter((t) => t.readyState === "live").length : 0 })),
    }),
  };

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", wire);
  else wire();
})();
