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

## Mac welcome sheet follow-up

- The free-name welcome sheet used a flexible List without a Mac sheet size. Set the same 520-point width as the adjacent Settings sheet and a 560-point height so the name, visibility, save and skip controls have room.
- Separate stacked branch preserves the ongoing iPhone/iPad qualification. Docdex symbols/impact returned writer backoff; inspected the adjacent sheet cases and sheetSurface directly. A small local-model draft was validated and used.
- Full PigeonpostMac build passes locally. Native Mac CI pending; no app version or store submission changed.

## 2026-10-29 23:57 UTC — CI permission-fixture and hit-target follow-up

- Current CI: iPhone 37/39 and iPad 53 pass/1 skip/2 fail. Artifacts show synthetic contacts cleared after the real permission-denial test; no real contacts were read in the SMS scenario. Explicit debug discovery fixtures now ignore device-address-book invalidation only; release builds and real/denied contact imports keep both revocation/change checks.
- The iPad verification tap left an enabled row with the correct number and no error; the iPhone call-history Block tap left its menu open. AX frames span blank row space beyond their labels. The tests now target inside the visible label once and keep all completion/confirmation assertions, with no action retry. Local iOS18 unchanged reproduction passed; CI iOS26 remains the authority for these platform-specific paths.
- Docdex symbols/impact/DAG inspected before edits; Swift test/view graphs contain no indexed edges, so direct lifecycle/test call sites were inspected. Local-model tap draft was insufficient and not applied unchanged. Updated iOS app build passed.

Follow-up validation: full local app build; 8/8 iPhone UI cases (all People plus call-history actions) and 7/7 iPad People cases passed with the real permission-denial case retained. CI iOS26 requalification is next.

## 2026-10-30 00:04 UTC — Mac composer focus race

Mac 26 CI on #188 failed the unchanged first-send keyboard assertion; the conversation and sent message were present in retained screenshots. The existing editor advanced its refocus token before its asynchronous attempt could see a window, permanently losing early requests. The editor now consumes only successful focus and retries pending requests when attached or re-enabled; dismantling cancels pending work. Added deterministic native checks for unattached/disabled/superseded/retired requests. Docdex symbols/impact inspected (Swift graph edges empty); direct MacThreadView and harness call sites reviewed. Local model draft was invalid and rejected. No other composer text/attachment behavior changed.

Mac follow-up validation: unsigned full app build and standalone native harness compile passed. Runtime harness explicitly refuses the locked local desktop, so native execution remains a CI requirement. Initial local signed build stopped on the absent Mac development profile; no signing or store settings were changed.

## 2026-10-30 — Asynchronous native test interactions

- Combined head `4c44a19b` passed general CI, both Mac suites, groups, StoreKit, iPhone messaging/calendar, all seven iPhone People cases and the call-history fix. Two UI timing failures remained; original bundles/screenshots/event traces are retained privately.
- The existing copy-address test asserted `/` immediately after Paste; its failure screenshot and later accessibility snapshot both contain the expected `/bekir/docdex`. The helper now waits up to five seconds for the exact value before the unchanged equality assertion, with one paste action.
- On iPad the phone verification tap used y=395.5 while keyboard dismissal moved the button to y=565.5. The test now dismisses the native iPad keyboard first, waits for it to disappear and taps Send once using the native element. This supersedes the earlier guessed quarter-width tap; no production hit-target defect was demonstrated. The earlier CI hierarchy confirms the native `Hide keyboard` label.
- Local iPad verification and three iPhone clipboard/verification tests pass. The local iPad keyboard was already hidden, so the explicit dismissal branch still requires the iPad26 CI run. All success, consent and verification assertions remain intact; no real messages were sent.
- Docdex symbols/impact/DAG were checked; Swift graph edges are incomplete, so direct callers were reviewed. The delegated clipboard draft had invalid XCTest syntax and was rejected. Only UI tests and this evidence log changed; production Apple source is unchanged.

## 2026-10-30 — Hidden keyboard geometry and draft preservation

- Head `537ff59b` passed general CI, both Mac suites, groups, StoreKit, iPhone messaging and calendar. The account suite passed 38/39: one unchanged device-approval case ran on an entirely black simulator scene with all AX geometry scaled to one third. Its original bundle is retained; no application or device-approval assertion was changed for that failure.
- iPad passed 53 cases with one platform skip and two failures. The phone screenshot shows a fully hidden keyboard, while AX retains a Keyboard at `(0,1133,744,0)`. Dismissal now waits for the keyboard's visible frame intersection to be empty. It still dismisses once, sends once and requires completed six-digit verification.
- The calendar screenshot shows the entered `abc` draft correctly retained after validation. Native focus had selected the existing number, so typing replaced it. The test now captures the actual invalid draft before Save, requires the typed invalid text to be present, and asserts exact preservation after the validation error.
- Three targeted iPad and three iPhone cases pass locally: invalid reminders, device-approval denial race and phone verification. The software keyboard was enabled for these dedicated simulators; the iPad trace confirms the real Hide keyboard action ran. The original hardware-keyboard preference was restored and both owned devices shut down.
- Docdex symbols/impact/DAG were checked before edits; the test files have no reported graph edges. The local-model draft invented XCTest APIs and repeated Save actions, so it was rejected. Only the two UI tests and this evidence log change; application source remains unchanged. Fresh current-head CI remains required.
