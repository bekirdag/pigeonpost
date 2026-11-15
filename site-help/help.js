// help.pigeonpost.dev: the help centre (design kit template "Status & help", help half).
//
// Routes are fragments, so every page is one static document under a script-src 'self' CSP:
//   #/                      home: search, categories, popular articles, quick answers, contact
//   #/c/<category>          a category's articles
//   #/a/<slug>[/<section>]  an article, optionally scrolled to one of its sections
//   #/search/<words>        search results
//   #/contact               every way to reach support
// Old links (#/for/<audience>, renamed article slugs) are redirected, never dropped.
//
// Article text comes from articles.js. Nothing is parsed as HTML: inline **bold**, `code` and
// [label](target) are turned into elements here, and everything else is a text node.
(function () {
  "use strict";
  const H = window.PP_HELP;
  const root = document.getElementById("hc-root");
  const SITE = "Pigeonpost Help";
  const DESCRIPTION = document.querySelector('meta[name="description"]');
  const HOME_DESCRIPTION = DESCRIPTION ? DESCRIPTION.getAttribute("content") : "";

  // ---- elements ------------------------------------------------------------------------------
  function el(tag, attrs, ...children) {
    const node = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v === undefined || v === null || v === false) continue;
      if (k.startsWith("on") && typeof v === "function") node.addEventListener(k.slice(2), v);
      else if (k === "class") node.className = v;
      else node.setAttribute(k, v === true ? "" : String(v));
    }
    for (const c of children.flat(Infinity)) {
      if (c === undefined || c === null || c === false || c === "") continue;
      node.append(c instanceof window.Node ? c : document.createTextNode(String(c)));
    }
    return node;
  }
  const SVG = "http://www.w3.org/2000/svg";
  function icon(name, cls) {
    const svg = document.createElementNS(SVG, "svg");
    svg.setAttribute("class", "pp-icon" + (cls ? " " + cls : ""));
    svg.setAttribute("aria-hidden", "true");
    const use = document.createElementNS(SVG, "use");
    use.setAttribute("href", `/kit/icons.svg#i-${name}`);
    svg.append(use);
    return svg;
  }

  // ---- data ----------------------------------------------------------------------------------
  const bySlug = new Map(H.articles.map((a) => [a.slug, a]));
  const aliases = new Map();
  for (const a of H.articles) for (const old of a.aliases || []) aliases.set(old, a.slug);
  const cat = (k) => H.categories.find((c) => c.key === k);
  const inCat = (k) => H.articles.filter((a) => a.cat === k);
  const LEGACY_AUDIENCE = { people: "getting-started", organizations: "enterprise", partners: "partners" };

  /// "Inbox is full" -> "inbox-is-full": a section's id inside its article.
  const sectionId = (text) => text.toLowerCase().normalize("NFKD").replace(/[^\w\s-]/g, "").trim().replace(/[\s_]+/g, "-").replace(/-+/g, "-");

  /// An article link target as written in articles.js -> an href. "/a/x", "/c/x", "/contact" and
  /// "/search/x" are help routes; anything with a scheme is left alone.
  function target(t) {
    if (/^(https?:|mailto:)/.test(t)) return t;
    if (t.startsWith("/")) return "#" + t;
    return t;
  }
  function inline(text) {
    const out = [];
    const re = /\*\*(.+?)\*\*|`([^`]+)`|\[([^\]]+)\]\(([^)\s]+)\)/g;
    let last = 0, m;
    while ((m = re.exec(text))) {
      if (m.index > last) out.push(text.slice(last, m.index));
      if (m[1] !== undefined) out.push(el("strong", {}, m[1]));
      else if (m[2] !== undefined) out.push(el("code", { class: "pp-code-inline" }, m[2]));
      else {
        const href = target(m[4]);
        const external = /^https?:/.test(href);
        out.push(el("a", { class: "pp-link", href, rel: external ? "noopener" : null }, m[3]));
      }
      last = re.lastIndex;
    }
    if (last < text.length) out.push(text.slice(last));
    return out;
  }
  const plain = (text) => text.replace(/\*\*(.+?)\*\*/g, "$1").replace(/`([^`]+)`/g, "$1").replace(/\[([^\]]+)\]\([^)\s]+\)/g, "$1");

  /// One body block -> elements. A string is a paragraph; arrays are typed blocks.
  function block(b) {
    if (typeof b === "string") return el("p", {}, inline(b));
    const [kind, ...rest] = b;
    switch (kind) {
      case "h": return el("h2", { id: sectionId(rest[0]), tabindex: "-1" }, rest[0]);
      case "steps": return el("ol", { class: "hc-steps" }, rest.map((s) => el("li", {}, inline(s))));
      case "list": return el("ul", {}, rest.map((s) => el("li", {}, inline(s))));
      case "note": case "warn": case "tip":
        return el("div", { class: `pp-alert pp-alert--${kind === "warn" ? "warning" : kind === "tip" ? "success" : "info"}`, role: "note" },
          icon(kind === "warn" ? "alert-triangle" : kind === "tip" ? "circle-check" : "info", "pp-alert__icon"),
          el("div", { class: "pp-alert__content" }, el("p", { class: "pp-alert__title" }, rest[0]), el("p", { class: "pp-alert__text" }, inline(rest[1]))));
      case "code": return el("pre", { class: "hc-code" }, el("code", {}, rest.join("\n")));
      default: throw new Error(`unknown block ${kind}`);
    }
  }
  function blockText(b) {
    if (typeof b === "string") return plain(b);
    return plain(b.slice(1).join(" "));
  }

  // ---- search --------------------------------------------------------------------------------
  const norm = (s) => s.toLowerCase().normalize("NFKD").replace(/[̀-ͯ]/g, "").replace(/ı/g, "i");
  const words = (s) => norm(s).split(/[^a-z0-9$.@/]+/).map((w) => w.replace(/^[./]+|[./]+$/g, "")).filter((w) => w.length > 1 || /\d/.test(w));
  const stem = (w) => w.length > 4 ? w.replace(/(ing|ed|es|s)$/, "") : w;
  const index = H.articles.map((a) => {
    const c = cat(a.cat);
    const heads = a.body.filter((b) => Array.isArray(b) && b[0] === "h").map((b) => b[1]);
    const fields = [
      [8, a.title], [4, (a.keywords || []).join(" ")], [3, a.summary], [3, c ? c.title : ""], [2, heads.join(" ")], [1, a.body.map(blockText).join(" ")],
    ].map(([w, t]) => [w, words(t).map(stem)]);
    return { a, fields };
  });
  const STOP = new Set("a an and are can do does for from get how i in is it me my of on or the to what when where why with you your".split(" "));
  function search(q) {
    const all = words(q);
    const kept = all.filter((w) => !STOP.has(w));
    const terms = [...new Set((kept.length ? kept : all).map(stem))];
    if (!terms.length) return [];
    return index.map((d) => {
      let score = 0, hit = 0;
      for (const t of terms) {
        let best = 0;
        for (const [w, ws] of d.fields) {
          if (ws.includes(t)) best = Math.max(best, w * 2);
          else if (t.length > 2 && ws.some((x) => x.startsWith(t))) best = Math.max(best, w);
        }
        if (best) hit++;
        score += best;
      }
      return { d, score, hit };
    }).filter((r) => r.hit === terms.length).sort((x, y) => y.score - x.score || x.d.a.title.localeCompare(y.d.a.title)).map((r) => r.d.a);
  }

  // ---- pieces --------------------------------------------------------------------------------
  function searchField(value, large) {
    const input = el("input", { class: "pp-input__control", type: "search", name: "q", placeholder: large ? "Search help, e.g. “refund”" : "Search help","aria-label": "Search help", autocomplete: "off", value: value || null });
    const form = el("form", { class: "hc-search-form", role: "search", action: "#" },
      el("div", { class: `pp-input pp-search pp-search--pill ${large ? "pp-input--l" : ""} hc-search` }, icon("search", "pp-input__icon"), input));
    form.addEventListener("submit", (e) => {
      e.preventDefault();
      const q = input.value.trim();
      if (q) location.hash = `#/search/${encodeURIComponent(q)}`;
    });
    return { form, input };
  }
  function card(href, iconName, title, subtitle) {
    return el("a", { class: "pp-card pp-card--interactive hc-card", href },
      el("div", { class: "pp-card__header" }, el("span", { class: "pp-card__icon" }, icon(iconName)),
        el("div", { class: "pp-card__heading" }, el("h3", { class: "pp-card__title" }, title), el("p", { class: "pp-card__subtitle" }, subtitle))));
  }
  function articleRow(a, showCat) {
    const c = cat(a.cat);
    return el("a", { class: "hc-row", href: `#/a/${a.slug}` },
      el("span", { class: "hc-row__text" }, el("strong", {}, a.title), el("span", {}, a.summary)),
      showCat && c ? el("span", { class: "pp-badge hc-row__cat" }, c.title) : null,
      icon("chevron-right", "hc-row__go"));
  }
  function crumbs(...parts) {
    return el("nav", { class: "pp-breadcrumb hc-crumbs", "aria-label": "Breadcrumb" },
      el("ol", {}, el("li", {}, el("a", { href: "#/" }, "Help")),
        parts.map((p, i) => el("li", {}, i === parts.length - 1 ? el("span", { "aria-current": "page" }, p[0]) : el("a", { href: p[1] }, p[0])))));
  }
  function contactBand() {
    return el("section", { class: "pp-cta-band pp-cta-band--compact hc-contact", "aria-label": "Contact support" },
      el("div", { class: "pp-cta-band__copy" }, el("h2", { class: "pp-cta-band__title" }, H.contact.title), el("p", { class: "pp-cta-band__text" }, H.contact.text)),
      el("div", { class: "pp-cta-band__actions" }, el("a", { class: "pp-btn pp-btn--primary", href: "#/contact" }, icon("message-circle"), "Contact support")));
  }
  function setMeta(title, description) {
    document.title = title ? `${title} · ${SITE}` : `${SITE}: guides for every Pigeonpost app`;
    if (DESCRIPTION) DESCRIPTION.setAttribute("content", description || HOME_DESCRIPTION);
  }
  function page(...children) { root.replaceChildren(...children.flat().filter(Boolean)); }

  // ---- pages ---------------------------------------------------------------------------------
  function home() {
    const { form } = searchField("", true);
    page(
      el("section", { class: "hc-hero" }, el("div", { class: "hc-wrap hc-hero__inner" },
        el("h1", { class: "pp-display-m" }, "How can we help?"),
        el("p", { class: "pp-body-l pp-muted" }, H.tagline), form)),
      el("div", { class: "hc-wrap hc-home" },
        el("section", { class: "pp-stack", "aria-labelledby": "hc-topics" }, el("h2", { class: "pp-h3", id: "hc-topics" }, "Browse by topic"),
          el("div", { class: "pp-grid pp-grid--3" }, H.categories.map((c) => card(`#/c/${c.key}`, c.icon, c.title, `${inCat(c.key).length} articles · ${c.text}`)))),
        el("section", { class: "pp-stack", "aria-labelledby": "hc-popular" }, el("h2", { class: "pp-h3", id: "hc-popular" }, "Popular articles"),
          el("div", { class: "pp-grid pp-grid--3" }, H.popular.map((s) => bySlug.get(s)).map((a) => card(`#/a/${a.slug}`, cat(a.cat).icon, a.title, a.summary)))),
        el("section", { class: "pp-stack", "aria-labelledby": "hc-quick" }, el("h2", { class: "pp-h3", id: "hc-quick" }, "Quick answers"),
          el("div", { class: "pp-faq" }, el("div", { class: "pp-faq__group" }, H.quick.map((q, i) =>
            el("details", { class: "pp-faq__item", name: "hc-faq", open: i === 0 },
              el("summary", { class: "pp-faq__q" }, q.q),
              el("div", { class: "pp-faq__a" }, el("p", {}, inline(q.a)))))))),
        contactBand()));
    setMeta("", "");
  }

  function category(key) {
    const c = cat(key);
    if (!c) return notFound();
    const list = inCat(key);
    page(inner(
      crumbs([c.title]),
      el("header", { class: "hc-head" }, el("span", { class: "pp-card__icon hc-head__icon" }, icon(c.icon)),
        el("div", {}, el("h1", { class: "pp-h1" }, c.title), el("p", { class: "pp-body-l pp-muted" }, c.text))),
      el("p", { class: "pp-caption" }, `${list.length} articles`),
      el("div", { class: "hc-rows" }, list.map((a) => articleRow(a, false))),
      el("nav", { class: "hc-other", "aria-label": "Other topics" }, el("p", { class: "pp-overline" }, "Other topics"),
        el("div", { class: "hc-chips" }, H.categories.filter((x) => x.key !== key).map((x) => el("a", { class: "pp-chip", href: `#/c/${x.key}` }, x.title)))),
      contactBand()));
    setMeta(c.title, c.text);
  }

  function article(slug, section) {
    const a = bySlug.get(slug);
    if (!a) return notFound(slug);
    const c = cat(a.cat);
    const heads = a.body.filter((b) => Array.isArray(b) && b[0] === "h").map((b) => b[1]);
    const siblings = inCat(a.cat);
    const related = (a.related || []).map((s) => bySlug.get(s)).filter(Boolean);
    const more = related.length ? related : siblings.filter((x) => x.slug !== a.slug).slice(0, 4);
    const thanks = el("p", { class: "pp-body-s hc-thanks", "aria-live": "polite" });
    const yes = el("button", { class: "pp-btn pp-btn--s", type: "button" }, icon("thumbs-up"), "Yes");
    const no = el("button", { class: "pp-btn pp-btn--s", type: "button" }, "No");
    const vote = (good) => {
      yes.disabled = no.disabled = true;
      (good ? yes : no).setAttribute("aria-pressed", "true");
      thanks.replaceChildren(...(good ? ["Thanks for letting us know."] : ["Sorry this didn't help. ", el("a", { class: "pp-link", href: "#/contact" }, "Tell our support team what you were looking for"), "."]));
    };
    yes.addEventListener("click", () => vote(true));
    no.addEventListener("click", () => vote(false));
    page(inner(
      crumbs([c.title, `#/c/${c.key}`], [a.title]),
      el("div", { class: "hc-article" },
        el("nav", { class: "hc-article__nav", "aria-label": `${c.title} articles` },
          el("p", { class: "pp-overline" }, c.title),
          siblings.map((x) => el("a", { href: `#/a/${x.slug}`, "aria-current": x.slug === a.slug ? "page" : null }, x.title))),
        el("article", { class: "hc-prose" },
          el("h1", {}, a.title),
          el("p", { class: "hc-lede" }, a.summary),
          a.body.map(block),
          el("div", { class: "hc-helpful" }, el("div", { class: "pp-cluster" }, el("span", { class: "pp-body-s pp-muted" }, "Was this helpful?"), yes, no), thanks)),
        el("aside", { class: "hc-article__side" },
          heads.length > 1 ? el("nav", { class: "hc-toc", "aria-label": "On this page" }, el("p", { class: "pp-overline" }, "On this page"),
            heads.map((h) => el("a", { href: `#/a/${a.slug}/${sectionId(h)}` }, h))) : null,
          el("div", { class: "pp-stack pp-stack--s hc-related" }, el("p", { class: "pp-overline" }, "Related"),
            more.map((x) => el("a", { class: "pp-link pp-link--standalone", href: `#/a/${x.slug}` }, x.title, icon("arrow-right")))))),
      el("a", { class: "pp-link pp-link--standalone hc-back", href: `#/c/${c.key}` }, icon("arrow-left"), `All ${c.title.toLowerCase()} articles`),
      contactBand()));
    setMeta(a.title, a.summary);
    return section ? document.getElementById(section) : null;
  }

  function results(q) {
    const found = search(q);
    page(inner(
      crumbs(["Search"]),
      el("h1", { class: "pp-h1" }, found.length ? `${found.length} ${found.length === 1 ? "article" : "articles"} for “${q}”` : `No articles for “${q}”`),
      found.length ? el("div", { class: "hc-rows" }, found.map((a) => articleRow(a, true)))
        : el("div", { class: "hc-empty" }, el("p", { class: "pp-body-l pp-muted" }, "Try fewer or different words, or browse a topic:"),
          el("div", { class: "hc-chips" }, H.categories.map((x) => el("a", { class: "pp-chip", href: `#/c/${x.key}` }, x.title)))),
      contactBand()), q);
    setMeta(`Search: ${q}`, "");
  }

  function contact() {
    page(inner(
      crumbs(["Contact support"]),
      el("header", { class: "hc-head" }, el("span", { class: "pp-card__icon hc-head__icon" }, icon("help-circle")),
        el("div", {}, el("h1", { class: "pp-h1" }, "Contact support"), el("p", { class: "pp-body-l pp-muted" }, H.contact.lede))),
      el("div", { class: "pp-grid pp-grid--2 hc-channels" }, H.contact.channels.map((ch) =>
        el("section", { class: "pp-card hc-channel" },
          el("div", { class: "pp-card__header" }, el("span", { class: "pp-card__icon" }, icon(ch.icon)),
            el("div", { class: "pp-card__heading" }, el("h2", { class: "pp-card__title" }, ch.title), el("p", { class: "pp-card__subtitle" }, ch.who))),
          el("div", { class: "pp-card__body" }, el("p", {}, inline(ch.text)),
            el("a", { class: "pp-btn pp-btn--s", href: target(ch.href) }, ch.action, icon("arrow-up-right")))))),
      el("section", { class: "pp-stack" }, el("h2", { class: "pp-h3" }, "Before you write"),
        el("ul", { class: "hc-list" }, H.contact.tips.map((t) => el("li", {}, inline(t)))))));
    setMeta("Contact support", H.contact.lede);
  }

  function notFound(slug) {
    page(inner(
      crumbs(["Not found"]),
      el("h1", { class: "pp-h1" }, "We couldn't find that page"),
      el("p", { class: "pp-body-l pp-muted" }, slug ? "The article may have moved. Search for it, or browse a topic." : "Search for what you need, or browse a topic."),
      el("div", { class: "hc-chips" }, H.categories.map((x) => el("a", { class: "pp-chip", href: `#/c/${x.key}` }, x.title))),
      contactBand()), slug ? slug.replace(/-/g, " ") : "");
    setMeta("Not found", "");
  }

  /// Inner pages: a search strip, then the page column.
  function inner(...children) {
    const q = arguments[children.length] || "";
    const { form } = searchField(typeof q === "string" ? q : "", false);
    return [el("div", { class: "hc-strip" }, el("div", { class: "hc-wrap" }, form)), el("div", { class: "hc-wrap hc-page" }, children)];
  }

  // ---- routing -------------------------------------------------------------------------------
  function parse(hash) {
    const raw = (hash || "").replace(/^#\/?/, "");
    const parts = raw.split("/").map((p) => { try { return decodeURIComponent(p); } catch (e) { return p; } });
    return { page: parts[0] || "", params: parts.slice(1) };
  }
  let lastPath = null;
  function route() {
    const { page: p, params } = parse(location.hash);
    // Old addresses keep working.
    if (p === "for") { location.replace(`#/c/${LEGACY_AUDIENCE[params[0]] || ""}`); return; }
    if (p === "a" && !bySlug.has(params[0]) && aliases.has(params[0])) { location.replace(`#/a/${aliases.get(params[0])}`); return; }
    let focus = null;
    const path = `${p}/${params[0] || ""}`;
    if (p === "a") focus = article(params[0], params[1]);
    else if (p === "c") category(params[0]);
    else if (p === "search") results(params.join("/"));
    else if (p === "contact") contact();
    else if (p === "") home();
    else notFound();
    if (focus) { focus.scrollIntoView({ block: "start" }); focus.focus({ preventScroll: true }); }
    else {
      window.scrollTo(0, 0);
      if (path !== lastPath) { const h = root.querySelector("h1"); if (h) { h.tabIndex = -1; h.focus({ preventScroll: true }); } }
    }
    lastPath = path;
  }
  window.addEventListener("hashchange", route);
  window.PPHelp = { search, sectionId, route };
  route();
})();
