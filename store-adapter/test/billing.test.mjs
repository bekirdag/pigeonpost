// The iyzico abonelik bridge over HTTP, against fixtures for saas_be and the postbox.
import assert from "node:assert/strict";
import { once } from "node:events";
import http from "node:http";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { ACCOUNT, OIDC_SUB, fixtureCatalog, json, readBody, spawnBridge, uuid } from "./harness.mjs";

const MEMBER = "Bearer member-token";
const SVC = "Bearer svc_fixture_billing_token_0123456789abcdef";
const SAAS_INTERNAL = "Bearer saas-internal-fixture";
const FORM = '<script type="text/javascript">var iyziInit = {token:"fixture"};</script><div id="iyzipay-checkout-form" class="responsive"></div>';
const future = (days) => new Date(Date.now() + days * 86400000).toISOString();
const intent = (n) => String(n).padStart(32, "a").slice(-32).replace(/[^a-f0-9]/g, "b");

function fixture() {
  const { file, catalog } = fixtureCatalog();
  const s = {
    catalog, calls: [], puts: [], orgCalls: [], operations: new Map(), subs: new Map(), list: [], access: [],
    offerOverride: {}, webPlan: { capacity: 0, used: 0, names: [] }, nextSub: 1,
  };
  const edition = (code) => catalog.plans[code].editions["2026"];
  const planByPackage = (slug, currency) => Object.entries(catalog.plans).find(([, p]) => p.package_slug === slug && p.currency === currency);
  s.server = http.createServer(async (req, res) => {
    const url = new URL(req.url, "http://localhost");
    const body = await readBody(req);
    const auth = req.headers.authorization;
    s.calls.push({ method: req.method, path: url.pathname, query: url.search, body, key: req.headers["idempotency-key"], auth,
      slug: req.headers["x-product-slug"] });
    const p = url.pathname;
    let m;
    // ---- postbox public
    if (p === "/v1/me/handles") return auth === MEMBER ? json(res, 200, { account: ACCOUNT, handles: [] }) : json(res, 401, {});
    if (p === "/v1/me/entitlements") return json(res, 200, { account: ACCOUNT, entitlements: [], web_plan: s.webPlan });
    if (p === "/v1/claims/plan") return json(res, 200, { namespace: `/${body.namespace}`, capacity: 3, used: 1 });
    // ---- postbox internal
    if (p.startsWith("/internal/v1/")) {
      if (auth !== SVC) return json(res, 401, { error: "invalid_service_token" });
      if ((m = /^\/internal\/v1\/entitlements\/([^/]+)\/([^/]+)\/([^/]+)$/.exec(p)) && req.method === "PUT") {
        s.puts.push({ provider: m[1], environment: m[2], reference: decodeURIComponent(m[3]), body });
        return json(res, 200, { applied: true, entitlement: body });
      }
      if (p === "/internal/v1/org-drafts") { s.orgCalls.push({ op: "draft", body }); return json(res, 201, { org: { id: "org_draftfixture01", status: "pending" } }); }
      if ((m = /^\/internal\/v1\/orgs\/([^/]+)\/(activate|entitlement)$/.exec(p))) {
        s.orgCalls.push({ op: m[2], id: m[1], body });
        return json(res, 200, { org: { id: m[1], status: "active" }, entitlement: body.entitlement || body });
      }
      if ((m = /^\/internal\/v1\/orgs\/([^/]+)$/.exec(p))) return json(res, 200, { org: { id: m[1], status: "active" }, entitlements: [] });
      return json(res, 404, {});
    }
    // ---- saas_be internal
    if (p === "/v1/internal/tenants/pigeonpost/subscription-packages") {
      return auth === SAAS_INTERNAL ? json(res, 200, { data: s.access }) : json(res, 401, { message: "Invalid adapter service token." });
    }
    if (auth !== MEMBER) return json(res, 401, { message: "Unauthorized" });
    // ---- saas_be member
    if (p === "/v1/integrations/iyzico/regional-pricing/offer") {
      const found = planByPackage(url.searchParams.get("package_slug"), url.searchParams.get("currency"));
      if (!found) return json(res, 200, { available: false, reason: "none" });
      const ed = found[1].editions["2026"];
      return json(res, 200, { available: true, currency: found[1].currency, currencyPolicy: found[1].currency === "TRY" ? "CALENDAR_YEAR_FIXED" : "FIXED",
        trial_period_days: 0, interval: found[1].interval, plan_id: ed.plan_id, package_id: uuid(9), amount: ed.amount, pricing_year: 2026,
        effective_from: "2025-12-31T21:00:00.000Z", effective_until: found[1].currency === "TRY" ? future(60) : null,
        first_collection_at: new Date().toISOString(), cancellation_terms: "x", ...s.offerOverride });
    }
    if (p === "/v1/integrations/iyzico/subscriptions" && req.method === "POST") {
      const prior = s.operations.get(req.headers["idempotency-key"]);
      if (prior) return JSON.stringify(prior.body) === JSON.stringify(body) ? json(res, 201, prior.result) : json(res, 409, { message: "This Idempotency-Key belongs to another Iyzico operation." });
      if (s.failStart) return json(res, s.failStart.status, { message: s.failStart.message });
      const id = uuid(s.nextSub++);
      const result = { subscription_id: id, binding_id: uuid(500 + s.nextSub), provider: "iyzico", status: "checkout_pending",
        checkout_form_content: FORM, checkout_form_token: "tok", trial_period_days: 0, expires_at: future(1) };
      s.operations.set(req.headers["idempotency-key"], { body, result });
      const ref = Object.values(catalog.plans).find((pl) => pl.editions["2026"].plan_id === body.plan_id).editions["2026"].provider_plan_reference;
      s.subs.set(id, { subscription_id: id, binding_id: result.binding_id, provider: "iyzico", provider_managed: true, mode: "live",
        provider_reference: null, pricing_plan_reference: ref, provider_status: "CHECKOUT_PENDING", status: "past_due",
        recurring_price: null, current_period_start: null, current_period_end: null, orders: [] });
      return json(res, 201, result);
    }
    if (p === "/v1/integrations/iyzico/subscriptions" && req.method === "GET") return json(res, 200, { items: s.list });
    if ((m = /^\/v1\/integrations\/iyzico\/subscriptions\/([^/]+)$/.exec(p)) && req.method === "GET") {
      const sub = s.subs.get(m[1]);
      return sub ? json(res, 200, typeof s.mangle === "function" ? s.mangle(sub) : sub) : json(res, 404, { message: "Iyzico subscription not found." });
    }
    if ((m = /^\/v1\/integrations\/iyzico\/subscriptions\/([^/]+)\/(cancel|retry|card-update|upgrade)$/.exec(p))) {
      const sub = s.subs.get(m[1]);
      if (m[2] === "cancel") { sub.status = "canceled"; sub.provider_status = "CANCELED"; return json(res, 201, { subscription_id: m[1], status: "canceled" }); }
      if (m[2] === "card-update") return json(res, 201, { subscription_id: m[1], token: "t", checkout_form_content: FORM, expires_in: 1800 });
      return json(res, 201, { subscription_id: m[1], status: sub.status });
    }
    if (p === "/v1/billing/profiles" && req.method === "POST") return json(res, 201, { id: uuid(77), ...body });
    if (p === "/v1/billing/profiles") return json(res, 200, { data: [] });
    if (p === "/v1/billing/invoices") return json(res, 200, { data: [{ id: "inv", status: "paid" }] });
    if (p === `/v1/billing/invoices/${uuid(61)}/download`) return json(res, 200, { url: "https://files.example.test/invoice.pdf" });
    if (p === `/v1/billing/invoices/${uuid(62)}/download`) return json(res, 200, { url: "javascript:alert(1)" });
    return json(res, 404, { message: `unexpected ${req.method} ${p}` });
  });
  s.activate = (id, { days = 365 } = {}) => {
    const sub = s.subs.get(id);
    Object.assign(sub, { status: "active", provider_status: "ACTIVE", provider_reference: "prov-sub",
      current_period_start: new Date().toISOString(), current_period_end: future(days),
      orders: [{ order_reference_code: "order-1", status: "SUCCESS", amount: "8.00", currency: "USD", period_end: future(days) }] });
    return sub;
  };
  s.edition = edition;
  s.file = file;
  return s;
}

async function setup(t, extraEnv = {}) {
  const s = fixture();
  s.server.listen(0, "127.0.0.1");
  await once(s.server, "listening");
  t.after(() => { s.server.closeAllConnections(); s.server.close(); });
  const upstream = `http://127.0.0.1:${s.server.address().port}`;
  const bridge = await spawnBridge(t, {
    MASAAS_SAAS_API_URL: upstream, PIGEONPOST_POSTBOX_URL: upstream, POSTBOX_INTERNAL_URL: upstream,
    POSTBOX_SERVICE_TOKEN: SVC.slice(7), SAAS_INTERNAL_TOKEN: SAAS_INTERNAL.slice(7), PIGEONPOST_CATALOG_FILE: s.file,
    ENTERPRISE_ORIGIN: "https://enterprise.pigeonpost.dev", ...extraEnv,
  });
  const call = async (method, path, body, headers = {}) => {
    const r = await fetch(bridge.base + "/api" + path, { method, headers: { authorization: MEMBER, ...(body ? { "content-type": "application/json" } : {}), ...headers },
      body: body ? (typeof body === "string" ? body : JSON.stringify(body)) : undefined });
    return { status: r.status, body: await r.json().catch(() => null), headers: r.headers };
  };
  return { s, bridge, call, base: bridge.base };
}

const PROFILE = uuid(42);
const checkout = (call, id, plan = "cloud-3names-yearly-usd", extra = {}) =>
  call("POST", "/v1/cloud/checkout", { intent_id: id, plan_code: plan, billing_profile_id: PROFILE, ...extra });

test("cloud checkout resolves the edition server-side and starts one hosted subscription", async (t) => {
  const { s, call, bridge } = await setup(t);
  const id = "0123456789abcdef0123456789abcdef";
  const r = await checkout(call, id, "cloud-3names-yearly-usd", { locale: "tr" });
  assert.equal(r.status, 200, JSON.stringify(r.body));
  assert.equal(r.body.checkout_form_content, FORM);
  assert.equal(r.body.amount, "24.00");
  const start = s.calls.find((c) => c.path === "/v1/integrations/iyzico/subscriptions" && c.method === "POST");
  assert.deepEqual(start.body, {
    plan_id: s.edition("cloud-3names-yearly-usd").plan_id, package_id: uuid(9), billing_profile_id: PROFILE,
    client_reference_id: `pp-cloud:${ACCOUNT}:${id}`, method: "hosted", initial_status: "ACTIVE", quantity: 1,
    trial_period_days: 0, return_url: `https://pigeonpost.dev/account?checkout=${id}`, locale: "tr",
  });
  assert.equal(start.key, `pp-cloud-${id}`);
  assert.equal(start.slug, "pigeonpost");
  // The durable state holds the intent, but never the hosted form, its token or card data.
  const saved = readFileSync(join(bridge.stateDir, "billing-state.json"), "utf8");
  assert.ok(saved.includes(id));
  assert.ok(!saved.includes("iyziInit") && !saved.includes("checkout_form"));

  // A replay (lost response, page reload) reuses the key and the identical body.
  const again = await checkout(call, id, "cloud-3names-yearly-usd", { locale: "tr" });
  assert.equal(again.status, 200);
  assert.equal(again.body.subscription_id, r.body.subscription_id);
  const starts = s.calls.filter((c) => c.path === "/v1/integrations/iyzico/subscriptions" && c.method === "POST");
  assert.equal(starts.length, 2);
  assert.deepEqual(starts[1].body, starts[0].body);
  assert.equal(s.calls.filter((c) => c.path.endsWith("/offer")).length, 1, "edition is resolved once per intent");

  // The same intent with different details is refused before any provider call.
  const before = s.calls.length;
  const changed = await checkout(call, id, "cloud-4names-yearly-usd");
  assert.equal(changed.status, 409);
  assert.equal(s.calls.filter((c) => c.method === "POST").length, s.calls.slice(0, before).filter((c) => c.method === "POST").length);
});

test("checkout refuses unknown plans, foreign fields, unapproved offers and a second subscription", async (t) => {
  const { s, call } = await setup(t);
  assert.equal((await checkout(call, intent(1), "cloud-11names-yearly-usd")).status, 400);
  assert.equal((await checkout(call, intent(2), "ent-5-monthly-usd")).status, 400);
  assert.equal((await checkout(call, intent(3), "cloud-1names-yearly-usd", { price: "0.01" })).status, 400);
  assert.equal((await call("POST", "/v1/cloud/checkout", { intent_id: "short", plan_code: "cloud-1names-yearly-usd", billing_profile_id: PROFILE })).status, 400);
  for (const [override, why] of [[{ amount: "7.00" }, "price differs"], [{ trial_period_days: 7 }, "trial"],
    [{ plan_id: uuid(999) }, "unknown edition"], [{ currency: "TRY" }, "currency"], [{ available: false, reason: "closed" }, "closed"]]) {
    s.offerOverride = override;
    const r = await checkout(call, intent(10 + Object.keys(override).length + why.length), "cloud-1names-yearly-usd");
    assert.equal(r.status, 503, why);
  }
  assert.equal(s.calls.filter((c) => c.path === "/v1/integrations/iyzico/subscriptions" && c.method === "POST").length, 0);
  s.offerOverride = {};
  s.list = [{ subscription_id: uuid(5), provider: "iyzico", provider_status: "ACTIVE", pricing_plan_reference: s.edition("cloud-2names-yearly-try").provider_plan_reference }];
  const dup = await checkout(call, intent(40), "cloud-5names-yearly-try");
  assert.equal(dup.status, 409);
  assert.equal(dup.body.code, "already_subscribed");
});

test("a currency without an active merchant POS is closed before any provider call", async (t) => {
  const { s, call } = await setup(t, { OPEN_CURRENCIES: "TRY" });
  const r = await checkout(call, "7".repeat(32), "cloud-1names-yearly-usd");
  assert.equal(r.status, 503);
  assert.equal(r.body.code, "currency_unavailable");
  assert.equal(s.calls.some((c) => c.path.startsWith("/v1/integrations")), false);
  assert.equal((await checkout(call, "8".repeat(32), "cloud-1names-yearly-try")).status, 200);
});

test("five new checkouts per account per hour; replays are free", async (t) => {
  const { call } = await setup(t);
  const ids = [1, 2, 3, 4, 5].map((n) => `${n}`.repeat(32));
  for (const id of ids) assert.equal((await checkout(call, id)).status, 200);
  assert.equal((await checkout(call, ids[0])).status, 200, "replay of an existing intent");
  const sixth = await checkout(call, "6".repeat(32));
  assert.equal(sixth.status, 429);
});

test("a definite refusal ends the intent; an ambiguous one stays replayable", async (t) => {
  const { s, call } = await setup(t);
  s.failStart = { status: 400, message: "Complete the buyer name and TCKN on the billing profile." };
  const id = "c".repeat(32);
  const r = await checkout(call, id);
  assert.equal(r.status, 400);
  assert.match(r.body.error, /TCKN/);
  assert.equal((await checkout(call, id)).status, 409, "a failed intent is not resubmitted");
  s.failStart = { status: 503, message: "upstream" };
  const ambiguous = "d".repeat(32);
  assert.equal((await checkout(call, ambiguous)).status, 502);
  s.failStart = null;
  assert.equal((await checkout(call, ambiguous)).status, 200, "same key replays after an outage");
});

test("return polling activates exactly once from authoritative state; pending grants nothing", async (t) => {
  const { s, call } = await setup(t);
  const id = "e".repeat(32);
  const started = await checkout(call, id, "cloud-3names-yearly-usd");
  const pending = await call("GET", `/v1/cloud/checkout/${id}`);
  assert.equal(pending.status, 200);
  assert.equal(pending.body.status, "pending");
  assert.equal(s.puts.length, 0, "a redirect or pending checkout grants nothing");
  s.activate(started.body.subscription_id);
  const active = await call("GET", `/v1/cloud/checkout/${id}`);
  assert.equal(active.body.status, "active");
  assert.equal(s.puts.length, 1);
  const put = s.puts[0];
  assert.deepEqual([put.provider, put.environment, put.reference], ["iyzico", "live", started.body.subscription_id]);
  assert.deepEqual(put.body.subject, { kind: "account", id: ACCOUNT });
  assert.equal(put.body.names, 3);
  assert.equal(put.body.plan_code, "cloud-3names-yearly-usd");
  assert.equal(put.body.status, "active");
  assert.ok(put.body.paid_through > Date.now() / 1000 + 300 * 86400);
  assert.ok(Number.isInteger(put.body.reconciliation_version));
  await call("GET", `/v1/cloud/checkout/${id}`);
  assert.equal(s.puts.length, 1, "unchanged state is not pushed again");
  const other = await call("GET", `/v1/cloud/checkout/${"f".repeat(32)}`);
  assert.equal(other.status, 404);
});

test("invalid saas_be responses never activate anything", async (t) => {
  const { s, call } = await setup(t);
  const id = "a1".repeat(16);
  const started = await checkout(call, id);
  s.activate(started.body.subscription_id);
  for (const mangle of [(sub) => ({ ...sub, mode: "test" }), (sub) => ({ ...sub, subscription_id: uuid(3) }),
    (sub) => ({ ...sub, provider_status: "WEIRD" }), (sub) => ({ ...sub, current_period_end: "soon" })]) {
    s.mangle = mangle;
    assert.equal((await call("GET", `/v1/cloud/checkout/${id}`)).status, 502);
  }
  s.mangle = (sub) => ({ ...sub, current_period_end: future(900) });
  assert.equal((await call("GET", `/v1/cloud/checkout/${id}`)).body.status, "pending", "implausible periods are not trusted");
  assert.equal(s.puts.length, 0);
});

test("cancel keeps the paid period; card update and retry use the hosted form and owned orders", async (t) => {
  const { s, call } = await setup(t);
  const started = await checkout(call, "b2".repeat(16), "cloud-2names-yearly-try");
  const sub = s.activate(started.body.subscription_id);
  const sid = started.body.subscription_id;
  const view = await call("GET", `/v1/cloud/subscriptions/${sid}`);
  assert.equal(view.body.status, "active");
  const paidThrough = s.puts.at(-1).body.paid_through;

  const card = await call("POST", `/v1/cloud/subscriptions/${sid}/card-update`, { intent_id: "c3".repeat(16), locale: "en" });
  assert.equal(card.status, 200);
  assert.equal(card.body.checkout_form_content, FORM);
  const cardCall = s.calls.find((c) => c.path.endsWith("/card-update"));
  assert.deepEqual(cardCall.body, { scope: "subscription", return_url: `https://pigeonpost.dev/account?card=${"c3".repeat(16)}`, locale: "en" });
  assert.equal(cardCall.key, `pp-card-update-${"c3".repeat(16)}`);

  assert.equal((await call("POST", `/v1/cloud/subscriptions/${sid}/retry`, { intent_id: "d4".repeat(16), order_reference_code: "order-1" })).status, 400,
    "only a failed order of this subscription can be retried");
  sub.orders.push({ order_reference_code: "order-2", status: "FAILED", amount: "817.00", currency: "TRY", period_end: future(400) });
  assert.equal((await call("POST", `/v1/cloud/subscriptions/${sid}/retry`, { intent_id: "d4".repeat(16), order_reference_code: "order-2" })).status, 200);

  const cancel = await call("POST", `/v1/cloud/subscriptions/${sid}/cancel`, { intent_id: "e5".repeat(16) });
  assert.equal(cancel.status, 200);
  assert.equal(cancel.body.status, "canceled");
  const last = s.puts.at(-1).body;
  assert.equal(last.status, "canceled");
  assert.equal(last.paid_through, paidThrough, "cancellation never shortens verified access");
  assert.ok(last.cancel_requested_at > 0);
  assert.equal(s.calls.find((c) => c.path.endsWith("/cancel")).key, `pp-cancel-${"e5".repeat(16)}`);

  assert.equal((await call("GET", `/v1/cloud/subscriptions/${uuid(999)}`)).status, 404);
});

test("upgrade stays in the same product, currency and interval and never strands names", async (t) => {
  const { s, call } = await setup(t);
  const started = await checkout(call, "f6".repeat(16), "cloud-3names-yearly-usd");
  s.activate(started.body.subscription_id);
  const sid = started.body.subscription_id;
  const up = (plan, period = "NOW") => call("POST", `/v1/cloud/subscriptions/${sid}/upgrade`, { intent_id: "a7".repeat(16), plan_code: plan, upgrade_period: period });
  assert.equal((await up("cloud-5names-yearly-try")).status, 400, "currency change");
  assert.equal((await up("ent-5-yearly-usd")).status, 400, "other product");
  assert.equal((await up("cloud-3names-yearly-usd")).status, 400, "same plan");
  s.webPlan = { capacity: 3, used: 3, names: ["a", "b", "c"] };
  const down = await up("cloud-2names-yearly-usd", "NEXT_PERIOD");
  assert.equal(down.status, 409);
  assert.equal(down.body.code, "names_in_use");
  const ok = await up("cloud-5names-yearly-usd");
  assert.equal(ok.status, 200, JSON.stringify(ok.body));
  const call_ = s.calls.find((c) => c.path.endsWith("/upgrade"));
  assert.deepEqual(call_.body, { plan_id: s.edition("cloud-5names-yearly-usd").plan_id, trial_period_days: 0, upgradePeriod: "NOW" });
});

test("the reconciler activates closed-tab purchases from the internal list and expires lapsed ones", async (t) => {
  const { s, call, base } = await setup(t);
  const plan = s.edition("cloud-4names-yearly-usd");
  const closedTab = uuid(321);
  s.access = [
    { subscription_id: closedTab, tenant_id: "pigeonpost", user_id: uuid(8), keycloak_id: OIDC_SUB, status: "active",
      plan_id: plan.plan_id, package_slug: "pp-cloud-4names", product_slug: "pigeonpost",
      external_reference: `pp-cloud:${ACCOUNT}:${"9".repeat(32)}`, current_period_start: new Date().toISOString(), current_period_end: future(365) },
    { subscription_id: uuid(322), tenant_id: "pigeonpost", status: "active", plan_id: plan.plan_id, package_slug: "pp-cloud-4names",
      external_reference: "pigeonpost:handle:legacy", current_period_start: new Date().toISOString(), current_period_end: future(300) },
    { subscription_id: uuid(323), tenant_id: "pigeonpost", status: "active", plan_id: plan.plan_id, package_slug: "pp-cloud-4names",
      external_reference: `pp-cloud:${ACCOUNT}`, current_period_start: new Date().toISOString(), current_period_end: future(9000) },
  ];
  // A Cloud plan paid until yesterday that is no longer in the access list.
  const lapsedDir = mkdtempSync(join(tmpdir(), "pp-lapsed-"));
  const lapsed = uuid(330);
  const lapsedUntil = Math.floor(Date.now() / 1000) - 86400;
  writeFileSync(join(lapsedDir, "billing-state.json"), JSON.stringify({ version: 1, intents: {}, refs: {
    [lapsed]: { kind: "cloud", account: ACCOUNT, plan_code: "cloud-1names-yearly-usd", plan_version: "2026", names: 1,
      status: "canceled", paid_through: lapsedUntil, version: 5, pushed: "old" } } }));
  // The reconciler runs on its own timer in production; here a second bridge with a short interval.
  const fast = await spawnBridge(t, {
    STORE_STATE_DIR: lapsedDir,
    MASAAS_SAAS_API_URL: `http://127.0.0.1:${s.server.address().port}`, PIGEONPOST_POSTBOX_URL: `http://127.0.0.1:${s.server.address().port}`,
    POSTBOX_INTERNAL_URL: `http://127.0.0.1:${s.server.address().port}`, POSTBOX_SERVICE_TOKEN: SVC.slice(7),
    SAAS_INTERNAL_TOKEN: SAAS_INTERNAL.slice(7), PIGEONPOST_CATALOG_FILE: s.file, RECONCILE_INTERVAL_MS: "150",
  });
  const deadline = Date.now() + 6000;
  while (!s.puts.some((p) => p.reference === closedTab) && Date.now() < deadline) await new Promise((r) => setTimeout(r, 100));
  const put = s.puts.find((p) => p.reference === closedTab);
  assert.ok(put, "closed-tab purchase activated without a member token");
  assert.deepEqual(put.body.subject, { kind: "account", id: ACCOUNT });
  assert.deepEqual(put.body.subject_check, { oidc_sub: OIDC_SUB });
  assert.equal(put.body.names, 4);
  assert.ok(!s.puts.some((p) => p.reference === uuid(322)), "legacy references are not Cloud plans");
  assert.ok(!s.puts.some((p) => p.reference === uuid(323)), "implausible periods are skipped");
  const count = s.puts.filter((p) => p.reference === closedTab).length;
  await new Promise((r) => setTimeout(r, 600));
  assert.equal(s.puts.filter((p) => p.reference === closedTab).length, count, "steady state is not re-pushed");
  assert.ok(s.calls.filter((c) => c.path.endsWith("/subscription-packages")).every((c) => c.auth === SAAS_INTERNAL));
  const expired = s.puts.filter((p) => p.reference === lapsed);
  assert.equal(expired.length, 1, "a lapsed plan is marked expired once");
  assert.equal(expired[0].body.status, "expired");
  assert.equal(expired[0].body.paid_through, lapsedUntil, "expiry never changes the verified period");
  assert.ok(expired[0].body.reconciliation_version > 5);
  void call; void base; void fast;
});

test("enterprise checkout drafts the org, starts with pp-org and activates on return", async (t) => {
  const { s, call } = await setup(t);
  const id = "ab".repeat(16);
  const profile = await call("POST", "/ent/billing-profile", { intent_id: id, account_type: "entity", legal_name: "Acme Ltd",
    entity_name: "Acme Ltd", tax_id: "1234567890", tax_office: "Kozyatagi", billing_email: "billing@acme.example",
    address: { line1: "1 Example St", city: "Istanbul", state: "Atasehir", postal_code: "34000", country: "TR" } });
  assert.equal(profile.status, 201);
  assert.equal((await call("POST", "/ent/billing-profile", { intent_id: id, legal_name: "x", billing_email: "x", address: { line1: "x", city: "y", country: "TR" }, admin: true })).status, 400);
  const r = await call("POST", "/ent/checkout", { intent_id: id, plan_code: "ent-30-yearly-usd", billing_profile_id: PROFILE,
    org: { name: "Acme", namespace: "acme" } }, { origin: "https://enterprise.pigeonpost.dev" });
  assert.equal(r.status, 200, JSON.stringify(r.body));
  assert.equal(r.headers.get("access-control-allow-origin"), "https://enterprise.pigeonpost.dev");
  assert.equal(r.body.org_id, "org_draftfixture01");
  assert.deepEqual(s.orgCalls[0].body, { operation_id: id, owner_account: ACCOUNT, name: "Acme", namespace: "acme", plan_code: "ent-30-yearly-usd", seats: 30 });
  const start = s.calls.find((c) => c.path === "/v1/integrations/iyzico/subscriptions" && c.method === "POST");
  assert.equal(start.body.client_reference_id, "pp-org:org_draftfixture01");
  assert.equal(start.body.return_url, "https://enterprise.pigeonpost.dev/checkout/return?d=org_draftfixture01");
  assert.equal(start.key, `pp-ent-${id}`);
  assert.equal((await call("POST", "/ent/checkout", { intent_id: "cd".repeat(16), plan_code: "ent-1000-yearly-usd", billing_profile_id: PROFILE, org: { name: "A", namespace: "a" } })).status, 400);
  const pending = await call("GET", "/ent/checkout/org_draftfixture01");
  assert.equal(pending.body.status, "pending");
  assert.equal(s.orgCalls.length, 1);
  s.activate(r.body.subscription_id);
  const done = await call("GET", "/ent/checkout/org_draftfixture01");
  assert.equal(done.body.status, "active");
  const activation = s.orgCalls.find((c) => c.op === "activate");
  assert.equal(activation.body.entitlement.seats, 30);
  assert.equal(activation.body.entitlement.reference, r.body.subscription_id);
  assert.equal(activation.body.operation_id, "activate-org_draftfixture01");
});

test("public catalog, request limits, auth and name registration", async (t) => {
  const { call, base } = await setup(t);
  const catalog = await (await fetch(base + "/api/v1/catalog")).json();
  const cloud3 = catalog.plans.find((p) => p.code === "cloud-3names-yearly-usd");
  assert.equal(cloud3.amount, "24.00");
  assert.equal(catalog.plans.length, 56);
  assert.equal(catalog.contact_sales_above_seats, 500);
  const big = await call("POST", "/v1/cloud/checkout", "x".repeat(70 * 1024));
  assert.equal(big.status, 413);
  assert.equal((await call("POST", "/v1/cloud/checkout", "{not json")).status, 400);
  const anon = await fetch(base + "/api/v1/cloud/checkout", { method: "POST", body: "{}" });
  assert.equal(anon.status, 401);
  const name = await call("POST", "/v1/names", { namespace: "alex" });
  assert.equal(name.status, 200);
  assert.equal(name.body.namespace, "/alex");
  const invoices = await call("GET", "/v1/billing/invoices");
  assert.equal(invoices.status, 200);
  assert.equal(invoices.body.invoices[0].id, "inv");
  const pdf = await call("GET", `/v1/billing/invoices/${uuid(61)}/download`);
  assert.equal(pdf.body.url, "https://files.example.test/invoice.pdf");
  assert.equal((await call("GET", `/v1/billing/invoices/${uuid(62)}/download`)).status, 502, "only https links are passed on");
  assert.equal((await call("GET", "/v1/billing/invoices/..%2Fx/download")).status, 404);
});

test("the account page's displayed Cloud prices are the catalog's", () => {
  const source = readFileSync(new URL("../../site/account-config.js", import.meta.url), "utf8");
  const window = {};
  new Function("window", source)(window);
  const catalog = JSON.parse(readFileSync(new URL("../catalog/pigeonpost-catalog.json", import.meta.url), "utf8"));
  for (let n = 1; n <= 10; n++) {
    for (const currency of ["USD", "TRY"]) {
      const edition = catalog.plans[`cloud-${n}names-yearly-${currency.toLowerCase()}`].editions[String(window.PIGEONPOST_ACCOUNT.cloud.pricingYear)];
      assert.equal(window.PIGEONPOST_ACCOUNT.cloud.plans[n][currency], edition.amount, `${n} ${currency}`);
    }
  }
});
