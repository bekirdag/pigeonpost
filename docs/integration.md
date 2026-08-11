# Pigeonpost — Integration Surface

Status: implemented integration contract. What a third-party tool actually calls, and what it must
never have to implement.
Opened: 2026-08-07

Success criterion from `product.md`: *a third-party tool integrates Pigeonpost without our
involvement.* That requires a surface to integrate against. This is it.

## The governing rule

**Nobody implements the protocol to send a message.** Gift wrapping, key derivation, successor
commitments, PoW stamps, cursor management, spam scoring — all of it lives behind the client library.
A RAG tool author should be able to add agent messaging in an afternoon without reading `keys.md`.

The wire format stays documented well enough for a clean-room implementation (day-one commitment #5
in `infrastructure.md`), but needing it should be the exception, not the integration path.

## Three levels

| Level | For | Effort |
| --- | --- | --- |
| **MCP server** | Agents and agent frameworks — the primary path | Point your client at it; no code |
| **CLI** | Scripts, CI, any language, quick experiments | Shell out |
| **Library** | Rust tools embedding messaging in their own process | Use a workspace, Git, or vendored source dependency |

All three are the same core; the MCP server and CLI are thin shells over the library.

The same binary is also the node server — `pigeonpost install` turns the host into a loft. One
install covers sending, receiving, and hosting, which is what makes operator-by-integration realistic
rather than aspirational. See `node.md`.

### MCP server (primary)

This is the surface a future Docdex integration would use, and how most agent tools can integrate.
Tools exposed:

| Tool | Purpose |
| --- | --- |
| `pigeonpost_identity` | Get this agent's address, creating it on first call; report queued and terminal outbox counts |
| `pigeonpost_resolve` | Handle or key address → pubkey, lofts, and recipient-signed exact attribution requirement; resolving does not consent |
| `pigeonpost_send` | Pigeonpost to an address, optionally with a call-local exact attribution agreement; report message-scoped delivered, queued, terminal, and deadline state |
| `pigeonpost_inbox` | Drain within one bounded wake-up, then list accepted or pending messages |
| `pigeonpost_storage_status` / `pigeonpost_set_storage_limits` | Inspect exact local usage and atomically replace the four bounded inbox/outbox limits |
| `pigeonpost_list_pending_deliveries` | Inspect payload-free metadata for copies still owed to lofts |
| `pigeonpost_list_completed_deliveries` / `pigeonpost_list_dead_letters` | Inspect payload-free successful or terminal copy metadata |
| `pigeonpost_delete_completed_delivery` / `pigeonpost_delete_dead_letter` | Remove one exact finished metadata row |
| `pigeonpost_delete_pending_delivery` | Explicitly abandon one exact undelivered copy with the required confirmation phrase |
| `pigeonpost_delete_message` | Erase one received Pigeonpost while retaining its permanent id-only replay tombstone |
| `pigeonpost_prune_finished_deliveries` | Remove one confirmed, age- and count-bounded batch of finished delivery metadata |
| `pigeonpost_remove_directory` | Explicitly remove one exact trusted-directory pin and cached snapshot |
| `pigeonpost_read` | Read a message body — returns an untrusted envelope, never a bare string |
| `pigeonpost_ack` | Mark one already-stored message read; drain advances the loft cursor |
| `pigeonpost_allow` / `pigeonpost_block` | Manage the allowlist |
| `pigeonpost_mark_spam` | Decrement the sender's local score |
| `pigeonpost_token_mint` / `pigeonpost_token_revoke` | Manage open-inbox capability tokens |
| `pigeonpost_attribution_status` | Read the exact recipient requirement and persistent sender agreement |
| `pigeonpost_attribution_recipient` | Select an exact recipient jurisdiction plus stable custody authority, or restore `off` |
| `pigeonpost_attribution_sender` | Select an exact persistent sender jurisdiction plus stable custody authority, or restore privacy-first `off` |
| `pigeonpost_registry_trust_status` | Inspect the exact public trust anchors and accepted witnessed checkpoint |
| `pigeonpost_registry_trust_reset` | Explicitly reset trust and all state learned through that registry |
| `pigeonpost_register_handle` | Claim a human-readable handle through a challenge-bound provider flow |
| `pigeonpost_rotate_handle` | Rebind an existing handle to this agent's current key after fresh provider proof |

The advertised list is generated from the same definitions used for runtime dispatch, so additions
do not rely on a separately maintained total. An agent that only sends and drains needs identity,
send, inbox, and read/ack operations.

Registry trust provisioning is deliberately absent from the model-callable MCP surface. An operator
must import the authenticated bundle through the CLI or an embedding's explicit provisioning path;
MCP may inspect or explicitly reset the resulting public state. The default MCP tool budget is 130
seconds: the registry client's complete 120-second witnessed audit plus 10 seconds of completion
headroom. Timeout and cancellation signal the worker and join it before a timeout response is sent or
a canceled response is suppressed, so a reported failure cannot leave a detached mutation to commit
later.

### CLI

```bash
pigeonpost id                                  # print this agent's address
pigeonpost send /github/wodo --body -              # body from stdin
pigeonpost inbox --json
pigeonpost flush --json                       # retry transient copies; show terminal copies
pigeonpost read <msg-id> --json
pigeonpost rotate --confirm /k/<current-address>
pigeonpost storage status --json
pigeonpost storage delete-message <msg-id> --confirm <msg-id>
pigeonpost storage prune-finished --before <unix-seconds> --limit 100 \
  --confirm prune-finished-pigeonpost-metadata
pigeonpost directory remove https://directory.example \
  --confirm https://directory.example
pigeonpost spam <msg-id>                       # drop the sender's score
pigeonpost token mint readme                   # → /k/j5pxq…#t=readme
pigeonpost attribution status --json
pigeonpost attribution recipient eu --authority <64-lowercase-hex>
pigeonpost attribution sender eu --authority <64-lowercase-hex>
pigeonpost attribution sender off              # restore privacy-first sending
pigeonpost send /k/j5pxq… --body "scope-pinned" \
  --attribution-jurisdiction eu --attribution-authority <64-lowercase-hex>
pigeonpost registry-trust import --file trust.json
pigeonpost registry-trust status --json
pigeonpost handle claim /github/yourname --registry https://registry.example
pigeonpost handle rotate /github/yourname --registry https://registry.example
pigeonpost registry-trust reset --confirm reset-registry-trust
```

Language-agnostic and scriptable. JSON output on everything that returns data.
`handle claim` reports `entry_kind=handle_bind`; `handle rotate` reports
`entry_kind=handle_rotate`. Both wait for the exact receipt leaf under a fresh witnessed head. A
strictly older binding is publication lag and is retried; a same-index or newer mismatch fails
closed. Rotation restores future handle routing after total key loss, but not the retired address,
local state, or Pigeonposts encrypted to the lost key.

`pigeonpost rotate` is the explicit local-identity transition: it requires the exact current (or
journaled predecessor) key address as confirmation, resumes the same durable plan after interruption,
and never mints a competing transition. It is deliberately operator-only rather than model-callable
MCP because it changes key custody and the agent's primary address. Handle rotation is a different
operation: it rebinds a public alias after fresh provider proof and is exposed through both CLI and
MCP.

### Library

The Rust client library is what the CLI and MCP server are built from, so there is one implementation
of the hard parts and three ways to reach it. Other languages use the MCP or CLI contract; native
Node and Python bindings are not part of the current SDS release claim. The Rust crates are internal
workspace components with `publish = false`, not crates.io artifacts; the stable public distribution
surface is the npm launcher plus its provenance-verified release binary.

## Attribution and witnessed registry trust

The recipient owns the custody scope. `attribution recipient <jurisdiction> --authority
<64-lowercase-hex>` selects an exact jurisdiction plus stable 32-byte custodian identifier. The
client first requires a fresh witnessed `Active` key in that scope, then publishes RecipientPolicy
v3 to every active or unexpired-draining Loft and advertises the same value in recipient-signed
AgentRecord v2. `off` permits an absent block again; it never converts an invalid block into a valid
one. Received attribution remains the explicit `Absent`, `Invalid`, or `Valid` enum.

A sender must explicitly agree to the recipient-advertised exact scope. A persistent default is set
with `attribution sender <jurisdiction> --authority <64-lowercase-hex>`, while CLI send flags and the
optional MCP `attribution_jurisdiction`/`attribution_authority` pair provide a call-local agreement.
The call-local form does not mutate shared sender state, so concurrent Pigeonposts to different
custodians cannot race. Resolution returns the signed requirement for inspection but is not consent.
Missing agreement, a jurisdiction or authority mismatch, an all-zero authority, or no fresh
witnessed matching `Active` key fails before the wrap is built. A sender may still volunteer an
exact scope when the recipient permits omission.

New attributed Loft admission accepts only an `Active` key whose typed id exactly matches the
recipient policy. `Retired` keys remain usable only to verify or disclose already-created wraps;
`Revoked` keys are never usable. If an immutable outbox wrap waits until its key retires before first
admission, the Loft refusal is terminal: the client erases the ciphertext, keeps bounded dead-letter
metadata, and requires a new explicit send under the current active key rather than silently
rewriting the signed wrap. Resolver/readiness failure or a key absent from the Loft's current
witnessed prefix returns HTTP 503 and retains the queued wrap for bounded retry; wrong scope,
malformed cryptography, or a witnessed non-`Active` admission state returns HTTP 400.

On receive, a required same-scope key absent from the agent's current witnessed prefix is also
retryable trust uncertainty, because a later Registry append may contain it. The client leaves only
that Loft cursor unchanged, reports that route failed for the wake, and continues draining other
Lofts until a consistency-verified refresh can decide the message. An explicit `Revoked` state is
invalid and advances normally; optional unresolved attribution is `Invalid` and does not pin
unattributed traffic behind it. Scope-changing commands share the drain's cross-process identity
lease, so a concurrent process fails busy before mutation and can retry after the wake.

Changing a recipient scope is intentionally immediate for unread fetched wraps. During a partial
multi-Loft update, the old AgentRecord remains published, and the recipient rechecks every fetched
block against its current local scope. Old-scope, missing, or mismatched blocks are dropped; drain
before changing scope if delayed old-scope traffic must still be accepted. Authenticated legacy
policies and records remain readable, but a boolean-only required policy or jurisdiction-only sender
setting is not assigned an authority and fails closed until explicitly reconfigured.

Registry trust is bootstrap input, not data learned from the registry being trusted. An operator
imports one complete, strict JSON bundle containing:

- bundle version, registry HTTPS origin, and expected checkpoint origin
- the pinned checkpoint public key
- a non-empty, independent witness roster and strict-majority threshold (`2k > N`)
- a minimum checkpoint size and root
- maximum cosignature age and future-clock-skew bounds

The bundle contains public anchors only. Keys and roots are exact lowercase hexadecimal values, and
unknown fields, duplicate witnesses, unsafe URLs, incoherent checkpoints, or out-of-range freshness
settings are rejected. Plain HTTP is accepted only for exact numeric IPv4 or IPv6 loopback hosts
for local testing; lexical hostnames such as `localhost` are rejected. The import is bounded to
64 KiB. Public registry DNS is resolved through a dedicated no-proxy transport: every answer must be
public, the validated answer set is pinned for the client lifetime, and redirects are never followed.
Exact numeric loopback is authorized only by supplying the independently provisioned trust bundle.

Strict majority guarantees quorum-set intersection only for one shared roster. It does not make the
overlap honest: no-gossip split-view prevention requires fewer equivocating witnesses than the
minimum intersection `2k - N`. If clients use different witness rosters, every accepted cross-roster
quorum pair needs a guaranteed non-equivocating overlap, or deployments need gossip/out-of-band
coordination. Use N-of-N when the only assumption is that at least one configured witness is honest.

The first valid import is durable and idempotent. A different bundle cannot silently replace it.
Changing trust requires `registry-trust reset --confirm reset-registry-trust`, which also deletes
cached handle and compliance-key projections, accepted checkpoint state, and their audit material.
`registry-trust status` prints only public anchors and checkpoint state, so it is safe to retain as
operational evidence. Trust anchors still need to reach the operator through an authenticated
channel independent of the registry deployment.

Embedders use the corresponding `Agent` methods:
`set_attribution_requirement`, `set_sender_attribution_requirement`,
`send_with_attribution_agreement`, `import_registry_trust`, `registry_trust_status`, and
`reset_registry_trust`. `AttributionRequirement` is the versioned exact
`{ jurisdiction, authority[32] }` value. MCP resolve exposes the recipient-signed requirement, MCP
send accepts an optional call-local exact pair, and the attribution tools manage exact settings
through closed, bounded schemas; trust import remains operator-only.

## The untrusted envelope is part of the API

A message body arrives from another LLM. It is data, never instruction — and an API that returns
bodies as plain strings makes the wrong thing the easy thing.

So `read` never returns a bare string. It returns a structure that carries the body alongside its
provenance and forces the caller to acknowledge what it is:

```json
{
  "from":        "/k/j5pxq82nf4wt3h9m6rbdck0syv",
  "from_handle": null,
  "trust":       { "allowlisted": false, "score": 0, "tier": "key_address" },
  "received_at": 1786105721119,
  "untrusted_body": "…"
}
```

The field is named `untrusted_body` deliberately: any prompt assembled from it carries the word into
the context, and any code reading it says so at the call site. Bindings wrap it in a type whose
formatting includes the untrusted marker, so the marker cannot be dropped by accident.

The reference policy reports inbound requests to a human rather than acting on them. Tools built
on this surface are free to do otherwise, but they have to choose it explicitly — the default cannot
be an agent that executes an inbound Pigeonpost.

## Local state

The library owns a SQLite database per agent: cursor, outbox, allowlist, sender scores, live tokens,
and the resolution cache with pinned successor commitments. Private keys are separate owner-only
files rather than SQLite rows: the operating identity stays under the agent home, while the
successor may use the explicit recovery-directory boundary below.

- **No agent-side daemon.** State is a file the library opens; there is no agent service to run,
  matching "agents wake, drain, disconnect." Hosting a loft remains a separate service
- **The outbox is durable.** Pigeonpost while offline and flush on the next wake. Transport errors,
  HTTP 408/409/425/429, and HTTP 5xx responses retry with bounded backoff and do not expire merely
  because an agent stays offline. Other HTTP 4xx responses and deterministic protocol/configuration
  failures enter durable terminal state instead of retrying forever. `pigeonpost flush --json`
  reports bounded reason codes and never persists a loft response body.
- **Local payload storage is finite and exact.** Schema 13 accounts inbox rows/body bytes and outbox
  rows/wrap-plus-token bytes transactionally. Operators may inspect or atomically replace all four
  limits within fixed hard maxima, but cannot lower a limit below live usage. Successful and
  terminal copies immediately erase wrap/token payload and retain bounded delivery metadata only.
  Wakes prune old successful metadata in bounded batches; terminal debt and undelivered copies need
  explicit operator deletion.
- **Received-message deletion preserves replay safety.** Schema 14 erases plaintext, sender,
  timestamp, attribution, and the matching spam mark, then keeps only the message id and deletion
  time as an indefinite tombstone. Tombstones never consume the active inbox quota, are never
  automatically evicted, and have a separate one-million-entry hard ceiling; deletion fails before
  erasure when that ceiling is full.
- **Loft removal has a bounded authenticated drain.** The client persists the retention advertised
  by the validated, identity-bound `/v1/info` response. Removal stops advertising the route
  immediately, then drains it for the smaller of that value and 30 days. Expiry permanently deletes
  its cursor and local-network authorization. A retired key's 90-day signed custody window cannot
  resurrect a route outside the current active/unexpired-draining set.
- **A wake-up is bounded.** Flush and drain use bounded network concurrency and one wall-clock
  deadline for the whole operation. Deadline cancellation drops all in-flight client futures before
  returning; queued work remains durable for the next wake.
- **Placement repair is durable and delivery-first.** The exact signed current record and rotation
  bundles, their deterministic own-loft/rendezvous targets, and per-target acknowledgements are
  committed before publication. Ordinary send/flush/drain wakes spend any remaining deadline on
  repair after message work, so an unavailable directory cannot block cached-route delivery. Call
  `Agent::maintain_placement()` for an explicit bounded repair wake and
  `Agent::placement_status()` for network-free pending/degraded counters. A restart retries the same
  bytes and skips already completed targets.
- **The cursor is client-side.** Lofts store no per-client state
- Concurrent access from several processes on one machine is a real case (an IDE and a CLI). SQLite
  serializes individual state transactions, while the active-identity lease serializes operations
  whose signed state spans storage and network I/O. Those mechanisms protect integrity, not semantic
  ownership of work — see the drain-owner rule below

## Fleet layout: one repo, one agent, one inbox

The usual shape is many agents on one developer's machine, one per repository. The two-tier namespace
is what makes that free: minting a key address costs a keypair and touches no registry, so a fleet
needs no registration at all.

`--home` / `PIGEONPOST_HOME` is global on every command. Give any address meant to survive a disk
loss its own stable recovery directory **before** the first `id`:

```bash
export PIGEONPOST_HOME="$PWD/.pigeonpost"   # per repo
mkdir -p "$HOME/.pigeonpost-recovery/my-project"
chmod 700 "$HOME/.pigeonpost-recovery" "$HOME/.pigeonpost-recovery/my-project"
export PIGEONPOST_RECOVERY_DIR="$(cd "$HOME/.pigeonpost-recovery/my-project" && pwd -P)"
pigeonpost id       # identity.key + state.db in home; successor.key in the recovery directory
```

| | Home | Address | Role |
|---|---|---|---|
| Front door | `~/.pigeonpost` | `/github/<login>` | Reachable by humans and strangers |
| Repo agent | `<repo>/.pigeonpost` | `/k/…` | The actual work; one per repo |

**Handles do not subdivide.** `/github/name/repo` is not expressible — `validate_name` rejects `/`, and
registration additionally requires the proved subject to equal the handle name. One provider account
yields exactly one handle bound to exactly one key. A handle is a front door, not an addressing
scheme for a fleet; agents address each other by key address.

Keeping secrets out of repositories entirely is the stricter variant: point `PIGEONPOST_HOME` at
`~/.pigeonpost/<repo-name>`. Same model, no chance of committing a key, at the cost of the repo no
longer being self-contained.

### One semantic drain owner per inbox

Several processes may *send* from one home safely, but sending is not read-only: resolution,
outbox, delivery, allowlist, score, and placement state all change through SQLite transactions and,
where a signed operation crosses network I/O, the active-identity lease.

Loft fetch is server-side stateless and idempotent. The cursor belongs to the client and is stored
per loft in `state.db`; fetching a page does not consume or hide its events at the Loft. A complete
`Agent::drain` holds the cross-process active-identity lease, so another cooperating process fails
fast instead of entering the same drain concurrently. Preserve that serialization: concurrent
drain implementations that bypass it can fetch and decrypt the same page, duplicate work, and blur
which process owns downstream handling even though message-id deduplication and monotonic cursor
updates protect the database itself.

Give each inbox one semantic drain owner, or serialize complete drains through an equivalent shared
guard. Where a shared front door feeds a fleet, that owner dispatches work onward to the relevant
repo agent's key address rather than letting every agent poll the same inbox.

### Two things that bite otherwise

- **`.gitignore` the state directory** before the first `pigeonpost id`, not after. `identity.key` is
  a raw private key at `0600`, and a repo-local home puts it one `git add -A` away from being public.
- **Only the handle is recoverable without a key.** A handle can be rebound through the identity
  provider, and the registry appends a rotation entry. A key address cannot: lose both
  `identity.key` and its committed `successor.key` and the address is gone permanently, by design.
  Since one account yields one handle, configure independent successor custody before creating any
  other agent whose address you intend to publish.

### Stable successor custody

The supported portable v0.2 interface is `Agent::open_with_options(home, AgentOpenOptions {
recovery_dir: Some(path) })`. The CLI exposes the same option globally as `--recovery-dir` and
`PIGEONPOST_RECOVERY_DIR`; `pigeonpost mcp` retains it and reuses it whenever a tool reopens the
agent. MCP host configuration therefore needs both `PIGEONPOST_HOME` and
`PIGEONPOST_RECOVERY_DIR` when external custody is selected.

The recovery directory must already exist as a canonical absolute owner-only directory, and it
must remain available at every agent open. Pigeonpost writes the committed `successor.key` and its
staged replacement there; it never creates or silently repairs an external custody directory.
Omitting the option remains backward-compatible and uses `<home>/recovery`, but first run warns when
that shares a storage device with `identity.key`.

The path is part of the agent's operating configuration, not a location to change casually. For an
existing identity, stop every process that can read the home, make a verified backup, durably move
the exact committed key into the prepared directory, remove the conflicting default copy, and then
reopen every integration with the same recovery setting. Missing or conflicting key material fails
closed. See `keys.md` for the complete contract.

## Key custody

A tool acting for an agent needs that agent's key. Options, in order of preference:

1. **OS keychain** — preferred architecture where an embedder supplies that backend
2. **Owner-only file** — the implemented v0.2 backend (`0600` through Unix custody on Linux/macOS,
   and a protected current-user DACL on Windows)
3. **Delegated** — an integration design in which the tool calls a local holder instead of reading
   the key; not a separate v0.2 public backend

The reference implementation can keep successor key `K2` behind the explicit recovery-directory
boundary. Its backward-compatible default remains below the home and therefore triggers the
same-storage warning when applicable. See `keys.md`.

## Docdex integration boundary

Docdex integration is outside this build. If its maintainer adopts this surface, IPMC can map onto it
directly:

- IPMC **seat identity** (owner + device + repo → one stable "developer of this repo" address) becomes
  a Pigeonpost keypair and therefore a key address, with no registration step
- IPMC inbox/outbox tools map to `pigeonpost_inbox` / `pigeonpost_send`
- The existing untrusted-content envelope, local SQLite outbox, and MCP tool surface carry over
- See `docdex/docs/planning/ipmc_implementation_plan.md`

Any sequencing, state migration, or acceptance test specific to Docdex belongs in that repository.
Nothing in this implementation should claim that integration has happened until its own evidence says
so.

## Stability commitments

- Tool names and CLI output shapes are versioned; breaking changes get a new major and a deprecation
  window
- `untrusted_body` never becomes a plain string, in any binding, at any version
- The wire format is documented and versioned independently of the library, so a clean-room client
  stays possible

## Future integration decisions

1. **Additional language surfaces** — whether native bindings add enough value beyond MCP and CLI
2. **Multi-agent processes** — one tool managing many agent identities at once (a fleet operator);
   whether that is one database with an agent column or one database per agent
3. **OIDC flow in a headless agent** — device-code flow works but needs a human at a browser once,
   which is fine for the handle tier and must never leak into the key-address path
