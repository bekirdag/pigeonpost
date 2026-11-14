// The shared shell: signed-out screen, sign-in return, role-filtered navigation, the platform
// switcher, routing, error states and the UI components every platform uses.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { webcrypto } from "node:crypto";
import { JSDOM, VirtualConsole } from "jsdom";

const read = (p) => readFileSync(new URL(`../${p}`, import.meta.url), "utf8");
const tick = async (n = 40) => { for (let i = 0; i < n; i++) await new Promise((r) => setImmediate(r)); };
const jwt = (claims = {}) => {
  const b = (o) => Buffer.from(JSON.stringify(o)).toString("base64url");
  return `${b({ alg: "none" })}.${b({ sub: "s1", preferred_username: "ada", name: "Ada Lovelace", ...claims })}.sig`;
};
const reply = (status, body) => ({ ok: status >= 200 && status < 300, status, headers: { get: () => null }, text: async () => JSON.stringify(body), json: async () => body });

function boot(t, { url = "https://account.pigeonpost.dev/", session, fetch } = {}) {
  const dom = new JSDOM("<!doctype html><html><body></body></html>", { url, runScripts: "outside-only", pretendToBeVisual: true, virtualConsole: new VirtualConsole() });
  t.after(() => dom.window.close());
  const w = dom.window;
  Object.defineProperty(w, "crypto", { value: webcrypto, configurable: true });
  const nav = [];
  w.PP_CONFIG = { postbox: "https://postbox.pigeonpost.dev", apiBase: "/api", oidc: { issuer: "https://auth.pigeonpost.dev/realms/pigeonpost-prod", clientId: "pigeonpost-web", scope: "openid", redirectPath: "/" } };
  const calls = [];
  w.fetch = async (u, init = {}) => { calls.push({ url: String(u), init }); return fetch ? fetch(String(u), init) : reply(404, { error: "not_found" }); };
  if (session) w.sessionStorage.setItem("pp_session", session);
  for (const f of ["theme.js", "core.js", "shell.js", "ui.js"]) w.eval(read(f));
  w.PP.navigate = (u) => nav.push(u);
  return { w, nav, calls };
}

const PAGES = (log) => [
  { key: "overview", title: "Overview", group: "", icon: "home", render: (v) => { log.push("overview"); v.append(v.ownerDocument.createElement("h1")); v.querySelector("h1").textContent = "Overview"; } },
  { key: "handles", title: "Handles", group: "Identity", icon: "at", count: () => 3, render: (v, ctx, params) => { log.push(`handles:${params.join(",")}`); } },
  { key: "secret", title: "Secret", group: "Admin", allow: (ctx) => ctx.admin, render: () => log.push("secret") },
  { key: "broken", title: "Broken", group: "Identity", render: () => { throw Object.assign(new Error("boom"), { code: "billing_unavailable" }); } },
];

test("signed out: the sign-in screen offers sign-in and registration, and sign-in goes to the realm with PKCE", async (t) => {
  const { w, nav } = boot(t);
  await w.PP.shell.start({ platform: "account", title: "Account", intro: { heading: "Your Pigeonpost", points: ["Inboxes"] }, context: async () => ({}), pages: PAGES([]) });
  const doc = w.document;
  assert.equal(doc.querySelector(".pp-signin-card h1").textContent, "Your Pigeonpost");
  const [signIn, register] = doc.querySelectorAll(".pp-signin-actions button");
  assert.equal(signIn.textContent, "Sign in");
  assert.equal(register.textContent, "Create an account");
  signIn.click();
  await tick();
  assert.equal(nav.length, 1);
  const u = new URL(nav[0]);
  assert.equal(u.origin + u.pathname, "https://auth.pigeonpost.dev/realms/pigeonpost-prod/protocol/openid-connect/auth");
  assert.equal(u.searchParams.get("client_id"), "pigeonpost-web");
  assert.equal(u.searchParams.get("redirect_uri"), "https://account.pigeonpost.dev/");
  assert.equal(u.searchParams.get("code_challenge_method"), "S256");
  // The verifier survives the round trip in localStorage.
  assert.ok(JSON.parse(w.localStorage.getItem("pp_pkce")).verifier);
  register.click();
  await tick();
  assert.equal(new URL(nav[1]).searchParams.get("prompt"), "create");
});

test("signed in: navigation is grouped, role-filtered and counted; the user menu shows the real name", async (t) => {
  const log = [];
  const { w } = boot(t, { session: jwt() });
  await w.PP.shell.start({ platform: "account", title: "Account", context: async () => ({ admin: false, platforms: ["partner"] }), pages: PAGES(log) });
  await tick();
  const doc = w.document;
  const items = [...doc.querySelectorAll(".pp-nav-item")].map((a) => a.getAttribute("data-page"));
  assert.deepEqual(items, ["overview", "handles", "broken"], "the admin page is hidden");
  assert.equal(doc.querySelector('[data-page="handles"] .pp-nav-count').textContent, "3");
  assert.equal(doc.querySelector(".pp-user-name").textContent, "Ada Lovelace");
  assert.deepEqual(log, ["overview"], "the first allowed page renders by default");
  assert.equal(doc.querySelector('.pp-nav-item[data-page="overview"]').getAttribute("aria-current"), "page");
  // Switcher lists this platform and the ones the context names.
  const sw = [...doc.querySelectorAll("#pp-switch-pop .pp-pop-item strong")].map((s) => s.textContent);
  assert.deepEqual(sw, ["Account", "Partner"]);
});

test("routing: hash routes pass params; an unknown or forbidden page falls back to the first allowed one", async (t) => {
  const log = [];
  const { w } = boot(t, { session: jwt(), url: "https://account.pigeonpost.dev/#/handles/abc/def" });
  await w.PP.shell.start({ platform: "account", title: "Account", context: async () => ({ admin: false }), pages: PAGES(log) });
  await tick();
  assert.deepEqual(log, ["handles:abc,def"]);
  w.location.hash = "#/secret";
  await tick();
  assert.equal(log.at(-1), "overview", "forbidden page is not rendered");
  assert.equal(w.location.hash, "#/overview");
  assert.equal(w.PP.href("handles", "a b", ""), "#/handles/a%20b");
});

test("a page that throws shows an explained error with a retry, not a blank page", async (t) => {
  const { w } = boot(t, { session: jwt(), url: "https://account.pigeonpost.dev/#/broken" });
  await w.PP.shell.start({ platform: "account", title: "Account", context: async () => ({}), pages: PAGES([]) });
  await tick();
  const err = w.document.querySelector(".pp-empty-error");
  assert.ok(err);
  assert.match(err.textContent, /Payments are not answering/);
  assert.equal(err.querySelector("button").textContent, "Try again");
});

test("no allowed page: the no-access screen", async (t) => {
  const { w } = boot(t, { session: jwt() });
  await w.PP.shell.start({ platform: "management", title: "Management", noAccess: { title: "Staff only" }, context: async () => ({}), pages: [{ key: "x", title: "X", allow: () => false, render: () => {} }] });
  await tick();
  assert.equal(w.document.querySelector(".pp-empty h3").textContent, "Staff only");
});

test("sign-in return: the code is exchanged through the bridge and the page returns to where it started", async (t) => {
  const { w, calls } = boot(t, { url: "https://account.pigeonpost.dev/?code=c1&state=s1", fetch: (u) => u.endsWith("/api/v1/auth/exchange") ? reply(200, { session: jwt() }) : reply(404, {}) });
  w.localStorage.setItem("pp_pkce", JSON.stringify({ state: "s1", verifier: "v1", nonce: "n", returnTo: "/#/handles" }));
  await w.PP.shell.start({ platform: "account", title: "Account", context: async () => ({}), pages: PAGES([]) });
  await tick();
  const ex = calls.find((c) => c.url.endsWith("/api/v1/auth/exchange"));
  assert.deepEqual(JSON.parse(ex.init.body), { code: "c1", redirectUri: "https://account.pigeonpost.dev/", codeVerifier: "v1" });
  assert.equal(w.location.pathname + w.location.hash, "/#/handles");
  assert.ok(w.PP.signedIn());
  assert.equal(w.localStorage.getItem("pp_pkce"), null);
});

test("ui.table: search, sort, filter and paging; empty states", async (t) => {
  const { w } = boot(t);
  const rows = Array.from({ length: 30 }, (_, i) => ({ id: `r${i}`, name: `Item ${i}`, n: i, kind: i % 2 ? "odd" : "even" }));
  const table = w.PP.ui.table({ columns: [{ key: "name", label: "Name", sort: true }, { key: "n", label: "N", num: true, sort: true }], rows, search: "Search items", filters: [{ key: "kind", label: "Kind", options: [["odd", "Odd"], ["even", "Even"]] }], pageSize: 10 });
  w.document.body.append(table);
  const bodyRows = () => [...table.querySelectorAll("tbody tr")];
  assert.equal(bodyRows().length, 10);
  assert.match(table.querySelector(".pp-pager").textContent, /1–10 of 30/);
  const sel = table.querySelector("select");
  sel.value = "odd"; sel.dispatchEvent(new w.Event("change"));
  assert.match(table.querySelector(".pp-pager").textContent, /of 15/);
  table.querySelectorAll("th button")[1].click();
  table.querySelectorAll("th button")[1].click();
  assert.equal(bodyRows()[0].querySelector("td").textContent, "Item 29", "descending sort");
  const search = table.querySelector("input[type=search]");
  search.value = "item 2"; search.dispatchEvent(new w.Event("input"));
  assert.ok(bodyRows().every((r) => /Item 2/.test(r.textContent)));
  search.value = "zzz"; search.dispatchEvent(new w.Event("input"));
  assert.equal(table.querySelector(".pp-empty h3").textContent, "No matches");
  const none = w.PP.ui.table({ columns: [{ key: "a", label: "A" }], rows: [], empty: { title: "No handles yet", text: "Get one." } });
  assert.equal(none.querySelector(".pp-empty h3").textContent, "No handles yet");
});

test("ui.form validates required fields and patterns before calling onSubmit", async (t) => {
  const { w } = boot(t);
  let got = null;
  const form = w.PP.ui.form([{ name: "iban", label: "IBAN", required: true, pattern: "TR[0-9]{24}" }, { name: "note", label: "Note" }], { onSubmit: (v) => { got = v; return "Saved"; } });
  w.document.body.append(form);
  form.dispatchEvent(new w.Event("submit", { cancelable: true }));
  await tick();
  assert.equal(got, null);
  assert.match(form.textContent, /IBAN is required/);
  form.inputs.iban.value = "TR12";
  form.dispatchEvent(new w.Event("submit", { cancelable: true }));
  await tick();
  assert.equal(got, null);
  form.inputs.iban.value = "TR" + "1".repeat(24);
  form.dispatchEvent(new w.Event("submit", { cancelable: true }));
  await tick();
  assert.deepEqual(JSON.parse(JSON.stringify(got)), { iban: "TR" + "1".repeat(24), note: "" });
  assert.equal(form.querySelector(".pp-form-status").textContent, "Saved");
});

test("ui.gate disables the area under it; ui.stepper marks done and current steps", async (t) => {
  const { w } = boot(t);
  const area = w.PP.el("button", {}, "Buy");
  const g = w.PP.ui.gate({ text: "Add billing details first.", action: { label: "Add billing details", href: "#/billing" } }, area);
  assert.equal(g.querySelector(".pp-gated-area").getAttribute("aria-disabled"), "true");
  assert.ok(g.querySelector(".pp-gated-area").hasAttribute("inert"));
  assert.equal(g.querySelector(".pp-gate a").getAttribute("href"), "#/billing");
  const s = w.PP.ui.stepper([{ key: "a", title: "Type", done: true }, { key: "b", title: "Details" }, { key: "c", title: "Bank" }], "b", () => {});
  const li = s.querySelectorAll("li");
  assert.ok(li[0].classList.contains("is-done"));
  assert.equal(li[1].getAttribute("aria-current"), "step");
  assert.equal(li[2].querySelector("button"), null, "a step two ahead is not reachable");
});

test("server text is never parsed as HTML", async (t) => {
  const { w } = boot(t);
  const nasty = "<img src=x onerror=alert(1)>";
  const t1 = w.PP.ui.table({ columns: [{ key: "a", label: "A" }], rows: [{ a: nasty }] });
  const c = w.PP.ui.card({ title: nasty }, nasty);
  const e = w.PP.ui.empty({ title: nasty, text: nasty });
  for (const n of [t1, c, e]) assert.equal(n.querySelector("img"), null);
});

test("PP.api: stepUp 'throw' keeps the page and carries the challenge; nested error bodies are flattened", async (t) => {
  const challenge = { ok: false, status: 401, headers: { get: (h) => (h === "www-authenticate" ? 'Bearer error="insufficient_user_authentication", acr_values="gold", max_age=300' : null) }, text: async () => JSON.stringify({ error: "step_up_required" }) };
  const { w, nav } = boot(t, { session: jwt(), fetch: (u) => (u.endsWith("/nested") ? reply(409, { error: { code: "conflict", message: "Changed meanwhile", details: { revision: 3 } } }) : challenge) });
  const e = await w.PP.postbox("/v1/staff/x", { stepUp: "throw" }).catch((x) => x);
  assert.equal(e.code, "step_up_required");
  assert.deepEqual({ ...e.challenge }, { acr_values: "gold", max_age: 300 });
  assert.equal(nav.length, 0, "no redirect");
  const n = await w.PP.postbox("/nested").catch((x) => x);
  assert.equal(n.code, "conflict");
  assert.equal(n.message, "Changed meanwhile");
  assert.equal(n.body.revision, 3);
  // Without the option the shared behaviour is unchanged: straight to the realm.
  await w.PP.postbox("/v1/staff/x").catch(() => {});
  await tick();
  assert.equal(new URL(nav[0]).searchParams.get("acr_values"), "gold");
});

test("themePosition left puts the theme switch at the top left; navHeader draws above the navigation", async (t) => {
  const { w } = boot(t, { session: jwt() });
  await w.PP.shell.start({ platform: "enterprise", title: "Enterprise", themePosition: "left", navHeader: (ctx) => w.PP.el("p", { class: "org" }, ctx.org), context: async () => ({ org: "Acme" }), pages: PAGES([]) });
  await tick();
  const doc = w.document;
  const top = doc.querySelector(".pp-top");
  const theme = top.querySelector("[data-theme-toggle]");
  assert.ok(theme.classList.contains("pp-theme-left"));
  assert.equal(theme.parentElement, top, "not inside the right-hand actions");
  assert.ok(theme.compareDocumentPosition(doc.getElementById("pp-crumbs")) & w.Node.DOCUMENT_POSITION_FOLLOWING, "before the breadcrumbs");
  assert.equal(doc.querySelector(".pp-top-actions [data-theme-toggle]"), null);
  assert.equal(doc.querySelector("#pp-nav-header .org").textContent, "Acme");
  await w.PP.shell.reload();
  assert.equal(doc.querySelectorAll("#pp-nav-header").length, 1, "redrawn, not duplicated");
  // Default stays at the right.
  const other = boot(t, { session: jwt() });
  await other.w.PP.shell.start({ platform: "account", title: "Account", context: async () => ({}), pages: PAGES([]) });
  await tick();
  assert.ok(other.w.document.querySelector(".pp-top-actions [data-theme-toggle]"));
});

test("breadcrumbs hold only the group and the page (no stray text)", async (t) => {
  const { w } = boot(t, { session: jwt() });
  await w.PP.shell.start({ platform: "account", title: "Account", context: async () => ({}), pages: PAGES([]) });
  await tick();
  assert.equal(w.document.getElementById("pp-crumbs").textContent, "AccountOverview");
});

test("api: the marketplace's nested error shape is read like the flat one; quiet probes never leave; files go up as the body", async (t) => {
  const seen = [];
  const { w, nav } = boot(t, { session: jwt(), fetch: (u, init) => {
    seen.push({ u, init });
    if (u.endsWith("/nested")) return reply(422, { error: { code: "price_not_invoiceable", message: "Pick another", details: { nearest: [1900, 2000] } } });
    if (u.endsWith("/probe")) return reply(401, { error: "step_up_required", acr_values: "gold" });
    if (u.endsWith("/logo")) return reply(200, { data: { ok: true } });
    return reply(404, {});
  } });
  const e = await w.PP.postbox("/nested").catch((x) => x);
  assert.equal(e.code, "price_not_invoiceable");
  assert.equal(e.message, "Pick another");
  assert.deepEqual([...e.body.nearest], [1900, 2000]);
  const q = await w.PP.postbox("/probe", { quiet: true }).catch((x) => x);
  assert.equal(q.code, "step_up_required");
  assert.ok(!(q instanceof w.PP.Redirecting));
  assert.equal(nav.length, 0, "a quiet call does not leave for the realm");
  const blob = new w.Blob(["png"], { type: "image/png" });
  await w.PP.postbox("/logo", { method: "PUT", blob, contentType: "image/png" });
  const up = seen.at(-1);
  assert.equal(up.init.body, blob);
  assert.equal(up.init.headers["content-type"], "image/png");
});
