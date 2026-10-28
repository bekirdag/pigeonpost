import assert from "node:assert/strict";
import test from "node:test";
import { createLimiter } from "../src/limits.js";
import { spawnBridge } from "./harness.mjs";

test("the public budget is per source and per minute", () => {
  let t = 1_000_000;
  const limiter = createLimiter({ perMinute: 3, now: () => t });
  for (let i = 0; i < 3; i++) assert.equal(limiter.take("a"), 0);
  assert.ok(limiter.take("a") > 0, "the fourth request inside a minute waits");
  assert.equal(limiter.take("b"), 0, "another address is unaffected");
  t += 60_001;
  assert.equal(limiter.take("a"), 0, "the window slides");
});

test("unauthenticated sign-in and availability routes answer 429 with Retry-After past the budget", async (t) => {
  const bridge = await spawnBridge(t, {
    STORE_PUBLIC_REQUESTS_PER_MINUTE: "2",
    // Nothing listens here: availability answers known:false, the exchange fails upstream.
    PIGEONPOST_POSTBOX_URL: "http://127.0.0.1:9", OIDC_ISSUER: "http://127.0.0.1:9/realms/x",
  });
  const avail = () => fetch(bridge.base + "/api/v1/handles/someone/availability");
  assert.equal((await avail()).status, 200);
  assert.equal((await avail()).status, 200);
  const refused = await avail();
  assert.equal(refused.status, 429);
  assert.ok(Number(refused.headers.get("retry-after")) >= 1);
  assert.equal((await refused.json()).code, "rate_limited");
  const exchange = await fetch(bridge.base + "/api/v1/auth/exchange", {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ code: "x", redirectUri: "https://pigeonpost.dev/account" }),
  });
  assert.equal(exchange.status, 429, "the sign-in routes share the same budget");
  // The public catalog stays unmetered.
  assert.equal((await fetch(bridge.base + "/api/v1/catalog")).status, 200);
});

test("a malformed path escape is a 400", async (t) => {
  const bridge = await spawnBridge(t, { PIGEONPOST_POSTBOX_URL: "http://127.0.0.1:9" });
  const r = await fetch(bridge.base + "/api/v1/handles/%E0%A4%A/availability");
  assert.equal(r.status, 400);
});
