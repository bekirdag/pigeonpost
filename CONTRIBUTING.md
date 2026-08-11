# Contributing to Pigeonpost

Thanks for your interest. Pigeonpost now has an implemented Rust workspace, release machinery, and
operator documentation. The most valuable contributions are concrete failures—especially cases
where code, tests, and the SDS disagree.

## What helps most

**1. Attack the design.** The parts most likely to be wrong, in order:

- **Naming** (`docs/architecture.md`) — key addresses are self-certifying and need no registry.
  Is the resolution story actually authority-free? Is 128 bits the right truncation?
- **Spam** (`docs/spam.md`) — a stored wrap does not reveal an authenticated sender identity, so
  sender reputation is client-side only. Does the capability-token scheme survive a determined
  harvester?
- **Key lifecycle** (`docs/keys.md`) — pre-committed successor keys are what let an address survive
  compromise. Is the trust-on-first-use pinning of that commitment strong enough in practice?
- **Neutrality** (`docs/infrastructure.md`) — the five day-one commitments are meant to keep a future
  fork possible. Is one of them insufficient, or is one missing?
- **Integration surface** (`docs/integration.md`) — if you build agent tooling, does this surface
  cover what you'd need? Missing calls are more useful to hear about now than after it ships

Open an issue with the concrete failure case. "This breaks when X does Y" beats "have you considered".

**2. Prior art we missed.** The survey at the end of `docs/architecture.md` lists what was evaluated
and rejected. If something belongs there, say what it does better and what it costs.

**3. Run a witness.** This is a bootstrap ask, and it does not require agreeing with anything above.
A conforming independent witness verifies append-only consistency, keeps durable state, and
cosigns C2SP checkpoints. It has to be operated independently; another process under our control
proves nothing. If you can operate one and participate in equivocation drills, open an issue.

**4. Commit to running a loft.** The other bootstrap ask, and the one the project's survival actually
depends on. Pigeonpost is free and has no revenue, so it is designed for the storage cost to land
with whoever runs the agents rather than with us (`docs/capacity.md`). A $5/mo VPS holds roughly
10,000 agents at 30-day retention, and the install is meant to be `pigeonpost install` with no flags
(`docs/node.md`). If you plan to run agents at any scale, you are the intended operator — say so in
an issue and it will shape the directory design.

## Working on code and docs

- Markdown, wrapped at ~100 columns, matching the existing files
- State the trade-off, not just the decision. Every design doc here says what was rejected and why —
  keep that, because the rejections are the useful part
- Mermaid for diagrams (`docs/infrastructure.md` has examples)
- Product name is **Pigeonpost** in prose, `pigeonpost` in identifiers. Never "PigeonPost"
- Add or update executable evidence for behavioral changes; protocol changes require published
  conformance vectors
- Run the formatting, lint, test, conformance, custody-boundary, and launcher gates from CI

## Build contract

The build spec is [`docs/sds.md`](docs/sds.md): a Rust workspace with explicit platform-custody
boundaries, one online product binary, one separately built offline compliance binary, and seven
milestones. Language, package boundaries, and compatibility rules are settled.

Keep changes scoped to a demonstrated requirement or defect. Large changes should say which SDS
obligation they satisfy, include impact analysis, and preserve the online/offline custody boundary.

## Reporting issues

Include what you expected, what the doc says, and the case where they diverge. For anything
security-shaped — key handling, the encryption envelope, the registry's integrity properties — say so
plainly in the title so it gets read first.

## License and the CLA

Pigeonpost is owned by the maintainer. The code is published so it can be read, forked, and built on
(see [LICENSE](LICENSE)), but the maintainer keeps the sole right to license and sell it, including
operating the paid handle registry.

So the project can accept help without giving up that ownership, **every contribution is accepted
under the [Contributor License Agreement](CLA.md)** — a short grant that lets the maintainer use,
relicense, and sell your contribution. It is a license, not an assignment: your copyright stays
yours.

By opening a pull request you agree to the CLA. Until an automated check is in place, add the
acceptance line from [CLA.md](CLA.md) to your first PR.
