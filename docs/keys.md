# Pigeonpost — Key Lifecycle

Status: implemented key-custody and rotation contract.
Opened: 2026-08-07

A key address is `base32(SHA-256(pubkey))[:26]` — the address **is** the key. That is what makes it
free, instant, and unsquattable, and it is also the problem: rotating the key produces a different
address, and every README pointing at the old one breaks. There is no registry entry to update.

This document is the answer. The short version: **commit to your successor before you need it.**

## The three cases

| Case | Old key usable? | Attacker holds it? |
| --- | --- | --- |
| **Planned rotation** — hygiene, algorithm upgrade, moving hosts | Yes | No |
| **Compromise** — the key leaked | Yes | Yes |
| **Loss** — disk died, no backup | No | No |

Any scheme that only handles the first case is not worth building. The design below handles the first
two and bounds the damage of the third.

## Rejected: bare forwarding records

The obvious design is a "change of address" record: the old key signs `{from, to}`, clients follow it.

It fails exactly when it matters. An attacker holding the old key can sign that record too — and can
sign it **first**, forwarding the address to themselves permanently. A recovery mechanism that hands
the address to whoever compromised it is worse than no mechanism, because people would rely on it.

## Adopted: pre-committed successor

At creation, an agent generates **two** keypairs: the operating key `K1` and a successor `K2` kept
somewhere else (offline, another host, a hardware token). Its signed agent record carries a
commitment to the successor from day one:

```json
{
  "pubkey":         "ed25519:<K1>",
  "successor_hash": "sha256:<SHA-256(K2_pubkey)>",
  "seq":            0,
  "lofts":          ["https://loft.example"],
  "sig":            "<signed by K1>"
}
```

Rotation reveals the successor. The outgoing key proves continuity and the incoming, precommitted
key co-signs the complete transition, including its own next commitment and the drain window:

```json
{
  "rotates":        "/k/j5pxq82nf4wt3h9m6rbdck0syv",
  "to_pubkey":      "ed25519:<K2>",
  "successor_hash": "sha256:<SHA-256(K3_pubkey)>",
  "seq":            1,
  "activated_at":   1786105721,
  "grace_until":    1793881721,
  "outgoing_sig":   "<signed by K1>",
  "incoming_sig":   "<signed by K2>"
}
```

A client accepts it only if `SHA-256(to_pubkey) == successor_hash` from the record it already holds.
Both signatures cover the same versioned canonical fields under separate signature domains. The K2
signature is essential: without it, an attacker holding K1 could force the intended K1→K2 transition
while replacing K2's next commitment with an attacker key. The new record commits to `K3`, and the
chain continues — every rotation sets up the next one.

### Why this survives compromise

An attacker holding `K1` can sign a rotation, but the only destination that verifies is the key that
was committed to in advance — which they do not have — and K2 must co-sign its next commitment.
**They cannot steal the address or poison the following transition.** The worst they can do alone is
make noise and continue acting as K1 until the owner completes the prepared rotation.

They can still read messages sent to `K1` until the owner rotates, and impersonate the agent as a sender
in the meantime. Compromise is not survivable *silently* — it is survivable without losing the
address, which is the property publishable addresses need.

### Why this survives loss

`K2` is a backup by construction. Keeping it on the same disk as `K1` defeats the point; the client
should say so at generation time rather than assume anyone read this file.

### Recovery directory contract

The v0.2 portable custody format is a stored `successor.key`, not a seed phrase. Before the first
operation that opens an agent, an operator may place successor material behind an explicit custody
boundary:

- library embedders pass `AgentOpenOptions { recovery_dir: Some(...) }` to
  `Agent::open_with_options`;
- the CLI and its MCP server use the global `--recovery-dir` option or
  `PIGEONPOST_RECOVERY_DIR`; and
- the selected directory must already exist as a canonical, absolute, non-root, owner-only
  directory. Pigeonpost does not create it, relax its permissions, or follow symbolic-link path
  components.

The directory holds `successor.key` and any staged `next-successor.key`. It must be mounted and
available every time the agent is opened, including every MCP tool execution; it is active custody,
not a cold archive that can disappear after creation. The operating key, token secret, SQLite
state, rotation journal, lock, and retired keys remain under the agent home.

For backward compatibility, omitting the option keeps the successor at
`<home>/recovery/successor.key`. First-run callers warn when that file shares a storage device with
`identity.key`. That layout works, but it does not protect the address from loss of that device.

Do not move a committed successor casually after creating an identity. To migrate an existing
default layout, stop every process that can open the agent home, make a verified backup, establish
the external owner-only directory, durably move the exact committed `successor.key`, and ensure no
conflicting default or legacy copy remains. Reopen every CLI, MCP, or library integration with the
same recovery directory before resuming work. A missing committed key, a conflicting default copy,
or a different recovery path makes agent open fail closed; never respond by minting a replacement.

### Operating the prepared rotation

Inspect and record the active key address, ensure the configured recovery directory and current
lofts are available, then authorize exactly that predecessor:

```bash
pigeonpost id
pigeonpost rotate --confirm /k/<current-address>
```

The client first persists one exact plan containing the source record, target record, dual-signed
rotation, deterministic publication targets, and staged next successor. It then publishes the source
side, promotes the already committed successor without changing the token secret, and publishes the
target side. Each exact target acknowledgement is durable. Cancellation, a process crash, or an
ambiguous network result leaves that same plan resumable; rerun the identical command and
confirmation rather than constructing a new rotation. The command refuses a different predecessor,
an uncommitted successor, unsafe/missing custody, or the 32-live-retired-identity ceiling before key
promotion.

Local key rotation is intentionally not a model-callable MCP tool because it changes primary key
custody and the agent address. `pigeonpost_rotate_handle` changes only the registry alias after fresh
provider proof; it does not perform this local cryptographic transition.

### The residual hole

**Lose both `K1` and `K2` and the address is gone permanently.** No recovery, by design — anything
that could restore it without a key is a mechanism an attacker could use too.

Two things make this acceptable rather than alarming: most agents are ephemeral and were never meant
to be durable, and anything that genuinely must outlive its keys should hold a handle, where the
registry can rebind `name → new pubkey`. That is the strongest practical argument for claiming one.

## Anti-rollback and anti-equivocation

**Sequence numbers.** Every transition advances `seq` by exactly one. Clients never accept an equal,
lower, or skipped sequence, so an old record cannot be replayed and a chain link cannot be hidden.

**Pin the first commitment (TOFU).** An attacker holding `K1` could republish the *original* record
with a successor hash of their own choosing, and a client that never saw the real one would believe
it. So clients pin `successor_hash` the first time they resolve an address and treat a later change
to it as an attack, not an update — surfaced to the operator, never silently accepted.

**Optional log anchoring.** An agent that wants better than trust-on-first-use can anchor its initial
commitment in the transparency log: one tiny entry, no OIDC, no name claimed. Off by default, because
requiring it would drag every key address back into the registry and cost the tier its independence.

## Grace period

Rotation is not instant across a network of agents that wake up weeks apart.

- The retired private key remains usable for the record's signed **90-day** dual-address window,
  and Pigeonposts to the old address remain normal during that window on every eligible route
- Route eligibility is independent of key custody: an active loft remains eligible, while a removed
  loft is retained only for `min(its authenticated advertised retention, 30 days)`. The client
  intersects every retired identity's historical loft list with those active/unexpired routes, so a
  90-day key cannot resurrect an expired endpoint or its cursor
- Senders update their cached resolution on first sight of a valid rotation record
- After the window, the old address is dropped and messages to it are undeliverable — not forwarded
  forever, because an indefinite forwarding table is state we said lofts would not keep
- The old private key is retained at `0600` only through that window. Cursor state is keyed by both
  loft and address so draining K1 can never advance or rewind K2's position
- At most 32 retired identities may be live at once, matching the maximum verified rotation-chain
  depth and bounding every open/drain scan; another rotation fails before promotion at that ceiling

## Handles

Handles do not have this problem. The registry rebinds `name → pubkey`, so a handle survives any
number of key rotations and is recoverable after total key loss.

- Rotation is a logged operation requiring a fresh provider proof—the same gate as registration
- It appends to the log rather than mutating it, so the binding history stays publicly auditable
- Recovery after losing every key means re-proving the provider identity. The implemented handle is
  anchored to a GitHub or Google account, and that upstream account is the backup

Use the explicit rebind surface after either a planned key change or total local key loss:

```bash
pigeonpost registry-trust import --file registry-trust.json
pigeonpost handle rotate /github/yourname --registry https://registry.example
```

Agent frameworks use the equivalent two-phase `pigeonpost_rotate_handle` MCP tool. Both surfaces
wait until the exact `handle_rotate` leaf from the receipt is included under a fresh witnessed head.
A strictly older witnessed binding means publication is still pending and is polled within the
bounded deadline; a same-index or newer mismatch is terminal. A completely fresh home may use this
flow because recovery depends on a new key plus fresh provider proof, not possession of the lost
key. Only future handle routing is restored: the old address, old local state, and ciphertext for the
lost key remain unrecoverable.

The trade is explicit: key addresses need nobody's permission and cannot be recovered; handles depend
on an identity provider and can be. Agents that need both run both, since a handle is an alias onto a
key address rather than a replacement for it.

One account yields **one** handle: names do not subdivide, and registration requires the proved
subject to equal the handle name. So in a fleet of agents exactly one can be handle-recoverable and
the rest cannot — which is what makes moving `K2` off the machine a per-agent duty rather than a
nicety. See "Fleet layout" in `integration.md`.

## What clients must implement

1. Generate `K1` **and** `K2` at first run; accept an explicit stable recovery directory before
   creation, and warn if the resulting files land on the same storage device
2. Publish `successor_hash` in the initial record — an agent that skips this can never rotate
3. Pin `successor_hash` and `seq` per resolved address; treat changes to a pinned commitment as
   hostile
4. Verify `SHA-256(to_pubkey) == successor_hash`, both key signatures, exact sequence, activation,
   and the signed 90-day grace interval before following any rotation
5. Promote K2 crash-safely, preserve the independent token secret, retain K1 only through grace,
   and drain old and new addresses with separate `(loft, address)` cursors
6. Re-commit to a fresh successor on every rotation, or the chain ends at one

## v0.2 decisions

1. **The successor is a separately stored key file.** Seed-phrase derivation is not part of this
   release: it would couple recovery to one high-value secret and enlarge the loss/compromise blast
   radius. The explicit recovery directory is the supported portable custody boundary.
2. **Bare key-address anchoring stays optional and off by default.** A caller may choose an
   independently verified transparency-log anchor, but ordinary key addresses remain registry-free.
   Public use does not silently enroll an address in the handle registry.
