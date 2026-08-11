#!/usr/bin/env node

"use strict";

// A deliberately small, test-only C2SP witness for the disposable composed acceptance topology.
// It verifies the registry's signed checkpoint before returning a timestamped cosignature. It is
// not a production witness: it checks the submitted proof's shape and sequence, but delegates the
// RFC 6962 consistency calculation to the independently tested registry client that sent it.

const crypto = require("node:crypto");
const fs = require("node:fs");
const http = require("node:http");

const PRIVATE_KEY_PREFIX = Buffer.from("302e020100300506032b657004220420", "hex");
const PUBLIC_KEY_PREFIX = Buffer.from("302a300506032b6570032100", "hex");
const MAX_BODY_BYTES = 64 * 1024;

function fail(message) {
  process.stderr.write(`acceptance witness: ${message}\n`);
  process.exit(1);
}

function readSeed(path) {
  const stat = fs.lstatSync(path);
  if (!stat.isFile() || stat.isSymbolicLink()) fail("seed must be a regular file");
  const seed = fs.readFileSync(path);
  if (seed.length !== 32) fail("seed must contain exactly 32 bytes");
  return seed;
}

function privateKey(seed) {
  return crypto.createPrivateKey({
    key: Buffer.concat([PRIVATE_KEY_PREFIX, seed]),
    format: "der",
    type: "pkcs8",
  });
}

function publicKeyFromSeed(seed) {
  const encoded = crypto.createPublicKey(privateKey(seed)).export({
    format: "der",
    type: "spki",
  });
  return encoded.subarray(encoded.length - 32);
}

function publicKeyFromRaw(raw) {
  return crypto.createPublicKey({
    key: Buffer.concat([PUBLIC_KEY_PREFIX, raw]),
    format: "der",
    type: "spki",
  });
}

function keyHash(name, algorithm, publicKey) {
  return crypto
    .createHash("sha256")
    .update(Buffer.from(`${name}\n`, "utf8"))
    .update(Buffer.from([algorithm]))
    .update(publicKey)
    .digest()
    .subarray(0, 4);
}

function parseCheckpoint(note, expectedOrigin, operatorRaw) {
  if (!note.endsWith("\n") || Buffer.byteLength(note) > MAX_BODY_BYTES) {
    throw new Error("invalid checkpoint bounds");
  }
  const separator = note.indexOf("\n\n");
  if (separator < 0) throw new Error("checkpoint has no signature separator");
  const body = note.slice(0, separator + 1);
  const bodyLines = body.slice(0, -1).split("\n");
  if (bodyLines.length !== 3 || bodyLines[0] !== expectedOrigin) {
    throw new Error("checkpoint origin or body is invalid");
  }
  if (!/^(0|[1-9][0-9]*)$/.test(bodyLines[1])) {
    throw new Error("checkpoint size is invalid");
  }
  const root = Buffer.from(bodyLines[2], "base64");
  if (root.length !== 32 || root.toString("base64") !== bodyLines[2]) {
    throw new Error("checkpoint root is invalid");
  }

  const signatureLines = note.slice(separator + 2).trimEnd().split("\n");
  const prefix = `— ${expectedOrigin} `;
  const line = signatureLines.find((candidate) => candidate.startsWith(prefix));
  if (!line || line.slice(prefix.length).includes(" ")) {
    throw new Error("checkpoint operator signature is missing");
  }
  const blob = Buffer.from(line.slice(prefix.length), "base64");
  if (blob.length !== 68) throw new Error("checkpoint operator signature is malformed");
  const expectedHash = keyHash(expectedOrigin, 0x01, operatorRaw);
  if (!crypto.timingSafeEqual(blob.subarray(0, 4), expectedHash)) {
    throw new Error("checkpoint operator key hash is invalid");
  }
  if (!crypto.verify(null, Buffer.from(body, "utf8"), publicKeyFromRaw(operatorRaw), blob.subarray(4))) {
    throw new Error("checkpoint operator signature is invalid");
  }
  return { body, size: BigInt(bodyLines[1]) };
}

function cosignatureLine(name, signingKey, witnessRaw, checkpoint) {
  const timestamp = BigInt(Math.floor(Date.now() / 1000));
  const message = Buffer.from(
    `cosignature/v1\ntime ${timestamp}\n${checkpoint.body}`,
    "utf8",
  );
  const signature = crypto.sign(null, message, signingKey);
  const encodedTimestamp = Buffer.alloc(8);
  encodedTimestamp.writeBigUInt64BE(timestamp);
  const blob = Buffer.concat([
    keyHash(name, 0x04, witnessRaw),
    encodedTimestamp,
    signature,
  ]);
  return `— ${name} ${blob.toString("base64")}\n`;
}

function option(name) {
  const index = process.argv.indexOf(name);
  if (index < 0 || index + 1 >= process.argv.length) fail(`missing ${name}`);
  return process.argv[index + 1];
}

function serve() {
  const seed = readSeed(option("--seed"));
  const name = option("--name");
  const origin = option("--origin");
  const operatorHex = option("--operator-key");
  const host = option("--host");
  const portText = option("--port");
  if (!/^[a-z0-9][a-z0-9./_-]{0,127}$/.test(name)) fail("invalid witness name");
  if (!origin || /[\r\n]/.test(origin)) fail("invalid checkpoint origin");
  if (!/^[0-9a-f]{64}$/.test(operatorHex)) fail("invalid operator public key");
  if (host !== "127.0.0.1") fail("test witness must bind to IPv4 loopback");
  if (!/^[0-9]+$/.test(portText)) fail("invalid port");
  const port = Number(portText);
  if (port < 1024 || port > 65535) fail("port is outside the unprivileged range");

  const signingKey = privateKey(seed);
  const witnessRaw = publicKeyFromSeed(seed);
  const operatorRaw = Buffer.from(operatorHex, "hex");
  let latest = null;

  const server = http.createServer((request, response) => {
    response.setHeader("cache-control", "no-store");
    response.setHeader("x-content-type-options", "nosniff");

    if (request.method === "GET" && request.url === "/health") {
      response.writeHead(200, { "content-type": "text/plain; charset=utf-8" });
      response.end("ok");
      return;
    }
    if (
      request.method === "GET" &&
      /^\/monitoring\/[0-9a-f]{64}\/checkpoint$/.test(request.url)
    ) {
      if (latest === null) {
        response.writeHead(404).end();
      } else {
        response.writeHead(200, { "content-type": "text/plain; charset=utf-8" });
        response.end(latest.note);
      }
      return;
    }
    if (request.method !== "POST" || request.url !== "/submission/add-checkpoint") {
      response.writeHead(404).end();
      return;
    }

    let length = 0;
    let tooLarge = false;
    const chunks = [];
    request.on("data", (chunk) => {
      length += chunk.length;
      if (length > MAX_BODY_BYTES) {
        tooLarge = true;
      } else {
        chunks.push(chunk);
      }
    });
    request.on("end", () => {
      if (tooLarge) {
        response.writeHead(413).end();
        return;
      }
      try {
        const requestBody = Buffer.concat(chunks).toString("utf8");
        const separator = requestBody.indexOf("\n\n");
        if (separator < 0) throw new Error("missing proof separator");
        const proofLines = requestBody.slice(0, separator).split("\n");
        const oldMatch = /^old (0|[1-9][0-9]*)$/.exec(proofLines.shift() || "");
        if (!oldMatch || proofLines.length > 63) throw new Error("invalid proof header");
        for (const proof of proofLines) {
          const decoded = Buffer.from(proof, "base64");
          if (decoded.length !== 32 || decoded.toString("base64") !== proof) {
            throw new Error("invalid consistency proof hash");
          }
        }

        const oldSize = BigInt(oldMatch[1]);
        const operatorNote = requestBody.slice(separator + 2);
        const checkpoint = parseCheckpoint(operatorNote, origin, operatorRaw);
        if (latest === null) {
          if (oldSize !== 0n) throw new Error("witness has no checkpoint at requested size");
        } else {
          if (oldSize !== latest.size || checkpoint.size < latest.size) {
            response.writeHead(409, { "content-type": "text/x.tlog.size" });
            response.end(`${latest.size}\n`);
            return;
          }
          if (checkpoint.size === latest.size && checkpoint.body !== latest.body) {
            throw new Error("same-size checkpoint equivocation");
          }
        }

        const cosignature = cosignatureLine(name, signingKey, witnessRaw, checkpoint);
        latest = {
          body: checkpoint.body,
          note: `${operatorNote}${cosignature}`,
          size: checkpoint.size,
        };
        response.writeHead(200, { "content-type": "text/plain; charset=utf-8" });
        response.end(cosignature);
      } catch (error) {
        // Every thrown message above is a fixed classification; never render request bytes.
        process.stderr.write(`acceptance witness: rejected checkpoint (${error.message})\n`);
        response.writeHead(422).end();
      }
    });
  });

  server.on("clientError", (_error, socket) => socket.end("HTTP/1.1 400 Bad Request\r\n\r\n"));
  server.listen(port, host, () => process.stdout.write(`witness listening on ${host}:${port}\n`));
}

const command = process.argv[2];
if (command === "keygen") {
  const path = process.argv[3];
  if (!path) fail("keygen requires a destination path");
  const descriptor = fs.openSync(path, "wx", 0o600);
  try {
    fs.writeFileSync(descriptor, crypto.randomBytes(32));
    fs.fsyncSync(descriptor);
  } finally {
    fs.closeSync(descriptor);
  }
} else if (command === "public-key") {
  const path = process.argv[3];
  if (!path) fail("public-key requires a seed path");
  process.stdout.write(publicKeyFromSeed(readSeed(path)).toString("hex"));
} else if (command === "serve") {
  serve();
} else {
  fail("usage: witness.js keygen PATH | public-key PATH | serve OPTIONS");
}
