// A real postbox on this computer for the Android app's MLS round trip (Phase 3b).
//
//   node tests/e2e-groups/android-postbox.mjs          # builds the postbox unless POSTBOX_BIN is set
//
// Starts a throwaway OIDC issuer and the postbox (temp database, flags `groups` and `disappearing`
// on), makes two people with one hosted mailbox each, and prints the instrumentation arguments:
//   adb reverse tcp:<port> tcp:<port>
//   ./gradlew :app:connectedDebugAndroidTest -Pandroid.testInstrumentationRunnerArguments.class=dev.pigeonpost.inbox.LocalPostboxMlsTest \
//     -Pandroid.testInstrumentationRunnerArguments.postbox=… .aliceToken=… .bobToken=… .alice=… .bob=…
// (Push the printed JSON line to /sdcard/Android/data/dev.pigeonpost.inbox.debug/files/postbox.json first.)
// Then keeps running until Ctrl-C. Same setup as tests/e2e-groups/run.mjs, without browsers.
import { spawn, execFileSync } from "node:child_process";
import http from "node:http";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const OPS = "svc_ops_" + crypto.randomBytes(20).toString("hex");
const work = mkdtempSync(path.join(tmpdir(), "pp-android-postbox-"));
const children = [];
process.on("exit", () => { for (const c of children) try { c.kill("SIGKILL"); } catch (_) {} if (!process.env.KEEP) rmSync(work, { recursive: true, force: true }); });
process.on("SIGINT", () => process.exit(130));
const listen = (server, port = 0) => new Promise((resolve) => server.listen(port, "127.0.0.1", () => resolve(server.address().port)));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const { publicKey, privateKey } = crypto.generateKeyPairSync("rsa", { modulusLength: 2048 });
const jwk = publicKey.export({ format: "jwk" });
const oidc = http.createServer((req, res) => {
  if (req.url.endsWith("/protocol/openid-connect/certs")) {
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ keys: [{ kty: "RSA", kid: "k", alg: "RS256", use: "sig", n: jwk.n, e: jwk.e }] }));
  } else { res.writeHead(404); res.end(); }
});
const ISSUER = `http://127.0.0.1:${await listen(oidc)}/realms/android`;
const b64u = (b) => Buffer.from(b).toString("base64url");
function jwt(sub, name) {
  const now = Math.floor(Date.now() / 1000);
  const head = b64u(JSON.stringify({ alg: "RS256", typ: "JWT", kid: "k" }));
  const body = b64u(JSON.stringify({ iss: ISSUER, sub, exp: now + 6 * 3600, iat: now, auth_time: now, azp: "pigeonpost-mobile", typ: "Bearer", preferred_username: name, aud: ["account"] }));
  return head + "." + body + "." + b64u(crypto.sign("RSA-SHA256", Buffer.from(head + "." + body), privateKey));
}

let bin = process.env.POSTBOX_BIN;
if (!bin) { execFileSync("cargo", ["build", "-q", "-p", "pigeonpost-postbox"], { cwd: ROOT, stdio: "inherit" }); bin = path.join(ROOT, "target/debug/pigeonpost-postbox"); }
const port = Number(process.env.PORT || 18555), intPort = port + 1;
writeFileSync(path.join(work, "kms"), crypto.randomBytes(32));
children.push(spawn(bin, [], { env: { ...process.env,
  POSTBOX_BIND: `127.0.0.1:${port}`, POSTBOX_PUBLIC_URL: `http://127.0.0.1:${port}`, POSTBOX_DB_PATH: path.join(work, "postbox.db"),
  POSTBOX_KMS: "sealed-file:" + path.join(work, "kms"), OIDC_ISSUER: ISSUER, POSTBOX_INTERNAL_BIND: `127.0.0.1:${intPort}`, POSTBOX_SVC_TOKEN_OPS: OPS,
  PIGEONPOST_BLOB_DIR: path.join(work, "blobs"), PIGEONPOST_BLOB_MIN_FREE_MB: "1", RUST_LOG: process.env.RUST_LOG || "warn" }, stdio: ["ignore", "inherit", "inherit"] }));
const PB = `http://127.0.0.1:${port}`;
for (let i = 0; ; i++) { try { if ((await fetch(PB + "/health")).ok) break; } catch (_) {} if (i > 120) throw new Error("postbox did not start"); await sleep(250); }
for (const name of ["groups", "disappearing"]) {
  const r = await fetch(`http://127.0.0.1:${intPort}/internal/v1/features/${name}`, { method: "PUT", headers: { authorization: "Bearer " + OPS, "content-type": "application/json" }, body: JSON.stringify({ scope: "global", enabled: true }) });
  if (!r.ok) throw new Error(`flag ${name}: ${r.status}`);
}
const people = {};
for (const [key, name] of [["alice", "Alice"], ["bob", "Bob"]]) {
  const token = jwt("android-" + key, name);
  const r = await fetch(PB + "/v1/identities", { method: "POST", headers: { authorization: "Bearer " + token, "content-type": "application/json" }, body: JSON.stringify({ label: name }) });
  if (r.status !== 201) throw new Error(`mailbox for ${name}: ${r.status} ${await r.text()}`);
  people[key] = { token, address: (await r.json()).address };
}
const args = { postbox: PB, aliceToken: people.alice.token, bobToken: people.bob.token, alice: people.alice.address, bob: people.bob.address };
console.log(JSON.stringify(args));
console.log(`adb reverse tcp:${port} tcp:${port}`);
await new Promise(() => {});
