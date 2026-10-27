# Scheduled lobby deployment — 27 October 2026

PR134 merged as `7e1d27a0508169440c9ca8d48d1773956d70f37d` after all 16 checks passed on `b28cb2fb15fa8007326fd7fbceaadbc5ee06d021`. The Windows native harness passed 18 calendar, 46 notification/protection, 12 recovery and 11 message-mode interactions, plus 34 call assertions and one informational screen-share record. The actual C# calendar/host selected the occurrence, admitted an earlier waiting attendee, exchanged synthetic audio/video with a browser peer, matched security codes and ended the meeting for everyone. Lobby, connected and completed-calendar screenshots were reviewed.

Deployed these five compatible source/assets on Raijin:

- `calls-service/src/scheduled.js`: require the trusted current `can_admit` grant with a supported member role.
- `site-inbox/calls.js`: load the initial waiting requests once, preserving newer SSE changes.
- `site-inbox/index.html`, `m.html`, `native-call.html`: load `calls.js?v=20261027-lobby2`.

The restricted server-side source backup is `/var/backups/pigeonpost/scheduled-lobby-20261027T072324Z`. No database was copied. Deployment checked old and new file hashes, original modes/owners, JavaScript syntax, zero active rooms and no queued postbox delivery before restarting `pigeonpost-calls.service`. Its configuration file stayed byte-for-byte unchanged. Rollback is source-only using that backup; do not restore a database over accepted writes.

The initial archive guard rejected directory entries before any server change. Rebuilt the archive with exactly five regular files; the guarded deployment then completed. Public `calls.js` returned 200 with the expected hash. Desktop web, mobile web and native-call HTML returned 200 with `no-cache` and the new call-script reference. Public and local call health were healthy with zero rooms/queued delivery after deployment. Automated calls and account fixtures remained local and synthetic.

Windows packages remain unsigned qualification artifacts for the coordinated native batch. Physical device and production notification QA remain. No store submission occurred; Apple still requires the owner's Claude review before App Review.
