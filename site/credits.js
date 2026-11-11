// Pigeonpost credits on the web (credits v1): the account page's Credits card.
//
// Credits pay for services other publishers run on Pigeonpost. They are kept and spent in US
// dollars. A person buys a fixed pack ($10, $25, $50, $100) with a one-time payment in USD, EUR or
// TRY on iyzico's hosted page; the price in the chosen currency is quoted first and fixed for the
// quote's life. Nothing here holds a credential or talks to a payment provider: every call goes
// through the bridge helpers the account page hands in (`bridgeGet`, `bridgeAction`), and the card
// form is iyzico's own page, which returns here with `?credits=<intent>`.
//
// Bridge routes (store-adapter/src/credits.js; the owner is always the signed-in account):
//   GET  /v1/credits/packs              the packs and the currencies open for payment
//   POST /v1/credits/quotes             {pack_id, currency} -> the charge for that pack
//   POST /v1/credits/checkout           {intent_id, quote_id, billing_profile_id} -> payment_page_url
//   GET  /v1/credits/checkout/{intent}  pending, paid, failed or expired
//   GET  /v1/credits/balance            {available_cents, held_cents}
//   GET  /v1/credits/history            {entries, next_cursor}
(function () {
  "use strict";

  const INTENT = /^[a-f0-9]{32}$/;

  function el(tag, attrs, ...children) {
    const node = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v == null || v === false) continue;
      if (k === "class") node.className = v;
      else if (k === "text") node.textContent = v;
      else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
      else node.setAttribute(k, v === true ? "" : String(v));
    }
    for (const c of children.flat()) if (c != null && c !== false) node.append(c);
    return node;
  }

  function money(minor, currency) {
    const n = Number(minor) / 100;
    try { return new Intl.NumberFormat("en-US", { style: "currency", currency }).format(n); } catch (_) { return `${n.toFixed(2)} ${currency}`; }
  }
  const usd = (cents) => money(cents, "USD");

  const ERRORS = {
    quote_expired: "That price has expired. Here is a new one.",
    quote_not_found: "Choose a pack again.",
    currency_unavailable: "Payments in that currency aren't open yet. Choose another.",
    billing_profile_required: "Save your billing details first.",
    billing_profile_incomplete: "Complete your billing details (name, address, city and country) first.",
    eu_uk_consumer_web_unavailable: "We can't sell credits on the web to consumers in the EU or UK yet. Use the Pigeonpost app on iOS or Android.",
    vat_id_invalid: "Check the VAT number in your billing details.",
    rate_limited: "Too many purchases started in the last hour. Try again later.",
    not_configured: "Buying credits isn't open yet.",
    checkout_page_lost: "That payment page can't be shown again. Start again.",
  };
  const errorText = (e) => ERRORS[e && e.body && e.body.code] || (e && e.status === 401 ? "Sign in again to continue." : `Something went wrong${e && e.message ? `: ${e.message}` : "."}`);

  const KIND = { purchase: "Bought credit", hold: "Reserved for a run", release: "Returned to your credit", sale: "Paid for a run",
    capture: "Paid for a run", refund: "Refunded", adjustment: "Adjustment", promo_grant: "Credit from Pigeonpost", chargeback: "Card payment reversed" };
  const SPENDS = new Set(["hold", "sale", "capture", "chargeback"]);

  /// Credits as one line: the balance, and what running jobs hold.
  function balanceText(b) {
    if (!b) return "Your credit could not be loaded.";
    return `Balance: ${usd(b.available_cents)}` + (b.held_cents > 0 ? ` · ${usd(b.held_cents)} reserved for runs in progress` : "");
  }

  function mount(box, deps) {
    const st = { packs: null, currencies: [], pack: "usd_25", currency: deps.preferredCurrency || "USD", quote: null, quoteGen: 0,
      balance: null, history: [], next: null, busy: false };
    const wait = deps.wait || ((ms) => new Promise((r) => setTimeout(r, ms)));
    const status = el("p", { id: "cr-status", role: "status", "aria-live": "polite", class: "ac-note" });
    const say = (text, ...extra) => { status.textContent = text || ""; for (const x of extra) if (x) status.append(" ", x); };
    const balanceLine = el("p", { id: "cr-balance", class: "cr-balance", text: "Loading…" });
    const quoteLine = el("p", { id: "cr-quote", class: "muted" });
    const historyList = el("ul", { id: "cr-history", class: "mp-list", "aria-label": "Credit history" });
    const more = el("button", { type: "button", class: "btn btn-small", id: "cr-more", hidden: true, onclick: () => loadHistory(true) }, "Show more");
    const go = el("button", { type: "submit", class: "btn btn-primary", id: "cr-buy-go", disabled: true }, "Buy credits");
    const packBox = el("fieldset", { class: "mp-section", id: "cr-packs" }, el("legend", { text: "Choose a pack" }));
    const currency = el("select", { id: "cr-currency", onchange: () => { st.currency = currency.value; requote(); } });
    const form = el("form", { class: "ac-form", id: "cr-buy", onsubmit: buy },
      packBox, el("label", { for: "cr-currency" }, "Pay in"), currency, quoteLine, go,
      el("p", { class: "ac-note", text: "You pay on iyzico's secure page and come back here. The price includes any tax." }));

    box.replaceChildren(
      el("h3", { id: "cr-title", text: "Credits" }),
      el("p", { class: "muted", text: "Credits pay for services other publishers run on Pigeonpost. They are kept in US dollars and don't expire. Unspent credit from a purchase can be refunded within 14 days." }),
      balanceLine, status, form,
      el("h4", { text: "History" }), historyList, more);

    function drawPacks() {
      packBox.replaceChildren(el("legend", { text: "Choose a pack" }));
      for (const p of st.packs || []) {
        const id = `cr-pack-${p.pack_id}`;
        packBox.append(el("label", { for: id, class: "mp-check" },
          el("input", { type: "radio", name: "cr-pack", id, value: p.pack_id, checked: p.pack_id === st.pack,
            onchange: () => { st.pack = p.pack_id; requote(); } }),
          `${usd(p.amount_cents)} of credit`));
      }
      currency.replaceChildren(...st.currencies.map((c) => el("option", { value: c, selected: c === st.currency }, c)));
    }

    async function requote() {
      const gen = ++st.quoteGen;
      st.quote = null;
      go.disabled = true;
      quoteLine.textContent = "Getting the price…";
      try {
        const q = await deps.bridgeAction("/v1/credits/quotes", { pack_id: st.pack, currency: st.currency });
        if (gen !== st.quoteGen) return;
        st.quote = q;
        const fx = q.fx && q.charge.currency !== "USD" ? ` (1 USD = ${q.fx.rate} ${q.charge.currency}${q.fx.source ? `, ${String(q.fx.source).toUpperCase()} rate` : ""})` : "";
        quoteLine.textContent = `You pay ${money(q.charge.amount_minor, q.charge.currency)} for ${usd(q.credit_cents)} of credit${fx}.`;
        go.textContent = `Buy ${usd(q.credit_cents)} of credit`;
        go.disabled = false;
      } catch (e) {
        if (gen !== st.quoteGen) return;
        quoteLine.textContent = "";
        say(errorText(e));
      }
    }

    async function buy(e) {
      e.preventDefault();
      if (st.busy || !st.quote) return;
      const profile = await deps.billingProfileId();
      if (!profile) { say(ERRORS.billing_profile_required); if (deps.goBilling) deps.goBilling(); return; }
      st.busy = true;
      go.disabled = true;
      say("Opening the payment page…");
      try {
        const intent = deps.randomHex32();
        const c = await deps.bridgeAction("/v1/credits/checkout", { intent_id: intent, quote_id: st.quote.quote_id, billing_profile_id: profile });
        if (!/^https:\/\//.test(String(c.payment_page_url || ""))) throw Object.assign(new Error("no payment page"), { body: {} });
        deps.navigate(c.payment_page_url);
      } catch (err) {
        say(errorText(err));
        if (err && err.body && err.body.code === "quote_expired") requote();
        else go.disabled = false;
      } finally {
        st.busy = false;
      }
    }

    async function loadBalance() {
      try { st.balance = await deps.bridgeGet("/v1/credits/balance"); } catch (_) { st.balance = null; }
      balanceLine.textContent = balanceText(st.balance);
    }

    function entry(h) {
      const spend = h.amount_cents < 0 || SPENDS.has(h.kind);
      const amount = `${spend ? "−" : "+"}${usd(Math.abs(h.amount_cents))}`;
      const paid = h.charge ? ` (paid ${money(h.charge.amount_minor, h.charge.currency)})` : "";
      return el("li", { class: "mp-row", "data-kind": h.kind },
        el("span", { text: `${h.created_at ? new Date(h.created_at).toLocaleDateString() : ""} · ${KIND[h.kind] || h.kind} · ${amount}${paid}` }),
        h.description ? el("span", { class: "muted", text: ` · ${h.description}` }) : null);
    }

    async function loadHistory(next) {
      try {
        const page = await deps.bridgeGet(`/v1/credits/history?limit=20${next && st.next ? `&cursor=${encodeURIComponent(st.next)}` : ""}`);
        st.history = (next ? st.history : []).concat(page.entries || []);
        st.next = page.next_cursor || null;
      } catch (_) { if (!next) st.history = null; }
      if (st.history === null) historyList.replaceChildren(el("li", { class: "muted", text: "Your history could not be loaded." }));
      else if (!st.history.length) historyList.replaceChildren(el("li", { class: "muted", text: "No credit bought or spent yet." }));
      else historyList.replaceChildren(...st.history.map(entry));
      more.hidden = !st.next;
    }

    /// Back from iyzico: the redirect proves nothing, so ask the bridge (which asks saas_be).
    async function confirmReturn(intent) {
      if (deps.clearReturn) deps.clearReturn("credits");
      say("Confirming your payment…");
      for (let attempt = 0; attempt < (deps.attempts || 20); attempt++) {
        try {
          const r = await deps.bridgeGet(`/v1/credits/checkout/${intent}`);
          if (r.status === "paid") { say(`Payment received. ${usd(r.credit_cents)} of credit was added.`); await Promise.all([loadBalance(), loadHistory(false)]); return; }
          if (r.status === "failed") { say("The payment did not go through. No credit was added."); return; }
          if (r.status === "expired") { say("The payment page expired before it was paid. No credit was added."); return; }
        } catch (e) {
          if (e && e.status === 404) { say("We could not find that purchase."); return; }
          if (e && e.status === 401) return;
        }
        await wait(3000);
      }
      say("Your payment is still being confirmed. Your balance will show the credit as soon as it is.");
    }

    async function start() {
      const params = new URLSearchParams(deps.search || "");
      const returning = params.get("credits");
      try {
        const p = await deps.bridgeGet("/v1/credits/packs");
        st.packs = p.packs || [];
        st.currencies = p.currencies && p.currencies.length ? p.currencies : ["USD"];
        if (!st.currencies.includes(st.currency)) st.currency = st.currencies[0];
        if (!st.packs.some((x) => x.pack_id === st.pack) && st.packs.length) st.pack = st.packs[0].pack_id;
        drawPacks();
        if (st.packs.length) requote();
      } catch (e) {
        form.hidden = true;
        say(errorText(e));
      }
      await Promise.all([loadBalance(), loadHistory(false)]);
      if (INTENT.test(returning || "")) await confirmReturn(returning);
    }

    const done = start();
    return { done, reload: () => Promise.all([loadBalance(), loadHistory(false)]) };
  }

  window.PPCredits = { mount, balanceText, money };
})();
