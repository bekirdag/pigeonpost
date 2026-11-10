# Nests host (nests-1) — runbook

Phase A design, threat model and measurements: [docs/nests/phase-a-plan.md](../../docs/nests/phase-a-plan.md).
Never install any of this on raijin or the postbox host (the only exception is `backup-target/`,
the append-only backup endpoint on raijin's HDD); the postbox host only gets the WireGuard
peer and the egress allowance for `10.66.0.2:8790`.

## Files

| File | Installed as |
|---|---|
| `host/runsc-nests` | `/usr/local/sbin/runsc-nests` — gVisor wrapper (systrap, no host sockets) |
| `host/50-nests-runsc.conf` | `/etc/containers/containers.conf.d/` — registers the `runsc-nests` runtime |
| `host/nests-pool-attach`, `host/nests-pool.service` | attach `/var/lib/nests/pool/pool.img` → VG `nests`, thin pool `pool` at boot |
| `host/50-nests-svc.netdev`, `.network` | `/etc/systemd/network/` — dummy `nests-svc`, `10.67.255.1/32` (broker address) |
| `host/nests.nft`, `host/nests-firewall.service` | `/etc/nests/nests.nft` — sandbox egress policy, loaded at boot |
| `host/wg-nests.conf.example` | `/etc/wireguard/wg-nests.conf` on both ends (`10.66.0.1` postbox ↔ `10.66.0.2` nests-1) |
| `host/nestd.toml.example` | `/etc/nests/nestd.toml` |
| `host/nestd-control.service`, `nestd-broker.service`, `nestd-mock-upstream.service` | `/etc/systemd/system/` (the mock only for tests without mswarm) |
| `host/journald-nests.conf` | `/etc/systemd/journald.conf.d/nests.conf` |
| `host/nests-backup`, `nests-restore`, `nests-backup.service`, `.timer` | `/usr/local/sbin/`, `/etc/systemd/system/` — nightly ledger copy + borg archive (ledger, LUKS keys, Nest volume images) to raijin's HDD; [docs/nests/backups.md](../../docs/nests/backups.md) |
| `host/backup.env.example`, `host/wg-backup.conf.example` | `/etc/nests/backup.env`, `/etc/wireguard/wg-backup.conf` — the backup tunnel (`10.68.0.1` nests-1 ↔ `10.68.0.2` raijin) |
| `backup-target/*` | raijin only: `wg-nestsbak`, the dedicated borg sshd and the `inet nestsbak` table; see backups.md |
| `runner/Containerfile`, `runner/build.sh` | runner image (digest-pinned bases) and the host `pigeonpost-nestd` binary |

## Install (once)

1. Back up `/etc` (`tar -C / -czf /root/setup-backup-<stamp>/etc.tgz etc`).
2. Packages: `podman crun netavark aardvark-dns uidmap wireguard-tools borgbackup sqlite3`; gVisor from
   its signed apt repository (key fingerprint `6F1D F85E 3A71 C249 18E7 27D5 6FC6 D554 E32B D943`),
   pinned with `/etc/apt/preferences.d/gvisor-runsc` and `apt-mark hold runsc`.
3. Install `runsc-nests` and `50-nests-runsc.conf`.
4. Storage pool: the commands in the header of `nests-pool-attach`, then
   `systemctl enable --now nests-pool`.
5. Network: install the `nests-svc` files, `networkctl reload`; then
   `podman network create --driver bridge --interface-name nests0 --subnet 10.67.0.0/24 --gateway 10.67.0.1 --disable-dns --ipv6=false nests`.
6. Firewall: `/etc/nests/nests.nft`, `systemctl enable --now nests-firewall`; ufw:
   - `ufw allow proto udp from 45.132.181.100 to any port 51820`
   - `ufw allow in on wg-nests proto tcp from 10.66.0.1 to 10.66.0.2 port 8790`
   - `ufw allow in on nests0 proto tcp from 10.67.0.0/24 to 10.67.255.1 port 8788`
   - `ufw allow proto udp from <raijin egress address> to any port 51821` (backup tunnel)
7. WireGuard: `wg genkey` on each host into `/etc/wireguard/nests.key` (0600), exchange public keys
   only, install `wg-nests.conf`, `systemctl enable --now wg-quick@wg-nests` on both.
8. Users and secrets: group `nests`, system user `nest-broker`; `/var/lib/nests` 0711,
   `/var/lib/nests/ledger` 2770 root:nests; `/etc/nests/upstream.key` 0600 root (the mswarm key,
   written by piping it from its source, never echoed; a random key only for the mock); `/etc/nests/control-token.sha256` 0600 root holding
   `pigeonpost-nestd hash-token < token` of the token in `/opt/pigeonpost-postbox/nests-service-token`
   on the postbox host.
9. Build: `deploy/nests/runner/build.sh <clean checkout of the merged commit>`; copy the printed
   `…@sha256:` reference into `nestd.toml`, install the extracted `pigeonpost-nestd` to `/usr/local/bin`.
10. Backups: follow [docs/nests/backups.md](../../docs/nests/backups.md) (both ends), then
    `systemctl enable --now wg-quick@wg-backup nests-backup.timer`.
11. `systemctl enable --now nestd-broker nestd-control`.

## Operate

- Status: `systemctl status nestd-control nestd-broker`; logs `journalctl -u nestd-control -u nestd-broker`.
- Health, from the postbox host: `curl -H "Authorization: Bearer $(cat /opt/pigeonpost-postbox/nests-service-token)" http://10.66.0.2:8790/v1/health`.
- Nests: `podman ps --filter label=dev.pigeonpost.nest`; volumes `lvs nests`; ledger `sqlite3 /var/lib/nests/ledger/nestd.db`.
- Firewall changed: `systemctl reload nests-firewall`; counters `nft list table inet nests`.
- Restarting `nestd-control` leaves Nests running; it reconciles on start.
- Model spend: `[broker]` caps in `nestd.toml` (per Nest and global, monthly tokens); this month's
  total: `sqlite3 /var/lib/nests/ledger/nestd.db "select period, sum(input_used+platform_input), sum(output_used+platform_output) from usage group by period"`.
- Backups: `journalctl -u nests-backup -n 30`, `nests-restore list`; see backups.md.

## Marketplace pool (M0, not set up on any host yet)

The pool is configured in `[sandbox.marketplace]` and runs publisher code under gVisor on this same
host, because the owner chose no KVM host (2026-11-09; the plan said Firecracker). Since M1 the
shared Nest dispatcher runs its workers there (below). The nestd ledger stays on SQLite, and OpenBao and SeaweedFS are not used yet (one
host). See [the deviations](../../docs/nests/contracts/README.md).

When it is enabled, in order:

1. `podman network create --driver bridge --interface-name nestsmp0 --subnet 10.69.0.0/24 --gateway 10.69.0.1 --disable-dns --ipv6=false nests-mp`
2. `ufw allow in on nestsmp0 proto tcp from 10.69.0.0/24 to 10.67.255.1 port 8788`
3. Reload the firewall so `nests.nft`'s `inet nests_marketplace` table and the `nestsmp0` bridge rule
   apply. They do nothing while the bridge does not exist.
4. Live isolation probes, on the host as root, against the runner image:
   `NESTS_LIVE_GVISOR=1 NESTS_LIVE_IMAGE=<runner image> cargo test -p pigeonpost-nests --test marketplace_isolation -- --ignored --nocapture`

## Marketplace M1: build, analysis and the shared Nest dispatcher (not set up on any host yet)

With `[sandbox.marketplace]` configured, `pigeonpost-nestd control` also serves the dispatcher
(`/v1/mp/jobs`, `/v1/mp/jobs/{job}`, `/v1/mp/releases/{release}/quarantine`) behind the same
control token. It runs only releases whose staff attestation the postbox sends with each job, one
worker per listing and buyer context under `<volume_root>/<context>/<worker>`, and the first-party
reconcile, idle stop and list leave those workers alone. Design, routes and deviations:
[docs/nests/contracts/README.md](../../docs/nests/contracts/README.md) ("Marketplace M1").

The build and analysis planes are `pigeonpost-nest-release`, run by an unprivileged user (the build
refuses rootful podman). The reference release's source tree is `reference/hr-policy-qa/`.

1. A fetch-only podman network for `cargo vendor` (the build step itself has `--network=none`), and
   the base images the Containerfile pins, pulled by digest beforehand (`--pull=never`).
2. `pigeonpost-nest-release build --config build.toml --src reference/hr-policy-qa --work <empty dir>`
   with `runtime`, `fetch_network`, `fetch_image` (pinned) and `image_name`; prints the build record
   for `POST /internal/v1/marketplace/builds`.
3. `pigeonpost-nest-release scan --src …` and `ai-review --route review.toml --src …` (model
   `qwen-latest-flash-next` on the self-hosted route only); each prints a stage record for
   `POST /internal/v1/marketplace/reviews/{review}/stages`. The behaviour test under gVisor:
   `NESTS_LIVE_GVISOR=1 NESTS_LIVE_RELEASE_IMAGE=<image> cargo test -p pigeonpost-nests --test marketplace_m1 -- --ignored live_behaviour`.
4. A staff reviewer approves in the postbox (`POST /v1/staff/reviews/{review}/decisions`), which
   attests the release digest and publishes the listing.

## Rotate the control token

1. On the postbox host write a new `nst_…` token (≥ 32 random bytes) to `/opt/pigeonpost-postbox/nests-service-token` and to `POSTBOX_NESTS_TOKEN` in `postbox.env`.
2. On nests-1: `pigeonpost-nestd hash-token > /etc/nests/control-token.sha256` (paste the token on stdin), `systemctl restart nestd-control`.
3. Recreate the postbox container so it reads the new env; check health.

## Phase B6: off-host key, tombstones, monitoring

**Host key (KEK).** Volume keys in `/var/lib/nests/keys` are stored wrapped under a 32-byte host
key that only the postbox holds: `POSTBOX_NESTS_KEK_FILE` points at a root-only file with 64 hex
characters. Escrow copy: the owner's Mac Keychain, service `pigeonpost-nests-kek`.
- Every minute the postbox calls `POST /v1/unlock {kek, tombstones}` over WireGuard.
- After any nestd restart or reboot, nestd is **locked** until that call. It creates and starts
  nothing, and reconcile waits. Sandboxes that were already running keep running.
- The first unlock records the key's fingerprint in `/var/lib/nests/kek.fpr`; a different key is
  refused from then on.
- Phase A key files are wrapped in place the first time each one is read while unlocked.
- Lose the KEK and every Nest volume, including every backup of it, is unreadable. Keep the escrow.

Create it once, on the postbox host (never on nests-1):

```sh
umask 077; openssl rand -hex 32 > /opt/pigeonpost-postbox/nests-kek
# add POSTBOX_NESTS_KEK_FILE=/opt/pigeonpost-postbox/nests-kek to the postbox environment, restart
```

**Tombstones.** The same call carries the ids of every Nest the postbox has deleted (newest
1000). nestd destroys any of them it still has: a missed delete, or a ledger or volume brought
back by `nests-restore`. A revoked Nest therefore stays revoked after a restore. Its mailbox
credential was revoked at delete time anyway.

**Restore order.**
1. `nests-restore ledger|volume …`.
2. Start nestd. It is locked.
3. Wait one minute: the postbox unlocks it and replays tombstones.
4. Reconcile starts what should run.

**Monitoring** (`monitoring/`). All of it runs on nests-1, on loopback:
- Prometheus (`127.0.0.1:9090`, 15 days) scrapes nestd's metrics (`127.0.0.1:9790`, no ids or
  content) and node_exporter (`127.0.0.1:9100`).
- `nests-rules.yml` covers: nestd down, locked, failed Nests, admission limit, thin pool, volume
  allocation, stale or missing backups, memory (the VMware balloon), root disk, and
  platform-error tokens.
- Alertmanager (`127.0.0.1:9093`) posts to `pigeonpost-nestd alert-relay` (`127.0.0.1:9796`).
  The relay sends one Pigeonpost message per notification from a dedicated alerts mailbox.
  Its token is `/etc/nests/alerts.token`, root 0400, passed with `LoadCredential`.
- The backup writes `/var/lib/nests/backup-last-success` after each archive.
- Packages: Debian `prometheus`, `prometheus-alertmanager`, `prometheus-node-exporter`
  (Apache-2.0). Bind each to loopback in `/etc/default/*`.
