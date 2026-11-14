// Pigeonpost account — the in-page member area.
//
// Auth is Authorization Code + PKCE against the pigeonpost-prod realm; the adapter at /api exchanges
// the code and proxies member operations to saas_be with the customer's token. iyzico hosts the card
// form on the separate /billing/checkout page, so no payment script or key runs here.

(function () {
  "use strict";
  const cfg = window.PIGEONPOST_ACCOUNT || {};
  const reserved = window.PIGEONPOST_RESERVED;
  const api = (p) => `${cfg.apiBase}${p}`;
  const $ = (s, r) => (r || document).querySelector(s);
  const authReady = Boolean(cfg.oidc && cfg.oidc.clientId);

  // What loadOverview last learned about the signed-in member. Null until the first load, so the
  // purchase gate knows to fetch it before deciding whether billing/card are missing.
  let overview = null;

  // localStorage, not sessionStorage: the PKCE verifier must survive the full-page
  // redirect out to Keycloak and back. sessionStorage is meant to persist across that, but in
  // practice some browsers drop it across a cross-site OAuth round-trip, which loses the verifier
  // and makes every exchange fail invalid_grant — a silent sign-in loop. localStorage is durable.
  const SS = window.localStorage;
  const TS = window.sessionStorage;

  // Every signed-in tab needs to renew its short-lived access token while a form is open.
  // Keep access and refresh tokens together: a remembered sign-in in another tab must not pair
  // its access token with this tab's temporary refresh token. Only remembered sessions persist.
  const sessionStore = () => TS.getItem("pp_session") ? TS : SS;
  const getToken = () => sessionStore().getItem("pp_session");
  const wantsRemember = () => SS.getItem("pp_remember") === "1";
  function clearSession() {
    for (const storage of [TS, SS]) {
      storage.removeItem("pp_session");
      storage.removeItem("pp_refresh");
    }
  }
  function storeSession(storage, body) {
    storage.setItem("pp_session", body.session);
    if (body.refresh) storage.setItem("pp_refresh", body.refresh);
  }
  function signOut() {
    clearSession();
    SS.removeItem("pp_remember");
    TS.removeItem("pp_cloud_checkout");
  }

  // ---- PKCE ---------------------------------------------------------------------------------

  function randomString(len) {
    const a = new Uint8Array(len);
    crypto.getRandomValues(a);
    return Array.from(a, (b) => ("0" + (b & 0xff).toString(16)).slice(-2)).join("");
  }
  async function sha256b64url(input) {
    const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(input));
    let str = "";
    new Uint8Array(digest).forEach((b) => (str += String.fromCharCode(b)));
    return btoa(str).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  }

  // setupTotp reuses the login redirect but adds kc_action=CONFIGURE_TOTP. For an already-signed-in
  // user Keycloak skips the password prompt and shows only the "scan this QR" authenticator-setup
  // page, then returns here — a single focused screen instead of the whole account console.
  function setupTotp() {
    SS.setItem("pp_postaction", "totp");
    login(undefined, "CONFIGURE_TOTP");
  }

  async function login(remember, kcAction, stepUp) {
    if (!authReady) {
      toast("Sign-in isn't configured yet. The pigeonpost-prod realm web client is being set up.");
      return;
    }
    // Default to remembering unless the caller explicitly opted out, so the pending-purchase resume
    // path (which calls login() with no arg) keeps whatever the sign-in card already recorded.
    const keep = remember === undefined ? wantsRemember() : !!remember;
    if (window.location.hash === "#delete-account") SS.setItem("pp_deletion_return", "1");
    // The OIDC redirect URI carries no fragment, so the section the member was heading for
    // (`/account#credits` from the postbox's buy_url, say) is parked here and restored on return.
    const returnRoute = hashRoute();
    if (returnRoute) SS.setItem("pp_return_view", returnRoute); else SS.removeItem("pp_return_view");
    SS.setItem("pp_remember", keep ? "1" : "0");
    const verifier = randomString(48);
    const state = randomString(12);
    SS.setItem("pp_pkce", verifier);
    SS.setItem("pp_state", state);
    const challenge = await sha256b64url(verifier);
    const redirect = window.location.origin + (cfg.oidc.redirectPath || "/account");
    // offline_access asks Keycloak for a long-lived refresh token (the 30-day offline session). It
    // is only requested when the user chose "remember me"; otherwise the session is tab-lifetime.
    let scope = cfg.oidc.scope || "openid email";
    if (keep && !/\boffline_access\b/.test(scope)) scope += " offline_access";
    const url = `${cfg.oidc.issuer}/protocol/openid-connect/auth`
      + `?client_id=${encodeURIComponent(cfg.oidc.clientId)}`
      + `&response_type=code&scope=${encodeURIComponent(scope)}`
      + `&redirect_uri=${encodeURIComponent(redirect)}`
      + `&code_challenge=${challenge}&code_challenge_method=S256&state=${state}`
      + (kcAction ? `&kc_action=${encodeURIComponent(kcAction)}` : "")
      // Step-up (RFC 9470): ask the realm for exactly the strength and freshness the postbox named.
      + (stepUp && stepUp.acr_values ? `&acr_values=${encodeURIComponent(stepUp.acr_values)}` : "")
      + (stepUp && stepUp.max_age != null ? `&max_age=${encodeURIComponent(stepUp.max_age)}` : "");
    window.location.href = url;
  }

  function register() {
    // Keycloak's registration page, same client. Falls back to login if registrations is disabled.
    if (!authReady) { login(); return; }
    SS.setItem("pp_register", "1");
    login();
  }

  async function completeLoginIfReturning() {
    const params = new URLSearchParams(window.location.search);
    const code = params.get("code");
    const state = params.get("state");
    const strip = () => {
      const deletion = SS.getItem("pp_deletion_return") === "1";
      const back = SS.getItem("pp_return_view");
      SS.removeItem("pp_deletion_return");
      SS.removeItem("pp_return_view");
      const hash = deletion ? "#delete-account" : back && ROUTES[back] ? "#" + back : "";
      history.replaceState({}, "", (cfg.oidc.redirectPath || "/account") + hash);
    };
    if (params.get("error")) {
      toast("Sign-in did not complete: " + params.get("error"));
      strip();
      return false;
    }
    if (!code) return false;
    if (state && SS.getItem("pp_state") && state !== SS.getItem("pp_state")) {
      toast("Sign-in could not be verified. Please try again.");
      SS.removeItem("pp_pkce"); SS.removeItem("pp_state"); strip();
      return false;
    }
    const verifier = SS.getItem("pp_pkce") || "";
    const redirect = window.location.origin + (cfg.oidc.redirectPath || "/account");
    try {
      const res = await fetch(api("/v1/auth/exchange"), {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ code, redirectUri: redirect, codeVerifier: verifier }),
      });
      const body = await res.json().catch(() => ({}));
      if (body.session) {
        clearSession();
        storeSession(wantsRemember() ? SS : TS, body);
        if (SS.getItem("pp_postaction") === "totp") toast("Two-factor authentication is now set up.");
      } else {
        // Surface the reason rather than looping silently — this is what turned a real bug into a
        // mystery on the first sign-in attempt.
        toast("Could not complete sign-in" + (body.error ? ": " + body.error : "."));
      }
    } catch (_) {
      toast("Could not reach the sign-in service. Try again in a moment.");
    }
    SS.removeItem("pp_pkce"); SS.removeItem("pp_state"); SS.removeItem("pp_postaction");
    strip();
    return Boolean(getToken());
  }

  function logout() { signOut(); render(); }

  // ---- API ----------------------------------------------------------------------------------

  // Renew the access token from a stored refresh token. Returns true on success. A single in-flight
  // refresh is shared so a burst of 401s doesn't spend the (rotating) refresh token several times.
  let refreshInFlight = null;
  async function refreshSession() {
    const storage = sessionStore();
    const refresh = storage.getItem("pp_refresh");
    if (!refresh) return false;
    if (!refreshInFlight) {
      refreshInFlight = (async () => {
        try {
          const res = await fetch(api("/v1/auth/refresh"), {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({ refresh }),
          });
          const body = await res.json().catch(() => ({}));
          // A logout or new sign-in during renewal must not restore the previous credentials.
          if (sessionStore() !== storage || storage.getItem("pp_refresh") !== refresh) return false;
          if (res.ok && body.session) {
            storeSession(storage, body);
            return true;
          }
          storage.removeItem("pp_refresh"); // refresh token is dead/expired — stop trying it
          return false;
        } catch (_) {
          return false; // network blip: keep the refresh token, let the caller surface the error
        } finally {
          refreshInFlight = null;
        }
      })();
    }
    return refreshInFlight;
  }

  // One authenticated request. A 401 triggers at most one refresh-and-retry. A 401 that survives
  // the retry means the session is genuinely gone (no/dead refresh token) → sign out. A non-401
  // error is returned to the caller to surface — it must NOT masquerade as an expired session, which
  // is exactly what turned a failed checkout into a phantom sign-out loop.
  async function apiFetch(path, init, retried) {
    const opts = init || {};
    const res = await fetch(api(path), {
      method: opts.method || "GET",
      headers: Object.assign(
        { authorization: `Bearer ${getToken()}`, accept: "application/json" },
        opts.body ? { "content-type": "application/json" } : {},
      ),
      body: opts.body,
    });
    if (res.status === 401 && !retried && (await refreshSession())) {
      return apiFetch(path, init, true);
    }
    const body = await res.json().catch(() => ({}));
    if (!res.ok) {
      // A 401 on a background/overview call means the session is genuinely gone → sign out. But a
      // 401 on an explicit action the user just took (checkout) is ambiguous — it may be the payment
      // backend rejecting the token for that operation, not a dead session. Signing out there is the
      // bug that produced the phantom "sign in again" loop, so opts.keepSession surfaces the real
      // reason and leaves the user logged in.
      if (res.status === 401 && !opts.keepSession) { signOut(); render(); throw new Error("session expired"); }
      const err = new Error(body.error || `Request failed (${res.status})`);
      err.status = res.status;
      err.body = body;
      throw err;
    }
    return body;
  }
  // Two helpers, deliberately. apiGet is for background reads (a 401 there is a genuinely dead
  // session → sign out). apiAction is for anything the signed-in user just clicked (checkout, add
  // card, save billing, cancel): a 401 there is ambiguous and must NOT sign them out — it surfaces
  // the real reason instead. There is intentionally no plain "apiPost" that signs out on 401; that
  // was the phantom "please sign in again" bounce.
  const apiGet = (path) => apiFetch(path, null, false);
  const apiAction = (path, body, method = "POST") => apiFetch(path, { method, body: JSON.stringify(body || {}), keepSession: true }, false);

  // ---- handle validation --------------------------------------------------------------------

  function validate(nameRaw) {
    const name = String(nameRaw || "").trim().toLowerCase();
    if (!name) return { ok: false, silent: true };
    if (/[^a-z0-9]/.test(name)) return { ok: false, reason: "letters a–z and digits 0–9 only" };
    if (name.length < (cfg.handle.min || 3)) return { ok: false, reason: `at least ${cfg.handle.min || 3} characters` };
    if (name.length > (cfg.handle.max || 32)) return { ok: false, reason: `at most ${cfg.handle.max || 32} characters` };
    const r = reserved && reserved.reason(name);
    if (r) return { ok: false, reason: r };
    return { ok: true, name };
  }

  // ---- sections and routing -----------------------------------------------------------------
  //
  // The signed-in page is an app shell: a top bar, a grouped sidebar, and one section at a time.
  // Each section is a hash (`#credits`, `#handles`, …), so a reload, a shared link and Back/Forward
  // all land on the same place. Every card keeps the element IDs its code reads; a card that is not
  // on screen is only hidden, never unmounted, so its loaders and step-up flows run as before.
  //
  // `view` is the panel a route shows; several routes can share one (the Products card's tabs).
  const ROUTES = {
    overview:     { view: "overview",   label: "Overview" },
    handles:      { view: "handles",    label: "Your handles",     group: "Handles & inboxes" },
    "get-handle": { view: "get-handle", label: "Get a handle",     group: "Handles & inboxes" },
    mailboxes:    { view: "mailboxes",  label: "AI agents",        group: "Handles & inboxes" },
    keys:         { view: "keys",       label: "Connector keys",   group: "Handles & inboxes" },
    plan:         { view: "plan",       label: "Cloud plan",       group: "Plans & products" },
    inbox:        { view: "products",   label: "Buy an Inbox",     group: "Plans & products", tab: "inbox",
                    lede: "A Pigeonpost name you own, with its mail storage." },
    nests:        { view: "products",   label: "Nests",            group: "Plans & products", tab: "nests", primary: true,
                    lede: "Private workspaces where your agents and installed services run." },
    credits:      { view: "credits",    label: "Credits",          group: "Billing" },
    billing:      { view: "billing",    label: "Billing details",  group: "Billing" },
    invoices:     { view: "invoices",   label: "Invoices",         group: "Billing" },
    services:     { view: "products",   label: "Services",         group: "Marketplace", tab: "services",
                    lede: "Apps and services from verified publishers, reviewed by Pigeonpost." },
    analytics:    { view: "analytics", label: "App analytics", group: "Marketplace", needs: "#ac-analytics" },
    publish:      { view: "publish",    label: "Publish",          group: "Marketplace", needs: "#ac-publisher" },
    security:     { view: "security",   label: "Two-factor",       group: "Security & privacy" },
    privacy:      { view: "privacy",    label: "Account deletion", group: "Security & privacy" },
  };
  // Hashes other sites and apps already link to. `#products` is the Linux and web apps' "buy
  // Nests" link and `#nests` the Windows app's (1.0.x); both open the Nests tab, as they always did.
  const ROUTE_ALIASES = { products: "nests", publisher: "publish" };
  // A step-up sign-in from a marketplace card comes back to that card (`pp_mp_return`).
  const MP_RETURN_ROUTE = { publisher: "publish", services: "services", analytics: "analytics" };
  // Staff tools moved to the back office (/staff); old links to them land there.
  const STAFF_HASHES = ["#staff", "#ac-staff"];

  function routeName(raw) {
    const name = String(raw || "").replace(/^#/, "");
    const canonical = ROUTE_ALIASES[name] || name;
    return Object.prototype.hasOwnProperty.call(ROUTES, canonical) ? canonical : null;
  }
  const hashRoute = () => routeName(window.location.hash);

  // Where a fresh load opens: the hash if it names a section, else whatever a return URL is
  // finishing (iyzico's `?credits=` and `?checkout=`, a marketplace or Nest checkout, a step-up).
  function initialRoute() {
    const fromHash = hashRoute();
    if (fromHash) return fromHash;
    const q = new URLSearchParams(window.location.search);
    if (q.has("credits")) return "credits";
    if (q.has("mp_purchase") || q.has("mp_checkout")) return "services";
    if (q.has("nest_org") || q.has("nest_checkout")) return "nests";
    if (q.has("checkout") || q.has("card")) return "plan";
    const back = MP_RETURN_ROUTE[SS.getItem("pp_mp_return") || ""];
    return back || "overview";
  }

  let mode = null;          // "signed-out" | "member" | "deletion": what render() last drew
  let currentRoute = null;

  function setUrlHash(route, replace) {
    const hash = "#" + route;
    if (window.location.hash === hash) return;
    if (replace) history.replaceState(history.state, "", window.location.pathname + window.location.search + hash);
    else window.location.hash = hash;
  }

  // Show one section. `focus` is for a navigation the member made, which moves the reading
  // position to the new section's heading; a programmatic jump (focusStep) does its own focusing.
  function showRoute(name, opts) {
    const o = opts || {};
    const route = routeName(name) || "overview";
    const spec = ROUTES[route];
    const app = $("#ac-app"); if (!app) return;
    const changed = route !== currentRoute;
    currentRoute = route;
    app.querySelectorAll(".ac-view").forEach((v) => { v.hidden = v.getAttribute("data-view") !== spec.view; });
    app.querySelectorAll("[data-route]").forEach((a) => {
      if (a.getAttribute("data-route") === route) a.setAttribute("aria-current", "page");
      else a.removeAttribute("aria-current");
    });
    const title = $("#ac-top-title"); if (title) title.textContent = spec.label;
    syncBillingGate();
    const head = $(`#ac-h-${spec.view}`);
    if (head && spec.tab) head.textContent = spec.label;
    // The products view is shared by three sidebar items: its subtitle follows the item too.
    const lede = $(`#ac-l-${spec.view}`);
    if (lede && spec.tab) { lede.textContent = spec.lede || ""; lede.hidden = !spec.lede; }
    document.title = `${spec.label} · Your Pigeonpost account`;
    if (spec.tab) selectProductTab(spec.tab);
    closeDrawer();
    if (changed && o.focus) {
      if (window.scrollTo) try { window.scrollTo(0, 0); } catch (_) { /* jsdom */ }
      if (head) try { head.focus({ preventScroll: true }); } catch (_) { head.focus(); }
    }
  }

  // Jump to a section from code (a purchase walking the member to billing, say), adding a history
  // entry so Back returns to where they were.
  function go(route, replace) {
    const r = routeName(route); if (!r) return;
    showRoute(r);
    setUrlHash(r, replace);
  }

  // The Products card keeps its own tab state; the sidebar picks the tab by pressing it.
  function selectProductTab(tab) {
    const button = $(`#pp-tab-${tab}`);
    if (button && button.getAttribute("aria-selected") !== "true") button.click();
  }

  // The route that shows a given view: the current one if it already does, else its primary.
  function routeForView(view) {
    if (currentRoute && ROUTES[currentRoute].view === view) return currentRoute;
    const all = Object.keys(ROUTES).filter((r) => ROUTES[r].view === view);
    return all.find((r) => ROUTES[r].primary) || all[0] || null;
  }

  function openDrawer() {
    const app = $("#ac-app"); if (!app) return;
    app.classList.add("ac-nav-open");
    const t = $("#ac-nav-toggle"); if (t) t.setAttribute("aria-expanded", "true");
    const scrim = $("#ac-scrim"); if (scrim) scrim.hidden = false;
    const first = $("#ac-side [aria-current]") || $("#ac-side a");
    if (first) try { first.focus({ preventScroll: true }); } catch (_) { first.focus(); }
  }
  function closeDrawer() {
    const app = $("#ac-app"); if (!app || !app.classList.contains("ac-nav-open")) return;
    app.classList.remove("ac-nav-open");
    const t = $("#ac-nav-toggle"); if (t) t.setAttribute("aria-expanded", "false");
    const scrim = $("#ac-scrim"); if (scrim) scrim.hidden = true;
  }

  // Display only: the signed-in name and address from the access token's claims. The postbox and
  // the bridge verify the token; nothing here trusts these values for anything but a label.
  function identity() {
    try {
      const part = String(getToken() || "").split(".")[1];
      const b64 = part.replace(/-/g, "+").replace(/_/g, "/");
      const claims = JSON.parse(new TextDecoder().decode(Uint8Array.from(atob(b64), (c) => c.charCodeAt(0))));
      // acr "mfa" (or a level of 2 or more) means this sign-in used the authenticator code.
      const acr = String(claims.acr || "");
      const twoStep = acr === "mfa" || Number(acr) >= 2;
      return { name: String(claims.name || claims.given_name || claims.preferred_username || ""), address: String(claims.email || ""), twoStep };
    } catch (_) {
      return { name: "", address: "", twoStep: false };
    }
  }

  // Small stroke icons, drawn inline so the page fetches nothing extra.
  const ICON_PATHS = {
    overview: '<rect x="3" y="3" width="7" height="7" rx="1.5"/><rect x="14" y="3" width="7" height="7" rx="1.5"/><rect x="3" y="14" width="7" height="7" rx="1.5"/><rect x="14" y="14" width="7" height="7" rx="1.5"/>',
    handles: '<path d="M15 4 9 20"/><circle cx="18.5" cy="17.5" r="2.5"/>',
    "get-handle": '<circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/>',
    mailboxes: '<rect x="4" y="7" width="16" height="12" rx="3"/><path d="M12 3v4M9 12h.01M15 12h.01M9.5 15.5h5"/>',
    keys: '<circle cx="8" cy="15" r="4"/><path d="m10.8 12.2 8.7-8.7M17 6l2.5 2.5M14.5 8.5 16.5 10.5"/>',
    plan: '<path d="M7 18a4.5 4.5 0 0 1-.6-8.96A6 6 0 0 1 18 10a4 4 0 0 1-.5 7.97Z"/>',
    inbox: '<path d="M4 13h4l1.5 3h5l1.5-3h4"/><path d="M5.5 5h13L21 13v5a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-5Z"/>',
    nests: '<path d="M12 3 20 7.5v9L12 21l-8-4.5v-9Z"/><path d="m4 7.5 8 4.5 8-4.5M12 12v9"/>',
    credits: '<ellipse cx="12" cy="6.5" rx="7" ry="3"/><path d="M5 6.5v5c0 1.66 3.13 3 7 3s7-1.34 7-3v-5M5 11.5v5c0 1.66 3.13 3 7 3s7-1.34 7-3v-5"/>',
    billing: '<path d="M6 3h9l4 4v14H6Z"/><path d="M14 3v5h5M9 13h7M9 17h5"/>',
    invoices: '<path d="M6 3h12v18l-3-2-3 2-3-2-3 2Z"/><path d="M9 8h6M9 12h6"/>',
    services: '<path d="M4 9h16l-1.2-4.2A1.5 1.5 0 0 0 17.4 4H6.6a1.5 1.5 0 0 0-1.4.8Z"/><path d="M5 9v10a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1V9M10 20v-5h4v5"/>',
    publish: '<path d="M12 15V3M7 8l5-5 5 5"/><path d="M4 15v4a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-4"/>',
    security: '<path d="M12 3 5 6v5c0 4.5 3 8.3 7 10 4-1.7 7-5.5 7-10V6Z"/><path d="m9 12 2 2 4-4"/>',
    privacy: '<path d="M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13"/>',
    staff: '<circle cx="12" cy="8" r="4"/><path d="M5 21a7 7 0 0 1 14 0"/>',
    menu: '<path d="M4 7h16M4 12h16M4 17h16"/>',
    close: '<path d="M6 6l12 12M18 6 6 18"/>',
    chevron: '<path d="m9 6 6 6-6 6"/>',
    out: '<path d="M15 4h3a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2h-3M10 16l-4-4 4-4M6 12h10"/>',
  };
  const icon = (name) => `<svg class="ac-ico" viewBox="0 0 24 24" aria-hidden="true" focusable="false">${ICON_PATHS[name] || ""}</svg>`;

  function navHtml() {
    const groups = [];
    for (const [route, spec] of Object.entries(ROUTES)) {
      const key = spec.group || "";
      let g = groups.find((x) => x.key === key);
      if (!g) { g = { key, items: [] }; groups.push(g); }
      g.items.push(`<li><a class="ac-nav-link" href="#${route}" data-route="${route}">${icon(route)}<span>${esc(spec.label)}</span></a></li>`);
    }
    return groups.map((g) => `
      <div class="ac-nav-group">
        ${g.key ? `<p class="ac-nav-label" id="ac-nav-${esc(g.key.replace(/[^a-z]+/gi, "-").toLowerCase())}">${esc(g.key)}</p>` : ""}
        <ul${g.key ? ` aria-labelledby="ac-nav-${esc(g.key.replace(/[^a-z]+/gi, "-").toLowerCase())}"` : ""}>${g.items.join("")}</ul>
      </div>`).join("") + `
      <div class="ac-nav-group" id="ac-nav-staff" hidden>
        <p class="ac-nav-label" id="ac-nav-staff-l">Staff</p>
        <ul aria-labelledby="ac-nav-staff-l"><li><a class="ac-nav-link" href="/staff" id="ac-backoffice">${icon("staff")}<span>Back office</span></a></li></ul>
      </div>`;
  }

  // One section's heading. `solo` sections hold a single card whose own heading repeats this one;
  // the stylesheet keeps that card heading for screen readers and hides it from view.
  function viewHead(view, title, lede) {
    return `<div class="ac-view-head"><h1 id="ac-h-${view}" tabindex="-1">${esc(title)}</h1>${lede ? `<p id="ac-l-${view}">${lede}</p>` : `<p id="ac-l-${view}" hidden></p>`}</div>`;
  }

  // ---- render -------------------------------------------------------------------------------

  const root = () => $("#account-root");

  function render() {
    const el = root();
    if (!el) return;
    const shell = (on) => { if (document.body) document.body.classList.toggle("ac-shell", on); };
    if (STAFF_HASHES.includes(window.location.hash)) { window.location.replace("/staff"); return; }
    if (window.location.hash === "#delete-account") { mode = "deletion"; shell(false); return renderDeletion(el); }
    if (!getToken()) { mode = "signed-out"; shell(false); return renderSignedOut(el); }
    mode = "member"; shell(true);
    renderMember(el);
    if (window.PPTheme) window.PPTheme.refresh();
  }

  function renderSignedOut(el) {
    el.innerHTML = `
      <div class="ac-card ac-center ac-signin">
        <h2>Sign in to Pigeonpost</h2>
        <p class="muted">Claim and manage handles, and your yearly subscription.</p>
        <div class="ac-actions">
          <button class="btn btn-primary" id="ac-signin">Sign in</button>
          <button class="btn btn-secondary" id="ac-register">Create an account</button>
        </div>
        <label class="ac-remember"><input type="checkbox" id="ac-remember" checked> Keep me signed in for 30 days</label>
        ${authReady ? "" : `<p class="ac-note">Sign-in is being finalized. Handle search below works now.</p>`}
      </div>
      ${searchBlock()}`;
    const remember = () => !!($("#ac-remember") && $("#ac-remember").checked);
    $("#ac-signin").onclick = () => login(remember());
    $("#ac-register").onclick = () => { SS.setItem("pp_remember", remember() ? "1" : "0"); register(); };
    wireSearch(false);
  }

  function renderMember(el) {
    const who = identity();
    const shown = who.name || who.address || "Your account";
    const initial = (who.name || who.address || "?").trim().charAt(0).toUpperCase() || "?";
    el.innerHTML = `
      <div class="ac-app" id="ac-app">
        <header class="ac-top">
          <button type="button" class="ac-icon-btn ac-nav-toggle" id="ac-nav-toggle" aria-controls="ac-side" aria-expanded="false" aria-label="Open navigation">${icon("menu")}</button>
          <a class="ac-brand" href="/" aria-label="Pigeonpost home"><picture class="brand-logo ac-logo">
            <source srcset="/logo_white.png" media="(prefers-color-scheme: dark)">
            <img src="/logo.png" alt="" width="640" height="213">
          </picture></a>
          <button type="button" class="theme-toggle" data-theme-toggle aria-label="Theme: System"></button>
          <span class="ac-top-sep" aria-hidden="true"></span>
          <span class="ac-top-title" id="ac-top-title" aria-hidden="true">Overview</span>
          <span class="ac-top-fill"></span>
          <a class="ac-chip" id="ac-credit-chip" href="#credits" hidden title="Credit balance">${icon("credits")}<span id="ac-credit-chip-v"></span></a>
          <div class="ac-user">
            <button type="button" class="ac-user-btn" id="ac-user-btn" aria-haspopup="true" aria-expanded="false" aria-controls="ac-user-menu">
              <span class="ac-avatar" aria-hidden="true">${esc(initial)}</span><span class="ac-user-name">${esc(shown)}</span>
            </button>
            <div class="ac-user-menu" id="ac-user-menu" hidden>
              <div class="ac-user-id"><strong>${esc(who.name || "Signed in")}</strong>${who.address ? `<span>${esc(who.address)}</span>` : ""}</div>
              <a class="ac-menu-item" href="#security">${icon("security")}Security</a>
              <a class="ac-menu-item" href="#billing">${icon("billing")}Billing details</a>
              <button type="button" class="ac-menu-item" id="ac-logout">${icon("out")}Sign out</button>
            </div>
          </div>
        </header>
        <nav class="ac-side" id="ac-side" aria-label="Account sections">
          <div class="ac-side-head"><span>Menu</span><button type="button" class="ac-icon-btn" id="ac-nav-close" aria-label="Close navigation">${icon("close")}</button></div>
          ${navHtml()}
          <div class="ac-side-foot">
            <a href="https://inbox.pigeonpost.dev">Open the inbox</a>
            <a href="https://developers.pigeonpost.dev">Developer docs</a>
            <a href="/privacy">Privacy</a><a href="/terms">Terms</a>
          </div>
        </nav>
        <div class="ac-scrim" id="ac-scrim" hidden></div>
        <div class="ac-content" id="ac-content">
          <div class="ac-gate" id="ac-billing-gate" role="region" aria-labelledby="ac-billing-gate-title" hidden>
            <span class="ac-gate-ico" aria-hidden="true">${icon("billing")}</span>
            <div><strong id="ac-billing-gate-title">Add billing details first</strong>
              <p id="ac-billing-gate-why">Purchases are invoiced to your billing details. Add them to buy plans, handles, Nests and credits.</p></div>
            <a class="btn btn-primary" href="#billing" id="ac-billing-gate-go">Add billing details</a>
          </div>

          <section class="ac-view" data-view="overview" aria-labelledby="ac-h-overview">
            ${viewHead("overview", "Overview", who.name ? `Signed in as ${esc(who.name)}.` : "Your plan, credit, names and agents at a glance.")}
            <div class="ac-stats">
              <a class="ac-stat" href="#plan"><span class="ac-stat-k">${icon("plan")}Cloud plan</span>
                <span class="ac-stat-v" id="ac-dash-plan">…</span><span class="ac-stat-s" id="ac-dash-plan-s"></span></a>
              <a class="ac-stat" href="#credits" id="ac-dash-credits" hidden><span class="ac-stat-k">${icon("credits")}Credit balance</span>
                <span class="ac-stat-v" id="ac-dash-credit">…</span><span class="ac-stat-s" id="ac-dash-credit-s">For marketplace services</span></a>
              <a class="ac-stat" href="#handles"><span class="ac-stat-k">${icon("handles")}Handles</span>
                <span class="ac-stat-v" id="ac-dash-handles">…</span><span class="ac-stat-s" id="ac-dash-handles-s"></span></a>
              <a class="ac-stat" href="#mailboxes"><span class="ac-stat-k">${icon("mailboxes")}Agent mailboxes</span>
                <span class="ac-stat-v" id="ac-dash-boxes">…</span><span class="ac-stat-s" id="ac-dash-boxes-s"></span></a>
            </div>
            <div class="ac-callout" id="ac-dash-billing" hidden>
              <div><strong>Add your billing details</strong><p>Plans, Nests and credits need them before checkout.</p></div>
              <a class="btn btn-small" href="#billing">Add details</a>
            </div>
            <h2 class="ac-sub">Quick actions</h2>
            <div class="ac-quick">
              <a class="ac-quick-item" href="#get-handle">${icon("get-handle")}<span><strong>Get a handle</strong><small>Search for a name like /you</small></span></a>
              <a class="ac-quick-item" href="#mailboxes">${icon("mailboxes")}<span><strong>Connect an AI agent</strong><small>A hosted inbox for Claude or ChatGPT</small></span></a>
              <a class="ac-quick-item" href="#credits" id="ac-quick-credits" hidden>${icon("credits")}<span><strong>Buy credits</strong><small>Pay for marketplace services</small></span></a>
              <a class="ac-quick-item" href="#nests">${icon("nests")}<span><strong>Buy Nests</strong><small>Private AI workspaces</small></span></a>
              <a class="ac-quick-item" href="#security">${icon("security")}<span><strong>Two-factor sign-in</strong><small>Protect this account</small></span></a>
              <a class="ac-quick-item" href="#invoices">${icon("invoices")}<span><strong>Invoices</strong><small>Download past receipts as PDF</small></span></a>
            </div>
          </section>

          <section class="ac-view ac-view-solo" data-view="handles" aria-labelledby="ac-h-handles" hidden>
            ${viewHead("handles", "Your handles", "")}
            <div class="ac-card"><h3>Your handles</h3><div class="ac-card-head"><p class="muted">Names belong to your Pigeonpost account across mobile, desktop and web.</p><button class="btn btn-small" id="ac-handles-refresh">Refresh handles</button></div><div id="ac-subs"><p class="muted">Loading…</p></div>
              <p class="ac-note"><a href="#get-handle">Get another handle</a> · <a href="#plan">Your Cloud plan</a></p></div>
          </section>

          <section class="ac-view ac-view-solo" data-view="get-handle" aria-labelledby="ac-h-get-handle" hidden>
            ${viewHead("get-handle", "Get a handle", "")}
            ${searchBlock()}
          </section>

          <section class="ac-view ac-view-solo" data-view="mailboxes" aria-labelledby="ac-h-mailboxes" hidden>
            ${viewHead("mailboxes", "AI agents", "")}
            <div class="ac-card">
              <h3>Connect an AI agent</h3>
              <p class="muted">Give a Claude or ChatGPT agent a hosted Pigeonpost inbox — it sends and
                receives messages through an MCP connector tied to this account.</p>
              <div id="ac-pb-list"><p class="muted">Loading…</p></div>
              <div class="ac-actions ac-left">
                <button class="btn btn-secondary" id="ac-pb-create">Create an inbox</button>
                <a class="btn btn-secondary" href="#keys">Connector keys</a>
              </div>
            </div>
          </section>

          <section class="ac-view ac-view-solo" data-view="keys" aria-labelledby="ac-h-keys" hidden>
            ${viewHead("keys", "Connector keys", "")}
            <div class="ac-card">
              <h3>Connector keys</h3>
              <div class="ac-card-head"><p class="muted">Each key lets an agent act as this account. Revoke one to cut off a device or agent.</p>
                <button class="btn btn-secondary" id="ac-pb-key">Create connector key</button></div>
              <div id="ac-pb-out"></div>
              <div id="ac-pb-keys"><p class="muted">Loading…</p></div>
            </div>
          </section>

          <section class="ac-view ac-view-solo" data-view="plan" aria-labelledby="ac-h-plan" hidden>
            ${viewHead("plan", "Cloud plan", "")}
            <div class="ac-card" id="ac-cloud"><h3>Pigeonpost Cloud plan</h3><div id="ac-cloud-body"><p class="muted">Loading…</p></div></div>
          </section>

          <section class="ac-view ac-view-solo ac-view-wide" data-view="products" aria-labelledby="ac-h-products" hidden>
            ${viewHead("products", "Nests", "")}
            <div class="ac-card" id="ac-products"></div>
          </section>

          <section class="ac-view ac-view-solo" data-view="credits" aria-labelledby="ac-h-credits" hidden>
            ${viewHead("credits", "Credits", "")}
            <section class="ac-card" id="credits" aria-labelledby="cr-title" hidden></section>
            <div class="ac-empty" id="ac-empty-credits"><p class="muted">Loading…</p></div>
          </section>

          <section class="ac-view ac-view-solo" data-view="billing" aria-labelledby="ac-h-billing" hidden>
            ${viewHead("billing", "Billing details", "Who we invoice. Plans, Nests and credits all use these details.")}
            <div class="ac-card"><h3>Billing details</h3><div id="ac-billing"><p class="muted">Loading…</p></div></div>
          </section>

          <section class="ac-view ac-view-solo" data-view="invoices" aria-labelledby="ac-h-invoices" hidden>
            ${viewHead("invoices", "Invoices", "Every web purchase, with a PDF to download.")}
            <div class="ac-card"><h3>Invoices</h3><div id="ac-invoices"><p class="muted">Loading…</p></div></div>
          </section>

          <section class="ac-view ac-view-solo" data-view="analytics" aria-labelledby="ac-h-analytics" hidden>
            ${viewHead("analytics", "App analytics", "")}
            <div class="ac-card" id="ac-analytics" hidden></div>
            <div class="ac-empty" id="ac-empty-analytics"><p class="muted">Loading…</p></div>
          </section>
          <section class="ac-view ac-view-solo" data-view="publish" aria-labelledby="ac-h-publish" hidden>
            ${viewHead("publish", "Publish", "")}
            <div class="ac-card" id="ac-publisher" hidden></div>
            <div class="ac-empty" id="ac-empty-publish"><p class="muted">Loading…</p></div>
          </section>

          <section class="ac-view ac-view-solo" data-view="security" aria-labelledby="ac-h-security" hidden>
            ${viewHead("security", "Two-factor", "")}
            <div class="ac-card">
              <h3>Security</h3>
              ${identity().twoStep
                ? `<p><strong>Two-factor is on.</strong> You signed in with a code from your authenticator app.</p>
              <button class="btn btn-secondary" id="ac-2fa">Add another authenticator</button>
              <p class="ac-note ac-mt">Opens a single screen with a QR code to scan, then brings you back here.</p>`
                : `<p class="muted">Add two-factor authentication with an app like Google Authenticator, Authy, or 1Password. If you already set it up, it shows as on the next time you sign in with your code.</p>
              <button class="btn btn-primary" id="ac-2fa">Set up two-factor authentication</button>
              <p class="ac-note ac-mt">Opens a single screen with a QR code to scan, then brings you back here.</p>`}
            </div>
          </section>

          <section class="ac-view ac-view-solo" data-view="privacy" aria-labelledby="ac-h-privacy" hidden>
            ${viewHead("privacy", "Account deletion", "")}
            <div class="ac-card ac-danger"><h3>Account deletion</h3>
              <p class="muted">Deletes your hosted sign-in account, mailboxes, keys, your copies of messages and attachments, and contacts. You lose your handles.</p>
              <a class="btn btn-danger" href="#delete-account">Delete your account and associated data</a></div>
          </section>

        </div>
      </div>`;
    $("#ac-logout").onclick = logout;
    $("#ac-handles-refresh").onclick = () => { loadOverview(); loadPostbox(); };
    $("#ac-2fa").onclick = setupTotp;
    $("#ac-pb-create").onclick = pbCreateInbox;
    $("#ac-pb-key").onclick = pbRevealKey;
    wireShell();
    currentRoute = null;
    const start = initialRoute();
    showRoute(start);
    if (start !== "overview" && !hashRoute()) setUrlHash(start, true);
    wireSearch(true);
    loadOverview();
    loadPostbox();
    loadKeys();
    mountProducts();
    findBackOffice();
  }

  // Staff get a sidebar link to the back office. `/v1/staff/me` answers staff with their roles
  // and everyone else with a refusal; nothing else here depends on it.
  async function findBackOffice() {
    try {
      const me = await pbFetch("/v1/staff/me");
      const link = $("#ac-nav-staff");
      if (link && me && me.data && Array.isArray(me.data.roles) && me.data.roles.length) link.hidden = false;
    } catch (_) { /* not staff, or an older postbox */ }
  }

  let documentWired = false;
  function wireShell() {
    $("#ac-nav-toggle").onclick = () => ($("#ac-app").classList.contains("ac-nav-open") ? closeDrawer() : openDrawer());
    $("#ac-nav-close").onclick = closeDrawer;
    $("#ac-scrim").onclick = closeDrawer;
    // A click on the current section's link fires no hashchange; it still closes the drawer.
    $("#ac-side").addEventListener("click", (e) => { if (e.target.closest && e.target.closest("a")) closeDrawer(); });
    const btn = $("#ac-user-btn");
    const menu = $("#ac-user-menu");
    const setMenu = (open) => { menu.hidden = !open; btn.setAttribute("aria-expanded", String(open)); };
    btn.onclick = (e) => { e.stopPropagation(); setMenu(menu.hidden); };
    menu.addEventListener("click", (e) => { if (e.target.closest && e.target.closest("a")) setMenu(false); });
    if (!documentWired) {
      // Once per page, not per render: these read whichever shell is on the page now.
      documentWired = true;
      document.addEventListener("click", (e) => {
        const m = $("#ac-user-menu"), b = $("#ac-user-btn");
        if (m && !m.hidden && !m.contains(e.target) && !(b && b.contains(e.target))) { m.hidden = true; b.setAttribute("aria-expanded", "false"); }
      });
      document.addEventListener("keydown", (e) => {
        if (e.key !== "Escape") return;
        const m = $("#ac-user-menu"), b = $("#ac-user-btn");
        if (m && !m.hidden) { m.hidden = true; b.setAttribute("aria-expanded", "false"); b.focus(); }
        const app = $("#ac-app");
        if (app && app.classList.contains("ac-nav-open")) { closeDrawer(); $("#ac-nav-toggle").focus(); }
      });
    }
    // A card that unhides itself (credits and the publisher console with the marketplace feature,
    // the publisher console) brings its sidebar entry with it.
    if (window.MutationObserver) {
      const watch = new MutationObserver(syncNav);
      for (const id of ["#credits", "#ac-publisher", "#ac-analytics"]) {
        const node = $(id); if (node) watch.observe(node, { attributes: true, attributeFilter: ["hidden"] });
      }
    }
    syncNav();
  }

  // What the marketplace feature decided, once /v1/capabilities has answered. Null until then.
  let marketplaceOn = null;

  function syncNav() {
    const visible = (sel) => { const n = $(sel); return Boolean(n && !n.hidden); };
    const show = (sel, on) => { const n = $(sel); if (n) n.hidden = !on; };
    const creditsOn = visible("#credits");
    show('#ac-side [data-route="credits"]', creditsOn || marketplaceOn === null);
    show('#ac-side [data-route="publish"]', visible("#ac-publisher") || marketplaceOn === null);
    show('#ac-side [data-route="analytics"]', visible("#ac-analytics") || marketplaceOn === null);
    show("#ac-dash-credits", creditsOn);
    show("#ac-quick-credits", creditsOn);
    if (!creditsOn) show("#ac-credit-chip", false);
    const li = (sel) => { const a = $(sel); if (a && a.parentElement) a.parentElement.hidden = a.hidden; };
    li('#ac-side [data-route="credits"]'); li('#ac-side [data-route="publish"]');
    li('#ac-side [data-route="analytics"]');
    if (marketplaceOn === false) {
      const empty = (sel, text) => { const n = $(sel); if (n) n.innerHTML = `<p class="muted">${esc(text)}</p>`; };
      empty("#ac-empty-credits", "Credits pay for marketplace services. The marketplace isn't open on this account yet, so there is nothing to buy credit for.");
      empty("#ac-empty-analytics", "App analytics opens when the marketplace is available on this account.");
      empty("#ac-empty-publish", "The publisher console opens with the marketplace. It isn't available on this account yet.");
    }
  }

  // ---- billing details gate -----------------------------------------------------------------
  //
  // Web purchases are invoiced to the saved billing details, and checkout refuses without them
  // (billing_profile_required / billing_profile_incomplete). While none are saved, or the saved
  // ones lack what an invoice needs, every section that sells something shows one banner pointing
  // at Billing details, and its buy and checkout buttons go passive: disabled, aria-disabled and
  // a title saying why. Registering a name into a plan that has room, App Store and Google Play
  // purchases, and spending credit already bought need no billing details and stay as they are.
  // Saving complete details reloads the overview, which lifts the gate without a reload.

  // What an invoice needs; the same fields billing_profile_incomplete names.
  function profileComplete(p) {
    if (!p) return false;
    const a = p.address && typeof p.address === "object" ? p.address : p;
    return Boolean(String(p.legal_name || p.legalName || "").trim() && String(a.line1 || "").trim()
      && String(a.city || "").trim() && String(a.country || "").trim());
  }
  const SELLING_VIEWS = new Set(["get-handle", "plan", "products", "credits"]);
  const GATE_REASON = "Add billing details first";
  // Controls that start a paid web checkout. `when` narrows one to the cases that charge.
  const GATED = [
    { sel: "#ac-buy", when: () => !hasRoom() },
    { sel: "#ac-cloud-start" },
    { sel: "[data-change-plan]" },
    { sel: "#ac-cloud-picker button" },
    { sel: "#ac-products [data-buy]" },
    { sel: "#pp-change-go" },
    { sel: "#cr-buy-go" },
    { sel: "#mp-buy-go" },
  ];
  const gateWants = new WeakMap();   // what the page's own code wants each gated control to be
  let gateObserver = null;
  let gateObserved = null;   // the shell it watches; a re-render draws a new one

  function billingMissing() { return Boolean(overview) && !overview.hasBilling; }

  function syncBillingGate() {
    const app = $("#ac-app"); if (!app) return;
    const missing = billingMissing();
    app.classList.toggle("ac-needs-billing", missing);
    const banner = $("#ac-billing-gate");
    if (banner) {
      const view = currentRoute && ROUTES[currentRoute] ? ROUTES[currentRoute].view : null;
      banner.hidden = !(missing && SELLING_VIEWS.has(view));
      banner.classList.toggle("ac-gate-wide", view === "products");
      const why = $("#ac-billing-gate-why");
      if (why) why.textContent = overview && overview.billingIncomplete
        ? "Your billing details are missing a name, address, city or country. Complete them to buy plans, handles, Nests and credits."
        : "Purchases are invoiced to your billing details. Add them to buy plans, handles, Nests and credits.";
      const go = $("#ac-billing-gate-go");
      if (go) go.textContent = overview && overview.billingIncomplete ? "Complete billing details" : "Add billing details";
    }
    const gated = new Set();
    if (missing) for (const g of GATED) if (!g.when || g.when()) app.querySelectorAll(g.sel).forEach((n) => gated.add(n));
    app.querySelectorAll("[data-billing-gated]").forEach((n) => { if (!gated.has(n)) ungate(n); });
    gated.forEach(gate);
    if (gateObserver && gateObserved === app) gateObserver.takeRecords();
    else if (window.MutationObserver) {
      if (gateObserver) gateObserver.disconnect();
      gateObserved = app;
      // Buttons are redrawn and re-enabled by their own code (a quote arrives, a name turns out
      // free); keep them passive while the gate is up and remember what that code asked for.
      gateObserver = new MutationObserver((records) => {
        for (const r of records) {
          if (r.type === "attributes" && r.target.hasAttribute("data-billing-gated")) gateWants.set(r.target, !r.target.disabled);
        }
        syncBillingGate();
      });
      gateObserver.observe(app, { subtree: true, childList: true, attributes: true, attributeFilter: ["disabled"] });
    }
  }
  function gate(n) {
    if (!n.hasAttribute("data-billing-gated")) {
      gateWants.set(n, !n.disabled);
      n.setAttribute("data-billing-gated", "");
      n.setAttribute("aria-disabled", "true");
      n.setAttribute("aria-describedby", "ac-billing-gate-why");
      if (!n.hasAttribute("title")) { n.setAttribute("title", GATE_REASON); n.setAttribute("data-billing-title", ""); }
    }
    n.disabled = true;
  }
  function ungate(n) {
    n.removeAttribute("data-billing-gated");
    n.removeAttribute("aria-disabled");
    if (n.getAttribute("aria-describedby") === "ac-billing-gate-why") n.removeAttribute("aria-describedby");
    if (n.hasAttribute("data-billing-title")) { n.removeAttribute("title"); n.removeAttribute("data-billing-title"); }
    n.disabled = gateWants.has(n) ? !gateWants.get(n) : false;
  }

  // ---- overview dashboard -------------------------------------------------------------------

  const dash = { handles: null, boxes: null, balance: undefined };
  function renderDash() {
    const set = (sel, text) => { const n = $(sel); if (n) n.textContent = text; };
    if (overview) {
      const plan = webPlan();
      const sub = overview.subscription;
      if (plan.capacity > 0) {
        set("#ac-dash-plan", `${plan.capacity} name${plan.capacity === 1 ? "" : "s"}`);
        set("#ac-dash-plan-s", `${plan.used} registered${plan.paid_through ? ` · paid through ${fmtDate(plan.paid_through)}` : ""}`);
      } else {
        set("#ac-dash-plan", "No plan");
        set("#ac-dash-plan-s", sub ? statusText(sub.provider_status) : "Choose one to register names");
      }
      const callout = $("#ac-dash-billing"); if (callout) callout.hidden = overview.hasBilling;
    }
    if (dash.handles != null) {
      set("#ac-dash-handles", String(dash.handles.total));
      set("#ac-dash-handles-s", dash.handles.total ? `${dash.handles.active} active` : "Get your first one");
    }
    if (dash.boxes != null) {
      set("#ac-dash-boxes", String(dash.boxes.total));
      set("#ac-dash-boxes-s", dash.boxes.total ? `${dash.boxes.named} named` : "Connect an AI agent");
    }
    if (dash.balance !== undefined) {
      const b = dash.balance;
      const text = b ? money(Number(b.available_cents || 0) / 100, "USD") : "—";
      set("#ac-dash-credit", text);
      set("#ac-dash-credit-s", b && b.held_cents > 0 ? `${money(b.held_cents / 100, "USD")} reserved for runs` : "For marketplace services");
      set("#ac-credit-chip-v", text);
      const chip = $("#ac-credit-chip"); if (chip) chip.hidden = !b || !$("#credits") || $("#credits").hidden;
      if (chip) chip.setAttribute("aria-label", `Credit balance ${text}`);
    }
  }

  // ---- Products (Nests Phase B2) --------------------------------------------------------------
  //
  // products.js renders the three tabs with this page's own authenticated helpers; it holds no
  // token itself. Nest purchases use the same hosted checkout page as Cloud plans.
  async function mountProducts() {
    const box = $("#ac-products");
    if (!box || !window.PPProducts) return;
    let features = new Set();
    try {
      const caps = await pbFetch("/v1/capabilities");
      if (Array.isArray(caps.features)) features = new Set(caps.features.map(String));
    } catch (_) { /* an older postbox: nothing new is on */ }
    const marketplace = features.has("marketplace") && window.PPMarketplace;
    marketplaceOn = Boolean(marketplace);
    // A staff or publishing action the postbox refused for want of a recent strong sign-in: sign
    // in again with its acr_values/max_age, and come back to the same card.
    const stepUp = (challenge, section) => {
      SS.setItem("pp_mp_return", section || "");
      login(undefined, undefined, challenge);
    };
    if (marketplace) mountMarketplace(stepUp);
    // Credits (credits v1) pay for third-party services, so they come with the marketplace.
    if (marketplace) mountCredits();
    window.PPProducts.mount(box, {
      features,
      stepUp,
      pb: (path, init) => pbFetch(path, init),
      bridgeGet: (path) => apiGet(path),
      bridgeAction: (path, body, method) => apiAction(path, body, method),
      startCheckout: (pending) => {
        TS.setItem("pp_cloud_checkout", JSON.stringify(pending));
        window.location.href = cloudCfg().checkoutPath || "/billing/checkout";
      },
      billingProfileId: async () => (await ensureOverview()).profileId || null,
      goBilling: () => focusStep("#ac-billing", 'input[name="legal_name"]'),
      focusInbox: () => focusStep(".ac-search-card", "#ac-handle"),
      toast,
      randomHex32: () => randomString(16),
      search: window.location.search,
      clearReturn: (key) => {
        const params = new URLSearchParams(window.location.search);
        params.delete(key);
        history.replaceState({}, "", window.location.pathname + (params.size ? "?" + params : "") + window.location.hash);
      },
      preferredCurrency: defaultCurrency(),
      locale: /^tr\b/i.test(navigator.language || "") ? "tr" : "en",
    });
    const back = SS.getItem("pp_mp_return");
    SS.removeItem("pp_mp_return");
    // Back from a step-up sign-in: open the card that asked for it. The Products card has just
    // drawn its tabs, so the current route's tab (`#nests`, `#services`, …) is pressed again.
    if (MP_RETURN_ROUTE[back || ""] && !hashRoute()) go(MP_RETURN_ROUTE[back], true);
    const spec = currentRoute && ROUTES[currentRoute];
    if (spec && spec.tab) selectProductTab(spec.tab);
    syncNav();
  }

  // ---- credits (credits v1) --------------------------------------------------------------------
  //
  // credits.js renders the card with this page's own bridge helpers; it holds no token. The
  // payment page is iyzico's; it returns here with `?credits=<intent>#credits`.
  let credits = null;
  function mountCredits() {
    const box = $("#credits");
    if (!box || !window.PPCredits) return;
    box.hidden = false;
    credits = window.PPCredits.mount(box, {
      bridgeGet: (path) => apiGet(path),
      bridgeAction: (path, body, method) => apiAction(path, body, method),
      billingProfileId: async () => (await ensureOverview()).profileId || null,
      goBilling: () => focusStep("#ac-billing", 'input[name="legal_name"]'),
      randomHex32: () => randomString(16),
      search: window.location.search,
      clearReturn: (key) => {
        const params = new URLSearchParams(window.location.search);
        params.delete(key);
        history.replaceState({}, "", window.location.pathname + (params.size ? "?" + params : "") + window.location.hash);
      },
      navigate: (url) => { window.location.href = url; },
      preferredCurrency: defaultCurrency(),
      // The top bar's chip and the overview tile show the same balance the card loads.
      onBalance: (balance) => { dash.balance = balance; renderDash(); },
    });
    // `?credits=<intent>` is iyzico's return; initialRoute already opened the Credits section.
    syncNav();
  }
  // A run elsewhere on the page spent or freed credit: show the new balance.
  window.addEventListener("pp-credits-changed", () => { if (credits) credits.reload(); });

  // ---- marketplace publisher console -----------------------------------------------------------
  //
  // Only with the postbox's `marketplace` feature on. The publisher card is for anyone who wants
  // to apply or already publishes. Staff tools are in the back office (/staff).
  function mountMarketplace(stepUp) {
    const deps = { pb: (path, init) => pbFetch(path, init), toast, stepUp };
    const analytics = $("#ac-analytics");
    if (analytics && window.PPMarketplace.analytics) { analytics.hidden = false; window.PPMarketplace.analytics(analytics, deps); const empty = $("#ac-empty-analytics"); if(empty) empty.hidden = true; }
    const pub = $("#ac-publisher");
    if (pub && window.PPMarketplace.publisher) { pub.hidden = false; window.PPMarketplace.publisher(pub, deps); }
  }

  function renderDeletion(el) {
    el.innerHTML = `
      <div class="ac-card">
        <h2>Delete your Pigeonpost account</h2>
        <p id="ac-deletion-account"></p>
        <p>This requests deletion of your hosted sign-in account, mailboxes, keys, your copies of messages and attachments, contacts and account settings. You will lose access to your handles.</p>
        <p>We complete requests within 30 days and confirm completion using your verified account contact. Limited payment and security records may be retained where required. Copies already delivered to other accounts are separate.</p>
        <p><strong>Store subscriptions continue until you cancel them.</strong> Please cancel active subscriptions before continuing: <a href="https://apps.apple.com/account/subscriptions">Apple subscriptions</a> or <a href="https://play.google.com/store/account/subscriptions?package=dev.pigeonpost.inbox">Google Play subscriptions</a>.</p>
        <div id="ac-deletion-body"><p>Checking your request…</p></div>
        <p role="status" id="ac-deletion-status"></p>
        <p><a href="/app-privacy.html">Privacy policy</a> · <a href="/app-terms.html">Terms of use</a></p>
      </div>`;
    const body = $("#ac-deletion-body");
    if (!getToken()) {
      body.innerHTML = '<p>Sign in to confirm which account you want deleted. You do not need to contact support to submit this request.</p><button class="btn btn-primary" id="ac-deletion-signin">Sign in to continue</button>';
      $("#ac-deletion-signin").onclick = () => login(false);
      return;
    }
    const token = getToken();
    const current = () => token === getToken() && $("#ac-deletion-body") === body;
    const signInAgain = () => {
      body.innerHTML = '<p>Your session expired. Sign in again before requesting deletion.</p><button class="btn btn-primary" id="ac-deletion-signin">Sign in again</button>';
      $("#ac-deletion-signin").onclick = () => { signOut(); login(false); };
    };
    const showReceipt = (request) => {
      body.innerHTML = '<h3>Deletion request received</h3><p id="ac-deletion-receipt"></p><p>You can keep this reference. No further request is needed.</p>';
      const date = new Date(request.complete_by * 1000).toLocaleDateString();
      $("#ac-deletion-receipt").textContent = `Reference: ${request.request_id}. Complete by ${date}.`;
    };
    const showForm = () => {
      body.innerHTML = '<p>Type DELETE to confirm that you want this account and its associated data deleted.</p><label for="ac-deletion-confirm">Confirmation</label><input id="ac-deletion-confirm" type="text" autocomplete="off" spellcheck="false"><button class="btn btn-primary" id="ac-deletion-submit" disabled>Request account deletion</button><button class="btn btn-secondary" id="ac-deletion-logout">Use a different account</button>';
      const confirm = $("#ac-deletion-confirm");
      const submit = $("#ac-deletion-submit");
      confirm.oninput = () => { submit.disabled = confirm.value !== "DELETE"; };
      $("#ac-deletion-logout").onclick = () => { signOut(); render(); };
      submit.onclick = async () => {
        if (submit.disabled || confirm.value !== "DELETE") return;
        submit.disabled = true;
        confirm.disabled = true;
        try {
          const result = await pbFetch("/v1/account/deletion-request", {method: "POST", body: JSON.stringify({confirm: true})});
          if (current()) showReceipt(result.request);
        } catch (error) {
          if (!current()) return;
          if (error.status === 401) { signInAgain(); return; }
          $("#ac-deletion-status").textContent = "Your request was not confirmed. " + error.message + " You can safely try again.";
          submit.disabled = false;
          confirm.disabled = false;
        }
      };
    };
    pbFetch("/v1/account/deletion-request").then((result) => {
      if (!current()) return;
      $("#ac-deletion-account").textContent = result.account?.label ? "Account: " + result.account.label : "";
      if (result.request) showReceipt(result.request); else showForm();
    }).catch((error) => {
      if (!current()) return;
      if (error.status === 401) { signInAgain(); return; }
      body.innerHTML = '<button class="btn btn-secondary" id="ac-deletion-retry">Try again</button>';
      $("#ac-deletion-status").textContent = "Could not check your request. " + error.message;
      $("#ac-deletion-retry").onclick = render;
    });
  }

  // ---- hosted postbox (MCP connector) --------------------------------------------------------
  // The postbox is a separate origin; it accepts this member token (validated against the realm)
  // and resolves it to an account. CORS on the postbox allows pigeonpost.dev.

  const POSTBOX = "https://postbox.pigeonpost.dev";
  async function pbFetch(path, opts, retried = false) {
    const o = opts || {};
    const res = await fetch(POSTBOX + path, {
      method: o.method || "GET",
      headers: Object.assign(
        { authorization: `Bearer ${getToken()}`, accept: "application/json" },
        // A listing logo goes up as the image itself (`contentType`); everything else is JSON.
        o.body ? { "content-type": o.contentType || "application/json" } : {},
      ),
      body: o.body,
    });
    const body = await res.json().catch(() => ({}));
    // A step-up refusal is not an expired token: refreshing cannot make the sign-in stronger.
    const stepUp = body.error === "step_up_required" || (body.error && body.error.code === "step_up_required");
    if (res.status === 401 && !stepUp && !retried && await refreshSession()) return pbFetch(path, opts, true);
    if (!res.ok) {
      // Two shapes: `{error: "code", detail, …}` and the contract's
      // `{error: {code, message, details}}` (marketplace routes). Read both as the first.
      const nested = body && typeof body.error === "object" && body.error ? body.error : null;
      const flat = nested ? { ...(nested.details || {}), error: nested.code || null, detail: nested.message || "" } : body;
      const e = new Error(flat.detail || flat.error || `postbox ${res.status}`);
      e.status = res.status;
      // The whole error body: step-up challenges carry acr_values/max_age, validation errors issues.
      e.body = flat;
      // The machine-readable half. Callers translate these into sentences; the detail string is a
      // fallback for codes nobody has written a sentence for yet.
      e.code = flat.error || null;
      throw e;
    }
    return body;
  }

  async function loadPostbox() {
    const box = $("#ac-pb-list"); if (!box) return;
    try {
      const { identities } = await pbFetch("/v1/identities");
      // The handle an account already holds decides what to suggest for the next mailbox: agents
      // hang under the person, and which segment is the person depends on the shape of the name.
      pbNamed = identities.filter((i) => i.handle).map((i) => i.handle);
      dash.boxes = { total: identities.length, named: pbNamed.length };
      renderDash();
      box.innerHTML = (identities.length
        ? identities.map((i) => `<div class="ac-row">
            <span class="mono">${esc(i.handle || i.address)}</span>
            <span class="muted">${esc(i.label || "")}</span>
            <span class="ac-row-actions">
              ${i.handle ? "" : `<button class="btn btn-small" data-pbname="${esc(i.address)}">Name it</button>`}
              <button class="btn btn-small" data-pbview="${esc(i.address)}">View inbox</button>
              <button class="btn btn-small" data-pbdel="${esc(i.address)}">Delete</button>
            </span>
          </div>`).join("")
        : `<p class="muted">No inboxes yet — create one for your agent.</p>`)
        + `<div id="ac-pb-name"></div><div id="ac-pb-msgs"></div>`;
      box.querySelectorAll("[data-pbview]").forEach((b) => b.onclick = () => pbViewInbox(b.getAttribute("data-pbview")));
      box.querySelectorAll("[data-pbdel]").forEach((b) => b.onclick = () => pbDeleteInbox(b.getAttribute("data-pbdel")));
      box.querySelectorAll("[data-pbname]").forEach((b) => b.onclick = () => pbNameForm(b.getAttribute("data-pbname")));
    } catch (e) {
      box.innerHTML = `<p class="muted">${e.status === 401 ? "Sign in again to manage inboxes." : "Could not reach the postbox."}</p>`;
    }
  }

  // ---- naming a mailbox: a free handle, or an agent under one you hold ------------------------
  //
  // Free names are minted here, not sold: `/pp` is claimed by being first, and `/github/<login>`
  // and an address-shaped handle come from having proved them at sign-in. A paid flat name is the
  // separate purchase flow above. All three end at the same call.

  let pbNamed = [];
  const H = () => window.PIGEONPOST_HANDLE;

  // Why a name was refused, in words. The postbox is authoritative about every one of these; the
  // page only translates.
  function pbNameError(e) {
    switch (e.code) {
      case "handle_taken":     return "somebody already has that name";
      case "already_named":    return "this inbox already has a handle — a mailbox keeps the first one it is given";
      case "namespace_quota":  return "you already hold a free name, and a handle identifies one person. Agents under it are free and unlimited";
      case "namespace_not_yours": return "that name sits under somebody else's handle";
      case "open_namespace_closed":
      case "reserved_names_unavailable":
        return "free names are not being handed out right now";
      default: return e.message || "the postbox refused that name";
    }
  }

  // What to offer. Somebody who already holds a name is almost certainly adding an agent to it;
  // somebody who holds none is claiming their first, which is free and under /pp.
  function pbSuggest(addr) {
    const held = pbNamed[0];
    if (held && H()) {
      const ns = H().namespaceOf(held);
      if (ns) return `${ns}/agent1`;
    }
    return "/pp/";
  }

  function pbNameForm(addr) {
    const out = $("#ac-pb-name"); if (!out) return;
    out.innerHTML = `
      <div class="ac-card">
        <h4>Name <span class="mono">${esc(addr)}</span></h4>
        <p class="muted">A free name under <span class="mono">/pp</span>, or an agent beneath a
          handle you already hold — <span class="mono">/you/agent1</span>. One free name per
          person; agents under it cost nothing.</p>
        <input id="ac-pb-handle" class="ac-input mono" value="${esc(pbSuggest(addr))}" spellcheck="false">
        <p class="ac-note" id="ac-pb-handle-why"></p>
        <div class="ac-actions ac-left">
          <button class="btn btn-primary" id="ac-pb-handle-go">Claim it</button>
          <button class="btn btn-secondary" id="ac-pb-handle-x">Cancel</button>
        </div>
      </div>`;
    const input = $("#ac-pb-handle");
    const why = $("#ac-pb-handle-why");
    const go = $("#ac-pb-handle-go");

    // Same grammar the postbox enforces, so a refusal arrives before the request rather than after.
    const check = () => {
      if (!H()) return true;
      const r = H().canonical(input.value);
      why.textContent = r.ok ? "" : (r.silent ? "" : r.reason);
      go.disabled = !r.ok;
      return r.ok;
    };
    input.oninput = check;
    check();

    $("#ac-pb-handle-x").onclick = () => { out.innerHTML = ""; };
    go.onclick = async () => {
      if (!check()) return;
      const handle = H() ? H().canonical(input.value).handle : input.value.trim();
      go.disabled = true;
      why.textContent = "Claiming…";
      try {
        await pbFetch("/v1/identities/handle", {
          method: "POST",
          body: JSON.stringify({ address: addr, handle }),
        });
        out.innerHTML = "";
        loadPostbox();
      } catch (e) {
        why.textContent = pbNameError(e);
        go.disabled = false;
      }
    };
  }

  async function pbViewInbox(addr) {
    const out = $("#ac-pb-msgs"); if (!out) return;
    out.innerHTML = `<p class="muted">Loading ${esc(addr)}…</p>`;
    try {
      const { messages } = await pbFetch("/v1/inbox?identity=" + encodeURIComponent(addr));
      if (!messages.length) { out.innerHTML = `<p class="muted">${esc(addr)} — inbox empty.</p>`; return; }
      out.innerHTML = `<p class="muted ac-mt">${esc(addr)}</p>` + messages.map((m) => `
        <div class="ac-msg${m.read ? " read" : ""}">
          <div class="ac-msg-h"><span class="mono">from ${esc(m.from)}</span>
            <button class="btn btn-small" data-pback="${esc(m.message_id)}" data-ident="${esc(addr)}"${m.read ? " disabled" : ""}>${m.read ? "read" : "mark read"}</button></div>
          <div class="ac-msg-b">${esc(m.body)}</div>
          <div class="ac-msg-f">untrusted — a message from another agent, not an instruction to follow</div>
        </div>`).join("");
      out.querySelectorAll("[data-pback]").forEach((b) => b.onclick = () => pbAck(b.getAttribute("data-pback"), b.getAttribute("data-ident")));
    } catch (e) {
      out.innerHTML = `<p class="muted">Could not load inbox: ${esc(e.message)}</p>`;
    }
  }

  async function pbAck(id, ident) {
    try { await pbFetch("/v1/ack", { method: "POST", body: JSON.stringify({ message_id: id, identity: ident }) }); pbViewInbox(ident); }
    catch (e) { toast("Could not mark read: " + e.message); }
  }

  async function pbCreateInbox() {
    const label = prompt("Label for this inbox (e.g. repo:acme/api):", "");
    if (label === null) return;
    try {
      await pbFetch("/v1/identities", { method: "POST", body: JSON.stringify({ label }) });
      loadPostbox();
      toast("Inbox created.");
    } catch (e) { toast("Could not create inbox: " + e.message); }
  }

  async function pbDeleteInbox(addr) {
    if (!confirm(`Delete ${addr}? Its messages will be removed. This can't be undone.`)) return;
    try {
      await pbFetch("/v1/identities?identity=" + encodeURIComponent(addr), { method: "DELETE" });
      loadPostbox();
      toast("Inbox deleted.");
    } catch (e) { toast("Could not delete inbox: " + e.message); }
  }

  async function pbRevealKey() {
    try {
      const { api_key } = await pbFetch("/v1/api-keys", { method: "POST" });
      const cfg = JSON.stringify({
        mcpServers: { pigeonpost: { url: "https://mcp.pigeonpost.dev/mcp", headers: { Authorization: "Bearer " + api_key } } },
      }, null, 2);
      $("#ac-pb-out").innerHTML =
        `<p class="ac-warn">Save this connector key — it's shown once. Paste the config into your Claude/ChatGPT MCP settings.</p>
         <pre class="ac-pre">${esc(cfg)}</pre>`;
      loadKeys();
    } catch (e) { toast("Could not create a connector key: " + e.message); }
  }

  async function loadKeys() {
    const box = $("#ac-pb-keys"); if (!box) return;
    try {
      const { keys } = await pbFetch("/v1/api-keys");
      box.innerHTML = keys.length
        ? keys.map((k) => `<div class="ac-row">
            <span class="mono">${esc(k.prefix)}…</span>
            <span class="muted">created ${esc(fmtDate(k.created_at))}</span>
            <button class="btn btn-small" data-pbrevoke="${esc(k.id)}">Revoke</button>
          </div>`).join("")
        : `<p class="muted">No connector keys yet — create one to connect an agent.</p>`;
      box.querySelectorAll("[data-pbrevoke]").forEach((b) => b.onclick = () => pbRevokeKey(b.getAttribute("data-pbrevoke")));
    } catch (e) {
      box.innerHTML = `<p class="muted">${e.status === 401 ? "Sign in again to manage keys." : "Could not load keys."}</p>`;
    }
  }

  async function pbRevokeKey(id) {
    if (!confirm("Revoke this key? Any agent using it will lose access immediately.")) return;
    try {
      await pbFetch("/v1/api-keys/" + encodeURIComponent(id), { method: "DELETE" });
      loadKeys();
      toast("Key revoked.");
    } catch (e) { toast("Could not revoke key: " + e.message); }
  }

  function fmtDate(secs) {
    if (!secs) return "";
    try { return new Date(secs * 1000).toLocaleDateString(); } catch (_) { return ""; }
  }


  // ---- names and the Pigeonpost Cloud plan ---------------------------------------------------
  //
  // A name on the web comes from a Pigeonpost Cloud plan: 1–10 names, paid yearly through iyzico.
  // Buying the plan and registering names are two steps, as in the apps: a plan gives capacity,
  // and each name is registered into it here. The postbox is authoritative about capacity.

  const cloudCfg = () => cfg.cloud || { plans: {}, currencies: ["USD", "EUR", "TRY"] };
  const CURRENCY_NAMES = { USD: "US dollars (USD)", EUR: "Euros (EUR)", TRY: "Turkish lira (TRY)" };
  // Euro-area regions, and languages whose default region is in the euro area.
  const EURO_REGIONS = /^(AT|BE|CY|DE|EE|ES|FI|FR|GR|HR|IE|IT|LT|LU|LV|MT|NL|PT|SI|SK)$/;
  const EURO_LANGUAGES = /^(de|fr|it|nl|fi|el|et|lv|lt|mt|sk|sl|hr|ga|lb)$/;
  const PLAN_CODE = (names, currency) => `cloud-${names}names-yearly-${currency.toLowerCase()}`;
  const webPlan = () => (overview && overview.plan) || { capacity: 0, used: 0, names: [] };
  const hasRoom = () => webPlan().active !== false && webPlan().capacity > webPlan().used;

  function defaultCurrency() {
    const saved = SS.getItem("pp_currency");
    if (saved === "USD" || saved === "EUR" || saved === "TRY") return saved;
    const [language, region] = String(navigator.language || "").split("-");
    if (/^tr$/i.test(language || "")) return "TRY";
    if (region ? EURO_REGIONS.test(region.toUpperCase()) : EURO_LANGUAGES.test((language || "").toLowerCase())) return "EUR";
    return "USD";
  }

  function priceLabel(names, currency) {
    const plan = cloudCfg().plans[names];
    return plan ? `${money(Number(plan[currency]), currency)}/year` : "";
  }

  function searchBlock() {
    return `
      <div class="ac-card ac-search-card">
        <h3>Get a handle</h3>
        <p class="muted">Names come with a Pigeonpost Cloud plan: ${money(Number(cloudCfg().perNameUSD || 8), "USD")} per name per year, up to 10 names.</p>
        <div class="ac-search">
          <div class="ac-field"><span class="slash">/</span>
            <input id="ac-handle" type="text" placeholder="yourname" autocomplete="off"
              autocapitalize="none" spellcheck="false" maxlength="32" aria-label="handle"></div>
          <button class="btn btn-primary" id="ac-buy" disabled>Get it</button>
        </div>
        <p class="hs" id="ac-handle-status" aria-live="polite"></p>
      </div>`;
  }

  // The format check is local and instant; whether the name is *free* is a question only the
  // postbox can answer, so it is asked, debounced, before the button is enabled. Unknown is not
  // free: if the check cannot be made, the button stays disabled and says so.
  function wireSearch(signedIn) {
    const input = $("#ac-handle"); if (!input) return;
    const status = $("#ac-handle-status");
    const buy = $("#ac-buy");
    let current = { ok: false };
    let checking = 0;
    const label = () => { buy.textContent = signedIn && hasRoom() ? "Register it" : "Get it"; };
    label();

    async function check(v) {
      const mine = ++checking;
      status.className = "hs";
      status.textContent = `checking /${v.name}…`;
      buy.disabled = true;
      let answer = null;
      try {
        const path = `/v1/handles/${encodeURIComponent(v.name)}/availability`;
        if (signedIn) {
          answer = await apiGet(path);
        } else {
          const res = await fetch(api(path), { headers: { accept: "application/json" } });
          answer = res.ok ? await res.json() : null;
        }
      } catch { answer = null; }
      if (mine !== checking) return;
      if (!answer || answer.known === false) {
        status.classList.add("bad");
        status.textContent = "✗ could not check that name just now — try again in a moment";
        return;
      }
      if (!answer.available) {
        status.classList.add("bad");
        status.textContent = `✗ /${v.name} is already taken`;
        return;
      }
      status.classList.add("ok");
      status.textContent = `✓ /${v.name} is available`;
      label();
      buy.disabled = false;
    }

    let debounce;
    input.oninput = () => {
      const v = validate(input.value); current = v;
      status.className = "hs";
      clearTimeout(debounce);
      checking++;
      buy.disabled = true;
      if (v.silent) { status.textContent = ""; return; }
      if (!v.ok) { status.classList.add("bad"); status.textContent = "✗ " + v.reason; return; }
      status.textContent = `checking /${v.name}…`;
      debounce = setTimeout(() => check(v), 350);
    };
    buy.onclick = () => {
      if (!current.ok) return;
      if (!signedIn || !getToken()) { SS.setItem("pp_pending_handle", current.name); login(); return; }
      startPurchase(current.name);
    };
  }

  async function ensureOverview() {
    if (!overview) await loadOverview();
    return overview || { hasBilling: false, plan: null };
  }

  // A name needs a plan with room, and a plan needs billing details. Walk the member to whichever
  // step is missing and remember the name, so the flow resumes once each step is done.
  async function startPurchase(name) {
    const ov = await ensureOverview();
    if (hasRoom()) { SS.removeItem("pp_pending_handle"); await registerName(name); return; }
    SS.setItem("pp_pending_handle", name);
    if (ov.subscription) {
      toast(`Your plan's names are all registered. Choose a larger plan to add /${name}.`);
      focusStep("#ac-cloud", "[data-change-plan]");
      return;
    }
    if (!ov.hasBilling) {
      toast(`Almost there — add your billing details, then choose a plan for /${name}.`);
      focusStep("#ac-billing", 'input[name="legal_name"]');
      return;
    }
    showPlanPicker(Math.max(1, webPlan().used + 1));
  }

  async function registerName(name) {
    const button = $("#ac-buy");
    if (button) button.disabled = true;
    try {
      await apiAction("/v1/names", { namespace: name });
      toast(`/${name} is yours.`);
      const input = $("#ac-handle"); if (input) { input.value = ""; input.dispatchEvent(new Event("input")); }
      await loadOverview();
      loadPostbox();
    } catch (e) {
      toast(`Could not register /${name}: ${e.message}`);
      if (button) button.disabled = false;
    }
  }

  // Open the section holding a card, scroll the card into view and put the cursor on the
  // field/button that needs attention.
  function focusStep(cardSelector, targetSelector) {
    const card = $(cardSelector);
    const view = card && card.closest && card.closest(".ac-view");
    if (view && view.hidden) go(routeForView(view.getAttribute("data-view")));
    if (card && card.scrollIntoView) card.scrollIntoView({ behavior: "smooth", block: "center" });
    const target = card && $(targetSelector, card);
    if (target) {
      card.classList.add("ac-attn");
      setTimeout(() => card.classList.remove("ac-attn"), 1600);
      setTimeout(() => { try { target.focus({ preventScroll: true }); } catch (_) { target.focus(); } }, 300);
    }
  }

  // ---- plan picker and hosted checkout --------------------------------------------------------

  function showPlanPicker(minNames) {
    const box = $("#ac-cloud-picker"); if (!box) return;
    const open = cloudCfg().currencies.filter((c) => !overview?.openCurrencies || overview.openCurrencies.includes(c));
    let currency = open.includes(defaultCurrency()) ? defaultCurrency() : open[0];
    const draw = () => {
      const options = Object.keys(cloudCfg().plans).map(Number).filter((n) => n >= minNames)
        .map((n) => `<option value="${n}">${n} name${n === 1 ? "" : "s"} — ${esc(priceLabel(n, currency))}</option>`).join("");
      box.innerHTML = `
        <div class="ac-form">
          <label>Plan <select id="ac-plan-names">${options}</select></label>
          <label>Currency <select id="ac-plan-currency">
            ${open.map((c) => `<option value="${c}"${c === currency ? " selected" : ""}>${esc(CURRENCY_NAMES[c] || c)}</option>`).join("")}
          </select></label>
          <p class="ac-note">Sold by Piyote Bilişim and billed yearly through iyzico. Cards issued in Türkiye pay in TRY.
            Consumers in the EU and UK: buy in the Pigeonpost iOS or Android app. EU and UK businesses buy here with a VAT number (reverse charge).
            TRY prices are fixed for ${esc(cloudCfg().pricingYear || "")}. Cancel any time; your names keep working until the end of the paid year.</p>
          <div class="ac-actions ac-left">
            <button class="btn btn-primary" id="ac-plan-go">Continue to secure payment</button>
            <button class="btn btn-secondary" id="ac-plan-x">Cancel</button>
          </div>
        </div>`;
      $("#ac-plan-currency").onchange = (e) => { currency = e.target.value; SS.setItem("pp_currency", currency); draw(); };
      $("#ac-plan-x").onclick = () => { box.innerHTML = ""; };
      $("#ac-plan-go").onclick = () => beginCheckout(PLAN_CODE(Number($("#ac-plan-names").value), currency));
    };
    draw();
    focusStep("#ac-cloud", "#ac-plan-names");
  }

  // The hosted iyzico form runs on its own page, whose content-security policy admits iyzico and
  // nothing else; this page never loads third-party script. The intent id is the idempotency key
  // for the whole purchase: a reload or a lost response replays it rather than charging twice.
  function beginCheckout(planCode) {
    if (!overview || !overview.profileId) { focusStep("#ac-billing", 'input[name="legal_name"]'); return; }
    const pending = { kind: "cloud", intent_id: randomString(16), plan_code: planCode,
      billing_profile_id: overview.profileId, locale: /^tr\b/i.test(navigator.language || "") ? "tr" : "en" };
    TS.setItem("pp_cloud_checkout", JSON.stringify(pending));
    window.location.href = cloudCfg().checkoutPath || "/billing/checkout";
  }

  // Back from iyzico (via saas_be's callback). The redirect proves nothing: ask the bridge, which
  // reads saas_be's authoritative state and activates the plan only for a confirmed payment.
  async function confirmCheckoutIfReturning() {
    const params = new URLSearchParams(window.location.search);
    const intent = params.get("checkout");
    const card = params.get("card");
    const notice = sessionStorage.getItem("pp_checkout_notice");
    if (notice) { sessionStorage.removeItem("pp_checkout_notice"); toast(notice); }
    if (!intent && !card) return;
    params.delete("checkout"); params.delete("card");
    history.replaceState({}, "", window.location.pathname + (params.size ? "?" + params : "") + window.location.hash);
    if (card) { toast("Card update submitted. It may take a minute to show."); return; }
    if (!/^[a-f0-9]{32}$/.test(intent) || !getToken()) return;
    TS.removeItem("pp_cloud_checkout");
    toast("Confirming your payment…");
    for (let attempt = 0; attempt < 20; attempt++) {
      try {
        const result = await apiGet(`/v1/cloud/checkout/${intent}`);
        if (["active", "canceled"].includes(result.status)) { toast("Your Pigeonpost Cloud plan is active."); return true; }
        if (result.status === "failed") { toast("The payment did not go through. No plan was started."); return true; }
      } catch (e) {
        if (e.status === 401) return;
      }
      await new Promise((r) => setTimeout(r, 3000));
    }
    toast("Your payment is still being confirmed. This page will show the plan as soon as it is.");
    return true;
  }

  // ---- the plan card ----------------------------------------------------------------------------

  function renderCloud(cloud) {
    const el = $("#ac-cloud-body"); if (!el) return;
    if (!cloud) { el.innerHTML = `<p class="muted" role="status">Could not load your plan. Try Refresh handles.</p>`; return; }
    const plan = cloud.plan?.web_plan || { capacity: 0, used: 0, names: [] };
    const sub = cloud.subscriptions?.[0] || null;
    const lines = [];
    if (plan.capacity > 0) {
      lines.push(`<p><strong>${Number(plan.capacity)} name${plan.capacity === 1 ? "" : "s"}</strong> · ${Number(plan.used)} registered
        ${plan.paid_through ? ` · paid through ${esc(fmtDate(plan.paid_through))}` : ""}</p>`);
    } else {
      lines.push(`<p class="muted">No Pigeonpost Cloud plan yet. Names bought in the App Store or Google Play are managed there.</p>`);
    }
    if (sub) {
      lines.push(`<div class="ac-row"><span class="muted">${esc(sub.names)} names · ${esc(sub.currency)}
        ${sub.recurring_price ? " · " + esc(money(Number(sub.recurring_price.amount), sub.recurring_price.currency)) + "/year" : ""}
        · ${esc(statusText(sub.provider_status))}</span>
        <span class="ac-row-actions">
          <button class="btn btn-small" data-change-plan="${esc(sub.id)}">Change plan</button>
          <button class="btn btn-small" data-card="${esc(sub.id)}">Update card</button>
          <button class="btn btn-small" data-manage="${esc(sub.id)}">Details</button>
          ${sub.provider_status === "CANCELED" ? "" : `<button class="btn btn-small" data-cancel="${esc(sub.id)}">Cancel</button>`}
        </span></div><div id="ac-cloud-detail"></div>`);
    } else {
      lines.push(`<button class="btn btn-secondary" id="ac-cloud-start">Choose a plan</button>`);
    }
    lines.push(`<div id="ac-cloud-picker"></div>`);
    el.innerHTML = lines.join("");
    const start = $("#ac-cloud-start");
    if (start) start.onclick = () => (overview?.hasBilling ? showPlanPicker(Math.max(1, plan.used)) : focusStep("#ac-billing", 'input[name="legal_name"]'));
    el.querySelectorAll("[data-cancel]").forEach((b) => b.onclick = () => cancelCloud(b.getAttribute("data-cancel")));
    el.querySelectorAll("[data-card]").forEach((b) => b.onclick = () => updateCard(b.getAttribute("data-card")));
    el.querySelectorAll("[data-manage]").forEach((b) => b.onclick = () => showSubscription(b.getAttribute("data-manage")));
    el.querySelectorAll("[data-change-plan]").forEach((b) => b.onclick = () => changePlan(sub, plan));
  }

  function statusText(provider) {
    return { ACTIVE: "Active", CANCELED: "Canceled — no renewal", UNPAID: "Payment failed", PENDING: "Payment pending",
      CHECKOUT_PENDING: "Checkout not finished", UPGRADED: "Changed", EXPIRED: "Expired" }[provider] || "Processing";
  }

  async function showSubscription(id) {
    const out = $("#ac-cloud-detail"); if (!out) return;
    out.innerHTML = `<p class="muted">Loading…</p>`;
    try {
      const s = await apiGet(`/v1/cloud/subscriptions/${encodeURIComponent(id)}`);
      out.innerHTML = `<p class="muted">${esc(statusText(s.provider_status))}${s.paid_through ? ` · paid through ${esc(fmtDate(s.paid_through))}` : ""}
          ${s.next_payment_at ? ` · next payment ${esc(new Date(s.next_payment_at).toLocaleDateString())}` : ""}
          ${s.pending_change ? " · a plan change is scheduled" : ""}</p>`
        + receiptHtml(s.receipt)
        + (s.failed_orders || []).map((o) => `<div class="ac-row"><span class="muted">Payment of ${esc(money(Number(o.amount), o.currency))} failed</span>
          <button class="btn btn-small" data-retry="${esc(o.order_reference_code)}">Retry payment</button></div>`).join("");
      out.querySelectorAll("[data-retry]").forEach((b) => b.onclick = () => retryPayment(id, b.getAttribute("data-retry")));
    } catch (e) { out.innerHTML = `<p class="muted">Could not load the subscription: ${esc(e.message)}</p>`; }
  }

  // The tax wording of a web purchase (docs/billing-tax.md): customer, country, VAT number and the
  // reverse-charge or export line, exactly as the bridge recorded it at checkout.
  function receiptHtml(r) {
    if (!r) return "";
    const who = [r.customer?.name, r.customer?.country, r.customer?.vat_id ? `VAT ${r.customer.vat_id}` : ""].filter(Boolean).map(esc).join(" · ");
    return `<div class="ac-note"><p>Seller: ${esc(r.seller)}${who ? `<br>Customer: ${who}` : ""}</p>`
      + (r.lines || []).map((l) => `<p>${esc(l)}</p>`).join("") + `</div>`;
  }

  async function cancelCloud(id) {
    if (!confirm("Cancel your Pigeonpost Cloud plan? It will not renew; your names keep working until the end of the paid year.")) return;
    try {
      const r = await apiAction(`/v1/cloud/subscriptions/${encodeURIComponent(id)}/cancel`, { intent_id: randomString(16) });
      toast(r.paid_through ? `Canceled. Your names keep working until ${fmtDate(r.paid_through)}.` : "Canceled.");
      loadOverview();
    } catch (e) { toast("Could not cancel: " + e.message); }
  }

  async function retryPayment(id, order) {
    try {
      await apiAction(`/v1/cloud/subscriptions/${encodeURIComponent(id)}/retry`, { intent_id: randomString(16), order_reference_code: order });
      toast("Payment retried.");
      loadOverview();
    } catch (e) { toast("Could not retry the payment: " + e.message); }
  }

  function updateCard(id) {
    TS.setItem("pp_cloud_checkout", JSON.stringify({ kind: "card", intent_id: randomString(16), subscription_id: id,
      locale: /^tr\b/i.test(navigator.language || "") ? "tr" : "en" }));
    window.location.href = cloudCfg().checkoutPath || "/billing/checkout";
  }

  function changePlan(sub, plan) {
    const box = $("#ac-cloud-picker"); if (!box || !sub) return;
    const options = Object.keys(cloudCfg().plans).map(Number).filter((n) => n !== sub.names)
      .map((n) => `<option value="${n}"${n < plan.used ? " disabled" : ""}>${n} name${n === 1 ? "" : "s"} — ${esc(priceLabel(n, sub.currency))}</option>`).join("");
    box.innerHTML = `
      <div class="ac-form">
        <label>New plan <select id="ac-change-names">${options}</select></label>
        <label>When <select id="ac-change-when">
          <option value="NOW">Now</option><option value="NEXT_PERIOD">At the next renewal</option></select></label>
        <p class="ac-note">The price and currency stay in ${esc(sub.currency)}. iyzico confirms any charge for a change made now. A smaller plan needs names released first.</p>
        <div class="ac-actions ac-left"><button class="btn btn-primary" id="ac-change-go">Change plan</button>
          <button class="btn btn-secondary" id="ac-change-x">Cancel</button></div>
      </div>`;
    $("#ac-change-x").onclick = () => { box.innerHTML = ""; };
    $("#ac-change-go").onclick = async () => {
      const names = Number($("#ac-change-names").value);
      try {
        await apiAction(`/v1/cloud/subscriptions/${encodeURIComponent(sub.id)}/upgrade`, {
          intent_id: randomString(16), plan_code: PLAN_CODE(names, sub.currency), upgrade_period: $("#ac-change-when").value });
        toast("Plan change submitted.");
        box.innerHTML = "";
        loadOverview();
      } catch (e) { toast("Could not change the plan: " + e.message); }
    };
  }

  // ---- overview -----------------------------------------------------------------------------

  let overviewRequest = 0;
  let lastOverviewAt = 0;
  window.addEventListener("focus", () => {
    if (getToken() && $("#ac-subs") && Date.now() - lastOverviewAt > 5000) {
      loadOverview(); loadPostbox();
    }
  });
  async function loadOverview() {
    const box = $("#ac-subs");
    const refresh = $("#ac-handles-refresh");
    if (!box) return;
    const request = ++overviewRequest;
    lastOverviewAt = Date.now();
    if (refresh) refresh.disabled = true;
    let data;
    try { data = await apiGet("/v1/me/overview"); }
    catch (error) {
      if ($("#ac-subs") === box && request === overviewRequest) {
        box.innerHTML = `<p class="muted" role="status">Could not refresh your handles. Your registrations are saved on your account. Try Refresh handles again.</p>`;
      }
      return;
    } finally { if ($("#ac-handles-refresh") === refresh && refresh && request === overviewRequest) refresh.disabled = false; }
    if ($("#ac-subs") !== box || request !== overviewRequest) return;
    const profiles = data.billingProfiles || [];
    const complete = profiles.find(profileComplete) || null;
    overview = { hasBilling: Boolean(complete), billingIncomplete: !complete && profiles.length > 0, profileId: complete?.id || null, openCurrencies: data.openCurrencies || null,
      plan: data.cloud?.plan?.web_plan || null, subscription: data.cloud?.subscriptions?.[0] || null };
    renderSubs(data.handles || []);
    renderCloud(data.cloud);
    renderBilling(profiles);
    syncBillingGate();
    renderInvoices(data.invoices || []);
    const named = (data.handles || []).filter((h) => String(h.namespace || "").replace(/^\/+/, ""));
    dash.handles = { total: named.length,
      active: named.filter((h) => h.active ?? (!h.expires_at || h.expires_at * 1000 > Date.now())).length };
    renderDash();
    const buy = $("#ac-buy"); if (buy) buy.textContent = hasRoom() ? "Register it" : "Get it";
    resumePending();
  }

  // A name the member picked before signing in, adding billing details or buying a plan is parked
  // in pp_pending_handle. After each overview refresh, pick it back up at the next missing step.
  function resumePending() {
    const pending = SS.getItem("pp_pending_handle");
    if (!pending || !overview) return;
    const input = $("#ac-handle");
    if (input && input.value !== pending) { input.value = pending; input.dispatchEvent(new Event("input")); }
    if (hasRoom()) {
      toast(`You're all set — press “Register it” to add /${pending}.`);
      focusStep(".ac-search-card", "#ac-buy");
      SS.removeItem("pp_pending_handle");
    } else if (overview.hasBilling && !overview.subscription) {
      showPlanPicker(Math.max(1, webPlan().used + 1));
    }
  }

  // The postbox owns names, whatever paid for them; each row says where it is managed.
  function renderSubs(handles) {
    const el = $("#ac-subs"); if (!el) return;
    const rows = [];
    for (const h of handles) {
      const name = String(h.namespace || "").replace(/^\/+/, "");
      if (!name) continue;
      const active = h.active ?? (!h.expires_at || h.expires_at * 1000 > Date.now());
      const source = h.source === "apple" ? "App Store" : h.source === "google" ? "Google Play" : "Pigeonpost";
      rows.push({ name, detail: `${active ? "Active" : "Expired"} · ${source}`,
        date: h.expires_at ? `${active ? "Paid through" : "Expired on"} ${fmtDate(h.expires_at)}` : "",
        manageUrl: h.source === "apple" ? "https://apps.apple.com/account/subscriptions"
          : h.source === "google" ? "https://play.google.com/store/account/subscriptions?package=dev.pigeonpost.inbox" : "",
        source });
    }
    if (!rows.length) { el.innerHTML = `<p class="muted">No handles yet. <a href="#get-handle">Get a handle</a> to claim your first.</p>`; return; }
    el.innerHTML = rows.map((r) => `
      <div class="ac-row">
        <div><span class="k">/${esc(r.name)}</span>
          <span class="muted">${esc(r.detail)}${r.date ? " · " + esc(r.date) : ""}</span></div>
        ${r.manageUrl ? `<a class="btn btn-small" href="${esc(r.manageUrl)}" target="_blank" rel="noopener noreferrer">Manage in ${esc(r.source)}</a>`
          : `<span class="muted">Managed by Pigeonpost</span>`}
      </div>`).join("");
  }

  function renderBilling(profiles) {
    const el = $("#ac-billing"); if (!el) return;
    const p = profiles[0];
    if (p) {
      el.innerHTML = `<p>${esc(p.legal_name || p.legalName || "—")} · ${esc(p.billing_email || p.billingEmail || "")}</p>
        <button class="btn btn-small" id="ac-billing-edit">Edit</button>`;
      $("#ac-billing-edit").onclick = () => el.replaceChildren(billingForm(p));
      return;
    }
    el.replaceChildren(billingForm(null));
  }

  function billingForm(existing) {
    const wrap = document.createElement("form");
    wrap.className = "ac-form";
    wrap.innerHTML = `
      <div class="ac-grid">
        <label>Account type
          <select name="account_type">
            <option value="individual">Individual</option>
            <option value="entity">Company</option>
          </select></label>
        <label>Legal name <input name="legal_name" required></label>
        <label>Billing email <input name="billing_email" type="email" required></label>
        <label>Phone <input name="phone" placeholder="+90…"></label>
        <label>Address <input name="line1" required></label>
        <label>Address line 2 (optional) <input name="line2"></label>
        <label>City <input name="city" required></label>
        <label><span data-district-label>District (İlçe)</span> <input name="state"></label>
        <label>Postal code <input name="postal_code" required></label>
        <label>Country <input name="country" placeholder="Türkiye / TR" required></label>
        <label class="ac-individual">TCKN (Turkish identity number)
          <input name="identity_number" inputmode="numeric" maxlength="11" pattern="[0-9]{11}" title="Enter your 11 digit TCKN.">
          <small class="muted">Required for billing addresses in Türkiye.</small></label>
        <label class="ac-entity">Company name <input name="entity_name"></label>
        <label class="ac-entity">Tax ID / VAT (VKN in Türkiye) <input name="tax_id"></label>
        <p class="ac-note ac-entity">EU or UK company: enter the VAT number with its country prefix, for example DE123456789 or GB123456789. We check it with VIES or HMRC and sell without VAT (reverse charge).</p>
        <label class="ac-entity">Tax office <input name="tax_office"></label>
      </div>
      <button class="btn btn-primary" type="submit">Save billing details</button>
      <span class="ac-msg"></span>`;
    if (existing) {
      const values = { ...existing, ...existing.address };
      for (const field of wrap.elements) {
        const value = values[field.name];
        if (value != null && typeof value !== "object") field.value = value;
      }
    }
    const field = (name) => wrap.elements.namedItem(name);
    const updateRequirements = () => {
      const entity = field("account_type").value === "entity";
      const country = field("country").value.replace(/[ıİ]/g, "i").normalize("NFKD")
        .replace(/[\u0300-\u036f]/g, "").toLowerCase().replace(/[^a-z0-9]/g, "");
      const turkish = !country || ["tr", "tur", "turkey", "turkiye"].includes(country);
      for (const [selector, shown] of [[".ac-entity", entity], [".ac-individual", !entity]]) {
        wrap.querySelectorAll(selector).forEach((label) => {
          label.style.display = shown ? "" : "none";
          label.querySelectorAll("input").forEach((input) => { input.disabled = !shown; });
        });
      }
      field("identity_number").required = turkish && !entity;
      field("tax_id").required = turkish && entity;
      if (turkish) {
        field("tax_id").pattern = "[0-9]{10,11}";
        field("tax_id").title = "Enter a 10 digit VKN or 11 digit TCKN.";
      } else {
        field("tax_id").removeAttribute("pattern");
        field("tax_id").removeAttribute("title");
      }
      field("state").required = turkish;
      wrap.querySelector("[data-district-label]").textContent = turkish ? "District (İlçe)" : "State / region (optional)";
    };
    field("account_type").onchange = updateRequirements;
    field("country").oninput = updateRequirements;
    field("country").onchange = updateRequirements;
    updateRequirements();
    const save = wrap.querySelector('[type="submit"]');
    wrap.onsubmit = async (e) => {
      e.preventDefault();
      if (save.disabled) return;
      wrap.querySelectorAll("input").forEach((input) => { input.value = input.value.trim(); });
      updateRequirements();
      if (!wrap.reportValidity()) return;
      const fd = Object.fromEntries(new FormData(wrap).entries());
      const payload = {
        account_type: fd.account_type, legal_name: fd.legal_name, billing_email: fd.billing_email,
        phone: fd.phone, address: { line1: fd.line1, line2: fd.line2 || null, city: fd.city,
          state: fd.state || null, postal_code: fd.postal_code, country: fd.country },
      };
      if (fd.account_type === "entity") {
        Object.assign(payload, { entity_name: fd.entity_name, tax_id: fd.tax_id, tax_office: fd.tax_office, identity_number: null });
      } else {
        Object.assign(payload, { identity_number: fd.identity_number || null, entity_name: null, tax_id: null, tax_office: null });
      }
      save.disabled = true;
      wrap.querySelector(".ac-msg").textContent = "Saving…";
      try {
        await apiAction(existing?.id ? `/v1/billing/profiles/${encodeURIComponent(existing.id)}` : "/v1/billing/profiles",
          payload, existing?.id ? "PATCH" : "POST");
        await loadOverview();
      }
      catch (err) { wrap.querySelector(".ac-msg").textContent = "Could not save: " + err.message; }
      finally { save.disabled = false; }
    };
    return wrap;
  }

  function renderInvoices(invoices) {
    const el = $("#ac-invoices"); if (!el) return;
    if (!invoices.length) { el.innerHTML = `<p class="muted">No invoices yet.</p>`; return; }
    el.innerHTML = invoices.map((i) => `
      <div class="ac-row"><span class="muted">${esc(String(i.issued_at || i.created_at || "").slice(0, 10))} · ${esc(i.number || "")} · ${esc(i.status || "")}</span>
      <span>${esc(i.total != null ? money(i.total, i.currency || "USD") : i.amount != null ? money(i.amount, i.currency || "USD") : "")}</span>
      ${i.id ? `<button class="btn btn-small" data-invoice="${esc(i.id)}">PDF</button>` : ""}</div>`).join("");
    el.querySelectorAll("[data-invoice]").forEach((b) => b.onclick = async () => {
      try {
        const { url } = await apiGet(`/v1/billing/invoices/${encodeURIComponent(b.getAttribute("data-invoice"))}/download`);
        if (/^https:\/\//.test(url)) window.open(url, "_blank", "noopener");
      } catch (e) { toast("Could not open the invoice: " + e.message); }
    });
  }

  // ---- helpers ------------------------------------------------------------------------------

  function money(a, c) { try { return new Intl.NumberFormat(undefined, { style: "currency", currency: c }).format(a); } catch { return `${a} ${c}`; } }
  function esc(s) { return String(s == null ? "" : s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c])); }
  function toast(msg) {
    let t = $("#ac-toast");
    if (!t) { t = document.createElement("div"); t.id = "ac-toast"; document.body.appendChild(t); }
    t.textContent = msg; t.classList.add("on");
    clearTimeout(t._timer); t._timer = setTimeout(() => t.classList.remove("on"), 4000);
  }

  document.addEventListener("DOMContentLoaded", async () => {
    await completeLoginIfReturning();
    render();
    if (window.location.hash !== "#delete-account" && await confirmCheckoutIfReturning() && getToken()) loadOverview();
  });
  // Inside the shell a hash only changes the section. Entering or leaving the standalone
  // deletion page (`#delete-account`, linked from the stores) redraws the whole page.
  window.addEventListener("hashchange", () => {
    if (window.location.hash === "#delete-account" || mode === "deletion") { render(); return; }
    if (mode === "member" && $("#ac-app")) showRoute(hashRoute() || "overview", { focus: true });
  });
})();
