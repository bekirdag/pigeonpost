# Native calendar management — plan

Continue after PR96 (calendar foundation) and PR98 (scheduled joins). Keep their qualified
checkpoints isolated while adding the remaining Apple calendar controls. No native store submission;
the combined batch and pre-App-Review Claude review still apply.

1. Confirm real server contracts for attendee scope, revision conflicts, invitation placement,
   linked groups, feed capabilities, ICS exports and import retry behavior. Reuse the current
   calendar transport and generation-scoped store. Never infer authorization from UI roles alone.
2. Add organizer attendee add/remove controls with frozen revisions and explicit occurrence scope.
   A stale conflict must retain the user's draft until explicit reload. Do not auto-add contacts or
   group members. Let attendees place their own invitations without changing the organizer's event.
3. Add linked-group selection using active membership, with explicit linking/unlinking and server
   rechecks. Existing group creation remains the only path to creating/inviting a meeting chat.
4. Add ICS import preview/selection/confirmation with bounded local file reads and visible warnings.
   Verify confirm retry semantics before permitting ambiguous retries. Imports remain private copies
   and cannot send invitations or fetch remote resources.
5. Add native file export and subscription-feed controls. Show feed links only on creation, keep
   them in memory, clear them on identity loss/dismissal and explain projection, expiry, rotation and
   revocation. Never log capabilities or fetch preview URLs automatically.
6. Validate store isolation and real API flows, then native interactions on iPhone/iPad and both
   app builds. Add only tests that protect meaningful behavior. Preserve existing calendar/call UI
   checks, then qualify CI before the next checkpoint. Complete guest/member routing separately.

Source truth: `docs/calendar.md`, the postbox calendar modules and current shared Calendar API.
Use scoped Docdex AST/symbols/impact/DAG; Swift edges can be empty, requiring source-list review.
