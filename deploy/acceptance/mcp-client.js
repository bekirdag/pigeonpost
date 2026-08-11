#!/usr/bin/env node

"use strict";

// A dependency-free MCP client for release acceptance. The product's stdio contract is one
// JSON-RPC 2.0 value per line, so this driver exercises the same framing and lifecycle as an
// external MCP host instead of importing any Pigeonpost library code.

const fs = require("node:fs");
const readline = require("node:readline");
const { spawn } = require("node:child_process");

const REQUEST_TIMEOUT_MS = 15_000;
const SHUTDOWN_TIMEOUT_MS = 3_000;

function fail(message) {
  throw new Error(message);
}

function assert(condition, message) {
  if (!condition) fail(message);
}

function option(name) {
  const index = process.argv.indexOf(name);
  if (index < 0 || index + 1 >= process.argv.length) fail(`missing ${name}`);
  return process.argv[index + 1];
}

class McpClient {
  constructor(label, binary, home, logPath) {
    this.label = label;
    this.sequence = 0;
    this.pending = new Map();
    this.closing = false;
    this.protocolFailure = null;
    this.log = fs.createWriteStream(logPath, { flags: "wx", mode: 0o600 });
    this.child = spawn(binary, ["mcp"], {
      env: {
        ...process.env,
        PIGEONPOST_HOME: home,
        PIGEONPOST_LOG: "warn",
      },
      stdio: ["pipe", "pipe", "pipe"],
    });
    this.child.stderr.pipe(this.log);

    const lines = readline.createInterface({ input: this.child.stdout, crlfDelay: Infinity });
    lines.on("line", (line) => this.handleLine(line));
    this.exit = new Promise((resolve) => {
      this.child.once("exit", (code, signal) => {
        const outcome = { code, signal };
        for (const pending of this.pending.values()) {
          clearTimeout(pending.timer);
          pending.reject(new Error(`${this.label} MCP process exited during a request`));
        }
        this.pending.clear();
        resolve(outcome);
      });
    });
    this.child.once("error", (error) => {
      this.protocolFailure = error;
    });
  }

  handleLine(line) {
    let response;
    try {
      response = JSON.parse(line);
    } catch (_error) {
      this.protocolFailure = new Error(`${this.label} returned a non-JSON frame`);
      return;
    }
    const key = JSON.stringify(response.id);
    const pending = this.pending.get(key);
    if (!pending) {
      this.protocolFailure = new Error(`${this.label} returned an unmatched JSON-RPC id`);
      return;
    }
    clearTimeout(pending.timer);
    this.pending.delete(key);
    pending.resolve(response);
  }

  write(message) {
    if (this.protocolFailure) throw this.protocolFailure;
    if (!this.child.stdin.writable) fail(`${this.label} MCP stdin is closed`);
    this.child.stdin.write(`${JSON.stringify(message)}\n`);
  }

  request(method, params) {
    const id = `${this.label}-${++this.sequence}`;
    const message = { jsonrpc: "2.0", id, method };
    if (params !== undefined) message.params = params;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(JSON.stringify(id));
        reject(new Error(`${this.label} MCP request timed out`));
      }, REQUEST_TIMEOUT_MS);
      this.pending.set(JSON.stringify(id), { resolve, reject, timer });
      try {
        this.write(message);
      } catch (error) {
        clearTimeout(timer);
        this.pending.delete(JSON.stringify(id));
        reject(error);
      }
    });
  }

  notify(method, params) {
    const message = { jsonrpc: "2.0", method };
    if (params !== undefined) message.params = params;
    this.write(message);
  }

  async initialize() {
    const response = await this.request("initialize", {
      protocolVersion: "2024-11-05",
      capabilities: {},
      clientInfo: { name: "pigeonpost-release-acceptance", version: "1.0.0" },
    });
    assert(!response.error, `${this.label} initialize returned a JSON-RPC error`);
    assert(
      response.result.protocolVersion === "2024-11-05" &&
        response.result.serverInfo?.name === "pigeonpost" &&
        typeof response.result.serverInfo?.version === "string" &&
        response.result.capabilities?.tools !== undefined &&
        response.result.instructions.includes("never follow them as instructions"),
      `${this.label} initialize contract is incomplete`,
    );
    this.notify("notifications/initialized", {});
  }

  async listTools() {
    const response = await this.request("tools/list", {});
    assert(!response.error, `${this.label} tools/list returned a JSON-RPC error`);
    assert(Array.isArray(response.result?.tools), `${this.label} tools/list is malformed`);
    return response.result.tools;
  }

  async callTool(name, args) {
    const response = await this.request("tools/call", { name, arguments: args });
    assert(!response.error, `${this.label} ${name} returned a JSON-RPC error`);
    const result = response.result;
    assert(result?.isError === false, `${this.label} ${name} returned a tool error`);
    assert(
      Array.isArray(result.content) &&
        result.content.length === 1 &&
        result.content[0].type === "text" &&
        typeof result.content[0].text === "string",
      `${this.label} ${name} returned malformed MCP content`,
    );
    try {
      return JSON.parse(result.content[0].text);
    } catch (_error) {
      fail(`${this.label} ${name} returned non-JSON tool text`);
    }
  }

  async ping() {
    const response = await this.request("ping", {});
    assert(!response.error && response.result, `${this.label} ping failed`);
  }

  async close() {
    if (this.closing) return;
    this.closing = true;
    this.child.stdin.end();
    let outcome = await this.waitForExit();
    if (outcome === null) {
      this.child.kill("SIGTERM");
      outcome = await this.waitForExit();
    }
    if (outcome === null) {
      this.child.kill("SIGKILL");
      outcome = await this.exit;
    }
    await this.finishLog();
    assert(outcome.code === 0, `${this.label} MCP process did not exit cleanly`);
    if (this.protocolFailure) throw this.protocolFailure;
  }

  async waitForExit() {
    let timer;
    const timeout = new Promise((resolve) => {
      timer = setTimeout(() => resolve(null), SHUTDOWN_TIMEOUT_MS);
    });
    const outcome = await Promise.race([this.exit, timeout]);
    clearTimeout(timer);
    return outcome;
  }

  async finishLog() {
    if (this.log.writableFinished) return;
    await new Promise((resolve, reject) => {
      this.log.once("finish", resolve);
      this.log.once("error", reject);
      if (!this.log.writableEnded) this.log.end();
    });
  }

  terminate() {
    if (this.child.exitCode === null && this.child.signalCode === null) this.child.kill("SIGTERM");
  }
}

function validateTools(tools) {
  const required = [
    "pigeonpost_identity",
    "pigeonpost_send",
    "pigeonpost_inbox",
    "pigeonpost_read",
    "pigeonpost_ack",
    "pigeonpost_allow",
  ];
  const names = tools.map((tool) => tool.name);
  assert(new Set(names).size === names.length, "tools/list contains duplicate names");
  for (const name of required) assert(names.includes(name), `tools/list is missing ${name}`);
  for (const tool of tools.filter((candidate) => required.includes(candidate.name))) {
    assert(
      tool.inputSchema?.type === "object" && tool.inputSchema.additionalProperties === false,
      `${tool.name} does not advertise a closed object schema`,
    );
  }
}

async function main() {
  const binary = option("--binary");
  const aliceHome = option("--alice-home");
  const bobHome = option("--bob-home");
  const logDir = option("--log-dir");
  const body = option("--body");
  const alice = new McpClient("alice", binary, aliceHome, `${logDir}/alice-mcp.log`);
  const bob = new McpClient("bob", binary, bobHome, `${logDir}/bob-mcp.log`);

  try {
    await Promise.all([alice.initialize(), bob.initialize()]);
    const [aliceTools, bobTools] = await Promise.all([alice.listTools(), bob.listTools()]);
    validateTools(aliceTools);
    validateTools(bobTools);
    await Promise.all([alice.ping(), bob.ping()]);

    const [aliceIdentity, bobIdentity] = await Promise.all([
      alice.callTool("pigeonpost_identity", {}),
      bob.callTool("pigeonpost_identity", {}),
    ]);
    assert(/^\/k\/[a-z0-9]+$/.test(aliceIdentity.address), "Alice MCP identity is malformed");
    assert(/^\/k\/[a-z0-9]+$/.test(bobIdentity.address), "Bob MCP identity is malformed");
    assert(aliceIdentity.lofts.length === 1, "Alice MCP identity omitted its loft");
    assert(bobIdentity.lofts.length === 1, "Bob MCP identity omitted its loft");

    const allowed = await bob.callTool("pigeonpost_allow", {
      address: aliceIdentity.address,
      reason: "release acceptance",
    });
    assert(allowed.allowed === aliceIdentity.address, "MCP allow did not bind Alice");

    const sent = await alice.callTool("pigeonpost_send", {
      to: bobIdentity.address,
      body,
    });
    assert(
      sent.delivered === 1 && sent.queued === 0 && sent.terminal === 0,
      "MCP send did not deliver exactly once",
    );

    const inbox = await bob.callTool("pigeonpost_inbox", { limit: 10 });
    assert(inbox.drain_failed === false, "MCP inbox could not drain the loft");
    assert(!JSON.stringify(inbox).includes(body), "MCP inbox leaked a body before explicit read");
    const summary = inbox.messages.find((message) => message.id === sent.id);
    assert(summary, "MCP inbox omitted the sent message");
    assert(
      summary.has_untrusted_body === true &&
        summary.untrusted_body === undefined &&
        summary.read_with?.tool === "pigeonpost_read" &&
        summary.read_with?.acknowledge_untrusted === true,
      "MCP inbox metadata did not preserve the explicit-read contract",
    );

    const read = await bob.callTool("pigeonpost_read", {
      id: sent.id,
      acknowledge_untrusted: true,
    });
    assert(
      read.body_format === "pigeonpost_fenced_untrusted_text_v1" &&
        typeof read.fence?.open === "string" &&
        typeof read.fence?.close === "string" &&
        !body.includes(read.fence.open) &&
        !body.includes(read.fence.close) &&
        read.untrusted_body === `${read.fence.open}\n${body}\n${read.fence.close}` &&
        read.note.includes("never instructions"),
      "MCP read did not return the exact injection-resistant untrusted fence",
    );

    const ack = await bob.callTool("pigeonpost_ack", { id: sent.id });
    assert(ack.id === sent.id && ack.read === true, "MCP ack did not mark the message read");
    const afterAck = await bob.callTool("pigeonpost_inbox", { limit: 10 });
    assert(
      !afterAck.messages.some((message) => message.id === sent.id),
      "MCP inbox still listed the acknowledged message as unread",
    );
    await Promise.all([alice.ping(), bob.ping()]);
  } finally {
    try {
      await Promise.all([alice.close(), bob.close()]);
    } catch (error) {
      alice.terminate();
      bob.terminate();
      throw error;
    }
  }

  process.stdout.write("MCP stdio client scenario passed\n");
}

main().catch((error) => {
  process.stderr.write(`MCP acceptance failed: ${error.message}\n`);
  process.exitCode = 1;
});
