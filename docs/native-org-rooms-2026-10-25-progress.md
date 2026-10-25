# Native organization room progress

- Isolated branch/worktree from #106 `320c8b8`, fast-forwarded to main `b5690bd` after #106 and #108 merged.
- Read the room server and native group contracts. Rooms are MLS groups with organization
  membership restrictions; the server queues newcomers as keying and holds no group keys.
  Native group refresh already activates provisioning groups created by its acting mailbox.
- Native account mailboxes currently retain address/handle/label only. The implementation must
  use real membership/room responses for organization context rather than guessing from names.
- Native group `GroupSummary.state` defaults to active when membership metadata is absent; room
  discovery must use a separate model or require explicit membership before treating a room as
  joined. No MLS protocol change, real room creation, feature enablement or store submission.

## Models and transport

- Added separate typed organization membership/delegation and room-directory responses. Canonical membership and explicit `read_inbox` delegation associate an inbox; handles grant no authority. Missing or unknown membership cannot open a room.
- Room lookup always includes a validated organization ID. Invalid or missing IDs fail locally, without falling through to the personal public-group directory. Creation retains exact encoded bytes and an idempotency key through ordinary credential renewal, and verifies organization, creator and explicit owner/keying membership in its response.
- Added the existing server `org_id` field to native group metadata so the app can confirm it before navigation. No encryption protocol changed.
- All 68 new model and actual shared-client wire checks pass locally. They cover policies, membership and delegation boundaries, Unicode input limits, malformed identifiers/responses, required query context and byte-identical 401 retry. The full shared runner passes with this foundation.

## Scoped state

- Implemented a state store bound to person, authorization generation, acting canonical mailbox and selected organization. Token acquisition and renewal recheck their context before and after suspension. Read results, confirmation choices and mutation results have independent ownership guards.
- The store hides old data immediately when context changes, before lifecycle callbacks run. Denied membership clears room metadata. Missing membership never opens a cached conversation; every successful join/create is followed by fresh organization/membership confirmation.
- Uncertain creation retains the exact request bytes and prevents accidental edits becoming a new creation. Explicit discard and mailbox changes preserve account backoff. Cancellation clears only its own loading indicator.
- All 84 controlled real-transport state checks pass, including delayed renewal, late creation and joining, and frozen person/authorization/organization choices. A stale form cannot submit under a new grant or another selected organization. Older account/mailbox completions cannot clear a new mutation's busy state.

## Screens and qualification

- Added the directory to the conversation menu on phone, tablet and Mac, behind both group and organization flags. Inbox switching requires an explicit confirmation and is unavailable during active or preparing calls. Private invitations use acceptance; public rooms require an explicit join. Navigation confirms the organization and explicit active/keying membership.
- Added preview and creation screens using the existing native design. Unknown create outcomes retain their draft and exact request; a confirmed room can be reopened without another creation request.
- iOS simulator and universal Intel/Apple Silicon Mac builds passed. Seven new UI cases are registered in both hosted suites. All request fixtures require DEBUG, the explicit fixture flag and a reserved `.invalid` origin; no production rooms or accounts are created.
- First local screen run: phone 6/7 and tablet 4/7. Joining, invitation acceptance, private-room boundaries and read-only behavior passed on both. Recorded failures showed a SwiftUI Menu wrapper whose hit flag differs from its visible button, the iPad confirmation popover's native outside-tap cancellation, and an error row below the tablet's visible form. Updated the test interactions and moved creation errors before retry actions. The iOS rebuild caught an unavailable accessibility modifier; removed it and the build passed. All seven cases now pass on both devices (119.9 seconds phone, 130.1 seconds tablet); earlier failure evidence is retained.
- Final iOS and universal Mac debug builds pass. Visually inspected the tablet directory: organization/inbox context, search, membership explanations, room list and creation control fit the sheet. The complete shared runner passes, including all 68 room wire/model and 84 room state checks.
- The Mac inbox integration harness compiled, then refused to run because this Mac's desktop was locked. No assertion was skipped or treated as passing. The owner has been asked to unlock it while other work continues. The previous native candidate's 197-check run remains earlier evidence, not qualification of this change.

Commands and evidence: `sh apps/ios/Tests/run.sh`; both debug schemes of `apps/ios/Pigeonpost.xcodeproj` (Mac builds both architectures); `OrganizationRoomUITests` from `apps/ios/UITests/PPUITests.xcodeproj` on iOS 18.3 phone and tablet; `sh apps/ios/Tests/mac-inbox.sh`. Build/test logs and both round-two result bundles are retained outside the repository in the continuation release directory. `git diff --cached --check` and `docdexd hook pre-commit` pass.
- Checked room-creation hints against SDS §6.4: all five explicit organization admin roles may create; an ordinary member follows `rooms_creation`. The client matches that server policy.

## Still in progress

Complete the Mac inbox harness once the desktop is unlocked, then hosted qualification. Existing GroupStore authorization/cache hardening remains a release gate before publishing the native batch. No production rooms or accounts were created by the test suite.
