// Marketplace catalog inside the Products page's Services tab (build plan R-49, OD-8): published
// listings with a publisher badge and a "Marketplace" filter, a listing's detail, and the buyer's
// own installations (install, settings, update, roll back, uninstall).
//
// Buying is web-only and not built yet: a listing with a price shows "Available soon". Only
// zero-priced listings can be installed. products.js asks this file for a controller once per
// mount and hands it a fresh container on every redraw; the controller keeps its own state.
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
(function () {
  "use strict";

  const MP = (window.PPMarketplace = window.PPMarketplace || {});
  const el = (...a) => MP.el(...a);
  const CONFIRM_WORD = "UNINSTALL";

  MP.catalog = function (deps) {
    const st = {
      filter: "all", query: "", listings: null, next: null, installs: [], error: null,
      open: null, detail: null, gen: 0, container: null, builtins: null,
    };
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

    function badge(l) {
      const p = l.publisher || {};
      return p.first_party
        ? el("span", { class: "mp-badge mp-badge-first", text: "By Pigeonpost" })
        : el("span", { class: "mp-badge", text: `Publisher: ${p.display_name || "unknown"}` });
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
      const input = el("input", { type: "search", id: "mp-cat-search-q", maxlength: "100", value: st.query || null, autocomplete: "off" });
      return el("form", { class: "ac-form mp-search", role: "search", id: "mp-cat-search", onsubmit: (e) => { e.preventDefault(); st.query = input.value.trim(); st.listings = null; draw(); load(); } },
        el("label", { for: "mp-cat-search-q" }, "Search the marketplace"), input,
        el("button", { type: "submit", class: "btn btn-small" }, "Search"));
    }

    function listView() {
      const box = el("div", { class: "mp-catalog", id: "mp-catalog" }, filterControls(), searchForm(), status);
      if (st.error) {
        box.append(el("p", { class: "muted", text: MP.errorText(st.error) }));
      } else if (!st.listings) {
        box.append(el("p", { class: "muted", text: "Loading the marketplace…" }));
      } else {
        const rows = st.listings;
        const mine = st.installs.filter((i) => i.status === "active");
        if (mine.length) {
          box.append(el("h4", { text: "Installed" }), el("ul", { class: "mp-list", "aria-label": "Installed services" }, mine.map((i) => {
            const l = rows.find((x) => x.id === i.listing_id);
            const name = l ? l.title : i.address || i.listing_id;
            return el("li", { class: "mp-row", "data-installation": i.id },
              el("strong", { text: name }), el("span", { class: "muted", text: `Version ${i.version || "?"}` }),
              i.update_available ? el("span", { class: "mp-badge", text: "Update available" }) : null,
              el("button", { type: "button", class: "btn btn-small", "aria-label": `Manage ${name}`, onclick: () => openListing(i.listing_id) }, "Manage"));
          })));
        }
        box.append(el("h4", { text: "Marketplace" }));
        if (!rows.length) {
          box.append(el("p", { class: "muted", id: "mp-cat-empty", text: st.query ? "Nothing matches that search." : "Nothing is listed yet." }));
        } else {
          box.append(el("ul", { class: "pp-cards mp-cards", "aria-label": "Marketplace listings" }, rows.map((l) => el("li", { class: "pp-card mp-card", "data-listing": l.id },
            el("h5", { text: l.title }), badge(l),
            el("p", { text: l.summary }),
            installFor(l.id) ? el("p", { class: "mp-badge", text: "Installed" }) : null,
            el("button", { type: "button", class: "btn btn-small", "aria-label": `Details for ${l.title}`, onclick: () => openListing(l.id) }, "Details")))));
          if (st.next) box.append(el("button", { type: "button", class: "btn btn-small", id: "mp-cat-more", onclick: () => load(true) }, "Show more"));
        }
      }
      if (st.filter === "all" && st.builtins) box.append(el("h4", { text: "From Pigeonpost" }), st.builtins);
      return box;
    }

    function operationsBlock(ops) {
      if (!ops.length) return el("p", { class: "muted", text: "This service lists no operations." });
      return el("ul", { class: "mp-ops", "aria-label": "Operations" }, ops.map((o) => el("li", {},
        el("code", { text: o.name }), ` version ${o.version || "?"}`,
        (o.fields || []).length ? el("span", { class: "muted", text: ` · asks for ${(o.fields || []).map((f) => f.name + (f.required ? "" : " (optional)")).join(", ")}` }) : null)));
    }

    function priceBlock(l) {
      const free = l.price && Number(l.price.amount_minor) === 0;
      return el("div", { class: "mp-price", id: "mp-price" },
        el("p", { class: "pp-price", text: free ? "Free while the marketplace is being tested." : "Available soon." }),
        el("p", { class: "ac-note", text: "Paid services will be sold here on the web, as a one-off purchase or monthly. Buying isn't open yet." }));
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

    function detailView() {
      const d = st.detail;
      const box = el("div", { class: "mp-detail", id: "mp-detail" },
        el("button", { type: "button", class: "btn btn-small", id: "mp-back", onclick: back }, "Back to services"), status);
      if (!d) { box.append(el("p", { class: "muted", text: "Loading…" })); return box; }
      if (d.error) { box.append(el("p", { text: MP.errorText(d.error) })); return box; }
      box.append(
        el("h4", { id: "mp-detail-title", tabindex: "-1", text: d.title }), badge(d),
        el("p", { class: "muted", text: `Version ${d.version}${d.published_at ? ` · listed ${MP.when(d.published_at)}` : ""}` }),
        el("p", { text: d.summary }),
        d.description ? el("p", { class: "mp-description", text: d.description }) : null,
        el("h5", { text: "What it does" }), operationsBlock(d.operations || []),
        priceBlock(d));
      const inst = installFor(d.id);
      if (inst) box.append(installationBlock(d, inst));
      else if (d.price && Number(d.price.amount_minor) === 0) box.append(installForm(d));
      box.append(el("p", { class: "ac-note" }, "Something wrong with this service? ", el("a", { href: "/app-support.html", text: "Tell Pigeonpost support" }), " and include its name."));
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
        if (!st.listings && !st.error && st.gen === 0) load();
      },
      reload: () => load(),
      open: openListing,
    };
  };
})();
