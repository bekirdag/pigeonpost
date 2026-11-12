// Exercise the actual account page with fake OIDC responses and fictional billing details.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { JSDOM, VirtualConsole } from "jsdom";

const source = readFileSync(new URL("../../site/account.js", import.meta.url), "utf8");
const config = readFileSync(new URL("../../site/account-config.js", import.meta.url), "utf8");
const tick = () => new Promise((resolve) => setImmediate(resolve));

test("all providers appear with canonical status and correct management actions", async t => {
  const dom = new JSDOM('<main id="account-root"></main>', { url: "https://pigeonpost.dev/account", runScripts: "outside-only", virtualConsole: new VirtualConsole() });
  t.after(() => dom.window.close());
  const { window } = dom;
  window.eval(config); window.sessionStorage.setItem("pp_session", "member-fixture");
  let fail = false, reads = 0;
  const handles = [
    { namespace: "apple-name", source: "apple", active: true, expires_at: 1999999999 },
    { namespace: "google-name", source: "google", active: false, expires_at: 100 },
    { namespace: "web-name", source: "entitlement", active: true, expires_at: null },
  ];
  window.fetch = async url => {
    const overview = String(url).endsWith("/me/overview");
    if (overview) reads++;
    return { ok: !overview || !fail, status: overview && fail ? 502 : 200, json: async () => overview
      ? { handles, subscriptions: [{ handle: "apple-name", id: "obsolete-card", status: "active" }], billingProfiles: [], paymentMethods: [], invoices: [] }
      : { identities: [], keys: [] } };
  };
  window.eval(source);
  for (let n = 0; n < 20; n++) await tick();
  const list = () => window.document.querySelector("#ac-subs");
  assert.match(list().textContent, /apple-name.*Active · App Store/s);
  assert.match(list().textContent, /google-name.*Expired · Google Play/s);
  assert.match(list().textContent, /web-name.*Active · Pigeonpost/s);
  assert.equal(list().querySelectorAll(".ac-row").length, 3, "old web subscription cannot duplicate an Apple handle");
  assert.equal(list().querySelector("[data-cancel]"), null, "old billing row cannot manage another provider");
  assert.ok(list().querySelector('a[href^="https://play.google.com/"]'));
  fail = true; window.document.querySelector("#ac-handles-refresh").click();
  for (let n = 0; n < 20; n++) await tick();
  assert.match(list().textContent, /Could not refresh/);
  assert.doesNotMatch(list().textContent, /No handles yet/);
  fail = false; handles.push({ namespace: "new-mobile", source: "google", active: true });
  window.document.querySelector("#ac-handles-refresh").click();
  for (let n = 0; n < 20; n++) await tick();
  assert.match(list().textContent, /new-mobile/);
  assert.equal(reads, 3);
});

async function deletionPage(t, { signedIn = true, existing = null, status = 200, deferPost = false, pendingCard = false } = {}) {
  const dom = new JSDOM('<main id="account-root"></main>', { url: "https://pigeonpost.dev/account#delete-account", runScripts: "outside-only", virtualConsole: new VirtualConsole() });
  t.after(() => dom.window.close());
  const { window } = dom;
  window.eval(config);
  if (signedIn) window.sessionStorage.setItem("pp_session", "member-token");
  if (pendingCard) window.localStorage.setItem("pp_card_session", "unfinished-card-setup");
  const calls = [];
  let finishPost;
  const receipt = {request_id: "del_example", requested_at: 10, complete_by: 2592010, completed_at: null};
  window.fetch = async (url, init = {}) => {
    const path = new URL(url).pathname;
    calls.push({path, method: init.method || "GET", body: init.body, authorization: init.headers?.authorization});
    assert.equal(path, "/v1/account/deletion-request", "deletion does not initiate billing or unrelated member calls");
    assert.equal(init.headers.authorization, "Bearer member-token");
    if (init.method === "POST" && deferPost) await new Promise(resolve => { finishPost = resolve; });
    return {ok: status < 400, status, json: async () => status === 401 ? {error: "unauthorized"} : {request: init.method === "POST" ? receipt : existing, account: {label: "review@example.test"}}};
  };
  window.eval(source);
  for (let i = 0; i < 20; i++) await tick();
  return {window, calls, receipt, finish: () => finishPost(), async flush() {for(let i = 0; i < 10; i++) await tick();}};
}

test("account deletion requires sign-in before any request is sent", async t => {
  const p = await deletionPage(t, {signedIn: false});
  assert.ok(p.window.document.querySelector("#ac-deletion-signin"));
  assert.equal(p.calls.length, 0);
});

test("deletion never resumes an unrelated unfinished payment-method setup", async t => {
  const p = await deletionPage(t, {pendingCard: true});
  assert.equal(p.window.location.hash, "#delete-account");
  assert.ok(p.window.document.querySelector("#ac-deletion-confirm"));
  assert.equal(p.calls.length, 1);
  assert.equal(p.window.localStorage.getItem("pp_card_session"), "unfinished-card-setup");
});

test("deletion identifies the account, requires exact confirmation, and sends only one request", async t => {
  const p = await deletionPage(t, {deferPost: true});
  const $ = s => p.window.document.querySelector(s);
  assert.match($("#ac-deletion-account").textContent, /review@example.test/);
  const input = $("#ac-deletion-confirm"), button = $("#ac-deletion-submit");
  for (const value of ["", "delete", "DELETE "]) {
    input.value = value; input.dispatchEvent(new p.window.Event("input"));
    assert.equal(button.disabled, true);
  }
  input.value = "DELETE"; input.dispatchEvent(new p.window.Event("input"));
  button.click(); button.click();
  assert.equal(button.disabled, true);
  assert.equal(input.disabled, true);
  const posts = p.calls.filter(c => c.method === "POST");
  assert.equal(posts.length, 1);
  assert.deepEqual(JSON.parse(posts[0].body), {confirm: true});
  p.finish(); await p.flush();
  assert.match($("#ac-deletion-receipt").textContent, /del_example/);
  assert.equal($("#ac-deletion-submit"), null);
});

test("a prior deletion receipt does not create a second request", async t => {
  const p = await deletionPage(t, {existing: {request_id: "del_prior", complete_by: 2592010}});
  assert.match(p.window.document.querySelector("#ac-deletion-receipt").textContent, /del_prior/);
  assert.equal(p.window.document.querySelector("#ac-deletion-submit"), null);
  assert.equal(p.calls.filter(c => c.method === "POST").length, 0);
});

test("expired sessions ask for a new sign-in without submitting deletion", async t => {
  const p = await deletionPage(t, {status: 401});
  assert.match(p.window.document.querySelector("#ac-deletion-signin").textContent, /Sign in again/);
  assert.equal(p.calls.length, 1);
});

test("a deletion response cannot overwrite a subsequent sign-out", async t => {
  const p = await deletionPage(t, {deferPost: true});
  const $ = s => p.window.document.querySelector(s);
  $("#ac-deletion-confirm").value = "DELETE";
  $("#ac-deletion-confirm").dispatchEvent(new p.window.Event("input"));
  $("#ac-deletion-submit").click();
  $("#ac-deletion-logout").click();
  p.finish(); await p.flush();
  assert.ok($("#ac-deletion-signin"));
  assert.equal($("#ac-deletion-receipt"), null);
});

// What an invoice needs (account.js profileComplete): a name, an address, a city and a country.
const COMPLETE_PROFILE = { id: "11111111-1111-4111-8111-111111111111", legal_name: "Example Person",
  address: { line1: "1 Example Street", city: "Istanbul", postal_code: "34000", country: "TR" } };

async function checkoutPage(t, { url = "https://pigeonpost.dev/account", plan = { capacity: 0, used: 0, names: [] }, profiles = [COMPLETE_PROFILE],
  subscriptions = [], recovery = false, statuses = ["pending", "active"], openCurrencies } = {}) {
  const dom = new JSDOM('<main id="account-root"></main>', { url, runScripts: "outside-only", virtualConsole: new VirtualConsole() });
  t.after(() => dom.window.close());
  const { window } = dom;
  window.eval(config);
  window.sessionStorage.setItem("pp_session", "member-token");
  const originalTimeout = window.setTimeout.bind(window);
  window.setTimeout = (fn, delay, ...args) => originalTimeout(fn, delay >= 350 ? 0 : delay, ...args);
  const calls = [];
  window.fetch = async (url, init = {}) => {
    const path = new URL(url, window.location.origin).pathname;
    calls.push({ path, method: init.method || "GET", body: init.body && JSON.parse(init.body) });
    const response = (body, status = 200) => ({ ok: status < 400, status, json: async () => body });
    if (path === "/api/v1/me/overview") return response({ handles: [], invoices: [], billingProfiles: profiles,
      cloud: { subscriptions, plan: { web_plan: plan } }, openCurrencies });
    if (path === "/api/v1/billing/profiles" && init.method === "POST") { profiles = [{ ...COMPLETE_PROFILE, ...JSON.parse(init.body) }]; return response(profiles[0], 201); }
    if (path.endsWith("/availability")) return response({ available: !recovery || init.headers?.authorization === "Bearer member-token", known: true });
    if (path === "/api/v1/names") return response({ namespace: "/example", capacity: plan.capacity, used: plan.used + 1 });
    if (path.startsWith("/api/v1/cloud/checkout/")) return response({ status: statuses.length > 1 ? statuses.shift() : statuses[0] });
    return response({ identities: [], keys: [] });
  };
  window.eval(source);
  window.document.dispatchEvent(new window.Event("DOMContentLoaded"));
  for (let i = 0; i < 30 && !window.document.querySelector("#ac-buy"); i++) await tick();
  for (let i = 0; i < 10; i++) await tick();
  return {
    window, calls,
    async buy() {
      const input = window.document.querySelector("#ac-handle");
      input.value = "example"; input.dispatchEvent(new window.Event("input"));
      const button = window.document.querySelector("#ac-buy");
      for (let i = 0; i < 30 && button.disabled; i++) await new Promise((resolve) => setTimeout(resolve, 1));
      assert.equal(button.disabled, false);
      button.click();
      for (let i = 0; i < 20; i++) await tick();
    },
  };
}

test("a plan with room registers the name directly, with no payment step", async (t) => {
  const page = await checkoutPage(t, { plan: { capacity: 3, used: 1, names: [] } });
  assert.equal(page.window.document.querySelector("#ac-buy").textContent, "Register it");
  await page.buy();
  const register = page.calls.filter((c) => c.path === "/api/v1/names");
  assert.equal(register.length, 1);
  assert.deepEqual(register[0].body, { namespace: "example" });
  assert.equal(page.calls.some((c) => c.path.startsWith("/api/v1/cloud/checkout")), false);
  assert.equal(page.window.document.querySelector("#ac-toast").textContent, "/example is yours.");
});

test("the signed-in owner can select an expired handle during recovery", async (t) => {
  const page = await checkoutPage(t, { recovery: true, plan: { capacity: 1, used: 0, names: [] } });
  await page.buy();
  assert.equal(page.calls.filter((c) => c.path === "/api/v1/names").length, 1);
});

test("without a plan the member chooses one; the hosted form gets a stable intent and no price from the page", async (t) => {
  const page = await checkoutPage(t);
  await page.buy();
  const doc = page.window.document;
  const names = doc.querySelector("#ac-plan-names");
  assert.ok(names, "plan picker shown");
  assert.equal(names.options[0].value, "1");
  assert.equal(names.options.length, 10);
  const currency = doc.querySelector("#ac-plan-currency");
  currency.value = "TRY"; currency.dispatchEvent(new page.window.Event("change"));
  assert.match(doc.querySelector("#ac-plan-names").options[2].textContent, /3 names/);
  assert.match(doc.querySelector("#ac-plan-names").options[2].textContent, /1[.,]?226/);
  doc.querySelector("#ac-plan-names").value = "3";
  try { doc.querySelector("#ac-plan-go").click(); } catch { /* jsdom does not navigate */ }
  const pending = JSON.parse(page.window.sessionStorage.getItem("pp_cloud_checkout"));
  assert.match(pending.intent_id, /^[a-f0-9]{32}$/);
  assert.equal(pending.plan_code, "cloud-3names-yearly-try");
  assert.equal(pending.billing_profile_id, "11111111-1111-4111-8111-111111111111");
  assert.equal(Object.keys(pending).some((k) => /price|amount/.test(k)), false);
  assert.equal(page.calls.some((c) => c.method === "POST"), false, "nothing is charged from the account page");
});

test("the picker offers only currencies the merchant can take", async (t) => {
  const page = await checkoutPage(t, { openCurrencies: ["TRY"] });
  await page.buy();
  const options = [...page.window.document.querySelector("#ac-plan-currency").options].map((o) => o.value);
  assert.deepEqual(options, ["TRY"]);
  assert.match(page.window.document.querySelector("#ac-plan-names").options[0].textContent, /409/);
});

test("EUR is offered once the bridge opens it, priced from the catalog copy", async (t) => {
  const page = await checkoutPage(t, { openCurrencies: ["USD", "EUR", "TRY"] });
  await page.buy();
  const doc = page.window.document;
  const currency = doc.querySelector("#ac-plan-currency");
  assert.deepEqual([...currency.options].map((o) => o.value), ["USD", "EUR", "TRY"]);
  assert.match([...currency.options][1].textContent, /Euros \(EUR\)/);
  currency.value = "EUR"; currency.dispatchEvent(new page.window.Event("change"));
  assert.match(doc.querySelector("#ac-plan-names").options[2].textContent, /21[.,]60/);
  doc.querySelector("#ac-plan-names").value = "3";
  try { doc.querySelector("#ac-plan-go").click(); } catch { /* jsdom does not navigate */ }
  assert.equal(JSON.parse(page.window.sessionStorage.getItem("pp_cloud_checkout")).plan_code, "cloud-3names-yearly-eur");
});

test("billing details come before a plan: Get it is passive and the banner leads to Billing details", async (t) => {
  const page = await checkoutPage(t, { url: "https://pigeonpost.dev/account#get-handle", profiles: [] });
  const doc = page.window.document;
  const input = doc.querySelector("#ac-handle");
  input.value = "example"; input.dispatchEvent(new page.window.Event("input"));
  for (let i = 0; i < 20; i++) await tick();
  doc.querySelector("#ac-buy").click();
  for (let i = 0; i < 5; i++) await tick();
  assert.equal(doc.querySelector("#ac-plan-names"), null, "a passive button starts nothing");
  page.window.location.hash = doc.querySelector("#ac-billing-gate-go").getAttribute("href");
  page.window.dispatchEvent(new page.window.HashChangeEvent("hashchange"));
  for (let i = 0; i < 5; i++) await tick();
  assert.equal(doc.querySelector("#ac-billing").closest(".ac-view").hidden, false);
  assert.equal(page.window.location.hash, "#billing");
});

test("saving complete billing details lifts the gate without a reload", async (t) => {
  const page = await checkoutPage(t, { url: "https://pigeonpost.dev/account#get-handle", profiles: [] });
  const doc = page.window.document;
  const input = doc.querySelector("#ac-handle");
  input.value = "example"; input.dispatchEvent(new page.window.Event("input"));
  for (let i = 0; i < 20; i++) await tick();
  assert.equal(doc.querySelector("#ac-buy").disabled, true);
  const form = doc.querySelector("#ac-billing form");
  const fields = { legal_name: "Example Person", billing_email: "billing@example.test", line1: "1 Example Street", city: "Istanbul",
    state: "Kadikoy", postal_code: "34000", country: "TR", identity_number: "10000000000" };
  for (const [k, v] of Object.entries(fields)) form.elements.namedItem(k).value = v;
  await form.onsubmit({ preventDefault() {} });
  for (let i = 0; i < 20; i++) await tick();
  assert.equal(page.calls.filter((c) => c.path === "/api/v1/billing/profiles" && c.method === "POST").length, 1);
  const buy = doc.querySelector("#ac-buy");
  assert.equal(buy.disabled, false, "the name was free, so Get it comes back");
  assert.equal(buy.hasAttribute("aria-disabled"), false);
  assert.equal(buy.hasAttribute("title"), false);
  assert.equal(doc.querySelector("#ac-billing-gate").hidden, true);
  assert.equal(doc.querySelector("#ac-app").classList.contains("ac-needs-billing"), false);
});

test("without billing details the selling sections show the gate and their buy buttons are passive", async (t) => {
  const page = await checkoutPage(t, { url: "https://pigeonpost.dev/account#get-handle", profiles: [] });
  const doc = page.window.document;
  const gate = doc.querySelector("#ac-billing-gate");
  assert.equal(gate.hidden, false, "the banner shows on Get a handle");
  assert.match(gate.textContent, /Add billing details first/);
  assert.equal(doc.querySelector("#ac-billing-gate-go").getAttribute("href"), "#billing");
  const input = doc.querySelector("#ac-handle");
  input.value = "example"; input.dispatchEvent(new page.window.Event("input"));
  for (let i = 0; i < 20; i++) await tick();
  const buy = doc.querySelector("#ac-buy");
  assert.match(doc.querySelector("#ac-handle-status").textContent, /available/, "the name check still runs");
  assert.equal(buy.disabled, true, "the buy button stays passive after the name check");
  assert.equal(buy.getAttribute("aria-disabled"), "true");
  assert.equal(buy.getAttribute("title"), "Add billing details first");
  page.window.location.hash = "#plan"; page.window.dispatchEvent(new page.window.HashChangeEvent("hashchange"));
  for (let i = 0; i < 5; i++) await tick();
  assert.equal(gate.hidden, false, "and on Cloud plan");
  assert.equal(doc.querySelector("#ac-cloud-start").disabled, true);
  page.window.location.hash = "#security"; page.window.dispatchEvent(new page.window.HashChangeEvent("hashchange"));
  for (let i = 0; i < 5; i++) await tick();
  assert.equal(gate.hidden, true, "sections that sell nothing have no banner");
});

test("an incomplete billing profile counts as missing", async (t) => {
  const page = await checkoutPage(t, { url: "https://pigeonpost.dev/account#plan", profiles: [{ id: "11111111-1111-4111-8111-111111111111", legal_name: "Example Person", address: { line1: "", city: "Istanbul", country: "TR" } }] });
  const doc = page.window.document;
  assert.equal(doc.querySelector("#ac-billing-gate").hidden, false);
  assert.match(doc.querySelector("#ac-billing-gate-why").textContent, /missing a name, address, city or country/);
  assert.equal(doc.querySelector("#ac-billing-gate-go").textContent, "Complete billing details");
  assert.equal(doc.querySelector("#ac-cloud-start").disabled, true);
});

test("a plan with room still registers names without billing details", async (t) => {
  const page = await checkoutPage(t, { profiles: [], plan: { capacity: 3, used: 1, names: [] } });
  await page.buy();
  assert.equal(page.calls.filter((c) => c.path === "/api/v1/names").length, 1);
});

test("a full plan points to a plan change instead of a second subscription", async (t) => {
  const page = await checkoutPage(t, { plan: { capacity: 1, used: 1, names: [] },
    subscriptions: [{ id: "22222222-2222-4222-8222-222222222222", names: 1, currency: "USD", provider_status: "ACTIVE" }] });
  await page.buy();
  assert.equal(page.window.document.querySelector("#ac-plan-names"), null);
  assert.match(page.window.document.querySelector("#ac-toast").textContent, /larger plan/);
  assert.ok(page.window.document.querySelector("[data-change-plan]"));
  assert.equal(page.window.location.hash, "#plan");
});

// ---- sections survive the sign-in round trip ----------------------------------------------------

test("signing in from a section comes back to it: the OIDC redirect carries no fragment", async (t) => {
  // The sign-in hashes its PKCE verifier (crypto.subtle, asynchronous) before it navigates. On a
  // loaded CI runner that can outlast a fixed number of ticks, and navigating a closed window
  // throws after the test has ended. Close only once jsdom has reported the navigation attempt.
  const virtualConsole = new VirtualConsole();
  let navigated;
  const navigation = new Promise((resolve) => { navigated = resolve; });
  virtualConsole.on("jsdomError", (e) => { if (/navigation/i.test(String(e && e.message))) navigated(); });
  const dom = new JSDOM('<main id="account-root"></main>', { url: "https://pigeonpost.dev/account#credits", runScripts: "outside-only", virtualConsole });
  t.after(async () => {
    let timer;
    await Promise.race([navigation, new Promise((r) => { timer = setTimeout(r, 10000); })]);
    clearTimeout(timer);
    dom.window.close();
  });
  const { window } = dom;
  if (!window.crypto.subtle) Object.defineProperty(window.crypto, "subtle", { value: globalThis.crypto.subtle });
  window.eval(config);
  window.fetch = async () => ({ ok: false, status: 404, json: async () => ({}) });
  window.eval(source); // jsdom fires DOMContentLoaded itself, once
  for (let i = 0; i < 10; i++) await tick();
  assert.ok(window.document.querySelector("#ac-signin"), "signed out: the plain sign-in card, no shell");
  assert.equal(window.document.querySelector("#ac-app"), null);
  window.document.querySelector("#ac-signin").click();
  for (let i = 0; i < 20; i++) await tick();
  assert.equal(window.localStorage.getItem("pp_return_view"), "credits");
});

test("the sign-in return restores the parked section and opens it in the shell", async (t) => {
  const dom = new JSDOM('<main id="account-root"></main>', { url: "https://pigeonpost.dev/account?code=fixture-code&state=fixture-state", runScripts: "outside-only", virtualConsole: new VirtualConsole() });
  t.after(() => dom.window.close());
  const { window } = dom;
  window.eval(config);
  window.localStorage.setItem("pp_state", "fixture-state");
  window.localStorage.setItem("pp_pkce", "fixture-verifier");
  window.localStorage.setItem("pp_return_view", "invoices");
  window.fetch = async (url) => {
    const path = new URL(url, window.location.origin).pathname;
    const body = path === "/api/v1/auth/exchange" ? { session: "member-token" } : { identities: [], keys: [], handles: [], invoices: [], billingProfiles: [] };
    return { ok: true, status: 200, json: async () => body };
  };
  window.eval(source); // jsdom fires DOMContentLoaded itself, once
  for (let i = 0; i < 30; i++) await tick();
  assert.equal(window.location.search, "", "the code and state leave the address bar");
  assert.equal(window.location.hash, "#invoices");
  assert.equal(window.localStorage.getItem("pp_return_view"), null);
  assert.equal(window.document.querySelector("#ac-invoices").closest(".ac-view").hidden, false);
  assert.equal(window.document.querySelector('#ac-side [aria-current="page"]').getAttribute("data-route"), "invoices");
});

test("#delete-account leaves the shell for the standalone deletion page and back", async (t) => {
  const page = await checkoutPage(t, { url: "https://pigeonpost.dev/account#security" });
  const { window } = page;
  assert.equal(window.document.querySelector("#ac-2fa").closest(".ac-view").hidden, false);
  assert.ok(window.document.body.classList.contains("ac-shell"));
  window.location.hash = "#delete-account";
  await new Promise((resolve) => setTimeout(resolve, 30));
  assert.ok(window.document.querySelector("#ac-deletion-body"));
  assert.equal(window.document.querySelector("#ac-app"), null);
  assert.equal(window.document.body.classList.contains("ac-shell"), false);
  window.location.hash = "#privacy";
  await new Promise((resolve) => setTimeout(resolve, 30));
  assert.ok(window.document.querySelector("#ac-app"));
  assert.equal(window.document.querySelector('#ac-side [aria-current="page"]').getAttribute("data-route"), "privacy");
});

test("the return from iyzico polls the bridge and never trusts the redirect", async (t) => {
  const intent = "0123456789abcdef0123456789abcdef";
  const page = await checkoutPage(t, { url: `https://pigeonpost.dev/account?checkout=${intent}`, statuses: ["pending", "active"] });
  for (let i = 0; i < 40 && !page.calls.filter((c) => c.path === `/api/v1/cloud/checkout/${intent}`).length >= 2; i++) await tick();
  for (let i = 0; i < 40; i++) await tick();
  const polls = page.calls.filter((c) => c.path === `/api/v1/cloud/checkout/${intent}`);
  assert.equal(polls.length, 2);
  assert.equal(page.window.location.search, "");
  assert.equal(page.window.document.querySelector("#ac-toast").textContent, "Your Pigeonpost Cloud plan is active.");
  assert.equal(page.calls.some((c) => c.method === "POST"), false);
});

test("the hosted form page replays the intent and runs only nonce-tagged iyzico scripts", async (t) => {
  const html = readFileSync(new URL("../../site/billing/checkout.html", import.meta.url), "utf8");
  const script = readFileSync(new URL("../../site/billing/checkout.js", import.meta.url), "utf8");
  assert.match(html, /<meta name="csp-nonce" content="__CSP_NONCE__">/);
  const dom = new JSDOM(html.replace("__CSP_NONCE__", "nonce123"), { url: "https://pigeonpost.dev/billing/checkout", runScripts: "outside-only", virtualConsole: new VirtualConsole() });
  t.after(() => dom.window.close());
  const { window } = dom;
  window.eval(config);
  window.sessionStorage.setItem("pp_session", "member-token");
  const pending = { kind: "cloud", intent_id: "a".repeat(32), plan_code: "cloud-2names-yearly-usd", billing_profile_id: "p", locale: "en" };
  window.sessionStorage.setItem("pp_cloud_checkout", JSON.stringify(pending));
  const calls = [];
  window.fetch = async (url, init) => {
    calls.push({ url, body: JSON.parse(init.body), auth: init.headers.authorization });
    return { ok: true, status: 200, json: async () => ({ plan_code: "cloud-2names-yearly-usd", amount: "16.00", currency: "USD",
      checkout_form_content: '<script>window.__iyzi = 1;</script><script src="https://evil.example/x.js"></script><script src="https://static.iyzipay.com/checkoutform/v2/bundle.js"></script><div id="iyzipay-checkout-form" class="responsive"></div><img src=x onerror="window.__xss=1">' }) };
  };
  window.eval(script);
  for (let i = 0; i < 20; i++) await tick();
  assert.equal(calls.length, 1);
  assert.equal(calls[0].url, "/api/v1/cloud/checkout");
  assert.deepEqual(calls[0].body, { intent_id: pending.intent_id, plan_code: pending.plan_code, billing_profile_id: "p", locale: "en" });
  const scripts = [...window.document.querySelectorAll("#pp-pay-form script")];
  assert.equal(scripts.length, 2, "foreign script hosts are dropped");
  assert.ok(scripts.every((s) => s.nonce === "nonce123" || s.getAttribute("nonce") === "nonce123"));
  assert.equal(scripts[1].src, "https://static.iyzipay.com/checkoutform/v2/bundle.js");
  assert.ok(window.document.querySelector("#iyzipay-checkout-form"));
  assert.match(window.document.querySelector("#pp-pay-summary").textContent, /2 names: \$16\.00 per year/);
  assert.equal(window.sessionStorage.getItem("pp_cloud_checkout") !== null, true, "a reload replays the same intent");
});


test("the hosted form page sends a company Nest checkout with its org", async (t) => {
  const html = readFileSync(new URL("../../site/billing/checkout.html", import.meta.url), "utf8");
  const script = readFileSync(new URL("../../site/billing/checkout.js", import.meta.url), "utf8");
  const dom = new JSDOM(html.replace("__CSP_NONCE__", "nonce123"), { url: "https://pigeonpost.dev/billing/checkout", runScripts: "outside-only", virtualConsole: new VirtualConsole() });
  t.after(() => dom.window.close());
  const { window } = dom;
  window.eval(config);
  window.sessionStorage.setItem("pp_session", "member-token");
  const pending = { kind: "nest", intent_id: "c".repeat(32), plan_code: "nest-ai-starter-monthly-usd", billing_profile_id: "p", locale: "en", org_id: "org_acme0123" };
  window.sessionStorage.setItem("pp_cloud_checkout", JSON.stringify(pending));
  const calls = [];
  window.fetch = async (url, init) => {
    calls.push({ url, body: JSON.parse(init.body) });
    return { ok: true, status: 200, json: async () => ({ plan_code: pending.plan_code, amount: "10.00", currency: "USD", org_id: pending.org_id,
      checkout_form_content: '<div id="iyzipay-checkout-form" class="responsive"></div>' }) };
  };
  window.eval(script);
  for (let i = 0; i < 20; i++) await tick();
  assert.equal(calls.length, 1);
  assert.equal(calls[0].url, "/api/v1/nests/checkout");
  assert.deepEqual(calls[0].body, { intent_id: pending.intent_id, plan_code: pending.plan_code, billing_profile_id: "p", locale: "en", org_id: "org_acme0123" });
  assert.match(window.document.querySelector("#pp-pay-summary").textContent, /^Company Nest package: \$10\.00 per month/);
});

test("the hosted form page sends a marketplace checkout for a company and says the term", async (t) => {
  const html = readFileSync(new URL("../../site/billing/checkout.html", import.meta.url), "utf8");
  const script = readFileSync(new URL("../../site/billing/checkout.js", import.meta.url), "utf8");
  const dom = new JSDOM(html.replace("__CSP_NONCE__", "nonce123"), { url: "https://pigeonpost.dev/billing/checkout", runScripts: "outside-only", virtualConsole: new VirtualConsole() });
  t.after(() => dom.window.close());
  const { window } = dom;
  window.eval(config);
  window.sessionStorage.setItem("pp_session", "member-token");
  const pending = { kind: "marketplace", intent_id: "d".repeat(32), listing_id: "lst_a", title: "HR helper",
    owner: { kind: "org", id: "org_acme0123" }, billing_profile_id: "p", locale: "en" };
  window.sessionStorage.setItem("pp_cloud_checkout", JSON.stringify(pending));
  const calls = [];
  window.fetch = async (url, init) => {
    calls.push({ url, body: JSON.parse(init.body) });
    return { ok: true, status: 200, json: async () => ({ intent_id: pending.intent_id, installation_id: "ins_x", listing_id: "lst_a",
      term: "monthly", amount_minor: 12000, currency: "USD", checkout_form_content: '<div id="iyzipay-checkout-form" class="responsive"></div>' }) };
  };
  window.eval(script);
  for (let i = 0; i < 20; i++) await tick();
  assert.equal(calls.length, 1);
  assert.equal(calls[0].url, "/api/v1/marketplace/checkout");
  assert.deepEqual(calls[0].body, { intent_id: pending.intent_id, listing_id: "lst_a", owner: { kind: "org", id: "org_acme0123" }, billing_profile_id: "p", locale: "en" });
  assert.match(window.document.querySelector("#pp-pay-summary").textContent, /^HR helper for your company: \$120\.00 per month, renewing monthly/);
});

test("the hosted form page warns about a store plan and starts the form only after the person confirms", async (t) => {
  const html = readFileSync(new URL("../../site/billing/checkout.html", import.meta.url), "utf8");
  const script = readFileSync(new URL("../../site/billing/checkout.js", import.meta.url), "utf8");
  const dom = new JSDOM(html.replace("__CSP_NONCE__", "nonce123"), { url: "https://pigeonpost.dev/billing/checkout", runScripts: "outside-only", virtualConsole: new VirtualConsole() });
  t.after(() => dom.window.close());
  const { window } = dom;
  window.eval(config);
  window.sessionStorage.setItem("pp_session", "member-token");
  const pending = { kind: "cloud", intent_id: "b".repeat(32), plan_code: "cloud-2names-yearly-usd", billing_profile_id: "p", locale: "en" };
  window.sessionStorage.setItem("pp_cloud_checkout", JSON.stringify(pending));
  const calls = [];
  window.fetch = async (url, init) => {
    const body = JSON.parse(init.body);
    calls.push({ url, body });
    if (!body.confirm_other_store) {
      return { ok: false, status: 409, json: async () => ({ code: "store_subscription_active", store: "app_store",
        error: "You already pay for Pigeonpost names through the App Store." }) };
    }
    return { ok: true, status: 200, json: async () => ({ plan_code: "cloud-2names-yearly-usd", amount: "16.00", currency: "USD",
      checkout_form_content: '<div id="iyzipay-checkout-form" class="responsive"></div>' }) };
  };
  window.eval(script);
  for (let i = 0; i < 20; i++) await tick();
  assert.equal(calls.length, 1);
  assert.match(window.document.querySelector("#pp-pay-summary").textContent, /App Store/);
  assert.equal(window.document.querySelector("#iyzipay-checkout-form"), null, "no payment form before the person confirms");
  assert.ok(window.sessionStorage.getItem("pp_cloud_checkout"), "the intent survives the warning");
  window.document.querySelector("#pp-pay-confirm").click();
  for (let i = 0; i < 20; i++) await tick();
  assert.equal(calls.length, 2);
  assert.deepEqual(calls[1].body, { ...{ intent_id: pending.intent_id, plan_code: pending.plan_code, billing_profile_id: "p", locale: "en" }, confirm_other_store: true });
  assert.ok(window.document.querySelector("#iyzipay-checkout-form"));
  assert.match(window.document.querySelector("#pp-pay-summary").textContent, /2 names/);
});

async function account(t, { remember, rejectFresh = false, existingProfile = null }) {
  const dom = new JSDOM('<main id="account-root"></main>', {
    url: "https://pigeonpost.dev/account?code=test-code&state=test-state",
    runScripts: "outside-only", virtualConsole: new VirtualConsole(),
  });
  t.after(() => dom.window.close());
  const { window } = dom;
  window.eval(config);
  window.localStorage.setItem("pp_remember", remember ? "1" : "0");
  window.localStorage.setItem("pp_state", "test-state");
  window.localStorage.setItem("pp_pkce", "test-verifier");
  const calls = [];
  let profile = existingProfile;
  window.fetch = async (url, init = {}) => {
    const path = new URL(url, window.location.origin).pathname;
    calls.push({ path, method: init.method || "GET", body: init.body, authorization: init.headers?.authorization });
    const response = (body, status = 200) => ({ ok: status < 400, status, json: async () => body });
    if (path === "/api/v1/auth/exchange") {
      return response({ session: "original-access", refresh: "original-refresh", expiresIn: 300 });
    }
    if (path === "/api/v1/auth/refresh") {
      assert.equal(JSON.parse(init.body).refresh, "original-refresh");
      return response({ session: "renewed-access", refresh: "rotated-refresh", expiresIn: 300 });
    }
    if (path === "/api/v1/me/overview") {
      return response({ subscriptions: [], handles: [], invoices: [], paymentMethods: [], billingProfiles: profile ? [profile] : [] });
    }
    if (path === "/api/v1/billing/profiles" || path === `/api/v1/billing/profiles/${profile?.id}`) {
      // The original access token expired while the user was filling in the form.
      if (rejectFresh || init.headers?.authorization !== "Bearer renewed-access") {
        return response({ error: "Unauthorized" }, 401);
      }
      if (path !== "/api/v1/billing/profiles") assert.equal(init.method, "PATCH");
      profile = { id: "profile-1", ...profile, ...JSON.parse(init.body) };
      return response(profile, init.method === "PATCH" ? 200 : 201);
    }
    return response({ identities: [], keys: [], namespaces: [], packages: [] });
  };
  window.eval(source);
  for (let i = 0; i < 30 && !window.document.querySelector("#ac-billing form, #ac-billing-edit"); i++) await tick();
  if (existingProfile) window.document.querySelector("#ac-billing-edit").click();
  const form = window.document.querySelector("#ac-billing form");
  assert.ok(form, "sign-in completes and the billing form appears");
  const fields = {
    legal_name: "Example User", billing_email: "billing@example.test", phone: "",
    line1: "1 Example Street", city: "Example City", state: "Example District", postal_code: "12345", country: "TR",
    identity_number: "10000000000",
  };
  if (!existingProfile) for (const [key, value] of Object.entries(fields)) form.elements.namedItem(key).value = value;
  return {
    window, calls, form, fields,
    get profile() { return profile; },
    async save() {
      await form.onsubmit({ preventDefault() {} });
      for (let i = 0; i < 5; i++) await tick();
    },
  };
}

test("individual Turkish billing requires TCKN and district before saving", async (t) => {
  const page = await account(t, { remember: false });
  const identity = page.form.elements.namedItem("identity_number");
  const district = page.form.elements.namedItem("state");
  assert.equal(identity.disabled, false);
  assert.equal(identity.required, true);
  assert.equal(identity.closest("label").style.display, "");
  for (const invalid of ["", "1234567890", "1234567890x"]) {
    identity.value = invalid;
    await page.save();
    assert.equal(page.calls.some((c) => c.path.includes("/billing/profiles")), false);
  }
  identity.value = page.fields.identity_number;
  district.value = "   ";
  await page.save();
  assert.equal(district.validity.valueMissing, true);
  assert.equal(page.profile, null);
  district.value = page.fields.state;
  await page.save();
  assert.equal(page.profile.identity_number, page.fields.identity_number);
  assert.equal(page.profile.address.state, page.fields.state);
  assert.equal(page.profile.tax_id, null);
});

test("editing adds TCKN to the existing profile and preserves its nested address", async (t) => {
  const existingProfile = {
    id: "saved-profile", account_type: "individual", legal_name: "Example User", billing_email: "billing@example.test",
    identity_number: null, address: { line1: "1 Example Street", line2: "Unit 2", city: "Example City",
      state: null, postal_code: "12345", country: "Türkiye" },
  };
  const page = await account(t, { remember: false, existingProfile });
  for (const [key, value] of Object.entries(existingProfile.address)) {
    assert.equal(page.form.elements.namedItem(key).value, value || "");
  }
  assert.equal(page.form.elements.namedItem("identity_number").required, true);
  page.form.elements.namedItem("identity_number").value = "10000000000";
  page.form.elements.namedItem("state").value = "Example District";
  await page.save();
  const writes = page.calls.filter((c) => c.path.includes("/billing/profiles"));
  assert.equal(writes.length, 2, "expired token is retried once");
  assert.ok(writes.every((c) => c.method === "PATCH" && c.path === "/api/v1/billing/profiles/saved-profile"));
  const payload = JSON.parse(writes.at(-1).body);
  assert.equal(payload.identity_number, "10000000000");
  assert.deepEqual(payload.address, { ...existingProfile.address, state: "Example District" });
  assert.equal(page.profile.id, existingProfile.id);
  page.window.document.querySelector("#ac-billing-edit").click();
  assert.equal(page.window.document.querySelector('[name="identity_number"]').value, "10000000000");
  assert.equal(page.window.document.querySelector('[name="state"]').value, "Example District");
});

test("company billing uses its VKN and does not submit the hidden individual TCKN", async (t) => {
  const page = await account(t, { remember: false });
  const field = (name) => page.form.elements.namedItem(name);
  field("account_type").value = "entity";
  field("account_type").dispatchEvent(new page.window.Event("change"));
  field("entity_name").value = "Example Company";
  field("tax_id").value = "1234567890";
  assert.equal(field("identity_number").disabled, true);
  assert.equal(field("identity_number").required, false);
  assert.equal(field("tax_id").required, true);
  await page.save();
  assert.equal(page.profile.tax_id, "1234567890");
  assert.equal(page.profile.identity_number, null);
  assert.equal(page.profile.entity_name, "Example Company");
});

for (const accountType of ["individual", "entity"]) {
  test(`foreign ${accountType} billing does not require Turkish identity or district`, async (t) => {
    const page = await account(t, { remember: false });
    const field = (name) => page.form.elements.namedItem(name);
    field("account_type").value = accountType;
    field("country").value = "Germany";
    field("country").dispatchEvent(new page.window.Event("input"));
    field("identity_number").value = "";
    field("state").value = "";
    field("tax_id").value = "DE123456789";
    field("entity_name").value = "Example Company";
    assert.equal(field("identity_number").required, false);
    assert.equal(field("state").required, false);
    await page.save();
    assert.equal(page.profile.address.country, "Germany");
    assert.equal(page.profile.identity_number, null);
    assert.equal(page.profile.tax_id, accountType === "entity" ? "DE123456789" : null);
  });
}

for (const remember of [false, true]) {
  test(`billing saves after access-token expiry with remember-me ${remember ? "enabled" : "disabled"}`, async (t) => {
    const page = await account(t, { remember });
    const { window, calls } = page;
    const refreshStorage = remember ? window.localStorage : window.sessionStorage;
    const otherStorage = remember ? window.sessionStorage : window.localStorage;
    assert.equal(refreshStorage.getItem("pp_session"), "original-access");
    assert.equal(otherStorage.getItem("pp_session"), null);
    assert.equal(refreshStorage.getItem("pp_refresh"), "original-refresh");
    assert.equal(otherStorage.getItem("pp_refresh"), null);

    await page.save();

    assert.equal(page.profile?.id, "profile-1");
    assert.equal(page.profile.legal_name, page.fields.legal_name);
    assert.equal(page.profile.address.line1, page.fields.line1);
    assert.deepEqual(calls.filter((c) => c.path === "/api/v1/billing/profiles").map((c) => c.authorization), ["Bearer original-access", "Bearer renewed-access"]);
    assert.equal(calls.filter((c) => c.path === "/api/v1/auth/refresh").length, 1);
    assert.equal(refreshStorage.getItem("pp_refresh"), "rotated-refresh");
    assert.equal(refreshStorage.getItem("pp_session"), "renewed-access");
    assert.equal(otherStorage.getItem("pp_refresh"), null);
    assert.ok(window.document.querySelector("#ac-billing-edit"), "saved profile is shown");

    window.document.querySelector("#ac-logout").click();
    assert.equal(window.localStorage.getItem("pp_session"), null);
    assert.equal(window.sessionStorage.getItem("pp_session"), null);
    assert.equal(window.localStorage.getItem("pp_refresh"), null);
    assert.equal(window.sessionStorage.getItem("pp_refresh"), null);
    assert.ok(window.document.querySelector("#ac-signin"));
  });
}

test("another tab's remembered sign-in cannot replace this tab's temporary account", async (t) => {
  const page = await account(t, { remember: false });
  const { window } = page;
  // localStorage is shared between tabs; another tab can sign in as a different account.
  window.localStorage.setItem("pp_session", "other-account-access");
  window.localStorage.setItem("pp_refresh", "other-account-refresh");
  window.localStorage.setItem("pp_remember", "1");

  await page.save();

  assert.equal(page.profile?.id, "profile-1");
  assert.deepEqual(page.calls.filter((c) => c.path === "/api/v1/billing/profiles").map((c) => c.authorization), ["Bearer original-access", "Bearer renewed-access"]);
  assert.equal(window.sessionStorage.getItem("pp_session"), "renewed-access");
  assert.equal(window.sessionStorage.getItem("pp_refresh"), "rotated-refresh");
  assert.equal(window.localStorage.getItem("pp_session"), "other-account-access");
  assert.equal(window.localStorage.getItem("pp_refresh"), "other-account-refresh");
});

test("a rejected renewed token stops after one retry and keeps the billing form", async (t) => {
  const page = await account(t, { remember: true, rejectFresh: true });
  await page.save();
  assert.equal(page.profile, null);
  assert.equal(page.calls.filter((c) => c.path === "/api/v1/billing/profiles").length, 2);
  assert.equal(page.calls.filter((c) => c.path === "/api/v1/auth/refresh").length, 1);
  assert.equal(page.form.querySelector(".ac-msg").textContent, "Could not save: Unauthorized");
  assert.equal(page.form.elements.namedItem("legal_name").value, page.fields.legal_name);
  assert.ok(page.window.document.querySelector("#ac-logout"));
});

test("logging out during renewal cannot restore the old session", async (t) => {
  const page = await account(t, { remember: false });
  const { window } = page;
  const originalFetch = window.fetch;
  let releaseRefresh;
  let refreshStarted;
  const started = new Promise((resolve) => { refreshStarted = resolve; });
  const released = new Promise((resolve) => { releaseRefresh = resolve; });
  window.fetch = async (url, init) => {
    if (new URL(url, window.location.origin).pathname === "/api/v1/auth/refresh") {
      refreshStarted();
      await released;
    }
    return originalFetch(url, init);
  };

  const saving = page.save();
  await started;
  window.document.querySelector("#ac-logout").click();
  releaseRefresh();
  await saving;

  assert.equal(page.profile, null);
  assert.equal(page.calls.filter((c) => c.path === "/api/v1/billing/profiles").length, 1);
  for (const storage of [window.localStorage, window.sessionStorage]) {
    assert.equal(storage.getItem("pp_session"), null);
    assert.equal(storage.getItem("pp_refresh"), null);
  }
  assert.ok(window.document.querySelector("#ac-signin"));
});

// ---- the appearance switch (site/theme.js) ---------------------------------------------------

const siteFile = (name) => readFileSync(new URL(`../../site/${name}`, import.meta.url), "utf8");

test("the account shell has the theme switch beside the logo; it cycles, persists and steers the logo", async (t) => {
  const dom = new JSDOM('<html><head><meta name="color-scheme" content="light dark"></head><body><main id="account-root"></main></body></html>',
    { url: "https://pigeonpost.dev/account", runScripts: "outside-only", virtualConsole: new VirtualConsole() });
  t.after(() => dom.window.close());
  const { window } = dom;
  window.localStorage.setItem("ppi_theme", "dark");
  window.eval(siteFile("theme.js"));
  const root = window.document.documentElement;
  assert.equal(root.getAttribute("data-theme"), "dark", "a stored choice applies before the page script runs");
  window.eval(config);
  window.sessionStorage.setItem("pp_session", "member-token");
  window.fetch = async () => ({ ok: true, status: 200, json: async () => ({ handles: [], invoices: [], billingProfiles: [], identities: [], keys: [] }) });
  window.eval(source);
  for (let i = 0; i < 10; i++) await tick();
  const toggle = window.document.querySelector(".ac-top [data-theme-toggle]");
  assert.ok(toggle, "the switch is in the top bar");
  assert.equal(toggle.previousElementSibling.className, "ac-brand", "right after the logo");
  assert.equal(toggle.getAttribute("aria-label"), "Theme: Dark");
  const source_ = window.document.querySelector(".ac-top picture source");
  assert.equal(source_.getAttribute("media"), "all", "the white logo shows when Dark is forced");
  toggle.click();
  assert.equal(root.hasAttribute("data-theme"), false);
  assert.equal(window.localStorage.getItem("ppi_theme"), null, "System is the absence of a choice");
  assert.equal(source_.getAttribute("media"), "(prefers-color-scheme: dark)");
  assert.equal(toggle.getAttribute("aria-label"), "Theme: System");
  toggle.click();
  assert.equal(root.getAttribute("data-theme"), "light");
  assert.equal(window.localStorage.getItem("ppi_theme"), "light");
  assert.equal(source_.getAttribute("media"), "not all");
  assert.equal(window.document.querySelector('meta[name="color-scheme"]').getAttribute("content"), "light");
});

test("the signed-out account page loads theme.js before its styles and has the switch", () => {
  const html = siteFile("account.html");
  assert.ok(html.indexOf("/theme.js?v=") < html.indexOf('rel="stylesheet"'));
  assert.match(html, /<\/picture><\/a>\s*<button type="button" class="theme-toggle" data-theme-toggle/);
});

test("site stylesheets honour data-theme with the same dark values as the media query", () => {
  const norm = (block) => block.split("\n").map((l) => l.trim()).filter(Boolean).join("\n");
  for (const name of ["style.css", "account.css"]) {
    const css = siteFile(name);
    const media = /@media \(prefers-color-scheme: dark\) \{\n  :root:not\(\[data-theme="light"\]\) \{([^}]*)\}/.exec(css);
    const forced = /:root\[data-theme="dark"\] \{([^}]*)\}/.exec(css);
    assert.ok(media && forced, name);
    assert.equal(norm(forced[1]), norm(media[1]), `${name}: the two dark blocks differ`);
  }
});
