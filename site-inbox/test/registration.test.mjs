// Verified publisher registration on the account page (site/marketplace-registration.js) against
// fake postbox answers: the steps and what each saves, masked bank details, document uploads,
// the KYC sandbox, agreements, submission with step-up, the needs_info reply and Turkish strings.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { JSDOM, VirtualConsole } from "jsdom";

const read = (p) => readFileSync(new URL(p, import.meta.url), "utf8");
const core = read("../../site/marketplace-core.js");
const registrationJs = read("../../site/marketplace-registration.js");
const tick = async (n = 25) => { for (let i = 0; i < n; i++) await new Promise((r) => setImmediate(r)); };
const fail = (status, code, body = {}) => Object.assign(new Error(code), { status, code, body: { error: code, ...body } });

const REQS = {
  account_types: [
    { id: "individual_tr", label: "Individual (resident in Turkey)", label_tr: "Bireysel (Türkiye'de yerleşik)", country: "TR", company: false, org_allowed: false, kyc_subject: "applicant", tax_id: "tckn", bank_methods: ["iban"], documents: [{ type: "bank_letter", label: "Bank letter", label_tr: "Banka yazısı", need: "optional" }] },
    { id: "company_tr", label: "Company in Turkey", label_tr: "Türkiye'de şirket", country: "TR", company: true, org_allowed: true, kyc_subject: "signatory", tax_id: "vkn", bank_methods: ["iban"],
      documents: [{ type: "faaliyet_belgesi", label: "Activity certificate", label_tr: "Faaliyet belgesi", need: "required", max_age_days: 183 }, { type: "vergi_levhasi", label: "Tax plate", label_tr: "Vergi levhası", need: "required" }] },
    { id: "individual_foreign", label: "Individual (outside Turkey)", label_tr: "Bireysel (yurt dışı)", country: null, company: false, org_allowed: false, kyc_subject: "applicant", tax_id: "foreign_optional", bank_methods: ["iban", "swift", "ach"], documents: [] },
  ],
  document_max_bytes: 10485760, agreement_version: "dda-1", content_policy_version: "dcp-1", kyc_provider: "mock",
};

function app(over = {}) {
  return {
    id: "pap_1", account_type: "individual_tr", account_type_label: "Individual (resident in Turkey)", state: "draft", editable: true, revision: 3,
    public_name: "Ada Tools", profile: { legal_name: "Ada Yılmaz", country: "TR" }, bank: null, tax: null, documents: [], kyc: [], kyc_provider: "mock",
    agreement: { agreement_version: null, content_policy_version: null, current_agreement_version: "dda-1", current_content_policy_version: "dcp-1" },
    checklist: [{ id: "profile_complete", ok: false, blocking: true, detail: "Missing: address" }, { id: "kyc", ok: true, blocking: true, detail: "Identity verified" }],
    events: [], staff_message: null, ...over,
  };
}

function page(t, { current = app(), lang = "en-US", extra } = {}) {
  const dom = new JSDOM('<div id="root"></div>', { url: "https://pigeonpost.dev/account", runScripts: "outside-only", virtualConsole: new VirtualConsole() });
  t.after(() => dom.window.close());
  const { window } = dom;
  Object.defineProperty(window.navigator, "language", { value: lang, configurable: true });
  for (const s of [core, registrationJs]) window.eval(s);
  const calls = [], raw = [], stepUps = [];
  const state = { app: current };
  const pb = async (path, init = {}) => {
    const method = init.method || "GET";
    const body = init.body ? JSON.parse(init.body) : undefined;
    calls.push({ path, method, body });
    if (extra) { const r = await extra(method, path, body, state); if (r !== undefined) return r; }
    if (path === "/v1/publisher-registration/requirements") return { data: REQS };
    if (method === "GET" && path === "/v1/publisher-registrations") return { data: state.app ? [state.app] : [] };
    if (method === "GET" && path === "/v1/publisher-registrations/pap_1") return { data: state.app };
    if (method === "PATCH" && path === "/v1/publisher-registrations/pap_1") { state.app = { ...state.app, revision: state.app.revision + 1 }; return { data: state.app }; }
    throw fail(404, "not_found");
  };
  const pbRaw = async (path, init = {}) => { raw.push({ path, method: init.method || "GET", body: init.body }); return { data: { id: "pad_1" } }; };
  const deps = { pb, pbRaw, toast: () => {}, stepUp: (challenge, section) => stepUps.push({ challenge, section }) };
  const root = window.document.querySelector("#root");
  window.PPMarketplace.registration(root, deps, { orgs: [] });
  const $ = (s) => window.document.querySelector(s);
  const click = (s) => $(s).dispatchEvent(new window.Event("click", { bubbles: true }));
  const set = (s, v) => { $(s).value = v; };
  return { window, $, click, set, calls, raw, stepUps, state, root };
}

test("a draft walks through details and payout, saving what each step holds", async (t) => {
  const p = page(t);
  await tick();
  assert.ok(p.$("#mp-reg-step-0"), "starts on the details step");
  assert.equal(p.$("#mp-reg-country").value, "TR");
  assert.ok(p.$("#mp-reg-country").hasAttribute("readonly"), "the country follows the account type");
  assert.equal(p.$("#mp-reg-signatory"), null, "individuals have no signatory");
  p.set("#mp-reg-line1", "Bağdat Cd. 1");
  p.set("#mp-reg-city", "İstanbul");
  p.set("#mp-reg-postal", "34000");
  p.set("#mp-reg-support", "support@example.com");
  p.set("#mp-reg-contact", "dev@example.com");
  p.click("#mp-reg-next");
  await tick();
  const save = p.calls.find((c) => c.method === "PATCH");
  assert.equal(save.body.revision, 3);
  assert.equal(save.body.public_name, "Ada Tools");
  assert.deepEqual(save.body.profile.address, { line1: "Bağdat Cd. 1", city: "İstanbul", postal_code: "34000", country: "TR" });
  assert.equal(save.body.profile.contact_email, "dev@example.com");
  assert.ok(p.$("#mp-reg-step-1"), "moves on to payout and tax");
  assert.equal(p.$("#mp-reg-method"), null, "Turkish payees have one method: IBAN");
  p.set("#mp-reg-holder", "Ada Yilmaz");
  p.set("#mp-reg-iban", "TR33 0006 1005 1978 6457 8413 26");
  p.set("#mp-reg-taxid", "10000000146");
  p.click("#mp-reg-next");
  await tick();
  const pay = p.calls.filter((c) => c.method === "PATCH")[1];
  assert.deepEqual(pay.body.bank, { holder_name: "Ada Yilmaz", method: "iban", iban: "TR33 0006 1005 1978 6457 8413 26" });
  assert.deepEqual(pay.body.tax, { tax_id: "10000000146", tax_country: "TR" });
  assert.ok(p.$("#mp-reg-step-2"));
});

test("saved bank and tax details show masked and are replaced, never edited in place", async (t) => {
  const p = page(t, { current: app({ bank: { holder_name: "Ada Yilmaz", iban: "TR33 •••• •••• •••• •••• ••13 26", currency: "TRY" }, tax: { tax_id: "•••••••••46", tax_country: "TR" } }) });
  await tick();
  p.click('[data-step="1"]');
  await tick();
  assert.match(p.$("#mp-reg-bank-saved").textContent, /TR33 •••• .* 26 · TRY/);
  assert.match(p.$("#mp-reg-tax-saved").textContent, /•••••••••46/);
  assert.equal(p.$("#mp-reg-iban"), null, "no field holds the saved IBAN");
  p.click("#mp-reg-bank-replace");
  await tick();
  assert.equal(p.$("#mp-reg-iban").value, "", "replacing starts empty");
});

test("a foreign payee chooses IBAN, SWIFT or ACH", async (t) => {
  const p = page(t, { current: app({ account_type: "individual_foreign", profile: { country: "US" } }) });
  await tick();
  p.click('[data-step="1"]');
  await tick();
  const method = p.$("#mp-reg-method");
  assert.deepEqual([...method.options].map((o) => o.value), ["iban", "swift", "ach"]);
  method.value = "ach";
  method.dispatchEvent(new p.window.Event("change"));
  assert.equal(p.$('[data-method="ach"]').hidden, false);
  assert.equal(p.$('[data-method="iban"]').hidden, true);
  p.set("#mp-reg-holder", "Ada L");
  p.set("#mp-reg-routing", "021000021");
  p.set("#mp-reg-acct2", "000123456789");
  p.set("#mp-reg-taxcountry", "us");
  p.set("#mp-reg-treaty", "us");
  p.set("#mp-reg-resexp", "2030-01-01");
  p.click("#mp-reg-save");
  await tick();
  const body = p.calls.find((c) => c.method === "PATCH").body;
  assert.deepEqual(body.bank, { holder_name: "Ada L", method: "ach", routing_number: "021000021", account_number: "000123456789", ach_account_type: "checking" });
  assert.deepEqual(body.tax, { tax_country: "US", treaty_country: "US", residence_certificate_expires_on: "2030-01-01" });
});

test("documents: the checklist per type, uploads with dates, and removal", async (t) => {
  const p = page(t, { current: app({ account_type: "company_tr", profile: { country: "TR" },
    documents: [{ id: "pad_9", type: "vergi_levhasi", filename: "levha.pdf", scan: "pending" }] }) });
  await tick();
  p.click('[data-step="2"]');
  await tick();
  const items = [...p.root.querySelectorAll("[data-doc]")].map((li) => li.getAttribute("data-doc"));
  assert.deepEqual(items, ["faaliyet_belgesi", "vergi_levhasi"]);
  assert.match(p.$('[data-doc="vergi_levhasi"]').textContent, /levha\.pdf.*waiting for the malware scan/);
  assert.ok(p.$("#mp-doc-faaliyet_belgesi-issued"), "an activity certificate needs its issue date");
  const file = p.$("#mp-doc-faaliyet_belgesi");
  const blob = new p.window.File(["%PDF-1.4"], "faaliyet.pdf", { type: "application/pdf" });
  Object.defineProperty(file, "files", { value: [blob] });
  p.set("#mp-doc-faaliyet_belgesi-issued", "2026-10-01");
  p.click('[data-upload="faaliyet_belgesi"]');
  await tick();
  assert.equal(p.raw.length, 1);
  assert.equal(p.raw[0].method, "POST");
  assert.equal(p.raw[0].path, "/v1/publisher-registrations/pap_1/documents?type=faaliyet_belgesi&filename=faaliyet.pdf&issued_on=2026-10-01");
  assert.equal(p.raw[0].body, blob, "the file goes as is");
  p.click('[data-remove="pad_9"]');
  await tick();
  assert.ok(p.calls.some((c) => c.method === "DELETE" && c.path === "/v1/publisher-registrations/pap_1/documents/pad_9"));
});

test("identity: the sandbox says so, and its simulation goes through the postbox", async (t) => {
  const pending = { id: "kyc_1", result: "pending", sandbox: true, verification_url: null, event: "request.pending" };
  const p = page(t, { current: app({ kyc: [pending] }), extra: (m, path, body, s) => {
    if (m === "POST" && path === "/v1/publisher-registrations/pap_1/kyc/kyc_1/simulate") {
      s.app = { ...s.app, kyc: [{ ...pending, result: "accepted" }] };
      return { data: s.app };
    }
  } });
  await tick();
  p.click('[data-step="3"]');
  await tick();
  assert.match(p.$("#mp-reg-sandbox").textContent, /Sandbox/);
  assert.match(p.$("#mp-reg-kyc-state").textContent, /In progress/);
  p.click("#mp-reg-sim-ok");
  await tick();
  assert.deepEqual(p.calls.find((c) => c.path.endsWith("/simulate")).body, { outcome: "accepted" });
  assert.match(p.$("#mp-reg-kyc-state").textContent, /Verified/);
  assert.equal(p.$("#mp-reg-kyc-start"), null, "nothing left to start");
});

test("a live check shows Shufti's hosted flow in the page, camera allowed", async (t) => {
  const p = page(t, { current: app({ kyc_provider: "shufti", kyc: [{ id: "kyc_2", result: "pending", sandbox: false, verification_url: "https://app.shuftipro.com/verification/abc" }] }) });
  await tick();
  p.click('[data-step="3"]');
  await tick();
  const frame = p.$("#mp-reg-kyc-frame");
  assert.equal(frame.getAttribute("src"), "https://app.shuftipro.com/verification/abc");
  assert.match(frame.getAttribute("allow"), /camera/);
  assert.equal(p.$("#mp-reg-sandbox"), null);
});

test("review: the checklist, the agreements, and submitting with step-up", async (t) => {
  let stepped = false;
  const p = page(t, { extra: (m, path) => {
    if (m === "POST" && path === "/v1/publisher-registrations/pap_1/submit") {
      if (!stepped) { stepped = true; throw fail(401, "step_up_required", { acr_values: "mfa", max_age: 600 }); }
      return { data: app({ state: "submitted", editable: false }) };
    }
  } });
  await tick();
  p.click('[data-step="4"]');
  await tick();
  assert.equal(p.$('[data-check="profile_complete"]').className.includes("is-missing"), true);
  assert.match(p.$('[data-check="profile_complete"]').textContent, /Missing: address/);
  const agree = p.$("#mp-reg-agree");
  agree.checked = true;
  agree.dispatchEvent(new p.window.Event("change"));
  await tick();
  assert.deepEqual(p.calls.find((c) => c.method === "PATCH").body.accept_agreements, { agreement_version: "dda-1", content_policy_version: "dcp-1" });
  p.click("#mp-reg-submit");
  await tick();
  p.$('#mp-reg-status [data-stepup="publisher"]').click();
  assert.deepEqual(JSON.parse(JSON.stringify(p.stepUps)), [{ challenge: { acr_values: "mfa", max_age: 600 }, section: "publisher" }]);
  p.click("#mp-reg-submit");
  await tick();
  assert.match(p.$("#mp-reg-badge").textContent, /Submitted/);
  assert.match(p.$("#mp-reg-status").textContent, /Submitted\. Pigeonpost reviews it/);
  assert.equal(p.$("#mp-reg-step-0"), null, "a submitted application is not editable");
});

test("needs_info shows Pigeonpost's message and resubmits with a reply", async (t) => {
  const p = page(t, { current: app({ state: "needs_info", staff_message: { message: "Upload a newer faaliyet belgesi" },
    events: [{ by: "staff", to: "needs_info", message: "Upload a newer faaliyet belgesi", at: "2026-11-11T10:00:00Z" }] }),
  extra: (m, path) => { if (m === "POST" && path.endsWith("/submit")) return { data: app({ state: "submitted", editable: false }) }; } });
  await tick();
  assert.match(p.$("#mp-reg-staff-message").textContent, /newer faaliyet belgesi/);
  assert.match(p.$("#mp-reg-badge").textContent, /More information needed/);
  p.click('[data-step="4"]');
  await tick();
  p.set("#mp-reg-reply", "Uploaded the October one");
  p.click("#mp-reg-submit");
  await tick();
  assert.deepEqual(p.calls.find((c) => c.path.endsWith("/submit")).body, { revision: 3, message: "Uploaded the October one" });
});

test("with no application, a type is chosen and the draft started", async (t) => {
  const p = page(t, { current: null, extra: (m, path, body, s) => {
    if (m === "POST" && path === "/v1/publisher-registrations") { s.app = app({ account_type: body.account_type }); return { data: s.app }; }
  } });
  await tick();
  p.set("#mp-reg-type", "company_tr");
  p.$("#mp-reg-type").dispatchEvent(new p.window.Event("change"));
  assert.match(p.$("#mp-reg-type-docs").textContent, /Activity certificate — Required/);
  p.set("#mp-reg-name", "Acme");
  p.$("#mp-reg-start").dispatchEvent(new p.window.Event("submit", { cancelable: true }));
  await tick();
  assert.deepEqual(p.calls.find((c) => c.method === "POST").body, { account_type: "company_tr", public_name: "Acme" });
  assert.ok(p.$("#mp-reg-signatory"), "a company names its authorised signatory");
});

test("Turkish browsers get Turkish labels", async (t) => {
  const p = page(t, { lang: "tr-TR" });
  await tick();
  assert.equal(p.$("#mp-reg-title").textContent, "Doğrulanmış yayıncı olun");
  assert.match(p.$('[data-step="1"]').textContent, /Ödeme ve vergi/);
});

test("when identity verification is unavailable the page says so and offers nothing to start", async (t) => {
  const p = page(t, { current: app({ kyc_provider: "unavailable", kyc: [] }) });
  await tick();
  p.click('[data-step="3"]');
  await tick();
  assert.match(p.$("#mp-reg-kyc-unavailable").textContent, /temporarily unavailable/);
  assert.equal(p.$("#mp-reg-kyc-start"), null);
  assert.equal(p.$("#mp-reg-sandbox"), null);
});
