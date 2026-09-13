import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import http from "node:http";
import test from "node:test";

test("account overview preserves cross-store ownership, expiry and failures", async t => {
  let status = 200, payload = { handles: [
    { namespace: "apple-name", source: "apple", expires_at: 300, active: true },
    { namespace: "google-name", source: "google", expires_at: 200, active: false },
  ] };
  const requests = [];
  const upstream = http.createServer((req, res) => {
    if (req.url.startsWith("/v1/me/handles")) {
      requests.push({ url: req.url, token: req.headers.authorization });
      res.writeHead(status, { "content-type": "application/json" }); res.end(JSON.stringify(payload));
    } else { res.writeHead(503); res.end('{}'); }
  });
  upstream.listen(0, "127.0.0.1"); await once(upstream, "listening");
  t.after(() => { upstream.closeAllConnections(); upstream.close(); });
  const base = `http://127.0.0.1:${upstream.address().port}`;
  const child = spawn(process.execPath, [new URL("../src/server.js", import.meta.url).pathname], {
    env: { PATH: process.env.PATH, PORT: "0", PIGEONPOST_POSTBOX_URL: base, MASAAS_SAAS_API_URL: base },
    stdio: ["ignore", "pipe", "pipe"],
  });
  t.after(async () => { if (child.exitCode === null && child.signalCode === null) { const exit = once(child, "exit"); child.kill(); await exit; } });
  const adapter = await new Promise((resolve, reject) => {
    let output = "";
    const timer = setTimeout(() => reject(new Error("adapter startup timed out")), 5000);
    child.stdout.on("data", chunk => { output += chunk; const found = /adapter on :(\d+)/.exec(output); if (found) { clearTimeout(timer); resolve(`http://127.0.0.1:${found[1]}`); } });
    child.once("error", reject);
  });
  const get = () => fetch(adapter + "/api/v1/me/overview", { headers: { authorization: "Bearer member-fixture" } });
  const good = await get();
  assert.equal(good.status, 200);
  assert.deepEqual((await good.json()).handles, payload.handles, "billing failure does not hide canonical ownership");
  assert.deepEqual(requests[0], { url: "/v1/me/handles?include_inactive=true", token: "Bearer member-fixture" });
  for (const [failure, expected] of [[401, 401], [503, 502]]) {
    status = failure;
    const response = await get();
    assert.equal(response.status, expected);
    assert.equal((await response.json()).handles, undefined, "failure is never an empty account");
  }
  status = 200; payload = {};
  assert.equal((await get()).status, 502, "malformed ownership cannot be treated as an empty list");
  const anonymous = await fetch(adapter + "/api/v1/me/overview");
  assert.equal(anonymous.status, 401);
});
