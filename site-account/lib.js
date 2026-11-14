// account.pigeonpost.dev — what every page shares: API helpers, the member's overview and billing
// state, the billing gate, hosted checkout, return-parameter handling and small formatters.
//
// Pages register themselves with ACC.page({...}); boot.js starts the shell with them in nav order.
// Nothing here writes server text as HTML (PP.el only).
(function () {
  "use strict";
  const ACC = (window.ACC = window.ACC || {});
  const el = (...a) => PP.el(...a);
  ACC.registry = ACC.registry || {};
  ACC.page = (spec) => { ACC.registry[spec.key] = spec; };

  // ---- API ---------------------------------------------------------------------------------

  /// The postbox as the signed-in member. A step-up comes back to the page that asked.
  ACC.pb = (path, opts = {}) => PP.postbox(path, opts);
  /// A background read through the bridge.
  ACC.bridgeGet = (path, opts = {}) => PP.bridge(path, opts);
  /// Something the member just clicked (checkout, save billing, cancel). A 401 here can be the
  /// payment backend refusing this one operation, so it is shown, never turned into a sign-out.
  ACC.bridgeAction = (path, body, method = "POST") => PP.bridge(path, { method, body: body || {}, keepSession: true });
  /// The machine-readable reason: the bridge says it in `code`, the postbox in `error`.
  ACC.code = (e) => (e && e.body && typeof e.body.code === "string" && e.body.code) || (e && typeof e.code === "string" ? e.code : null);

  ACC.hex32 = function () {
    const b = new Uint8Array(16);
    window.crypto.getRandomValues(b);
    return Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
  };
  ACC.locale = () => (/^tr\b/i.test(window.navigator.language || "") ? "tr" : "en");

  const EURO_REGIONS = /^(AT|BE|CY|DE|EE|ES|FI|FR|GR|HR|IE|IT|LT|LU|LV|MT|NL|PT|SI|SK)$/;
  const EURO_LANGUAGES = /^(de|fr|it|nl|fi|el|et|lv|lt|mt|sk|sl|hr|ga|lb)$/;
  /// The currency to offer first: the last one chosen, else one from the browser's language.
  ACC.currency = function () {
    const saved = PP.durable.get("pp_currency");
    if (saved === "USD" || saved === "EUR" || saved === "TRY") return saved;
    const [language, region] = String(window.navigator.language || "").split("-");
    if (/^tr$/i.test(language || "")) return "TRY";
    if (region ? EURO_REGIONS.test(region.toUpperCase()) : EURO_LANGUAGES.test((language || "").toLowerCase())) return "EUR";
    return "USD";
  };
  ACC.setCurrency = (c) => PP.durable.set("pp_currency", c);
  ACC.CURRENCY_NAMES = { USD: "US dollars (USD)", EUR: "Euros (EUR)", TRY: "Turkish lira (TRY)" };

  /// An amount in major units ("8.00", 12.5) as money.
  ACC.money = (amount, currency) => {
    try { return new Intl.NumberFormat(PP.locale(), { style: "currency", currency }).format(Number(amount)); } catch { return `${amount} ${currency}`; }
  };
  /// US cents as money.
  ACC.usd = (cents) => ACC.money(Number(cents || 0) / 100, "USD");
  /// Unix seconds, ISO text or milliseconds as a short date.
  ACC.date = function (v) {
    if (v === undefined || v === null || v === "") return "—";
    let ms = typeof v === "number" ? (v > 1e12 ? v : v * 1000) : Date.parse(v);
    if (!Number.isFinite(ms)) return String(v);
    try { return new Date(ms).toLocaleDateString(PP.locale(), { year: "numeric", month: "short", day: "numeric" }); } catch { return "—"; }
  };

  // ---- caches --------------------------------------------------------------------------------

  const cache = new Map();
  /// One request shared by every caller until `force` or ACC.forget(key).
  ACC.cached = function (key, load, force) {
    if (force || !cache.has(key)) {
      const p = Promise.resolve().then(load);
      cache.set(key, p);
      p.catch(() => { if (cache.get(key) === p) cache.delete(key); });
    }
    return cache.get(key);
  };
  ACC.forget = (...keys) => { for (const k of keys) cache.delete(k); };

  /// What an invoice needs; the same fields `billing_profile_incomplete` names.
  ACC.profileComplete = function (p) {
    if (!p) return false;
    const a = p.address && typeof p.address === "object" ? p.address : p;
    return Boolean(String(p.legal_name || p.legalName || "").trim() && String(a.line1 || "").trim()
      && String(a.city || "").trim() && String(a.country || "").trim());
  };

  /// The bridge's member overview (handles from every store, billing profiles, invoices, the Cloud
  /// plan), with the billing state every buy area depends on.
  ACC.overview = function (force) {
    return ACC.cached("overview", async () => {
      const data = await ACC.bridgeGet("/v1/me/overview");
      const profiles = Array.isArray(data.billingProfiles) ? data.billingProfiles : [];
      const complete = profiles.find(ACC.profileComplete) || null;
      const cloud = data.cloud || null;
      const plan = (cloud && cloud.plan && cloud.plan.web_plan) || { capacity: 0, used: 0, names: [] };
      const handles = (Array.isArray(data.handles) ? data.handles : []).map((h) => {
        const name = String(h.namespace || "").replace(/^\/+/, "");
        const active = h.active ?? (!h.expires_at || h.expires_at * 1000 > Date.now());
        return { ...h, name, active };
      }).filter((h) => h.name);
      return {
        raw: data,
        billing: { has: Boolean(complete), incomplete: !complete && profiles.length > 0, profileId: complete ? complete.id : null, profiles },
        cloud,
        cloudError: !cloud,
        plan,
        subscription: (cloud && Array.isArray(cloud.subscriptions) && cloud.subscriptions[0]) || null,
        handles,
        invoices: Array.isArray(data.invoices) ? data.invoices : [],
        openCurrencies: Array.isArray(data.openCurrencies) ? data.openCurrencies : null,
      };
    }, force);
  };
  ACC.hasRoom = (ov) => Boolean(ov && ov.plan && ov.plan.active !== false && ov.plan.capacity > ov.plan.used);
  ACC.identities = (force) => ACC.cached("identities", async () => (await ACC.pb("/v1/identities")).identities || [], force);
  ACC.balance = (force) => ACC.cached("balance", () => ACC.bridgeGet("/v1/credits/balance"), force);
  ACC.feature = (ctx, name) => Boolean(ctx && ctx.features && ctx.features.has(name));

  // ---- the billing gate (P8) ------------------------------------------------------------------

  /// Purchases are invoiced to the saved billing details, and checkout refuses without them. With
  /// none saved (or incomplete ones) the buy area is shown passive under one "do this first" action.
  ACC.gateText = (ov) => (ov && ov.billing.incomplete
    ? "Your billing details are missing a name, address, city or country. Complete them before you buy."
    : "Purchases are invoiced to your billing details. Add them before you buy plans, handles, Nests, services or credits.");
  ACC.billingGate = function (ov, ...area) {
    if (ov && ov.billing.has) return el("div", { class: "acc-buy-area" }, area);
    return PP.ui.gate({
      text: ACC.gateText(ov),
      action: { label: ov && ov.billing.incomplete ? "Complete billing details first" : "Add billing details first", href: PP.href("billing"), icon: "billing" },
    }, ...area);
  };

  // ---- hosted checkout -------------------------------------------------------------------------

  /// The iyzico form runs on its own page (/billing/checkout) whose policy admits iyzico and
  /// nothing else. The intent id is the idempotency key: a reload replays rather than charges twice.
  ACC.startCheckout = function (pending) {
    PP.store.set("pp_cloud_checkout", JSON.stringify(pending));
    PP.navigate((window.PP_CLOUD && window.PP_CLOUD.checkoutPath) || "/billing/checkout");
  };

  // ---- return parameters ------------------------------------------------------------------------

  ACC.param = (key) => new URLSearchParams(window.location.search).get(key);
  /// Drop a return parameter so a reload does not confirm the same payment again.
  ACC.clearParam = function (...keys) {
    const params = new URLSearchParams(window.location.search);
    let changed = false;
    for (const k of keys) if (params.has(k)) { params.delete(k); changed = true; }
    if (!changed) return;
    const q = params.toString();
    window.history.replaceState(window.history.state, "", window.location.pathname + (q ? "?" + q : "") + window.location.hash);
  };
  ACC.INTENT = /^[a-f0-9]{32}$/;
  ACC.wait = (ms) => new Promise((r) => window.setTimeout(r, ms));
  /// How long a confirmation poll waits between asks (tests make it 0).
  ACC.pollMs = 3000;
  ACC.pollAttempts = 20;

  /// Ask the bridge until a payment settles. `done(result)` returns a message when it has.
  ACC.poll = async function (path, done) {
    for (let i = 0; i < ACC.pollAttempts; i++) {
      try {
        const r = await ACC.bridgeGet(path, { keepSession: true });
        const msg = done(r);
        if (msg) return msg;
      } catch (e) {
        if (e && e.status === 404) return { text: "We could not find that purchase.", tone: "error" };
        if (e && e.status === 401) return null;
      }
      await ACC.wait(ACC.pollMs);
    }
    return { text: "Your payment is still being confirmed. This page shows it as soon as it is.", tone: "info" };
  };

  // ---- small UI pieces ------------------------------------------------------------------------

  /// A status line that says what an action did (role=status), for inside cards.
  ACC.statusLine = function () {
    const p = el("p", { class: "acc-status", role: "status", "aria-live": "polite" });
    p.say = (text, tone, ...extra) => {
      p.className = `acc-status${tone ? ` is-${tone}` : ""}`;
      p.replaceChildren(text || "", ...extra.filter(Boolean).map((x) => [" ", x]).flat());
    };
    return p;
  };
  /// Set attributes on a node built elsewhere (PP.ui.button takes only its own options).
  ACC.attr = (node, attrs) => { for (const [k, v] of Object.entries(attrs)) if (v !== null && v !== undefined) node.setAttribute(k, String(v)); return node; };
  /// PP.ui.button plus an `id` and extra attributes.
  ACC.btn = (label, opts = {}) => {
    const { id, attrs, ...rest } = opts;
    const b = PP.ui.button(label, rest);
    if (id) b.id = id;
    if (attrs) ACC.attr(b, attrs);
    return b;
  };
  /// A labelled control in the shared field style.
  ACC.field = function (label, input, { hint, optional, wide, id } = {}) {
    const fid = input.id || id || `acc-f-${PP.random(5)}`;
    input.id = fid;
    if (!input.classList.contains("pp-input") && input.type !== "checkbox" && input.type !== "radio") input.classList.add("pp-input");
    return el("div", { class: `pp-field${wide ? " pp-field-wide" : ""}` },
      el("label", { for: fid }, label, optional ? el("span", { class: "pp-optional" }, " (optional)") : null),
      input, hint ? el("p", { class: "pp-hint" }, hint) : null);
  };
  /// A select from [[value, text]] pairs.
  ACC.select = (options, value, attrs = {}) => el("select", { class: "pp-input", ...attrs },
    options.map(([v, t, extra]) => el("option", { value: v, selected: String(v) === String(value) ? true : null, ...(extra || {}) }, t)));
  /// A link that opens outside the account.
  ACC.ext = (href, text) => el("a", { href, target: "_blank", rel: "noopener noreferrer" }, text);
  /// A feature this account does not have yet.
  ACC.notYet = (title, text, icon = "info") => PP.ui.empty({ icon, title, text });
  /// Download a file the postbox serves only with the bearer (hosted job results).
  ACC.download = async function (path, name) {
    const res = await PP.postbox(path, { raw: true });
    const blob = await res.blob();
    const url = window.URL.createObjectURL(blob);
    const a = el("a", { href: url, download: name || "result.pdf", class: "pp-sr" });
    document.body.append(a);
    a.click();
    a.remove();
    window.setTimeout(() => window.URL.revokeObjectURL(url), 30000);
  };
})();
