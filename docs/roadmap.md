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

**Now runnable, behind a second key.** `run_tests`, `make_change`, `git_push` and `deploy` are
carried out when — and only when — the sender was granted the verb *and* the machine that would do
the work names it at a permission tier that allows it. The sender's grant lives on the postbox; the
tier lives in `agentd.toml`. Only the first is reachable from the network.

There is deliberately no sandbox. The point is to act on the real checkout, so isolation would
defeat it, and a boundary that looked real without being one would be worse than none. What bounds
a run instead is the route: which repository, which machine, which sender, which tier, a branch
allowlist for anything that leaves the machine, a per-sender daily ceiling, `agentd pause`, and an
audit line for every decision including the refusals.

**Still refused: `read_file`.** `full` supersedes it, and a path-confined reader is a different
feature with a different threat model.

**Designed, not built: a real publish barrier for panels.** A route can now ask for a panel — a
second model reads the work and comments before the reply is sent. What that bounds is the reply and
the local working tree. It is not a gate on publishing: at `full` the draft phase is authorised to
push and deploy, and by the time a reviewer sees the draft the push has happened. The draft prompt
asks the main agent to hold the last step until the review is in, and that is a request to a model
rather than a barrier — so the documentation says "reviewed before it was sent" and never "reviewed
before it was published". Making it real means splitting the draft into propose-then-execute, with
the executing half gated on the panel's verdict. That is a larger feature with its own failure modes
(a proposal that cannot be replayed, a machine that changed underneath it), and it is recorded here
rather than half-built.

**Still true: no session continuity.** A headless run starts cold every time. For a status report
that is arguably better, since it goes and looks rather than recalling; for anything conversational
it is a real loss, and it cannot be fixed while an idle session cannot be woken.

The server-enforced never-auto list is now `read_credentials`, `spend`, `delete_files` and
`run_shell`, and no contact entry overrides it. `run_shell` stays there even though `full` already
implies shell access: a verb for it would add nothing and would cost the ability to refuse it by
name.

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
