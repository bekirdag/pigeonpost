// Plan: the Pigeonpost Cloud plan (1–10 names, yearly through iyzico): choose, change, update the
// card, see details and retry a failed payment, cancel. Back from iyzico: `?checkout=<intent>`
// (confirmed with the bridge) and `?card=<intent>`.
//
// Bridge routes: GET /v1/cloud/checkout/{intent}, GET /v1/cloud/subscriptions/{id},
// POST /v1/cloud/subscriptions/{id}/(cancel|retry|upgrade); checkout and card-update run on
// /billing/checkout.
(function () {
  "use strict";
  const el = (...a) => PP.el(...a);
  const cloud = () => window.PP_CLOUD || { plans: {}, currencies: ["USD"] };
  const PLAN_CODE = (names, currency) => `cloud-${names}names-yearly-${currency.toLowerCase()}`;
  const priceLabel = (names, currency) => {
    const p = cloud().plans[names];
    return p ? `${ACC.money(p[currency], currency)}/year` : "";
  };
  ACC.statusText = (provider) => ({ ACTIVE: "Active", CANCELED: "Canceled — no renewal", UNPAID: "Payment failed", PENDING: "Payment pending",
    CHECKOUT_PENDING: "Checkout not finished", UPGRADED: "Changed", EXPIRED: "Expired" }[provider] || "Processing");
  const STATUS_TONE = { ACTIVE: "success", CANCELED: "neutral", UNPAID: "danger", PENDING: "warning", CHECKOUT_PENDING: "warning", EXPIRED: "neutral" };

  /// The plan picker: how many names, which currency, then the hosted payment page.
  ACC.planPicker = function (ov, minNames, { onCancel } = {}) {
    const open = cloud().currencies.filter((c) => !ov.openCurrencies || ov.openCurrencies.includes(c));
    let currency = open.includes(ACC.currency()) ? ACC.currency() : open[0];
    const box = el("div", { class: "acc-picker", id: "acc-plan-picker" });
    const draw = () => {
      const names = ACC.select(Object.keys(cloud().plans).map(Number).filter((n) => n >= minNames)
        .map((n) => [n, `${n} name${n === 1 ? "" : "s"} — ${priceLabel(n, currency)}`]), minNames, { id: "acc-plan-names" });
      const cur = ACC.select(open.map((c) => [c, ACC.CURRENCY_NAMES[c] || c]), currency, { id: "acc-plan-currency" });
      cur.addEventListener("change", () => { currency = cur.value; ACC.setCurrency(currency); draw(); });
      box.replaceChildren(
        el("div", { class: "pp-form" }, ACC.field("Plan", names), ACC.field("Currency", cur)),
        el("p", { class: "pp-hint" }, `Sold by Piyote Bilişim and billed yearly through iyzico. Cards issued in Türkiye pay in TRY. Consumers in the EU and UK: buy in the Pigeonpost iOS or Android app; EU and UK businesses buy here with a VAT number (reverse charge). TRY prices are fixed for ${cloud().pricingYear}. Cancel any time; your names keep working until the end of the paid year.`),
        el("div", { class: "pp-form-actions" },
          onCancel ? ACC.btn("Cancel", { onclick: onCancel }) : null,
          ACC.btn("Continue to secure payment", { kind: "primary", id: "acc-plan-go", onclick: () => {
            if (!ov.billing.profileId) { PP.go("billing"); return; }
            ACC.startCheckout({ kind: "cloud", intent_id: ACC.hex32(), plan_code: PLAN_CODE(Number(names.value), currency), billing_profile_id: ov.billing.profileId, locale: ACC.locale() });
          } })));
    };
    draw();
    return box;
  };

  function changeSheet(sub, plan) {
    PP.openSheet("Change your plan", (body) => {
      const names = ACC.select(Object.keys(cloud().plans).map(Number).filter((n) => n !== sub.names)
        .map((n) => [n, `${n} name${n === 1 ? "" : "s"} — ${priceLabel(n, sub.currency)}`, n < plan.used ? { disabled: true } : null]), "", { id: "acc-change-names" });
      const when = ACC.select([["NOW", "Now"], ["NEXT_PERIOD", "At the next renewal"]], "NOW", { id: "acc-change-when" });
      const status = ACC.statusLine();
      body.append(el("div", { class: "pp-form" }, ACC.field("New plan", names), ACC.field("When", when)),
        el("p", { class: "pp-hint" }, `The price and currency stay in ${sub.currency}. iyzico confirms any charge for a change made now. A smaller plan needs names released first.`),
        status,
        el("div", { class: "pp-form-actions" }, ACC.btn("Cancel", { onclick: () => PP.closeSheet() }),
          ACC.btn("Change plan", { kind: "primary", id: "acc-change-go", onclick: async () => {
            try {
              await ACC.bridgeAction(`/v1/cloud/subscriptions/${encodeURIComponent(sub.id)}/upgrade`, { intent_id: ACC.hex32(), plan_code: PLAN_CODE(Number(names.value), sub.currency), upgrade_period: when.value });
              PP.closeSheet();
              PP.toast("Plan change submitted.", "success");
              ACC.forget("overview");
              PP.shell.refresh();
            } catch (e) { status.say("Could not change the plan: " + PP.explain(e), "error"); }
          } })));
    });
  }

  function receipt(r) {
    if (!r) return null;
    const who = [r.customer && r.customer.name, r.customer && r.customer.country, r.customer && r.customer.vat_id ? `VAT ${r.customer.vat_id}` : ""].filter(Boolean).join(" · ");
    return el("div", { class: "acc-receipt" }, el("p", {}, `Seller: ${r.seller || "—"}`), who ? el("p", {}, `Customer: ${who}`) : null, (r.lines || []).map((l) => el("p", { class: "pp-muted" }, String(l))));
  }

  function detailsSheet(sub) {
    const body = PP.openSheet("Plan details", (b) => b.append(PP.ui.loading(2)));
    (async () => {
      try {
        const s = await ACC.bridgeGet(`/v1/cloud/subscriptions/${encodeURIComponent(sub.id)}`, { keepSession: true });
        body.replaceChildren(
          PP.ui.kv([["Status", PP.ui.status(s.provider_status, { [s.provider_status]: [ACC.statusText(s.provider_status), STATUS_TONE[s.provider_status] || "neutral"] })],
            ["Paid through", s.paid_through ? ACC.date(s.paid_through) : null], ["Next payment", s.next_payment_at ? ACC.date(s.next_payment_at) : null],
            s.pending_change ? ["Scheduled", "A plan change is scheduled"] : null]),
          receipt(s.receipt),
          (s.failed_orders || []).map((o) => PP.ui.card({ tone: "warning", title: `Payment of ${ACC.money(o.amount, o.currency)} failed`,
            actions: ACC.btn("Retry payment", { kind: "primary", size: "sm", onclick: async () => {
              await ACC.bridgeAction(`/v1/cloud/subscriptions/${encodeURIComponent(sub.id)}/retry`, { intent_id: ACC.hex32(), order_reference_code: o.order_reference_code });
              PP.toast("Payment retried.", "success");
              PP.closeSheet();
              ACC.forget("overview");
              PP.shell.refresh();
            } }) })));
      } catch (e) { body.replaceChildren(PP.ui.errorState(e)); }
    })();
  }

  /// Back from iyzico through saas_be's callback. The redirect proves nothing; the bridge reads the
  /// authoritative state and activates the plan only for a confirmed payment.
  async function returning(view) {
    const intent = ACC.param("checkout");
    const card = ACC.param("card");
    if (!intent && !card) return;
    ACC.clearParam("checkout", "card");
    const banner = ACC.statusLine();
    view.append(banner);
    if (card) { banner.say("Card update submitted. It may take a minute to show.", "ok"); return; }
    if (!ACC.INTENT.test(intent)) return;
    PP.store.del("pp_cloud_checkout");
    banner.say("Confirming your payment…");
    const r = await ACC.poll(`/v1/cloud/checkout/${intent}`, (x) => (["active", "canceled"].includes(x.status) ? { text: "Your Pigeonpost Cloud plan is active.", tone: "ok" }
      : x.status === "failed" ? { text: "The payment did not go through. No plan was started.", tone: "error" } : null));
    if (!r) return;
    banner.say(r.text, r.tone);
    ACC.forget("overview");
    if (r.tone === "ok" && banner.isConnected) PP.shell.refresh();
  }

  ACC.page({
    key: "plan", title: "Plan", group: "Billing", icon: "plan",
    async render(view) {
      view.append(PP.ui.header({ title: "Plan", text: "Pigeonpost Cloud: your names on every device, paid yearly." }));
      returning(view).catch(() => {});
      const ov = await ACC.overview();
      const plan = ov.plan;
      const sub = ov.subscription;
      if (ov.cloudError) {
        view.append(PP.ui.card({}, PP.ui.errorState(new Error("Your plan could not be loaded. Try again in a minute."), () => { ACC.forget("overview"); PP.shell.refresh(); })));
        return;
      }
      const included = PP.ui.card({ title: "What is included" }, el("ul", { class: "acc-ticks" },
        ["Your names on mobile, desktop and the web", "Unlimited agents under each name, free", "Meeting summaries, in every paid plan", "Cancel any time; names keep working to the end of the paid year"]
          .map((t) => el("li", {}, PP.icon("check"), t))));
      if (sub) {
        const status = PP.ui.status(sub.provider_status, { [sub.provider_status]: [ACC.statusText(sub.provider_status), STATUS_TONE[sub.provider_status] || "neutral"] });
        view.append(el("div", { class: "pp-cols" },
          PP.ui.card({ title: `${plan.capacity || sub.names} name${(plan.capacity || sub.names) === 1 ? "" : "s"}`, text: `${plan.used} registered${plan.paid_through ? ` · paid through ${ACC.date(plan.paid_through)}` : ""}`, actions: status },
            PP.ui.kv([["Billed", sub.recurring_price ? `${ACC.money(sub.recurring_price.amount, sub.recurring_price.currency)} a year` : sub.currency], ["Currency", sub.currency]]),
            el("div", { class: "pp-card-actions" },
              ACC.billingGate(ov, ACC.btn("Change plan", { kind: "primary", id: "acc-change-plan", onclick: () => changeSheet(sub, plan) })),
              ACC.btn("Update card", { onclick: () => ACC.startCheckout({ kind: "card", intent_id: ACC.hex32(), subscription_id: sub.id, locale: ACC.locale() }) }),
              ACC.btn("Details", { onclick: () => detailsSheet(sub) }),
              sub.provider_status === "CANCELED" ? null : ACC.btn("Cancel plan", { kind: "danger", onclick: async () => {
                if (!(await PP.ui.confirm({ title: "Cancel your plan?", text: "It will not renew. Your names keep working until the end of the paid year.", confirm: "Cancel plan", danger: true }))) return;
                const r = await ACC.bridgeAction(`/v1/cloud/subscriptions/${encodeURIComponent(sub.id)}/cancel`, { intent_id: ACC.hex32() });
                PP.toast(r && r.paid_through ? `Canceled. Your names keep working until ${ACC.date(r.paid_through)}.` : "Canceled.");
                ACC.forget("overview");
                PP.shell.refresh();
              } }))),
          included));
      } else {
        const capacityText = plan.capacity > 0 ? `${plan.capacity} names · ${plan.used} registered` : "No Pigeonpost Cloud plan yet. Names bought in the App Store or Google Play are managed there.";
        view.append(el("div", { class: "pp-cols" },
          PP.ui.card({ title: "Choose a plan", text: capacityText },
            ACC.billingGate(ov, ACC.planPicker(ov, Math.max(1, plan.used)))),
          included));
      }
    },
  });
})();
