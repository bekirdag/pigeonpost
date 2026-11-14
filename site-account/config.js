// account.pigeonpost.dev — configuration. No secret lives here.
//
// The member API (the billing bridge) is reverse-proxied at /api on this origin; the postbox is its
// own origin. Sign-in is the pigeonpost-prod realm's public `pigeonpost-web` client (PKCE S256),
// returning to this page itself.
window.PP_CONFIG = {
  postbox: "https://postbox.pigeonpost.dev",
  apiBase: "/api",
  home: "/",
  oidc: {
    issuer: "https://auth.pigeonpost.dev/realms/pigeonpost-prod",
    clientId: "pigeonpost-web",
    scope: "openid email profile",
    redirectPath: "/",
  },
};

// Pigeonpost Cloud on the web: iyzico subscriptions through saas_be, yearly, no trial. These are
// display copies of the bridge's catalog (store-adapter/catalog/pigeonpost-catalog.json); the
// bridge refuses any edition that differs from it. Same numbers as site/account-config.js.
window.PP_CLOUD = (() => {
  const usd = { 1: "8.00", 2: "16.00", 3: "24.00", 4: "32.00", 5: "40.00", 6: "48.00", 7: "56.00", 8: "64.00", 9: "72.00", 10: "80.00" };
  const eur = { 1: "7.20", 2: "14.40", 3: "21.60", 4: "28.80", 5: "36.00", 6: "43.20", 7: "50.40", 8: "57.60", 9: "64.80", 10: "72.00" };
  const tryByYear = {
    2026: { 1: "409.00", 2: "817.00", 3: "1226.00", 4: "1634.00", 5: "2043.00", 6: "2451.00", 7: "2860.00", 8: "3268.00", 9: "3677.00", 10: "4085.00" },
    2027: { 1: "449.00", 2: "899.00", 3: "1339.00", 4: "1789.00", 5: "2239.00", 6: "2679.00", 7: "3129.00", 8: "3569.00", 9: "4019.00", 10: "4469.00" },
  };
  let now = new Date().getFullYear();
  try { now = Number(new Intl.DateTimeFormat("en-US", { year: "numeric", timeZone: "Europe/Istanbul" }).format(new Date())); } catch { /* keep local year */ }
  const years = Object.keys(tryByYear).map(Number).sort((a, b) => a - b);
  const pricingYear = years.filter((y) => y <= now).pop() || years[0];
  const plans = {};
  for (const n of Object.keys(usd)) plans[n] = { USD: usd[n], EUR: eur[n], TRY: tryByYear[pricingYear][n] };
  return { pricingYear, currencies: ["USD", "EUR", "TRY"], perNameUSD: "8.00", plans, checkoutPath: "/billing/checkout" };
})();
