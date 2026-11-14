// management.pigeonpost.dev — the pieces every staff page shares, on top of site-shared/ (PP.el,
// PP.ui, PP.postbox). Pages register themselves in PPM.pages; app.js starts the shell.
//
// Rules kept from the back office (site/staff.js), which this platform replaces:
//  - The postbox decides who may see what (staff role, step-up, audit record). Navigation is
//    filtered by the roles /v1/staff/me reports, as a convenience, never as the control.
//  - A step-up refusal never leaves the page by itself: the page says "Confirm it's you" in place,
//    and the strong sign-in comes back to the same screen (PP.stepUp with a resume).
//  - Message bodies are never requested or shown. Secrets, IBANs and tax ids are masked again on
//    the way to the screen (`mask`/`shown`), on top of the postbox's own masking.
//  - Every list has a loading, an empty and an error state. Nothing from a server is written as
//    HTML.
(function () {
  "use strict";
  const PP = window.PP;
  const el = (...a) => PP.el(...a);
  const ui = PP.ui;
  const M = (window.PPM = window.PPM || {});
  M.pages = M.pages || [];

  // ---- roles ---------------------------------------------------------------------------------

  M.ROLES = ["owner", "support", "finance", "reviewer", "ops"];
  M.ROLE_TEXT = {
    owner: "People and the audit log",
    support: "People, plans and support",
    finance: "Money, plans and partner analytics",
    reviewer: "Applications, marketplace review and operations",
    ops: "Marketplace operations",
  };
  M.has = (ctx, role) => Boolean(ctx && Array.isArray(ctx.roles) && ctx.roles.includes(role));
  /// A page's `allow`: one of `roles`, or the platform owner for an owner-only page.
  M.allow = (roles, { ownerOnly = false } = {}) => (ctx) => {
    if (!ctx || !Array.isArray(ctx.roles) || !ctx.roles.length) return false;
    if (ownerOnly) return Boolean(ctx.owner);
    return roles.some((r) => ctx.roles.includes(r));
  };

  // ---- masking (identical rules to site/staff.js) --------------------------------------------

  /// Everything but the last `keep` characters hidden; short values hidden entirely.
  M.mask = function (value, keep = 4) {
    const chars = Array.from(String(value == null ? "" : value));
    if (chars.length <= keep * 2) return "••••";
    return "••••" + chars.slice(-keep).join("");
  };
  const SECRET_KEY = /(^|_)(iban|tax_?id|vkn|tckn|secret|token|password|passphrase|api_?key|key_hash|private_key|seed|authorization|cookie|card)($|_)/i;
  const IBAN = /^[A-Z]{2}\d{2}[A-Z0-9 ]{10,32}$/;
  /// What a field may show: a secret-named field, or one holding an IBAN, is masked unless the
  /// server already masked it.
  M.shown = function (key, value) {
    if (value == null || value === "") return value;
    if (typeof value === "object") return value;
    const s = String(value);
    if (s.includes("•")) return s;
    if (SECRET_KEY.test(String(key || ""))) return M.mask(s);
    const compact = s.replace(/\s/g, "");
    if (IBAN.test(compact) && /\d{6,}/.test(compact)) return M.mask(compact);
    return s;
  };
  M.maskDeep = function maskDeep(v, key) {
    if (Array.isArray(v)) return v.map((x) => maskDeep(x, key));
    if (v && typeof v === "object") {
      const out = {};
      for (const [k, x] of Object.entries(v)) out[k] = maskDeep(x, k);
      return out;
    }
    return M.shown(key, v);
  };
  /// One CSV cell, quoted; a cell a spreadsheet would read as a formula gets a leading `'`.
  M.csvCell = function (x) {
    let v = String(x == null ? "" : x);
    if (/^[=+\-@\t\r]/.test(v)) v = "'" + v;
    return `"${v.replace(/"/g, '""')}"`;
  };

  // ---- API -----------------------------------------------------------------------------------

  /// A postbox call that keeps a step-up refusal on the page (it throws, with `challenge`).
  M.api = (path, opts = {}) => PP.postbox(path, { ...opts, stepUp: "throw" });
  M.get = (path) => M.api(path);
  M.post = (path, body, method = "POST") => M.api(path, { method, body: body || {} });
  M.qs = (obj) => {
    const p = new URLSearchParams();
    for (const [k, v] of Object.entries(obj || {})) if (v !== undefined && v !== null && v !== "") p.set(k, String(v));
    return p.toString();
  };

  const ERRORS = {
    step_up_required: "This needs a recent two-step sign-in.",
    staff_required: "Your staff roles don't cover this.",
    human_required: "Only a person signed in here can do this.",
    not_found: "Nothing found.",
    finance_unavailable: "The finance service isn't configured or isn't answering.",
    finance_refused: "The finance service refused this.",
    finance_conflict: "The finance service says this already changed.",
    invalid_cursor: "That page is out of date. Start again from the first page.",
    feature_disabled: "This needs the marketplace to be on for your account.",
    step_up_unavailable: "Strong sign-in isn't set up on this server.",
    conflict: "Someone changed this meanwhile. Reload and look again.",
    application_decided: "This application was already decided.",
  };
  M.explain = function (e) {
    if (e instanceof PP.Redirecting) return null;
    if (e && e.code && ERRORS[e.code]) return ERRORS[e.code] + (e.code.startsWith("finance_") && e.message && e.message !== e.code ? ` ${e.message}` : "");
    return PP.explain(e);
  };
  const isStepUp = (e) => Boolean(e && e.code === "step_up_required");
  M.isStepUp = isStepUp;

  /// Leave for the strong sign-in the postbox asked for, and come back to this screen.
  M.confirm = function (e) {
    const ch = (e && e.challenge) || {};
    const me = (PP.shell.ctx() || {}).me || {};
    return PP.stepUp(
      { acr_values: ch.acr_values || (me.step_up && me.step_up.acr_values) || undefined, max_age: ch.max_age ?? 600 },
      { name: "mgmt-return", payload: { hash: window.location.hash || "" } });
  };
  PP.onResume("mgmt-return", (payload) => {
    if (payload && payload.hash && payload.hash !== window.location.hash) window.history.replaceState(null, "", window.location.pathname + payload.hash);
    PP.toast("Confirmed. You're back where you were.", "success");
  });

  /// "Confirm it's you", in place of whatever needed it.
  M.stepUpCard = function (e, { compact = false } = {}) {
    return el("div", { class: `pm-stepup${compact ? " pm-stepup-compact" : ""}`, role: "alert" },
      el("span", { class: "pm-stepup-icon" }, PP.icon("security")),
      el("div", { class: "pm-stepup-text" },
        el("strong", {}, "Confirm it's you"),
        el("p", { class: "pp-muted" }, "Staff tools need a recent two-step sign-in. You'll come straight back to this screen.")),
      ui.button("Confirm it's you", { kind: "primary", icon: "key", onclick: () => M.confirm(e) }));
  };

  /// The error state for staff screens: step-up in place, 404 as "nothing found", else the
  /// shared error state with a retry.
  M.errorBox = function (e, retry, { notFound } = {}) {
    if (isStepUp(e)) return M.stepUpCard(e);
    if (e && e.status === 404 && notFound) return ui.empty({ icon: "search", title: notFound, text: "Check the id, or search for it." });
    const box = ui.errorState(e, retry);
    const p = box.querySelector(".pp-muted");
    if (p) p.textContent = M.explain(e) || "Something went wrong.";
    return box;
  };
  /// Page-level: thrown errors from render() land in the shell; a step-up refusal is drawn here.
  M.guard = async function (view, fn) {
    try { await fn(); } catch (e) {
      if (e instanceof PP.Redirecting) throw e;
      if (isStepUp(e)) { view.append(M.stepUpCard(e)); return; }
      throw e;
    }
  };

  // ---- formatting ----------------------------------------------------------------------------

  M.usd = (cents) => ui.money(cents, "USD");
  M.minor = (amount, currency) => (amount == null ? "—" : ui.money(amount, currency || "USD"));
  M.num = (n) => (n == null ? "—" : Number(n).toLocaleString(PP.locale()));
  M.day = (v) => ui.when(v);
  M.time = (v) => ui.whenTime(v);
  const TONE = {
    active: "success", succeeded: "success", published: "success", approved: "success", verified: "success", captured: "success", running: "success", completed: "success", live: "success", normal: "success",
    past_due: "warning", grace: "warning", pending: "warning", open: "warning", queued: "warning", applied: "warning", uncertain: "warning", blocked: "warning", held: "warning", holding: "warning", submitted: "info", in_review: "info", needs_information: "warning", waiting_partner: "warning", draft: "neutral",
    failed: "danger", suspended: "danger", quarantined: "danger", revoked: "danger", expired: "danger", canceled: "danger", cancelled: "danger", deleted: "danger", refused: "danger", killed: "danger", rejected: "danger", read_only: "warning",
  };
  M.status = (s, label) => (s ? ui.badge(label || String(s).replace(/_/g, " "), TONE[s] || "neutral") : "—");
  M.link = (text, page, ...params) => el("a", { href: PP.href(page, ...params), class: "pm-link" }, text);
  M.mono = (text) => el("span", { class: "pp-mono", translate: "no" }, text == null || text === "" ? "—" : String(text));
  /// Key/value pairs with the masking rule applied: [label, value, key?].
  M.kv = (pairs) => ui.kv(pairs.filter(Boolean).map(([k, v, key]) => {
    const val = M.shown(key || k, v);
    return [k, val instanceof window.Node ? val : val == null || val === "" ? "—" : String(val)];
  }));

  /// Filter chips: one choice at a time. options [[value, label, count?]].
  M.chips = function ({ label, options, value, onChange }) {
    const row = el("div", { class: "pm-chips", role: "group", "aria-label": label });
    const draw = (current) => {
      row.replaceChildren(...options.map(([v, t, n]) => el("button", {
        type: "button", class: `pm-chip${v === current ? " is-on" : ""}`, "aria-pressed": String(v === current), "data-value": v,
        onclick: () => { draw(v); onChange(v); },
      }, t, n != null ? el("span", { class: "pm-chip-n" }, String(n)) : null)));
    };
    draw(value);
    return row;
  };

  /// A server-paged list (`next_cursor`): search, filter chips/selects, a sortable table and
  /// "Load more". spec: { search, chips: {name,label,options,value}, filters: [{name,label,options}],
  /// fetch(URLSearchParams) → {data,next_cursor}, columns, rowHref, onRow, empty, limit, caption }.
  M.list = function (root, spec) {
    const st = { q: spec.q || "", filters: { ...(spec.defaults || {}) }, rows: [], next: null, loading: false, error: null, done: false, seq: 0 };
    if (spec.chips) st.filters[spec.chips.name] = spec.chips.value ?? st.filters[spec.chips.name] ?? "";
    const bar = el("div", { class: "pm-toolbar" });
    if (spec.chips) bar.append(M.chips({ label: spec.chips.label, options: spec.chips.options, value: st.filters[spec.chips.name], onChange: (v) => { st.filters[spec.chips.name] = v; load(true); } }));
    const form = el("form", { class: "pm-filters", role: "search", onsubmit: (e) => { e.preventDefault(); st.q = q ? q.value.trim() : ""; load(true); } });
    const q = spec.search === false || !spec.search ? null : el("input", { class: "pp-input pp-search", type: "search", placeholder: spec.search, "aria-label": spec.search, value: st.q, autocomplete: "off", spellcheck: "false" });
    if (q) form.append(el("label", { class: "pp-search-wrap" }, PP.icon("search"), q));
    for (const f of spec.filters || []) {
      const sel = el("select", { class: "pp-input pp-select", "aria-label": f.label, onchange: () => { st.filters[f.name] = sel.value; load(true); } },
        f.options.map(([v, t]) => el("option", { value: v, selected: (st.filters[f.name] || "") === v ? true : null }, t)));
      form.append(sel);
    }
    if (q) form.append(ui.button("Search", { type: "submit", size: "sm" }));
    if (form.childNodes.length) bar.append(form);
    const out = el("div", { class: "pm-list", "aria-live": "polite" });
    const more = el("div", { class: "pm-more" });
    if (bar.childNodes.length) root.append(bar);
    root.append(out, more);

    async function load(reset) {
      const seq = ++st.seq;
      if (reset) { st.rows = []; st.next = null; st.done = false; }
      st.loading = true; st.error = null; draw();
      const p = new URLSearchParams();
      if (st.q) p.set("q", st.q);
      for (const [k, v] of Object.entries(st.filters)) if (v) p.set(k, v);
      if (st.next) p.set("cursor", st.next);
      p.set("limit", String(spec.limit || 25));
      try {
        const page = await spec.fetch(p);
        if (seq !== st.seq) return;
        st.rows = st.rows.concat(page.data || []);
        st.next = page.next_cursor || null;
        st.done = true;
      } catch (e) {
        if (seq !== st.seq) return;
        if (e instanceof PP.Redirecting) return;
        st.error = e;
      }
      st.loading = false; draw();
    }
    function draw() {
      out.replaceChildren(); more.replaceChildren();
      if (st.loading && !st.rows.length) { out.append(ui.loading(3)); return; }
      if (st.error && !st.rows.length) { out.append(M.errorBox(st.error, () => load(true))); return; }
      if (st.done && !st.rows.length) {
        out.append(ui.empty(st.q ? { icon: "search", title: `Nothing matches “${st.q}”`, text: "Try another search or filter." } : (typeof spec.empty === "string" ? { title: spec.empty } : spec.empty || { title: "Nothing here yet" })));
        return;
      }
      out.append(ui.table({ columns: spec.columns, rows: st.rows, rowHref: spec.rowHref, onRow: spec.onRow, caption: spec.caption, dense: spec.dense, pageSize: 1000 }));
      if (st.error) more.append(M.errorBox(st.error, () => load(false)));
      if (st.loading) more.append(el("span", { class: "pp-muted" }, "Loading…"));
      else if (st.next) more.append(ui.button("Load more", { size: "sm", onclick: () => load(false) }));
      if (!st.loading) more.append(el("span", { class: "pp-muted pm-count" }, `${st.rows.length}${st.next ? "+" : ""} shown`));
    }
    load(true);
    return { reload: () => load(true), state: st };
  };

  /// A detail fetch: loading, then draw(data); 404 says "nothing found".
  M.detail = async function (root, path, draw, { notFound = "Nothing found with that id" } = {}) {
    const box = el("div", { class: "pm-detail" });
    root.append(box);
    const go = async () => {
      box.replaceChildren(ui.loading(4));
      let v;
      try { v = await M.get(path); } catch (e) {
        if (e instanceof PP.Redirecting) return;
        box.replaceChildren(M.errorBox(e, go, { notFound }));
        return;
      }
      box.replaceChildren();
      draw(box, v.data || {});
    };
    await go();
  };

  /// A form whose submit calls the postbox. A step-up refusal shows "Confirm it's you" under it.
  M.form = function (fields, { submit, onSubmit, danger, cls } = {}) {
    const slot = el("div", { class: "pm-form-slot" });
    const f = ui.form(fields, {
      submit, cls,
      onSubmit: async (values) => {
        slot.replaceChildren();
        try { return await onSubmit(values, f); } catch (e) {
          if (e instanceof PP.Redirecting) throw e;
          if (isStepUp(e)) slot.append(M.stepUpCard(e, { compact: true }));
          throw Object.assign(new Error(M.explain(e) || "Something went wrong."), { code: null });
        }
      },
    });
    if (danger) { const b = f.querySelector("button[type=submit]"); if (b) b.className = "pp-btn pp-btn-danger"; }
    f.append(slot);
    return f;
  };

  /// A button that opens a reason form under itself (moderation, kills, closes).
  M.reasonAction = function (label, run, { danger = false, field = "Reason", placeholder = "Why — the record keeps it", max = 2000 } = {}) {
    const wrap = el("div", { class: "pm-inline" });
    const btn = ui.button(label, { size: "sm", kind: danger ? "danger" : "default", onclick: () => {
      btn.hidden = true;
      wrap.append(M.form([{ name: "reason", label: field, required: true, maxlength: max, placeholder }], {
        submit: label, danger,
        onSubmit: async (v) => run(v.reason),
      }));
      const i = wrap.querySelector("input, textarea");
      if (i) i.focus();
    } });
    wrap.append(btn);
    return wrap;
  };

  /// The "not live yet" page: what it will hold and why it isn't here yet.
  M.soon = function ({ title, text, holds, why }) {
    return (view) => {
      view.append(ui.header({ title, text }),
        ui.card({ cls: "pm-soon" }, ui.empty({ icon: "info", title: "Not live yet", text: why }),
          holds && holds.length ? el("div", { class: "pm-soon-list" }, el("p", { class: "pp-eyebrow" }, "It will hold"), el("ul", {}, holds.map((h) => el("li", {}, PP.icon("check"), h)))) : null));
    };
  };

  /// Download a staff-only file the postbox returns as base64 JSON, without keeping it.
  M.download = async function (path) {
    const { data: d } = await M.get(path);
    const bytes = Uint8Array.from(window.atob(d.content_base64 || ""), (c) => c.charCodeAt(0));
    const url = URL.createObjectURL(new Blob([bytes], { type: "application/octet-stream" }));
    const a = el("a", { href: url, download: d.filename || "document" });
    document.body.append(a); a.click(); a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  };

  /// A CSV file from rows of cells.
  M.saveCsv = function (name, rows) {
    const url = URL.createObjectURL(new Blob([rows.map((r) => r.map(M.csvCell).join(",")).join("\r\n")], { type: "text/csv;charset=utf-8" }));
    const a = el("a", { href: url, download: name });
    document.body.append(a); a.click(); a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  };

  M.page = (spec) => { M.pages.push(spec); return spec; };
})();
