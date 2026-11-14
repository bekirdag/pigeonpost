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
const onboardingJs = read("../../site/publisher-onboarding.js");
const products = read("../../site/products.js");
const creditsJs = read("../../site/credits.js");
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
  for (const s of [core, catalogJs, onboardingJs, publisherJs, staffJs, creditsJs, products]) window.eval(s);
  const calls = [], stepUps = [];
  const pb = async (path, init = {}) => {
    const method = init.method || "GET";
    // A logo goes up as the file itself (`contentType`); everything else is JSON.
    const body = typeof init.body === "string" ? JSON.parse(init.body) : init.body;
    calls.push({ path, method, body, contentType: init.contentType });
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
  assert.equal(p.$('[data-listing="lst_a"] .mp-publisher-name').textContent, "Acme");
  assert.equal(p.$('[data-listing="lst_a"] .mp-verified'), null, "not certified, no badge");
  assert.equal(p.$('[data-listing="lst_p"] .mp-publisher-name').textContent, "Pigeonpost");
  assert.ok(p.$('[data-listing="lst_p"] .mp-verified'), "first-party listings carry the badge");
  const builtins = p.doc.querySelectorAll("#mp-builtins [data-builtin]");
  assert.equal(builtins.length, 5, "All services keeps the built-in assistants, as cards");
  assert.equal(p.$("#mp-builtins-title").textContent, "Coming from Pigeonpost");
  const pa = p.$('[data-builtin="pp_personal_assistant"]');
  assert.equal(pa.querySelector(".mp-ribbon").textContent, "Coming soon");
  assert.equal(pa.querySelector(".mp-logo").dataset.logo, "default");
  assert.equal(pa.querySelector(".mp-card-title").textContent, "Personal assistant");
  assert.equal(pa.querySelector(".mp-publisher-name").textContent, "Pigeonpost");
  assert.ok(pa.querySelector(".mp-verified"));
  assert.equal(pa.querySelector(".mp-card-category").textContent, "Productivity");
  assert.equal(pa.querySelector(".mp-card-hosting"), null);
  assert.equal(pa.querySelector("button, a"), null, "a teaser opens nothing");
  assert.equal(p.doc.querySelectorAll("#pp-panel-services .pp-soon").length, 0, "no separate Coming soon line");
  p.choose(p.$("#mp-filter-marketplace"));
  assert.equal(p.$("#mp-builtins"), null, "Marketplace shows listings only");
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
  assert.match(p.$("#mp-pub-status").textContent, /Draft created/);
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
    const dossier = (id, state = id === "pub_1" ? "approved" : "submitted", revision = 4) => ({ id, state, revision, kind: "company", country: "TR", details: { public_name: id === "pub_1" ? "Acme" : "Newco" }, documents: [], history: [], kyc_status: "not_started" });
    if (path.startsWith("/v1/staff/publisher-dossiers?")) {
      const approved = new URL(path, "https://x").searchParams.get("status") === "approved";
      const data = approved ? [{ id: "pub_1", display_name: "Acme", state: "approved" }] : apps.map(a => ({ ...a, state: "submitted" }));
      return { data, total: data.length, next_offset: null };
    }
    if (path.startsWith("/v1/staff/publisher-dossiers/")) {
      const id = path.split("/")[4];
      return { data: dossier(id, method === "POST" ? body.decision : undefined, method === "POST" ? body.revision + 1 : 4) };
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

test("submitted applications require a versioned review, conflicts are excluded, and revocation is confirmed", async (t) => {
  const p = staffPage(t);
  await tick();
  const row = id => [...p.doc.querySelectorAll(".on-table tbody tr")].find(r => r.querySelector("small").textContent === id);
  assert.match(row("pub_8").textContent, /Conflict of interest/);
  assert.equal(row("pub_8").querySelector("button"), null);
  row("pub_9").querySelector("button").click();
  await tick();
  p.$('.on-review select[aria-label="Decision"]').value = "approved";
  p.$(".on-review textarea").value = "Verified";
  p.submit(p.$(".on-review form"));
  await tick();
  assert.deepEqual(p.calls.find(c => c.path === "/v1/staff/publisher-dossiers/pub_9/review").body, { revision: 4, decision: "approved", reason: "Verified", requested_documents: [] });
  const filter = p.$(".on-filters select");
  filter.value = "approved";
  filter.dispatchEvent(new p.window.Event("change", { bubbles: true }));
  p.submit(p.$(".on-filters"));
  await tick();
  row("pub_1").querySelector("button").click();
  await tick();
  p.$('.on-review textarea[aria-label="Revocation reason"]').value = "abuse";
  p.submit(p.$(".on-review form"));
  await tick();
  assert.equal(p.calls.some(c => c.path.endsWith("/revocation")), false);
  p.type(p.$('.on-review input[aria-label="Type REVOKE to confirm"]'), "REVOKE");
  p.submit(p.$(".on-review form"));
  await tick();
  assert.deepEqual(p.calls.find(c => c.path === "/v1/staff/publishers/pub_1/revocation").body, { reason: "abuse" });
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

test("a conflicted reviewer cannot record a decision", async (t) => {
  const p = staffPage(t, { extra: (m, path) => { if (m === "POST" && path === "/v1/staff/publisher-dossiers/pub_9/review") throw fail(403, "staff_conflict"); } });
  await tick();
  const row = [...p.doc.querySelectorAll(".on-table tbody tr")].find(r => r.querySelector("small").textContent === "pub_9");
  row.querySelector("button").click();
  await tick();
  p.$('.on-review select[aria-label="Decision"]').value = "rejected";
  p.$(".on-review textarea").value = "No";
  p.submit(p.$(".on-review form"));
  await tick();
  assert.match(p.$('#mp-staff-apps p[role="status"]').textContent, /staff_conflict/);
  assert.equal(p.calls.find(c => c.path.endsWith("/pub_9/review")).body.revision, 4);
  assert.match(p.$(".on-review h3").textContent, /Submitted/);
});

// ---- the account page ---------------------------------------------------------------------------

async function accountPage(t, { features, nested = false, url = "https://pigeonpost.dev/account", before = null, staffRoles = null }) {
  // jsdom does not navigate: a sign-in redirect only reports "Not implemented: navigation" here.
  const navigations = [];
  const virtualConsole = new VirtualConsole();
  virtualConsole.on("jsdomError", (e) => { if (/navigation/i.test(String(e && e.message))) navigations.push(e.message); });
  const dom = new JSDOM('<main id="account-root"></main>', { url, runScripts: "outside-only", virtualConsole });
  // Let any pending page work (the step-up's PKCE digest, late fetches) finish before closing.
  t.after(async () => { for (let i = 0; i < 20; i++) await tick(1); dom.window.close(); });
  const { window } = dom;
  window.eval(accountConfig);
  window.sessionStorage.setItem("pp_session", "member-fixture");
  if (before) before(window);
  // The step-up sign-in builds a PKCE challenge before it navigates.
  if (!window.crypto.subtle) Object.defineProperty(window.crypto, "subtle", { value: globalThis.crypto.subtle });
  const calls = [];
  window.fetch = async (url, init = {}) => {
    // Bridge calls are relative (`/api/...`); postbox calls are absolute.
    const u = new URL(url, "https://pigeonpost.dev");
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
    if (path === "/v1/staff/me" && staffRoles) return res({ data: { account_id: "acct_s", roles: staffRoles, platform_owner: false } });
    if (path.startsWith("/v1/staff/")) return res({ error: "staff_required", detail: "staff only" }, 403);
    if (path === "/v1/orgs") return res({ orgs: [] });
    if (path === "/api/v1/credits/packs") return res({ packs: [{ pack_id: "usd_10", amount_cents: 1000 }, { pack_id: "usd_25", amount_cents: 2500 }], currencies: ["USD"] });
    if (path === "/api/v1/credits/quotes") return res({ quote_id: "q1", pack_id: "usd_25", credit_cents: 2500, charge: { currency: "USD", amount_minor: 2500, tax_minor: 0 }, fx: null, expires_at: "2030-01-01T00:00:00Z" });
    if (path === "/api/v1/credits/balance") return res({ available_cents: 1234, held_cents: 0 });
    if (path.startsWith("/api/v1/credits/history")) return res({ entries: [], next_cursor: null });
    return res({ identities: [], keys: [], handles: [], subscriptions: [], invoices: [], paymentMethods: [], billingProfiles: [], data: [], packages: [] });
  };
  for (const s of [core, catalogJs, onboardingJs, publisherJs, staffJs, creditsJs, products]) window.eval(s);
  window.eval(accountJs);
  for (let i = 0; i < 40; i++) await tick(1);
  return { window, doc: window.document, calls, navigations };
}

test("the account page mounts the publisher console only with the marketplace feature, and staff only for staff", async (t) => {
  const off = await accountPage(t, { features: ["nests"] });
  assert.equal(off.doc.querySelector("#ac-publisher").hidden, true);
  assert.equal(off.doc.querySelector("#ac-staff"), null, "the staff card moved to the back office");
  assert.equal(off.doc.querySelector("#ac-nav-staff").hidden, true);
  assert.equal(off.calls.some((c) => c.path.startsWith("/v1/publishers")), false);
  assert.deepEqual(off.calls.filter((c) => c.path.startsWith("/v1/staff")).map((c) => c.path), ["/v1/staff/me"], "only the back office probe");
  assert.equal(off.doc.querySelector("#credits").hidden, true, "credits come with the marketplace");
  assert.equal(off.calls.some((c) => c.path.startsWith("/api/v1/credits")), false);

  const on = await accountPage(t, { features: ["nests", "marketplace"] });
  assert.equal(on.doc.querySelector("#ac-publisher").hidden, false);
  assert.match(on.doc.querySelector("#mp-pub-title").textContent, /Publish on the marketplace/);
  assert.equal(on.doc.querySelector("#ac-nav-staff").hidden, true, "a 403 staff_required keeps the back office link hidden");
  assert.equal(on.doc.querySelector("#credits").hidden, false);
  assert.match(on.doc.querySelector("#cr-balance").textContent, /Balance: \$12\.34/);
});

test("staff get a back office link, and the old staff hashes go to /staff", async (t) => {
  const staff = await accountPage(t, { features: ["nests"], staffRoles: ["support"] });
  const nav = staff.doc.querySelector("#ac-nav-staff");
  assert.equal(nav.hidden, false);
  assert.equal(nav.querySelector("a").getAttribute("href"), "/staff");
  for (const hash of ["#staff", "#ac-staff"]) {
    const old = await accountPage(t, { features: ["nests", "marketplace"], url: `https://pigeonpost.dev/account${hash}` });
    assert.equal(old.navigations.length > 0, true, `${hash} navigates away`);
    assert.equal(old.doc.querySelector("#ac-app"), null, `${hash} draws no account shell`);
  }
});

// ---- the account page's sections and deep links ---------------------------------------------

const shown = (doc) => [...doc.querySelectorAll(".ac-view")].filter((v) => !v.hidden).map((v) => v.getAttribute("data-view"));
const current = (doc) => [...doc.querySelectorAll("#ac-side [aria-current=page]")].map((a) => a.getAttribute("data-route"));
const INTENT = "0123456789abcdef0123456789abcdef";
// A fragment change and a history traversal each queue their hashchange as a task.
const settle = () => new Promise((r) => setTimeout(r, 30));

test("iyzico's credits return (?credits=<intent>#credits) opens the Credits section", async (t) => {
  const p = await accountPage(t, { features: ["nests", "marketplace"], url: `https://pigeonpost.dev/account?credits=${INTENT}#credits` });
  assert.deepEqual(shown(p.doc), ["credits"]);
  assert.deepEqual(current(p.doc), ["credits"]);
  assert.equal(p.window.location.hash, "#credits");
  assert.equal(p.doc.querySelector("#credits").hidden, false);
  assert.match(p.doc.querySelector("#ac-top-title").textContent, /Credits/);
  assert.match(p.doc.querySelector("#ac-credit-chip").textContent, /\$12\.34/, "the top bar shows the balance the card loaded");
  assert.equal(p.doc.querySelector("#ac-credit-chip").hidden, false);
});

test("a credits return without its fragment still lands on Credits, and the hash is restored for reloads", async (t) => {
  const p = await accountPage(t, { features: ["nests", "marketplace"], url: `https://pigeonpost.dev/account?credits=${INTENT}` });
  assert.deepEqual(shown(p.doc), ["credits"]);
  assert.equal(p.window.location.hash, "#credits");
});

test("the postbox's buy_url (/account#credits) opens Credits, and the sidebar follows hash changes and Back", async (t) => {
  const p = await accountPage(t, { features: ["nests", "marketplace"], url: "https://pigeonpost.dev/account#credits" });
  assert.deepEqual(shown(p.doc), ["credits"]);
  p.window.location.hash = "#billing";
  await settle();
  assert.deepEqual(shown(p.doc), ["billing"]);
  assert.deepEqual(current(p.doc), ["billing"]);
  assert.ok(p.doc.querySelector("#ac-billing"), "cards stay mounted while hidden");
  p.window.history.back();
  await settle();
  assert.deepEqual(shown(p.doc), ["credits"]);
  assert.deepEqual(current(p.doc), ["credits"]);
});

test("no hash opens the overview, with the plan, handles, mailboxes and credit balance", async (t) => {
  const p = await accountPage(t, { features: ["nests", "marketplace"] });
  assert.deepEqual(shown(p.doc), ["overview"]);
  assert.equal(p.window.location.hash, "");
  assert.equal(p.doc.querySelector("#ac-dash-plan").textContent, "No plan");
  assert.equal(p.doc.querySelector("#ac-dash-handles").textContent, "0");
  assert.equal(p.doc.querySelector("#ac-dash-boxes").textContent, "0");
  assert.match(p.doc.querySelector("#ac-dash-credit").textContent, /\$12\.34/);
});

test("the apps' #products and #nests links open the Nests tab; #services opens the catalog tab", async (t) => {
  for (const hash of ["#products", "#nests"]) {
    const p = await accountPage(t, { features: ["nests", "marketplace"], url: `https://pigeonpost.dev/account${hash}` });
    assert.deepEqual(shown(p.doc), ["products"]);
    assert.deepEqual(current(p.doc), ["nests"]);
    assert.equal(p.doc.querySelector("#pp-tab-nests").getAttribute("aria-selected"), "true");
  }
  const s = await accountPage(t, { features: ["nests", "marketplace"], url: "https://pigeonpost.dev/account#services" });
  assert.deepEqual(shown(s.doc), ["products"]);
  assert.equal(s.doc.querySelector("#pp-tab-services").getAttribute("aria-selected"), "true");
  s.window.location.hash = "#inbox";
  await settle();
  assert.equal(s.doc.querySelector("#pp-tab-inbox").getAttribute("aria-selected"), "true");
});

test("a step-up sign-in comes back to the publisher console it left", async (t) => {
  const p = await accountPage(t, { features: ["nests", "marketplace"], before: (w) => w.localStorage.setItem("pp_mp_return", "publisher") });
  assert.deepEqual(shown(p.doc), ["publish"]);
  assert.equal(p.window.location.hash, "#publish");
  assert.equal(p.window.localStorage.getItem("pp_mp_return"), null);
});

test("without the marketplace, Credits and Publish leave the sidebar and their sections explain why", async (t) => {
  const p = await accountPage(t, { features: ["nests"], url: "https://pigeonpost.dev/account#credits" });
  assert.deepEqual(shown(p.doc), ["credits"]);
  assert.equal(p.doc.querySelector('#ac-side [data-route="credits"]').hidden, true);
  assert.equal(p.doc.querySelector('#ac-side [data-route="publish"]').hidden, true);
  assert.equal(p.doc.querySelector("#ac-nav-staff").hidden, true, "staff navigation only for staff");
  assert.match(p.doc.querySelector("#ac-empty-credits").textContent, /marketplace/);
  assert.equal(p.doc.querySelector("#ac-credit-chip").hidden, true);
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

// ---- credits on a priced operation ----------------------------------------------------------

const PER_JOB = { ...HOSTED, operations: [{ ...HOSTED.operations[0], price: { amount_cents: 2500, currency: "USD", term: "per_job", funding: "credits", on_sale: true } }] };

function pricedPage(t, { available = 10000, run, jobs = [] } = {}) {
  const bridge = [];
  const p = catalogPage(t, {
    detail: PER_JOB, listings: [{ ...LISTING, hosting: "publisher" }],
    extra: (method, path, body) => {
      if (method === "GET" && path === "/v1/marketplace/hosted-jobs") return { data: jobs };
      if (method === "POST" && path === "/v1/marketplace/hosted-jobs") return run ? run(body) : { data: { job_id: "job_1" } };
      if (method === "POST" && /\/cancel$/.test(path)) { jobs[0].execution = "cancelled"; jobs[0].charge.status = "released"; return { data: jobs[0] }; }
      return undefined;
    },
    deps: { bridgeGet: async (path) => { bridge.push(path); if (path === "/v1/credits/balance") return { available_cents: available, held_cents: 0 }; return {}; } },
  });
  p.bridge = bridge;
  return p;
}

test("a priced operation shows its price and the buyer's credit, and runs from that credit", async (t) => {
  const p = pricedPage(t);
  await openDetail(p);
  await tick();
  assert.match(p.$("#mp-run-0-price").textContent, /Price: \$25\.00 per run, paid from your credit/);
  assert.match(p.$("#mp-run-0-balance").textContent, /Your credit: \$100\.00/);
  assert.equal(p.$("#mp-run-0-go").disabled, false);
  assert.equal(p.$("#mp-run-0-buy"), null);
  assert.ok(p.bridge.includes("/v1/credits/balance"));
  p.$("#mp-run-0-role").value = "CTO";
  p.submit(p.$("#mp-run-0"));
  await tick();
  assert.ok(p.calls.find((c) => c.method === "POST" && c.path === "/v1/marketplace/hosted-jobs"));
  assert.match(p.$("#mp-cat-status").textContent, /\$25\.00 is reserved from your credit/);
});

test("a short balance offers Buy credits instead of a run, and a 402 says so too", async (t) => {
  const p = pricedPage(t, { available: 1000 });
  await openDetail(p);
  await tick();
  assert.equal(p.$("#mp-run-0-go").disabled, true);
  assert.match(p.$("#mp-run-0-short").textContent, /You need \$15\.00 more/);
  assert.equal(p.$("#mp-run-0-buy").getAttribute("href"), "#credits");
  assert.match(p.$("#mp-run-0-buy").textContent, /Buy credits/);

  const q = pricedPage(t, { run: () => { throw fail(402, "insufficient_credit", { available_cents: 0, required_cents: 2500 }); } });
  await openDetail(q);
  await tick();
  q.$("#mp-run-0-role").value = "CTO";
  q.submit(q.$("#mp-run-0"));
  await tick();
  assert.match(q.$("#mp-cat-status").textContent, /Your credit is too low for this run/);
  assert.ok(q.$("#mp-cat-status a[href='#credits']"));
});

test("a run's charge is shown in the list and a running job can be cancelled", async (t) => {
  const jobs = [{ job_id: "job_1", listing_id: "lst_a", operation: "hr.research", execution: "running", created_at: "2026-11-11T10:00:00Z", result: null,
    charge: { amount_cents: 2500, currency: "USD", status: "held" } }];
  const p = pricedPage(t, { jobs });
  await openDetail(p);
  await tick();
  assert.match(p.$('#mp-jobs [data-job="job_1"]').textContent, /\$25\.00 reserved/);
  p.$('[data-cancel-job="job_1"]').click();
  await tick();
  assert.ok(p.calls.find((c) => c.method === "POST" && c.path === "/v1/marketplace/hosted-jobs/job_1/cancel"));
  assert.match(p.$("#mp-cat-status").textContent, /Cancelled\. The reserved credit comes back/);
  assert.match(p.$('#mp-jobs [data-job="job_1"]').textContent, /returned to your credit/);
  assert.equal(p.$('[data-cancel-job="job_1"]'), null);
});

test("a free operation of a publisher-hosted listing never asks for credit", async (t) => {
  const free = catalogPage(t, { detail: HOSTED, listings: [{ ...LISTING, hosting: "publisher" }], extra: (method, path) => (method === "GET" && path === "/v1/marketplace/hosted-jobs" ? { data: [] } : undefined),
    deps: { bridgeGet: async (path) => { if (path.startsWith("/v1/credits")) throw new Error("no credit call expected"); return {}; } } });
  await openDetail(free);
  await tick();
  assert.equal(free.$("#mp-run-0-price"), null);
  assert.equal(free.$("#mp-run-0-go").disabled, false);
});

test("a publisher prices each operation of a publisher-hosted listing per job, in USD", async (t) => {
  const ops = [{ name: "hr.research", title: "HR research", price: null }];
  const p = publisherPage(t, {
    listings: [{ id: "lst_1", title: "Wodo AI HR", summary: "A", visibility: "public", status: "published", version: "1.0.0", hosting: "publisher",
      price: { amount_minor: 0, currency: "TRY", term: "one_off", funding: "test_credits" } }],
    extra: (m, path, body) => {
      if (m === "GET" && path === "/v1/marketplace/listings/lst_1") return { data: { id: "lst_1", hosting: "publisher", operations: ops } };
      if (m === "PUT" && path === "/v1/publishers/pub_1/listings/lst_1/operations/hr.research/price") {
        ops[0].price = { amount_cents: body.amount_cents, currency: "USD", term: "per_job", on_sale: true };
        return { data: { listing_id: "lst_1", operation: "hr.research", price: ops[0].price, offer: null } };
      }
      return undefined;
    },
  });
  await tick();
  p.$('[data-price="lst_1"]').click();
  await tick();
  assert.equal(p.$("#mp-price-lst_1"), null, "no one-off or monthly price for a publisher-hosted listing");
  assert.match(p.$("#mp-jobprice-lst_1").textContent, /Pigeonpost keeps 10% of each job/);
  assert.match(p.$("#mp-jobprice-lst_1-0").textContent, /HR research: price per job.*Now: Free/s);
  p.$("#mp-jobprice-lst_1-0-amount").value = "25";
  p.submit(p.$("#mp-jobprice-lst_1-0"));
  await tick();
  const put = p.calls.find((c) => c.method === "PUT");
  assert.deepEqual(put.body, { amount_cents: 2500, currency: "USD" });
  assert.match(p.$("#mp-pub-status").textContent, /Price saved/);
  assert.match(p.$("#mp-jobprice-lst_1-0").textContent, /Now: \$25\.00 per job/);
});

test("staff see a publisher-hosted integration's per-job prices in review", async (t) => {
  const p = staffPage(t, { extra: (m, path) => (path === "/v1/staff/reviews/rev_1" ? { data: { id: "rev_1", status: "open", release_id: "rel_1", version: "1.0.0", release_digest: "sha256:abc", security_state: "normal", first_party: false, kind: "integration", stages: [],
    integration: { service_address: "/wodo/hr", service_mailbox: "/k/x", service_owned: true, operations: [{ name: "hr.research", title: "HR research" }],
      prices: [{ listing_id: "lst_1", operation: "hr.research", amount_cents: 2500, currency: "USD", term: "per_job" }] } } } : undefined) });
  await tick();
  p.$('[data-review="rev_1"] button').click();
  await tick();
  assert.match(p.$("#mp-sr-integration").textContent, /Service address \/wodo\/hr/);
  assert.match(p.$("#mp-sr-prices").textContent, /hr\.research \$25\.00/);
});

test("a result that could not reach the inbox says it is available on the job page", async (t) => {
  const jobs = [{ job_id: "job_9", listing_id: "lst_a", operation: "hr.research", execution: "succeeded", delivery: "undeliverable", created_at: "2026-11-11T10:00:00Z", result: { summary: "ok", files: [] }, charge: null }];
  const p = catalogPage(t, { detail: HOSTED, listings: [{ ...LISTING, hosting: "publisher" }], extra: (method, path) => (method === "GET" && path === "/v1/marketplace/hosted-jobs" ? { data: jobs } : undefined) });
  await openDetail(p);
  await tick();
  const row = p.$('#mp-jobs [data-job="job_9"]').textContent;
  assert.match(row, /available on the job page/);
  assert.doesNotMatch(row, /in your inbox$/);
});

// ---- the marketplace look: logos, cards, catalog controls, coming soon ------------------------

const LOGO = (id) => ({ hash: "0123456789abcdef", urls: Object.fromEntries(["64", "128", "512"].map((n) => [n, `/v1/marketplace/listings/${id}/logo/0123456789abcdef/${n}.webp`])) });
const SAMPLE = (id, title, category, extra = {}) => ({
  ...LISTING, id, title, category, summary: `${title} summary`, coming_soon: true, sample: true, logo: null,
  publisher: { display_name: "Pigeonpost", first_party: true, verified: true }, ...extra,
});

test("default logos are the title's initials on a tone derived from the id, the same every time", async (t) => {
  const p = catalogPage(t);
  const MP = p.window.PPMarketplace;
  assert.equal(MP.initials("Contract reviewer"), "CR");
  assert.equal(MP.initials("Translator (TR/EN/DE)"), "Tr");
  assert.equal(MP.initials("  meeting   minutes writer "), "MM");
  assert.equal(MP.initials("Çeviri"), "Çe");
  assert.equal(MP.initials(""), "?");
  const tone = MP.logoTone("lst_sample_translator");
  assert.ok(Number.isInteger(tone) && tone >= 0 && tone < MP.TONES);
  assert.equal(MP.logoTone("lst_sample_translator"), tone, "deterministic");
  const tones = new Set(["lst_a", "lst_b", "lst_c", "lst_d", "lst_e", "lst_f", "lst_g", "lst_h"].map((id) => MP.logoTone(id)));
  assert.ok(tones.size >= 4, "ids spread over the tones");
  const tile = MP.logo({ id: "lst_sample_translator", title: "Translator (TR/EN/DE)" }, { size: 48 });
  assert.equal(tile.dataset.logo, "default");
  assert.ok(tile.classList.contains(`mp-tone-${tone}`));
  assert.equal(tile.style.getPropertyValue("--mp-hue"), String(MP.TONE_HUES[tone]), "the gradient starts from the id's hue");
  assert.equal(tile.textContent, "Tr");
  assert.equal(tile.getAttribute("aria-hidden"), "true", "decorative next to its title");
  const named = MP.logo({ id: "lst_x", title: "X ray" }, { size: 96, alt: "X ray logo" });
  assert.equal(named.getAttribute("role"), "img");
  assert.equal(named.getAttribute("aria-label"), "X ray logo");
  // An uploaded logo is an image from the postbox; anything else falls back to the default.
  const img = MP.logo({ id: "lst_i", title: "Invoice reader", logo: LOGO("lst_i") }, { size: 48 });
  assert.equal(img.tagName, "IMG");
  assert.equal(img.getAttribute("src"), "https://postbox.pigeonpost.dev/v1/marketplace/listings/lst_i/logo/0123456789abcdef/128.webp");
  assert.equal(img.getAttribute("alt"), "");
  const big = MP.logo({ id: "lst_i", title: "Invoice reader", logo: LOGO("lst_i") }, { size: 96, alt: "Invoice reader logo" });
  assert.match(big.getAttribute("src"), /\/512\.webp$/);
  assert.equal(big.getAttribute("alt"), "Invoice reader logo");
  const hostile = MP.logo({ id: "lst_h", title: "Hostile", logo: { urls: { 128: "javascript:alert(1)", 512: "//evil.example/x.webp" } } }, { size: 48 });
  assert.equal(hostile.dataset.logo, "default");
});

test("cards show the logo, the publisher with its verified badge, the category, hosting and price", async (t) => {
  const listings = [
    { ...LISTING, category: "legal", publisher: { display_name: "Acme", first_party: false, verified: true }, price: { amount_minor: 0 } },
    { ...LISTING, id: "lst_i", title: "Invoice reader", category: "finance", logo: LOGO("lst_i"), hosting: "publisher", price_from: { amount_cents: 25, currency: "USD", unit: "run" }, publisher: { display_name: "Ledger Co", first_party: false, verified: false } },
    { ...LISTING, id: "lst_m", title: "Monthly digest", price: { amount_minor: 1000, currency: "USD", term: "monthly", on_sale: true } },
  ];
  const p = catalogPage(t, { listings });
  await openServices(p);
  const a = p.$('[data-listing="lst_a"]');
  assert.equal(a.querySelector(".mp-logo").dataset.logo, "default");
  assert.equal(a.querySelector(".mp-logo-letters").textContent, "HH");
  assert.equal(a.querySelector(".mp-publisher-name").textContent, "Acme");
  assert.ok(a.querySelector(".mp-verified"), "a certified publisher has the verified badge");
  assert.equal(a.querySelector(".mp-card-category").textContent, "Legal");
  assert.equal(a.querySelector(".mp-card-hosting").textContent, "Runs in your Nest");
  assert.equal(a.querySelector(".mp-card-price").textContent, "Free");
  assert.equal(a.querySelector(".mp-card-summary").textContent, "Answers HR questions");
  assert.equal(a.querySelector(".mp-ribbon"), null);
  const i = p.$('[data-listing="lst_i"]');
  assert.equal(i.querySelector("img.mp-logo").getAttribute("src"), "https://postbox.pigeonpost.dev/v1/marketplace/listings/lst_i/logo/0123456789abcdef/128.webp");
  assert.equal(i.querySelector(".mp-verified"), null);
  assert.equal(i.querySelector(".mp-card-hosting").textContent, "Hosted by the publisher");
  assert.equal(i.querySelector(".mp-card-price").textContent, "$0.25 per run");
  assert.equal(p.$('[data-listing="lst_m"] .mp-card-price').textContent, "$10.00/month");
  // The whole card opens the listing through its title, which names it for screen readers.
  const open = i.querySelector(".mp-card-link");
  assert.equal(open.tagName, "BUTTON");
  assert.equal(open.getAttribute("aria-label"), "Details for Invoice reader");
});

test("coming-soon samples wear a ribbon, fill the featured row, and filter and sort by category and price", async (t) => {
  const listings = [
    { ...LISTING, category: "productivity", published_at: "2026-11-01T00:00:00Z", price: { amount_minor: 2500, currency: "USD", term: "one_off", on_sale: true } },
    { ...LISTING, id: "lst_f", title: "Free notes", category: "productivity", published_at: "2026-11-05T00:00:00Z", price: { amount_minor: 0 } },
    SAMPLE("lst_sample_contract_reviewer", "Contract reviewer", "legal", { featured: true, featured_rank: 1 }),
    SAMPLE("lst_sample_translator", "Translator (TR/EN/DE)", "language", { featured: true, featured_rank: 3 }),
    SAMPLE("lst_sample_meeting_minutes", "Meeting minutes writer", "productivity", { featured: true, featured_rank: 2 }),
    SAMPLE("lst_sample_code_review", "Code review assistant", "developer"),
  ];
  const p = catalogPage(t, { listings });
  await openServices(p);
  const soon = p.$('#mp-grid [data-listing="lst_sample_contract_reviewer"]');
  assert.ok(soon.classList.contains("mp-card-coming"));
  assert.equal(soon.querySelector(".mp-ribbon").textContent, "Coming soon");
  assert.equal(soon.querySelector(".mp-card-price"), null, "the ribbon says it; no second label");
  assert.equal(soon.querySelector(".mp-card-sample").textContent, "Sample");
  assert.equal(soon.querySelector(".mp-publisher-name").textContent, "Pigeonpost");
  const featured = [...p.doc.querySelectorAll("#mp-featured [data-listing]")].map((n) => n.dataset.listing);
  assert.deepEqual(featured, ["lst_sample_contract_reviewer", "lst_sample_meeting_minutes", "lst_sample_translator"], "featured rank order");
  // Featured order in the grid: what is available now first (newest first), then what is coming,
  // featured ones leading.
  const grid = () => [...p.doc.querySelectorAll("#mp-grid [data-listing]")].map((n) => n.dataset.listing);
  assert.deepEqual(grid(), ["lst_f", "lst_a", "lst_sample_contract_reviewer", "lst_sample_meeting_minutes", "lst_sample_translator", "lst_sample_code_review"]);
  // Categories offered are the ones present; choosing one narrows the grid and hides the featured row.
  const cat = p.$("#mp-cat-category");
  assert.deepEqual([...cat.options].map((o) => o.value), ["", "productivity", "legal", "language", "developer"]);
  cat.value = "productivity";
  cat.dispatchEvent(new p.window.Event("change", { bubbles: true }));
  assert.equal(p.$("#mp-featured"), null);
  assert.deepEqual(grid(), ["lst_f", "lst_a", "lst_sample_meeting_minutes"]);
  const sort = p.$("#mp-cat-sort");
  sort.value = "price";
  sort.dispatchEvent(new p.window.Event("change", { bubbles: true }));
  assert.deepEqual(grid(), ["lst_f", "lst_a", "lst_sample_meeting_minutes"], "cheapest first, coming soon last");
  const sort2 = p.$("#mp-cat-sort");
  sort2.value = "newest";
  sort2.dispatchEvent(new p.window.Event("change", { bubbles: true }));
  assert.equal(grid()[0], "lst_f");
  const cat2 = p.$("#mp-cat-category");
  cat2.value = "legal";
  cat2.dispatchEvent(new p.window.Event("change", { bubbles: true }));
  assert.deepEqual(grid(), ["lst_sample_contract_reviewer"]);
});

test("a coming-soon listing's page offers a disabled Coming soon button and nothing to install", async (t) => {
  const detail = { ...DETAIL, ...SAMPLE("lst_a", "HR helper", "recruiting"), description: "Screens applications.\n\nThis is a sample listing from Pigeonpost.", operations: [], config_schema: SCHEMA };
  const p = catalogPage(t, { detail });
  await openDetail(p);
  const go = p.$("#mp-coming-soon");
  assert.ok(go, "the primary button");
  assert.equal(go.textContent, "Coming soon");
  assert.equal(go.disabled, true);
  assert.equal(go.getAttribute("aria-describedby"), "mp-coming-note");
  assert.equal(p.$("#mp-install"), null);
  assert.equal(p.$("#mp-buy"), null);
  assert.equal(p.$(".mp-detail-hero .mp-logo").getAttribute("aria-label"), "HR helper logo");
  assert.ok(p.$(".mp-detail-hero .mp-chip-soon"));
  assert.equal(p.$("#mp-publisher-card .mp-publisher-name").textContent, "Pigeonpost");
  assert.equal(p.$("#mp-support-link").getAttribute("href"), "/app-support.html");
  assert.equal(p.$("#mp-privacy-link").getAttribute("href"), "/privacy");
  assert.equal(p.calls.some((c) => c.method !== "GET"), false, "nothing was sent");
});

test("a listing's page shows its links, facts and screenshots, and installs as before", async (t) => {
  const detail = { ...DETAIL, logo: LOGO("lst_a"), category: "hr", support_url: "https://acme.example/help", privacy_url: "https://acme.example/privacy",
    screenshots: [{ url: "https://acme.example/s1.png", alt: "The question form" }, { url: "javascript:alert(1)" }],
    publisher: { display_name: "Acme", first_party: false, verified: true } };
  const p = catalogPage(t, { detail });
  await openDetail(p);
  assert.match(p.$(".mp-detail-hero img.mp-logo").getAttribute("src"), /\/512\.webp$/);
  assert.equal(p.$(".mp-detail-hero img.mp-logo").getAttribute("alt"), "HR helper logo");
  assert.equal(p.$("#mp-support-link").getAttribute("href"), "https://acme.example/help");
  assert.equal(p.$("#mp-support-link").getAttribute("rel"), "noopener noreferrer");
  assert.equal(p.$("#mp-privacy-link").getAttribute("href"), "https://acme.example/privacy");
  const shots = [...p.doc.querySelectorAll(".mp-shots img")];
  assert.equal(shots.length, 1, "only https screenshots");
  assert.equal(shots[0].getAttribute("alt"), "The question form");
  assert.ok(p.$("#mp-publisher-card .mp-verified"));
  assert.match(p.$(".mp-facts").textContent, /HR/);
  assert.ok(p.$("#mp-install"), "a free listing still installs");
  assert.equal(p.$("#mp-coming-soon"), null);
});

test("the publisher console uploads a listing logo as the image itself, and can go back to the default", async (t) => {
  const listings = [{ id: "lst_1", title: "HR helper", summary: "s", description: "", status: "draft", visibility: "public", version: "1.0.0", logo: LOGO("lst_1") }];
  const p = publisherPage(t, { listings, extra: (m, path) => (path.endsWith("/logo") ? { data: { listing_id: "lst_1", logo: null } } : undefined) });
  await tick();
  assert.equal(p.$('[data-listing="lst_1"] img.mp-logo').getAttribute("src").endsWith("/128.webp"), true);
  p.$('button[aria-label="Edit HR helper"]').click();
  await tick();
  const form = p.$("#mp-logo-lst_1");
  assert.ok(form);
  const input = p.$("#mp-logo-lst_1-file");
  assert.equal(input.getAttribute("accept"), "image/png,image/jpeg,image/webp");
  const svg = new p.window.File(["<svg/>"], "logo.svg", { type: "image/svg+xml" });
  Object.defineProperty(input, "files", { value: [svg], configurable: true });
  p.submit(form);
  await tick();
  assert.match(p.$("#mp-pub-status").textContent, /SVG isn't accepted/);
  assert.equal(p.calls.some((c) => c.path.endsWith("/logo")), false);
  const png = new p.window.File([new Uint8Array([137, 80, 78, 71])], "logo.png", { type: "image/png" });
  Object.defineProperty(input, "files", { value: [png], configurable: true });
  p.submit(form);
  await tick();
  const put = p.calls.find((c) => c.method === "PUT" && c.path === "/v1/publishers/pub_1/listings/lst_1/logo");
  assert.ok(put, "PUT …/logo");
  assert.equal(put.contentType, "image/png");
  assert.equal(put.body, png, "the file, not JSON");
  if (!p.$("#mp-logo-lst_1-remove")) { p.$('button[aria-label="Edit HR helper"]').click(); await tick(); }
  p.$("#mp-logo-lst_1-remove").click();
  await tick();
  assert.ok(p.calls.find((c) => c.method === "DELETE" && c.path === "/v1/publishers/pub_1/listings/lst_1/logo"));
});

test("the shared products view takes its heading and subtitle from the sidebar item", async (t) => {
  const p = await accountPage(t, { features: ["nests", "marketplace"], url: "https://pigeonpost.dev/account#services" });
  const head = () => p.doc.querySelector("#ac-h-products").textContent;
  const lede = () => p.doc.querySelector("#ac-l-products");
  assert.equal(head(), "Services");
  assert.match(lede().textContent, /verified publishers/);
  assert.equal(lede().hidden, false);
  p.window.location.hash = "#nests";
  p.window.dispatchEvent(new p.window.HashChangeEvent("hashchange"));
  for (let i = 0; i < 10; i++) await tick(1);
  assert.equal(head(), "Nests");
  assert.match(lede().textContent, /workspaces/);
});

test("a publisher-hosted listing's notice is an informational callout, not a warning", async (t) => {
  const p = catalogPage(t, { detail: { ...DETAIL, hosting: "publisher", operations: [] } });
  await openDetail(p);
  const note = p.$("#mp-hosted-notice");
  assert.ok(note.classList.contains("mp-callout"));
  assert.equal(note.classList.contains("pp-warn"), false);
  assert.equal(note.getAttribute("role"), "note");
  assert.equal(note.querySelector("svg").getAttribute("aria-hidden"), "true");
  assert.equal(note.querySelector("strong").textContent, "Hosted by the publisher");
});

test("a live listing that offers a built-in assistant replaces its teaser", async (t) => {
  const listings = [{ ...LISTING, id: "lst_w", title: "Wodo AI HR", category: "hr" }, { ...LISTING, id: "lst_s", title: "AI CRM preview", coming_soon: true }];
  const p = catalogPage(t, { listings });
  await openServices(p);
  const ids = [...p.doc.querySelectorAll("#mp-builtins [data-builtin]")].map((n) => n.dataset.builtin);
  assert.deepEqual(ids, ["pp_personal_assistant", "pp_ai_crm", "pp_shopping_assistant", "pp_marketing_assistant"], "AI HR hidden; a coming-soon listing replaces nothing");
});
