// management.pigeonpost.dev — which postbox and realm client this platform talks to.
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
