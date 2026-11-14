// Headless Chrome screenshots of an assembled copy of management.pigeonpost.dev.
//
//   node test/screenshots.mjs <out-dir>
//
// Copies site-management/ and site-shared/ (as /shared/) into a temp directory, adds a TEST-ONLY
// page (harness.html + harness.js: a fake session, a stub fetch and a fake WebSocket) that exists
// only in that temp directory, serves it with `python3 -m http.server`, and captures each screen
// at 1440×900 and 390×844. Nothing written here is part of the site.
import { mkdtempSync, mkdirSync, cpSync, readFileSync, writeFileSync, readdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const here = fileURLToPath(new URL("..", import.meta.url));
const shared = fileURLToPath(new URL("../../site-shared/", import.meta.url));
const out = process.argv[2] || join(tmpdir(), "pm-shots");
mkdirSync(out, { recursive: true });
const root = mkdtempSync(join(tmpdir(), "pm-site-"));
for (const f of readdirSync(here)) if (!["test", "node_modules", "package.json", "package-lock.json"].includes(f)) cpSync(join(here, f), join(root, f), { recursive: true });
mkdirSync(join(root, "shared"));
for (const f of readdirSync(shared)) if (/\.(js|css)$/.test(f) || f === "fonts") cpSync(join(shared, f), join(root, "shared", f), { recursive: true });

// ---- the test-only harness ---------------------------------------------------------------------
const harness = String.raw`
(function () {
  var q = new URLSearchParams(location.search);
  var b = function (o) { return btoa(unescape(encodeURIComponent(JSON.stringify(o)))).replace(/=+$/, "").replace(/\+/g, "-").replace(/\//g, "_"); };
  if (q.get("as") !== "out") sessionStorage.setItem("pp_session", b({ alg: "none" }) + "." + b({ sub: "s", preferred_username: "bekir", name: "Bekir Dağ" }) + ".x");
  try { localStorage.setItem("ppi_theme", q.get("theme") || "light"); } catch (e) {}
  var roles = q.get("as") === "none" ? null : ["owner", "support", "finance", "reviewer", "ops"];
  var iso = function (d) { return new Date(Date.now() - d * 3600e3).toISOString(); };
  var unix = function (d) { return Math.floor(Date.now() / 1000 - d * 3600); };
  var apps = [
    ["pub_wodo", "Wodo Teknoloji A.Ş.", "submitted", "company", "TR", 30], ["pub_lumen", "Lumen Labs Ltd", "submitted", "company", "GB", 50],
    ["pub_kerem", "Kerem Yılmaz", "in_review", "individual", "TR", 70], ["pub_north", "Northwind Analytics Inc.", "needs_information", "company", "US", 120],
    ["pub_atlas", "Atlas Research GmbH", "submitted", "company", "DE", 200]];
  var dossier = { id: "pub_wodo", state: "submitted", revision: 3, kind: "company", country: "TR", kyc_status: "not_started", submitted_at: unix(30), updated_at: unix(30),
    details: { legal_name: "Wodo Teknoloji Anonim Şirketi", registration_number: "0947 1234 5600 0017", tax_id: "9470123456", duns: "", duns_unavailable_reason: "Applied, number pending from Dun & Bradstreet", address: "Maslak Mah. Büyükdere Cad. 255, Sarıyer, İstanbul", contact_name: "Bekir Dağ", contact_email: "legal@wodo.io", bank_name: "Garanti BBVA", account_holder: "Wodo Teknoloji A.Ş.", iban: "TR330006100519786457841326", swift: "TGBATRIS", currency: "TRY", bank_country: "TR", public_name: "Wodo", public_summary: "AI hiring tools: research reports on candidates, delivered as PDF.", website: "https://wodo.io" },
    documents: [{ id: "d1", category: "registration", filename: "faaliyet-belgesi-2026.pdf", size: 412000, created_at: unix(31) }, { id: "d2", category: "tax", filename: "vergi-levhasi.pdf", size: 198000, created_at: unix(31) }, { id: "d3", category: "authorization", filename: "imza-sirkuleri.pdf", size: 260000, created_at: unix(31) }],
    requested_documents: [], history: [{ actor: "acct_wodo", event: "submitted", revision: 3, note: null, created_at: unix(30) }, { actor: "acct_staff", event: "needs_information", revision: 2, note: "Please upload the signature circular (imza sirküleri).", created_at: unix(80) }, { actor: "acct_wodo", event: "submitted", revision: 1, note: null, created_at: unix(120) }] };
  var accounts = [["acct_7Hq2", "bekir@wodo.io", 3, 5, true, 2000], ["acct_9Lm1", "emre@wodo.io", 1, 2, false, 900], ["acct_2Xc8", "marina.k@gmail.com", 1, 1, false, 400], ["acct_4Pz0", "hello@lumenlabs.co", 2, 3, false, 100], ["acct_8Rt5", "kerem@yilmaz.dev", 1, 1, false, 20]];
  var tickets = [
    { id: "t1", subject: "When does the first payout run?", publisher_name: "Wodo", publisher_id: "pub_wodo", status: "open", priority: "high", category: "billing", updated_at: iso(0.3), unread: true },
    { id: "t2", subject: "Release 1.0.4 stuck in behaviour test", publisher_name: "Lumen Labs", publisher_id: "pub_lumen", status: "open", priority: "normal", category: "apps", updated_at: iso(2), unread: false },
    { id: "t3", subject: "Which tax certificate do you need?", publisher_name: "Northwind Analytics", publisher_id: "pub_north", status: "waiting_partner", priority: "normal", category: "application", updated_at: iso(20), unread: false }];
  var R = {
    "/v1/staff/me": function () { return roles ? [200, { data: { account_id: "acct_7Hq2", roles: roles, platform_owner: true, step_up: { acr_values: "gold" }, finance_configured: true } }] : [403, { error: "staff_required" }]; },
    "/v1/staff/dashboard": [200, { data: { counts: { signups_today: 14, signups_7d: 96, accounts: 4812, active_accounts_30d: 2130, handles: 3920, mailboxes: 6104, orgs_active: 37, nests_running: 58, open_reviews: 2, publisher_applications: 4, open_reports: 1, hosted_jobs_failed_24h: 0 }, alerts: [], as_of: iso(0) } }],
    "/v1/staff/credits/sales": [200, { data: [{ amount_cents: 2500, commission_cents: 250 }, { amount_cents: 2500, commission_cents: 250 }, { amount_cents: 5000, commission_cents: 500 }], next_cursor: null }],
    "/v1/staff/credits/refunds": [200, { data: [], next_cursor: null }],
    "/v1/publishers": [200, { data: [{ id: "pub_wodo", display_name: "Wodo" }] }],
    "/v1/orgs": [200, { orgs: [{ id: "org_wodo" }] }],
    "/v1/staff/publisher-dossiers": [200, { data: apps.map(function (a) { return { id: a[0], display_name: a[1], state: a[2], kind: a[3], country: a[4], revision: 3, updated_at: iso(a[5]), kyc_status: a[3] === "individual" ? "pending" : "not_started", reviewer: a[2] === "in_review" ? "acct_7Hq2" : null, conflict: false }; }), total: 5, next_offset: null }],
    "/v1/staff/publisher-dossiers/pub_wodo": [200, { data: dossier }],
    "/v1/staff/roles": [200, { data: [{ id: "r1", account_id: "acct_7Hq2", role: "reviewer", granted_by: "platform", granted_at: iso(900) }, { id: "r2", account_id: "acct_9Lm1", role: "support", granted_by: "acct_7Hq2", granted_at: iso(300) }] }],
    "/v1/staff/accounts": [200, { data: accounts.map(function (a) { return { id: a[0], sign_in_address: a[1], handles: a[2], mailboxes: a[3], staff: a[4], created_at: iso(a[5]) }; }), next_cursor: "c2" }],
    "/v1/staff/accounts/acct_7Hq2": [200, { data: { id: "acct_7Hq2", created_at: iso(2000), oidc_subject: "f3a1c9e2-7d4b-4e0a-9a51-2b7c0d9e8f11", sign_in_identities: [{ provider: "realm", login: "bekir@wodo.io", verified_at: iso(2000) }, { provider: "github", login: "bekirdag", verified_at: iso(1900) }], staff_roles: [{ role: "reviewer" }], handles: [{ namespace: "bekir", source: "apple", expires_at: iso(-4000) }, { namespace: "wodo", source: "entitlement" }, { namespace: "piyote", source: "grant" }], mailboxes: [{ address: "/bekir", handle: "/bekir", label: "Personal", kind: "account", created_at: iso(2000) }, { address: "/bekir/pa", handle: "/bekir/pa", label: "Assistant", kind: "nest", created_at: iso(500) }], devices: [{ platform: "ios", os: "iOS 18.1", kind: "app", mailbox: "/bekir", push_token: "a9f0c1d2e3b4a5f6c7d8e9f0a1b2c3d4", updated_at: iso(1) }], entitlements: [{ plan_code: "cloud_pro", source: "iyzico", status: "active", period_end: iso(-500), provider_ref: "sub_19ab" }], subscriptions: { apple: [{ product_id: "handle.annual", expires_at: iso(-4000), reference: "2000000812345678" }], google: [], nest_store: [{ package: "pigeonpost_ai", store: "web", state: "active", expires_at: iso(-300), reference: "nst_77" }] }, nests: [{ label: "Assistant", state: "running", funding: "plan" }], orgs: [{ id: "org_wodo", name: "Wodo", relation: "owner", status: "active" }], recent_audit: [] } }],
    "/v1/staff/orgs": [200, { data: [{ id: "org_wodo", name: "Wodo", namespace: "wodo", status: "active", seats: 25, active_members: 18, agents: 6, plan_code: "team_25" }, { id: "org_lumen", name: "Lumen Labs", namespace: "lumen", status: "past_due", seats: 10, active_members: 9, agents: 2, plan_code: "team_10" }], next_cursor: null }],
    "/v1/support/tickets": [200, { data: tickets, next_offset: null }],
    "/v1/support/tickets/t1": [200, { data: { id: "t1", subject: "When does the first payout run?", publisher_name: "Wodo", publisher_id: "pub_wodo", publisher_status: "approved", status: "open", priority: "high", category: "billing", assigned_to: "acct_7Hq2", last_seq: 3, older_before: null, messages: [
      { seq: 1, author_kind: "partner", body: "Hi! Our first sales came in this week. When does the first payout run, and do you need an invoice from us first?", created_at: iso(5) },
      { seq: 2, author_kind: "staff", body: "Congratulations on the first sales! Payouts run monthly on the 1st. We need your invoice for the statement before the transfer.", created_at: iso(4) },
      { seq: 3, author_kind: "partner", body: "Great — where do we upload the invoice?", created_at: iso(0.3) }] } }],
    "/v1/staff/reviews": [200, { data: [{ id: "rvw_01", publisher_name: "Wodo", publisher_id: "pub_wodo", version: "1.0.4", stages_recorded: ["intake", "deterministic_scan", "behavioral_test"], revision: 2, first_party: false, conflict: false }] }],
  };
  window.fetch = function (u, init) {
    var url = new URL(String(u), location.href);
    var r = R[url.pathname] || ((init && init.method && init.method !== "GET") ? [200, { data: {} }] : [200, { data: [], next_cursor: null }]);
    if (typeof r === "function") r = r();
    return new Promise(function (res) { setTimeout(function () { res(new Response(JSON.stringify(r[1]), { status: r[0], headers: { "content-type": "application/json" } })); }, 20); });
  };
  if (q.get("debug")) setTimeout(function () { var W = document.documentElement.clientWidth, bad = []; document.querySelectorAll("body *").forEach(function (n) { var r = n.getBoundingClientRect(); if (r.right > W + 1 && r.width) bad.push(n.tagName + "." + String(n.className && n.className.baseVal !== undefined ? n.className.baseVal : n.className).split(" ").join(".") + ":" + Math.round(r.width)); }); var pre = document.createElement("pre"); pre.id = "overflow"; pre.textContent = "OVERFLOW " + W + " " + bad.slice(0, 25).join(" | "); document.body.appendChild(pre); }, 2500);
  window.WebSocket = function () { var s = this; s.send = function () {}; s.close = function () {}; setTimeout(function () { s.onopen && s.onopen(); s.onmessage && s.onmessage({ data: JSON.stringify({ type: "ready" }) }); }, 50); };
})();`;
writeFileSync(join(root, "harness.js"), harness);
writeFileSync(join(root, "harness.html"), readFileSync(join(root, "index.html"), "utf8").replace('<script src="/shared/theme.js"></script>', '<script src="/harness.js"></script>\n<script src="/shared/theme.js"></script>'));

const shots = [
  ["overview", "#/overview"], ["applications", "#/applications"], ["application", "#/applications/pub_wodo"],
  ["accounts", "#/accounts"], ["account", "#/accounts/acct_7Hq2"], ["enterprises", "#/enterprises"],
  ["support", "#/support/t1"], ["credits", "#/credits"], ["nests-soon", "#/nests"], ["staff", "#/staff"],
  ["no-access", "#/overview", "as=none"], ["signed-out", "", "as=out"], ["overview-dark", "#/overview", "theme=dark"], ["application-dark", "#/applications/pub_wodo", "theme=dark"],
];
const port = 8700 + Math.floor(Math.random() * 200);
const server = spawn("python3", ["-m", "http.server", String(port), "--bind", "127.0.0.1"], { cwd: root, stdio: "ignore" });
await new Promise((r) => setTimeout(r, 900));
const chrome = process.env.CHROME || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const profile = mkdtempSync(join(tmpdir(), "pm-chrome-"));
// Chrome on macOS will not size a window below 500 px, so phone widths are emulated over the
// DevTools protocol (Emulation.setDeviceMetricsOverride) instead of --window-size.
const cdpPort = 9300 + Math.floor(Math.random() * 500);
const browser = spawn(chrome, ["--headless=new", "--disable-gpu", "--hide-scrollbars", `--user-data-dir=${profile}`, `--remote-debugging-port=${cdpPort}`, "about:blank"], { stdio: "ignore" });
let target = null;
for (let i = 0; i < 50 && !target; i++) {
  await new Promise((r) => setTimeout(r, 200));
  try { target = (await (await fetch(`http://127.0.0.1:${cdpPort}/json/list`)).json()).find((x) => x.type === "page"); } catch { /* starting */ }
}
const ws = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((r) => ws.addEventListener("open", r, { once: true }));
let seq = 0;
const pending = new Map();
ws.addEventListener("message", (e) => { const m = JSON.parse(e.data); if (m.id && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id); } });
const send = (method, params = {}) => new Promise((r) => { const id = ++seq; pending.set(id, r); ws.send(JSON.stringify({ id, method, params })); });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const only = process.env.SHOTS ? process.env.SHOTS.split(",") : null;
try {
  for (const [name, hash, query] of shots.filter(([n]) => !only || only.includes(n))) {
    for (const [w, h, tag] of [[1440, 900, "desktop"], [390, 844, "phone"]]) {
      await send("Emulation.setDeviceMetricsOverride", { width: w, height: h, deviceScaleFactor: 1, mobile: w < 600 });
      await send("Page.navigate", { url: "about:blank" });
      await send("Page.navigate", { url: `http://127.0.0.1:${port}/harness.html${query ? "?" + query : ""}${hash}` });
      await sleep(1600);
      const over = await send("Runtime.evaluate", { returnByValue: true, expression: `(() => { const W = document.documentElement.clientWidth; const bad = []; document.querySelectorAll("body *").forEach((n) => { const r = n.getBoundingClientRect(); if (r.width && r.right > W + 1 && !n.closest(".pp-side, .pp-table-host, .pm-chips")) bad.push(n.tagName + "." + String(n.getAttribute("class") || "").replace(/ /g, ".")); }); return { W, scroll: document.documentElement.scrollWidth, bad: bad.slice(0, 8) }; })()` });
      const v = over.result.result.value;
      if (v.scroll > v.W || v.bad.length) console.log(`OVERFLOW ${name}-${tag}: width ${v.W}, scrollWidth ${v.scroll}`, v.bad.join(" | "));
      const shot = await send("Page.captureScreenshot", { format: "png" });
      const file = join(out, `${name}-${tag}.png`);
      writeFileSync(file, Buffer.from(shot.result.data, "base64"));
      console.log(file);
    }
  }
} finally { ws.close(); browser.kill(); server.kill(); }
