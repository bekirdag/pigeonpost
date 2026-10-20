// saas_be (MASAAS member backend) iyzico abonelik client, with strict response validation.
//
// Member calls carry the customer's own JWT and `x-product-slug`; saas_be enforces tenant and
// buyer ownership. Every mutation sends an explicit Idempotency-Key derived from a client intent,
// so a lost response is replayed rather than repeated. Nothing returned is trusted until it has
// been checked here: ids, statuses, modes, amounts and dates.

import { config } from "./config.js";
import { isUuid } from "./catalog.js";

export const fail = (status, message, code) => { throw Object.assign(new Error(message), { status, code }); };

// Upstream text is shown to the member, so it is flattened and bounded; tokens never appear in it.
function clean(message) {
  return String(message || "").replace(/[\u0000-\u001f\u007f]+/g, " ").replace(/eyJ[\w-]+\.[\w-]+\.[\w-]*/g, "[token]").slice(0, 300);
}

async function request(url, { method = "GET", headers = {}, body, timeoutMs = config.requestTimeoutMs } = {}) {
  let res;
  try {
    res = await fetch(url, {
      method, headers, body: body === undefined ? undefined : JSON.stringify(body), redirect: "error",
      signal: AbortSignal.timeout(timeoutMs),
    });
  } catch (error) {
    // A timeout on a mutation is ambiguous; the caller replays the same key, never a new one.
    fail(504, error?.name === "TimeoutError" ? "Billing did not answer in time. Try again; nothing is charged twice." : "Billing is unreachable. Try again shortly.", "upstream_unavailable");
  }
  const text = await res.text().catch(() => "");
  if (text.length > 1_000_000) fail(502, "Billing returned an oversized response.", "invalid_upstream");
  let payload = null;
  try { payload = text ? JSON.parse(text) : null; } catch { payload = null; }
  if (!res.ok) {
    const raw = payload?.message ?? payload?.error?.message ?? payload?.error ?? payload?.detail;
    const message = Array.isArray(raw) ? raw.join("; ") : typeof raw === "object" ? raw?.message : raw;
    const status = res.status === 401 ? 401 : res.status >= 500 ? 502 : [400, 403, 404, 409, 422, 429].includes(res.status) ? res.status : 502;
    fail(status, clean(message) || `Billing returned ${res.status}`, "upstream_rejected");
  }
  if (payload === null || typeof payload !== "object") fail(502, "Billing returned an invalid response.", "invalid_upstream");
  return payload;
}

function member(token, method, path, body, { key, timeoutMs } = {}) {
  const headers = {
    accept: "application/json",
    authorization: `Bearer ${token}`,
    "user-agent": "pigeonpost billing bridge",
    "x-product-slug": config.masaasProductSlug,
    "x-tenant-id": config.tenantId,
  };
  if (body !== undefined) {
    headers["content-type"] = "application/json";
    if (!/^[a-zA-Z0-9_.:-]{8,128}$/.test(key || "")) fail(500, "Missing idempotency key", "bridge_error");
    headers["idempotency-key"] = key;
  }
  return request(`${config.masaasSaasApiBase}${path}`, { method, headers, body, timeoutMs });
}

// ---- validators -------------------------------------------------------------------------------

const iso = (value) => (value === null || value === undefined ? null : Number.isFinite(Date.parse(value)) ? value : undefined);
const AMOUNT = /^\d{1,8}\.\d{2}$/;
const REF = /^[A-Za-z0-9_-]{1,128}$/;

export function validOffer(offer, { currency, interval }) {
  if (!offer || typeof offer !== "object") fail(502, "Billing returned an invalid offer.", "invalid_upstream");
  if (offer.available !== true) fail(503, clean(offer.reason) || "This plan is not available right now.", "offer_unavailable");
  if (!isUuid(offer.plan_id) || !isUuid(offer.package_id) || offer.currency !== currency
    || !AMOUNT.test(String(offer.amount)) || !Number.isInteger(offer.pricing_year)
    || offer.currencyPolicy !== (currency === "TRY" ? "CALENDAR_YEAR_FIXED" : "FIXED")
    || offer.trial_period_days !== 0 || (offer.interval && offer.interval !== interval)
    || (currency === "TRY" && !(Date.parse(offer.effective_until) > Date.now()))) {
    fail(503, "Plan pricing and terms are not ready.", "offer_invalid");
  }
  return offer;
}

export function validCheckout(result) {
  if (!result || !isUuid(result.subscription_id) || !isUuid(result.binding_id) || result.provider !== "iyzico"
    || result.status !== "checkout_pending" || typeof result.checkout_form_content !== "string"
    || !result.checkout_form_content.trim() || result.checkout_form_content.length > 200_000
    || Number(result.trial_period_days) !== 0 || iso(result.expires_at) === undefined) {
    fail(502, "Billing returned an invalid checkout.", "invalid_upstream");
  }
  return result;
}

const LOCAL = ["active", "trialing", "past_due", "canceled", "paused", "incomplete", "unpaid", "expired"];
const PROVIDER = ["INITIALIZING", "CHECKOUT_PENDING", "UNCERTAIN", "FAILED", "ACTIVE", "PENDING", "UNPAID", "UPGRADED", "CANCELED", "EXPIRED"];

/// A member subscription read (`GET …/subscriptions/{id}?refresh=true`).
export function validSubscription(sub, id) {
  if (!sub || sub.subscription_id !== id || sub.provider !== "iyzico" || sub.mode !== config.iyzicoMode
    || !PROVIDER.includes(sub.provider_status) || !LOCAL.includes(sub.status)
    || (sub.pricing_plan_reference !== null && !REF.test(String(sub.pricing_plan_reference)))
    || [sub.current_period_start, sub.current_period_end].some((v) => iso(v) === undefined)
    || (sub.orders !== undefined && !Array.isArray(sub.orders))) {
    fail(502, "Billing returned an invalid subscription.", "invalid_upstream");
  }
  for (const order of sub.orders || []) {
    if (!REF.test(String(order.order_reference_code)) || !["SUCCESS", "FAILED"].includes(order.status)
      || iso(order.period_end) === undefined) fail(502, "Billing returned an invalid order.", "invalid_upstream");
  }
  return sub;
}

export function validSummary(item) {
  return item && isUuid(item.subscription_id) && item.provider === "iyzico" && PROVIDER.includes(item.provider_status)
    && (item.pricing_plan_reference === null || REF.test(String(item.pricing_plan_reference)));
}

// ---- member operations ------------------------------------------------------------------------

export const offer = (token, packageSlug, currency) =>
  member(token, "GET", `/v1/integrations/iyzico/regional-pricing/offer?package_slug=${encodeURIComponent(packageSlug)}&currency=${currency}`);

export const startSubscription = (token, body, key) =>
  member(token, "POST", "/v1/integrations/iyzico/subscriptions", body, { key, timeoutMs: config.checkoutTimeoutMs });

export const getSubscription = (token, id) =>
  member(token, "GET", `/v1/integrations/iyzico/subscriptions/${id}?refresh=true`, undefined, { timeoutMs: config.checkoutTimeoutMs });

export async function listSubscriptions(token) {
  const page = await member(token, "GET", "/v1/integrations/iyzico/subscriptions?page=1&count=100");
  if (!Array.isArray(page.items)) fail(502, "Billing returned an invalid subscription list.", "invalid_upstream");
  return page.items.filter(validSummary);
}

export const operate = (token, id, operation, body, key) =>
  member(token, "POST", `/v1/integrations/iyzico/subscriptions/${id}/${operation}`, body, { key, timeoutMs: config.checkoutTimeoutMs });

export const createBillingProfile = (token, body, key) => member(token, "POST", "/v1/billing/profiles", body, { key });
export const updateBillingProfile = (token, id, body, key) => member(token, "PATCH", `/v1/billing/profiles/${encodeURIComponent(id)}`, body, { key });
export const listBillingProfiles = (token) => member(token, "GET", "/v1/billing/profiles?limit=20");
export const listInvoices = (token) => member(token, "GET", "/v1/billing/invoices?limit=50");
export async function invoiceDownload(token, id) {
  const result = await member(token, "GET", `/v1/billing/invoices/${id}/download`);
  let url;
  try { url = new URL(result.url); } catch { url = null; }
  if (!url || url.protocol !== "https:") fail(502, "Billing returned an invalid invoice link.", "invalid_upstream");
  return { url: url.href };
}

// ---- internal (reconciler) --------------------------------------------------------------------

/// The tenant's access-granting subscriptions (saas_be's adapter sync route). For iyzico this lists
/// only subscriptions with a provider-confirmed, current paid period.
export async function accessList() {
  if (!config.saasInternalToken) fail(503, "Reconciliation is not configured.", "not_configured");
  const url = `${config.masaasSaasApiBase}/v1/internal/tenants/${encodeURIComponent(config.tenantId)}/subscription-packages?product_slug=${encodeURIComponent(config.masaasProductSlug)}&limit=1000`;
  const payload = await request(url, { headers: { accept: "application/json", authorization: `Bearer ${config.saasInternalToken}` } });
  if (!Array.isArray(payload.data)) fail(502, "Billing returned an invalid access list.", "invalid_upstream");
  return payload.data.filter((row) => row && isUuid(row.subscription_id) && row.tenant_id === config.tenantId
    && typeof row.external_reference === "string" && iso(row.current_period_end) && iso(row.current_period_start) !== undefined);
}
