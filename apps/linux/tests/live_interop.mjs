// Linux ↔ web group interop against a real, locally built postbox (Phase 3b).
//
//   POSTBOX_BIN=…/pigeonpost-postbox node apps/linux/tests/live_interop.mjs
//
// Starts a throwaway OIDC issuer (RSA key + JWKS) so the postbox accepts real member JWTs, the
// postbox with a temp database and the `groups` and `disappearing` flags on, then drives two
// members: Alice through the web inbox's own client (site-inbox/groups-core.js + the vendored WASM
// library, as in the browser) and Bob through the Linux app's code (apps/linux/pigeonpost:
// groups_core, groupstore, api.Postbox) in a Python process with the UniFFI binding. Every step
// goes through the real /v1 routes, so both clients are checked against the server and each other.
import { spawn } from "node:child_process";
import http from "node:http";
import crypto from "node:crypto";
import { mkdtempSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import vm from "node:vm";
import readline from "node:readline";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../..");
const LINUX = path.join(ROOT, "apps/linux");
const BIN = process.env.POSTBOX_BIN;
if (!BIN) throw new Error("set POSTBOX_BIN to a built pigeonpost-postbox");
const work = mkdtempSync(path.join(tmpdir(), "pp-linux-interop-"));
const children = [];
const servers = [];
const log = (...a) => console.log(new Date().toISOString().slice(11, 19), ...a);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
process.on("exit", () => {
  for (const c of children) { try { c.kill("SIGKILL"); } catch (_) { /* gone */ } }
  for (const s of servers) { try { s.close(); } catch (_) { /* gone */ } }
  rmSync(work, { recursive: true, force: true });
});
const listen = (server) => new Promise((resolve) => server.listen(0, "127.0.0.1", () => { servers.push(server); resolve(server.address().port); }));
async function until(fn, what, ms = 30000) {
  const end = Date.now() + ms;
  let last;
  while (Date.now() < end) {
    try { last = await fn(); if (last) return last; } catch (e) { last = e; }
    await sleep(200);
  }
  throw new Error("timed out: " + what + (last instanceof Error ? " (" + last.message + ")" : ""));
}

// ---- OIDC issuer -----------------------------------------------------------------------------
const { publicKey, privateKey } = crypto.generateKeyPairSync("rsa", { modulusLength: 2048 });
const jwk = publicKey.export({ format: "jwk" });
const oidc = http.createServer((req, res) => {
  if (req.url.endsWith("/protocol/openid-connect/certs")) {
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ keys: [{ kty: "RSA", kid: "k1", alg: "RS256", use: "sig", n: jwk.n, e: jwk.e }] }));
  } else { res.writeHead(404); res.end(); }
});
const ISSUER = `http://127.0.0.1:${await listen(oidc)}/realms/e2e`;
const b64u = (b) => Buffer.from(b).toString("base64url");
function jwt(sub, name, azp) {
  const now = Math.floor(Date.now() / 1000);
  const head = b64u(JSON.stringify({ alg: "RS256", typ: "JWT", kid: "k1" }));
  const body = b64u(JSON.stringify({ iss: ISSUER, sub, exp: now + 3600, iat: now, auth_time: now, azp, typ: "Bearer", preferred_username: name, aud: ["account"] }));
  return head + "." + body + "." + b64u(crypto.sign("RSA-SHA256", Buffer.from(head + "." + body), privateKey));
}

// ---- postbox ---------------------------------------------------------------------------------
const free = async () => { const s = http.createServer(); const p = await listen(s); s.close(); servers.splice(servers.indexOf(s), 1); return p; };
const port = await free(), internal = await free();
const OPS = "svc_ops_" + crypto.randomBytes(20).toString("hex");
writeFileSync(path.join(work, "kms"), crypto.randomBytes(32));
const postbox = spawn(BIN, [], {
  env: { ...process.env, POSTBOX_BIND: `127.0.0.1:${port}`, POSTBOX_PUBLIC_URL: `http://127.0.0.1:${port}`,
    POSTBOX_DB_PATH: path.join(work, "postbox.db"), POSTBOX_KMS: "sealed-file:" + path.join(work, "kms"), OIDC_ISSUER: ISSUER,
    POSTBOX_INTERNAL_BIND: `127.0.0.1:${internal}`, POSTBOX_SVC_TOKEN_OPS: OPS, PIGEONPOST_BLOB_DIR: path.join(work, "blobs"),
    PIGEONPOST_BLOB_MIN_FREE_MB: "1", RUST_LOG: process.env.RUST_LOG || "warn" },
  stdio: ["ignore", "inherit", "inherit"],
});
children.push(postbox);
const PB = `http://127.0.0.1:${port}`;
await until(() => fetch(PB + "/health").then((r) => r.ok), "postbox health");
for (const name of ["groups", "disappearing", "message_filtering", "call_admission", "call_history", "avatars"]) {
  const r = await fetch(`http://127.0.0.1:${internal}/internal/v1/features/${name}`, { method: "PUT", headers: { authorization: "Bearer " + OPS, "content-type": "application/json" }, body: JSON.stringify({ scope: "global", enabled: true }) });
  assert.ok(r.ok, `flag ${name}: ${r.status}`);
}
log("postbox up", PB);

async function rest(token, method, p, body, identity, extra = {}) {
  const headers = { authorization: "Bearer " + token, ...(identity ? { "x-pigeonpost-identity": identity } : {}), ...extra };
  let payload;
  if (body instanceof Uint8Array) { headers["content-type"] = "application/octet-stream"; payload = body; }
  else if (body !== undefined) { headers["content-type"] = "application/json"; payload = JSON.stringify(body); }
  const r = await fetch(PB + p, { method, headers, body: payload });
  if (extra.raw && r.ok) return new Uint8Array(await r.arrayBuffer());
  const text = await r.text();
  let json = {}; try { json = text ? JSON.parse(text) : {}; } catch (_) { /* not json */ }
  if (!r.ok) { const e = new Error(json.error || "http_" + r.status); e.status = r.status; e.code = json.error; e.body = json; throw e; }
  return json;
}

const people = {};
for (const [key, name, azp] of [["alice", "Alice", "pigeonpost-web"], ["bob", "Bob", "pigeonpost-linux"]]) {
  const token = jwt("interop-" + key, name, azp);
  const made = await rest(token, "POST", "/v1/identities", { label: name });
  people[key] = { token, address: made.address };
}
log("mailboxes", people.alice.address, people.bob.address);

// ---- Alice: the web inbox's client with the vendored WASM library ------------------------------
const SITE = path.join(ROOT, "site-inbox");
vm.runInThisContext(readFileSync(path.join(SITE, "groups-core.js"), "utf8"));
const Core = globalThis.PigeonpostGroupsCore;
const { loadGroupCrypto } = await import(path.join(SITE, "vendor/groupcrypto/loader.js"));
const gc = await loadGroupCrypto({ wasmBytes: await readFile(path.join(SITE, "vendor/groupcrypto/pigeonpost_groupcrypto_bg.wasm")) });
const aliceApi = async (p, o = {}) => {
  try {
    const extra = { ...(o.device ? { "x-pigeonpost-device": o.device } : {}), ...(o.raw ? { raw: true } : {}) };
    const out = await rest(people.alice.token, o.method || "GET", p, o.rawBody ? new Uint8Array(o.rawBody) : o.body, people.alice.address, extra);
    return o.raw ? out.buffer : out;
  } catch (e) { throw new Core.GroupApiError(e.status || 0, e.code, e.body); }
};
const alice = Core.createClient({ gc, api: aliceApi, mailbox: people.alice.address, sleep: (ms) => sleep(Math.min(ms, 50)), jitter: () => 0 });
await alice.init();
await alice.topUpKeyPackages();
const aliceText = (gid) => alice.history(gid).filter((x) => x.kind === "message" && !x.deleted).map((x) => x.text);

// ---- Bob: the Linux app's code in Python ------------------------------------------------------
const python = spawn(process.env.PYTHON || "python3", ["-I", path.join(LINUX, "tests/live_linux_device.py"), PB, people.bob.token, people.bob.address],
  { stdio: ["pipe", "pipe", "inherit"], cwd: LINUX });
children.push(python);
const lines = readline.createInterface({ input: python.stdout });
const replies = [];
let waiter = null;
lines.on("line", (line) => { replies.push(JSON.parse(line)); if (waiter) { waiter(); waiter = null; } });
async function bob(op, args = {}) {
  python.stdin.write(JSON.stringify({ op, ...args }) + "\n");
  while (!replies.length) await new Promise((r) => { waiter = r; });
  const reply = replies.shift();
  if (!reply.ok) throw new Error(`linux ${op}: ${reply.code} ${reply.message}`);
  return reply;
}
const bobText = async (gid) => (await bob("history", { group: gid })).items.filter((x) => x.kind === "message" && !x.deleted).map((x) => x.text);

const info = await bob("init");
assert.match(info.device_id, /^linux-[a-z2-7]{12}$/);
log("linux device", info.device_id);

// 1. The web creates a group and invites Linux; Linux accepts; the web's device adds it.
const created = await alice.createGroup({ name: "Interop", invite: [people.bob.address] });
const g1 = created.group.id;
const listed = (await bob("rest", { method: "GET", path: "/v1/groups?state=invited" })).body.groups;
assert.ok(listed.some((g) => g.id === g1), "Linux sees the invitation");
await bob("rest", { method: "POST", path: `/v1/groups/${g1}/invitation`, body: { accept: true } });
await alice.processPending(g1, { noJitter: true });
assert.deepEqual((await bob("welcomes")).joined, [g1]);
log("linux joined the web's group", g1);

// 2. Messages both ways, and an attachment from Linux.
await alice.send(g1, { text: "hello linux" });
await bob("sync", { group: g1 });
assert.deepEqual(await bobText(g1), ["hello linux"]);
const fileBytes = crypto.randomBytes(4096);
await bob("send", { group: g1, text: "hello web", files: [{ name: "notes.bin", mime: "application/octet-stream", b64: fileBytes.toString("base64") }] });
await alice.sync(g1);
assert.deepEqual(aliceText(g1), ["hello linux", "hello web"]);
const sent = alice.history(g1).find((x) => x.text === "hello web");
assert.equal(sent.files[0].name, "notes.bin");
assert.deepEqual(Buffer.from(await alice.openFile(g1, sent.files[0])), fileBytes);
const s1 = await bob("status", { group: g1 });
assert.equal(s1.safety_number, alice.safetyNumber(g1), "same device list on both");
assert.equal(s1.epoch, alice.epoch(g1));
log("chat and attachment OK; safety numbers match");

// 3. Linux creates a group and invites the web; the web accepts; Linux's device adds it.
const g2 = (await bob("create", { name: "From Linux", invite: [people.alice.address] })).group;
await rest(people.alice.token, "POST", `/v1/groups/${g2}/invitation`, { accept: true }, people.alice.address);
await bob("pending", { group: g2 });
assert.deepEqual(await alice.processWelcomes(), [g2]);
await bob("send", { group: g2, text: "made on linux" });
await alice.sync(g2);
assert.deepEqual(aliceText(g2), ["made on linux"]);
await alice.send(g2, { text: "read on linux" });
await bob("sync", { group: g2 });
assert.deepEqual(await bobText(g2), ["made on linux", "read on linux"]);
assert.equal((await bob("status", { group: g2 })).safety_number, alice.safetyNumber(g2));
log("Linux-created group OK");

// 4. Disappearing timer set from Linux; the web's next message carries an expiry.
await bob("set_timer", { group: g2, secs: 300 });
const timed = await alice.send(g2, { text: "brief" });
assert.ok(timed.expires_at > Date.now() / 1000, "expires_at set by the postbox");
await bob("sync", { group: g2 });
const brief = (await bob("history", { group: g2 })).items.find((x) => x.text === "brief");
assert.equal(brief.expires_at, timed.expires_at);
log("timer OK");

// 5. Linux restarts over its encrypted store: same device, history intact, still decrypts.
const restarted = await bob("restart");
assert.equal(restarted.device_id, info.device_id);
await alice.send(g1, { text: "after restart" });
await bob("sync", { group: g1 });
assert.deepEqual(await bobText(g1), ["hello linux", "hello web", "after restart"]);
log("restart OK");

// 6. A second web device for Alice: Linux sees it in a verified commit (device-change notice).
const alice2 = Core.createClient({ gc, api: aliceApi, mailbox: people.alice.address, sleep: (ms) => sleep(Math.min(ms, 50)), jitter: () => 0 });
await alice2.init();
await alice2.topUpKeyPackages();
await bob("pending", { group: g1 });
assert.deepEqual(await alice2.processWelcomes(), [g1]);
await bob("sync", { group: g1 });
const commits = (await bob("history", { group: g1 })).items.filter((x) => x.kind === "commit");
assert.ok(commits.some((c) => c.added.some((d) => d.mailbox === people.alice.address && d.device_id === alice2.deviceId())), "Linux saw the new device");
await alice.sync(g1);
assert.equal((await bob("status", { group: g1 })).safety_number, alice.safetyNumber(g1));
log("device-change visibility OK");

// 7. Linux signs out: its device is revoked; the web commits the removal; Linux's state is gone.
await bob("revoke");
await alice.processPending(g1, { noJitter: true });
const crypto1 = await rest(people.alice.token, "GET", `/v1/groups/${g1}/crypto`, undefined, people.alice.address, { "x-pigeonpost-device": alice.deviceId() });
assert.ok(!crypto1.pending.length, "the removal was committed");
assert.equal(alice.members(g1).filter((m) => m.mailbox === people.bob.address).length, 0);
log("revocation OK");

// ---- Phase 1b routes exactly as the Linux window calls them ------------------------------------
const call = async (method, p, extra = {}) => (await bob("call", { method, path: p, ...extra })).body;
const policy = await call("GET", "/v1/policy");
assert.equal(policy.unknown_calls, "ring");
assert.equal((await call("PUT", "/v1/policy", { data: { unknown_messages: "filter" } })).unknown_messages, "filter");
assert.equal((await call("PUT", "/v1/policy", { data: { call_history_days: 30 } })).call_history_days, 30);
await call("PUT", "/v1/contacts", { data: { peer: "/spamco/*", alias: null, admission: "block", autonomy: "review", allowed_verbs: [] } });
assert.ok((await call("GET", "/v1/contacts", { query: { admission: "block" } })).contacts.some((c) => c.peer === "/spamco/*"));
await call("DELETE", "/v1/contacts", { query: { peer: "/spamco/*" } });
assert.ok(!(await call("GET", "/v1/contacts", { query: { admission: "block" } })).contacts.some((c) => c.peer === "/spamco/*"));
const log0 = await call("GET", "/v1/calls/log", { query: { filter: "all", limit: "50" } });
assert.deepEqual([log0.entries, log0.unseen_missed], [[], 0]);
// A stranger's message lands in Filtered; Accept moves it into the inbox.
const carol = { token: jwt("interop-carol", "Carol", "pigeonpost-web") };
carol.address = (await rest(carol.token, "POST", "/v1/identities", { label: "Carol" })).address;
await rest(carol.token, "POST", "/v1/send", { from: carol.address, to: people.bob.address, body: "hi from a stranger" }, carol.address);
const filtered = (await call("GET", "/v1/inbox", { query: { filtered: "1", include_read: "true" } })).messages;
assert.ok(filtered.some((m) => m.body === "hi from a stranger"), "filtered by unknown_messages=filter");
const inboxBefore = (await call("GET", "/v1/inbox", { query: { include_read: "true" } })).messages;
assert.ok(!inboxBefore.some((m) => m.body === "hi from a stranger"));
assert.equal((await call("POST", "/v1/filtered/accept", { data: { peer: carol.address } })).accepted, true);
assert.ok((await call("GET", "/v1/inbox", { query: { include_read: "true" } })).messages.some((m) => m.body === "hi from a stranger"));
// One-to-one timer on the thread with Carol.
const thread = (await call("GET", "/v1/threads")).threads.find((t) => t.peer === carol.address || t.is_default);
await call("PATCH", "/v1/threads/" + encodeURIComponent(thread.thread_id), { data: { ephemeral_secs: 3600 } });
assert.equal((await call("GET", "/v1/threads")).threads.find((t) => t.thread_id === thread.thread_id).ephemeral_secs, 3600);
// Profile picture: the cropped JPEG the window uploads, then its 256 px JPEG variant.
const uploaded = (await bob("avatar_upload")).body.avatar;
assert.equal(uploaded.visibility, "everyone");
const own = (await call("GET", "/v1/profile/avatar")).avatar;
assert.ok(own.urls.jpeg.startsWith("/v1/avatar-blobs/"));
const jpeg = Buffer.from(await call("GET", own.urls.jpeg, { binary: true }), "base64");
assert.equal(jpeg[0], 0xff); assert.equal(jpeg[1], 0xd8);
const seen = await rest(people.alice.token, "GET", "/v1/avatars?addresses=" + encodeURIComponent(people.bob.address));
assert.equal(seen.avatars[people.bob.address].hash, own.hash, "an everyone picture is visible to others");
log("Phase 1b routes OK");

python.stdin.write(JSON.stringify({ op: "quit" }) + "\n");
console.log("LINUX INTEROP OK");
process.exit(0);
