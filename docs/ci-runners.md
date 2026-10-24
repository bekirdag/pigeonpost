# GitHub Actions runners

Linux/X64 jobs run on GitHub-hosted Ubuntu runners by default. Each of them keeps a selector for the
shared self-hosted label set `self-hosted`, `Linux`, `X64`, `wodo-ci`, but it is used only when the
repository variable `PIGEONPOST_SELF_HOSTED_CI` is `true`, and then only for pushes, manual runs and
pull requests from this repository:

```yaml
runs-on: ${{ vars.PIGEONPOST_SELF_HOSTED_CI == 'true' && (github.event_name != 'pull_request' || github.event.pull_request.head.repo.full_name == github.repository) && fromJSON('["self-hosted","Linux","X64","wodo-ci"]') || 'ubuntu-24.04' }}
```

Fork pull requests always keep their GitHub-hosted label. Keep that condition when editing a
selector; contributor changes must never select our persistent workers (they mount the host's
Docker socket and allow passwordless sudo inside the container, which is host root).

## Why the pool is opt-in (2026-10-21)

Routing every Linux job to the pool (`df33ad8`, `8f981b2`) turned main red on every branch. The
causes, from the job logs and the hosts:

- **Capacity.** Only `wodomini-ci-pigeonpost` was ever online. Wodomini runs one runner container
  per repository (15 of them) but its job-start hook (`~/ci-runners/.hooks/job-start.py`) allows
  two concurrent jobs per host. A single main push queued for over three hours. The Suku worker was
  unpacked but never registered; Suku's root disk is 100% full and its swap is exhausted.
- **Image gaps.** `wodo-ci-runner:24.04` is slimmer than the hosted image the workflows were written
  for: no `sqlite3` (acceptance), `ruby` (lint policy checks), Chrome (inbox call layout), .NET
  (`setup-dotnet` defaults to `/usr/share/dotnet`, which the `runner` user cannot create) or Java.
- **Environment.** The container sets `HOME` to a bind-mounted per-repository directory while the
  image's toolchain lives in `/opt/rustup` and `/opt/cargo`. Jobs that rely on the hosted image's
  preinstalled Rust instead of `rust-toolchain` failed on the worker (`rustc -vV` in rust-cache,
  `cargo: command not found` in the Linux desktop interop job).

Turning the variable on again needs, at minimum: a second registered worker or a higher slot
limit, the missing packages in the image (or per-job installs into `$RUNNER_TOOL_CACHE`, with
`DOTNET_INSTALL_DIR` under it), and a green run of `ci.yml`, `group-crypto.yml`, `android.yml` and
`linux-desktop.yml` on a branch with the variable set.

## Required exceptions

| Job type | Runner | Reason |
|---|---|---|
| Normal Linux/X64 tests, Android builds, packaging and release preparation | Hosted Ubuntu; `wodo-ci` pool only with `PIGEONPOST_SELF_HOSTED_CI=true` | See above. |
| Fork PR jobs | Original hosted Ubuntu label | Contributor code runs separately from persistent workers. |
| Native Linux/X64 group-encryption library | `ubuntu-22.04`; `wodo-ci` pool when opted in | Keep the glibc 2.35 floor check; the initial self-hosted build passed it. `ubuntu-22.04` is the default and fork-PR label. |
| Native Linux ARM64, Windows and macOS jobs | Existing OS/architecture label | No matching self-hosted worker is registered for this repository. |
| `release.yml` → `container_attest`, `release_attest` | `ubuntu-24.04` | Downstream verification requires `--deny-self-hosted-runners`; the attestation producer must meet it. |
| `release.yml` → `npm_publish` | `ubuntu-24.04` | The current npm trusted-publishing environment requires a supported hosted runner. |

The release policy check in `ci.yml` protects the three identity/publication exceptions. Build and
verification jobs can use self-hosted workers without weakening accepted artifact provenance.
[Runner selection](https://docs.github.com/en/actions/how-tos/write-workflows/choose-where-workflows-run/choose-the-runner-for-a-job),
[npm trusted publishing](https://docs.npmjs.com/trusted-publishers/).

## Worker prerequisites and diagnosis

- As checked on 2026-10-21, GitHub exposes `wodomini-ci-pigeonpost` online with the shared labels.
  Its dedicated runner container uses `wodo-ci-runner:24.04`. This inventory is a dated observation;
  use the repository runners API before assuming another machine is registered or available.
- Maintain the toolchain/system capabilities required by each scheduled job, including Rust/rustup,
  Node action compatibility, Docker where used, native build dependencies and browser/KVM support
  for the existing UI tests. Workflow setup actions still provision their declared Java/SDK tools.
- The group-crypto Android AAR job explicitly installs NDK `28.2.13676358`; do not rely on
  `ANDROID_NDK_LATEST_HOME` or a hosted-image-only preinstallation. The installed path is passed
  to the existing API-26/16-KB-aligned build script.
- Queued jobs do not prove a broken selector: inspect their requested labels, runner availability
  and the active job. A single worker serializes jobs that share this label set.
- Read job-level failure logs when a completed job belongs to a still-running workflow. Record the
  exact failing prerequisite before changing workflow behavior or the worker image.

Validate changes with `actionlint` and the existing release publication policy. A workflow edit
does not authorize a production release, Store submission or a release-tag dispatch.
