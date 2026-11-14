// Pigeonpost back office (pigeonpost.dev/staff), for Piyote staff only.
//
// The account page's app shell (top bar, grouped left menu, one screen at a time, hash routes) and
// its design tokens, from account.css; this page's own pieces are in staff.css. Every number and
// row comes from the postbox's `/v1/staff/*` routes, which decide who may see what (staff role,
// step-up, audit record). This file only decides which menu entries to draw for the caller's
// roles, so a hidden entry is a convenience, never the control.
//
// Rules kept here:
//  - Non-staff see a plain "not available" page and nothing else.
//  - Message bodies are never requested or shown. Secrets, IBANs and tax ids are masked again on
//    the way to the screen (`mask`), on top of the postbox's own masking.
//  - Every table has a loading, an empty and an error state; a step-up refusal offers the strong
//    sign-in and comes back to the same screen.
//  - The session stays in this tab (sessionStorage), unless the account page already kept one.
//
// Sign-in is Authorization Code + PKCE against the pigeonpost-prod realm, returning to /staff;
// the code is exchanged by the bridge at /api, as on the account page.

(function () {
  "use strict";

  const cfg = window.PIGEONPOST_ACCOUNT || {};
  const scfg = window.PIGEONPOST_STAFF || {};
  const POSTBOX = scfg.postbox || "https://postbox.pigeonpost.dev";
  const API = cfg.apiBase || "/api";
  const SS = window.localStorage;
  const TS = window.sessionStorage;
  const $ = (s, r) => (r || document).querySelector(s);
  const MP = window.PPMarketplace || {};

  // ---- small DOM helpers ------------------------------------------------------------------------

  function el(tag, attrs, ...children) {
    const node = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v == null || v === false) continue;
      if (k === "class") node.className = v;
      else if (k === "text") node.textContent = v;
      else if (k === "html") node.innerHTML = v; // only for our own static icon markup
      else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
      else node.setAttribute(k, v === true ? "" : String(v));
    }
    for (const c of children.flat()) if (c != null && c !== false) node.append(c);
    return node;
  }

  // ---- masking ---------------------------------------------------------------------------------

  /// Everything but the last `keep` characters hidden; short values hidden entirely.
  function mask(value, keep = 4) {
    const s = String(value == null ? "" : value);
    const chars = Array.from(s);
    if (chars.length <= keep * 2) return "••••";
    return "••••" + chars.slice(-keep).join("");
  }

  const SECRET_KEY = /(^|_)(iban|tax_?id|vkn|tckn|secret|token|password|passphrase|api_?key|key_hash|private_key|seed|authorization|cookie|card)($|_)/i;
  const IBAN = /^[A-Z]{2}\d{2}[A-Z0-9 ]{10,32}$/;
  const already = (s) => s.includes("•");

  /// What a field may show. A field named like a secret, or holding an IBAN, is masked unless the
  /// server already masked it.
  function shown(key, value) {
    if (value == null || value === "") return value;
    if (typeof value === "object") return value;
    const s = String(value);
    if (already(s)) return s;
    if (SECRET_KEY.test(String(key || ""))) return mask(s);
    const compact = s.replace(/\s/g, "");
    if (IBAN.test(compact) && /\d{6,}/.test(compact)) return mask(compact);
    return s;
  }

  /// A details object (audit rows) with every secret-named value masked, recursively.
  function maskDeep(v, key) {
    if (Array.isArray(v)) return v.map((x) => maskDeep(x, key));
    if (v && typeof v === "object") {
      const out = {};
      for (const [k, x] of Object.entries(v)) out[k] = maskDeep(x, k);
      return out;
    }
    return shown(key, v);
  }

  // ---- formatting ------------------------------------------------------------------------------

  const fmtDate = (iso) => {
    if (!iso) return "—";
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return String(iso);
    return d.toLocaleString(undefined, { year: "numeric", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
  };
  const fmtDay = (iso) => {
    if (!iso) return "—";
    const d = new Date(iso);
    return Number.isNaN(d.getTime()) ? String(iso) : d.toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });
  };
  const usd = (cents) => {
    if (cents == null || cents === "") return "—";
    const n = Number(cents) / 100;
    try { return new Intl.NumberFormat(undefined, { style: "currency", currency: "USD" }).format(n); } catch (_) { return `$${n.toFixed(2)}`; }
  };
  const minor = (amount, currency) => {
    if (amount == null) return "—";
    try { return new Intl.NumberFormat(undefined, { style: "currency", currency }).format(Number(amount) / 100); } catch (_) { return `${(Number(amount) / 100).toFixed(2)} ${currency || ""}`; }
  };
  const num = (n) => (n == null ? "—" : Number(n).toLocaleString());

  // ---- session ---------------------------------------------------------------------------------

  const sessionStore = () => (TS.getItem("pp_session") ? TS : SS);
  const getToken = () => sessionStore().getItem("pp_session");
  function clearSession() {
    for (const s of [TS, SS]) { s.removeItem("pp_session"); s.removeItem("pp_refresh"); }
  }

  function randomString(len) {
    const a = new Uint8Array(len);
    crypto.getRandomValues(a);
    return Array.from(a, (b) => ("0" + b.toString(16)).slice(-2)).join("");
  }
  async function sha256b64url(input) {
    const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(input));
    let str = "";
    new Uint8Array(digest).forEach((b) => (str += String.fromCharCode(b)));
    return btoa(str).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  }
  const redirectUri = () => window.location.origin + (scfg.redirectPath || "/staff");

  /// Off to the realm. `stepUp` asks for exactly the strength and freshness the postbox named.
  async function login(stepUp) {
    if (!cfg.oidc || !cfg.oidc.clientId) { toast("Sign-in isn't configured."); return; }
    const verifier = randomString(48);
    const state = randomString(12);
    TS.setItem("pp_staff_pkce", verifier);
    TS.setItem("pp_staff_state", state);
    TS.setItem("pp_staff_return", window.location.hash || "");
    const challenge = await sha256b64url(verifier);
    const url = `${cfg.oidc.issuer}/protocol/openid-connect/auth`
      + `?client_id=${encodeURIComponent(cfg.oidc.clientId)}`
      + `&response_type=code&scope=${encodeURIComponent(cfg.oidc.scope || "openid profile")}`
      + `&redirect_uri=${encodeURIComponent(redirectUri())}`
      + `&code_challenge=${challenge}&code_challenge_method=S256&state=${state}`
      + (stepUp && stepUp.acr_values ? `&acr_values=${encodeURIComponent(stepUp.acr_values)}` : "")
      + (stepUp && stepUp.max_age != null ? `&max_age=${encodeURIComponent(stepUp.max_age)}` : "");
    window.location.assign(url);
  }

  async function completeLoginIfReturning() {
    const params = new URLSearchParams(window.location.search);
    const code = params.get("code");
    if (!code && !params.get("error")) return;
    const back = TS.getItem("pp_staff_return") || "";
    const strip = () => history.replaceState({}, "", window.location.pathname + back);
    if (params.get("error")) { strip(); toast("Sign-in did not complete."); return; }
    const state = params.get("state");
    const verifier = TS.getItem("pp_staff_pkce") || "";
    const expected = TS.getItem("pp_staff_state");
    TS.removeItem("pp_staff_pkce"); TS.removeItem("pp_staff_state"); TS.removeItem("pp_staff_return");
    if (!expected || state !== expected) { strip(); toast("Sign-in could not be verified. Try again."); return; }
    try {
      const res = await fetch(`${API}/v1/auth/exchange`, {
        method: "POST", headers: { "content-type": "application/json" },
        body: JSON.stringify({ code, redirectUri: redirectUri(), codeVerifier: verifier }),
      });
      const body = await res.json().catch(() => ({}));
      if (body.session) {
        clearSession();
        TS.setItem("pp_session", body.session);
        if (body.refresh) TS.setItem("pp_refresh", body.refresh);
      } else toast("Could not complete sign-in.");
    } catch (_) { toast("Could not reach the sign-in service."); }
    strip();
  }

  let refreshing = null;
  function refreshSession() {
    const store = sessionStore();
    const refresh = store.getItem("pp_refresh");
    if (!refresh) return Promise.resolve(false);
    if (!refreshing) {
      refreshing = (async () => {
        try {
          const res = await fetch(`${API}/v1/auth/refresh`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ refresh }) });
          const body = await res.json().catch(() => ({}));
          if (res.ok && body.session && store.getItem("pp_refresh") === refresh) {
            store.setItem("pp_session", body.session);
            if (body.refresh) store.setItem("pp_refresh", body.refresh);
            return true;
          }
          return false;
        } catch (_) { return false; } finally { refreshing = null; }
      })();
    }
    return refreshing;
  }

  /// One postbox call. Errors carry `status`, `code` and the flat body (step-up challenges carry
  /// `acr_values` and `max_age`).
  async function pb(path, init, retried) {
    const o = init || {};
    const res = await fetch(POSTBOX + path, {
      method: o.method || "GET",
      headers: Object.assign({ authorization: `Bearer ${getToken()}`, accept: "application/json" }, o.body ? { "content-type": "application/json" } : {}),
      body: o.body,
    });
    const body = res.status === 204 ? {} : await res.json().catch(() => ({}));
    const nested = body && typeof body.error === "object" && body.error ? body.error : null;
    const flat = nested ? { ...(nested.details || {}), error: nested.code || null, detail: nested.message || "" } : body;
    if (res.status === 401 && flat.error !== "step_up_required" && !retried && await refreshSession()) return pb(path, init, true);
    if (!res.ok) {
      const e = new Error(flat.detail || flat.error || `postbox ${res.status}`);
      e.status = res.status; e.code = flat.error || null; e.body = flat;
      throw e;
    }
    return body;
  }
  const post = (path, body, method = "POST") => pb(path, { method, body: JSON.stringify(body || {}) });

  // ---- theme -------------------------------------------------------------------------------------
  // The shared switch (site/theme.js, loaded in <head>) owns the choice, its storage and
  // `html[data-theme]`; this page only places a `data-theme-toggle` button and asks it to draw.
  const refreshTheme = () => { if (window.PPTheme) window.PPTheme.refresh(); };

  // ---- icons -------------------------------------------------------------------------------------

  const ICONS = {
    dashboard: '<rect x="3" y="3" width="7" height="7" rx="1.5"/><rect x="14" y="3" width="7" height="7" rx="1.5"/><rect x="3" y="14" width="7" height="7" rx="1.5"/><rect x="14" y="14" width="7" height="7" rx="1.5"/>',
    accounts: '<circle cx="12" cy="8" r="4"/><path d="M5 21a7 7 0 0 1 14 0"/>',
    handles: '<path d="M15 4 9 20"/><circle cx="18.5" cy="17.5" r="2.5"/>',
    mailboxes: '<rect x="3" y="6" width="18" height="13" rx="2.5"/><path d="m4 8 8 5 8-5"/>',
    orgs: '<path d="M4 21V5a1 1 0 0 1 1-1h8a1 1 0 0 1 1 1v16M14 9h5a1 1 0 0 1 1 1v11M8 8h2M8 12h2M8 16h2M3 21h18"/>',
    plans: '<path d="M7 18a4.5 4.5 0 0 1-.6-8.96A6 6 0 0 1 18 10a4 4 0 0 1-.5 7.97Z"/>',
    credits: '<ellipse cx="12" cy="6.5" rx="7" ry="3"/><path d="M5 6.5v5c0 1.66 3.13 3 7 3s7-1.34 7-3v-5M5 11.5v5c0 1.66 3.13 3 7 3s7-1.34 7-3v-5"/>',
    payouts: '<path d="M3 10h18M5 10v8M9 10v8M15 10v8M19 10v8M3 20h18M12 3l9 5H3Z"/>',
    invoices: '<path d="M6 3h12v18l-3-2-3 2-3-2-3 2Z"/><path d="M9 8h6M9 12h6"/>',
    publishers: '<path d="M12 15V3M7 8l5-5 5 5"/><path d="M4 15v4a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-4"/>',
    reviews: '<path d="M9 11l2 2 4-4"/><path d="M12 3 5 6v5c0 4.5 3 8.3 7 10 4-1.7 7-5.5 7-10V6Z"/>',
    listings: '<path d="M4 9h16l-1.2-4.2A1.5 1.5 0 0 0 17.4 4H6.6a1.5 1.5 0 0 0-1.4.8Z"/><path d="M5 9v10a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1V9M10 20v-5h4v5"/>',
    jobs: '<circle cx="12" cy="12" r="8"/><path d="M12 8v4l3 2"/>',
    nests: '<path d="M12 3 20 7.5v9L12 21l-8-4.5v-9Z"/><path d="m4 7.5 8 4.5 8-4.5M12 12v9"/>',
    calls: '<path d="M5 4h4l2 5-2.5 1.5a11 11 0 0 0 5 5L15 13l5 2v4a2 2 0 0 1-2 2A16 16 0 0 1 3 6a2 2 0 0 1 2-2"/>',
    limits: '<path d="M4 20a8 8 0 1 1 16 0"/><path d="m12 14 4-5"/>',
    flags: '<path d="M5 21V4M5 4h11l-2 4 2 4H5"/>',
    abuse: '<path d="M12 3 2 20h20Z"/><path d="M12 10v4M12 17h.01"/>',
    staff: '<circle cx="9" cy="8" r="3.5"/><path d="M2.5 20a6.5 6.5 0 0 1 13 0M16 4.5a3.5 3.5 0 0 1 0 7M18 14a6 6 0 0 1 3.5 6"/>',
    audit: '<path d="M8 3h8l4 4v14H4V3Z"/><path d="M9 12h6M9 16h4M9 8h3"/>',
    health: '<path d="M3 12h4l2-5 4 10 2-5h6"/>',
    menu: '<path d="M4 7h16M4 12h16M4 17h16"/>',
    close: '<path d="M6 6l12 12M18 6 6 18"/>',
    out: '<path d="M15 4h3a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2h-3M10 16l-4-4 4-4M6 12h10"/>',
    back: '<path d="m15 6-6 6 6 6"/>',
    search: '<circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/>',
    account: '<circle cx="12" cy="8" r="4"/><path d="M5 21a7 7 0 0 1 14 0"/>',
  };
  const icon = (name) => `<svg class="ac-ico" viewBox="0 0 24 24" aria-hidden="true" focusable="false">${ICONS[name] || ""}</svg>`;

  // ---- routes -----------------------------------------------------------------------------------

  const ALL = ["owner", "support", "finance", "reviewer", "ops"];
  // `roles`: who sees the entry (the postbox decides again). `owner_only`: the configured platform
  // owner. `soon`: a later phase, drawn greyed and not clickable.
  const ROUTES = [
    { id: "dashboard", label: "Dashboard", group: "Overview", roles: ALL },
    { id: "accounts", label: "Accounts", group: "People", roles: ["owner", "support"] },
    { id: "handles", label: "Handles", group: "People", roles: ["owner", "support"] },
    { id: "mailboxes", label: "Mailboxes", group: "People", roles: ["owner", "support"] },
    { id: "orgs", label: "Organizations", group: "People", roles: ["owner", "support"] },
    { id: "plans", label: "Entitlements", group: "Plans", roles: ["support", "finance"] },
    { id: "credits", label: "Credits", group: "Money", roles: ["finance"] },
    { id: "payouts", label: "Payouts", group: "Money", roles: ["finance"], soon: true },
    { id: "invoices", label: "Invoices", group: "Money", roles: ["finance"], soon: true },
    { id: "analytics", label: "App analytics", group: "Marketplace", roles: ["finance"] },
    { id: "publishers", label: "Publishers", group: "Marketplace", roles: ["reviewer", "finance"] },
    { id: "reviews", label: "Reviews", group: "Marketplace", roles: ["reviewer"] },
    { id: "listings", label: "Listings & reports", group: "Marketplace", roles: ["reviewer", "ops"] },
    { id: "jobs", label: "Hosted jobs", group: "Marketplace", roles: ["reviewer", "ops"] },
    { id: "nests", label: "Nests", group: "Infrastructure", roles: ["ops"], soon: true },
    { id: "calls", label: "Calls & streams", group: "Infrastructure", roles: ["ops"], soon: true },
    { id: "limits", label: "Usage limits", group: "Infrastructure", roles: ["ops"], soon: true },
    { id: "flags", label: "Feature flags", group: "Infrastructure", roles: ["owner", "ops"], soon: true },
    { id: "abuse", label: "Reports", group: "Trust & safety", roles: ["support"], soon: true },
    { id: "staff", label: "Staff & roles", group: "System", roles: ["owner"], ownerOnly: true },
    { id: "audit", label: "Audit log", group: "System", roles: ["owner"] },
    { id: "health", label: "Health", group: "System", roles: ["ops"], soon: true },
  ];

  /// `#accounts/acct_1` → { route: "accounts", arg: "acct_1" }.
  function parseHash(hash) {
    const raw = String(hash || "").replace(/^#\/?/, "");
    const [route, ...rest] = raw.split("/");
    return { route: route || "", arg: rest.length ? rest.map((p) => { try { return decodeURIComponent(p); } catch (_) { return p; } }).join("/") : null };
  }
  const hashFor = (route, arg) => "#" + route + (arg ? "/" + String(arg).split("/").map(encodeURIComponent).join("/") : "");

  let me = null; // the /v1/staff/me answer
  const canSee = (r) => me && (r.ownerOnly ? me.platform_owner : r.roles.some((x) => me.roles.includes(x)));
  const has = (role) => Boolean(me && me.roles.includes(role));

  // ---- shared pieces ----------------------------------------------------------------------------

  function toast(msg) {
    let t = $("#ac-toast");
    if (!t) { t = el("div", { id: "ac-toast", role: "status", "aria-live": "polite" }); document.body.append(t); }
    t.textContent = msg; t.classList.add("on");
    clearTimeout(toast.timer); toast.timer = setTimeout(() => t.classList.remove("on"), 3200);
  }

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
  };
  const errorText = (e) => (e && e.code && ERRORS[e.code] ? ERRORS[e.code] + (e.code.startsWith("finance_") && e.message && e.message !== e.code ? ` ${e.message}` : "") : e && e.status === 401 ? "Your session ended. Sign in again." : `Something went wrong${e && e.message ? `: ${e.message}` : "."}`);

  /// A state box: loading, empty, or an error with retry (and the strong sign-in when that is
  /// what is missing).
  function stateBox(kind, text, retry, err) {
    const box = el("div", { class: `st-state st-${kind}`, role: kind === "error" ? "alert" : "status" });
    if (kind === "loading") box.append(el("span", { class: "st-spinner", "aria-hidden": "true" }), el("span", { text: text || "Loading…" }));
    else box.append(el("p", { text }));
    if (kind === "error") {
      const actions = el("div", { class: "st-state-actions" });
      if (err && err.code === "step_up_required") {
        const b = err.body || {};
        actions.append(el("button", { type: "button", class: "btn btn-primary btn-small st-stepup", onclick: () => login({ acr_values: b.acr_values || (me && me.step_up.acr_values), max_age: b.max_age ?? 600 }) }, "Confirm it's you"));
      }
      if (retry) actions.append(el("button", { type: "button", class: "btn btn-small", onclick: retry }, "Try again"));
      box.append(actions);
    }
    return box;
  }

  /// A table of `rows` with `cols` ({ key, label, render?, mono?, wide? }). Phone-width layouts
  /// turn rows into labelled cards (staff.css), so every cell carries its column label.
  function table(cols, rows, opts) {
    const o = opts || {};
    if (!rows.length) return stateBox("empty", o.empty || "Nothing here yet.");
    const t = el("table", { class: "st-table" + (o.dense ? " st-dense" : "") });
    if (o.caption) t.append(el("caption", { class: "st-sr", text: o.caption }));
    t.append(el("thead", {}, el("tr", {}, cols.map((c) => el("th", { scope: "col", text: c.label })))));
    const body = el("tbody");
    for (const row of rows) {
      const tr = el("tr", o.onRow ? { class: "st-click", tabindex: "0", onclick: (e) => { if (!e.target.closest("button,a,input,select,textarea,form")) o.onRow(row); }, onkeydown: (e) => { if (e.key === "Enter" && e.target === tr) o.onRow(row); } } : {});
      for (const c of cols) {
        const raw = c.render ? c.render(row) : shown(c.key, row[c.key]);
        const td = el("td", { "data-label": c.label, class: [c.mono ? "mono" : "", c.num ? "st-num" : ""].join(" ").trim() || null });
        if (raw instanceof Node) td.append(raw); else td.textContent = raw == null || raw === "" ? "—" : String(raw);
        tr.append(td);
      }
      body.append(tr);
    }
    t.append(body);
    return el("div", { class: "st-table-wrap" }, t);
  }

  const badge = (text, tone) => el("span", { class: `st-badge${tone ? " st-" + tone : ""}`, text });
  const STATUS_TONE = { active: "ok", succeeded: "ok", published: "ok", approved: "ok", verified: "ok", captured: "ok", running: "ok", completed: "ok",
    past_due: "warn", grace: "warn", pending: "warn", open: "warn", queued: "warn", applied: "warn", uncertain: "warn", blocked: "warn", held: "warn", holding: "warn",
    failed: "bad", suspended: "bad", quarantined: "bad", revoked: "bad", expired: "bad", canceled: "bad", cancelled: "bad", deleted: "bad", refused: "bad", killed: "bad", rejected: "bad" };
  const status = (s) => (s ? badge(String(s).replace(/_/g, " "), STATUS_TONE[s]) : "—");
  const link = (text, route, arg) => el("a", { href: hashFor(route, arg), class: "st-link mono", text });

  /// Label/value pairs.
  function kv(pairs) {
    const dl = el("dl", { class: "st-kv" });
    for (const [k, v, key] of pairs) {
      const val = shown(key || k, v);
      dl.append(el("dt", { text: k }), el("dd", {}, val instanceof Node ? val : val == null || val === "" ? "—" : String(val)));
    }
    return dl;
  }

  function card(title, ...body) {
    return el("section", { class: "ac-card st-card" }, title ? el("h2", { class: "st-card-h", text: title }) : null, ...body);
  }

  /// A screen heading with an optional back link.
  function head(title, lede, back) {
    return el("div", { class: "ac-view-head st-head" },
      back ? el("a", { class: "st-back", href: back, html: icon("back") + "<span>Back</span>" }) : null,
      el("h1", { id: "st-h", tabindex: "-1", text: title }),
      lede ? el("p", { text: lede }) : null);
  }

  /// A searchable, filterable, paged list. `fetchPage(params)` resolves to the postbox page.
  function listScreen(root, spec) {
    const st = { q: spec.initialQ || "", filters: { ...(spec.defaults || {}) }, rows: [], next: null, loading: false, error: null, done: false };
    const form = el("form", { class: "st-toolbar", role: "search", onsubmit: (e) => { e.preventDefault(); st.q = qInput ? qInput.value.trim() : ""; load(true); } });
    const qInput = spec.search === false ? null : el("input", { type: "search", id: `${spec.id}-q`, class: "st-search", placeholder: spec.placeholder || "Search", value: st.q, "aria-label": spec.placeholder || "Search", autocomplete: "off", spellcheck: "false" });
    if (qInput) form.append(el("div", { class: "st-search-box", html: icon("search") }, qInput));
    for (const f of spec.filters || []) {
      const sel = el("select", { id: `${spec.id}-${f.name}`, "aria-label": f.label, onchange: () => { st.filters[f.name] = sel.value; load(true); } },
        f.options.map(([v, t]) => el("option", { value: v, text: t, selected: (st.filters[f.name] || "") === v })));
      form.append(el("label", { class: "st-filter" }, el("span", { class: "st-filter-l", text: f.label }), sel));
    }
    if (qInput || (spec.filters || []).length) form.append(el("button", { type: "submit", class: "btn btn-small btn-primary" }, qInput ? "Search" : "Apply"));
    else form.hidden = true;
    const out = el("div", { class: "st-list", "aria-live": "polite" });
    const more = el("div", { class: "st-more" });
    root.append(form, out, more);

    async function load(reset) {
      if (st.loading) return;
      if (reset) { st.rows = []; st.next = null; st.done = false; }
      st.loading = true; st.error = null; draw();
      const params = new URLSearchParams();
      if (st.q) params.set("q", st.q);
      for (const [k, v] of Object.entries(st.filters)) if (v) params.set(k, v);
      if (st.next) params.set("cursor", st.next);
      params.set("limit", String(spec.limit || 25));
      try {
        const page = await spec.fetch(params);
        st.rows = st.rows.concat(page.data || []);
        st.next = page.next_cursor || null;
        st.done = true;
      } catch (e) { st.error = e; }
      st.loading = false; draw();
    }
    function draw() {
      out.textContent = ""; more.textContent = "";
      if (st.rows.length) out.append(table(spec.columns, st.rows, { onRow: spec.onRow, caption: spec.caption, empty: spec.empty }));
      if (st.loading) (st.rows.length ? more : out).append(stateBox("loading"));
      else if (st.error) out.append(stateBox("error", errorText(st.error), () => load(!st.rows.length), st.error));
      else if (st.done && !st.rows.length) out.append(stateBox("empty", st.q ? `Nothing matches “${st.q}”.` : spec.empty || "Nothing here yet."));
      if (!st.loading && !st.error && st.next) more.append(el("button", { type: "button", class: "btn btn-small", onclick: () => load(false) }, "Load more"));
      if (!st.loading && st.rows.length) more.append(el("span", { class: "muted st-count", text: `${st.rows.length}${st.next ? "+" : ""} shown` }));
    }
    load(true);
    return { reload: () => load(true), state: st };
  }

  /// A detail screen: loading, error, or `draw(data)`.
  async function detail(root, path, drawFn) {
    const box = el("div");
    root.append(box);
    const go = async () => {
      box.textContent = "";
      box.append(stateBox("loading"));
      try {
        const v = await pb(path);
        box.textContent = "";
        drawFn(box, v.data);
      } catch (e) {
        box.textContent = "";
        box.append(e.status === 404 ? stateBox("empty", "Nothing found with that id.") : stateBox("error", errorText(e), go, e));
      }
    };
    await go();
  }

  /// A small form inside a card: fields, a submit button, its own status line.
  function actionForm(spec) {
    const statusLine = el("p", { class: "st-form-status", role: "status", "aria-live": "polite" });
    const form = el("form", { class: "ac-form st-form", id: spec.id, onsubmit: async (e) => {
      e.preventDefault();
      const btn = form.querySelector("button[type=submit]");
      btn.disabled = true; statusLine.textContent = spec.busy || "Working…"; statusLine.className = "st-form-status";
      try {
        const msg = await spec.submit(form);
        statusLine.textContent = msg || "Done."; statusLine.classList.add("st-ok");
        if (spec.after) spec.after();
      } catch (err) {
        statusLine.textContent = "";
        statusLine.append(stateBox("error", errorText(err), null, err));
      } finally { btn.disabled = false; }
    } }, ...spec.fields, el("div", { class: "st-form-actions" }, el("button", { type: "submit", class: `btn btn-small ${spec.danger ? "btn-danger" : "btn-primary"}` }, spec.button)), statusLine);
    return form;
  }
  const field = (id, label, input, hint) => el("label", { for: id, class: "st-field" }, el("span", { text: label }), input, hint ? el("small", { class: "muted", text: hint }) : null);
  const textInput = (id, attrs) => el("input", Object.assign({ id, type: "text", autocomplete: "off", spellcheck: "false" }, attrs || {}));
  const select = (id, options, value) => el("select", { id }, options.map(([v, t]) => el("option", { value: v, text: t, selected: v === value })));

  // ---- screens ----------------------------------------------------------------------------------

  const SCREENS = {};

  SCREENS.dashboard = async (root) => {
    root.append(head("Dashboard", "The platform at a glance. Counts only; open a screen for the people behind them."));
    const tiles = el("div", { class: "st-tiles" });
    const alerts = el("div", { class: "st-alerts" });
    root.append(alerts, tiles);
    tiles.append(stateBox("loading"));
    let d;
    try { d = (await pb("/v1/staff/dashboard")).data; } catch (e) { tiles.textContent = ""; tiles.append(stateBox("error", errorText(e), () => show(), e)); return; }
    tiles.textContent = "";
    const c = d.counts || {};
    const tile = (label, value, sub, route) => el(route && canSee(ROUTES.find((r) => r.id === route)) ? "a" : "div", { class: "st-tile", href: route ? hashFor(route) : null },
      el("span", { class: "st-tile-k", text: label }), el("span", { class: "st-tile-v", text: num(value) }), sub ? el("span", { class: "st-tile-s", text: sub }) : null);
    tiles.append(
      tile("Signups today", c.signups_today, `${num(c.signups_7d)} in 7 days`, "accounts"),
      tile("Accounts", c.accounts, `${num(c.active_accounts_30d)} active in 30 days`, "accounts"),
      tile("Handles", c.handles, null, "handles"),
      tile("Mailboxes", c.mailboxes, null, "mailboxes"),
      tile("Organizations", c.orgs_active, "active", "orgs"),
      tile("Nests running", c.nests_running, null, null),
      tile("Open reviews", c.open_reviews, `${num(c.publisher_applications)} publisher applications`, "reviews"),
      tile("Open reports", c.open_reports, "listing reports", "listings"),
      tile("Failed hosted jobs", c.hosted_jobs_failed_24h, "last 24 hours", "jobs"));
    if (has("finance")) {
      const money = el("div", { class: "st-tile" }, el("span", { class: "st-tile-k", text: "Credit sales today" }), el("span", { class: "st-tile-v", text: "…" }), el("span", { class: "st-tile-s", text: "" }));
      const refunds = el("a", { class: "st-tile", href: hashFor("credits") }, el("span", { class: "st-tile-k", text: "Refunds to resolve" }), el("span", { class: "st-tile-v", text: "…" }), el("span", { class: "st-tile-s", text: "uncertain" }));
      tiles.prepend(money, refunds);
      const today = new Date(); today.setUTCHours(0, 0, 0, 0);
      pb(`/v1/staff/credits/sales?from=${encodeURIComponent(today.toISOString())}&limit=100`).then((v) => {
        const rows = v.data || [];
        money.children[1].textContent = usd(rows.reduce((a, r) => a + Number(r.amount_cents || 0), 0));
        money.children[2].textContent = `${rows.length}${v.next_cursor ? "+" : ""} sales, ${usd(rows.reduce((a, r) => a + Number(r.commission_cents || 0), 0))} commission`;
      }).catch((e) => { money.children[1].textContent = "—"; money.children[2].textContent = errorText(e); });
      pb("/v1/staff/credits/refunds?status=uncertain&limit=100").then((v) => { refunds.children[1].textContent = num((v.data || []).length); })
        .catch((e) => { refunds.children[1].textContent = "—"; refunds.children[2].textContent = errorText(e); });
    }
    const ALERT_TEXT = { charges_refused: (a) => (Number(a.count) === 1 ? "1 hosted-job charge was refused by billing and needs a look." : `${num(a.count)} hosted-job charges were refused by billing and need a look.`), finance_unconfigured: () => "The finance service is not configured on this postbox, so credit screens are off." };
    for (const a of d.alerts || []) alerts.append(el("div", { class: "st-alert", role: "note" }, (ALERT_TEXT[a.code] || (() => a.code))(a)));
    root.append(el("p", { class: "muted st-asof", text: `As of ${fmtDate(d.as_of)}.` }));
  };

  // People

  SCREENS.accounts = async (root, arg) => {
    if (arg) return accountDetail(root, arg);
    root.append(head("Accounts", "Search by account id, sign-in address, handle or mailbox address."));
    listScreen(root, {
      id: "st-acc", placeholder: "acct_…, name@domain, /handle or /k/ address",
      fetch: (p) => pb(`/v1/staff/accounts?${p}`),
      onRow: (r) => go("accounts", r.id), caption: "Accounts",
      columns: [
        { key: "id", label: "Account", mono: true, render: (r) => link(r.id, "accounts", r.id) },
        { key: "sign_in_address", label: "Sign-in address" },
        { key: "handles", label: "Handles", num: true },
        { key: "mailboxes", label: "Mailboxes", num: true },
        { key: "staff", label: "Staff", render: (r) => (r.staff ? badge("staff", "info") : "—") },
        { key: "created_at", label: "Created", render: (r) => fmtDay(r.created_at) },
      ],
      empty: "No accounts yet.",
    });
  };

  async function accountDetail(root, id) {
    root.append(head(id, "Account detail. This view is recorded in the audit log.", "#accounts"));
    await detail(root, `/v1/staff/accounts/${encodeURIComponent(id)}`, (box, a) => {
      const sign = (a.sign_in_identities || []).find((s) => s.provider === "realm");
      box.append(card("Account", kv([
        ["Account id", el("span", { class: "mono", text: a.id })],
        ["Sign-in address", sign ? sign.login : "—"],
        ["Created", fmtDate(a.created_at)],
        ["OIDC subject", a.oidc_subject, "oidc_subject"],
        ["Staff roles", (a.staff_roles || []).filter((r) => !r.revoked_at).map((r) => r.role).join(", ") || "—"],
        ["Deletion", a.deletion ? `requested ${fmtDay(a.deletion.requested_at)}, ${a.deletion.completed_at ? "completed " + fmtDay(a.deletion.completed_at) : "due " + fmtDay(a.deletion.complete_by)}` : "—"],
      ])));
      const grid = el("div", { class: "st-grid" });
      grid.append(
        card("Sign-in identities", table([{ key: "provider", label: "Provider" }, { key: "login", label: "Login" }, { key: "verified_at", label: "Verified", render: (r) => fmtDay(r.verified_at) }], a.sign_in_identities || [], { dense: true, empty: "No proved identities." })),
        card("Handles", table([{ key: "namespace", label: "Namespace", render: (r) => link("/" + r.namespace, "handles", r.namespace) }, { key: "source", label: "Source" }, { key: "expires_at", label: "Expires", render: (r) => fmtDay(r.expires_at) }], a.handles || [], { dense: true, empty: "No handles." })));
      box.append(grid);
      box.append(card("Mailboxes", table([
        { key: "address", label: "Address", mono: true },
        { key: "handle", label: "Handle", mono: true },
        { key: "label", label: "Label" },
        { key: "kind", label: "Kind", render: (r) => badge(r.kind.replace("_", " ")) },
        { key: "created_at", label: "Created", render: (r) => fmtDay(r.created_at) },
      ], a.mailboxes || [], { dense: true, empty: "No mailboxes." })));
      box.append(card("Devices", table([
        { key: "platform", label: "Platform" }, { key: "os", label: "OS" }, { key: "kind", label: "Kind" },
        { key: "mailbox", label: "Mailbox", mono: true }, { key: "push_token", label: "Push token", mono: true },
        { key: "updated_at", label: "Last seen", render: (r) => fmtDate(r.updated_at) },
      ], a.devices || [], { dense: true, empty: "No devices." })));
      const plans = [].concat(
        (a.entitlements || []).map((e) => ({ what: e.plan_code, source: e.source, status: e.status, until: e.period_end || e.paid_through, ref: e.provider_ref })),
        ((a.subscriptions || {}).apple || []).map((s) => ({ what: s.product_id, source: "App Store", status: new Date(s.expires_at) > new Date() ? "active" : "expired", until: s.expires_at, ref: s.reference })),
        ((a.subscriptions || {}).google || []).map((s) => ({ what: s.product_id, source: "Google Play", status: s.state, until: s.expires_at, ref: s.reference })),
        ((a.subscriptions || {}).nest_store || []).map((s) => ({ what: s.package, source: `Nest (${s.store})`, status: s.state, until: s.expires_at, ref: s.reference })));
      box.append(card("Plans & entitlements", table([
        { key: "what", label: "Plan / product" }, { key: "source", label: "Source" }, { key: "status", label: "Status", render: (r) => status(r.status) },
        { key: "until", label: "Until", render: (r) => fmtDay(r.until) }, { key: "ref", label: "Reference", mono: true },
      ], plans, { dense: true, empty: "No plans." })));
      const two = el("div", { class: "st-grid" });
      two.append(
        card("Organizations", table([{ key: "name", label: "Name", render: (r) => link(r.name, "orgs", r.id) }, { key: "relation", label: "Relation" }, { key: "status", label: "Status", render: (r) => status(r.status) }], a.orgs || [], { dense: true, empty: "No organizations." })),
        card("Nests", table([{ key: "label", label: "Label" }, { key: "state", label: "State", render: (r) => status(r.state) }, { key: "funding", label: "Funding" }], a.nests || [], { dense: true, empty: "No Nests." })));
      box.append(two);
      if (has("finance")) {
        const credit = el("div");
        box.append(card("Credit", credit));
        creditOwner(credit, "account", a.id, { compact: true });
      }
      box.append(card("Recent audit", auditTable(a.recent_audit || [], "No recent audit records.")));
    });
  }

  SCREENS.handles = async (root, arg) => {
    if (arg) return handleDetail(root, arg);
    root.append(head("Handles", "Every namespace and who holds it."));
    listScreen(root, {
      id: "st-ns", placeholder: "Namespace or account id",
      filters: [{ name: "source", label: "Source", options: [["", "Any source"], ["apple", "App Store"], ["google", "Google Play"], ["entitlement", "Entitlement"], ["grant", "Grant"], ["test_preview", "Test preview"]] }],
      fetch: (p) => pb(`/v1/staff/handles?${p}`), onRow: (r) => go("handles", r.namespace), caption: "Handles",
      columns: [
        { key: "namespace", label: "Handle", render: (r) => link("/" + r.namespace, "handles", r.namespace) },
        { key: "account_id", label: "Holder", render: (r) => link(r.account_id, "accounts", r.account_id) },
        { key: "source", label: "Source" },
        { key: "mailboxes", label: "Mailboxes", num: true },
        { key: "expires_at", label: "Expires", render: (r) => fmtDay(r.expires_at) },
        { key: "release_at", label: "Release", render: (r) => fmtDay(r.release_at) },
      ],
      empty: "No handles yet.",
    });
  };

  async function handleDetail(root, ns) {
    root.append(head("/" + ns, "Who holds this handle, what lives under it, and its history.", "#handles"));
    await detail(root, `/v1/staff/handles/${encodeURIComponent(ns)}`, (box, h) => {
      box.append(card("Holder", h.holder ? kv([
        ["Account", link(h.holder.account_id, "accounts", h.holder.account_id)], ["Source", h.holder.source],
        ["Verified", fmtDate(h.holder.verified_at)], ["Expires", fmtDay(h.holder.expires_at)], ["Release date", fmtDay(h.holder.release_at)],
      ]) : stateBox("empty", "Nobody holds this namespace now.")));
      box.append(card("Mailboxes under it", table([{ key: "handle", label: "Handle", mono: true }, { key: "address", label: "Address", mono: true }, { key: "account_id", label: "Account", render: (r) => (r.account_id ? link(r.account_id, "accounts", r.account_id) : "—") }, { key: "created_at", label: "Created", render: (r) => fmtDay(r.created_at) }], h.mailboxes || [], { dense: true, empty: "No mailboxes." })));
      const subs = [].concat((h.subscriptions.apple || []), (h.subscriptions.google || []));
      box.append(card("Store subscriptions", table([{ key: "store", label: "Store" }, { key: "product_id", label: "Product" }, { key: "account_id", label: "Account" }, { key: "expires_at", label: "Expires", render: (r) => fmtDay(r.expires_at) }, { key: "reference", label: "Reference", mono: true }], subs, { dense: true, empty: "No store subscriptions." })));
      if ((h.orgs || []).length || (h.holds || []).length) box.append(card("Organization", table([{ key: "name", label: "Org", render: (r) => link(r.name, "orgs", r.id) }, { key: "status", label: "Status", render: (r) => status(r.status) }], h.orgs || [], { dense: true, empty: "No organization." }), table([{ key: "handle", label: "Held handle", mono: true }, { key: "reason", label: "Reason" }, { key: "created_at", label: "Since", render: (r) => fmtDay(r.created_at) }], h.holds || [], { dense: true, empty: "No holds." })));
      box.append(card("History", auditTable(h.history || [], "No recorded grants or releases.")));
    });
  }

  SCREENS.mailboxes = async (root) => {
    root.append(head("Mailboxes", "Identities with their account, handle and last device check-in. Never their messages."));
    listScreen(root, {
      id: "st-mb", placeholder: "Address, handle, label or account",
      filters: [{ name: "kind", label: "Kind", options: [["", "Every kind"], ["account", "Personal"], ["org_agent", "Org agents"], ["nest", "Nests"], ["anonymous", "Anonymous"]] }],
      fetch: (p) => pb(`/v1/staff/mailboxes?${p}`), caption: "Mailboxes",
      columns: [
        { key: "address", label: "Address", mono: true },
        { key: "handle", label: "Handle", mono: true },
        { key: "label", label: "Label" },
        { key: "kind", label: "Kind", render: (r) => badge(r.kind.replace("_", " ")) },
        { key: "account_id", label: "Account", render: (r) => (r.account_id ? link(r.account_id, "accounts", r.account_id) : "—") },
        { key: "last_activity", label: "Last activity", render: (r) => fmtDate(r.last_activity) },
      ],
      empty: "No mailboxes yet.",
    });
  };

  SCREENS.orgs = async (root, arg) => {
    if (arg) return orgDetail(root, arg);
    root.append(head("Organizations", "Orgs, their owners, seats and plans."));
    listScreen(root, {
      id: "st-org", placeholder: "Org id, namespace, name or owner",
      filters: [{ name: "status", label: "Status", options: [["", "Any status"], ["active", "Active"], ["past_due", "Past due"], ["read_only", "Read only"], ["suspended", "Suspended"], ["pending", "Pending"], ["deleted", "Deleted"]] }],
      fetch: (p) => pb(`/v1/staff/orgs?${p}`), onRow: (r) => go("orgs", r.id), caption: "Organizations",
      columns: [
        { key: "name", label: "Name", render: (r) => link(r.name, "orgs", r.id) },
        { key: "namespace", label: "Namespace", render: (r) => "/" + r.namespace },
        { key: "status", label: "Status", render: (r) => status(r.status) },
        { key: "seats", label: "Seats", num: true },
        { key: "active_members", label: "Members", num: true },
        { key: "agents", label: "Agents", num: true },
        { key: "plan_code", label: "Plan", mono: true },
      ],
      empty: "No organizations yet.",
    });
  };

  async function orgDetail(root, id) {
    root.append(head("Organization", "Members, domains, agents and API keys. Keys show their last four characters only.", "#orgs"));
    await detail(root, `/v1/staff/orgs/${encodeURIComponent(id)}`, (box, o) => {
      $("#st-h").textContent = o.name;
      box.append(card("Organization", kv([
        ["Org id", el("span", { class: "mono", text: o.id })], ["Namespace", "/" + o.namespace], ["Status", status(o.status)],
        ["Owner", link(o.owner_account, "accounts", o.owner_account)], ["Seats", `${o.seats} (agents up to ${o.agent_limit})`],
        ["Plan", o.plan_code], ["Period end", fmtDay(o.period_end)], ["Created", fmtDate(o.created_at)],
      ])));
      box.append(card("Members", table([
        { key: "name", label: "Name" }, { key: "handle", label: "Handle", mono: true }, { key: "contact", label: "Contact" },
        { key: "roles", label: "Roles", render: (r) => (r.roles || []).join(", ") || "member" }, { key: "status", label: "Status", render: (r) => status(r.status) },
        { key: "account_id", label: "Account", render: (r) => (r.account_id ? link(r.account_id, "accounts", r.account_id) : "—") },
      ], o.members || [], { dense: true, empty: "No members." })));
      const g = el("div", { class: "st-grid" });
      g.append(
        card("Domains", table([{ key: "domain", label: "Domain" }, { key: "verified_at", label: "Verified", render: (r) => (r.verified_at ? fmtDay(r.verified_at) : badge("unverified", "warn")) }], o.domains || [], { dense: true, empty: "No domains." })),
        card("Entitlements", table([{ key: "plan_code", label: "Plan", mono: true }, { key: "source", label: "Source" }, { key: "status", label: "Status", render: (r) => status(r.status) }], o.entitlements || [], { dense: true, empty: "No entitlements." })));
      box.append(g);
      box.append(card("Agents", table([{ key: "handle", label: "Handle", mono: true }, { key: "label", label: "Label" }, { key: "status", label: "Status", render: (r) => status(r.status) }, { key: "last_seen_at", label: "Last seen", render: (r) => fmtDate(r.last_seen_at) }], o.agents || [], { dense: true, empty: "No agents." })));
      box.append(card("API keys", table([
        { key: "name", label: "Name" }, { key: "masked", label: "Key", mono: true }, { key: "scopes", label: "Scopes", render: (r) => (Array.isArray(r.scopes) ? r.scopes.join(", ") : String(r.scopes || "")) },
        { key: "last_used_at", label: "Last used", render: (r) => fmtDate(r.last_used_at) }, { key: "revoked_at", label: "State", render: (r) => (r.revoked_at ? badge("revoked", "bad") : badge("live", "ok")) },
      ], o.api_keys || [], { dense: true, empty: "No API keys." })));
    });
  }

  // Plans

  SCREENS.plans = async (root) => {
    root.append(head("Entitlements & subscriptions", "Account and org entitlements from every source, and the store records behind them."));
    const tabs = el("div", { class: "st-tabs", role: "tablist" });
    const panel = el("div", { class: "st-tab-panel" });
    root.append(tabs, panel);
    const TABS = [["entitlements", "Entitlements"], ["apple", "App Store"], ["google", "Google Play"], ["nest", "Nest purchases"]];
    const pick = (which) => {
      tabs.querySelectorAll("button").forEach((b) => b.setAttribute("aria-selected", String(b.dataset.tab === which)));
      panel.textContent = "";
      if (which === "entitlements") listScreen(panel, {
        id: "st-ent", placeholder: "Account, org or plan code",
        filters: [{ name: "source", label: "Source", options: [["", "Any source"], ["apple", "Apple"], ["google", "Google"], ["iyzico", "Web (iyzico)"], ["grant", "Grant"], ["test", "Test"]] },
          { name: "status", label: "Status", options: [["", "Any status"], ["active", "Active"], ["grace", "Grace"], ["past_due", "Past due"], ["expired", "Expired"], ["canceled", "Canceled"]] }],
        fetch: (p) => pb(`/v1/staff/entitlements?${p}`), caption: "Entitlements",
        columns: [
          { key: "subject_id", label: "Subject", render: (r) => (r.subject_kind === "org" ? link(r.subject_id, "orgs", r.subject_id) : link(r.subject_id, "accounts", r.subject_id)) },
          { key: "plan_code", label: "Plan", mono: true }, { key: "source", label: "Source" },
          { key: "status", label: "Status", render: (r) => status(r.status) },
          { key: "period_end", label: "Period end", render: (r) => fmtDay(r.period_end) },
          { key: "provider_ref", label: "Reference", mono: true },
        ],
        empty: "No entitlements.",
      });
      else listScreen(panel, {
        id: `st-sub-${which}`, placeholder: "Account, namespace or product",
        defaults: { store: which },
        fetch: (p) => pb(`/v1/staff/subscriptions?${p}`), caption: "Subscriptions",
        columns: [
          { key: "account_id", label: "Account", render: (r) => link(r.account_id, "accounts", r.account_id) },
          { key: "product_id", label: "Product", mono: true },
          { key: which === "nest" ? "package" : "namespace", label: which === "nest" ? "Package" : "Handle" },
          { key: "state", label: "State", render: (r) => status(r.state || (new Date(r.expires_at) > new Date() ? "active" : "expired")) },
          { key: "expires_at", label: "Expires", render: (r) => fmtDay(r.expires_at) },
          { key: "reference", label: "Reference", mono: true },
        ],
        empty: "No store records.",
      });
    };
    for (const [id, label] of TABS) tabs.append(el("button", { type: "button", role: "tab", "data-tab": id, class: "st-tab", onclick: () => pick(id) }, label));
    pick("entitlements");
  };

  // Money

  /// Balance and history for one credit owner, with refunds on purchases.
  function creditOwner(box, kind, id, opts) {
    const o = opts || {};
    const bal = el("div", { class: "st-balance" }, stateBox("loading"));
    const hist = el("div");
    box.append(bal, hist);
    pb(`/v1/staff/credits/owners/${kind}/${encodeURIComponent(id)}/balance`).then((v) => {
      bal.textContent = "";
      bal.append(el("div", { class: "st-tile" }, el("span", { class: "st-tile-k", text: "Available" }), el("span", { class: "st-tile-v", text: usd(v.data.available_cents) })),
        el("div", { class: "st-tile" }, el("span", { class: "st-tile-k", text: "Held" }), el("span", { class: "st-tile-v", text: usd(v.data.held_cents) })));
    }).catch((e) => { bal.textContent = ""; bal.append(stateBox(e.status === 404 ? "empty" : "error", e.status === 404 ? "No credit account yet." : errorText(e), null, e)); });
    if (o.compact) return;
    listScreen(hist, {
      id: "st-crh", search: false, limit: 50,
      fetch: (p) => pb(`/v1/staff/credits/owners/${kind}/${encodeURIComponent(id)}/history?${p}`), caption: "Credit history",
      columns: [
        { key: "at", label: "When", render: (r) => fmtDate(r.at) },
        { key: "type", label: "Type", render: (r) => badge(r.type) },
        { key: "amount_cents", label: "USD credit", render: (r) => usd(r.amount_cents), num: true },
        { key: "charge", label: "Charged", render: (r) => (r.charge ? minor(r.charge.amount_minor, r.charge.currency) : "—") },
        { key: "fx", label: "FX", render: (r) => (r.fx && (r.fx.rate || r.fx.usd_rate) ? String(r.fx.rate || r.fx.usd_rate) : "—") },
        { key: "status", label: "Status", render: (r) => status(r.status) },
        { key: "id", label: "", render: (r) => (r.type === "purchase" && r.status !== "refunded" ? refundButton(r) : r.type === "adjustment" ? el("span", { class: "muted", text: r.reason || "" }) : "") },
      ],
      empty: "No credit activity.",
    });
  }

  function refundButton(p) {
    const wrap = el("div", { class: "st-inline" });
    const btn = el("button", { type: "button", class: "btn btn-small btn-danger", onclick: () => {
      btn.hidden = true;
      const reason = textInput(`st-ref-${p.id}`, { required: true, maxlength: "255", placeholder: "Reason" });
      wrap.append(actionForm({ id: `st-ref-f-${p.id}`, button: "Refund to card", danger: true, busy: "Refunding…",
        fields: [field(`st-ref-${p.id}`, `Refund what is left of ${usd(p.amount_cents - (p.refunded_cents || 0))}`, reason)],
        submit: async () => { const v = await post(`/v1/staff/credits/purchases/${encodeURIComponent(p.id)}/refund`, { reason: reason.value.trim() }); return `Refund ${v.data && v.data.status ? v.data.status : "requested"}.`; } }));
    } }, "Refund");
    wrap.append(btn);
    return wrap;
  }

  SCREENS.credits = async (root, arg) => {
    root.append(head("Credits", "Purchases, balances, holds, sales, refunds and adjustments, from the finance service. Every view is recorded."));
    if (me && !me.finance_configured) root.append(el("div", { class: "st-alert", role: "note", text: "The finance service is not configured on this postbox. These screens will answer “not configured”." }));
    // Owner lookup.
    const [kind0, id0] = arg ? arg.split("/") : ["account", ""];
    const kindSel = select("st-cr-kind", [["account", "Account"], ["org", "Organization"]], kind0 || "account");
    const idIn = textInput("st-cr-id", { value: id0 || "", placeholder: "acct_… or org_…", required: true });
    const ownerBox = el("div");
    const lookup = el("form", { class: "st-toolbar", onsubmit: (e) => { e.preventDefault(); if (idIn.value.trim()) go("credits", `${kindSel.value}/${idIn.value.trim()}`); } },
      el("label", { class: "st-filter" }, el("span", { class: "st-filter-l", text: "Owner" }), kindSel),
      el("div", { class: "st-search-box", html: icon("search") }, idIn),
      el("button", { type: "submit", class: "btn btn-small btn-primary" }, "Look up"));
    root.append(card("Balance and history", lookup, ownerBox));
    if (id0) creditOwner(ownerBox, kind0, id0);
    else ownerBox.append(stateBox("empty", "Look up an account or organization to see its balance, purchases and history."));

    // Refunds.
    const refunds = el("div");
    root.append(card("Refunds", el("p", { class: "muted", text: "Uncertain refunds are stuck: check the iyzico panel, then resolve. Failed gives the buyer the credit back." }), refunds));
    listScreen(refunds, {
      id: "st-crr", search: false, defaults: { status: "uncertain" },
      filters: [{ name: "status", label: "Status", options: [["uncertain", "Uncertain"], ["pending", "Pending"], ["failed", "Failed"], ["succeeded", "Succeeded"], ["", "All"]] }],
      fetch: (p) => pb(`/v1/staff/credits/refunds?${p}`), caption: "Refunds",
      columns: [
        { key: "created_at", label: "Requested", render: (r) => fmtDate(r.created_at) },
        { key: "owner_id", label: "Owner", render: (r) => link(r.owner_id, "credits", `${r.owner_kind}/${r.owner_id}`) },
        { key: "credit_cents", label: "Credit", render: (r) => usd(r.credit_cents), num: true },
        { key: "charge", label: "To card", render: (r) => (r.charge ? minor(r.charge.amount_minor, r.charge.currency) : "—") },
        { key: "status", label: "Status", render: (r) => status(r.status) },
        { key: "refund_id", label: "", render: (r) => (r.status === "uncertain" || r.status === "pending" ? resolveForm(r) : "") },
      ],
      empty: "No refunds in this state.",
    });

    // Sales.
    const sales = el("div");
    root.append(card("Credit sales", sales));
    listScreen(sales, {
      id: "st-crs", placeholder: "Publisher id", search: false,
      fetch: (p) => pb(`/v1/staff/credits/sales?${p}`), caption: "Credit sales",
      columns: [
        { key: "sold_at", label: "Sold", render: (r) => fmtDate(r.sold_at) },
        { key: "listing_id", label: "Listing", mono: true },
        { key: "publisher_id", label: "Publisher", mono: true },
        { key: "amount_cents", label: "Price", render: (r) => usd(r.amount_cents), num: true },
        { key: "commission_cents", label: "Commission", render: (r) => usd(r.commission_cents), num: true },
        { key: "publisher_cents", label: "Publisher share", render: (r) => usd(r.publisher_cents), num: true },
        { key: "owner_id", label: "Buyer", render: (r) => (r.owner_id ? link(r.owner_id, "credits", `${r.owner_kind}/${r.owner_id}`) : "—") },
      ],
      empty: "No credit sales yet.",
    });

    // Adjustments.
    root.append(card("Adjust a balance", adjustForm()));
  };

  function resolveForm(r) {
    const wrap = el("div", { class: "st-inline" });
    const btn = el("button", { type: "button", class: "btn btn-small", onclick: () => {
      btn.hidden = true;
      const p = `st-res-${r.refund_id}`;
      const outcome = select(`${p}-o`, [["", "Choose…"], ["succeeded", "iyzico refunded it (close)"], ["failed", "iyzico did not (give the credit back)"]], "");
      const reason = textInput(`${p}-r`, { required: true, maxlength: "255" });
      const ref = textInput(`${p}-ref`, { maxlength: "128" });
      wrap.append(actionForm({ id: `${p}-f`, button: "Resolve", busy: "Resolving…",
        fields: [field(`${p}-o`, "What the iyzico panel shows", outcome), field(`${p}-r`, "Reason", reason), field(`${p}-ref`, "iyzico reference (optional)", ref)],
        submit: async () => {
          if (!outcome.value) throw Object.assign(new Error("Choose what the iyzico panel shows."), { code: null });
          await post(`/v1/staff/credits/refunds/${encodeURIComponent(r.refund_id)}/resolve`, { outcome: outcome.value, reason: reason.value.trim(), ...(ref.value.trim() ? { provider_reference: ref.value.trim() } : {}) });
          return "Resolved.";
        } }));
    } }, "Resolve");
    wrap.append(btn);
    return wrap;
  }

  function adjustForm() {
    // One key per adjustment: kept across a retry of a failed submit (so a retry adjusts at most
    // once), replaced after a success together with the form, so the next one is a new adjustment.
    let key = "adj-" + randomString(10);
    const kind = select("st-adj-kind", [["account", "Account"], ["org", "Organization"]], "account");
    const id = textInput("st-adj-id", { required: true, placeholder: "acct_…" });
    const amount = el("input", { id: "st-adj-amount", type: "number", step: "0.01", min: "-10000", max: "10000", required: true, placeholder: "e.g. 5.00 or -5.00" });
    const reason = textInput("st-adj-reason", { required: true, maxlength: "255" });
    const confirm = el("input", { id: "st-adj-confirm", type: "checkbox", required: true });
    return actionForm({
      id: "st-adj", button: "Apply adjustment", busy: "Applying…",
      fields: [
        el("div", { class: "st-form-row" }, field("st-adj-kind", "Owner", kind), field("st-adj-id", "Owner id", id)),
        el("div", { class: "st-form-row" }, field("st-adj-amount", "Amount in USD", amount, "Positive adds credit, negative removes it. At most $10,000 either way."), field("st-adj-reason", "Reason", reason)),
        el("label", { class: "mp-check st-check", for: "st-adj-confirm" }, confirm, " I checked the owner and the amount. This is recorded under my name."),
      ],
      submit: async (form) => {
        const cents = Math.round(Number(amount.value) * 100);
        if (!Number.isFinite(cents) || cents === 0 || Math.abs(cents) > 1000000) throw Object.assign(new Error("Enter a non-zero amount up to $10,000."), { code: null });
        const who = id.value.trim();
        await post("/v1/staff/credits/adjustments", { owner_kind: kind.value, owner_id: who, amount_cents: cents, reason: reason.value.trim(), idempotency_key: key });
        key = "adj-" + randomString(10);
        form.reset();
        return `Adjusted ${who} by ${usd(cents)}.`;
      },
    });
  }

  // Marketplace

  const mpDeps = () => ({ pb: (path, init) => pb(path, init), toast, stepUp: (challenge) => login(challenge) });

  SCREENS.analytics = async (root) => {
    if (MP.analytics) MP.analytics(root, mpDeps(), {staff:true});
  };

  SCREENS.publishers = async (root) => {
    root.append(head("Publishers", "Applications and certification. Payout details come from the finance service, masked."));
    if (has("reviewer")) {
      const box = el("section", { class: "ac-card st-card", id: "st-mp-apps" });
      root.append(box);
      if (MP.staff) MP.staff(box, mpDeps(), { only: ["applications"], embedded: true });
    }
    if (has("finance")) {
      const outBox = el("div");
      const pid = textInput("st-pp-id", { required: true, placeholder: "pub_…" });
      root.append(card("Payout details",
        el("form", { class: "st-toolbar", onsubmit: async (e) => {
          e.preventDefault(); outBox.textContent = ""; outBox.append(stateBox("loading"));
          try {
            const p = (await pb(`/v1/staff/publishers/${encodeURIComponent(pid.value.trim())}/payout-profile`)).data;
            outBox.textContent = "";
            outBox.append(kv([["Publisher", p.publisher_id], ["IBAN", p.iban, "iban"], ["Bank", p.bank_name], ["Country", p.country], ["Currency", p.currency], ["Status", status(p.status)], ["Verified", fmtDate(p.verified_at)]]));
          } catch (err) { outBox.textContent = ""; outBox.append(err.status === 404 ? stateBox("empty", "No payout details on file.") : stateBox("error", errorText(err), null, err)); }
        } }, el("div", { class: "st-search-box", html: icon("search") }, pid), el("button", { type: "submit", class: "btn btn-small btn-primary" }, "Look up")),
        outBox));
    }
  };

  SCREENS.reviews = async (root) => {
    root.append(head("Reviews", "Release and integration reviews, decisions over the exact release digest, and release security."));
    const box = el("section", { class: "ac-card st-card", id: "st-mp-reviews" });
    root.append(box);
    if (MP.staff) MP.staff(box, mpDeps(), { only: ["reviews"], embedded: true });
  };

  SCREENS.listings = async (root) => {
    root.append(head("Listings & reports", "Moderation, buyer reports, kill switches, publisher quotas and source builds."));
    const lst = el("div");
    root.append(card("Listings", lst));
    listScreen(lst, {
      id: "st-lst", search: false,
      filters: [{ name: "status", label: "Status", options: [["", "Any status"], ["published", "Published"], ["suspended", "Suspended"], ["draft", "Draft"], ["withdrawn", "Withdrawn"]] }],
      fetch: (p) => pb(`/v1/staff/listings?${p}`), caption: "Listings",
      columns: [
        { key: "title", label: "Listing" },
        { key: "publisher", label: "Publisher", render: (r) => (r.publisher && r.publisher.display_name) || r.publisher_id },
        { key: "status", label: "Status", render: (r) => status(r.status) },
        { key: "security_state", label: "Security", render: (r) => (r.security_state && r.security_state !== "normal" ? status(r.security_state) : "—") },
        { key: "open_reports", label: "Reports", num: true },
        { key: "id", label: "", render: (r) => listingActions(r) },
      ],
      empty: "No listings.",
    });
    const reps = el("div");
    root.append(card("Buyer reports", reps));
    listScreen(reps, {
      id: "st-rep", search: false, defaults: { status: "open" },
      filters: [{ name: "status", label: "Status", options: [["open", "Open"], ["closed", "Closed"]] }],
      fetch: (p) => pb(`/v1/staff/listing-reports?${p}`), caption: "Reports",
      columns: [
        { key: "created_at", label: "Reported", render: (r) => fmtDate(r.created_at) },
        { key: "listing", label: "Listing", render: (r) => (r.listing ? r.listing.title : r.listing_id) },
        { key: "reason", label: "Reason", render: (r) => badge(r.reason) },
        { key: "detail", label: "Detail" },
        { key: "id", label: "", render: (r) => (r.status === "open" ? closeReport(r) : el("span", { class: "muted", text: r.resolution || "" })) },
      ],
      empty: "No reports in this state.",
    });
    root.append(card("Publisher controls", publisherControls()));
    root.append(card("Source builds", sourceBuilds()));
  };

  function reasonAction(label, idp, run, danger) {
    const wrap = el("div", { class: "st-inline" });
    const btn = el("button", { type: "button", class: `btn btn-small${danger ? " btn-danger" : ""}`, onclick: () => {
      btn.hidden = true;
      const reason = textInput(`${idp}-r`, { required: true, maxlength: "2000", placeholder: "Reason" });
      wrap.append(actionForm({ id: `${idp}-f`, button: label, danger, fields: [field(`${idp}-r`, "Reason", reason)], submit: () => run(reason.value.trim()) }));
    } }, label);
    wrap.append(btn);
    return wrap;
  }

  function listingActions(l) {
    const box = el("div", { class: "st-actions" });
    const id = encodeURIComponent(l.id);
    box.append(l.status === "suspended"
      ? reasonAction("Reinstate", `st-l-r-${l.id}`, async (reason) => { await post(`/v1/staff/listings/${id}/moderation`, { action: "reinstate", reason }); return "Reinstated."; })
      : reasonAction("Suspend", `st-l-s-${l.id}`, async (reason) => { await post(`/v1/staff/listings/${id}/moderation`, { action: "suspend", reason }); return "Suspended. It is out of the catalog."; }, true));
    box.append(reasonAction("Kill switch", `st-l-k-${l.id}`, async (reason) => { await post(`/v1/staff/marketplace/listings/${id}/kill`, { action: "kill", reason }); return "Killed. Its workers are stopping."; }, true));
    return box;
  }

  function closeReport(r) {
    const wrap = el("div", { class: "st-inline" });
    const btn = el("button", { type: "button", class: "btn btn-small", onclick: () => {
      btn.hidden = true;
      const res = textInput(`st-rep-${r.id}`, { required: true, maxlength: "500", placeholder: "What was done" });
      wrap.append(actionForm({ id: `st-rep-f-${r.id}`, button: "Close report", fields: [field(`st-rep-${r.id}`, "Resolution", res)], submit: async () => { await post(`/v1/staff/listing-reports/${encodeURIComponent(r.id)}/close`, { resolution: res.value.trim() }); return "Closed."; } }));
    } }, "Close");
    wrap.append(btn);
    return wrap;
  }

  function publisherControls() {
    const pid = textInput("st-pc-id", { required: true, placeholder: "pub_…" });
    const n = (id, label, value) => field(id, label, el("input", { id, type: "number", min: "0", step: "1", required: true, value: String(value) }));
    const reason = textInput("st-pc-reason", { required: true, maxlength: "2000" });
    const quotas = actionForm({
      id: "st-pc-q", button: "Set quotas",
      fields: [field("st-pc-id", "Publisher id", pid), el("div", { class: "st-form-row" }, n("st-pc-b", "Builds per day", 5), n("st-pc-w", "Workers", 2), n("st-pc-t", "Broker tokens a month", 1000000), n("st-pc-j", "Jobs per minute", 60)), field("st-pc-reason", "Reason", reason)],
      submit: async (f) => {
        const v = (id) => parseInt(f.querySelector(`#${id}`).value, 10);
        await post(`/v1/staff/marketplace/publishers/${encodeURIComponent(pid.value.trim())}/quotas`, { builds_per_day: v("st-pc-b"), max_workers: v("st-pc-w"), monthly_broker_tokens: v("st-pc-t"), jobs_per_minute: v("st-pc-j"), reason: reason.value.trim() }, "PUT");
        return "Quotas set.";
      },
    });
    const kpid = textInput("st-pk-id", { required: true, placeholder: "pub_…" });
    const kact = select("st-pk-a", [["kill", "Kill every listing of this publisher"], ["restore", "Restore"]], "kill");
    const kreason = textInput("st-pk-r", { required: true, maxlength: "2000" });
    const kill = actionForm({ id: "st-pk", button: "Apply", danger: true, fields: [el("div", { class: "st-form-row" }, field("st-pk-id", "Publisher id", kpid), field("st-pk-a", "Action", kact)), field("st-pk-r", "Reason", kreason)],
      submit: async () => { await post(`/v1/staff/marketplace/publishers/${encodeURIComponent(kpid.value.trim())}/kill`, { action: kact.value, reason: kreason.value.trim() }); return kact.value === "kill" ? "Publisher killed." : "Publisher restored."; } });
    return el("div", { class: "st-split" }, el("div", {}, el("h3", { class: "st-sub", text: "Quotas" }), quotas), el("div", {}, el("h3", { class: "st-sub", text: "Kill switch" }), kill));
  }

  function sourceBuilds() {
    const pid = textInput("st-sb-p", { required: true, placeholder: "pub_…" });
    const repo = textInput("st-sb-u", { required: true, placeholder: "https://github.com/…" });
    const commit = textInput("st-sb-c", { required: true, placeholder: "40 or 64 hex characters" });
    const out = el("div");
    const req = actionForm({ id: "st-sb", button: "Request build", fields: [field("st-sb-p", "Publisher id", pid), field("st-sb-u", "Repository", repo), field("st-sb-c", "Commit", commit)],
      submit: async () => { const v = await post("/v1/staff/marketplace/source-builds", { publisher_id: pid.value.trim(), repo_url: repo.value.trim(), commit: commit.value.trim() }); return `Build ${(v.data && (v.data.id || v.data.build_id)) || ""} requested.`; } });
    const bid = textInput("st-sb-id", { required: true, placeholder: "Build id" });
    const look = el("form", { class: "st-toolbar", onsubmit: async (e) => {
      e.preventDefault(); out.textContent = ""; out.append(stateBox("loading"));
      try { const b = (await pb(`/v1/staff/marketplace/source-builds/${encodeURIComponent(bid.value.trim())}`)).data || {}; out.textContent = ""; out.append(kv(Object.entries(b).filter(([, v]) => typeof v !== "object").map(([k, v]) => [k.replace(/_/g, " "), v, k]))); }
      catch (err) { out.textContent = ""; out.append(stateBox(err.status === 404 ? "empty" : "error", err.status === 404 ? "No such build." : errorText(err), null, err)); }
    } }, el("div", { class: "st-search-box", html: icon("search") }, bid), el("button", { type: "submit", class: "btn btn-small btn-primary" }, "Look up"));
    return el("div", { class: "st-split" }, el("div", {}, el("h3", { class: "st-sub", text: "Request" }), req), el("div", {}, el("h3", { class: "st-sub", text: "Look up" }), look, out));
  }

  SCREENS.jobs = async (root) => {
    root.append(head("Hosted jobs", "Publisher-hosted jobs: states, delivery and charge state. Never their input or results."));
    listScreen(root, {
      id: "st-jobs", search: false,
      filters: [{ name: "state", label: "State", options: [["", "Every state"], ["queued", "Queued"], ["running", "Running"], ["succeeded", "Succeeded"], ["failed", "Failed"], ["expired", "Expired"], ["cancelled", "Cancelled"]] }],
      fetch: async (p) => { p.delete("limit"); p.delete("cursor"); return pb(`/v1/staff/hosted-jobs?${p}`); }, caption: "Hosted jobs",
      columns: [
        { key: "created_at", label: "Created", render: (r) => fmtDate(r.created_at) },
        { key: "listing_title", label: "Listing" },
        { key: "operation", label: "Operation", mono: true },
        { key: "execution", label: "State", render: (r) => status(r.execution) },
        { key: "delivery", label: "Delivery", render: (r) => (r.delivery ? status(r.delivery) : "—") },
        { key: "charge_state", label: "Charge", render: (r) => (r.charge_state ? status(r.charge_state) : "free") },
        { key: "error_code", label: "Error", mono: true },
      ],
      empty: "No hosted jobs.",
    });
  };

  // System

  SCREENS.staff = async (root) => {
    root.append(head("Staff & roles", "Who holds which staff role. Only the platform owner grants or revokes; every change is recorded."));
    const list = el("div");
    const draw = async () => {
      list.textContent = ""; list.append(stateBox("loading"));
      try {
        const rows = (await pb("/v1/staff/roles")).data || [];
        list.textContent = "";
        list.append(table([
          { key: "account_id", label: "Account", render: (r) => link(r.account_id, "accounts", r.account_id) },
          { key: "role", label: "Role", render: (r) => badge(r.role, "info") },
          { key: "granted_by", label: "Granted by", mono: true },
          { key: "granted_at", label: "Since", render: (r) => fmtDay(r.granted_at) },
          { key: "id", label: "", render: (r) => reasonlessRevoke(r, draw) },
        ], rows, { empty: "No staff role rows yet. The platform owner holds owner without one." }));
      } catch (e) { list.textContent = ""; list.append(stateBox("error", errorText(e), draw, e)); }
    };
    root.append(card("Roles", list));
    const acct = textInput("st-sr-acct", { required: true, placeholder: "acct_…" });
    const role = select("st-sr-role", ALL.map((r) => [r, r]), "support");
    root.append(card("Grant a role", actionForm({ id: "st-sr", button: "Grant", fields: [el("div", { class: "st-form-row" }, field("st-sr-acct", "Account id", acct), field("st-sr-role", "Role", role))],
      submit: async () => { await post("/v1/staff/roles", { account_id: acct.value.trim(), role: role.value }); return `Granted ${role.value}.`; }, after: draw })),
      el("p", { class: "muted st-note", text: "owner: people and the audit log · support: people and plans · finance: money and plans · reviewer: marketplace review and operations · ops: marketplace operations." }));
    draw();
  };

  function reasonlessRevoke(r, after) {
    const b = el("button", { type: "button", class: "btn btn-small btn-danger", "aria-label": `Revoke ${r.role} from ${r.account_id}`, onclick: async () => {
      if (b.dataset.armed !== "1") { b.dataset.armed = "1"; b.textContent = "Confirm revoke"; return; }
      b.disabled = true;
      try { await pb(`/v1/staff/roles/${encodeURIComponent(r.id)}`, { method: "DELETE" }); toast(`Revoked ${r.role}.`); after(); } catch (e) { b.disabled = false; toast(errorText(e)); }
    } }, "Revoke");
    return b;
  }

  /// One CSV cell, quoted. A cell a spreadsheet would read as a formula (`=`, `+`, `-`, `@`, tab,
  /// carriage return) gets a leading `'`, so an audit export can never run anything.
  function csvCell(x) {
    let v = String(x == null ? "" : x);
    if (/^[=+\-@\t\r]/.test(v)) v = "'" + v;
    return `"${v.replace(/"/g, '""')}"`;
  }

  function auditTable(rows, empty) {
    return table([
      { key: "at", label: "When", render: (r) => fmtDate(r.at) },
      { key: "action", label: "Action", mono: true },
      { key: "actor_account", label: "Actor", render: (r) => (r.actor_account ? el("span", { class: "mono", text: r.actor_account }) : badge(r.actor_kind)) },
      { key: "target", label: "Target", mono: true },
      { key: "details", label: "Details", render: (r) => (r.details && Object.keys(r.details).length ? el("code", { class: "st-details", text: JSON.stringify(maskDeep(r.details)) }) : "—") },
    ], rows, { dense: true, empty });
  }

  SCREENS.audit = async (root) => {
    root.append(head("Audit log", "Every recorded action on the platform, newest first. Reading it is recorded too."));
    const f = { actor: textInput("st-au-actor", { placeholder: "acct_…" }), action: textInput("st-au-action", { placeholder: "staff. or nest.create" }), target: textInput("st-au-target", { placeholder: "account: or org:org_…" }), from: el("input", { id: "st-au-from", type: "date" }), to: el("input", { id: "st-au-to", type: "date" }) };
    const st = { rows: [], next: null, loading: false, error: null };
    const out = el("div", { "aria-live": "polite" });
    const more = el("div", { class: "st-more" });
    const params = (extra) => {
      const p = new URLSearchParams();
      for (const k of ["actor", "action", "target"]) if (f[k].value.trim()) p.set(k, f[k].value.trim());
      if (f.from.value) p.set("from", f.from.value);
      if (f.to.value) p.set("to", String(Math.floor(new Date(f.to.value + "T23:59:59Z").getTime() / 1000)));
      for (const [k, v] of Object.entries(extra || {})) p.set(k, v);
      return p;
    };
    const load = async (reset) => {
      if (reset) { st.rows = []; st.next = null; }
      st.loading = true; st.error = null; draw();
      try { const v = await pb(`/v1/staff/audit?${params(st.next ? { cursor: st.next, limit: "50" } : { limit: "50" })}`); st.rows = st.rows.concat(v.data || []); st.next = v.next_cursor; } catch (e) { st.error = e; }
      st.loading = false; draw();
    };
    const draw = () => {
      out.textContent = ""; more.textContent = "";
      if (st.rows.length) out.append(auditTable(st.rows, ""));
      if (st.loading) (st.rows.length ? more : out).append(stateBox("loading"));
      else if (st.error) out.append(stateBox("error", errorText(st.error), () => load(true), st.error));
      else if (!st.rows.length) out.append(stateBox("empty", "No records match."));
      if (!st.loading && st.next) more.append(el("button", { type: "button", class: "btn btn-small", onclick: () => load(false) }, "Load more"));
    };
    const exportCsv = async () => {
      try {
        const v = await pb(`/v1/staff/audit?${params({ export: "1", limit: "500" })}`);
        const esc = (x) => csvCell(x);
        const lines = [["id", "at", "actor_account", "actor_kind", "action", "target", "org_id", "details"].join(",")]
          .concat((v.data || []).map((r) => [r.id, r.at, r.actor_account, r.actor_kind, r.action, r.target, r.org_id, JSON.stringify(maskDeep(r.details))].map(esc).join(",")));
        const url = URL.createObjectURL(new Blob([lines.join("\n")], { type: "text/csv" }));
        const a = el("a", { href: url, download: `pigeonpost-audit-${new Date().toISOString().slice(0, 10)}.csv` });
        document.body.append(a); a.click(); a.remove(); setTimeout(() => URL.revokeObjectURL(url), 1000);
        toast(`Exported ${(v.data || []).length} records.`);
      } catch (e) { toast(errorText(e)); }
    };
    const form = el("form", { class: "st-toolbar st-audit-filters", onsubmit: (e) => { e.preventDefault(); load(true); } },
      field("st-au-actor", "Actor", f.actor), field("st-au-action", "Action", f.action), field("st-au-target", "Target", f.target),
      field("st-au-from", "From", f.from), field("st-au-to", "To", f.to),
      el("div", { class: "st-form-actions" }, el("button", { type: "submit", class: "btn btn-small btn-primary" }, "Filter"), el("button", { type: "button", class: "btn btn-small", onclick: exportCsv }, "Export CSV")));
    root.append(form, out, more);
    load(true);
  };

  SCREENS.soon = async (root, _arg, spec) => {
    root.append(head(spec.label, ""), card(null, stateBox("empty", "Coming in a later phase of the back office.")));
  };

  // ---- the shell --------------------------------------------------------------------------------

  function claimsName() {
    try {
      const part = String(getToken() || "").split(".")[1];
      const json = JSON.parse(new TextDecoder().decode(Uint8Array.from(atob(part.replace(/-/g, "+").replace(/_/g, "/")), (c) => c.charCodeAt(0))));
      return String(json.name || json.preferred_username || "");
    } catch (_) { return ""; }
  }

  function navHtml() {
    const groups = [];
    for (const r of ROUTES) {
      if (!canSee(r)) continue;
      let g = groups.find((x) => x.key === r.group);
      if (!g) { g = { key: r.group, items: [] }; groups.push(g); }
      g.items.push(r);
    }
    const slug = (s) => s.toLowerCase().replace(/[^a-z]+/g, "-");
    return groups.map((g) => `<div class="ac-nav-group"><p class="ac-nav-label" id="st-nav-${slug(g.key)}">${g.key.replace(/&/g, "&amp;")}</p><ul aria-labelledby="st-nav-${slug(g.key)}">${g.items.map((r) => r.soon
      ? `<li><span class="ac-nav-link st-soon" aria-disabled="true" title="Coming soon">${icon(r.id)}<span>${r.label.replace(/&/g, "&amp;")}</span><em>soon</em></span></li>`
      : `<li><a class="ac-nav-link" href="#${r.id}" data-route="${r.id}">${icon(r.id)}<span>${r.label.replace(/&/g, "&amp;")}</span></a></li>`).join("")}</ul></div>`).join("");
  }

  function renderShell(rootEl) {
    document.body.classList.add("ac-shell", "st-body");
    const name = claimsName() || (me && me.account_id) || "Staff";
    rootEl.innerHTML = `
      <div class="ac-app st-app" id="ac-app">
        <header class="ac-top">
          <button type="button" class="ac-icon-btn ac-nav-toggle" id="ac-nav-toggle" aria-controls="ac-side" aria-expanded="false" aria-label="Open navigation">${icon("menu")}</button>
          <a class="ac-brand" href="#dashboard" aria-label="Back office home"><picture class="brand-logo ac-logo">
            <source srcset="/logo_white.png" media="(prefers-color-scheme: dark)">
            <img src="/logo.png" alt="" width="640" height="213">
          </picture></a>
          <span class="st-pill" aria-label="Back office">Back office</span>
          <span class="ac-top-sep" aria-hidden="true"></span>
          <span class="ac-top-title" id="ac-top-title" aria-hidden="true"></span>
          <span class="ac-top-fill"></span>
          <button type="button" class="theme-toggle" id="st-theme" data-theme-toggle aria-label="Theme: System"></button>
          <div class="ac-user">
            <button type="button" class="ac-user-btn" id="ac-user-btn" aria-haspopup="true" aria-expanded="false" aria-controls="ac-user-menu">
              <span class="ac-avatar" aria-hidden="true"></span><span class="ac-user-name"></span>
            </button>
            <div class="ac-user-menu" id="ac-user-menu" hidden>
              <div class="ac-user-id"><strong></strong><span class="st-roles"></span></div>
              <a class="ac-menu-item" href="/account">${icon("account")}Your account</a>
              <button type="button" class="ac-menu-item" id="st-logout">${icon("out")}Sign out</button>
            </div>
          </div>
        </header>
        <nav class="ac-side" id="ac-side" aria-label="Back office sections">
          <div class="ac-side-head"><span>Back office</span><button type="button" class="ac-icon-btn" id="ac-nav-close" aria-label="Close navigation">${icon("close")}</button></div>
          ${navHtml()}
          <div class="ac-side-foot"><a href="/account">Your account</a><a href="https://inbox.pigeonpost.dev">Inbox</a></div>
        </nav>
        <div class="ac-scrim" id="ac-scrim" hidden></div>
        <main class="ac-content st-content" id="ac-content"><section class="ac-view st-view" id="st-view" aria-labelledby="st-h"></section></main>
      </div>`;
    // Names and roles go in as text, never markup.
    $(".ac-avatar").textContent = (name.trim().charAt(0) || "?").toUpperCase();
    $(".ac-user-name").textContent = name;
    $(".ac-user-id strong").textContent = name;
    $(".st-roles").textContent = `${(me.roles || []).join(", ")}${me.platform_owner ? " · platform owner" : ""}`;
    refreshTheme();
    $("#st-logout").onclick = () => { clearSession(); window.location.assign("/account"); };
    const app = $("#ac-app");
    const openDrawer = () => { app.classList.add("ac-nav-open"); $("#ac-nav-toggle").setAttribute("aria-expanded", "true"); $("#ac-scrim").hidden = false; const f = $("#ac-side [aria-current]") || $("#ac-side a"); if (f) f.focus(); };
    const closeDrawer = () => { app.classList.remove("ac-nav-open"); $("#ac-nav-toggle").setAttribute("aria-expanded", "false"); $("#ac-scrim").hidden = true; };
    $("#ac-nav-toggle").onclick = () => (app.classList.contains("ac-nav-open") ? closeDrawer() : openDrawer());
    $("#ac-nav-close").onclick = closeDrawer;
    $("#ac-scrim").onclick = closeDrawer;
    $("#ac-side").addEventListener("click", (e) => { if (e.target.closest("a")) closeDrawer(); });
    const btn = $("#ac-user-btn"), menu = $("#ac-user-menu");
    btn.onclick = (e) => { e.stopPropagation(); menu.hidden = !menu.hidden; btn.setAttribute("aria-expanded", String(!menu.hidden)); };
    document.addEventListener("click", (e) => { if (!menu.hidden && !menu.contains(e.target)) { menu.hidden = true; btn.setAttribute("aria-expanded", "false"); } });
    document.addEventListener("keydown", (e) => { if (e.key === "Escape") { closeDrawer(); if (!menu.hidden) { menu.hidden = true; btn.focus(); } } });
    renderShell.closeDrawer = closeDrawer;
  }

  function go(route, arg) {
    const h = hashFor(route, arg);
    if (window.location.hash === h) show(); else window.location.hash = h;
  }

  let showing = 0;
  async function show() {
    const view = $("#st-view"); if (!view) return;
    const { route, arg } = parseHash(window.location.hash);
    let spec = ROUTES.find((r) => r.id === route);
    if (!spec || !canSee(spec)) { spec = ROUTES[0]; if (route) history.replaceState(null, "", window.location.pathname + "#dashboard"); }
    const gen = ++showing;
    view.textContent = "";
    document.querySelectorAll("#ac-side [data-route]").forEach((a) => { if (a.dataset.route === spec.id) a.setAttribute("aria-current", "page"); else a.removeAttribute("aria-current"); });
    $("#ac-top-title").textContent = spec.label;
    document.title = `${spec.label} · Pigeonpost back office`;
    if (renderShell.closeDrawer) renderShell.closeDrawer();
    try { window.scrollTo(0, 0); } catch (_) { /* jsdom */ }
    const screen = spec.soon ? SCREENS.soon : SCREENS[spec.id];
    await screen(view, arg, spec);
    if (gen === showing) { const h = $("#st-h"); if (h && show.focus) try { h.focus({ preventScroll: true }); } catch (_) { h.focus(); } }
    show.focus = true;
  }

  function renderPlain(rootEl, title, text, action) {
    document.body.classList.remove("ac-shell");
    document.title = `${title} · Pigeonpost`;
    rootEl.textContent = "";
    rootEl.append(el("div", { class: "ac-card ac-center st-plain", id: "st-plain" },
      el("h1", { class: "st-plain-h", text: title }), el("p", { class: "muted", text }),
      action || el("p", {}, el("a", { href: "/", text: "Go to pigeonpost.dev" }))));
  }

  async function start() {
    const rootEl = $("#staff-root"); if (!rootEl) return;
    await completeLoginIfReturning();
    if (!getToken()) {
      renderPlain(rootEl, "Back office", "Sign in with your Pigeonpost staff account.",
        el("div", { class: "ac-actions" }, el("button", { type: "button", class: "btn btn-primary", id: "st-signin", onclick: () => login() }, "Sign in")));
      return;
    }
    try {
      me = (await pb("/v1/staff/me")).data;
    } catch (e) {
      if (e.status === 401 && e.code !== "step_up_required") {
        clearSession();
        renderPlain(rootEl, "Back office", "Your session ended. Sign in again.", el("div", { class: "ac-actions" }, el("button", { type: "button", class: "btn btn-primary", id: "st-signin", onclick: () => login() }, "Sign in")));
        return;
      }
      if (e.status === 403 || e.status === 404) { renderPlain(rootEl, "Not available", "This page is not available."); return; }
      renderPlain(rootEl, "Back office", errorText(e), el("div", { class: "ac-actions" }, el("button", { type: "button", class: "btn btn-secondary", onclick: () => start() }, "Try again")));
      return;
    }
    if (!me || !Array.isArray(me.roles) || !me.roles.length) { renderPlain(rootEl, "Not available", "This page is not available."); return; }
    renderShell(rootEl);
    window.addEventListener("hashchange", show);
    if (!window.location.hash) history.replaceState(null, "", window.location.pathname + "#dashboard");
    await show();
  }

  window.PPStaff = { mask, shown, maskDeep, csvCell, parseHash, hashFor, ROUTES, start, _setMe: (m) => { me = m; }, canSee: (id) => canSee(ROUTES.find((r) => r.id === id)) };
  if (!scfg.manualStart) {
    if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start); else start();
  }
})();
