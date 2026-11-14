// Load a portal page into jsdom with its real scripts, a fake fetch and a captured navigation.
import { readFileSync } from "node:fs";
import { webcrypto } from "node:crypto";
import { JSDOM, VirtualConsole } from "jsdom";

const root = new URL("../", import.meta.url);
/// A site file; `shared/…` is site-shared/, which the deploy copies into /shared/.
export const read = (path) => readFileSync(new URL(path.startsWith("shared/") ? `../site-shared/${path.slice(7)}` : path, root), "utf8");
export const tick = async (n = 30) => { for (let i = 0; i < n; i++) await new Promise((r) => setImmediate(r)); };

/// A JSON response the scripts can read like fetch's.
export function reply(status, body, headers = {}) {
  const h = new Map(Object.entries(headers).map(([k, v]) => [k.toLowerCase(), v]));
  return { ok: status >= 200 && status < 300, status, headers: { get: (k) => h.get(k.toLowerCase()) ?? null },
    text: async () => JSON.stringify(body), json: async () => body, blob: async () => new Blob([JSON.stringify(body)]) };
}

/// A fake member JWT carrying display claims.
export function jwt(claims = {}) {
  const b = (o) => Buffer.from(JSON.stringify(o)).toString("base64url");
  return `${b({ alg: "none" })}.${b({ sub: "s1", preferred_username: "ada", ...claims })}.sig`;
}

/// `page` is an HTML file; its <script src> tags are run in order after the fixtures are in place.
export function load(t, page, { url = "https://enterprise.pigeonpost.dev/", fetch, session, storage = {}, before } = {}) {
  const html = read(page);
  const scripts = [...html.matchAll(/<script src="\/([^"]+)"><\/script>/g)].map((m) => m[1]);
  const dom = new JSDOM(html.replace(/<script src="[^"]+"><\/script>/g, ""), { url, runScripts: "outside-only", pretendToBeVisual: true, virtualConsole: new VirtualConsole() });
  t.after(() => dom.window.close());
  const { window } = dom;
  Object.defineProperty(window, "crypto", { value: webcrypto, configurable: true });
  if (!window.TextEncoder) window.TextEncoder = TextEncoder;
  const calls = [];
  const nav = [];
  let resolveNavigation;
  const navigation = new Promise((resolve) => { resolveNavigation = resolve; });
  window.fetch = async (u, init = {}) => {
    const call = { url: String(u), method: init.method || "GET", body: init.body ? JSON.parse(init.body) : undefined, headers: init.headers || {} };
    calls.push(call);
    return fetch ? fetch(call) : reply(404, { error: "not_found" });
  };
  window.confirm = () => true;
  if (session) window.sessionStorage.setItem("pp_session", session);
  // pp_pkce and pp_resume survive the realm round trip in localStorage (shared core); the rest is per tab.
  for (const [k, v] of Object.entries(storage)) (/^pp_(pkce|resume)$/.test(k) ? window.localStorage : window.sessionStorage).setItem(k, typeof v === "string" ? v : JSON.stringify(v));
  window.eval(read("config.js"));
  if (before) before(window);
  for (const s of scripts) {
    if (s === "config.js") continue;
    window.eval(read(s));
    if (s === "shared/core.js") window.PP.navigate = (to) => { nav.push(to); resolveNavigation(to); };
  }
  return { window, document: window.document, calls, nav, navigation, $: (s) => window.document.querySelector(s) };
}

/// Strip jsdom realm prototypes so deepStrictEqual compares values only.
export const plain = (x) => JSON.parse(JSON.stringify(x));
