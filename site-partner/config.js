// partner.pigeonpost.dev — where this platform's postbox, bridge and realm client are.
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
