# Pigeonpost — Node Network

Status: network design and implemented selection/admission contract. Operational pool status is not
asserted here.
Opened: 2026-08-07

`capacity.md` argues *why* the network must carry its own weight. This is *how*. Operator-facing
install and packaging is in `node.md`.

## The asymmetry everything rests on

**The recipient chooses where its messages rest, not the sender.** Every agent publishes a signed loft
list at its address; a sender reads that list and deposits there.

That one rule is what makes distribution work. Load follows the recipient's choice, so it lands on
hardware belonging to whoever cares about that agent — and no central component ever assigns traffic
to anyone.

## Roles

| Role | What it does | Who runs it |
| --- | --- | --- |
| **Loft** | Stores gift-wrapped events keyed by recipient pubkey | Anyone |
| **Directory** | Signed, static list of pool lofts and their advertised capacity | Us (mirrorable, replaceable) |
| **Prober** | Checks liveness and honesty; adjusts weights | Us (flat cost) |
| **Client** | Selects lofts, publishes its loft list, sends and drains | The agent |

## Directory entry

```json
{
  "endpoint":      "https://loft.example.com",
  "pubkey":        "ed25519:...",
  "operator":      "/github/someorg",
  "capacity_gb":   200,
  "utilization":   0.42,
  "retention_days": 30,
  "policy":        { "open": true, "pow_floor": 18, "max_event_bytes": 65536 },
  "state":         "active",
  "health":        { "uptime_30d": 0.998, "probe_fail_streak": 0 },
  "sig":           "<signed by the loft key>"
}
```

`operator` is optional and names a Pigeonpost handle, but the current entry proves only that the
loft key signed that label; it does **not** prove that the handle owner authorized the loft. It is
therefore display/advisory metadata, not an attestation. Clients always treat the successfully
probed endpoint host as a failure domain. A declared operator label may collapse additional lofts
across hosts, but it never replaces the host and can never split one host into fake identities.
Handle-backed attestation requires a separate handle-key authorization and witnessed registry
resolution before clients may prefer it as verified accountability.

The directory itself is a signed static file on a CDN, mirrorable by anyone, and the client's
directory URL is configuration. Multiple directories are accepted by design; this is the same exit
right the registry has, applied to the pool.

## Directory integrity

Whoever publishes the directory steers traffic, which makes it the most centralized thing in the
design. The obvious fix is to split the signing key k-of-n, as witnesses do for the registry. That is
the wrong first move: cosigners can only attest *"this is the file that was published."* They cannot
tell whether it was composed honestly without each running their own prober, so k-of-n signing over a
file we compose still lets us compose it. It defends against substitution, not against bias.

**So don't distribute the key — remove the power the key has.** Four properties, each removing one
thing a directory operator could otherwise do:

| Power | Removed by |
| --- | --- |
| **Forge an entry** — invent a loft, or inflate someone's capacity | **Entries are signed by the loft's own key.** We compile submissions; we do not author them. An entry without the loft's signature is invalid |
| **Silently exclude** an honest loft | **The directory is an entry type in the transparency log we already run for names.** Submissions and removals are appended, public, and permanent. The registry accepts those leaves only when the exact loft-self-signed mutation also carries an origin- and operation-bound authorization from an explicitly pinned directory document key. The reference directory creates that authorization only after it has durably reserved the exact locally admissible transition, so registry publication cannot get ahead of the pending/probe gate. An operator can prove they submitted and check they are in |
| **Fudge weights** to favor chosen nodes | **Weights are computed, not decreed.** The prober's raw measurements are published and signed, and the weighting formula is public and deterministic. Anyone can recompute every weight from public data — wrong output is arithmetic anyone can catch |
| **Serve different views** to different clients | **Strict-majority (`2k > N`) cosigned tree heads**, reusing the registry's witnesses rather than recruiting a second set |

The first three are the substance; cosigning is defence in depth for the one case the others leave
open. Reusing the existing witnesses matters — a separate cosigner set recruited only for the
directory would be a second bootstrap ask for the least valuable of the four properties.
Strict-majority validation guarantees set intersection only for clients sharing that roster. It
prevents a no-gossip fork only while fewer than `2k - N` witnesses equivocate; different rosters need
a guaranteed non-equivocating overlap or external coordination.

The current release runs one supervised, bounded prober for each directory service and publishes
its signed raw measurements. A future multi-prober format could require agreement across independent
measurement signers, but clients do not claim or enforce k-of-n prober consensus today.

### Why this is enough here, when a full log was needed for names

The directory is a **hint, not authoritative data**, and that difference is what sizes the machinery:

- A name binding must be globally consistent or messages go to the wrong agent. A directory entry only
  influences which loft an agent picks for *its own* inbox
- Senders follow the recipient's published loft list, never the directory
- A bad pick is survivable — 2–3 loft redundancy, and a hostile loft cannot read messages
- The diversity constraint caps any single operator's share of one agent's list

The residual risk is concentration of metadata observation, not loss or corruption of messages. That is
worth the four properties above and not worth a second consensus system.

**Verification is the client's job.** A client that never fetches our directory, uses another one, or
pins a hand-written loft list is fully functional. Nothing in the protocol requires our directory to
exist — which is the property that makes all of the above a safety net rather than a dependency.

## Loft lifecycle

```
  submit          probe 24h clean         3 probe failures        72h degraded
pending ──────────────► active ─────────────► degraded ──────────────► removed
                          │                      │
                          │ operator sets        │ recovers
                          ▼ drain date           ▼
                       draining ──── date ──► removed          back to active
```

| State | New agents may select it? | Existing agents keep using it? |
| --- | --- | --- |
| `pending` | No | — |
| `active` | Yes | Yes |
| `degraded` | No | Yes, until they notice and replace |
| `draining` | No | Yes, until the drain date — reads keep working |
| `removed` | No | No |

`draining` is the graceful exit: the operator announces a date, stops attracting new agents, and
keeps serving reads so nothing in flight is lost.

The implemented operator path takes an exact absolute UTC instant, signs it with the existing loft
key, and posts it to the same configured directory origin used for submission:

```bash
pigeonpost loft drain \
  --dir /srv/pigeonpost \
  --directory https://directory.example \
  --endpoint https://loft.example.com \
  --after 2030-09-01T00:00:00Z
```

Submission and drain share one locally durable sequence per directory origin, exact endpoint, and
loft key. The CLI fsyncs the exact signed operation before bounded, no-proxy, no-redirect network
I/O. If the result is ambiguous, the same command resends the same signed object; a different
operation or deadline cannot consume that pending sequence. The operator keeps the loft serving
reads through the announced deadline and backs up `.pigeonpost-directory-mutations/` with
`loft.key`. HTTP directory targets are accepted only for exact numeric IPv4/IPv6 loopback
development origins; production uses public HTTPS. `localhost`, `ws`/`wss`, credentials, paths,
queries, and fragments are not directory origins.

Probation starts at the first successful probe, not at submission. It must remain clean for a full
24 hours; any failure restarts the interval. A registration still pending after seven days expires
to `removed`. Pending registrations stay visible in the append-only registry log but never enter the
signed routing snapshot. The snapshot is capped at 512 routable entries (and 2 MiB), while the
pending queue is capped at 4096.
Publication uses two durable local commits around the witnessed registry append. First, a full
transactional transition preflight commits one exact non-routable reservation under SQLite WAL
`synchronous=FULL`; the reservation counts against pending capacity, fences that endpoint from
probes/expiry/divergent mutations, and contains the immutable registry mutation plus exact local
request. After witnessed inclusion, one transaction consumes it while applying the projection and
advancing the checkpoint. A bounded supervisor exact-replays reservations after cancellation,
ambiguous responses, or restart, and `/ready` remains false until recovery completes. No registry
leaf is therefore created for a locally rejected transition, and no acknowledged leaf can be
silently lost from local projection.
Before any key proves control, independently signed `(endpoint, loft key)` candidates coexist; no
signature-only claim reserves the endpoint. The first candidate whose probe sees its exact key and
successfully stores/returns the canary atomically becomes the canonical binding. Competing
candidates, old measurements, health, and retention canaries are discarded, stale in-flight probes
cannot change the winner, and the winner starts the full clean probation at that first success. A
removed endpoint with a successful historical key-matching probe stays bound to that key. The same
key may submit a strictly higher signed sequence to re-enrol, but it returns to `pending` and must
complete probation again; other keys cannot evict it. All signed claim streams remain visible in
the registry's immutable history.

## Probing

The prober holds its own keypair and treats every loft as untrusted.

| Check | Cadence | Failure means |
| --- | --- | --- |
| Reachable, TLS valid, `/v1/info` key matches submission and origin matches the exact probed canonical endpoint | 5 min | Liveness, endpoint substitution, or credential-origin confusion |
| Accepts a test event, returns it immediately | 5 min | Write path broken |
| Persisted one-recipient canary is still readable through the advertised boundary | Daily | **Lying about retention** |

Canaries are checked daily, including at one day and seven days when the advertised window reaches
those ages, and one hour before the exact advertised expiry boundary; a successful boundary check
rotates the canary. Every fetch uses bounded, progress-checked pagination.

Promotion needs 24 continuous hours clean. Three consecutive failures demote to `degraded`, which
drops the weight to zero for *new* selections without disrupting agents already there. Uptime is
computed only from measurements in the exact preceding 30 days. Signed version-2 measurement pages
carry a bounded cursor, next cursor, and `more` flag so an auditor can traverse all retained evidence.
The prober leases due endpoints in fair batches, uses at most 16 concurrent probes, and has one
45-second whole-sweep deadline.

**Over-advertising capacity is self-correcting.** A loft that claims more than it has fills up, starts
rejecting writes, and gets de-weighted within one probe cycle. Capacity is a hint; observed behavior
is the truth, so nobody has to be trusted.

## Client selection

```
select_lofts(target = 3):
    keep = [own_loft] if running_own_loft else []
    keep += [l for l in current_lofts if l.state in (active, draining) and healthy(l)]

    for entry in directory.active():
        domains = { authenticated_endpoint_host(entry) }
        domains += { "claimed:" + entry.operator } if entry.operator
        exclude if domains intersects failure_domains(keep)
        exclude if entry.retention_days < my_minimum
        exclude if entry.policy.max_event_bytes < my_typical

    while len(keep) < target and candidates:
        pick = weighted_random(candidates, weight = w(entry))
        keep.append(pick)
        candidates.drop_conflicting_failure_domains(pick)

    return keep

w(entry) = capacity_gb × (1 − utilization) × health.uptime_30d
```

Three properties, each load-bearing:

- **Weighted-random, not best-first.** Deterministically picking "the best" loft stampedes whichever
  node looks best that hour, then the next one. Randomness spreads load without coordination
- **Sticky.** An agent does not churn lofts. Re-selection happens on failure, drain, or policy
  mismatch — never on a better option appearing
- **Diverse.** No two lofts in a list share a probed endpoint host or a declared operator label.
  The host is always enforced; an unverified label can only collapse more candidates. Three relays
  in one rack is correlated failure wearing a disguise

Stickiness has a consequence worth stating plainly: **relief applies to growth, not to installed
base.** As pool capacity rises, our share of *new* agents falls, so our load ratchets down over time
rather than dropping when operators join.

### Why our share shrinks automatically

Selection weight is a share of pool capacity. Ours advertises a fixed number — our budget — so as
total capacity grows, `C_ours / C_total` falls with no migration, no client update, and no decision by
anyone. That is the entire mechanism by which the network takes weight off us.

## Message paths

```
send:     resolve address → read recipient's loft list → wrap (envelope v3)
          → stamp PoW at recipient's advertised difficulty → publish to all their lofts

receive:  wake → query each of my lofts since cursor → dedupe by event id
          → advance cursor → disconnect
```

The same message arrives 2–3 times and is deduplicated. Redundancy is what makes any single loft's
failure a non-event.

The client also treats record placement as durable wake work. It commits the exact signed record or
rotation bundle and deterministic own-loft plus rendezvous targets before issuing requests, records
success per URL, and resumes unfinished targets after restart. Send/flush/drain perform this repair
only after message work and within the same bounded deadline. Directory failure is exposed as
degraded placement health but does not block a cached loft drain; unchanged, completed targets are
not republished.

## Replacement and the transition window

When an agent replaces loft X with Y it republishes its loft list — but senders holding a cached list
keep depositing at X, and messages already resting there do not move.

So the client **drains both** for a transition window of `min(X.retention_days, 30)` before dropping
X. Lofts never forward messages to each other: forwarding would require lofts to hold per-recipient
routing state, and "lofts are dumb storage" is what keeps them cheap enough to donate.

## Admission policy — decided

**Open admission.** Any loft may join the directory; there is no approval step.

The reasoning: gating admission makes us a gatekeeper, which is requirement 6 all over again, and
encryption already bounds what a bad node can do. A hostile loft **cannot read messages**. It can drop
messages — caught by agents periodically fetching back what they published, and survived through 2–3
redundancy. It can observe metadata, which probing cannot fix and admission control would not fix
either, since a patient attacker passes review.

Quality is handled after the fact by probing and client-side de-weighting rather than before the fact
by us saying yes.

## Threat model

| Attack | Bounded by |
| --- | --- |
| Read messages | Impossible — gift-wrapped envelope |
| Drop or withhold messages | Self-probing detects it; 2–3 loft redundancy survives it |
| Lie about capacity | Fills up, rejects writes, de-weighted within a probe cycle |
| Lie about retention | Prober reads back test events at retention age |
| **Harvest metadata** | **Not solved.** Mitigated by recipient-hosting, operator diversity, and randomized timestamps. The residual risk of using someone else's loft |
| Sybil the pool to attract traffic | Host-based diversity prevents one endpoint host from occupying multiple slots; self-declared operator labels can only collapse candidates, not prove common ownership. Multiple independently hosted identities remain a residual Sybil risk bounded by capacity cost, random selection, redundancy, and the optional directory exit right |
| Poison the directory | Entries self-signed by lofts; submissions and removals appended to the transparency log; weights recomputable from published probe data; tree heads cosigned by the registry's witnesses. The directory URL is client configuration and the whole thing is optional |

## v0.2 decisions

1. **Utilization is observed through the probe path.** The reference loft computes
   `min(used_bytes / capacity_bytes, 1)` from its storage counters and exposes it at `/v1/info`.
   The prober reads that value only from the exact endpoint whose canonical origin and loft key it
   has verified, then records it in the signed measurement stream. A malformed, non-finite, or
   out-of-range value fails closed. It is not copied from the operator's directory submission.
2. **Seven days is the reference client's minimum retention.** `SelectionCriteria::default()`
   excludes a new pool candidate below seven advertised days. An embedder may deliberately choose
   a stricter policy; seven days is the interoperable default, not a promise by every loft.
3. **An operator label is collapse-only metadata.** The probed endpoint host is always a failure
   domain. A self-declared label may collapse more candidates, but cannot split one host, attest
   handle ownership, or earn a preference. A future handle-key authorization needs its own
   versioned protocol and witnessed verification before that can change.
4. **One supervised prober runs with each directory service.** It is bounded and its signed raw
   measurements are public. v0.2 neither claims nor enforces agreement among several independent
   probers; adding a measurement quorum is a future protocol change.
