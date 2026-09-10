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
    const call = { path: parsed.pathname, method: opts.method || "GET", opts };
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
