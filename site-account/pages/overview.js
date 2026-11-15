// Overview (kit: templates/account.html): a greeting with the person's real name, the numbers
// that matter as stat cards, a "Get started" checklist, and beside it the person's Nest (when they
// have one) and quick actions.
(function () {
  "use strict";
  const el = (...a) => PP.el(...a);

  function greeting(now = new Date()) {
    const h = now.getHours();
    return h < 5 ? "Good evening" : h < 12 ? "Good morning" : h < 18 ? "Good afternoon" : "Good evening";
  }

  // The postbox's Nest states → the kit's Nest status chip.
  const NEST_KIT = { ready: ["running", "Ready"], working: ["running", "Working"], needs_you: ["error", "Needs you"], paused: ["paused", "Paused"], unavailable: ["error", "Unavailable"] };

  /// The first usable Nest as the kit's Nest card: name, address, package and how full it is.
  function nestCard(d) {
    const nests = (d && d.nests) || [];
    const n = nests.find((x) => x.status !== "unavailable") || nests[0];
    if (!n) return null;
    const [kit, label] = NEST_KIT[n.status] || ["sleeping", String(n.status || "Unknown").replace(/_/g, " ")];
    const a = d.allowance || {};
    const slots = Number(a.slots) || 0;
    const used = Number(a.used) || 0;
    const fill = el("span", { class: "pp-meter__fill" });
    fill.style.setProperty("--value", `${slots ? Math.min(100, Math.round((used / slots) * 100)) : 0}%`);
    const paid = a.funding && a.funding.paid_through ? `paid through ${ACC.date(a.funding.paid_through)}` : "";
    return el("article", { class: "pp-nest-card", "data-status": kit, id: "acc-nest-card" },
      el("div", { class: "pp-nest-card__top" },
        el("span", { class: "pp-nest-card__avatar", "aria-hidden": "true" }, PP.icon("bird")),
        el("div", { class: "pp-nest-card__id" }, el("h3", { class: "pp-nest-card__name" }, n.label || "Your Nest"), n.address ? el("p", { class: "pp-nest-card__handle", translate: "no" }, n.address) : null),
        el("span", { class: "pp-nest-status", "data-status": kit }, label)),
      d.package && d.package.name ? el("p", { class: "pp-nest-card__package" }, el("strong", {}, d.package.name), paid ? el("span", {}, paid) : null) : null,
      slots ? el("div", { class: "pp-nest-card__meters" }, el("div", { class: "pp-meter" },
        el("p", { class: "pp-meter__label" }, el("span", {}, "Nests in your package"), el("span", { class: "pp-meter__value" }, `${used} of ${slots}`)),
        el("div", { class: "pp-meter__track", role: "progressbar", "aria-label": "Nests in your package", "aria-valuenow": String(used), "aria-valuemin": "0", "aria-valuemax": String(slots) }, fill))) : null,
      el("div", { class: "pp-nest-card__foot" },
        n.address ? ACC.btn("Open in inbox", { kind: "primary", icon: "inbox", href: `https://inbox.pigeonpost.dev/#peer=${encodeURIComponent(n.address)}` }) : null,
        el("a", { class: "pp-btn pp-nest-card__icon-btn", href: PP.href("nests"), "aria-label": "Manage your Nests" }, PP.icon("settings"))));
  }

  ACC.page({
    key: "overview", title: "Overview", group: "", icon: "home",
    async render(view, ctx) {
      const first = ((n) => (/@/.test(n) ? "" : n))(String((ctx.user && ctx.user.name) || "").trim().split(/\s+/)[0]);
      const marketplace = ACC.feature(ctx, "marketplace");
      const nests = ACC.feature(ctx, "nests");
      const [ov, ids, nest, bal] = await Promise.allSettled([
        ACC.overview(),
        ACC.identities(),
        nests ? ACC.pb("/v1/nests") : Promise.resolve(null),
        marketplace ? ACC.balance() : Promise.resolve(null),
      ]);
      const o = ov.status === "fulfilled" ? ov.value : null;
      const handles = o ? o.handles : null;
      const plan = o ? o.plan : null;
      const stat = (icon, label, value, hint, href, extra = {}) => ({ icon, label, value, hint, href, ...extra });
      const nd = nest.status === "fulfilled" ? nest.value : null;
      const tiles = [
        stat("inbox", "Inboxes", ids.status === "fulfilled" ? ids.value.length : "—",
          ids.status === "fulfilled" ? (ids.value.length ? `${ids.value.filter((i) => i.handle).length} with a handle` : "Create one for an agent") : "Could not load", PP.href("inboxes")),
        stat("at", "Handles", handles ? handles.length : "—", handles ? (handles.length ? `${handles.filter((h) => h.active).length} active` : "Get your first one") : "Could not load", PP.href("handles"),
          handles && handles.some((h) => !h.active) ? { hintTone: "bad" } : {}),
        nests ? stat("nest", "Active Nests", nd ? (nd.nests || []).filter((n) => n.status !== "unavailable" && n.status !== "paused").length : "—",
          nd && nd.allowance ? `${nd.allowance.used} of ${nd.allowance.slots} in your package` : "Private agent workspaces", PP.href("nests")) : null,
        marketplace ? stat("wallet", "Credit balance", bal.status === "fulfilled" && bal.value ? ACC.usd(bal.value.available_cents) : "—",
          bal.status === "fulfilled" && bal.value && bal.value.held_cents > 0 ? `${ACC.usd(bal.value.held_cents)} reserved for runs` : "For marketplace services", PP.href("wallet")) : null,
        stat("plan", "Plan", plan ? (plan.capacity > 0 ? `${plan.capacity} name${plan.capacity === 1 ? "" : "s"}` : "No plan") : "—",
          plan ? (plan.capacity > 0 ? `${plan.used} registered${plan.paid_through ? ` · paid through ${ACC.date(plan.paid_through)}` : ""}` : "Choose one to register names") : "Could not load", PP.href("plan")),
      ];
      const steps = [
        { title: "Add your billing details", text: "Plans, handles, Nests and credits are invoiced to them.", done: Boolean(o && o.billing.has), action: { label: "Add details", href: PP.href("billing"), kind: "primary" } },
        { title: "Turn on two-factor sign-in", text: "A code from an authenticator app protects this account.", done: Boolean(ctx.twoStep), action: { label: "Set it up", href: PP.href("security") } },
        { title: "Get your first handle", text: "A name like /you that people and agents can reach.", done: Boolean(handles && handles.length), action: { label: "Find a name", href: PP.href("handles", "get") } },
      ];
      const done = steps.filter((s) => s.done).length;
      const side = el("div", { class: "acc-stack" },
        nestCard(nd),
        PP.ui.card({ title: "Quick actions", icon: "zap" }, el("div", { class: "acc-quick" },
          quick("at", "Get a handle", "Search for a name like /you", PP.href("handles", "get")),
          quick("agents", "Connect an AI agent", "A hosted inbox for Claude or ChatGPT", PP.href("inboxes")),
          nests ? quick("nest", "Buy a Nest", "A private AI workspace", PP.href("nests")) : null,
          marketplace ? quick("store", "Browse services", "Reviewed apps from verified publishers", PP.href("store", "services")) : null,
          quick("invoice", "Invoices", "Download receipts as PDF", PP.href("invoices")))));
      view.append(
        PP.ui.header({ title: first ? `${greeting()}, ${first}` : greeting(), text: "Your inboxes, handles, Nests and purchases in one place.",
          actions: [ACC.btn("Open inbox", { icon: "external", href: "https://inbox.pigeonpost.dev" }), ACC.btn("Connect an agent", { kind: "primary", icon: "plus", href: PP.href("inboxes") })] }),
        PP.ui.stats(tiles),
        el("div", { class: "pp-cols acc-overview-cols" },
          PP.ui.card({ title: "Get started", icon: "rocket", text: done === steps.length ? "All set. Nice work." : `${done} of ${steps.length} done`, actions: el("span", { class: "pp-caption" }, `${Math.round((done / steps.length) * 100)}%`) }, PP.ui.checklist(steps)),
          side));
    },
  });

  function quick(icon, title, text, href) {
    return el("a", { class: "acc-quick-item", href }, PP.icon(icon), el("span", {}, el("strong", {}, title), el("small", {}, text)), PP.icon("chevron", "acc-quick-go"));
  }
})();
