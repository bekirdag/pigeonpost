// management.pigeonpost.dev — Overview, People (accounts, handles, mailboxes, plans & purchases)
// and Enterprises. Routes and shapes are the back office's (site/staff.js); every detail read is
// recorded in the audit log by the postbox.
(function () {
  "use strict";
  const PP = window.PP, M = window.PPM, ui = PP.ui;
  const el = (...a) => PP.el(...a);
  const enc = encodeURIComponent;

  // ---- Overview ------------------------------------------------------------------------------

  M.page({
    key: "overview", title: "Overview", group: "", icon: "home", allow: M.allow(M.ROLES),
    render: async (view, ctx) => {
      const me = ctx.me || {};
      view.append(ui.header({
        eyebrow: "Pigeonpost management",
        title: "Overview",
        text: "The platform at a glance. Counts only — open a queue to see the people behind them.",
        actions: el("div", { class: "pm-roles" }, (ctx.roles || []).map((r) => ui.badge(r, "info")), me.platform_owner ? ui.badge("platform owner", "success") : null),
      }));
      const alerts = el("div", { class: "pm-alerts" });
      const tiles = el("div", { class: "pm-queues" }, ui.loading(2));
      view.append(alerts, tiles);
      let d;
      try { d = (await M.get("/v1/staff/dashboard")).data || {}; } catch (e) {
        if (e instanceof PP.Redirecting) throw e;
        tiles.replaceChildren(M.errorBox(e, () => PP.shell.refresh()));
        return;
      }
      const c = d.counts || {};
      const can = (key) => PP.shell.state.opts.pages.some((p) => p.key === key && (!p.allow || p.allow(ctx)));
      const href = (key, ...p) => (can(key) ? PP.href(key, ...p) : null);
      const queues = [
        { label: "Open applications", value: M.num(c.publisher_applications), hint: "Partners waiting for a decision", href: href("applications"), tone: c.publisher_applications ? "warning" : null },
        { label: "Open reviews", value: M.num(c.open_reviews), hint: "Releases waiting for staff", href: href("reviews"), tone: c.open_reviews ? "warning" : null },
        { label: "Open reports", value: M.num(c.open_reports), hint: "Buyer reports on listings", href: href("listings"), tone: c.open_reports ? "danger" : null },
        { label: "Failed hosted jobs", value: M.num(c.hosted_jobs_failed_24h), hint: "Last 24 hours", href: href("jobs"), tone: c.hosted_jobs_failed_24h ? "danger" : null },
      ];
      const people = [
        { label: "Sign-ups today", value: M.num(c.signups_today), hint: `${M.num(c.signups_7d)} in 7 days`, href: href("accounts") },
        { label: "Accounts", value: M.num(c.accounts), hint: `${M.num(c.active_accounts_30d)} active in 30 days`, href: href("accounts") },
        { label: "Handles", value: M.num(c.handles), href: href("handles") },
        { label: "Mailboxes", value: M.num(c.mailboxes), href: href("mailboxes") },
        { label: "Organizations", value: M.num(c.orgs_active), hint: "active", href: href("enterprises") },
        { label: "Nests running", value: M.num(c.nests_running) },
      ];
      tiles.replaceChildren(
        ui.section({ title: "Queues", text: "What is waiting for staff right now." }, ui.stats(queues)),
        ui.section({ title: "People and usage" }, ui.stats(people)));
      if (M.has(ctx, "finance")) {
        const money = { label: "Credit sales today", value: "…", hint: "" };
        const refunds = { label: "Refunds to resolve", value: "…", hint: "uncertain", href: href("credits"), tone: null };
        const host = el("div", {}, ui.stats([money, refunds]));
        tiles.insertBefore(ui.section({ title: "Money today", text: "From the finance service." }, host), tiles.children[1] || null);
        const today = new Date(); today.setUTCHours(0, 0, 0, 0);
        const [sales, refs] = await Promise.allSettled([
          M.get(`/v1/staff/credits/sales?${M.qs({ from: today.toISOString(), limit: 100 })}`),
          M.get("/v1/staff/credits/refunds?status=uncertain&limit=100"),
        ]);
        if (sales.status === "fulfilled") {
          const rows = sales.value.data || [];
          money.value = M.usd(rows.reduce((a, r) => a + Number(r.amount_cents || 0), 0));
          money.hint = `${rows.length}${sales.value.next_cursor ? "+" : ""} sales · ${M.usd(rows.reduce((a, r) => a + Number(r.commission_cents || 0), 0))} commission`;
        } else { money.value = "—"; money.hint = M.explain(sales.reason) || ""; }
        if (refs.status === "fulfilled") { refunds.value = M.num((refs.value.data || []).length); refunds.tone = (refs.value.data || []).length ? "warning" : null; }
        else { refunds.value = "—"; refunds.hint = M.explain(refs.reason) || ""; }
        host.replaceChildren(ui.stats([money, refunds]));
      }
      const ALERT = {
        charges_refused: (a) => (Number(a.count) === 1 ? "1 hosted-job charge was refused by billing and needs a look." : `${M.num(a.count)} hosted-job charges were refused by billing and need a look.`),
        finance_unconfigured: () => "The finance service is not configured on this postbox, so credit screens are off.",
      };
      for (const a of d.alerts || []) alerts.append(ui.gate({ text: (ALERT[a.code] || (() => String(a.code)))(a), tone: "warning" }));
      view.append(el("p", { class: "pp-faint pm-asof" }, `As of ${M.time(d.as_of)}.`));
    },
  });

  // ---- shared tables -------------------------------------------------------------------------

  M.auditTable = function (rows, empty) {
    return ui.table({
      dense: true, rows: rows || [], pageSize: 50,
      empty: { icon: "audit", title: empty || "No records" },
      columns: [
        { key: "at", label: "When", render: (r) => M.time(r.at), sort: true },
        { key: "action", label: "Action", mono: true, sort: true },
        { key: "actor_account", label: "Actor", render: (r) => (r.actor_account ? M.mono(r.actor_account) : ui.badge(r.actor_kind || "system")) },
        { key: "target", label: "Target", mono: true },
        { key: "details", label: "Details", render: (r) => (r.details && Object.keys(r.details).length ? el("code", { class: "pm-details" }, JSON.stringify(M.maskDeep(r.details))) : "—") },
      ],
    });
  };
  const small = (columns, rows, empty) => ui.table({ columns, rows: rows || [], dense: true, pageSize: 50, empty: { title: empty } });

  // ---- Accounts ------------------------------------------------------------------------------

  M.page({
    key: "accounts", title: "Accounts", group: "People", icon: "people", allow: M.allow(["owner", "support"]),
    render: (view, ctx, [id]) => M.guard(view, () => (id ? accountDetail(view, ctx, id) : accountList(view))),
  });

  function accountList(view) {
    view.append(ui.header({ title: "Accounts", text: "Search by account id, sign-in address, handle or mailbox address. Opening an account is recorded in the audit log." }));
    M.list(view, {
      search: "acct_…, name@domain, /handle or /k/ address", caption: "Accounts",
      fetch: (p) => M.get(`/v1/staff/accounts?${p}`),
      rowHref: (r) => PP.href("accounts", r.id),
      empty: { icon: "people", title: "No accounts yet" },
      columns: [
        { key: "id", label: "Account", render: (r) => M.link(r.id, "accounts", r.id), sort: true },
        { key: "sign_in_address", label: "Sign-in address", sort: true },
        { key: "handles", label: "Handles", num: true, sort: true },
        { key: "mailboxes", label: "Mailboxes", num: true, sort: true },
        { key: "staff", label: "Staff", render: (r) => (r.staff ? ui.badge("staff", "info") : "—") },
        { key: "created_at", label: "Created", render: (r) => M.day(r.created_at), sort: (r) => String(r.created_at || "") },
      ],
    });
  }

  async function accountDetail(view, ctx, id) {
    PP.shell.crumb(id);
    view.append(ui.header({ eyebrow: "Account", title: id, text: "Everything this account holds and buys. This view is recorded in the audit log.", actions: ui.button("All accounts", { href: PP.href("accounts") }) }));
    await M.detail(view, `/v1/staff/accounts/${enc(id)}`, (box, a) => {
      const sign = (a.sign_in_identities || []).find((s) => s.provider === "realm");
      const plans = [].concat(
        (a.entitlements || []).map((e) => ({ what: e.plan_code, source: e.source, status: e.status, until: e.period_end || e.paid_through, ref: e.provider_ref })),
        ((a.subscriptions || {}).apple || []).map((s) => ({ what: s.product_id, source: "App Store", status: new Date(s.expires_at) > new Date() ? "active" : "expired", until: s.expires_at, ref: s.reference })),
        ((a.subscriptions || {}).google || []).map((s) => ({ what: s.product_id, source: "Google Play", status: s.state, until: s.expires_at, ref: s.reference })),
        ((a.subscriptions || {}).nest_store || []).map((s) => ({ what: s.package, source: `Nest (${s.store})`, status: s.state, until: s.expires_at, ref: s.reference })));
      const roles = (a.staff_roles || []).filter((r) => !r.revoked_at).map((r) => r.role);
      box.append(ui.stats([
        { label: "Handles", value: (a.handles || []).length },
        { label: "Mailboxes", value: (a.mailboxes || []).length },
        { label: "Devices", value: (a.devices || []).length },
        { label: "Plans & purchases", value: plans.length },
        { label: "Nests", value: (a.nests || []).length },
        { label: "Organizations", value: (a.orgs || []).length },
      ]));
      const tabs = [
        ["summary", "Summary"], ["identity", "Identities & handles", (a.handles || []).length + (a.sign_in_identities || []).length],
        ["mailboxes", "Mailboxes & devices", (a.mailboxes || []).length], ["buys", "Purchases & entitlements", plans.length],
        ["nests", "Nests & orgs", (a.nests || []).length + (a.orgs || []).length],
        M.has(ctx, "finance") ? ["credit", "Credit"] : null, ["audit", "Recent audit", (a.recent_audit || []).length],
      ].filter(Boolean).map(([key, label, count]) => ({ key, label, count }));
      const panel = el("div", { class: "pm-tab-panel" });
      const show = (k) => {
        bar.replaceWith(bar = ui.tabs(tabs, k, show));
        panel.replaceChildren();
        if (k === "summary") panel.append(ui.card({ title: "Account" }, M.kv([
          ["Account id", ui.copyable(a.id)], ["Sign-in address", sign ? sign.login : "—"], ["Created", M.time(a.created_at)],
          ["OIDC subject", a.oidc_subject, "oidc_subject"], ["Staff roles", roles.join(", ") || "—"],
          ["Deletion", a.deletion ? `requested ${M.day(a.deletion.requested_at)}, ${a.deletion.completed_at ? "completed " + M.day(a.deletion.completed_at) : "due " + M.day(a.deletion.complete_by)}` : "—"],
        ])));
        if (k === "identity") panel.append(el("div", { class: "pp-grid pm-grid-2" },
          ui.card({ title: "Sign-in identities" }, small([{ key: "provider", label: "Provider" }, { key: "login", label: "Login" }, { key: "verified_at", label: "Verified", render: (r) => M.day(r.verified_at) }], a.sign_in_identities, "No proved identities")),
          ui.card({ title: "Handles" }, small([{ key: "namespace", label: "Namespace", render: (r) => M.link("/" + r.namespace, "handles", r.namespace) }, { key: "source", label: "Source" }, { key: "expires_at", label: "Expires", render: (r) => M.day(r.expires_at) }], a.handles, "No handles"))));
        if (k === "mailboxes") panel.append(
          ui.card({ title: "Mailboxes", text: "Identities only — message bodies are never shown here." }, small([
            { key: "address", label: "Address", mono: true }, { key: "handle", label: "Handle", mono: true }, { key: "label", label: "Label" },
            { key: "kind", label: "Kind", render: (r) => ui.badge(String(r.kind || "").replace("_", " ")) }, { key: "created_at", label: "Created", render: (r) => M.day(r.created_at) },
          ], a.mailboxes, "No mailboxes")),
          ui.card({ title: "Devices" }, small([
            { key: "platform", label: "Platform" }, { key: "os", label: "OS" }, { key: "kind", label: "Kind" },
            { key: "mailbox", label: "Mailbox", mono: true }, { key: "push_token", label: "Push token", render: (r) => M.mono(M.shown("push_token", r.push_token)) },
            { key: "updated_at", label: "Last seen", render: (r) => M.time(r.updated_at) },
          ], a.devices, "No devices")));
        if (k === "buys") panel.append(ui.card({ title: "Plans, store subscriptions and Nest purchases", text: "From every source: web, App Store, Google Play and Nest stores." }, small([
          { key: "what", label: "Plan / product" }, { key: "source", label: "Source" }, { key: "status", label: "Status", render: (r) => M.status(r.status) },
          { key: "until", label: "Until", render: (r) => M.day(r.until) }, { key: "ref", label: "Reference", mono: true },
        ], plans, "No plans or purchases")));
        if (k === "nests") panel.append(el("div", { class: "pp-grid pm-grid-2" },
          ui.card({ title: "Nests" }, small([{ key: "label", label: "Label" }, { key: "state", label: "State", render: (r) => M.status(r.state) }, { key: "funding", label: "Funding" }], a.nests, "No Nests")),
          ui.card({ title: "Organizations" }, small([{ key: "name", label: "Name", render: (r) => M.link(r.name, "enterprises", r.id) }, { key: "relation", label: "Relation" }, { key: "status", label: "Status", render: (r) => M.status(r.status) }], a.orgs, "No organizations"))));
        if (k === "credit") { const c = el("div"); panel.append(ui.card({ title: "Credit", actions: ui.button("Open in Credits", { size: "sm", href: PP.href("credits", "account", a.id) }) }, c)); M.creditOwner(c, "account", a.id, { compact: true }); }
        if (k === "audit") panel.append(ui.card({ title: "Recent audit" }, M.auditTable(a.recent_audit, "No recent audit records")));
      };
      let bar = el("div");
      box.append(bar, panel);
      show("summary");
    }, { notFound: "No account with that id" });
  }

  // ---- Handles -------------------------------------------------------------------------------

  M.page({
    key: "handles", title: "Handles", group: "People", icon: "at", allow: M.allow(["owner", "support"]),
    render: (view, ctx, [ns]) => M.guard(view, () => (ns ? handleDetail(view, ns) : handleList(view))),
  });

  function handleList(view) {
    view.append(ui.header({ title: "Handles", text: "Every namespace and who holds it." }));
    M.list(view, {
      search: "Namespace or account id", caption: "Handles",
      chips: { name: "source", label: "Source", value: "", options: [["", "Any source"], ["apple", "App Store"], ["google", "Google Play"], ["entitlement", "Entitlement"], ["grant", "Grant"], ["test_preview", "Test preview"]] },
      fetch: (p) => M.get(`/v1/staff/handles?${p}`), rowHref: (r) => PP.href("handles", r.namespace),
      empty: { icon: "at", title: "No handles yet" },
      columns: [
        { key: "namespace", label: "Handle", render: (r) => M.link("/" + r.namespace, "handles", r.namespace), sort: true },
        { key: "account_id", label: "Holder", render: (r) => (r.account_id ? M.link(r.account_id, "accounts", r.account_id) : "—") },
        { key: "source", label: "Source", sort: true },
        { key: "mailboxes", label: "Mailboxes", num: true, sort: true },
        { key: "expires_at", label: "Expires", render: (r) => M.day(r.expires_at), sort: (r) => String(r.expires_at || "") },
        { key: "release_at", label: "Release", render: (r) => M.day(r.release_at) },
      ],
    });
  }

  async function handleDetail(view, ns) {
    PP.shell.crumb("/" + ns);
    view.append(ui.header({ eyebrow: "Handle", title: "/" + ns, text: "Who holds this handle, what lives under it, and its history.", actions: ui.button("All handles", { href: PP.href("handles") }) }));
    await M.detail(view, `/v1/staff/handles/${enc(ns)}`, (box, h) => {
      const subs = [].concat(((h.subscriptions || {}).apple || []), ((h.subscriptions || {}).google || []));
      box.append(el("div", { class: "pp-grid pm-grid-2" },
        ui.card({ title: "Holder" }, h.holder ? M.kv([
          ["Account", M.link(h.holder.account_id, "accounts", h.holder.account_id)], ["Source", h.holder.source],
          ["Verified", M.time(h.holder.verified_at)], ["Expires", M.day(h.holder.expires_at)], ["Release date", M.day(h.holder.release_at)],
        ]) : ui.empty({ title: "Nobody holds this namespace now" })),
        ui.card({ title: "Mailboxes under it" }, small([{ key: "handle", label: "Handle", mono: true }, { key: "address", label: "Address", mono: true }, { key: "account_id", label: "Account", render: (r) => (r.account_id ? M.link(r.account_id, "accounts", r.account_id) : "—") }, { key: "created_at", label: "Created", render: (r) => M.day(r.created_at) }], h.mailboxes, "No mailboxes"))));
      box.append(ui.card({ title: "Store subscriptions" }, small([{ key: "store", label: "Store" }, { key: "product_id", label: "Product" }, { key: "account_id", label: "Account" }, { key: "expires_at", label: "Expires", render: (r) => M.day(r.expires_at) }, { key: "reference", label: "Reference", mono: true }], subs, "No store subscriptions")));
      if ((h.orgs || []).length || (h.holds || []).length) box.append(ui.card({ title: "Organization" },
        small([{ key: "name", label: "Org", render: (r) => M.link(r.name, "enterprises", r.id) }, { key: "status", label: "Status", render: (r) => M.status(r.status) }], h.orgs, "No organization"),
        small([{ key: "handle", label: "Held handle", mono: true }, { key: "reason", label: "Reason" }, { key: "created_at", label: "Since", render: (r) => M.day(r.created_at) }], h.holds, "No holds")));
      box.append(ui.card({ title: "History" }, M.auditTable(h.history, "No recorded grants or releases")));
    }, { notFound: "No such handle" });
  }

  // ---- Mailboxes -----------------------------------------------------------------------------

  M.page({
    key: "mailboxes", title: "Mailboxes", group: "People", icon: "inbox", allow: M.allow(["owner", "support"]),
    render: (view) => M.guard(view, () => {
      view.append(ui.header({ title: "Mailboxes", text: "Identities with their account, handle and last device check-in. Never their messages." }));
      M.list(view, {
        search: "Address, handle, label or account", caption: "Mailboxes",
        chips: { name: "kind", label: "Kind", value: "", options: [["", "Every kind"], ["account", "Personal"], ["org_agent", "Org agents"], ["nest", "Nests"], ["anonymous", "Anonymous"]] },
        fetch: (p) => M.get(`/v1/staff/mailboxes?${p}`),
        empty: { icon: "inbox", title: "No mailboxes yet" },
        columns: [
          { key: "address", label: "Address", mono: true, sort: true },
          { key: "handle", label: "Handle", mono: true, sort: true },
          { key: "label", label: "Label" },
          { key: "kind", label: "Kind", render: (r) => ui.badge(String(r.kind || "").replace("_", " ")) },
          { key: "account_id", label: "Account", render: (r) => (r.account_id ? M.link(r.account_id, "accounts", r.account_id) : "—") },
          { key: "last_activity", label: "Last activity", render: (r) => M.time(r.last_activity), sort: (r) => String(r.last_activity || "") },
        ],
      });
    }),
  });

  // ---- Plans & purchases ---------------------------------------------------------------------

  M.page({
    key: "plans", title: "Plans & purchases", group: "People", icon: "plan", allow: M.allow(["support", "finance"]),
    render: (view, ctx, [tab0]) => M.guard(view, () => {
      view.append(ui.header({ title: "Plans & purchases", text: "Account and organization entitlements from every source, and the store records behind them." }));
      const TABS = [{ key: "entitlements", label: "Entitlements" }, { key: "apple", label: "App Store" }, { key: "google", label: "Google Play" }, { key: "nest", label: "Nest purchases" }];
      const panel = el("div", { class: "pm-tab-panel" });
      let bar = el("div");
      view.append(bar, panel);
      const pick = (which) => {
        bar.replaceWith(bar = ui.tabs(TABS, which, pick));
        panel.replaceChildren();
        if (which === "entitlements") M.list(panel, {
          search: "Account, org or plan code", caption: "Entitlements",
          filters: [{ name: "source", label: "Source", options: [["", "Any source"], ["apple", "Apple"], ["google", "Google"], ["iyzico", "Web (iyzico)"], ["grant", "Grant"], ["test", "Test"]] }],
          chips: { name: "status", label: "Status", value: "", options: [["", "Any status"], ["active", "Active"], ["grace", "Grace"], ["past_due", "Past due"], ["expired", "Expired"], ["canceled", "Canceled"]] },
          fetch: (p) => M.get(`/v1/staff/entitlements?${p}`), empty: { icon: "plan", title: "No entitlements" },
          columns: [
            { key: "subject_id", label: "Subject", render: (r) => (r.subject_kind === "org" ? M.link(r.subject_id, "enterprises", r.subject_id) : M.link(r.subject_id, "accounts", r.subject_id)) },
            { key: "plan_code", label: "Plan", mono: true, sort: true }, { key: "source", label: "Source", sort: true },
            { key: "status", label: "Status", render: (r) => M.status(r.status), sort: true },
            { key: "period_end", label: "Period end", render: (r) => M.day(r.period_end), sort: (r) => String(r.period_end || "") },
            { key: "provider_ref", label: "Reference", mono: true },
          ],
        });
        else M.list(panel, {
          search: "Account, namespace or product", caption: "Subscriptions", defaults: { store: which },
          fetch: (p) => M.get(`/v1/staff/subscriptions?${p}`), empty: { icon: "store", title: "No store records" },
          columns: [
            { key: "account_id", label: "Account", render: (r) => M.link(r.account_id, "accounts", r.account_id) },
            { key: "product_id", label: "Product", mono: true, sort: true },
            { key: which === "nest" ? "package" : "namespace", label: which === "nest" ? "Package" : "Handle" },
            { key: "state", label: "State", render: (r) => M.status(r.state || (new Date(r.expires_at) > new Date() ? "active" : "expired")) },
            { key: "expires_at", label: "Expires", render: (r) => M.day(r.expires_at), sort: (r) => String(r.expires_at || "") },
            { key: "reference", label: "Reference", mono: true },
          ],
        });
      };
      pick(TABS.some((t) => t.key === tab0) ? tab0 : "entitlements");
    }),
  });

  // ---- Enterprises ---------------------------------------------------------------------------

  M.page({
    key: "enterprises", title: "Organizations", group: "Enterprises", icon: "building", allow: M.allow(["owner", "support"]),
    render: (view, ctx, [id]) => M.guard(view, () => (id ? orgDetail(view, id) : orgList(view))),
  });

  function orgList(view) {
    view.append(ui.header({ title: "Organizations", text: "Every enterprise: owner, seats, members, agents and plan." }));
    M.list(view, {
      search: "Org id, namespace, name or owner", caption: "Organizations",
      chips: { name: "status", label: "Status", value: "", options: [["", "Any status"], ["active", "Active"], ["past_due", "Past due"], ["read_only", "Read only"], ["suspended", "Suspended"], ["pending", "Pending"], ["deleted", "Deleted"]] },
      fetch: (p) => M.get(`/v1/staff/orgs?${p}`), rowHref: (r) => PP.href("enterprises", r.id),
      empty: { icon: "building", title: "No organizations yet" },
      columns: [
        { key: "name", label: "Name", render: (r) => M.link(r.name, "enterprises", r.id), sort: true },
        { key: "namespace", label: "Namespace", render: (r) => "/" + r.namespace, sort: true },
        { key: "status", label: "Billing state", render: (r) => M.status(r.status), sort: true },
        { key: "seats", label: "Seats", num: true, sort: true },
        { key: "active_members", label: "Members", num: true, sort: true },
        { key: "agents", label: "Agents", num: true, sort: true },
        { key: "plan_code", label: "Package", mono: true, sort: true },
      ],
    });
  }

  async function orgDetail(view, id) {
    const head = ui.header({ eyebrow: "Organization", title: id, text: "Members, seats, package, billing state, domains, agents and API keys. Keys show their last four characters only.", actions: ui.button("All organizations", { href: PP.href("enterprises") }) });
    view.append(head);
    await M.detail(view, `/v1/staff/orgs/${enc(id)}`, (box, o) => {
      head.querySelector("h1").textContent = o.name || id;
      PP.shell.crumb(o.name || id);
      const members = o.members || [];
      box.append(ui.stats([
        { label: "Billing state", value: String(o.status || "—").replace(/_/g, " "), tone: o.status === "active" ? "success" : o.status ? "warning" : null },
        { label: "Seats", value: o.seats, hint: `${members.filter((m) => m.status === "active").length} active members` },
        { label: "Agents", value: (o.agents || []).length, hint: `up to ${o.agent_limit ?? "—"}` },
        { label: "Package", value: o.plan_code || "—", hint: o.period_end ? `period ends ${M.day(o.period_end)}` : null },
      ]));
      box.append(ui.card({ title: "Organization" }, M.kv([
        ["Org id", ui.copyable(o.id)], ["Namespace", "/" + o.namespace], ["Status", M.status(o.status)],
        ["Owner", o.owner_account ? M.link(o.owner_account, "accounts", o.owner_account) : "—"], ["Seats", `${o.seats} (agents up to ${o.agent_limit})`],
        ["Package", o.plan_code], ["Period end", M.day(o.period_end)], ["Created", M.time(o.created_at)],
      ])));
      box.append(ui.card({ title: "Members" }, small([
        { key: "name", label: "Name", sort: true }, { key: "handle", label: "Handle", mono: true }, { key: "contact", label: "Contact" },
        { key: "roles", label: "Roles", render: (r) => (r.roles || []).join(", ") || "member" }, { key: "status", label: "Status", render: (r) => M.status(r.status) },
        { key: "account_id", label: "Account", render: (r) => (r.account_id ? M.link(r.account_id, "accounts", r.account_id) : "—") },
      ], members, "No members")));
      box.append(el("div", { class: "pp-grid pm-grid-2" },
        ui.card({ title: "Domains" }, small([{ key: "domain", label: "Domain" }, { key: "verified_at", label: "Verified", render: (r) => (r.verified_at ? M.day(r.verified_at) : ui.badge("unverified", "warning")) }], o.domains, "No domains")),
        ui.card({ title: "Entitlements" }, small([{ key: "plan_code", label: "Plan", mono: true }, { key: "source", label: "Source" }, { key: "status", label: "Status", render: (r) => M.status(r.status) }], o.entitlements, "No entitlements"))));
      box.append(ui.card({ title: "Agents" }, small([{ key: "handle", label: "Handle", mono: true }, { key: "label", label: "Label" }, { key: "status", label: "Status", render: (r) => M.status(r.status) }, { key: "last_seen_at", label: "Last seen", render: (r) => M.time(r.last_seen_at) }], o.agents, "No agents")));
      box.append(ui.card({ title: "API keys" }, small([
        { key: "name", label: "Name" }, { key: "masked", label: "Key", render: (r) => M.mono(M.shown("masked", r.masked)) },
        { key: "scopes", label: "Scopes", render: (r) => (Array.isArray(r.scopes) ? r.scopes.join(", ") : String(r.scopes || "")) },
        { key: "last_used_at", label: "Last used", render: (r) => M.time(r.last_used_at) },
        { key: "revoked_at", label: "State", render: (r) => (r.revoked_at ? ui.badge("revoked", "danger") : ui.badge("live", "success")) },
      ], o.api_keys, "No API keys")));
      if (M.has(PP.shell.ctx(), "finance")) { const c = el("div"); box.append(ui.card({ title: "Credit", actions: ui.button("Open in Credits", { size: "sm", href: PP.href("credits", "org", o.id) }) }, c)); M.creditOwner(c, "org", o.id, { compact: true }); }
    }, { notFound: "No organization with that id" });
  }
})();
