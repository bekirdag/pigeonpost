import assert from "node:assert/strict";
import { once } from "node:events";
import http from "node:http";
import { spawn } from "node:child_process";
import test from "node:test";

const subscriptionId = "11111111-1111-4111-8111-111111111111";
const paymentId = "22222222-2222-4222-8222-222222222222";
const plan = "handle-yearly-annual-usd";
const reference = "pigeonpost:handle:example";
const future = () => new Date(Date.now() + 365 * 86400000).toISOString();
const send = (res, status, body) => { res.writeHead(status, { "content-type": "application/json" }); res.end(JSON.stringify(body)); };

test("checkout follows the subscription payment contract over HTTP", async (t) => {
  let subs, payments, available, completionStatus, calls, grants;
  const reset = () => { subs = []; payments = []; available = true; completionStatus = 200; calls = []; grants = []; };
  reset();
  const upstream = http.createServer(async (req, res) => {
    const url = new URL(req.url, "http://localhost");
    const chunks = [];
    for await (const chunk of req) chunks.push(chunk);
    const body = chunks.length ? JSON.parse(Buffer.concat(chunks)) : undefined;
    calls.push({ path: url.pathname, method: req.method, body, key: req.headers["idempotency-key"] });
    if (url.pathname === "/v1/me/handles") return send(res, 200, { account: "account-owner", handles: [] });
    if (url.pathname.endsWith("/availability")) return send(res, 200, { available });
    if (url.pathname === "/v1/namespaces") { grants.push(body); return send(res, 200, { ok: true }); }
    if (req.headers.authorization !== "Bearer member-token") return send(res, 401, { message: "Unauthorized" });
    if (url.pathname === "/v1/billing/profiles/saved-profile" && req.method === "PATCH") {
      assert.equal(req.headers["x-product-slug"], "pigeonpost");
      assert.ok(req.headers["idempotency-key"]);
      return send(res, 200, { id: "saved-profile", ...body });
    }
    if (url.pathname === "/v1/subscriptions" && req.method === "GET") return send(res, 200, { data: subs, next_cursor: null });
    if (url.pathname === "/v1/billing/payments" && req.method === "GET") return send(res, 200, { data: payments, next_cursor: null });
    if (url.pathname === `/v1/billing/payments/${paymentId}`) return send(res, 200, payments[0]);
    if (url.pathname === "/v1/subscriptions" && req.method === "POST") {
      // Mirror the strict DTO, so the originally reported request fails this fixture.
      const unknown = Object.keys(body).find((key) => !["plan_slug", "external_reference", "payment_callback_url"].includes(key));
      if (unknown) return send(res, 400, { message: `property ${unknown} should not exist` });
      assert.ok(req.headers["idempotency-key"]);
      subs = [{ id: subscriptionId, plan_slug: body.plan_slug, external_reference: body.external_reference, status: "past_due", current_period_end: null }];
      payments = [{ id: paymentId, status: "pending", metadata: { commercial_metadata: { subscription_id: subscriptionId },
        provider_response: { checkout_session_id: "test-session", checkout_redirect_url: "https://bank.example.test/checkout" } } }];
      return send(res, 201, { status: "payment_action_required", subscription_id: subscriptionId, payment_id: paymentId,
        action: { session_id: "test-session", redirect_url: "https://bank.example.test/checkout" } });
    }
    if (url.pathname.endsWith("/payment-checkout/complete")) {
      assert.deepEqual(body, { payment_id: paymentId, session_id: "test-session" });
      if (completionStatus !== 200) return send(res, completionStatus, { message: "Payment is still under review" });
      payments[0].status = "succeeded";
      subs[0] = { ...subs[0], status: "active", current_period_end: future() };
      return send(res, 200, subs[0]);
    }
    return send(res, 404, { message: `Unexpected upstream route: ${req.method} ${url.pathname}` });
  });
  upstream.listen(0, "127.0.0.1");
  await once(upstream, "listening");
  t.after(() => upstream.close());
  const upstreamUrl = `http://127.0.0.1:${upstream.address().port}`;
  const child = spawn(process.execPath, [new URL("../src/server.js", import.meta.url).pathname], {
    env: { ...process.env, PORT: "0", MASAAS_SAAS_API_URL: upstreamUrl, PIGEONPOST_POSTBOX_URL: upstreamUrl,
      PIGEONPOST_NAMESPACE_GRANT: "fictional-grant", STORE_ALLOWED_ORIGINS: "https://pigeonpost.dev" },
    stdio: ["ignore", "pipe", "pipe"],
  });
  t.after(() => child.kill());
  const base = await new Promise((resolve, reject) => {
    let output = "";
    child.stdout.on("data", (chunk) => { output += chunk; const match = /adapter on :(\d+)/.exec(output); if (match) resolve(`http://127.0.0.1:${match[1]}`); });
    child.once("error", reject);
    child.once("exit", (code) => reject(new Error(`adapter exited ${code}`)));
  });
  const post = async (path, body, token = "member-token") => {
    const response = await fetch(base + path, { method: "POST", headers: { "content-type": "application/json", authorization: `Bearer ${token}` }, body: JSON.stringify(body) });
    return { status: response.status, body: await response.json() };
  };
  const buy = () => post("/v1/checkout", { handle: "example", operationId: "a".repeat(32) });

  await t.test("billing edits forward PATCH with member auth, TCKN and district", async () => {
    reset();
    const body = { account_type: "individual", identity_number: "10000000000", address: {
      line1: "1 Example Street", line2: "Unit 2", city: "Example City", state: "Example District", postal_code: "12345", country: "TR",
    } };
    const options = { method: "PATCH", headers: { "content-type": "application/json", origin: "https://pigeonpost.dev" }, body: JSON.stringify(body) };
    const denied = await fetch(base + "/api/v1/billing/profiles/saved-profile", options);
    assert.equal(denied.status, 401);
    assert.equal(calls.length, 0);
    const response = await fetch(base + "/api/v1/billing/profiles/saved-profile", {
      ...options, headers: { ...options.headers, authorization: "Bearer member-token" },
    });
    assert.equal(response.status, 200);
    assert.deepEqual(await response.json(), { id: "saved-profile", ...body });
    assert.equal(calls.length, 1);
    assert.equal(calls[0].method, "PATCH");
    assert.equal(calls[0].path, "/v1/billing/profiles/saved-profile");
    assert.deepEqual(calls[0].body, body);
    const preflight = await fetch(base + "/api/v1/billing/profiles/saved-profile", {
      method: "OPTIONS", headers: { origin: "https://pigeonpost.dev", "access-control-request-method": "PATCH" },
    });
    assert.equal(preflight.status, 204);
    assert.ok(preflight.headers.get("access-control-allow-methods").split(", ").includes("PATCH"));
  });

  await t.test("Get it starts hosted payment without the rejected metadata or another card setup", async () => {
    reset();
    const response = await buy();
    assert.equal(response.status, 200);
    assert.equal(response.body.checkoutUrl, "https://bank.example.test/checkout");
    assert.equal(response.body.status, "payment_action_required");
    const create = calls.find((c) => c.path === "/v1/subscriptions" && c.method === "POST");
    assert.deepEqual(create.body, { plan_slug: plan, external_reference: reference, payment_callback_url: "https://pigeonpost.dev/api/v1/checkout/callback" });
    assert.equal(calls.some((c) => c.path.includes("setup-session")), false);
    assert.equal(grants.length, 0);
  });

  await t.test("another click resumes the pending bank session without another subscription or charge", async () => {
    const response = await buy();
    assert.equal(response.status, 200);
    assert.equal(response.body.paymentId, paymentId);
    assert.equal(calls.filter((c) => c.path === "/v1/subscriptions" && c.method === "POST").length, 1);
    assert.equal(calls.some((c) => c.path.endsWith("retry-payment")), false);
  });

  await t.test("unpaid subscriptions cannot grant a handle", async () => {
    assert.equal((await post("/v1/handles/claim", { handle: "example" })).status, 402);
    assert.equal(grants.length, 0);
  });

  await t.test("bank POST becomes a safe GET and does not claim payment by itself", async () => {
    const response = await fetch(`${base}/v1/checkout/callback?subscription_id=${subscriptionId}&payment_id=${paymentId}&token=untrusted`, {
      method: "POST", body: "token=untrusted", redirect: "manual",
    });
    assert.equal(response.status, 303);
    assert.equal(response.headers.get("location"), `https://pigeonpost.dev/account?checkout_return=1&subscription_id=${subscriptionId}&payment_id=${paymentId}`);
    assert.equal(grants.length, 0);
  });

  await t.test("under-review payment stays unpaid and completion can be checked again", async () => {
    completionStatus = 400;
    assert.equal((await post("/v1/checkout/complete", { subscriptionId, paymentId })).status, 400);
    assert.equal(grants.length, 0);
    completionStatus = 200;
    const response = await post("/v1/checkout/complete", { subscriptionId, paymentId });
    assert.equal(response.status, 200);
    assert.equal(response.body.bound, true);
    assert.equal(grants[0].namespace, "example");
    assert.equal(grants[0].account_id, "account-owner");
    assert.ok(grants[0].expires_at > Date.now() / 1000);
    const completions = calls.filter((c) => c.path.endsWith("payment-checkout/complete"));
    assert.notEqual(completions[0].key, completions[1].key, "a cached pending result must not prevent a fresh status check");
  });

  await t.test("a completed purchase is delivered again without payment", async () => {
    available = false;
    const before = calls.filter((c) => c.method === "POST").length;
    assert.equal((await buy()).body.bound, true);
    assert.equal(calls.filter((c) => c.method === "POST").length, before);
  });

  await t.test("wrong plan, expired period and unknown status cannot grant a handle", async () => {
    for (const change of [{ plan_slug: "cheap-plan" }, { current_period_end: "2020-01-01" }, { status: undefined }]) {
      reset(); subs = [{ id: subscriptionId, external_reference: reference, plan_slug: plan, status: "active", current_period_end: future(), ...change }];
      assert.equal((await post("/v1/handles/claim", { handle: "example" })).status, 402);
      assert.equal(grants.length, 0);
    }
  });

  await t.test("a foreign subscription or unrelated payment cannot complete", async () => {
    reset();
    assert.equal((await post("/v1/checkout/complete", { subscriptionId, paymentId })).status, 404);
    await buy();
    payments[0].metadata.commercial_metadata.subscription_id = "another-subscription";
    assert.equal((await post("/v1/checkout/complete", { subscriptionId, paymentId })).status, 404);
    assert.equal(grants.length, 0);
  });

  await t.test("an unavailable handle is rejected before any payment call", async () => {
    reset(); available = false;
    assert.equal((await buy()).status, 409);
    assert.equal(calls.some((c) => c.method === "POST"), false);
    assert.equal(grants.length, 0);
  });
});
