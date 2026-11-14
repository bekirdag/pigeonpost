// Pigeonpost for teams — the admin app (/app, SDS §5.2–5.3) on the shared platform shell
// (site-shared/: PP.shell, PP.ui). Pages, role-filtered as before: Overview, Setup, People, Agents,
// Rooms, Nests, Services, Rules, Security, Packages, Billing, Organization, API keys and Webhooks
// (behind `org_api`) and Support. One primary action per page (top right), at most one table per
// page, one sheet at a time. Every decision is the postbox's; this page only shows what it may do.
//
// Routes are the shell's hash routes (/app#/people). Old /app/<page> links still land on the same
// page: the path is turned into the hash before the shell starts.
(function () {
  "use strict";
  const PPE = window.PP;
  const ui = PPE.ui;
  const R = PPE.rules;
  const el = PPE.el;
  const cfg = window.PP_CONFIG || window.PPE_CONFIG || {};
  const T = (text, vars) => PPE.t(text, vars);

  const ADMIN = ["owner", "admin", "people_admin", "agent_admin", "billing_admin"];
  const NEST_ADMIN = ["owner", "admin", "agent_admin"];
  const ROLE_NAMES = { owner: "Owner", admin: "Admin", people_admin: "People admin", agent_admin: "Agent admin", billing_admin: "Billing admin" };
  // Which roles (and server feature) each page needs. The postbox enforces every route anyway.
  const ACCESS = {
    overview: { roles: ADMIN },
    people: { roles: ["owner", "admin", "people_admin"] },
    agents: { roles: ["owner", "admin", "agent_admin"] },
    rooms: { roles: ADMIN },
    nests: { roles: [...NEST_ADMIN, "billing_admin"], feature: "nests" },
    services: { roles: [...NEST_ADMIN, "billing_admin"], feature: "marketplace" },
    rules: { roles: ["owner", "admin", "people_admin", "agent_admin"] },
    security: { roles: ["owner", "admin"] },
    packages: { roles: ["owner", "admin", "billing_admin"] },
    billing: { roles: ["owner", "admin", "billing_admin"] },
    organization: { roles: ["owner", "admin"] },
    "api-keys": { roles: ["owner", "admin"], feature: "org_api" },
    webhooks: { roles: ["owner", "admin"], feature: "org_api" },
  };
  const CAPABILITIES = [
    ["enroll", "Log it in on a machine"],
    ["read_inbox", "Read its inbox in the apps"],
    ["ssh", "Open a terminal on its workspace (SSH)"],
  ];
  const STATUS = {
    active: ["Active", "success"], invited: ["Invited", "warning"], suspended: ["Suspended", "danger"],
    removed: ["Removed", "danger"], deleted: ["Deleted", "danger"], provisioning: ["Setting up", "warning"],
    past_due: ["Payment due", "warning"], read_only: ["Read-only", "danger"], pending: ["Pending", "warning"],
  };
  const SUPPORT_EMAIL = "support@pigeonpost.dev";
  const HELP = "https://help.pigeonpost.dev";
  const ACCOUNT = "https://pigeonpost.dev/account";

  const app = { features: [], orgs: [], org: null, me: null, members: null, state: null, disabled: new Set() };
  PPE.app = app;
  const sshOn = () => app.features.includes("agent_ssh");
  const streamsOn = () => app.features.includes("streaming");

  // ---- helpers ---------------------------------------------------------------------------------

  const has = (...roles) => (app.me && Array.isArray(app.me.roles) ? app.me.roles.some((r) => roles.includes(r)) : false);
  app.can = (page) => {
    if (app.state !== "admin" || !app.org) return false;
    const a = ACCESS[page];
    if (!a) return true;
    return has(...a.roles) && (!a.feature || app.features.includes(a.feature));
  };
  /// Roles this admin may hand out (SDS §6.4, D-13): only the owner appoints full admins.
  app.grantable = () => (has("owner") ? ["admin", "people_admin", "agent_admin", "billing_admin"]
    : has("admin") ? ["people_admin", "agent_admin", "billing_admin"] : []);
  const orgPath = (rest = "") => `/v1/orgs/${encodeURIComponent(app.org.id)}${rest}`;
  const ns = () => (app.org ? app.org.namespace : "");
  const inviteEmailOn = () => app.features.includes("org_invite_email");
  const writable = () => !["read_only", "suspended"].includes(app.org && app.org.status);
  const fullName = (m) => `${(m && m.first_name) || ""} ${(m && m.last_name) || ""}`.trim();
  /// A read that may fail without failing the page; a redirect still stops it.
  const soft = (p) => Promise.resolve(p).catch((e) => { if (e instanceof PPE.Redirecting) throw e; return null; });

  /// Re-render a page: the current one in place, another one by navigating to it.
  async function go(page) {
    const want = PPE.href(page);
    if (window.location.hash === want) await PPE.shell.refresh();
    else window.location.hash = want;
  }
  app.go = go;

  const input = (attrs = {}) => el("input", { class: "pp-input", type: "text", ...attrs });
  const select = (attrs, ...options) => el("select", { class: "pp-input", ...attrs }, ...options);
  const textarea = (attrs = {}) => el("textarea", { class: "pp-input", rows: 3, ...attrs });
  const check = (box, ...label) => el("label", { class: "check" }, box, el("span", {}, ...label));
  const actions = (...nodes) => el("div", { class: "actions" }, ...nodes);
  const statusLine = () => el("p", { class: "status-line", role: "status" });
  const bad = (status, text) => { status.className = "status-line status-bad"; status.textContent = text; };
  const prefixed = (node) => el("div", { class: "prefixed" }, el("span", { translate: "no" }, `/${ns()}/`), node);
  const statusBadge = (s) => ui.status(s, STATUS);
  const meter = (used, max, label) => {
    const bar = el("span", {});
    bar.style.width = `${Math.min(100, Math.round(((used || 0) / Math.max(1, max || 1)) * 100))}%`;
    return el("div", { class: "meter", role: "img", "aria-label": label }, bar);
  };
  const ask = (text, { confirm = "Confirm", danger = true, title } = {}) => ui.confirm({ title: title || "Are you sure?", text, confirm, danger });

  function secretBox(text, onceNote) {
    return el("div", {},
      el("div", { class: "secret" }, el("code", {}, text), ui.button("Copy", { size: "sm", onclick: () => PPE.copy(text) })),
      onceNote ? el("p", { class: "hint" }, onceNote) : null);
  }
  /// A button that runs `fn` once at a time and explains a failure.
  const act = (label, fn, opts = {}) => ui.button(label, { ...opts, onclick: fn });

  // ---- banners shown above every page ------------------------------------------------------------

  function banner() {
    const s = app.org && app.org.status;
    const billing = app.can("billing") ? { label: "Open Billing", href: PPE.href("billing") } : null;
    if (s === "past_due") return ui.gate({ text: "The last payment didn't go through. Update the card in Billing to avoid interruption.", action: billing });
    if (s === "read_only") return ui.gate({ tone: "danger", text: "The organization is read-only: no new invitations or agents, and company mailboxes can't send. Update the payment in Billing.", action: billing });
    if (s === "suspended") {
      // D-14: a suspended organization is deleted on this date unless it is renewed first.
      const at = app.org.deletion_at ? new Date(app.org.deletion_at * 1000).toLocaleDateString(PPE.locale(), { year: "numeric", month: "long", day: "numeric" }) : null;
      return ui.gate({ tone: "danger", action: billing, text: at
        ? `The organization is suspended: company addresses don't receive mail. Renew in Billing to restore it. If it isn't renewed, it will be deleted on ${at}, with its rooms and company addresses.`
        : "The organization is suspended: company addresses don't receive mail. Renew in Billing to restore it." });
    }
    return null;
  }

  /// The owner picks their own company address once (docs/orgs.md §2).
  function firstRun() {
    if (!has("owner") || (app.me && app.me.mailbox)) return null;
    const name = input({ maxlength: 32, spellcheck: "false", autocapitalize: "none", value: (PPE.whoami().username || "").toLowerCase().replace(/[^a-z0-9._-]/g, "").slice(0, 32), "aria-label": "Your address" });
    const status = statusLine();
    const save = el("button", { class: "pp-btn pp-btn-primary", type: "submit" }, "Use this address");
    const form = el("form", { class: "pp-card pp-card-accent first-run" },
      el("h2", {}, `Choose your address in ${app.org.name}`),
      el("p", { class: "pp-muted" }, "This is how people reach you at work. The company's General room is set up with it."),
      PPE.field("Your address", prefixed(name)), status, actions(save));
    form.addEventListener("submit", async (e) => {
      e.preventDefault();
      const v = name.value.trim().toLowerCase();
      if (!/^[a-z0-9][a-z0-9._-]{0,31}$/.test(v)) return bad(status, "Use letters, numbers, dots, underscores or hyphens.");
      save.disabled = true;
      try {
        await PPE.postbox(orgPath("/me/mailbox"), { method: "POST", body: { handle: `/${ns()}/${v}` } });
        PPE.toast(`You're /${ns()}/${v}. Open your inbox to finish setting up General.`);
        await load(app.org.id);
        await PPE.shell.reload();
      } catch (err) { save.disabled = false; if (!(err instanceof PPE.Redirecting)) bad(status, PPE.explain(err)); }
    });
    return form;
  }

  /// Every admin page: the org banner and first-run card above it; a feature the server switched
  /// off since capabilities were read hides its pages.
  function page(key, fn) {
    return async (view, ctx, params) => {
      try {
        const node = await fn(params || []);
        view.append(...[banner(), key === "setup" ? null : firstRun(), node].filter(Boolean));
      } catch (e) {
        const a = ACCESS[key];
        if (e && e.code === "feature_disabled" && a && a.feature) {
          app.disabled.add(a.feature);
          app.features = app.features.filter((f) => f !== a.feature);
          await PPE.shell.reload();
          return;
        }
        throw e;
      }
    };
  }

  // ---- Overview --------------------------------------------------------------------------------

  /// What is done and what is next, for Overview and Setup.
  async function setupFacts() {
    const [members, rules, domains, rooms] = await Promise.all([
      app.can("people") ? soft(PPE.postbox(orgPath("/members")).then((r) => r.members || [])) : null,
      app.can("rules") ? soft(PPE.postbox(orgPath("/rules")).then((r) => r.rules || [])) : null,
      app.can("security") ? soft(PPE.postbox(orgPath("/domains")).then((r) => r.domains || [])) : null,
      app.can("rooms") ? soft(PPE.postbox(orgPath("/rooms")).then((r) => r.rooms || [])) : null,
    ]);
    if (members) app.members = members;
    return { members, rules, domains, rooms };
  }

  function setupSteps(f) {
    const s = (app.org && app.org.settings) || {};
    const steps = [
      { key: "organization", title: "Create the organization", text: `${app.org.name} is at /${app.org.namespace}.`, done: true },
      { key: "address", title: "Choose your company address", text: "How people reach you at work. It also sets up the General room.", done: Boolean(app.me && app.me.mailbox), show: has("owner") },
      { key: "domain", title: "Verify your domain", text: "Prove you own your company's domain, so invitations can be limited to it.", done: Boolean(f.domains && f.domains.some((d) => d.verified_at)), show: app.can("security"),
        action: { label: "Add domain", onclick: () => domainSheet() } },
      { key: "people", title: "Invite your team", text: "Each person gets a company address and joins the auto-join rooms.", done: Boolean(f.members && f.members.filter((m) => m.status !== "removed").length > 1), show: app.can("people"),
        action: { label: "Invite people", onclick: () => inviteSheet(), disabled: !writable() } },
      { key: "rules", title: "Set company rules", text: "Decide who can reach whom, for everyone at once. Templates make it one click.", done: Boolean(f.rules && f.rules.length), show: app.can("rules"),
        action: { label: "Open Rules", href: PPE.href("rules") } },
      { key: "mfa", title: "Require two-step sign-in", text: "Everyone signs in with a second step. Recommended for every company.", done: Boolean(s.require_mfa), show: app.can("security"),
        action: { label: "Turn it on", onclick: async () => { await patchSettings({ require_mfa: true }); PPE.toast("Saved"); await go(PPE.parseRoute(window.location.hash).page || "overview"); } } },
    ];
    return steps.filter((x) => x.show !== false);
  }

  async function overview() {
    const o = app.org;
    const [facts, bill, invoices, nests] = await Promise.all([
      setupFacts(),
      app.can("billing") ? soft(PPE.postbox(orgPath("/billing"))) : null,
      app.can("billing") ? soft(PPE.bridge("/v1/billing/invoices").then((r) => r.invoices || [])) : null,
      app.can("nests") ? soft(PPE.postbox(orgPath("/nests"))) : null,
    ]);
    const who = (app.ctx && app.ctx.user) || {};
    const first = String(who.name || "").trim().split(/\s+/)[0] || "";
    const open = (invoices || []).filter((i) => /^(open|unpaid|pending|overdue|past_due)$/i.test(String(i.status || "")));
    const m = /^ent-(\d+)-(monthly|yearly)-/.exec((bill && bill.plan_code) || o.plan_code || "") || [];
    const steps = setupSteps(facts);
    const left = steps.filter((s) => !s.done).length;
    const quick = [
      app.can("people") ? ui.button("Invite people", { icon: "people", onclick: () => inviteSheet(), disabled: !writable() }) : null,
      app.can("agents") ? ui.button("Add agent", { icon: "agents", onclick: () => addAgentSheet(), disabled: !writable() }) : null,
      app.can("rooms") ? ui.button("Create room", { icon: "rooms", onclick: () => roomSheet() }) : null,
      ui.button("Open your inbox", { icon: "inbox", href: cfg.inbox || "https://inbox.pigeonpost.dev" }),
    ].filter(Boolean);
    return el("div", { class: "pp-view-inner" },
      ui.header({ eyebrow: o.name, title: first ? T("Welcome, {1}", { 1: first }) : "Overview",
        text: left ? T("{1} setup steps left for {2}.", { 1: left, 2: o.name }) : `Everything is set up for ${o.name}.` }),
      ui.stats([
        { label: "Seats used", value: `${o.seats_used ?? (facts.members ? facts.members.filter((x) => x.status !== "removed").length : "—")} / ${o.seats}`, hint: "People in the organization", href: app.can("people") ? PPE.href("people") : null },
        { label: "Agents", value: `${o.agents_used ?? 0} / ${o.agent_limit}`, hint: "Agent addresses in use", href: app.can("agents") ? PPE.href("agents") : null },
        facts.rooms ? { label: "Rooms", value: facts.rooms.length, hint: "Company rooms", href: PPE.href("rooms") } : null,
        { label: "Package", value: T("{1} seats", { 1: o.seats }), hint: m[2] === "monthly" ? "Billed monthly" : m[2] === "yearly" ? "Billed yearly" : "Organization plan", href: app.can("packages") ? PPE.href("packages") : null },
        invoices ? { label: "Open invoices", value: open.length, hint: open.length ? "Waiting for payment" : "Nothing to pay", tone: open.length ? "warning" : null, href: PPE.href("billing") } : null,
        nests ? { label: "Nests", value: `${(nests.allowance || {}).used || 0} / ${(nests.allowance || {}).slots || 0}`, hint: nests.package ? "Company Nest package" : "No Nest package yet", href: PPE.href("nests") } : null,
      ]),
      el("div", { class: "pp-cols" },
        ui.card({ title: "Get set up", text: left ? "Finish these to get the most out of Pigeonpost for teams." : "All done. You can change any of these later.", actions: ui.button("Open Setup", { size: "sm", href: PPE.href("setup") }) },
          ui.checklist(steps.map((s) => ({ title: s.title, text: s.text, done: s.done, action: s.action })))),
        ui.card({ title: "Quick actions" }, el("div", { class: "quick" }, quick),
          ui.kv([["Organization", el("span", { translate: "no" }, o.name)], ["Address", el("span", { class: "pp-mono", translate: "no" }, `/${o.namespace}`)], ["Status", statusBadge(o.status)], ["Your role", (app.me.roles || []).map((r) => ROLE_NAMES[r] || r).join(", ") || "Member"]]))));
  }

  // ---- Setup -----------------------------------------------------------------------------------

  async function setup(params) {
    if (app.state !== "admin") return gateCard();
    const steps = setupSteps(await setupFacts());
    const current = steps.find((s) => s.key === params[0]) || steps.find((s) => !s.done) || steps[steps.length - 1];
    let body;
    if (current.key === "address" && !current.done) body = firstRun();
    else {
      body = ui.card({ title: current.title, text: current.text },
        current.done ? el("p", { class: "pp-muted" }, el("strong", {}, "Done. "), "You can change this later.") : null,
        current.action && !current.done ? actions(ui.button(current.action.label, { kind: "primary", ...current.action })) : null);
    }
    return el("div", { class: "pp-view-inner" },
      ui.header({ title: "Setup", text: "Set up your organization step by step. Each step can be changed later.",
        actions: ui.button("Start another organization", { href: "/start", icon: "plus" }) }),
      ui.stepper(steps.map((s) => ({ key: s.key, title: s.title, done: s.done })), current.key, (k) => PPE.go("setup", k)),
      body,
      ui.section({ title: "All steps" }, ui.checklist(steps.map((s) => ({ title: s.title, text: s.text, done: s.done,
        action: s.key === "address" ? { label: "Choose", href: PPE.href("setup", "address") } : s.action })))));
  }

  /// Signed in, but nothing to administer here: the one page shown.
  function gateCard() {
    const s = app.state;
    if (s === "soon") {
      return ui.empty({ icon: "info", title: "Coming soon", text: "Organizations are opening gradually.", action: { label: "See packages", href: "/" } });
    }
    if (s === "member") {
      return ui.empty({ icon: "inbox", title: "Your company workspace is in the inbox", text: "Rooms, people and your company address are all there. This page is for admins.",
        action: { label: "Open your inbox", href: cfg.inbox || "https://inbox.pigeonpost.dev" } });
    }
    return el("div", { class: "pp-view-inner" },
      ui.header({ title: "No organization yet", text: "Create one for your team, or ask your admin for an invitation." }),
      ui.stepper([{ key: "org", title: "Your organization" }, { key: "billing", title: "Billing details" }, { key: "pay", title: "Review and pay" }], "org"),
      ui.card({ title: "Create an organization", text: "Choose a name and a company address, add billing details, then pay securely with iyzico. You become the organization's owner." },
        actions(ui.button("See packages", { href: "/" }), ui.button("Create an organization", { kind: "primary", href: "/start" }))),
      ui.card({ title: "Have an invitation?", text: "Open the link in the invitation you received. It works only for the address it was sent to." }));
  }

  // ---- People ----------------------------------------------------------------------------------

  async function people() {
    const { members = [] } = await PPE.postbox(orgPath("/members"));
    app.members = members;
    const o = app.org;
    const live = members.filter((m) => m.status !== "removed");
    const invite = ui.button("Invite people", { kind: "primary", icon: "plus", onclick: () => inviteSheet(), disabled: !writable() });
    return el("div", { class: "pp-view-inner" },
      ui.header({ title: "People", text: `${o.seats_used ?? live.length} of ${o.seats} seats used`, actions: invite }),
      ui.table({
        rows: live, search: "Search people", caption: "People",
        filters: [{ key: "status", label: "Status", options: [["active", "Active"], ["invited", "Invited"], ["suspended", "Suspended"]] }],
        empty: { icon: "people", title: "No one here yet", text: "Invite your first teammates.", action: writable() ? { label: "Invite people", onclick: () => inviteSheet() } : null },
        onRow: (m) => memberSheet(m),
        columns: [
          { key: "name", label: "Name", sort: (m) => fullName(m), render: (m) => el("div", {}, el("span", { translate: "no" }, fullName(m) || "—"), el("span", { class: "sub" }, m.contact_masked || "")) },
          { key: "handle", label: "Address", mono: true, sort: true, render: (m) => el("span", { translate: "no" }, m.handle || m.address || "") },
          { key: "roles", label: "Role", render: (m) => ((m.roles || []).length ? m.roles.map((r) => ui.badge(ROLE_NAMES[r] || r, r === "owner" ? "info" : "neutral")) : el("span", { class: "pp-muted" }, "Member")) },
          { key: "status", label: "Status", sort: true, render: (m) => el("div", {}, statusBadge(m.status), m.invite && m.invite.state === "expired" ? el("span", { class: "sub" }, "Expired — resend") : null) },
          { key: "last", label: "Last active", sort: (m) => m.last_active_at || m.invited_at || 0, render: (m) => (m.status === "invited" ? `Invited ${PPE.date(m.invited_at)}` : PPE.ago(m.last_active_at)) },
          { key: "act", label: "", render: (m) => ui.button("Manage", { size: "sm", onclick: () => memberSheet(m) }) },
        ],
      }));
  }
  app.people = people;

  /// Parse pasted CSV rows: first, last, contact[, handle].
  app.parseRows = function (text) {
    return String(text || "").split(/\r?\n/).map((line) => line.split(/[,;\t]/).map((c) => c.trim())).filter((c) => c.length >= 3 && c[2])
      .slice(0, 200).map(([first_name, last_name, contact, handle]) => ({ first_name, last_name, contact, handle: handle || "" }));
  };
  app.handleFor = (first) => String(first || "").toLowerCase().normalize("NFKD").replace(/[\u0300-\u036f]/g, "").replace(/ı/g, "i").replace(/[^a-z0-9._-]/g, "").slice(0, 32);

  function inviteSheet() {
    PPE.openSheet("Invite people", (body) => {
      const list = el("div", { class: "invite-list" });
      const grantable = app.grantable();
      const addRow = (v = {}) => {
        const first = input({ maxlength: 60, value: v.first_name || "", "aria-label": "First name" });
        const last = input({ maxlength: 60, value: v.last_name || "", "aria-label": "Last name" });
        const contact = input({ maxlength: 254, spellcheck: "false", value: v.contact || "", "aria-label": "Contact address" });
        const handle = input({ maxlength: 32, spellcheck: "false", value: (v.handle || "").replace(`/${ns()}/`, ""), "aria-label": "Company address" });
        let touched = Boolean(v.handle);
        first.addEventListener("input", () => { if (!touched) handle.value = app.handleFor(first.value); });
        handle.addEventListener("input", () => { touched = true; });
        const role = select({ "aria-label": "Role" }, el("option", { value: "" }, "Member"), grantable.map((r) => el("option", { value: r }, ROLE_NAMES[r])));
        const remove = ui.button("Remove", { kind: "quiet", size: "sm", onclick: () => row.remove() });
        const row = el("div", { class: "pp-card invite-row" },
          el("div", { class: "row" }, PPE.field("First name", first), PPE.field("Last name", last)),
          PPE.field("Contact address", contact, "The invitation goes here, and they must sign in with this address."),
          PPE.field("Company address", prefixed(handle)),
          grantable.length ? PPE.field("Role", role) : null,
          actions(remove));
        row._read = () => ({ first_name: first.value.trim(), last_name: last.value.trim(), contact: contact.value.trim(),
          handle: handle.value.trim() ? `/${ns()}/${handle.value.trim().toLowerCase()}` : "", roles: role.value ? [role.value] : [] });
        list.append(row);
      };
      const paste = textarea({ "aria-label": "Paste rows", placeholder: "Jane, Doe, jane@acme.com, jane" });
      const status = statusLine();
      addRow();
      const send = act("Send invitations", async () => {
        const invites = Array.from(list.children).map((r) => r._read()).filter((r) => r.first_name || r.contact);
        const wrong = invites.find((r) => !r.first_name || !r.last_name || !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(r.contact) || !/^\/[a-z0-9][a-z0-9._-]*\/[a-z0-9][a-z0-9._-]{0,31}$/.test(r.handle));
        if (!invites.length || wrong) return bad(status, "Every row needs a first and last name, a valid contact address and a company address.");
        const r = await PPE.postbox(orgPath("/invites"), { method: "POST", body: { invites } });
        inviteResults(r.results || [], invites);
      }, { kind: "primary" });
      body.append(list,
        actions(ui.button("Add another", { icon: "plus", onclick: () => addRow() })),
        el("details", { class: "paste" }, el("summary", {}, "Paste a list"),
          PPE.field("One person per line: first name, last name, contact address, and optionally their address", paste),
          ui.button("Add these", { size: "sm", onclick: () => { app.parseRows(paste.value).forEach(addRow); paste.value = ""; } })),
        status, actions(send));
    });
  }

  function inviteResults(results, asked) {
    PPE.openSheet("Invitations", (body) => {
      const once = inviteEmailOn() ? null : "Shown once. Send it to them yourself — it only works for the invited address.";
      for (const [i, r] of results.entries()) {
        const who = asked[i] ? `${asked[i].first_name} ${asked[i].last_name}` : r.handle;
        const ok = r.status === "invited";
        body.append(el("div", { class: "pp-card" },
          el("h3", {}, el("span", { translate: "no" }, who), " ", ui.badge(ok ? "Invited" : PPE.explain({ code: r.status, message: r.status }), ok ? "success" : "danger")),
          el("p", { class: "pp-mono pp-muted", translate: "no" }, r.handle || ""),
          ok && r.link ? secretBox(r.link, once) : ok ? el("p", { class: "pp-muted" }, "The invitation is on its way.") : null));
      }
      body.append(actions(ui.button("Done", { kind: "primary", onclick: () => { PPE.closeSheet(); go("people"); } })));
    });
  }

  function memberSheet(m) {
    PPE.openSheet(fullName(m) || m.handle, (body) => {
      const isOwner = (m.roles || []).includes("owner");
      const first = input({ value: m.first_name || "", maxlength: 60 });
      const last = input({ value: m.last_name || "", maxlength: 60 });
      const saveName = act("Save name", async () => {
        await PPE.postbox(orgPath(`/members/${encodeURIComponent(m.id)}`), { method: "PATCH", body: { first_name: first.value.trim(), last_name: last.value.trim() } });
        PPE.toast("Saved"); go("people");
      }, { size: "sm" });
      body.append(ui.kv([["Address", el("span", { class: "pp-mono", translate: "no" }, m.handle || "—")], ["Status", statusBadge(m.status)], ["Invited to", m.contact_masked || "—"]]),
        el("div", { class: "row" }, PPE.field("First name", first), PPE.field("Last name", last)), actions(saveName));

      const grantable = app.grantable();
      if (!isOwner && grantable.length && m.status !== "invited") {
        const boxes = grantable.map((r) => check(el("input", { type: "checkbox", value: r, checked: (m.roles || []).includes(r) }), ROLE_NAMES[r]));
        const saveRoles = act("Save roles", async () => {
          const roles = boxes.map((b) => b.querySelector("input")).filter((c) => c.checked).map((c) => c.value);
          const keep = (m.roles || []).filter((r) => !grantable.includes(r));
          await setRoles(m.id, [...keep, ...roles]);
        }, { size: "sm" });
        body.append(el("h3", {}, "Roles"), ...boxes, el("p", { class: "hint" }, "Admin roles never include reading anyone's messages."), actions(saveRoles));
      }

      const row = actions();
      if (m.status === "invited" && m.invite) {
        row.append(act("Resend invitation", async () => {
          const r = await PPE.postbox(orgPath(`/invites/${encodeURIComponent(m.invite.id)}/resend`), { method: "POST", body: {} });
          PPE.openSheet("New invitation link", (b) => b.append(el("p", {}, "The old link no longer works."),
            r.link ? secretBox(r.link, inviteEmailOn() ? null : "Shown once.") : el("p", {}, "The invitation is on its way.")));
        }), act("Revoke invitation", async () => {
          await PPE.postbox(orgPath(`/invites/${encodeURIComponent(m.invite.id)}`), { method: "DELETE" });
          PPE.closeSheet(); PPE.toast("Invitation revoked"); go("people");
        }, { kind: "danger" }));
      } else if (!isOwner && (m.status === "active" || m.status === "suspended")) {
        row.append(act(m.status === "active" ? "Suspend" : "Reactivate", async () => {
          await PPE.postbox(orgPath(`/members/${encodeURIComponent(m.id)}`), { method: "PATCH", body: { status: m.status === "active" ? "suspended" : "active" } });
          PPE.closeSheet(); PPE.toast(m.status === "active" ? "Suspended" : "Reactivated"); go("people");
        }), ui.button("Remove…", { kind: "danger", onclick: () => removeSheet(m) }));
      }
      body.append(row);
    });
  }

  PPE.onResume("app.roles", async (p) => setRoles(p.member_id, p.roles));
  async function setRoles(memberId, roles) {
    await PPE.postbox(orgPath(`/members/${encodeURIComponent(memberId)}`), { method: "PATCH", body: { roles }, resume: { name: "app.roles", payload: { member_id: memberId, roles } } });
    PPE.closeSheet(); PPE.toast("Roles updated"); go("people");
  }

  function removeSheet(m) {
    PPE.openSheet(`Remove ${m.first_name || m.handle}`, (body) => {
      const keep = el("input", { type: "radio", name: "mode", value: "reserve", checked: true });
      const del = el("input", { type: "radio", name: "mode", value: "delete" });
      const confirm = act("Remove", async () => {
        await PPE.postbox(orgPath(`/members/${encodeURIComponent(m.id)}/remove`), { method: "POST", body: { mode: keep.checked ? "reserve" : "delete" } });
        PPE.closeSheet(); PPE.toast("Removed"); go("people");
      }, { kind: "danger" });
      body.append(el("p", {}, `${m.handle} loses access to the company mailbox, rooms and every agent delegated to them. Their personal account is not affected.`),
        check(keep, el("strong", {}, "Keep the address for someone else. "), "Invite a new person to the same address later. They start with an empty mailbox; history doesn't move."),
        check(del, el("strong", {}, "Delete the address. "), "Mail to it stops being delivered and the name is freed."),
        actions(ui.button("Cancel", { onclick: () => PPE.closeSheet() }), confirm));
    });
  }

  // ---- Agents ----------------------------------------------------------------------------------

  async function agents() {
    const { agents: list = [] } = await PPE.postbox(orgPath("/agents"));
    const o = app.org;
    const rows = list.filter((a) => a.status !== "deleted");
    const add = ui.button("Add agent", { kind: "primary", icon: "plus", onclick: () => addAgentSheet(), disabled: !writable() });
    return el("div", { class: "pp-view-inner" },
      ui.header({ title: "Agents", text: `${o.agents_used ?? rows.length} of ${o.agent_limit}`, actions: add }),
      ui.table({
        rows, search: "Search agents", caption: "Agents", onRow: (a) => agentSheet(a),
        empty: { icon: "agents", title: "No agents yet", text: "Add one and log it in on its machine." },
        columns: [
          { key: "handle", label: "Address", sort: true, render: (a) => el("div", {}, el("span", { class: "pp-mono", translate: "no" }, a.handle), a.label ? el("span", { class: "sub" }, a.label) : null) },
          { key: "operators", label: "Operators", render: (a) => ((a.operators || []).length ? a.operators.map((p) => ui.badge(p.handle)) : el("span", { class: "pp-muted" }, "Agent admins")) },
          { key: "machine", label: "Machine", render: (a) => (a.devices ? (a.last_seen_at ? `Last seen ${PPE.ago(a.last_seen_at)}` : "Connected") : "Never connected") },
          { key: "status", label: "Status", sort: true, render: (a) => statusBadge(a.status) },
          { key: "act", label: "", render: (a) => ui.button("Manage", { size: "sm", onclick: () => agentSheet(a) }) },
        ],
      }));
  }

  app.connectCommand = (handle, label) => `pigeonpost login --device --handle ${handle}${label ? ` --label "${String(label).replace(/["\\$`]/g, "")}"` : ""} --onboard`;

  async function addAgentSheet() {
    let members = app.members;
    if (!members) { try { members = (await PPE.postbox(orgPath("/members"))).members || []; } catch { members = []; } }
    PPE.openSheet("Add agent", (body) => {
      const name = input({ maxlength: 32, spellcheck: "false", autocapitalize: "none", "aria-label": "Address" });
      const label = input({ maxlength: 80, placeholder: "Build box" });
      const ops = members.filter((m) => m.status === "active" && !(m.roles || []).some((r) => NEST_ADMIN.includes(r)))
        .map((m) => check(el("input", { type: "checkbox", value: m.id }), `${m.first_name} ${m.last_name} `, el("span", { class: "pp-muted pp-mono" }, m.handle)));
      const how = ["qr", "code", "terminal"].map((v, i) => check(el("input", { type: "radio", name: "how", value: v, checked: i === 0 }),
        el("strong", {}, ["QR on the machine. ", "Enrollment code. ", "From the terminal. "][i]),
        ["Run one command on the machine and approve it from your phone.", "A one-time code, valid for 24 hours, for scripted installs.", "Run the command yourself later."][i]));
      const status = statusLine();
      const create = act("Add agent", async () => {
        const v = name.value.trim().toLowerCase();
        if (!/^[a-z0-9][a-z0-9._-]{0,31}$/.test(v)) return bad(status, "Use letters, numbers, dots, underscores or hyphens.");
        const operators = ops.map((o) => o.querySelector("input")).filter((c) => c.checked).map((c) => c.value);
        const r = await PPE.postbox(orgPath("/agents"), { method: "POST", body: { handle: `/${ns()}/${v}`, ...(label.value.trim() ? { label: label.value.trim() } : {}), operators } });
        const choice = how.map((h) => h.querySelector("input")).find((c) => c.checked).value;
        connectSheet(r.agent || { handle: `/${ns()}/${v}`, address: null, label: label.value.trim() }, choice);
      }, { kind: "primary" });
      body.append(PPE.field("Address", prefixed(name)),
        PPE.field("Label", label, "Where it runs, so people recognize it."),
        ops.length ? el("fieldset", { class: "pp-card" }, el("legend", {}, "Operators: people who may log it in"), ...ops) : null,
        el("fieldset", { class: "pp-card" }, el("legend", {}, "How to connect"), ...how),
        status, actions(create));
    });
  }

  function connectSheet(agent, choice) {
    PPE.openSheet(`Connect ${agent.handle}`, (body) => {
      if (choice === "code") {
        body.append(el("p", {}, "Creating a code needs a fresh, strong sign-in. The code works once, for this agent only."),
          actions(act("Create enrollment code", () => enroll(agent), { kind: "primary" })));
      } else {
        body.append(el("p", {}, "On the agent's machine, run:"), secretBox(app.connectCommand(agent.handle, agent.label)),
          el("p", { class: "pp-muted" }, "It shows a QR code and a confirmation number. Scan it with your phone (or open the link), check the number matches, and hold Approve."));
      }
      body.append(actions(ui.button("Done", { onclick: () => { PPE.closeSheet(); go("agents"); } })));
    });
  }

  PPE.onResume("app.enroll", async (p) => enroll({ address: p.address, handle: p.handle }));
  async function enroll(agent) {
    const key = agent.address || agent.handle;
    const r = await PPE.postbox(orgPath(`/agents/${encodeURIComponent(key)}/enrollments`), { method: "POST", body: { ttl_hours: 24 },
      resume: { name: "app.enroll", payload: { address: agent.address, handle: agent.handle } } });
    PPE.openSheet(`Enrollment code for ${agent.handle}`, (body) => body.append(
      el("p", {}, `Valid until ${new Date(r.expires_at * 1000).toLocaleString(PPE.locale())}. Shown once.`),
      secretBox(r.code),
      el("p", {}, "On the machine, paste it into:"),
      secretBox("pigeonpost login --enroll-stdin --onboard"),
      el("p", { class: "hint" }, "Reading it from standard input keeps the code out of shell history.")));
  }

  async function agentSheet(a) {
    const ssh = sshOn() ? await sshState(a).catch(() => null) : null;
    const [devices, delegations, members] = await Promise.all([
      PPE.postbox(orgPath(`/agents/${encodeURIComponent(a.address)}/devices`)).then((r) => r.devices || []).catch(() => []),
      PPE.postbox(orgPath("/delegations")).then((r) => (r.delegations || []).filter((d) => d.target === a.address || d.target === a.handle)).catch(() => []),
      app.members ? Promise.resolve(app.members) : PPE.postbox(orgPath("/members")).then((r) => r.members || []).catch(() => []),
    ]);
    PPE.openSheet(a.handle, (body) => {
      const label = input({ maxlength: 80, value: a.label || "" });
      body.append(PPE.field("Label", label), actions(act("Save", async () => {
        await PPE.postbox(orgPath(`/agents/${encodeURIComponent(a.address)}`), { method: "PATCH", body: { label: label.value.trim() } });
        PPE.toast("Saved"); go("agents");
      }, { size: "sm" })));

      body.append(el("h3", {}, "Machines"));
      const live = devices.filter((d) => !d.revoked_at);
      if (!live.length) body.append(el("p", { class: "pp-muted" }, "Not logged in anywhere."), el("p", {}, "Run on the machine:"), secretBox(app.connectCommand(a.handle, a.label)));
      for (const d of live) {
        body.append(el("div", { class: "setting" }, el("p", {}, d.label || "Machine", el("span", { class: "sub pp-muted" }, ` · since ${PPE.date(d.created_at)} · ${d.source}`)),
          act("Disconnect", async () => {
            await PPE.postbox(orgPath(`/devices/${encodeURIComponent(d.id)}`), { method: "DELETE" });
            PPE.toast("Disconnected"); PPE.closeSheet(); go("agents");
          }, { size: "sm", kind: "danger" })));
      }
      body.append(actions(act("New enrollment code", () => enroll(a), { size: "sm" })));

      body.append(el("h3", {}, "Operators"));
      for (const d of delegations) {
        body.append(el("div", { class: "setting" }, el("p", {}, d.member_handle, el("span", { class: "sub pp-muted" }, ` · ${(d.capabilities || []).join(", ")}${d.expires_at ? ` · until ${PPE.date(d.expires_at)}` : ""}`)),
          act("Revoke", async () => {
            await PPE.postbox(orgPath(`/delegations/${encodeURIComponent(d.id)}`), { method: "DELETE" });
            PPE.toast("Revoked"); PPE.closeSheet(); go("agents");
          }, { size: "sm" })));
      }
      const who = select({ "aria-label": "Person" }, members.filter((m) => m.status === "active").map((m) => el("option", { value: m.id }, `${m.first_name} ${m.last_name} (${m.handle})`)));
      const caps = CAPABILITIES.map(([k, t]) => check(el("input", { type: "checkbox", value: k, checked: k === "enroll" }), t));
      const until = input({ type: "date", "aria-label": "Until (optional)" });
      const grant = act("Add operator", async () => {
        const capabilities = caps.map((c) => c.querySelector("input")).filter((c) => c.checked).map((c) => c.value);
        if (!who.value || !capabilities.length) return PPE.toast("Choose a person and at least one permission.", "error");
        const expires_at = until.value ? Math.floor(Date.parse(until.value + "T23:59:59Z") / 1000) : undefined;
        await PPE.postbox(orgPath("/delegations"), { method: "POST", body: { member_id: who.value, target: a.address, capabilities, ...(expires_at ? { expires_at } : {}) } });
        PPE.toast("Operator added"); PPE.closeSheet(); go("agents");
      }, { size: "sm" });
      body.append(el("details", {}, el("summary", {}, "Add an operator"), PPE.field("Person", who), ...caps, PPE.field("Until (optional)", until), actions(grant)));

      if (sshOn()) body.append(sshSection(a, ssh));

      body.append(actions(
        act(a.status === "active" ? "Suspend" : "Reactivate", async () => {
          await PPE.postbox(orgPath(`/agents/${encodeURIComponent(a.address)}`), { method: "PATCH", body: { status: a.status === "active" ? "suspended" : "active" } });
          PPE.closeSheet(); go("agents");
        }),
        act("Delete agent", async () => {
          if (!(await ask(`Delete ${a.handle}? Its machines are disconnected and the address stops receiving.`, { confirm: "Delete agent" }))) return;
          await PPE.postbox(orgPath(`/agents/${encodeURIComponent(a.address)}`), { method: "DELETE" });
          PPE.closeSheet(); PPE.toast("Deleted"); go("agents");
        }, { kind: "danger" })));
    });
  }

  // ---- Rooms -----------------------------------------------------------------------------------

  async function rooms() {
    const { rooms: list = [] } = await PPE.postbox(orgPath("/rooms"));
    const create = ui.button("Create room", { kind: "primary", icon: "plus", onclick: () => roomSheet() });
    return el("div", { class: "pp-view-inner" },
      ui.header({ title: "Rooms", text: "Company rooms are end-to-end encrypted. New members join auto-join rooms when they accept.", actions: create }),
      ui.table({
        rows: list, search: "Search rooms", caption: "Rooms", onRow: (r) => manageRoom(r),
        empty: { icon: "rooms", title: "No rooms yet." },
        columns: [
          { key: "name", label: "Room", sort: true, render: (r) => el("div", {}, el("span", { translate: "no" }, r.name), r.description ? el("span", { class: "sub" }, r.description) : null) },
          { key: "member_count", label: "Members", num: true, sort: (r) => r.member_count || 0, render: (r) => String(r.member_count ?? 0) },
          { key: "post", label: "Who can post", render: (r) => (r.post_policy === "admins" ? "Admins only" : "Everyone") },
          { key: "listed", label: "Directory", render: (r) => el("div", {}, r.listed ? "Listed" : "Unlisted", r.join_policy === "private" ? el("span", { class: "sub" }, "Invite only") : null) },
          { key: "auto", label: "Auto-join", render: (r) => (r.auto_join ? "Yes" : "No") },
          { key: "state", label: "State", render: (r) => (r.activation_state === "provisioning" ? statusBadge("provisioning") : statusBadge("active")) },
          { key: "act", label: "", render: (r) => ui.button("Manage", { size: "sm", onclick: () => manageRoom(r) }) },
        ],
      }),
      list.some((r) => r.activation_state === "provisioning") ? el("p", { class: "note" }, "A room is set up when its creator next opens the inbox: their device creates the encryption keys.") : null);
  }

  function roomSheet() {
    PPE.openSheet("Create room", (body) => {
      const name = input({ maxlength: 80, placeholder: "design" });
      const desc = textarea({ maxlength: 500 });
      const join = select({}, el("option", { value: "public" }, "Anyone at the company can join"), el("option", { value: "private" }, "Invite only"));
      const post = select({}, el("option", { value: "everyone" }, "Everyone"), el("option", { value: "admins" }, "Only room admins"));
      const listed = el("input", { type: "checkbox", checked: true });
      const auto = el("input", { type: "checkbox" });
      body.append(PPE.field("Name", name), PPE.field("Description (optional)", desc), PPE.field("Who can join", join), PPE.field("Who can post", post),
        check(listed, "Listed in the company directory"),
        check(auto, "Add everyone who joins the company"),
        actions(act("Create room", async () => {
          if (!name.value.trim()) return PPE.toast("Give the room a name.", "error");
          await PPE.postbox(orgPath("/rooms"), { method: "POST", body: { name: name.value.trim(), ...(desc.value.trim() ? { description: desc.value.trim() } : {}),
            join_policy: join.value, post_policy: post.value, listed: listed.checked, auto_join: auto.checked } });
          PPE.closeSheet(); PPE.toast("Room created. Open your inbox to finish setting up its encryption."); go("rooms");
        }, { kind: "primary" })));
    });
  }

  function manageRoom(r) {
    PPE.openSheet(r.name, (body) => {
      const listed = el("input", { type: "checkbox", checked: Boolean(r.listed) });
      const auto = el("input", { type: "checkbox", checked: Boolean(r.auto_join) });
      body.append(check(listed, "Listed in the company directory"),
        check(auto, "Add everyone who joins the company"),
        el("p", { class: "hint" }, "Members, posting and history are managed in the room itself, in the inbox."),
        actions(act("Delete room", async () => {
          if (!(await ask(`Delete ${r.name}? Members see "This group was deleted" and its messages are purged within 30 days.`, { confirm: "Delete room" }))) return;
          await PPE.postbox(orgPath(`/rooms/${encodeURIComponent(r.id)}`), { method: "DELETE" });
          PPE.closeSheet(); PPE.toast("Deleted"); go("rooms");
        }, { kind: "danger" }), act("Save", async () => {
          await PPE.postbox(orgPath(`/rooms/${encodeURIComponent(r.id)}`), { method: "PATCH", body: { listed: listed.checked, auto_join: auto.checked } });
          PPE.closeSheet(); PPE.toast("Saved"); go("rooms");
        }, { kind: "primary" })));
    });
  }

  // ---- Nests (org-owned Nests, Nests Phase B7) ---------------------------------------------------

  const nestBuyer = () => has("owner", "billing_admin");
  const nestCount = (n) => (Number(n) === 1 ? "1 Nest" : T("{1} Nests", { 1: n }));
  const nestAdmin = () => has(...NEST_ADMIN);
  const PACKAGE_NAMES = (packages) => Object.fromEntries(((packages && packages.packages) || []).map((p) => [p.code, p.name || p.code]));
  const newIntent = () => {
    const b = new Uint8Array(16);
    window.crypto.getRandomValues(b);
    return Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
  };

  async function nests() {
    const [v, bridge, packages, profiles] = await Promise.all([
      PPE.postbox(orgPath("/nests")),
      nestBuyer() ? soft(PPE.bridge(`/v1/nests?org_id=${encodeURIComponent(app.org.id)}`)) : null,
      soft(PPE.postbox("/v1/nest-packages")),
      nestBuyer() ? soft(PPE.bridge("/v1/billing/profiles").then((r) => r.profiles || [])) : null,
    ]);
    const a = v.allowance || { slots: 0, used: 0 };
    const names = PACKAGE_NAMES(packages);
    const list = v.nests || [];
    const full = !v.package || a.used >= a.slots;
    const create = nestAdmin() ? ui.button("Create a company Nest", { kind: "primary", icon: "plus", disabled: full || !writable(), title: full ? "The company has no free Nest slot." : null, onclick: () => nestSheet() }) : null;
    const parts = [
      ui.header({ title: "Nests", text: "Private agent workspaces for the company. Each one uses a company agent address and a slot of the company's Nest package.", actions: create }),
      ui.stats([
        { label: "Package", value: v.package ? (names[v.package.code] || v.package.code) : "None", hint: v.package ? nestCount(v.package.slots) : "No Nest package yet" },
        { label: "In use", value: `${a.used} / ${a.slots}`, hint: "Company Nests" },
      ]),
      ui.table({
        rows: list, caption: "Company Nests", search: list.length > 6 ? "Search Nests" : null,
        empty: { icon: "nest", title: "No company Nests yet", text: v.package ? "Create one for a team or a project." : "Buy a Nest package for the company first." },
        columns: [
          { key: "label", label: "Name", sort: true, render: (n) => el("span", { translate: "no" }, n.label || "—") },
          { key: "address", label: "Address", mono: true, render: (n) => el("span", { translate: "no" }, n.address || "—") },
          { key: "state", label: "State", sort: true, render: (n) => ui.status(n.state, { running: ["Running", "success"], ready: ["Ready", "success"], provisioning: ["Setting up", "warning"], paused: ["Paused", "neutral"], failed: ["Failed", "danger"] }) },
          { key: "role", label: "Your access", render: (n) => (n.role === "owner" ? "Admin" : "Operator") },
          { key: "act", label: "", render: (n) => (nestAdmin() ? ui.button("Operators", { size: "sm", onclick: () => operatorsSheet(n) }) : null) },
        ],
      }),
    ];
    parts.push(nestPackages(v, bridge, packages, profiles));
    return el("div", { class: "pp-view-inner" }, ...parts);
  }

  /// The company's Nest package: what it has, or what it can buy (owner and billing admins).
  function nestPackages(v, bridge, packages, profiles) {
    if (!nestBuyer()) {
      return v.package ? null : ui.card({ title: "Nest packages", text: "This company has no Nest package yet. Its billing admin can buy one." });
    }
    if (!bridge) return ui.card({ title: "Nest packages", text: "Nest prices could not be loaded. Try again shortly." });
    const subs = bridge.subscriptions || [];
    if (subs.length) return nestSubscription(subs[0], bridge, packages);
    if (v.package) return ui.card({ title: "Nest package", text: "The company's Nest package is funded another way (a grant or another store). Contact support to change it." });
    const plans = ((bridge.prices && bridge.prices.plans) || []).filter((p) => p.family === "nest" && p.available !== false);
    const open = (bridge.prices && bridge.prices.open_currencies) || ["USD"];
    const currency = open.includes("USD") ? "USD" : open[0];
    const sellable = ((packages && packages.packages) || []).filter((p) => p.sellable !== false);
    const profile = (profiles || [])[0];
    const tiles = el("div", { class: "pp-tiles" }, sellable.map((p) => {
      const plan = plans.find((pl) => pl.package === p.code && pl.currency === currency);
      return el("div", { class: "pp-tile", "data-package": p.code },
        el("div", { class: "pp-tile-head" }, el("span", { class: "pp-tile-logo", "aria-hidden": "true" }, PPE.icon("nest")),
          el("div", { class: "pp-tile-title" }, el("strong", {}, p.name || p.code), el("span", {}, nestCount(p.slots)))),
        p.model === "managed" ? el("p", { class: "pp-muted" }, "AI included, shared by the company's Nests.") : el("p", { class: "pp-muted" }, "No AI connected: a private workspace you set up yourself"),
        el("div", { class: "pp-tile-foot" },
          plan ? el("span", { class: "pp-price" }, `${PPE.money(plan.amount, plan.currency)} ${T("per month")}`) : el("span", { class: "pp-muted" }, "Not available to buy here yet."),
          plan ? ui.button("Buy for the company", { kind: "primary", size: "sm", "aria-label": `Buy ${p.name}`, onclick: () => buyNest(plan.code, profile) }) : null));
    }));
    const box = ui.section({ title: "Buy a Nest package for the company", text: "Sold by Piyote Bilişim and billed monthly through iyzico. Cancel any time; the Nests keep running until the end of the paid month." },
      profile ? tiles : ui.gate({ text: "Add billing details first, then buy a Nest package.", action: { label: "Add billing details", href: PPE.href("billing", "details") } }, tiles));
    return box;
  }

  function buyNest(planCode, profile) {
    if (!profile) { PPE.toast("Add your billing details first, then buy a Nest package."); return PPE.go("billing", "details"); }
    // The iyzico form runs only on /checkout, the one page whose policy admits it.
    PPE.store.set("ppe_nest_checkout", JSON.stringify({ intent_id: newIntent(), plan_code: planCode, billing_profile_id: profile.id || profile.billing_profile_id,
      locale: (document.documentElement.lang || "en").startsWith("tr") ? "tr" : "en", org_id: app.org.id }));
    window.location.assign("/checkout");
  }

  function nestSubscription(sub, bridge, packages) {
    const names = PACKAGE_NAMES(packages);
    const plans = ((bridge.prices && bridge.prices.plans) || []).filter((p) => p.family === "nest" && p.currency === sub.currency && p.code !== sub.plan_code);
    const title = `${names[sub.package] || sub.package || "Nest package"} · ${sub.currency}${sub.recurring_price ? ` · ${PPE.money(sub.recurring_price.amount, sub.recurring_price.currency)} ${T("per month")}` : ""}`;
    if (sub.buyer === false) {
      return ui.card({ title: "Company web Nest package", text: title }, el("p", { class: "pp-muted" }, "Another billing admin bought this package. Only they can change or cancel it."));
    }
    const pick = select({ "aria-label": "Change to" }, plans.map((pl) => el("option", { value: pl.code }, names[pl.package] || pl.code)));
    const canceled = sub.provider_status === "CANCELED";
    return ui.card({ title: "Company web Nest package", text: title,
      actions: canceled ? null : act("Cancel package", async () => {
        if (!(await ask("Cancel the company's Nest package? Its Nests keep running until the end of the paid month.", { confirm: "Cancel package" }))) return;
        await PPE.bridge(`/v1/nests/subscriptions/${encodeURIComponent(sub.id)}/cancel`, { method: "POST", body: { intent_id: newIntent() } });
        PPE.toast("Canceled. The Nests keep running until the end of the paid month."); go("nests");
      }, { kind: "danger", size: "sm" }) },
      canceled ? el("p", { class: "pp-muted" }, "Canceled — no renewal.") : null,
      plans.length && !canceled ? el("div", { class: "inline-form" }, PPE.field("Change to", pick), act("Change package", async () => {
        const r = await PPE.bridge(`/v1/nests/subscriptions/${encodeURIComponent(sub.id)}/upgrade`, { method: "POST", body: { intent_id: newIntent(), plan_code: pick.value } });
        PPE.toast(r.effective === "now" ? "The larger package is active now." : "The package changes at the next renewal.");
        go("nests");
      })) : null);
  }

  const NEST_ERRORS = { no_nest_slot: "The company has no free Nest slot. Its billing admin can buy or upgrade a package.", agent_limit: "The company has used all its agent addresses.",
    handle_taken: "That address is taken.", invalid_handle: "That isn't a valid address in your company namespace.", bad_label: "Names are 1 to 64 printable characters.",
    invalid_member: "Someone you picked is no longer an active member. Reload and try again.", too_many_operators: "A Nest can have at most 50 operators." };

  async function operatorChoices() {
    if (!app.can("people")) return [];
    const members = app.members || (await soft(PPE.postbox(orgPath("/members?status=active")).then((r) => r.members || []))) || [];
    return members.filter((m) => m.status === "active" && m.id !== (app.me && app.me.member_id) && !(m.roles || []).some((r) => NEST_ADMIN.includes(r)));
  }

  async function nestSheet() {
    const people = await operatorChoices();
    PPE.openSheet("Create a company Nest", (body) => {
      const handle = input({ maxlength: 32, spellcheck: "false", autocapitalize: "none", "aria-label": "Address in your company namespace" });
      const label = input({ maxlength: 64, "aria-label": "Name" });
      const boxes = people.map((m) => check(el("input", { type: "checkbox", value: m.id }), fullName(m) || m.handle));
      const status = statusLine();
      body.append(PPE.field("Address in your company namespace", prefixed(handle)), PPE.field("Name", label),
        boxes.length ? el("fieldset", { class: "pp-card" }, el("legend", {}, "Who can use it (admins always can)"), ...boxes) : el("p", { class: "hint" }, "Admins can always use it. People admins can add operators."),
        status, actions(act("Create", async () => {
          const h = handle.value.trim().toLowerCase();
          if (!/^[a-z0-9][a-z0-9._-]{0,31}$/.test(h)) return bad(status, "Use letters, numbers, dots, underscores or hyphens.");
          if (!label.value.trim()) return bad(status, "Names are 1 to 64 printable characters.");
          const operators = boxes.map((b) => b.querySelector("input")).filter((c) => c.checked).map((c) => c.value);
          try {
            await PPE.postbox(orgPath("/nests"), { method: "POST", body: { handle: `/${ns()}/${h}`, label: label.value.trim(), ...(operators.length ? { operators } : {}) } });
          } catch (e) { if (e && NEST_ERRORS[e.code]) return bad(status, NEST_ERRORS[e.code]); throw e; }
          PPE.closeSheet(); PPE.toast("Company Nest created."); go("nests");
        }, { kind: "primary" })));
    });
  }

  async function operatorsSheet(n) {
    const url = orgPath(`/nests/${encodeURIComponent(n.nest_id)}/operators`);
    const [current, people] = await Promise.all([PPE.postbox(url).then((r) => r.operators || []), operatorChoices()]);
    PPE.openSheet(T("Who can use {1}", { 1: n.label || n.address }), (body) => {
      const held = new Set(current.map((o) => o.member_id));
      const choices = people.map((m) => ({ id: m.id, name: fullName(m) || m.handle }));
      for (const o of current) if (!choices.some((c) => c.id === o.member_id)) choices.push({ id: o.member_id, name: fullName(o) || o.handle || "Member" });
      const boxes = choices.map((c) => check(el("input", { type: "checkbox", value: c.id, checked: held.has(c.id) }), c.name));
      const status = statusLine();
      body.append(choices.length ? el("fieldset", { class: "pp-card" }, el("legend", {}, "Operators (admins always can)"), ...boxes) : el("p", { class: "pp-muted" }, "No one operates this Nest. Only admins can use it."),
        people.length ? null : el("p", { class: "hint" }, "Only people admins can see the member list, so you can remove operators here but not add them."),
        status, choices.length ? actions(act("Save operators", async () => {
          try {
            const r = await PPE.postbox(url, { method: "PUT", body: { operators: boxes.map((b) => b.querySelector("input")).filter((c) => c.checked).map((c) => c.value) } });
            PPE.toast(T("Saved. {1} can use this Nest besides admins.", { 1: (r.operators || []).length }));
            PPE.closeSheet();
          } catch (e) { if (e && NEST_ERRORS[e.code]) return bad(status, NEST_ERRORS[e.code]); throw e; }
        }, { kind: "primary" })) : null);
    });
  }

  // ---- Services (marketplace listings installed for the org, Marketplace M2b) ---------------------

  async function services() {
    const [installed, catalog] = await Promise.all([
      PPE.postbox(orgPath("/nest-installations")).then((r) => r.data || []),
      soft(PPE.postbox("/v1/marketplace/listings").then((r) => r.data || r.listings || [])),
    ]);
    const listings = catalog || [];
    const titleOf = (id) => (listings.find((l) => l.id === id) || {}).title;
    const tiles = listings.filter((l) => l.status === undefined || l.status === "published").map((l) => {
      const free = !l.price_from && (!l.price || Number(l.price.amount_minor) === 0);
      const hosted = l.hosting === "publisher";
      const price = l.coming_soon ? "Coming soon" : l.price_from && Number(l.price_from.amount_cents) > 0 ? `${ui.money(l.price_from.amount_cents, l.price_from.currency || "USD")} ${T("per run")}`
        : free ? "Free" : `${ui.money(l.price.amount_minor, l.price.currency)}${l.price.term === "monthly" ? ` ${T("per month")}` : ""}`;
      const done = installed.some((i) => i.listing_id === l.id && i.status === "active");
      const action = l.coming_soon ? null
        : done ? ui.badge("Installed", "success")
        : hosted || !free ? ui.button("How to use it", { size: "sm", onclick: () => runNote(l) })
        : nestAdmin() ? ui.button("Install for the company", { size: "sm", kind: "primary", disabled: !writable(), onclick: () => installSheet(l) }) : null;
      return el("div", { class: "pp-tile", "data-listing": l.id },
        el("div", { class: "pp-tile-head" }, el("span", { class: "pp-tile-logo", "aria-hidden": "true" }, String(l.title || "?").trim().slice(0, 2).toUpperCase()),
          el("div", { class: "pp-tile-title" }, el("strong", { translate: "no" }, l.title || l.id), el("span", {}, hosted ? "Hosted by the publisher" : "Runs in a company Nest"))),
        l.summary ? el("p", { class: "pp-muted", translate: "no" }, l.summary) : null,
        el("div", { class: "pp-tile-foot" }, el("span", { class: "pp-price" }, price), action));
    });
    return el("div", { class: "pp-view-inner" },
      ui.header({ title: "Services", text: "Marketplace services installed for the company, and the catalog to add more.",
        actions: ui.button("Browse the marketplace", { href: "https://pigeonpost.dev/marketplace", icon: "external" }) }),
      ui.section({ title: "Installed for the company" }, ui.table({
        rows: installed, caption: "Installed services",
        empty: { icon: "store", title: "Nothing installed yet", text: "Free services can be installed for the company below." },
        columns: [
          { key: "listing", label: "Service", sort: (i) => titleOf(i.listing_id) || i.listing_id, render: (i) => el("span", { translate: "no" }, i.label || titleOf(i.listing_id) || i.listing_id) },
          { key: "agent_address", label: "Address", mono: true, render: (i) => el("span", { translate: "no" }, i.handle || i.agent_address || i.address || "—") },
          { key: "status", label: "Status", sort: true, render: (i) => statusBadge(i.status) },
          { key: "role", label: "Your access", render: (i) => (i.role === "admin" ? "Admin" : "Operator") },
          { key: "created_at", label: "Installed", sort: true, render: (i) => ui.when(i.created_at) },
        ],
      })),
      ui.section({ title: "Catalog", text: "Paid and publisher-hosted services are bought and run from a member's own account with their credits for now; free services that run in a Nest can be installed for the company here." },
        tiles.length ? el("div", { class: "pp-tiles" }, tiles) : ui.empty({ icon: "store", title: "The catalog is empty right now", text: "New services appear here as publishers release them." })));
  }

  function runNote(l) {
    PPE.openSheet(l.title || "Service", (body) => body.append(
      el("p", {}, "Paid and publisher-hosted services are bought and run from a member's own account with their credits for now. Buying them for the whole company is coming."),
      el("p", { class: "pp-muted" }, "Members open it from the marketplace, sign in with their own account and pay from their wallet."),
      actions(ui.button("Open in the marketplace", { kind: "primary", href: `https://pigeonpost.dev/marketplace#/listing/${encodeURIComponent(l.id)}`, icon: "external" }))));
  }

  async function installSheet(l) {
    const people = await operatorChoices();
    PPE.openSheet(T("Install {1} for the company", { 1: l.title || l.id }), (body) => {
      const handle = input({ maxlength: 32, spellcheck: "false", autocapitalize: "none", "aria-label": "Address in your company namespace" });
      const label = input({ maxlength: 64, value: l.title || "", "aria-label": "Name" });
      const boxes = people.map((m) => check(el("input", { type: "checkbox", value: m.id }), fullName(m) || m.handle));
      const status = statusLine();
      body.append(el("p", { class: "pp-muted" }, "It gets its own company agent address. Members you pick can use it; admins always can."),
        PPE.field("Address in your company namespace", prefixed(handle)), PPE.field("Name", label),
        boxes.length ? el("fieldset", { class: "pp-card" }, el("legend", {}, "Who can use it (admins always can)"), ...boxes) : null,
        status, actions(act("Install", async () => {
          const h = handle.value.trim().toLowerCase();
          if (!/^[a-z0-9][a-z0-9._-]{0,31}$/.test(h)) return bad(status, "Use letters, numbers, dots, underscores or hyphens.");
          const operators = boxes.map((b) => b.querySelector("input")).filter((c) => c.checked).map((c) => c.value);
          try {
            await PPE.postbox(orgPath("/nest-installations"), { method: "POST", body: { listing_id: l.id, handle: `/${ns()}/${h}`, ...(label.value.trim() ? { label: label.value.trim() } : {}), ...(operators.length ? { operators } : {}) } });
          } catch (e) { if (e && NEST_ERRORS[e.code]) return bad(status, NEST_ERRORS[e.code]); throw e; }
          PPE.closeSheet(); PPE.toast("Installed for the company."); go("services");
        }, { kind: "primary" })));
    });
  }

  // ---- Rules -----------------------------------------------------------------------------------

  function picker(kind, custom, side) {
    const sel = select({ "aria-label": side === "owners" ? "Who the rule is for" : "Who they hear from" },
      el("option", { value: "everyone" }, side === "owners" ? `Everyone at ${app.org.name}` : `everyone at ${app.org.name}`),
      el("option", { value: "members" }, side === "owners" ? `People at ${app.org.name}` : `people at ${app.org.name}`),
      el("option", { value: "agents" }, side === "owners" ? `Agents at ${app.org.name}` : `agents at ${app.org.name}`),
      side === "peers" ? el("option", { value: "anyone" }, "anyone") : null,
      el("option", { value: "address" }, "a specific address…"));
    sel.value = kind;
    const text = input({ value: custom, placeholder: side === "owners" ? `/${ns()}/agent1` : `/${ns()}/jane`, "aria-label": "Address or pattern", spellcheck: "false" });
    text.hidden = kind !== "address";
    sel.addEventListener("change", () => { text.hidden = sel.value !== "address"; if (!text.hidden) text.focus(); });
    return { sel, text };
  }

  /// One rule card with its sentence editor. `rule` is null for a new one.
  function ruleCard(rule, onDone, draft) {
    const f = R.toForm(rule || draft || { owners: `/${ns()}/*`, peers: `/${ns()}/*`, admission: "allow", known: true, priority: 100 }, ns());
    const owners = picker(f.ownersKind, f.ownersCustom, "owners");
    const peers = picker(f.peersKind, f.peersCustom, "peers");
    const admission = select({ "aria-label": "Mail" }, el("option", { value: "allow" }, "accept messages from"), el("option", { value: "block" }, "block messages from"), el("option", { value: "" }, "keep their own setting for"));
    admission.value = f.admission;
    const known = el("input", { type: "checkbox", checked: f.known });
    const autonomy = select({ "aria-label": "Requests" }, el("option", { value: "" }, "their own setting"), el("option", { value: "review" }, "always ask a person"), el("option", { value: "auto" }, "act on their own for"));
    autonomy.value = f.autonomy;
    const verbs = input({ value: f.verbs, placeholder: "report_status, run_tests", "aria-label": "Actions", spellcheck: "false" });
    verbs.hidden = f.autonomy !== "auto";
    autonomy.addEventListener("change", () => { verbs.hidden = autonomy.value !== "auto"; });
    const locked = el("input", { type: "checkbox", checked: f.locked });
    const priority = input({ type: "number", min: 1, max: 1000, value: f.priority, "aria-label": "Priority", class: "pp-input pp-input-num" });
    const desc = input({ value: f.description, maxlength: 200, placeholder: "What this rule is for", "aria-label": "Description" });
    const status = statusLine();
    const read = () => ({ ownersKind: owners.sel.value, ownersCustom: owners.text.value, peersKind: peers.sel.value, peersCustom: peers.text.value,
      admission: admission.value, known: known.checked, autonomy: autonomy.value, verbs: verbs.value, locked: locked.checked, priority: priority.value, description: desc.value.trim() });
    const save = act(rule ? "Save" : "Add rule", async () => {
      const body = R.fromForm(read(), ns());
      if (body.error) return bad(status, body.error);
      await PPE.postbox(orgPath(rule ? `/rules/${encodeURIComponent(rule.id)}` : "/rules"), { method: rule ? "PATCH" : "POST", body });
      PPE.toast(rule ? "Rule saved" : "Rule added"); onDone();
    }, { size: "sm", kind: "primary" });
    const del = rule ? act("Delete", async () => {
      await PPE.postbox(orgPath(`/rules/${encodeURIComponent(rule.id)}`), { method: "DELETE" });
      PPE.toast("Rule deleted"); onDone();
    }, { size: "sm", kind: "quiet" }) : ui.button("Cancel", { size: "sm", kind: "quiet", onclick: () => card.remove() });
    const card = el("div", { class: "pp-card rule" },
      el("div", { class: "sentence" }, owners.sel, owners.text, " ", admission, " ", peers.sel, peers.text,
        check(known, "as a known contact"), " Requests: ", autonomy, verbs),
      el("div", { class: "meta" }, el("label", { class: "check" }, locked, " Locked (agents can't override)"), el("label", { class: "inline" }, "Priority ", priority), desc),
      rule ? el("p", { class: "note" }, R.sentence(rule, app.org.name, ns())) : null,
      status, actions(del, save));
    card._read = read;
    return card;
  }
  app.ruleCard = ruleCard;

  async function rules() {
    const { rules: list = [] } = await PPE.postbox(orgPath("/rules"));
    const wrap = el("div", { class: "rules" });
    const reload = () => go("rules");
    const add = ui.button("Add rule", { kind: "primary", icon: "plus", onclick: () => wrap.prepend(ruleCard(null, reload)) });
    const templates = el("div", { class: "templates" }, R.templates(ns()).map((t) => act(t.title, async () => {
      if (t.needsEdit) { wrap.prepend(ruleCard(null, reload, t.rules[0])); return; }
      for (const rule of t.rules) await PPE.postbox(orgPath("/rules"), { method: "POST", body: rule });
      PPE.toast("Template added"); reload();
    }, { size: "sm" })));
    for (const r of list.slice().sort((a, b) => a.priority - b.priority)) wrap.append(ruleCard(r, reload));
    return el("div", { class: "pp-view-inner" },
      ui.header({ title: "Rules", text: "Who can reach whom, for everyone at once. A person's own block always wins.", actions: [ui.button("Test a rule", { onclick: testSheet }), add] }),
      ui.card({ title: "Start from a template:" }, templates),
      list.length ? null : ui.empty({ icon: "rules", title: "No rules yet. Everyone uses their own settings." }),
      wrap);
  }

  function testSheet() {
    PPE.openSheet("Test a rule", (body) => {
      const from = input({ placeholder: "/acme/jane or any address", spellcheck: "false" });
      const to = input({ placeholder: `/${ns()}/agent1`, spellcheck: "false" });
      const verb = input({ placeholder: "run_tests (optional)", spellcheck: "false" });
      const out = el("div", { role: "status", "aria-live": "polite" });
      const run = act("Test", async () => {
        const r = await PPE.postbox(orgPath("/rules/evaluate"), { method: "POST", body: { from: from.value.trim(), to: to.value.trim(), ...(verb.value.trim() ? { verb: verb.value.trim() } : {}) } });
        out.replaceChildren(ui.card({ title: r.admitted ? "Delivered" : "Refused", tone: r.admitted ? null : "danger" },
          ui.kv([["Known contact", r.known ? "Yes" : "No"], ["Requests", r.autonomy === "auto" ? `Acted on without review: ${(r.verbs || []).join(", ") || "—"}` : "Wait for a person"],
            ["Decided by", { personal_block: "Their own block", personal_contact: "Their own contact settings", org_rule: "A company rule", inbox_policy: "Their inbox settings" }[r.decided_by] || r.decided_by || "—"]]),
          r.matched_rule ? el("p", { class: "note" }, `Rule: ${r.matched_rule.description || r.matched_rule.id}`) : null,
          r.explanation ? el("p", { class: "pp-muted" }, r.explanation) : null));
      }, { kind: "primary" });
      body.append(PPE.field("From", from), PPE.field("To", to), PPE.field("Request", verb), actions(run), out);
    });
  }

  // ---- Packages --------------------------------------------------------------------------------

  const subPath = (rest = "") => `/ent/orgs/${encodeURIComponent(app.org.id)}/subscription${rest}`;
  const INTERVAL = { month: "monthly", year: "yearly" };

  async function loadPlan() {
    const b = await PPE.postbox(orgPath("/billing"));
    // The card subscription (iyzico through saas_be). A granted plan has none.
    let sub = null;
    if (b.source === "iyzico") {
      try { sub = await PPE.bridge(subPath()); } catch (e) { if (e instanceof PPE.Redirecting) throw e; }
    }
    const manage = Boolean(sub && sub.can_manage && sub.buyer !== false);
    const canceled = Boolean(sub && (sub.status === "canceled" || sub.cancel_requested_at));
    return { b, sub, manage, canceled };
  }

  async function packages() {
    const { b, sub, manage, canceled } = await loadPlan();
    const m = /^ent-(\d+)-(monthly|yearly)-(usd|try|eur)$/.exec(b.plan_code || "") || [];
    const interval = m[2] || "yearly";
    const currency = (m[3] || (sub && sub.currency) || "usd").toUpperCase();
    const P = PPE.pricing;
    if (P && P.loadCatalog) await P.loadCatalog();
    const options = (sub && sub.options) || [];
    const change = manage && !canceled && options.length ? ui.button("Change plan", { kind: "primary", onclick: () => changePlanSheet(sub) }) : null;
    const tiles = (P ? P.stops() : cfg.seatStops || []).map((seats) => {
      const amount = P ? P.amount(seats, interval, currency) : null;
      const current = Number(b.seats) === seats;
      const opt = options.find((o) => o.seats === seats);
      return el("div", { class: `pp-tile${current ? " is-current" : ""}`, "data-seats": String(seats) },
        el("div", { class: "pp-tile-title" }, el("strong", {}, T("{1} seats", { 1: seats })), el("span", {}, P ? P.summary(seats) : "")),
        el("div", { class: "pp-tile-foot" },
          el("span", { class: "pp-price" }, amount ? `${PPE.money(amount, currency)} ${T(interval === "yearly" ? "per year" : "per month")}` : "—"),
          current ? ui.badge("Current plan", "info") : opt && manage && !canceled ? ui.button(opt.fits ? "Switch" : "Too small", { size: "sm", disabled: !opt.fits, onclick: () => changePlanSheet(sub, opt.plan_code) }) : null));
    });
    return el("div", { class: "pp-view-inner" },
      ui.header({ title: "Packages", text: "Your organization's package: seats, agent addresses and storage. Everyone in the company shares them.", actions: change }),
      ui.stats([
        { label: "Current package", value: T("{1} seats", { 1: b.seats }), hint: interval === "yearly" ? "Billed yearly" : "Billed monthly" },
        { label: "Seats used", value: `${b.seats_used} / ${b.seats}` },
        { label: "Agent addresses", value: `${b.agents_used} / ${b.agent_limit}` },
        { label: canceled ? "Ends" : "Renews", value: b.paid_through ? PPE.date(b.paid_through) : b.period_end ? PPE.date(b.period_end) : "—" },
      ]),
      !sub ? ui.gate({ tone: "info", text: b.source === "iyzico" ? "The payment details could not be loaded. Try again shortly." : "This plan isn't paid by card here. To change it, contact support@pigeonpost.dev." })
        : !manage ? ui.gate({ tone: "info", text: sub.can_manage ? "Only the person who bought this plan can change the card, the seats or cancel it." : "The owner and billing admins manage the plan." }) : null,
      ui.section({ title: "Every package", text: `Same billing period (${interval}) and currency (${currency}). Above 500 seats, talk to sales.` }, el("div", { class: "pp-tiles" }, tiles)),
      ui.card({ title: "Need more than 500 seats, or a different billing period?", text: "Our team sets up larger plans, yearly invoicing and other currencies." },
        actions(ui.button("Talk to sales", { href: "/?seats=500" }), ui.button("Email support", { href: `mailto:${SUPPORT_EMAIL}` }))));
  }

  function changePlanSheet(sub, wanted) {
    PPE.openSheet("Change plan", (body) => {
      const current = (sub.options || []).find((o) => o.current) || {};
      const cur = sub.currency || "USD";
      const per = INTERVAL[sub.interval] || "yearly";
      const prices = (cfg.prices && cfg.prices[cur] && cfg.prices[cur][per]) || {};
      const pick = select({ "aria-label": "Seats" }, (sub.options || []).map((o) => el("option", { value: o.plan_code, selected: wanted ? o.plan_code === wanted : o.current, disabled: !o.fits && !o.current },
        `${T("{1} seats", { 1: o.seats })}${prices[o.seats] ? ` — ${PPE.money(prices[o.seats], cur)} ${T(per === "yearly" ? "per year" : "per month")}` : ""}${o.current ? ` ${T("(current)")}` : !o.fits ? ` ${T("(too small)")}` : ""}`)));
      const explain = el("p", { class: "note", role: "status" });
      const save = act("Change plan", async () => {
        const r = await PPE.bridge(subPath("/change"), { method: "POST", body: { intent_id: newIntent(), plan_code: pick.value } });
        PPE.closeSheet();
        PPE.toast(r.effective === "now" ? "Plan changed" : "Seat change scheduled for the next renewal");
        await load(app.org.id); await PPE.shell.reload();
      }, { kind: "primary", disabled: true });
      const update = () => {
        const o = (sub.options || []).find((x) => x.plan_code === pick.value);
        save.disabled = !o || o.current || !o.fits;
        explain.textContent = !o || o.current ? "Choose a different number of seats."
          : !o.fits ? `The organization uses ${sub.seats_used} seats and ${sub.agents_used} agents. Remove people or agents first.`
          : o.when === "now" ? `More seats start now: iyzico charges the ${o.seats}-seat plan today, and it renews from today.`
          : `Fewer seats start at the next renewal${sub.next_payment_at ? ` (${PPE.date(sub.next_payment_at)})` : ""}. Until then you keep ${current.seats || sub.seats} seats.`;
      };
      pick.addEventListener("change", update);
      update();
      body.append(el("p", { class: "pp-muted" }, `Same billing period (${per}) and currency (${cur}). To switch either, contact support@pigeonpost.dev. Above 500 seats, talk to sales.`),
        PPE.field("Seats", pick), explain, actions(save));
    });
  }

  // ---- Billing ---------------------------------------------------------------------------------

  async function billing(params) {
    const [{ b, sub, manage, canceled }, invoices, profiles] = await Promise.all([
      loadPlan(),
      soft(PPE.bridge("/v1/billing/invoices").then((r) => r.invoices || [])),
      soft(PPE.bridge("/v1/billing/profiles").then((r) => r.profiles || [])),
    ]);
    const m = /^ent-(\d+)-(monthly|yearly)-(usd|try|eur)$/.exec(b.plan_code || "") || [];
    const interval = m[2] || "yearly";
    const tab = ["plan", "details", "credits", "invoices"].includes(params[0]) ? params[0] : "plan";
    const tabs = ui.tabs([
      { key: "plan", label: "Plan and payment" }, { key: "details", label: "Billing details" },
      { key: "credits", label: "Credits" }, { key: "invoices", label: "Invoices", count: (invoices || []).length || null },
    ], tab, (k) => PPE.go("billing", k));
    let body;
    if (tab === "details") body = billingDetails(profiles);
    else if (tab === "credits") body = credits();
    else if (tab === "invoices") body = invoiceTable(invoices);
    else {
      body = el("div", { class: "pp-cols" },
        ui.card({ title: `${b.seats} seats, ${interval}`, text: canceled && b.paid_through ? `Canceled. Paid through ${PPE.date(b.paid_through)}, then the organization stops.`
          : b.paid_through ? `Paid through ${PPE.date(b.paid_through)}` : b.period_end ? `Renews ${PPE.date(b.period_end)}` : null,
        actions: app.can("packages") ? ui.button("See packages", { size: "sm", href: PPE.href("packages") }) : null },
        sub && sub.pending_change ? el("p", { class: "note" }, "A seat change is scheduled for the next renewal.") : null,
        el("p", {}, `Seats: ${b.seats_used} of ${b.seats}`), meter(b.seats_used, b.seats, `${b.seats_used} of ${b.seats} seats used`),
        el("p", {}, `Agents: ${b.agents_used} of ${b.agent_limit}`), meter(b.agents_used, b.agent_limit, `${b.agents_used} of ${b.agent_limit} agents`)),
        paymentCard(b, sub, manage, canceled));
    }
    return el("div", { class: "pp-view-inner" },
      ui.header({ title: "Billing", text: "The plan, the card it is paid with, the details on your invoices and the invoices themselves.",
        actions: manage && !canceled && (sub.options || []).length ? ui.button("Change plan", { kind: "primary", onclick: () => changePlanSheet(sub) }) : null }),
      tabs, body);
  }

  function invoiceTable(invoices) {
    if (invoices === null) return ui.empty({ icon: "invoice", title: "Invoices could not be loaded", text: "Try again shortly." });
    return ui.table({
      rows: invoices, caption: "Invoices", empty: { icon: "invoice", title: "No invoices yet." },
      columns: [
        { key: "date", label: "Date", sort: (i) => String(i.issued_at || i.created_at || ""), render: (i) => String(i.issued_at || i.created_at || "").slice(0, 10) },
        { key: "number", label: "Number", mono: true, render: (i) => i.number || "—" },
        { key: "status", label: "Status", render: (i) => ui.status(String(i.status || "").toLowerCase(), { paid: ["Paid", "success"], open: ["Open", "warning"], unpaid: ["Unpaid", "warning"], overdue: ["Overdue", "danger"], void: ["Void", "neutral"] }) },
        { key: "total", label: "Total", num: true, render: (i) => (i.total != null ? PPE.money(i.total, i.currency || "USD") : "") },
        { key: "pdf", label: "", render: (i) => (i.id ? act("PDF", async () => { const r = await PPE.bridge(`/v1/billing/invoices/${encodeURIComponent(i.id)}/download`); if (/^https:\/\//.test(r.url || "")) window.open(r.url, "_blank", "noopener"); }, { size: "sm" }) : null) },
      ],
    });
  }

  function billingDetails(profiles) {
    if (profiles === null) return ui.empty({ icon: "billing", title: "Billing details could not be loaded", text: "Try again shortly." });
    if (!profiles.length) {
      return ui.empty({ icon: "billing", title: "No billing details yet", text: "Billing details are added when the organization's plan is bought. They go on every invoice.",
        action: { label: "Email support", href: `mailto:${SUPPORT_EMAIL}` } });
    }
    return el("div", { class: "pp-grid" }, profiles.map((p) => {
      const addr = p.address || {};
      return ui.card({ title: p.entity_name || p.legal_name || p.name || "Billing details", text: p.account_type === "individual" ? "Individual" : "Company" },
        ui.kv([
          ["Tax office", p.tax_office], ["Tax number (VKN)", p.tax_id], ["Invoice contact address", p.billing_email || p.email],
          ["Street address", [addr.line1, addr.line2].filter(Boolean).join(", ")], ["City", [addr.state, addr.city].filter(Boolean).join(", ")], ["Country", addr.country],
        ].filter(([, v]) => v)),
        el("p", { class: "hint" }, "To change the details on future invoices, contact support@pigeonpost.dev."));
    }));
  }

  /// Credits are an account's wallet (credits v1); organizations have none yet.
  function credits() {
    return ui.card({ title: "Credits", text: "Credits are kept in each person's own wallet and spent by them on marketplace services. Organizations don't have a shared wallet yet." },
      el("p", { class: "pp-muted" }, "Top up your own wallet from your account. Packs are $10, $25, $50 and $100; unspent credit can be refunded within 14 days and never expires."),
      actions(ui.button("Open your wallet", { href: `${ACCOUNT}#wallet`, icon: "wallet" })));
  }

  /// Card, retries and cancellation. Only the person who bought the plan can change it at iyzico.
  function paymentCard(b, sub, manage, canceled) {
    if (!sub) {
      return ui.card({ title: "Payment" }, el("p", { class: "pp-muted" }, b.source === "iyzico" ? "The payment details could not be loaded. Try again shortly."
        : "This plan isn't paid by card here. To change it, contact support@pigeonpost.dev."));
    }
    if (!manage) {
      return ui.card({ title: "Payment" }, el("p", { class: "pp-muted" }, sub.can_manage ? "Only the person who bought this plan can change the card, the seats or cancel it."
        : "The owner and billing admins manage the plan."));
    }
    const card = ui.button("Update card", { onclick: () => {
      // The iyzico form runs only on /checkout, the one page whose policy admits it.
      PPE.store.set("ppe_ent_card", JSON.stringify({ org_id: app.org.id, intent_id: newIntent() }));
      window.location.assign("/checkout");
    } });
    const retries = (sub.failed_orders || []).map((o) => el("div", { class: "setting" },
      el("p", {}, `Payment of ${PPE.money(o.amount, o.currency)} failed${o.failed_at ? ` on ${PPE.date(o.failed_at)}` : ""}.`),
      act("Try again", async () => {
        await PPE.bridge(subPath("/retry"), { method: "POST", body: { intent_id: newIntent(), order_reference_code: o.order_reference_code } });
        PPE.toast("Payment retried"); go("billing");
      }, { size: "sm" })));
    const cancel = act("Cancel plan", async () => {
      const until = b.paid_through ? PPE.date(b.paid_through) : "the end of the paid period";
      if (!(await ask(`Cancel the plan? Nothing renews. The organization keeps working until ${until}, then company mail stops.`, { confirm: "Cancel plan" }))) return;
      await PPE.bridge(subPath("/cancel"), { method: "POST", body: { intent_id: newIntent() } });
      PPE.toast("Plan canceled"); await load(app.org.id); await PPE.shell.reload();
    }, { kind: "danger" });
    return ui.card({ title: "Payment", text: "Your card details stay with iyzico; Pigeonpost never sees them." },
      ...retries,
      canceled ? el("p", { class: "note" }, "This plan is canceled. To keep the organization after the paid period, start a new plan from the packages page.") : actions(card, cancel));
  }

  // ---- SSH to an agent's workspace (Phase 8, SDS §11) -------------------------------------------
  // Supervision only: switching it on for the agent, approving its machine's host key, and
  // approving, denying or ending sessions. Opening a terminal is `pigeonpost ssh` (docs/agent-ssh.md):
  // the browser has no vetted SSH client, and the server never carries a shell's plaintext.

  async function sshState(a) {
    const enc = encodeURIComponent(a.address);
    const [status, sessions] = await Promise.all([
      PPE.postbox(`/v1/agents/${enc}/shell`).catch(() => null),
      PPE.postbox("/v1/shell/sessions").then((r) => (r.sessions || []).filter((x) => x.agent === a.address && x.state !== "closed")).catch(() => []),
    ]);
    return { status, sessions };
  }

  function sshSection(a, ssh) {
    const st = (ssh && ssh.status) || {};
    const org = st.org || {};
    const wrap = el("div", { class: "ssh" }, el("h3", {}, "Terminal (SSH)"));
    if (!(app.org.settings || {}).ssh_allowed) {
      wrap.append(el("p", { class: "pp-muted" }, "SSH to agents is off for this organization. An owner or admin can allow it under Security."));
      return wrap;
    }
    wrap.append(toggle("Allow SSH to this agent (people still need the “Open a terminal” permission)", Boolean(org.agent_enabled), async (v) => {
      await PPE.postbox(orgPath(`/agents/${encodeURIComponent(a.address)}/shell`), { method: "PUT", body: { enabled: v } });
    }));
    const facts = st.enabled
      ? `${st.connected ? "Machine connected" : "Machine not connected"} · ${st.mode} mode · host key ${st.host_key_fp}`
      : "The agent's machine has not enabled SSH in its agentd.toml.";
    wrap.append(el("p", { class: "pp-muted" }, facts));
    if (st.pending_host_key_fp) {
      wrap.append(el("div", { class: "setting" }, el("p", {}, "New host key waiting for approval", el("span", { class: "sub pp-muted pp-mono" }, ` ${st.pending_host_key_fp}`)),
        act("Approve new key", async () => {
          if (!(await ask(`The agent's machine presents a new host key:\n${st.pending_host_key_fp}\n\nApprove it only if you can confirm this fingerprint on the machine (pigeonpost agentd shell). Open sessions end.`, { confirm: "Approve new key", danger: false }))) return;
          await PPE.postbox(`/v1/agents/${encodeURIComponent(a.address)}/shell/host-key`, { method: "POST", body: { fingerprint: st.pending_host_key_fp } });
          PPE.toast("New host key approved"); PPE.closeSheet(); go("agents");
        }, { size: "sm" })));
    }
    for (const x of (ssh && ssh.sessions) || []) {
      const who = (x.requester && (x.requester.handle || x.requester.account)) || "someone";
      const row = el("div", { class: "setting" }, el("p", {}, who, el("span", { class: "sub pp-muted" }, ` · ${x.state.replace("_", " ")} · since ${PPE.date(x.created_at)}`)));
      const sessionAct = (label, path, method, msg, kind) => act(label, async () => {
        await PPE.postbox(`/v1/shell/sessions/${encodeURIComponent(x.id)}${path}`, { method, body: method === "POST" ? {} : undefined });
        PPE.toast(msg); PPE.closeSheet(); go("agents");
      }, { size: "sm", kind });
      if (x.state === "awaiting_approval") row.append(sessionAct("Approve", "/approve", "POST", "Approved"), sessionAct("Deny", "/deny", "POST", "Denied", "quiet"));
      else row.append(sessionAct("End", "", "DELETE", "Session ended", "danger"));
      wrap.append(row);
    }
    wrap.append(el("p", { class: "pp-muted" }, "People with permission connect from a terminal:"), secretBox(`pigeonpost ssh ${a.handle}`));
    return wrap;
  }

  // ---- Security --------------------------------------------------------------------------------

  function toggle(sentence, checked, onChange) {
    const box = el("input", { type: "checkbox", role: "switch", checked, "aria-label": sentence });
    box.addEventListener("change", async () => {
      box.disabled = true;
      try { await onChange(box.checked); PPE.toast("Saved"); } catch (e) { box.checked = !box.checked; PPE.fail(e); } finally { box.disabled = false; }
    });
    return el("div", { class: "setting" }, el("p", {}, sentence), el("span", { class: "switch" }, box));
  }
  const patchSettings = (settings) => PPE.postbox(orgPath(""), { method: "PATCH", body: { settings } }).then((r) => { if (r.org) app.org = r.org; });

  async function security() {
    const s = app.org.settings || {};
    const [{ domains = [] }, audit] = await Promise.all([PPE.postbox(orgPath("/domains")), PPE.postbox(orgPath("/audit?limit=50"))]);
    const add = ui.button("Add domain", { kind: "primary", icon: "plus", onclick: domainSheet });
    const domainCards = domains.map((d) => ui.card({ title: d.domain, actions: d.verified_at ? ui.badge("Verified", "success") : ui.badge("Not verified", "warning") },
      d.verified_at ? el("p", { class: "pp-muted" }, `Verified ${PPE.date(d.verified_at)}; checked again every week.`)
        : el("div", {}, el("p", {}, "Add this TXT record at your DNS provider, then press Verify:"), secretBox(`${d.txt_name || d.domain}  TXT  "${d.txt_value}"`)),
      actions(act("Remove", async () => { await PPE.postbox(orgPath(`/domains/${encodeURIComponent(d.domain)}`), { method: "DELETE" }); go("security"); }, { size: "sm", kind: "quiet" }),
        d.verified_at ? null : act("Verify", async () => {
          const r = await PPE.postbox(orgPath(`/domains/${encodeURIComponent(d.domain)}/verify`), { method: "POST", body: {} });
          PPE.toast(r.verified ? `${d.domain} is verified` : "The record isn't visible yet. DNS can take a while.", r.verified ? "info" : "error"); go("security");
        }, { size: "sm" }))));
    const filter = input({ type: "search", placeholder: "Filter by action, e.g. member.invite", "aria-label": "Filter audit log", class: "pp-input pp-search" });
    const tbody = el("tbody", {});
    let next = audit.next_before_id || null;
    const fill = (entries, append) => {
      if (!append) tbody.replaceChildren();
      for (const e of entries) {
        tbody.append(el("tr", {}, el("td", { "data-label": "When" }, new Date(e.at * 1000).toLocaleString(PPE.locale())), el("td", { "data-label": "Who" }, e.actor || e.actor_kind || "—"),
          el("td", { class: "pp-mono", "data-label": "Action" }, e.action), el("td", { class: "pp-mono", "data-label": "Target" }, e.target || "")));
      }
      if (!tbody.children.length) tbody.append(el("tr", {}, el("td", { colspan: "4", class: "pp-muted" }, "No entries match.")));
    };
    fill(audit.entries || []);
    const more = act("Load more", () => query(true), { size: "sm" });
    more.hidden = !next;
    const query = async (append) => {
      const q = new URLSearchParams({ limit: "50" });
      if (filter.value.trim()) q.set("action", filter.value.trim());
      if (append && next) q.set("before_id", String(next));
      const r = await PPE.postbox(orgPath(`/audit?${q}`));
      next = r.next_before_id || null; more.hidden = !next; fill(r.entries || [], append);
    };
    let t = null;
    filter.addEventListener("input", () => { window.clearTimeout(t); t = window.setTimeout(() => query(false).catch(PPE.fail), 300); });
    const exportBtn = (format) => act(`Export ${format.toUpperCase()}`, () => exportAudit(format), { size: "sm" });
    return el("div", { class: "pp-view-inner" },
      ui.header({ title: "Security", text: "Sign-in policy, verified domains and the audit log.", actions: add }),
      ui.card({ title: "Policy" },
        toggle("Require two-step sign-in for everyone in the organization", Boolean(s.require_mfa), (v) => patchSettings({ require_mfa: v })),
        toggle("Only invite addresses at verified domains", Boolean(s.invite_domains_only), (v) => patchSettings({ invite_domains_only: v })),
        sshOn() ? toggle("Allow SSH to agents (each agent and each person still needs its own permission)", Boolean(s.ssh_allowed), (v) => patchSettings({ ssh_allowed: v })) : null,
        sshOn() ? toggle("Every SSH session needs an owner's, admin's or agent admin's approval", Boolean(s.ssh_requires_approval), (v) => patchSettings({ ssh_requires_approval: v })) : null,
        streamsOn() ? viewerLimit(s) : null),
      ui.section({ title: "Domains" }, domains.length ? el("div", { class: "pp-grid" }, domainCards) : el("p", { class: "pp-muted" }, "No domains yet. Verify your company's domain to restrict invitations to it.")),
      app.can("api-keys") ? ui.card({ title: "API keys and webhooks", text: "Scoped keys let your own systems manage people, agents, rules and rooms. Webhooks tell them when something changes. Neither can read messages." },
        actions(ui.button("Manage API keys", { size: "sm", href: PPE.href("api-keys") }), ui.button("Manage webhooks", { size: "sm", href: PPE.href("webhooks") }))) : null,
      ui.section({ title: "Audit log", actions: [exportBtn("csv"), exportBtn("json")] },
        el("label", { class: "pp-search-wrap" }, PPE.icon("search"), filter),
        el("div", { class: "pp-table-host" }, el("table", { class: "pp-table pp-table-dense" }, el("thead", {}, el("tr", {}, ["When", "Who", "Action", "Target"].map((h) => el("th", { scope: "col" }, h)))), tbody)),
        actions(more)));
  }

  // Viewers per live stream in this organization's rooms (SDS §14A.5). Blank = the server's limit;
  // the server refuses anything above it.
  function viewerLimit(s) {
    const box = input({ type: "number", min: "1", step: "1", inputmode: "numeric", placeholder: "Server default", value: s.stream_max_viewers ? String(s.stream_max_viewers) : "" });
    const save = act("Save", async () => {
      const raw = box.value.trim();
      const n = raw === "" ? 0 : Number(raw);
      if (!Number.isInteger(n) || n < 0) return PPE.toast("Enter a whole number, or leave it empty for the default.", "error");
      await patchSettings({ stream_max_viewers: n });
      PPE.toast("Saved.");
    }, { size: "sm" });
    return PPE.field("Viewers per live stream", el("div", { class: "row" }, box, save), "Leave empty to use the server's limit.");
  }

  function domainSheet() {
    PPE.openSheet("Add domain", (body) => {
      const d = input({ placeholder: "acme.com", spellcheck: "false", autocapitalize: "none" });
      body.append(PPE.field("Domain", d, "You'll add one TXT record to prove you control it."), actions(act("Add domain", async () => {
        const v = d.value.trim().toLowerCase().replace(/\.$/, "");
        if (!/^(?=.{4,253}$)([a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]{2,63}$/.test(v)) return PPE.toast("Enter a domain like acme.com.", "error");
        await PPE.postbox(orgPath("/domains"), { method: "POST", body: { domain: v } });
        PPE.closeSheet(); go(PPE.parseRoute(window.location.hash).page || "security");
      }, { kind: "primary" })));
    });
  }

  PPE.onResume("app.export", async (p) => exportAudit(p.format));
  async function exportAudit(format) {
    const res = await PPE.postbox(orgPath(`/audit/export?format=${format}`), { raw: true, resume: { name: "app.export", payload: { format } } });
    const blob = await res.blob();
    const a = el("a", { href: URL.createObjectURL(blob), download: `audit-${app.org.namespace}-${new Date().toISOString().slice(0, 10)}.${format}` });
    document.body.append(a); a.click(); a.remove();
    window.setTimeout(() => URL.revokeObjectURL(a.href), 2000);
  }

  // ---- API keys (Phase 5, docs/org-api.md §1) --------------------------------------------------

  const SCOPES = [
    ["org:read", "Read the organization, people, agents, rules, rooms, usage and billing status."],
    ["members:write", "Invite, rename, suspend and remove ordinary members (never owners or admins)."],
    ["agents:write", "Create, delete and disconnect agents."],
    ["rules:write", "Add restrictive rules (block or review) and change the ones keys created."],
    ["rooms:write", "Create, update and delete rooms, and invite company mailboxes to them."],
    ["messages:send", "Send direct messages to people and agents here, as one agent you choose."],
    ["audit:read", "Read the audit log."],
    ["webhooks:write", "Create, rotate and delete webhooks."],
  ];
  const EVENTS = [
    ["member.invited", "Someone was invited"], ["member.joined", "Someone joined"], ["member.suspended", "Someone was suspended"],
    ["member.removed", "Someone was removed"], ["agent.created", "An agent was added"], ["agent.connected", "An agent logged in on a machine"],
    ["agent.disconnected", "An agent was disconnected"], ["room.created", "A room was created"], ["room.deleted", "A room was deleted"],
    ["billing.status_changed", "The billing status changed"],
  ];
  const WEBHOOK_DOCS = "https://developers.pigeonpost.dev/org-api-webhooks";

  /// Loose client-side check of one allowlist entry; the postbox decides.
  app.looksLikeCidr = (v) => /^(\d{1,3}\.){3}\d{1,3}(\/\d{1,2})?$/.test(v) || (/^[0-9a-f:.]+(\/\d{1,3})?$/i.test(v) && v.includes(":"));

  /// The status a key is in, at `now` (Unix seconds).
  app.keyState = (k, now = Date.now() / 1000) => (k.revoked_at ? "revoked" : k.expires_at && k.expires_at <= now ? "expired" : "active");

  /// Show a secret exactly once. When the sheet closes, the text leaves the page and the only
  /// reference to it is dropped; `after` then runs (usually a list refresh).
  function secretSheet(title, secret, notes, after) {
    const held = { value: secret };
    const code = el("code", { "aria-label": "Secret" }, held.value);
    PPE.openSheet(title, (body) => body.append(
      el("p", {}, el("strong", {}, "Copy it now. You won't see it again."), " Pigeonpost keeps only a fingerprint of it."),
      el("div", { class: "secret" }, code, ui.button("Copy", { size: "sm", onclick: () => { if (held.value) PPE.copy(held.value); } })),
      ...(notes || []).filter(Boolean).map((n) => el("p", { class: "hint" }, n)),
      actions(ui.button("I've stored it", { kind: "primary", onclick: () => PPE.closeSheet() }))),
    { onClose: () => { held.value = null; code.textContent = ""; if (after) after(); } });
  }
  app.secretSheet = secretSheet;

  async function apiKeys() {
    const { keys = [] } = await PPE.postbox(orgPath("/api-keys"));
    const create = ui.button("Create key", { kind: "primary", icon: "plus", onclick: () => keySheet() });
    const now = Date.now() / 1000;
    return el("div", { class: "pp-view-inner" },
      ui.header({ title: "API keys", text: "Keys let your own systems manage this organization. A key never reads messages, and it stops working if the person who made it loses their admin role.", actions: create }),
      ui.table({
        rows: keys, caption: "API keys", empty: { icon: "key", title: "No keys yet. Create one for each system that needs access." },
        columns: [
          { key: "name", label: "Name", sort: true, render: (k) => el("div", {}, k.name, el("span", { class: "sub" }, k.created_by ? `by ${k.created_by}` : "")) },
          { key: "scopes", label: "Scopes", render: (k) => el("div", { class: "chips" }, (k.scopes || []).map((s) => ui.badge(s)), k.send_as ? el("span", { class: "sub pp-mono" }, `sends as ${k.send_as}`) : null,
            (k.ip_allowlist || []).length ? el("span", { class: "sub" }, `${k.ip_allowlist.length} allowed network${k.ip_allowlist.length === 1 ? "" : "s"}`) : null) },
          { key: "last4", label: "Key", mono: true, render: (k) => `pk_org_…${k.last4 || ""}` },
          { key: "created_at", label: "Created", sort: true, render: (k) => PPE.date(k.created_at) },
          { key: "expires_at", label: "Expires", render: (k) => (k.expires_at ? PPE.date(k.expires_at) : "Never") },
          { key: "last_used_at", label: "Last used", render: (k) => (k.last_used_at ? PPE.ago(k.last_used_at) : "Never") },
          { key: "state", label: "Status", render: (k) => { const s = app.keyState(k, now); return ui.badge({ active: "Active", expired: "Expired", revoked: "Revoked" }[s], { active: "success", expired: "warning", revoked: "danger" }[s]); } },
          { key: "act", label: "", render: (k) => {
            if (app.keyState(k, now) === "revoked") return null;
            const b = act("Revoke", async () => {
              if (!(await ask(`Revoke ${k.name}? Anything using it stops working at once. This can't be undone.`, { confirm: "Revoke" }))) return;
              await PPE.postbox(orgPath(`/api-keys/${encodeURIComponent(k.id)}`), { method: "DELETE" });
              PPE.toast("Key revoked"); go("api-keys");
            }, { size: "sm", kind: "danger" });
            b.setAttribute("aria-label", `Revoke ${k.name}`);
            return b;
          } },
        ],
      }),
      el("p", { class: "note" }, "Creating a key needs a fresh, strong sign-in. Each key is shown once; if it's lost, revoke it and create another."));
  }

  async function keySheet() {
    let agentList = [];
    try { agentList = ((await PPE.postbox(orgPath("/agents"))).agents || []).filter((a) => a.status === "active"); } catch (e) { if (e instanceof PPE.Redirecting) throw e; }
    PPE.openSheet("Create key", (body) => {
      const name = input({ maxlength: 80, placeholder: "HR sync", autocomplete: "off" });
      const scopes = SCOPES.map(([s, text]) => check(el("input", { type: "checkbox", value: s }), el("strong", { class: "pp-mono" }, s), el("span", { class: "sub pp-muted" }, text)));
      const sendAs = select({}, el("option", { value: "" }, agentList.length ? "Choose an agent…" : "No active agents"),
        agentList.map((a) => el("option", { value: a.handle }, a.label ? `${a.handle} (${a.label})` : a.handle)));
      const sendAsField = PPE.field("Send messages as", sendAs, "Messages this key sends come from this agent's address. Only direct messages to people and agents here.");
      const sendBox = scopes[SCOPES.findIndex(([s]) => s === "messages:send")].querySelector("input");
      sendAsField.hidden = true;
      sendBox.addEventListener("change", () => { sendAsField.hidden = !sendBox.checked; });
      const days = input({ type: "number", min: 1, max: 730, step: 1, inputmode: "numeric", placeholder: "Never" });
      const allow = textarea({ spellcheck: "false", autocapitalize: "none", placeholder: "203.0.113.0/24\n2001:db8::/32" });
      const status = statusLine();
      const make = act("Create key", async () => {
        const chosen = scopes.map((l) => l.querySelector("input")).filter((c) => c.checked).map((c) => c.value);
        const ips = allow.value.split(/\r?\n/).map((l) => l.trim()).filter(Boolean);
        if (!name.value.trim()) return bad(status, "Give the key a name, so people know what uses it.");
        if (!chosen.length) return bad(status, "Choose at least one scope.");
        if (chosen.includes("messages:send") && !sendAs.value) return bad(status, "Choose the agent this key sends messages as.");
        if (days.value.trim() && !(/^\d+$/.test(days.value.trim()) && Number(days.value) >= 1 && Number(days.value) <= 730)) return bad(status, "Expiry is a whole number of days, from 1 to 730.");
        if (ips.length > 20) return bad(status, "At most 20 allowed networks.");
        const wrong = ips.find((v) => !app.looksLikeCidr(v));
        if (wrong) return bad(status, `${wrong} doesn't look like an address or network (for example 203.0.113.0/24).`);
        const req = { name: name.value.trim(), scopes: chosen,
          ...(days.value.trim() ? { expires_in_days: Number(days.value) } : {}),
          ...(ips.length ? { ip_allowlist: ips } : {}),
          ...(chosen.includes("messages:send") ? { send_as: sendAs.value } : {}) };
        status.className = "status-line"; status.textContent = "";
        try {
          await createKey(req);
        } catch (e) {
          if (e && e.code === "step_up_unavailable") return bad(status, "Strong sign-in isn't configured on this server, so keys can't be created.");
          throw e;
        }
      }, { kind: "primary" });
      body.append(PPE.field("Name", name),
        el("fieldset", { class: "pp-card" }, el("legend", {}, "Scopes: what the key may do"), ...scopes),
        sendAsField,
        PPE.field("Expires after (days, optional)", days, "From 1 to 730. Leave empty for a key that doesn't expire."),
        PPE.field("Allowed networks (optional)", allow, "One address or CIDR range per line, at most 20. Requests from anywhere else are refused."),
        el("p", { class: "hint" }, "You'll be asked to sign in again with a strong method before the key is made."),
        status, actions(ui.button("Cancel", { onclick: () => PPE.closeSheet() }), make));
    });
  }

  /// After a step-up round trip, the same request is sent again (nothing secret is kept across it).
  PPE.onResume("app.apikey", async (req) => {
    try { await createKey(req); } catch (e) {
      if (e && e.code === "step_up_unavailable") return PPE.toast("Strong sign-in isn't configured on this server, so keys can't be created.", "error");
      throw e;
    }
  });
  async function createKey(req) {
    const r = await PPE.postbox(orgPath("/api-keys"), { method: "POST", body: req, resume: { name: "app.apikey", payload: req } });
    const k = r.key || {};
    secretSheet(`Key "${k.name || req.name}" created`, r.secret,
      ["Use it as: Authorization: Bearer <key>, against https://postbox.pigeonpost.dev/org/v1.",
        k.expires_at ? `It expires on ${PPE.date(k.expires_at)}.` : null],
      () => go("api-keys"));
  }

  // ---- Webhooks (Phase 5, docs/org-api.md §6) --------------------------------------------------

  const DELIVERY = { delivered: ["Delivered", "success"], retrying: ["Retrying", "warning"], failed: ["Failed", "danger"], blocked: ["Blocked", "danger"] };
  const deliveryChip = (status) => ui.status(status, DELIVERY);
  const eventsText = (events) => ((events || []).includes("*") ? "All events" : (events || []).join(", "));

  async function webhooks() {
    const { webhooks: list = [] } = await PPE.postbox(orgPath("/webhooks"));
    const add = ui.button("Add webhook", { kind: "primary", icon: "plus", onclick: () => webhookSheet(), disabled: list.length >= 10 });
    return el("div", { class: "pp-view-inner" },
      ui.header({ title: "Webhooks", text: "Pigeonpost calls these addresses when people, agents, rooms or billing change. Deliveries carry ids and addresses, never message content.", actions: add }),
      ui.table({
        rows: list, caption: "Webhooks", empty: { icon: "webhook", title: "No webhooks yet." },
        columns: [
          { key: "url", label: "Endpoint", render: (w) => el("div", {}, el("span", { class: "pp-mono" }, w.url), w.description ? el("span", { class: "sub" }, w.description) : null) },
          { key: "events", label: "Events", render: (w) => eventsText(w.events) },
          { key: "last", label: "Last delivery", render: (w) => { const d = w.last_delivery; return d ? el("div", {}, deliveryChip(d.status), el("span", { class: "sub" }, `${d.response_status ? `HTTP ${d.response_status} · ` : ""}${PPE.ago(d.at)}`)) : el("span", { class: "pp-muted" }, "Nothing sent yet"); } },
          { key: "act", label: "", render: (w) => { const b = ui.button("Manage", { size: "sm", onclick: () => manageWebhook(w) }); b.setAttribute("aria-label", `Manage ${w.url}`); return b; } },
        ],
      }),
      el("p", { class: "note" }, "Every delivery is signed with HMAC-SHA256 in the Pigeonpost-Signature header. Check it with your signing secret, reject old timestamps and ignore repeated event ids. ",
        el("a", { href: WEBHOOK_DOCS, rel: "noopener" }, "How to verify signatures")));
  }

  function webhookSheet() {
    PPE.openSheet("Add webhook", (body) => {
      const url = input({ type: "url", maxlength: 2048, spellcheck: "false", autocapitalize: "none", placeholder: "https://hooks.example.com/pigeonpost" });
      const all = el("input", { type: "checkbox", value: "*" });
      const boxes = EVENTS.map(([ev, text]) => check(el("input", { type: "checkbox", value: ev }), el("span", { class: "pp-mono" }, ev), el("span", { class: "sub pp-muted" }, text)));
      all.addEventListener("change", () => { for (const b of boxes) { const c = b.querySelector("input"); c.disabled = all.checked; } });
      const desc = input({ maxlength: 200, placeholder: "HR system" });
      const status = statusLine();
      const save = act("Add webhook", async () => {
        const v = url.value.trim();
        let ok = false;
        try { const u = new URL(v); ok = u.protocol === "https:" && !u.username && !u.password && !u.hash; } catch { ok = false; }
        if (!ok) return bad(status, "Use an https:// address without a user name or #fragment.");
        const events = all.checked ? ["*"] : boxes.map((b) => b.querySelector("input")).filter((c) => c.checked).map((c) => c.value);
        if (!events.length) return bad(status, "Choose at least one event, or All events.");
        const r = await PPE.postbox(orgPath("/webhooks"), { method: "POST", body: { url: v, events, ...(desc.value.trim() ? { description: desc.value.trim() } : {}) } });
        secretSheet("Webhook signing secret", r.secret, [`Deliveries to ${(r.webhook && r.webhook.url) || v} are signed with it.`], () => go("webhooks"));
      }, { kind: "primary" });
      body.append(PPE.field("Endpoint address", url, "Must be https:// on port 443 or 8443, and reachable on the public internet."),
        el("fieldset", { class: "pp-card" }, el("legend", {}, "Events"),
          check(all, el("strong", {}, "All events"), el("span", { class: "sub pp-muted" }, "Including ones added later.")), ...boxes),
        PPE.field("Description (optional)", desc),
        status, actions(ui.button("Cancel", { onclick: () => PPE.closeSheet() }), save));
    });
  }

  function manageWebhook(w) {
    PPE.openSheet(w.description || "Webhook", (body) => {
      body.append(ui.kv([
        ["Endpoint", el("span", { class: "pp-mono" }, w.url)],
        ["Events", eventsText(w.events)],
        ["Created", `${PPE.date(w.created_at)}${w.created_by ? ` by ${w.created_by}` : ""}`],
        ["Secret rotated", w.rotated_at ? PPE.date(w.rotated_at) : "Never"]]),
      w.previous_secret_expires_at && w.previous_secret_expires_at > Date.now() / 1000
        ? el("p", { class: "note" }, `The previous secret also signs deliveries until ${new Date(w.previous_secret_expires_at * 1000).toLocaleString(PPE.locale())}.`) : null);
      body.append(actions(
        act("Recent deliveries", () => deliveriesSheet(w)),
        act("Rotate secret", async () => {
          if (!(await ask("Make a new signing secret? The current one keeps signing deliveries for 24 hours, so you can switch over.", { confirm: "Rotate secret", danger: false }))) return;
          const r = await PPE.postbox(orgPath(`/webhooks/${encodeURIComponent(w.id)}/rotate`), { method: "POST", body: {} });
          const until = r.previous_secret_expires_at ? new Date(r.previous_secret_expires_at * 1000).toLocaleString(PPE.locale()) : "24 hours from now";
          secretSheet("New signing secret", r.secret, [`The old secret keeps working for 24 hours, until ${until}. Deliveries carry both signatures until then.`], () => go("webhooks"));
        }),
        act("Delete webhook", async () => {
          if (!(await ask(`Delete the webhook for ${w.url}? Pending deliveries are cancelled.`, { confirm: "Delete webhook" }))) return;
          await PPE.postbox(orgPath(`/webhooks/${encodeURIComponent(w.id)}`), { method: "DELETE" });
          PPE.closeSheet(); PPE.toast("Webhook deleted"); go("webhooks");
        }, { kind: "danger" })));
    });
  }

  async function deliveriesSheet(w) {
    const { deliveries = [] } = await PPE.postbox(orgPath(`/webhooks/${encodeURIComponent(w.id)}/deliveries?limit=50`));
    PPE.openSheet("Recent deliveries", (body) => {
      body.append(el("p", { class: "pp-muted pp-mono" }, w.url));
      if (!deliveries.length) body.append(el("p", { class: "pp-muted" }, "Nothing has been sent yet."));
      const list = el("ul", { class: "deliveries" }, deliveries.map((d) => el("li", { class: "setting" },
        el("p", {}, el("span", { class: "pp-mono" }, d.event_type), " ", deliveryChip(d.status),
          el("span", { class: "sub pp-muted" }, [new Date(d.at * 1000).toLocaleString(PPE.locale()), `attempt ${d.attempt}`, d.response_status ? `HTTP ${d.response_status}` : "no answer",
            d.duration_ms != null ? `${d.duration_ms} ms` : null].filter(Boolean).join(" · ")),
          d.error ? el("span", { class: "sub" }, d.error) : null))));
      body.append(list, actions(ui.button("Back", { onclick: () => manageWebhook(w) })));
    });
  }

  // ---- Organization ----------------------------------------------------------------------------

  async function organization() {
    const o = app.org;
    const name = input({ maxlength: 120, value: o.name });
    const save = act("Save", async () => {
      const r = await PPE.postbox(orgPath(""), { method: "PATCH", body: { name: name.value.trim() } });
      if (r.org) app.org = r.org;
      PPE.toast("Saved"); await PPE.shell.reload();
    }, { kind: "primary" });
    const rc = select({ "aria-label": "Who can create rooms" }, el("option", { value: "everyone" }, "everyone"), el("option", { value: "admins" }, "admins only"));
    rc.value = (o.settings && o.settings.rooms_creation) || "everyone";
    rc.addEventListener("change", async () => { try { await patchSettings({ rooms_creation: rc.value }); PPE.toast("Saved"); } catch (e) { PPE.fail(e); } });
    const parts = [ui.header({ title: "Organization", text: "The company's name, address and room settings.", actions: save }),
      el("div", { class: "pp-cols" },
        ui.card({ title: "Profile" }, PPE.field("Name", name),
          ui.kv([["Address", el("span", { class: "pp-mono", translate: "no" }, `/${o.namespace}`)], ["Status", statusBadge(o.status)], ["Created", PPE.date(o.created_at)], ["Organization id", ui.copyable(o.id)]]),
          el("p", { class: "hint" }, "The address can't change after creation.")),
        ui.card({ title: "Rooms" }, el("div", { class: "setting" }, el("p", {}, "Rooms can be created by "), rc)))];
    if (has("owner")) {
      const confirm = input({ spellcheck: "false", autocapitalize: "none", "aria-label": `Type ${o.namespace} to confirm` });
      const del = act("Delete organization", () => deleteOrg(confirm.value.trim()), { kind: "danger", disabled: true });
      confirm.addEventListener("input", () => { del.disabled = confirm.value.trim() !== o.namespace; });
      parts.push(ui.card({ title: "Delete organization", tone: "danger" },
        el("p", {}, "Mail to every company address stops at once. People keep their personal accounts; their company mailboxes are removed. Agents are disconnected. A card plan is canceled first, so nothing renews. The address can be recovered for 30 days."),
        PPE.field(`Type ${o.namespace} to confirm`, confirm), actions(del)));
    }
    return el("div", { class: "pp-view-inner" }, ...parts);
  }

  PPE.onResume("app.delete", async (p) => deleteOrg(p.confirm));
  async function deleteOrg(confirmNamespace) {
    // Stop the card subscription first, so a deleted organization is never charged again.
    let sub = null;
    try { sub = await PPE.bridge(subPath()); } catch (e) { if (e instanceof PPE.Redirecting) throw e; }
    if (sub && sub.status !== "canceled" && !sub.cancel_requested_at && sub.buyer !== false) {
      await PPE.bridge(subPath("/cancel"), { method: "POST", body: { intent_id: newIntent() } });
    } else if (sub && sub.status !== "canceled" && !sub.cancel_requested_at
      && !(await ask("The plan was bought by someone else and keeps renewing until they cancel it. Delete anyway?", { confirm: "Delete organization" }))) {
      return;
    }
    await PPE.postbox(orgPath(""), { method: "DELETE", body: { confirm_namespace: confirmNamespace }, resume: { name: "app.delete", payload: { confirm: confirmNamespace } } });
    PPE.store.del("ppe_org");
    const main = document.getElementById("pp-main");
    if (main) {
      main.replaceChildren(el("div", { class: "pp-view" }, ui.empty({ icon: "building", title: "Organization deleted", text: "Company mail has stopped. The address can be recovered for 30 days.",
        action: { label: "Done", href: "/" } })));
    }
  }

  // ---- Support ---------------------------------------------------------------------------------

  async function support() {
    const o = app.org;
    const subject = encodeURIComponent(o ? `Support: ${o.name} (/${o.namespace})` : "Support");
    return el("div", { class: "pp-view-inner" },
      ui.header({ title: "Support", text: "Help with your organization, billing or the apps." }),
      el("div", { class: "pp-grid" },
        ui.card({ title: "Help centre", text: "Guides for admins: inviting people, agents, rules, rooms, billing and security." },
          actions(ui.button("Open the help centre", { kind: "primary", href: HELP, icon: "external" }))),
        ui.card({ title: "Write to support", text: "We answer within one business day. Include your organization's address so we can find it." },
          o ? ui.kv([["Organization", el("span", { translate: "no" }, o.name)], ["Address", el("span", { class: "pp-mono", translate: "no" }, `/${o.namespace}`)], ["Organization id", ui.copyable(o.id)]]) : null,
          actions(ui.button(`Email ${SUPPORT_EMAIL}`, { href: `mailto:${SUPPORT_EMAIL}?subject=${subject}`, icon: "support" }))),
        ui.card({ title: "Larger teams and contracts", text: "More than 500 seats, yearly invoicing, a data processing agreement or a security review." },
          actions(ui.button("Talk to sales", { href: "/?seats=500" }))),
        ui.card({ title: "For developers", text: "The organization API, webhooks and the command line." },
          actions(ui.button("Developer docs", { href: "https://developers.pigeonpost.dev/org-api", icon: "docs" }), ui.button("System status", { href: "https://status.pigeonpost.dev", icon: "server" })))),
      el("p", { class: "note" }, "Pigeonpost staff never ask for your password or sign-in codes, and admins can never read anyone's messages."));
  }

  // ---- the shell -------------------------------------------------------------------------------

  const PAGES = [
    { key: "overview", title: "Overview", group: "", icon: "home", fn: overview },
    { key: "setup", title: "Setup", group: "", icon: "flag", fn: setup, always: true },
    { key: "people", title: "People", group: "Workspace", icon: "people", fn: people },
    { key: "agents", title: "Agents", group: "Workspace", icon: "agents", fn: agents },
    { key: "rooms", title: "Rooms", group: "Workspace", icon: "rooms", fn: rooms },
    { key: "nests", title: "Nests", group: "Nests & services", icon: "nest", fn: nests },
    { key: "services", title: "Services", group: "Nests & services", icon: "store", fn: services },
    { key: "rules", title: "Rules", group: "Policy", icon: "rules", fn: rules },
    { key: "security", title: "Security", group: "Policy", icon: "security", fn: security },
    { key: "packages", title: "Packages", group: "Company", icon: "plan", fn: packages },
    { key: "billing", title: "Billing", group: "Company", icon: "billing", fn: billing },
    { key: "organization", title: "Organization", group: "Company", icon: "building", fn: organization },
    { key: "api-keys", title: "API keys", group: "Developers", icon: "key", fn: apiKeys },
    { key: "webhooks", title: "Webhooks", group: "Developers", icon: "webhook", fn: webhooks },
    { key: "support", title: "Support", group: "Help", icon: "support", fn: support, always: true },
  ];
  app.PAGES = PAGES;

  async function load(id) {
    const r = await PPE.postbox(`/v1/orgs/${encodeURIComponent(id)}`);
    app.org = r.org;
    app.me = r.me || {};
    app.members = null;
    PPE.store.set("ppe_org", id);
  }

  /// A read for the platform switcher only: plain fetch, so a refusal never signs anyone out.
  async function probe(path) {
    try {
      const res = await window.fetch(cfg.postbox + path, { headers: { accept: "application/json", authorization: `Bearer ${PPE.token()}` } });
      return res.ok ? await res.json() : null;
    } catch { return null; }
  }
  async function platforms() {
    const out = ["account", "enterprise"];
    const [pubs, staff] = await Promise.all([probe("/v1/publishers"), probe("/v1/staff/me")]);
    const list = pubs && (Array.isArray(pubs.data) ? pubs.data : Array.isArray(pubs.publishers) ? pubs.publishers : []);
    if (list && list.length) out.push("partner");
    const roles = staff && ((staff.data && staff.data.roles) || staff.roles);
    if (Array.isArray(roles) && roles.length) out.push("management");
    return out;
  }

  /// The signed-in person as they should be named: the realm's display name, else their name on
  /// the member list, else their username. Never a placeholder.
  async function person() {
    const c = PPE.claims(PPE.token());
    let name = c.name || [c.given_name, c.family_name].filter(Boolean).join(" ");
    if (!name && app.me && app.me.member_id && app.can("people")) {
      const members = app.members || (await soft(PPE.postbox(orgPath("/members")).then((r) => r.members || []))) || [];
      if (members.length) app.members = members;
      const me = members.find((m) => m.id === app.me.member_id);
      if (me) name = fullName(me);
    }
    const mailbox = app.me && app.me.mailbox && app.me.mailbox.handle;
    return { name: name || c.preferred_username || "", username: mailbox || c.preferred_username || "", email: c.email || "" };
  }

  async function context() {
    if (window.PPE_I18N && window.PPE_I18N.addSwitch) window.PPE_I18N.addSwitch();
    const probing = platforms();
    const caps = await PPE.postbox("/v1/capabilities");
    app.features = (Array.isArray(caps.features) ? caps.features : Object.keys(caps.features || {})).filter((f) => !app.disabled.has(f));
    app.state = null; app.org = null; app.me = null;
    if (!app.features.includes("enterprise_portal") || !app.features.includes("orgs")) {
      app.state = "soon";
    } else {
      const { orgs = [] } = await PPE.postbox("/v1/orgs");
      app.orgs = orgs;
      const admins = orgs.filter((o) => (o.my_roles || []).some((r) => ADMIN.includes(r)));
      if (!admins.length) {
        app.state = orgs.length ? "member" : "none";
      } else {
        const q = new URLSearchParams(window.location.search);
        const wanted = q.get("org") || PPE.store.get("ppe_org");
        const pick = admins.find((o) => o.id === wanted) || admins[0];
        await load(pick.id);
        app.state = "admin";
      }
    }
    const ctx = { state: app.state, org: app.org, features: app.features, user: await person(), platforms: await probing };
    app.ctx = ctx;
    return ctx;
  }

  /// Above the navigation: which organization this is, and a picker when there are several.
  function orgHeader() {
    if (!app.org) return null;
    const admins = app.orgs.filter((o) => (o.my_roles || []).some((r) => ADMIN.includes(r)));
    const card = el("div", { class: "org-card", translate: "no" },
      el("span", { class: "org-mark", "aria-hidden": "true" }, (app.org.name || "?").trim().charAt(0).toUpperCase() || "?"),
      el("div", { class: "org-text" }, el("strong", {}, app.org.name), el("span", { class: "pp-mono" }, `/${app.org.namespace}`)));
    if (admins.length < 2) return card;
    const pick = select({ "aria-label": "Organization", class: "pp-input org-pick" }, admins.map((o) => el("option", { value: o.id, selected: o.id === app.org.id }, o.name)));
    pick.addEventListener("change", async () => { PPE.store.set("ppe_org", pick.value); await PPE.shell.reload(); });
    return el("div", {}, card, pick);
  }

  // Old links: /app/<page>?org=… becomes /app?org=…#/<page>.
  const legacy = /^\/app\/([a-z-]+)\/?$/.exec(window.location.pathname);
  if (legacy && !window.location.hash) window.history.replaceState(null, "", `/app${window.location.search}#/${legacy[1]}`);
  else if (legacy) window.history.replaceState(null, "", `/app${window.location.search}${window.location.hash}`);

  app.ready = PPE.shell.start({
    platform: "enterprise",
    title: "Enterprise",
    themePosition: "left",
    intro: {
      heading: "Sign in to manage your organization",
      text: "Owners and admins manage people, agents and rooms here.",
      points: ["Invite people and give them company addresses", "Add agents and set who can reach whom", "Buy packages and Nests, and download invoices"],
      footnote: el("span", {}, "New here? ", el("a", { href: "/" }, "See packages"), " · ", el("a", { href: HELP }, "Help")),
    },
    context,
    navHeader: orgHeader,
    footLinks: [
      { label: "Open inbox", href: cfg.inbox || "https://inbox.pigeonpost.dev", icon: "inbox" },
      { label: "Packages and pricing", href: "/", icon: "plan" },
      { label: "Help centre", href: HELP, icon: "support", external: true },
    ],
    noAccess: { title: "Nothing here for this account", text: "This account has no organization to manage." },
    pages: PAGES.map((p) => ({ key: p.key, title: p.title, group: p.group, icon: p.icon,
      allow: () => (p.always ? true : app.can(p.key)),
      render: page(p.key, p.fn) })),
  });
})();
