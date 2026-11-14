// Earnings & payouts. The postbox has no publisher statement route yet (payout build F), so this
// page says what the first statement (2026-12-01) will show and fills what is already known: the
// balances from analytics (payable now, reserve, paid out) and the payout account on file.
//   GET /v1/publishers/{p}/analytics?from&to     finance.publisher_balances
(function () {
  "use strict";
  const PP = window.PP, PPP = window.PPP, ui = PP.ui;
  const el = (...a) => PP.el(...a);

  PPP.FIRST_STATEMENT = "2026-12-01";

  PPP.page({
    key: "earnings", title: "Earnings & payouts", group: "Performance", icon: "money", order: 6,
    async render(view, ctx) {
      view.append(ui.header({ title: "Earnings & payouts", text: "Monthly statements, what Pigeonpost keeps, withholding and what is paid to you.", actions: PPP.picker(ctx) }));
      if (!ctx.pub) { view.append(PPP.noPublisher(ctx, "Earnings and payouts")); return; }
      let balances = null, finance = null;
      if (PPP.approved(ctx)) {
        const now = new Date();
        const from = new Date(now.getTime() - 89 * 86400000).toISOString().slice(0, 10);
        const to = new Date(now.getTime() + 86400000).toISOString().slice(0, 10);
        try {
          const r = await PP.postbox(`${PPP.base(ctx)}/analytics?${new URLSearchParams({ from: `${from}T00:00:00Z`, to: `${to}T00:00:00Z` })}`);
          finance = r.data && r.data.finance;
          if (finance && finance.status === "available") balances = finance.publisher_balances || [];
        } catch (e) { if (e instanceof PP.Redirecting) throw e; }
      }
      const first = new Date(`${PPP.FIRST_STATEMENT}T00:00:00Z`);
      const firstText = first.toLocaleDateString(PP.locale(), { year: "numeric", month: "long", day: "numeric", timeZone: "UTC" });

      view.append(el("section", { class: "ppp-hero ppp-hero-info", "aria-label": "Statements" },
        el("div", { class: "ppp-hero-main" },
          el("p", { class: "pp-eyebrow" }, "Monthly statements"),
          el("h2", {}, `Your first statement arrives on ${firstText}`),
          el("p", { class: "pp-lede" }, "Each month Pigeonpost sends a statement for the month before. You upload your invoice for the net amount, and the payout follows by SWIFT, EFT or havale to the account on file. Turkish payees can be paid in TRY.")),
        el("div", { class: "ppp-statement-sample", "aria-label": "What a statement shows" },
          el("p", { class: "pp-eyebrow" }, "Each statement shows"),
          ui.kv([
            ["Gross sales", "What buyers paid for your apps"],
            ["Pigeonpost 10%", "Piyote's commission as merchant of record"],
            ["Refunds", "Refunds and chargebacks for those sales"],
            ["Withholding", "Stopaj, where it applies to you"],
            ["Net payout", "What is paid to you"],
          ]))));

      view.append(ui.section({ title: "Balances", text: "Current, across all your apps. Paid out counts bank-reconciled transfers in the last 90 days." },
        balances && balances.length
          ? ui.table({
            caption: "Balances",
            columns: [
              { key: "currency", label: "Currency" },
              { key: "payable_minor", label: "Payable now", num: true, render: (r) => ui.money(r.payable_minor, r.currency) },
              { key: "reserve_minor", label: "Reserve held", num: true, render: (r) => ui.money(r.reserve_minor, r.currency) },
              { key: "recoverable_minor", label: "Recoverable", num: true, render: (r) => ui.money(r.recoverable_minor, r.currency) },
              { key: "paid_out_minor", label: "Paid out (90 days)", num: true, render: (r) => ui.money(r.paid_out_minor, r.currency) },
            ],
            rows: balances,
          })
          : ui.card({}, ui.empty({ icon: "money", title: PPP.approved(ctx) ? (finance && finance.status !== "available" ? "Balances aren't available right now" : "Nothing earned yet") : "Earnings start after approval", text: PPP.approved(ctx) ? "Once buyers run your apps, what you have earned and what is payable appears here." : "Finish your application, publish an app and set its price.", action: PPP.approved(ctx) ? { label: "Open analytics", href: PP.href("analytics") } : { label: "Your application", href: PP.href("application") } }))));

      const d = (ctx.app && ctx.app.details) || {};
      const acct = d.iban ? `IBAN •••• ${d.iban.replace(/\s/g, "").slice(-4)}` : d.account_number ? `•••• ${d.account_number.slice(-4)}` : "";
      view.append(el("div", { class: "pp-cols" },
        ui.card({ title: "Statements", text: "Download each month's statement and upload your invoice against it." },
          ui.table({ columns: [{ key: "m", label: "Month" }, { key: "g", label: "Gross" }, { key: "n", label: "Net" }, { key: "s", label: "Status" }], rows: [], empty: { icon: "invoice", title: "No statements yet", text: `The first one covers November 2026 and arrives on ${firstText}.` } }),
          ui.gate({ tone: "info", text: "Invoice upload opens with your first statement." }, ui.dropzone({ label: "Upload your invoice (PDF)", accept: "application/pdf", onFiles: () => {} }))),
        ui.card({ title: "Payout account", text: "From your application.", actions: PPP.editable(ctx) ? ui.button("Edit", { size: "sm", href: PP.href("application", "bank") }) : ui.button("Change", { size: "sm", href: PP.href("support") }) },
          ui.kv([["Bank", [d.bank_name, d.bank_country].filter(Boolean).join(" · ")], ["Account holder", d.account_holder], ["Account", acct], ["Payout currency", d.currency], ["Tax ID", d.tax_id ? `•••• ${String(d.tax_id).slice(-3)}` : ""]]))));
    },
  });
})();
