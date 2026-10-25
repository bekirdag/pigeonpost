# Group edit lifetime and version deletion

Continue SDS §14C and §17 before releasing native group controls. The existing group edit transaction uses the last version's created_at and the current group timer, and deletion clears only a single row. Direct messages already keep root deadlines. Preserve the existing encrypted body, parent-seq wire format, authorization, ciphertext-only postbox and immutable request-key semantics.

1. Add actual-MLS, real-socket regression cases for timer disable/shorten/change, chained edits at the original24-hour boundary, missing/expired/deleted roots and deleting any version. Run them before the fix and retain failures.
2. Resolve the preceding-version chain inside the same existing writer transaction. Check its original send time and earliest existing expiry; require a complete live chain and the current unreplaced own target. Never grant a new lifetime or window to an edit. No plaintext is opened by the server.
3. Delete the whole version component after validating the current member's authority against the root. Release every version's attachments, preserve tombstones/seqs, and wake all members with version seqs. Maintain legacy event fields for older clients.
4. Test concurrent edit/delete, other members, removed members, unrelated messages and other groups, then run full postbox tests, clippy/format and hosted platform CI. Document the exact parent-seq chain semantics and on-device history limits.
5. Integrate native version deletion/expiry wake-ups and UI only after the server contract is qualified. Deploy with a private server backup and the existing live-DB-preserving rollback rule.

Progress: [evidence ledger](group-edit-hardening-2026-10-25-progress.md). No crypto primitive, store submission or feature-flag change belongs to this patch.
