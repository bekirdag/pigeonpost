// Pigeonpost for teams — shared core: sign-in (Authorization Code + PKCE), step-up, API calls,
// and the few UI primitives every page uses (toast, one sheet at a time, element builder).
//
// Rules this file keeps for every page:
//  - Nothing from a server or a URL is ever written as HTML. Text goes through textContent.
//  - Tokens live in sessionStorage only (this tab). The invitation token never leaves the tab
//    except in a POST body.
//  - A sign-in redirect is only ever followed back to a same-origin path.
(function () {
  "use strict";
  const cfg = window.PPE_CONFIG || {};
  const PPE = (window.PPE = window.PPE || {});

  // ---- storage ---------------------------------------------------------------------------------

  const store = {
    get(key) { try { return window.sessionStorage.getItem(key); } catch { return null; } },
    set(key, value) { try { window.sessionStorage.setItem(key, value); } catch { /* blocked */ } },
    del(key) { try { window.sessionStorage.removeItem(key); } catch { /* blocked */ } },
    json(key) { try { return JSON.parse(store.get(key) || "null"); } catch { return null; } },
  };
  PPE.store = store;

  // ---- encoding --------------------------------------------------------------------------------

  function b64url(bytes) {
    let s = "";
    for (const b of bytes) s += String.fromCharCode(b);
    return window.btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  }
  function random(n = 32) {
    const bytes = new Uint8Array(n);
    window.crypto.getRandomValues(bytes);
    return b64url(bytes);
  }
  async function challengeFor(verifier) {
    const digest = await window.crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier));
    return b64url(new Uint8Array(digest));
  }
  PPE.b64url = b64url;
  PPE.random = random;
  PPE.challengeFor = challengeFor;

  /// The claims of a JWT, for display only. Nothing is authorized on these; the postbox validates
  /// the token itself.
  PPE.claims = function claims(jwt) {
    try {
      const part = String(jwt || "").split(".")[1] || "";
      const text = window.atob(part.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((part.length + 3) % 4));
      return JSON.parse(decodeURIComponent(Array.from(text, (c) => "%" + c.charCodeAt(0).toString(16).padStart(2, "0")).join("")));
    } catch { return {}; }
  };

  // ---- session ---------------------------------------------------------------------------------

  PPE.token = () => store.get("ppe_session");
  PPE.signedIn = () => Boolean(PPE.token());
  PPE.setSession = function (body) {
    store.set("ppe_session", body.session);
    if (body.refresh) store.set("ppe_refresh", body.refresh);
  };
  PPE.signOutLocal = function () {
    store.del("ppe_session");
    store.del("ppe_refresh");
  };
  PPE.whoami = function () {
    const c = PPE.claims(PPE.token());
    return { name: c.name || c.preferred_username || "", username: c.preferred_username || "", sub: c.sub || "" };
  };

  /// Only a same-origin absolute path may be returned to after sign-in.
  PPE.safePath = function (path) {
    const p = String(path || "");
    return /^\/(?![/\\])[^\s]*$/.test(p) ? p : "/app";
  };

  /// Build the authorize URL. Pure apart from what it is given, so it can be tested.
  PPE.authorizeUrl = function ({ state, nonce, challenge, acr_values, max_age, prompt, origin }) {
    const o = cfg.oidc || {};
    const q = new URLSearchParams({
      client_id: o.clientId,
      response_type: "code",
      scope: o.scope,
      redirect_uri: (origin || window.location.origin) + (o.redirectPath || "/auth/callback"),
      state,
      nonce,
      code_challenge: challenge,
      code_challenge_method: "S256",
    });
    if (acr_values) q.set("acr_values", acr_values);
    if (max_age !== undefined && max_age !== null) q.set("max_age", String(max_age));
    if (prompt) q.set("prompt", prompt);
    return `${o.issuer}/protocol/openid-connect/auth?${q.toString()}`;
  };

  /// Leave for the realm. `returnTo` is where the callback page sends the person afterwards.
  PPE.signIn = async function ({ returnTo, acr_values, max_age, prompt } = {}) {
    const verifier = random(48);
    const state = random(24);
    const nonce = random(24);
    store.set("ppe_pkce", JSON.stringify({
      state, verifier, nonce,
      returnTo: PPE.safePath(returnTo || window.location.pathname + window.location.search),
      stepUp: Boolean(acr_values),
    }));
    const url = PPE.authorizeUrl({ state, nonce, challenge: await challengeFor(verifier), acr_values, max_age, prompt });
    PPE.navigate(url);
  };

  /// Switch account: forget this tab's session and make the realm ask again.
  PPE.switchAccount = function (returnTo) {
    PPE.signOutLocal();
    return PPE.signIn({ returnTo, prompt: "login" });
  };

  PPE.signOut = function () {
    PPE.signOutLocal();
    const o = cfg.oidc || {};
    const q = new URLSearchParams({ client_id: o.clientId, post_logout_redirect_uri: window.location.origin + "/" });
    PPE.navigate(`${o.issuer}/protocol/openid-connect/logout?${q.toString()}`);
  };

  /// The callback page: check state, exchange the code through the bridge, go back.
  PPE.completeSignIn = async function () {
    const params = new URLSearchParams(window.location.search);
    const pending = store.json("ppe_pkce");
    store.del("ppe_pkce");
    window.history.replaceState({}, "", window.location.pathname);
    if (params.get("error")) throw new Error(params.get("error_description") || "Sign-in was cancelled.");
    const code = params.get("code");
    if (!pending || !code || params.get("state") !== pending.state) throw new Error("This sign-in could not be verified. Start again.");
    const res = await window.fetch(`${cfg.apiBase || "/api"}/v1/auth/exchange`, {
      method: "POST",
      headers: { "content-type": "application/json", accept: "application/json" },
      body: JSON.stringify({ code, redirectUri: window.location.origin + (cfg.oidc.redirectPath || "/auth/callback"), codeVerifier: pending.verifier }),
    });
    const body = await res.json().catch(() => ({}));
    if (!res.ok || !body.session) throw new Error(body.error || "Sign-in failed.");
    PPE.setSession(body);
    return PPE.safePath(pending.returnTo);
  };

  let refreshing = null;
  async function refresh() {
    const token = store.get("ppe_refresh");
    if (!token) return false;
    if (!refreshing) {
      refreshing = (async () => {
        try {
          const res = await window.fetch(`${cfg.apiBase || "/api"}/v1/auth/refresh`, {
            method: "POST",
            headers: { "content-type": "application/json", accept: "application/json" },
            body: JSON.stringify({ refresh: token }),
          });
          if (!res.ok) { if (res.status === 401) store.del("ppe_refresh"); return false; }
          const body = await res.json();
          if (!body.session) return false;
          PPE.setSession(body);
          return true;
        } catch { return false; } finally { refreshing = null; }
      })();
    }
    return refreshing;
  }

  // ---- step-up (SDS §3.4) ----------------------------------------------------------------------

  /// `acr_values` and `max_age` from an RFC 9470 challenge header, or from the JSON body (CORS
  /// does not expose WWW-Authenticate to this page unless the server lists it).
  PPE.parseChallenge = function (header, body) {
    const out = { acr_values: null, max_age: 600 };
    const h = String(header || "");
    const acr = /acr_values="([^"]*)"/.exec(h);
    const age = /max_age=(\d+)/.exec(h);
    if (acr) out.acr_values = acr[1];
    if (age) out.max_age = Number(age[1]);
    if (body && typeof body === "object") {
      if (!out.acr_values && body.acr_values) out.acr_values = Array.isArray(body.acr_values) ? body.acr_values.join(" ") : String(body.acr_values);
      if (!age && Number.isInteger(body.max_age)) out.max_age = body.max_age;
    }
    return out;
  };

  const resumers = {};
  /// Register what to redo after a step-up round trip, by name.
  PPE.onResume = function (name, fn) { resumers[name] = fn; };
  /// Run the action that was waiting for a step-up, once, if it belongs to this page.
  PPE.runResume = async function () {
    const pending = store.json("ppe_resume");
    if (!pending || !resumers[pending.name]) return false;
    store.del("ppe_resume");
    if (Date.now() - (pending.at || 0) > 15 * 60 * 1000) return false;
    await resumers[pending.name](pending.payload);
    return true;
  };

  PPE.stepUp = async function (challenge, resume) {
    if (resume) store.set("ppe_resume", JSON.stringify({ ...resume, at: Date.now() }));
    await PPE.signIn({ acr_values: challenge.acr_values || undefined, max_age: challenge.max_age ?? 600 });
  };

  // ---- HTTP ------------------------------------------------------------------------------------

  class ApiError extends Error {
    constructor(status, body) {
      super((body && (body.detail || body.message || body.error)) || `Request failed (${status})`);
      this.status = status;
      this.body = body || {};
      this.code = (body && (body.error || body.code)) || null;
    }
  }
  PPE.ApiError = ApiError;
  /// Thrown after a redirect has started, so callers stop quietly.
  PPE.Redirecting = class Redirecting extends Error {};

  /// One request. `base` is "postbox" or "bridge". A 401 refreshes once; `step_up_required`
  /// leaves for the realm and comes back to `resume`; a 401 that survives the refresh signs in.
  PPE.api = async function (base, path, { method = "GET", body, auth = true, resume, raw = false } = {}, retried = false) {
    const root = base === "bridge" ? (cfg.apiBase || "/api") : cfg.postbox;
    const headers = { accept: "application/json" };
    if (body !== undefined) headers["content-type"] = "application/json";
    if (auth && PPE.token()) headers.authorization = `Bearer ${PPE.token()}`;
    const res = await window.fetch(root + path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) });
    if (raw && res.ok) return res;
    const text = await res.text();
    let json = null;
    try { json = text ? JSON.parse(text) : {}; } catch { json = { error: "bad_response" }; }
    if (res.ok) return json;
    if (res.status === 401 && auth) {
      const code = json && (json.error || json.code);
      if (code === "step_up_required") {
        await PPE.stepUp(PPE.parseChallenge(res.headers.get("www-authenticate"), json), resume);
        throw new PPE.Redirecting("step-up");
      }
      if (!retried && (await refresh())) return PPE.api(base, path, { method, body, auth, resume, raw }, true);
      PPE.signOutLocal();
      await PPE.signIn({});
      throw new PPE.Redirecting("sign-in");
    }
    throw new ApiError(res.status, json);
  };
  PPE.postbox = (path, opts) => PPE.api("postbox", path, opts);
  PPE.bridge = (path, opts) => PPE.api("bridge", path, opts);

  PPE.navigate = function (url) { window.location.assign(url); };

  // ---- UI --------------------------------------------------------------------------------------

  PPE.$ = (sel, root = document) => root.querySelector(sel);
  PPE.$$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));

  /// Build an element. Strings and numbers become text nodes; attributes starting with "on" are
  /// listeners. Never parses HTML.
  PPE.el = function el(tag, attrs, ...children) {
    const node = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v === undefined || v === null || v === false) continue;
      if (k.startsWith("on") && typeof v === "function") node.addEventListener(k.slice(2).toLowerCase(), v);
      else if (k === "class") node.className = v;
      else if (k === "text") node.textContent = v;
      else if (v === true) node.setAttribute(k, "");
      else node.setAttribute(k, String(v));
    }
    for (const child of children.flat(Infinity)) {
      if (child === undefined || child === null || child === false) continue;
      node.append(child instanceof window.Node ? child : document.createTextNode(String(child)));
    }
    return node;
  };

  PPE.toast = function (text, kind = "info") {
    let region = document.getElementById("ppe-toasts");
    if (!region) {
      region = PPE.el("div", { id: "ppe-toasts", class: "toasts", role: "status", "aria-live": "polite" });
      document.body.append(region);
    }
    const t = PPE.el("div", { class: `toast toast-${kind}` }, text);
    region.append(t);
    window.setTimeout(() => t.remove(), kind === "error" ? 8000 : 4500);
  };

  /// One sheet at a time, sliding in from the right. Returns the body element to fill.
  /// `onClose` runs once when this sheet goes away (close button, Escape, scrim, or another sheet),
  /// so a sheet holding a one-time secret can drop it.
  let lastFocus = null;
  let onSheetClose = null;
  PPE.openSheet = function (title, build, { onClose } = {}) {
    PPE.closeSheet();
    lastFocus = document.activeElement;
    onSheetClose = typeof onClose === "function" ? onClose : null;
    const close = PPE.el("button", { class: "icon-btn", type: "button", "aria-label": "Close", onclick: () => PPE.closeSheet() }, "×");
    const heading = PPE.el("h2", { id: "ppe-sheet-title" }, title);
    const body = PPE.el("div", { class: "sheet-body" });
    const sheet = PPE.el("aside", { class: "sheet", role: "dialog", "aria-modal": "true", "aria-labelledby": "ppe-sheet-title", tabindex: "-1" },
      PPE.el("header", { class: "sheet-head" }, heading, close), body);
    const scrim = PPE.el("div", { class: "scrim", id: "ppe-scrim", onclick: () => PPE.closeSheet() });
    document.body.append(scrim, sheet);
    sheet.addEventListener("keydown", (e) => { if (e.key === "Escape") PPE.closeSheet(); });
    if (build) build(body);
    const first = sheet.querySelector("input, select, textarea, button:not(.icon-btn)");
    (first || sheet).focus();
    return body;
  };
  PPE.closeSheet = function () {
    const done = onSheetClose;
    onSheetClose = null;
    PPE.$$(".sheet, #ppe-scrim").forEach((n) => n.remove());
    if (done) { try { done(); } catch { /* never block closing */ } }
    if (lastFocus && document.contains(lastFocus)) lastFocus.focus();
    lastFocus = null;
  };

  PPE.field = function (label, input, hint) {
    const id = input.id || `f-${random(6)}`;
    input.id = id;
    return PPE.el("div", { class: "field" }, PPE.el("label", { for: id }, label), input, hint ? PPE.el("p", { class: "hint" }, hint) : null);
  };

  PPE.money = function (amount, currency) {
    try { return new Intl.NumberFormat("en", { style: "currency", currency, maximumFractionDigits: Number(amount) % 1 ? 2 : 0 }).format(Number(amount)); }
    catch { return `${amount} ${currency}`; }
  };
  PPE.date = function (unix) {
    if (!unix) return "—";
    try { return new Date(unix * 1000).toLocaleDateString("en", { year: "numeric", month: "short", day: "numeric" }); } catch { return "—"; }
  };
  PPE.ago = function (unix, now = Date.now() / 1000) {
    if (!unix) return "Never";
    const s = Math.max(0, now - unix);
    if (s < 90) return "Just now";
    if (s < 3600 * 1.5) return `${Math.round(s / 60)} min ago`;
    if (s < 86400 * 1.5) return `${Math.round(s / 3600)} h ago`;
    return `${Math.round(s / 86400)} days ago`;
  };

  /// Copy text, with a toast either way.
  PPE.copy = async function (text) {
    try { await window.navigator.clipboard.writeText(text); PPE.toast("Copied"); }
    catch { PPE.toast("Copy it from the box above", "error"); }
  };

  /// Explain an error to a person.
  PPE.explain = function (e) {
    if (e instanceof PPE.Redirecting) return null;
    const known = {
      seat_limit: "All seats are in use. Add seats in Billing, or revoke an invitation.",
      agent_limit: "You have used every agent address in this plan.",
      org_read_only: "The organization is read-only until the payment is sorted out.",
      org_inactive: "This organization is not active.",
      handle_taken: "That address is taken.",
      domain_not_verified: "That address is not at a verified domain.",
      human_required: "Only a person signed in can do this.",
      step_up_unavailable: "Strong sign-in is not available yet, so this action is switched off.",
      forbidden: "Your role does not allow this.",
      not_org_member: "You are not a member of this organization.",
      rate_limited: "Too many attempts. Wait a little and try again.",
      feature_disabled: "This is not enabled for your account yet.",
      webhook_limit: "An organization can have at most 10 webhooks. Delete one first.",
      invalid_webhook_url: "Use a public https:// address on port 443 or 8443.",
    };
    return (e && e.code && known[e.code]) || (e && e.message) || "Something went wrong.";
  };
  PPE.fail = function (e) {
    const text = PPE.explain(e);
    if (text) PPE.toast(text, "error");
  };

  /// Header sign-in state, shared by every page that has `#ppe-account`.
  PPE.renderAccount = function () {
    const slot = document.getElementById("ppe-account");
    if (!slot) return;
    slot.replaceChildren();
    if (PPE.signedIn()) {
      const who = PPE.whoami();
      slot.append(PPE.el("span", { class: "who" }, who.username || "Signed in"),
        PPE.el("button", { class: "btn btn-quiet", type: "button", onclick: () => PPE.signOut() }, "Sign out"));
    } else {
      slot.append(PPE.el("button", { class: "btn btn-quiet", type: "button", onclick: () => PPE.signIn({ returnTo: "/app" }) }, "Sign in"));
    }
  };
})();
