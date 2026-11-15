// The enterprise "Talk to sales" route: validated, rate-limited, bounded, never echoed.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import http from "node:http";
import { once } from "node:events";
import { spawnBridge, readBody } from "./harness.mjs";
import { createNotifier, FRAME, LOGO_DARK, LOGO_LIGHT, noticeRequest, renderLead } from "../src/sales.js";

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

test("a lead is emailed to a person through the pigeonpost-notice workflow, without making the browser wait", async (t) => {
  const seen = [];
  let release;
  const gate = new Promise((resolve) => { release = resolve; });
  const saas = http.createServer(async (req, res) => {
    seen.push({ path: req.url, key: req.headers["x-api-key"], idem: req.headers["idempotency-key"], body: await readBody(req) });
    await gate;
    res.writeHead(201, { "content-type": "application/json" });
    res.end("{}");
  });
  saas.listen(0, "127.0.0.1");
  await once(saas, "listening");
  t.after(() => { release(); saas.closeAllConnections(); saas.close(); });
  const { base, stateDir } = await spawnBridge(t, { SALES_NOTIFY_URL: `http://127.0.0.1:${saas.address().port}`,
    SALES_NOTIFY_KEY: "notify-fixture-key", SALES_NOTIFY_TO: "sales@example.test" });
  const res = await post(base, { ...good, message: "<script>x</script> & more" });
  assert.equal(res.status, 202, "answered while the notification is still in flight");
  for (let i = 0; i < 50 && !seen.length; i++) await new Promise((r) => setTimeout(r, 20));
  release();
  assert.equal(seen.length, 1);
  const { path, key, idem, body } = seen[0];
  assert.equal(path, "/v1/internal/notifications/email");
  assert.equal(key, "notify-fixture-key");
  assert.equal(body.workflow_id, "pigeonpost-notice");
  assert.equal(body.to, "sales@example.test");
  assert.equal(body.payload.notice_kind, "sales_lead");
  assert.equal(idem, body.transaction_id);
  const recorded = JSON.parse(readFileSync(join(stateDir, "sales-requests.jsonl"), "utf8").trim());
  assert.equal(body.payload.ref_id, recorded.id, "the notification names the recorded request");
  assert.ok(!body.html.includes("<script>x"), "the lead is escaped");
  assert.ok(body.html.includes("&lt;script&gt;x&lt;/script&gt; &amp; more"));
  assert.ok(body.text.includes("Example Corp") && body.text.includes("ada@example.test"));
  // The design kit frame: the only images are the two static, versioned logos, and nothing is a link.
  const images = [...body.html.matchAll(/<img[^>]*src="([^"]+)"/g)].map((m) => m[1]);
  assert.deepEqual(images, [LOGO_LIGHT, LOGO_DARK], "no tracking pixel");
  assert.ok(!/href=/i.test(body.html), "no links");
  assert.ok(body.html.includes("max-width:560px") && body.html.includes("prefers-color-scheme: dark"));
  assert.ok(body.html.includes("<!-- pp-email-frame v1 -->") && body.html.includes("Pigeonpost · Piyote Bilişim Ltd. Şti. · pigeonpost.dev"));
  assert.ok(!/@font-face|@import|<script|<link/i.test(body.html), "no webfont, script or stylesheet");
});

test("the lead uses the design kit frame and the payload fits the notice schema limits", () => {
  const long = { id: "lead_x", at: "2026-11-04T10:00:00.000Z", name: "N", company: "C", contact: "a@b.test", size: "900", message: "m".repeat(2000) };
  const body = noticeRequest(long, "info@pigeonpost.dev", "no-reply@pigeonpost.dev");
  // saas_be's pigeonpost-notice schema: html 1..512000 and text 1..256000 UTF-16 units.
  assert.ok(body.html.length > 0 && body.html.length <= 512_000);
  assert.ok(body.text.length > 0 && body.text.length <= 256_000);
  assert.equal(body.payload.html, body.html);
  assert.ok(body.html.startsWith("<!DOCTYPE html>"));
  assert.ok(body.text.endsWith("Pigeonpost · Piyote Bilişim Ltd. Şti. · pigeonpost.dev\n"));
});

test("the frame is byte for byte the postbox's shared frame", () => {
  const rust = readFileSync(new URL("../../crates/pigeonpost-postbox/src/mail_frame.html", import.meta.url), "utf8");
  assert.equal(FRAME, rust.replace(/\r\n/g, "\n"));
});

test("the lead notification matches the notice schema and the subject stays one line", () => {
  const lead = { id: "lead_0123456789abcdef01", at: "2026-11-04T10:00:00.000Z", name: "Ada", company: "Evil\r\nBcc: x@y", contact: "a@b.test", size: "900" };
  const body = noticeRequest(lead, "info@pigeonpost.dev", "no-reply@pigeonpost.dev");
  assert.equal(body.tenant_id, "pigeonpost");
  assert.equal(body.payload.tenant_id, "pigeonpost");
  assert.equal(body.payload.delivery_id, body.transaction_id);
  assert.ok(!/[\r\n]/.test(body.subject) && body.subject.length <= 998);
  assert.deepEqual(Object.keys(body.payload).sort(), ["delivery_id", "html", "locale", "notice_kind", "ref_id", "subject", "tenant_id", "text"]);
  assert.equal(renderLead(lead).subject, body.subject);
});

test("sending retries a bounded number of times and is off without configuration", async () => {
  let calls = 0;
  const failing = createNotifier({ url: "http://127.0.0.1:9", key: "k", delays: [0, 1, 1], fetch: async () => { calls++; return { ok: false }; } });
  assert.equal(await failing.send({ id: "lead_x", at: "t", name: "n", company: "c", contact: "a@b.test" }), false);
  assert.equal(calls, 3);
  assert.deepEqual(failing.health(), { configured: true, sent: 0, failed: 3, gave_up: 1 });
  const flaky = createNotifier({ url: "https://saas.example", key: "k", delays: [0, 1], fetch: async () => ({ ok: ++calls > 4 }) });
  assert.equal(await flaky.send({ id: "lead_y", at: "t", name: "n", company: "c", contact: "a@b.test" }), true);
  assert.equal(createNotifier({ url: "http://example.test", key: "k" }).configured, false, "plain http only to loopback");
  assert.equal(createNotifier({ url: "https://saas.example", key: "" }).configured, false);
});
