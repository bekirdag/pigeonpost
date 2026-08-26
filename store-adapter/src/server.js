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

    // "Remember me" renewal: refresh token -> fresh member session. Kept public (the refresh token
    // is the credential); a bad/expired token surfaces as 401 so the browser drops it and re-prompts.
    if (method === "POST" && path === "/v1/auth/refresh") {
      const { refresh } = await readJson(req);
      if (!refresh) return send(res, 400, { error: "refresh required" }, origin);
      let tokens;
      try {
        tokens = await masaas.refreshOidcToken(refresh);
      } catch (_) {
        return send(res, 401, { error: "session expired" }, origin);
      }
      return send(res, 200, {
        session: tokens.access_token,
        refresh: tokens.refresh_token || refresh,
        expiresIn: tokens.expires_in || null,
      }, origin);
    }

    // Handle availability, from the postbox — the server that actually hands handles out.
    //
    // `null` means the postbox could not be reached, and the answer to "may I sell this" when you
    // do not know is no. It used to be the opposite: an unreachable or erroring check read as
    // "free", which is how the site came to offer a name it had already sold.
    const avail = /^\/v1\/handles\/([^/]+)\/availability$/.exec(path);
    if (method === "GET" && avail) {
      const name = decodeURIComponent(avail[1]).toLowerCase();
      const available = await masaas.handleAvailable(name);
      return send(res, 200, { name, available: available === true, known: available !== null }, origin);
    }

    // ---- everything below needs the customer's token ---------------------------------------
    const token = bearer(req);
    if (!token) return send(res, 401, { error: "sign in required" }, origin);

    if (method === "GET" && path === "/v1/me/overview") {
      // One round-trip for the account page: subscriptions + billing + invoices + payment methods.
      const [subs, profiles, invoices, methods, handles] = await Promise.all([
        masaas.listSubscriptions(token).catch(() => null),
        masaas.listBillingProfiles(token).catch(() => null),
        masaas.listInvoices(token).catch(() => null),
        masaas.listPaymentMethods(token).catch(() => null),
        // What the account actually owns, which the billing system is not the record of. A handle
        // bought in the App Store has no subscription here, and listing only subscriptions is what
        // made the account page say "No handles yet" to somebody holding two.
        masaas.accountHandles(token).catch(() => null),
      ]);
      return send(res, 200, {
        subscriptions: normalizeSubs(subs),
        handles: handles || [],
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

    // Add a card: MASAAS returns a hosted setup URL; the browser redirects to it. The return URL is
    // kept query-free so MASAAS can append its own completion params cleanly; we also hand back the
    // session id so the browser can complete even when MASAAS returns the URL verbatim.
    if (method === "POST" && path === "/v1/billing/payment-methods/setup") {
      const { billingProfileId } = await readJson(req);
      const session = await masaas.paymentSetupSession(token, back("/account"), back("/account"), billingProfileId);
      const url = pickHostedUrl(session);
      if (!url) return send(res, 502, { error: "no hosted setup url returned", session }, origin);
      return send(res, 200, { setupUrl: url, sessionId: pickSessionId(session), provider: pickProvider(session) }, origin);
    }

    // Finish adding a card: the hosted page captured it at the gateway, but MASAAS only persists the
    // payment method once we complete the session with the token it handed back on the return URL.
    if (method === "POST" && path === "/v1/billing/payment-methods/complete") {
      const { sessionId, provider, billingProfileId } = await readJson(req);
      if (!sessionId) return send(res, 400, { error: "sessionId required" }, origin);
      const paymentMethod = await masaas.completePaymentSession(token, {
        session_id: sessionId,
        ...(provider ? { provider } : {}),
        ...(billingProfileId ? { billing_profile_id: billingProfileId } : {}),
      });
      return send(res, 200, { paymentMethod }, origin);
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

    // Deliver a handle the card paid for.
    //
    // Called by the account page when it comes back from checkout, and safe to call again: it
    // proves the subscription is active before it binds anything, and the postbox upserts.
    //
    // This is the step the web purchase never had. Apple's path binds itself — the app claims the
    // transaction and the postbox checks it with Apple — so a handle bought on a phone worked and
    // one bought with a card did not resolve for anybody.
    if (method === "POST" && path === "/v1/handles/claim") {
      const { handle } = await readJson(req);
      const name = String(handle || "").trim().toLowerCase().replace(/^\/+/, "");
      if (!name) return send(res, 400, { error: "handle required" }, origin);

      // The billing system is the authority on whether this was paid for, and it is asked with the
      // member's own token so one account cannot claim another's purchase.
      const subs = normalizeSubs(await masaas.listSubscriptions(token).catch(() => null));
      const paid = subs.find(
        (s) => String(s.handle || "").replace(/^\/+/, "").toLowerCase() === name &&
               ["active", "trialing", "past_due"].includes(String(s.status || "").toLowerCase()),
      );
      if (!paid) return send(res, 402, { error: "no active subscription for that handle" }, origin);

      const accountId = await masaas.accountIdFor(token);
      if (!accountId) return send(res, 502, { error: "could not identify the account" }, origin);

      const expiresAt = paid.renewsAt ? Math.floor(new Date(paid.renewsAt).getTime() / 1000) : 0;
      const bound = await masaas.grantNamespace({
        namespace: name,
        accountId,
        expiresAt: Number.isFinite(expiresAt) && expiresAt > 0 ? expiresAt : 0,
      });
      if (!bound.ok) return send(res, 502, { error: `could not bind the handle: ${bound.reason}` }, origin);
      return send(res, 200, { handle: name, bound: true }, origin);
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
// The token that MASAAS wants back at complete-session time. MASAAS does not append it to the return
// URL when the URL already carries a query, so the browser stashes this before redirecting.
function pickSessionId(session) {
  const s = session && (session.session || session.setup || session);
  return s?.session_id || s?.sessionId || s?.payment_setup_session || s?.paymentSetupSession ||
    s?.id || s?.token || null;
}
function pickProvider(session) {
  const s = session && (session.session || session.setup || session);
  return s?.provider || s?.gateway || null;
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
