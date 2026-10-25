# Native calendar notification routing progress

- Inspected the actual APNs/FCM payloads and Apple delegate. Calendar reminders currently route to organizer `peer`. A separate production-base branch adds typed event/occurrence locators while preserving old fields and preview privacy.
- Apple `CalendarStore.openEvent` is the currently displayed event used by polling; it must not be reused for pending navigation.
- Account capabilities can finish loading after the inbox view appears. Routing must wait for `features.loaded` and owned mailbox resolution, then configure calendar state synchronously after selection.
- Scoped Docdex symbols/impact inspected. Swift graphs have no edges, so both target builds and the explicit Mac harness source list remain qualification gates.

## Implementation

- Added a Foundation-only notice parser with bounded event/occurrence IDs and an exact recipient mailbox. Malformed calendar notices stay out of message routing; legacy notices open Calendar without guessing an event.
- Shared Apple routing waits for capabilities and verifies account ownership before selecting an inbox. A tap cannot change inbox during an active/preparing call, replace another sheet or discard a calendar editor belonging to another inbox; it asks for a deliberate retry instead.
- Pending event navigation is separate from active polling. Same-inbox notices queue until an open editor/detail dismisses; closing Calendar or changing identity erases the pending target. Navigation uses the ordinary event API; no automatic RSVP, join or contact creation.
- Foreground calendar pushes use calendar banners only for an owned mailbox and never become message announcements or clear the message badge.

## Validation

- Complete shared model/transport/store runner passes, including 40 new untrusted-payload/ownership checks; calendar model40, draft36, import11, API20, store46, inbox128 and groups84 remain green.
- iPhone and iPad: all four notice navigation cases pass (authorized event, another owned inbox, legacy fallback, unowned inbox refusal). The strengthened owned-inbox test then passes on both with a response title unique to the actual recipient mailbox.
- iOS Debug and universal arm64/x86_64 Mac builds pass. Standalone Mac inbox harness compiles; local runtime is unavailable on the locked desktop and is not claimed as passing.
- Evidence: private `native-calendar-routing-{iphone,ipad}.xcresult`, `native-calendar-routing-identity-{iphone,ipad}.xcresult`, model logs, app build logs and Mac harness compile log. Full hosted CI and modal/call interruption interaction coverage remain pending.
- Final navigation-lifetime clearing builds on both apps. No store submission. Guest/meeting URL entry remains the next routing step.
