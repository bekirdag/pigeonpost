// Pigeonpost for teams — the admin app (/app, SDS §5.2–5.3): People, Agents, Rooms, Rules, Billing,
// Security, API keys and Webhooks (Phase 5, behind `org_api`), Organization. One primary action per page (top right), at most one table per page, one
// sheet at a time. Every decision is the postbox's; this page only shows what it may do.
(function () {
  "use strict";
  const PPE = window.PPE;
  const R = PPE.rules;
  const el = PPE.el;
  const cfg = window.PPE_CONFIG || {};

  const ADMIN = ["owner", "admin", "people_admin", "agent_admin", "billing_admin"];
  const ROLE_NAMES = { owner: "Owner", admin: "Admin", people_admin: "People admin", agent_admin: "Agent admin", billing_admin: "Billing admin" };
  const PAGES = [
    { key: "people", title: "People", roles: ["owner", "admin", "people_admin"] },
    { key: "agents", title: "Agents", roles: ["owner", "admin", "agent_admin"] },
    { key: "rooms", title: "Rooms", roles: ADMIN },
    { key: "rules", title: "Rules", roles: ["owner", "admin", "people_admin", "agent_admin"] },
    { key: "billing", title: "Billing", roles: ["owner", "admin", "billing_admin"] },
    { key: "security", title: "Security", roles: ["owner", "admin"] },
    { key: "api-keys", title: "API keys", roles: ["owner", "admin"], feature: "org_api" },
    { key: "webhooks", title: "Webhooks", roles: ["owner", "admin"], feature: "org_api" },
    { key: "organization", title: "Organization", roles: ["owner", "admin"] },
  ];
  const CAPABILITIES = [
    ["enroll", "Log it in on a machine"],
    ["bind_handle", "Bind the address to a machine's mailbox"],
    ["configure_rules", "Change its personal rules"],
    ["read_inbox", "Read its inbox in the apps"],
    ["ssh", "Open a terminal on its workspace (SSH)"],
  ];
  const sshOn = () => app.features.includes("agent_ssh");
  const streamsOn = () => app.features.includes("streaming");

  const app = { features: [], orgs: [], org: null, me: null, page: "people" };
  PPE.app = app;

  // ---- helpers ---------------------------------------------------------------------------------

  const has = (...roles) => (app.me && Array.isArray(app.me.roles) ? app.me.roles.some((r) => roles.includes(r)) : false);
  app.can = (page) => { const p = PAGES.find((x) => x.key === page); return Boolean(p && has(...p.roles) && (!p.feature || app.features.includes(p.feature))); };
  /// Roles this admin may hand out (SDS §6.4, D-13): only the owner appoints full admins.
  app.grantable = () => (has("owner") ? ["admin", "people_admin", "agent_admin", "billing_admin"]
    : has("admin") ? ["people_admin", "agent_admin", "billing_admin"] : []);
  const orgPath = (rest = "") => `/v1/orgs/${encodeURIComponent(app.org.id)}${rest}`;
  const ns = () => (app.org ? app.org.namespace : "");
  const inviteEmailOn = () => app.features.includes("org_invite_email");

  function chipFor(status) {
    const kind = { active: "chip-ok", invited: "chip-warn", suspended: "chip-bad", removed: "chip-bad", deleted: "chip-bad", provisioning: "chip-warn" }[status] || "";
    const text = { active: "Active", invited: "Invited", suspended: "Suspended", provisioning: "Setting up" }[status] || status;
    return el("span", { class: `chip ${kind}` }, text);
  }
  function head(title, sub, primary) {
    return el("div", { class: "page-head" }, el("div", {}, el("h1", { tabindex: "-1" }, title), sub ? el("p", {}, sub) : null), primary || null);
  }
  function content(...nodes) {
    const c = document.getElementById("content");
    c.replaceChildren(...nodes);
    const h = c.querySelector("h1");
    if (h) h.focus();
  }
  function secretBox(text, onceNote) {
    return el("div", {},
      el("div", { class: "secret" }, el("code", {}, text), el("button", { class: "btn btn-small", type: "button", onclick: () => PPE.copy(text) }, "Copy")),
      onceNote ? el("p", { class: "hint" }, onceNote) : null);
  }
  function busy(button, fn) {
    return async (...args) => {
      if (button) button.disabled = true;
      try { await fn(...args); } catch (e) { PPE.fail(e); } finally { if (button && document.contains(button)) button.disabled = false; }
    };
  }

  // ---- shell -----------------------------------------------------------------------------------

  function gate(...nodes) {
    document.getElementById("shell").hidden = true;
    const g = document.getElementById("gate");
    g.hidden = false;
    g.replaceChildren(el("div", { class: "card center-card" }, ...nodes));
  }

  function renderNav() {
    const nav = document.getElementById("nav");
    nav.replaceChildren();
    const admins = app.orgs.filter((o) => (o.my_roles || []).some((r) => ADMIN.includes(r)));
    if (admins.length > 1) {
      const pick = el("select", { "aria-label": "Organization" }, admins.map((o) => el("option", { value: o.id, selected: o.id === app.org.id }, o.name)));
      pick.addEventListener("change", () => { PPE.store.set("ppe_org", pick.value); load(pick.value).then(() => go(app.page, true)); });
      nav.append(pick);
    }
    nav.append(el("div", { class: "org-name" }, app.org.name), el("div", { class: "org-ns" }, `/${app.org.namespace}`));
    for (const p of PAGES) {
      if (!app.can(p.key)) continue;
      const a = el("a", { href: `/app/${p.key}`, "aria-current": p.key === app.page ? "page" : null }, p.title);
      a.addEventListener("click", (e) => { e.preventDefault(); go(p.key); });
      nav.append(a);
    }
    nav.append(el("a", { href: cfg.inbox || "https://inbox.pigeonpost.dev" }, "Open inbox ↗"));
  }

  function banner() {
    const s = app.org.status;
    if (s === "past_due") return el("div", { class: "banner" }, "The last payment didn't go through. Update the card in Billing to avoid interruption.");
    if (s === "read_only") return el("div", { class: "banner banner-danger" }, "The organization is read-only: no new invitations or agents, and company mailboxes can't send. Update the payment in Billing.");
    if (s === "suspended") return el("div", { class: "banner banner-danger" }, "The organization is suspended: company addresses don't receive mail. Renew in Billing to restore it.");
    return null;
  }

  /// The owner picks their own company address once (docs/orgs.md §2).
  function firstRun() {
    if (!has("owner") || (app.me && app.me.mailbox)) return null;
    const name = el("input", { type: "text", maxlength: 32, spellcheck: "false", autocapitalize: "none", value: (PPE.whoami().username || "").toLowerCase().replace(/[^a-z0-9._-]/g, "").slice(0, 32) });
    const status = el("p", { class: "status-line", role: "status" });
    const save = el("button", { class: "btn btn-primary", type: "submit" }, "Use this address");
    const form = el("form", { class: "card" },
      el("h2", {}, `Choose your address in ${app.org.name}`),
      el("p", { class: "muted" }, "This is how people reach you at work. The company's General room is set up with it."),
      PPE.field("Your address", el("div", { class: "prefixed" }, el("span", {}, `/${ns()}/`), name)), status,
      el("div", { class: "actions" }, save));
    form.addEventListener("submit", busy(save, async (e) => {
      e.preventDefault();
      const v = name.value.trim().toLowerCase();
      if (!/^[a-z0-9][a-z0-9._-]{0,31}$/.test(v)) { status.className = "status-line status-bad"; status.textContent = "Use letters, numbers, dots, underscores or hyphens."; return; }
      await PPE.postbox(orgPath("/me/mailbox"), { method: "POST", body: { handle: `/${ns()}/${v}` } });
      PPE.toast(`You're /${ns()}/${v}. Open your inbox to finish setting up General.`);
      await load(app.org.id);
      go(app.page, true);
    }));
    return form;
  }

  async function go(page, replace = false) {
    if (!app.can(page)) page = (PAGES.find((p) => app.can(p.key)) || PAGES[0]).key;
    app.page = page;
    const url = `/app/${page}`;
    if (window.location.pathname !== url) (replace ? window.history.replaceState : window.history.pushState).call(window.history, {}, "", url);
    renderNav();
    document.title = `${PAGES.find((p) => p.key === page).title} — ${app.org.name}`;
    content(el("p", { class: "muted" }, "Loading…"));
    try {
      const node = await PAGE[page]();
      content(...[banner(), firstRun(), node].filter(Boolean));
    } catch (e) {
      if (e instanceof PPE.Redirecting) return;
      // The server switched the feature off since capabilities were read: hide its pages.
      const p = PAGES.find((x) => x.key === page);
      if (e && e.code === "feature_disabled" && p && p.feature) {
        app.features = app.features.filter((f) => f !== p.feature);
        return go(page, true);
      }
      content(el("div", { class: "card" }, el("h1", {}, "Couldn't load this page"), el("p", { class: "muted" }, PPE.explain(e))));
    }
  }
  app.go = go;

  // ---- People ----------------------------------------------------------------------------------

  async function people() {
    const { members = [] } = await PPE.postbox(orgPath("/members"));
    app.members = members;
    const o = app.org;
    const invite = el("button", { class: "btn btn-primary", type: "button", onclick: () => inviteSheet() }, "Invite people");
    if (o.status === "read_only" || o.status === "suspended") invite.disabled = true;
    const rows = members.filter((m) => m.status !== "removed").map((m) => el("tr", {},
      el("td", {}, `${m.first_name || ""} ${m.last_name || ""}`.trim() || "—", el("span", { class: "sub" }, m.contact_masked || "")),
      el("td", { class: "mono" }, m.handle || m.address || ""),
      el("td", {}, (m.roles || []).length ? m.roles.map((r) => el("span", { class: "chip" }, ROLE_NAMES[r] || r)) : el("span", { class: "muted" }, "Member")),
      el("td", {}, chipFor(m.status), m.invite && m.invite.state === "expired" ? el("span", { class: "sub" }, "Expired — resend") : null),
      el("td", {}, m.status === "invited" ? `Invited ${PPE.date(m.invited_at)}` : PPE.ago(m.last_active_at)),
      el("td", { class: "row-actions" }, el("button", { class: "btn btn-small", type: "button", "aria-label": `Manage ${m.first_name || m.handle}`, onclick: () => memberSheet(m) }, "Manage"))));
    return el("div", {},
      head("People", `${o.seats_used ?? members.filter((m) => m.status !== "removed").length} of ${o.seats} seats used`, invite),
      rows.length ? el("div", { class: "table-wrap" }, el("table", {},
        el("thead", {}, el("tr", {}, ["Name", "Address", "Role", "Status", "Last active", ""].map((h) => el("th", { scope: "col" }, h)))),
        el("tbody", {}, rows))) : el("div", { class: "empty card" }, "No one here yet. Invite your first teammates."));
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
      const list = el("div", {});
      const grantable = app.grantable();
      const addRow = (v = {}) => {
        const first = el("input", { type: "text", maxlength: 60, value: v.first_name || "", "aria-label": "First name" });
        const last = el("input", { type: "text", maxlength: 60, value: v.last_name || "", "aria-label": "Last name" });
        const contact = el("input", { type: "text", maxlength: 254, spellcheck: "false", value: v.contact || "", "aria-label": "Contact address" });
        const handle = el("input", { type: "text", maxlength: 32, spellcheck: "false", value: (v.handle || "").replace(`/${ns()}/`, ""), "aria-label": "Company address" });
        let touched = Boolean(v.handle);
        first.addEventListener("input", () => { if (!touched) handle.value = app.handleFor(first.value); });
        handle.addEventListener("input", () => { touched = true; });
        const role = el("select", { "aria-label": "Role" }, el("option", { value: "" }, "Member"), grantable.map((r) => el("option", { value: r }, ROLE_NAMES[r])));
        const remove = el("button", { class: "btn btn-quiet btn-small", type: "button", onclick: () => row.remove() }, "Remove");
        const row = el("div", { class: "card invite-row" },
          el("div", { class: "row" }, PPE.field("First name", first), PPE.field("Last name", last)),
          PPE.field("Contact address", contact, "The invitation goes here, and they must sign in with this address."),
          PPE.field("Company address", el("div", { class: "prefixed" }, el("span", {}, `/${ns()}/`), handle)),
          grantable.length ? PPE.field("Role", role) : null,
          el("div", { class: "actions" }, remove));
        row._read = () => ({ first_name: first.value.trim(), last_name: last.value.trim(), contact: contact.value.trim(),
          handle: handle.value.trim() ? `/${ns()}/${handle.value.trim().toLowerCase()}` : "", roles: role.value ? [role.value] : [] });
        list.append(row);
      };
      const paste = el("textarea", { "aria-label": "Paste rows", placeholder: "Jane, Doe, jane@acme.com, jane" });
      const status = el("p", { class: "status-line", role: "status" });
      const send = el("button", { class: "btn btn-primary", type: "button" }, "Send invitations");
      addRow();
      send.addEventListener("click", busy(send, async () => {
        const invites = Array.from(list.children).map((r) => r._read()).filter((r) => r.first_name || r.contact);
        const bad = invites.find((r) => !r.first_name || !r.last_name || !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(r.contact) || !/^\/[a-z0-9][a-z0-9._-]*\/[a-z0-9][a-z0-9._-]{0,31}$/.test(r.handle));
        if (!invites.length || bad) { status.className = "status-line status-bad"; status.textContent = "Every row needs a first and last name, a valid contact address and a company address."; return; }
        const r = await PPE.postbox(orgPath("/invites"), { method: "POST", body: { invites } });
        inviteResults(r.results || [], invites);
      }));
      body.append(list,
        el("div", { class: "actions" }, el("button", { class: "btn", type: "button", onclick: () => addRow() }, "Add another")),
        el("details", {}, el("summary", {}, "Paste a list"),
          PPE.field("One person per line: first name, last name, contact address, and optionally their address", paste),
          el("button", { class: "btn btn-small", type: "button", onclick: () => { app.parseRows(paste.value).forEach(addRow); paste.value = ""; } }, "Add these")),
        status, el("div", { class: "actions" }, send));
    });
  }

  function inviteResults(results, asked) {
    PPE.openSheet("Invitations", (body) => {
      const once = inviteEmailOn() ? null : "Shown once. Send it to them yourself — it only works for the invited address.";
      for (const [i, r] of results.entries()) {
        const who = asked[i] ? `${asked[i].first_name} ${asked[i].last_name}` : r.handle;
        const ok = r.status === "invited";
        body.append(el("div", { class: "card" },
          el("h3", {}, who, " ", el("span", { class: `chip ${ok ? "chip-ok" : "chip-bad"}` }, ok ? "Invited" : PPE.explain({ code: r.status, message: r.status }))),
          el("p", { class: "mono muted" }, r.handle || ""),
          ok && r.link ? secretBox(r.link, once) : ok ? el("p", { class: "muted" }, "The invitation is on its way.") : null));
      }
      body.append(el("div", { class: "actions" }, el("button", { class: "btn btn-primary", type: "button", onclick: () => { PPE.closeSheet(); go("people", true); } }, "Done")));
    });
  }

  function memberSheet(m) {
    PPE.openSheet(`${m.first_name || ""} ${m.last_name || ""}`.trim() || m.handle, (body) => {
      const isOwner = (m.roles || []).includes("owner");
      const first = el("input", { type: "text", value: m.first_name || "", maxlength: 60 });
      const last = el("input", { type: "text", value: m.last_name || "", maxlength: 60 });
      const saveName = el("button", { class: "btn btn-small", type: "button" }, "Save name");
      saveName.addEventListener("click", busy(saveName, async () => {
        await PPE.postbox(orgPath(`/members/${encodeURIComponent(m.id)}`), { method: "PATCH", body: { first_name: first.value.trim(), last_name: last.value.trim() } });
        PPE.toast("Saved"); go("people", true);
      }));
      body.append(el("dl", { class: "kv" }, el("dt", {}, "Address"), el("dd", { class: "mono" }, m.handle || "—"),
        el("dt", {}, "Status"), el("dd", {}, chipFor(m.status)), el("dt", {}, "Invited to"), el("dd", {}, m.contact_masked || "—")),
      el("div", { class: "row" }, PPE.field("First name", first), PPE.field("Last name", last)), el("div", { class: "actions" }, saveName));

      const grantable = app.grantable();
      if (!isOwner && grantable.length && m.status !== "invited") {
        const boxes = grantable.map((r) => { const c = el("input", { type: "checkbox", value: r, checked: (m.roles || []).includes(r) }); return el("label", { class: "check" }, c, ROLE_NAMES[r]); });
        const saveRoles = el("button", { class: "btn btn-small", type: "button" }, "Save roles");
        saveRoles.addEventListener("click", busy(saveRoles, async () => {
          const roles = boxes.map((b) => b.querySelector("input")).filter((c) => c.checked).map((c) => c.value);
          const keep = (m.roles || []).filter((r) => !grantable.includes(r));
          await setRoles(m.id, [...keep, ...roles]);
        }));
        body.append(el("h3", {}, "Roles"), ...boxes, el("p", { class: "hint" }, "Admin roles never include reading anyone's messages."), el("div", { class: "actions" }, saveRoles));
      }

      const actions = el("div", { class: "actions" });
      if (m.status === "invited" && m.invite) {
        const resend = el("button", { class: "btn", type: "button" }, "Resend invitation");
        resend.addEventListener("click", busy(resend, async () => {
          const r = await PPE.postbox(orgPath(`/invites/${encodeURIComponent(m.invite.id)}/resend`), { method: "POST", body: {} });
          PPE.openSheet("New invitation link", (b) => b.append(el("p", {}, "The old link no longer works."),
            r.link ? secretBox(r.link, inviteEmailOn() ? null : "Shown once.") : el("p", {}, "The invitation is on its way.")));
        }));
        const revoke = el("button", { class: "btn btn-danger", type: "button" }, "Revoke invitation");
        revoke.addEventListener("click", busy(revoke, async () => {
          await PPE.postbox(orgPath(`/invites/${encodeURIComponent(m.invite.id)}`), { method: "DELETE" });
          PPE.closeSheet(); PPE.toast("Invitation revoked"); go("people", true);
        }));
        actions.append(resend, revoke);
      } else if (!isOwner && (m.status === "active" || m.status === "suspended")) {
        const toggle = el("button", { class: "btn", type: "button" }, m.status === "active" ? "Suspend" : "Reactivate");
        toggle.addEventListener("click", busy(toggle, async () => {
          await PPE.postbox(orgPath(`/members/${encodeURIComponent(m.id)}`), { method: "PATCH", body: { status: m.status === "active" ? "suspended" : "active" } });
          PPE.closeSheet(); PPE.toast(m.status === "active" ? "Suspended" : "Reactivated"); go("people", true);
        }));
        const remove = el("button", { class: "btn btn-danger", type: "button", onclick: () => removeSheet(m) }, "Remove…");
        actions.append(toggle, remove);
      }
      body.append(actions);
    });
  }

  PPE.onResume("app.roles", async (p) => setRoles(p.member_id, p.roles));
  async function setRoles(memberId, roles) {
    await PPE.postbox(orgPath(`/members/${encodeURIComponent(memberId)}`), { method: "PATCH", body: { roles }, resume: { name: "app.roles", payload: { member_id: memberId, roles } } });
    PPE.closeSheet(); PPE.toast("Roles updated"); go("people", true);
  }

  function removeSheet(m) {
    PPE.openSheet(`Remove ${m.first_name || m.handle}`, (body) => {
      const keep = el("input", { type: "radio", name: "mode", value: "reserve", checked: true });
      const del = el("input", { type: "radio", name: "mode", value: "delete" });
      const go_ = el("button", { class: "btn btn-danger", type: "button" }, "Remove");
      go_.addEventListener("click", busy(go_, async () => {
        await PPE.postbox(orgPath(`/members/${encodeURIComponent(m.id)}/remove`), { method: "POST", body: { mode: keep.checked ? "reserve" : "delete" } });
        PPE.closeSheet(); PPE.toast("Removed"); go("people", true);
      }));
      body.append(el("p", {}, `${m.handle} loses access to the company mailbox, rooms and every agent delegated to them. Their personal account is not affected.`),
        el("label", { class: "check" }, keep, el("span", {}, el("strong", {}, "Keep the address for someone else. "), "Invite a new person to the same address later. They start with an empty mailbox; history doesn't move.")),
        el("label", { class: "check" }, del, el("span", {}, el("strong", {}, "Delete the address. "), "Mail to it stops being delivered and the name is freed.")),
        el("div", { class: "actions" }, el("button", { class: "btn", type: "button", onclick: () => PPE.closeSheet() }, "Cancel"), go_));
    });
  }

  // ---- Agents ----------------------------------------------------------------------------------

  async function agents() {
    const { agents: list = [] } = await PPE.postbox(orgPath("/agents"));
    const o = app.org;
    const add = el("button", { class: "btn btn-primary", type: "button", onclick: () => addAgentSheet() }, "Add agent");
    if (o.status === "read_only" || o.status === "suspended") add.disabled = true;
    const rows = list.filter((a) => a.status !== "deleted").map((a) => el("tr", {},
      el("td", { class: "mono" }, a.handle, a.label ? el("span", { class: "sub" }, a.label) : null),
      el("td", {}, (a.operators || []).length ? a.operators.map((p) => el("span", { class: "chip" }, p.handle)) : el("span", { class: "muted" }, "Agent admins")),
      el("td", {}, a.devices ? (a.last_seen_at ? `Last seen ${PPE.ago(a.last_seen_at)}` : "Connected") : "Never connected"),
      el("td", {}, chipFor(a.status)),
      el("td", { class: "row-actions" }, el("button", { class: "btn btn-small", type: "button", onclick: () => agentSheet(a) }, "Manage"))));
    return el("div", {},
      head("Agents", `${o.agents_used ?? rows.length} of ${o.agent_limit}`, add),
      rows.length ? el("div", { class: "table-wrap" }, el("table", {},
        el("thead", {}, el("tr", {}, ["Address", "Operators", "Machine", "Status", ""].map((h) => el("th", { scope: "col" }, h)))),
        el("tbody", {}, rows))) : el("div", { class: "empty card" }, "No agents yet. Add one and log it in on its machine."));
  }

  app.connectCommand = (handle, label) => `pigeonpost login --device --handle ${handle}${label ? ` --label "${String(label).replace(/["\\$`]/g, "")}"` : ""} --onboard`;

  async function addAgentSheet() {
    let members = app.members;
    if (!members) { try { members = (await PPE.postbox(orgPath("/members"))).members || []; } catch { members = []; } }
    PPE.openSheet("Add agent", (body) => {
      const name = el("input", { type: "text", maxlength: 32, spellcheck: "false", autocapitalize: "none" });
      const label = el("input", { type: "text", maxlength: 80, placeholder: "Build box" });
      const ops = members.filter((m) => m.status === "active" && !(m.roles || []).some((r) => ["owner", "admin", "agent_admin"].includes(r)))
        .map((m) => el("label", { class: "check" }, el("input", { type: "checkbox", value: m.id }), `${m.first_name} ${m.last_name} `, el("span", { class: "muted mono" }, m.handle)));
      const how = ["qr", "code", "terminal"].map((v, i) => el("label", { class: "check" }, el("input", { type: "radio", name: "how", value: v, checked: i === 0 }),
        el("span", {}, el("strong", {}, ["QR on the machine. ", "Enrollment code. ", "From the terminal. "][i]),
          ["Run one command on the machine and approve it from your phone.", "A one-time code, valid for 24 hours, for scripted installs.", "Run the command yourself later."][i])));
      const status = el("p", { class: "status-line", role: "status" });
      const create = el("button", { class: "btn btn-primary", type: "button" }, "Add agent");
      create.addEventListener("click", busy(create, async () => {
        const v = name.value.trim().toLowerCase();
        if (!/^[a-z0-9][a-z0-9._-]{0,31}$/.test(v)) { status.className = "status-line status-bad"; status.textContent = "Use letters, numbers, dots, underscores or hyphens."; return; }
        const operators = ops.map((o) => o.querySelector("input")).filter((c) => c.checked).map((c) => c.value);
        const r = await PPE.postbox(orgPath("/agents"), { method: "POST", body: { handle: `/${ns()}/${v}`, ...(label.value.trim() ? { label: label.value.trim() } : {}), operators } });
        const choice = how.map((h) => h.querySelector("input")).find((c) => c.checked).value;
        connectSheet(r.agent || { handle: `/${ns()}/${v}`, address: null, label: label.value.trim() }, choice);
      }));
      body.append(PPE.field("Address", el("div", { class: "prefixed" }, el("span", {}, `/${ns()}/`), name)),
        PPE.field("Label", label, "Where it runs, so people recognize it."),
        ops.length ? el("fieldset", { class: "card" }, el("legend", {}, "Operators: people who may log it in"), ...ops) : null,
        el("fieldset", { class: "card" }, el("legend", {}, "How to connect"), ...how),
        status, el("div", { class: "actions" }, create));
    });
  }

  function connectSheet(agent, choice) {
    PPE.openSheet(`Connect ${agent.handle}`, (body) => {
      if (choice === "code") {
        const make = el("button", { class: "btn btn-primary", type: "button" }, "Create enrollment code");
        make.addEventListener("click", busy(make, () => enroll(agent)));
        body.append(el("p", {}, "Creating a code needs a fresh, strong sign-in. The code works once, for this agent only."), el("div", { class: "actions" }, make));
      } else {
        body.append(el("p", {}, "On the agent's machine, run:"), secretBox(app.connectCommand(agent.handle, agent.label)),
          el("p", { class: "muted" }, "It shows a QR code and a confirmation number. Scan it with your phone (or open the link), check the number matches, and hold Approve."));
      }
      body.append(el("div", { class: "actions" }, el("button", { class: "btn", type: "button", onclick: () => { PPE.closeSheet(); go("agents", true); } }, "Done")));
    });
  }

  PPE.onResume("app.enroll", async (p) => enroll({ address: p.address, handle: p.handle }));
  async function enroll(agent) {
    const key = agent.address || agent.handle;
    const r = await PPE.postbox(orgPath(`/agents/${encodeURIComponent(key)}/enrollments`), { method: "POST", body: { ttl_hours: 24 },
      resume: { name: "app.enroll", payload: { address: agent.address, handle: agent.handle } } });
    PPE.openSheet(`Enrollment code for ${agent.handle}`, (body) => body.append(
      el("p", {}, `Valid until ${new Date(r.expires_at * 1000).toLocaleString()}. Shown once.`),
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
      const label = el("input", { type: "text", maxlength: 80, value: a.label || "" });
      const save = el("button", { class: "btn btn-small", type: "button" }, "Save");
      save.addEventListener("click", busy(save, async () => {
        await PPE.postbox(orgPath(`/agents/${encodeURIComponent(a.address)}`), { method: "PATCH", body: { label: label.value.trim() } });
        PPE.toast("Saved"); go("agents", true);
      }));
      body.append(PPE.field("Label", label), el("div", { class: "actions" }, save));

      body.append(el("h3", {}, "Machines"));
      const live = devices.filter((d) => !d.revoked_at);
      if (!live.length) body.append(el("p", { class: "muted" }, "Not logged in anywhere."), el("p", {}, "Run on the machine:"), secretBox(app.connectCommand(a.handle, a.label)));
      for (const d of live) {
        const rev = el("button", { class: "btn btn-small btn-danger", type: "button" }, "Disconnect");
        rev.addEventListener("click", busy(rev, async () => {
          await PPE.postbox(orgPath(`/devices/${encodeURIComponent(d.id)}`), { method: "DELETE" });
          PPE.toast("Disconnected"); PPE.closeSheet(); go("agents", true);
        }));
        body.append(el("div", { class: "setting" }, el("p", {}, d.label || "Machine", el("span", { class: "sub muted" }, ` · since ${PPE.date(d.created_at)} · ${d.source}`)), rev));
      }
      const code = el("button", { class: "btn btn-small", type: "button" }, "New enrollment code");
      code.addEventListener("click", busy(code, () => enroll(a)));
      body.append(el("div", { class: "actions" }, code));

      body.append(el("h3", {}, "Operators"));
      for (const d of delegations) {
        const rev = el("button", { class: "btn btn-small", type: "button" }, "Revoke");
        rev.addEventListener("click", busy(rev, async () => {
          await PPE.postbox(orgPath(`/delegations/${encodeURIComponent(d.id)}`), { method: "DELETE" });
          PPE.toast("Revoked"); PPE.closeSheet(); go("agents", true);
        }));
        body.append(el("div", { class: "setting" }, el("p", {}, d.member_handle, el("span", { class: "sub muted" }, ` · ${(d.capabilities || []).join(", ")}${d.expires_at ? ` · until ${PPE.date(d.expires_at)}` : ""}`)), rev));
      }
      const who = el("select", { "aria-label": "Person" }, members.filter((m) => m.status === "active").map((m) => el("option", { value: m.id }, `${m.first_name} ${m.last_name} (${m.handle})`)));
      const caps = CAPABILITIES.map(([k, t]) => el("label", { class: "check" }, el("input", { type: "checkbox", value: k, checked: k === "enroll" }), t));
      const until = el("input", { type: "date", "aria-label": "Until (optional)" });
      const grant = el("button", { class: "btn btn-small", type: "button" }, "Add operator");
      grant.addEventListener("click", busy(grant, async () => {
        const capabilities = caps.map((c) => c.querySelector("input")).filter((c) => c.checked).map((c) => c.value);
        if (!who.value || !capabilities.length) return PPE.toast("Choose a person and at least one permission.", "error");
        const expires_at = until.value ? Math.floor(Date.parse(until.value + "T23:59:59Z") / 1000) : undefined;
        await PPE.postbox(orgPath("/delegations"), { method: "POST", body: { member_id: who.value, target: a.address, capabilities, ...(expires_at ? { expires_at } : {}) } });
        PPE.toast("Operator added"); PPE.closeSheet(); go("agents", true);
      }));
      body.append(el("details", {}, el("summary", {}, "Add an operator"), PPE.field("Person", who), ...caps, PPE.field("Until (optional)", until), el("div", { class: "actions" }, grant)));

      if (sshOn()) body.append(sshSection(a, ssh));

      const toggle = el("button", { class: "btn", type: "button" }, a.status === "active" ? "Suspend" : "Reactivate");
      toggle.addEventListener("click", busy(toggle, async () => {
        await PPE.postbox(orgPath(`/agents/${encodeURIComponent(a.address)}`), { method: "PATCH", body: { status: a.status === "active" ? "suspended" : "active" } });
        PPE.closeSheet(); go("agents", true);
      }));
      const del = el("button", { class: "btn btn-danger", type: "button" }, "Delete agent");
      del.addEventListener("click", busy(del, async () => {
        if (!window.confirm(`Delete ${a.handle}? Its machines are disconnected and the address stops receiving.`)) return;
        await PPE.postbox(orgPath(`/agents/${encodeURIComponent(a.address)}`), { method: "DELETE" });
        PPE.closeSheet(); PPE.toast("Deleted"); go("agents", true);
      }));
      body.append(el("div", { class: "actions" }, toggle, del));
    });
  }

  // ---- Rooms -----------------------------------------------------------------------------------

  async function rooms() {
    const { rooms: list = [] } = await PPE.postbox(orgPath("/rooms"));
    const create = el("button", { class: "btn btn-primary", type: "button", onclick: () => roomSheet() }, "Create room");
    const rows = list.map((r) => el("tr", {},
      el("td", {}, r.name, r.description ? el("span", { class: "sub" }, r.description) : null),
      el("td", {}, String(r.member_count ?? 0)),
      el("td", {}, r.post_policy === "admins" ? "Admins only" : "Everyone"),
      el("td", {}, r.listed ? "Listed" : "Unlisted", r.join_policy === "private" ? el("span", { class: "sub" }, "Invite only") : null),
      el("td", {}, r.auto_join ? "Yes" : "No"),
      el("td", {}, r.activation_state === "provisioning" ? chipFor("provisioning") : chipFor("active")),
      el("td", { class: "row-actions" }, el("button", { class: "btn btn-small", type: "button", onclick: () => manageRoom(r) }, "Manage"))));
    return el("div", {}, head("Rooms", "Company rooms are end-to-end encrypted. New members join auto-join rooms when they accept.", create),
      rows.length ? el("div", { class: "table-wrap" }, el("table", {},
        el("thead", {}, el("tr", {}, ["Room", "Members", "Who can post", "Directory", "Auto-join", "State", ""].map((h) => el("th", { scope: "col" }, h)))),
        el("tbody", {}, rows))) : el("div", { class: "empty card" }, "No rooms yet."),
      list.some((r) => r.activation_state === "provisioning") ? el("p", { class: "note" }, "A room is set up when its creator next opens the inbox: their device creates the encryption keys.") : null);
  }

  function roomSheet() {
    PPE.openSheet("Create room", (body) => {
      const name = el("input", { type: "text", maxlength: 80, placeholder: "design" });
      const desc = el("textarea", { maxlength: 500 });
      const join = el("select", {}, el("option", { value: "public" }, "Anyone at the company can join"), el("option", { value: "private" }, "Invite only"));
      const post = el("select", {}, el("option", { value: "everyone" }, "Everyone"), el("option", { value: "admins" }, "Only room admins"));
      const listed = el("input", { type: "checkbox", checked: true });
      const auto = el("input", { type: "checkbox" });
      const create = el("button", { class: "btn btn-primary", type: "button" }, "Create room");
      create.addEventListener("click", busy(create, async () => {
        if (!name.value.trim()) return PPE.toast("Give the room a name.", "error");
        await PPE.postbox(orgPath("/rooms"), { method: "POST", body: { name: name.value.trim(), ...(desc.value.trim() ? { description: desc.value.trim() } : {}),
          join_policy: join.value, post_policy: post.value, listed: listed.checked, auto_join: auto.checked } });
        PPE.closeSheet(); PPE.toast("Room created. Open your inbox to finish setting up its encryption."); go("rooms", true);
      }));
      body.append(PPE.field("Name", name), PPE.field("Description (optional)", desc), PPE.field("Who can join", join), PPE.field("Who can post", post),
        el("label", { class: "check" }, listed, "Listed in the company directory"),
        el("label", { class: "check" }, auto, "Add everyone who joins the company"),
        el("div", { class: "actions" }, create));
    });
  }

  function manageRoom(r) {
    PPE.openSheet(r.name, (body) => {
      const listed = el("input", { type: "checkbox", checked: Boolean(r.listed) });
      const auto = el("input", { type: "checkbox", checked: Boolean(r.auto_join) });
      const save = el("button", { class: "btn", type: "button" }, "Save");
      save.addEventListener("click", busy(save, async () => {
        await PPE.postbox(orgPath(`/rooms/${encodeURIComponent(r.id)}`), { method: "PATCH", body: { listed: listed.checked, auto_join: auto.checked } });
        PPE.closeSheet(); PPE.toast("Saved"); go("rooms", true);
      }));
      const del = el("button", { class: "btn btn-danger", type: "button" }, "Delete room");
      del.addEventListener("click", busy(del, async () => {
        if (!window.confirm(`Delete ${r.name}? Members see "This group was deleted" and its messages are purged within 30 days.`)) return;
        await PPE.postbox(orgPath(`/rooms/${encodeURIComponent(r.id)}`), { method: "DELETE" });
        PPE.closeSheet(); PPE.toast("Deleted"); go("rooms", true);
      }));
      body.append(el("label", { class: "check" }, listed, "Listed in the company directory"),
        el("label", { class: "check" }, auto, "Add everyone who joins the company"),
        el("p", { class: "hint" }, "Members, posting and history are managed in the room itself, in the inbox."),
        el("div", { class: "actions" }, del, save));
    });
  }

  // ---- Rules -----------------------------------------------------------------------------------

  function picker(kind, custom, side) {
    const sel = el("select", { "aria-label": side === "owners" ? "Who the rule is for" : "Who they hear from" },
      el("option", { value: "everyone" }, side === "owners" ? `Everyone at ${app.org.name}` : `everyone at ${app.org.name}`),
      el("option", { value: "members" }, side === "owners" ? `People at ${app.org.name}` : `people at ${app.org.name}`),
      el("option", { value: "agents" }, side === "owners" ? `Agents at ${app.org.name}` : `agents at ${app.org.name}`),
      side === "peers" ? el("option", { value: "anyone" }, "anyone") : null,
      el("option", { value: "address" }, "a specific address…"));
    sel.value = kind;
    const text = el("input", { type: "text", value: custom, placeholder: side === "owners" ? `/${ns()}/agent1` : `/${ns()}/jane`, "aria-label": "Address or pattern", spellcheck: "false" });
    text.hidden = kind !== "address";
    sel.addEventListener("change", () => { text.hidden = sel.value !== "address"; if (!text.hidden) text.focus(); });
    return { sel, text };
  }

  /// One rule card with its sentence editor. `rule` is null for a new one.
  function ruleCard(rule, onDone, draft) {
    const f = R.toForm(rule || draft || { owners: `/${ns()}/*`, peers: `/${ns()}/*`, admission: "allow", known: true, priority: 100 }, ns());
    const owners = picker(f.ownersKind, f.ownersCustom, "owners");
    const peers = picker(f.peersKind, f.peersCustom, "peers");
    const admission = el("select", { "aria-label": "Mail" }, el("option", { value: "allow" }, "accept messages from"), el("option", { value: "block" }, "block messages from"), el("option", { value: "" }, "keep their own setting for"));
    admission.value = f.admission;
    const known = el("input", { type: "checkbox", checked: f.known });
    const autonomy = el("select", { "aria-label": "Requests" }, el("option", { value: "" }, "their own setting"), el("option", { value: "review" }, "always ask a person"), el("option", { value: "auto" }, "act on their own for"));
    autonomy.value = f.autonomy;
    const verbs = el("input", { type: "text", value: f.verbs, placeholder: "report_status, run_tests", "aria-label": "Actions", spellcheck: "false" });
    verbs.hidden = f.autonomy !== "auto";
    autonomy.addEventListener("change", () => { verbs.hidden = autonomy.value !== "auto"; });
    const locked = el("input", { type: "checkbox", checked: f.locked });
    const priority = el("input", { type: "number", min: 1, max: 1000, value: f.priority, "aria-label": "Priority", style: null });
    const desc = el("input", { type: "text", value: f.description, maxlength: 200, placeholder: "What this rule is for", "aria-label": "Description" });
    const status = el("p", { class: "status-line", role: "status" });
    const read = () => ({ ownersKind: owners.sel.value, ownersCustom: owners.text.value, peersKind: peers.sel.value, peersCustom: peers.text.value,
      admission: admission.value, known: known.checked, autonomy: autonomy.value, verbs: verbs.value, locked: locked.checked, priority: priority.value, description: desc.value.trim() });
    const save = el("button", { class: "btn btn-small btn-primary", type: "button" }, rule ? "Save" : "Add rule");
    save.addEventListener("click", busy(save, async () => {
      const body = R.fromForm(read(), ns());
      if (body.error) { status.className = "status-line status-bad"; status.textContent = body.error; return; }
      await PPE.postbox(orgPath(rule ? `/rules/${encodeURIComponent(rule.id)}` : "/rules"), { method: rule ? "PATCH" : "POST", body });
      PPE.toast(rule ? "Rule saved" : "Rule added"); onDone();
    }));
    const del = rule ? el("button", { class: "btn btn-small btn-quiet", type: "button" }, "Delete") : el("button", { class: "btn btn-small btn-quiet", type: "button", onclick: () => card.remove() }, "Cancel");
    if (rule) del.addEventListener("click", busy(del, async () => {
      await PPE.postbox(orgPath(`/rules/${encodeURIComponent(rule.id)}`), { method: "DELETE" });
      PPE.toast("Rule deleted"); onDone();
    }));
    const card = el("div", { class: "rule" },
      el("div", { class: "sentence" }, owners.sel, owners.text, " ", admission, " ", peers.sel, peers.text,
        el("label", { class: "check", style: null }, known, "as a known contact"), " Requests: ", autonomy, verbs),
      el("div", { class: "meta" }, el("label", {}, locked, " Locked (agents can't override)"), el("label", {}, "Priority ", priority), desc),
      rule ? el("p", { class: "note" }, R.sentence(rule, app.org.name, ns())) : null,
      status, el("div", { class: "actions" }, del, save));
    card._read = read;
    return card;
  }
  app.ruleCard = ruleCard;

  async function rules() {
    const { rules: list = [] } = await PPE.postbox(orgPath("/rules"));
    const wrap = el("div", {});
    const reload = () => go("rules", true);
    const add = el("button", { class: "btn btn-primary", type: "button", onclick: () => wrap.prepend(ruleCard(null, reload)) }, "Add rule");
    const templates = el("div", { class: "templates" }, R.templates(ns()).map((t) => {
      const b = el("button", { class: "btn btn-small", type: "button" }, t.title);
      b.addEventListener("click", busy(b, async () => {
        if (t.needsEdit) { wrap.prepend(ruleCard(null, reload, t.rules[0])); return; }
        for (const rule of t.rules) await PPE.postbox(orgPath("/rules"), { method: "POST", body: rule });
        PPE.toast("Template added"); reload();
      }));
      return b;
    }));
    for (const r of list.slice().sort((a, b) => a.priority - b.priority)) wrap.append(ruleCard(r, reload));
    return el("div", {},
      head("Rules", "Who can reach whom, for everyone at once. A person's own block always wins.", add),
      el("p", { class: "muted" }, "Start from a template:"), templates,
      el("div", { class: "actions", style: null }, el("button", { class: "btn btn-quiet btn-small", type: "button", onclick: testSheet }, "Test a rule")),
      list.length ? null : el("div", { class: "empty card" }, "No rules yet. Everyone uses their own settings."),
      wrap);
  }

  function testSheet() {
    PPE.openSheet("Test a rule", (body) => {
      const from = el("input", { type: "text", placeholder: "/acme/jane or any address", spellcheck: "false" });
      const to = el("input", { type: "text", placeholder: `/${ns()}/agent1`, spellcheck: "false" });
      const verb = el("input", { type: "text", placeholder: "run_tests (optional)", spellcheck: "false" });
      const out = el("div", { role: "status", "aria-live": "polite" });
      const run = el("button", { class: "btn btn-primary", type: "button" }, "Test");
      run.addEventListener("click", busy(run, async () => {
        const r = await PPE.postbox(orgPath("/rules/evaluate"), { method: "POST", body: { from: from.value.trim(), to: to.value.trim(), ...(verb.value.trim() ? { verb: verb.value.trim() } : {}) } });
        out.replaceChildren(el("div", { class: "card" },
          el("h3", {}, r.admitted ? "Delivered" : "Refused"),
          el("dl", { class: "kv" }, el("dt", {}, "Known contact"), el("dd", {}, r.known ? "Yes" : "No"), el("dt", {}, "Requests"), el("dd", {}, r.autonomy === "auto" ? `Acted on without review: ${(r.verbs || []).join(", ") || "—"}` : "Wait for a person"),
            el("dt", {}, "Decided by"), el("dd", {}, { personal_block: "Their own block", personal_contact: "Their own contact settings", org_rule: "A company rule", inbox_policy: "Their inbox settings" }[r.decided_by] || r.decided_by || "—")),
          r.matched_rule ? el("p", { class: "note" }, `Rule: ${r.matched_rule.description || r.matched_rule.id}`) : null,
          r.explanation ? el("p", { class: "muted" }, r.explanation) : null));
      }));
      body.append(PPE.field("From", from), PPE.field("To", to), PPE.field("Request", verb), el("div", { class: "actions" }, run), out);
    });
  }

  // ---- Billing ---------------------------------------------------------------------------------

  async function billing() {
    const b = await PPE.postbox(orgPath("/billing"));
    const m = /^ent-(\d+)-(monthly|yearly)-(usd|try)$/.exec(b.plan_code || "") || [];
    const interval = m[2] || "yearly";
    const change = el("a", { class: "btn btn-primary", href: `/?seats=${b.seats}&interval=${interval}&currency=${(m[3] || "usd").toUpperCase()}` }, "Change plan");
    const pct = (used, max) => `${Math.min(100, Math.round(((used || 0) / Math.max(1, max || 1)) * 100))}%`;
    let invoices = [];
    try { invoices = ((await PPE.bridge("/v1/invoices")).invoices || []); } catch (e) { if (e instanceof PPE.Redirecting) throw e; }
    const rows = invoices.map((i) => {
      const pdf = el("button", { class: "btn btn-small", type: "button" }, "PDF");
      pdf.addEventListener("click", busy(pdf, async () => { const r = await PPE.bridge(`/v1/invoices/${encodeURIComponent(i.id)}/download`); if (/^https:\/\//.test(r.url || "")) window.open(r.url, "_blank", "noopener"); }));
      return el("tr", {}, el("td", {}, String(i.issued_at || i.created_at || "").slice(0, 10)), el("td", {}, i.number || "—"), el("td", {}, i.status || ""),
        el("td", {}, i.total != null ? PPE.money(i.total, i.currency || "USD") : ""), el("td", { class: "row-actions" }, i.id ? pdf : null));
    });
    return el("div", {},
      head("Billing", null, change),
      el("div", { class: "card" },
        el("h2", {}, `${b.seats} seats, ${interval}`),
        el("p", { class: "muted" }, b.paid_through ? `Paid through ${PPE.date(b.paid_through)}` : b.period_end ? `Renews ${PPE.date(b.period_end)}` : ""),
        el("p", {}, `Seats: ${b.seats_used} of ${b.seats}`), el("div", { class: "meter", role: "img", "aria-label": `${b.seats_used} of ${b.seats} seats used` }, el("span", { style: null, "data-w": pct(b.seats_used, b.seats) })),
        el("p", {}, `Agents: ${b.agents_used} of ${b.agent_limit}`), el("div", { class: "meter", role: "img", "aria-label": `${b.agents_used} of ${b.agent_limit} agents` }, el("span", { "data-w": pct(b.agents_used, b.agent_limit) })),
        el("p", { class: "note" }, "To lower the number of seats, first remove people or agents above the new size. To cancel or change the card, contact support@pigeonpost.dev — self-service changes for company plans are coming.")),
      el("h2", {}, "Invoices"),
      rows.length ? el("div", { class: "table-wrap" }, el("table", {}, el("thead", {}, el("tr", {}, ["Date", "Number", "Status", "Total", ""].map((h) => el("th", { scope: "col" }, h)))), el("tbody", {}, rows)))
        : el("p", { class: "muted" }, "No invoices yet."));
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
    const wrap = el("div", {}, el("h3", {}, "Terminal (SSH)"));
    if (!(app.org.settings || {}).ssh_allowed) {
      wrap.append(el("p", { class: "muted" }, "SSH to agents is off for this organization. An owner or admin can allow it under Security."));
      return wrap;
    }
    wrap.append(toggle("Allow SSH to this agent (people still need the “Open a terminal” permission)", Boolean(org.agent_enabled), async (v) => {
      await PPE.postbox(orgPath(`/agents/${encodeURIComponent(a.address)}/shell`), { method: "PUT", body: { enabled: v } });
    }));
    const facts = st.enabled
      ? `${st.connected ? "Machine connected" : "Machine not connected"} · ${st.mode} mode · host key ${st.host_key_fp}`
      : "The agent's machine has not enabled SSH in its agentd.toml.";
    wrap.append(el("p", { class: "muted" }, facts));
    if (st.pending_host_key_fp) {
      const ok = el("button", { class: "btn btn-small", type: "button" }, "Approve new key");
      ok.addEventListener("click", busy(ok, async () => {
        if (!window.confirm(`The agent's machine presents a new host key:\n${st.pending_host_key_fp}\n\nApprove it only if you can confirm this fingerprint on the machine (pigeonpost agentd shell). Open sessions end.`)) return;
        await PPE.postbox(`/v1/agents/${encodeURIComponent(a.address)}/shell/host-key`, { method: "POST", body: { fingerprint: st.pending_host_key_fp } });
        PPE.toast("New host key approved"); PPE.closeSheet(); go("agents", true);
      }));
      wrap.append(el("div", { class: "setting" }, el("p", {}, "New host key waiting for approval", el("span", { class: "sub muted mono" }, ` ${st.pending_host_key_fp}`)), ok));
    }
    for (const x of (ssh && ssh.sessions) || []) {
      const who = (x.requester && (x.requester.handle || x.requester.account)) || "someone";
      const row = el("div", { class: "setting" }, el("p", {}, who, el("span", { class: "sub muted" }, ` · ${x.state.replace("_", " ")} · since ${PPE.date(x.created_at)}`)));
      const act = (label, path, method, msg, cls) => {
        const b = el("button", { class: `btn btn-small ${cls || ""}`, type: "button" }, label);
        b.addEventListener("click", busy(b, async () => {
          await PPE.postbox(`/v1/shell/sessions/${encodeURIComponent(x.id)}${path}`, { method, body: method === "POST" ? {} : undefined });
          PPE.toast(msg); PPE.closeSheet(); go("agents", true);
        }));
        return b;
      };
      if (x.state === "awaiting_approval") row.append(act("Approve", "/approve", "POST", "Approved"), act("Deny", "/deny", "POST", "Denied", "btn-quiet"));
      else row.append(act("End", "", "DELETE", "Session ended", "btn-danger"));
      wrap.append(row);
    }
    wrap.append(el("p", { class: "muted" }, "People with permission connect from a terminal:"), secretBox(`pigeonpost ssh ${a.handle}`));
    return wrap;
  }

  // ---- Security --------------------------------------------------------------------------------

  function toggle(sentence, checked, onChange) {
    const input = el("input", { type: "checkbox", role: "switch", checked, "aria-label": sentence });
    input.addEventListener("change", async () => {
      input.disabled = true;
      try { await onChange(input.checked); PPE.toast("Saved"); } catch (e) { input.checked = !input.checked; PPE.fail(e); } finally { input.disabled = false; }
    });
    return el("div", { class: "setting" }, el("p", {}, sentence), el("span", { class: "switch" }, input));
  }
  const patchSettings = (settings) => PPE.postbox(orgPath(""), { method: "PATCH", body: { settings } }).then((r) => { if (r.org) app.org = r.org; });

  async function security() {
    const s = app.org.settings || {};
    const [{ domains = [] }, audit] = await Promise.all([PPE.postbox(orgPath("/domains")), PPE.postbox(orgPath("/audit?limit=50"))]);
    const add = el("button", { class: "btn btn-primary", type: "button", onclick: domainSheet }, "Add domain");
    const domainCards = domains.map((d) => {
      const verify = el("button", { class: "btn btn-small", type: "button" }, "Verify");
      verify.addEventListener("click", busy(verify, async () => {
        const r = await PPE.postbox(orgPath(`/domains/${encodeURIComponent(d.domain)}/verify`), { method: "POST", body: {} });
        PPE.toast(r.verified ? `${d.domain} is verified` : "The record isn't visible yet. DNS can take a while.", r.verified ? "info" : "error"); go("security", true);
      }));
      const remove = el("button", { class: "btn btn-small btn-quiet", type: "button" }, "Remove");
      remove.addEventListener("click", busy(remove, async () => { await PPE.postbox(orgPath(`/domains/${encodeURIComponent(d.domain)}`), { method: "DELETE" }); go("security", true); }));
      return el("div", { class: "card" }, el("h3", {}, d.domain, " ", d.verified_at ? el("span", { class: "chip chip-ok" }, "Verified") : el("span", { class: "chip chip-warn" }, "Not verified")),
        d.verified_at ? el("p", { class: "muted" }, `Verified ${PPE.date(d.verified_at)}; checked again every week.`)
          : el("div", {}, el("p", {}, "Add this TXT record at your DNS provider, then press Verify:"), secretBox(`${d.txt_name || d.domain}  TXT  "${d.txt_value}"`)),
        el("div", { class: "actions" }, remove, d.verified_at ? null : verify));
    });
    const filter = el("input", { type: "search", placeholder: "Filter by action, e.g. member.invite", "aria-label": "Filter audit log" });
    const tbody = el("tbody", {});
    let next = audit.next_before_id || null;
    const fill = (entries, append) => {
      if (!append) tbody.replaceChildren();
      for (const e of entries) tbody.append(el("tr", {}, el("td", {}, new Date(e.at * 1000).toLocaleString()), el("td", {}, e.actor || e.actor_kind || "—"), el("td", { class: "mono" }, e.action), el("td", { class: "mono" }, e.target || "")));
    };
    fill(audit.entries || []);
    const more = el("button", { class: "btn btn-small", type: "button", hidden: !next }, "Load more");
    const query = async (append) => {
      const q = new URLSearchParams({ limit: "50" });
      if (filter.value.trim()) q.set("action", filter.value.trim());
      if (append && next) q.set("before_id", String(next));
      const r = await PPE.postbox(orgPath(`/audit?${q}`));
      next = r.next_before_id || null; more.hidden = !next; fill(r.entries || [], append);
    };
    more.addEventListener("click", busy(more, () => query(true)));
    let t = null;
    filter.addEventListener("input", () => { window.clearTimeout(t); t = window.setTimeout(() => query(false).catch(PPE.fail), 300); });
    const exportBtn = (format) => { const b = el("button", { class: "btn btn-small", type: "button" }, `Export ${format.toUpperCase()}`); b.addEventListener("click", busy(b, () => exportAudit(format))); return b; };
    return el("div", {},
      head("Security", null, add),
      el("div", { class: "card" },
        toggle("Require two-step sign-in for everyone in the organization", Boolean(s.require_mfa), (v) => patchSettings({ require_mfa: v })),
        toggle("Only invite addresses at verified domains", Boolean(s.invite_domains_only), (v) => patchSettings({ invite_domains_only: v })),
        sshOn() ? toggle("Allow SSH to agents (each agent and each person still needs its own permission)", Boolean(s.ssh_allowed), (v) => patchSettings({ ssh_allowed: v })) : null,
        sshOn() ? toggle("Every SSH session needs an owner's, admin's or agent admin's approval", Boolean(s.ssh_requires_approval), (v) => patchSettings({ ssh_requires_approval: v })) : null,
        streamsOn() ? viewerLimit(s) : null),
      el("h2", {}, "Domains"), domains.length ? domainCards : el("p", { class: "muted" }, "No domains yet. Verify your company's domain to restrict invitations to it."),
      app.can("api-keys") ? el("div", { class: "card" }, el("h2", {}, "API keys and webhooks"),
        el("p", { class: "muted" }, "Scoped keys let your own systems manage people, agents, rules and rooms. Webhooks tell them when something changes. Neither can read messages."),
        el("div", { class: "actions" }, pageLink("api-keys", "Manage API keys"), pageLink("webhooks", "Manage webhooks"))) : null,
      el("h2", {}, "Audit log"),
      el("div", { class: "row" }, filter, el("div", { class: "actions", style: null }, exportBtn("csv"), exportBtn("json"))),
      el("div", { class: "table-wrap" }, el("table", {}, el("thead", {}, el("tr", {}, ["When", "Who", "Action", "Target"].map((h) => el("th", { scope: "col" }, h)))), tbody)),
      el("div", { class: "actions" }, more));
  }

  // Viewers per live stream in this organization's rooms (SDS §14A.5). Blank = the server's limit;
  // the server refuses anything above it.
  function viewerLimit(s) {
    const input = el("input", { type: "number", min: "1", step: "1", inputmode: "numeric", placeholder: "Server default", value: s.stream_max_viewers ? String(s.stream_max_viewers) : "" });
    const save = el("button", { class: "btn btn-small", type: "button" }, "Save");
    save.addEventListener("click", busy(save, async () => {
      const raw = input.value.trim();
      const n = raw === "" ? 0 : Number(raw);
      if (!Number.isInteger(n) || n < 0) return PPE.toast("Enter a whole number, or leave it empty for the default.", "error");
      await patchSettings({ stream_max_viewers: n });
      PPE.toast("Saved.");
    }));
    return PPE.field("Viewers per live stream", el("div", { class: "row" }, input, save), "Leave empty to use the server's limit.");
  }

  function domainSheet() {
    PPE.openSheet("Add domain", (body) => {
      const d = el("input", { type: "text", placeholder: "acme.com", spellcheck: "false", autocapitalize: "none" });
      const add = el("button", { class: "btn btn-primary", type: "button" }, "Add domain");
      add.addEventListener("click", busy(add, async () => {
        const v = d.value.trim().toLowerCase().replace(/\.$/, "");
        if (!/^(?=.{4,253}$)([a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]{2,63}$/.test(v)) return PPE.toast("Enter a domain like acme.com.", "error");
        await PPE.postbox(orgPath("/domains"), { method: "POST", body: { domain: v } });
        PPE.closeSheet(); go("security", true);
      }));
      body.append(PPE.field("Domain", d, "You'll add one TXT record to prove you control it."), el("div", { class: "actions" }, add));
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

  function pageLink(page, text) {
    const a = el("a", { class: "btn btn-small", href: `/app/${page}` }, text);
    a.addEventListener("click", (e) => { e.preventDefault(); go(page); });
    return a;
  }

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
      el("div", { class: "secret" }, code, el("button", { class: "btn btn-small", type: "button", onclick: () => { if (held.value) PPE.copy(held.value); } }, "Copy")),
      ...(notes || []).filter(Boolean).map((n) => el("p", { class: "hint" }, n)),
      el("div", { class: "actions" }, el("button", { class: "btn btn-primary", type: "button", onclick: () => PPE.closeSheet() }, "I've stored it"))),
    { onClose: () => { held.value = null; code.textContent = ""; if (after) after(); } });
  }
  app.secretSheet = secretSheet;

  async function apiKeys() {
    const { keys = [] } = await PPE.postbox(orgPath("/api-keys"));
    const create = el("button", { class: "btn btn-primary", type: "button", onclick: () => keySheet() }, "Create key");
    const now = Date.now() / 1000;
    const rows = keys.map((k) => {
      const state = app.keyState(k, now);
      const chip = el("span", { class: `chip ${{ active: "chip-ok", expired: "chip-warn", revoked: "chip-bad" }[state]}` }, { active: "Active", expired: "Expired", revoked: "Revoked" }[state]);
      let actions = null;
      if (state !== "revoked") {
        actions = el("button", { class: "btn btn-small btn-danger", type: "button", "aria-label": `Revoke ${k.name}` }, "Revoke");
        actions.addEventListener("click", busy(actions, async () => {
          if (!window.confirm(`Revoke ${k.name}? Anything using it stops working at once. This can't be undone.`)) return;
          await PPE.postbox(orgPath(`/api-keys/${encodeURIComponent(k.id)}`), { method: "DELETE" });
          PPE.toast("Key revoked"); go("api-keys", true);
        }));
      }
      return el("tr", {},
        el("td", {}, k.name, el("span", { class: "sub" }, k.created_by ? `by ${k.created_by}` : "")),
        el("td", {}, (k.scopes || []).map((s) => el("span", { class: "chip mono" }, s)), k.send_as ? el("span", { class: "sub mono" }, `sends as ${k.send_as}`) : null,
          (k.ip_allowlist || []).length ? el("span", { class: "sub" }, `${k.ip_allowlist.length} allowed network${k.ip_allowlist.length === 1 ? "" : "s"}`) : null),
        el("td", { class: "mono" }, `pk_org_…${k.last4 || ""}`),
        el("td", {}, PPE.date(k.created_at)),
        el("td", {}, k.expires_at ? PPE.date(k.expires_at) : "Never"),
        el("td", {}, k.last_used_at ? PPE.ago(k.last_used_at) : "Never"),
        el("td", {}, chip),
        el("td", { class: "row-actions" }, actions));
    });
    return el("div", {},
      head("API keys", "Keys let your own systems manage this organization. A key never reads messages, and it stops working if the person who made it loses their admin role.", create),
      rows.length ? el("div", { class: "table-wrap" }, el("table", {},
        el("thead", {}, el("tr", {}, ["Name", "Scopes", "Key", "Created", "Expires", "Last used", "Status", ""].map((h) => el("th", { scope: "col" }, h)))),
        el("tbody", {}, rows))) : el("div", { class: "empty card" }, "No keys yet. Create one for each system that needs access."),
      el("p", { class: "note" }, "Creating a key needs a fresh, strong sign-in. Each key is shown once; if it's lost, revoke it and create another."));
  }

  async function keySheet() {
    let agentList = [];
    try { agentList = ((await PPE.postbox(orgPath("/agents"))).agents || []).filter((a) => a.status === "active"); } catch (e) { if (e instanceof PPE.Redirecting) throw e; }
    PPE.openSheet("Create key", (body) => {
      const name = el("input", { type: "text", maxlength: 80, placeholder: "HR sync", autocomplete: "off" });
      const scopes = SCOPES.map(([s, text]) => el("label", { class: "check" }, el("input", { type: "checkbox", value: s }), el("span", {}, el("strong", { class: "mono" }, s), el("span", { class: "sub muted" }, text))));
      const sendAs = el("select", {}, el("option", { value: "" }, agentList.length ? "Choose an agent…" : "No active agents"),
        agentList.map((a) => el("option", { value: a.handle }, a.label ? `${a.handle} (${a.label})` : a.handle)));
      const sendAsField = PPE.field("Send messages as", sendAs, "Messages this key sends come from this agent's address. Only direct messages to people and agents here.");
      const sendBox = scopes[SCOPES.findIndex(([s]) => s === "messages:send")].querySelector("input");
      sendAsField.hidden = true;
      sendBox.addEventListener("change", () => { sendAsField.hidden = !sendBox.checked; });
      const days = el("input", { type: "number", min: 1, max: 730, step: 1, inputmode: "numeric", placeholder: "Never" });
      const allow = el("textarea", { spellcheck: "false", autocapitalize: "none", placeholder: "203.0.113.0/24\n2001:db8::/32" });
      const status = el("p", { class: "status-line", role: "status" });
      const bad = (text) => { status.className = "status-line status-bad"; status.textContent = text; };
      const make = el("button", { class: "btn btn-primary", type: "button" }, "Create key");
      make.addEventListener("click", busy(make, async () => {
        const chosen = scopes.map((l) => l.querySelector("input")).filter((c) => c.checked).map((c) => c.value);
        const ips = allow.value.split(/\r?\n/).map((l) => l.trim()).filter(Boolean);
        if (!name.value.trim()) return bad("Give the key a name, so people know what uses it.");
        if (!chosen.length) return bad("Choose at least one scope.");
        if (chosen.includes("messages:send") && !sendAs.value) return bad("Choose the agent this key sends messages as.");
        if (days.value.trim() && !(/^\d+$/.test(days.value.trim()) && Number(days.value) >= 1 && Number(days.value) <= 730)) return bad("Expiry is a whole number of days, from 1 to 730.");
        if (ips.length > 20) return bad("At most 20 allowed networks.");
        const wrong = ips.find((v) => !app.looksLikeCidr(v));
        if (wrong) return bad(`${wrong} doesn't look like an address or network (for example 203.0.113.0/24).`);
        const req = { name: name.value.trim(), scopes: chosen,
          ...(days.value.trim() ? { expires_in_days: Number(days.value) } : {}),
          ...(ips.length ? { ip_allowlist: ips } : {}),
          ...(chosen.includes("messages:send") ? { send_as: sendAs.value } : {}) };
        status.className = "status-line"; status.textContent = "";
        try {
          await createKey(req);
        } catch (e) {
          if (e && e.code === "step_up_unavailable") return bad("Strong sign-in isn't configured on this server, so keys can't be created.");
          throw e;
        }
      }));
      body.append(PPE.field("Name", name),
        el("fieldset", { class: "card" }, el("legend", {}, "Scopes: what the key may do"), ...scopes),
        sendAsField,
        PPE.field("Expires after (days, optional)", days, "From 1 to 730. Leave empty for a key that doesn't expire."),
        PPE.field("Allowed networks (optional)", allow, "One address or CIDR range per line, at most 20. Requests from anywhere else are refused."),
        el("p", { class: "hint" }, "You'll be asked to sign in again with a strong method before the key is made."),
        status, el("div", { class: "actions" }, el("button", { class: "btn", type: "button", onclick: () => PPE.closeSheet() }, "Cancel"), make));
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
      () => go("api-keys", true));
  }

  // ---- Webhooks (Phase 5, docs/org-api.md §6) --------------------------------------------------

  function deliveryChip(status) {
    const kind = { delivered: "chip-ok", retrying: "chip-warn", failed: "chip-bad", blocked: "chip-bad" }[status] || "";
    const text = { delivered: "Delivered", retrying: "Retrying", failed: "Failed", blocked: "Blocked" }[status] || status;
    return el("span", { class: `chip ${kind}` }, text);
  }
  const eventsText = (events) => ((events || []).includes("*") ? "All events" : (events || []).join(", "));

  async function webhooks() {
    const { webhooks: list = [] } = await PPE.postbox(orgPath("/webhooks"));
    const add = el("button", { class: "btn btn-primary", type: "button", onclick: () => webhookSheet() }, "Add webhook");
    if (list.length >= 10) add.disabled = true;
    const rows = list.map((w) => {
      const d = w.last_delivery;
      return el("tr", {},
        el("td", { class: "mono" }, w.url, w.description ? el("span", { class: "sub" }, w.description) : null),
        el("td", {}, eventsText(w.events)),
        el("td", {}, d ? [deliveryChip(d.status), el("span", { class: "sub" }, `${d.response_status ? `HTTP ${d.response_status} · ` : ""}${PPE.ago(d.at)}`)] : el("span", { class: "muted" }, "Nothing sent yet")),
        el("td", { class: "row-actions" }, el("button", { class: "btn btn-small", type: "button", "aria-label": `Manage ${w.url}`, onclick: () => manageWebhook(w) }, "Manage")));
    });
    return el("div", {},
      head("Webhooks", "Pigeonpost calls these addresses when people, agents, rooms or billing change. Deliveries carry ids and addresses, never message content.", add),
      rows.length ? el("div", { class: "table-wrap" }, el("table", {},
        el("thead", {}, el("tr", {}, ["Endpoint", "Events", "Last delivery", ""].map((h) => el("th", { scope: "col" }, h)))),
        el("tbody", {}, rows))) : el("div", { class: "empty card" }, "No webhooks yet."),
      el("p", { class: "note" }, "Every delivery is signed with HMAC-SHA256 in the Pigeonpost-Signature header. Check it with your signing secret, reject old timestamps and ignore repeated event ids. ",
        el("a", { href: WEBHOOK_DOCS, rel: "noopener" }, "How to verify signatures")));
  }

  function webhookSheet() {
    PPE.openSheet("Add webhook", (body) => {
      const url = el("input", { type: "url", maxlength: 2048, spellcheck: "false", autocapitalize: "none", placeholder: "https://hooks.example.com/pigeonpost" });
      const all = el("input", { type: "checkbox", value: "*" });
      const boxes = EVENTS.map(([ev, text]) => el("label", { class: "check" }, el("input", { type: "checkbox", value: ev }), el("span", {}, el("span", { class: "mono" }, ev), el("span", { class: "sub muted" }, text))));
      all.addEventListener("change", () => { for (const b of boxes) { const c = b.querySelector("input"); c.disabled = all.checked; } });
      const desc = el("input", { type: "text", maxlength: 200, placeholder: "HR system" });
      const status = el("p", { class: "status-line", role: "status" });
      const bad = (text) => { status.className = "status-line status-bad"; status.textContent = text; };
      const save = el("button", { class: "btn btn-primary", type: "button" }, "Add webhook");
      save.addEventListener("click", busy(save, async () => {
        const v = url.value.trim();
        let ok = false;
        try { const u = new URL(v); ok = u.protocol === "https:" && !u.username && !u.password && !u.hash; } catch { ok = false; }
        if (!ok) return bad("Use an https:// address without a user name or #fragment.");
        const events = all.checked ? ["*"] : boxes.map((b) => b.querySelector("input")).filter((c) => c.checked).map((c) => c.value);
        if (!events.length) return bad("Choose at least one event, or All events.");
        const r = await PPE.postbox(orgPath("/webhooks"), { method: "POST", body: { url: v, events, ...(desc.value.trim() ? { description: desc.value.trim() } : {}) } });
        secretSheet("Webhook signing secret", r.secret, [`Deliveries to ${(r.webhook && r.webhook.url) || v} are signed with it.`], () => go("webhooks", true));
      }));
      body.append(PPE.field("Endpoint address", url, "Must be https:// on port 443 or 8443, and reachable on the public internet."),
        el("fieldset", { class: "card" }, el("legend", {}, "Events"),
          el("label", { class: "check" }, all, el("span", {}, el("strong", {}, "All events"), el("span", { class: "sub muted" }, "Including ones added later."))), ...boxes),
        PPE.field("Description (optional)", desc),
        status, el("div", { class: "actions" }, el("button", { class: "btn", type: "button", onclick: () => PPE.closeSheet() }, "Cancel"), save));
    });
  }

  function manageWebhook(w) {
    PPE.openSheet(w.description || "Webhook", (body) => {
      body.append(el("dl", { class: "kv" },
        el("dt", {}, "Endpoint"), el("dd", { class: "mono" }, w.url),
        el("dt", {}, "Events"), el("dd", {}, eventsText(w.events)),
        el("dt", {}, "Created"), el("dd", {}, `${PPE.date(w.created_at)}${w.created_by ? ` by ${w.created_by}` : ""}`),
        el("dt", {}, "Secret rotated"), el("dd", {}, w.rotated_at ? PPE.date(w.rotated_at) : "Never")),
      w.previous_secret_expires_at && w.previous_secret_expires_at > Date.now() / 1000
        ? el("p", { class: "note" }, `The previous secret also signs deliveries until ${new Date(w.previous_secret_expires_at * 1000).toLocaleString()}.`) : null);
      const deliveries = el("button", { class: "btn", type: "button" }, "Recent deliveries");
      deliveries.addEventListener("click", busy(deliveries, () => deliveriesSheet(w)));
      const rotate = el("button", { class: "btn", type: "button" }, "Rotate secret");
      rotate.addEventListener("click", busy(rotate, async () => {
        if (!window.confirm("Make a new signing secret? The current one keeps signing deliveries for 24 hours, so you can switch over.")) return;
        const r = await PPE.postbox(orgPath(`/webhooks/${encodeURIComponent(w.id)}/rotate`), { method: "POST", body: {} });
        const until = r.previous_secret_expires_at ? new Date(r.previous_secret_expires_at * 1000).toLocaleString() : "24 hours from now";
        secretSheet("New signing secret", r.secret, [`The old secret keeps working for 24 hours, until ${until}. Deliveries carry both signatures until then.`], () => go("webhooks", true));
      }));
      const del = el("button", { class: "btn btn-danger", type: "button" }, "Delete webhook");
      del.addEventListener("click", busy(del, async () => {
        if (!window.confirm(`Delete the webhook for ${w.url}? Pending deliveries are cancelled.`)) return;
        await PPE.postbox(orgPath(`/webhooks/${encodeURIComponent(w.id)}`), { method: "DELETE" });
        PPE.closeSheet(); PPE.toast("Webhook deleted"); go("webhooks", true);
      }));
      body.append(el("div", { class: "actions" }, deliveries, rotate, del));
    });
  }

  async function deliveriesSheet(w) {
    const { deliveries = [] } = await PPE.postbox(orgPath(`/webhooks/${encodeURIComponent(w.id)}/deliveries?limit=50`));
    PPE.openSheet("Recent deliveries", (body) => {
      body.append(el("p", { class: "muted mono" }, w.url));
      if (!deliveries.length) body.append(el("p", { class: "muted" }, "Nothing has been sent yet."));
      const list = el("ul", { class: "deliveries" }, deliveries.map((d) => el("li", { class: "setting" },
        el("p", {}, el("span", { class: "mono" }, d.event_type), " ", deliveryChip(d.status),
          el("span", { class: "sub muted" }, [new Date(d.at * 1000).toLocaleString(), `attempt ${d.attempt}`, d.response_status ? `HTTP ${d.response_status}` : "no answer",
            d.duration_ms != null ? `${d.duration_ms} ms` : null].filter(Boolean).join(" · ")),
          d.error ? el("span", { class: "sub" }, d.error) : null))));
      body.append(list, el("div", { class: "actions" }, el("button", { class: "btn", type: "button", onclick: () => manageWebhook(w) }, "Back")));
    });
  }

  // ---- Organization ----------------------------------------------------------------------------

  async function organization() {
    const o = app.org;
    const name = el("input", { type: "text", maxlength: 120, value: o.name });
    const save = el("button", { class: "btn btn-primary", type: "button" }, "Save");
    save.addEventListener("click", busy(save, async () => {
      const r = await PPE.postbox(orgPath(""), { method: "PATCH", body: { name: name.value.trim() } });
      if (r.org) app.org = r.org; PPE.toast("Saved"); go("organization", true);
    }));
    const rc = el("select", { "aria-label": "Who can create rooms" }, el("option", { value: "everyone" }, "everyone"), el("option", { value: "admins" }, "admins only"));
    rc.value = (o.settings && o.settings.rooms_creation) || "everyone";
    rc.addEventListener("change", async () => { try { await patchSettings({ rooms_creation: rc.value }); PPE.toast("Saved"); } catch (e) { PPE.fail(e); } });
    const parts = [head("Organization", null, save),
      el("div", { class: "card" }, PPE.field("Name", name),
        el("dl", { class: "kv" }, el("dt", {}, "Address"), el("dd", { class: "mono" }, `/${o.namespace}`), el("dt", {}, "Status"), el("dd", {}, chipFor(o.status)), el("dt", {}, "Created"), el("dd", {}, PPE.date(o.created_at))),
        el("p", { class: "hint" }, "The address can't change after creation.")),
      el("div", { class: "card" }, el("div", { class: "setting" }, el("p", {}, "Rooms can be created by ", rc)))];
    if (has("owner")) {
      const confirm = el("input", { type: "text", spellcheck: "false", autocapitalize: "none", "aria-label": `Type ${o.namespace} to confirm` });
      const del = el("button", { class: "btn btn-danger", type: "button", disabled: true }, "Delete organization");
      confirm.addEventListener("input", () => { del.disabled = confirm.value.trim() !== o.namespace; });
      del.addEventListener("click", busy(del, () => deleteOrg(confirm.value.trim())));
      parts.push(el("div", { class: "card" }, el("h2", {}, "Delete organization"),
        el("p", {}, "Mail to every company address stops at once. People keep their personal accounts; their company mailboxes are removed. Agents are disconnected. The address can be recovered for 30 days."),
        PPE.field(`Type ${o.namespace} to confirm`, confirm), el("div", { class: "actions" }, del)));
    }
    return el("div", {}, ...parts);
  }

  PPE.onResume("app.delete", async (p) => deleteOrg(p.confirm));
  async function deleteOrg(confirmNamespace) {
    await PPE.postbox(orgPath(""), { method: "DELETE", body: { confirm_namespace: confirmNamespace }, resume: { name: "app.delete", payload: { confirm: confirmNamespace } } });
    PPE.store.del("ppe_org");
    gate(el("h1", {}, "Organization deleted"), el("p", { class: "muted" }, "Company mail has stopped. The address can be recovered for 30 days."), el("a", { class: "btn", href: "/" }, "Done"));
  }

  const PAGE = { people, agents, rooms, rules, billing, security, "api-keys": apiKeys, webhooks, organization };

  // ---- load ------------------------------------------------------------------------------------

  async function load(id) {
    const r = await PPE.postbox(`/v1/orgs/${encodeURIComponent(id)}`);
    app.org = r.org;
    app.me = r.me || {};
    app.members = null;
    PPE.store.set("ppe_org", id);
  }

  /// Meter widths without inline styles (the CSP allows none): applied after render.
  new window.MutationObserver(() => {
    for (const s of PPE.$$("[data-w]")) { s.style.width = s.dataset.w; s.removeAttribute("data-w"); }
  }).observe(document.body, { childList: true, subtree: true });

  async function init() {
    PPE.renderAccount();
    if (!PPE.signedIn()) {
      gate(el("h1", {}, "Sign in to manage your organization"), el("p", { class: "muted" }, "Owners and admins manage people, agents and rooms here."),
        el("div", { class: "actions" }, el("a", { class: "btn", href: "/" }, "See packages"), el("button", { class: "btn btn-primary", type: "button", onclick: () => PPE.signIn({ returnTo: window.location.pathname + window.location.search }) }, "Sign in")));
      return;
    }
    try {
      const caps = await PPE.postbox("/v1/capabilities");
      app.features = Array.isArray(caps.features) ? caps.features : Object.keys(caps.features || {});
      if (!app.features.includes("enterprise_portal") || !app.features.includes("orgs")) {
        gate(el("h1", {}, "Coming soon"), el("p", { class: "muted" }, "Organizations are opening gradually."), el("a", { class: "btn", href: "/" }, "See packages"));
        return;
      }
      const { orgs = [] } = await PPE.postbox("/v1/orgs");
      app.orgs = orgs;
      const admins = orgs.filter((o) => (o.my_roles || []).some((r) => ADMIN.includes(r)));
      if (!admins.length) {
        if (orgs.length) {
          gate(el("h1", {}, "Your company workspace is in the inbox"), el("p", { class: "muted" }, "Rooms, people and your company address are all there. This page is for admins."),
            el("div", { class: "actions" }, el("a", { class: "btn btn-primary", href: cfg.inbox || "https://inbox.pigeonpost.dev" }, "Open your inbox")));
        } else {
          gate(el("h1", {}, "No organization yet"), el("p", { class: "muted" }, "Create one for your team, or ask your admin for an invitation."),
            el("div", { class: "actions" }, el("a", { class: "btn btn-primary", href: "/" }, "Create an organization")));
        }
        return;
      }
      const q = new URLSearchParams(window.location.search);
      const wanted = q.get("org") || PPE.store.get("ppe_org");
      const pick = admins.find((o) => o.id === wanted) || admins[0];
      await load(pick.id);
      document.getElementById("gate").hidden = true;
      document.getElementById("shell").hidden = false;
      const page = (window.location.pathname.split("/")[2] || "").replace(/[^a-z-]/g, "");
      await go(PAGE[page] ? page : "people", true);
      await PPE.runResume().catch(PPE.fail);
    } catch (e) {
      if (e instanceof PPE.Redirecting) return;
      gate(el("h1", {}, "Couldn't load your organization"), el("p", { class: "muted" }, PPE.explain(e)), el("button", { class: "btn", type: "button", onclick: () => window.location.reload() }, "Try again"));
    }
  }
  window.addEventListener("popstate", () => { const page = window.location.pathname.split("/")[2]; if (app.org && PAGE[page]) go(page, true); });
  app.init = init;
  init();
})();
