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


## Checkpoint — scoped edit actions and retries

- Added mailbox/peer/subject action state and edit operations with operation identities. Late completions cannot clear a new mailbox's action or in-flight state. Ordinary send failures are also scoped before showing a toast.
- Edits preserve structured request verbs, nested arguments, unknown context and independent notes; only the visible task/question changes. The native work envelope's duplicated note changes with its task. Pending local rows cannot be edited before a server copy exists.
- Identical failed edit retries retain the exact reference; changing text creates a new attempt. Controlled transport checks include a rejected double save, 503/retry, successful folded refresh and two mailbox edits completing out of order. Full standalone suites pass: inbox **117 checks**, groups **31**, Phase 1b **33**, plus the added pure editing/fingerprint cases (`native2-action-behavior.log`, `native2-edit-model.log`).
- Docdex's targeted index request returned an HTTP error, but the subsequent symbol and AST reads recovered; graph results contain no Swift edges, so shared targets and test scripts were inspected directly. Small local drafts for the fingerprint/tests were invalid and rejected; none were applied.
- A direct test command's temporary cleanup was rejected by automatic execution review because it used rm -f. The safe retry retains its test binary in the private release folder and passes. No permission escalation or deletion was used.
- UI, group message actions, calendar and enterprise screens remain incomplete. Attachment-upload identity across an ambiguous send retry still needs review before normal send retries can claim identical payloads. This checkpoint is not a release candidate.


## Checkpoint — direct-message Apple screens and attachment retries

- Added feature-gated reply/edit controls below each card, UIKit/Mac context-menu actions, quoted-message jumps, edit metadata, receipt marks, agent status and message/version information. Shared composer banners preserve an ordinary text/file draft while editing; cancel/save restore that draft. Failed sends retain an exact attempt, including sorted envelope JSON and cached server attachment ids. Cache entries contain digests/metadata only and are keyed by permanent mailbox plus message reference, so navigation does not alter a retry's attachment ids. Replaced attempts drop their upload metadata; failed optimistic rows are replaced on retry.
- iPhone and Mac app builds pass (`native2-app-files-build.log`, `native2-mac-build.log`). Updated Mac standalone source list includes the new shared view. Its first compile exposed the test-only StagedFile initializer's different parameter order; aligned the stub with production and corrected the call. The next Mac GUI run compiled, then explicitly refused the locked desktop session; no GUI bypass or interaction pass is claimed.
- Controlled native transport **128 checks** pass in `native2-file-retry-scope.log`, including the attachment response-loss/retry and mailbox-return case. Full standalone suites passed before that last navigation-cache refinement, with group31/Phase1b33 unchanged.
- New real iPhone UI cases cover reply/cancel/send, capability-off controls and edit/cancel/save/version history. Initial failures exposed inherited accessibility identifiers replacing individual buttons. Removed container identifiers, enlarged actual button hit areas and retained per-action identifiers. Reply and capability-off cases pass in `native2-message-actions-ax.xcresult`; edit/history passes in `native2-edit-visible.xcresult` after geometry-based scrolling corrected test overshoot. All original action/draft/history assertions remain. Version-history PNG was exported and visually inspected. A final combined run and newer-runtime qualification remain required.
- The first UI invocation used a wrong relative path while adding test methods, then ran an empty selection. The resulting zero-test “success” bundle (`native2-message-actions.xcresult`) is retained as failed evidence and is excluded from qualification. Corrected actual cases failed before the accessibility fix and pass as recorded above.
- Read-receipt privacy settings, encrypted-group actions, calendar and enterprise screens still remain. No release or store submission.


## Checkpoint — receipt privacy, status accessibility and parent highlighting

- Integrated the validated #87 head `dfda1fa` without conflicts at `72ce0d0`; this retains the exact composer fixture and purchase-control fixes as well as main's enterprise/device login. #87's newer iPhone, iPad, StoreKit, Mac crypto and group interop jobs pass. Its unrelated Windows Loft fixture failure is repaired on #89 and will enter through main after qualification.
- Added the read-receipt policy control and typed JSON-boolean saves, including the default for older policies. Privacy loads and saves now carry a per-visit generation, reject old mailbox results, and serialize mutations so one stale policy response cannot revert another control. Pure policy tests retain unrelated settings and reject invalid receipt selections.
- Match the SDS presentation: seen marks use the shared green; working shows a slow gear (static plus Working text under reduced motion), failed shows a warning, done returns to receipt ticks. Statuses have specific accessible labels. Reply jumps briefly highlight the parent, with per-jump identities preventing late timers from clearing a newer highlight. Escape cancels the action banner.
- Full standalone Swift suites pass in `native2-privacy-models.log` (inbox128, group31, Phase1b38). Both iPhone and Mac builds pass after the privacy/status changes. All **five** targeted native UI cases pass together in `native2-direct-status-privacy.xcresult`: reply/send, edit/history, feature-off controls, receipt/agent accessible status and the read-receipt opt-out while other privacy features are off. This is local iOS18.3 evidence; the newer runtime still requires CI. Added Phase1bTests to that workflow and parsed its YAML.
- Docdex indexed the policy model/view after an initial missing-index result. Its built-in debug-directory exclusion refuses AST indexing of Debug/Fixtures.swift; the exclusion was preserved. The status cases instead use the already indexed DEBUG-only inbox fixture installer; a release build cannot enable them.
- Group actions/receipts, calendar, enterprise screens and the other native platforms remain pending. Native release/store submission stays held.
