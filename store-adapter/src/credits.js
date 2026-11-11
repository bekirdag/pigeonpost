// Prepaid credits (credits v1): Pigeonpost web and postbox <-> this bridge <-> saas_be's credit
// routes (`/v1/internal/tenants/{tenant}/marketplace/credits`, scope `marketplace:bridge`).
//
// Credits are kept and spent in USD cents. A person buys a fixed pack ($10, $25, $50, $100) with a
// one-time iyzico payment in USD, EUR or TRY; saas_be quotes the charge (TRY and EUR converted at
// its stored rate, fixed for the quote's life), hosts the 3DS payment page and credits the account
// only on a verified result. Third-party publisher-hosted jobs are paid from that credit: the
// postbox holds the job's price before queueing it, captures the hold once the result is
// delivered (saas_be journals the sale: 10% to Piyote, the rest payable to the publisher) and
// releases it on every other ending.
//
// Who decides what:
//  - saas_be owns the money: packs, quotes, FX, the payment page, purchases, balances, holds,
//    sales and the journal. It is the only judge of "paid" and of "enough credit".
//  - This bridge decides who the owner is: always the signed-in account, from its own member
//    token, never from a request body. It keeps the quotes and checkouts it started so one account
//    can never use another's.
//  - The postbox decides when a job is admitted, captured or released, and calls the internal
//    routes with BRIDGE_INTERNAL_TOKEN.
//
// Routes (server.js mounts them):
//   member token (as POST /v1/marketplace/checkout): GET /v1/credits/packs, POST /v1/credits/quotes,
//     POST /v1/credits/checkout, GET /v1/credits/checkout/{intent}, GET /v1/credits/balance,
//     GET /v1/credits/history
//   postbox, BRIDGE_INTERNAL_TOKEN: POST /internal/credits/holds,
//     POST /internal/credits/holds/{hold}/capture, POST /internal/credits/holds/{hold}/release,
//     GET /internal/credits/balances/{owner_kind}/{owner_id}
//
// Refunds and staff adjustments need `marketplace:finance`; this bridge holds no such token and
// calls neither.

import { config } from "./config.js";
import { isUuid } from "./catalog.js";
import * as saas from "./saas.js";
import * as postbox from "./postbox.js";
import * as tax from "./tax.js";
import { State } from "./state.js";

const { fail } = saas;

export const PACKS = { usd_10: 1000, usd_25: 2500, usd_50: 5000, usd_100: 10000 };
const CURRENCIES = ["USD", "EUR", "TRY"];
const INTENT_ID = /^[a-f0-9]{32}$/;
// Ids saas_be makes (uuid or prefixed); job and offer ids the postbox makes.
const REF = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
const ACCOUNT = /^acct_[A-Za-z0-9_]{6,64}$/;
const ORG = /^org_[a-z0-9]{6,64}$/;
const CURSOR = /^[A-Za-z0-9._:=-]{1,256}$/;
const HOUR = 3600_000;
const iso = (v) => typeof v === "string" && Number.isFinite(Date.parse(v));
const cents = (v) => Number.isInteger(v) && v >= 0;

function strict(body, allowed, required = allowed) {
  if (!body || typeof body !== "object" || Array.isArray(body)) fail(400, "A JSON object is required.", "invalid_request");
  const extra = Object.keys(body).find((key) => !allowed.includes(key));
  if (extra) fail(400, `Unexpected field: ${extra}`, "invalid_request");
  const missing = required.find((key) => body[key] === undefined);
  if (missing) fail(400, `${missing} is required.`, "invalid_request");
  return body;
}

function httpsUrl(value) {
  try { return new URL(value).protocol === "https:"; } catch { return false; }
}

/// `{owner_kind, owner_id}` as saas_be names an owner. An account today; `org` is reserved.
function validOwner(o) {
  if (!o || typeof o !== "object" || Array.isArray(o)) return false;
  return (o.owner_kind === "account" && ACCOUNT.test(o.owner_id || "")) || (o.owner_kind === "org" && ORG.test(o.owner_id || ""));
}

// ---- saas_be credit routes --------------------------------------------------------------------------

function cr(method, path, body) {
  if (!config.marketplace.enabled) fail(503, "Credits are not open yet.", "not_configured");
  return saas.request(`${config.masaasSaasApiBase}/v1/internal/tenants/${encodeURIComponent(config.tenantId)}/marketplace/credits${path}`, {
    method,
    headers: { accept: "application/json", authorization: `Bearer ${config.marketplace.bridgeToken}`,
      "user-agent": "pigeonpost billing bridge", ...(body ? { "content-type": "application/json" } : {}) },
    body, timeoutMs: config.checkoutTimeoutMs, keep: [402],
  });
}

const invalid = (what) => fail(502, `Billing returned an invalid ${what}.`, "invalid_upstream");
const items = (v) => (Array.isArray(v) ? v : Array.isArray(v?.items) ? v.items : Array.isArray(v?.data) ? v.data : null);

function validBalance(b) {
  if (!b || !cents(b.available_cents) || !cents(b.held_cents)) invalid("balance");
  return { available_cents: b.available_cents, held_cents: b.held_cents };
}

function validQuote(q, pack, currency) {
  const c = q?.charge;
  if (!q || !REF.test(String(q.quote_id || "")) || q.pack_id !== pack || q.credit_cents !== PACKS[pack]
    || !c || c.currency !== currency || !Number.isInteger(c.amount_minor) || c.amount_minor < 1
    || !cents(c.tax_minor) || !iso(q.expires_at)
    || !(q.fx === null || q.fx === undefined || (typeof q.fx === "object" && Number(q.fx.rate) > 0))) invalid("quote");
  return { quote_id: q.quote_id, pack_id: q.pack_id, credit_cents: q.credit_cents,
    charge: { currency: c.currency, amount_minor: c.amount_minor, tax_minor: c.tax_minor },
    fx: q.fx ? { rate: q.fx.rate, source: q.fx.source ?? null, as_of: q.fx.as_of ?? null } : null,
    expires_at: q.expires_at };
}

/// One history entry, with only the fields a person needs to read their own history.
/// Kinds are saas_be's (purchase, hold, release, sale, refund, adjustment, ...), snake_case.
function historyEntry(e) {
  if (!e || typeof e !== "object") return null;
  const kind = String(e.kind || e.type || "");
  if (!/^[a-z][a-z_]{0,31}$/.test(kind) || !Number.isInteger(e.amount_cents)) return null;
  const at = [e.created_at, e.occurred_at, e.at].find(iso) || null;
  const pick = (k) => (typeof e[k] === "string" && e[k].length <= 200 ? e[k] : null);
  return { kind, amount_cents: e.amount_cents, created_at: at, status: pick("status"),
    description: pick("description") || pick("title") || pick("reason"), listing_id: pick("listing_id"),
    job_ref: pick("job_ref"), purchase_id: pick("purchase_id"),
    charge: e.charge && Number.isInteger(e.charge.amount_minor) && CURRENCIES.includes(e.charge.currency)
      ? { amount_minor: e.charge.amount_minor, currency: e.charge.currency } : null };
}

// ---- the module -------------------------------------------------------------------------------------

export function createCredits(dir) {
  // Quotes and checkouts this bridge started, by account. No card data and no payment page links
  // are written to disk; a replay in this process gets the same link back.
  const state = new State(dir, "credits-state.json", { version: 1, quotes: {}, checkouts: {} });
  const pages = new Map();

  const ownerOf = (account) => ({ owner_kind: "account", owner_id: account });

  // ---- member routes -------------------------------------------------------------------------------

  async function packs(token) {
    await postbox.accountFor(token);
    const list = items(await cr("GET", "/packs"));
    if (!list) invalid("pack list");
    const out = list.filter((p) => p && p.active === true && PACKS[p.pack_id] === p.amount_cents)
      .map((p) => ({ pack_id: p.pack_id, amount_cents: p.amount_cents }))
      .sort((a, b) => a.amount_cents - b.amount_cents);
    return { packs: out, currencies: CURRENCIES.filter((c) => config.openCurrencies.includes(c)) };
  }

  async function quote(token, body) {
    strict(body, ["pack_id", "currency"]);
    if (!Object.hasOwn(PACKS, body.pack_id)) fail(400, "Choose one of the credit packs.", "invalid_request");
    if (!CURRENCIES.includes(body.currency)) fail(400, "currency must be USD, EUR or TRY.", "invalid_request");
    if (!config.openCurrencies.includes(body.currency)) fail(409, `Payments in ${body.currency} are not open yet.`, "currency_unavailable");
    const account = await postbox.accountFor(token);
    const q = validQuote(await cr("POST", "/quotes", { owner: ownerOf(account), pack_id: body.pack_id, currency: body.currency }),
      body.pack_id, body.currency);
    await state.update((data) => {
      // Old quotes go; a quote lives minutes.
      for (const [id, rec] of Object.entries(data.quotes)) if (Date.now() - rec.created_at > 24 * HOUR) delete data.quotes[id];
      data.quotes[q.quote_id] = { account, ...q, created_at: Date.now() };
    });
    return q;
  }

  /// The buyer details saas_be passes to iyzico, from the account's own billing profile.
  function buyerOf(profile, userId, ip) {
    const name = String(profile.legal_name || "").trim().replace(/\s+/g, " ");
    const at = name.lastIndexOf(" ");
    const a = profile.address || {};
    const buyer = { user_id: userId, billing_profile_id: profile.id, name: at > 0 ? name.slice(0, at) : name, surname: at > 0 ? name.slice(at + 1) : name,
      email: profile.billing_email, ip, country: a.country, city: a.city,
      address: [a.line1, a.line2, a.postal_code].filter((x) => typeof x === "string" && x.trim()).join(", ") };
    if (typeof profile.identity_number === "string" && profile.identity_number.trim()) buyer.identity_number = profile.identity_number.trim();
    if (!name || !buyer.email || !buyer.country || !buyer.city || !buyer.address) {
      fail(400, "Complete your billing details (name, billing contact, address, city, country) first.", "billing_profile_incomplete");
    }
    return buyer;
  }

  function checkoutView(id, rec) {
    return { intent_id: id, checkout_id: rec.checkout_id || null, payment_page_url: pages.get(id) || null,
      expires_at: rec.expires_at || null, pack_id: rec.pack_id, credit_cents: rec.credit_cents, charge: rec.charge };
  }

  async function checkout(token, body, { ipCountry = null, buyerIp = null } = {}) {
    strict(body, ["intent_id", "quote_id", "billing_profile_id"]);
    if (!INTENT_ID.test(body.intent_id || "")) fail(400, "Reload the page before starting again.", "invalid_intent");
    if (!REF.test(String(body.quote_id || ""))) fail(400, "Choose a pack again.", "invalid_request");
    if (!isUuid(body.billing_profile_id)) fail(400, "Save your billing details first.", "billing_profile_required");
    if (!config.marketplace.enabled) fail(503, "Credits are not open yet.", "not_configured");
    const account = await postbox.accountFor(token);
    const id = body.intent_id;
    let rec = await state.read((data) => data.checkouts[id] && { ...data.checkouts[id] });
    if (rec && (rec.account !== account || rec.quote_id !== body.quote_id || rec.billing_profile_id !== body.billing_profile_id)) {
      fail(409, "This purchase was started with different details. Reload and try again.", "intent_conflict");
    }
    if (rec?.status === "failed") fail(409, "This purchase did not start. Reload and try again.", "intent_failed");
    if (rec?.checkout_id) {
      if (!pages.get(id)) fail(409, "This payment page can no longer be shown. Start the purchase again.", "checkout_page_lost");
      return checkoutView(id, rec);
    }
    const q = await state.read((data) => data.quotes[body.quote_id] && { ...data.quotes[body.quote_id] });
    // Only a quote this bridge made for this account; anyone else's is "not found".
    if (!q || q.account !== account) fail(404, "Choose a pack again.", "quote_not_found");
    if (Date.parse(q.expires_at) <= Date.now()) fail(409, "This price has expired. Choose a pack again.", "quote_expired");
    const ip = String(buyerIp || "");
    if (!ip) fail(400, "We could not tell where this request came from.", "buyer_ip_unknown");
    const profiles = await saas.listBillingProfiles(token);
    const profile = items(profiles)?.find((p) => p && p.id === body.billing_profile_id);
    if (!profile) fail(400, "Save your billing details first.", "billing_profile_required");
    // The EU/UK VAT rule (docs/billing-tax.md, D-34), as for every web sale.
    const decision = await tax.decide(profile, ipCountry);
    const user = await saas.profile(token);
    if (!isUuid(user?.id)) fail(502, "Billing returned an invalid profile.", "invalid_upstream");
    const buyer = buyerOf(profile, user.id, ip);
    const outcome = await state.update((data) => {
      if (data.checkouts[id]) return { existing: data.checkouts[id] };
      const recent = Object.values(data.checkouts).filter((c) => c.account === account && c.created_at > Date.now() - HOUR).length;
      if (recent >= config.checkoutsPerHour) return { limited: true };
      data.checkouts[id] = { account, quote_id: q.quote_id, billing_profile_id: body.billing_profile_id, pack_id: q.pack_id,
        credit_cents: q.credit_cents, charge: q.charge, fx: q.fx, tax: decision, status: "starting",
        created_at: Date.now(), updated_at: Date.now() };
      return { created: data.checkouts[id] };
    });
    if (outcome.limited) fail(429, "Too many purchases started in the last hour. Try again later.", "rate_limited");
    rec = { ...(outcome.created || outcome.existing) };
    const save = (patch) => state.update((data) => Object.assign(data.checkouts[id], patch, { updated_at: Date.now() }));
    try {
      const c = await cr("POST", "/checkouts", { quote_id: q.quote_id, owner: ownerOf(account), buyer,
        return_url: `${config.allowedOrigins[0]}/account?credits=${id}#credits`, idempotency_key: `pp-credits-${id}` });
      if (!REF.test(String(c?.checkout_id || "")) || !httpsUrl(c.payment_page_url) || String(c.payment_page_url).length > 2000) invalid("checkout");
      pages.set(id, c.payment_page_url);
      await save({ checkout_id: c.checkout_id, status: "pending", expires_at: iso(c.expires_at) ? c.expires_at : null });
      return checkoutView(id, await state.read((data) => ({ ...data.checkouts[id] })));
    } catch (error) {
      if ([400, 403, 404, 409, 422].includes(error.status)) await save({ status: "failed" });
      throw error;
    }
  }

  async function checkoutStatus(token, id) {
    if (!INTENT_ID.test(id || "")) fail(404, "Purchase not found", "not_found");
    const account = await postbox.accountFor(token);
    const rec = await state.read((data) => data.checkouts[id] && { ...data.checkouts[id] });
    if (!rec || rec.account !== account) fail(404, "Purchase not found", "not_found");
    let status = rec.status === "failed" ? "failed" : "pending";
    let purchase = rec.purchase_id || null;
    if (rec.checkout_id && !["paid", "failed", "expired"].includes(rec.status)) {
      const s = await cr("GET", `/checkouts/${encodeURIComponent(rec.checkout_id)}`);
      if (!s || s.checkout_id !== rec.checkout_id || !["pending", "paid", "failed", "expired"].includes(s.status)) invalid("checkout");
      status = s.status;
      purchase = typeof s.purchase_id === "string" && REF.test(s.purchase_id) ? s.purchase_id : null;
      if (status !== "pending") await state.update((data) => Object.assign(data.checkouts[id], { status, purchase_id: purchase, updated_at: Date.now() }));
    } else if (rec.checkout_id) {
      status = rec.status;
    }
    return { intent_id: id, checkout_id: rec.checkout_id || null, status, purchase_id: purchase,
      pack_id: rec.pack_id, credit_cents: rec.credit_cents, charge: rec.charge };
  }

  async function balance(token) {
    const account = await postbox.accountFor(token);
    return { owner: ownerOf(account), currency: "USD", ...validBalance(await cr("GET", `/balances/account/${encodeURIComponent(account)}`)) };
  }

  async function history(token, query) {
    const account = await postbox.accountFor(token);
    const limit = query.has("limit") ? Number(query.get("limit")) : 20;
    if (!Number.isInteger(limit) || limit < 1 || limit > 100) fail(400, "limit must be 1 to 100.", "invalid_request");
    const cursor = query.get("cursor");
    if (cursor !== null && !CURSOR.test(cursor)) fail(400, "cursor is invalid.", "invalid_request");
    const page = await cr("GET", `/history/account/${encodeURIComponent(account)}?limit=${limit}${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ""}`);
    const list = items(page);
    if (!list) invalid("history");
    const next = typeof page?.next_cursor === "string" && CURSOR.test(page.next_cursor) ? page.next_cursor : null;
    return { entries: list.map(historyEntry).filter(Boolean), next_cursor: next };
  }

  // ---- postbox (internal) ----------------------------------------------------------------------------

  async function hold(body) {
    strict(body, ["owner", "offer_id", "job_ref", "idempotency_key"]);
    if (!validOwner(body.owner)) fail(400, "owner is invalid.", "invalid_request");
    for (const key of ["offer_id", "job_ref", "idempotency_key"]) if (!REF.test(String(body[key] || ""))) fail(400, `${key} is invalid.`, "invalid_request");
    let h;
    try {
      h = await cr("POST", "/holds", { owner: { owner_kind: body.owner.owner_kind, owner_id: body.owner.owner_id },
        offer_id: body.offer_id, job_ref: body.job_ref, idempotency_key: body.idempotency_key });
    } catch (error) {
      if (error.status === 402) {
        const u = error.upstream || {};
        const n = (k) => (cents(u[k]) ? u[k] : cents(u.details?.[k]) ? u.details[k] : null);
        fail(402, "Not enough credit for this job.", "insufficient_credit", { available_cents: n("available_cents"), required_cents: n("required_cents") });
      }
      throw error;
    }
    if (!REF.test(String(h?.hold_id || "")) || !Number.isInteger(h.amount_cents) || h.amount_cents < 1 || !iso(h.expires_at)) invalid("hold");
    return { hold_id: h.hold_id, amount_cents: h.amount_cents, expires_at: h.expires_at };
  }

  async function capture(holdId, body) {
    if (!REF.test(holdId || "")) fail(404, "Hold not found", "not_found");
    strict(body, ["job_ref"]);
    if (!REF.test(String(body.job_ref || ""))) fail(400, "job_ref is invalid.", "invalid_request");
    const s = await cr("POST", `/holds/${encodeURIComponent(holdId)}/capture`, { job_ref: body.job_ref });
    if (!REF.test(String(s?.sale_id || "")) || ![s.amount_cents, s.commission_bps, s.commission_cents, s.publisher_cents].every(cents)
      || s.commission_cents + s.publisher_cents !== s.amount_cents) invalid("sale");
    return { sale_id: s.sale_id, amount_cents: s.amount_cents, commission_bps: s.commission_bps,
      commission_cents: s.commission_cents, publisher_cents: s.publisher_cents };
  }

  async function release(holdId, body) {
    if (!REF.test(holdId || "")) fail(404, "Hold not found", "not_found");
    strict(body, ["reason"]);
    if (typeof body.reason !== "string" || !/^[a-z][a-z0-9_]{0,63}$/.test(body.reason)) fail(400, "reason is invalid.", "invalid_request");
    try {
      const r = await cr("POST", `/holds/${encodeURIComponent(holdId)}/release`, { reason: body.reason });
      if (r?.released !== true) invalid("release");
    } catch (error) {
      if (error.status === 409) fail(409, "This hold was captured and cannot be released.", "hold_captured");
      throw error;
    }
    return { released: true };
  }

  async function ownerBalance(kind, id) {
    if (!validOwner({ owner_kind: kind, owner_id: id })) fail(404, "Owner not found", "not_found");
    return validBalance(await cr("GET", `/balances/${kind}/${encodeURIComponent(id)}`));
  }

  return { packs, quote, checkout, checkoutStatus, balance, history, hold, capture, release, ownerBalance };
}
