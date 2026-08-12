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

  // localStorage, not sessionStorage: the PKCE verifier and session must survive the full-page
  // redirect out to Keycloak and back. sessionStorage is meant to persist across that, but in
  // practice some browsers drop it across a cross-site OAuth round-trip, which loses the verifier
  // and makes every exchange fail invalid_grant — a silent sign-in loop. localStorage is durable.
  const SS = window.localStorage;
  const getToken = () => SS.getItem("pp_session");
  const setToken = (t) => SS.setItem("pp_session", t);
  const clearToken = () => SS.removeItem("pp_session");

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

  async function login() {
    if (!authReady) {
      toast("Sign-in isn't configured yet. The pigeonpost-prod realm web client is being set up.");
      return;
    }
    const verifier = randomString(48);
    const state = randomString(12);
    SS.setItem("pp_pkce", verifier);
    SS.setItem("pp_state", state);
    const challenge = await sha256b64url(verifier);
    const redirect = window.location.origin + (cfg.oidc.redirectPath || "/account");
    const url = `${cfg.oidc.issuer}/protocol/openid-connect/auth`
      + `?client_id=${encodeURIComponent(cfg.oidc.clientId)}`
      + `&response_type=code&scope=${encodeURIComponent(cfg.oidc.scope || "openid email")}`
      + `&redirect_uri=${encodeURIComponent(redirect)}`
      + `&code_challenge=${challenge}&code_challenge_method=S256&state=${state}`;
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
      } else {
        // Surface the reason rather than looping silently — this is what turned a real bug into a
        // mystery on the first sign-in attempt.
        toast("Could not complete sign-in" + (body.error ? ": " + body.error : "."));
      }
    } catch (_) {
      toast("Could not reach the sign-in service. Try again in a moment.");
    }
    SS.removeItem("pp_pkce"); SS.removeItem("pp_state");
    strip();
    return Boolean(getToken());
  }

  function logout() { clearToken(); render(); }

  // ---- API ----------------------------------------------------------------------------------

  async function apiGet(path) {
    const res = await fetch(api(path), { headers: { authorization: `Bearer ${getToken()}`, accept: "application/json" } });
    if (res.status === 401) { clearToken(); render(); throw new Error("session expired"); }
    return res.json();
  }
  async function apiPost(path, body) {
    const res = await fetch(api(path), {
      method: "POST",
      headers: { authorization: `Bearer ${getToken()}`, "content-type": "application/json" },
      body: JSON.stringify(body || {}),
    });
    if (res.status === 401) { clearToken(); render(); throw new Error("session expired"); }
    return res.json();
  }

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
        ${authReady ? "" : `<p class="ac-note">Sign-in is being finalized. Handle search below works now.</p>`}
      </div>
      ${searchBlock()}`;
    $("#ac-signin").onclick = login;
    $("#ac-register").onclick = register;
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
      <div class="ac-card"><h3>Billing details</h3><div id="ac-billing"><p class="muted">Loading…</p></div></div>
      <div class="ac-card"><h3>Payment method</h3><div id="ac-pay"><p class="muted">Loading…</p></div></div>
      <div class="ac-card"><h3>Invoices</h3><div id="ac-invoices"><p class="muted">Loading…</p></div></div>`;
    $("#ac-logout").onclick = logout;
    wireSearch(true);
    loadOverview();
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
      buyHandle(current.name);
    };
  }

  async function buyHandle(name) {
    try {
      const body = await apiPost("/v1/checkout", { handle: name });
      if (body.checkoutUrl) { window.location.href = body.checkoutUrl; return; }
      toast(body.error || "Could not start checkout.");
    } catch (e) { if (e.message !== "session expired") toast("Checkout failed. Try again shortly."); }
  }

  async function loadOverview() {
    let data;
    try { data = await apiGet("/v1/me/overview"); } catch { return; }
    renderSubs(data.subscriptions || []);
    renderBilling(data.billingProfiles || []);
    renderPay(data.paymentMethods || []);
    renderInvoices(data.invoices || []);
    // Finish a pending purchase started before sign-in.
    const pending = SS.getItem("pp_pending_handle");
    if (pending) { SS.removeItem("pp_pending_handle"); }
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
    try { await apiPost(`/v1/subscriptions/${encodeURIComponent(id)}/cancel`, {}); loadOverview(); }
    catch (e) { if (e.message !== "session expired") toast("Could not cancel."); }
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
      try { await apiPost("/v1/billing/profiles", payload); loadOverview(); }
      catch (err) { if (err.message !== "session expired") wrap.querySelector(".ac-msg").textContent = "Could not save."; }
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
      const body = await apiPost("/v1/billing/payment-methods/setup", {});
      if (body.setupUrl) { window.location.href = body.setupUrl; return; }
      toast(body.error || "Could not start card setup.");
    } catch (e) { if (e.message !== "session expired") toast("Could not add a card."); }
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

  document.addEventListener("DOMContentLoaded", async () => {
    await completeLoginIfReturning();
    render();
  });
})();
