# Native round two — progress

Plan: [implementation plan](native-round2-2026-10-24-plan.md).

## Current state

Shared Apple direct-message actions, scoped retries, receipt privacy and status UI pass local and
hosted iPhone/iPad/StoreKit qualification. Authenticated group projection and receipt models are
implemented. Group controls have passed a local interaction run; durable retry recovery is being
qualified in the real transport suite. Combined screen qualification is running. Calendar,
enterprise screens and other platforms remain pending.
Isolated worktree `pigeonpost-native-round2-20261024`, branch `codex/native-round2-20261024`, now
includes main through `1132ad7`. Inherited PRs #83, #89 and #87 are merged. Core and web changes
are deployed as recorded below. No native store submission has been made; Apple's review hold remains.

| Area | Status | Required evidence |
|---|---|---|
| Apple actions/receipts | Direct actions/privacy qualified; group UI and durable retries locally pass | Combined UI and hosted qualification |
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


## Checkpoint — current-runtime UI qualification and composer focus

- Hosted iPad qualification passed at `6d44cf7`. On iPhone every new message-action, receipt/privacy and Phase 1b case passed; the remaining legacy every-draft-shape case failed before typing because XCTest dispatched a tap at one third of the actual accessibility frame. Its failure hierarchy locates the composer correctly and does not establish a production first-responder defect.
- That single test now taps the composer's actual window coordinates, bounds attempts to three, and requires a keyboard before typing. It never retries typed text or a send. All clearing assertions remain. The local focused run executed **one actual test**, passing in `native2-focus-window.xcresult` (29.286 seconds); a fresh hosted iPhone run is still required. Production focus code is unchanged.
- The separate StoreKit restore test failed with an empty post-sync transaction snapshot. PR #87 now waits up to five seconds for the exact expected entitlement set; no additional purchase/sync or cached receipt fallback. The native StoreKit test passes locally; hosted requalification remains pending.


## Checkpoint — authenticated Apple group actions and receipts contract

- Added group receipt DTOs, masked aggregation and identity-scoped REST reads. Late history/member/device/receipt responses are discarded after mailbox changes. UI integration remains pending. The reviewed local DTO draft was used with synthesized equality in place of its redundant implementation.
- Incoming edit/reply references must match their encrypted fields. A mismatch produces a security warning and cannot acquire an edit action through outer metadata. Visible history now folds only a checked inner reference, from the same authenticated sender to an earlier known message; old unverified records and server-only replacement claims cannot hide plaintext. Kept previous-version chains on the device, with iterative projection for long chains and known original expiry/edit-window checks. Previews also drop expired messages.
- Pure group suite **60/60** passes, including canonical/malformed/overflow references, mismatched authors, missing/forward parents, unverified records, pending edits, a2,000-message chain, expiry and masked receipt cases. Real Swift engine+MLS+postbox+browser interop **36/36** passes (`native2-groups-e2e-actions.log`), including encrypted replies/edits and retained versions. iPhone and Mac builds pass (`native2-group-foundation-build.log`, `native2-group-foundation-mac-build.log`). Full standalone recheck is in progress.
- Review found a server gap to repair before the group edit UI: group edits currently take the current group's timer and last version's creation time. Chained edits could extend both expiry and the24-hour window. The direct-message path already inherits its root's deadlines. A separate hardening branch will add real-MLS regression evidence and align group expiry, window and version deletion semantics. Native group controls are not yet enabled by this checkpoint; no store release.


## Checkpoint — hosted suite reaches every test, job budget too short

- At `0db2d3a`, hosted iPad and StoreKit pass. iPhone logs record 50 passing tests, 3 platform/screenshot skips and 0 failed tests, including the exact legacy draft matrix and every new direct action/privacy case. The35-minute whole-job deadline cancelled report finalization immediately after the last Phase1b case. The cancelled run is **not** counted as a successful suite.
- Increased only that job's total budget to45 minutes; no test/assertion/retry/selection changed. Merged Apple `a00c259` (its cache resolution retains all native-call face changes), and will re-run the complete newer-runtime suite. Local core/real-MLS and both app-build evidence remains as above; native group UI, calendar and enterprise screens still pending.

## Checkpoint — hosted Apple qualification and production baseline

- Hosted run `40144248685` succeeds at `3ca3fc3`: full iPhone, iPad and StoreKit jobs complete, including report preservation. This supersedes the cancelled whole-job result above. No test selection or assertion was weakened.
- Main through `1132ad7` is merged into this worktree. Core group lifetime/version-deletion repair is live from `67fa27e`, image `pigeonpost-postbox:groups-276fb28` (`1725df4c…`), with postbox and reaper running and zero restarts. Candidate and old-image DB canaries pass; the live DB was retained.
- Web tree `1fe9961…` is live. Public13 entry assets and12 crypto runtime resources match expected digests; old and new WASM initialize; all196 preserved PDF/call runtime files match source. A private53-file rollback rehearsal passes. Root-only backup `/var/backups/pigeonpost/web-20261025T005415Z` is marked complete. No mailbox messages or tokens were used for these public checks.
- Group UI implementation follows the repaired lifetime/delete contract. Historical server chains on self-hosted installations still need normalization in hardening; production had no edit rows to repair. Native stores remain held for one qualified batch and the owner's Apple/Claude review step.

## Checkpoint — group controls, remote erasure and retained files

- Added group reply/edit controls, quoted-parent navigation/highlighting, scoped composer cancellation/restoration, device-local version information and masked per-member receipt status. Sheet state is unified; mailbox/group identity remounts the composer. Explicit delete/expiry erases every authenticated local version, keys and queued dependent edits. Temporary previews use owned private directories and reject late results after deletion/navigation.
- The native poll now rechecks retained metadata, because polling only newer seqs cannot see a deletion of an older message. The additive metadata-only server query omits application/commit bytes, preserves pagination/member/device/interval checks and never advances delivered receipts. Caption edits can retain only their own current target's attachment ids; references move transactionally, preserving lifetime without an extra upload. Another author, message or group cannot claim them.
- Server full suite **449/449 +2 workers**, clippy with warnings denied and the native postbox build pass. Pure group logic **76/76** and real Swift/MLS/postbox/browser **45/45** pass (`native2-group-postbox-full.log`, `native2-groups-erasure-files-e2e-fixed.log`). Initial runtime qualification caught a Swift exclusivity crash from reading the optional vault while modifying it; staged the read before the write. The corrected real run covers deletion, restart, retained files and later-device access.
- iPhone/Mac builds pass for these controls. Five actual local UI cases pass together in `native2-group-controls-fixed.xcresult`: reply/cancel/send/quote, edit/cancel/save/versions/receipt privacy, chain deletion, capabilities off and existing group send/security lines. The initial UI run exposed a confirmation selection lost during dismissal and controls hidden under the keyboard. Captured confirmation values and scroll after keyboard layout; receipt rows now expose individual accessible status values. All behavior assertions remain.
- Durable pending-send/restart handling is now being qualified separately. These changes have not been submitted to any native store or deployed to production. Docdex's test runner rejected the nested Swift script/Cargo target mapping; repository test scripts and a package-specific Cargo run supplied the real test evidence instead.

## Checkpoint — exact encrypted retry and bounded recovery

- Prepared requests and the advanced MLS sender state are sealed atomically before posting. Ambiguous failures retain the exact JSON, stable client id and original attachment references. Identical failed intents reuse that request; only explicit epoch/key-activation rejections permit re-encryption. Acknowledgement removes the request, and a later deliberate identical send gets a new id. Old vaults retain their original device identity; a restarted in-flight attempt offers Retry/Dismiss instead of an endless spinner. Local dismissal preserves any confirmed server copy and rolls back its local removal if persistence fails.
- The queue has a 16-attempt/eight-MiB body/request budget and a 24-hour retry window. Deletion/expiry erases dependent queued edits as well as history. A resumed own-message confirmation must match the cached ciphertext. Draft fingerprints prevent a history retry from clearing a different composer draft.
- Full standalone Swift suites pass (`native2-group-outbox-models-final.log`): group **84 checks**, inbox **128**, Phase 1b **38**, plus existing auth/thread/navigation/handle/call suites. Real MLS/server/browser **65/65** pass (`native2-group-outbox-bounds-e2e.log`): a loopback fault proxy drops replies both before and after commit, the device restarts, epochs advance, recipient counts remain one, exact JSON hashes agree, files decrypt, explicit rejection re-encrypts under the same id, deleted pending edits cannot retry, capacity is bounded and acknowledgement/dismissal frees slots. Proxy evidence contains hashes/ids only, never headers or plaintext.
- Final iPhone and Mac builds pass (`native2-group-final-build.log`, `native2-group-final-mac-build.log`). Complete group screens and the existing direct/privacy cases are being checked together on iPhone; group screens are being checked on iPad. The first combined invocation failed to compile a new test's element-count assertion; corrected it to use the element query. That invocation executed no tests and is excluded from qualification. Hosted newer-runtime qualification and native release remain pending.
