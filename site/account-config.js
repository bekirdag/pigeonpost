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
  // USD 8 per name per year (the App Store price). TRY prices are fixed for each calendar year
  // (saas_be regional editions, switching at midnight Istanbul on January 1); the bridge refuses
  // any edition that differs from its catalog (store-adapter/catalog/pigeonpost-catalog.json), so
  // these numbers are display copies of it.
  cloud: (() => {
    const usd = { 1: "8.00", 2: "16.00", 3: "24.00", 4: "32.00", 5: "40.00",
      6: "48.00", 7: "56.00", 8: "64.00", 9: "72.00", 10: "80.00" };
    const tryByYear = {
      2026: { 1: "409.00", 2: "817.00", 3: "1226.00", 4: "1634.00", 5: "2043.00",
        6: "2451.00", 7: "2860.00", 8: "3268.00", 9: "3677.00", 10: "4085.00" },
      2027: { 1: "449.00", 2: "899.00", 3: "1339.00", 4: "1789.00", 5: "2239.00",
        6: "2679.00", 7: "3129.00", 8: "3569.00", 9: "4019.00", 10: "4469.00" },
    };
    let now = new Date().getFullYear();
    try { now = Number(new Intl.DateTimeFormat("en-US", { year: "numeric", timeZone: "Europe/Istanbul" }).format(new Date())); } catch { /* keep local year */ }
    const years = Object.keys(tryByYear).map(Number).sort((a, b) => a - b);
    const pricingYear = years.filter((y) => y <= now).pop() || years[0];
    const plans = {};
    for (const n of Object.keys(usd)) plans[n] = { USD: usd[n], TRY: tryByYear[pricingYear][n] };
    return {
      pricingYear,
      tryByYear,
      currencies: ["USD", "TRY"],
      perNameUSD: "8.00",
      plans,
      checkoutPath: "/billing/checkout",
    };
  })(),

  handle: { min: 3, max: 32 },
};
