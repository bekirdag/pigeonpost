# GitHub Actions runners

Compatible Linux/X64 jobs use the shared label set `self-hosted`, `Linux`, `X64`, `wodo-ci` for
pushes, manual runs and pull requests from this repository. The selectors do not pin a machine;
additional compatible workers can join this pool without another workflow edit.

Fork pull requests retain their original GitHub-hosted Ubuntu labels. Keep this condition when
editing a selector; contributor changes must not automatically select our persistent workers.
Repository approval rules and worker isolation remain operational controls as well.

## Required exceptions

| Job type | Runner | Reason |
|---|---|---|
| Normal Linux/X64 tests, Android builds, packaging and release preparation | `wodo-ci` pool | Available self-hosted Linux/X64 workers. |
| Fork PR jobs | Original hosted Ubuntu label | Contributor code runs separately from persistent workers. |
| Native Linux/X64 group-encryption library | `wodo-ci` pool | Keep the glibc 2.35 floor check; the initial self-hosted build passed it. `ubuntu-22.04` remains the fork-PR fallback. |
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
