// Pigeonpost platforms — the app shell every signed-in platform shares (account., enterprise.,
// partner., management.): left navigation, top bar with the platform switcher, theme switch and
// user menu, a hash router and the signed-out screen.
//
// A platform describes itself once:
//
//   PP.shell.start({
//     platform: "account",                 // this platform's key (PLATFORMS below)
//     title: "Account",                    // shown next to the logo
//     intro: { heading, text },            // the signed-out screen
//     context: async () => ({ ... }),      // who the caller is here; thrown errors show an error screen
//     pages: [{ key, title, group, icon, allow(ctx), count(ctx), render(view, ctx, params) }],
//     themePosition: "left",               // optional: the theme switch at the top left (default: right)
//     navHeader: (ctx) => node,            // optional: drawn above the navigation (e.g. an org picker)
//   });
//
// `allow(ctx)` hides a page the caller may not use (the postbox still enforces every route);
// `count(ctx)` puts a number badge on the navigation entry; `tag` a small text label ("soon"); `render` fills `view` and may be async.
// Routes are `#/<page>/<param>/<param>`; the first allowed page is the default.
//
// Rules: nothing from a server is written as HTML (PP.el only); one page renders at a time and a
// later navigation cancels an earlier render's output (`view.isConnected` turns false).
(function () {
  "use strict";
  const PP = (window.PP = window.PP || {});
  const el = (...a) => PP.el(...a);

  /// Every platform, in switcher order. `url` is the platform's home; a platform appears in the
  /// switcher only when the caller's context says they have it (`ctx.platforms`).
  const PLATFORMS = [
    { key: "account", name: "Account", url: "https://account.pigeonpost.dev/", text: "Your inboxes, handles, Nests and purchases" },
    { key: "enterprise", name: "Enterprise", url: "https://enterprise.pigeonpost.dev/app", text: "Your organization's people, agents and billing" },
    { key: "partner", name: "Partner", url: "https://partner.pigeonpost.dev/", text: "Your business, apps, pricing and earnings" },
    { key: "management", name: "Management", url: "https://management.pigeonpost.dev/", text: "Pigeonpost staff" },
  ];
  PP.PLATFORMS = PLATFORMS;

  // Stroke icons (24×24 paths), built as SVG nodes.
  const ICONS = {
    home: ["M3 11.5 12 4l9 7.5", "M5 10v10h14V10", "M10 20v-6h4v6"],
    user: ["M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2", "M12 3a4 4 0 1 0 0 8 4 4 0 0 0 0-8Z"],
    down: ["m6 9 6 6 6-6"],
    inbox: ["M4 13h4l1.5 3h5l1.5-3h4", "M5.5 5h13L21 13v5a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-5Z"],
    at: ["M16 12a4 4 0 1 1-8 0 4 4 0 0 1 8 0Z", "M16 12v1.5a2.5 2.5 0 0 0 5 0V12a9 9 0 1 0-3.6 7.2"],
    nest: ["M4 14c0 3.3 3.6 6 8 6s8-2.7 8-6", "M4 14c2-1.5 5-2 8-2s6 .5 8 2", "M12 12V7", "M9 7h6", "M12 4v3"],
    store: ["M4 9h16l-1.5-5h-13Z", "M5 9v10h14V9", "M9 19v-5h6v5"],
    jobs: ["M9 5h6M9 3h6v4H9z", "M6 5H5a1 1 0 0 0-1 1v14a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1V6a1 1 0 0 0-1-1h-1", "m8.5 13.5 2 2 4.5-4.5"],
    wallet: ["M4 7a2 2 0 0 1 2-2h12v4", "M4 7v11a2 2 0 0 0 2 2h14V9H6a2 2 0 0 1-2-2Z", "M16 14.5h.01"],
    plan: ["M12 3 3 8l9 5 9-5Z", "m3 13 9 5 9-5"],
    billing: ["M3 7a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z", "M3 10h18M7 15h3"],
    invoice: ["M7 3h10l3 3v15l-3-2-2 2-3-2-3 2-2-2-3 2V6Z", "M9 9h6M9 13h6"],
    security: ["M12 3 5 6v5c0 4.5 3 8.3 7 10 4-1.7 7-5.5 7-10V6Z", "m9 12 2 2 4-4"],
    privacy: ["M6 10V8a6 6 0 1 1 12 0v2", "M5 10h14v10H5z", "M12 14v2"],
    support: ["M4 5h16v11H9l-5 4Z", "M8 9h8M8 12h5"],
    people: ["M16 20a4 4 0 0 0-8 0", "M12 13a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7Z", "M20.5 19a3.5 3.5 0 0 0-3-3.4M17 6.2a3 3 0 0 1 0 5.6M3.5 19a3.5 3.5 0 0 1 3-3.4M7 6.2a3 3 0 0 0 0 5.6"],
    agents: ["M7 7h10a3 3 0 0 1 3 3v6a3 3 0 0 1-3 3H7a3 3 0 0 1-3-3v-6a3 3 0 0 1 3-3Z", "M12 3v4M9 12h.01M15 12h.01M9.5 15.5h5"],
    rooms: ["M4 5h16v10H9l-5 4Z"],
    rules: ["M4 6h10M18 6h2M4 12h4M12 12h8M4 18h12", "M16 4v4M10 10v4M18 16v4"],
    key: ["M8 19a4 4 0 1 0 0-8 4 4 0 0 0 0 8Z", "m10.8 12.2 8.7-8.7M17 6l2.5 2.5M14.5 8.5l2 2"],
    webhook: ["M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1", "M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1"],
    building: ["M4 21V5a1 1 0 0 1 1-1h9a1 1 0 0 1 1 1v16M15 9h4a1 1 0 0 1 1 1v11M3 21h18", "M8 8h3M8 12h3M8 16h3"],
    apps: ["M4 4h7v7H4zM13 4h7v7h-7zM4 13h7v7H4zM13 13h7v7h-7z"],
    tag: ["M3 12V4h8l10 10-8 8Z", "M7.5 7.5h.01"],
    chart: ["M4 20V10M10 20V4M16 20v-7M22 20H2"],
    money: ["M12 3v18", "M16.5 7.5c0-1.7-2-3-4.5-3s-4.5 1.3-4.5 3S9.5 10 12 10.5s4.5 1.3 4.5 3-2 3-4.5 3-4.5-1.3-4.5-3"],
    form: ["M6 3h9l4 4v14H6z", "M9 12h7M9 16h7M9 8h3"],
    docs: ["M5 4h10l4 4v12H5z", "M9 12h6M9 16h4"],
    review: ["M3 12s3.5-7 9-7 9 7 9 7-3.5 7-9 7-9-7-9-7Z", "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6Z"],
    flag: ["M5 21V4h11l-1.5 4L16 12H5"],
    server: ["M4 4h16v6H4zM4 14h16v6H4z", "M8 7h.01M8 17h.01"],
    audit: ["M12 8v4l3 2", "M3.5 12a8.5 8.5 0 1 0 2.5-6L3 9", "M3 4v5h5"],
    grid: ["M4 4h4v4H4zM10 4h4v4h-4zM16 4h4v4h-4zM4 10h4v4H4zM10 10h4v4h-4zM16 10h4v4h-4zM4 16h4v4H4zM10 16h4v4h-4zM16 16h4v4h-4z"],
    menu: ["M4 7h16M4 12h16M4 17h16"],
    close: ["M6 6l12 12M18 6 6 18"],
    out: ["M15 4h3a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2h-3M10 16l-4-4 4-4M6 12h10"],
    search: ["M11 18a7 7 0 1 0 0-14 7 7 0 0 0 0 14Z", "m20 20-4-4"],
    plus: ["M12 5v14M5 12h14"],
    chevron: ["m9 6 6 6-6 6"],
    external: ["M14 4h6v6", "M20 4 11 13", "M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5"],
    copy: ["M9 9h11v11H9z", "M5 15H4V4h11v1"],
    check: ["m5 12 5 5 9-10"],
    alert: ["M12 4 2.5 20h19Z", "M12 10v4M12 17h.01"],
    info: ["M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18Z", "M12 11v5M12 8h.01"],
  };
  PP.icon = function (name, cls) {
    const NS = "http://www.w3.org/2000/svg";
    const svg = document.createElementNS(NS, "svg");
    svg.setAttribute("class", cls ? `ico ${cls}` : "ico");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("aria-hidden", "true");
    svg.setAttribute("focusable", "false");
    for (const d of ICONS[name] || ICONS.grid) {
      const p = document.createElementNS(NS, "path");
      p.setAttribute("d", d);
      svg.append(p);
    }
    return svg;
  };

  // The design kit's brand lockup and platform tiles (design.pigeonpost.dev, Platform switcher).
  const PLATFORM_ICONS = { account: "user", enterprise: "building", partner: "store", management: "security" };
  function logo() {
    return el("img", { class: "pp-brand-logo", src: "/shared/brand/logo-symbol.png", alt: "", width: "28", height: "26" });
  }
  function tile(key) {
    return el("span", { class: "pp-platform-tile", "data-platform": key, "aria-hidden": "true" }, PP.icon(PLATFORM_ICONS[key] || "grid"));
  }

  // ---- routing -------------------------------------------------------------------------------

  /// `#/page/a/b` → { page: "page", params: ["a", "b"] }. Unknown text is just an unknown page.
  PP.parseRoute = function (hash) {
    const parts = String(hash || "").replace(/^#\/?/, "").split("/").filter(Boolean).map((p) => {
      try { return decodeURIComponent(p); } catch { return p; }
    });
    return { page: parts[0] || "", params: parts.slice(1) };
  };
  PP.href = (page, ...params) => "#/" + [page, ...params].filter((p) => p !== undefined && p !== null && p !== "").map((p) => encodeURIComponent(String(p))).join("/");
  PP.go = (page, ...params) => { window.location.hash = PP.href(page, ...params); };

  // ---- the shell -----------------------------------------------------------------------------

  const state = { opts: null, ctx: null, current: null, renderSeq: 0 };
  PP.shell = PP.shell || {};
  PP.shell.state = state;

  /// Re-read the context (after something changed who the caller is: an application submitted,
  /// a role granted) and redraw the navigation and the current page.
  PP.shell.reload = async function () {
    state.ctx = await state.opts.context();
    drawNav();
    drawUser();
    route();
  };
  /// Redraw the current page only (after an action on it).
  PP.shell.refresh = () => route();
  PP.shell.ctx = () => state.ctx;

  function allowed(page) {
    try { return !page.allow || Boolean(page.allow(state.ctx)); } catch { return false; }
  }
  function visiblePages() { return state.opts.pages.filter(allowed); }

  function frame() {
    const o = state.opts;
    const nav = el("nav", { class: "pp-nav", id: "pp-nav", "aria-label": `${o.title} sections` });
    const brand = el("a", { class: "pp-brand", href: PP.href(""), "aria-label": `Pigeonpost ${o.title}` },
      logo(),
      el("span", { class: "pp-brand-text" }, el("strong", {}, "Pigeonpost"), el("span", {}, o.title)));
    const side = el("aside", { class: "pp-side", id: "pp-side" },
      el("div", { class: "pp-side-head" }, brand,
        el("button", { class: "pp-icon-btn pp-side-close", type: "button", "aria-label": "Close menu", onclick: () => setDrawer(false) }, PP.icon("close"))),
      nav,
      el("div", { class: "pp-side-foot", id: "pp-side-foot" }));
    const left = o.themePosition === "left";
    const theme = () => el("button", { type: "button", class: `pp-icon-btn pp-theme${left ? " pp-theme-left" : ""}`, "data-theme-toggle": "" });
    const top = el("header", { class: "pp-top" },
      el("button", { class: "pp-icon-btn pp-menu-btn", type: "button", "aria-label": "Open menu", "aria-controls": "pp-side", onclick: () => setDrawer(true) }, PP.icon("menu")),
      left ? theme() : null,
      el("div", { class: "pp-switch", id: "pp-switch" }),
      el("div", { class: "pp-crumbs", id: "pp-crumbs" }),
      el("div", { class: "pp-top-actions" },
        left ? null : theme(),
        el("div", { class: "pp-user", id: "pp-user" })));
    const main = el("main", { class: "pp-main", id: "pp-main", tabindex: "-1" });
    const scrim = el("div", { class: "pp-drawer-scrim", onclick: () => setDrawer(false) });
    const app = el("div", { class: "pp-app" }, side, scrim, el("div", { class: "pp-body" }, top, main));
    document.body.replaceChildren(app);
    if (window.PPTheme && window.PPTheme.refresh) window.PPTheme.refresh();
  }

  function setDrawer(open) {
    document.body.classList.toggle("pp-drawer-open", Boolean(open));
    if (open) { const first = document.querySelector("#pp-nav a"); if (first) first.focus(); }
  }

  function drawNav() {
    const nav = document.getElementById("pp-nav");
    if (!nav) return;
    nav.replaceChildren();
    const old = document.getElementById("pp-nav-header");
    if (old) old.remove();
    if (state.opts.navHeader) {
      let head = null;
      try { head = state.opts.navHeader(state.ctx); } catch { head = null; }
      if (head) nav.before(el("div", { class: "pp-nav-header", id: "pp-nav-header" }, head));
    }
    const groups = new Map();
    for (const p of visiblePages()) {
      if (p.hidden) continue;
      const g = p.group || "";
      if (!groups.has(g)) groups.set(g, []);
      groups.get(g).push(p);
    }
    for (const [g, pages] of groups) {
      const list = el("ul", { class: "pp-nav-list" });
      for (const p of pages) {
        let count = null;
        try { count = p.count ? p.count(state.ctx) : null; } catch { count = null; }
        list.append(el("li", {},
          el("a", { class: "pp-nav-item", href: PP.href(p.key), "data-page": p.key },
            PP.icon(p.icon || "grid"), el("span", { class: "pp-nav-label" }, p.title),
            p.tag ? el("span", { class: "pp-nav-tag" }, p.tag) : null,
            count ? el("span", { class: "pp-nav-count", "aria-label": `${count} waiting` }, String(count > 99 ? "99+" : count)) : null)));
      }
      nav.append(el("div", { class: "pp-nav-group" }, g ? el("p", { class: "pp-nav-heading" }, g) : null, list));
    }
    const foot = document.getElementById("pp-side-foot");
    if (foot) {
      foot.replaceChildren();
      for (const link of state.opts.footLinks || []) {
        foot.append(el("a", { class: "pp-foot-link", href: link.href, target: link.external ? "_blank" : null, rel: link.external ? "noopener" : null }, PP.icon(link.icon || "external"), link.label));
      }
    }
    markActive();
  }

  function markActive() {
    for (const a of document.querySelectorAll(".pp-nav-item")) {
      const on = a.getAttribute("data-page") === (state.current && state.current.key);
      a.classList.toggle("is-active", on);
      if (on) a.setAttribute("aria-current", "page"); else a.removeAttribute("aria-current");
    }
  }

  /// The platforms the caller has, as the context reports them (always including this one).
  function myPlatforms() {
    const keys = new Set([state.opts.platform, ...((state.ctx && state.ctx.platforms) || [])]);
    return PLATFORMS.filter((p) => keys.has(p.key));
  }

  function drawUser() {
    const slot = document.getElementById("pp-user");
    const sw = document.getElementById("pp-switch");
    if (!slot || !sw) return;
    slot.replaceChildren();
    sw.replaceChildren();
    const who = (state.ctx && state.ctx.user) || PP.whoami();
    const name = who.name || who.username || who.email || "Signed in";
    const initial = (name.trim().charAt(0) || "?").toUpperCase();

    const platforms = myPlatforms();
    if (platforms.length > 1) {
      const list = el("div", { class: "pp-pop", id: "pp-switch-pop", hidden: true, role: "menu" },
        el("p", { class: "pp-pop-heading" }, "Switch to"),
        platforms.map((p) => el("a", { class: "pp-pop-item" + (p.key === state.opts.platform ? " is-current" : ""), href: p.url, role: "menuitem", "aria-current": p.key === state.opts.platform ? "true" : null },
          tile(p.key), el("span", { class: "pp-pop-text" }, el("strong", {}, p.name), el("span", {}, p.text)), PP.icon("check", "pp-pop-check"))));
      const btn = el("button", { class: "pp-switch-btn", type: "button", "aria-haspopup": "true", "aria-expanded": "false", "aria-controls": "pp-switch-pop" },
        tile(state.opts.platform), el("span", {}, state.opts.title), PP.icon("down"));
      btn.addEventListener("click", (e) => { e.stopPropagation(); togglePop("pp-switch-pop", btn); });
      sw.append(btn, list);
    }

    const menu = el("div", { class: "pp-pop pp-pop-right", id: "pp-user-pop", hidden: true, role: "menu" },
      el("div", { class: "pp-user-id", translate: "no" }, el("strong", {}, name), who.email && who.email !== name ? el("span", {}, who.email) : (who.username && who.username !== name ? el("span", {}, who.username) : null)),
      (state.opts.userLinks || []).map((l) => el("a", { class: "pp-pop-item pp-pop-row", href: l.href, role: "menuitem" }, PP.icon(l.icon || "external"), l.label)),
      el("a", { class: "pp-pop-item pp-pop-row", href: "https://inbox.pigeonpost.dev", role: "menuitem" }, PP.icon("inbox"), "Open your inbox"),
      el("a", { class: "pp-pop-item pp-pop-row", href: "https://help.pigeonpost.dev", role: "menuitem" }, PP.icon("support"), "Help"),
      el("button", { class: "pp-pop-item pp-pop-row", type: "button", role: "menuitem", onclick: () => PP.signOut() }, PP.icon("out"), "Sign out"));
    const btn = el("button", { class: "pp-user-btn", type: "button", "aria-haspopup": "true", "aria-expanded": "false", "aria-controls": "pp-user-pop" },
      el("span", { class: "pp-avatar", "aria-hidden": "true", translate: "no" }, initial),
      el("span", { class: "pp-user-name", translate: "no" }, name));
    btn.addEventListener("click", (e) => { e.stopPropagation(); togglePop("pp-user-pop", btn); });
    slot.append(btn, menu);
  }

  function togglePop(id, btn) {
    const pop = document.getElementById(id);
    if (!pop) return;
    const open = pop.hidden;
    closePops();
    pop.hidden = !open;
    btn.setAttribute("aria-expanded", String(open));
  }
  function closePops() {
    for (const p of document.querySelectorAll(".pp-pop")) p.hidden = true;
    for (const b of document.querySelectorAll("[aria-controls^='pp-'][aria-expanded]")) b.setAttribute("aria-expanded", "false");
  }

  function crumbs(page, extra) {
    const c = document.getElementById("pp-crumbs");
    if (!c) return;
    // replaceChildren() would print a null as text, so only nodes go in.
    c.replaceChildren(el("span", { class: "pp-crumb-group" }, page.group || state.opts.title),
      PP.icon("chevron", "pp-crumb-sep"),
      extra ? el("a", { href: PP.href(page.key) }, page.title) : el("span", { class: "pp-crumb-here" }, page.title),
      ...(extra ? [PP.icon("chevron", "pp-crumb-sep"), el("span", { class: "pp-crumb-here" }, extra)] : []));
  }
  /// A detail page names itself in the breadcrumbs (e.g. the listing being edited).
  PP.shell.crumb = function (text) { if (state.current) crumbs(state.current, text); };

  async function route() {
    const { page: key, params } = PP.parseRoute(window.location.hash);
    const pages = visiblePages();
    let page = pages.find((p) => p.key === key);
    if (!page) {
      page = pages[0];
      if (!page) { showNoAccess(); return; }
      if (key) { window.history.replaceState(null, "", PP.href(page.key)); }
    }
    state.current = page;
    markActive();
    crumbs(page);
    setDrawer(false);
    closePops();
    document.title = `${page.title} — Pigeonpost ${state.opts.title}`;
    const seq = ++state.renderSeq;
    const main = document.getElementById("pp-main");
    const view = el("div", { class: "pp-view", "data-page": page.key });
    main.replaceChildren(view);
    view.append(PP.ui ? PP.ui.loading() : el("p", { class: "muted" }, "Loading…"));
    try {
      const fill = el("div", { class: "pp-view-inner" });
      await page.render(fill, state.ctx, params);
      if (seq !== state.renderSeq) return;
      view.replaceChildren(fill);
    } catch (e) {
      if (seq !== state.renderSeq || e instanceof PP.Redirecting) return;
      view.replaceChildren(PP.ui ? PP.ui.errorState(e, () => route()) : el("p", {}, PP.explain(e)));
    }
    if (!params.length) main.scrollTop = 0;
    const h = view.querySelector("h1");
    if (h) { h.setAttribute("tabindex", "-1"); h.focus({ preventScroll: true }); }
  }

  function showNoAccess() {
    const main = document.getElementById("pp-main");
    const o = state.opts;
    main.replaceChildren(el("div", { class: "pp-view" }, PP.ui.empty({
      icon: "privacy",
      title: o.noAccess && o.noAccess.title || "Nothing here for this account",
      text: o.noAccess && o.noAccess.text || "This account has no access to this platform.",
      action: o.noAccess && o.noAccess.action,
    })));
  }

  function signedOut() {
    const o = state.opts;
    const intro = o.intro || {};
    const card = el("section", { class: "pp-signin-card" },
      el("div", { class: "pp-signin-brand" }, logo(), el("span", {}, el("strong", {}, "Pigeonpost"), " ", o.title)),
      el("h1", {}, intro.heading || `Sign in to Pigeonpost ${o.title}`),
      intro.text ? el("p", { class: "pp-lede" }, intro.text) : null,
      intro.points ? el("ul", { class: "pp-signin-points" }, intro.points.map((t) => el("li", {}, PP.icon("check"), t))) : null,
      el("div", { class: "pp-signin-actions" },
        el("button", { class: "pp-btn pp-btn-primary pp-btn-lg", type: "button", onclick: () => PP.signIn({}) }, "Sign in"),
        intro.register !== false ? el("button", { class: "pp-btn pp-btn-lg", type: "button", onclick: () => PP.signIn({ prompt: "create" }) }, "Create an account") : null),
      intro.footnote ? el("p", { class: "pp-signin-foot" }, intro.footnote) : null);
    document.body.replaceChildren(el("div", { class: "pp-signin" },
      el("button", { type: "button", class: `pp-icon-btn pp-theme pp-signin-theme${o.themePosition === "left" ? " pp-theme-left" : ""}`, "data-theme-toggle": "" }),
      card));
    document.title = `Sign in — Pigeonpost ${o.title}`;
    if (window.PPTheme && window.PPTheme.refresh) window.PPTheme.refresh();
  }

  function fatal(e) {
    frame();
    const main = document.getElementById("pp-main");
    main.replaceChildren(el("div", { class: "pp-view" }, PP.ui.errorState(e, () => window.location.reload())));
  }

  /// Boot a platform. Handles the sign-in return (`?code=` on the page itself or /auth/callback),
  /// a pending step-up resume, the signed-out screen and the router.
  PP.shell.start = async function (opts) {
    state.opts = opts;
    document.addEventListener("click", (e) => { if (!e.target.closest || !e.target.closest(".pp-pop, .pp-switch-btn, .pp-user-btn")) closePops(); });
    document.addEventListener("keydown", (e) => { if (e.key === "Escape") { closePops(); setDrawer(false); } });
    const params = new URLSearchParams(window.location.search);
    if (params.get("code") || params.get("error")) {
      try {
        const back = await PP.completeSignIn();
        window.history.replaceState(null, "", back);
      } catch (e) {
        // A silent check that found no realm session is not an error: show the sign-in screen.
        if (!e.silent) PP.toast(PP.explain(e), "error");
      }
    }
    if (!PP.signedIn()) {
      // Sessions are per tab. Once per tab, ask the realm without a prompt: someone already signed
      // in on any Pigeonpost platform comes straight back signed in; otherwise the realm answers
      // login_required and the sign-in screen shows. (A signed-out tab never loops: the flag stays.)
      if (opts.silentSignIn !== false && !PP.store.get("pp_silent_checked")) {
        PP.store.set("pp_silent_checked", "1");
        await PP.signIn({ prompt: "none" });
        return;
      }
      signedOut();
      return;
    }
    frame();
    try {
      state.ctx = await opts.context();
    } catch (e) {
      if (e instanceof PP.Redirecting) return;
      fatal(e);
      return;
    }
    drawNav();
    drawUser();
    window.addEventListener("hashchange", route);
    try { await PP.runResume(); } catch (e) { PP.fail(e); }
    await route();
  };
})();
