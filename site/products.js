// Pigeonpost Products page (Nests Phase B2): Buy an Inbox · Buy Nests · Services.
//
// Mounted by account.js into #ac-products with its own authenticated helpers, so this file holds
// no credentials and makes no request of its own:
//   pb(path, {method, body})        the postbox, as the signed-in member
//   bridgeGet(path) / bridgeAction(path, body, method)   the billing bridge (/api)
//   startCheckout(pending)          remembers the intent and opens the hosted iyzico page
//   billingProfileId()              the member's saved billing profile, or null
//   goBilling(), focusInbox(), toast(msg), randomHex32(), search (location.search), locale
//
// Everything Nests-related appears only when the postbox reports the `nests` feature for this
// account. Prices come from the bridge (`GET /api/v1/nests` returns them only to accounts that see
// Nests); this page never shows a price it computed. A purchase is refused by the postbox, not
// guessed here, when the account is already funded on another rail.
(function () {
  "use strict";

  const SERVICES = [
    ["Personal assistant", "Tasks, reminders and scheduling help from your own agent."],
    ["AI CRM", "Leads, pipeline and follow-ups, run by an agent your team can talk to."],
    ["AI HR", "Job posts, candidate intake and interview briefs."],
    ["Shopping assistant", "Compare products and prepare purchases you approve."],
    ["Marketing assistant", "Campaign plans and drafts, reviewed before anything is sent."],
  ];
  const CURRENCY_NAMES = { USD: "US dollar", EUR: "Euro", TRY: "Turkish lira" };
  const STATUS_TEXT = {
    ready: "Ready", working: "Working…", needs_you: "Needs you", paused: "Paused", unavailable: "Unavailable",
  };
  const REASON_TEXT = {
    payment: "Its package is not paid for. Renew or buy a package to bring it back.",
    over_capacity: "Your package has fewer Nests than you have. Delete one or upgrade.",
    setting_up: "Setting up — this takes a few seconds.",
    failed: "Setup failed. Retry it below or contact support.",
  };
  // The org roles that may create and manage company Nests (nest_access.rs, Perm::ManageAgents).
  const NEST_ADMIN_ROLES = ["owner", "admin", "agent_admin"];
  // The org roles that may buy and manage the company's package (org.rs, Perm::ManageBilling). The
  // bridge checks this again with the postbox; this only decides what to show.
  const BILLING_ROLES = ["owner", "billing_admin"];
  const ORG_ID = /^org_[a-z0-9]{6,64}$/;
  const RAIL = { apple: "the App Store", google: "Google Play", iyzico: "Pigeonpost (web)", grant: "a Pigeonpost grant", test: "a test grant" };

  function el(tag, attrs, ...children) {
    const node = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v == null || v === false) continue;
      if (k === "class") node.className = v;
      else if (k === "text") node.textContent = v;
      else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
      else node.setAttribute(k, v === true ? "" : String(v));
    }
    for (const c of children.flat()) if (c != null && c !== false) node.append(c);
    return node;
  }
  const gib = (b) => `${Math.round((Number(b) || 0) / (1024 * 1024 * 1024))} GB`;
  const tokens = (n) => (n >= 1e6 ? `${(n / 1e6).toLocaleString()}M` : `${Math.round(n / 1e3)}k`);
  const money = (a, c) => { try { return new Intl.NumberFormat(undefined, { style: "currency", currency: c }).format(Number(a)); } catch { return `${a} ${c}`; } };

  function packageLines(p) {
    const lines = [
      `${p.slots} Nest${p.slots === 1 ? "" : "s"}`,
      `${gib(p.storage_bytes)} of files, shared`,
      `${p.active_hours} running hours a month, shared`,
    ];
    lines.push(p.model === "managed"
      ? `AI included: ${tokens(p.input_tokens)} input + ${tokens(p.output_tokens)} output tokens a month, shared`
      : "No AI connected: a private workspace you set up yourself");
    return lines;
  }

  function mount(root, deps) {
    // Back from a company checkout (`?nest_org=`), the page opens in that company's context.
    const returnOrg = new URLSearchParams(deps.search || "").get("nest_org");
    const state = { tab: "nests", context: ORG_ID.test(returnOrg || "") ? returnOrg : "personal", currency: null, data: null, gen: 0 };
    const nestsOn = () => Boolean(deps.features && deps.features.has("nests"));
    const catalog = deps.features && deps.features.has("marketplace") && window.PPMarketplace && window.PPMarketplace.catalog
      ? window.PPMarketplace.catalog(deps) : null;

    function tabs() {
      const order = [["inbox", "Buy an Inbox"], ["nests", "Buy Nests"], ["services", "Services"]];
      const list = el("div", { class: "pp-tabs", role: "tablist", "aria-label": "Products" });
      for (const [id, label] of order) {
        const selected = state.tab === id;
        list.append(el("button", {
          type: "button", role: "tab", id: `pp-tab-${id}`, "aria-selected": String(selected),
          "aria-controls": `pp-panel-${id}`, tabindex: selected ? "0" : "-1", class: "pp-tab",
          onclick: () => select(id),
        }, label));
      }
      list.addEventListener("keydown", (e) => {
        const i = order.findIndex(([id]) => id === state.tab);
        let next = null;
        if (e.key === "ArrowRight") next = order[(i + 1) % order.length][0];
        if (e.key === "ArrowLeft") next = order[(i + order.length - 1) % order.length][0];
        if (e.key === "Home") next = order[0][0];
        if (e.key === "End") next = order[order.length - 1][0];
        if (next) { e.preventDefault(); select(next); root.querySelector(`#pp-tab-${next}`)?.focus(); }
      });
      return list;
    }

    function select(id) {
      state.tab = id;
      draw();
    }

    function panel(id, ...children) {
      return el("section", { role: "tabpanel", id: `pp-panel-${id}`, "aria-labelledby": `pp-tab-${id}`, class: "pp-panel", tabindex: "0" }, ...children);
    }

    function inboxPanel() {
      return panel("inbox",
        el("p", { text: "An Inbox is a Pigeonpost name you own, like /bekir, with its mail storage. Your names, and the plans that include them, are in Your handles and Pigeonpost Cloud plan below." }),
        el("button", { type: "button", class: "btn btn-secondary", id: "pp-inbox-find", onclick: () => deps.focusInbox && deps.focusInbox() }, "Find a name"));
    }

    function servicesPanel() {
      const builtins = el("ul", { class: "pp-services" }, SERVICES.map(([name, what]) =>
        el("li", { class: "pp-service" }, el("strong", { text: name }), el("span", { text: what }), el("span", { class: "pp-soon", text: "Coming soon" }))));
      // With the `marketplace` feature on, marketplace-catalog.js owns this tab (OD-8: listings live
      // here, with a publisher badge and a Marketplace filter) and shows these under "All services".
      if (catalog) {
        const box = el("div", { id: "pp-marketplace" });
        catalog.render(box, builtins);
        return panel("services", box);
      }
      return panel("services",
        el("p", { class: "muted", text: "Ready-made assistants that run on a Nest. They are not available yet." }),
        builtins);
    }

    function nestsPanel() {
      const p = panel("nests");
      if (!nestsOn()) {
        p.append(el("p", { class: "muted", id: "pp-nests-off", text: "Nests aren't available for your account yet." }));
        return p;
      }
      const d = state.data;
      if (!d) {
        p.append(el("p", { class: "muted", role: "status", text: "Loading…" }));
        return p;
      }
      if (d.error) {
        p.append(el("p", { role: "status", text: d.error }));
        return p;
      }
      // Personal or company context: whose Nests and whose package.
      if (d.orgs.length) {
        const sel = el("select", { id: "pp-context", "aria-label": "Show Nests for", onchange: (e) => { state.context = e.target.value; draw(); } },
          el("option", { value: "personal", text: "Personal", selected: state.context === "personal" }),
          d.orgs.map((o) => el("option", { value: o.id, text: o.name, selected: state.context === o.id })));
        p.append(el("label", { class: "pp-context" }, "Show Nests for ", sel));
      }
      if (state.context !== "personal") {
        p.append(orgSection(state.context));
        return p;
      }
      p.append(personalSection(d));
      return p;
    }

    function nestRow(n) {
      return el("li", { class: "pp-nest", "data-nest": n.nest_id },
        el("strong", { text: n.label }),
        el("span", { class: `pp-status pp-status-${n.status || "unavailable"}`, text: STATUS_TEXT[n.status] || n.state || "" }),
        n.status_reason && REASON_TEXT[n.status_reason] ? el("span", { class: "muted", text: REASON_TEXT[n.status_reason] }) : null,
        n.status === "needs_you" && n.delete_after ? el("span", { class: "pp-warn", text: `Deleted on ${new Date(n.delete_after * 1000).toLocaleDateString()} unless it is paid for.` }) : null,
        n.address ? el("a", { href: `https://inbox.pigeonpost.dev/#peer=${encodeURIComponent(n.address)}`, text: "Open in your inbox" }) : null);
    }

    function createNestForm(a) {
      const label = el("input", { id: "pp-nest-label", required: true, maxlength: "64", autocomplete: "off" });
      return el("form", { class: "ac-form", id: "pp-nest-create", onsubmit: (e) => { e.preventDefault(); createNest(label.value); } },
        el("h4", { text: `Add a Nest (${a.slots - a.used} of ${a.slots} free)` }),
        el("label", { for: "pp-nest-label" }, "Name"), label,
        el("button", { type: "submit", class: "btn btn-secondary" }, "Create"));
    }

    function personalSection(d) {
      const box = el("div", { class: "pp-personal" });
      const a = d.nests.allowance || { slots: 0, used: 0, duplicates: [] };
      const funding = a.funding;
      if (funding) {
        box.append(el("p", { id: "pp-funding" },
          el("strong", { text: d.nests.package?.name || funding.package }),
          ` · ${a.used} of ${a.slots} Nest${a.slots === 1 ? "" : "s"} in use · bought through ${RAIL[funding.source] || funding.source}`,
          funding.paid_through ? ` · paid through ${new Date(funding.paid_through * 1000).toLocaleDateString()}` : ""));
        if (funding.source === "apple" || funding.source === "google") {
          box.append(el("p", { class: "ac-note", text: `Change or cancel this package in ${RAIL[funding.source]}. Buying another one here would charge you twice.` }));
        }
      }
      if ((a.duplicates || []).length) {
        box.append(el("p", { class: "pp-warn", role: "alert", id: "pp-duplicates", text: `You are paying for ${a.duplicates.length} more Nest package${a.duplicates.length === 1 ? "" : "s"} than you use. Cancel the extra one where you bought it and ask for a refund; it does not add Nests.` }));
      }
      for (const s of d.nests.setups || []) {
        box.append(el("div", { class: "pp-setup", role: "status" },
          el("span", { text: s.status === "failed" ? `Paid — setting up “${s.label}” failed.` : `Paid — setting up “${s.label}”…` }),
          el("button", { type: "button", class: "btn btn-small", "data-retry": s.setup_id, onclick: () => retry(s.setup_id) }, "Retry now")));
      }
      if ((d.nests.nests || []).length) box.append(el("ul", { class: "pp-nests", "aria-label": "Your Nests" }, d.nests.nests.map(nestRow)));
      if (funding && a.used < a.slots) box.append(createNestForm(a));
      const subs = d.bridge?.subscriptions || [];
      if (subs.length) box.append(subscriptionBlock(subs[0], d));
      const buyable = !funding || funding.source === "iyzico" || funding.source === "grant" || funding.source === "test";
      if (!subs.length && buyable) box.append(packagesBlock(d));
      return box;
    }

    function pricesFor(d) {
      const plans = (d.bridge?.prices?.plans || []).filter((pl) => pl.available !== false);
      const currencies = [...new Set(plans.map((pl) => pl.currency))].filter((c) => !(d.bridge?.prices?.open_currencies) || d.bridge.prices.open_currencies.includes(c));
      if (!state.currency || !currencies.includes(state.currency)) {
        state.currency = currencies.includes(deps.preferredCurrency) ? deps.preferredCurrency : currencies[0] || null;
      }
      return { plans, currencies };
    }

    function packagesBlock(d, orgId) {
      const { plans, currencies } = pricesFor(d);
      const box = el("div", { class: "pp-packages" });
      if (currencies.length > 1) {
        box.append(el("label", {}, "Currency ", el("select", { id: "pp-currency", onchange: (e) => { state.currency = e.target.value; draw(); } },
          currencies.map((c) => el("option", { value: c, text: CURRENCY_NAMES[c] || c, selected: c === state.currency })))));
      }
      const cards = el("ul", { class: "pp-cards" });
      for (const p of d.packages.packages.filter((x) => x.sellable)) {
        const plan = plans.find((pl) => pl.package === p.code && pl.currency === state.currency);
        cards.append(el("li", { class: "pp-card", "data-package": p.code },
          el("h4", { text: p.name }),
          el("ul", {}, packageLines(p).map((l) => el("li", { text: l }))),
          plan ? el("p", { class: "pp-price", text: `${money(plan.amount, plan.currency)} a month` }) : el("p", { class: "muted", text: "Not available to buy here yet." }),
          plan ? el("button", { type: "button", class: "btn btn-primary", "data-buy": plan.code, "aria-label": `Buy ${p.name}`, onclick: () => buy(plan.code, orgId) }, "Buy") : null));
      }
      box.append(cards);
      box.append(el("p", { class: "ac-note", text: "Sold by Piyote Bilişim and billed monthly through iyzico. Cancel any time; your Nests keep running until the end of the paid month, then you have 30 days to export before they are deleted." }));
      return box;
    }

    function subscriptionBlock(sub, d, orgId) {
      const { plans } = pricesFor(d);
      const others = plans.filter((pl) => pl.currency === sub.currency && pl.code !== sub.plan_code);
      const title = `${orgId ? "Company" : "Your"} web Nest package · ${sub.currency}${sub.recurring_price ? " · " + money(sub.recurring_price.amount, sub.recurring_price.currency) + " a month" : ""}`;
      // saas_be lets only the buyer change a subscription; another billing admin sees it, read-only.
      if (orgId && sub.buyer === false) {
        return el("div", { class: "pp-sub", "data-sub": sub.id }, el("p", { text: title }),
          el("p", { class: "muted", id: "pp-not-buyer", text: "Another billing admin bought this package. Only they can change or cancel it." }));
      }
      return el("div", { class: "pp-sub", "data-sub": sub.id },
        el("p", { text: title }),
        others.length ? el("label", {}, "Change to ", el("select", { id: "pp-change" }, others.map((pl) => el("option", { value: pl.code, text: d.packages.packages.find((p) => p.code === pl.package)?.name || pl.code })))) : null,
        others.length ? el("button", { type: "button", class: "btn btn-small", id: "pp-change-go", onclick: () => change(sub.id, root.querySelector("#pp-change").value) }, "Change package") : null,
        sub.provider_status === "CANCELED" ? el("span", { class: "muted", text: " Canceled — no renewal." })
          : el("button", { type: "button", class: "btn btn-small", id: "pp-cancel", onclick: () => cancel(sub.id) }, "Cancel"));
    }

    // The company list carries no status header, so each row asks for its own (GET /v1/nests/{nest}
    // serves both kinds of Nest). A row that can't be read still shows its name.
    async function orgRows(nests) {
      return Promise.all(nests.map((n) => deps.pb(`/v1/nests/${encodeURIComponent(n.nest_id)}`).then((v) => ({ ...n, ...v })).catch(() => n)));
    }

    function orgSection(orgId) {
      const org = state.data.orgs.find((o) => o.id === orgId) || {};
      const box = el("div", { class: "pp-org", id: "pp-org" }, el("p", { class: "muted", role: "status", text: "Loading…" }));
      const path = `/v1/orgs/${encodeURIComponent(orgId)}`;
      (async () => {
        const v = await deps.pb(`${path}/nests`);
        const rows = await orgRows(v.nests || []);
        // Only people admins may list members. Anyone else creates the Nest and adds operators later.
        const members = org.admin ? await deps.pb(`${path}/members?status=active`).then((m) => (m.members || []).filter((x) => x.id !== org.memberId)).catch(() => []) : [];
        // The owner and billing admins see the company's web package and prices from the bridge.
        const bridge = org.billing ? await deps.bridgeGet(`/v1/nests?org_id=${encodeURIComponent(orgId)}`).catch(() => null) : null;
        box.textContent = "";
        const a = v.allowance || { slots: 0, used: 0 };
        box.append(el("p", { text: v.package ? `Company package: ${a.used} of ${a.slots} Nests in use.`
          : bridge ? "This company has no Nest package yet. Buy one below." : "This company has no Nest package yet. Its billing admin can buy one." }));
        if (rows.length) box.append(el("ul", { class: "pp-nests", "aria-label": "Company Nests" }, rows.map(nestRow)));
        if (bridge) {
          const d = { packages: state.data.packages, bridge };
          const subs = bridge.subscriptions || [];
          // A package funded another way (a grant, another rail) is not offered a second time.
          if (subs.length) box.append(subscriptionBlock(subs[0], d, orgId));
          else if (!v.package) box.append(packagesBlock(d, orgId));
        }
        if (!org.admin) return;
        const handle = el("input", { id: "pp-org-handle", required: true, autocomplete: "off" });
        const label = el("input", { id: "pp-org-label", required: true, maxlength: "64" });
        const picks = members.map((m) => el("input", { type: "checkbox", value: m.id, "data-operator": m.id, "aria-label": memberName(m) }));
        box.append(el("form", { class: "ac-form", id: "pp-org-create", onsubmit: (e) => { e.preventDefault(); createOrgNest(orgId, handle.value, label.value, picks.filter((c) => c.checked).map((c) => c.value)); } },
          el("h4", { text: "Create a company Nest" }),
          el("label", { for: "pp-org-handle" }, "Address in your company namespace"), handle,
          el("label", { for: "pp-org-label" }, "Name"), label,
          picks.length ? el("fieldset", { class: "pp-operators" }, el("legend", { text: "Who can use it (admins always can)" }),
            picks.map((c, i) => el("label", { class: "pp-operator" }, c, ` ${memberName(members[i])}`))) : null,
          el("button", { type: "submit", class: "btn btn-secondary" }, "Create")));
      })().catch((e) => { box.textContent = ""; box.append(el("p", { role: "status", text: e.status === 404 ? "You can't see this company's Nests." : "Couldn't load the company's Nests." })); });
      return box;
    }

    function memberName(m) {
      return [m.first_name, m.last_name].filter(Boolean).join(" ") || m.handle || "Member";
    }

    // ---- actions ----------------------------------------------------------------------------

    async function buy(planCode, orgId) {
      const profile = await deps.billingProfileId();
      if (!profile) { deps.toast("Add your billing details first, then buy a Nest package."); deps.goBilling && deps.goBilling(); return; }
      deps.startCheckout({ kind: "nest", intent_id: deps.randomHex32(), plan_code: planCode, billing_profile_id: profile, locale: deps.locale || "en",
        ...(orgId ? { org_id: orgId } : {}) });
    }

    async function retry(setupId) {
      try { await deps.pb(`/v1/nest-setups/${encodeURIComponent(setupId)}/retry`, { method: "POST", body: "{}" }); deps.toast("Trying again…"); }
      catch (_) { deps.toast("Couldn't retry right now. We keep trying automatically."); }
      load();
    }

    async function change(subId, planCode) {
      try {
        const r = await deps.bridgeAction(`/v1/nests/subscriptions/${encodeURIComponent(subId)}/upgrade`, { intent_id: deps.randomHex32(), plan_code: planCode });
        deps.toast(r.effective === "now" ? "Your larger package is active now." : "Your package changes at the next renewal.");
      } catch (e) { deps.toast(`Couldn't change the package: ${e.message}`); }
      load();
    }

    async function cancel(subId) {
      try { await deps.bridgeAction(`/v1/nests/subscriptions/${encodeURIComponent(subId)}/cancel`, { intent_id: deps.randomHex32() }); deps.toast("Canceled. Your Nests keep running until the end of the paid month."); }
      catch (e) { deps.toast(`Couldn't cancel: ${e.message}`); }
      load();
    }

    async function createNest(label) {
      try {
        await deps.pb("/v1/nests", { method: "POST", body: JSON.stringify({ label: label.trim() }) });
        deps.toast("Nest created.");
      } catch (e) {
        deps.toast({ nest_slots_full: "Your package has no free Nest. Delete one or upgrade.", no_nest_package: "Buy a Nest package first.", bad_label: "Names are 1 to 64 printable characters." }[e.code] || `Couldn't create it: ${e.message}`);
      }
      load();
    }

    async function createOrgNest(orgId, handle, label, operators) {
      try {
        const body = { handle: handle.trim(), label: label.trim() };
        if (operators.length) body.operators = operators;
        await deps.pb(`/v1/orgs/${encodeURIComponent(orgId)}/nests`, { method: "POST", body: JSON.stringify(body) });
        deps.toast("Company Nest created.");
      } catch (e) {
        deps.toast({ no_nest_slot: "The company has no free Nest slot. Its billing admin can buy or upgrade a package.", agent_limit: "The company has used all its agent addresses.", handle_taken: "That address is taken.", invalid_handle: "That isn't a valid address in your company namespace.", bad_label: "Names are 1 to 64 printable characters." }[e.code] || `Couldn't create it: ${e.message}`);
      }
      draw();
    }

    // Back from iyzico: the redirect proves nothing; the bridge reads the authoritative state.
    async function confirmReturn() {
      const params = new URLSearchParams(deps.search || "");
      const intent = params.get("nest_checkout");
      const company = ORG_ID.test(params.get("nest_org") || "");
      if (params.has("nest_org")) deps.clearReturn && deps.clearReturn("nest_org");
      if (!intent || !/^[a-f0-9]{32}$/.test(intent)) return;
      deps.clearReturn && deps.clearReturn("nest_checkout");
      deps.toast("Confirming your payment…");
      for (let i = 0; i < (deps.confirmAttempts || 20); i++) {
        try {
          const r = await deps.bridgeGet(`/v1/nests/checkout/${intent}`);
          if (["active", "canceled"].includes(r.status)) {
            deps.toast(company ? "The company's Nest package is active. Its admins can create company Nests now." : "Your Nest package is active. Your first Nest is being set up.");
            load(); return;
          }
          if (r.status === "failed") { deps.toast("The payment did not go through. Nothing was charged."); return; }
        } catch (e) { if (e.status === 401) return; }
        await new Promise((r) => setTimeout(r, deps.confirmDelayMs ?? 3000));
      }
      deps.toast("Your payment is still being confirmed. This page will show your package as soon as it is.");
    }

    async function load() {
      if (!nestsOn()) { draw(); return; }
      const gen = ++state.gen;
      try {
        const [packages, nests, bridge, orgs] = await Promise.all([
          deps.pb("/v1/nest-packages"),
          deps.pb("/v1/nests"),
          deps.bridgeGet("/v1/nests").catch(() => null),
          deps.pb("/v1/orgs").then((v) => (v.orgs || []).map((o) => ({ id: o.id, name: o.name, memberId: o.my_member_id, admin: (o.my_roles || []).some((r) => NEST_ADMIN_ROLES.includes(r)), billing: (o.my_roles || []).some((r) => BILLING_ROLES.includes(r)) }))).catch(() => []),
        ]);
        if (gen !== state.gen) return;
        state.data = { packages, nests, bridge, orgs };
      } catch (e) {
        if (gen !== state.gen) return;
        state.data = { error: e.code === "feature_disabled" ? "Nests aren't available for your account yet." : "Couldn't load your Nests. Try again later." };
      }
      draw();
    }

    function draw() {
      root.textContent = "";
      root.append(el("h3", { id: "pp-title", text: "Products" }), tabs());
      root.append(state.tab === "inbox" ? inboxPanel() : state.tab === "services" ? servicesPanel() : nestsPanel());
    }

    draw();
    load();
    confirmReturn();
    return { reload: load, select };
  }

  window.PPProducts = { mount, packageLines };
})();
