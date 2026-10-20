// The approve-a-machine page: fragment handling, rendering, hold-to-approve, step-up and errors.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { webcrypto } from "node:crypto";
import { JSDOM, VirtualConsole } from "jsdom";

const html = readFileSync(new URL("../pair.html", import.meta.url), "utf8")
  .replace(/<script[^>]*><\/script>/g, "");
const source = readFileSync(new URL("../pair.js", import.meta.url), "utf8");
const tick = () => new Promise((r) => setImmediate(r));
const settle = async () => { for (let i = 0; i < 30; i++) await tick(); };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const PAIRING = {
  pairing_id: "pair_1", org: { id: "org_1", name: "Acme Inc." }, handle: "/acme/agent1",
  address: "/k/agent", label: "Build box", confirmation: "123 456", capabilities: ["enroll"],
  ip_country: "TR", expires_at: Math.floor(Date.now() / 1000) + 500, can_approve: true,
};

function page(t, { hash = "#abcd-efgh", token = "tok", responses = {}, search = "" } = {}) {
  const dom = new JSDOM(html, { url: `https://inbox.pigeonpost.dev/pair.html${search}${hash}`, runScripts: "outside-only", virtualConsole: new VirtualConsole() });
  t.after(() => dom.window.close());
  const { window } = dom;
  Object.defineProperty(window, "crypto", { value: webcrypto });
  window.TextEncoder = TextEncoder;
  window.PIGEONPOST_INBOX = { postbox: "https://postbox.test", oidc: { issuer: "https://auth.test/realms/r", clientId: "pigeonpost-web", scope: "openid profile" } };
  if (token) window.localStorage.setItem("ppi_token", token);
  const calls = [];
  const assigned = [];
  window.PigeonpostPairNavigate = (u) => assigned.push(u);
  window.fetch = async (url, init = {}) => {
    const path = new URL(url).pathname;
    calls.push({ path, body: init.body ? JSON.parse(init.body) : null });
    const r = responses[path] || { status: 200, body: {} };
    const out = typeof r === "function" ? r() : r;
    return { ok: out.status < 400, status: out.status, headers: { get: (h) => (out.headers || {})[h] || null }, json: async () => out.body };
  };
  return { window, calls, assigned, start: async () => { window.eval(source); await settle(); } };
}

test("the code leaves the address bar, is kept for this tab, and the request is rendered", async (t) => {
  const p = page(t, { responses: { "/v1/device-pairings/resolve": { status: 200, body: PAIRING } } });
  await p.start();
  const d = p.window.document;
  assert.equal(p.window.location.hash, "", "fragment removed");
  assert.equal(p.window.sessionStorage.getItem("ppp_code"), "ABCD-EFGH");
  assert.deepEqual(p.calls[0], { path: "/v1/device-pairings/resolve", body: { user_code: "ABCD-EFGH" } });
  assert.equal(d.getElementById("pair-review").hidden, false);
  assert.equal(d.getElementById("pair-handle").textContent, "/acme/agent1");
  assert.equal(d.getElementById("pair-confirmation").textContent, "123 456");
  assert.equal(d.getElementById("pair-country").textContent, "TR");
  assert.match(d.getElementById("pair-expires").textContent, /^\d+:\d\d$/);
});

test("no token: asks to sign in, and the sign-in carries PKCE, state and nonce", async (t) => {
  const p = page(t, { token: null });
  await p.start();
  assert.equal(p.window.document.getElementById("pair-signin").hidden, false);
  p.window.document.getElementById("pair-signin-btn").click();
  await settle();
  const url = new URL(p.assigned[0]);
  assert.equal(url.searchParams.get("redirect_uri"), "https://inbox.pigeonpost.dev/pair.html");
  assert.equal(url.searchParams.get("code_challenge_method"), "S256");
  assert.ok(url.searchParams.get("state") && url.searchParams.get("nonce"));
  assert.equal(url.searchParams.get("acr_values"), null);
});

test("a short press does not approve; a full hold does", async (t) => {
  let approved = 0;
  const p = page(t, { responses: {
    "/v1/device-pairings/resolve": { status: 200, body: PAIRING },
    "/v1/device-pairings/pair_1/approve": () => { approved++; return { status: 200, body: { approved: true } }; },
  } });
  await p.start();
  const btn = p.window.document.getElementById("pair-approve");
  const ev = (type) => new p.window.Event(type, { bubbles: true });
  btn.dispatchEvent(ev("pointerdown"));
  await sleep(300);
  btn.dispatchEvent(ev("pointerup"));
  await sleep(1600);
  assert.equal(approved, 0, "released early");
  assert.match(p.window.document.getElementById("pair-status").textContent, /Keep holding/);

  btn.dispatchEvent(new p.window.KeyboardEvent("keydown", { key: " " }));
  await sleep(p.window.PigeonpostPair.HOLD_MS + 200);
  await settle();
  assert.equal(approved, 1);
  assert.deepEqual(p.calls.at(-1).body, { user_code: "ABCD-EFGH" });
  assert.equal(p.window.document.getElementById("pair-done").hidden, false);
  assert.match(p.window.document.getElementById("pair-done-text").textContent, /close this page/);
  assert.equal(p.window.sessionStorage.getItem("ppp_code"), null);
});

test("step-up: approval redirects for a strong sign-in with the named acr_values and max_age", async (t) => {
  const p = page(t, { responses: {
    "/v1/device-pairings/resolve": { status: 200, body: PAIRING },
    "/v1/device-pairings/pair_1/approve": { status: 401, body: { error: "step_up_required" },
      headers: { "www-authenticate": 'Bearer error="insufficient_user_authentication", acr_values="mfa", max_age=600' } },
  } });
  await p.start();
  p.window.document.getElementById("pair-approve").dispatchEvent(new p.window.Event("pointerdown"));
  await sleep(p.window.PigeonpostPair.HOLD_MS + 200);
  await settle();
  const url = new URL(p.assigned[0]);
  assert.equal(url.searchParams.get("acr_values"), "mfa");
  assert.equal(url.searchParams.get("max_age"), "600");
  assert.equal(p.window.sessionStorage.getItem("ppp_code"), "ABCD-EFGH", "code kept for the return");
  assert.equal(p.window.sessionStorage.getItem("ppp_resume"), "approve");
});

test("cannot approve: the button is disabled with the reason", async (t) => {
  const p = page(t, { responses: { "/v1/device-pairings/resolve": { status: 200, body: { ...PAIRING, can_approve: false, reason: "delegation_required" } } } });
  await p.start();
  const d = p.window.document;
  assert.equal(d.getElementById("pair-approve").disabled, true);
  assert.match(d.getElementById("pair-reason").textContent, /operators/);
});

test("errors: unknown, expired and missing codes end clearly", async (t) => {
  for (const [code, re] of [["pairing_not_found", /not valid/], ["pairing_expired", /expired/]]) {
    const p = page(t, { responses: { "/v1/device-pairings/resolve": { status: code === "pairing_expired" ? 410 : 404, body: { error: code } } } });
    await p.start();
    assert.equal(p.window.document.getElementById("pair-error").hidden, false);
    assert.match(p.window.document.getElementById("pair-error-text").textContent, re);
  }
  const p = page(t, { hash: "" });
  await p.start();
  assert.match(p.window.document.getElementById("pair-error-text").textContent, /No code/);
  const bad = page(t, { hash: "#<script>" });
  await bad.start();
  assert.equal(bad.window.sessionStorage.getItem("ppp_code"), null, "malformed codes are not kept");
});

test("deny ends the request", async (t) => {
  let denied = 0;
  const p = page(t, { responses: {
    "/v1/device-pairings/resolve": { status: 200, body: PAIRING },
    "/v1/device-pairings/pair_1/deny": () => { denied++; return { status: 200, body: {} }; },
  } });
  await p.start();
  p.window.document.getElementById("pair-deny").click();
  await settle();
  assert.equal(denied, 1);
  assert.match(p.window.document.getElementById("pair-done-text").textContent, /Declined/);
});

test("challenge parsing", async (t) => {
  const p = page(t, { hash: "" });
  await p.start();
  assert.deepEqual(JSON.parse(JSON.stringify(p.window.PigeonpostPair.parseChallenge('Bearer error="x", acr_values="a b", max_age=300'))), { acr_values: "a b", max_age: 300 });
});
