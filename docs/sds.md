# Pigeonpost — Software Design Specification

Status: build specification. Supersedes the "pre-implementation" posture of the design docs.
Opened: 2026-08-07

The design docs answer *what* and *why*. This answers *what gets built, in what order, and how we
know it works*. Read `architecture.md`, `keys.md`, `spam.md`, `network.md` first — this document
does not re-argue those decisions, it implements them.

## 1. Settled decisions

Four questions were open across the design docs. All four are now closed.

| Decision | Resolution |
| --- | --- |
| **Build transport fresh vs. contribute to Block's Buzz** | **Build fresh.** Pigeonpost is its own product and its own service network. Buzz is workspace-shaped and would pull the design toward a chat product; we would spend the same effort adapting it that we spend writing a loft, and inherit a roadmap we do not control |
| **Docdex sequencing** | **Out of scope for this build.** Docdex integration is owned by the Docdex maintainer and will be built against the published client surface (`integration.md`) on their own schedule. Nothing in this plan waits on it, and nothing here should be shaped by it |
| **Sender attribution** | **Escrowed to per-epoch compliance keys, verified by the recipient** (`law.md` §3). A single master key was rejected: one compromise retroactively deanonymizes every sender of every stored message across the network, permanently. Monthly epochs bound the blast radius, and destroying an epoch's private key makes that month's blocks undecryptable by anyone. **This is a product decision, not a legal requirement** — no US, EU, or Turkish instrument compels it, and §2.1's sealed trace log already satisfies every duty we found |
| **Jurisdictional retention** | **Counsel-selected 365–730 days TR, 30 days US, preservation-only EU** (`law.md` §2.2). Türkiye's Law 5651 Art. 5 is the only conditional standing duty identified among the three; counsel must confirm classification and select the current period before activation. The exact choice and approval commitment live in versioned inventory state, not a source-code default. The statute also requires accuracy, integrity and confidentiality of retained data, which is why the trace log is sealed rather than plain. In the EU, retention without a mandate is the violation, not the safe choice |

**Patterns, not wire compatibility.** The envelope follows the Nostr gift-wrap *pattern* — seal,
then wrap with a fresh random keypair per recipient (NIP-59's design), hashcash stamps (NIP-13's
design) — but actual Nostr is secp256k1 end to end, and NIP-44 derives its keys from a secp256k1
ECDH and authenticates with HMAC-SHA256. Pigeonpost identities are Ed25519 (`keys.md` is built on
them), so genuine NIP wire compatibility is impossible without changing the identity layer, and
chasing it would buy access to a relay network we already decided not to join. The wire format is
therefore Pigeonpost's own — **envelope v3**, §5.1 — the design borrows what those specs got right,
and published conformance vectors, not NIP numbers, are the compatibility contract. A loft is a
Pigeonpost node serving Pigeonpost agents; it implements what this product needs and nothing else.

## 2. Requirements traceability

Every component exists to satisfy a numbered requirement from `product.md`. Anything that traces to
nothing gets cut.

| Req | Requirement | Satisfied by |
| --- | --- | --- |
| 1 | Works when the recipient is offline | `loft` durable storage; client cursor |
| 2 | Free — no fees, domain, or wallet | Key addresses; no payment path anywhere |
| 3 | Address with no human involved | `core` address derivation from keypair |
| 4 | Handles claimable and permanent | `registry` + transparency log |
| 5 | E2E encrypted, sender and recipient only | `core` envelope v3. **Carve-out:** message *content* stays sender-and-recipient-only and is never decryptable by us. Sender *identity* is additionally recoverable by the compliance key holder under court order (`law.md` §3) |
| 6 | Not controlled by us once adopted | Log dump, witness policy, directory as config, MIT. **Materially weakened** by attribution escrow: we hold a key that recovers sender identity. A fork that strips attribution is legitimate and should be expected (`law.md` §3.3) |
| 7 | No background daemon required | Client is a library call over SQLite; no agent-side service |
| 8 | Our cost is a budget, not a function of adoption | `directory` capacity weighting; `loft` install |
| 9 | Answer lawful orders without decrypting content | `core` attribution block; `loft` sealed trace log; `compliance` custody and disclosure tooling |

## 3. Stack

| Concern | Choice | Why |
| --- | --- | --- |
| Language | Rust (2021, stable, pinned via `rust-toolchain.toml`) | One codebase for client, CLI, and all three servers; matches Docdex; single static binary is what makes `pigeonpost install` a one-liner |
| Async runtime | Tokio | Loft is I/O bound |
| HTTP | Axum | Registry, directory, loft REST surface |
| Transport | HTTP REST over authenticated HTTPS | The implemented loft, registry, and directory surfaces are request/response APIs. WebSocket is deferred until a measured need exists; it is not part of the compatibility contract |
| Crypto | `ed25519-dalek`, `curve25519-dalek`, `chacha20poly1305`, `hkdf`, `sha2` | Envelope v3: Ed25519 signatures, X25519 ECDH, HKDF-SHA256, and XChaCha20-Poly1305 AEAD in a gift-wrap pattern. Real NIP-44 is incompatible with Ed25519 identities and buys nothing on our own network |
| Client state | SQLite (`rusqlite`, bundled) | Embedded, no service, survives restarts |
| Loft storage | SQLite initially; pluggable `LoftStore` trait | A $5 VPS loft never outgrows SQLite. The trait exists so a large operator can swap in Postgres without forking |
| Transparency log | Own Merkle implementation over append-only SQLite entries plus a persisted incremental frontier, C2SP `tlog-checkpoint` format | Interoperable with existing witness tooling without requiring a second storage service |
| Serialization | JSON for ordinary wire/storage; v3 variable ciphertexts are bounded lowercase hex strings; strict versioned binary codecs for cryptographic claims and sealed trace records | Compact strings avoid nested decimal-array expansion while keeping traffic inspectable. Readers retain the deployed v2 byte-array shape. Signed or retained evidence needs one canonical byte representation and fixed bounds |
| Config | TOML | Matches `node.md` |
| Distribution | One provenance-bearing npm launcher (`@bekirdag/pigeonpost`) that downloads a checksum-pinned release asset on first run | Works with `--ignore-scripts`, avoids package-name bootstrapping under OIDC trusted publishing, and keeps one public package contract. Every cached execution is re-hashed from a protected single-link path into a fresh private execution copy. Every Rust workspace package is internal and declares `publish = false`; there is no crates.io release surface |

For launcher caches, POSIX implementations enforce current-UID directory ownership, directory mode
`0700` or stricter, non-group/world-writable single-link files, and a mode-`0500` execution copy.
Node does not expose equivalent POSIX UID, mode, or owner-DACL enforcement on Windows. The Windows
launcher therefore defaults to
`%LOCALAPPDATA%\Pigeonpost\cache` and relies on the current user's profile DACL as its access-control
boundary; an operator-selected `PIGEONPOST_CACHE` must have an equivalently restricted DACL. All
platforms still reject links and unsafe file shapes and verify the bounded bytes before execution.
A concurrent Windows `EEXIST`/`EPERM` publication winner is accepted only after full verification.
Recognized `run/exec-*` remnants become eligible after seven days, and each invocation scans at most
128 entries and removes at most 16 exact executable-plus-empty-parent shapes; cleanup is never
recursive and retains every unknown shape or entry.

**Rejected:** Postgres as a default (a dependency on every donated node kills the volunteer model);
protobuf/CBOR for ordinary traffic (premature, hurts debuggability at this volume); a separate
daemon process on the agent side (violates requirement 7). The fixed trace/claim codecs above are a
narrow evidence-format exception, not a second general wire protocol.

## 4. Repository layout

```
pigeonpost/
├── Cargo.toml                  workspace
├── rust-toolchain.toml
├── crates/
│   ├── pigeonpost-compliance-format/ canonical public evidence/key identifiers; no crypto keys
│   ├── pigeonpost-compliance-seal/   online trace sealing; no compliance private-key operations
│   ├── pigeonpost-compliance/  offline custody, unsealing, disclosure, retention
│   ├── pigeonpost-unix-custody/ internal descriptor-relative Unix private-state primitives
│   ├── pigeonpost-windows-custody/ internal retained-handle Windows private-state primitives
│   ├── pigeonpost-core/        addressing, keys, envelope, PoW, wire types
│   ├── pigeonpost-client/      agent state, outbox, cursor, selection, scoring
│   ├── pigeonpost-loft/        durable inbox server
│   ├── pigeonpost-registry/    handle registry + transparency log
│   ├── pigeonpost-directory/   directory service + prober
│   ├── pigeonpost-mcp/         MCP server over the client
│   └── pigeonpost-cli/         the `pigeonpost` binary
├── deploy/                     Dockerfiles + compose templates (no secrets, no hosts)
├── npm/                        distribution wrapper
└── docs/
```

One online/product binary, subcommands: client verbs (`id`, `send`, `inbox`, `read`, `spam`, `token`),
the node bootstrap verb `install`, authenticated lifecycle verbs (`loft submit`, `loft drain`), and
explicit server verbs (`loft serve`, `registry serve`, `directory serve`) that service units invoke.
There is no underspecified aggregate `status` command: process liveness, protocol readiness, local
storage health, and directory lifecycle are distinct states and must not be collapsed into one
ambiguous success result.

**`pigeonpost-compliance` ships a separate `ppcompliance` binary, and that separation is
load-bearing.** Cargo features are additive and unify across a build graph, so they cannot enforce
this boundary. Online services may depend only on `pigeonpost-compliance-format` and
`pigeonpost-compliance-seal`; only the offline package contains private-key interfaces or unsealing
code. CI inspects the final server dependency closure/SBOM and fails if the offline package or an
unseal symbol is reachable. Production nodes hold compliance public keys plus, while a trace epoch
is open, that epoch's symmetric sealing key for crash recovery. They never hold a compliance private
key and cannot unwrap a closed epoch.

The separately distributed offline operator is Linux/macOS-only. The six
online/product targets still include Windows, but Windows `ppcompliance` output is release-blocked:
the offline custody layer does not yet prove owner-only DACLs, hard-link count/identity, safe
parents, and atomic replacement with the same guarantees as the audited Unix descriptor path.
Shipping an offline custody binary without those properties would make the package boundary a
false assurance. CI rejects any staged Windows custody asset until that implementation is audited.
The Windows online binary supports client state, local CLI state, the Directory database, and a
private loopback Loft database through the retained-handle custody layer. Production regulated Loft
trace capture, Registry serving, and executable compliance-key ceremonies are Linux/macOS-only and
fail on all other targets before creating directories, locks, keys, databases, journals, or
sidecars. Service installation is also Linux/macOS-only.

The two platform-custody crates are internal implementation boundaries, never public packages. On
Unix, bounded normalized paths are walked from retained root/ancestor descriptors without following
intermediate or final links; ownership, mutability, type, link count, and stable device/inode
identity are checked before descriptor-relative create/open/replace/remove and durable parent sync.
Private descendants are created at mode `0700`, private files at their exact restrictive creation
mode, and only audited root-owned sticky system ancestors and macOS aliases are exceptions to the
current-user rule. On Windows, components are opened relative to retained no-delete-share handles;
remote volumes, reparse points, hard links, unexpected owners, null or inherited-permissive DACLs,
and identity changes are rejected. The identity comparison uses the volume plus full 128-bit file
id, and atomic publication uses no-clobber or replacement operations with write-through semantics.
Existing unsafe state fails closed. The installer may tighten an already proved, current-user-owned,
non-writable service directory from `0755` to `0700`; it never repairs group/world-writable or
otherwise untrusted state.

SQLite is the one bounded pathname re-entry: custody first validates the complete ancestor chain,
main name, any pre-existing WAL/SHM and rollback journal; after SQLite enables WAL it proves the
connection's reported path, retains main/WAL/SHM identities, revalidates the journal name, and keeps
those handles/descriptors until after the `rusqlite::Connection` drops. Startup and readiness repeat
the named-object checks, so replacement becomes a fail-closed availability event. This protects the
cross-principal filesystem boundary; same-EUID code and root remain able to cause denial of service,
and eliminating SQLite's own pathname reopen entirely would require a custom VFS or process
isolation rather than a claim made by this release.

## 5. Components

### 5.1 `pigeonpost-core`

No I/O, no async. Pure functions and types, so it is exhaustively testable and reusable by every
other crate.

- **Address derivation** — `addr = "/k/" + base32(SHA-256(pubkey))[:26]`, plus parsing and
  validation for both address tiers
- **Agent record v2** — construct, sign, verify; `successor_hash` and `seq` per `keys.md`, plus the
  recipient's optional exact attribution requirement. Authenticated v1 records remain read-only and
  may be migrated only by the recipient signing a higher-sequence v2 record; an attribution scope is
  never grafted onto legacy signed bytes
- **Rotation record** — construct, verify `SHA-256(to_pubkey) == pinned successor_hash`, enforce
  monotonic `seq`
- **Envelope v3** — seal with X25519 ECDH + HKDF-SHA256 + XChaCha20-Poly1305, then wrap with a fresh
  per-recipient keypair, timestamp jitter up to two days, and the inverse. v3 is the only write
  format. Unattributed v2 remains read-only for the installed `0.1.x` base; attributed v2 is never
  compliance-valid because that prototype cannot be independently authenticated by a custodian.
  v1 was never deployed and remains unsupported. Version dispatch preserves the exact v2 signature
  and id rules rather than silently reinterpreting old bytes. V3 seal and outer ciphertext fields
  serialize as bounded lowercase hex strings; the reader also accepts v2 ciphertext byte arrays.
  The default loft admits every legal 64 KiB plaintext, including the worst JSON-escaping case,
  under a 2 MiB event ceiling and a separately bounded whole-request ceiling
- **Stable v3 event id** — `SHA-256(domain ‖ version ‖ ephemeral_pubkey ‖ recipient ‖ nonce ‖
  len(ciphertext) ‖ ciphertext ‖ created_at)`. It excludes the outer signature, PoW nonce, and
  attribution block. The ephemeral wrap signature covers every id field plus the entire block, and
  the loft verifies that signature before storage. This makes the id available before attribution
  is constructed, prevents a circular dependency, and preserves the trace join key if a block is
  stripped or corrupted
- **Typed compliance-key id** — a fixed canonical record containing format version, purpose
  (`attribution | network_trace | identity_trace`), jurisdiction (`US | EU | TR | test`),
  32-byte authority id, epoch start in milliseconds, and generation. A bare integer epoch cannot
  prevent a network key being mistaken for an identity key or distinguish authorities and rotations
- **Attribution requirement v1** — the recipient-selected fixed 34-byte
  `{ version, jurisdiction, authority[32] }` scope. The authority is a stable custodian identifier
  across monthly epochs and generations, not a public key supplied by the sender; an all-zero
  authority, unknown version, unknown jurisdiction, wrong length, or trailing bytes fail closed.
  The same exact value is authenticated in AgentRecord v2 for pre-send discovery and
  RecipientPolicy v3 for Loft admission
- **Attribution block v3 inside envelope v3** — `{ version, key_id, key_digest, e_pk, nonce, ct }`.
  `key_digest = SHA-256(P_c)` and `ct` is always 120 bytes: a fixed 104-byte claim plus the AEAD tag.
  The claim is `{ sender_pubkey[32], sent_at_ms:u64, sender_signature[64] }`. The sender signature
  binds the block version, canonical key id, key digest, `e_pk`, v3 event id, recipient, and
  timestamp. AEAD associated data binds the same public context. The custodian can therefore derive
  the AAD from the wrap, decrypt with the epoch secret, and independently verify the claimed sender;
  neither operation needs recipient-only plaintext
- **Attribution time and privacy jitter** — the monthly key is selected and validated against the
  sender-signed true `sent_at_ms`. The public `created_at` is jittered backward by as much as two
  days and may legitimately fall in the preceding month; it only bounds the claim to
  `created_at <= sent_at <= created_at + jitter + clock_skew` and never selects the key epoch
- **Construction order and anti-substitution** — the sender creates the rumor/seal ciphertext and
  attribution ephemeral key; the sender-signed seal covers that ephemeral secret; the outer
  ciphertext and v3 event id are then fixed; only then is the sender-signed claim encrypted. The
  ephemeral wrap signature finally covers the block. Copying a block to another event or recipient,
  swapping its key id/digest, forging a victim sender, or stripping the block all fail verification
- **`verify_attribution`** — the recipient recomputes the shared secret from the sender-signed
  `attribution_sk`, uses the compliance key independently verified from the transparency log, and
  verifies the fixed claim, its sender signature, the exact event/recipient/key binding, and the
  timestamp window. Returns `Valid | Invalid | Absent`; attributed v2 and unresolved, expired, or
  wrong-purpose keys are `Invalid`, never `Valid`
- **Attribution trust boundary** — core is deliberately I/O-free, so its low-level open/verify
  functions judge cryptographic validity against the caller-supplied key candidate; constructing
  that value is not proof of Registry provenance. Product acceptance occurs in the high-level
  Agent path, which supplies a key only after witnessed Registry history, typed purpose, epoch, and
  validity verification. Integrations that bypass that resolution boundary may report only
  key-relative cryptographic validity, not Registry-authenticated attribution. On the required
  product path, a matching key id absent from the client's current witnessed prefix is unresolved
  trust state, not evidence of invalidity: the client leaves that Loft cursor unchanged and retries
  after a Registry refresh. An explicitly witnessed `Revoked` state remains invalid
- **PoW** — hashcash stamp (NIP-13-style leading-zero bits) mined and verified over the wrapped
  event id
- **Capability tokens** — mint (HMAC over a label with the recipient's secret), hash for publication,
  verify
- **`UntrustedBody`** — a newtype whose `Display`/`Debug`/serialization always carry the untrusted
  marker. It is a compile-time obstacle to treating a Pigeonpost message as instruction, and the reason
  `integration.md` promises it never becomes a bare string

### 5.2 `pigeonpost-client`

The library every integration uses. Owns one SQLite file per agent.

Responsibilities: identity and successor key management; loft selection (`network.md` §Client
selection); outbox with retry; per-loft cursors; dedupe across lofts; allowlist and sender scoring;
token management; resolution cache with pinned `successor_hash`; the grace-window drain on loft
replacement and key rotation.

Attribution is recipient-owned. Enabling it first proves that witnessed Registry history contains a
fresh `Active` key in the exact selected jurisdiction/authority scope, then signs RecipientPolicy v3
to every active or unexpired-draining Loft, and only after those updates succeed publishes the same
scope in AgentRecord v2. Senders resolve that signed record but do not thereby consent: they must
explicitly agree to the exact scope, either for one send or through a persistent default. A
call-local agreement never mutates the persistent default, so concurrent sends to recipients with
different custodians cannot race through shared configuration. Missing consent, a scope mismatch,
or the absence of a fresh witnessed matching `Active` key fails before wrapping.

Recipient scope mutation holds the same cross-process active-identity lease as a complete drain,
policy signature, and AgentRecord publication. A concurrent CLI/MCP process therefore either runs
wholly before or after that receive wake, or receives the existing fail-fast busy result before any
local setting changes; an old-scope page cannot be stored after a completed scope change.

Policy transitions are fail-closed. During a partial enable or scope change, the old AgentRecord is
left published until every target policy accepts the new bytes; anything admitted through a stale
replica is rechecked against the recipient's current exact scope after unwrap and dropped if it does
not match. Changing or disabling the scope therefore applies to unread fetched wraps immediately.
Operators must drain before changing scope if they intend to accept old-scope delayed traffic; the
client does not retain an implicit list of superseded authorities.

Receive refreshes witnessed compliance history once before fetching. If a required block matches
the recipient's exact signed scope but names a key absent from the currently witnessed prefix, that
prefix cannot prove that the key was not appended later. Registry unavailability therefore stops
only the affected Loft route before cursor advance; other in-flight/configured Lofts continue in
the same bounded wake. After a consistent witnessed refresh the same ciphertext is retried exactly
once. A witnessed `Retired` key remains usable to verify a wrap whose signed send time was inside
its epoch, while a witnessed `Revoked` key is dropped as invalid. Under an optional policy, an
unresolved attribution block is reported `Invalid` without pinning ordinary unattributed traffic
behind it.

Removing a loft drops it from the signed routing record immediately but retains its authenticated
read and policy-update path until the absolute drain deadline, because stale sender caches may still
deposit there. PoW, token, and attribution policy changes target active plus unexpired draining
lofts; the same state query atomically forgets expired routes before transport, so a later setting
cannot revive them. The deadline is the smaller of the loft's authenticated advertised retention and
30 days. Retired-key custody remains a separately signed 90-day window, but its historical route set
is intersected with the current active/unexpired-draining routes so it cannot resurrect an expired
loft.

Every loft transport target, including a recipient-signed agent-record URL and explicit `?l=` hint,
passes one origin validator before any request: bounded URL length, HTTPS for non-loopback hosts,
exact loopback-only HTTP, no credentials/query/fragment/service path, no redirects, no environment
proxy, and bounded connect/request/response budgets. A recipient signature authenticates who chose
a URL; it does not make an arbitrary URL safe for the sender to request.

**Key custody.** On Linux/macOS, clients use the internal descriptor-relative Unix custody layer;
Windows clients use retained no-delete-share handles plus protected current-user DACL and full
file-identity checks.
The state database, identity seed, successor seed, runtime configuration, trust material, and
placement journals reject unsafe ancestors, links, aliases, ownership/access-control drift, and
named-object replacement before use. The successor is staged at an independently configurable path
and same-disk placement is detected and warned about. OS keychain and delegated signers remain
backend extensions, not release claims.

**Durable placement.** Before the client sends any agent-record or rotation publication request, it
transactionally records the exact signed bytes and the deterministic per-URL target set. Success is
acknowledged per target and only against those exact bytes, so a late response cannot complete a
newer plan. If the routing fields have not changed, a directory-membership change reuses the same
signed record and sequence, preserves completion for unchanged URLs, and adds only the new
rendezvous targets. Rotation publication uses the same journal for the exact source record, target
record, and dual-signed rotation bundle; partial success survives restart, including a restart
between source publication, key promotion, and target publication.

Ordinary `send`/`flush` and `drain` wakes give message delivery the bounded wake budget first, then
use only the remainder for bounded placement repair; MCP wakes inherit those library semantics. A
directory outage never erases a verified cached snapshot or blocks an already-configured loft drain.
It sets durable degraded health instead. Integrators can call `maintain_placement` for an explicit
bounded repair wake and `placement_status` for a synchronous, network-free view of pending current
record targets, rendezvous targets, rotation targets, and directory-refresh health. Repeated wakes
do not republish targets already completed for the exact active bytes.

**Local storage lifecycle.** Client schema 13 introduced exact transactional usage counters and
finite operator-configurable inbox/outbox budgets. Successful and terminal outbox copies erase their
wrap and token immediately and retain bounded delivery metadata only; successful metadata is pruned
in bounded wake batches, while terminal debt or an undelivered copy requires an explicit operator
action. Client schema 14 makes received-message deletion logical and permanent: plaintext, sender,
time, attribution, and spam-mark state are erased, while the id remains as an indefinite replay
tombstone under a separate one-million-entry hard ceiling. Tombstones never consume the active
inbox-message quota and are never automatically evicted. The same schema persists authenticated
loft retention, installs the handle-projection public-key index used by recipient reputation, and
validates every legacy projection row before enabling that hot path. Storage status, limit changes,
exact deletion, bounded finished-metadata pruning, and trusted-directory removal are available
through the library, CLI, and closed MCP schemas.
Client schema 15 adds the exact canonical attribution requirement to cached resolutions and stores
separate exact sender and recipient settings. A legacy `attribution_required=true` bit or
jurisdiction-only sender setting has no authenticated stable authority and is rejected until the
operator explicitly selects a complete scope; it is never assigned one heuristically. The fixed
34-byte value is validated on write and whenever a cached resolution is read. Current-schema open
retains constant work in cached-row volume; migration validates every legacy row it transforms and
rolls back the schema step on any failure.

### 5.3 `pigeonpost-loft`

No per-client sessions or fetch cursors are stored — those are the client's job. A loft does retain
bounded message rows and the authenticated control state needed to enforce recipient policy,
capability tokens, agent-record rendezvous, key rotation, and sealed trace recovery; §6.2 names the
authoritative tables and artifacts.

- **Ingest**: size cap *before* allocation, well-formedness, PoW at the recipient's registered
  floor, capability token if the recipient requires one, per-connection rate limit, then store.
  The floor is necessarily **flat per recipient** — the wrap hides sender identity from the loft, so
  the per-sender trust and scoring treatment in `spam.md` is applied client-side after unwrap.
  Capability tokens are an additional loft-bound gate; neither a token nor allowlisting bypasses
  the recipient's flat PoW floor
- **Fetch**: **key-authenticated and stateless** — the request carries a signature by the recipient
  key over `(loft_pubkey, canonical_loft_origin, unix_minute, cursor)`, valid in a ±5-minute
  window; no challenge round-trip, no server state. The server verifies its configured canonical
  origin and `/v1/info` must name the exact requested origin. Binding both origin and key prevents
  a hostile endpoint from claiming another loft's key and relaying the resulting credential.
  Without fetch authentication, anyone could bulk-download every ciphertext addressed to an agent
  — precisely the harvest-now-decrypt-later trap `product.md` rules out. Paginated
- **Retention**: expire on operator policy; an incremental background sweep (bounded batches, so the
  delete never holds the writer lock long), not a per-request check
- **Policy registration**: signed by the recipient key and carrying a **monotonic `seq`** the loft
  enforces — otherwise a captured old update could be replayed to re-enable a revoked token or
  lower the PoW floor. A versioned signed policy BLOB is authoritative; compare-and-swap and write
  happen in one transaction. Partial setters read/merge/re-sign so enabling one gate cannot silently
  disable another. Sets the floor and live token hashes, with a hard cap on token count.
  RecipientPolicy v3 signs the exact optional attribution requirement. Authenticated v1/v2 policies
  remain readable as historical state, but a legacy required bit without an owner-selected
  jurisdiction and authority cannot authorize new configuration and must be re-signed at a higher
  sequence
- **Token presentation is loft-bound**: the sender presents
  `H(token ‖ loft_pubkey ‖ canonical_loft_origin)` and the recipient registers that bound hash with
  each loft. The origin is length-framed and canonical. A hostile endpoint cannot claim another
  loft's key, collect a presentation, and replay it at the honest origin
- **Attribution gate**: when RecipientPolicy v3 carries a requirement, publish rejects a non-v3
  wrap, an absent block, or a block whose key id does not match the exact recipient-signed
  jurisdiction and authority. Every newly admitted attributed wrap, even for a recipient that
  permits omission, must resolve through fresh witnessed Registry state to the exact key in
  `Active` status and must pass purpose, digest, epoch, validity, shape, and public wrap-signature
  checks. `Retired` keys are historical receive/disclosure material only; `Revoked` keys are never
  accepted. The Loft cannot decrypt the claim, so sender correctness is rechecked client-side after
  unwrap against the recipient's current exact scope. A wrong scope, malformed proof, digest or
  epoch mismatch, or a witnessed non-`Active` status is HTTP 400 and terminal for that immutable
  wrap. An unconfigured, stale, or unavailable resolver, a failed cache lookup, or a key absent from
  the current witnessed prefix is HTTP 503 and retryable: an older append-only prefix cannot prove
  that no later matching key exists
- **Delayed attributed outbox**: a wrap is immutable once signed. If its key becomes `Retired`
  before first Loft admission, the Loft returns a terminal client error; the outbox erases the
  ciphertext, retains only bounded dead-letter metadata, and never rewrites or silently re-escrows
  it. The sender must perform a new explicit send under the current matching `Active` key
- **Trace capture** (`law.md` §2.1): one sealed `NetworkTraceRecord` per in-scope inbound request,
  carrying
  `ts_ms`, `src_ip`, `src_port`, op, and the relevant key or event id. Millisecond timestamps and the
  source port are both required — behind CGNAT an address alone resolves to nobody. Records go
  through a **bounded synchronous handoff** to the segment writer under a short caller-enforced
  timeout; if the handoff cannot acknowledge capture, fail the request. The implementation may use
  a bounded channel internally, but that is not the interface contract. Dropping records silently
  defeats the purpose, and if we cannot record a request we should not serve it. The production
  sink has one supervised writer, a 64-record queue, batches at most 32 records for at most 2 ms,
  and exposes eight fixed blocking caller lanes. It acknowledges each caller only after the batch
  containing that exact frame is durably synced. Saturation and caller timeouts fail the request;
  writer failure, worker exit, or panic poisons readiness. Coordinated shutdown closes admission,
  drains accepted records, syncs and finalizes the active segment, and joins the worker
- **Trace capacity contract**: a public trace-enabled listener accepts only a sink whose validated
  contract covers the configured global request ceiling, canonical UTC-key epochs, segment size,
  logical byte cap, and complete jurisdiction/capture/retention policy. One shared
  cryptography-free policy validator computes the required epochs, and startup independently
  recomputes both that runway and the conservative storage requirement from the contract. Public
  serving accepts only the built-in audited `SealedTraceSink` and `SqliteStore` concrete adapters;
  the SQLite instance must be an owner-custodied file-backed database, never the in-memory test
  constructor. An ad-hoc implementation cannot self-assert durable capture or storage. US standing
  capture provisions at least 31 UTC epochs for the exact 30-day policy, Türkiye provisions the
  counsel-selected 365–730 days plus the current epoch, EU preservation provisions exactly every UTC
  epoch intersected by the authenticated interval, and test mode provisions at least one. Capacity
  is runway, not automatic retention enforcement or a filesystem quota. Before any live-key,
  segment, or manifest mutation, the sink preflights the exact next transition—terminal artifacts,
  release of the old live key, new live key/header, record, and fresh reserve—so exhaustion cannot
  strand or destroy the acknowledged epoch
- **Restart-safe trace admission**: the exact global ceiling is charged against one durable,
  UTC-minute-aligned SQLite counter before the trace sink mutates. The charge survives process
  restart and multiple processes, is never refunded after admission, and a backward clock fails
  closed. The SQLite value is a reserved-slot high-water mark: one `synchronous=FULL` transaction
  reserves at most 64 slots, every process-local dispense revalidates the durable singleton, and
  unused reserve is deliberately burned by restart, rollover, or a limit change. Conservative early
  rate limiting is therefore valid; exceeding the configured ceiling is not. A trace-enabled loft
  refuses startup with a store that cannot provide this durable contract
- **Jurisdiction is configured, not guessed from an address.** TR nodes use the counsel-selected
  365–730-day period committed into the matching offline inventory; US nodes use the explicit
  30-day product period; EU nodes default to no standing capture and write only while an
  authenticated preservation policy is active. Every segment and key id carries that jurisdiction
- **Reverse proxies**: forwarded source metadata is honoured only when the connected peer is on an
  explicit `trusted_proxies` allowlist. The trusted edge must supply a syntactically valid source
  address **and port** using the configured `Forwarded`/edge fields and must strip client-supplied
  copies. Empty list means direct `ConnectInfo`. If required source-port evidence is unavailable,
  the request fails closed instead of writing a misleading trace
- **Refuses to start** when active trace policy has no matching, currently valid network-trace public
  key, when proxy mode cannot supply a source port, or when a production public listener is using an
  unsafe trace/proxy configuration. Writing evidence nobody can read or trust is worse than failing
  startup
- **`/v1/info`**: reports software and protocol versions, the exact canonical origin and loft public
  key, exact capacity/used/event counters, derived utilization, current accepting status, retention,
  and the complete directory policy (`open`, `pow_floor`, `max_event_bytes`). Before first
  publication, `loft submit` requires the reported key to equal the local owner key and capacity to
  be an exact nonzero GiB value, then signs the reported capacity, retention, and policy. Every
  Directory probe requires the compatible protocol and exact submitted origin/key, compares live
  capacity, retention, and policy with that signed claim, and recomputes utilization from the
  reported counters; any mismatch fails the probe before the endpoint can remain eligible
- **Agent record cache**: serves signed agent records, which is safe because they are self-verifying
- **Bounded admission**: body, identifier, page, connection, source, recipient, and global budgets
  are enforced before expensive work. The source-enforced HTTP boundary accepts at most 256 live
  connections and 128 in-flight requests by default; excess accepts are closed without a user-space
  queue. Hyper has a timer-backed five-second HTTP/1 incomplete-header deadline, bounded HTTP/2
  streams/header lists, a 15-second handler deadline, a 15-second response-body deadline, and a
  35-second absolute connection lifetime. The request permit moves into the response body and is
  released only at body EOF, error, timeout, or drop, so constructing a response does not release
  admission while a slow socket still owns it. Fetch database access, page assembly, and JSON/hex
  serialization share the bounded blocking lane rather than a Tokio scheduler worker. The result is
  checked against the 8 MiB ceiling, and its exact body bytes are charged to global,
  effective-source, and recipient egress buckets before send; failed later charges are
  conservatively not refunded. Capacity check
  plus insert is one transaction and accounts for database/WAL overhead; duplicate ids are
  idempotent. A hostile recipient cannot consume the shared source/global budget through a
  victim-keyed limiter. The v0.2 client rejects a fetch response over 8 MiB and reserves that full
  amount before consuming its body from a process-wide 64 MiB drain budget. The reservation remains
  held through JSON decoding, event processing, and durable cursor advance; cancellation drops the
  in-scope future and permit, with no detached fetch work. The public configuration API has explicit
  upper bounds (1 PiB capacity, 3,650 retention days, 2 MiB events, 2 MiB + 64 KiB requests, 500/8
  MiB fetch pages, 4,096 connections, 4,096 requests, 256 blocking operations, 65,536 limiter keys,
  300-second header/request/response deadlines, a 30-second trace deadline, 10,000-row sweeps,
  one-day sweep intervals, and 64 trusted proxies). Rate and byte budgets are nonzero and bounded.
  Full keyed limiter maps reject a new key in constant time before their tracked cleanup deadline;
  cleanup begins no earlier than the first possible expiry and runs at most one full scan per rate
  window, so rotating recipient/source keys cannot force an O(map-size) retain on every request.
  The fallible constructor validates these relationships before allocating semaphores or keyed state
- SQLite runs in WAL mode with a `busy_timeout`, explicit schema migrations, and one supervised DB
  actor (or an equivalent bounded async pool). Async handlers never hold a synchronous SQLite mutex;
  background sweeps are supervised and surface failure. Persistent loft stores use
  `synchronous=FULL`; a successful admission is returned only after the committing transaction, so
  an acknowledged event is protected by SQLite's power-loss durability contract. Linux/macOS
  stores, and private-loopback stores on Windows, retain and revalidate the database, WAL, SHM, and
  parent custody described in §4. Public or compliance-enabled service additionally requires the
  Linux/macOS-only sealed-trace writer and fails on all other targets before storage mutation

**Key-address resolution (rendezvous).** A sender holding only `/k/…` needs the agent record, but
the record is what lists the lofts — a bootstrap loop the design docs left open. Resolution: the
client publishes its record to its own lofts **and to 3 rendezvous lofts** chosen deterministically
from the directory. Rank active lofts by `SHA-256(loft_pubkey ‖ addr)`, then greedily take the first
three whose signed endpoint-host and declared-operator failure domains do not overlap. Any sender
computes the same set and asks all three. Records are self-verifying and carry `seq`; **senders take
the highest unambiguous valid `seq` across three independently served responses**. For each
current v2 record, that signature also authenticates the optional exact attribution requirement;
fetching it exposes recipient policy but never supplies sender consent. For each
unavailable or missing primary response, walk only far enough down the same ranking to restore three
valid responses, bounded to twelve diverse candidates. This makes rollback require control of every
responding endpoint-host entry, rather than one entry. It does **not** prove independent human
operators: operator labels are self-asserted, key/hostname Sybil resistance is a directory-admission
and deployment assumption, and production must document that assumption rather than claim it from
the hash alone. When directory membership shifts, the client durably republishes on the next wake.
The lookup request pattern is itself part of the contract: contact exactly the first three diverse
candidates initially; only a missing or unavailable valid response opens the next ranked candidate,
and stop after three valid responses or twelve candidates. A valid lower sequence remains evidence
for same-sequence equivocation checking even when a higher sequence is also present. An explicit
`?l=` hint performs no directory load or rendezvous request.
An address published with no directory in play (fully self-hosted circles) carries an explicit hint
— `/k/…?l=https://loft.example.com` — which bypasses directory and rendezvous traffic entirely.

### 5.4 `pigeonpost-registry`

Handles are the only naming tier here: key-address resolution never touches this component. The same
append-only transparency log also carries compliance-key history and independently authorized
directory mutations; those entries do not turn the registry into a key-address resolver.

Handles are canonically provider-scoped (`/github/name`, `/google/subject`). Bare aliases are not part
of the compatibility contract: choosing which provider owns a collision would make the registry an
identity adjudicator rather than a verifier of an upstream allocation.

- `POST /v1/register` — verify the identity proof **and a signature over the registration payload by
  the pubkey being bound**. Without proof of key possession, anyone could bind a handle to someone
  *else's* pubkey — an impersonation/confusion primitive. Then append and return an inclusion proof
- `POST /v1/rotate` — rebind after fresh OIDC proof; appended, never mutated
- Verified claims share one exact global binding ceiling derived as the smaller of the audited HTTP
  request ceiling and the canonical trace-format maximum of 454,795 bindings per minute. That
  maximum is `floor(65,536 manifest entries × 10,000 records/segment ÷ 1,441 possible UTC-day
  windows)`; a shorter configured segment lowers the realizable ceiling by the same formula and
  fails startup rather than weakening trace coverage. After provider verification and before
  trace submission or log append, the registry
  charges a durable UTC-minute singleton in SQLite; the charge survives restart and concurrent
  processes, is not refunded after a later failure, and rejects backward time. Provider mode also
  requires a sealed claim-trace sink whose advertised record rate and canonical UTC-epoch runway
  cover that exact ceiling. The complete contract includes its segment limit and independent
  network/identity logical caps; both the sink constructor and the Registry serving boundary
  independently recompute the jurisdictional epoch runway and required purpose-specific bytes.
  Witnessed public or provider-enabled loopback serving accepts only the built-in audited
  `SealedClaimTraceSink` concrete adapter and rejects a missing, malformed, self-asserted,
  understated, or underprovisioned contract. The Registry itself must likewise hold a stable,
  owner-custodied persistent database; in-memory/temporary/URI storage is never a witnessed public
  backend. Test-only loopback fixtures may inject a custom sink
  but are not a public serving path. As at the loft, one
  FULL-synchronous commit reserves at most 64 durable slots; every local dispense read-validates
  the singleton, unused reserve burns, and conservative early limiting never becomes a refund
- `pigeonpost handle rotate` and MCP `pigeonpost_rotate_handle` expose that operation to operators
  and agents through the same challenge-bound flow as the initial claim. They work from a fresh
  agent home after total key loss because the new key and fresh provider proof authorize the rebind;
  they restore future handle routing only, never the old address, state, or ciphertext
- `GET /v1/resolve/{handle}` — cacheable binding convenience projection + inclusion proof. The
  proof authenticates that historical leaf, not that it is the latest binding at the witnessed head
- `GET /v1/log/checkpoint` — signed tree head with witness cosignatures
- `GET /v1/log/consistency?from&to` — bounded RFC 6962 consistency proof between two published
  heads; malformed, reversed, or unpublished ranges are refused
- `GET /v1/log/status` — no-store operational readiness, committed/published sizes, publication
  lag, and quorum timestamp; it never exposes pending leaf contents
- `GET /v1/log/entries?from&to` — bounded JSON delivery fallback over the half-open range
  `[from,to)`
- `GET /v1/log/dump` — without query parameters, the complete canonical NDJSON exit/mirror stream,
  continuous from entry zero through the head captured when the request begins. It uses one
  separately bounded full-dump lane, has no absolute wall-clock cutoff, and disconnects a reader
  that makes no body progress for 10 seconds; it cannot occupy a product range-stream permit
- `GET /v1/log/dump?from=<u64>&to=<u64>` — an exact half-open canonical NDJSON range. Published leaf
  bytes are immutable, so a valid range response has an exact range-bound ETag and immutable public
  cache policy. `to - from` may not exceed 8,192 leaves; one response is capped at 32 MiB and its
  client attempt at 30 seconds. The server applies a 10-second idle-progress deadline and a
  120-second absolute deadline. A request beyond the published head is refused rather than shortened
- Product handle resolution treats `/resolve` as a candidate, then independently derives the latest
  binding from the exact witnessed log prefix. One global handle-audit frontier feeds a normalized
  local projection for every handle, so the first lookup audits from leaf zero once and subsequent
  lookups inspect only newly appended leaves; resolving a second new handle at the same head never
  downloads history again. Claims and rotations form a strict per-handle state machine: exactly one
  initial claim, no rotation before a claim, a stable provider subject, and a changed key at a later
  log index. Omitted, malformed, out-of-sequence, unsupported-version, and unknown-kind entries fail
  closed, as does a `/resolve` row that is older than the audit-derived binding. Registry replay is
  written only to a private ephemeral SQLite staging database. It holds no agent-database writer.
  Fresh replay walks exact immutable dump ranges from leaf zero in segments of at most 8,192 leaves,
  32 MiB transferred bytes, and 30 seconds per attempt; every NDJSON line is independently capped at
  64 KiB. Each response must contain exactly the requested continuous sequence. A
  missing/unsupported range route, timeout, clean early EOF, transfer cap, stream-permit exhaustion,
  or other delivery failure discards partial segment state and safely replays that segment through
  JSON pages of at most 256 leaves. Malformed or unknown entries, sequence/state violations, and
  proof or root failures are terminal and never become delivery fallback.
  A process admits at most four complete Registry audits without queueing. Response JSON decoding,
  NDJSON line decoding, signature/proof hashing, state-machine replay, ephemeral SQLite projection,
  and final root/projection validation all run in that bounded blocking lane rather than on a Tokio
  worker. Dropping or timing out an async caller does not release the lane permit until its already
  started blocking job exits. Compliance refresh passes its at-most-4,096-key prior state by `Arc`
  and performs any required projection clone inside the lane.
  Claim and rotation publication callers wait for the exact receipt index, key, and entry kind under
  a fresh witnessed head. A witnessed binding at a strictly older index is publication lag and may
  be polled within the bounded deadline. Any same-index or newer mismatch is terminal; it must never
  be mistaken for eventual consistency.
  After the exact root and projection match, one short transaction rechecks the starting snapshot
  and commits the normalized delta, audit progress, requested cache row, and sole registry pin.
- `GET /v1/compliance-keys` — a bounded convenience projection plus the fresh witnessed head. The
  projection's inclusion proofs authenticate rows the registry chose to return, but cannot prove it
  omitted no later revocation. Product clients therefore request metadata only and use the same
  bounded segmented replay engine as handle resolution. Both NDJSON segments and the safe JSON-page
  delivery fallback rebuild the exact RFC 6962 root with a compact frontier and derive the latest key
  status from continuous history before accepting the projection. Later refreshes fetch only unseen
  leaves. Metadata, consistency proof, transfer, fallback, and verification share one 120-second
  total deadline, and failure never advances the durable pin or derived cache.
  Publishing
  `P_c^epoch` in the log is what stops us handing one agent a different compliance key from everyone
  else: targeting would require forking the log, which is the attack the log already catches. Each
  `ComplianceKeyPublish` carries the typed key id, public key, validity interval, and status. Entry
  variants and codecs are explicitly versioned; unknown kinds fail closed. Existing leaf bytes are
  immutable, so new variants append without reinterpreting or reordering old leaves.
  `GET /v1/compliance-keys/{key_id}` is the bounded single-key convenience projection; it has the
  same proof-versus-completeness limitation and never replaces continuous witnessed replay
- `POST /v1/directory/add` and `/v1/directory/remove` — accept only a body carrying both the loft's
  existing self-signature and an Ed25519 authorization from an explicitly pinned directory
  publisher. The publisher signature uses a strict versioned domain and length-prefixes the
  configured registry origin, then binds the add/remove tag and exact body bytes. Exact trusted
  proxy resolution and the bounded per-source binding charge occur first, preventing forged
  requests from spending Ed25519 work at the larger global-request ceiling. Publisher allowlist
  lookup, signature verification, and then JSON decoding run in the same fail-fast blocking lane as
  readiness and storage; authorization still precedes decoding. Unknown, missing,
  forged, duplicate-header, cross-operation, and cross-origin authorization fails without log
  growth. This preserves open loft admission at the directory while preventing direct callers from
  impersonating the publisher. The reference publisher produces that authorization only after a
  full local transition preflight has committed an exact durable reservation, so its 4,096-entry
  pending/probe gate cannot be bypassed by registry-first ordering. A witnessed/full registry
  refuses startup without a nonempty bounded publisher allowlist; the loopback full-surface fixture
  requires an explicit test key. Exact retries reuse the immutable signed request and the existing
  idempotent leaf semantics

Registry storage uses a dedicated local filesystem or volume with a hard operator-enforced quota
covering the SQLite database, WAL, and SHM files (10 GiB baseline, alert before 80 percent). The
quota may be expanded without changing log identity. There is deliberately no finite global leaf
cap and no retention/compaction path for committed transparency leaves. The current Registry
persistent-custody and trace-writer contract is Linux/macOS-only: service and executable
compliance-key ceremonies reject all other targets before a runtime directory, lock, checkpoint
key, database, journal, sidecar, or listener is created. Loopback read-only fixtures are test
surfaces, not a Windows production Registry claim.

Clients pin a configured genesis/checkpoint key and require the inclusion root to equal a fresh,
valid signed checkpoint. A root returned beside its own proof is not a trust anchor. Checkpoint-key
rotation, consistency from the last pin, witness threshold, staleness, and equivocation behavior are
part of the client contract. `registry_pin` is the only durable checkpoint authority. Handle and
compliance audit rows are replay-progress caches that may lag that pin but may never lead it or
conflict at the same size. Their signed notes and compact Merkle frontiers are persisted with the
derived normalized projections; each first audit begins at leaf zero and later audits process only
new leaves. A first key publication must be `active`; only active → retired/revoked and retired →
revoked transitions are valid. For attribution, only `active` authorizes construction or new Loft
admission; `retired` remains visible for historical receive/custodian verification, and `revoked`
authorizes neither. Range and dump routes are paginated/streamed under fixed body,
per-entry, deadline, and concurrency budgets.

The registry client uses a dedicated no-proxy, no-redirect transport. A public hostname is resolved
once per client; every answer must be a public Internet address, and the complete validated answer
set is pinned for that client's lifetime so later DNS rebinding cannot redirect an audit. Numeric
special-use addresses fail closed. Exact numeric loopback remains a development-only path and is
accepted only when the caller also supplies independently provisioned registry trust.

Every product trust/configuration boundary requires a nonempty witness roster and a
strict-majority threshold: `2 * threshold > witness_count`. Thus 1-of-1 and 2-of-3 are valid while
1-of-2 and 1-of-3 fail closed. This guarantees set intersection only for the same roster. Preventing
a fork without gossip additionally requires every possible intersection to contain a
non-equivocating witness; with at most `f` equivocators, `f < 2 * threshold - witness_count`.
Consequently 2-of-3 tolerates no equivocator, and N-of-N is required if the only operational
assumption is “at least one of N is honest.” Clients using different rosters need guaranteed
non-equivocating overlap across every accepted quorum pair or gossip/out-of-band checkpoint
comparison.

The v0.2 fresh-bootstrap support contract covers a witnessed prefix of at most 1,000,000 leaves whose
canonical NDJSON is at most 256 MiB, when effective transfer throughput is at least 10 MiB/s and RTT
is at most 100 ms. The 120-second total deadline includes metadata, consistency proof, every segment
or fallback page, state derivation, and final root verification. The unscoped dump remains available
at larger tree sizes as the exit stream, but client bootstrap beyond this envelope is not guaranteed;
it requires a future authenticated snapshot/map/checkpoint design rather than smaller pages or a
larger timeout. Contract tests cover the segment protocol, fallback, final-root authentication,
maximum valid JSON-page framing, and transfer/RTT budget arithmetic; they are not a measured
million-leaf production-hardware benchmark. The timing target assumes the immutable range route is
available; the 256-leaf
JSON fallback preserves safe delivery compatibility but does not carry the 1,000,000-leaf timing
guarantee.

Compliance-key writes have no public HTTP route. They use the local
`pigeonpost registry compliance-key` operator ceremony while the registry service is offline. The
server and operator take the same nonblocking `registry.lock` for their entire lifetime (a regular,
single-link, owner-only file on Unix);
the ceremony refuses to open storage while the server or another ceremony holds it. The
ceremony requires the existing checkpoint signing key, an independently stored matching backup,
an exact repeated canonical key id, a complete witness-publication policy, and explicit execution
confirmation. It rejects test jurisdiction, noncanonical hexadecimal, non-contributory X25519
points, fixed-length “months,” non-daily trace epochs, and any typed-field mismatch. It audits the
entire stored log before appending, then waits until the new immutable index is covered by the
durable witnessed head. If that finalization times out, an exact retry reuses the committed index
rather than appending another leaf (`runtime-configuration.md`).

The registry persists separate **committed** and **published** heads. An append, its Merkle nodes,
projections, and operator checkpoint commit atomically; independently verified C2SP receipts then
commit per witness, and only a durable quorum transaction advances the published head. All public
receipts, handle resolution, entry ranges, dumps, compliance projections, and consistency proofs
are capped at that published head. A pending append returns its immutable index with an older tree
size and no inclusion path, so it is pollable but cannot be mistaken for final inclusion. Startup
revalidates persisted receipts and the published checkpoint against local history and configured
keys. Public or identity-enabled runtime startup requires explicit external witness endpoints and
fails closed when the quorum is missing, stale, or beyond the configured publication-lag bound.

The registry emits two separately sealed records for a handle claim: a network record containing
source address/port and an opaque one-time correlation commitment, and an identity record containing
the provider subject and that commitment but no address. They use distinct purposes, stores, epoch
keys, custody authorisations, retention schedules, and offline commands. That separation is not a
convention — it is the *La Quadrature du Net II* requirement that IP addresses stay
watertight-separated from civil identity data (`law.md` §1.2). No record type or ordinary command
holds or unseals both. One named supervised commit worker owns both online writers behind a fixed
64-claim queue. It batches at most 32 claims for at most 2 ms, and acknowledges a claim only after
the exact network and identity frames have both reached their separate durable stores. Saturation
fails immediately; either-store sync failure, worker exit, or panic poisons registration readiness.
A canceled receipt still in the queue is discarded before write, so an HTTP deadline cannot detach
a blocking trace task that later mutates the registry. Shutdown closes admission, drains accepted
claims, finalizes both streams, and joins the worker before returning.

Identity proof is per-provider, because the providers are not uniform. Google uses an OIDC identity
token: fetch and cache its pinned issuer JWKS, verify signature, `iss`, `aud`, `exp`, `iat`, and
`nonce`, then bind the claimed name to the token's opaque subject. The authorization request uses
Google's minimum valid `openid profile` scope pair, but the registry discards every optional profile
claim and persists only `sub`. **GitHub does not issue OIDC
identity tokens for users** — its OIDC issuer serves Actions workflows only — so GitHub uses an OAuth2
authorization-code flow: the registry exchanges the code server-side and reads the account login
from the API. The `proof` field of `POST /v1/register` is provider-tagged, and each adapter sits
behind one `IdentityProof` trait. Issuers are pinned to an allowlist; no dynamic discovery. The
CLI binds an exact loopback listener before requesting a short-lived challenge, validates fixed
provider metadata, uses GitHub PKCE plus `state` or Google `nonce` plus `state`, relays Google's URI
fragment only within that loopback origin, and submits the one-shot proof. Registration is
rate-limited per account and per IP — the OIDC gate stops squatting, not request floods.
Challenge consumption and the handle append commit atomically with the exact result sequence. A
retry bearing the same challenge token and bound-key signature recovers that stored receipt before
provider I/O, trace capture, account charging, or global binding admission; a different handle,
key, PKCE value, or operation fails closed.
The provider's opaque subject is the stable owner of a handle: rotation may change the bound key but
must preserve that exact subject, so an upstream username reassignment cannot inherit the old
identity's history.

### 5.5 `pigeonpost-directory`

Directory service plus prober, per `network.md` §Directory integrity.

- `POST /v1/directory/submit` — accepts a loft-signed entry; we compile, we do not author
- `GET /directory.json` — signed, CDN-cacheable, mirrorable
- `GET /v1/probe/measurements.json` — signed version-2 raw-measurement pages. The request cursor,
  next cursor, `more` bit, endpoint, and at most 500 measurements are all covered by the signature,
  so a verifier can traverse the complete rolling window without trusting pagination metadata
- `GET /v1/probe` and `GET /v1/probe/{endpoint}` — v0.1 compatibility views over the same signed
  measurement data. New clients use `/v1/probe/measurements.json`; the aliases receive the same
  64 KiB body ceiling, request concurrency/rate admission, and bounded request deadline and add no
  weaker unsigned representation
- Prober: liveness every 5 min and retention honesty daily. A persisted one-recipient canary is read
  back every day through the advertised-retention boundary (checked one hour early so an honest
  loft may delete at its exact boundary), then rotated. A missing aged canary is an unhealthy probe
- Lifecycle: `pending → active → degraded → draining → removed`. Promotion requires 24 continuous
  clean hours beginning with the first successful probe; any failure resets probation. A pending
  entry expires after seven days. Pending and removed entries remain auditable in the registry log
  but never appear in the routing snapshot
- Before ownership is proven, independently signed `(endpoint, loft key)` candidates coexist and
  no candidate reserves the endpoint. The first successful key-matching probe atomically becomes
  canonical, discards its competitors and their evidence, and starts the full clean probation; a
  stale in-flight probe for a losing key is ignored. A removed, ownership-proven loft remains
  permanently key-bound and may re-enrol only under the same key with a strictly higher
  authenticated sequence. Resubmitting while merely degraded never clears observed failure state,
  and an unproven claimant cannot bypass probing by announcing a drain
- Submissions and removals use the durable admission/publication protocol below and append to the
  registry's log only after local pre-admission succeeds

An add or drain is never sent to the registry first. In one `BEGIN IMMEDIATE` transaction the
directory repeats signature, key-binding, state, monotonic-sequence, replay, and capacity checks,
executes the exact prospective local transition in a rolled-back savepoint, and commits an exact
schema-4 reservation before any publisher authorization or HTTP POST. File-backed directory SQLite
uses WAL with `synchronous=FULL` for both this commit and finalization. The reservation binds the
operation, endpoint, loft key, sequence, canonical registry mutation, exact local request,
reservation time, and whether the add consumes a pending-capacity slot; its SHA-256 identifier is
domain-separated and length-prefixed. At most 4,096 reservations exist, and capacity accounting is
`pending lofts + pending candidates + reserved capacity slots`, so reserving and later projecting
one add can never create a transient extra slot.

The production `directory serve` boundary accepts only a file-backed database opened through the
Directory's platform-custody path. In-memory SQLite remains available only to unit tests and the
explicitly loopback-only read fixture. Before SQLite opens, the Directory validates the complete
ancestor chain, main file, any WAL/SHM sidecars, and rollback journal. It then retains the parent,
main, WAL, and SHM identities across the connection lifetime. Before a public listener starts and on
every readiness check, it re-proves those names and identities; replacement or disappearance fails
closed. On Linux/macOS, the internal Unix layer uses descriptor-relative custody; on Windows, the
retained-handle layer uses no-delete-share handles with the current-user
DACL/reparse/link/full-file-id policy from §4.
Creating a new public Directory database also requires a separately provisioned owner-only raw
32-byte `signing_key_file`; production CLI startup never invents this identity. A keyless restart is
allowed only when the existing database already contains a validated pinned signing seed. The
normal library build exposes existing-state open only and has no implicit signing-key generation
API; generation remains confined to explicit test utilities.

An outstanding reservation is not a directory projection: it is absent from routing snapshots and
cannot be promoted, probed, expired, replaced, or changed by retention bookkeeping. The same
endpoint is fenced against a divergent add/drain while the reservation exists. Drain requests first
perform a read-only authenticated key lookup and charge the bound per-loft rate bucket; the
transactional reservation repeats every validation after charging, so a rejected rate limit leaves
no reservation and a race cannot turn the preflight into authority.

Only the exact reserved leaf is then signed and sent. After a fresh witnessed inclusion receipt,
one immediate transaction byte-compares and consumes the reservation, applies the exact local
projection, and advances the accepted registry checkpoint. Cancellation, process failure, an
ambiguous response, or a crash between those commits leaves the reservation recoverable rather
than guessing whether the leaf exists. A bounded supervisor exact-replays reservations in stable
order using the registry's idempotent append, and startup/request readiness remains false until none
remain. Exact retries after restart—or after a drain deadline changed `draining` to `removed`—are
idempotent; a divergent retry cannot reuse or replace the reservation. Every recovery SQLite read,
checkpoint read, and finalization runs in the directory server's bounded blocking lane, while the
prober's SQLite lease/record/retention bookkeeping runs in its own single-operation supervised
blocking lane; no SQLite mutex guard crosses an async wait. A registry-backed router refuses to be
constructed without an active Tokio runtime because that would omit the recovery supervisor.

Schema 3 is verified before migration and schema 4 before use by comparing canonical
`sqlite_schema` table and explicit-index SQL with generated pristine and exact-release reference
schemas. This comparison includes declared types, nullability, defaults, primary/unique keys,
`CHECK` constraints, and index definitions; a same-column but weakened or otherwise unknown shape
is refused without DDL or a `user_version` change.

Mutation signatures bind endpoint, loft key, monotonic sequence, and operation; drain/remove cannot
be performed anonymously or replayed. The registry therefore projects independent monotonic
streams per `(endpoint, loft key)` while retaining every competing claim in the immutable audit log;
the probed directory state, not the projection, decides which stream may route. The prober resolves
and validates every address, forbids
loopback/private/link-local/multicast/unspecified/reserved destinations and redirects, rechecks the
connected peer to resist DNS rebinding, and confirms `/v1/info` presents both the submitted loft key
and the exact canonical origin being probed.
Only HTTPS loft origins are admitted in production; HTTP is loopback-only, and `ws`/`wss` are not
protocol origins. Probe I/O is time/body/concurrency bounded. Each sweep leases at most 512 due
entries in fair order, runs at most 16 probes concurrently, and stops after one 45-second whole-sweep
deadline so slow endpoints cannot starve the pool. Retention-honesty reads use bounded,
progress-checked pagination rather than repeatedly reading the first page.

Published uptime and raw evidence use one exact rolling 30-day window; lifetime counters do not
influence weights. Public directory snapshots contain at most 512 non-pending entries and are also
checked against the client's fixed 2 MiB response ceiling. Pending admission is capped at 4096.
The HTTP boundary defaults to 128 non-queueing live connections and 64 non-queueing requests, a
timer-backed five-second incomplete-header deadline, a 30-second handler deadline, a 15-second
response-body deadline, and a 50-second absolute connection lifetime. A request permit follows its
response through body EOF/error/drop, so a slow reader cannot free admission early. Signed directory
and measurement documents are assembled, signed, serialized, and hashed in the bounded blocking
lane, remain capped at 2 MiB, and debit their exact body bytes from default 256 MiB/minute global
and 64 MiB/minute per-source egress budgets before send;
conditional `304` responses debit zero body bytes. Global/per-source/per-loft-key request buckets,
direct socket peers, and only bounded RFC 7239 `Forwarded` chains from exact configured proxy IPs
complete the boundary. A full keyed bucket map rejects misses in constant time until its tracked
cleanup deadline and performs at most one full cleanup scan per rate window. `X-Forwarded-For`, ambiguous chains, and portless trusted-proxy sources fail
closed. `/health` is liveness-only and skips source parsing, but still consumes the shared request
permit, global request budget, handler/body deadlines, and connection lifetime; `/ready` requires a healthy local database, a supervised-prober
heartbeat no older than three probe intervals, and a fresh witnessed registry checkpoint with no
publication lag.

Directory add/drain requests are authenticated control-plane mutations whose immutable leaves are
published in the registry log. They are source-limited but are not `NetworkTraceRecord v1` events;
adding persisted directory-source evidence would require a new trace operation and explicit legal
scope, not reuse of the existing message/claim schema.

Client diversity always includes the successfully probed endpoint host as a failure domain. The
optional loft-signed `operator` handle is a self-asserted label until a separate handle-key
authorization format exists; it may collapse additional candidates across hosts but never replaces
the host or expands eligibility. The product does not call that label an attestation or use it as a
security proof.

### 5.6 Compliance packages

Canonical formats, online sealing, offline custody, and disclosure (`law.md` §4). Split by package so
production cannot link the decrypt path — see §4.

- **Formats** — fixed, versioned, length-bounded binary codecs for key ids, network traces, identity
  traces, segment headers/footers, terminal epoch manifests, wrapped-key metadata, disclosure
  commitments, and attribution claims. Unknown versions, enum values, lengths, and trailing bytes
  fail closed
- **Epoch keys** — one exact 86,400,000-millisecond UTC-aligned epoch per jurisdiction and purpose
  for trace records, one per month for attribution. One shared validator governs registry
  publication, live resolution/sealing, offline inventory, and registry replay. Records are sealed
  under the epoch key; the epoch key is wrapped to the offline
  compliance public key. An online trace writer retains only its current daily symmetric key; its
  supervised boundary wake finalizes and zeroizes that key at rollover even when the node is idle,
  and it cannot unwrap a closed epoch. Attribution uses only the published compliance public key
  online
- **Custody** — a `CustodyBackend` process-adapter boundary for an externally provisioned KMS,
  HSM, or *k*-of-*n* ceremony. The repository includes a test-only software backend, not a native
  production KMS or Shamir implementation. `S_c` never touches a node; regional residency and
  approval enforcement are deployment/custodian responsibilities that must pass the §7 gates
- **Independent key authentication** — before any approval or unwrap, the offline operator streams
  a complete registry NDJSON dump, recomputes its RFC 6962 root and a pinned historical prefix,
  verifies the final operator checkpoint plus a fresh pinned-witness quorum, and replays every
  compliance-key transition. The selected purpose/jurisdiction/epoch, validity interval, and exact
  custody public key must have been published `Active`; a later `Revoked` state blocks disclosure.
  A locally configured, forged, omitted, or selectively projected key is not authority
- **Producer and epoch authentication** — each trace epoch independently pins the expected producer
  node id and Ed25519 segment signer. The operator rejects an embedded self-declared signer,
  duplicate segment ids, custody-key digests that differ from the audited registry key, and mixed
  epoch-key commitments. A segment footer proves one file, not that an epoch is complete, so every
  closed epoch also has one producer-signed v1 terminal manifest. Its canonical bytes bind the exact
  key id, producer node id, signer, custody-key digest, epoch-key commitment, exclusive canonical
  epoch end, total segment/record counts, and the complete ordered list of contiguous segment
  indices, ids, record counts, open/close times, and header/footer hashes. Paths are local transport
  metadata and are never signed. The online owner verifies every listed segment one at a time,
  publishes the owner-only manifest atomically, and only then destroys the live epoch key; restart
  accepts an existing marker only when it is byte-for-byte the same verified result. The decoder is
  bounded to 65,536 segments (at most 655,360,000 records at the segment limit), verifies the
  signature over every preceding byte, and exposes a streaming verifier whose mandatory terminal
  check detects omission, reordering, duplication, extra input, mixed keys, and mixed commitments.
  Decrypted records must repeat the pinned node id and key-id jurisdiction
- **Disclosure log** — the offline ledger uses the registry's exact RFC 6962 construction with a
  two-phase intent/completion record for every unwrap. Its public leaves contain timestamp,
  jurisdiction, purpose, epoch ids, result counts, and salted commitments to the order reference,
  selectors, requester, and approver — never the raw values. The encrypted private audit record
  carries those values. Open/recovery strictly streams the unchanged length-prefixed file (bounded
  to 512 MiB and 100,000 leaves), truncating only an incomplete final record. Runtime state retains
  an 8-byte leaf-offset index, request-id uniqueness and outstanding-intent state, an incremental
  Merkle frontier/cached root, and bounded proof blocks rather than the file, decoded leaves, or the
  full leaf-hash tree. The authenticated restart sidecar is constant-sized committed-prefix
  metadata — file identity/mutation marker, generation, exact byte length, root, last-leaf hash,
  and at most one exact 4 KiB pending leaf — and never persists those growing indexes. Each open
  rebuilds them from the bounded stream and requires the computed generation/root/last hash to
  match before recovering the pending record. Appends therefore perform only constant-sized
  sidecar writes plus the durable public record. Root/checkpoint reads do not rehash history;
  arbitrary inclusion and consistency proofs remain available and use the same verification
  machinery as the names log
- **Destruction is by key and inventory** — deleting every custody copy of a wrapped epoch key makes
  all segment bytes unreadable. The scheduler inventories live DB state, WAL/sidecars, snapshots,
  backups, KMS versions, and Shamir shares before it may record completion; deleting one row while a
  backup still unwraps is not erasure. A signed terminal manifest and its pinned producer, signer,
  custody digest, key id, and epoch end remain mandatory. Missing, extra, or corrupt ciphertext
  bodies are recorded durably as integrity degradation before destruction, but cannot retain their
  decryption key forever; disclosure still rejects any incomplete or corrupt bundle. A single
  transactional hold state machine pins an epoch against expiry, and a §2703(f) request is a
  renewable 90-day preservation hold
- **Inventory ceremony** — strict PPinv v3 embeds retention-policy v1, including fixed US/EU product
  choices, counsel-selected Türkiye days within 365–730, and a nonzero approval-record commitment.
  Operator config v2 is required; v1 is rejected rather than assigned an implicit policy. Per-epoch
  private declaration, staging, import, and active paths are distinct. The declaration must
  state every required storage class as present or verified absent with a unique nonzero commitment.
  Create/provision/import use atomic no-replace publication; update is a retained-state-only,
  monotonic full-snapshot merge that cannot remove or mutate an existing copy. It can extend the
  policy under a new approval commitment but cannot shorten computed retention. Raw locators and
  absence evidence come only from a bounded owner-only declaration file, never arguments or output

```
ppcompliance status
ppcompliance inventory create    --epoch <id>
ppcompliance inventory provision --epoch <id>
ppcompliance inventory import    --epoch <id>
ppcompliance inventory update    --epoch <id>
ppcompliance unseal     --epoch <id> < private-request.toml
ppcompliance shred      --before <date> [--dry-run|--execute]
ppcompliance hold       --epoch <id> --until <date> < private-request.toml
ppcompliance hold renew --epoch <id> --hold <hold-id> --until <date> < private-request.toml
ppcompliance hold release --epoch <id> --hold <hold-id> < private-request.toml
ppcompliance checkpoint
```

`unseal` and `hold` accept raw case values only through a bounded (32 KiB), strict version-1 TOML
declaration on stdin. `unseal` requires `order_reference`, `requester_identity`, and one to eight
canonical selectors; every hold place, renewal, and release requires `order_reference` plus two
distinct signatures from the pinned approval roster. Renewal persists its predecessor hold id and
release names the exact canonical hold id. Unknown fields and versions fail closed.
Raw order references, selectors, and requester identities are rejected in argv and environment
variables so shell history, process inspection, and inherited environments cannot expose them. The
stdin descriptor comes from the separately protected case-management boundary and uses a neutral,
owner-only name if file-backed; terminal stdin is rejected. `unseal` appends its disclosure leaf **before** it prints anything —
reading a record without leaving a trace should require patching the binary — and atomically
advances the signed checkpoint handoff before releasing disclosure bytes. It establishes a signed
empty floor before the first intent, refuses a missing floor for a nonempty ledger, and rejects a
handoff that is newer, conflicting, wrongly signed, or not RFC 6962-consistent with the local head.
`shred` defaults to
`--dry-run`: it destroys evidence irreversibly, which is correct at end of retention and catastrophic
a day early. Held and unexpired epochs are skipped and counted independently; every eligible epoch
is attempted, failures do not suppress other epochs, and persisted `Shredding` states always resume.
Discovery releases each authenticated directory guard and decoded manifest after retaining only its
bounded commitment/integrity summary. Execution reopens and authenticates exactly one epoch at a
time and rejects any manifest-commitment change before it can delete a key copy, so the number or
size of other eligible manifests cannot exhaust custody file descriptors or memory.
Dry-run and execution report integrity-degraded trace epochs. Execution persists the authenticated
terminal-manifest commitment and monotonic `Verified`/`Degraded` result before requesting deletion
of the first key copy; a later clean read cannot erase an earlier degradation.
`checkpoint` atomically publishes an owner-only signed note to a configured handoff path and never
replaces a retained floor with an older or inconsistent head. A
separately provisioned and monitored scheduler must copy that file to the public transparency
endpoint on the names-log cadence; the publisher receives neither signing keys nor private state.

### 5.7 `pigeonpost-mcp` and `pigeonpost-cli`

Thin shells over `pigeonpost-client`. Tool surface and CLI verbs are already specified in
`integration.md` §MCP server and `node.md` §Commands. Those sections define the public integration
and node-operator subsets, while `pigeonpost --help` and the checked CLI parser are the exhaustive
command contract, including policy, trust, registry-compliance, directory lifecycle, and pending
delivery administration.

Registry trust import is an operator/provisioning action and is not model-callable through MCP;
MCP exposes only the public trust status and an exact-confirmation reset. The default MCP tool
deadline is 130 seconds, leaving 10 seconds beyond the registry client's complete 120-second audit.
Deadline and cancellation paths cooperatively stop and join the worker before reporting timeout or
suppressing a canceled response, so no detached state mutation can commit after the caller observes
failure.

## 6. Data models

### 6.1 Client SQLite

```sql
meta(key PK, value)
lofts(url PK, pubkey, role, state, added_at, drain_after, allow_local, retention_days)
outbox(row PK, message_id, to_addr, loft_url, wrap BLOB, token,
       allow_local, attempts, last_error, next_attempt_at, created_at, sent_at,
       terminal_at, terminal_reason)
cursors(loft_url, address, cursor, PRIMARY KEY (loft_url, address))
directories(url PK, signing_key, added_at, enabled, last_generated_at, etag, snapshot BLOB)
registry_config(id=1 PK, url, origin, checkpoint_key, witness_threshold,
                minimum_size, minimum_root, freshness bounds)
registry_witnesses(name PK, pubkey UNIQUE)
registry_pin(id=1 PK, size, root, witnessed_at)
registry_audit(id=1 PK, state BLOB) -- signed note + compact full-log frontier + derived key states
registry_handle_audit(id=1 PK, state BLOB) -- global handle replay progress; never a second pin
registry_handle_projection(handle PK, pubkey, subject, log_index)
  INDEX (pubkey)
handle_resolutions(handle PK, address, pubkey, log_index, checkpoint_size, resolved_at)
compliance_keys(key_id PK, publication BLOB, public_key, log_index,
                checkpoint_size, witnessed_at, fetched_at)
messages(id PK, from_pubkey, from_address, received_at, read, state, body, attribution, deleted_at)
                                              -- state: pending|accepted|deleted; deleted keeps id only
spam_marks(message_id PK, marked_at) -- durable idempotency ledger; one penalty per message id
allowlist(pubkey PK, added_at, reason)
scores(pubkey PK, score, updated_at)
resolutions(addr PK, pubkey, successor_hash, seq, lofts, fetched_at, pow_min,
            attribution_requirement BLOB NULL)
rotation_chains(from_addr PK, to_addr, record BLOB, fetched_at)
own_rotations(from_addr PK, record BLOB, source_record BLOB, target_record BLOB,
              lofts, grace_until)
own_record_publication(id=1 PK, address, record BLOB, updated_at)
own_record_publication_targets(url PK, allow_local, rendezvous, completed)
own_rotation_publication_targets(from_addr, url, allow_local, rendezvous, completed,
                                 PRIMARY KEY (from_addr, url))
placement_health(id=1 PK, directory_refresh_degraded, last_attempt_at)
storage_accounting(id=1 PK,
                   inbox_message_limit, inbox_body_bytes_limit,
                   outbox_row_limit, outbox_payload_bytes_limit,
                   inbox_messages, inbox_tombstones, inbox_body_bytes,
                   outbox_rows, outbox_payload_bytes)
```

`resolutions.successor_hash` is the trust-on-first-use pin from `keys.md`; a later change is
surfaced as hostile, never silently accepted.

The three publication tables are an exact write-ahead journal, not a reconstructable cache. The
signed record or rotation bundle is committed before network I/O, and completion is retained only
for a matching URL in the matching exact plan. `placement_health` makes directory-control-plane
loss visible without conflating it with cached message-path availability. Client schema 12 added
these tables transactionally. Schema 13 added bounded persistence validation, exact storage
accounting, immediate wrap/token erasure after a copy succeeds or becomes terminal, and finite
payload/row limits. Schema 14 added permanent id-only inbox tombstones, authenticated loft-retention
state, and the indexed/validated public-key lookup. Schema 15 adds the nullable fixed-length
canonical attribution requirement to signed resolution cache rows and exact sender/recipient
settings to metadata. The requirement codec rejects unknown versions, jurisdictions, zero
authorities, wrong lengths, and trailing bytes on write and cached-resolution access. Every step is
transactional and shape-verified; a failed migration leaves the prior version untouched, and older
clients must not open the migrated database.

File-backed client state uses the §4 platform-custody boundary. On Linux/macOS, the internal Unix
custody layer retains descriptor-relative parent/main/WAL/SHM objects; Windows retains the complete
ancestor chain and no-delete-share handles for main/WAL/SHM under the protected current-user DACL.
Both reject an unsafe pre-existing rollback journal before SQLite, verify the connection path after
open, and revalidate the named objects before use. Identity/successor keys and adjacent runtime/trust
files use the same platform policy rather than relying on a later permission repair. All other
targets reject persistent client storage before path access or mutation.

### 6.2 Loft SQLite

```sql
events(cursor PK AUTOINCREMENT, id UNIQUE, recipient, stored_at, expires_at, size, blob BLOB)
  INDEX (recipient, cursor); INDEX (recipient, stored_at); INDEX (expires_at)
recipient_policy(pubkey PK, seq, policy_version, policy BLOB, updated_at)
                -- signed canonical BLOB is authoritative and contains the capped token-hash set
agent_records(address PK, record BLOB, seq, updated_at)            -- self-verifying; rendezvous serving
rotation_records(from_address PK, from_pubkey UNIQUE, to_address, to_pubkey, seq,
                 activated_at, grace_until, record BLOB, stored_at)
storage_stats(singleton=1 PK, bytes_used, event_count, control_bytes, control_reserved)
trace_admission(singleton=1 PK, window_start_ms, admitted)

trace_segments(segment_id PK, key_id, purpose, jurisdiction, opened_at, closed_at, path UNIQUE,
               wrapped_key BLOB, record_count, first_hash, final_hash, state)
```

The existing deployed schema already stores a signed policy BLOB. That BLOB remains the sole source
of truth; shadow columns for individual signed fields would create dual authority. RecipientPolicy
v3 and its signature domain authenticate the exact optional attribution scope; the retained boolean
is only a consistency mirror for v1/v2 JSON compatibility. `PRAGMA user_version` drives
ordered, idempotent, transactional migrations with fixture tests from every released schema. A node
refuses to open a newer schema and never relies on `CREATE TABLE IF NOT EXISTS` as migration.
Loft schema v5 charges policy/record/rotation control rows exactly and reserves the bounded future
rotation row when an agent record is accepted, so a later rotation cannot exceed capacity after its
source record was acknowledged. Schema v6 adds the restart-safe UTC-minute trace-admission singleton.
Opening an existing predecessor conservatively burns the upgrade minute; fresh databases begin at
zero. Its `admitted` value is a 64-slot reservation high-water rather than a count of successfully
served requests; unused reserved slots remain spent after crash, restart, rollover, or a limit
change. Startup reconciles every event, control, reservation, segment, and trace-admission invariant
against the exact current schema before serving. A persistent store additionally retains and
revalidates its private parent, main database, WAL, and SHM, validates any rollback journal, and
proves SQLite's reported connection path. Linux and macOS support public/compliance service; Windows
supports a private loopback Loft database only. All other targets reject persistent Loft storage
before path access or mutation.

**Sealed trace records are append-only files on disk, not rows.** They are opaque blobs with a
different lifecycle, access path and deletion semantics from everything else here; only the wrapped
key and segment metadata belong in SQLite. Each terminal `.ppmanifest` lives beside its purpose's
segments, is mode `0600` (or stricter), and is atomically published only after the exclusive epoch
end. Legal holds and disclosure state live in the offline custody database, not in a loft table that
the retention process could bypass.

Before recovery or live-key mutation, every online purpose directory acquires the fixed
`.pigeonpost-trace-writer-v1.lock` through a no-follow, close-on-exec, owner-only descriptor and
takes its exclusive OS lock without waiting. The descriptor must name one stable regular inode with
one link and mode `0600` or stricter, and remains held through worker shutdown; a second writer or a
replaced/unsafe artifact fails closed. The registry acquires its network and identity leases in
canonical path order and drops a partial acquisition on failure. The lock is runtime coordination,
not evidence: export copies only the signed terminal manifest and its declared segments, and the
offline exact-layout verifier rejects a copied lock or any other extra entry. This writer/lease
contract is Linux/macOS-only in v0.2: every persistent segment-writer constructor and every public
caller returns unsupported before path access or mutation on all other targets.

**Offline inventory is strict binary state, not a loose runbook.** PPinv v3 carries the typed key id,
retention-policy v1 (US days, EU days, Türkiye days, test days, and counsel approval commitment),
computed expiry, canonical committed copy declarations, legal holds, shred state, and optional
terminal-manifest integrity evidence. Canonical v2 state decodes with no integrity evidence and is
rewritten as v3 on the next mutation; v1, unknown versions, trailing/truncated bytes, noncanonical
copy order, missing storage classes, zero commitments, policy/config mismatch, and non-monotonic
updates fail closed. A v1 inventory must be recreated or imported through `ppcompliance inventory`;
it is never silently assigned a policy.

### 6.3 Directory SQLite

Directory schema v4 is authoritative for `lofts`, `pending_claims`, `probes`,
`retention_canaries`, `directory_meta`, and `directory_mutation_reservations`, with explicit
probe-due, age, and reservation-age indexes. `PRAGMA user_version` permits only the verified v3→v4
transactional migration or the exact current schema. Startup compares canonical `sqlite_schema`
table and explicit-index definitions with generated pristine/reference schemas; an unknown version,
missing index, or weakened primary/unique/foreign-key/`CHECK` shape is refused without schema
mutation. Connection-level SQLite pragmas such as WAL mode may already have been applied.
Public serving additionally requires the owner-custodied persistent database descriptor retained by
the Directory open path. Across Linux/macOS and Windows it validates the rollback journal, retains
parent/main/WAL/SHM identities through connection shutdown, and verifies the connection's reported
path. The internal Unix layer uses descriptor-relative identity on Linux/macOS; the Windows layer
uses retained no-delete-share handles, protected current-user DACLs, reparse/link checks, and full
volume/file identities. An in-memory or replaced database can be used only by explicit test paths
and cannot serve publicly. Listener startup and public `/ready` repeat the custody proof, so
post-start replacement or missing WAL/SHM withdraws readiness.

### 6.4 Registry SQLite

Registry schema v8 commits the append-only `entries`, RFC 6962 `merkle_nodes`, `checkpoints`,
`registry_state`, published witness state/receipts, handle history/current binding projection,
compliance-key history, independently authorized directory mutation streams, one-shot identity
challenges whose consumption atomically records the exact binding sequence, the durable
`global_binding_admission` UTC-minute singleton, and the migration ledger in the same immediate
transaction as each accepted append. Every supported existing predecessor, including the
authenticated unversioned v0.1.0 import and the v6→v7 path, conservatively burns its current UTC
minute rather than granting an extra restart window; v7→v8 invalidates ephemeral outstanding
identity challenges that cannot retroactively name an atomic result, and only a genuinely fresh v8
database starts at zero. The stored admission count is a 64-slot reservation high-water, so unused
reserve may cause safe early limiting but can never create a refund.
Ordered migrations exist only for recognized released/prototype predecessors; newer or unknown
versions fail closed. Startup replays and byte-verifies stored leaves, projections, Merkle state,
and checkpoint identity before serving readiness; missing or altered required tables/columns fail
the same startup verification rather than being recreated by `CREATE TABLE IF NOT EXISTS`.
Registry HTTP limits have explicit audited maxima and are validated before any semaphore or limiter
allocation, so oversized operator configuration returns a startup error rather than panicking or
allocating attacker-amplified state. The Registry origin transport is intentionally HTTP/1.1-only:
an edge may terminate client HTTP/2, but its Registry upstream must not multiplex streams. This
ensures writes on an unrelated request cannot reset the socket-progress watchdog for a stalled
query-free log dump while that dump holds the sole no-absolute-cutoff lane. One process-global
response-body bucket (256 MiB/minute by default, 1 TiB/minute audited maximum) is charged before
every ordinary response body frame or dump chunk and never refunded after later drop, bounding
origin and metered egress even for a continuously progressing mirror or repeated immutable ranges.
Persistent Registry SQLite is Linux/macOS-only in v0.2. It validates the private ancestor,
main/WAL/SHM, and rollback-journal names before use, retains descriptor identities through
connection shutdown, proves SQLite's connection path, and repeats the proof at public
startup/readiness. Existing-only open and legacy migration refuse a missing main database without
creating a database, journal, or sidecar. The CLI rejects Registry serve and executable
compliance-key ceremonies on all other targets before any persistent mutation.

### 6.5 Offline custody artifacts

Offline custody deliberately does not share an online SQLite database. Its authoritative state is
the exact terminal `.ppmanifest` plus declared sealed segments, a strict PPinv v3 destruction
inventory carrying retention/copy/hold/shred state, and the append-only disclosure ledger with its
authenticated restart-state sidecar. Every codec is versioned, bounded, and rejects unknown fields,
trailing/truncated bytes, noncanonical order, unsafe file identity/permissions, or rollback relative
to its authenticated state. The disclosure operator additionally verifies a separately retained
signed checkpoint floor before every read/mutation and advances it before releasing bytes. Rolling
back the log, sidecar, and handoff together is not locally distinguishable; production therefore
requires the independent publisher/monitor to retain and reject any signed head regression or fork.
That publisher evidence is an activation gate, not a codec claim. Updates are atomic and monotonic:
inventories cannot drop committed copies or holds, and an intent leaf must precede its matching
completion/failure leaf.

### 6.6 Registry log entry

```json
{ "version": 1, "seq": 48211,
  "type": "handle_bind|handle_rotate|directory_add|directory_remove|compliance_key_publish",
  "payload": { }, "ts_ms": 1786105721119 }
```

One log, several strictly tagged entry types — the directory reuses the names log rather than
standing up a second one. JSON is its API representation; each variant has immutable canonical leaf
bytes. Unknown stored/API variants fail closed instead of defaulting to another entry kind.

## 7. Milestones

Each code milestone is independently verifiable. M6 also has the explicitly listed legal,
organizational, custody, and independent-witness gates; source code cannot fabricate those facts.

| # | Deliverable | Done when |
| --- | --- | --- |
| **M0** | Workspace, CI, `pigeonpost-core` | Address derivation, envelope round-trip, rotation verification, and PoW pass property tests and published conformance vectors |
| **M1** | `pigeonpost-loft` + minimal client | Two agents on one machine exchange an encrypted message through a real loft; recipient offline at send time; fetch is key-authenticated |
| **M2** | `pigeonpost-client` + CLI | `id`, `send`, `inbox`, `read`, `ack` over SQLite with outbox and cursors; `read` returns `UntrustedBody`; survives restart |
| **M3** | `pigeonpost-registry` | Handle claimed and later rebound through public CLI and MCP surfaces against a real provider flow (GitHub OAuth2 and one true-OIDC provider), including recovery from a completely fresh home after total key loss; `resolve` requires imported trust, verifies inclusion and a fresh strict-majority witness quorum under the stated fault model, continuously audits the exact witnessed prefix to derive the latest valid binding, and persists append-only continuity without holding the agent DB writer during network I/O; log dump downloadable and independently verifiable |
| **M4** | `pigeonpost-directory` + `pigeonpost install` | A second loft joins via submission, is probed, and is selected by a client through capacity weighting; `install` turns a clean box into a private loft with no flags |
| **M5** | Spam layers + MCP + npm release | `acceptAll=false`, PoW, tokens, and local scoring all enforced end to end; MCP tools work from a real MCP client; `@bekirdag/pigeonpost` ships real binaries |
| **M6** | Lawful access (`law.md`) | A trace record survives a round trip and is readable *only* with authorized offline custody; a host holding public keys cannot read a closed segment; a custodian independently authenticates an attribution claim from a wrap while a recipient advertises and enforces one exact jurisdiction/authority scope, a sender explicitly agrees, new admission uses only its witnessed `Active` key, and recipient verification rejects forged, replayed, wrongly-escrowed, legacy-attributed, and time-invalid blocks while telling `Absent` from `Invalid`; shredding an epoch makes every inventoried copy unreadable; held epochs survive retention; intent/completion disclosure checkpoints verify; no observed client/source IP address or raw disclosure selector appears in any ordinary/public log across the full integration run |

**First deployment lands at M1** — see the private bootstrap runbook, which is deliberately not in
this repository.

**Envelope v3 lands before M6 activation** — it is a breaking write-format change required to fix
the independently unverifiable v2 attribution prototype. v2 remains read-only for ordinary
unattributed `0.1.x` messages; v1 is unsupported. The loft gate is activated only after v3 clients,
registry compliance-key history, and both server versions are deployed.

**M6 has non-code prerequisites that no milestone can satisfy** (`law.md` §6): an EU designated
establishment or legal representative notified to a member state and reachable inside 8 hours, a
provisioned compliance keypair in custody, a published legal-process intake address with an
order-authentication procedure, and counsel sign-off on `law.md` §8. The first of these is due
**18 August 2026**.

## 8. Testing

- **Unit** — every `core` primitive, with exhaustive boundary cases and property-style generated
  cases for derivation, envelope, and rotation
- **Conformance vectors** — published fixtures for address derivation, v2 read compatibility, v3
  event ids, typed key ids, attribution signing bytes, the exact RecipientPolicy v2 compatibility
  signature and current v3 bytes, the AgentRecord v1 compatibility fixture and current v2
  signature, and fixed trace codecs, so a
  clean-room implementation can prove itself. The trace/key/disclosure set is checked by
  `cargo test --locked -p pigeonpost-compliance --test conformance`. This is day-one commitment #5
  made concrete
- **Integration suites** — crate-level real-HTTP suites collectively spin registries, directories,
  and multiple lofts and drive real clients through offline delivery, loft failure mid-send,
  witnessed handle delivery, handle rebinding from a fresh home followed by future delivery, key
  rotation with a grace window, and spam rejection. A release acceptance
  script composes the deployed services rather than duplicating their fixtures in a root test crate
- **Adversarial tests** — a loft that drops messages, one that lies about retention, one that serves a
  forged successor commitment, and a harvester bulk-fetching another agent's ciphertexts (must be
  refused by fetch auth). Handle publication additionally tests that only a strictly older witnessed
  binding is retryable and that same-index/newer receipt mismatches and a wrong `handle_bind` versus
  `handle_rotate` kind fail closed. Each must be caught by the mechanism the design says catches it
- **Transport-admission tests** — real TCP peers send incomplete headers and stop reading large
  responses at both the loft and directory. Tests prove the timer closes partial headers, the
  request permit remains occupied after response construction, the transport lifetime reaps a body
  already handed to the socket, and admission recovers only after EOF/drop. Exact fetch/directory
  egress charges independently exhaust their global, source, and recipient dimensions
- **Attribution adversarial tests** — a victim-key forged claim; a valid block lifted to another
  event or recipient; mutated key id/digest/time; a block escrowed to a key absent from the trusted
  log; wrong recipient-signed jurisdiction or authority; all-zero/unknown requirement encodings; a
  block stripped in transit; attributed v2; and a patched client omitting the block entirely. New
  admission tests cover `Active` acceptance, `Retired`/`Revoked` rejection, voluntary attributed
  wraps under an optional policy, and an immutable delayed outbox copy that becomes terminal when
  its key retires. A temporary Loft resolver outage must return retryable 503, retain the exact
  queued wrap, and deliver it after readiness recovers. Client tests cover explicit exact
  agreement, call-local non-mutation, legacy migration, partial policy rollout, scope-change
  rejection of unread old-scope wraps, and the recipient distinction between `Absent` and
  `Invalid`. They also prove that a fresh older witnessed prefix plus Registry outage pins the
  required message's Loft cursor without starving a second Loft, then accepts that same ciphertext
  after a consistency-verified refresh reveals the later `Active` key. A deterministic shared-home
  barrier test proves a concurrent scope mutation cannot cross an in-flight drain lease. The
  offline custodian test uses only the wrap, Registry key history, and custody secret
- **Custody tests** — a host holding only `P_c` cannot read a closed segment; unseal after shred
  fails with the valid custody key in hand; the retention sweep skips a held epoch and says so;
  inventory policy round trips, legacy/tampered/truncated states fail closed, every storage class is
  required, owner-only source custody is enforced, publication refuses overwrite/mismatch, and
  updates cannot remove or mutate existing commitments
- **Trace-writer concurrency tests** — same or overlapping purpose directories refuse a second
  constructor immediately without changing trace evidence; disjoint directories coexist; reverse
  registry directory order cannot deadlock; a dropped owner can reopen; unsafe/replaced lease
  artifacts fail closed; and records acknowledged before and after a refused competitor survive
  crash recovery and public verification
- **No observed client/source IP or raw selector in logs** — capture stdout, stderr, proxy logs,
  public disclosure leaves, metrics, and crash output across the whole integration run and assert
  neither appears. Operator-requested local configuration output is not an application access log;
  the test records every direct and trusted-proxy source address and checks those exact values. This is the
  single easiest way for the sealed-trace design to leak, and it will be
  introduced by someone debugging at 2am rather than by someone writing a feature, so it belongs in
  CI and not in a review checklist
- **CI** — `cargo test --locked --all`, strict native and Windows helper/consumer Clippy,
  `fmt --check`, exact client/server/offline feature and dependency-graph assertions, a six-target
  online release matrix, a four-target Linux/macOS offline-custody matrix, one SBOM and attestation
  bound to every native artifact and its locked target-filtered normal/build Cargo graph, explicit
  SPDX-attestation verification on fresh and resumed releases, an assertion that no Windows
  offline-custody artifact is staged, and a Cargo-metadata gate proving every Rust workspace package
  remains `publish = false`. Both exact staged Windows online assets must, before upload, initialize
  isolated client/CLI state with protected main/WAL/SHM files, start a private-loopback Loft through
  `/ready`, and start the Directory through `/health` and witnessed `/ready`, with owner-private
  DACL checks, bounded process shutdown, and exact temporary-root cleanup. The Directory portion
  uses a read-only size-zero witnessed Registry fixture because the real Registry correctly rejects
  Windows; it proves checkpoint verification without claiming Windows Registry support

## 9. Security

- Threat model lives in `network.md` §Threat model and `spam.md`; this section is implementation
  obligations only
- Private keys leave their owner process only through the configured custody/store protocol;
  nothing is logged that could reconstruct one
- The loft treats every inbound blob as hostile: size cap before allocation, verify before store
- Registry OIDC verification pins issuers to an allowlist; no dynamic issuer discovery
- Untrusted Pigeonpost bodies never reach a log line, an error message, or a shell
- Every replayable surface — fetch auth, policy updates, agent records, rotation records, directory
  entries — carries a monotonic `seq` or a bounded time window, and the verifier rejects
  non-increasing values. Replay is a class of bug, not a per-endpoint afterthought
- Dependencies audited in CI (`cargo audit`); no new runtime dependency without a note in the PR
- **No observed client/source IP address in any log line, error, metric label, or crash report.**
  The sealed trace store is the only place an observed network address may land — anywhere else it sits outside the retention timer,
  outside custody, and outside the disclosure log, and the separation argument in `law.md` §1.2
  collapses
- **`S_c` never in the repository, never in online config, never on a node**, and never copied to an
  ordinary workstation. Enforced by separate package/dependency graphs plus key inventory in §4,
  not by additive Cargo features or policy alone
- Epoch keys are zeroized at rollover; `zeroize` is already a workspace dependency
- **We never hand over keys — only records.** A lawful order is answered by unsealing the named range
  under §5.6 and producing the matched records. Disclosing a key would grant unlimited retroactive
  access far beyond any order's scope (`law.md` §5)

## 10. Resolved items

Formerly open; decided at review.

1. **Loft storage beyond SQLite** — SQLite only, until a *named* operator actually hits its limits.
   The `LoftStore` trait ships at M1 so the door stays open; writing a Postgres backend nobody runs
   is speculative work
2. **Windows** — the client/CLI binary, client state, Directory database, and a private loopback Loft
   database ship with audited retained-handle online custody. `pigeonpost install` service mode,
   production regulated Loft traces, Registry serving/operator ceremonies, and `ppcompliance`
   remain Linux/macOS-only in v0.2. Windows offline custody still needs an audited owner-DACL,
   link/identity, parent-path, inventory, and atomic-replacement implementation; the online helper
   neither supplies nor waives that separate boundary. Unsupported regulated roles fail before
   persistent mutation rather than silently using a weaker file layer
3. **Capacity default at install** — `max(1 GiB, min(20 GiB, floor(20% of free disk)))`, overridable
   with `--capacity-gb`. The fixed cap bounds a runaway on shared boxes; the percentage stops small
   VPSes from over-promising; and the 1 GiB floor keeps the generated configuration within the
   runtime's valid range on very small or nearly full filesystems. `install` prints the chosen
   number and how to change it. If free space cannot be determined, it fails before writing identity
   or configuration unless the operator supplies an explicit bounded value; it never guesses
4. **General Nostr traffic** — no, and now structurally so: envelope v3 and key-authenticated fetch
   mean a Pigeonpost loft is not usable as a public Nostr relay. A flag to accept foreign traffic
   would be scope creep spent as someone else's bandwidth
