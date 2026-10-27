// Phase 5 pages against a fake postbox: API keys (step-up, shown-once secret, revoke) and webhooks
// (create, rotate, delete, deliveries), gated by role and by the `org_api` capability.
import assert from "node:assert/strict";
import test from "node:test";
import { load, reply, tick, jwt, plain } from "./helpers.mjs";

const ORG = { id: "org_abc123", name: "Acme", namespace: "acme", status: "active", seats: 30, seats_used: 3, agent_limit: 300, agents_used: 1,
  plan_code: "ent-30-yearly-usd", settings: {}, created_at: 1800000000, revision: 1 };
const NOW = Math.floor(Date.now() / 1000);
const KEYS = [
  { id: "okey_1", name: "HR sync", scopes: ["org:read", "members:write"], last4: "abcd", send_as: null, ip_allowlist: ["203.0.113.0/24"], created_at: 1800000000, created_by: "/acme/bekir", expires_at: null, last_used_at: null, revoked_at: null },
  { id: "okey_2", name: "Old bot", scopes: ["messages:send"], last4: "wxyz", send_as: "/acme/ci-bot", ip_allowlist: [], created_at: 1800000000, created_by: "/acme/bekir", expires_at: NOW - 10, last_used_at: NOW - 60, revoked_at: null },
  { id: "okey_3", name: "Gone", scopes: ["audit:read"], last4: "0000", send_as: null, ip_allowlist: [], created_at: 1800000000, created_by: "/acme/bekir", expires_at: null, last_used_at: null, revoked_at: 1800000100 },
];
const HOOKS = [
  { id: "wh_1", url: "https://hooks.example.test/pp", events: ["*"], description: "HR system", status: "active", created_at: 1800000000, created_by: "/acme/bekir",
    rotated_at: null, previous_secret_expires_at: null, last_delivery: { at: NOW - 30, status: "delivered", response_status: 204 } },
  { id: "wh_2", url: "https://siem.example.test/in", events: ["member.joined", "member.removed"], description: "", status: "active", created_at: 1800000000, created_by: "/acme/bekir",
    rotated_at: null, previous_secret_expires_at: null, last_delivery: null },
];
const KEY_SECRET = `pk_org_${"S".repeat(32)}`;
const HOOK_SECRET = `whsec_${"H".repeat(32)}`;

function postbox({ roles = ["owner"], features = ["orgs", "enterprise_portal", "org_api"], create, disabled = false } = {}) {
  const sent = [];
  const fn = (c) => {
    const p = new URL(c.url).pathname;
    const base = `/v1/orgs/${ORG.id}`;
    if (c.method !== "GET") sent.push({ method: c.method, path: p, body: c.body });
    if (p === "/v1/capabilities") return reply(200, { features });
    if (p === "/v1/orgs") return reply(200, { orgs: [{ ...ORG, my_roles: roles }] });
    if (p === base) return reply(200, { org: ORG, me: { member_id: "mem_1", roles, mailbox: { handle: "/acme/bekir" } } });
    if (p === `${base}/members`) return reply(200, { members: [] });
    if (p === `${base}/domains`) return reply(200, { domains: [] });
    if (p === `${base}/audit`) return reply(200, { entries: [], next_before_id: null });
    if (p === `${base}/agents`) return reply(200, { agents: [{ handle: "/acme/ci-bot", address: "/k/ci", status: "active", label: "Build box" }, { handle: "/acme/old", address: "/k/o", status: "suspended" }] });
    if (disabled && (p.includes("/api-keys") || p.includes("/webhooks"))) return reply(404, { error: "feature_disabled" });
    if (p === `${base}/api-keys` && c.method === "GET") return reply(200, { keys: KEYS });
    if (p === `${base}/api-keys` && c.method === "POST") {
      if (create) return create(c);
      return reply(201, { key: { ...KEYS[0], id: "okey_new", name: c.body.name, scopes: c.body.scopes, last4: "SSSS" }, secret: KEY_SECRET });
    }
    if (p.startsWith(`${base}/api-keys/`) && c.method === "DELETE") return reply(200, { revoked: true });
    if (p === `${base}/webhooks` && c.method === "GET") return reply(200, { webhooks: HOOKS });
    if (p === `${base}/webhooks` && c.method === "POST") return reply(201, { webhook: { ...HOOKS[1], id: "wh_new", url: c.body.url, events: c.body.events }, secret: HOOK_SECRET });
    if (p === `${base}/webhooks/wh_1/rotate`) return reply(200, { webhook: HOOKS[0], secret: `whsec_${"R".repeat(32)}`, previous_secret_expires_at: NOW + 86400 });
    if (p === `${base}/webhooks/wh_1` && c.method === "DELETE") return reply(200, { deleted: true });
    if (p === `${base}/webhooks/wh_1/deliveries`) return reply(200, { deliveries: [
      { id: "whd_2", event_id: "evt_2", event_type: "member.joined", attempt: 2, status: "failed", response_status: 500, error: "endpoint answered 500", duration_ms: 812, at: NOW - 100 },
      { id: "whd_1", event_id: "evt_1", event_type: "room.created", attempt: 1, status: "delivered", response_status: 204, error: null, duration_ms: 95, at: NOW - 200 },
    ] });
    return reply(404, { error: "not_found" });
  };
  fn.sent = sent;
  return fn;
}

const open = (t, page, opts = {}) => load(t, "app.html", { url: `https://enterprise.pigeonpost.dev/app/${page}`, session: jwt(), fetch: postbox(opts), ...opts.load });
const navText = (window) => [...window.document.querySelectorAll("#nav a")].map((a) => a.textContent);
const sheet = (window) => window.document.querySelector(".sheet");
const button = (root, text) => [...root.querySelectorAll("button, a.btn")].find((b) => b.textContent === text);

async function keySheet(t, opts = {}) {
  const pb = postbox(opts);
  const ctx = load(t, "app.html", { url: "https://enterprise.pigeonpost.dev/app/api-keys", session: jwt(), fetch: pb, ...(opts.load || {}) });
  await tick(60);
  ctx.$("#content .page-head .btn-primary").click();
  await tick(30);
  const s = sheet(ctx.window);
  const scope = (v) => s.querySelector(`input[type=checkbox][value="${v}"]`);
  return { ...ctx, pb, s, scope };
}

test("owners see API keys and webhooks in the nav; the keys page lists every key with its state", async (t) => {
  const { window, $ } = open(t, "api-keys");
  await tick(60);
  assert.deepEqual(navText(window).slice(0, 9), ["People", "Agents", "Rooms", "Rules", "Billing", "Security", "API keys", "Webhooks", "Organization"]);
  assert.equal($("#content h1").textContent, "API keys");
  assert.equal(window.document.querySelectorAll("#content table").length, 1);
  assert.equal(window.document.querySelectorAll("#content .page-head .btn-primary").length, 1);
  const rows = [...window.document.querySelectorAll("#content tbody tr")].map((r) => r.textContent);
  assert.match(rows[0], /HR sync.*org:read.*members:write.*1 allowed network.*pk_org_…abcd.*Never.*Never.*Active.*Revoke/);
  assert.match(rows[1], /Old bot.*sends as \/acme\/ci-bot.*pk_org_…wxyz.*Expired/);
  assert.match(rows[2], /Gone.*Revoked/);
  assert.equal(window.document.querySelectorAll("#content tbody tr")[2].querySelector("button"), null, "a revoked key has no actions");
});

test("admins see the pages too; people, agent and billing admins don't", async (t) => {
  const admin = open(t, "webhooks", { roles: ["admin"] });
  await tick(60);
  assert.ok(navText(admin.window).includes("API keys") && navText(admin.window).includes("Webhooks"));
  assert.equal(admin.$("#content h1").textContent, "Webhooks");
  for (const roles of [["people_admin"], ["agent_admin", "billing_admin"]]) {
    const other = open(t, "api-keys", { roles });
    await tick(60);
    const nav = navText(other.window);
    assert.ok(!nav.includes("API keys") && !nav.includes("Webhooks"), roles.join());
    assert.notEqual(other.$("#content h1").textContent, "API keys", "a hidden page falls back");
  }
});

test("with org_api off the pages are hidden, and a feature_disabled answer hides them too", async (t) => {
  const off = open(t, "api-keys", { features: ["orgs", "enterprise_portal"] });
  await tick(60);
  assert.ok(!navText(off.window).includes("API keys") && !navText(off.window).includes("Webhooks"));
  assert.equal(off.$("#content h1").textContent, "People");
  off.window.PPE.app.go("webhooks"); await tick(30);
  assert.equal(off.$("#content h1").textContent, "People");

  const gone = open(t, "webhooks", { disabled: true });
  await tick(80);
  assert.ok(!navText(gone.window).includes("Webhooks"));
  assert.equal(gone.$("#content h1").textContent, "People");
  assert.ok(!gone.window.PPE.app.features.includes("org_api"));
});

test("creating a key posts the form, shows the secret once, and drops it when closed", async (t) => {
  const { window, s, scope, pb } = await keySheet(t);
  s.querySelector("input[type=text]").value = "HR sync 2";
  scope("org:read").checked = true;
  scope("members:write").checked = true;
  s.querySelector("input[type=number]").value = "90";
  s.querySelector("textarea").value = "203.0.113.0/24\n\n2001:db8::/32\n";
  button(s, "Create key").click();
  await tick(30);
  assert.deepEqual(plain(pb.sent[0]), { method: "POST", path: `/v1/orgs/${ORG.id}/api-keys`,
    body: { name: "HR sync 2", scopes: ["org:read", "members:write"], expires_in_days: 90, ip_allowlist: ["203.0.113.0/24", "2001:db8::/32"] } });
  const shown = sheet(window);
  assert.equal(shown.querySelector(".secret code").textContent, KEY_SECRET);
  assert.match(shown.textContent, /You won't see it again/);
  assert.equal(window.document.body.textContent.split(KEY_SECRET).length - 1, 1, "the secret is on the page exactly once");
  const code = shown.querySelector(".secret code");
  button(shown, "I've stored it").click();
  await tick(40);
  assert.equal(sheet(window), null);
  assert.equal(code.textContent, "", "the detached node no longer holds it");
  assert.ok(!window.document.documentElement.outerHTML.includes(KEY_SECRET), "gone from the page");
  assert.ok(!JSON.stringify(Object.fromEntries(Object.entries(window.sessionStorage))).includes(KEY_SECRET), "never stored");
  assert.equal(window.document.querySelector("#content h1").textContent, "API keys", "the list is refreshed");
});

test("messages:send needs an agent; only active agents are offered; client checks run before posting", async (t) => {
  const { window, s, scope, pb } = await keySheet(t);
  const sendAsField = s.querySelector("select").closest(".field");
  assert.equal(sendAsField.hidden, true, "the agent picker is hidden until messages:send is chosen");
  s.querySelector("input[type=text]").value = "Notifier";
  scope("messages:send").checked = true;
  scope("messages:send").dispatchEvent(new window.Event("change"));
  assert.equal(sendAsField.hidden, false);
  assert.deepEqual([...s.querySelectorAll("select option")].map((o) => o.value), ["", "/acme/ci-bot"]);
  button(s, "Create key").click();
  await tick(10);
  assert.match(s.querySelector(".status-line").textContent, /Choose the agent/);
  s.querySelector("textarea").value = "not-an-ip";
  s.querySelector("select").value = "/acme/ci-bot";
  button(s, "Create key").click();
  await tick(10);
  assert.match(s.querySelector(".status-line").textContent, /not-an-ip doesn't look like/);
  s.querySelector("textarea").value = "";
  s.querySelector("input[type=number]").value = "999";
  button(s, "Create key").click();
  await tick(10);
  assert.match(s.querySelector(".status-line").textContent, /1 to 730/);
  assert.equal(pb.sent.length, 0);
  s.querySelector("input[type=number]").value = "";
  button(s, "Create key").click();
  await tick(30);
  assert.deepEqual(plain(pb.sent[0].body), { name: "Notifier", scopes: ["messages:send"], send_as: "/acme/ci-bot" });
});

test("step_up_required takes the existing step-up path and replays the request on return", async (t) => {
  const challenge = (c) => reply(401, { error: "step_up_required", acr_values: "gold", max_age: 300 });
  const first = await keySheet(t, { create: challenge });
  first.s.querySelector("input[type=text]").value = "Audit export";
  first.scope("audit:read").checked = true;
  button(first.s, "Create key").click();
  const to = await first.navigation;
  const u = new URL(to);
  assert.equal(u.searchParams.get("acr_values"), "gold");
  assert.equal(u.searchParams.get("max_age"), "300");
  const resume = JSON.parse(first.window.sessionStorage.getItem("ppe_resume"));
  assert.equal(resume.name, "app.apikey");
  assert.deepEqual(resume.payload, { name: "Audit export", scopes: ["audit:read"] });
  assert.equal(JSON.parse(first.window.sessionStorage.getItem("ppe_pkce")).returnTo, "/app/api-keys");
  assert.equal(first.window.document.querySelectorAll(".toast-error").length, 0, "no error toast while leaving");

  // Back from the realm with a stronger session: the same request goes out once and the secret shows.
  const pb = postbox();
  const back = load(t, "app.html", { url: "https://enterprise.pigeonpost.dev/app/api-keys", session: jwt(), fetch: pb,
    storage: { ppe_resume: { ...resume } } });
  await tick(90);
  assert.deepEqual(plain(pb.sent), [{ method: "POST", path: `/v1/orgs/${ORG.id}/api-keys`, body: { name: "Audit export", scopes: ["audit:read"] } }]);
  assert.equal(sheet(back.window).querySelector(".secret code").textContent, KEY_SECRET);
  assert.equal(back.window.sessionStorage.getItem("ppe_resume"), null);
});

test("step_up_unavailable says keys can't be created here", async (t) => {
  const { s, scope, nav } = await keySheet(t, { create: () => reply(403, { error: "step_up_unavailable" }) });
  s.querySelector("input[type=text]").value = "x";
  scope("org:read").checked = true;
  button(s, "Create key").click();
  await tick(30);
  assert.equal(s.querySelector(".status-line").textContent, "Strong sign-in isn't configured on this server, so keys can't be created.");
  assert.equal(nav.length, 0);
});

test("revoking asks first, then deletes the key", async (t) => {
  const pb = postbox();
  const { window } = load(t, "app.html", { url: "https://enterprise.pigeonpost.dev/app/api-keys", session: jwt(), fetch: pb });
  await tick(60);
  let asked = "";
  window.confirm = (q) => { asked = q; return false; };
  window.document.querySelector('#content button[aria-label="Revoke HR sync"]').click();
  await tick(20);
  assert.match(asked, /Revoke HR sync\?/);
  assert.equal(pb.sent.length, 0, "cancelled");
  window.confirm = () => true;
  window.document.querySelector('#content button[aria-label="Revoke HR sync"]').click();
  await tick(30);
  assert.deepEqual(plain(pb.sent), [{ method: "DELETE", path: `/v1/orgs/${ORG.id}/api-keys/okey_1` }]);
});

test("the webhooks page lists endpoints, events and last delivery, and links the signature guide", async (t) => {
  const { window, $ } = open(t, "webhooks");
  await tick(60);
  assert.equal(window.document.querySelectorAll("#content table").length, 1);
  const rows = [...window.document.querySelectorAll("#content tbody tr")].map((r) => r.textContent);
  assert.match(rows[0], /https:\/\/hooks\.example\.test\/pp.*HR system.*All events.*Delivered.*HTTP 204/);
  assert.match(rows[1], /member\.joined, member\.removed.*Nothing sent yet/);
  assert.equal($('#content a[href="https://developers.pigeonpost.dev/org-api-webhooks"]').textContent, "How to verify signatures");
});

test("adding a webhook posts url, events and description and shows the signing secret once", async (t) => {
  const pb = postbox();
  const { window, $ } = load(t, "app.html", { url: "https://enterprise.pigeonpost.dev/app/webhooks", session: jwt(), fetch: pb });
  await tick(60);
  $("#content .page-head .btn-primary").click();
  const s = sheet(window);
  s.querySelector("input[type=url]").value = "http://hooks.example.test/x";
  s.querySelector('input[value="member.joined"]').checked = true;
  button(s, "Add webhook").click();
  await tick(10);
  assert.match(s.querySelector(".status-line").textContent, /https:\/\//);
  s.querySelector("input[type=url]").value = "https://hooks.example.test/x";
  const all = s.querySelector('input[value="*"]');
  all.checked = true; all.dispatchEvent(new window.Event("change"));
  assert.equal(s.querySelector('input[value="member.joined"]').disabled, true);
  s.querySelector('input[type=text]').value = "Directory sync";
  button(s, "Add webhook").click();
  await tick(30);
  assert.deepEqual(plain(pb.sent[0]), { method: "POST", path: `/v1/orgs/${ORG.id}/webhooks`, body: { url: "https://hooks.example.test/x", events: ["*"], description: "Directory sync" } });
  assert.equal(sheet(window).querySelector(".secret code").textContent, HOOK_SECRET);
  button(sheet(window), "I've stored it").click();
  await tick(30);
  assert.ok(!window.document.documentElement.outerHTML.includes(HOOK_SECRET));
});

test("rotating shows the new secret once with the 24-hour note; deleting confirms; deliveries render", async (t) => {
  const pb = postbox();
  const { window } = load(t, "app.html", { url: "https://enterprise.pigeonpost.dev/app/webhooks", session: jwt(), fetch: pb });
  await tick(60);
  const manage = () => { window.document.querySelector('#content button[aria-label="Manage https://hooks.example.test/pp"]').click(); return sheet(window); };

  button(manage(), "Rotate secret").click();
  await tick(30);
  assert.deepEqual(plain(pb.sent[0]), { method: "POST", path: `/v1/orgs/${ORG.id}/webhooks/wh_1/rotate`, body: {} });
  const rotated = sheet(window);
  assert.equal(rotated.querySelector(".secret code").textContent, `whsec_${"R".repeat(32)}`);
  assert.match(rotated.textContent, /old secret keeps working for 24 hours/);
  button(rotated, "I've stored it").click();
  await tick(40);
  assert.ok(!window.document.documentElement.outerHTML.includes("R".repeat(32)));

  button(manage(), "Recent deliveries").click();
  await tick(30);
  const items = [...sheet(window).querySelectorAll(".deliveries li")].map((li) => li.textContent);
  assert.equal(items.length, 2);
  assert.match(items[0], /member\.joined.*Failed.*attempt 2.*HTTP 500.*812 ms.*endpoint answered 500/);
  assert.match(items[1], /room\.created.*Delivered.*attempt 1.*HTTP 204.*95 ms/);
  assert.equal(sheet(window).querySelectorAll("table").length, 0, "the page keeps its one table");

  button(manage(), "Delete webhook").click();
  await tick(30);
  assert.deepEqual(plain(pb.sent.at(-1)), { method: "DELETE", path: `/v1/orgs/${ORG.id}/webhooks/wh_1` });
  assert.equal(sheet(window), null);
});

test("the security page points to the new pages only when they are available", async (t) => {
  const on = open(t, "security");
  await tick(60);
  assert.ok(button(on.$("#content"), "Manage API keys"));
  assert.doesNotMatch(on.$("#content").textContent, /Coming in Phase 5/);
  const off = open(t, "security", { features: ["orgs", "enterprise_portal"] });
  await tick(60);
  assert.equal(button(off.$("#content"), "Manage API keys"), undefined);
});

test("helpers: allowlist entries and key states", async (t) => {
  const { window } = open(t, "api-keys");
  await tick(60);
  const app = window.PPE.app;
  for (const ok of ["203.0.113.0/24", "198.51.100.7", "2001:db8::/32", "::1"]) assert.ok(app.looksLikeCidr(ok), ok);
  for (const bad of ["example.com", "203.0.113", "abc", "1.2.3.4/x"]) assert.ok(!app.looksLikeCidr(bad), bad);
  assert.equal(app.keyState({ revoked_at: 5, expires_at: 1 }, 10), "revoked");
  assert.equal(app.keyState({ expires_at: 9 }, 10), "expired");
  assert.equal(app.keyState({ expires_at: 11 }, 10), "active");
  assert.equal(app.keyState({}, 10), "active");
});
