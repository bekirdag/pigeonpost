// Payment state lives in MASAAS; handle ownership lives in the postbox.
import { createHash } from "node:crypto";
import { config } from "./config.js";
import * as billing from "./masaas.js";

const referencePrefix = "pigeonpost:handle:";
const inFlight = new Set();
const fail = (status, message) => { throw Object.assign(new Error(message), { status }); };
const callbackUrl = () => `${config.allowedOrigins[0]}/api/v1/checkout/callback`;
const requestKey = (...parts) => `pp_checkout_${createHash("sha256").update(JSON.stringify(parts)).digest("hex")}`;

export function handleName(value) {
  const name = String(value || "").trim().toLowerCase().replace(/^\//, "");
  if (!/^[a-z0-9]{3,32}$/.test(name)) fail(400, "A handle needs 3–32 letters or digits");
  return name;
}

export function subscriptionHandle(subscription) {
  const ref = subscription?.external_reference;
  return typeof ref === "string" && ref.startsWith(referencePrefix) ? ref.slice(referencePrefix.length) : "";
}

function matches(subscription, name) {
  return subscriptionHandle(subscription) === name && subscription.plan_slug === config.planSlug;
}

function paidThrough(subscription) {
  const end = Date.parse(subscription?.current_period_end);
  return subscription?.status === "active" && Number.isFinite(end) && end > Date.now() ? Math.floor(end / 1000) : 0;
}

function requireDelivery() {
  if (!config.namespaceGrantToken) fail(503, "Handle checkout is temporarily unavailable");
}

async function deliver(token, subscription, name, accountId) {
  requireDelivery();
  const expiresAt = matches(subscription, name) && paidThrough(subscription);
  if (!expiresAt) fail(402, "No paid, active subscription for that handle");
  const owner = accountId || await billing.accountIdFor(token);
  if (!owner) fail(502, "Could not identify the account");
  const bound = await billing.grantNamespace({ namespace: name, accountId: owner, expiresAt });
  if (!bound.ok) fail(502, `Payment is confirmed, but the handle could not be assigned: ${bound.reason}. Try again to finish this purchase.`);
  return { status: "active", handle: name, subscriptionId: subscription.id, bound: true };
}

function hostedPayment(result, name) {
  const url = result?.action?.redirect_url;
  if (!url || new URL(url).protocol !== "https:") fail(502, "The payment provider did not return a secure checkout URL");
  return {
    status: "payment_action_required", handle: name, subscriptionId: result.subscription_id,
    paymentId: result.payment_id, checkoutUrl: url,
  };
}

async function checkoutResult(token, result, name, accountId) {
  if (result?.status === "payment_action_required") return hostedPayment(result, name);
  return deliver(token, result, name, accountId);
}

export async function startCheckout(token, { handle, operationId }) {
  const name = handleName(handle);
  if (!/^[a-f0-9]{32}$/.test(operationId || "")) fail(400, "Reload the account page before starting checkout");
  requireDelivery();
  const accountId = await billing.accountIdFor(token);
  if (!accountId) fail(502, "Could not identify the account");
  // Prevent concurrent clicks from racing the subscription/payment writes on this adapter.
  if (inFlight.has(name)) fail(409, "Checkout is still starting. Try again in a moment");
  inFlight.add(name);
  try {
    const subscriptions = await billing.listSubscriptions(token);
    const paid = subscriptions.find((s) => matches(s, name) && paidThrough(s));
    if (paid) return await deliver(token, paid, name, accountId);
    const available = await billing.handleAvailable(name);
    if (available === null) fail(503, "Could not check handle availability. Try again shortly");
    if (!available) fail(409, `/${name} is already taken`);

    const pending = subscriptions.find((s) => matches(s, name) && s.status === "past_due");
    const key = requestKey(accountId, name, operationId, pending?.id || "create");
    if (pending) {
      const payments = (await billing.listPayments(token)).filter((p) =>
        p.metadata?.commercial_metadata?.subscription_id === pending.id);
      // Resume the existing bank page, including after a lost HTTP response or a reload.
      // A pending payment may still settle: never start another charge alongside it.
      const unsettled = payments.find((p) => p.status === "pending");
      if (unsettled) {
        const provider = unsettled.metadata?.provider_response;
        if (!provider?.checkout_session_id || !provider.checkout_redirect_url) {
          fail(409, "Payment is still being processed. Do not start another payment; try again shortly");
        }
        return hostedPayment({ subscription_id: pending.id, payment_id: unsettled.id,
          action: { redirect_url: provider.checkout_redirect_url } }, name);
      }
      // The backend reuses a succeeded, uninvoiced payment before considering a new charge.
      return await checkoutResult(token,
        await billing.retrySubscriptionPayment(token, pending.id, callbackUrl(), key), name, accountId);
    }
    return await checkoutResult(token,
      await billing.subscribeToPlan(token, config.planSlug, referencePrefix + name, callbackUrl(), key), name, accountId);
  } finally {
    inFlight.delete(name);
  }
}

export async function completeCheckout(token, { subscriptionId, paymentId }) {
  requireDelivery();
  if (!subscriptionId || !paymentId) fail(400, "The checkout return is incomplete");
  const subscription = await billing.getSubscription(token, subscriptionId);
  const name = handleName(subscriptionHandle(subscription));
  if (!matches(subscription, name)) fail(404, "Handle purchase not found");
  if (paidThrough(subscription)) return deliver(token, subscription, name);
  const payment = await billing.getPayment(token, paymentId);
  if (payment.metadata?.commercial_metadata?.subscription_id !== subscription.id) fail(404, "Handle payment not found");
  const sessionId = payment.metadata?.provider_response?.checkout_session_id;
  if (!sessionId) fail(409, "The payment session is not ready. Try again shortly");
  // Resolve the session from the member-scoped payment record, never from callback input.
  // Completion can change from pending to succeeded, so each check gets a fresh request key.
  const result = await billing.completeSubscriptionPayment(token, subscription.id, payment.id, sessionId);
  return deliver(token, result, name);
}

export async function claimHandle(token, handle) {
  const name = handleName(handle);
  const subscriptions = await billing.listSubscriptions(token);
  const paid = subscriptions.find((s) => matches(s, name) && paidThrough(s));
  if (!paid) fail(402, "No paid, active subscription for that handle");
  return deliver(token, paid, name);
}
