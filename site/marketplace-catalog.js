// Marketplace catalog inside the Products page's Services tab (build plan R-49, OD-8): published
// listings with a publisher badge and a "Marketplace" filter, a listing's detail, and the buyer's
// own installations (install, settings, update, roll back, uninstall).
//
// Buying is web-only. A priced listing is bought for yourself or for a company whose billing you
// manage: the bridge starts the purchase (`POST /api/v1/marketplace/checkout`), the hosted iyzico
// form takes the card on /billing/checkout, and the return (`?mp_purchase=<intent>`) asks the
// bridge whether it was paid before installing with the purchase's id. A paid installation shows
// its licence, paid-through date, payments with receipts, and Cancel for a monthly one.
// products.js asks this file for a controller once per mount and hands it a fresh container on
// every redraw; the controller keeps its own state.
//
// Calls (all through deps.pb):
//   GET    /v1/marketplace/listings?q=&cursor=        the public catalog
//   GET    /v1/marketplace/listings/{listing}         detail: operations, config_schema, price
//   GET    /v1/nest-installations                     this account's installations
//   POST   /v1/nest-installations                     install {listing_id, configuration?}
//   PUT    /v1/nest-installations/{id}/configuration  {configuration}
//   POST   /v1/nest-installations/{id}/update         {release_id?}
//   POST   /v1/nest-installations/{id}/rollback       {}
//   DELETE /v1/nest-installations/{id}                uninstall
//   POST   /v1/orgs/{org}/nest-installations           install for a company {listing_id, handle, purchase_id}
//   POST   /v1/marketplace/hosted-jobs                run an operation of a publisher-hosted listing
//                                                    {listing_id, operation, input}
//   GET    /v1/marketplace/hosted-jobs                this account's hosted jobs
//   POST   /v1/marketplace/hosted-jobs/{id}/cancel    stop a run before its result arrives
//   (logo images) GET /v1/marketplace/listings/{listing}/logo/{hash}/{size}.webp, public and immutable
//
// The look (developer-program plan §1): cards from MP.card (marketplace-core.js) with logo or the
// default initials tile, a featured row, category filter and sort (client-side over the loaded
// page), and a listing page with a publisher card. A `coming_soon` listing shows a disabled
// "Coming soon" button and nothing to buy, install or run (the postbox refuses with 409 too).
//
// A publisher-hosted listing (`hosting: "publisher"`) is run by its publisher: it is never
// installed, its page says so before anything is sent, and each operation has a Run form built
// from its input fields. Results come back to the buyer's inbox as a message with the PDF attached.
// An operation with a per-job price (`operation.price`, USD) is paid from the account's credit:
// the form shows the price and the balance (`GET /api/v1/credits/balance` on the bridge), and a
// "Buy credits" link to the account's Credits card when the balance is short.
// and the billing bridge (deps.bridgeGet / deps.bridgeAction / deps.startCheckout):
//   POST   /api/v1/marketplace/checkout               (on /billing/checkout) start a purchase
//   GET    /api/v1/marketplace/checkout/{intent}       where a purchase stands after iyzico
//   GET    /api/v1/marketplace/installations/{id}      licence and payments of a paid installation
//   POST   /api/v1/marketplace/installations/{id}/cancel   cancel a monthly licence at period end
//   GET    /api/v1/billing/invoices/{id}/download      a receipt
(function () {
  "use strict";

  const MP = (window.PPMarketplace = window.PPMarketplace || {});
  const el = (...a) => MP.el(...a);
  const CONFIRM_WORD = "UNINSTALL";
  const INTENT = /^[a-f0-9]{32}$/;
  // The org roles that may install for a company (ManageAgents) and buy for it (ManageBilling).
  const NEST_ADMIN_ROLES = ["owner", "admin", "agent_admin"];
  const BILLING_ROLES = ["owner", "billing_admin"];
  const LICENCE_TEXT = {
    active: "Active", canceling: "Active until the end of the paid period", paused: "Paused: payment is due",
    expired: "Ended", terminated: "Ended", revoked: "Revoked (refunded)",
  };


  MP.catalog = function (deps) {
    const st = {
      filter: "all", query: "", category: "", sort: "featured", listings: null, next: null, installs: [], error: null,
      open: null, detail: null, gen: 0, container: null, builtins: null,
      orgs: null, purchase: null, licences: {},
    };
    // Back from iyzico: `?mp_purchase=<intent>` (the bridge's return URL; `mp_checkout` too).
    const returnParams = new URLSearchParams(deps.search || "");
    const returning = [returnParams.get("mp_purchase"), returnParams.get("mp_checkout")].find((v) => INTENT.test(v || "")) || null;
    const status = MP.statusRegion("mp-cat-status");

    const installFor = (listingId) => st.installs.find((i) => i.listing_id === listingId && i.status === "active");

    async function load(more) {
      const gen = ++st.gen;
      try {
        const q = new URLSearchParams();
        if (st.query) q.set("q", st.query);
        if (more && st.next) q.set("cursor", st.next);
        const qs = q.toString();
        const [page, installs] = await Promise.all([
          deps.pb(`/v1/marketplace/listings${qs ? "?" + qs : ""}`),
          more ? Promise.resolve({ data: st.installs }) : deps.pb("/v1/nest-installations").catch(() => ({ data: [] })),
        ]);
        if (gen !== st.gen) return;
        st.listings = more ? (st.listings || []).concat(page.data || []) : page.data || [];
        st.next = page.next_cursor || null;
        st.installs = installs.data || [];
        st.error = null;
      } catch (e) {
        if (gen !== st.gen) return;
        st.error = e;
      }
      draw();
    }

    async function openListing(id) {
      st.open = id;
      st.detail = null;
      draw();
      try {
        const [listing, installs] = await Promise.all([
          deps.pb(`/v1/marketplace/listings/${encodeURIComponent(id)}`),
          deps.pb("/v1/nest-installations").catch(() => ({ data: st.installs })),
        ]);
        if (st.open !== id) return;
        st.detail = listing.data;
        st.installs = installs.data || [];
      } catch (e) {
        if (st.open !== id) return;
        st.detail = { error: e };
      }
      draw();
      st.container?.querySelector("#mp-detail-title")?.focus();
    }

    function back() {
      st.open = null;
      st.detail = null;
      draw();
      st.container?.querySelector("#mp-cat-search-q")?.focus();
    }

    /// The publisher line and hosting badge, as the cards show them.
    function badge(l) {
      return el("span", { class: "mp-byline" }, MP.publisherLine(l), " ",
        l.sample ? null : el("span", { class: `mp-chip mp-chip-hosting${l.hosting === "publisher" ? " mp-chip-publisher" : ""}`, "data-hosting": l.hosting || "pigeonpost", text: MP.hostingText(l) }));
    }

    const HOSTED_NOTICE = "Hosted by the publisher — Pigeonpost reviewed the publisher and this integration, not the code; your inputs are sent to the publisher.";
    const JOB_TEXT = { queued: "Sending to the publisher", running: "With the publisher", succeeded: "Done — the result is in your inbox",
      failed: "Failed", expired: "Ran out of time", cancelled: "Cancelled" };
    // Inbox delivery gave up after its last attempt: the result is still kept on the job.
    const jobText = (j) => (j.execution === "succeeded" && j.delivery === "undeliverable"
      ? "Done — it couldn't be delivered to your inbox; the result is available on the job page"
      : JOB_TEXT[j.execution] || j.execution);
    const CHARGE_TEXT = { held: "reserved", charged: "paid", released: "returned to your credit" };
    const RUN_ERRORS = {
      insufficient_credit: "Your credit is too low for this run. Buy credits, then run it again.",
      not_on_sale: "This service's price is being updated. Try again in a minute.",
      billing_unavailable: "Payments aren't answering right now. Nothing was charged; try again shortly.",
    };
    const buyCredits = (id) => el("a", { class: "btn btn-secondary btn-small", id, href: "#credits" }, "Buy credits");

    /// The account's credit, for priced operations (`null`: not loaded or could not be).
    async function loadCredit() {
      try { st.credit = deps.bridgeGet ? await deps.bridgeGet("/v1/credits/balance") : null; } catch (_) { st.credit = null; }
      st.creditLoaded = true;
      draw();
    }
    const creditChanged = () => { try { window.dispatchEvent(new window.Event("pp-credits-changed")); } catch (_) { /* old browsers */ } };

    function hostedNotice() {
      // Information, not an error: an amber callout with an info icon (the billing gate's tokens).
      const NS = "http://www.w3.org/2000/svg";
      const icon = document.createElementNS(NS, "svg");
      icon.setAttribute("viewBox", "0 0 24 24"); icon.setAttribute("aria-hidden", "true"); icon.setAttribute("focusable", "false");
      icon.setAttribute("class", "mp-callout-icon");
      for (const [tag, attrs] of [["circle", { cx: "12", cy: "12", r: "9" }], ["path", { d: "M12 11v5" }], ["path", { d: "M12 8h.01" }]]) {
        const n = document.createElementNS(NS, tag);
        for (const [k, v] of Object.entries(attrs)) n.setAttribute(k, v);
        icon.append(n);
      }
      const [lead, rest] = HOSTED_NOTICE.split(" — ");
      return el("div", { class: "mp-callout mp-hosted-notice", id: "mp-hosted-notice", role: "note" }, icon,
        el("p", {}, el("strong", { text: lead }), ` — ${rest}`));
    }

    /// One Run form per operation, from its input fields (`{name, max_chars, required, label?, multiline?}`).
    function runForm(l, op, i) {
      const prefix = `mp-run-${i}`;
      const inputs = [];
      const box = el("fieldset", {}, el("legend", { text: op.title || op.name }));
      if (op.description) box.append(el("p", { class: "muted", text: op.description }));
      for (const f of op.fields || []) {
        const id = `${prefix}-${String(f.name).replace(/[^a-z0-9_]/g, "_")}`;
        const attrs = { id, maxlength: f.max_chars, required: f.required === true, autocomplete: "off" };
        const input = f.multiline ? el("textarea", { ...attrs, rows: "4" }) : el("input", { ...attrs, type: "text" });
        inputs.push([f.name, input]);
        box.append(el("label", { for: id }, (f.label || f.name) + (f.required ? " (required)" : "")), input);
      }
      const price = op.price && Number(op.price.amount_cents) > 0 ? op.price : null;
      let blocked = false;
      if (price) {
        box.append(el("p", { class: "mp-run-price", id: `${prefix}-price`, text: `Price: ${MP.money(price.amount_cents, "USD")} per run, paid from your credit.` }));
        const have = st.credit && Number.isInteger(st.credit.available_cents) ? st.credit.available_cents : null;
        if (price.on_sale === false) {
          blocked = true;
          box.append(el("p", { class: "muted", text: RUN_ERRORS.not_on_sale }));
        } else if (!st.creditLoaded) {
          box.append(el("p", { class: "muted", id: `${prefix}-balance`, text: "Checking your credit…" }));
        } else if (have === null) {
          box.append(el("p", { class: "muted", id: `${prefix}-balance`, text: "Your credit could not be loaded." }), buyCredits(`${prefix}-buy`));
        } else {
          box.append(el("p", { id: `${prefix}-balance`, text: `Your credit: ${MP.money(have, "USD")}.` }));
          if (have < price.amount_cents) {
            blocked = true;
            box.append(el("p", { class: "pp-warn", id: `${prefix}-short`, text: `You need ${MP.money(price.amount_cents - have, "USD")} more.` }), buyCredits(`${prefix}-buy`));
          }
        }
      }
      box.append(el("p", { class: "ac-note", text: price ? "The price is reserved when you run it, and charged only once the result is in your inbox. If it fails or runs out of time, it comes back to your credit." : "The result is sent to your inbox as a PDF." }),
        el("button", { type: "submit", class: "btn btn-primary", id: `${prefix}-go`, disabled: blocked }, `Run ${op.title || op.name}`));
      return el("form", { class: "ac-form mp-run", id: prefix, "data-operation": op.name, onsubmit: async (e) => {
        e.preventDefault();
        const input = {};
        for (const [name, node] of inputs) if (node.value.trim() !== "") input[name] = node.value;
        try {
          await deps.pb("/v1/marketplace/hosted-jobs", { method: "POST", body: JSON.stringify({ listing_id: l.id, operation: op.name, operation_version: op.version, input }) });
          status.say(price ? `Sent to the publisher. ${MP.money(price.amount_cents, "USD")} is reserved from your credit; the result will arrive in your inbox.` : "Sent to the publisher. The result will arrive in your inbox.");
          for (const [, node] of inputs) node.value = "";
          if (price) { creditChanged(); loadCredit(); }
        } catch (err) {
          if (err && err.code === "insufficient_credit") status.say(RUN_ERRORS.insufficient_credit, buyCredits("mp-run-buy"));
          else MP.report(deps, status, err, "services", RUN_ERRORS);
          if (price) loadCredit();
        }
        await loadJobs(l.id);
      } }, box);
    }

    async function loadJobs(listingId) {
      try {
        const v = await deps.pb("/v1/marketplace/hosted-jobs");
        st.jobs = (v.data || []).filter((j) => j.listing_id === listingId);
      } catch (_) { st.jobs = []; }
      draw();
    }

    function jobsBlock(l) {
      if (st.jobs === undefined || st.jobsFor !== l.id) { st.jobsFor = l.id; st.jobs = null; loadJobs(l.id); }
      const box = el("section", { class: "mp-jobs", id: "mp-jobs", "aria-labelledby": "mp-jobs-title" }, el("h5", { id: "mp-jobs-title", text: "Your runs" }));
      if (!st.jobs) { box.append(el("p", { class: "muted", text: "Loading…" })); return box; }
      if (!st.jobs.length) { box.append(el("p", { class: "muted", text: "Nothing run yet." })); return box; }
      box.append(el("ul", { class: "mp-list", "aria-label": "Runs" }, st.jobs.map((j) => el("li", { class: "mp-row", "data-job": j.job_id },
        el("span", { text: `${MP.when(j.created_at)} · ${j.operation} · ${jobText(j)}` }),
        j.charge ? el("span", { class: "muted", "data-charge": j.charge.status, text: ` · ${MP.money(j.charge.amount_cents, "USD")} ${CHARGE_TEXT[j.charge.status] || j.charge.status}` }) : null,
        j.result && j.result.summary ? el("span", { class: "muted", text: ` · ${j.result.summary.slice(0, 200)}` }) : null,
        ["queued", "running"].includes(j.execution) ? el("button", { type: "button", class: "btn btn-small", "data-cancel-job": j.job_id, onclick: async () => {
          try {
            await deps.pb(`/v1/marketplace/hosted-jobs/${encodeURIComponent(j.job_id)}/cancel`, { method: "POST", body: "{}" });
            status.say(j.charge ? "Cancelled. The reserved credit comes back to you within a minute." : "Cancelled.");
            if (j.charge) creditChanged();
          } catch (err) { MP.report(deps, status, err, "services", { job_closed: "This run has already finished." }); }
          await loadJobs(j.listing_id);
        } }, "Cancel") : null))));
      return box;
    }

    function hostedBlock(l) {
      const box = el("div", { class: "mp-hosted", id: "mp-hosted" });
      const priced = (l.operations || []).some((op) => op.price && Number(op.price.amount_cents) > 0);
      if (priced && st.creditFor !== l.id) { st.creditFor = l.id; st.creditLoaded = false; st.credit = null; loadCredit(); }
      (l.operations || []).forEach((op, i) => box.append(runForm(l, op, i)));
      box.append(jobsBlock(l));
      return box;
    }

    function filterControls() {
      const opt = (value, label) => {
        const id = `mp-filter-${value}`;
        return el("label", { class: "mp-check", for: id },
          el("input", { type: "radio", name: "mp-filter", id, value, checked: st.filter === value, onchange: () => { st.filter = value; draw(); } }),
          ` ${label}`);
      };
      return el("fieldset", { class: "mp-filter" }, el("legend", { text: "Show" }), opt("all", "All services"), opt("marketplace", "Marketplace"));
    }

    function searchForm() {
      const input = el("input", { type: "search", id: "mp-cat-search-q", maxlength: "100", value: st.query || null, autocomplete: "off", placeholder: "Search services" });
      return el("form", { class: "mp-search", role: "search", id: "mp-cat-search", onsubmit: (e) => { e.preventDefault(); st.query = input.value.trim(); st.listings = null; draw(); load(); } },
        el("label", { class: "mp-sr", for: "mp-cat-search-q" }, "Search the marketplace"),
        el("span", { class: "mp-search-icon", "aria-hidden": "true" }), input,
        el("button", { type: "submit", class: "btn btn-small" }, "Search"));
    }

    const SORTS = [["featured", "Featured"], ["newest", "Newest"], ["price", "Price: low to high"]];

    /// The minor-unit price a sort compares; coming-soon listings sort last.
    const sortPrice = (l) => (l.coming_soon ? Infinity
      : l.price_from && Number(l.price_from.amount_cents) > 0 ? Number(l.price_from.amount_cents)
        : Number((l.price && l.price.amount_minor) || 0));
    const published = (l) => Date.parse(l.published_at || "") || 0;

    function sorted(rows) {
      const out = rows.slice();
      if (st.sort === "newest") out.sort((a, b) => published(b) - published(a));
      else if (st.sort === "price") out.sort((a, b) => sortPrice(a) - sortPrice(b) || published(b) - published(a));
      else {
        // Featured: what can be used now before what is coming, each in the featured row's order
        // and then newest first.
        const rank = (l) => (l.featured ? Number(l.featured_rank ?? 1e6) : 1e9);
        out.sort((a, b) => Number(Boolean(a.coming_soon)) - Number(Boolean(b.coming_soon)) || rank(a) - rank(b) || published(b) - published(a));
      }
      return out;
    }

    function toolbar(rows) {
      const present = new Set(rows.map((l) => l.category).filter(Boolean));
      const cats = MP.CATEGORIES.filter(([c]) => present.has(c) || c === st.category);
      const category = el("select", { id: "mp-cat-category", onchange: (e) => { st.category = e.target.value; draw(); st.container?.querySelector("#mp-cat-category")?.focus(); } },
        el("option", { value: "", text: "All categories", selected: !st.category }),
        cats.map(([c, label]) => el("option", { value: c, text: label, selected: st.category === c })));
      const sort = el("select", { id: "mp-cat-sort", onchange: (e) => { st.sort = e.target.value; draw(); st.container?.querySelector("#mp-cat-sort")?.focus(); } },
        SORTS.map(([v, label]) => el("option", { value: v, text: label, selected: st.sort === v })));
      return el("div", { class: "mp-toolbar" },
        el("label", { class: "mp-select" }, el("span", { text: "Category" }), category),
        el("label", { class: "mp-select" }, el("span", { text: "Sort by" }), sort));
    }

    function listView() {
      const box = el("div", { class: "mp-catalog", id: "mp-catalog" },
        el("div", { class: "mp-hero" },
          el("div", { class: "mp-hero-text" },
            el("h4", { class: "mp-hero-title", text: "Find a service" }),
            el("p", { class: "mp-hero-sub", text: "Every release is built from source and reviewed by Pigeonpost before it is listed. Publishers are verified before they can list anything." })),
          searchForm()),
        filterControls(), status);
      if (st.error) {
        box.append(el("p", { class: "muted", text: MP.errorText(st.error) }));
      } else if (!st.listings) {
        box.append(el("div", { class: "mp-grid mp-grid-loading", "aria-hidden": "true" }, [0, 1, 2].map(() => el("div", { class: "mp-card mp-card-skeleton" }))),
          el("p", { class: "mp-sr", role: "status", text: "Loading the marketplace…" }));
      } else {
        const rows = st.listings;
        const mine = st.installs.filter((i) => i.status === "active");
        if (mine.length) {
          box.append(el("h4", { class: "mp-section-title", text: "Installed" }), el("ul", { class: "mp-list mp-installed", "aria-label": "Installed services" }, mine.map((i) => {
            const l = rows.find((x) => x.id === i.listing_id);
            const name = l ? l.title : i.address || i.listing_id;
            return el("li", { class: "mp-row", "data-installation": i.id },
              MP.logo(l || { id: i.listing_id, title: name }, { size: 40 }),
              el("strong", { text: name }), el("span", { class: "muted", text: `Version ${i.version || "?"}` }),
              i.update_available ? el("span", { class: "mp-chip mp-chip-update", text: "Update available" }) : null,
              el("button", { type: "button", class: "btn btn-small", "aria-label": `Manage ${name}`, onclick: () => openListing(i.listing_id) }, "Manage"));
          })));
        }
        const browsing = !st.query && !st.category;
        const featured = browsing ? sorted(rows.filter((l) => l.featured)).slice(0, 3) : [];
        if (featured.length) {
          box.append(el("section", { class: "mp-featured", id: "mp-featured", "aria-labelledby": "mp-featured-title" },
            el("h4", { class: "mp-section-title", id: "mp-featured-title", text: "Featured" }),
            el("ul", { class: "mp-grid mp-grid-featured", "aria-label": "Featured listings" },
              featured.map((l) => MP.card(l, { featured: true, open: () => openListing(l.id), installed: Boolean(installFor(l.id)) })))));
        }
        const shown = sorted(st.category ? rows.filter((l) => l.category === st.category) : rows);
        box.append(el("div", { class: "mp-list-head" },
          el("h4", { class: "mp-section-title", id: "mp-all-title", text: st.query ? `Results for “${st.query}”` : "All services" }),
          rows.length ? toolbar(rows) : null));
        if (!shown.length) {
          box.append(el("p", { class: "mp-empty", id: "mp-cat-empty", text: st.query ? "Nothing matches that search." : st.category ? "Nothing in this category yet." : "Nothing is listed yet." }));
        } else {
          box.append(el("ul", { class: "mp-grid mp-cards", id: "mp-grid", "aria-labelledby": "mp-all-title" },
            shown.map((l) => MP.card(l, { open: () => openListing(l.id), installed: Boolean(installFor(l.id)) }))));
          if (st.next) box.append(el("button", { type: "button", class: "btn btn-small", id: "mp-cat-more", onclick: () => load(true) }, "Show more"));
        }
      }
      if (st.filter === "all" && st.builtins) box.append(el("h4", { class: "mp-section-title", text: "From Pigeonpost" }), st.builtins);
      return box;
    }

    function operationsBlock(ops) {
      if (!ops.length) return el("p", { class: "muted", text: "This service lists no operations." });
      return el("ul", { class: "mp-ops", "aria-label": "Operations" }, ops.map((o) => el("li", {},
        el("code", { text: o.name }), ` version ${o.version || "?"}`,
        (o.fields || []).length ? el("span", { class: "muted", text: ` · asks for ${(o.fields || []).map((f) => f.name + (f.required ? "" : " (optional)")).join(", ")}` }) : null)));
    }

    function priceBlock(l) {
      const free = !l.price || Number(l.price.amount_minor) === 0;
      if (free) {
        return el("div", { class: "mp-price", id: "mp-price" },
          el("p", { class: "pp-price", text: "Free while the marketplace is being tested." }));
      }
      return el("div", { class: "mp-price", id: "mp-price" },
        el("p", { class: "pp-price", text: MP.priceText(l.price) }),
        el("p", { class: "ac-note", text: l.price.term === "monthly"
          ? "Sold by Piyote Bilişim on behalf of the publisher and billed monthly through iyzico. Cancel any time; it keeps working until the end of the paid month."
          : "Sold by Piyote Bilişim on behalf of the publisher. A one-off purchase through iyzico; the price includes tax." }),
        l.price.on_sale ? null : el("p", { class: "muted", text: "Not on sale right now. Check back soon." }));
    }

    async function loadOrgs() {
      if (st.orgs) return st.orgs;
      try {
        const v = await deps.pb("/v1/orgs");
        st.orgs = (v.orgs || []).map((o) => {
          const roles = o.my_roles || [];
          return { id: o.id, name: o.name, admin: roles.some((r) => NEST_ADMIN_ROLES.includes(r)), billing: roles.some((r) => BILLING_ROLES.includes(r)) };
        }).filter((o) => o.admin && o.billing);
      } catch (_) { st.orgs = []; }
      draw();
      return st.orgs;
    }

    /// Buy: for yourself, or for a company you install for and manage billing for.
    function buyForm(l) {
      if (!l.price || !l.price.on_sale) return null;
      if (st.orgs === null) loadOrgs();
      const who = el("select", { id: "mp-buy-owner" },
        el("option", { value: "personal", text: "For me" }),
        ...(st.orgs || []).map((o) => el("option", { value: o.id, text: `For ${o.name}` })));
      const form = el("form", { class: "ac-form", id: "mp-buy", onsubmit: async (e) => {
        e.preventDefault();
        const profile = deps.billingProfileId ? await deps.billingProfileId() : null;
        if (!profile) {
          status.say("Add your billing details first; the receipt is made out to them.",
            el("button", { type: "button", class: "btn btn-small", id: "mp-buy-billing", onclick: () => deps.goBilling && deps.goBilling() }, "Billing details"));
          return;
        }
        const owner = who.value === "personal" ? { kind: "account" } : { kind: "org", id: who.value };
        deps.startCheckout({
          kind: "marketplace", intent_id: deps.randomHex32(), listing_id: l.id, title: l.title,
          owner, billing_profile_id: profile, locale: deps.locale || "en",
          price_text: MP.priceText(l.price),
        });
      } }, el("fieldset", {}, el("legend", { text: "Buy" }),
        el("label", { for: "mp-buy-owner" }, "Who it's for"), who,
        el("button", { type: "submit", class: "btn btn-primary", id: "mp-buy-go" }, `Buy · ${MP.priceText(l.price)}`)));
      return form;
    }

    /// Where a purchase stands after iyzico; the redirect itself proves nothing.
    async function checkPurchase(intent) {
      try {
        const r = await deps.bridgeGet(`/v1/marketplace/checkout/${intent}`);
        st.purchase = r;
        if (r.listing_id) { st.open = r.listing_id; await openListing(r.listing_id); }
        if (r.status === "pending") status.say("Waiting for iyzico to confirm the payment…",
          el("button", { type: "button", class: "btn btn-small", id: "mp-purchase-refresh", onclick: () => checkPurchase(intent) }, "Check again"));
        else if (r.status === "paid") status.say("Payment received. Finish installing below.");
        else if (r.status === "installed") status.say("This purchase is installed.");
        else status.say(r.status === "expired" ? "The payment was not completed. Nothing was charged." : "This purchase did not go through.");
      } catch (err) { MP.report(deps, status, err, "services"); }
      draw();
    }

    /// The install a paid purchase admits: its id is the installation's.
    function finishForm(l, p) {
      const org = p.owner && p.owner.kind === "org" ? p.owner.id : null;
      const cfg = MP.configForm(l.config_schema, { idPrefix: "mp-finish-cfg", legend: "Settings" });
      const handle = org ? el("input", { id: "mp-finish-handle", required: true, pattern: "[a-z0-9][a-z0-9-]{1,30}", autocomplete: "off" }) : null;
      return el("form", { class: "ac-form", id: "mp-finish", onsubmit: async (e) => {
        e.preventDefault();
        const body = { listing_id: l.id, purchase_id: p.installation_id };
        const c = cfg.read();
        if (Object.keys(c).length) body.configuration = c;
        try {
          if (org) {
            body.handle = handle.value.trim();
            await deps.pb(`/v1/orgs/${encodeURIComponent(org)}/nest-installations`, { method: "POST", body: JSON.stringify(body) });
            status.say(`${l.title} is installed for the company. Manage it from the company's Nests.`);
          } else {
            await deps.pb("/v1/nest-installations", { method: "POST", body: JSON.stringify(body) });
            status.say(`${l.title} is installed.`);
          }
          st.purchase = null;
        } catch (err) { MP.report(deps, status, err, "services"); }
        await refreshInstalls();
      } }, el("fieldset", {}, el("legend", { text: "Finish installing" }),
        org ? el("label", { for: "mp-finish-handle" }, "Its address in the company (letters, digits, dashes)") : null, handle,
        cfg.empty ? null : cfg.node,
        el("button", { type: "submit", class: "btn btn-primary", id: "mp-finish-go" }, "Install")));
    }

    /// A paid installation's licence and payments, from the bridge.
    function licenceBlock(inst) {
      const box = el("div", { class: "mp-licence", id: "mp-licence" });
      const known = st.licences[inst.id];
      if (known === undefined) {
        st.licences[inst.id] = null;
        deps.bridgeGet(`/v1/marketplace/installations/${encodeURIComponent(inst.id)}`)
          .then((v) => { st.licences[inst.id] = v; draw(); })
          .catch((err) => { st.licences[inst.id] = { error: err }; draw(); });
      }
      if (!known) { box.append(el("p", { class: "muted", text: "Loading the licence…" })); return box; }
      if (known.error) { box.append(el("p", { class: "muted", text: MP.errorText(known.error) })); return box; }
      const l = known.licence || {};
      box.append(el("p", { id: "mp-licence-status", text: `Licence: ${LICENCE_TEXT[l.status] || l.status}${l.paid_through ? ` · paid through ${MP.when(l.paid_through)}` : ""}` }));
      if (l.deletion_due_at) box.append(el("p", { class: "pp-warn", text: `Its data is deleted on ${MP.when(l.deletion_due_at)} unless it is paid.` }));
      const pays = known.payments || [];
      if (pays.length) {
        box.append(el("ul", { class: "mp-list", id: "mp-payments", "aria-label": "Payments" }, pays.map((p) => el("li", { class: "mp-row" },
          el("span", { text: `${MP.when(p.settled_at)} · ${MP.money(p.amount_minor, p.currency)} · ${p.reversal_kind ? p.reversal_kind : p.status}` }),
          p.invoice_id ? el("button", { type: "button", class: "btn btn-small", "data-receipt": p.invoice_id, onclick: async () => {
            try {
              const { url } = await deps.bridgeGet(`/v1/billing/invoices/${encodeURIComponent(p.invoice_id)}/download`);
              if (/^https:\/\//.test(url || "")) window.open(url, "_blank", "noopener");
            } catch (err) { MP.report(deps, status, err, "services"); }
          } }, "Receipt") : null))));
      }
      if (known.term === "monthly" && l.status === "active" && !l.cancel_at_period_end) {
        box.append(el("button", { type: "button", class: "btn btn-small", id: "mp-cancel", onclick: async () => {
          try {
            await deps.bridgeAction(`/v1/marketplace/installations/${encodeURIComponent(inst.id)}/cancel`, {}, "POST");
            status.say("Cancelled. It keeps working until the end of the paid month.");
          } catch (err) { MP.report(deps, status, err, "services"); }
          delete st.licences[inst.id];
          draw();
        } }, "Cancel the monthly licence"));
      }
      return box;
    }

    function installForm(l) {
      const form = MP.configForm(l.config_schema, { idPrefix: "mp-install-cfg", legend: "Settings" });
      return el("form", { class: "ac-form", id: "mp-install", onsubmit: async (e) => {
        e.preventDefault();
        const body = { listing_id: l.id };
        const cfg = form.read();
        if (Object.keys(cfg).length) body.configuration = cfg;
        try {
          await deps.pb("/v1/nest-installations", { method: "POST", body: JSON.stringify(body) });
          status.say(`${l.title} is installed.`);
        } catch (err) { MP.report(deps, status, err, "services"); }
        await refreshInstalls();
      } }, form.empty ? null : form.node, el("button", { type: "submit", class: "btn btn-primary", id: "mp-install-go" }, "Install"));
    }

    async function refreshInstalls() {
      try { st.installs = (await deps.pb("/v1/nest-installations")).data || []; } catch (_) { /* keep what we had */ }
      draw();
    }

    function installationBlock(l, inst) {
      const path = `/v1/nest-installations/${encodeURIComponent(inst.id)}`;
      const box = el("section", { class: "mp-installation", id: "mp-installation", "aria-labelledby": "mp-installation-title" },
        el("h5", { id: "mp-installation-title", text: "Your installation" }),
        el("p", { text: `Installed version ${inst.version || "?"}${inst.funding === "test_credits" ? " · free test installation" : ""}` }));
      if (inst.security_state && inst.security_state !== "normal") {
        box.append(el("p", { class: "pp-warn", role: "alert", text: "Pigeonpost has stopped this version for a security review. It takes no jobs until that ends." }));
      }
      if (inst.listing_status && inst.listing_status !== "published") {
        box.append(el("p", { class: "muted", text: "The publisher has taken this listing down. Your installation stays until you remove it." }));
      }
      if (inst.funding === "card") box.append(licenceBlock(inst));
      const act = async (what, sub, body, done) => {
        try {
          await deps.pb(`${path}${sub}`, { method: what, body: body === undefined ? undefined : JSON.stringify(body) });
          status.say(done);
        } catch (err) { MP.report(deps, status, err, "services"); }
        await refreshInstalls();
      };
      const actions = el("div", { class: "mp-actions" });
      if (inst.update_available) {
        actions.append(el("button", { type: "button", class: "btn btn-secondary", id: "mp-update", onclick: () => act("POST", "/update", { release_id: inst.update_available.release_id }, `Updated to version ${inst.update_available.version}.`) },
          `Update to version ${inst.update_available.version}`));
      }
      if (inst.previous_release_id) {
        actions.append(el("button", { type: "button", class: "btn btn-small", id: "mp-rollback", onclick: () => act("POST", "/rollback", {}, "Went back to the previous version.") }, "Go back to the previous version"));
      }
      if (actions.childNodes.length) box.append(actions);

      const cfg = MP.configForm(inst.config_schema, { idPrefix: "mp-cfg", values: inst.configuration, secrets: inst.secrets, legend: "Settings" });
      if (!cfg.empty) {
        box.append(el("form", { class: "ac-form", id: "mp-configure", onsubmit: (e) => { e.preventDefault(); act("PUT", "/configuration", { configuration: cfg.read() }, "Settings saved."); } },
          cfg.node, el("button", { type: "submit", class: "btn btn-secondary" }, "Save settings")));
      }

      const typed = el("input", { id: "mp-uninstall-confirm", autocomplete: "off", spellcheck: "false" });
      const go = el("button", { type: "submit", class: "btn btn-small", id: "mp-uninstall-go", disabled: true }, "Uninstall");
      typed.addEventListener("input", () => { go.disabled = typed.value !== CONFIRM_WORD; });
      box.append(el("form", { class: "ac-form", id: "mp-uninstall", onsubmit: (e) => {
        e.preventDefault();
        if (typed.value !== CONFIRM_WORD) return;
        act("DELETE", "", undefined, `${l.title} is uninstalled. Its settings and secrets are deleted.`);
      } }, el("fieldset", {}, el("legend", { text: "Uninstall" }),
        el("p", { class: "muted", text: "This deletes its settings, secrets and data, and cancels its open jobs." }),
        el("label", { for: "mp-uninstall-confirm" }, `Type ${CONFIRM_WORD} to confirm`), typed, go)));
      return box;
    }

    function link(href, text, id) {
      return /^https:\/\//.test(href || "") ? el("a", { href, id, rel: "noopener noreferrer", target: "_blank", text }) : null;
    }

    /// The publisher card beside the description.
    function publisherCard(d) {
      const p = d.publisher || {};
      return el("div", { class: "mp-side-card", id: "mp-publisher-card", role: "group", "aria-labelledby": "mp-publisher-title" },
        el("h5", { id: "mp-publisher-title", text: "Publisher" }),
        el("p", { class: "mp-side-pub" }, MP.publisherLine(d)),
        el("p", { class: "muted", text: p.first_party
          ? "Made by Pigeonpost."
          : p.verified ? "A verified publisher: Pigeonpost checked who they are before they could list anything." : "" }),
        el("ul", { class: "mp-links" },
          el("li", {}, link(d.support_url, "Support", "mp-support-link") || el("a", { href: "/app-support.html", id: "mp-support-link", text: "Support" })),
          el("li", {}, link(d.privacy_url, "Privacy policy", "mp-privacy-link") || el("a", { href: "/privacy", id: "mp-privacy-link", text: "Privacy policy" })),
          el("li", {}, el("a", { href: "/app-support.html", text: "Report a problem" }))));
    }

    function factsCard(d) {
      const row = (k, v) => (v ? [el("dt", { text: k }), el("dd", { text: v })] : []);
      return el("div", { class: "mp-side-card", role: "group", "aria-labelledby": "mp-facts-title" },
        el("h5", { id: "mp-facts-title", text: "Details" }),
        el("dl", { class: "mp-facts" },
          row("Price", MP.cardPrice(d)),
          row("Category", MP.categoryLabel(d.category)),
          row("Runs", d.sample ? null : MP.hostingText(d)),
          row("Version", d.coming_soon ? null : d.version),
          row("Listed", d.coming_soon ? null : MP.when(d.published_at))));
    }

    function screenshots(d) {
      const shots = (Array.isArray(d.screenshots) ? d.screenshots : []).filter((x) => x && /^https:\/\//.test(x.url || ""));
      if (!shots.length) return null;
      return el("section", { class: "mp-shots", "aria-label": "Screenshots" },
        el("ul", { class: "mp-shot-list" }, shots.slice(0, 8).map((x, i) => el("li", {},
          el("img", { src: x.url, alt: x.alt || `Screenshot ${i + 1} of ${d.title}`, loading: "lazy", decoding: "async" })))));
    }

    /// The primary action: what this listing can do for you now.
    function comingSoonAction() {
      return el("div", { class: "mp-cta", id: "mp-cta" },
        el("button", { type: "button", class: "btn btn-primary", id: "mp-coming-soon", disabled: true, "aria-describedby": "mp-coming-note" }, "Coming soon"),
        el("p", { class: "ac-note", id: "mp-coming-note", text: "This is a preview of a service on its way. It can't be bought, installed or run yet." }));
    }

    function detailView() {
      const d = st.detail;
      const box = el("div", { class: "mp-detail", id: "mp-detail" },
        el("button", { type: "button", class: "btn btn-small mp-back", id: "mp-back", onclick: back }, "← Back to services"), status);
      if (!d) { box.append(el("p", { class: "muted", text: "Loading…" })); return box; }
      if (d.error) { box.append(el("p", { text: MP.errorText(d.error) })); return box; }
      const hero = el("header", { class: `mp-detail-hero${d.coming_soon ? " mp-detail-coming" : ""}` },
        MP.logo(d, { size: 96, alt: `${d.title} logo` }),
        el("div", { class: "mp-detail-titles" },
          el("h4", { id: "mp-detail-title", tabindex: "-1", text: d.title }), badge(d),
          el("div", { class: "mp-card-tags" },
            d.category && MP.categoryLabel(d.category) ? el("span", { class: "mp-chip", text: MP.categoryLabel(d.category) }) : null,
            d.coming_soon ? el("span", { class: "mp-chip mp-chip-soon", text: "Coming soon" }) : null,
            d.sample ? el("span", { class: "mp-chip mp-chip-sample", text: "Sample listing" }) : null),
          d.coming_soon ? null : el("p", { class: "muted mp-detail-meta", text: `Version ${d.version}${d.published_at ? ` · listed ${MP.when(d.published_at)}` : ""}` })));
      const main = el("div", { class: "mp-detail-main" },
        el("p", { class: "mp-detail-summary", text: d.summary }),
        screenshots(d),
        d.description ? el("p", { class: "mp-description", text: d.description }) : null,
        d.hosting === "publisher" && !d.coming_soon ? hostedNotice() : null);
      const side = el("aside", { class: "mp-detail-side", "aria-label": "About this listing" }, factsCard(d), publisherCard(d));
      box.append(hero, el("div", { class: "mp-detail-body" }, main, side));
      if (d.coming_soon) {
        main.prepend(comingSoonAction());
        return box;
      }
      main.append(el("h5", { text: "What it does" }), operationsBlock(d.operations || []));
      if (d.hosting === "publisher") {
        main.append(hostedBlock(d));
        return box;
      }
      main.append(priceBlock(d));
      const inst = installFor(d.id);
      const paid = st.purchase && st.purchase.listing_id === d.id && st.purchase.status === "paid" ? st.purchase : null;
      if (inst) main.append(installationBlock(d, inst));
      else if (paid) main.append(finishForm(d, paid));
      else if (d.price && Number(d.price.amount_minor) === 0) main.append(installForm(d));
      else main.append(buyForm(d));
      return box;
    }

    function draw() {
      const c = st.container;
      if (!c) return;
      c.textContent = "";
      c.append(st.open ? detailView() : listView());
    }

    return {
      /// Render into `container`; `builtins` is the products page's own "Coming soon" list.
      render(container, builtins) {
        st.container = container;
        st.builtins = builtins || null;
        draw();
        if (!st.listings && !st.error && st.gen === 0) {
          load();
          if (returning) {
            if (deps.clearReturn) { deps.clearReturn("mp_purchase"); deps.clearReturn("mp_checkout"); }
            checkPurchase(returning);
          }
        }
      },
      reload: () => load(),
      open: openListing,
    };
  };
})();
