// management.pigeonpost.dev — Partners: the application queue and its review workspace, the
// business directory (with payout details for finance) and partner analytics.
//
// Calls (all existing postbox routes):
//   GET  /v1/staff/publisher-dossiers?status=&q=&kind=&country=&sort=&offset=
//   GET  /v1/staff/publisher-dossiers/{id}
//   GET  /v1/staff/publisher-dossiers/{id}/documents/{doc}          (base64 JSON, downloaded)
//   POST /v1/staff/publisher-dossiers/{id}/review   {revision, decision, reason, requested_documents}
//   POST /v1/staff/publishers/{id}/revocation       {reason}
//   GET  /v1/staff/roles                            to count reviewers (single-reviewer note)
//   GET  /v1/staff/publishers/{id}/payout-profile   (finance)
//   GET  /v1/staff/publisher-analytics?q=&offset=   GET /v1/staff/publishers/{id}/analytics?from=&to=&listing_id=
(function () {
  "use strict";
  const PP = window.PP, M = window.PPM, ui = PP.ui;
  const el = (...a) => PP.el(...a);
  const enc = encodeURIComponent;

  const STATES = { draft: "Draft", submitted: "Submitted", in_review: "In review", needs_information: "Waiting on applicant", approved: "Approved", rejected: "Rejected", revoked: "Revoked" };
  M.APP_STATES = STATES;
  const FIELDS = {
    legal_name: "Full legal name", registration_number: "Company registration number", tax_id: "Tax identification number", duns: "DUNS number", duns_unavailable_reason: "Why no DUNS number",
    address: "Registered / residential address", contact_name: "Authorized contact", contact_email: "Contact email",
    bank_name: "Bank name", account_holder: "Account holder", iban: "IBAN", account_number: "Account number", swift: "SWIFT / BIC", currency: "Payout currency", bank_country: "Bank country",
    public_name: "Public name", public_summary: "About their work", website: "Website",
  };
  const GROUPS = [
    ["Legal", ["legal_name", "registration_number", "tax_id", "duns", "duns_unavailable_reason", "address"]],
    ["Contact", ["contact_name", "contact_email"]],
    ["Bank", ["bank_name", "account_holder", "iban", "account_number", "swift", "currency", "bank_country"]],
    ["Public profile", ["public_name", "public_summary", "website"]],
  ];
  const DOC_CATEGORIES = [["registration", "Registration"], ["tax", "Tax"], ["bank", "Bank"], ["authorization", "Authorization"], ["additional", "Additional"]];
  const region = (c) => { try { return new Intl.DisplayNames([PP.locale()], { type: "region" }).of(c); } catch { return c; } };

  /// How many people hold the reviewer role (null when the roles list isn't readable).
  M.reviewerCount = async function () {
    try { const r = await M.get("/v1/staff/roles"); return new Set((r.data || []).filter((x) => x.role === "reviewer").map((x) => x.account_id)).size; } catch { return null; }
  };
  /// The single-reviewer note: with one reviewer, every decision is a single-person approval.
  M.singleNote = (count) => (count !== null && count <= 1
    ? ui.gate({ text: "Single-person approval: you are the only reviewer, so nobody else checks this decision. It is logged as a single-person override.", tone: "warning" })
    : el("p", { class: "pp-muted pm-note" }, "Your decision is logged under your name."));

  /// What a reviewer should check before deciding, from the dossier itself.
  M.checklist = function (a) {
    const d = a.details || {};
    const cats = new Set((a.documents || []).map((x) => x.category));
    const company = a.kind === "company";
    const requested = a.requested_documents || [];
    return [
      { title: "Legal identity", text: company ? "Legal name, registration number and address" : "Legal name and address", done: Boolean(d.legal_name && d.address && (!company || d.registration_number)) },
      { title: "Tax id", text: "Tax identification number present", done: Boolean(d.tax_id) },
      company ? { title: "DUNS", text: "A DUNS number, or a reason why there is none", done: Boolean(d.duns || d.duns_unavailable_reason) } : null,
      { title: company ? "Company papers" : "Identity check", text: company ? "Registration and tax documents uploaded" : `KYC: ${String(a.kyc_status || "not started").replace(/_/g, " ")}`, done: company ? cats.has("registration") && cats.has("tax") : a.kyc_status === "approved" || a.kyc_status === "verified" },
      { title: "Bank details", text: "Bank, holder and IBAN or account number", done: Boolean(d.bank_name && d.account_holder && (d.iban || d.account_number)) },
      { title: "Public profile", text: "Name and summary for the partner directory", done: Boolean(d.public_name && d.public_summary) },
      requested.length ? { title: "Requested papers", text: `Asked for: ${requested.join(", ")}`, done: requested.every((c) => cats.has(c)) } : null,
    ].filter(Boolean);
  };

  // ---- Applications --------------------------------------------------------------------------

  M.page({
    key: "applications", title: "Applications", group: "Partners", icon: "form", allow: M.allow(["reviewer"]),
    count: (ctx) => (ctx.counts && ctx.counts.publisher_applications) || null,
    render: (view, ctx, [id]) => M.guard(view, () => (id ? appDetail(view, id) : appQueue(view))),
  });

  const queueState = { status: "submitted", q: "", kind: "", country: "", sort: "oldest", offset: 0 };

  async function appQueue(view) {
    view.append(ui.header({ title: "Applications", text: "Publisher applications, oldest first. Open one to read the dossier, download papers privately and decide." }));
    const st = queueState;
    const host = el("div");
    const chips = M.chips({
      label: "Status", value: st.status, onChange: (v) => { st.status = v; st.offset = 0; load(); },
      options: [["submitted", "Submitted"], ["in_review", "In review"], ["needs_information", "Waiting on applicant"], ["approved", "Approved"], ["rejected", "Rejected"], ["revoked", "Revoked"], ["draft", "Drafts"], ["all", "All"]],
    });
    const form = ui.form([
      { name: "q", label: "Search", placeholder: "Publisher name or application id", value: st.q, maxlength: 100 },
      { name: "kind", label: "Applicant type", type: "select", value: st.kind, options: [["", "All types"], ["company", "Company"], ["individual", "Individual"]] },
      { name: "country", label: "Country (ISO code)", value: st.country, maxlength: 2, pattern: "[A-Za-z]{2}", placeholder: "TR" },
      { name: "sort", label: "Sort", type: "select", value: st.sort, options: [["oldest", "Oldest first"], ["newest", "Recently updated"], ["name", "Publisher name"]] },
    ], { submit: "Apply", cls: "pm-form-inline pm-app-filters", onSubmit: (v) => { Object.assign(st, { q: v.q, kind: v.kind, country: v.country.toUpperCase(), sort: v.sort, offset: 0 }); load(); } });
    view.append(el("div", { class: "pm-toolbar" }, chips), form, host);

    async function load() {
      host.replaceChildren(ui.loading(4));
      let r;
      try { r = await M.get(`/v1/staff/publisher-dossiers?${M.qs({ status: st.status, q: st.q, kind: st.kind, country: st.country, sort: st.sort, offset: st.offset })}`); } catch (e) {
        if (e instanceof PP.Redirecting) return;
        host.replaceChildren(M.errorBox(e, load));
        return;
      }
      const rows = r.data || [];
      if (!rows.length) {
        host.replaceChildren(ui.empty({ icon: "form", title: st.status === "submitted" ? "No applications waiting" : "No applications match", text: st.status === "submitted" ? "New submissions appear here. Check “Waiting on applicant” for ones you asked for more papers." : "Try another status or clear the search." }));
        return;
      }
      host.replaceChildren(
        el("p", { class: "pp-muted pm-count" }, `${M.num(r.total)} application${r.total === 1 ? "" : "s"}`),
        ui.table({
          caption: "Applications", pageSize: 1000, rows,
          rowHref: (a) => (a.conflict ? PP.href("applications") : PP.href("applications", a.id)),
          columns: [
            { key: "display_name", label: "Publisher", sort: true, render: (a) => el("div", { class: "pm-stack" }, el("strong", {}, a.display_name), M.mono(a.id)) },
            { key: "kind", label: "Type", sort: true, render: (a) => (a.kind ? ui.badge(a.kind) : "—") },
            { key: "country", label: "Country", sort: true, render: (a) => (a.country ? region(a.country) : "—") },
            { key: "state", label: "Status", sort: true, render: (a) => M.status(a.state, STATES[a.state]) },
            { key: "kyc_status", label: "KYC", render: (a) => String(a.kyc_status || "—").replace(/_/g, " ") },
            { key: "updated_at", label: "Updated", sort: (a) => String(a.updated_at || ""), render: (a) => M.day(a.updated_at) },
            { key: "reviewer", label: "Reviewer", render: (a) => (a.reviewer ? M.mono(a.reviewer) : "—") },
            { key: "id", label: "", render: (a) => (a.conflict ? ui.badge("Conflict of interest", "danger") : M.link("Open", "applications", a.id)) },
          ],
        }),
        r.next_offset != null || st.offset ? el("div", { class: "pm-more" },
          ui.button("Previous", { size: "sm", disabled: st.offset === 0, onclick: () => { st.offset = Math.max(0, st.offset - 25); load(); } }),
          ui.button("Next", { size: "sm", disabled: r.next_offset == null, onclick: () => { st.offset = r.next_offset; load(); } })) : null);
    }
    await load();
  }

  async function appDetail(view, id) {
    const head = ui.header({ eyebrow: "Application", title: id, text: "Private staff workspace. Bank and tax numbers are masked; papers download straight to this device.", actions: ui.button("Back to the queue", { href: PP.href("applications") }) });
    view.append(head);
    const reviewers = M.reviewerCount();
    await M.detail(view, `/v1/staff/publisher-dossiers/${enc(id)}`, async (box, a) => {
      const d = a.details || {};
      const name = d.public_name || d.legal_name || a.id;
      head.querySelector("h1").textContent = name;
      PP.shell.crumb(name);
      box.append(ui.stats([
        { label: "Status", value: STATES[a.state] || a.state, tone: a.state === "approved" ? "success" : ["rejected", "revoked"].includes(a.state) ? "danger" : "warning" },
        { label: "Applicant", value: a.kind === "company" ? "Company" : a.kind === "individual" ? "Individual" : "—", hint: a.country ? region(a.country) : null },
        { label: "Papers", value: (a.documents || []).length, hint: `KYC: ${String(a.kyc_status || "not started").replace(/_/g, " ")}` },
        { label: "Revision", value: a.revision, hint: a.submitted_at ? `submitted ${M.day(a.submitted_at)}` : null },
      ]));
      const left = el("div", { class: "pm-col" });
      const right = el("div", { class: "pm-col" });
      box.append(el("div", { class: "pm-split" }, left, right));
      for (const [title, keys] of GROUPS) left.append(ui.card({ title }, M.kv(keys.map((k) => [FIELDS[k], k === "bank_country" && d[k] ? region(d[k]) : d[k], k]))));
      const docs = a.documents || [];
      left.append(ui.card({ title: "Papers", text: "Downloads are recorded and never cached here." }, docs.length ? ui.table({
        dense: true, rows: docs, pageSize: 100,
        columns: [
          { key: "category", label: "Category", render: (x) => ui.badge(x.category) },
          { key: "filename", label: "File" },
          { key: "size", label: "Size", num: true, render: (x) => `${Math.max(1, Math.round(Number(x.size || 0) / 1024))} KB` },
          { key: "created_at", label: "Added", render: (x) => M.day(x.created_at) },
          { key: "id", label: "", render: (x) => ui.button("Download", { size: "sm", icon: "docs", onclick: () => M.download(`/v1/staff/publisher-dossiers/${enc(a.id)}/documents/${enc(x.id)}`) }) },
        ],
      }) : ui.empty({ icon: "docs", title: "No papers uploaded" })));
      const count = await reviewers;
      if (["submitted", "in_review"].includes(a.state)) right.append(ui.card({ title: "Decision", tone: "accent" }, decisionForm(a, count)));
      else if (a.state === "needs_information") right.append(ui.card({ title: "Waiting on the applicant" }, el("p", { class: "pp-muted" }, `Requested: ${(a.requested_documents || []).join(", ") || "more information"}. The application comes back to Submitted when they answer.`)));
      right.append(ui.card({ title: "Checklist", text: "From the dossier; the decision is yours." }, ui.checklist(M.checklist(a))));
      if (a.state === "approved") right.append(ui.card({ title: "Revoke certification", tone: "danger" }, revokeForm(a, count, () => PP.shell.refresh())));
      right.append(ui.card({ title: "Staff notes and history", text: "Every event, newest first. Notes are visible to the applicant." }, history(a)));
    }, { notFound: "No application with that id" });
  }

  function history(a) {
    const rows = a.history || [];
    if (!rows.length) return ui.empty({ icon: "audit", title: "No history yet" });
    return el("ol", { class: "pm-timeline" }, rows.map((e) => el("li", {},
      el("div", { class: "pm-timeline-head" }, el("strong", {}, STATES[e.event] || String(e.event || "").replace(/_/g, " ")), el("span", { class: "pp-faint" }, `${M.time(e.created_at)} · revision ${e.revision}${e.actor ? " · " + e.actor : ""}`)),
      e.note ? el("p", {}, e.note) : null)));
  }

  /// Approve / ask for more papers / reject (or claim it for review), over the revision read.
  M.decisionForm = decisionForm;
  function decisionForm(a, reviewers) {
    const wrap = el("div", { class: "pm-decision" });
    let decision = a.state === "submitted" ? "in_review" : "approved";
    const DEC = [["in_review", "Claim for review"], ["needs_information", "Ask for more papers"], ["approved", "Approve"], ["rejected", "Reject"]].filter(([v]) => a.state === "submitted" || v !== "in_review");
    const docs = el("fieldset", { class: "pm-docs", hidden: decision !== "needs_information" ? true : null },
      el("legend", {}, "Papers to ask for"),
      DOC_CATEGORIES.map(([v, t]) => el("label", { class: "pm-check" }, el("input", { type: "checkbox", name: "requested", value: v }), el("span", {}, t))));
    const chips = M.chips({ label: "Decision", value: decision, options: DEC, onChange: (v) => { decision = v; docs.hidden = v !== "needs_information"; reason.placeholder = PLACE[v]; } });
    const PLACE = { in_review: "A note for the record, e.g. “Checking the tax certificate”", needs_information: "Exactly what is missing — the applicant sees this", approved: "Why it is approved — the applicant sees this", rejected: "Why it is rejected — the applicant sees this" };
    const form = M.form([{ name: "reason", label: "Reason (visible to the applicant)", type: "textarea", required: true, maxlength: 2000, rows: 3, wide: true }], {
      submit: "Record decision",
      onSubmit: async (v) => {
        const requested = decision === "needs_information" ? [...docs.querySelectorAll("input:checked")].map((i) => i.value) : [];
        if (decision === "needs_information" && !requested.length) throw new Error("Pick at least one kind of paper to ask for.");
        if (decision === "approved" || decision === "rejected") {
          const ok = await ui.confirm({ title: decision === "approved" ? "Approve this publisher?" : "Reject this application?", text: decision === "approved" ? "They become a certified publisher and can submit apps for review." : "The applicant sees your reason.", confirm: decision === "approved" ? "Approve" : "Reject", danger: decision === "rejected" });
          if (!ok) return "Not recorded.";
        }
        await M.post(`/v1/staff/publisher-dossiers/${enc(a.id)}/review`, { revision: a.revision, decision, reason: v.reason, requested_documents: requested });
        PP.toast({ in_review: "Claimed for review.", needs_information: "Asked for more papers.", approved: "Approved.", rejected: "Rejected." }[decision], "success");
        PP.shell.refresh();
        return "Recorded.";
      },
    });
    const reason = form.querySelector("textarea");
    reason.placeholder = PLACE[decision];
    form.prepend(chips, docs);
    wrap.append(M.singleNote(reviewers), form);
    return wrap;
  }

  function revokeForm(a, reviewers, after) {
    return el("div", {}, el("p", { class: "pp-muted" }, "Revoking suspends every listing of this publisher and cancels queued builds."), M.singleNote(reviewers), M.form([
      { name: "reason", label: "Reason", type: "textarea", required: true, maxlength: 2000, rows: 2, wide: true },
      { name: "typed", label: "Type REVOKE to confirm", required: true, validate: (v) => (v === "REVOKE" ? "" : "Type REVOKE in capitals.") },
    ], { submit: "Revoke publisher", danger: true, onSubmit: async (v) => {
      await M.post(`/v1/staff/publishers/${enc(a.id)}/revocation`, { reason: v.reason });
      PP.toast("Revoked. Its listings are suspended and queued builds cancelled.", "success");
      after();
      return "Revoked.";
    } }));
  }

  // ---- Businesses ----------------------------------------------------------------------------

  M.page({
    key: "businesses", title: "Businesses", group: "Partners", icon: "building", allow: M.allow(["reviewer", "finance"]),
    render: (view, ctx) => M.guard(view, async () => {
      view.append(ui.header({ title: "Businesses", text: "Every partner business and where it stands. Payout details come from the finance service, masked." }));
      if (M.has(ctx, "reviewer")) {
        const host = el("div");
        view.append(ui.section({ title: "Directory" }, host));
        let status = "approved", q = "", offset = 0;
        const chips = M.chips({ label: "Status", value: status, options: [["approved", "Certified"], ["in_review", "In review"], ["needs_information", "Waiting on applicant"], ["submitted", "Submitted"], ["rejected", "Rejected"], ["revoked", "Revoked"], ["all", "All"]], onChange: (v) => { status = v; offset = 0; load(); } });
        const search = ui.form([{ name: "q", label: "Search", placeholder: "Business name or id", maxlength: 100 }], { submit: "Search", cls: "pm-form-inline", onSubmit: (v) => { q = v.q; offset = 0; load(); } });
        const out = el("div");
        host.append(el("div", { class: "pm-toolbar" }, chips), search, out);
        const load = async () => {
          out.replaceChildren(ui.loading(3));
          let r;
          try { r = await M.get(`/v1/staff/publisher-dossiers?${M.qs({ status, q, sort: "name", offset })}`); } catch (e) { if (!(e instanceof PP.Redirecting)) out.replaceChildren(M.errorBox(e, load)); return; }
          const rows = r.data || [];
          out.replaceChildren(rows.length ? ui.table({
            rows, pageSize: 1000, caption: "Businesses", rowHref: (b) => PP.href("applications", b.id),
            columns: [
              { key: "display_name", label: "Business", sort: true, render: (b) => el("div", { class: "pm-stack" }, el("strong", {}, b.display_name), M.mono(b.id)) },
              { key: "kind", label: "Type", render: (b) => (b.kind ? ui.badge(b.kind) : "—") },
              { key: "country", label: "Country", sort: true, render: (b) => (b.country ? region(b.country) : "—") },
              { key: "state", label: "Status", sort: true, render: (b) => M.status(b.state, STATES[b.state]) },
              { key: "updated_at", label: "Updated", render: (b) => M.day(b.updated_at) },
            ],
          }) : ui.empty({ icon: "building", title: "No businesses in this state" }),
          r.next_offset != null || offset ? el("div", { class: "pm-more" },
            ui.button("Previous", { size: "sm", disabled: offset === 0, onclick: () => { offset = Math.max(0, offset - 25); load(); } }),
            ui.button("Next", { size: "sm", disabled: r.next_offset == null, onclick: () => { offset = r.next_offset; load(); } })) : null);
        };
        await load();
      }
      if (M.has(ctx, "finance")) {
        const out = el("div");
        view.append(ui.section({ title: "Payout details", text: "Look up a publisher's bank details for a payout. Numbers are masked." },
          ui.card({}, ui.form([{ name: "id", label: "Publisher id", required: true, placeholder: "pub_…" }], { submit: "Look up", cls: "pm-form-inline", onSubmit: async (v) => {
            out.replaceChildren(ui.loading(2));
            try {
              const p = (await M.get(`/v1/staff/publishers/${enc(v.id)}/payout-profile`)).data || {};
              out.replaceChildren(M.kv([["Publisher", p.publisher_id], ["IBAN", p.iban, "iban"], ["Bank", p.bank_name], ["Country", p.country], ["Currency", p.currency], ["Status", M.status(p.status)], ["Verified", M.time(p.verified_at)]]));
            } catch (e) { if (!(e instanceof PP.Redirecting)) out.replaceChildren(e.status === 404 ? ui.empty({ icon: "money", title: "No payout details on file" }) : M.errorBox(e)); }
          } }), out)));
      }
    }),
  });

  // ---- Partner analytics ---------------------------------------------------------------------

  const day = (d) => d.toISOString().slice(0, 10);
  const addDay = (s) => new Date(Date.parse(s + "T00:00:00Z") + 86400000).toISOString();

  M.page({
    key: "partner-analytics", title: "Partner analytics", group: "Partners", icon: "chart", allow: M.allow(["finance"]),
    render: (view) => M.guard(view, async () => {
      view.append(ui.header({ title: "Partner analytics", text: "Usage and earnings per publisher and per app, as the publisher sees them. All dates are UTC; currencies are never added together." }));
      const st = { publishers: [], next: null, q: "", selected: "", app: "", from: day(new Date(Date.now() - 29 * 86400000)), to: day(new Date()), currency: "USD", data: null };
      const pubSel = el("select", { class: "pp-input pp-select", "aria-label": "Publisher", onchange: () => { st.selected = pubSel.value; st.app = ""; report(); } });
      const appSel = el("select", { class: "pp-input pp-select", "aria-label": "App", onchange: () => { st.app = appSel.value; report(); } }, el("option", { value: "" }, "All apps"));
      const from = el("input", { class: "pp-input", type: "date", "aria-label": "From (UTC)", value: st.from });
      const to = el("input", { class: "pp-input", type: "date", "aria-label": "Through (UTC)", value: st.to });
      const cur = el("select", { class: "pp-input pp-select", "aria-label": "Currency", onchange: () => { st.currency = cur.value; draw(); } }, ["USD", "EUR", "TRY"].map((c) => el("option", { value: c }, c)));
      const find = ui.form([{ name: "q", label: "Find a publisher", placeholder: "Publisher name or id", maxlength: 100 }], { submit: "Find", cls: "pm-form-inline", onSubmit: (v) => { st.q = v.q; pick(false); } });
      const moreBtn = ui.button("More publishers", { size: "sm", onclick: () => pick(true) });
      moreBtn.hidden = true;
      const controls = el("form", { class: "pm-filters", onsubmit: (e) => { e.preventDefault(); st.from = from.value; st.to = to.value; report(); } },
        pubSel, appSel, from, to, ui.button("Update report", { type: "submit", kind: "primary", size: "sm" }),
        [7, 30, 90].map((n) => ui.button(`${n} days`, { size: "sm", kind: "quiet", onclick: () => { st.to = day(new Date()); st.from = day(new Date(Date.parse(st.to + "T00:00:00Z") - (n - 1) * 86400000)); from.value = st.from; to.value = st.to; report(); } })));
      const body = el("div", { class: "pm-analytics" });
      view.append(ui.card({}, find, moreBtn, controls), body);

      async function pick(more) {
        body.replaceChildren(ui.loading(2));
        let r;
        try { r = await M.get(`/v1/staff/publisher-analytics?${M.qs({ q: st.q, offset: more ? st.next || 0 : 0 })}`); } catch (e) { if (!(e instanceof PP.Redirecting)) body.replaceChildren(M.errorBox(e, () => pick(more))); return; }
        st.publishers = more ? st.publishers.concat(r.data || []) : r.data || [];
        st.next = r.next_offset ?? null;
        moreBtn.hidden = st.next == null;
        pubSel.replaceChildren(...st.publishers.map((p) => el("option", { value: p.id }, p.display_name)));
        if (!st.publishers.some((p) => p.id === st.selected)) st.selected = (st.publishers[0] || {}).id || "";
        pubSel.value = st.selected;
        if (st.selected) report(); else body.replaceChildren(ui.empty({ icon: "chart", title: "No publishers match", text: "Search by name or id." }));
      }
      async function report() {
        if (!st.selected) return;
        let end;
        try { end = addDay(st.to); } catch { end = null; }
        const start = Date.parse(st.from + "T00:00:00Z");
        if (!end || !Number.isFinite(start) || Date.parse(end) <= start || Date.parse(end) - start > 366 * 86400000) { body.replaceChildren(ui.gate({ text: "Choose an increasing date range of at most 366 days.", tone: "warning" })); return; }
        body.replaceChildren(ui.loading(4));
        const p = new URLSearchParams({ from: st.from + "T00:00:00Z", to: end });
        if (st.app) p.set("listing_id", st.app);
        try { st.data = (await M.get(`/v1/staff/publishers/${enc(st.selected)}/analytics?${p}`)).data; } catch (e) { if (!(e instanceof PP.Redirecting)) body.replaceChildren(M.errorBox(e, report)); return; }
        appSel.replaceChildren(el("option", { value: "" }, "All apps"), ...(st.data.app_choices || []).map((a) => el("option", { value: a.id }, a.title)));
        appSel.value = st.app;
        draw();
      }
      function draw() {
        const data = st.data;
        if (!data) return;
        const u = data.usage || {}, f = data.finance || {}, ok = f.status === "available";
        const money = (n, c) => (n == null ? "—" : M.minor(n, c));
        body.replaceChildren(
          ui.stats([
            { label: "Jobs submitted", value: M.num(u.jobs), hint: `${M.num(u.succeeded)} succeeded · ${M.num(u.failed)} failed` },
            { label: "Active users", value: M.num(u.active_users) },
            { label: "New installations", value: M.num(u.new_installations) },
            { label: "Active installations now", value: M.num(u.active_installations) },
          ]),
          ok ? ui.card({ title: "Earnings for sales in this period", text: "Net is after commission and reversals, before payout withholding." }, ui.table({
            dense: true, rows: f.currencies || [], pageSize: 20,
            columns: [{ key: "currency", label: "Currency" }, { key: "sales", label: "Sales", num: true },
              ...[["gross_minor", "Gross"], ["tax_minor", "Tax"], ["commission_minor", "Commission"], ["publisher_minor", "Publisher earned"], ["reversed_publisher_minor", "Reversed"], ["net_publisher_minor", "Net"]].map(([k, l]) => ({ key: k, label: l, num: true, render: (r) => money(r[k], r.currency) }))],
          })) : ui.gate({ text: f.message || "Earnings are unavailable. Missing amounts are not zero.", tone: "warning" }),
          ui.card({ title: "Apps", actions: [cur, ui.button("Export CSV", { size: "sm", icon: "docs", onclick: () => exportCsv(data) })] }, ui.table({
            rows: data.apps || [], pageSize: 50, sort: { key: "jobs", dir: "desc" }, empty: { icon: "apps", title: "No apps in this report yet" },
            columns: [
              { key: "title", label: "App", sort: true, render: (a) => el("div", { class: "pm-stack" }, el("strong", {}, a.title), el("span", { class: "pp-faint" }, a.status || "")) },
              { key: "jobs", label: "Jobs", num: true, sort: (a) => Number((a.usage || {}).jobs || 0), render: (a) => M.num((a.usage || {}).jobs) },
              { key: "succeeded", label: "Succeeded", num: true, render: (a) => M.num((a.usage || {}).succeeded) },
              { key: "users", label: "Active users", num: true, render: (a) => M.num((a.usage || {}).active_users) },
              { key: "installs", label: "Active installs", num: true, render: (a) => M.num((a.usage || {}).active_installations) },
              { key: "net", label: `Net (${st.currency})`, num: true, render: (a) => { const r = ok ? (f.apps || []).find((x) => x.listing_id === a.id && x.currency === st.currency) : null; return money(ok ? (r ? r.net_publisher_minor : 0) : null, st.currency); } },
            ],
          })),
          ok && (f.publisher_balances || []).length ? ui.card({ title: "Balances and payouts", text: "Current balances over all apps. Paid out counts bank-reconciled transfers in this range." }, ui.table({
            dense: true, rows: f.publisher_balances, pageSize: 20,
            columns: [{ key: "currency", label: "Currency" }, ...[["payable_minor", "Payable now"], ["reserve_minor", "Reserve held"], ["recoverable_minor", "Recoverable"], ["paid_out_minor", "Paid out in period"]].map(([k, l]) => ({ key: k, label: l, num: true, render: (r) => money(r[k], r.currency) }))],
          })) : null,
          el("p", { class: "pp-faint" }, `Updated ${M.time(data.generated_at)}. Jobs are counted by submission date with their current outcome; review jobs are excluded.`));
      }
      function exportCsv(data) {
        const f = data.finance || {};
        const rows = [["App ID", "App", "Currency", "Jobs", "Succeeded", "Failed", "Active users", "Active installations", "Gross minor", "Commission minor", "Net publisher minor"]];
        for (const a of data.apps || []) {
          const earn = f.status === "available" ? (f.apps || []).filter((r) => r.listing_id === a.id) : [];
          for (const r of earn.length ? earn : [null]) {
            const u = a.usage || {};
            rows.push([a.id, a.title, r ? r.currency : "", u.jobs, u.succeeded, u.failed, u.active_users, u.active_installations, r ? r.gross_minor : "", r ? r.commission_minor : "", r ? r.net_publisher_minor : ""]);
          }
        }
        M.saveCsv(`partner-analytics-${st.selected}-${st.from}-${st.to}.csv`, rows);
      }
      await pick(false);
    }),
  });
})();
