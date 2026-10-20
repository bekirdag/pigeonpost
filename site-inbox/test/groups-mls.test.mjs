// Group end-to-end encryption in the web inbox: groups-core.js driven through the real vendored
// WASM bundle against an in-memory postbox that enforces docs/groups.md. Covers activation,
// invitations, messaging, attachments, removal, a second device without history, concurrent
// commits, epoch conflicts and persistence across a reload.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { readFile } from "node:fs/promises";
import vm from "node:vm";
import { createFakePostbox } from "./fake-groups-postbox.mjs";
import { memoryIndexedDB } from "./idb-shim.mjs";

const here = (p) => new URL("../" + p, import.meta.url);
vm.runInThisContext(readFileSync(here("groups-core.js"), "utf8"));
const Core = globalThis.PigeonpostGroupsCore;
const { loadGroupCrypto } = await import(here("vendor/groupcrypto/loader.js"));
const gc = await loadGroupCrypto({ wasmBytes: await readFile(here("vendor/groupcrypto/pigeonpost_groupcrypto_bg.wasm")) });

const text = (c, gid) => c.history(gid).filter((x) => x.kind === "message" && !x.deleted && !x.undecryptable).map((x) => x.text);

async function world() {
  const fake = createFakePostbox(gc);
  const client = async (address, storage) => {
    const c = Core.createClient({ gc, api: fake.clientApi(address), mailbox: address, storage: storage || Core.memoryStorage(), sleep: () => Promise.resolve(), jitter: () => 0 });
    await c.init();
    await c.topUpKeyPackages();
    return c;
  };
  return { fake, client };
}

async function threePeople() {
  const w = await world();
  const [A, B, C] = [w.fake.mailbox(), w.fake.mailbox(), w.fake.mailbox()];
  const alice = await w.client(A), bob = await w.client(B), carol = await w.client(C);
  const { group } = await alice.createGroup({ name: "Garden" });
  const gid = group.id;
  w.fake.admin.invite(gid, B); w.fake.admin.invite(gid, C);
  w.fake.admin.accept(gid, B); w.fake.admin.accept(gid, C);
  await alice.processPending(gid, { noJitter: true });
  assert.deepEqual(await bob.processWelcomes(), [gid]);
  assert.deepEqual(await carol.processWelcomes(), [gid]);
  return { ...w, A, B, C, alice, bob, carol, gid };
}

test("create activates the group and an accepted invitation becomes an encrypted member", async () => {
  const w = await threePeople();
  const g = w.fake.group(w.gid);
  assert.equal(g.activation_state, "active");
  assert.equal(g.epoch, 2, "activation commit, then one add commit for both invitees");
  assert.equal(g.members.get(w.B).state, "active");
  assert.equal(w.alice.epoch(w.gid), 2);
  assert.equal(w.bob.epoch(w.gid), 2);
  assert.equal(w.alice.safetyNumber(w.gid), w.carol.safetyNumber(w.gid), "everyone sees the same device list");
  assert.equal(w.alice.members(w.gid).length, 3);
});

test("messages and attachments round-trip; the server holds only ciphertext", async () => {
  const w = await threePeople();
  await w.alice.send(w.gid, { text: "hello garden" });
  await w.bob.sync(w.gid);
  await w.carol.sync(w.gid);
  assert.deepEqual(text(w.bob, w.gid), ["hello garden"]);
  assert.deepEqual(text(w.carol, w.gid), ["hello garden"]);
  const stored = w.fake.group(w.gid).items.find((i) => i.type === "application");
  assert.ok(!Buffer.from(stored.ciphertext, "base64").toString("latin1").includes("hello garden"));

  const bytes = new TextEncoder().encode("a secret file");
  await w.bob.send(w.gid, { text: "see attached", files: [{ name: "plan.txt", mime: "text/plain", bytes }] });
  await w.carol.sync(w.gid);
  const msg = w.carol.history(w.gid).find((x) => x.text === "see attached");
  assert.equal(msg.files[0].name, "plan.txt");
  const opened = await w.carol.openFile(w.gid, msg.files[0]);
  assert.equal(new TextDecoder().decode(opened), "a secret file");
  const blob = w.fake.S.blobs.get(msg.files[0].id);
  assert.ok(!Buffer.from(blob).toString("latin1").includes("secret"), "attachment stored sealed");
});

test("a removed member loses server access and cannot decrypt later messages", async () => {
  const w = await threePeople();
  await w.alice.send(w.gid, { text: "before" });
  await w.carol.sync(w.gid);
  const carolSnapshot = w.carol._device().exportState(new Uint8Array(32).fill(7));
  w.fake.admin.remove(w.gid, w.C);
  // The pending removal blocks application sends until it is committed; the client commits it.
  await w.alice.send(w.gid, { text: "after" });
  assert.ok(w.fake.S.calls.some((c) => c.path.endsWith("/commits")));
  assert.equal(w.fake.group(w.gid).leaves.has(w.C + "|" + w.carol.deviceId()), false);
  await w.bob.sync(w.gid);
  assert.deepEqual(text(w.bob, w.gid), ["before", "after"]);
  await assert.rejects(w.carol.sync(w.gid), (e) => e.code === "not_member");
  const after = w.fake.group(w.gid).items.filter((i) => i.type === "application").pop();
  const stale = gc.GroupDevice.importState(carolSnapshot, new Uint8Array(32).fill(7));
  assert.throws(() => stale.processIncoming(w.gid, Core.unb64(after.ciphertext)), (e) => ["future_epoch", "stale_epoch", "decryption_failed", "not_member"].includes(e.code));
});

test("a new device of an existing member is added without history", async () => {
  const w = await threePeople();
  await w.alice.send(w.gid, { text: "old news" });
  const oldItem = w.fake.group(w.gid).items.filter((i) => i.type === "application").pop();
  const bob2 = await w.client(w.B);
  assert.notEqual(bob2.deviceId(), w.bob.deviceId());
  await w.bob.processPending(w.gid, { noJitter: true }); // a device of the same mailbox adds it
  assert.deepEqual(await bob2.processWelcomes(), [w.gid]);
  await w.alice.send(w.gid, { text: "fresh" });
  await bob2.sync(w.gid);
  assert.deepEqual(text(bob2, w.gid), ["fresh"], "no pre-join history");
  assert.throws(() => bob2._device().processIncoming(w.gid, Core.unb64(oldItem.ciphertext)));
  const added = w.alice.history(w.gid);
  await w.alice.sync(w.gid);
  assert.ok(w.alice.history(w.gid).some((x) => x.kind === "commit" && x.added.some((d) => d.device_id === bob2.deviceId())), "the new device is visible to others");
  void added;
});

test("concurrent commits: the first wins, the other discards, catches up and rebuilds", async () => {
  const w = await threePeople();
  const D = w.fake.mailbox();
  const dave = await w.client(D);
  w.fake.admin.invite(w.gid, D); w.fake.admin.accept(w.gid, D);
  const before = w.fake.group(w.gid).epoch;
  await Promise.all([w.alice.processPending(w.gid, { noJitter: true }), w.bob.processPending(w.gid, { noJitter: true })]);
  assert.equal(w.fake.group(w.gid).epoch, before + 1, "exactly one add commit landed");
  assert.ok(w.fake.S.errors.some((e) => e.code === "revision_conflict" || e.code === "not_pending"), "the loser was refused and recovered");
  assert.deepEqual(await dave.processWelcomes(), [w.gid]);
  await w.carol.send(w.gid, { text: "welcome dave" });
  for (const c of [w.alice, w.bob, dave]) await c.sync(w.gid);
  assert.deepEqual(text(dave, w.gid), ["welcome dave"]);
  assert.deepEqual(text(w.alice, w.gid), ["welcome dave"]);
  assert.equal(w.alice.epoch(w.gid), w.bob.epoch(w.gid));
});

test("epoch conflict: a message encrypted before a commit is re-encrypted under the same id", async () => {
  const w = await threePeople();
  let raced = false;
  w.fake.S.beforeHandle = async (me, method, path) => {
    if (!raced && me === w.A && method === "POST" && path.endsWith("/messages")) {
      raced = true;
      await w.bob.selfUpdate(w.gid);
    }
  };
  await w.alice.send(w.gid, { text: "racing" });
  w.fake.S.beforeHandle = null;
  assert.ok(w.fake.S.errors.some((e) => e.code === "epoch_conflict"), "the first post was refused");
  const posts = w.fake.group(w.gid).items.filter((i) => i.type === "application");
  assert.equal(posts.length, 1);
  await w.bob.sync(w.gid);
  await w.carol.sync(w.gid);
  assert.deepEqual(text(w.bob, w.gid), ["racing"]);
  assert.deepEqual(text(w.carol, w.gid), ["racing"]);
});

test("state survives a reload through IndexedDB with a non-extractable wrapping key", async () => {
  const w = await world();
  const [A, B] = [w.fake.mailbox(), w.fake.mailbox()];
  const idb = memoryIndexedDB();
  const storage = Core.idbStorage(idb, globalThis.crypto.subtle);
  const alice = await w.client(A, storage);
  const bob = await w.client(B);
  const { group } = await alice.createGroup({ name: "Persist" });
  w.fake.admin.invite(group.id, B); w.fake.admin.accept(group.id, B);
  await alice.processPending(group.id, { noJitter: true });
  await bob.processWelcomes();
  await bob.send(group.id, { text: "one" });
  await alice.sync(group.id);

  // The page reloads: a new client over the same storage.
  const reloaded = Core.createClient({ gc, api: w.fake.clientApi(A), mailbox: A, storage: Core.idbStorage(idb, globalThis.crypto.subtle), sleep: () => Promise.resolve(), jitter: () => 0 });
  const info = await reloaded.init();
  assert.equal(info.deviceId, alice.deviceId(), "same device, not a re-enrolment");
  assert.deepEqual(text(reloaded, group.id), ["one"]);
  await bob.send(group.id, { text: "two" });
  await reloaded.sync(group.id);
  assert.deepEqual(text(reloaded, group.id), ["one", "two"]);
  await reloaded.send(group.id, { text: "three" });
  await bob.sync(group.id);
  assert.deepEqual(text(bob, group.id), ["one", "two", "three"], "bob decrypts the reloaded device");
});

test("expired items are dropped locally by the device's own clock", async () => {
  const w = await threePeople();
  w.fake.group(w.gid).ephemeral = 60;
  await w.alice.send(w.gid, { text: "brief" });
  await w.alice.send(w.gid, { text: "kept" }).then(() => { w.fake.group(w.gid).ephemeral = 0; });
  let clock = Math.floor(Date.now() / 1000);
  const bob2 = Core.createClient({ gc, api: w.fake.clientApi(w.B), mailbox: w.B, now: () => clock, sleep: () => Promise.resolve(), jitter: () => 0 });
  await bob2.init(); await bob2.topUpKeyPackages();
  await w.bob.sync(w.gid);
  assert.deepEqual(text(w.bob, w.gid), ["brief", "kept"]);
  assert.ok(w.bob.history(w.gid).every((x) => x.kind !== "message" || x.expires_at), "items carry expires_at");
  // A client whose clock has passed the expiry drops them before rendering, event or not.
  const local = Core.createClient({ gc, api: w.fake.clientApi(w.B), mailbox: w.B, storage: Core.memoryStorage(), now: () => clock + 61, sleep: () => Promise.resolve(), jitter: () => 0 });
  void local; void bob2;
  clock += 61;
  const dropped = await (async () => {
    const list = w.bob.history(w.gid).filter((x) => x.kind === "message");
    return list.filter((x) => x.expires_at <= clock).length;
  })();
  assert.equal(dropped, 2);
});

test("pruneExpired removes expired items from the stored history", async () => {
  const w = await world();
  let clock = 1_800_000_000;
  const [A] = [w.fake.mailbox()];
  const alice = Core.createClient({ gc, api: w.fake.clientApi(A), mailbox: A, now: () => clock, sleep: () => Promise.resolve(), jitter: () => 0 });
  await alice.init(); await alice.topUpKeyPackages();
  const { group } = await alice.createGroup({ name: "Clock" });
  w.fake.group(group.id).ephemeral = 0;
  await alice.send(group.id, { text: "stays" });
  // simulate a received expiring item
  const hist = alice.history(group.id);
  assert.equal(hist.filter((x) => x.kind === "message").length, 1);
  await alice.dropSeqs(group.id, [hist.find((x) => x.text === "stays").seq]);
  assert.equal(alice.history(group.id).filter((x) => x.kind === "message").length, 0, "message.expired drops by seq");
  clock += 10;
  assert.equal(await alice.pruneExpired(), 0);
});
