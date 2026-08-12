// Pigeonpost account — the in-page member area.
//
// Auth is Authorization Code + PKCE against the pigeonpost-prod realm; the adapter at /api exchanges
// the code and proxies member operations to MASAAS with the customer's token. MASAAS hosts the card
// capture, so no payment script or key runs here.

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

  // localStorage, not sessionStorage: the PKCE verifier and session must survive the full-page
  // redirect out to Keycloak and back. sessionStorage is meant to persist across that, but in
  // practice some browsers drop it across a cross-site OAuth round-trip, which loses the verifier
  // and makes every exchange fail invalid_grant — a silent sign-in loop. localStorage is durable.
  const SS = window.localStorage;
  const getToken = () => SS.getItem("pp_session");
  const setToken = (t) => SS.setItem("pp_session", t);
  const clearToken = () => SS.removeItem("pp_session");

  // "Remember me" keeps a refresh token so the access token can be silently renewed for up to 30
  // days (Keycloak offline session), instead of forcing a fresh sign-in when the short-lived access
  // token expires. Only stored when the user opts in.
  const getRefresh = () => SS.getItem("pp_refresh");
  const setRefresh = (t) => (t ? SS.setItem("pp_refresh", t) : SS.removeItem("pp_refresh"));
  const clearRefresh = () => SS.removeItem("pp_refresh");
  const wantsRemember = () => SS.getItem("pp_remember") === "1";
  function signOut() {
    clearToken();
    clearRefresh();
    SS.removeItem("pp_remember");
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

  async function login(remember, kcAction) {
    if (!authReady) {
      toast("Sign-in isn't configured yet. The pigeonpost-prod realm web client is being set up.");
      return;
    }
    // Default to remembering unless the caller explicitly opted out, so the pending-purchase resume
    // path (which calls login() with no arg) keeps whatever the sign-in card already recorded.
    const keep = remember === undefined ? wantsRemember() : !!remember;
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
      + (kcAction ? `&kc_action=${encodeURIComponent(kcAction)}` : "");
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
    const strip = () => history.replaceState({}, "", cfg.oidc.redirectPath || "/account");
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
        setToken(body.session);
        // Keep the refresh token only when the user asked to be remembered. Keycloak returns one
        // whenever offline_access was granted, but we honour the checkbox regardless.
        if (wantsRemember() && body.refresh) setRefresh(body.refresh);
        else clearRefresh();
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
    const refresh = getRefresh();
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
          if (res.ok && body.session) {
            setToken(body.session);
            if (body.refresh) setRefresh(body.refresh); // Keycloak rotates refresh tokens
            return true;
          }
          clearRefresh(); // refresh token is dead/expired — stop trying it
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
  const apiAction = (path, body) => apiFetch(path, { method: "POST", body: JSON.stringify(body || {}), keepSession: true }, false);

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

  // ---- render -------------------------------------------------------------------------------

  const root = () => $("#account-root");

  function render() {
    const el = root();
    if (!el) return;
    if (!getToken()) return renderSignedOut(el);
    renderMember(el);
  }

  function renderSignedOut(el) {
    el.innerHTML = `
      <div class="ac-card ac-center">
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
    el.innerHTML = `
      <div class="ac-head">
        <h2>Your account</h2>
        <button class="btn btn-small" id="ac-logout">Sign out</button>
      </div>
      ${searchBlock()}
      <div class="ac-card"><h3>Your handles</h3><div id="ac-subs"><p class="muted">Loading…</p></div></div>
      <div class="ac-card">
        <h3>Connect an AI agent</h3>
        <p class="muted">Give a Claude or ChatGPT agent a hosted Pigeonpost inbox — it sends and
          receives messages through an MCP connector tied to this account.</p>
        <div id="ac-pb-list"><p class="muted">Loading…</p></div>
        <div class="ac-actions ac-left">
          <button class="btn btn-secondary" id="ac-pb-create">Create an inbox</button>
          <button class="btn btn-secondary" id="ac-pb-key">Create connector key</button>
        </div>
        <div id="ac-pb-out"></div>
        <h3 class="ac-mt">Connector keys</h3>
        <p class="muted">Each key lets an agent act as this account. Revoke one to cut off a device or agent.</p>
        <div id="ac-pb-keys"><p class="muted">Loading…</p></div>
      </div>
      <div class="ac-card"><h3>Billing details</h3><div id="ac-billing"><p class="muted">Loading…</p></div></div>
      <div class="ac-card"><h3>Payment method</h3><div id="ac-pay"><p class="muted">Loading…</p></div></div>
      <div class="ac-card">
        <h3>Security</h3>
        <p class="muted">Add two-factor authentication with an app like Google Authenticator, Authy, or 1Password.</p>
        <button class="btn btn-secondary" id="ac-2fa">Set up two-factor authentication</button>
        <p class="ac-note ac-mt">Opens a single screen with a QR code to scan, then brings you back here.</p>
      </div>
      <div class="ac-card"><h3>Invoices</h3><div id="ac-invoices"><p class="muted">Loading…</p></div></div>`;
    $("#ac-logout").onclick = logout;
    $("#ac-2fa").onclick = setupTotp;
    $("#ac-pb-create").onclick = pbCreateInbox;
    $("#ac-pb-key").onclick = pbRevealKey;
    wireSearch(true);
    loadOverview();
    loadPostbox();
    loadKeys();
  }

  // ---- hosted postbox (MCP connector) --------------------------------------------------------
  // The postbox is a separate origin; it accepts this member token (validated against the realm)
  // and resolves it to an account. CORS on the postbox allows pigeonpost.dev.

  const POSTBOX = "https://postbox.pigeonpost.dev";
  async function pbFetch(path, opts) {
    const o = opts || {};
    const res = await fetch(POSTBOX + path, {
      method: o.method || "GET",
      headers: Object.assign(
        { authorization: `Bearer ${getToken()}`, accept: "application/json" },
        o.body ? { "content-type": "application/json" } : {},
      ),
      body: o.body,
    });
    const body = await res.json().catch(() => ({}));
    if (!res.ok) {
      const e = new Error(body.detail || body.error || `postbox ${res.status}`);
      e.status = res.status;
      throw e;
    }
    return body;
  }

  async function loadPostbox() {
    const box = $("#ac-pb-list"); if (!box) return;
    try {
      const { identities } = await pbFetch("/v1/identities");
      box.innerHTML = (identities.length
        ? identities.map((i) => `<div class="ac-row">
            <span class="mono">${esc(i.address)}</span>
            <span class="muted">${esc(i.label || "")}</span>
            <span class="ac-row-actions">
              <button class="btn btn-small" data-pbview="${esc(i.address)}">View inbox</button>
              <button class="btn btn-small" data-pbdel="${esc(i.address)}">Delete</button>
            </span>
          </div>`).join("")
        : `<p class="muted">No inboxes yet — create one for your agent.</p>`)
        + `<div id="ac-pb-msgs"></div>`;
      box.querySelectorAll("[data-pbview]").forEach((b) => b.onclick = () => pbViewInbox(b.getAttribute("data-pbview")));
      box.querySelectorAll("[data-pbdel]").forEach((b) => b.onclick = () => pbDeleteInbox(b.getAttribute("data-pbdel")));
    } catch (e) {
      box.innerHTML = `<p class="muted">${e.status === 401 ? "Sign in again to manage inboxes." : "Could not reach the postbox."}</p>`;
    }
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

  function searchBlock() {
    const price = cfg.price ? `${money(cfg.price.amount, cfg.price.currency)}/${cfg.price.interval}` : "$5/year";
    return `
      <div class="ac-card ac-search-card">
        <h3>Get a handle</h3>
        <div class="ac-search">
          <div class="ac-field"><span class="slash">/</span>
            <input id="ac-handle" type="text" placeholder="yourname" autocomplete="off"
              autocapitalize="none" spellcheck="false" maxlength="32" aria-label="handle"></div>
          <button class="btn btn-primary" id="ac-buy" disabled>Get it — ${price}</button>
        </div>
        <p class="hs" id="ac-handle-status" aria-live="polite"></p>
      </div>`;
  }

  function wireSearch(signedIn) {
    const input = $("#ac-handle"); if (!input) return;
    const status = $("#ac-handle-status");
    const buy = $("#ac-buy");
    let current = { ok: false };
    input.oninput = () => {
      const v = validate(input.value); current = v;
      status.className = "hs";
      if (v.silent) { status.textContent = ""; buy.disabled = true; return; }
      if (!v.ok) { status.classList.add("bad"); status.textContent = "✗ " + v.reason; buy.disabled = true; return; }
      status.classList.add("ok"); status.textContent = `✓ /${v.name} — availability confirmed at checkout`;
      buy.disabled = false;
    };
    buy.onclick = () => {
      if (!current.ok) return;
      if (!signedIn || !getToken()) { SS.setItem("pp_pending_handle", current.name); login(); return; }
      startPurchase(current.name);
    };
  }

  // Make sure we know the member's billing/card state before gating a purchase. loadOverview sets
  // `overview`; if the buy button is pressed before that first load lands, fetch it now.
  async function ensureOverview() {
    if (!overview) await loadOverview();
    return overview || { hasBilling: false, hasCard: false };
  }

  // A purchase needs billing details, then a card, then checkout. Rather than let the backend reject
  // an incomplete purchase with a raw error, walk the member to whichever step is missing — billing
  // first, card second — and remember the handle so the flow resumes once each step is done.
  async function startPurchase(name) {
    const ov = await ensureOverview();
    if (!ov.hasBilling) {
      SS.setItem("pp_pending_handle", name);
      toast(`Almost there — add your billing details to claim /${name}.`);
      focusStep("#ac-billing", 'input[name="legal_name"]');
      return;
    }
    if (!ov.hasCard) {
      SS.setItem("pp_pending_handle", name);
      toast(`One more step — add a payment card to claim /${name}.`);
      focusStep("#ac-pay", "#ac-addcard");
      return;
    }
    SS.removeItem("pp_pending_handle");
    buyHandle(name);
  }

  // Scroll a card into view and put the cursor on the field/button that needs attention.
  function focusStep(cardSelector, targetSelector) {
    const card = $(cardSelector);
    if (card) card.scrollIntoView({ behavior: "smooth", block: "center" });
    const target = card && $(targetSelector, card);
    if (target) {
      card.classList.add("ac-attn");
      setTimeout(() => card.classList.remove("ac-attn"), 1600);
      setTimeout(() => { try { target.focus({ preventScroll: true }); } catch (_) { target.focus(); } }, 300);
    }
  }

  async function buyHandle(name) {
    try {
      const body = await apiAction("/v1/checkout", { handle: name });
      if (body.checkoutUrl) { window.location.href = body.checkoutUrl; return; }
      toast(body.error || "Could not start checkout.");
    } catch (e) {
      // A 401 that survived the refresh-and-retry means the session token is genuinely stale (expired,
      // or minted before the current realm claims). Tell the user how to fix it rather than showing a
      // cryptic "unauthorized"; anything else surfaces the real backend reason.
      if (e.status === 401) toast("Your session has expired — sign out and sign back in, then try again.");
      else toast("Checkout couldn't start: " + e.message);
    }
  }

  async function loadOverview() {
    let data;
    try { data = await apiGet("/v1/me/overview"); } catch { return; }
    const profiles = data.billingProfiles || [];
    const methods = data.paymentMethods || [];
    overview = { hasBilling: profiles.length > 0, hasCard: methods.length > 0 };
    renderSubs(data.subscriptions || []);
    renderBilling(profiles);
    renderPay(methods);
    renderInvoices(data.invoices || []);
    resumePending();
  }

  // A purchase the member started before completing billing/card (or before signing in) is parked in
  // pp_pending_handle. After each overview refresh, pick it back up: advance to the next missing step,
  // or — when everything's in place — pre-fill the handle and put the cursor on the buy button so the
  // claim is one deliberate click, not a surprise redirect.
  function resumePending() {
    const pending = SS.getItem("pp_pending_handle");
    if (!pending || !overview) return;
    if (!overview.hasBilling || !overview.hasCard) { startPurchase(pending); return; }
    const input = $("#ac-handle");
    if (input) { input.value = pending; input.dispatchEvent(new Event("input")); }
    toast(`You're all set — press “Get it” to claim /${pending}.`);
    focusStep(".ac-search-card", "#ac-buy");
  }

  function renderSubs(subs) {
    const el = $("#ac-subs"); if (!el) return;
    if (!subs.length) { el.innerHTML = `<p class="muted">No handles yet. Search above to claim one.</p>`; return; }
    el.innerHTML = subs.map((s) => `
      <div class="ac-row">
        <div><span class="k">/${esc(s.handle || "—")}</span>
          <span class="muted">${esc(s.status)}${s.renewsAt ? " · renews " + esc(String(s.renewsAt).slice(0, 10)) : ""}</span></div>
        <button class="btn btn-small" data-cancel="${esc(s.id)}">Cancel</button>
      </div>`).join("");
    el.querySelectorAll("[data-cancel]").forEach((b) => b.onclick = () => cancelSub(b.getAttribute("data-cancel")));
  }
  async function cancelSub(id) {
    if (!confirm("Cancel this handle? It stops resolving at the end of the paid term; the key address keeps working.")) return;
    try { await apiAction(`/v1/subscriptions/${encodeURIComponent(id)}/cancel`, {}); loadOverview(); }
    catch (e) { toast("Could not cancel: " + e.message); }
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
        <label>City <input name="city" required></label>
        <label>Postal code <input name="postal_code" required></label>
        <label>Country <input name="country" placeholder="TR" required></label>
        <label class="ac-entity">Company name <input name="entity_name"></label>
        <label class="ac-entity">Tax ID / VAT <input name="tax_id"></label>
        <label class="ac-entity">Tax office <input name="tax_office"></label>
      </div>
      <button class="btn btn-primary" type="submit">Save billing details</button>
      <span class="ac-msg"></span>`;
    if (existing) {
      for (const [k, v] of Object.entries(existing)) {
        const f = wrap.querySelector(`[name="${k}"]`); if (f && typeof v !== "object") f.value = v;
      }
    }
    const type = wrap.querySelector('[name="account_type"]');
    const toggleEntity = () => wrap.querySelectorAll(".ac-entity").forEach((e) => e.style.display = type.value === "entity" ? "" : "none");
    type.onchange = toggleEntity; toggleEntity();
    wrap.onsubmit = async (e) => {
      e.preventDefault();
      const fd = Object.fromEntries(new FormData(wrap).entries());
      const payload = {
        account_type: fd.account_type, legal_name: fd.legal_name, billing_email: fd.billing_email,
        phone: fd.phone, address: { line1: fd.line1, city: fd.city, postal_code: fd.postal_code, country: fd.country },
      };
      if (fd.account_type === "entity") Object.assign(payload, { entity_name: fd.entity_name, tax_id: fd.tax_id, tax_office: fd.tax_office });
      wrap.querySelector(".ac-msg").textContent = "Saving…";
      try { await apiAction("/v1/billing/profiles", payload); loadOverview(); }
      catch (err) { wrap.querySelector(".ac-msg").textContent = "Could not save: " + err.message; }
    };
    return wrap;
  }

  function renderPay(methods) {
    const el = $("#ac-pay"); if (!el) return;
    const rows = methods.map((m) => `<div class="ac-row"><span class="muted">${esc(m.brand || m.type || "card")} ···· ${esc(m.last4 || m.last_four || "")}</span></div>`).join("");
    el.innerHTML = `${rows || `<p class="muted">No card on file.</p>`}
      <button class="btn btn-small" id="ac-addcard">Add a card</button>`;
    $("#ac-addcard").onclick = addCard;
  }
  async function addCard() {
    try {
      const body = await apiAction("/v1/billing/payment-methods/setup", {});
      if (body.setupUrl) {
        // Stash the session so we can finalise the card on return even if MASAAS sends the browser
        // back to a bare /account with no completion params (which is what it does today).
        if (body.sessionId) SS.setItem("pp_card_session", body.sessionId);
        if (body.provider) SS.setItem("pp_card_provider", body.provider);
        window.location.href = body.setupUrl;
        return;
      }
      toast(body.error || "Could not start card setup.");
    } catch (e) { toast("Could not add a card: " + e.message); }
  }

  function renderInvoices(invoices) {
    const el = $("#ac-invoices"); if (!el) return;
    if (!invoices.length) { el.innerHTML = `<p class="muted">No invoices yet.</p>`; return; }
    el.innerHTML = invoices.map((i) => `
      <div class="ac-row"><span class="muted">${esc(String(i.created_at || i.issued_at || "").slice(0, 10))} · ${esc(i.status || "")}</span>
      <span>${i.amount != null ? money(i.amount, i.currency || "USD") : ""}</span></div>`).join("");
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

  // Coming back from the hosted card page, finalise the payment method before the account renders, so
  // the card shows immediately instead of "No card on file". The session id comes from whatever MASAAS
  // appended (matching the shared backend's params) or, failing that, the token we stashed on the way
  // out. Runs only when there's something to complete.
  async function completeCardSetupIfReturning() {
    if (!getToken()) return;
    const params = new URLSearchParams(window.location.search);
    const stashed = SS.getItem("pp_card_session");
    const returned = params.get("payment_setup") === "success" || params.get("added") === "card";
    if (!stashed && !returned) return;
    const sessionId = params.get("payment_setup_session") || params.get("token") || params.get("session_id") || stashed;
    const provider = params.get("provider") || SS.getItem("pp_card_provider") || undefined;
    SS.removeItem("pp_card_session"); SS.removeItem("pp_card_provider");
    // Strip the card-setup params so a reload doesn't retry a spent session.
    ["payment_setup", "payment_setup_session", "token", "session_id", "provider", "added"].forEach((k) => params.delete(k));
    const q = params.toString();
    history.replaceState({}, "", (cfg.oidc.redirectPath || "/account") + (q ? "?" + q : ""));
    if (!sessionId) return; // nothing to finalise; the overview will reflect whatever MASAAS has
    try {
      await apiAction("/v1/billing/payment-methods/complete", { sessionId, provider });
      toast("Card saved.");
    } catch (e) {
      toast("Could not finish saving the card: " + e.message);
    }
  }

  document.addEventListener("DOMContentLoaded", async () => {
    await completeLoginIfReturning();
    await completeCardSetupIfReturning();
    render();
  });
})();
