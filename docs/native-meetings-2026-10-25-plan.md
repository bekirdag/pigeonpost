# Native scheduled meetings — plan

Continue the Apple calendar checkpoint from PR96 without changing its qualified screen behavior.
Native stores stay held for the combined batch and the owner's Claude review before App Review.

1. Reuse the existing calls client for member joins from calendar occurrence details. The call
   service and postbox remain authoritative for membership, lobby, host roles and join windows.
   Do not call internal meeting-access routes or turn the public meeting code into authorization.
2. Validate meeting locators, preserve opaque occurrence identities, capture the acting mailbox,
   and close calendar sheets before presenting the call overlay. Offer voice and video joins.
3. Make call-page configuration and renewal reject stale account/mailbox/page generations.
   Clear queued operations on identity loss; do not let a late token configure an old mailbox.
4. Add a native bridge method with bounded, sanitized results and explicit errors for old pages.
   Keep direct calls and closed-app ring answering compatible. Test sign-out/switch/reload races,
   waiting/admitted/error results and exact occurrence forwarding without real credentials.
5. Add fixture UI coverage on iPhone/iPad and app builds on both Apple platforms. Qualify the
   JavaScript bridge and existing call alerting suite, then the real scheduled service checks.
6. Review deep links and guest capabilities separately before claiming complete routing. Guest
   fragments must never enter analytics, logs or arbitrary network fetches. Remaining attendee,
   linked-group, import/export/feed screens and non-Apple parity stay on the main ledger.

Use scoped Docdex symbols/impact/DAG. Swift graphs currently have no edges: verify source lists,
root overlays, sheet dismissal and CallKit callers directly. No new infrastructure dependency.
