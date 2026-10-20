// Web purchases on iyzico abonelik (SDS §8.5): individual Pigeonpost Cloud plans and enterprise
// seat packages, started as saas_be hosted checkouts and activated in the postbox through its
// internal entitlement API.
//
// Trust boundaries:
//  - The browser chooses a plan code and an intent id, nothing else. Prices, saas_be plan ids,
//    client references and return URLs are resolved here and checked against the catalog.
//  - A redirect back from iyzico proves nothing. Access follows saas_be's authoritative state,
//    read with the member's token (return polling) or the internal access list (reconciler).
//  - paid_through only ever comes from a provider-confirmed paid period; a cancellation or a
//    missing subscription never shortens access below it, and nothing here extends it.

import { createHash } from "node:crypto";
import { config } from "./config.js";
import { isUuid, limitsForPackage, plan, planForLocalId, planForProviderReference } from "./catalog.js";
import * as saas from "./saas.js";
import * as postbox from "./postbox.js";

const { fail } = saas;
const INTENT = /^[a-f0-9]{32}$/;
const ORDER = /^[a-zA-Z0-9_-]{1,128}$/;
const NAMESPACE = /^[a-z0-9][a-z0-9._-]{0,31}$/;
const YEAR_SECONDS = 400 * 86400;
const now = () => Math.floor(Date.now() / 1000);
const fingerprint = (value) => createHash("sha256").update(JSON.stringify(value)).digest("hex");
const environment = () => (config.iyzicoMode === "live" ? "live" : "test");
const seconds = (value) => (value ? Math.floor(Date.parse(value) / 1000) : null);

function strict(body, allowed, required = allowed) {
  if (!body || typeof body !== "object" || Array.isArray(body)) fail(400, "A JSON object is required.", "invalid_request");
  const extra = Object.keys(body).find((key) => !allowed.includes(key));
  if (extra) fail(400, `Unexpected field: ${extra}`, "invalid_request");
  const missing = required.find((key) => body[key] === undefined);
  if (missing) fail(400, `${missing} is required.`, "invalid_request");
  return body;
}
const intentId = (value) => (INTENT.test(value || "") ? value : fail(400, "Reload the page before starting again.", "invalid_intent"));
const locale = (value) => (value === undefined ? "en" : ["tr", "en"].includes(value) ? value : fail(400, "locale must be tr or en.", "invalid_request"));

export function createBilling(state) {
  // ---- checkout intents -------------------------------------------------------------------

  /// Record the intent before any provider call, enforcing the per-account hourly budget. A replay
  /// of the same intent (same account, same request) returns the stored intent instead.
  async function reserve(intent, request) {
    const print = fingerprint(request);
    const outcome = await state.update((data) => {
      const existing = data.intents[intent.id];
      if (existing) {
        if (existing.account !== intent.account || existing.fingerprint !== print) return { conflict: true };
        return { intent: existing, replay: true };
      }
      const since = Date.now() - 3600_000;
      const recent = Object.values(data.intents).filter((i) => i.account === intent.account && i.created_at > since).length;
      if (recent >= config.checkoutsPerHour) return { limited: true };
      data.intents[intent.id] = { ...intent, fingerprint: print, created_at: Date.now(), status: "starting" };
      return { intent: data.intents[intent.id], replay: false };
    });
    if (outcome.conflict) fail(409, "This checkout was started with different details. Reload and try again.", "intent_conflict");
    if (outcome.limited) fail(429, "Too many checkouts started in the last hour. Try again later.", "rate_limited");
    if (outcome.intent.status === "failed") fail(409, "This checkout did not start. Reload and try again.", "intent_failed");
    return outcome.intent;
  }

  const saveIntent = (id, patch) => state.update((data) => Object.assign(data.intents[id], patch, { updated_at: Date.now() }));

  /// Resolve the saas_be edition for a plan code and check it against the catalog.
  async function edition(token, entry) {
    const offer = saas.validOffer(await saas.offer(token, entry.package_slug, entry.currency),
      { currency: entry.currency, interval: entry.interval });
    const listed = entry.editions[String(offer.pricing_year)];
    if (!listed?.plan_id || listed.plan_id !== offer.plan_id || listed.amount !== String(offer.amount)) {
      fail(503, "Plan pricing has changed and is being reviewed. Try again later.", "offer_mismatch");
    }
    return { plan_id: offer.plan_id, package_id: offer.package_id, amount: listed.amount, year: offer.pricing_year };
  }

  async function start(token, intent, entry, body) {
    // A replay must send byte-identical input, so the edition is resolved once and stored.
    let selection = intent.selection;
    if (!selection) {
      selection = await edition(token, entry);
      await saveIntent(intent.id, { selection });
    }
    let result;
    try {
      result = saas.validCheckout(await saas.startSubscription(token, {
        plan_id: selection.plan_id, package_id: selection.package_id, billing_profile_id: body.billing_profile_id,
        client_reference_id: intent.client_reference, method: "hosted", initial_status: "ACTIVE", quantity: 1,
        trial_period_days: 0, return_url: intent.return_url, locale: body.locale,
      }, `${intent.kind === "ent" ? "pp-ent" : "pp-cloud"}-${intent.id}`));
    } catch (error) {
      // A definite refusal (bad billing details, conflict) ends this intent; the page starts a new
      // one after the member fixes it. Anything ambiguous stays replayable under the same key.
      if ([400, 403, 404, 422].includes(error.status)) await saveIntent(intent.id, { status: "failed", error: error.message });
      throw error;
    }
    if (intent.subscription_id && intent.subscription_id !== result.subscription_id) fail(502, "Billing returned a different subscription.", "invalid_upstream");
    await saveIntent(intent.id, { subscription_id: result.subscription_id, status: "checkout_pending" });
    await state.update((data) => {
      data.refs[result.subscription_id] ||= { kind: intent.kind, account: intent.account, draft_id: intent.draft_id || null,
        plan_code: intent.plan_code, intent: intent.id, status: "pending", paid_through: null, version: 0, pushed: null };
    });
    return {
      intent_id: intent.id, subscription_id: result.subscription_id, plan_code: intent.plan_code,
      amount: selection.amount, currency: entry.currency, interval: entry.interval,
      checkout_form_content: result.checkout_form_content, expires_at: result.expires_at,
      ...(intent.draft_id ? { org_id: intent.draft_id } : {}),
    };
  }

  // ---- entitlement mapping ----------------------------------------------------------------

  /// From an authoritative member read to the entitlement the postbox should hold.
  function fromSubscription(sub, ref) {
    const t = now();
    const end = seconds(sub.current_period_end);
    const paid = (sub.orders || []).filter((o) => o.status === "SUCCESS" && o.period_end).map((o) => seconds(o.period_end));
    let paidThrough = ref?.paid_through ?? null;
    const plausible = (value) => value && value > t && value < t + YEAR_SECONDS;
    if (sub.status === "active" && plausible(end)) paidThrough = Math.max(paidThrough || 0, end);
    // A cancellation stops renewals; the period that was paid for stays paid.
    if (sub.status === "canceled" && plausible(end) && paid.some((p) => p >= end - 86400)) paidThrough = Math.max(paidThrough || 0, end);
    if (!paidThrough) return null;
    const status = paidThrough <= t ? "expired"
      : sub.status === "active" ? "active" : sub.status === "past_due" ? "past_due"
        : sub.status === "canceled" ? "canceled" : ref?.status && ref.status !== "pending" ? ref.status : "active";
    const resolved = planForProviderReference(sub.pricing_plan_reference);
    return { plan_code: resolved?.code || ref.plan_code, plan_version: resolved?.year || ref.plan_version || "unknown",
      status, period_end: end || paidThrough, paid_through: paidThrough,
      cancel_requested_at: sub.status === "canceled" ? (ref?.cancel_requested_at || t) : null };
  }

  /// Push an entitlement for one saas_be subscription if it changed since the last push.
  async function push(id, ref, ent, subjectCheck) {
    const entry = plan(ent.plan_code);
    const limits = entry || {};
    const body = {
      plan_code: ent.plan_code, plan_version: String(ent.plan_version), status: ent.status,
      period_end: ent.period_end, paid_through: ent.paid_through,
      cancel_requested_at: ent.cancel_requested_at ?? null, pending_plan_code: null, pending_effective_at: null,
    };
    if (ref.kind === "cloud") {
      const names = limits.names ?? ref.names;
      if (!names) fail(502, "Unknown plan capacity", "catalog_unknown");
      Object.assign(body, { names, seats: null, agent_limit: null, storage_bytes: null });
    }
    const print = fingerprint(body);
    if (ref.pushed === print) return { applied: false, unchanged: true };
    const version = Math.max(Date.now(), (ref.version || 0) + 1);
    let result;
    if (ref.kind === "cloud") {
      result = await postbox.putEntitlement("iyzico", environment(), id, {
        subject: { kind: "account", id: ref.account }, ...(subjectCheck ? { subject_check: subjectCheck } : {}),
        ...body, reconciliation_version: version, operation_id: `sync-${id}-${version}`,
      });
    } else {
      const seats = limits.seats ?? ref.seats;
      const entitlement = { provider: "iyzico", environment: environment(), reference: id, plan_code: body.plan_code,
        plan_version: body.plan_version, seats, status: body.status, period_end: body.period_end,
        paid_through: body.paid_through, reconciliation_version: version };
      result = ref.org_active
        ? await postbox.putOrgEntitlement(ref.draft_id, entitlement)
        : await postbox.activateOrg(ref.draft_id, { operation_id: `activate-${ref.draft_id}`, entitlement });
    }
    await state.update((data) => {
      Object.assign(data.refs[id], {
        pushed: print, version, status: ent.status, paid_through: ent.paid_through, plan_code: ent.plan_code,
        plan_version: String(ent.plan_version), cancel_requested_at: ent.cancel_requested_at ?? null,
        ...(ref.kind === "cloud" ? { names: body.names } : { seats: limits.seats ?? ref.seats, org_active: true,
          org_status: result?.org?.status || data.refs[id].org_status }),
      });
    });
    return result;
  }

  async function syncMember(token, id) {
    const ref = await state.read((data) => data.refs[id] && { ...data.refs[id] });
    if (!ref) fail(404, "Subscription not found", "not_found");
    const sub = saas.validSubscription(await saas.getSubscription(token, id), id);
    const ent = fromSubscription(sub, ref);
    let pushed = null;
    if (ent) pushed = await push(id, ref, ent);
    return { sub, ent, pushed };
  }

  function view(intent, sub, ent) {
    const status = ent ? ent.status : sub?.provider_status === "FAILED" || intent.status === "failed" ? "failed" : "pending";
    return { intent_id: intent.id, subscription_id: intent.subscription_id || null, plan_code: intent.plan_code,
      status, provider_status: sub?.provider_status || null, paid_through: ent?.paid_through || null,
      next_payment_at: sub?.next_payment_at || null, ...(intent.draft_id ? { org_id: intent.draft_id } : {}) };
  }

  async function ownedIntent(token, id, kind) {
    const account = await postbox.accountFor(token);
    const intent = await state.read((data) => data.intents[id] && { ...data.intents[id] });
    if (!intent || intent.account !== account || intent.kind !== kind) fail(404, "Checkout not found", "not_found");
    return intent;
  }

  // ---- individual Cloud plans -------------------------------------------------------------

  async function cloudCheckout(token, body) {
    strict(body, ["intent_id", "plan_code", "billing_profile_id", "locale"], ["intent_id", "plan_code", "billing_profile_id"]);
    const id = intentId(body.intent_id);
    const entry = plan(body.plan_code);
    if (!entry || entry.family !== "cloud") fail(400, "Choose a Pigeonpost Cloud plan.", "invalid_plan");
    if (!isUuid(body.billing_profile_id)) fail(400, "Save your billing details first.", "billing_profile_required");
    const request = { kind: "cloud", plan_code: entry.code, billing_profile_id: body.billing_profile_id, locale: locale(body.locale) };
    const account = await postbox.accountFor(token);
    const known = await state.read((data) => data.intents[id]);
    if (!known) {
      // One active Cloud subscription per account: more names come from changing plan, never from
      // a second, overlapping charge.
      const active = (await saas.listSubscriptions(token)).find((s) =>
        ["ACTIVE", "PENDING", "UNPAID"].includes(s.provider_status) && planForProviderReference(s.pricing_plan_reference)?.family === "cloud");
      if (active) fail(409, "You already have a Pigeonpost Cloud subscription. Change its plan instead.", "already_subscribed");
    }
    const intent = await reserve({
      id, kind: "cloud", account, plan_code: entry.code,
      // `pp-cloud:<account>` identifies the buyer; the intent suffix lets a member retry after an
      // abandoned hosted form, which saas_be keeps reserved under its client reference.
      client_reference: `pp-cloud:${account}:${id}`,
      return_url: `${config.allowedOrigins[0]}/account?checkout=${id}`,
    }, { ...request, account });
    return start(token, intent, entry, request);
  }

  async function cloudStatus(token, id) {
    const intent = await ownedIntent(token, intentId(id), "cloud");
    if (!intent.subscription_id) return view(intent, null, null);
    const { sub, ent } = await syncMember(token, intent.subscription_id);
    if (ent && intent.status !== "active") await saveIntent(intent.id, { status: "active" });
    return view(intent, sub, ent);
  }

  /// The member's Cloud subscriptions with their management state, and the postbox's plan view.
  async function cloudOverview(token) {
    const [subs, plan_] = await Promise.all([saas.listSubscriptions(token), postbox.memberGet(token, "/v1/me/entitlements")]);
    const subscriptions = subs.map((s) => {
      const resolved = planForProviderReference(s.pricing_plan_reference);
      if (!resolved || resolved.family !== "cloud") return null;
      return { id: s.subscription_id, plan_code: resolved.code, names: resolved.names, currency: resolved.currency,
        provider_status: s.provider_status, recurring_price: s.recurring_price || null,
        pending_change: s.pending_change && Object.keys(s.pending_change).length ? true : false,
        reconciliation_required: Boolean(s.reconciliation_required) };
    }).filter(Boolean);
    return { subscriptions, plan: plan_ };
  }

  async function ownedCloudSubscription(token, id) {
    if (!isUuid(id)) fail(404, "Subscription not found", "not_found");
    const account = await postbox.accountFor(token);
    const sub = saas.validSubscription(await saas.getSubscription(token, id), id);
    const resolved = planForProviderReference(sub.pricing_plan_reference);
    if (!resolved || resolved.family !== "cloud") fail(404, "Subscription not found", "not_found");
    await state.update((data) => {
      data.refs[id] ||= { kind: "cloud", account, plan_code: resolved.code, status: "pending", paid_through: null, version: 0, pushed: null };
    });
    const ref = await state.read((data) => ({ ...data.refs[id] }));
    if (ref.account !== account) fail(404, "Subscription not found", "not_found");
    return { sub, resolved, ref };
  }

  async function cloudSubscription(token, id) {
    const { sub, ref } = await ownedCloudSubscription(token, id);
    const ent = fromSubscription(sub, ref);
    if (ent) await push(id, ref, ent);
    return { id, plan_code: ent?.plan_code || ref.plan_code, status: ent?.status || "pending",
      provider_status: sub.provider_status, paid_through: ent?.paid_through || null,
      current_period_end: sub.current_period_end, next_payment_at: sub.next_payment_at || null,
      recurring_price: sub.recurring_price || null, pending_change: sub.pending_change || null,
      failed_orders: (sub.orders || []).filter((o) => o.status === "FAILED").map((o) => ({
        order_reference_code: o.order_reference_code, amount: o.amount, currency: o.currency, failed_at: o.failed_at || null })) };
  }

  async function cloudOperation(token, id, operation, body) {
    const { sub, resolved, ref } = await ownedCloudSubscription(token, id);
    let payload, key;
    if (operation === "cancel") {
      strict(body, ["intent_id"]);
      payload = {};
    } else if (operation === "retry") {
      strict(body, ["intent_id", "order_reference_code"]);
      if (!ORDER.test(body.order_reference_code) || !(sub.orders || []).some((o) => o.status === "FAILED" && o.order_reference_code === body.order_reference_code)) {
        fail(400, "Choose a failed payment of this subscription.", "invalid_order");
      }
      payload = { order_reference_code: body.order_reference_code };
    } else if (operation === "card-update") {
      strict(body, ["intent_id", "locale"], ["intent_id"]);
      payload = { scope: "subscription", return_url: `${config.allowedOrigins[0]}/account?card=${intentId(body.intent_id)}`, locale: locale(body.locale) };
    } else if (operation === "upgrade") {
      strict(body, ["intent_id", "plan_code", "upgrade_period"]);
      const target = plan(body.plan_code);
      if (!target || target.family !== "cloud" || target.currency !== resolved.currency || target.code === resolved.code) {
        fail(400, "Choose a different Cloud plan in the same currency.", "invalid_plan");
      }
      if (!["NOW", "NEXT_PERIOD"].includes(body.upgrade_period)) fail(400, "upgrade_period must be NOW or NEXT_PERIOD.", "invalid_request");
      const view_ = await postbox.memberGet(token, "/v1/me/entitlements");
      const used = Number(view_?.web_plan?.used ?? 0);
      if (target.names < used) fail(409, `This account uses ${used} names. Release names before choosing a ${target.names}-name plan.`, "names_in_use");
      const selection = await edition(token, target);
      payload = { plan_id: selection.plan_id, trial_period_days: 0, upgradePeriod: body.upgrade_period };
    } else fail(404, "not found", "not_found");
    key = `pp-${operation}-${intentId(body.intent_id)}`;
    const result = await saas.operate(token, id, operation, payload, key);
    if (operation === "card-update") {
      if (typeof result?.checkout_form_content !== "string" || !result.checkout_form_content.trim() || result.checkout_form_content.length > 200_000) {
        fail(502, "Billing returned an invalid card form.", "invalid_upstream");
      }
      return { subscription_id: id, checkout_form_content: result.checkout_form_content };
    }
    if (operation === "cancel") await state.update((data) => { data.refs[id].cancel_requested_at ||= now(); });
    const fresh = saas.validSubscription(await saas.getSubscription(token, id), id);
    const ent = fromSubscription(fresh, { ...ref, cancel_requested_at: operation === "cancel" ? now() : ref.cancel_requested_at });
    if (ent) await push(id, await state.read((data) => ({ ...data.refs[id] })), ent);
    return { id, operation, status: ent?.status || "pending", provider_status: fresh.provider_status,
      paid_through: ent?.paid_through || null, pending_change: fresh.pending_change || null };
  }

  // ---- enterprise (Phase 4 portal calls these) --------------------------------------------

  const PROFILE_FIELDS = ["account_type", "legal_name", "first_name", "last_name", "entity_name", "tax_id", "vat_number",
    "identity_number", "tax_office", "e_fatura_alias", "billing_email", "phone", "address"];
  const ADDRESS_FIELDS = ["line1", "line2", "city", "state", "postal_code", "country"];

  async function enterpriseBillingProfile(token, body) {
    strict(body, ["intent_id", ...PROFILE_FIELDS], ["intent_id", "legal_name", "billing_email", "address"]);
    const id = intentId(body.intent_id);
    const { intent_id: _ignored, ...profile } = body;
    if (profile.account_type !== undefined && !["individual", "entity"].includes(profile.account_type)) fail(400, "account_type is invalid.", "invalid_request");
    strict(profile.address, ADDRESS_FIELDS, ["line1", "city", "country"]);
    for (const [key, value] of Object.entries({ ...profile, ...profile.address })) {
      if (key !== "address" && value !== null && (typeof value !== "string" || value.length > 512)) fail(400, `${key} is invalid.`, "invalid_request");
    }
    await postbox.accountFor(token);
    return saas.createBillingProfile(token, profile, `pp-ent-profile-${id}`);
  }

  async function enterpriseCheckout(token, body) {
    strict(body, ["intent_id", "plan_code", "billing_profile_id", "org", "locale"], ["intent_id", "plan_code", "billing_profile_id", "org"]);
    const id = intentId(body.intent_id);
    const entry = plan(body.plan_code);
    if (!entry || entry.family !== "ent") fail(400, "Choose an enterprise plan. Above 500 seats, contact sales.", "invalid_plan");
    if (!isUuid(body.billing_profile_id)) fail(400, "Save the billing details first.", "billing_profile_required");
    strict(body.org, ["name", "namespace"]);
    const name = String(body.org.name || "").trim();
    const namespace = String(body.org.namespace || "").trim().toLowerCase();
    if (!name || name.length > 120) fail(400, "Give the organization a name of up to 120 characters.", "invalid_request");
    if (!NAMESPACE.test(namespace)) fail(400, "Choose an address of 1–32 letters, numbers, dots, underscores or hyphens.", "invalid_namespace");
    const request = { kind: "ent", plan_code: entry.code, billing_profile_id: body.billing_profile_id, locale: locale(body.locale), name, namespace };
    const account = await postbox.accountFor(token);
    const intent = await reserve({ id, kind: "ent", account, plan_code: entry.code }, { ...request, account });
    let draftId = intent.draft_id;
    if (!draftId) {
      const created = await postbox.createOrgDraft({ operation_id: id, owner_account: account, name, namespace, plan_code: entry.code, seats: entry.seats });
      draftId = created?.org?.id;
      if (typeof draftId !== "string" || !/^org_[a-z0-9]{6,64}$/.test(draftId)) fail(502, "Organization draft failed.", "postbox_invalid");
      await saveIntent(id, { draft_id: draftId, client_reference: `pp-org:${draftId}`,
        return_url: `${config.enterpriseOrigin}/checkout/return?d=${draftId}` });
    }
    const current = await state.read((data) => ({ ...data.intents[id] }));
    const result = await start(token, current, entry, request);
    await state.update((data) => { data.refs[result.subscription_id].seats = entry.seats; });
    return result;
  }

  async function enterpriseStatus(token, draftId) {
    const account = await postbox.accountFor(token);
    const intent = await state.read((data) => Object.values(data.intents).find((i) => i.kind === "ent" && i.draft_id === draftId && i.account === account));
    if (!intent) fail(404, "Checkout not found", "not_found");
    if (!intent.subscription_id) return view(intent, null, null);
    const { sub, ent } = await syncMember(token, intent.subscription_id);
    const org = ent ? (await postbox.getOrg(draftId))?.org || null : null;
    return { ...view(intent, sub, ent), org };
  }

  // ---- reconciler -------------------------------------------------------------------------

  /// Activate and renew from saas_be's internal access list, so a closed tab or a missed return
  /// still reaches the postbox. Subscriptions that left the list keep their verified paid period
  /// and are marked expired once it ends.
  async function reconcile() {
    const rows = await saas.accessList();
    const seen = new Set();
    const report = { activated: 0, renewed: 0, expired: 0, skipped: 0, errors: 0 };
    for (const row of rows) {
      const cloud = /^pp-cloud:(acct_[A-Za-z0-9_]{6,64})(?::[a-f0-9]{32})?$/.exec(row.external_reference);
      const org = /^pp-org:(org_[a-z0-9]{6,64})$/.exec(row.external_reference);
      if (!cloud && !org) continue;
      seen.add(row.subscription_id);
      const end = seconds(row.current_period_end);
      if (!(end > now() && end < now() + YEAR_SECONDS) || !["active", "trialing"].includes(row.status)) { report.skipped++; continue; }
      const edition_ = planForLocalId(row.plan_id);
      const limits = edition_ || limitsForPackage(row.package_slug);
      if (!limits || limits.family !== (cloud ? "cloud" : "ent")) { report.skipped++; continue; }
      try {
        const ref = await state.update((data) => {
          const existing = data.refs[row.subscription_id];
          if (existing && (cloud ? existing.account !== cloud[1] : existing.draft_id !== org[1])) return null;
          return (data.refs[row.subscription_id] ||= {
            kind: cloud ? "cloud" : "ent", account: cloud ? cloud[1] : null, draft_id: org ? org[1] : null,
            plan_code: edition_?.code || null, status: "pending", paid_through: null, version: 0, pushed: null,
            ...(cloud ? { names: limits.names } : { seats: limits.seats }),
          }) && { ...data.refs[row.subscription_id] };
        });
        if (!ref) { report.skipped++; continue; }
        const planCode = edition_?.code || ref.plan_code;
        if (!planCode) { report.skipped++; continue; }
        const wasActive = Boolean(ref.paid_through && ref.paid_through > now());
        const ent = { plan_code: planCode, plan_version: edition_?.year || ref.plan_version || "unknown",
          status: ref.cancel_requested_at ? "canceled" : "active", period_end: end,
          paid_through: Math.max(end, ref.paid_through || 0), cancel_requested_at: ref.cancel_requested_at || null };
        const result = await push(row.subscription_id, { ...ref, names: limits.names ?? ref.names, seats: limits.seats ?? ref.seats }, ent,
          cloud && typeof row.keycloak_id === "string" ? { oidc_sub: row.keycloak_id } : null);
        if (!result?.unchanged) wasActive ? report.renewed++ : report.activated++;
        if (ref.intent) await state.update((data) => { if (data.intents[ref.intent]) data.intents[ref.intent].status = "active"; });
      } catch (error) {
        report.errors++;
        console.error(`reconcile: ${row.subscription_id} ${error.code || error.status || "error"}`);
      }
    }
    const lapsed = await state.read((data) => Object.entries(data.refs)
      .filter(([id, r]) => !seen.has(id) && r.paid_through && r.paid_through <= now() && r.status !== "expired")
      .map(([id, r]) => [id, { ...r }]));
    for (const [id, ref] of lapsed) {
      try {
        await push(id, ref, { plan_code: ref.plan_code, plan_version: ref.plan_version || "unknown", status: "expired",
          period_end: ref.paid_through, paid_through: ref.paid_through, cancel_requested_at: ref.cancel_requested_at || null });
        report.expired++;
      } catch (error) {
        report.errors++;
        console.error(`reconcile: ${id} ${error.code || error.status || "error"}`);
      }
    }
    return report;
  }

  return { cloudCheckout, cloudStatus, cloudOverview, cloudSubscription, cloudOperation,
    enterpriseBillingProfile, enterpriseCheckout, enterpriseStatus, reconcile };
}
