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
```sh
sudo -u nestbuild XDG_RUNTIME_DIR=/run/user/2350 podman network create nestbuild-fetch
# for each pinned image IMG@sha256:DIGEST:
podman pull IMG@sha256:DIGEST
podman save IMG@sha256:DIGEST | sudo -u nestbuild XDG_RUNTIME_DIR=/run/user/2350 podman load
sudo -u nestbuild XDG_RUNTIME_DIR=/run/user/2350 podman image exists IMG@sha256:DIGEST && echo loaded
sudo -u nestbuild XDG_RUNTIME_DIR=/run/user/2350 podman info --format '{{.Host.Security.Rootless}}'
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

## Limits and what is not done yet

- The behaviour test (honeytokens in a running sandbox) does not run on the build plane yet; it is recorded through POST /internal/v1/marketplace/reviews/{review}/stages by the existing tooling.
- The hosted test environment and the staging postbox on this host are not set up by these files.
- Whether the archive's manifest digest survives podman load unchanged on nests-1 must be confirmed on the real hosts (nestd refuses the image if it does not).