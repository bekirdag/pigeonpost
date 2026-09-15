# Pigeonpost for Android

Native Kotlin and Jetpack Compose client for Pigeonpost, published by Wodo Teknoloji A.Ş. The iOS app under `apps/ios` is the page/workflow reference. The client uses the production API, browser sign-in and Google Play handle subscriptions.

## Build

Use JDK 17 and an Android SDK with `platforms;android-36` and `build-tools;35.0.0`. Open this directory in Android Studio, or set `ANDROID_HOME` / an ignored `local.properties` containing `sdk.dir=/absolute/path/to/sdk`.

```sh
./gradlew :core:test :app:lintDebug :app:assembleDebug :app:assembleRelease :app:bundleRelease
./gradlew :app:connectedDebugAndroidTest
```

The second command needs a connected device or emulator. The wrapper pins Gradle 8.13 and verifies its distribution SHA-256. AGP 8.13.2, Kotlin 2.2.21 and Compose BOM 2025.12.01 are pinned as a compatible API 36 toolchain; dependency upgrade notices are reviewed separately from build failures.

- Installable development APK: `app/build/outputs/apk/debug/app-debug.apk` (`dev.pigeonpost.inbox.debug`, Android debug signing).
- Unsigned release APK: `app/build/outputs/apk/release/app-release-unsigned.apk`.
- Release bundle: `app/build/outputs/bundle/release/app-release.aab` (unsigned unless an upload key is supplied).
- Minimum Android version: Android 8.0 / API 26. Compile and target: API 36.
- GitHub Actions: [Android workflow](../../.github/workflows/android.yml) builds these artifacts and runs JVM and emulator tests. CI validates and produces artifacts; Play releases require the dedicated upload key and a separate console submission.

## Implemented workflows

Browser sign-in with PKCE and provider selection; first-inbox creation; mailbox switching; inbox/search/unread/held states; conversation subjects; message history, send and acknowledgement; native Markdown, find, copy and original text; delete/report messages; document/photo attachments and Android open/share/save; contacts with namespace precedence and explicit permission confirmation; archive; quota; free handle registration for approved testers; up to ten annual Google Play handle subscriptions with restoration; versioned terms acceptance; policy/support links; account deletion requests; account sign-out; QR sign-in scanning directly in Settings; background notifications through Firebase Cloud Messaging; phone and wide-screen layouts; system light/dark appearance; adaptive and themed Pigeonpost launcher icons.

Settings follows the iOS handle flow: enter a name, check availability, confirm registration, then open the new `/name/main` inbox. Approved testers retain one complimentary handle, authorized by the server's private allowlist. The separate paid section uses Google Play Billing 9.1.0 and server verification for up to ten annual subscriptions. The app displays Google's localized prices and renewal terms before checkout. See the [billing setup and validation guide](BILLING.md) and [complimentary handle configuration](../../deploy/postbox/README.md#complimentary-preview-handles). Play internal-test membership, license testing, and complimentary-handle eligibility are separate settings.

`core` owns wire models, conversation/subject assembly, the API client and coroutine state. `app` owns Compose, Android lifecycle, AppAuth, Keystore, file pickers/FileProvider and QR capture. HTTP, response decoding and Markdown parsing run off the UI thread. History always requests both sent and read messages. Drafts are isolated by mailbox, peer and subject and live for the view model's lifetime; they are cleared by sign-out and are not restored after process death. A failed or uncertain send is never automatically repeated.

Conversation rendering starts with the latest ten messages in a native reverse-layout lazy list. Scrolling toward older history adds ten at a time; search can reveal matches outside that window. The REST response remains a complete snapshot. Stable message IDs preserve a reader's position when messages arrive, and saved window boundaries and a viewport anchor preserve it through screen recreation. The latest message stays bottom-aligned even when taller than the screen or resized by delayed content and the keyboard.

The inbox front page has no duplicate address strip. Copy controls remain in the mailbox picker and Settings. The picker keeps the account's main readable root first, followed by other purchased roots, named children and raw key addresses. Get a handle contains new-registration and paid-but-unassigned recovery controls; existing names remain under Handles, with Google Play renewals, restore and management in their own subpage.

The app uses the existing public client `pigeonpost-mobile`, issuer `https://auth.pigeonpost.dev/realms/pigeonpost-prod`, and callback `dev.pigeonpost.inbox://oauth2redirect`. It has no client secret. AppAuth request state/PKCE are persisted with the session in app-private, no-backup storage encrypted by an Android Keystore AES-GCM key. Do not install debug and release builds together when testing authentication: both deliberately use the existing registered native callback scheme.

Message bodies are presentation data. Server `autonomy`, `verb` and `held_because` decide the displayed state. Opening a subject marks its incoming messages read; it does not authorize agent execution. Contact permission options come from the server vocabulary and exclude `never_auto`. Adding a known sender grants no new automatic permissions.

## Repeatable UI checks

Ordinary debug launches use the real service. The explicit debug-only fixture path supports deterministic instrumentation and screenshots:

```sh
adb shell am start -S -n dev.pigeonpost.inbox.debug/dev.pigeonpost.inbox.MainActivity \
  --es pigeonpost.fixtures inbox
```

Modes: `policy` (terms acceptance), `inbox`, `empty`, `offline`, `signin`, `long` (1,000 historical messages), `long-tall` (also an oversized latest Markdown message), `handles` (approved tester registration). Fixture classes live only in `src/debug`; the release source set contains a no-op hook. Fixture tests do not prove a real authenticated send, registration or notification was delivered.

## Release signing and service work

`dev.pigeonpost.inbox` is registered as Pigeonpost in Wodo's company Google Play account, with Play App Signing enabled. Keep Google's distribution signing key separate from the dedicated upload key. The iOS/Windows registrations are separate from this Android package.

The release build accepts these environment variables together: `ANDROID_UPLOAD_KEYSTORE` (absolute file path), `ANDROID_UPLOAD_STORE_PASSWORD`, `ANDROID_UPLOAD_KEY_ALIAS`, and `ANDROID_UPLOAD_KEY_PASSWORD`. Keep the upload key and passwords in an appropriate secret store. A signed release additionally requires `PIGEONPOST_FIREBASE_API_KEY`. Store the restricted Firebase client key outside Git and supply it through the local build environment or GitHub Actions secret of that name. Only non-secret Firebase project identifiers are tracked. Providing incomplete signing settings or omitting the Firebase key fails a signed build; unsigned development artifacts can use fixtures without a key. The dedicated upload key is stored outside the repository and its password is in macOS Keychain. No signing key is committed, and CI does not publish to Google Play.

## Release validation and limits

Android 0.2.6 (9) brings the iOS build 41 inbox, mailbox-ordering, handle-acquisition and history fixes to Android. Release submission and signature evidence are recorded separately after the signed bundle is verified. On 15 September, Play showed the preceding 0.2.5 (8) publicly available at 100%, all 177 selectable territories targeted (including France), and one active base plan for each of the ten handle products.

Android 0.2.5 (8) adds recovery for interrupted handle requests, namespace labels for default inboxes, an automatic address slash, root Settings scanning and background notifications. The signed release was submitted for public Google Play review on 14 September 2026 with updated notification disclosures, store text and reviewer instructions. The console confirms all three publishing changes are in review, with no unsent changes; managed publishing is disabled, so approval releases the update at 100 percent in all selected countries. The app also requires a terms acceptance screen before messaging. The current terms revision is saved with the encrypted session; a fresh sign-in, a changed terms revision or sign-out requires acceptance again. Policy, support and account deletion links remain available before acceptance. Received messages can be reported from their actions menu, and senders can be blocked from Conversation info.

The reliability changes passed JVM tests, release lint/build/signature verification, all 18 existing emulator workflow tests, two notification privacy/account tests and an opt-in live FCM delivery test. After rotating the Firebase client key, the signed bundle was rebuilt and live FCM delivery passed again. Its SHA-256 is `e962b1ac503b0968f49868c4da85f56c744af48ee6662697acc9c0bcba9e6b2e`, built from `fa6f8888bbaddd20a0e4823a6e063f5c66ba873d`; [Android CI](https://github.com/bekirdag/pigeonpost/actions/runs/34844292101) passed at that commit. The consent screen was also checked at normal and 200% font scale. The preceding Play-installed 0.2.0 (3) passed real no-charge license-test purchases, server receipt verification, restoration and cancellation; see [billing validation](BILLING.md#validation). Fixture tests do not replace live service or device acceptance.

The old Firebase client key was revoked and verified rejected by Google; GitHub secret-scanning alert #1 is resolved. The replacement permits only Firebase Installations and FCM Registration, with Android restrictions for the production package's three current Play signing certificates and the separate debug package/certificate. All three production signing identities passed provider verification, and an unrelated package was rejected. Recheck these restrictions when Play signing certificates change. Build-time injection keeps the literal out of source; the key remains extractable from the app, so API/application restrictions and server authentication are still required.

Android notifications require device permission and notify the selected inbox. Foreground conversations continue to use bounded long polling. Push registration uses WorkManager and is revoked on sign-out; display is also gated locally by the selected identity. Google receives generic alerts and routing identifiers, without message text or attachments. Firebase Analytics is disabled. Physical-device QR camera, TalkBack, low-memory recovery, a French billing account and the remaining accelerated billing lifecycle cases still need wider device acceptance. Recheck Play policy declarations, reviewer credentials, public deletion/privacy URLs and product availability for every public release.

References: [Kotlin-first Android](https://developer.android.com/kotlin/first), [AGP compatibility](https://developer.android.com/build/releases/agp-8-13-0-release-notes), [AppAuth](https://github.com/openid/AppAuth-Android), [Android app signing](https://developer.android.com/studio/publish/app-signing).
