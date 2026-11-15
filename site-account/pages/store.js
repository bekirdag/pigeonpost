// Store: tabs Inbox · Nests · Services (P27). Services is the marketplace catalog with logos (P12):
// search, category and sort; a listing page (#/store/services/<listing>) to run a publisher-hosted
// operation with credits, install a free service, buy a paid one, finish a paid install after
// iyzico (`?mp_purchase=<intent>`), and manage an installation (update, roll back, settings,
// licence and receipts, uninstall).
//
// Postbox: GET /v1/marketplace/listings[?q=&cursor=], GET /v1/marketplace/listings/{id},
//   GET/POST /v1/nest-installations, PUT …/{id}/configuration, POST …/{id}/update|rollback,
//   DELETE …/{id}, GET/POST /v1/marketplace/hosted-jobs, POST …/{job}/cancel.
// Bridge: GET /v1/credits/balance, GET /v1/marketplace/checkout/{intent},
//   GET /v1/marketplace/installations/{id}, POST …/{id}/cancel, GET /v1/billing/invoices/{id}/download.
(function () {
  "use strict";
  const el = (...a) => PP.el(...a);
  const CONFIRM_WORD = "UNINSTALL";
  const CATEGORIES = [
    ["productivity", "Productivity"], ["legal", "Legal"], ["finance", "Finance"], ["support", "Customer support"], ["marketing", "Marketing"],
    ["sales", "Sales"], ["language", "Language"], ["research", "Research"], ["developer", "Developer tools"], ["hr", "HR"], ["recruiting", "Recruiting"], ["other", "Other"],
  ];
  const catLabel = (c) => (CATEGORIES.find(([k]) => k === c) || [null, null])[1];
  // Pigeonpost's own assistants, not open yet.
  const BUILTINS = [
    ["Personal assistant", "Tasks, reminders and scheduling help from your own agent.", "productivity"],
    ["AI CRM", "Leads, pipeline and follow-ups, run by an agent your team can talk to.", "sales"],
    ["AI HR", "Job posts, candidate intake and interview briefs.", "hr"],
    ["Shopping assistant", "Compare products and prepare purchases you approve.", "other"],
    ["Marketing assistant", "Campaign plans and drafts, reviewed before anything is sent.", "marketing"],
  ].map(([title, summary, category]) => ({ id: `pp_${title.toLowerCase().replace(/[^a-z0-9]+/g, "_")}`, title, summary, category, coming_soon: true, builtin: true, publisher: { display_name: "Pigeonpost", first_party: true } }));
  const LICENCE = { active: ["Active", "success"], canceling: ["Active until the end of the paid period", "warning"], paused: ["Paused: payment is due", "warning"], expired: ["Ended", "neutral"], terminated: ["Ended", "neutral"], revoked: ["Revoked (refunded)", "danger"] };
  ACC.JOB_STATUS = { queued: ["Sending to the publisher", "info"], running: ["With the publisher", "info"], succeeded: ["Done", "success"], failed: ["Failed", "danger"], expired: ["Ran out of time", "warning"], cancelled: ["Cancelled", "neutral"] };
  ACC.CHARGE_TEXT = { held: "reserved", charged: "paid", released: "returned to your credit" };
  const MP_ERRORS = {
    feature_disabled: "The marketplace isn't available for your account yet.",
    already_installed: "You already have this installed.",
    installation_changed: "This installation changed in the meantime. Reload and try again.",
    invalid_configuration: "Some settings aren't valid.",
    configuration_invalid_for_release: "Your settings don't fit that version. Change them and try again.",
    no_previous_release: "There is no earlier version to go back to.",
    up_to_date: "You already have the newest version.",
    review_required: "This release hasn't been approved by Pigeonpost review.",
    security_quarantined: "This release is quarantined.",
    listing_suspended: "This listing is suspended.",
    insufficient_credit: "Your credit is too low for this run. Buy credits, then run it again.",
    not_on_sale: "This service's price is being updated. Try again in a minute.",
    job_closed: "This run has already finished.",
  };
  ACC.mpError = (e) => {
    const code = ACC.code(e);
    let text = (code && MP_ERRORS[code]) || PP.explain(e);
    const issues = e && e.body && Array.isArray(e.body.issues) ? e.body.issues : [];
    if (issues.length) text += " " + issues.map((i) => `${(i.path || "").replace(/^\//, "") || "settings"}: ${i.message || i.code}`).join("; ");
    return text;
  };

  // ---- logos and cards (kit: templates/marketplace.html — .pp-listing-card, .pp-listing-logo) ----

  const initials = (title) => {
    const words = String(title || "").replace(/\([^)]*\)/g, " ").split(/[^\p{L}\p{N}]+/u).filter(Boolean);
    if (!words.length) return "?";
    if (words.length === 1) { const w = Array.from(words[0]); return w[0].toLocaleUpperCase() + (w[1] || "").toLocaleLowerCase(); }
    return (Array.from(words[0])[0] + Array.from(words[1])[0]).toLocaleUpperCase();
  };
  const tone = (id) => { let h = 0x811c9dc5; const s = String(id || ""); for (let i = 0; i < s.length; i++) { h ^= s.charCodeAt(i); h = Math.imul(h, 0x01000193) >>> 0; } return h % 6; };
  const TONES = ["violet", "blue", "teal", "amber", "green", "red"];
  const CAT_ICON = { productivity: "file-text", legal: "briefcase", finance: "receipt", support: "headphones", marketing: "trending-up", sales: "line-chart", language: "languages", research: "search", developer: "git-branch", hr: "users", recruiting: "user-plus", other: "layout-grid" };
  const assetUrl = (p) => {
    const s = String(p || "");
    if (/^https:\/\/postbox\.pigeonpost\.dev\//.test(s)) return s;
    return /^\/v1\/marketplace\/listings\/[^/]+\/logo\/[0-9a-f]{16}\/(64|128|512)\.webp$/.test(s) ? window.PP_CONFIG.postbox + s : null;
  };
  /// A listing's logo; without an uploaded one, its initials on a tone picked from its id.
  ACC.logo = function (l, size = 44) {
    const big = size > 64 ? " pp-listing-logo--l" : "";
    const fallback = () => el("span", { class: `pp-listing-logo pp-listing-logo--${TONES[tone(l && l.id)]}${big} acc-logo`, "data-size": String(size), "aria-hidden": "true" }, initials(l && l.title));
    const urls = l && l.logo && l.logo.urls;
    const src = urls ? assetUrl(size > 64 ? urls["512"] : urls["128"]) : null;
    if (!src) return fallback();
    const img = el("img", { class: `pp-listing-logo${big} acc-logo`, "data-size": String(size), src, alt: "", width: String(size), height: String(size), loading: "lazy", decoding: "async" });
    img.addEventListener("error", () => img.replaceWith(fallback()), { once: true });
    return img;
  };
  /// The price as the kit's card shows it: the amount, then its unit in small type.
  const priceParts = (l) => {
    if (l.coming_soon) return ["Coming soon"];
    // A listing page carries each operation's price instead of `price_from`.
    const runs = (l.operations || []).map((o) => Number(o.price && o.price.amount_cents) || 0).filter((c) => c > 0);
    if (!l.price_from && runs.length) return [`${runs.length > 1 ? "From " : ""}${ACC.usd(Math.min(...runs))}`, "per run"];
    if (l.price_from && Number(l.price_from.amount_cents) > 0) return [ACC.usd(l.price_from.amount_cents), "per run"];
    const p = l.price;
    if (!p || Number(p.amount_minor) === 0) return ["Free"];
    const m = ACC.money(Number(p.amount_minor) / 100, p.currency);
    return p.term === "monthly" ? [m, "per month"] : [m, "one-off"];
  };
  const cardPrice = (l) => { const [a, b] = priceParts(l); return b ? `${a} ${b}` : a; };
  const priceText = (p) => {
    if (!p || Number(p.amount_minor) === 0) return "Free";
    const m = ACC.money(Number(p.amount_minor) / 100, p.currency);
    return p.term === "monthly" ? `${m} a month` : `${m}, one-off`;
  };
  const publisherName = (l) => { const p = l.publisher || {}; return p.first_party ? "Pigeonpost" : p.display_name || "Unknown publisher"; };
  const verified = (l) => { const p = l.publisher || {}; return p.first_party || p.verified; };
  const hosting = (l) => (l.hosting === "publisher" ? "Hosted by the publisher" : "Runs in your Nest");
  const linkable = (l) => !(l.builtin || (l.coming_soon && !String(l.id).startsWith("lst_")));
  const verifiedMark = (l, long) => (verified(l) ? el("span", { class: "pp-verified", title: (l.publisher || {}).first_party ? "Made by Pigeonpost" : "Verified publisher" }, PP.icon("badge-check"), long ? "Verified publisher" : "Verified") : null);

  function tile(l, { installed, version } = {}) {
    const href = PP.href("store", "services", l.id);
    const [amount, unit] = priceParts(l);
    return el("article", { class: `pp-listing-card acc-tile${l.coming_soon ? " pp-listing-card--soon is-soon" : ""}`, "data-listing": l.id },
      l.coming_soon ? el("span", { class: "pp-listing-card__ribbon", "aria-hidden": "true" }, "Coming soon") : null,
      el("div", { class: "pp-listing-card__top" }, ACC.logo(l),
        el("div", { class: "pp-listing-card__id" },
          el("h3", { class: "pp-listing-card__title" }, linkable(l) ? el("a", { class: "pp-listing-card__link", href, "aria-label": `${l.title}${l.coming_soon ? " (coming soon)" : ""}` }, l.title) : l.title),
          el("p", { class: "pp-listing-card__publisher" }, publisherName(l), " ", verifiedMark(l)))),
      l.category && catLabel(l.category) ? el("span", { class: "pp-listing-card__category" }, PP.icon(CAT_ICON[l.category] || "layout-grid"), catLabel(l.category)) : null,
      l.summary ? el("p", { class: "pp-listing-card__desc" }, l.summary) : null,
      el("div", { class: "pp-listing-card__foot" },
        el("span", { class: "pp-listing-card__price" }, amount, unit ? el("small", {}, ` ${unit}`) : null),
        installed ? PP.ui.badge(version ? `Installed · ${version}` : "Installed", "success", { dot: true })
          : l.coming_soon || l.sample ? null : el("span", { class: "pp-listing-card__host" }, PP.icon(l.hosting === "publisher" ? "server" : "egg"), hosting(l))));
  }

  // ---- catalog -------------------------------------------------------------------------------

  const st = { query: "", category: "", sort: "featured" };
  const published = (l) => Date.parse(l.published_at || "") || 0;
  const sortPrice = (l) => (l.coming_soon ? Infinity : l.price_from && Number(l.price_from.amount_cents) > 0 ? Number(l.price_from.amount_cents) : Number((l.price && l.price.amount_minor) || 0));
  function sorted(rows) {
    const out = rows.slice();
    if (st.sort === "newest") out.sort((a, b) => published(b) - published(a));
    else if (st.sort === "price") out.sort((a, b) => sortPrice(a) - sortPrice(b) || published(b) - published(a));
    else {
      const rank = (l) => (l.featured ? Number(l.featured_rank ?? 1e6) : 1e9);
      out.sort((a, b) => Number(Boolean(a.coming_soon)) - Number(Boolean(b.coming_soon)) || rank(a) - rank(b) || published(b) - published(a));
    }
    return out;
  }

  /// A featured listing as the kit's feature card: what it is, its real summary and price.
  function featureCard(l) {
    return el("div", { class: "acc-feature-card", "data-listing": l.id },
      el("div", { class: "acc-feature-text" },
        el("span", { class: "pp-overline" }, ["Featured", catLabel(l.category)].filter(Boolean).join(" · ")),
        el("h3", {}, l.title),
        el("p", {}, [l.summary, l.coming_soon ? "" : `${cardPrice(l)}.`].filter(Boolean).join(" "))),
      linkable(l) ? ACC.btn("View", { size: "sm", href: PP.href("store", "services", l.id) }) : null);
  }

  async function catalog(view) {
    const q = new URLSearchParams();
    if (st.query) q.set("q", st.query);
    const [page, installs] = await Promise.all([
      ACC.pb(`/v1/marketplace/listings${q.toString() ? "?" + q : ""}`),
      ACC.pb("/v1/nest-installations").catch(() => ({ data: [] })),
    ]);
    let rows = page.data || [];
    let next = page.next_cursor || null;
    const active = (installs.data || []).filter((i) => i.status === "active");
    const installedIds = new Map(active.map((i) => [i.listing_id, i]));

    // Search hero (kit: .kt-help-hero with a pill search).
    const search = el("input", { class: "pp-input pp-search acc-hero-search", type: "search", id: "acc-mp-q", placeholder: "Search services", value: st.query || null, maxlength: "100", "aria-label": "Search services" });
    const form = el("form", { class: "acc-hero-form", role: "search", onsubmit: (e) => { e.preventDefault(); st.query = search.value.trim(); PP.shell.refresh(); } },
      el("label", { class: "pp-search-wrap acc-hero-wrap" }, PP.icon("search"), search));
    const hero = el("section", { class: "acc-hero" },
      el("h2", { class: "pp-display-m" }, "Find a service"),
      el("p", { class: "pp-body-l pp-muted" }, "Every release is built from source and reviewed by Pigeonpost before it is listed. Publishers are verified before they can list anything."),
      form);

    const sort = ACC.select([["featured", "Featured"], ["newest", "Newest"], ["price", "Price: low to high"]], st.sort, { id: "acc-mp-sort", "aria-label": "Sort by" });
    sort.classList.add("pp-select");
    sort.addEventListener("change", () => { st.sort = sort.value; draw(); });
    // The category filter: the kit's sidebar list with counts, plus a select for phones.
    const cat = ACC.select([["", "All categories"], ...CATEGORIES], st.category, { id: "acc-mp-cat", "aria-label": "Category" });
    cat.classList.add("pp-select", "acc-cat-select");
    cat.addEventListener("change", () => { st.category = cat.value; draw(); });
    const filters = el("aside", { class: "acc-market-filters", "aria-label": "Categories" });
    const results = el("p", { class: "pp-body-s pp-muted", id: "acc-mp-count", role: "status" });
    const listArea = el("div", { class: "pp-stack pp-stack--l acc-market-main" });
    const top = el("div", { class: "acc-catalog" });

    function drawFilters() {
      const live = rows.filter((l) => !l.coming_soon || String(l.id).startsWith("lst_"));
      const counts = new Map();
      for (const l of live) counts.set(l.category, (counts.get(l.category) || 0) + 1);
      const item = (key, label, icon, n) => el("a", { class: "pp-sidebar__item", href: "#", "aria-current": st.category === key ? "page" : null, "data-category": key,
        onclick: (e) => { e.preventDefault(); st.category = key; cat.value = key; draw(); } },
      PP.icon(icon), el("span", { class: "pp-sidebar__label" }, label), el("span", { class: "pp-sidebar__count" }, String(n)));
      filters.replaceChildren(el("div", { class: "pp-sidebar__group" }, el("p", { class: "pp-sidebar__heading" }, "Categories"),
        item("", "All", "layout-grid", live.length),
        CATEGORIES.filter(([k]) => counts.get(k)).map(([k, label]) => item(k, label, CAT_ICON[k] || "layout-grid", counts.get(k)))));
    }

    function draw() {
      drawFilters();
      top.replaceChildren();
      if (active.length && !st.query && !st.category) {
        top.append(PP.ui.section({ title: "Your services", text: "Installed in your Nests." }, el("div", { class: "pp-listing-grid" }, active.map((i) => {
          const l = rows.find((x) => x.id === i.listing_id) || { id: i.listing_id, title: i.address || i.listing_id };
          return el("div", { class: "acc-installed", "data-installation": i.id }, tile(l, { installed: true, version: i.version ? `v${i.version}` : "" }),
            i.update_available ? PP.ui.badge("Update available", "info") : null);
        }))));
      }
      const featured = !st.query && !st.category ? sorted(rows.filter((l) => l.featured)).slice(0, 3) : [];
      if (featured.length) top.append(el("section", { class: "pp-section", "aria-label": "Featured" }, el("h2", { class: "pp-sr" }, "Featured"), el("div", { class: "acc-featured" }, featured.map(featureCard))));
      const shown = sorted(st.category ? rows.filter((l) => l.category === st.category) : rows);
      results.textContent = `${shown.length} ${shown.length === 1 ? "service" : "services"}${st.query ? ` for “${st.query}”` : ""}${st.category ? ` in ${catLabel(st.category)}` : ""}`;
      listArea.replaceChildren(
        el("div", { class: "pp-cluster pp-cluster--between" }, el("h2", { class: "acc-market-title" }, st.query ? `Results for “${st.query}”` : st.category ? catLabel(st.category) : "All services"), el("div", { class: "pp-cluster" }, cat, sort)),
        results,
        shown.length ? el("div", { class: "pp-listing-grid", id: "acc-mp-grid" }, shown.map((l) => tile(l, { installed: installedIds.has(l.id) })))
          : PP.ui.empty({ icon: "store", title: st.query ? "Nothing matches that search" : st.category ? "Nothing in this category yet" : "Nothing is listed yet", text: "New services are reviewed by Pigeonpost before they appear here." }),
        next ? el("div", {}, ACC.btn("Show more", { id: "acc-mp-more", onclick: async () => {
          const p = await ACC.pb(`/v1/marketplace/listings?${new URLSearchParams({ ...(st.query ? { q: st.query } : {}), cursor: next })}`);
          rows = rows.concat(p.data || []); next = p.next_cursor || null; draw();
        } })) : "");
      if (!st.query && !st.category) {
        const live = rows.filter((l) => !l.coming_soon).map((l) => ` ${String(l.title || "").toLowerCase()} `);
        const soon = BUILTINS.filter((b) => !live.some((t) => t.includes(` ${b.title.toLowerCase()} `)));
        if (soon.length) listArea.append(PP.ui.section({ title: "Coming from Pigeonpost", text: "Ready-made assistants that will run on a Nest." }, el("div", { class: "pp-listing-grid", id: "acc-mp-builtins" }, soon.map((b) => tile(b)))));
      }
    }
    draw();
    view.append(hero, top, el("div", { class: "acc-market" }, filters, listArea));
  }

  // ---- listing page ------------------------------------------------------------------------------

  /// One configuration form from a release's flat `config_schema`. A `writeOnly` property is a
  /// secret: never sent back, so its field is empty and says whether a value is set.
  ACC.configForm = function (schema, { prefix = "acc-cfg", values = {}, secrets = [] } = {}) {
    const set = new Set((secrets || []).filter((s) => s.set).map((s) => s.name));
    const props = (schema && schema.properties) || {};
    const required = new Set((schema && schema.required) || []);
    const names = Object.keys(props);
    const box = el("div", { class: "pp-form" });
    const readers = [];
    for (const name of names) {
      const p = props[name] || {};
      const id = `${prefix}-${name.replace(/[^A-Za-z0-9_-]/g, "_")}`;
      const label = p.title || name;
      const secret = p.writeOnly === true;
      const current = Object.prototype.hasOwnProperty.call(values, name) ? values[name] : p.default;
      let input;
      if (p.type === "boolean") {
        input = el("input", { type: "checkbox", id, checked: current === true ? true : null });
        box.append(el("div", { class: "pp-field pp-field-check" }, el("label", { for: id }, input, el("span", {}, label)), p.description ? el("p", { class: "pp-hint" }, p.description) : null));
        readers.push(() => [name, input.checked]);
        continue;
      }
      if (Array.isArray(p.enum)) input = ACC.select([...(required.has(name) ? [] : [["", "—"]]), ...p.enum.map((v) => [String(v), String(v)])], current === undefined ? "" : String(current), { id });
      else if (p.type === "integer" || p.type === "number") input = el("input", { type: "number", id, step: p.type === "integer" ? "1" : "any", min: p.minimum ?? null, max: p.maximum ?? null, value: current == null ? null : String(current) });
      else input = el("input", { type: secret ? "password" : "text", id, maxlength: p.maxLength ?? null, autocomplete: secret ? "new-password" : "off", spellcheck: "false", value: secret || current == null ? null : String(current) });
      if (required.has(name) && !(secret && set.has(name))) input.required = true;
      let clear = null;
      const extra = [];
      if (secret) {
        extra.push(el("p", { class: "pp-hint", "data-secret": name }, set.has(name) ? "Set. Leave empty to keep it." : "Not set."));
        if (set.has(name) && !required.has(name)) {
          clear = el("input", { type: "checkbox", id: `${id}-clear` });
          extra.push(el("label", { class: "acc-check", for: `${id}-clear` }, clear, ` Clear ${label}`));
        }
      }
      const f = ACC.field(label + (required.has(name) ? "" : ""), input, { hint: p.description, optional: !required.has(name) });
      f.append(...extra);
      box.append(f);
      readers.push(() => {
        if (secret) { if (clear && clear.checked) return [name, null]; return input.value === "" ? null : [name, input.value]; }
        if (input.value === "") return null;
        if (p.type === "integer") return [name, parseInt(input.value, 10)];
        if (p.type === "number") return [name, Number(input.value)];
        if (Array.isArray(p.enum)) { const hit = p.enum.find((v) => String(v) === input.value); return [name, hit === undefined ? input.value : hit]; }
        return [name, input.value];
      });
    }
    return { node: box, empty: !names.length, read: () => { const out = {}; for (const r of readers) { const kv = r(); if (kv) out[kv[0]] = kv[1]; } return out; } };
  };

  /// The run form for one publisher-hosted operation (kit: the marketplace template's .pp-order):
  /// its fields, what one run costs from the credit balance, and one primary button.
  function runCard(l, op, i, credit, status) {
    const prefix = `acc-run-${i}`;
    const inputs = [];
    const fields = (op.fields || []).map((f) => {
      const id = `${prefix}-${String(f.name).replace(/[^a-z0-9_]/g, "_")}`;
      const input = f.multiline ? el("textarea", { id, class: "pp-input", rows: "4", maxlength: f.max_chars ?? null, required: f.required ? true : null }) : el("input", { id, class: "pp-input", type: "text", maxlength: f.max_chars ?? null, required: f.required ? true : null, autocomplete: "off" });
      inputs.push([f.name, input]);
      return ACC.field(f.label || f.name, input, { optional: !f.required, wide: true });
    });
    const price = op.price && Number(op.price.amount_cents) > 0 ? op.price : null;
    let blocked = false;
    const notes = [];
    const line = (a, b, cls, id) => el("div", { class: `pp-order__line${cls ? ` ${cls}` : ""}`, id }, el("span", {}, a), el("span", {}, b));
    if (price) {
      const have = credit && Number.isInteger(credit.available_cents) ? credit.available_cents : null;
      const lines = [line("1 run", ACC.usd(price.amount_cents), "", `${prefix}-price`)];
      if (have !== null) {
        lines.push(line("Your credit", ACC.usd(have), "pp-order__line--muted", `${prefix}-balance`));
        if (have >= price.amount_cents) lines.push(line("Credit after this run", ACC.usd(have - price.amount_cents), "pp-order__line--muted"));
      }
      lines.push(line("Total", ACC.usd(price.amount_cents), "pp-order__line--total"));
      notes.push(el("div", { class: "pp-order__lines" }, lines));
      if (price.on_sale === false) { blocked = true; notes.push(el("p", { class: "pp-muted" }, MP_ERRORS.not_on_sale)); }
      else if (have === null) notes.push(el("p", { class: "pp-muted", id: `${prefix}-balance` }, "Your credit could not be loaded."));
      else if (have < price.amount_cents) {
        blocked = true;
        notes.push(PP.ui.gate({ text: `You need ${ACC.usd(price.amount_cents - have)} more credit to run this.`, action: { label: "Buy credits", href: PP.href("wallet") } }));
      }
    }
    const go = ACC.btn(price ? `Run for ${ACC.usd(price.amount_cents)}` : `Run ${op.title || op.name}`, { kind: "primary", size: "lg", type: "submit", id: `${prefix}-go`, disabled: blocked, icon: "zap" });
    go.classList.add("pp-btn--block");
    const form = el("form", { class: "acc-run-form", id: prefix, "data-operation": op.name, novalidate: true, onsubmit: async (e) => {
      e.preventDefault();
      if (go.disabled) return;
      const input = {};
      for (const [name, node] of inputs) if (node.value.trim() !== "") input[name] = node.value;
      go.disabled = true;
      try {
        await ACC.pb("/v1/marketplace/hosted-jobs", { method: "POST", body: { listing_id: l.id, operation: op.name, operation_version: op.version, input } });
        status.say(price ? `Sent to the publisher. ${ACC.usd(price.amount_cents)} is reserved from your credit; the result will arrive in your inbox.` : "Sent to the publisher. The result will arrive in your inbox.", "ok");
        for (const [, node] of inputs) node.value = "";
        ACC.forget("balance");
      } catch (err) {
        status.say(ACC.mpError(err), "error", ACC.code(err) === "insufficient_credit" ? ACC.btn("Buy credits", { size: "sm", href: PP.href("wallet") }) : null);
      }
      go.disabled = blocked;
    } }, fields, notes, go);
    return el("div", { class: "pp-order acc-order", "data-operation": op.name },
      el("h3", { class: "pp-order__title" }, op.title || op.name),
      op.description ? el("p", { class: "pp-body-s pp-muted" }, op.description) : null,
      form,
      el("p", { class: "pp-caption" }, price ? "The price is reserved when you run it and charged only once the result is in your inbox. If it fails or runs out of time, it comes back to your credit." : "The result is sent to your inbox as a PDF."));
  }

  function licenceCard(inst, status) {
    const box = PP.ui.card({ title: "Licence and payments" }, PP.ui.loading(1));
    (async () => {
      let v;
      try { v = await ACC.bridgeGet(`/v1/marketplace/installations/${encodeURIComponent(inst.id)}`, { keepSession: true }); }
      catch (e) { box.replaceChildren(el("p", { class: "pp-muted" }, ACC.mpError(e))); return; }
      const l = v.licence || {};
      box.replaceChildren(el("div", { class: "pp-card-head" }, el("h3", {}, "Licence and payments"), PP.ui.status(l.status, LICENCE)),
        PP.ui.kv([["Paid through", l.paid_through ? ACC.date(l.paid_through) : null], ["Term", v.term === "monthly" ? "Monthly" : "One-off"]]),
        l.deletion_due_at ? PP.ui.gate({ tone: "danger", text: `Its data is deleted on ${ACC.date(l.deletion_due_at)} unless it is paid.` }) : null,
        (v.payments || []).length ? PP.ui.table({ dense: true, caption: "Payments", rows: v.payments, columns: [
          { key: "settled_at", label: "Date", render: (p) => ACC.date(p.settled_at) },
          { key: "amount_minor", label: "Amount", num: true, render: (p) => ACC.money(Number(p.amount_minor) / 100, p.currency) },
          { key: "status", label: "Status", render: (p) => p.reversal_kind || p.status },
          { key: "invoice_id", label: "", render: (p) => (p.invoice_id ? ACC.btn("Receipt", { size: "sm", onclick: () => ACC.openInvoice(p.invoice_id) }) : null) },
        ] }) : null,
        v.term === "monthly" && l.status === "active" && !l.cancel_at_period_end ? ACC.btn("Cancel the monthly licence", { kind: "danger", size: "sm", id: "acc-mp-cancel", onclick: async () => {
          if (!(await PP.ui.confirm({ title: "Cancel this licence?", text: "It keeps working until the end of the paid month.", confirm: "Cancel licence", danger: true }))) return;
          try { await ACC.bridgeAction(`/v1/marketplace/installations/${encodeURIComponent(inst.id)}/cancel`, {}); status.say("Cancelled. It keeps working until the end of the paid month.", "ok"); }
          catch (e) { status.say(ACC.mpError(e), "error"); }
          PP.shell.refresh();
        } }) : null);
    })();
    return box;
  }

  function installationCard(l, inst, status) {
    const path = `/v1/nest-installations/${encodeURIComponent(inst.id)}`;
    const act = async (method, sub, body, done) => {
      try { await ACC.pb(`${path}${sub}`, { method, body }); PP.toast(done, "success"); PP.shell.refresh(); }
      catch (e) { status.say(ACC.mpError(e), "error"); }
    };
    const cfg = ACC.configForm(inst.config_schema, { prefix: "acc-cfg", values: inst.configuration || {}, secrets: inst.secrets });
    const typed = el("input", { class: "pp-input", id: "acc-uninstall-confirm", autocomplete: "off", spellcheck: "false" });
    const un = ACC.btn("Uninstall", { kind: "danger", type: "submit", id: "acc-uninstall-go", disabled: true });
    typed.addEventListener("input", () => { un.disabled = typed.value !== CONFIRM_WORD; });
    return [
      PP.ui.card({ title: "Your installation", text: `Version ${inst.version || "?"}${inst.funding === "test_credits" ? " · free test installation" : ""}`,
        actions: [inst.update_available ? ACC.btn(`Update to ${inst.update_available.version}`, { kind: "primary", size: "sm", id: "acc-mp-update", onclick: () => act("POST", "/update", { release_id: inst.update_available.release_id }, `Updated to version ${inst.update_available.version}.`) }) : null,
          inst.previous_release_id ? ACC.btn("Previous version", { size: "sm", id: "acc-mp-rollback", onclick: () => act("POST", "/rollback", {}, "Went back to the previous version.") }) : null] },
        inst.security_state && inst.security_state !== "normal" ? PP.ui.gate({ tone: "danger", text: "Pigeonpost has stopped this version for a security review. It takes no jobs until that ends." }) : null,
        inst.listing_status && inst.listing_status !== "published" ? el("p", { class: "pp-muted" }, "The publisher has taken this listing down. Your installation stays until you remove it.") : null,
        cfg.empty ? null : el("form", { class: "acc-stack", id: "acc-mp-configure", onsubmit: (e) => { e.preventDefault(); act("PUT", "/configuration", { configuration: cfg.read() }, "Settings saved."); } },
          el("h4", {}, "Settings"), cfg.node, el("div", { class: "pp-form-actions" }, ACC.btn("Save settings", { type: "submit" })))),
      inst.funding === "card" ? licenceCard(inst, status) : null,
      PP.ui.card({ tone: "danger", title: "Uninstall", text: "This deletes its settings, secrets and data, and cancels its open jobs." },
        el("form", { class: "acc-inline", onsubmit: (e) => { e.preventDefault(); if (typed.value === CONFIRM_WORD) act("DELETE", "", undefined, `${l.title} is uninstalled.`); } },
          ACC.field(`Type ${CONFIRM_WORD} to confirm`, typed), un)),
    ];
  }

  function installCard(l, status, purchase) {
    const cfg = ACC.configForm(l.config_schema, { prefix: purchase ? "acc-finish-cfg" : "acc-install-cfg" });
    return PP.ui.card({ title: purchase ? "Finish installing" : "Install", text: purchase ? "Payment received. Choose its settings and install it." : "Free while the marketplace is being tested." },
      el("form", { class: "acc-stack", id: purchase ? "acc-mp-finish" : "acc-mp-install", onsubmit: async (e) => {
        e.preventDefault();
        const body = { listing_id: l.id };
        if (purchase) body.purchase_id = purchase.installation_id;
        const c = cfg.read();
        if (Object.keys(c).length) body.configuration = c;
        try { await ACC.pb("/v1/nest-installations", { method: "POST", body }); PP.toast(`${l.title} is installed.`, "success"); PP.shell.refresh(); }
        catch (err) { status.say(ACC.mpError(err), "error"); }
      } }, cfg.empty ? null : cfg.node, el("div", { class: "pp-form-actions" }, ACC.btn("Install", { kind: "primary", type: "submit", id: "acc-mp-install-go" }))));
  }

  /// Buying a paid Nest service (kit: .pp-order with the price and the billing gate in front of Pay).
  function buyCard(l, ov) {
    const on = l.price && l.price.on_sale;
    const button = ACC.btn(`Buy · ${priceText(l.price)}`, { kind: "primary", size: "lg", id: "mp-buy-go", disabled: !on, onclick: () => {
      if (!ov.billing.profileId) { PP.go("billing"); return; }
      ACC.startCheckout({ kind: "marketplace", intent_id: ACC.hex32(), listing_id: l.id, title: l.title, owner: { kind: "account" }, billing_profile_id: ov.billing.profileId, locale: ACC.locale(), price_text: priceText(l.price) });
    } });
    button.classList.add("pp-btn--block");
    return el("div", { class: "pp-order acc-order" },
      el("h3", { class: "pp-order__title" }, `Buy ${l.title}`),
      el("div", { class: "pp-order__item" }, el("span", { class: "pp-order__item-icon" }, PP.icon("egg")),
        el("span", { class: "pp-order__item-text" }, el("strong", {}, l.title), el("span", {}, l.price.term === "monthly" ? "Monthly licence · runs in your Nest" : "One-off licence · runs in your Nest")),
        el("span", { class: "pp-order__item-price" }, priceText(l.price))),
      el("p", { class: "pp-caption" }, l.price.term === "monthly"
        ? "Sold by Piyote Bilişim on behalf of the publisher and billed monthly through iyzico. Cancel any time; it keeps working until the end of the paid month."
        : "Sold by Piyote Bilişim on behalf of the publisher. A one-off purchase through iyzico; the price includes tax."),
      on ? ACC.billingGate(ov, button) : el("p", { class: "pp-muted" }, "Not on sale right now. Check back soon."));
  }

  /// A listing (kit: the marketplace template's service detail): breadcrumb, the listing header,
  /// what it does and who publishes it on the left; the run, buy or install card in a sticky column.
  async function listing(view, ctx, id) {
    const status = ACC.statusLine();
    const [d, installs] = await Promise.all([
      ACC.pb(`/v1/marketplace/listings/${encodeURIComponent(id)}`).then((v) => v.data || v),
      ACC.pb("/v1/nest-installations").catch(() => ({ data: [] })),
    ]);
    PP.shell.crumb(d.title);
    const inst = (installs.data || []).find((i) => i.listing_id === d.id && i.status === "active");
    const category = d.category && catLabel(d.category);
    const crumbs = el("nav", { class: "pp-breadcrumb", "aria-label": "Breadcrumb" }, el("ol", {},
      el("li", {}, el("a", { href: PP.href("store", "services") }, "Services")),
      category ? el("li", {}, el("span", {}, category)) : null,
      el("li", {}, el("span", { "aria-current": "page" }, d.title))));
    const meta = [
      el("span", {}, publisherName(d), " ", verifiedMark(d, true)),
      el("span", {}, PP.icon("shield-check"), d.coming_soon ? "Reviewed before it opens" : "Reviewed by Pigeonpost"),
      d.coming_soon ? null : el("span", {}, PP.icon("tag"), `Version ${d.version || "?"}`),
      d.coming_soon || d.sample ? null : el("span", {}, PP.icon(d.hosting === "publisher" ? "server" : "egg"), hosting(d)),
      d.sample ? el("span", {}, PP.icon("info"), "Sample listing") : null,
      inst ? el("span", {}, PP.ui.badge("Installed", "success", { dot: true })) : null,
    ];
    const head = el("header", { class: "pp-listing-header" }, ACC.logo(d, 96),
      el("div", { class: "pp-listing-header__id" },
        category ? el("p", { class: "pp-listing-header__crumbs" }, `Services › ${category}`) : null,
        el("h1", { class: "pp-listing-header__title" }, d.title),
        d.summary ? el("p", { class: "pp-listing-header__summary" }, d.summary) : null,
        el("div", { class: "pp-listing-header__meta" }, meta)));
    const main = el("div", { class: "pp-stack pp-stack--l acc-detail-main" }, head, status);
    const side = el("div", { class: "pp-stack acc-detail-side" });
    view.append(crumbs, el("div", { class: "acc-detail" }, main, side));

    const shots = (Array.isArray(d.screenshots) ? d.screenshots : []).filter((x) => x && /^https:\/\/postbox\.pigeonpost\.dev\//.test(x.url || ""));
    if (shots.length) main.append(el("div", { class: "acc-shots" }, shots.slice(0, 8).map((x, i) => el("img", { src: x.url, alt: x.alt || `Screenshot ${i + 1}`, loading: "lazy" }))));
    if (d.description) main.append(el("section", { class: "acc-prose" }, el("h2", {}, "About"), el("p", { class: "acc-pre-line" }, d.description)));
    const ops = d.operations || [];
    const details = PP.ui.card({ title: "Details", icon: "info" }, PP.ui.kv([["Price", cardPrice(d)], ["Category", category], ["Runs", d.sample ? null : hosting(d)], ["Listed", d.coming_soon ? null : ACC.date(d.published_at)]]));
    const publisher = PP.ui.card({ title: "Publisher", icon: "building", text: (d.publisher || {}).first_party ? "Made by Pigeonpost." : verified(d) ? "A verified publisher: Pigeonpost checked who they are before they could list anything." : null },
      el("ul", { class: "acc-links" },
        el("li", {}, /^https:\/\//.test(d.support_url || "") ? ACC.ext(d.support_url, "Support") : el("a", { href: PP.href("support") }, "Support")),
        el("li", {}, /^https:\/\//.test(d.privacy_url || "") ? ACC.ext(d.privacy_url, "Privacy policy") : ACC.ext("https://pigeonpost.dev/privacy", "Privacy policy")),
        el("li", {}, el("a", { href: PP.href("support") }, "Report a problem"))));

    if (d.coming_soon) {
      side.append(PP.ui.gate({ tone: "info", text: "This is a preview of a service on its way. It can't be bought, installed or run yet." }), details);
      main.append(publisher);
      return;
    }
    if (d.hosting === "publisher") {
      main.append(PP.ui.gate({ tone: "info", text: "Hosted by the publisher — Pigeonpost reviewed the publisher and this integration, not the code; your inputs are sent to the publisher." }));
      if (ops.length) main.append(el("section", { class: "acc-prose" }, el("h2", {}, "What it can do"),
        el("ul", { class: "acc-ticks" }, ops.map((o) => el("li", {}, PP.icon("check"), el("span", {}, el("strong", {}, o.title || o.name), o.description ? ` — ${o.description}` : ""))))));
      main.append(details, publisher, el("p", {}, el("a", { href: PP.href("purchases") }, "See your runs and results in Purchases & jobs")));
      const priced = ops.some((op) => op.price && Number(op.price.amount_cents) > 0);
      const credit = priced ? await ACC.balance(true).catch(() => null) : null;
      ops.forEach((op, i) => side.append(runCard(d, op, i, credit, status)));
      return;
    }
    main.append(PP.ui.card({ title: "What it does", icon: "list" }, ops.length ? el("ul", { class: "acc-ticks" }, ops.map((o) => el("li", {}, PP.icon("check"), el("span", {}, el("code", {}, o.name), ` version ${o.version || "?"}`, (o.fields || []).length ? el("span", { class: "pp-muted" }, ` · asks for ${o.fields.map((f) => f.name + (f.required ? "" : " (optional)")).join(", ")}`) : null)))) : el("p", { class: "pp-muted" }, "This service lists no operations.")),
    details, publisher);
    const paid = ACC.purchase && ACC.purchase.listing_id === d.id && ACC.purchase.status === "paid" ? ACC.purchase : null;
    if (inst) {
      const [summary, ...rest] = installationCard(d, inst, status).filter(Boolean);
      side.append(summary);
      main.append(...rest);
    } else if (paid) side.append(installCard(d, status, paid));
    else if (!d.price || Number(d.price.amount_minor) === 0) side.append(installCard(d, status, null));
    else side.append(buyCard(d, await ACC.overview()));
  }

  /// Back from iyzico: `?mp_purchase=<intent>`. The redirect proves nothing; the bridge says where
  /// the purchase stands, and a paid one opens its listing to finish installing.
  async function purchaseReturn() {
    const intent = [ACC.param("mp_purchase"), ACC.param("mp_checkout")].find((v) => ACC.INTENT.test(v || ""));
    if (!intent) return null;
    ACC.clearParam("mp_purchase", "mp_checkout");
    try {
      const r = await ACC.bridgeGet(`/v1/marketplace/checkout/${intent}`, { keepSession: true });
      ACC.purchase = r;
      const msg = r.status === "pending" ? "Waiting for iyzico to confirm the payment. Reload in a moment."
        : r.status === "paid" ? "Payment received. Finish installing below."
          : r.status === "installed" ? "This purchase is installed."
            : r.status === "expired" ? "The payment was not completed. Nothing was charged." : "This purchase did not go through.";
      PP.toast(msg, r.status === "paid" || r.status === "installed" ? "success" : r.status === "pending" ? "info" : "error");
      return r.listing_id || null;
    } catch (e) { PP.toast(ACC.mpError(e), "error"); }
    return null;
  }

  function inboxTab(view) {
    view.append(el("div", { class: "pp-grid" },
      PP.ui.card({ title: "A name of your own", text: "An Inbox is a Pigeonpost name you own, like /you, with its mail storage." },
        el("p", { class: "pp-price" }, `${ACC.money(window.PP_CLOUD.perNameUSD, "USD")} per name per year`),
        el("p", { class: "pp-muted" }, "Up to 10 names in one Pigeonpost Cloud plan. Agents under your name are free."),
        el("div", { class: "pp-card-actions" }, ACC.btn("Find a name", { kind: "primary", icon: "search", href: PP.href("handles", "get"), id: "acc-store-find" }), ACC.btn("Your plan", { href: PP.href("plan") }))),
      PP.ui.card({ title: "Free for agents", text: "Hosted inboxes for Claude or ChatGPT agents cost nothing." },
        ACC.btn("Create an agent inbox", { href: PP.href("inboxes") }))));
  }

  ACC.page({
    key: "store", title: "Store", group: "", icon: "store",
    async render(view, ctx, params) {
      const back = await purchaseReturn();
      if (back && params[1] !== back) { PP.go("store", "services", back); return; }
      const tab = ["inbox", "nests", "services"].includes(params[0]) ? params[0] : "services";
      if (tab === "services" && params[1] && ACC.feature(ctx, "marketplace")) { await listing(view, ctx, params[1]); return; }
      view.append(PP.ui.header({ title: "Store", text: "Names, Nests and services from verified publishers." }),
        PP.ui.tabs([{ key: "inbox", label: "Inbox" }, { key: "nests", label: "Nests" }, { key: "services", label: "Services" }], tab, (k) => PP.go("store", k)));
      if (tab === "inbox") { inboxTab(view); return; }
      if (tab === "nests") {
        if (!ACC.feature(ctx, "nests")) { view.append(PP.ui.card({}, ACC.notYet("Nests aren't available for your account yet", "When they open for you, packages appear here.", "nest"))); return; }
        const [d, ov] = await Promise.all([ACC.nestData(), ACC.overview()]);
        view.append(el("p", { class: "pp-muted" }, "Private workspaces where your agents and installed services run. Manage the ones you have in ", el("a", { href: PP.href("nests") }, "Nests"), "."),
          ACC.nestPackages(d, ov, () => PP.shell.refresh()));
        return;
      }
      if (!ACC.feature(ctx, "marketplace")) {
        view.append(PP.ui.section({ title: "Coming from Pigeonpost", text: "Ready-made assistants that run on a Nest. They are not available yet." }, el("div", { class: "pp-tiles", id: "acc-mp-builtins" }, BUILTINS.map((b) => tile(b)))));
        return;
      }
      await catalog(view);
    },
  });

  /// A receipt or invoice PDF opens through the bridge's short-lived link.
  ACC.openInvoice = async function (id) {
    const { url } = await ACC.bridgeGet(`/v1/billing/invoices/${encodeURIComponent(id)}/download`, { keepSession: true });
    if (/^https:\/\//.test(url || "")) window.open(url, "_blank", "noopener");
  };
})();
