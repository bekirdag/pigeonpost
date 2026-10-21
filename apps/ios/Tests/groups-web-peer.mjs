// The web inbox's own group client (site-inbox/groups-core.js with the vendored WASM bundle), run
// in Node as "Dana" against the same test postbox as the Swift test: proof that a message, a file
// and a membership change written by the Apple client read correctly in the browser's client and
// back. Driven by Tests/groups-e2e.sh beside GroupsE2ETests.swift.
//
//   node apps/ios/Tests/groups-web-peer.mjs <setup.json>
//
// Waits for an invitation to a group called "Interop", accepts it, waits to be added, reads the
// Swift side's message and file, answers with its own, and exits 0 — or 1 with the reason.
import { readFileSync } from "node:fs";
import { readFile } from "node:fs/promises";
import vm from "node:vm";
import path from "node:path";
import { fileURLToPath } from "node:url";

const site = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../../site-inbox");
vm.runInThisContext(readFileSync(path.join(site, "groups-core.js"), "utf8"));
const Core = globalThis.PigeonpostGroupsCore;
const { loadGroupCrypto } = await import(path.join(site, "vendor/groupcrypto/loader.js"));
const gc = await loadGroupCrypto({ wasmBytes: await readFile(path.join(site, "vendor/groupcrypto/pigeonpost_groupcrypto_bg.wasm")) });

const setup = JSON.parse(readFileSync(process.argv[2], "utf8"));
const me = setup.people.dana;
const say = (...a) => console.log("  web  ", ...a);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const deadline = Date.now() + 120000;
async function until(what, fn) {
  while (Date.now() < deadline) {
    const v = await fn().catch(() => null);
    if (v) return v;
    await sleep(300);
  }
  throw new Error("timed out: " + what);
}

async function api(p, o = {}) {
  const headers = { authorization: "Bearer " + me.token, "x-pigeonpost-identity": me.address, accept: o.raw ? "application/octet-stream" : "application/json" };
  if (o.device) headers["x-pigeonpost-device"] = o.device;
  let body;
  if (o.rawBody) { headers["content-type"] = "application/octet-stream"; body = o.rawBody; }
  else if (o.body) { headers["content-type"] = "application/json"; body = JSON.stringify(o.body); }
  const res = await fetch(setup.postbox + p, { method: o.method || "GET", headers, body });
  if (o.raw && res.ok) return res.arrayBuffer();
  const value = await res.json().catch(() => ({}));
  if (!res.ok) throw new Core.GroupApiError(res.status, value.error, value);
  return value;
}

try {
  const client = Core.createClient({ gc, api, mailbox: me.address, storage: Core.memoryStorage() });
  await client.init();
  await client.topUpKeyPackages();
  say("device", client.deviceId(), "ready");
  const group = await until("an invitation to Interop", async () => {
    const { groups = [] } = await api("/v1/groups?state=invited");
    return groups.find((g) => g.name === "Interop" && g.my && g.my.state === "invited");
  });
  await api(`/v1/groups/${group.id}/invitation`, { method: "POST", body: { accept: true } });
  await until("being added", async () => (await client.processWelcomes()).includes(group.id) || client.inGroup(group.id));
  say("joined", group.id);
  const fromSwift = await until("the Swift message", async () => {
    await client.sync(group.id);
    return client.history(group.id).find((x) => x.kind === "message" && x.text === "from Swift, with a file");
  });
  if (fromSwift.files.length !== 1 || fromSwift.files[0].name !== "notes.txt") throw new Error("the file did not survive: " + JSON.stringify(fromSwift.files));
  const opened = new TextDecoder().decode(await client.openFile(group.id, fromSwift.files[0]));
  if (opened !== "written on a Mac") throw new Error("file contents differ: " + opened);
  say("read the Swift message and opened its file");
  await client.send(group.id, { text: "from the web client", files: [{ name: "reply.txt", mime: "text/plain", bytes: new TextEncoder().encode("written in a browser") }] });
  say("answered");
  process.exit(0);
} catch (e) {
  console.error("  web   FAIL", e && (e.stack || e.message || e));
  process.exit(1);
}
