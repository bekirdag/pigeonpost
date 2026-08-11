# Pigeonpost — Infrastructure

Status: implemented operating model. Source code does not attest production activation; see
`handoff.md` and `deploy/README.md`.
Opened: 2026-08-07

Domain: `pigeonpost.dev`. Runtime endpoints are operator configuration and are never compiled into
the binaries.

## Topology

```mermaid
flowchart TB
    subgraph clients["Agent side — no server"]
        A1["Agent A<br/>Ed25519 key + local SQLite<br/>outbox / cursor / cache"]
        A2["Agent B<br/>wakes, drains, disconnects"]
    end

    subgraph ours["We run this — explicit v0.2 budgets"]
        REG["Registry service<br/>POST /register, GET /resolve<br/>+ append-only Merkle log"]
        DUMP["Full log exit stream + immutable ranges<br/>CDN-cacheable mirror feed"]
        WEB["Name lookup page<br/>static site"]
        OIDC["Identity-provider registrations<br/>GitHub OAuth2 / Google OIDC<br/>config only, no server"]
        DIR["Loft directory + prober<br/>signed list: capacity, retention, policy"]
        L1["Loft 1<br/>advertises budgeted capacity"]
        L2["Loft 2<br/>advertises budgeted capacity"]
    end

    subgraph others["Third parties run this — grows with adoption"]
        W1["Witness A"]
        W2["Witness B"]
        W3["Witness C"]
        LSELF["Operator's own loft<br/>hosts their own agents' messages"]
        L3["Pool loft"]
        L4["Pool loft"]
        MIR["Registry mirror"]
    end

    A1 -->|"register: handle + pubkey + provider proof"| REG
    A1 -->|"resolve /github/superaidev"| REG
    A1 -.->|"pick lofts, capacity-weighted"| DIR
    A1 -->|"publish wrapped msg to 2–3 lofts"| LSELF
    A1 --> L3
    A1 --> L1
    A2 -->|"fetch events for my pubkey since cursor"| LSELF
    A2 --> L4
    A2 --> L2

    REG --> DUMP
    REG -.->|"verifies provider proof"| OIDC
    DUMP --> MIR
    DIR -.->|"probe liveness, de-weight bad nodes"| L3
    DIR -.-> L4
    W1 -->|"verify append-only + cosign head"| REG
    W2 --> REG
    W3 --> REG
    A1 -.->|"enforce witness policy: 2k > N"| W1
    WEB --> REG

    classDef we fill:#1f6feb22,stroke:#1f6feb
    classDef them fill:#2da44e22,stroke:#2da44e
    class REG,DUMP,WEB,OIDC,DIR,L1,L2 we
    class W1,W2,W3,L3,L4,LSELF,MIR them
```

## Servers to run

| # | Component | Run it? | What it is | Cost |
|---|---|---|---|---|
| 1 | **Registry service** | Yes — day one | HTTP API `POST /v1/register` + `GET /v1/resolve/{handle}`, backed by an append-only Merkle log. Verifies a provider-tagged proof and key-possession signature, appends the binding, and publishes it only after the witness threshold. | 1 small VPS; names are KBs |
| 2 | **Log dump / mirror feed** | Yes — day one | The whole log remains a downloadable no-query stream, not just an API. Immutable exact `[from,to)` NDJSON ranges provide cacheable fresh-client replay. The full stream has an isolated one-request lane and idle-progress timeout, so it cannot consume product range capacity. Together they preserve the exit right without requiring every client to transfer one growing object. | CDN/object-storage traffic within the specified v0.2 bounds |
| 3 | **Loft** (Pigeonpost node) | Yes — 2 to seed the pool | Durable inbox. Stores envelope-v3 wraps keyed by recipient pubkey under the configured jurisdictional retention policy, with no per-client cursor state. **We advertise a capacity equal to our budget and no more**; the pool carries the rest. | $10–20/mo each, by choice |
| 4 | **Identity-provider registrations** | Yes — but no server | GitHub OAuth2 and Google OIDC applications. Credentials are runtime configuration, not infrastructure embedded in a build. | Free |
| 5 | **Name lookup page** | Yes — trivial | Static page that resolves a handle. The only web UI in v1 scope. | ~$0 |
| 6 | **Witnesses** | **No — recruit independent operators** | C2SP witnesses keep durable consistency state, verify checkpoint evolution, and cosign. A process we operate is not independent; production needs a strict-majority threshold (`2k > N`) plus an equivocation drill and an operator-justified `f < 2k - N` equivocator bound. | Free to us |
| 7 | **Registry mirror** | No | Anyone re-serving the dump. First-class by design. | — |
| 8 | **Loft directory + prober** | Yes — day one | Signed list of pool lofts with advertised capacity, retention, and policy, plus a prober that de-weights dead or dishonest entries. Entries are self-signed by the lofts; submissions and removals are appended to the registry's transparency log; weights are recomputable from published probe data. Admission and snapshot caps keep its operating budget explicit. | ~$0 + 1 small VPS |
| 9 | **Community lofts** | **No — this is the point** | Anyone's Pigeonpost node, listed in the directory after probation. Operators running their own agents host their own agents' messages—one `pigeonpost install` away (`node.md`). | Free to us |

Not on the list, deliberately: no chain node, no validators, no consensus network, no background daemon on the agent side, no message queue. Agents hold their own cursor; lofts are dumb storage.

The components we run have explicit budgets rather than an unlimited flat-cost promise. Lofts are
the component that grows with message traffic and are therefore designed to be run by other people.
Registry storage and fresh-bootstrap transfer grow with committed leaves; v0.2 bounds that operating
claim to the specified envelope in `capacity.md`; end-to-end capacity measurement remains a release
operations gate.

## Minimum viable bootstrap

```mermaid
flowchart LR
    V1["VPS #1<br/>registry + log + lookup page"] --> S3["Object storage<br/>log dump"]
    V2["VPS #2<br/>loft"]
    V3["VPS #3<br/>loft"]
    X["3 recruited witnesses<br/>someone else's cron"] -.-> V1
```

Three VPSes, one bucket, three volunteers. Everything else in the design is someone else choosing to join — and past bootstrap, that is not a hope but the funding model: the two lofts above stay at whatever capacity we choose to advertise, and the pool grows around them.

With three witnesses, 2-of-3 gives intersecting signer sets but tolerates no shared equivocator.
Use 3-of-3 when the only justified assumption is that at least one of the three is honest. Clients
with different witness rosters need guaranteed non-equivocating overlap or gossip/out-of-band
checkpoint comparison.

## Message path

```mermaid
sequenceDiagram
    participant A as Agent A (sender)
    participant R as Registry
    participant L as Loft
    participant B as Agent B (offline)

    A->>R: resolve /github/superaidev
    R-->>A: pubkey + inclusion proof
    Note over A: verify proof against<br/>strict-majority cosigned head (2k > N)
    A->>R: fetch B's loft list (kind 10050)
    A->>L: publish gift-wrapped event
    Note over L: stored wrap hides sender key, true send time, kind, and body.<br/>Regulated request-source/receipt metadata is separately sealed.
    Note over B: hours or weeks pass
    B->>L: fetch events for my pubkey since <cursor>
    L-->>B: wrapped events
    Note over B: unwrap, advance cursor, disconnect.<br/>Bodies surfaced to a human as untrusted data.
```

## Scaling notes

- **Registry** uses one small box plus CDN/object-storage caching for the v0.2 envelope: up to
  1,000,000 leaves and 256 MiB canonical NDJSON, with fresh bootstrap engineered against the
  throughput, RTT, and 120-second budget in `capacity.md`. Protocol tests and budget arithmetic cover
  the mechanism, while end-to-end million-leaf wall-clock validation remains an operational release
  gate. Exact immutable range streams keep replay
  cacheable; the full no-query stream remains the mirror/exit artifact. Higher scale is not claimed
  until an authenticated snapshot/map/checkpoint design is specified and measured. A representative
  end-to-end benchmark is still required before claiming observed production throughput at the bound.
- **Lofts** are the only component that grows with traffic, and they shard naturally — an agent's loft list is per-agent, so adding capacity means adding relays, not resizing one. This is also why the loft is the component handed to other operators: growth is absorbed by adding nodes we don't own. Numbers in `capacity.md`.
- **Second registry log** operated by someone else, with clients accepting both, is the milestone where Pigeonpost stops being ours. Design for it; don't build it.

## Keeping the promise

"Design for it, don't build it" is only honest if the door stays open. These five things must be true
from **day one** — each is cheap now and impossible to retrofit once there are users:

| # | Commitment | Why it can't wait |
|---|---|---|
| 1 | **No Pigeonpost domain in any protocol identifier** | An address that embeds `pigeonpost.dev` makes us a permanent dependency. Key addresses are derived from keys and handles are bare paths — neither names us. Already true; keep it true |
| 2 | **Clients hold a list of registries, even when it has one entry** | Retrofitting multi-registry into a deployed client is a breaking change that strands everyone who doesn't upgrade |
| 3 | **Strict-majority witness policy is client-side configuration, never a compiled-in default of ours** | If clients trust "whatever the operator says", the exit right is theatre |
| 4 | **The log dump ships before the API does** | A dump added later covers only what we chose to publish. The no-query stream stays continuous from entry zero for forks; immutable exact range streams make that same history practical to verify and cache within the supported envelope |
| 5 | **Wire format documented well enough for a clean-room implementation, MIT-licensed** | A fork nobody can legally or practically build is not an exit |
| 6 | **Client loft selection is directory-driven, never hardcoded to our lofts** | A hardcoded default cannot be undone in the field, and it makes us absorb the long tail forever no matter how many community lofts exist |
| 7 | **Loft mode ships inside the client library, installable in one command** | Self-hosting has to be a flag, not a project, or nobody does it — `node.md` |
| 8 | **Advertised capacity and retention are protocol fields** | Capacity-weighted selection is what turns our spend into a dial; it must exist before there is anything to weight |

None of these require a second operator to exist. They require that when one shows up, nothing has to
be renegotiated — which is what makes the neutrality claim something other than a promise about our
future behavior.

Commitments 6–8 are also what keep the service solvent: they are why our infrastructure cost is a
number we choose rather than a residual we absorb. See `capacity.md`.
