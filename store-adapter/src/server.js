// Pigeonpost store adapter — HTTP surface the store frontend calls.
//
// Dependency-free (Node's built-in http). Mirrors theneuralledger's member-backend proxy: the
// browser holds the customer's OIDC token; this service forwards member operations to MASAAS with
// that token plus x-product-slug, and uses the runtime token only for catalog/entitlement reads.
// MASAAS holds the payment-gateway keys and hosts the card capture.

import http from "node:http";
import { config, configured } from "./config.js";
import * as masaas from "./masaas.js";

const RESERVED_HINT = "handle rules and reserved names are enforced by the registry at claim time";

function send(res, status, body, origin) {
  const headers = {
    "content-type": "application/json",
    "cache-control": "no-store",
  };
  if (origin && config.allowedOrigins.includes(origin)) {
    headers["access-control-allow-origin"] = origin;
    headers["access-control-allow-headers"] = "authorization, content-type";
    headers["access-control-allow-methods"] = "GET, POST, OPTIONS";
    headers["vary"] = "origin";
  }
  res.writeHead(status, headers);
  res.end(JSON.stringify(body));
}

function bearer(req) {
  const h = req.headers["authorization"] || "";
  const m = /^Bearer\s+(.+)$/i.exec(h);
  return m ? m[1] : null;
}

async function readJson(req) {
  const chunks = [];
  for await (const c of req) chunks.push(c);
  if (!chunks.length) return {};
  try { return JSON.parse(Buffer.concat(chunks).toString("utf8")); } catch { return {}; }
}

function statusFrom(err) {
  return Number.isInteger(err?.status) ? err.status : 502;
}

const server = http.createServer(async (req, res) => {
  const origin = req.headers.origin;
  if (req.method === "OPTIONS") return send(res, 204, {}, origin);

  const url = new URL(req.url, "http://localhost");
  const path = url.pathname;

  try {
    // Liveness + whether real wiring is present.
    if (req.method === "GET" && path === "/healthz") {
      return send(res, 200, { ok: true, configured: configured(), product: config.masaasProductSlug }, origin);
    }

    // Public catalog — what a handle costs, straight from MASAAS.
    if (req.method === "GET" && path === "/v1/packages") {
      if (!configured()) return send(res, 200, { packages: [], preview: true }, origin);
      const packages = await masaas.publicPackages();
      return send(res, 200, { packages }, origin);
    }

    // Handle availability — registry read + a note that the registry is authoritative on rules.
    const avail = /^\/v1\/handles\/([^/]+)\/availability$/.exec(path);
    if (req.method === "GET" && avail) {
      const name = decodeURIComponent(avail[1]).toLowerCase();
      const taken = await masaas.registryResolves(name);
      if (taken === null) return send(res, 200, { name, available: null, note: RESERVED_HINT }, origin);
      return send(res, 200, { name, available: !taken, note: RESERVED_HINT }, origin);
    }

    // OIDC code -> member session. The client secret, if any, stays here.
    if (req.method === "POST" && path === "/v1/auth/exchange") {
      const { code, redirectUri, codeVerifier } = await readJson(req);
      if (!code || !redirectUri) return send(res, 400, { error: "code and redirectUri required" }, origin);
      const tokens = await masaas.exchangeOidcCode(code, redirectUri, codeVerifier);
      // The store keeps the access token as its session bearer, exactly as TNL's client does.
      return send(res, 200, { session: tokens.access_token, expiresIn: tokens.expires_in }, origin);
    }

    // ---- member operations: require the customer's token ------------------------------------
    const token = bearer(req);
    const memberPaths = ["/v1/subscriptions", "/v1/checkout/session", "/v1/billing/profile"];
    const needsMember = memberPaths.some((p) => path === p) || /^\/v1\/subscriptions\/[^/]+\/cancel$/.test(path);
    if (needsMember && !token) return send(res, 401, { error: "sign in required" }, origin);

    if (req.method === "GET" && path === "/v1/subscriptions") {
      const subs = await masaas.listSubscriptions(token);
      return send(res, 200, normalizeSubs(subs), origin);
    }

    const cancel = /^\/v1\/subscriptions\/([^/]+)\/cancel$/.exec(path);
    if (req.method === "POST" && cancel) {
      await masaas.cancelSubscription(token, decodeURIComponent(cancel[1]));
      return send(res, 200, { ok: true }, origin);
    }

    // The buy: create the subscription, then hand back MASAAS's hosted payment URL to redirect to.
    if (req.method === "POST" && path === "/v1/checkout/session") {
      const { handle, packageId, returnUrl, cancelUrl } = await readJson(req);
      if (!packageId) return send(res, 400, { error: "packageId required" }, origin);
      const subscription = await masaas.selectPackage(token, packageId, handle ? { handle } : undefined);
      const back = returnUrl || `${config.allowedOrigins[0]}/account`;
      const session = await masaas.paymentSetupSession(token, back, cancelUrl || back);
      const checkoutUrl = pickHostedUrl(session);
      if (!checkoutUrl) return send(res, 502, { error: "no hosted payment url returned", subscription }, origin);
      return send(res, 200, { checkoutUrl, subscription }, origin);
    }

    return send(res, 404, { error: "not found" }, origin);
  } catch (err) {
    return send(res, statusFrom(err), { error: err.message || "adapter error" }, origin);
  }
});

function pickHostedUrl(session) {
  const s = session && (session.session || session.setup || session);
  return (
    s?.hosted_url || s?.hostedUrl || s?.checkout_url || s?.checkoutUrl ||
    s?.redirect_url || s?.redirectUrl || s?.url || null
  );
}

function normalizeSubs(payload) {
  const list = Array.isArray(payload) ? payload : (payload?.data || payload?.subscriptions || []);
  const subscriptions = list.map((s) => ({
    id: s.id || s.subscription_id,
    handle: s.metadata?.handle || s.handle || "",
    status: s.status || "active",
    renewsAt: s.current_period_end || s.renews_at || s.renewsAt || "",
  }));
  return { subscriptions };
}

server.listen(config.port, () => {
  // eslint-disable-next-line no-console
  console.log(`pigeonpost store adapter on :${config.port} — configured=${configured()} product=${config.masaasProductSlug}`);
});
