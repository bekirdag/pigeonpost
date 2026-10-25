// A real postbox for the Apple group tests: a throwaway OIDC issuer (so the postbox validates real
// member JWTs, as with the production realm), the postbox binary on a temp database with `groups`
// and `disappearing` switched on, and one member account with one mailbox for each person.
//
//   node apps/ios/Tests/groups-postbox.mjs <postbox-binary> <out.json>
//
// Writes {postbox, people: {alice: {token, address}, …}} to <out.json> once everything answers,
// then runs until it is sent SIGTERM. The same setup as tests/e2e-groups/run.mjs.
import { spawn } from "node:child_process";
import http from "node:http";
import crypto from "node:crypto";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

const [bin, out] = process.argv.slice(2);
if (!bin || !out) { console.error("usage: groups-postbox.mjs <postbox-binary> <out.json>"); process.exit(2); }
const work = mkdtempSync(path.join(tmpdir(), "pp-apple-groups-"));
const OPS = "svc_ops_" + crypto.randomBytes(20).toString("hex");
const children = [], servers = [];
function cleanup() {
  for (const c of children) { try { c.kill("SIGKILL"); } catch (_) { /* gone */ } }
  for (const s of servers) { try { s.close(); } catch (_) { /* gone */ } }
  rmSync(work, { recursive: true, force: true });
}
process.on("exit", cleanup);
for (const sig of ["SIGINT", "SIGTERM"]) process.on(sig, () => process.exit(0));

const listen = (server) => new Promise((r) => server.listen(0, "127.0.0.1", () => { servers.push(server); r(server.address().port); }));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const { publicKey, privateKey } = crypto.generateKeyPairSync("rsa", { modulusLength: 2048 });
const jwk = publicKey.export({ format: "jwk" });
const oidc = http.createServer((req, res) => {
  if (req.url.endsWith("/protocol/openid-connect/certs")) {
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ keys: [{ kty: "RSA", kid: "k1", alg: "RS256", use: "sig", n: jwk.n, e: jwk.e }] }));
  } else { res.writeHead(404); res.end(); }
});
const ISSUER = `http://127.0.0.1:${await listen(oidc)}/realms/apple`;
const b64u = (b) => Buffer.from(b).toString("base64url");
function jwt(sub, name) {
  const now = Math.floor(Date.now() / 1000);
  const head = b64u(JSON.stringify({ alg: "RS256", typ: "JWT", kid: "k1" }));
  const body = b64u(JSON.stringify({ iss: ISSUER, sub, exp: now + 6 * 3600, iat: now, auth_time: now, azp: "pigeonpost-mobile", typ: "Bearer", preferred_username: name, aud: ["account"] }));
  return head + "." + body + "." + b64u(crypto.sign("RSA-SHA256", Buffer.from(head + "." + body), privateKey));
}

const free = async () => { const s = http.createServer(); const p = await listen(s); s.close(); servers.splice(servers.indexOf(s), 1); return p; };
const pbPort = await free(), intPort = await free();
writeFileSync(path.join(work, "kms"), crypto.randomBytes(32));
const postbox = spawn(bin, [], {
  env: {
    ...process.env,
    POSTBOX_BIND: `127.0.0.1:${pbPort}`, POSTBOX_PUBLIC_URL: `http://127.0.0.1:${pbPort}`,
    POSTBOX_DB_PATH: path.join(work, "postbox.db"), POSTBOX_KMS: "sealed-file:" + path.join(work, "kms"),
    OIDC_ISSUER: ISSUER, POSTBOX_INTERNAL_BIND: `127.0.0.1:${intPort}`, POSTBOX_SVC_TOKEN_OPS: OPS,
    PIGEONPOST_BLOB_DIR: path.join(work, "blobs"), PIGEONPOST_BLOB_MIN_FREE_MB: "1", RUST_LOG: process.env.RUST_LOG || "warn",
  },
  stdio: ["ignore", "inherit", "inherit"],
});
children.push(postbox);
const PB = `http://127.0.0.1:${pbPort}`, INT = `http://127.0.0.1:${intPort}`;
for (let i = 0; ; i++) {
  try { if ((await fetch(PB + "/health")).ok) break; } catch (_) { /* starting */ }
  if (i > 120) throw new Error("postbox did not start");
  await sleep(250);
}
for (const name of ["groups", "disappearing", "account_events"]) {
  const r = await fetch(`${INT}/internal/v1/features/${name}`, { method: "PUT", headers: { authorization: "Bearer " + OPS, "content-type": "application/json" }, body: JSON.stringify({ scope: "global", enabled: true }) });
  if (!r.ok) throw new Error(`flag ${name}: ${r.status}`);
}
const people = {};
for (const [key, name] of [["alice", "Alice"], ["bob", "Bob"], ["carol", "Carol"], ["dana", "Dana"]]) {
  const token = jwt("apple-" + key, name);
  const r = await fetch(PB + "/v1/identities", { method: "POST", headers: { authorization: "Bearer " + token, "content-type": "application/json" }, body: JSON.stringify({ label: name }) });
  if (r.status !== 201) throw new Error(`mailbox for ${name}: ${r.status} ${await r.text()}`);
  people[key] = { token, address: (await r.json()).address };
}
// A loopback-only transport fault for exact-request retry tests. It can drop a POST before
// forwarding or after the real postbox committed it. Recorded evidence contains hashes/ids,
// never bearer headers or message bodies. This server exists only for this throwaway fixture.
let lossMode = "off";
let attempts = [];
const lossProxy = http.createServer(async (req, res) => {
  try {
    const chunks = [];
    for await (const chunk of req) chunks.push(chunk);
    const bytes = Buffer.concat(chunks);
    if (req.url === "/__test/control" && req.method === "POST") {
      const mode = JSON.parse(bytes.toString()).mode;
      if (!["off", "before", "after"].includes(mode)) { res.writeHead(400); res.end(); return; }
      lossMode = mode;
      if (mode !== "off") attempts = [];
      res.writeHead(200, { "content-type": "application/json" }); res.end("{}"); return;
    }
    if (req.url === "/__test/attempts" && req.method === "GET") {
      res.writeHead(200, { "content-type": "application/json" }); res.end(JSON.stringify({ attempts })); return;
    }
    const message = req.method === "POST" && /^\/v1\/groups\/[^/]+\/messages$/.test(req.url);
    let evidence;
    if (message) {
      const body = JSON.parse(bytes.toString());
      evidence = { sha: crypto.createHash("sha256").update(bytes).digest("hex"), cmid: body.client_message_id, status: null };
      attempts.push(evidence);
      if (lossMode === "before") { req.socket.destroy(); return; }
    }
    const headers = Object.fromEntries(Object.entries(req.headers).filter(([name]) => !["host", "connection", "content-length"].includes(name)));
    const upstream = await fetch(PB + req.url, { method: req.method, headers,
      ...(["GET", "HEAD"].includes(req.method) ? {} : { body: bytes }) });
    const response = Buffer.from(await upstream.arrayBuffer());
    if (evidence) evidence.status = upstream.status;
    if (message && lossMode === "after") { req.socket.destroy(); return; }
    res.writeHead(upstream.status, { "content-type": upstream.headers.get("content-type") || "application/octet-stream" });
    res.end(response);
  } catch (_) { if (!res.destroyed) { res.writeHead(502); res.end(); } }
});
const LOSS = `http://127.0.0.1:${await listen(lossProxy)}`;
writeFileSync(out, JSON.stringify({ postbox: PB, lossProxy: LOSS, people }));
console.log("groups postbox ready on", PB);
