# Native calendar and scheduled meetings — progress

Plan: [implementation plan](native-calendar-2026-10-25-plan.md).

## Current state

Isolated worktree `pigeonpost-native-calendar-20261025`, branch `codex/native-calendar-20261025`,
based on `9011334` / PR95, with its final repair merged via9dfb7f9. Current local qualification:
40 time/model +36 draft +20 transport +24 store +34 real-postbox checks,8 iPhone and8 iPad
interaction cases, and both app builds pass. The Mac calendar window is visually checked. Remaining
calendar controls and hosted CI follow; no calendar release or store submission.

| Area | State | Required evidence |
|---|---|---|
| Models/time/API | 40 model,36 draft and20 transport checks pass | Real server integration with store/editor |
| Store | 24 state checks and real server checks pass | Final screen/CI qualification |
| Agenda/event screens | 8 cases pass on each device | Hosted newer-runtime qualification |
| Invitations/settings/import/export | Invitations/settings implemented; import/export/feed screens pending | Capability custody and remaining UI |
| Scheduled meetings/routing | Contract read | Native bridge, lobby/host/occurrence cases |
| Other native platforms | Pending | Android/Windows/Linux parity checks |
| Release | Held | CI, artifacts, owner/Claude review and real devices |

## Findings

- The calendar belongs to a mailbox. Calendar metadata is sealed at rest and readable by the
  service; it must not inherit encrypted-chat lock wording.
- Server dates use literal wall times plus IANA zone, with explicit fold choice for repeated
  times. Foundation date normalization cannot silently change what the person entered.
- Scheduled meeting codes are locators, not credentials. Joining uses the calls service's
  verified participant/occurrence authorization, with host powers independent of arrival order.
- New-worktree Docdex initialization returned index-writer backoff. File reads work. Wait for
  index readiness before model/navigation code changes and structural/impact analysis.

## Time and response-model checkpoint

- Index initialization recovered; scoped symbols/impact/DAG reads completed. Swift edges are
  empty, so inspected both Xcode synchronized Shared roots and standalone source lists directly.
- Added tolerant response DTOs and strict calendar date/wall-time helpers. A real Foundation
  prototype verified gap refusal and distinct Earlier/Later instants before adopting strict
  matching. A local draft supplied the three reviewed folder/person/reminder declarations.
- **38 actual checks pass** (`native-calendar-models.log`), covering calendar validity, Unicode/
  format rejection, Istanbul seconds, New York spring gap/fall fold,23/25-hour days, a42-day month
  whose UTC duration crosses DST, original occurrence identity, unknown statuses, redacted guest
  projections and preview opt-out. Registered the suite in the existing Swift runner.
- These are foundation checks, not calendar screen/API or release qualification. UI, calls bridge,
  import/export and other native platforms remain pending. PR95 continues qualifying separately.

## Mailbox API checkpoint

- Added mailbox-header API calls for calendars/events, invitations/RSVP, scopes/revisions, guest
  links, settings/reminders, subscriptions, free/busy and ICS import/export. Encoded create bodies
  survive token renewal unchanged; caller-provided ids cannot normalize into another route.
  Unknown availability stays distinct from a known empty interval list.
- The full standalone Swift runner passes: calendar **40 models +18 API checks**, with existing
  inbox128/group84/Phase1b38 suites unchanged. Transport cases use the real PostboxClient and
  controlled URLProtocol:401 renewal, exact bytes/id, mailbox isolation, integer revisions,
  occurrence scope, window/path rejection, raw ICS, external-reminder rejection and privacy-only
  settings updates. Evidence: `native-calendar-full-models.log`.
- Both iPhone and Mac builds pass (`native-calendar-foundation-build.log`,
  `native-calendar-foundation-mac-build.log`). A temporary task-owned symlink reused the existing
  build cache and was removed afterwards; cache files were preserved. Calendar screens, scoped
  store behavior, scheduled-call bridge and real server integration remain unqualified/pending.

## Mailbox state checkpoint

- Added an observable shared store whose reads, details and mutations capture the acting mailbox
  and a generation. An older mailbox's result cannot restore its data or clear a newer operation.
  A missing event also invalidates sibling in-flight details. No calendars selected is distinct
  from all calendars; Today adopts the saved timezone after preferences load.
- Create retries retain the same canonical payload/request key for an ambiguous failure, with an
  in-memory16-attempt/24-hour budget. This is not crash-persistent retry storage. Definite rejected
  requests free their slot. Revision conflicts preserve the caller's draft and are not rebased.
- **22 optimized state checks pass** (`native-calendar-store.log`). The first fixture incorrectly
  let an optimized build release the client's weakly held token provider; retaining it in the
  fixture host fixed the harness. No product workaround was needed. Registered the suite in the
  existing standalone runner. Calendar screens and real-server meeting tests remain pending.

## Editor and initial screen qualification

- Added a frozen-revision editor: whole-series and effective occurrence values stay distinct;
  unchanged known fields are absent from PATCH. Attendees, exceptions, meeting policy and locale
  survive unrelated edits. Repeated times require an offset choice; all-day last dates are shown
  inclusively and sent with an exclusive end.36 draft checks pass (`native-calendar-draft.log`),
  including malformed reminder input never turning into an unintended “remove all”.
- Connected capability-gated Calendar entries on phone and desktop/iPad, a shared scoped lifecycle,
  month/day agenda, event create/edit/cancel, RSVP, separate filtered invitations, personal reminders,
  calendar management, and privacy/timezone/working-hours forms. Cancellation confirmations capture
  the reviewed revision rather than adopting a poll's newer revision. Remaining attendee management,
  import/export/feeds, scheduled-call bridge, deep links and real server cases are still pending.
- Inspection found and corrected a foundation API defect: subscription routes are
  `/v1/calendar/feeds/{id}`, not `/v1/calendars/{id}/feed`. Added read/revoke transport cases; their
  rerun is pending. Do not treat the earlier18 cases as proof of those corrected routes.
- Both initial iPhone and Mac screen builds pass. First simulator launch nevertheless caught a
  UIKit self-sizing loop in the lazy month grid (row height295↔357); replaced it with six eager,
  measured week rows. Runtime repair verification and interaction tests are pending. Evidence:
  `native-calendar-launch-system.log` and the local app crash report; do not count the first build
  as a successful launch. Test-only calendar transport handles a reserved `.invalid` origin and
  uses synthetic mailboxes; no real invitations or production writes are used for these UI checks.
- Upstream PR95 merged as `845a02a1022982f134d34f8425a2b89fab1b072a` after all current CI jobs
  passed at63b60b5: iPhone59 cases/3 existing skips/0 failures, iPad/StoreKit, Mac14/26, core Windows/
  Linux/Mac, MLS and audits. The local calendar branch includes its repair via9dfb7f9. The matching
  core image is building separately; it has not yet been deployed at this checkpoint.

## Runtime interaction follow-up

- The eager month rows fix the launch crash. The first actual navigation check then caught List's
  automatic button style invoking Previous/Next/Today together; explicit plain buttons fix that.
  Launch,44-point selected day, month navigation, Today and agenda/detail pass in
  `native-calendar-launch-buttons.xcresult`.
- The full Swift runner passes40 calendar models +36 drafts +20 transport +22 store checks,
  alongside inbox128/group84/Phase1b38 (`native-calendar-full-editor.log`). The corrected feed routes
  are now covered by the passing transport cases, still awaiting real-postbox integration.
- First8-case iPhone suite:4 pass,4 fail (`native-calendar-iphone-first.xcresult`). Inspection
  distinguishes harness mistakes (an enabled toolbar wrapper instead of its disabled Button;
  Done in an underlying sheet) from product gaps (a secondary toolbar submenu losing its explicit
  entry, and a reminder validation error scrolling offscreen). The conflict itself correctly
  retains the unsent draft and disables Save. Added pinned feedback and explicit toolbar placement;
  focused rerun and iPad checks are in progress. No assertions of completed calendar qualification.
- PR95's matching core image is now deployed after source-tree equality, SQLite backup/quick_check,
  isolated candidate and rollback-image canaries. Source845a02a; both services use
  `pigeonpost-postbox:apple-actions-63b60b5`, id
  `sha256:417d07d199cb28f03b557946bf733e44993e283655fea401d97348b6d0831642`.
  Backup `/var/backups/pigeonpost/postbox-20261025T043832Z` stays on the server. Both services run
  with0 restarts; runtime env files match the pre-deploy copies, source is clean, public health200
  and unauthenticated capabilities401. The original live database is retained. No native store submission.

## Scoped screen and real-server checkpoint

- The iPad's slower conflict flow exposed a real store defect: polling cleared a write error but
  retained its conflict flag, hiding recovery while Save stayed disabled. Successful reads now keep
  unacknowledged write errors, and failed incidental reads cannot replace them.24 optimized store
  checks pass, including unconfirmed-create and stale-edit messages surviving a successful poll.
- The current iPhone app passes all8 calendar cases (`native-calendar-iphone-sticky-qualified.xcresult`).
  The full iPad run passes7/8; the remaining occurrence edit case hit a menu before it became
  hittable. Popup actions now wait for existence/hittability instead of swiping, which can dismiss
  iPad popups. That focused case passes (`native-calendar-ipad-popup.xcresult`); a coherent full8-case
  rerun is in progress. Both actual app builds pass. No simulator assertion has been removed.
- **34 real postbox checks pass** (`native-calendar-e2e.log`), through the native client/store and
  a throwaway loopback issuer/database: explicit stranger filtering, agenda exclusion, response
  without contact creation, co-host/organizer authority, redacted guest contacts, personal links
  with automatic delivery disabled, stale revisions, stable moved ids, cache erasure after removal, reminders,
  partial privacy settings, feeds/read-once links/revocation, ICS export/import and a response lost
  after commit. The retry retains exact body hashes/request key and creates one event. Fixtures
  initially assumed second-suffixed response times and a pending invitation without admitting its
  sender; corrected them against actual server canonical formatting/admission, preserving the
  semantic identity/authorization assertions.
- The shared postbox fixture has an optional calendar mode with synthetic verified addresses;
  original group mode remains compatible. The existing65-case Swift/browser real-MLS suite passes
  after this change (`native-calendar-fixture-groups-regression.log`). No real invitation emails,
  candidate data or production mailboxes are used. CI now includes calendar UI on both devices and
  the real calendar suite beside the existing group suite; hosted execution is pending.
- Remaining server correction before release: `calendar/time.rs::fmt_local` drops nonzero seconds
  as well as formatting zero seconds without a suffix. Imported/authored nonzero-second times must
  round-trip before the native store batch. This is a discovered server defect, not fixed here yet.
- New UI work is still a checkpoint: scheduled-call joining/routing, attendee editing/placement,
  linked group management, import/export/feed screens, broader Mac runtime checks and the other
  native platforms remain pending. Native stores remain held. Temporary build-cache link removed;
  the shared cache is retained.

## Calendar screen checkpoint ready for hosted CI

- Coherent final iPad run passes all8 cases (`native-calendar-ipad-final.xcresult`), alongside the
  passing8-case iPhone run. A local popup helper now waits before tapping; it does not swipe away
  an iPad menu while trying to locate its command. Current Mac/iPhone builds pass, workflow YAML
  parses with Ruby's standard parser, and staged whitespace validation and Docdex hook pass.
- A real Mac fixture launch exposed an omitted second toolbar menu despite a successful build.
  The Mac Calendar options menu now lives in the month header; the iOS toolbar keeps its tested
  layout. Visually checked the actual Mac window (`native-calendar-mac-final.png`), then stopped
  only the task-owned fixture process. This is a runtime/layout check, not a full Mac interaction
  suite. No real account data or other applications were captured.
- No release claim: CI and the remaining calendar controls are still required. Native store
  submissions remain held for the combined batch and the owner's pre-App-Review Claude review.

## CI vocabulary follow-up

- PR96's initial lint job rejected address-transport terminology in new copy and a bare lifecycle
  property, before Clippy. Updated the copy to contact/invitation wording and renamed that property.
  The local synthetic OIDC issuer must retain standard claim keys; it receives one explicit file
  exception beside the existing protocol exceptions, with the vocabulary rule otherwise intact.
- The exact repository vocabulary guard passes locally. Both app builds pass after the rename
  (`native-calendar-terms-build.log`, `native-calendar-terms-mac-build.log`). This changes wording
  and an internal property name; the qualified API payloads, UI controls and permission checks
  retain their behavior. Hosted qualification must use the new commit.

## Mac integration harness follow-up

- Hosted Mac jobs on both OS versions exposed an omitted dependency in the standalone inbox
  integration harness: its fixed swiftc source list did not include the new calendar sources.
  Added the real calendar files and made the existing Session test double observable with its
  signed-in state. The application targets already compile these shared files automatically.
- A local delegated draft omitted required methods/observation; rejected it and applied the
  minimal correction directly. The complete harness compiles after the correction. Runtime exits69
  at its existing unlocked-desktop prerequisite on this Mac; no runtime pass is claimed. Hosted
  desktop regressions remain required. All128 non-UI inbox checks pass with the shared stub change
  (`native-calendar-harness-inbox.log`).

## Hosted iPadOS26 accessibility follow-up

- At072c581 the Mac14/26 jobs, real MLS/calendar suites, all Rust/web checks and StoreKit pass.
  Hosted iPad has6/8 calendar cases passing and the existing layout suite has0 failures/1 skip.
  The two failures are test assumptions, confirmed by the retained screenshot/accessibility tree:
  iPad starts with conversations collapsed, and the reload action exposes a nested wrapper and
  button with the same identifier. The actual48pt popover action is present.
- The capability test now uses the app's Show conversations control before looking for sidebar
  entries. The conflict test waits for and taps the hittable matching reload action, retaining
  unsent-draft, disabled-save, explicit-confirmation and reloaded-current-value assertions.
  No production UI or conflict behavior changes. A delegated draft invented APIs and was rejected.
- Evidence: `pr96-ipad-ci.log`, artifact12589294393 and exported attachments under the private
  release directory. Both focused cases pass on local iPhone and iPad after the correction
  (`native-calendar-ci-followup-{iphone,ipad}.xcresult`). A new hosted run is required before merge.
