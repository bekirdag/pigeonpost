// Shared pieces of the load harness: a synthetic OIDC issuer, a disposable postbox, a keep-alive
// HTTP client, latency statistics and a CPU/RSS sampler. Loopback only; nothing here talks to a
// deployed service. No dependencies beyond Node itself (>= 22).

import http from "node:http";
import crypto from "node:crypto";
import { spawn, execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync, statSync, existsSync, rmSync, chmodSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const b64u = (buf) => Buffer.from(buf).toString("base64url");
export const log = (...a) => console.error(new Date().toISOString().slice(11, 19), ...a);

// ---- synthetic OIDC issuer -----------------------------------------------------------------------

export async function startIssuer() {
  const { publicKey, privateKey } = crypto.generateKeyPairSync("rsa", { modulusLength: 2048 });
  const jwk = { ...publicKey.export({ format: "jwk" }), kid: "load", alg: "RS256", use: "sig" };
  const body = JSON.stringify({ keys: [jwk] });
  const server = http.createServer((req, res) => {
    if (req.url.endsWith("/protocol/openid-connect/certs")) {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(body);
    } else {
      res.writeHead(404);
      res.end();
    }
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const port = server.address().port;
  const url = `http://127.0.0.1:${port}/realms/load`;
  return {
    port,
    url,
    close: () => server.close(),
    token(sub, strong = false) {
      const now = Math.floor(Date.now() / 1000);
      const head = b64u(JSON.stringify({ alg: "RS256", typ: "JWT", kid: "load" }));
      const claims = {
        iss: url, sub, iat: now, exp: now + 6 * 3600, auth_time: now, azp: "pigeonpost-web",
        typ: "Bearer", aud: ["account"], preferred_username: sub,
      };
      if (strong) claims.acr = "load-mfa";
      const payload = b64u(JSON.stringify(claims));
      const sig = crypto.sign("RSA-SHA256", Buffer.from(`${head}.${payload}`), privateKey);
      return `${head}.${payload}.${b64u(sig)}`;
    },
  };
}

// ---- disposable postbox --------------------------------------------------------------------------

export function freePort() {
  return new Promise((resolve) => {
    const s = http.createServer();
    s.listen(0, "127.0.0.1", () => {
      const p = s.address().port;
      s.close(() => resolve(p));
    });
  });
}

export async function startPostbox(bin, issuer, extraEnv = {}) {
  const work = mkdtempSync(path.join(tmpdir(), "pp-load-"));
  chmodSync(work, 0o700);
  writeFileSync(path.join(work, "kms"), crypto.randomBytes(32));
  writeFileSync(path.join(work, "reserved"), "admin\n");
  const port = await freePort();
  const internalPort = await freePort();
  const ops = "svc_ops_" + crypto.randomBytes(20).toString("hex");
  const billing = "svc_billing_" + crypto.randomBytes(20).toString("hex");
  const calls = "svc_calls_" + crypto.randomBytes(20).toString("hex");
  const env = {
    PATH: "/usr/bin:/bin",
    RUST_LOG: "warn",
    POSTBOX_BIND: `127.0.0.1:${port}`,
    POSTBOX_INTERNAL_BIND: `127.0.0.1:${internalPort}`,
    POSTBOX_PUBLIC_URL: `http://127.0.0.1:${port}`,
    POSTBOX_DB_PATH: `${work}/postbox.db`,
    POSTBOX_KMS: `sealed-file:${work}/kms`,
    POSTBOX_LOFT_DIR: `${work}/loft`,
    PIGEONPOST_BLOB_DIR: `${work}/blobs`,
    PIGEONPOST_BLOB_MIN_FREE_MB: "1",
    REGISTRY_URL: `http://127.0.0.1:${issuer.port}`,
    OIDC_ISSUER: issuer.url,
    POSTBOX_SVC_TOKEN_OPS: ops,
    POSTBOX_SVC_TOKEN_BILLING: billing,
    POSTBOX_SVC_TOKEN_CALLS: calls,
    POSTBOX_STEP_UP_ACR_VALUES: "load-mfa",
    PIGEONPOST_RESERVED_NAMES: `${work}/reserved`,
    ...extraEnv,
  };
  const proc = spawn(bin, [], { env, stdio: ["ignore", "ignore", "pipe"] });
  let stderrTail = "";
  proc.stderr.on("data", (d) => { stderrTail = (stderrTail + d).slice(-4000); });
  const base = `http://127.0.0.1:${port}`;
  const internal = `http://127.0.0.1:${internalPort}`;
  for (let i = 0; i < 240; i++) {
    if (proc.exitCode !== null) throw new Error("postbox exited during startup: " + stderrTail);
    try {
      if ((await request("GET", base + "/health")).status === 200) break;
    } catch (_) { /* not yet */ }
    await sleep(250);
    if (i === 239) throw new Error("postbox did not start");
  }
  return {
    proc, base, internal, ops, billing, calls, work, pid: proc.pid,
    stderrTail: () => stderrTail,
    dbBytes() {
      let n = 0;
      for (const f of ["postbox.db", "postbox.db-wal", "postbox.db-shm"]) {
        const p = path.join(work, f);
        if (existsSync(p)) n += statSync(p).size;
      }
      return n;
    },
    async stop() {
      if (proc.exitCode === null) {
        proc.kill("SIGKILL");
        await new Promise((r) => proc.once("exit", r));
      }
      if (!process.env.LOAD_KEEP) rmSync(work, { recursive: true, force: true });
    },
  };
}

// ---- HTTP ----------------------------------------------------------------------------------------

export const agent = new http.Agent({ keepAlive: true, maxSockets: 4096, maxFreeSockets: 4096 });

export function request(method, url, { token, body, headers = {}, timeoutMs = 30_000, raw = false } = {}) {
  return new Promise((resolve, reject) => {
    const t0 = performance.now();
    const data = body === undefined ? null : Buffer.from(typeof body === "string" ? body : JSON.stringify(body));
    const h = { accept: "application/json", ...headers };
    if (token) h.authorization = "Bearer " + token;
    if (data) { h["content-type"] = h["content-type"] || "application/json"; h["content-length"] = data.length; }
    const req = http.request(url, { method, headers: h, agent }, (res) => {
      const chunks = [];
      res.on("data", (c) => chunks.push(c));
      res.on("end", () => {
        const ms = performance.now() - t0;
        const text = Buffer.concat(chunks).toString();
        let json = null;
        if (!raw) { try { json = JSON.parse(text); } catch (_) { json = null; } }
        resolve({ status: res.statusCode, headers: res.headers, json, text, ms });
      });
      res.on("error", reject);
    });
    req.setTimeout(timeoutMs, () => req.destroy(new Error("timeout")));
    req.on("error", reject);
    if (data) req.write(data);
    req.end();
  });
}

// A long-lived server-sent event stream. `onEvent(name, data)` per frame.
export function openSse(url, { token, headers = {}, onEvent, onOpen, onClose }) {
  const h = { accept: "text/event-stream", ...headers };
  if (token) h.authorization = "Bearer " + token;
  // A dedicated non-pooled socket: a stream never ends, so it must not occupy the shared pool.
  const req = http.request(url, { method: "GET", headers: h, agent: false }, (res) => {
    if (onOpen) onOpen(res.statusCode);
    if (res.statusCode !== 200) { res.resume(); return; }
    let buf = "";
    res.setEncoding("utf8");
    res.on("data", (chunk) => {
      buf += chunk;
      let i;
      while ((i = buf.indexOf("\n\n")) >= 0) {
        const frame = buf.slice(0, i);
        buf = buf.slice(i + 2);
        let name = "message";
        let data = "";
        for (const line of frame.split("\n")) {
          if (line.startsWith("event:")) name = line.slice(6).trim();
          else if (line.startsWith("data:")) data += line.slice(5).trim();
        }
        if (data && onEvent) onEvent(name, data);
      }
    });
    res.on("end", () => onClose && onClose());
  });
  req.on("error", () => onClose && onClose());
  req.end();
  return { close: () => req.destroy() };
}

// ---- statistics ----------------------------------------------------------------------------------

export class Stats {
  constructor(name) {
    this.name = name;
    this.lat = [];
    this.codes = {};
    this.errors = 0;
    this.t0 = performance.now();
    this.t1 = null;
  }
  add(status, ms) {
    this.codes[status] = (this.codes[status] || 0) + 1;
    this.lat.push(ms);
  }
  error() { this.errors++; }
  stop() { this.t1 = performance.now(); return this; }
  summary(okCodes = [200, 201]) {
    const wall = ((this.t1 ?? performance.now()) - this.t0) / 1000;
    const s = [...this.lat].sort((a, b) => a - b);
    const q = (p) => (s.length ? s[Math.min(s.length - 1, Math.floor(p * s.length))] : NaN);
    const ok = okCodes.reduce((n, c) => n + (this.codes[c] || 0), 0);
    const total = s.length + this.errors;
    return {
      name: this.name, requests: total, ok, rps: +(total / wall).toFixed(1), ok_rps: +(ok / wall).toFixed(1),
      p50: +q(0.5).toFixed(1), p95: +q(0.95).toFixed(1), p99: +q(0.99).toFixed(1), max: +(s.at(-1) ?? NaN).toFixed(1),
      codes: this.codes, net_errors: this.errors, seconds: +wall.toFixed(1),
    };
  }
}

// Closed loop: `concurrency` workers each running `fn` back to back for `seconds`.
export async function closedLoop(name, concurrency, seconds, fn) {
  const st = new Stats(name);
  const end = performance.now() + seconds * 1000;
  await Promise.all(Array.from({ length: concurrency }, async (_, w) => {
    let i = 0;
    while (performance.now() < end) {
      try {
        const r = await fn(w, i++);
        st.add(r.status, r.ms);
      } catch (_) {
        st.error();
      }
    }
  }));
  return st.stop();
}

// Open loop: a fixed arrival rate regardless of how slow the server gets (the honest way to see
// queueing). Requests in flight are capped at `maxInFlight`; arrivals over the cap count as dropped.
export async function openLoop(name, ratePerSec, seconds, fn, maxInFlight = 2000) {
  const st = new Stats(name);
  st.dropped = 0;
  const pending = new Set();
  const interval = 1000 / ratePerSec;
  const start = performance.now();
  let n = 0;
  while (performance.now() - start < seconds * 1000) {
    const due = start + n * interval;
    const wait = due - performance.now();
    if (wait > 1) await sleep(wait);
    n++;
    if (pending.size >= maxInFlight) { st.dropped++; continue; }
    const p = fn(n).then((r) => st.add(r.status, r.ms), () => st.error()).finally(() => pending.delete(p));
    pending.add(p);
  }
  await Promise.all(pending);
  return st.stop();
}

// ---- process sampler -----------------------------------------------------------------------------

function cpuSeconds(t) {
  // ps TIME: [[dd-]hh:]mm:ss.cc
  const [a, b] = t.includes("-") ? t.split("-") : [null, t];
  const parts = b.split(":").map(Number);
  let s = 0;
  for (const p of parts) s = s * 60 + p;
  return s + (a ? Number(a) * 86400 : 0);
}

export function procSample(pid) {
  try {
    const out = execFileSync("ps", ["-o", "rss=,time=", "-p", String(pid)], { encoding: "utf8" }).trim();
    const [rss, time] = out.split(/\s+/);
    return { rssMb: Number(rss) / 1024, cpu: cpuSeconds(time) };
  } catch (_) {
    return { rssMb: NaN, cpu: NaN };
  }
}

// Wraps a phase: CPU % (of one core) over the phase and peak RSS seen by a 500 ms sampler.
export async function measured(pid, fn) {
  const a = procSample(pid);
  const t0 = performance.now();
  let peak = a.rssMb;
  const timer = setInterval(() => { peak = Math.max(peak, procSample(pid).rssMb); }, 500);
  try {
    const result = await fn();
    const b = procSample(pid);
    const wall = (performance.now() - t0) / 1000;
    peak = Math.max(peak, b.rssMb);
    return { result, cpuPct: +((100 * (b.cpu - a.cpu)) / wall).toFixed(0), rssMb: +b.rssMb.toFixed(0), peakRssMb: +peak.toFixed(0) };
  } finally {
    clearInterval(timer);
  }
}

export function table(rows, cols) {
  const head = "| " + cols.join(" | ") + " |";
  const sep = "|" + cols.map(() => "---").join("|") + "|";
  return [head, sep, ...rows.map((r) => "| " + cols.map((c) => (r[c] === undefined ? "" : typeof r[c] === "object" ? JSON.stringify(r[c]) : r[c])).join(" | ") + " |")].join("\n");
}
