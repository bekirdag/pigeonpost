import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import test from "node:test";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

async function health(t, overrides) {
  const child = spawn(process.execPath, [new URL("../src/server.js", import.meta.url).pathname], {
    env: { PATH: process.env.PATH, PORT: "0", MASAAS_RUNTIME_TOKEN: "", RECONCILE_INTERVAL_MS: "0",
      STORE_STATE_DIR: mkdtempSync(join(tmpdir(), "pp-health-")),
      POSTBOX_INTERNAL_URL: "http://10.0.0.2:8991", POSTBOX_SERVICE_TOKEN: "svc_fictional_billing_token_0123456789abcdef",
      SAAS_INTERNAL_TOKEN: "fictional-saas-internal", ...overrides },
    stdio: ["ignore", "pipe", "pipe"],
  });
  t.after(async () => {
    if (child.exitCode !== null || child.signalCode !== null) return;
    const exited = once(child, "exit");
    child.kill();
    await exited;
  });
  const base = await new Promise((resolve, reject) => {
    let output = "";
    const timer = setTimeout(() => reject(new Error("adapter startup timed out")), 5000);
    const finish = (error, value) => { clearTimeout(timer); error ? reject(error) : resolve(value); };
    child.stdout.on("data", (chunk) => {
      output += chunk;
      const match = /adapter on :(\d+)/.exec(output);
      if (match) finish(null, `http://127.0.0.1:${match[1]}`);
    });
    child.once("error", (error) => finish(error));
    child.once("exit", (code) => finish(new Error(`adapter exited ${code}`)));
  });
  const response = await fetch(base + "/api/healthz");
  assert.equal(response.status, 200, "liveness remains available for incomplete configuration");
  return response.json();
}

test("health accepts checkout configuration without an unused runtime token", async (t) => {
  assert.deepEqual(await health(t, {}), { ok: true, configured: true, product: "pigeonpost" });
});

test("activation and reconciliation credentials are required", async (t) => {
  for (const missing of ["POSTBOX_SERVICE_TOKEN", "SAAS_INTERNAL_TOKEN", "POSTBOX_INTERNAL_URL"]) {
    const result = await health(t, { MASAAS_RUNTIME_TOKEN: "fictional-runtime", [missing]: "" });
    assert.equal(result.configured, false, missing);
  }
});

test("health reports invalid endpoint and return-origin configuration", async (t) => {
  for (const overrides of [
    { MASAAS_SAAS_API_URL: "not-a-url" },
    { OIDC_ISSUER: "file:///tmp/issuer" },
    { STORE_ALLOWED_ORIGINS: "invalid,https://pigeonpost.dev" },
    { STORE_ALLOWED_ORIGINS: "http://pigeonpost.dev" },
    { STORE_ALLOWED_ORIGINS: "https://pigeonpost.dev/account" },
    { ENTERPRISE_ORIGIN: "https://enterprise.pigeonpost.dev/x" },
    { IYZICO_MODE: "sandbox" },
  ]) {
    assert.equal((await health(t, overrides)).configured, false);
  }
});
