# Pigeonpost CLI 0.7.21 release notes

Published to npm as `@bekirdag/pigeonpost@0.7.21` from tag `v0.7.21`. Upgrading the package does not
upgrade a running agentd: run `pigeonpost agentd install`, then restart the daemon.

## Agent SSH

- **`pigeonpost ssh`** opens a shell on an agent's machine through the Pigeonpost tunnel, with a
  step-up check on the client. The machine's agentd must allow it under `[mailbox.shell]`.
- Allowlisted commands run inside the agent's workspace container, not on the host.
- Commands that arrive after a session is torn down are refused.

## Sign-in

- **`pigeonpost login --device` works again.** The identity provider requires PKCE on this flow, and the CLI now sends it. Failures also show the provider's reason instead of a bare HTTP status.
- **`pigeonpost login` over SSH or on a machine with no display now signs in with a code.** The browser redirect could never reach those machines. `--no-browser` keeps the redirect sign-in, for example with a forwarded port.

## Calendar

- **`pigeonpost calendar`** works with a hosted mailbox's calendar from the terminal: list occurrences, create events that invite handles, and answer invitations. The MCP server offers the same actions to agents.

## Security

- The bundled SQLite is upgraded, which closes RUSTSEC-2026-0459.

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

## 0.7.21

0.7.19 was tagged but never published: Alpine replaced musl 1.2.5-r12 with 1.2.5-r13 (security update), so the pinned Linux musl builds failed. 0.7.21 is the same code with the musl pin moved to 1.2.5-r13.

## 0.7.21

0.7.20 was tagged but never published: GitHub retired the hosted macos-14 runner image, so the Apple Silicon build could not start. 0.7.21 is the same code, built on macos-15.
