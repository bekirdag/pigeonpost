// Minimal Chrome DevTools Protocol client: a headless browser peer, or an attached WebView2.
import { spawn } from "node:child_process";
import { existsSync, mkdtempSync } from "node:fs";
import os from "node:os";
import path from "node:path";

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
export async function until(fn, ms = 20000, step = 250, what = "condition") {
  const start = Date.now();
  for (;;) {
    let value; try { value = await fn(); } catch { value = undefined; }
    if (value) return value;
    if (Date.now() - start > ms) throw new Error("timeout waiting for " + what);
    await sleep(step);
  }
}

const browsers = process.platform === "win32"
  ? ["C:/Program Files/Google/Chrome/Application/chrome.exe", "C:/Program Files (x86)/Google/Chrome/Application/chrome.exe",
     "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe", "C:/Program Files/Microsoft/Edge/Application/msedge.exe"]
  : ["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"];

// A headless browser whose page is driven over CDP. Fake devices only; never real hardware.
export async function launch(extra = [], init = null) {
  const exe = browsers.find((b) => existsSync(b));
  if (!exe) throw new Error("No Chrome or Edge found for the browser peer");
  const port = 9400 + Math.floor(Math.random() * 500);
  const dir = mkdtempSync(path.join(os.tmpdir(), "ppcall-"));
  const proc = spawn(exe, ["--headless=new", `--remote-debugging-port=${port}`, `--user-data-dir=${dir}`, "--no-first-run",
    "--use-fake-device-for-media-stream", "--use-fake-ui-for-media-stream", "--autoplay-policy=no-user-gesture-required", ...extra, "about:blank"], { stdio: "ignore" });
  const page = await attach(port, (t) => t.type === "page", 20000, init);
  return { ...page, close: () => { page.close(); proc.kill(); } };
}

// Attach to the first page target matching `match` on a DevTools port (also a WebView2's).
export async function attach(port, match, ms = 30000, init = null) {
  const target = await until(async () => {
    const targets = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
    return targets.find(match);
  }, ms, 250, `DevTools target on port ${port}`);
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => { ws.addEventListener("open", resolve); ws.addEventListener("error", reject); });
  let id = 0; const pending = new Map(); const logs = [];
  ws.addEventListener("message", (e) => {
    const m = JSON.parse(e.data);
    if (m.id && pending.has(m.id)) { const { res, rej } = pending.get(m.id); pending.delete(m.id); m.error ? rej(new Error(JSON.stringify(m.error))) : res(m.result); }
    else if (m.method === "Runtime.consoleAPICalled" || m.method === "Runtime.exceptionThrown") logs.push(JSON.stringify(m.params).slice(0, 300));
  });
  const send = (method, params = {}) => new Promise((res, rej) => { const i = ++id; pending.set(i, { res, rej }); ws.send(JSON.stringify({ id: i, method, params })); });
  const evaluate = async (expression) => {
    const r = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
    if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails).slice(0, 400));
    return r.result.value;
  };
  await send("Runtime.enable"); await send("Page.enable");
  if (init) await send("Page.addScriptToEvaluateOnNewDocument", { source: init });
  return { send, evaluate, logs, url: target.url, close: () => ws.close() };
}
