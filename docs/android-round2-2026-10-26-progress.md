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
- Committed/pushed wire foundation `38f256c`, then merged current main (including qualified Apple
  PR #111 and the deployed receipt fix) locally as `9497ef9` before the next state checkpoint.
- Added direct-message composer state and negotiated reads/acks, including the Filtered view.
  Reply/edit modes are scoped to the draft; edits preserve request verbs/other arguments and restore
  ordinary drafts/files. Sends capture their inputs before coroutine scheduling. Uncertain keyed
  sends retain body/reference/upload IDs, block accidental replacement and support an explicit
  retry. General draft keys remain stable when the server assigns a default-thread ID. Pending
  edits project onto their root and confirmed references suppress duplicate pending bubbles.
- Optimistic read marks now include the edited revision. Message-info results are guarded by the
  mailbox and dialog generation and cleared on conversation/subject navigation. Two additional
  tests reproduced a send stuck after the existing handle-registration account reload, and stale
  info returning after navigation. Account metadata reload now has its own generation while reset
  retires the authorization; the two reproductions fail before the fix and pass afterward.
- State qualification passes all 119 JVM tests (including eleven new state cases and all nine real
  MLS cases, no skips) and complete debug Kotlin compilation. The retained evidence is in
  `android-message-actions-state-*.log` and `android-message-actions-reload-*.log` under the private
  continuation validation directory. This is in-process recovery; encrypted recovery after app
  restart remains a separate hardening item. Composer controls, action rows/ticks, message-info
  presentation, emulator coverage and real postbox integration are the next implementation steps.
