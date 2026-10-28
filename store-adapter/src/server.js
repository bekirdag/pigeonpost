// Pigeonpost billing bridge — the member backend the pigeonpost.dev account page (and, later, the
// enterprise portal) calls. Reverse-proxied under https://pigeonpost.dev/api.
//
// The browser holds the customer's own OIDC token; this service forwards member billing calls to
// saas_be with it, starts iyzico abonelik hosted checkouts, and activates paid plans in the postbox
// through the postbox's internal entitlement API. No card data reaches this service: iyzico hosts
// the card form, and saas_be holds the merchant keys.

import http from "node:http";
import { createHash, randomUUID } from "node:crypto";
import { config, configured } from "./config.js";
import * as account from "./masaas.js";
import * as saas from "./saas.js";
import * as postbox from "./postbox.js";
import { publicCatalog, isUuid } from "./catalog.js";
import { State } from "./state.js";
import { createBilling } from "./billing.js";
import { createSales, sourceOf } from "./sales.js";
import { createLimiter } from "./limits.js";

const state = new State(config.stateDir);
const billing = createBilling(state);
const sales = createSales(config.stateDir);
const publicLimiter = createLimiter({ perMinute: config.publicRequestsPerMinute });
const MAX_BODY = 64 * 1024;

function send(res, status, body, origin, extra = {}) {
  const headers = { "content-type": "application/json", "cache-control": "no-store", "x-content-type-options": "nosniff", ...extra };
  if (origin && [...config.allowedOrigins, config.enterpriseOrigin].includes(origin)) {
    headers["access-control-allow-origin"] = origin;
    headers["access-control-allow-headers"] = "authorization, content-type";
    headers["access-control-allow-methods"] = "GET, POST, PATCH, OPTIONS";
    headers["vary"] = "origin";
  }
  res.writeHead(status, headers);
  res.end(JSON.stringify(body));
}

const bearer = (req) => {
  const m = /^Bearer\s+(\S+)$/i.exec(req.headers["authorization"] || "");
  return m ? m[1] : null;
};

async function readJson(req) {
  const chunks = [];
  let size = 0;
  for await (const c of req) {
    size += c.length;
    if (size > MAX_BODY) throw Object.assign(new Error("Request body is too large."), { status: 413 });
    chunks.push(c);
  }
  if (!chunks.length) return {};
  try { return JSON.parse(Buffer.concat(chunks).toString("utf8")); }
  catch { throw Object.assign(new Error("Request body must be JSON."), { status: 400 }); }
}

const decodePart = (part) => {
  try { return decodeURIComponent(part); }
  catch { throw Object.assign(new Error("Malformed path."), { status: 400 }); }
};
const statusFrom = (e) => (Number.isInteger(e?.status) && e.status >= 400 && e.status <= 599 ? e.status : 502);
const list = (payload) => (Array.isArray(payload) ? payload : Array.isArray(payload?.data) ? payload.data : []);
const profileKey = (token, body) => `pp-profile-${createHash("sha256").update(token).update(JSON.stringify(body)).digest("hex").slice(0, 40)}`;

const server = http.createServer(async (req, res) => {
  const origin = req.headers.origin;
  if (req.method === "OPTIONS") return send(res, 204, {}, origin);
  const url = new URL(req.url, "http://localhost");
  const path = url.pathname.replace(/^\/api/, "") || "/";
  const method = req.method;
  let match;

  try {
    if (method === "GET" && path === "/healthz") {
      return send(res, 200, { ok: true, configured: configured(), product: config.masaasProductSlug }, origin);
    }

    // Public: the approved price list.
    if (method === "GET" && path === "/v1/catalog") return send(res, 200, publicCatalog(), origin);

    // Unauthenticated routes that make this service call Keycloak or the postbox share one budget
    // per source address. (Talk to sales has its own, stricter, hourly limit.)
    if ((method === "POST" && (path === "/v1/auth/exchange" || path === "/v1/auth/refresh"))
        || (method === "GET" && /^\/v1\/handles\/[^/]+\/availability$/.test(path))) {
      const wait = publicLimiter.take(sourceOf(req));
      if (wait) return send(res, 429, { error: "Too many requests. Try again shortly.", code: "rate_limited" }, origin, { "retry-after": String(wait) });
    }

    // OIDC login return: code -> member session. Any client secret stays server-side.
    if (method === "POST" && path === "/v1/auth/exchange") {
      const { code, redirectUri, codeVerifier } = await readJson(req);
      if (!code || !redirectUri) return send(res, 400, { error: "code and redirectUri required" }, origin);
      const tokens = await account.exchangeOidcCode(code, redirectUri, codeVerifier);
      return send(res, 200, { session: tokens.access_token, refresh: tokens.refresh_token || null, expiresIn: tokens.expires_in || null }, origin);
    }

    // "Remember me" renewal: refresh token -> fresh member session.
    if (method === "POST" && path === "/v1/auth/refresh") {
      const { refresh } = await readJson(req);
      if (!refresh) return send(res, 400, { error: "refresh required" }, origin);
      let tokens;
      try { tokens = await account.refreshOidcToken(refresh); }
      catch { return send(res, 401, { error: "session expired" }, origin); }
      return send(res, 200, { session: tokens.access_token, refresh: tokens.refresh_token || refresh, expiresIn: tokens.expires_in || null }, origin);
    }

    // Handle availability, from the postbox. `known: false` means "could not tell", never "free".
    if (method === "GET" && (match = /^\/v1\/handles\/([^/]+)\/availability$/.exec(path))) {
      const name = decodePart(match[1]).toLowerCase();
      const available = await account.handleAvailable(name, bearer(req));
      return send(res, 200, { name, available: available === true, known: available !== null }, origin);
    }

    // Enterprise "Talk to sales" (above 500 seats). Public, validated, rate-limited per source.
    if (method === "POST" && path === "/ent/sales") {
      return send(res, 202, sales.record(sourceOf(req), await readJson(req)), origin);
    }

    // ---- everything below needs the customer's token ---------------------------------------
    const token = bearer(req);
    if (!token) return send(res, 401, { error: "sign in required" }, origin);

    if (method === "GET" && path === "/v1/me/overview") {
      // What the account owns comes from the postbox, which every store reports to; a failure
      // there is an error, never an empty account. Billing parts degrade to null individually.
      const [handles, profiles, invoices, cloud] = await Promise.all([
        account.accountHandles(token),
        saas.listBillingProfiles(token).catch(() => null),
        saas.listInvoices(token).catch(() => null),
        billing.cloudOverview(token).catch(() => null),
      ]);
      return send(res, 200, { handles, billingProfiles: list(profiles), invoices: list(invoices), cloud,
        openCurrencies: config.openCurrencies }, origin);
    }

    if (method === "GET" && path === "/v1/billing/profiles") {
      return send(res, 200, { profiles: list(await saas.listBillingProfiles(token)) }, origin);
    }
    if (method === "POST" && path === "/v1/billing/profiles") {
      const profile = await readJson(req);
      return send(res, 201, await saas.createBillingProfile(token, profile, profileKey(token, profile)), origin);
    }
    if (method === "PATCH" && (match = /^\/v1\/billing\/profiles\/([^/]+)$/.exec(path))) {
      const id = decodePart(match[1]);
      if (!isUuid(id) && !/^[A-Za-z0-9_-]{1,64}$/.test(id)) return send(res, 404, { error: "not found" }, origin);
      const profile = await readJson(req);
      return send(res, 200, await saas.updateBillingProfile(token, id, profile, `pp-profile-edit-${randomUUID()}`), origin);
    }

    if (method === "GET" && path === "/v1/billing/invoices") {
      return send(res, 200, { invoices: list(await saas.listInvoices(token)) }, origin);
    }
    if (method === "GET" && (match = /^\/v1\/billing\/invoices\/([^/]+)\/download$/.exec(path))) {
      if (!isUuid(match[1])) return send(res, 404, { error: "not found" }, origin);
      return send(res, 200, await saas.invoiceDownload(token, match[1]), origin);
    }

    // ---- individual Cloud plans -------------------------------------------------------------
    if (method === "GET" && path === "/v1/cloud") return send(res, 200, await billing.cloudOverview(token), origin);
    if (method === "POST" && path === "/v1/cloud/checkout") {
      return send(res, 200, await billing.cloudCheckout(token, await readJson(req)), origin);
    }
    if (method === "GET" && (match = /^\/v1\/cloud\/checkout\/([a-f0-9]{32})$/.exec(path))) {
      return send(res, 200, await billing.cloudStatus(token, match[1]), origin);
    }
    if (method === "GET" && (match = /^\/v1\/cloud\/subscriptions\/([^/]+)$/.exec(path))) {
      return send(res, 200, await billing.cloudSubscription(token, match[1]), origin);
    }
    if (method === "POST" && (match = /^\/v1\/cloud\/subscriptions\/([^/]+)\/(cancel|retry|card-update|upgrade)$/.exec(path))) {
      return send(res, 200, await billing.cloudOperation(token, match[1], match[2], await readJson(req)), origin);
    }
    // Register a name inside the account's web plan.
    if (method === "POST" && path === "/v1/names") {
      const { namespace } = await readJson(req);
      if (typeof namespace !== "string" || !namespace.trim() || namespace.length > 64) return send(res, 400, { error: "namespace required" }, origin);
      return send(res, 200, await postbox.claimName(token, namespace.trim()), origin);
    }

    // ---- enterprise (the Phase 4 portal calls these) ---------------------------------------
    if (method === "POST" && path === "/ent/billing-profile") {
      return send(res, 201, await billing.enterpriseBillingProfile(token, await readJson(req)), origin);
    }
    if (method === "POST" && path === "/ent/checkout") {
      return send(res, 200, await billing.enterpriseCheckout(token, await readJson(req)), origin);
    }
    if (method === "GET" && (match = /^\/ent\/checkout\/(org_[a-z0-9]{6,64})$/.exec(path))) {
      return send(res, 200, await billing.enterpriseStatus(token, match[1]), origin);
    }

    return send(res, 404, { error: "not found" }, origin);
  } catch (err) {
    const status = statusFrom(err);
    if (status >= 500) console.error(`${method} ${path}: ${status} ${err.code || ""}`.trim());
    return send(res, status, { error: err.message || "bridge error", ...(err.code ? { code: err.code } : {}) }, origin);
  }
});

// Missed-return reconciliation: closed tabs, lost callbacks and renewals reach the postbox here.
let reconciling = false;
async function reconcileOnce() {
  if (reconciling || !configured()) return;
  reconciling = true;
  try {
    const report = await billing.reconcile();
    if (report.activated || report.renewed || report.expired || report.errors) console.log(`reconcile ${JSON.stringify(report)}`);
  } catch (error) {
    console.error(`reconcile failed: ${error.code || error.status || "error"}`);
  } finally {
    reconciling = false;
  }
}
if (config.reconcileIntervalMs > 0) setInterval(reconcileOnce, config.reconcileIntervalMs).unref();

server.listen(config.port, () => {
  // eslint-disable-next-line no-console
  console.log(`pigeonpost store adapter on :${server.address().port} configured=${configured()} product=${config.masaasProductSlug}`);
  if (config.reconcileIntervalMs > 0) setTimeout(reconcileOnce, 2000).unref();
});
