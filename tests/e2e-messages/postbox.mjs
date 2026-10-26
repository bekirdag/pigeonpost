// Disposable, loopback-only postbox and committed-response-loss proxy. Never uses real accounts.
// node tests/e2e-messages/postbox.mjs /absolute/postbox-binary /private/setup.json
import { spawn } from "node:child_process";
import crypto from "node:crypto";
import http from "node:http";
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

process.umask(0o077);
const [bin, out, mode] = process.argv.slice(2);
if (!bin || !out || !path.isAbsolute(bin) || !path.isAbsolute(out) || (mode && !["groups", "calendar", "orgs"].includes(mode))) {
  console.error("usage: postbox.mjs /absolute/postbox-binary /private/setup.json [groups|calendar|orgs]");
  process.exit(2);
}
const work = mkdtempSync(path.join(tmpdir(), "pp-message-actions-"));
const servers = new Set();
let child, setupWritten = false;
process.on("exit", () => {
  child?.kill("SIGKILL");
  for (const server of servers) server.close();
  if (setupWritten) rmSync(out, { force: true });
  rmSync(work, { recursive: true, force: true });
});
for (const signal of ["SIGINT", "SIGTERM"]) process.on(signal, () => process.exit(0));
const listen = (server) => new Promise((resolve, reject) => {
  server.once("error", reject);
  server.listen(0, "127.0.0.1", () => { servers.add(server); resolve(server.address().port); });
});
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const { publicKey, privateKey } = crypto.generateKeyPairSync("rsa", { modulusLength: 2048 });
const jwk = publicKey.export({ format: "jwk" });
const issuerPort = await listen(http.createServer((req, res) => {
  if (req.url === "/realms/messages/protocol/openid-connect/certs") {
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ keys: [{ ...jwk, kid: "fixture", alg: "RS256", use: "sig" }] }));
  } else { res.writeHead(404); res.end(); }
}));
const issuer = `http://127.0.0.1:${issuerPort}/realms/messages`;
function token(subject, name) {
  const now = Math.floor(Date.now() / 1000);
  const header = Buffer.from(JSON.stringify({ alg: "RS256", typ: "JWT", kid: "fixture" })).toString("base64url");
  const body = Buffer.from(JSON.stringify({ iss: issuer, sub: subject, iat: now, exp: now + 3600,
    auth_time: now, azp: "pigeonpost-mobile", typ: "Bearer", preferred_username: name, aud: ["account"],
    ...(["calendar", "orgs"].includes(mode) ? { email: `${name}@calendar-fixture.invalid`, email_verified: true } : {}),
    ...(mode === "orgs" ? { acr: "fixture-mfa" } : {}) })).toString("base64url");
  return `${header}.${body}.${crypto.sign("RSA-SHA256", Buffer.from(`${header}.${body}`), privateKey).toString("base64url")}`;
}
// Reserve both sockets together so the kernel cannot assign the same port to both listeners.
const reservation = http.createServer(), internalReservation = http.createServer();
const port = await listen(reservation), internalPort = await listen(internalReservation);
await Promise.all([reservation, internalReservation].map((server) => new Promise((resolve) => {
  server.close(() => { servers.delete(server); resolve(); });
})));
const postbox = `http://127.0.0.1:${port}`, internal = `http://127.0.0.1:${internalPort}`;
const ops = `svc_ops_${crypto.randomBytes(20).toString("hex")}`;
const billing = `svc_billing_${crypto.randomBytes(20).toString("hex")}`;
writeFileSync(path.join(work, "kms"), crypto.randomBytes(32), { mode: 0o600 });
if (mode === "orgs") writeFileSync(path.join(work, "reserved-names"), "admin\nsupport\n", { mode: 0o600 });
child = spawn(bin, [], {
  // Deliberately omit inherited provider/service credentials. All state and configured origins
  // belong to this fixture; even registry fallback is a loopback 404 endpoint.
  env: { PATH: process.env.PATH || "/usr/bin:/bin", RUST_LOG: "warn",
    POSTBOX_BIND: `127.0.0.1:${port}`, POSTBOX_INTERNAL_BIND: `127.0.0.1:${internalPort}`,
    POSTBOX_PUBLIC_URL: postbox, POSTBOX_DB_PATH: path.join(work, "postbox.db"),
    POSTBOX_KMS: `sealed-file:${path.join(work, "kms")}`, POSTBOX_LOFT_DIR: path.join(work, "loft"),
    PIGEONPOST_BLOB_DIR: path.join(work, "blobs"), PIGEONPOST_BLOB_MIN_FREE_MB: "1",
    REGISTRY_URL: `http://127.0.0.1:${issuerPort}`, OIDC_ISSUER: issuer, POSTBOX_SVC_TOKEN_OPS: ops,
    ...(mode === "orgs" ? { POSTBOX_SVC_TOKEN_BILLING: billing, POSTBOX_STEP_UP_ACR_VALUES: "fixture-mfa",
      PIGEONPOST_RESERVED_NAMES: path.join(work, "reserved-names") } : {}) },
  stdio: ["ignore", "ignore", "inherit"],
});
child.once("error", () => { console.error("Could not start the fixture binary."); process.exit(1); });
for (let attempt = 0; ; attempt++) {
  if (child.exitCode !== null || child.signalCode !== null) throw new Error("Fixture postbox exited during startup.");
  try { if ((await fetch(`${postbox}/health`)).ok) break; } catch { /* starting */ }
  if (attempt >= 120) throw new Error("Fixture postbox did not start.");
  await sleep(250);
}
for (const feature of ["message_edit", "message_reply", "receipts", "agent_status", "message_filtering", "account_events", ...(["groups", "orgs"].includes(mode) ? ["groups", "disappearing"] : []), ...(mode === "calendar" ? ["calendar"] : []), ...(mode === "orgs" ? ["orgs"] : [])]) {
  const response = await fetch(`${internal}/internal/v1/features/${feature}`, { method: "PUT",
    headers: { authorization: `Bearer ${ops}`, "content-type": "application/json" },
    body: JSON.stringify({ scope: "global", enabled: true }) });
  if (!response.ok) throw new Error(`Fixture feature ${feature}: ${response.status}`);
}
const people = {}, run = crypto.randomUUID();
for (const name of ["alice", "bob", "carol"]) {
  const credential = token(`message-${run}-${name}`, name);
  const response = await fetch(`${postbox}/v1/identities`, { method: "POST",
    headers: { authorization: `Bearer ${credential}`, "content-type": "application/json" },
    body: JSON.stringify({ label: name }) });
  if (response.status !== 201) throw new Error(`Fixture mailbox ${name}: ${response.status}`);
  people[name] = { token: credential, address: (await response.json()).address };
}
let organizationId;
if (mode === "orgs") {
  // Read one account ID from this newly created fixture database, never a configured/live DB.
  // All organization state is then created through the actual internal/public HTTP contracts.
  const { DatabaseSync } = await import("node:sqlite");
  const db = new DatabaseSync(path.join(work, "postbox.db"), { readOnly: true });
  let owner;
  try { owner = db.prepare("SELECT account_id FROM identities WHERE address = ?").get(people.alice.address)?.account_id; }
  finally { db.close(); }
  if (typeof owner !== "string") throw new Error("Fixture owner account was not found.");
  const call = async (base, credential, method, route, body, status) => {
    const response = await fetch(base + route, { method,
      headers: { authorization: `Bearer ${credential}`, "content-type": "application/json" },
      body: JSON.stringify(body), redirect: "error" });
    if (response.status !== status) {
      const result = await response.json().catch(() => ({}));
      const code = typeof result.error === "string" && /^[a-z_]{1,80}$/.test(result.error) ? result.error : "unknown";
      throw new Error(`Organization fixture ${route.split("/").at(-1)}: ${response.status} ${code}`);
    }
    return response.json();
  };
  const draft = await call(internal, billing, "POST", "/internal/v1/org-drafts", {
    operation_id: crypto.randomUUID(), owner_account: owner, name: "Loopback company", namespace: "fixture",
    plan_code: "ent-5-monthly-usd", seats: 5,
  }, 201);
  organizationId = draft.org.id;
  const through = Math.floor(Date.now() / 1000) + 30 * 86400;
  await call(internal, billing, "PUT", `/internal/v1/orgs/${organizationId}/activate`, {
    operation_id: crypto.randomUUID(), entitlement: { provider: "grant", environment: "test",
      reference: `fixture-${crypto.randomUUID()}`, plan_code: "ent-5-monthly-usd", plan_version: "2026",
      seats: 5, status: "active", period_end: through, paid_through: through, reconciliation_version: 1 },
  }, 200);
  const own = await call(postbox, people.alice.token, "POST", `/v1/orgs/${organizationId}/me/mailbox`, { handle: "alice" }, 201);
  people.alice.personalAddress = people.alice.address;
  people.alice.address = own.mailbox.address;
  const invited = await call(postbox, people.alice.token, "POST", `/v1/orgs/${organizationId}/invites`, {
    invites: [{ first_name: "Fixture", last_name: "Member", contact: "bob@calendar-fixture.invalid", handle: "bob", roles: [] }],
  }, 200);
  const invitation = invited.results?.[0]?.link?.split("#")[1];
  if (!invitation) throw new Error("Synthetic organization invitation was not returned.");
  const joined = await call(postbox, people.bob.token, "POST", "/v1/org-invites/accept", { token: invitation }, 200);
  people.bob.personalAddress = people.bob.address;
  people.bob.address = joined.mailbox.address;
}
let loss = "off", attempts = [];
const lossPort = await listen(http.createServer(async (req, res) => {
  try {
    const parts = []; let count = 0;
    for await (const part of req) {
      count += part.length;
      if (count > 512 * 1024) { res.writeHead(413); res.end(); return; }
      parts.push(part);
    }
    const bytes = Buffer.concat(parts);
    if (req.url === "/__test/control" && req.method === "POST") {
      const next = JSON.parse(bytes.toString()).mode;
      if (!["off", "before", "after"].includes(next)) { res.writeHead(400); res.end(); return; }
      loss = next;
      if (next !== "off") attempts = [];
      res.writeHead(200, { "content-type": "application/json" }); res.end("{}"); return;
    }
    if (req.url === "/__test/attempts" && req.method === "GET") {
      res.writeHead(200, { "content-type": "application/json" }); res.end(JSON.stringify({ attempts })); return;
    }
    const targeted = req.method === "POST" && (req.url === "/v1/send" || /^\/v1\/groups\/[^/]+\/messages$/.test(req.url) ||
      mode === "calendar" && (req.url === "/v1/calendars" || req.url === "/v1/calendar/events" || /^\/v1\/calendar\/import\/[^/]+\/confirm$/.test(req.url)) ||
      mode === "orgs" && req.url.split("?")[0] === "/v1/groups");
    let evidence;
    if (targeted) {
      evidence = { sha: crypto.createHash("sha256").update(bytes).digest("hex"),
        msgRef: JSON.parse(bytes.toString()).msg_ref, cmid: JSON.parse(bytes.toString()).client_message_id,
        ...(["calendar", "orgs"].includes(mode) ? { requestKey: JSON.parse(bytes.toString()).request_key } : {}), status: null };
      attempts.push(evidence);
      if (loss === "before") { req.socket.destroy(); return; }
    }
    const headers = Object.fromEntries(Object.entries(req.headers).filter(([name]) => !["host", "connection", "content-length"].includes(name)));
    const response = await fetch(`${postbox}${req.url}`, { method: req.method, headers,
      ...(["GET", "HEAD"].includes(req.method) ? {} : { body: bytes }), redirect: "error" });
    const body = Buffer.from(await response.arrayBuffer());
    if (evidence) evidence.status = response.status;
    if (targeted && loss === "after") { req.socket.destroy(); return; }
    res.writeHead(response.status, { "content-type": response.headers.get("content-type") || "application/json" });
    res.end(body);
  } catch { if (!res.destroyed) { res.writeHead(502); res.end(); } }
}));
writeFileSync(out, JSON.stringify({ postbox, lossProxy: `http://127.0.0.1:${lossPort}`, people, ...(organizationId ? { organizationId } : {}) }), { mode: 0o600, flag: "wx" });
setupWritten = true;
chmodSync(out, 0o600);
console.log("Message action fixture ready (loopback only).");
