# Native account and encrypted group scope progress

- Isolated child branch from the organization-room candidate `c5ebe56` (#109); the room candidate and its test inputs are preserved.
- Read Account, Session, PostboxClient, GroupStore, GroupEngine, GroupVault and both root lifecycles. Session already rejects cross-person refresh results and has an authorization UUID; use these existing guarantees.
- Confirmed GroupStore caches engines by mailbox, handles events by mailbox only and does not bind persisted group state to a revocable authorization. Account loads have no authorization guard. GroupStore's refresh task is not retired or ownership-checked during reset.
- Requested Docdex symbols and impact analysis. The new worktree's index initially reports an index-writer backoff; retry after indexing settles. Swift graph results in the identical parent tree were empty, so use direct callers and compiler/tests in addition.
- The room candidate passes both seven-case screen suites and both builds. Its full shared suite passes; the Mac integration run awaits an unlocked desktop. PR #109's hosted iPadOS 26.5 run found a collapsed sidebar in the room-entry test; its screenshot and accessibility tree were inspected and the test now uses the real “Show conversations” control. Fast-forwarded this child branch to that fix, `e3832c5`; hosted requalification is running.

## Account and transport

- Added captured token providers and response-scope checks for ordinary and file requests. Kept the live `Account.client` for long-lived services such as CalendarStore; `scopedClient()` creates an operation snapshot. The scoped proxy owns no session or account.
- Identity reads/first-inbox creation reject late results. Another person cannot see cached identities or capabilities before lifecycle callbacks run. Same-person strong authorization retains its screen and capability hints, while a new root task reloads identities for the new grant.
- A canceled identity load retires only its own loading state. Final validation passes all 21 account-scope and 58 session checks, including cancellation followed by a retry under the same authorization.

## Group state and disk boundary

- New authorization retires the old synchronous access lease; selecting another inbox retires its child lease, including A-to-B-to-A. Each group activation captures the subject, authorization, inbox generation, canonical address and its own lease. Cached plaintext, metadata and callbacks check this context immediately.
- Group clients and engine events remain tied to the activation that created them. Refresh coalescing has an ownership key, reset cancels the old refresh, and engines are recreated from sealed state rather than reused across grants. Missing membership is now unknown instead of active.
- The vault checks its lease around the whole synchronous key/seal/write operation. Parent-to-child locks never span an await. Local retirement waits for an already executing save, then erasure happens before remote revocation. Late logout responses cannot erase a replacement login's state.
- Final iOS and universal Mac builds pass after correcting a missing return in the newly guarded member getter. All 14 access-lease checks, 20 controlled real-GroupStore race checks and 90 group model/vault checks pass. The complete final shared runner passes, including existing room, approval, calendar, inbox and purchase-controller checks.
- Real MLS integration passes 74 checks against a freshly rebuilt isolated postbox and the actual web peer. A paused scoped HTTP read and queued re-enrolment are refused after retirement; getters reveal no old history or device metadata, no additional request or disk mutation occurs, a fresh lease preserves the existing identity and retired startup cannot recreate erased storage. Only scratch state and synthetic actors are used.
- All 29 affected screen cases pass on each iOS 18.3 device: 14 groups, eight device approvals and seven organization rooms (318.9 seconds phone, 361.4 seconds tablet). Strong authentication still returns to review and requires another deliberate confirmation. The sidebar-navigation refinement inherited from #109 also passes separately on both devices (30.5 seconds phone, 34.2 seconds tablet); no application source changed after these successful runs.
- The combined Mac inbox harness compiled successfully, then correctly refused to run while the desktop remains locked. Its assertions remain pending. The owner question is still open; other work continues.

## Remaining

Run the Mac inbox harness when the desktop is unlocked. Commit/push and qualify hosted checks after the diff and pre-commit review. Calendar scope/durable retries and the other native/SDS phases remain subsequent work; no store submission or production feature enablement occurred here.
