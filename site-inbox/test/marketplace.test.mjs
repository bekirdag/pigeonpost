// Marketplace on the web: the Services catalog (site/marketplace-catalog.js through products.js),
// the publisher console (site/marketplace-publisher.js), the staff dashboard
// (site/marketplace-staff.js) and how the account page mounts them, against fake postbox answers.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { JSDOM, VirtualConsole } from "jsdom";

const read = (p) => readFileSync(new URL(p, import.meta.url), "utf8");
const core = read("../../site/marketplace-core.js");
const catalogJs = read("../../site/marketplace-catalog.js");
const publisherJs = read("../../site/marketplace-publisher.js");
const staffJs = read("../../site/marketplace-staff.js");
const products = read("../../site/products.js");
const accountJs = read("../../site/account.js");
const accountConfig = read("../../site/account-config.js");
const tick = async (n = 25) => { for (let i = 0; i < n; i++) await new Promise((r) => setImmediate(r)); };
const fail = (status, code, body = {}) => Object.assign(new Error(code), { status, code, body: { error: code, ...body } });

const SCHEMA = {
  type: "object", additionalProperties: false,
  properties: {
    tone: { type: "string", enum: ["short", "long"], title: "Tone" },
    limit: { type: "integer", minimum: 1, maximum: 10 },
    api_key: { type: "string", maxLength: 200, writeOnly: true, title: "API key" },
  },
  required: [],
};
const LISTING = { id: "lst_a", title: "HR helper", summary: "Answers HR questions", publisher: { display_name: "Acme", first_party: false }, version: "1.0.0", visibility: "public", status: "published" };
const DETAIL = { ...LISTING, description: "Long text.", operations: [{ name: "hr.ask", version: "1.0", fields: [{ name: "question", required: true }] }], config_schema: SCHEMA, price: { amount_minor: 0, funding: "test_credits" } };
const INSTALL = {
  id: "ins_1", listing_id: "lst_a", status: "active", version: "1.0.0", security_state: "normal", listing_status: "published",
  configuration: { tone: "long" }, config_schema: SCHEMA, secrets: [{ name: "api_key", set: true }], funding: "test_credits",
  update_available: null, previous_release_id: null,
};

/// A window with every marketplace script loaded. `routes(method, path, body)` answers deps.pb.
function page(t, routes) {
  const dom = new JSDOM('<div id="root"></div>', { url: "https://pigeonpost.dev/account", runScripts: "outside-only", virtualConsole: new VirtualConsole() });
  t.after(() => dom.window.close());
  const { window } = dom;
  for (const s of [core, catalogJs, publisherJs, staffJs, products]) window.eval(s);
  const calls = [], stepUps = [];
  const pb = async (path, init = {}) => {
    const method = init.method || "GET";
    const body = init.body ? JSON.parse(init.body) : undefined;
    calls.push({ path, method, body });
    const out = await routes(method, path, body);
    if (out === undefined) throw fail(404, "not_found");
    return out;
  };
  const deps = { pb, toast: () => {}, stepUp: (challenge, section) => stepUps.push({ challenge, section }) };
  const doc = window.document;
  const $ = (s) => doc.querySelector(s);
  const submit = (form) => form.dispatchEvent(new window.Event("submit", { bubbles: true, cancelable: true }));
  const type = (input, value) => { input.value = value; input.dispatchEvent(new window.Event("input", { bubbles: true })); };
  const choose = (input) => { input.checked = true; input.dispatchEvent(new window.Event("change", { bubbles: true })); };
  return { window, doc, $, calls, stepUps, deps, submit, type, choose, root: $("#root") };
}

// ---- Services catalog -------------------------------------------------------------------------

function catalogPage(t, { features = ["nests", "marketplace"], installs = [], detail = DETAIL, listings = [LISTING], extra, deps: more = {} } = {}) {
  const p = page(t, (method, path, body) => {
    if (extra) { const r = extra(method, path, body); if (r !== undefined) return r; }
    if (method === "GET" && path.startsWith("/v1/marketplace/listings?") || path === "/v1/marketplace/listings") return { data: listings, next_cursor: null };
    if (method === "GET" && path === "/v1/marketplace/listings/lst_a") return { data: detail };
    if (method === "GET" && path === "/v1/nest-installations") return { data: installs };
    if (method === "POST" && path === "/v1/nest-installations") { installs.push({ ...INSTALL, secrets: [{ name: "api_key", set: Boolean(body.configuration?.api_key) }] }); return { data: installs[0] }; }
    if (/^\/v1\/nest-installations\/ins_1/.test(path)) return { data: {} };
    return undefined;
  });
  p.window.PPProducts.mount(p.root, {
    ...p.deps, features: new Set(features),
    bridgeGet: async () => ({ subscriptions: [], prices: { plans: [] } }), bridgeAction: async () => ({}),
    startCheckout: () => {}, billingProfileId: async () => null, randomHex32: () => "ab".repeat(16), search: "", locale: "en",
    ...more,
  });
  return p;
}

async function openServices(p) {
  await tick();
  p.$("#pp-tab-services").click();
  await tick();
}

async function openDetail(p) {
  await openServices(p);
  p.$('button[aria-label="Details for HR helper"]').click();
  await tick();
}

test("the Services tab lists marketplace listings with a publisher badge and a Marketplace filter", async (t) => {
  const p = catalogPage(t, { listings: [LISTING, { ...LISTING, id: "lst_p", title: "Policy Q&A", publisher: { display_name: "Pigeonpost", first_party: true } }] });
  await openServices(p);
  const legend = p.$("#mp-catalog fieldset.mp-filter legend");
  assert.equal(legend.textContent, "Show");
  assert.equal(p.$('[data-listing="lst_a"] .mp-badge').textContent, "Publisher: Acme");
  assert.equal(p.$('[data-listing="lst_p"] .mp-badge-first').textContent, "By Pigeonpost");
  assert.equal(p.doc.querySelectorAll(".pp-service").length, 5, "All services keeps the built-in assistants");
  assert.equal(p.doc.querySelectorAll("#pp-panel-services .pp-soon").length, 5);
  p.choose(p.$("#mp-filter-marketplace"));
  assert.equal(p.doc.querySelectorAll(".pp-service").length, 0, "Marketplace shows listings only");
  assert.equal(p.doc.querySelectorAll("[data-listing]").length, 2);
  assert.ok(p.$("#mp-cat-status[role=status]"), "outcomes go to a status region");
});

test("with the marketplace feature off the Services tab is unchanged and asks nothing", async (t) => {
  const p = catalogPage(t, { features: ["nests"] });
  await openServices(p);
  assert.equal(p.$("#mp-catalog"), null);
  assert.equal(p.doc.querySelectorAll(".pp-service").length, 5);
  assert.equal(p.doc.querySelectorAll("#pp-panel-services button").length, 0);
  assert.equal(p.calls.some((c) => c.path.startsWith("/v1/marketplace") || c.path.startsWith("/v1/nest-installations")), false);
});

test("search sends the query and an empty result says so", async (t) => {
  const p = catalogPage(t, { listings: [] });
  await openServices(p);
  p.$("#mp-cat-search-q").value = "crm";
  p.submit(p.$("#mp-cat-search"));
  await tick();
  assert.ok(p.calls.some((c) => c.path === "/v1/marketplace/listings?q=crm"));
  assert.match(p.$("#mp-cat-empty").textContent, /Nothing matches/);
});

test("a free listing shows its operations and installs with its settings", async (t) => {
  const p = catalogPage(t);
  await openDetail(p);
  assert.equal(p.$("#mp-detail-title").textContent, "HR helper");
  assert.equal(p.doc.activeElement, p.$("#mp-detail-title"), "focus moves to the detail");
  assert.equal(p.$('.mp-ops code').textContent, "hr.ask");
  assert.match(p.$("#mp-price").textContent, /Free while the marketplace is being tested\./);
  assert.equal(p.$("#mp-buy"), null, "a free listing is installed, not bought");
  const fields = p.$("#mp-install fieldset.mp-config");
  assert.equal(fields.querySelector("legend").textContent, "Settings");
  const secret = p.$("#mp-install-cfg-api_key");
  assert.equal(secret.type, "password");
  assert.equal(p.$('#mp-install [data-secret="api_key"]').textContent, "Not set.");
  assert.equal(p.$('label[for="mp-install-cfg-tone"]').textContent, "Tone");
  p.$("#mp-install-cfg-tone").value = "short";
  p.$("#mp-install-cfg-limit").value = "3";
  secret.value = "k";
  p.submit(p.$("#mp-install"));
  await tick();
  const post = p.calls.find((c) => c.method === "POST" && c.path === "/v1/nest-installations");
  assert.deepEqual(post.body, { listing_id: "lst_a", configuration: { tone: "short", limit: 3, api_key: "k" } });
  assert.match(p.$("#mp-cat-status").textContent, /HR helper is installed/);
  assert.ok(p.$("#mp-installation"), "the installation replaces the install form");
});

const PRICED = { ...DETAIL, price: { amount_minor: 12000, currency: "TRY", term: "monthly", funding: "card", on_sale: true } };
const ORGS = { orgs: [
  { id: "org_acmeco1", name: "Acme Co", my_roles: ["owner"] },
  { id: "org_viewer1", name: "Viewer Co", my_roles: ["member"] },
] };

test("a priced listing is bought for me or a company I manage billing for, through the hosted checkout", async (t) => {
  const checkouts = [];
  let profile = null;
  const p = catalogPage(t, {
    detail: PRICED,
    extra: (m, path) => (m === "GET" && path === "/v1/orgs" ? ORGS : undefined),
    deps: { startCheckout: (pending) => checkouts.push(pending), billingProfileId: async () => profile, goBilling: () => checkouts.push("billing") },
  });
  await openDetail(p);
  await tick();
  assert.match(p.$("#mp-price").textContent, /a month/);
  assert.match(p.$("#mp-price").textContent, /billed monthly through iyzico/);
  assert.equal(p.$("#mp-install"), null, "a priced listing has no free install");
  const owners = [...p.$("#mp-buy-owner").options].map((o) => o.value);
  assert.deepEqual(owners, ["personal", "org_acmeco1"], "only companies whose billing I manage");
  // Without billing details the buyer is sent to them first.
  p.submit(p.$("#mp-buy"));
  await tick();
  assert.match(p.$("#mp-cat-status").textContent, /billing details first/);
  p.$("#mp-buy-billing").click();
  assert.deepEqual(checkouts, ["billing"]);
  profile = "11111111-2222-4333-8444-555555555555";
  p.$("#mp-buy-owner").value = "org_acmeco1";
  p.submit(p.$("#mp-buy"));
  await tick();
  const pending = checkouts[1];
  assert.equal(pending.kind, "marketplace");
  assert.equal(pending.listing_id, "lst_a");
  assert.deepEqual(JSON.parse(JSON.stringify(pending.owner)), { kind: "org", id: "org_acmeco1" });
  assert.equal(pending.billing_profile_id, profile);
  assert.match(pending.intent_id, /^[a-f0-9]{32}$/);
});

test("a listing not on sale shows its price and no buy button", async (t) => {
  const p = catalogPage(t, { detail: { ...PRICED, price: { ...PRICED.price, on_sale: false, term: "one_off" } } });
  await openDetail(p);
  assert.match(p.$("#mp-price").textContent, /one-off/);
  assert.match(p.$("#mp-price").textContent, /Not on sale right now/);
  assert.equal(p.$("#mp-buy"), null);
  assert.equal(p.$("#mp-install"), null);
});

test("back from iyzico the bridge says it was paid, and the install uses the purchase id", async (t) => {
  const intent = "cd".repeat(16);
  const answers = [
    { intent_id: intent, installation_id: "ins_paid1", listing_id: "lst_a", owner: { kind: "account" }, status: "pending" },
    { intent_id: intent, installation_id: "ins_paid1", listing_id: "lst_a", owner: { kind: "account" }, status: "paid" },
  ];
  const cleared = [];
  const p = catalogPage(t, {
    detail: PRICED,
    deps: {
      search: `?mp_purchase=${intent}`,
      clearReturn: (k) => cleared.push(k),
      bridgeGet: async (path) => {
        if (path === `/v1/marketplace/checkout/${intent}`) return answers.length > 1 ? answers.shift() : answers[0];
        return { subscriptions: [], prices: { plans: [] } };
      },
    },
  });
  await tick(60);
  assert.equal(p.$("#pp-tab-services").getAttribute("aria-selected"), "true", "the return opens Services");
  assert.ok(cleared.includes("mp_purchase"));
  assert.match(p.$("#mp-cat-status").textContent, /Waiting for iyzico/);
  p.$("#mp-purchase-refresh").click();
  await tick(60);
  assert.match(p.$("#mp-cat-status").textContent, /Payment received/);
  p.$("#mp-finish-cfg-tone").value = "short";
  p.submit(p.$("#mp-finish"));
  await tick();
  const post = p.calls.find((c) => c.method === "POST" && c.path === "/v1/nest-installations");
  assert.deepEqual(post.body, { listing_id: "lst_a", purchase_id: "ins_paid1", configuration: { tone: "short" } });
});

test("a company purchase installs as the company's, on an address it names", async (t) => {
  const intent = "ef".repeat(16);
  const p = catalogPage(t, {
    detail: PRICED,
    extra: (m, path) => (m === "POST" && path === "/v1/orgs/org_acmeco1/nest-installations" ? { data: { id: "ins_org1" } } : undefined),
    deps: {
      search: `?mp_purchase=${intent}`,
      bridgeGet: async (path) => (path.startsWith("/v1/marketplace/checkout/")
        ? { intent_id: intent, installation_id: "ins_org1", listing_id: "lst_a", owner: { kind: "org", id: "org_acmeco1" }, status: "paid" }
        : { subscriptions: [], prices: { plans: [] } }),
    },
  });
  await tick(60);
  p.$("#mp-finish-handle").value = "hr-helper";
  p.submit(p.$("#mp-finish"));
  await tick();
  const post = p.calls.find((c) => c.method === "POST" && c.path === "/v1/orgs/org_acmeco1/nest-installations");
  assert.deepEqual(post.body, { listing_id: "lst_a", purchase_id: "ins_org1", handle: "hr-helper" });
  assert.match(p.$("#mp-cat-status").textContent, /installed for the company/);
});

test("a paid installation shows its licence, payments with receipts, and Cancel for monthly", async (t) => {
  const actions = [], opened = [];
  let licence = { status: "active", paid_through: "2030-01-31T00:00:00Z", cancel_at_period_end: false };
  const p = catalogPage(t, {
    detail: PRICED,
    installs: [{ ...INSTALL, funding: "card" }],
    deps: {
      bridgeGet: async (path) => {
        if (path === "/v1/marketplace/installations/ins_1") {
          return { installation_id: "ins_1", term: "monthly", licence, payments: [
            { payment_id: "pay_1", status: "settled", amount_minor: 12000, currency: "TRY", invoice_id: "inv_1", settled_at: "2030-01-01T00:00:00Z" },
          ] };
        }
        if (path === "/v1/billing/invoices/inv_1/download") return { url: "https://files.example/receipt.pdf" };
        return { subscriptions: [], prices: { plans: [] } };
      },
      bridgeAction: async (path, body, method) => { actions.push({ path, method }); licence = { ...licence, status: "canceling", cancel_at_period_end: true }; return {}; },
    },
  });
  p.window.open = (url) => opened.push(url);
  await openDetail(p);
  await tick(40);
  assert.match(p.$("#mp-licence-status").textContent, /Licence: Active · paid through/);
  assert.equal(p.doc.querySelectorAll("#mp-payments li").length, 1);
  p.$('[data-receipt="inv_1"]').click();
  await tick();
  assert.deepEqual(opened, ["https://files.example/receipt.pdf"]);
  p.$("#mp-cancel").click();
  await tick(40);
  assert.deepEqual(actions, [{ path: "/v1/marketplace/installations/ins_1/cancel", method: "POST" }]);
  assert.match(p.$("#mp-cat-status").textContent, /keeps working until the end of the paid month/);
  assert.match(p.$("#mp-licence-status").textContent, /until the end of the paid period/);
  assert.equal(p.$("#mp-cancel"), null, "a cancelled licence has nothing more to cancel");
});

test("an install refused for its settings lists the issues", async (t) => {
  const p = catalogPage(t, { extra: (m, path) => {
    if (m === "POST" && path === "/v1/nest-installations") throw fail(400, "invalid_configuration", { issues: [{ path: "/limit", code: "above_maximum", message: "limit is at most 10" }] });
  } });
  await openDetail(p);
  p.submit(p.$("#mp-install"));
  await tick();
  assert.match(p.$("#mp-cat-status").textContent, /Some settings aren't valid\. limit: limit is at most 10/);
});

test("settings keep a set secret unless replaced or cleared", async (t) => {
  const p = catalogPage(t, { installs: [{ ...INSTALL }] });
  await openDetail(p);
  assert.equal(p.$('#mp-configure [data-secret="api_key"]').textContent, "Set. Leave empty to keep it.");
  assert.equal(p.$("#mp-cfg-api_key").value, "", "a secret is never shown");
  assert.equal(p.$("#mp-cfg-tone").value, "long");
  p.submit(p.$("#mp-configure"));
  await tick();
  let put = p.calls.filter((c) => c.method === "PUT").pop();
  assert.equal(put.path, "/v1/nest-installations/ins_1/configuration");
  assert.deepEqual(put.body, { configuration: { tone: "long" } });
  p.$("#mp-cfg-api_key-clear").checked = true;
  p.submit(p.$("#mp-configure"));
  await tick();
  put = p.calls.filter((c) => c.method === "PUT").pop();
  assert.deepEqual(put.body, { configuration: { tone: "long", api_key: null } });
});

test("update, roll back and a typed uninstall", async (t) => {
  const p = catalogPage(t, { installs: [{ ...INSTALL, update_available: { release_id: "rel_2", version: "1.1.0" }, previous_release_id: "rel_0" }] });
  await openDetail(p);
  assert.equal(p.$("#mp-update").textContent, "Update to version 1.1.0");
  p.$("#mp-update").click();
  await tick();
  assert.deepEqual(p.calls.find((c) => c.path === "/v1/nest-installations/ins_1/update").body, { release_id: "rel_2" });
  p.$("#mp-rollback").click();
  await tick();
  assert.equal(p.calls.find((c) => c.path === "/v1/nest-installations/ins_1/rollback").method, "POST");
  const go = p.$("#mp-uninstall-go");
  assert.equal(go.disabled, true);
  assert.equal(p.$("#mp-uninstall legend").textContent, "Uninstall");
  p.type(p.$("#mp-uninstall-confirm"), "uninstall");
  assert.equal(go.disabled, true, "the word must match exactly");
  p.submit(p.$("#mp-uninstall"));
  await tick();
  assert.equal(p.calls.some((c) => c.method === "DELETE"), false);
  p.type(p.$("#mp-uninstall-confirm"), "UNINSTALL");
  assert.equal(go.disabled, false);
  p.submit(p.$("#mp-uninstall"));
  await tick();
  assert.ok(p.calls.some((c) => c.method === "DELETE" && c.path === "/v1/nest-installations/ins_1"));
});

test("installed services are listed above the catalog and a quarantined version is flagged", async (t) => {
  const p = catalogPage(t, { installs: [{ ...INSTALL, security_state: "quarantined", update_available: { release_id: "rel_2", version: "1.1.0" } }] });
  await openServices(p);
  const row = p.$('[data-installation="ins_1"]');
  assert.match(row.textContent, /HR helper.*Version 1\.0\.0.*Update available/s);
  row.querySelector("button").click();
  await tick();
  assert.match(p.$("#mp-installation [role=alert]").textContent, /security review/);
  p.$("#mp-back").click();
  assert.equal(p.doc.activeElement, p.$("#mp-cat-search-q"), "Back returns focus to the catalog");
});

// ---- publisher console ------------------------------------------------------------------------

const PUB = { id: "pub_1", owner_kind: "account", owner_id: "acct_me", display_name: "Acme", status: "approved", decision_reason: null, role: "owner" };
const REVIEW = {
  review_id: "rev_1", publisher_id: "pub_1", release_id: "rel_1", revision: 3, status: "approved",
  stages: [
    { name: "intake", status: "passed" }, { name: "deterministic_scan", status: "passed" },
    { name: "ai_analysis", status: "passed" }, { name: "behavioral_test", status: "failed", blocker_code: "stage_failed", reason: "see the findings" },
    { name: "human_review", status: "pending" }, { name: "attestation", status: "pending" },
  ],
  allowed_actions: ["view_findings", "withdraw", "publish_listing"],
};

function publisherPage(t, { publishers = [PUB], orgs = [], roles = [], listings = [], extra } = {}) {
  const p = page(t, (method, path, body) => {
    if (extra) { const r = extra(method, path, body); if (r !== undefined) return r; }
    if (path === "/v1/publishers") return { data: publishers };
    if (path === "/v1/orgs") return { orgs };
    const b = "/v1/publishers/pub_1";
    if (method === "GET" && path === `${b}/roles`) return { data: roles };
    if (method === "GET" && path === `${b}/templates`) return { data: [{ id: "tpl_1", slug: "hr-helper", name: "HR helper" }] };
    if (method === "GET" && path === `${b}/builds`) return { data: [{ id: "bld_1", template_id: "tpl_1", version: "1.0.0", status: "built", source: { repository: "https://git.example/acme/hr", commit: "a".repeat(40) }, release_id: "rel_1", review_id: "rev_1" }] };
    if (method === "GET" && path === `${b}/reviews`) return { data: [REVIEW] };
    if (method === "GET" && path === `${b}/reviews/rev_1`) return { data: REVIEW };
    if (method === "GET" && path === `${b}/reviews/rev_1/findings`) return { data: { review_id: "rev_1", stages: [
      { stage: "deterministic_scan", decision: "passed", reason: "", findings: [{ rule: "licence", path: "Cargo.lock", detail: "ok", blocker: false }] },
      { stage: "ai_analysis", decision: "advisory", reason: "", findings: [], advisory_findings: 2, model: "m" },
      { stage: "behavioral_test", decision: "failed", reason: "leak", findings: [{ rule: "honeytoken_leak", blocker: true }] },
    ] } };
    if (method === "GET" && path === `${b}/listings`) return { data: listings };
    if (method !== "GET") return { data: { review_id: "rev_1", ...REVIEW } };
    return undefined;
  });
  p.window.PPMarketplace.publisher(p.root, p.deps);
  return p;
}

test("someone with no publisher applies for themselves or for an org they run", async (t) => {
  const p = publisherPage(t, { publishers: [], orgs: [{ id: "org_1", name: "Acme Ltd", my_roles: ["owner"] }, { id: "org_2", name: "Other", my_roles: ["member"] }] });
  await tick();
  assert.equal(p.$("#mp-apply legend").textContent, "Apply to publish");
  const owner = p.$("#mp-apply-owner");
  assert.deepEqual([...owner.options].map((o) => o.textContent), ["My own account", "Acme Ltd"], "only orgs they own or administer");
  p.$("#mp-apply-name").value = " Acme ";
  owner.value = "org_1";
  p.submit(p.$("#mp-apply"));
  await tick();
  assert.deepEqual(p.calls.find((c) => c.path === "/v1/publisher-applications").body, { display_name: "Acme", org_id: "org_1" });
  assert.match(p.$("#mp-pub-status").textContent, /Application sent/);
});

test("a second application is explained, not guessed", async (t) => {
  const p = publisherPage(t, { publishers: [], extra: (m, path) => { if (path === "/v1/publisher-applications") throw fail(409, "application_exists"); } });
  await tick();
  p.$("#mp-apply-name").value = "Acme";
  p.submit(p.$("#mp-apply"));
  await tick();
  assert.match(p.$("#mp-pub-status").textContent, /already an open or approved application/);
});

test("an application under review shows its state and nothing to build yet", async (t) => {
  const p = publisherPage(t, { publishers: [{ ...PUB, status: "applied" }] });
  await tick();
  assert.match(p.$("#mp-pub-state").textContent, /Acme.*personal publisher.*Owner/s);
  assert.match(p.root.textContent, /Application received/);
  assert.equal(p.$("#mp-templates"), null);
  assert.equal(p.calls.some((c) => c.path.endsWith("/templates")), false);
});

test("a certified publisher creates a template and submits a pinned build", async (t) => {
  const p = publisherPage(t);
  await tick();
  assert.match(p.$("#mp-team").textContent, /A personal publisher is just you/);
  p.$("#mp-tpl-name").value = "CRM";
  p.$("#mp-tpl-slug").value = "crm";
  p.submit(p.$("#mp-tpl-create"));
  await tick();
  assert.deepEqual(p.calls.find((c) => c.method === "POST" && c.path.endsWith("/templates")).body, { slug: "crm", name: "CRM" });
  assert.equal(p.$("#mp-build-commit").getAttribute("pattern"), "[0-9a-f]{40}|[0-9a-f]{64}");
  p.$("#mp-build-version").value = "1.2.0";
  p.$("#mp-build-repo").value = "https://git.example/acme/crm";
  p.$("#mp-build-commit").value = "B".repeat(40);
  p.submit(p.$("#mp-build-create"));
  await tick();
  assert.deepEqual(p.calls.find((c) => c.method === "POST" && c.path.endsWith("/builds")).body,
    { template_id: "tpl_1", version: "1.2.0", source: { repository: "https://git.example/acme/crm", commit: "b".repeat(40) } });
  assert.match(p.$('[data-build="bld_1"]').textContent, /HR helper 1\.0\.0.*Built.*aaaaaaaaaaaa/s);
});

test("a review shows its stages, allowed actions and redacted findings, and can be withdrawn", async (t) => {
  const p = publisherPage(t);
  await tick();
  p.$('[data-review="rev_1"] button').click();
  await tick();
  assert.equal(p.doc.activeElement, p.$("#mp-review-title"));
  assert.match(p.$("#mp-review-state").textContent, /Approved · revision 3/);
  assert.deepEqual([...p.doc.querySelectorAll("#mp-review [data-stage]")].map((x) => x.dataset.stage),
    ["intake", "deterministic_scan", "ai_analysis", "behavioral_test", "human_review", "attestation"]);
  assert.match(p.$('#mp-review [data-stage="behavioral_test"]').textContent, /Behaviour test: Failed · see the findings/);
  assert.ok(p.$("#mp-review-withdraw") && p.$("#mp-review-list") && p.$("#mp-review-findings"));
  assert.equal(p.$("#mp-review-resubmit"), null, "only the actions the server allows");
  p.$("#mp-review-findings").click();
  await tick();
  const f = p.$("#mp-findings");
  assert.match(f.querySelector('[data-stage="deterministic_scan"]').textContent, /licence in Cargo\.lock · ok/);
  assert.match(f.querySelector('[data-stage="ai_analysis"]').textContent, /2 advisory notes for staff/);
  assert.match(f.querySelector('[data-stage="behavioral_test"]').textContent, /honeytoken_leak · blocks the release/);
  p.$("#mp-review-withdraw").click();
  await tick();
  assert.ok(p.calls.some((c) => c.method === "POST" && c.path === "/v1/publishers/pub_1/reviews/rev_1/withdraw"));
  p.$("#mp-review-list").click();
  assert.equal(p.doc.activeElement, p.$("#mp-listing-title"), "Create a listing jumps to the form");
  assert.equal(p.$("#mp-listing-release").value, "rel_1");
});

test("a listing is created as a draft, edited by changed fields only, and published with step-up", async (t) => {
  let stepUpOnce = true;
  const p = publisherPage(t, {
    listings: [{ id: "lst_1", title: "HR helper", summary: "Answers", description: "", visibility: "public", status: "draft", version: "1.0.0" }],
    extra: (m, path) => {
      if (m === "POST" && path.endsWith("/publish") && stepUpOnce) { stepUpOnce = false; throw fail(401, "step_up_required", { acr_values: "gold", max_age: 300 }); }
    },
  });
  await tick();
  p.$("#mp-listing-title").value = "HR helper";
  p.$("#mp-listing-summary").value = "Answers HR questions";
  p.choose(p.$("#mp-listing-unlisted"));
  p.submit(p.$("#mp-listing-create"));
  await tick();
  assert.deepEqual(p.calls.find((c) => c.method === "POST" && c.path === "/v1/publishers/pub_1/listings").body,
    { release_id: "rel_1", title: "HR helper", summary: "Answers HR questions", visibility: "unlisted" });
  assert.equal(p.$("#mp-listing-create .mp-visibility legend").textContent, "Who can see it");
  p.$('button[aria-label="Edit HR helper"]').click();
  p.$("#mp-edit-lst_1-summary").value = "Answers HR questions";
  p.choose(p.$("#mp-edit-lst_1-private"));
  p.submit(p.$("#mp-edit-lst_1"));
  await tick();
  const patch = p.calls.find((c) => c.method === "PATCH");
  assert.equal(patch.path, "/v1/publishers/pub_1/listings/lst_1");
  assert.deepEqual(patch.body, { summary: "Answers HR questions", visibility: "private" });
  p.$('[data-publish="lst_1"]').click();
  await tick();
  assert.match(p.$("#mp-pub-status").textContent, /two-step sign-in/);
  p.$('#mp-pub-status [data-stepup="publisher"]').click();
  assert.deepEqual(JSON.parse(JSON.stringify(p.stepUps)), [{ challenge: { acr_values: "gold", max_age: 300 }, section: "publisher" }]);
  p.$('[data-publish="lst_1"]').click();
  await tick();
  assert.match(p.$("#mp-pub-status").textContent, /HR helper is published/);
  assert.match(p.$("#mp-earnings").textContent, /Set a price on a listing/);
  assert.ok(p.$("#mp-installs"));
});

test("a finance role reads but cannot change anything", async (t) => {
  const p = publisherPage(t, { publishers: [{ ...PUB, owner_kind: "org", owner_id: "org_1", role: "publisher_finance" }],
    listings: [{ id: "lst_1", title: "HR helper", summary: "A", visibility: "public", status: "draft", version: "1.0.0" }] });
  await tick();
  assert.match(p.$("#mp-readonly").textContent, /can read this publisher but not change it/);
  for (const sel of ["#mp-tpl-create", "#mp-build-create", "#mp-listing-create", "#mp-role-grant", "[data-publish]"]) assert.equal(p.$(sel), null, sel);
  p.$('[data-review="rev_1"] button').click();
  await tick();
  assert.equal(p.$("#mp-review-withdraw"), null);
  assert.ok(p.$("#mp-review-findings"), "findings stay readable");
});

test("an org admin gives and removes publisher roles", async (t) => {
  const p = publisherPage(t, { publishers: [{ ...PUB, owner_kind: "org", owner_id: "org_1", role: "org_admin" }], roles: [{ id: "prl_1", account_id: "acct_2", role: "publisher_admin" }] });
  await tick();
  assert.match(p.$('[data-role="prl_1"]').textContent, /acct_2.*Publisher admin/s);
  p.$("#mp-role-account").value = "acct_3";
  p.$("#mp-role-role").value = "publisher_finance";
  p.submit(p.$("#mp-role-grant"));
  await tick();
  assert.deepEqual(p.calls.find((c) => c.method === "POST" && c.path.endsWith("/roles")).body, { account_id: "acct_3", role: "publisher_finance" });
  p.$('[data-role="prl_1"] button').click();
  await tick();
  assert.ok(p.calls.some((c) => c.method === "DELETE" && c.path === "/v1/publishers/pub_1/roles/prl_1"));
});

test("a revoked publisher sees why", async (t) => {
  const p = publisherPage(t, { publishers: [{ ...PUB, status: "revoked", decision_reason: "malware" }] });
  await tick();
  assert.match(p.root.textContent, /Certification revoked.*Reason: malware/s);
  assert.equal(p.$("#mp-tpl-create"), null);
});

// ---- staff dashboard ----------------------------------------------------------------------------

const QUEUE = [
  { id: "rev_1", release_id: "rel_1", publisher_id: "pub_1", publisher_name: "Acme", version: "1.0.0", status: "open", revision: 4, stages_recorded: ["intake", "deterministic_scan", "ai_analysis", "behavioral_test"], first_party: false, conflict: false },
  { id: "rev_2", release_id: "rel_2", publisher_id: "pub_2", publisher_name: "Mine", version: "0.1.0", status: "open", revision: 1, stages_recorded: ["intake"], first_party: false, conflict: true },
];
const APPS = [
  { id: "pub_9", owner_kind: "account", display_name: "Newco", status: "applied", created_at: "2026-11-09T00:00:00Z", conflict: false },
  { id: "pub_8", owner_kind: "org", display_name: "Mine Ltd", status: "applied", created_at: "2026-11-09T00:00:00Z", conflict: true },
];

function staffPage(t, { gate = null, reviewers = ["acct_owner"], apps = APPS, extra } = {}) {
  const p = page(t, (method, path, body) => {
    if (extra) { const r = extra(method, path, body); if (r !== undefined) return r; }
    if (path.startsWith("/v1/staff/reviews?")) { if (gate) throw gate; return { data: QUEUE }; }
    if (path.startsWith("/v1/staff/publisher-applications?")) {
      const status = new URL(path, "https://x").searchParams.get("status");
      return { data: status === "approved" ? [{ ...APPS[0], id: "pub_1", display_name: "Acme", status: "approved" }] : apps };
    }
    if (path === "/v1/staff/roles") { if (!reviewers) throw fail(403, "forbidden"); return { data: reviewers.map((a) => ({ id: "s_" + a, account_id: a, role: "reviewer" })) }; }
    if (path === "/v1/marketplace/listings") return { data: [LISTING] };
    if (path === "/v1/staff/reviews/rev_1") return { data: { id: "rev_1", status: "open", release_id: "rel_1", version: "1.0.0", release_digest: "sha256:abc", image: "img@sha256:1", sbom_digest: "sha256:s", source_digest: "sha256:src", security_state: "normal", first_party: false,
      stages: [{ id: "d1", stage: "intake", decision: "passed", reviewer: "build-plane", reason: "", evidence_digest: "sha256:e1", created_at: "2026-11-09T00:00:00Z" }] } };
    if (path === "/v1/staff/reviews/rev_2") return { data: { id: "rev_2", status: "open", release_id: "rel_2", release_digest: "sha256:def", stages: [] } };
    if (method === "POST") return { data: {} };
    return undefined;
  });
  p.window.PPMarketplace.staff(p.root, p.deps);
  return p;
}

for (const [label, gate] of [["a non-staff account", fail(403, "staff_required")], ["the marketplace feature off", fail(404, "feature_disabled")]]) {
  test(`the staff dashboard stays hidden for ${label}`, async (t) => {
    const p = staffPage(t, { gate });
    await tick();
    assert.equal(p.root.hidden, true);
    assert.equal(p.root.textContent, "");
    assert.equal(p.calls.length, 1, "one probe, nothing else");
  });
}

test("staff without a fresh strong sign-in get one step-up button", async (t) => {
  const p = staffPage(t, { gate: fail(401, "step_up_required", { acr_values: "gold urn:pigeonpost:acr:mfa", max_age: 600 }) });
  await tick();
  assert.equal(p.root.hidden, false);
  assert.match(p.root.textContent, /recent two-step sign-in/);
  assert.equal(p.$("#mp-staff-reviews"), null);
  p.$('[data-stepup="staff"]').click();
  assert.deepEqual(JSON.parse(JSON.stringify(p.stepUps)), [{ challenge: { acr_values: "gold urn:pigeonpost:acr:mfa", max_age: 600 }, section: "staff" }]);
});

test("the review queue flags conflicts and a conflicted review cannot be decided", async (t) => {
  const p = staffPage(t);
  await tick();
  assert.match(p.$("#mp-staff-single").textContent, /only reviewer.*single-person approval/);
  assert.match(p.$('[data-review="rev_1"]').textContent, /Acme 1\.0\.0.*4 of 4 checks recorded/s);
  assert.equal(p.$('[data-review="rev_1"] [data-conflict]'), null);
  assert.match(p.$('[data-review="rev_2"] [data-conflict]').textContent, /Conflict/);
  p.$('[data-review="rev_2"] button').click();
  await tick();
  assert.ok(p.$("#mp-sr-conflict"));
  assert.equal(p.$("#mp-sr-decision"), null);
  assert.equal(p.$("#mp-sr-security-form"), null);
});

test("a decision is recorded over the release digest and labelled single-person", async (t) => {
  const p = staffPage(t);
  await tick();
  p.$('[data-review="rev_1"] button').click();
  await tick();
  assert.equal(p.doc.activeElement, p.$("#mp-sr-title"));
  assert.equal(p.$("#mp-sr-digest").textContent, "sha256:abc");
  assert.match(p.$('#mp-sr [data-stage="intake"]').textContent, /Intake: passed.*evidence sha256:e1/s);
  assert.match(p.$("#mp-sr-decision-single").textContent, /Single-person approval/);
  p.submit(p.$("#mp-sr-decision"));
  await tick();
  assert.match(p.$("#mp-staff-status").textContent, /Choose a decision/);
  p.$("#mp-sr-decision-approved").checked = true;
  p.$("#mp-sr-decision-reason").value = "Looks right";
  p.submit(p.$("#mp-sr-decision"));
  await tick();
  assert.deepEqual(p.calls.find((c) => c.path === "/v1/staff/reviews/rev_1/decisions").body, { decision: "approved", reason: "Looks right", release_digest: "sha256:abc" });
});

test("with a second reviewer the single-person label goes away", async (t) => {
  const p = staffPage(t, { reviewers: ["acct_owner", "acct_two"] });
  await tick();
  assert.equal(p.$("#mp-staff-single"), null);
  p.$('[data-review="rev_1"] button').click();
  await tick();
  assert.doesNotMatch(p.$("#mp-sr-decision-single").textContent, /Single-person/);
});

test("release quarantine and a typed revocation", async (t) => {
  const p = staffPage(t);
  await tick();
  p.$('[data-review="rev_1"] button').click();
  await tick();
  const form = () => p.$("#mp-sr-security-form");
  p.$("#mp-sr-security-form-quarantine").checked = true;
  p.$("#mp-sr-security-form-reason").value = "suspicious";
  p.submit(form());
  await tick();
  assert.deepEqual(p.calls.find((c) => c.path === "/v1/staff/releases/rel_1/security").body, { action: "quarantine", reason: "suspicious" });
  p.$("#mp-sr-security-form-revoke").checked = true;
  p.$("#mp-sr-security-form-reason").value = "bad";
  p.submit(form());
  await tick();
  assert.equal(p.calls.filter((c) => c.path === "/v1/staff/releases/rel_1/security").length, 1, "revoke needs the typed word");
  p.$("#mp-sr-security-form-confirm").value = "REVOKE";
  p.submit(form());
  await tick();
  assert.deepEqual(p.calls.filter((c) => c.path === "/v1/staff/releases/rel_1/security")[1].body, { action: "revoke", reason: "bad" });
});

test("applications are decided, conflicted ones are not offered, and certified publishers can be revoked", async (t) => {
  const p = staffPage(t);
  await tick();
  assert.ok(p.$('[data-application="pub_8"] [data-conflict]'));
  assert.equal(p.$('[data-application="pub_8"] form'), null);
  p.$("#mp-app-pub_9-ok").checked = true;
  p.$("#mp-app-pub_9-reason").value = "Verified";
  p.submit(p.$("#mp-app-pub_9"));
  await tick();
  assert.deepEqual(p.calls.find((c) => c.path === "/v1/staff/publisher-applications/pub_9/decisions").body, { decision: "approved", reason: "Verified" });
  const filter = p.$("#mp-staff-app-filter");
  filter.value = "approved";
  filter.dispatchEvent(new p.window.Event("change", { bubbles: true }));
  await tick();
  p.$('button[aria-label="Revoke Acme"]').click();
  p.$("#mp-revoke-pub_1-reason").value = "abuse";
  p.submit(p.$("#mp-revoke-pub_1"));
  await tick();
  assert.equal(p.calls.some((c) => c.path.endsWith("/revocation")), false);
  p.type(p.$("#mp-revoke-pub_1-confirm"), "REVOKE");
  p.submit(p.$("#mp-revoke-pub_1"));
  await tick();
  assert.deepEqual(p.calls.find((c) => c.path === "/v1/staff/publishers/pub_1/revocation").body, { reason: "abuse" });
});

test("a listing is suspended with a reason", async (t) => {
  const p = staffPage(t);
  await tick();
  assert.equal(p.$("#mp-mod-options option").value, "lst_a");
  p.$("#mp-mod-listing").value = "lst_a";
  p.$("#mp-mod-suspend").checked = true;
  p.$("#mp-mod-reason").value = "reported";
  p.submit(p.$("#mp-mod"));
  await tick();
  assert.deepEqual(p.calls.find((c) => c.path === "/v1/staff/listings/lst_a/moderation").body, { action: "suspend", reason: "reported" });
});

test("a decision refused for a stake reads as a conflict", async (t) => {
  const p = staffPage(t, { extra: (m, path) => { if (m === "POST" && path.endsWith("/decisions")) throw fail(403, "staff_conflict"); } });
  await tick();
  p.$("#mp-app-pub_9-no").checked = true;
  p.$("#mp-app-pub_9-reason").value = "No";
  p.submit(p.$("#mp-app-pub_9"));
  await tick();
  assert.match(p.$("#mp-staff-status").textContent, /You have a stake in this publisher/);
});

// ---- the account page ---------------------------------------------------------------------------

async function accountPage(t, { features, nested = false }) {
  // jsdom does not navigate: a sign-in redirect only reports "Not implemented: navigation" here.
  const navigations = [];
  const virtualConsole = new VirtualConsole();
  virtualConsole.on("jsdomError", (e) => { if (/navigation/i.test(String(e && e.message))) navigations.push(e.message); });
  const dom = new JSDOM('<main id="account-root"></main>', { url: "https://pigeonpost.dev/account", runScripts: "outside-only", virtualConsole });
  // Let any pending page work (the step-up's PKCE digest, late fetches) finish before closing.
  t.after(async () => { for (let i = 0; i < 20; i++) await tick(1); dom.window.close(); });
  const { window } = dom;
  window.eval(accountConfig);
  window.sessionStorage.setItem("pp_session", "member-fixture");
  // The step-up sign-in builds a PKCE challenge before it navigates.
  if (!window.crypto.subtle) Object.defineProperty(window.crypto, "subtle", { value: globalThis.crypto.subtle });
  const calls = [];
  window.fetch = async (url, init = {}) => {
    const u = new URL(url);
    const path = u.pathname + u.search;
    calls.push({ path, method: init.method || "GET" });
    const res = (body, status = 200) => ({ ok: status < 400, status, json: async () => body });
    if (path === "/v1/capabilities") return res({ features });
    if (path === "/v1/publishers") return res({ data: [{ ...PUB, status: "approved" }] });
    if (path === "/v1/publishers/pub_1/listings") return res({ data: [{ id: "lst_1", title: "HR helper", summary: "A", visibility: "public", status: "draft", version: "1.0.0" }] });
    if (path === "/v1/publishers/pub_1/listings/lst_1/publish") {
      // The postbox's own error shape is nested (`{error: {code, message, details}}`).
      return nested
        ? res({ error: { code: "step_up_required", message: "sign in again", details: { acr_values: "gold", max_age: 300 } } }, 401)
        : res({ error: "step_up_required", acr_values: "gold", max_age: 300 }, 401);
    }
    if (path.startsWith("/v1/publishers/pub_1/")) return res({ data: [] });
    if (path.startsWith("/v1/staff/")) return res({ error: "staff_required", detail: "staff only" }, 403);
    if (path === "/v1/orgs") return res({ orgs: [] });
    return res({ identities: [], keys: [], handles: [], subscriptions: [], invoices: [], paymentMethods: [], billingProfiles: [], data: [], packages: [] });
  };
  for (const s of [core, catalogJs, publisherJs, staffJs, products]) window.eval(s);
  window.eval(accountJs);
  for (let i = 0; i < 40; i++) await tick(1);
  return { window, doc: window.document, calls, navigations };
}

test("the account page mounts the publisher console only with the marketplace feature, and staff only for staff", async (t) => {
  const off = await accountPage(t, { features: ["nests"] });
  assert.equal(off.doc.querySelector("#ac-publisher").hidden, true);
  assert.equal(off.doc.querySelector("#ac-staff").hidden, true);
  assert.equal(off.calls.some((c) => c.path.startsWith("/v1/publishers") || c.path.startsWith("/v1/staff")), false);

  const on = await accountPage(t, { features: ["nests", "marketplace"] });
  assert.equal(on.doc.querySelector("#ac-publisher").hidden, false);
  assert.match(on.doc.querySelector("#mp-pub-title").textContent, /Publish on the marketplace/);
  assert.equal(on.doc.querySelector("#ac-staff").hidden, true, "a 403 staff_required keeps the staff card hidden");
});

test("a step-up refusal on the account page does not burn a token refresh and keeps the challenge", async (t) => {
  const p = await accountPage(t, { features: ["nests", "marketplace"] });
  p.doc.querySelector('[data-publish="lst_1"]').click();
  for (let i = 0; i < 20; i++) await tick(1);
  assert.equal(p.calls.some((c) => c.path === "/api/v1/auth/refresh"), false);
  const btn = p.doc.querySelector('#mp-pub-status [data-stepup="publisher"]');
  assert.ok(btn, "the console offers the strong sign-in");
  btn.click();
  // Wait for the sign-in redirect itself (after the async PKCE digest), not a fixed number of ticks:
  // on a slow runner the digest outlived ten ticks and finished after the window was closed.
  for (let i = 0; i < 400 && !p.navigations.length; i++) await tick(1);
  assert.ok(p.navigations.length, "the strong sign-in redirect was attempted");
  assert.equal(p.window.localStorage.getItem("pp_mp_return"), "publisher", "the page comes back to the publisher card");
});

test("the postbox's nested error shape reads as its code, with the step-up challenge", async (t) => {
  const p = await accountPage(t, { features: ["nests", "marketplace"], nested: true });
  p.doc.querySelector('[data-publish="lst_1"]').click();
  for (let i = 0; i < 20; i++) await tick(1);
  assert.equal(p.calls.some((c) => c.path === "/api/v1/auth/refresh"), false, "a step-up is not an expired token");
  const btn = p.doc.querySelector('#mp-pub-status [data-stepup="publisher"]');
  assert.ok(btn, "the nested step_up_required is recognised");
});

test("a publisher prices a listing with step-up and is offered the nearest invoiceable prices", async (t) => {
  let answers = [
    () => { throw fail(401, "step_up_required", { acr_values: "gold", max_age: 300 }); },
    () => { throw fail(422, "price_not_invoiceable", { nearest: [11999, 12001] }); },
    () => ({ data: { listing_id: "lst_1", price: { amount_minor: 12001, currency: "TRY", term: "monthly" } } }),
  ];
  const p = publisherPage(t, {
    listings: [{ id: "lst_1", title: "HR helper", summary: "A", visibility: "public", status: "published", version: "1.0.0",
      price: { amount_minor: 0, currency: "TRY", term: "one_off", funding: "test_credits" } }],
    extra: (m, path) => (m === "PUT" && path === "/v1/publishers/pub_1/listings/lst_1/price" ? answers.shift()() : undefined),
  });
  await tick();
  p.$('[data-price="lst_1"]').click();
  assert.match(p.$("#mp-price-lst_1").textContent, /Now: Free/);
  p.$("#mp-price-lst_1-amount").value = "120";
  p.$("#mp-price-lst_1-term").value = "monthly";
  p.submit(p.$("#mp-price-lst_1"));
  await tick();
  assert.ok(p.$('#mp-pub-status [data-stepup="publisher"]'), "a price change asks for step-up");
  p.submit(p.$("#mp-price-lst_1"));
  await tick();
  const puts = p.calls.filter((c) => c.method === "PUT");
  assert.deepEqual(puts[1].body, { amount_minor: 12000, currency: "TRY", term: "monthly" });
  const near = [...p.doc.querySelectorAll("#mp-price-lst_1-nearest [data-nearest]")].map((b) => b.getAttribute("data-nearest"));
  assert.deepEqual(near, ["11999", "12001"]);
  p.$('#mp-price-lst_1-nearest [data-nearest="12001"]').click();
  await tick();
  assert.equal(p.calls.filter((c) => c.method === "PUT")[2].body.amount_minor, 12001);
  assert.match(p.$("#mp-pub-status").textContent, /Price saved/);
});

// ---- publisher-hosted listings --------------------------------------------------------------

const HOSTED = { ...DETAIL, hosting: "publisher", service_address: "/acme/hr", operations: [{ name: "hr.research", version: "1.0", title: "HR research", fields: [{ name: "role", max_chars: 200, required: true, label: "Role" }, { name: "notes", max_chars: 2000, required: false, multiline: true }], output: { kind: "file", media_type: "application/pdf" } }] };

test("a publisher-hosted listing shows the hosting notice, runs an operation from its fields and lists runs", async (t) => {
  const jobs = [];
  const p = catalogPage(t, { detail: HOSTED, listings: [{ ...LISTING, hosting: "publisher" }], extra: (method, path, body) => { if (method === "GET" && path === "/v1/marketplace/hosted-jobs") return { data: jobs }; if (method === "POST" && path === "/v1/marketplace/hosted-jobs") { const j = { job_id: "job_1", listing_id: "lst_a", operation: body.operation, execution: "queued", created_at: "2026-11-11T10:00:00Z", result: null }; jobs.push(j); return { data: j }; } return undefined; } });
  await openDetail(p);
  assert.match(p.$("#mp-hosted-notice").textContent, /Hosted by the publisher/);
  assert.match(p.$("#mp-hosted-notice").textContent, /your inputs are sent to the publisher/);
  assert.equal(p.$("#mp-install"), null);
  assert.equal(p.$("#mp-buy"), null);
  assert.ok(p.$('[data-hosting="publisher"]'));
  assert.equal(p.$("#mp-run-0-notes").tagName, "TEXTAREA");
  assert.equal(p.$("#mp-run-0-role").required, true);
  p.$("#mp-run-0-role").value = "CTO";
  p.submit(p.$("#mp-run-0"));
  await tick();
  const post = p.calls.find((c) => c.method === "POST" && c.path === "/v1/marketplace/hosted-jobs");
  assert.deepEqual(post.body, { listing_id: "lst_a", operation: "hr.research", operation_version: "1.0", input: { role: "CTO" } });
  assert.match(p.$("#mp-cat-status").textContent, /Sent to the publisher/);
  assert.match(p.$('#mp-jobs [data-job="job_1"]').textContent, /Sending to the publisher/);
});

test("a priced publisher-hosted listing offers no run until paid runs exist", async (t) => {
  const HOSTED_PRICE = { ...HOSTED, price: { amount_minor: 99900, currency: "TRY", term: "one_off", funding: "card", on_sale: true } };
  const p = catalogPage(t, { detail: HOSTED_PRICE, listings: [{ ...LISTING, hosting: "publisher" }], extra: (method, path) => { if (method === "GET" && path === "/v1/marketplace/hosted-jobs") return { data: [] }; return undefined; } });
  await openDetail(p);
  assert.equal(p.$("#mp-run-0"), null);
  assert.match(p.$("#mp-hosted-paid").textContent, /aren't available yet/);
});
