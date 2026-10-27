// meeting-intel HTTP entry point. Binds to loopback; only the calls service (with its token) talks to
// it. See docs/meeting-summaries.md and deploy/meeting-intel/README.md.
//
//   node src/server.js                         serve
//   node src/server.js --generate-key <file>   write a new P-256 signing key (PKCS#8 PEM, mode 0600)
//   node src/server.js --public-key <file>     print the base64 public key browsers pin

import http from "node:http";
import crypto from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { createIntel, IntelError } from "./service.js";
import { createDecoder, createWhisper } from "./audio.js";
import { createSummarizer } from "./summary.js";
import { b64, generateSigningKeyPem, loadSigningKey } from "./crypto.js";

const MAX_BODY = 300 * 1024;

export function configFromEnv(env = process.env) {
  const keyFile = env.INTEL_SIGNING_KEY_FILE || (env.CREDENTIALS_DIRECTORY ? env.CREDENTIALS_DIRECTORY + "/signing.pem" : "");
  const readSecret = (file) => (file ? readFileSync(file, "utf8").trim() : "");
  return {
    port: Number(env.INTEL_PORT || 8797),
    host: env.INTEL_HOST || "127.0.0.1",
    serviceToken: env.INTEL_SERVICE_TOKEN || readSecret(env.INTEL_SERVICE_TOKEN_FILE),
    signingKeyFile: keyFile,
    whisperUrl: env.INTEL_WHISPER_URL || "http://127.0.0.1:8798/inference",
    sttName: env.INTEL_STT_NAME || "whisper.cpp",
    ffmpeg: env.INTEL_FFMPEG || "ffmpeg",
    llmUrl: env.INTEL_LLM_URL || "http://127.0.0.1:11452/v1/chat/completions",
    llmModel: env.INTEL_LLM_MODEL || "qwen-latest-flash-next",
    llmName: env.INTEL_LLM_MODEL || "qwen-latest-flash-next",
    llmKey: readSecret(env.INTEL_LLM_KEY_FILE),
    postboxUrl: (env.POSTBOX_INTERNAL_URL || "http://127.0.0.1:18991").replace(/\/+$/, ""),
    postboxToken: env.POSTBOX_SERVICE_TOKEN || readSecret(env.POSTBOX_SERVICE_TOKEN_FILE),
  };
}

export function createPostboxClient({ url, token, fetch: fetchImpl = globalThis.fetch, timeoutMs = 15_000 }) {
  return async function postbox(method, path, body) {
    const ac = new AbortController();
    const timer = setTimeout(() => ac.abort(), timeoutMs);
    try {
      const res = await fetchImpl(url + path, {
        method, signal: ac.signal,
        headers: { authorization: "Bearer " + token, "content-type": "application/json", accept: "application/json" },
        body: body === undefined ? undefined : JSON.stringify(body),
      });
      let value = null;
      try { value = await res.json(); } catch (_) { value = null; }
      return { status: res.status, body: value };
    } catch (_) {
      return { status: 0, body: null };
    } finally {
      clearTimeout(timer);
    }
  };
}

export function createHandler(cfg, intel) {
  const tokenHash = crypto.createHash("sha256").update(cfg.serviceToken || "").digest();
  function authorized(req) {
    const m = /^Bearer (.+)$/.exec(req.headers.authorization || "");
    if (!m || !cfg.serviceToken) return false;
    return crypto.timingSafeEqual(crypto.createHash("sha256").update(m[1]).digest(), tokenHash);
  }
  async function readJson(req) {
    if (!(req.headers["content-type"] || "").startsWith("application/json")) throw new IntelError(415, "json_required");
    let size = 0;
    const chunks = [];
    for await (const c of req) {
      size += c.length;
      if (size > MAX_BODY) throw new IntelError(413, "too_large");
      chunks.push(c);
    }
    try {
      const v = JSON.parse(Buffer.concat(chunks).toString("utf8") || "{}");
      if (!v || typeof v !== "object" || Array.isArray(v)) throw new Error();
      return v;
    } catch (_) {
      throw new IntelError(400, "bad_json");
    }
  }
  const send = (res, status, value) => {
    res.writeHead(status, { "content-type": "application/json", "cache-control": "no-store" });
    res.end(JSON.stringify(value));
  };
  return async function handle(req, res) {
    const path = new URL(req.url, "http://intel.local").pathname;
    try {
      if (req.method === "GET" && path === "/v1/health") return send(res, 200, intel.health());
      if (!authorized(req)) throw new IntelError(401, "unauthorized");
      if (req.method === "POST" && path === "/v1/sessions") return send(res, 200, intel.open(await readJson(req)));
      const m = /^\/v1\/sessions\/(ses_[a-f0-9]{24})\/(chunks|discard|finish|abort)$/.exec(path);
      if (req.method === "POST" && m) {
        const s = intel.sessionFor(m[1]);
        const body = await readJson(req);
        if (m[2] === "chunks") return send(res, 200, await intel.chunk(s, body));
        if (m[2] === "discard") return send(res, 200, intel.discard(s, body));
        if (m[2] === "abort") return send(res, 200, intel.abort(s));
        intel.finish(s, String(body.reason || "stopped").slice(0, 32));
        return send(res, 200, { finishing: true });
      }
      throw new IntelError(404, "not_found");
    } catch (err) {
      const e = err instanceof IntelError ? err : new IntelError(500, "internal");
      if (!(err instanceof IntelError)) console.error("intel: unexpected error", err && err.name);
      if (!res.headersSent) send(res, e.status, { error: e.code });
      else res.end();
    }
  };
}

function main(argv) {
  if (argv[0] === "--generate-key") {
    writeFileSync(argv[1], generateSigningKeyPem(), { mode: 0o600, flag: "wx" });
    console.log(b64(loadSigningKey(readFileSync(argv[1], "utf8")).publicRaw));
    return;
  }
  if (argv[0] === "--public-key") {
    const k = loadSigningKey(readFileSync(argv[1], "utf8"));
    console.log(JSON.stringify({ key: b64(k.publicRaw), key_id: k.keyId }));
    return;
  }
  const cfg = configFromEnv();
  if (!cfg.serviceToken || cfg.serviceToken.length < 32) throw new Error("INTEL_SERVICE_TOKEN (32+ characters) is required");
  if (!cfg.postboxToken) throw new Error("POSTBOX_SERVICE_TOKEN is required");
  const signing = loadSigningKey(readFileSync(cfg.signingKeyFile, "utf8"));
  const intel = createIntel(cfg, {
    signing,
    decode: createDecoder({ ffmpeg: cfg.ffmpeg }),
    transcribe: createWhisper({ url: cfg.whisperUrl }),
    summarizer: createSummarizer({ url: cfg.llmUrl, model: cfg.llmModel, apiKey: cfg.llmKey }),
    postbox: createPostboxClient({ url: cfg.postboxUrl, token: cfg.postboxToken }),
  });
  const server = http.createServer(createHandler(cfg, intel));
  server.headersTimeout = 15_000;
  server.requestTimeout = 60_000;
  server.listen(cfg.port, cfg.host, () => console.log(`intel: listening on ${cfg.host}:${cfg.port} key_id=${signing.keyId} stt=${cfg.sttName} llm=${cfg.llmModel}`));
  const stop = () => { server.close(); setTimeout(() => process.exit(0), 2_000).unref(); };
  process.on("SIGTERM", stop);
  process.on("SIGINT", stop);
}

if (import.meta.url === `file://${process.argv[1]}`) main(process.argv.slice(2));
