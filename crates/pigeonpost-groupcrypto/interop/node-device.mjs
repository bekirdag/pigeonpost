// WASM interop agent: the exact vendored web bundle (site-inbox/vendor/groupcrypto), loaded
// through its integrity-checking loader, driven from Node. Protocol: interop/run_interop.py.
import { readFile } from 'node:fs/promises';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';

const vendor = new URL('../../../site-inbox/vendor/groupcrypto/', import.meta.url);
const { loadGroupCrypto } = await import(new URL('loader.js', vendor));
const gc = await loadGroupCrypto({
  wasmBytes: await readFile(fileURLToPath(new URL('pigeonpost_groupcrypto_bg.wasm', vendor))),
});

const b64 = (u8) => Buffer.from(u8).toString('base64');
const unb64 = (s) => new Uint8Array(Buffer.from(s, 'base64'));
let device = null;

const commitJson = (b) => ({
  epoch: b.epoch,
  commit_id: b.commitId,
  commit: b64(b.commit),
  welcome: b.welcome ? b64(b.welcome) : null,
  signature: b64(b.envelopeSignature),
});
const attJson = (a) => ({ key: b64(a.key), sha: b64(a.sealedSha256), len: a.plaintextLen });
const attIn = (a) => ({ key: unb64(a.key), sealedSha256: unb64(a.sha), plaintextLen: a.len });

function incomingJson(r) {
  switch (r.type) {
    case 'message':
      return {
        type: 'message',
        body: b64(r.body),
        sender_mailbox: r.sender.mailbox,
        sender_device: r.sender.deviceId,
        client_message_id: r.clientMessageId,
        epoch: r.epoch,
        attachments: r.attachments.map(attJson),
      };
    case 'commit':
      return {
        type: 'commit',
        epoch: r.outcome.epoch,
        self_removed: r.outcome.selfRemoved,
        committer_mailbox: r.outcome.committer.mailbox,
        added: r.outcome.added.length,
        removed: r.outcome.removed.length,
      };
    case 'ownCommitConfirmed':
      return { type: 'own_commit_confirmed', epoch: r.outcome.epoch };
    case 'historyOffer':
      return { type: 'history_offer' };
    default:
      return { type: 'own' };
  }
}

function handle(req) {
  const g = req.group;
  switch (req.op) {
    case 'init': {
      device = gc.GroupDevice.newWithIdentitySeed(unb64(req.seed), req.device);
      return { mailbox: device.mailbox, signature_key: b64(device.signaturePublicKey) };
    }
    case 'key_packages': {
      const kps = device.generateKeyPackages(req.count);
      kps.push(device.generateLastResortKeyPackage());
      return { key_packages: kps.map(b64) };
    }
    case 'create':
      return { epoch: device.createGroup(g).epoch };
    case 'add':
      return commitJson(device.addMembers(g, req.key_packages.map(unb64)));
    case 'remove_mailbox':
      return commitJson(device.removeMailbox(g, req.mailbox));
    case 'self_update':
      return commitJson(device.selfUpdate(g));
    case 'confirm':
      return { epoch: device.confirmCommit(g).epoch };
    case 'discard':
      device.discardCommit(g);
      return {};
    case 'join': {
      const j = device.joinFromWelcome(unb64(req.welcome));
      return { group: j.groupId, epoch: j.epoch, members: j.members.length };
    }
    case 'encrypt': {
      const atts = [];
      let sealed = null;
      if (req.attachment) {
        const s = gc.sealAttachment(unb64(req.attachment));
        sealed = b64(s.sealed);
        atts.push(s.reference);
      }
      const m = device.encryptMessage(g, req.id, unb64(req.body), atts);
      return { ciphertext: b64(m.ciphertext), signature: b64(m.envelopeSignature), epoch: m.epoch, sealed };
    }
    case 'process':
      return incomingJson(device.processIncoming(g, unb64(req.message)));
    case 'open_attachment':
      return { plaintext: b64(gc.openAttachment(unb64(req.sealed), attIn(req.ref))) };
    case 'status':
      return {
        epoch: device.epoch(g),
        epoch_authenticator: b64(device.epochAuthenticator(g)),
        safety_number: device.safetyNumber(g),
        members: device.members(g).length,
      };
    case 'persist': {
      const key = gc.generateStateKey();
      const blob = device.exportState(key);
      const restored = gc.GroupDevice.importState(blob, key);
      device.free();
      device = restored;
      return { bytes: blob.length };
    }
    case 'snapshot': {
      const key = gc.generateStateKey();
      return { state: b64(device.exportState(key)), key: b64(key) };
    }
    case 'probe_snapshot': {
      const d = gc.GroupDevice.importState(unb64(req.state), unb64(req.key));
      try {
        return { code: 'ok', result: incomingJson(d.processIncoming(g, unb64(req.message))) };
      } catch (e) {
        return { code: e.code };
      } finally {
        d.free();
      }
    }
    default:
      throw Object.assign(new Error(`unknown op ${req.op}`), { code: 'invalid_input' });
  }
}

const rl = createInterface({ input: process.stdin });
for await (const line of rl) {
  const req = JSON.parse(line);
  if (req.op === 'quit') break;
  let resp;
  try {
    resp = { ok: true, ...handle(req) };
  } catch (e) {
    if (!e.code) throw e;
    resp = { ok: false, code: e.code, message: e.message };
  }
  process.stdout.write(JSON.stringify(resp) + '\n');
}
process.exit(0);
