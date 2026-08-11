# Pigeonpost — Capacity and Cost Distribution

Status: design rationale, implemented v0.2 protocol, and specified capacity policy.
Opened: 2026-08-07

Pigeonpost is free and has no revenue. If our infrastructure cost is a function of adoption, success
bankrupts us and the project dies at exactly the moment it works. So the requirement is not "make it
cheap" — it is:

> **Our cost must be a number we choose, and stay there while the network grows.**

Everything below exists to make that true. The mechanism is that load lands on whoever benefits from
it, and our share is a budget we advertise rather than a residual we absorb.

## What scales, and what doesn't

### The registry has an explicit v0.2 envelope

The registry is deliberately small, but it is not infinitely flat-cost:

- **Key addresses never enter the log.** Registrations are bounded by provider-proof-gated handle
  claims, not by agent count
- **Resolution is self-verifying, therefore cacheable.** Inclusion proofs and signed agent records
  prove themselves, so any CDN or mirror can serve them without being trusted. Origin load stays flat
  across cacheable reads within the supported envelope
- Ed25519 verification is ~50 µs; even 6K resolves/sec is a fraction of one core

The v0.2 support contract targets a fresh continuous audit through **1,000,000 leaves** whose canonical
NDJSON is at most **256 MiB**, with at least **10 MiB/s** effective transfer throughput and at most
**100 ms** round-trip latency, inside one **120-second** client bootstrap budget. Clients replay
immutable exact ranges in segments of at most **8,192 leaves**, **32 MiB**, and **30 seconds**; a CDN
can cache each range by its exact ETag without being trusted. Bounded JSON pages of at most 256
leaves are the safe delivery fallback. The unscoped full dump remains the mirror and exit artifact,
not the client scalability mechanism. It runs in one isolated lane with an idle-progress timeout but
no absolute cutoff; it cannot exhaust the separately bounded exact-range lanes.

The timing target assumes the immutable range route is available. JSON fallback preserves safe
compatibility when segmented delivery is unavailable, but it is not the million-leaf performance
path and does not inherit that timing target.

Automated tests cover exact multi-segment replay, stale/failed segment fallback, final-root tamper
rejection, the maximum valid 256-leaf JSON fallback page, and the stated transfer/RTT budget
arithmetic. They are not a measured million-leaf production-hardware benchmark. The numbers above
are therefore the v0.2 protocol support contract under its stated network assumptions; release
acceptance must record a representative end-to-end million-leaf wall-clock run before describing
them as observed production throughput or a verified timing guarantee. The arithmetic reserves
82.1 seconds of the 120-second budget for local parsing, hashing, and projection work; it does not
measure that work.

This is a bounded capacity envelope, not an unlimited-growth claim. Registry disk, append work, and
fresh-client replay still grow with the log. Operating beyond any of those v0.2 bounds requires a
future authenticated snapshot/map/checkpoint design, a new measured envelope, and an explicit
compatibility decision. One small box and a CDN are the v0.2 topology, not a promise for every
possible scale.

### Lofts are the entire bill

Illustrative long-retention scenario: ~5 KB average stored wrapped event, senders publish to 2.5
lofts, and operators deliberately choose 90-day retention. The shipped/private-loft default is 30
days; retention is authenticated per loft and the table below is therefore a planning scenario, not
a default-runtime claim.

| Agents | Msgs/day | Ingest/day | Steady-state storage, network-wide |
| --- | --- | --- | --- |
| 100K | 1M | 12 GB | ~1 TB |
| 1M | 20M | 250 GB | ~22 TB |
| 10M | 500M | 6 TB | ~560 TB |

Lofts are storage- and bandwidth-bound, never CPU-bound — signature and PoW checks are rounding
errors. Sustained bandwidth at the bottom row is roughly 1–1.5 Gbps network-wide.

**Why this stays tractable at all:** there is no fan-out amplification. A message goes to one
recipient × 2.5 lofts. Social networks and public-feed relays multiply by follower count; we don't.

**The cost trap:** metered egress. The 10M-agent workload is ~$4K/mo of bandwidth on a hyperscaler
and effectively flat-rate on Hetzner/OVH-class hosting. Lofts must never be deployed where egress is
billed at $0.09/GB.

## The equilibrium: recipient-hosted by default

The single most important property: **a recipient chooses where its own messages rest.** The loft list is
published by the recipient, so the natural place for an agent's inbox is hardware belonging to
whoever runs that agent.

```
Operator runs 50 agents on a box  →  run the loft on that box too
                                  →  their messages, their disk, their bandwidth
```

This inverts the cost model. Storage lands with the party that already cares about those agents, and
it is *cheaper for them* than the alternative — their agents' metadata never leaves their
infrastructure, which is a privacy argument that happens to pay our bills.

Default loft-list policy in the reference client:

| Operator situation | Loft list |
| --- | --- |
| Runs their own loft | Own loft (primary) + 1 pool loft (redundancy) |
| No infrastructure | 2–3 pool lofts, capacity-weighted |

**Operator-by-integration.** The same release contains the client and loft roles, so a tool embedding
Pigeonpost can also host its users' messages. Loft mode is never enabled from an agent-count,
capacity, public-IP, or hostname threshold. Hosting always requires an explicit operator command and
configuration; serving does not silently join a directory. Integrators deliberately becoming
operators remains the seeding mechanism without turning a client upgrade into a server rollout.

**One command to become a node.** After the matching provenance-verified release is published,
install `@bekirdag/pigeonpost@0.2.0` globally. On a supported macOS or Linux host with its user
service manager available, `pigeonpost install` then turns the current directory into a private loft
without additional flags. Other hosts use `--no-service` and an operator-chosen supervisor. If
self-hosting is a project, nobody does it and none of the above happens — see `node.md`.

## The pool and the directory

Agents whose operator has no infrastructure — the genuine long tail — draw from a **pool** of
community lofts, published as a signed **directory**: a static, mirrorable, CDN-served file listing
each loft's endpoint, advertised capacity, retention, and acceptance policy.

Clients select from the directory at random, weighted by advertised capacity. Our lofts are entries
in that list, **never a hardcoded default in the client**. As operators join, our share of default
traffic falls automatically — no migration, no client update, no charity from anyone.

**Precedent, not a dependency:** the NTP Pool — the volunteer network that keeps much of the
internet's clocks in sync — serves billions of clients from roughly 4,000 donated servers,
coordinated by a directory that hands each client a subset of the pool. Its operators own almost no
hardware. That organizational model is what we are copying; Pigeonpost has no relationship to that
project and uses none of its infrastructure.

## The dial: we advertise a budget

Our lofts publish an advertised capacity equal to **what we have decided to spend**, not what we
happen to have free. Capacity-weighted selection then sends us that share and no more.

Consequences worth being explicit about:

- Adoption growth does not increase our bill. It increases the *pool's* required capacity
- If the pool is undersupplied, clients see degraded service — slower loft acceptance, shorter
  retention, occasional rejection — and the client says plainly that the pool needs operators
- That pressure recruits operators. It does not generate an invoice we cannot pay

This is the whole design in one line: **scarcity is expressed as recruitment pressure, never as our
overdraft.**

## Keeping a useful loft cheap enough to donate

The volunteer model only works if a meaningful contribution is small:

```
10,000 agents × 10 msgs/day × 5 KB × 30-day retention ≈ 15 GB
```

A $5/mo VPS serves ten thousand agents. That is the number that makes donation plausible, and it is
worth protecting:

- **Retention is per-loft policy, advertised.** A 7-day loft is a useful loft. Clients can mix one
  long-retention loft with two short ones
- **Attachments stay out of scope.** Inline payload size is the one variable that multiplies
  everything above; a 5 MB attachment norm makes every number here 1000× worse and ends the volunteer
  model outright
- **PoW stamps** cap abuse-driven growth, so donated capacity is spent on real messages

## Integrity of a distributed pool

A loft cannot read messages — gift wrapping sees to that. A hostile or incompetent one can only drop
messages, withhold them, or observe metadata (recipient pubkey, timing, size).

- **Redundancy.** Publishing to 2–3 lofts means one bad node is survivable by default
- **Self-probing.** An agent periodically fetches back what it published to its own lofts. Cheap,
  requires no protocol support, and catches silent dropping
- **Directory probing.** We run a prober that checks liveness and honesty and de-weights bad entries.
  Candidate and published-snapshot caps bound this service; a larger pool requires more independent
  directories rather than unbounded work by one operator
- **Metadata exposure is the real risk**, not data loss — which is another argument for recipient-
  hosted inboxes, where the metadata never leaves the operator

## If nobody shows up

Honest failure mode. If adoption arrives and operators don't, we do **not** absorb it:

1. Our advertised capacity stays at budget
2. A loft rejects new writes at its configured capacity, while its configured retention sweeps
   expired events; neither value changes itself in response to demand
3. Request admission stays bounded by concurrency and per-minute request **and byte** budgets at
   three levels: global, connected source, and verified recipient. Recipient charging happens only
   after the public envelope verifies, so rotating malformed recipient bytes cannot consume a
   victim's allowance
4. The client surfaces the pool's state and points at explicit self-hosting

There is deliberately no per-address stored-byte reservation or quota in v0.2. Storage is governed
by the loft-wide capacity and retention window; abuse pressure is governed by the bounded request
and byte budgets above. Operators may change advertised capacity, retention, and admission budgets
deliberately, but the process never shortens retention or enables a new role on its own.

Messaging stays free — the *protocol* is free and always will be. Our lofts are best-effort
infrastructure that promise no capacity to anyone, the same posture every free public resolver takes.
Framing that clearly now is what stops it from reading as a bait-and-switch later.

## Day-one requirements

These are cheap now and unretrofittable once clients are deployed. They join the commitments in
`infrastructure.md`:

1. **Client loft selection is directory-driven from the first release**, even when the directory has
   two entries and both are ours. A hardcoded default cannot be undone in the field
2. **Loft mode ships in the library and installs in one command**, so self-hosting is a flag rather
   than a project (`node.md`)
3. **Advertised capacity and retention are protocol fields**, so weighting works before there is
   anything to weight
4. **Loft hosting and pool enrollment are explicit operator actions.** No client or library
   threshold may enable a listener or submit a directory mutation automatically

## v0.2 decisions

1. **Admission is open, then measured.** A loft-authenticated submission enters `pending` without
   human approval. The first candidate to prove the exact endpoint/key binding through a real
   store-and-fetch probe wins that binding; 24 continuous clean hours promote it to `active`.
2. **There is no self-hosting threshold.** Loft mode is never auto-enabled. Operators explicitly
   install or serve a loft, and explicitly submit a public endpoint after its prerequisites are met.
3. **Directory mutations share the registry transparency log.** Authenticated additions and
   removals first reserve the exact locally admissible transition durably, then must be included
   under a fresh witnessed checkpoint before the local directory applies them. Reservations that
   add pending load count against the same 4,096 budget and are non-routable until transactional
   finalization. The signed routing snapshot remains the efficient client distribution form.
4. **Fair use combines capacity, retention, and bounded admission.** Lofts enforce configured
   capacity and retention plus global, connected-source, and verified-recipient request/byte
   budgets. v0.2 does not maintain a per-address stored-byte quota.
