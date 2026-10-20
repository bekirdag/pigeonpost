// Pigeonpost inbox — group end-to-end encryption client (Phase 3a).
//
// The state machine every client runs against the postbox's group routes (docs/groups.md §3,
// "Client loop"; protocol in docs/group-e2ee.md). No DOM here: the UI (groups.js) drives it, and
// the Node tests drive it against a fake postbox with the real WASM bundle.
//
// What the postbox sees: who is in which group, which devices exist, ciphertext sizes and timing.
// What it cannot see: message text, filenames, attachment keys, group secrets. Everything readable
// is produced here, from the MLS library, and kept only in this browser's own encrypted storage —
// a device cannot decrypt a message twice, so the plaintext it decrypted once is the only copy it
// will ever have.
//
// Loaded two ways: as a classic script in the inbox (attaches `window.PigeonpostGroupsCore`) and
// through `vm` in the Node tests.
(function (root) {
  "use strict";

  const ENC_SCHEME = "mls-rfc9420-x25519-aes128gcm-ed25519+ppgc1";
  const KEY_PACKAGE_TARGET = 20;
  const KEY_PACKAGE_FLOOR = 10;
  const HISTORY_LIMIT = 1000;
  const MAX_RETRIES = 6;
  const TIMERS = [null, 300, 3600, 28800, 86400, 604800, 2419200];

  // ---- bytes ----------------------------------------------------------------------------------

  function b64(u8) {
    let s = "";
    const bytes = u8 instanceof Uint8Array ? u8 : new Uint8Array(u8);
    for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
    return btoa(s);
  }
  function unb64(s) {
    const bin = atob(String(s || ""));
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  }
  const enc = new TextEncoder();
  const dec = new TextDecoder();

  const BASE32 = "abcdefghijklmnopqrstuvwxyz234567";
  function randomId(n) {
    const bytes = new Uint8Array(n);
    root.crypto.getRandomValues(bytes);
    let out = "";
    for (const b of bytes) out += BASE32[b & 31];
    return out;
  }
  const newDeviceId = () => "web-" + randomId(12);
  const newRequestKey = () => "rk_" + randomId(20);
  const newClientMessageId = () => "cm_" + randomId(24);

  function hex(u8) {
    return Array.from(u8 || [], (b) => b.toString(16).padStart(2, "0")).join("");
  }

  // ---- errors ---------------------------------------------------------------------------------

  class GroupApiError extends Error {
    constructor(status, code, body) {
      super(code || "http_" + status);
      this.status = status;
      this.code = code || "http_" + status;
      this.body = body || {};
    }
  }

  // ---- message documents ----------------------------------------------------------------------

  // What travels inside the encryption. The postbox never sees any of it.
  function encodeBody(doc) {
    const out = { v: 1, type: doc.type || "text" };
    if (doc.text != null) out.text = String(doc.text).slice(0, 64000);
    if (Array.isArray(doc.files) && doc.files.length) {
      out.files = doc.files.map((f) => ({ id: String(f.id), name: String(f.name || "file").slice(0, 200), mime: String(f.mime || "application/octet-stream").slice(0, 120), size: Number(f.size) || 0 }));
    }
    if (doc.reply_to) out.reply_to = String(doc.reply_to);
    if (doc.edit_of) out.edit_of = String(doc.edit_of);
    return enc.encode(JSON.stringify(out));
  }
  function decodeBody(bytes) {
    try {
      const doc = JSON.parse(dec.decode(bytes));
      if (!doc || typeof doc !== "object") throw new Error("not an object");
      return {
        type: typeof doc.type === "string" ? doc.type : "text",
        text: typeof doc.text === "string" ? doc.text : "",
        files: Array.isArray(doc.files) ? doc.files.filter((f) => f && typeof f === "object").slice(0, 32).map((f) => ({
          id: String(f.id || ""), name: String(f.name || "file"), mime: String(f.mime || "application/octet-stream"), size: Number(f.size) || 0,
        })) : [],
        reply_to: typeof doc.reply_to === "string" ? doc.reply_to : null,
        edit_of: typeof doc.edit_of === "string" ? doc.edit_of : null,
      };
    } catch (_) {
      return { type: "text", text: dec.decode(bytes), files: [], reply_to: null, edit_of: null };
    }
  }

  function memberJson(m) {
    return { mailbox: m.mailbox, device_id: m.deviceId, fingerprint: hex(m.fingerprint) };
  }

  // ---- storage --------------------------------------------------------------------------------

  // In memory, for tests and for a browser where IndexedDB is unavailable (then nothing survives a
  // reload, and the UI says so).
  function memoryStorage() {
    const rows = new Map();
    return {
      persistent: false,
      async load(mailbox) {
        const row = rows.get(mailbox);
        return row ? structuredClone(row) : null;
      },
      async save(mailbox, record) {
        rows.set(mailbox, structuredClone(record));
      },
      async remove(mailbox) { rows.delete(mailbox); },
      dump: () => rows,
    };
  }

  // IndexedDB: one record per mailbox. The MLS state blob is already encrypted under a 32-byte
  // state key; that key and the decrypted history are themselves encrypted with an AES-GCM key that
  // WebCrypto generated as non-extractable and that IndexedDB stores as an opaque CryptoKey. A
  // script cannot read that key's bytes out, but any script running on this origin can still ask
  // the browser to use it: this protects against copying the database files, not against code
  // running in the page. Clearing site data destroys the device for good.
  function idbStorage(idb, subtle) {
    const DB = "pigeonpost-groups", V = 1;
    let dbp = null;
    const open = () => dbp || (dbp = new Promise((resolve, reject) => {
      const req = idb.open(DB, V);
      req.onupgradeneeded = () => {
        const db = req.result;
        if (!db.objectStoreNames.contains("keys")) db.createObjectStore("keys");
        if (!db.objectStoreNames.contains("devices")) db.createObjectStore("devices");
      };
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => reject(req.error);
    }));
    const tx = async (stores, mode, fn) => {
      const db = await open();
      return new Promise((resolve, reject) => {
        const t = db.transaction(stores, mode);
        let result;
        Promise.resolve(fn(t)).then((r) => { result = r; }, reject);
        t.oncomplete = () => resolve(result);
        t.onerror = () => reject(t.error);
        t.onabort = () => reject(t.error || new Error("aborted"));
      });
    };
    const req2p = (r) => new Promise((resolve, reject) => { r.onsuccess = () => resolve(r.result); r.onerror = () => reject(r.error); });
    let wrapKey = null;
    async function key() {
      if (wrapKey) return wrapKey;
      const existing = await tx(["keys"], "readonly", (t) => req2p(t.objectStore("keys").get("wrap")));
      if (existing) return (wrapKey = existing);
      const fresh = await subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]);
      await tx(["keys"], "readwrite", (t) => { t.objectStore("keys").put(fresh, "wrap"); });
      return (wrapKey = fresh);
    }
    async function seal(bytes) {
      const iv = new Uint8Array(12);
      root.crypto.getRandomValues(iv);
      const ct = new Uint8Array(await subtle.encrypt({ name: "AES-GCM", iv }, await key(), bytes));
      return { iv, ct };
    }
    async function open_(box) {
      return new Uint8Array(await subtle.decrypt({ name: "AES-GCM", iv: box.iv }, await key(), box.ct));
    }
    return {
      persistent: true,
      async load(mailbox) {
        const row = await tx(["devices"], "readonly", (t) => req2p(t.objectStore("devices").get(mailbox)));
        if (!row) return null;
        try {
          const stateKey = await open_(row.stateKey);
          const extra = JSON.parse(dec.decode(await open_(row.extra)));
          return { deviceId: row.deviceId, registered: row.registered, stateKey, stateBlob: row.stateBlob, ...extra };
        } catch (_) {
          return null; // a wrap key that no longer opens it: this device is gone
        }
      },
      // One transaction per save: the state blob, its key and the history land together or not at
      // all, so a crash cannot leave an old blob next to newer cursors.
      async save(mailbox, record) {
        const { deviceId, registered, stateKey, stateBlob, ...extra } = record;
        const sealedKey = await seal(stateKey);
        const sealedExtra = await seal(enc.encode(JSON.stringify(extra)));
        await tx(["devices"], "readwrite", (t) => {
          t.objectStore("devices").put({ deviceId, registered, stateKey: sealedKey, stateBlob, extra: sealedExtra }, mailbox);
        });
      },
      async remove(mailbox) {
        await tx(["devices"], "readwrite", (t) => { t.objectStore("devices").delete(mailbox); });
      },
    };
  }

  // ---- the client -----------------------------------------------------------------------------

  // `api(path, {method, body, device, raw, rawBody})` → parsed JSON, or throws GroupApiError.
  // `device: true` asks the caller to send `x-pigeonpost-device`.
  function createClient(opts) {
    const gc = opts.gc;
    const storage = opts.storage || memoryStorage();
    const mailbox = opts.mailbox;
    const now = opts.now || (() => Math.floor(Date.now() / 1000));
    const sleep = opts.sleep || ((ms) => new Promise((r) => setTimeout(r, ms)));
    const jitter = opts.jitter || (() => Math.floor(Math.random() * 3000));
    const emit = opts.onEvent || (() => {});

    let device = null;
    let record = null; // {deviceId, registered, stateKey, stateBlob, groups:{gid:{cursor, inGroup, epochAcked}}, history:{gid:[...]}, warnings}
    let chain = Promise.resolve();

    // Everything that touches the MLS state runs one at a time: the library holds one pending commit
    // per group and the state blob must be written after each mutation, in order.
    function serial(fn) {
      const run = chain.then(fn, fn);
      chain = run.catch(() => {});
      return run;
    }

    const api = (path, o = {}) => opts.api(path, { ...o, device: o.device ? record.deviceId : undefined });

    async function persist() {
      record.stateBlob = device.exportState(record.stateKey);
      await storage.save(mailbox, record);
    }

    function groupRec(gid) {
      record.groups = record.groups || {};
      if (!record.groups[gid]) record.groups[gid] = { cursor: 0, inGroup: false, acked: 0 };
      return record.groups[gid];
    }
    function history(gid) {
      record.history = record.history || {};
      if (!record.history[gid]) record.history[gid] = [];
      return record.history[gid];
    }
    function remember(gid, item) {
      const list = history(gid);
      if (item.seq != null) {
        const at = list.findIndex((x) => x.seq === item.seq && x.kind === item.kind);
        if (at >= 0) { list[at] = { ...list[at], ...item }; return; }
      }
      list.push(item);
      list.sort((a, b) => (a.seq || 0) - (b.seq || 0));
      if (list.length > HISTORY_LIMIT) list.splice(0, list.length - HISTORY_LIMIT);
    }

    // -- device ----------------------------------------------------------------------------------

    async function init() {
      return serial(async () => {
        record = await storage.load(mailbox);
        if (record && record.stateBlob && record.registered) {
          try {
            device = gc.GroupDevice.importState(record.stateBlob, record.stateKey);
          } catch (_) {
            record = null;
          }
        }
        if (!record || !device) await enrol();
        return { deviceId: record.deviceId, persistent: storage.persistent !== false };
      });
    }

    async function enrol() {
      const deviceId = newDeviceId();
      device = new gc.GroupDevice(mailbox, deviceId);
      record = { deviceId, registered: false, stateKey: gc.generateStateKey(), stateBlob: null, groups: {}, history: {}, ownCommits: {} };
      const res = await opts.api("/v1/mls/devices", {
        method: "POST",
        body: { device_id: deviceId, signature_key: b64(device.signaturePublicKey) },
      });
      device.installAttestation(unb64(res.mailbox_public_key), unb64(res.attestation));
      record.registered = true;
      record.fingerprint = res.fingerprint_digits || gc.fingerprintDigits(device.fingerprint);
      await persist();
      emit({ type: "device_enrolled", deviceId });
    }

    // A device the server no longer knows (revoked elsewhere, or a database reset) starts again.
    async function reEnrol() {
      await storage.remove(mailbox);
      record = null;
      device = null;
      await enrol();
    }

    async function topUpKeyPackages() {
      return serial(async () => {
        const count = await api("/v1/mls/key-packages/count", { device: true });
        const need = Math.max(0, KEY_PACKAGE_TARGET - (count.unclaimed || 0));
        if (count.unclaimed >= KEY_PACKAGE_FLOOR && count.last_resort) return 0;
        const packages = need ? Array.from(device.generateKeyPackages(need)) : [];
        const body = { request_key: newRequestKey(), key_packages: packages.map(b64) };
        if (!count.last_resort) body.last_resort = b64(device.generateLastResortKeyPackage());
        await persist();
        await api("/v1/mls/key-packages", { method: "POST", body, device: true });
        return packages.length;
      });
    }

    // -- welcomes ----------------------------------------------------------------------------------

    async function processWelcomes() {
      return serial(async () => {
        const { welcomes = [] } = await api("/v1/mls/welcomes", { device: true });
        const joined = [];
        for (const w of welcomes) {
          let ok = false, code = null;
          try {
            if (device.groups().includes(w.group_id)) device.forgetGroup(w.group_id);
            device.joinFromWelcome(unb64(w.welcome));
            const g = groupRec(w.group_id);
            g.inGroup = true;
            g.cursor = Math.max(Number(w.seq) || 0, 0);
            g.acked = 0;
            ok = true;
            joined.push(w.group_id);
          } catch (e) {
            code = (e && e.code) || "join_failed";
          }
          await persist();
          await api(`/v1/mls/welcomes/${encodeURIComponent(w.welcome_id)}/ack`, {
            method: "POST", device: true, body: { request_key: newRequestKey(), joined: ok, ...(code ? { error_code: code } : {}) },
          }).catch(() => {});
          if (ok) {
            remember(w.group_id, { kind: "notice", seq: Number(w.seq) || 0, local: true, notice: "you_joined", created_at: now() });
            await ackEpoch(w.group_id);
          }
        }
        return joined;
      }).then(async (joined) => {
        // A last-resort package may have been reused for this join; a self-update replaces that leaf
        // key straight away (P-2 §13.6). Only when the device has no single-use packages left, since
        // that is exactly when the last-resort one was handed out.
        if (joined.length) {
          let count = null;
          try { count = await api("/v1/mls/key-packages/count", { device: true }); } catch (_) { /* best effort */ }
          if (count && !count.unclaimed) {
            for (const gid of joined) await selfUpdate(gid).catch(() => {});
          }
        }
        return joined;
      });
    }

    async function ackEpoch(gid) {
      const epoch = device.epoch(gid);
      const g = groupRec(gid);
      if (epoch <= (g.acked || 0)) return;
      try {
        await api(`/v1/groups/${encodeURIComponent(gid)}/epoch-ack`, { method: "POST", device: true, body: { epoch } });
        g.acked = epoch;
      } catch (_) { /* retried at the next commit */ }
    }

    // -- reading -----------------------------------------------------------------------------------

    // Fetch and process everything after the cursor, strictly in sequence order. `upTo` stops early
    // (used between a commit's acceptance and merging it).
    async function syncLocked(gid, upTo) {
      const g = groupRec(gid);
      if (!g.inGroup) return { processed: 0 };
      let processed = 0;
      for (let page = 0; page < 1000; page++) {
        let res;
        try {
          res = await api(`/v1/groups/${encodeURIComponent(gid)}/messages?after_seq=${g.cursor}&limit=100`, { device: true });
        } catch (e) {
          if (e.status === 410) { await resyncLocked(gid); return { processed, resync: true }; }
          throw e;
        }
        const items = (res.items || []).slice().sort((a, b) => a.seq - b.seq);
        let stopped = false;
        for (const item of items) {
          if (upTo != null && item.seq > upTo) { stopped = true; break; }
          if (item.seq <= g.cursor) continue;
          const outcome = processItem(gid, item);
          g.cursor = item.seq;
          processed++;
          await persist();
          if (outcome === "removed") { emit({ type: "removed", groupId: gid }); return { processed, removed: true }; }
          if (outcome === "gap") { return { processed, gap: true }; }
          if (outcome === "commit") await ackEpoch(gid);
        }
        if (stopped || !res.more || !items.length) break;
      }
      if (processed) emit({ type: "history", groupId: gid });
      return { processed };
    }

    function isOwn(item) {
      return item.sender === mailbox && item.sender_device === record.deviceId;
    }

    // Returns "message" | "commit" | "notice" | "removed" | "gap" | "skipped".
    function processItem(gid, item) {
      if (item.type === "notice") {
        remember(gid, { kind: "notice", seq: item.seq, notice: item.kind, actor: item.actor || null, metadata: item.metadata || {}, created_at: item.created_at, expires_at: item.expires_at || null });
        return "notice";
      }
      if (item.type === "commit") {
        if (isOwn(item) && !device.hasPendingCommit(gid)) return "skipped"; // merged when it was accepted
        let r;
        try {
          r = device.processIncoming(gid, unb64(item.commit));
        } catch (e) {
          if (e && e.code === "future_epoch") return "gap";
          // A commit this device cannot verify: it stops at this epoch and fails closed (P-2 §3).
          remember(gid, { kind: "warning", seq: item.seq, warning: "commit_rejected", code: (e && e.code) || "mls", created_at: item.created_at });
          return "skipped";
        }
        if (r.type === "commit" || r.type === "ownCommitConfirmed") {
          const o = r.outcome;
          const verified = { committer: memberJson(o.committer), added: o.added.map(memberJson), removed: o.removed.map(memberJson), updated: o.updated.map(memberJson) };
          remember(gid, { kind: "commit", seq: item.seq, epoch: o.epoch, ...verified, created_at: item.created_at });
          if (!sameDevices(verified.added, item.adds) || !sameDevices(verified.removed, item.removes)) {
            remember(gid, { kind: "warning", seq: item.seq + 0.5, warning: "membership_mismatch", created_at: item.created_at });
          }
          if (o.selfRemoved) {
            try { device.forgetGroup(gid); } catch (_) { /* already gone */ }
            groupRec(gid).inGroup = false;
            return "removed";
          }
          return "commit";
        }
        return "skipped";
      }
      if (item.type === "application") {
        if (item.deleted_at || item.ciphertext == null) {
          remember(gid, { kind: "message", seq: item.seq, sender: item.sender, deleted: true, created_at: item.created_at, expires_at: item.expires_at || null, replaced_by: item.replaced_by || null });
          return "message";
        }
        if (isOwn(item)) {
          // Recorded when it was sent; the server copy only confirms its seq.
          const mine = history(gid).find((x) => x.kind === "message" && x.cmid === item.client_message_id);
          if (mine) { mine.seq = item.seq; mine.created_at = item.created_at; mine.expires_at = item.expires_at || null; mine.pending = false; }
          return "skipped";
        }
        let r;
        try {
          r = device.processIncoming(gid, unb64(item.ciphertext));
        } catch (e) {
          if (e && e.code === "future_epoch") return "gap";
          remember(gid, { kind: "message", seq: item.seq, sender: item.sender, undecryptable: true, code: (e && e.code) || "mls", created_at: item.created_at, expires_at: item.expires_at || null });
          return "message";
        }
        if (r.type !== "message") return "skipped";
        if (r.sender.mailbox !== item.sender) {
          remember(gid, { kind: "warning", seq: item.seq, warning: "sender_mismatch", created_at: item.created_at });
          return "skipped";
        }
        const doc = decodeBody(r.body);
        const refs = Array.from(r.attachments || []);
        const files = doc.files.map((f, i) => ({ ...f, ref: refs[i] ? { key: b64(refs[i].key), sha: b64(refs[i].sealedSha256), len: refs[i].plaintextLen } : null }));
        remember(gid, {
          kind: "message", seq: item.seq, sender: item.sender, senderDevice: item.sender_device, cmid: item.client_message_id,
          text: doc.text, files, reply_to: doc.reply_to, edit_of: doc.edit_of || (item.edit_of != null ? String(item.edit_of) : null),
          created_at: item.created_at, expires_at: item.expires_at || null,
        });
        if (item.edit_of != null) {
          const target = history(gid).find((x) => x.kind === "message" && x.seq === Number(item.edit_of));
          if (target) { target.replaced_by = item.seq; }
        }
        return "message";
      }
      return "skipped";
    }

    function sameDevices(verified, declared) {
      if (!Array.isArray(declared)) return true; // nothing declared to compare with
      const a = verified.map((d) => d.mailbox + "|" + d.device_id).sort();
      const b = declared.map((d) => d.mailbox + "|" + d.device_id).sort();
      return a.length === b.length && a.every((x, i) => x === b[i]);
    }

    async function resyncLocked(gid) {
      try { device.forgetGroup(gid); } catch (_) { /* not there */ }
      const g = groupRec(gid);
      g.inGroup = false;
      remember(gid, { kind: "warning", seq: g.cursor + 0.5, warning: "resynced", created_at: now() });
      await persist();
      await api(`/v1/groups/${encodeURIComponent(gid)}/resync`, { method: "POST", device: true, body: { request_key: newRequestKey() } }).catch(() => {});
      emit({ type: "resync", groupId: gid });
    }

    function sync(gid) {
      return serial(() => syncLocked(gid));
    }

    // -- commits -----------------------------------------------------------------------------------

    // Build, post and merge one commit. `build()` returns a commit bundle (or null for nothing to
    // do) and `declared` the adds/removes to state. On a lost race, discard, catch up, rebuild.
    async function commitLocked(gid, plan, { activation = false } = {}) {
      for (let attempt = 0; attempt < MAX_RETRIES; attempt++) {
        // A provisioning group has nothing to read yet (and its creator is not yet an active reader).
        if (!activation) await syncLocked(gid);
        if (!groupRec(gid).inGroup) return null;
        const prepared = await plan();
        if (!prepared) return null;
        const { bundle, adds, removes, revision } = prepared;
        await persist(); // the pending commit is part of the state
        try {
          const res = await api(`/v1/groups/${encodeURIComponent(gid)}/commits`, {
            method: "POST", device: true,
            body: {
              request_key: newRequestKey(), epoch: bundle.epoch, commit_id: bundle.commitId, commit: b64(bundle.commit),
              envelope_signature: b64(bundle.envelopeSignature), welcome: bundle.welcome ? b64(bundle.welcome) : null,
              adds, removes, expected_revision: revision,
            },
          });
          // Anything sequenced before this commit is still in the old epoch: read it before merging.
          if (!activation) await syncLocked(gid, res.seq - 1);
          const o = device.confirmCommit(gid);
          const g = groupRec(gid);
          g.cursor = Math.max(g.cursor, res.seq);
          remember(gid, { kind: "commit", seq: res.seq, epoch: o.epoch, committer: memberJson(o.committer), added: o.added.map(memberJson), removed: o.removed.map(memberJson), updated: o.updated.map(memberJson), created_at: now() });
          await persist();
          await ackEpoch(gid);
          emit({ type: "history", groupId: gid });
          return res;
        } catch (e) {
          try { device.discardCommit(gid); } catch (_) { /* already cleared */ }
          await persist();
          if (e.status === 409 && (e.code === "revision_conflict" || e.code === "epoch_conflict")) {
            await sleep(Math.min(10000, 200 * 2 ** attempt) + Math.floor(Math.random() * 100));
            continue;
          }
          throw e;
        }
      }
      throw new GroupApiError(409, "revision_conflict");
    }

    // Commit whatever the server says is pending: adds for keying members and new devices, removals
    // for departed members and revoked devices.
    async function processPending(gid, { noJitter = false } = {}) {
      let crypto = await api(`/v1/groups/${encodeURIComponent(gid)}/crypto`, { device: true });
      if (!crypto.in_group && crypto.activation_state === "active") return null;
      if (!crypto.pending || !crypto.pending.length) return null;
      if (!noJitter) {
        await sleep(jitter());
        crypto = await api(`/v1/groups/${encodeURIComponent(gid)}/crypto`, { device: true });
        if (!crypto.pending || !crypto.pending.length) return null;
      }
      return serial(() => commitLocked(gid, async () => {
        const fresh = await api(`/v1/groups/${encodeURIComponent(gid)}/crypto`, { device: true });
        const pending = fresh.pending || [];
        if (!pending.length) return null;
        const addMailboxes = [...new Set(pending.filter((p) => p.kind === "add").map((p) => p.mailbox))];
        const removes = pending.filter((p) => p.kind === "remove").map((p) => ({ mailbox: p.mailbox, device_id: p.device_id }));
        let claims = [];
        if (addMailboxes.length) {
          const claimed = await api(`/v1/groups/${encodeURIComponent(gid)}/key-packages/claim`, {
            method: "POST", device: true, body: { request_key: newRequestKey(), mailboxes: addMailboxes },
          });
          // Only devices the server still lists as pending; never this device itself.
          const wanted = new Set(pending.filter((p) => p.kind === "add").map((p) => p.mailbox + "|" + p.device_id));
          claims = (claimed.claims || []).filter((c) => wanted.has(c.mailbox + "|" + c.device_id) && !(c.mailbox === mailbox && c.device_id === record.deviceId));
        }
        if (!claims.length && !removes.length) return null;
        const bundle = device.commit(gid, claims.map((c) => unb64(c.key_package)), removes.map((r) => ({ mailbox: r.mailbox, deviceId: r.device_id })));
        return {
          bundle,
          adds: claims.map((c) => ({ mailbox: c.mailbox, device_id: c.device_id, key_package_ref: c.key_package_ref })),
          removes,
          revision: fresh.revision,
        };
      }));
    }

    function selfUpdate(gid) {
      return serial(() => commitLocked(gid, async () => {
        const fresh = await api(`/v1/groups/${encodeURIComponent(gid)}/crypto`, { device: true });
        return { bundle: device.selfUpdate(gid), adds: [], removes: [], revision: fresh.revision };
      }));
    }

    // -- creating ----------------------------------------------------------------------------------

    // Create the row, create the MLS group at epoch 0, and post the first commit (which activates
    // the group). The mailbox's other devices, if any, are added in that same commit.
    async function createGroup(settings) {
      const created = await opts.api("/v1/groups", { method: "POST", body: { request_key: newRequestKey(), ...settings } });
      const gid = created.group.id;
      await activate(gid);
      return created;
    }

    async function activate(gid) {
      return serial(async () => {
        if (!device.groups().includes(gid)) {
          device.createGroup(gid);
          const g = groupRec(gid);
          g.inGroup = true;
          await persist();
        }
        return commitLocked(gid, async () => {
          const fresh = await api(`/v1/groups/${encodeURIComponent(gid)}/crypto`, { device: true });
          if (fresh.activation_state === "active" && device.epoch(gid) > 0) return null;
          const adds = (fresh.pending || []).filter((p) => p.kind === "add" && !(p.mailbox === mailbox && p.device_id === record.deviceId));
          let claims = [];
          if (adds.length) {
            const claimed = await api(`/v1/groups/${encodeURIComponent(gid)}/key-packages/claim`, {
              method: "POST", device: true, body: { request_key: newRequestKey(), mailboxes: [...new Set(adds.map((a) => a.mailbox))] },
            });
            claims = (claimed.claims || []).filter((c) => !(c.mailbox === mailbox && c.device_id === record.deviceId));
          }
          const bundle = claims.length ? device.commit(gid, claims.map((c) => unb64(c.key_package)), []) : device.selfUpdate(gid);
          return { bundle, adds: claims.map((c) => ({ mailbox: c.mailbox, device_id: c.device_id, key_package_ref: c.key_package_ref })), removes: [], revision: fresh.revision };
        }, { activation: true });
      });
    }

    // -- sending -----------------------------------------------------------------------------------

    async function uploadFiles(gid, files) {
      const out = [], refs = [];
      for (const f of files || []) {
        const sealed = gc.sealAttachment(f.bytes);
        const res = await api(`/v1/groups/${encodeURIComponent(gid)}/attachments`, { method: "POST", device: true, rawBody: sealed.sealed });
        out.push({ id: res.id, name: f.name, mime: f.mime, size: f.bytes.length });
        refs.push(sealed.reference);
      }
      return { files: out, refs };
    }

    async function send(gid, doc) {
      const cmid = newClientMessageId();
      const { files, refs } = await uploadFiles(gid, doc.files);
      const body = encodeBody({ ...doc, files });
      const localFiles = files.map((f, i) => ({ ...f, ref: { key: b64(refs[i].key), sha: b64(refs[i].sealedSha256), len: refs[i].plaintextLen } }));
      return serial(async () => {
        const local = { kind: "message", seq: null, pending: true, sender: mailbox, senderDevice: record.deviceId, cmid, text: doc.text || "", files: localFiles, reply_to: doc.reply_to || null, edit_of: doc.edit_of != null ? String(doc.edit_of) : null, created_at: now(), mine: true };
        for (let attempt = 0; attempt < MAX_RETRIES; attempt++) {
          await syncLocked(gid);
          if (!groupRec(gid).inGroup) throw new GroupApiError(403, "not_member");
          const out = device.encryptMessage(gid, cmid, body, refs);
          await persist();
          try {
            const res = await api(`/v1/groups/${encodeURIComponent(gid)}/messages`, {
              method: "POST", device: true,
              body: {
                request_key: "rk_" + cmid, client_message_id: cmid, epoch: out.epoch, enc_scheme: ENC_SCHEME,
                ciphertext: b64(out.ciphertext), envelope_signature: b64(out.envelopeSignature),
                attachments: files.map((f) => f.id),
                ...(doc.reply_to != null ? { reply_to: Number(doc.reply_to) } : {}),
                ...(doc.edit_of != null ? { edit_of: Number(doc.edit_of) } : {}),
                ...(doc.mentions && doc.mentions.length ? { mentions: doc.mentions } : {}),
              },
            });
            local.seq = res.seq; local.pending = false; local.expires_at = res.expires_at || null;
            remember(gid, local);
            if (doc.edit_of != null) {
              const target = history(gid).find((x) => x.kind === "message" && x.seq === Number(doc.edit_of));
              if (target) target.replaced_by = res.seq;
            }
            await persist();
            emit({ type: "history", groupId: gid });
            return res;
          } catch (e) {
            if (e.status === 409 && e.code === "epoch_conflict") { await sleep(50 * (attempt + 1)); continue; }
            if (e.status === 409 && e.code === "key_activation_pending") {
              // A removal is waiting for a commit. Any member device may make it; this one does.
              await commitPendingLocked(gid);
              continue;
            }
            throw e;
          }
        }
        throw new GroupApiError(409, "epoch_conflict");
      });
    }

    // processPending without re-entering the serial queue (we already hold it).
    async function commitPendingLocked(gid) {
      return commitLocked(gid, async () => {
        const fresh = await api(`/v1/groups/${encodeURIComponent(gid)}/crypto`, { device: true });
        const removes = (fresh.pending || []).filter((p) => p.kind === "remove").map((p) => ({ mailbox: p.mailbox, device_id: p.device_id }));
        if (!removes.length) return null;
        return { bundle: device.commit(gid, [], removes.map((r) => ({ mailbox: r.mailbox, deviceId: r.device_id }))), adds: [], removes, revision: fresh.revision };
      });
    }

    async function openFile(gid, file) {
      const res = await opts.api(`/v1/groups/${encodeURIComponent(gid)}/attachments/${encodeURIComponent(file.id)}`, { raw: true, device: record.deviceId });
      const sealed = new Uint8Array(res);
      return gc.openAttachment(sealed, { key: unb64(file.ref.key), sealedSha256: unb64(file.ref.sha), plaintextLen: file.ref.len });
    }

    // -- local housekeeping ------------------------------------------------------------------------

    // Drop expired items locally, by this device's own clock, whether or not the event arrived.
    async function pruneExpired() {
      if (!record) return 0;
      const t = now();
      let dropped = 0;
      for (const gid of Object.keys(record.history || {})) {
        const list = record.history[gid];
        const kept = list.filter((x) => !(x.expires_at && x.expires_at <= t));
        dropped += list.length - kept.length;
        record.history[gid] = kept;
      }
      if (dropped) await serial(() => storage.save(mailbox, { ...record, stateBlob: device.exportState(record.stateKey) }));
      return dropped;
    }

    async function dropSeqs(gid, seqs) {
      const set = new Set((seqs || []).map(Number));
      return serial(async () => {
        record.history[gid] = history(gid).filter((x) => !(x.kind === "message" && set.has(x.seq)));
        await persist();
      });
    }

    async function forget(gid) {
      return serial(async () => {
        try { device.forgetGroup(gid); } catch (_) { /* not there */ }
        if (record.groups) delete record.groups[gid];
        if (record.history) delete record.history[gid];
        await persist();
      });
    }

    function members(gid) {
      try { return Array.from(device.members(gid)).map(memberJson); } catch (_) { return []; }
    }
    function safetyNumber(gid) {
      try { return device.safetyNumber(gid); } catch (_) { return null; }
    }
    function epoch(gid) {
      try { return device.epoch(gid); } catch (_) { return null; }
    }

    return {
      init, topUpKeyPackages, processWelcomes, sync, processPending, selfUpdate, createGroup, activate, send, openFile,
      pruneExpired, dropSeqs, forget, members, safetyNumber, epoch, reEnrol,
      inGroup: (gid) => Boolean(record && record.groups && record.groups[gid] && record.groups[gid].inGroup),
      cursor: (gid) => (record && record.groups && record.groups[gid] ? record.groups[gid].cursor : 0),
      history: (gid) => (record && record.history && record.history[gid] ? record.history[gid].slice() : []),
      deviceId: () => record && record.deviceId,
      fingerprint: () => (device ? gc.fingerprintDigits(device.fingerprint) : null),
      mailbox,
      // Tests only: the raw device, to prove what it can and cannot decrypt.
      _device: () => device,
    };
  }

  function timerLabel(secs) {
    if (!secs) return "Off";
    const table = { 300: "5 minutes", 3600: "1 hour", 28800: "8 hours", 86400: "1 day", 604800: "1 week", 2419200: "4 weeks" };
    return table[secs] || `${Math.round(secs / 60)} minutes`;
  }

  const api = { ENC_SCHEME, TIMERS, timerLabel, b64, unb64, hex, encodeBody, decodeBody, memoryStorage, idbStorage, createClient, GroupApiError, randomId };
  root.PigeonpostGroupsCore = api;
  if (typeof module !== "undefined" && module.exports) module.exports = api;
})(typeof window !== "undefined" ? window : globalThis);
