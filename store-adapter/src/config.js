// Pigeonpost store adapter — configuration, all from the environment.
//
// Mirrors the theneuralledger adapter's env contract so an operator who has wired one can wire this
// the same way. No payment-gateway secrets appear here: MASAAS holds the Stripe keys and hosts the
// card capture. The only privileged value is the MASAAS runtime token, used for catalog and
// entitlement reads; member billing operations travel on the customer's own token.

const trimSlash = (s) => String(s || "").replace(/\/+$/, "");

export const config = {
  port: Number(process.env.PORT || 8787),

  // Allowed browser origin. Everything is on pigeonpost.dev; the adapter is reverse-proxied at
  // https://pigeonpost.dev/api, so same-origin requests carry no Origin header and CORS is moot —
  // this list only matters if the adapter is ever addressed cross-origin.
  allowedOrigins: (process.env.STORE_ALLOWED_ORIGINS || "https://pigeonpost.dev")
    .split(",").map((s) => s.trim()).filter(Boolean),

  // MASAAS control-plane API (catalog, entitlement snapshot).
  masaasApiBaseUrl: trimSlash(process.env.MASAAS_API_URL || "https://api.masaas.org/v1"),

  // MASAAS member SaaS backend (subscriptions, billing profile, hosted payment session).
  // Defaults the same way theneuralledger's does: <product app url>/saas-api.
  masaasProductAppUrl: trimSlash(process.env.MASAAS_PRODUCT_APP_URL || "https://app-pigeonpost.masaas.org"),
  get masaasSaasApiBase() {
    return trimSlash(process.env.MASAAS_SAAS_API_URL || `${this.masaasProductAppUrl}/saas-api`);
  },

  masaasProductSlug: process.env.MASAAS_PRODUCT_SLUG || "pigeonpost",

  // The price plan a purchase subscribes to. Stable slug — never the regenerated price_plan UUID.
  planSlug: process.env.MASAAS_PLAN_SLUG || "handle-yearly-annual-usd",

  // Runtime/service token — catalog + entitlement reads only. Never sent to the browser.
  masaasRuntimeToken: process.env.MASAAS_RUNTIME_TOKEN || "",

  // sealunit OIDC for the product realm — the adapter exchanges the auth code here.
  oidc: {
    issuer: trimSlash(process.env.OIDC_ISSUER || "https://sso.sealunit.com/realms/pigeonpost"),
    clientId: process.env.OIDC_CLIENT_ID || "pigeonpost-store",
    clientSecret: process.env.OIDC_CLIENT_SECRET || "", // empty = public client (PKCE)
  },

  registryUrl: trimSlash(process.env.PIGEONPOST_REGISTRY_URL || "https://registry.pigeonpost.dev"),

  requestTimeoutMs: Number(process.env.MASAAS_TIMEOUT_MS || 15000),
};

export function configured() {
  return Boolean(config.masaasRuntimeToken && config.masaasProductSlug);
}
