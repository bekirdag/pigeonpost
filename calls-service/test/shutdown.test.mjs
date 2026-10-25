import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import http from "node:http";
import { fileURLToPath } from "node:url";

async function bounded(promise, milliseconds, message) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(message)), milliseconds);
    })]);
  } finally { clearTimeout(timer); }
}

for (const signal of ["SIGTERM", "SIGINT"]) {
  test(`${signal} closes live event streams and exits promptly`, { skip: process.platform === "win32", timeout: 15_000 }, async () => {
    const child = spawn(process.execPath, [fileURLToPath(new URL("../src/server.js", import.meta.url))], {
      env: { PATH: process.env.PATH, CALLS_HOST: "127.0.0.1", CALLS_PORT: "0" },
      stdio: ["ignore", "pipe", "pipe"],
    });
    const exited = new Promise((resolve) => child.once("exit", (code, killedBy) => resolve({ code, killedBy })));
    let output = "", errors = "", stream, response;
    child.stderr.on("data", (chunk) => { errors += chunk; });
    try {
      const port = await bounded(new Promise((resolve, reject) => {
        child.once("error", reject);
        child.stdout.on("data", (chunk) => {
          output += chunk;
          const match = /calls: listening on 127\.0\.0\.1:([0-9]+)/.exec(output);
          if (match) resolve(Number(match[1]));
        });
        exited.then(() => reject(new Error("Service exited before readiness: " + errors)));
      }), 5_000, "Service did not listen");
      assert.ok(port > 0);
      const guestResponse = await fetch(`http://127.0.0.1:${port}/v1/guests`, {
        method: "POST", headers: { "content-type": "application/json" }, body: "{}", signal: AbortSignal.timeout(5_000),
      });
      assert.equal(guestResponse.status, 200);
      const guest = await guestResponse.json();
      await bounded(new Promise((resolve, reject) => {
        stream = http.get(`http://127.0.0.1:${port}/v1/events`, { headers: { authorization: `Guest ${guest.guest_token}` } }, (incoming) => {
          response = incoming;
          assert.equal(incoming.statusCode, 200);
          let data = "";
          incoming.on("data", (chunk) => { data += chunk; if (data.includes("event: ready\n")) resolve(); });
          incoming.on("error", reject);
        });
        stream.on("error", reject);
      }), 5_000, "Guest event stream did not become ready");
      assert.equal(response.complete, false, "The client keeps the event stream open");
      assert.equal(child.kill(signal), true);
      const result = await bounded(exited, 3_000, "Shutdown hung on an open event stream");
      assert.deepEqual(result, { code: 0, killedBy: null });
    } finally {
      response?.destroy();
      stream?.destroy();
      if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
      await bounded(exited, 3_000, "Child cleanup failed");
    }
  });
}
