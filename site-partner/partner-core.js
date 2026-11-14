// partner.pigeonpost.dev — what every partner page shares: who the caller is here (their
// publishers, the current one, its application), the application states, and small helpers.
//
// The postbox is the only authority. This file only reads what it says the caller may see; every
// route still checks the caller itself. Routes used here:
//   GET  /v1/publishers                                  publishers this person acts for, with role
//   GET  /v1/publisher-applications/{p}/dossier          the private application of one publisher
//   POST /v1/publisher-applications                      {display_name, org_id?}: start one
//   GET  /v1/orgs                                        organizations (to apply for, and "enterprise")
//   GET  /v1/staff/me                                    staff roles (only to offer "management")
(function () {
  "use strict";
  const PP = window.PP;
  const el = (...a) => PP.el(...a);
  const ui = PP.ui;
  const PPP = (window.PPP = window.PPP || {});
  PPP.pages = PPP.pages || [];
  PPP.page = (p) => { PPP.pages.push(p); };

  const enc = encodeURIComponent;
  PPP.enc = enc;
  PPP.base = (ctx) => `/v1/publishers/${enc(ctx.pub.id)}`;
  PPP.appBase = (ctx) => `/v1/publisher-applications/${enc(ctx.pub.id)}`;

  /// Organization roles that may apply for an organization, and publisher roles that may change it.
  const ORG_APPLY_ROLES = ["owner", "admin"];
  const ADMIN_ROLES = ["owner", "org_admin", "publisher_admin"];
  PPP.ROLE_TEXT = { owner: "Owner", org_admin: "Organization admin", publisher_admin: "Publisher admin", publisher_finance: "Publisher finance (read only)" };

  /// Application states, with what they mean and what to do next.
  PPP.STATES = {
    none: { label: "Not started", tone: "neutral", hint: "Tell us about your business to start publishing on Pigeonpost." },
    draft: { label: "Draft", tone: "neutral", hint: "Finish the steps of your application and submit it for review. Your progress is saved." },
    submitted: { label: "Submitted", tone: "info", hint: "Your application is in the review queue. Updates and any requests from Pigeonpost appear here." },
    in_review: { label: "In review", tone: "info", hint: "Pigeonpost is reviewing your application. Nothing is needed from you right now." },
    needs_information: { label: "Needs information", tone: "warning", hint: "Pigeonpost asked for more information. Answer the request and submit again." },
    approved: { label: "Approved", tone: "success", hint: "Your business is verified. You can submit apps, list them and set prices." },
    rejected: { label: "Rejected", tone: "danger", hint: "Your application was not approved. Read the reason below, or contact support to talk it through." },
    revoked: { label: "Revoked", tone: "danger", hint: "Your publishing certification was revoked and your listings are suspended. Contact support." },
  };
  /// The journey shown on the overview: where a state sits.
  PPP.JOURNEY = [["draft", "Draft"], ["submitted", "Submitted"], ["in_review", "In review"], ["decision", "Decision"]];

  /// The application state of the current publisher: the dossier's when it is known, else the
  /// publisher's certification status.
  PPP.stateOf = function (ctx) {
    if (!ctx || !ctx.pub) return "none";
    if (ctx.pub.status === "revoked") return "revoked";
    if (ctx.app && ctx.app.state) return ctx.app.state;
    return { applied: "draft", approved: "approved", rejected: "rejected", revoked: "revoked" }[ctx.pub.status] || "draft";
  };
  PPP.approved = (ctx) => Boolean(ctx && ctx.pub && ctx.pub.status === "approved");
  PPP.isAdmin = (ctx) => Boolean(ctx && ctx.pub && ADMIN_ROLES.includes(ctx.pub.role));
  PPP.canEdit = (ctx) => PPP.approved(ctx) && PPP.isAdmin(ctx);
  PPP.editable = (ctx) => Boolean(ctx && ctx.app && ctx.pub && ctx.pub.status === "applied" && ["draft", "needs_information"].includes(ctx.app.state));

  /// The latest staff request for more information, when the application is waiting on it.
  PPP.request = function (app) {
    if (!app || app.state !== "needs_information") return null;
    const e = (app.history || []).find((h) => h.event === "needs_information");
    return { note: e ? e.note : "", at: e ? e.created_at : null, documents: app.requested_documents || [] };
  };

  // ---- context ---------------------------------------------------------------------------------

  const PUB_KEY = "pp_partner_publisher";
  PPP.choose = function (id) {
    PP.store.set(PUB_KEY, id);
    return PP.shell.reload();
  };

  /// Who the caller is on partner.: their publishers, the current one and its application, and
  /// which other platforms they have. Only `/v1/publishers` may fail the page; the probes for
  /// other platforms never do.
  PPP.context = async function () {
    const probe = (path, pick) => PP.postbox(path, { quiet: true }).then(pick).catch((e) => { if (e instanceof PP.Redirecting) throw e; return null; });
    const [list, staffRoles, orgs] = await Promise.all([
      PP.postbox("/v1/publishers").then((r) => ({ data: (r && r.data) || [] })).catch((e) => {
        if (e instanceof PP.Redirecting) throw e;
        if (e && (e.code === "feature_disabled" || e.status === 403 || e.status === 404)) return { disabled: true, data: [] };
        throw e;
      }),
      probe("/v1/staff/me", (r) => (r && r.data && Array.isArray(r.data.roles) ? r.data.roles : [])),
      probe("/v1/orgs", (r) => (r && Array.isArray(r.orgs) ? r.orgs : [])),
    ]);
    const publishers = list.data;
    const stored = PP.store.get(PUB_KEY);
    const pub = publishers.find((p) => p.id === stored) || publishers[0] || null;
    let app = null;
    let appError = null;
    if (pub) {
      try {
        const r = await PP.postbox(`/v1/publisher-applications/${enc(pub.id)}/dossier`);
        app = r && r.data && r.data.details ? r.data : null;
      } catch (e) {
        if (e instanceof PP.Redirecting) throw e;
        appError = e;
      }
    }
    const platforms = ["account"];
    if ((orgs || []).some((o) => (o.my_roles || []).some((r) => ORG_APPLY_ROLES.includes(r)))) platforms.push("enterprise");
    if ((staffRoles || []).length) platforms.push("management");
    return {
      user: PP.whoami(),
      platforms,
      disabled: Boolean(list.disabled),
      publishers,
      pub,
      app,
      appError,
      orgs: (orgs || []).filter((o) => (o.my_roles || []).some((r) => ORG_APPLY_ROLES.includes(r))),
      staffRoles: staffRoles || [],
    };
  };

  /// Keep the application everyone reads in step after a write.
  PPP.setApp = function (ctx, app) {
    if (app && app.details) ctx.app = app;
    return ctx.app;
  };

  // ---- shared pieces -----------------------------------------------------------------------------

  /// The publisher picker, when a person acts for more than one.
  PPP.picker = function (ctx) {
    if (!ctx.publishers || ctx.publishers.length < 2) return null;
    const sel = el("select", { class: "pp-input pp-select", "aria-label": "Business", id: "ppp-business-picker" },
      ctx.publishers.map((p) => el("option", { value: p.id, selected: ctx.pub && p.id === ctx.pub.id ? true : null }, p.display_name)));
    sel.addEventListener("change", () => PPP.choose(sel.value));
    return el("label", { class: "ppp-picker" }, el("span", { class: "pp-muted" }, "Business"), sel);
  };

  /// "Start your application": a name buyers will see, and who publishes (you or an organization).
  PPP.startForm = function (ctx, { compact } = {}) {
    const fields = [{ name: "display_name", label: "Business or publisher name", required: true, maxlength: 80, placeholder: "Acme Labs", hint: "The name buyers see next to your apps. You can change the public profile later.", autocomplete: "organization" }];
    if (ctx.orgs && ctx.orgs.length) fields.push({ name: "org_id", label: "Publish as", type: "select", value: "", options: [["", "Myself (individual or my own company)"], ...ctx.orgs.map((o) => [o.id, `${o.name} (organization)`])] });
    return ui.form(fields, {
      submit: compact ? "Start application" : "Start your application",
      cls: "ppp-start-form",
      onSubmit: async (v) => {
        const name = v.display_name;
        if (name.length < 2) throw new Error("Use at least 2 characters.");
        const body = { display_name: name };
        if (v.org_id) body.org_id = v.org_id;
        const r = await PP.postbox("/v1/publisher-applications", { method: "POST", body });
        const id = r && r.data && (r.data.id || r.data.publisher_id);
        if (id) PP.store.set(PUB_KEY, id);
        await PP.shell.reload();
        PP.go("application");
        return "Your application is started.";
      },
    });
  };

  /// What a page shows when there is no publisher yet.
  PPP.noPublisher = function (ctx, what) {
    return ui.empty({ icon: "form", title: "Start your application first", text: `${what} opens once you have started an application to publish on Pigeonpost.`, action: { label: "Start your application", href: PP.href("overview") } });
  };

  /// "Finish your application first", above an area that is shown but disabled.
  PPP.gate = function (ctx, text, ...area) {
    const state = PPP.stateOf(ctx);
    const action = state === "revoked" || state === "rejected" ? { label: "Contact support", href: PP.href("support") } : { label: state === "draft" || state === "needs_information" ? "Finish your application" : "View your application", href: PP.href("application") };
    return ui.gate({ text, action }, ...area);
  };

  PPP.stateBadge = (state) => { const s = PPP.STATES[state] || PPP.STATES.draft; return ui.badge(s.label, s.tone); };

  /// A listing's logo: its uploaded image from the postbox, or its initials on a tone of its own.
  const LOGO_PATH = /^\/v1\/marketplace\/listings\/[^/]+\/logo\/[0-9a-f]{16}\/(64|128|512)\.webp$/;
  PPP.initials = function (title) {
    const words = String(title || "").replace(/\([^)]*\)/g, " ").split(/[^\p{L}\p{N}]+/u).filter(Boolean);
    if (!words.length) return "?";
    if (words.length === 1) { const w = Array.from(words[0]); return w[0].toLocaleUpperCase() + (w[1] || "").toLocaleLowerCase(); }
    return (Array.from(words[0])[0] + Array.from(words[1])[0]).toLocaleUpperCase();
  };
  PPP.tone = function (id) {
    let h = 0x811c9dc5;
    const s = String(id || "");
    for (let i = 0; i < s.length; i++) { h ^= s.charCodeAt(i); h = Math.imul(h, 0x01000193) >>> 0; }
    return h % 8;
  };
  PPP.logo = function (l, size = 40) {
    const urls = l && l.logo && l.logo.urls;
    const pick = urls && (size > 64 ? urls["512"] : urls["128"]);
    const src = pick && LOGO_PATH.test(pick) ? (window.PP_CONFIG.postbox || "") + pick : null;
    const tile = () => el("span", { class: `ppp-logo ppp-logo-${size} ppp-tone-${PPP.tone(l && l.id)}`, "aria-hidden": "true" }, PPP.initials(l && l.title));
    if (!src) return tile();
    const img = el("img", { class: `ppp-logo ppp-logo-${size}`, src, alt: "", width: size, height: size, loading: "lazy" });
    img.addEventListener("error", () => img.replaceWith(tile()), { once: true });
    return img;
  };

  PPP.CATEGORIES = [
    ["productivity", "Productivity"], ["legal", "Legal"], ["finance", "Finance"], ["support", "Customer support"],
    ["marketing", "Marketing"], ["sales", "Sales"], ["language", "Language"], ["research", "Research"],
    ["developer", "Developer tools"], ["hr", "HR"], ["recruiting", "Recruiting"], ["other", "Other"],
  ];

  PPP.priceText = function (l) {
    if (l.hosting === "publisher") {
      return l.price_from && Number(l.price_from.amount_cents) > 0 ? `From ${ui.money(l.price_from.amount_cents, "USD")} per job` : "Priced per job";
    }
    const p = l.price;
    if (!p || !Number(p.amount_minor)) return "Free";
    const m = ui.money(p.amount_minor, p.currency);
    return p.term === "monthly" ? `${m} a month` : `${m} one-off`;
  };

  PPP.count = (n) => Number(n || 0).toLocaleString(PP.locale());

  /// Read a file as base64 (the documents route takes JSON).
  PPP.fileBase64 = function (file) {
    return new Promise((resolve, reject) => {
      const r = new window.FileReader();
      r.onload = () => resolve(String(r.result).split(",")[1] || "");
      r.onerror = () => reject(new Error("That file could not be read."));
      r.readAsDataURL(file);
    });
  };

  /// Save a base64 file the postbox returned, without opening it in this origin.
  PPP.saveBase64 = function (filename, b64, type = "application/octet-stream") {
    const bytes = Uint8Array.from(window.atob(b64 || ""), (c) => c.charCodeAt(0));
    const url = URL.createObjectURL(new window.Blob([bytes], { type }));
    const a = el("a", { href: url, download: filename || "document" });
    document.body.append(a);
    a.click();
    a.remove();
    window.setTimeout(() => URL.revokeObjectURL(url), 1000);
  };

  /// Pages register cleanups (a live socket) that run when the person leaves the page.
  const leaving = [];
  PPP.onLeave = (fn) => { leaving.push(fn); };
  PPP.leave = function () {
    while (leaving.length) { const fn = leaving.pop(); try { fn(); } catch { /* never block navigation */ } }
  };
  window.addEventListener("hashchange", () => PPP.leave());

  // ---- step-up resume ----------------------------------------------------------------------------
  //
  // A price change, a publish or a withdraw may answer 401 step_up_required. PP.api then leaves
  // for the realm with this resume; back here, the same call runs once more and the page returns
  // to where it was. Only publisher routes are ever replayed.
  PPP.stepped = function (path, method, body, back) {
    return { name: "partner.replay", payload: { path, method, body, back } };
  };
  PP.onResume("partner.replay", async (p) => {
    if (!p || typeof p.path !== "string" || !/^\/v1\/publishers\/[^/]+\/listings\//.test(p.path)) return;
    if (!["PUT", "POST", "DELETE", "PATCH"].includes(p.method)) return;
    if (p.back && /^#\/[\w/%.-]*$/.test(p.back)) window.history.replaceState(null, "", p.back);
    await PP.postbox(p.path, { method: p.method, body: p.body });
    PP.toast("Done. Your change is saved.", "success");
  });
  /// A write that may need a recent strong sign-in.
  PPP.strong = function (path, method, body) {
    return PP.postbox(path, { method, body, resume: PPP.stepped(path, method, body, window.location.hash) });
  };
})();
