// The back office (site/staff.html, site/staff.js): the shell and its role-filtered menu, hash
// routing, table states, the "not available" page, step-up, and masking. Fake postbox answers.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { JSDOM, VirtualConsole } from "jsdom";

const read = (p) => readFileSync(new URL(p, import.meta.url), "utf8");
const accountConfig = read("../../site/account-config.js");
const core = read("../../site/marketplace-core.js");
const mpStaff = read("../../site/marketplace-staff.js");
const staffJs = read("../../site/staff.js");
const staffHtml = read("../../site/staff.html");
const staffCss = read("../../site/staff.css");
const tick = async (n = 30) => { for (let i = 0; i < n; i++) await new Promise((r) => setImmediate(r)); };
const settle = () => new Promise((r) => setTimeout(r, 30));

const ME = (roles, extra = {}) => ({ data: { account_id: "acct_me", roles, platform_owner: false, step_up: { acr_values: "mfa", write_max_age: 600, read_max_age: 3600 }, finance_configured: true, ...extra } });
const DASH = { data: { counts: { accounts: 12, signups_today: 2, signups_7d: 5, active_accounts_30d: 9, handles: 4, mailboxes: 20, orgs_active: 1, nests_running: 3, open_reviews: 1, open_reports: 0, publisher_applications: 2, hosted_jobs_failed_24h: 0, charges_refused: 1 }, alerts: [{ code: "charges_refused", count: 1 }], as_of: "2026-11-11T10:00:00Z" } };
const ACCOUNTS = { data: [{ id: "acct_1", created_at: "2026-11-01T00:00:00Z", sign_in_address: "a@b.example", mailboxes: 2, handles: 1, staff: false }], next_cursor: "o25" };
const ACCOUNT = { data: {
  id: "acct_1", created_at: "2026-11-01T00:00:00Z", oidc_subject: "sub-1",
  sign_in_identities: [{ provider: "realm", login: "a@b.example", verified_at: "2026-11-01T00:00:00Z" }],
  // As if the server forgot to mask: the page masks a field named like a secret on its own.
  devices: [{ push_token: "apns-raw-token-0123456789", mailbox: "/k/x", platform: "apns", environment: "production", kind: "alert", os: "iOS", updated_at: "2026-11-10T00:00:00Z" }],
  mailboxes: [], handles: [], orgs: [], entitlements: [], subscriptions: { apple: [], google: [], nest_store: [] }, nests: [], staff_roles: [], deletion: null,
  recent_audit: [{ id: 9, at: "2026-11-10T00:00:00Z", actor_account: "acct_me", actor_kind: "human", action: "staff.accounts.read", target: "account:acct_1", org_id: null, details: { iban: "TR330006100519786457841326", note: "ok" } }],
} };

/// The back office in a window. `routes(method, path, body)` answers postbox calls; `undefined`
/// is a 404. Navigations (sign-in redirects) are collected, not followed.
async function page(t, { hash = "#dashboard", token = "fixture-token", routes = () => undefined, storage = {} } = {}) {
  const navigations = [];
  const virtualConsole = new VirtualConsole();
  virtualConsole.on("jsdomError", (e) => { if (/navigation/i.test(String(e && e.message))) navigations.push(e.message); });
  const dom = new JSDOM('<div id="staff-root"></div>', { url: `https://pigeonpost.dev/staff${hash}`, runScripts: "outside-only", virtualConsole });
  t.after(async () => { await tick(5); dom.window.close(); });
  const { window } = dom;
  if (!window.crypto.subtle) Object.defineProperty(window.crypto, "subtle", { value: globalThis.crypto.subtle });
  if (token) window.sessionStorage.setItem("pp_session", token);
  for (const [k, v] of Object.entries(storage)) window.localStorage.setItem(k, v);
  const calls = [];
  window.fetch = async (url, init = {}) => {
    const u = new URL(url, "https://pigeonpost.dev");
    const path = u.pathname + u.search;
    const method = init.method || "GET";
    const body = init.body ? JSON.parse(init.body) : undefined;
    calls.push({ origin: u.origin, path, method, body, auth: init.headers && init.headers.authorization });
    const out = await routes(method, path, body);
    const res = (b, status = 200) => ({ ok: status < 400, status, json: async () => b });
    if (out === undefined) return res({ error: { code: "not_found", message: "no such record" } }, 404);
    if (out && out.__status) return res(out.body, out.__status);
    return res(out);
  };
  window.PIGEONPOST_STAFF = { manualStart: true };
  window.eval(accountConfig);
  for (const s of [core, mpStaff, staffJs]) window.eval(s);
  await window.PPStaff.start();
  await tick();
  const doc = window.document;
  return { window, doc, $: (s) => doc.querySelector(s), $$: (s) => [...doc.querySelectorAll(s)], calls, navigations };
}
const fail = (status, code, details = {}) => ({ __status: status, body: { error: { code, message: code, details } } });
const go = async (p, hash) => { p.window.location.hash = hash; await settle(); await tick(); };
const navLabels = (p) => p.$$("#ac-side a.ac-nav-link").map((a) => a.textContent.trim());

// ---- masking ----------------------------------------------------------------------------------

test("masking hides secrets, IBANs and tax ids, keeps what the server already masked", (t) => {
  const dom = new JSDOM("", { url: "https://pigeonpost.dev/staff", runScripts: "outside-only" });
  t.after(() => dom.window.close());
  dom.window.PIGEONPOST_STAFF = { manualStart: true };
  dom.window.eval(staffJs);
  const S = dom.window.PPStaff;
  assert.equal(S.mask("abcdefghijkl"), "••••ijkl");
  assert.equal(S.mask("short"), "••••");
  assert.equal(S.shown("iban", "TR330006100519786457841326"), "••••1326");
  assert.equal(S.shown("tax_id", "1234567890"), "••••7890");
  assert.equal(S.shown("api_key", "pk_live_abcdef0123456789"), "••••6789");
  assert.equal(S.shown("push_token", "apns-raw-token-0123456789"), "••••6789");
  assert.equal(S.shown("note", "TR33 0006 1005 1978 6457 8413 26"), "••••1326", "an IBAN is masked whatever the field is called");
  assert.equal(S.shown("reference", "••••9999"), "••••9999", "already masked stays as it is");
  assert.equal(S.shown("login", "a@b.example"), "a@b.example");
  assert.equal(S.shown("plan_code", "cloud-2names-yearly-usd"), "cloud-2names-yearly-usd");
  assert.deepEqual(JSON.parse(JSON.stringify(S.maskDeep({ a: { client_secret: "0123456789abcdef" }, list: [{ iban: "TR330006100519786457841326" }], n: 3 }))),
    { a: { client_secret: "••••cdef" }, list: [{ iban: "••••1326" }], n: "3" });
});

test("audit CSV cells can never be read as formulas", (t) => {
  const dom = new JSDOM("", { url: "https://pigeonpost.dev/staff", runScripts: "outside-only" });
  t.after(() => dom.window.close());
  dom.window.PIGEONPOST_STAFF = { manualStart: true };
  dom.window.eval(staffJs);
  const cell = dom.window.PPStaff.csvCell;
  for (const [input, out] of [["=HYPERLINK(1)", `"'=HYPERLINK(1)"`], ["+1", `"'+1"`], ["-2", `"'-2"`], ["@sum", `"'@sum"`],
    ["\tx", `"'\tx"`], ["\rx", `"'\rx"`], ['say "hi"', '"say ""hi"""'], ["staff.accounts.read", '"staff.accounts.read"'], [null, '""'], [42, '"42"']]) {
    assert.equal(cell(input), out, JSON.stringify(input));
  }
});

test("hash routes parse and round-trip", (t) => {
  const dom = new JSDOM("", { url: "https://pigeonpost.dev/staff", runScripts: "outside-only" });
  t.after(() => dom.window.close());
  dom.window.PIGEONPOST_STAFF = { manualStart: true };
  dom.window.eval(staffJs);
  const S = dom.window.PPStaff;
  assert.deepEqual({ ...S.parseHash("#accounts/acct_1") }, { route: "accounts", arg: "acct_1" });
  assert.deepEqual({ ...S.parseHash("#credits/account/acct_1") }, { route: "credits", arg: "account/acct_1" });
  assert.deepEqual({ ...S.parseHash("") }, { route: "", arg: null });
  assert.equal(S.hashFor("handles", "a b"), "#handles/a%20b");
  assert.deepEqual({ ...S.parseHash(S.hashFor("handles", "a b")) }, { route: "handles", arg: "a b" });
});

// ---- who gets in ------------------------------------------------------------------------------

test("signed out: one sign-in button, nothing asked of the postbox", async (t) => {
  const p = await page(t, { token: null });
  assert.ok(p.$("#st-signin"));
  assert.equal(p.calls.length, 0);
  assert.equal(p.$("#ac-app"), null);
  p.$("#st-signin").click();
  await tick();
  assert.equal(p.navigations.length, 1, "off to the realm");
});

test("non-staff get a plain not-available page and nothing else", async (t) => {
  for (const refusal of [fail(403, "staff_required"), fail(403, "human_required")]) {
    const p = await page(t, { routes: (m, path) => (path === "/v1/staff/me" ? refusal : undefined) });
    assert.equal(p.$("#st-plain h1").textContent, "Not available");
    assert.equal(p.$("#ac-app"), null);
    assert.equal(p.$("#ac-side"), null);
    assert.deepEqual(p.calls.map((c) => c.path), ["/v1/staff/me"]);
    assert.equal(p.window.document.title, "Not available · Pigeonpost");
  }
});

test("every call goes to the postbox with the session, never a finance credential", async (t) => {
  const p = await page(t, { routes: (m, path) => (path === "/v1/staff/me" ? ME(["finance"]) : path === "/v1/staff/dashboard" ? DASH : { data: [], next_cursor: null }) });
  assert.ok(p.calls.length > 1);
  for (const c of p.calls) {
    assert.equal(c.origin, "https://postbox.pigeonpost.dev", c.path);
    assert.equal(c.auth, "Bearer fixture-token");
  }
});

// ---- the shell and its menu --------------------------------------------------------------------

test("the menu shows only the caller's screens, with later phases greyed", async (t) => {
  const p = await page(t, { routes: (m, path) => (path === "/v1/staff/me" ? ME(["support"]) : path === "/v1/staff/dashboard" ? DASH : undefined) });
  assert.ok(p.$("#ac-app.st-app"), "the account shell");
  assert.ok(p.window.document.body.classList.contains("ac-shell"));
  assert.deepEqual(navLabels(p), ["Dashboard", "Accounts", "Handles", "Mailboxes", "Organizations", "Entitlements"]);
  const soon = p.$$(".st-soon").map((s) => s.textContent.trim());
  assert.ok(soon.some((s) => s.startsWith("Reports")), soon.join("|"));
  for (const s of p.$$(".st-soon")) {
    assert.equal(s.tagName, "SPAN", "a coming-soon entry is not a link");
    assert.equal(s.getAttribute("aria-disabled"), "true");
  }
  assert.equal(p.$('#ac-side [aria-current="page"]').dataset.route, "dashboard");
  assert.match(p.$("#st-view").textContent, /Signups today/);
  assert.match(p.$(".st-alerts").textContent, /charge was refused by billing/);
  // Finance sees money, not people.
  const f = await page(t, { routes: (m, path) => (path === "/v1/staff/me" ? ME(["finance"]) : path === "/v1/staff/dashboard" ? DASH : { data: [], next_cursor: null }) });
  assert.deepEqual(navLabels(f), ["Dashboard", "Entitlements", "Credits", "Publishers"]);
  // Staff & roles is the platform owner's alone, whatever the role rows say.
  const o = await page(t, { routes: (m, path) => (path === "/v1/staff/me" ? ME(["owner"], { platform_owner: true }) : path === "/v1/staff/dashboard" ? DASH : undefined) });
  assert.ok(navLabels(o).includes("Staff & roles"));
  assert.ok(navLabels(o).includes("Audit log"));
  const r = await page(t, { routes: (m, path) => (path === "/v1/staff/me" ? ME(["owner"]) : path === "/v1/staff/dashboard" ? DASH : undefined) });
  assert.ok(!navLabels(r).includes("Staff & roles"));
});

test("routing: a screen per hash, detail routes, and a hash the caller can't use falls back", async (t) => {
  const p = await page(t, {
    hash: "#accounts",
    routes: (m, path) => {
      if (path === "/v1/staff/me") return ME(["support"]);
      if (path === "/v1/staff/dashboard") return DASH;
      if (path.startsWith("/v1/staff/accounts?")) return ACCOUNTS;
      if (path === "/v1/staff/accounts/acct_1") return ACCOUNT;
      return undefined;
    },
  });
  assert.equal(p.$("#st-h").textContent, "Accounts");
  assert.equal(p.window.document.title, "Accounts · Pigeonpost back office");
  assert.equal(p.$("#ac-top-title").textContent, "Accounts");
  assert.equal(p.$$(".st-table tbody tr").length, 1);
  assert.ok(p.$(".st-more button"), "a next page is offered");
  // Search sends q; the next page sends the cursor.
  p.$("#st-acc-q").value = "a@b";
  p.$("form.st-toolbar").dispatchEvent(new p.window.Event("submit", { cancelable: true }));
  await tick();
  assert.ok(p.calls.some((c) => c.path === "/v1/staff/accounts?q=a%40b&limit=25"), JSON.stringify(p.calls.map((c) => c.path)));
  p.$(".st-more button").click();
  await tick();
  assert.ok(p.calls.some((c) => /cursor=o25/.test(c.path)));
  // A row opens the detail route.
  p.$(".st-table tbody tr").click();
  await settle(); await tick();
  assert.equal(p.window.location.hash, "#accounts/acct_1");
  assert.equal(p.$("#st-h").textContent, "acct_1");
  assert.ok(p.$(".st-back"));
  // The device token and the IBAN in the audit details never reach the screen whole.
  const text = p.$("#st-view").textContent;
  assert.ok(!text.includes("apns-raw-token"), "push token masked");
  assert.ok(text.includes("••••6789"));
  assert.ok(!text.includes("TR330006100519786457841326"), "IBAN masked");
  // A screen this role has no part in goes back to the dashboard.
  await go(p, "#credits");
  assert.equal(p.window.location.hash, "#dashboard");
  assert.equal(p.$("#st-h").textContent, "Dashboard");
  await go(p, "#nonsense");
  assert.equal(p.$("#st-h").textContent, "Dashboard");
});

test("tables show loading, empty and error states, with a retry", async (t) => {
  let fails = true;
  let release;
  const gate = new Promise((r) => { release = r; });
  const p = await page(t, {
    hash: "#handles",
    routes: async (m, path) => {
      if (path === "/v1/staff/me") return ME(["support"]);
      if (path.startsWith("/v1/staff/handles?")) {
        await gate;
        if (fails) return fail(500, "store_error");
        return { data: [], next_cursor: null };
      }
      return undefined;
    },
  });
  assert.ok(p.$(".st-state.st-loading"), "loading while the postbox answers");
  release(); await tick();
  const err = p.$(".st-state.st-error");
  assert.ok(err, "the error state");
  assert.equal(err.getAttribute("role"), "alert");
  fails = false;
  [...err.querySelectorAll("button")].find((b) => b.textContent === "Try again").click();
  await tick();
  assert.ok(p.$(".st-state.st-empty"), "the empty state");
  assert.match(p.$(".st-state.st-empty").textContent, /No handles yet/);
});

test("a step-up refusal offers the strong sign-in and comes back to the same screen", async (t) => {
  const p = await page(t, {
    hash: "#orgs",
    routes: (m, path) => {
      if (path === "/v1/staff/me") return ME(["owner"]);
      if (path.startsWith("/v1/staff/orgs?")) return fail(401, "step_up_required", { acr_values: "mfa", max_age: 3600 });
      return undefined;
    },
  });
  const btn = p.$(".st-stepup");
  assert.ok(btn, p.$("#st-view").textContent);
  btn.click();
  await tick();
  assert.equal(p.navigations.length, 1);
  assert.equal(p.window.sessionStorage.getItem("pp_staff_return"), "#orgs");
  assert.ok(p.window.sessionStorage.getItem("pp_staff_pkce"));
});

test("the theme switch cycles system, light and dark and remembers the choice", async (t) => {
  const p = await page(t, { routes: (m, path) => (path === "/v1/staff/me" ? ME(["ops"]) : path === "/v1/staff/dashboard" ? DASH : undefined), storage: { pp_theme: "dark" } });
  const root = p.window.document.documentElement;
  assert.equal(root.getAttribute("data-theme"), "dark", "a stored choice applies on load");
  p.$("#st-theme").click();
  assert.equal(root.hasAttribute("data-theme"), false, "system");
  assert.equal(p.window.localStorage.getItem("pp_theme"), "system");
  p.$("#st-theme").click();
  assert.equal(root.getAttribute("data-theme"), "light");
  p.$("#st-theme").click();
  assert.equal(root.getAttribute("data-theme"), "dark");
  assert.match(p.$("#st-theme").getAttribute("aria-label"), /Theme: dark/);
});

test("the credits screen sends refunds and adjustments with a reason, the owner and one key per adjustment", async (t) => {
  let failNext = false;
  const p = await page(t, {
    hash: "#credits/account/acct_1",
    routes: (m, path, body) => {
      if (path === "/v1/staff/me") return ME(["finance"]);
      if (path === "/v1/staff/credits/owners/account/acct_1/balance") return { data: { available_cents: 2500, held_cents: 0 } };
      if (path.startsWith("/v1/staff/credits/owners/account/acct_1/history")) return { data: [{ type: "purchase", id: "0f8fad5b-d9cb-469f-a165-70867728950e", at: "2026-11-01T00:00:00Z", amount_cents: 2500, status: "succeeded", charge: { currency: "TRY", amount_minor: 99000 } }], next_cursor: null };
      if (path.startsWith("/v1/staff/credits/refunds")) return { data: [], next_cursor: null };
      if (path.startsWith("/v1/staff/credits/sales")) return { data: [], next_cursor: null };
      if (path === "/v1/staff/credits/adjustments" && failNext) { failNext = false; return fail(503, "finance_unavailable"); }
      if (m === "POST") return { data: { status: "pending", body } };
      return undefined;
    },
  });
  assert.match(p.$(".st-balance").textContent, /\$25\.00/);
  const refund = [...p.doc.querySelectorAll("button")].find((b) => b.textContent === "Refund");
  refund.click();
  const form = p.$('form[id^="st-ref-f-"]');
  form.querySelector("input").value = "charged twice";
  form.dispatchEvent(new p.window.Event("submit", { cancelable: true }));
  await tick();
  const sent = p.calls.find((c) => c.method === "POST" && /\/refund$/.test(c.path));
  assert.deepEqual(sent.body, { reason: "charged twice" });
  // Adjustments: dollars to cents and a confirmation. A failed submit keeps its key (a retry
  // adjusts at most once); a success resets the form and the next adjustment gets a new key.
  failNext = true;
  const fill = (cents) => { p.$("#st-adj-id").value = "acct_1"; p.$("#st-adj-amount").value = cents; p.$("#st-adj-reason").value = "goodwill"; p.$("#st-adj-confirm").checked = true; };
  const submit = async () => { p.$("#st-adj").dispatchEvent(new p.window.Event("submit", { cancelable: true })); await tick(); };
  fill("-5.25"); await submit();
  assert.ok(p.$("#st-adj .st-state.st-error"), "the failure is shown");
  assert.equal(p.$("#st-adj-amount").value, "-5.25", "a failed submit keeps the form");
  await submit();
  assert.equal(p.$("#st-adj-amount").value, "", "a success resets the form");
  assert.equal(p.$("#st-adj-confirm").checked, false);
  assert.match(p.$("#st-adj .st-form-status").textContent, /Adjusted acct_1 by -\$5\.25/);
  fill("3"); await submit();
  const adj = p.calls.filter((c) => c.path === "/v1/staff/credits/adjustments");
  assert.equal(adj.length, 3);
  assert.equal(adj[0].body.amount_cents, -525);
  assert.equal(adj[0].body.owner_id, "acct_1");
  assert.equal(adj[0].body.idempotency_key, adj[1].body.idempotency_key, "the retry reuses the failed attempt's key");
  assert.notEqual(adj[2].body.idempotency_key, adj[1].body.idempotency_key, "the next adjustment has its own key");
  assert.equal(adj[2].body.amount_cents, 300);
});

// ---- the page itself ---------------------------------------------------------------------------

test("staff.html reuses the account shell, loads nothing from elsewhere and is kept out of search", () => {
  assert.match(staffHtml, /<link rel="stylesheet" href="\/account\.css/);
  assert.match(staffHtml, /<link rel="stylesheet" href="\/staff\.css/);
  assert.match(staffHtml, /<meta name="robots" content="noindex, nofollow">/);
  for (const src of staffHtml.matchAll(/(?:src|href)="([^"]+)"/g)) assert.ok(src[1].startsWith("/"), `${src[1]} is same-origin`);
  assert.ok(!/<script>/.test(staffHtml), "no inline script");
  // An explicit theme re-declares the account tokens both ways, and the tables fold at phone width.
  assert.match(staffCss, /:root\[data-theme="light"\][^}]*--ac-bg/);
  assert.match(staffCss, /:root\[data-theme="dark"\][^}]*--ac-bg/);
  assert.match(staffCss, /@media \(max-width: 720px\)[\s\S]*td::before \{ content: attr\(data-label\)/);
});
