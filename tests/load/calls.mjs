// calls-service load test (Phase 9). Disposable, loopback only: the real calls service
// (calls-service/src/server.js) against a real local postbox (member auth, call admission, call
// history), synthetic accounts, no media. The internal API path goes through a proxy that adds the
// round-trip time of the production SSH tunnel (--rtt-ms), because the call-event queue is sent one
// event at a time and is therefore bound by that latency.
//
//   node tests/load/calls.mjs --bin target/release/pigeonpost-postbox [--quick] [--rtt-ms 40] [--out r.json]
//
// Scenarios:
//   idle      S open signaling sessions (/v1/events): open rate, memory per session
//   calls     1:1 calls at rising setup rates: start → incoming → accept → joined; 10 signals
//             each way; then leave. Setup latency, signal relay latency, call-event queue
//   rooms     R concurrent active calls held open: memory per room, health
//   restart   SIGTERM with sessions and calls live: exit time, what is lost, reconnect storm time

import http from "node:http";
import { spawn } from "node:child_process";
import { writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import * as L from "./lib.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const args = process.argv.slice(2);
const arg = (name, dflt) => {
  const i = args.indexOf("--" + name);
  return i >= 0 ? args[i + 1] : dflt;
};
const QUICK = args.includes("--quick");
const BIN = path.resolve(arg("bin", "target/release/pigeonpost-postbox"));
const OUT = arg("out", null);
const RTT = Number(arg("rtt-ms", 40));
const PAIRS = Number(arg("pairs", QUICK ? 100 : 600));
const IDLE = Number(arg("idle-sessions", QUICK ? 300 : 3000));
const STEP = QUICK ? 5 : 10;
const results = { started: new Date().toISOString(), quick: QUICK, rtt_ms: RTT, pairs: PAIRS, scenarios: {} };

// ---- a delaying proxy for the internal API --------------------------------------------------------
function delayProxy(target, delayMs) {
  const t = new URL(target);
  const server = http.createServer((req, res) => {
    const chunks = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => setTimeout(() => {
      const up = http.request({ host: t.hostname, port: t.port, method: req.method, path: req.url, headers: req.headers, agent: L.agent }, (r) => {
        const out = [];
        r.on("data", (c) => out.push(c));
        r.on("end", () => setTimeout(() => { res.writeHead(r.statusCode, r.headers); res.end(Buffer.concat(out)); }, delayMs / 2));
      });
      up.on("error", () => { res.writeHead(502); res.end(); });
      up.end(Buffer.concat(chunks));
    }, delayMs / 2));
  });
  return new Promise((r) => server.listen(0, "127.0.0.1", () => r({ server, url: `http://127.0.0.1:${server.address().port}` })));
}

// ---- the calls service ----------------------------------------------------------------------------
async function startCalls(pb, internalUrl, port) {
  port = port || (await L.freePort());
  const proc = spawn(process.execPath, [path.join(ROOT, "calls-service/src/server.js")], {
    env: {
      PATH: process.env.PATH, CALLS_PORT: String(port), CALLS_HOST: "127.0.0.1", CALLS_POSTBOX_URL: pb.base,
      POSTBOX_INTERNAL_URL: internalUrl, POSTBOX_SERVICE_TOKEN: pb.calls, CALLS_TRUST_PROXY: "0",
      CALLS_ALLOWED_ORIGINS: "https://inbox.pigeonpost.dev",
    },
    stdio: ["ignore", "ignore", "pipe"],
  });
  let tail = "";
  proc.stderr.on("data", (d) => { tail = (tail + d).slice(-3000); });
  const base = `http://127.0.0.1:${port}`;
  for (let i = 0; i < 100; i++) {
    try { if ((await L.request("GET", base + "/v1/health")).status === 200) break; } catch (_) { /* starting */ }
    await L.sleep(100);
  }
  return { proc, base, port, pid: proc.pid, tail: () => tail };
}

// A member's signaling session: resolves once `ready` arrives with the session id. Assign `.on`
// on the result to receive later events.
function session(calls, key) {
  return new Promise((resolve) => {
    let done = false;
    const obj = {};
    const t0 = performance.now();
    const s = L.openSse(calls.base + "/v1/events", {
      token: key,
      onOpen: (code) => { if (code !== 200 && !done) { done = true; resolve({ error: code }); } },
      onClose: () => { if (!done) { done = true; resolve({ error: "closed" }); } },
      onEvent: (name, data) => {
        let d;
        try { d = JSON.parse(data); } catch (_) { return; }
        if (name === "ready" && !done) { done = true; resolve(Object.assign(obj, { id: d.session, close: s.close, ms: performance.now() - t0 })); }
        else if (obj.on) obj.on(name, d);
      },
    });
  });
}

const issuer = await L.startIssuer();
const pb = await L.startPostbox(BIN, issuer);
const proxy = await delayProxy(pb.internal, RTT);
let calls = await startCalls(pb, proxy.url);
const post = (p, key, body) => L.request("POST", calls.base + p, { token: key, body });

async function pool(n, concurrency, fn) {
  let next = 0;
  const out = new Array(n);
  await Promise.all(Array.from({ length: concurrency }, async () => { while (next < n) { const i = next++; out[i] = await fn(i); } }));
  return out;
}

async function setup() {
  const t0 = performance.now();
  const people = await pool(PAIRS * 2, 16, async (i) => {
    const token = issuer.token(`calls-${i}`);
    const r = await L.request("POST", pb.base + "/v1/identities", { token, body: { label: `c${i}` } });
    const k = await L.request("POST", pb.base + "/v1/api-keys", { token, body: { name: "load" } });
    if (r.status !== 201 || k.status !== 201) throw new Error(`account ${i}: ${r.status}/${k.status}`);
    return { i, address: r.json.address, key: k.json.api_key };
  });
  // Callees know their callers, so admission rings (an unnamed caller would be silenced).
  await pool(PAIRS, 16, async (p) => {
    const r = await L.request("PUT", pb.base + "/v1/contacts", { token: people[2 * p + 1].key, body: { peer: people[2 * p].address } });
    if (r.status !== 200) throw new Error("contact " + r.status);
  });
  L.log(`setup: ${PAIRS * 2} accounts in ${((performance.now() - t0) / 1000).toFixed(1)} s`);
  return people;
}

async function idle(people) {
  // Up to 12 sessions per account are allowed; spread S sessions over all accounts.
  const before = L.procSample(calls.pid);
  const t0 = performance.now();
  const st = new L.Stats("session open");
  const opened = await pool(IDLE, 64, async (k) => {
    const s = await session(calls, people[k % people.length].key);
    if (s.error) st.error(); else st.add(200, s.ms);
    return s;
  });
  const secs = (performance.now() - t0) / 1000;
  await L.sleep(1500);
  const after = L.procSample(calls.pid);
  const health = (await L.request("GET", calls.base + "/v1/health")).json;
  const sum = st.stop().summary([200]);
  results.scenarios.idle = {
    sessions: health.sessions, open_rate: +(IDLE / secs).toFixed(0), open_p50: sum.p50, open_p99: sum.p99, failed: sum.net_errors,
    rss_before_mb: +before.rssMb.toFixed(0), rss_after_mb: +after.rssMb.toFixed(0),
    kb_per_session: +(((after.rssMb - before.rssMb) * 1024) / Math.max(1, health.sessions)).toFixed(1),
  };
  L.log(`idle: ${health.sessions} sessions, ${results.scenarios.idle.kb_per_session} KB each, open p99 ${sum.p99} ms`);
  return opened;
}

// One 1:1 call, end to end. Returns timings or throws.
async function oneCall(people, p, holdMs, sessionsCache) {
  const A = people[2 * p];
  const B = people[2 * p + 1];
  const waiters = { incoming: null, joined: null, signal: [] };
  let incomingAt = 0;
  const onB = (name, d) => { if (name === "incoming" && waiters.incoming) waiters.incoming(d); if (name === "signal") waiters.signal.shift()?.(d); };
  const onA = (name, d) => { if (name === "participant_joined" && waiters.joined) waiters.joined(d); if (name === "signal") waiters.signal.shift()?.(d); };
  let sa = sessionsCache.get(2 * p);
  let sb = sessionsCache.get(2 * p + 1);
  if (!sa) { sa = await session(calls, A.key); sessionsCache.set(2 * p, sa); }
  if (!sb) { sb = await session(calls, B.key); sessionsCache.set(2 * p + 1, sb); }
  if (sa.error || sb.error) throw new Error("session " + (sa.error || sb.error));
  sa.on = onA;
  sb.on = onB;
  const t0 = performance.now();
  const incoming = new Promise((r) => { waiters.incoming = (d) => { incomingAt = performance.now(); r(d); }; });
  const joined = new Promise((r) => { waiters.joined = r; });
  const start = await post("/v1/calls", A.key, { session: sa.id, kind: "audio", from: A.address, to: B.address });
  if (start.status !== 200) throw new Error("start " + start.status + " " + start.text);
  const room = start.json.room;
  const ring = await Promise.race([incoming, L.sleep(10_000).then(() => null)]);
  if (!ring) throw new Error("no ring");
  const acc = await post(`/v1/rooms/${room}/accept`, B.key, { session: sb.id });
  if (acc.status !== 200) throw new Error("accept " + acc.status);
  const j = await Promise.race([joined, L.sleep(10_000).then(() => null)]);
  if (!j) throw new Error("no join");
  const setupMs = performance.now() - t0;
  const pa = start.json.you;
  const pbid = acc.json.you;
  // 10 signals each way, relay latency measured at the receiving session.
  const relay = [];
  for (let k = 0; k < 10; k++) {
    for (const [from, key, sid, to] of [[A, A.key, sa.id, pbid], [B, B.key, sb.id, pa]]) {
      const got = new Promise((r) => waiters.signal.push(r));
      const ts = performance.now();
      const body = k === 0
        ? { session: sid, to, type: from === A ? "offer" : "answer", data: { type: from === A ? "offer" : "answer", sdp: "v=0\r\n" + "a=x\r\n".repeat(400) } }
        : { session: sid, to, type: "ice", data: { candidate: `candidate:${k} 1 udp 2122260223 192.0.2.1 5${k}000 typ host`, sdpMid: "0", sdpMLineIndex: 0 } };
      const r = await post(`/v1/rooms/${room}/signal`, key, body);
      if (r.status !== 200) throw new Error("signal " + r.status + " " + r.text);
      await Promise.race([got, L.sleep(5000)]);
      relay.push(performance.now() - ts);
    }
  }
  if (holdMs) await L.sleep(holdMs);
  const lv = await post(`/v1/rooms/${room}/leave`, A.key, { session: sa.id });
  if (lv.status !== 200) throw new Error("leave " + lv.status);
  return { setupMs, ringMs: incomingAt - t0, relay };
}

async function callRamp(people) {
  const rows = [];
  const cache = new Map();
  // Sessions first, so the steps measure calls rather than first-time member authentication.
  await pool(PAIRS * 2, 32, async (i) => {
    cache.set(i, await session(calls, people[i].key));
  });
  let pairCursor = 0;
  for (const rate of QUICK ? [2, 10] : [2, 5, 10, 20, 40]) {
    const setup = new L.Stats("setup");
    const ringS = new L.Stats("ring");
    const relayS = new L.Stats("relay");
    let failures = 0;
    const errs = {};
    const before = (await L.request("GET", calls.base + "/v1/health")).json.postbox;
    const m = await L.measured(calls.pid, async () => {
      const inflight = [];
      const n = Math.round(rate * STEP);
      for (let k = 0; k < n; k++) {
        const p = pairCursor++ % PAIRS;
        inflight.push(oneCall(people, p, 0, cache).then((r) => {
          setup.add(200, r.setupMs);
          ringS.add(200, r.ringMs);
          for (const x of r.relay) relayS.add(200, x);
        }, (e) => { failures++; const key = String(e.message).split(" ").slice(0, 2).join(" "); errs[key] = (errs[key] || 0) + 1; }));
        await L.sleep(1000 / rate);
      }
      await Promise.all(inflight);
    });
    // Let the call-event queue drain and see how far behind it was.
    const at = (await L.request("GET", calls.base + "/v1/health")).json.postbox;
    let drainS = 0;
    const tq = performance.now();
    for (;;) {
      const h = (await L.request("GET", calls.base + "/v1/health")).json.postbox;
      if (!h.queued || performance.now() - tq > 120_000) break;
      await L.sleep(250);
    }
    drainS = (performance.now() - tq) / 1000;
    const after = (await L.request("GET", calls.base + "/v1/health")).json.postbox;
    const s = setup.stop().summary([200]);
    const rl = relayS.stop().summary([200]);
    const rg = ringS.stop().summary([200]);
    rows.push({
      calls_per_s: rate, calls: Math.round(rate * STEP), failed: failures, errors: errs,
      ring_p50: rg.p50, ring_p99: rg.p99, setup_p50: s.p50, setup_p99: s.p99, relay_p50: rl.p50, relay_p99: rl.p99,
      events_queued_at_end: at.queued, queue_drain_s: +drainS.toFixed(1),
      events_sent: after.sent - before.sent, events_dropped: after.dropped - before.dropped,
      admission_ok: after.admission_ok - before.admission_ok, admission_failed: after.admission_failed - before.admission_failed,
      cpu_pct: m.cpuPct, rss_mb: m.peakRssMb,
    });
    L.log(`calls ${rate}/s: setup p50 ${s.p50} p99 ${s.p99}, relay p99 ${rl.p99}, queued ${at.queued} (drain ${drainS.toFixed(1)} s), dropped ${after.dropped - before.dropped}, failed ${failures} ${JSON.stringify(errs)}`);
  }
  results.scenarios.calls = { rows };
  return cache;
}

async function heldRooms(people, cache) {
  const rows = [];
  for (const R of QUICK ? [50] : [100, 300]) {
    const r0 = L.procSample(calls.pid);
    const holds = [];
    let ok = 0;
    const t0 = performance.now();
    // Start R calls that stay up for the measurement, at 20 calls/s (staying within rate limits).
    for (let k = 0; k < R; k++) {
      holds.push(oneCall(people, k % PAIRS, 15_000, cache).then(() => ok++, () => {}));
      await L.sleep(50);
    }
    await L.sleep(Math.max(0, 6000 - (performance.now() - t0)));
    const h = (await L.request("GET", calls.base + "/v1/health")).json;
    const r1 = L.procSample(calls.pid);
    rows.push({ target_rooms: R, rooms_seen: h.rooms, sessions: h.sessions, rss_before_mb: +r0.rssMb.toFixed(0), rss_mb: +r1.rssMb.toFixed(0),
      kb_per_room: +(((r1.rssMb - r0.rssMb) * 1024) / Math.max(1, h.rooms)).toFixed(1) });
    await Promise.all(holds);
    rows.at(-1).completed = ok;
    L.log(`rooms ${R}: health rooms ${h.rooms}, rss ${r1.rssMb.toFixed(0)} MB, completed ${ok}`);
  }
  results.scenarios.rooms = { rows };
}

async function restart(people, cache) {
  // Live: some calls in progress, every cached session open.
  const live = [];
  for (let k = 0; k < Math.min(50, PAIRS); k++) live.push(oneCall(people, k, 20_000, cache).catch(() => null));
  await L.sleep(4000);
  const h = (await L.request("GET", calls.base + "/v1/health")).json;
  const t0 = performance.now();
  calls.proc.kill("SIGTERM");
  await new Promise((r) => calls.proc.once("exit", r));
  const exitMs = performance.now() - t0;
  const port = calls.port;
  await Promise.all(live);
  // A restart: every client reconnects at once (worst case — browsers back off a little).
  calls = await startCalls(pb, proxy.url, port);
  const t1 = performance.now();
  const st = new L.Stats("reconnect");
  const why = {};
  const n = Math.min(cache.size, 12 * people.length);
  const keys = [...cache.keys()];
  // 100 connects in flight: an unbounded burst mostly measures the OS listen backlog (macOS caps it
  // at kern.ipc.somaxconn = 128), and browsers reconnect with jitter anyway.
  await pool(n, 100, async (k) => {
    const i = keys[k];
    const s = await session(calls, people[i].key);
    if (s.error) { st.error(); why[s.error] = (why[s.error] || 0) + 1; } else { st.add(200, s.ms); s.close(); }
  });
  const storm = st.stop().summary([200]);
  results.scenarios.restart = {
    sessions_before: h.sessions, rooms_before: h.rooms, queued_events_before: h.postbox.queued,
    sigterm_exit_ms: +exitMs.toFixed(0), reconnects: n, reconnect_storm_s: +((performance.now() - t1) / 1000).toFixed(1),
    reconnect_p50: storm.p50, reconnect_p99: storm.p99, reconnect_failed: storm.net_errors, reconnect_failures: why,
    lost: "rooms, rings, knocks and queued call events are in memory and do not survive (by design today)",
  };
  L.log(`restart: exit ${exitMs.toFixed(0)} ms with ${h.sessions} sessions/${h.rooms} rooms; ${n} reconnects in ${results.scenarios.restart.reconnect_storm_s} s p99 ${storm.p99}, failed ${storm.net_errors} ${JSON.stringify(why)}`);
}

try {
  const people = await setup();
  if (args.includes("--restart-only")) {
    const cache = new Map();
    await pool(PAIRS * 2, 32, async (i) => cache.set(i, await session(calls, people[i].key)));
    await restart(people, cache);
  } else {
    const opened = await idle(people);
    for (const s of opened) if (s && s.close) s.close();
    await L.sleep(1000);
    const cache = await callRamp(people);
    await heldRooms(people, cache);
    await restart(people, cache);
  }
  results.finished = new Date().toISOString();
  const text = JSON.stringify(results, null, 2);
  if (OUT) writeFileSync(OUT, text);
  console.log(text);
} catch (e) {
  console.error("FAIL:", e.stack, "\ncalls stderr:\n" + calls.tail());
  process.exitCode = 1;
} finally {
  try { calls.proc.kill("SIGKILL"); } catch (_) { /* gone */ }
  proxy.server.close();
  await pb.stop();
  issuer.close();
  L.agent.destroy();
  setTimeout(() => process.exit(process.exitCode || 0), 200).unref();
}
