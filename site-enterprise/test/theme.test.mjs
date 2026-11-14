// The appearance switch (theme.js): System by default, Light and Dark remembered in localStorage,
// applied before the page's other scripts, and harmless when storage is blocked.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { load, read, tick } from "./helpers.mjs";

const scheme = (document) => document.querySelector('meta[name="color-scheme"]').getAttribute("content");

test("default is System", async (t) => {
  const { window, document, $ } = await load(t, 'app.html', { url: "https://enterprise.pigeonpost.dev/" });
  
  assert.equal(document.documentElement.getAttribute('data-theme'), null);
  const button = $('[data-theme-toggle]');
  assert.ok(button);
  assert.equal(button.getAttribute('data-choice'), 'system');
  assert.equal(button.getAttribute('aria-label'), 'Theme: System');
});

test("clicking cycles system -> light -> dark -> system", async (t) => {
  const { window, document, $ } = await load(t, 'app.html', { url: "https://enterprise.pigeonpost.dev/" });
  
  // Initial state: System
  assert.equal(document.documentElement.getAttribute('data-theme'), null);
  assert.equal(window.localStorage.getItem('ppi_theme'), null);
  let button = $('[data-theme-toggle]');
  assert.equal(button.getAttribute('data-choice'), 'system');
  assert.equal(button.getAttribute('aria-label'), 'Theme: System');
  
  // Click 1: Light
  button.click();
  await tick();
  
  assert.equal(document.documentElement.getAttribute('data-theme'), 'light');
  assert.equal(window.localStorage.getItem('ppi_theme'), 'light');
  assert.equal(scheme(document), 'light');
  button = $('[data-theme-toggle]');
  assert.equal(button.getAttribute('data-choice'), 'light');
  assert.equal(button.getAttribute('aria-label'), 'Theme: Light');
  
  // Click 2: Dark
  button.click();
  await tick();
  
  assert.equal(document.documentElement.getAttribute('data-theme'), 'dark');
  assert.equal(window.localStorage.getItem('ppi_theme'), 'dark');
  assert.equal(scheme(document), 'dark');
  button = $('[data-theme-toggle]');
  assert.equal(button.getAttribute('data-choice'), 'dark');
  assert.equal(button.getAttribute('aria-label'), 'Theme: Dark');
  
  // Click 3: System
  button.click();
  await tick();
  assert.equal(scheme(document), 'light dark');
  
  assert.equal(document.documentElement.getAttribute('data-theme'), null);
  assert.equal(window.localStorage.getItem('ppi_theme'), null);
  button = $('[data-theme-toggle]');
  assert.equal(button.getAttribute('data-choice'), 'system');
  assert.equal(button.getAttribute('aria-label'), 'Theme: System');
});

test("stored dark is applied before other scripts", async (t) => {
  const { window, document } = await load(t, 'app.html', { 
    url: "https://enterprise.pigeonpost.dev/",
    before: (w) => {
      w.localStorage.setItem('ppi_theme', 'dark');
    }
  });
  
  assert.equal(document.documentElement.getAttribute('data-theme'), 'dark');
});

test("localStorage throwing defaults to system and allows clicking", async (t) => {
  const { window, document, $ } = await load(t, 'app.html', { 
    url: "https://enterprise.pigeonpost.dev/",
    before: (w) => {
      const origGetItem = w.Storage.prototype.getItem;
      const origSetItem = w.Storage.prototype.setItem;
      const origRemoveItem = w.Storage.prototype.removeItem;
      
      w.Storage.prototype.getItem = function(key) {
        if (key === 'ppi_theme') throw new Error("Storage blocked");
        return origGetItem.call(this, key);
      };
      w.Storage.prototype.setItem = function(key, value) {
        if (key === 'ppi_theme') throw new Error("Storage blocked");
        return origSetItem.call(this, key, value);
      };
      w.Storage.prototype.removeItem = function(key) {
        if (key === 'ppi_theme') throw new Error("Storage blocked");
        return origRemoveItem.call(this, key);
      };
    }
  });
  
  // Defaults to system
  assert.equal(document.documentElement.getAttribute('data-theme'), null);
  let button = $('[data-theme-toggle]');
  assert.ok(button);
  assert.equal(button.getAttribute('data-choice'), 'system');
  assert.equal(button.getAttribute('aria-label'), 'Theme: System');
  
  // Click switches to light (in memory)
  button.click();
  await tick();
  
  assert.equal(document.documentElement.getAttribute('data-theme'), 'light');
  button = $('[data-theme-toggle]');
  assert.equal(button.getAttribute('data-choice'), 'light');
  assert.equal(button.getAttribute('aria-label'), 'Theme: Light');
});

test("every page carries the switch, and pigeonpost.dev serves the same theme.js", () => {
  for (const page of ["index.html", "start.html", "invite.html", "checkout.html", "checkout/return.html", "auth/callback.html"]) {
    const html = read(page);
    assert.match(html, /<button type="button" class="theme-toggle" data-theme-toggle aria-label="Theme: System"><\/button>/, page);
    assert.ok(html.indexOf('src="/theme.js"') < html.indexOf('rel="stylesheet"'), `${page}: theme.js runs before the styles`);
  }
  // The admin app draws its switch in the shared shell (top left of the top bar), from the shared copy.
  const app = read("app.html");
  assert.ok(app.indexOf('src="/shared/theme.js"') < app.indexOf('rel="stylesheet"'), "app.html: theme.js runs before the styles");
  assert.equal(read("shared/theme.js"), read("theme.js"), "site-shared/theme.js drifted from site-enterprise/theme.js");
  assert.equal(readFileSync(new URL("../../site/theme.js", import.meta.url), "utf8"), read("theme.js"), "site/theme.js drifted; re-copy it");
});

test("the portal's dark palette is the same for the device setting and the switch", () => {
  const css = read("site.css");
  const media = /@media \(prefers-color-scheme: dark\) \{\n  :root:not\(\[data-theme="light"\]\) \{([^}]*)\}/.exec(css)[1];
  const forced = /:root\[data-theme="dark"\] \{([^}]*)\}/.exec(css)[1];
  const norm = (block) => block.split("\n").map((l) => l.trim()).filter(Boolean).join("\n");
  assert.equal(norm(forced), norm(media));
});
