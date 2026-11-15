// Developer partners (partners.html) in the marketplace template's look: the directory of approved
// publishers, and one publisher's page (?publisher=<id>) with its public apps as listing cards.
//
// Reads `GET /v1/marketplace/partners?q=&offset=` and `GET /v1/marketplace/partners/{id}`; a
// publisher's apps are drawn from the public catalog (`GET /v1/marketplace/listings`) when they are
// in it, and from the partner answer's own title and summary otherwise.
(function () {
  "use strict";

  const MP = window.PPMarketplace;
  const K = MP && MP.kit;
  const root = document.getElementById("partners-content");
  if (!MP || !K || !root) return;
  const el = MP.el;
  const icon = K.icon;
  let offset = 0;
  let q = "";
  const id = new URLSearchParams(location.search).get("publisher");

  async function api(path) {
    const r = await fetch(MP.postboxOrigin + path, { credentials: "omit", headers: { accept: "application/json" } });
    if (!r.ok) throw new Error(r.status === 404 ? "This publisher is not publicly available." : "Partners cannot be loaded right now. Please try again.");
    return r.json();
  }

  function publisherCard(p) {
    const n = Number(p.app_count) || 0;
    const tile = { id: p.id, title: p.profile.name };
    return el("article", { class: "pp-listing-card", "data-publisher": p.id },
      el("div", { class: "pp-listing-card__top" },
        K.logo(tile),
        el("div", { class: "pp-listing-card__id" },
          el("h2", { class: "pp-listing-card__title" }, el("a", { class: "pp-listing-card__link", href: K.publisherHref(p.id), text: p.profile.name })),
          el("p", { class: "pp-listing-card__publisher" }, el("span", { class: "pp-verified" }, icon("badge-check"), "Approved publisher")))),
      el("p", { class: "pp-listing-card__desc", text: p.profile.summary || "Independent developer" }),
      el("div", { class: "pp-listing-card__foot" },
        el("span", { class: "pp-listing-card__price" }, String(n), " ", el("small", { text: n === 1 ? "public app" : "public apps" })),
        el("span", { class: "pp-listing-card__host" }, icon("store"), "In the Marketplace")));
  }

  async function publisher() {
    const { data: p } = await api(`/v1/marketplace/partners/${encodeURIComponent(id)}`);
    let listings = [];
    try { listings = await K.fetchListings(); } catch (_) { listings = []; }
    const byId = new Map(listings.map((l) => [l.id, l]));
    const name = p.profile.name;
    document.title = `${name} — Pigeonpost publisher`;
    document.getElementById("partners-hero").hidden = true;
    let site = null;
    if (p.profile.website) {
      try {
        const u = new URL(p.profile.website);
        if (u.protocol === "https:") site = el("span", {}, icon("globe"), el("a", { href: u.href, target: "_blank", rel: "noopener noreferrer", text: "Publisher website" }));
      } catch (_) { site = null; }
    }
    const apps = Array.isArray(p.apps) ? p.apps : [];
    root.replaceChildren(
      el("nav", { class: "pp-breadcrumb mk-crumbs", "aria-label": "Breadcrumb" },
        el("ol", {},
          el("li", {}, el("a", { href: "/marketplace.html", text: "Marketplace" })),
          el("li", {}, el("a", { href: "/partners.html", text: "Developer partners" })),
          el("li", {}, el("span", { "aria-current": "page", text: name })))),
      el("div", { class: "mk-detail__main mk-publisher" },
        el("header", { class: "pp-listing-header mk-publisher__header" },
          K.logo({ id: p.id, title: name }, true),
          el("div", { class: "pp-listing-header__id" },
            el("p", { class: "pp-listing-header__crumbs", text: "Marketplace › Developer partners" }),
            el("h1", { class: "pp-listing-header__title", text: name }),
            el("p", { class: "pp-listing-header__summary", text: p.profile.summary || "Approved Pigeonpost developer partner." }),
            el("div", { class: "pp-listing-header__meta" },
              el("span", {}, el("span", { class: "pp-verified" }, icon("badge-check"), "Approved publisher")),
              el("span", {}, icon("store"), `${apps.length} public ${apps.length === 1 ? "app" : "apps"}`),
              site)))),
      el("h2", { class: "mk-section-title", id: "partner-apps", text: "Apps" }),
      apps.length
        ? el("div", { class: "pp-listing-grid", "aria-labelledby": "partner-apps" },
          apps.map((a) => K.card(byId.get(a.id) || { id: a.id, title: a.title, summary: a.summary, publisher: { display_name: name, verified: true }, hosting: null })))
        : K.empty("store", "No public apps yet", "This publisher has no public apps yet."));
  }

  async function directory() {
    const r = await api(`/v1/marketplace/partners?${new URLSearchParams({ q, offset })}`);
    const search = el("input", { type: "search", class: "pp-input__control", value: q, "aria-label": "Search publishers", placeholder: "Search publishers" });
    const form = el("form", { class: "mk-partners-search", role: "search", onsubmit: (e) => { e.preventDefault(); q = search.value; offset = 0; load(); } },
      el("div", { class: "pp-input pp-search pp-search--pill pp-input--l" }, icon("search", "pp-input__icon"), search),
      el("button", { type: "submit", class: "pp-btn pp-btn--primary pp-btn--l" }, "Search"));
    const rows = Array.isArray(r.data) ? r.data : [];
    const hero = document.getElementById("partners-hero");
    if (hero && !hero.querySelector("form")) hero.append(form);
    else if (hero) hero.querySelector("form").replaceWith(form);
    root.replaceChildren(...[
      rows.length
        ? el("div", { class: "pp-listing-grid mk-partner-grid", "aria-label": "Developer partners" }, rows.map(publisherCard))
        : K.empty("search", "No approved publishers match this search", null),
      offset > 0 || r.next_offset != null
        ? el("div", { class: "mk-pager" },
          el("button", { type: "button", class: "pp-btn", disabled: offset === 0, onclick: () => { offset = Math.max(0, offset - 25); load(); } }, icon("chevron-left"), "Previous"),
          el("button", { type: "button", class: "pp-btn", disabled: r.next_offset == null, onclick: () => { offset = r.next_offset; load(); } }, "Next", icon("chevron-right")))
        : null].filter(Boolean));
  }

  async function load() {
    root.replaceChildren(el("p", { class: "pp-body-s pp-muted mk-loading", text: "Loading…" }));
    try {
      if (id) await publisher(); else await directory();
    } catch (e) {
      root.replaceChildren(K.empty("cloud", e.message || "Partners cannot be loaded right now.", null,
        el("button", { type: "button", class: "pp-btn", onclick: load, text: "Try again" })));
    }
  }

  load();
})();
