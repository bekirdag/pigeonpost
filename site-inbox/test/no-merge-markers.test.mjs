// Shipped inbox files must never carry leftover merge-conflict markers. A stray "=======" in app.css
// once silently voided the 7–9 person call grid rule.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const skip = new Set(["node_modules", "test", "vendor", "pdfjs", "fonts"]);

function* shipped(dir) {
  for (const name of readdirSync(dir)) {
    if (skip.has(name)) continue;
    const path = join(dir, name);
    if (statSync(path).isDirectory()) yield* shipped(path);
    else if (/\.(css|js|html)$/.test(name)) yield path;
  }
}

test("no merge-conflict markers in shipped inbox files", () => {
  const found = [];
  for (const path of shipped(root)) {
    readFileSync(path, "utf8").split("\n").forEach((line, i) => {
      if (/^(<{7} |={7}$|>{7} )/.test(line)) found.push(`${path.slice(root.length + 1)}:${i + 1}`);
    });
  }
  assert.deepEqual(found, []);
});
