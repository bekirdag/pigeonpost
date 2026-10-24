// Pigeonpost for teams — create an organization (SDS §5.3 "Start wizard"): the organization and
// its address, the billing details, then the server-priced summary and the hosted payment page.
(function () {
  "use strict";
  const PPE = window.PPE;
  const P = PPE.pricing;
  const el = PPE.el;
  const NS = /^[a-z0-9][a-z0-9._-]{0,31}$/;
  const draftKey = "ppe_start";

  const q = new URLSearchParams(window.location.search);
  const saved = PPE.store.json(draftKey) || {};
  const wiz = {
    step: saved.step || 1,
    seats: P.stops().includes(Number(q.get("seats"))) ? Number(q.get("seats")) : saved.seats || 30,
    interval: ["monthly", "yearly"].includes(q.get("interval")) ? q.get("interval") : saved.interval || "yearly",
    currency: ["USD", "TRY"].includes(q.get("currency")) ? q.get("currency") : saved.currency || "USD",
    name: saved.name || "",
    namespace: saved.namespace || "",
    intent: /^[a-f0-9]{32}$/.test(saved.intent || "") ? saved.intent : null,
    profileId: saved.profileId || null,
  };
  function persist() { PPE.store.set(draftKey, JSON.stringify(wiz)); }
  function newIntent() {
    const b = new Uint8Array(16);
    window.crypto.getRandomValues(b);
    return Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
  }
  if (!wiz.intent) wiz.intent = newIntent();

  const root = () => document.getElementById("wizard");
  function progress() {
    PPE.$$(".steps li").forEach((li, i) => {
      li.classList.toggle("done", i + 1 < wiz.step);
      if (i + 1 === wiz.step) li.setAttribute("aria-current", "step"); else li.removeAttribute("aria-current");
      li.setAttribute("aria-label", `Step ${i + 1}${i + 1 < wiz.step ? ", done" : ""}`);
    });
  }
  function show(...nodes) { root().replaceChildren(...nodes); progress(); const h = root().querySelector("h1"); if (h) { h.tabIndex = -1; h.focus(); } }

  /// "Acme Inc." -> "acme"
  function suggest(name) {
    return String(name).toLowerCase().normalize("NFKD").replace(/[̀-ͯ]/g, "")
      .replace(/ı/g, "i").replace(/\b(inc|ltd|llc|gmbh|a\.?s|sti|co)\b\.?/g, "")
      .replace(/[^a-z0-9]+/g, "").slice(0, 32);
  }

  // ---- step 1: organization -------------------------------------------------------------------
  function step1() {
    const name = el("input", { type: "text", required: true, maxlength: 120, autocomplete: "organization", value: wiz.name });
    const ns = el("input", { type: "text", required: true, maxlength: 32, spellcheck: "false", autocapitalize: "none", value: wiz.namespace, "aria-describedby": "ns-status" });
    const status = el("p", { class: "status-line", id: "ns-status", role: "status" });
    const next = el("button", { class: "btn btn-primary", type: "submit" }, "Continue");
    let edited = Boolean(wiz.namespace);
    let checked = { value: null, ok: false };
    let timer = null;
    async function check() {
      const value = ns.value.trim().toLowerCase();
      checked = { value, ok: false };
      if (!value) { status.textContent = ""; return; }
      if (!NS.test(value)) { status.className = "status-line status-bad"; status.textContent = "Use 1–32 lowercase letters, numbers, dots, underscores or hyphens, starting with a letter or number."; return; }
      status.className = "status-line"; status.textContent = "Checking…";
      try {
        const r = await PPE.bridge(`/v1/handles/${encodeURIComponent(value)}/availability`, { auth: false });
        if (ns.value.trim().toLowerCase() !== value) return;
        if (!r.known) { status.className = "status-line status-bad"; status.textContent = "We couldn't check that address right now. Try again."; return; }
        checked = { value, ok: Boolean(r.available) };
        status.className = `status-line ${r.available ? "status-ok" : "status-bad"}`;
        status.textContent = r.available ? `/${value} is available.` : `/${value} is taken. Try another.`;
      } catch { status.className = "status-line status-bad"; status.textContent = "We couldn't check that address right now."; }
    }
    const later = () => { window.clearTimeout(timer); timer = window.setTimeout(check, 350); };
    name.addEventListener("input", () => { if (!edited) { ns.value = suggest(name.value); later(); } });
    ns.addEventListener("input", () => { edited = true; later(); });
    const form = el("form", { novalidate: true },
      el("h1", {}, "Your organization"),
      el("p", { class: "muted" }, `${wiz.seats} seats · ${wiz.interval === "yearly" ? "billed yearly" : "billed monthly"} · `, el("a", { href: `/?seats=${wiz.seats}&interval=${wiz.interval}&currency=${wiz.currency}` }, "change")),
      PPE.field("Organization name", name, "As people should see it, e.g. Acme Inc."),
      PPE.field("Company address", el("div", { class: "prefixed" }, el("span", { "aria-hidden": "true" }, "/"), ns),
        "Everyone's address starts with this: /acme/jane. It can't be changed later."),
      status, el("div", { class: "actions" }, next));
    form.addEventListener("submit", async (e) => {
      e.preventDefault();
      if (!name.value.trim()) { status.className = "status-line status-bad"; status.textContent = "Give the organization a name."; name.focus(); return; }
      const value = ns.value.trim().toLowerCase();
      if (checked.value !== value) await check();
      if (!checked.ok) { ns.focus(); return; }
      wiz.name = name.value.trim(); wiz.namespace = value; wiz.step = 2; persist(); render();
    });
    show(form);
    if (ns.value) check();
  }

  // ---- step 2: billing ------------------------------------------------------------------------
  function step2() {
    const f = {};
    const input = (key, attrs = {}) => (f[key] = el("input", { type: "text", maxlength: 512, ...attrs }));
    f.account_type = el("select", {}, el("option", { value: "entity" }, "Company"), el("option", { value: "individual" }, "Individual"));
    const entityOnly = [], individualOnly = [];
    const group = (list, node) => { list.push(node); return node; };
    const status = el("p", { class: "status-line", role: "status" });
    const next = el("button", { class: "btn btn-primary", type: "submit" }, "Continue");
    const back = el("button", { class: "btn", type: "button", onclick: () => { wiz.step = 1; persist(); render(); } }, "Back");
    const form = el("form", { novalidate: true },
      el("h1", {}, "Billing details"),
      el("p", { class: "muted" }, "These go on your invoices. Invoices are issued for every payment."),
      PPE.field("Invoice type", f.account_type),
      PPE.field("Legal name", input("legal_name", { required: true, autocomplete: "organization" })),
      group(entityOnly, PPE.field("Tax office", input("tax_office"))),
      group(entityOnly, PPE.field("Tax number (VKN)", input("tax_id", { inputmode: "numeric" }))),
      group(individualOnly, PPE.field("Turkish identity number (TCKN)", input("identity_number", { inputmode: "numeric", maxlength: 11 }), "Required for billing addresses in Türkiye.")),
      PPE.field("Invoice contact address", input("billing_contact", { required: true, spellcheck: "false" }), "Invoices and receipts are sent here."),
      PPE.field("Phone (optional)", input("phone", { autocomplete: "tel" })),
      PPE.field("Street address", input("line1", { required: true, autocomplete: "address-line1" })),
      el("div", { class: "row" },
        PPE.field("District (İlçe)", input("state")),
        PPE.field("City", input("city", { required: true, autocomplete: "address-level2" }))),
      el("div", { class: "row" },
        PPE.field("Postal code", input("postal_code", { autocomplete: "postal-code" })),
        PPE.field("Country", input("country", { required: true, value: "Türkiye", autocomplete: "country-name" }))),
      status, el("div", { class: "actions" }, back, next));
    const sync = () => {
      const entity = f.account_type.value === "entity";
      entityOnly.forEach((n) => { n.hidden = !entity; });
      individualOnly.forEach((n) => { n.hidden = entity; });
    };
    f.account_type.addEventListener("change", sync);
    sync();
    form.addEventListener("submit", async (e) => {
      e.preventDefault();
      for (const k of Object.keys(f)) if (f[k].value !== undefined && f[k].tagName === "INPUT") f[k].value = f[k].value.trim();
      const entity = f.account_type.value === "entity";
      const missing = ["legal_name", "billing_contact", "line1", "city", "country"].find((k) => !f[k].value)
        || (entity ? ["tax_office", "tax_id"].find((k) => !f[k].value) : null);
      const bad = (msg, k) => { status.className = "status-line status-bad"; status.textContent = msg; if (k) f[k].focus(); };
      if (missing) return bad("Fill in the highlighted details.", missing);
      if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(f.billing_contact.value)) return bad("Enter a valid invoice contact address.", "billing_contact");
      if (entity && !/^\d{10,11}$/.test(f.tax_id.value)) return bad("Enter a 10 digit VKN (or 11 digit TCKN).", "tax_id");
      if (!entity && f.identity_number.value && !/^\d{11}$/.test(f.identity_number.value)) return bad("Enter your 11 digit TCKN.", "identity_number");
      const body = {
        intent_id: wiz.intent, account_type: entity ? "entity" : "individual", legal_name: f.legal_name.value,
        billing_email: f.billing_contact.value, phone: f.phone.value || null,
        address: { line1: f.line1.value, line2: null, city: f.city.value, state: f.state.value || null, postal_code: f.postal_code.value || null, country: f.country.value },
        ...(entity ? { entity_name: f.legal_name.value, tax_id: f.tax_id.value, tax_office: f.tax_office.value }
          : { identity_number: f.identity_number.value || null }),
      };
      next.disabled = true; status.className = "status-line"; status.textContent = "Saving…";
      try {
        const r = await PPE.bridge("/ent/billing-profile", { method: "POST", body });
        const id = r.id || (r.data && r.data.id);
        if (!id) throw new Error("Billing details were not saved.");
        wiz.profileId = id; wiz.step = 3; persist(); render();
      } catch (err) { next.disabled = false; bad(PPE.explain(err) || "Could not save."); }
    });
    show(form);
  }

  // ---- step 3: summary and payment ------------------------------------------------------------
  function step3() {
    const amount = P.amount(wiz.seats, wiz.interval, wiz.currency);
    const plan = P.planCode(wiz.seats, wiz.interval, wiz.currency);
    const rows = [
      ["Organization", `${wiz.name} (/${wiz.namespace})`],
      ["Seats", `${wiz.seats} people, you included`],
      ["Agent addresses", `up to ${wiz.seats * 10}`],
      ["Storage", `${wiz.seats * 5} GB, shared`],
      ["Billing", wiz.interval === "yearly" ? "Every year" : "Every month"],
      ["Price", amount ? `${PPE.money(amount, wiz.currency)} ${wiz.interval === "yearly" ? "per year" : "per month"}` : "Shown on the payment page"],
    ];
    const pay = el("button", { class: "btn btn-primary btn-large", type: "button" }, "Continue to secure payment");
    const back = el("button", { class: "btn", type: "button", onclick: () => { wiz.step = 2; persist(); render(); } }, "Back");
    const status = el("p", { class: "status-line", role: "status" });
    const closed = !P.isOpen(wiz.currency);
    pay.disabled = closed;
    pay.addEventListener("click", () => {
      PPE.store.set("ppe_ent_checkout", JSON.stringify({ intent_id: wiz.intent, plan_code: plan, billing_profile_id: wiz.profileId,
        org: { name: wiz.name, namespace: wiz.namespace }, locale: (navigator.language || "en").toLowerCase().startsWith("tr") ? "tr" : "en" }));
      window.location.assign("/checkout");
    });
    show(el("div", {},
      el("h1", {}, "Review and pay"),
      el("div", { class: "card" }, el("table", { class: "summary-table" }, el("tbody", {}, rows.map(([k, v]) => el("tr", {}, el("td", {}, k), el("td", {}, v)))))),
      el("p", { class: "note" }, "The plan renews automatically until you cancel. The amount shown is what you are charged; your invoice states any VAT. Card details go directly to iyzico and never reach Pigeonpost."),
      closed ? el("p", { class: "banner" }, `Checkout in ${wiz.currency} opens soon. `, el("a", { href: `/?seats=${wiz.seats}&interval=${wiz.interval}` }, "Choose another currency")) : null,
      status, el("div", { class: "actions" }, back, pay)));
  }

  function needSignIn() {
    show(el("div", { class: "card" },
      el("h1", {}, "Create your organization"),
      el("p", { class: "muted" }, "Sign in with your Pigeonpost account, or create one. You become the organization's owner."),
      el("div", { class: "actions" }, el("button", { class: "btn btn-primary", type: "button", onclick: () => PPE.signIn({ returnTo: window.location.pathname + window.location.search }) }, "Sign in to continue"))));
  }

  function notYet() {
    show(el("div", { class: "card" }, el("h1", {}, "Coming soon"),
      el("p", { class: "muted" }, "Organizations are opening gradually. We'll let you know when yours can start."),
      el("a", { class: "btn", href: "/" }, "Back to packages")));
  }

  async function render() {
    PPE.renderAccount();
    if (!PPE.signedIn()) return needSignIn();
    if (wiz.step === 1) return step1();
    if (wiz.step === 2) return step2();
    return step3();
  }

  async function init() {
    persist();
    if (PPE.signedIn()) {
      try {
        const caps = await PPE.postbox("/v1/capabilities");
        const features = Array.isArray(caps.features) ? caps.features : Object.keys(caps.features || {});
        if (!features.includes("enterprise_portal")) { PPE.renderAccount(); return notYet(); }
      } catch (e) { if (e instanceof PPE.Redirecting) return; }
    }
    P.loadCatalog().finally(render);
  }
  PPE.start = { wiz, suggest, render };
  init();
})();
