// Pigeonpost Nest runtime helper for Node (ESM, no dependencies).
//
// A marketplace Nest app runs as one long-lived process inside its sandbox. The platform hands it
// jobs as files and reads results back as files; the app's only network peer is the model broker.
// This module is that contract and nothing more:
//
//   <root>/jobs/in/<job>.json    one job: {job_id, operation, operation_version, payload,
//                                config?}; config is the installation's configuration
//   <root>/jobs/out/<job>.json   its result: {job_id, status: "succeeded", answer, sources,
//                                model_used} or {job_id, status: "failed", error_code}
//   <root>/run/broker-token      the broker token for this start (nbt_…)
//   NEST_BROKER_URL              http://host:port of the broker
//
// <root> is NEST_ROOT, or /nest in the sandbox. Copy this file into your app; it is not published
// to a registry. The same contract in Python: crates/pigeonpost-nest-kit/sdk/python/pigeonpost_nest.py.

import fs from "node:fs";
import http from "node:http";
import path from "node:path";

export const SDK_VERSION = "0.1.0";
/** Largest job file read, as the postbox caps requests at 256 KiB. */
export const MAX_JOB_BYTES = 256 * 1024;
/** Largest result the dispatcher reads back. */
export const MAX_RESULT_BYTES = 512 * 1024;
const MAX_REPLY_BYTES = 512 * 1024;
const BROKER_TIMEOUT_MS = 120_000;
const MAX_PROGRESS_PER_JOB = 100;
const ERROR_CODE = /^[a-z][a-z0-9_]{0,63}$/;
const EFFECT_KEY = /^[A-Za-z0-9._:-]{1,128}$/;
const EFFECT_KIND = /^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$/;

/** A job outcome the app chose: the job fails with `code` and nothing else leaves the sandbox. */
export class NestError extends Error {
  constructor(code, message) {
    super(message || code);
    this.name = "NestError";
    this.code = ERROR_CODE.test(code) ? code : "internal_error";
  }
}

/** The broker refused or could not be reached. `code` is the broker's error code. */
export class BrokerError extends NestError {
  constructor(code, message, status) {
    super(code, message);
    this.name = "BrokerError";
    this.status = status ?? 0;
  }
}

export function nestRoot(env = process.env) {
  return env.NEST_ROOT || "/nest";
}

function readJson(file, limit) {
  try {
    // Only a bounded regular file: never follow a link someone left in the inbox.
    const stat = fs.lstatSync(file);
    if (!stat.isFile() || stat.size > limit) return null;
    return JSON.parse(fs.readFileSync(file, "utf8"));
  } catch {
    return null;
  }
}

/** The installation's configuration from a job, or `{}` when it carries none. Read-only. */
export function configOf(raw) {
  const c = raw?.config;
  return Object.freeze(c && typeof c === "object" && !Array.isArray(c) ? { ...c } : {});
}

/** One HTTP/1.1 request to the broker. Resolves {status, body} or rejects with {sent, error}. */
function brokerRequest(baseUrl, token, method, urlPath, body, extraHeaders = {}) {
  return new Promise((resolve, reject) => {
    const url = new URL(urlPath, baseUrl);
    const data = body === undefined ? "" : JSON.stringify(body);
    const req = http.request(
      {
        method,
        hostname: url.hostname,
        port: url.port,
        path: url.pathname,
        headers: {
          authorization: `Bearer ${token}`,
          "content-type": "application/json",
          "content-length": Buffer.byteLength(data),
          ...extraHeaders,
        },
        timeout: BROKER_TIMEOUT_MS,
        agent: false,
      },
      (res) => {
        const chunks = [];
        let size = 0;
        res.on("data", (c) => {
          size += c.length;
          if (size > MAX_REPLY_BYTES) {
            req.destroy(new Error("reply too large"));
            return;
          }
          chunks.push(c);
        });
        res.on("end", () => {
          let parsed = null;
          try {
            parsed = JSON.parse(Buffer.concat(chunks).toString("utf8") || "null");
          } catch {
            parsed = null;
          }
          resolve({ status: res.statusCode, body: parsed });
        });
        res.on("error", (error) => reject({ sent: true, error }));
      },
    );
    let connected = false;
    req.on("socket", (s) => s.on("connect", () => (connected = true)));
    req.on("timeout", () => req.destroy(new Error("timed out")));
    req.on("error", (error) => reject({ sent: connected, error }));
    req.end(data);
  });
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/**
 * The model broker as one app sees it. The broker chooses the model, caps every request, and
 * charges the Nest's monthly allowance; the app never holds a provider key.
 */
export class Broker {
  constructor({ url = process.env.NEST_BROKER_URL, root = nestRoot() } = {}) {
    this.url = url && /^http:\/\/[^/]+\/?$/.test(url) ? url : null;
    this.root = root;
    this.used = false;
    this.usage = { input: 0, output: 0, requests: 0 };
  }

  token() {
    try {
      const t = fs.readFileSync(path.join(this.root, "run", "broker-token"), "utf8").trim();
      return t.startsWith("nbt_") ? t : null;
    } catch {
      return null;
    }
  }

  /** Whether a broker is configured for this start. A no-AI Nest never needs one. */
  get available() {
    return this.url !== null && this.token() !== null;
  }

  async call(method, urlPath, body, headers) {
    const token = this.token();
    if (this.url === null || token === null) {
      throw new BrokerError("no_broker", "no broker is configured for this Nest");
    }
    return brokerRequest(this.url, token, method, urlPath, body, headers);
  }

  /**
   * One completion. `messages` is [{role: "system"|"user"|"assistant", content}], or pass
   * `system` and `user`. Returns {text, usage: {input, output}}.
   *
   * Busy (one request at a time per Nest) is retried briefly, because the broker refuses it before
   * anything is reserved or sent. Nothing else is retried: an upstream failure may already have
   * reached the model provider.
   */
  async complete({ system, user, messages, maxTokens = 400 } = {}) {
    const msgs = messages ?? [
      ...(system ? [{ role: "system", content: String(system) }] : []),
      { role: "user", content: String(user ?? "") },
    ];
    const body = { messages: msgs, max_tokens: maxTokens };
    for (let attempt = 0; ; attempt++) {
      let reply;
      try {
        reply = await this.call("POST", "/v1/chat/completions", body);
      } catch (e) {
        if (e instanceof BrokerError) throw e;
        throw new BrokerError(e.sent ? "broker_outcome_unknown" : "broker_unreachable", String(e.error?.message ?? e));
      }
      if (reply.status === 409 && reply.body?.error === "busy" && attempt < 3) {
        await sleep(250 * 2 ** attempt);
        continue;
      }
      if (reply.status !== 200) {
        const code = typeof reply.body?.error === "string" ? reply.body.error : "broker_error";
        throw new BrokerError(code, reply.body?.detail ?? `broker answered ${reply.status}`, reply.status);
      }
      const text = reply.body?.choices?.[0]?.message?.content;
      if (typeof text !== "string") throw new BrokerError("broker_error", "no answer in the reply", 200);
      const usage = {
        input: Number(reply.body?.usage?.prompt_tokens ?? 0),
        output: Number(reply.body?.usage?.completion_tokens ?? 0),
      };
      this.used = true;
      this.usage.input += usage.input;
      this.usage.output += usage.output;
      this.usage.requests += 1;
      return { text, usage };
    }
  }

  /** This Nest's monthly allowance: {period, input_tokens: {used, limit}, output_tokens: {…}}. */
  async budget() {
    let reply;
    try {
      reply = await this.call("GET", "/v1/budget");
    } catch (e) {
      if (e instanceof BrokerError) throw e;
      throw new BrokerError("broker_unreachable", String(e.error?.message ?? e));
    }
    if (reply.status !== 200) throw new BrokerError(reply.body?.error ?? "broker_error", "budget refused", reply.status);
    return reply.body;
  }

  /**
   * One of the installation's secret settings (`writeOnly` in config_schema). Secrets are never
   * in job.config or the environment; ask the broker when you need one and do not write it to the
   * volume or a log. Throws BrokerError `secret_not_set` when it is not set.
   */
  async secret(name) {
    if (typeof name !== "string" || !/^[a-z][a-z0-9_]{0,63}$/.test(name)) throw new NestError("invalid_secret_name");
    let reply;
    try {
      reply = await this.call("GET", `/v1/secrets/${name}`);
    } catch (e) {
      if (e instanceof BrokerError) throw e;
      throw new BrokerError("broker_unreachable", String(e.error?.message ?? e));
    }
    if (reply.status !== 200 || typeof reply.body?.value !== "string") {
      throw new BrokerError(reply.body?.error ?? "broker_error", "secret refused", reply.status);
    }
    return reply.body.value;
  }
}

/** What a handler receives. */
export class Job {
  constructor(raw, { broker, config }) {
    this.id = typeof raw.job_id === "string" ? raw.job_id : "";
    this.operation = raw.operation;
    this.version = raw.operation_version;
    this.payload = raw.payload && typeof raw.payload === "object" ? raw.payload : {};
    this.config = config;
    this.broker = broker;
    this.effects = new Map();
    this.progressSent = 0;
  }

  /**
   * Report progress (0..1) with a short message. Best effort: a platform that does not take
   * progress yet ignores it, and a failure never fails the job.
   */
  async progress(fraction, message = "") {
    if (this.progressSent >= MAX_PROGRESS_PER_JOB || !this.broker.available) return;
    this.progressSent++;
    const f = Math.max(0, Math.min(1, Number(fraction) || 0));
    try {
      await this.broker.call("POST", "/v1/progress", {
        job_id: this.id,
        fraction: f,
        message: String(message).slice(0, 200),
      });
    } catch {
      // Progress is advisory.
    }
  }

  /**
   * Ask the platform to perform one external effect (a write outside the Nest) on the buyer's
   * behalf. The Nest itself has no route to the outside: the effect goes to the broker, which
   * holds the buyer's grant, asks for approval when `requiresApproval` is set, and performs it at
   * most once per `key`.
   *
   * `key` is your idempotency key: derive it from the job (its id and input, never model output)
   * so that a redelivered job asks for the same effect, never a second one. The first request made
   * under a key is the one approved and performed; the outcome ({reference, body}) carries that
   * request's body back. Returns the outcome object. Throws NestError with
   * `approval_required` (not approved yet), `effects_unavailable` (this platform does not offer
   * external writes), `idempotency_conflict` (same key, another kind), `effect_refused` or
   * `effect_outcome_unknown` (the request was sent and no answer came back; it is reconciled by
   * the platform and never retried here).
   */
  async effect({ key, kind, summary = "", body = {}, requiresApproval = false }) {
    if (!EFFECT_KEY.test(String(key))) throw new NestError("invalid_effect", "effect key: 1-128 of A-Z a-z 0-9 . _ : -");
    if (!EFFECT_KIND.test(String(kind))) throw new NestError("invalid_effect", "effect kind: dotted lower-case words");
    if (this.effects.has(key)) return this.effects.get(key);
    const request = {
      job_id: this.id,
      key,
      kind,
      summary: String(summary).slice(0, 500),
      body,
      requires_approval: Boolean(requiresApproval),
    };
    let reply;
    try {
      reply = await this.broker.call("POST", "/v1/effects", request, { "idempotency-key": key });
    } catch (e) {
      if (e instanceof BrokerError && e.code === "no_broker") throw new NestError("effects_unavailable");
      if (e.sent) throw new NestError("effect_outcome_unknown", String(e.error?.message ?? e));
      throw new NestError("effects_unavailable", String(e.error?.message ?? e));
    }
    if (reply.status === 200 && reply.body?.state === "done") {
      const outcome = reply.body.outcome ?? {};
      this.effects.set(key, outcome);
      return outcome;
    }
    if (reply.status === 202 || reply.body?.state === "awaiting_approval") throw new NestError("approval_required");
    if (reply.status === 404) throw new NestError("effects_unavailable");
    if (reply.status === 409) throw new NestError("idempotency_conflict");
    if (reply.status === 403) throw new NestError("effect_refused", reply.body?.detail);
    throw new NestError("effect_outcome_unknown", `broker answered ${reply.status}`);
  }
}

function failed(jobId, code) {
  return { job_id: jobId, status: "failed", error_code: ERROR_CODE.test(code) ? code : "internal_error" };
}

/** Turn what a handler returned into the result the dispatcher reads. */
export function resultOf(jobId, value, modelUsed) {
  const shaped =
    value && typeof value === "object" && !Array.isArray(value) && "answer" in value
      ? value
      : { answer: value ?? null };
  const out = {
    job_id: jobId,
    status: "succeeded",
    answer: shaped.answer ?? null,
    sources: Array.isArray(shaped.sources) ? shaped.sources : [],
    model_used: typeof shaped.model_used === "boolean" ? shaped.model_used : modelUsed,
  };
  if (Buffer.byteLength(JSON.stringify(out)) > MAX_RESULT_BYTES) return failed(jobId, "result_too_large");
  return out;
}

/** Run one job file through the handlers and write its result. Returns the result. */
export async function processJob(file, handlers, { root = nestRoot(), url = process.env.NEST_BROKER_URL } = {}) {
  const raw = readJson(file, MAX_JOB_BYTES);
  const jobId = typeof raw?.job_id === "string" ? raw.job_id : "";
  let result;
  if (raw === null || typeof raw !== "object" || Array.isArray(raw)) {
    result = failed(jobId, "invalid_job");
  } else {
    const handler = handlers[`${raw.operation}@${raw.operation_version}`];
    if (typeof handler !== "function") {
      result = failed(jobId, "unknown_operation");
    } else {
      const broker = new Broker({ url, root });
      try {
        const job = new Job(raw, { broker, config: configOf(raw) });
        result = resultOf(jobId, await handler(job), broker.used);
      } catch (e) {
        if (!(e instanceof NestError)) process.stderr.write(`pigeonpost-nest: ${jobId}: ${e?.stack ?? e}\n`);
        result = failed(jobId, e instanceof NestError ? e.code : "internal_error");
      }
    }
  }
  const out = path.join(root, "jobs", "out", path.basename(file));
  const tmp = out.replace(/\.json$/, ".tmp");
  fs.writeFileSync(tmp, JSON.stringify(result));
  fs.renameSync(tmp, out);
  fs.rmSync(file, { force: true });
  return result;
}

/** The job files waiting now, oldest name first. */
export function pendingJobs(root = nestRoot()) {
  const inbox = path.join(root, "jobs", "in");
  try {
    return fs
      .readdirSync(inbox)
      .filter((n) => n.endsWith(".json"))
      .sort()
      .map((n) => path.join(inbox, n));
  } catch {
    return [];
  }
}

/** Process every waiting job once. Returns how many were processed. */
export async function runOnce(handlers, opts = {}) {
  const root = opts.root ?? nestRoot();
  fs.mkdirSync(path.join(root, "jobs", "in"), { recursive: true });
  fs.mkdirSync(path.join(root, "jobs", "out"), { recursive: true });
  let n = 0;
  for (const file of pendingJobs(root)) {
    await processJob(file, handlers, { ...opts, root });
    n++;
  }
  return n;
}

/**
 * The app's main loop: process jobs as they arrive, forever. `handlers` maps
 * "operation@version" to an async function (job) => result.
 */
export async function serve(handlers, opts = {}) {
  const pollMs = opts.pollMs ?? 500;
  for (;;) {
    try {
      await runOnce(handlers, opts);
    } catch (e) {
      // A volume error must not stop the worker; the job stays in the inbox for the next pass.
      process.stderr.write(`pigeonpost-nest: ${e?.stack ?? e}\n`);
    }
    await sleep(pollMs);
  }
}
