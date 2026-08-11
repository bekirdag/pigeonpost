# `ppcompliance`

`ppcompliance` is the offline-only Pigeonpost custody and disclosure operator. Its dependency
closure contains no HTTP client, server framework, async runtime, or database engine. Online nodes
link `pigeonpost-compliance-seal`; they do not link this package.

The command surface is:

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

The offline operator is distributed only as a separately named, checksummed and attested macOS or
Linux GitHub Release asset. It is not in the npm launcher or the online GHCR image. There is no
Windows custody asset: the current Windows file layer does not yet prove owner-only DACLs,
hard-link/file identity, safe parent traversal, and atomic replacement. Verify `SHA256SUMS` and the
artifact attestation before moving a supported artifact into the offline custody environment.

`PIGEONPOST_COMPLIANCE_HOME` must name an absolute mode-`0700` directory containing a mode-`0600`
`config.toml`. Raw order references, selectors, and requester identities are accepted only in a
strict, versioned TOML declaration read from bounded stdin. They are rejected in argv and are never
read from environment variables, printed, or placed in a subprocess argument. Supply stdin from
the encrypted case-management boundary or an owner-only neutral-name file descriptor; do not type
case values into an interactive shell. The binary refuses terminal stdin for these commands.
Pigeonpost-controlled persistence stores the raw values only in the separately encrypted private
audit record.

An `unseal` declaration is:

```toml
version = 1
order_reference = "case reference"
requester_identity = "authenticated requester"
selectors = ["event_id=64-lowercase-hex-characters"]
```

A `hold` declaration contains only `version = 1` and `order_reference`. Place, renew, and release
all require two distinct signatures from the pinned approval roster before the inventory can
change. Renewal records the prior hold id as immutable lineage; release names the exact canonical
hold id. Unknown fields, an unknown version, malformed UTF-8/TOML, empty required values, too many
selectors, and declarations over 32 KiB fail closed. The external case store remains responsible
for protecting and retiring the input declaration after the operation.

`shred` is always a dry-run unless `--execute` is explicit. It evaluates every selected epoch
independently: held and unexpired epochs are counted and skipped, eligible epochs continue even if
another custodian fails, and an inventory already in `Shredding` resumes from its durable receipts.
Discovery releases each authenticated manifest and owner-directory handle immediately, retaining
only a bounded commitment/integrity summary for execute and no candidate state for dry-run.
Execution reopens one epoch at a time and refuses a changed manifest commitment before deletion.
The extra switch resolves the SDS's safety requirement without assigning irreversible meaning to
the absence of `--dry-run`.

## Disclosure ledger operation

The append-only disclosure ledger keeps its existing `PPDISC` length-prefixed file format. Opening
it strictly streams at most 512 MiB/100,000 leaves and will recover only a truncated final record;
an invalid length, malformed complete record, duplicate request, or invalid intent/completion order
fails closed without rewriting the evidence. The v2 authenticated restart sidecar stores only
constant-sized committed-prefix metadata and one bounded pending record. It intentionally rejects
unreleased development sidecars that persisted growing indexes; Pigeonpost 0.1 did not ship this
ledger, so no production evidence-file migration is required for the 0.2 release.

The operator does not load the ledger into memory. It retains compact record offsets, request-id
uniqueness and outstanding-intent state, incremental Merkle frontiers, one cached root, and bounded
proof blocks. `status` and `checkpoint` therefore never rehash all historical leaves. Inclusion and
consistency proofs remain reproducible from the durable log and verify against the same RFC 6962
roots as the registry implementation.

## Configuration

All configured paths are absolute. Every inventory declaration, staging, import, and active path
must be distinct, must live below the compliance home, and cannot alias another configured secret,
artifact, ledger, or adapter path. Secret and inventory files, their directories, and the compliance
home must exclude group/other access. Adapter executables must be regular, non-symlink, executable
files and must not be group- or world-writable.

Operator configuration version 2 is required. Version 1 predates policy-bearing inventories and is
rejected rather than being interpreted with an implicit retention decision.

The offline registry audit accepts only a nonempty strict-majority witness policy (`2k > N`), so
1-of-1 and 2-of-3 are valid while 1-of-2 and 1-of-3 fail closed. This proves same-roster set
intersection, not honesty. No-gossip fork resistance additionally requires `f < 2k - N` for at most
`f` equivocators; use N-of-N if the only justified assumption is that at least one of N is honest.
Different rosters require guaranteed non-equivocating overlap or external checkpoint comparison.

```toml
version = 2
ledger_path = "/offline/pigeonpost/disclosure.log"
private_audit_directory = "/offline/pigeonpost/private-audit"
private_audit_key_path = "/offline/pigeonpost/private-audit.key"
checkpoint_origin = "pigeonpost.example/disclosures"
checkpoint_signing_key_path = "/offline/pigeonpost/checkpoint.key"
checkpoint_output_path = "/offline/pigeonpost/publication/disclosure.checkpoint"

[registry_audit]
log_path = "/offline/pigeonpost/registry.ndjson"
checkpoint_path = "/offline/pigeonpost/registry.checkpoint"
expected_origin = "pigeonpost.example/registry"
checkpoint_key = "<64 lowercase hex characters>"
witness_threshold = 2
minimum_checkpoint_size = 48211
minimum_checkpoint_root = "<64 lowercase hex characters>"
max_cosignature_age_seconds = 86400
future_clock_skew_seconds = 300

[[registry_audit.witnesses]]
name = "witness-one.example"
public_key = "<64 lowercase hex characters>"

[[registry_audit.witnesses]]
name = "witness-two.example"
public_key = "<64 lowercase hex characters>"

[approval]
request_ttl_ms = 300000

[[approval.approvers]]
public_key = "<64 lowercase hex characters>"
identity = "officer-one"

[[approval.approvers]]
public_key = "<64 lowercase hex characters>"
identity = "outside-counsel"

[approval.command]
executable = "/offline/bin/pigeonpost-approval-adapter"
args = []
timeout_ms = 30000
inherit_environment = ["KMS_PROFILE"]

[destruction_command]
executable = "/offline/bin/pigeonpost-destruction-adapter"
args = []
timeout_ms = 30000

[[epochs]]
key_id = "<94 lowercase hex characters>"
inventory_path = "/offline/pigeonpost/inventories/<key-id>.ppinv"
inventory_declaration_path = "/offline/pigeonpost/inventories/<key-id>.toml"
inventory_staging_path = "/offline/pigeonpost/inventories/<key-id>.staged.ppinv"
inventory_import_path = "/offline/pigeonpost/import/<key-id>.ppinv"

[epochs.retention_policy]
version = 1
tr_days = 548
counsel_approval_commitment = "<64 lowercase hex characters>"

[epochs.artifact]
kind = "trace_segments"
expected_node_id = "<64 lowercase hex characters>"
expected_signer_public_key = "<64 lowercase hex characters>"
expected_custody_key_digest = "<SHA-256 of the 32-byte custody public key; 64 lowercase hex characters>"
directory = "/offline/pigeonpost/trace-epochs/<key-id>"

[epochs.custody]
mode = "external"
public_key = "<64 lowercase hex characters>"

[epochs.custody.command]
executable = "/offline/bin/pigeonpost-custody-adapter"
args = []
timeout_ms = 30000
```

Attribution epochs use `kind = "attribution_wraps"` and `paths` to individual serialized v3 wraps.
Network and identity epochs use `trace_segments`, a `directory`, and no `paths` field. A trace
directory is dedicated to one key id and must contain exactly its owner-only terminal
`.ppmanifest` plus the complete declared segment set; missing, extra, renamed, reordered,
duplicated, or mixed artifacts fail closed. The directory must be an absolute canonical owner-only
directory, and segment files must be regular owner-only single-link files. A symlink, hard link,
path alias, or directory escape is rejected. One `unseal` command names exactly one typed epoch, so
no ordinary disclosure can combine network and identity records.

The exact local names are `network-<key-id>-<eight-digit-index>.pptrace` or
`identity-<key-id>-<eight-digit-index>.pptrace`, starting at index `00000000`, plus
`network-<key-id>.ppmanifest` or `identity-<key-id>.ppmanifest`. Filenames are local transport
metadata rather than signed fields, but the offline directory contract is strict so the operator
can derive one unambiguous path for each signed contiguous index.

Every trace epoch pins its expected producer node id, Ed25519 segment signer, and custody-key digest
in offline configuration. `expected_custody_key_digest` is SHA-256 over the exact raw 32-byte
custody public key; for external custody it must match `epochs.custody.public_key` at configuration
load, and `unseal` also requires it to match the independently audited registry key. The operator
authenticates the terminal manifest and every declared segment before inventory create, provision,
import, or update and before requesting disclosure approvals. Shred always requires the authentic
pinned terminal manifest, but missing, extra, or corrupt ciphertext bodies are persisted as
integrity degradation before the first key deletion and do not make the decryption key immortal.
Disclosure continues to fail closed on any such degradation. The operator requires the canonical exclusive epoch end to have passed,
checks the signed producer, signer, custody digest, epoch-key commitment, segment/record totals, and
ordered segment metadata, and always performs the terminal completeness check. Verification and
disclosure process one segment at a time under the manifest's 65,536-segment bound rather than a
64-file or 64-MiB whole-epoch ceiling. After unwrap, every record must also carry the configured
node id and the key id's jurisdiction. The encrypted private audit record and public disclosure
result commitment bind the exact authenticated terminal-manifest commitment across restart.

The operator verifies `checkpoint_output_path` as a monotonic signed RFC 6962 floor before status,
checkpoint, or disclosure work. It establishes a signed empty floor before the first intent,
advances it before releasing disclosure bytes, and refuses a missing floor for a nonempty ledger or
a newer/conflicting/inconsistent floor. `checkpoint` atomically replaces that path with the
owner-only signed C2SP note and also emits the identical note on stdout. A separately provisioned
scheduler/publisher copies only
that signed file to the public transparency endpoint on the names-log cadence; it never receives
the signing key, ledger, inventories, or private audit records. Provisioning and monitoring that
external scheduler remains a deployment gate rather than a property asserted by this package. It
must retain and reject any signed regression or fork because coordinated rollback of all three
local artifacts (log, sidecar, and handoff) is not locally distinguishable.

`software_development` custody accepts a raw 32-byte key file only for the `test` jurisdiction.
US, EU, and TR epochs refuse that mode at configuration load; they require the external custody
adapter.

Before it asks for approvals, `unseal` independently authenticates the configured custody key from
the complete imported registry log. It verifies exact leaf order and count, recomputes both the
pinned `minimum_checkpoint_*` prefix and the final checkpoint root, requires a fresh threshold of
pinned witness cosignatures, and replays all key status transitions. The dump is deliberately the
full `/v1/log/dump` NDJSON history, not a server-projected compliance-key list: a projection could
omit a later revocation. Copy the dump and signed checkpoint into the offline environment through
the custody import procedure; an inconsistent pair fails closed.

Inventories use strict PPinv v3. Each state embeds retention-policy v1: the US product choice is
exactly 30 days, EU is preservation-only (zero standing days), and the test jurisdiction is one day.
`tr_days` has no product default: counsel must select 365–730 days and the configuration must carry a
nonzero commitment to that approval record. Changing the selected Türkiye period is a private
configuration ceremony, not a code release. An inventory whose embedded policy differs from its
epoch configuration, including PPinv v1, fails closed. Canonical v2 state is accepted with no
trace-integrity evidence and is upgraded to v3 on its next mutation.

The inventory ceremony never accepts a raw locator in an argument. The configured declaration path
must be a bounded, owner-only TOML file with `version = 1`, the exact canonical `key_id`, the epoch's
`created_at_ms`, and one or more `[[copies]]` declarations. Every required class must appear:
`live_metadata`, `sqlite_wal`, `sidecar`, `snapshot`, `backup`, `kms_version`, and `shamir_share`.
Each declaration states `present` or `verified_absent`, supplies a unique nonzero 32-byte nonce as
lowercase hexadecimal, and puts its private locator or absence evidence in `private_material`.
That material is consumed only from the private file, committed and zeroized; it is never persisted
in PPinv, printed, logged, or passed to a subprocess.

```toml
version = 1
key_id = "<94 lowercase hex characters>"
created_at_ms = 1786060800000

[[copies]]
kind = "live_metadata"
state = "present"
nonce = "<64 lowercase hex characters>"
private_material = "<private locator or verification evidence>"
```

`inventory create` validates the complete declaration and writes the configured staging file with
an atomic no-replace publication. `inventory provision` validates that staged PPinv and publishes
the active `inventory_path`, again without replacement. `inventory import` performs the same exact
key/policy validation from `inventory_import_path`. `inventory update` accepts only a complete,
monotonic declaration while the inventory is retained: it cannot omit or mutate an existing
commitment. It may extend the encoded policy without adding a copy, but can never shorten that
epoch's computed retention and every policy change requires a different nonzero counsel approval
commitment. Its crash-safe replacement preserves holds. All four operations take the same exclusive
operator lock used by hold, shred, disclosure, and status. Source and destination parent directories
must be owner-only; source files must be regular, single-link, owner-only files.

## Adapter protocols

Adapters receive exact binary requests on stdin and return exact binary responses on stdout.
Unknown versions, trailing bytes, oversized output, nonzero exit status, and timeout all fail
closed. The operator clears the subprocess environment, then restores only the explicitly listed
variables. Stderr is discarded and never forwarded into operator diagnostics.

All integers are big-endian.

### Approval

Request:

```text
"PPAPREQ\0"[8] | version:u8 | request_id[32] | jurisdiction:u8 | purpose:u8
| created_ms:u64 | expires_ms:u64 | key_count:u8 | key_id[47] * key_count
| order_len:u16 | order | requester_len:u16 | requester
| selectors_len:u16 | canonical_selectors
```

Response (exactly two records):

```text
"PPAPRES\0"[8] | version:u8
| (approver_ed25519_public[32] | approved_ms:u64 | signature[64]) * 2
```

Each signature covers `"pigeonpost/disclosure-approval/v1" || request_id || approved_ms || 0x01`.
Both keys must be distinct members of the pinned roster.

### Custody agreement

Request:

```text
"PPCUSTQ\0"[8] | version:u8 | request_id[32] | key_id[47]
| configured_custody_public[32] | peer_x25519_public[32]
```

Response:

```text
"PPCUSTR\0"[8] | version:u8 | x25519_shared_secret[32]
```

The shared secret exists only long enough to unwrap the named epoch key and is zeroized. The epoch
key is never printed or returned; only selector-matching records are emitted, after the disclosure
completion leaf is durable.

### Destruction

Request:

```text
"PPSHRED\0"[8] | version:u8 | key_id[47] | copy_id[32] | copy_kind:u8
```

Response:

```text
"PPSHRES\0"[8] | version:u8 | result:u8 | evidence_commitment[32]
```

`result` is `1` for destroyed and `2` for independently verified absent. State is persisted after
every receipt, so a failed ceremony resumes in `Shredding` and can never skip an unreceipted copy.
