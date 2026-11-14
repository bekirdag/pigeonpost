// management.pigeonpost.dev — System (staff & roles, audit log), and the areas whose postbox
// routes don't exist yet (Nests, calls, limits, trust & safety reports, health, feature flags).
(function () {
  "use strict";
  const PP = window.PP, M = window.PPM, ui = PP.ui;
  const el = (...a) => PP.el(...a);
  const enc = encodeURIComponent;

  // ---- not live yet --------------------------------------------------------------------------

  const SOON = [
    { key: "nests", title: "Nests", group: "Infrastructure", icon: "nest", roles: ["ops"], text: "Every Nest: owner, package, host and state.",
      why: "The postbox has no staff route that lists Nests yet. Running Nests are counted on the Overview, and each account's Nests show on its Accounts page.",
      holds: ["Nests list with owner, package, host (nests-1) and state", "Start, stop and restore actions with a reason", "Backup status from the borg repository"] },
    { key: "calls", title: "Calls & streams", group: "Infrastructure", icon: "server", roles: ["ops"], text: "Live calls, SFU rooms and group streams.",
      why: "Call and SFU metrics are not exposed to staff routes yet; the LiveKit and relay dashboards on the servers are the source for now.",
      holds: ["Active rooms and participants (no media, no content)", "Relay and SFU health", "Push delivery failures"] },
    { key: "limits", title: "Usage limits", group: "Infrastructure", icon: "rules", roles: ["ops"], text: "Rate limits and quotas across the platform.",
      why: "Limits are configured on the postbox host today and have no read route. Publisher quotas are set under Marketplace → Apps & releases.",
      holds: ["Per-account and per-org rate limits", "Temporary raises with an expiry", "Who changed what, from the audit log"] },
    { key: "reports", title: "Reports", group: "Trust & safety", icon: "flag", roles: ["support"], text: "Abuse and spam reports from people and agents.",
      why: "Spam and abuse reports are stored per mailbox and have no staff route yet. Buyer reports on marketplace listings are live under Marketplace → Listings & reports.",
      holds: ["Reports by mailbox and sender, never message bodies", "Blocks and sender reputation", "Decisions with a reason, recorded in the audit log"] },
    { key: "health", title: "Health", group: "System", icon: "server", roles: ["ops"], text: "Component status for the whole platform.",
      why: "status.pigeonpost.dev (plan §5.7) will publish the health feed; this page will read the same feed when it exists.",
      holds: ["Postbox, auth, calls, Nests, payments and sites", "Incidents and their timelines", "Links to status.pigeonpost.dev"] },
    { key: "flags", title: "Feature flags", group: "System", icon: "flag", roles: ["owner", "ops"], text: "Rollout switches per account, org or platform.",
      why: "Flags live in the postbox configuration and environment today; there is no route to read or change them safely yet.",
      holds: ["Every flag with its scope and state", "Per-account and per-org overrides", "Changes recorded with a reason"] },
  ];

  // ---- Staff & roles -------------------------------------------------------------------------

  M.page({
    key: "staff", title: "Staff & roles", group: "System", icon: "key", allow: M.allow(["owner"], { ownerOnly: true }),
    render: (view) => M.guard(view, async () => {
      view.append(ui.header({ title: "Staff & roles", text: "Who holds which staff role. Only the platform owner grants or revokes; every change is recorded." }));
      const list = el("div");
      const draw = async () => {
        list.replaceChildren(ui.loading(3));
        let rows;
        try { rows = (await M.get("/v1/staff/roles")).data || []; } catch (e) { if (!(e instanceof PP.Redirecting)) list.replaceChildren(M.errorBox(e, draw)); return; }
        list.replaceChildren(ui.table({
          rows, pageSize: 100, caption: "Staff roles",
          empty: { icon: "key", title: "No staff role rows yet", text: "The platform owner holds owner without one." },
          columns: [
            { key: "account_id", label: "Account", sort: true, render: (r) => M.link(r.account_id, "accounts", r.account_id) },
            { key: "role", label: "Role", sort: true, render: (r) => ui.badge(r.role, "info") },
            { key: "granted_by", label: "Granted by", mono: true },
            { key: "granted_at", label: "Since", render: (r) => M.day(r.granted_at) },
            { key: "id", label: "", render: (r) => ui.button("Revoke", { size: "sm", kind: "danger", onclick: async () => {
              if (!(await ui.confirm({ title: `Revoke ${r.role}?`, text: `${r.account_id} loses the ${r.role} role straight away. This is recorded.`, confirm: "Revoke", danger: true }))) return;
              try { await M.api(`/v1/staff/roles/${enc(r.id)}`, { method: "DELETE" }); PP.toast(`Revoked ${r.role}.`, "success"); draw(); }
              catch (e) { if (M.isStepUp(e)) list.prepend(M.stepUpCard(e, { compact: true })); else PP.toast(M.explain(e), "error"); }
            } }) },
          ],
        }));
      };
      view.append(ui.section({ title: "Roles" }, list));
      view.append(ui.section({ title: "Grant a role" }, ui.card({}, M.form([
        { name: "account_id", label: "Account id", required: true, placeholder: "acct_…" },
        { name: "role", label: "Role", type: "select", value: "support", options: M.ROLES.map((r) => [r, r]) },
      ], { submit: "Grant", cls: "pm-form-inline", onSubmit: async (v) => { await M.post("/v1/staff/roles", { account_id: v.account_id, role: v.role }); draw(); return `Granted ${v.role}.`; } }),
      ui.kv(M.ROLES.map((r) => [r, M.ROLE_TEXT[r]])))));
      await draw();
    }),
  });

  // ---- Audit log -----------------------------------------------------------------------------

  M.page({
    key: "audit", title: "Audit log", group: "System", icon: "audit", allow: M.allow(["owner"]),
    render: (view) => M.guard(view, async () => {
      view.append(ui.header({ title: "Audit log", text: "Every recorded action on the platform, newest first. Reading it is recorded too." }));
      const st = { rows: [], next: null, filters: {} };
      const out = el("div", { "aria-live": "polite" });
      const more = el("div", { class: "pm-more" });
      const params = (extra) => {
        const p = new URLSearchParams();
        const f = st.filters;
        for (const k of ["actor", "action", "target"]) if (f[k]) p.set(k, f[k]);
        if (f.from) p.set("from", f.from);
        if (f.to) p.set("to", String(Math.floor(new Date(f.to + "T23:59:59Z").getTime() / 1000)));
        for (const [k, v] of Object.entries(extra || {})) p.set(k, v);
        return p;
      };
      const load = async (reset) => {
        if (reset) { st.rows = []; st.next = null; out.replaceChildren(ui.loading(4)); }
        more.replaceChildren(el("span", { class: "pp-muted" }, "Loading…"));
        try {
          const v = await M.get(`/v1/staff/audit?${params(st.next ? { cursor: st.next, limit: "50" } : { limit: "50" })}`);
          st.rows = st.rows.concat(v.data || []);
          st.next = v.next_cursor || null;
        } catch (e) {
          if (e instanceof PP.Redirecting) return;
          more.replaceChildren();
          out.replaceChildren(M.errorBox(e, () => load(true)));
          return;
        }
        out.replaceChildren(M.auditTable(st.rows, "No records match"));
        more.replaceChildren(st.next ? ui.button("Load more", { size: "sm", onclick: () => load(false) }) : null, el("span", { class: "pp-muted pm-count" }, `${st.rows.length}${st.next ? "+" : ""} shown`));
      };
      const exportCsv = async () => {
        try {
          const v = await M.get(`/v1/staff/audit?${params({ export: "1", limit: "500" })}`);
          M.saveCsv(`pigeonpost-audit-${new Date().toISOString().slice(0, 10)}.csv`, [["id", "at", "actor_account", "actor_kind", "action", "target", "org_id", "details"]]
            .concat((v.data || []).map((r) => [r.id, r.at, r.actor_account, r.actor_kind, r.action, r.target, r.org_id, JSON.stringify(M.maskDeep(r.details))])));
          PP.toast(`Exported ${(v.data || []).length} records.`, "success");
        } catch (e) { if (M.isStepUp(e)) out.prepend(M.stepUpCard(e, { compact: true })); else PP.fail(e); }
      };
      view.append(ui.card({}, ui.form([
        { name: "actor", label: "Actor", placeholder: "acct_…" },
        { name: "action", label: "Action", placeholder: "staff. or nest.create" },
        { name: "target", label: "Target", placeholder: "account: or org:org_…" },
        { name: "from", label: "From", type: "date" },
        { name: "to", label: "To", type: "date" },
      ], { submit: "Filter", cls: "pm-form-inline pm-audit-filters", secondary: ui.button("Export CSV", { icon: "docs", onclick: exportCsv }), onSubmit: (v) => { st.filters = v; load(true); } })), out, more);
      await load(true);
    }),
  });

  for (const s of SOON) M.page({ key: s.key, title: s.title, group: s.group, icon: s.icon, tag: "soon", allow: M.allow(s.roles), render: M.soon(s) });
})();
