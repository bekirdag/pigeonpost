# Pigeonpost CLI 0.7.19 release notes

Published to npm as `@bekirdag/pigeonpost@0.7.19` from tag `v0.7.19`. Upgrading the package does not
upgrade a running agentd: run `pigeonpost agentd install`, then restart the daemon.

## Agent SSH

- **`pigeonpost ssh`** opens a shell on an agent's machine through the Pigeonpost tunnel, with a
  step-up check on the client. The machine's agentd must allow it under `[mailbox.shell]`.
- Allowlisted commands run inside the agent's workspace container, not on the host.
- Commands that arrive after a session is torn down are refused.

## Organization agents (enterprise beta)

- **`pigeonpost login --device --handle /acme/agent1 [--label "Build box"] [--workspace DIR] [--onboard]`**
  brings a machine up as one company agent. The terminal shows a short code, a confirmation number
  and a QR code. A person with authority over that agent (an agent admin, or someone with an
  `enroll` delegation) scans it, checks that the confirmation number matches, signs in with
  two-step verification and holds to approve. The machine receives only that agent mailbox's
  credential — never a personal sign-in.
- **`pigeonpost login --enroll-stdin`** (or `--enroll CODE`) redeems a one-time enrollment code an
  admin created in the enterprise portal, for scripted installs. Prefer `--enroll-stdin`: a code on
  the command line can end up in shell history.
- After either flow, an existing account session on that machine is revoked at the realm and
  removed, so the machine holds the agent's mailbox credential only.
- `--onboard` records the workspace context for the agent mailbox. It grants no SSH access and no
  extra automation.
- `--postbox URL` selects the postbox to enroll with (default `https://postbox.pigeonpost.dev`).

Existing `pigeonpost login --device` without `--handle` is unchanged.
