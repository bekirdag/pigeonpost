// Invitation acceptance (SDS §7.2). The token arrives in the URL fragment, so it never reaches a
// server log or a Referer. It is moved out of the address bar at once, kept only in this tab
// across sign-in, and sent only in POST bodies. Nothing is accepted by opening the link: a person
// signs in and presses Accept.
(function () {
  "use strict";
  const PPE = window.PPE;
  const el = PPE.el;
  const cfg = window.PPE_CONFIG || {};
  const KEY = "ppe_invite_token";
  const TOKEN = /^[A-Za-z0-9_-]{32,128}$/;

  /// Take the token from the fragment (and remove it from the address bar), or from this tab.
  PPE.takeInviteToken = function () {
    const hash = window.location.hash.replace(/^#/, "");
    if (window.location.href.includes("#")) {
      // A new link starts a new decision, even if its token is invalid or empty.
      // Only a fragment-less OIDC return may resume an earlier acceptance.
      PPE.store.del(KEY);
      PPE.store.del("ppe_resume");
      window.history.replaceState({}, "", window.location.pathname + window.location.search);
      if (TOKEN.test(hash)) PPE.store.set(KEY, hash);
    }
    const t = PPE.store.get(KEY);
    return t && TOKEN.test(t) ? t : null;
  };

  const box = (...nodes) => { const b = document.getElementById("invite"); b.replaceChildren(...nodes); const h = b.querySelector("h1"); if (h) { h.tabIndex = -1; h.focus(); } };

  function invalid() {
    PPE.store.del(KEY);
    box(el("h1", {}, "This invitation link isn't valid"),
      el("p", { class: "muted" }, "It may have expired, been used, or been replaced by a newer one. Ask the person who invited you to send it again."));
  }

  function notice(preview) {
    const org = preview.org && preview.org.name ? preview.org.name : "The organization";
    const rooms = Array.isArray(preview.rooms) && preview.rooms.length ? preview.rooms.join(", ") : null;
    return el("p", { class: "note" },
      `${org}'s admins will see your company address, status and last activity, not your messages.`,
      rooms ? ` Joining adds you to: ${rooms}.` : "",
      " Check that this page is on enterprise.pigeonpost.dev before you sign in.");
  }

  function render(preview, token) {
    const org = preview.org || {};
    const signedIn = PPE.signedIn();
    const who = PPE.whoami();
    const accept = el("button", { class: "btn btn-primary btn-large", type: "button" }, signedIn ? "Accept invitation" : "Sign in to accept");
    const status = el("p", { class: "status-line", role: "status" });
    accept.addEventListener("click", () => (signedIn ? acceptInvite(token, status, accept) : PPE.signIn({ returnTo: "/invite" })));
    box(
      el("h1", {}, `${org.name || "A team"} invites you`),
      el("p", { class: "muted" }, preview.inviter ? `${preview.inviter} invited you to join on Pigeonpost. Your address will be:` : "Your address will be:"),
      el("div", { class: "big-address" }, preview.handle || ""),
      Array.isArray(preview.role_names) && preview.role_names.length ? el("p", {}, `Role: ${preview.role_names.join(", ")}`) : null,
      el("p", { class: "muted" }, `This invitation is for ${preview.contact_masked || "you"} and expires ${PPE.date(preview.expires_at)}.`),
      preview.requires_mfa ? el("p", { class: "note" }, "This organization requires two-step sign-in. You'll be asked to set it up if you haven't.") : null,
      notice(preview),
      signedIn ? el("p", { class: "note" }, `Signed in as ${who.username || "your account"}. `, el("button", { class: "btn btn-quiet btn-small", type: "button", onclick: () => PPE.switchAccount("/invite") }, "Use another account")) : null,
      status, el("div", { class: "actions" }, accept));
  }

  PPE.onResume("invite.accept", async () => {
    const token = PPE.store.get(KEY);
    if (token) await acceptInvite(token, null, null);
  });

  async function acceptInvite(token, status, button) {
    if (button) button.disabled = true;
    try {
      const r = await PPE.postbox("/v1/org-invites/accept", { method: "POST", body: { token }, resume: { name: "invite.accept" } });
      PPE.store.del(KEY);
      welcome(r);
    } catch (e) {
      if (e instanceof PPE.Redirecting) return;
      if (button) button.disabled = false;
      if (e.code === "invite_address_mismatch") return mismatch(e.body.contact_masked);
      if (e.code === "invite_invalid" || e.status === 404) return invalid();
      if (status) { status.className = "status-line status-bad"; status.textContent = PPE.explain(e); }
      else PPE.fail(e);
    }
  }

  function mismatch(masked) {
    const who = PPE.whoami();
    box(el("h1", {}, "This invitation is for another address"),
      el("p", {}, `It was sent to ${masked || "a different address"}, but you're signed in as ${who.username || "another account"}.`),
      el("p", { class: "muted" }, "Sign in with the account that uses the invited address. If you don't have one yet, create it with that address."),
      el("div", { class: "actions" }, el("button", { class: "btn btn-primary", type: "button", onclick: () => PPE.switchAccount("/invite") }, "Switch account")));
  }

  function welcome(r) {
    const org = (r && r.org) || {};
    const mailbox = (r && r.mailbox) || {};
    box(el("h1", {}, `Welcome to ${org.name || "your team"}`),
      el("p", {}, "Your company address is ready:"),
      el("div", { class: "big-address" }, mailbox.handle || mailbox.address || ""),
      el("p", { class: "muted" }, "It's in your inbox next to your other addresses. Get the apps to use it on your phone and computer, then scan the QR code in the app to sign in."),
      el("div", { class: "actions" },
        el("a", { class: "btn", href: `${cfg.site || "https://pigeonpost.dev"}/download` }, "Get the apps"),
        el("a", { class: "btn btn-primary", href: cfg.inbox || "https://inbox.pigeonpost.dev" }, "Open your inbox")));
  }

  async function init() {
    PPE.renderAccount();
    const token = PPE.takeInviteToken();
    if (!token) return invalid();
    if (await PPE.runResume()) return;
    let preview;
    try { preview = await PPE.postbox("/v1/org-invites/preview", { method: "POST", auth: false, body: { token } }); }
    catch (e) { return e.status === 429 ? box(el("h1", {}, "Too many attempts"), el("p", { class: "muted" }, "Wait a few minutes, then open the link again.")) : invalid(); }
    render(preview, token);
  }
  PPE.invite = { render, mismatch, welcome, invalid };
  init();
})();
