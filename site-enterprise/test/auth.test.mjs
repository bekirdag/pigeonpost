// Sign-in (PKCE), the callback, and step-up challenges.
import assert from "node:assert/strict";
import test from "node:test";
import { createHash } from "node:crypto";
import { load, reply, tick, jwt, plain } from "./helpers.mjs";

test("sign-in sends a PKCE S256 challenge for the stored verifier and one fixed redirect", async (t) => {
  const { window, nav } = load(t, "auth/callback.html", { url: "https://enterprise.pigeonpost.dev/auth/callback" });
  await window.PPE.signIn({ returnTo: "/app/rules" });
  const url = new URL(nav.at(-1));
  assert.equal(url.origin + url.pathname, "https://auth.pigeonpost.dev/realms/pigeonpost-prod/protocol/openid-connect/auth");
  const pending = JSON.parse(window.sessionStorage.getItem("ppe_pkce"));
  assert.equal(url.searchParams.get("client_id"), "pigeonpost-web");
  assert.equal(url.searchParams.get("redirect_uri"), "https://enterprise.pigeonpost.dev/auth/callback");
  assert.equal(url.searchParams.get("code_challenge_method"), "S256");
  assert.equal(url.searchParams.get("code_challenge"), createHash("sha256").update(pending.verifier).digest("base64url"));
  assert.equal(url.searchParams.get("state"), pending.state);
  assert.ok(url.searchParams.get("scope").split(" ").includes("openid"));
  assert.equal(pending.returnTo, "/app/rules");
  assert.equal(url.searchParams.get("acr_values"), null);
});

test("the callback refuses a foreign state and exchanges a matching code through the bridge", async (t) => {
  const pending = { state: "s-good", verifier: "v".repeat(64), nonce: "n", returnTo: "/app/agents" };
  const bad = load(t, "auth/callback.html", { url: "https://enterprise.pigeonpost.dev/auth/callback?code=c&state=s-evil", storage: { ppe_pkce: pending } });
  await tick();
  assert.equal(bad.calls.length, 0);
  assert.match(bad.document.querySelector("#callback").textContent, /couldn't|could not/i);

  const good = load(t, "auth/callback.html", { url: "https://enterprise.pigeonpost.dev/auth/callback?code=c1&state=s-good", storage: { ppe_pkce: pending },
    fetch: () => reply(200, { session: "acc", refresh: "ref" }) });
  await tick();
  const ex = good.calls[0];
  assert.equal(ex.url, "/api/v1/auth/exchange");
  assert.deepEqual(ex.body, { code: "c1", redirectUri: "https://enterprise.pigeonpost.dev/auth/callback", codeVerifier: pending.verifier });
  assert.equal(good.window.sessionStorage.getItem("ppe_session"), "acc");
  assert.equal(good.window.sessionStorage.getItem("ppe_pkce"), null, "the verifier is single use");
});

test("only same-origin paths are returned to", (t) => {
  const { window } = load(t, "auth/callback.html", { url: "https://enterprise.pigeonpost.dev/auth/callback" });
  const safe = window.PPE.safePath;
  assert.equal(safe("/app/people?org=org_1"), "/app/people?org=org_1");
  for (const evil of ["//evil.test/x", "https://evil.test", "/\\evil.test", "javascript:alert(1)", ""]) assert.equal(safe(evil), "/app");
});

test("a step-up challenge leaves for the realm with acr_values and max_age and remembers the action", async (t) => {
  const { window, nav } = load(t, "auth/callback.html", { url: "https://enterprise.pigeonpost.dev/auth/callback", session: jwt(),
    fetch: () => reply(401, { error: "step_up_required", acr_values: "mfa", max_age: 600 }) });
  const PPE = window.PPE;
  assert.deepEqual(plain(PPE.parseChallenge('Bearer error="insufficient_user_authentication", acr_values="gold urn:x", max_age=300')), { acr_values: "gold urn:x", max_age: 300 });
  assert.deepEqual(plain(PPE.parseChallenge(null, { acr_values: ["mfa"], max_age: 600 })), { acr_values: "mfa", max_age: 600 });
  await assert.rejects(PPE.postbox("/v1/orgs/o/agents/x/enrollments", { method: "POST", body: {}, resume: { name: "app.enroll", payload: { address: "/k/x" } } }), PPE.Redirecting);
  const url = new URL(nav.at(-1));
  assert.equal(url.searchParams.get("acr_values"), "mfa");
  assert.equal(url.searchParams.get("max_age"), "600");
  const resume = JSON.parse(window.sessionStorage.getItem("ppe_resume"));
  assert.equal(resume.name, "app.enroll");
  assert.deepEqual(plain(resume.payload), { address: "/k/x" });
  let ran = null;
  PPE.onResume("app.enroll", (p) => { ran = p; });
  assert.equal(await PPE.runResume(), true);
  assert.deepEqual(plain(ran), { address: "/k/x" });
  assert.equal(await PPE.runResume(), false, "a resumed action runs once");
});

test("a plain 401 refreshes once through the bridge and retries", async (t) => {
  let n = 0;
  const { window, calls } = load(t, "auth/callback.html", { url: "https://enterprise.pigeonpost.dev/auth/callback", session: "old", storage: { ppe_refresh: "r1" },
    fetch: (c) => c.url.endsWith("/v1/auth/refresh") ? reply(200, { session: "new", refresh: "r2" }) : (n++ ? reply(200, { ok: 1 }) : reply(401, { error: "unauthorized" })) });
  const r = await window.PPE.postbox("/v1/orgs");
  assert.deepEqual(plain(r), { ok: 1 });
  assert.equal(calls.at(-1).headers.authorization, "Bearer new");
});
