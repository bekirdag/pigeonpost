# Native calendar management — progress

Plan: [management plan](native-calendar-tools-2026-10-25-plan.md).

- The isolated branch began with PR98 head 117742c and production a5389f8. It now includes PR98's
  qualified test corrections through 3436b93 and merged import retry PR99 (main 58459ad). No new Rust
  logic was authored here; the imported backend correction has its own plan, tests and rollout.
- Added scoped attendee, placement, group-link, feed, export and import store operations. Revision
  writes share the event lock; late results cannot cross mailboxes. Status reads strip feed URLs
  and preserve unresolved write errors. Store tests pass 46 checks; existing model/draft/API checks
  remain green. Bounded local file reads and frozen import selections pass 11 focused checks.
- Added participant add/remove screens with captured revisions and explicit series/occurrence
  scope; invitation placement and existing-group linking do not add contacts or group invitations.
  Added private ICS import preview/selection/confirmation, native file export, and subscription
  creation/replacement/revocation. Subscription URLs remain in view memory and are never fetched.
- The real temporary postbox suite passes 46 checks, including private placement, removal and
  reinvitation, feed projection/revocation, raw export and an actual connection loss after an import
  commits. Retrying the frozen native confirmation recovers one copy with identical request bytes.
  The shared fault proxy intercepts confirmations only in its optional calendar test mode.
- Final iOS and universal Mac builds pass. The full native regression script passes, including
  40 calendar model, 36 draft, 11 import-file, 20 transport and 46 store checks. The standalone
  Mac UI harness compiles; its interactive run still requires an unlocked desktop and is left to CI.
- The iPad's complete 19-case calendar UI suite passes with no skips. The iPhone complete run passes
  18 cases; its export-cancellation check assumed a shallow restored picker folder. The corrected
  breadcrumb check passes in a focused rerun. Every calendar case has now succeeded locally, with
  hosted qualification still pending. Private result bundles preserve both the initial failures
  and corrected results; these do not establish calls or file saving on physical devices.
- A reproduced iPhone participant-reload crash came from indexed SwiftUI bindings outliving their
  array rows. The shared invitee fields now resolve by stable UUID in both management and event
  creation. Calendar participant reload/removal and draft regressions pass. The subscription screen uses an
  explicit sheet after eager navigation reproduced a main-thread layout loop on both simulators;
  creation, one-time display and revocation now pass on both.
- DEBUG fixtures use only the reserved .invalid transport and synthetic data. A bounded host helper
  stages a marked synthetic ICS in each explicit simulator's local document provider, refuses
  unrelated folders/files and never deletes data. A real document-picker interaction selects that
  file and reaches the preview on both devices. A separate seeded-preview check covers response-loss
  recovery; the real temporary postbox verifies parsing, canonical confirmation bytes and one copy.
- Hosted parent CI exposed independent message-edit input failures in existing composer/group UI
  tests. Those require investigation before the parent PRs merge; no failing checks are bypassed.
  PR98's isolated Mac Rust deadline test is being rerun once after recording its late in-flight
  peer-arrival timing assumption for hardening. Production import PR99 remains fully qualified.
- All stores are held. Nests/Products remain planning only; no hosting implementation is in scope.

## Parent composer qualification

Integrated the conversation/edit-scoped input fix from PR96 through PR98. Both native builds, all 10 iPhone composer cases, focused group edit/reply cases, iPad edit/cancel/save cases and corrected calendar retry selectors pass locally. Hosted full CI is running on the integrated branch; no store submission. See `native-composer-isolation-2026-10-25-progress.md` for exact evidence and superseded selector failures.
