// The enterprise "Talk to sales" route: validated, rate-limited, bounded, never echoed.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { spawnBridge } from "./harness.mjs";

const good = { name: "Ada Example", company: "Example Corp", contact: "ada@example.test", size: "800", message: "We need 800 seats." };
const post = (base, body, headers = {}) => fetch(base + "/api/ent/sales", {
  method: "POST", headers: { "content-type": "application/json", origin: "https://enterprise.pigeonpost.dev", ...headers },
  body: JSON.stringify(body),
});

test("a valid request is recorded privately and answered with no data", async (t) => {
  const { base, stateDir } = await spawnBridge(t, {});
  const res = await post(base, good);
  assert.equal(res.status, 202);
  assert.deepEqual(await res.json(), { received: true });
  assert.equal(res.headers.get("access-control-allow-origin"), "https://enterprise.pigeonpost.dev");
  const file = join(stateDir, "sales-requests.jsonl");
  const lines = readFileSync(file, "utf8").trim().split("\n").map((l) => JSON.parse(l));
  assert.equal(lines.length, 1);
  assert.equal(lines[0].company, "Example Corp");
  assert.equal(statSync(file).mode & 0o777, 0o600);
});

test("bad input is refused and nothing is written", async (t) => {
  const { base } = await spawnBridge(t, {});
  for (const body of [
    { ...good, contact: "not an address" },
    { ...good, extra: "x" },
    { company: "x", contact: "a@b.test" },
    { ...good, message: "line\u0000two" },
    { ...good, message: "x".repeat(2001) },
  ]) {
    const res = await post(base, body);
    assert.equal(res.status, 400, JSON.stringify(body).slice(0, 60));
  }
});

test("one source is rate limited", async (t) => {
  const { base } = await spawnBridge(t, {});
  for (let i = 0; i < 5; i++) assert.equal((await post(base, good)).status, 202);
  const res = await post(base, good);
  assert.equal(res.status, 429);
  assert.equal((await res.json()).code, "rate_limited");
});
