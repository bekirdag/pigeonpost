// Wallet: credits (P23) — balance, a pack ($10/$25/$50/$100) quoted in USD, EUR or TRY and paid on
// iyzico's page, history, and the 14-day refund of unspent credit. Back from iyzico:
// `?credits=<intent>`, confirmed with the bridge.
//
// Bridge: GET /v1/credits/packs, POST /v1/credits/quotes {pack_id, currency},
//   POST /v1/credits/checkout {intent_id, quote_id, billing_profile_id} → payment_page_url,
//   GET /v1/credits/checkout/{intent}, GET /v1/credits/balance, GET /v1/credits/history?limit=&cursor=.
(function () {
  "use strict";
  const el = (...a) => PP.el(...a);
  const money = (minor, c) => ACC.money(Number(minor) / 100, c);
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
  const why = (e) => ERRORS[ACC.code(e)] || PP.explain(e);
  const KIND = { purchase: "Bought credit", hold: "Reserved for a run", release: "Returned to your credit", sale: "Paid for a run",
    capture: "Paid for a run", refund: "Refunded", adjustment: "Adjustment", promo_grant: "Credit from Pigeonpost", chargeback: "Card payment reversed" };
  const SPENDS = new Set(["hold", "sale", "capture", "chargeback"]);

  async function returning(banner) {
    const intent = ACC.param("credits");
    if (!intent) return;
    ACC.clearParam("credits");
    if (!ACC.INTENT.test(intent)) return;
    banner.say("Confirming your payment…");
    const r = await ACC.poll(`/v1/credits/checkout/${intent}`, (x) => (x.status === "paid" ? { text: `Payment received. ${ACC.usd(x.credit_cents)} of credit was added.`, tone: "ok" }
      : x.status === "failed" ? { text: "The payment did not go through. No credit was added.", tone: "error" }
        : x.status === "expired" ? { text: "The payment page expired before it was paid. No credit was added.", tone: "error" } : null));
    if (!r) return;
    banner.say(r.text, r.tone);
    if (r.tone === "ok") { ACC.forget("balance"); if (banner.isConnected) window.setTimeout(() => PP.shell.refresh(), 1500); }
  }

  function buyCard(packs, currencies, ov) {
    const st = { pack: packs.some((p) => p.pack_id === "usd_25") ? "usd_25" : (packs[0] || {}).pack_id, currency: currencies.includes(ACC.currency()) ? ACC.currency() : currencies[0], quote: null, gen: 0 };
    const quote = el("p", { class: "pp-muted", id: "cr-quote", role: "status", "aria-live": "polite" });
    const status = ACC.statusLine();
    const go = ACC.btn("Buy credits", { kind: "primary", type: "submit", id: "cr-buy-go", disabled: true });
    const choices = el("div", { class: "acc-packs", role: "radiogroup", "aria-label": "Choose a pack" }, packs.map((p) => {
      const id = `cr-pack-${p.pack_id}`;
      const input = el("input", { type: "radio", name: "cr-pack", id, value: p.pack_id, checked: p.pack_id === st.pack ? true : null, class: "pp-sr" });
      input.addEventListener("change", () => { st.pack = p.pack_id; requote(); });
      return el("label", { class: "acc-pack", for: id }, input, el("strong", {}, ACC.usd(p.amount_cents)), el("span", {}, "of credit"));
    }));
    const cur = ACC.select(currencies.map((c) => [c, ACC.CURRENCY_NAMES[c] || c]), st.currency, { id: "cr-currency" });
    cur.addEventListener("change", () => { st.currency = cur.value; ACC.setCurrency(cur.value); requote(); });

    async function requote() {
      const gen = ++st.gen;
      st.quote = null;
      go.disabled = true;
      quote.textContent = "Getting the price…";
      try {
        const q = await ACC.bridgeAction("/v1/credits/quotes", { pack_id: st.pack, currency: st.currency });
        if (gen !== st.gen) return;
        st.quote = q;
        const fx = q.fx && q.charge.currency !== "USD" ? ` (1 USD = ${q.fx.rate} ${q.charge.currency}${q.fx.source ? `, ${String(q.fx.source).toUpperCase()} rate` : ""})` : "";
        quote.textContent = `You pay ${money(q.charge.amount_minor, q.charge.currency)} for ${ACC.usd(q.credit_cents)} of credit${fx}.`;
        go.querySelector("span").textContent = `Buy ${ACC.usd(q.credit_cents)} of credit`;
        go.disabled = false;
      } catch (e) {
        if (gen !== st.gen) return;
        quote.textContent = "";
        status.say(why(e), "error");
      }
    }
    const form = el("form", { class: "acc-stack", id: "cr-buy", onsubmit: async (e) => {
      e.preventDefault();
      if (!st.quote || go.disabled) return;
      if (!ov.billing.profileId) { status.say(ERRORS.billing_profile_required, "error"); return; }
      go.disabled = true;
      status.say("Opening the payment page…");
      try {
        const c = await ACC.bridgeAction("/v1/credits/checkout", { intent_id: ACC.hex32(), quote_id: st.quote.quote_id, billing_profile_id: ov.billing.profileId });
        if (!/^https:\/\//.test(String(c.payment_page_url || ""))) throw new Error("No payment page came back. Try again.");
        PP.navigate(c.payment_page_url);
      } catch (err) {
        status.say(why(err), "error");
        if (ACC.code(err) === "quote_expired") requote(); else go.disabled = false;
      }
    } }, choices, el("div", { class: "pp-form" }, ACC.field("Pay in", cur)), quote, status,
    el("div", { class: "pp-form-actions" }, go),
    el("p", { class: "pp-hint" }, "You pay on iyzico's secure page and come back here. The price includes any tax."));
    if (packs.length && ov.billing.has) requote();
    return PP.ui.card({ title: "Add credit", text: "Credits pay for services other publishers run on Pigeonpost. They are kept in US dollars and don't expire." },
      ACC.billingGate(ov, form));
  }

  function history(first) {
    let rows = first.entries || [];
    let next = first.next_cursor || null;
    const host = el("div", {});
    const draw = () => host.replaceChildren(PP.ui.table({
      caption: "Credit history",
      columns: [
        { key: "created_at", label: "Date", render: (h) => ACC.date(h.created_at) },
        { key: "kind", label: "What", render: (h) => el("div", { class: "acc-cell-stack" }, KIND[h.kind] || h.kind, h.description ? el("span", { class: "pp-faint" }, h.description) : null) },
        { key: "amount_cents", label: "Amount", num: true, render: (h) => {
          const spend = h.amount_cents < 0 || SPENDS.has(h.kind);
          return el("span", { class: spend ? "acc-minus" : "acc-plus" }, `${spend ? "−" : "+"}${ACC.usd(Math.abs(h.amount_cents))}`, h.charge ? el("span", { class: "pp-faint" }, ` (paid ${money(h.charge.amount_minor, h.charge.currency)})`) : null);
        } },
      ],
      rows,
      pageSize: 50,
      empty: { icon: "wallet", title: "No credit bought or spent yet", text: "Your purchases and runs will show here." },
    }), next ? ACC.btn("Show more", { size: "sm", id: "cr-more", onclick: async () => {
      const p = await ACC.bridgeGet(`/v1/credits/history?limit=20&cursor=${encodeURIComponent(next)}`);
      rows = rows.concat(p.entries || []); next = p.next_cursor || null; draw();
    } }) : null);
    draw();
    return host;
  }

  ACC.page({
    key: "wallet", title: "Wallet", group: "Billing", icon: "wallet",
    async render(view, ctx) {
      const banner = ACC.statusLine();
      view.append(PP.ui.header({ title: "Wallet", text: "Credit for marketplace services, in US dollars." }), banner);
      if (!ACC.feature(ctx, "marketplace")) {
        view.append(PP.ui.card({}, ACC.notYet("Credits aren't open on this account yet", "Credits pay for marketplace services. When the marketplace opens for you, your wallet appears here.", "wallet")));
        return;
      }
      returning(banner).catch(() => {});
      const [bal, packs, hist, ov] = await Promise.all([
        ACC.balance(true).catch(() => null),
        ACC.bridgeGet("/v1/credits/packs").catch((e) => ({ error: e })),
        ACC.bridgeGet("/v1/credits/history?limit=20").catch(() => null),
        ACC.overview(),
      ]);
      view.append(PP.ui.stats([
        { label: "Available", value: bal ? ACC.usd(bal.available_cents) : "—", hint: bal ? "Ready to spend" : "Could not be loaded" },
        { label: "Reserved", value: bal ? ACC.usd(bal.held_cents) : "—", hint: "Held for runs in progress" },
      ]));
      const left = packs.error ? PP.ui.card({ title: "Add credit" }, el("p", { class: "pp-muted" }, why(packs.error)))
        : buyCard(packs.packs || [], packs.currencies && packs.currencies.length ? packs.currencies : ["USD"], ov);
      view.append(el("div", { class: "pp-cols" }, left,
        PP.ui.card({ title: "Refunds", text: "Unspent credit from a purchase can be refunded within 14 days of buying it." },
          el("p", { class: "pp-muted" }, "Credit already spent on runs is not refundable; a run that fails or runs out of time returns its credit automatically."),
          ACC.btn("Ask for a refund", { href: PP.href("support"), icon: "support" }))));
      view.append(PP.ui.section({ title: "History" }, hist ? history(hist) : PP.ui.errorState(new Error("Your history could not be loaded."), () => PP.shell.refresh())));
    },
  });
})();
