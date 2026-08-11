# Pigeonpost — production runtime configuration

Status: operator contract.

The server commands load configuration from the directory passed with `--dir`:

- `pigeonpost loft serve --dir /srv/pigeonpost` reads `/srv/pigeonpost/loft.toml`
- `pigeonpost registry serve --dir /srv/pigeonpost` reads `/srv/pigeonpost/registry.toml`

The separately distributed offline `ppcompliance` binary has a different custody boundary and
configuration schema. Its complete operator, adapter, recovery, and example-config contract is in
[`compliance-operations.md`](compliance-operations.md); do not derive it from these online-service
examples.

Relative paths in either file are resolved beneath `--dir`. Parent traversal is rejected. Absolute
paths are accepted for deliberately independent trace volumes and mounted secrets. Configuration
files must be regular, non-symlink files no larger than 64 KiB and must not be group/world writable.

## Witnessed registry block

Both production runtimes use the same strict block. Values below are placeholders, not bootstrap
keys. Every key and checkpoint must come through the custody and independent-witness process in
`law.md`; the runtime never creates or approves them.

```toml
[compliance.registry]
registry_url = "https://registry.example/"
expected_origin = "registry.example/log"
registry_checkpoint_key = "REGISTRY_ED25519_PUBLIC_KEY_HEX"
witness_threshold = 2
minimum_checkpoint_size = 123
minimum_checkpoint_root = "PINNED_CHECKPOINT_ROOT_HEX"
max_staleness_seconds = 600
refresh_interval_seconds = 60
state_path = "compliance/registry-state.json"

[[compliance.registry.witnesses]]
name = "independent-witness-a"
public_key = "WITNESS_A_ED25519_PUBLIC_KEY_HEX"

[[compliance.registry.witnesses]]
name = "independent-witness-b"
public_key = "WITNESS_B_ED25519_PUBLIC_KEY_HEX"
```

Keys are canonical lowercase 32-byte hex. Witness names and keys must be unique, witness keys must
differ from the registry key, and the threshold must be a strict majority of the configured roster:
`2 * threshold > witness_count`. Thus 1-of-1 and 2-of-3 are valid; 1-of-2 and 1-of-3 are rejected.
The URL must be an HTTPS origin, except that an HTTP origin whose host parses directly as a numeric
loopback IPv4 or IPv6 address is accepted for local tests. Lexical hostnames such as `localhost`
are rejected. Credentials, non-root paths, queries, and fragments in the URL are rejected.

The minimum checkpoint is an out-of-band rollback floor. A size of zero is accepted only with the
RFC 6962 empty-tree root (`e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`);
a nonzero size must not use that root. Production should pin an observed checkpoint instead of
relying on the empty-tree bootstrap value.

The cache verifies the registry signature, fresh witness quorum, exact log entries, inclusion
proofs, and consistency from the pinned or last accepted checkpoint before replacing
`state_path`. Request handlers use only that verified cache. Refresh runs under supervision; once
the last verified checkpoint exceeds `max_staleness_seconds`, or the exact current purpose key is
no longer available after a refresh attempt, the service becomes unready and the supervisor stops
it. Shutdown drains for a bounded interval and then aborts lagging tasks.

Registry clients admit at most four complete audits process-wide without queueing. JSON/NDJSON
decoding, signature and Merkle verification, state replay, handle-projection SQLite, and final
projection checks run in the associated bounded blocking lane. Cancellation retains capacity until
an already-started job exits. The compliance cache shares prior state through `Arc`, so refreshing a
4,096-key projection does not deep-clone it on a Tokio worker.

## Registry witness submission block

The read-side block above says which cosignatures a loft or registry key cache trusts. A registry
operator additionally configures where its own checkpoints are submitted. This is a separate block
because witness submission credentials and topology belong only on the registry host:

```toml
[witnessing]
threshold = 2
max_cosignature_age_seconds = 600
future_clock_skew_seconds = 30
max_lag_entries = 0
poll_interval_seconds = 5
connect_timeout_seconds = 5
request_timeout_seconds = 15
retry_initial_ms = 250
retry_max_ms = 15000
retry_deadline_seconds = 60

[[witnessing.witnesses]]
name = "independent-witness-a"
public_key = "WITNESS_A_ED25519_PUBLIC_KEY_HEX"
submission_prefix = "https://witness-a.example/submission/"
monitoring_prefix = "https://witness-a.example/monitoring/"

[[witnessing.witnesses]]
name = "independent-witness-b"
public_key = "WITNESS_B_ED25519_PUBLIC_KEY_HEX"
submission_prefix = "https://witness-b.example/submission/"
monitoring_prefix = "https://witness-b.example/monitoring/"
```

The prefixes use the C2SP `tlog-witness` endpoints: the operator posts to
`<submission_prefix>/add-checkpoint`. A crash-after-submit conflict is first retried at the
witness-reported size (compatible with stable v1); remaining conflicts are recovered or diagnosed
from the current editor-protocol monitoring route
`<monitoring_prefix>/<sha256(origin)>/checkpoint`. HTTPS is mandatory except for literal loopback
IPv4/IPv6 test endpoints. Redirects, proxy-environment routing, URL credentials, queries, and
fragments are rejected. Response, request, proof, retry, and total-deadline bounds are enforced.

Public binds and every identity-provider mode require this block. Startup performs a bounded sync
and refuses readiness unless the last durable public head has a fresh quorum. New leaves commit to
the private append-only state first, but every public receipt, resolution, range, projection,
checkpoint, and dump stays at the last quorum-cosigned head. A directory polls the authenticated
read side while a leaf is pending, then replays the unchanged signed mutation once after zero-lag
publication; it does not consume the mutation-rate budget while witnesses catch up. The five-second
default poll fits inside the directory's 15-second default publication wait. Exact retries become
final only after their index is below the published tree size. `/health`
returns unavailable when quorum freshness or `max_lag_entries` is violated;
`GET /v1/log/status` reports only readiness, committed/published sizes, lag, and witness time.
The empty-tree transition is the protocol-defined special case: after origin/order checks, a
persisted witnessed size-zero checkpoint advances to the first leaf with an empty consistency
proof.

When `[compliance.registry]` and `[witnessing]` coexist, their witness names, public keys, and
threshold must match exactly. This prevents the registry from publishing under a weaker policy
than the one used to admit its own compliance keys. Matching configuration still does not prove
independence: the listed services must be provisioned and operated by unrelated parties.

The strict-majority check guarantees only that any two accepted quorums from this one roster share
at least `2 * threshold - witness_count` signers. No-gossip split-view prevention additionally
requires fewer equivocating witnesses than that minimum intersection, so every intersection still
contains a witness that refuses inconsistent history. In particular, 2-of-3 tolerates no
equivocating witness; if the only operational assumption is “at least one of N is honest,” use
N-of-N. Clients with different rosters need a guaranteed non-equivocating overlap across every
accepted quorum or an external gossip/out-of-band coordination mechanism.

### Directory publisher authorization

Every witnessed registry also pins the public identity of each directory process allowed to append
loft mutations:

```toml
[server]
directory_publisher_keys = ["DIRECTORY_ED25519_PUBLIC_KEY_HEX"]
```

One to 64 canonical lowercase, strong, unique Ed25519 keys are accepted. A witnessed/full registry
refuses to start with an empty list. This does not gate which lofts may join: the public directory
still accepts every valid loft-self-signed submission into its bounded pending/probe workflow. It
only prevents callers from bypassing that workflow and writing directly to the shared log.

The directory signs a strict v1 request representation with its existing document-signing identity.
That representation length-prefixes the configured registry origin, binds the add/remove operation,
and commits the exact bounded JSON body. The registry selects only an allowlisted key, verifies the
signature before JSON decoding, source charging, readiness checks, or SQLite work, then independently
verifies the embedded loft signature. The HTTP `Host` header is never part of this decision. An exact
retry reuses the same body and authorization; it cannot cross operations or be replayed into another
registry origin even when an operator intentionally reuses a directory key.

For a new directory, provision an owner-only raw 32-byte seed and configure it before first open:

```toml
signing_key_file = "secrets/directory-signing.key"
```

The directory pins that identity in the private database on first open and rejects a later mismatch.
Existing databases may omit `signing_key_file`; they retain the already-persisted signing identity,
whose public key is present in the signed directory document. Add that exact public key to
`directory_publisher_keys` before enabling witnessed registry serving. Never copy the seed to the
registry host; only the public key belongs in `registry.toml`.

### Registry storage quota

Run `registry.db`, `registry.db-wal`, and `registry.db-shm` on one dedicated local filesystem or
volume with a hard storage quota. Ten GiB is the release baseline; reserve additional host space for
backups outside that quota and alert before 80 percent. The quota must cover the database and both
SQLite sidecars, not just the main file. A filesystem/volume quota is required because it remains
effective across process crashes and bounds every file SQLite can grow; an application leaf-count
cap would be irreversible and would eventually disable the public log. When the quota alarm fires,
expand the dedicated volume and its quota—never delete, compact away, or renumber committed leaves.

## Production platform boundary

The online regulatory trace stores are Linux/macOS-only. On those targets, the internal Unix
custody path uses no-follow descriptor opens, owner/mode/link-count checks, stable inode identity,
descriptor-safe directory validation, crash-atomic publication, and durable parent synchronization.

On Windows, the client, CLI private state, Directory database, and a private loopback Loft database
use protected current-user DACLs, no-delete-share handles, complete retained ancestor chains,
reparse/hard-link rejection, and full volume/file identity checks. Any non-test regulatory trace
configuration still fails before directory, key, database, or trace mutation because the segment
writer is Linux/macOS-only. Registry service and executable compliance-key ceremonies likewise
reject all other targets before creating runtime directories, locks, checkpoint state, databases,
journals, or sidecars. Pigeonpost will not write source-address or provider-identity evidence on
Windows in v0.2. Do not work around this by selecting the test jurisdiction: production runtime
validation rejects it. Service installation and `ppcompliance` are also Linux/macOS-only.

Release CI exercises each staged Windows online binary before upload. The harness creates isolated
client state and verifies the protected main/WAL/SHM files, starts a private-loopback Loft through
`/ready`, and starts the exact Directory through `/health` and witnessed `/ready`, checking
owner-private DACLs and cleanup throughout. Because the Registry service is intentionally
Linux/macOS-only, that Directory check uses a read-only, size-zero witnessed Registry fixture. The
fixture tests the Directory's real checkpoint verification path; it is not a Windows Registry
implementation or a production-support claim.

## Public loft

A private loopback loft with `[pool].join = false` may omit compliance configuration. A loft is
treated as public when any of these is true:

- its bind address is not loopback;
- `[pool].join = true`;
- `[pool].domain` is nonempty, including a loopback service exposed through a TLS proxy.

Public startup requires the witnessed block above plus:

```toml
[loft]
bind = "127.0.0.1:7717"
storage_path = "data/loft.db"
capacity_gb = 20
retention_days = 30
trusted_proxies = ["127.0.0.1"]

[loft.policy]
open = true
pow_floor = 0
max_event_bytes = 2097152
allowlist = []

[pool]
join = true
domain = "loft.example"

[compliance.trace]
directory = "/var/lib/pigeonpost-traces/loft-network"
signing_key_file = "secrets/network-segment-signing.key"
max_records_per_segment = 10000
max_storage_gb = 384

[compliance.trace.policy]
jurisdiction = "tr"
capture = "standing"
retention_days = 365
```

This example uses the default loft-wide admission ceiling of 2,400 requests per minute. The TR
policy needs capacity for 365 retained closed UTC epochs plus the current open epoch. At 10,000
records per segment, the conservative network-trace estimate is about 324.27 GiB, so the configured
384 GiB logical budget passes startup with margin. `loft.capacity_gb` is the message-store budget;
`compliance.trace.max_storage_gb` is a separate append-only trace budget.

The server derives one canonical credential origin from this configuration: `https://` plus the
exact `[pool].domain` when present, otherwise the exact numeric-loopback `http://` bind origin.
`/v1/info`, fetch authentication, and capability presentations all use that same value. A
non-loopback listener without `pool.domain`, a noncanonical domain/port spelling, or an origin that
does not satisfy the HTTPS/loopback rule refuses to start; this prevents one endpoint from claiming
another loft's public key and relaying credentials there.

The loft constructs one `WitnessedRegistryKeyCache` for both attribution-key admission and
network-trace key rollover, one `SealedTraceSink`, and uses the loft SQLite store only as the public
`TraceSegmentCatalog`. It then starts through the supervised `pigeonpost_loft::serve` path. Startup
fails if the cache cannot refresh, the checkpoint is stale, no exact current network-trace key is
available, or the signing/trace stores are unsafe. A non-loopback listener additionally checks the
concrete adapter identities: only the audited `SealedTraceSink` plus `SqliteStore` pair may claim
public durable capture, and that SQLite instance must retain an owner-custodied file-backed
database rather than the in-memory test constructor. Custom trait implementations cannot
self-assert that boundary.

Before inspecting a live key or recovering an open segment, the sink acquires the directory's fixed
`.pigeonpost-trace-writer-v1.lock` without waiting and retains its validated descriptor until the
worker has stopped. A concurrent writer, symlink, linked/permissive file, or replaced lock makes
startup/capture fail closed. The persistent lock file is an online coordination artifact: transfer
only the terminal manifest and its declared segments into a dedicated offline epoch directory.
Offline intake rejects the lock file or any other extra entry.

The trace sink owns one writer thread and one fixed 64-record queue. It group-commits at most 32
records after a collection window of at most 2 ms; the server exposes eight fixed blocking capture
lanes so concurrent requests can share a sync without creating an unbounded Tokio blocking queue.
A traced operation proceeds only after the sync covering its exact frame succeeds. Queue saturation
or the configured caller deadline fails that request, while writer failure, worker exit, or panic
poisons `/ready`. Coordinated shutdown stops new trace admission, drains accepted records, durably
finalizes the active segment, and joins the writer thread.

At a UTC-day rollover, a supervised boundary wake runs on the same writer even when no later request
arrives. It verifies every closed segment for the old key and atomically publishes
`network-<canonical-key-id>.ppmanifest` with owner-only permissions before unlinking the live key.
The signed terminal marker fixes the producer, signer, custody-key digest, epoch-key
commitment, exclusive epoch end, totals, and complete ordered segment hashes. Restart repeats this
step idempotently if a crash occurred after segment finalization or manifest publication; a missing,
different, malformed, linked, or incompletely verifiable marker leaves the live key in place and
keeps the sink unavailable.

The file-backed loft database uses SQLite WAL with `synchronous=FULL`. The HTTP service acknowledges
an admitted event only after that transaction commits; reopen tests cover the acknowledged row.

`trusted_proxies` contains exact proxy IP addresses, never client networks. An unlisted peer's
forwarding headers are ignored. A listed proxy must strip client-supplied copies and send a valid
RFC 7239 `Forwarded` source with both address and port; ambiguous or portless data fails closed.
Leave the list empty for direct connections.

The CLI currently uses the bounded `LoftConfig` defaults: 256 live connections, 128 in-flight
requests, a timer-backed five-second incomplete-header deadline, a 15-second handler deadline, and
a 15-second response-body deadline. The transport also closes every connection after the sum of
those deadlines (35 seconds), including a socket that stopped reading after Hyper accepted a large
body frame. Request permits remain attached to response bodies through EOF/error/drop. Fetch pages
are read, assembled, and serialized in the bounded blocking lane; their exact bytes debit the
existing 128 MiB/minute global, 32 MiB/minute effective-source, and 16 MiB/minute recipient byte
budgets before transmission. These egress charges do not manufacture additional request counts and
are not refunded after a later bucket rejects.

Unknown TOML fields are rejected, including in `[pool]`, so a spelling error cannot silently turn
off public-exposure detection. The current server supports only `open = true`, `pow_floor = 0`, and
an empty operator allowlist; other values are rejected instead of ignored. `max_event_bytes` is
wired into the HTTP admission bound and cannot exceed the protocol ceiling. `pool.directory_url`
is accepted only as an empty legacy field; use `pigeonpost loft submit` explicitly to publish or
update a directory entry.

The programmatic `LoftConfig` API is bounded as strictly as the CLI. Its absolute ceilings are
1,048,576 GiB capacity, 3,650 retention days, 2 MiB per event, 2 MiB plus 64 KiB per request, 500
events and 8 MiB per fetch page, 4,096 concurrent connections, 4,096 concurrent requests, 256
blocking operations, 65,536 limiter keys, 300-second header, request, and response deadlines, a
30-second trace handoff, 10,000 rows per retention sweep, a one-day sweep interval, and 64 trusted
proxies. Per-recipient/source request rates top out at
1,000,000 per minute, the global rate at 10,000,000 per minute, and each byte budget at 1 TiB per
minute. Every rate dimension must be nonzero. A full keyed map rejects a new source or recipient in
constant time until its tracked cleanup deadline and runs at most one full scan per rate window; it
never rescans all live keys for every miss. `Loft::new` validates every value and the
trace/request deadline relationship before constructing semaphores or keyed admission state; a
zero, excessive, or internally inconsistent value returns a configuration error without starting.
Those are generic HTTP safety ceilings, not a promise that every value is compatible with sealed
trace evidence. The immutable trace contract carries the complete jurisdiction, capture mode, and
standing-retention selection; startup uses the shared policy validator to recompute the minimum
UTC-key runway before sizing storage. A trace-enabled listener must also fit one terminal manifest per UTC epoch. With
65,536 manifest entries, at most 10,000 records per segment, and 1,441 possible minute windows per
UTC day, the maximum global trace-compatible rate is 454,795/minute. A shorter segment limit lowers
it to `floor(65,536 * max_records_per_segment / 1,441)`. Startup rejects any larger plan.

Public installation is intentionally two phase because the installer cannot invent external trust
material:

```bash
pigeonpost install --domain loft.example --no-service
# Provision the witnessed block, trace block, signing seed, keys, and initial trusted state.
pigeonpost loft serve --dir /srv/pigeonpost
```

Use the actual directory selected by `install`; `/srv/pigeonpost` is illustrative.

## Registry identity-provider mode

A loopback registry without provider credentials may remain a bounded development-only resolver
and transparency-log server. A public bind always requires the witness-submission block. Enabling
GitHub, Google, or the mock test provider activates identity-provider mode and requires both
witness blocks plus:

```toml
[server]
trusted_proxies = ["127.0.0.1"]

[server.limits]
max_concurrent_connections = 128
max_concurrent_requests = 64
max_blocking_operations = 8
max_dump_streams = 4
blocking_timeout_ms = 30000
header_timeout_ms = 5000
global_requests_per_minute = 100
global_response_bytes_per_minute = 268435456
source_challenges_per_minute = 20
source_bindings_per_minute = 40
max_source_keys = 4096
account_bindings_per_minute = 10
max_account_keys = 4096

[compliance.claim_trace]
network_directory = "/var/lib/pigeonpost-traces/registry-network"
identity_directory = "/var/lib/pigeonpost-traces/registry-identity"
network_signing_key_file = "secrets/claim-network-signing.key"
identity_signing_key_file = "secrets/claim-identity-signing.key"
max_records_per_segment = 10000
network_max_storage_gb = 16
identity_max_storage_gb = 16

[compliance.claim_trace.policy]
jurisdiction = "tr"
capture = "standing"
retention_days = 365
```

The registry derives the global claim-admission and trace-planning ceiling as
`min(global_requests_per_minute, 454_795)`, even when identity bindings are expected to be rarer.
The example therefore plans 100 records per minute. With 366 UTC epochs and 10,000 records per
segment, the conservative estimates are about 13.52 GiB for network trace and 14.01 GiB for identity
trace. The two 16 GiB logical budgets therefore pass startup. Raising
`global_requests_per_minute`, extending TR retention, shortening segments, or widening an EU
preservation interval can require much larger budgets; recalculate before rollout instead of
copying these values unchanged.

Witnessed provider serving accepts only the built-in `SealedClaimTraceSink`. Its immutable contract
carries the same complete jurisdiction/capture/retention policy plus the rate, segment limit, epoch
runway, and both purpose caps; Registry independently recomputes the policy runway and both byte
requirements. The explicit loopback test fixture may inject a custom sink, but that surface is not
a production serving path.

Server construction validates this block before allocating semaphores or limiter maps. The audited
hard ceilings are 4,096 concurrent connections, 4,096 concurrent requests, 256 blocking operations,
64 range-dump streams, a 300,000 ms blocking/header deadline, 10,000,000 global HTTP
requests/minute, 1 TiB of response bodies/minute, 1,000,000 challenge or binding requests/minute per
source, and 65,536 retained source buckets. Identity-provider mode additionally
caps global binding admissions at 454,795/minute; a larger read-only HTTP ceiling does not widen
that claim or trace-planning boundary. A shorter `max_records_per_segment` lowers the realizable
rate to `floor(65,536 * max_records_per_segment / 1,441)` and startup verifies it. Zero values or values above a ceiling fail startup; the
ceilings are safety bounds, not recommended production settings.

The Registry origin accepts HTTP/1.1 only. A TLS edge may advertise HTTP/2 to public clients, but it
must use non-multiplexed HTTP/1.1 upstream connections to the Registry. Do not enable an HTTP/2
origin transport: a query-free mirror dump has no absolute total cutoff while it makes socket write
progress, and multiplexed writes from another stream must never mask a stalled dump.

`global_response_bytes_per_minute` is charged before each ordinary response body frame and before
every Registry dump chunk enters Hyper. It covers query-free mirrors and immutable ranges in
one process-wide bucket, so a progressing mirror has bounded origin/metered egress and repeated
range requests cannot bypass the ceiling. A rejected or later-dropped chunk is not refunded. The
one-minute rollover is monotonic-process time; use the default 256 MiB/minute unless measured
capacity and cost controls justify a lower value.

The per-account binding budget is charged immediately after a provider verifies its stable subject
and before claim-trace submission. Its fixed-size in-memory table is keyed by a domain-separated
SHA-256 digest of provider namespace and opaque subject; the limiter never logs or persists raw
subjects. Consequently, rotating source or proxy addresses cannot evade it, while a different
verified account has its own bucket. Expired buckets are evicted after the one-minute window; a
full table fails closed.

The global durable admission values are reservation high-water marks, not counts of successful
claims. Registry and loft each reserve at most 64 slots per `synchronous=FULL` transaction, dispense
only after that commit, and read-validate the singleton before using local remainder. Unused reserve
is intentionally burned by restart, UTC-minute rollover, or a runtime-limit change; this may reject
some requests early but cannot admit beyond the configured trace plan or refund a failed trace.

The source binding budget also covers authenticated directory-log mutations. One healthy witnessed
mutation normally spends two registry requests from the directory host: the initial append returns
a pending receipt, and the unchanged retry returns the final inclusion proof after publication.
The default of 40 therefore preserves the directory's default 20-mutations-per-source budget. If an
operator lowers either limit, `source_bindings_per_minute` must remain at least twice the directory
`source_mutations_per_minute`; the separate per-account budget remains 10 for identity bindings.
For directory mutations, trusted-proxy source resolution and this source charge happen before
publisher cryptography. Allowlist lookup, Ed25519 verification, authorization-before-JSON decoding,
readiness, and SQLite mutation then share `max_blocking_operations`; a forged publisher request can
therefore neither spend crypto at the global HTTP ceiling nor queue unbounded blocking work.

Consumed identity challenges retain their exact committed binding sequence. An exact retry with
the same challenge, handle, bound key, PKCE value, and operation returns that receipt without a
second provider request, trace record, account charge, or durable global-admission charge. This is
the recovery path for a response lost after SQLite committed; any changed field fails closed.

The registry origin in `[compliance.registry].expected_origin` must exactly match `--origin`, and
`registry_checkpoint_key` must be the public key derived from that deployment's `checkpoint.key`.
When compliance is configured, `checkpoint.key` must already exist with owner-only permissions;
the runtime will not invent an identity that cannot match the independently provisioned public
key. Automatic checkpoint-key creation remains available only for a loopback, read-only,
non-compliance, non-witnessed registry.
Network and identity trace directories must be separate and non-nested. Their signing seeds must
differ from each other and from the registry checkpoint seed. The registry exposes no raw HTTP
router. Its high-level serve boundary verifies the actual bound listener and current
witness/registration readiness before constructing routes. The only unwitnessed normal mode is an
explicit loopback-only read surface; it mounts no identity challenge, registration, rotation, or
directory-mutation route. The witnessed service retains bounded blocking/dump lanes, global,
per-source, and stable-account budgets, trusted-proxy source resolution, and exact peer
`ConnectInfo`. Witness publication, key-cache refresh, and HTTP tasks are supervised together.
Every refresh attempt rechecks both the exact current network-trace key
and the exact current identity-trace key. Claim capture itself uses one named worker, a fixed
64-claim queue, and batches of at most 32 claims collected for at most 2 ms. A receipt succeeds only
after both purpose-separated frames are synced; saturation fails immediately, a canceled queued
receipt is discarded before write, and a sync failure or panic poisons readiness. Graceful shutdown
closes admission, drains every accepted claim, finalizes both streams, and joins the worker without
putting a non-cancelable blocking join behind a timeout.

Claim-trace startup canonicalizes and sorts the network and identity directories, then acquires both
nonblocking writer leases before either purpose is recovered. This fixed order prevents swapped
configurations from deadlocking; if either directory overlaps a live writer, startup releases any
partial acquisition and mutates no live key or segment. The outer `registry.lock` remains an
additional service/operator guard, not a replacement for these purpose-directory leases.

At daily closure the registry separately publishes
`network-<canonical-key-id>.ppmanifest` and
`identity-<canonical-key-id>.ppmanifest` in their respective directories. Each marker is signed by
that purpose's segment key and is verified/idempotent before its live key is destroyed. No manifest
contains a filesystem path, and no single artifact or online command combines the two purposes.
Every admitted registry HTTP request also has one fixed 30-second total deadline around body
extraction and handler work. The fail-fast concurrency permit is dropped when that deadline fires,
so a stalled request cannot pin server admission indefinitely.

GitHub configuration is all-or-nothing:

```text
PIGEONPOST_GITHUB_CLIENT_ID
PIGEONPOST_GITHUB_CLIENT_SECRET_FILE=/absolute/path/to/github-client-secret
```

The secret file is mounted read-only in production and must be a nonempty regular file of at most
4 KiB containing one line of visible ASCII with no whitespace. Links are refused. On Unix it must
be owned by the running service account, have exactly one filesystem link, and use mode `0400` or
`0600`; the descriptor identity is checked before any byte is read. Supplying only the public ID or
only the file aborts startup, as does combining `PIGEONPOST_GITHUB_CLIENT_SECRET` with
`PIGEONPOST_GITHUB_CLIENT_SECRET_FILE`.

Direct `PIGEONPOST_GITHUB_CLIENT_SECRET` values are disabled by default because the process and
unrelated child processes could inherit them. The only compatibility path is explicit local
development: set `PIGEONPOST_ALLOW_INSECURE_PROVIDER_SECRET_ENV=1` and bind the registry to a
numeric loopback address. A non-loopback listener still refuses that source. Production preflight
rejects the flag and any direct secret variable before launching a subprocess. Google uses only
the public `PIGEONPOST_GOOGLE_CLIENT_ID`. Mock identity providers are not compiled into production
Pigeonpost binaries. They exist only behind the explicit source-test feature and production
preflight rejects both the retired and source-test mock flags.

## Directory HTTP and readiness

`directory.toml` accepts the witnessed `[registry]` block above plus bounded HTTP admission:

```toml
witness_wait_seconds = 15
signing_key_file = "secrets/directory-signing.key"

[server]
trusted_proxies = ["127.0.0.1"]

[server.limits]
max_concurrent_connections = 128
max_concurrent_requests = 64
max_concurrent_mutations = 1
max_blocking_operations = 16
header_timeout_ms = 5000
request_timeout_ms = 30000
response_timeout_ms = 15000
blocking_timeout_ms = 5000
global_requests_per_minute = 6000
global_response_bytes_per_minute = 268435456
source_requests_per_minute = 600
source_response_bytes_per_minute = 67108864
source_mutations_per_minute = 20
loft_mutations_per_minute = 10
max_rate_keys = 4096
```

The proxy rules are identical to the registry contract: entries are exact proxy IPs, an unlisted
peer's forwarding headers are ignored, and a listed peer must supply one bounded RFC 7239
`Forwarded` chain with an exact source port. `X-Forwarded-For`, duplicate/ambiguous `for`
parameters, portless sources, and invalid chains fail closed. Production serving installs Axum
`ConnectInfo`; the connection and request limits reject excess work instead of queueing it.
`header_timeout_ms` is installed in Hyper with a Tokio timer, `request_timeout_ms` bounds handler
work including witnessed append/polling, checkpoint verification, and the local commit, and
`response_timeout_ms` bounds the body and graceful write drain. Their sum is also an absolute
connection lifetime, so a body already yielded to a socket cannot retain admission forever. The
request permit is owned by the response until EOF/error/drop. `/health` skips peer/source parsing
because it is liveness-only, but it still consumes the shared request semaphore, global request
bucket, handler deadline, response lifetime, and connection boundary; HTTP/2 health streams cannot
multiply a separate admission lane. Signed directory and measurement
documents are assembled, signed, serialized, and hashed in the bounded blocking lane, checked
against 2 MiB, and charged by exact body bytes to the global and effective-source response budgets
before send; a conditional `304` has no body charge. Directory limits reject more than 4,096 live
connections or requests, more than 300 seconds for any transport/handler deadline, more than 65,536
rate keys, or more than 1 TiB/minute in either response-byte dimension. Once a keyed map is full,
new misses reject in constant time until the tracked cleanup deadline; cleanup performs at most one
full scan per rate window.
SQLite never runs on a Tokio worker in these paths: request and recovery operations share the
configured fail-fast `max_blocking_operations` lane and `blocking_timeout_ms`; prober lease,
retention, and result bookkeeping use a separate single-operation supervised blocking lane. A
registry-backed router must be constructed inside the active Tokio runtime so its reservation
recovery supervisor cannot be omitted.

Before an add or drain can be signed for registry publication, the directory commits an exact
schema-4 reservation after a full transactional local-transition preflight. Outstanding
reservations count against the 4,096 pending/candidate budget when they would add load, fence their
endpoint against probing/expiry/other mutations, and are not routable projections. A witnessed
receipt is finalized by atomically consuming the reservation, applying the projection, and
advancing the local registry checkpoint. Cancellation, restart, and ambiguous registry responses
are recovered by exact idempotent replay; `/ready` stays unavailable until the reservation table is
empty. Per-loft drain charging happens after a read-only signature/key preflight and before the
reservation transaction, which repeats all validation.

`GET /health` proves only that the HTTP task is alive. `GET /ready` additionally checks SQLite,
requires a supervised-prober heartbeat no older than three five-minute intervals, and verifies a
fresh witnessed registry publication with zero committed/published lag. It first reconciles every
outstanding exact mutation reservation and fails closed if recovery cannot finish. Compose uses
`/ready` for the directory healthcheck.

The directory database contains the directory signing seed and retention-canary recipient seeds,
so it is itself a private-custody file. On Linux/macOS, the internal Unix layer opens it without
following the final link, requires a regular current-user-owned `0600`-or-stricter single-link
inode, holds and verifies the current-user-owned `0700` parent descriptor, compares stable
device/inode identity, and retains the main/WAL/SHM descriptors through SQLite shutdown. On Windows
it requires protected current-user-only DACLs, rejects remote volumes, reparse points and hard
links, compares full volume/128-bit file IDs, and retains no-delete-share handles for every ancestor
plus main/WAL/SHM.
Both paths reject an unsafe pre-existing rollback journal before SQLite opens, verify SQLite's
reported connection path, and reject unsafe newly created sidecars before serving. Existing unsafe
state is never silently blessed. Keep the database and its sidecars on one local filesystem that
preserves these guarantees; do not mount it through a link or junction. The production serve
boundary revalidates every retained name and identity before the listener and on every public
readiness check. `Directory::in_memory` is confined to unit tests and the loopback-only read
fixture; it cannot self-assert public durability.
Persistent directory SQLite runs in WAL mode with `synchronous=FULL`; both the pre-publication
reservation and the post-receipt projection/checkpoint commit must survive power loss before their
corresponding network action or success response can escape.

## Database upgrade gate from v0.1.0

Stop each service and take a tested SQLite/WAL-aware backup before allowing the new binary to open
its database. The two deployed v0.1.0 shapes have different authorization rules:

- The v0.1.0 registry did not set `user_version` and stored unversioned handle rows. An empty database may
  upgrade directly. A nonempty database is refused unless `registry serve` receives
  `--legacy-checkpoint <file>` (or `PIGEONPOST_LEGACY_CHECKPOINT`) containing the last checkpoint
  signed by the same `checkpoint.key`. Its origin, size, and reconstructed RFC 6962 root must match
  exactly. A bad/missing checkpoint or unknown row kind leaves the old schema untouched. A
  successful import keeps `legacy_entries_v0` for audit and records the authorization checkpoint.
- Registry schema 5 upgrades transactionally to schema 6 by rebuilding the directory projection as
  independent `(endpoint, loft key)` streams from the immutable log. This removes projection-level
  endpoint pinning without deleting or rewriting a competing historical claim.
- Registry schema 6 upgrades transactionally to schema 7 by adding one durable global
  identity-binding admission window. Every supported existing predecessor, including an authenticated
  unversioned v0.1.0 import, marks its current UTC minute fully spent at the 1,000,000 hard ceiling,
  so the identity-binding path cools down until the next minute instead of granting a second
  unaccounted window. Only a genuinely fresh schema-7 database begins empty. Subsequent admissions
  reserve and burn durable slots before trace/log work; restart cannot reset the window, and a
  backward clock step fails closed until wall time catches up.
- Registry schema 7 upgrades transactionally to schema 8 by replacing the ephemeral challenge
  table with a result-bearing shape. Challenge consumption and the handle append now commit in one
  transaction, and the consumed row records the exact binding sequence so a timeout can recover
  the same receipt. Outstanding pre-upgrade challenges are invalidated because they cannot
  retroactively prove such a result.
- The v0.1.0 directory shape upgrades automatically in one immediate transaction. It preserves
  release-shaped signed entries and probe rows, adds monotonic mutation/probe scheduling fields,
  backfills probe health and durable ownership evidence, creates the signing/meta and retention
  tables plus the parallel pending-candidate table and the bounded exact-mutation reservation table,
  verifies the final shape, and only then sets schema version 4. An exact schema-3 predecessor is
  verified before it can add the reservation table. Verification compares canonical
  `sqlite_schema` table and explicit-index SQL against generated pristine and exact-release
  references, including declared types, `NOT NULL`, defaults, primary/unique keys, `CHECK`
  constraints, and index definitions; same-column but weakened or operator-modified schema-3/4
  shapes are refused without changing `user_version`. Existing active,
  degraded, draining, or successfully probed rows remain key-bound; never-proven pending/removed
  rows remain releasable after expiry. Malformed legacy data, an unknown unversioned shape, or a
  future version rolls back/refuses without a partial migration.

Run the release migration fixtures against the exact candidate binary before touching a production
copy, then start one service at a time and require `/ready` before continuing. Never point the
compliance-key operator at a legacy registry as a migration shortcut.

## Offline compliance-key publication ceremony

The registry has no public compliance-key write route. Publication is an operator ceremony that
opens the local registry database with the existing checkpoint signing key while the public
registry process is stopped. The operator workstation still needs outbound access to every
configured C2SP witness. Both commands hold the same nonblocking `registry.lock` (regular,
single-link, and owner-only on Unix); either command refuses immediately if the server or another
ceremony already holds it. Do not delete or bypass the lock. A pre-upgrade v0.1.0 server does not
know about this lock, so it must still be stopped and
verified separately before its migration. Before any executable run:

1. Stop the registry and confirm it is offline. Take a restorable SQLite backup with SQLite backup
   tooling or a storage snapshot that accounts for the WAL, then test that backup separately.
2. Verify that `checkpoint.key` has an independently stored, owner-only 32-byte backup. The command
   requires its absolute path, verifies identical contents without printing them, and rejects a
   symlink, hard link, path inside the registry directory, or mismatched copy.
3. Obtain the custody public key and canonical typed key id through the approved custody ceremony.
   Private custody key material never enters this command or the registry host.
4. Review `registry.toml`. A complete `[witnessing]` block and at least one pinned
   `server.directory_publisher_keys` entry are mandatory for subsequent witnessed serving; if the
   registry also has `[compliance.registry]`, its checkpoint key, witness set, and threshold must
   match exactly.

Run the command once without `--execute`. This validates the canonical id, typed fields, interval,
and X25519 public point, prints a machine-readable preview, and does not open the database:

```text
pigeonpost --json registry compliance-key publish \
  --dir /srv/pigeonpost/registry \
  --origin pigeonpost.dev/registry \
  --key-id <94-lowercase-hex> \
  --confirm-key-id <the-same-94-lowercase-hex> \
  --checkpoint-backup /offline-or-separate-volume/checkpoint.key \
  --purpose attribution \
  --jurisdiction tr \
  --authority <64-lowercase-hex> \
  --epoch-start-ms <utc-month-start-ms> \
  --generation <u32> \
  --public-key <64-lowercase-hex-x25519> \
  --not-after-ms <next-utc-month-start-ms>
```

For `attribution`, the validity interval is exactly one UTC calendar month. For `network-trace`
and `identity-trace`, it is exactly one UTC day. The start must equal the epoch in the typed id;
uppercase/noncanonical hexadecimal, non-contributory X25519 points, all-zero authorities, and the
test jurisdiction are rejected. An executable run repeats the command with `--confirm-offline`
and `--execute`.

The append, projections, Merkle nodes, and operator checkpoint commit atomically before witnesses
are contacted. Success means the leaf's index is below the durable witnessed published size. If
the witness deadline expires, the command emits `result=committed_unwitnessed` and exits nonzero;
restart the ceremony with the exact same command. Exact retries reuse the committed index and do
not append a duplicate leaf.

Retire or revoke an existing witnessed key without resupplying or redefining its immutable public
key and interval:

```text
pigeonpost --json registry compliance-key transition \
  --dir /srv/pigeonpost/registry \
  --origin pigeonpost.dev/registry \
  --key-id <94-lowercase-hex> \
  --confirm-key-id <the-same-94-lowercase-hex> \
  --checkpoint-backup /offline-or-separate-volume/checkpoint.key \
  --status retired \
  --confirm-offline --execute
```

Only `active -> retired`, `active -> revoked`, and `retired -> revoked` are accepted. Repeating the
same target state is an idempotent witness-resume operation. Archive the JSON result with the
change authorization: it contains only public identifiers, status, immutable log index,
committed/published roots and sizes, and witness time—never custody secrets or witness endpoints.

## Signing seed files

Each `*_signing_key_file` contains exactly 32 raw seed bytes. It must already exist, be a regular
non-symlink file, and be owner-only (`0600` or stricter on Unix). The runtime never creates these
files. Segment signing keys authenticate both public segment structure and the terminal manifest
for that purpose; they are separate from the offline custody secrets that can unwrap epoch keys.

## Jurisdiction and capture policy

- `jurisdiction = "us"` requires `capture = "standing"`, fixes effective retention at 30 days,
  and forbids preservation dates. `retention_days` may be omitted or set to exactly `30`; no other
  value is accepted.
- `jurisdiction = "tr"` requires `capture = "standing"`, an explicit `retention_days` from 365
  through 730 inclusive, and no preservation dates.
- `jurisdiction = "eu"` requires `capture = "preservation"` plus
  `preservation_starts_at_ms` and `preservation_expires_at_ms`, with a nonempty bounded half-open
  interval. `retention_days` is forbidden.
- The test jurisdiction is rejected by the operator CLI.
- `max_records_per_segment` must be between 1 and 10,000.

Standing policies size the append-only online store for `retention_days + 1` UTC epochs: the closed
history plus the current open epoch. US therefore sizes 31 epochs and TR sizes 366 through 731.
EU sizing counts every UTC epoch intersected by `[preservation_starts_at_ms,
preservation_expires_at_ms)`, including partial boundary days. The loft requires
`max_storage_gb`; the registry requires independent `network_max_storage_gb` and
`identity_max_storage_gb`. Each value is GiB (`1 GiB = 1,073,741,824 bytes`) and must cover the
conservative estimate for the configured global admission rate, epoch count, frame size, segment
headers/footers, terminal manifests, live recovery key, and terminal reserve.

These fields are fail-closed logical append budgets. They do not delete records at a legal deadline,
prove legal retention, measure physical filesystem blocks, or establish a hard host quota. Keep
loft-network, registry-network, and registry-identity traces in separate, non-nested directories,
outside the SQLite role volumes. Put each directory on an independently quota-managed filesystem,
dataset, or project whose physical quota exceeds the application budget by enough for filesystem
overhead and alert headroom. Compose bind-mount separation and a successful startup capacity check
are necessary evidence, but neither proves that host-side quota enforcement exists.

Selecting a jurisdiction, authenticating preservation authority, publishing current
purpose-separated custody public keys, operating independent witnesses, provisioning the initial
verified cache, and approving retention/custody are external production prerequisites. Startup
checks their machine-verifiable artifacts; it cannot truthfully manufacture their legal or
organizational independence.
