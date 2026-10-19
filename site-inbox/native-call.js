// Pigeonpost — the bridge between the iPhone/iPad app and the call client (calls.js).
//
// The app owns sign-in; this page only ever holds the member token the app hands it, renews it by
// asking the app, and tells the app how much of the screen the call needs: all of it (a call or an
// incoming ring), a strip (a call minimised while reading mail) or none.
(function () {
  "use strict";

  const bridge = window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.pigeonpost;
  const tell = (message) => { try { if (bridge) bridge.postMessage(message); } catch (_) { /* app gone */ } };

  let token = null, me = null, peer = null, names = {};
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
    displayName: (key) => (key && names[key]) || key,
    toast: (text) => tell({ type: "toast", text: String(text || "") }),
  };

  const calls = () => window.PigeonpostCalls;

  window.PigeonpostNative = {
    // Signed in as `mailbox` ({address, handle}); `known` maps peer keys to display names.
    configure(options) {
      token = options.token || null;
      names = options.known || {};
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
