// Pigeonpost account — configuration.
//
// Everything is on pigeonpost.dev. The member API is reverse-proxied at /api (the billing bridge).
// Sign-in and registration are hosted on auth.pigeonpost.dev for the pigeonpost-prod realm, themed
// to match this site. No secret lives here.
window.PIGEONPOST_ACCOUNT = {
  // Member API (the billing bridge), same origin.
  apiBase: "/api",

  oidc: {
    issuer: "https://auth.pigeonpost.dev/realms/pigeonpost-prod",
    clientId: "pigeonpost-web",   // live in the pigeonpost-prod realm (public, PKCE S256)
    redirectPath: "/account",
    scope: "openid email profile",
  },

  // Pigeonpost Cloud on the web: iyzico subscriptions through saas_be, yearly, no trial.
  // USD 8 per name per year (the App Store price). TRY prices are fixed for the calendar year
  // (saas_be regional editions); the bridge refuses any edition that differs from its catalog
  // (store-adapter/catalog/pigeonpost-catalog.json), so these numbers are display copies of it.
  cloud: {
    pricingYear: 2026,
    currencies: ["USD", "TRY"],
    perNameUSD: "8.00",
    plans: {
      1: { USD: "8.00", TRY: "409.00" },
      2: { USD: "16.00", TRY: "817.00" },
      3: { USD: "24.00", TRY: "1226.00" },
      4: { USD: "32.00", TRY: "1634.00" },
      5: { USD: "40.00", TRY: "2043.00" },
      6: { USD: "48.00", TRY: "2451.00" },
      7: { USD: "56.00", TRY: "2860.00" },
      8: { USD: "64.00", TRY: "3268.00" },
      9: { USD: "72.00", TRY: "3677.00" },
      10: { USD: "80.00", TRY: "4085.00" },
    },
    checkoutPath: "/billing/checkout",
  },

  handle: { min: 3, max: 32 },
};
