// Marketplace purchases through the bridge, against one fixture standing in for saas_be's
// marketplace finance routes, its member API, and the postbox (member and internal). Synthetic
// tokens and values only; nothing contacts a payment provider.
import assert from "node:assert/strict";
import { once } from "node:events";
import http from "node:http";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";
import { ACCOUNT, fixtureCatalog, json, readBody, spawnBridge, uuid } from "./harness.mjs";

const MP = "/v1/internal/tenants/pigeonpost/marketplace";
const MP_TOKEN = "mp-bridge-fixture-token-0123456789";
const INTERNAL = "bridge-internal-fixture-token-0123456789";
const SVC = "svc_fixture_billing_token_0123456789abcdef";
const MEMBER = "Bearer member-token";
const INSTALL = "ins_aaaaaaaaaaaaaaaa";
const TOKEN1 = "mpa_" + "t".repeat(43);
const TOKEN2 = "mpa_" + "r".repeat(43);
const FORM = "<div id=iyzipay-checkout-form></div>";
const LICENCE = uuid(703);

function fixture() {
  const s = {
    term: "one_off", authStatus: "pending_payment", authorizedAt: null, token: TOKEN1, consumedBy: null,
    calls: [], projections: [], purchases: [], list: [], orgBilling: { can_manage: true }, offers: {},
    profiles: [{ id: uuid(42), account_type: "individual", legal_name: "Ada Buyer", tax_id: null,
      address: { line1: "1 Main St", city: "Austin", postal_code: "73301", country: "United States" } }],
  };
  s.licence = () => ({ licence_id: LICENCE, listing_id: "lst_one", publisher_id: "pub_one", release_line: "v1",
    owner_kind: s.owner?.kind || "account", owner_id: s.owner?.id || ACCOUNT, term: s.term, status: "active", access: true,
    paid_through: s.term === "monthly" ? new Date(Date.now() + 20 * 86400000).toISOString() : null,
    cancel_at_period_end: false, paused_at: null, deletion_due_at: null, revoked_at: null, revoke_reason: null,
    subscription_id: null, ...s.licenceOverride });
  const offerView = (id, b) => ({ offer_id: id, listing_id: b.listing_id, publisher_id: b.publisher_id, release_line: b.release_line,
    title: b.title, term: b.term, currency: b.currency, price_minor: b.price_minor, status: b.status || "active",
    sellable: (b.status || "active") === "active" && (b.term === "one_off" || Boolean(b.provisioned)), provisioned_at: null });
  const authView = (id) => ({ authorization_id: id, quote_id: uuid(700), status: s.authStatus, intent_hash: "ab".repeat(32),
    licence_id: s.authorizedAt ? LICENCE : null, expires_at: new Date(Date.now() + 300000).toISOString(),
    authorized_at: s.authorizedAt, consumed_at: null, consumer_reference: s.consumedBy, checkout_status: "pending" });

  s.server = http.createServer(async (req, res) => {
    const url = new URL(req.url, "http://localhost");
    const p = url.pathname;
    const body = await readBody(req);
    const auth = req.headers.authorization;
    s.calls.push({ method: req.method, path: p, query: url.search, body, auth });
    let m;
    if (p.startsWith(MP)) {
      if (auth !== `Bearer ${MP_TOKEN}`) return json(res, 401, { code: "AUTH_INVALID_TOKEN", message: "Invalid marketplace service token." });
      const r = p.slice(MP.length);
      if ((m = /^\/offers\/([^/]+)$/.exec(r)) && req.method === "PUT") {
        if (body.price_minor % 7 === 0) {
          return json(res, 422, { code: "VALIDATION_FAILED", message: "cannot be invoiced exactly", details: { nearest: [body.price_minor - 1, body.price_minor + 1] } });
        }
        s.offers[m[1]] = { ...s.offers[m[1]], ...body };
        return json(res, 200, offerView(m[1], s.offers[m[1]]));
      }
      if ((m = /^\/offers\/([^/]+)\/provision$/.exec(r))) {
        s.offers[m[1]].provisioned = true;
        return json(res, 200, offerView(m[1], s.offers[m[1]]));
      }
      if (r === "/quotes") {
        return json(res, 201, { quote_id: uuid(700), offer_id: body.offer_id, listing_id: "lst_one", release_line: "v1", term: s.term,
          amount_minor: 12000, currency: "TRY", tax_inclusive: true, intent_hash: "ab".repeat(32), expires_at: new Date(Date.now() + 900000).toISOString() });
      }
      if ((m = /^\/quotes\/([^/]+)\/checkout$/.exec(r))) {
        return json(res, 201, { checkout_id: uuid(701), quote_id: m[1], authorization_id: uuid(702), authorization_token: TOKEN1,
          kind: s.term === "monthly" ? "subscription" : "one_off", status: "pending", amount_minor: 12000, currency: "TRY",
          checkout_form_content: FORM, payment_page_url: null, subscription_id: null, expires_at: new Date(Date.now() + 1800000).toISOString() });
      }
      if ((m = /^\/authorizations\/([0-9a-f-]{36})$/.exec(r))) return json(res, 200, authView(m[1]));
      if (r === "/authorizations/consume") {
        if (body.token !== s.token) return json(res, 404, { code: "RESOURCE_NOT_FOUND", message: "Payment authorization not found." });
        if (s.consumedBy && s.consumedBy !== body.consumer_reference) return json(res, 409, { code: "IDEMPOTENCY_CONFLICT", message: "cannot be consumed" });
        if (!s.authorizedAt) return json(res, 409, { code: "IDEMPOTENCY_CONFLICT", message: "cannot be consumed" });
        const replay = Boolean(s.consumedBy);
        s.consumedBy = body.consumer_reference;
        s.authStatus = "consumed";
        return json(res, 200, { ...authView(body.authorization_id), replay, licence: s.licence() });
      }
      if ((m = /^\/authorizations\/([^/]+)\/reissue$/.exec(r))) {
        s.token = TOKEN2;
        return json(res, 200, { ...authView(m[1]), authorization_token: TOKEN2 });
      }
      if (r === "/licences") return json(res, 200, { items: [s.licence()] });
      if (r === `/licences/${LICENCE}`) return json(res, 200, s.licence());
      if (r === `/licences/${LICENCE}/payments`) {
        return json(res, 200, { items: [{ payment_id: uuid(704), kind: "sale", status: "settled", amount_minor: 12000, tax_minor: 2000,
          currency: "TRY", invoice_id: uuid(705), period_start: null, period_end: null, settled_at: new Date().toISOString(), reversal_kind: null }] });
      }
      if (r === `/licences/${LICENCE}/cancel`) {
        s.licenceOverride = { ...s.licenceOverride, status: "canceling", cancel_at_period_end: true };
        return json(res, 200, s.licence());
      }
      return json(res, 404, { message: `unexpected ${req.method} ${r}` });
    }
    // ---- postbox internal
    if ((m = /^\/internal\/v1\/marketplace\/licences\/([^/]+)$/.exec(p))) {
      if (auth !== `Bearer ${SVC}`) return json(res, 401, { error: { code: "invalid_service_token" } });
      s.projections.push({ id: m[1], body });
      return json(res, 200, { data: { installation_id: body.installation_id, licence_id: m[1], runtime: body.access ? "running" : "stopped" } });
    }
    if (auth !== MEMBER) return json(res, 401, { message: "Unauthorized" });
    // ---- postbox member
    if (p === "/v1/me/handles") return json(res, 200, { account: ACCOUNT, handles: [] });
    if (p === "/v1/me/entitlements") return json(res, 200, { account: ACCOUNT, entitlements: [], web_plan: { capacity: 0, used: 0, names: [] } });
    if (/^\/v1\/orgs\/[^/]+\/billing$/.test(p)) return json(res, 200, s.orgBilling);
    if (p === "/v1/marketplace/purchases") {
      s.purchases.push(body);
      if (s.purchaseRefusal) return json(res, s.purchaseRefusal.status, { error: { code: s.purchaseRefusal.code, message: "refused" } });
      return json(res, 201, { data: { purchase_id: INSTALL, installation_id: INSTALL, listing_id: body.listing_id,
        owner_kind: body.owner_kind, owner_id: body.owner_id, status: "open",
        offer: { offer_id: "mpo_fixture", term: s.term, currency: "TRY", amount_minor: 12000, release_line: "v1", title: "Team digest" },
        intent: { action: "install", listing_id: body.listing_id, release_line: "v1", owner_kind: body.owner_kind, owner_id: body.owner_id, installation_id: INSTALL } } });
    }
    // ---- saas_be member
    if (p === "/v1/profile") return json(res, 200, { id: uuid(800) });
    if (p === "/v1/billing/profiles") return json(res, 200, { data: s.profiles });
    if (p === "/v1/integrations/iyzico/subscriptions") return json(res, 200, { items: s.list });
    return json(res, 404, { message: `unexpected ${req.method} ${p}` });
  });
  return s;
}

async function setup(t, env = {}) {
  const s = fixture();
  s.server.listen(0, "127.0.0.1");
  await once(s.server, "listening");
  t.after(() => { s.server.closeAllConnections(); s.server.close(); });
  const upstream = `http://127.0.0.1:${s.server.address().port}`;
  const bridge = await spawnBridge(t, {
    MASAAS_SAAS_API_URL: upstream, PIGEONPOST_POSTBOX_URL: upstream, POSTBOX_INTERNAL_URL: upstream,
    POSTBOX_SERVICE_TOKEN: SVC, SAAS_INTERNAL_TOKEN: "saas-internal-fixture", PIGEONPOST_CATALOG_FILE: fixtureCatalog().file,
    OPEN_CURRENCIES: "USD,EUR,TRY", SAAS_MARKETPLACE_TOKEN: MP_TOKEN, BRIDGE_INTERNAL_TOKEN: INTERNAL,
    VIES_CHECK_URL: `${upstream}/vies`, ...env,
  });
  const call = async (method, path, body, auth = MEMBER, prefix = "/api") => {
    const r = await fetch(`${bridge.base}${prefix}${path}`, { method,
      headers: { authorization: auth, "x-real-ip": "203.0.113.9", ...(body ? { "content-type": "application/json" } : {}) },
      body: body ? JSON.stringify(body) : undefined });
    return { status: r.status, body: await r.json().catch(() => null) };
  };
  const internal = (method, path, body, auth = `Bearer ${INTERNAL}`) => call(method, path, body, auth, "");
  return { s, bridge, call, internal };
}

const buy = (call, intent, extra = {}) =>
  call("POST", "/v1/marketplace/checkout", { intent_id: intent, listing_id: "lst_one", billing_profile_id: uuid(42), locale: "en", ...extra });
const consumeBody = (s, patch = {}) => ({ purchase_id: INSTALL, intent: { ...s.purchaseIntent, ...patch }, consumer_reference: INSTALL });
const count = (s, method, suffix) => s.calls.filter((c) => c.method === method && c.path === MP + suffix).length;

/// A paid one-off purchase, ready to be consumed.
async function paid(t, env, term = "one_off") {
  const ctx = await setup(t, env);
  ctx.s.term = term;
  const r = await buy(ctx.call, "a".repeat(32));
  assert.equal(r.status, 200, JSON.stringify(r.body));
  ctx.s.purchaseIntent = { action: "install", listing_id: "lst_one", release_line: "v1", owner_kind: "account", owner_id: ACCOUNT, installation_id: INSTALL };
  ctx.s.authStatus = "authorized";
  ctx.s.authorizedAt = new Date().toISOString();
  return ctx;
}

test("offers are mirrored, monthly ones provisioned, and an uninvoiceable price names the nearest", async (t) => {
  const { s, call, internal } = await setup(t);
  const offer = { listing_id: "lst_one", publisher_id: "pub_one", release_line: "v1", title: "Team digest", term: "monthly", currency: "TRY", price_minor: 12000 };
  let r = await internal("PUT", "/internal/marketplace/offers/mpo_one", offer);
  assert.equal(r.status, 200, JSON.stringify(r.body));
  assert.equal(r.body.offer.sellable, true);
  assert.equal(count(s, "PUT", "/offers/mpo_one"), 1);
  assert.equal(count(s, "POST", "/offers/mpo_one/provision"), 1);
  // One-off offers need no provisioning; retiring sends the same immutable fields.
  r = await internal("PUT", "/internal/marketplace/offers/mpo_two", { ...offer, term: "one_off" });
  assert.equal(r.status, 200);
  assert.equal(count(s, "POST", "/offers/mpo_two/provision"), 0);
  r = await internal("PUT", "/internal/marketplace/offers/mpo_two", { ...offer, term: "one_off", status: "retired" });
  assert.deepEqual([r.status, r.body.offer.status], [200, "retired"]);

  r = await internal("PUT", "/internal/marketplace/offers/mpo_bad", { ...offer, price_minor: 12005 });
  assert.equal(r.status, 422);
  assert.equal(r.body.code, "price_not_invoiceable");
  assert.deepEqual(r.body.nearest, [12004, 12006]);
  // Validation before any upstream call, and only the postbox's token opens these routes.
  const before = s.calls.length;
  assert.equal((await internal("PUT", "/internal/marketplace/offers/mpo_x", { ...offer, term: "yearly" })).status, 400);
  assert.equal((await internal("PUT", "/internal/marketplace/offers/mpo_x", { ...offer, price_minor: 0 })).status, 400);
  assert.equal((await internal("PUT", "/internal/marketplace/offers/mpo_x", { ...offer, wallet: true })).status, 400);
  assert.equal((await internal("PUT", "/internal/marketplace/offers/mpo_one", offer, "Bearer wrong-token-0123456789012345")).status, 401);
  assert.equal((await internal("PUT", "/internal/marketplace/offers/mpo_one", offer, MEMBER)).status, 401);
  // Never through the public proxy prefix, even with the right token.
  assert.equal((await call("PUT", "/internal/marketplace/offers/mpo_one", offer, `Bearer ${INTERNAL}`)).status, 404);
  assert.equal(s.calls.length, before);
});

test("a one-off checkout is quoted server-side, keeps no form on disk, and reports paid", async (t) => {
  const { s, bridge, call } = await setup(t);
  const intent = "a".repeat(32);
  const r = await buy(call, intent);
  assert.equal(r.status, 200, JSON.stringify(r.body));
  assert.equal(r.body.checkout_form_content, FORM);
  assert.equal(r.body.installation_id, INSTALL);
  assert.equal(r.body.amount_minor, 12000);
  assert.deepEqual(s.purchases[0], { listing_id: "lst_one", owner_kind: "account", owner_id: ACCOUNT, intent_id: intent });
  const quote = s.calls.find((c) => c.path === `${MP}/quotes`);
  assert.deepEqual(quote.body.buyer, { owner_kind: "account", owner_id: ACCOUNT, user_id: uuid(800), billing_profile_id: uuid(42) });
  assert.equal(quote.body.intent.installation_id, INSTALL);
  assert.equal(quote.body.offer_id, "mpo_fixture");
  const checkout = s.calls.find((c) => c.path === `${MP}/quotes/${uuid(700)}/checkout`);
  assert.deepEqual(checkout.body, { return_url: `https://pigeonpost.dev/account?mp_purchase=${intent}`, buyer_ip: "203.0.113.9", locale: "en" });

  // A replay returns the same checkout and quotes nothing new; other details are refused.
  const again = await buy(call, intent);
  assert.equal(again.status, 200);
  assert.equal(again.body.checkout_form_content, FORM);
  assert.equal(count(s, "POST", "/quotes"), 1);
  assert.equal((await buy(call, intent, { listing_id: "lst_two" })).status, 409);
  const saved = readFileSync(join(bridge.stateDir, "marketplace-state.json"), "utf8");
  assert.ok(saved.includes(intent) && !saved.includes("iyzipay"));

  let st = await call("GET", `/v1/marketplace/checkout/${intent}`);
  assert.deepEqual([st.status, st.body.status], [200, "pending"]);
  s.authStatus = "authorized"; s.authorizedAt = new Date().toISOString();
  st = await call("GET", `/v1/marketplace/checkout/${intent}`);
  assert.equal(st.body.status, "paid");
  assert.equal(st.body.licence_id, LICENCE);
  assert.ok(Array.isArray(st.body.receipt.lines));
  // Someone else's intent is not there.
  assert.equal((await call("GET", `/v1/marketplace/checkout/${"f".repeat(32)}`)).status, 404);
});

test("EU and UK consumers are refused before anything is reserved or quoted", async (t) => {
  const { s, bridge, call } = await setup(t);
  s.profiles[0].address.country = "Germany";
  const r = await buy(call, "b".repeat(32));
  assert.equal(r.status, 403);
  assert.equal(r.body.code, "eu_uk_consumer_web_unavailable");
  assert.equal(count(s, "POST", "/quotes"), 0);
  let saved = "";
  try { saved = readFileSync(join(bridge.stateDir, "marketplace-state.json"), "utf8"); } catch { /* never written */ }
  assert.ok(!saved.includes("b".repeat(32)));
});

test("the postbox's refusal stops the purchase", async (t) => {
  const { s, call } = await setup(t);
  s.purchaseRefusal = { status: 409, code: "not_on_sale" };
  const r = await buy(call, "c".repeat(32));
  assert.deepEqual([r.status, r.body.code], [409, "not_on_sale"]);
  assert.equal(count(s, "POST", "/quotes"), 0);
});

test("consume admits exactly the bound intent, once, and replays idempotently", async (t) => {
  const { s, internal } = await paid(t);
  let r = await internal("POST", "/internal/marketplace/authorizations/consume", consumeBody(s));
  assert.equal(r.status, 200, JSON.stringify(r.body));
  assert.equal(r.body.replay, false);
  assert.equal(r.body.licence.licence_id, LICENCE);
  const sent = s.calls.filter((c) => c.path === `${MP}/authorizations/consume`);
  assert.equal(sent.length, 1);
  assert.deepEqual(sent[0].body, { authorization_id: uuid(702), token: TOKEN1, intent: s.purchaseIntent, consumer_reference: INSTALL });
  r = await internal("POST", "/internal/marketplace/authorizations/consume", consumeBody(s));
  assert.deepEqual([r.status, r.body.replay], [200, true]);
  // Another installation, listing or owner is refused here, before saas_be is asked.
  for (const patch of [{ installation_id: "ins_bbbbbbbbbbbbbbbb" }, { listing_id: "lst_two" }, { owner_id: "acct_someoneelse0" }]) {
    r = await internal("POST", "/internal/marketplace/authorizations/consume", consumeBody(s, patch));
    assert.equal(r.status, 409, JSON.stringify(patch));
    assert.equal(r.body.code, "intent_mismatch");
  }
  r = await internal("POST", "/internal/marketplace/authorizations/consume", { ...consumeBody(s), consumer_reference: "ins_cccccccccccccccc" });
  assert.deepEqual([r.status, r.body.code], [409, "intent_mismatch"]);
  assert.equal(s.calls.filter((c) => c.path === `${MP}/authorizations/consume`).length, 2);
  assert.equal((await internal("POST", "/internal/marketplace/authorizations/consume", { ...consumeBody(s), purchase_id: "ins_dddddddddddddddd" })).status, 404);
});

test("an unpaid purchase is refused and a lapsed window is reissued once", async (t) => {
  const { s, internal } = await paid(t);
  s.authStatus = "pending_payment"; s.authorizedAt = null;
  let r = await internal("POST", "/internal/marketplace/authorizations/consume", consumeBody(s));
  assert.equal(r.status, 402);
  assert.equal(r.body.code, "payment_pending");
  // Paid, but the token no longer matches (rotated, window lapsed): reissue and consume.
  s.authStatus = "expired"; s.authorizedAt = new Date().toISOString(); s.token = "mpa_" + "z".repeat(43);
  r = await internal("POST", "/internal/marketplace/authorizations/consume", consumeBody(s));
  assert.equal(r.status, 200, JSON.stringify(r.body));
  assert.equal(count(s, "POST", `/authorizations/${uuid(702)}/reissue`), 1);
  assert.equal(s.calls.filter((c) => c.path === `${MP}/authorizations/consume`).at(-1).body.token, TOKEN2);
});

test("subscriptions a marketplace licence started are hidden from the plan logic", async (t) => {
  const { s, call } = await setup(t);
  s.list = [{ subscription_id: uuid(5), provider: "iyzico", provider_status: "ACTIVE", pricing_plan_reference: null, external_reference: `mp-checkout:${uuid(9)}` },
    { subscription_id: uuid(6), provider: "iyzico", provider_status: "ACTIVE", pricing_plan_reference: "ref-x", plan_name: "marketplace:mpo_x" }];
  const cloud = await call("GET", "/v1/cloud");
  assert.equal(cloud.status, 200, JSON.stringify(cloud.body));
  assert.deepEqual(cloud.body.subscriptions, []);
  const nests = await call("GET", "/v1/nests");
  assert.deepEqual(nests.body.subscriptions, []);
});

test("buying for an org needs its billing managed by the caller", async (t) => {
  const { s, call } = await setup(t);
  s.orgBilling = { can_manage: false };
  let r = await buy(call, "d".repeat(32), { owner: { kind: "org", id: "org_fixture01" } });
  assert.equal(r.status, 403);
  assert.equal(s.purchases.length, 0);
  // Not for another account either.
  r = await buy(call, "d".repeat(32), { owner: { kind: "account", id: "acct_someoneelse0" } });
  assert.equal(r.status, 403);
  s.orgBilling = { can_manage: true };
  s.owner = { kind: "org", id: "org_fixture01" };
  r = await buy(call, "e".repeat(32), { owner: { kind: "org", id: "org_fixture01" } });
  assert.equal(r.status, 200, JSON.stringify(r.body));
  assert.equal(s.purchases[0].owner_kind, "org");
  const quote = s.calls.find((c) => c.path === `${MP}/quotes`);
  assert.deepEqual(quote.body.buyer.owner_kind, "org");
});

test("the reconciler projects the licence onto the installation; buyers see and cancel it", async (t) => {
  const { s, call, internal } = await paid(t, { RECONCILE_INTERVAL_MS: "150" }, "monthly");
  const r = await internal("POST", "/internal/marketplace/authorizations/consume", consumeBody(s));
  assert.equal(r.status, 200, JSON.stringify(r.body));
  const waitFor = async (pred) => {
    for (let i = 0; i < 60; i++) {
      const hit = s.projections.find(pred);
      if (hit) return hit;
      await new Promise((ok) => setTimeout(ok, 100));
    }
    assert.fail(`no projection: ${JSON.stringify(s.projections)}`);
  };
  const first = await waitFor((p) => p.body.access === true);
  assert.equal(first.id, LICENCE);
  assert.equal(first.body.installation_id, INSTALL);
  assert.ok(Number.isInteger(first.body.reconciliation_version));
  s.licenceOverride = { status: "paused", access: false, paused_at: new Date().toISOString(), deletion_due_at: new Date(Date.now() + 72 * 3600000).toISOString() };
  const paused = await waitFor((p) => p.body.status === "paused");
  assert.equal(paused.body.access, false);
  assert.ok(paused.body.reconciliation_version > first.body.reconciliation_version);

  // The buyer's installation screen: licence, payments with invoice ids, cancel at period end.
  s.licenceOverride = {};
  const view = await call("GET", `/v1/marketplace/installations/${INSTALL}`);
  assert.equal(view.status, 200, JSON.stringify(view.body));
  assert.equal(view.body.licence.licence_id, LICENCE);
  assert.equal(view.body.payments[0].invoice_id, uuid(705));
  const list = await call("GET", "/v1/marketplace/licences");
  assert.equal(list.body.licences[0].installation_id, INSTALL);
  const cancelled = await call("POST", `/v1/marketplace/installations/${INSTALL}/cancel`);
  assert.equal(cancelled.status, 200, JSON.stringify(cancelled.body));
  assert.equal(cancelled.body.licence.status, "canceling");
  assert.ok(s.projections.some((p) => p.body.cancel_at_period_end === true));
  assert.equal((await call("GET", "/v1/marketplace/installations/ins_zzzzzzzzzzzzzzzz")).status, 404);
});
