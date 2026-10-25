# Native device approval progress

Plan: [implementation plan](native-device-approval-2026-10-25-plan.md).

- Created an isolated branch from calendar-link PR103 `2c9596c` in the owned native-round2 worktree.
- Read the existing server/native pairing contracts, web challenge handling, native Session and
  PostboxClient. Session refresh currently has no generation guard and ordinary transport retries
  every 401, including a strong-authentication challenge. These need explicit qualification before
  native approval is enabled. Swift impact graphs have no edges; exact symbols and caller searches
  supplement the graph.
- Added leaf code/link/preview models and registered 50 passing pure checks in the shared runner.
  Codes reject Unicode lookalikes before normalization; links require the exact HTTPS inbox origin,
  pairing path and fragment without query/authority extras. Preview bounds, terminal confirmation,
  expiry and permission denial are checked before approval can be offered. The local draft was
  rejected because it represented invalid codes as an empty success and used an uninitialized
  instance method. Primary implementation passes the actual fixtures.
- Transport, session generation guards, strong sign-in and native approval UI remain to implement.
  No realm mutation, real pairing or store submission yet. Calendar PR100 picker corrections are
  qualified independently and are being carried through the parent stack.
