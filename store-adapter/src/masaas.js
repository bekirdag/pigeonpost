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

export const listSubscriptions = (t) => memberBackend(t, "GET", "/v1/subscriptions?limit=20");
export const billingProfiles = (t) => memberBackend(t, "GET", "/v1/billing/profiles?limit=20");
export const createBillingProfile = (t, profile) => memberBackend(t, "POST", "/v1/billing/profiles", profile);
export const cancelSubscription = (t, id, reason) =>
  memberBackend(t, "POST", `/v1/subscriptions/${encodeURIComponent(id)}/cancel`, { reason: reason || "customer_requested" });

// The buy: MASAAS creates the subscription and returns a hosted payment session URL.
export const selectPackage = (t, packageId, metadata) =>
  memberBackend(t, "POST", "/v1/subscriptions", { package_id: packageId, metadata });

export const paymentSetupSession = (t, returnUrl, cancelUrl, billingProfileId) =>
  memberBackend(t, "POST", "/v1/billing/payment-methods/setup-session", {
    return_url: returnUrl,
    cancel_url: cancelUrl,
    ...(billingProfileId ? { billing_profile_id: billingProfileId } : {}),
  });

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
