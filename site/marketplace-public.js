// The public marketplace page (marketplace.html) on the design kit's marketplace template: the
// catalog (search, featured row, categories, listing cards) and a listing's page
// (?listing=<id>: header, what it does, how it was checked, price and the way to buy or run it).
//
// Read without signing in from the postbox: `GET /v1/marketplace/listings` and
// `GET /v1/marketplace/listings/{id}`. Buying and running happen in the account
// (account.pigeonpost.dev, Products → Services); this page only links there.
(function () {
  "use strict";

  const MP = window.PPMarketplace;
  const K = MP && MP.kit;
  const app = document.getElementById("mk-app");
  if (!MP || !K || !app || typeof fetch !== "function") return;
  const el = MP.el;
  const icon = K.icon;
  const hero = document.getElementById("mk-hero");
  const about = document.getElementById("mk-about");
  const search = document.getElementById("mk-q");
  const params = new URLSearchParams(location.search);

  // ---- catalog -------------------------------------------------------------------------------

  const state = { rows: [], q: params.get("q") || "", category: params.get("category") || "", now: false, sort: "featured" };

  const rank = (l) => (l.featured ? Number(l.featured_rank ?? 1e6) : 1e9);
  const priceCents = (l) => {
    if (l.coming_soon) return Infinity;
    if (l.price_from) return Number(l.price_from.amount_cents) || 0;
    return l.price ? Number(l.price.amount_minor) || 0 : 0;
  };
  const SORTS = {
    featured: (a, b) => Number(Boolean(a.coming_soon)) - Number(Boolean(b.coming_soon)) || rank(a) - rank(b) || a.title.localeCompare(b.title),
    newest: (a, b) => String(b.published_at || "").localeCompare(String(a.published_at || "")),
    price: (a, b) => priceCents(a) - priceCents(b) || a.title.localeCompare(b.title),
  };

  function matches(l) {
    if (state.category && l.category !== state.category) return false;
    if (state.now && l.coming_soon) return false;
    const q = state.q.trim().toLocaleLowerCase();
    if (!q) return true;
    const hay = [l.title, l.summary, l.description, K.publisherName(l), MP.categoryLabel(l.category)].join(" ").toLocaleLowerCase();
    return q.split(/\s+/).every((w) => hay.includes(w));
  }

  function syncUrl() {
    const p = new URLSearchParams();
    if (state.category) p.set("category", state.category);
    if (state.q.trim()) p.set("q", state.q.trim());
    const qs = p.toString();
    history.replaceState(null, "", `${location.pathname}${qs ? `?${qs}` : ""}`);
  }

  function featuredRow() {
    const picks = state.rows.filter((l) => l.featured).sort((a, b) => rank(a) - rank(b)).slice(0, 3);
    if (!picks.length) return null;
    return el("section", { class: "mk-featured", "aria-label": "Featured" },
      picks.map((l) => el("div", { class: "mk-feature-card" },
        el("div", { class: "pp-stack pp-stack--s" },
          el("span", { class: "pp-overline mk-feature-card__eyebrow", text: [l.coming_soon ? "Coming soon" : "Featured", MP.categoryLabel(l.category)].filter(Boolean).join(" · ") }),
          el("h2", { class: "mk-feature-card__title", text: l.title }),
          el("p", { text: l.summary || "" })),
        el("a", { class: "pp-btn pp-btn--s", href: K.listingHref(l.id), "aria-label": `View ${l.title}` }, "View"))));
  }

  function categoryCounts() {
    const counts = new Map();
    for (const l of state.rows) if (MP.categoryLabel(l.category)) counts.set(l.category, (counts.get(l.category) || 0) + 1);
    return [...counts.entries()].sort((a, b) => b[1] - a[1] || MP.categoryLabel(a[0]).localeCompare(MP.categoryLabel(b[0])));
  }

  function pick(code) {
    return (e) => {
      e.preventDefault();
      state.category = code;
      syncUrl();
      drawCatalog(true);
    };
  }

  function filters() {
    const cats = categoryCounts();
    const side = el("div", { class: "pp-sidebar__group mk-market__cats" },
      el("p", { class: "pp-sidebar__heading", text: "Categories" }),
      el("a", { class: "pp-sidebar__item", href: "/marketplace.html", "aria-current": state.category ? null : "page", onclick: pick("") },
        icon("layout-grid"), el("span", { class: "pp-sidebar__label", text: "All" }), el("span", { class: "pp-sidebar__count", text: String(state.rows.length) })),
      cats.map(([code, n]) => el("a", { class: "pp-sidebar__item", href: K.categoryHref(code), "aria-current": state.category === code ? "page" : null, onclick: pick(code) },
        icon(K.categoryIcon(code)), el("span", { class: "pp-sidebar__label", text: MP.categoryLabel(code) }), el("span", { class: "pp-sidebar__count", text: String(n) }))));
    const chips = el("div", { class: "pp-chip-group pp-chip-group--scroll mk-market__chips", role: "group", "aria-label": "Categories" },
      el("button", { type: "button", class: "pp-chip pp-chip--s", "aria-pressed": state.category ? "false" : "true", onclick: pick("") }, "All"),
      cats.map(([code]) => el("button", { type: "button", class: "pp-chip pp-chip--s", "aria-pressed": state.category === code ? "true" : "false", onclick: pick(code) }, MP.categoryLabel(code))));
    const live = state.rows.filter((l) => !l.coming_soon).length;
    const now = el("input", { class: "pp-switch", type: "checkbox", role: "switch", id: "mk-now", checked: state.now });
    now.addEventListener("change", () => { state.now = now.checked; drawCatalog(); });
    const toggle = el("label", { class: "pp-choice pp-choice--end mk-market__now", for: "mk-now" },
      el("span", { class: "pp-choice__text" },
        el("span", { class: "pp-choice__label", text: "Available now only" }),
        el("span", { class: "pp-choice__desc", text: `${live} of ${state.rows.length} can be bought or run today` })),
      now);
    return el("aside", { class: "mk-market__filters", "aria-label": "Filters" }, side, chips, toggle);
  }

  function results() {
    const shown = state.rows.filter(matches).sort(SORTS[state.sort]);
    const sort = el("select", { class: "pp-select__control", id: "mk-sort", "aria-label": "Sort" },
      [["featured", "Featured first"], ["newest", "Newest"], ["price", "Price, low to high"]].map(([v, t]) => el("option", { value: v, text: t, selected: state.sort === v })));
    sort.addEventListener("change", () => { state.sort = sort.value; drawCatalog(); });
    const label = { featured: "featured first", newest: "newest first", price: "price, low to high" }[state.sort];
    const where = state.category ? ` in ${MP.categoryLabel(state.category)}` : "";
    return el("div", { class: "pp-stack pp-stack--l mk-results" },
      el("div", { class: "pp-cluster pp-cluster--between mk-results__head" },
        el("p", { class: "pp-body-s pp-muted", id: "mk-count", text: `${shown.length} ${shown.length === 1 ? "listing" : "listings"}${where} · ${label}` }),
        el("div", { class: "pp-select pp-select--s mk-sort" }, sort, icon("chevron-down", "pp-select__chevron"))),
      shown.length
        ? el("div", { class: "pp-listing-grid", "aria-labelledby": "mk-count" }, shown.map((l) => K.card(l)))
        : K.empty("search", "No listings match", state.q ? `Nothing matches “${state.q.trim()}”${where}.` : `Nothing${where} yet.`,
          el("button", { type: "button", class: "pp-btn", onclick: () => { state.q = ""; state.category = ""; state.now = false; if (search) search.value = ""; syncUrl(); drawCatalog(); } }, "Show all listings")));
  }

  function drawCatalog(keepFocus) {
    const active = document.activeElement;
    const focusKey = active && active.closest && active.closest(".mk-market__filters") ? (active.getAttribute("href") || active.textContent) : null;
    const browsing = !state.q.trim() && !state.category && !state.now;
    app.replaceChildren(...[
      browsing ? featuredRow() : null,
      el("div", { class: "mk-market" }, filters(), results())].filter(Boolean));
    if (keepFocus && focusKey) {
      const again = [...app.querySelectorAll(".mk-market__filters a, .mk-market__filters button")].find((n) => (n.getAttribute("href") || n.textContent) === focusKey);
      if (again) again.focus();
    }
  }

  async function catalog() {
    if (search) {
      search.value = state.q;
      let t = 0;
      search.addEventListener("input", () => {
        clearTimeout(t);
        t = setTimeout(() => { state.q = search.value; syncUrl(); if (state.rows.length) drawCatalog(); }, 150);
      });
    }
    app.replaceChildren(el("p", { class: "pp-body-s pp-muted mk-loading", text: "Loading the catalog…" }));
    try {
      state.rows = await K.fetchListings();
    } catch (_) {
      app.replaceChildren(K.empty("cloud", "The catalog can't be loaded right now", "Please try again in a moment.",
        el("button", { type: "button", class: "pp-btn", onclick: catalog }, "Try again")));
      return;
    }
    if (!MP.categoryLabel(state.category)) state.category = "";
    if (search) search.placeholder = `Search ${state.rows.length} ${state.rows.length === 1 ? "listing" : "listings"}`;
    if (!state.rows.length) {
      app.replaceChildren(K.empty("store", "The catalog is just opening", "Listings are being added. Check back soon."));
      return;
    }
    drawCatalog();
  }

  // ---- a listing's page ---------------------------------------------------------------------

  function paragraphs(text) {
    return String(text || "").split(/\n{2,}/).map((p) => p.trim()).filter(Boolean).map((p) => el("p", { text: p }));
  }

  function opPrice(op) {
    const p = op && op.price;
    if (!p || !(Number(p.amount_cents) > 0)) return null;
    return MP.money(p.amount_cents, p.currency || "USD");
  }

  function outputText(op) {
    const o = (op && op.output) || {};
    if (o.kind === "file") return o.media_type === "application/pdf" ? "A PDF file, sent to you as a message" : "A file, sent to you as a message";
    if (o.kind) return "A message with the result";
    return null;
  }

  function operationBlock(op) {
    const fields = Array.isArray(op.fields) ? op.fields : [];
    const price = opPrice(op);
    const out = outputText(op);
    const facts = [
      price ? ["Price", `${price} per run`] : null,
      out ? ["You get", out] : null,
      op.deadline_hours ? ["Deadline", `${op.deadline_hours} hours`] : null,
    ].filter(Boolean);
    return el("section", { class: "mk-op" },
      el("h3", { class: "mk-op__title", text: op.title || op.name }),
      op.description ? el("p", { class: "mk-op__desc", text: op.description }) : null,
      facts.length ? el("dl", { class: "mk-facts" }, facts.map(([k, v]) => el("div", { class: "mk-facts__row" }, el("dt", { text: k }), el("dd", { text: v })))) : null,
      fields.length ? el("div", { class: "mk-op__fields" },
        el("p", { class: "mk-op__label", text: "You tell it" }),
        el("ul", { class: "mk-op__list" }, fields.map((f) => el("li", {},
          el("span", { text: f.label || f.name }),
          el("span", { class: f.required ? "pp-badge pp-badge--s pp-badge--accent" : "pp-badge pp-badge--s", text: f.required ? "Required" : "Optional" }))))) : null);
  }

  /// What is true of every listing here, by how it runs (the site's review copy).
  function trust(l) {
    const rows = [];
    if (l.sample) {
      rows.push(["sparkles", "A sample from Pigeonpost", "It shows what is coming. It is not available yet, and nothing can be bought or run from it."]);
    } else if (l.hosting === "publisher") {
      rows.push(["shield-check", "Reviewed by Pigeonpost", "Named Pigeonpost staff approved this listing before it was published. A new version is reviewed again."]);
      rows.push(["server", "Hosted by the publisher", "The service runs on the publisher's own systems and sends its result to you as a Pigeonpost message."]);
    } else {
      rows.push(["git-branch", "Built from source", "Pigeonpost builds every release from the publisher's source code and never runs an image a publisher uploads."]);
      rows.push(["scan-line", "Scanned and behaviour-tested", "Each release is scanned and tested before review."]);
      rows.push(["shield-check", "Approved by Pigeonpost staff", "Named staff approve each release before it is listed. A new version goes through review again."]);
      rows.push(["lock", "Your own copy", "You get an isolated worker with its own sandbox and storage. It cannot reach the internet, other Pigeonpost services or other buyers' Nests."]);
    }
    const p = l.publisher || {};
    if (p.verified && !p.first_party) rows.push(["badge-check", "Verified publisher", `${K.publisherName(l)} is a certified Pigeonpost publisher.`]);
    if (p.first_party) rows.push(["badge-check", "Made by Pigeonpost", "Published by Pigeonpost itself."]);
    const links = [
      l.support_url && /^https:\/\//.test(l.support_url) ? el("a", { href: l.support_url, target: "_blank", rel: "noopener noreferrer", text: "Support" }) : null,
      l.privacy_url && /^https:\/\//.test(l.privacy_url) ? el("a", { href: l.privacy_url, target: "_blank", rel: "noopener noreferrer", text: "Privacy" }) : null,
    ].filter(Boolean);
    return el("section", { class: "mk-trust", "aria-labelledby": "mk-trust-title" },
      el("h2", { class: "mk-trust__title", id: "mk-trust-title", text: "How this listing is checked" }),
      el("ul", { class: "mk-trust__list" }, rows.map(([i, t, d]) => el("li", { class: "mk-trust__item" },
        el("span", { class: "mk-trust__icon" }, icon(i)),
        el("span", { class: "mk-trust__text" }, el("strong", { text: t }), el("span", { text: d }))))),
      links.length ? el("p", { class: "mk-trust__links" }, links.flatMap((a, i) => (i ? [" · ", a] : [a]))) : null);
  }

  function order(l) {
    const ops = (Array.isArray(l.operations) ? l.operations : []).filter((op) => opPrice(op) && op.price.on_sale !== false);
    if (l.coming_soon) {
      return el("aside", { class: "pp-order mk-order", "aria-label": "Availability" },
        el("h2", { class: "pp-order__title", text: "Coming soon" }),
        el("p", { class: "pp-body-s pp-muted", text: "This listing isn't available yet, so it can't be bought or run." }),
        el("a", { class: "pp-btn pp-btn--l pp-btn--block", href: "/marketplace.html" }, icon("arrow-left"), "Browse the marketplace"));
    }
    if (ops.length) {
      const first = opPrice(ops[0]);
      const credits = ops.every((op) => op.price && op.price.funding === "credits");
      return el("aside", { class: "pp-order mk-order", "aria-label": "Run" },
        el("h2", { class: "pp-order__title", text: `Run ${l.title}` }),
        el("dl", { class: "pp-order__lines" },
          ops.map((op) => el("div", { class: "pp-order__line" }, el("dt", { text: `1 run · ${op.title || op.name}` }), el("dd", { text: opPrice(op) }))),
          ops.length === 1 ? el("div", { class: "pp-order__line pp-order__line--total" }, el("dt", { text: "Total" }), el("dd", { text: first })) : null),
        el("a", { class: "pp-btn pp-btn--primary pp-btn--l pp-btn--block", href: K.accountHref(l.id) }, icon("zap"), ops.length === 1 ? `Run for ${first}` : "Run in your account"),
        el("p", { class: "pp-caption", text: `You sign in and run it from your account at account.pigeonpost.dev${credits ? ", paid from your Pigeonpost credits" : ""}. The result arrives as a Pigeonpost message.` }));
    }
    const [amount, unit] = K.price(l);
    const free = amount === "Free";
    return el("aside", { class: "pp-order mk-order", "aria-label": free ? "Install" : "Buy" },
      el("h2", { class: "pp-order__title", text: `${free ? "Install" : "Get"} ${l.title}` }),
      el("dl", { class: "pp-order__lines" },
        el("div", { class: "pp-order__line pp-order__line--total" }, el("dt", { text: unit === "per month" ? "Monthly" : "Price" }), el("dd", { text: unit && unit !== "one-off" ? `${amount} ${unit}` : amount }))),
      el("a", { class: "pp-btn pp-btn--primary pp-btn--l pp-btn--block", href: K.accountHref(l.id) }, icon(free ? "download" : "shopping-bag"), free ? "Install from your account" : "Buy in your account"),
      el("p", { class: "pp-caption", text: "You sign in and finish on account.pigeonpost.dev, under Products → Services. The price is shown before you pay." }));
  }

  function selectTab(tabs, panels, i, focus) {
    tabs.forEach((t, j) => { t.setAttribute("aria-selected", String(i === j)); t.tabIndex = i === j ? 0 : -1; panels[j].hidden = i !== j; });
    if (focus) tabs[i].focus();
  }

  function tabset(items) {
    if (items.length === 1) return el("div", { class: "mk-prose" }, items[0][1]);
    const tabs = [];
    const panels = [];
    items.forEach(([label, body], i) => {
      const tab = el("button", { class: "pp-tab", type: "button", role: "tab", id: `mk-tab-${i}`, "aria-controls": `mk-panel-${i}`, "aria-selected": i === 0 ? "true" : "false", tabindex: i === 0 ? "0" : "-1" }, label);
      tab.addEventListener("click", () => selectTab(tabs, panels, i));
      tab.addEventListener("keydown", (e) => {
        const d = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
        if (d) { e.preventDefault(); selectTab(tabs, panels, (i + d + items.length) % items.length, true); }
      });
      tabs.push(tab);
      panels.push(el("div", { class: "pp-tab-panel mk-prose", role: "tabpanel", id: `mk-panel-${i}`, "aria-labelledby": `mk-tab-${i}`, tabindex: "0", hidden: i !== 0 }, body));
    });
    return el("div", { class: "mk-tabset" }, el("div", { class: "pp-tabs pp-tabs--scroll", role: "tablist", "aria-label": "Listing" }, tabs), panels);
  }

  function drawListing(l) {
    const category = MP.categoryLabel(l.category);
    const p = l.publisher || {};
    document.title = `${l.title} — Pigeonpost Marketplace`;
    const pubName = K.publisherName(l);
    const meta = [
      el("span", {}, p.first_party || !l.publisher_id ? pubName : el("a", { href: K.publisherHref(l.publisher_id), text: pubName }), " ", K.verified(l, true)),
      l.sample ? el("span", {}, icon("sparkles"), "Sample listing") : el("span", {}, icon("shield-check"), "Reviewed by Pigeonpost"),
      el("span", {}, icon(K.hosting(l)[0]), K.hosting(l)[1]),
      !l.sample && l.version ? el("span", {}, icon("tag"), `Version ${l.version}`) : null,
    ];
    const ops = Array.isArray(l.operations) ? l.operations : [];
    const sections = [["Overview", paragraphs(l.description || l.summary)]];
    if (ops.length) sections.push(["What it can do", ops.map(operationBlock)]);
    app.replaceChildren(
      el("nav", { class: "pp-breadcrumb mk-crumbs", "aria-label": "Breadcrumb" },
        el("ol", {},
          el("li", {}, el("a", { href: "/marketplace.html", text: "Marketplace" })),
          category ? el("li", {}, el("a", { href: K.categoryHref(l.category), text: category })) : null,
          el("li", {}, el("span", { "aria-current": "page", text: l.title })))),
      el("div", { class: "mk-detail" },
        el("div", { class: "pp-stack pp-stack--l mk-detail__main" },
          el("header", { class: "pp-listing-header" },
            K.logo(l, true),
            el("div", { class: "pp-listing-header__id" },
              el("p", { class: "pp-listing-header__crumbs", text: ["Marketplace", category].filter(Boolean).join(" › ") }),
              el("h1", { class: "pp-listing-header__title", text: l.title }),
              l.summary ? el("p", { class: "pp-listing-header__summary", text: l.summary }) : null,
              el("div", { class: "pp-listing-header__meta" }, meta))),
          tabset(sections),
          trust(l)),
        order(l)));
  }

  async function listing(id) {
    if (hero) hero.hidden = true;
    if (about) about.hidden = true;
    app.classList.add("mk-app--detail");
    app.replaceChildren(el("p", { class: "pp-body-s pp-muted mk-loading", text: "Loading…" }));
    let res;
    try {
      res = await fetch(`${MP.postboxOrigin}/v1/marketplace/listings/${encodeURIComponent(id)}`, { headers: { accept: "application/json" }, credentials: "omit" });
    } catch (_) { res = null; }
    if (!res || !res.ok) {
      const gone = res && (res.status === 404 || res.status === 403);
      app.replaceChildren(K.empty(gone ? "search" : "cloud", gone ? "This listing isn't available" : "This listing can't be loaded right now",
        gone ? "It may have been removed, or it isn't public." : "Please try again in a moment.",
        el("a", { class: "pp-btn", href: "/marketplace.html" }, "Browse the marketplace")));
      return;
    }
    const l = ((await res.json()) || {}).data;
    if (!l || !l.id) { app.replaceChildren(K.empty("search", "This listing isn't available", null, el("a", { class: "pp-btn", href: "/marketplace.html" }, "Browse the marketplace"))); return; }
    drawListing(l);
  }

  const id = params.get("listing");
  if (id) listing(id); else catalog();
})();
