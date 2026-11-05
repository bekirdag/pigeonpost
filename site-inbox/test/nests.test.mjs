// Nests Phase B2–B5 on the web: the Products page (site/products.js) and the inbox's Nest status
// header and management dialog (site-inbox/nests.js), against fake postbox and bridge answers.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { JSDOM, VirtualConsole } from "jsdom";

const products = readFileSync(new URL("../../site/products.js", import.meta.url), "utf8");
const nestsJs = readFileSync(new URL("../nests.js", import.meta.url), "utf8");
const tick = async (n = 20) => { for (let i = 0; i < n; i++) await new Promise((r) => setImmediate(r)); };

const PACKAGES = {
  catalog_version: "nests-2026-11",
  packages: [
    { code: "nest.ai.starter.monthly", name: "Pigeonpost AI", slots: 1, storage_bytes: 5 * 2 ** 30, active_hours: 10, input_tokens: 1e6, output_tokens: 1e5, model: "managed", sellable: true },
    { code: "nest.ai.pro.monthly", name: "Pigeonpost AI Pro", slots: 10, storage_bytes: 100 * 2 ** 30, active_hours: 120, input_tokens: 15e6, output_tokens: 15e5, model: "managed", sellable: false },
    { code: "nest.host.starter.monthly", name: "Empty Nest", slots: 1, storage_bytes: 5 * 2 ** 30, active_hours: 10, input_tokens: 0, output_tokens: 0, model: "none", sellable: true },
  ],
};
const PRICES = { plans: [
  { code: "nest-ai-starter-monthly-usd", package: "nest.ai.starter.monthly", currency: "USD", amount: "10.00", available: true },
  { code: "nest-host-starter-monthly-usd", package: "nest.host.starter.monthly", currency: "USD", amount: "5.00", available: true },
  { code: "nest-ai-starter-monthly-eur", package: "nest.ai.starter.monthly", currency: "EUR", amount: "9.00", available: true },
] };

function productsPage(t, { features = ["nests"], nests = { allowance: { slots: 0, used: 0, duplicates: [] }, nests: [], setups: [] }, profile = "00000000-0000-4000-8000-000000000042", prices = PRICES } = {}) {
  const dom = new JSDOM('<div id="root"></div>', { url: "https://pigeonpost.dev/account", runScripts: "outside-only", virtualConsole: new VirtualConsole() });
  t.after(() => dom.window.close());
  const { window } = dom;
  window.eval(products);
  const calls = [], checkouts = [], toasts = [];
  let wentToBilling = false;
  const deps = {
    features: new Set(features),
    pb: async (path, init = {}) => {
      calls.push({ path, method: init.method || "GET", body: init.body });
      if (path === "/v1/nest-packages") return PACKAGES;
      if (path === "/v1/nests") return nests;
      if (path === "/v1/orgs") return { orgs: [] };
      if (path.startsWith("/v1/nest-setups/")) return { setups: [] };
      throw Object.assign(new Error("unexpected " + path), { status: 404 });
    },
    bridgeGet: async (path) => { calls.push({ path, bridge: true }); return { subscriptions: [], prices }; },
    bridgeAction: async () => ({}),
    startCheckout: (p) => checkouts.push(p),
    billingProfileId: async () => profile,
    goBilling: () => { wentToBilling = true; },
    toast: (m) => toasts.push(m),
    randomHex32: () => "ab".repeat(16),
    search: "",
    preferredCurrency: "USD",
    locale: "en",
  };
  window.PPProducts.mount(window.document.querySelector("#root"), deps);
  return { window, doc: window.document, calls, checkouts, toasts, billing: () => wentToBilling };
}

test("three accessible tabs switch by click and by arrow keys", async (t) => {
  const p = productsPage(t);
  await tick();
  const tabs = [...p.doc.querySelectorAll('[role="tab"]')];
  assert.deepEqual(tabs.map((x) => x.textContent), ["Buy an Inbox", "Buy Nests", "Services"]);
  assert.equal(p.doc.querySelector('[role="tab"][aria-selected="true"]').textContent, "Buy Nests");
  p.doc.querySelector("#pp-tab-services").click();
  assert.equal(p.doc.querySelector('[role="tab"][aria-selected="true"]').id, "pp-tab-services");
  assert.equal(p.doc.querySelector('[role="tabpanel"]').getAttribute("aria-labelledby"), "pp-tab-services");
  const list = p.doc.querySelector('[role="tablist"]');
  list.dispatchEvent(new p.window.KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
  assert.equal(p.doc.querySelector('[role="tab"][aria-selected="true"]').id, "pp-tab-inbox", "wraps around");
  assert.equal(p.doc.activeElement.id, "pp-tab-inbox", "focus follows");
  // Services are shown, never sold.
  p.doc.querySelector("#pp-tab-services").click();
  assert.equal(p.doc.querySelectorAll(".pp-service").length, 5);
  assert.equal(p.doc.querySelectorAll("#pp-panel-services button").length, 0);
  assert.match(p.doc.querySelector("#pp-panel-services").textContent, /Coming soon/);
});

test("with the flag off nothing about Nests is fetched or offered", async (t) => {
  const p = productsPage(t, { features: [] });
  await tick();
  assert.ok(p.doc.querySelector("#pp-nests-off"));
  assert.equal(p.doc.querySelectorAll("[data-buy]").length, 0);
  assert.equal(p.calls.length, 0, "no Nest call at all");
});

test("only sellable packages, with the bridge's prices, and checkout carries a server-checked intent", async (t) => {
  const p = productsPage(t);
  await tick();
  const cards = [...p.doc.querySelectorAll(".pp-card")].map((c) => c.getAttribute("data-package"));
  assert.deepEqual(cards, ["nest.ai.starter.monthly", "nest.host.starter.monthly"], "Pro is not sellable");
  assert.match(p.doc.querySelector('[data-package="nest.ai.starter.monthly"] .pp-price').textContent, /10/);
  assert.match(p.doc.querySelector('[data-package="nest.host.starter.monthly"]').textContent, /No AI connected/);
  // Switching currency uses the bridge's EUR price, not a converted one.
  const cur = p.doc.querySelector("#pp-currency");
  cur.value = "EUR"; cur.dispatchEvent(new p.window.Event("change"));
  assert.match(p.doc.querySelector('[data-package="nest.ai.starter.monthly"] .pp-price').textContent, /9/);
  assert.equal(p.doc.querySelector('[data-package="nest.host.starter.monthly"] [data-buy]'), null, "no EUR price, no buy button");
  p.doc.querySelector('[data-buy="nest-ai-starter-monthly-eur"]').click();
  await tick();
  assert.deepEqual(JSON.parse(JSON.stringify(p.checkouts)), [{ kind: "nest", intent_id: "ab".repeat(16), plan_code: "nest-ai-starter-monthly-eur", billing_profile_id: "00000000-0000-4000-8000-000000000042", locale: "en" }]);
});

test("without billing details buying goes to the billing form; without prices nothing is buyable", async (t) => {
  const p = productsPage(t, { profile: null });
  await tick();
  p.doc.querySelector('[data-buy="nest-ai-starter-monthly-usd"]').click();
  await tick();
  assert.equal(p.checkouts.length, 0);
  assert.ok(p.billing());
  const q = productsPage(t, { prices: null });
  await tick();
  assert.equal(q.doc.querySelectorAll("[data-buy]").length, 0);
});

test("truthful states: a store-funded package, a duplicate, and a paid setup that can be retried", async (t) => {
  const p = productsPage(t, { nests: {
    allowance: { slots: 1, used: 0, funding: { source: "apple", package: "nest.ai.starter.monthly", status: "active", paid_through: 1900000000 }, duplicates: [{ source: "google", package: "nest.host.starter.monthly" }] },
    package: { name: "Pigeonpost AI" }, nests: [],
    setups: [{ setup_id: "nsu_0123456789abcdef01234567", label: "Pigeonpost AI", status: "pending" }],
  } });
  await tick();
  assert.match(p.doc.querySelector("#pp-funding").textContent, /App Store/);
  assert.match(p.doc.querySelector("#pp-panel-nests").textContent, /Change or cancel this package in the App Store/);
  assert.equal(p.doc.querySelectorAll("[data-buy]").length, 0, "no second charge offered");
  assert.ok(p.doc.querySelector("#pp-duplicates"));
  assert.match(p.doc.querySelector(".pp-setup").textContent, /Paid — setting up/);
  p.doc.querySelector("[data-retry]").click();
  await tick();
  assert.ok(p.calls.some((c) => c.path === "/v1/nest-setups/nsu_0123456789abcdef01234567/retry" && c.method === "POST"));
});

// ---- the inbox side -------------------------------------------------------------------------------

function inbox(t, { features = ["nests"] } = {}) {
  const dom = new JSDOM('<header id="thread-head"></header><nav id="side-nav" hidden></nav>', { url: "https://inbox.pigeonpost.dev/", runScripts: "outside-only", virtualConsole: new VirtualConsole() });
  t.after(() => dom.window.close());
  const { window } = dom;
  const calls = [], toasts = [];
  const NEST = { nest_id: "nst_0123456789abcdef", address: "/k/nestaddress", label: "Research" };
  let approvals = [{ approval_id: "nap_0123456789abcdef01234567", tool: "send_message", call: { tool: "send_message", args: { to: "/bob", body: "hello bob" } }, args_hash: "a".repeat(64), status: "pending" }];
  window.fetch = async (url, init = {}) => {
    const u = new URL(url);
    calls.push({ path: u.pathname, method: init.method || "GET", headers: init.headers || {}, body: init.body });
    const json = (status, body) => ({ ok: status < 400, status, json: async () => body });
    if (u.pathname === "/v1/nests" && (init.method || "GET") === "GET") return json(200, { nests: [NEST] });
    if (u.pathname === "/v1/orgs") return json(200, { orgs: [] });
    if (u.pathname === `/v1/nests/${NEST.nest_id}`) {
      if (init.method === "DELETE") return json(200, { state: "deleted" });
      return json(200, { ...NEST, status: "needs_you", status_reason: "payment" });
    }
    if (u.pathname.endsWith("/approvals")) return json(200, { approvals });
    if (/\/approvals\/.*\/approve$/.test(u.pathname)) { approvals = []; return json(200, { status: "approved" }); }
    if (u.pathname.endsWith("/files") && init.method === "POST") return json(201, { status: "parsed" });
    if (u.pathname.endsWith("/files")) return json(200, { files: [], used_bytes: 0, quota_bytes: 5 * 2 ** 30 });
    if (u.pathname.endsWith("/usage")) return json(200, { input_tokens: { used: 5, limit: 100 }, output_tokens: { used: 1, limit: 10 }, running_secs: 3600 });
    return json(404, { error: "not_found" });
  };
  window.PigeonpostNestsHost = { postbox: () => "https://postbox.example", token: () => "member", renew: async () => false, features: () => new Set(features), toast: (m) => toasts.push(m), pollMs: 10 ** 9 };
  window.eval(nestsJs);
  return { window, doc: window.document, calls, toasts, NEST };
}

test("a conversation with a Nest gets a status header; any other conversation does not", async (t) => {
  const p = inbox(t);
  await p.window.PigeonpostNests.onMailbox();
  assert.ok(p.doc.querySelector("#products-link"), "Products is reachable from the inbox");
  p.window.PigeonpostNests.renderHead("/k/someone-else");
  assert.equal(p.doc.querySelector("#nest-status"), null);
  p.window.PigeonpostNests.renderHead(p.NEST.address);
  await tick();
  assert.equal(p.doc.querySelector("#nest-status").getAttribute("role"), "status");
  assert.equal(p.doc.querySelector("#nest-status-text").textContent, "Needs you");
  assert.match(p.doc.querySelector("#nest-status-note").textContent, /paid for/);
  p.window.PigeonpostNests.renderHead(null);
  assert.equal(p.doc.querySelector("#nest-status"), null);
});

test("the flag off hides everything", async (t) => {
  const p = inbox(t, { features: [] });
  await p.window.PigeonpostNests.onMailbox();
  p.window.PigeonpostNests.renderHead(p.NEST.address);
  assert.equal(p.doc.querySelector("#nest-status"), null);
  assert.equal(p.doc.querySelector("#products-link"), null);
  assert.equal(p.calls.length, 0);
});

test("approving posts the hash that was shown; files carry an encoded name; delete needs the name", async (t) => {
  const p = inbox(t);
  await p.window.PigeonpostNests.onMailbox();
  p.window.PigeonpostNests.renderHead(p.NEST.address);
  await tick();
  p.doc.querySelector("#nest-manage-btn").click();
  await tick();
  assert.equal(p.doc.querySelector("#nest-dialog").getAttribute("role"), "dialog");
  assert.match(p.doc.querySelector(".nest-card").textContent, /Send a message to \/bob/);
  assert.match(p.doc.querySelector(".nest-call").textContent, /hello bob/);
  p.doc.querySelector("[data-approve]").click();
  await tick();
  const approve = p.calls.find((c) => /\/approve$/.test(c.path));
  assert.deepEqual(JSON.parse(approve.body), { args_hash: "a".repeat(64) });
  // Upload: the raw file, its name percent-encoded in the header.
  const file = new p.window.File(["budget"], "Q3 plan é.txt", { type: "text/plain" });
  const input = p.doc.querySelector("#nest-file-input");
  Object.defineProperty(input, "files", { value: [file] });
  p.doc.querySelector("#nest-upload").click();
  await tick();
  const up = p.calls.find((c) => c.method === "POST" && c.path.endsWith("/files"));
  assert.equal(up.headers["x-file-name"], "Q3%20plan%20%C3%A9.txt");
  assert.equal(p.window.PigeonpostNests.encodeName("../a b"), "..%2Fa%20b");
  // Delete: nothing is sent until the name is typed exactly.
  p.doc.querySelector("#nest-delete-confirm").value = "research";
  p.doc.querySelector("#nest-delete").click();
  await tick();
  assert.equal(p.calls.filter((c) => c.method === "DELETE").length, 0);
  p.doc.querySelector("#nest-delete-confirm").value = "Research";
  p.doc.querySelector("#nest-delete").click();
  await tick();
  const del = p.calls.find((c) => c.method === "DELETE");
  assert.deepEqual(JSON.parse(del.body), { confirm: "Research" });
});
