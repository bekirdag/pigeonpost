// The admin app against a fake postbox: gating, the People table, invitations and rules.
import assert from "node:assert/strict";
import test from "node:test";
import { load, reply, tick, jwt, plain } from "./helpers.mjs";

const ORG = { id: "org_abc123", name: "Acme", namespace: "acme", status: "active", seats: 30, seats_used: 3, agent_limit: 300, agents_used: 1,
  plan_code: "ent-30-yearly-usd", settings: { require_mfa: false, invite_domains_only: false, rooms_creation: "everyone" }, created_at: 1800000000, revision: 1 };
const MEMBERS = [
  { id: "mem_1", first_name: "Bekir", last_name: "Dag", handle: "/acme/bekir", address: "/k/a", roles: ["owner"], status: "active", contact_masked: "b***@acme.test", last_active_at: 1800000000 },
  { id: "mem_2", first_name: "Jane", last_name: "Doe", handle: "/acme/jane", address: "/k/b", roles: [], status: "invited", contact_masked: "j***@acme.test", invited_at: 1800000000, invite: { id: "inv_1", expires_at: 1900000000, state: "pending" } },
  { id: "mem_3", first_name: "Sam", last_name: "Roe", handle: "/acme/sam", address: "/k/c", roles: ["people_admin"], status: "suspended", contact_masked: "s***@acme.test" },
];

function postbox({ roles = ["owner"], features = ["orgs", "enterprise_portal"], rules = [], mailbox = { handle: "/acme/bekir" } } = {}) {
  const posted = [];
  const fn = (c) => {
    const p = new URL(c.url).pathname;
    if (p === "/v1/capabilities") return reply(200, { features });
    if (p === "/v1/orgs") return reply(200, { orgs: [{ ...ORG, my_roles: roles }] });
    if (p === `/v1/orgs/${ORG.id}`) return reply(200, { org: ORG, me: { member_id: "mem_1", roles, mailbox } });
    if (p.endsWith("/members")) return reply(200, { members: MEMBERS });
    if (p.endsWith("/invites") && c.method === "POST") { posted.push(c.body); return reply(200, { results: c.body.invites.map((i, n) => ({ status: "invited", member_id: `mem_n${n}`, invite_id: `inv_n${n}`, handle: i.handle, link: `https://enterprise.pigeonpost.dev/invite#${"T".repeat(43)}`, expires_at: 1900000000 })) }); }
    if (p.endsWith("/rules") && c.method === "GET") return reply(200, { rules, revision: 1 });
    if (p.endsWith("/rules") && c.method === "POST") { posted.push(c.body); return reply(201, { rule: { id: "rul_1", ...c.body } }); }
    return reply(404, { error: "not_found" });
  };
  fn.posted = posted;
  return fn;
}

test("signed out, the app asks for sign-in; without the flag it says coming soon", async (t) => {
  const out = load(t, "app.html", { url: "https://enterprise.pigeonpost.dev/app" });
  await tick();
  assert.match(out.$("#gate").textContent, /Sign in to manage/);
  const off = load(t, "app.html", { url: "https://enterprise.pigeonpost.dev/app", session: jwt(), fetch: postbox({ features: ["orgs"] }) });
  await tick();
  assert.match(off.$("#gate").textContent, /Coming soon/);
});

test("a plain member is sent to the inbox", async (t) => {
  const { $ } = load(t, "app.html", { url: "https://enterprise.pigeonpost.dev/app", session: jwt(), fetch: postbox({ roles: [] }) });
  await tick();
  assert.match($("#gate").textContent, /Your company workspace is in the inbox/);
  assert.equal($("#gate a").getAttribute("href"), "https://inbox.pigeonpost.dev");
});

test("People shows seats, one table and a row per person", async (t) => {
  const { $, window } = load(t, "app.html", { url: "https://enterprise.pigeonpost.dev/app/people", session: jwt(), fetch: postbox() });
  await tick(60);
  assert.equal($("#shell").hidden, false);
  assert.match($("#content h1").textContent, /People/);
  assert.match($("#content .page-head p").textContent, /3 of 30 seats used/);
  assert.equal(window.document.querySelectorAll("#content table").length, 1);
  const rows = [...window.document.querySelectorAll("#content tbody tr")].map((r) => r.textContent);
  assert.equal(rows.length, 3);
  assert.match(rows[0], /Bekir Dag.*\/acme\/bekir.*Owner.*Active/);
  assert.match(rows[1], /Jane Doe.*Invited/);
  assert.match(rows[2], /People admin.*Suspended/);
  const primary = window.document.querySelectorAll("#content .page-head .btn-primary");
  assert.equal(primary.length, 1, "one primary action");
  const nav = [...window.document.querySelectorAll("#nav a")].map((a) => a.textContent);
  assert.deepEqual(nav.slice(0, 7), ["People", "Agents", "Rooms", "Rules", "Billing", "Security", "Organization"]);
});

test("a people admin cannot grant roles and does not see billing or security", async (t) => {
  const { window } = load(t, "app.html", { url: "https://enterprise.pigeonpost.dev/app/people", session: jwt(), fetch: postbox({ roles: ["people_admin"] }) });
  await tick(60);
  const nav = [...window.document.querySelectorAll("#nav a")].map((a) => a.textContent);
  assert.ok(nav.includes("People") && nav.includes("Rules") && nav.includes("Rooms"));
  assert.ok(!nav.includes("Billing") && !nav.includes("Security") && !nav.includes("Agents"));
  window.PPE.app.go("billing"); await tick(30);
  assert.match(window.document.querySelector("#content h1").textContent, /People/, "a hidden page falls back");
  assert.deepEqual(plain(window.PPE.app.grantable()), []);
});

test("inviting shows each link once with a copy action when invitation messages are off", async (t) => {
  const pb = postbox();
  const { $, window } = load(t, "app.html", { url: "https://enterprise.pigeonpost.dev/app/people", session: jwt(), fetch: pb });
  await tick(60);
  $("#content .page-head .btn-primary").click();
  const sheet = () => window.document.querySelector(".sheet");
  const inputs = sheet().querySelectorAll(".invite-row input");
  inputs[0].value = "John"; inputs[0].dispatchEvent(new window.Event("input"));
  inputs[1].value = "Smith"; inputs[2].value = "john@acme.test";
  assert.equal(inputs[3].value, "john", "the address follows the first name");
  const roleOptions = [...sheet().querySelectorAll("select option")].map((o) => o.value);
  assert.deepEqual(roleOptions, ["", "admin", "people_admin", "agent_admin", "billing_admin"]);
  [...sheet().querySelectorAll("button")].find((b) => b.textContent === "Send invitations").click();
  await tick(30);
  assert.deepEqual(pb.posted[0], { invites: [{ first_name: "John", last_name: "Smith", contact: "john@acme.test", handle: "/acme/john", roles: [] }] });
  assert.match(sheet().textContent, /Shown once/);
  assert.match(sheet().querySelector(".secret code").textContent, /^https:\/\/enterprise\.pigeonpost\.dev\/invite#T+$/);
});

test("the owner without a company address is asked to choose one", async (t) => {
  const { $ } = load(t, "app.html", { url: "https://enterprise.pigeonpost.dev/app/people", session: jwt(), fetch: postbox({ mailbox: null }) });
  await tick(60);
  assert.match($("#content").textContent, /Choose your address in Acme/);
});

test("the rules page adds a template and draws sentence editors", async (t) => {
  const pb = postbox({ rules: [{ id: "rul_9", owners: "/acme/*", peers: "/acme/*", admission: "allow", known: true, autonomy: null, verbs: null, locked: true, priority: 100 }] });
  const { window } = load(t, "app.html", { url: "https://enterprise.pigeonpost.dev/app/rules", session: jwt(), fetch: pb });
  await tick(60);
  const card = window.document.querySelector(".rule");
  assert.match(card.textContent, /Everyone at Acme accept messages from everyone at Acme as a known contact\. Locked for agents\./);
  [...window.document.querySelectorAll(".templates button")].find((b) => /Agents only/.test(b.textContent)).click();
  await tick(30);
  assert.deepEqual(pb.posted.map((r) => [r.owners, r.peers, r.admission]), [["agents", "/acme/*", "allow"], ["agents", "*", "block"]]);
});

test("Security shows the SSH switches only with agent_ssh, and saves them", async (t) => {
  const make = (features) => {
    const patches = [];
    const base = postbox({ features });
    const fn = (c) => {
      const p = new URL(c.url).pathname;
      if (p.endsWith("/domains")) return reply(200, { domains: [] });
      if (p.endsWith("/audit")) return reply(200, { entries: [] });
      if (p === `/v1/orgs/${ORG.id}` && c.method === "PATCH") { patches.push(c.body); return reply(200, { org: { ...ORG, settings: { ...ORG.settings, ...c.body.settings } } }); }
      return base(c);
    };
    fn.patches = patches;
    return fn;
  };
  const off = load(t, "app.html", { url: "https://enterprise.pigeonpost.dev/app/security", session: jwt(), fetch: make(["orgs", "enterprise_portal"]) });
  await tick(60);
  assert.doesNotMatch(off.$("#content").textContent, /SSH/);
  const f = make(["orgs", "enterprise_portal", "agent_ssh"]);
  const on = load(t, "app.html", { url: "https://enterprise.pigeonpost.dev/app/security", session: jwt(), fetch: f });
  await tick(60);
  const sw = [...on.window.document.querySelectorAll("#content input[role=switch]")].find((i) => /Allow SSH to agents/.test(i.getAttribute("aria-label")));
  assert.ok(sw, "the org switch is there");
  assert.ok(!sw.checked, "off by default");
  sw.checked = true;
  sw.dispatchEvent(new on.window.Event("change"));
  await tick(30);
  assert.deepEqual(plain(f.patches), [{ settings: { ssh_allowed: true } }]);
  assert.match(on.$("#content").textContent, /needs an owner's, admin's or agent admin's approval/);
});
