# Pigeonpost for Android

Native Kotlin and Jetpack Compose client for Pigeonpost, published by Wodo Teknoloji A.Ş. The iOS app under `apps/ios` is the page/workflow reference. This is an initial development build, with production API and browser sign-in wiring.

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
- GitHub Actions: [Android workflow](../../.github/workflows/android.yml) builds these artifacts and runs JVM and emulator tests. Artifacts are development builds, not a public release.

## Implemented workflows

Browser sign-in with PKCE and provider selection; first-inbox creation; mailbox switching; inbox/search/unread/held states; conversation subjects; message history, send and acknowledgement; native Markdown, find, copy and original text; delete/report spam; document/photo attachments and Android open/share/save; contacts with namespace precedence and explicit permission confirmation; archive; quota; own-handle status; account sign-out; QR sign-in scanning; phone and wide-screen layouts; system light/dark appearance.

`core` owns wire models, conversation/subject assembly, the API client and coroutine state. `app` owns Compose, Android lifecycle, AppAuth, Keystore, file pickers/FileProvider and QR capture. HTTP, response decoding and Markdown parsing run off the UI thread. History always requests both sent and read messages. Drafts are isolated by mailbox, peer and subject and live for the view model's lifetime; they are cleared by sign-out and are not restored after process death. A failed or uncertain send is never automatically repeated.

The app uses the existing public client `pigeonpost-mobile`, issuer `https://auth.pigeonpost.dev/realms/pigeonpost-prod`, and callback `dev.pigeonpost.inbox://oauth2redirect`. It has no client secret. AppAuth request state/PKCE are persisted with the session in app-private, no-backup storage encrypted by an Android Keystore AES-GCM key. Do not install debug and release builds together when testing authentication: both deliberately use the existing registered native callback scheme.

Message bodies are presentation data. Server `autonomy`, `verb` and `held_because` decide the displayed state. Opening a subject marks its incoming messages read; it does not authorize agent execution. Contact permission options come from the server vocabulary and exclude `never_auto`. Adding a known sender grants no new automatic permissions.

## Repeatable UI checks

Ordinary debug launches use the real service. The explicit debug-only fixture path supports deterministic instrumentation and screenshots:

```sh
adb shell am start -S -n dev.pigeonpost.inbox.debug/dev.pigeonpost.inbox.MainActivity \
  --es pigeonpost.fixtures inbox
```

Modes: `inbox`, `empty`, `offline`, `signin`, `long` (1,000 historical messages). Fixture classes live only in `src/debug`; the release source set contains a no-op hook. Fixture tests do not prove a real authenticated send, purchase or notification was delivered.

## Release signing and service work

`dev.pigeonpost.inbox` is registered as Pigeonpost in Wodo's company Google Play account, with Play App Signing enabled. Keep Google's distribution signing key separate from the dedicated upload key. The iOS/Windows registrations are separate from this Android package.

The release build accepts these environment variables together: `ANDROID_UPLOAD_KEYSTORE` (absolute file path), `ANDROID_UPLOAD_STORE_PASSWORD`, `ANDROID_UPLOAD_KEY_ALIAS`, and `ANDROID_UPLOAD_KEY_PASSWORD`. Keep the upload key and passwords in an appropriate secret store. Providing an incomplete set fails the build; providing none produces unsigned release artifacts. The dedicated upload key is stored outside the repository and its password is in macOS Keychain. No signing key is committed, and CI does not publish to Google Play.

Remaining release gates:

- Finish a real account sign-in, refresh/logout, two-inbox messaging and attachment round trip on Android. The live authorization entry point is checked separately from fixture tests.
- Test a physical device's camera QR flow, file/photo providers, TalkBack, background/foreground lifecycle and low-memory recovery.
- Configure Firebase/FCM and implement Android token registration and postbox delivery. The current server sends APNs only; background Android notifications are unavailable. Foreground inbox updates work through bounded long polling.
- Configure Google Play products and server-verified Google purchase handling before enabling Android handle purchases. The Apple transaction route must not receive Google purchases. Existing owned-namespace information can be displayed.
- Complete Data safety/store content, broader testing and production-device acceptance before a public download or production store release. Company registration and signing setup are complete; the first release uses internal testing.

References: [Kotlin-first Android](https://developer.android.com/kotlin/first), [AGP compatibility](https://developer.android.com/build/releases/agp-8-13-0-release-notes), [AppAuth](https://github.com/openid/AppAuth-Android), [Android app signing](https://developer.android.com/studio/publish/app-signing).
