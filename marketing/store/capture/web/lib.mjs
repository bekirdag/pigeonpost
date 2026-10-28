// Shared plumbing for the web-surface store captures: the local stack (a demo postbox, the
// repository's own call service, optionally a local LiveKit, and the inbox's static files), headless
// Chrome through puppeteer-core, the four device classes, and the status-bar composite.
//
// Ports (private to this harness): web 8960, calls 8961, postbox 8962, LiveKit 8970/8971 and
// 51100-51400/udp, all on 127.0.0.1.
import { spawn, execFileSync } from "node:child_process";
import { writeFileSync, readFileSync, mkdirSync, mkdtempSync, rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import puppeteer from "puppeteer-core";
import { createStatic } from "../../../../tests/e2e-streams/serve.mjs";

export const HERE = path.dirname(fileURLToPath(import.meta.url));
export const REPO = path.resolve(HERE, "../../../..");
export const CAPTURES = path.resolve(HERE, "../../captures");
// live and meeting-notes come from the native apps in the store set; these web versions are kept
// only for comparison, outside the repository unless PP_WEB_EXTRA_OUT says otherwise.
export const EXTRA_OUT = process.env.PP_WEB_EXTRA_OUT || path.join(os.tmpdir(), "pp-store-web-extra");
export const P = { web: 8960, calls: 8961, pb: 8962, lk: 8970, lkTcp: 8971 };
export const SECRET = "demo-only-livekit-secret-for-local-screenshots-0001";
export const SERVICE_TOKEN = "svc_store_screenshots_local";
export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
export const AVATAR_MEDIA = readFileSync(path.join(HERE, "avatar-media.js"), "utf8");

// Each device class: the CSS viewport, the pixel ratio, the output pixels and the status bar height.
export const DEVICES = {
  "ios-phone": { w: 440, h: 956, dpr: 3, out: [1320, 2868], bar: 62, os: "ios", mobile: true,
    ua: "Mozilla/5.0 (iPhone; CPU iPhone OS 18_3 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.3 Mobile/15E148 Safari/604.1" },
  "ios-tablet": { w: 1376, h: 1032, dpr: 2, out: [2752, 2064], bar: 24, os: "ios", mobile: true,
    ua: "Mozilla/5.0 (iPad; CPU OS 18_3 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.3 Mobile/15E148 Safari/604.1" },
  "android-phone": { w: 412, h: 914.3, dpr: 1080 / 412, out: [1080, 2400], bar: 24, os: "android", mobile: true,
    ua: "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Mobile Safari/537.36" },
  "android-tablet": { w: 1280, h: 800, dpr: 2, out: [2560, 1600], bar: 24, os: "android", mobile: true,
    ua: "Mozilla/5.0 (Linux; Android 14; Pixel Tablet) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36" },
};

const procs = [];
function run(cmd, args, env, name) {
  const p = spawn(cmd, args, { env: { ...process.env, ...env }, stdio: ["ignore", "pipe", "pipe"] });
  const log = (d) => { if (process.env.VERBOSE) process.stdout.write(`[${name}] ${d}`); };
  p.stdout.on("data", log); p.stderr.on("data", log);
  procs.push(p);
  return p;
}

export async function startStack({ postbox, sfu = false, extraEnv = {} }) {
  const env = {
    CALLS_PORT: String(P.calls), CALLS_HOST: "127.0.0.1", CALLS_POSTBOX_URL: `http://127.0.0.1:${P.pb}`,
    CALLS_ALLOWED_ORIGINS: `http://127.0.0.1:${P.web}`, CALLS_TRUST_PROXY: "0", CALLS_TURN_SECRET: "",
    POSTBOX_INTERNAL_URL: `http://127.0.0.1:${P.pb}`, POSTBOX_SERVICE_TOKEN: SERVICE_TOKEN, ...extraEnv,
  };
  if (sfu) {
    const yaml = path.join(mkdtempSync(path.join(os.tmpdir(), "pp-store-lk-")), "livekit.yaml");
    writeFileSync(yaml, `port: ${P.lk}\nbind_addresses: ["127.0.0.1"]\nrtc:\n  tcp_port: ${P.lkTcp}\n  port_range_start: 51100\n  port_range_end: 51400\n  use_external_ip: false\n  node_ip: 127.0.0.1\nkeys:\n  APIstore: ${SECRET}\nlogging:\n  level: warn\n`);
    run("livekit-server", ["--config", yaml], {}, "lk");
    Object.assign(env, { CALLS_SFU_URL: `ws://127.0.0.1:${P.lk}`, CALLS_SFU_API_URL: `http://127.0.0.1:${P.lk}`, CALLS_SFU_KEY: "APIstore", CALLS_SFU_SECRET: SECRET, CALLS_SFU_BUDGET_MBPS: "400", CALLS_STREAM_START_RESERVE: "1" });
  }
  await new Promise((r) => postbox.server.listen(P.pb, "127.0.0.1", r));
  run("node", [path.join(REPO, "calls-service/src/server.js")], env, "calls");
  const web = createStatic(path.join(REPO, "site-inbox"), P.calls);
  await new Promise((r) => web.listen(P.web, "127.0.0.1", r));
  await sleep(1500);
  return { stop: () => { web.close(); postbox.server.close(); for (const p of procs) p.kill("SIGTERM"); } };
}

export async function browser() {
  return puppeteer.launch({
    executablePath: "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", headless: true, protocolTimeout: 120000,
    userDataDir: mkdtempSync(path.join(os.tmpdir(), "pp-store-chrome-")),
    args: ["--autoplay-policy=no-user-gesture-required", "--disable-background-timer-throttling", "--disable-renderer-backgrounding",
      "--disable-backgrounding-occluded-windows", "--use-fake-ui-for-media-stream", "--no-first-run", "--hide-scrollbars", "--font-render-hinting=none"],
  });
}

// The clock reads 9:41 this morning, like the status bar: Date is shifted by a fixed offset, so time
// still moves (timers, call clocks) but every wall-clock time the page shows agrees with 9:41.
export const CLOCK = `(() => {
  const R = Date; const t = new R(); t.setHours(9, 41, 0, 0); const off = t.getTime() - R.now();
  class D extends R { constructor(...a) { if (a.length) super(...a); else super(R.now() + off); } static now() { return R.now() + off; } }
  window.Date = D;
})();`;

// Nothing a capture page asks for may leave this machine: any request not to 127.0.0.1 (or a data:
// or blob: URL) is aborted, so no real postbox, sign-in or CDN is ever contacted.
export async function localOnly(page) {
  await page.setRequestInterception(true);
  page.on("request", (r) => {
    const u = r.url();
    if (u.startsWith("http://127.0.0.1:") || u.startsWith("data:") || u.startsWith("blob:")) r.continue(); else r.abort();
  });
}

// A call page (native-call.html, as every app loads it) signed in as one invented person.
export async function callPage(b, name, { known = {}, person = name, scene = null, viewport = { width: 640, height: 480 }, touch = true, address = null, clock = false } = {}) {
  const page = await (await b.createBrowserContext()).newPage();
  page.on("pageerror", (e) => { if (process.env.VERBOSE) console.log(`[${name}] pageerror`, e.message); });
  await localOnly(page);
  page.__touch = touch;
  await page.setViewport({ ...viewport, hasTouch: touch });
  await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "dark" }]);
  await page.evaluateOnNewDocument(`window.PIGEONPOST_INBOX = { postbox: "http://127.0.0.1:${P.pb}" }; window.__PP_PERSON = ${JSON.stringify(person)}; window.__PP_SCENE = ${JSON.stringify(scene)};`);
  if (clock) await page.evaluateOnNewDocument(CLOCK);
  await page.evaluateOnNewDocument(AVATAR_MEDIA);
  await page.goto(`http://127.0.0.1:${P.web}/native-call.html`, { waitUntil: "load" });
  await page.evaluate((n, k, a) => window.PigeonpostNative.configure({ token: "tok-" + n, mailbox: { address: a || "/k/demo" + n, handle: "/" + n }, known: k }), name, known, address);
  return page;
}

// Lay the page out at a device class (viewport less the status bar), screenshot it, and put a
// status bar of the page's own top colour above it at the exact store pixel size.
export async function shoot(page, cls, id, { settle = 1500, before = null, dir = CAPTURES } = {}) {
  const d = DEVICES[cls];
  const barPx = Math.round(d.bar * d.dpr);
  const cssH = (d.out[1] - barPx) / d.dpr;
  await page.bringToFront();
  await page.setUserAgent(d.ua);
  await page.setViewport({ width: d.w, height: Math.round(cssH), deviceScaleFactor: d.dpr, isMobile: false, hasTouch: page.__touch ?? true });
  await sleep(settle);
  if (before) await before(page, cls);
  const tmp = mkdtempSync(path.join(os.tmpdir(), "pp-store-shot-"));
  const body = path.join(tmp, "body.png");
  await page.screenshot({ path: body, captureBeyondViewport: false });
  const top = await page.evaluate(() => {
    const pick = (el) => { while (el) { const c = getComputedStyle(el).backgroundColor; if (c && c !== "rgba(0, 0, 0, 0)" && c !== "transparent") return c; el = el.parentElement; } return "rgb(15, 18, 22)"; };
    return pick(document.elementFromPoint(innerWidth / 2, 2));
  });
  const bar = path.join(tmp, "bar.png");
  await statusBar(page.browser(), d, top, bar);
  const outDir = path.join(dir, cls); mkdirSync(outDir, { recursive: true });
  const out = path.join(outDir, id + ".png");
  execFileSync("magick", [bar, "(", body, "-resize", `${d.out[0]}x${d.out[1] - barPx}!`, ")", "-append", "+repage", "-resize", `${d.out[0]}x${d.out[1]}!`,
    "-background", "black", "-alpha", "remove", "-alpha", "off", "-colorspace", "sRGB", "-strip",
    
    "-define", "png:color-type=2", "-define", "png:exclude-chunks=date,time,tIME,tEXt,zTXt,iTXt", out]);
  rmSync(tmp, { recursive: true, force: true });
  console.log("wrote", path.relative(REPO, out));
  return out;
}

// The status bar: 9:41, full signal, Wi-Fi and battery, drawn in HTML at the device's pixel ratio.
async function statusBar(b, d, background, file) {
  const light = (() => { const m = background.match(/\d+(\.\d+)?/g) || [0, 0, 0]; const [r, g, bl] = m.map(Number); return (0.299 * r + 0.587 * g + 0.114 * bl) < 140; })();
  const ink = light ? "#ffffff" : "#000000";
  const ios = d.os === "ios";
  const phone = d.w < 600;
  const html = `<!doctype html><html><head><style>
    @font-face { font-family: Geist; src: url("file://${REPO}/site-inbox/fonts/Geist-Variable.woff2"); }
    html,body { margin:0; width:${d.w}px; height:${d.bar}px; background:${background}; overflow:hidden; color:${ink};
      font-family: ${ios ? "-apple-system, 'SF Pro Text', Helvetica" : "Roboto, 'Google Sans', Geist, Helvetica"}, sans-serif; }
    .bar { display:flex; align-items:center; justify-content:space-between; height:100%; box-sizing:border-box;
      padding: ${ios ? (phone ? "14px 34px 0 52px" : "0 24px 0 22px") : "0 18px 0 22px"}; }
    .time { font-weight:${ios ? 600 : 500}; font-size:${ios ? (phone ? 17 : 15) : 14}px; letter-spacing:${ios ? "-0.2px" : "0.1px"}; }
    .icons { display:flex; gap:${ios ? 6 : 5}px; align-items:center; }
    svg { display:block; fill:${ink}; }
  </style></head><body><div class="bar">
    <div class="time">9:41${ios && !phone ? "&nbsp;&nbsp;<span style='font-weight:500'>Tue Oct 28</span>" : ""}</div>
    <div class="icons">${ios ? `
      <svg width="18" height="12" viewBox="0 0 18 12"><rect x="0" y="8" width="3" height="4" rx="1"/><rect x="5" y="5.5" width="3" height="6.5" rx="1"/><rect x="10" y="3" width="3" height="9" rx="1"/><rect x="15" y="0" width="3" height="12" rx="1"/></svg>
      <svg width="16" height="12" viewBox="0 0 16 12"><path d="M8 2.2c2.4 0 4.6.9 6.2 2.5l1.2-1.2C13.4 1.5 10.8.5 8 .5S2.6 1.5.6 3.5l1.2 1.2C3.4 3.1 5.6 2.2 8 2.2zm0 3.4c1.5 0 2.8.6 3.8 1.5l1.2-1.2C11.7 4.6 9.9 3.9 8 3.9S4.3 4.6 3 5.9l1.2 1.2c1-.9 2.3-1.5 3.8-1.5zm0 3.4c.6 0 1.1.2 1.5.6L8 11.1 6.5 9.6c.4-.4.9-.6 1.5-.6z"/></svg>
      <svg width="27" height="13" viewBox="0 0 27 13"><rect x=".5" y=".5" width="23" height="12" rx="3.5" fill="none" stroke="${ink}" stroke-opacity=".4"/><rect x="2" y="2" width="20" height="9" rx="2"/><path d="M25 4.5v4c.8-.3 1.3-1.1 1.3-2s-.5-1.7-1.3-2z" fill-opacity=".45"/></svg>`
    : `
      <svg width="16" height="16" viewBox="0 0 24 24"><path d="M12 21.5 23.6 7A17.9 17.9 0 0 0 12 3 17.9 17.9 0 0 0 .4 7z"/></svg>
      <svg width="16" height="16" viewBox="0 0 24 24"><path d="M2 22h20V2z"/></svg>
      <svg width="10" height="16" viewBox="0 0 10 18"><rect x="3" y="0" width="4" height="2" rx=".5"/><rect x="0" y="1.5" width="10" height="16.5" rx="1.5"/></svg>`}
    </div></div></body></html>`;
  const page = await b.newPage();
  await page.setViewport({ width: d.w, height: d.bar, deviceScaleFactor: d.dpr });
  await page.setContent(html, { waitUntil: "load" });
  await page.screenshot({ path: file });
  await page.close();
}
