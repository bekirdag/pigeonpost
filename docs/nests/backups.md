# Nests backups (nests-1 → raijin's HDD)

**Date:** 2026-11-04 · **Status:** live; nightly; restore rehearsed end to end on a test Nest.
**Files:** `deploy/nests/host/` (nests-1 side) and `deploy/nests/backup-target/` (raijin side).

Owner decision: the backup target is raijin's HDD. raijin also hosts other services, so everything
below adds new, separate pieces there and changes nothing that already runs on it.

## Design

```
nests-1 (root, nightly timer)                                  raijin (behind NAT)
  nests-backup                                                   wg-nestsbak 10.68.0.2  ── dials out ──┐
    sqlite .backup of the ledger                                 nests-backup-sshd :2222 (tunnel only) │
    thin LVM snapshot of every Nest LV (fsfreeze'd)                user nestsbak, one key, ForceCommand │
    borg create  ──ssh──► wg-backup 10.68.0.1 ═══ WireGuard ═══►     borg serve --append-only           │
                                                                     --restrict-to-repository …/repo   │
                                                                     --storage-quota 2T                 │
                         UDP 51821 ◄──────────────────────────────────────────────────────────────────┘
                                                                 /mnt/backups/nests/repo  (sda, HDD)
```

| Property | How |
|---|---|
| Location | raijin's rotational disk (`sda`, ext4, mounted at `/mnt/backups`), directory `/mnt/backups/nests` (0700, user `nestsbak`) |
| Raijin never sees plaintext | borg `keyfile-blake2` mode: the repository key is a file on nests-1 (`/etc/nests/backup/keys/`), not in the repository, and is protected by a random passphrase (`/etc/nests/backup/passphrase`). Nest volumes are additionally stored as their LUKS2 ciphertext. |
| Key escrow | One copy of the key file and the passphrase on the owner's Mac (Application Support folder, mode 0600, and the login Keychain). Never in the repository, never on raijin. Lose both copies and the backups are unreadable. |
| Append-only | The only thing nests-1's key can run on raijin is `borg serve --append-only` for that one repository. A compromised nests-1 can add archives, but a delete or prune it issues is only a log entry: the data stays on disk and the transaction can be rolled back. Space is reclaimed only by the operator on raijin (`borg compact`, see below). |
| Disk-fill protection | `--storage-quota 2T` (the HDD has ~21 TB free; current repository use is megabytes) |
| Network | Only the tunnel. raijin is behind NAT, so raijin dials nests-1 (`PersistentKeepalive`); nests-1 accepts the WireGuard UDP port from raijin's egress address only (ufw). Over the tunnel, nftables on nests-1 (`inet nests_backup`) lets nests-1 open exactly TCP 10.68.0.2:2222 and lets raijin open nothing; on raijin (`inet nestsbak`) only that port is reachable from the tunnel — not the main sshd, not Docker-published ports, nothing routed — and raijin opens nothing towards nests-1. Nest sandboxes cannot reach the tunnel (`inet nests` default deny, plus a forward drop on `wg-backup`). |
| SSH on raijin | A second sshd (`nests-backup-sshd.service`, `/etc/ssh/sshd_nestsbak_config`) bound to 10.68.0.2:2222 only: `AllowUsers nestsbak`, public key only, no PAM/password/TTY/forwarding, `ForceCommand` borg serve. The key in `/etc/ssh/nestsbak_authorized_keys` carries `restrict,from="10.68.0.1"`. The main sshd and its config are untouched. |

### What one archive holds

`nests-<UTC stamp>`:

- `var/backups/nests/stage/nestd.db` — consistent ledger copy (`sqlite3 .backup`);
- `var/backups/nests/stage/keys/<nest_id>.key` — the LUKS key of every Nest volume in the archive;
- `var/backups/nests/stage/etc/` — `nestd.toml`, `nests.nft`, `control-token.sha256` (never `upstream.key`);
- `dev/nests/bak-<nest_id>` — the raw image of each Nest's thin LV (LUKS2 header + ciphertext),
  read from a thin snapshot taken while the mounted filesystem was frozen (`fsfreeze`, well under a
  second), so it is filesystem-consistent. Unwritten areas are zeros and deduplicate to nothing.

The LUKS keys travel with the images because a volume without its key cannot be restored; both are
inside borg's encryption, so the target still sees neither.

### Schedule and retention

`nests-backup.timer`: daily 03:17 UTC (±10 min, `Persistent=true`). Retention, applied by nests-1
with `borg prune` after every run: 14 daily, 8 weekly, 6 monthly archives. On an append-only target
that prune is logical. Reclaiming the space is a deliberate operator step on raijin, done after
checking from the owner's Mac (with the escrowed key) that the archive list is the expected one:

```sh
# on raijin, as root — after verifying `borg list` from the Mac
sudo -u nestsbak borg compact /mnt/backups/nests/repo
```

Never run it when the history looks wrong: that is exactly the case append-only protects against
(an attacker's deletes are undone by rolling the repository back to the transaction before them;
see borg's "append-only mode" documentation).

Local copies on nests-1: the nightly ledger `.backup` is also kept 14 days in `/var/backups/nests`.

## Restore

`/usr/local/sbin/nests-restore` on nests-1:

```sh
nests-restore list                                  # archives
nests-restore show nests-20261104T…                 # contents and sizes
nests-restore ledger nests-20261104T… /root/nestd-restored.db
nests-restore volume nests-20261104T… <nest_id> [<as_id>]
```

`volume` recreates `nests/nest-<as_id>` as a thin LV of the original size, writes the image with
`dd conv=sparse` (the LV stays thin), restores the key file (0400) and checks the LUKS header. It
refuses to overwrite an existing LV or key.

### Disaster recovery (nests-1 lost)

1. Rebuild the host from `deploy/nests/README.md` (packages, pool, network, firewall, WireGuard).
2. Recreate `/etc/nests/backup/` from the owner's Mac (key file into `keys/`, passphrase), a fresh
   SSH key for the target (replace the line in raijin's `/etc/ssh/nestsbak_authorized_keys`), the
   target's host key in `known_hosts`, and `/etc/nests/backup.env`.
3. `nests-restore ledger <archive> /var/lib/nests/ledger/nestd.db` (with `nestd-control` stopped),
   then `nests-restore volume <archive> <id>` for each Nest in it.
4. Start `nestd-control`; reconcile starts every Nest that is wanted running, with a new generation
   (new broker token; the mailbox credential on the volume is unchanged).

### Restore rehearsal (2026-11-04)

A test Nest was created through the production postbox ops path (synthetic account, flag on for
that account only; the global `nests` flag stayed off), files were written into its volume, the
backup ran, and the volume was restored under a new id from the archive on raijin: LUKS opened with
the restored key, filesystem clean (`e2fsck -n`), and the SHA-256 of every file (7, including a
3 MB random canary) matched the original. The ledger copy passed `PRAGMA integrity_check` and
contained the Nest's row. Measured: backup of the running 5 GiB Nest 20 s (5.37 GB read, 174 MB
stored after compression and deduplication); restore 42 s; the restored LV stayed thin (3.3 %).
The Nest kept running through the freeze. The restored copy was removed afterwards; the test Nest,
the synthetic account and its flag row were removed after the broker test.

Append-only, tested the same day: nests-1 created a probe archive, then ran `borg delete` and
`borg compact` on it. The archive disappeared from the listing, but the repository on raijin grew
by the probe's size and lost nothing: the delete is a log entry, the client's compact is a no-op.

## Operate

- Status: `systemctl list-timers nests-backup.timer`; last run `journalctl -u nests-backup -n 30`.
- Tunnel: `wg show wg-backup` (nests-1) / `wg show wg-nestsbak` (raijin): a handshake within the
  last two minutes means raijin is dialling in.
- raijin's egress address is in the nests-1 ufw rule for UDP 51821. If it changes, the handshake
  stops and the nightly run fails at `ssh`; update the rule (`ufw allow proto udp from <new> to any
  port 51821`, delete the old one).
- Repository size: on raijin `du -sh /mnt/backups/nests/repo`.
- Rotate the nests-1 → raijin SSH key: new key in `/etc/nests/backup/id_ed25519`, replace the line
  in raijin's `/etc/ssh/nestsbak_authorized_keys` (keep `restrict,from="10.68.0.1"`).
