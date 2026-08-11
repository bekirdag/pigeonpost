# `ppcompliance` operations and adapter contract

This is the public operator runbook for the offline Pigeonpost custody, disclosure, hold, and
destruction binary. It documents the contract implemented by the current Rust source; it does not
contain deployment-specific hosts, keys, identities, or case data.

| Contract | Required version |
| --- | ---: |
| This runbook | 1 |
| Operator configuration | 2 |
| Private stdin declaration | 1 |
| Inventory declaration | 1 |
| Approval adapter protocol | 1 |
| Custody adapter protocol | 1 |
| Destruction adapter protocol | 1 |
| Retention policy | 1 |

The implementation sources are the final compatibility authority:

- [`operator.rs`](../crates/pigeonpost-compliance/src/operator.rs) defines the configuration,
  command, subprocess, privacy, and wire contracts.
- [`retention.rs`](../crates/pigeonpost-compliance/src/retention.rs) defines inventory, retention,
  hold, approval-signature, and destruction state.
- [`pigeonpost-compliance-format`](../crates/pigeonpost-compliance-format/src/lib.rs) defines typed
  key ids and epoch boundaries.
- [`trace_epoch.rs`](../crates/pigeonpost-compliance/src/trace_epoch.rs) and the
  [`pigeonpost-compliance-seal` crate](../crates/pigeonpost-compliance-seal/src/lib.rs) define
  authenticated trace artifacts.
- [`ledger.rs`](../crates/pigeonpost-compliance/src/ledger.rs) defines the public disclosure ledger
  and authenticated restart sidecar.

An adapter or configuration that relies on behavior not stated here and not enforced by those
sources is unsupported. Unknown fields, versions, enum values, lengths, trailing bytes, unsafe file
identity, and invalid state transitions fail closed.

## Scope and activation gates

`ppcompliance` is an offline-only binary. Its dependency closure contains no network client, server
framework, async runtime, or database engine. Online nodes use only the sealing/format crates and
cannot invoke this decrypt path. The operator is distributed as a separately named, checksummed,
attested native release asset; it is not part of the npm launcher or online container image.

Operational custody is supported only on Linux and macOS. There is no supported Windows custody
asset. `--help` and `--version` can return without loading custody state, but every real operation
rejects an unsupported platform before touching configuration, stdin, or persistent state.

Source code cannot satisfy the following production prerequisites. Do not activate regulated
capture or attribution escrow until the responsible organization has recorded evidence for all of
them:

1. Counsel has resolved the open classification and territorial questions in
   [`law.md` §8](law.md#8-open--counsel-decides-not-us), approved the intended response procedure,
   and selected the Türkiye retention period in the allowed 365–730-day band. The configuration
   stores only a commitment to that decision, not the decision record itself.
2. The EU designated establishment or legal representative described in
   [`law.md` §6](law.md#6-operating-this) is notified to a member state, accepted languages are
   declared, and a human can be reached inside eight hours. The SDS records 18 August 2026 as the
   deadline.
3. One published legal-process intake route and an issuing-authority authentication procedure are
   operating. An attached document is untrusted until independently authenticated.
4. Purpose- and jurisdiction-separated compliance keypairs are provisioned in segregated external
   custody. Türkiye-related custody or data transfer receives the separate review required by
   `law.md`.
5. At least two independently controlled approval keys and their human procedures are operating.
   Automated signing is not dual control.
6. A pinned, independently administered registry witness roster and a justified threshold/fault
   model are operating. A strict majority proves same-roster intersection; it does not prove that a
   witness is honest.
7. A separate publisher and monitor retain disclosure checkpoints outside the custody host and
   reject any regression or fork. Coordinated rollback of the local ledger, sidecar, and handoff is
   otherwise locally indistinguishable.
8. The complete key-copy inventory covers live metadata, SQLite WAL, sidecars, snapshots, backups,
   KMS versions, and Shamir shares. Every declared copy id is mapped to its real destruction target
   inside the external custody procedure.

This runbook describes software behavior, not legal advice. A production response discloses only
records matched to the authenticated order and states the subset of operated nodes it covers. A
requester is never given a key.

## Trust and data flow

```text
protected case system --bounded private stdin--> ppcompliance
                                             |--> approval adapter --> two human-held keys
offline registry dump + witnessed checkpoint --> registry verifier
sealed epoch artifacts -----------------------> artifact verifier
                                             |--> custody adapter --> segregated KMS/HSM
                                             |--> destruction adapter --> every inventoried copy
                                             |--> encrypted private audit records
                                             |--> append-only disclosure ledger + .state
                                             `--> signed checkpoint handoff --> external publisher/monitor
```

Only the approval adapter receives raw order reference, requester identity, and selector/scope
bytes. The custody adapter receives a request id, typed key id, configured public key, and ephemeral
X25519 peer key. The destruction adapter receives only a typed key id, opaque copy id, and copy-kind
number. Neither custody nor destruction receives a raw case selector or raw storage locator.

## Install and filesystem custody

Verify the release checksum and artifact attestation before transfer into the offline environment,
then verify them again after transfer. Do not substitute a locally built online binary or the npm
launcher for the separate `ppcompliance` asset.

Set `PIGEONPOST_COMPLIANCE_HOME` to one absolute directory. It is the only environment variable the
operator uses to locate its configuration. For the public example, it would be:

```text
PIGEONPOST_COMPLIANCE_HOME=/srv/pigeonpost-compliance
```

The home and private parent directories must be owner-only (mode `0700` on Unix), and private files
must be owner-only (mode `0600`). `config.toml` is read from the home and is limited to 256 KiB. All
configured paths must be absolute and cannot contain `.` or `..` components. The four inventory
paths for every epoch must be distinct, live below the canonical home, and not alias any configured
artifact, secret, ledger, checkpoint, adapter, lock, or another inventory path.

Adapter executables must satisfy all of these checks at configuration load and again around spawn:

- absolute path; regular executable file; no final symlink;
- no group or world write permission;
- owned by root or the effective operator uid;
- if owned by the operator rather than root, exactly one hard link;
- stable named-file identity before and after process creation.

Do not place an adapter in a directory writable by an unrelated account. The executable checks do
not turn an unsafe operational account, device daemon, KMS policy, or adapter implementation into a
trusted boundary.

Provision an authenticated, accurate UTC clock inside the offline boundary. Registry witness
freshness, approval timestamps, key validity, retention, legal-hold expiry, and date-based shred
selection all depend on that clock; the binary has no network time client.

## Operator configuration v2

Start from the checked-in
[`ppcompliance-config-v2.toml`](examples/ppcompliance-config-v2.toml). It is intentionally
non-operational until every `REPLACE_*` value is supplied through the provisioning ceremony.
Configuration structs reject unknown fields. There is no compatibility interpretation for
configuration v1 and no `validate-config` subcommand; validate a candidate in an isolated custody
fixture before atomically installing it as `config.toml`.

### Top-level fields

| Field | Contract |
| --- | --- |
| `version` | Required integer `2`. |
| `ledger_path` | Append-only public disclosure log, bounded to 512 MiB and 100,000 leaves. Its companion path is the same filename with `.state` appended. |
| `private_audit_directory` | Destination for encrypted `<request-id>.ppaudit` records. |
| `private_audit_key_path` | Owner-only file containing exactly one nonzero 32-byte raw key. |
| `checkpoint_origin` | Nonempty public disclosure-checkpoint origin, at most 256 bytes and without control bytes. |
| `checkpoint_signing_key_path` | Owner-only file containing exactly one nonzero 32-byte Ed25519 seed. The same bytes authenticate the ledger restart sidecar. |
| `checkpoint_output_path` | Owner-only atomic handoff containing the latest signed disclosure checkpoint; read bound 64 KiB. |
| `registry_audit` | One complete offline registry verification policy. |
| `approval` | One exact two-signature approval policy. |
| `destruction_command` | Optional at parse time; required when `shred --execute` has an eligible epoch. |
| `epochs` | Between 1 and 4,096 unique canonical key-id entries. |

The ledger, sidecar, private-audit material, signing key, handoff, registry inputs, adapter
executables, artifacts, custody secrets, and inventory paths must remain distinct. A configuration
reload happens for every real command.

### Registry audit

| Field | Contract |
| --- | --- |
| `log_path` | Owner-only complete registry NDJSON dump; at most 512 MiB, 1,000,000 sequential entries, and 64 KiB per line. Lines end with LF, never CRLF. |
| `checkpoint_path` | Owner-only final signed and witnessed registry checkpoint; at most 64 KiB. |
| `expected_origin` | Nonempty exact registry origin, at most 256 bytes and without whitespace/control bytes. |
| `checkpoint_key` | Canonical lowercase-hex 32-byte Ed25519 public key. |
| `witnesses` | Between 1 and 32 unique name/public-key pairs; each key is valid Ed25519 and differs from the checkpoint key. |
| `witness_threshold` | Nonzero strict majority: `2 * threshold > witness_count`. |
| `minimum_checkpoint_size` / `minimum_checkpoint_root` | Independently retained historical prefix floor. Size zero is valid only with the RFC 6962 empty root; a nonzero size cannot use that root. |
| `max_cosignature_age_seconds` | Positive freshness window for the final witness quorum. |
| `future_clock_skew_seconds` | Accepted future skew, no greater than the freshness window. |

Before any approval or unwrap, the operator streams the whole dump, verifies exact sequence and the
final root, recomputes the pinned prefix root, verifies the registry signature and fresh witness
threshold, and replays every compliance-key state transition. It accepts an initially `Active` key,
allows `Active -> Retired|Revoked` and `Retired -> Revoked`, rejects an invalid history, refuses a
currently `Revoked` key, and retains `Retired` only for historical disclosure within its signed
validity interval. At most 4,096 unique compliance keys are retained during replay. A projected key
list is not a valid input because it could omit a later revocation.

A threshold of 2 among 3 witnesses is valid for same-roster intersection. It provides no-gossip
fork resistance only under the separately justified bound `f < 2k - N`; use N-of-N when the only
defensible assumption is that at least one of N witnesses is honest. Roster changes require an
external continuity ceremony.

### Approval policy and command blocks

`approval.request_ttl_ms` is 1 through 3,600,000. The roster contains 2 through 32 distinct valid
Ed25519 public keys. Each configured identity is nonempty, at most 4 KiB, and contains no NUL. The
operator always requires exactly two distinct roster signatures for disclosure and for hold place,
renew, or release; this count is not configurable.

Every `approval.command`, optional `destruction_command`, and external `epochs.custody.command` has
the same strict schema:

| Field | Contract |
| --- | --- |
| `executable` | Absolute path satisfying the executable custody checks above. |
| `args` | Optional fixed array, at most 32 values; each is at most 4,096 bytes and contains no NUL. Never put case data or locators here. |
| `timeout_ms` | 100 through 300,000 milliseconds. |
| `inherit_environment` | Optional allowlist of at most 16 unique names. Each is 1–128 bytes and contains only `A-Z`, `0-9`, or `_`. |

An empty environment allowlist is the preferred baseline. If a device selector must be inherited,
pass only its variable name and ensure its value is not case data, a private locator, or key
material.

### Epoch fields

Every `[[epochs]]` block describes exactly one key id and one purpose. Network and identity records
must never share an epoch, artifact directory, inventory, or custody authorization.

| Field | Contract |
| --- | --- |
| `key_id` | Exactly 47 canonical bytes encoded as 94 lowercase hexadecimal characters. |
| `inventory_path` | Active PPinv state used by all operations. |
| `inventory_declaration_path` | Strict private TOML input for create/update. |
| `inventory_staging_path` | Atomic no-replace output of create and input of provision. |
| `inventory_import_path` | Existing PPinv input for import. |
| `retention_policy.version` | Required integer `1`. |
| `retention_policy.tr_days` | Counsel-selected integer 365–730. It is present and validated in every complete policy, even for a non-TR epoch. |
| `retention_policy.counsel_approval_commitment` | Nonzero canonical lowercase-hex 32-byte commitment to the private approval record. |
| `artifact` | Purpose-matched `trace_segments` or `attribution_wraps` contract. |
| `custody` | Production `external` adapter contract, or test-only `software_development`. |

Retention begins at the purpose-aware exclusive epoch end, not at creation. Policy v1 fixes US at
30 days, EU at zero standing days, and `test` at one day. Attribution epochs end at the next UTC
calendar-month boundary; network and identity epochs are exactly one UTC-aligned 86,400,000-ms day.
Changing the Türkiye value requires a different nonzero approval commitment and may extend but
never shorten an active inventory's computed retention.

### Artifact variants

For `kind = "trace_segments"`:

- `paths` must be absent/empty;
- `directory` is one absolute, canonical, owner-only directory dedicated to the key id;
- `expected_node_id` is one nonzero lowercase-hex 32-byte producer id;
- `expected_signer_public_key` is one valid, non-weak lowercase-hex Ed25519 public key;
- `expected_custody_key_digest` is nonzero lowercase hex for SHA-256 over the exact raw 32-byte
  custody public key.

The directory contains exactly one terminal manifest and the complete contiguous segment set:
`network-<key-id>-<eight-digit-index>.pptrace` or
`identity-<key-id>-<eight-digit-index>.pptrace`, starting at `00000000`, plus the matching
`network-<key-id>.ppmanifest` or `identity-<key-id>.ppmanifest`. Missing, extra, duplicate,
reordered, renamed, linked, mixed, or escaped artifacts fail closed. A segment is at most 16 MiB and
10,000 records; a terminal manifest is bounded to 65,536 segments. Verification streams one segment
at a time.

For `kind = "attribution_wraps"`:

- `directory` and every `expected_*` trace field must be absent;
- `paths` contains 1 through 64 unique absolute paths to serialized envelope-v3 wraps;
- each wrap is at most 1 MiB, the configured epoch total is at most 64 MiB, and each block must
  authenticate and name the configured attribution key id.

### Custody variants

Production uses:

```toml
[epochs.custody]
mode = "external"
public_key = "<64 lowercase hex characters>"

[epochs.custody.command]
executable = "/absolute/path/to/custody-adapter"
args = []
timeout_ms = 30000
inherit_environment = []
```

The public key is a nonzero raw 32-byte X25519 key. `command` is required and
`secret_key_path` is forbidden. For a trace epoch, SHA-256 of this public key must exactly match
`artifact.expected_custody_key_digest`. Before unseal, the same public key must match the complete,
witnessed registry history for the key id.

The only software variant is:

```toml
[epochs.custody]
mode = "software_development"
secret_key_path = "/absolute/owner-only/path/to/raw-32-byte-key"
```

It is accepted only for the `test` jurisdiction and forbids `public_key` and `command`. It is not a
production KMS, HSM, or share ceremony and must never be used to justify activation.

## Canonical key-id bytes

All adapter protocols carry the raw 47-byte id; CLI/configuration use its 94-character lowercase
hex encoding.

```text
offset  size  meaning
0       1     key-id version: 1
1       1     purpose: 1 attribution, 2 network_trace, 3 identity_trace
2       1     jurisdiction: 1 us, 2 eu, 3 tr, 255 test
3       32    stable authority id
35      8     epoch_start_ms, unsigned big-endian
43      4     generation, unsigned big-endian
```

Trace starts must be UTC-day aligned. Attribution starts must be the first instant of a UTC calendar
month. The shared purpose-aware validator rejects any other boundary.

## Private stdin declarations

`unseal` and every hold operation require non-terminal stdin on Linux/macOS. Feed it directly from
the protected case system or an owner-only neutral-name descriptor. Do not type values into an
interactive shell, put them in argv/environment variables, enable shell tracing, or route them
through a general-purpose job log.

The entire stdin declaration is at most 32 KiB, valid UTF-8, strict TOML, and version 1. Unknown
fields fail. Order reference and requester identity are each nonempty, at most 4 KiB, and contain no
NUL or control byte.

### Disclosure request

```toml
version = 1
order_reference = "authenticated case reference"
requester_identity = "authenticated requester"
selectors = ["event_id=<64 lowercase hex characters>"]
```

There are 1 through 8 selectors. Each is at most 1,024 bytes, has the form `key=value`, and contains
no NUL/control byte. Keys are unique, sorted internally, and ANDed. The final canonical selector
bytes sent for approval are NUL-separated and cannot exceed 4 KiB.

| Purpose | Allowed selectors | Required join key |
| --- | --- | --- |
| `network_trace` | `event_id`, `recipient`, `owner`, `correlation_commitment`, `operation` | At least one of the first four. `operation` alone is rejected. |
| `identity_trace` | `correlation_commitment` | `correlation_commitment` |
| `attribution` | `event_id`, `recipient` | Either value |

Every id/commitment value is one nonzero 32-byte lowercase-hex value. Network `operation` is exactly
`publish`, `fetch`, `put_agent`, or `claim`.

### Hold request

The same declaration is used for place, renew, and release:

```toml
version = 1
order_reference = "authenticated preservation reference"
```

The operator supplies the fixed requester label `legal-process-intake` to the approval adapter. A
hold term is positive and at most 90 days. Renewal creates a new hold id with immutable predecessor
lineage; release names the exact current hold id. Dates are strict `YYYY-MM-DD` UTC dates with a
year of at least 1970. `--until` means the end of that UTC day.

## Inventory declaration and lifecycle

The configured declaration file is strict TOML version 1, at most 256 KiB, and owner-only. It must
name the exact key id and set `created_at_ms` to the key id's exact epoch start. It contains 1 through
64 copies and must include every one of the seven required kinds. An unused class is recorded as
`verified_absent`, never omitted.

```toml
version = 1
key_id = "<94 lowercase hex characters>"
created_at_ms = 1786060800000

[[copies]]
kind = "live_metadata"
state = "present"
nonce = "<unique nonzero 64 lowercase hex characters>"
private_material = "private locator"

[[copies]]
kind = "sqlite_wal"
state = "verified_absent"
nonce = "<unique nonzero 64 lowercase hex characters>"
private_material = "private verification evidence"

[[copies]]
kind = "sidecar"
state = "present"
nonce = "<unique nonzero 64 lowercase hex characters>"
private_material = "private locator"

[[copies]]
kind = "snapshot"
state = "present"
nonce = "<unique nonzero 64 lowercase hex characters>"
private_material = "private locator"

[[copies]]
kind = "backup"
state = "present"
nonce = "<unique nonzero 64 lowercase hex characters>"
private_material = "private locator"

[[copies]]
kind = "kms_version"
state = "present"
nonce = "<unique nonzero 64 lowercase hex characters>"
private_material = "private locator"

[[copies]]
kind = "shamir_share"
state = "present"
nonce = "<unique nonzero 64 lowercase hex characters>"
private_material = "private locator or holder reference"
```

Each `private_material` value is nonempty and at most 4 KiB. Each nonce is unique in the declaration.
The operator uses the nonce and private material to derive a stable opaque copy id, then zeroizes the
raw value. Raw locators and absence evidence are not written to PPinv, stdout, argv, environment,
the destruction adapter request, or the public disclosure ledger. The external custodian must
securely retain its own copy-id-to-target mapping before the active inventory is provisioned.
Encoded staging, import, and active PPinv files are each limited to 128 KiB. One inventory can carry
at most 64 copy records and 64 hold records. Canonical persisted state is PPinv v3. PPinv v1 is
rejected; valid policy-bearing v2 state is accepted without trace-integrity evidence and is upgraded
to v3 on its next mutation.

The supported transitions are:

1. `inventory create` authenticates a configured trace epoch, consumes the complete declaration,
   and atomically creates the staging PPinv without replacement.
2. `inventory provision` validates staging and atomically creates the active inventory without
   replacement.
3. `inventory import` validates an externally prepared PPinv from the import path and atomically
   creates the active inventory without replacement.
4. `inventory update` accepts a complete declaration only while active state is `Retained`. It may
   add copies or extend policy under a new approval commitment; it cannot omit, mutate, or remove an
   existing copy, shorten retention, or erase holds.

Create/provision and import are alternative activation paths. Never overwrite an active inventory
to force either path. All inventory, disclosure, hold, shred, checkpoint, and status operations use
the same exclusive `operator.lock`. `status` is not a lock-free probe: opening a ledger can recover
one authenticated crash-interrupted tail record, and it requires every configured active inventory
to exist and validate.

## Command and stdout contracts

The exhaustive command surface is:

```text
ppcompliance status
ppcompliance inventory create --epoch <canonical-key-id-hex>
ppcompliance inventory provision --epoch <canonical-key-id-hex>
ppcompliance inventory import --epoch <canonical-key-id-hex>
ppcompliance inventory update --epoch <canonical-key-id-hex>
ppcompliance unseal --epoch <canonical-key-id-hex> < private-request.toml
ppcompliance shred --before <YYYY-MM-DD> [--dry-run|--execute]
ppcompliance hold --epoch <canonical-key-id-hex> --until <YYYY-MM-DD> < private-request.toml
ppcompliance hold renew --epoch <canonical-key-id-hex> --hold <hold-id-hex> --until <YYYY-MM-DD> < private-request.toml
ppcompliance hold release --epoch <canonical-key-id-hex> --hold <hold-id-hex> < private-request.toml
ppcompliance checkpoint
ppcompliance --version
```

### Fixed line output

`status` emits exactly these newline-delimited keys in this order:

```text
status=ready
disclosure_leaves=<integer>
incomplete_disclosures=<integer>
disclosure_root=<64 lowercase hex characters>
inventories_retained=<integer>
inventories_shredding=<integer>
inventories_shredded=<integer>
active_holds=<integer>
external_custody_epochs=<integer>
development_custody_epochs=<integer>
```

`status=ready` means the configured local files, inventories, signing key, and checkpoint floor
validated. It does not authenticate trace/attribution artifacts, refresh registry inputs, test an
adapter, prove the external publisher is healthy, or satisfy an activation gate.

Inventory commands emit:

```text
inventory=staged
inventory=provisioned
inventory=imported
```

Update emits all three lines:

```text
inventory=updated
policy_updated=<true|false>
added_copies=<integer>
```

Hold place/renew emits `hold_id=<64 lowercase hex characters>`. Release emits
`released_hold_id=<64 lowercase hex characters>`.

`shred` defaults to dry-run when neither mode flag is present. Supplying both flags is invalid.
`--before` is a UTC epoch-start selection boundary, not permission to bypass the independently
computed epoch end, retention, or holds.

Dry-run emits:

```text
mode=dry_run
eligible_epochs=<integer>
resumed_epochs=<integer>
skipped_held_epochs=<integer>
skipped_unexpired_epochs=<integer>
already_shredded_epochs=<integer>
discovery_failed_epochs=<integer>
trace_integrity_degraded_epochs=<integer>
```

Execution emits:

```text
mode=execute
shredded_epochs=<integer>
resumed_epochs=<integer>
skipped_held_epochs=<integer>
skipped_unexpired_epochs=<integer>
already_shredded_epochs=<integer>
discovery_failed_epochs=<integer>
execution_failed_epochs=<integer>
trace_integrity_degraded_epochs=<integer>
failed_epochs=<integer>
```

Discovery or execution failures do not suppress work on another eligible epoch. The summary is
still written, followed by a nonzero process exit. A `Shredding` inventory resumes only its
unreceipted copies. The operator records authenticated trace-integrity state before requesting the
first deletion and never allows a later clean observation to erase an earlier degradation.

`checkpoint` writes the same signed C2SP/RFC-6962 note to `checkpoint_output_path` and stdout. It
requires an existing ledger. The handoff is advanced internally before any successful disclosure
bytes are released, so a separate explicit `checkpoint` is not required after each unseal.

### Disclosure NDJSON

`unseal` emits zero or more JSON objects, one object per line. A successful request with no matching
record emits zero bytes but still records the authorized intent and successful completion. Output is
limited to 1,000 records and 4 MiB total.

Attribution record:

```json
{"kind":"attribution","key_id":"<94-hex>","event_id":"<64-hex>","recipient":"<64-hex>","sender_public_key":"<64-hex>","sent_at_ms":1786105721119}
```

Network trace record (`event_id`, `recipient`, `owner`, and `correlation_commitment` can be `null`):

```json
{"kind":"network_trace","jurisdiction":"us","operation":"publish","timestamp_ms":1786105721119,"node_id":"<64-hex>","source_address":"192.0.2.10","source_port":7717,"event_id":"<64-hex-or-null>","recipient":"<64-hex-or-null>","owner":null,"size_bytes":256,"correlation_commitment":null}
```

Identity trace record:

```json
{"kind":"identity_trace","jurisdiction":"eu","timestamp_ms":1786105721119,"node_id":"<64-hex>","correlation_commitment":"<64-hex>","provider":"oidc","provider_subject":"<provider subject>"}
```

`jurisdiction` is `us`, `eu`, `tr`, or `test`. Identity `provider` is `oidc`, `saml`,
`local_directory`, or `oauth2`.

Treat stdout as regulated output: connect it directly to an approved encrypted case destination,
verify a successful exit, and prevent terminal capture, shell history, CI logs, crash reports, or
metrics from recording it. No content plaintext is available through this command.

### Exit and diagnostic behavior

Success exits zero. Any usage, configuration, storage, authorization, custody, artifact, state,
limit, or platform error exits nonzero. Stderr contains only `ppcompliance: ` plus one coarse error
category; underlying paths, subprocess errors, case values, custody responses, and selector values
are deliberately withheld. Adapter stderr is discarded.

Do not infer that a nonzero exit means no durable mutation occurred. A crash, output write failure,
or later epoch failure can follow a persisted inventory transition, disclosure leaf, checkpoint,
hold receipt, integrity observation, or destruction receipt. Capture stdout atomically where
possible, then reconcile with `status`, the active inventory, the public checkpoint monitor, and the
external custody audit before retrying.

## Common subprocess contract

All adapters are direct executables, not shell commands. `ppcompliance` clears the environment,
restores only allowlisted variables, pipes one exact binary request to stdin, reads a bounded exact
binary response from stdout, and sends adapter stderr to the null device. It does not add a newline
or framing outside the protocol. All integers below are unsigned and big-endian.

The operator fails closed on spawn/I/O failure, early exit before the full request is written,
nonzero status, timeout, overlong/short/trailing output, wrong magic/version, or an invalid response.
On Linux/macOS each adapter gets a separate process group; timeout or protocol failure terminates
the process and its group. An adapter must not fork a detached worker, retain stdin/stdout after its
decision, or report success before the external action is durable.

Adapter timeouts are hard safety deadlines, not advisory latency targets. Design device operations
to finish well inside the configured value and make every operation idempotent enough for the
documented recovery flow.

## Approval adapter protocol v1

Request, variable length:

```text
"PPAPREQ\0"[8]
| version:u8
| request_id[32]
| jurisdiction:u8
| purpose:u8
| created_at_ms:u64
| expires_at_ms:u64
| key_count:u8
| key_id[47] * key_count
| order_length:u16 | order_reference[order_length]
| requester_length:u16 | requester_identity[requester_length]
| scope_length:u16 | scope_bytes[scope_length]
```

The current CLI always sends `key_count = 1`. Purpose, jurisdiction, and key-id discriminants use
the canonical values above. The adapter must parse exact lengths, reject trailing bytes, display the
typed epoch and bounded private fields only inside the protected approval surface, and obtain two
independent human decisions before signing.

For disclosure, `scope_bytes` is the sorted NUL-separated canonical selector set. For hold actions it
is binary and has one of these exact forms:

```text
"place"   | until_end_of_utc_day_ms:u64
"renew"   | prior_hold_id[32] | until_end_of_utc_day_ms:u64
"release" | hold_id[32]
```

The fixed requester label for a hold is `legal-process-intake`. An adapter must authorize the exact
scope bytes and case workflow; it must not approve based only on that label or a key id.

Response, exactly 217 bytes:

```text
"PPAPRES\0"[8]
| version:u8
| approver_1_ed25519_public[32] | approved_1_ms:u64 | signature_1[64]
| approver_2_ed25519_public[32] | approved_2_ms:u64 | signature_2[64]
```

Each signature is Ed25519 over exactly:

```text
"pigeonpost/disclosure-approval/v1"
| request_id[32]
| approved_at_ms:u64
| 0x01
```

The keys must be distinct members of the pinned roster. Each approval time must be no earlier than
request creation, no later than request expiry, and no more than five minutes ahead of the
operator's current clock. The configured roster identity corresponding to each returned key is
placed only in the encrypted private audit record.

The repository's
[`m6_acceptance_adapter.rs`](../crates/pigeonpost-compliance/examples/m6_acceptance_adapter.rs) is a
test-only exact-binary fixture. It signs automatically with fixed seeds and is neither a production
approval service nor a model for human authorization.

## Custody adapter protocol v1

Request, exactly 152 bytes:

```text
"PPCUSTQ\0"[8]
| version:u8
| request_id[32]
| key_id[47]
| configured_custody_public[32]
| peer_x25519_public[32]
```

Response, exactly 41 bytes:

```text
"PPCUSTR\0"[8]
| version:u8
| x25519_shared_secret[32]
```

The returned shared secret must be nonzero and must be the X25519 agreement between the named
epoch's segregated custody private key and `peer_x25519_public`. The adapter must bind the operation
to `request_id`, map the exact typed `key_id` to the authorized device object, verify that its public
key equals `configured_custody_public`, and refuse an unavailable, retired-from-custody, mismatched,
or unauthorized object. `ppcompliance` independently verifies that public key against the complete
witnessed registry history before it requests approval.

The shared secret is used transiently to unwrap only the named epoch key and is zeroized. The epoch
key and compliance private key are never printed, returned to the requester, or persisted as a
standing decrypted copy. A production adapter should keep the private operation inside the HSM/KMS
or independently administered share ceremony and retain only its approved external audit evidence.

No production custody adapter is included in the repository. Building, reviewing, provisioning,
and disaster-testing it is an external activation gate.

## Destruction adapter protocol v1

Request, exactly 89 bytes:

```text
"PPSHRED\0"[8]
| version:u8
| key_id[47]
| copy_id[32]
| copy_kind:u8
```

Copy-kind values are:

| Value | Kind |
| ---: | --- |
| 1 | `live_metadata` |
| 2 | `sqlite_wal` |
| 3 | `sidecar` |
| 4 | `snapshot` |
| 5 | `backup` |
| 6 | `kms_version` |
| 7 | `shamir_share` |

Response, exactly 42 bytes:

```text
"PPSHRES\0"[8]
| version:u8
| result:u8
| evidence_commitment[32]
```

`result = 1` means destroyed; `result = 2` means independently verified absent. The evidence
commitment must be nonzero. Its private opening and supporting device/provider proof remain in the
external custody record.

The request intentionally omits the raw locator. Before provisioning, the external procedure must
bind each opaque `copy_id` and kind to exactly one real target. It must reject unknown or mismatched
ids, perform or verify the actual irreversible action, durably record evidence before responding,
and be idempotent: a retry after a lost response must return a valid destroyed/verified-absent
receipt rather than recreating or forgetting the target.

The test-only Rust adapter linked above removes one fixture key file and does not implement a
production inventory map, device erasure, backup handling, or independent absence proof.

## Disclosure lifecycle

For one `unseal`, the operator performs this ordered sequence under the exclusive lock:

1. Parse one typed epoch and selector set; require active inventory state `Retained`.
2. Authenticate the configured custody public key through the complete imported registry history.
3. Authenticate the complete purpose-specific artifact set and enforce the witnessed key validity
   interval on every disclosed record.
4. Create a salted-commitment request id and obtain exactly two valid approval responses.
5. Prepare encrypted private-audit bytes for the case values and approver identities.
6. Construct the external custody boundary, open/create the disclosure ledger, authenticate its
   `.state`, and verify the retained signed checkpoint floor.
7. Publish a signed empty floor before the first intent, if needed; close any prior incomplete
   request with a failure leaf; append the new intent.
8. After the intent is durable, atomically create the no-replace `.ppaudit` record, ask custody for
   the transient agreement, unwrap only the named epoch, select and verify records, and append
   success/failure completion.
9. Atomically advance `checkpoint_output_path` before writing any result bytes to stdout.

The public ledger contains commitments, timestamps, purpose/jurisdiction, epoch ids, status, and
result counts, not raw order reference, requester, selector, approver identity, source address, or
disclosed record. The raw case values exist in Pigeonpost-controlled persistence only inside the
separately encrypted private-audit record. Protect the approval adapter and final stdout destination
because both necessarily see authorized private material.

## Hold and destruction lifecycle

Place, renew, release, status, disclosure, and shred serialize on the same inventory state. A valid
active hold prevents `Retained -> Shredding`; a concurrent expiry cannot race past it. Each hold
mutation requires two approval signatures. A 90-day hold can be renewed through a new authorized
receipt and predecessor id.

Always run `shred --dry-run` first and review every count against the case system, retention policy,
external copy map, and expected epoch set. `--execute` then:

1. evaluates every selected epoch independently;
2. skips held, unexpired, and already-shredded epochs;
3. authenticates the terminal trace manifest and records body integrity before any deletion;
4. persists `Shredding` before external deletion;
5. invokes the destruction adapter once per unreceipted copy and persists each receipt immediately;
6. marks `Shredded` only after every declared copy is destroyed or verified absent.

Missing or corrupt trace ciphertext is recorded as integrity degradation and still permits key
destruction after a valid pinned terminal manifest; otherwise corrupt bytes could make a key
immortal. The same bundle remains undisclosable. Sealed artifacts may remain after crypto-shred,
but no backup, KMS version, share, or other inventoried key copy may be restored afterward.

The binary does not invoke the approval adapter for `shred`. Required human dual control,
provider-side deletion policy, change authorization, and separation of duties must therefore be
enforced by the external destruction/custody procedure and operational access controls.

## Checkpoint lifecycle

There are two separate checkpoint systems:

- The imported registry checkpoint authenticates compliance-key history. Its public key, witnesses,
  freshness, and historical floor live in `[registry_audit]`.
- The generated disclosure checkpoint authenticates this operator's append-only disclosure ledger.
  Its signing seed is `checkpoint_signing_key_path`; its current owner-only handoff is
  `checkpoint_output_path`.

The disclosure floor may be absent only while the ledger is empty. A nonempty ledger with no floor,
a floor newer than the ledger, a same-size different root, a bad signature/origin, or an
RFC-6962-inconsistent older floor fails closed. The external publisher copies only the signed
handoff; it must never receive the signing seed, ledger, `.state`, inventories, audit key, or private
audit records. An independent monitor retains the last accepted public head and alerts/refuses on
size regression, same-size root change, or invalid consistency.

Publication transport, scheduling, monitoring, and proof retention are not implemented by
`ppcompliance`. They are production infrastructure prerequisites and should run at the names-log
cadence specified by the SDS.

## Backup, recovery, and rollback boundaries

Treat these as one custody recovery set, with separate access classes:

- `config.toml` and the exact adapter binaries/configuration identities;
- the disclosure ledger and its `<ledger-filename>.state` companion;
- the current signed disclosure checkpoint handoff and independently retained public heads;
- every active PPinv inventory, including hold and partial-destruction receipts;
- encrypted `.ppaudit` files and the private-audit encryption key;
- the disclosure checkpoint signing seed;
- imported registry dump/checkpoint and the independently recorded minimum floor;
- sealed artifacts and terminal manifests;
- external KMS/HSM/share metadata and the private copy-id mapping.

Do not restore one mutable file in isolation. In particular:

- Never roll back the ledger without its authenticated `.state` and checkpoint history, or the
  inventory without its current hold/destruction receipts.
- Never restore a custody secret, backup, KMS version, or share after its inventory recorded a
  destruction/absence receipt. Recovery media are themselves inventory copies and must be erased in
  the same ceremony.
- Never lower `minimum_checkpoint_size/root` to make an older registry import pass.
- Never replace the public checkpoint monitor's retained head with a custody-host backup.
- Do not rotate `checkpoint_signing_key_path` or `private_audit_key_path` in place. The current CLI
  has no migration command: the former also authenticates the restart sidecar and existing handoff,
  while the latter protects existing private audits. A reviewed versioned migration is required.
- Do not replace config v2 with an older schema or reuse an inventory/staging path for another key.

After a crash or suspected corruption:

1. Isolate the custody host and preserve a read-only incident copy; do not repeatedly invoke
   destructive commands while the evidence set is unknown.
2. Compare the local disclosure handoff with the independently retained public head and the local
   registry floor with the independent registry record.
3. Restore only the latest coherent recovery set whose inventory receipts and external custody
   audit agree. Reapply owner-only permissions and verify adapter file identities.
4. Run `status`. It may recover only one authenticated crash-interrupted final ledger record; any
   other malformed or inconsistent state remains a failure.
5. If an inventory is `Shredding`, reconcile every external receipt and resume with an explicit
   reviewed `shred --execute`. Do not recreate an absent copy.
6. Keep disclosure disabled until `status`, registry continuity, adapter health, and the external
   checkpoint monitor all agree.

An old but internally coherent backup of ledger, sidecar, and handoff can pass local checks. The
independent publisher/monitor is therefore mandatory recovery evidence, not optional observability.

## Commissioning and recurring checks

Before production activation:

- verify the exact native binary checksum, attestation, version, and supported platform;
- verify home/file permissions, distinct paths, adapter ownership/link count, and a cleared adapter
  environment;
- exercise each production adapter against a nonproduction device partition, including malformed
  magic/version/length, trailing output, nonzero exit, timeout, process-tree termination, duplicate
  approval, wrong key, unknown copy, and retry-after-lost-response cases;
- import and independently verify a complete fresh registry dump/checkpoint and its retained floor;
- provision every epoch inventory and reconcile all seven copy kinds against the external map;
- confirm `development_custody_epochs=0` in production;
- run `status`, then a reviewed `shred --dry-run` with a boundary that exercises held and unexpired
  skips without selecting a production key for deletion;
- verify the disclosure checkpoint publisher/monitor rejects a synthetic regression/fork in an
  isolated environment;
- verify private stdin and disclosure stdout never enter terminal capture, ordinary logs, metrics,
  crash reports, or reverse-proxy access logs;
- complete the legal, custody, independent-witness, and organizational activation record.

During operation:

- refresh the full registry dump and fresh witnessed checkpoint through the approved offline import
  ceremony before a disclosure;
- monitor adapter latency against its hard timeout and alert on any coarse operator failure without
  adding private values to diagnostics;
- reconcile `status` inventory/hold counts with the case and custody systems;
- publish and independently retain each new disclosure checkpoint;
- add new epoch inventories before artifacts need disclosure, and never let an untracked copy leave
  the declared inventory;
- use dry-run plus independent review before every execution batch;
- test recovery from coherent backups without exposing or recreating destroyed material.

The release-level exact-binary lifecycle is exercised by
[`m6_binary_acceptance.rs`](../crates/pigeonpost-compliance/tests/m6_binary_acceptance.rs) through
[`deploy/acceptance/m6-compliance.sh`](../deploy/acceptance/m6-compliance.sh). The test uses isolated
`test`-jurisdiction software custody and the test-only adapter. Passing it proves the packaged binary
contract in that fixture; it does not prove production legal authority, witness independence,
external custody, destruction completeness, public checkpoint monitoring, or disaster recovery.
