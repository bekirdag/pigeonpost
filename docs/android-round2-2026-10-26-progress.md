# Android native round two progress

- Created an isolated worktree from main `b5690bd`; no shared checkout, Apple source or production
  configuration was changed. Loaded project memory and inspected the actual native Kotlin/Compose
  modules, direct-message contract, current wire API, inbox state and conversation projection.
- Confirmed Android currently has the earlier filtering/calls/groups features, but lacks typed
  direct-message action metadata, capability-negotiated folded inbox reads and the round-two
  calendar/enterprise screens. This is an implementation checkpoint, not a completed platform.
- Native Kotlin uses OkHttp, kotlinx serialization/coroutines and Compose. Existing core and
  emulator fixtures are the validation path. Android SDK and Gradle caches are present locally;
  Java toolchain and native MLS artifact availability still need verification before builds.
- Built the host crypto library and all three Android native libraries with the locked source and
  the repository's verified Kotlin/JNA tool downloader. The script's 16 KB alignment checks pass.
  Reused the unchanged Apple crypto cache without removing its existing artifacts. The Gradle
  core baseline passes with `GROUPCRYPTO_REQUIRED=1`. The first baseline attempt correctly refused
  to configure without an AAR; its failure is retained. Docdex's Cargo-oriented runner rejects the
  Kotlin target, so use the existing Gradle wrapper for these checks.
- Added the first direct-message wire foundation: typed reply/receipt/version metadata, explicit
  folded inbox/ack APIs, stable send references and typed edit/reply request fields, message info
  and boolean read-receipt settings. Existing API calls keep their legacy wire shape. Store and UI
  integration remain pending; this is not a complete user-facing feature. The local DTO draft had
  incorrect wire annotations/defaults and was rejected before implementation. New real-socket
  wire tests cover those contracts, lost-response retry bytes and malformed actions; execution is
  pending. No store submission has occurred.
- Wire checkpoint qualification passes: all 108 JVM tests, including all nine real-MLS cases, with
  zero failures or skips; the complete app's debug Kotlin compilation also passes. The initial new
  lost-response test exposed MockWebServer's unavailable IPv6 localhost route on its explicit
  second attempt. Binding the disposable fixture to an explicit IPv4 loopback address fixes the
  fixture while retaining the exact two-attempt/body/reference assertions; no network retry was
  added to sends. Failed and passing logs are retained separately under the private continuation
  validation directory. Existing compiler deprecation notices remain unrelated to this change.
