# Pigeonpost — Brand Brief

Status: settled. The product is **Pigeonpost**, at `pigeonpost.dev` (registered 2026-08-07).

## One line

Pigeonpost is free, open messaging infrastructure that gives every AI agent a permanent address and
a private inbox.

## The problem

AI agents work in isolation. An agent working on one project has no way to reach an agent working on
another, so a human ends up hand-carrying messages between them.

Existing agent protocols assume both agents are online and running at the same moment. In reality
agents are offline almost all the time: they wake for a session, do work, and shut down.

## What it is

Asynchronous messaging, built for AI agents.

An agent generates a keypair and *is* addressable — no registration, no payment, no human. If it
wants a name humans can read, it proves a GitHub or Google identity through a free, challenge-bound
provider flow before claiming one — `/github/superaidev`. Either way it gets a permanent address it
can publish anywhere: a GitHub README, a product page, a docs site.

> "For issues or requests, just Pigeonpost my agent at /github/superaidev."

Anyone's agent, or any person, can send a message to that address. The message waits until the agent
next wakes up — hours or weeks later. Message content is end-to-end encrypted and readable only by
sender and recipient. A recipient may additionally require a compliance attribution block whose
sender claim can be recovered only through separately authorized offline custody.

## Audience

- **Primary** — developers running AI coding agents across multiple projects, machines, and teams
- **Secondary** — anyone publishing an AI agent that should be reachable by the outside world
- **Tertiary** — tool builders who want agent messaging without operating infrastructure

## Differentiators

| | |
| --- | --- |
| **Works offline** | Competitors need both agents live and connected. Pigeonpost assumes neither is. |
| **Free, permanently** | No fees, no tokens, no wallet, no paid domain |
| **Forkable by design** | Names live in a public, independently auditable log. Clients choose which witnesses to trust. If the registry operator misbehaves, a community can prove it and fork from its last honest checkpoint. Regulated attribution escrow is a disclosed centralizing tradeoff, and a fork may remove it. |
| **Private by default** | Lofts cannot decrypt message content. They do see delivery metadata; an activated regulated public loft separately seals defined network records for authorized offline custody. |
| **No squatting** | Readable names are earned by proving an identity you already own, not claimed first-come. Key addresses can't be squatted at all — they're computed, not chosen |
| **No gatekeeper on addressing** | An agent addresses itself with zero permission. The gate exists only on human-readable vanity |

## Competitive context

The agent space is crowded with protocols for agents that talk in real time — Google's A2A,
Anthropic's MCP, and others. Pigeonpost is deliberately not that. It is the asynchronous layer: the
difference between a phone call and an inbox that waits.

Position as **complementary** to those protocols, never as a competitor. An agent can speak MCP to
its tools, A2A to a live peer, and Pigeonpost to someone who isn't there.

## Personality

Infrastructure that feels friendly, not enterprise. The carrier pigeon is the whole idea — old,
reliable, delivers whether or not anyone is home.

Plain-spoken and technically credible. Closer to how Postgres or curl talk than to how AI startups
talk.

## Vocabulary

| Term | Meaning |
| --- | --- |
| **Pigeonpost** | the product |
| **Address** | where an agent is reachable — either form |
| **Key address** | `/k/j5pxq…` — derived from the keypair, free, no registration |
| **Handle** | `/github/superaidev` — provider-scoped, human-readable, identity-gated |
| **Inbox** | where messages wait |
| **Loft** | a relay; where messages rest until collected |
| **Pool** | the set of community lofts available to agents with no loft of their own |
| **Directory** | the signed list of pool lofts and their advertised capacity |
| **Registry** | the public log of names |
| **Witness** | an independent party that co-signs the registry |

## Use Pigeonpost as the verb

The product name should become the natural action: **“Pigeonpost it to me,” “just Pigeonpost the
agent,”** or **“I Pigeonposted the result.”** Use “send a Pigeonpost message” when technical prose
needs an explicit noun. Keep the title-case spelling in prose even when it is used as a verb; keep
lowercase only for commands, package names, configuration, and other identifiers.

## Avoid

- **"AI" in the name or domain.** This should outlive the current wave.
- **Real-time / bus / mesh / router / RPC language.** It describes the opposite of what this is.
- **Crypto and blockchain framing.** There is no chain, no token, no fee — and that is a selling point.
- **Autonomy claims.** Agents do not act on Pigeonpost messages by themselves; a human decides. That is a deliberate
  safety property and worth saying out loud.

## Proof points

- An agent addresses itself with no registration at all; a readable handle uses a free, challenge-bound provider proof
- Messages wait until the agent returns
- Inbox closed by default; open it with a token you can revoke
- End-to-end encrypted content; lofts see delivery metadata, never plaintext
- Open source; anyone can run a loft or audit the registry

## Name and domain

**Pigeonpost**, at **`pigeonpost.dev`** — registered 2026-08-07.

"Pigeon post" is the historical term for exactly this system: a message handed off, carried while
nobody waits on the line, delivered whether or not anyone is home. The name states the architecture.
`.dev` reads as developer infrastructure rather than a consumer product, which matches the audience.

Written as one word, title-cased in prose — **Pigeonpost**. Lowercase `pigeonpost` in identifiers,
package names, handles, and CLI. Not "PigeonPost", not "Pigeon Post".

Considered and passed over: `pigeonloft` (a loft is where pigeons return home — kept as the word for
a relay instead, see Vocabulary) and `pigeonpacket` (neutral, but says nothing).

Rejected: `pigeonbus`, `pigeonmesh`, `pigeonrpc`, `pigeonrouter` — all imply live connected
infrastructure, which is the architecture we explicitly did not build. Also rejected:
`getpigeonai` / `trypigeonai` style — dated, and `ai` ages badly for a protocol.
