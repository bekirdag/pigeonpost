// MASAAS client — the proxy pattern taken from the theneuralledger adapter.
//
// Two authorities, exactly as in TNL:
//   - the runtime token authenticates catalog/entitlement reads (server-held);
//   - the customer's own token authenticates member billing operations, forwarded with the
//     `x-product-slug` header. The adapter never holds a payment-gateway key — MASAAS does, and it
//     hosts the card capture.

import { randomUUID } from "node:crypto";
import { config } from "./config.js";

function idempotencyKey(prefix) {
  return `${prefix}_${randomUUID()}`;
}

async function doFetch(url, init) {
  const controller = AbortSignal.timeout(config.requestTimeoutMs);
  const res = await fetch(url, { ...init, signal: controller });
  const text = await res.text().catch(() => "");
  let payload = null;
  try { payload = text ? JSON.parse(text) : null; } catch { payload = { raw: text }; }
  if (!res.ok) {
    const message =
      (payload && (payload.message || payload.error || payload.detail)) ||
      `MASAAS returned ${res.status}`;
    throw Object.assign(new Error(message), { status: res.status, payload });
  }
  return payload;
}

// ---- runtime-token reads (public-ish catalog, entitlement snapshot) ---------------------------

export async function publicPackages() {
  const url = new URL(`${config.masaasApiBaseUrl}/catalog/public/packages`);
  url.searchParams.set("product_slug", config.masaasProductSlug);
  return doFetch(url, { headers: { accept: "application/json" } });
}

export async function entitlementSnapshot(tenantId) {
  const url = new URL(`${config.masaasApiBaseUrl}/catalog/runtime/snapshot`);
  url.searchParams.set("product_slug", config.masaasProductSlug);
  url.searchParams.set("tenant_id", tenantId);
  return doFetch(url, {
    headers: {
      accept: "application/json",
      authorization: `Bearer ${config.masaasRuntimeToken}`,
    },
  });
}

// ---- member-token proxy (subscriptions, billing profile, hosted payment) ----------------------

export async function memberBackend(memberToken, method, path, body) {
  const headers = {
    accept: "application/json",
    authorization: `Bearer ${memberToken}`,
    "user-agent": "pigeonpost store adapter",
    "x-product-slug": config.masaasProductSlug,
  };
  const hasBody = body !== undefined && method !== "GET";
  if (hasBody) {
    headers["content-type"] = "application/json";
    headers["idempotency-key"] = idempotencyKey(`pp_${method.toLowerCase()}`);
  }
  const target = /^https?:\/\//i.test(path) ? path : `${config.masaasSaasApiBase}${path.startsWith("/") ? path : `/${path}`}`;
  return doFetch(target, { method, headers, body: hasBody ? JSON.stringify(body) : undefined });
}

// Subscriptions
export const listSubscriptions = (t) => memberBackend(t, "GET", "/v1/subscriptions?limit=20");
export const cancelSubscription = (t, id, reason) =>
  memberBackend(t, "POST", `/v1/subscriptions/${encodeURIComponent(id)}/cancel`, { reason: reason || "customer_requested" });

// The buy: subscribe to a price plan by its stable slug (never the regenerated price_plan UUID).
// MASAAS creates the subscription; the hosted payment session captures the card.
export const subscribeToPlan = (t, planSlug, metadata) =>
  memberBackend(t, "POST", "/v1/subscriptions", { plan_slug: planSlug, ...(metadata ? { metadata } : {}) });

// Billing profile (individual or entity + tax fields)
export const listBillingProfiles = (t) => memberBackend(t, "GET", "/v1/billing/profiles?limit=20");
export const createBillingProfile = (t, profile) => memberBackend(t, "POST", "/v1/billing/profiles", profile);

// Payment methods — MASAAS hosts the card capture; setup-session returns the hosted URL.
export const listPaymentMethods = (t) => memberBackend(t, "GET", "/v1/billing/payment-methods?limit=20");
export const paymentGateways = (t) => memberBackend(t, "GET", "/v1/billing/payment-methods/gateways");
export const paymentSetupSession = (t, returnUrl, cancelUrl, billingProfileId) =>
  memberBackend(t, "POST", "/v1/billing/payment-methods/setup-session", {
    return_url: returnUrl,
    cancel_url: cancelUrl,
    ...(billingProfileId ? { billing_profile_id: billingProfileId } : {}),
  });
export const completePaymentSession = (t, body) =>
  memberBackend(t, "POST", "/v1/billing/payment-methods/complete-session", body);

// Invoices
export const listInvoices = (t) => memberBackend(t, "GET", "/v1/billing/invoices?limit=20");

// ---- OIDC code exchange (adapter-side; browser never sees a client secret) ---------------------

export async function exchangeOidcCode(code, redirectUri, codeVerifier) {
  const url = `${config.oidc.issuer}/protocol/openid-connect/token`;
  const form = new URLSearchParams({
    grant_type: "authorization_code",
    code,
    redirect_uri: redirectUri,
    client_id: config.oidc.clientId,
  });
  if (config.oidc.clientSecret) form.set("client_secret", config.oidc.clientSecret);
  if (codeVerifier) form.set("code_verifier", codeVerifier);
  return doFetch(url, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded", accept: "application/json" },
    body: form.toString(),
  });
}

// Silently renew the access token from a stored refresh token ("remember me"). The refresh token
// itself never reaches the browser as a bearer credential — it round-trips only to Keycloak here.
export async function refreshOidcToken(refreshToken) {
  const url = `${config.oidc.issuer}/protocol/openid-connect/token`;
  const form = new URLSearchParams({
    grant_type: "refresh_token",
    refresh_token: refreshToken,
    client_id: config.oidc.clientId,
  });
  if (config.oidc.clientSecret) form.set("client_secret", config.oidc.clientSecret);
  return doFetch(url, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded", accept: "application/json" },
    body: form.toString(),
  });
}

// ---- registry read (handle availability) ------------------------------------------------------

export async function registryResolves(name) {
  // A flat handle occupies the bare path segment. Resolving 200 = taken, 404 = free.
  const url = `${config.registryUrl}/v1/resolve/handle/${encodeURIComponent(name)}`;
  try {
    const res = await fetch(url, { signal: AbortSignal.timeout(6000) });
    return res.status === 200;
  } catch {
    return null; // unknown
  }
}
