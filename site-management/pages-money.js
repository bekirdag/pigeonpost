// management.pigeonpost.dev — Money: credits (balances, history, refunds, sales, adjustments),
// payouts and invoices. Every credit view is recorded by the postbox; adjustments are capped at
// $10,000 either way here as well as on the server.
(function () {
  "use strict";
  const PP = window.PP, M = window.PPM, ui = PP.ui;
  const el = (...a) => PP.el(...a);
  const enc = encodeURIComponent;

  /// The largest adjustment either way, in cents ($10,000).
  M.ADJUST_CAP_CENTS = 1000000;
  /// Dollars text → cents, or an explanation of why not.
  M.adjustCents = function (text) {
    const n = Number(String(text || "").trim());
    if (!String(text || "").trim() || !Number.isFinite(n)) return { error: "Enter an amount in USD, e.g. 5.00 or -5.00." };
    const cents = Math.round(n * 100);
    if (cents === 0) return { error: "Enter a non-zero amount." };
    if (Math.abs(cents) > M.ADJUST_CAP_CENTS) return { error: "At most $10,000 either way. Larger corrections go through finance review, not this form." };
    return { cents };
  };

  /// Balance (and, unless compact, the history with refunds) for one credit owner.
  M.creditOwner = function (box, kind, id, { compact = false } = {}) {
    const bal = el("div", { class: "pm-balance" }, ui.loading(1));
    box.append(bal);
    M.get(`/v1/staff/credits/owners/${kind}/${enc(id)}/balance`).then((v) => {
      const d = v.data || {};
      bal.replaceChildren(ui.stats([{ label: "Available", value: M.usd(d.available_cents) }, { label: "Held", value: M.usd(d.held_cents), hint: "Reserved for running jobs" }]));
    }).catch((e) => {
      if (e instanceof PP.Redirecting) return;
      bal.replaceChildren(e.status === 404 ? ui.empty({ icon: "wallet", title: "No credit account yet" }) : M.errorBox(e));
    });
    if (compact) return;
    const hist = el("div");
    box.append(el("h3", { class: "pm-sub" }, "History"), hist);
    M.list(hist, {
      search: false, limit: 50, caption: "Credit history",
      fetch: (p) => M.get(`/v1/staff/credits/owners/${kind}/${enc(id)}/history?${p}`),
      empty: { icon: "wallet", title: "No credit activity" },
      columns: [
        { key: "at", label: "When", render: (r) => M.time(r.at) },
        { key: "type", label: "Type", render: (r) => ui.badge(r.type) },
        { key: "amount_cents", label: "USD credit", render: (r) => M.usd(r.amount_cents), num: true },
        { key: "charge", label: "Charged", render: (r) => (r.charge ? M.minor(r.charge.amount_minor, r.charge.currency) : "—") },
        { key: "fx", label: "FX", render: (r) => (r.fx && (r.fx.rate || r.fx.usd_rate) ? String(r.fx.rate || r.fx.usd_rate) : "—") },
        { key: "status", label: "Status", render: (r) => M.status(r.status) },
        { key: "id", label: "", render: (r) => (r.type === "purchase" && r.status !== "refunded" ? refundAction(r) : r.type === "adjustment" ? el("span", { class: "pp-muted" }, r.reason || "") : "") },
      ],
    });
  };

  function refundAction(p) {
    const left = Number(p.amount_cents || 0) - Number(p.refunded_cents || 0);
    return M.reasonAction("Refund", async (reason) => {
      const v = await M.post(`/v1/staff/credits/purchases/${enc(p.id)}/refund`, { reason });
      return `Refund ${v.data && v.data.status ? v.data.status : "requested"}.`;
    }, { danger: true, field: `Refund what is left of ${M.usd(left)} to the card`, max: 255 });
  }

  function resolveAction(r) {
    const wrap = el("div", { class: "pm-inline" });
    const btn = ui.button("Resolve", { size: "sm", onclick: () => {
      btn.hidden = true;
      wrap.append(M.form([
        { name: "outcome", label: "What the iyzico panel shows", type: "select", required: true, options: [["", "Choose…"], ["succeeded", "iyzico refunded it (close)"], ["failed", "iyzico did not (give the credit back)"]] },
        { name: "reason", label: "Reason", required: true, maxlength: 255 },
        { name: "ref", label: "iyzico reference", maxlength: 128 },
      ], { submit: "Resolve", onSubmit: async (v) => {
        await M.post(`/v1/staff/credits/refunds/${enc(r.refund_id)}/resolve`, { outcome: v.outcome, reason: v.reason, ...(v.ref ? { provider_reference: v.ref } : {}) });
        return "Resolved.";
      } }));
    } });
    wrap.append(btn);
    return wrap;
  }

  /// The adjustment form. One idempotency key per adjustment: kept across a retry of a failed
  /// submit (so a retry adjusts at most once), replaced after a success.
  M.adjustForm = function (prefill = {}) {
    let key = "adj-" + PP.random(10);
    const form = M.form([
      { name: "owner_kind", label: "Owner", type: "select", required: true, value: prefill.kind || "account", options: [["account", "Account"], ["org", "Organization"]] },
      { name: "owner_id", label: "Owner id", required: true, placeholder: "acct_… or org_…", value: prefill.id || "", maxlength: 128 },
      { name: "amount", label: "Amount in USD", type: "number", required: true, step: "0.01", min: "-10000", max: "10000", placeholder: "e.g. 5.00 or -5.00",
        hint: "Positive adds credit, negative removes it. At most $10,000 either way.", validate: (v) => M.adjustCents(v).error || "" },
      { name: "reason", label: "Reason", required: true, maxlength: 255, wide: true },
      { name: "confirm", label: "I checked the owner and the amount. This is recorded under my name.", type: "checkbox", required: true },
    ], {
      submit: "Apply adjustment", cls: "pm-form-grid",
      onSubmit: async (v, f) => {
        const { cents, error } = M.adjustCents(v.amount);
        if (error) throw new Error(error);
        const ok = await ui.confirm({ title: "Apply this adjustment?", text: `${cents > 0 ? "Add" : "Remove"} ${M.usd(Math.abs(cents))} ${cents > 0 ? "to" : "from"} ${v.owner_kind} ${v.owner_id}. It is recorded under your name.`, confirm: "Apply adjustment", danger: cents < 0 });
        if (!ok) return "Not applied.";
        await M.post("/v1/staff/credits/adjustments", { owner_kind: v.owner_kind, owner_id: v.owner_id, amount_cents: cents, reason: v.reason, idempotency_key: key });
        key = "adj-" + PP.random(10);
        f.reset();
        return `Adjusted ${v.owner_id} by ${M.usd(cents)}.`;
      },
    });
    return form;
  };

  M.page({
    key: "credits", title: "Credits", group: "Money", icon: "wallet", allow: M.allow(["finance"]),
    render: (view, ctx, [kind0, id0]) => M.guard(view, () => {
      view.append(ui.header({ title: "Credits", text: "Balances, purchases, holds, sales, refunds and adjustments, from the finance service. Every view is recorded." }));
      if (ctx.me && ctx.me.finance_configured === false) view.append(ui.gate({ text: "The finance service is not configured on this postbox. These screens will answer “not configured”.", tone: "warning" }));
      const TABS = [{ key: "ledger", label: "Balances & ledger" }, { key: "refunds", label: "Refunds" }, { key: "sales", label: "Sales" }, { key: "adjust", label: "Adjust a balance" }];
      let bar = el("div");
      const panel = el("div", { class: "pm-tab-panel" });
      view.append(bar, panel);
      const pick = (t) => {
        bar.replaceWith(bar = ui.tabs(TABS, t, pick));
        panel.replaceChildren();
        if (t === "ledger") {
          const owner = el("div");
          const look = ui.form([
            { name: "kind", label: "Owner", type: "select", value: kind0 || "account", options: [["account", "Account"], ["org", "Organization"]] },
            { name: "id", label: "Owner id", required: true, value: id0 || "", placeholder: "acct_… or org_…" },
          ], { submit: "Look up", cls: "pm-form-inline", onSubmit: (v) => { PP.go("credits", v.kind, v.id); } });
          panel.append(ui.card({ title: "Balance and history", text: "Look up an account or organization." }, look, owner));
          if (id0) M.creditOwner(owner, kind0 === "org" ? "org" : "account", id0);
          else owner.append(ui.empty({ icon: "wallet", title: "Look up an owner", text: "Their balance, purchases and history appear here. Purchases can be refunded from the history." }));
        }
        if (t === "refunds") {
          const box = el("div");
          panel.append(ui.card({ title: "Refunds", text: "Uncertain refunds are stuck: check the iyzico panel, then resolve. Failed gives the buyer the credit back." }, box));
          M.list(box, {
            search: false, caption: "Refunds",
            chips: { name: "status", label: "Status", value: "uncertain", options: [["uncertain", "Uncertain"], ["pending", "Pending"], ["failed", "Failed"], ["succeeded", "Succeeded"], ["", "All"]] },
            fetch: (p) => M.get(`/v1/staff/credits/refunds?${p}`), empty: { icon: "check", title: "No refunds in this state" },
            columns: [
              { key: "created_at", label: "Requested", render: (r) => M.time(r.created_at) },
              { key: "owner_id", label: "Owner", render: (r) => M.link(r.owner_id, "credits", r.owner_kind, r.owner_id) },
              { key: "credit_cents", label: "Credit", render: (r) => M.usd(r.credit_cents), num: true },
              { key: "charge", label: "To card", render: (r) => (r.charge ? M.minor(r.charge.amount_minor, r.charge.currency) : "—") },
              { key: "status", label: "Status", render: (r) => M.status(r.status) },
              { key: "refund_id", label: "", render: (r) => (r.status === "uncertain" || r.status === "pending" ? resolveAction(r) : "") },
            ],
          });
        }
        if (t === "sales") {
          const box = el("div");
          panel.append(ui.card({ title: "Credit sales", text: "Third-party sales paid with credits: price, Piyote commission and the publisher's share." }, box));
          M.list(box, {
            search: false, caption: "Credit sales",
            fetch: (p) => M.get(`/v1/staff/credits/sales?${p}`), empty: { icon: "money", title: "No credit sales yet" },
            columns: [
              { key: "sold_at", label: "Sold", render: (r) => M.time(r.sold_at) },
              { key: "listing_id", label: "Listing", mono: true },
              { key: "publisher_id", label: "Publisher", mono: true },
              { key: "amount_cents", label: "Price", render: (r) => M.usd(r.amount_cents), num: true, sort: (r) => Number(r.amount_cents || 0) },
              { key: "commission_cents", label: "Commission", render: (r) => M.usd(r.commission_cents), num: true },
              { key: "publisher_cents", label: "Publisher share", render: (r) => M.usd(r.publisher_cents), num: true },
              { key: "owner_id", label: "Buyer", render: (r) => (r.owner_id ? M.link(r.owner_id, "credits", r.owner_kind, r.owner_id) : "—") },
            ],
          });
        }
        if (t === "adjust") panel.append(ui.card({ title: "Adjust a balance", text: "For goodwill credit and corrections. Every adjustment is recorded under your name with its reason." }, M.adjustForm({ kind: kind0, id: id0 })));
      };
      pick("ledger");
    }),
  });

  M.page({
    key: "payouts", title: "Payouts", group: "Money", icon: "money", tag: "soon", allow: M.allow(["finance"]),
    render: M.soon({
      title: "Payouts", text: "Monthly publisher payouts by SWIFT, EFT or havale.",
      why: "The payout run (build F) has no postbox route yet. Until it ships, publisher payout details are looked up under Partners → Businesses and balances under Partners → Partner analytics.",
      holds: ["Monthly statements per publisher and currency", "Invoice received / missing, which gates each payout", "Withholding (stopaj) once the accountant confirms the rates", "TRY conversion for Turkish payees", "Paid / failed with the bank reference"],
    }),
  });
  M.page({
    key: "invoices", title: "Invoices", group: "Money", icon: "invoice", tag: "soon", allow: M.allow(["finance"]),
    render: M.soon({
      title: "Invoices", text: "Invoices Piyote issues and publisher invoices it receives.",
      why: "There is no staff invoice route on the postbox yet; invoices live in the billing service (saas_be) and its own panel for now.",
      holds: ["Customer invoices for plans, credits and Nests", "Publisher invoices uploaded against payout statements", "Refund and credit notes"],
    }),
  });
})();
