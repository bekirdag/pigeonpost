# Pigeonpost — Running a Loft

Status: implemented operator contract.
Opened: 2026-08-07

A **loft** stores encrypted Pigeonpost events for agents. It never receives an agent's private key
and cannot open message bodies. The network protocol and admission lifecycle are in
[`network.md`](network.md); the exact server configuration and production fail-closed matrix are in
[`runtime-configuration.md`](runtime-configuration.md).

## Package

The supported package is `@bekirdag/pigeonpost`. It installs one native `pigeonpost` binary for the
client, MCP server, loft, registry, and directory roles. Use the command below only after the
provenance-verified v0.2.0 package and matching GitHub release are published.

```bash
npm i -g @bekirdag/pigeonpost@0.2.0
```

The npm launcher downloads the matching GitHub Release binary on first run, verifies its checked-in
SHA-256, caches it, and then executes it. It re-hashes a cache hit before every execution and
atomically replaces a missing or invalid cache entry. `PIGEONPOST_RELEASE_BASE` may point at an
internal mirror; the same checksum remains mandatory. Supported release targets are listed in
[`publishing.md`](publishing.md).

When `install` is reached through this npm launcher, the generated service records the stable npm
entrypoint and its absolute Node runtime—not the versioned native cache path. Every service start
therefore passes through the launcher verification boundary, survives safe cache cleanup, and
selects the package version currently installed at that entrypoint. A service created by running a
standalone native release directly remains pinned to that exact native executable.

Service-mode installation through npm requires a global package installation (`npm i -g
@bekirdag/pigeonpost@0.2.0`). `npx` and `npm exec` may run from a disposable `_npx` cache, so
`pigeonpost install` rejects those launcher paths instead of writing one into systemd or launchd.
Those invocations remain valid for ordinary commands and for `pigeonpost install --no-service`.

## Private loft: the one-command path

```bash
mkdir -p "$PWD/pigeonpost-loft"
pigeonpost install --dir "$PWD/pigeonpost-loft"
```

With no domain and no bind override, installation:

1. creates or reuses an owner-only Ed25519 loft key;
2. writes a strict `loft.toml` and the SQLite storage directory;
3. chooses a bounded capacity of at most 20 GiB unless `--capacity-gb` is supplied;
4. binds the loft to `127.0.0.1:7717`;
5. installs and starts a user systemd service on Linux or a LaunchAgent on macOS; and
6. waits for `/ready` before reporting success.

The default storage cap is `max(1 GiB, min(20 GiB, floor(20% of free disk)))`; the one-GiB floor
keeps the generated configuration valid on very small or nearly full filesystems. The installer
prints the selected cap and the `--capacity-gb` override needed to change it. If the platform cannot
report free space, installation fails before generating keys or configuration unless the operator
supplies an explicit bounded `--capacity-gb` value; it never guesses a capacity.

If no supported service manager is available, use `--no-service` and start the exact generated
configuration yourself:

```bash
pigeonpost install --dir "$PWD/pigeonpost-loft" --no-service
pigeonpost loft serve --dir "$PWD/pigeonpost-loft"
```

Windows supports the native binary and manual loft process, but does not claim a Windows service
installer: use `--no-service` and supervise `pigeonpost loft serve` with an operator-chosen manager.
That support is private-loopback only; production regulatory capture currently fails closed on
Windows as documented in [`runtime-configuration.md`](runtime-configuration.md).

Point an agent at the local loft explicitly:

```bash
pigeonpost loft add http://127.0.0.1:7717
```

`loft serve` never joins a directory and never performs an external mutation during startup.

## Public loft: deliberate two-phase activation

A public loft needs more than a domain and certificate. It also needs independently supplied
witness trust, purpose-separated compliance keys, trace custody configuration, and a trusted proxy
boundary. The installer cannot invent those prerequisites, so public installation intentionally
stops before service activation:

```bash
pigeonpost install \
  --dir /srv/pigeonpost \
  --domain loft.example.com \
  --no-service
```

That command writes `loft.toml` and `loft.Caddyfile`, with the loft itself kept on loopback behind
the TLS proxy. Complete the provisioning checklist in
[`runtime-configuration.md`](runtime-configuration.md), activate the proxy and supervised loft, and
verify the public endpoint from outside its host. Only then submit the exact HTTPS origin:

```bash
pigeonpost loft submit \
  --dir /srv/pigeonpost \
  --directory https://directory.example \
  --endpoint https://loft.example.com
```

Submission is signed locally by `loft.key`. It enters `pending`; only 24 continuous hours of clean
independent probing can promote it to `active`. Repeated updates and graceful drain operations use
one strictly increasing authenticated mutation sequence. The directory cannot author an entry for a
loft or silently replace its key.

Every install prints a jurisdiction notice. Public-loft operators must obtain operator-specific
legal review for every jurisdiction in which they operate or are established, configure only the
approved retention and trace purposes, provision the documented custody controls, and establish
the applicable process-intake path before public submission. The software does not infer those
duties from an IP address or silently opt an operator into a jurisdiction. See [`law.md`](law.md)
and the fail-closed production checklist in [`runtime-configuration.md`](runtime-configuration.md).

## Implemented commands

```text
pigeonpost install [--dir DIR] [--domain D] [--capacity-gb N]
                   [--retention-days N] [--bind IP:PORT] [--no-service]
pigeonpost loft serve [--dir DIR] [--bind IP:PORT]
                      [--capacity-gb N] [--retention-days N]
pigeonpost loft submit --dir DIR --directory URL --endpoint URL [--operator HANDLE]
pigeonpost loft drain --dir DIR --directory URL --endpoint URL
                      --after YYYY-MM-DDTHH:MM:SSZ
```

There is no hidden updater, uninstall command, service-status command, or automatic pool enrollment.
Stop or remove a service with the platform service manager, preserving the configured directory and
`loft.key` unless you intentionally retire that identity. Public operators must announce a signed
drain through the directory before shutdown; see the lifecycle procedure in
[`network.md`](network.md).

## npm service upgrades

Upgrade the global package and make it prove the new native artifact before cutting over the
supervised loft:

```bash
npm i -g @bekirdag/pigeonpost@0.2.0
pigeonpost --version
```

Only after both commands succeed, restart the existing service:

```bash
# Linux
systemctl --user restart pigeonpost-loft.service

# macOS
launchctl kickstart -k "gui/$(id -u)/dev.pigeonpost.loft"
```

Check `http://127.0.0.1:7717/ready` after the restart. The stable launcher selects the new
version-specific cache entry, verifies it again, and then starts the loft with the preserved
directory. Removing `~/.cache/pigeonpost` while the loft is running does not affect that process;
the next start fetches and verifies the required artifact again.

If the npm prefix, launcher entrypoint, or Node runtime path changes, first back up the complete loft
directory, then rerun `pigeonpost install` with the same directory and original bounded options. The
installer atomically replaces the manager definition and explicitly restarts it; Linux does not rely
on `enable --now`, which would leave an already-running old executable untouched. An internal mirror
must also be configured in the service manager's environment so a cache miss can be repaired without
public egress.

`submit` and `drain` hold one nonblocking operator lock and persist the exact signed mutation under
`DIR/.pigeonpost-directory-mutations/` before contacting the directory. A timeout, disconnect, or
non-success status leaves that mutation pending. Repeat the same command to retry it; changing the
directory path, endpoint, operator label, or drain deadline is refused until the exact pending
mutation succeeds. Back up this owner-only state with `loft.key`—do not delete or hand-edit it to
skip a sequence.

The hardened lifecycle-state backend in this release is Unix-only. Windows remains supported for a
private manually supervised loft, but public directory submit/drain fails closed until equivalent
reparse-point, ACL, hard-link, and stable-file-identity custody is implemented.

## Configuration

The installer writes bounded values rather than relying on implicit server defaults. A private
configuration begins like this:

```toml
[loft]
bind = "127.0.0.1:7717"
storage_path = "/absolute/path/to/pigeonpost-loft/data/loft.db"
capacity_gb = 20
retention_days = 30
trusted_proxies = []

[loft.policy]
open = true
pow_floor = 0
max_event_bytes = 2097152

[pool]
join = false
```

`pigeonpost loft serve --dir DIR` reads `DIR/loft.toml`; CLI overrides apply only to the named
bounded fields. Unknown fields, unsafe values, non-loopback exposure without the public trust
blocks, stale witness state, or incomplete compliance custody cause startup to fail.

## Containers

Production images are immutable release artifacts. Pin a version **and digest**, mount the complete
operator-provisioned directory at `/var/lib/pigeonpost`, and run the explicit role:

```text
ghcr.io/bekirdag/pigeonpost:<version>@sha256:<digest>
  loft serve --dir /var/lib/pigeonpost
```

Use the hardened Compose definitions under `deploy/` as the executable example. They set a
read-only root filesystem, drop capabilities, bound resources, and mount only the role-specific
state. Do not adapt the old shorthand `pigeonpost loft --domain ...`; that command never existed.

## Security and recovery rules

- Keep `loft.key`, `loft.toml`, directory-mutation state, compliance state, and trace state
  owner-only. Never pass credentials or key material through command-line arguments.
- Back up `loft.key`, directory-mutation state, and the database together. Restoring only part can
  change the authenticated identity, lose the monotonic directory sequence, or lose stored events.
- A public endpoint is HTTPS-only. Plain HTTP is accepted only for loopback development.
- `/health` proves only process liveness. Use `/ready` for routing and rollout decisions.
- Preserve the loft directory across upgrades. A package or image rollback must use the documented
  schema compatibility and restore drill rather than replacing state in place.
- Directory submission is explicit and outbound. Serving a loft does not phone home or join a pool.
