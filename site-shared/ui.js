// Pigeonpost platforms — UI components on top of core.js (PP.el) and shell.js (PP.icon).
//
// One vocabulary for every platform, so a table, an empty list or a "do this first" gate looks and
// behaves the same in account., enterprise., partner. and management.:
//
//   PP.ui.header({ title, text, actions })        page title row
//   PP.ui.section({ title, text, actions }, ...body)
//   PP.ui.card({ title, text, actions, tone }, ...body)
//   PP.ui.stats([{ label, value, hint, href, tone }])
//   PP.ui.table({ columns, rows, empty, onRow, search, filters, pageSize, sort })
//   PP.ui.empty({ icon, title, text, action })    PP.ui.errorState(e, retry)    PP.ui.loading()
//   PP.ui.badge(text, tone)    PP.ui.status(state, map)    PP.ui.kv(pairs)
//   PP.ui.tabs(items, active, onChange)    PP.ui.stepper(steps, current, onStep)
//   PP.ui.gate({ text, action })                   a disabled area with one "do this first" action
//   PP.ui.button(label, { kind, onclick, href, disabled, icon, busy })
//   PP.ui.confirm({ title, text, confirm, danger }) → Promise<boolean>
//   PP.ui.form(fields, { submit, onSubmit })       simple validated forms
//   PP.ui.copyable(text)    PP.ui.money(cents, currency)    PP.ui.when(unix)
//
// Nothing here writes HTML; all server text goes through text nodes.
(function () {
  "use strict";
  const PP = (window.PP = window.PP || {});
  const el = (...a) => PP.el(...a);
  const ui = (PP.ui = PP.ui || {});

  const asNodes = (x) => (x === undefined || x === null || x === false ? [] : Array.isArray(x) ? x : [x]);

  ui.button = function (label, { kind = "default", onclick, href, disabled, icon, busy, type = "button", title, size } = {}) {
    const cls = `pp-btn${kind !== "default" ? ` pp-btn-${kind}` : ""}${size ? ` pp-btn-${size}` : ""}`;
    const inner = [icon ? PP.icon(icon) : null, el("span", {}, label)];
    if (href && !disabled) return el("a", { class: cls, href, title, target: /^https?:/.test(href) && !href.includes(window.location.host) ? "_blank" : null, rel: /^https?:/.test(href) ? "noopener" : null }, inner);
    const b = el("button", { class: cls, type, disabled: disabled || busy ? true : null, title, "aria-busy": busy ? "true" : null }, inner);
    if (onclick) {
      b.addEventListener("click", async (e) => {
        if (b.disabled) return;
        b.disabled = true;
        b.setAttribute("aria-busy", "true");
        try { await onclick(e); } catch (err) { PP.fail(err); } finally {
          if (b.isConnected) { b.disabled = Boolean(disabled); b.removeAttribute("aria-busy"); }
        }
      });
    }
    return b;
  };

  ui.header = function ({ title, text, actions, eyebrow } = {}) {
    return el("header", { class: "pp-page-head" },
      el("div", { class: "pp-page-titles" },
        eyebrow ? el("p", { class: "pp-eyebrow" }, eyebrow) : null,
        el("h1", {}, title),
        text ? el("p", { class: "pp-lede" }, text) : null),
      actions ? el("div", { class: "pp-page-actions" }, asNodes(actions)) : null);
  };

  ui.section = function ({ title, text, actions, id } = {}, ...body) {
    return el("section", { class: "pp-section", id },
      title || actions ? el("div", { class: "pp-section-head" },
        el("div", {}, title ? el("h2", {}, title) : null, text ? el("p", { class: "pp-muted" }, text) : null),
        actions ? el("div", { class: "pp-section-actions" }, asNodes(actions)) : null) : null,
      body);
  };

  ui.card = function ({ title, text, actions, tone, cls } = {}, ...body) {
    return el("div", { class: `pp-card${tone ? ` pp-card-${tone}` : ""}${cls ? ` ${cls}` : ""}` },
      title || actions ? el("div", { class: "pp-card-head" },
        el("div", {}, title ? el("h3", {}, title) : null, text ? el("p", { class: "pp-muted" }, text) : null),
        actions ? el("div", { class: "pp-card-actions" }, asNodes(actions)) : null) : null,
      body);
  };

  ui.grid = (...cards) => el("div", { class: "pp-grid" }, cards);

  ui.stats = function (items) {
    return el("div", { class: "pp-stats" }, items.filter(Boolean).map((s) => {
      const body = [el("span", { class: "pp-stat-label" }, s.label), el("strong", { class: "pp-stat-value" }, s.value === undefined || s.value === null ? "—" : String(s.value)), s.hint ? el("span", { class: "pp-stat-hint" }, s.hint) : null];
      return s.href ? el("a", { class: `pp-stat${s.tone ? ` pp-tone-${s.tone}` : ""}`, href: s.href }, body) : el("div", { class: `pp-stat${s.tone ? ` pp-tone-${s.tone}` : ""}` }, body);
    }));
  };

  ui.badge = (text, tone = "neutral") => el("span", { class: `pp-badge pp-tone-${tone}` }, text);

  /// A state badge from a map { state: [label, tone] }; unknown states show as neutral text.
  ui.status = function (value, map) {
    const m = (map && map[value]) || [String(value || "—").replace(/_/g, " "), "neutral"];
    return ui.badge(m[0], m[1]);
  };

  ui.kv = function (pairs) {
    return el("dl", { class: "pp-kv" }, pairs.filter(Boolean).map(([k, v]) => [el("dt", {}, k), el("dd", {}, v === undefined || v === null || v === "" ? "—" : v)]));
  };

  ui.empty = function ({ icon = "info", title, text, action } = {}) {
    return el("div", { class: "pp-empty" },
      el("span", { class: "pp-empty-icon" }, PP.icon(icon)),
      title ? el("h3", {}, title) : null,
      text ? el("p", { class: "pp-muted" }, text) : null,
      action ? el("div", { class: "pp-empty-action" }, action instanceof window.Node ? action : ui.button(action.label, { kind: "primary", ...action })) : null);
  };

  ui.errorState = function (e, retry) {
    const text = PP.explain(e) || "Something went wrong.";
    const id = e && e.body && (e.body.request_id || e.body.requestId);
    return el("div", { class: "pp-empty pp-empty-error", role: "alert" },
      el("span", { class: "pp-empty-icon" }, PP.icon("alert")),
      el("h3", {}, "This could not be loaded"),
      el("p", { class: "pp-muted" }, text),
      id ? el("p", { class: "pp-faint pp-mono" }, `Reference: ${id}`) : null,
      retry ? el("div", { class: "pp-empty-action" }, ui.button("Try again", { onclick: retry })) : null);
  };

  ui.loading = function (rows = 4) {
    return el("div", { class: "pp-loading", "aria-busy": "true", "aria-label": "Loading" },
      el("div", { class: "pp-skel pp-skel-title" }),
      Array.from({ length: rows }, () => el("div", { class: "pp-skel" })));
  };

  /// "Do this first": an explanation and one action, above an area that is shown but disabled.
  ui.gate = function ({ text, action, tone = "warning" }, ...area) {
    const banner = el("div", { class: `pp-gate pp-tone-${tone}`, role: "note" },
      PP.icon(tone === "danger" ? "alert" : "info"),
      el("p", {}, text),
      action ? (action instanceof window.Node ? action : ui.button(action.label, { kind: "primary", ...action })) : null);
    if (!area.length) return banner;
    return el("div", { class: "pp-gated" }, banner, el("div", { class: "pp-gated-area", inert: true, "aria-disabled": "true" }, area));
  };

  ui.tabs = function (items, active, onChange) {
    const bar = el("div", { class: "pp-tabs", role: "tablist" });
    for (const t of items) {
      const on = t.key === active;
      bar.append(el("button", { class: `pp-tab${on ? " is-active" : ""}`, type: "button", role: "tab", "aria-selected": String(on), onclick: () => onChange(t.key) },
        t.label, t.count ? el("span", { class: "pp-tab-count" }, String(t.count)) : null));
    }
    return bar;
  };

  /// Steps of a multi-step form. `steps`: [{ key, title, done }]; clicking a reachable step calls onStep.
  ui.stepper = function (steps, current, onStep) {
    const idx = steps.findIndex((s) => s.key === current);
    return el("ol", { class: "pp-stepper" }, steps.map((s, i) => {
      const cls = `pp-step${i === idx ? " is-current" : ""}${s.done ? " is-done" : ""}`;
      const reachable = onStep && (s.done || i <= idx + (steps[idx] && steps[idx].done ? 1 : 0));
      const label = [el("span", { class: "pp-step-num" }, s.done && i !== idx ? PP.icon("check") : String(i + 1)), el("span", { class: "pp-step-title" }, s.title)];
      return el("li", { class: cls, "aria-current": i === idx ? "step" : null },
        reachable ? el("button", { type: "button", onclick: () => onStep(s.key) }, label) : el("span", {}, label));
    }));
  };

  ui.confirm = function ({ title = "Are you sure?", text, confirm = "Confirm", danger = false } = {}) {
    return new Promise((resolve) => {
      let answered = false;
      const done = (v) => { if (answered) return; answered = true; PP.closeSheet(); resolve(v); };
      PP.openSheet(title, (body) => {
        if (text) body.append(el("p", {}, text));
        body.append(el("div", { class: "pp-form-actions" },
          ui.button("Cancel", { onclick: () => done(false) }),
          ui.button(confirm, { kind: danger ? "danger" : "primary", onclick: () => done(true) })));
      }, { onClose: () => { if (!answered) { answered = true; resolve(false); } } });
    });
  };

  ui.copyable = function (text, { mono = true } = {}) {
    return el("span", { class: "pp-copyable" },
      el("span", { class: mono ? "pp-mono" : "", translate: "no" }, text),
      el("button", { class: "pp-icon-btn pp-icon-btn-sm", type: "button", "aria-label": `Copy ${text}`, onclick: () => PP.copy(text) }, PP.icon("copy")));
  };

  ui.money = function (cents, currency = "USD") {
    if (cents === undefined || cents === null || cents === "") return "—";
    return PP.money(Number(cents) / 100, currency);
  };
  /// Unix seconds or ISO text → a short date.
  ui.when = function (v) {
    if (!v) return "—";
    const n = typeof v === "number" ? v : Date.parse(v) / 1000;
    return Number.isFinite(n) ? PP.date(n) : String(v);
  };
  ui.whenTime = function (v) {
    if (!v) return "—";
    const n = typeof v === "number" ? v * 1000 : Date.parse(v);
    if (!Number.isFinite(n)) return String(v);
    try { return new Date(n).toLocaleString(PP.locale(), { year: "numeric", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }); } catch { return String(v); }
  };

  /// A data table with optional search, filter chips, sortable columns, paging and a row action.
  /// columns: [{ key, label, render(row), sort: true | (row) => value, num, mono, width }]
  ui.table = function ({ columns, rows, empty, onRow, rowHref, search, filters, pageSize = 25, sort, caption, dense } = {}) {
    const st = { q: "", filter: {}, sort: sort || null, page: 0 };
    const wrap = el("div", { class: "pp-table-wrap" });
    const tools = el("div", { class: "pp-table-tools" });
    const host = el("div", { class: "pp-table-host" });
    const pager = el("div", { class: "pp-pager" });
    if (search) {
      const input = el("input", { class: "pp-input pp-search", type: "search", placeholder: typeof search === "string" ? search : "Search", "aria-label": typeof search === "string" ? search : "Search" });
      input.addEventListener("input", () => { st.q = input.value.trim().toLowerCase(); st.page = 0; draw(); });
      tools.append(el("label", { class: "pp-search-wrap" }, PP.icon("search"), input));
    }
    for (const f of filters || []) {
      const sel = el("select", { class: "pp-input pp-select", "aria-label": f.label }, el("option", { value: "" }, `${f.label}: all`), f.options.map(([v, t]) => el("option", { value: v }, t)));
      sel.addEventListener("change", () => { st.filter[f.key] = sel.value; st.page = 0; draw(); });
      tools.append(sel);
    }
    if (tools.childNodes.length) wrap.append(tools);
    wrap.append(host, pager);

    const valueOf = (c, r) => (typeof c.sort === "function" ? c.sort(r) : r[c.key]);
    function filtered() {
      let out = rows.slice();
      if (st.q) out = out.filter((r) => columns.some((c) => String(r[c.key] ?? "").toLowerCase().includes(st.q)) || JSON.stringify(r).toLowerCase().includes(st.q));
      for (const f of filters || []) {
        const v = st.filter[f.key];
        if (v) out = out.filter((r) => String(typeof f.value === "function" ? f.value(r) : r[f.key]) === v);
      }
      if (st.sort) {
        const c = columns.find((x) => x.key === st.sort.key);
        if (c) out.sort((a, b) => {
          const x = valueOf(c, a), y = valueOf(c, b);
          const cmp = typeof x === "number" && typeof y === "number" ? x - y : String(x ?? "").localeCompare(String(y ?? ""), undefined, { numeric: true });
          return st.sort.dir === "desc" ? -cmp : cmp;
        });
      }
      return out;
    }
    function draw() {
      const all = filtered();
      if (!all.length) {
        host.replaceChildren(rows.length ? ui.empty({ icon: "search", title: "No matches", text: "Try a different search or filter." }) : ui.empty(empty || { title: "Nothing here yet" }));
        pager.replaceChildren();
        return;
      }
      const pages = Math.max(1, Math.ceil(all.length / pageSize));
      st.page = Math.min(st.page, pages - 1);
      const slice = all.slice(st.page * pageSize, st.page * pageSize + pageSize);
      const head = el("tr", {}, columns.map((c) => {
        if (!c.sort) return el("th", { scope: "col", class: c.num ? "pp-num" : null, style: c.width ? `width:${c.width}` : null }, c.label);
        const on = st.sort && st.sort.key === c.key;
        const btn = el("button", { type: "button", class: "pp-sort" + (on ? ` is-${st.sort.dir}` : ""), onclick: () => {
          st.sort = on && st.sort.dir === "asc" ? { key: c.key, dir: "desc" } : { key: c.key, dir: "asc" };
          draw();
        } }, c.label);
        return el("th", { scope: "col", class: c.num ? "pp-num" : null, "aria-sort": on ? (st.sort.dir === "asc" ? "ascending" : "descending") : "none" }, btn);
      }));
      const body = slice.map((r) => {
        const tr = el("tr", { class: onRow || rowHref ? "pp-row-link" : null }, columns.map((c) => {
          let v;
          try { v = c.render ? c.render(r) : r[c.key]; } catch { v = "—"; }
          return el("td", { class: [c.num ? "pp-num" : "", c.mono ? "pp-mono" : ""].join(" ").trim() || null, "data-label": c.label }, v === undefined || v === null || v === "" ? "—" : v);
        }));
        if (rowHref) {
          tr.addEventListener("click", (e) => { if (!e.target.closest("a, button, input, select")) window.location.hash = rowHref(r).replace(/^#/, ""); });
          tr.tabIndex = 0;
          tr.addEventListener("keydown", (e) => { if (e.key === "Enter") window.location.hash = rowHref(r).replace(/^#/, ""); });
        } else if (onRow) {
          tr.addEventListener("click", (e) => { if (!e.target.closest("a, button, input, select")) onRow(r); });
          tr.tabIndex = 0;
          tr.addEventListener("keydown", (e) => { if (e.key === "Enter") onRow(r); });
        }
        return tr;
      });
      host.replaceChildren(el("table", { class: `pp-table${dense ? " pp-table-dense" : ""}` }, caption ? el("caption", { class: "pp-sr" }, caption) : null, el("thead", {}, head), el("tbody", {}, body)));
      pager.replaceChildren();
      if (pages > 1) {
        pager.append(
          el("span", { class: "pp-muted" }, `${st.page * pageSize + 1}–${Math.min(all.length, (st.page + 1) * pageSize)} of ${all.length}`),
          ui.button("Previous", { size: "sm", disabled: st.page === 0, onclick: () => { st.page--; draw(); } }),
          ui.button("Next", { size: "sm", disabled: st.page >= pages - 1, onclick: () => { st.page++; draw(); } }));
      } else if (all.length > 5) {
        pager.append(el("span", { class: "pp-muted" }, `${all.length} items`));
      }
    }
    draw();
    return wrap;
  };

  /// A small validated form. fields: [{ name, label, type, value, required, options, hint, maxlength,
  /// pattern, placeholder, rows, autocomplete }]. onSubmit(values) may throw to show an error.
  ui.form = function (fields, { submit = "Save", onSubmit, secondary, cls } = {}) {
    const form = el("form", { class: `pp-form${cls ? ` ${cls}` : ""}`, novalidate: true });
    const inputs = {};
    for (const f of fields) {
      if (f.type === "heading") { form.append(el("h3", { class: "pp-form-heading" }, f.label)); continue; }
      let input;
      const common = { name: f.name, required: f.required ? true : null, placeholder: f.placeholder || null, autocomplete: f.autocomplete || null, maxlength: f.maxlength || null, pattern: f.pattern || null, disabled: f.disabled ? true : null };
      if (f.type === "select") {
        input = el("select", { class: "pp-input", ...common }, (f.options || []).map(([v, t]) => el("option", { value: v, selected: String(f.value ?? "") === String(v) ? true : null }, t)));
      } else if (f.type === "textarea") {
        input = el("textarea", { class: "pp-input", rows: f.rows || 4, ...common });
        input.value = f.value ?? "";
      } else if (f.type === "checkbox") {
        input = el("input", { type: "checkbox", ...common, checked: f.value ? true : null });
      } else {
        input = el("input", { class: "pp-input", type: f.type || "text", ...common, inputmode: f.inputmode || null, min: f.min ?? null, max: f.max ?? null, step: f.step ?? null });
        input.value = f.value ?? "";
      }
      inputs[f.name] = input;
      const id = `pp-f-${f.name}-${PP.random(4)}`;
      input.id = id;
      const err = el("p", { class: "pp-field-error", id: `${id}-err`, hidden: true });
      input.setAttribute("aria-describedby", `${id}-err`);
      if (f.type === "checkbox") {
        form.append(el("div", { class: "pp-field pp-field-check" }, el("label", { for: id }, input, el("span", {}, f.label)), f.hint ? el("p", { class: "pp-hint" }, f.hint) : null, err));
      } else {
        form.append(el("div", { class: `pp-field${f.wide ? " pp-field-wide" : ""}` }, el("label", { for: id }, f.label, f.required ? null : el("span", { class: "pp-optional" }, " (optional)")), input, f.hint ? el("p", { class: "pp-hint" }, f.hint) : null, err));
      }
    }
    const status = el("p", { class: "pp-form-status", role: "status", "aria-live": "polite" });
    const submitBtn = el("button", { class: "pp-btn pp-btn-primary", type: "submit" }, submit);
    form.append(el("div", { class: "pp-form-actions" }, secondary || null, submitBtn), status);
    form.values = () => {
      const out = {};
      for (const [k, i] of Object.entries(inputs)) out[k] = i.type === "checkbox" ? i.checked : typeof i.value === "string" ? i.value.trim() : i.value;
      return out;
    };
    form.inputs = inputs;
    form.addEventListener("submit", async (e) => {
      e.preventDefault();
      let ok = true;
      for (const f of fields) {
        const i = inputs[f.name];
        if (!i) continue;
        const errEl = document.getElementById(`${i.id}-err`);
        let msg = "";
        if (f.type !== "checkbox" && f.required && !String(i.value || "").trim()) msg = `${f.label} is required.`;
        else if (f.type === "checkbox" && f.required && !i.checked) msg = "Please confirm this.";
        else if (i.value && f.pattern && !new RegExp(`^(?:${f.pattern})$`).test(i.value.trim())) msg = f.patternHint || `Check the format of ${f.label.toLowerCase()}.`;
        else if (f.validate) msg = f.validate(i.type === "checkbox" ? i.checked : i.value.trim(), form.values()) || "";
        if (errEl) { errEl.textContent = msg; errEl.hidden = !msg; }
        i.toggleAttribute("aria-invalid", Boolean(msg));
        if (msg && ok) { ok = false; i.focus(); }
      }
      if (!ok || !onSubmit) return;
      submitBtn.disabled = true;
      status.textContent = "";
      status.className = "pp-form-status";
      try {
        const msg = await onSubmit(form.values());
        if (msg) { status.textContent = msg; status.classList.add("is-ok"); }
      } catch (err) {
        if (!(err instanceof PP.Redirecting)) { status.textContent = PP.explain(err); status.classList.add("is-error"); }
      } finally {
        if (submitBtn.isConnected) submitBtn.disabled = false;
      }
    });
    return form;
  };

  /// A file picker that also accepts drag and drop. onFiles(FileList).
  ui.dropzone = function ({ label = "Drop a file here or choose one", accept, multiple = false, onFiles } = {}) {
    const input = el("input", { type: "file", accept: accept || null, multiple: multiple ? true : null, class: "pp-sr" });
    const zone = el("label", { class: "pp-drop" }, PP.icon("plus"), el("span", {}, label), input);
    input.addEventListener("change", () => { if (input.files && input.files.length) onFiles(input.files); input.value = ""; });
    zone.addEventListener("dragover", (e) => { e.preventDefault(); zone.classList.add("is-over"); });
    zone.addEventListener("dragleave", () => zone.classList.remove("is-over"));
    zone.addEventListener("drop", (e) => { e.preventDefault(); zone.classList.remove("is-over"); if (e.dataTransfer && e.dataTransfer.files.length) onFiles(e.dataTransfer.files); });
    return zone;
  };

  /// A short list of next steps ("Get started") with done ticks.
  ui.checklist = function (items) {
    return el("ul", { class: "pp-checklist" }, items.map((i) => el("li", { class: i.done ? "is-done" : null },
      el("span", { class: "pp-check-dot" }, i.done ? PP.icon("check") : null),
      el("div", {}, el("strong", {}, i.title), i.text ? el("p", { class: "pp-muted" }, i.text) : null),
      !i.done && i.action ? (i.action instanceof window.Node ? i.action : ui.button(i.action.label, { size: "sm", ...i.action })) : null)));
  };
})();
