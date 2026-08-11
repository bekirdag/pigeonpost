// Pigeonpost handle store — client logic.
//
// This page is the branded front door. The store itself — sign-in, checkout, card capture,
// subscription management — is hosted by MASAAS; the buttons here hand off to it. With no
// masaasStoreUrl configured the page runs in preview and explains that instead of redirecting.

(function () {
  "use strict";
  const cfg = window.PIGEONPOST_STORE || {};
  const reserved = window.PIGEONPOST_RESERVED;
  const live = Boolean(cfg.masaasStoreUrl);

  const $ = (sel, root) => (root || document).querySelector(sel);
  const MIN = (cfg.handle && cfg.handle.min) || 3;
  const MAX = (cfg.handle && cfg.handle.max) || 32;

  function storeUrl(extraParams) {
    const base = cfg.masaasStoreUrl.replace(/\/+$/, "") + (cfg.billingPath || "/billing");
    const p = new URLSearchParams(extraParams || {});
    const qs = p.toString();
    return qs ? `${base}?${qs}` : base;
  }

  // ---- validation ---------------------------------------------------------------------------

  function validate(nameRaw) {
    const name = String(nameRaw || "").trim().toLowerCase();
    if (!name) return { ok: false, silent: true };
    if (/[^a-z0-9]/.test(name)) {
      return { ok: false, reason: "letters a–z and digits 0–9 only — no spaces, @, dots, or dashes" };
    }
    if (name.length < MIN) return { ok: false, reason: `at least ${MIN} characters` };
    if (name.length > MAX) return { ok: false, reason: `at most ${MAX} characters` };
    const r = reserved && reserved.reason(name);
    if (r) return { ok: false, reason: r };
    return { ok: true, name };
  }

  // ---- live price from the public catalog ---------------------------------------------------

  async function loadPrice() {
    if (!cfg.catalog || !cfg.catalog.apiUrl) return null;
    try {
      const url = `${cfg.catalog.apiUrl}/catalog/public/packages?product_slug=${encodeURIComponent(cfg.catalog.productSlug)}`;
      const res = await fetch(url, { headers: { accept: "application/json" } });
      if (!res.ok) return null;
      const body = await res.json();
      const pkg = (body.packages || []).find((p) => p.package_slug === cfg.catalog.packageSlug);
      const plan = pkg && (pkg.active_price_plans || []).find((p) => p.plan_slug === cfg.catalog.planSlug);
      if (!plan) return null;
      return { amount: plan.amount, currency: plan.currency, interval: plan.interval_unit };
    } catch (_) {
      return null;
    }
  }

  // ---- store page ---------------------------------------------------------------------------

  function initStore() {
    const input = $("#handle-input");
    const status = $("#handle-status");
    const buy = $("#buy-btn");
    const priceEl = $("#price");
    if (!input) return;

    const showPrice = (p) => { if (p) priceEl.textContent = `${formatMoney(p.amount, p.currency)}/${p.interval}`; };
    showPrice(cfg.price);
    loadPrice().then(showPrice);

    let current = { ok: false };

    function render(v) {
      current = v;
      status.className = "hs";
      if (v.silent) { status.textContent = ""; buy.disabled = true; return; }
      if (!v.ok) { status.classList.add("bad"); status.textContent = "✗ " + v.reason; buy.disabled = true; return; }
      status.classList.add("ok");
      status.textContent = `✓ /${v.name} looks good — availability is confirmed when you check out`;
      buy.disabled = false;
    }

    input.addEventListener("input", () => render(validate(input.value)));
    input.addEventListener("keydown", (e) => { if (e.key === "Enter" && current.ok) startCheckout(current.name); });
    buy.addEventListener("click", () => { if (current.ok) startCheckout(current.name); });
  }

  function startCheckout(name) {
    if (!live) {
      showModal(
        "Not selling yet",
        `The store is built and this button is where checkout begins. Once the MASAAS store URL is ` +
        `set in <code>config.js</code>, it sends you to sign in and subscribe to ` +
        `<strong>/${name}</strong> at $${(cfg.price && cfg.price.amount) || 5}/year.`
      );
      return;
    }
    // Hand off to the MASAAS-hosted store. Pass the wanted handle and package as hints; MASAAS owns
    // sign-in, the card form, and the subscription from here.
    window.location.href = storeUrl({
      package: cfg.catalog.packageSlug,
      plan: cfg.catalog.planSlug,
      handle: name,
    });
  }

  // ---- account page: hand off to the hosted store's billing view ----------------------------

  function initAccount() {
    const root = $("#account-root");
    if (!root) return;
    if (!live) {
      root.innerHTML = `
        <div class="card">
          <h2>Your handles</h2>
          <p class="muted">Your handles and subscription are managed in the MASAAS-hosted store.
          Once its URL is configured, this page sends you straight there. Running in preview now.</p>
        </div>`;
      return;
    }
    root.innerHTML = `
      <div class="card center">
        <h2>Manage your handles</h2>
        <p>Your handles, renewals, and payment method live in the secure store.</p>
        <a class="btn btn-primary" href="${escapeAttr(storeUrl())}">Open the store →</a>
      </div>`;
    // Auto-forward after a moment so the account link behaves like a redirect without trapping anyone.
    setTimeout(() => { window.location.href = storeUrl(); }, 1200);
  }

  // ---- helpers ------------------------------------------------------------------------------

  function formatMoney(amount, currency) {
    try { return new Intl.NumberFormat(undefined, { style: "currency", currency }).format(amount); }
    catch (_) { return `${amount} ${currency}`; }
  }
  function escapeAttr(s) { return String(s).replace(/"/g, "&quot;"); }
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

  document.addEventListener("DOMContentLoaded", () => {
    if (!live) document.body.classList.add("preview");
    initStore();
    initAccount();
  });
})();
