# Native calendar and scheduled meetings — progress

Plan: [implementation plan](native-calendar-2026-10-25-plan.md).

## Current state

Isolated worktree `pigeonpost-native-calendar-20261025`, branch `codex/native-calendar-20261025`,
based on `9011334` / PR95. Response/time models and the API layer are implemented with 40 model
and 18 transport checks passing. The mailbox store passes22 optimized state checks. Screen work is
next; no calendar navigation or store submission.

| Area | State | Required evidence |
|---|---|---|
| Models/time/API | 40 model and 18 transport checks pass | Server integration with the store/editor |
| Store | 22 state checks pass | Real server and screen integration |
| Agenda/event screens | Pending | CRUD, recurrence/scopes, layout/accessibility |
| Invitations/settings/import/export | Pending | Admission, redaction, capability custody |
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
