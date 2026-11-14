// The account platform on the shared shell: sign-in screen, navigation without partner or staff
// pages, the platform switcher from the context probes, old links and every page's content.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { boot, tick, fixtures, jwt } from "./helpers.mjs";

const NAV = ["overview", "inboxes", "handles", "nests", "store", "purchases", "wallet", "plan", "billing", "invoices", "security", "privacy", "support"];

test("signed out: the sign-in screen; a #delete-account link explains why to sign in", async (t) => {
  const a = boot(t, { session: null });
  await a.started;
  assert.equal(a.$(".pp-signin-card h1").textContent, "Your Pigeonpost account");
  assert.equal(a.$$(".pp-signin-actions button").length, 2);
  const b = boot(t, { session: null, url: "https://account.pigeonpost.dev/#delete-account" });
  await b.started;
  assert.equal(b.$(".pp-signin-card h1").textContent, "Delete your Pigeonpost account");
  assert.equal(b.$$(".pp-signin-actions button").length, 1, "no registration on the deletion screen");
});

test("navigation: the end-user pages in groups, nothing partner or staff, the real name, Become a partner", async (t) => {
  const a = boot(t);
  await a.started; await tick();
  assert.deepEqual(a.$$(".pp-nav-item").map((n) => n.getAttribute("data-page")), NAV);
  assert.deepEqual(a.$$(".pp-nav-heading").map((n) => n.textContent), ["Messaging", "Nests & services", "Billing", "Account"]);
  const nav = a.$("#pp-side").textContent;
  for (const gone of ["My business", "My apps", "My analytics", "Back office", "Staff", "Partner support"]) assert.ok(!nav.includes(gone), gone);
  assert.ok(!a.document.querySelector('a[href="/staff"]'));
  assert.equal(a.$(".pp-user-name").textContent, "Ada Lovelace");
  const partner = a.$$("#pp-user-pop a").find((x) => x.textContent === "Become a partner");
  assert.equal(partner.getAttribute("href"), "https://partner.pigeonpost.dev/");
  assert.equal(a.$("#pp-switch-pop"), null, "no switcher for a person with only this platform");
  assert.equal(a.$('.pp-nav-item[data-page="overview"]').getAttribute("aria-current"), "page");
  assert.match(a.$("h1").textContent, /, Ada$/);
});

test("context: the switcher lists Partner, Enterprise and Management from the probes", async (t) => {
  const a = boot(t, { routes: fixtures({
    "GET /v1/publishers": { data: [{ id: "pub_1", status: "applied" }] },
    "GET /v1/orgs": { orgs: [{ id: "org_abcdef", name: "Acme", my_roles: ["billing_admin"] }] },
    "GET /v1/staff/me": { data: { roles: ["support"] } },
  }) });
  await a.started; await tick();
  assert.deepEqual(a.$$("#pp-switch-pop .pp-pop-item strong").map((s) => s.textContent), ["Account", "Enterprise", "Partner", "Management"]);
});

test("context: failing probes do not break the page", async (t) => {
  const a = boot(t, { routes: fixtures({
    "GET /v1/publishers": () => { throw new Error("network"); },
    "GET /v1/orgs": [500, { error: "store_error" }],
    "GET /v1/staff/me": [401, { error: "unauthorized" }],
    "GET /v1/capabilities": [503, {}],
  }) });
  await a.started; await tick();
  assert.equal(a.$$(".pp-nav-item").length, NAV.length);
  assert.ok(a.w.PP.signedIn(), "a probe's 401 does not sign the person out");
  assert.equal(a.nav.length, 0, "no redirect to the realm");
  await a.go("#/wallet");
  assert.match(a.text(), /Credits aren't open on this account yet/, "features off: an explained empty state");
});

test("old links land on the new pages; partner and staff sections leave for their platforms", async (t) => {
  for (const [hash, page] of [["#credits", "wallet"], ["#get-handle", "handles"], ["#mailboxes", "inboxes"], ["#keys", "inboxes"], ["#products", "nests"], ["#services", "store"], ["#delete-account", "privacy"]]) {
    const a = boot(t, { url: `https://account.pigeonpost.dev/${hash}` });
    await a.started; await tick();
    assert.equal(a.$(".pp-view").getAttribute("data-page"), page, hash);
  }
  const l = boot(t, { url: "https://account.pigeonpost.dev/?listing=lst_tr" });
  await l.started; await tick();
  assert.equal(l.w.location.hash, "#/store/services/lst_tr");
  assert.equal(l.w.location.search, "");
  const d = boot(t, { url: "https://account.pigeonpost.dev/#delete-account" });
  await d.started; await tick();
  assert.equal(d.w.location.hash, "#/privacy/delete");
  const p = boot(t, { url: "https://account.pigeonpost.dev/#business" });
  await p.started; await tick();
  assert.deepEqual(p.nav, ["https://partner.pigeonpost.dev/"]);
  const s = boot(t, { url: "https://account.pigeonpost.dev/#staff" });
  await s.started; await tick();
  assert.deepEqual(s.nav, ["https://management.pigeonpost.dev/"]);
});

const CONTENT = {
  overview: [/Inboxes/, /Credit balance/, /\$25\.00/, /Get started/, /Turn on two-factor/],
  inboxes: [/\/ada\/agent1/, /k_def456@pigeonpost\.dev/, /Name it/],
  "inboxes/keys": [/ppk_12ab…/, /Revoke/],
  handles: [/\/ada/, /App Store/, /Manage in App Store/, /\/lovelace/],
  "handles/get": [/Find your name/, /Register it/],
  nests: [/Research/, /Ready/, /Pigeonpost AI/, /1 of 3/],
  "store/inbox": [/A name of your own/, /\$8\.00 per name per year/],
  "store/nests": [/Pigeonpost AI/, /\$10\.00 a month/],
  "store/services": [/Find a service/, /Wodo AI HR/, /Translator/, /Featured/, /Coming from Pigeonpost/],
  "store/services/lst_tr": [/Translator/, /\$5\.00 a month/, /Buy ·/],
  "store/services/lst_hr": [/Hosted by the publisher/, /Price\$25\.00 per run/, /Your credit: \$25\.00/],
  purchases: [/Wodo AI HR/, /report\.pdf/, /\$25\.00 paid/],
  "purchases/installed": [/No services installed/],
  "purchases/licences": [/No paid services/],
  wallet: [/Available/, /\$25\.00/, /Refunds/, /14 days/, /Bought credit/, /Paid for a run/],
  plan: [/2 names/, /Active/, /Change plan/, /Update card/, /Cancel plan/, /Meeting summaries/],
  billing: [/Ada Lovelace/, /Complete/, /1 Analytical St/],
  "billing/edit": [/Account type/, /Legal name/, /Save billing details/],
  invoices: [/PP-2026-001/, /Paid/, /PDF/],
  security: [/Two-factor sign-in/, /Set up two-factor authentication/, /ada@example\.com/],
  privacy: [/Your data/, /Delete your account/],
  "privacy/delete": [/Account: ada@example\.com/, /Type DELETE/],
  support: [/help\.pigeonpost\.dev/, /support@pigeonpost\.dev/],
};

test("every page renders its content", async (t) => {
  const a = boot(t);
  await a.started; await tick();
  for (const [route, wants] of Object.entries(CONTENT)) {
    await a.go(`#/${route}`);
    await tick(40);
    assert.equal(a.$(".pp-empty-error"), null, `${route}: no error state`);
    for (const w of wants) assert.match(a.text(), w, route);
    assert.ok(a.$(".pp-view h1"), `${route}: has a heading`);
    assert.ok(!a.$("#pp-crumbs").textContent.includes("null"), `${route}: breadcrumbs`);
  }
});

const EMPTY = fixtures({
  "GET /v1/identities": { identities: [] },
  "GET /v1/api-keys": { keys: [] },
  "GET /api/v1/me/overview": { handles: [], billingProfiles: [], invoices: [], cloud: { plan: { web_plan: { capacity: 0, used: 0 } }, subscriptions: [] } },
  "GET /v1/nests": { nests: [], allowance: { slots: 0, used: 0, duplicates: [] }, setups: [] },
  "GET /api/v1/credits/history": { entries: [], next_cursor: null },
  "GET /v1/marketplace/listings": { data: [], next_cursor: null },
  "GET /v1/marketplace/hosted-jobs": { data: [] },
});

test("empty account: every list explains itself and offers the next step", async (t) => {
  const a = boot(t, { routes: EMPTY });
  await a.started; await tick();
  const cases = { inboxes: /No inboxes yet/, "inboxes/keys": /No connector keys yet/, handles: /No handles yet/, nests: /No Nests yet/, purchases: /Nothing run yet/, wallet: /No credit bought or spent yet/, invoices: /No invoices yet/, "store/services": /Nothing is listed yet/, plan: /No Pigeonpost Cloud plan yet/ };
  for (const [route, want] of Object.entries(cases)) {
    await a.go(`#/${route}`);
    await tick(40);
    assert.match(a.text(), want, route);
  }
  await a.go("#/overview");
  assert.match(a.text(), /3 of 3 steps left/);
  await a.go("#/billing");
  assert.match(a.text(), /Add billing details/);
  assert.ok(a.$("#acc-billing-form"), "no profile: the form, straight away");
});

test("a page whose load fails shows the error with a retry, not a blank page", async (t) => {
  const a = boot(t, { routes: fixtures({ "GET /api/v1/me/overview": [502, { error: "Billing is not answering", code: "billing_unavailable" }] }) });
  await a.started; await tick();
  await a.go("#/invoices");
  assert.ok(a.$(".pp-empty-error"));
  assert.match(a.text(), /Try again/);
});

test("two-factor: an mfa sign-in shows it on; set-up asks the realm for CONFIGURE_TOTP", async (t) => {
  const a = boot(t, { session: jwt({ acr: "mfa" }), url: "https://account.pigeonpost.dev/#/security" });
  await a.started; await tick();
  assert.match(a.text(), /Two-factor is on/);
  a.$("#acc-2fa").click();
  await tick();
  const u = new URL(a.nav.at(-1));
  assert.equal(u.searchParams.get("kc_action"), "CONFIGURE_TOTP");
  assert.equal(JSON.parse(a.w.localStorage.getItem("pp_pkce")).returnTo, "/#/security");
});

test("static: no inline script or style, no HTML from strings", () => {
  const root = new URL("../", import.meta.url);
  const html = readFileSync(new URL("index.html", root), "utf8");
  assert.ok(!/<script(?![^>]*\bsrc=)[^>]*>/.test(html), "no inline script");
  assert.ok(!/<style|style="/.test(html), "no inline style");
  const files = ["lib.js", "boot.js", "billing/checkout.js", ...readdirSync(new URL("pages/", root)).map((f) => `pages/${f}`)];
  for (const f of files) {
    const src = readFileSync(new URL(f, root), "utf8");
    assert.ok(!/\.innerHTML\s*=|insertAdjacentHTML|outerHTML\s*=/.test(src), `${f}: no HTML strings`);
    assert.ok(!/setAttribute\(\s*["']style|style:\s*["`][a-z-]+\s*:/.test(src), `${f}: no style attributes`);
  }
});
