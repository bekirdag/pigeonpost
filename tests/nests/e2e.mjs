#!/usr/bin/env node
// Nests Phase A end-to-end test against a live postbox and Nests host, with synthetic accounts.
//
//   STATE=/private/scratch/nests-e2e.json OPS_TOKEN=… INTERNAL=http://127.0.0.1:18991 \
//     node tests/nests/e2e.mjs up [N]      # N synthetic accounts, each: test inbox + one Nest
//     node tests/nests/e2e.mjs ask          # every inbox asks its Nest; waits for each reply
//     node tests/nests/e2e.mjs pause-resume # the first Nest: paused Nests do not answer; resume does
//     node tests/nests/e2e.mjs down         # delete Nests, mailboxes, keys, flag rows
//
// INTERNAL is the postbox internal API (ops token), e.g. an SSH forward to the postbox host's
// 127.0.0.1:8991. STATE holds the synthetic accounts' API keys: keep it outside the repository.
// Nothing here prints a credential.
import { readFileSync, writeFileSync } from "node:fs";
import { randomBytes } from "node:crypto";
import { pb, proofOfWork, waitFor } from "./lib.mjs";

const POSTBOX = process.env.POSTBOX || "https://postbox.pigeonpost.dev";
const INTERNAL = process.env.INTERNAL || "http://127.0.0.1:18991";
const OPS = process.env.OPS_TOKEN;
const STATE = process.env.STATE;
if (!OPS || !STATE) { console.error("OPS_TOKEN and STATE are required"); process.exit(2); }
const t0 = Date.now();
const log = (...a) => console.log(((Date.now() - t0) / 1000).toFixed(1).padStart(7), ...a);
const ops = (method, path, body) => pb(INTERNAL, method, path, OPS, body);
const load = () => JSON.parse(readFileSync(STATE, "utf8"));
const save = (s) => writeFileSync(STATE, JSON.stringify(s, null, 2), { mode: 0o600 });

async function up(n) {
  const state = { nests: [] };
  for (let i = 0; i < n; i++) {
    const acct = await pb(POSTBOX, "POST", "/v1/accounts", null, await proofOfWork(POSTBOX));
    const inbox = await pb(POSTBOX, "POST", "/v1/identities", acct.api_key, { label: `nests-e2e-inbox-${i}` });
    await ops("PUT", "/internal/v1/features/nests", { scope: "account", account_id: acct.account_id, enabled: true });
    state.nests.push({ account_id: acct.account_id, api_key: acct.api_key, inbox: inbox.address, inbox_token: inbox.capability_token });
  }
  save(state);
  log(`${n} synthetic accounts with a test inbox and the nests flag (account scope)`);
  const created = await Promise.all(state.nests.map(async (e, i) => {
    const start = Date.now();
    const r = await ops("POST", "/internal/v1/nests", { account_id: e.account_id, label: `e2e ${i}`, allow_senders: [e.inbox] });
    e.nest_id = r.nest_id; e.address = r.address; e.create_ms = Date.now() - start;
    await waitFor(async () => (await ops("GET", `/internal/v1/nests/${e.nest_id}`)).state === "ready", { timeoutMs: 120000, intervalMs: 250 });
    e.ready_ms = Date.now() - start;
    return e;
  }));
  save(state);
  for (const e of created) log(`nest ${e.nest_id} ${e.address}: create ${e.create_ms} ms, enrolled ${e.ready_ms} ms`);
  const ms = created.map((e) => e.ready_ms).sort((a, b) => a - b);
  log(`ready: median ${ms[Math.floor(ms.length / 2)]} ms, max ${ms[ms.length - 1]} ms`);
}

async function send(e, question) {
  const body = JSON.stringify({ v: 1, verb: "answer_question", args: { question } });
  await pb(POSTBOX, "POST", "/v1/send", e.inbox_token, { to: e.address, body, msg_ref: randomBytes(8).toString("hex") });
}

async function replyFrom(e, after, needle) {
  const { messages } = await pb(POSTBOX, "GET", "/v1/inbox?include_read=false", e.inbox_token);
  const m = (messages || []).find((m) => m.from === e.address && (m.seq || 0) > after && String(m.body || "").includes(needle));
  if (m) await pb(POSTBOX, "POST", "/v1/ack", e.inbox_token, { message_id: m.message_id }).catch(() => {});
  return m;
}

async function ask() {
  const state = load();
  const results = await Promise.all(state.nests.map(async (e, i) => {
    const q = `e2e question ${i} ${randomBytes(4).toString("hex")}`;
    const start = Date.now();
    await send(e, q);
    const m = await waitFor(() => replyFrom(e, 0, q), { timeoutMs: 180000, intervalMs: 500 });
    return { nest: e.nest_id, ms: Date.now() - start, reply: String(m.body).slice(0, 140) };
  }));
  for (const r of results) log(`reply from ${r.nest} in ${r.ms} ms: ${r.reply}`);
}

async function pauseResume() {
  const e = load().nests[0];
  let r = await ops("POST", `/internal/v1/nests/${e.nest_id}/pause`);
  log(`paused: postbox state ${r.state}, runtime ${r.runtime?.state}`);
  const q = `while paused ${randomBytes(4).toString("hex")}`;
  await send(e, q);
  await new Promise((res) => setTimeout(res, 20000));
  if (await replyFrom(e, 0, q)) throw new Error("a paused Nest answered");
  log("no answer while paused (20 s)");
  const start = Date.now();
  r = await ops("POST", `/internal/v1/nests/${e.nest_id}/resume`);
  log(`resumed: runtime ${r.runtime?.state}`);
  await waitFor(() => replyFrom(e, 0, q), { timeoutMs: 180000, intervalMs: 500 });
  log(`answer after resume in ${Date.now() - start} ms`);
}

async function down() {
  const state = load();
  for (const e of state.nests) {
    if (e.nest_id) {
      const r = await ops("DELETE", `/internal/v1/nests/${e.nest_id}`);
      log(`deleted ${e.nest_id}: ${r.state}`);
      await pb(POSTBOX, "DELETE", `/v1/identities?identity=${encodeURIComponent(e.address)}`, e.api_key).catch((x) => log("nest mailbox:", x.message));
    }
    await pb(POSTBOX, "DELETE", `/v1/identities?identity=${encodeURIComponent(e.inbox)}`, e.api_key).catch((x) => log("inbox:", x.message));
    await ops("PUT", "/internal/v1/features/nests", { scope: "account", account_id: e.account_id, enabled: null }).catch((x) => log("flag:", x.message));
    const { keys } = await pb(POSTBOX, "GET", "/v1/api-keys", e.api_key).catch(() => ({ keys: [] }));
    for (const k of keys || []) await pb(POSTBOX, "DELETE", `/v1/api-keys/${encodeURIComponent(k.id)}`, e.api_key).catch(() => {});
  }
  log("synthetic accounts cleaned up");
}

const [cmd, arg] = process.argv.slice(2);
const run = { up: () => up(Number(arg || 1)), ask, "pause-resume": pauseResume, down }[cmd];
if (!run) { console.error("usage: e2e.mjs up [N] | ask | pause-resume | down"); process.exit(2); }
run().catch((e) => { console.error("FAILED:", e.message); process.exit(1); });
