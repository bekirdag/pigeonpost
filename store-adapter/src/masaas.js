// Sign-in and postbox reads for the account page.
//
// The OIDC code exchange and refresh run here so a confidential client's secret (if any) never
// reaches the browser. Billing calls live in saas.js; plan activation in postbox.js.

import { config } from "./config.js";

async function doFetch(url, init) {
  const res = await fetch(url, { ...init, signal: AbortSignal.timeout(config.requestTimeoutMs) });
  const text = await res.text().catch(() => "");
  let payload = null;
  try { payload = text ? JSON.parse(text) : null; } catch { payload = null; }
  if (!res.ok) {
    const message = (payload && (payload.error_description || payload.message || payload.error)) || `Sign-in returned ${res.status}`;
    throw Object.assign(new Error(String(message).slice(0, 200)), { status: res.status });
  }
  return payload;
}

// ---- OIDC code exchange (adapter-side; browser never sees a client secret) ---------------------

export async function exchangeOidcCode(code, redirectUri, codeVerifier) {
  const url = `${config.oidc.issuer}/protocol/openid-connect/token`;
  const form = new URLSearchParams({
    grant_type: "authorization_code",
    code,
    redirect_uri: redirectUri,
    client_id: config.oidc.clientId,
  });
  if (config.oidc.clientSecret) form.set("client_secret", config.oidc.clientSecret);
  if (codeVerifier) form.set("code_verifier", codeVerifier);
  return doFetch(url, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded", accept: "application/json" },
    body: form.toString(),
  });
}

// Silently renew the access token from a stored refresh token ("remember me").
export async function refreshOidcToken(refreshToken) {
  const url = `${config.oidc.issuer}/protocol/openid-connect/token`;
  const form = new URLSearchParams({
    grant_type: "refresh_token",
    refresh_token: refreshToken,
    client_id: config.oidc.clientId,
  });
  if (config.oidc.clientSecret) form.set("client_secret", config.oidc.clientSecret);
  return doFetch(url, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded", accept: "application/json" },
    body: form.toString(),
  });
}

// ---- postbox reads (handle availability, and what an account owns) ----------------------------

/// Whether a handle can still be registered, according to the server that hands them out.
/// Returns `true`, `false`, or `null` for "could not tell" — and `null` must never read as free.
export async function handleAvailable(name, memberToken) {
  const url = `${config.postboxUrl}/v1/handles/${encodeURIComponent(name)}/availability`;
  try {
    const res = await fetch(url, {
      signal: AbortSignal.timeout(6000),
      headers: memberToken ? { authorization: `Bearer ${memberToken}` } : {},
    });
    if (!res.ok) return null;
    const body = await res.json();
    return typeof body?.available === "boolean" ? body.available : null;
  } catch {
    return null;
  }
}

/// The handles an account holds, whatever paid for them (App Store, Google Play, web plan).
export async function accountHandles(token) {
  const url = `${config.postboxUrl}/v1/me/handles?include_inactive=true`;
  const res = await fetch(url, {
    headers: { authorization: `Bearer ${token}`, accept: "application/json" },
    signal: AbortSignal.timeout(6000),
  });
  if (!res.ok) {
    throw Object.assign(new Error(res.status === 401 ? "session expired" : "Could not load your handles. Please try again."),
      { status: res.status === 401 ? 401 : 502 });
  }
  const body = await res.json();
  if (!Array.isArray(body?.handles)) throw Object.assign(new Error("Could not load your handles. Please try again."), { status: 502 });
  return body.handles;
}
