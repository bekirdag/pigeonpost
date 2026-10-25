# Native authorization lifetime progress

Plan: [scope plan](native-session-scope-2026-10-25-plan.md).

- Started from device foundation `fcd629c` in the owned native-round2 worktree, on a separate branch.
- Prior Session symbol/graph inspection found no generation guard around token adoption or the
  final local forget after asynchronous realm logout. Swift impact graphs have no dependency edges;
  the actual class, Keychain calls, constructor uses and token-provider integration are the review
  boundaries. A local review draft was generic; the actual Session regression harness is required.
- Reproduced the actual failure before the fix: an authorization-code exchange completed after
  `forget()` and restored the retired account. Private baseline log reports
  `FAIL: late authorization must not revive sign-out`. The test executes the real Session with a
  synthetic URLProtocol, browser callback and in-memory Keychain; no real realm request is possible.
- Added authorization identity/tickets and refresh task identities. Old successes/failures cannot
  adopt credentials into a retired/replaced authorization or clear a newer refresh. Realm logout
  retires local state before its request suspends. Browser and native Apple adoption share the
  acceptance guard. An ordinary refresh preserves the authorization identity and cannot change a
  known subject.
- Twenty actual Session checks pass: retirement during exchange, concurrent refresh sharing,
  competing browser successes/failures, cancellation, delayed logout, old refresh refusal and
  continued new-session refresh, a subject mismatch and native-controller replacement/callback
  ownership. All twenty checks, the complete shared runner, iOS Debug and universal Mac builds
  pass together. Defaults still use the normal shared URLSession and system browser. Native
  controllers are cancelled on retirement and callbacks are accepted only from the current one.
- Evidence: private `validation/native-session-lifetime-{before,after,expanded}.log`,
  `native-session-scope-shared.log` and corresponding app build logs. The first baseline intentionally
  fails; later complete qualification passes. No real Apple sign-in, realm MFA, native approval UI,
  signed release or store submission is claimed.
- Caller audit confirms Account clears its local selection before awaiting Session logout. Broader
  account/mailbox/cache lifetime binding remains a hardening review item before native release;
  the new authorization identity is available to bind the upcoming approval flow explicitly.

## Integration qualification

- The parent calendar management branch passes hosted iPhone CI: 78 cases, three existing skips,
  zero failures. Hosted iPad retains two system Files interaction failures; its other calendar and
  layout checks pass. One failure is an accessibility query inside a custom expectation being
  interrupted by the waiter. The candidate uses direct element-state expectations and fresh file
  actions; both file cases pass on each owned iPhone/iPad simulator with the integrated app.
- Added an optional manual file-only diagnostic mode to the existing iOS workflow. It defaults off;
  every pull request still runs the complete existing suites. YAML and shell syntax validate.
- Consolidate the remaining calendar/device-foundation/authentication stack into one main-targeted
  integration PR and require its complete iPhone/iPad/Mac qualification. Parent draft runs can be
  retired as superseded; their earlier failure evidence remains available. Do not merge until the
  actual combined tree passes. All native store and signed-profile gates remain held.
