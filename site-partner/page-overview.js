// Overview: where the application stands and what to do next, with this month's earnings, live
// apps and open support conversations once there is a publisher.
//   GET /v1/publishers/{p}/listings                    live apps (approved publishers)
//   GET /v1/publishers/{p}/analytics?from&to           this month's earnings
//   GET /v1/support/tickets?publisher={p}&status=all   open conversations
(function () {
  "use strict";
  const PP = window.PP, PPP = window.PPP, ui = PP.ui;
  const el = (...a) => PP.el(...a);

  function nextStep(state) {
    switch (state) {
      case "draft": return { label: "Continue your application", href: PP.href("application") };
      case "needs_information": return { label: "Answer Pigeonpost's request", href: PP.href("application") };
      case "submitted":
      case "in_review": return { label: "View your application", href: PP.href("application") };
      case "approved": return { label: "Go to your apps", href: PP.href("apps") };
      default: return { label: "Contact support", href: PP.href("support") };
    }
  }

  /// Draft → Submitted → In review → Decision, with where this application is.
  function journey(state) {
    const order = { draft: 0, needs_information: 0, submitted: 1, in_review: 2, approved: 3, rejected: 3, revoked: 3 };
    const at = order[state] ?? 0;
    return el("ol", { class: "ppp-journey", "aria-label": "Application progress" }, PPP.JOURNEY.map(([key, label], i) => {
      let text = label;
      if (key === "decision" && at === 3) text = PPP.STATES[state].label;
      if (key === "draft" && state === "needs_information") text = "Needs information";
      const cls = i < at ? "is-done" : i === at ? "is-here" : "";
      return el("li", { class: `ppp-journey-step ${cls}`, "aria-current": i === at ? "step" : null },
        el("span", { class: "ppp-journey-dot" }, i < at ? PP.icon("check") : String(i + 1)), el("span", {}, text));
    }));
  }

  function hero(ctx) {
    const state = PPP.stateOf(ctx);
    const s = PPP.STATES[state];
    const name = (ctx.app && ctx.app.details && (ctx.app.details.public_name || ctx.app.details.legal_name)) || ctx.pub.display_name;
    const request = PPP.request(ctx.app);
    const step = nextStep(state);
    return el("section", { class: `ppp-hero ppp-hero-${s.tone}`, "aria-label": "Application status" },
      el("div", { class: "ppp-hero-main" },
        el("p", { class: "pp-eyebrow" }, "Your publisher application"),
        el("div", { class: "ppp-hero-title" }, el("h2", { translate: "no" }, name), PPP.stateBadge(state)),
        el("p", { class: "pp-lede" }, s.hint),
        ctx.pub.decision_reason && ["rejected", "revoked"].includes(state) ? el("p", { class: "ppp-reason" }, el("strong", {}, "Reason: "), ctx.pub.decision_reason) : null,
        request ? el("div", { class: "ppp-request" }, el("strong", {}, "Pigeonpost asked: "), request.note || "More information is needed.") : null,
        el("div", { class: "ppp-hero-actions" },
          ui.button(step.label, { kind: "primary", href: step.href, icon: "chevron" }),
          state !== "rejected" && state !== "revoked" ? ui.button("Get help", { href: PP.href("support"), icon: "support" }) : null)),
      journey(state));
  }

  async function figures(ctx) {
    const base = PPP.base(ctx);
    const now = new Date();
    const from = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), 1)).toISOString().slice(0, 10) + "T00:00:00Z";
    const to = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate() + 1)).toISOString().slice(0, 10) + "T00:00:00Z";
    const quiet = (p) => p.catch((e) => { if (e instanceof PP.Redirecting) throw e; return null; });
    const approved = PPP.approved(ctx);
    const [listings, analytics, tickets] = await Promise.all([
      approved ? quiet(PP.postbox(`${base}/listings`)) : null,
      approved ? quiet(PP.postbox(`${base}/analytics?${new URLSearchParams({ from, to })}`)) : null,
      quiet(PP.postbox(`/v1/support/tickets?${new URLSearchParams({ publisher: ctx.pub.id, status: "all" })}`)),
    ]);
    const rows = listings ? listings.data || [] : null;
    const fin = analytics && analytics.data && analytics.data.finance;
    let earned = null;
    if (fin && fin.status === "available") {
      const cur = (fin.currencies || []).filter((c) => Number(c.net_publisher_minor));
      earned = cur.length ? cur.map((c) => ui.money(c.net_publisher_minor, c.currency)).join(" · ") : ui.money(0, "USD");
    }
    const open = tickets ? (tickets.data || []).filter((t) => t.status !== "resolved") : null;
    return {
      stats: [
        { label: "Earnings this month", value: approved ? (earned || "—") : "—", hint: approved ? (fin && fin.status !== "available" ? "Earnings are not available right now" : "Net, after Pigeonpost's 10%") : "After approval", href: PP.href("analytics") },
        { label: "Active apps", value: rows ? rows.filter((l) => l.status === "published").length : "—", hint: rows ? `${rows.length} listing${rows.length === 1 ? "" : "s"} in total` : "After approval", href: PP.href("listings") },
        { label: "Open support", value: open ? open.length : "—", hint: open && open.some((t) => t.status === "waiting_partner") ? "Some need your reply" : "Conversations with Pigeonpost", href: PP.href("support"), tone: open && open.some((t) => t.status === "waiting_partner") ? "warning" : null },
      ],
      rows,
    };
  }

  function setup(ctx, rows) {
    const state = PPP.stateOf(ctx);
    const sent = !["none", "draft"].includes(state);
    return ui.checklist([
      { title: "Start an application", text: "Name your business and choose who publishes.", done: true },
      { title: "Submit it for review", text: "Legal, tax and bank details, documents and identity.", done: sent, action: { label: "Continue", href: PP.href("application") } },
      { title: "Get approved", text: "Pigeonpost verifies your business.", done: state === "approved" },
      { title: "Submit your first app", text: "A build of a pinned commit, or a hosted integration.", done: false, action: state === "approved" ? { label: "Open apps", href: PP.href("apps") } : null },
      { title: "Publish a listing with a price", text: "Buyers find it in the Services catalog.", done: Boolean(rows && rows.some((l) => l.status === "published")), action: state === "approved" ? { label: "Open listings", href: PP.href("listings") } : null },
    ]);
  }

  function welcome(ctx) {
    return [
      ui.header({ eyebrow: "Pigeonpost Partner", title: `Welcome${ctx.user && ctx.user.name ? `, ${ctx.user.name.split(" ")[0]}` : ""}`, text: "Publish services on Pigeonpost: apply once, then list your apps, set their prices and follow usage and earnings." }),
      el("section", { class: "ppp-hero ppp-hero-start", "aria-label": "Start your application" },
        el("div", { class: "ppp-hero-main" },
          el("p", { class: "pp-eyebrow" }, "Become a partner"),
          el("h2", {}, "Start your application"),
          el("p", { class: "pp-lede" }, "It takes about 15 minutes. Have your registration and tax documents and your bank details at hand. Individuals verify their identity online. You can stop at any step and come back."),
          PPP.startForm(ctx)),
        el("ol", { class: "ppp-start-steps" },
          [["form", "Apply", "Business type, legal and tax details, bank account, documents."],
            ["security", "Get verified", "Pigeonpost reviews every publisher, usually within a few working days."],
            ["apps", "Publish", "Submit apps, list them, set per-job or card prices."],
            ["money", "Get paid", "Monthly statements and payouts, Pigeonpost keeps 10%."]].map(([icon, title, text]) =>
            el("li", {}, el("span", { class: "ppp-start-icon" }, PP.icon(icon)), el("div", {}, el("strong", {}, title), el("p", { class: "pp-muted" }, text)))))),
    ];
  }

  PPP.page({
    key: "overview", title: "Overview", group: "", icon: "home", order: 0,
    async render(view, ctx) {
      if (ctx.disabled) {
        view.append(ui.header({ title: "Overview" }), ui.empty({ icon: "info", title: "Publishing isn't open on this account yet", text: "The Pigeonpost marketplace is being opened account by account. Contact us and we'll let you know when you can apply.", action: { label: "Contact us", href: "https://help.pigeonpost.dev" } }));
        return;
      }
      if (!ctx.pub) { view.append(...welcome(ctx)); return; }
      const picker = PPP.picker(ctx);
      view.append(ui.header({ eyebrow: ctx.pub.owner_kind === "org" ? "Organization publisher" : "Personal publisher", title: "Overview", text: "Your application, apps and earnings at a glance.", actions: picker }));
      view.append(hero(ctx));
      const f = await figures(ctx);
      view.append(ui.stats(f.stats));
      view.append(el("div", { class: "pp-cols" },
        ui.card({ title: "Getting set up", text: "Five steps from application to your first sale." }, setup(ctx, f.rows)),
        ui.card({ title: "Shortcuts" }, el("div", { class: "ppp-links" },
          [["building", "My business", "Business details, public profile and team", "business"],
            ["apps", "My apps", "Builds, integrations, reviews and test Nests", "apps"],
            ["chart", "My analytics", "Usage and earnings per app", "analytics"],
            ["support", "Support", "Talk to the Pigeonpost team", "support"]].map(([icon, title, text, key]) =>
            el("a", { class: "ppp-link", href: PP.href(key) }, el("span", { class: "ppp-link-icon" }, PP.icon(icon)), el("span", {}, el("strong", {}, title), el("span", { class: "pp-muted" }, text))))))));
    },
  });
})();
