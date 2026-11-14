// Analytics: usage and earnings per app and in total, for a date range (UTC). All aggregation
// and authorization happen on the postbox.
//   GET /v1/publishers/{p}/analytics?from=…T00:00:00Z&to=…T00:00:00Z[&listing_id=]
(function () {
  "use strict";
  const PP = window.PP, PPP = window.PPP, ui = PP.ui;
  const el = (...a) => PP.el(...a);
  const NS = "http://www.w3.org/2000/svg";

  const day = (d) => d.toISOString().slice(0, 10);
  const addDay = (s) => new Date(Date.parse(`${s}T00:00:00Z`) + 86400000).toISOString().slice(0, 10);
  const range = { from: null, to: null, app: "", currency: "USD" };

  const csvCell = (v) => `"${String(v ?? "").replace(/^[=+@\-\t\r]/, "'$&").replace(/"/g, '""')}"`;
  PPP.analyticsCsv = function (data) {
    const keys = ["sales", "gross_minor", "tax_minor", "commission_minor", "publisher_minor", "reversed_publisher_minor", "net_publisher_minor"];
    const rows = [["App ID", "App", "Currency", "Jobs", "Succeeded", "Failed", "Cancelled", "In progress", "Active users", "New installations", "Active installations now", "Sales", "Gross minor", "Tax minor", "Commission minor", "Publisher earned minor", "Reversed publisher minor", "Net publisher minor"]];
    const available = data.finance && data.finance.status === "available";
    for (const a of data.apps || []) {
      const earn = available ? data.finance.apps.filter((r) => r.listing_id === a.id) : [];
      for (const r of earn.length ? earn : [null]) {
        const u = a.usage;
        rows.push([a.id, a.title, r ? r.currency : "", u.jobs, u.succeeded, u.failed, u.cancelled, u.in_progress, u.active_users, u.new_installations, u.active_installations, ...keys.map((k) => (r ? r[k] : available ? 0 : ""))]);
      }
    }
    return rows.map((r) => r.map(csvCell).join(",")).join("\r\n");
  };

  /// Daily columns as SVG (attributes only: the page's CSP has no inline styles).
  function bars(points, label, format) {
    const max = Math.max(1, ...points.map((p) => p.value));
    const svg = document.createElementNS(NS, "svg");
    svg.setAttribute("viewBox", "0 0 720 160");
    svg.setAttribute("class", "ppp-chart");
    svg.setAttribute("role", "img");
    svg.setAttribute("aria-label", `${label}; the daily values are in the table below`);
    svg.setAttribute("preserveAspectRatio", "none");
    const w = 720 / Math.max(1, points.length);
    for (const g of [0.25, 0.5, 0.75, 1]) {
      const line = document.createElementNS(NS, "line");
      line.setAttribute("x1", "0"); line.setAttribute("x2", "720");
      line.setAttribute("y1", String(150 - g * 140)); line.setAttribute("y2", String(150 - g * 140));
      line.setAttribute("class", "ppp-chart-grid");
      svg.append(line);
    }
    points.forEach((p, i) => {
      const h = (p.value / max) * 140;
      const r = document.createElementNS(NS, "rect");
      r.setAttribute("x", String(i * w + Math.min(2, w * 0.15)));
      r.setAttribute("width", String(Math.max(1, w - Math.min(4, w * 0.3))));
      r.setAttribute("y", String(150 - h));
      r.setAttribute("height", String(Math.max(p.value ? 2 : 0, h)));
      r.setAttribute("rx", "2");
      r.setAttribute("class", "ppp-chart-bar");
      const t = document.createElementNS(NS, "title");
      t.textContent = `${p.day}: ${format(p.value)}`;
      r.append(t);
      svg.append(r);
    });
    const peak = points.some((p) => p.value) ? format(max) : format(0);
    return ui.card({ title: label, text: `Daily peak ${peak}` }, svg,
      el("details", { class: "ppp-details" }, el("summary", {}, "Daily values"),
        ui.table({ dense: true, pageSize: 31, columns: [{ key: "day", label: "Date (UTC)" }, { key: "value", label, num: true, render: (r) => format(r.value) }], rows: points.slice().reverse() })));
  }

  PPP.page({
    key: "analytics", title: "Analytics", group: "Performance", icon: "chart", order: 5,
    async render(view, ctx) {
      view.append(ui.header({ title: "Analytics", text: "How much your apps are used and what they earn. Dates are UTC.", actions: PPP.picker(ctx) }));
      if (!ctx.pub) { view.append(PPP.noPublisher(ctx, "Analytics")); return; }
      if (!range.from) { const now = new Date(); range.to = day(now); range.from = day(new Date(now.getTime() - 29 * 86400000)); }

      const controls = el("form", { class: "ppp-controls", "aria-label": "Report range" });
      const app = el("select", { class: "pp-input pp-select", "aria-label": "App" }, el("option", { value: "" }, "All apps"));
      const from = el("input", { class: "pp-input", type: "date", value: range.from, "aria-label": "From" });
      const to = el("input", { class: "pp-input", type: "date", value: range.to, "aria-label": "Through" });
      const presets = el("div", { class: "ppp-presets" }, [7, 30, 90, 365].map((n) => ui.button(n === 365 ? "12 months" : `${n} days`, { size: "sm", kind: "quiet", onclick: () => {
        range.to = day(new Date()); range.from = day(new Date(Date.parse(`${range.to}T00:00:00Z`) - (n - 1) * 86400000)); PP.shell.refresh();
      } })));
      controls.append(el("label", {}, el("span", {}, "App"), app), el("label", {}, el("span", {}, "From"), from), el("label", {}, el("span", {}, "Through"), to), ui.button("Update", { type: "submit", kind: "primary" }), presets);
      controls.addEventListener("submit", (e) => { e.preventDefault(); range.from = from.value; range.to = to.value; range.app = app.value; PP.shell.refresh(); });
      app.addEventListener("change", () => { range.app = app.value; range.from = from.value; range.to = to.value; PP.shell.refresh(); });
      view.append(controls);

      const end = addDay(range.to);
      const span = Date.parse(`${end}T00:00:00Z`) - Date.parse(`${range.from}T00:00:00Z`);
      if (!(span > 0) || span > 366 * 86400000) { view.append(ui.gate({ tone: "danger", text: "Choose a date range of at most 366 days, with the start before the end." })); return; }
      const q = new URLSearchParams({ from: `${range.from}T00:00:00Z`, to: `${end}T00:00:00Z` });
      if (range.app) q.set("listing_id", range.app);
      const data = (await PP.postbox(`${PPP.base(ctx)}/analytics?${q}`)).data;
      for (const a of data.app_choices || []) app.append(el("option", { value: a.id, selected: a.id === range.app ? true : null }, a.title));

      const u = data.usage || {};
      const f = data.finance || {};
      const available = f.status === "available";
      const currencies = available ? (f.currencies || []) : [];
      if (available && currencies.length && !currencies.some((c) => c.currency === range.currency)) range.currency = currencies[0].currency;
      const cur = currencies.find((c) => c.currency === range.currency);
      view.append(ui.stats([
        { label: "Jobs", value: PPP.count(u.jobs), hint: `${PPP.count(u.succeeded)} succeeded · ${PPP.count(u.failed)} failed` },
        { label: "Active users", value: PPP.count(u.active_users), hint: "Distinct accounts that ran a job" },
        { label: "New installations", value: PPP.count(u.new_installations), hint: `${PPP.count(u.active_installations)} active now` },
        { label: `Net earnings (${range.currency})`, value: available ? ui.money(cur ? cur.net_publisher_minor : 0, range.currency) : "—", hint: available ? `Gross ${ui.money(cur ? cur.gross_minor : 0, range.currency)}` : "Not available right now", tone: available ? "success" : null },
      ]));
      if (!available) view.append(ui.gate({ tone: "warning", text: f.message || "Earnings are unavailable right now. Missing amounts are not zero." }));

      const days = [];
      for (let ms = Date.parse(`${range.from}T00:00:00Z`); ms < Date.parse(`${end}T00:00:00Z`); ms += 86400000) days.push(day(new Date(ms)));
      const charts = el("div", { class: "ppp-chart-grid-2" }, bars(days.map((d) => ({ day: d, value: ((data.daily || []).find((r) => r.day === d) || {}).jobs || 0 })), "Jobs per day", PPP.count));
      if (available) charts.append(bars(days.map((d) => ({ day: d, value: (f.daily || []).filter((r) => r.day === d && r.currency === range.currency).reduce((n, r) => n + Number(r.net_publisher_minor || 0), 0) })), `Net earnings per day (${range.currency})`, (v) => ui.money(v, range.currency)));
      view.append(charts);

      const earn = (id) => (available ? (f.apps || []).find((r) => r.listing_id === id && r.currency === range.currency) : null);
      const curSel = available && currencies.length > 1 ? el("select", { class: "pp-input pp-select", "aria-label": "Currency" }, currencies.map((c) => el("option", { value: c.currency, selected: c.currency === range.currency ? true : null }, c.currency))) : null;
      if (curSel) curSel.addEventListener("change", () => { range.currency = curSel.value; PP.shell.refresh(); });
      view.append(ui.section({ title: "Per app", text: "Jobs by submission date with their current outcome; active installations are a snapshot of now.",
        actions: [curSel, ui.button("Export CSV", { icon: "docs", onclick: () => {
          const url = URL.createObjectURL(new window.Blob([PPP.analyticsCsv(data)], { type: "text/csv;charset=utf-8" }));
          const a = el("a", { href: url, download: `partner-analytics-${range.from}-${range.to}.csv` });
          document.body.append(a); a.click(); a.remove();
          window.setTimeout(() => URL.revokeObjectURL(url), 1000);
        } })] },
      ui.table({
        caption: "Usage and earnings per app", sort: { key: "jobs", dir: "desc" },
        columns: [
          { key: "title", label: "App", sort: true, render: (a) => el("span", {}, el("strong", {}, a.title), " ", el("span", { class: "pp-faint" }, a.status || "")) },
          { key: "jobs", label: "Jobs", num: true, sort: (a) => a.usage.jobs, render: (a) => PPP.count(a.usage.jobs) },
          { key: "succeeded", label: "Succeeded", num: true, render: (a) => PPP.count(a.usage.succeeded) },
          { key: "users", label: "Active users", num: true, sort: (a) => a.usage.active_users, render: (a) => PPP.count(a.usage.active_users) },
          { key: "installs", label: "Installs now", num: true, render: (a) => PPP.count(a.usage.active_installations) },
          { key: "gross", label: `Gross (${range.currency})`, num: true, render: (a) => (available ? ui.money((earn(a.id) || {}).gross_minor || 0, range.currency) : "—") },
          { key: "net", label: `Net (${range.currency})`, num: true, sort: (a) => (earn(a.id) || {}).net_publisher_minor || 0, render: (a) => (available ? ui.money((earn(a.id) || {}).net_publisher_minor || 0, range.currency) : "—") },
        ],
        rows: data.apps || [],
        empty: { icon: "chart", title: "No apps in this report yet", text: "Once you publish an app, its usage and earnings appear here.", action: { label: "Go to listings", href: PP.href("listings") } },
      })));

      if (available && currencies.length) {
        view.append(ui.section({ title: "Earnings by currency", text: "Net is after Pigeonpost's commission and reversals, before payout withholding. Currencies are never added together." },
          ui.table({
            caption: "Earnings by currency",
            columns: [
              { key: "currency", label: "Currency" },
              { key: "sales", label: "Sales", num: true, render: (r) => PPP.count(r.sales) },
              ...[["gross_minor", "Gross"], ["tax_minor", "Tax"], ["commission_minor", "Pigeonpost 10%"], ["reversed_publisher_minor", "Refunds"], ["net_publisher_minor", "Net"]].map(([k, label]) => ({ key: k, label, num: true, render: (r) => ui.money(r[k], r.currency) })),
            ],
            rows: currencies,
          })));
      }
    },
  });
})();
