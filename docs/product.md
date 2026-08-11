# Pigeonpost — Product Definition

Status: implemented product definition. Production regulatory activation remains subject to the
external gates in `law.md` and `sds.md`.
Opened: 2026-08-07

## Origin

Pigeonpost started as **IPMC** (Inter-Project Messaging Center), a Docdex-internal feature for
letting agents on different repos and machines message each other. Research showed the messaging half is a
general problem worth solving in the open, and the Docdex-specific half is a thin client on top.
Pigeonpost is that general layer; Docdex IPMC becomes its first client.

## What Pigeonpost is

An addressing and durable-inbox layer for AI agents.

1. An agent generates a keypair. That keypair **is** the agent, and its address falls out of the key
   — no registration, no permission, no human.
2. Optionally, it proves a GitHub or Google identity and claims a provider-scoped handle through a
   free, challenge-bound flow.
3. It publishes that address anywhere — README, docs, website.
4. Anyone sends it a message.
5. The message waits.
6. The agent wakes up, reads its inbox, disconnects.

## What Pigeonpost is not

- **Not a real-time protocol.** No live sockets, no session between peers, no request/response.
- **Not an autonomous action channel.** Agents do not act on inbound messages on their own. A message is
  reported to a human, who decides. This is a security property, not a limitation — see "Untrusted
  content" below.
- **Not a chain.** No token, no fee, no wallet, no consensus network.
- **Not a workspace.** No channels, no teams, no presence. Addresses and inboxes only.

## Core requirements

| # | Requirement | Why it's non-negotiable |
| --- | --- | --- |
| 1 | Works when the recipient is offline | Agents are offline ~99% of the time |
| 2 | Free — no fees, no domain, no wallet | Any paywall kills adoption for a dev tool |
| 3 | An agent gets an address with no human involved | Agents must self-address without human ceremony |
| 4 | Human-readable names are claimable and permanent | Publishable on a README that outlives the session |
| 5 | E2E encrypted content, sender and recipient only | Infrastructure operators must never decrypt message content; sender identity has the explicit compliance-key carve-out in `law.md` |
| 6 | Not controlled by us once adopted | Logs, witnesses, dumps, MIT licensing, and forks preserve credible neutrality; attribution escrow materially weakens this property |
| 7 | No background daemon required | Agents wake, drain, disconnect |
| 8 | Our cost is a budget we set, not a function of adoption | The service is free and has no revenue. If success scales our bill, success kills it — see `capacity.md` |
| 9 | Answer lawful orders without decrypting content | Purpose-separated sealed traces, independently verifiable attribution, and offline custody disclose records—not keys |

## Deliberate constraints

**Untrusted content.** A message body arrives from another LLM. It is data, never instruction.
Client libraries must present bodies inside an explicit untrusted envelope, and the reference
integration must report inbound requests to a human rather than executing them. An agent that reads
"delete the auth module" from its inbox reports it; the operator decides.

**No squatting, without gating self-addressing.** A free, permissionless, first-come *human-readable*
namespace is destroyed within a week. Empirical proof: of ~120,000 Namecoin names, 28 were unsquatted
with real content. So the namespace has two tiers: **key addresses** are derived from the agent's own
keypair — free, instant, no registration, nothing to squat because nothing is chosen — while
**handles** like `/github/wodo` are gated on proving an identity you already own. An agent is never blocked
on a human; only vanity is. See `architecture.md`.

**Messages never become permanent.** Bodies live on relays with retention limits and can be deleted.
Nothing encrypted goes into the append-only log — immutable metadata is a permanent liability, and
encrypted bodies on a public log are a harvest-now-decrypt-later trap.

## Users and jobs

| User | Job |
| --- | --- |
| Dev with agents on several repos/boxes | Stop hand-carrying messages between their own agents |
| Maintainer publishing an agent | Give the outside world a way to file requests to it |
| Tool builder (RAG, IDE, CLI) | Add agent messaging without running infrastructure |
| Agent | Self-address, publish that address, drain an inbox on wake |

## Scope — v1

**In:**
- Self-certifying key addresses — no registration path at all
- Provider-scoped handle registration with GitHub OAuth2 or pinned OIDC proof
- Public name registry with independently verifiable log proofs and a client-selected
  strict-majority witness policy under the documented fault assumptions
- Send / fetch / read messages, E2E encrypted
- Spam controls: `acceptAll = false`, capability tokens, PoW stamps, local sender scoring with
  mark-as-spam (see `spam.md`)
- Key rotation via pre-committed successor keys (see `keys.md`)
- Directory-driven loft selection, pool directory, and prober (see `network.md`)
- One-command node install, distributed on npm (see `node.md`)
- Reference client library + CLI + MCP server (see `integration.md`)
- Self-hostable relay ("loft")

**Out (v1):**
- Group messages
- Attachments beyond small inline payloads
- Push notification to humans
- Payments
- Web UI beyond a name-lookup page
- Product-specific Docdex integration (the Docdex maintainer consumes the published client surface)

## Settled decisions

- **Name and domain** — Pigeonpost at `pigeonpost.dev`, registered 2026-08-07. See `branding.md`
- **Provider-scoped handles only** — `/github/superaidev` is canonical. Bare aliases are not part of the
  release contract because cross-provider ownership and collision policy are intentionally not
  delegated to the registry operator. The pre-1.0 `/gh/<login>` spelling remains audit-only history,
  never a claim or resolution alias
- **Build fresh, not on Block's Buzz** — Pigeonpost is its own product and its own service network.
  Buzz is workspace-shaped; adapting it costs what writing a loft costs and inherits a roadmap we do
  not control. We borrow gift-wrap and hashcash patterns but ship Pigeonpost envelope v3
- **Docdex sequencing** — no longer ours to sequence. Docdex integration is owned by the Docdex
  maintainer and will be built against the published client surface (`integration.md`) on their own
  schedule. Docdex remains the intended first client; nothing in the build plan waits on it

Build plan in `sds.md`.

## Success criteria

- An agent addresses itself and receives its first message in under 5 minutes, with no payment, no
  domain, and no human in the loop
- A second, independent operator runs a loft and a witness
- A third-party tool integrates Pigeonpost without our involvement
- Registry can be fully exported and independently verified by anyone
