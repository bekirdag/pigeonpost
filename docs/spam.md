# Pigeonpost — Spam Control

Status: implemented control model; remaining policy choices are listed at the end.
Opened: 2026-08-07

An openly advertised, free, permissionless inbox is a spam magnet, and the GitHub-README use case
*requires* the open-inbox mode — the hardest case. Free registration of key addresses (see
`architecture.md`) means an attacker can mint identities at the cost of a hash. This needs an answer
at design time.

## The constraint that shapes everything

Messages are gift-wrapped (NIP-59 pattern; envelope v3 in `sds.md`). **The stored wrap reveals a
recipient key but not an authenticated sender key, true send timestamp, or content.** That is the
privacy property, and it is not negotiable. A required attribution block does not change this: the
loft validates only public shape and witnessed epoch data; it has no key that can decrypt the sender
claim. A regulated public loft separately observes the transport source and exact receipt time long
enough to seal them as purpose-specific trace records; a proxy/NAT address is not an authenticated
application sender and is never a server-side sender-reputation identity.

It splits the design space cleanly:

| Where a control can live | What it can see | Therefore it can use |
| --- | --- | --- |
| **Loft** (before storage) | Wrapped event; separately sealed request-source metadata where policy requires it | Proof-of-work, size, rate, recipient-supplied tokens, recipient policy |
| **Client** (after unwrap) | Real sender pubkey, content | Allowlists, sender reputation, tier, content heuristics |

Anything keyed on an authenticated sender identity is **necessarily client-side**. There is no
server-side sender reputation in this architecture, and adding one means unwrapping messages—which
we will not do—or misrepresenting transport metadata as identity.

## Options considered

| Option | Verdict |
| --- | --- |
| **Closed by default** (`acceptAll = false`) | **Adopt.** Costs nothing, solves the common case entirely. Most agents talk to a handful of known peers |
| **Capability tokens** — publish `/github/wodo#t=readme`, revoke the token if abused | **Adopt.** The recipient can enable a loft-enforced token gate without deanonymizing anyone |
| **Proof-of-work stamps** (NIP-13) on wrapped messages | **Adopt.** The loft enforces one recipient-signed flat floor on every wrap; zero disables it |
| **Local sender score + mark-as-spam** | **Adopt.** Purely client-side, no privacy cost, no coordination. This is the "drop the score" mechanism |
| **Tier gradient** — trust key addresses less than OIDC-backed handles | **Adopt client-side.** It is available only after unwrap and local identity verification |
| **Loft policy** — operators set their own acceptance rules | **Adopt.** Already implied by federation |
| **Web of trust** from public contact lists | **Partial.** Useful positive signal, but Nostr kind-3 contact lists are public and leak the social graph. Opt-in only |
| **Shared/published spam reports** | **Reject for the initial release.** Sybil-reportable (mint keys, mass-report a rival) and leaks who corresponded with whom. Revisit only tier-gated and opt-in |
| **Payment, bond, or stake** | **Reject.** Breaks "free, permanently" — a founding requirement |
| **Content filtering / LLM triage at the loft** | **Reject.** Requires unwrapping. Client-side triage is fine |
| **CAPTCHA / human challenge** | **Reject.** Senders are agents; there is no human in the loop to challenge |

## Recommended design

Five layers, cheapest first. A message must survive all of them.

```
        ┌─ 1. Loft policy ──────── rate, size, retention (operator's rules)
        ├─ 2. Token gate ───────── if enabled, every wrap needs a live token
        ├─ 3. PoW stamp ────────── flat recipient floor on every wrap; zero disables
  wrap  │                          ······ unwrap boundary ······
        ├─ 4. Allowlist ────────── acceptAll = false: known senders pass, rest queue
        └─ 5. Sender score ─────── local reputation; mark-as-spam decrements
```

### 1. Closed by default

`acceptAll = false` is the implemented default. A message from a sender not on the allowlist does not
reach the agent; it lands in a **pending** queue the operator can review. An agent that never opens
its inbox to strangers has no spam problem at all.

### 2. Capability tokens (the open-inbox answer)

Instead of publishing a bare address, publish an address plus a revocable token:

```
"Pigeonpost requests to my agent at /github/wodo#t=readme"
```

- The recipient mints tokens locally and registers a separate **presentation hash** for each loft,
  bound to both that loft's public key and exact canonical origin. A presentation collected by a
  hostile endpoint is therefore unusable at another origin even if it claims the same loft key
- When the signed token gate is enabled, a loft rejects every wrap without a presentation matching
  that list — it learns nothing about senders, only that the token is live
- Abuse of a published token is answered by revoking it and editing one line of the README
- Different tokens per surface (`#t=readme`, `#t=docs`, `#t=conf-talk`) localize the blast radius and
  reveal *which* published surface is being harvested

This gives an open inbox with a kill switch, and needs no reputation system at all.

### 3. Proof-of-work stamps

Every wrap carries a NIP-13 stamp over the wrapped event when the recipient's signed floor is
nonzero. The loft enforces that flat floor before storing. Insufficient work is a non-monetary
policy refusal (`403 Forbidden`), not a payment mechanism.

The v0.2 client supports floors from 0 through **18 bits**. It rejects a configured or advertised
higher value before encryption, reply trust, or durable queue mutation. Mining runs off the async
runtime under both an 8,000,000-attempt cap and a 10-second wall-clock/cancellation budget. A
process-wide fail-fast cap permits at most two active miners, with capacity held until cancelled CPU
work actually exits, so hostile signed records cannot monopolize MCP or wake-up workers.

The loft cannot know which sender is presenting a wrap, so neither a known sender nor a capability
token bypasses that floor. Sender-aware treatment happens after unwrap in the client. The token gate,
when enabled, is an additional requirement:

| Path | Difficulty |
| --- | --- |
| Token gate enabled, live capability | Passes the token gate; still meets the flat PoW floor |
| Token gate enabled, no live capability | Rejected before storage |
| Token gate disabled | Flat PoW floor still applies |
| Known sender | Same loft checks; allowlist treatment begins only after unwrap |
| Low local score after unwrap | Held, blocked, or dropped by that recipient's client |

Seconds of CPU for one genuine message; ruinous for a million. PoW alone is not a spam solution —
it is a rate limiter that makes the other layers affordable.

### 4. Allowlist

Sender pubkey checked against the agent's contacts after unwrap. Populated automatically: **anyone
you Pigeonpost is allowlisted for replies.** This makes the common request/response flow work with
no configuration.

### 5. Local sender score (mark-as-spam)

A per-recipient score in the agent's local SQLite. Never shared, never published, never consulted by
infrastructure.

| Signal | Effect |
| --- | --- |
| I Pigeonposted them | Strong positive — implies allowlist |
| Prior message accepted, not flagged | Positive, small |
| Holds an OIDC handle | Positive, small |
| First contact from a bare key address | Neutral, starts at zero |
| **Operator marks as spam** | **Strong negative** |
| Marked as spam by me repeatedly | Below threshold → silently dropped at unwrap |

Scores **decay toward neutral**, so a mistaken flag is not a life sentence and a compromised-then-
recovered key can come back. A low score can hold, block, or drop future messages locally; it cannot
make the sender-hidden loft apply a sender-specific PoW value.

The OIDC signal is deliberately fail-closed and offline. It is granted only when the complete local
handle projection exactly matches the sole durable registry pin and that exact checkpoint note has
a still-fresh configured strict-majority witness quorum. Witness independence and the fault bound
remain operational assumptions. Missing, lagged, or expired evidence simply withholds the
bonus; malformed state or an invalid signature is an error. The lookup happens only after unwrap,
never queries the registry for an incoming sender, and a key disappears from the tier as soon as a
witnessed handle rotation replaces it. The small bonus can rescue a borderline drop into ordinary
pending review; it never bypasses the allowlist or the explicit `acceptAll` setting.

Because the score is local, a spammer must earn its reputation separately with every victim, and
learns nothing about why it was dropped.

## What this does not solve

- **A determined attacker with one OIDC account** can send a message to every address it can
  scrape. Recipient-local mark-as-spam can reduce that key's score only for that recipient; v0.2
  has no global handle-suspension mechanism
- **First contact remains possible** — by design. A stranger still has to satisfy the recipient's
  flat PoW floor and, when the token gate is enabled, present a live capability before storage
- **No global reputation** means no herd immunity: each agent learns about each spammer
  independently. This is the deliberate price of not deanonymizing senders

## Shipped v0.2 decisions

- **PoW bounds are fixed:** advertised difficulty is accepted only from 0 through 18 bits. A local
  attempt is bounded by 8,000,000 hashes and 10 seconds, and at most two PoW miners run in one agent
  process at a time.
- **Pending state and reputation are per-agent:** each agent keeps its own queue, allowlist, and
  scores in its local SQLite state. Operators do not get an implicit fleet-wide reputation pool.
- **There is no global handle suspension in v0.2:** the small bonus follows only the exact current
  witnessed handle-to-key projection. A witnessed rotation or removal makes the old key lose that
  bonus; it does not impose a global sender penalty.
- **Delayed or shared abuse feedback is deferred:** v0.2 does not publish or consume shared reports.
  Adding that channel now would introduce a Sybil surface and sender-linkability without a settled
  privacy-preserving design.
