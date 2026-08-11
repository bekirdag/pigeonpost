// Pigeonpost account — configuration.
//
// Everything is on pigeonpost.dev. The member API is reverse-proxied at /api (the store adapter).
// Sign-in and registration are hosted by sealunit for the pigeonpost-prod realm, themed to match
// this site. No secret lives here.
window.PIGEONPOST_ACCOUNT = {
  // Member API (the adapter), same origin.
  apiBase: "/api",

  // sealunit OIDC for the product realm. clientId is filled in once the realm's web client exists;
  // empty clientId = sign-in shows "not yet configured" instead of a broken redirect.
  oidc: {
    issuer: "https://sso.sealunit.com/realms/pigeonpost-prod",
    clientId: "pigeonpost-web",   // live in the pigeonpost-prod realm (public, PKCE S256)
    redirectPath: "/account",
    scope: "openid email profile",
  },

  catalog: {
    apiUrl: "https://api.masaas.org/v1",
    productSlug: "pigeonpost",
    packageSlug: "handle-yearly",
    planSlug: "handle-yearly-annual-usd",
  },
  price: { amount: 5, currency: "USD", interval: "year" },

  handle: { min: 3, max: 32 },
};
