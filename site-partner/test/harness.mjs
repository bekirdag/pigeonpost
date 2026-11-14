// Boots partner.pigeonpost.dev the way a browser does: the scripts index.html names, in its
// order, against a fake postbox (a route table) and a stub WebSocket.
import { readFileSync } from "node:fs";
import { webcrypto } from "node:crypto";
import { JSDOM, VirtualConsole } from "jsdom";

const here = new URL("../", import.meta.url);
const shared = new URL("../../site-shared/", import.meta.url);
const read = (u) => readFileSync(u, "utf8");

/// The scripts index.html loads, as files on disk (/shared/… is site-shared/).
export function scripts() {
  const html = read(new URL("index.html", here));
  return [...html.matchAll(/<script src="([^"]+)"><\/script>/g)].map((m) => m[1]).map((src) =>
    src.startsWith("/shared/") ? new URL(src.slice("/shared/".length), shared) : new URL(src.slice(1), here));
}

export const tick = async (n = 60) => { for (let i = 0; i < n; i++) await new Promise((r) => setImmediate(r)); };

export const jwt = (claims = {}) => {
  const b = (o) => Buffer.from(JSON.stringify(o)).toString("base64url");
  return `${b({ alg: "none" })}.${b({ sub: "acct_1", preferred_username: "ada", name: "Ada Lovelace", ...claims })}.sig`;
};

export const reply = (status, body, headers = {}) => ({
  ok: status >= 200 && status < 300, status,
  headers: { get: (k) => headers[String(k).toLowerCase()] ?? null },
  text: async () => (body === undefined ? "" : JSON.stringify(body)),
  json: async () => body,
});

/// A WebSocket stand-in: records what the page sends; tests push server frames.
export function socketClass(list) {
  return class FakeSocket {
    constructor(url) { this.url = String(url); this.sent = []; this.readyState = 0; list.push(this); setTimeout(() => { this.readyState = 1; this.onopen && this.onopen(); }, 0); }
    send(data) { this.sent.push(JSON.parse(data)); }
    close() { this.readyState = 3; this.closed = true; this.onclose && this.onclose(); }
    serve(frame) { this.onmessage && this.onmessage({ data: JSON.stringify(frame) }); }
  };
}

/// Boot the page. `routes(method, path, body)` answers postbox calls (path includes the query);
/// return undefined for a 404.
export async function boot(t, { hash = "", routes = () => undefined, session = jwt(), storage = {} } = {}) {
  const dom = new JSDOM("<!doctype html><html><head></head><body></body></html>", {
    url: `https://partner.pigeonpost.dev/${hash}`, runScripts: "outside-only", pretendToBeVisual: true, virtualConsole: new VirtualConsole(),
  });
  // The shell asks the realm silently once per tab; tests start as a tab that already did.
  try { dom.window.sessionStorage.setItem("pp_silent_checked", "1"); } catch { /* opaque origin */ }
  t.after(() => dom.window.close());
  const w = dom.window;
  Object.defineProperty(w, "crypto", { value: webcrypto, configurable: true });
  const calls = [];
  const sockets = [];
  const nav = [];
  w.WebSocket = socketClass(sockets);
  w.fetch = async (u, init = {}) => {
    const url = new URL(String(u), "https://partner.pigeonpost.dev");
    const method = init.method || "GET";
    let body = init.body;
    if (typeof body === "string") { try { body = JSON.parse(body); } catch { /* raw */ } }
    calls.push({ method, url: url.toString(), path: url.pathname + url.search, origin: url.origin, body, headers: init.headers || {} });
    const r = routes(method, url.pathname + url.search, body, url);
    return r || reply(404, { error: "not_found" });
  };
  if (session) w.sessionStorage.setItem("pp_session", session);
  for (const [k, v] of Object.entries(storage)) w.localStorage.setItem(k, v);
  for (const file of scripts()) {
    w.eval(read(file));
    if (file.pathname.endsWith("/core.js")) w.PP.navigate = (x) => nav.push(x);
  }
  await tick();
  return { w, doc: w.document, calls, sockets, nav, text: () => w.document.body.textContent };
}

export async function go(env, hash) {
  env.w.location.hash = hash;
  await tick();
}

// ---- fixtures ----------------------------------------------------------------------------------

export const PUB = "pub_acme";
export const publisher = (status = "applied", extra = {}) => ({ id: PUB, owner_kind: "account", owner_id: "acct_1", display_name: "Acme Labs", status, decision_reason: null, created_at: "2026-11-01T10:00:00Z", updated_at: "2026-11-10T10:00:00Z", role: "owner", ...extra });

const EMPTY = { kind: "company", country: "TR", legal_name: "Acme Yazilim A.S.", registration_number: "", tax_id: "", duns: "", duns_unavailable_reason: "", address: "", contact_name: "", contact_email: "", bank_country: "", bank_name: "", account_holder: "", iban: "", account_number: "", swift: "", currency: "", public_name: "Acme Labs", public_summary: "", website: "", consent: false, kyc_consent: false };
export const COMPLETE = { ...EMPTY, registration_number: "123456", tax_id: "1234567890", duns_unavailable_reason: "Not registered with D&B yet", address: "Levent, Istanbul", contact_name: "Ada Lovelace", contact_email: "ada@acme.example", bank_country: "TR", bank_name: "Garanti BBVA", account_holder: "Acme Yazilim A.S.", iban: "TR330006100519786457841326", currency: "TRY", consent: true };
export function dossier(over = {}) {
  const { details, ...rest } = over;
  return { id: PUB, state: "draft", revision: 3, kind: "company", country: "TR", submitted_at: null, updated_at: 1763100000, reviewer: null, kyc_status: "not_started", requested_documents: [], documents: [], history: [], ...rest, details: { ...EMPTY, ...(details || {}) } };
}
export const DOCS = [
  { id: "pdoc_1", category: "registration", filename: "faaliyet.pdf", mime: "application/pdf", size: 81234, created_at: 1763000000 },
  { id: "pdoc_2", category: "tax", filename: "vergi-levhasi.pdf", mime: "application/pdf", size: 61234, created_at: 1763000100 },
];

export const LISTINGS = [
  { id: "lst_1", publisher_id: PUB, release_id: "rel_1", template_id: "tpl_1", title: "Contract Reviewer", summary: "Reads contracts and flags risky clauses.", description: "Long text", visibility: "public", status: "published", version: "1.0.0", price: { amount_minor: 1900, currency: "USD", term: "one_off" }, hosting: "pigeonpost", category: "legal", support_url: "https://acme.example/help", privacy_url: null, logo: null, published_at: "2026-11-05T10:00:00Z", updated_at: "2026-11-06T10:00:00Z" },
  { id: "lst_2", publisher_id: PUB, release_id: "rel_2", template_id: "tpl_2", title: "HR Research", summary: "A research report per candidate.", description: "", visibility: "public", status: "draft", version: "1.0.4", price: null, hosting: "publisher", category: "hr", support_url: null, privacy_url: null, logo: null, published_at: null, updated_at: "2026-11-07T10:00:00Z" },
];
export const TEMPLATES = [{ id: "tpl_1", slug: "contract-reviewer", name: "Contract Reviewer", created_at: "2026-11-01T10:00:00Z" }];
export const BUILDS = [{ id: "bld_1", template_id: "tpl_1", version: "1.0.0", status: "built", source: { repository: "https://github.com/acme/cr", commit: "a".repeat(40) }, release_id: "rel_1", review_id: "rev_1", created_at: "2026-11-02T10:00:00Z" }];
export const REVIEWS = [{ review_id: "rev_1", kind: "nest", release_id: "rel_1", status: "approved", revision: 2, stages: ["intake", "deterministic_scan", "behavioral_test", "human_review", "attestation"].map((name) => ({ name, status: "passed", reason: null })), allowed_actions: ["view_findings", "publish_listing"], updated_at: "2026-11-04T10:00:00Z" }];

export function analytics() {
  const days = Array.from({ length: 30 }, (_, i) => new Date(Date.now() - (29 - i) * 86400000).toISOString().slice(0, 10));
  const usage = (j) => ({ jobs: j, succeeded: j - 2, failed: 2, cancelled: 0, in_progress: 0, active_users: Math.ceil(j / 3), new_installations: 4, active_installations: 9 });
  const money = (g) => ({ sales: 10, gross_minor: g, tax_minor: Math.round(g * 0.2), commission_minor: Math.round(g * 0.1), publisher_minor: Math.round(g * 0.7), reversed_publisher_minor: 0, net_publisher_minor: Math.round(g * 0.7) });
  return {
    generated_at: new Date().toISOString(), usage: usage(120),
    app_choices: [{ id: "lst_1", title: "Contract Reviewer" }, { id: "lst_2", title: "HR Research" }],
    apps: [{ id: "lst_1", title: "Contract Reviewer", status: "published", usage: usage(80) }, { id: "lst_2", title: "HR Research", status: "draft", usage: usage(40) }],
    daily: days.map((day, i) => ({ day, jobs: i % 7 })),
    finance: {
      status: "available",
      currencies: [{ currency: "USD", ...money(50000) }],
      apps: [{ listing_id: "lst_1", currency: "USD", ...money(30000) }, { listing_id: "lst_2", currency: "USD", ...money(20000) }],
      daily: days.map((day, i) => ({ day, currency: "USD", net_publisher_minor: i * 100 })),
      publisher_balances: [{ currency: "USD", payable_minor: 35000, reserve_minor: 2000, recoverable_minor: 0, paid_out_minor: 0 }],
    },
  };
}
export const TICKETS = [
  { id: "tkt_1", subject: "When will my application be reviewed?", category: "application", status: "waiting_partner", priority: "normal", unread: true, updated_at: "2026-11-13T09:00:00Z", publisher_name: "Acme Labs" },
  { id: "tkt_2", subject: "Payout currency", category: "billing", status: "resolved", priority: "normal", unread: false, updated_at: "2026-11-10T09:00:00Z", publisher_name: "Acme Labs" },
];
export const ticketDetail = (id) => ({ ...TICKETS.find((t) => t.id === id), messages: [
  { seq: 1, author_kind: "partner", body: "Hello, any news?", created_at: "2026-11-12T09:00:00Z" },
  { seq: 2, author_kind: "staff", body: "Please upload your vergi levhası.", created_at: "2026-11-13T09:00:00Z" },
], last_seq: 2, older_before: null });

/// A postbox for one publisher in a given state. `extra(method, path, body)` overrides.
export function postbox({ pub = publisher(), app = dossier(), staff = null, orgs = [], extra = () => undefined } = {}) {
  return (method, path, body) => {
    const hit = extra(method, path, body);
    if (hit) return hit;
    const p = path.split("?")[0];
    if (p === "/v1/publishers" && method === "GET") return reply(200, { data: pub ? [pub] : [], next_cursor: null });
    if (p === "/v1/staff/me") return staff ? reply(200, { data: { roles: staff } }) : reply(403, { error: "staff_required" });
    if (p === "/v1/orgs") return reply(200, { orgs });
    if (pub && p === `/v1/publisher-applications/${PUB}/dossier` && method === "GET") return reply(200, { data: app });
    if (pub && p === `/v1/publishers/${PUB}/listings` && method === "GET") return reply(200, { data: LISTINGS });
    if (pub && p === `/v1/publishers/${PUB}/templates`) return reply(200, { data: TEMPLATES });
    if (pub && p === `/v1/publishers/${PUB}/builds`) return reply(200, { data: BUILDS });
    if (pub && p === `/v1/publishers/${PUB}/reviews`) return reply(200, { data: REVIEWS });
    if (pub && p === `/v1/publishers/${PUB}/test-nests`) return reply(404, { error: { code: "staging_only", message: "Staging only" } });
    if (pub && p === `/v1/publishers/${PUB}/analytics`) return reply(200, { data: analytics() });
    if (pub && p === "/v1/support/tickets" && method === "GET") return reply(200, { data: TICKETS, next_offset: null });
    const t = /^\/v1\/support\/tickets\/(tkt_\d)$/.exec(p);
    if (t && method === "GET") return reply(200, { data: ticketDetail(t[1]) });
    if (/^\/v1\/support\/tickets\/tkt_\d\/read$/.test(p)) return reply(200, { data: {} });
    return undefined;
  };
}
