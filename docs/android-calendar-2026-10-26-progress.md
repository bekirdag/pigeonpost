# Android calendar progress

Plan: [Android calendar and scheduled meetings](android-calendar-2026-10-26-plan.md).

- Created an isolated child of `093a646` so the authorization candidate and its build inputs stay unchanged during full validation. Its runtime changes are `3ef4c57`; earlier checks passed 165 JVM cases and 42 native workflows, with final full/release/real-postbox qualification still running.
- Read the calendar wire contract and the Apple API/model/time/store architecture. Calendar is per canonical mailbox, reads must not create the default calendar, creation uses a stable body/key, and service-side encryption must not be described as E2EE.
- New worktree Docdex memory is empty and initial index stats reported writer backoff. Exact file reads work. Retry index health and AST/impact/DAG before implementation; do not restart the shared daemon or index umbrella directories.
- No calendar code, production write, external invitation or store submission yet. Billing callback custody and remaining cache lifetime hardening remain explicit parent follow-ups.

## Time and response foundation

- Added tolerant response DTOs and strict IANA-zone/Gregorian authoring helpers. Unknown statuses remain strings; absent permission fields grant nothing. Contact-guest/feed capability URLs have redacted debug descriptions. Source event JSON has an in-memory slot to retain unknown nested policy fields during an explicit edit.
- **12 new JVM cases** pass for ASCII/format/calendar validation, literal seconds, gap refusal, explicit fold offsets, 23/25-hour days, a fully skipped date and its preceding interval, end ordering, 42-day month bounds, exclusive all-day ends, opaque moved occurrence identity and redacted DTO defaults. The full inherited core suite now passes **177 cases, zero failures/skips** (`android-calendar-models-qualified.log`). A private-constructor data-class copy visibility warning was removed with an immutable value-equality class.
- Index health recovered (997 documents before these files); AST/impact/DAG were used. Kotlin symbols/edges are empty, so source call sites and compiler/runtime checks supplement the graph. Direct Gradle is the documented fallback because the repository-level Docdex runner selects Cargo rather than the Android Gradle tasks.
- Local self-hosted CodeLlama delegation returned 404 Unknown self-hosted model. The fallback supplied three DTO shapes but incorrect camelCase wire names and a missing default; corrected those against the contract and verified serialized names. No paid fallback was used.
- Fast-forwarded the parent authorization evidence/test-only checkpoint `d519f3b`. Its final qualification is 165 JVM, 76 native (73 pass/3 existing skips), lint/unsigned release and direct/group real-postbox interoperability. Message actions PR #113 merged as `581c7c6`; authorization PR #114 is draft and qualifying. No store submission.
