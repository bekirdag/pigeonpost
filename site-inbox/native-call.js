// Pigeonpost — the bridge between the native apps (iPhone, iPad, Mac, Android, Windows, Linux) and
// the call client (calls.js).
//
// The app owns sign-in; this page only ever holds the member token the app hands it, renews it by
// asking the app, and tells the app how much of the screen the call needs: all of it (a call or an
// incoming ring), a strip (a call minimised while reading mail) or none.
(function () {
  "use strict";

  // One message channel per host: WKWebView (iPhone, iPad, Mac) and WebKitGTK (Linux) register a
  // `pigeonpost` script message handler; Android's WebView exposes a `PigeonpostAndroid` object that
  // takes a JSON string; Windows' WebView2 has `chrome.webview`.
  function tell(message) {
    try {
      const apple = window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.pigeonpost;
      if (apple) { apple.postMessage(message); return; }
      if (window.PigeonpostAndroid && window.PigeonpostAndroid.postMessage) { window.PigeonpostAndroid.postMessage(JSON.stringify(message)); return; }
      if (window.chrome && window.chrome.webview) window.chrome.webview.postMessage(message);
    } catch (_) { /* app gone */ }
  }

  let token = null, me = null, peer = null, names = {}, nativeRing = false, faces = {};
  let renewing = null, settle = null;
  let scheduled = null;

  function cancelRenewal() {
    const done = settle;
    renewing = null; settle = null;
    if (done) done(false);
  }
  function finishScheduled(attempt, result) {
    if (scheduled !== attempt) return;
    scheduled = null;
    clearTimeout(attempt.timer);
    attempt.resolve(result);
  }
  function cancelScheduled() {
    if (scheduled) finishScheduled(scheduled, { state: "cancelled" });
  }

  window.PigeonpostCallsHost = {
    signedIn: () => Boolean(token && me),
    token: () => token,
    renew: () => {
      if (!renewing) {
        renewing = new Promise((resolve) => { settle = resolve; });
        tell({ type: "renew" });
      }
      return renewing;
    },
    mailbox: () => me,
    peer: () => peer,
    knownPeer: (key) => Boolean(key && Object.prototype.hasOwnProperty.call(names, key)),
    // The app's name for a peer (a contact alias or its own derived name), else the shared rule.
    displayName: (key) => (key && names[key]) || (window.PigeonpostNames ? window.PigeonpostNames.displayName(key) : key),
    // Phase 1b: a profile picture the app already holds, laid over the initials (which stay
    // underneath as the fallback), as the inbox's own Avatars.paint does.
    decorateAvatar: (node, key) => {
      if (!node) return;
      const url = key && Object.prototype.hasOwnProperty.call(faces, key) ? faces[key] : null;
      if (typeof url === "string" && url.startsWith("data:image/")) {
        node.classList.add("has-photo");
        node.style.backgroundImage = 'url("' + url + '")';
      } else {
        node.classList.remove("has-photo");
        node.style.backgroundImage = "";
      }
    },
    toast: (text) => tell({ type: "toast", text: String(text || "") }),
    // Phase 1c (docs/call-alerting.md). With `nativeRing`, the app shows incoming calls itself
    // (CallKit, a system notification) and the page shows no ring screen of its own.
    ring: (data) => { tell(Object.assign({ type: "ring" }, data)); return nativeRing; },
    ringEnded: (data) => tell(Object.assign({ type: "ring_ended" }, data)),
    callState: (data) => tell(Object.assign({ type: "call" }, data)),
    muted: (muted) => tell({ type: "muted", muted: Boolean(muted) }),
  };

  const calls = () => window.PigeonpostCalls;

  window.PigeonpostNative = {
    // Signed in as `mailbox` ({address, handle}); `known` maps peer keys to display names.
    configure(options) {
      const next = options.mailbox || null;
      const changed = !me || !next || me.address !== next.address;
      if (changed) {
        cancelRenewal(); cancelScheduled();
        if (calls()) calls().stop();
      }
      token = options.token || null;
      names = options.known || {};
      nativeRing = options.nativeRing === true;
      faces = options.avatars && typeof options.avatars === "object" ? options.avatars : {};
      me = next;
      if (!calls()) return;
      if (token && me) calls().start();
    },
    renewed(fresh) {
      token = fresh || null;
      const done = settle;
      renewing = null; settle = null;
      if (done) done(Boolean(fresh));
    },
    call(key, kind) {
      peer = key;
      const button = document.getElementById(kind === "video" ? "call-video-btn" : "call-audio-btn");
      if (button) button.click();
    },
    // A calendar join is always a member join as the mailbox the app is currently showing.
    // Lobby/host roles and occurrence windows are decided by the call service, never this bridge.
    joinScheduled(code, options) {
      const o = options || {};
      if (!token || !me || o.as !== me.address) return Promise.resolve({ state: "error", code: "identity_changed" });
      if (!/^[ABCDEFGHJKMNPQRSTUVWXYZ23456789]{10}$/.test(code)
          || typeof o.occurrenceId !== "string" || !o.occurrenceId.length || o.occurrenceId.length > 256
          || /[\u0000-\u001f\u007f]/.test(o.occurrenceId)) return Promise.resolve({ state: "error", code: "invalid_meeting" });
      if (!calls() || typeof calls().joinScheduled !== "function") return Promise.resolve({ state: "error", code: "bridge_unavailable" });
      if (scheduled) return Promise.resolve({ state: "error", code: "busy" });
      const address = me.address;
      return new Promise((resolve) => {
        const attempt = { resolve, timer: null };
        scheduled = attempt;
        attempt.timer = setTimeout(() => {
          if (scheduled !== attempt) return;
          finishScheduled(attempt, { state: "error", code: "timeout" });
          calls()?.hangUp?.();
        }, 25_000);
        // Copy only the supported member inputs; a role, guest ticket or other authority cannot
        // accidentally pass through from a native caller.
        Promise.resolve().then(() => scheduled !== attempt ? { state: "cancelled" } : calls().joinScheduled(code, {
          as: address, occurrenceId: o.occurrenceId, kind: o.kind === "audio" ? "audio" : "video",
          title: typeof o.title === "string" ? o.title.slice(0, 200) : "Meeting",
        })).then((result) => {
          const state = result && result.state;
          if (state === "joined" || state === "waiting" || state === "cancelled" || state === "choose_occurrence") {
            finishScheduled(attempt, { state });
          } else {
            const code = result && /^[a-z0-9_]{1,64}$/.test(result.code) ? result.code : "unavailable";
            finishScheduled(attempt, { state: "error", code,
              message: typeof result?.message === "string" ? result.message.slice(0, 400) : "Couldn’t join the meeting." });
          }
        }, () => finishScheduled(attempt, { state: "error", code: "network" }));
      });
    },
    // Phase 6: watch a group's live stream, or go live, as the mailbox the app is showing. Who may do
    // either is decided by the postbox and the calls service; the stream is end-to-end encrypted
    // in this page (live.js), with no fallback on a web view that cannot encrypt frames.
    watchStream(group, streamId) {
      const live = window.PigeonpostLive;
      if (!token || !me) return Promise.resolve({ ok: false, code: "signed_out" });
      if (!live) return Promise.resolve({ ok: false, code: "bridge_unavailable" });
      if (!group || !/^grp_[a-z2-7]{26}$/.test(group.id || "") || !/^str_[a-z2-7]{26}$/.test(streamId || "")) return Promise.resolve({ ok: false, code: "invalid_stream" });
      if (!live.supported()) return Promise.resolve({ ok: false, code: "needs_e2ee" });
      return live.watch({ id: group.id, name: String(group.name || "").slice(0, 80) }, streamId);
    },
    goLive(group, options) {
      const live = window.PigeonpostLive, o = options || {};
      if (!token || !me) return Promise.resolve({ ok: false, code: "signed_out" });
      if (!live) return Promise.resolve({ ok: false, code: "bridge_unavailable" });
      if (!group || !/^grp_[a-z2-7]{26}$/.test(group.id || "")) return Promise.resolve({ ok: false, code: "invalid_group" });
      if (!live.supported()) return Promise.resolve({ ok: false, code: "needs_e2ee" });
      return live.goLive({ id: group.id, name: String(group.name || "").slice(0, 80) }, { title: String(o.title || "").slice(0, 100), camera: o.camera !== false, screen: o.screen === true });
    },
    stopStream() { const live = window.PigeonpostLive; if (live && live.active()) live.stop(); },
    // Phase 1c: act on a ring the app showed itself. `ringToken` comes from the push, or is null for
    // a ring the page reported.
    answer(room, ringToken) { if (calls()) calls().answer(room, ringToken || null); },
    decline(room, ringToken) { if (calls()) calls().decline(room, ringToken || null); },
    setMuted(muted) { if (calls()) calls().setMuted(Boolean(muted)); },
    hangUp() { cancelScheduled(); if (calls()) calls().hangUp(); },
    signOut() {
      cancelRenewal(); cancelScheduled();
      window.PigeonpostLive?.stop?.();
      if (calls()) calls().stop();
      token = null; me = null; peer = null;
    },
  };

  // How much of the screen the call wants, whenever that changes.
  let last = null;
  function report() {
    const shown = (id) => { const node = document.getElementById(id); return Boolean(node && !node.hidden); };
    const mode = shown("call-ring") || shown("call-ui") || shown("live-ui") ? "full" : shown("call-mini") ? "mini" : "hidden";
    if (mode === last) return;
    last = mode;
    tell({ type: "mode", mode });
  }
  function watch() {
    new MutationObserver(report).observe(document.body, { subtree: true, childList: true, attributes: true, attributeFilter: ["hidden"] });
    report();
    tell({ type: "ready" });
  }
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", watch);
  else watch();
})();
