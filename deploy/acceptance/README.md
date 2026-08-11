# Local release acceptance

These scripts provide the repeatable local gate that sits between source validation and publication.
They do not contact production, change GitHub/npm/MCP state, or read any existing agent directory.

## Run the complete gate

From the repository root:

```bash
./deploy/acceptance/local.sh
```

Without `PIGEONPOST_BIN`, the script builds and exercises `target/debug/pigeonpost`. The binary
portion first creates two separate `PIGEONPOST_HOME` directories and one private loopback loft
beneath a new `mktemp` directory. It proves:

- an absent recipient receives a message after the sender process has exited;
- the isolated agents reverse sender/recipient roles and deliver in the other direction;
- the same sender can queue against a stopped loft, exit, reopen its durable state, and flush after
  the loft restarts on the same endpoint and storage;
- both messages are available only through the fenced untrusted-body read surface; and
- the tested plaintext never appears in the loft or agent stderr logs.

The Linux CI gate also runs `install-defaults.sh` against that exact binary. It invokes precisely
`pigeonpost install` with no options in a clean working directory and clean home. A fixture
`systemctl` is available only through that process's private `PATH`; it sends the generated unit
through the real `systemd-analyze verify` parser, starts the exact binary, and records the activation
order. The installer itself runs under an ordinary `022` umask. The gate requires owner-only key,
configuration, unit, and database files; the exact 20%-of-free-space capacity formula against a
real available-space reading plus deterministic unit cases; SQLite integrity, schema version, and
required tables; and a live
`/ready` response. It never contacts the host service manager. On a non-Linux development host, run
the same proof in a disposable, source-built Linux acceptance image with
`./deploy/acceptance/install-defaults.sh --container`. The acceptance image adds only the systemd
parser and SQLite inspector and verifies that the product binary's SHA-256 is unchanged.

The same exact binary is then used by `composed-services.sh` to start a second disposable topology:
one checkpoint-signing registry, its test-only C2SP witness, one directory with its supervised
prober, and two independently keyed loft processes. That topology proves:

- both signed loft submissions commit through the directory to a fresh witnessed registry head;
- restarting the directory preserves its accepted checkpoint and makes its immediate prober sweep
  examine both submitted lofts;
- the production prober refuses local HTTP endpoints, the signed public snapshot excludes those
  pending lofts, and a real client refresh succeeds but bootstrap refuses `NoLofts` instead of
  selecting an unsafe candidate;
- two real clients publish records to both lofts, delivery remains available while one loft is
  stopped, and the owed second copy stays durable; and
- after that loft restarts on the same endpoint and storage, a later client process flushes the
  owed copy without exposing the tested body in service or client logs.

Before the composed topology, `mcp-stdio.sh` launches two instances of the exact binary's real MCP
stdio host and drives them through line-delimited JSON-RPC without importing product code. It proves
the `initialize`/initialized-notification lifecycle, `tools/list` schemas, identity discovery,
allowing a sender, delivery, inbox metadata, explicit fenced-body read, acknowledgement, and a
second clean ping. Its adversarial body contains a fence-looking delimiter; the client verifies the
body cannot terminate the server-chosen fence and that neither the body nor delimiter appears in
process logs.

The loopback result is intentionally fail closed. Production probing admits only public HTTPS
origins, so an isolated harness cannot promote its private HTTP lofts without weakening the shipped
network policy. Positive promotion and weighted selection remain covered by the real-loft source
integration suite; deployment verification must repeat selection against externally reachable TLS
origins.

The source-backed portion then runs the adversarial cases whose cryptographic fixtures do not
belong in shell:

- permanent HTTP failures become durable dead letters without response-body reflection, while
  retryable failures recover and wake-up concurrency/deadlines remain bounded;
- witnessed attribution succeeds online and from a fresh cache, and required attribution rejects
  an omitted or malformed block;
- whole-process registry, loft, and directory logs/responses do not disclose protected source
  addresses, selectors, message bodies, or internal panic details; and
- the installer-generated proxy configuration and exact `deploy/Caddyfile.local` Compose edge
  configuration adapt under pinned Caddy with their admin endpoints disabled, HTTP access logging
  absent, and global runtime loggers discarded; the local edge is additionally constrained to
  `:17717` and the loft's `127.0.0.1:7717`, while a live generated proxy emits no address or raw-
  selector canary on success, malformed input, or upstream failure; and
- the exact v0.1.0 database shapes migrate transactionally or fail unchanged.

Every process wait and retry loop is bounded. Normal completion and failure terminate every managed
process and remove only newly created directories whose basenames match `pigeonpost-acceptance.*`
or the dedicated `pigeonpost-mcp-acceptance.*` / `pigeonpost-composed-acceptance.*` patterns.

## Exercise an exact release binary

After verifying a downloaded artifact against the immutable release and `SHA256SUMS`, run only the
black-box scenarios against that exact file:

```bash
PIGEONPOST_BIN="$PWD/pigeonpost-linux-x64" \
  ./deploy/acceptance/local.sh --binary-only
```

Use the matching native asset on macOS. Windows release acceptance runs in CI with the exact staged
`.exe` before upload:

```powershell
./deploy/acceptance/windows-release.ps1 -BinaryPath ./dist/pigeonpost-win32-x64.exe
```

That harness checks isolated client state, a private-loopback Loft, and Directory checkpoint
readiness plus owner-private DACLs and SQLite sidecars. Its read-only size-zero witnessed Registry
fixture exists only because the product Registry correctly rejects Windows. These POSIX scripts do
not claim to validate Windows service behavior.

The M6 gate requires the matching online and offline release assets plus the test-only adapter built
from the same source checkout. It creates a closed, producer-signed sealed trace and makes the exact
online binary emit an attributed envelope v3 before exercising the exact offline binary's custody
lifecycle:

```bash
cargo build --release --locked -p pigeonpost-compliance --example m6_acceptance_adapter
PIGEONPOST_BIN="$PWD/pigeonpost-linux-x64" \
PPCOMPLIANCE_BIN="$PWD/ppcompliance-linux-x64" \
PIGEONPOST_M6_ADAPTER_BIN="$PWD/target/release/examples/m6_acceptance_adapter" \
  ./deploy/acceptance/m6-compliance.sh
```

The fixture proves that public artifacts alone cannot unseal either record class, while authorized
test custody can. It verifies disclosure intent/completion pairs, legal-hold preservation,
cryptographic shred and post-shred refusal, checkpoint publication, and exact source-address and
selector canaries across every captured command stream, public artifact, and fixture request log.
The adapter is software-test infrastructure only: it is not packaged, does not represent an
external approval, and must never be used as production custody or destruction evidence.

To run only the composed role topology:

```bash
PIGEONPOST_BIN="$PWD/pigeonpost-linux-x64" \
  ./deploy/acceptance/composed-services.sh
```

To run only the MCP-host scenario:

```bash
PIGEONPOST_BIN="$PWD/pigeonpost-linux-x64" \
  ./deploy/acceptance/mcp-stdio.sh
```

`witness.js` is an acceptance fixture, not an operator witness. It verifies the registry's signed
checkpoint and enforces monotonic sizes before cosigning, but it does not independently recompute
the submitted RFC 6962 consistency proof. Never copy it into a deployed topology.

To retain the isolated directory for a failed-run investigation, set
`PIGEONPOST_ACCEPTANCE_KEEP=1`. The output prints the exact path. Never point the scripts at a real
`PIGEONPOST_HOME`, loft directory, registry directory, or directory-service database.

## Source and rollback gates separately

```bash
./deploy/acceptance/local.sh --source-gates-only
./deploy/acceptance/proxy-privacy.sh
./deploy/acceptance/migration-rollback.sh
sh ./deploy/acceptance/preflight-policy.sh
```

The proxy test uses the official Caddy 2.10.2 Alpine multi-platform image by immutable digest. It
adapts both the exact `loft.Caddyfile` emitted by the tested Pigeonpost binary and the checked-in
`deploy/Caddyfile.local` used by the local Compose edge. It runs the generated file in a container
with an isolated loopback backend and captures the proxy's complete stdout/stderr across successful,
malformed-request, and failed-upstream paths. Docker must be available for the complete source gate.
`PIGEONPOST_BIN` can select a verified release binary for this test as well.

The preflight-policy test supplies isolated `gh`, `docker`, and `stat` stubs. It proves malformed
release tags fail before external I/O, mutable or unattested releases fail closed, and an operator
cannot substitute a different attested image for the pointer stored in the immutable release. It
also proves that direct provider-secret environment injection, incomplete provider configuration,
empty or oversized files, wrong ownership, permissive modes, multiple links, and symlinks fail
closed. The registry-data and three purpose-separated trace paths must be present, canonical,
owner-only, distinct, and non-nested. The canary secret never reaches output or any child process,
and unrelated subprocesses inherit none of the provider configuration. A valid path requires the
release attestation, pointer checksum and artifact provenance, the exact OCI
digest's repository, tag, source-commit, hosted-workflow, and non-self-hosted provenance constraints,
the four private writable mounts, and a read-only owner-only provider secret mount before image and
Compose inspection. Host-side quota enforcement remains an operator-evidence gate; this isolated
test does not claim to prove it.

`container-release.sh` is the release-runner gate for the exact child manifests. The amd64 child
starts as UID 10001 with a read-only root, writable bounded tmpfs, dropped capabilities, and a fresh
named data volume; starts a private loopback loft (so the release test never invents production
witness/custody keys), reaches the real `/ready` healthcheck, and completes an isolated
send/inbox/fenced-read round trip between two other container processes. The arm64 child must at
least reach the same readiness boundary under QEMU. The workflow runs both before the digest receives
stable commit and version aliases.

The same gate renders `deploy/compose.loft.yml` and proves that its loft binds only inside the
shared loopback namespace, its pinned edge mounts that exact Caddyfile read-only, and only the edge
port is published on host loopback. This is a static composition check; the live request paths above
continue to run in a disposable container and never attach to an operator volume.

The rollback drill creates four databases from the checked-in v0.1.0 SQL fixtures, writes a
sentinel under WAL mode, takes each backup through SQLite's online backup API, mutates the source,
restores the backup with all writers stopped, and compares the complete logical dump. It then runs
the focused client, loft, directory, and registry migration/refusal tests.

This proves the backup mechanism and the release-shaped forward migrations. It deliberately does
not suggest that an old binary may open a migrated database. Production rollback means restoring
the verified pre-upgrade backup before starting the previous digest; it never means pointing the
previous binary at forward-migrated state.

## What remains external

The local gate cannot prove npm trusted-publisher configuration, GitHub immutable-release settings,
GHCR public visibility, independent witness operation, provider credentials, offline custody,
legal approval, or that production is running the generated proxy configuration unchanged. Those
remain release/deployment prerequisites and must stay fail closed.
