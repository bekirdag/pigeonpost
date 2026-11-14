// Overview: a greeting with the person's real name, the five numbers that matter and a short
// "Get started" checklist (billing details, two-factor, first handle).
(function () {
  "use strict";
  const el = (...a) => PP.el(...a);

  function greeting(now = new Date()) {
    const h = now.getHours();
    return h < 5 ? "Good evening" : h < 12 ? "Good morning" : h < 18 ? "Good afternoon" : "Good evening";
  }

  ACC.page({
    key: "overview", title: "Overview", group: "", icon: "home",
    async render(view, ctx) {
      const first = String((ctx.user && ctx.user.name) || "").trim().split(/\s+/)[0];
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
      const stat = (label, value, hint, href, tone) => ({ label, value, hint, href, tone });
      const tiles = [
        stat("Inboxes", ids.status === "fulfilled" ? ids.value.length : "—",
          ids.status === "fulfilled" ? (ids.value.length ? `${ids.value.filter((i) => i.handle).length} with a handle` : "Create one for an agent") : "Could not load", PP.href("inboxes")),
        stat("Handles", handles ? handles.length : "—", handles ? (handles.length ? `${handles.filter((h) => h.active).length} active` : "Get your first one") : "Could not load", PP.href("handles")),
        nests ? stat("Active Nests", nest.status === "fulfilled" && nest.value ? (nest.value.nests || []).filter((n) => n.status !== "unavailable" && n.status !== "paused").length : "—",
          nest.status === "fulfilled" && nest.value && nest.value.allowance ? `${nest.value.allowance.used} of ${nest.value.allowance.slots} in your package` : "Private agent workspaces", PP.href("nests")) : null,
        marketplace ? stat("Credit balance", bal.status === "fulfilled" && bal.value ? ACC.usd(bal.value.available_cents) : "—",
          bal.status === "fulfilled" && bal.value && bal.value.held_cents > 0 ? `${ACC.usd(bal.value.held_cents)} reserved for runs` : "For marketplace services", PP.href("wallet")) : null,
        stat("Plan", plan ? (plan.capacity > 0 ? `${plan.capacity} name${plan.capacity === 1 ? "" : "s"}` : "No plan") : "—",
          plan ? (plan.capacity > 0 ? `${plan.used} registered${plan.paid_through ? ` · paid through ${ACC.date(plan.paid_through)}` : ""}` : "Choose one to register names") : "Could not load", PP.href("plan")),
      ];
      const steps = [
        { title: "Add your billing details", text: "Plans, handles, Nests and credits are invoiced to them.", done: Boolean(o && o.billing.has), action: { label: "Add details", href: PP.href("billing") } },
        { title: "Turn on two-factor sign-in", text: "A code from an authenticator app protects this account.", done: Boolean(ctx.twoStep), action: { label: "Set it up", href: PP.href("security") } },
        { title: "Get your first handle", text: "A name like /you that people and agents can reach.", done: Boolean(handles && handles.length), action: { label: "Find a name", href: PP.href("handles", "get") } },
      ];
      const left = steps.filter((s) => !s.done).length;
      view.append(
        PP.ui.header({ eyebrow: "Your Pigeonpost", title: first ? `${greeting()}, ${first}` : "Welcome back", text: "Your inboxes, handles, Nests and purchases in one place." }),
        PP.ui.stats(tiles),
        el("div", { class: "pp-cols" },
          PP.ui.card({ title: "Get started", text: left ? `${left} of ${steps.length} steps left` : "All set. Nice work." }, PP.ui.checklist(steps)),
          PP.ui.card({ title: "Quick actions" }, el("div", { class: "acc-quick" },
            quick("at", "Get a handle", "Search for a name like /you", PP.href("handles", "get")),
            quick("agents", "Connect an AI agent", "A hosted inbox for Claude or ChatGPT", PP.href("inboxes")),
            nests ? quick("nest", "Buy a Nest", "A private AI workspace", PP.href("nests")) : null,
            marketplace ? quick("store", "Browse services", "Reviewed apps from verified publishers", PP.href("store", "services")) : null,
            quick("invoice", "Invoices", "Download receipts as PDF", PP.href("invoices"))))));
    },
  });

  function quick(icon, title, text, href) {
    return el("a", { class: "acc-quick-item", href }, PP.icon(icon), el("span", {}, el("strong", {}, title), el("small", {}, text)), PP.icon("chevron", "acc-quick-go"));
  }
})();
