// Small helpers for the Nests end-to-end test (drafted by a local model, reviewed).
import { createHash } from "node:crypto";

function zeroBits(buf) {
  let n = 0;
  for (const b of buf) {
    if (b === 0) { n += 8; continue; }
    return n + Math.clz32(b) - 24;
  }
  return n;
}

export async function pb(base, method, path, key, body) {
  const headers = {};
  if (body !== undefined) headers["content-type"] = "application/json";
  if (key) headers.authorization = `Bearer ${key}`;
  const res = await fetch(`${base}${path}`, { method, headers, body: body !== undefined ? JSON.stringify(body) : undefined });
  let data = {};
  try { data = await res.json(); } catch (_) { /* empty body */ }
  if (!res.ok) throw new Error(`${method} ${path} -> ${res.status} ${data.error || data.message || res.statusText}`);
  return data;
}

export async function proofOfWork(base) {
  const { challenge, bits } = await pb(base, "GET", "/v1/pow/challenge");
  for (let i = 0; ; i++) {
    if (zeroBits(createHash("sha256").update(`${challenge}:${i}`).digest()) >= bits) {
      return { pow_challenge: challenge, pow_solution: String(i) };
    }
  }
}

export async function waitFor(fn, { timeoutMs, intervalMs = 1000 }) {
  const start = Date.now();
  for (;;) {
    const result = await fn();
    if (result) return result;
    if (Date.now() - start >= timeoutMs) throw new Error("waitFor: timed out");
    await new Promise((r) => setTimeout(r, intervalMs));
  }
}
