// Exercise the actual account page with fake OIDC responses and fictional billing details.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { JSDOM, VirtualConsole } from "jsdom";

const source = readFileSync(new URL("../../site/account.js", import.meta.url), "utf8");
const config = readFileSync(new URL("../../site/account-config.js", import.meta.url), "utf8");
const tick = () => new Promise((resolve) => setImmediate(resolve));

async function account(t, { remember, rejectFresh = false }) {
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
  let profile = null;
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
    if (path === "/api/v1/billing/profiles") {
      // The original access token expired while the user was filling in the form.
      if (rejectFresh || init.headers?.authorization !== "Bearer renewed-access") {
        return response({ error: "Unauthorized" }, 401);
      }
      profile = { id: "profile-1", ...JSON.parse(init.body) };
      return response(profile, 201);
    }
    return response({ identities: [], keys: [], namespaces: [], packages: [] });
  };
  window.eval(source);
  for (let i = 0; i < 30 && !window.document.querySelector("#ac-billing form"); i++) await tick();
  const form = window.document.querySelector("#ac-billing form");
  assert.ok(form, "sign-in completes and the billing form appears");
  const fields = {
    legal_name: "Example User", billing_email: "billing@example.test", phone: "",
    line1: "1 Example Street", city: "Example City", postal_code: "12345", country: "TR",
  };
  for (const [key, value] of Object.entries(fields)) form.elements.namedItem(key).value = value;
  return {
    window, calls, form, fields,
    get profile() { return profile; },
    async save() {
      await form.onsubmit({ preventDefault() {} });
      for (let i = 0; i < 5; i++) await tick();
    },
  };
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
