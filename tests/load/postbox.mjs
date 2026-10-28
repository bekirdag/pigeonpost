// Postbox load test (Phase 9). Disposable, loopback only: a release postbox binary on a fresh
// temporary database (WAL, as production), a synthetic OIDC issuer, synthetic accounts. Never
// point this at a deployed postbox.
//
//   cargo build --release -p pigeonpost-postbox
//   node tests/load/postbox.mjs --bin target/release/pigeonpost-postbox [--quick] [--out results.json]
//   node tests/load/postbox.mjs --bin … --only org      # just the org API scenario (re-measure)
//
// Scenarios (see tests/load/README.md for the load model):
//   setup    synthetic accounts, one mailbox + API key each, ring of contacts (i → i+1)
//   ramp     1:1 send, closed loop at rising concurrency — throughput and latency knee
//   inbox    GET /v1/inbox at rising concurrency once inboxes hold mail
//   events   M open /v1/events streams; open-loop sends + inbox polls at 10× launch peak;
//            delivery latency send → `mail` frame on every stream of the recipient; then a send-rate ramp
//   org      org API keys at their 600/min limit (sustained) and over it (burst): latency, 429 accuracy
//   flood    unauthenticated junk at high concurrency while a member sends at the target rate

import { DatabaseSync } from "node:sqlite";
import { writeFileSync } from "node:fs";
import path from "node:path";
import * as L from "./lib.mjs";

const args = process.argv.slice(2);
const arg = (name, dflt) => {
  const i = args.indexOf("--" + name);
  return i >= 0 ? args[i + 1] : dflt;
};
const QUICK = args.includes("--quick");
const BIN = path.resolve(arg("bin", "target/release/pigeonpost-postbox"));
const ONLY = arg("only", null);
const OUT = arg("out", null);
const ACCOUNTS = Number(arg("accounts", QUICK ? 120 : 500));
const STREAMS_PER_ACCOUNT = Number(arg("streams-per-account", QUICK ? 2 : 4));
const STEP = QUICK ? 4 : 10; // seconds per ramp step
const SUSTAIN = QUICK ? 10 : 60; // seconds of the 10× mixed run
// The load model (README): launch = 1,000 active accounts, 20k messages/day, busy-minute peak
// ≈ 2.5 sends/s and ≈ 33 inbox polls/s; the test target is 10× that.
const TARGET_SEND_RPS = Number(arg("send-rps", 25));
const TARGET_POLL_RPS = Number(arg("poll-rps", 330));
// Rows of already-read history to put in the messages table before measuring. Mailbox queries
// cost depends on the table size, so an empty database flatters every number. 300k rows ≈ one
// week of the 10× load model (two copies per message).
const SEED_ROWS = Number(arg("seed-rows", QUICK ? 20000 : 300000));
// Experiment: add the expression index the mailbox queries need (COALESCE(owner, recipient)) before
// measuring, to compare against the shipped schema without rebuilding the binary.
const MAILBOX_INDEX = args.includes("--mailbox-index");

const results = { started: new Date().toISOString(), quick: QUICK, accounts: ACCOUNTS, mailbox_index: MAILBOX_INDEX, scenarios: {} };
const want = (name) => !ONLY || ONLY.split(",").includes(name);
const body = (i) => `load message ${i} ` + "x".repeat(280);

const issuer = await L.startIssuer();
const pb = await L.startPostbox(BIN, issuer);
const pid = pb.pid;
const P = (p) => pb.base + p;
const flag = (name) => L.request("PUT", pb.internal + `/internal/v1/features/${name}`, { token: pb.ops, body: { scope: "global", enabled: true } });

function messageCount() {
  const db = new DatabaseSync(path.join(pb.work, "postbox.db"), { readOnly: true });
  try { return db.prepare("SELECT count(*) AS n FROM messages").get().n; } finally { db.close(); }
}

async function pool(n, concurrency, fn) {
  let next = 0;
  const out = new Array(n);
  await Promise.all(Array.from({ length: concurrency }, async () => {
    while (next < n) {
      const i = next++;
      out[i] = await fn(i);
    }
  }));
  return out;
}

async function setup() {
  const t0 = performance.now();
  const people = await pool(ACCOUNTS, 16, async (i) => {
    const token = issuer.token(`load-${i}`);
    const r = await L.request("POST", P("/v1/identities"), { token, body: { label: `load ${i}` } });
    if (r.status !== 201) throw new Error(`identity ${i}: ${r.status} ${r.text}`);
    const k = await L.request("POST", P("/v1/api-keys"), { token, body: { name: "load" } });
    if (k.status !== 201) throw new Error(`api key ${i}: ${k.status} ${k.text}`);
    return { i, token, address: r.json.address, key: k.json.api_key };
  });
  await pool(ACCOUNTS, 16, async (i) => {
    const me = people[(i + 1) % ACCOUNTS];
    const r = await L.request("PUT", P("/v1/contacts"), { token: me.key, body: { peer: people[i].address } });
    if (r.status !== 200) throw new Error(`contact ${i}: ${r.status} ${r.text}`);
  });
  const seconds = (performance.now() - t0) / 1000;
  results.scenarios.setup = { accounts: ACCOUNTS, seconds: +seconds.toFixed(1), db_bytes: pb.dbBytes() };
  L.log(`setup: ${ACCOUNTS} accounts in ${seconds.toFixed(1)} s`);
  return people;
}

const send = (people, i) => {
  const from = people[i % ACCOUNTS];
  const to = people[(from.i + 1) % ACCOUNTS];
  return L.request("POST", P("/v1/send"), { token: from.key, body: { to: to.address, body: body(i) } });
};

// Copies existing rows (fresh ids, marked read) until the table holds `SEED_ROWS`, through a
// second SQLite connection while the postbox runs (WAL + busy timeout, as the reaper does).
async function seedHistory(people) {
  await pool(ACCOUNTS, 16, (i) => send(people, i));
  const t0 = performance.now();
  const db = new DatabaseSync(path.join(pb.work, "postbox.db"));
  try {
    db.exec("PRAGMA busy_timeout = 5000");
    const cols = db.prepare("PRAGMA table_info(messages)").all().map((c) => c.name);
    const select = cols.map((c) => (c === "id" ? "lower(hex(randomblob(32)))" : c === "read" ? "1" : c)).join(", ");
    if (MAILBOX_INDEX) db.exec("CREATE INDEX IF NOT EXISTS load_messages_by_mailbox ON messages(COALESCE(owner, recipient), created_at)");
    let n = db.prepare("SELECT count(*) AS n FROM messages").get().n;
    while (n < SEED_ROWS) {
      const take = Math.min(n, SEED_ROWS - n);
      db.exec(`INSERT INTO messages (${cols.join(", ")}) SELECT ${select} FROM messages LIMIT ${take}`);
      n += take;
    }
  } finally {
    db.close();
  }
  const rows = messageCount();
  results.scenarios.seed = { rows, db_bytes: pb.dbBytes(), seconds: +((performance.now() - t0) / 1000).toFixed(1) };
  L.log(`seed: ${rows} message rows, ${(pb.dbBytes() / 1048576).toFixed(0)} MB`);
}

async function ramp(people) {
  const rows = [];
  const levels = QUICK ? [1, 8, 64] : [1, 4, 16, 64, 256];
  for (const c of levels) {
    const before = pb.dbBytes();
    const m = await L.measured(pid, () => L.closedLoop(`send c=${c}`, c, STEP, (w, n) => send(people, Math.floor(Math.random() * 1e9))));
    const s = m.result.summary([201]);
    rows.push({ concurrency: c, ...s, cpu_pct: m.cpuPct, rss_mb: m.peakRssMb, db_growth_kb: Math.round((pb.dbBytes() - before) / 1024) });
    L.log(`ramp c=${c}: ${s.ok_rps}/s p50 ${s.p50} p99 ${s.p99} cpu ${m.cpuPct}%`);
  }
  const n = messageCount();
  results.scenarios.ramp = { rows, messages_after: n, db_bytes_after: pb.dbBytes(), bytes_per_message: Math.round(pb.dbBytes() / Math.max(1, n)) };
}

async function inbox(people) {
  const rows = [];
  for (const c of QUICK ? [8, 64] : [4, 16, 64, 256]) {
    const m = await L.measured(pid, () => L.closedLoop(`inbox c=${c}`, c, STEP, () => {
      const p = people[Math.floor(Math.random() * ACCOUNTS)];
      return L.request("GET", P("/v1/inbox?include_read=true"), { token: p.key });
    }));
    const s = m.result.summary([200]);
    rows.push({ concurrency: c, ...s, cpu_pct: m.cpuPct, rss_mb: m.peakRssMb });
    L.log(`inbox c=${c}: ${s.ok_rps}/s p50 ${s.p50} p99 ${s.p99}`);
  }
  const msgs = messageCount();
  results.scenarios.inbox = { rows, avg_messages_per_mailbox: Math.round(msgs / ACCOUNTS) };
}

// Delivery latency: send start → the `mail` frame carrying that message id, on every stream the
// recipient's account holds.
// Clients that poll acknowledge what they read; without this every poll in the mixed run would
// re-open all mail the ramp left unread, which no real client does.
function markAllRead() {
  const db = new DatabaseSync(path.join(pb.work, "postbox.db"));
  try {
    db.exec("PRAGMA busy_timeout = 5000");
    db.exec("UPDATE messages SET read = 1 WHERE read = 0");
  } finally {
    db.close();
  }
}

async function events(people) {
  markAllRead();
  const waiting = new Map(); // message_id -> { t0, need, got }
  const early = new Map(); // message_id -> [frame arrival times] seen before the send answered
  let cur = { first: new L.Stats("delivery first"), all: new L.Stats("delivery all") };
  let opened = 0;
  let refused = 0;
  const streams = [];
  const record = (w, at) => {
    const t = at - w.t0;
    w.got++;
    if (w.got === 1) cur.first.add(200, t);
    if (w.got === w.need) cur.all.add(200, t);
  };
  const openStreams = people.flatMap((p) => Array.from({ length: STREAMS_PER_ACCOUNT }, () => p));
  const tOpen = performance.now();
  await pool(openStreams.length, 64, (k) => new Promise((resolve) => {
    const p = openStreams[k];
    const s = L.openSse(P("/v1/events"), {
      token: p.key,
      onOpen: (code) => { if (code === 200) opened++; else refused++; resolve(); },
      onClose: () => resolve(),
      onEvent: (name, data) => {
        if (name !== "mail") return;
        let id;
        try { id = JSON.parse(data).message_id; } catch (_) { return; }
        const at = performance.now();
        const w = waiting.get(id);
        if (w) { record(w, at); if (w.got === w.need) waiting.delete(id); }
        else { if (!early.has(id)) early.set(id, []); early.get(id).push(at); }
      },
    });
    streams.push(s);
  }));
  const openSeconds = (performance.now() - tOpen) / 1000;
  L.log(`events: ${opened} streams open (${refused} refused) in ${openSeconds.toFixed(1)} s`);
  await L.sleep(1000);

  const trackedSend = async (i) => {
    const t0 = performance.now();
    const r = await send(people, i);
    if (r.status === 201 && r.json?.message_id) {
      const id = r.json.message_id;
      const w = { t0, need: STREAMS_PER_ACCOUNT, got: 0 };
      for (const at of early.get(id) || []) record(w, at);
      early.delete(id);
      if (w.got < w.need) waiting.set(id, w);
    }
    return r;
  };
  const drain = () => { const lost = waiting.size; waiting.clear(); early.clear(); return lost; };

  const mixed = await L.measured(pid, async () => {
    const [s, p] = await Promise.all([
      L.openLoop(`send ${TARGET_SEND_RPS}/s`, TARGET_SEND_RPS, SUSTAIN, (n) => trackedSend(n * 7919)),
      L.openLoop(`poll ${TARGET_POLL_RPS}/s`, TARGET_POLL_RPS, SUSTAIN, () => {
        const q = people[Math.floor(Math.random() * ACCOUNTS)];
        return L.request("GET", P("/v1/inbox"), { token: q.key });
      }),
    ]);
    await L.sleep(2000);
    return { s, p };
  });
  const mixedRow = {
    streams_open: opened, streams_refused: refused, stream_open_seconds: +openSeconds.toFixed(1),
    send: mixed.result.s.summary([201]), poll: mixed.result.p.summary([200]),
    delivery_first: cur.first.stop().summary([200]), delivery_all: cur.all.stop().summary([200]),
    undelivered_after_2s: drain(), cpu_pct: mixed.cpuPct, rss_mb: mixed.peakRssMb,
  };
  L.log(`mixed 10x: send p99 ${mixedRow.send.p99} poll p99 ${mixedRow.poll.p99} delivery p50 ${mixedRow.delivery_first.p50} p99 ${mixedRow.delivery_first.p99} (all ${mixedRow.delivery_all.p99}) lost ${mixedRow.undelivered_after_2s} cpu ${mixed.cpuPct}%`);

  // Send-rate ramp with every stream still open: where does delivery latency break?
  const rampRows = [];
  for (const rate of QUICK ? [50, 200] : [50, 100, 200, 400, 800]) {
    cur = { first: new L.Stats("d1"), all: new L.Stats("dall") };
    const m = await L.measured(pid, async () => {
      const r = await L.openLoop(`send ${rate}/s`, rate, STEP, (n) => trackedSend(n * 104729 + rate));
      await L.sleep(2000);
      return r;
    });
    const lostR = drain();
    const sum = m.result.summary([201]);
    const ds = cur.first.stop().summary([200]);
    const da = cur.all.stop().summary([200]);
    rampRows.push({ offered_rps: rate, ok_rps: sum.ok_rps, send_p50: sum.p50, send_p99: sum.p99, codes: sum.codes, dropped: m.result.dropped,
      delivery_p50: ds.p50, delivery_p99: ds.p99, delivery_all_p99: da.p99, undelivered: lostR, cpu_pct: m.cpuPct, rss_mb: m.peakRssMb });
    L.log(`events ramp ${rate}/s: ok ${sum.ok_rps}/s send p99 ${sum.p99} delivery p99 ${ds.p99} all ${da.p99} lost ${lostR} cpu ${m.cpuPct}%`);
  }
  for (const s of streams) s.close();
  results.scenarios.events = { streams_per_account: STREAMS_PER_ACCOUNT, mixed: mixedRow, ramp: rampRows };
}

async function orgSetup(people, orgCount, keysPerOrg, tag) {
  const keys = [];
  const db = new DatabaseSync(path.join(pb.work, "postbox.db"), { readOnly: true });
  try {
    for (let o = 0; o < orgCount; o++) {
      const owner = people[(o * 37 + (tag === "burst" ? 11 : 0)) % ACCOUNTS];
      const account = db.prepare("SELECT account_id FROM identities WHERE address = ?").get(owner.address).account_id;
      const run = `${tag}${o}${Date.now().toString(36)}`;
      let r = await L.request("POST", pb.internal + "/internal/v1/org-drafts", { token: pb.billing, body: {
        operation_id: `op-${run}-0123456789abcdef`, owner_account: account, name: `Load ${run}`,
        namespace: `load${run}`.slice(0, 30), plan_code: "ent-5-monthly-usd", seats: 5 } });
      if (r.status !== 201) throw new Error("org draft " + r.status + r.text);
      const org = r.json.org.id;
      const paid = Math.floor(Date.now() / 1000) + 30 * 86400;
      r = await L.request("PUT", pb.internal + `/internal/v1/orgs/${org}/activate`, { token: pb.billing, body: {
        operation_id: `act-${run}-0123456789`, entitlement: { provider: "grant", environment: "test", reference: `ref-${run}`,
          plan_code: "ent-5-monthly-usd", plan_version: "2026", seats: 5, status: "active", period_end: paid, paid_through: paid, reconciliation_version: 1 } } });
      if (r.status !== 200) throw new Error("org activate " + r.status + r.text);
      const strong = issuer.token(`load-${owner.i}`, true);
      for (let k = 0; k < keysPerOrg; k++) {
        r = await L.request("POST", P(`/v1/orgs/${org}/api-keys`), { token: strong, body: { name: `load ${k}`, scopes: ["org:read"], expires_in_days: 1 } });
        if (r.status !== 201) throw new Error("org key " + r.status + r.text);
        keys.push(r.json.secret);
      }
    }
  } finally {
    db.close();
  }
  return keys;
}

async function org(people) {
  await flag("orgs");
  await flag("org_api");
  const sustainedKeys = await orgSetup(people, 4, 5, "sus");
  const burstKeys = await orgSetup(people, 4, 5, "burst");
  // Sustained: every key at 9 requests/s (540/min, under the 600/min limit), 20 keys at once.
  const perKey = 9;
  const sus = await L.measured(pid, () => L.openLoop(`org sustained ${perKey * sustainedKeys.length}/s`, perKey * sustainedKeys.length, QUICK ? 10 : 30,
    (n) => L.request("GET", P("/org/v1/org"), { token: sustainedKeys[n % sustainedKeys.length] })));
  const susSum = sus.result.summary([200]);
  L.log(`org sustained: ${susSum.ok_rps}/s p50 ${susSum.p50} p99 ${susSum.p99} codes ${JSON.stringify(susSum.codes)}`);
  // Burst: 700 requests per key as fast as 8 workers per key go. Exactly 600 should pass.
  const perKeyOk = [];
  const burst = await L.measured(pid, async () => {
    const st = new L.Stats("org burst");
    await Promise.all(burstKeys.map(async (key) => {
      let ok = 0;
      let sent = 0;
      await Promise.all(Array.from({ length: 8 }, async () => {
        while (sent < 700) {
          sent++;
          try {
            const r = await L.request("GET", P("/org/v1/org"), { token: key });
            st.add(r.status, r.ms);
            if (r.status === 200) ok++;
          } catch (_) { st.error(); }
        }
      }));
      perKeyOk.push(ok);
    }));
    return st.stop();
  });
  const bSum = burst.result.summary([200]);
  L.log(`org burst: ${bSum.rps}/s p99 ${bSum.p99} ok per key min ${Math.min(...perKeyOk)} max ${Math.max(...perKeyOk)}`);
  results.scenarios.org = {
    sustained: { keys: sustainedKeys.length, per_key_rps: perKey, ...susSum, cpu_pct: sus.cpuPct, rss_mb: sus.peakRssMb },
    burst: { keys: burstKeys.length, requests_per_key: 700, ...bSum, ok_per_key_min: Math.min(...perKeyOk), ok_per_key_max: Math.max(...perKeyOk), cpu_pct: burst.cpuPct },
  };
}

async function flood(people) {
  const junk = [
    () => L.request("GET", P("/v1/inbox"), { token: "pk_live_" + "0".repeat(64) }),
    () => L.request("GET", P("/org/v1/org"), { token: "pk_org_" + "a".repeat(32) }),
    () => L.request("POST", P("/v1/accounts"), { body: { pow_challenge: "x", pow_solution: "y" } }),
    () => L.request("GET", P("/v1/pow/challenge")),
    () => L.request("POST", P("/v1/identities"), { body: { label: "x", pow_challenge: "x", pow_solution: "y" } }),
    () => L.request("GET", P("/v1/whoami"), { token: "eyJhbGciOiJSUzI1NiJ9.e30.x" }),
  ];
  const conc = QUICK ? 64 : 256;
  const m = await L.measured(pid, async () => {
    const [j, s] = await Promise.all([
      L.closedLoop(`junk c=${conc}`, conc, STEP * 1.5, (w, n) => junk[(w + n) % junk.length]()),
      L.openLoop(`member send ${TARGET_SEND_RPS}/s under flood`, TARGET_SEND_RPS, STEP * 1.5, (n) => send(people, n * 31)),
    ]);
    return { j, s };
  });
  const js = m.result.j.summary([200, 201]);
  const ss = m.result.s.summary([201]);
  L.log(`flood: junk ${js.rps}/s codes ${JSON.stringify(js.codes)}; member send p50 ${ss.p50} p99 ${ss.p99} codes ${JSON.stringify(ss.codes)}`);
  results.scenarios.flood = { junk: js, member_send: ss, cpu_pct: m.cpuPct, rss_mb: m.peakRssMb };
}

try {
  results.machine = { node: process.version, platform: process.platform, arch: process.arch };
  const people = await setup();
  if (SEED_ROWS > 0) await seedHistory(people);
  if (want("ramp")) await ramp(people);
  if (want("inbox")) await inbox(people);
  if (want("events")) await events(people);
  if (want("org")) await org(people);
  if (want("flood")) await flood(people);
  results.final = { messages: messageCount(), db_bytes: pb.dbBytes(), rss_mb: +L.procSample(pid).rssMb.toFixed(0) };
  results.finished = new Date().toISOString();
  const text = JSON.stringify(results, null, 2);
  if (OUT) writeFileSync(OUT, text);
  console.log(text);
} catch (e) {
  console.error("FAIL:", e.message, "\npostbox stderr tail:\n" + pb.stderrTail());
  process.exitCode = 1;
} finally {
  await pb.stop();
  issuer.close();
  L.agent.destroy();
  setTimeout(() => process.exit(process.exitCode || 0), 200).unref();
}
