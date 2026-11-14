// Security (two-factor status from the sign-in's acr, as #296; authenticator set-up through the
// realm's CONFIGURE_TOTP action; sign-in methods), Privacy (data, and the account deletion flow that
// the stores link to as #delete-account) and Support.
//
// Postbox: GET/POST /v1/account/deletion-request {confirm: true}.
(function () {
  "use strict";
  const el = (...a) => PP.el(...a);
  const issuer = () => window.PP_CONFIG.oidc.issuer;

  /// Keycloak skips the password for a signed-in person and shows only the "scan this QR" screen.
  ACC.setupTotp = function () {
    PP.durable.set("pp_postaction", "totp");
    return PP.signIn({ kc_action: "CONFIGURE_TOTP", returnTo: "/" + PP.href("security") });
  };

  ACC.page({
    key: "security", title: "Security", group: "Account", icon: "security",
    async render(view, ctx) {
      const on = Boolean(ctx.twoStep);
      view.append(PP.ui.header({ title: "Security", text: "How you sign in, and the second step that protects it." }),
        el("div", { class: "pp-cols" },
          PP.ui.card({ title: "Two-factor sign-in", tone: on ? null : "warning", actions: on ? PP.ui.badge("On", "success") : PP.ui.badge("Not confirmed", "warning") },
            on ? el("p", {}, el("strong", {}, "Two-factor is on. "), "Signing in asks for a code from your authenticator app.")
              : el("p", { class: "pp-muted" }, "Add two-factor authentication with an app like Google Authenticator, Authy or 1Password."),
            el("div", { class: "pp-card-actions" }, ACC.btn(on ? "Add another authenticator" : "Set up two-factor authentication", { kind: on ? "default" : "primary", id: "acc-2fa", icon: "security", onclick: ACC.setupTotp })),
            el("p", { class: "pp-hint" }, "Opens a single screen with a QR code to scan, then brings you back here.")),
          PP.ui.card({ title: "Sign-in methods" },
            PP.ui.kv([["Signed in as", (ctx.user && (ctx.user.email || ctx.user.username)) || null], ["Name", ctx.user && ctx.user.name], ["Second step", on ? "Authenticator app" : "None"]]),
            el("div", { class: "pp-card-actions" },
              ACC.btn("Manage password and sign-in methods", { icon: "external", href: `${issuer()}/account/` }),
              ACC.btn("Sign in again", { kind: "quiet", onclick: () => PP.switchAccount("/" + PP.href("security")) })))));
    },
  });

  // ---- privacy and deletion ----------------------------------------------------------------------

  function deletionCard() {
    const body = el("div", { class: "acc-stack", id: "acc-deletion-body" }, PP.ui.loading(1));
    const status = ACC.statusLine();
    const receipt = (r) => body.replaceChildren(PP.ui.gate({ tone: "info", text: "Deletion request received. You can keep this reference; no further request is needed." }),
      PP.ui.kv([["Reference", PP.ui.copyable(r.request_id)], ["Complete by", ACC.date(r.complete_by)]]));
    const showForm = () => {
      const typed = el("input", { class: "pp-input", id: "acc-deletion-confirm", autocomplete: "off", spellcheck: "false" });
      const go = ACC.btn("Request account deletion", { kind: "danger", type: "submit", id: "acc-deletion-submit", disabled: true });
      typed.addEventListener("input", () => { go.disabled = typed.value !== "DELETE"; });
      body.replaceChildren(el("form", { class: "acc-stack", onsubmit: async (e) => {
        e.preventDefault();
        if (typed.value !== "DELETE" || go.disabled) return;
        go.disabled = true; typed.disabled = true;
        try {
          const r = await ACC.pb("/v1/account/deletion-request", { method: "POST", body: { confirm: true } });
          receipt(r.request);
        } catch (err) {
          status.say(`Your request was not confirmed. ${PP.explain(err)} You can safely try again.`, "error");
          go.disabled = false; typed.disabled = false;
        }
      } }, ACC.field("Type DELETE to confirm that you want this account and its associated data deleted.", typed), el("div", { class: "pp-form-actions" },
        ACC.btn("Use a different account", { onclick: () => PP.switchAccount("/" + PP.href("privacy", "delete")) }), go)), status);
    };
    (async () => {
      try {
        const r = await ACC.pb("/v1/account/deletion-request");
        if (r.account && r.account.label) body.before(el("p", {}, `Account: ${r.account.label}`));
        if (r.request) receipt(r.request); else showForm();
      } catch (e) { body.replaceChildren(PP.ui.errorState(e, () => PP.shell.refresh())); }
    })();
    return PP.ui.card({ tone: "danger", title: "Delete your Pigeonpost account", cls: "acc-deletion" },
      el("p", {}, "This requests deletion of your hosted sign-in account, mailboxes, keys, your copies of messages and attachments, contacts and account settings. You will lose your handles."),
      el("p", { class: "pp-muted" }, "We complete requests within 30 days and confirm using your verified account contact. Limited payment and security records may be kept where the law requires. Copies already delivered to other accounts are separate."),
      PP.ui.gate({ tone: "warning", text: "Store subscriptions continue until you cancel them. Cancel them first in Apple or Google Play." }),
      el("ul", { class: "acc-links" },
        el("li", {}, ACC.ext("https://apps.apple.com/account/subscriptions", "Apple subscriptions")),
        el("li", {}, ACC.ext("https://play.google.com/store/account/subscriptions?package=dev.pigeonpost.inbox", "Google Play subscriptions"))),
      body);
  }

  ACC.page({
    key: "privacy", title: "Privacy", group: "Account", icon: "privacy",
    async render(view, ctx, params) {
      view.append(PP.ui.header({ title: "Privacy", text: "Your data, and deleting your account." }));
      if (params[0] === "delete") { PP.shell.crumb("Delete account"); view.append(deletionCard()); return; }
      view.append(el("div", { class: "pp-grid" },
        PP.ui.card({ title: "Your data", text: "What we keep, why, and for how long is set out in the privacy policy." },
          el("ul", { class: "acc-links" },
            el("li", {}, ACC.ext("https://pigeonpost.dev/privacy", "Privacy policy")),
            el("li", {}, ACC.ext("https://pigeonpost.dev/terms", "Terms of service")),
            el("li", {}, el("a", { href: "mailto:privacy@pigeonpost.dev" }, "Ask for a copy of your data: privacy@pigeonpost.dev")))),
        PP.ui.card({ tone: "danger", title: "Delete your account", text: "Deletes your hosted sign-in account, mailboxes, keys, your copies of messages and attachments, and contacts. You lose your handles." },
          el("div", { class: "pp-card-actions" }, ACC.btn("Delete your account and data", { kind: "danger", href: PP.href("privacy", "delete"), id: "acc-delete-start" })))));
    },
  });

  // ---- support -----------------------------------------------------------------------------------

  // Live support conversations (/v1/support/tickets) are scoped to a publisher on the postbox, so
  // a person's help here is the help centre and the support inbox.
  ACC.page({
    key: "support", title: "Support", group: "Account", icon: "support",
    async render(view) {
      view.append(PP.ui.header({ title: "Support", text: "Answers, and a person when you need one." }),
        el("div", { class: "pp-grid" },
          PP.ui.card({ title: "Help centre", text: "Guides for inboxes, handles, Nests, billing and the apps." },
            el("div", { class: "pp-card-actions" }, ACC.btn("Open help.pigeonpost.dev", { kind: "primary", icon: "external", href: "https://help.pigeonpost.dev" }))),
          PP.ui.card({ title: "Write to us", text: "Billing questions, refunds of unspent credit, and anything that isn't working. Send it from the email on your account so we can find it." },
            el("p", {}, el("a", { href: "mailto:support@pigeonpost.dev", id: "acc-support-mail" }, "support@pigeonpost.dev"))),
          PP.ui.card({ title: "Privacy and abuse" },
            el("ul", { class: "acc-links" },
              el("li", {}, el("a", { href: "mailto:privacy@pigeonpost.dev" }, "privacy@pigeonpost.dev")),
              el("li", {}, el("a", { href: "mailto:abuse@pigeonpost.dev" }, "abuse@pigeonpost.dev")))),
          PP.ui.card({ title: "Service status" }, el("p", {}, ACC.ext("https://status.pigeonpost.dev", "status.pigeonpost.dev")))));
    },
  });
})();
