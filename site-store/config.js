// Pigeonpost handle store — deployment configuration.
//
// The customer-facing store (login, checkout, card capture, subscription management) is HOSTED BY
// MASAAS at the product app URL below. This branded landing validates the handle and hands the
// buyer off to that hosted store to sign in and subscribe. No payment keys, no API calls, no
// secrets live here — MASAAS owns all of it, exactly as the product was provisioned.
window.PIGEONPOST_STORE = {
  // The MASAAS-hosted store for this product. Empty = preview mode.
  masaasStoreUrl: "https://app-pigeonpost.masaas.org",

  // Where in the hosted store the customer subscribes / manages. Packages are shown here.
  billingPath: "/billing",

  // The public catalog — used only to read the live price so this page never drifts from MASAAS.
  catalog: {
    apiUrl: "https://api.masaas.org/v1",
    productSlug: "pigeonpost",
    packageSlug: "handle-yearly",
    planSlug: "handle-yearly-annual-usd", // stable across revisions; never use the price_plan UUID
  },

  // Display fallback if the live catalog read fails. MASAAS is authoritative.
  price: { amount: 5, currency: "USD", interval: "year" },

  registryUrl: "https://registry.pigeonpost.dev",

  // Handle grammar — mirrors the registry's flat-handle rules for instant feedback. The registry,
  // and MASAAS at checkout, remain authoritative.
  handle: { min: 3, max: 32, pattern: "^[a-z0-9]+$" },
};
