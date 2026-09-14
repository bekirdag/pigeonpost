import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { webcrypto } from "node:crypto";
import { JSDOM, VirtualConsole } from "jsdom";

const APP = new URL("../", import.meta.url);
const read = (file) => readFileSync(new URL(file, APP), "utf8");
const identity = { address: "/k/new-inbox", label: "My inbox" };
const response = (body, status = 200) => ({
  ok: status >= 200 && status < 300, status, json: async () => body,
});

async function until(predicate) {
  const deadline = Date.now() + 2000;
  while (!predicate()) {
    assert.ok(Date.now() < deadline, "onboarding did not reach the expected state");
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

function app(t, { signedIn = true, existing = false, intercept } = {}) {
  const dom = new JSDOM(read("index.html"), {
    url: "https://inbox.pigeonpost.dev/", runScripts: "outside-only",
    pretendToBeVisual: true, virtualConsole: new VirtualConsole(),
  });
  t.after(() => dom.window.close());
  const w = dom.window;
  const server = { identities: existing ? [identity] : [], calls: [] };
  Object.defineProperty(w, "crypto", { value: webcrypto, configurable: true });
  w.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });
  w.fetch = async (url, opts = {}) => {
    const parsed = new URL(url);
    const call = { path: parsed.pathname, search: parsed.searchParams, method: opts.method || "GET", opts };
    server.calls.push(call);
    const intercepted = await intercept?.(call, server);
    if (intercepted) return intercepted;
    if (call.path === "/v1/identities") {
      if (call.method === "POST") {
        assert.equal(opts.headers.authorization, "Bearer pk_test_account");
        assert.deepEqual(JSON.parse(opts.body), {});
        server.identities.push(identity);
        return response({ address: identity.address }, 201);
      }
      return response({ identities: server.identities });
    }
    if (call.path === "/v1/whoami") return response({ ...identity, handle: null });
    if (call.path === "/v1/events") return response({ error: "not_found" }, 404);
    if (call.path === "/v1/inbox") {
      if (parsed.searchParams.has("wait")) return new Promise(() => {});
      return response({ messages: [] });
    }
    if (call.path === "/v1/contacts") return response({ contacts: [], policy: {} });
    if (call.path === "/v1/threads") return response({ threads: [] });
    if (call.path === "/v1/archive") return response({ archived: [] });
    throw new Error(`Unexpected request: ${call.method} ${call.path}`);
  };
  if (signedIn) w.localStorage.setItem("ppi_token", "pk_test_account");
  w.eval(read("config.js"));
  w.eval(read("app.js"));
  const $ = (id) => w.document.getElementById(id);
  // A visible child inside a hidden parent was the production bug. Check the entire ancestry.
  const visible = (id) => $(id).closest("[hidden]") === null;
  const creates = () => server.calls.filter((c) => c.path === "/v1/identities" && c.method === "POST");
  const offered = () => !$("create-inbox-btn").hidden && !$("create-inbox-btn").disabled;
  return { w, $, server, visible, creates, offered };
}

test("a signed-in newcomer can reach Create my inbox and open it with one request", async (t) => {
  const a = app(t);
  await until(a.offered);
  assert.ok(a.visible("create-inbox-btn"));
  assert.ok(!a.visible("signin-btn"));
  assert.ok(!a.visible("app"));
  a.$("create-inbox-btn").click();
  a.$("create-inbox-btn").click();
  await until(() => a.visible("app") && a.creates().length === 1);
  assert.ok(!a.visible("signin"));
  assert.equal(a.creates().length, 1);
  assert.equal(a.server.identities.length, 1);
  assert.equal(a.$("me-sub").textContent, identity.address);
});

test("existing accounts open their mailbox without creating another", async (t) => {
  const a = app(t, { existing: true });
  await until(() => a.$("me-sub").textContent === identity.address);
  assert.ok(a.visible("app"));
  assert.ok(!a.visible("create-inbox-btn"));
  assert.equal(a.creates().length, 0);
});

test("inbox settings read the same handles across providers and distinguish lookup failures", async t => {
  let failure = false;
  const a = app(t, { existing: true, intercept: call => {
    if (call.path !== "/v1/me/handles") return;
    return failure ? response({ error: "unavailable" }, 503) : response({ handles: [
      { namespace: "apple-name", source: "apple", active: true },
      { namespace: "google-name", source: "google", active: false },
      { namespace: "web-name", source: "entitlement", active: true },
    ] });
  } });
  await until(() => a.$("me-sub").textContent === identity.address);
  a.$("settings-btn").click();
  a.$("settings-nav-handles").click();
  assert.ok(a.visible("acct-handles"));
  await until(() => a.$("acct-handles").textContent.includes("/google-name"));
  assert.match(a.$("acct-handles").textContent, /apple-name · Active · App Store/);
  assert.match(a.$("acct-handles").textContent, /google-name · Expired · Google Play/);
  assert.match(a.$("acct-handles").textContent, /web-name · Active · Pigeonpost/);
  failure = true; a.$("acct-handles-refresh").click();
  await until(() => a.$("acct-handles").textContent.includes("Could not refresh"));
  assert.doesNotMatch(a.$("acct-handles").textContent, /No handles/);
});

test("signed-out visitors see only sign-in and make no account requests", async (t) => {
  const a = app(t, { signedIn: false });
  await until(() => a.visible("signin-btn"));
  assert.ok(!a.visible("create-inbox-btn"));
  assert.ok(!a.visible("app"));
  assert.equal(a.server.calls.length, 0);
});

test("an account still loading is not presented as a ready inbox", async (t) => {
  let release;
  const loading = new Promise((resolve) => { release = resolve; });
  const a = app(t, { intercept: (c) => c.path === "/v1/identities" ? loading : undefined });
  await until(() => a.server.calls.length > 0);
  assert.ok(!a.visible("app"));
  assert.ok(!a.visible("signin-btn"));
  assert.match(a.$("signin-note").textContent, /loading/i);
  release(response({ identities: [] }));
  await until(a.offered);
});

test("a refused creation stays visible and can be retried", async (t) => {
  let fail = true;
  const a = app(t, { intercept: (c) => {
    if (c.path === "/v1/identities" && c.method === "POST" && fail) {
      fail = false;
      return response({ error: "store_error", detail: "Please retry" }, 503);
    }
  } });
  await until(a.offered);
  a.$("create-inbox-btn").click();
  await until(() => a.offered() && /could not create/i.test(a.$("signin-note").textContent));
  assert.ok(a.visible("create-inbox-btn"));
  assert.ok(!a.visible("app"));
  a.$("create-inbox-btn").click();
  await until(() => a.visible("app"));
  assert.equal(a.creates().length, 2);
  assert.equal(a.server.identities.length, 1);
});

for (const failure of ["lost response", "failed listing after creation"]) {
  test(`retry after ${failure} opens the existing inbox without another mint`, async (t) => {
    let fail = true;
    const a = app(t, { intercept: (c, server) => {
      if (!fail || c.path !== "/v1/identities") return;
      if (failure === "lost response" && c.method === "POST") {
        fail = false;
        server.identities.push(identity);
        throw new TypeError("Failed to fetch");
      }
      if (failure === "failed listing after creation" && c.method === "GET" && server.identities.length) {
        fail = false;
        return response({ error: "store_error" }, 503);
      }
    } });
    await until(a.offered);
    a.$("create-inbox-btn").click();
    await until(() => a.offered() && /could not create/i.test(a.$("signin-note").textContent));
    assert.ok(a.visible("create-inbox-btn"));
    a.$("create-inbox-btn").click();
    await until(() => a.visible("app"));
    assert.equal(a.creates().length, 1);
    assert.equal(a.server.identities.length, 1);
  });
}

test("a failed initial listing offers a read-only retry before mailbox creation", async (t) => {
  let fail = true;
  const a = app(t, { intercept: (c) => {
    if (c.path === "/v1/identities" && c.method === "GET" && fail) {
      fail = false;
      return response({ error: "store_error" }, 503);
    }
  } });
  await until(a.offered);
  assert.ok(a.visible("create-inbox-btn"));
  assert.match(a.$("signin-note").textContent, /could not load/i);
  a.$("create-inbox-btn").click();
  await until(() => a.offered() && a.$("create-inbox-btn").textContent === "Create my inbox");
  assert.equal(a.creates().length, 0);
});

test("an expired session during setup returns to sign-in", async (t) => {
  const a = app(t, { intercept: (c) => {
    if (c.path === "/v1/identities" && c.method === "POST") {
      return response({ error: "unauthorized" }, 401);
    }
  } });
  await until(a.offered);
  a.$("create-inbox-btn").click();
  await until(() => a.w.localStorage.getItem("ppi_token") === null);
  assert.ok(a.visible("signin-btn"));
  assert.ok(!a.visible("create-inbox-btn"));
  assert.ok(!a.visible("app"));
});


test("settings has focused pages, restores focus on back and keeps controls usable", async t => {
  const a = app(t, { existing: true });
  await until(() => a.$("me-sub").textContent === identity.address);
  a.$("settings-btn").click();
  assert.ok(!a.visible("acct-handles"));
  assert.ok(!a.visible("size-up"));
  assert.equal(a.w.document.activeElement.id, "settings-nav-account");
  for (const [page, control] of [["account", "acct-address"], ["handles", "acct-handles"], ["inbox", "size-up"], ["contacts", "contact-add"]]) {
    a.$("settings-nav-" + page).click();
    assert.ok(a.visible(control));
    assert.equal(a.w.document.activeElement.id, "settings-title");
    a.w.document.dispatchEvent(new a.w.KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    assert.ok(a.visible("settings-nav-" + page));
    assert.equal(a.w.document.activeElement.id, "settings-nav-" + page);
  }
  a.$("settings-nav-inbox").click();
  a.$("size-up").click();
  assert.equal(a.$("size-value").textContent, "110%");
  a.$("settings-back").click();
  a.$("settings-nav-inbox").click();
  assert.equal(a.$("size-value").textContent, "110%");
  a.$("settings-done").focus();
  a.w.document.dispatchEvent(new a.w.KeyboardEvent("keydown", { key: "Tab", bubbles: true, cancelable: true }));
  assert.equal(a.w.document.activeElement.id, "settings-back");
  a.$("settings-close").click();
  assert.ok(!a.visible("settings-sheet"));
  assert.equal(a.w.document.activeElement.id, "settings-btn");
  a.$("settings-btn").click();
  assert.ok(a.visible("settings-nav-account"));
});

test("copy buttons use complete addresses and never select another mailbox", async (t) => {
  const raw = "/k/" + "b".repeat(128);
  const boxes = [{ address: "/k/main", handle: "/demo/main" }, { address: raw, label: "A friendly label" }];
  const a = app(t, { existing: true, intercept(call) {
    if (call.path === "/v1/identities") return response({ identities: boxes });
    if (call.path === "/v1/whoami") return response(boxes.find((b) => b.address === call.search.get("identity")));
  } });
  const copied = [];
  Object.defineProperty(a.w.navigator, "clipboard", { value: { writeText: async (text) => copied.push(text) } });
  await until(() => a.$("me-sub").textContent === "/demo/main");
  a.$("copy-my-address").click();
  await until(() => a.$("toast").textContent === "Address copied");
  assert.deepEqual(copied, ["/demo/main"]);
  a.$("identity-btn").click();
  const button = [...a.$("identity-menu").querySelectorAll(".copy-address")].find((b) => b.dataset.address === raw);
  button.querySelector("svg").dispatchEvent(new a.w.MouseEvent("click", { bubbles: true }));
  assert.deepEqual(copied, ["/demo/main", raw]);
  assert.equal(a.$("identity-menu").hidden, false);
  assert.equal(a.$("me-sub").textContent, "/demo/main");
  assert.notEqual(a.w.localStorage.getItem("ppi_identity"), raw);
  a.$("settings-btn").click();
  a.$("copy-acct-mailbox").click();
  a.$("copy-acct-address").click();
  assert.deepEqual(copied.slice(-2), ["/demo/main", "/k/main"]);
});

test("unnamed inbox copy stays available while clipboard denial reports failure", async (t) => {
  const a = app(t, { existing: true });
  await until(() => a.$("me-sub").textContent === identity.address);
  assert.equal(a.$("identity-btn").disabled, true);
  assert.equal(a.$("copy-my-address").disabled, false);
  Object.defineProperty(a.w.navigator, "clipboard", { value: { writeText: async () => { throw new Error("denied"); } } });
  a.$("copy-my-address").click();
  await until(() => a.$("toast").textContent.includes("Couldn’t copy"));
  assert.notEqual(a.$("copy-my-address").title, "Address copied");
  a.$("settings-btn").click();
  assert.equal(a.$("copy-acct-mailbox").disabled, true, "Never copy the 'not named' placeholder");
  assert.equal(a.$("copy-acct-address").disabled, false);
});
