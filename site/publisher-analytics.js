// Shared publisher/staff analytics. All authorization and aggregation happen on the server.
(function () {
  "use strict";
  const MP = (window.PPMarketplace = window.PPMarketplace || {});
  function el(tag, attrs, ...children) {
    const node = document.createElement(tag);
    for (const [key, value] of Object.entries(attrs || {})) {
      if (value == null || value === false) continue;
      if (key === "text") node.textContent = value;
      else if (key === "class") node.className = value;
      else if (key.startsWith("on")) node.addEventListener(key.slice(2), value);
      else node.setAttribute(key, value === true ? "" : String(value));
    }
    for (const child of children.flat())
      if (child != null && child !== false) node.append(child);
    return node;
  }
  const number = (n) => Number(n || 0).toLocaleString();
  const money = (n, currency) =>
    n == null
      ? "—"
      : new Intl.NumberFormat(undefined, {
          style: "currency",
          currency,
        }).format(n / 100);
  const day = (d) => d.toISOString().slice(0, 10);
  const addDay = (s) =>
    new Date(Date.parse(s + "T00:00:00Z") + 86400000).toISOString();
  const csvCell = (value) =>
    '"' +
    String(value ?? "")
      .replace(/^[=+@\-\t\r]/, "'$&")
      .replace(/"/g, '""') +
    '"';
  MP.analyticsCsv = function (data) {
    const header = [
      "App ID",
      "App",
      "Currency",
      "Jobs",
      "Succeeded",
      "Failed",
      "Cancelled",
      "In progress",
      "Active users",
      "New installations",
      "Active installations now",
      "Sales",
      "Gross minor",
      "Tax minor",
      "Commission minor",
      "Publisher earned minor",
      "Reversed publisher minor",
      "Net publisher minor",
    ];
    const rows = [header];
    for (const app of data.apps) {
      const earnings =
        data.finance.status === "available"
          ? data.finance.apps.filter((r) => r.listing_id === app.id)
          : [];
      for (const r of earnings.length ? earnings : [null]) {
        const u = app.usage;
        rows.push([
          app.id,
          app.title,
          r?.currency || "",
          u.jobs,
          u.succeeded,
          u.failed,
          u.cancelled,
          u.in_progress,
          u.active_users,
          u.new_installations,
          u.active_installations,
          ...[
            "sales",
            "gross_minor",
            "tax_minor",
            "commission_minor",
            "publisher_minor",
            "reversed_publisher_minor",
            "net_publisher_minor",
          ].map((k) =>
            r ? r[k] : data.finance.status === "available" ? 0 : "",
          ),
        ]);
      }
    }
    return rows.map((r) => r.map(csvCell).join(",")).join("\r\n");
  };
  function table(headings, rows, label) {
    return el(
      "div",
      {
        class: "pa-table-wrap",
        tabindex: "0",
        role: "region",
        "aria-label": label,
      },
      el(
        "table",
        { class: "pa-table" },
        el("caption", { class: "pa-sr-only", text: label }),
        el(
          "thead",
          {},
          el(
            "tr",
            {},
            headings.map((h) => el("th", { scope: "col", text: h })),
          ),
        ),
        el(
          "tbody",
          {},
          rows.map((row) =>
            el(
              "tr",
              {},
              row.map((v, i) =>
                el(i === 0 ? "th" : "td", i === 0 ? { scope: "row" } : {}, v),
              ),
            ),
          ),
        ),
      ),
    );
  }
  function trend(points, label, format) {
    const section = el(
      "section",
      { class: "pa-trend" },
      el("h3", { text: label }),
    );
    const ns = "http://www.w3.org/2000/svg",
      svg = document.createElementNS(ns, "svg");
    svg.setAttribute("viewBox", "0 0 720 150");
    svg.setAttribute("role", "img");
    svg.setAttribute(
      "aria-label",
      label + "; daily values follow in the data table",
    );
    const max = Math.max(1, ...points.map((p) => p.value));
    const line = document.createElementNS(ns, "polyline");
    line.setAttribute(
      "points",
      points
        .map(
          (p, i) =>
            `${10 + (i * 700) / Math.max(1, points.length - 1)},${140 - (p.value / max) * 125}`,
        )
        .join(" "),
    );
    line.setAttribute("fill", "none");
    line.setAttribute("stroke", "currentColor");
    line.setAttribute("stroke-width", "3");
    svg.append(line);
    section.append(
      el("p", {
        class: "muted",
        text: `Daily peak: ${format(max === 1 && !points.some((p) => p.value) ? 0 : max)}`,
      }),
      svg,
      el(
        "details",
        {},
        el("summary", { text: "View daily values" }),
        table(
          ["Date (UTC)", label],
          points.map((p) => [p.day, format(p.value)]),
          label + " by day",
        ),
      ),
    );
    return section;
  }
  MP.analytics = function (root, deps, options = {}) {
    const staff = Boolean(options.staff),
      now = new Date();
    let publishers = [],
      selected = "",
      search = "",
      next = null,
      data = null,
      error = null,
      loading = false,
      seq = 0,
      pickerSeq = 0;
    let from = day(new Date(now.getTime() - 29 * 86400000)),
      to = day(now),
      app = "",
      currency = "USD",
      sort = "jobs";
    root.classList.add("pa-root");
    const status = el("p", {
      role: "status",
      "aria-live": "polite",
      class: "muted",
    });
    const body = el("div", {}),
      controls = el("form", { class: "pa-controls" });
    const pubSelect = el("select", {
      id: "pa-publisher",
      "aria-label": "Publisher",
      onchange: () => {
        selected = pubSelect.value;
        app = "";
        load();
      },
    });
    const appSelect = el(
      "select",
      {
        id: "pa-app",
        "aria-label": "App",
        onchange: () => {
          app = appSelect.value;
          load();
        },
      },
      el("option", { value: "", text: "All apps" }),
    );
    const fromInput = el("input", {
      type: "date",
      id: "pa-from",
      required: true,
      value: from,
    });
    const toInput = el("input", {
      type: "date",
      id: "pa-to",
      required: true,
      value: to,
    });
    const currencySelect = el(
      "select",
      {
        id: "pa-currency",
        "aria-label": "Currency",
        onchange: () => {
          currency = currencySelect.value;
          draw();
        },
      },
      ["USD", "EUR", "TRY"].map((c) => el("option", { value: c, text: c })),
    );
    const sortSelect = el(
      "select",
      {
        "aria-label": "Sort apps",
        onchange: () => {
          sort = sortSelect.value;
          draw();
        },
      },
      [
        ["jobs", "Most used"],
        ["earnings", "Highest net earnings"],
        ["title", "Name"],
      ].map(([v, t]) => el("option", { value: v, text: t })),
    );
    const apply = el("button", {
      type: "submit",
      class: "btn btn-primary",
      text: "Update report",
    });
    const refresh = () => {
      fromInput.value = from;
      toInput.value = to;
      load();
    };
    controls.append(
      el("label", { for: "pa-publisher" }, "Publisher", pubSelect),
      el("label", { for: "pa-app" }, "App", appSelect),
      el("label", { for: "pa-from" }, "From (UTC)", fromInput),
      el("label", { for: "pa-to" }, "Through (UTC)", toInput),
      apply,
    );
    controls.addEventListener("submit", (e) => {
      e.preventDefault();
      from = fromInput.value;
      to = toInput.value;
      load();
    });
    const presets = el(
      "div",
      { class: "pa-presets" },
      [7, 30, 90].map((n) =>
        el("button", {
          type: "button",
          class: "btn btn-small btn-secondary",
          text: `Last ${n} days`,
          onclick: () => {
            to = day(new Date());
            from = day(
              new Date(Date.parse(to + "T00:00:00Z") - (n - 1) * 86400000),
            );
            refresh();
          },
        }),
      ),
    );
    root.replaceChildren(
      el("h2", { text: "App analytics" }),
      el("p", {
        class: "muted",
        text: "Usage and earnings across your apps. All dates use UTC.",
      }),
      status,
    );
    if (staff) {
      const input = el("input", {
        type: "search",
        placeholder: "Publisher name or ID",
        "aria-label": "Find a publisher",
      });
      const form = el(
        "form",
        {
          class: "pa-search",
          onsubmit: (e) => {
            e.preventDefault();
            search = input.value.trim();
            pick(false);
          },
        },
        input,
        el("button", {
          type: "submit",
          class: "btn btn-secondary",
          text: "Find publishers",
        }),
      );
      const more = el("button", {
        type: "button",
        class: "btn btn-small",
        text: "More publishers",
        hidden: true,
        onclick: () => pick(true),
      });
      root.append(form, more);
      options.more = more;
    }
    root.append(controls, presets, body);
    function reportError(e) {
      return e?.status === 401
        ? "Your session ended. Sign in again."
        : e?.code === "step_up_required"
          ? "A recent strong sign-in is required."
          : MP.errorText
            ? MP.errorText(e)
            : "The report could not be loaded.";
    }
    async function pick(more) {
      const gen = ++pickerSeq;
      status.textContent = "Loading publishers…";
      try {
        const res = await deps.pb(
          staff
            ? `/v1/staff/publisher-analytics?q=${encodeURIComponent(search)}&offset=${more ? next || 0 : 0}`
            : "/v1/publishers",
        );
        if (gen !== pickerSeq) return;
        publishers = more ? [...publishers, ...res.data] : res.data || [];
        next = res.next_offset;
        if (options.more) options.more.hidden = next == null;
        pubSelect.replaceChildren(
          ...publishers.map((p) =>
            el("option", { value: p.id, text: p.display_name }),
          ),
        );
        if (!publishers.some((p) => p.id === selected))
          selected = publishers[0]?.id || "";
        pubSelect.value = selected;
        app = "";
        if (selected) load();
        else {
          ++seq;
          data = null;
          body.replaceChildren();
          status.textContent = staff
            ? "No publishers match your search."
            : "Apply as a publisher to see your app analytics.";
        }
      } catch (e) {
        if (gen !== pickerSeq) return;
        ++seq;
        data = null;
        error = e;
        loading = false;
        draw();
      }
    }
    async function load() {
      const gen = ++seq;
      data = null;
      error = null;
      if (!selected) return;
      let end;
      try {
        end = addDay(to);
      } catch (_) {
        error = { message: "Choose valid dates." };
        draw();
        return;
      }
      if (
        !from ||
        Date.parse(end) <= Date.parse(from + "T00:00:00Z") ||
        Date.parse(end) - Date.parse(from + "T00:00:00Z") > 366 * 86400000
      ) {
        error = {
          message: "Choose an increasing date range of at most 366 days.",
        };
        draw();
        return;
      }
      loading = true;
      draw();
      const params = new URLSearchParams({
        from: from + "T00:00:00Z",
        to: end,
      });
      if (app) params.set("listing_id", app);
      try {
        const res = await deps.pb(
          `/v1/${staff ? "staff/" : ""}publishers/${encodeURIComponent(selected)}/analytics?${params}`,
        );
        if (gen !== seq) return;
        data = res.data;
        appSelect.replaceChildren(
          el("option", { value: "", text: "All apps" }),
          ...data.app_choices.map((a) =>
            el("option", { value: a.id, text: a.title }),
          ),
        );
        appSelect.value = app;
      } catch (e) {
        if (gen !== seq) return;
        error = e;
      }
      if (gen === seq) {
        loading = false;
        draw();
      }
    }
    function draw() {
      body.replaceChildren();
      apply.disabled = loading;
      root.setAttribute("aria-busy", String(loading));
      status.textContent = loading
        ? "Loading report…"
        : error
          ? reportError(error)
          : data
            ? `Updated ${new Date(data.generated_at).toLocaleString()}`
            : "";
      if (error) {
        body.append(
          el("button", {
            type: "button",
            class: "btn btn-secondary",
            text:
              error.code === "step_up_required" ? "Verify sign-in" : "Retry",
            onclick: () =>
              error.code === "step_up_required" && deps.stepUp
                ? deps.stepUp(
                    error.body?.step_up || error.body || error,
                    "analytics",
                  )
                : selected ? load() : pick(false),
          }),
        );
        return;
      }
      if (!data) return;
      const u = data.usage,
        f = data.finance,
        available = f.status === "available";
      body.append(
        el(
          "div",
          { class: "pa-stats" },
          [
            ["Jobs submitted", u.jobs],
            ["Active users", u.active_users],
            ["New installations", u.new_installations],
            ["Active installations now", u.active_installations],
          ].map(([label, value]) =>
            el(
              "div",
              { class: "pa-stat" },
              el("span", { text: label }),
              el("strong", { text: number(value) }),
            ),
          ),
        ),
        el("p", {
          class: "muted",
          text: `${number(u.succeeded)} succeeded · ${number(u.failed)} failed or expired · ${number(u.cancelled)} cancelled · ${number(u.in_progress)} in progress`,
        }),
        el("p", {
          class: "muted pa-note",
          text: "Jobs are counted by submission date, with their current outcome. Active users are distinct accounts that submitted jobs, deduplicated across apps. Active installations are a current snapshot, not a historical total. Review jobs are excluded.",
        }),
      );
      if (!available)
        body.append(
          el("p", {
            class: "pa-warning",
            role: "status",
            text:
              f.message ||
              "Earnings are unavailable. Missing amounts are not zero.",
          }),
        );
      else {
        body.append(
          el("h3", { text: "Earnings for sales in this period" }),
          table(
            [
              "Currency",
              "Sales",
              "Gross sales",
              "Tax",
              "Commission",
              "Publisher earned",
              "Reversed earnings",
              "Net earnings",
            ],
            f.currencies.map((r) => [
              r.currency,
              number(r.sales),
              ...[
                "gross_minor",
                "tax_minor",
                "commission_minor",
                "publisher_minor",
                "reversed_publisher_minor",
                "net_publisher_minor",
              ].map((k) => money(r[k], r.currency)),
            ]),
            "Earnings by currency",
          ),
          el("p", {
            class: "muted pa-note",
            text: "Sales are grouped by when earnings accrued. Reversed earnings reflect refunds and chargebacks recorded so far for those sales, even if reversed later. Net earnings are after commission and reversals, before payout withholding. Currencies are never added together.",
          }),
        );
      }
      const actions = el(
        "div",
        { class: "pa-controls" },
        el("label", { for: "pa-currency" }, "Display currency", currencySelect),
        el("label", {}, "Sort apps", sortSelect),
        el("button", {
          type: "button",
          class: "btn btn-secondary",
          text: "Export app CSV",
          onclick: () => {
            const blob = new Blob([MP.analyticsCsv(data)], {
                type: "text/csv;charset=utf-8",
              }),
              url = URL.createObjectURL(blob),
              link = el("a", {
                href: url,
                download: `publisher-analytics-${from}-${to}.csv`,
              });
            link.click();
            setTimeout(() => URL.revokeObjectURL(url), 1000);
          },
        }),
      );
      currencySelect.value = currency;
      sortSelect.value = sort;
      body.append(el("h3", { text: "App performance" }), actions);
      const earnings = (id) =>
        available
          ? f.apps.find((r) => r.listing_id === id && r.currency === currency)
          : null;
      const apps = [...data.apps].sort((a, b) =>
        sort === "title"
          ? a.title.localeCompare(b.title)
          : sort === "earnings"
            ? (earnings(b.id)?.net_publisher_minor || 0) -
              (earnings(a.id)?.net_publisher_minor || 0)
            : b.usage.jobs - a.usage.jobs,
      );
      if (!apps.length)
        body.append(
          el("p", {
            text: "No apps in this report yet. Once you publish an app, its usage and earnings will appear here.",
          }),
        );
      else
        body.append(
          table(
            [
              "App",
              "Jobs",
              "Succeeded",
              "Failed / expired",
              "Active users",
              "New installs",
              "Active installs now",
              `Gross (${currency})`,
              `Net earnings (${currency})`,
            ],
            apps.map((a) => {
              const r = earnings(a.id),
                v = a.usage;
              return [
                el(
                  "span",
                  {},
                  el("strong", { text: a.title }),
                  el("small", { class: "muted", text: a.status }),
                ),
                number(v.jobs),
                number(v.succeeded),
                number(v.failed),
                number(v.active_users),
                number(v.new_installations),
                number(v.active_installations),
                money(available ? r?.gross_minor || 0 : null, currency),
                money(available ? r?.net_publisher_minor || 0 : null, currency),
              ];
            }),
            "Per-app usage and earnings",
          ),
        );
      if (
        available &&
        f.apps.some((r) => !data.app_choices.some((a) => a.id === r.listing_id))
      )
        body.append(
          el("p", {
            class: "pa-warning",
            text: "Totals include earnings for unavailable or unlinked apps. Those amounts remain in the publisher totals.",
          }),
        );
      const days = [];
      for (
        let ms = Date.parse(from + "T00:00:00Z");
        ms < Date.parse(addDay(to));
        ms += 86400000
      )
        days.push(day(new Date(ms)));
      const daily = days.map((d) => ({
        day: d,
        value: data.daily.find((r) => r.day === d)?.jobs || 0,
      }));
      const charts = el(
        "div",
        { class: "pa-trends" },
        trend(daily, "Jobs per day", number),
      );
      if (available)
        charts.append(
          trend(
            days.map((d) => ({
              day: d,
              value: f.daily
                .filter((r) => r.day === d && r.currency === currency)
                .reduce((n, r) => n + r.net_publisher_minor, 0),
            })),
            `Net earnings per day (${currency})`,
            (v) => money(v, currency),
          ),
        );
      body.append(charts);
      if (available)
        body.append(
          el("h3", { text: "Publisher balances and payouts" }),
          el("p", {
            class: "muted pa-note",
            text: "Balances are current and cover all apps, even when one app is selected. Paid out counts bank-reconciled transfers in this date range; it is not the same as earnings.",
          }),
          table(
            [
              "Currency",
              "Payable now",
              "Reserve held",
              "Recoverable",
              "Paid out in period",
            ],
            f.publisher_balances.map((r) => [
              r.currency,
              ...[
                "payable_minor",
                "reserve_minor",
                "recoverable_minor",
                "paid_out_minor",
              ].map((k) => money(r[k], r.currency)),
            ]),
            "Publisher balances and payouts",
          ),
        );
    }
    pick(false);
    return { reload: load };
  };
})();
