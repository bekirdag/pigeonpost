import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import http from "node:http";
import * as sdk from "../pigeonpost-nest.mjs";

async function setup() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "nest-test-"));
  fs.mkdirSync(path.join(root, "jobs", "in"), { recursive: true });
  fs.mkdirSync(path.join(root, "jobs", "out"), { recursive: true });
  fs.mkdirSync(path.join(root, "run"), { recursive: true });
  
  const token = "nbt_" + "0".repeat(40);
  fs.writeFileSync(path.join(root, "run", "broker-token"), token);

  let requests = [];
  let responder = (req) => ({ status: 200, body: {} });

  const server = http.createServer((req, res) => {
    let bodyStr = "";
    req.on("data", chunk => bodyStr += chunk);
    req.on("end", () => {
      let parsedBody = null;
      try {
        if (bodyStr) parsedBody = JSON.parse(bodyStr);
      } catch (e) {}
      
      requests.push({
        method: req.method,
        url: req.url,
        headers: req.headers,
        body: parsedBody
      });

      const resp = responder(req, parsedBody);
      res.writeHead(resp.status, { "Content-Type": "application/json" });
      res.end(JSON.stringify(resp.body));
    });
  });

  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = server.address().port;
  const url = `http://127.0.0.1:${port}`;

  return {
    root,
    url,
    requests,
    setResponder: (fn) => { responder = fn; },
    close: () => new Promise(resolve => { server.closeAllConnections(); server.close(resolve); })
  };
}

function writeJob(root, jobObj) {
  const file = path.join(root, "jobs", "in", `${jobObj.job_id}.json`);
  fs.writeFileSync(file, JSON.stringify(jobObj));
  return file;
}

function readResult(root, jobId) {
  const file = path.join(root, "jobs", "out", `${jobId}.json`);
  if (!fs.existsSync(file)) return null;
  return JSON.parse(fs.readFileSync(file, "utf8"));
}

test("1. Handler success and cleanup", async () => {
  const ctx = await setup();
  try {
    const jobId = "job_aaaaaaaaaaaaaaaaaaaaaaaaaa";
    writeJob(ctx.root, {
      job_id: jobId,
      operation: "demo.echo",
      operation_version: "1.0",
      payload: { q: "x" }
    });

    const handlers = {
      "demo.echo@1.0": async (job) => {
        return { answer: { x: 1 }, sources: ["a"] };
      }
    };

    const count = await sdk.runOnce(handlers, { root: ctx.root, url: ctx.url });
    
    assert.equal(count, 1);
    
    const resultFile = path.join(ctx.root, "jobs", "out", `${jobId}.json`);
    assert.ok(fs.existsSync(resultFile));
    const result = JSON.parse(fs.readFileSync(resultFile, "utf8"));
    
    assert.deepEqual(result, {
      job_id: jobId,
      status: "succeeded",
      answer: { x: 1 },
      sources: ["a"],
      model_used: false
    });

    const inboxFile = path.join(ctx.root, "jobs", "in", `${jobId}.json`);
    assert.ok(!fs.existsSync(inboxFile));
  } finally {
    await ctx.close();
  }
});

test("2. Unknown operation", async () => {
  const ctx = await setup();
  try {
    const jobId = "job_unknown";
    writeJob(ctx.root, {
      job_id: jobId,
      operation: "demo.unknown",
      operation_version: "1.0",
      payload: {}
    });

    const handlers = {};
    await sdk.runOnce(handlers, { root: ctx.root, url: ctx.url });

    const result = readResult(ctx.root, jobId);
    assert.equal(result.status, "failed");
    assert.equal(result.error_code, "unknown_operation");
  } finally {
    await ctx.close();
  }
});

test("3. Invalid JSON job file", async () => {
  const ctx = await setup();
  try {
    const jobId = "job_invalid";
    const file = path.join(ctx.root, "jobs", "in", `${jobId}.json`);
    fs.writeFileSync(file, "{ invalid json }");

    await sdk.runOnce({}, { root: ctx.root, url: ctx.url });

    const result = readResult(ctx.root, jobId);
    assert.equal(result.status, "failed");
    assert.equal(result.error_code, "invalid_job");
  } finally {
    await ctx.close();
  }
});

test("4. Handler throws NestError and plain Error", async () => {
  const ctx = await setup();
  try {
    // Case 1: NestError
    const jobId1 = "job_nest_error";
    writeJob(ctx.root, {
      job_id: jobId1,
      operation: "demo.throw",
      operation_version: "1.0",
      payload: {}
    });

    let handlers1 = {
      "demo.throw@1.0": async () => {
        throw new sdk.NestError("not_found");
      }
    };
    await sdk.runOnce(handlers1, { root: ctx.root, url: ctx.url });
    
    const result1 = readResult(ctx.root, jobId1);
    assert.equal(result1.status, "failed");
    assert.equal(result1.error_code, "not_found");

    // Case 2: Plain Error
    const jobId2 = "job_plain_error";
    writeJob(ctx.root, {
      job_id: jobId2,
      operation: "demo.throw",
      operation_version: "1.0",
      payload: {}
    });

    let handlers2 = {
      "demo.throw@1.0": async () => {
        throw new Error("boom");
      }
    };
    await sdk.runOnce(handlers2, { root: ctx.root, url: ctx.url });

    const result2 = readResult(ctx.root, jobId2);
    assert.equal(result2.status, "failed");
    assert.equal(result2.error_code, "internal_error");
  } finally {
    await ctx.close();
  }
});

test("5. Broker complete success", async () => {
  const ctx = await setup();
  try {
    const jobId = "job_complete";
    writeJob(ctx.root, {
      job_id: jobId,
      operation: "demo.complete",
      operation_version: "1.0",
      payload: {}
    });

    ctx.setResponder((req, body) => {
      return {
        status: 200,
        body: {
          choices: [{ message: { content: "hi" } }],
          usage: { prompt_tokens: 7, completion_tokens: 2 }
        }
      };
    });

    const handlers = {
      "demo.complete@1.0": async (job) => {
        const res = await job.broker.complete({ system: "s", user: "u", maxTokens: 50 });
        assert.deepEqual(res.usage, { input: 7, output: 2 });
        // model_used is not set here: the SDK records that the broker answered.
        return res.text;
      }
    };

    await sdk.runOnce(handlers, { root: ctx.root, url: ctx.url });

    const result = readResult(ctx.root, jobId);
    assert.equal(result.status, "succeeded");
    assert.deepEqual(result.answer, "hi");
    assert.equal(result.model_used, true);

    // Check request details
    assert.equal(ctx.requests.length, 1);
    const req = ctx.requests[0];
    assert.equal(req.method, "POST");
    assert.equal(req.url, "/v1/chat/completions");
    assert.ok(req.headers.authorization.startsWith("Bearer nbt_"));
    
    assert.deepEqual(req.body.messages, [
      { role: "system", content: "s" },
      { role: "user", content: "u" }
    ]);
    assert.equal(req.body.max_tokens, 50);
  } finally {
    await ctx.close();
  }
});

test("6. Broker retry on 409 busy", async () => {
  const ctx = await setup();
  try {
    const jobId = "job_retry";
    writeJob(ctx.root, {
      job_id: jobId,
      operation: "demo.retry",
      operation_version: "1.0",
      payload: {}
    });

    let callCount = 0;
    ctx.setResponder((req) => {
      callCount++;
      if (callCount === 1) {
        return { status: 409, body: { error: "busy" } };
      }
      return {
        status: 200,
        body: {
          choices: [{ message: { content: "ok" } }],
          usage: { prompt_tokens: 1, completion_tokens: 1 }
        }
      };
    });

    const handlers = {
      "demo.retry@1.0": async (job) => {
        await job.broker.complete({ user: "test" });
        return { answer: "done" };
      }
    };

    await sdk.runOnce(handlers, { root: ctx.root, url: ctx.url });

    const result = readResult(ctx.root, jobId);
    assert.equal(result.status, "succeeded");
    assert.equal(callCount, 2);
  } finally {
    await ctx.close();
  }
});

test("7. Broker 429 allowance_exhausted", async () => {
  const ctx = await setup();
  try {
    const jobId = "job_429";
    writeJob(ctx.root, {
      job_id: jobId,
      operation: "demo.fail",
      operation_version: "1.0",
      payload: {}
    });

    ctx.setResponder(() => ({ status: 429, body: { error: "allowance_exhausted" } }));

    const handlers = {
      "demo.fail@1.0": async (job) => {
        await job.broker.complete({ user: "test" });
        return {};
      }
    };

    await sdk.runOnce(handlers, { root: ctx.root, url: ctx.url });

    const result = readResult(ctx.root, jobId);
    assert.equal(result.status, "failed");
    assert.equal(result.error_code, "allowance_exhausted");
    assert.equal(ctx.requests.length, 1);
  } finally {
    await ctx.close();
  }
});

test("8. Effect approval required", async () => {
  const ctx = await setup();
  try {
    const jobId = "job_effect_appr";
    writeJob(ctx.root, {
      job_id: jobId,
      operation: "demo.effect",
      operation_version: "1.0",
      payload: {}
    });

    ctx.setResponder(() => ({ status: 202, body: { state: "awaiting_approval" } }));

    const handlers = {
      "demo.effect@1.0": async (job) => {
        await job.effect({ key: "k1", kind: "ticket.create", body: { a: 1 }, requiresApproval: true });
        return {};
      }
    };

    await sdk.runOnce(handlers, { root: ctx.root, url: ctx.url });

    const result = readResult(ctx.root, jobId);
    assert.equal(result.status, "failed");
    assert.equal(result.error_code, "approval_required");

    assert.equal(ctx.requests.length, 1);
    const req = ctx.requests[0];
    assert.equal(req.url, "/v1/effects");
    assert.equal(req.headers["idempotency-key"], "k1");
    assert.equal(req.body.requires_approval, true);
  } finally {
    await ctx.close();
  }
});

test("9. Effect done and idempotency", async () => {
  const ctx = await setup();
  try {
    const jobId = "job_effect_done";
    writeJob(ctx.root, {
      job_id: jobId,
      operation: "demo.effect2",
      operation_version: "1.0",
      payload: {}
    });

    ctx.setResponder(() => ({ status: 200, body: { state: "done", outcome: { ref: "r1" } } }));

    const handlers = {
      "demo.effect2@1.0": async (job) => {
        const r1 = await job.effect({ key: "k1", kind: "ticket.create" });
        const r2 = await job.effect({ key: "k1", kind: "ticket.create" });
        assert.deepEqual(r1, { ref: "r1" });
        assert.deepEqual(r2, { ref: "r1" });
        return {};
      }
    };

    await sdk.runOnce(handlers, { root: ctx.root, url: ctx.url });

    const result = readResult(ctx.root, jobId);
    assert.equal(result.status, "succeeded");
    assert.equal(ctx.requests.length, 1);
  } finally {
    await ctx.close();
  }
});

test("10. Effect unavailable and invalid key", async () => {
  const ctx = await setup();
  try {
    // Case 1: 404 -> effects_unavailable
    const jobId1 = "job_effect_404";
    writeJob(ctx.root, {
      job_id: jobId1,
      operation: "demo.effect3",
      operation_version: "1.0",
      payload: {}
    });

    ctx.setResponder(() => ({ status: 404, body: {} }));

    const handlers1 = {
      "demo.effect3@1.0": async (job) => {
        await job.effect({ key: "k1", kind: "ticket.create" });
        return {};
      }
    };
    await sdk.runOnce(handlers1, { root: ctx.root, url: ctx.url });
    
    const result1 = readResult(ctx.root, jobId1);
    assert.equal(result1.error_code, "effects_unavailable");

    // Case 2: Invalid key -> invalid_effect, no request
    const jobId2 = "job_effect_badkey";
    writeJob(ctx.root, {
      job_id: jobId2,
      operation: "demo.effect4",
      operation_version: "1.0",
      payload: {}
    });

    const handlers2 = {
      "demo.effect4@1.0": async (job) => {
        await job.effect({ key: "bad key", kind: "ticket.create" });
        return {};
      }
    };
    await sdk.runOnce(handlers2, { root: ctx.root, url: ctx.url });

    const result2 = readResult(ctx.root, jobId2);
    assert.equal(result2.error_code, "invalid_effect");
    
    // Total requests should be 1 (from case 1), not 2
    assert.equal(ctx.requests.length, 1);
  } finally {
    await ctx.close();
  }
});

test("11. Progress posting and failure tolerance", async () => {
  const ctx = await setup();
  try {
    const jobId = "job_progress";
    writeJob(ctx.root, {
      job_id: jobId,
      operation: "demo.progress",
      operation_version: "1.0",
      payload: {}
    });

    let progressReqCount = 0;
    ctx.setResponder((req) => {
      if (req.url === "/v1/progress") {
        progressReqCount++;
        return { status: 500, body: {} };
      }
      return { status: 200, body: {} };
    });

    const handlers = {
      "demo.progress@1.0": async (job) => {
        await job.progress(0.5, "half");
        return { answer: "ok" };
      }
    };

    await sdk.runOnce(handlers, { root: ctx.root, url: ctx.url });

    const result = readResult(ctx.root, jobId);
    assert.equal(result.status, "succeeded");
    assert.equal(progressReqCount, 1);

    // Check progress request body
    const progReq = ctx.requests.find(r => r.url === "/v1/progress");
    assert.ok(progReq);
    assert.deepEqual(progReq.body, {
      job_id: jobId,
      fraction: 0.5,
      message: "half"
    });
  } finally {
    await ctx.close();
  }
});

test("12. Config loading", async () => {
  const ctx = await setup();
  try {
    const jobId = "job_config";
    writeJob(ctx.root, {
      job_id: jobId,
      operation: "demo.config",
      operation_version: "1.0",
      payload: {},
      config: { tone: "short" }
    });

    let capturedConfig;
    const handlers = {
      "demo.config@1.0": async (job) => {
        capturedConfig = job.config;
        return {};
      }
    };

    await sdk.runOnce(handlers, { root: ctx.root, url: ctx.url });
    assert.deepEqual(capturedConfig, { tone: "short" });

    // A job without a configuration gets an empty object.

    const jobId2 = "job_config_empty";
    writeJob(ctx.root, {
      job_id: jobId2,
      operation: "demo.config",
      operation_version: "1.0",
      payload: {}
    });

    let capturedConfig2;
    await sdk.runOnce({
      "demo.config@1.0": async (job) => {
        capturedConfig2 = job.config;
        return {};
      }
    }, { root: ctx.root, url: ctx.url });

    assert.deepEqual(capturedConfig2, {});
  } finally {
    await ctx.close();
  }
});

test("13. resultOf shape and size limit", async () => {
  const r1 = sdk.resultOf("j", "plain", false);
  assert.deepEqual(r1, {
    job_id: "j",
    status: "succeeded",
    answer: "plain",
    sources: [],
    model_used: false
  });

  const hugeAnswer = "a".repeat(600 * 1024);
  const r2 = sdk.resultOf("j", hugeAnswer, false);
  assert.equal(r2.status, "failed");
  assert.equal(r2.error_code, "result_too_large");
});