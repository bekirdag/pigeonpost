// Pigeonpost inbox — one-to-one voice and video calls.
//
// The call service (`/calls/v1` on this origin) only rings mailboxes and passes WebRTC setup
// messages between the two browsers; it checks every request against the postbox's own answer to
// "which mailboxes does this token own". Audio and video go browser to browser — directly where the
// network allows, through the TURN relay where it does not — encrypted with DTLS-SRTP keys that only
// the two browsers hold. Both screens show a security code derived from both browsers' DTLS
// fingerprints: if the codes match, nothing between them, this service included, is in the middle.
//
// Nothing here acts on message bodies, and nothing here is recorded.
(function () {
  "use strict";

  const $ = (id) => document.getElementById(id);
  const BASE = "/calls/v1";
  // About 4 Mbit/s of media per sender: Opus at up to 128 kbit/s plus video capped at 3.8 Mbit/s.
  // That is the encoders' payload budget. RTP/UDP/IP headers (and TURN framing when relayed) add a
  // few percent on the wire, and the video side adapts down on weaker links rather than stalling.
  const AUDIO_BPS = 128_000;
  const VIDEO_BPS = 3_800_000;
  const RECONNECT_GRACE_MS = 15_000;

  const supported = Boolean(window.RTCPeerConnection && navigator.mediaDevices && navigator.mediaDevices.getUserMedia);
  const forceRelay = (() => { try { return localStorage.getItem("ppi_calls_relay") === "1"; } catch (_) { return false; } })();

  const host = () => window.PigeonpostCallsHost || null;

  let streamWanted = false;
  let streamGen = 0;
  let streamAbort = null;
  let session = null;
  let call = null; // the call on screen, placed or accepted
  let incoming = null; // a call ringing here that has not been answered
  const stats = { pair: null, outbound: {}, inbound: {}, security: null };

  // ---- the event stream from the call service ------------------------------------------------

  function start() {
    if (!supported || streamWanted || !host() || !host().signedIn()) return;
    streamWanted = true;
    runStream(++streamGen);
  }

  function stop() {
    streamWanted = false;
    streamGen += 1;
    if (streamAbort) streamAbort.abort();
    streamAbort = null;
    session = null;
    dismissIncoming();
    if (call) finish("signed_out", null, { notify: true });
    renderHead();
  }

  async function runStream(gen) {
    let backoff = 1000, renewed = false;
    while (streamWanted && gen === streamGen) {
      streamAbort = new AbortController();
      try {
        const res = await fetch(BASE + "/events", {
          headers: { authorization: "Bearer " + host().token(), accept: "text/event-stream" },
          signal: streamAbort.signal,
        });
        if (res.status === 401) {
          if (!renewed && (await host().renew())) { renewed = true; continue; }
          streamWanted = false;
          return;
        }
        // No call service on this origin (a local preview, or a deployment without it): calls are
        // simply unavailable, which the header reflects by not offering them.
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
              try { onEvent(name[1], JSON.parse(data[1])); } catch (err) { console.warn("calls: bad event", err && err.name); }
            }
          }
        }
      } catch (err) {
        if (!streamWanted || gen !== streamGen) return;
      }
      session = null;
      renderHead();
      if (!streamWanted || gen !== streamGen) return;
      await new Promise((r) => setTimeout(r, backoff));
      backoff = Math.min(backoff * 2, 30_000);
    }
  }

  function onEvent(name, data) {
    if (name === "ready") {
      session = data.session;
      renderHead();
      // A dropped stream or a reload mid-call: take the call back over on the new stream.
      if (call && call.id && (call.state === "connecting" || call.state === "active")) {
        post(`/calls/${call.id}/resume`, { session }).catch(() => {});
      }
      return;
    }
    if (name === "incoming") return onIncoming(data);
    if (name === "accepted") {
      if (call && call.id === data.call && call.role === "caller") {
        call.peer = data.callee || call.peer;
        call.state = "connecting";
        stopTone();
        setStatus("Connecting…");
        connectAsCaller().catch((err) => finish("failed", "The call could not connect.", { notify: true, err }));
      }
      return;
    }
    if (name === "signal") {
      if (call && call.id === data.call) onSignal(data.type, data.data).catch((err) => finish("failed", "The call could not connect.", { notify: true, err }));
      return;
    }
    if (name === "ended") {
      if (incoming && incoming.call === data.call) {
        const missed = incoming;
        dismissIncoming();
        if (data.reason !== "answered_elsewhere") host()?.toast("Missed " + kindWord(missed.kind) + " call from " + who(missed.from) + ".");
        return;
      }
      if (call && call.id === data.call) finish(data.reason, endedMessage(data.reason), { notify: false });
    }
  }

  // ---- HTTP to the call service ---------------------------------------------------------------

  async function request(method, path, body, renewed) {
    const res = await fetch(BASE + path, {
      method,
      headers: Object.assign({ authorization: "Bearer " + host().token() }, body ? { "content-type": "application/json" } : {}),
      body: body ? JSON.stringify(body) : undefined,
    });
    if (res.status === 401 && !renewed && (await host().renew())) return request(method, path, body, true);
    const value = await res.json().catch(() => ({}));
    if (!res.ok) {
      const err = new Error(value.error || "http_" + res.status);
      err.status = res.status;
      err.code = value.error;
      throw err;
    }
    return value;
  }
  const post = (path, body) => request("POST", path, body);
  const get = (path) => request("GET", path);

  // ---- placing and answering ------------------------------------------------------------------

  async function place(kind) {
    const h = host();
    const me = h && h.mailbox(), peer = h && h.peer();
    if (!me || !peer || !supported) return;
    if (call || incoming) { h.toast("Finish the current call first."); return; }
    if (!session) { h.toast("Calling isn’t ready yet. Try again in a moment."); start(); return; }
    call = freshCall({ role: "caller", kind, peer: { address: peer, handle: null }, mailbox: me });
    openStage();
    setStatus("Starting " + kindWord(kind) + "…");
    try {
      call.local = await getMedia(kind);
    } catch (err) {
      finish("media", mediaMessage(err, kind), { notify: false });
      return;
    }
    if (!call) return;
    showLocal();
    try {
      const placed = await post("/calls", { session, from: me.address, to: peer, kind: call.kind });
      if (!call) { post(`/calls/${placed.call}/end`, { session }).catch(() => {}); return; }
      call.id = placed.call;
      setStatus("Ringing…");
      startTone("ringback");
    } catch (err) {
      finish("failed", placeError(err), { notify: false });
    }
  }

  function onIncoming(data) {
    if (call || incoming) {
      // Already talking or already ringing: tell the caller "busy" instead of stacking rings.
      post(`/calls/${data.call}/decline`, { session, reason: "busy" }).catch(() => {});
      return;
    }
    incoming = data;
    const h = host();
    const from = data.from || {};
    const peerKey = from.handle || from.address;
    $("call-incoming-title").textContent = "Incoming " + kindWord(data.kind) + " call";
    $("call-incoming-from").textContent = who(from);
    const known = h && (h.knownPeer(from.handle) || h.knownPeer(from.address));
    $("call-incoming-note").textContent = "To " + (data.to || "") + (known ? "" : " · not in your contacts");
    h?.paintAvatar($("call-incoming-avatar"), peerKey);
    $("call-incoming").hidden = false;
    $("call-accept").focus({ preventScroll: true });
    startTone("ring");
    if (document.hidden && "Notification" in window && Notification.permission === "granted") {
      try {
        const n = new Notification("Incoming " + kindWord(data.kind) + " call", { body: who(from), tag: "pp-call-" + data.call });
        n.onclick = () => { window.focus(); n.close(); };
        incoming.notification = n;
      } catch (_) { /* notifications are a courtesy */ }
    }
  }

  function dismissIncoming() {
    if (!incoming) return;
    try { incoming.notification?.close(); } catch (_) { /* already gone */ }
    incoming = null;
    $("call-incoming").hidden = true;
    stopTone();
  }

  async function accept() {
    const ringing = incoming;
    if (!ringing) return;
    dismissIncoming();
    call = freshCall({ role: "callee", kind: ringing.kind, peer: ringing.from, mailbox: { address: ringing.to } });
    call.id = ringing.call;
    openStage();
    setStatus("Connecting…");
    try {
      call.local = await getMedia(ringing.kind);
    } catch (err) {
      post(`/calls/${ringing.call}/decline`, { session }).catch(() => {});
      finish("media", mediaMessage(err, ringing.kind), { notify: false });
      return;
    }
    if (!call) return;
    showLocal();
    try {
      call.state = "connecting";
      await createPeer();
      await post(`/calls/${call.id}/accept`, { session });
    } catch (err) {
      finish("failed", err.code === "not_ringing" ? "That call has already ended." : "The call could not be answered.", { notify: false });
    }
  }

  function decline() {
    const ringing = incoming;
    if (!ringing) return;
    dismissIncoming();
    post(`/calls/${ringing.call}/decline`, { session }).catch(() => {});
  }

  function hangUp() {
    if (call) finish("ended", null, { notify: true });
  }

  // ---- media ----------------------------------------------------------------------------------

  async function getMedia(kind) {
    const audio = {
      echoCancellation: true, noiseSuppression: true, autoGainControl: true,
      channelCount: { ideal: 1 }, sampleRate: { ideal: 48_000 },
    };
    const video = { width: { ideal: 1920 }, height: { ideal: 1080 }, frameRate: { ideal: 30, max: 30 }, facingMode: "user" };
    let media;
    try {
      media = await navigator.mediaDevices.getUserMedia({ audio, video: kind === "video" ? video : false });
    } catch (err) {
      // No camera, or the camera refused: a video call still goes ahead as a voice call rather than
      // not at all. A refused microphone ends it — a call nobody can hear is not a call.
      if (kind !== "video" || err.name === "NotAllowedError" && /audio|microphone/i.test(err.message || "")) throw err;
      media = await navigator.mediaDevices.getUserMedia({ audio, video: false });
      host()?.toast("No camera available — continuing with audio only.");
    }
    for (const track of media.getAudioTracks()) track.contentHint = "speech";
    return media;
  }

  function freshCall(fields) {
    return Object.assign({
      id: null, state: "starting", pc: null, local: null, remote: new MediaStream(),
      pendingIce: [], startedAt: 0, timer: null, statsTimer: null, dropTimer: null, restarted: false,
      muted: false, cameraOff: false,
    }, fields);
  }

  async function createPeer() {
    let iceServers = [];
    try { iceServers = (await get("/turn?session=" + encodeURIComponent(session))).iceServers || []; }
    catch (_) { /* direct connections may still work */ }
    if (!call) throw new Error("ended");
    const pc = new RTCPeerConnection({
      iceServers,
      iceTransportPolicy: forceRelay ? "relay" : "all",
      bundlePolicy: "max-bundle",
      rtcpMuxPolicy: "require",
    });
    call.pc = pc;
    pc.onicecandidate = (e) => { sendSignal("ice", e.candidate ? e.candidate.toJSON() : null); };
    pc.ontrack = (e) => {
      if (!call) return;
      if (!call.remote.getTracks().includes(e.track)) call.remote.addTrack(e.track);
      const view = $("call-remote-video");
      if (view.srcObject !== call.remote) view.srcObject = call.remote;
      view.play().catch(() => {});
      renderStage();
    };
    pc.onconnectionstatechange = () => onConnectionState();
    return pc;
  }

  async function connectAsCaller() {
    const pc = await createPeer();
    for (const track of call.local.getTracks()) {
      const transceiver = pc.addTransceiver(track, { direction: "sendrecv", streams: [call.local] });
      preferCodecs(transceiver, track.kind);
    }
    // A voice call that the other side answers on video still only carries audio; a video call
    // keeps a receive slot for video even if this end has no camera.
    if (call.kind === "video" && !call.local.getVideoTracks().length) {
      preferCodecs(pc.addTransceiver("video", { direction: "recvonly" }), "video");
    }
    await sendOffer(false);
  }

  async function sendOffer(iceRestart) {
    const pc = call.pc;
    const offer = await pc.createOffer({ iceRestart });
    offer.sdp = tuneOpus(offer.sdp);
    await pc.setLocalDescription(offer);
    await tuneSenders(pc);
    await sendSignal("offer", { type: "offer", sdp: pc.localDescription.sdp });
  }

  async function onSignal(type, data) {
    if (!call) return;
    const pc = call.pc || (call.role === "callee" ? await createPeer() : null);
    if (!pc) return;
    if (type === "offer" && call.role === "callee") {
      await pc.setRemoteDescription(data);
      const first = !call.answered;
      if (first) {
        // Attach this end's tracks to the transceivers the offer created, in the offer's order, so
        // the answer has exactly the caller's m-lines and nothing extra.
        for (const transceiver of pc.getTransceivers()) {
          const kind = transceiver.receiver.track.kind;
          const track = call.local.getTracks().find((t) => t.kind === kind && !pc.getSenders().some((s) => s.track === t));
          if (track) {
            await transceiver.sender.replaceTrack(track);
            transceiver.sender.setStreams?.(call.local);
            transceiver.direction = "sendrecv";
          } else {
            transceiver.direction = "recvonly";
          }
          preferCodecs(transceiver, kind);
        }
      }
      const answer = await pc.createAnswer();
      answer.sdp = tuneOpus(answer.sdp);
      await pc.setLocalDescription(answer);
      await tuneSenders(pc);
      call.answered = true;
      await flushIce();
      await sendSignal("answer", { type: "answer", sdp: pc.localDescription.sdp });
      return;
    }
    if (type === "answer" && call.role === "caller") {
      await pc.setRemoteDescription(data);
      await flushIce();
      return;
    }
    if (type === "ice") {
      if (!pc.remoteDescription) { call.pendingIce.push(data); return; }
      await addIce(data);
    }
  }

  async function flushIce() {
    const queued = call ? call.pendingIce.splice(0) : [];
    for (const candidate of queued) await addIce(candidate);
  }

  async function addIce(candidate) {
    try { await call.pc.addIceCandidate(candidate || null); }
    catch (_) { /* a candidate for a transport that was not kept is harmless */ }
  }

  function sendSignal(type, data) {
    if (!call || !call.id || !session) return Promise.resolve();
    return post(`/calls/${call.id}/signal`, { session, type, data }).catch(() => {});
  }

  function preferCodecs(transceiver, kind) {
    if (!transceiver.setCodecPreferences || !window.RTCRtpReceiver?.getCapabilities) return;
    const caps = RTCRtpReceiver.getCapabilities(kind);
    if (!caps) return;
    const rank = (c) => {
      const t = c.mimeType.toLowerCase();
      if (kind === "audio") return t === "audio/opus" ? 0 : 5;
      // H.264 first: it has hardware encoders on Macs, iPhones and most PCs, which is what makes
      // 1080p sustainable without heating the machine. VP9 and VP8 follow for browsers without it.
      if (t === "video/h264") return /profile-level-id=64/.test(c.sdpFmtpLine || "") ? 0 : 1;
      if (t === "video/vp9") return 2;
      if (t === "video/vp8") return 3;
      if (t === "video/av1") return 4;
      return 6; // rtx, red, ulpfec keep their place after the media codecs
    };
    const codecs = caps.codecs.slice().sort((a, b) => rank(a) - rank(b));
    try { transceiver.setCodecPreferences(codecs); } catch (_) { /* keep the browser's own order */ }
  }

  // Opus settings this end asks the other end to send with: full-band, 128 kbit/s ceiling, in-band
  // error correction on, discontinuous transmission off (DTX's silence gaps clip soft speech).
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

  async function tuneSenders(pc) {
    for (const sender of pc.getSenders()) {
      if (!sender.track) continue;
      const params = sender.getParameters();
      if (!params.encodings || !params.encodings.length) params.encodings = [{}];
      const enc = params.encodings[0];
      if (sender.track.kind === "video") {
        enc.maxBitrate = VIDEO_BPS;
        enc.maxFramerate = 30;
        enc.scaleResolutionDownBy = 1;
        params.degradationPreference = "balanced";
      } else {
        enc.maxBitrate = AUDIO_BPS;
      }
      enc.priority = "high";
      enc.networkPriority = "high";
      try { await sender.setParameters(params); } catch (_) { /* older browsers ignore parts of this */ }
    }
  }

  function onConnectionState() {
    if (!call || !call.pc) return;
    const s = call.pc.connectionState;
    if (s === "connected") {
      clearTimeout(call.dropTimer);
      call.dropTimer = null;
      if (call.state !== "active") {
        call.state = "active";
        call.startedAt = Date.now();
        stopTone();
        call.timer = setInterval(renderClock, 1000);
        call.statsTimer = setInterval(sampleStats, 2000);
        verifySecurity().catch(() => {});
      }
      renderClock();
      return;
    }
    if (s === "disconnected" || s === "failed") {
      setStatus("Reconnecting…");
      if (call.role === "caller" && !call.restarted && s === "failed") {
        call.restarted = true;
        sendOffer(true).catch(() => {});
      }
      if (!call.dropTimer) call.dropTimer = setTimeout(() => finish("disconnected", "The call dropped.", { notify: true }), RECONNECT_GRACE_MS);
    }
  }

  // The security code: both browsers' DTLS certificate fingerprints, hashed together. Each browser
  // can only see the other's fingerprint through the call service, so if the service substituted
  // its own certificate the two screens would show different codes.
  async function verifySecurity() {
    const pc = call && call.pc;
    if (!pc) return;
    const fingerprint = (sdp) => ((/a=fingerprint:\S+ ([0-9A-F:]+)/i.exec(sdp || "") || [])[1] || "").toUpperCase();
    const local = fingerprint(pc.localDescription?.sdp), remote = fingerprint(pc.remoteDescription?.sdp);
    if (!local || !remote) return;
    // The certificate the remote end actually used for DTLS must be the one its description named.
    const report = await pc.getStats();
    let presented = null;
    report.forEach((s) => {
      if (s.type === "transport" && s.remoteCertificateId) presented = report.get(s.remoteCertificateId)?.fingerprint || null;
    });
    if (presented && presented.toUpperCase() !== remote) {
      finish("security", "The call was ended: the other side's encryption certificate did not match.", { notify: true });
      return;
    }
    const [a, b] = [local, remote].sort();
    const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode("pigeonpost-call-sas-v1\n" + a + "\n" + b));
    const code = String(new DataView(digest).getUint32(0) % 1_000_000).padStart(6, "0");
    stats.security = { code, presentedMatches: presented ? true : null };
    if (!call) return;
    $("call-sas-code").textContent = code.slice(0, 3) + " " + code.slice(3);
    $("call-sas").hidden = false;
  }

  async function sampleStats() {
    if (!call || !call.pc) return;
    const report = await call.pc.getStats();
    const prev = { out: stats.outbound, inb: stats.inbound };
    const out = {}, inb = {};
    let pair = null;
    report.forEach((s) => {
      if (s.type === "transport" && s.selectedCandidatePairId) pair = report.get(s.selectedCandidatePairId);
      if (s.type === "outbound-rtp" && s.kind) out[s.kind] = s;
      if (s.type === "inbound-rtp" && s.kind) inb[s.kind] = s;
    });
    if (!pair) report.forEach((s) => { if (s.type === "candidate-pair" && s.nominated && s.state === "succeeded") pair = s; });
    const local = pair && report.get(pair.localCandidateId), remote = pair && report.get(pair.remoteCandidateId);
    const rate = (now, before) => before && now && now.timestamp > before.timestamp
      ? Math.round(((now.bytesSent ?? now.bytesReceived) - (before.bytesSent ?? before.bytesReceived)) * 8 / ((now.timestamp - before.timestamp) / 1000))
      : null;
    stats.pair = pair ? { local: local?.candidateType, remote: remote?.candidateType, protocol: local?.relayProtocol || local?.protocol, rtt: pair.currentRoundTripTime } : null;
    stats.sending = {
      video: out.video ? { width: out.video.frameWidth, height: out.video.frameHeight, fps: out.video.framesPerSecond, bps: rate(out.video, prev.out.video), limitation: out.video.qualityLimitationReason } : null,
      audio: out.audio ? { bps: rate(out.audio, prev.out.audio) } : null,
    };
    stats.receiving = {
      video: inb.video ? { width: inb.video.frameWidth, height: inb.video.frameHeight, fps: inb.video.framesPerSecond, bps: rate(inb.video, prev.inb.video), decoded: inb.video.framesDecoded } : null,
      audio: inb.audio ? { bps: rate(inb.audio, prev.inb.audio), level: inb.audio.audioLevel, concealed: inb.audio.concealedSamples, received: inb.audio.totalSamplesReceived } : null,
    };
    stats.outbound = out;
    stats.inbound = inb;
    renderQuality();
  }

  // ---- ending ---------------------------------------------------------------------------------

  function finish(reason, message, { notify }) {
    const ending = call;
    if (!ending) return;
    call = null;
    stopTone();
    clearInterval(ending.timer);
    clearInterval(ending.statsTimer);
    clearTimeout(ending.dropTimer);
    if (notify && ending.id && session) post(`/calls/${ending.id}/end`, { session }).catch(() => {});
    try { ending.pc?.close(); } catch (_) { /* already closed */ }
    for (const track of ending.local?.getTracks() || []) track.stop();
    for (const track of ending.remote?.getTracks() || []) track.stop();
    for (const id of ["call-remote-video", "call-local-video"]) { const v = $(id); v.pause?.(); v.srcObject = null; }
    stats.pair = null; stats.security = null;
    $("call-ui").hidden = true;
    $("call-sas").hidden = true;
    renderHead();
    const last = ending.startedAt ? " · " + clock(Date.now() - ending.startedAt) : "";
    if (message) host()?.toast(message);
    else if (reason === "ended") host()?.toast("Call ended" + last + ".");
    host()?.focusThread?.();
  }

  // A closing tab cannot wait for an answer, but it can still tell the other side it has gone.
  window.addEventListener("pagehide", () => {
    if (call && call.id && session) {
      try {
        fetch(BASE + `/calls/${call.id}/end`, {
          method: "POST", keepalive: true,
          headers: { authorization: "Bearer " + host().token(), "content-type": "application/json" },
          body: JSON.stringify({ session }),
        });
      } catch (_) { /* best effort */ }
    }
  });

  // ---- screen ---------------------------------------------------------------------------------

  function renderHead() {
    const h = host();
    const show = Boolean(supported && h && h.signedIn() && h.peer() && h.mailbox());
    for (const id of ["call-audio-btn", "call-video-btn"]) {
      const button = $(id);
      if (!button) continue;
      button.hidden = !show;
      button.disabled = Boolean(call || incoming);
    }
  }

  function openStage() {
    const h = host();
    const peerKey = call.peer.handle || call.peer.address;
    $("call-name").textContent = h ? h.displayName(peerKey) : peerKey;
    $("call-handle").textContent = peerKey;
    h?.paintAvatar($("call-avatar"), peerKey);
    $("call-mute").setAttribute("aria-pressed", "false");
    $("call-camera").setAttribute("aria-pressed", "false");
    $("call-quality").textContent = "";
    $("call-sas").hidden = true;
    $("call-ui").hidden = false;
    renderStage();
    renderHead();
    $("call-hangup").focus({ preventScroll: true });
  }

  function showLocal() {
    const view = $("call-local-video");
    view.srcObject = call.local;
    view.play().catch(() => {});
    renderStage();
  }

  function renderStage() {
    if (!call) return;
    const stage = $("call-stage");
    const hasLocalVideo = Boolean(call.local && call.local.getVideoTracks().length);
    const hasRemoteVideo = call.remote.getVideoTracks().some((t) => t.readyState === "live" && !t.muted);
    stage.dataset.kind = call.kind;
    stage.dataset.remoteVideo = String(hasRemoteVideo);
    stage.dataset.localVideo = String(hasLocalVideo && !call.cameraOff);
    $("call-camera").hidden = !hasLocalVideo;
    for (const t of call.remote.getVideoTracks()) { t.onmute = t.onunmute = renderStage; }
  }

  function setStatus(text) { $("call-status").textContent = text; }

  function renderClock() {
    if (call && call.state === "active") setStatus(clock(Date.now() - call.startedAt));
  }

  function renderQuality() {
    const rx = stats.receiving && stats.receiving.video;
    const route = stats.pair && stats.pair.local === "relay" ? "Relayed" : stats.pair ? "Direct" : "";
    const res = rx && rx.height ? (rx.height >= 1080 ? "1080p" : rx.height >= 720 ? "720p" : rx.height + "p") : "";
    $("call-quality").textContent = [res, route].filter(Boolean).join(" · ");
    renderStage();
  }

  function toggleMute() {
    if (!call || !call.local) return;
    call.muted = !call.muted;
    for (const t of call.local.getAudioTracks()) t.enabled = !call.muted;
    const b = $("call-mute");
    b.setAttribute("aria-pressed", String(call.muted));
    b.setAttribute("aria-label", call.muted ? "Unmute microphone" : "Mute microphone");
    b.title = b.getAttribute("aria-label");
  }

  function toggleCamera() {
    if (!call || !call.local) return;
    call.cameraOff = !call.cameraOff;
    for (const t of call.local.getVideoTracks()) t.enabled = !call.cameraOff;
    const b = $("call-camera");
    b.setAttribute("aria-pressed", String(call.cameraOff));
    b.setAttribute("aria-label", call.cameraOff ? "Turn camera on" : "Turn camera off");
    b.title = b.getAttribute("aria-label");
    renderStage();
  }

  // ---- tones ----------------------------------------------------------------------------------

  let tone = null;
  function startTone(kind) {
    stopTone();
    const Ctx = window.AudioContext || window.webkitAudioContext;
    if (!Ctx) return;
    try {
      const ctx = new Ctx();
      const gain = ctx.createGain();
      gain.gain.value = 0;
      gain.connect(ctx.destination);
      const freqs = kind === "ring" ? [523.25, 659.25] : [440];
      const oscs = freqs.map((f) => { const o = ctx.createOscillator(); o.type = "sine"; o.frequency.value = f; o.connect(gain); o.start(); return o; });
      const level = kind === "ring" ? 0.07 : 0.035;
      const on = kind === "ring" ? 900 : 1200, period = kind === "ring" ? 2600 : 4000;
      const pulse = () => {
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

  // ---- words ----------------------------------------------------------------------------------

  const kindWord = (kind) => (kind === "video" ? "video" : "voice");
  const who = (m) => (m && (m.handle || m.address)) || "someone";
  const clock = (ms) => {
    const s = Math.max(0, Math.floor(ms / 1000));
    const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), r = s % 60;
    return (h ? h + ":" + String(m).padStart(2, "0") : String(m)) + ":" + String(r).padStart(2, "0");
  };

  function endedMessage(reason) {
    const name = call ? who(call.peer) : "They";
    return {
      declined: name + " declined the call.",
      busy: name + " is on another call.",
      no_answer: "No answer.",
      cancelled: "The call was cancelled.",
      disconnected: "The call dropped.",
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

  // ---- wiring ---------------------------------------------------------------------------------

  function wire() {
    $("call-audio-btn")?.addEventListener("click", () => place("audio"));
    $("call-video-btn")?.addEventListener("click", () => place("video"));
    $("call-accept")?.addEventListener("click", accept);
    $("call-decline")?.addEventListener("click", decline);
    $("call-hangup")?.addEventListener("click", hangUp);
    $("call-mute")?.addEventListener("click", toggleMute);
    $("call-camera")?.addEventListener("click", toggleCamera);
    document.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && incoming && !$("call-incoming").hidden) decline();
    });
    start();
    renderHead();
  }

  window.PigeonpostCalls = {
    start, stop, renderHead,
    // For automated checks: the live connection's route, resolution, rates and security code.
    inspect: () => ({ session, call: call && { id: call.id, role: call.role, kind: call.kind, state: call.state }, incoming: incoming && { call: incoming.call, kind: incoming.kind }, stats: JSON.parse(JSON.stringify({ pair: stats.pair, sending: stats.sending, receiving: stats.receiving, security: stats.security })) }),
  };

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", wire);
  else wire();
})();
