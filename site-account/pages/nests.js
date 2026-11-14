// Nests: the person's Nests (status, open in the inbox), the package that funds them, creating a
// Nest in a free slot, retrying a setup, changing or cancelling a web package, and buying one.
// Company Nests belong to enterprise.pigeonpost.dev.
//
// Postbox: GET /v1/nest-packages, GET/POST /v1/nests, POST /v1/nest-setups/{id}/retry.
// Bridge: GET /v1/nests (prices, web subscriptions), GET /v1/nests/checkout/{intent},
//         POST /v1/nests/subscriptions/{id}/(upgrade|cancel). Return: `?nest_checkout=` (+`nest_org`).
(function () {
  "use strict";
  const el = (...a) => PP.el(...a);
  const STATUS = { ready: ["Ready", "success"], working: ["Working…", "info"], needs_you: ["Needs you", "warning"], paused: ["Paused", "neutral"], unavailable: ["Unavailable", "danger"] };
  const REASON = {
    payment: "Its package is not paid for. Renew or buy a package to bring it back.",
    over_capacity: "Your package has fewer Nests than you have. Delete one or upgrade.",
    setting_up: "Setting up — this takes a few seconds.",
    failed: "Setup failed. Retry it or contact support.",
  };
  const RAIL = { apple: "the App Store", google: "Google Play", iyzico: "Pigeonpost (web)", grant: "a Pigeonpost grant", test: "a test grant" };
  const CURRENCY = { USD: "US dollar", EUR: "Euro", TRY: "Turkish lira" };
  const gib = (b) => `${Math.round((Number(b) || 0) / (1024 * 1024 * 1024))} GB`;
  const tokens = (n) => (n >= 1e6 ? `${(n / 1e6).toLocaleString()}M` : `${Math.round(n / 1e3)}k`);

  ACC.packageLines = (p) => [
    `${p.slots} Nest${p.slots === 1 ? "" : "s"}`,
    `${gib(p.storage_bytes)} of files, shared`,
    `${p.active_hours} running hours a month, shared`,
    p.model === "managed" ? `AI included: ${tokens(p.input_tokens)} input + ${tokens(p.output_tokens)} output tokens a month, shared` : "No AI connected: a private workspace you set up yourself",
  ];

  /// Packages, prices and this account's Nests in one load.
  ACC.nestData = (force) => ACC.cached("nests", async () => {
    const [packages, nests, bridge] = await Promise.all([
      ACC.pb("/v1/nest-packages"),
      ACC.pb("/v1/nests"),
      ACC.bridgeGet("/v1/nests", { keepSession: true }).catch(() => null),
    ]);
    return { packages, nests, bridge };
  }, force);

  let currency = null;
  function prices(d) {
    const plans = ((d.bridge && d.bridge.prices && d.bridge.prices.plans) || []).filter((p) => p.available !== false);
    const open = d.bridge && d.bridge.prices && d.bridge.prices.open_currencies;
    const currencies = [...new Set(plans.map((p) => p.currency))].filter((c) => !open || open.includes(c));
    if (!currency || !currencies.includes(currency)) currency = currencies.includes(ACC.currency()) ? ACC.currency() : currencies[0] || null;
    return { plans, currencies };
  }

  /// The package cards with Buy, under the billing gate. Shared with Store → Nests.
  ACC.nestPackages = function (d, ov, redraw) {
    const { plans, currencies } = prices(d);
    const sellable = ((d.packages && d.packages.packages) || []).filter((p) => p.sellable);
    if (!sellable.length) return PP.ui.empty({ icon: "nest", title: "No packages on sale", text: "Nest packages are not on sale right now." });
    const pick = currencies.length > 1 ? ACC.select(currencies.map((c) => [c, CURRENCY[c] || c]), currency, { id: "acc-nest-currency", "aria-label": "Currency" }) : null;
    if (pick) pick.addEventListener("change", () => { currency = pick.value; ACC.setCurrency(currency); redraw(); });
    const tiles = el("div", { class: "pp-tiles", id: "acc-nest-packages" }, sellable.map((p) => {
      const plan = plans.find((x) => x.package === p.code && x.currency === currency);
      return el("article", { class: "pp-tile acc-package", "data-package": p.code },
        el("div", { class: "pp-tile-head" }, el("span", { class: "pp-tile-logo", "aria-hidden": "true" }, PP.icon("nest")), el("div", { class: "pp-tile-title" }, el("strong", {}, p.name), el("span", {}, p.model === "managed" ? "AI included" : "Bring your own AI"))),
        el("ul", { class: "acc-ticks" }, ACC.packageLines(p).map((l) => el("li", {}, PP.icon("check"), l))),
        el("div", { class: "pp-tile-foot" },
          plan ? el("span", { class: "pp-price" }, `${ACC.money(plan.amount, plan.currency)} a month`) : el("span", { class: "pp-muted" }, "Not available to buy here yet"),
          plan ? ACC.attr(ACC.btn("Buy", { kind: "primary", size: "sm", onclick: () => {
            if (!ov.billing.profileId) { PP.go("billing"); return; }
            ACC.startCheckout({ kind: "nest", intent_id: ACC.hex32(), plan_code: plan.code, billing_profile_id: ov.billing.profileId, locale: ACC.locale() });
          } }), { "aria-label": `Buy ${p.name}`, "data-buy": plan.code }) : null));
    }));
    return ACC.billingGate(ov, pick ? el("div", { class: "acc-toolbar" }, ACC.field("Currency", pick)) : null, tiles,
      el("p", { class: "pp-hint" }, "Sold by Piyote Bilişim and billed monthly through iyzico. Cancel any time; your Nests keep running until the end of the paid month, then you have 30 days to export before they are deleted."));
  };

  function nestTable(list) {
    return PP.ui.table({
      caption: "Your Nests",
      columns: [
        { key: "label", label: "Nest", sort: true, render: (n) => el("strong", {}, n.label) },
        { key: "status", label: "Status", render: (n) => PP.ui.status(n.status || "unavailable", STATUS) },
        { key: "reason", label: "Note", render: (n) => (n.status === "needs_you" && n.delete_after ? `Deleted on ${ACC.date(n.delete_after)} unless it is paid for.` : REASON[n.status_reason] || "") },
        { key: "open", label: "", render: (n) => (n.address ? ACC.btn("Open in inbox", { size: "sm", icon: "external", href: `https://inbox.pigeonpost.dev/#peer=${encodeURIComponent(n.address)}` }) : null) },
      ],
      rows: list,
      empty: { icon: "nest", title: "No Nests yet", text: "Create one in a free slot of your package." },
    });
  }

  async function returning(view) {
    const intent = ACC.param("nest_checkout");
    const company = /^org_[a-z0-9]{6,64}$/.test(ACC.param("nest_org") || "");
    if (!intent && !company) return;
    ACC.clearParam("nest_checkout", "nest_org");
    if (!intent || !ACC.INTENT.test(intent)) return;
    const banner = ACC.statusLine();
    view.append(banner);
    banner.say("Confirming your payment…");
    const r = await ACC.poll(`/v1/nests/checkout/${intent}`, (x) => (["active", "canceled"].includes(x.status)
      ? { text: company ? "The company's Nest package is active. Its admins create company Nests in Enterprise." : "Your Nest package is active. Your first Nest is being set up.", tone: "ok" }
      : x.status === "failed" ? { text: "The payment did not go through. Nothing was charged.", tone: "error" } : null));
    if (!r) return;
    banner.say(r.text, r.tone, company ? ACC.ext("https://enterprise.pigeonpost.dev/app", "Open Enterprise") : null);
    ACC.forget("nests");
  }

  ACC.page({
    key: "nests", title: "Nests", group: "Nests & services", icon: "nest",
    async render(view, ctx) {
      view.append(PP.ui.header({ title: "Nests", text: "Private workspaces where your agents and installed services run." }));
      if (!ACC.feature(ctx, "nests")) { view.append(PP.ui.card({}, ACC.notYet("Nests aren't available for your account yet", "When they open for you, your Nests and packages appear here.", "nest"))); return; }
      returning(view).catch(() => {});
      const [d, ov] = await Promise.all([ACC.nestData(true), ACC.overview()]);
      const draw = () => { PP.shell.refresh(); };
      const n = d.nests || {};
      const a = n.allowance || { slots: 0, used: 0, duplicates: [] };
      const funding = a.funding;
      const subs = (d.bridge && d.bridge.subscriptions) || [];
      if (funding) {
        view.append(PP.ui.stats([
          { label: "Package", value: (n.package && n.package.name) || funding.package, hint: `Bought through ${RAIL[funding.source] || funding.source}` },
          { label: "Nests in use", value: `${a.used} of ${a.slots}` },
          { label: "Paid through", value: funding.paid_through ? ACC.date(funding.paid_through) : "—" },
        ]));
        if (funding.source === "apple" || funding.source === "google") view.append(PP.ui.gate({ tone: "info", text: `Change or cancel this package in ${RAIL[funding.source]}. Buying another one here would charge you twice.` }));
      }
      if ((a.duplicates || []).length) view.append(PP.ui.gate({ tone: "danger", text: `You are paying for ${a.duplicates.length} more Nest package${a.duplicates.length === 1 ? "" : "s"} than you use. Cancel the extra one where you bought it and ask for a refund; it does not add Nests.` }));
      for (const s of n.setups || []) {
        view.append(PP.ui.gate({ tone: s.status === "failed" ? "danger" : "info", text: s.status === "failed" ? `Paid — setting up “${s.label}” failed.` : `Paid — setting up “${s.label}”…`,
          action: ACC.btn("Retry now", { size: "sm", onclick: async () => {
            try { await ACC.pb(`/v1/nest-setups/${encodeURIComponent(s.setup_id)}/retry`, { method: "POST", body: {} }); PP.toast("Trying again…"); }
            catch { PP.toast("Couldn't retry right now. We keep trying automatically.", "error"); }
            draw();
          } }) }));
      }
      const yours = PP.ui.section({ title: "Your Nests",
        actions: funding && a.used < a.slots ? ACC.btn("Create a Nest", { kind: "primary", icon: "plus", onclick: () => createSheet(a) }) : null }, nestTable(n.nests || []));
      view.append(yours);
      if (subs.length) view.append(subscriptionCard(subs[0], d, ov));
      const buyable = !funding || ["iyzico", "grant", "test"].includes(funding.source);
      if (!subs.length && buyable) view.append(PP.ui.section({ title: funding ? "Packages" : "Buy a Nest package", text: "Monthly packages; each one funds a number of Nests." }, ACC.nestPackages(d, ov, draw)));
      if ((ctx.platforms || []).includes("enterprise")) view.append(PP.ui.gate({ tone: "info", text: "Company Nests are managed by your organization in Enterprise.", action: { label: "Open Enterprise", href: "https://enterprise.pigeonpost.dev/app" } }));
    },
  });

  function createSheet(a) {
    PP.openSheet("Create a Nest", (body) => {
      body.append(el("p", { class: "pp-muted" }, `${a.slots - a.used} of ${a.slots} slots free in your package.`),
        PP.ui.form([{ name: "label", label: "Name", required: true, maxlength: 64 }], {
          submit: "Create",
          onSubmit: async (v) => {
            try { await ACC.pb("/v1/nests", { method: "POST", body: { label: v.label } }); }
            catch (e) {
              throw new Error({ nest_slots_full: "Your package has no free Nest. Delete one or upgrade.", no_nest_package: "Buy a Nest package first.", bad_label: "Names are 1 to 64 printable characters." }[ACC.code(e)] || PP.explain(e));
            }
            PP.closeSheet();
            PP.toast("Nest created.", "success");
            ACC.forget("nests");
            PP.shell.refresh();
          },
        }));
    });
  }

  function subscriptionCard(sub, d, ov) {
    const { plans } = prices(d);
    const others = plans.filter((p) => p.currency === sub.currency && p.code !== sub.plan_code);
    const name = (code) => (((d.packages && d.packages.packages) || []).find((p) => p.code === code) || {}).name || code;
    const pick = others.length ? ACC.select(others.map((p) => [p.code, name(p.package)]), "", { id: "acc-nest-change" }) : null;
    return PP.ui.card({ title: "Your web Nest package", text: `${sub.currency}${sub.recurring_price ? ` · ${ACC.money(sub.recurring_price.amount, sub.recurring_price.currency)} a month` : ""}`,
      actions: sub.provider_status === "CANCELED" ? PP.ui.badge("Canceled — no renewal") : PP.ui.badge(ACC.statusText(sub.provider_status), "success") },
      el("div", { class: "pp-card-actions" },
        pick ? ACC.billingGate(ov, el("div", { class: "acc-inline" }, ACC.field("Change to", pick), ACC.btn("Change package", { id: "acc-nest-change-go", onclick: async () => {
          try {
            const r = await ACC.bridgeAction(`/v1/nests/subscriptions/${encodeURIComponent(sub.id)}/upgrade`, { intent_id: ACC.hex32(), plan_code: pick.value });
            PP.toast(r && r.effective === "now" ? "Your larger package is active now." : "Your package changes at the next renewal.", "success");
          } catch (e) { PP.toast(`Couldn't change the package: ${PP.explain(e)}`, "error"); }
          ACC.forget("nests");
          PP.shell.refresh();
        } }))) : null,
        sub.provider_status === "CANCELED" ? null : ACC.btn("Cancel package", { kind: "danger", onclick: async () => {
          if (!(await PP.ui.confirm({ title: "Cancel your Nest package?", text: "Your Nests keep running until the end of the paid month.", confirm: "Cancel package", danger: true }))) return;
          try { await ACC.bridgeAction(`/v1/nests/subscriptions/${encodeURIComponent(sub.id)}/cancel`, { intent_id: ACC.hex32() }); PP.toast("Canceled. Your Nests keep running until the end of the paid month."); }
          catch (e) { PP.toast(`Couldn't cancel: ${PP.explain(e)}`, "error"); }
          ACC.forget("nests");
          PP.shell.refresh();
        } })));
  }
})();
