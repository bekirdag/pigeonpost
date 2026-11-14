// Boots site-management/index.html in jsdom with its real scripts (site-shared/ served as /shared/)
// and a fake fetch answering from a route table.
import { readFileSync } from "node:fs";
import { webcrypto } from "node:crypto";
import { JSDOM, VirtualConsole } from "jsdom";

export const tick = async (n = 60) => { for (let i = 0; i < n; i++) await new Promise((r) => setImmediate(r)); };
export const jwt = (claims = {}) => {
  const b = (o) => Buffer.from(JSON.stringify(o)).toString("base64url");
  return `${b({ alg: "none" })}.${b({ sub: "s1", preferred_username: "ada", name: "Ada Lovelace", ...claims })}.sig`;
};
export const reply = (status, body, headers = {}) => ({
  ok: status >= 200 && status < 300, status,
  headers: { get: (h) => headers[String(h).toLowerCase()] ?? null },
  text: async () => JSON.stringify(body), json: async () => body,
});
export const stepUp = () => reply(401, { error: "step_up_required" }, { "www-authenticate": 'Bearer error="insufficient_user_authentication", acr_values="gold urn:pigeonpost:acr:mfa", max_age=600' });

const here = (p) => new URL(p, import.meta.url);
export const read = (p) => readFileSync(here(p), "utf8");
export const scripts = () => [...read("../index.html").matchAll(/<script src="([^"]+)"><\/script>/g)].map((m) => m[1]);
const source = (src) => read(src.startsWith("/shared/") ? `../../site-shared/${src.slice(8)}` : `..${src}`);

export const ME = (roles, extra = {}) => reply(200, { data: { account_id: "acct_me", roles, platform_owner: roles.includes("owner"), step_up: { acr_values: "gold", write_max_age: 600, read_max_age: 900 }, finance_configured: true, ...extra } });

/// Fake WebSocket: records instances and sent frames; `serverSend(obj)` delivers a frame.
function fakeSocket(w, list) {
  return class FakeSocket {
    constructor(url) { this.url = url; this.sent = []; list.push(this); setTimeout(() => this.onopen && this.onopen(), 0); }
    send(x) { this.sent.push(JSON.parse(x)); }
    close() { this.closed = true; }
    serverSend(obj) { if (this.onmessage) this.onmessage({ data: JSON.stringify(obj) }); }
  };
}

export function boot(t, { url = "https://management.pigeonpost.dev/", session = jwt(), routes = {}, setup } = {}) {
  const dom = new JSDOM("<!doctype html><html><head></head><body></body></html>", { url, runScripts: "outside-only", pretendToBeVisual: true, virtualConsole: new VirtualConsole() });
  // The shell asks the realm silently once per tab; tests start as a tab that already did.
  try { dom.window.sessionStorage.setItem("pp_silent_checked", "1"); } catch { /* opaque origin */ }
  t.after(() => dom.window.close());
  const w = dom.window;
  Object.defineProperty(w, "crypto", { value: webcrypto, configurable: true });
  const calls = [], nav = [], sockets = [];
  w.WebSocket = fakeSocket(w, sockets);
  w.fetch = async (u, init = {}) => {
    const full = new URL(String(u), url);
    const method = (init.method || "GET").toUpperCase();
    let body = init.body;
    try { body = typeof body === "string" ? JSON.parse(body) : body; } catch { /* raw */ }
    const req = { url: full.href, path: full.pathname, search: full.search, query: Object.fromEntries(full.searchParams), method, body, headers: init.headers || {} };
    calls.push(req);
    const exact = routes[`${method} ${full.pathname}${full.search}`];
    const loose = routes[`${method} ${full.pathname}`];
    const r = exact !== undefined ? exact : loose;
    if (r === undefined) return reply(404, { error: "not_found" });
    return typeof r === "function" ? r(req) : r;
  };
  if (session) w.sessionStorage.setItem("pp_session", session);
  if (setup) setup(w);
  for (const src of scripts()) {
    w.eval(source(src));
    if (src === "/shared/core.js") w.PP.navigate = (u) => nav.push(u);
  }
  return { w, calls, nav, sockets, $: (s) => w.document.querySelector(s), $$: (s) => [...w.document.querySelectorAll(s)] };
}

export const navKeys = (w) => [...w.document.querySelectorAll(".pp-nav-item")].map((a) => a.getAttribute("data-page"));
export async function go(w, hash) { w.location.hash = hash; await tick(); }
export function type(w, input, value) { input.value = value; input.dispatchEvent(new w.Event("input", { bubbles: true })); }
export function submit(w, form) { form.dispatchEvent(new w.Event("submit", { cancelable: true, bubbles: true })); }
