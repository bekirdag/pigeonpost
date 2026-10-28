#!/usr/bin/env node
// Checks every rendered store image against the store rules before anyone uploads it:
// exact pixel size per set, PNG 8-bit RGB with no alpha channel, file size, and the expected count.
//
//   node marketing/store/render/validate.mjs [out-dir]
import { readFile, readdir, stat } from "node:fs/promises";
import { existsSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { SETS } from "./render.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const out = path.resolve(process.argv[2] || path.join(os.homedir(), "Documents/releases/pigeonpost/store-visuals-20261028"));
const copy = JSON.parse(await readFile(path.join(here, "..", "copy.json"), "utf8"));
const rules = { ...Object.fromEntries(Object.entries(SETS).map(([k, v]) => [k, { w: v.w, h: v.h, count: copy.scenes.length }])),
  "android-feature": { w: 1024, h: 500, count: 1 } };
// Google Play: 8 MB per screenshot and 1 MB for the feature graphic; App Store: 10 MB (kept under 8 for both).
const maxBytes = (set) => (set === "android-feature" ? 1024 * 1024 : 8 * 1024 * 1024);

let failures = 0, total = 0;
const fail = (msg) => { failures++; console.log("FAIL " + msg); };
for (const [set, rule] of Object.entries(rules)) {
  const dir = path.join(out, set);
  const files = existsSync(dir) ? (await readdir(dir)).filter((f) => f.endsWith(".png")).sort() : [];
  if (files.length !== rule.count) fail(`${set}: ${files.length} images, expected ${rule.count}`);
  for (const f of files) {
    total++;
    const file = path.join(dir, f);
    const b = await readFile(file);
    if (b.readUInt32BE(0) !== 0x89504e47 || b.toString("ascii", 12, 16) !== "IHDR") { fail(`${set}/${f}: not a PNG`); continue; }
    const w = b.readUInt32BE(16), h = b.readUInt32BE(20), depth = b[24], type = b[25];
    const bytes = (await stat(file)).size;
    const problems = [];
    if (w !== rule.w || h !== rule.h) problems.push(`${w}x${h} != ${rule.w}x${rule.h}`);
    if (type !== 2) problems.push(`colour type ${type} (want 2, RGB without alpha)`);
    if (depth !== 8) problems.push(`bit depth ${depth}`);
    if (bytes > maxBytes(set)) problems.push(`${(bytes / 1048576).toFixed(2)} MB too large`);
    if (problems.length) fail(`${set}/${f}: ${problems.join("; ")}`);
    else console.log(`ok   ${set}/${f} ${w}x${h} RGB8 ${(bytes / 1048576).toFixed(2)} MB`);
  }
}
console.log(`${total} images, ${failures} problems`);
process.exit(failures ? 1 : 0);
