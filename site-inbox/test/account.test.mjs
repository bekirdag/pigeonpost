// Exercise the actual account page with fake OIDC responses and fictional billing details.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { JSDOM, VirtualConsole } from "jsdom";

const source = readFileSync(new URL("../../site/account.js", import.meta.url), "utf8");
const config = readFileSync(new URL("../../site/account-config.js", import.meta.url), "utf8");
const tick = () => new Promise((resolve) => setImmediate(resolve));

async function deletionPage(t, { signedIn = true, existing = null, status = 200, deferPost = false } = {}) {
  const dom = new JSDOM('<main id="account-root"></main>', { url: "https://pigeonpost.dev/account#delete-account", runScripts: "outside-only", virtualConsole: new VirtualConsole() });
  t.after(() => dom.window.close());
  const { window } = dom;
  window.eval(config);
  if (signedIn) window.sessionStorage.setItem("pp_session", "member-token");
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

async function checkoutPage(t, { checkoutStatus = 200, checkoutBody, url = "https://pigeonpost.dev/account", completeStatus = 200, recovery = false } = {}) {
  const dom = new JSDOM('<main id="account-root"></main>', { url, runScripts: "outside-only", virtualConsole: new VirtualConsole() });
  t.after(() => dom.window.close());
  const { window } = dom;
  window.eval(config);
  window.sessionStorage.setItem("pp_session", "member-token");
  const originalTimeout = window.setTimeout.bind(window);
  window.setTimeout = (fn, delay, ...args) => originalTimeout(fn, delay === 350 ? 0 : delay, ...args);
  const calls = [];
  window.fetch = async (url, init = {}) => {
    const path = new URL(url, window.location.origin).pathname;
    calls.push({ path, body: init.body && JSON.parse(init.body) });
    const response = (body, status = 200) => ({ ok: status < 400, status, json: async () => body });
    if (path === "/api/v1/me/overview") return response({ subscriptions: [], handles: [], invoices: [], billingProfiles: [{ id: "profile-1" }], paymentMethods: [{ id: "card-1" }] });
    if (path.endsWith("/availability")) return response({ available: !recovery || init.headers?.authorization === "Bearer member-token", known: true });
    if (path === "/api/v1/checkout") return response(checkoutBody || { status: "payment_action_required", subscriptionId: "subscription-1", paymentId: "payment-1", checkoutUrl: "https://bank.example.test/checkout" }, checkoutStatus);
    if (path === "/api/v1/checkout/complete") return response(completeStatus === 200 ? { status: "active", handle: "example", bound: true } : { error: "Payment is still under review" }, completeStatus);
    return response({ identities: [], keys: [], namespaces: [], packages: [] });
  };
  window.eval(source);
  for (let i = 0; i < 30 && !window.document.querySelector("#ac-buy"); i++) await tick();
  return {
    window, calls,
    async buy() {
      const input = window.document.querySelector("#ac-handle");
      input.value = "example"; input.dispatchEvent(new window.Event("input"));
      const button = window.document.querySelector("#ac-buy");
      for (let i = 0; i < 30 && button.disabled; i++) await new Promise((resolve) => setTimeout(resolve, 1));
      assert.equal(button.disabled, false);
      button.click(); button.click();
      for (let i = 0; i < 20; i++) await tick();
    },
  };
}

test("Get it keeps the payment reference before redirect and ignores a double click", async (t) => {
  const page = await checkoutPage(t);
  await page.buy();
  const calls = page.calls.filter((c) => c.path === "/api/v1/checkout");
  assert.equal(calls.length, 1);
  assert.match(calls[0].body.operationId, /^[a-f0-9]{32}$/);
  const pending = JSON.parse(page.window.sessionStorage.getItem("pp_checkout"));
  assert.equal(pending.subscriptionId, "subscription-1");
  assert.equal(pending.paymentId, "payment-1");
  assert.equal(page.window.localStorage.getItem("pp_bought_handle"), null, "starting payment does not claim that the handle is paid for");
  assert.equal(page.calls.some((c) => c.path === "/api/v1/handles/claim"), false);
});

test("the signed-in owner can select an expired handle during recovery", async (t) => {
  const page = await checkoutPage(t, { recovery: true });
  await page.buy();
  assert.equal(page.calls.filter((c) => c.path === "/api/v1/checkout").length, 1);
});

test("an ambiguous checkout failure keeps a stable attempt and never claims payment success", async (t) => {
  const page = await checkoutPage(t, { checkoutStatus: 502, checkoutBody: { error: "Example checkout rejection" } });
  await page.buy();
  const first = page.calls.find((c) => c.path === "/api/v1/checkout");
  await page.buy();
  const attempts = page.calls.filter((c) => c.path === "/api/v1/checkout");
  assert.equal(attempts[1].body.operationId, first.body.operationId);
  assert.equal(page.window.localStorage.getItem("pp_bought_handle"), null);
  assert.equal(page.window.document.querySelector("#ac-toast").textContent, "Checkout couldn't start: Example checkout rejection");
});

test("a definite payment rejection allows a fresh attempt after correction", async (t) => {
  const page = await checkoutPage(t, { checkoutStatus: 400, checkoutBody: { error: "Payment declined" } });
  await page.buy();
  const first = page.calls.find((c) => c.path === "/api/v1/checkout");
  await page.buy();
  assert.notEqual(page.calls.filter((c) => c.path === "/api/v1/checkout")[1].body.operationId, first.body.operationId);
});

test("a completed checkout shows the delivered handle without another hosted step", async (t) => {
  const page = await checkoutPage(t, { checkoutBody: { status: "active", handle: "example", bound: true } });
  await page.buy();
  assert.equal(page.window.sessionStorage.getItem("pp_checkout"), null);
  assert.equal(page.window.document.querySelector("#ac-toast").textContent, "/example is yours.");
});

for (const completeStatus of [200, 400]) {
  test(`bank return checks payment and ${completeStatus === 200 ? "shows ownership" : "preserves pending state"}`, async (t) => {
    const page = await checkoutPage(t, { url: "https://pigeonpost.dev/account?checkout_return=1&subscription_id=subscription-1&payment_id=payment-1", completeStatus });
    for (let i = 0; i < 10; i++) await tick();
    const completion = page.calls.find((c) => c.path === "/api/v1/checkout/complete");
    assert.deepEqual(completion.body, { subscriptionId: "subscription-1", paymentId: "payment-1" });
    assert.equal(page.calls.some((c) => c.path === "/api/v1/checkout"), false);
    assert.equal(page.window.location.search, "");
    assert.equal(Boolean(page.window.sessionStorage.getItem("pp_checkout")), completeStatus !== 200);
  });
}

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
