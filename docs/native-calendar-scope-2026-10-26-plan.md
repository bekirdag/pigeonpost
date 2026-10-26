# Native calendar authorization boundary

## Confirmed gaps

CalendarStore resets on mailbox/capability changes but does not capture the signed-in person, authorization or acting-inbox generation. Its long-lived client follows the current session. A delayed 401 may renew a replacement grant, and cached event details remain available before the lifecycle callback resets them. Creation and import helpers must also retain their original client through every suspension.

## Change order

1. Add an explicit calendar context containing person, authorization, acting-inbox generation and canonical mailbox. Wire both real Apple app roots through an Account-backed initializer; preserve the isolated fixture transport.
2. Gate cached data and the editor scope immediately against the live context. Configure a scoped client per activation. Bind refresh, detail reads, mutation/read helpers, cleanup ownership and live polling to the captured activation, including cancellation.
3. Pass the captured client into each operation instead of consulting mutable store state after an await. Prevent late creation/rejection cleanup from modifying another activation's retry state. Preserve exact request bytes and existing idempotency keys within their original context.
4. Treat denied access as invalidating cached reads and outstanding read results. A late successful response must not resurrect data after a refusal has been observed. Keep retry intent separate from server-read data.
5. Extend lifecycle identities for same-inbox reauthorization and A-to-B-to-A. Test delayed token/renewal/response, stale editor scope, concurrent mutation cleanup, access loss, cancellation and the existing lost-response flows.
6. Run the complete native shared suite, calendar integration, both Apple builds and the affected phone/tablet calendar, room and approval screens. Keep a separate progress ledger and qualify hosted checks before the native batch.

The existing server import receipt is already sealed and idempotent. Generic server create receipts still need a compatibility-aware at-rest correction before store publication, as recorded in the import progress document. Native draft recovery across process termination and explicit handling of edited uncertain creations are separate retry work; do not claim those complete through an authorization-only change. No store submission is authorized past the owner's Claude review gate.
