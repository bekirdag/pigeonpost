// Pigeonpost inbox — the /g/<group-id>#<token> invite link (Phase 3a).
//
// The fragment never leaves the browser. It is moved into this origin's storage for at most 30
// minutes (long enough to sign in), removed from the address bar, and the inbox takes over: it
// shows the group's preview and asks before joining.
(function () {
  "use strict";
  var match = /^\/g\/(grp_[A-Za-z0-9]{1,60})\/?$/.exec(location.pathname);
  var token = (location.hash || "").replace(/^#/, "");
  var note = document.getElementById("join-group-note");
  if (!match) {
    if (note) note.textContent = "This invite link is not valid.";
    return;
  }
  if (token && !/^[A-Za-z0-9_-]{16,128}$/.test(token)) token = "";
  try {
    localStorage.setItem("ppi_group_join", JSON.stringify({ group_id: match[1], link_token: token || null, at: Date.now() }));
  } catch (_) {
    if (note) note.textContent = "This browser blocks storage, so the link cannot be opened here.";
    return;
  }
  history.replaceState(null, "", location.pathname);
  location.replace("/");
})();
