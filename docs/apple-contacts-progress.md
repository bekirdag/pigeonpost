# Apple contacts progress

- Isolated checkout `pigeonpost-contacts-apple-20261029`, branch `codex/contacts-apple-20261029`, base `78a8118e` (merged contacts server/web). PR #180 remains separate while iPhone/iPad CI runs.
- Initialized Docdex at this exact worktree root; HTTP-scoped tree and repo memory recall succeeded. Global MCP session is pinned to main, so worktree calls use explicit repo-id HTTP requests.
- Read contacts contract, shared Account authorization/acting-generation boundary and organization-room API/store patterns. Plan recorded before implementation; no application edits yet.

- Implemented shared typed API, scoped PeopleStore, local protected/excluded-from-backup alias files, PhoneNumberKit adapter, names/phone/discovery UI, explicit invitations and contact-name labels. Exact-root Docdex symbol/open/impact recovered; Swift file graph has no edges, so verified call sites directly. Local-model DTO draft was used only after adding Sendable/reviewing every field.
- First focused suite: 42 checks pass, including payload privacy, malformed matches, login/inbox replacement, quotas and invitation cancellation/replay. First Mac build caught iOS-only limited Contacts status referenced on Mac; guarded by platform. Corrected before retry.
- No real contacts, SMS, phone verification or store submission used by tests. Native fixtures keep all network operations in memory.

- Full existing Apple core suite passed, including 46 people checks with on-disk isolation/backup exclusion. Subsequent exact-match phone validation and three published cross-client vectors bring the focused suite to **50 passing checks**. Mac and iPhone/iPad simulator builds pass. Four initial native UI cases pass. Adding actual Contacts-denial, six-digit verification and invite-confirmation UI cases, all on a dedicated empty simulator with synthetic API responses.
- Local-model UI test draft failed validation (undeclared app, missing launch scope and no actual cancellation) and was not used. Primary implementation uses real app IDs and waits.
- Added the contact tests to both native CI selections and PhoneNumber to the privacy manifest; Mac now packages the same manifest. No App Store Connect changes made.

- Expanded Apple UI run: disclosure cancel, actual OS Contacts denial/recovery, skippable name, free name, phone settings and six-digit verification passed. SMS confirmation exposed a real SwiftUI dismissal race: the binding cleared the pending review before the async send began. Confirmation is now consumed synchronously and carried as a one-use, scope-bound intent; a regression simulates the automatic binding reset. Requalification pending.

- SMS dismissal regression fixed: **51 core checks and all 7 native UI tests pass**, including actual Contacts denial/recovery and explicit invitation cancel/confirm. Further legacy-name preservation adds a 52nd passing core check. Final iOS and Mac builds pass. Visual review of the synthetic Find people screen confirms readable native layout and separate invitation action.

- Review caught initial Settings reads being skipped during background profile loading. Read-only initial loads now wait for the existing operation with cancellation and scope checks; mutations remain one attempt. Three delayed-read regressions bring the focused suite to **55 passing checks**. Onboarding forms reset on scope change. Rebuild/retest after combining PR #180 remains required.

- Combined PR #180 (`2b27ed98`) into this branch. Resolved first-message constructor/selection and CI conflicts: contact-prefilled recipients now use the scoped `Inbox.started` path; all existing suites plus PeopleUITests remain selected (account shard and iPad). Requalification follows before opening the PR.

- Combined full core script passes (55 People checks and the existing suites). iOS/Mac builds pass. Seven contact UI cases pass. The first-message discard case exposed a real empty-thread/retained accessibility row defect in #180; fixed there and merged back. Both unchanged first-send/discard UI cases now pass. No native version bump or store submission. Phone/iPad/Mac CI remains the merge gate.

## iPad confirmation qualification

- iPad baseline: 8 selected tests, 2 failures because native confirmation popovers omit a visible Cancel action. Both contact disclosure and SMS confirmation now use explicit alerts; action binding and one-use send guard are unchanged.
- Unchanged iPad tests: 8/8 pass (7 People tests plus first-conversation navigation). Unchanged iPhone confirmation tests: 2/2 pass after the change. Logs: `apple-contacts-ipad-alert.log` and `apple-contacts-iphone-alert.log` in the private continuation evidence directory. No real contacts, SMS or messages were used.
- Docdex symbols resolved the view; impact graph returned no edges, so the Settings/onboarding call sites and shared iOS/macOS usage were checked directly. Local delegation returned an invalid alert signature; it was rejected and corrected before compilation.

- Fresh Mac CI exposed the standalone inbox harness missing the new People types (the full app built correctly). Split free-name and contact-label views from phone/permission views, and include the real People model/API/store/name/label sources in the harness. This keeps its actual Inbox and native view assertions intact without stubbing the new labels or adding PhoneNumberKit to an unrelated harness.

- Mac harness now compiles with real People sources and the updated `initialPeer` dialog test double; local UI execution stops at its existing locked-desktop guard (exit 69), so no local UI pass is claimed. Fresh full iOS and macOS app builds pass after the view extraction. CI will execute the unchanged Mac UI assertions on an unlocked runner.
