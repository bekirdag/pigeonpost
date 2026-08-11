# Deploying Pigeonpost

Status: deployment runbook and release contract. This file does not attest that an image has been
published, any service is deployed, witnesses are independently operated, or custody and regulatory
gates are active.

Pigeonpost uses one image containing the loft, registry, and directory roles. A conforming release
workflow publishes that image for Linux arm64 and x64, scans the exact multi-architecture digest,
and emits `pigeonpost-container.txt` so an operator never has to trust a mutable tag.

The official image is built and run on digest-pinned Debian 13 (Trixie), whose current stable
support horizon is materially longer than Bullseye's 2026-08-31 LTS end. Bullseye remains only the
oldest container in which the separately published static musl assets are executed. This does not
support Docker 18.09-era production hosts: `preflight.sh` requires modern Compose, Buildx,
exact-digest, and provenance verification, and operators must upgrade or replace a host that cannot
satisfy those checks without weakening seccomp.

The three Compose files have different purposes:

| File | Purpose |
| --- | --- |
| `compose.loft.yml` | Build and run one private host-loopback loft for local development |
| `compose.core.yml` | Build the three-role operator topology; uses the same six preprovisioned host paths and external gates as production |
| `compose.prod.yml` | Run the released image by its exact GHCR digest; never builds |

All published host ports bind only to loopback. The development loft keeps the Pigeonpost process
on container loopback and uses the pinned, log-discarding `Caddyfile.local` sidecar in the same
network namespace; this preserves the production listener guard without exposing the port publicly.
The core and production topologies bind inside their containers but refuse to start until their
production compliance and witness configuration is complete. Put production services behind the
host's existing TLS reverse proxy.
The `loft.Caddyfile` emitted by `pigeonpost install --domain ...` is the privacy-safe reference:
HTTP access logging is absent, Caddy's admin endpoint is disabled, and its global runtime logger is
discarded so source addresses cannot escape the sealed trace store. Do not add Caddy `log` or
global `debug` directives, and do not enable proxy or platform access logs. Any replacement proxy
must pass the successful, malformed-request, and failed-upstream capture gate in
[`acceptance/proxy-privacy.sh`](acceptance/proxy-privacy.sh).

## Local release acceptance

Before creating a release tag, run the isolated binary, adversarial source, and database rollback
gates in [`acceptance/README.md`](acceptance/README.md):

```bash
./deploy/acceptance/local.sh
```

After downloading and verifying a release asset, repeat its black-box scenarios with
`PIGEONPOST_BIN=/absolute/path/to/pigeonpost-<platform> --binary-only`. The harness creates its own
agent homes and private service state, never an existing Pigeonpost directory. The binary gate also
drives two real MCP stdio hosts through initialization, tool discovery, delivery, an adversarial
fenced read, and acknowledgement. It then
starts a witnessed registry, directory prober, and two lofts, commits both submissions, exercises
signed-directory bootstrap refusing `NoLofts` for local HTTP, and proves delivery remains available when
one of the two configured lofts is offline.

## Production preflight

Download the container reference, checksum manifest, and the binary used for the provenance check
from the intended GitHub release. Verify only the artifacts actually downloaded; a full
`sha256sum -c SHA256SUMS` requires downloading every file named by the manifest:

```bash
export PIGEONPOST_RELEASE='v0.2.0'
mkdir "pigeonpost-$PIGEONPOST_RELEASE"
gh release download "$PIGEONPOST_RELEASE" --repo bekirdag/pigeonpost \
  --pattern SHA256SUMS \
  --pattern pigeonpost-container.txt \
  --pattern pigeonpost-linux-x64 \
  --dir "pigeonpost-$PIGEONPOST_RELEASE"
cd "pigeonpost-$PIGEONPOST_RELEASE"
awk '$2 == "pigeonpost-container.txt" || $2 == "pigeonpost-linux-x64"' SHA256SUMS \
  | sha256sum -c -
test "$(gh release view "$PIGEONPOST_RELEASE" --repo bekirdag/pigeonpost \
  --json isImmutable --jq .isImmutable)" = true
gh release verify "$PIGEONPOST_RELEASE" --repo bekirdag/pigeonpost
export PIGEONPOST_SOURCE_SHA="$(gh api \
  "repos/bekirdag/pigeonpost/commits/$PIGEONPOST_RELEASE" --jq .sha)"
for artifact in pigeonpost-container.txt pigeonpost-linux-x64; do
  gh attestation verify "$artifact" \
    --repo bekirdag/pigeonpost \
    --signer-workflow github.com/bekirdag/pigeonpost/.github/workflows/release.yml \
    --source-ref "refs/tags/$PIGEONPOST_RELEASE" \
    --source-digest "$PIGEONPOST_SOURCE_SHA" \
    --deny-self-hosted-runners
done
gh attestation verify pigeonpost-linux-x64 \
  --repo bekirdag/pigeonpost \
  --predicate-type https://spdx.dev/Document/v2.3 \
  --signer-workflow github.com/bekirdag/pigeonpost/.github/workflows/release.yml \
  --source-ref "refs/tags/$PIGEONPOST_RELEASE" \
  --source-digest "$PIGEONPOST_SOURCE_SHA" \
  --deny-self-hosted-runners

export PIGEONPOST_IMAGE="$(cat pigeonpost-container.txt)"
export PIGEONPOST_ORIGIN='pigeonpost.dev/registry'
export PIGEONPOST_REGISTRY_DATA_HOST_PATH='/srv/pigeonpost-registry'
export PIGEONPOST_DIRECTORY_DATA_HOST_PATH='/srv/pigeonpost-directory'
export PIGEONPOST_LOFT_DATA_HOST_PATH='/srv/pigeonpost-loft'
export PIGEONPOST_LOFT_NETWORK_TRACE_HOST_PATH='/srv/pigeonpost-traces/loft-network'
export PIGEONPOST_REGISTRY_NETWORK_TRACE_HOST_PATH='/srv/pigeonpost-traces/registry-network'
export PIGEONPOST_REGISTRY_IDENTITY_TRACE_HOST_PATH='/srv/pigeonpost-traces/registry-identity'
sudo install -d -m 0700 -o 10001 -g 10001 \
  "$PIGEONPOST_REGISTRY_DATA_HOST_PATH" \
  "$PIGEONPOST_DIRECTORY_DATA_HOST_PATH" \
  "$PIGEONPOST_LOFT_DATA_HOST_PATH" \
  "$PIGEONPOST_LOFT_NETWORK_TRACE_HOST_PATH" \
  "$PIGEONPOST_REGISTRY_NETWORK_TRACE_HOST_PATH" \
  "$PIGEONPOST_REGISTRY_IDENTITY_TRACE_HOST_PATH"
# If GitHub identity claims are enabled, provision the owner-only file described in
# docs/identity-providers.md before setting these public/configuration values:
export PIGEONPOST_GITHUB_CLIENT_ID='your-github-oauth-client-id'
export PIGEONPOST_GITHUB_CLIENT_SECRET_FILE='/srv/pigeonpost/secrets/github-client-secret'
../deploy/preflight.sh
```

`preflight.sh` rejects non-release tags, non-official image names, malformed digests, partial GitHub
identity configuration, an insecure direct provider-secret environment value, an unsafe provider
secret file, and test-only mock identities. It captures the provider configuration before its first
subprocess and then removes every provider variable from the environment inherited by provenance,
checksum, image, and other unrelated tools. Only the final Compose render receives the public client
IDs and owner-only secret-file path; the secret value is never placed in a subprocess environment.
The preflight independently resolves the release tag, requires the exact OCI digest's
hosted-workflow provenance from `release.yml`, checks that the remote image exists, and renders the
production Compose configuration. It also requires six canonical, pairwise-distinct,
non-nested, preprovisioned private directories—the three role databases plus three trace
purposes—owned by UID 10001 with mode `0700`.
Never substitute `latest`, a version tag, or a locally built image for `PIGEONPOST_IMAGE`.

The six illustrative `/srv` paths do not by themselves create quota domains. Before preflight,
place each path in its required quota scope and record the
configured quota plus current free-space evidence. Compose and `preflight.sh` can verify path
separation, ownership, mode, and mount intent; they cannot prove host-side quota enforcement.

Before changing a running service:

- Record the currently running digest; that is the rollback target.
- Take and restore-test volume backups using the host's normal backup system.
- Copy the registry `checkpoint.key` into the documented offline backup location.
- Confirm registry data and each trace purpose have their documented physical quotas and headroom,
  with every trace purpose separate from the SQLite role mounts, and that the reverse proxy still
  owns ports 80 and 443.
- Confirm all compliance witnesses, identity providers, and custody operators required by the
  selected policy are reachable and have completed their non-code setup.
- Follow the release-specific [v0.2.0 migration and rollback note](../docs/migrations/v0.2.0.md),
  including its stopped-writer, WAL-aware backup, restore verification, and role ordering.

The registry checkpoint key is irreplaceable. Loft data expires by design, but losing the checkpoint
key breaks continuity for every witness following that registry.

## Required server configuration

`--dir` automatically loads `loft.toml` or `registry.toml` from that role's private state mount. The
image does not accept a production bypass: a loft bound beyond loopback, configured with `[pool] join =
true`, or given a nonempty `[pool].domain` refuses to start until its compliance configuration is
complete. Only a private loopback loft with pool joining disabled may omit it. A registry with
identity-provider configuration applies the same fail-closed requirement. GitHub requires both the
public client ID and `PIGEONPOST_GITHUB_CLIENT_SECRET_FILE`; a partial pair is an error. In the
production Compose path the host file is mounted read-only at a fixed container path. It must be a
nonempty, owner-only (mode `0400` or `0600`), single-link regular file no larger than 4 KiB and owned
by the container service UID (`10001`). Symlinks and a direct
`PIGEONPOST_GITHUB_CLIENT_SECRET` environment value are refused.

The canonical schema, validation rules, and complete examples live in
[`docs/runtime-configuration.md`](../docs/runtime-configuration.md); the summary below highlights
the deployment-critical fields.

For a public interactive installation, run `pigeonpost install --domain ... --no-service`, add the
complete compliance block and key material, and only then enable/start the generated service. This
prevents an incomplete public node from briefly accepting traffic during setup.

A production `loft.toml` contains the existing `[loft]` settings and may list the exact proxy source
addresses as `trusted_proxies = ["IP", ...]`. It also requires:

- `[compliance.registry]`: `registry_url`, `expected_origin`, `registry_checkpoint_key`,
  `witness_threshold`, `minimum_checkpoint_size`, `minimum_checkpoint_root`,
  `max_staleness_seconds`, `refresh_interval_seconds`, and `state_path`;
- one or more `[[compliance.registry.witnesses]]` entries with `name` and `public_key`;
- `[compliance.trace]`: `directory`, `signing_key_file`, `max_records_per_segment`, and
  `max_storage_gb`;
- `[compliance.trace.policy]`: `jurisdiction` (`tr`, `us`, or `eu`) and `capture` (`standing` or
  `preservation`). US is fixed at 30 retention days (the field may be omitted or exactly `30`); TR
  requires an explicit `retention_days` from 365 through 730; EU forbids `retention_days` and
  requires `preservation_starts_at_ms` plus `preservation_expires_at_ms`.

A production `registry.toml` requires one or more canonical
`[server] directory_publisher_keys` matching the public document-signing identities of the
directories allowed to publish admitted loft mutations, and may contain `trusted_proxies`. Its
`[server.limits]` fields are
`max_concurrent_connections`, `max_concurrent_requests`, `max_blocking_operations`,
`max_dump_streams`, `blocking_timeout_ms`, `header_timeout_ms`,
`global_requests_per_minute`, `global_response_bytes_per_minute`,
`source_challenges_per_minute`, `source_bindings_per_minute`, `max_source_keys`,
`account_bindings_per_minute`, and `max_account_keys`. When an identity provider is enabled it
requires the same `[compliance.registry]`
witness configuration plus:

- `[compliance.claim_trace]`: `network_directory`, `identity_directory`,
  `network_signing_key_file`, `identity_signing_key_file`, `max_records_per_segment`,
  `network_max_storage_gb`, and `identity_max_storage_gb`;
- `[compliance.claim_trace.policy]` with the same jurisdiction and capture fields.

Signing-key files are raw 32-byte seeds and must already exist as regular, non-symlink files with
owner-only mode `0600` or `0400`. Network and identity seeds must differ from each other and from the
registry checkpoint seed. Relative state and key paths resolve under `--dir`. In the production
Compose topology, use these absolute trace paths so trace data cannot fall back into a SQLite role
volume:

| Storage | Fixed container path | Required host-path variable |
| --- | --- | --- |
| Registry `--dir` (database, WAL, and SHM) | `/var/lib/pigeonpost` | `PIGEONPOST_REGISTRY_DATA_HOST_PATH` |
| Directory `--dir` (database, WAL, signing key, and mutation reservations) | `/var/lib/pigeonpost` | `PIGEONPOST_DIRECTORY_DATA_HOST_PATH` |
| Loft `--dir` (database, WAL, key, policy, and queued events) | `/var/lib/pigeonpost` | `PIGEONPOST_LOFT_DATA_HOST_PATH` |
| Loft `directory` | `/var/lib/pigeonpost-traces/loft-network` | `PIGEONPOST_LOFT_NETWORK_TRACE_HOST_PATH` |
| Registry `network_directory` | `/var/lib/pigeonpost-traces/registry-network` | `PIGEONPOST_REGISTRY_NETWORK_TRACE_HOST_PATH` |
| Registry `identity_directory` | `/var/lib/pigeonpost-traces/registry-identity` | `PIGEONPOST_REGISTRY_IDENTITY_TRACE_HOST_PATH` |

The application sizes an append-only UTC-epoch runway, including the current epoch for standing
capture. US uses 31 epochs; TR uses `retention_days + 1`; EU counts every UTC epoch intersected by
its preservation interval. The public Loft and witnessed Registry serving boundaries accept only
their built-in audited sealed trace adapters (and the Loft's built-in durable SQLite adapter), then
independently recompute the complete policy runway and byte budgets; custom trait implementations
cannot self-assert production readiness. Both service databases must be owner-custodied persistent
files, and the Directory database must likewise retain and revalidate its owner-custodied file and
parent descriptors before public startup and on readiness. SQLite memory/temporary/URI storage is test-only. The
`max_storage_gb` fields are logical fail-closed budgets, not legal
deletion schedules, physical-block accounting, or hard-quota proof. The complete examples use a
384 GiB loft trace budget at 2,400 requests/minute and separate 16 GiB registry purpose budgets at
100 requests/minute; those values pass the conservative current sizing formula. Registry claim
admission and planning use `min(global_requests_per_minute, 454_795)`: the read-only HTTP surface
may retain its separate 10,000,000/minute audited ceiling, but that never enlarges the identity
binding boundary past 454,795/minute. That maximum assumes 10,000 records per segment; shorter
segments lower it to `floor(65,536 * max_records_per_segment / 1,441)` and fail startup when the
configured rate cannot fit one terminal UTC-epoch manifest. Recalculate after changing admission
rates, policy intervals, or segment size. Set each host quota above its logical budget to allow for filesystem
overhead and operational alert headroom.

Provision a new directory's owner-only raw seed as `signing_key_file` in `directory.toml`; pin only
its derived public key in the registry configuration. Keep the registry database and its WAL/SHM on
a dedicated local volume with an enforced quota (10 GiB baseline, alert before 80 percent). Expand
that volume when needed; never prune or renumber transparency-log leaves.

Deployments upgrading from the legacy `pigeonpost_registry` named volume must migrate deliberately:

1. Stop the registry and confirm no process can write the old volume.
2. Take and restore-test a SQLite/WAL-aware backup as required by the release migration note.
3. Provision the canonical `0700`, UID-10001-owned `PIGEONPOST_REGISTRY_DATA_HOST_PATH` on its
   quota-managed local storage.
4. Copy the complete stopped volume contents—including the database, WAL, SHM, configuration,
   checkpoint key, and registry lock state—while preserving ownership and mode. Verify the copied
   database with SQLite integrity checking and compare the required key/config files before startup.
5. Render Compose, start only the registry, and require `/health` plus witnessed continuity before
   continuing. Retain the stopped legacy volume as the rollback source until the release is accepted;
   do not delete it during migration and do not bind directly into Docker's volume-internal path.

Apply the same stopped-writer, backup/restore, ownership-preserving copy, integrity, and rollback
procedure to the legacy `pigeonpost_directory` and `pigeonpost_mail` named volumes before setting
`PIGEONPOST_DIRECTORY_DATA_HOST_PATH` and `PIGEONPOST_LOFT_DATA_HOST_PATH`. The production Compose
file deliberately does not auto-attach those volumes: doing so would leave the Directory custody
check dependent on Docker's root-owned volume parent and would make the loft's physical capacity an
unbounded shared-disk promise. Retain all three stopped legacy volumes until the complete release is
accepted; never start v0.2 against empty host paths and call that a migration.

Configuration alone is insufficient. Before startup, operators must provision daily
purpose/jurisdiction keys, establish a fresh independent strict-majority witness quorum (`2k > N`)
or a persisted initial cache plus a reachable registry under the same policy, provision
offline-custody public keys, and record an explicit jurisdiction/preservation decision. The quorum
threshold guarantees same-roster intersection, not honesty; the deployment must justify
`f < 2k - N` for at most `f` equivocators or add gossip/out-of-band checkpoint comparison.
Pigeonpost servers seal and retain records; only the separate offline custody process can unseal
them. None of those prerequisites is created or proved by this runbook.

## Roll out and verify

Pull the exact digest first, then update one role at a time:

```bash
docker pull "$PIGEONPOST_IMAGE"
docker compose -f deploy/compose.prod.yml up -d --wait registry
docker compose -f deploy/compose.prod.yml up -d --wait directory
docker compose -f deploy/compose.prod.yml up -d --wait loft
```

The health checks exercise the running HTTP services, not merely the executable:

| Role | Local readiness check |
| --- | --- |
| Registry | `http://127.0.0.1:7718/health` |
| Directory | `http://127.0.0.1:7719/ready` |
| Loft | `http://127.0.0.1:7717/ready` |

Directory readiness includes exact replay/finalization of any schema-4 mutation reservation left by
a canceled request, ambiguous registry response, or prior process exit. `/health` may be live while
that witnessed recovery is still unavailable. Do not delete reservation rows or bypass `/ready`;
restore registry/witness reachability and let the bounded supervisor reconcile them.

After each role becomes healthy, verify its public TLS endpoint before moving to the next role. Run
an end-to-end Pigeonpost exchange only after all three public endpoints are healthy.

## Roll back

Set `PIGEONPOST_IMAGE` to the previously recorded digest and run the same one-role-at-a-time commands.
v0.2.0 introduces storage migrations, so an image-only rollback is unsafe. Follow the
[migration and rollback note](../docs/migrations/v0.2.0.md): stop the new writers and restore the
verified pre-upgrade state before selecting the previous digest. Never start an old binary on a
forward-migrated database.

## Runtime constraints

The Compose defaults assume the host has other work to protect:

- fixed unprivileged UID/GID `10001:10001`, all capabilities dropped, and no new privileges;
- read-only root filesystem and a 64 MiB `noexec,nosuid,nodev` temporary filesystem;
- CPU, memory, process, file-descriptor, and log-size ceilings;
- required quota-managed host binds for registry, directory, and loft state, plus three
  purpose-separated trace binds outside every SQLite role path;
- a 30-second graceful-stop window through `tini`.

`pigeonpost_mail` remains the legacy loft-volume identifier only for migration and rollback.
Production v0.2 uses the explicit loft host path after the stopped volume has been copied and
verified; never rename the legacy volume in place or let Compose silently attach an empty volume.

Capacity is the loft's advertised budget. A full loft refuses new writes instead of consuming the
rest of a shared disk; that is the intended failure mode described in `docs/capacity.md`.
Trace capacity is independent: its configured logical budget refuses further trace writes when
headroom is exhausted, while the host remains responsible for an independently enforced physical
quota. Do not put a trace source path inside Docker's named-volume storage tree.

## TLS and DNS

The default loopback ports are:

| Example public role | Reverse-proxy target |
| --- | --- |
| `loft.example` | `127.0.0.1:7717` |
| `registry.example` | `127.0.0.1:7718` |
| `directory.example` | `127.0.0.1:7719` |

Use explicit DNS records for services hosted away from the wildcard target. If the host has no
existing certificate automation, use a DNS-01 ACME challenge so deployment does not seize port 80.
