// Prepaid credits through the bridge, against one fixture standing in for saas_be's credit routes,
// its member API and the postbox's member API. Synthetic tokens, accounts and amounts only;
// nothing contacts a payment provider.
import assert from "node:assert/strict";
import { once } from "node:events";
import http from "node:http";
import test from "node:test";
import { ACCOUNT, fixtureCatalog, json, readBody, spawnBridge, uuid } from "./harness.mjs";

const CR = "/v1/internal/tenants/pigeonpost/marketplace/credits";
const MP_TOKEN = "mp-bridge-fixture-token-0123456789";
const INTERNAL = "bridge-internal-fixture-token-0123456789";
const MEMBER = "Bearer member-token";
const OTHER = "Bearer other-member-token";
const OTHER_ACCOUNT = "acct_fedcba9876543210fedcba98";
const PAGE = "https://sandbox-cpp.iyzipay.com/?token=fixture";
const INTENT = "ab".repeat(16);

function fixture() {
  const s = {
    calls: [], available: 7500, held: 2500, checkoutStatus: "pending",
    packs: [{ pack_id: "usd_10", amount_cents: 1000, active: true }, { pack_id: "usd_100", amount_cents: 10000, active: true },
      { pack_id: "usd_25", amount_cents: 2500, active: true }, { pack_id: "usd_50", amount_cents: 5000, active: true },
      { pack_id: "usd_5", amount_cents: 500, active: true }, { pack_id: "usd_50", amount_cents: 5000, active: false }],
    profiles: [{ id: uuid(42), account_type: "individual", legal_name: "Ada Lovelace Buyer", billing_email: "ada@example.test",
      identity_number: "11111111111", address: { line1: "1 Main St", city: "Istanbul", postal_code: "34000", country: "TR" } }],
  };
  s.server = http.createServer(async (req, res) => {
    const url = new URL(req.url, "http://localhost");
    const p = url.pathname;
    const body = await readBody(req);
    const auth = req.headers.authorization;
    s.calls.push({ method: req.method, path: p, query: url.search, body, auth });
    let m;
    if ((m = /^\/v1\/internal\/tenants\/pigeonpost\/marketplace\/offers\/([^/]+)$/.exec(p)) && req.method === "PUT") {
      if (auth !== `Bearer ${MP_TOKEN}`) return json(res, 401, { message: "Invalid marketplace service token." });
      return json(res, 200, { offer_id: m[1], listing_id: body.listing_id, publisher_id: body.publisher_id, release_line: body.release_line,
        title: body.title, term: body.term, currency: body.currency, price_minor: body.price_minor, status: body.status, sellable: body.status === "active" });
    }
    if (p.startsWith(CR)) {
      if (auth !== `Bearer ${MP_TOKEN}`) return json(res, 401, { message: "Invalid marketplace service token." });
      const r = p.slice(CR.length);
      if (r === "/packs") return json(res, 200, { items: s.packs });
      if (r === "/quotes") {
        const usd = { usd_10: 1000, usd_25: 2500, usd_50: 5000, usd_100: 10000 }[body.pack_id];
        const rate = { USD: null, EUR: 0.92, TRY: 34.25 }[body.currency];
        return json(res, 201, { quote_id: uuid(900 + s.calls.length), pack_id: body.pack_id, credit_cents: usd,
          charge: { currency: body.currency, amount_minor: rate ? Math.round(usd * rate) : usd, tax_minor: 0 },
          fx: rate ? { rate: String(rate), source: "tcmb", as_of: "2026-11-11T09:00:00Z" } : null,
          expires_at: new Date(Date.now() + (s.quoteTtl ?? 900_000)).toISOString() });
      }
      if (r === "/checkouts") return json(res, 201, { checkout_id: uuid(950), payment_page_url: PAGE, expires_at: new Date(Date.now() + 1800_000).toISOString() });
      if ((m = /^\/checkouts\/([^/]+)$/.exec(r))) {
        return json(res, 200, { checkout_id: m[1], status: s.checkoutStatus, ...(s.checkoutStatus === "paid" ? { purchase_id: uuid(960) } : {}) });
      }
      if ((m = /^\/balances\/account\/([^/]+)$/.exec(r))) {
        if (m[1] === "acct_poor000000") return json(res, 200, { available_cents: 100, held_cents: 0 });
        return json(res, 200, { available_cents: s.available, held_cents: s.held });
      }
      if ((m = /^\/history\/account\/([^/]+)$/.exec(r))) {
        return json(res, 200, { items: [
          { kind: "sale", amount_cents: -2500, created_at: "2026-11-11T10:00:00Z", description: "Wodo AI HR — HR research", job_ref: "job_a", internal_note: "x" },
          { kind: "purchase", amount_cents: 10000, created_at: "2026-11-10T10:00:00Z", charge: { amount_minor: 342500, currency: "TRY" } },
          { kind: "bogus kind", amount_cents: 1 },
        ], next_cursor: "c2" });
      }
      if (r === "/holds") {
        if (body.owner.owner_id === "acct_poor000000") {
          return json(res, 402, { code: "insufficient_credit", message: "insufficient credit", available_cents: 100, required_cents: 2500 });
        }
        return json(res, 201, { hold_id: "hold_1", amount_cents: 2500, expires_at: "2030-01-01T00:00:00Z" });
      }
      if (r === "/holds/hold_1/capture") {
        return json(res, 200, { sale_id: "sale_1", amount_cents: 2500, commission_bps: 1000, commission_cents: 250, publisher_cents: 2250 });
      }
      if (r === "/holds/hold_1/release") return json(res, 409, { code: "CONFLICT", message: "captured" });
      if (r === "/holds/hold_2/release") return json(res, 200, { released: true });
      return json(res, 404, { message: `unexpected ${req.method} ${r}` });
    }
    // ---- postbox member
    if (p === "/v1/me/handles") {
      if (auth === MEMBER) return json(res, 200, { account: ACCOUNT, handles: [] });
      if (auth === OTHER) return json(res, 200, { account: OTHER_ACCOUNT, handles: [] });
      return json(res, 401, { error: "unauthorized" });
    }
    if (![MEMBER, OTHER].includes(auth)) return json(res, 401, { message: "Unauthorized" });
    // ---- saas_be member
    if (p === "/v1/profile") return json(res, 200, { id: uuid(800) });
    if (p === "/v1/billing/profiles") return json(res, 200, { data: s.profiles });
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
    POSTBOX_SERVICE_TOKEN: "svc_fixture_billing_token_0123456789abcdef", SAAS_INTERNAL_TOKEN: "saas-internal-fixture",
    PIGEONPOST_CATALOG_FILE: fixtureCatalog().file, OPEN_CURRENCIES: "USD,EUR,TRY",
    SAAS_MARKETPLACE_TOKEN: MP_TOKEN, BRIDGE_INTERNAL_TOKEN: INTERNAL, ...env,
  });
  const call = async (method, path, body, auth = MEMBER, prefix = "/api") => {
    const r = await fetch(`${bridge.base}${prefix}${path}`, { method,
      headers: { authorization: auth, "x-real-ip": "203.0.113.9", "cf-ipcountry": "TR", ...(body ? { "content-type": "application/json" } : {}) },
      body: body ? JSON.stringify(body) : undefined });
    return { status: r.status, body: await r.json() };
  };
  return { s, call, upstream };
}

const crCalls = (s, path) => s.calls.filter((c) => c.path === `${CR}${path}`);

test("packs, quotes and balance are the signed-in account's own", async (t) => {
  const { s, call } = await setup(t);
  const packs = await call("GET", "/v1/credits/packs");
  assert.equal(packs.status, 200);
  assert.deepEqual(packs.body.packs.map((p) => p.pack_id), ["usd_10", "usd_25", "usd_50", "usd_100"]);
  assert.deepEqual(packs.body.currencies, ["USD", "EUR", "TRY"]);

  const q = await call("POST", "/v1/credits/quotes", { pack_id: "usd_25", currency: "TRY" });
  assert.equal(q.status, 200, JSON.stringify(q.body));
  assert.equal(q.body.credit_cents, 2500);
  assert.deepEqual(q.body.charge, { currency: "TRY", amount_minor: 85625, tax_minor: 0 });
  assert.equal(q.body.fx.source, "tcmb");
  assert.deepEqual(crCalls(s, "/quotes")[0].body.owner, { owner_kind: "account", owner_id: ACCOUNT });
  // The owner is never taken from the body.
  const forged = await call("POST", "/v1/credits/quotes", { pack_id: "usd_25", currency: "TRY", owner: { owner_kind: "account", owner_id: OTHER_ACCOUNT } });
  assert.equal(forged.status, 400);
  assert.equal((await call("POST", "/v1/credits/quotes", { pack_id: "usd_5", currency: "USD" })).status, 400);
  assert.equal((await call("POST", "/v1/credits/quotes", { pack_id: "usd_10", currency: "GBP" })).status, 400);

  const b = await call("GET", "/v1/credits/balance");
  assert.equal(b.status, 200);
  assert.deepEqual(b.body, { owner: { owner_kind: "account", owner_id: ACCOUNT }, currency: "USD", available_cents: 7500, held_cents: 2500 });
  assert.equal(s.calls.at(-1).path, `${CR}/balances/account/${ACCOUNT}`);

  const h = await call("GET", "/v1/credits/history?limit=5&cursor=c1");
  assert.equal(h.status, 200);
  assert.equal(h.body.entries.length, 2);
  assert.equal(h.body.entries[0].description, "Wodo AI HR — HR research");
  assert.equal(h.body.entries[0].internal_note, undefined);
  assert.deepEqual(h.body.entries[1].charge, { amount_minor: 342500, currency: "TRY" });
  assert.equal(h.body.next_cursor, "c2");
  assert.match(s.calls.at(-1).query, /limit=5&cursor=c1/);
  assert.equal((await call("GET", "/v1/credits/history?limit=500")).status, 400);

  assert.equal((await call("GET", "/v1/credits/balance", null, "")).status, 401);
  assert.equal((await call("GET", "/v1/credits/balance", null, "Bearer stranger")).status, 401);
});

test("a checkout uses the account's own quote and billing details and replays by intent", async (t) => {
  const { s, call } = await setup(t);
  const q = await call("POST", "/v1/credits/quotes", { pack_id: "usd_100", currency: "USD" });
  const start = { intent_id: INTENT, quote_id: q.body.quote_id, billing_profile_id: uuid(42) };
  // Another account cannot spend this quote.
  const stolen = await call("POST", "/v1/credits/checkout", { ...start, intent_id: "cd".repeat(16) }, OTHER);
  assert.equal(stolen.status, 404);
  assert.equal(stolen.body.code, "quote_not_found");

  const c = await call("POST", "/v1/credits/checkout", start);
  assert.equal(c.status, 200, JSON.stringify(c.body));
  assert.equal(c.body.payment_page_url, PAGE);
  assert.equal(c.body.credit_cents, 10000);
  const sent = crCalls(s, "/checkouts")[0].body;
  assert.equal(sent.quote_id, q.body.quote_id);
  assert.deepEqual(sent.owner, { owner_kind: "account", owner_id: ACCOUNT });
  assert.equal(sent.return_url, `https://pigeonpost.dev/account?credits=${INTENT}#credits`);
  assert.equal(sent.idempotency_key, `pp-credits-${INTENT}`);
  assert.deepEqual(sent.buyer, { user_id: uuid(800), billing_profile_id: "00000000-0000-4000-8000-000000000042", name: "Ada Lovelace", surname: "Buyer", email: "ada@example.test",
    ip: "203.0.113.9", country: "TR", city: "Istanbul", address: "1 Main St, 34000", identity_number: "11111111111" });

  // A replay returns the same page without a second checkout; other details are a conflict.
  const again = await call("POST", "/v1/credits/checkout", start);
  assert.equal(again.body.payment_page_url, PAGE);
  assert.equal(crCalls(s, "/checkouts").length, 1);
  assert.equal((await call("POST", "/v1/credits/checkout", { ...start, billing_profile_id: uuid(43) })).status, 409);

  // Back from iyzico: pending, then paid.
  const pending = await call("GET", `/v1/credits/checkout/${INTENT}`);
  assert.equal(pending.body.status, "pending");
  s.checkoutStatus = "paid";
  const paid = await call("GET", `/v1/credits/checkout/${INTENT}`);
  assert.equal(paid.body.status, "paid");
  assert.equal(paid.body.purchase_id, uuid(960));
  const before = crCalls(s, `/checkouts/${uuid(950)}`).length;
  assert.equal((await call("GET", `/v1/credits/checkout/${INTENT}`)).body.status, "paid");
  assert.equal(crCalls(s, `/checkouts/${uuid(950)}`).length, before, "a settled purchase is not asked again");
  assert.equal((await call("GET", `/v1/credits/checkout/${INTENT}`, null, OTHER)).status, 404);
});

test("expired quotes, EU consumers and incomplete billing details do not reach a payment page", async (t) => {
  const { s, call } = await setup(t);
  s.quoteTtl = -1000;
  const old = await call("POST", "/v1/credits/quotes", { pack_id: "usd_10", currency: "USD" });
  const r = await call("POST", "/v1/credits/checkout", { intent_id: INTENT, quote_id: old.body.quote_id, billing_profile_id: uuid(42) });
  assert.equal(r.body.code, "quote_expired");
  s.quoteTtl = undefined;
  s.profiles[0].address.country = "DE";
  const q = await call("POST", "/v1/credits/quotes", { pack_id: "usd_10", currency: "EUR" });
  const eu = await call("POST", "/v1/credits/checkout", { intent_id: "ef".repeat(16), quote_id: q.body.quote_id, billing_profile_id: uuid(42) });
  assert.equal(eu.status, 403);
  assert.equal(eu.body.code, "eu_uk_consumer_web_unavailable");
  s.profiles[0].address.country = "TR";
  delete s.profiles[0].billing_email;
  const q2 = await call("POST", "/v1/credits/quotes", { pack_id: "usd_10", currency: "USD" });
  const bad = await call("POST", "/v1/credits/checkout", { intent_id: "12".repeat(16), quote_id: q2.body.quote_id, billing_profile_id: uuid(42) });
  assert.equal(bad.body.code, "billing_profile_incomplete");
  assert.equal(crCalls(s, "/checkouts").length, 0);
});

test("only USD, EUR and TRY that are open can be quoted", async (t) => {
  const { call } = await setup(t, { OPEN_CURRENCIES: "TRY,USD" });
  assert.deepEqual((await call("GET", "/v1/credits/packs")).body.currencies, ["USD", "TRY"]);
  const r = await call("POST", "/v1/credits/quotes", { pack_id: "usd_10", currency: "EUR" });
  assert.equal(r.status, 409);
  assert.equal(r.body.code, "currency_unavailable");
});

test("the postbox's hold, capture, release and balance routes need the internal token and stay off /api", async (t) => {
  const { s, call } = await setup(t);
  const internal = (method, path, body, auth = `Bearer ${INTERNAL}`, prefix = "") => call(method, path, body, auth, prefix);
  const hold = { owner: { owner_kind: "account", owner_id: "acct_rich000000" }, offer_id: "mpo_x", job_ref: "job_a", idempotency_key: "job_a" };

  const ok = await internal("POST", "/internal/credits/holds", hold);
  assert.equal(ok.status, 201);
  assert.deepEqual(ok.body, { hold_id: "hold_1", amount_cents: 2500, expires_at: "2030-01-01T00:00:00Z" });
  assert.deepEqual(crCalls(s, "/holds")[0].body, hold);

  const poor = await internal("POST", "/internal/credits/holds", { ...hold, owner: { owner_kind: "account", owner_id: "acct_poor000000" } });
  assert.equal(poor.status, 402);
  assert.equal(poor.body.code, "insufficient_credit");
  assert.equal(poor.body.available_cents, 100);
  assert.equal(poor.body.required_cents, 2500);

  const cap = await internal("POST", "/internal/credits/holds/hold_1/capture", { job_ref: "job_a" });
  assert.equal(cap.status, 200);
  assert.deepEqual(cap.body, { sale_id: "sale_1", amount_cents: 2500, commission_bps: 1000, commission_cents: 250, publisher_cents: 2250 });

  const captured = await internal("POST", "/internal/credits/holds/hold_1/release", { reason: "failed" });
  assert.equal(captured.status, 409);
  assert.equal(captured.body.code, "hold_captured");
  assert.deepEqual((await internal("POST", "/internal/credits/holds/hold_2/release", { reason: "deadline_passed" })).body, { released: true });

  const bal = await internal("GET", "/internal/credits/balances/account/acct_rich000000");
  assert.equal(bal.status, 200);
  assert.equal(bal.body.available_cents, 7500);

  // Bad input never reaches saas_be.
  const n = s.calls.length;
  assert.equal((await internal("POST", "/internal/credits/holds", { ...hold, owner: { owner_kind: "person", owner_id: "x" } })).status, 400);
  assert.equal((await internal("POST", "/internal/credits/holds", { ...hold, price_cents: 1 })).status, 400);
  assert.equal((await internal("POST", "/internal/credits/holds/hold_1/release", { reason: "Bad Reason!" })).status, 400);
  assert.equal(s.calls.length, n);

  assert.equal((await internal("POST", "/internal/credits/holds", hold, "Bearer wrong-token")).status, 401);
  assert.equal((await internal("POST", "/internal/credits/holds", hold, MEMBER)).status, 401);
  assert.equal((await internal("GET", "/internal/credits/balances/account/acct_rich000000", null, `Bearer ${INTERNAL}`, "/api")).status, 404);
});

test("a per-job price is mirrored as a USD offer whatever card currencies are open", async (t) => {
  const { s, call } = await setup(t, { OPEN_CURRENCIES: "TRY" });
  const offer = { listing_id: "lst_a", publisher_id: "pub_a", release_line: "v1", title: "Wodo AI HR — HR research",
    term: "per_job", currency: "USD", price_minor: 2500, status: "active" };
  const r = await call("PUT", "/internal/marketplace/offers/mpo_job", offer, `Bearer ${INTERNAL}`, "");
  assert.equal(r.status, 200, JSON.stringify(r.body));
  assert.equal(r.body.offer.term, "per_job");
  assert.equal(s.calls.at(-1).body.term, "per_job");
  assert.equal(s.calls.filter((c) => c.path.endsWith("/provision")).length, 0);
  const eur = await call("PUT", "/internal/marketplace/offers/mpo_job2", { ...offer, currency: "EUR" }, `Bearer ${INTERNAL}`, "");
  assert.equal(eur.status, 400);
});

test("credits stay closed without the marketplace tokens", async (t) => {
  const { call } = await setup(t, { SAAS_MARKETPLACE_TOKEN: "", BRIDGE_INTERNAL_TOKEN: "" });
  assert.equal((await call("GET", "/v1/credits/packs")).status, 503);
  assert.equal((await call("POST", "/internal/credits/holds", {}, `Bearer ${INTERNAL}`, "")).status, 404);
});
