// account.pigeonpost.dev — start the shell: who the person is, which other platforms they have,
// old links (`#credits`, `#delete-account`, iyzico's `?credits=` …) onto the new routes, and the
// pages in navigation order. Nothing partner or staff lives here; a person who is one gets the
// platform switcher instead.
(function () {
  "use strict";
  const ORDER = ["overview", "inboxes", "handles", "nests", "store", "purchases", "wallet", "plan", "billing", "invoices", "security", "privacy", "support"];
  // The org roles that make someone an Enterprise admin (site-enterprise/app.js ADMIN).
  const ORG_ADMIN = ["owner", "admin", "people_admin", "agent_admin", "billing_admin"];

  // Hashes the old pigeonpost.dev/account page, the apps, the stores and emails link to.
  const LEGACY = {
    overview: ["overview"], handles: ["handles"], "get-handle": ["handles", "get"], mailboxes: ["inboxes"], keys: ["inboxes", "keys"],
    plan: ["plan"], inbox: ["store", "inbox"], nests: ["nests"], products: ["nests"], services: ["store", "services"], credits: ["wallet"],
    billing: ["billing"], invoices: ["invoices"], security: ["security"], privacy: ["privacy"], "delete-account": ["privacy", "delete"],
  };
  // Partner and staff sections moved to their own platforms.
  const MOVED = {
    business: "https://partner.pigeonpost.dev/", publisher: "https://partner.pigeonpost.dev/", publish: "https://partner.pigeonpost.dev/",
    apps: "https://partner.pigeonpost.dev/", analytics: "https://partner.pigeonpost.dev/", staff: "https://management.pigeonpost.dev/", "ac-staff": "https://management.pigeonpost.dev/",
  };
  // A return from iyzico opens the page that finishes it (the query names are the bridge's).
  const RETURNS = [["credits", ["wallet"]], ["mp_purchase", ["store", "services"]], ["mp_checkout", ["store", "services"]], ["nest_checkout", ["nests"]], ["nest_org", ["nests"]], ["checkout", ["plan"]], ["card", ["plan"]]];

  /// Where an old link points now. Returns true when the page is leaving for another platform.
  ACC.mapLegacy = function () {
    const raw = window.location.hash.replace(/^#/, "");
    const q = new URLSearchParams(window.location.search);
    let target = null;
    if (raw && !raw.startsWith("/")) {
      if (MOVED[raw]) { PP.navigate(MOVED[raw]); return true; }
      target = LEGACY[raw] || null;
    }
    // A return parameter wins over the section an old return URL named (`?credits=…#credits`).
    if (!raw || !raw.startsWith("/")) {
      const hit = RETURNS.find(([k]) => q.has(k));
      if (hit) target = hit[1];
      // pigeonpost.dev/marketplace links a listing as `?listing=lst_…` (old catalog deep link).
      const listing = q.get("listing");
      if (!hit && /^lst_[a-z0-9]+$/.test(listing || "")) {
        target = ["store", "services", listing];
        q.delete("listing");
        window.history.replaceState(window.history.state, "", window.location.pathname + (q.toString() ? "?" + q : ""));
      }
    }
    if (target) window.history.replaceState(window.history.state, "", window.location.pathname + window.location.search + PP.href(...target));
    return false;
  };

  /// Display only: the token's claims name the person; the postbox authorizes everything.
  function person() {
    const c = PP.claims(PP.token());
    const acr = String(c.acr || "");
    return {
      user: { name: String(c.name || [c.given_name, c.family_name].filter(Boolean).join(" ") || c.preferred_username || ""), email: String(c.email || ""), username: String(c.preferred_username || "") },
      // acr "mfa" (or a level of 2 or more) means this sign-in used the authenticator code (#296).
      twoStep: acr === "mfa" || Number(acr) >= 2,
    };
  }

  /// How many authenticator-app credentials the signed-in account has, from the realm's account API
  /// (CORS admits this origin). Null when it cannot be read.
  async function authenticatorCount() {
    const issuer = (window.PP_CONFIG.oidc || {}).issuer;
    if (!issuer || !PP.token()) return null;
    const res = await window.fetch(`${issuer}/account/credentials`, { headers: { authorization: `Bearer ${PP.token()}`, accept: "application/json" } });
    if (!res.ok) return null;
    const list = await res.json();
    if (!Array.isArray(list)) return null;
    const otp = list.find((c) => c && c.type === "otp");
    return otp ? (otp.userCredentialMetadatas || []).length : 0;
  }

  ACC.context = async function () {
    if (ACC.mapLegacy()) throw new PP.Redirecting("moved");
    const notice = PP.store.get("pp_checkout_notice");
    if (notice) { PP.store.del("pp_checkout_notice"); PP.toast(notice, "info"); }
    if (PP.durable.get("pp_postaction") === "totp") { PP.durable.del("pp_postaction"); PP.toast("Two-factor authentication is set up.", "success"); }
    const probe = (path) => PP.postbox(path, { keepSession: true });
    const [caps, pubs, orgs, staff, creds] = await Promise.allSettled([probe("/v1/capabilities"), probe("/v1/publishers"), probe("/v1/orgs"), probe("/v1/staff/me"), authenticatorCount()]);
    const ok = (r) => (r.status === "fulfilled" ? r.value : null);
    const features = new Set(((ok(caps) || {}).features || []).map(String));
    const platforms = [];
    const orgList = (ok(orgs) || {}).orgs || [];
    if (orgList.some((o) => (o.my_roles || []).some((r) => ORG_ADMIN.includes(r)))) platforms.push("enterprise");
    // A publisher row exists from the first application on, so this covers applicants too.
    if (((ok(pubs) || {}).data || []).length) platforms.push("partner");
    const roles = ((ok(staff) || {}).data || {}).roles;
    if (Array.isArray(roles) && roles.length) platforms.push("management");
    const me = person();
    // Two-step sign-in is on when the account has an authenticator registered (the realm's account
    // API), whatever this particular sign-in did; the token's acr stays the fallback.
    const otp = ok(creds);
    if (typeof otp === "number") me.twoStep = otp > 0 || me.twoStep;
    return { ...me, features, platforms, orgs: orgList };
  };

  ACC.pagesInOrder = () => ORDER.map((k) => ACC.registry[k]).filter(Boolean);

  ACC.start = function () {
    const deleting = /^#(delete-account|\/privacy\/delete)$/.test(window.location.hash);
    return PP.shell.start({
      platform: "account",
      title: "Account",
      intro: deleting
        ? { heading: "Delete your Pigeonpost account", text: "Sign in to confirm which account you want deleted. You do not need to contact support to ask for this.", register: false }
        : { heading: "Your Pigeonpost account", text: "Your inboxes, handles, Nests and purchases in one place.", points: ["Hosted inboxes for your AI agents", "Handles from every store, with renewals", "Nests, services and credits", "Plan, billing details and invoices"] },
      context: ACC.context,
      pages: ACC.pagesInOrder(),
      userLinks: [{ label: "Become a partner", href: "https://partner.pigeonpost.dev/", icon: "building" }],
      footLinks: [
        { label: "Developer docs", href: "https://developers.pigeonpost.dev", external: true, icon: "docs" },
        { label: "Privacy policy", href: "https://pigeonpost.dev/privacy", external: true, icon: "privacy" },
        { label: "Terms", href: "https://pigeonpost.dev/terms", external: true, icon: "form" },
      ],
    });
  };

  if (!window.ACC_NO_AUTOSTART) {
    if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", () => ACC.start());
    else ACC.start();
  }
})();
