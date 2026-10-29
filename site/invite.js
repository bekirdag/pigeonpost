// SMS invitation landing page (docs/contacts-and-phone.md §5). The code in the link names an
// invitation, never the number; the opt-out sends only the code and always gets the same answer.
(function () {
  "use strict";
  var POSTBOX = "https://postbox.pigeonpost.dev";
  var code = "";
  try {
    code = new URLSearchParams(window.location.search).get("c") || "";
  } catch (_) {
    code = "";
  }
  if (!/^[abcdefghjkmnpqrstuvwxyz23456789]{12}$/.test(code)) return;
  // Drop the code from the address bar so it is not kept in history or shared by accident.
  try {
    window.history.replaceState(null, "", window.location.pathname);
  } catch (_) {}
  var box = document.getElementById("optout");
  var btn = document.getElementById("optout-btn");
  var status = document.getElementById("optout-status");
  if (!box || !btn || !status) return;
  box.hidden = false;
  btn.addEventListener("click", function () {
    btn.disabled = true;
    status.textContent = "Working…";
    fetch(POSTBOX + "/v1/sms/opt-out", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ code: code }),
      credentials: "omit",
      referrerPolicy: "no-referrer",
    })
      .then(function (r) {
        if (r.status === 429) throw new Error("busy");
        if (!r.ok) throw new Error("failed");
        status.textContent = "Done. Pigeonpost will not text this number invitations again.";
      })
      .catch(function (e) {
        btn.disabled = false;
        status.textContent =
          e && e.message === "busy"
            ? "Too many tries from this network. Please try again in an hour, or reply STOP."
            : "That did not work. Please try again, or reply STOP to the message.";
      });
  });
})();
