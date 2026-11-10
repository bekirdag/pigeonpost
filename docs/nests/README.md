# Pigeonpost Nests

A Nest is your AI agent and its private cloud workspace, reached through an ordinary Pigeonpost
conversation. This folder holds the tracked Nests documents.

| Document | Status |
|---|---|
| [phase-a-plan.md](phase-a-plan.md) | Phase A: built, deployed and tested behind the `nests` flag (off in production); gVisor host, control service, model broker, enrollment |
| [../../deploy/nests/README.md](../../deploy/nests/README.md) | Runbook for the Nests host |
| [phase-b.md](phase-b.md) | Phase B: first-party Nests — packages, purchases, slots (B1), then Products, chat, files, management, data safety, org Nests |
| [backups.md](backups.md) | Nest backups to raijin's HDD: append-only borg over WireGuard, key escrow, restore |
| [contracts/README.md](contracts/README.md) | Marketplace core API contracts (OpenAPI 3.1 and JSON Schemas); the M0 routes and the M1 reference path (native jobs, staff review and attestation, quarantine) are implemented behind the `marketplace` flag, the rest proposed; records the owner's deviations from the build plan |

Local planning folder (`docs/planning/`, not tracked):

| Document | Status |
|---|---|
| `2026-10-21-hosted-agents-plan.md` | Original Nest product and infrastructure plan |
| `2026-10-21-products-and-agent-services-plan.md` | Products page: Buy an Inbox, Buy Nests, Services |
| `2026-10-21-hosted-agents-progress.md` | Planning and Phase A progress record |
| `2026-11-03-nests-marketplace-build-plan.md` | **Next build plan**: Nests Phase B, then the marketplace and developer environment. Starts after the current build plan (big-features SDS R-01…R-46 and its audit fixes) is finished |
| `2026-11-03-NESTS-MARKETPLACE-ALIGNMENT.md` | How the owner's external marketplace specification (v5) fits the current system |

Order: Phase A (done) → Phase B (first-party Nests: purchases, Products page, chat, files and tools
with approvals, backups) → marketplace phases M0–M6, each behind its own flag and gate.
