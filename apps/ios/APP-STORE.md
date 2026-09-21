# Pigeonpost iOS public release

Review remediation audited 21 September 2026 against App Store Connect and production.

## Submission and purchases

- App: **Pigeonpost Inbox**, App Store Connect ID `6803521541`.
- Bundle: `dev.pigeonpost.inbox`; team: `AH277897AV`. Preserve both identifiers.
- Public version **1.0, build 41**, submission `bd53e886-70a2-47e4-af63-565e965b2927`: **Rejected / Unresolved Issues** under guideline 2.1(b). Apple could not locate the handle subscriptions and requires a successful sandbox purchase recording captured on a physical device before resubmission.
- All ten annual handle subscriptions and their groups are included in that submission. The first product is `dev.pigeonpost.inbox.handle.yearly`; the others are `dev.pigeonpost.inbox.handle2.yearly` through `dev.pigeonpost.inbox.handle10.yearly`. Each purchases a separate name, at USD 8/year in the US, USD 80/year for ten. Local storefront prices apply.
- The revised purchase screen is directly in **Settings → Get a handle**, also accessible through **Settings → Handles → Get a handle**. It displays Apple's product name and local price, annual term, automatic renewal and cancellation information, plus Restore purchases, Manage subscriptions, privacy and terms. An owned, unassigned purchase uses **Finish registration** without another charge. If the catalog is unavailable, the app offers a retry and does not invent a purchase price.
- The basic cryptographic mailbox remains available without buying a readable handle.

All ten products have the correct annual term, USD 8/year US price and availability in 175 territories. The live reviewer account can sign in, load account handles and retrieve all ten product IDs. The older individual product review notes referenced the obsolete “Your handles” settings route; update them with the direct route alongside the new binary. These checks establish catalog and account health, but do not prove a sandbox purchase succeeds on a physical device.

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

## Remaining verification and the next release

Wodo's Paid Apps Agreement, banking, tax forms and EU trader status were all active in App Store Connect on 21 September. Separately, Apple's company record still lists an incorrect Alabama region in Wodo's Türkiye address; correcting that record is outside this purchase-flow remediation.

Before resubmitting, attach a recording captured on a physical iPhone or iPad that starts at the Home Screen, launches the new TestFlight build, uses the supplied review account, demonstrates the core messaging features and completes a sandbox handle purchase. Show access to additional purchases and restoration as well. Simulator screenshots and StoreKit configuration tests are supporting evidence only. Preserve the app version, ten subscription versions and ten subscription-group versions in the public review submission.

Physical-device APNs delivery was not reverified in this audit. No APNs errors appeared in the current postbox container's logs, which alone does not prove delivery. Validate production notification receipt and opening the correct conversation on a real iPhone before claiming that check passed.

For another release, recheck the live review state, reviewer credentials, product catalog, restore and interrupted-purchase recovery, reporting/blocking, deletion request path, privacy declarations and policy URLs. Capture fictional screenshots from the matching native UI, run `apps/ios/Tests/run.sh` and relevant purchase tests, build with the existing signed workflow, and record the exact submitted build and product states. Keep credentials, signing material and production data outside Git.

## Build and credential custody

Use `.github/workflows/ios-testflight.yml` on a macOS runner with the required current SDK. Its App Store Connect authentication uses `APPLE_API_KEY_ID`, `APPLE_API_ISSUER_ID` and `APPLE_API_PRIVATE_KEY`, alongside the existing signing identity and profile secrets referenced by the workflow. Keep all values out of source and command output. Every upload needs an unused build number. Public submission is a separate App Store Connect action after the build finishes processing.

Keep the native Apple-framework architecture and render received messages as content. The iOS app must not execute an agent's message body. For APNs configuration, key custody and production environment settings, use `deploy/postbox/README.md`; preserve the existing signing identities and registered device data.
