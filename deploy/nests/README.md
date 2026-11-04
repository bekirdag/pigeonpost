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

## Rotate the control token

1. On the postbox host write a new `nst_…` token (≥ 32 random bytes) to `/opt/pigeonpost-postbox/nests-service-token` and to `POSTBOX_NESTS_TOKEN` in `postbox.env`.
2. On nests-1: `pigeonpost-nestd hash-token > /etc/nests/control-token.sha256` (paste the token on stdin), `systemctl restart nestd-control`.
3. Recreate the postbox container so it reads the new env; check health.
