# Calendar notification destinations progress

- Read the actual APNs/FCM payloads, notification constructors and calendar worker. Calendar reminders currently carry organizer `peer`, opaque occurrence `message_id`, and `unread=0`; the APNs payload applies that zero as an inbox badge.
- Isolated branch starts from production `58459ad`. Scoped symbols and impact inspected before edits; implementation and qualification pending.
- This worktree was previously reserved for cache sealing, with no code or plan written there. Cache sealing remains an independent hardening item; this change addresses native calendar navigation first.

## Implementation and qualification

- Optional `CalendarTarget` added to APNs/FCM notification construction. Invitations carry the event; reminders also carry the opaque occurrence. Existing message IDs/peer/mailbox fields stay compatible. Calendar APNs notices omit message badges and use a separate thread; FCM keeps generic text and string-only data.
- All 457 postbox unit/integration cases and both avatar worker cases pass, including reminder eligibility/idempotence and the new APNs/FCM payload/privacy cases. No live provider credentials or pushes used.
- Local test draft invented a `sender` field and unrelated payload keys; rejected and replaced with checks against the actual provider wire contract.
- Impact connects notification delivery to main, groups, FCM and calendar; constructors checked explicitly. Formatting applied. Clippy initially caught the larger worker plan variant; boxing its notification keeps the plan small without suppressing the lint. Final 457+2 tests, Clippy with warnings denied, and pinned OpenAPI validation (168 paths) pass. Pre-commit and CI pending; no deployment yet.
