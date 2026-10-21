// End-to-end: the enterprise portal in headless Chrome, served with the production routing and CSP.
//
//   node test/e2e/run.mjs                       # everything mocked (postbox, bridge, realm)
//   POSTBOX_URL=http://127.0.0.1:8990 \        # a real local postbox instead of the mock
//   TOKEN_OWNER=… TOKEN_INVITEE=… TOKEN_OTHER=… ORG_ID=org_… \
//   node test/e2e/run.mjs
//
// Walks: packages → start wizard (sign-in, organization, billing, summary) → the isolated checkout
// page (its nonce-only CSP must run the provider's inline script) → checkout return polling → admin
// People (choose the owner's address, invite someone, copy the shown-once link) → the invitation as
// the wrong account (masked mismatch) and as the right one (welcome) → Rules (template + test).
//
// Against a real postbox the purchase is skipped: the organization must already be active and
// ORG_ID owned by TOKEN_OWNER (activate it through the internal entitlement API first). The
// postbox is reached same-origin through /__postbox/, so its CORS list does not matter here.
//
// Chrome: CHROME=/path/to/chrome (default: the macOS app path). A throwaway headless profile is
// created per browser under a temp dir and deleted afterwards, as tests/e2e-groups does.

import http from "node:http";
import { spawn } from "node:child_process";
import { existsSync, readFileSync, mkdtempSync, rmSync, statSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { randomBytes } from "node:crypto";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SITE = path.resolve(HERE, "../..");
const CATALOG = JSON.parse(readFileSync(path.resolve(SITE, "../store-adapter/catalog/pigeonpost-catalog.json"), "utf8"));
const CHROME = process.env.CHROME || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const REAL = process.env.POSTBOX_URL || null;
const work = mkdtempSync(path.join(os.tmpdir(), "ppe-e2e-"));
const children = [];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const log = (...a) => console.log("•", ...a);
async function until(fn, what, ms = 15000) {
  const end = Date.now() + ms;
  let last;
  while (Date.now() < end) {
    try { const v = await fn(); if (v) return v; } catch (e) { last = e; }
    await sleep(150);
  }
  throw new Error(`timed out: ${what}${last ? ` (${last.message})` : ""}`);
}

// ---- fake identities ---------------------------------------------------------------------------

const b64 = (o) => Buffer.from(JSON.stringify(o)).toString("base64url");
const fakeJwt = (user) => `${b64({ alg: "none" })}.${b64({ sub: `sub-${user}`, preferred_username: user, exp: 4102444800 })}.e2e`;
const TOKENS = {
  owner: process.env.TOKEN_OWNER || fakeJwt("owner"),
  invitee: process.env.TOKEN_INVITEE || fakeJwt("john"),
  other: process.env.TOKEN_OTHER || fakeJwt("someone"),
};
let currentUser = "owner"; // who the mock realm signs in next

// ---- mock postbox (docs/orgs.md subset) --------------------------------------------------------

const db = {
  org: null, members: [], rules: [], inviteTokens: new Map(),
};
const userOf = (req) => Object.entries(TOKENS).find(([, t]) => req.headers.authorization === `Bearer ${t}`)?.[0] || null;
function mockPostbox(req, res, url, body) {
  const json = (s, b) => { res.writeHead(s, { "content-type": "application/json" }); res.end(JSON.stringify(b)); };
  const p = url.pathname;
  const who = userOf(req);
  if (p === "/v1/org-invites/preview" && req.method === "POST") {
    const inv = db.inviteTokens.get(body.token);
    if (!inv) return json(404, { error: "invite_invalid" });
    return json(200, { org: { name: db.org.name, namespace: db.org.namespace }, handle: inv.handle, role_names: [], inviter: "Owner Person",
      contact_masked: "j***@acme.test", expires_at: Math.floor(Date.now() / 1000) + 7 * 86400, rooms: ["General"], requires_mfa: false });
  }
  if (!who) return json(401, { error: "unauthorized" });
  if (p === "/v1/capabilities") return json(200, { features: ["orgs", "enterprise_portal"] });
  if (p === "/v1/org-invites/accept") {
    const inv = db.inviteTokens.get(body.token);
    if (!inv) return json(404, { error: "invite_invalid" });
    if (who !== "invitee") return json(403, { error: "invite_address_mismatch", contact_masked: "j***@acme.test" });
    const m = db.members.find((x) => x.id === inv.member_id);
    Object.assign(m, { status: "active", joined_at: Math.floor(Date.now() / 1000) });
    delete m.invite;
    db.inviteTokens.delete(body.token);
    return json(200, { org: { id: db.org.id, name: db.org.name }, member: m, mailbox: { address: "/k/john", handle: m.handle } });
  }
  if (!db.org) return json(404, { error: "not_found" });
  const base = `/v1/orgs/${db.org.id}`;
  const owner = who === "owner";
  const roles = owner ? ["owner"] : [];
  const org = () => ({ ...db.org, seats_used: db.members.filter((m) => m.status !== "removed").length });
  if (p === "/v1/orgs") return json(200, { orgs: owner || who === "invitee" ? [{ ...org(), my_roles: roles }] : [], operated: [] });
  if (p === base && req.method === "GET") return json(200, { org: org(), me: { member_id: "mem_owner", roles, mailbox: db.ownerMailbox } });
  if (!owner) return json(403, { error: "forbidden" });
  if (p === `${base}/me/mailbox`) { db.ownerMailbox = { address: "/k/owner", handle: body.handle }; db.members[0].handle = body.handle; return json(201, { mailbox: db.ownerMailbox }); }
  if (p === `${base}/members`) return json(200, { members: db.members });
  if (p === `${base}/invites` && req.method === "POST") {
    const results = body.invites.map((i) => {
      const token = randomBytes(32).toString("base64url");
      const id = `mem_${randomBytes(4).toString("hex")}`;
      db.members.push({ id, first_name: i.first_name, last_name: i.last_name, handle: i.handle, roles: i.roles || [], status: "invited", contact_masked: "j***@acme.test", invited_at: Math.floor(Date.now() / 1000), invite: { id: `inv_${id}`, state: "pending" } });
      db.inviteTokens.set(token, { handle: i.handle, member_id: id });
      return { status: "invited", member_id: id, invite_id: `inv_${id}`, handle: i.handle, link: `${ORIGIN}/invite#${token}`, expires_at: Math.floor(Date.now() / 1000) + 7 * 86400 };
    });
    return json(200, { results });
  }
  if (p === `${base}/rules` && req.method === "GET") return json(200, { rules: db.rules, revision: db.rules.length });
  if (p === `${base}/rules` && req.method === "POST") { const r = { id: `rul_${db.rules.length + 1}`, ...body }; db.rules.push(r); return json(201, { rule: r }); }
  if (p === `${base}/rules/evaluate`) {
    const known = db.rules.some((r) => r.known && body.from.startsWith(`/${db.org.namespace}/`) && body.to.startsWith(`/${db.org.namespace}/`));
    return json(200, { admitted: true, known, autonomy: "review", verbs: [], matched_rule: known ? { id: db.rules[0].id, description: db.rules[0].description } : null, decided_by: known ? "org_rule" : "inbox_policy" });
  }
  return json(404, { error: "not_found", detail: p });
}

// ---- mock bridge -------------------------------------------------------------------------------

let statusPolls = 0;
function mockBridge(req, res, p, body) {
  const json = (s, b) => { res.writeHead(s, { "content-type": "application/json", "cache-control": "no-store" }); res.end(JSON.stringify(b)); };
  if (p === "/v1/catalog") {
    const plans = Object.entries(CATALOG.plans).map(([code, x]) => ({ code, family: x.family, currency: x.currency, interval: x.interval, seats: x.seats, amount: x.editions["2026"].amount, available: true }));
    return json(200, { open_currencies: ["TRY", "USD"], plans });
  }
  if (p === "/v1/auth/exchange") return json(200, { session: TOKENS[body.code] || "", refresh: null });
  if (p.startsWith("/v1/handles/")) return json(200, { name: p.split("/")[3], available: true, known: true });
  if (p === "/ent/sales") return json(202, { received: true });
  if (!req.headers.authorization) return json(401, { error: "sign in required" });
  if (p === "/ent/billing-profile") { if (!body.billing_email || !body.address) return json(400, { error: "bad" }); return json(201, { id: "00000000-0000-4000-8000-000000000001" }); }
  if (p === "/ent/checkout") {
    db.org = { id: "org_e2e000001", name: body.org.name, namespace: body.org.namespace, status: "pending", seats: Number(/^ent-(\d+)/.exec(body.plan_code)[1]), agent_limit: 300, agents_used: 0, plan_code: body.plan_code, settings: {}, created_at: Math.floor(Date.now() / 1000) };
    db.members = [{ id: "mem_owner", first_name: "Owner", last_name: "Person", handle: null, roles: ["owner"], status: "active", contact_masked: "o***@acme.test" }];
    return json(200, { intent_id: body.intent_id, subscription_id: "sub", plan_code: body.plan_code, amount: "1800.00", currency: "USD", interval: "year",
      org_id: db.org.id, checkout_form_content: `<div id="iyzipay-checkout-form" class="responsive">Mock payment form</div><script>document.getElementById("iyzipay-checkout-form").setAttribute("data-ran","1")</script><script src="https://evil.test/x.js"></script>` });
  }
  if (p.startsWith("/ent/checkout/")) {
    if (++statusPolls >= 2) db.org.status = "active";
    return json(200, { status: db.org.status === "active" ? "active" : "pending", org: db.org.status === "active" ? db.org : null });
  }
  if (p === "/v1/invoices") return json(200, { invoices: [] });
  return json(404, { error: "not found" });
}

// ---- the site, with nginx's routing and headers ------------------------------------------------

const CSP = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'; object-src 'none'";
const TYPES = { ".html": "text/html; charset=utf-8", ".js": "text/javascript", ".css": "text/css", ".png": "image/png", ".ico": "image/x-icon", ".woff2": "font/woff2", ".txt": "text/plain" };
let ORIGIN = "";

function routeFile(p) {
  if (p === "/" ) return "index.html";
  if (p === "/app" || p.startsWith("/app/")) return "app.html";
  if (p === "/checkout") return "checkout.html";
  if (p === "/checkout/return") return "checkout/return.html";
  if (p === "/auth/callback") return "auth/callback.html";
  if (p.endsWith(".html") && p !== "/") return null;
  const direct = path.join(SITE, p);
  if (!direct.startsWith(SITE + path.sep)) return null;
  if (existsSync(direct) && statSync(direct).isFile()) return p.slice(1);
  if (existsSync(direct + ".html")) return p.slice(1) + ".html";
  return null;
}

async function readBody(req) {
  const chunks = [];
  for await (const c of req) chunks.push(c);
  const text = Buffer.concat(chunks).toString("utf8");
  try { return text ? JSON.parse(text) : {}; } catch { return {}; }
}

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, ORIGIN);
  const p = url.pathname;
  if (p.startsWith("/__postbox/")) {
    if (REAL) {
      const upstream = await fetch(REAL + p.slice("/__postbox".length) + url.search, { method: req.method, headers: { ...(req.headers.authorization ? { authorization: req.headers.authorization } : {}), "content-type": "application/json" },
        body: ["GET", "HEAD"].includes(req.method) ? undefined : JSON.stringify(await readBody(req)) });
      res.writeHead(upstream.status, { "content-type": upstream.headers.get("content-type") || "application/json", ...(upstream.headers.get("www-authenticate") ? { "www-authenticate": upstream.headers.get("www-authenticate") } : {}) });
      res.end(Buffer.from(await upstream.arrayBuffer()));
      return;
    }
    url.pathname = p.slice("/__postbox".length);
    return mockPostbox(req, res, url, await readBody(req));
  }
  if (p.startsWith("/api/")) return mockBridge(req, res, p.slice(4), await readBody(req));
  // The mock realm: authorize signs in `currentUser` and returns straight to the callback.
  if (p === "/realm/protocol/openid-connect/auth") {
    const back = new URL(url.searchParams.get("redirect_uri"));
    back.searchParams.set("code", currentUser);
    back.searchParams.set("state", url.searchParams.get("state"));
    lastAuthorize = url;
    res.writeHead(302, { location: back.toString() }); res.end(); return;
  }
  if (p === "/realm/protocol/openid-connect/logout") { res.writeHead(302, { location: url.searchParams.get("post_logout_redirect_uri") || "/" }); res.end(); return; }
  if (p === "/config.js") {
    const cfg = readFileSync(path.join(SITE, "config.js"), "utf8")
      .replace('postbox: "https://postbox.pigeonpost.dev"', `postbox: "${ORIGIN}/__postbox"`)
      .replace('issuer: "https://auth.pigeonpost.dev/realms/pigeonpost-prod"', `issuer: "${ORIGIN}/realm"`)
      .replace('inbox: "https://inbox.pigeonpost.dev"', `inbox: "${ORIGIN}/inbox-stub"`);
    res.writeHead(200, { "content-type": TYPES[".js"], "content-security-policy": CSP }); res.end(cfg); return;
  }
  const file = routeFile(p);
  if (!file) { res.writeHead(404, { "content-type": "text/plain" }); res.end("not found"); return; }
  let data = readFileSync(path.join(SITE, file));
  let csp = CSP;
  if (file === "checkout.html") {
    const nonce = randomBytes(12).toString("hex");
    data = Buffer.from(data.toString("utf8").replace("__CSP_NONCE__", nonce));
    csp = `default-src 'self'; script-src 'self' 'nonce-${nonce}'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'`;
  }
  res.writeHead(200, { "content-type": TYPES[path.extname(file)] || "application/octet-stream", "content-security-policy": csp, "x-content-type-options": "nosniff" });
  res.end(data);
});
let lastAuthorize = null;
await new Promise((r) => server.listen(0, "127.0.0.1", r));
ORIGIN = `http://127.0.0.1:${server.address().port}`;
log("portal at", ORIGIN, REAL ? `against postbox ${REAL}` : "with mock postbox");

// ---- Chrome over the DevTools protocol (as tests/e2e-groups/run.mjs) --------------------------

async function browser(label) {
  const dir = path.join(work, "chrome-" + label);
  const proc = spawn(CHROME, ["--headless=new", "--remote-debugging-port=0", `--user-data-dir=${dir}`, "--no-first-run", "--no-default-browser-check", "--disable-gpu", "--window-size=1280,900", "about:blank"], { stdio: "ignore" });
  children.push(proc);
  const port = await until(() => { const f = path.join(dir, "DevToolsActivePort"); return existsSync(f) && readFileSync(f, "utf8").split("\n")[0]; }, "chrome start " + label, 20000);
  const target = await until(async () => (await (await fetch(`http://127.0.0.1:${port}/json/list`)).json()).find((t) => t.type === "page"), "page target");
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((r, j) => { ws.onopen = r; ws.onerror = j; });
  let id = 0;
  const waiting = new Map();
  const problems = [];
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    if (msg.id && waiting.has(msg.id)) { const { resolve, reject } = waiting.get(msg.id); waiting.delete(msg.id); msg.error ? reject(new Error(msg.error.message)) : resolve(msg.result); }
    if (msg.method === "Runtime.exceptionThrown") problems.push("EXCEPTION " + (msg.params.exceptionDetails.exception?.description || msg.params.exceptionDetails.text));
    if (msg.method === "Runtime.consoleAPICalled" && msg.params.type === "error") problems.push("console.error " + msg.params.args.map((a) => a.value ?? a.description).join(" "));
    if (msg.method === "Log.entryAdded" && msg.params.entry.level === "error") problems.push(`${msg.params.entry.source}: ${msg.params.entry.text}`);
  };
  const send = (method, params = {}) => new Promise((resolve, reject) => { const n = ++id; waiting.set(n, { resolve, reject }); ws.send(JSON.stringify({ id: n, method, params })); });
  await send("Runtime.enable"); await send("Page.enable"); await send("Log.enable");
  const evaluate = async (expression) => {
    const r = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text);
    return r.result.value;
  };
  const go = async (url) => { await send("Page.navigate", { url }); await sleep(300); };
  const text = (sel = "body") => evaluate(`(document.querySelector(${JSON.stringify(sel)}) || {}).innerText || ""`);
  const click = (sel, label) => evaluate(`(() => { const all = [...document.querySelectorAll(${JSON.stringify(sel)})]; const b = ${label ? `all.find((x) => x.textContent.trim().includes(${JSON.stringify(label)}))` : "all[0]"}; if (!b || b.disabled) return false; b.click(); return true; })()`);
  const fill = (sel, value, index = 0) => evaluate(`(() => { const i = document.querySelectorAll(${JSON.stringify(sel)})[${index}]; if (!i) return false; i.value = ${JSON.stringify(value)}; i.dispatchEvent(new Event("input", { bubbles: true })); i.dispatchEvent(new Event("change", { bubbles: true })); return true; })()`);
  const where = () => evaluate("location.pathname + location.search + location.hash");
  return { label, evaluate, go, text, click, fill, where, problems, close: () => { ws.close(); proc.kill("SIGKILL"); } };
}

let failed = false;
const check = (cond, what) => { if (!cond) throw new Error("check failed: " + what); log("ok:", what); };

try {
  const A = await browser("owner");
  let orgId = process.env.ORG_ID || null;

  if (!REAL) {
    // 1. Packages.
    await A.go(ORIGIN + "/");
    await until(() => A.text("#price").then((t) => /1,800/.test(t)), "packages price");
    check(/30 people · up to 300 agent addresses · 150 GB pooled storage/.test(await A.text("#summary")), "packages summary for 30 seats");
    await A.evaluate(`(() => { const s = document.getElementById("seats"); s.value = "9"; s.dispatchEvent(new Event("input")); })()`);
    check(!(await A.evaluate(`document.getElementById("sales").hidden`)), "500+ shows Talk to sales");
    await A.evaluate(`(() => { const s = document.getElementById("seats"); s.value = "4"; s.dispatchEvent(new Event("input")); })()`);
    await A.click("#start");

    // 2. Start wizard: sign in through the (mock) realm, then the three steps.
    await until(() => A.where().then((w) => w.startsWith("/start")), "on /start");
    await until(() => A.click("#wizard button", "Sign in to continue"), "sign-in button");
    await until(() => A.text("#wizard").then((t) => t.includes("Your organization")), "back on step 1 signed in", 20000);
    const authorize = lastAuthorize;
    check(authorize.searchParams.get("code_challenge_method") === "S256" && authorize.searchParams.get("redirect_uri") === ORIGIN + "/auth/callback", "PKCE authorize with the one redirect path");
    await A.fill("#wizard input", "Acme Inc.", 0);
    await until(() => A.text("#ns-status").then((t) => t.includes("/acme is available")), "namespace available");
    await A.click("#wizard button[type=submit]");
    await until(() => A.text("#wizard").then((t) => t.includes("Billing details")), "step 2");
    const fields = { 1: "Acme Teknoloji A.Ş.", 2: "Kadıköy", 3: "1234567890", 5: "billing@acme.test", 7: "Moda Cad. 1", 8: "Kadıköy", 9: "İstanbul", 10: "34710" };
    for (const [i, v] of Object.entries(fields)) await A.fill("#wizard input", v, Number(i) - 1);
    await A.click("#wizard button[type=submit]");
    await until(() => A.text("#wizard").then((t) => t.includes("Review and pay")), "step 3", 10000);
    check((await A.text("#wizard")).includes("$1,800 per year"), "summary shows the catalog price");
    await A.click("#wizard button", "Continue to secure payment");

    // 3. The isolated checkout page runs only the provider's nonce'd inline script.
    await until(() => A.evaluate(`document.getElementById("iyzipay-checkout-form")?.getAttribute("data-ran") === "1"`), "provider script ran under the nonce CSP", 10000);
    check(!(await A.evaluate(`[...document.scripts].some((s) => s.src.includes("evil.test"))`)), "non-provider script URLs are dropped");
    orgId = await A.evaluate(`sessionStorage.getItem("ppe_ent_draft")`);
    check(/^org_/.test(orgId || ""), "draft id kept for the return page");

    // 4. Return polling until active, then the admin app.
    await A.go(`${ORIGIN}/checkout/return?d=${orgId}`);
    await until(() => A.where().then((w) => w.startsWith("/app/people")), "redirected to People", 30000);
  } else {
    currentUser = "owner";
    await A.go(`${ORIGIN}/app/people?org=${orgId}`);
    await until(() => A.click("#gate button", "Sign in"), "sign in to app");
  }

  // 5. People: first-run address, invite.
  await until(() => A.text("#content").then((t) => t.includes("People")), "People page", 20000);
  if (await A.text("#content").then((t) => t.includes("Choose your address"))) {
    await A.fill("#content form input", "owner");
    await A.click("#content form button[type=submit]");
    await until(() => A.text("#content").then((t) => !t.includes("Choose your address")), "owner address chosen", 10000);
    log("owner chose /acme/owner");
  }
  await A.click("#content .page-head .btn-primary");
  await until(() => A.evaluate(`Boolean(document.querySelector(".sheet .invite-row"))`), "invite sheet");
  await A.fill(".sheet .invite-row input", "John", 0);
  await A.fill(".sheet .invite-row input", "Smith", 1);
  await A.fill(".sheet .invite-row input", process.env.INVITE_CONTACT || "john@acme.test", 2);
  await A.click(".sheet button", "Send invitations");
  const link = await until(() => A.evaluate(`document.querySelector(".sheet .secret code")?.textContent || ""`), "invite link shown", 10000);
  check(/\/invite#[A-Za-z0-9_-]{40,}$/.test(link), "invite link carries the token in the fragment");
  check((await A.text(".sheet")).includes("Shown once"), "link is marked shown once");
  await A.click(".sheet button", "Done");

  // 6. The invitation: wrong account first, then the invited one.
  const B = await browser("invitee");
  const local = link.replace(/^https:\/\/enterprise\.pigeonpost\.dev/, ORIGIN);
  await B.go(local);
  await until(() => B.text("#invite").then((t) => t.includes("invites you")), "invite preview");
  check(!(await B.where()).includes("#"), "token removed from the address bar");
  currentUser = "other";
  await B.click("#invite button", "Sign in to accept");
  await until(() => B.text("#invite").then((t) => t.includes("Accept invitation")), "back signed in as the wrong account", 15000);
  await B.click("#invite button", "Accept invitation");
  await until(() => B.text("#invite").then((t) => t.includes("for another address")), "masked mismatch");
  currentUser = "invitee";
  await B.click("#invite button", "Switch account");
  await until(() => B.text("#invite").then((t) => t.includes("Accept invitation")), "back signed in as the invitee", 15000);
  await B.click("#invite button", "Accept invitation");
  await until(() => B.text("#invite").then((t) => t.includes("Welcome to")), "welcome");
  check((await B.text("#invite")).includes("/acme/john") || Boolean(REAL), "welcome shows the company address");

  // 7. Rules: a template and a test.
  await A.go(`${ORIGIN}/app/rules`);
  await until(() => A.text("#content").then((t) => t.includes("Rules")), "Rules page", 15000);
  await A.click(".templates button", "Everyone at the company is known");
  await until(() => A.evaluate(`document.querySelectorAll(".rule").length > 0`), "template rule shown", 10000);
  await A.click("#content button", "Test a rule");
  await A.fill(".sheet input", "/acme/john", 0);
  await A.fill(".sheet input", "/acme/owner", 1);
  await A.click(".sheet button", "Test");
  await until(() => A.text(".sheet").then((t) => t.includes("Delivered")), "rule test result");
  check((await A.text(".sheet")).includes("Known contact"), "test explains the outcome");

  for (const b of [A, B]) {
    const bad = b.problems.filter((p) => !/favicon|ERR_NAME_NOT_RESOLVED|inbox-stub|evil\.test|status of 40[13] /.test(p));
    if (bad.length) { console.error(`${b.label} page problems:\n  ${bad.join("\n  ")}`); failed = true; }
  }
  if (!failed) log("E2E OK");
} catch (e) {
  failed = true;
  console.error("E2E FAILED:", e.message);
} finally {
  for (const c of children) try { c.kill("SIGKILL"); } catch { /* gone */ }
  server.close();
  try { rmSync(work, { recursive: true, force: true }); } catch { /* best effort */ }
  process.exit(failed ? 1 : 0);
}
