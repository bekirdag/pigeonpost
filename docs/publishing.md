# Pigeonpost — Publishing and release operations

Status: release runbook. The MCP server ships in the same binary as the CLI and server roles.
Opened: 2026-08-08

`integration.md` calls the MCP server the primary integration path, ahead of the CLI and the
library. That makes distribution a product concern rather than a marketing one: a messaging network
with one participant is worth nothing, and MCP directories are the one place the population we need
— agents that can install capabilities — is already assembled.

This document lists where to publish, in what order, and what has to be true before we do.

## The shape of the ecosystem

Three layers, and mistaking one for another wastes effort.

| Layer | What it holds | Examples |
| --- | --- | --- |
| **Package registries** | The actual code | npm, PyPI, Docker Hub |
| **The official MCP Registry** | *Metadata pointing at* packages. Namespace-authenticated, deliberately unopinionated, no curation | `registry.modelcontextprotocol.io` |
| **Aggregators / marketplaces** | Curation, ratings, search, install UX. Most pull from the official registry hourly | Smithery, Glama, PulseMCP, mcp.so |

The official registry is explicitly **not** meant to be consumed by host applications directly — it
is an upstream for aggregators. So one correct publish upstream propagates to most of the
downstream directories without a separate submission each. That is the whole reason to do it first.

## Order of operations

Each step is a hard prerequisite for the next; the registry validator rejects metadata pointing at
an artifact that does not exist yet.

1. **GitHub and GHCR release** — six native, execution-tested `pigeonpost` binaries, four separately
   named macOS/Linux `ppcompliance` binaries, and one scanned, digest-addressed multi-architecture
   online server image. The tag workflow generates complete checksums, one SBOM bound to each native
   binary, and build provenance. Each SBOM binds the exact binary digest to a locked,
   target-filtered Cargo graph: normal and build dependencies are retained, dev-only edges are
   excluded, and root-only inventories fail release verification. The offline operator is
   deliberately absent from both the server image and npm launcher
2. **npm** — one provenance-bearing `@bekirdag/pigeonpost` launcher. On first use it downloads the
   matching release binary, verifies its package-pinned SHA-256 digest, rejects unsafe cache
   file/link state, and re-verifies into a fresh execution copy on every run (`sds.md` §3). POSIX
   launchers additionally enforce current-UID directory ownership and private, non-writable modes
3. **Official MCP Registry** — the `server.json` publish. Everything downstream keys off this
4. **Aggregators that do not auto-index** — a handful need a manual submission
5. **Client-specific catalogues** — where the install is one click inside a host we care about

The tag workflow enforces that order with separate authority boundaries. Native builds, per-target
SBOM generation, release assembly, image scanning, and runtime tests have no publication or OIDC
authority. An OCI candidate is pushed without a tag; a pinned first-party job attests that exact
digest; and a read-only job verifies, scans, executes, and proves anonymous access to it. Release
files then move from deterministic assembly to a first-party attestation job, a read-only provenance
verifier, and finally a contents-write-only `gh release create` job. Only after that fresh release
is immutable—or an existing immutable release has passed the complete resume verification—may the
package-write promotion job recover the checksummed digest pointer from that release, reverify its
OCI provenance and anonymous visibility, and create the stable commit and version aliases. npm and
MCP publication can begin only after promotion converges at the release provenance gate.

The launcher cache defaults to `~/.cache/pigeonpost` on macOS/Linux and
`%LOCALAPPDATA%\Pigeonpost\cache` on Windows. POSIX UID and mode checks are not available through
Node's Windows filesystem API, so the Windows boundary is the current user's profile DACL, not an
owner/mode assertion made by the launcher. A custom Windows `PIGEONPOST_CACHE` is conforming only
when its operator restricts the directory DACL against writes by untrusted principals. File-shape,
link-count, size, identity, and checksum verification still apply on Windows. Concurrent Windows
publication accepts an `EEXIST`/`EPERM` destination only after that full verification. Stale
execution staging cleanup is seven-day age-gated and bounded per invocation; it removes only an
exact recognized executable and the resulting empty directory, never an unknown entry recursively.

## 1. Package registries

| Platform | Why | Notes |
| --- | --- | --- |
| **npm** | The registry's ownership check for npm packages reads `mcpName` from `package.json`. No npm package, no listing | Publish the package *before* the registry metadata |
| **crates.io** | Skip. Rust crates are internal implementation components, not independently supported artifacts | Every workspace manifest declares `publish = false`; CI verifies Cargo metadata reports an empty publication allowlist and rejects every normal, development, or build dependency on the offline `pigeonpost-compliance` crate from another workspace package |
| **GitHub Releases** | Immutable native assets for the online product and the separately distributed offline custody operator | Verify `SHA256SUMS`, the asset attestation, and the SBOM bound to that exact target; never obtain `ppcompliance` through the online image |
| **GHCR** | Primary server-image registry. Production consumes the exact digest recorded in each GitHub release | The release workflow builds Linux arm64/x64, scans the digest, and publishes provenance |
| **Docker Hub** | Optional mirror. It must preserve the verified GHCR digest and may never become the production source of truth | A loft image is independently useful — see `node.md` |
| **PyPI** | Skip. There is no Python artifact and inventing one to be listed twice is not a reason | |

The one-package contract means exactly one package-registry publication:
`@bekirdag/pigeonpost` on npm. The Rust workspace may be consumed from this repository as source,
but no crate is published separately and no release credential may gain crates.io authority. This
prevents internal compatibility surfaces from becoming accidental public package contracts.

The two Linux native assets are static musl executables built in a release-only toolchain derived
from the digest-pinned Rust/Alpine base with exact musl development packages. Release CI rejects an
ELF program interpreter or `DT_NEEDED` dependency and executes both the online and offline assets
in the digest-pinned Bullseye baseline before M6. The official server image is different: it uses
digest-pinned current-stable Trixie build/runtime stages and exact runtime packages. Debian's
[release lifecycle](https://www.debian.org/releases/) lists Bullseye LTS ending 2026-08-31 and
Trixie support through 2028-08-09 plus LTS through 2030-06-30; review and refresh the pinned image
and package set before the chosen base enters its final supported year.

The npm source tree deliberately has no `checksums.json`: those digests do not exist until the six
online release binaries have passed their platform gates. Both `prepack` and `prepublishOnly` fail
closed for source-directory operations until release assembly writes the exact canonical six-key
manifest. Assembly explicitly enables lifecycle scripts, fixes the manifest mode to `0644`, and
then verifies that the tarball contains exactly `LICENSE`, `README.md`, the executable launcher,
`checksums.json`, and `package.json` with their required modes. npm does not rerun those lifecycle
scripts when publishing a prebuilt tarball, so the release contract permits that operation only for
the protected job consuming the checksum-verified immutable release bytes. Do not use
`--ignore-scripts` to construct a release.

Trusted publishing also needs a sufficiently recent npm CLI. The publication job downloads the
exact npm 11.16.0 tarball URL, verifies the independently reviewed SHA-256 pinned in the workflow,
and installs only that local tarball with lifecycle scripts disabled before checking the CLI
version. A version-only `npm install -g npm@…` is not an acceptable release step because mutable
registry metadata would choose executable bytes inside a package-write job.

After a fresh publish or an idempotent resume, the job downloads npm's exact tarball and compares it
byte-for-byte with the immutable GitHub release tarball. The pinned npm CLI then performs its
cryptographic registry-signature and attestation audit. Pigeonpost decodes only the resulting
verified SLSA statement and requires its npm subject SHA-512, repository, `release.yml` path, tag,
source commit, GitHub-hosted builder, and repository-scoped invocation to match the release. Merely
observing a provenance predicate in mutable package metadata is not release evidence.

## 2. The official MCP Registry

Backed by Anthropic, GitHub, Microsoft and PulseMCP. Still marked **preview** — breaking changes and
data resets are possible, which is an argument for automating the publish, not for delaying it.

The checked-in [`server.json`](../server.json) is the release input. The tag workflow downloads a
fixed `mcp-publisher` release, verifies its SHA-256 before execution, and uses GitHub OIDC:

```bash
mcp-publisher login github-oidc
mcp-publisher publish
```

Do not regenerate the file during a release. Its top-level version, npm package version, and Cargo
workspace version must already equal the tag.

Two requirements that block a publish if missed:

- **`mcpName` in `package.json` must match `name` in `server.json`.** This is the ownership proof
  for npm-hosted servers, and a mismatch is a hard failure rather than a warning
- **The npm package must already be live.** The validator resolves the artifact

### The namespace decision

Names are reverse-DNS and tied to a verified GitHub account or domain. Two options, and one is
foreclosed:

| Namespace | Verified by | Available |
| --- | --- | --- |
| `io.github.bekirdag/pigeonpost` | GitHub account | **Yes** — matches the repo and the npm scope |
| `com.pigeonpost/…` | DNS on `pigeonpost.com` | **No.** We do not own the domain; the 2002 registrant actively uses it and has not sold it |
| `com.piyote/pigeonpost` | DNS on a domain we do control | Possible, but it buries the project under an unrelated brand |

**Take `io.github.bekirdag/pigeonpost`.** It is consistent with the npm scope, the GitHub repo, and
the exit-right story — the namespace is provably ours and does not depend on a domain negotiation
that may never conclude. Revisit only if `pigeonpost.com` is ever acquired.

### Automate it

Publishing from CI on tag keeps the registry entry from drifting from the released binary, which is
the failure mode that makes a listing worse than no listing. The release workflow publishes the
byte-verified npm tarball first, then publishes the checksummed `server.json` with a separate GitHub
OIDC job that has no GitHub write permission and needs no stored secret.

## 3. Aggregators

Most of these pull from the official registry automatically. Submit manually only where noted.

| Platform | Reach | Submission | Worth it |
| --- | --- | --- | --- |
| **PulseMCP** | Largest hand-reviewed directory; publishes weekly visitor estimates per server | Submit button in the nav | **Yes.** Human review means a listing here reads as vetted, and the traffic numbers are the only usage signal we will get for free |
| **Glama** | ~37k+ servers indexed; auto-indexes open-source repos from GitHub | Auto-indexed; claim the listing | **Yes** — claim it so the metadata is ours rather than scraped |
| **Smithery** | Largest catalogue by breadth; hosted-install path | Registry submission | **Yes**, with a caveat below |
| **mcp.so** | 20k+ servers, one of the largest aggregators | Submit button or their GitHub issues | Low effort, take it |
| **MCPMarket** | 10k+ across 23 categories, client-agnostic | Site submission | Marginal. Do it last |

**The Smithery caveat.** Directories that offer hosted or one-click remote installs generally expect
a Streamable HTTP server they can run. Pigeonpost is deliberately a local stdio process that owns a
SQLite file and a keypair — hosting it elsewhere would mean *someone else holding the agent's
identity key*, which contradicts `keys.md` and requirement 6. List as a local/stdio server. If a
platform only supports hosted servers, decline rather than compromise the key custody model.

## 4. Client-specific catalogues

Reach into a specific host's install flow. Lower volume, much higher intent.

| Platform | Notes |
| --- | --- |
| **GitHub MCP Registry** | Discovery and install surface inside GitHub's own tooling; sources from the official registry |
| **Docker MCP Catalog** | Needs the Docker image from step 1. Reaches enterprise users who will not run untrusted `npx` |
| **VS Code / Copilot MCP gallery** | Where a large share of agent developers already are |
| **Cline marketplace** | Integrated install workflow, active community |
| **Anthropic connectors directory** | Highest-intent audience for us by a distance — but review is selective, and see the gate below |
| **`awesome-mcp-servers`** | A GitHub PR. Free, still a real source of discovery traffic |

## Gates — what must be true before any of this

Publishing is what makes the threat model real. Until listed, Pigeonpost is a design with a threat
model; once listed, strangers can send attacker-authored text into any installed agent's context.

1. **`integration.md` must match the code.** CI tests the documented MCP tool names and schemas,
   including token revocation and handle registration. That table is the integrator contract and a
   mismatch blocks release
2. **Tool annotations remain enforced.** Read-only tools carry `readOnlyHint`; mutating or
   open-world tools carry the corresponding negative/read-world hints. Hosts use these to decide
   what auto-approves
3. **`read` requires acknowledgement.** MCP requires `acknowledge_untrusted: true`, inbox summaries
   omit bodies, and explicit reads return a fenced `untrusted_body`. The core type also serializes as
   a marked object rather than a transparent attacker-authored string
4. **The EU e-Evidence designation exists** (`law.md` §6). Listing on public directories puts users
   in many jurisdictions and makes "offering services in the Union" harder to argue against. Publish
   after the designation, not before — the sequencing costs nothing. Before creating a tag, create
   the hardcoded GitHub environment `production-release`, require at least one reviewer, enable
   **prevent self-review**, disable administrator bypass, and allow only tag pattern `v*`. Enable a
   custom deployment-protection GitHub App whose external broker rejects any deployment whose
   repository, tag, commit, or designation-evidence digest lacks an independently approved record.
   GitHub invokes an enabled custom protection rule before the environment job starts. The app
   alone is not independent of a repository administrator who can select or remove environment
   rules. Before publication, either (a) make the external broker the registry publisher/credential
   holder so this repository can produce only candidate evidence, or (b) place the repository under
   a separately administered organization/enterprise ruleset that applies to administrators and
   protects the release workflow, authorization config, environment, and tag policy. A repository
   administrator must not control that external boundary. Pin the expected app slug and the
   approver's DER-SPKI Ed25519 public key in `.github/release-authorization.json` only after that
   boundary exists. Both values remain `null` until it does; this is an intentional fail-closed
   publication gate, not a placeholder to fill with developer-generated material. See GitHub's
   [custom deployment protection rule documentation](https://docs.github.com/en/actions/how-tos/deploy/configure-and-manage-deployments/configure-custom-protection-rules)
   The authorization job also requires GitHub's environment API to return
   `can_admins_bypass: false`. A missing field is not treated as an older-compatible response: it
   is indistinguishable from redaction and blocks publication.
5. **The in-workflow evidence matches the external approval.** Store a lowercase SHA-256 of the
   designation evidence (the evidence itself stays outside the repository) in the environment
   variable `PIGEONPOST_EU_EVIDENCE_DESIGNATION_SHA256`, and store the independent approver's
   one-release signature as `PIGEONPOST_RELEASE_APPROVAL_SIGNATURE_B64`. The job checks that the
   configured app is enabled and verifies the signature over repository, tag, commit, and evidence
   digest. The app slug, key, verifier, and workflow are all in or selected by repository-administered
   state, so these checks are defense in depth and auditable evidence—not the independent trust
   boundary. The separately governed ruleset or publisher broker must validate the same release
   without deriving trust from the tagged commit or repository-admin-controlled configuration
6. **Repository and registry immutability are active.** Enable GitHub immutable releases before the
   tag. For an existing GHCR package, make it public so anonymous digest pulls work; a namespace
   that has never been published must instead follow the one-time bootstrap ceremony below.
   Configure npm trusted publishing for `bekirdag/pigeonpost`, workflow `release.yml`, and the
   `npm publish` action. The workflow token cannot read GitHub's Administration-only
   immutable-release setting, so an operator must
   verify the dedicated repository setting through GitHub's versioned API before pushing the tag.
   After publication the workflow verifies `isImmutable`; if the release is still mutable or its
   release attestation cannot be verified, it blocks every downstream package publication and
   leaves the release untouched for explicit operator inspection. Automation never deletes or
   overwrites a published release, substitutes mutable bytes, or falls back to a long-lived npm
   token. Protect `main` with at least one approving review, required status checks, and force-push
   and deletion prevention. The authorization job paginates GitHub's effective rules for `main`,
   follows every reported repository or parent organization/enterprise ruleset ID through the
   versioned repository-ruleset API, and requires each detailed record to be active, branch-scoped,
   source-matched, and to carry an explicit empty `bypass_actors` array. GitHub omits that field when
   the caller cannot inspect bypass actors; omission therefore blocks publication instead of being
   interpreted as an empty list. The tagged commit must also be contained in `main`. Under the
   configured branch, environment, and external broker controls, an unreviewed side commit cannot
   become a release merely by receiving a version tag
7. **Publication and production provider credentials stay separate.** npm, MCP Registry, GitHub
   release, provenance, image, and package-promotion jobs must not receive identity-provider client
   secrets. A production operator provisions the GitHub provider secret directly on the target as
   an owner-only, single-link regular file and gives preflight only its absolute path. Preflight
   rejects direct secret environment injection, validates the file contract, and removes provider
   configuration before running unrelated subprocesses. Compose mounts the file read-only and gives
   `registry serve` only the fixed in-container `PIGEONPOST_GITHUB_CLIENT_SECRET_FILE` path. See
   [`identity-providers.md`](identity-providers.md) and
   [`runtime-configuration.md`](runtime-configuration.md)

### One-time GHCR namespace bootstrap

GitHub's
[package-visibility contract](https://docs.github.com/en/packages/learn-github-packages/configuring-a-packages-access-control-and-visibility)
makes a first package private by default, permits anonymous pulls only after a Container Registry
package is public, and does not permit a public package to become private again. Visibility cannot
be changed before the package exists. Do not work around this ordering with a manually pushed
image, temporary tag, personal access token, or stable alias. Use the first real tag workflow as
the bootstrap, and keep every other release gate in force:

1. Complete every pre-tag requirement above, including immutable GitHub releases, protected release
   authorization, npm trusted publishing, the reviewed release commit on protected `main`, and the
   check that both stable GHCR aliases are absent. Push the reviewed tag exactly once.
2. If the package is already anonymously readable and the `container_verify` job passes
   **Require anonymous access to the exact digest before release assembly**, no visibility exception
   is needed; let the workflow continue. Otherwise, this ceremony applies only when the workflow
   fails at that exact step after `container_candidate`, `container_attest`, and every preceding
   provenance, scan, and execution check has passed. A failure anywhere earlier is not a bootstrap
   condition: stop without changing package visibility.
3. Record the workflow run ID, attempt, tag, commit, and exact `sha256:...` digest from the
   successful **Build and push a per-attempt candidate image** step's workflow summary. Confirm that
   `release_create`, stable-alias promotion, npm publication, and MCP Registry publication did not
   run. The candidate must still be untagged.
4. In that package's GitHub settings, independently confirm that its source repository is exactly
   `bekirdag/pigeonpost` and that this repository's Actions access retains write permission. Obtain
   approval for the one-way visibility decision, then change only the package visibility to
   **Public**. Do not grant a person publication credentials or push any additional bytes.
5. From a clean release workstation with GNU `sha256sum`, prove that the recorded digest—not a
   tag—is anonymously readable. An empty, temporary Docker configuration makes the absence of
   registry credentials explicit; do not put a credential in the command or that directory:

   ```bash
   IMAGE='ghcr.io/bekirdag/pigeonpost'
   DIGEST='sha256:<64-lowercase-hex-from-the-workflow-summary>'
   ANONYMOUS_DOCKER_CONFIG="$(mktemp -d)"
   ANONYMOUS_INDEX="$(mktemp)"

   DOCKER_CONFIG="$ANONYMOUS_DOCKER_CONFIG" \
     docker buildx imagetools inspect "$IMAGE@$DIGEST" --raw > "$ANONYMOUS_INDEX"
   test "sha256:$(sha256sum "$ANONYMOUS_INDEX" | cut -d' ' -f1)" = "$DIGEST"
   ```

   Treat any authentication prompt, network ambiguity, missing manifest, or digest mismatch as a
   failure. Preserve the recorded run evidence according to the release evidence policy; the empty
   temporary Docker configuration and manifest scratch file contain no credentials.
6. Use GitHub's
   [**Re-run failed jobs**](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/re-run-workflows-and-jobs)
   on the same tag workflow; GitHub preserves the original `GITHUB_SHA` and `GITHUB_REF` and reruns
   failed jobs plus their dependents. Do not move or recreate the tag, start a different release,
   rebuild locally, create an alias, or publish npm/MCP metadata by hand. The rerun must re-enter at
   `container_verify`, repeat the anonymous exact-digest gate, and permit downstream release
   assembly and publication only after that gate succeeds. If GitHub cannot preserve the successful
   job outputs required by the rerun, stop for operator review instead of substituting a manual
   publication path.

Retain the bootstrap candidate even if the rerun fails. The digest-wide deletion risk and bounded
untagged-manifest retention rules below apply unchanged.

### GHCR candidate retention

Release builds use BuildKit's digest-only canonical push and never create a `candidate-*` tag. A
failed run can therefore leave an untagged OCI manifest and its blobs, but it cannot leave a public
candidate tag or reserve either stable alias. If a run fails after the immutable GitHub release is
created but before both aliases are promoted, the next tag-workflow attempt takes the resume path,
recovers the verified digest from that release, and idempotently creates only the missing aliases;
it does not rebuild, append release assets, or replace the release. Do not automate deletion with
the GitHub Packages version endpoint: deleting a package version is digest-wide and could remove
commit/version aliases that later share the same manifest. Configure a bounded GHCR
retention/garbage-collection process outside the release token that selects only untagged manifests
older than the documented recovery window, rechecks immediately before deletion that the digest has
no tags, and caps deletions per run. Treat any digest with a tag—or any incomplete/ambiguous
registry response—as retained.

### Independent release authorization ceremony

After the release commit is reviewed on `main`, the release operator sends the independent approver
the canonical payload—not a precomputed statement with different fields. Generate it from the same
checked-in verifier used by CI:

```bash
GITHUB_REPOSITORY=bekirdag/pigeonpost \
GITHUB_REF_NAME=v0.2.0 \
GITHUB_SHA=<40-lowercase-hex-release-commit> \
DESIGNATION_EVIDENCE_SHA256=<64-lowercase-hex-evidence-digest> \
node deploy/acceptance/verify-release-authorization.mjs --print-payload \
  > pigeonpost-release-authorization-v0.2.0.txt
```

The independent approver compares every field with the approved evidence and signs those exact
bytes using the offline Ed25519 private key. Where the external broker is the independent boundary,
it records and enforces the same approved tuple and must not derive trust from this repository. For
an OpenSSL-managed key, the signing operation is:

```bash
openssl pkeyutl -sign -rawin \
  -inkey <offline-approver-private-key> \
  -in pigeonpost-release-authorization-v0.2.0.txt \
  | openssl base64 -A
```

Put only that canonical base64 signature into the protected environment secret, approve the matching
tag deployment, and remove/rotate the secret after the run. Never copy the private key into GitHub,
the repository, a production host, or a developer workstation. A changed tag, commit, repository, or
designation digest invalidates the signature. The signature alone cannot authorize publication when
the external deployment-protection broker is absent or rejects the deployment.

## Maintenance

- One `server.json`, released, checksummed, and published from CI on tag. Never hand-edit a live
  listing
- Never rerun the release workflow for `v0.1.0` or replace any existing release asset. Prepare a new
  version, validate it completely, and create a new immutable tag
- Publish and smoke-test the exact npm tarball produced by CI; never rebuild a package between test
  and publish
- Review GHCR untagged-manifest retention after failed release attempts; never delete by digest
  unless an independently authorized registry process proves it has no stable aliases
- Build and execute `pigeonpost` on all six release runners and `ppcompliance` on the four
  macOS/Linux runners. Keep `ppcompliance` out of npm and GHCR, but include those four assets, each
  native binary's own target-filtered Cargo dependency SBOM, checksums, and generic plus explicit
  SPDX attestations in the same immutable GitHub release. Before either Windows online asset is
  uploaded, run its exact staged bytes through isolated client/CLI initialization, protected
  main/WAL/SHM checks, a private-loopback Loft `/ready`, and Directory `/health` plus witnessed
  `/ready`; require owner-private DACLs, process shutdown, and exact temporary-root cleanup. Do not
  stage a Windows offline-operator artifact until its private-state layer has audited owner-only
  DACL, hard-link rejection, stable file identity, safe-parent, and atomic-replacement controls; the
  release workflow enforces that absence. The online Windows helpers do not satisfy or waive that
  separate offline custody requirement
- Claim every auto-indexed listing so the description is ours
- Re-read this list before each major release. Directory rankings and submission routes shift fast
  enough that a list written in August 2026 should be treated as stale by early 2027
