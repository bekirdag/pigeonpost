// The postbox, from the bridge's side: the public member API (who is this account, claim a name)
// and the internal entitlement API (service token, private link only).

import { config } from "./config.js";
import { fail } from "./saas.js";

async function call(url, init, label) {
  let res;
  try {
    res = await fetch(url, { ...init, redirect: "error", signal: AbortSignal.timeout(10000) });
  } catch {
    fail(503, `${label} is unreachable. Try again shortly.`, "postbox_unavailable");
  }
  const body = await res.json().catch(() => null);
  return { status: res.status, ok: res.ok, body };
}

// ---- member API --------------------------------------------------------------------------------

/// The postbox's own id for the signed-in account (its `/v1/me/handles` names it).
export async function accountFor(token) {
  const r = await call(`${config.postboxUrl}/v1/me/handles`,
    { headers: { authorization: `Bearer ${token}`, accept: "application/json" } }, "Your account");
  if (r.status === 401) fail(401, "session expired", "unauthorized");
  const account = r.body?.account;
  if (!r.ok || typeof account !== "string" || !/^acct_[A-Za-z0-9_]{6,64}$/.test(account)) {
    fail(502, "Could not identify the account", "postbox_invalid");
  }
  return account;
}

export async function memberGet(token, path) {
  const r = await call(`${config.postboxUrl}${path}`,
    { headers: { authorization: `Bearer ${token}`, accept: "application/json" } }, "Your account");
  if (r.status === 401) fail(401, "session expired", "unauthorized");
  if (!r.ok || !r.body || typeof r.body !== "object") fail(502, "Could not load your plan. Try again.", "postbox_invalid");
  return r.body;
}

export async function claimName(token, namespace) {
  const r = await call(`${config.postboxUrl}/v1/claims/plan`, {
    method: "POST",
    headers: { authorization: `Bearer ${token}`, accept: "application/json", "content-type": "application/json" },
    body: JSON.stringify({ namespace }),
  }, "Your account");
  if (!r.ok) {
    const status = [400, 401, 402, 403, 409].includes(r.status) ? r.status : 502;
    fail(status, String(r.body?.detail || r.body?.error || "The name could not be registered").slice(0, 300), r.body?.error || "postbox_rejected");
  }
  return r.body;
}

// ---- internal API ------------------------------------------------------------------------------

function internal(method, path, body) {
  if (!config.postboxInternalUrl || !config.postboxServiceToken) fail(503, "Plan activation is not configured.", "not_configured");
  return call(`${config.postboxInternalUrl}${path}`, {
    method,
    headers: {
      authorization: `Bearer ${config.postboxServiceToken}`,
      accept: "application/json",
      ...(body ? { "content-type": "application/json" } : {}),
    },
    body: body ? JSON.stringify(body) : undefined,
  }, "Plan activation");
}

function expect(r, what) {
  if (!r.ok) fail(r.status === 404 ? 404 : r.status === 409 ? 409 : 502,
    `${what} failed: ${String(r.body?.error || r.status)}`, r.body?.error || "postbox_rejected");
  return r.body;
}

const seg = (value) => encodeURIComponent(value);

export async function putEntitlement(provider, environment, reference, body) {
  return expect(await internal("PUT", `/internal/v1/entitlements/${seg(provider)}/${seg(environment)}/${seg(reference)}`, body), "Entitlement update");
}
export async function createOrgDraft(body) {
  return expect(await internal("POST", "/internal/v1/org-drafts", body), "Organization draft");
}
export async function activateOrg(id, body) {
  return expect(await internal("PUT", `/internal/v1/orgs/${seg(id)}/activate`, body), "Organization activation");
}
export async function putOrgEntitlement(id, body) {
  return expect(await internal("PUT", `/internal/v1/orgs/${seg(id)}/entitlement`, body), "Organization entitlement");
}
export async function getOrg(id) {
  return expect(await internal("GET", `/internal/v1/orgs/${seg(id)}`), "Organization read");
}
