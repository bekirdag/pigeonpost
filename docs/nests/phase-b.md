# Pigeonpost Nests — Phase B (first-party Nests)

**Status:** in progress behind the `nests` flag (off in production). No sales: commercial launch
needs the owner's approval of the catalog and prices, updated privacy pages and store review.
**Plan:** `docs/planning/2026-11-03-nests-marketplace-build-plan.md` §8 (local planning folder).
**Before:** [phase-a-plan.md](phase-a-plan.md) (runtime, broker, enrollment), [backups.md](backups.md).

## B1 — purchases and entitlements

### Catalog

`crates/pigeonpost-postbox/src/nest_catalog.rs`, edition `nests-2026-11`. A package is one monthly
subscription; every rail maps onto the same table.

| Package | Slots | Storage (pooled) | Hours/month (pooled) | Managed AI / month (pooled) | Offered |
|---|---|---|---|---|---|
| `nest.ai.starter.monthly` — Pigeonpost AI | 1 | 5 GiB | 10 | 1M input + 100k output | yes |
| `nest.ai.plus.monthly` — Pigeonpost AI Plus | 3 | 20 GiB | 40 | 5M + 500k | yes |
| `nest.ai.pro.monthly` — Pigeonpost AI Pro | 10 | 100 GiB | 120 | 15M + 1.5M | no: does not fit nests-1 |
| `nest.host.starter.monthly` — Empty Nest | 1 | 5 GiB | 10 | none (no model connected) | yes |

| Rail | Product |
|---|---|
| App Store | `dev.pigeonpost.inbox.nests.<ai\|host>.<tier>.monthly` in the **Nests** subscription group (separate from the handle group) |
| Google Play | subscription `pigeonpost.nests.<ai\|host>.<tier>`, base plan `monthly` |
| Web (iyzico Subscription V2 via the billing bridge) | plan codes `nest-<ai\|host>-<tier>-monthly-<usd\|eur\|try>` |

Prices are not in the code. Store prices live in App Store Connect and Play Console, web prices in
the billing bridge catalog; all are **proposals pending owner approval**. Model allowances are split
evenly across a package's slots (the broker enforces per-Nest budgets), never multiplied by them.
Workspace hours are shown, not yet metered (B5).

### Flow

1. The client reads `GET /v1/nest-packages` (catalog, available rails, the account's StoreKit
   `appAccountToken` and Google `obfuscatedAccountId`).
2. Before any purchase sheet: `POST /v1/nest-purchases/intents {package, rail}`. It refuses a second
   charge: a live package bought on another rail (`nest_package_active`) or the same package again
   (`nest_package_owned`). Upgrades on the funding rail are allowed.
3. The store purchase carries the binding token. The client sends the transaction id
   (`POST /v1/nest-purchases/apple`) or purchase token (`POST /v1/nest-purchases/google`); the
   postbox verifies it with Apple/Google, checks the binding, records it in `nest_store_purchases`
   and mirrors it into `entitlements` (plan code = package code). Google purchases are acknowledged
   only after they are durable, and **not** when they duplicate a live package (Google refunds an
   unacknowledged purchase after three days). Web purchases go through the bridge, which writes
   `nest-…` entitlements with the internal API.
4. The first Nest is set up at once. If that fails (nestd down, no disk), the purchase stays as a
   **paid setup** (`nest_setups`), retried with backoff by the reconciler and by the owner
   (`POST /v1/nest-setups/{id}/retry`). One setup per funding entitlement, ever.

### Allowance and lifecycle

- Slots come only from live Nest entitlements; the flag gates the feature but never grants a slot.
  One package funds an account (highest rank wins); others are listed as `duplicates` and never add.
- A failed provisioning attempt holds no slot; the sweep tears its half-made runtime and mailbox down.
- Every 60 s the reconciler re-reads store subscriptions (each at most every 6 h) so renewals, grace,
  refunds and upgrades land while every app is closed; then the funding sweep:
  - over the allowance after a **downgrade**: the newest excess Nests pause (`over_capacity`), never deleted;
  - **no live package** (expiry, refund, revocation): every package Nest pauses (`unfunded`) at once;
    after a 30-day export window from the last paid-through it is deleted;
  - funding back (renewal, restore, upgrade): Nests paused for funding resume.
- Phase A Nests created through the ops path (`funding = ops`) are never touched by the sweep.

### Security notes

- Buying, restoring, creating and retrying need a person (`Requirement::Human`): never an API key,
  a mailbox token, a connector or an org key. Reading needs the account's own credential.
- The client never says what it bought or what it costs: product ids and plan codes are mapped by
  the server, unknown `nest…` codes are refused by the internal API.
- A store subscription is bound to the first account that proves it (`purchase_already_used`);
  Apple purchases must carry the account's `appAccountToken`, Google purchases its obfuscated id.
- Replaced (upgraded) Google tokens are retired and cannot be revived by a late verification;
  refunds cut the paid-through to the refund time.

## B3 — chat with a Nest

- **Verb.** Plain prose to a Nest mailbox from a sender its owner granted `chat` is decided `auto`
  with verb `chat` (`verbs::decide_for`). It applies only when all of these hold:
  - the recipient is a live Nest;
  - the body is not a request envelope;
  - it is 1–8000 characters.

  Anywhere else prose stays held for a person. The `chat` grant itself is refused on a mailbox
  that is not a Nest. A new Nest grants its owner's own mailboxes `answer_question` and `chat`;
  strangers get nothing, so their mail stays data.
- **agentd.** `chat` is runnable only on a `runtime = "nest"` route; a runtime with tools never
  takes prose as work. The Nest runner's route enables `answer_question` and `chat`, read-only,
  one run at a time.
- **Runner.** A chat turn arrives as structured JSON (`nest_turn: 1`). The runner sends:
  - a chat system prompt;
  - the stored conversation with that sender;
  - the new text in the user role.

  History is one file per sender (hashed address) on the Nest's encrypted volume: the newest 40
  turns, at most 24k characters replayed. A replied-to message and an edit notice ride in the
  system role as labelled data.
- **No-AI profile.** A Nest with no model allowance (Empty Nest) gets `402 no_model` from the
  broker, and the runner replies that no AI is connected. It never says the allowance ran out.
- **Status header.** `GET /v1/nests/{nest}` (and each item of `GET /v1/nests`) carries `status`:
  - `ready`, which includes idle-stopped;
  - `working`, when a run reported `working` in the last 15 minutes;
  - `needs_you`, when it is unfunded (`payment`);
  - `paused`;
  - `unavailable`, while setting up or after a failure.
- **Model route.** The broker upstream is configuration (`upstream_url`, `model`). Today it is
  mswarm's self-hosted load balancer with `qwen-latest-flash-next`, the owner's choice "for now".
- **End to end:** `tests/nests/e2e.mjs chat` (two turns; with a real model the codeword must be
  remembered) and `tests/nests/e2e.mjs stranger` (no answer in 30 s).

## B4 — files and tools

- **Upload.** `POST /v1/nests/{nest}/files`: a person only; at most 25 MiB, 60 uploads an hour,
  1000 files per Nest, within the Nest's storage quota. The name is display text and never a path.
  The bytes go to nestd over the private link and are stored as `files/quarantine/<id>` on the
  Nest's encrypted volume.
- **Parser sandbox.** nestd parses every file in a separate gVisor container from the runner image
  (`pigeonpost-nest-parser`):
  - no network, read-only root, uid 10001, no capabilities;
  - the input read-only at `/in`, an empty output directory at `/out`;
  - 256 MiB memory, 16 processes, 1 CPU, killed after 120 s.

  It never runs inside the Nest's own sandbox, so a parser exploit cannot reach the Nest's mailbox
  or broker credentials.
- **Formats and bounds** (`crates/pigeonpost-nests/src/parse.rs`).
  - The kind comes from the bytes: text/Markdown, CSV, PDF (a small FlateDecode/text-operator
    extractor; encrypted files are refused), DOCX, XLSX, ZIP.
  - Limits: 2M output characters; ZIP at most 500 entries, 100 MiB inflated, a 200:1 ratio per
    entry, one nesting level. Lying size headers, links and unsafe entry names (`..`, absolute,
    drive, backslash) are skipped.
  - Output names are always `part-NNNN.txt`.
  - nestd then verifies the output: the manifest parses, there are no stray files or links, and the
    size is bounded. Otherwise it discards everything.
  - Coverage (pages/sheets/entries with text) and warnings are reported to the person.
- **Using files.** In chat, the runner adds the best-matching excerpts from `files/parsed/` to the
  system turn, fenced as untrusted file content with the file's name. File text is never the user's
  turn, and the model is told never to follow instructions inside files.
- **Tools and approvals.** A Nest never performs an external write.
  - The runner may end a reply with one ```` ```pigeonpost-action ```` block. It turns that into a
    proposal (`POST /v1/nest-approvals`, the Nest's mailbox credential) and tells the person that
    approval is needed.
  - Tool vocabulary: `send_message {to, body}` only. The never-auto categories do not exist as tools.
  - The card binds `args_hash` (SHA-256 of the canonical call). A newer proposal for the same
    recipient supersedes the pending one, which is how an edited request invalidates an old approval.
  - Approving needs a person and the hash they saw. Cards are single-use and expire after 24 h; at
    most 20 are open per Nest.
  - The postbox carries the call out as the tool broker under its destination policy: the owner's
    own mailboxes or contacts they allowed, never a wildcard or the Nest itself; 50 sends a day.
- **Tests.**
  - Archive traversal names, bombs, nesting, entry limits, hostile bytes.
  - Output verification (strays, links, missing parts).
  - The parser sandbox profile.
  - Prompt injection: a file's instruction produces at most a card the destination policy refuses.
  - Approval binding: superseded, changed hash, single use, automation and other people refused.

## B5 — management

| Route | Who | What |
|---|---|---|
| `POST /v1/nests/{nest}/pause`, `/resume` | the owner, a person | Pause holds mail until resume. A Nest paused for funding (`needs_payment`) or over its package (`over_capacity`) cannot be resumed by its owner; the funding sweep resumes it once it is paid for. The sweep never lifts an owner's own pause. |
| `PATCH /v1/nests/{nest}` | the owner, a person | Rename. |
| `DELETE /v1/nests/{nest}` | the owner, a person | Requires `{confirm: <label>}`. Runtime, volume and mailbox credential are destroyed, open approvals closed, the slot freed. Deleting one's last Nest never brings a new one back. |
| `GET /v1/nests/{nest}/usage` | the account | This month's model tokens (used/limit), requests and running seconds, taken from the Nests ledger, plus stored file bytes and the package's pooled hours. |
| `GET /v1/nests/{nest}/export` | the owner, a person | A zip built on the Nests host from the Nest's own volume: `files/` originals under sanitized, de-duplicated display names, `text/` parsed text, `conversations/`, `index.json`. It is built in memory, never on the unencrypted host disk, one export at a time, at most 512 MiB of files. |

Clients show files, usage and pause/delete with export on every platform (client work follows the
server PRs).

## B6 — data safety

- **Backups:** borg append-only to raijin's HDD over WireGuard, nightly ([backups.md](backups.md));
  this replaces the plan's "restic to a Wodo target". Each archive now writes a success marker.
- **Off-host LUKS unlock:** volume keys are wrapped (ChaCha20-Poly1305, bound to the Nest id)
  under a host KEK held only by the postbox (`POSTBOX_NESTS_KEK_FILE`). The postbox releases it
  every minute over WireGuard (`POST /v1/unlock`). nestd is locked until then; a wrong KEK is
  refused by fingerprint. A copy of nests-1's disk, a VMware snapshot or a backup no longer
  contains a usable key. A live compromise of running nestd (which holds the KEK in memory) is not
  covered, and that is stated plainly.
- **Tombstones:** each unlock carries the postbox's deleted Nest ids; nestd destroys any it still
  has, so a restore cannot bring a revoked Nest back.
- **Monitoring:** nestd `/metrics` on loopback (no ids or content). Prometheus, Alertmanager and
  node_exporter run on nests-1; alerts arrive as Pigeonpost messages through the alert relay.
  Rules are in `deploy/nests/monitoring/`.
- **Restore rehearsal:** RPO ≤ 24 h (nightly), RTO ≤ 4 h. Rehearsed on deploy; evidence is in the ledger.
