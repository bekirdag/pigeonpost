// The one sign-in return path (Keycloak allowlists exactly this URL): verify, exchange, go back.
(function () {
  "use strict";
  const PPE = window.PPE;
  PPE.completeSignIn().then(
    (next) => window.location.replace(next),
    (e) => {
      const box = document.getElementById("callback");
      box.replaceChildren(PPE.el("h2", {}, "Sign-in didn't finish"), PPE.el("p", { class: "muted" }, e.message),
        PPE.el("a", { class: "btn btn-primary", href: "/app" }, "Try again"));
    });
})();
