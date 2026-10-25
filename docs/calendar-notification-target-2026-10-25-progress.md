# Calendar notification destinations progress

- Read the actual APNs/FCM payloads, notification constructors and calendar worker. Calendar reminders currently carry organizer `peer`, opaque occurrence `message_id`, and `unread=0`; the APNs payload applies that zero as an inbox badge.
- Isolated branch starts from production `58459ad`. Scoped symbols and impact inspected before edits; implementation and qualification pending.
- This worktree was previously reserved for cache sealing, with no code or plan written there. Cache sealing remains an independent hardening item; this change addresses native calendar navigation first.

## Implementation and qualification

- Optional `CalendarTarget` added to APNs/FCM notification construction. Invitations carry the event; reminders also carry the opaque occurrence. Existing message IDs/peer/mailbox fields stay compatible. Calendar APNs notices omit message badges and use a separate thread; FCM keeps generic text and string-only data.
- All 457 postbox unit/integration cases and both avatar worker cases pass, including reminder eligibility/idempotence and the new APNs/FCM payload/privacy cases. No live provider credentials or pushes used.
- Local test draft invented a `sender` field and unrelated payload keys; rejected and replaced with checks against the actual provider wire contract.
- Impact connects notification delivery to main, groups, FCM and calendar; constructors checked explicitly. Formatting applied. Clippy initially caught the larger worker plan variant; boxing its notification keeps the plan small without suppressing the lint. Final 457+2 tests, Clippy with warnings denied, and pinned OpenAPI validation (168 paths) pass. Pre-commit passes; hosted qualification and rollout evidence follow below.

## Production rollout

- All 13 hosted checks passed at `e5cea045c470bcc8e992b088c5d83c8c3b9fb2ad`; PR101 merged as `6950c29a82ae1bf7e52265db73615e3a5cecde79`. Candidate and merge trees match.
- Built `pigeonpost-postbox:calendar-notice-e5cea04`, image `sha256:560d31e6db61146c1648498f7dd04de15cd9717787a91a442d7a4dd1e319d822`, on the core host with the existing two-CPU/four-GiB build ceiling.
- Server-only SQLite backup/quick_check and network-disabled candidate/old-image startup checks pass. Root-only rollback backup: `/var/backups/pigeonpost/postbox-20261025T123527Z` on 45.132.181.100; no production database copied to the Mac.
- Postbox and reaper now run that exact image with zero restarts. Source `6950c29` is clean; environment hashes match the backup. Public health 200 and anonymous capabilities 401 pass. No configuration/feature gate or store submission changed.
- Private pinned build/rollout scripts and prepare/deploy/verification JSON remain in the continuation release directory. This is deployed server support; native releases still wait for the qualified batch.
