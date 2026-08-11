// Pigeonpost handle store — deployment configuration.
//
// Every value here is filled in once the MASAAS tenant exists and Stripe test keys are issued.
// Until then the store runs in "preview" mode: handle validation and the whole UI work, but the
// checkout button explains what is not yet wired instead of calling a dead endpoint.
//
// Nothing secret belongs in this file — it ships to the browser. The Stripe *publishable* key is
// public by design; the secret key lives only in the adapter service and MASAAS.
window.PIGEONPOST_STORE = {
  // The Pigeonpost billing adapter (the service that fronts saas_be's internal checkout API).
  // Empty string = preview mode.
  adapterBaseUrl: "",

  // Stripe publishable key, test mode (pk_test_...). Public. Empty = preview mode.
  stripePublishableKey: "",

  // sealunit OIDC for the product realm — where "Sign in" sends the browser.
  oidc: {
    issuer: "",            // e.g. https://sso.sealunit.com/realms/pigeonpost
    clientId: "",          // the store's public OIDC client id
    redirectPath: "/account",
  },

  // The registry these handles are sold in. Read-only here; used to show a resolve link.
  registryUrl: "https://registry.pigeonpost.dev",

  // Commercial terms shown to the buyer. The authority is the MASAAS price plan; these are display.
  price: { amount: 5, currency: "USD", interval: "year" },

  // The MASAAS product/package a purchase subscribes to.
  product: { slug: "pigeonpost", packageId: "" },

  // Handle grammar — mirrors the registry's flat-handle rules so the UI can validate before asking
  // the server. The server remains authoritative.
  handle: { min: 3, max: 32, pattern: "^[a-z0-9]+$" },
};
