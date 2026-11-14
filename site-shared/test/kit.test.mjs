// The platforms wear the design kit (design.pigeonpost.dev): tokens.css is the kit's tokens and
// base, vendored; platform.css and every page stylesheet name colours only through those tokens.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync, existsSync } from "node:fs";

const root = new URL("../../", import.meta.url);
const read = (p) => readFileSync(new URL(p, root), "utf8");
const strip = (css) => css.replace(/\/\*[\s\S]*?\*\//g, "");

test("tokens.css is the kit's: brand palette, semantic roles, both dark blocks identical", () => {
  const t = read("site-shared/tokens.css");
  for (const v of ["--pp-navy-700: #05368B", "--pp-blue-500: #086CFF", "--pp-green-500: #26CE2D", "--pp-accent: var(--pp-blue-500)", "--pp-radius-l: 14px", "--pp-fs-h1: 28px"]) assert.ok(t.includes(v), v);
  assert.match(t, /src: url\("fonts\/Geist-Variable\.woff2"\)/);
  assert.ok(existsSync(new URL("site-shared/fonts/Geist-Variable.woff2", root)));
  const media = /@media \(prefers-color-scheme: dark\) \{\s*:root:not\(\[data-theme="light"\]\) \{([^}]*)\}/.exec(t)[1];
  const forced = /:root\[data-theme="dark"\], \[data-theme="dark"\] \{([^}]*)\}/.exec(t)[1];
  const norm = (b) => b.split("\n").map((l) => l.trim()).filter(Boolean).join("\n");
  assert.equal(norm(forced), norm(media));
});

test("platform and page stylesheets name no colour outside the kit tokens", () => {
  for (const f of ["site-shared/platform.css", "site-account/account.css", "site-partner/partner.css", "site-management/management.css", "site-help/help.css", "site-status/status.css"]) {
    assert.doesNotMatch(strip(read(f)), /#[0-9a-fA-F]{3,8}\b|rgba?\(/, f);
  }
});

test("the shell shows the kit's brand lockup and platform tiles", () => {
  const shell = read("site-shared/shell.js");
  assert.match(shell, /\/shared\/brand\/logo-symbol\.png/);
  assert.ok(existsSync(new URL("site-shared/brand/logo-symbol.png", root)));
  assert.match(shell, /pp-platform-tile/);
});
