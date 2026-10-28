#!/usr/bin/env node
// Composes the store screenshots: headline copy over a brand background, with a raw app capture in a
// drawn device frame. HTML/CSS rendered by headless Chrome, then flattened to opaque sRGB PNG.
//
//   node marketing/store/render/render.mjs [out-dir] [--only=<set>[,<set>]] [--scene=<id>]
//
// Inputs: ../copy.json (headlines and order), ../captures/<class>/<capture>.png (raw app screens).
// Output: <out-dir>/<set>/<nn>-<scene>.png, <out-dir>/index.html (contact sheet).
// Needs Google Chrome and ImageMagick (`magick`). Only free/open assets: Geist (OFL), the Pigeonpost logo.
import { execFile } from "node:child_process";
import { mkdir, readFile, writeFile, rm, mkdtemp } from "node:fs/promises";
import { existsSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { promisify } from "node:util";

const run = promisify(execFile);
const here = path.dirname(fileURLToPath(import.meta.url));
const store = path.resolve(here, "..");
const asset = (name) => pathToFileURL(path.join(here, "assets", name)).href;
const CAPTURES = path.resolve(process.env.PP_CAPTURES || path.join(store, "captures"));
const CHROME = process.env.CHROME || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";

// Each output set: canvas size, the capture class it frames, and how the device is drawn.
export const SETS = {
  // fit: the whole device shows. bleed: the device is sized by width and runs off the bottom edge,
  // which keeps the screen legible when the canvas is shorter than the device (16:9 phone, landscape).
  "ios-6.9":        { w: 1320, h: 2868, source: "ios-phone",      device: "iphone",         layout: "fit" },
  "ios-6.5":        { w: 1284, h: 2778, source: "ios-phone",      device: "iphone",         layout: "fit" },
  "ipad-13":        { w: 2752, h: 2064, source: "ios-tablet",     device: "ipad",           layout: "bleed", width: 0.84 },
  "android-phone":  { w: 1080, h: 1920, source: "android-phone",  device: "android",        layout: "bleed", width: 0.76 },
  "android-7in":    { w: 1920, h: 1200, source: "android-tablet", device: "android-tablet", layout: "bleed", width: 0.80 },
  "android-10in":   { w: 2560, h: 1600, source: "android-tablet", device: "android-tablet", layout: "bleed", width: 0.80 },
};

const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
const lines = (s) => esc(s).split("\n").join("<br>");

async function size(file) {
  const { stdout } = await run("magick", ["identify", "-format", "%w %h", file]);
  const [w, h] = stdout.trim().split(" ").map(Number);
  return { w, h };
}

// Frame geometry, all as fractions of the screen width so every size scales the same way.
const FRAMES = {
  iphone:           { bezel: 0.034, outer: 0.150, inner: 0.125, island: true },
  android:          { bezel: 0.032, outer: 0.105, inner: 0.085, punch: true },
  ipad:             { bezel: 0.026, outer: 0.040, inner: 0.026 },
  "android-tablet": { bezel: 0.026, outer: 0.036, inner: 0.022 },
};

function css(set, theme) {
  const portrait = set.h > set.w;
  // k: one typographic unit. Portrait follows width; landscape follows height.
  const k = portrait ? set.w / 100 : set.h / 100;
  const dark = theme === "dark";
  return `
  @font-face { font-family: Geist; src: url("${asset("Geist-Variable.woff2")}") format("woff2"); font-weight: 100 900; }
  * { box-sizing: border-box; margin: 0; padding: 0; }
  html, body { width: ${set.w}px; height: ${set.h}px; overflow: hidden; }
  body {
    font-family: Geist, sans-serif; -webkit-font-smoothing: antialiased;
    background: ${dark
      ? "radial-gradient(120% 70% at 85% 0%, #2a57b8 0%, rgba(42,87,184,0) 60%), radial-gradient(90% 60% at 0% 100%, rgba(34,197,94,.22) 0%, rgba(34,197,94,0) 60%), linear-gradient(170deg, #16326b 0%, #0c1a3d 100%)"
      : "radial-gradient(110% 60% at 90% 0%, #d6e3ff 0%, rgba(214,227,255,0) 62%), radial-gradient(90% 55% at 0% 100%, #dcf5e4 0%, rgba(220,245,228,0) 60%), linear-gradient(175deg, #f4f7fd 0%, #eef2f8 100%)"};
    color: ${dark ? "#ffffff" : "#16326b"};
    display: flex; flex-direction: column; align-items: center;
    padding: ${portrait ? 6.2 * k : 5.2 * k}px ${portrait ? 6 * k : 6 * k}px 0;
  }
  header { text-align: center; flex: none; }
  .logo { height: ${portrait ? 4.6 * k : 4.4 * k}px; display: block; margin: 0 auto ${2.4 * k}px; ${dark ? "" : "mix-blend-mode: multiply;"} }
  .eyebrow { display: inline-flex; align-items: center; gap: ${0.9 * k}px; font-size: ${portrait ? 2.7 * k : 2.5 * k}px; font-weight: 600;
    letter-spacing: .08em; text-transform: uppercase; color: ${dark ? "#4ade80" : "#15803d"}; margin-bottom: ${1.6 * k}px; }
  .eyebrow svg { width: 1em; height: 1em; }
  h1 { font-size: ${portrait ? 7.5 * k : 6.2 * k}px; line-height: 1.08; font-weight: 700; letter-spacing: -0.025em; text-wrap: balance; white-space: ${portrait ? "normal" : "nowrap"}; }
  p.sub { font-size: ${portrait ? 3.5 * k : 3.0 * k}px; line-height: 1.3; font-weight: 450; margin-top: ${1.8 * k}px;
    color: ${dark ? "#c6d3ee" : "#444c58"}; text-wrap: balance; }
  main { flex: 1; width: 100%; position: relative; }
  .device { position: absolute; left: 50%; transform: translateX(-50%);
    background: linear-gradient(145deg, #3b4049, #15181d 40%, #2a2e35);
    box-shadow: 0 ${2 * k}px ${6 * k}px rgba(8, 18, 45, ${dark ? 0.55 : 0.28}), inset 0 0 0 ${0.25 * k}px #4a505a; }
  .screen { position: absolute; overflow: hidden; background: #000; }
  .screen img { width: 100%; height: 100%; display: block; }
  .island { position: absolute; left: 50%; transform: translateX(-50%); background: #000; border-radius: 999px; }
  .punch { position: absolute; left: 50%; transform: translateX(-50%); background: #05070a; border-radius: 50%;
    box-shadow: inset 0 0 0 2px #1c2128; }
  `;
}

const LOCK = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"><rect x="4" y="11" width="16" height="10" rx="2.5"/><path d="M8 11V7.5a4 4 0 0 1 8 0V11"/></svg>`;

function page(set, scene, capture, cap, index) {
  const portrait = set.h > set.w;
  const frame = FRAMES[set.device];
  const logo = index === 0 ? `<img class="logo" src="${asset(scene.theme === "dark" ? "logo-white.png" : "logo.png")}" alt="">` : "";
  return `<!doctype html><html><head><meta charset="utf-8"><style>${css(set, scene.theme)}</style></head><body>
<header>${logo}<div class="eyebrow">${LOCK}${esc(scene.eyebrow)}</div><h1 id="h">${portrait ? lines(scene.headline) : esc(scene.headline.replace(/\n/g, " "))}</h1>${scene.sub ? `<p class="sub">${esc(scene.sub)}</p>` : ""}</header>
<main id="m"><div class="device" id="d"><div class="screen" id="s"><img src="${pathToFileURL(capture).href}" alt=""></div>
${frame.island ? '<div class="island" id="i"></div>' : ""}${frame.punch ? '<div class="punch" id="p"></div>' : ""}</div></main>
<script>
  // Fit the framed screen into the space the header leaves, keeping the capture's exact aspect.
  const f = ${JSON.stringify(frame)}, cw = ${cap.w}, ch = ${cap.h}, portrait = ${portrait};
  // A landscape headline is one line; shrink it until it fits the canvas.
  const h = document.getElementById("h");
  for (let fs = parseFloat(getComputedStyle(h).fontSize); h.scrollWidth > ${set.w * 0.92} && fs > 20; fs -= 2) h.style.fontSize = fs + "px";
  const m = document.getElementById("m").getBoundingClientRect();
  const gapTop = ${portrait ? set.w * 0.05 : set.h * 0.045}, gapBottom = ${portrait ? set.w * 0.06 : set.h * 0.05};
  const availH = m.height - gapTop - gapBottom, availW = m.width * (portrait ? 0.9 : 0.94);
  // device = screen + 2 * bezel, bezel = f.bezel * screenWidth
  let sw = ${set.layout === "bleed"} ? ${set.w * (set.width || 0.8)} / (1 + 2 * f.bezel)
    : Math.min(availW / (1 + 2 * f.bezel), availH / (ch / cw + 2 * f.bezel));
  const sh = sw * ch / cw, b = sw * f.bezel;
  const d = document.getElementById("d"), s = document.getElementById("s");
  Object.assign(d.style, { width: (sw + 2 * b) + "px", height: (sh + 2 * b) + "px", top: gapTop + "px", borderRadius: (sw * f.outer) + "px" });
  Object.assign(s.style, { left: b + "px", top: b + "px", width: sw + "px", height: sh + "px", borderRadius: (sw * f.inner) + "px" });
  const i = document.getElementById("i");
  if (i) Object.assign(i.style, { width: (sw * 0.286) + "px", height: (sw * 0.084) + "px", top: (b + sw * 0.025) + "px" });
  const p = document.getElementById("p");
  if (p) Object.assign(p.style, { width: (sw * 0.034) + "px", height: (sw * 0.034) + "px", top: (b + sw * 0.022) + "px" });
</script></body></html>`;
}

function featurePage(copy, captures, ratio, android) {
  const w = 1024, h = 500;
  const phone = (img, x, y, sw, z) => {
    const f = FRAMES[android ? "android" : "iphone"], b = sw * f.bezel;
    return `<div class="device" style="left:${x}px;top:${y}px;width:${sw + 2 * b}px;height:${(sw * ratio) + 2 * b}px;border-radius:${sw * f.outer}px;z-index:${z}">
      <div class="screen" style="left:${b}px;top:${b}px;width:${sw}px;height:${sw * ratio}px;border-radius:${sw * f.inner}px"><img src="${pathToFileURL(img).href}"></div>
      ${android ? `<div class="punch" style="width:${sw * 0.034}px;height:${sw * 0.034}px;top:${b + sw * 0.022}px"></div>`
        : `<div class="island" style="width:${sw * 0.286}px;height:${sw * 0.084}px;top:${b + sw * 0.025}px"></div>`}</div>`;
  };
  return `<!doctype html><html><head><meta charset="utf-8"><style>
  @font-face { font-family: Geist; src: url("${asset("Geist-Variable.woff2")}") format("woff2"); font-weight: 100 900; }
  * { box-sizing: border-box; margin: 0; padding: 0; }
  html, body { width: ${w}px; height: ${h}px; overflow: hidden; }
  body { font-family: Geist, sans-serif; -webkit-font-smoothing: antialiased; color: #fff; position: relative;
    background: radial-gradient(70% 90% at 95% 10%, #2a57b8 0%, rgba(42,87,184,0) 65%), radial-gradient(60% 80% at 0% 100%, rgba(34,197,94,.25) 0%, rgba(34,197,94,0) 60%), linear-gradient(160deg, #16326b 0%, #0c1a3d 100%); }
  .copy { position: absolute; left: 56px; top: 0; bottom: 0; width: 520px; display: flex; flex-direction: column; justify-content: center; }
  .logo { width: 250px; margin-bottom: 30px; }
  h1 { font-size: 44px; line-height: 1.08; font-weight: 700; letter-spacing: -0.02em; }
  p { font-size: 20px; line-height: 1.35; color: #c6d3ee; margin-top: 16px; }
  .device { position: absolute; background: linear-gradient(145deg, #3b4049, #15181d 40%, #2a2e35); box-shadow: 0 16px 40px rgba(5, 12, 30, .55), inset 0 0 0 2px #4a505a; }
  .screen { position: absolute; overflow: hidden; } .screen img { width: 100%; height: 100%; display: block; }
  .island { position: absolute; left: 50%; transform: translateX(-50%); background: #000; border-radius: 999px; }
  .punch { position: absolute; left: 50%; transform: translateX(-50%); background: #05070a; border-radius: 50%; }
  </style></head><body>
  <div class="copy"><img class="logo" src="${asset("logo-white.png")}" alt=""><h1>${lines(copy.headline)}</h1><p>${esc(copy.sub)}</p></div>
  ${phone(captures[0], 620, 70, 200, 1)}${phone(captures[1], 790, 30, 200, 2)}
  </body></html>`;
}

async function shoot(html, w, h, out, tmp) {
  const file = path.join(tmp, path.basename(out, ".png") + "-" + Math.random().toString(36).slice(2) + ".html");
  await writeFile(file, html);
  const raw = file.replace(/\.html$/, ".raw.png");
  const profile = await mkdtemp(path.join(tmp, "chrome-"));
  await run(CHROME, ["--headless=new", "--disable-gpu", "--hide-scrollbars", "--no-first-run", "--no-default-browser-check",
    `--user-data-dir=${profile}`, "--force-device-scale-factor=1", "--allow-file-access-from-files", "--force-color-profile=srgb",
    `--window-size=${w},${h}`, "--virtual-time-budget=4000", `--screenshot=${raw}`, pathToFileURL(file).href], { timeout: 60000 })
    // Headless Chrome on macOS often exits non-zero in teardown after writing the shot; the file is what counts.
    .catch((e) => { if (!existsSync(raw)) throw e; });
  // Opaque, 8-bit RGB, sRGB: App Store refuses an alpha channel.
  await run("magick", [raw, "-background", "#000000", "-alpha", "remove", "-alpha", "off", "-colorspace", "sRGB",
    "-strip", "-define", "png:color-type=2", "-define", "png:bit-depth=8", out]);
  const got = await size(out);
  if (got.w !== w || got.h !== h) throw new Error(`${out}: ${got.w}x${got.h}, expected ${w}x${h}`);
  await rm(profile, { recursive: true, force: true });
}

async function pool(tasks, n) {
  const results = []; let i = 0;
  await Promise.all(Array.from({ length: n }, async () => { while (i < tasks.length) { const t = tasks[i++]; results.push(await t()); } }));
  return results;
}

// The contact sheet: every image in the output folder, grouped by set, with its headline.
async function sheet(out, copy) {
  const byId = Object.fromEntries(copy.scenes.map((s) => [s.id, s]));
  byId["feature-graphic"] = { id: "feature-graphic", headline: copy.feature_graphic.headline, sub: copy.feature_graphic.sub };
  const order = [...Object.keys(SETS), "android-feature"];
  const labels = { "ios-6.9": "App Store · iPhone 6.9\"", "ios-6.5": "App Store · iPhone 6.5\"", "ipad-13": "App Store · iPad 13\"",
    "android-phone": "Google Play · phone", "android-7in": "Google Play · 7\" tablet", "android-10in": "Google Play · 10\" tablet", "android-feature": "Google Play · feature graphic" };
  const { readdir } = await import("node:fs/promises");
  let body = "";
  for (const set of order) {
    const dir = path.join(out, set);
    if (!existsSync(dir)) continue;
    const files = (await readdir(dir)).filter((f) => f.endsWith(".png")).sort();
    if (!files.length) continue;
    const cards = [];
    for (const f of files) {
      const { w, h } = await size(path.join(dir, f));
      const scene = byId[f.replace(/\.png$/, "")] || { headline: f };
      cards.push(`<figure><a href="${set}/${f}"><img src="${set}/${f}" loading="lazy" alt="${esc(scene.headline.replace(/\n/g, " "))}"></a>
        <figcaption><b>${esc(scene.headline.replace(/\n/g, " "))}</b>${scene.sub ? `<span>${esc(scene.sub)}</span>` : ""}<code>${set}/${f} · ${w}×${h}</code></figcaption></figure>`);
    }
    body += `<section><h2>${esc(labels[set] || set)} <small>${files.length} images</small></h2><div class="grid ${SETS[set] && SETS[set].h > SETS[set].w ? "tall" : "wide"}">${cards.join("")}</div></section>`;
  }
  return `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Pigeonpost store visuals</title><style>
:root { --bg: #f4f6f9; --card: #fff; --ink: #14181f; --muted: #6b7480; --rule: #e1e5eb; }
@media (prefers-color-scheme: dark) { :root { --bg: #0f1216; --card: #171b21; --ink: #f4f6f9; --muted: #949ba6; --rule: #2c313a; } }
* { box-sizing: border-box; } body { margin: 0; padding: 32px 16px 64px; font: 15px/1.45 -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; background: var(--bg); color: var(--ink); }
main { max-width: 1400px; margin: 0 auto; } h1 { font-size: 26px; margin: 0 0 4px; } .lede { color: var(--muted); margin: 0 0 32px; }
h2 { font-size: 18px; margin: 40px 0 14px; padding-bottom: 8px; border-bottom: 1px solid var(--rule); } h2 small { color: var(--muted); font-weight: 400; font-size: 14px; }
.grid { display: grid; gap: 18px; } .grid.tall { grid-template-columns: repeat(auto-fill, minmax(190px, 1fr)); } .grid.wide { grid-template-columns: repeat(auto-fill, minmax(340px, 1fr)); }
figure { margin: 0; background: var(--card); border: 1px solid var(--rule); border-radius: 12px; overflow: hidden; }
figure img { display: block; width: 100%; height: auto; } figcaption { padding: 10px 12px 12px; display: grid; gap: 3px; }
figcaption span { color: var(--muted); font-size: 13px; } figcaption code { color: var(--muted); font-size: 11.5px; }
</style></head><body><main><h1>Pigeonpost store visuals</h1>
<p class="lede">For review before upload. Rendered by <code>marketing/store/render/render.mjs</code> from <code>marketing/store/copy.json</code> and the captures in <code>marketing/store/captures/</code>. All people, agents, messages and media are invented.</p>
${body}</main></body></html>`;
}

async function main() {
  const args = process.argv.slice(2);
  const out = path.resolve(args.find((a) => !a.startsWith("--")) || path.join(os.homedir(), "Documents/releases/pigeonpost/store-visuals-20261028"));
  const only = args.find((a) => a.startsWith("--only="))?.slice(7).split(",");
  const sceneOnly = args.find((a) => a.startsWith("--scene="))?.slice(8);
  const copy = JSON.parse(await readFile(path.join(store, "copy.json"), "utf8"));
  const tmp = await mkdtemp(path.join(os.tmpdir(), "pp-store-"));
  const tasks = [], made = [], missing = [];
  for (const [name, set] of Object.entries(SETS)) {
    if (only && !only.includes(name)) continue;
    await mkdir(path.join(out, name), { recursive: true });
    for (const [index, scene] of copy.scenes.entries()) {
      if (sceneOnly && scene.id !== sceneOnly) continue;
      const capture = path.join(CAPTURES, set.source, scene.capture + ".png");
      if (!existsSync(capture)) { missing.push(`${set.source}/${scene.capture}.png`); continue; }
      const file = path.join(out, name, scene.id + ".png");
      tasks.push(async () => { const cap = await size(capture); await shoot(page(set, scene, capture, cap, index), set.w, set.h, file, tmp);
        made.push({ set: name, file: path.relative(out, file), scene }); console.log("made", path.relative(out, file)); });
    }
  }
  if (!only || only.includes("android-feature")) {
    const caps = ["inbox", "agent-thread"].map((c) => path.join(CAPTURES, "ios-phone", c + ".png"));
    // The feature graphic is for Google Play, so it shows the Android phone screens when they exist.
    const android = ["inbox", "agent-thread"].map((c) => path.join(CAPTURES, "android-phone", c + ".png"));
    const use = android.every(existsSync) ? android : caps;
    if (use.every(existsSync)) {
      await mkdir(path.join(out, "android-feature"), { recursive: true });
      const file = path.join(out, "android-feature", "feature-graphic.png");
      const dims = await size(use[0]);
      const html = featurePage(copy.feature_graphic, use, dims.h / dims.w, use === android);
      tasks.push(async () => { await shoot(html, 1024, 500, file, tmp); made.push({ set: "android-feature", file: path.relative(out, file),
        scene: { id: "feature-graphic", headline: copy.feature_graphic.headline, sub: copy.feature_graphic.sub } }); console.log("made", path.relative(out, file)); });
    } else missing.push("feature graphic: inbox/agent-thread phone captures");
  }
  await pool(tasks, 4);
  await writeFile(path.join(out, "index.html"), await sheet(out, copy));
  await rm(tmp, { recursive: true, force: true });
  if (missing.length) console.warn("missing captures:\n  " + [...new Set(missing)].join("\n  "));
  console.log(`${made.length} images in ${out}`);
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) main().catch((e) => { console.error(e); process.exit(1); });
