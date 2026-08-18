# Pigeonpost — what is coming

Status: living document. Opened 2026-08-16.

What is *shipped* is described in the guides; this is the short list of things that are designed,
deliberately not built yet, and the reason for the wait. It exists so that "not yet" is a recorded
decision rather than an omission somebody discovers.

## Unattended execution — shipped, and what is still held back

The runtime adapter this section used to describe as "designed, deliberately not built" is built.
`agentd` will run a request itself, spawning a one-shot headless agent rather than waiting for a
session, and it stays off until `agentd.toml` says `execute = true`. See the guides for the config;
what belongs here is what is *still* refused and why.

**The reason the wait was worth it.** The hard part was never spawning a process. It is that the
verb must select the action while the body never does: a sender's prose is attacker-influenceable
text, and under automatic execution it reaches a model with tools. So the rails were built and
reviewed first, and the daemon spent its early life classifying and auditing every message *as if*
it would act — which meant `agentd-audit.jsonl` showed what would have run on real traffic before
anything could.

**Still refused: `read_file` and `run_tests`.** Both are grantable on the postbox and neither is
runnable here. They reach the filesystem, and the sandboxing that would make that safe does not
exist yet — a path argument from a peer is exactly the shape of a request that must not be trusted
because the verb was granted. The two verbs that *are* runnable, `report_status` and
`answer_question`, take no path and no command by design.

**Still true: no session continuity.** A headless run starts cold every time. For a status report
that is arguably better, since it goes and looks rather than recalling; for anything conversational
it is a real loss, and it cannot be fixed while an idle session cannot be woken.

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
