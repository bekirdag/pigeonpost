// Static guarantees: the CSP has nothing inline to forgive, the tokens are the design kit's, and no page
// links a third-party script.
import assert from "node:assert/strict";
import test from "node:test";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";

const root = new URL("../", import.meta.url).pathname;
const files = (dir) => readdirSync(dir).flatMap((f) => {
  const p = join(dir, f);
  if (["node_modules", "test", "fonts"].includes(f)) return [];
  return statSync(p).isDirectory() ? files(p) : [p];
});
const html = files(root).filter((f) => f.endsWith(".html"));

test("no inline scripts, handlers or styles; only same-origin scripts", () => {
  assert.ok(html.length >= 7);
  for (const f of html) {
    const s = readFileSync(f, "utf8");
    assert.doesNotMatch(s, /<script(?![^>]*\bsrc=)[^>]*>/, f);
    assert.doesNotMatch(s, /\son[a-z]+=/i, f);
    assert.doesNotMatch(s, /\sstyle=/i, f);
    for (const m of s.matchAll(/<script src="([^"]+)"/g)) assert.match(m[1], /^\/(shared\/)?[a-z0-9-]+\.js$/, `${f}: ${m[1]}`);
  }
});

test("scripts never parse server text as HTML outside the isolated payment page", () => {
  for (const f of files(root).filter((f) => f.endsWith(".js"))) {
    const s = readFileSync(f, "utf8");
    if (f.endsWith("checkout.js")) continue;
    assert.doesNotMatch(s, /innerHTML|outerHTML|insertAdjacentHTML|document\.write/, f);
  }
});

test("every page reads the design kit tokens from /shared/, and no stylesheet hard-codes a colour", () => {
  for (const f of html) {
    const s = readFileSync(f, "utf8");
    assert.match(s, /<link rel="stylesheet" href="\/shared\/tokens\.css">/, f);
    assert.doesNotMatch(s, /href="\/tokens\.css"/, f);
  }
  const kit = readFileSync(join(root, "../site-shared/tokens.css"), "utf8");
  for (const v of ["--pp-navy-700: #05368B", "--pp-blue-500: #086CFF", "--pp-green-500: #26CE2D"]) assert.ok(kit.includes(v), v);
  for (const name of ["site.css", "app.css"]) {
    const css = readFileSync(join(root, name), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
    assert.doesNotMatch(css, /#[0-9a-fA-F]{3,8}\b|rgba?\(/, `${name} names a colour outside the kit tokens`);
  }
});
