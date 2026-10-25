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
      token = options.token || null;
      names = options.known || {};
      nativeRing = options.nativeRing === true;
      faces = options.avatars && typeof options.avatars === "object" ? options.avatars : {};
      const next = options.mailbox || null;
      const changed = !me || !next || me.address !== next.address;
      me = next;
      if (!calls()) return;
      if (changed) calls().stop();
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
    // Phase 1c: act on a ring the app showed itself. `ringToken` comes from the push, or is null for
    // a ring the page reported.
    answer(room, ringToken) { if (calls()) calls().answer(room, ringToken || null); },
    decline(room, ringToken) { if (calls()) calls().decline(room, ringToken || null); },
    setMuted(muted) { if (calls()) calls().setMuted(Boolean(muted)); },
    hangUp() { if (calls()) calls().hangUp(); },
    signOut() {
      token = null; me = null; peer = null;
      if (calls()) calls().stop();
    },
  };

  // How much of the screen the call wants, whenever that changes.
  let last = null;
  function report() {
    const shown = (id) => { const node = document.getElementById(id); return Boolean(node && !node.hidden); };
    const mode = shown("call-ring") || shown("call-ui") ? "full" : shown("call-mini") ? "mini" : "hidden";
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
