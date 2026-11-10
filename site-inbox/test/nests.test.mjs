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

function productsPage(t, { features = ["nests"], nests = { allowance: { slots: 0, used: 0, duplicates: [] }, nests: [], setups: [] }, profile = "00000000-0000-4000-8000-000000000042", prices = PRICES, orgs = [], orgNests = null, members = null, bridgeFor = null, search = "", operators = null } = {}) {
  const dom = new JSDOM('<div id="root"></div>', { url: "https://pigeonpost.dev/account", runScripts: "outside-only", virtualConsole: new VirtualConsole() });
  t.after(() => dom.window.close());
  const { window } = dom;
  window.eval(products);
  const calls = [], checkouts = [], toasts = [], cleared = [];
  let wentToBilling = false;
  const deps = {
    features: new Set(features),
    pb: async (path, init = {}) => {
      calls.push({ path, method: init.method || "GET", body: init.body });
      if (path === "/v1/nest-packages") return PACKAGES;
      if (path === "/v1/nests") return nests;
      if (path === "/v1/orgs") return { orgs };
      if (orgNests && /^\/v1\/orgs\/[^/]+\/nests$/.test(path)) return orgNests;
      if (operators && /^\/v1\/orgs\/[^/]+\/nests\/[^/]+\/operators$/.test(path)) return operators(path, init);
      if (/^\/v1\/orgs\/[^/]+\/members\?status=active$/.test(path)) {
        if (!members) throw Object.assign(new Error("forbidden"), { status: 403, code: "forbidden" });
        return { members };
      }
      if (/^\/v1\/nests\/nst_/.test(path)) return { status: "working", status_reason: null };
      if (path.startsWith("/v1/nest-setups/")) return { setups: [] };
      throw Object.assign(new Error("unexpected " + path), { status: 404 });
    },
    bridgeGet: async (path) => {
      calls.push({ path, bridge: true });
      const answer = bridgeFor ? await bridgeFor(path) : undefined;
      return answer !== undefined ? answer : { subscriptions: [], prices };
    },
    bridgeAction: async (path, body) => { calls.push({ path, bridge: true, method: "POST", body }); return {}; },
    startCheckout: (p) => checkouts.push(p),
    billingProfileId: async () => profile,
    goBilling: () => { wentToBilling = true; },
    toast: (m) => toasts.push(m),
    randomHex32: () => "ab".repeat(16),
    search,
    clearReturn: (key) => cleared.push(key),
    confirmDelayMs: 0,
    preferredCurrency: "USD",
    locale: "en",
  };
  window.PPProducts.mount(window.document.querySelector("#root"), deps);
  return { window, doc: window.document, calls, checkouts, toasts, cleared, billing: () => wentToBilling };
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

function inbox(t, { features = ["nests"], detail = {}, approve = null, hash = "" } = {}) {
  const dom = new JSDOM('<header id="thread-head"></header><nav id="side-nav" hidden></nav>', { url: "https://inbox.pigeonpost.dev/" + hash, runScripts: "outside-only", virtualConsole: new VirtualConsole() });
  t.after(() => dom.window.close());
  const { window } = dom;
  const calls = [], toasts = [], opened = [];
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
      if (init.method === "PATCH") return json(200, { nest_id: NEST.nest_id, label: JSON.parse(init.body).label });
      return json(200, { ...NEST, role: "owner", state: "paused", paused_reason: "unfunded", status: "needs_you", status_reason: "payment", delete_after: 1900000000, ...detail });
    }
    if (u.pathname.endsWith("/approvals")) return json(200, { approvals });
    if (/\/approvals\/.*\/approve$/.test(u.pathname)) { if (approve) return approve(json); approvals = []; return json(200, { status: "approved" }); }
    if (u.pathname.endsWith("/pause") || u.pathname.endsWith("/resume")) return json(200, { nest_id: NEST.nest_id, state: "running" });
    if (u.pathname.endsWith("/files") && init.method === "POST") return json(201, { status: "parsed" });
    if (u.pathname.endsWith("/files")) return json(200, { files: [], used_bytes: 0, quota_bytes: 5 * 2 ** 30 });
    if (u.pathname.endsWith("/export")) return { ok: true, status: 200, blob: async () => new window.Blob(["PK"]) };
    if (u.pathname.endsWith("/usage")) return json(200, { input_tokens: { used: 5, limit: 100 }, output_tokens: { used: 1, limit: 10 }, running_secs: 3600, requests: 7, files_bytes: 3 * 2 ** 20 });
    return json(404, { error: "not_found" });
  };
  window.PigeonpostNestsHost = { postbox: () => "https://postbox.example", token: () => "member", renew: async () => false, features: () => new Set(features), toast: (m) => toasts.push(m), pollMs: 10 ** 9, openConversation: (peer) => opened.push(peer) };
  window.eval(nestsJs);
  return { window, doc: window.document, calls, toasts, opened, NEST, setApprovals: (a) => { approvals = a; } };
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

// ---- role, state and races ------------------------------------------------------------------------

const openDialog = async (p) => {
  await p.window.PigeonpostNests.onMailbox();
  p.window.PigeonpostNests.renderHead(p.NEST.address);
  await tick();
  p.doc.querySelector("#nest-manage-btn").click();
  await tick();
};
const RUNNING = { state: "running", paused_reason: null, status: "ready", status_reason: null, delete_after: null };

test("an operator sees files and approvals but no Nest controls", async (t) => {
  const p = inbox(t, { detail: { ...RUNNING, role: "operator" } });
  await openDialog(p);
  assert.ok(p.doc.querySelector(".nest-card"));
  assert.ok(p.doc.querySelector("#nest-file-input"));
  for (const id of ["nest-pause", "nest-resume", "nest-export", "nest-rename", "nest-delete"]) assert.equal(p.doc.querySelector("#" + id), null, id);
});

test("a Nest held for payment offers no Resume, links to packages and warns of deletion", async (t) => {
  const p = inbox(t);
  await openDialog(p);
  assert.equal(p.doc.querySelector("#nest-resume"), null);
  assert.match(p.doc.querySelector("#nest-held").textContent, /paid for/);
  assert.match(p.doc.querySelector("#nest-status-note").textContent, /deleted on/i);
  assert.equal(p.doc.querySelector("#nest-status-pay").hidden, false);
  assert.ok(p.doc.querySelector("#nest-export"), "the export window stays open");
});

test("a Nest paused by its owner offers Resume, not Pause; a running one the reverse", async (t) => {
  const p = inbox(t, { detail: { ...RUNNING, state: "paused", paused_reason: "owner", status: "paused" } });
  await openDialog(p);
  assert.equal(p.doc.querySelector("#nest-pause"), null);
  p.doc.querySelector("#nest-resume").click();
  await tick();
  assert.ok(p.calls.some((c) => c.method === "POST" && c.path.endsWith("/resume")));
  const q = inbox(t, { detail: RUNNING });
  await openDialog(q);
  assert.equal(q.doc.querySelector("#nest-resume"), null);
  assert.ok(q.doc.querySelector("#nest-pause"));
  assert.equal(q.doc.querySelector("#nest-status-pay").hidden, true);
});

test("the header counts actions to approve and opens them", async (t) => {
  const p = inbox(t, { detail: RUNNING });
  await p.window.PigeonpostNests.onMailbox();
  p.window.PigeonpostNests.renderHead(p.NEST.address);
  await tick();
  const btn = p.doc.querySelector("#nest-approvals-btn");
  assert.equal(btn.hidden, false);
  assert.equal(btn.textContent, "1 action to approve");
  btn.click();
  await tick();
  assert.equal(p.doc.querySelector("#nest-dialog").hidden, false);
  assert.ok(p.doc.querySelector(".nest-card"));
});

test("redrawing the same thread does not poll again, and one click loads the dialog once", async (t) => {
  const p = inbox(t, { detail: RUNNING });
  await p.window.PigeonpostNests.onMailbox();
  const statusGets = () => p.calls.filter((c) => c.path === `/v1/nests/${p.NEST.nest_id}` && c.method === "GET").length;
  p.window.PigeonpostNests.renderHead(p.NEST.address);
  p.window.PigeonpostNests.renderHead(p.NEST.address);
  p.window.PigeonpostNests.renderHead(p.NEST.address);
  await tick();
  assert.equal(statusGets(), 1);
  const before = p.calls.filter((c) => c.path.endsWith("/files")).length;
  p.doc.querySelector("#nest-manage-btn").click();
  await tick();
  assert.equal(p.calls.filter((c) => c.path.endsWith("/files")).length - before, 1);
});

test("overlapping loads draw each section once, and a closed dialog stays empty", async (t) => {
  const p = inbox(t, { detail: RUNNING });
  await p.window.PigeonpostNests.onMailbox();
  p.window.PigeonpostNests.renderHead(p.NEST.address);
  await tick();
  p.doc.querySelector("#nest-manage-btn").click();
  p.doc.querySelector("#nest-manage-btn").click();
  await tick();
  assert.equal(p.doc.querySelectorAll(".nest-section").length, 4);
  p.doc.querySelector("#nest-manage-btn").click();
  p.doc.querySelector("#nest-close").click();
  await tick();
  assert.equal(p.doc.querySelector("#nest-body").textContent, "");
});

test("a double click approves once; a changed action reloads with an explanation", async (t) => {
  const p = inbox(t, { detail: RUNNING });
  await openDialog(p);
  const button = p.doc.querySelector("[data-approve]");
  button.click();
  button.click();
  await tick();
  assert.equal(p.calls.filter((c) => /\/approve$/.test(c.path)).length, 1);
  const q = inbox(t, { detail: RUNNING, approve: (json) => json(409, { error: "approval_changed", detail: "x" }) });
  await openDialog(q);
  q.doc.querySelector("[data-approve]").click();
  await tick();
  assert.match(q.toasts.join(), /changed since it was shown/);
  assert.ok(q.doc.querySelector(".nest-card"), "the current version is shown again");
});

test("a card's text is shown verbatim, never as markup", async (t) => {
  const p = inbox(t, { detail: RUNNING });
  const hostile = '<img src=x onerror="alert(1)">';
  p.setApprovals([{ approval_id: "nap_1", tool: "send_message", call: { args: { to: "/bob", body: hostile } }, args_hash: "b".repeat(64), status: "pending" }]);
  await openDialog(p);
  assert.equal(p.doc.querySelector(".nest-call").textContent, hostile);
  assert.equal(p.doc.querySelector(".nest-call img"), null);
});

test("an oversized or empty file is refused before any upload", async (t) => {
  const p = inbox(t, { detail: RUNNING });
  await openDialog(p);
  const choose = (file) => { Object.defineProperty(p.doc.querySelector("#nest-file-input"), "files", { value: [file], configurable: true }); p.doc.querySelector("#nest-upload").click(); };
  const big = new p.window.File(["x"], "big.bin");
  Object.defineProperty(big, "size", { value: 25 * 1024 * 1024 + 1 });
  choose(big);
  await tick();
  choose(new p.window.File([], "empty.txt"));
  await tick();
  assert.equal(p.calls.filter((c) => c.method === "POST" && c.path.endsWith("/files")).length, 0);
  assert.match(p.toasts.join(), /at most 25 MB/);
  assert.match(p.toasts.join(), /empty/);
});

test("usage shows the ledger's numbers and storage is not rounded up", async (t) => {
  const p = inbox(t, { detail: RUNNING });
  await openDialog(p);
  const usage = p.doc.querySelector("#nest-usage").textContent;
  assert.match(usage, /5 of 100 input and 1 of 10 output/);
  assert.match(usage, /Requests: 7/);
  assert.match(usage, /Running: 1\.0 hours/);
  assert.match(usage, /Files stored: 3 MB/);
  assert.equal(p.doc.querySelector("#nest-files-used").textContent, "0 KB of 5 GB used");
});

test("export downloads the zip under the Nest's name", async (t) => {
  const p = inbox(t, { detail: RUNNING });
  await openDialog(p);
  let saved = null;
  p.window.URL.createObjectURL = () => "blob:fake";
  p.window.URL.revokeObjectURL = () => {};
  p.window.HTMLAnchorElement.prototype.click = function () { saved = this.getAttribute("download"); };
  p.doc.querySelector("#nest-export").click();
  await tick();
  assert.equal(saved, "nest-Research.zip");
});

test("#peer= opens only one of the account's own Nests", async (t) => {
  const p = inbox(t, { detail: RUNNING, hash: "#peer=" + encodeURIComponent("/k/nestaddress") });
  await p.window.PigeonpostNests.onMailbox();
  assert.deepEqual(p.opened, ["/k/nestaddress"]);
  assert.equal(p.window.location.hash, "");
  const q = inbox(t, { detail: RUNNING, hash: "#peer=" + encodeURIComponent("/k/stranger") });
  await q.window.PigeonpostNests.onMailbox();
  assert.deepEqual(q.opened, []);
});

// ---- Products: adding Nests, company Nests ----------------------------------------------------------

const FUNDED = (used) => ({ allowance: { slots: 3, used, funding: { source: "iyzico", package: "nest.ai.plus.monthly", status: "active" }, duplicates: [] }, package: { name: "Pigeonpost AI Plus" }, nests: [], setups: [] });

test("a free slot can be filled; a full package offers no form", async (t) => {
  const p = productsPage(t, { nests: FUNDED(1) });
  await tick();
  p.doc.querySelector("#pp-nest-label").value = " Second ";
  p.doc.querySelector("#pp-nest-create").dispatchEvent(new p.window.Event("submit", { cancelable: true }));
  await tick();
  const post = p.calls.find((c) => c.path === "/v1/nests" && c.method === "POST");
  assert.deepEqual(JSON.parse(post.body), { label: "Second" });
  const q = productsPage(t, { nests: FUNDED(3) });
  await tick();
  assert.equal(q.doc.querySelector("#pp-nest-create"), null);
});

test("a company admin creates a Nest with operators; rows carry the live status", async (t) => {
  const p = productsPage(t, {
    orgs: [{ id: "org1", name: "Acme", my_roles: ["agent_admin"], my_member_id: "m_me" }],
    orgNests: { allowance: { slots: 3, used: 1 }, package: { code: "x", slots: 3 }, nests: [{ nest_id: "nst_a", address: "/k/a", label: "Ops", state: "running", role: "owner" }] },
    members: [{ id: "m_me", first_name: "Me" }, { id: "m_2", first_name: "Ada", last_name: "L" }],
  });
  await tick();
  const ctx = p.doc.querySelector("#pp-context");
  ctx.value = "org1"; ctx.dispatchEvent(new p.window.Event("change"));
  await tick();
  assert.equal(p.doc.querySelector('[data-nest="nst_a"] .pp-status').textContent, "Working…");
  assert.equal(p.doc.querySelectorAll("[data-operator]").length, 1, "the admin isn't offered to themselves");
  assert.match(p.doc.querySelector(".pp-operators").textContent, /Ada L/);
  p.doc.querySelector('[data-operator="m_2"]').checked = true;
  p.doc.querySelector("#pp-org-handle").value = "ops";
  p.doc.querySelector("#pp-org-label").value = "Ops two";
  p.doc.querySelector("#pp-org-create").dispatchEvent(new p.window.Event("submit", { cancelable: true }));
  await tick();
  const post = p.calls.find((c) => c.path === "/v1/orgs/org1/nests" && c.method === "POST");
  assert.deepEqual(JSON.parse(post.body), { handle: "ops", label: "Ops two", operators: ["m_2"] });
});

test("a company operator sees its Nests but no create form; no member list means no picker", async (t) => {
  const org = (roles) => ({ orgs: [{ id: "org1", name: "Acme", my_roles: roles, my_member_id: "m_me" }], orgNests: { allowance: { slots: 1, used: 1 }, package: null, nests: [] } });
  const p = productsPage(t, org(["member"]));
  await tick();
  let ctx = p.doc.querySelector("#pp-context");
  ctx.value = "org1"; ctx.dispatchEvent(new p.window.Event("change"));
  await tick();
  assert.equal(p.doc.querySelector("#pp-org-create"), null);
  assert.ok(!p.calls.some((c) => /members/.test(c.path)));
  const q = productsPage(t, org(["agent_admin"]));
  await tick();
  ctx = q.doc.querySelector("#pp-context");
  ctx.value = "org1"; ctx.dispatchEvent(new q.window.Event("change"));
  await tick();
  assert.ok(q.doc.querySelector("#pp-org-create"));
  assert.equal(q.doc.querySelector(".pp-operators"), null);
});

// ---- buying a company package ---------------------------------------------------------------------

const ACME = "org_acme0123";
const NO_PACKAGE = { allowance: { slots: 0, used: 0 }, package: null, nests: [] };
const company = async (p) => {
  await tick();
  const ctx = p.doc.querySelector("#pp-context");
  ctx.value = ACME; ctx.dispatchEvent(new p.window.Event("change"));
  await tick();
};
const orgBridge = (subscriptions = []) => (path) => (path === `/v1/nests?org_id=${ACME}` ? { org_id: ACME, subscriptions, prices: PRICES } : undefined);

test("a company's billing admin buys a package in company context", async (t) => {
  const p = productsPage(t, { orgs: [{ id: ACME, name: "Acme", my_roles: ["billing_admin"], my_member_id: "m_me" }], orgNests: NO_PACKAGE, bridgeFor: orgBridge() });
  await company(p);
  assert.ok(p.calls.some((c) => c.bridge && c.path === `/v1/nests?org_id=${ACME}`));
  assert.match(p.doc.querySelector("#pp-org").textContent, /no Nest package yet\. Buy one below\./);
  assert.deepEqual([...p.doc.querySelectorAll("#pp-org .pp-card")].map((c) => c.getAttribute("data-package")), ["nest.ai.starter.monthly", "nest.host.starter.monthly"]);
  assert.equal(p.doc.querySelector("#pp-org-create"), null, "a billing admin alone does not create Nests");
  p.doc.querySelector('#pp-org [data-buy="nest-host-starter-monthly-usd"]').click();
  await tick();
  assert.deepEqual(JSON.parse(JSON.stringify(p.checkouts)), [{ kind: "nest", intent_id: "ab".repeat(16), plan_code: "nest-host-starter-monthly-usd",
    billing_profile_id: "00000000-0000-4000-8000-000000000042", locale: "en", org_id: ACME }]);
});

test("members who don't manage billing keep the message and see no prices", async (t) => {
  for (const roles of [["member"], ["agent_admin"], ["admin"]]) {
    const p = productsPage(t, { orgs: [{ id: ACME, name: "Acme", my_roles: roles, my_member_id: "m_me" }], orgNests: NO_PACKAGE, bridgeFor: orgBridge() });
    await company(p);
    assert.match(p.doc.querySelector("#pp-org").textContent, /Its billing admin can buy one\./, roles.join());
    assert.equal(p.doc.querySelectorAll("#pp-org [data-buy]").length, 0, roles.join());
    assert.ok(!p.calls.some((c) => c.bridge && /org_id=/.test(c.path)), roles.join());
  }
});

test("the company's web package can be changed or canceled by its buyer, and only seen by another admin", async (t) => {
  const SUB = { id: "00000000-0000-4000-8000-000000000077", plan_code: "nest-ai-starter-monthly-usd", currency: "USD", package: "nest.ai.starter.monthly", slots: 1, provider_status: "ACTIVE", recurring_price: { amount: "10.00", currency: "USD" }, buyer: true };
  const funded = { allowance: { slots: 1, used: 0 }, package: { code: "nest.ai.starter.monthly", slots: 1 }, nests: [] };
  const p = productsPage(t, { orgs: [{ id: ACME, name: "Acme", my_roles: ["owner"], my_member_id: "m_me" }], orgNests: funded, bridgeFor: orgBridge([SUB]) });
  await company(p);
  assert.match(p.doc.querySelector("#pp-org .pp-sub").textContent, /Company web Nest package · USD/);
  assert.equal(p.doc.querySelectorAll("#pp-org [data-buy]").length, 0, "no second package");
  p.doc.querySelector("#pp-cancel").click();
  await tick();
  assert.ok(p.calls.some((c) => c.method === "POST" && c.path === `/v1/nests/subscriptions/${SUB.id}/cancel`));
  const q = productsPage(t, { orgs: [{ id: ACME, name: "Acme", my_roles: ["billing_admin"], my_member_id: "m_me" }], orgNests: funded, bridgeFor: orgBridge([{ ...SUB, buyer: false, provider_status: null }]) });
  await company(q);
  assert.ok(q.doc.querySelector("#pp-not-buyer"));
  assert.equal(q.doc.querySelector("#pp-cancel"), null);
  assert.equal(q.doc.querySelector("#pp-change-go"), null);
  // Funded another way (a grant or a store): nothing to buy or manage here.
  const r = productsPage(t, { orgs: [{ id: ACME, name: "Acme", my_roles: ["owner"], my_member_id: "m_me" }], orgNests: funded, bridgeFor: orgBridge([]) });
  await company(r);
  assert.equal(r.doc.querySelectorAll("#pp-org [data-buy]").length, 0);
  assert.equal(r.doc.querySelector("#pp-org .pp-sub"), null);
});

test("back from a company checkout the page opens in that company and confirms it", async (t) => {
  const id = "cd".repeat(16);
  const p = productsPage(t, {
    orgs: [{ id: ACME, name: "Acme", my_roles: ["billing_admin"], my_member_id: "m_me" }], orgNests: NO_PACKAGE,
    search: `?nest_checkout=${id}&nest_org=${ACME}`,
    bridgeFor: (path) => (path === `/v1/nests/checkout/${id}` ? { status: "active", org_id: ACME } : orgBridge()(path)),
  });
  await tick(40);
  assert.equal(p.doc.querySelector("#pp-context").value, ACME);
  assert.ok(p.doc.querySelector("#pp-org"));
  assert.deepEqual(p.cleared.sort(), ["nest_checkout", "nest_org"]);
  assert.ok(p.toasts.some((m) => /company's Nest package is active/.test(m)), JSON.stringify(p.toasts));
  // A malformed org in the return is ignored.
  const q = productsPage(t, { orgs: [{ id: ACME, name: "Acme", my_roles: ["owner"] }], orgNests: NO_PACKAGE, search: "?nest_org=../x" });
  await tick();
  assert.equal(q.doc.querySelector("#pp-context").value, "personal");
});

test("a Nest awaiting payment says when it will be deleted", async (t) => {
  const p = productsPage(t, { nests: { ...FUNDED(1), nests: [{ nest_id: "nst_a", label: "A", status: "needs_you", status_reason: "payment", delete_after: 1900000000 }] } });
  await tick();
  assert.match(p.doc.querySelector('[data-nest="nst_a"] .pp-warn').textContent, /Deleted on/);
});

test("a company admin manages a Nest's operators", async (t) => {
  const p = productsPage(t, {
    orgs: [{ id: "org1", name: "Acme", my_roles: ["agent_admin"], my_member_id: "m_me" }],
    orgNests: { allowance: { slots: 3, used: 1 }, package: { code: "x", slots: 3 }, nests: [{ nest_id: "nst_a", address: "/k/a", label: "Ops", state: "running", role: "owner" }] },
    members: [
      { id: "m_me", first_name: "Me" },
      { id: "m_2", first_name: "Ada", last_name: "L" },
      { id: "m_3", first_name: "Bo" },
      { id: "m_4", first_name: "Cy", roles: ["admin"] }
    ],
    operators: (path, init) => {
      if (init.method === "PUT") {
        return { operators: JSON.parse(init.body).operators.map(id => ({ member_id: id })) };
      }
      return { operators: [{ member_id: "m_3", first_name: "Bo" }] };
    }
  });

  await tick();
  const ctx = p.doc.querySelector("#pp-context");
  ctx.value = "org1";
  ctx.dispatchEvent(new p.window.Event("change"));
  await tick();

  const toggle = p.doc.querySelector('[data-manage-operators="nst_a"]');
  assert.ok(toggle, "toggle button exists");
  assert.equal(toggle.getAttribute("aria-expanded"), "false");

  const panel = p.doc.querySelector("#pp-ops-nst_a");
  assert.ok(panel, "panel exists");
  assert.ok(panel.hidden, "panel is hidden initially");

  toggle.click();
  await tick();

  assert.equal(toggle.getAttribute("aria-expanded"), "true");
  assert.ok(!panel.hidden, "panel is visible after click");

  const checkboxes = panel.querySelectorAll("[data-nest-operator]");
  assert.equal(checkboxes.length, 2, "exactly two checkboxes offered (m_2 and m_3)");
  
  const ids = Array.from(checkboxes).map(cb => cb.getAttribute("data-nest-operator")).sort();
  assert.deepEqual(ids, ["m_2", "m_3"], "checkboxes are for m_2 and m_3");

  const cbM3 = panel.querySelector('[data-nest-operator="m_3"]');
  const cbM2 = panel.querySelector('[data-nest-operator="m_2"]');
  
  assert.ok(cbM3.checked, "m_3 is checked");
  assert.ok(!cbM2.checked, "m_2 is not checked");
  for (const cb of checkboxes) assert.match(panel.querySelector(`label[for="${cb.id}"]`).textContent, /\S/, "every checkbox is labelled");

  const fieldset = panel.querySelector("fieldset.pp-operators");
  assert.ok(fieldset, "fieldset exists");
  const legend = fieldset.querySelector("legend");
  assert.ok(legend.textContent.includes("Ops"), "legend contains 'Ops'");

  cbM2.checked = true;
  cbM3.checked = false;

  const form = panel.querySelector("form");
  assert.ok(form, "form exists in panel");
  form.dispatchEvent(new p.window.Event("submit", { cancelable: true }));
  
  await tick();

  const putCall = p.calls.find(c => c.path === "/v1/orgs/org1/nests/nst_a/operators" && c.method === "PUT");
  assert.ok(putCall, "PUT call was made");
  assert.deepEqual(JSON.parse(putCall.body), { operators: ["m_2"] });

  const status = panel.querySelector('[role="status"]');
  assert.ok(status, "status element exists");
  assert.match(status.textContent, /Saved\. 1 person/);
});

test("without the member list an admin can only remove operators; errors are announced", async (t) => {
  const p = productsPage(t, {
    orgs: [{ id: "org1", name: "Acme", my_roles: ["agent_admin"], my_member_id: "m_me" }],
    orgNests: { allowance: { slots: 3, used: 1 }, package: { code: "x", slots: 3 }, nests: [{ nest_id: "nst_a", address: "/k/a", label: "Ops", state: "running", role: "owner" }] },
    members: null, // member list fetch fails
    operators: (path, init) => {
      if (init.method === "PUT") {
        throw Object.assign(new Error("x"), { status: 409, code: "org_read_only" });
      }
      return { operators: [{ member_id: "m_3", first_name: "Bo" }] };
    }
  });

  await tick();
  const ctx = p.doc.querySelector("#pp-context");
  ctx.value = "org1";
  ctx.dispatchEvent(new p.window.Event("change"));
  await tick();

  const toggle = p.doc.querySelector('[data-manage-operators="nst_a"]');
  assert.ok(toggle, "toggle button exists");
  
  toggle.click();
  await tick();

  const panel = p.doc.querySelector("#pp-ops-nst_a");
  assert.ok(panel, "panel exists");
  assert.ok(!panel.hidden, "panel is visible after click");

  const checkboxes = panel.querySelectorAll("[data-nest-operator]");
  assert.equal(checkboxes.length, 1, "only one checkbox offered (m_3)");
  assert.equal(checkboxes[0].getAttribute("data-nest-operator"), "m_3");

  assert.match(panel.textContent, /remove operators here but not add/);

  const form = panel.querySelector("form");
  assert.ok(form, "form exists in panel");
  form.dispatchEvent(new p.window.Event("submit", { cancelable: true }));
  
  await tick();

  const status = panel.querySelector('[role="status"]');
  assert.ok(status, "status element exists");
  assert.match(status.textContent, /read-only/);
});
