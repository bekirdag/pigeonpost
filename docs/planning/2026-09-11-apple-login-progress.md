# Apple login progress

## Inspection
- Both apps use Shared/Auth/Session.swift with ASWebAuthenticationSession and S256 PKCE to the pigeonpost-prod realm; native client pigeonpost-mobile.
- App bundle dev.pigeonpost.inbox, team AH277897AV. User completed the Services ID dev.pigeonpost.signin and supplied the Apple key outside the repository.
- Existing iOS workflow includes processing and beta-group assignment. Latest successful upload: 1.0 (29), workflow run 33196277546.
- Local Xcode 16.2 supports local validation; store uploads use the existing macOS 26 CI runner.
- Pigeonpost working tree began clean. su_iam has unrelated untracked .claude/ configuration, which must be preserved.
- Docdex symbols locate shared authorization; the Swift impact graph returned no edges, so direct call-site searches and both platform builds will supplement it.

## Implementation and deployment
- Both native sign-in views offer Apple. The shared authorization URL uses `kc_idp_hint=apple` and `prompt=login`, preserving state validation, public client identity, S256 PKCE and the existing Keychain/refresh flow.
- IAM PR https://github.com/sealunit/su_iam/pull/1 merged to main b28c36b. Both full IAM CI runs passed (34574969112 and 34574976419).
- Production /srv/sealunit on wodomini is clean at the same main commit. A verified incremental bundle replaced unavailable deploy-user GitHub SSH access. Previous production-only changes are preserved on preserve/pre-apple-login-20260911; prior in-repo backups were moved to protected backup storage. Local .claude configuration is preserved via .git/info/exclude.
- Database and Compose backup: /srv/backups/pigeonpost-apple-20260911. The running Keycloak 24.0.5 image a7dc64ff515c includes Apple adapter 1.12.0, preserves the public realm issuer and JVM limits, and caps logs at 10m x3.
- The Apple provisioner only changes the apple identity provider and retains verified existing-account linking. Adapter-generated client-secret JWTs renew automatically. The private key remains in protected server storage and is redacted from realm exports.

## Validation and delivery
- Adapter Gradle tests and its Linux Docker image build pass.
- Authentication URL tests, thread model tests, iOS Simulator Release and universal macOS Release builds pass. Docdex did not detect the shell test runner, so apps/ios/Tests/run.sh was run directly. The iOS sign-in screen was inspected in the simulator.
- The Mac build launches and is signed with Developer ID for team AH277897AV, hardened runtime, no debugger entitlement, and arm64/x86_64 architectures. Local download: ~/Downloads/Pigeonpost-Desktop-1.0-30.zip. Notarization is being completed through the existing App Store Connect credentials in CI.
- iOS 1.0 (30) uploaded successfully (34574975587). Independent status run 34575749950 confirms VALID, IN_BETA_TESTING, and group Pigeonpost Internal.
- Live mobile and web authorization requests reach Apple's service with the expected Services ID and HTTPS form-post callback. Cancellation returns to Keycloak; missing state is refused; all three providers remain visible. A deliberately invalid-code token request was rejected as invalid_grant, rather than invalid_client.
- Actual Apple-account consent and completed sign-in still require an available browser/user session. Browser runtime discovery returned no connections; the user was asked to connect one while deployment continued. No user credentials or consent were fabricated.
- Pigeonpost CI exposed two pre-existing issues: fixed App Store Connect vocabulary exceptions, and replaced an expired August 2026 attribution test key with the current canonical month. All 26 offline-delivery tests pass after the fixture correction; production Rust behavior is unchanged.

## Remaining verification
Complete notarization, final Pigeonpost CI/merge and source synchronization; record any real Apple-account login when the browser becomes available.
