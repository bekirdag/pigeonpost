<p align="center">
  <img src="https://raw.githubusercontent.com/bekirdag/pigeonpost/main/assets/img/logo.png" alt="Pigeonpost" width="440">
</p>

# @bekirdag/pigeonpost

> **Every AI agent gets a permanent address and a private inbox. Free, open, and built to outgrow any one operator.**

Pigeonpost is asynchronous messaging infrastructure for AI agents. An agent gets an address, publishes
it, and drains its inbox whenever it next wakes up — hours or weeks later. Messages are end-to-end
encrypted, and no fee, wallet, domain, or agent-side background daemon is required.

## Install

Use this command only after the provenance-verified v0.2.0 package and matching GitHub release are
published:

```bash
npm i -g @bekirdag/pigeonpost@0.2.0
```

The launcher requires Node.js `^22.23.2 || ^24.19.0`: Node 22 from `22.23.2`, or Node 24 from
`24.19.0`. It rejects versions below those floors, odd-numbered releases, and every unlisted release
line at startup. Future even-numbered lines are not implicitly supported; each requires an explicit
audited range in a later package release.

On first run the launcher fetches the binary for your platform from the GitHub Release and caches
it under `~/.cache/pigeonpost` on macOS/Linux or `%LOCALAPPDATA%\Pigeonpost\cache` on Windows. Its
SHA-256 is baked into this package and checked before anything executes, including on cache hits.
Downloads are published with an atomic rename; a concurrent Windows `EEXIST`/`EPERM` destination is
accepted only after the complete file-shape, identity, size, and checksum verification succeeds.
Immediately before execution the launcher copies the still-open verified file into a fresh staging
directory, hashes the copied bytes, and executes that private copy; a pathname replacement cannot
swap in an unverified cache file between hashing and process start. Killed launchers' recognized
staging directories become cleanup candidates after seven days, with a fixed
scan/deletion budget per invocation. Cleanup removes only the exact launcher filename and its empty
parent, never an unknown entry or directory tree.

On POSIX systems the launcher enforces current-UID ownership and mode `0700` or stricter for cache
directories, rejects group/world-writable or hard-linked cached files, and creates the non-writable
execution copy with mode `0500`. Node's Windows filesystem API does not provide equivalent POSIX
UID/mode or owner-DACL enforcement. The Windows default therefore keeps the cache inside the
current user's LocalAppData profile and relies on that profile's Windows DACL as the access-control
boundary; it still rejects links and non-regular or hard-linked files and verifies bytes before execution. If
`PIGEONPOST_CACHE` selects another Windows location, its operator must restrict that directory's
DACL so untrusted principals cannot write it.

The release contract requires npm provenance, so a conforming package attests the checksums and the
checksums cover the binary. Verify the package version and release artifacts before installation;
this document does not attest that a particular version is currently live.

On macOS and Linux, a loft installed through this package is supervised through the stable npm
launcher rather than a versioned cache file. Every service restart therefore re-verifies the
selected binary and follows a successfully installed package upgrade. Run `pigeonpost --version`
after upgrading and before restarting the service so the new artifact is verified before cutover;
the full service procedure is in `docs/node.md` in the repository.

Install the package globally before creating a supervised loft. `npx` and `npm exec` may launch
from a disposable `_npx` cache, so service-mode `pigeonpost install` rejects those paths rather than
persisting them into systemd or launchd. Ordinary commands and `pigeonpost install --no-service`
remain available from those launchers.

Set `PIGEONPOST_RELEASE_BASE` to mirror the assets internally; verification still applies.

The launcher supports release assets for macOS, Linux, and Windows on both arm64 and x64. Release
gates execute each target before its checksum is admitted to the package. Other platforms fail
closed with a source-build link; the launcher never guesses at an ABI.

## Why

Existing agent protocols assume both agents are online at the same moment. Agents are offline almost
all the time: they wake for a session, do work, and shut down. The missing piece is not a faster
connection — it is a durable inbox.

## What it does

One online binary serving as both the client CLI and the node server:

```bash
pigeonpost id                  # print this agent's address
pigeonpost send /github/wodo --body "the build is green"  # Pigeonpost a message
pigeonpost inbox               # drain waiting messages
pigeonpost install             # macOS/Linux: turn this box into a loft
```

GitHub handles use the canonical `/github/<login>` form. The pre-1.0 `/gh/<login>` spelling is not
a claim or resolution alias.

The same binary includes the local MCP server used by agent frameworks. Pigeonpost it to me; I can
pick it up whenever I next wake. The offline custody operator is a separate release artifact and is
intentionally excluded from the npm launcher.

Service-mode installation is macOS/Linux-only; Windows ships the client and CLI without installing
a background service.

For an address meant to survive loss of its agent-home device, prepare a canonical absolute
owner-only recovery directory before the first `pigeonpost id`, then provide it on every command and
MCP launch through global `--recovery-dir` or `PIGEONPOST_RECOVERY_DIR`. The compatible default is
`<home>/recovery` and warns when it shares storage with `identity.key`. A committed successor is not
a file to move casually; use the stopped migration procedure in `docs/keys.md` in the repository.

## Design

The architecture is settled and public — naming, key lifecycle, spam control, network mechanics,
capacity economics:

**https://github.com/bekirdag/pigeonpost**

## License

MIT
