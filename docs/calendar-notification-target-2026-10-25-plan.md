# Calendar notification destinations

Native calendar routing needs an authoritative event locator. Current reminders contain only an opaque occurrence in `message_id` and name the organizer as `peer`, so a tap is incorrectly treated as a conversation. An occurrence must not be reverse-engineered into an event ID.

1. Add optional typed calendar destinations to the existing notification model. Set them only on calendar invitations/reminders; preserve message IDs, legacy fields and ordinary/group notifications.
2. Carry `type=calendar`, `calendar_event_id`, optional `calendar_occurrence_id`, and the existing recipient mailbox through APNs and FCM. Identifiers grant no access; native clients must use their selected mailbox's authorization.
3. Keep calendar pushes separate from message badge/thread counts. Preserve generic FCM bodies and preview privacy.
4. Qualify notification payloads, calendar invitation/reminder effects, all postbox regressions, formatting, Clippy, OpenAPI and CI before a controlled server deployment. No live test pushes or production invitations.
5. Integrate Apple notification routing separately into the native calendar stack; old calendar notices should open the calendar rather than the organizer's thread. No automatic RSVP, join, mailbox enrollment or contact creation.

No new dependencies, schema, credentials, paid infrastructure or store submission.
