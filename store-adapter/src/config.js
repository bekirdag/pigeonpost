// Pigeonpost billing bridge — configuration, all from the environment.
//
// No payment-gateway secrets appear here: saas_be (MASAAS) holds the iyzico merchant keys and
// iyzico hosts the card form. Member billing operations travel on the customer's own token. Two
// service credentials exist: the postbox internal-API token (entitlement writes) and saas_be's
// internal adapter token (the subscription-access list the reconciler reads).

const trimSlash = (s) => String(s || "").replace(/\/+$/, "");

export const config = {
  port: Number(process.env.PORT || 8787),

  // Allowed browser origins. The first is pigeonpost.dev, which is also the only origin an
  // individual checkout returns to. Everything is reverse-proxied at https://pigeonpost.dev/api.
  allowedOrigins: (process.env.STORE_ALLOWED_ORIGINS || "https://pigeonpost.dev")
    .split(",").map((s) => s.trim()).filter(Boolean),

  // Where an enterprise checkout returns. It must be one of saas_be's configured return origins.
  enterpriseOrigin: trimSlash(process.env.ENTERPRISE_ORIGIN || "https://enterprise.pigeonpost.dev"),

  // MASAAS control-plane API (legacy public catalog).
  masaasApiBaseUrl: trimSlash(process.env.MASAAS_API_URL || "https://api.masaas.org/v1"),

  // MASAAS member SaaS backend (saas_be): billing profiles, iyzico subscriptions, invoices.
  masaasProductAppUrl: trimSlash(process.env.MASAAS_PRODUCT_APP_URL || "https://app-pigeonpost.masaas.org"),
  get masaasSaasApiBase() {
    return trimSlash(process.env.MASAAS_SAAS_API_URL || `${this.masaasProductAppUrl}/saas-api`);
  },

  masaasProductSlug: process.env.MASAAS_PRODUCT_SLUG || "pigeonpost",
  // The saas_be tenant. Used for the internal subscription-access list.
  tenantId: process.env.MASAAS_TENANT_ID || "pigeonpost",

  // saas_be's internal adapter token (`API_KEY_INTROSPECTION_TOKEN` there). Read-only use: the
  // reconciler lists this tenant's access-granting subscriptions so a closed tab still activates.
  saasInternalToken: process.env.SAAS_INTERNAL_TOKEN || "",

  // Which iyzico environment saas_be's tenant runs in; every binding must report the same mode.
  iyzicoMode: process.env.IYZICO_MODE || "live",

  // Optional runtime/service token for legacy entitlement reads. Not used by the iyzico flow.
  masaasRuntimeToken: process.env.MASAAS_RUNTIME_TOKEN || "",

  oidc: {
    issuer: trimSlash(process.env.OIDC_ISSUER || "https://sso.sealunit.com/realms/pigeonpost"),
    clientId: process.env.OIDC_CLIENT_ID || "pigeonpost-store",
    clientSecret: process.env.OIDC_CLIENT_SECRET || "", // empty = public client (PKCE)
  },

  registryUrl: trimSlash(process.env.PIGEONPOST_REGISTRY_URL || "https://registry.pigeonpost.dev"),

  // The postbox's public API: handle availability, the member's account id, name claims.
  postboxUrl: trimSlash(process.env.PIGEONPOST_POSTBOX_URL || "https://postbox.pigeonpost.dev"),

  // The postbox's internal API (private link only) and this service's token for it
  // (`POSTBOX_SVC_TOKEN_BILLING` on the postbox side).
  postboxInternalUrl: trimSlash(process.env.POSTBOX_INTERNAL_URL || ""),
  postboxServiceToken: process.env.POSTBOX_SERVICE_TOKEN || "",

  // Durable bridge state: checkout intents, rate-limit history, last pushed entitlements.
  stateDir: process.env.STORE_STATE_DIR || process.env.STATE_DIRECTORY || "/var/lib/pigeonpost-store-adapter",

  // Currencies new checkouts may use. iyzico refuses a plan currency the merchant has no active
  // POS for ("Active pos is not found for currency"), so a currency opens only once it does.
  openCurrencies: (process.env.OPEN_CURRENCIES || "USD,TRY").split(",").map((s) => s.trim().toUpperCase()).filter(Boolean),

  // At most this many new checkouts per account per hour (replays of one intent do not count).
  checkoutsPerHour: Number(process.env.CHECKOUTS_PER_HOUR || 5),

  // Missed-return reconciliation interval. 0 disables the timer (tests drive it explicitly).
  reconcileIntervalMs: Number(process.env.RECONCILE_INTERVAL_MS ?? 60000),

  requestTimeoutMs: Number(process.env.MASAAS_TIMEOUT_MS || 15000),
  // Starting a hosted checkout can make several provider requests inside saas_be.
  checkoutTimeoutMs: Number(process.env.CHECKOUT_TIMEOUT_MS || 60000),
};

const httpUrl = (value) => {
  try {
    return ["http:", "https:"].includes(new URL(value).protocol);
  } catch {
    return false;
  }
};
const httpsOrigin = (value) => {
  try {
    const url = new URL(value);
    return url.protocol === "https:" && url.origin === value;
  } catch {
    return false;
  }
};

// Configuration completeness only; this does not probe upstream availability or validate keys.
export function configured() {
  return [config.masaasProductSlug, config.tenantId, config.oidc.clientId, config.postboxServiceToken,
    config.saasInternalToken]
    .every((value) => typeof value === "string" && value.trim().length > 0)
    && config.allowedOrigins.length > 0
    && ["live", "test"].includes(config.iyzicoMode)
    && [config.masaasApiBaseUrl, config.masaasSaasApiBase, config.oidc.issuer, config.postboxUrl,
      config.postboxInternalUrl].every(httpUrl)
    && [...config.allowedOrigins, config.enterpriseOrigin].every(httpsOrigin);
}
