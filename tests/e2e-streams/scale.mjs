// Scale check against a real media server (docs/streaming.md, "Capacity"): one streamer and N
// headless-Chrome viewers, all synthetic, through an already running calls service that is
// configured for that media server (CALLS_URL), with the stand-in postbox for membership.
// Viewers are added in steps; each step is held and measured. Media-host throughput and CPU are
// sampled separately on the media host while this runs; the step boundaries are printed with
// timestamps so the two can be lined up.
//   CALLS_PORT=18560 node scale.mjs 10 20 30
import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import puppeteer from "puppeteer-core";
import { createMockPostbox } from "./mock-postbox.mjs";
import { createStatic } from "./serve.mjs";

const REPO = process.env.REPO || path.resolve(path.dirname(new URL(import.meta.url).pathname), "../..");
const HERE = path.dirname(new URL(import.meta.url).pathname);
const STEPS = process.argv.slice(2).map(Number).filter(Boolean);
const HOLD = Number(process.env.HOLD || 40_000);
const P = { calls: Number(process.env.CALLS_PORT || 18560), pb: Number(process.env.PB_PORT || 18891), web: Number(process.env.WEB_PORT || 18561) };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const now = () => new Date().toISOString();
const members = { "/k/host": "owner" };
for (let i = 0; i < Math.max(...STEPS) + 2; i++) members["/k/v" + i] = "member";
const pb = createMockPostbox({ members });
await new Promise((r) => pb.server.listen(P.pb, "127.0.0.1", r));
const web = createStatic(path.join(process.env.SITE || path.join(REPO, "site-inbox")), P.calls);
await new Promise((r) => web.listen(P.web, "127.0.0.1", r));
const inject = readFileSync(path.join(HERE, "inject.js"), "utf8").replace('window.__POSTBOX || "http://127.0.0.1:8891"', `"http://127.0.0.1:${P.pb}"`);
const browser = await puppeteer.launch({
  executablePath: process.env.CHROME || "/usr/bin/google-chrome", headless: true, protocolTimeout: 120_000,
  args: ["--no-sandbox", "--autoplay-policy=no-user-gesture-required", "--disable-background-timer-throttling", "--disable-renderer-backgrounding", "--disable-backgrounding-occluded-windows", "--use-fake-ui-for-media-stream"],
});
async function page(name, viewport) {
  const p = await (await browser.createBrowserContext()).newPage();
  await p.setViewport(viewport || { width: 1280, height: 720 });
  await p.evaluateOnNewDocument(inject);
  await p.goto(`http://127.0.0.1:${P.web}/native-call.html`, { waitUntil: "load" });
  await p.evaluate((n) => window.PigeonpostNative.configure({ token: "tok-" + n, mailbox: { address: "/k/" + n, handle: null } }), name);
  return p;
}
const GROUP = { id: pb.GID, name: "Crew" };
const out = { started: now(), steps: [] };
const host = await page("host");
const live = await host.evaluate((g) => window.PigeonpostNative.goLive(g, { title: "Scale", camera: true }), GROUP);
console.log(now(), "LIVE", JSON.stringify(live));
if (!live.ok) process.exit(1);
const viewers = [];
async function sample() {
  const per = await Promise.all(viewers.map(async (v) => {
    try {
      const s = await v.evaluate(async () => ({ i: window.PigeonpostLive.inspect(), st: await window.PigeonpostLive.stats(), lat: window.__lat.slice(-5) }));
      const cam = (s.st || []).find((x) => x.source === "camera" && x.subscribed) || {};
      return { ok: Boolean(s.i), epoch: s.i && s.i.epoch, dec: s.i && s.i.decryptErrors, frames: cam.frames || 0, bytes: cam.bytes || 0, w: cam.width, h: cam.height, lat: s.lat };
    } catch (e) { return { ok: false, err: String(e).slice(0, 80) }; }
  }));
  return per;
}
for (const n of STEPS) {
  const t0 = now();
  while (viewers.length < n) {
    const v = await page("v" + viewers.length);
    const r = await v.evaluate((g, s) => window.PigeonpostNative.watchStream(g, s), GROUP, live.stream);
    if (!r.ok) console.log(now(), "watch failed", viewers.length, JSON.stringify(r));
    viewers.push(v);
  }
  console.log(now(), "STEP", n, "joined since", t0);
  await sleep(10_000);
  const a = await sample();
  const ta = Date.now();
  console.log(now(), "MEASURE-START", n);
  await sleep(HOLD);
  const b = await sample();
  const tb = Date.now();
  console.log(now(), "MEASURE-END", n);
  const secs = (tb - ta) / 1000;
  const rates = b.map((x, i) => ({ kbps: Math.round(((x.bytes - (a[i].bytes || 0)) * 8) / secs / 1000), fps: +((x.frames - (a[i].frames || 0)) / secs).toFixed(1), w: x.w, h: x.h, dec: x.dec, ok: x.ok }));
  const lats = b.flatMap((x) => x.lat || []).sort((p, q) => p - q);
  const step = {
    viewers: n, at: [new Date(ta).toISOString(), new Date(tb).toISOString()],
    decoding: rates.filter((r) => r.fps > 5).length, medianKbps: rates.map((r) => r.kbps).sort((p, q) => p - q)[Math.floor(rates.length / 2)],
    totalKbps: rates.reduce((s, r) => s + r.kbps, 0), minFps: Math.min(...rates.map((r) => r.fps)), resolutions: [...new Set(rates.map((r) => `${r.w}x${r.h}`))],
    decryptErrors: rates.reduce((s, r) => s + (r.dec || 0), 0), latencyMedianMs: lats[Math.floor(lats.length / 2)], latencyP90Ms: lats[Math.floor(lats.length * 0.9)],
    hostInspect: await host.evaluate(() => window.PigeonpostLive.inspect()),
  };
  out.steps.push(step);
  console.log(now(), "RESULT", JSON.stringify(step));
}
writeFileSync(path.join(process.env.TMPDIR || "/tmp", "pp-streams-scale.json"), JSON.stringify(out, null, 2));
await host.evaluate(() => window.PigeonpostLive.stop()).catch(() => {});
await sleep(1500);
await browser.close();
web.close(); pb.server.close();
process.exit(0);
