// Pigeonpost handle store — client logic.
//
// Preview-safe: with no adapter configured, every network step degrades to an honest
// explanation rather than a broken call. When config.js is filled in, the same code paths become live.

(function () {
  "use strict";
  const cfg = window.PIGEONPOST_STORE || {};
  const reserved = window.PIGEONPOST_RESERVED;
  const live = Boolean(cfg.adapterBaseUrl);

  const $ = (sel, root) => (root || document).querySelector(sel);
  const grammar = new RegExp(cfg.handle && cfg.handle.pattern ? cfg.handle.pattern : "^[a-z0-9]+$");
  const MIN = (cfg.handle && cfg.handle.min) || 3;
  const MAX = (cfg.handle && cfg.handle.max) || 32;

  // ---- validation ---------------------------------------------------------------------------

  function validate(nameRaw) {
    const name = String(nameRaw || "").trim().toLowerCase();
    if (!name) return { ok: false, silent: true };
    if (/[^a-z0-9]/.test(name)) {
      return { ok: false, reason: "letters a–z and digits 0–9 only — no spaces, @, dots, or dashes" };
    }
    if (name.length < MIN) return { ok: false, reason: `at least ${MIN} characters` };
    if (name.length > MAX) return { ok: false, reason: `at most ${MAX} characters` };
    if (!grammar.test(name)) return { ok: false, reason: "not a valid handle" };
    const r = reserved && reserved.reason(name);
    if (r) return { ok: false, reason: r };
    return { ok: true, name };
  }

  // ---- store page ---------------------------------------------------------------------------

  function initStore() {
    const input = $("#handle-input");
    const status = $("#handle-status");
    const buy = $("#buy-btn");
    const priceEl = $("#price");
    if (!input) return;

    if (cfg.price) {
      priceEl.textContent = `${formatMoney(cfg.price.amount, cfg.price.currency)}/${cfg.price.interval}`;
    }

    let checkTimer = null;
    let current = { ok: false };

    function render(v, availability) {
      current = v;
      status.className = "hs";
      if (v.silent) { status.textContent = ""; buy.disabled = true; return; }
      if (!v.ok) {
        status.classList.add("bad");
        status.textContent = "✗ " + v.reason;
        buy.disabled = true;
        return;
      }
      if (availability === "checking") {
        status.classList.add("wait");
        status.textContent = "checking availability…";
        buy.disabled = true;
        return;
      }
      if (availability === "taken") {
        status.classList.add("bad");
        status.textContent = "✗ already taken";
        buy.disabled = true;
        return;
      }
      if (availability === "unknown") {
        status.classList.add("ok");
        status.textContent = `✓ /${v.name} looks valid — availability confirmed at checkout`;
        buy.disabled = false;
        return;
      }
      status.classList.add("ok");
      status.textContent = `✓ /${v.name} is available`;
      buy.disabled = false;
    }

    input.addEventListener("input", () => {
      const v = validate(input.value);
      render(v, null);
      clearTimeout(checkTimer);
      if (v.ok) {
        render(v, "checking");
        checkTimer = setTimeout(() => checkAvailability(v.name).then((state) => {
          if (validate(input.value).name === v.name) render(v, state);
        }), 350);
      }
    });

    buy.addEventListener("click", () => {
      if (!current.ok) return;
      startCheckout(current.name);
    });
  }

  async function checkAvailability(name) {
    if (!live) return "unknown";
    try {
      const res = await fetch(`${cfg.adapterBaseUrl}/v1/handles/${encodeURIComponent(name)}/availability`, {
        headers: { accept: "application/json" },
      });
      if (!res.ok) return "unknown";
      const body = await res.json();
      return body.available ? "available" : "taken";
    } catch (_) {
      return "unknown";
    }
  }

  async function startCheckout(name) {
    if (!live) {
      showModal(
        "Not selling yet",
        `The store is built and this is exactly where checkout runs. It is not connected to a ` +
        `payment gateway yet, so <strong>/${name}</strong> cannot be purchased today. When the ` +
        `MASAAS billing tenant is wired in <code>config.js</code>, this button opens the hosted card form.`
      );
      return;
    }
    const token = getSession();
    if (!token) { login(name); return; }
    try {
      const res = await fetch(`${cfg.adapterBaseUrl}/v1/checkout/session`, {
        method: "POST",
        headers: { "content-type": "application/json", authorization: `Bearer ${token}` },
        body: JSON.stringify({
          handle: name,
          packageId: cfg.product.packageId,
          returnUrl: window.location.origin + "/account",
          cancelUrl: window.location.origin + "/",
        }),
      });
      const body = await res.json();
      // MASAAS hosts the card capture. The adapter returns its hosted URL; we redirect to it. No
      // payment-gateway script or key ever runs in this page.
      if (body.checkoutUrl) { window.location.href = body.checkoutUrl; return; }
      showModal("Checkout unavailable", body.error || "The checkout session could not be created.");
    } catch (e) {
      showModal("Checkout failed", "Could not reach the store backend. Try again shortly.");
    }
  }

  // ---- account / subscription page ----------------------------------------------------------

  function initAccount() {
    const root = $("#account-root");
    if (!root) return;
    handleOidcReturn();
    const token = getSession();
    if (!token) { renderSignedOut(root); return; }
    if (!live) { renderPreviewAccount(root); return; }
    loadSubscriptions(root, token);
  }

  function renderSignedOut(root) {
    root.innerHTML = `
      <div class="card center">
        <h2>Sign in</h2>
        <p>Manage the handles on your account and your yearly subscription.</p>
        <button class="btn btn-primary" id="signin-btn">Sign in with your provider</button>
      </div>`;
    $("#signin-btn").addEventListener("click", () => login(null));
  }

  function renderPreviewAccount(root) {
    root.innerHTML = `
      <div class="card">
        <h2>Your handles</h2>
        <p class="muted">This is the subscription page. Once the store is connected to the billing
        tenant, it lists the handles on your account, each one's renewal date, and lets you cancel or
        change a plan. It is running in preview mode now, so there is nothing to show yet.</p>
        <div class="sub-row placeholder">
          <div><span class="k">/yourhandle</span><span class="muted">renews — · —</span></div>
          <div><span class="pill">preview</span></div>
        </div>
      </div>`;
  }

  async function loadSubscriptions(root, token) {
    root.innerHTML = `<div class="card"><p class="muted">Loading your subscriptions…</p></div>`;
    try {
      const res = await fetch(`${cfg.adapterBaseUrl}/v1/subscriptions`, {
        headers: { authorization: `Bearer ${token}`, accept: "application/json" },
      });
      const body = await res.json();
      const subs = body.subscriptions || [];
      if (!subs.length) {
        root.innerHTML = `<div class="card"><h2>Your handles</h2>
          <p class="muted">No handles yet. <a href="/">Claim one →</a></p></div>`;
        return;
      }
      root.innerHTML = `<div class="card"><h2>Your handles</h2>${subs.map(subRow).join("")}</div>`;
      root.querySelectorAll("[data-cancel]").forEach((b) =>
        b.addEventListener("click", () => cancelSub(b.getAttribute("data-cancel"), token)));
    } catch (e) {
      root.innerHTML = `<div class="card"><p class="bad">Could not load your subscriptions.</p></div>`;
    }
  }

  function subRow(s) {
    return `<div class="sub-row">
      <div><span class="k">/${escapeHtml(s.handle)}</span>
      <span class="muted">${s.status} · renews ${escapeHtml(s.renewsAt || "—")}</span></div>
      <div><button class="btn btn-small" data-cancel="${escapeHtml(s.id)}">Cancel</button></div>
    </div>`;
  }

  async function cancelSub(id, token) {
    if (!confirm("Cancel this handle's subscription? It stops resolving at the end of the paid term; the underlying key address keeps working.")) return;
    await fetch(`${cfg.adapterBaseUrl}/v1/subscriptions/${encodeURIComponent(id)}/cancel`, {
      method: "POST", headers: { authorization: `Bearer ${token}` },
    });
    location.reload();
  }

  // ---- auth (OIDC redirect, minimal) --------------------------------------------------------

  function login(pendingHandle) {
    if (!cfg.oidc || !cfg.oidc.issuer || !cfg.oidc.clientId) {
      showModal("Sign-in not wired", "The store's OIDC client is not configured yet. It points at the sealunit realm once the tenant exists.");
      return;
    }
    if (pendingHandle) sessionStorage.setItem("pp_pending_handle", pendingHandle);
    const redirect = window.location.origin + (cfg.oidc.redirectPath || "/account");
    const url = `${cfg.oidc.issuer}/protocol/openid-connect/auth`
      + `?client_id=${encodeURIComponent(cfg.oidc.clientId)}`
      + `&response_type=code&scope=openid%20email`
      + `&redirect_uri=${encodeURIComponent(redirect)}`;
    window.location.href = url;
  }

  function handleOidcReturn() {
    const params = new URLSearchParams(window.location.search);
    const code = params.get("code");
    if (!code || !live) return;
    // The adapter exchanges the code for a session; browser never sees the client secret.
    fetch(`${cfg.adapterBaseUrl}/v1/auth/exchange`, {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ code, redirectUri: window.location.origin + (cfg.oidc.redirectPath || "/account") }),
    }).then((r) => r.json()).then((b) => {
      if (b.session) { setSession(b.session); history.replaceState({}, "", cfg.oidc.redirectPath || "/account"); location.reload(); }
    }).catch(() => {});
  }

  const getSession = () => sessionStorage.getItem("pp_session");
  const setSession = (s) => sessionStorage.setItem("pp_session", s);

  // ---- tiny helpers -------------------------------------------------------------------------

  function formatMoney(amount, currency) {
    try { return new Intl.NumberFormat(undefined, { style: "currency", currency }).format(amount); }
    catch (_) { return `${amount} ${currency}`; }
  }
  function escapeHtml(s) {
    return String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  }
  function showModal(title, html) {
    let m = $("#modal");
    if (!m) {
      m = document.createElement("div");
      m.id = "modal";
      m.innerHTML = `<div class="modal-card"><h3></h3><div class="modal-body"></div>
        <button class="btn btn-primary" id="modal-close">Got it</button></div>`;
      document.body.appendChild(m);
      m.addEventListener("click", (e) => { if (e.target === m || e.target.id === "modal-close") m.classList.remove("on"); });
    }
    $("#modal h3").textContent = title;
    $("#modal .modal-body").innerHTML = html;
    m.classList.add("on");
  }

  // ---- boot ---------------------------------------------------------------------------------

  document.addEventListener("DOMContentLoaded", () => {
    if (!live) document.body.classList.add("preview");
    initStore();
    initAccount();
  });
})();
