// Pigeonpost store adapter — the member backend the pigeonpost.dev account page calls.
//
// Modeled on theneuralledger's adapter: the browser holds the customer's own OIDC token; this
// service forwards member operations to the MASAAS SaaS backend with that token plus x-product-slug.
// MASAAS holds the payment-gateway keys and hosts the card capture. Reverse-proxied under
// https://pigeonpost.dev/api so everything stays on one origin.

import http from "node:http";
import { config, configured } from "./config.js";
import * as masaas from "./masaas.js";

function send(res, status, body, origin) {
  const headers = { "content-type": "application/json", "cache-control": "no-store" };
  if (origin && config.allowedOrigins.includes(origin)) {
    headers["access-control-allow-origin"] = origin;
    headers["access-control-allow-headers"] = "authorization, content-type";
    headers["access-control-allow-methods"] = "GET, POST, OPTIONS";
    headers["vary"] = "origin";
  }
  res.writeHead(status, headers);
  res.end(JSON.stringify(body));
}

const bearer = (req) => {
  const m = /^Bearer\s+(.+)$/i.exec(req.headers["authorization"] || "");
  return m ? m[1] : null;
};

async function readJson(req) {
  const chunks = [];
  for await (const c of req) chunks.push(c);
  if (!chunks.length) return {};
  try { return JSON.parse(Buffer.concat(chunks).toString("utf8")); } catch { return {}; }
}

const statusFrom = (e) => (Number.isInteger(e?.status) ? e.status : 502);
const back = (p) => `${config.allowedOrigins[0]}${p}`;

const server = http.createServer(async (req, res) => {
  const origin = req.headers.origin;
  if (req.method === "OPTIONS") return send(res, 204, {}, origin);
  const path = new URL(req.url, "http://localhost").pathname.replace(/^\/api/, "") || "/";
  const method = req.method;

  try {
    if (method === "GET" && path === "/healthz") {
      return send(res, 200, { ok: true, configured: configured(), product: config.masaasProductSlug }, origin);
    }

    // Public — no auth. What a handle costs, from MASAAS.
    if (method === "GET" && path === "/v1/packages") {
      const packages = await masaas.publicPackages().catch(() => ({ packages: [] }));
      return send(res, 200, packages, origin);
    }

    // OIDC login return: code -> member session. Any client secret stays server-side.
    if (method === "POST" && path === "/v1/auth/exchange") {
      const { code, redirectUri, codeVerifier } = await readJson(req);
      if (!code || !redirectUri) return send(res, 400, { error: "code and redirectUri required" }, origin);
      const tokens = await masaas.exchangeOidcCode(code, redirectUri, codeVerifier);
      return send(res, 200, {
        session: tokens.access_token,
        refresh: tokens.refresh_token || null,
        expiresIn: tokens.expires_in || null,
      }, origin);
    }

    // Handle availability (registry read; the registry is authoritative on rules).
    const avail = /^\/v1\/handles\/([^/]+)\/availability$/.exec(path);
    if (method === "GET" && avail) {
      const name = decodeURIComponent(avail[1]).toLowerCase();
      const taken = await masaas.registryResolves(name);
      return send(res, 200, { name, available: taken === null ? null : !taken }, origin);
    }

    // ---- everything below needs the customer's token ---------------------------------------
    const token = bearer(req);
    if (!token) return send(res, 401, { error: "sign in required" }, origin);

    if (method === "GET" && path === "/v1/me/overview") {
      // One round-trip for the account page: subscriptions + billing + invoices + payment methods.
      const [subs, profiles, invoices, methods] = await Promise.all([
        masaas.listSubscriptions(token).catch(() => null),
        masaas.listBillingProfiles(token).catch(() => null),
        masaas.listInvoices(token).catch(() => null),
        masaas.listPaymentMethods(token).catch(() => null),
      ]);
      return send(res, 200, {
        subscriptions: normalizeSubs(subs),
        billingProfiles: list(profiles),
        invoices: list(invoices),
        paymentMethods: list(methods),
      }, origin);
    }

    if (method === "GET" && path === "/v1/subscriptions") {
      return send(res, 200, { subscriptions: normalizeSubs(await masaas.listSubscriptions(token)) }, origin);
    }

    const cancel = /^\/v1\/subscriptions\/([^/]+)\/cancel$/.exec(path);
    if (method === "POST" && cancel) {
      await masaas.cancelSubscription(token, decodeURIComponent(cancel[1]));
      return send(res, 200, { ok: true }, origin);
    }

    if (method === "GET" && path === "/v1/billing/profiles") {
      return send(res, 200, { profiles: list(await masaas.listBillingProfiles(token)) }, origin);
    }
    if (method === "POST" && path === "/v1/billing/profiles") {
      const profile = await readJson(req);
      return send(res, 201, await masaas.createBillingProfile(token, profile), origin);
    }

    if (method === "GET" && path === "/v1/billing/invoices") {
      return send(res, 200, { invoices: list(await masaas.listInvoices(token)) }, origin);
    }

    if (method === "GET" && path === "/v1/billing/payment-methods") {
      return send(res, 200, { paymentMethods: list(await masaas.listPaymentMethods(token)) }, origin);
    }

    // Add a card: MASAAS returns a hosted setup URL; the browser redirects to it.
    if (method === "POST" && path === "/v1/billing/payment-methods/setup") {
      const { billingProfileId } = await readJson(req);
      const session = await masaas.paymentSetupSession(token, back("/account?added=card"), back("/account"), billingProfileId);
      const url = pickHostedUrl(session);
      if (!url) return send(res, 502, { error: "no hosted setup url returned", session }, origin);
      return send(res, 200, { setupUrl: url }, origin);
    }

    // Buy a handle: subscribe to the plan, then hand back the hosted payment URL.
    if (method === "POST" && path === "/v1/checkout") {
      const { handle } = await readJson(req);
      const subscription = await masaas.subscribeToPlan(token, config.planSlug, handle ? { handle } : undefined);
      const session = await masaas.paymentSetupSession(token, back("/account?bought=1"), back("/account"));
      const url = pickHostedUrl(session);
      // If a card is already on file MASAAS may not need a hosted step; fall back to the account page.
      return send(res, 200, { subscription, checkoutUrl: url || back("/account?bought=1") }, origin);
    }

    return send(res, 404, { error: "not found" }, origin);
  } catch (err) {
    return send(res, statusFrom(err), { error: err.message || "adapter error" }, origin);
  }
});

function pickHostedUrl(session) {
  const s = session && (session.session || session.setup || session);
  return s?.hosted_url || s?.hostedUrl || s?.checkout_url || s?.checkoutUrl ||
    s?.redirect_url || s?.redirectUrl || s?.url || null;
}
function list(payload) {
  if (Array.isArray(payload)) return payload;
  return payload?.data || payload?.[Object.keys(payload || {})[0]] || [];
}
function normalizeSubs(payload) {
  const arr = list(payload);
  return arr.map((s) => ({
    id: s.id || s.subscription_id,
    handle: s.metadata?.handle || s.handle || "",
    status: s.status || "active",
    renewsAt: s.current_period_end || s.renews_at || s.renewsAt || "",
    planSlug: s.plan_slug || s.planSlug || "",
  }));
}

server.listen(config.port, () => {
  // eslint-disable-next-line no-console
  console.log(`pigeonpost store adapter on :${config.port} configured=${configured()} product=${config.masaasProductSlug}`);
});
