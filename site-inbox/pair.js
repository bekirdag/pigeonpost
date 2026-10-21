// Pigeonpost — approve a machine that asked to sign in as an organization agent (SDS §10,
// docs/orgs.md §8).
//
// The terminal shows a QR for /pair.html#ABCD-EFGH. This page reads the code from the fragment,
// removes it from the address bar at once, keeps it only in this tab's sessionStorage across the
// sign-in round trip, and sends it only in POST bodies. Approving needs a recent strong sign-in
// (step-up): on `401 step_up_required` the page asks the realm for exactly the `acr_values` and
// `max_age` the postbox named, then comes back here and resumes.
//
// The inbox's own session (ppi_token / ppi_refresh in localStorage) is reused when there is one, so
// someone already signed in to the inbox approves without signing in again.
(function () {
  "use strict";

  const cfg = window.PIGEONPOST_INBOX || {};
  const LS = window.localStorage;
  const SS = window.sessionStorage;
  const K = { token: "ppi_token", refresh: "ppi_refresh" };
  // This page's own PKCE/state/nonce slots, so a sign-in started in the inbox is never clobbered.
  const P = { verifier: "ppp_pkce", state: "ppp_state", nonce: "ppp_nonce", code: "ppp_code" };
  const CODE_RE = /^[A-Z0-9]{4}-?[A-Z0-9]{4}$/;
  const HOLD_MS = 1500;
  const $ = (id) => document.getElementById(id);

  const authUrl = (path) => cfg.oidc.issuer + "/protocol/openid-connect" + path;
  const redirectUri = () => window.location.origin + "/pair.html";

  function randomString(len) {
    const a = new Uint8Array(len);
    crypto.getRandomValues(a);
    return Array.from(a, (b) => ("0" + b.toString(16)).slice(-2)).join("");
  }

  async function sha256b64url(input) {
    const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(input));
    let str = "";
    new Uint8Array(digest).forEach((b) => (str += String.fromCharCode(b)));
    return btoa(str).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  }

  function claimsOf(jwt) {
    try {
      return JSON.parse(atob(jwt.split(".")[1].replace(/-/g, "+").replace(/_/g, "/")));
    } catch (_) {
      return {};
    }
  }

  // ---- the code ------------------------------------------------------------------------------

  /// Take the code out of the fragment (and the address bar) into this tab's session storage.
  function captureCode() {
    const raw = decodeURIComponent((window.location.hash || "").replace(/^#/, "")).trim().toUpperCase();
    if (raw) {
      history.replaceState(null, "", window.location.pathname);
      if (CODE_RE.test(raw)) SS.setItem(P.code, raw.length === 8 ? raw.slice(0, 4) + "-" + raw.slice(4) : raw);
    }
    return SS.getItem(P.code);
  }

  // ---- sign-in -------------------------------------------------------------------------------

  async function signIn(stepUp) {
    const verifier = randomString(48);
    const state = randomString(12);
    const nonce = randomString(12);
    LS.setItem(P.verifier, verifier);
    LS.setItem(P.state, state);
    LS.setItem(P.nonce, nonce);
    let url = authUrl("/auth")
      + `?client_id=${encodeURIComponent(cfg.oidc.clientId)}`
      + `&response_type=code&scope=${encodeURIComponent((cfg.oidc.scope || "openid") + " offline_access")}`
      + `&redirect_uri=${encodeURIComponent(redirectUri())}`
      + `&code_challenge=${await sha256b64url(verifier)}&code_challenge_method=S256`
      + `&state=${state}&nonce=${nonce}`;
    if (stepUp) {
      url += `&acr_values=${encodeURIComponent(stepUp.acr_values)}&max_age=${encodeURIComponent(stepUp.max_age)}`;
    }
    // A seam for the tests; in a browser this is plain navigation.
    (window.PigeonpostPairNavigate || ((u) => window.location.assign(u)))(url);
  }

  async function completeSignIn() {
    const params = new URLSearchParams(window.location.search);
    if (!params.get("code") && !params.get("error")) return true;
    history.replaceState(null, "", window.location.pathname);
    const verifier = LS.getItem(P.verifier);
    const state = LS.getItem(P.state);
    const nonce = LS.getItem(P.nonce);
    [P.verifier, P.state, P.nonce].forEach((k) => LS.removeItem(k));
    if (params.get("error")) {
      show("error", "Sign-in did not complete: " + params.get("error"));
      return false;
    }
    if (!state || params.get("state") !== state) {
      show("error", "Sign-in could not be verified. Open the link from the terminal again.");
      return false;
    }
    try {
      const res = await fetch(authUrl("/token"), {
        method: "POST",
        headers: { "content-type": "application/x-www-form-urlencoded" },
        body: new URLSearchParams({
          grant_type: "authorization_code",
          client_id: cfg.oidc.clientId,
          code: params.get("code"),
          redirect_uri: redirectUri(),
          code_verifier: verifier || "",
        }),
      });
      const body = await res.json().catch(() => ({}));
      if (!res.ok || !body.access_token) {
        show("error", "Could not complete sign-in" + (body.error_description ? ": " + body.error_description : "."));
        return false;
      }
      if (body.id_token && nonce && claimsOf(body.id_token).nonce !== nonce) {
        show("error", "Sign-in could not be verified. Open the link from the terminal again.");
        return false;
      }
      LS.setItem(K.token, body.access_token);
      if (body.refresh_token) LS.setItem(K.refresh, body.refresh_token);
      return true;
    } catch (_) {
      show("error", "Could not reach the sign-in service.");
      return false;
    }
  }

  async function renew() {
    const refresh = LS.getItem(K.refresh);
    if (!refresh) return false;
    try {
      const res = await fetch(authUrl("/token"), {
        method: "POST",
        headers: { "content-type": "application/x-www-form-urlencoded" },
        body: new URLSearchParams({ grant_type: "refresh_token", client_id: cfg.oidc.clientId, refresh_token: refresh }),
      });
      const body = await res.json().catch(() => ({}));
      if (res.ok && body.access_token) {
        LS.setItem(K.token, body.access_token);
        if (body.refresh_token) LS.setItem(K.refresh, body.refresh_token);
        return true;
      }
    } catch (_) { /* fall through */ }
    return false;
  }

  // ---- postbox -------------------------------------------------------------------------------

  /// Parse an RFC 9470 challenge: `acr_values="a b", max_age=600`.
  function parseChallenge(header) {
    if (!header) return {};
    const acr = /acr_values="([^"]*)"/.exec(header);
    const age = /max_age=(\d+)/.exec(header);
    return { acr_values: acr ? acr[1] : undefined, max_age: age ? Number(age[1]) : undefined };
  }

  async function post(path, body, retried) {
    const token = LS.getItem(K.token);
    const res = await fetch(cfg.postbox + path, {
      method: "POST",
      headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
      body: JSON.stringify(body),
    });
    const json = await res.json().catch(() => ({}));
    if (res.status === 401 && json.error !== "step_up_required" && !retried && (await renew())) {
      return post(path, body, true);
    }
    if (!res.ok) {
      const header = res.headers && res.headers.get ? res.headers.get("www-authenticate") : null;
      const challenge = parseChallenge(header);
      throw Object.assign(new Error(json.detail || json.error || `postbox ${res.status}`), {
        status: res.status,
        code: json.error,
        acr_values: json.acr_values || challenge.acr_values,
        max_age: json.max_age || challenge.max_age || 600,
        body: json,
      });
    }
    return json;
  }

  // ---- rendering -----------------------------------------------------------------------------

  const STATES = ["loading", "signin", "review", "done", "error"];

  function show(state, message) {
    for (const s of STATES) {
      const el = $("pair-" + s);
      if (el) el.hidden = s !== state;
    }
    if (message !== undefined) {
      const target = $(state === "done" ? "pair-done-text" : state === "error" ? "pair-error-text" : "pair-status");
      if (target) target.textContent = message;
    }
  }

  function text(id, value) {
    const el = $(id);
    if (el) el.textContent = value == null || value === "" ? "—" : String(value);
  }

  let countdown = null;
  function startCountdown(expiresAt) {
    clearInterval(countdown);
    const tick = () => {
      const left = Math.max(0, Math.floor(expiresAt - Date.now() / 1000));
      text("pair-expires", left > 0 ? `${Math.floor(left / 60)}:${String(left % 60).padStart(2, "0")}` : "expired");
      if (left <= 0) {
        clearInterval(countdown);
        show("error", "This request has expired. Run the command on the machine again for a new code.");
      }
    };
    tick();
    countdown = setInterval(tick, 1000);
  }

  let pairing = null;

  function render(p) {
    pairing = p;
    text("pair-org", p.org && p.org.name);
    text("pair-handle", p.handle);
    text("pair-label", p.label);
    text("pair-confirmation", p.confirmation);
    text("pair-country", p.ip_country || "unknown");
    text("pair-capabilities", (p.capabilities || []).join(", "));
    const approve = $("pair-approve");
    const why = $("pair-reason");
    if (p.can_approve === false) {
      approve.disabled = true;
      approve.setAttribute("aria-disabled", "true");
      why.hidden = false;
      why.textContent = reasonText(p.reason);
    } else {
      approve.disabled = false;
      approve.removeAttribute("aria-disabled");
      why.hidden = true;
    }
    if (p.expires_at) startCountdown(p.expires_at);
    show("review");
  }

  function reasonText(reason) {
    switch (reason) {
      case "delegation_required":
        return "You are not allowed to bring this agent onto a machine. Ask an agent admin to add you as one of its operators.";
      case "org_inactive":
        return "This organization is not active, so new machines cannot join it.";
      case "not_org_member":
        return "You are not a member of this organization.";
      default:
        return "You cannot approve this request" + (reason ? ` (${reason})` : ".");
    }
  }

  // ---- hold to approve ---------------------------------------------------------------------

  let holdTimer = null;
  let holdTick = null;
  let holdStart = 0;

  function setProgress(fraction) {
    const ring = $("pair-ring");
    if (ring) ring.style.setProperty("--progress", String(Math.min(1, Math.max(0, fraction))));
    const btn = $("pair-approve");
    if (btn) btn.setAttribute("data-progress", String(Math.round(Math.min(1, fraction) * 100)));
  }

  function holdBegin(event) {
    const btn = $("pair-approve");
    if (!pairing || btn.disabled || holdTimer) return;
    if (event && event.preventDefault) event.preventDefault();
    holdStart = Date.now();
    btn.classList.add("holding");
    setProgress(0);
    holdTick = setInterval(() => setProgress((Date.now() - holdStart) / HOLD_MS), 50);
    holdTimer = setTimeout(() => {
      holdEnd(true);
      approve();
    }, HOLD_MS);
  }

  function holdEnd(completed) {
    clearTimeout(holdTimer);
    clearInterval(holdTick);
    holdTimer = null;
    holdTick = null;
    const btn = $("pair-approve");
    if (btn) btn.classList.remove("holding");
    if (completed !== true) {
      setProgress(0);
      const hint = $("pair-status");
      if (hint && holdStart && Date.now() - holdStart < HOLD_MS) hint.textContent = "Keep holding to approve.";
    }
  }

  async function approve() {
    const code = SS.getItem(P.code);
    setProgress(1);
    text("pair-status", "Approving…");
    try {
      await post(`/v1/device-pairings/${encodeURIComponent(pairing.pairing_id)}/approve`, { user_code: code });
      SS.removeItem(P.code);
      clearInterval(countdown);
      show("done", `Approved. ${pairing.handle} is signing in on that machine; you can close this page.`);
    } catch (e) {
      handleError(e, true);
    }
  }

  async function deny() {
    const code = SS.getItem(P.code);
    try {
      await post(`/v1/device-pairings/${encodeURIComponent(pairing.pairing_id)}/deny`, { user_code: code });
    } catch (_) { /* a refused deny still ends the request here */ }
    SS.removeItem(P.code);
    clearInterval(countdown);
    show("done", "Declined. The machine was not signed in.");
  }

  function handleError(e, approving) {
    if (e.code === "step_up_required") {
      // Remember that we were approving, so the page asks again (one hold) after the sign-in.
      SS.setItem("ppp_resume", approving ? "approve" : "");
      show("signin");
      text("pair-signin-text", "Approving a machine needs a fresh, strong sign-in.");
      return signIn({ acr_values: e.acr_values || "", max_age: e.max_age || 600 });
    }
    if (e.status === 401) {
      show("signin");
      return;
    }
    if (e.code === "pairing_expired") return show("error", "This request has expired. Run the command on the machine again for a new code.");
    if (e.code === "pairing_not_found") return show("error", "This code is not valid. Check it on the terminal, or run the command again.");
    if (e.code === "step_up_unavailable") return show("error", "Strong sign-in is not available on this server yet, so machines cannot be approved here.");
    if (e.code === "rate_limited") return show("error", "Too many attempts. Wait a minute and try again.");
    if (e.status === 403) return show("error", reasonText(e.code));
    show("error", e.message || "Something went wrong.");
  }

  // ---- boot ----------------------------------------------------------------------------------

  async function start() {
    const code = captureCode();
    $("pair-approve").addEventListener("pointerdown", holdBegin);
    for (const ev of ["pointerup", "pointerleave", "pointercancel"]) $("pair-approve").addEventListener(ev, holdEnd);
    $("pair-approve").addEventListener("keydown", (e) => {
      if ((e.key === " " || e.key === "Enter") && !e.repeat) holdBegin(e);
    });
    $("pair-approve").addEventListener("keyup", (e) => {
      if (e.key === " " || e.key === "Enter") holdEnd();
    });
    $("pair-approve").addEventListener("click", (e) => e.preventDefault());
    $("pair-deny").addEventListener("click", deny);
    $("pair-signin-btn").addEventListener("click", () => signIn(null));

    if (!(await completeSignIn())) return;
    if (!code) return show("error", "No code was found. Scan the QR on the terminal again, or open the full link it shows.");
    text("pair-code", code);
    if (!LS.getItem(K.token)) return show("signin");
    show("loading");
    try {
      render(await post("/v1/device-pairings/resolve", { user_code: code }));
      if (SS.getItem("ppp_resume") === "approve") {
        SS.removeItem("ppp_resume");
        text("pair-status", "Signed in again. Hold the button once more to approve.");
      }
    } catch (e) {
      handleError(e, false);
    }
  }

  window.PigeonpostPair = { start, parseChallenge, HOLD_MS };
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start);
  else start();
})();
