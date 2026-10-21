// The appearance setting: System (default), Light or Dark, chosen in Settings → Inbox and appearance.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { webcrypto } from "node:crypto";
import { JSDOM, VirtualConsole } from "jsdom";
import { fixtureServer } from "./fixtures.mjs";

const read = name => readFileSync(new URL("../" + name, import.meta.url), "utf8");
const tick = (ms = 30) => new Promise(resolve => setTimeout(resolve, ms));

function page(t, { stored, blockStorage = false, early = true } = {}) {
  const errors = [], console = new VirtualConsole();
  console.on("jsdomError", e => errors.push(e.message));
  const dom = new JSDOM(read("index.html"), { url: "https://inbox.pigeonpost.dev/", runScripts: "outside-only", pretendToBeVisual: true, virtualConsole: console });
  const w = dom.window;
  t.after(() => { w.close(); assert.deepEqual(errors, [], "No uncaught script errors"); });
  if (stored) w.localStorage.setItem("ppi_theme", stored);
  if (blockStorage) {
    // Only the theme key refuses, so this exercises the theme code's own guard rather than the
    // session code, which has never claimed to run without storage.
    for (const name of ["getItem", "setItem", "removeItem"]) {
      const real = w.Storage.prototype[name];
      w.Storage.prototype[name] = function (key, ...rest) {
        if (key === "ppi_theme") throw new w.DOMException("denied", "SecurityError");
        return real.call(this, key, ...rest);
      };
    }
  }
  const root = w.document.documentElement;
  const meta = () => w.document.querySelector('meta[name="color-scheme"]').getAttribute("content");
  // What the browser does in <head>, before the stylesheet: theme.js alone.
  if (early) w.eval(read("theme.js"));
  return { w, root, meta };
}

async function boot(p) {
  const server = fixtureServer();
  Object.defineProperty(p.w, "crypto", { value: webcrypto });
  p.w.matchMedia = query => ({ matches: query.includes("min-width"), addEventListener() {} });
  p.w.fetch = server.fetch;
  try { p.w.localStorage.setItem("ppi_token", "fixture_account"); } catch (_) { /* blocked storage */ }
  p.w.eval(read("names.js"));
  p.w.eval(read("config.js"));
  p.w.eval(read("app.js"));
  await tick(60);
  const $ = id => p.w.document.getElementById(id);
  const radios = () => [...$("theme-choice").querySelectorAll('input[name="theme"]')];
  const choose = value => {
    const input = radios().find(r => r.value === value);
    input.checked = true;
    input.dispatchEvent(new p.w.Event("change", { bubbles: true }));
  };
  const checked = () => radios().find(r => r.checked)?.value;
  return { ...p, $, radios, choose, checked };
}

test("System is the default: no attribute, the stylesheet follows the device", async t => {
  const a = await boot(page(t));
  assert.equal(a.root.hasAttribute("data-theme"), false);
  assert.equal(a.meta(), "light dark");
  assert.deepEqual(a.radios().map(r => r.value), ["system", "light", "dark"]);
  assert.equal(a.checked(), "system");
});

test("a stored choice is applied before first paint by theme.js, ahead of app.js", t => {
  for (const theme of ["light", "dark"]) {
    const p = page(t, { stored: theme });
    assert.equal(p.root.getAttribute("data-theme"), theme);
    assert.equal(p.meta(), theme);
  }
  const index = read("index.html");
  const early = index.indexOf('<script src="theme.js'), sheet = index.indexOf('rel="stylesheet"');
  assert.ok(early > 0 && early < sheet && early < index.indexOf("</head>"), "theme.js loads in <head> before the stylesheet");
});

test("an unknown stored value and blocked storage both fall back to System without errors", async t => {
  const odd = await boot(page(t, { stored: "sepia" }));
  assert.equal(odd.root.hasAttribute("data-theme"), false);
  assert.equal(odd.checked(), "system");

  const blocked = await boot(page(t, { blockStorage: true }));
  assert.equal(blocked.root.hasAttribute("data-theme"), false);
  blocked.choose("dark");
  assert.equal(blocked.root.getAttribute("data-theme"), "dark", "still applies for this visit");
});

test("choosing a theme applies it, persists it, and System forgets it", async t => {
  const a = await boot(page(t));
  a.$("settings-btn").click();
  a.$("settings-nav-inbox").click();
  a.choose("dark");
  assert.equal(a.root.getAttribute("data-theme"), "dark");
  assert.equal(a.w.localStorage.getItem("ppi_theme"), "dark");
  assert.equal(a.meta(), "dark");
  a.choose("light");
  assert.equal(a.root.getAttribute("data-theme"), "light");
  assert.equal(a.w.localStorage.getItem("ppi_theme"), "light");
  assert.equal(a.meta(), "light");
  a.choose("system");
  assert.equal(a.root.hasAttribute("data-theme"), false);
  assert.equal(a.w.localStorage.getItem("ppi_theme"), null);
  assert.equal(a.meta(), "light dark");
});

test("a persisted choice survives a reload and is shown as the checked option", async t => {
  const first = await boot(page(t));
  first.choose("light");
  const stored = first.w.localStorage.getItem("ppi_theme");
  const again = await boot(page(t, { stored }));
  assert.equal(again.root.getAttribute("data-theme"), "light");
  again.$("settings-btn").click();
  again.$("settings-nav-inbox").click();
  assert.equal(again.checked(), "light");
  // app.js applies the same rule even if theme.js never ran.
  const late = await boot(page(t, { stored: "dark", early: false }));
  assert.equal(late.root.getAttribute("data-theme"), "dark");
});

test("the stylesheet lets an explicit choice override the device", () => {
  const css = read("app.css");
  const tokens = block => new Map([...block.matchAll(/(--[a-z0-9-]+):\s*([^;]+);/g)].map(m => [m[1], m[2].trim()]));
  const at = css.indexOf("@media (prefers-color-scheme: dark)");
  const media = css.slice(at, css.indexOf("\n  }", at));
  // System: the device's dark mode applies unless Light was chosen.
  assert.match(media, /^@media \(prefers-color-scheme: dark\) \{\n {2}:root:not\(\[data-theme="light"\]\) \{\n {4}color-scheme: dark;/);
  // Dark chosen: the same palette with no media query around it, token for token.
  const darkAt = css.indexOf(':root[data-theme="dark"] {');
  assert.ok(darkAt > at, "explicit dark block exists");
  const explicit = css.slice(darkAt, css.indexOf("\n}", darkAt));
  assert.match(explicit, /color-scheme: dark;/);
  assert.deepEqual([...tokens(explicit)], [...tokens(media)], "the explicit dark palette matches the device one");
  assert.ok(tokens(media).size > 40);
  // Light chosen: native parts drawn light even on a dark device; colours are the :root defaults.
  assert.match(css, /:root\[data-theme="light"\] \{ color-scheme: light; \}/);
  assert.match(css, /:root \{\n {2}color-scheme: light dark;/);
});

test("the favicons still follow the device, not the setting", () => {
  const index = read("index.html");
  assert.match(index, /favicon-32\.png" media="\(prefers-color-scheme: light\)"/);
  assert.match(index, /favicon-32-dark\.png" media="\(prefers-color-scheme: dark\)"/);
});
