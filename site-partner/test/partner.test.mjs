// partner.pigeonpost.dev against a fake postbox: every page renders its data and its empty state,
// the application saves drafts and holds submit until complete, prices and publishing go through
// step-up, listings wait for approval, and support is live over a WebSocket.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { boot, go, tick, reply, postbox, publisher, dossier, COMPLETE, DOCS, PUB, LISTINGS } from "./harness.mjs";

const $ = (env, sel) => env.doc.querySelector(sel);
const $$ = (env, sel) => [...env.doc.querySelectorAll(sel)];
const button = (env, label, root) => [...(root || env.doc).querySelectorAll("button, a.pp-btn")].find((b) => b.textContent.trim() === label);

test("index.html: CSP allows only this origin and the postbox; no inline script or style", () => {
  const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
  const csp = /Content-Security-Policy" content="([^"]+)"/.exec(html)[1];
  assert.match(csp, /script-src 'self';/);
  assert.match(csp, /style-src 'self';/);
  assert.match(csp, /connect-src 'self' https:\/\/postbox\.pigeonpost\.dev wss:\/\/postbox\.pigeonpost\.dev;/);
  assert.match(csp, /img-src 'self' data: https:\/\/postbox\.pigeonpost\.dev;/);
  assert.doesNotMatch(html, /<script>|<script [^s]|style=/);
  for (const f of ["partner-core.js", "page-overview.js", "page-application.js", "page-business.js", "page-apps.js", "page-listings.js", "page-analytics.js", "page-earnings.js", "page-support.js", "page-docs.js", "app.js"]) {
    const js = readFileSync(new URL(`../${f}`, import.meta.url), "utf8");
    assert.doesNotMatch(js, /innerHTML|insertAdjacentHTML|document\.write|style:\s*["`]/, `${f} writes no HTML or inline styles`);
  }
});

test("signed out: the partner sign-in screen", async (t) => {
  const env = await boot(t, { session: null });
  assert.equal($(env, ".pp-signin-card h1").textContent, "Publish on Pigeonpost");
  assert.ok(button(env, "Sign in"));
});

test("no publisher yet: Overview leads with Start your application, which creates one", async (t) => {
  let created = null;
  let pubs = [];
  const env = await boot(t, { routes: (m, p, b) => {
    if (p === "/v1/publishers") return reply(200, { data: pubs });
    if (p === "/v1/publisher-applications" && m === "POST") { created = b; pubs = [publisher()]; return reply(201, { data: { id: PUB } }); }
    return postbox({ pub: null })(m, p, b);
  } });
  assert.match($(env, "h1").textContent, /Welcome, Ada/);
  assert.match(env.text(), /Start your application/);
  const nav = $$(env, ".pp-nav-item").map((a) => a.getAttribute("data-page"));
  assert.deepEqual(nav, ["overview", "application", "business", "apps", "listings", "analytics", "earnings", "support", "docs"]);
  const form = $(env, ".ppp-start-form");
  form.querySelector("input").value = "Acme Labs";
  form.dispatchEvent(new env.w.Event("submit", { cancelable: true }));
  await tick();
  assert.deepEqual(JSON.parse(JSON.stringify(created)), { display_name: "Acme Labs" });
  assert.equal(env.w.location.hash, "#/application");
});

test("context: the switcher adds management for staff and enterprise for org admins; failing probes never break the page", async (t) => {
  const env = await boot(t, { routes: postbox({ staff: ["reviewer"], orgs: [{ id: "org_1", name: "Acme Org", my_roles: ["admin"] }] }) });
  const sw = $$(env, "#pp-switch-pop .pp-pop-item strong").map((s) => s.textContent);
  assert.deepEqual(sw, ["Account", "Enterprise", "Partner", "Management"]);

  const env2 = await boot(t, { routes: postbox({ extra: (m, p) => {
    if (p === "/v1/staff/me") return reply(401, { error: "step_up_required", acr_values: "gold" });
    if (p === "/v1/orgs") return reply(500, { error: "boom" });
  } }) });
  assert.equal(env2.nav.length, 0, "a probe never leaves for the realm");
  assert.match($(env2, ".ppp-hero").textContent, /Draft/);
  assert.deepEqual($$(env2, "#pp-switch-pop .pp-pop-item strong").map((s) => s.textContent), ["Account", "Partner"]);
});

test("overview: the status hero shows the state, the staff request and the next step", async (t) => {
  const app = dossier({ state: "needs_information", requested_documents: ["tax"], history: [{ actor: "staff", event: "needs_information", revision: 4, note: "Upload a current vergi levhası.", created_at: "2026-11-13T09:00:00Z" }] });
  const env = await boot(t, { routes: postbox({ app }) });
  const hero = $(env, ".ppp-hero");
  assert.match(hero.textContent, /Needs information/);
  assert.match(hero.textContent, /Upload a current vergi levhası/);
  assert.equal(button(env, "Answer Pigeonpost's request").getAttribute("href"), "#/application");
  assert.equal($(env, '[data-page="application"] .pp-nav-count').textContent, "1");
  assert.match($(env, ".pp-stats").textContent, /Open support\s*1/);

  const approved = await boot(t, { routes: postbox({ pub: publisher("approved"), app: dossier({ state: "approved", details: COMPLETE }) }) });
  assert.match($(approved, ".ppp-hero").textContent, /Approved/);
  assert.ok(button(approved, "Go to your apps"));
  assert.match($(approved, ".pp-stats").textContent, /Active apps\s*1/);
  assert.match($(approved, ".pp-stats").textContent, /\$350/);
});

test("application: steps save the draft with only the postbox's fields; submit waits until every step is complete", async (t) => {
  const saves = [];
  let submitted = false;
  let app = dossier();
  const env = await boot(t, { hash: "#/application/type", routes: postbox({ app, extra: (m, p, b) => {
    if (p === `/v1/publisher-applications/${PUB}/dossier` && m === "PUT") {
      saves.push(b);
      app = dossier({ revision: b.revision + 1, details: b.details, documents: app.documents });
      return reply(200, { data: app });
    }
    if (p === `/v1/publisher-applications/${PUB}/submit`) { submitted = true; return reply(200, { data: dossier({ ...app, state: "submitted", revision: app.revision + 1 }) }); }
  } }) });
  const steps = $$(env, ".pp-stepper li").map((li) => li.textContent);
  assert.equal(steps.length, 8);
  assert.match(steps.join("|"), /Account type.*Legal details.*Tax.*Bank.*Documents.*Identity.*Agreements.*Review & submit/);
  // Step 1 is complete already (company in TR); continue saves it.
  button(env, "Save and continue").click();
  await tick();
  assert.equal(saves.length, 1);
  assert.equal(saves[0].revision, 3);
  assert.deepEqual(Object.keys(saves[0].details).sort(), Object.keys(COMPLETE).sort(), "exactly the Details fields");
  assert.match(env.w.location.hash, /#\/application\/legal/);
  // Type into a field, save the draft: the value goes up.
  const legal = $(env, 'input[name="contact_name"]');
  legal.value = "Ada Lovelace"; legal.dispatchEvent(new env.w.Event("input"));
  button(env, "Save draft").click();
  await tick();
  assert.equal(saves.at(-1).details.contact_name, "Ada Lovelace");
  assert.equal(saves.at(-1).revision, 4, "each save sends the revision the last one returned");
  // Review: incomplete, so submit is off and says what's missing.
  await go(env, "#/application/review");
  const submit = button(env, "Submit for review");
  assert.ok(submit.disabled, "submit is blocked");
  assert.match($(env, "#ppp-submit-blocked").textContent, /Legal details.*Tax.*Bank.*Documents.*Agreements/);
  submit.click();
  await tick();
  assert.equal(submitted, false);

  // Everything complete: submit is on and posts the current revision.
  app = dossier({ details: COMPLETE, documents: DOCS, revision: 9 });
  const env2 = await boot(t, { hash: "#/application/review", routes: postbox({ app, extra: (m, p, b) => {
    if (p === `/v1/publisher-applications/${PUB}/dossier` && m === "PUT") return reply(200, { data: dossier({ details: b.details, documents: DOCS, revision: b.revision + 1 }) });
    if (p === `/v1/publisher-applications/${PUB}/submit`) { submitted = b; return reply(200, { data: dossier({ details: COMPLETE, documents: DOCS, state: "submitted", revision: 11 }) }); }
  } }) });
  assert.equal($$(env2, ".pp-stepper li.is-done").length, 7);
  const go2 = button(env2, "Submit for review");
  assert.equal(go2.disabled, false);
  go2.click();
  await tick();
  assert.deepEqual(JSON.parse(JSON.stringify(submitted)), { revision: 10 });
});

test("application: individuals see the KYC step, and a clear state when Shufti Pro isn't switched on", async (t) => {
  const app = dossier({ kind: "individual", details: { ...COMPLETE, kind: "individual", kyc_consent: true } });
  const env = await boot(t, { hash: "#/application/identity", routes: postbox({ app, extra: (m, p, b) => {
    if (p.endsWith("/dossier") && m === "PUT") return reply(200, { data: dossier({ ...app, revision: b.revision + 1, details: b.details }) });
    if (p.endsWith("/kyc")) return reply(503, { error: "kyc_not_configured", detail: "Identity verification is not configured yet. Your draft is saved." });
  } }) });
  assert.match(env.text(), /Identity check \(KYC\)/);
  assert.equal($(env, ".pp-stepper li:nth-child(6)").classList.contains("is-done"), false);
  button(env, "Start identity check").click();
  await tick();
  assert.match(env.text(), /The identity check opens when Pigeonpost enables it/);
  assert.equal(env.nav.length, 0);
});

test("application: a submitted application is read-only, with history", async (t) => {
  const env = await boot(t, { hash: "#/application", routes: postbox({ app: dossier({ state: "in_review", details: COMPLETE, documents: DOCS, history: [{ actor: "a", event: "submitted", revision: 5, note: "", created_at: "2026-11-12T09:00:00Z" }] }) }) });
  assert.match(env.text(), /locked while Pigeonpost reviews it/);
  assert.equal($(env, ".pp-stepper"), null);
  assert.match(env.text(), /faaliyet\.pdf/);
  assert.match($(env, ".ppp-history").textContent, /Submitted/);
});

test("listings are gated until the application is approved", async (t) => {
  const env = await boot(t, { hash: "#/listings", routes: postbox() });
  const gate = $(env, ".pp-gate");
  assert.match(gate.textContent, /Finish your application first/);
  assert.equal(gate.querySelector("a").getAttribute("href"), "#/application");
  assert.ok($(env, ".pp-gated-area").hasAttribute("inert"));
  assert.ok(!env.calls.some((c) => c.path.includes("/listings")), "nothing is fetched before approval");
  await go(env, "#/apps");
  assert.match($(env, ".pp-gate").textContent, /Finish your application first/);
});

test("listings: data, detail and empty state", async (t) => {
  const env = await boot(t, { hash: "#/listings", routes: postbox({ pub: publisher("approved"), app: dossier({ state: "approved", details: COMPLETE }) }) });
  const rows = $$(env, "tbody tr");
  assert.equal(rows.length, 2);
  assert.match(rows[0].textContent + rows[1].textContent, /Contract Reviewer.*Published|Published.*Contract Reviewer/s);
  assert.match(env.text(), /\$19/);
  await go(env, "#/listings/lst_1");
  assert.equal($(env, "h1").textContent, "Contract Reviewer");
  assert.ok(button(env, "Withdraw"));
  assert.equal($(env, 'select[name="category"]').value, "legal");

  const empty = await boot(t, { hash: "#/listings", routes: postbox({ pub: publisher("approved"), extra: (m, p) => (p.startsWith(`/v1/publishers/${PUB}/listings`) ? reply(200, { data: [] }) : undefined) }) });
  assert.match(empty.text(), /No listings yet/);
  assert.ok(button(empty, "New listing"));
});

test("pricing and publishing ask for a strong sign-in: 401 step_up_required leaves through PP.stepUp and replays on return", async (t) => {
  const priceCalls = [];
  const env = await boot(t, { hash: "#/listings/lst_1", routes: postbox({ pub: publisher("approved"), app: dossier({ state: "approved", details: COMPLETE }), extra: (m, p, b) => {
    if (p === `/v1/publishers/${PUB}/listings/lst_1/price`) { priceCalls.push(b); return reply(401, { error: { code: "step_up_required", message: "Confirm it's you", details: { acr_values: "gold", max_age: 300 } } }); }
  } }) });
  const form = [...env.doc.querySelectorAll("form")].find((f) => f.querySelector('input[name="amount"]'));
  form.querySelector('input[name="amount"]').value = "25";
  form.dispatchEvent(new env.w.Event("submit", { cancelable: true }));
  await tick();
  assert.deepEqual(JSON.parse(JSON.stringify(priceCalls[0])), { amount_minor: 2500, currency: "USD", term: "one_off" });
  assert.equal(env.nav.length, 1, "left for the realm");
  const u = new URL(env.nav[0]);
  assert.equal(u.pathname, "/realms/pigeonpost-prod/protocol/openid-connect/auth");
  assert.equal(u.searchParams.get("acr_values"), "gold", "the nested error shape's challenge is read");
  assert.equal(u.searchParams.get("max_age"), "300");
  const resume = JSON.parse(env.w.localStorage.getItem("pp_resume"));
  assert.equal(resume.name, "partner.replay");
  assert.deepEqual(resume.payload, { path: `/v1/publishers/${PUB}/listings/lst_1/price`, method: "PUT", body: { amount_minor: 2500, currency: "USD", term: "one_off" }, back: "#/listings/lst_1" });

  // Back from the realm: the same call runs once, and the page shows the listing again.
  const replayed = [];
  const back = await boot(t, { hash: "#/overview", storage: { pp_resume: JSON.stringify({ ...resume, at: Date.now() }) }, routes: postbox({ pub: publisher("approved"), extra: (m, p, b) => {
    if (p === `/v1/publishers/${PUB}/listings/lst_1/price`) { replayed.push([m, b]); return reply(200, { data: {} }); }
  } }) });
  assert.deepEqual(JSON.parse(JSON.stringify(replayed)), [["PUT", { amount_minor: 2500, currency: "USD", term: "one_off" }]]);
  assert.equal(back.w.location.hash, "#/listings/lst_1");
  assert.equal(back.w.localStorage.getItem("pp_resume"), null);
});

test("publish: a draft listing's Publish button posts through step-up", async (t) => {
  const env = await boot(t, { hash: "#/listings/lst_2", routes: postbox({ pub: publisher("approved"), extra: (m, p) => {
    if (p === `/v1/publishers/${PUB}/listings/lst_2/publish`) return reply(401, { error: "step_up_required" }, { "www-authenticate": 'Bearer error="insufficient_user_authentication", acr_values="gold", max_age=600' });
    if (p === "/v1/marketplace/listings/lst_2") return reply(200, { data: { ...LISTINGS[1], operations: [{ name: "research.report", title: "Research report", price: null }] } });
  } }) });
  assert.match(env.text(), /Price per job/);
  assert.match(env.text(), /Research report/);
  button(env, "Publish").click();
  await tick();
  assert.equal(env.nav.length, 1);
  assert.equal(new URL(env.nav[0]).searchParams.get("acr_values"), "gold");
  assert.equal(JSON.parse(env.w.localStorage.getItem("pp_resume")).payload.path, `/v1/publishers/${PUB}/listings/lst_2/publish`);
});

test("business: details, public profile (editable while a draft), team; empty without a publisher", async (t) => {
  const saves = [];
  const env = await boot(t, { hash: "#/business", routes: postbox({ app: dossier({ details: COMPLETE }), extra: (m, p, b) => { if (m === "PUT") { saves.push(b); return reply(200, { data: dossier({ details: b.details, revision: 4 }) }); } } }) });
  assert.match(env.text(), /Acme Yazilim A\.S\./);
  assert.match(env.text(), /IBAN •••• 1326/);
  await go(env, "#/business/profile");
  const form = $(env, "form.pp-form");
  form.querySelector('textarea[name="public_summary"]').value = "We build legal tools.";
  form.dispatchEvent(new env.w.Event("submit", { cancelable: true }));
  await tick();
  assert.equal(saves[0].details.public_summary, "We build legal tools.");
  await go(env, "#/business/team");
  assert.match(env.text(), /A personal publisher is just you/);
  const none = await boot(t, { hash: "#/business", routes: postbox({ pub: null }) });
  assert.match(none.text(), /Start your application first/);
});

test("apps: builds, reviews with stages, and the staging-only test Nest state", async (t) => {
  const env = await boot(t, { hash: "#/apps", routes: postbox({ pub: publisher("approved"), extra: (m, p) => {
    if (p === `/v1/publishers/${PUB}/reviews/rev_1`) return reply(200, { data: { review_id: "rev_1", kind: "nest", release_id: "rel_1", status: "approved", revision: 2, allowed_actions: ["view_findings", "publish_listing"], stages: [{ name: "intake", status: "passed" }, { name: "human_review", status: "passed" }] } });
  } }) });
  assert.match(env.text(), /Contract Reviewer 1\.0\.0/);
  assert.match(env.text(), /aaaaaaaaaaaa/);
  await go(env, "#/apps/review/rev_1");
  assert.match($(env, ".ppp-stages").textContent, /Intake.*Staff review/);
  assert.equal(button(env, "Create a listing").getAttribute("href"), "#/listings/new/rel_1");
  await go(env, "#/apps/tests");
  assert.match(env.text(), /Test Nests run on the staging postbox/);
  await go(env, "#/apps/integrations");
  assert.ok($(env, 'textarea[name="operations"]'));
});

test("analytics: totals, per-app rows and charts; the empty state without apps", async (t) => {
  const env = await boot(t, { hash: "#/analytics", routes: postbox({ pub: publisher("approved") }) });
  assert.match($(env, ".pp-stats").textContent, /Jobs\s*120/);
  assert.match($(env, ".pp-stats").textContent, /\$350/);
  assert.equal($$(env, ".ppp-chart").length, 2);
  assert.ok(env.calls.some((c) => /\/analytics\?from=\d{4}-\d\d-\d\dT00%3A00%3A00Z&to=/.test(c.path)));
  assert.match(env.text(), /Contract Reviewer/);
  const empty = await boot(t, { hash: "#/analytics", routes: postbox({ pub: publisher("approved"), extra: (m, p) => (p.includes("/analytics") ? reply(200, { data: { usage: {}, apps: [], app_choices: [], daily: [], finance: { status: "unavailable", message: "Finance is offline." } } }) : undefined) }) });
  assert.match(empty.text(), /No apps in this report yet/);
  assert.match(empty.text(), /Finance is offline/);
});

test("earnings: the first-statement state, balances and the payout account", async (t) => {
  const env = await boot(t, { hash: "#/earnings", routes: postbox({ pub: publisher("approved"), app: dossier({ state: "approved", details: COMPLETE }) }) });
  assert.match(env.text(), /Your first statement arrives on December 1, 2026/);
  assert.match(env.text(), /Pigeonpost 10%/);
  assert.match(env.text(), /Withholding/);
  assert.match(env.text(), /\$350/);
  assert.match(env.text(), /Invoice upload opens with your first statement/);
  const before = await boot(t, { hash: "#/earnings", routes: postbox() });
  assert.match(before.text(), /Earnings start after approval/);
});

test("support: connects over a WebSocket, re-reads on changes, and sends replies over HTTP", async (t) => {
  const sent = [];
  const env = await boot(t, { hash: "#/support", routes: postbox({ extra: (m, p, b) => { if (m === "POST" && /\/messages/.test(p)) { sent.push([p, b]); return reply(200, { data: {} }); } } }) });
  assert.equal(env.sockets.length, 1);
  const ws = env.sockets[0];
  assert.equal(ws.url, "wss://postbox.pigeonpost.dev/v1/support/live");
  await tick();
  assert.deepEqual(ws.sent[0], { token: env.w.sessionStorage.getItem("pp_session"), publisher: PUB, staff: false });
  assert.equal($$(env, ".ppp-ticket").length, 2);
  ws.serve({ type: "ready" });
  await tick();
  assert.equal($(env, ".ppp-live").dataset.state, "live");
  const before = env.calls.filter((c) => c.path.startsWith("/v1/support/tickets?")).length;
  ws.serve({ type: "support.changed", ticket_id: "tkt_9" });
  await tick();
  assert.ok(env.calls.filter((c) => c.path.startsWith("/v1/support/tickets?")).length > before, "a change re-reads the list");
  // Open a conversation without leaving the page: the socket stays.
  $(env, '.ppp-ticket[data-ticket="tkt_1"]').click();
  await tick();
  assert.match($(env, ".ppp-messages").textContent, /vergi levhası/);
  assert.equal(env.sockets.length, 1);
  const box = $(env, ".ppp-composer textarea");
  box.value = "Uploaded it now."; box.dispatchEvent(new env.w.Event("input"));
  button(env, "Send").click();
  await tick();
  assert.equal(sent[0][0], `/v1/support/tickets/tkt_1/messages?publisher=${PUB}`);
  assert.equal(sent[0][1].body, "Uploaded it now.");
  // Leaving the page closes the socket.
  await go(env, "#/docs");
  assert.ok(ws.closed);
  assert.match(env.text(), /Publisher-hosted apps/);
});
