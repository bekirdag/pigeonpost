// Pigeonpost for teams (enterprise.pigeonpost.dev) — configuration. No secret lives here.
//
// The billing bridge is same-origin at /api (nginx proxies it), so the sign-in code exchange and the
// checkout calls never leave this origin. Organization data comes from the postbox, which validates
// the member token itself.
window.PPE_CONFIG = {
  postbox: "https://postbox.pigeonpost.dev",
  apiBase: "/api",
  inbox: "https://inbox.pigeonpost.dev",
  site: "https://pigeonpost.dev",
  oidc: {
    issuer: "https://auth.pigeonpost.dev/realms/pigeonpost-prod",
    clientId: "pigeonpost-web", // public client, PKCE S256
    redirectPath: "/auth/callback",
    // The address claim is what an invitation is matched against, so this site asks for it.
    scope: "openid email profile",
  },
  seatStops: [5, 10, 15, 20, 30, 50, 100, 200, 500],
  // A display copy of the enterprise editions in store-adapter/catalog/pigeonpost-catalog.json
  // (a test keeps it equal). TRY prices are fixed per calendar year and switch at midnight
  // Istanbul on January 1. The live list from /api/v1/catalog wins when it loads; checkout prices
  // are always decided by the bridge, never by this page.
  ...(() => {
    const tryByYear = {
      2026: {
        monthly: { 5: "1532.00", 10: "3064.00", 15: "4596.00", 20: "6127.00", 30: "9191.00", 50: "13786.00", 100: "24508.00", 200: "45953.00", 500: "114881.00" },
        yearly: { 5: "15318.00", 10: "30635.00", 15: "45953.00", 20: "61270.00", 30: "91905.00", 50: "137857.00", 100: "245079.00", 200: "459524.00", 500: "1148808.00" },
      },
      2027: {
        monthly: { 5: "1679.00", 10: "3349.00", 15: "5019.00", 20: "6699.00", 30: "10090.00", 50: "15090.00", 100: "26790.00", 200: "50190.00", 500: "125900.00" },
        yearly: { 5: "16790.00", 10: "33490.00", 15: "50190.00", 20: "66990.00", 30: "100900.00", 50: "150900.00", 100: "267900.00", 200: "501900.00", 500: "1259000.00" },
      },
    };
    let now = new Date().getFullYear();
    try { now = Number(new Intl.DateTimeFormat("en-US", { year: "numeric", timeZone: "Europe/Istanbul" }).format(new Date())); } catch { /* keep local year */ }
    const years = Object.keys(tryByYear).map(Number).sort((a, b) => a - b);
    const pricingYear = years.filter((y) => y <= now).pop() || years[0];
    return {
      pricingYear,
      tryByYear,
      prices: {
        USD: {
          monthly: { 5: "30.00", 10: "60.00", 15: "90.00", 20: "120.00", 30: "180.00", 50: "270.00", 100: "480.00", 200: "900.00", 500: "2250.00" },
          yearly: { 5: "300.00", 10: "600.00", 15: "900.00", 20: "1200.00", 30: "1800.00", 50: "2700.00", 100: "4800.00", 200: "9000.00", 500: "22500.00" },
        },
        TRY: tryByYear[pricingYear],
      },
    };
  })(),
};
