// management.pigeonpost.dev: who sees what, step-up in place, the application decision, the
// credit adjustment cap, list states, the no-access screen and the live support inbox.
import test from "node:test";
import assert from "node:assert/strict";
import { boot, tick, reply, stepUp, ME, navKeys, go, type, submit, read, scripts } from "./helpers.mjs";

const EMPTY = reply(200, { data: [], next_cursor: null });
const DASH = reply(200, { data: { counts: { signups_today: 3, signups_7d: 20, accounts: 120, publisher_applications: 2, open_reviews: 1, open_reports: 0 }, alerts: [], as_of: "2026-11-14T10:00:00Z" } });
const base = (roles, extra = {}) => ({ "GET /v1/staff/me": ME(roles), "GET /v1/staff/dashboard": DASH, "GET /v1/publishers": reply(200, { data: [] }), "GET /v1/orgs": reply(200, { orgs: [] }), ...extra });

test("not staff: 403 staff_required shows the Staff only screen with a way back to the account", async (t) => {
  const { w, $ } = boot(t, { routes: { "GET /v1/staff/me": reply(403, { error: "staff_required" }) } });
  await tick();
  assert.equal($(".pp-empty h3").textContent, "Staff only");
  assert.equal($(".pp-empty a").getAttribute("href"), "https://account.pigeonpost.dev/");
  assert.deepEqual(navKeys(w), [], "no navigation at all");
});

test("signed out: the sign-in screen offers no registration", async (t) => {
  const { $$ } = boot(t, { session: null });
  await tick();
  assert.deepEqual($$(".pp-signin-actions button").map((b) => b.textContent), ["Sign in"]);
});

test("role filtering: finance sees money, reviewer sees reviews, owner sees staff & roles", async (t) => {
  const finance = boot(t, { routes: base(["finance"]) });
  await tick();
  const f = navKeys(finance.w);
  for (const k of ["overview", "plans", "credits", "payouts", "invoices", "partner-analytics", "businesses"]) assert.ok(f.includes(k), `finance sees ${k}`);
  for (const k of ["reviews", "applications", "staff", "audit", "accounts", "support"]) assert.ok(!f.includes(k), `finance does not see ${k}`);

  const reviewer = boot(t, { routes: base(["reviewer"]) });
  await tick();
  const r = navKeys(reviewer.w);
  for (const k of ["applications", "businesses", "reviews", "listings", "jobs", "releases"]) assert.ok(r.includes(k), `reviewer sees ${k}`);
  for (const k of ["credits", "staff", "accounts"]) assert.ok(!r.includes(k), `reviewer does not see ${k}`);
  assert.equal(reviewer.$('[data-page="applications"] .pp-nav-count').textContent, "2", "open applications counted");

  const owner = boot(t, { routes: base(["owner"]) });
  await tick();
  const o = navKeys(owner.w);
  for (const k of ["staff", "audit", "accounts", "handles", "mailboxes", "enterprises", "support", "flags"]) assert.ok(o.includes(k), `owner sees ${k}`);
  for (const k of ["credits", "reviews"]) assert.ok(!o.includes(k), `owner without other roles does not see ${k}`);

  // Owner role without being the platform owner: no Staff & roles.
  const notOwner = boot(t, { routes: base(["owner"], { "GET /v1/staff/me": ME(["owner"], { platform_owner: false }) }) });
  await tick();
  assert.ok(!navKeys(notOwner.w).includes("staff"));

  // A forbidden route falls back to the first allowed page.
  await go(finance.w, "#/staff");
  assert.equal(finance.w.location.hash, "#/overview");
  // Soon pages are tagged in the navigation.
  assert.equal(finance.$('[data-page="payouts"] .pp-nav-tag').textContent, "soon");
});

test("step-up: a refusal shows Confirm it's you in place, calls PP.stepUp, and resumes on this screen", async (t) => {
  const { w, $, nav } = boot(t, { url: "https://management.pigeonpost.dev/#/accounts", routes: base(["support"], { "GET /v1/staff/accounts": stepUp() }) });
  await tick();
  const card = $(".pm-stepup");
  assert.ok(card, "step-up card drawn");
  assert.match(card.textContent, /Confirm it's you/);
  assert.equal(nav.length, 0, "nothing left the page by itself");
  const seen = [];
  const real = w.PP.stepUp;
  w.PP.stepUp = (challenge, resume) => { seen.push({ challenge, resume }); return real(challenge, resume); };
  card.querySelector("button").click();
  await tick();
  assert.equal(seen.length, 1);
  assert.equal(seen[0].challenge.acr_values, "gold urn:pigeonpost:acr:mfa");
  assert.equal(seen[0].challenge.max_age, 600);
  assert.deepEqual(JSON.parse(JSON.stringify(seen[0].resume)), { name: "mgmt-return", payload: { hash: "#/accounts" } });
  const to = new URL(nav[0]);
  assert.equal(to.searchParams.get("acr_values"), "gold urn:pigeonpost:acr:mfa");
  assert.equal(JSON.parse(w.localStorage.getItem("pp_pkce")).returnTo, "/#/accounts");

  // Back from the realm: the pending resume brings the person to the same screen.
  const back = boot(t, { url: "https://management.pigeonpost.dev/", routes: base(["support"], { "GET /v1/staff/accounts": EMPTY }), setup: (win) => win.localStorage.setItem("pp_resume", JSON.stringify({ name: "mgmt-return", payload: { hash: "#/accounts" }, at: Date.now() })) });
  await tick();
  assert.equal(back.w.location.hash, "#/accounts");
  assert.equal(back.$("h1").textContent, "Accounts");
  assert.match(back.$("#pp-toasts").textContent, /back where you were/);
  assert.equal(back.w.localStorage.getItem("pp_resume"), null, "resume is used once");
});

const DOSSIER = {
  id: "pub_acme", state: "submitted", revision: 4, kind: "company", country: "TR", kyc_status: "not_started", submitted_at: 1763000000, updated_at: 1763100000,
  details: { legal_name: "Acme Teknoloji A.Ş.", registration_number: "123", tax_id: "1234567890", address: "İstanbul", bank_name: "Garanti", account_holder: "Acme", iban: "TR330006100519786457841326", public_name: "Acme", public_summary: "Tools" },
  documents: [{ id: "doc1", category: "registration", filename: "faaliyet.pdf", size: 20480, created_at: 1763000000 }],
  requested_documents: [], history: [{ actor: "acct_acme", event: "submitted", revision: 4, note: null, created_at: 1763000000 }],
};

test("applications: the queue filters by status; the detail masks bank numbers and posts the decision body", async (t) => {
  const posted = [];
  const { w, $, $$, calls } = boot(t, { url: "https://management.pigeonpost.dev/#/applications", routes: base(["reviewer"], {
    "GET /v1/staff/publisher-dossiers": (req) => reply(200, { data: req.query.status === "submitted" ? [{ id: "pub_acme", display_name: "Acme", state: "submitted", revision: 4, kind: "company", country: "TR", updated_at: "2026-11-14T09:00:00Z", kyc_status: "not_started", conflict: false }] : [], total: req.query.status === "submitted" ? 1 : 0, next_offset: null }),
    "GET /v1/staff/publisher-dossiers/pub_acme": reply(200, { data: DOSSIER }),
    "GET /v1/staff/roles": reply(200, { data: [{ id: "r1", account_id: "acct_me", role: "reviewer" }] }),
    "POST /v1/staff/publisher-dossiers/pub_acme/review": (req) => { posted.push(req.body); return reply(201, { data: { ...DOSSIER, state: req.body.decision } }); },
  }) });
  await tick();
  assert.match($("tbody").textContent, /Acme/);
  $$(".pm-chip").find((c) => c.dataset.value === "approved").click();
  await tick();
  assert.equal(calls.filter((c) => c.path === "/v1/staff/publisher-dossiers").at(-1).query.status, "approved");
  assert.equal($(".pp-empty h3").textContent, "No applications match");

  await go(w, "#/applications/pub_acme");
  assert.equal($("h1").textContent, "Acme");
  const text = $(".pm-detail").textContent;
  assert.ok(!text.includes("TR330006100519786457841326"), "IBAN is not shown in full");
  assert.ok(!text.includes("1234567890"), "tax id is not shown in full");
  assert.match(text, /••••1326/);
  assert.match(text, /Single-person approval/, "the only reviewer is told so");
  assert.ok($(".pp-checklist"), "checklist drawn");

  // Ask for more papers: needs at least one category, then posts the exact body.
  const decision = $(".pm-decision");
  [...decision.querySelectorAll(".pm-chip")].find((c) => c.dataset.value === "needs_information").click();
  const reason = decision.querySelector("textarea");
  type(w, reason, "Please upload your vergi levhası.");
  submit(w, decision.querySelector("form"));
  await tick();
  assert.equal(posted.length, 0, "no category picked yet");
  assert.match(decision.textContent, /Pick at least one kind of paper/);
  decision.querySelector('input[value="tax"]').checked = true;
  submit(w, decision.querySelector("form"));
  await tick();
  assert.deepEqual(posted[0], { revision: 4, decision: "needs_information", reason: "Please upload your vergi levhası.", requested_documents: ["tax"] });
});

test("applications: approving asks for confirmation first", async (t) => {
  const posted = [];
  const { w, $ } = boot(t, { url: "https://management.pigeonpost.dev/#/applications/pub_acme", routes: base(["reviewer"], {
    "GET /v1/staff/publisher-dossiers/pub_acme": reply(200, { data: { ...DOSSIER, state: "in_review" } }),
    "GET /v1/staff/roles": reply(200, { data: [{ account_id: "a", role: "reviewer" }, { account_id: "b", role: "reviewer" }] }),
    "POST /v1/staff/publisher-dossiers/pub_acme/review": (req) => { posted.push(req.body); return reply(201, { data: DOSSIER }); },
  }) });
  await tick();
  const decision = $(".pm-decision");
  assert.doesNotMatch(decision.textContent, /Single-person/, "two reviewers: no single-person note");
  [...decision.querySelectorAll(".pm-chip")].find((c) => c.dataset.value === "approved").click();
  type(w, decision.querySelector("textarea"), "All papers check out.");
  submit(w, decision.querySelector("form"));
  await tick();
  assert.equal(posted.length, 0, "waiting for the confirm sheet");
  [...w.document.querySelectorAll(".sheet button")].find((b) => b.textContent === "Approve").click();
  await tick();
  assert.deepEqual(posted[0], { revision: 4, decision: "approved", reason: "All papers check out.", requested_documents: [] });
});

test("credits: the adjustment cap ($10,000 either way) is enforced before anything is sent", async (t) => {
  const posted = [];
  const { w, $, $$ } = boot(t, { url: "https://management.pigeonpost.dev/#/credits", routes: base(["finance"], {
    "POST /v1/staff/credits/adjustments": (req) => { posted.push(req.body); return reply(201, { data: { id: "adj1" } }); },
  }) });
  await tick();
  $$(".pp-tab").find((b) => b.textContent.startsWith("Adjust")).click();
  await tick();
  const form = $(".pm-form-grid");
  type(w, form.querySelector('[name="owner_id"]'), "acct_buyer");
  type(w, form.querySelector('[name="reason"]'), "Goodwill for the outage");
  form.querySelector('[name="confirm"]').checked = true;
  for (const bad of ["10000.01", "-20000", "0"]) {
    type(w, form.querySelector('[name="amount"]'), bad);
    submit(w, form);
    await tick();
    assert.equal(posted.length, 0, `${bad} is refused`);
  }
  assert.match(form.textContent, /non-zero|At most \$10,000/);
  type(w, form.querySelector('[name="amount"]'), "-10000");
  submit(w, form);
  await tick();
  [...w.document.querySelectorAll(".sheet button")].find((b) => b.textContent === "Apply adjustment").click();
  await tick();
  assert.equal(posted.length, 1);
  assert.equal(posted[0].amount_cents, -1000000);
  assert.equal(posted[0].owner_kind, "account");
  assert.equal(posted[0].owner_id, "acct_buyer");
  assert.match(posted[0].idempotency_key, /^adj-/);
  assert.equal(w.PPM.adjustCents("10000.001").cents, 1000000, "rounds to cents first");
  assert.ok(w.PPM.adjustCents("10000.01").error);
});

test("lists: empty and error states, with a retry that recovers", async (t) => {
  let fail = true;
  const { w, $ } = boot(t, { url: "https://management.pigeonpost.dev/#/accounts", routes: base(["support"], {
    "GET /v1/staff/accounts": () => (fail ? reply(500, { error: "internal", request_id: "req_42" }) : reply(200, { data: [], next_cursor: null })),
  }) });
  await tick();
  const err = $(".pp-empty-error");
  assert.ok(err);
  assert.match(err.textContent, /req_42/);
  fail = false;
  err.querySelector("button").click();
  await tick();
  assert.equal($(".pp-empty h3").textContent, "No accounts yet");
  void w;
});

test("account detail: tabs show purchases, and push tokens are masked", async (t) => {
  const { w, $, $$ } = boot(t, { url: "https://management.pigeonpost.dev/#/accounts/acct_1", routes: base(["support"], {
    "GET /v1/staff/accounts/acct_1": reply(200, { data: { id: "acct_1", created_at: "2026-10-01T00:00:00Z", sign_in_identities: [{ provider: "realm", login: "ada@example.com" }], handles: [{ namespace: "ada", source: "apple" }], mailboxes: [], devices: [{ platform: "ios", push_token: "abcdef0123456789abcdef" }], entitlements: [{ plan_code: "cloud_pro", source: "iyzico", status: "active" }], subscriptions: { apple: [], google: [], nest_store: [] }, nests: [], orgs: [], recent_audit: [] } }),
  }) });
  await tick();
  assert.match($(".pp-kv").textContent, /ada@example.com/);
  $$(".pp-tab").find((b) => b.textContent.startsWith("Purchases")).click();
  assert.match($(".pm-tab-panel").textContent, /cloud_pro/);
  $$(".pp-tab").find((b) => b.textContent.startsWith("Mailboxes")).click();
  assert.ok(!$(".pm-tab-panel").textContent.includes("abcdef0123456789abcdef"));
  void w;
});

test("support: the live inbox authenticates its socket as staff and re-reads on support.changed", async (t) => {
  const tickets = [{ id: "t1", subject: "Payout question", publisher_name: "Acme", status: "open", priority: "normal", updated_at: "2026-11-14T09:00:00Z", unread: true }];
  const { w, $, calls, sockets } = boot(t, { url: "https://management.pigeonpost.dev/#/support", routes: base(["support"], {
    "GET /v1/support/tickets": reply(200, { data: tickets, next_offset: null }),
    "GET /v1/support/tickets/t1": reply(200, { data: { id: "t1", subject: "Payout question", publisher_name: "Acme", publisher_id: "pub_acme", publisher_status: "approved", status: "open", priority: "normal", category: "billing", last_seq: 2, older_before: null, messages: [{ seq: 1, author_kind: "partner", body: "<b>When</b> is the payout?", created_at: "2026-11-14T09:00:00Z" }] } }),
    "POST /v1/support/tickets/t1/read": reply(200, {}),
    "POST /v1/support/tickets/t1/messages": reply(201, { data: {} }),
  }) });
  await tick();
  assert.equal(sockets.length, 1);
  assert.equal(sockets[0].url, "wss://postbox.pigeonpost.dev/v1/support/live");
  assert.equal(sockets[0].sent[0].staff, true);
  assert.equal(sockets[0].sent[0].publisher, null);
  assert.ok(sockets[0].sent[0].token);
  assert.equal(calls.find((c) => c.path === "/v1/support/tickets").query.staff, "true");
  sockets[0].serverSend({ type: "ready" });
  await tick();
  assert.match($(".pm-live").textContent, /Live/);
  $('[data-ticket="t1"]').click();
  await tick();
  assert.equal($(".pm-msg p").textContent, "<b>When</b> is the payout?", "server text stays text");
  assert.equal($(".pm-msg b"), null);
  const before = calls.filter((c) => c.path === "/v1/support/tickets/t1").length;
  sockets[0].serverSend({ type: "support.changed", ticket_id: "t1" });
  await tick();
  assert.ok(calls.filter((c) => c.path === "/v1/support/tickets/t1").length > before, "the open conversation is re-read");
  type(w, $(".pm-composer textarea"), "Payouts run on the 1st.");
  submit(w, $(".pm-composer"));
  await tick();
  const sent = calls.find((c) => c.method === "POST" && c.path === "/v1/support/tickets/t1/messages");
  assert.equal(sent.body.body, "Payouts run on the 1st.");
  assert.ok(sent.body.client_id);
  // Leaving the page closes the socket.
  await go(w, "#/overview");
  await tick();
  assert.ok(sockets.every((s) => s.closed));
});

test("soon pages say what they will hold and why they are not live", async (t) => {
  const { $ } = boot(t, { url: "https://management.pigeonpost.dev/#/nests", routes: base(["ops"]) });
  await tick();
  assert.equal($("h1").textContent, "Nests");
  assert.match($(".pm-soon").textContent, /Not live yet/);
  assert.match($(".pm-soon").textContent, /no staff route/);
  assert.ok($(".pm-soon-list li"));
});

test("the page ships no inline script or style, is not indexed, and never writes server text as HTML", () => {
  const html = read("../index.html");
  assert.match(html, /<meta name="robots" content="noindex,nofollow">/);
  assert.match(html, /script-src 'self'; style-src 'self'/);
  assert.doesNotMatch(html, /<script>(?!<\/script>)|<style|style="/);
  assert.doesNotMatch(html, /\son[a-z]+="/);
  for (const src of scripts().filter((s) => !s.startsWith("/shared/"))) {
    const js = read(`..${src}`);
    assert.doesNotMatch(js, /innerHTML|outerHTML|insertAdjacentHTML|document\.write/, `${src} writes HTML`);
    assert.doesNotMatch(js, /\.setAttribute\("style"|style: `|style: "/, `${src} sets inline styles (CSP style-src 'self')`);
  }
  assert.match(read("../config.js"), /clientId: "pigeonpost-web"/);
});
