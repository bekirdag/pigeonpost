// An in-memory postbox implementing the group and MLS rules of docs/groups.md §1–§3, using the
// real WASM server helpers (inspectKeyPackage, inspectEnvelope, verifyEnvelopeSignature,
// welcomeRecipients) for the checks the real postbox makes with the Rust crate. Small on purpose:
// it is the contract the browser client is tested against, not a second server.
import { webcrypto } from "node:crypto";

const b64 = (u8) => Buffer.from(u8).toString("base64");
const unb64 = (s) => new Uint8Array(Buffer.from(String(s), "base64"));
const hex = (u8) => Buffer.from(u8).toString("hex");

export class ApiError extends Error {
  constructor(status, code, body = {}) { super(code); this.status = status; this.code = code; this.body = body; }
}

function attestationPayload(mailbox, deviceId, sigKey) {
  const parts = [];
  const enc = new TextEncoder();
  const u16 = (n) => [(n >> 8) & 255, n & 255];
  parts.push(...enc.encode("pigeonpost/groupcrypto/device-attestation/v1\0"));
  const m = enc.encode(mailbox), d = enc.encode(deviceId);
  parts.push(...u16(m.length), ...m, ...u16(d.length), ...d, ...u16(1), ...u16(sigKey.length), ...sigKey);
  return new Uint8Array(parts);
}

export function createFakePostbox(gc) {
  const S = {
    seeds: new Map(),        // mailbox -> seed
    devices: new Map(),      // mailbox -> Map(deviceId -> {sigKey, revoked})
    kps: new Map(),          // refHex -> {mailbox, deviceId, bytes, lastResort, ref, claimedBy}
    groups: new Map(),
    welcomes: [],
    blobs: new Map(),
    nextId: 1,
    calls: [],
    errors: [],
    beforeHandle: null,
  };
  const id = (p) => p + "_" + (S.nextId++);

  function mailbox(name) {
    const seed = new Uint8Array(32);
    webcrypto.getRandomValues(seed);
    const address = gc.mailboxForSeed(seed);
    S.seeds.set(address, seed);
    S.devices.set(address, new Map());
    return address;
  }

  function devOf(me, deviceId) {
    const d = S.devices.get(me)?.get(deviceId);
    if (!d) throw new ApiError(403, "unknown_device");
    if (d.revoked) throw new ApiError(403, "device_revoked");
    return d;
  }
  function group(gid) {
    const g = S.groups.get(gid);
    if (!g) throw new ApiError(404, "not_found");
    return g;
  }
  const key = (m, d) => m + "|" + d;

  function pending(g) {
    const out = [];
    for (const [m, mem] of g.members) {
      if (mem.state !== "active" && mem.state !== "keying") continue;
      if (g.activation_state === "provisioning" && m !== g.creator) continue;
      for (const [d, dev] of S.devices.get(m) || []) {
        if (dev.revoked) continue;
        const k = key(m, d);
        if (!g.leaves.has(k)) out.push({ kind: "add", mailbox: m, device_id: d, since: 0 });
      }
    }
    for (const k of g.leaves) {
      const [m, d] = k.split("|");
      const mem = g.members.get(m);
      const dev = S.devices.get(m)?.get(d);
      if (!mem || !["active", "keying"].includes(mem.state) || !dev || dev.revoked || g.stale.has(k)) {
        out.push({ kind: "remove", mailbox: m, device_id: d, since: 0 });
      }
    }
    return out;
  }

  function push(g, item) {
    g.lastSeq += 1;
    item.seq = g.lastSeq;
    item.created_at = Math.floor(Date.now() / 1000);
    g.items.push(item);
    return item;
  }

  // Server-side helpers tests call directly.
  const admin = {
    invite(gid, address) {
      const g = group(gid);
      g.members.set(address, { role: "member", state: "invited", firstSeq: null });
      push(g, { type: "notice", kind: "member_invited", metadata: { address } });
    },
    accept(gid, address) {
      const g = group(gid);
      const m = g.members.get(address);
      if (!m || m.state !== "invited") throw new ApiError(403, "invite_required");
      m.state = "keying";
    },
    remove(gid, address, state = "removed") {
      const g = group(gid);
      const m = g.members.get(address);
      m.state = state;
      g.revision += 1;
      push(g, { type: "notice", kind: state === "left" ? "member_left" : "member_removed", metadata: { address } });
    },
    revoke(address, deviceId) { S.devices.get(address).get(deviceId).revoked = true; },
  };

  async function handle(me, path, o) {
    const method = o.method || "GET";
    const url = new URL(path, "http://fake");
    const p = url.pathname, q = url.searchParams, body = o.body || {};
    S.calls.push({ me, method, path: p });
    if (S.beforeHandle) await S.beforeHandle(me, method, p, body);

    if (p === "/v1/mls/devices" && method === "POST") {
      const devs = S.devices.get(me);
      if (devs.has(body.device_id)) throw new ApiError(409, "device_exists");
      const sigKey = unb64(body.signature_key);
      devs.set(body.device_id, { sigKey, revoked: false });
      const att = gc.signAttestationWithSeed(S.seeds.get(me), attestationPayload(me, body.device_id, sigKey));
      return { device_id: body.device_id, attestation: b64(att), mailbox_public_key: b64(mailboxKey(me)), fingerprint_digits: "" };
    }
    if (p === "/v1/mls/key-packages/count") {
      devOf(me, o.device);
      let unclaimed = 0, lastResort = false;
      for (const k of S.kps.values()) if (k.mailbox === me && k.deviceId === o.device) {
        if (k.lastResort) lastResort = true; else if (!k.claimedBy) unclaimed++;
      }
      return { unclaimed, last_resort: lastResort };
    }
    if (p === "/v1/mls/key-packages" && method === "POST") {
      const d = devOf(me, o.device);
      const all = [...(body.key_packages || []).map((k) => [k, false]), ...(body.last_resort ? [[body.last_resort, true]] : [])];
      for (const [k, lr] of all) {
        const bytes = unb64(k);
        const info = gc.inspectKeyPackage(bytes);
        if (info.mailbox !== me || info.deviceId !== o.device || hex(info.signatureKey) !== hex(d.sigKey)) throw new ApiError(400, "invalid_key_package");
        if (lr) for (const [r, v] of S.kps) if (v.mailbox === me && v.deviceId === o.device && v.lastResort) S.kps.delete(r);
        S.kps.set(hex(info.keyPackageRef), { mailbox: me, deviceId: o.device, bytes: k, lastResort: lr, ref: b64(info.keyPackageRef), claimedBy: null });
      }
      return { stored: all.length };
    }
    if (p === "/v1/mls/welcomes") {
      devOf(me, o.device);
      return { welcomes: S.welcomes.filter((w) => w.mailbox === me && w.device_id === o.device && !w.acked).map(({ welcome_id, group_id, epoch, seq, welcome }) => ({ welcome_id, group_id, epoch, seq, welcome })) };
    }
    let m = /^\/v1\/mls\/welcomes\/([^/]+)\/ack$/.exec(p);
    if (m) {
      const w = S.welcomes.find((x) => x.welcome_id === decodeURIComponent(m[1]) && x.mailbox === me);
      if (!w) throw new ApiError(404, "not_found");
      w.acked = true;
      if (!body.joined) S.groups.get(w.group_id).stale.add(key(me, w.device_id));
      return { ok: true };
    }
    if (p === "/v1/groups" && method === "POST") {
      const gid = "grp_" + Math.random().toString(36).slice(2, 12);
      const g = { id: gid, name: body.name, creator: me, revision: 1, epoch: 0, activation_state: "provisioning", lastSeq: 0, items: [], members: new Map([[me, { role: "owner", state: "keying", firstSeq: null }]]), leaves: new Set(), stale: new Set(), claims: new Map(), seen: new Map(), post_policy: body.post_policy || "everyone" };
      S.groups.set(gid, g);
      return { group: { id: gid, name: body.name, activation_state: "provisioning", revision: 1, epoch: 0 }, invitations: [] };
    }
    m = /^\/v1\/groups\/([^/]+)(\/.*)?$/.exec(p);
    if (!m) throw new ApiError(404, "not_found");
    const g = group(decodeURIComponent(m[1]));
    const rest = m[2] || "";
    const mem = g.members.get(me);

    if (rest === "/crypto") {
      devOf(me, o.device);
      return { epoch: g.epoch, revision: g.revision, activation_state: g.activation_state, in_group: g.leaves.has(key(me, o.device)), pending: pending(g), lagging_devices: [] };
    }
    if (rest === "/key-packages/claim") {
      devOf(me, o.device);
      if (!mem || !(mem.state === "active" || (g.activation_state === "provisioning" && me === g.creator))) throw new ApiError(403, "not_member");
      const claims = [], exhausted = [];
      for (const target of body.mailboxes) {
        for (const pnd of pending(g).filter((x) => x.kind === "add" && x.mailbox === target)) {
          let pick = [...S.kps.values()].find((k) => k.mailbox === target && k.deviceId === pnd.device_id && !k.lastResort && !k.claimedBy);
          if (!pick) pick = [...S.kps.values()].find((k) => k.mailbox === target && k.deviceId === pnd.device_id && k.lastResort);
          if (!pick) { exhausted.push({ mailbox: target, device_id: pnd.device_id }); continue; }
          if (!pick.lastResort) pick.claimedBy = key(me, o.device);
          g.claims.set(pick.ref, { by: key(me, o.device), mailbox: target, device_id: pnd.device_id });
          claims.push({ mailbox: target, device_id: pnd.device_id, key_package: pick.bytes, key_package_ref: pick.ref });
        }
      }
      return { claims, exhausted };
    }
    if (rest === "/commits") {
      const d = devOf(me, o.device);
      const creatorActivation = g.activation_state === "provisioning" && me === g.creator;
      if (!mem || !(mem.state === "active" || creatorActivation)) throw new ApiError(403, "not_member");
      if (body.epoch !== g.epoch || body.expected_revision !== g.revision) throw new ApiError(409, "revision_conflict", { current_epoch: g.epoch, revision: g.revision });
      const commit = unb64(body.commit);
      gc.verifyEnvelopeSignature(d.sigKey, commit, unb64(body.envelope_signature));
      const env = gc.inspectEnvelope(commit);
      if (env.kind !== "commit" || env.groupId !== g.id || env.senderMailbox !== me || env.senderDevice !== o.device || env.clientMessageId !== body.commit_id || env.epoch !== g.epoch) throw new ApiError(400, "invalid_commit");
      const pend = pending(g);
      for (const a of body.adds) {
        if (!pend.some((x) => x.kind === "add" && x.mailbox === a.mailbox && x.device_id === a.device_id)) throw new ApiError(403, "not_pending");
        const c = g.claims.get(a.key_package_ref);
        if (!c || c.by !== key(me, o.device) || c.mailbox !== a.mailbox || c.device_id !== a.device_id) throw new ApiError(403, "unclaimed");
      }
      for (const r of body.removes) {
        const ok = pend.some((x) => x.kind === "remove" && x.mailbox === r.mailbox && x.device_id === r.device_id) || r.mailbox === me;
        if (!ok) throw new ApiError(403, "not_pending");
      }
      if (body.adds.length) {
        if (!body.welcome) throw new ApiError(400, "welcome_required");
        const recips = Array.from(gc.welcomeRecipients(unb64(body.welcome))).map(b64).sort();
        const refs = body.adds.map((a) => a.key_package_ref).sort();
        if (JSON.stringify(recips) !== JSON.stringify(refs)) throw new ApiError(400, "welcome_mismatch");
      }
      const item = push(g, { type: "commit", epoch: g.epoch, sender: me, sender_device: o.device, commit_id: body.commit_id, commit: body.commit, adds: body.adds.map(({ mailbox, device_id }) => ({ mailbox, device_id })), removes: body.removes });
      if (creatorActivation) { g.leaves.add(key(me, o.device)); g.activation_state = "active"; mem.state = "active"; mem.firstSeq = item.seq + 1; }
      for (const r of body.removes) { g.leaves.delete(key(r.mailbox, r.device_id)); g.stale.delete(key(r.mailbox, r.device_id)); }
      for (const a of body.adds) {
        g.leaves.add(key(a.mailbox, a.device_id));
        const am = g.members.get(a.mailbox);
        if (am.state === "keying") { am.state = "active"; am.firstSeq = item.seq + 1; }
        S.welcomes.push({ welcome_id: id("wel"), group_id: g.id, epoch: g.epoch + 1, seq: item.seq, welcome: body.welcome, mailbox: a.mailbox, device_id: a.device_id });
      }
      g.epoch += 1;
      g.revision += 1;
      return { seq: item.seq, epoch: g.epoch, revision: g.revision };
    }
    if (rest === "/epoch-ack") return { ok: true };
    if (rest === "/resync") { g.stale.add(key(me, o.device)); return { ok: true }; }
    if (rest.startsWith("/messages") && method === "GET") {
      devOf(me, o.device);
      if (!mem || mem.state !== "active") throw new ApiError(403, "not_member");
      const after = Number(q.get("after_seq") || 0), limit = Math.min(100, Number(q.get("limit") || 100));
      const visible = g.items.filter((i) => i.seq > after && i.seq >= (mem.firstSeq || 0) - 1);
      const items = visible.slice(0, limit);
      return { items, last_seq: g.lastSeq, epoch: g.epoch, more: visible.length > limit };
    }
    if (rest === "/messages" && method === "POST") {
      const d = devOf(me, o.device);
      if (!mem || mem.state !== "active") throw new ApiError(403, "not_member");
      if (g.post_policy === "admins" && !["owner", "admin"].includes(mem.role)) throw new ApiError(403, "posting_restricted");
      const dup = g.items.find((i) => i.type === "application" && i.sender === me && i.client_message_id === body.client_message_id);
      if (dup) return { seq: dup.seq, id: dup.id, expires_at: null };
      if (body.epoch !== g.epoch) throw new ApiError(409, "epoch_conflict", { current_epoch: g.epoch });
      if (pending(g).some((x) => x.kind === "remove")) throw new ApiError(409, "key_activation_pending");
      const ct = unb64(body.ciphertext);
      gc.verifyEnvelopeSignature(d.sigKey, ct, unb64(body.envelope_signature));
      const env = gc.inspectEnvelope(ct);
      if (env.kind !== "application" || env.groupId !== g.id || env.senderMailbox !== me || env.senderDevice !== o.device || env.clientMessageId !== body.client_message_id) throw new ApiError(400, "invalid_envelope");
      const item = push(g, { type: "application", id: id("gm"), epoch: g.epoch, sender: me, sender_device: o.device, client_message_id: body.client_message_id, ciphertext: body.ciphertext, attachments: (body.attachments || []).map((a) => ({ id: a, bytes: S.blobs.get(a)?.length || 0 })), edit_of: body.edit_of ?? null, expires_at: g.ephemeral ? Math.floor(Date.now() / 1000) + g.ephemeral : null });
      return { seq: item.seq, id: item.id, expires_at: item.expires_at };
    }
    if (rest === "/attachments" && method === "POST") {
      devOf(me, o.device);
      const att = id("gat");
      S.blobs.set(att, new Uint8Array(o.rawBody));
      return { id: att, bytes: o.rawBody.length };
    }
    m = /^\/attachments\/(.+)$/.exec(rest);
    if (m && method === "GET") {
      if (!mem || mem.state !== "active") throw new ApiError(403, "not_member");
      const blob = S.blobs.get(decodeURIComponent(m[1]));
      if (!blob) throw new ApiError(404, "not_found");
      return blob.buffer.slice(blob.byteOffset, blob.byteOffset + blob.byteLength);
    }
    throw new ApiError(404, "not_found:" + method + " " + p);
  }

  // The mailbox's ed25519 public key, derived from its seed with Node's crypto.
  const pubKeys = new Map();
  function mailboxKey(me) {
    if (!pubKeys.has(me)) {
      pubKeys.set(me, edPublic(S.seeds.get(me)));
    }
    return pubKeys.get(me);
  }

  // Each client gets an api() that acts as one mailbox.
  function clientApi(me) {
    return async (path, o = {}) => {
      try {
        const out = await handle(me, path, o);
        return o.raw ? out : JSON.parse(JSON.stringify(out));
      } catch (e) {
        if (e instanceof ApiError) S.errors.push({ me, path, code: e.code });
        if (e instanceof ApiError) throw new (globalThis.PigeonpostGroupsCore.GroupApiError)(e.status, e.code, e.body);
        throw e;
      }
    };
  }

  return { S, mailbox, clientApi, admin, group: (gid) => S.groups.get(gid) };
}

import { createPrivateKey, createPublicKey } from "node:crypto";
function edPublic(seed) {
  const der = Buffer.concat([Buffer.from("302e020100300506032b657004220420", "hex"), Buffer.from(seed)]);
  const pub = createPublicKey(createPrivateKey({ key: der, format: "der", type: "pkcs8" })).export({ format: "der", type: "spki" });
  return new Uint8Array(pub.subarray(pub.length - 32));
}
