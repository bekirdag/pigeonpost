# Pigeonpost — what is coming

Status: living document. Opened 2026-08-16.

What is *shipped* is described in the guides; this is the short list of things that are designed,
deliberately not built yet, and the reason for the wait. It exists so that "not yet" is a recorded
decision rather than an omission somebody discovers.

## Unattended execution — the runtime adapter

**What works today.** A request from a trusted sender arrives stamped `auto` by the postbox. An
agent with a session open reads it and carries it out without asking its human — the grant is the
permission. Delivery is push-based, so nothing polls.

**What is missing.** When no session is running, the request waits. Nothing starts an agent to
handle it, because nothing on the machine is willing to launch a model that holds tools on the
strength of a message from the network.

**What would close it.** One function: *run this verb, in this workspace, with this untrusted
context, and return stdout*. Above that boundary everything is already built and running —
per-mailbox routing, per-verb argument schemas, tool allowlists, loop prevention, concurrency
ceilings, an audit line per decision, and a kill switch. The daemon already classifies and audits
every message *as if* it would act, so `agentd-audit.jsonl` shows what would have run before
anything can. `execute` defaults to false.

Below the boundary there are two small implementations, chosen per mailbox rather than detected:

```toml
[[mailbox]]
address   = "/bekir/okacam"
workspace = "/Users/bekirdag/Documents/apps/okacam"
runtime   = "claude"        # or "codex"
verbs     = ["report_status", "answer_question"]
```

**Why it waits.** The hard part is not spawning a process. It is that the verb must select the
action while the body never does: a sender's prose is attacker-influenceable text, and under
automatic execution it reaches a model with tools. The rails were built first so they can be
reviewed before anything runs behind them, and the first two verbs admitted are the two that cannot
change anything — `report_status` and `answer_question`. `run_tests` and `read_file` reach the
filesystem and wait for real sandboxing.

The server-enforced never-auto list (`git_push`, `deploy`, `read_credentials`, `spend`,
`delete_files`, `run_shell`) is the backstop underneath all of it, and no contact entry overrides it.

## Witness independence

The registry is witnessed, but by a witness the same operator runs — which proves the software
works and nothing about the log's history, because one hand holds both keys.
`docs/infrastructure.md` §6 states the requirement: recruit independent operators, a strict-majority
threshold, and an equivocation drill. That is a people problem, not a configuration one, and it has
to be solved before anyone outside relies on the transparency log.

## The loft on the witnessed build

registry and directory run the current build; the loft does not yet. The remaining failure is the
witnessed-registry key cache declining a checkpoint that does now carry a witness quorum. Progress
and everything already eliminated are recorded in the session handoff.

## Reach beyond a held connection

Webhooks per mailbox (URL, HMAC, retry) for agents on publicly reachable hosts and CI, where
holding a connection open makes no sense. Push notifications for a phone app — for humans, not
agents.

## Not planned

- **Waking a sleeping machine.** Mail waits. Agents that must answer promptly belong on an
  always-on host. This is the premise, not a defect.
- **Server-side execution.** The postbox delivers and classifies; it never runs anybody's code.
- **Replacing `review`.** Most mail should still reach a person. Autonomy is the exception someone
  grants deliberately, per sender and per verb.
