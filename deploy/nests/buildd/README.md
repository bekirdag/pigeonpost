# Build and analysis plane (pigeonpost-nest-buildd)

The build and analysis plane is a dedicated service for compiling, scanning, and attesting marketplace releases. It runs on a separate host from nests-1 to isolate the untrusted build process from production workers and internal services. For now it shares a host that also runs other services (payments, identity, model serving) using strict isolation boundaries; later it will move to its own VM without code changes.

## What runs where

| host | what | talks to |
|------|------|----------|
| postbox | submit builds via POST /v1/builds with submit token | build plane (WireGuard, port 8795) |
| nests-1 | pull artifacts via GET /v1/artifacts/{image_digest} with artifact token | build plane (WireGuard, port 8795) |
| build host | pigeonpost-nest-buildd serves API on wg-nestbuild:8795; runs builds in gVisor/podman containers | DNS resolver, fetch allowlist (TCP 443), model route; accepts inbound TCP 8795 from postbox and nests-1 WireGuard addresses only |

## Isolation on a host that already runs Docker

- Dedicated unprivileged user nestbuild (uid 2350), not in docker or sudo groups; rootless podman only (the service refuses rootful podman); no docker.sock access.
- gVisor via wrapper /usr/local/bin/runsc-buildd (systrap platform, sandbox network, no host UDS/FIFO), mapped as runtime runsc in ~nestbuild/.config/containers/containers.conf.
- nftables table inet nestbuild (/etc/nftables.d/nestbuild.nft): output from uid nestbuild limited to DNS on the resolver, TCP 443 to allowlist set fetch_v4, and the model route; everything else dropped including loopback (where Keycloak, saas_be and mswarm publish ports), Docker bridges, other private ranges, and IPv6. Input to TCP 8795 only on wg-nestbuild from postbox and nests-1 WireGuard addresses.
- Allowlist set refreshed by nestbuild-allowlist (from /etc/pigeonpost-buildd/allowlist.txt) every 15 minutes by a timer; starts empty, fails closed.
- Resource caps: /etc/systemd/system/user-2350.slice.d/50-nestbuild-limits.conf (CPUQuota=400%, MemoryMax=8G, TasksMax=4096) covers service and containers; per-container limits memory_mib/cpu_millis/pids in buildd.toml.
- Files: /etc/pigeonpost-buildd (config, token hashes root:nestbuild 0640), /var/lib/pigeonpost-buildd (db, work, artifacts, owned nestbuild 0700). Nothing mounts saas_be, Keycloak or mswarm volumes; systemd unit uses ProtectSystem=strict with only those paths writable.

## How a release is built

Publisher code is untrusted and runs in exactly one place: the **compile step**, inside gVisor.

| step | what runs | runtime | network | writable |
|------|-----------|---------|---------|----------|
| git | `git` (entrypoint set explicitly, so alpine/git's own `ENTRYPOINT ["git"]` never doubles it) | runsc | nestbuild-fetch (allowlisted hosts) | the source dir |
| fetch | `cargo vendor --locked`, `npm ci --ignore-scripts`, `pip download --require-hashes` | runsc | nestbuild-fetch | vendor/ |
| compile | the release's `nest-build.toml` `command` (build scripts, proc macros, its own tools) | runsc | **none** | its copy of the tree at /src only; root read-only, no capabilities, `--timeout` |
| assemble | nothing: `podman build --timestamp=0` of a Containerfile with no `RUN`, `ADD` or `COPY --from` | (none started) | none | — |
| behaviour test | the built image | runsc | none | its volume |

Why the compile is not a `RUN` step in `podman build`: under rootless podman with runsc,
buildah's `RUN` cannot produce a layer. With gVisor's default root overlay the step's writes stay
in the sandbox's own overlay and never reach buildah's layer, so `COPY --from=build` finds
nothing; with `--overlay2=none` the writes go to the host, but the image's root directory is 0555
and containers.conf grants no capabilities, so they are refused. Both were reproduced off-host with
the same runsc release (release-20261102.0) and podman 5.8: `RUN` output missing with the default
overlay; `Permission denied` with `--overlay2=none` (under crun too, so it is the capability, not
gVisor); `--overlay2=none` plus `--cap-add=DAC_OVERRIDE` makes `RUN` work. That last combination
was not adopted: it gives the build container a capability, sends every root write to the host
through the gofer, and keeps untrusted code inside buildah's least-used path. A plain `podman run`
with a bind-mounted tree, the path git, the fetch step and the behaviour test already use, needs
none of that, and the assembly that follows is a file copy: three assemblies of the same files
(one with different mtimes) gave one image digest.

The Containerfile the assembly reads is the one from the source tree, copied outside the tree the
compile step can write. After the compile step, setuid/setgid/sticky bits are cleared and special
files are refused. The toolchain image is one the build plane offers (`fetch_image`,
`node_fetch_image`, `python_fetch_image`, `toolchain_images`); a release cannot name any other
local image, so it cannot compile in another publisher's release image.

## Install on the build host (Ubuntu 24.04 with Docker already installed)

1. apt install podman uidmap passt nftables wireguard sqlite3 ; install gVisor runsc to /usr/local/bin/runsc from the gVisor release (verify its checksum).
```sh
apt install -y podman uidmap passt nftables wireguard sqlite3
# download runsc, verify checksum, then:
install -m 0755 runsc /usr/local/bin/runsc
```

2. useradd --system --create-home --uid 2350 --shell /usr/sbin/nologin nestbuild ; add subordinate ids: usermod --add-subuids 200000-265535 --add-subgids 200000-265535 nestbuild ; loginctl enable-linger nestbuild.
```sh
useradd --system --create-home --uid 2350 --shell /usr/sbin/nologin nestbuild
usermod --add-subuids 200000-265535 --add-subgids 200000-265535 nestbuild
loginctl enable-linger nestbuild
```

3. install -m 0755 runsc-buildd /usr/local/bin/ ; install -d -o nestbuild -g nestbuild -m 0700 /home/nestbuild/.config/containers ; install -o nestbuild -g nestbuild -m 0600 containers.conf /home/nestbuild/.config/containers/containers.conf.
```sh
install -m 0755 runsc-buildd /usr/local/bin/runsc-buildd
install -d -o nestbuild -g nestbuild -m 0700 /home/nestbuild/.config/containers
install -o nestbuild -g nestbuild -m 0600 containers.conf /home/nestbuild/.config/containers/containers.conf
```

4. As nestbuild (sudo -u nestbuild XDG_RUNTIME_DIR=/run/user/2350 ...): podman network create nestbuild-fetch ; load every pinned image into nestbuild's storage: git, the toolchains (rust, node, python, and any `toolchain_images`) and the runtime bases the releases' Containerfiles pin (the reference release's debian:bookworm-slim, the kit's node and python bases). The registry is not on nestbuild's allowlist, so pull them as root and load them; then check each one by its pinned reference, because `--pull=never` finds an image only by that digest. podman info --format '{{.Host.Security.Rootless}}' must print true.
The images production preloads (the same pins as `buildd.toml` and the kit's templates; when a
pin changes there, load the new one here):

| role | image |
|------|-------|
| git | `docker.io/alpine/git:2.47.2@sha256:062a01ad7a0eb17cff382bc5e26086b4d710e56dfdfdf001109a49b6d9bd378c` |
| rust (fetch and toolchain) | `docker.io/library/rust:1.91.1-slim-bookworm@sha256:8514999d4786ef12efe89239e86b3d0a021b94b9d35108c8efe6c79ca7dc1a65` |
| node | `docker.io/library/node:22.21.1-bookworm-slim@sha256:25b3eb23a00590b7499f2a2ce939322727fcce1b15fdd69754fcd09536a3ae2c` |
| python | `docker.io/library/python:3.12.12-slim-bookworm@sha256:593bd06efe90efa80dc4eee3948be7c0fde4134606dd40d8dd8dbcade98e669c` |
| runtime base | `docker.io/library/debian:bookworm-slim@sha256:99c0b25c46d0214d5a2091092e7908907dcaecd6a962cde0ac700acee222b6f3` |

The build host already runs Docker, so pull with root's `docker` (by digest), stream it into
nestbuild's rootless storage, and tag it: an image pulled by digest is saved without a name and
loads as `<none>`, which `podman tag` fixes. Then check every pin.
```sh
sudo -u nestbuild XDG_RUNTIME_DIR=/run/user/2350 podman network create nestbuild-fetch
IMAGES="
docker.io/alpine/git:2.47.2@sha256:062a01ad7a0eb17cff382bc5e26086b4d710e56dfdfdf001109a49b6d9bd378c
docker.io/library/rust:1.91.1-slim-bookworm@sha256:8514999d4786ef12efe89239e86b3d0a021b94b9d35108c8efe6c79ca7dc1a65
docker.io/library/node:22.21.1-bookworm-slim@sha256:25b3eb23a00590b7499f2a2ce939322727fcce1b15fdd69754fcd09536a3ae2c
docker.io/library/python:3.12.12-slim-bookworm@sha256:593bd06efe90efa80dc4eee3948be7c0fde4134606dd40d8dd8dbcade98e669c
docker.io/library/debian:bookworm-slim@sha256:99c0b25c46d0214d5a2091092e7908907dcaecd6a962cde0ac700acee222b6f3
"
NB="sudo -u nestbuild XDG_RUNTIME_DIR=/run/user/2350"
for ref in $IMAGES; do
  name="${ref%@*}"                 # e.g. docker.io/library/rust:1.91.1-slim-bookworm
  repo="${name%:*}"                # e.g. docker.io/library/rust
  byd="$repo@${ref#*@}"            # e.g. docker.io/library/rust@sha256:8514...
  docker pull "$byd"
  id=$(docker image inspect --format '{{.Id}}' "$byd")
  docker save "$byd" | $NB podman load
  # pulled by digest, the image loads without a name: tag it by its image id
  $NB podman tag "$id" "$name"
done
# check: every pin must be found by the exact reference the build plane uses (--pull=never)
for ref in $IMAGES; do
  $NB podman image exists "$ref" && echo "ok      $ref" || echo "MISSING $ref"
done
$NB podman info --format '{{.Host.Security.Rootless}}'   # must print true
```

5. install -d -m 0750 -o root -g nestbuild /etc/pigeonpost-buildd ; copy buildd.toml.example to /etc/pigeonpost-buildd/buildd.toml and fill the placeholders; copy allowlist.txt.example to allowlist.txt.
```sh
install -d -m 0750 -o root -g nestbuild /etc/pigeonpost-buildd
cp buildd.toml.example /etc/pigeonpost-buildd/buildd.toml
# edit /etc/pigeonpost-buildd/buildd.toml to fill placeholders
cp allowlist.txt.example /etc/pigeonpost-buildd/allowlist.txt
```

6. Tokens: generate two different tokens on the machines that will hold them (openssl rand -hex 32): the submit token on the postbox host, the artifact token on nests-1. Copy only their hashes here: pigeonpost-nest-buildd hash-token < token > /etc/pigeonpost-buildd/submit-token.sha256 (and artifact-token.sha256); chown root:nestbuild, chmod 0640.
```sh
# On postbox host: openssl rand -hex 32 > submit.token ; then copy hash here
pigeonpost-nest-buildd hash-token < /path/to/submit.token > /etc/pigeonpost-buildd/submit-token.sha256
# On nests-1: openssl rand -hex 32 > artifact.token ; then copy hash here
pigeonpost-nest-buildd hash-token < /path/to/artifact.token > /etc/pigeonpost-buildd/artifact-token.sha256
chown root:nestbuild /etc/pigeonpost-buildd/submit-token.sha256 /etc/pigeonpost-buildd/artifact-token.sha256
chmod 0640 /etc/pigeonpost-buildd/submit-token.sha256 /etc/pigeonpost-buildd/artifact-token.sha256
```

7. install -d -o nestbuild -g nestbuild -m 0700 /var/lib/pigeonpost-buildd.
```sh
install -d -o nestbuild -g nestbuild -m 0700 /var/lib/pigeonpost-buildd
```

8. WireGuard: wg-nestbuild.conf.example -> /etc/wireguard/wg-nestbuild.conf; systemctl enable --now wg-quick@wg-nestbuild ; open the UDP port in ufw from the postbox host and nests-1 public addresses only. If the build host is behind NAT (raijin is), reverse it: the postbox host and nests-1 listen (ufw allows the build host's egress address only) and the build host dials out to both with `Endpoint` and `PersistentKeepalive = 25`.
```sh
install -m 0644 wg-nestbuild.conf.example /etc/wireguard/wg-nestbuild.conf
# edit /etc/wireguard/wg-nestbuild.conf with peer keys and endpoints
systemctl enable --now wg-quick@wg-nestbuild
ufw allow from <POSTBOX_PUBLIC_ADDR> to any port <WG_UDP_PORT> proto udp
ufw allow from <NESTS1_PUBLIC_ADDR> to any port <WG_UDP_PORT> proto udp
```

9. nftables: install -m 0644 nestbuild.nft /etc/nftables.d/ ; fill the resolver (include the systemd-resolved stub 127.0.0.53 if /etc/resolv.conf points at it), model route and caller sets ; check and load it with nft ; load it at boot with its own unit, nestbuild-nft.service. **Do not** add an include to /etc/nftables.conf or enable nftables.service on a Docker host: that file starts with `flush ruleset` and would wipe Docker's rules. install -m 0755 nestbuild-allowlist /usr/local/sbin/ ; install the .service/.timer ; systemctl enable --now nestbuild-allowlist.timer (every minute; each name resolved three times; public CIDR lines such as GitHub's published ranges are taken as they are) ; run it once and check nft list set inet nestbuild fetch_v4.
```sh
install -m 0644 nestbuild.nft /etc/nftables.d/nestbuild.nft
# edit /etc/nftables.d/nestbuild.nft to fill resolver, model route, caller sets
nft -c -f /etc/nftables.d/nestbuild.nft && nft -f /etc/nftables.d/nestbuild.nft
install -m 0644 nestbuild-nft.service /etc/systemd/system/ && systemctl enable nestbuild-nft.service
install -m 0755 nestbuild-allowlist /usr/local/sbin/nestbuild-allowlist
# install nestbuild-allowlist.service and .timer into /etc/systemd/system/
systemctl daemon-reload && systemctl enable --now nestbuild-allowlist.timer
nestbuild-allowlist
nft list set inet nestbuild fetch_v4
```

10. Limits: install -D -m 0644 50-nestbuild-limits.conf /etc/systemd/system/user-2350.slice.d/50-nestbuild-limits.conf ; systemctl daemon-reload.
```sh
install -D -m 0644 50-nestbuild-limits.conf /etc/systemd/system/user-2350.slice.d/50-nestbuild-limits.conf
systemctl daemon-reload
```

11. Binary: build with cargo build --release -p pigeonpost-nests --bin pigeonpost-nest-buildd on a build machine; install -m 0755 to /usr/local/bin/. pigeonpost-nest-buildd check --config /etc/pigeonpost-buildd/buildd.toml.
```sh
# On a build machine:
cargo build --release -p pigeonpost-nests --bin pigeonpost-nest-buildd
install -m 0755 target/release/pigeonpost-nest-buildd /usr/local/bin/
pigeonpost-nest-buildd check --config /etc/pigeonpost-buildd/buildd.toml
```

12. install pigeonpost-nest-buildd.service to /etc/systemd/system/ ; systemctl enable --now pigeonpost-nest-buildd ; journalctl -u pigeonpost-nest-buildd.
```sh
install -m 0644 pigeonpost-nest-buildd.service /etc/systemd/system/pigeonpost-nest-buildd.service
systemctl enable --now pigeonpost-nest-buildd
journalctl -u pigeonpost-nest-buildd
```

Moving to a dedicated VM later: repeat these steps there, change the WireGuard endpoint on the postbox and nests-1, and change POSTBOX_MARKETPLACE_BUILDD_URL and nestd's artifacts.url. No code changes.

## Connect the postbox

Set these environment variables in the postbox service configuration:

```sh
POSTBOX_MARKETPLACE_BUILDD_URL=http://<BUILD_WG_ADDR>:8795
POSTBOX_MARKETPLACE_BUILDD_TOKEN_FILE=/etc/pigeonpost/buildd-submit.token
POSTBOX_MARKETPLACE_ATTESTATION_KEY_FILE=/etc/pigeonpost/attestation.key
POSTBOX_MARKETPLACE_ATTESTATION_TRUSTED_KEYS=<comma-separated public keys of earlier keys, during rotation>
POSTBOX_MARKETPLACE_BUILDS_PER_DAY=5
POSTBOX_MARKETPLACE_MAX_WORKERS=20
POSTBOX_MARKETPLACE_MONTHLY_BROKER_TOKENS=2000000
POSTBOX_MARKETPLACE_JOBS_PER_MINUTE=10
```

In production `<BUILD_WG_ADDR>` is `10.71.0.1` (the `wg-nestbuild` tunnel is `10.71.0.0/29`:
build host `.1`, postbox host `.2`, nests-1 `.3`; never `10.69.0.0/24`, the nests-mp pool on
nests-1), so `POSTBOX_MARKETPLACE_BUILDD_URL=http://10.71.0.1:8795`. The billing bridge, on the
same build host, is reached over the same tunnel: `POSTBOX_BRIDGE_URL=http://10.71.0.1:8787`.
The build host is behind NAT, so it is the side that dials out (step 8).

The postbox container runs as the image's nonroot user (uid 65532): mount the token and key files
read-only (`:ro`) and make them owned by uid 65532, or the postbox cannot read them.

The submit token file must be 0600 and contain the raw submit token. The attestation key file must be 0600 and contain a 64-hex-digit Ed25519 seed (generate with openssl rand -hex 32; keep an escrow copy offline). Staff can override quotas per publisher with PUT /v1/staff/marketplace/publishers/{publisher}/quotas. The postbox logs the key id (ppk_...) at start. Print the public key for nestd with `pigeonpost-nest-buildd public-key < /etc/pigeonpost/attestation.key` (it reads the seed on stdin and prints the public key and its key id; run it on the postbox host, the seed never leaves it). nestd logs the key ids it trusts at start; they must match.

## Connect nests-1

In nestd.toml, section [sandbox.marketplace]:

```toml
[sandbox.marketplace]
attestation_keys = ["<64-hex public key>"]
worker_gib = 1

[sandbox.marketplace.artifacts]
url = "http://<BUILD_WG_ADDR>:8795"
token_file = "/etc/nests/buildd-artifact.token"
cache_dir = "/var/lib/nests/artifacts"
max_bytes = 4294967296
```

attestation_keys is required; list more than one during rotation. token_file must be 0400 root and contain the raw artifact token. storage.backend must be lvm-luks (worker volumes are LUKS2 thin volumes with KEK-wrapped keys, locked until the postbox unlocks the host, taken by nests-backup, destroyed by tombstone replay).

nests-1 must be allowed out to <BUILD_WG_ADDR>:8795 over wg-nestbuild only. Add an ufw rule or nftables rule on nests-1 permitting outbound TCP 8795 to <BUILD_WG_ADDR> via the wg-nestbuild interface.

## Signing key and rotation

1. Generate a new seed on the postbox host: openssl rand -hex 32 > /etc/pigeonpost/attestation-new.key ; chmod 0600.
2. Add its public key to attestation_keys on every worker host (nestd.toml) and restart nestd.
3. Switch POSTBOX_MARKETPLACE_ATTESTATION_KEY_FILE to the new seed file and put the old public key in POSTBOX_MARKETPLACE_ATTESTATION_TRUSTED_KEYS; restart the postbox.
4. Re-attest live releases (a new staff approval) so their newest attestation uses the new key.
5. Remove the old key from nestd attestation_keys and from the postbox trusted keys list.

A compromised key: remove it everywhere at once; every release attested with it stops at the next dispatch until re-attested.

## Check it works

```sh
# From the postbox host, must print ok:
curl -s http://<BUILD_WG_ADDR>:8795/healthz

# As nestbuild, gVisor container works (the entrypoint is set, as the build plane sets it):
sudo -u nestbuild XDG_RUNTIME_DIR=/run/user/2350 podman run --rm --runtime=runsc --network=none '--entrypoint=["git"]' <GIT_IMAGE>@sha256:<DIGEST> --version

# As nestbuild, the compile step's shape: gVisor, no network, read-only root, a writable bind
# mount. Must print "hi" from the host side:
d=$(sudo -u nestbuild mktemp -d) && sudo -u nestbuild XDG_RUNTIME_DIR=/run/user/2350 podman run --rm --pull=never --runtime=runsc --network=none --cap-drop=ALL --security-opt=no-new-privileges --read-only --tmpfs=/tmp:rw,nosuid,nodev,size=64m --volume=$d:/src:rw --workdir=/src '--entrypoint=["sh"]' <GIT_IMAGE>@sha256:<DIGEST> -c 'echo hi > /src/x' && sudo -u nestbuild cat $d/x

# As nestbuild, non-allowlisted HTTPS must fail:
sudo -u nestbuild XDG_RUNTIME_DIR=/run/user/2350 curl -m 5 https://example.com

# As nestbuild, loopback access must fail:
sudo -u nestbuild XDG_RUNTIME_DIR=/run/user/2350 curl -m 5 http://127.0.0.1:<any local service port>

# Verify nftables counters increment:
nft list table inet nestbuild
```

The live test builds the reference release (fetch, compile in gVisor, assembly) as nestbuild: NESTS_LIVE_BUILD=1 NESTS_LIVE_FETCH_IMAGE=<RUST_IMAGE>@sha256:<DIGEST> NESTS_LIVE_FETCH_NETWORK=nestbuild-fetch cargo test -p pigeonpost-nests --test marketplace_m1 -- --ignored live_build (see that test's docs; set NESTS_LIVE_TOOLCHAIN_IMAGE when the reference's toolchain is not the fetch image). Run it twice: the two `image_digest` values must be equal.

## Automated security checks (developer program plan §4)

The deterministic scan and the behaviour test carry the review's security tools. Every tool runs
here, as nestbuild, in a gVisor container with `--network=none`, `--cap-drop=ALL`, a read-only
root and read-only mounts (only its own `/out` is writable). The databases are fetched by a second
user, `nestscan`, with its own nftables allowlist, and are only ever read by a scan.

| Check | Tool (pinned) | Scans | Blocks |
|---|---|---|---|
| Dependency CVEs | OSV-Scanner 2.7.0 (`--offline-vulnerabilities`, platform `--config`) | lockfile and SBOM | critical/high with a fix (an ungraded one with a fix counts as high) |
| Image CVEs | Grype 0.121.0 | the saved OCI archive | critical/high with a fix |
| Malware | ClamAV 1.4.6 LTS + YARA 4.5.2 | source, compiled context, the image's layers (unpacked inside gVisor) | any hit, and anything ClamAV cannot read in full (`--alert-exceeds-max/-encrypted/-broken`) or YARA reports as an error or timeout |
| SAST | Semgrep 1.181.0, vendored `scanners/semgrep-rules.yml`, `--disable-nosem`, no size or time cut-off | source (copy without publisher config) | `ERROR` + `confidence: HIGH`; any error or skipped file |
| Secrets | gitleaks 8.30.1 (`--redact`, platform `--config`, no ignore file, `--ignore-gitleaks-allow`) on top of the built-in rules | source (copy without publisher config) | any hit |
| Obfuscation | in-process heuristics (entropy, packed JS, base64 blobs, binaries) | source | never |
| Egress | gVisor strace (`runsc-behaviour`) | the behaviour run | any attempt but the broker, DNS included; a missing boot log or a trace over 64 MiB (`egress.flood`) |
| Buyer data | random per-job canaries (no fixed prefix) | the run's results and both output streams, whatever the exit; raw, hex, URL-encoded, base64, split across lines | a canary outside its job |
| Resources | the user manager's cgroup, CPU counted to the end of the run, answers timed when complete | the behaviour run | more than 2x the pool budget |

A tool that cannot run, a ruleset whose digest is not the pinned one, or a database missing or
older than `max_db_age_hours` is a blocking `scanner.*` finding (fail closed); its id is unique to
the run, so a re-scan can never take a failure for a finding already answered.

**Publisher-owned scanner configuration is never used.** gitleaks, OSV-Scanner and Semgrep scan
a copy of the source without `.gitleaks.toml`, `gitleaks.toml`, `.gitleaksignore`,
`osv-scanner.toml`, `.semgrepignore`, `.semgrep*` and `.gitignore` (each one found is an advisory
`evasion.scanner_config` finding for staff); each tool gets the platform's own configuration
(compiled into buildd) and has its inline opt-outs switched off. Each finding is
`{id, severity, tool, rule, path, package, message, blocking}`; staff see all of it, the publisher
sees the postbox's redaction (malware signature and rule names, trace destinations and canaries
stay with staff).

Why Grype and not Trivy: Grype's database is one archive from one host (`grype.anchore.io`), with
its checksum in the listing it fetches, imported offline into a directory we mount read-only; it
reports `fix.state` per match, which is exactly the blocking rule; and the same binary scans an
OCI archive or a CycloneDX SBOM (the monthly re-scan). Trivy's database is an OCI artifact on
ghcr.io, which would put GitHub's container registry and its CDN on the updater's allowlist, and
it bundles secret and misconfiguration scanners this plane already runs separately.

The same queue scans for the postbox: `POST /v1/scans` (submit token) with `kind: files` (a
publisher-hosted service's result files: ClamAV and YARA) or `kind: release` (the monthly
re-scan: OSV and Grype on the SBOM, and Grype, ClamAV and YARA on the image while it is here),
then `GET /v1/scans/{scan_id}`. Builds run first; a scan running when buildd stops runs again at
start. An image no re-scan has named yet is kept for `keep_unscanned_days` (120); one a re-scan
has named is kept while re-scans keep naming it, then `keep_days` after its last. A re-scan of an
image that is gone is a blocking `scanner.image_unavailable` finding (SBOM-only).

### Unannounced checks of publisher-hosted services

The announced conformance run tells a service it is being reviewed. Two checks it cannot tell
from real use (the postbox, `marketplace_shadow`):

- **Shadow jobs**: ordinary hosted jobs (real listing id, normal job id, the plain contract
  message, realistic varied input) from the platform owner's account, with no charge to anyone
  and no delivery. Staff start one with `POST /v1/staff/marketplace/listings/{listing}/shadow-jobs`
  (step-up, no stake); `POSTBOX_MARKETPLACE_SHADOW_DAYS` (30; `0` off) also schedules one per
  published publisher-hosted listing at a random time, about once per that many days. Its result
  gets the conformance checks and ClamAV/YARA here. A failure flags the release; a malware hit
  also stops the listing (the kill switch, actor `system:analysis`).
- **Sampled buyer results**: `POSTBOX_MARKETPLACE_RESULT_SAMPLE_PERCENT` (5; `0` off) of real
  succeeded results go to `POST /v1/scans` after they are recorded (the buyer's delivery does not
  wait). The files stay on this host only until the scan ends. A hit stops the listing and flags
  it.

Staff see every flag (re-scan, shadow job, result sample) at
`GET /v1/staff/marketplace/security-flags`, acknowledge with
`POST /v1/staff/marketplace/security-flags/{flag}/acknowledge`, and lift a stop with the
existing kill route (`action: restore`). Keep the shadow schedule, its input pool and the sample
rate out of publisher-facing text beyond the policy line that such checks exist.

### Deploy on raijin (operator steps; nothing here is automatic)

Packages and users (as root):

```sh
apt install -y unzip python3 curl          # the updater's own needs; podman/runsc are already there
useradd --system --create-home --uid 2351 --shell /usr/sbin/nologin nestscan
usermod --add-subuids 300000-365535 --add-subgids 300000-365535 nestscan
loginctl enable-linger nestscan
install -m 0755 deploy/nests/buildd/runsc-behaviour /usr/local/bin/runsc-behaviour
install -m 0755 deploy/nests/buildd/nestscan-update /usr/local/sbin/nestscan-update
install -m 0755 deploy/nests/buildd/nestbuild-allowlist /usr/local/sbin/nestbuild-allowlist
install -m 0644 deploy/nests/buildd/nestbuild-tmpfiles.conf /etc/tmpfiles.d/nestbuild.conf
systemd-tmpfiles --create /etc/tmpfiles.d/nestbuild.conf
# The behaviour traces on a size-limited tmpfs, never raijin's shared disk:
echo 'tmpfs /var/lib/pigeonpost-buildd/netlog tmpfs size=512m,mode=0700,uid=2350,gid=2350,nosuid,nodev,noexec 0 0' >> /etc/fstab
mount /var/lib/pigeonpost-buildd/netlog && df -h /var/lib/pigeonpost-buildd/netlog
install -o root -g nestbuild -m 0640 deploy/nests/buildd/scanners/semgrep-rules.yml /etc/pigeonpost-buildd/semgrep-rules.yml
install -o root -g nestscan -m 0640 deploy/nests/buildd/scanners/pigeonpost.yar /etc/pigeonpost-buildd/pigeonpost.yar
install -o root -g nestscan -m 0640 deploy/nests/buildd/scanners/freshclam.conf /etc/pigeonpost-buildd/freshclam.conf
install -o root -g nestscan -m 0640 deploy/nests/buildd/scandb.env.example /etc/pigeonpost-buildd/scandb.env
install -m 0644 deploy/nests/buildd/scandb-allowlist.txt.example /etc/pigeonpost-buildd/scandb-allowlist.txt
# the same containers.conf (runsc and runsc-behaviour) for both users
install -o nestbuild -g nestbuild -m 0644 -D deploy/nests/buildd/containers.conf ~nestbuild/.config/containers/containers.conf
install -o nestscan -g nestscan -m 0644 -D deploy/nests/buildd/containers.conf ~nestscan/.config/containers/containers.conf
sha256sum /etc/pigeonpost-buildd/semgrep-rules.yml   # must be the semgrep_rules_sha256 in buildd.toml
```

Images (build once with the network, then load into both users' storage; never pulled at scan
time):

```sh
podman build --pull=always -t localhost/pigeonpost-scanners:2026.11 \
  -f deploy/nests/buildd/scanners/Containerfile deploy/nests/buildd/scanners
podman pull docker.io/clamav/clamav:1.4.6_base@sha256:109477fb9dedf7b4542d0fc7be7f8ef73a55d9d731e9568821347825219407aa
podman pull docker.io/semgrep/semgrep:1.181.0@sha256:81474464c1f07483ba0682a44787fd93c91397419762a04abe2bafaae396304b
podman save -o /tmp/scanners.tar localhost/pigeonpost-scanners:2026.11
podman save -o /tmp/clamav.tar docker.io/clamav/clamav:1.4.6_base
podman save -o /tmp/semgrep.tar docker.io/semgrep/semgrep:1.181.0
for u in nestbuild:2350 nestscan:2351; do
  user=${u%%:*}; uid=${u##*:}
  for t in scanners clamav $( [ "$user" = nestbuild ] && echo semgrep ); do
    sudo -u "$user" XDG_RUNTIME_DIR=/run/user/$uid podman load -i /tmp/$t.tar
  done
done
sudo -u nestbuild XDG_RUNTIME_DIR=/run/user/2350 podman image inspect --format '{{.Digest}}' localhost/pigeonpost-scanners:2026.11
# put that digest in buildd.toml [scanners] image and in scandb.env SCANNER_IMAGE (both
# "localhost/pigeonpost-scanners:2026.11@sha256:<digest>"); rm /tmp/{scanners,clamav,semgrep}.tar
```

nftables (the new `scandb_v4` set and `nestscan_out` chain; the user must exist first, or nft
refuses the file):

```sh
cp deploy/nests/buildd/nestbuild.nft /etc/nftables.d/nestbuild.nft   # re-fill the resolver, model, caller sets and <BUILD_WG_ADDR> as before
nft -c -f /etc/nftables.d/nestbuild.nft && nft -f /etc/nftables.d/nestbuild.nft
sudo -u nestscan XDG_RUNTIME_DIR=/run/user/2351 podman network create nestscan-update
```

Timers (the updater every 6 hours, its allowlist every minute):

```sh
install -m 0644 deploy/nests/buildd/nestscan-update.service deploy/nests/buildd/nestscan-update.timer \
  deploy/nests/buildd/nestscan-allowlist.service deploy/nests/buildd/nestscan-allowlist.timer /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now nestscan-allowlist.timer
systemctl start nestscan-allowlist.service && nft list set inet nestbuild scandb_v4
systemctl start nestscan-update.service      # the first run takes ~10 minutes
journalctl -u nestscan-update -n 50
cat /var/lib/pigeonpost-scandb/current/manifest.json
systemctl enable --now nestscan-update.timer
```

buildd.toml: add `[scanners]`, `[behaviour]` and `[behaviour.budget]` from buildd.toml.example
(the budget is the marketplace pool's per-Nest limits from nestd.toml), add
`/var/lib/pigeonpost-scandb` read-only to the service (`ReadOnlyPaths=` is implied by
`ProtectSystem=strict`; nothing else to change), then:

```sh
sudo -u nestbuild pigeonpost-nest-buildd check --config /etc/pigeonpost-buildd/buildd.toml  # "ok"; lists any stale database
systemctl restart pigeonpost-nest-buildd
```

On the postbox: `POSTBOX_MARKETPLACE_RESCAN_DAYS=30` (the default; `0` switches the monthly
re-scan off), `POSTBOX_MARKETPLACE_SHADOW_DAYS=30` and `POSTBOX_MARKETPLACE_RESULT_SAMPLE_PERCENT=5`
(the defaults). The conformance run needs no setting beyond the build plane already configured.

### Check it works

```sh
U="sudo -u nestbuild XDG_RUNTIME_DIR=/run/user/2350"
# A scan has no network (must fail):
$U podman run --rm --pull=never --runtime=runsc --network=none --entrypoint=grype <SCANNER_IMAGE> db check
# The updater cannot leave its allowlist (must fail), and nestbuild cannot reach the database hosts:
sudo -u nestscan curl -m 5 https://example.com ; $U curl -m 5 https://grype.anchore.io
# ClamAV and YARA see the EICAR test file (both must report a hit):
d=$($U mktemp -d) && printf '%s' 'X5O!P%@AP[4\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*' | $U tee $d/eicar >/dev/null
$U podman run --rm --pull=never --runtime=runsc --network=none --read-only --volume=$d:/scan:ro \
  --volume=/var/lib/pigeonpost-scandb/current/clamav:/db:ro --entrypoint=clamscan <CLAMAV_IMAGE> --database=/db --infected /scan
$U podman run --rm --pull=never --runtime=runsc --network=none --read-only --volume=$d:/scan:ro \
  --volume=/var/lib/pigeonpost-scandb/current/yara:/db:ro --entrypoint=yara <SCANNER_IMAGE> -C -r /db/rules.yarc /scan
# The egress trace: a connect from inside appears in the trace directory, then is parsed:
$U podman run --rm --pull=never --runtime=runsc-behaviour --network=none --cidfile=$d/cid \
  '--entrypoint=["sh"]' <GIT_IMAGE>@sha256:<DIGEST> -c 'wget -T2 http://93.184.216.34/ || true'
grep -h 'connect(' /var/lib/pigeonpost-buildd/netlog/$(cat $d/cid)/* | head   # Family: AF_INET, Addr: 93.184.216.34, Port: 80
$U rm -rf $d /var/lib/pigeonpost-buildd/netlog/*
```

If the trace has no `connect(` line, the strace format or flags differ in this runsc release:
do not enable `[behaviour]` until the egress unit tests' fixture
(`crates/pigeonpost-nests/tests/fixtures/scanners/strace.txt`) matches what this host prints.

### Disk, CPU and network

| What | Where | Size |
|---|---|---|
| OSV archives (crates.io, npm, PyPI) | `/var/lib/pigeonpost-scandb` | ~0.4 GB |
| Grype database (v6) | same | ~1.5–2 GB |
| ClamAV signatures (main, daily, bytecode) | same | ~0.35 GB |
| YARA rules, compiled | same | < 0.1 GB |
| one snapshot | | ~3 GB |
| peak (current + previous + the one being built) | | ~9 GB: keep 12 GB free |
| images: scanners ~0.3 GB, ClamAV ~0.2 GB, Semgrep ~0.8 GB | nestbuild's storage (scanners + ClamAV also in nestscan's) | ~1.8 GB |
| retained images of published releases | `/var/lib/pigeonpost-buildd/artifacts` | one archive per published release (0.1–0.5 GB each) |
| behaviour traces | `/var/lib/pigeonpost-buildd/netlog` (tmpfs, 512 MiB) | a few MB per run, read up to 64 MiB, deleted after it (tmpfiles: 1 day) |
| unpacked image layers | the build's work dir, during the scan | about the image's uncompressed size, deleted with the build |

Per build, the tools add roughly 2–4 minutes of CPU inside the existing slice (ClamAV loading its
signatures is the largest part, ~30 s; Grype on the image 10–30 s; Semgrep 10–60 s) and ClamAV
peaks at ~1.5–2 GiB of memory (the per-tool limit is 4 GiB, under the slice's 8 GiB). The
updater downloads ~0.5 GB per run for OSV and the Grype database (ClamAV fetches diffs), about
2 GB a day at four runs; it runs at `Nice=10` with idle I/O. The monthly re-scan costs about one
minute per published release.

## Limits and what is not done yet

- Integration conformance runs and re-scans need this build plane: without it a review job's result is answered `503` (the service retries) and fails at its deadline, and no re-scan runs.
- YARA reads image layers as they are in the archive (`podman save` stores them uncompressed); a compressed layer is covered by ClamAV, which unpacks archives, not by YARA.
- The hosted test environment and the staging postbox on this host are not set up by these files.
- Whether the archive's manifest digest survives podman load unchanged on nests-1 must be confirmed on the real hosts (nestd refuses the image if it does not).