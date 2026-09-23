# Pigeonpost iOS public release

Review remediation audited 21 September 2026 against App Store Connect and production.

## Submission and purchases

- App: **Pigeonpost Inbox**, App Store Connect ID `6803521541`.
- Bundle: `dev.pigeonpost.inbox`; team: `AH277897AV`. Preserve both identifiers.
- **TestFlight 1.0 (43)** was uploaded on 23 September 2026 from branch `codex/ios-iap-catalog-20260923` (PR #27, [run 35871523695](https://github.com/bekirdag/pigeonpost/actions/runs/35871523695)) and added to Pigeonpost Internal.
- Build 42 was **rejected on 23 September 2026 under 2.1(b)**. Review ran on an iPad Air (M3) and reported that Handle 2 to Handle 10 "could not be found in the submitted binary". The app received its product IDs only from the postbox at run time, and the purchase screen showed only the next unused product. Build 43 compiles all ten IDs into `HandleCatalog` (`Shared/Store/HandlePurchases.swift`). Its purchase screen also has an **All handle subscriptions** section that lists every product, and any unused product can be chosen and bought there.
- The same submission, `6b5a1281-f265-46dc-ac15-0cb74da89b5a`, was resubmitted with build 43 on **23 September 2026 at 14:35 UTC** and is **Waiting for Review** with all 21 items. **Resubmitting after a rejection:** attach the new build to the version and update the notes. PATCH the REJECTED `reviewSubmissionItem` with `resolved: true`, then PATCH the submission with `submitted: true`. Until the item is resolved, the submit call returns `409 Version is not ready to be submitted yet`, however long you wait.
- The rejected build-41 submission `bd53e886-70a2-47e4-af63-565e965b2927` was removed after its 21 items were backed up, allowing its locked product screenshots to be replaced. No subscriptions were deleted.
- All ten annual handle subscriptions and their groups were submitted together with build 42. The first product is `dev.pigeonpost.inbox.handle.yearly`; the others are `dev.pigeonpost.inbox.handle2.yearly` through `dev.pigeonpost.inbox.handle10.yearly`. Each purchases a separate name, at USD 8/year in the US, USD 80/year for ten. Local storefront prices apply.
- The purchase screen is directly in **Settings → Get a handle**, also accessible through **Settings → Handles → Get a handle**. It displays Apple's product name and local price, annual term, automatic renewal and cancellation information, plus Restore purchases, Manage subscriptions, privacy and terms. An owned, unassigned purchase uses **Finish registration** without another charge. If the catalog is unavailable, the app offers a retry and does not invent a purchase price.
- The basic cryptographic mailbox remains available without buying a readable handle.

All ten products have the correct annual term, USD 8/year US price and availability in 175 territories. The live reviewer account can sign in, load account handles and retrieve all ten product IDs. The individual product review notes now use the direct route and explain how successive independent subscriptions are offered. All ten review screenshots were replaced and verified in COMPLETE state with matching checksums. Public and beta review notes were updated while preserving their demo credentials. The physical recording shows successful first and second independent Apple sandbox purchases; a separate authenticated API read verified both registrations belong to the demo account.

Native simulator compilation and Swift model/controller checks passed. All 26 hosted iOS UI regressions passed in [run 35570377895](https://github.com/bekirdag/pigeonpost/actions/runs/35570377895), and all 15 PR checks passed before PR #24 merged. The real StoreKit adapter test used Apple's local configuration to exercise ten concurrent subscriptions, restoration and expiry; it is separate from the physical sandbox recording.

The optional scanner is directly in Settings. Default inbox labels distinguish namespaces, and new conversations supply the leading slash.

## Reviewer access

Maintain working credentials in App Store Connect's sign-in fields; never copy credentials into this document. Normal sign-in requires neither an OTP nor a QR code. The review account has sample conversations and separately owns the complimentary, non-expiring `/ppappreview/main` mailbox, available from the mailbox picker.

For the optional scanner, open [the review sign-in page](https://postbox.pigeonpost.dev/review-sign-in) on another screen and generate a fresh QR. In the app choose **Settings → Scan for login**, scan, sign in and approve. Tap Done on the success page. Codes last ten minutes; generating a fresh code avoids expired attachments. The demonstration discards its device credential and never retrieves an account token. A live generated QR was decoded and successfully approved using the reviewer account.

Sign-in uses `ASWebAuthenticationSession` with PKCE. Sign in with Apple is offered alongside the other social providers.

## Screenshots and presentation

The public version has two uploaded, checksum-verified screenshots in **COMPLETE** state:

| Screenshot | Source | Size |
| --- | --- | --- |
| Inbox | `Screenshots/6.9-inch/store-1-list.png` | 1320 × 2868 |
| Conversation and held request | `Screenshots/6.9-inch/store-2-thread.png` | 1320 × 2868 |

They show the running native app with fictional fixture messages. Never substitute personal mailbox content. The app targets iPhone (`TARGETED_DEVICE_FAMILY = 1`); a future iPad release requires layout validation and its own screenshots. See [Apple's screenshot instructions](https://developer.apple.com/help/app-store-connect/manage-app-information/upload-app-previews-and-screenshots/).

The app icon is supplied through the asset catalog. The listing uses Productivity as its primary category and Developer Tools as secondary; the audience override is 18+. Recheck the questionnaire whenever content or audience changes.

## Review requirements checked

The audit used [Apple's current App Review Guidelines](https://developer.apple.com/app-store/review/guidelines/), especially 1.2, 2.1, 2.3, 3.1.2, 4.8 and 5.1.1. The following describe implemented behavior, rather than a guarantee of approval.

- **Reporting and blocking:** press and hold an incoming message for Report spam (`Shared/Views/MessageBubble.swift`). Sender details offer admission Block (`ContactSheet` in `Shared/Views/SettingsSheet.swift`). Abuse can also be reported through support.
- **Access and metadata:** the reviewer has working access, sample content and the fresh QR flow. Purchase instructions name the actual screen. The public description explains which features require subscriptions.
- **Privacy:** `PrivacyInfo.xcprivacy` declares no tracking and the UserDefaults required reason `CA92.1`. The App Store privacy declaration covers thirteen collected data types, linked to identity for the declared service purposes, with no tracking. Keep declarations aligned with the implementation and service.
- **Deletion:** Settings → Account → Delete account opens the authenticated [deletion page](https://pigeonpost.dev/account#delete-account). It shows the account, requires DELETE confirmation, records a durable request and supplies a reference and 30-day deadline.
- **Fulfillment:** deletion is manual, following `deploy/account-deletions.md`, including identity-provider and applicable Apple-token revocation, owned-data removal and completion notification. The operator timer monitors pending requests. It does not perform automatic erasure. Cancel App Store subscriptions separately. This follows [Apple's account-deletion guidance](https://developer.apple.com/support/offering-account-deletion-in-your-app/).
- **Support and policies:** the listing's [support page](https://pigeonpost.dev/app-support.html), [privacy policy](https://pigeonpost.dev/app-privacy.html) and [terms](https://pigeonpost.dev/app-terms.html) are live. Settings and the purchase screen link privacy and terms; the terms include Apple's standard EULA.

## Release evidence and remaining verification

Wodo's Paid Apps Agreement, banking, tax forms and EU trader status were all active in App Store Connect on 21 September. Separately, Apple's company record still lists an incorrect Alabama region in Wodo's Türkiye address; correcting that record is outside this purchase-flow remediation.

The attached `Pigeonpost-1.0-42-iPhone-Sandbox-Purchases.mp4` starts at the physical iPhone Home Screen, launches the TestFlight app, demonstrates demo-account messaging, and shows successful Apple purchases at 1:44 and 2:40 followed by the registered handles and their inboxes. Attachment `ca746064-4a0f-4f85-b8ef-f6c4680e79c4` is COMPLETE with its checksum verified. The 3:41 review copy retains the continuous beginning of the original video and ends after the owned-handle list, before the external App Store sign-in screen. The original is preserved privately.

The recording shows account-handle refresh, not StoreKit Restore purchases or completed subscription management. Reviewer notes state that distinction and explain that the demo account now owns two test names, leaving eight unused Apple slots. Physical restoration remains an additional verification item; automated restoration and expiry tests passed. Sandbox subscriptions have accelerated expiry, so existing test names may show renewal controls by review time.

Physical-device APNs delivery was not reverified in this audit. No APNs errors appeared in the current postbox container's logs, which alone does not prove delivery. Validate production notification receipt and opening the correct conversation on a real iPhone before claiming that check passed.

For another release, recheck the live review state, reviewer credentials, product catalog, restore and interrupted-purchase recovery, reporting/blocking, deletion request path, privacy declarations and policy URLs. Capture fictional screenshots from the matching native UI, run `apps/ios/Tests/run.sh` and relevant purchase tests, build with the existing signed workflow, and record the exact submitted build and product states. Keep credentials, signing material and production data outside Git.

## Build and credential custody

Use `.github/workflows/ios-testflight.yml` on a macOS runner with the required current SDK. Its App Store Connect authentication uses `APPLE_API_KEY_ID`, `APPLE_API_ISSUER_ID` and `APPLE_API_PRIVATE_KEY`, alongside the existing signing identity and profile secrets referenced by the workflow. Keep all values out of source and command output. Every upload needs an unused build number. Public submission is a separate App Store Connect action after the build finishes processing.

Keep the native Apple-framework architecture and render received messages as content. The iOS app must not execute an agent's message body. For APNs configuration, key custody and production environment settings, use `deploy/postbox/README.md`; preserve the existing signing identities and registered device data.
