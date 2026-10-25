# Calendar time precision correction

`calendar/time.rs::fmt_local` always formats to minutes although authoring accepts seconds. The
same helper serializes API responses, persisted event/override times and import drafts. A time such
as09:00:17 can therefore change after saving and loading, including its recurrence key.

1. Confirm the shared helper's callers and existing socket-test setup through scoped symbols/graph.
2. Add a real API/persistence regression: create second-precision events, read/list, edit without
   changing timing, move an occurrence, and import/export. Keep occurrence identity and duration.
3. Preserve nonzero seconds while retaining the existing minute form for zero-second values.
   Do not guess lost historical input or rewrite stored occurrence identities.
4. Run the focused regression against the old implementation first, then the correction, full
   postbox tests, Clippy and required CI. Audit production counts only for potential legacy damage;
   never print event content, identities, grant links or credentials.
5. Publish a small independent PR. After qualification, use a pinned image, server-local SQLite
   backup/quick_check and isolated candidate/rollback checks before the core rollout.

The Apple calendar UI checkpoint stays in draft PR96 while this correction qualifies. No native
store submission and no change to the owner's Claude review hold.
