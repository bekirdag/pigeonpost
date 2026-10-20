// Durable bridge state in one JSON file, written atomically (temp file + rename) and serialized
// through a single queue. The volume is tiny (a few checkout intents per account), so a file is
// enough; what matters is that an intent survives a restart before saas_be is called, so a lost
// response is replayed with the same idempotency key instead of starting a second subscription.
//
// Shape:
//   intents[intent_id]  = { kind, account, plan_code, ..., subscription_id, status, created_at }
//   refs[subscription]  = { kind, account|draft_id, plan_code, paid_through, status, version, pushed }
// No card data, checkout tokens or form contents are ever stored here.

import { mkdirSync, readFileSync, renameSync, writeFileSync, openSync, fsyncSync, closeSync } from "node:fs";
import { join } from "node:path";

export class State {
  constructor(dir) {
    this.dir = dir;
    this.file = join(dir, "billing-state.json");
    this.queue = Promise.resolve();
    this.data = { version: 1, intents: {}, refs: {} };
    try {
      mkdirSync(dir, { recursive: true, mode: 0o700 });
      const parsed = JSON.parse(readFileSync(this.file, "utf8"));
      if (parsed?.version === 1 && parsed.intents && parsed.refs) this.data = parsed;
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
  }

  /// Run `fn(data)` with exclusive access and persist the result before resolving.
  update(fn) {
    const run = this.queue.then(() => {
      const result = fn(this.data);
      const tmp = `${this.file}.${process.pid}.tmp`;
      writeFileSync(tmp, JSON.stringify(this.data), { mode: 0o600 });
      const fd = openSync(tmp, "r");
      try { fsyncSync(fd); } finally { closeSync(fd); }
      renameSync(tmp, this.file);
      return result;
    });
    this.queue = run.catch(() => {});
    return run;
  }

  read(fn) {
    return this.queue.then(() => fn(this.data));
  }
}
