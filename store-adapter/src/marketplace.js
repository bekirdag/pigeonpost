// Paid marketplace purchases: Pigeonpost postbox (listings, installations) <-> this bridge <->
// saas_be marketplace finance (`/v1/internal/tenants/{tenant}/marketplace`, scope
// `marketplace:bridge`). Card terms: one-off and monthly. Publisher-hosted jobs are sold for
// prepaid credits instead (credits.js); their `per_job` USD offers are mirrored through here too.
//
// Who decides what:
//  - The postbox owns listings, prices as publishers set them, and installations. A buyer's
//    purchase starts there (`POST /v1/marketplace/purchases`, the buyer's own token), which checks
//    the listing is published, installable for this buyer and on sale, and names the offer, the
//    installation id the payment will admit, and the exact intent.
//  - saas_be owns the money: offers, quotes, the hosted iyzico form, payment authorizations,
//    licences, invoices and the journal. It is the only judge of "paid".
//  - This bridge holds nothing a buyer can spend: the single-use authorization token stays here,
//    and the postbox consumes it through the narrow internal route below for exactly the intent the
//    payment was bound to.
//
// Routes (server.js mounts them):
//   buyer, member token: POST /v1/marketplace/checkout, GET /v1/marketplace/checkout/{intent},
//     GET /v1/marketplace/licences, GET /v1/marketplace/installations/{installation},
//     POST /v1/marketplace/installations/{installation}/cancel
//   postbox, BRIDGE_INTERNAL_TOKEN: PUT /internal/marketplace/offers/{offer} (one_off, monthly or
//     per_job),
//     POST /internal/marketplace/authorizations/consume
//
// Finance routes (payouts, refunds, chargebacks, statements, journal) need `marketplace:finance`
// and are staff tools; this bridge holds no such token and calls none of them.

import { createHash, timingSafeEqual } from "node:crypto";
import { isIP } from "node:net";
import { config } from "./config.js";
import { isUuid } from "./catalog.js";
import * as saas from "./saas.js";
import * as postbox from "./postbox.js";
import * as tax from "./tax.js";
import { State } from "./state.js";

const { fail } = saas;
const INTENT_ID = /^[a-f0-9]{32}$/;
const ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$/;
const INSTALLATION = /^ins_[a-z2-7]{16,32}$/;
const ACCOUNT = /^acct_[A-Za-z0-9_]{6,64}$/;
const ORG = /^org_[a-z0-9]{6,64}$/;
const TERMS = ["one_off", "monthly"];
// Offer terms: the card terms, and `per_job` (credits v1: a publisher-hosted operation, USD only,
// held and captured against prepaid credit, never sold through a card checkout).
const OFFER_TERMS = [...TERMS, "per_job"];
const CURRENCIES = ["TRY", "USD", "EUR"];
const LICENCE_STATUSES = ["active", "canceling", "paused", "expired", "terminated", "revoked"];
const AUTH_STATUSES = ["pending_payment", "authorized", "consumed", "failed", "expired", "revoked"];
const DAY = 86400_000;
const fingerprint = (value) => createHash("sha256").update(JSON.stringify(value)).digest("hex");
const iso = (v) => v === null || v === undefined || (typeof v === "string" && Number.isFinite(Date.parse(v)));

function strict(body, allowed, required = allowed) {
  if (!body || typeof body !== "object" || Array.isArray(body)) fail(400, "A JSON object is required.", "invalid_request");
  const extra = Object.keys(body).find((key) => !allowed.includes(key));
  if (extra) fail(400, `Unexpected field: ${extra}`, "invalid_request");
  const missing = required.find((key) => body[key] === undefined);
  if (missing) fail(400, `${missing} is required.`, "invalid_request");
  return body;
}

/// An intent as saas_be hashes it: an object of 1–16 non-empty strings with snake_case keys,
/// compared by its sorted entries.
function canonicalIntent(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const entries = Object.entries(value);
  if (entries.length < 1 || entries.length > 16) return null;
  if (!entries.every(([k, v]) => /^[a-z][a-z0-9_]{0,63}$/.test(k) && typeof v === "string" && v && v.length <= 256)) return null;
  return JSON.stringify(entries.sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)));
}

function sameSecret(a, b) {
  const x = Buffer.from(String(a || "")), y = Buffer.from(String(b || ""));
  return x.length === y.length && x.length > 0 && timingSafeEqual(x, y);
}

// ---- saas_be marketplace client -------------------------------------------------------------------

function mp(method, path, body) {
  if (!config.marketplace.enabled) fail(503, "Marketplace payments are not configured.", "not_configured");
  return saas.request(`${config.masaasSaasApiBase}/v1/internal/tenants/${encodeURIComponent(config.tenantId)}/marketplace${path}`, {
    method,
    headers: { accept: "application/json", authorization: `Bearer ${config.marketplace.bridgeToken}`,
      "user-agent": "pigeonpost billing bridge", ...(body ? { "content-type": "application/json" } : {}) },
    body, timeoutMs: config.checkoutTimeoutMs,
  });
}

function validOfferView(o, id) {
  if (!o || o.offer_id !== id || !OFFER_TERMS.includes(o.term) || !CURRENCIES.includes(o.currency)
    || !Number.isInteger(o.price_minor) || !["active", "retired"].includes(o.status) || typeof o.sellable !== "boolean") {
    fail(502, "Billing returned an invalid offer.", "invalid_upstream");
  }
  return o;
}

export function validLicence(l) {
  if (!l || !isUuid(l.licence_id) || !LICENCE_STATUSES.includes(l.status) || typeof l.access !== "boolean"
    || !TERMS.includes(l.term) || !["account", "org"].includes(l.owner_kind) || typeof l.owner_id !== "string"
    || typeof l.listing_id !== "string" || ![l.paid_through, l.deletion_due_at, l.paused_at, l.revoked_at].every(iso)) {
    fail(502, "Billing returned an invalid licence.", "invalid_upstream");
  }
  return l;
}

function validAuthorization(a, id) {
  if (!a || a.authorization_id !== id || !AUTH_STATUSES.includes(a.status) || ![a.authorized_at, a.consumed_at, a.expires_at].every(iso)
    || (a.licence_id !== null && a.licence_id !== undefined && !isUuid(a.licence_id))) {
    fail(502, "Billing returned an invalid payment authorization.", "invalid_upstream");
  }
  return a;
}

function validPayment(p) {
  return p && isUuid(p.payment_id) && Number.isInteger(p.amount_minor) && Number.isInteger(p.tax_minor)
    && CURRENCIES.includes(p.currency) && iso(p.settled_at) && iso(p.period_start) && iso(p.period_end);
}

// ---- the module -------------------------------------------------------------------------------------

export function createMarketplace(dir) {
  const state = new State(dir, "marketplace-state.json", { version: 1, purchases: {}, offers: {} });
  // Hosted forms are never written to disk; a replay within this process gets the same form back.
  const forms = new Map();

  const save = (intent, patch) => state.update((data) => Object.assign(data.purchases[intent], patch, { updated_at: Date.now() }));
  const byInstallation = (id) => state.read((data) => {
    const found = Object.entries(data.purchases).find(([, p]) => p.installation_id === id);
    return found ? { intent_id: found[0], ...found[1] } : null;
  });

  // ---- who may act for an owner ----------------------------------------------------------------

  /// The owner a purchase is for. An account buys for itself; an org purchase needs someone who
  /// manages the org's billing (the postbox's `can_manage`, as for the enterprise plan).
  async function ownerFor(token, owner, { manage }) {
    const account = await postbox.accountFor(token);
    const kind = owner?.kind ?? "account";
    if (kind === "account") {
      if (owner?.id !== undefined && owner.id !== account) fail(403, "You can buy only for your own account or an organization whose billing you manage.", "forbidden");
      return { account, owner: { kind: "account", id: account } };
    }
    if (kind !== "org" || !ORG.test(owner?.id || "")) fail(400, "owner must be your account or an organization.", "invalid_request");
    const billing = await postbox.orgBilling(token, owner.id);
    if (manage && billing.can_manage !== true) fail(403, "Only the owner or a billing admin can buy for the organization.", "forbidden");
    return { account, owner: { kind: "org", id: owner.id } };
  }

  /// A purchase the caller may see (`manage`: act on). Anyone else gets 404.
  async function ownedPurchase(token, installationId, { manage }) {
    if (!INSTALLATION.test(installationId || "")) fail(404, "Purchase not found", "not_found");
    const rec = await byInstallation(installationId);
    if (!rec) fail(404, "Purchase not found", "not_found");
    try {
      await ownerFor(token, rec.owner, { manage });
    } catch (error) {
      if ([403, 404].includes(error.status)) fail(404, "Purchase not found", "not_found");
      throw error;
    }
    return rec;
  }

  // ---- tax (D-34) --------------------------------------------------------------------------------

  async function billingProfile(token, id) {
    const profiles = await saas.listBillingProfiles(token);
    const list = Array.isArray(profiles) ? profiles : Array.isArray(profiles?.data) ? profiles.data : [];
    const found = list.find((p) => p && p.id === id);
    if (!found) fail(400, "Save your billing details first.", "billing_profile_required");
    return found;
  }

  // ---- buyer: checkout ---------------------------------------------------------------------------

  function validPurchase(p, listingId, owner) {
    const o = p?.offer;
    if (!p || !INSTALLATION.test(p.installation_id || "") || p.listing_id !== listingId
      || p.owner_kind !== owner.kind || p.owner_id !== owner.id
      || !o || !ID.test(o.offer_id || "") || !TERMS.includes(o.term) || !CURRENCIES.includes(o.currency)
      || !Number.isInteger(o.amount_minor) || o.amount_minor < 1 || !ID.test(o.release_line || "")
      || canonicalIntent(p.intent) === null || p.intent.installation_id !== p.installation_id
      || p.intent.listing_id !== listingId || p.intent.owner_kind !== owner.kind || p.intent.owner_id !== owner.id) {
      fail(502, "The marketplace answered unexpectedly.", "postbox_invalid");
    }
    return p;
  }

  async function checkout(token, body, { ipCountry = null, buyerIp = null } = {}) {
    strict(body, ["intent_id", "listing_id", "owner", "billing_profile_id", "locale"], ["intent_id", "listing_id", "billing_profile_id"]);
    if (!INTENT_ID.test(body.intent_id || "")) fail(400, "Reload the page before starting again.", "invalid_intent");
    if (!ID.test(body.listing_id || "")) fail(400, "Choose a listing.", "invalid_request");
    if (!isUuid(body.billing_profile_id)) fail(400, "Save your billing details first.", "billing_profile_required");
    if (body.owner !== undefined) strict(body.owner, ["kind", "id"], ["kind"]);
    const locale = body.locale === undefined ? "en" : ["tr", "en"].includes(body.locale) ? body.locale : fail(400, "locale must be tr or en.", "invalid_request");
    if (!config.marketplace.enabled) fail(503, "Marketplace payments are not open yet.", "not_configured");
    const { account, owner } = await ownerFor(token, body.owner, { manage: true });
    const request = { listing_id: body.listing_id, owner, billing_profile_id: body.billing_profile_id, locale };
    const print = fingerprint(request);
    const id = body.intent_id;

    let rec = await state.read((data) => data.purchases[id] && { ...data.purchases[id] });
    if (rec && (rec.account !== account || rec.fingerprint !== print)) fail(409, "This purchase was started with different details. Reload and try again.", "intent_conflict");
    if (rec?.status === "failed") fail(409, "This purchase did not start. Reload and try again.", "intent_failed");
    if (rec?.checkout_id) return checkoutView(id, rec, forms.get(id) || null);

    if (!rec) {
      // The postbox decides whether this listing may be bought by this owner, and what it costs.
      const purchase = validPurchase(await postbox.marketplaceCall(token, "POST", "/v1/marketplace/purchases",
        { listing_id: body.listing_id, owner_kind: owner.kind, owner_id: owner.id, intent_id: id }), body.listing_id, owner);
      if (!config.openCurrencies.includes(purchase.offer.currency)) fail(503, `Payments in ${purchase.offer.currency} are not open yet.`, "currency_unavailable");
      // The EU/UK VAT rule (docs/billing-tax.md, D-34), exactly as for Nest packages: decided from
      // the billing profile and the request's country before anything is reserved or quoted.
      const decision = await tax.decide(await billingProfile(token, body.billing_profile_id), ipCountry);
      const user = await saas.profile(token);
      if (!isUuid(user?.id)) fail(502, "Billing returned an invalid profile.", "invalid_upstream");
      const outcome = await state.update((data) => {
        if (data.purchases[id]) return { existing: data.purchases[id] };
        const since = Date.now() - 3600_000;
        const recent = Object.values(data.purchases).filter((p) => p.account === account && p.created_at > since).length;
        if (recent >= config.checkoutsPerHour) return { limited: true };
        data.purchases[id] = { account, owner, listing_id: body.listing_id, installation_id: purchase.installation_id,
          offer: purchase.offer, intent: purchase.intent, billing_profile_id: body.billing_profile_id, user_id: user.id,
          tax: decision, locale, fingerprint: print, status: "starting", created_at: Date.now(), updated_at: Date.now() };
        return { created: data.purchases[id] };
      });
      if (outcome.limited) fail(429, "Too many checkouts started in the last hour. Try again later.", "rate_limited");
      rec = { ...(outcome.created || outcome.existing) };
      if (rec.account !== account || rec.fingerprint !== print) fail(409, "This purchase was started with different details. Reload and try again.", "intent_conflict");
    }

    try {
      if (!rec.quote_id) {
        const q = await mp("POST", "/quotes", { offer_id: rec.offer.offer_id,
          buyer: { owner_kind: rec.owner.kind, owner_id: rec.owner.id, user_id: rec.user_id, billing_profile_id: rec.billing_profile_id },
          intent: rec.intent });
        if (!isUuid(q?.quote_id) || q.offer_id !== rec.offer.offer_id || q.amount_minor !== rec.offer.amount_minor
          || q.currency !== rec.offer.currency || q.term !== rec.offer.term || q.tax_inclusive !== true) {
          fail(503, "The price of this listing has changed. Reload and try again.", "offer_mismatch");
        }
        await save(id, { quote_id: q.quote_id, intent_hash: q.intent_hash || null });
        rec.quote_id = q.quote_id;
      }
      const ip = rec.offer.term === "one_off" ? (isIP(String(buyerIp || "")) ? String(buyerIp) : fail(400, "We could not tell where this request came from.", "buyer_ip_unknown")) : undefined;
      const c = await mp("POST", `/quotes/${rec.quote_id}/checkout`, {
        return_url: `${config.allowedOrigins[0]}/account?mp_purchase=${id}`,
        ...(ip ? { buyer_ip: ip } : {}), locale: rec.locale,
      });
      if (!isUuid(c?.checkout_id) || !isUuid(c.authorization_id) || c.amount_minor !== rec.offer.amount_minor || c.currency !== rec.offer.currency
        || c.kind !== (rec.offer.term === "monthly" ? "subscription" : "one_off")) {
        fail(502, "Billing returned an invalid checkout.", "invalid_upstream");
      }
      const form = typeof c.checkout_form_content === "string" && c.checkout_form_content.trim() && c.checkout_form_content.length <= 200_000
        ? c.checkout_form_content : null;
      const tokenOk = typeof c.authorization_token === "string" && /^mpa_[A-Za-z0-9_-]{20,120}$/.test(c.authorization_token);
      await save(id, { checkout_id: c.checkout_id, authorization_id: c.authorization_id,
        ...(tokenOk ? { authorization_token: c.authorization_token } : {}), status: "checkout_pending", expires_at: c.expires_at || null });
      rec = await state.read((data) => ({ ...data.purchases[id] }));
      if (!form) {
        // saas_be shows a form once per quote; a lost first answer cannot be shown again.
        fail(409, "This checkout can no longer be shown. Start the purchase again.", "checkout_form_lost");
      }
      forms.set(id, form);
      return checkoutView(id, rec, form);
    } catch (error) {
      // A definite refusal ends this purchase; an ambiguous one stays replayable.
      if ([400, 403, 404, 422].includes(error.status) && error.code !== "buyer_ip_unknown") await save(id, { status: "failed", error: error.message });
      if (error.status === 409 && /live licence/i.test(error.message)) {
        await save(id, { status: "failed", error: error.message });
        fail(409, "This owner already holds a licence for this listing.", "licence_held");
      }
      throw error;
    }
  }

  function checkoutView(id, rec, form) {
    return { intent_id: id,
      installation_id: rec.installation_id, listing_id: rec.listing_id, owner: rec.owner,
      offer_id: rec.offer.offer_id, term: rec.offer.term, amount_minor: rec.offer.amount_minor, currency: rec.offer.currency,
      checkout_id: rec.checkout_id || null, checkout_form_content: form, expires_at: rec.expires_at || null };
  }

  /// Where a purchase stands: `pending` (not paid yet), `paid` (ready to install), `installed`,
  /// `failed`, `expired` (never paid) or `revoked` (refunded or charged back).
  function purchaseStatus(rec, auth) {
    if (!auth) return rec.status === "failed" ? "failed" : "pending";
    if (auth.status === "consumed") return "installed";
    if (auth.status === "authorized") return "paid";
    if (auth.status === "expired") return auth.authorized_at ? "paid" : "expired";
    if (auth.status === "pending_payment") return "pending";
    return auth.status;
  }

  async function checkoutStatus(token, intentId) {
    if (!INTENT_ID.test(intentId || "")) fail(404, "Checkout not found", "not_found");
    const account = await postbox.accountFor(token);
    const rec = await state.read((data) => data.purchases[intentId] && { ...data.purchases[intentId] });
    if (!rec || rec.account !== account) fail(404, "Checkout not found", "not_found");
    let auth = null;
    if (rec.authorization_id) {
      auth = validAuthorization(await mp("GET", `/authorizations/${rec.authorization_id}?refresh=${rec.status === "installed" ? "false" : "true"}`), rec.authorization_id);
      if (auth.licence_id && auth.licence_id !== rec.licence_id) await save(intentId, { licence_id: auth.licence_id });
    }
    const profile = await saas.listBillingProfiles(token).catch(() => null);
    const list = Array.isArray(profile) ? profile : Array.isArray(profile?.data) ? profile.data : [];
    return { intent_id: intentId, installation_id: rec.installation_id, listing_id: rec.listing_id, owner: rec.owner,
      term: rec.offer.term, amount_minor: rec.offer.amount_minor, currency: rec.offer.currency,
      status: purchaseStatus(rec, auth), licence_id: auth?.licence_id || rec.licence_id || null,
      receipt: tax.receipt(rec.tax, list.find((p) => p && p.id === rec.billing_profile_id) || null) };
  }

  // ---- buyer: licences, payments, cancel -----------------------------------------------------------

  async function licences(token, query) {
    const kind = query.get("owner_kind") || "account";
    const id = query.get("owner_id") || undefined;
    const { owner } = await ownerFor(token, { kind, id }, { manage: false });
    const listing = query.get("listing_id");
    if (listing && !ID.test(listing)) fail(400, "listing_id is invalid.", "invalid_request");
    const page = await mp("GET", `/licences?owner_kind=${owner.kind}&owner_id=${encodeURIComponent(owner.id)}${listing ? `&listing_id=${encodeURIComponent(listing)}` : ""}`);
    if (!Array.isArray(page?.items)) fail(502, "Billing returned an invalid licence list.", "invalid_upstream");
    const installs = await state.read((data) => Object.fromEntries(Object.values(data.purchases)
      .filter((p) => p.licence_id).map((p) => [p.licence_id, p.installation_id])));
    return { owner, licences: page.items.map(validLicence).map((l) => ({ ...publicLicence(l), installation_id: installs[l.licence_id] || null })) };
  }

  const publicLicence = (l) => ({ licence_id: l.licence_id, listing_id: l.listing_id, release_line: l.release_line ?? null,
    owner_kind: l.owner_kind, owner_id: l.owner_id, term: l.term, status: l.status, access: l.access,
    paid_through: l.paid_through ?? null, cancel_at_period_end: Boolean(l.cancel_at_period_end),
    paused_at: l.paused_at ?? null, deletion_due_at: l.deletion_due_at ?? null, revoked_at: l.revoked_at ?? null });

  async function installation(token, installationId) {
    const rec = await ownedPurchase(token, installationId, { manage: false });
    if (!rec.licence_id) fail(404, "This installation has no licence yet.", "no_licence");
    const [licence, payments] = await Promise.all([mp("GET", `/licences/${rec.licence_id}`), mp("GET", `/licences/${rec.licence_id}/payments`)]);
    validLicence(licence);
    if (!Array.isArray(payments?.items)) fail(502, "Billing returned an invalid payment list.", "invalid_upstream");
    const profiles = await saas.listBillingProfiles(token).catch(() => null);
    const list = Array.isArray(profiles) ? profiles : Array.isArray(profiles?.data) ? profiles.data : [];
    return { installation_id: rec.installation_id, listing_id: rec.listing_id, owner: rec.owner, term: rec.offer.term,
      licence: publicLicence(licence),
      // Receipts: each payment's invoice opens through GET /v1/billing/invoices/{invoice_id}/download.
      payments: payments.items.filter(validPayment).map((p) => ({ payment_id: p.payment_id, kind: p.kind, status: p.status,
        amount_minor: p.amount_minor, tax_minor: p.tax_minor, currency: p.currency, invoice_id: p.invoice_id ?? null,
        period_start: p.period_start ?? null, period_end: p.period_end ?? null, settled_at: p.settled_at, reversal_kind: p.reversal_kind ?? null })),
      receipt: tax.receipt(rec.tax, list.find((p) => p && p.id === rec.billing_profile_id) || null) };
  }

  /// Cancel a monthly licence at the end of its paid period, from the installation screen.
  async function cancel(token, installationId) {
    const rec = await ownedPurchase(token, installationId, { manage: true });
    if (!rec.licence_id) fail(404, "This installation has no licence yet.", "no_licence");
    if (rec.offer.term !== "monthly") fail(400, "A one-off licence does not renew; there is nothing to cancel.", "not_monthly");
    const licence = validLicence(await mp("POST", `/licences/${rec.licence_id}/cancel`));
    // The postbox shows the change at once; the reconciler would also bring it within a minute.
    await project(rec.intent_id, licence).catch((error) => console.error(`marketplace: projection after cancel ${error.code || error.status || "error"}`));
    return { installation_id: rec.installation_id, licence: publicLicence(licence) };
  }

  // ---- postbox (internal) ----------------------------------------------------------------------------

  const internalOk = (req) => {
    const m = /^Bearer\s+(\S+)$/i.exec(req.headers.authorization || "");
    return Boolean(config.marketplace.internalToken && m && sameSecret(m[1], config.marketplace.internalToken));
  };

  /// Mirror a listing's price to saas_be as an immutable offer (a price, term, currency or release
  /// line change is a new offer id, chosen by the postbox), provisioning a monthly one once.
  async function putOffer(offerId, body) {
    if (!ID.test(offerId || "")) fail(400, "Invalid offer id.", "invalid_request");
    strict(body, ["listing_id", "publisher_id", "release_line", "title", "term", "currency", "price_minor", "status"],
      ["listing_id", "publisher_id", "release_line", "title", "term", "currency", "price_minor"]);
    for (const key of ["listing_id", "publisher_id", "release_line"]) if (!ID.test(body[key] || "")) fail(400, `${key} is invalid.`, "invalid_request");
    if (typeof body.title !== "string" || !body.title.trim() || body.title.length > 200) fail(400, "title is invalid.", "invalid_request");
    if (!OFFER_TERMS.includes(body.term)) fail(400, "term must be one_off, monthly or per_job.", "invalid_request");
    if (!CURRENCIES.includes(body.currency)) fail(400, "currency must be TRY, USD or EUR.", "invalid_request");
    // Credits are kept and spent in USD: a per-job price is USD cents, and no card is charged for
    // it, so the iyzico POS currencies do not apply.
    const perJob = body.term === "per_job";
    if (perJob && body.currency !== "USD") fail(400, "A per-job price is in USD.", "invalid_request");
    if (!Number.isInteger(body.price_minor) || body.price_minor < 1 || body.price_minor > 100_000_000) fail(400, "price_minor is invalid.", "invalid_request");
    const status = body.status ?? "active";
    if (!["active", "retired"].includes(status)) fail(400, "status must be active or retired.", "invalid_request");
    if (status === "active" && !perJob && !config.openCurrencies.includes(body.currency)) fail(409, `Payments in ${body.currency} are not open yet.`, "currency_unavailable");
    let offer;
    try {
      offer = validOfferView(await mp("PUT", `/offers/${encodeURIComponent(offerId)}`, { ...body, title: body.title.trim(), status }), offerId);
    } catch (error) {
      if (error.status === 422) {
        // saas_be: a tax-inclusive price that cannot be invoiced exactly; it names the nearest
        // prices that can (in `details.nearest`, or `nearest` on older builds).
        const raw = error.upstream?.details?.nearest ?? error.upstream?.nearest;
        const nearest = Array.isArray(raw) ? raw.filter((n) => Number.isInteger(n) && n > 0).slice(0, 4) : [];
        fail(422, "This price cannot be invoiced exactly with tax. Choose one of the nearest prices.", "price_not_invoiceable", { nearest });
      }
      throw error;
    }
    if (offer.price_minor !== body.price_minor || offer.term !== body.term || offer.currency !== body.currency) {
      fail(502, "Billing returned a different offer.", "invalid_upstream");
    }
    if (offer.term === "monthly" && offer.status === "active" && !offer.sellable && config.marketplace.provisionMonthly) {
      offer = validOfferView(await mp("POST", `/offers/${encodeURIComponent(offerId)}/provision`), offerId);
    }
    await state.update((data) => { data.offers[offerId] = { ...body, status, sellable: offer.sellable, mirrored_at: Date.now() }; });
    return { offer };
  }

  async function reissue(rec) {
    const r = await mp("POST", `/authorizations/${rec.authorization_id}/reissue`);
    if (typeof r?.authorization_token !== "string" || !/^mpa_[A-Za-z0-9_-]{20,120}$/.test(r.authorization_token)) fail(502, "Billing returned an invalid authorization.", "invalid_upstream");
    await save(rec.intent_id, { authorization_token: r.authorization_token });
    return r.authorization_token;
  }

  /// The postbox admits an installation: the payment's single-use authorization is consumed for
  /// exactly the intent it was bound to, with the installation id as its reference (idempotent).
  async function consume(body) {
    strict(body, ["purchase_id", "intent", "consumer_reference"]);
    if (!INSTALLATION.test(body.purchase_id || "")) fail(404, "Purchase not found", "not_found");
    const rec = await byInstallation(body.purchase_id);
    if (!rec) fail(404, "Purchase not found", "not_found");
    const presented = canonicalIntent(body.intent);
    if (presented === null || presented !== canonicalIntent(rec.intent) || body.consumer_reference !== rec.installation_id) {
      fail(409, "This payment was made for a different installation.", "intent_mismatch");
    }
    if (!rec.authorization_id) fail(402, "This purchase has not been paid.", "payment_required");
    let token = rec.authorization_token || await reissue(rec);
    const attempt = () => mp("POST", "/authorizations/consume", { authorization_id: rec.authorization_id, token, intent: rec.intent, consumer_reference: rec.installation_id });
    let result;
    for (let tries = 0; ; tries++) {
      try { result = await attempt(); break; } catch (error) {
        if (![404, 409].includes(error.status) || tries > 0) {
          if (error.status === 409 && tries > 0) fail(402, "This payment cannot admit an installation.", "payment_required");
          throw error;
        }
        const auth = validAuthorization(await mp("GET", `/authorizations/${rec.authorization_id}?refresh=true`), rec.authorization_id);
        if (auth.status === "consumed") fail(409, "This payment already admitted another installation.", "authorization_used");
        if (auth.status === "pending_payment") fail(402, "The payment is not confirmed yet. Try again in a moment.", "payment_pending");
        if (auth.status === "failed") fail(402, "The payment did not go through.", "payment_failed");
        if (auth.status === "revoked") fail(402, "This payment was refunded or charged back.", "payment_revoked");
        if (auth.status === "expired" && !auth.authorized_at) fail(402, "This checkout expired before it was paid.", "payment_expired");
        if (auth.status === "authorized" && error.status === 409) fail(409, "This payment was made for a different installation.", "intent_mismatch");
        // Paid but the window lapsed (or the token was lost): rotate the token and try once more.
        token = await reissue(rec);
      }
    }
    if (!result?.licence) fail(502, "Billing returned no licence.", "invalid_upstream");
    const licence = validLicence(result.licence);
    if (licence.listing_id !== rec.listing_id || licence.owner_kind !== rec.owner.kind || licence.owner_id !== rec.owner.id || licence.term !== rec.offer.term) {
      fail(502, "Billing returned a licence for something else.", "invalid_upstream");
    }
    await save(rec.intent_id, { licence_id: licence.licence_id, status: "installed", consumed_at: rec.consumed_at || Date.now() });
    return { replay: result.replay === true, licence: publicLicence(licence) };
  }

  // ---- reconciler: licence lifecycle onto installations ------------------------------------------

  function projection(rec, l) {
    return { installation_id: rec.installation_id, listing_id: rec.listing_id, owner_kind: rec.owner.kind, owner_id: rec.owner.id,
      term: l.term, status: l.status, access: l.access, paid_through: l.paid_through ?? null,
      cancel_at_period_end: Boolean(l.cancel_at_period_end), deletion_due_at: l.deletion_due_at ?? null, revoked_at: l.revoked_at ?? null };
  }

  /// Push one licence's state to the postbox if it changed, or while a deletion boundary is due.
  async function project(intentId, licence) {
    const rec = await state.read((data) => data.purchases[intentId] && { intent_id: intentId, ...data.purchases[intentId] });
    if (!rec || rec.status !== "installed" || rec.licence_id !== licence.licence_id) return { pushed: false };
    const body = projection(rec, licence);
    const print = fingerprint(body);
    const due = body.deletion_due_at && Date.parse(body.deletion_due_at) <= Date.now() && !body.access;
    if (rec.projected?.print === print && !(due && !rec.closed)) return { pushed: false };
    const version = Math.max(Date.now(), (rec.projected?.version || 0) + 1);
    const result = await postbox.putLicence(licence.licence_id, { ...body, reconciliation_version: version });
    await save(intentId, { projected: { print, version }, ...(result?.runtime === "removed" ? { closed: true } : {}) });
    return { pushed: true, result };
  }

  async function reconcile() {
    const report = { pushed: 0, unchanged: 0, closed: 0, errors: 0 };
    if (!config.marketplace.enabled) return report;
    const open = await state.read((data) => Object.entries(data.purchases)
      .filter(([, p]) => p.authorization_id && !p.closed).map(([id, p]) => ({ intent_id: id, ...p })));
    for (const rec of open) {
      try {
        let licenceId = rec.licence_id;
        if (!licenceId) {
          const auth = validAuthorization(await mp("GET", `/authorizations/${rec.authorization_id}`), rec.authorization_id);
          if (auth.licence_id) { licenceId = auth.licence_id; await save(rec.intent_id, { licence_id: licenceId }); }
          // Never paid and long gone: stop looking at it.
          if (!auth.authorized_at && ["failed", "expired"].includes(auth.status) && Date.now() - rec.created_at > DAY) {
            await save(rec.intent_id, { closed: true }); report.closed++;
          }
        }
        if (!licenceId || rec.status !== "installed") { report.unchanged++; continue; }
        const licence = validLicence(await mp("GET", `/licences/${licenceId}`));
        const r = await project(rec.intent_id, licence);
        r.pushed ? report.pushed++ : report.unchanged++;
      } catch (error) {
        report.errors++;
        console.error(`marketplace reconcile: ${rec.installation_id} ${error.code || error.status || "error"}`);
      }
    }
    return report;
  }

  return { internalOk, checkout, checkoutStatus, licences, installation, cancel, putOffer, consume, reconcile };
}
