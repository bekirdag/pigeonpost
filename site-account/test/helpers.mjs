// Boot site-account/index.html in jsdom with its real scripts (the shared ones from site-shared/),
// a fake postbox and bridge, and captured navigation.
import { readFileSync } from "node:fs";
import { webcrypto } from "node:crypto";
import { JSDOM, VirtualConsole } from "jsdom";

const here = new URL("../", import.meta.url);
const shared = new URL("../site-shared/", here);
export const read = (p) => readFileSync(p.startsWith("shared/") ? new URL(p.slice(7), shared) : new URL(p, here), "utf8");
export const tick = async (n = 60) => { for (let i = 0; i < n; i++) await new Promise((r) => setImmediate(r)); };

export function reply(status, body) {
  return { ok: status >= 200 && status < 300, status, headers: { get: () => null }, text: async () => JSON.stringify(body), json: async () => body, blob: async () => new Blob([JSON.stringify(body)]) };
}
export function jwt(claims = {}) {
  const b = (o) => Buffer.from(JSON.stringify(o)).toString("base64url");
  return `${b({ alg: "none" })}.${b({ sub: "s1", preferred_username: "ada", name: "Ada Lovelace", email: "ada@example.com", ...claims })}.sig`;
}

export const PROFILE = { id: "bp_1", account_type: "individual", legal_name: "Ada Lovelace", billing_email: "ada@example.com", address: { line1: "1 Analytical St", city: "London", country: "GB", postal_code: "N1" } };

/// A full account: two inboxes, handles from two stores, a plan, Nests, the marketplace.
export function fixtures(over = {}) {
  const base = {
    "GET /v1/capabilities": { features: ["nests", "marketplace"] },
    "GET /v1/publishers": { data: [] },
    "GET /v1/orgs": { orgs: [] },
    "GET /v1/staff/me": [403, { error: "staff_required" }],
    "GET /v1/identities": { identities: [{ address: "k_abc123@pigeonpost.dev", label: "repo:acme/api", handle: "/ada/agent1" }, { address: "k_def456@pigeonpost.dev", label: "scratch", handle: null }] },
    "GET /v1/api-keys": { keys: [{ id: "key_1", prefix: "ppk_12ab", created_at: 1760000000 }] },
    "GET /api/v1/me/overview": {
      handles: [{ namespace: "/ada", source: "apple", expires_at: 4102444800 }, { namespace: "/lovelace", source: "iyzico", expires_at: 4102444800 }],
      billingProfiles: [PROFILE],
      invoices: [{ id: "11111111-1111-4111-8111-111111111111", number: "PP-2026-001", status: "paid", total: 8, currency: "USD", issued_at: "2026-10-01" }],
      cloud: { plan: { web_plan: { capacity: 2, used: 1, paid_through: 4102444800 } }, subscriptions: [{ id: "sub_1", names: 2, currency: "USD", provider_status: "ACTIVE", recurring_price: { amount: "16.00", currency: "USD" } }] },
      openCurrencies: ["USD", "EUR", "TRY"],
    },
    "GET /v1/nests": { nests: [{ nest_id: "n1", label: "Research", status: "ready", address: "nest_n1@pigeonpost.dev" }], allowance: { slots: 3, used: 1, duplicates: [], funding: { source: "iyzico", package: "pp_ai", paid_through: 4102444800 } }, package: { name: "Pigeonpost AI" }, setups: [] },
    "GET /v1/nest-packages": { packages: [{ code: "pp_ai", name: "Pigeonpost AI", sellable: true, slots: 3, storage_bytes: 10737418240, active_hours: 200, model: "managed", input_tokens: 2000000, output_tokens: 500000 }] },
    "GET /api/v1/nests": { prices: { plans: [{ code: "nest-pp_ai-monthly-usd", package: "pp_ai", currency: "USD", amount: "10.00" }], open_currencies: ["USD"] }, subscriptions: [] },
    "GET /api/v1/credits/balance": { available_cents: 2500, held_cents: 500 },
    "GET /api/v1/credits/packs": { packs: [{ pack_id: "usd_10", amount_cents: 1000 }, { pack_id: "usd_25", amount_cents: 2500 }], currencies: ["USD", "TRY"] },
    "POST /api/v1/credits/quotes": { quote_id: "q1", credit_cents: 2500, charge: { amount_minor: 2500, currency: "USD" } },
    "GET /api/v1/credits/history": { entries: [{ kind: "purchase", amount_cents: 2500, created_at: "2026-11-01T10:00:00Z" }, { kind: "capture", amount_cents: -500, created_at: "2026-11-02T10:00:00Z", description: "Wodo AI HR" }], next_cursor: null },
    "GET /v1/marketplace/listings": { data: [
      { id: "lst_hr", title: "Wodo AI HR", summary: "Research a candidate.", category: "hr", hosting: "publisher", featured: true, price_from: { amount_cents: 2500 }, publisher: { display_name: "Wodo", verified: true } },
      { id: "lst_tr", title: "Translator", summary: "Translate documents.", category: "language", price: { amount_minor: 500, currency: "USD", term: "monthly", on_sale: true }, publisher: { display_name: "Lingo", verified: true } },
    ], next_cursor: null },
    "GET /v1/marketplace/listings/lst_tr": { data: { id: "lst_tr", title: "Translator", summary: "Translate documents.", version: "1.0.0", category: "language", hosting: "pigeonpost", price: { amount_minor: 500, currency: "USD", term: "monthly", on_sale: true }, operations: [{ name: "translate", version: "1" }], publisher: { display_name: "Lingo", verified: true } } },
    "GET /v1/marketplace/listings/lst_hr": { data: { id: "lst_hr", title: "Wodo AI HR", summary: "Research a candidate.", version: "1.0.4", hosting: "publisher", operations: [{ name: "research", version: "1", title: "Research a candidate", price: { amount_cents: 2500, on_sale: true }, fields: [{ name: "name", required: true, max_chars: 100 }] }], publisher: { display_name: "Wodo", verified: true } } },
    "GET /v1/nest-installations": { data: [] },
    "GET /v1/marketplace/hosted-jobs": { data: [{ job_id: "job_1", listing_id: "lst_hr", listing_title: "Wodo AI HR", operation: "research", execution: "succeeded", created_at: "2026-11-02T10:00:00Z", charge: { amount_cents: 2500, status: "charged" }, result: { summary: "Report ready", files: [{ index: 0, name: "report.pdf", download_path: "/v1/marketplace/hosted-jobs/job_1/files/0" }] } }] },
    "GET /api/v1/marketplace/licences": { licences: [] },
    "GET /v1/account/deletion-request": { account: { label: "ada@example.com" }, request: null },
  };
  return { ...base, ...over };
}

/// Boot the account page. `routes` maps "METHOD /path" (postbox) or "METHOD /api/path" (bridge),
/// query string stripped, to a body or [status, body] or a function(call) returning either.
export function boot(t, { url = "https://account.pigeonpost.dev/", session = jwt(), routes = fixtures(), storage = {}, local = {} } = {}) {
  const html = read("index.html");
  const scripts = [...html.matchAll(/<script src="\/([^"]+)"><\/script>/g)].map((m) => m[1]);
  const dom = new JSDOM(html.replace(/<script src="[^"]+"><\/script>/g, ""), { url, runScripts: "outside-only", pretendToBeVisual: true, virtualConsole: new VirtualConsole() });
  // The shell asks the realm silently once per tab; tests start as a tab that already did.
  try { dom.window.sessionStorage.setItem("pp_silent_checked", "1"); } catch { /* opaque origin */ }
  t.after(() => dom.window.close());
  const w = dom.window;
  Object.defineProperty(w, "crypto", { value: webcrypto, configurable: true });
  const calls = [];
  const nav = [];
  w.fetch = async (u, init = {}) => {
    const full = new URL(String(u), url);
    const isBridge = full.origin === new URL(url).origin;
    const key = `${init.method || "GET"} ${full.pathname}`;
    const call = { url: full.toString(), key, method: init.method || "GET", body: init.body ? JSON.parse(init.body) : undefined, headers: init.headers || {}, bridge: isBridge };
    calls.push(call);
    let r = routes[key];
    if (typeof r === "function") r = r(call);
    if (r === undefined) return reply(404, { error: "not_found" });
    return Array.isArray(r) ? reply(r[0], r[1]) : reply(200, r);
  };
  w.open = (u) => nav.push(`open:${u}`);
  w.scrollTo = () => {};
  if (session) w.sessionStorage.setItem("pp_session", session);
  for (const [k, v] of Object.entries(storage)) w.sessionStorage.setItem(k, typeof v === "string" ? v : JSON.stringify(v));
  for (const [k, v] of Object.entries(local)) w.localStorage.setItem(k, typeof v === "string" ? v : JSON.stringify(v));
  w.ACC_NO_AUTOSTART = true;
  for (const s of scripts) {
    w.eval(read(s));
    if (s === "shared/core.js") w.PP.navigate = (to) => nav.push(to);
  }
  w.ACC.pollMs = 0;
  w.ACC.debounceMs = 0;
  const started = w.ACC.start();
  const $ = (s) => w.document.querySelector(s);
  const $$ = (s) => [...w.document.querySelectorAll(s)];
  const text = () => (w.document.getElementById("pp-main") || w.document.body).textContent;
  const go = async (hash) => { w.location.hash = hash; await tick(); };
  return { w, document: w.document, calls, nav, started, $, $$, text, go };
}
