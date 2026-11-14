// Purchases & jobs: runs of publisher-hosted services (state, charge, result files to download,
// cancel before the result), installed services, and paid marketplace licences.
//
// Postbox: GET /v1/marketplace/hosted-jobs, POST …/{job}/cancel, GET …/{job}/files/{index} (bearer),
//          GET /v1/nest-installations. Bridge: GET /v1/marketplace/licences.
(function () {
  "use strict";
  const el = (...a) => PP.el(...a);
  const LICENCE = { active: ["Active", "success"], canceling: ["Ends at period end", "warning"], paused: ["Payment due", "warning"], expired: ["Ended", "neutral"], terminated: ["Ended", "neutral"], revoked: ["Refunded", "danger"] };

  function jobState(j) {
    if (j.execution === "succeeded" && j.delivery === "undeliverable") return PP.ui.badge("Done — not delivered; download it here", "warning");
    return PP.ui.status(j.execution, ACC.JOB_STATUS);
  }

  ACC.page({
    key: "purchases", title: "Purchases & jobs", group: "Nests & services", icon: "jobs",
    async render(view, ctx, params) {
      view.append(PP.ui.header({ title: "Purchases & jobs", text: "What you have run, installed and bought from the marketplace." }));
      if (!ACC.feature(ctx, "marketplace")) {
        view.append(PP.ui.card({}, ACC.notYet("The marketplace isn't open on this account yet", "Services you run, install or buy will be listed here.", "jobs")));
        return;
      }
      const tab = ["installed", "licences"].includes(params[0]) ? params[0] : "jobs";
      view.append(PP.ui.tabs([{ key: "jobs", label: "Runs" }, { key: "installed", label: "Installed services" }, { key: "licences", label: "Licences" }], tab, (k) => PP.go("purchases", k === "jobs" ? "" : k)));
      if (tab === "jobs") {
        const v = await ACC.pb("/v1/marketplace/hosted-jobs");
        view.append(PP.ui.table({
          caption: "Runs",
          search: "Search runs",
          filters: [{ key: "execution", label: "State", options: Object.entries(ACC.JOB_STATUS).map(([k, [t]]) => [k, t]) }],
          sort: { key: "created_at", dir: "desc" },
          columns: [
            { key: "created_at", label: "Started", sort: true, render: (j) => ACC.date(j.created_at) },
            { key: "listing_title", label: "Service", sort: true, render: (j) => el("a", { href: PP.href("store", "services", j.listing_id) }, j.listing_title || j.listing_id) },
            { key: "operation", label: "Operation" },
            { key: "execution", label: "State", render: jobState },
            { key: "charge", label: "Cost", render: (j) => (j.charge ? `${ACC.usd(j.charge.amount_cents)} ${ACC.CHARGE_TEXT[j.charge.status] || j.charge.status}` : "Free") },
            { key: "result", label: "Result", render: (j) => {
              const files = (j.result && j.result.files) || [];
              const summary = j.result && j.result.summary ? el("span", { class: "pp-faint" }, String(j.result.summary).slice(0, 140)) : null;
              return el("div", { class: "acc-cell-stack" }, summary, files.map((f) => ACC.btn(f.name || `File ${f.index + 1}`, { size: "sm", icon: "docs", attrs: { "data-file": f.download_path }, onclick: () => ACC.download(f.download_path, f.name) })),
                ["queued", "running"].includes(j.execution) ? ACC.btn("Cancel", { size: "sm", kind: "danger", attrs: { "data-cancel-job": j.job_id }, onclick: async () => {
                  await ACC.pb(`/v1/marketplace/hosted-jobs/${encodeURIComponent(j.job_id)}/cancel`, { method: "POST", body: {} });
                  PP.toast(j.charge ? "Cancelled. The reserved credit comes back to you within a minute." : "Cancelled.");
                  ACC.forget("balance");
                  PP.shell.refresh();
                } }) : null);
            } },
          ],
          rows: v.data || [],
          empty: { icon: "jobs", title: "Nothing run yet", text: "Run a publisher-hosted service from the Store; its result arrives in your inbox and here.", action: { label: "Browse services", href: PP.href("store", "services") } },
        }));
        return;
      }
      if (tab === "installed") {
        const v = await ACC.pb("/v1/nest-installations");
        view.append(PP.ui.table({
          caption: "Installed services",
          columns: [
            { key: "listing_id", label: "Service", render: (i) => el("span", { class: "pp-mono", translate: "no" }, i.address || i.listing_id) },
            { key: "version", label: "Version", render: (i) => [i.version || "?", i.update_available ? [" ", PP.ui.badge("Update available", "info")] : null] },
            { key: "status", label: "State", render: (i) => PP.ui.status(i.status, { active: ["Active", "success"], removed: ["Removed", "neutral"] }) },
            { key: "funding", label: "Paid", render: (i) => (i.funding === "card" ? "Card" : i.funding === "test_credits" ? "Free test" : "Free") },
            { key: "manage", label: "", render: (i) => ACC.btn("Manage", { size: "sm", href: PP.href("store", "services", i.listing_id) }) },
          ],
          rows: (v.data || []).filter((i) => i.status !== "removed"),
          empty: { icon: "store", title: "No services installed", text: "Install one from the Store.", action: { label: "Browse services", href: PP.href("store", "services") } },
        }));
        return;
      }
      const v = await ACC.bridgeGet("/v1/marketplace/licences", { keepSession: true });
      view.append(PP.ui.table({
        caption: "Licences",
        columns: [
          { key: "listing_id", label: "Service", render: (l) => el("a", { href: PP.href("store", "services", l.listing_id) }, l.listing_id) },
          { key: "term", label: "Term", render: (l) => (l.term === "monthly" ? "Monthly" : "One-off") },
          { key: "status", label: "State", render: (l) => PP.ui.status(l.status, LICENCE) },
          { key: "paid_through", label: "Paid through", sort: true, render: (l) => (l.paid_through ? ACC.date(l.paid_through) : "—") },
        ],
        rows: v.licences || [],
        empty: { icon: "invoice", title: "No paid services", text: "Services you buy are listed here with their licence." },
      }), el("p", { class: "pp-faint" }, "Receipts for each payment are on the service's page and in Invoices."));
    },
  });
})();
