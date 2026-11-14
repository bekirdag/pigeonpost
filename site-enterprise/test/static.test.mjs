// Static guarantees: the CSP has nothing inline to forgive, the tokens are the inbox's, and no page
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

// The inbox now takes its tokens from the Pigeonpost Design Kit (site-inbox/kit/tokens.css), so its
// app.css no longer carries this block. The portal moves onto the kit in its own change; until then
// this copy is frozen rather than compared.
test("design tokens are an exact copy of the inbox's", { skip: "site-inbox moved onto the design kit tokens" }, () => {
  const ours = readFileSync(join(root, "tokens.css"), "utf8").split("\n").slice(2).join("\n");
  const inbox = readFileSync(join(root, "../site-inbox/app.css"), "utf8");
  assert.ok(inbox.includes(ours.trimEnd()), "tokens.css drifted from site-inbox/app.css; re-copy it");
});
