// management.pigeonpost.dev — boots the shared shell with the staff pages.
//
// Who the caller is comes from GET /v1/staff/me (roles, platform owner, step-up policy). A caller
// without a staff role (403 staff_required, or 404 while staff routes are off) sees the "Staff
// only" screen and nothing else. The platform switcher lists account. always, and partner. and
// enterprise. when a best-effort probe says the person has them.
(function () {
  "use strict";
  const PP = window.PP, M = window.PPM;

  const GROUPS = ["", "People", "Enterprises", "Partners", "Marketplace", "Infrastructure", "Money", "Support", "Trust & safety", "System"];
  const pages = M.pages.slice().sort((a, b) => GROUPS.indexOf(a.group || "") - GROUPS.indexOf(b.group || ""));

  const STAFF_ONLY = {
    title: "Staff only",
    text: "management.pigeonpost.dev is for Pigeonpost staff. This account has no staff role. If you should have one, ask the platform owner.",
    action: { label: "Go to your account", href: "https://account.pigeonpost.dev/", icon: "external" },
  };

  const opts = {
    platform: "management",
    title: "Management",
    intro: {
      heading: "Pigeonpost management",
      text: "For Pigeonpost staff only. Sign in with your staff account; staff tools ask for a recent two-step sign-in.",
      points: ["People, enterprises and partners", "Applications, reviews and support", "Credits, audit and roles"],
      register: false,
      footnote: "Not staff? Your account lives at account.pigeonpost.dev.",
    },
    noAccess: STAFF_ONLY,
    footLinks: [
      { label: "Your account", href: "https://account.pigeonpost.dev/", icon: "external", external: true },
      { label: "Status", href: "https://status.pigeonpost.dev/", icon: "server", external: true },
    ],
    pages,
    context,
  };

  /// Settles within `ms`, as `fallback` if the probe is slow or fails.
  const probe = (p, ms, fallback) => Promise.race([p.catch(() => fallback), new Promise((r) => setTimeout(() => r(fallback), ms))]);

  async function context() {
    let me;
    try {
      me = (await M.get("/v1/staff/me")).data;
    } catch (e) {
      if (e instanceof PP.Redirecting) throw e;
      if (M.isStepUp(e)) {
        opts.noAccess = { title: "Confirm it's you", text: "Staff tools need a recent two-step sign-in. You'll come straight back here.", action: { label: "Confirm it's you", icon: "key", onclick: () => M.confirm(e) } };
        return { roles: [], platforms: ["account"] };
      }
      if (e.status === 403 || e.status === 404) { opts.noAccess = STAFF_ONLY; return { roles: [], platforms: ["account"] }; }
      throw e;
    }
    if (!me || !Array.isArray(me.roles) || !me.roles.length) { opts.noAccess = STAFF_ONLY; return { roles: [], platforms: ["account"] }; }
    const [pubs, orgs, dash] = await Promise.all([
      probe(PP.postbox("/v1/publishers", { stepUp: "throw" }), 2500, null),
      probe(PP.postbox("/v1/orgs", { stepUp: "throw" }), 2500, null),
      probe(M.get("/v1/staff/dashboard"), 2500, null),
    ]);
    const platforms = ["account", "management"];
    if (pubs && Array.isArray(pubs.data) && pubs.data.length) platforms.push("partner");
    if (orgs && Array.isArray(orgs.orgs) && orgs.orgs.length) platforms.push("enterprise");
    return { me, roles: me.roles, owner: Boolean(me.platform_owner), platforms, counts: (dash && dash.data && dash.data.counts) || {} };
  }

  M.opts = opts;
  if (!(window.PP_CONFIG || {}).manualStart) {
    const go = () => PP.shell.start(opts);
    if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", go); else go();
  }
})();
