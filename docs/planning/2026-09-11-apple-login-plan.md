# Apple login for the iOS and macOS apps

## Objective and authorization
Add Apple alongside existing Keycloak SSO, produce new builds for both apps, and deliver the iOS build to the existing TestFlight group. The user authorized implementation, credential use, deployment, builds and TestFlight publication on 2026-09-11. Private keys must remain outside source control and app bundles.

## Implementation order
1. Inspect current app authentication, IAM deployment and signing pipelines. Verify the Apple key and identifiers without logging secrets.
2. Add a version-compatible Apple provider in IAM with signature validation, server-side client-secret generation, and the existing verified first-login/account-linking flow. Preserve existing realm settings and clients. Back up production configuration before applying changes.
3. Extend shared app authentication to select Apple through the existing PKCE browser flow. Add accessible Apple sign-in buttons on both platforms while preserving normal sign-in and account switching.
4. Validate provider configuration and callback behavior, app authentication URL construction, iOS/macOS builds and relevant regression tests. Exercise the live provider and document any account-consent step requiring the user.
5. Commit and push tested changes, deploy IAM, produce a local macOS artifact, and run the iOS distribution workflow with the next build number. Verify processing and TestFlight group assignment.
6. Record versions, artifacts, health and repository sync evidence. Keep rollback instructions and a separate progress record.

## Acceptance
- Both apps expose Sign in with Apple and continue using Keycloak-issued credentials.
- Apple key material stays on trusted server storage; expiring client secrets are regenerated automatically.
- Existing providers and mailbox ownership remain intact; linking requires proof of the existing account.
- macOS build is available locally; iOS build is in the testers' group with IN_BETA_TESTING state.
- Changed source and production repos are clean and synced, preserving unrelated local work.
