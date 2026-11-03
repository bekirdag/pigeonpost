// Contract: every billing-bridge path the portal calls is a route the bridge serves.
//
// The portal's bridge calls are read from its source (PPE.bridge(...) and the direct /api fetches),
// template parts are filled with valid sample ids, and each request is sent to the real bridge
// (store-adapter/src/server.js) with nothing behind it. A served route either answers or fails on
// its upstream; only a path the bridge does not know ends in its bare route-miss 404.
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import test from "node:test";
import { spawnBridge } from "../../store-adapter/test/harness.mjs";

const SITE = new URL("../", import.meta.url);
const ORG = "org_contract0001";
const UUID = "00000000-0000-4000-8000-000000000061";

/// `/ent/orgs/${...}/subscription` style templates to concrete paths.
function fill(template) {
  return template
    .replace(/\/invoices\/\$\{[^}]*\}/g, `/invoices/${UUID}`)
    .replace(/\/handles\/\$\{[^}]*\}/g, "/handles/acme")
    .replace(/\/ent\/orgs\/\$\{[^}]*\}/g, `/ent/orgs/${ORG}`)
    .replace(/\/ent\/checkout\/\$\{[^}]*\}/g, `/ent/checkout/${ORG}`)
    .replace(/\$\{cfg\.apiBase \|\| "\/api"\}/g, "");
}

/// Every bridge request in the portal's scripts, as { method, path, where }.
export function portalCalls() {
  const calls = [];
  for (const file of readdirSync(SITE).filter((f) => f.endsWith(".js"))) {
    const lines = readFileSync(new URL(file, SITE), "utf8").split("\n");
    const helpers = {};
    lines.forEach((line, n) => {
      // A local path helper such as `const subPath = (rest = "") => \`/ent/orgs/${...}/subscription${rest}\`;`
      const helper = /const (\w+) = \(rest = ""\) => `([^`]+)\$\{rest\}`/.exec(line);
      if (helper) helpers[helper[1]] = helper[2];
      const where = `${file}:${n + 1}`;
      // Options may continue on the next lines of the same call.
      const method = (/method: "(\w+)"/.exec(lines.slice(n, n + 3).join(" ").split(/;\s*$|;\s/)[0]) || [])[1] || "GET";
      for (const m of line.matchAll(/PPE\.bridge\((?:"([^"]+)"|`([^`]+)`|(\w+)\((?:"([^"]*)")?\))/g)) {
        const path = m[1] || m[2] || (helpers[m[3]] !== undefined ? helpers[m[3]] + (m[4] || "") : null);
        assert.ok(path, `${where}: a bridge call this test cannot resolve; extend fill() or name its helper`);
        calls.push({ method, path: fill(path), where });
      }
      for (const m of line.matchAll(/window\.fetch\(`\$\{cfg\.apiBase \|\| "\/api"\}([^`]+)`/g)) {
        const direct = /method: "(\w+)"/.exec(lines.slice(n, n + 3).join(" "));
        calls.push({ method: direct ? direct[1] : "GET", path: fill(m[1]), where });
      }
    });
  }
  return calls;
}

async function bridge(t) {
  // Upstreams on a closed port: a served route fails on its upstream, never with the route miss.
  const dead = "http://127.0.0.1:9";
  const b = await spawnBridge(t, { MASAAS_SAAS_API_URL: dead, PIGEONPOST_POSTBOX_URL: dead, POSTBOX_INTERNAL_URL: dead,
    POSTBOX_SERVICE_TOKEN: "svc_contract_fixture_token_0123456789", MASAAS_OIDC_ISSUER: dead, ENTERPRISE_ORIGIN: "https://enterprise.pigeonpost.dev" });
  return async (method, path) => {
    const r = await fetch(`${b.base}/api${path}`, { method, headers: { authorization: "Bearer contract", "content-type": "application/json" },
      body: method === "GET" ? undefined : "{}" });
    return { status: r.status, body: await r.json().catch(() => null) };
  };
}
const routeMiss = (r) => r.status === 404 && r.body && r.body.error === "not found" && !r.body.code;

test("the portal's bridge calls are found", () => {
  const calls = portalCalls();
  const paths = calls.map((c) => `${c.method} ${c.path}`);
  for (const expected of ["GET /v1/billing/invoices", `GET /v1/billing/invoices/${UUID}/download`, `GET /ent/orgs/${ORG}/subscription`,
    `POST /ent/orgs/${ORG}/subscription/change`, `POST /ent/orgs/${ORG}/subscription/cancel`, `POST /ent/orgs/${ORG}/subscription/retry`,
    `POST /ent/orgs/${ORG}/subscription/card-update`, "POST /ent/checkout", "POST /ent/billing-profile", "GET /v1/catalog", "POST /v1/auth/exchange"]) {
    assert.ok(paths.includes(expected), `${expected} not found among ${JSON.stringify(paths)}`);
  }
});

test("every bridge path the portal calls is served by the bridge", async (t) => {
  const call = await bridge(t);
  const misses = [];
  for (const c of portalCalls()) {
    const r = await call(c.method, c.path);
    if (routeMiss(r)) misses.push(`${c.where} ${c.method} ${c.path}`);
  }
  assert.deepEqual(misses, [], "portal calls with no bridge route");
  // The check itself can fail: the old invoice path is a route miss.
  assert.ok(routeMiss(await call("GET", "/v1/invoices")), "a path the bridge does not serve is detected");
});
