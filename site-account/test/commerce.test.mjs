// Buying: the billing gate (P8), hosted checkout hand-off, the returns from iyzico on the new
// origin, and the actions every old account card had.
import test from "node:test";
import assert from "node:assert/strict";
import { boot, tick, fixtures, PROFILE, reply, read } from "./helpers.mjs";
import { JSDOM, VirtualConsole } from "jsdom";

const NO_BILLING = (over = {}) => fixtures({
  "GET /api/v1/me/overview": { handles: [], billingProfiles: [], invoices: [], cloud: { plan: { web_plan: { capacity: 0, used: 0 } }, subscriptions: [] }, openCurrencies: ["USD"] },
  "GET /v1/nests": { nests: [], allowance: { slots: 0, used: 0, duplicates: [] }, setups: [] },
  ...over,
});
const INTENT = "0123456789abcdef0123456789abcdef";

test("billing gate: without billing details every buy area is passive under 'Add billing details first'", async (t) => {
  const a = boot(t, { routes: NO_BILLING() });
  await a.started; await tick();
  const gated = (sel) => {
    const n = a.$(sel);
    assert.ok(n, `${sel} is drawn`);
    const area = n.closest(".pp-gated-area");
    assert.ok(area, `${sel} sits in the gated area`);
    assert.ok(area.hasAttribute("inert"));
    assert.equal(area.getAttribute("aria-disabled"), "true");
    const gate = area.parentElement.querySelector(".pp-gate a");
    assert.equal(gate.textContent, "Add billing details first");
    assert.equal(gate.getAttribute("href"), "#/billing");
  };
  await a.go("#/handles/get"); gated("#acc-buy");
  await a.go("#/plan"); gated("#acc-plan-go");
  await a.go("#/nests"); gated("[data-buy]");
  await a.go("#/store/nests"); gated("[data-buy]");
  await a.go("#/store/services/lst_tr"); gated("#mp-buy-go");
  await a.go("#/wallet"); gated("#cr-buy-go");
  assert.ok(!a.calls.some((c) => c.key === "POST /api/v1/credits/quotes"), "no quote is asked for while the gate is up");
  // Spending credit already bought needs no billing details.
  await a.go("#/store/services/lst_hr");
  assert.equal(a.$("#acc-run-0-go").closest(".pp-gated-area"), null);
});

test("billing gate: incomplete details say what is missing; registering into a plan with room is not gated", async (t) => {
  const a = boot(t, { routes: fixtures({ "GET /api/v1/me/overview": { handles: [], billingProfiles: [{ id: "bp_2", legal_name: "Ada" }], invoices: [], cloud: { plan: { web_plan: { capacity: 2, used: 0 } }, subscriptions: [] } } }) });
  await a.started; await tick();
  await a.go("#/handles/get");
  assert.equal(a.$("#acc-buy").closest(".pp-gated-area"), null);
  await a.go("#/wallet");
  assert.match(a.$(".pp-gate").textContent, /missing a name, address, city or country/);
  assert.equal(a.$(".pp-gate a").textContent, "Complete billing details first");
});

test("saving billing details lifts the gate", async (t) => {
  let saved = null;
  const routes = NO_BILLING({ "POST /api/v1/billing/profiles": (c) => { saved = c.body; return [201, { id: "bp_9" }]; } });
  const a = boot(t, { routes, url: "https://account.pigeonpost.dev/#/billing" });
  await a.started; await tick();
  const f = a.$("#acc-billing-form").inputs;
  const set = (k, v) => { f[k].value = v; f[k].dispatchEvent(new a.w.Event("input")); };
  set("legal_name", "Ada Lovelace"); set("billing_email", "ada@example.com"); set("line1", "Bağdat Cd. 1"); set("city", "İstanbul"); set("postal_code", "34000"); set("country", "Türkiye"); set("state", "Kadıköy");
  a.$("#acc-billing-form").dispatchEvent(new a.w.Event("submit", { cancelable: true }));
  await tick();
  assert.equal(saved, null, "a Turkish individual needs a TCKN");
  assert.match(a.text(), /TCKN \(Turkish identity number\) is required/);
  set("identity_number", "12345678901");
  routes["GET /api/v1/me/overview"] = { ...routes["GET /api/v1/me/overview"], billingProfiles: [{ ...PROFILE, id: "bp_9" }] };
  a.$("#acc-billing-form").dispatchEvent(new a.w.Event("submit", { cancelable: true }));
  await tick();
  assert.deepEqual(JSON.parse(JSON.stringify(saved)), { account_type: "individual", legal_name: "Ada Lovelace", billing_email: "ada@example.com", phone: "",
    address: { line1: "Bağdat Cd. 1", line2: null, city: "İstanbul", state: "Kadıköy", postal_code: "34000", country: "Türkiye" }, identity_number: "12345678901", entity_name: null, tax_id: null, tax_office: null });
  await a.go("#/wallet");
  assert.equal(a.$(".pp-gated-area"), null);
});

test("buying hands a pending checkout to /billing/checkout: plan, Nest package, marketplace service", async (t) => {
  const a = boot(t, { routes: fixtures({ "GET /api/v1/me/overview": { handles: [], billingProfiles: [PROFILE], invoices: [], cloud: { plan: { web_plan: { capacity: 0, used: 0 } }, subscriptions: [] }, openCurrencies: ["USD"] } }) });
  await a.started; await tick();
  const pending = () => JSON.parse(a.w.sessionStorage.getItem("pp_cloud_checkout"));
  await a.go("#/plan");
  a.$("#acc-plan-go").click(); await tick();
  assert.equal(a.nav.at(-1), "/billing/checkout");
  assert.equal(pending().kind, "cloud");
  assert.equal(pending().plan_code, "cloud-1names-yearly-usd");
  assert.equal(pending().billing_profile_id, "bp_1");
  assert.match(pending().intent_id, /^[a-f0-9]{32}$/);
  await a.go("#/nests");
  a.$("[data-buy]").click(); await tick();
  assert.deepEqual([pending().kind, pending().plan_code], ["nest", "nest-pp_ai-monthly-usd"]);
  await a.go("#/store/services/lst_tr");
  a.$("#mp-buy-go").click(); await tick();
  assert.equal(pending().kind, "marketplace");
  assert.deepEqual(JSON.parse(JSON.stringify(pending().owner)), { kind: "account" });
  assert.equal(pending().listing_id, "lst_tr");
});

test("credits: a quote, then iyzico's own page; the return ?credits= is confirmed with the bridge", async (t) => {
  const a = boot(t, { routes: fixtures({ "POST /api/v1/credits/checkout": { payment_page_url: "https://sandbox-cpp.iyzipay.com/pay/x" } }) });
  await a.started; await tick();
  await a.go("#/wallet");
  assert.match(a.$("#cr-quote").textContent, /You pay \$25\.00 for \$25\.00 of credit/);
  a.$("#cr-buy").dispatchEvent(new a.w.Event("submit", { cancelable: true }));
  await tick();
  assert.equal(a.nav.at(-1), "https://sandbox-cpp.iyzipay.com/pay/x");
  const co = a.calls.find((c) => c.key === "POST /api/v1/credits/checkout");
  assert.equal(co.body.quote_id, "q1");
  assert.equal(co.body.billing_profile_id, "bp_1");

  let asks = 0;
  const r = boot(t, { url: `https://account.pigeonpost.dev/?credits=${INTENT}#credits`, routes: fixtures({ [`GET /api/v1/credits/checkout/${INTENT}`]: () => (++asks < 2 ? { status: "pending" } : { status: "paid", credit_cents: 2500 }) }) });
  await r.started; await tick(120);
  assert.equal(r.w.location.hash, "#/wallet");
  assert.equal(asks, 2);
  assert.match(r.text(), /Payment received\. \$25\.00 of credit was added/);
  assert.equal(r.w.location.search, "", "the return parameter is dropped");
});

test("plan return: ?checkout= polls the bridge; ?card= says the card update was submitted", async (t) => {
  const a = boot(t, { url: `https://account.pigeonpost.dev/?checkout=${INTENT}`, routes: fixtures({ [`GET /api/v1/cloud/checkout/${INTENT}`]: { status: "active" } }), storage: { pp_cloud_checkout: { kind: "cloud" } } });
  await a.started; await tick(120);
  assert.equal(a.w.location.hash, "#/plan");
  assert.ok(a.calls.some((c) => c.key === `GET /api/v1/cloud/checkout/${INTENT}`));
  assert.equal(a.w.sessionStorage.getItem("pp_cloud_checkout"), null);
  const b = boot(t, { url: `https://account.pigeonpost.dev/?card=${INTENT}` });
  await b.started; await tick();
  assert.match(b.text(), /Card update submitted/);
  assert.equal(b.w.location.search, "");
});

test("Nest return: ?nest_checkout= confirms; a company package (nest_org) points to Enterprise", async (t) => {
  const a = boot(t, { url: `https://account.pigeonpost.dev/?nest_checkout=${INTENT}&nest_org=org_abcdef`, routes: fixtures({ [`GET /api/v1/nests/checkout/${INTENT}`]: { status: "active" } }) });
  await a.started; await tick(120);
  assert.equal(a.w.location.hash, "#/nests");
  assert.match(a.text(), /company's Nest package is active/);
  assert.ok(a.$('a[href="https://enterprise.pigeonpost.dev/app"]'));
});

test("marketplace return: ?mp_purchase= opens the listing to finish installing", async (t) => {
  const a = boot(t, { url: `https://account.pigeonpost.dev/?mp_purchase=${INTENT}`, routes: fixtures({
    [`GET /api/v1/marketplace/checkout/${INTENT}`]: { status: "paid", listing_id: "lst_tr", installation_id: "ins_1" },
    "POST /v1/nest-installations": [201, { data: { id: "ins_1" } }],
  }) });
  await a.started; await tick(120);
  assert.equal(a.w.location.hash, "#/store/services/lst_tr");
  assert.ok(a.$("#acc-mp-finish"), "the finish-installing form");
  a.$("#acc-mp-finish").dispatchEvent(new a.w.Event("submit", { cancelable: true }));
  await tick();
  const call = a.calls.find((c) => c.key === "POST /v1/nest-installations");
  assert.deepEqual(JSON.parse(JSON.stringify(call.body)), { listing_id: "lst_tr", purchase_id: "ins_1" });
});

test("a checkout page's notice is shown once back on the account", async (t) => {
  const a = boot(t, { storage: { pp_checkout_notice: "Your session expired. Sign in and try again." } });
  await a.started; await tick();
  assert.match(a.$("#pp-toasts").textContent, /Your session expired/);
  assert.equal(a.w.sessionStorage.getItem("pp_checkout_notice"), null);
});

test("handles: availability is asked for, then the name is registered into the plan", async (t) => {
  const a = boot(t, { routes: fixtures({ "GET /api/v1/handles/grace/availability": { name: "grace", available: true, known: true }, "POST /api/v1/names": { ok: true } }) });
  await a.started; await tick();
  await a.go("#/handles/get");
  const input = a.$("#acc-handle");
  input.value = "ad"; input.dispatchEvent(new a.w.Event("input")); await tick();
  assert.match(a.$("#acc-handle-status").textContent, /At least 3/);
  input.value = "grace"; input.dispatchEvent(new a.w.Event("input")); await tick(80);
  assert.match(a.$("#acc-handle-status").textContent, /\/grace is available/);
  assert.equal(a.$("#acc-buy").disabled, false);
  a.$("#acc-buy").click(); await tick();
  const call = a.calls.find((c) => c.key === "POST /api/v1/names");
  assert.deepEqual(JSON.parse(JSON.stringify(call.body)), { namespace: "grace" });
});

test("inboxes: create, name and delete; connector keys are shown once and revoked", async (t) => {
  const a = boot(t, { routes: fixtures({
    "POST /v1/identities": [201, { address: "k_new@pigeonpost.dev" }],
    "POST /v1/identities/handle": { ok: true },
    "DELETE /v1/identities": { ok: true },
    "POST /v1/api-keys": [201, { api_key: "ppk_secret_value" }],
    "DELETE /v1/api-keys/key_1": { ok: true },
  }) });
  await a.started; await tick();
  await a.go("#/inboxes");
  const click = (label, root = a.document) => [...root.querySelectorAll("button")].find((b) => b.textContent === label).click();
  click("Create an inbox"); await tick();
  const form = a.$(".sheet form");
  form.inputs.label.value = "my agent";
  form.dispatchEvent(new a.w.Event("submit", { cancelable: true })); await tick();
  assert.equal(a.calls.find((c) => c.key === "POST /v1/identities").body.label, "my agent");
  click("Name it"); await tick();
  assert.equal(a.$("#acc-name-handle").value, "/ada/agent1", "an agent under the handle already held");
  a.$("#acc-name-handle").value = "/ada/helper"; a.$("#acc-name-handle").dispatchEvent(new a.w.Event("input"));
  a.$(".sheet form").dispatchEvent(new a.w.Event("submit", { cancelable: true })); await tick();
  assert.deepEqual(JSON.parse(JSON.stringify(a.calls.find((c) => c.key === "POST /v1/identities/handle").body)), { address: "k_def456@pigeonpost.dev", handle: "/ada/helper" });
  await tick();
  click("Delete"); await tick();
  click("Delete inbox", a.$(".sheet")); await tick();
  assert.match(a.calls.find((c) => c.key === "DELETE /v1/identities").url, /identity=k_abc123%40pigeonpost\.dev/);
  await a.go("#/inboxes/keys");
  click("Create connector key"); await tick();
  assert.match(a.$(".sheet pre").textContent, /Bearer ppk_secret_value/);
  click("Done", a.$(".sheet")); await tick();
  assert.equal(a.$(".sheet"), null);
  assert.ok(!a.document.body.textContent.includes("ppk_secret_value"), "the key is gone once the panel closes");
  click("Revoke"); await tick();
  click("Revoke key", a.$(".sheet")); await tick();
  assert.ok(a.calls.some((c) => c.key === "DELETE /v1/api-keys/key_1"));
});

test("plan: change, cancel and card update go to the bridge routes", async (t) => {
  const a = boot(t, { routes: fixtures({ "POST /api/v1/cloud/subscriptions/sub_1/upgrade": { ok: true }, "POST /api/v1/cloud/subscriptions/sub_1/cancel": { paid_through: 4102444800 } }) });
  await a.started; await tick();
  await a.go("#/plan");
  a.$("#acc-change-plan").click(); await tick();
  a.$("#acc-change-names").value = "3";
  a.$("#acc-change-go").click(); await tick();
  const up = a.calls.find((c) => c.key === "POST /api/v1/cloud/subscriptions/sub_1/upgrade").body;
  assert.equal(up.plan_code, "cloud-3names-yearly-usd");
  assert.equal(up.upgrade_period, "NOW");
  [...a.$$("button")].find((b) => b.textContent === "Update card").click(); await tick();
  assert.equal(JSON.parse(a.w.sessionStorage.getItem("pp_cloud_checkout")).kind, "card");
  [...a.$$("button")].find((b) => b.textContent === "Cancel plan").click(); await tick();
  [...a.$(".sheet").querySelectorAll("button")].find((b) => b.textContent === "Cancel plan").click(); await tick();
  assert.ok(a.calls.some((c) => c.key === "POST /api/v1/cloud/subscriptions/sub_1/cancel"));
});

test("hosted run: a priced operation reserves credit; a result file downloads with the bearer", async (t) => {
  const a = boot(t, { routes: fixtures({ "POST /v1/marketplace/hosted-jobs": [201, { data: { job_id: "job_2" } }], "GET /v1/marketplace/hosted-jobs/job_1/files/0": { pdf: true } }) });
  await a.started; await tick();
  a.w.URL.createObjectURL = () => "blob:x"; a.w.URL.revokeObjectURL = () => {};
  await a.go("#/store/services/lst_hr");
  a.$("#acc-run-0-name").value = "Grace Hopper";
  a.$("#acc-run-0").dispatchEvent(new a.w.Event("submit", { cancelable: true })); await tick();
  const run = a.calls.find((c) => c.key === "POST /v1/marketplace/hosted-jobs").body;
  assert.deepEqual(JSON.parse(JSON.stringify(run)), { listing_id: "lst_hr", operation: "research", operation_version: "1", input: { name: "Grace Hopper" } });
  assert.match(a.text(), /\$25\.00 is reserved from your credit/);
  await a.go("#/purchases");
  a.$('[data-file="/v1/marketplace/hosted-jobs/job_1/files/0"]').click(); await tick();
  const dl = a.calls.find((c) => c.key === "GET /v1/marketplace/hosted-jobs/job_1/files/0");
  assert.match(dl.headers.authorization, /^Bearer /);
});

test("a run that costs more than the balance is blocked with Buy credits", async (t) => {
  const a = boot(t, { routes: fixtures({ "GET /api/v1/credits/balance": { available_cents: 100, held_cents: 0 } }), url: "https://account.pigeonpost.dev/#/store/services/lst_hr" });
  await a.started; await tick();
  assert.equal(a.$("#acc-run-0-go").disabled, true);
  assert.match(a.text(), /You need \$24\.00 more credit/);
  assert.equal([...a.$$(".pp-gate a")].find((x) => x.textContent === "Buy credits").getAttribute("href"), "#/wallet");
});

test("account deletion: type DELETE, then the request and its reference", async (t) => {
  const a = boot(t, { url: "https://account.pigeonpost.dev/#delete-account", routes: fixtures({ "POST /v1/account/deletion-request": { request: { request_id: "del_123", complete_by: 4102444800 } } }) });
  await a.started; await tick();
  const go = a.$("#acc-deletion-submit");
  assert.equal(go.disabled, true);
  a.$("#acc-deletion-confirm").value = "DELETE"; a.$("#acc-deletion-confirm").dispatchEvent(new a.w.Event("input"));
  assert.equal(go.disabled, false);
  go.closest("form").dispatchEvent(new a.w.Event("submit", { cancelable: true })); await tick();
  assert.deepEqual(JSON.parse(JSON.stringify(a.calls.find((c) => c.key === "POST /v1/account/deletion-request").body)), { confirm: true });
  assert.match(a.text(), /del_123/);
});

test("invoices open their PDF through the bridge's link", async (t) => {
  const a = boot(t, { url: "https://account.pigeonpost.dev/#/invoices", routes: fixtures({ "GET /api/v1/billing/invoices/11111111-1111-4111-8111-111111111111/download": { url: "https://files.example/inv.pdf" } }) });
  await a.started; await tick();
  a.$("[data-invoice]").click(); await tick();
  assert.equal(a.nav.at(-1), "open:https://files.example/inv.pdf");
});

// ---- /billing/checkout --------------------------------------------------------------------------

function checkoutPage(t, { pending, session = "tok", fetch } = {}) {
  const html = read("billing/checkout.html");
  const dom = new JSDOM(html.replace(/<script src="[^"]+"><\/script>/g, ""), { url: "https://account.pigeonpost.dev/billing/checkout", runScripts: "outside-only", virtualConsole: new VirtualConsole() });
  t.after(() => dom.window.close());
  const w = dom.window;
  const calls = [];
  const replaced = [];
  if (session) w.sessionStorage.setItem("pp_session", session);
  if (pending) w.sessionStorage.setItem("pp_cloud_checkout", JSON.stringify(pending));
  w.fetch = async (u, init) => { calls.push({ url: String(u), body: JSON.parse(init.body), auth: init.headers.authorization }); return fetch(String(u)); };
  w.eval(read("config.js"));
  // location.replace is not configurable in jsdom; the page calls it only to go back.
  w.__back = (u) => replaced.push(u);
  w.eval(read("billing/checkout.js").replace("window.location.replace(home())", "window.__back(home())"));
  return { w, calls, replaced, $: (s) => w.document.querySelector(s) };
}

test("checkout page: the pending plan starts the iyzico form through /api with the session", async (t) => {
  const p = checkoutPage(t, { pending: { kind: "cloud", intent_id: INTENT, plan_code: "cloud-1names-yearly-usd", billing_profile_id: "bp_1", locale: "en" },
    fetch: () => reply(200, { plan_code: "cloud-1names-yearly-usd", amount: "8.00", currency: "USD", checkout_form_content: '<div id="iyzipay-checkout-form"></div><script src="https://static.iyzipay.com/checkoutform/v2/bundle.js"></script>' }) });
  await tick();
  assert.equal(p.calls[0].url, "/api/v1/cloud/checkout");
  assert.equal(p.calls[0].auth, "Bearer tok");
  assert.match(p.$("#pp-pay-summary").textContent, /Pigeonpost Cloud, 1 name/);
  const s = p.$("#pp-pay-form script");
  assert.equal(s.src, "https://static.iyzipay.com/checkoutform/v2/bundle.js");
  assert.equal(s.nonce, "__CSP_NONCE__");
  assert.equal(p.$("#pp-pay-back").getAttribute("href"), "/#/plan");
});

test("checkout page: nothing pending goes back to the account; a 401 goes back with a notice", async (t) => {
  const a = checkoutPage(t, { fetch: () => reply(500, {}) });
  await tick();
  assert.deepEqual(a.replaced, ["/"]);
  const b = checkoutPage(t, { pending: { kind: "nest", intent_id: INTENT, plan_code: "x" }, fetch: () => reply(401, { error: "unauthorized" }) });
  await tick();
  assert.deepEqual(b.replaced, ["/#/nests"]);
  assert.match(b.w.sessionStorage.getItem("pp_checkout_notice"), /session expired/);
});
