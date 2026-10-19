// Pigeonpost calls — signaling and relay credentials for one-to-one web calls.
//
// What this service is: a rendezvous. It tells a mailbox that another mailbox is ringing it, carries
// the WebRTC offer/answer and ICE candidates between the two browsers, and mints short-lived TURN
// credentials. What it is not: a media path. Audio and video travel browser to browser (or through
// the TURN relay) encrypted with DTLS-SRTP keys that never leave the two endpoints; nothing here can
// read them, and the clients show a security code derived from both endpoints' DTLS fingerprints so
// two people can confirm that this service did not put itself in the middle.
//
// Who may do what is decided by the postbox, not here. Every request carries the member's own token;
// this service asks the postbox which mailboxes that token's account owns (`/v1/identities`, then
// `/v1/whoami` for each mailbox's handle) and acts only within that set. It holds no account data of
// its own and keeps every call in memory for its lifetime only.
//
// Dependency-free on purpose: Node's http, crypto and timers are the whole runtime.

import http from "node:http";
import crypto from "node:crypto";

export function configFromEnv(env = process.env) {
  const turnUrls = (env.CALLS_TURN_URLS || "").split(/[\s,]+/).filter(Boolean);
  return {
    port: Number(env.CALLS_PORT || 8791),
    host: env.CALLS_HOST || "127.0.0.1",
    postbox: (env.CALLS_POSTBOX_URL || "https://postbox.pigeonpost.dev").replace(/\/+$/, ""),
    turnSecret: env.CALLS_TURN_SECRET || "",
    turnUrls,
    // Long enough to outlast a long call: coturn re-checks the credential on every allocation
    // refresh, so a credential that expires mid-call drops the relayed media with it.
    turnTtlSeconds: Number(env.CALLS_TURN_TTL_SECONDS || 6 * 3600),
    allowedOrigins: (env.CALLS_ALLOWED_ORIGINS || "https://inbox.pigeonpost.dev").split(/[\s,]+/).filter(Boolean),
    ringMs: Number(env.CALLS_RING_MS || 45_000),
  };
}

const LIMITS = {
  bodyBytes: 128 * 1024,
  sdpChars: 100_000,
  candidateChars: 1_000,
  tokenChars: 8_192,
  sessionsPerAccount: 12,
  sessionsTotal: 5_000,
  callsTotal: 2_000,
  callsPerMinute: 6,
  callsPerHour: 60,
  signalsPerMinute: 400,
  authCacheEntries: 10_000,
  authCacheMs: 60_000,
  heartbeatMs: 20_000,
  resumeGraceMs: 20_000,
  maxCallMs: 4 * 3600_000,
  endedRetentionMs: 60_000,
};

const ADDRESS = /^\/[A-Za-z0-9._~\-/]{1,200}$/;
const KINDS = new Set(["audio", "video"]);
const SIGNALS = new Set(["offer", "answer", "ice"]);

class HttpError extends Error {
  constructor(status, code, message) {
    super(message || code);
    this.status = status;
    this.code = code;
  }
}

const sha256 = (s) => crypto.createHash("sha256").update(s).digest("hex");
const newId = () => crypto.randomBytes(16).toString("base64url");
const norm = (address) => String(address).toLowerCase();

export function createCallsService(cfg, deps = {}) {
  const fetchImpl = deps.fetch || globalThis.fetch;
  const now = deps.now || (() => Date.now());

  const authCache = new Map(); // sha256(token) -> { account, mailboxes, names, expires }
  const sessions = new Map(); // session id -> session
  const sessionsByName = new Map(); // normalized address/handle -> Set<session>
  const calls = new Map(); // call id -> call
  const callRate = new Map(); // account -> number[] (timestamps)

  // ---- authentication: the postbox decides which mailboxes a token may act as ----------------

  async function postboxJson(path, token) {
    const res = await fetchImpl(cfg.postbox + path, {
      headers: { authorization: "Bearer " + token, accept: "application/json" },
    });
    if (res.status === 401 || res.status === 403) throw new HttpError(401, "unauthorized");
    if (!res.ok) throw new HttpError(502, "postbox_unavailable");
    return res.json();
  }

  async function authenticate(req) {
    const header = req.headers.authorization || "";
    const match = /^Bearer ([A-Za-z0-9._~+/=-]+)$/.exec(header);
    if (!match || match[1].length > LIMITS.tokenChars) throw new HttpError(401, "unauthorized");
    const token = match[1];
    const key = sha256(token);
    const hit = authCache.get(key);
    if (hit && hit.expires > now()) return hit;

    const { identities } = await postboxJson("/v1/identities", token);
    const list = Array.isArray(identities) ? identities.slice(0, 200) : [];
    const mailboxes = await Promise.all(list.map(async (identity) => {
      const address = String(identity.address || "");
      if (!ADDRESS.test(address)) return null;
      try {
        const who = await postboxJson("/v1/whoami?identity=" + encodeURIComponent(address), token);
        const handle = typeof who.handle === "string" && ADDRESS.test(who.handle) ? who.handle : null;
        return { address, handle };
      } catch (err) {
        if (err.code === "unauthorized") throw err;
        return { address, handle: null };
      }
    }));
    const owned = mailboxes.filter(Boolean);
    if (!owned.length) throw new HttpError(403, "no_mailbox");
    const names = new Set();
    for (const m of owned) {
      names.add(norm(m.address));
      if (m.handle) names.add(norm(m.handle));
    }
    // An account is identified by the set of mailboxes it owns. Nothing else about it is known here.
    const account = sha256(owned.map((m) => m.address).sort().join("\n")).slice(0, 32);
    const entry = { account, mailboxes: owned, names, expires: now() + LIMITS.authCacheMs };
    if (authCache.size >= LIMITS.authCacheEntries) authCache.delete(authCache.keys().next().value);
    authCache.set(key, entry);
    return entry;
  }

  function mailboxFor(auth, address) {
    const n = norm(address);
    return auth.mailboxes.find((m) => norm(m.address) === n || (m.handle && norm(m.handle) === n)) || null;
  }

  // ---- event streams ---------------------------------------------------------------------------

  function send(session, event, data) {
    if (!session || session.closed) return false;
    try {
      session.res.write(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`);
      return true;
    } catch (_) {
      return false;
    }
  }

  function openStream(req, res, auth) {
    let accountSessions = 0;
    for (const s of sessions.values()) if (s.account === auth.account) accountSessions++;
    if (accountSessions >= LIMITS.sessionsPerAccount) throw new HttpError(429, "too_many_sessions");
    if (sessions.size >= LIMITS.sessionsTotal) throw new HttpError(503, "busy");

    res.writeHead(200, {
      "content-type": "text/event-stream; charset=utf-8",
      "cache-control": "no-store",
      "x-accel-buffering": "no",
      connection: "keep-alive",
    });
    const session = { id: newId(), account: auth.account, names: auth.names, mailboxes: auth.mailboxes, res, closed: false };
    sessions.set(session.id, session);
    for (const name of session.names) {
      if (!sessionsByName.has(name)) sessionsByName.set(name, new Set());
      sessionsByName.get(name).add(session);
    }
    send(session, "ready", { session: session.id, mailboxes: auth.mailboxes });
    const beat = setInterval(() => {
      try { res.write(": keepalive\n\n"); } catch (_) { /* closed below */ }
    }, LIMITS.heartbeatMs);
    beat.unref?.();
    const close = () => {
      if (session.closed) return;
      session.closed = true;
      clearInterval(beat);
      sessions.delete(session.id);
      for (const name of session.names) {
        const set = sessionsByName.get(name);
        if (set) { set.delete(session); if (!set.size) sessionsByName.delete(name); }
      }
      sessionGone(session);
    };
    req.on("close", close);
    res.on("error", close);
  }

  function liveSession(auth, id) {
    const session = sessions.get(String(id || ""));
    if (!session || session.closed || session.account !== auth.account) throw new HttpError(409, "no_session");
    return session;
  }

  // ---- calls -------------------------------------------------------------------------------------

  function publicCall(call) {
    return { call: call.id, kind: call.kind, from: call.from, to: call.to, state: call.state };
  }

  function rateLimited(account) {
    const t = now();
    const recent = (callRate.get(account) || []).filter((x) => t - x < 3600_000);
    callRate.set(account, recent);
    const lastMinute = recent.filter((x) => t - x < 60_000).length;
    if (lastMinute >= LIMITS.callsPerMinute || recent.length >= LIMITS.callsPerHour) return true;
    recent.push(t);
    return false;
  }

  function isEngaged(name) {
    for (const call of calls.values()) {
      if (call.state === "ended") continue;
      if (norm(call.from.address) === name || (call.from.handle && norm(call.from.handle) === name)) return true;
      if (call.callee && (norm(call.callee.address) === name || (call.callee.handle && norm(call.callee.handle) === name))) return true;
      if (call.toName === name && call.state !== "ringing") return true;
    }
    return false;
  }

  function sessionInCall(session) {
    for (const call of calls.values()) {
      if (call.state !== "ended" && (call.callerSession === session.id || call.calleeSession === session.id)) return true;
    }
    return false;
  }

  function startCall(auth, body) {
    const session = liveSession(auth, body.session);
    const kind = KINDS.has(body.kind) ? body.kind : null;
    if (!kind) throw new HttpError(400, "bad_kind");
    const from = mailboxFor(auth, body.from || "");
    if (!from) throw new HttpError(403, "not_your_mailbox");
    const to = String(body.to || "");
    if (!ADDRESS.test(to)) throw new HttpError(400, "bad_recipient");
    if (mailboxFor(auth, to) && norm(mailboxFor(auth, to).address) === norm(from.address)) {
      throw new HttpError(400, "cannot_call_self");
    }
    if (sessionInCall(session)) throw new HttpError(409, "already_in_call");
    if (calls.size >= LIMITS.callsTotal) throw new HttpError(503, "busy");
    if (rateLimited(auth.account)) throw new HttpError(429, "rate_limited");

    const toName = norm(to);
    const call = {
      id: newId(), kind, from, to, toName, state: "ringing",
      callerAccount: auth.account, callerSession: session.id,
      callee: null, calleeAccount: null, calleeSession: null,
      ringing: new Set(), createdAt: now(), timers: [], grace: null,
    };
    calls.set(call.id, call);

    // Busy and offline both look like an unanswered call from the outside: the caller learns
    // nothing about whether the other mailbox is online until somebody there picks up.
    if (!isEngagedOtherThan(toName, call)) {
      for (const target of sessionsByName.get(toName) || []) {
        if (target.id === session.id) continue;
        if (sessionInCall(target)) continue;
        call.ringing.add(target.id);
        send(target, "incoming", publicCall(call));
      }
    }
    call.timers.push(setTimeout(() => end(call, "no_answer"), cfg.ringMs));
    call.timers.push(setTimeout(() => end(call, "max_duration"), LIMITS.maxCallMs));
    for (const t of call.timers) t.unref?.();
    return publicCall(call);
  }

  function isEngagedOtherThan(name, call) {
    const saved = call.state;
    call.state = "ended";
    try { return isEngaged(name); } finally { call.state = saved; }
  }

  function callFor(id) {
    const call = calls.get(String(id || ""));
    if (!call) throw new HttpError(404, "no_call");
    return call;
  }

  function accept(auth, call, body) {
    const session = liveSession(auth, body.session);
    if (call.state !== "ringing") throw new HttpError(409, "not_ringing");
    if (!call.ringing.has(session.id)) throw new HttpError(403, "not_your_call");
    const callee = mailboxFor(auth, call.to);
    if (!callee) throw new HttpError(403, "not_your_call");
    call.state = "connecting";
    call.callee = callee;
    call.calleeAccount = auth.account;
    call.calleeSession = session.id;
    clearTimeout(call.timers[0]);
    for (const id of call.ringing) {
      if (id !== session.id) send(sessions.get(id), "ended", { call: call.id, reason: "answered_elsewhere" });
    }
    call.ringing.clear();
    send(sessions.get(call.callerSession), "accepted", { call: call.id, callee });
    return publicCall(call);
  }

  function decline(auth, call, body) {
    const session = liveSession(auth, body.session);
    if (call.state !== "ringing" || !call.ringing.has(session.id)) throw new HttpError(409, "not_ringing");
    end(call, body.reason === "busy" ? "busy" : "declined", session.id);
    return { call: call.id, state: "ended" };
  }

  function hangUp(auth, call, body) {
    const session = liveSession(auth, body.session);
    const party = call.callerSession === session.id || call.calleeSession === session.id;
    if (!party) throw new HttpError(403, "not_your_call");
    end(call, call.state === "ringing" ? "cancelled" : "ended", session.id);
    return { call: call.id, state: "ended" };
  }

  function signal(auth, call, body) {
    const session = liveSession(auth, body.session);
    if (call.state !== "connecting" && call.state !== "active") throw new HttpError(409, "not_connected");
    const fromCaller = call.callerSession === session.id;
    const fromCallee = call.calleeSession === session.id;
    if (!fromCaller && !fromCallee) throw new HttpError(403, "not_your_call");
    if (!SIGNALS.has(body.type)) throw new HttpError(400, "bad_signal");
    const data = validSignal(body.type, body.data);
    const counter = fromCaller ? "callerSignals" : "calleeSignals";
    const t = now();
    call[counter] = (call[counter] || []).filter((x) => t - x < 60_000);
    if (call[counter].length >= LIMITS.signalsPerMinute) throw new HttpError(429, "rate_limited");
    call[counter].push(t);
    if (body.type === "answer" && fromCallee) call.state = "active";
    const peer = sessions.get(fromCaller ? call.calleeSession : call.callerSession);
    if (!send(peer, "signal", { call: call.id, type: body.type, data })) throw new HttpError(409, "peer_unreachable");
    return { ok: true };
  }

  function validSignal(type, data) {
    if (type === "offer" || type === "answer") {
      if (!data || data.type !== type || typeof data.sdp !== "string" || data.sdp.length > LIMITS.sdpChars) {
        throw new HttpError(400, "bad_signal");
      }
      return { type, sdp: data.sdp };
    }
    if (data === null) return null; // end of candidates
    if (!data || typeof data.candidate !== "string" || data.candidate.length > LIMITS.candidateChars) {
      throw new HttpError(400, "bad_signal");
    }
    const out = { candidate: data.candidate };
    if (typeof data.sdpMid === "string" && data.sdpMid.length <= 64) out.sdpMid = data.sdpMid;
    if (Number.isInteger(data.sdpMLineIndex) && data.sdpMLineIndex >= 0 && data.sdpMLineIndex < 16) {
      out.sdpMLineIndex = data.sdpMLineIndex;
    }
    return out;
  }

  // A page reload or a dropped event stream gives the browser a new session. Media between the two
  // browsers does not depend on this service, so the call stays up for a short grace period and the
  // same mailbox may take it back over.
  function resume(auth, call, body) {
    const session = liveSession(auth, body.session);
    if (call.state === "ended" || call.state === "ringing") throw new HttpError(409, "not_connected");
    let role = null;
    if (call.callerAccount === auth.account) role = "caller";
    else if (call.calleeAccount === auth.account) role = "callee";
    if (!role) throw new HttpError(403, "not_your_call");
    call[role === "caller" ? "callerSession" : "calleeSession"] = session.id;
    clearTimeout(call.grace);
    call.grace = null;
    return { ...publicCall(call), role };
  }

  function sessionGone(session) {
    for (const call of calls.values()) {
      if (call.state === "ended") continue;
      if (call.ringing.delete(session.id) && call.state === "ringing") continue;
      if (call.callerSession === session.id && call.state === "ringing") {
        end(call, "cancelled", session.id);
        continue;
      }
      if (call.callerSession === session.id || call.calleeSession === session.id) {
        clearTimeout(call.grace);
        call.grace = setTimeout(() => {
          const caller = sessions.get(call.callerSession), callee = sessions.get(call.calleeSession);
          if (!caller || !callee) end(call, "disconnected");
        }, LIMITS.resumeGraceMs);
        call.grace.unref?.();
      }
    }
  }

  function end(call, reason, bySession) {
    if (call.state === "ended") return;
    call.state = "ended";
    call.reason = reason;
    for (const t of call.timers) clearTimeout(t);
    clearTimeout(call.grace);
    const notify = new Set([call.callerSession, call.calleeSession, ...call.ringing]);
    for (const id of notify) {
      if (id && id !== bySession) send(sessions.get(id), "ended", { call: call.id, reason });
    }
    call.ringing.clear();
    const cleanup = setTimeout(() => calls.delete(call.id), LIMITS.endedRetentionMs);
    cleanup.unref?.();
  }

  // ---- TURN credentials (RFC 8489 long-term credential, coturn "use-auth-secret" form) ----------

  function turnCredentials(auth) {
    if (!cfg.turnSecret || !cfg.turnUrls.length) return { iceServers: [], ttl: 0 };
    const expires = Math.floor(now() / 1000) + cfg.turnTtlSeconds;
    const username = `${expires}:${auth.account.slice(0, 12)}`;
    const credential = crypto.createHmac("sha1", cfg.turnSecret).update(username).digest("base64");
    return { iceServers: [{ urls: cfg.turnUrls, username, credential }], ttl: cfg.turnTtlSeconds };
  }

  // ---- HTTP --------------------------------------------------------------------------------------

  async function readJson(req) {
    const type = req.headers["content-type"] || "";
    if (!type.startsWith("application/json")) throw new HttpError(415, "json_required");
    let size = 0;
    const chunks = [];
    for await (const chunk of req) {
      size += chunk.length;
      if (size > LIMITS.bodyBytes) throw new HttpError(413, "too_large");
      chunks.push(chunk);
    }
    try {
      const value = JSON.parse(Buffer.concat(chunks).toString("utf8") || "{}");
      if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error();
      return value;
    } catch (_) {
      throw new HttpError(400, "bad_json");
    }
  }

  function json(res, status, value) {
    res.writeHead(status, { "content-type": "application/json", "cache-control": "no-store" });
    res.end(JSON.stringify(value));
  }

  async function handle(req, res) {
    const url = new URL(req.url, "http://calls.local");
    const path = url.pathname.replace(/^\/calls(?=\/)/, "");
    try {
      if (req.method === "GET" && path === "/v1/health") {
        return json(res, 200, { ok: true, sessions: sessions.size, calls: calls.size });
      }
      if (req.method === "POST") {
        // Bearer tokens already defeat cross-site forgery; the origin check keeps it that way if a
        // cookie is ever introduced.
        const origin = req.headers.origin;
        if (origin && !cfg.allowedOrigins.includes(origin)) throw new HttpError(403, "bad_origin");
      }
      const auth = await authenticate(req);
      if (req.method === "GET" && path === "/v1/events") return openStream(req, res, auth);
      if (req.method === "GET" && path === "/v1/turn") {
        liveSession(auth, url.searchParams.get("session"));
        return json(res, 200, turnCredentials(auth));
      }
      if (req.method === "POST" && path === "/v1/calls") return json(res, 200, startCall(auth, await readJson(req)));
      const m = /^\/v1\/calls\/([A-Za-z0-9_-]{10,64})\/(accept|decline|end|signal|resume)$/.exec(path);
      if (req.method === "POST" && m) {
        const body = await readJson(req);
        const call = callFor(m[1]);
        const action = { accept, decline, end: hangUp, signal, resume }[m[2]];
        return json(res, 200, action(auth, call, body));
      }
      throw new HttpError(404, "not_found");
    } catch (err) {
      if (!(err instanceof HttpError)) {
        console.error("calls: unexpected error", err && err.name);
        err = new HttpError(500, "internal");
      }
      if (!res.headersSent) json(res, err.status, { error: err.code });
      else res.end();
    }
  }

  return { handle, state: { sessions, calls, authCache } };
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const cfg = configFromEnv();
  if (!cfg.turnSecret || !cfg.turnUrls.length) console.warn("calls: TURN is not configured; calls will be direct only");
  const service = createCallsService(cfg);
  const server = http.createServer((req, res) => service.handle(req, res));
  server.headersTimeout = 15_000;
  server.requestTimeout = 0; // event streams stay open
  server.listen(cfg.port, cfg.host, () => console.log(`calls: listening on ${cfg.host}:${cfg.port}`));
  const stop = () => server.close(() => process.exit(0));
  process.on("SIGTERM", stop);
  process.on("SIGINT", stop);
}
