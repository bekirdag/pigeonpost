// help.pigeonpost.dev: home (search, audiences), an audience's articles, one article, contact.
// Routes: #/ · #/for/<audience> · #/a/<slug>. Text only through PP.el.
(function () {
  "use strict";
  const PP = window.PP;
  const el = (...a) => PP.el(...a);
  const H = window.PP_HELP;
  const root = document.getElementById("hc-root");
  const aud = (k) => H.audiences.find((a) => a.key === k);

  function articleLink(a) {
    return el("a", { class: "hc-article-link", href: `#/a/${a.slug}` }, el("strong", {}, a.title), el("span", {}, a.summary));
  }
  function contact(a) {
    return el("section", { class: "pp-card hc-contact" },
      el("h2", {}, "Still need help?"),
      el("p", { class: "pp-muted" }, "Message our team. Signed-in conversations are answered in the same place, live."),
      el("div", { class: "hc-contact-actions" },
        (a ? [a] : H.audiences).map((x) => el("a", { class: "pp-btn", href: x.support }, `${x.title}: contact support`)),
        el("a", { class: "pp-btn pp-btn-quiet", href: "mailto:support@pigeonpost.dev" }, "Email support@pigeonpost.dev")));
  }
  function search(q) {
    q = q.trim().toLowerCase();
    if (!q) return [];
    const words = q.split(/\s+/);
    return H.articles.map((a) => {
      const text = [a.title, a.summary, ...(a.steps || []), ...(a.points || [])].join(" ").toLowerCase();
      const score = words.reduce((n, w) => n + (a.title.toLowerCase().includes(w) ? 3 : 0) + (text.includes(w) ? 1 : 0), 0);
      return { a, score, all: words.every((w) => text.includes(w)) };
    }).filter((x) => x.all).sort((x, y) => y.score - x.score).map((x) => x.a);
  }
  function home() {
    const results = el("div", { class: "hc-results", "aria-live": "polite" });
    const input = el("input", { class: "pp-input hc-search", type: "search", placeholder: "Search help — e.g. refund, handle, payout", "aria-label": "Search help" });
    input.addEventListener("input", () => {
      const r = search(input.value);
      results.replaceChildren(input.value.trim() ? (r.length ? el("div", { class: "hc-list" }, r.map(articleLink)) : el("p", { class: "pp-muted" }, "No articles match. Try other words, or contact support below.")) : "");
    });
    root.replaceChildren(
      el("section", { class: "hc-hero" }, el("h1", {}, "How can we help?"), el("label", { class: "pp-search-wrap hc-search-wrap" }, PP.icon("search"), input), results),
      el("section", { class: "hc-audiences" }, H.audiences.map((a) => el("a", { class: "pp-card hc-aud", href: `#/for/${a.key}` },
        el("span", { class: "hc-aud-icon" }, PP.icon(a.icon)), el("h2", {}, a.title), el("p", { class: "pp-muted" }, a.text),
        el("span", { class: "hc-count pp-faint" }, `${H.articles.filter((x) => x.audience === a.key).length} articles`)))),
      contact());
    document.title = "Pigeonpost Help";
  }
  function audience(key) {
    const a = aud(key);
    if (!a) return home();
    root.replaceChildren(
      el("nav", { class: "hc-crumbs" }, el("a", { href: "#/" }, "Help"), " / ", el("span", {}, a.title)),
      el("h1", {}, a.title), el("p", { class: "pp-lede" }, a.text),
      el("div", { class: "hc-list" }, H.articles.filter((x) => x.audience === key).map(articleLink)),
      contact(a));
    document.title = `${a.title} — Pigeonpost Help`;
  }
  function article(slug) {
    const a = H.articles.find((x) => x.slug === slug);
    if (!a) return home();
    const au = aud(a.audience);
    root.replaceChildren(
      el("nav", { class: "hc-crumbs" }, el("a", { href: "#/" }, "Help"), " / ", el("a", { href: `#/for/${au.key}` }, au.title)),
      el("article", { class: "pp-card hc-article" },
        el("h1", {}, a.title), el("p", { class: "pp-lede" }, a.summary),
        a.steps ? el("ol", { class: "hc-steps" }, a.steps.map((s) => el("li", {}, s))) : null,
        a.points ? el("ul", { class: "hc-points" }, a.points.map((s) => el("li", {}, s))) : null),
      el("section", {}, el("h2", { class: "hc-more" }, "More for " + au.title.toLowerCase()), el("div", { class: "hc-list" }, H.articles.filter((x) => x.audience === a.audience && x.slug !== slug).slice(0, 4).map(articleLink))),
      contact(au));
    document.title = `${a.title} — Pigeonpost Help`;
  }
  function route() {
    const { page, params } = PP.parseRoute(window.location.hash);
    if (page === "a") article(params[0]); else if (page === "for") audience(params[0]); else home();
    window.scrollTo(0, 0);
    const h = root.querySelector("h1"); if (h) { h.tabIndex = -1; h.focus({ preventScroll: true }); }
  }
  window.addEventListener("hashchange", route);
  route();
})();
