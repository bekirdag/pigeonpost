// Local call test server: serves site-inbox/ and proxies /calls/* to a local call service.
// The browser peer's inbox page gets the two-persona stand-in postbox (fixture-calls.js) and
// synthetic microphones/cameras (synth-media.js). native-call.html is served unchanged, so the
// Windows app's WebView2 asks for real permissions and gets Chromium's fake devices
// (--use-fake-device-for-media-stream): no real microphone or camera is ever opened.
import http from "node:http";
import { readFile } from "node:fs/promises";
import path from "node:path";

const [root, port, callsPort] = [process.argv[2], Number(process.argv[3]), Number(process.argv[4])];
const here = (name) => new URL("./" + name, import.meta.url);
const types = { ".wasm": "application/wasm", ".html": "text/html", ".css": "text/css", ".js": "application/javascript", ".png": "image/png", ".woff2": "font/woff2", ".ico": "image/x-icon", ".svg": "image/svg+xml" };
const policy = "default-src 'self'; img-src 'self' data: blob:; media-src 'self' blob:; style-src 'self'; script-src 'self' 'wasm-unsafe-eval'; worker-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'; object-src 'none'";

http.createServer(async (req, res) => {
  const url = new URL(req.url, "http://x");
  let file = decodeURIComponent(url.pathname);
  if (file.startsWith("/calls/")) {
    const upstream = http.request({ host: "127.0.0.1", port: callsPort, path: req.url, method: req.method, headers: req.headers }, (r) => { res.writeHead(r.statusCode, r.headers); r.pipe(res); });
    upstream.on("error", () => { if (!res.headersSent) res.writeHead(502); res.end(); });
    req.pipe(upstream);
    res.on("close", () => upstream.destroy());
    return;
  }
  try {
    if (file === "/__synth.js") { res.writeHead(200, { "content-type": "application/javascript" }); return res.end(await readFile(here("synth-media.js"))); }
    if (file === "/__fixture.js") { res.writeHead(200, { "content-type": "application/javascript" }); return res.end(await readFile(here("fixture-calls.js"))); }
    if (file === "/") file = "/index.html";
    if (file.includes("..")) throw new Error("path");
    let body = await readFile(path.join(root, file));
    if (file === "/index.html") body = String(body).replace(/<script src="config\.js(?:\?[^\"]*)?"><\/script>/,
      (tag) => '<script src="/__fixture.js"></script><script src="/__synth.js"></script>' + tag);
    res.writeHead(200, { "content-type": types[path.extname(file)] || "application/octet-stream", "content-security-policy": policy });
    res.end(body);
  } catch { res.writeHead(404); res.end(); }
}).listen(port, "127.0.0.1", () => console.log("serving", port));
