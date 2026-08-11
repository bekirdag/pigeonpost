# Pigeonpost — Architecture

Status: settled architecture; implementation contract and compatibility rules live in `sds.md`.
Opened: 2026-08-07

## The three layers

The system splits into three independent layers. Only one of them is hard.

| Layer | Needs global consensus? | Cost |
| --- | --- | --- |
| **Identity** — the keypair that *is* the agent | No. Self-certifying. | Free, infinite, instant |
| **Transport** — carrying and holding messages | No. Federated relays. | Free |
| **Naming** — provider-scoped `/github/superaidev` | **Yes**, for human-readable handles only. Key addresses need none. | All the difficulty lives here |

Keeping these separate is what makes the rest of the design work. In particular, **identity is never
derived from the name**, which is what allows an agent to exist immediately as a bare keypair and
claim a human-readable name later without changing who it is.

## Stack

| Layer | Choice |
| --- | --- |
| Identity | Ed25519 keypair |
| Naming | Self-certifying key addresses; provider-proof-gated handles over mirrored namespaces |
| Registry | Append-only Merkle transparency log |
| Registry integrity | Configurable witness-quorum verification (C2SP `tlog-witness` / Sigsum model); independent operation is a deployment prerequisite |
| Transport | Pigeonpost nodes ("lofts"); gift-wrap ideas are borrowed, not Nostr wire traffic |
| Encryption | Gift-wrapped messages — NIP-59 pattern, own wire format (envelope v3, `sds.md`) |
| Integration | Local MCP/CLI/library surface; Docdex adoption is outside this build |

**Division of storage:** the registry holds names — permanent, public, tiny. Lofts hold messages —
expiring, encrypted, private. Messages never touch the registry.

## Identity

An agent generates an Ed25519 keypair locally. That key is the identity. No registration, no fee, no
coordination, no permission.

Consequences:
- An agent is addressable from the moment it exists — its key address is derived, not granted
- Human-readable handles are an optional, later upgrade — cosmetic, except that they are also the
  only recoverable tier
- Because the address *is* the key, rotation would change the address. Agents therefore commit to a
  successor key at creation, which is what lets an address survive rotation and compromise. Losing
  both the operating key and its successor loses the address permanently — see `keys.md`

## Naming

### The constraint applies to only half the problem

Zooko's triangle: an identifier cannot simultaneously be human-meaningful, secure, and decentralized.
Blockchains "square" it only by adding transaction costs and governance risk — both rejected here.

But the triangle binds only *human-meaningful* names. Drop that corner and the problem evaporates: an
identifier derived from the key is secure and decentralized for free, and needs no registry, no
consensus, and no permission. Agents mostly do not need a meaningful name — they need somewhere to be
reached.

So the namespace splits in two, with different rules, because the tiers have different problems.

| Tier | Form | Gate | Registry | Squattable? |
| --- | --- | --- | --- | --- |
| **Key address** | `/k/j5pxq82nf4wt3h9m6rbdck0syv` | None | None — self-certifying | No: not chosen, computed |
| **Provider handle** | `/github/wodo` | Provider identity proof | Transparency log | No: gated on a spam-defended identity |

**No agent is ever blocked on a human.** A key address exists the moment the keypair does. The
provider gate applies only to the optional human-readable tier.

### Tier 1 — key addresses (free, permissionless, no human)

The address is a fingerprint of the pubkey:

```
addr = "/k/" + base32( SHA-256( pubkey ) )[:26]     # 128 bits
```

No registration call. No log entry. No allocation decision, therefore nothing to squat, gate, or
adjudicate — you cannot claim an address you do not hold the key for, and you cannot choose which one
you get.

**Resolution without an authority.** A truncated hash is one-way, so a sender holding only the
address still needs the pubkey to encrypt to. It fetches a **self-signed agent record** — pubkey plus
loft list, signed by the key itself:

```json
{ "pubkey": "ed25519:...", "successor_hash": "sha256:...", "seq": 0,
  "lofts": ["https://..."], "sig": "..." }
```

(`successor_hash` commits to the next key in advance, so an address survives rotation and compromise
— see `keys.md`.)

Any loft, mirror, or CDN can serve that record, because serving it confers no power: the recipient
verifies `base32(SHA-256(pubkey))[:26] == addr` and checks the signature. A wrong answer is
arithmetic anyone can catch. Directories are a **cache, not an authority** — the property the
human-readable tier needs a whole transparency log to achieve.

Consequences worth stating:
- Key addresses never enter the log, so the log stays small and free of Sybil pressure
- No witness policy, no inclusion proof, no cosigned head on this path
- 128 bits is sized against *second*-preimage (2¹²⁸) — targeting a specific victim. Birthday
  collisions between two attacker-held keys buy nothing. Exact length is tunable
- Not memorable, and not meant to be. This tier is for machines and for pasting

### Tier 2 — handles (provider-proof-gated)

A free, permissionless, first-come *human-readable* namespace fails empirically, not just
theoretically. Of roughly 120,000 Namecoin registrations, 28 were unsquatted with non-trivial
content. Namecoin *charged* to register; free would be strictly worse.

So for this tier the design question is not "which ledger" but **what scarce thing gates a
registration** — and the answer is an identity that already exists and is already spam-defended:

```
/github/superaidev     claimable only by GitHub user `superaidev`
/google/104729183746501928374  claimable only by that Google subject
```

Squatting becomes structurally impossible: the allocation decision already happened somewhere that
fought spam for a decade. The implemented adapters are GitHub OAuth2 and Google OIDC. Adding another
namespace requires a verifier for that upstream allocation; it is not a configuration-only alias.

This follows the same broad upstream-identity-in, verifiable-binding-out pattern used by systems such
as Sigstore/Fulcio, while respecting that GitHub user identity uses OAuth2 rather than OIDC.

#### Provider-scoped names

`/github/superaidev` is the canonical provider form and is never auto-aliased to a bare `/superaidev`.
The two are separate names with separate owners: proving the GitHub account `superaidev` earns
`/github/superaidev` and nothing else.

#### How many handles one account may hold

**Three.** A provider account — one GitHub login, one Google subject — may hold up to three handles
at once, counted on the stable opaque subject rather than the current display name.

The reason there is a number at all: upstream names are mutable. An account that renames would
otherwise have to choose between the name people already published and the name the provider now
shows. Allowing a small number lets it keep both, without opening the namespace to an account that
renames in a loop.

Mechanics that matter:

- Counted at admission inside the same writer transaction that appends the leaf, so two concurrent
  claims for one account cannot both see a free slot
- **Rotations do not consume a slot** — a rotation rebinds a handle the account already owns, and
  charging it would make an account at its limit unable to recover from key loss
- Re-sending an identical claim is idempotent and does not consume a second slot
- A handle held by a *different* account is still a binding conflict, answered as such rather than
  as a quota breach

Earlier schemas enforced exactly one handle per subject with a `UNIQUE` constraint. The allowance is
now a counted limit rather than a constraint, so changing the number is a constant rather than a
migration.

#### Persistence

Handle history is append-only. Bindings change through authenticated rotation records rather than
expiry or mutation; key addresses never expire because there is no allocated pool to reclaim.

#### Registration flow

```http
POST /v1/register
{
  "handle": "/github/superaidev",
  "pubkey": "<lower-case hex Ed25519 key>",
  "signature": "<signature by that key over the canonical claim payload>",
  "proof":  "<provider-tagged identity proof — OIDC token, or OAuth2 code for GitHub>"
}

201 { "handle": "/github/superaidev", "log_index": 48211, "inclusion_proof": { ... } }
```

`POST /v1/rotate` has the same key-possession and fresh-provider-proof gate and appends a
`handle_rotate` leaf for the handle's stable provider subject. Operators use
`pigeonpost handle rotate`; agent frameworks use `pigeonpost_rotate_handle`. Both wait for the exact
receipt leaf at a fresh witnessed head. This restores future handle routing after total local key
loss, not the old address, state, or ciphertext.

One call. Free. No domain. No wallet.

A handle is an **alias onto a key address**, never a replacement: the keypair remains the identity,
so an agent can gain, lose, or change handles without becoming a different agent.

## Registry

### Why not a blockchain

Names are a **log**, not a ledger of balances. There is no double-spend to prevent — the requirement
is only that everyone sees the same `name → pubkey` bindings. That is a transparency log, not a
consensus network.

Rejected explicitly:
- **Hyperledger Fabric / Indy** — permissioned; someone runs the validators, which is either us
  (fails "not under our control") or a consortium we must recruit and govern. Governance dressed as
  technology.
- **Public L1/L2** — transaction fees, wallets, seed phrases. Fails "free" and kills adoption.

### The design

- The reference topology begins with one append-only Merkle log under the initial operator's control
- Configured external **witnesses** verify append-only-ness and cosign the tree head through the
  C2SP `tlog-witness` protocol. A witness is a small service plus a signature; its value depends on
  operational independence
- Clients enforce a nonempty **strict-majority witness policy**: accept only if cosigned by k of
  {A…J} where `2k > N`. This rejects 1-of-2 and 1-of-3 while preserving 1-of-1 and 2-of-3
- A strict majority guarantees set intersection for the same roster, not an honest intersection.
  No-gossip split-view prevention additionally requires `f < 2k - N` for at most `f` equivocating
  witnesses. Thus 2-of-3 tolerates no equivocator; if the only assumption is “at least one of N is
  honest,” use N-of-N. Different client rosters require a guaranteed non-equivocating overlap or
  gossip/out-of-band checkpoint comparison
- Inclusion proofs grow logarithmically—about 20 sibling hashes at the v0.2
  1,000,000-leaf envelope—but proof size alone does not make full-history bootstrap unbounded

Reference material: C2SP `tlog-witness`, Sigsum, Trillian, Go's `tlog` package, and Filippo
Valsorda's transparent-keyserver writeup (name→key bindings in a tlog is precisely this use case).

### What makes it not ours

Node count is not neutrality. The reference implementation, repo and seed infrastructure confer de
facto control for years regardless. Real neutrality comes from **exit rights**:

1. The entire log is publicly downloadable through the no-query dump, while immutable exact
   `[from,to)` NDJSON ranges expose the same canonical history for bounded verification and caching.
   One isolated full-dump lane has an idle-progress deadline but no absolute cutoff; slow mirrors
   cannot consume the separately bounded product range lane
2. Anyone can mirror it, and mirrors are first-class
3. Clients hold a strict-majority witness policy they choose, not one we ship
4. On misbehavior, the community forks the log at the last honest checkpoint and repoints clients —
   everyone keeps their names

This is how Certificate Transparency governs the entire web PKI across competing operators with no
consensus protocol between them.

### The v0.2 bootstrap boundary

A fresh client verifies one witnessed head, then replays the exact continuous prefix in NDJSON
ranges of at most 8,192 leaves, 32 MiB, and 30 seconds per attempt. Each range is immutable and has
an exact ETag, so an untrusted CDN or mirror can serve it safely. Delivery failure may fall back to
the existing JSON range pages of at most 256 leaves after discarding partial segment state.
Malformed entries, invalid handle/compliance state transitions, sequence gaps, and a final RFC 6962
root mismatch are authenticated-data failures and remain terminal.

The v0.2 support boundary ends at 1,000,000 leaves and 256 MiB canonical NDJSON, assuming at least 10 MiB/s
effective throughput and at most 100 ms RTT, under one 120-second bootstrap deadline. Higher scale
needs a future authenticated snapshot/map/checkpoint design and new measurements; the architecture
does not infer unlimited bootstrap capacity from logarithmic inclusion proofs. That timing envelope
assumes the immutable range route; bounded JSON pages are a safe compatibility fallback, not the
million-leaf performance path. Repository tests establish the protocol and network-budget contract,
not measured million-leaf production-hardware throughput.

### The canonical namespace

Exit rights are not the same as a free-for-all namespace. `/github/bekirdag` is only worth publishing in
a README if it means the same thing to everyone who reads it — a handle that resolves differently
depending on which registry a client happens to ask is not a name, it is a coincidence.

So the reserved namespaces are **universal by definition**:

- `github` and `google` resolve **only** at origin `pigeonpost.dev/registry`
- Every checkpoint already carries that origin in its C2SP signed note, so the binding is
  cryptographic rather than conventional — a checkpoint from any other origin is not a checkpoint for
  these namespaces
- Clients pin the canonical origin **and** its checkpoint key for `github` and `google`. A registry
  serving those namespaces under a different key is not answering the question the client asked
- A conforming implementation that serves `github` or `google` from another origin is non-conforming, in
  the same sense that an alternate DNS root serving `.com` is non-conforming

**Self-hosting keeps its own space.** Anyone running their own registry uses a namespace they
control, and the client resolves it without special-casing:

| Namespace | Authority | Universal? |
| --- | --- | --- |
| `k` | Nobody — self-certifying | Yes, by construction |
| `github`, `google` | `pigeonpost.dev/registry` | Yes, by definition |

Third-party registries remain possible through the dump-and-fork exit right, but they do not acquire
canonical provider namespaces merely by choosing a similar path. A new provider namespace requires
an implemented identity verifier and an explicit compatibility decision.

This is deliberately narrower than "not ours." Naming is the one place where a shared answer is worth
more than an independent one, and pinning is what makes the shared answer real. Everything that made
the log escapable still holds: the dump, the mirrors, the fork-at-last-honest-checkpoint path. What
changes is that a fork of the *canonical* namespace is a visible, deliberate community act rather
than something a client wanders into by pointing at a different URL.

**Note the asymmetry with lofts.** Lofts are fungible storage and are never hardcoded in the client
(`capacity.md`) — an agent draws from a pool and our share falls as operators join. The registry is a
naming authority and *is* pinned. Different jobs, opposite treatment, on purpose.

## Transport

### Storage model

Relays ("lofts") are durable inboxes, not live sockets. This store-and-forward model is what makes
the offline case work.

```
sender online, recipient offline  →  loft stores the wrapped message
recipient wakes                   →  fetch events for my pubkey since <cursor>
                                  →  read, advance cursor, disconnect
```

No background listener or daemon on the agent side. The implemented transport is authenticated HTTP
request/response; WebSocket support is deferred and is not part of the compatibility contract. A
hosted loft is a separate supervised background service.

### Delivery

- Each agent publishes a signed **agent record** carrying its loft list so senders know where to
  deposit Pigeonpost messages
- Senders publish to 2–3 lofts for redundancy
- Retention is loft policy—the implemented default is 30 days. Pigeonpost messages expiring is a
  feature, not a gap
- Cursor is client-side; the loft stores no per-client state

### Encryption

Gift wrapping, NIP-59 pattern (Pigeonpost envelope v3 — wire format in `sds.md`):
- Inner message sealed with X25519 ECDH + HKDF-SHA256 + XChaCha20-Poly1305
- Sealed, then wrapped with a **fresh random keypair per recipient**
- Timestamps randomized up to two days
- No shared conversation identifier to correlate on

A loft stores a wrapped event addressed to a pubkey; the wrap does not reveal the sender's long-term
key, true send time, content, or kind. When the recipient requires attribution, the v3 wrap also
carries a fixed encrypted sender claim. The loft can validate its public shape and witnessed key
epoch but cannot decrypt it; only the separately authorized offline custodian can recover the claim.
A regulated public loft separately seals observed source-network and exact receipt metadata under
short-lived purpose-specific keys. Message content remains sender-and-recipient-only.

### Why messages never go in the log

Immutability is a liability for message bodies. Metadata (who ↔ whom, when, how large) would become
permanent and unrevocable, and encrypted bodies on a public append-only log are a
harvest-now-decrypt-later trap: one future key compromise retroactively opens the entire history.

## Spam

An openly advertised, free, permissionless inbox is a spam magnet, and the GitHub-README use case
*requires* the open-inbox mode — the hardest case. Free key addresses make identities cost a hash.
This needs an answer at design time, not later.

Gift-wrapping means a loft cannot derive an authenticated sender key, so anything keyed on sender
identity is necessarily client-side. Observed network source is separately sealed trace metadata,
not sender identity. Layered defense, cheapest first:

- **Loft policy** — operators set their own rate, size, and acceptance rules
- **Capability tokens** — when the signed token gate is enabled, every wrap also carries a live,
  revocable presentation bound to the loft key and exact canonical origin; enforced at the loft
  without deanonymizing anyone or making a captured presentation portable to another endpoint
- **Proof-of-work stamps** on every wrap, at the recipient's flat advertised floor; zero disables it
- **Closed by default.** `acceptAll = false`; strangers land in a pending queue
- **Local sender score** — client-side reputation, decremented by mark-as-spam, never shared

Full evaluation, including the options rejected and why, in `spam.md`.

## Intended operator model

This table describes architecture, not current deployment status.

| Component | Who runs it | Cost |
| --- | --- | --- |
| Loft (relay) | Us + anyone | ~$10–20/mo VPS, or co-located on existing infrastructure |
| Registry log | Us initially | Names are KBs; negligible |
| Witnesses | Third parties | Free to us — that is the bootstrap ask |
| OIDC app | Us | Free |
| Transaction fees | — | **None.** No chain, no token |

Bootstrap requires one log plus independently operated witnesses. Add witnesses as adopters join.
If it succeeds, someone runs a second log and clients accept both — that is the moment it stops
being ours. Source support for quorum verification is not proof that those operators exist.

## Docdex integration boundary

The SDS places Docdex integration outside this build. A Docdex maintainer can adopt the Pigeonpost
client/MCP source surface, once released and verified, without changing the protocol:

- IPMC **seat identity** (owner + device + repo → one stable "developer of this repo" address) maps
  to a Pigeonpost keypair
- IPMC inbox/outbox tools map to Pigeonpost fetch/send
- Existing IPMC design work — seat derivation, MCP tool surface, local SQLite outbox and cache, the
  untrusted-content envelope — carries over unchanged
- Any Docdex-specific sequencing or state migration belongs in the Docdex repository, not here

## Prior art surveyed

| Project | Verdict |
| --- | --- |
| **A2A** (Google/Linux Foundation) | Mature, 150+ orgs — but an agent is a server with a URL. Wrong shape for offline agents. Complementary, not competing. |
| **MCP** (Anthropic) | Agent-to-tool. Pigeonpost is *exposed as* MCP tools; not an alternative. |
| **ASMTP** | Excellent inbox design (envelope/cursor/monitor primitives, "headers cheap, bodies opt-in"), but ~4 stars and one company. Worth borrowing the data model; no interop value. |
| **ATP / AMTP** | SMTP-shaped agent messaging. ATP is an expiring individual IETF draft. Small. |
| **ANP** | DID-based (`did:wba`), v1.1. Direct-connection focused. Heavier identity stack than we need. |
| **DIDComm v2** | Genuinely mature, mediator/pickup gives real offline inboxes, Rust libraries exist. Rejected as overweight: its value is trust between strangers, most of which we get from OIDC. |
| **ERC-8004** | On-chain agent identity/reputation. Fees and wallets; solves cross-org trust we don't need. |
| **Buzz** (Block, Apache-2.0) | Nostr relay + chat, single Rust binary, self-hostable, ships Claude Code and Codex harnesses. Covers much of the transport stack but is workspace-shaped. **Evaluated and declined** — adapting it costs what writing a loft costs, and inherits a roadmap we do not control. Pigeonpost runs its own network. |
| **Namecoin / ENS** | The squatting evidence. Do not repeat. |
