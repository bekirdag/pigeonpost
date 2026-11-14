// Pigeonpost for teams — the enterprise layer over the shared platform core (/shared/core.js, from
// site-shared/). Sign-in, step-up, API calls, sheets and toasts are the shared ones, so a session
// started on any page of this site (start, invite, checkout, the callback) is the one /app uses.
//
// This file only keeps what the public pages need on top: the `PPE` name they were written
// against (the same object as `PP`), small icons for pages that load no shell, and the header's
// account menu (`#ppe-account`). Load order: config.js, /shared/core.js, then this file.
(function () {
  "use strict";
  const PP = window.PP;
  if (!PP || !PP.el) throw new Error("load /shared/core.js before /core.js");
  // A script that ran earlier may have hung its module on a provisional PPE object; keep it.
  if (window.PPE && window.PPE !== PP) Object.assign(PP, window.PPE);
  const PPE = (window.PPE = PP);
  const cfg = window.PPE_CONFIG || {};

  // Small stroke icons for pages without the shell (the shell's PP.icon has the full set).
  const ICONS = {
    inbox: ["M4 13h4l1.5 3h5l1.5-3h4", "M5.5 5h13L21 13v5a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-5Z"],
    out: ["M15 4h3a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2h-3M10 16l-4-4 4-4M6 12h10"],
    building: ["M4 21V5a1 1 0 0 1 1-1h9a1 1 0 0 1 1 1v16M15 9h4a1 1 0 0 1 1 1v11M3 21h18", "M8 8h3M8 12h3M8 16h3"],
  };
  if (!PPE.icon) {
    PPE.icon = function (name) {
      const NS = "http://www.w3.org/2000/svg";
      const svg = document.createElementNS(NS, "svg");
      svg.setAttribute("class", "ico");
      svg.setAttribute("viewBox", "0 0 24 24");
      svg.setAttribute("aria-hidden", "true");
      svg.setAttribute("focusable", "false");
      for (const d of ICONS[name] || []) {
        const p = document.createElementNS(NS, "path");
        p.setAttribute("d", d);
        svg.append(p);
      }
      return svg;
    };
  }

  /// Header sign-in state, shared by every public page that has `#ppe-account`. Signed in, it is
  /// the account menu: the person's real name, the admin app, the inbox and Sign out.
  let menuWired = false;
  PPE.renderAccount = function () {
    const slot = document.getElementById("ppe-account");
    if (!slot) return;
    slot.replaceChildren();
    if (PPE.signedIn()) {
      const who = PPE.whoami();
      const shown = who.name || who.username || "Signed in";
      const initial = (shown.trim().charAt(0) || "?").toUpperCase();
      const menu = PPE.el("div", { class: "user-menu", id: "ppe-user-menu", hidden: true },
        PPE.el("div", { class: "user-id", translate: "no" }, PPE.el("strong", {}, shown), who.username && who.username !== shown ? PPE.el("span", {}, who.username) : null),
        PPE.el("a", { class: "menu-item", href: "/app" }, PPE.icon("building"), "Manage your organization"),
        PPE.el("a", { class: "menu-item", href: cfg.inbox || "https://inbox.pigeonpost.dev" }, PPE.icon("inbox"), "Open your inbox"),
        PPE.el("button", { class: "menu-item", type: "button", onclick: () => PPE.signOut() }, PPE.icon("out"), "Sign out"));
      const button = PPE.el("button", { class: "user-btn", id: "ppe-user-btn", type: "button", "aria-haspopup": "true", "aria-expanded": "false", "aria-controls": "ppe-user-menu" },
        PPE.el("span", { class: "avatar", "aria-hidden": "true", translate: "no" }, initial),
        PPE.el("span", { class: "who", translate: "no" }, shown));
      button.addEventListener("click", (e) => { e.stopPropagation(); setMenu(menu.hidden); });
      slot.append(PPE.el("div", { class: "user" }, button, menu));
      if (!menuWired) {
        menuWired = true;
        document.addEventListener("click", (e) => {
          const m = document.getElementById("ppe-user-menu");
          if (m && !m.hidden && !m.contains(e.target)) setMenu(false);
        });
        document.addEventListener("keydown", (e) => {
          const m = document.getElementById("ppe-user-menu");
          if (e.key === "Escape" && m && !m.hidden) { setMenu(false); const b = document.getElementById("ppe-user-btn"); if (b) b.focus(); }
        });
      }
    } else {
      slot.append(PPE.el("button", { class: "btn btn-quiet", type: "button", onclick: () => PPE.signIn({ returnTo: "/app" }) }, "Sign in"));
    }
  };
  function setMenu(open) {
    const m = document.getElementById("ppe-user-menu");
    const b = document.getElementById("ppe-user-btn");
    if (!m || !b) return;
    m.hidden = !open;
    b.setAttribute("aria-expanded", String(open));
  }
})();
