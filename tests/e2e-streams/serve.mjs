// The inbox's static files, with /calls/ proxied to the calls service (as nginx does in production).
import http from "node:http";
import { readFile } from "node:fs/promises";
import path from "node:path";
const TYPES = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".wasm": "application/wasm", ".woff2": "font/woff2", ".png": "image/png", ".json": "application/json" };
export function createStatic(root, callsPort, extra = {}) {
  return http.createServer(async (req, res) => {
    const u = new URL(req.url, "http://x");
    // Test-only routes (engines.mjs): an autopilot viewer page and a place for it to report to.
    if (extra.routes && extra.routes[u.pathname]) return extra.routes[u.pathname](req, res);
    if (u.pathname.startsWith("/calls/")) {
      const up = http.request({ host: "127.0.0.1", port: callsPort, path: req.url, method: req.method, headers: { ...req.headers, host: "127.0.0.1" } }, (r) => { res.writeHead(r.statusCode, r.headers); r.pipe(res); });
      up.on("error", () => { res.writeHead(502); res.end(); });
      req.pipe(up);
      res.on("close", () => up.destroy());
      return;
    }
    const file = path.join(root, path.normalize(u.pathname === "/" ? "/index.html" : u.pathname));
    if (!file.startsWith(root)) { res.writeHead(403); return res.end(); }
    try {
      const data = await readFile(file);
      res.writeHead(200, { "content-type": TYPES[path.extname(file)] || "application/octet-stream", "cache-control": "no-store" });
      res.end(data);
    } catch { res.writeHead(404); res.end(); }
  });
}
