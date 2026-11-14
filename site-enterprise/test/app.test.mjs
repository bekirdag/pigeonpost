// The admin app on the shared shell, against a fake postbox and bridge: sign-in and the states
// without an organization, role-filtered navigation, the real name, the theme switch at the top
// left, Overview, People and invitations, rules, Security, Nests, Services, Packages, Billing and
// Support.
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
const LISTINGS = [
  { id: "lst_free", title: "Standup notes", summary: "Daily notes from your rooms.", status: "published", hosting: "nest", price: { amount_minor: 0, currency: "USD", term: "one_off" } },
  { id: "lst_paid", title: "Wodo AI HR", summary: "Research reports.", status: "published", hosting: "publisher", price_from: { amount_cents: 2500, currency: "USD", unit: "run" } },
  { id: "lst_soon", title: "Coming thing", status: "published", coming_soon: true },
];

function postbox({ roles = ["owner"], features = ["orgs", "enterprise_portal"], rules = [], mailbox = { handle: "/acme/bekir" }, orgs, nests, installs = [], profiles = [{ id: "00000000-0000-4000-8000-0000000000aa", legal_name: "Acme A.Ş.", tax_id: "1234567890" }], bridgeNests } = {}) {
  const posted = [];
  const sent = [];
  const fn = (c) => {
    const u = new URL(c.url, "https://enterprise.pigeonpost.dev");
    const p = u.pathname;
    if (c.method !== "GET") sent.push({ method: c.method, path: p, body: c.body });
    if (p.startsWith("/api/")) {
      const b = p.slice(4);
      if (b === "/v1/billing/invoices") return reply(200, { invoices: [{ id: "00000000-0000-4000-8000-000000000061", number: "PP-1", status: "open", total: "180.00", currency: "USD", issued_at: "2026-11-01" }] });
      if (b === "/v1/billing/profiles") return reply(200, { profiles });
      if (b === "/v1/nests" && u.searchParams.get("org_id") === ORG.id) return reply(200, bridgeNests || { org_id: ORG.id, subscriptions: [], prices: { open_currencies: ["USD"], plans: [{ code: "nest-team-usd", family: "nest", currency: "USD", package: "team", slots: 5, amount: "49.00", available: true }] } });
      return reply(404, { error: "not found" });
    }
    if (p === "/v1/capabilities") return reply(200, { features });
    if (p === "/v1/publishers" || p === "/v1/staff/me") return reply(403, { error: "forbidden" });
    if (p === "/v1/orgs") return reply(200, { orgs: orgs || [{ ...ORG, my_roles: roles }] });
    if (p === `/v1/orgs/${ORG.id}`) return reply(200, { org: ORG, me: { member_id: "mem_1", roles, mailbox } });
    if (p.endsWith("/members")) return reply(200, { members: MEMBERS });
    if (p.endsWith("/invites") && c.method === "POST") { posted.push(c.body); return reply(200, { results: c.body.invites.map((i, n) => ({ status: "invited", member_id: `mem_n${n}`, invite_id: `inv_n${n}`, handle: i.handle, link: `https://enterprise.pigeonpost.dev/invite#${"T".repeat(43)}`, expires_at: 1900000000 })) }); }
    if (p.endsWith("/rules") && c.method === "GET") return reply(200, { rules, revision: 1 });
    if (p.endsWith("/rules") && c.method === "POST") { posted.push(c.body); return reply(201, { rule: { id: "rul_1", ...c.body } }); }
    if (p.endsWith("/rooms")) return reply(200, { rooms: [{ id: "room_1", name: "General", member_count: 3, listed: true, auto_join: true }] });
    if (p.endsWith("/domains")) return reply(200, { domains: [] });
    if (p.endsWith("/audit")) return reply(200, { entries: [] });
    if (p === `/v1/orgs/${ORG.id}/billing`) return reply(200, { plan_code: "ent-30-yearly-usd", seats: 30, seats_used: 3, agent_limit: 300, agents_used: 1, status: "active", paid_through: 1900000000, source: "grant" });
    if (p === `/v1/orgs/${ORG.id}/nests` && c.method === "GET") return reply(200, nests || { nests: [], allowance: { slots: 0, used: 0 }, package: null });
    if (p === `/v1/orgs/${ORG.id}/nests` && c.method === "POST") return reply(201, { nest_id: "nst_1", handle: c.body.handle, state: "provisioning" });
    if (p === "/v1/nest-packages") return reply(200, { packages: [{ code: "team", name: "Team", slots: 5, sellable: true, model: "managed" }] });
    if (p === `/v1/orgs/${ORG.id}/nest-installations` && c.method === "GET") return reply(200, { data: installs, next_cursor: null });
    if (p === `/v1/orgs/${ORG.id}/nest-installations` && c.method === "POST") return reply(201, { data: { id: "ins_1", listing_id: c.body.listing_id, status: "active" } });
    if (p === "/v1/marketplace/listings") return reply(200, { data: LISTINGS, next_cursor: null });
    return reply(404, { error: "not_found" });
  };
  fn.posted = posted;
  fn.sent = sent;
  return fn;
}

const open = (t, page, opts = {}) => load(t, "app.html", { url: `https://enterprise.pigeonpost.dev/app#/${page}`, session: jwt(opts.claims || { name: "Ada Lovelace" }), fetch: opts.fetch || postbox(opts), ...(opts.load || {}) });
const navText = (window) => [...window.document.querySelectorAll(".pp-nav-item")].map((a) => a.textContent);
const main = (window) => window.document.getElementById("pp-main");
const h1 = (window) => main(window).querySelector("h1").textContent;
const button = (root, text) => [...root.querySelectorAll("button, a.pp-btn")].find((b) => b.textContent.trim() === text);
const sheet = (window) => window.document.querySelector(".sheet");
const shown = (window) => main(window).querySelector(".pp-view").getAttribute("data-page");
/// Wait (up to ~2 s of event-loop turns) for a condition, so a loaded machine does not fail a test.
const until = async (fn, n = 400) => { for (let i = 0; i < n; i++) { try { if (fn()) return true; } catch { /* not yet */ } await tick(5); } return false; };

test("signed out, the app asks for sign-in; without the flag it says coming soon", async (t) => {
  const out = load(t, "app.html", { url: "https://enterprise.pigeonpost.dev/app" });
  await tick();
  assert.match(out.$(".pp-signin-card h1").textContent, /Sign in to manage your organization/);
  assert.equal(out.$('.pp-signin-card a[href="/"]').textContent, "See packages");
  const off = load(t, "app.html", { url: "https://enterprise.pigeonpost.dev/app", session: jwt(), fetch: postbox({ features: ["orgs"] }) });
  await tick(60);
  assert.match(main(off.window).textContent, /Coming soon/);
  assert.deepEqual(navText(off.window), ["Setup", "Support"]);
});

test("a plain member is sent to the inbox; someone with no organization is offered the start flow", async (t) => {
  const member = open(t, "", { roles: [] });
  await tick(60);
  assert.match(main(member.window).textContent, /Your company workspace is in the inbox/);
  assert.equal(button(main(member.window), "Open your inbox").getAttribute("href"), "https://inbox.pigeonpost.dev");
  assert.deepEqual(navText(member.window), ["Setup", "Support"]);
  const none = open(t, "", { orgs: [] });
  await tick(60);
  assert.equal(h1(none.window), "No organization yet");
  assert.equal(button(main(none.window), "Create an organization").getAttribute("href"), "/start");
  assert.equal(main(none.window).querySelectorAll(".pp-stepper li").length, 3, "the start flow's three steps");
});

test("old /app/<page> links land on the same page; the default page is Overview", async (t) => {
  const old = load(t, "app.html", { url: "https://enterprise.pigeonpost.dev/app/people?org=org_abc123", session: jwt(), fetch: postbox() });
  await tick(60);
  assert.equal(old.window.location.pathname + old.window.location.search + old.window.location.hash, "/app?org=org_abc123#/people");
  assert.equal(h1(old.window), "People");
  const home = open(t, "");
  await tick(60);
  assert.equal(shown(home.window), "overview");
});

test("the user menu names the signed-in person, never a placeholder", async (t) => {
  const named = open(t, "people");
  await tick(60);
  assert.equal(named.$(".pp-user-name").textContent, "Ada Lovelace");
  assert.match(named.$("#pp-user-pop").textContent, /Ada Lovelace/);
  // No display name in the token: the member list's name for this member.
  const fromList = open(t, "people", { claims: { name: undefined, preferred_username: "bekir" } });
  await tick(60);
  assert.equal(fromList.$(".pp-user-name").textContent, "Bekir Dag");
  assert.doesNotMatch(fromList.window.document.body.textContent, /sample user/i);
});

test("the theme switch sits at the top left of the top bar, signed in and signed out", async (t) => {
  const { window, $ } = open(t, "people");
  await tick(60);
  const theme = $(".pp-top [data-theme-toggle]");
  assert.ok(theme.classList.contains("pp-theme-left"));
  assert.equal($(".pp-top-actions [data-theme-toggle]"), null);
  const kids = [...$(".pp-top").children].filter((n) => !n.classList.contains("pp-menu-btn"));
  assert.equal(kids[0], theme, "first in the top bar");
  assert.equal(theme.getAttribute("aria-label"), "Theme: System");
  theme.click();
  assert.equal(window.document.documentElement.getAttribute("data-theme"), "light");
  const out = load(t, "app.html", { url: "https://enterprise.pigeonpost.dev/app" });
  assert.ok(out.$(".pp-signin [data-theme-toggle]").classList.contains("pp-theme-left"));
});

test("navigation is grouped and role-filtered; the org sits above it; the platform switcher lists what the person has", async (t) => {
  const owner = open(t, "people", { features: ["orgs", "enterprise_portal", "nests", "marketplace", "org_api"] });
  await tick(60);
  assert.deepEqual(navText(owner.window), ["Overview", "Setup", "People", "Agents", "Rooms", "Nests", "Services", "Rules", "Security", "Packages", "Billing", "Organization", "API keys", "Webhooks", "Support"]);
  assert.deepEqual([...owner.window.document.querySelectorAll(".pp-nav-heading")].map((h) => h.textContent), ["Workspace", "Nests & services", "Policy", "Company", "Developers", "Help"]);
  assert.match(owner.$("#pp-nav-header").textContent, /Acme\/acme/);
  assert.deepEqual([...owner.window.document.querySelectorAll("#pp-switch-pop .pp-pop-item strong")].map((s) => s.textContent), ["Account", "Enterprise"]);

  const billingAdmin = open(t, "people", { roles: ["billing_admin"], features: ["orgs", "enterprise_portal", "nests", "marketplace"] });
  await tick(60);
  assert.deepEqual(navText(billingAdmin.window), ["Overview", "Setup", "Rooms", "Nests", "Services", "Packages", "Billing", "Support"]);
  assert.notEqual(h1(billingAdmin.window), "People", "a hidden page falls back");

  const people = open(t, "people", { roles: ["people_admin"] });
  await until(() => h1(people.window) === "People");
  const nav = navText(people.window);
  assert.ok(nav.includes("People") && nav.includes("Rules") && nav.includes("Rooms"));
  assert.ok(!nav.includes("Billing") && !nav.includes("Security") && !nav.includes("Agents") && !nav.includes("Packages"));
  people.window.PPE.app.go("billing");
  await until(() => people.window.location.hash !== "#/billing" && shown(people.window) === "overview");
  assert.equal(shown(people.window), "overview", "a hidden page falls back to the first allowed one");
  assert.deepEqual(plain(people.window.PPE.app.grantable()), []);
});

test("the switcher adds Partner and Management only when their probes answer", async (t) => {
  const base = postbox();
  const fetch = (c) => {
    const p = new URL(c.url).pathname;
    if (p === "/v1/publishers") return reply(200, { data: [{ id: "pub_1", role: "owner" }] });
    if (p === "/v1/staff/me") return reply(200, { data: { roles: ["support"] } });
    return base(c);
  };
  const { window } = open(t, "people", { fetch });
  await tick(60);
  assert.deepEqual([...window.document.querySelectorAll("#pp-switch-pop .pp-pop-item strong")].map((s) => s.textContent), ["Account", "Enterprise", "Partner", "Management"]);
  // A probe that fails never signs anyone out.
  const failing = open(t, "people", { fetch: (c) => (/\/v1\/(publishers|staff\/me)$/.test(new URL(c.url).pathname) ? reply(401, { error: "unauthorized" }) : base(c)) });
  await tick(60);
  assert.equal(failing.nav.length, 0);
  assert.equal(h1(failing.window), "People");
});

test("Overview shows seats, agents, rooms, package, open invoices, Nests and the setup checklist", async (t) => {
  const { window } = open(t, "overview", { features: ["orgs", "enterprise_portal", "nests"], nests: { nests: [{ nest_id: "nst_1", label: "Research", state: "running", role: "owner" }], allowance: { slots: 5, used: 1 }, package: { code: "team", slots: 5 } } });
  await tick(80);
  assert.equal(h1(window), "Welcome, Ada");
  const stats = Object.fromEntries([...main(window).querySelectorAll(".pp-stat")].map((s) => [s.querySelector(".pp-stat-label").textContent, s.querySelector(".pp-stat-value").textContent]));
  assert.equal(stats["Seats used"], "3 / 30");
  assert.equal(stats.Agents, "1 / 300");
  assert.equal(stats.Rooms, "1");
  assert.equal(stats.Package, "30 seats");
  assert.equal(stats["Open invoices"], "1");
  assert.equal(stats.Nests, "1 / 5");
  const items = [...main(window).querySelectorAll(".pp-checklist li")];
  assert.ok(items.length >= 5);
  assert.ok(items[0].classList.contains("is-done"), "the organization exists");
  const invite = items.find((li) => /Invite your team/.test(li.textContent));
  assert.ok(invite.classList.contains("is-done"), "more than one person");
  assert.ok(!items.find((li) => /Verify your domain/.test(li.textContent)).classList.contains("is-done"));
  assert.ok(main(window).querySelector('a.pp-stat[href="#/people"]'));
});

test("Setup walks the same steps as a stepper and opens the next undone one", async (t) => {
  const { window } = open(t, "setup");
  await tick(80);
  assert.equal(h1(window), "Setup");
  const steps = [...main(window).querySelectorAll(".pp-stepper li")];
  assert.ok(steps[0].classList.contains("is-done"));
  assert.equal(main(window).querySelector(".pp-stepper li[aria-current=step]").textContent.replace(/^\d+/, ""), "Verify your domain");
  button(main(window), "Add domain").click();
  assert.equal(sheet(window).querySelector("h2").textContent, "Add domain");
  assert.equal(button(main(window), "Start another organization").getAttribute("href"), "/start");
});

test("People shows seats, one table and a row per person", async (t) => {
  const { window, $ } = open(t, "people");
  await tick(60);
  assert.equal(h1(window), "People");
  assert.match($("#pp-main .pp-page-head p").textContent, /3 of 30 seats used/);
  assert.equal(main(window).querySelectorAll("table").length, 1);
  const rows = [...main(window).querySelectorAll("tbody tr")].map((r) => r.textContent);
  assert.equal(rows.length, 3);
  assert.match(rows[0], /Bekir Dag.*\/acme\/bekir.*Owner.*Active/);
  assert.match(rows[1], /Jane Doe.*Invited/);
  assert.match(rows[2], /People admin.*Suspended/);
  assert.equal(main(window).querySelectorAll(".pp-page-head .pp-btn-primary").length, 1, "one primary action");
});

test("inviting shows each link once with a copy action when invitation messages are off", async (t) => {
  const pb = postbox();
  const { window, $ } = open(t, "people", { fetch: pb });
  await tick(60);
  $("#pp-main .pp-page-head .pp-btn-primary").click();
  const inputs = sheet(window).querySelectorAll(".invite-row input");
  inputs[0].value = "John"; inputs[0].dispatchEvent(new window.Event("input"));
  inputs[1].value = "Smith"; inputs[2].value = "john@acme.test";
  assert.equal(inputs[3].value, "john", "the address follows the first name");
  const roleOptions = [...sheet(window).querySelectorAll("select option")].map((o) => o.value);
  assert.deepEqual(roleOptions, ["", "admin", "people_admin", "agent_admin", "billing_admin"]);
  button(sheet(window), "Send invitations").click();
  await tick(30);
  assert.deepEqual(pb.posted[0], { invites: [{ first_name: "John", last_name: "Smith", contact: "john@acme.test", handle: "/acme/john", roles: [] }] });
  assert.match(sheet(window).textContent, /Shown once/);
  assert.match(sheet(window).querySelector(".secret code").textContent, /^https:\/\/enterprise\.pigeonpost\.dev\/invite#T+$/);
});

test("the owner without a company address is asked to choose one", async (t) => {
  const { window } = open(t, "people", { mailbox: null });
  await tick(60);
  assert.match(main(window).textContent, /Choose your address in Acme/);
});

test("the rules page adds a template and draws sentence editors", async (t) => {
  const pb = postbox({ rules: [{ id: "rul_9", owners: "/acme/*", peers: "/acme/*", admission: "allow", known: true, autonomy: null, verbs: null, locked: true, priority: 100 }] });
  const { window } = open(t, "rules", { fetch: pb });
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
      if (p === `/v1/orgs/${ORG.id}` && c.method === "PATCH") { patches.push(c.body); return reply(200, { org: { ...ORG, settings: { ...ORG.settings, ...c.body.settings } } }); }
      return base(c);
    };
    fn.patches = patches;
    return fn;
  };
  const off = open(t, "security", { fetch: make(["orgs", "enterprise_portal"]) });
  await tick(60);
  assert.doesNotMatch(main(off.window).textContent, /SSH/);
  const f = make(["orgs", "enterprise_portal", "agent_ssh"]);
  const on = open(t, "security", { fetch: f });
  await tick(60);
  const sw = [...main(on.window).querySelectorAll("input[role=switch]")].find((i) => /Allow SSH to agents/.test(i.getAttribute("aria-label")));
  assert.ok(sw, "the org switch is there");
  assert.ok(!sw.checked, "off by default");
  sw.checked = true;
  sw.dispatchEvent(new on.window.Event("change"));
  await tick(30);
  assert.deepEqual(plain(f.patches), [{ settings: { ssh_allowed: true } }]);
  assert.match(main(on.window).textContent, /needs an owner's, admin's or agent admin's approval/);
});

// ---- Nests and Services ---------------------------------------------------------------------------

test("Nests lists the company's Nests and, without a package, offers one to buy through the isolated checkout", async (t) => {
  const { window } = open(t, "nests", { features: ["orgs", "enterprise_portal", "nests"] });
  await tick(80);
  assert.equal(h1(window), "Nests");
  assert.match(main(window).textContent, /No company Nests yet/);
  const create = button(main(window), "Create a company Nest");
  assert.ok(create.disabled, "no package, no free slot");
  const tile = main(window).querySelector('.pp-tile[data-package="team"]');
  assert.match(tile.textContent, /Team.*5 Nests.*\$49 per month/);
  button(tile, "Buy for the company").click();
  const pending = JSON.parse(window.sessionStorage.getItem("ppe_nest_checkout"));
  assert.equal(pending.org_id, ORG.id);
  assert.equal(pending.plan_code, "nest-team-usd");
  assert.equal(pending.billing_profile_id, "00000000-0000-4000-8000-0000000000aa");
  assert.match(pending.intent_id, /^[a-f0-9]{32}$/);
});

test("Nests without billing details shows the gate; with a free slot an admin creates one", async (t) => {
  const noProfile = open(t, "nests", { features: ["orgs", "enterprise_portal", "nests"], profiles: [] });
  await tick(80);
  const gate = main(noProfile.window).querySelector(".pp-gate");
  assert.match(gate.textContent, /Add billing details first/);
  assert.equal(gate.querySelector("a").getAttribute("href"), "#/billing/details");
  assert.ok(main(noProfile.window).querySelector(".pp-gated-area").hasAttribute("inert"));

  const pb = postbox({ features: ["orgs", "enterprise_portal", "nests"], nests: { nests: [], allowance: { slots: 5, used: 0 }, package: { code: "team", slots: 5 } },
    bridgeNests: { org_id: ORG.id, subscriptions: [{ id: "sub_1", plan_code: "nest-team-usd", currency: "USD", package: "team", buyer: true, provider_status: "ACTIVE" }], prices: { plans: [] } } });
  const { window } = open(t, "nests", { fetch: pb, features: ["orgs", "enterprise_portal", "nests"] });
  await tick(80);
  assert.match(main(window).textContent, /Company web Nest package/);
  button(main(window), "Create a company Nest").click();
  await tick(20);
  const s = sheet(window);
  s.querySelectorAll("input")[0].value = "research";
  s.querySelectorAll("input")[1].value = "Research";
  button(s, "Create").click();
  await tick(30);
  assert.deepEqual(plain(pb.sent.find((x) => x.path === `/v1/orgs/${ORG.id}/nests`)), { method: "POST", path: `/v1/orgs/${ORG.id}/nests`, body: { handle: "/acme/research", label: "Research" } });
});

test("Services lists installs, installs a free listing for the company and explains paid ones", async (t) => {
  const pb = postbox({ features: ["orgs", "enterprise_portal", "marketplace"], installs: [{ id: "ins_0", listing_id: "lst_old", label: "Old helper", status: "active", agent_address: "/acme/helper", role: "admin", created_at: 1800000000 }] });
  const { window } = open(t, "services", { fetch: pb, features: ["orgs", "enterprise_portal", "marketplace"] });
  await tick(80);
  assert.equal(h1(window), "Services");
  assert.match(main(window).querySelector("tbody").textContent, /Old helper.*\/acme\/helper.*Active/);
  const free = main(window).querySelector('.pp-tile[data-listing="lst_free"]');
  assert.match(free.textContent, /Free/);
  const paid = main(window).querySelector('.pp-tile[data-listing="lst_paid"]');
  assert.match(paid.textContent, /\$25 per run/);
  assert.ok(!button(paid, "Install for the company"), "a paid listing is not installed for the company here");
  assert.match(main(window).querySelector('.pp-tile[data-listing="lst_soon"]').textContent, /Coming soon/);
  assert.match(main(window).textContent, /run from a member's own account/);
  button(free, "Install for the company").click();
  await tick(20);
  const s = sheet(window);
  s.querySelectorAll("input")[0].value = "standup";
  button(s, "Install").click();
  await tick(30);
  assert.deepEqual(plain(pb.sent.find((x) => x.path.endsWith("/nest-installations")).body), { listing_id: "lst_free", handle: "/acme/standup", label: "Standup notes" });
  button(paid, "How to use it").click();
  assert.match(sheet(window).textContent, /member's own account/);
});

test("Nests and Services stay hidden without their server features", async (t) => {
  const { window } = open(t, "nests");
  await tick(60);
  assert.ok(!navText(window).includes("Nests") && !navText(window).includes("Services"));
  assert.equal(shown(window), "overview");
});

// ---- Support -----------------------------------------------------------------------------------

test("Support links the help centre, the support address with the organization, sales and status", async (t) => {
  const { window } = open(t, "support");
  await tick(60);
  assert.equal(h1(window), "Support");
  assert.equal(button(main(window), "Open the help centre").getAttribute("href"), "https://help.pigeonpost.dev");
  const mail = [...main(window).querySelectorAll("a")].find((a) => a.getAttribute("href").startsWith("mailto:"));
  assert.match(mail.getAttribute("href"), /^mailto:support@pigeonpost\.dev\?subject=Support%3A%20Acme%20\(%2Facme\)$/);
  assert.match(main(window).textContent, /org_abc123/);
  assert.ok(button(main(window), "Talk to sales"));
  // Everyone signed in reaches it, even without an organization to manage.
  const member = open(t, "support", { roles: [] });
  await tick(60);
  assert.equal(h1(member.window), "Support");
});

// ---- Packages and Billing ------------------------------------------------------------------------

function billingFetch({ roles = ["owner"], sub = {}, billing = {} } = {}) {
  const base = postbox({ roles });
  const bridgeCalls = [];
  const SUB = { subscription_id: "00000000-0000-4000-8000-000000000001", plan_code: "ent-30-yearly-usd", seats: 30, currency: "USD", interval: "year",
    status: "active", paid_through: 1900000000, next_payment_at: "2030-03-17T00:00:00Z", failed_orders: [], buyer: true, can_manage: true,
    seats_used: 3, agents_used: 1, pending_change: null,
    options: [5, 10, 30, 50].map((n) => ({ plan_code: `ent-${n}-yearly-usd`, seats: n, agent_limit: n * 10, current: n === 30, fits: n >= 5, when: n > 30 ? "now" : "next_period" })),
    ...sub };
  const fn = (c) => {
    const u = new URL(c.url, "https://enterprise.pigeonpost.dev");
    if (u.pathname.startsWith("/api/")) {
      const p = u.pathname.slice(4);
      bridgeCalls.push({ method: c.method, path: p, body: c.body });
      if (p === "/v1/billing/invoices") return reply(200, { invoices: [{ id: "00000000-0000-4000-8000-000000000061", number: "PP-1", status: "paid", total: "180.00", currency: "USD", issued_at: "2026-11-01" }] });
      if (p === "/v1/billing/profiles") return reply(200, { profiles: [{ id: "p1", account_type: "entity", entity_name: "Acme A.Ş.", tax_office: "Kadıköy", tax_id: "1234567890", billing_email: "billing@acme.test", address: { line1: "Moda Cad. 1", city: "İstanbul", country: "Türkiye" } }] });
      if (p === `/ent/orgs/${ORG.id}/subscription`) return reply(200, SUB);
      if (p === `/ent/orgs/${ORG.id}/subscription/change`) return reply(200, { ...SUB, effective: c.body.plan_code === "ent-50-yearly-usd" ? "now" : "next_period" });
      if (p === `/ent/orgs/${ORG.id}/subscription/cancel`) return reply(200, { ...SUB, status: "canceled" });
      if (p === "/v1/catalog") return reply(503, {});
      return reply(404, { error: "not found" });
    }
    if (u.pathname === `/v1/orgs/${ORG.id}/billing`) return reply(200, { plan_code: "ent-30-yearly-usd", seats: 30, seats_used: 3, agent_limit: 300, agents_used: 1,
      status: "active", paid_through: 1900000000, source: "iyzico", can_manage: true, ...billing });
    return base(c);
  };
  fn.bridgeCalls = bridgeCalls;
  return fn;
}

test("Billing loads invoices from the bridge's path and changes seats in the same period", async (t) => {
  const fetch = billingFetch();
  const { $, window } = open(t, "billing/invoices", { fetch });
  await tick(80);
  assert.equal(h1(window), "Billing");
  assert.ok(fetch.bridgeCalls.some((c) => c.path === "/v1/billing/invoices"), "invoices come from /v1/billing/invoices");
  assert.ok(!fetch.bridgeCalls.some((c) => c.path === "/v1/invoices"));
  assert.match(main(window).querySelector("tbody").textContent, /PP-1/);
  const change = $("#pp-main .pp-page-head .pp-btn-primary");
  assert.equal(change.tagName, "BUTTON", "Change plan opens a sheet, not the new-organization wizard");
  change.click();
  await tick(10);
  const pick = sheet(window).querySelector("select");
  assert.deepEqual([...pick.options].map((o) => o.value), ["ent-5-yearly-usd", "ent-10-yearly-usd", "ent-30-yearly-usd", "ent-50-yearly-usd"]);
  pick.value = "ent-50-yearly-usd";
  pick.dispatchEvent(new window.Event("change"));
  assert.match(sheet(window).querySelector("[role=status]").textContent, /More seats start now/);
  const save = button(sheet(window), "Change plan");
  assert.equal(save.disabled, false);
  save.click();
  await tick(40);
  const sent = fetch.bridgeCalls.find((c) => c.path.endsWith("/subscription/change"));
  assert.equal(sent.method, "POST");
  assert.equal(sent.body.plan_code, "ent-50-yearly-usd");
  assert.match(sent.body.intent_id, /^[a-f0-9]{32}$/);
});

test("Billing cancels through the bridge after a confirmation and sends card updates to the isolated checkout page", async (t) => {
  const fetch = billingFetch();
  const { window } = open(t, "billing", { fetch });
  await tick(80);
  button(main(window), "Cancel plan").click();
  await tick(10);
  assert.match(sheet(window).textContent, /Cancel the plan\? Nothing renews/);
  assert.equal(fetch.bridgeCalls.filter((c) => c.path.endsWith("/cancel")).length, 0, "nothing until confirmed");
  button(sheet(window), "Cancel plan").click();
  await tick(60);
  const sent = fetch.bridgeCalls.find((c) => c.path.endsWith("/subscription/cancel"));
  assert.equal(sent.method, "POST");
  const again = open(t, "billing", { fetch: billingFetch() });
  await tick(80);
  button(main(again.window), "Update card").click();
  const card = JSON.parse(again.window.sessionStorage.getItem("ppe_ent_card"));
  assert.equal(card.org_id, ORG.id);
});

test("Billing shows billing details and explains that credits are personal", async (t) => {
  const details = open(t, "billing/details", { fetch: billingFetch() });
  await tick(80);
  assert.match(main(details.window).textContent, /Acme A\.Ş\..*Kadıköy.*1234567890.*billing@acme\.test/);
  const credits = open(t, "billing/credits", { fetch: billingFetch() });
  await tick(80);
  assert.match(main(credits.window).textContent, /Organizations don't have a shared wallet yet/);
  assert.equal(main(credits.window).querySelector(".pp-tab.is-active").textContent, "Credits");
});

test("Billing is read-only for a manager who did not buy the plan, and for granted plans", async (t) => {
  const notBuyer = billingFetch({ sub: { buyer: false } });
  const a = open(t, "billing", { fetch: notBuyer });
  await tick(80);
  assert.equal(a.$("#pp-main .pp-page-head .pp-btn-primary"), null);
  assert.match(main(a.window).textContent, /Only the person who bought this plan/);
  const granted = billingFetch({ billing: { source: "grant" } });
  const b = open(t, "billing", { fetch: granted });
  await tick(80);
  assert.ok(!granted.bridgeCalls.some((c) => c.path.includes("/subscription")), "no subscription lookup for a granted plan");
  assert.match(main(b.window).textContent, /isn't paid by card here/);
});

test("Packages shows every seat stop with the current one marked, and switching opens the change sheet", async (t) => {
  const { window } = open(t, "packages", { fetch: billingFetch() });
  await tick(80);
  assert.equal(h1(window), "Packages");
  const current = main(window).querySelector(".pp-tile.is-current");
  assert.equal(current.getAttribute("data-seats"), "30");
  assert.match(current.textContent, /30 seats.*\$1,800 per year.*Current plan/);
  button(main(window).querySelector('.pp-tile[data-seats="50"]'), "Switch").click();
  await tick(10);
  assert.equal(sheet(window).querySelector("select").value, "ent-50-yearly-usd");
});
