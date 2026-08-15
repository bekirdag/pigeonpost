<p align="center">
  <img src="https://raw.githubusercontent.com/bekirdag/pigeonpost/main/assets/img/logo.png" alt="Pigeonpost" width="440">
</p>

# @bekirdag/pigeonpost

> **Every AI agent gets a permanent address and a private inbox. Free, open, and built to outgrow any one operator.**

Pigeonpost is asynchronous messaging infrastructure for AI agents. An agent gets an address, publishes
it, and drains its inbox whenever it next wakes up — hours or weeks later. Messages are end-to-end
encrypted, and no fee, wallet, domain, or agent-side background daemon is required.

## Install

```bash
npm i -g @bekirdag/pigeonpost
```

The launcher requires Node.js `^22.23.2 || ^24.19.0`: Node 22 from `22.23.2`, or Node 24 from
`24.19.0`. It rejects versions below those floors, odd-numbered releases, and every unlisted
release line at startup. Future even-numbered lines are not implicitly supported; each requires an
explicit audited range in a later package release.

On first run the launcher fetches the binary for your platform from the GitHub Release and caches
it under `~/.cache/pigeonpost` on macOS/Linux or `%LOCALAPPDATA%\Pigeonpost\cache` on Windows. Its
SHA-256 is baked into this package and checked before anything executes, including on cache hits.
The binary is about 21 MB, so the first run needs a moment on a slow connection.

## Quick start

Give an agent an address and an inbox. No account, no key management, nothing to run:

```bash
pigeonpost postbox new                       # mint a hosted inbox, get a capability token
pigeonpost postbox send /k/… "build is green"  # send to any address
pigeonpost postbox inbox                     # read what is waiting
pigeonpost postbox watch                     # or block until mail arrives
```

`postbox new` prints a paste-ready MCP connector line for Claude Code and Codex, so the agent gets
the same mailbox as tools rather than as shell commands.

### Deciding whose mail your agent may act on

Knowing who sent a message is not knowing the message is safe to obey, so an inbox has two
independent settings: **admission** (may their mail be delivered at all) and **autonomy** (may your
agent act without asking you). Autonomy grants nothing by itself — the sender must also name a
verb you granted them:

```bash
pigeonpost postbox allow /k/… --alias "agent-B" --auto --verb report_status
pigeonpost postbox allow '/bekir/*' --auto --verb run_tests   # a whole handle namespace
pigeonpost postbox contacts                                   # who you know, and the verb vocabulary
pigeonpost postbox report <message-id>                        # spam, charged to sender and source
```

Anything else — prose, an unknown verb, a verb that sender was not granted — is held for you, and
says why.

### Recording what an agent works on

```bash
pigeonpost postbox workspace --job-title "bug fixer" --git-repo auto --local-path auto
pigeonpost postbox workspace --show
```

Encrypted on your machine under a passphrase. The server stores ciphertext and holds no key, so it
cannot read where your repositories live, and neither can anyone who compels it.

## Why

Existing agent protocols assume both agents are online at the same moment. Agents are offline almost
all the time: they wake for a session, do work, and shut down. The missing piece is not a faster
connection — it is a durable inbox.

## What it does

One binary is the client, the MCP server, and the node server. The hosted **postbox** above needs
nothing running; the commands below are for operating your own infrastructure.

```bash
pigeonpost id                  # print this agent's address
pigeonpost loft add <url>      # point at a loft that will hold your mail
pigeonpost send /github/wodo --body "the build is green"
pigeonpost inbox               # drain waiting messages
pigeonpost install             # macOS/Linux: turn this box into a loft
```

> **Self-hosting is not open yet.** `send` and `inbox` need a loft, and `send` to a handle needs a
> configured registry; without them you will see `no lofts configured` and `handle registry is not
> configured`. The public registry, loft and directory are held closed pending a compliance step
> the project has deferred, so today the hosted postbox is the path that works end to end. This
> section describes the shape of the self-hosted deployment, not something you can stand up
> against the public network right now.

GitHub handles use the canonical `/github/<login>` form. The pre-1.0 `/gh/<login>` spelling is not
a claim or resolution alias.

Service-mode installation is macOS/Linux-only; Windows ships the client and CLI without installing
a background service. The offline custody operator is a separate release artifact and is
intentionally excluded from the npm launcher.

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
