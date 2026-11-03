# Pigeonpost Nests — Phase A plan (nests-1, gVisor)

**Date:** 2026-11-03 · **Status:** Phase A built behind the `nests` flag (off in production); no sales.
**Source plan:** `docs/planning/2026-10-21-hosted-agents-plan.md` (local planning folder). This document
adapts it to the only Nests host that exists, and records what was measured there.
**Runbook:** [deploy/nests/README.md](../../deploy/nests/README.md).

## 1. Scope and owner decisions

- One host, `nests-1` (193.164.5.238): a VMware VM, 7 vCPU Xeon Gold 6138, 15.6 GiB RAM as seen by
  the guest, one 110 GB disk, Ubuntu 24.04. **No KVM** (`/dev/kvm` absent, no `vmx`). No more
  hardware until there are paying clients; a dedicated KVM host comes later.
- Sandbox: **gVisor `runsc` (Apache-2.0), systrap platform**, under rootful podman. The runtime is a
  trait (`crates/pigeonpost-nests/src/sandbox.rs`), so a Firecracker/KVM worker replaces it without
  touching the control plane, the ledger or the postbox contract.
- Free/open-source only: podman, crun, netavark (Apache-2.0), gVisor (Apache-2.0), LVM2/cryptsetup
  (GPL-2.0), nftables (GPL-2.0), WireGuard (GPL-2.0), SQLite (public domain), restic (BSD-2).
- Never on raijin or the postbox host. The postbox host only gained a WireGuard peer.

Phase A delivers: lifecycle (create, provision, start, pause, resume, stop, wake, delete), a durable
run/lease/usage ledger, idle stop, reconcile after reboot, the runner image, the model broker with
reserved per-Nest token budgets, postbox internal endpoints plus a public enrollment redeem, and an
end-to-end test. It does **not** deliver purchases, the Products page, native/web UI, file
ingestion, tools, or the real mswarm tenant (§7).

## 2. Architecture on this hardware

```
 clients ──► postbox (45.132.181.100) ──WireGuard 10.66.0.1→10.66.0.2:8790──► nestd control (root)
               ▲   │ ops token: /internal/v1/nests*                          │ podman + runsc-nests
               │   │ wake-on-mail ──────────────────────────────────────────►│ LVM thin + LUKS2
               │   │                                                         ▼
               │   └◄── HTTPS 443 (only allowed egress) ◄── Nest sandbox (gVisor, uid 10001)
               │                                              │ agentd (mailbox credential only)
               │                                              │ nest runner (one bounded model call)
               │                                              ▼
               │                                 model broker 10.67.255.1:8788 (user nest-broker)
               │                                              │ reserve → call → settle (SQLite ledger)
               │                                              ▼
               └──────────────────────────────── mswarm PP tenant → OpenRouter → DeepSeek
                                                 (Phase A: loopback mock until the tenant exists)
```

| Piece | Where | Notes |
|---|---|---|
| `pigeonpost-nestd control` | nests-1, root, binds `10.66.0.2:8790` only | Bearer token compared as SHA-256; the token lives only on the postbox host |
| `pigeonpost-nestd broker` | nests-1, user `nest-broker`, binds `10.67.255.1:8788` | systemd-sandboxed; upstream key via `LoadCredential`, never in a sandbox |
| Ledger | `/var/lib/nests/ledger/nestd.db` (SQLite WAL, 0660 root:nests) | Shared by control and broker |
| Runner image | `localhost/pigeonpost-nest-runner@sha256:…` | Debian bookworm-slim + `pigeonpost` (agentd) + `pigeonpost-nest-runner`; 124 MB |
| Postbox | prod, flag `nests` off | `/internal/v1/nests*` (ops token), `POST /v1/nests/enroll`, wake-on-mail |

### Why podman, not containerd

Rootful podman is daemonless: there is no long-lived API socket that could ever be exposed to a
sandbox or misconfigured onto a network, and the control service is the only process that runs it.
It integrates gVisor with one `containers.conf` runtime entry, netavark gives a plain bridge that
nftables can police, and the repository's agent-SSH container mode already speaks podman. containerd
would add a daemon, a socket and a separate CLI (nerdctl) for no Phase A benefit; the Firecracker path
(firecracker-containerd or a direct supervisor) is behind the same trait either way.

### Why SQLite, not PostgreSQL, for Phase A

One host and one writer per row. SQLite in WAL mode gives atomic reservations (`BEGIN IMMEDIATE`)
shared by two local processes with nothing to operate, patch or back up beyond one file
(`nests-backup`, nightly `.backup`). The schema is plain SQL; it moves to PostgreSQL (the source
plan's choice) when the control plane is separated from the workers or a second worker exists,
because leases and reservations then span hosts.

## 3. The sandbox profile (gVisor)

Every Nest is started with the same fixed arguments (`GvisorPodman::run_args`, asserted by a test):

| Control | Setting |
|---|---|
| Kernel boundary | `runsc --platform=systrap --network=sandbox --host-uds=none --host-fifo=none --overlay2=root:memory` (wrapper `/usr/local/sbin/runsc-nests`) |
| Identity | `--user 10001:10001`, `--cap-drop ALL`, `no-new-privileges`, no `--privileged`, no host PID/IPC/network |
| Filesystem | `--read-only` image; `/tmp` tmpfs 64 MiB `noexec,nosuid,nodev`; the Nest volume at `/nest` `noexec,nosuid,nodev`; no other host mount, no container socket |
| Memory | `--memory 512m --memory-swap 512m` (no swap); start at 512 MiB, measured below |
| CPU | cgroup v2 weight 100 (fair share, no pinned core), cap `--cpus 1.0` per Nest |
| Processes | `--pids-limit 128` |
| Disk | one thin LV per Nest, default 5 GiB = the hard quota; sum of quotas ≤ 60 GiB (no thin overcommit) |
| Network | bridge `nests0` 10.67.0.0/24, no DNS (`--dns none`), two pinned names: the postbox and the broker |
| Logs | podman `k8s-file`, 1 MB cap; agentd logs only metadata |
| Image | pinned by digest; `--pull never`; config refuses an unpinned image |

### Storage: encrypted per-Nest volumes on a single disk

`/var/lib/nests/pool/pool.img` (64 GiB, preallocated) → loop device → LVM VG `nests` → thin pool
(62.6 GiB). Each Nest gets `nests/nest-<id>` (thin LV, size = quota), formatted inside **its own
LUKS2 container** (aes-xts-plain64, 512-bit, random 64-byte key in `/var/lib/nests/keys/<id>.key`,
0400 root). The volume is unlocked and mounted only while the Nest runs; stop closes it. Delete runs
`cryptsetup erase`, removes the LV and the key: the data is unrecoverable even before the thin pool
reuses its blocks (which is why `--zero n` is acceptable).

What the encryption does and does not do: it gives crypto-erase on delete, keeps one Nest's blocks
unreadable to anything that only sees another Nest's device, and makes a stray copy of `pool.img`
useless. The keys are on the same disk, readable by root on this host, so it **does not protect
against a root compromise of nests-1 or against the VMware host** (which can read guest memory). An
off-host unlock key (for example released by the postbox over WireGuard at boot) is a Phase B
hardening item.

### Network policy (`/etc/nests/nests.nft`)

Default deny for everything a Nest starts. Allowed: TCP 443 to the postbox address
(45.132.181.100), and TCP 8788 to the broker's host address 10.67.255.1. Nothing else: no host
services (SSH, control API), no metadata/link-local, no RFC 1918/CGNAT ranges, no neighbouring
hosts in 193.164.5.0/24 (this /24 also holds other Wodo hosts, including the call relay), no
internet, no DNS, no IPv6. A bridge-family rule drops every frame between two Nest ports, so Nests
cannot reach each other at layer 2 either. The chains run at priority −10 and only drop, so ufw and
netavark cannot widen them. ufw additionally admits only the three Nest-related flows (§ runbook).

## 4. Identity, credentials and trust boundaries

| Boundary | Mechanism |
|---|---|
| Who may manage Nests | Only the postbox, over WireGuard, with `POSTBOX_NESTS_TOKEN` (SHA-256 on nests-1). The postbox exposes the lifecycle only to the **ops** service token (owner admin path) in Phase A. |
| Who owns a Nest | The postbox: `nests` table (account, address), `nests` flag per account, per-account limit (default 1). nestd never decides ownership. |
| The Nest's mailbox | A server-custodied agent mailbox under the owner's account. The postbox discards its original token, issues a **single-use enrollment code** (15 min, hash stored). nestd writes the code into the volume; the sandbox generates an Ed25519 device key, signs the code, and redeems it at `POST /v1/nests/enroll`, which **rotates** the mailbox token and returns it to the sandbox only. Delete rotates it again to a value nobody holds. |
| What the sandbox holds | Its mailbox token and a broker token for the current start (`nbt_…`, generation-bound, revoked at stop/pause/delete). No account credential, no provider key, no host socket. |
| Who may make it act | agentd only answers `answer_question` requests the postbox marked `autonomy=auto`; Phase A grants that only to the owner's own `/k/` mailboxes listed at creation. Strangers' mail is never executed. |
| Model spend | The broker reserves the worst case (estimated input + max output) against the Nest's monthly input and output limits **before** calling upstream, one request at a time, fixed model, text only, 16k input / 1k output caps. Settled once from upstream usage; an unknown outcome (timeout, missing usage) is charged to the platform error budget, never retried blindly. |

### Threat model: gVisor here versus a microVM (stated plainly)

gVisor is a **weaker** boundary than the Firecracker/KVM microVM the plan selected:

- A Firecracker guest has its own kernel behind hardware virtualization; an escape needs a bug in
  KVM or the small Firecracker VMM, jailed and seccomp-filtered. gVisor's Sentry is a user-space
  kernel in Go that intercepts syscalls (systrap: seccomp-trap based); an escape needs a bug in the
  Sentry **and** a way past its own seccomp filter to the host kernel's restricted syscall surface.
  Its attack surface is small compared with a plain container, but it shares the host kernel and is
  a younger, software-only boundary.
- On this host the "hardware" boundary is VMware's, and it protects the host from us, not Nests
  from each other or from the provider.
- systrap on a VM without KVM is gVisor's slowest platform for syscall-heavy work; that is
  acceptable for agents that mostly wait on the model, not for compilers or browsers.

Consequences for Phase A: no arbitrary code execution is offered (the runner has no tools, no shell
verb is admitted, the image has no compiler), network egress is the postbox and the broker only, and
every Nest is one tenant's single agent. This is the source plan's "restricted, no-arbitrary-code
profile" for gVisor. Browser/code/terminal tools wait for the microVM worker.

### Migration path to Firecracker

1. A KVM host (bare metal, or a VM with nested virtualization exposed) joins as a worker.
2. Implement `Sandbox` for Firecracker (a jailer-run microVM per Nest, the same OCI image converted to
   a rootfs, the LUKS volume attached as a virtio block device instead of a bind mount, a tap device
   on the same nftables policy). `Volumes` stays LVM+LUKS.
3. Move the ledger to PostgreSQL and use the existing leases (holder + generation fencing) so two
   workers never run the same Nest; add a `worker` column for placement.
4. Migrate a Nest by stop → copy the encrypted LV → start on the new worker (new generation).
   The postbox contract, enrollment, broker tokens and budgets do not change.

## 5. Capacity on this VM (measured 2026-11-03)

**Memory is the binding constraint, and less is available than the VM size suggests.** The VMware
balloon driver holds about **10.2 GiB** of the guest's 15.6 GiB (`vmware-toolbox-cmd stat balloon`:
2,661,116 pages), so the guest reports ~4.8 GiB available with almost nothing running. The provider
can return or take more at will; the owner should ask the provider whether the 15 GiB is reserved.

See §8 for the measured per-Nest numbers and the resulting admission settings.

## 6. Operations

- Units: `nests-pool`, `nests-firewall`, `wg-quick@wg-nests`, `nestd-control`, `nestd-broker`,
  `nestd-mock-upstream`, `nests-backup.timer`. Restarting `nestd-control` leaves Nests running;
  start-up and a 60-second loop reconcile the host with the ledger: orphan or stale-generation
  sandboxes are removed, wanted Nests restarted (a Nest that dies within 2 minutes of starting is
  marked failed instead of looping), paused Nests stay paused, deletions are finished, abandoned
  reservations are settled as platform cost, idle Nests (15 min without model traffic) are stopped.
  Mail to an idle-stopped Nest wakes it (postbox → `POST /v1/nests/{id}/wake`, debounced 10 s).
- Logs: journald capped at 1 GiB/30 days; container logs 1 MB each; no prompt or answer text is
  logged by the broker, the mock or nestd.
- Backups: nightly ledger copy kept 14 days in `/var/backups/nests`. **No independent backup storage
  is defined for Nest volumes yet**; `nests-backup` runs restic (encrypted) when
  `/etc/nests/backup.env` names a repository. Until then a Nest's files exist only on nests-1.
- Hardening kept: key-only SSH, ufw deny-in (+ the three Nest flows), fail2ban, unattended upgrades.
  `runsc` is held at `release-20261102.0` from gVisor's signed repository (key `6F1D F85E 3A71 C249
  18E7 27D5 6FC6 D554 E32B D943`); base images are pinned by digest in the Containerfile.

## 7. What the owner needs to provide

1. **mswarm PP tenant key.** mswarm has no tenant-creation endpoint; tenants and keys live in
   saas_be/MASAAS. Issuing a key needs an mswarm admin JWT (`mswarm_admin` scope) for
   `POST /v1/admin/api-keys/<pp-tenant-id>` with `{label: "pigeonpost-nests", scopes, ip_allowlist:
   ["193.164.5.238"], environment: "production", product_id}`, with `saas_be.api_key_issuance_upstream`
   on so it is durable. Put the returned key in `/etc/nests/upstream.key`, set `upstream_url =
   "https://api.mswarm.org/v1/chat/completions"`, stop the mock. Do not use a trusted
   `MSWARM_API_KEYS` entry: those can act for other tenants.
2. **mswarm gaps to know about:** no per-key model allowlist and no spend ceiling (MASAAS guardrails
   are request counts); callers get token counts but no cost. The broker therefore enforces the model
   and the token budget itself; a tenant-wide monetary cap must be set on the OpenRouter/MASAAS side.
3. **Backup target** for Nest volumes (any Wodo-controlled storage reachable from nests-1), so
   restic can run.
4. **The real RAM reservation** of the VM from the provider (ballooning, §5).

## 8. Phase B (not in Phase A)

Purchase flows (StoreKit, Play, iyzico web) and entitlement from paid packages instead of the flag;
the Products page with Buy an Inbox / Buy Nests / Services on web, desktop and mobile; native and web
Nest management (status header, files, usage); plain conversation (today a Nest answers
`answer_question` request envelopes from the owner's own mailboxes, not free-form chat); file and
archive ingestion; typed tools and approvals; real mswarm tenant and DeepSeek route promotion;
off-host volume backups and restore rehearsal; off-host LUKS unlock; Prometheus metrics; the
Firecracker/KVM worker and PostgreSQL ledger.
