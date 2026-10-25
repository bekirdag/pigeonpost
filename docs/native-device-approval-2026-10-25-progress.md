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
- The actual server resolver additionally returns `state`, `creates_agent`, and a nullable address
  when approval would create an agent. Models and tests now cover this wire contract: 54 checks
  pass. Unknown/decided states cannot become another approval; a new-agent request is explicit.
- Added resolve/approve/deny transport with code-only POST bodies, validated request identifiers,
  exact-target approval acknowledgments and generic malformed-response errors. Denial must be
  followed by another resolve: the current server can acknowledge denial after a concurrent
  approval has won, so that acknowledgment alone does not establish the final state.
- `APIError` now carries bounded RFC 9470 authentication requirements from the server body/header.
  `PostboxClient` returns strong-authentication challenges before its ordinary 401 refresh/replay.
  Malformed challenges stop replay without inventing a requirement; ordinary expired tokens still
  renew once. The actual transport suite passes 47 checks, including request paths/bodies, malformed
  previews and target mismatches. No requesting machine receives a human token.
- Full shared Swift runner, iOS Debug and universal Mac builds pass with these foundation changes.
  Private evidence: `validation/native-device-approval-shared.log` and the corresponding iOS/Mac
  build logs. Session generation guards, strong sign-in and native approval UI remain to implement;
  no approval entry point is enabled by this foundation. No realm mutation, real pairing or store
  submission yet.
- Parent #96 merged at `bf908404`, #98 at `1bace09c` after all checks passed. #100/#102/#103 carry
  the qualified file/picker corrections and are in fresh hosted qualification. The web call bridge
  rollout is being prepared separately; no claim of deployment is made by this checkpoint.
