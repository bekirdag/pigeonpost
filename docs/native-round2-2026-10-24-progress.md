# Native round two — progress

Plan: [implementation plan](native-round2-2026-10-24-plan.md).

## Current state

Shared Apple message-action DTOs, API wire calls and conversation metadata are implemented and
tested locally. The composer/actions UI, privacy controls and other platforms are still pending.
Isolated worktree `pigeonpost-native-round2-20261024`, branch `codex/native-round2-20261024`, based
on PR #87 at `b7a7eff` (including merged #90). No native release or production deployment has been
made in this phase. Existing PR checks are being finalized in their original worktrees.

| Area | Status | Required evidence |
|---|---|---|
| Apple actions/receipts | API/metadata implemented; local model/wire suites pass | Composer, privacy, group actions and native UI qualification |
| Android actions/receipts | Pending | Core and Compose/emulator tests |
| Windows actions/receipts | Pending | Core, native UI and call/notification regression checks |
| Linux actions/receipts | Pending | Python, GTK and sandbox checks |
| Calendar/meetings | Server contract reviewed | All four native clients, DST and revision semantics |
| Enterprise client screens | Server/native approval contracts reviewed | Roles, rooms, delegation and native MFA checks |
| Release batch | Held | Final builds, store status, Apple review hold and device checks |

## Findings

- Apple shares its API DTOs, conversation assembly, inbox model and message bubbles between iOS/iPadOS and Mac. iPhone history also owns a UIKit context menu; it must change with the shared SwiftUI menu.
- Native inbox queries currently use legacy message rendering. Round two must request `actions=1&fold=1`, acknowledge with `with_edits`, retain metadata while marking rows read and handle late mailbox results safely.
- Composer drafts are scoped by mailbox/peer/subject on both Apple layouts. Reply/edit state must preserve this scoping and failed-send drafts, including when an asynchronous send completes after a mailbox switch.
- Native calendar has no implementation yet. Its data is sealed at rest, not E2EE; do not present it as end-to-end encrypted.
- Scoped Docdex initialization is temporarily holding the new index writer. Source reads work; symbol/impact checks returned `backoff_required`. Retry after indexing before changing code.

## Checkpoint — Apple wire and metadata

- Index initialization completed; symbols/AST and impact checks succeeded before changes. Swift graphs have no dependency edges, so shared app targets and the standalone test scripts were inspected explicitly. Implement DTOs first, then API/projection, then local read-state handling, then UI.
- Additive optional message fields preserve older-server decoding and tolerate future receipt state strings. Inbox queries request folded action metadata; acknowledgements include versions. Send calls carry a random 128-bit base32 reference and preserve it, reply and body through a real simulated 401/token-renewal retry.
- Conversation assembly retains replies, edit metadata, disappearing expiry and sender receipt/agent progress. Local read marking now copies the entire value before changing its read bit, retaining present and future metadata.
- Model cases verify reference encoding/padding, exact 24-hour/expiry edit boundaries, legacy decoding and Unicode quote limits. Quote bounding preserves whole graphemes while staying within the server's Unicode-scalar budget.
- Full standalone Apple Swift model suites pass in `validation/native2-apple-wire-models.log`, including **88 inbox/wire checks**, 31 group logic checks and 33 Phase 1b checks. This is API/model evidence, not native screen or newer-runtime qualification.
- The three small DTO declarations came from a local draft and were reviewed against the contract before use. No new dependencies or authentication/authorization shortcuts.
