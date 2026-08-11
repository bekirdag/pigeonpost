# Pigeonpost — Handoff

Reviewed 2026-08-10 against the current SDS and working tree. This is what a new maintainer needs
that is not obvious from the code, ordered by when you will need it. It is a source-tree handoff,
not a package, deployment, custody, witness, or regulatory-status attestation.

## 1. What exists, honestly

The tree contains the client, CLI, loft, registry, directory/prober, MCP, spam controls, and the
compliance mechanics described by the SDS. The important M6 accounting is now:

| Phase | State |
| --- | --- |
| **P0** — Google identity used a mutable contact-derived id | Implemented: claims bind the provider's stable opaque subject |
| **P1** — independently verifiable sender attribution | Implemented in envelope v3: typed key id/digest, fixed claim, sender signature, event/recipient/time binding, and `Valid | Invalid | Absent` results |
| **P2** — separated compliance packages | Implemented: canonical formats, online-only sealing, and the separate offline `ppcompliance` custody/disclosure operator |
| **P3** — loft attribution and network traces | Implemented: the exact signed recipient jurisdiction/authority gate and Active-key rule are enforced at publish, and in-scope requests use a fail-closed sealed trace handoff |
| **P4** — registry/client/runtime integration | Implemented: versioned `ComplianceKeyPublish` history and APIs, witnessed full-log key derivation, recipient-signed AgentRecord v2 discovery, RecipientPolicy v3 enforcement, explicit sender agreement, client attribution construction/verification, purpose-separated registry claim traces, and strict runtime configuration |

Envelope v3 is the only write format. Unattributed v2 remains a compatibility read path; attributed
v2 is never compliance-valid, and v1 is unsupported. Do not reintroduce a statement that the client
never calls `wrap_attributed`, the Loft ignores the exact signed attribution requirement, or the
compliance package and Registry entry kind do not exist—the source now proves the opposite.

The current SQLite contracts are client v15, loft v6, directory v4, and registry v8. Client v13/v14
add exact local storage accounting, immediate finished-copy payload erasure, explicit deletion and
pruning, permanent id-only replay tombstones, authenticated loft-retention state, and the indexed
handle-public-key path. Client v15 adds the exact signed-scope resolution column and separate exact
sender/recipient settings; legacy boolean/jurisdiction-only configuration fails closed. Loft v5/v6
add exact control/reservation accounting and durable UTC-minute
trace admission. Registry v7 adds the equivalent durable global verified-binding admission; v8
makes challenge consumption plus binding append atomic and records the exact result sequence. The two
online services charge before trace mutation, do not refund later failure, fail closed on clock
rollback, and conservatively burn every supported predecessor migration minute instead of granting a restart
window. Both counters are 64-slot durable reservation high-waters, not success counters: every
local dispense revalidates SQLite, and unused reserve burns across restart, rollover, or limit
change, so safe early rate limiting is expected under those events.

Schema v15's exact requirement is a fixed 34-byte codec. Writes and cached-resolution reads reject
unknown versions or jurisdictions, zero authorities, wrong lengths, and trailing bytes. The hot
current-schema open remains constant in cache-row volume; migration is transactional and leaves the
prior version untouched on failure.

Public/provider serving requires a validated trace-capacity contract, not merely a writable
directory. Startup recomputes each conservative purpose budget from the exact enforced per-minute
ceiling, segment limit, UTC-key runway, complete jurisdiction/capture/retention policy, and
configured logical byte cap. Public Loft and witnessed Registry paths also require the exact
built-in sealed trace adapter (and the Loft's durable SQLite adapter), so a custom trait
implementation cannot self-assert production durability. The Loft SQLite and witnessed Registry
databases, plus the public Directory database containing its signing and canary seeds, must be
owner-custodied persistent files; in-memory/temporary SQLite remains test-only. On Linux/macOS, the
internal Unix custody path is descriptor-relative. Windows client, Directory, and private-loopback
Loft custody retains protected current-user handles for the complete ancestor/main/WAL/SHM chain
and validates rollback journals; regulated Loft traces, Registry service/operator paths, service
installation, and the offline operator are Linux/macOS-only and reject all other targets before
mutation. Each public start revalidates the held names and identities at startup and readiness, so
replacing a file, sidecar, or parent fails closed; only the Directory's explicit loopback read
fixture accepts in-memory state. Registry validates both
network and identity caps again at the public serving boundary. Production Compose requires
distinct preprovisioned host binds for all three role-data paths. Loft network traces and Registry
network/identity traces are three additional separate operator mounts; ordinary Docker named
volumes do not prove the SDS hard-quota requirement.

**Implemented does not mean activated.** The repository cannot prove that external custody has been
provisioned, two-person approval rosters are real, counsel and jurisdiction gates are complete,
independent witnesses are operated by unrelated parties, release artifacts are published, or any
public service is deployed. `law.md` §8 and the operator runbooks are the gates. Describe source
mechanics and operational facts separately.

The code verifies strict-majority witness quorums and fails closed when freshness or consistency is
missing. That proves set intersection for one shared roster, not an honest intersection or an
independently operated fleet. No-gossip split-view prevention requires fewer equivocating witnesses
than `2k - N`; 2-of-3 therefore tolerates no equivocator, while N-of-N is required if the only
assumption is that at least one witness is honest. Different client rosters need a guaranteed
non-equivocating overlap or gossip/out-of-band coordination. Do not describe the registry as
independently witnessed until operator evidence names unrelated witnesses, records their observed
checkpoints, and states this fault assumption.

## 2. Read the docs in this order

`docs/` is the design, and it is unusually load-bearing — most of the non-obvious code exists
because a document argued for it.

1. **`product.md`** — what this is, what it deliberately is not, the nine requirements. Every
   component traces to a numbered requirement; anything that traces to nothing gets cut.
2. **`architecture.md`** — the two-tier namespace. This is the core decision.
3. **`sds.md`** — the build spec: crates, data models, milestones, testing.
4. **`keys.md`**, **`spam.md`**, **`network.md`**, **`capacity.md`** — read when you touch those areas.
5. **`law.md`** — read before touching `envelope.rs`, the loft's storage, or the registry.
6. **`compliance-operations.md`** — the exact offline operator/configuration/adapter contract;
   read it before changing `ppcompliance` or planning a regulated deployment.

`private/` is gitignored. If operator-only bootstrap or deployment records are handed over through a
separate channel, treat those—not tracked prose—as the source of operational truth. Never copy
hosts, addresses, credentials, or key locations into the repository.

## 3. Repo map

```
crates/
  pigeonpost-compliance-format/ canonical public evidence formats and typed key ids
  pigeonpost-compliance-seal/   online trace sealing; no private-key operations
  pigeonpost-compliance/        offline custody, approvals, disclosure, holds, shredding
  pigeonpost-unix-custody/      internal descriptor-relative Unix private-state primitives
  pigeonpost-windows-custody/   internal retained-handle Windows private-state primitives
  pigeonpost-core/        addressing, keys, envelope v3, v2 read compatibility, PoW, tokens
  pigeonpost-loft/        the durable inbox: server and client halves in one crate
  pigeonpost-client/      agent state, outbox, cursors, spam layers (SQLite)
  pigeonpost-registry/    handles over an RFC 6962 transparency log
  pigeonpost-directory/   the pool, the prober, capacity-weighted selection
  pigeonpost-mcp/         the MCP tool surface over the client
  pigeonpost-cli/         one binary, every role
deploy/                   Dockerfile + compose; see deploy/README.md
npm/                      provenance/checksum-enforcing launcher source
```

Every Rust workspace package is internal and declares `publish = false`; do not create a crates.io
release lane. The single public package is the `@bekirdag/pigeonpost` npm launcher.

One binary with subcommands. Client verbs (`id`, `send`, `inbox`, …), operator verbs (`install`,
`loft submit`, `loft drain`), and server verbs (`loft serve`, `registry serve`, `directory serve`).

## 4. Run the whole thing locally

```bash
cargo test --locked --all --all-targets --all-features
cargo test --locked --all --doc --all-features
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo fmt --all --check

# The release-binary gates create disposable homes and allocate their own ports.
./deploy/acceptance/local.sh
./deploy/acceptance/install-defaults.sh # Linux; use --container on other hosts
./deploy/acceptance/proxy-privacy.sh
./deploy/acceptance/migration-rollback.sh
```

`local.sh` covers two isolated agents, online/offline/retry delivery, a real stdio MCP client, and a
composed witness/registry/directory/two-loft topology. A production directory correctly refuses to
promote loopback HTTP lofts, so the isolated topology proves that fail-closed boundary and exercises
two-loft delivery through explicit local trust. Capacity-weighted selection of public HTTPS lofts is
a post-deployment acceptance gate; do not add a release-binary environment bypass merely to make a
local prober report green.

Mock identity providers exist only behind the explicit `test-utilities` source-test feature. They
are absent from production binaries, and production preflight rejects both the retired and
source-test mock flags. Never weaken that compile boundary to make a deployment test convenient.

## 5. Decisions where the obvious move is wrong

These cost real time to work out. Changing them is fine — knowing they were deliberate is not
optional.

**`Wrap::id()` excludes `pow_nonce` and `attribution`.** Mining varies `pow_nonce`, so an id that
included it could never be ground against. Attribution is excluded because the trace log keys on
`event_id`, and an id that shifted depending on whether a block was attached would break exactly
the correlation that design exists to provide.

**The wrap signature covers every field that feeds `id()`.** Version, recipient, and created_at
included. Anything inside the id but outside the signature lets a hostile loft mutate a message
into a different event id, breaking cross-loft dedupe and trace correlation both.

**V3 attribution is built after the stable event id.** The event id excludes the attribution block,
outer signature, and PoW nonce, so it can be fixed without a cycle. The attribution AEAD context and
sender signature bind that event id, recipient, typed key id, key digest, ephemeral key, and send
time; the final one-use outer signature then covers the complete block. This is why stripping,
copying, or substituting a block fails without changing the dedupe/trace join key.

**True send time selects the attribution month; visible time does not.** `created_at` is jittered
backward by up to two days and can cross into the previous month. Verify the sender-signed
`sent_at_ms` against the key epoch and use visible time only to bound the permitted jitter window.

**`Attribution` is an enum, not a bool.** A forged block is an attempt to look compliant, which is
a stronger adverse signal than no block. Core reports an epoch the caller does not supply as
`Invalid`, never `Valid`; the required product path first distinguishes unavailable Registry
knowledge from an explicitly invalid block so temporary uncertainty cannot destroy a message.

**Attribution scope belongs to the recipient, while consent belongs to the sender.** The stable
jurisdiction/authority pair is signed into AgentRecord v2 and RecipientPolicy v3. Resolution exposes
it but never opts the sender in. Prefer the call-local send agreement for concurrent recipients;
mutating one process-wide default around each send creates a cross-recipient race.

Recipient scope changes use the active-identity lease across local mutation, Loft policy sync, and
AgentRecord publication. Drain holds that same lease for its whole wake. Concurrent processes thus
fail fast before mutation instead of letting a page validated under the old scope land after a
completed change.

**`Active` is for new escrow; `Retired` is history only.** A Loft rejects a newly arriving
attributed wrap under a retired key even if its signature and epoch are otherwise valid. Recipients
and custodians may verify already-created wraps under retired history, while revoked material is
always invalid. An immutable outbox wrap that first reaches a Loft after retirement becomes a
terminal dead letter; create a new explicit send under the current active key rather than rewriting
signed ciphertext.

**Unknown is not revoked.** A fresh witnessed Registry prefix is proof of that prefix, not proof
that no later leaf exists. If a required wrap names a same-scope key absent from the recipient's
prefix, Registry outage leaves that Loft cursor unchanged; a consistency-verified refresh may then
recover the same ciphertext. The route-local deferral marks only that Loft failed for the wake and
continues draining every other Loft. At Loft admission, resolver/readiness failure or an unknown
cache key is HTTP 503 and keeps the outbox retryable. Wrong scope, malformed cryptography, or a
witnessed `Retired`/`Revoked` admission key is HTTP 400 and terminal. Optional attribution remains
non-blocking: unresolved blocks are `Invalid` so hostile optional metadata cannot pin plain traffic.

**Sender-keyed anything is client-side.** Gift wrapping hides the sender from the loft. The loft
enforces a *flat* per-recipient PoW floor; the per-sender gradient in `spam.md` is applied after
unwrap. Any design that has the loft reasoning about senders is wrong on its face.

**Signed policy and record preimages are built field by field.** Add a field to either struct without
adding it to the matching versioned payload and a cache or Loft can flip it while merely holding it.
The exact attribution requirement has tamper and legacy-fixture tests; add the same protection for
whatever signed field comes next.

**Capacity is a budget, not free disk.** A full loft refusing writes is correct behaviour, not a
bug — `capacity.md` explains why our cost has to be a number we choose.

**Trace capacity is runway, not retention.** The online writers never delete sealed epochs and their
logical caps are not filesystem quotas. US/TR/EU policy determines how many canonical UTC-key epochs
must be provisioned; offline inventory/holds/shredding enforce legal lifecycle. Production mounts
must supply independent hard-quota and free-space evidence.

**Loft selection is weighted-random, sticky, and operator-diverse.** Best-first stampedes whichever
node looks best that hour. Stickiness means relief applies to growth, not installed base.

**`UntrustedBody` never becomes a `String`.** `Debug` withholds the contents so messages cannot reach a
log line by accident. `integration.md` commits to this at every version, in every binding.

## 6. Gotchas that cost hours

- **A loft sitting at `pending` is correct**, not broken. Promotion needs 24 h of clean probes. The
  prober logs success at `debug` and the default filter is `warn`, so a healthy prober is silent.
  I called it broken once by checking before the five-minute interval came round.
- **A row in `directory_mutation_reservations` is committed work, not stale clutter.** The directory
  persisted the exact locally admissible add/drain before registry publication. It is deliberately
  non-routable and fenced from probes/expiry; the bounded supervisor must exact-replay and
  transactionally finalize it before `/ready` succeeds. Never delete a reservation to repair
  readiness, and never move registry publication ahead of that `synchronous=FULL` commit.
- **`npm publish a/b` reads a bare two-segment path as `github:a/b`** and tries to fetch it. Use
  `./a/b`.
- **The official v0.2 image uses current-stable Trixie; Bullseye is only a static-asset execution
  floor.** Debian 11 LTS ends on 2026-08-31, so old-seccomp compatibility cannot justify freezing an
  expiring production base. Production needs the Compose, Buildx, exact-digest, and provenance
  capabilities enforced by the preflight; upgrade or replace Docker 18.09-era hosts that cannot
  prove that contract. Never disable seccomp.
- **`scp` fails silently to old sshd.** Modern macOS scp speaks SFTP; it fails with a bare
  `Connection closed` and the *next* command can still look successful. Use `cat | ssh 'cat >'`.
- **Read-only and operator commands must not create an identity.** `handle resolve`, `handle
  checkpoint`, and `loft submit` run before identity setup. Minting keys in someone's home as a
  side effect of a lookup is a papercut that has been fixed twice already.
- **Handle recovery is an explicit rebind, not another claim.** Use `pigeonpost handle rotate` or
  MCP `pigeonpost_rotate_handle`; both require a fresh provider proof and the new key's signature,
  then wait for the exact `handle_rotate` leaf at a witnessed head. From a fresh home this restores
  future routing only—the lost key address, local state, and old ciphertext remain lost.
- **The trace writer lock is persistent state, not stale clutter.** Never delete or rotate
  `.pigeonpost-trace-writer-v1.lock`; a sink holds its stable inode for its full lifetime and a
  second writer must fail immediately. Export an offline epoch by copying only its terminal
  manifest and declared segments. Copying the lock makes strict offline intake reject the bundle.
- **A schema upgrade burns the current admission minute on purpose.** Loft v5→v6 and Registry v6→v7
  cannot know how much their predecessor admitted before restart. Refusing new traced work until the
  next UTC minute is the only conservative migration; changing the singleton to zero recreates the
  restart-bypass vulnerability.

## 7. Conventions

- **Tests assert the design's claims, not just the code's behaviour.** `the_loft_cannot_read_what_it_stores`
  serialises what the operator holds and greps for the plaintext and the sender's key.
  `a_rewritten_log_fails_its_consistency_proof` forges history and confirms a witness catches it.
  When you add a mechanism, add the test that would fail if the claim were false.
- **Verify a test fails against the broken code before keeping it.** The P0 tests were checked this
  way; two M2 tests were not, and passed while proving nothing until they were rewritten.
- **Stage an explicit allowlist, then review every staged path and byte.** Never use a broad staging
  command in this repository: local identities, planning records, private operations material, and
  unapproved assets may coexist with product work. Add tracked changes and each intended new path
  deliberately, confirm ignored/private paths remain absent, then run the repository's secret
  scanner (when available) and inspect the complete staged patch:
  ```bash
  git diff --cached --name-status
  git diff --cached --check
  git diff --cached --no-ext-diff --binary
  ```
- **Conformance vectors are a protocol commitment.** Changing a value in
  `crates/pigeonpost-core/tests/conformance.rs` without a version bump is wrong.

## 8. What to do next

Keep source completion and operational activation as two workstreams:

1. **Finish the whole-workspace and end-to-end acceptance gates.** Preserve exact command output in
   the ignored progress record; do not replace evidence with an aggregate test count.
2. **Exercise the release acceptance matrix.** Cover two online agents, an offline recipient,
   durable outbox retry, recipient return, duplicate loft delivery, and failure of one loft. Run it
   against local services before using any public endpoint.
3. **Establish independent witness operations.** A configured strict-majority threshold is useful
   only when unrelated operators actually observe and cosign the log. Record identities, keys,
   checkpoint provenance, freshness, the assumed equivocator bound (`f < 2k - N`), client-roster
   alignment, and an equivocation drill.
4. **Provision offline custody and legal-process operations.** External key custody, approver
   rosters, inventories, intake authentication, counsel decisions, and jurisdiction policy are
   non-code prerequisites. Test restore, hold, disclosure, and shred ceremonies without using
   production records.
5. **Release and deploy only from verified artifacts.** Follow `publishing.md` and
   `deploy/README.md`, record exact digests and rollback state, and do not infer publication or
   service health from this repository.

## 9. Things I would push back on

Stated so you can disagree deliberately rather than inherit them silently.

- **Attribution escrow weakens requirement 6** ("not controlled by us once adopted"), and `law.md`
  §3.3 says so. A fork that strips attribution is legitimate and should be expected. Make sure
  whoever describes this product externally knows that.
- **Configured witnesses are not independent-witness evidence.** Until unrelated operators are
  identified and their checkpoints are observed, the transparency log's strongest operational
  property is unproven.
- **The registry is a single log on a single box.** `infrastructure.md` commits to the five day-one
  properties that keep a second operator possible; they are cheap now and impossible to retrofit.
  Check them before shipping anything that touches the registry.

### Resolved directory-operator-label boundary

The current directory entry's optional `operator` string is signed only by the loft key. That proves
the loft asserted a label; it does not prove the owner of the named Pigeonpost handle authorized the
loft. The safe behavior already treats the probed endpoint host as the mandatory failure domain and
lets an unverified label only collapse more candidates, never create diversity.

The SDS now settles this conservatively: `operator` is advisory and may only collapse additional
candidates; it never replaces the authenticated endpoint host, expands eligibility, or counts as an
attestation. A future handle-authorized operator statement would require a new versioned signed
format, witnessed handle state, and an explicit migration decision. It is not silently inferred by
the current release.
