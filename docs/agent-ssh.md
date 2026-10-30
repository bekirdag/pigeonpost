# Agent SSH — a terminal on an agent's workspace (Phase 8, SDS §11)

A person opens a shell on the machine an agent runs on, positioned in — and, in the confined
modes, confined to — that agent's workspace. Agent machines expose no ports. The session is SSH,
end to end between the person's client and an SSH server embedded in the agent's own `agentd`,
spliced through a tunnel broker that sees ciphertext only.

```
 pigeonpost ssh /acme/agent1          postbox                         agentd on the agent's machine
   │ POST /v1/shell/sessions ─────────► authorize (person + step-up + grant)
   │ ◄── cert (5 min), leg token,       ── shell.request (agent leg) ─────► local opt-in, CA pin, bindings
   │     pinned host key                                                    │
   │ WSS tunnel.pigeonpost.dev/t/<sid> ◄──── broker splices ciphertext ────► WSS (outbound)
   │ SSH: host key must be the pinned one; user certificate must name this session
```

Status: shipped behind the `agent_ssh` feature flag. Off unless the flag is on for the account,
the machine owner enables it locally, and — for org agents — the org and the agent admin allow it.

## 1. Who may open a session

Every one of these must hold, decided in the transaction that issues credentials and re-checked
every 15 s (and immediately after a revocation) for every open session:

| # | Condition | Refusal |
|---|---|---|
| 1 | A **person**: a validated interactive sign-in. Account API keys, org API keys, connectors, another agent's mailbox credential and the agent's own credential are refused. | `human_required` |
| 2 | A **recent strong sign-in**: `auth_time` within 10 minutes and an allowlisted `acr` (§3.4). | `step_up_required` (RFC 9470 challenge) |
| 3 | The person **owns** the agent (a mailbox on their own account), or — for an org agent — is the org **owner**, or holds a live delegation carrying `ssh` for exactly this agent. Org admins and agent admins get no shell implicitly (SDS §6.4). | `delegation_required`; non-members get `no_agent` (existence is not revealed) |
| 4 | For an org agent: the org's `ssh_allowed` setting (off by default; turning it on needs step-up) **and** the agent admin's per-agent switch (off by default; turning it on needs step-up). | `ssh_not_allowed`, `ssh_not_enabled` |
| 5 | The agent's machine has **opted in locally** (`[mailbox.shell]`), is connected, and its host key is the pinned one. | `shell_not_offered`, `agent_offline`, `host_key_pending` |
| 6 | Optional: the org's `ssh_requires_approval` — an owner, admin or agent admin other than the requester approves (with step-up), then the requester collects credentials with a fresh step-up. | `202 awaiting_approval` |

The verbs model is untouched: `run_shell` stays `never_auto`, and no message can open a shell.

A session ends when: either side hangs up; the requester or a supervisor ends it
(`DELETE /v1/shell/sessions/{id}`); its grant stops holding (a delegation revoked or expired, a
membership suspended or removed, a role or authority revision changed, the org setting or the
agent switch turned off, the org suspended, the agent deleted); the machine switches SSH off or
changes mode; the host key changes; 15 minutes pass with no traffic; 8 hours pass; the two legs do
not meet within 2 minutes of issuance; the broker cannot reach the postbox for 60 s; or the agent's
control stream has been gone for 60 s. Teardown kills the session's whole process group and
removes its container.

## 2. Credentials

* **SSH CA** — an Ed25519 key in a root-only file on the postbox host (`POSTBOX_SSH_CA_KEY`,
  mounted read-only into the container; never in the repository, never printed). The postbox
  refuses to load a key file anyone but its owner can read.
* **User certificate** — over the client's *ephemeral* Ed25519 key (made in memory per session,
  never stored), valid for 5 minutes (enough to authenticate once), type `user`, a single
  principal `pp-ssh-<session id>` (never a readable handle), a random serial, only the
  `permit-pty` extension (no port/agent/X11 forwarding, no user rc), no critical options. Its key
  id is the signed session grant: `{sid, agent, org, account, member, via, dlg, dlg_rev, host_fp,
  client_fp, mode, ends_by, channels}`.
* **Leg tokens** — two per session (client, agent), 256-bit, stored as SHA-256, spent once by the
  broker before the connect deadline. The agent leg reaches the machine only over its own
  authenticated control stream.
* **Host key** — per machine, `<PIGEONPOST_HOME>/shell-host-ed25519` (owner-only). The postbox
  pins the first key an agent mailbox reports. A different key is held as *pending* and no
  session opens until a supervisor approves it (`POST /v1/agents/{address}/shell/host-key`, step-up,
  exact fingerprint). Clients additionally pin per agent (`ssh-known-agents.json`) and refuse a
  changed key unless given `--accept-host-key <exact fingerprint>`. The independent check is
  `pigeonpost agentd shell` on the machine, which prints the fingerprint.

agentd never learns the CA from the postbox (Phase 9). For `https://postbox.pigeonpost.dev` the CA
is pinned in the agentd release (`RELEASE_CA`, `SHA256:JYtla5q8tBzb4vFjJgeUNBiHblmEdsBvlKy2G+FbfGg`);
a pin file or `PIGEONPOST_SSH_CA` cannot widen it, and a rotation ships in a new release. For any
other postbox the owner supplies the CA out of band — `PIGEONPOST_SSH_CA`, or the public key in
`shell-ca.pub` — after confirming its fingerprint with that postbox's operator; until then agentd
logs the offered fingerprint and refuses all sessions. A different CA is always refused. It checks every certificate binding against the
`shell.request` it received: CA, validity window, type, principal, key fingerprint, serial, agent,
mode, no critical options.

## 3. The broker

`crates/pigeonpost-tunnel` (Rust, axum/tungstenite), on raijin behind the existing proxies at
`wss://tunnel.pigeonpost.dev/t/<session id>`. It holds no keys. Each leg's first WebSocket message
is its leg token; the broker spends it at the postbox's internal API with its own service token
(`POSTBOX_SVC_TOKEN_TUNNEL` → endpoints `shell/legs`, `shell/check`, `shell/closed` only) and
then splices binary messages with bounded queues. It logs session id, side, byte counts, duration
and end reason — nothing else. Limits: 256 KiB per message, 200 sessions, 60 new connections per
client address per minute, 15 min idle, the postbox's absolute deadline, fail closed after 60 s
without the postbox.

Why the SDS's postbox-side SSE ("shell.request on the agent's existing event stream") became a
per-mailbox control stream: an org agent enrolled with a device credential has no account stream
at all, and the account stream would deliver to every machine of that account. The control stream
uses the agent mailbox's own credential, so a request reaches only the machine holding that agent.

## 4. On the agent's machine

Off by default; only `agentd.toml` turns it on, and nothing remote can:

```toml
[[mailbox]]
address = "/acme/agent1"
workspace = "/srv/app"
[mailbox.shell]
enabled = true
mode = "container"                    # container | allowlist | unconfined — required, no default
image = "docker.io/library/debian:13@sha256:…"  # container / allowlist; default pinned by digest
docker_rootful_acknowledged = false   # confined modes + Docker: see below
runtime = "podman"                    # confined modes: podman | docker (default: podman if installed)
network = "none"                      # confined modes: none | outbound
read_only = false                     # confined modes: mount the workspace read-only
max_sessions = 2                      # 1-8
idle_secs = 900                       # 60-900
```

`pigeonpost agentd shell` shows what is offered, problems, the host-key fingerprint and the CA pin.
Every session is announced in `agentd.log`, as a desktop notification, and in
`agentd-shell.jsonl` (metadata only).

| Mode | Boundary | Platforms |
|---|---|---|
| `container` (recommended) | Podman (rootless, `--userns keep-id`; preferred) or Docker (`--user` = this machine's uid). Docker is refused when `docker info` shows a rootful daemon without user-namespace remapping (container root = host root after an escape), unless the owner sets `docker_rootful_acknowledged = true`; rootless Docker, `userns-remap` and Docker Desktop's VM are accepted. The default image is `debian:13` pinned by digest. Only the workspace, bind-mounted at `/workspace`; `--cap-drop ALL`, `no-new-privileges`, read-only root filesystem, 64 MB `/tmp`, 256 pids, 1 GB, 1 CPU, `--init`, network `none` unless `outbound`, never the runtime socket. Named per session and removed on teardown. | Linux; macOS through Docker Desktop's VM (exercised by the end-to-end harness) |
| `allowlist` | No shell and no terminal. Each request is one exact argv from `[[mailbox.shell.command]]` (absolute program; shells, interpreters and launchers such as `env`, `xargs`, `find`, `sudo` are refused in the config), executed as the container entrypoint — never through a shell on the host — within the same boundary, limits, network policy and runtime checks as `container` mode. Up to `paths` trailing arguments, each a relative path that resolves, symlinks included, inside the workspace. Characters a shell would interpret are refused outright. Validated workspace paths map to `/workspace` inside the container. | Linux; macOS through the runtime VM |
| `unconfined` | The machine user's login shell, `cwd` = workspace. Needs `unconfined_acknowledged = true` as a second explicit statement. Same risk as the existing `full` tier. | Linux, macOS |
| `user` (SDS) | Not implemented in this build; refused by name rather than run as something weaker. | — |

In every mode: SSH `env` requests are refused and the environment is built from nothing (no
variable of agentd's own reaches a session except `PATH`, and unconfined `HOME/USER/LOGNAME/SHELL`);
`TERM` is sanitized; X11, agent forwarding, TCP/streamlocal forwarding in both directions and every
subsystem (so SFTP/SCP) are refused; one session channel per connection. The workspace may not be
`/`, the user's whole home, or contain or sit inside the Pigeonpost home that holds this machine's
credentials. Windows: not offered.

**Allowlist upgrade requirement:** install Podman or Docker even if the previous allowlist worked
without it. There is no fallback to executing on the host. The configured absolute program must
still exist locally for configuration validation and also exist at that path in the selected
image; a program inside the workspace maps to `/workspace`. Choose an image containing your
approved tools. The default Debian image does not include Git. The `image`, `runtime`, `network`,
`read_only` and Docker acknowledgement options apply to both confined modes.

An exact argv allowlist limits the requested command; it cannot stop that command from loading
workspace-controlled hooks or configuration. Those hooks now execute inside the session container.
Treat workspace contents as executable/untrusted, use `read_only` when writes are unnecessary, and
keep `network = "none"` unless outbound access is intended. The container is the isolation boundary;
it does not make arbitrary hooks harmless to the mounted workspace.

Recording (`record = true`) is not implemented and is refused.

## 5. Using it

```sh
pigeonpost ssh /acme/agent1                 # interactive shell
pigeonpost ssh /acme/agent1 -- git status   # one command (the only way into allowlist mode)
pigeonpost ssh /acme/agent1 --accept-host-key SHA256:…   # after a verified host-key rotation
```

When the postbox asks for step-up the CLI opens a browser for a fresh Authorization Code + PKCE
sign-in with the server's `acr_values` and `max_age=0`, requesting only `openid` (no offline
session); that token is used for the request and never saved.

### Why there is no web terminal (yet)

The SDS (§11.3 "Apps") allows the in-app **Open terminal** button only once a *vetted browser SSH
client transport* exists: xterm.js renders text but implements none of SSH's key exchange,
authentication or host verification. No maintained, reviewed browser SSH implementation was
available to vendor; compiling our Rust client to WebAssembly would put a new, unreviewed crypto
build under a web origin. Doing SSH server-side instead would hand the server the plaintext, which
this design forbids. The secure alternative the SDS names is shipped instead: the CLI client.
The enterprise portal shows the command (`pigeonpost ssh <handle>`) and supervises (switches,
approvals, host keys, ending sessions); it never carries a terminal.

### What native round three must add

* A status line for operated/owned agents (`GET /v1/agents/{address}/shell`) and, for supervisors,
  pending approvals with Approve/Deny/End and the host-key rotation prompt showing both
  fingerprints.
* An **Open terminal** action only after a vetted native SSH stack is in place (SwiftNIO-SSH on
  Apple, a reviewed library on Android/Windows/Linux) with the same rules as the CLI: ephemeral
  key in memory, step-up before the request, the postbox-pinned host key plus a local pin with a
  visible key-change warning, the broker URL checked to be `wss://tunnel.pigeonpost.dev/t/<sid>`,
  and nothing logged but metadata.

## 6. API

| Method | Path | Who |
|---|---|---|
| POST | `/v1/shell/sessions` `{agent, client_public_key, cols, rows}` → 201 credentials, 202 awaiting approval | person + step-up + grant |
| GET | `/v1/shell/sessions[?state=]`, `/v1/shell/sessions/{id}` | requester, supervisors |
| POST | `/v1/shell/sessions/{id}/approve`, `/deny` | supervisor ≠ requester, step-up |
| POST | `/v1/shell/sessions/{id}/connect` | requester, step-up |
| DELETE | `/v1/shell/sessions/{id}` | requester, supervisors |
| GET | `/v1/agents/{address}/shell` | owner, supervisors, delegates |
| POST | `/v1/agents/{address}/shell/host-key` `{fingerprint}` | supervisor, step-up |
| PUT | `/v1/orgs/{id}/agents/{address}/shell` `{enabled}` | owner/admin/agent admin (step-up to enable) |
| PATCH | `/v1/orgs/{id}` `settings.ssh_allowed`, `settings.ssh_requires_approval` | owner/admin (step-up to enable / to drop approval) |
| GET | `/v1/shell/ca` | anyone |
| PUT | `/v1/shell/agent`, GET `/v1/shell/agent/events` | the agent mailbox's own credential |
| POST | `/internal/v1/shell/legs`, `/check`, `/closed` | the broker's service token |

"Supervisor" = the personal owner, or an active org owner, admin or agent admin. Supervising is
not the right to a shell.

Audit (`audit_log`, metadata only): `ssh.session.request`, `ssh.session.approve`,
`ssh.session.open` (credentials issued: via, mode, delegation, key and host fingerprints, serial),
`ssh.session.close` (reason, duration, bytes each way), `ssh.host_key.register`,
`ssh.host_key.rotation_requested`, `ssh.host_key.rotate`, `ssh.agent.enable/disable`,
`org.update` for the settings. Closed session rows are deleted after 30 days; the audit log keeps
the record for 2 years.

## 7. Operations

* Postbox env: `POSTBOX_SSH_CA_KEY=/run/ssh-ca/ca` (file mounted read-only from a root-only
  host path), `POSTBOX_SHELL_BROKER_URL=wss://tunnel.pigeonpost.dev`,
  `POSTBOX_SVC_TOKEN_TUNNEL=svc_…`. Without the CA or URL every route answers `shell_unavailable`.
* Broker: `deploy/tunnel/` (systemd unit, env example, nginx server blocks, runbook).
* CA rotation: generate a new key server-side, swap the file, restart the postbox; every agent
  machine refuses the new CA until its owner deletes `shell-ca.pub` (deliberate: a swapped CA is
  exactly what the pin is for). Announce before rotating.
* Rollback: flag `agent_ssh` off (`PUT /internal/v1/features/agent_ssh {"scope":"global","enabled":false}`);
  stopping the broker ends every live session.

## 8. Tests

* `crates/pigeonpost-postbox/src/shell_tests.rs` — authorization matrix (owner, org owner, `ssh`
  delegate; admin, agent admin, `read_inbox` delegate, revoked delegate, outsider; account key,
  org key, another agent, the agent itself; missing step-up; flag off), lifecycle and legs,
  revocation (delegation, org setting, member removal), approval, host-key pinning and rotation,
  timeouts, unconfigured server; `policy.rs` unit matrix; certificate unit tests (expiry, CA,
  principal, extensions).
* `crates/pigeonpost-tunnel/tests/broker.rs` — admission, splice, revocation, idle, connect
  timeout, fail-closed.
* `crates/pigeonpost-cli/src/shell/*` — config refusals, allowlist escapes (traversal, symlinks,
  metacharacters, env assignment), container flags, clean environments; the SSH server over an
  in-memory stream (certificate bindings incl. expiry, env/X11/agent/port forwarding/SFTP refused,
  process-group teardown).
* `tests/e2e-ssh/verify.py` — real postbox, broker, `agentd run` and `pigeonpost ssh` on loopback
  with a recording proxy: commands and an interactive terminal, the broker capture holding SSH
  banners and ciphertext only, allowlist escapes and an approved workspace hook attempting host access over real SSH, both confined modes (when Docker is
  available; required in CI), step-up and stranger refusals, kill and idle teardown, a lying
  directory's host key refused in the handshake, host-key rotation, metadata-only audit.
