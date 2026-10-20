// Shared harness: spawn the real bridge against local HTTP fixtures standing in for saas_be and
// the postbox (public and internal APIs). No payment provider or production service is contacted.
import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

export const ACCOUNT = "acct_0123456789abcdef01234567";
export const OIDC_SUB = "4b0c6d58-3c87-4e8d-9d64-0d6c1f8f0a11";
export const uuid = (n) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;

/// A catalog whose 2026 editions are "provisioned" with fixture ids.
export function fixtureCatalog() {
  const real = JSON.parse(readFileSync(new URL("../catalog/pigeonpost-catalog.json", import.meta.url), "utf8"));
  let n = 100;
  for (const plan of Object.values(real.plans)) {
    const edition = plan.editions["2026"];
    edition.plan_id = uuid(++n);
    edition.provider_plan_reference = `ref-${n}`;
  }
  const dir = mkdtempSync(join(tmpdir(), "pp-catalog-"));
  const file = join(dir, "catalog.json");
  writeFileSync(file, JSON.stringify(real));
  return { file, catalog: real };
}

export async function spawnBridge(t, env) {
  const stateDir = mkdtempSync(join(tmpdir(), "pp-bridge-"));
  const child = spawn(process.execPath, [new URL("../src/server.js", import.meta.url).pathname], {
    env: { PATH: process.env.PATH, PORT: "0", STORE_STATE_DIR: stateDir, RECONCILE_INTERVAL_MS: "0",
      STORE_ALLOWED_ORIGINS: "https://pigeonpost.dev", ...env },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let stderr = "";
  child.stderr.on("data", (c) => { stderr += c; });
  t.after(async () => {
    if (child.exitCode !== null || child.signalCode !== null) return;
    const exited = once(child, "exit");
    child.kill();
    await exited;
  });
  const base = await new Promise((resolve, reject) => {
    let output = "";
    const timer = setTimeout(() => reject(new Error(`bridge startup timed out ${stderr}`)), 5000);
    child.stdout.on("data", (chunk) => {
      output += chunk;
      const match = /adapter on :(\d+)/.exec(output);
      if (match) { clearTimeout(timer); resolve(`http://127.0.0.1:${match[1]}`); }
    });
    child.once("exit", (code) => { clearTimeout(timer); reject(new Error(`bridge exited ${code}: ${stderr}`)); });
  });
  return { base, stateDir, child, stderr: () => stderr };
}

export function json(res, status, body) {
  res.writeHead(status, { "content-type": "application/json" });
  res.end(JSON.stringify(body));
}

export async function readBody(req) {
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  return chunks.length ? JSON.parse(Buffer.concat(chunks)) : undefined;
}
