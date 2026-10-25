# Native calendar and scheduled meetings — plan

Continue SDS §14B and native round two, using [calendar contract](calendar.md) and the deployed
web implementation as the specification. This worktree starts from native actions checkpoint
`9011334` / PR95 so that checkpoint can qualify without source changes.

## Build order

1. Add tolerant response DTOs, strict authoring/time models and mailbox-scoped API calls. Preserve
   literal wall time and IANA zone, refuse nonexistent times and ask Earlier/Later for a repeated
   time. Keep all-day exclusive end dates and recurring occurrence ids intact.
2. Add a shared Apple calendar store with generation-scoped reads/mutations, exact failed-create
   request keys, revision-conflict recovery and capability gates. Calendar details are in-memory
   per mailbox; no lock/E2EE claim. Do not silently create the default calendar on a read.
3. Add dashboard calendar navigation, month/day agenda, calendar management, event creation and
   editing, recurrence/scope selection, cancellation, invitations/RSVP and reminders. Filtered
   invitations remain separate and answering never creates a contact.
4. Add scheduled meeting controls through the existing native call renderer, including explicit
   occurrence choice, lobby/host permissions and cancellation/window errors. A locator grants no
   role; joining or accepting an invitation does not add anyone to a linked group.
5. Add calendar privacy/timezone/working-hours settings, export/subscription/import, contact-guest
   link copying and reminder/deep-link routing. Respect the email capability and never fetch
   calendar-import URLs or display guest contacts to attendees.
6. Qualify Apple on local and newer-runtime iPhone/iPad/Mac checks, then port the same contract to
   Android, Windows and Linux as part of the native release batch.

## Validation

- Wire fixtures and real postbox cases for mailbox ownership, feature-off/old servers, optimistic
  revisions, idempotency and late results after switching mailboxes.
- Exact wall-time strings, gap/fold cases, 23/25-hour days, recurring timezone behavior, all-day
  exclusive ends and opaque moved-occurrence ids.
- Native interaction/layout/accessibility cases for agendas, invitations, edit/cancel scopes,
  conflicts, privacy settings and scheduled meeting permissions.
- ICS/import/feed privacy, capability-token custody, removal/404 cache erasure and call renderer
  regressions. No production invitations or messages to real recipients during tests.

## Boundaries

Nests/Products remain planning only. No paid infrastructure or new third-party app dependencies.
Postmark domain setup still belongs to the owner prerequisite list. Keep one native store batch;
do not submit Apple for review before the owner's Claude review step, and remove mainland China
before a CallKit submission. Do not change PR95's source while it qualifies.
