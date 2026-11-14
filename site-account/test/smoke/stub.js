// Screenshot smoke only (assembled into a temp dir by assemble.mjs; never deployed): a fake
// signed-in session and a stub postbox/bridge, so every signed-in page can be looked at.
(function () {
  "use strict";
  const q = new URLSearchParams(window.location.search);
  const b = (o) => btoa(JSON.stringify(o)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  sessionStorage.setItem("pp_session", `${b({ alg: "none" })}.${b({ sub: "s1", preferred_username: "ada", name: "Ada Lovelace", email: "ada@example.com" })}.sig`);
  if (q.get("theme")) localStorage.setItem("ppi_theme", q.get("theme"));
  const fix = window.__FIX;
  if (q.get("billing") === "none") fix["GET /api/v1/me/overview"] = { ...fix["GET /api/v1/me/overview"], billingProfiles: [] };
  if (q.get("platforms") === "all") {
    fix["GET /v1/publishers"] = { data: [{ id: "pub_1" }] };
    fix["GET /v1/orgs"] = { orgs: [{ id: "org_abcdef", name: "Acme", my_roles: ["owner"] }] };
  }
  window.fetch = async (u, init = {}) => {
    const url = new URL(String(u), window.location.href);
    const r = fix[`${init.method || "GET"} ${url.pathname}`];
    const [status, body] = r === undefined ? [404, { error: "not_found" }] : Array.isArray(r) ? r : [200, r];
    return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
  };
})();
