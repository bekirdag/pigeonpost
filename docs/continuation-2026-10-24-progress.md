# Big-features continuation progress — 2026-10-24

Plan: [Continuation plan](continuation-2026-10-24-plan.md).

## Current state

Inherited #83, #89 and #87 and shared security/operations/web/group fixes are merged. The web
inbox, enterprise portal and approval page run the controlled coherent asset rollout (base source
`1132ad76`, tree `1fe9961b…`). The six changed call scripts/HTML entry points additionally run
merged #98 source `1bace09c`; origin and public verification pass. Postbox/reaper run production source `6950c29`, image
`pigeonpost-postbox:calendar-notice-e5cea04`; public health and anonymous authorization checks pass.

Apple direct/group edit/reply/status/privacy work is merged in #95. Calendar foundation #96 and
scheduled native calls #98 are merged after all hosted checks passed. Calendar management #100,
notice routing #102 and meeting/scanner links #103 are stacked and undergoing full CI. A CI-discovered edit/ordinary-draft input leak is fixed and
passes local iPhone/iPad keyboard regressions; all three branches include it. A later hosted
version-history assertion exposed a test replacing text from the beginning without selecting it;
native Select All passes locally and is carried through the stack for hosted confirmation.
Independent #101 is merged and deployed with calendar push destinations and separate calendar
notice/message badge handling. Native notice routing has
40 parser/ownership checks, four passing navigation cases on each device, strengthened actual
recipient tests and passing iOS/universal Mac builds; hosted qualification follows.

Calendar link distribution/profile qualification, enterprise native screens, remaining native
platforms and later SDS phases remain open. Device foundation #104 has 54 model and 47 actual
transport/authentication checks, a passing shared runner and both builds; it enables no approval
entry point. The reproduced Session lifetime race is fixed locally with 20 actual Session checks,
the full shared runner and both builds passing. The remaining Apple stack is being consolidated
for complete integration CI before fresh native authorization UI is enabled.
All native releases remain batched. The owner's Claude review
before Apple App Review and mainland-China removal before a CallKit release remain mandatory.
Postmark, payment/seller setup, DNS, exposed-secret rotation and owner real-device checks remain
external prerequisites; none is represented as completed here. Historical checkpoints below
record their state at the time, rather than the current release status.

### Apple integration qualification — 25 October

- Calendar management, notice/meeting routing, pairing foundation and Session lifetime fixes are
  consolidated in #105. At `e445231`, the complete hosted phone suite, StoreKit and both Mac
  jobs pass; the iPad failed only an AX activation-point query for already-visible response text.
- A separate fixture-only diagnostic branch preserves the Files picker evidence. Its iPad import
  and cancellation pass. The phone trace reaches picker presentation but no import callback;
  its screenshot remains on Recents because the cold service appeared after the three-second
  navigation wait. Export is visibly open while a Save hittability predicate times out. Neither
  trace shows a parent lifecycle change, so no picker lifecycle guard has been weakened.
- The test now waits for Files navigation, checks exporter presence independently of Save
  activation, and verifies invitation response text without querying its activation point. The
  actual selection/preview, cancellation/return and moved-invitation assertions remain required.
  All three targeted tests pass on each local device. Hosted qualification of the update follows.
- Native machine confirmation is isolated in `codex/native-device-ui-20261025`, with fresh
  sign-in proof and store/UI tests still being qualified. No native app has been submitted.
- A later hosted iPad import reached the fixture file but did not open it. The grid cell's
  activation point falls in its filename area; the test now touches the thumbnail on both
  devices and rechecks an explicit Open action after selection. Selection/preview and export
  cancellation pass locally on each device. Native machine confirmation is now draft #106:
  eight UI cases pass per device, Session/proof/store checks and both builds pass, and the
  serial Mac inbox stress passes 197 checks. Genuine OTP and signed-device evidence stay open.
- Linux privacy CI found a listener-bind collision, then a new server address matching an
  earlier client address. Independent #107 keeps fixture listener ports outside the kernel's
  automatic client range; its complete local privacy gate, concurrent runs and deliberate
  leak rejection pass. Integration candidates include that test-only fix for hosted validation.

### Call bridge rollout — 25 October

- Deployed exactly `calls.js`, `native-call.js`, and `index.html`, `join.html`, `m.html`,
  `native-call.html` from merged #98. The source-difference inventory excludes only tests/package
  metadata; all runtime changes are covered by the candidate manifest.
- Backed up only those files on Raijin at `/var/backups/pigeonpost/web-calls-20261025T154649Z`,
  rehearsed restoration on synthetic paths, compared the live baseline and published scripts
  before their HTML entry points. Origin file hashes and content types pass.
- Public scripts match exact release hashes. Public HTML differs only by the existing Cloudflare
  analytics script insertion; after removing that one known line, every document matches the
  candidate hash. Main/inbox health, association JSON and postbox health/anonymous authorization
  probes pass. Nginx, association bytes and legacy group-encryption assets are unchanged.
- Private evidence: `validation/web-calls-{prepare,deploy,public-verification-final}.json`,
  `web-calls-stage.json` and the scoped rollout scripts in the continuation release directory.
  No new core image, feature flag, real call or native store submission in this rollout.

## Intake evidence

- Read the local October 21 handoff and phase ledger from the shared checkout; loaded `codex` profile, repo memory, wake-up context and the applicable planning/progress and Pigeonpost instructions.
- Shared checkout is clean on an older release branch; all valid inherited worktrees are clean. One unrelated/prunable scratch worktree has a missing gitdir and was left untouched.
- Fresh worktree: `pigeonpost-big-features-20261024`, branch `codex/big-features-continuation-20261024`, based on current `origin/main` `d1f36e2`.
- PR #83 remains open at `01d87fd`; web and dependency-audit checks failed. PR #89 remains open at `cf4abfd`; web, formatting, audit and black-box checks failed. PR #87 remains open at `7b3e752`; iPhone/iPad, web, formatting, audit and black-box checks failed. These are completed failures, not still-running jobs.
- Shared-checkout Docdex index is dated October 15, so it is not fresh implementation evidence. Read current planning file slices directly; the continuation worktree will get its own scoped index for code inspection.
- Local disk has approximately 75 GiB available. Avoid duplicating native build caches and preserve unrelated simulator state.

## Work ledger

| Area | State | Next evidence |
|---|---|---|
| Shared baseline / CI | #90 compatible patches, suite guard and documented advisory assessment | Canonical path rebuild fixed; verify fresh reproducibility and Windows call checks |
| #83 enterprise/device login | Pairing fixes pushed at `cde5d01`; CLI custody tests pass | Complete CLI validation, integrate baseline, deploy approval page and portal |
| #89 edit/reply/receipts | Atomic delivery, inherited expiry and fail-closed privacy pushed at `ca2fa1e` | Integrate patched baseline and verify fresh checks |
| #87 Apple block/filter/groups | iPad newer-runtime audit passes; `33d0a74` bootstraps pinned Rust | Verify newer iPhone purchase tests and all native jobs |
| Native round two | Pending | Verified server contracts and platform test plans |
| Store batch | Held until parity and review | Current store state, final signed artifacts, Apple review decision |
| Phases 5–8 | Pending | Source/SDS gap analysis after open PRs |
| Hardening/review handoff | Pending | Inherited backlog and security/dependency evidence |

## Failures and recovery notes

- Initial worktree-status inventory encountered the stale scratch gitdir. Repeated the inventory while explicitly skipping prunable worktrees; all valid inherited worktrees are clean. No cleanup or reset was performed.
- Generic clone routines contain inferred approval gates. The owner has explicitly authorized this continuation; apply plan/progress/validation and controlled-release rules within that scope, preserving the specific Apple submission hold.

## Checkpoint — first fixes and live-state verification

- **#83:** the old test counted event-loop turns while native WebCrypto ran asynchronously, then parsed an unset navigation URL. The fixture now exposes a navigation promise and deliberately delays the digest, verifies the real S256 challenge and stored state/nonce, and waits for the step-up redirect. Production authentication code is unchanged. Commit `fb7f396` pushed. Targeted tests **8/8**, full inbox **179/179**, enterprise **26/26**, bridge **20/20** pass locally. Fresh CI `40110383557` passes inbox, formatting, OpenAPI and black-box checks; platform jobs were still completing at this checkpoint and the inherited audit gate remains red.
- **#89:** merged current main to incorporate #88's hosted-runner routing and OpenAPI repairs; no conflicts. Formatted the one ownership-test assertion reported by CI. Commit `c7648de` pushed. Formatting passes, message-action server tests **8/8**, inbox **183/183** pass. The previous web/black-box failures were missing Chrome/sqlite3 on the old runner pool, not failed privacy assertions. Keep the new CI result separate from that older run.
- **#87:** merged current main locally without conflicts. Downloaded the historical iPad result bundle and exported screenshots. Portrait Calls shows a native sidebar overlay over dimmed content; the old audit counts underlying controls as overlapping. This is visual evidence for a possible audit-scope issue, not proof that accessibility is correct. Added hierarchy/hit-test attachments and started a targeted local diagnostic before choosing a fix. Local Xcode 16.2/iOS 18.3 fixture build passes; CI uses the newer iOS runtime and remains necessary. Existing iPhone failures are the two purchase-navigation tests listed in the handoff.
- **Postbox:** live source `c0d6a4b`, image `pigeonpost-postbox:p3b-c0d6a4b`, both services running. Source ancestry includes Phase 4 server commit `001f21c`; org tables exist. Global features include calendar/groups/filtering/call flags, but orgs/enterprise_portal are not globally enabled. Step-up is configured as `mfa`; actual realm behavior still needs verification. No service/env/database mutation made.
- **Routes:** Raijin already has the enterprise vhost and directory, but the inbox approval page is absent. Public probes for enterprise and pair return 403 while postbox health is 200; determine origin versus edge behavior before declaring routing ready. Enterprise and TURN names resolve to Cloudflare addresses; that alone does not verify explicit records or DNS-only TURN.
- **Dependency hardening:** the continuation branch updates quinn-proto `0.11.19`, rustls `0.23.46`, rustls-webpki `0.103.16`, webpki-roots `1.0.11`, ctutils `0.4.5`, Tokio `1.54.1`, chacha20 `0.10.2`. These are compatible updates. A new crypto-bigint advisory (`RUSTSEC-2026-0387`, fixed in 0.7.6+) still reaches the older P-256/P-384/K-256 dependencies through OpenMLS; no ignore or unreviewed crypto-provider migration was added. Record this as an unresolved hardening gate, not a clean audit.
- **Regression failure preserved:** the patched full workspace compiled, but the oversized-request Loft integration test hit macOS `Connection reset by peer` while uploading a rejected body. The isolated reproduction passed. Investigate deterministic early-rejection coverage; do not label the full suite passed or remove the boundary assertion.

## Tooling and evidence

- The persistent MCP connection is bound to the shared checkout. Explicit per-worktree CLI/HTTP initialization and fresh MCP sessions were used instead; no umbrella directory mounted. AST/symbol/impact checks were scoped to each changed test. Empty JavaScript/Swift graph edges do not prove no UI coupling; direct HTML/project/shared-view references were also inspected.
- `docdexd run-tests` cannot map a nested Node test inside a Cargo workspace; it returned `unsupported Cargo test target`. The documented `docdexd test run-node` helper passed the targeted pairing suite; repository-native npm/Cargo commands ran the full/focused suites where appropriate.
- Small local-model delegation was attempted and retried for the pairing test; both drafts invented APIs or failed to implement the promised resolver, so neither was applied. Primary review implemented and validated the change.
- Local diagnostic artifacts are in the private release area under `continuation-20261024/`; fixture build log, historical iPad screenshots and targeted test results are retained. Never treat the iOS 18 diagnostic as qualification of the failing newer-runtime CI case.

## Checkpoint — validated dependency patches and authorization review

- Full patched root workspace regression (`cargo test --locked --workspace --all-targets --all-features`) **passes** after replacing the oversized upload in the Loft acceptance test with a real TCP header-only request. The test still requires HTTP 413 and no stored event, now also requiring a response within five seconds without sending any body. The initial macOS reset failure remains recorded above. Loft offline suite: **26/26**; formatting passes. Full logs are retained privately in `validation/workspace-patched.log` and `validation/loft-offline.log`.
- Audit of the patched lockfile reports **one** remaining vulnerability, crypto-bigint 0.5.5 (`RUSTSEC-2026-0387`), with no new ignore. Latest published OpenMLS RustCrypto provider is still 0.6.0 and does not offer a compatible patched dependency path. The application's fixed MLS suite is X25519/AES-GCM/Ed25519; the direct P-256 use verifies public calendar guest proofs. Continue reachability review before any release decision.
- #83 functional CI is complete and passes all platform, inbox, privacy and format checks; audit remains red. #89's completed functional checks pass (including Windows x64/ARM64 builds); its audit remains red. No branch-protection bypass.
- #87 current-main merge and accessibility diagnostics pushed at `56855ef`. Local iPad Calls audit passes on iOS 18.3, and both historical purchase-navigation failures pass on the matching local fixture binary. Newer-runtime CI is required and running; local passes do not close those CI defects.
- Device-approval review found two genuine UI authorization issues: malformed new links could retain another machine's pending code, and a failed denial was displayed as successful. A hold could also continue after clicking Decline. Fixes clear stale fragment state, report refusal failures, cancel the pending hold and serialize decisions. Focused tests **12/12** and full inbox **183/183** pass; committed and pushed as `cde5d01` on #83.
- Scoped Docdex pre-commit validation completed with exit 0 for the staged dependency checkpoint and Apple diagnostic change. A simple local-model test-refactor draft was invalid (invented function/signature); it was rejected. The final Loft change was authored and validated by the primary agent.

- Patched root workspace Clippy (`--all-targets --all-features -- -D warnings`) passes. Security update PR remains draft while the remaining advisory is reviewed. No new audit ignore has been added.

## Checkpoint — message-action review found and repaired delivery races

- Added a real-socket concurrent same-reference retry test to #89. It **failed before the fix**, returning two different delivered message ids. The previous sequential idempotency test had missed the race.
- Replaced separate recipient enqueue, best-effort sender enqueue and later metadata/expiry writes with one SQLite IMMEDIATE transaction. The transaction rechecks reference ownership, returns original ids on a concurrent retry, rejects a competing sender's reference claim and persists both sealed copies, metadata and expiry together. Reader visibility and metadata failures cannot expose an edit as ordinary unannotated mail.
- Added rollback fault injection, self-send idempotency, competing-sender and stale-edit-root/deadline cases. Also found an edit could survive its disappearing original after the thread timer was disabled; edits now retain the original's expiry, with a send/disable/edit/sweep regression. Focused action/store suites pass **21/21**. Full postbox suite **441/441** plus avatar worker **2/2**, formatting and full postbox Clippy pass; atomic-delivery/expiry fixes committed and pushed to #89 at `cc70925`.
- Receipt privacy lookup failures now fail closed for direct-message receipts; normal missing preferences still default on in the store. The blocked-sender lookup already failed closed.
- Apple CI `40113531468`: real StoreKit job passes; iPhone job stopped in the standalone loading model tests before UI tests. Cancelled URLSession transports can still be removed after the cancelled task resumes, contaminating the next case's pending-request count. Added bounded waits for transport drain; local Swift suites pass, including inbox **71 checks**, group logic **31**, Phase 1b **33**. This fix is not pushed yet, to let the in-progress iPad diagnostic preserve its artifacts first.
- Verified Raijin's actual enterprise origin serves the old **66-byte Coming soon placeholder**. `/app` and inbox `/pair.html` return 404 at origin. The bridge health route works. Portal/device-page deployment is still outstanding; public 403 responses alone had not established this state.

## Checkpoint — Windows PoW fixture and newer iPad evidence

- #89 atomic send/expiry fixes are pushed at `cc70925`; group privacy fail-closed follow-up at `ca2fa1e` passes the receipt tests and postbox Clippy. Final head CI is running. Both direct and group privacy settings now fail closed on storage lookup errors.
- A newer #83 run failed Windows only in the existing Loft PoW refusal fixture. Its random wrap with default nonce zero already satisfies the 8-bit floor **1/256** times. The fixture now selects a nonce verified below that floor before asserting HTTP 403; the production PoW algorithm and threshold are unchanged. A small local draft used the verified APIs correctly and was reviewed before application. Loft real-socket suite **26/26** passes.
- Downloaded and inspected #87's newer-runtime result bundle (`56855ef`). It has the same eight portrait Calls-sidebar overlaps, all behind the native floating/dimming sidebar. Hierarchy confirms the covered General button is **not hittable** while sidebar controls are hittable; there is no XCUI popover/sheet container. Added explicit native-column occlusion clipping to the layout audit, preserving checks inside the sidebar and for exposed parts of background controls, plus geometry coverage and a covered-control hit-test assertion. Local validation is running before the next push; newer-runtime CI remains required.
- Shared persistent browser verification confirms the public enterprise page shows Coming soon, matching the origin. Existing unrelated browser tabs and sessions are preserved. Main has no branch protection; no required-check bypass was used or needed. The remaining upstream crypto advisory is still a final release/hardening gate, not a clean audit.

## Checkpoint — assessed crypto exception and boundary hardening

- The remaining advisory was traced to P-curve integer operations. Current production group secrets use only suite 0x0001 (X25519/AES-GCM/Ed25519); state imports enforce Ed25519. The direct P-256 use verifies public calendar guest proofs; private P-256 signing is test-only. OpenMLS already rejects mismatched Welcome suites before HPKE decryption, but consumes a matching key package first.
- Added an explicit wrapper-level Welcome suite check before any provider/key-store call. A genuine-Welcome mutation regression failed before the fix and passes after it, including joining with the untouched Welcome afterward on the same device. This closes the input-driven key-package consumption issue and pins the fixed-suite invariant.
- Documented the reachability assessment and removal/reassessment conditions in `docs/security-advisory-2026-10-24.md`. Added RUSTSEC-2026-0387 as a non-applicable-path exception to both audit configs, explicitly **not** a patched dependency. No vendored crypto patch or provider migration. Both audits now return exit 0 with this documented exception; existing unmaintained-package warnings remain visible. External group-crypto review and native/cross-language qualification remain release prerequisites.
- Updated the separate group workspace's ctutils lock to 0.4.5. Before that lock update, standalone lifecycle/property tests and full Clippy passed with the new suite guard; repeat the group checks after the lock update and rebuild its committed web WASM on the reference Linux x86_64/Rust 1.91.1 host before pushing.
- Keycloak accepts both enterprise `/auth/callback` and inbox `/pair.html` redirects (302 to the correct callback with `login_required`, no authorization code). Python requests hit Cloudflare browser-integrity 1010; the existing shared browser/request session established actual routing. No sign-in token/code was printed and no security rule was weakened.

## Checkpoint — reference bundle, dependency checks and live step-up configuration

- Reference Linux x86_64 build on Raijin completed in an isolated Docker workspace, bounded to **2 CPUs / 4 GB** with no ports or production service changes. The first attempt installed wasm-bindgen into the isolated Cargo home but did not place it on the container's executable search path; retried with a link inside the new disposable container. The second build and `--check` are byte-for-byte reproducible. Retrieved only generated bundle files and verified all SHA256SUMS. WASM SHA-256: `64f4557965893248375ec6e6407a014f1923dfd2079cbd12e74f3276f26f58c4`.
- After the standalone lock patch, the complete group workspace tests pass, including lifecycle, malformed-input property cases and the new unsupported-suite/key-package-preservation regression. Root and group workspace Clippy pass with warnings denied. Full inbox tests using the rebuilt real WASM pass **171/171** on the shared-main baseline.
- The inbox install exposed a high-severity source-map-js advisory (GHSA-68fv-2mgg-jv7q). Updated only that compatible transitive package; npm reports **0 vulnerabilities**, and inbox tests still pass **171/171**. The NPM lock's graph has no edges; manifest and resolved package use were inspected directly. Its graph check occurred after the update, unlike the pre-edit Rust graph checks.
- Verified live Pigeonpost web step-up through read-only Keycloak configuration: `mfa -> level 2`, a dedicated client browser-flow binding, required OTP at level 2, and a 600-second level-2 maximum age. The public enterprise and pairing callbacks are accepted. Native mobile currently lacks this mapping/binding; add it alongside native step-up support in round two. No auth configuration was changed and no credentials were printed.
- #87 at `1f92a3b`: **iPad passes on the newer runtime**, real StoreKit passes, Mac tests pass. iPhone now reaches the UI suite (loading-model race fixed) and fails only the two existing purchase tests. Both inspect the payment button after scrolling to the name field; newer Form laziness can leave the button uninstantiated. Fixes scroll/wait for the actual button while retaining price/product assertions; local targeted tests are underway. A local Swift draft invented nonexistent controls and was rejected.
- Cloudflare DNS console session has expired to SSO sign-in. Asked the owner asynchronously to sign in; keep DNS-dependent writes pending. Coturn is active on the existing relay with port 3478 and external IP 193.164.5.36. Continue independent source/release work; a login timeout is not permission or authentication.


## Checkpoint — clean-build path diagnosis and credential preflight

- #90 CI caught a real WASM byte mismatch. The disposable reference build placed `CARGO_HOME` inside the checkout; rustc applies the **last** matching remap rule, so the broad repository rule overrode the dependency-source rule. Embedded strings confirmed `/pigeonpost/.wasm-cargo/registry/src` versus CI’s `/cargo/registry/src`. Reordered rules broad-to-specific. Two independent clean Linux x86_64 builds with nested and external Cargo-cache paths now match each other **and the previously failing CI rebuild**: SHA-256 `f185d0bd6f783430db1edd81aba0a90570a0b93c9bac486a81f9bc90c816de16`, 1,723,957 bytes. Retrieved generated files and verified checksums; fresh CI remains required. The earlier cached self-check did not qualify cross-layout reproducibility.
- #90’s Windows call failure is at the **headless browser peer launch**, before the Pigeonpost process is started (`drive.mjs:70`). Windows application-error events in the collected log are not sufficient to attribute this failure to app startup. Keep the original evidence; inspect native artifacts and require a fresh complete call-harness pass.
- #87’s next iPhone run stopped before UI tests because the hosted image lacked Cargo in the Xcode build environment. Added the repository’s existing pinned Rust-toolchain action to all three iOS jobs, parsed workflow YAML, ran staged validation, and pushed `33d0a74`. The scroll-to-payment-button fix at `3a0dfeb` passed both targeted tests locally; newer-runtime confirmation remains pending.
- #83 review found device keys used an unsafe predictable temporary filename and relied on inherited Windows permissions. The new flow now preflights a private home and both existing credential files before any request, uses the existing SDK’s bounded private-file custody and atomic writes for device keys and newly enrolled mailbox capabilities, and zeroizes key buffers. Existing shared/permissive homes are refused without changing their permissions. Focused tests cover corrupt/oversized files, linked leaves, unsafe homes and planted old temporary symlinks; the parser never echoes credential contents. Full CLI and Clippy validation are underway.
- Small local drafts for the remap reorder and negative test used incorrect ordering/invalid Rust and were rejected. No generated draft was applied. Docdex graphs for scripts/workflows have no edges; workflow callers and build-script paths were inspected directly.


## Checkpoint — merged baseline and deterministic commit-recovery test

- Shared baseline #90 passed every functional job, both audits with the documented advisory exceptions, all cross-language crypto/native checks and byte-for-byte WASM/binding reproduction. Tag-only release was correctly skipped. Merged as `26f10b8a5adb80fdd03c337d5c5ac10b50c475f5`; integrated it without conflicts into #83 (`7136f1e`), #89 (`d140fc6`) and #87 (`b7a7eff`) and pushed final qualification runs.
- #83 CLI custody changes pass the integrated full CLI suite; inbox tests after installing the patched lock pass. Enterprise invitation regression (new/invalid/empty fragment during an old acceptance) failed before the fix, then full portal tests passed **31/31** at `a52d4c5`. Full postbox on the #89 integrated baseline passes **441/441** plus avatar worker **2/2**.
- #89 Windows CI failed an unrelated existing registry test: `timed_out_final_commit_recovers_the_exact_atomic_challenge_result` never reached its post-SQLite barrier. Inspection showed its 25 ms per-blocking-operation budget could expire during preparation/admission. The test now uses the normal 30-second budget for real preliminary work, asserts the request and permit are still held at the committed-write barrier, then pauses/advances only that Tokio runtime's test clock to expire the final adapter deadline. Recovery assertions (one append, same log index, `appended=false`) are unchanged. Tokio `test-util` is enabled **only as a dev feature**; production code and limits are unchanged. Focused case passes in 0.01 seconds; full registry tests and all-target/all-feature Clippy pass. Windows requalification is required.
- #87's Cargo-location fix is built into the integrated run; iPhone now passes fixture installation and reaches native UI tests. Newer iPad and iPhone results remain pending at this checkpoint. Local custom-Cargo-home reproduction fails with the old build script and passes with the corrected one, while retaining SDK/compiler environment isolation for Rust.
- Native round two has an isolated branch `codex/native-round2-20261024`, checkpoint `dd90fc7`: shared Apple action DTO/API/projection and local read-state preservation, with **88 inbox/wire checks** and full standalone model suites passing. UI, calendar, enterprise client screens and the other platforms remain pending; no native release.


## Checkpoint — enterprise deployed and publicly verified

- #83 passed every check and merged as `8db5df3c5ae4c16e4d070235b3d490bcb155b45d`. Staged 49 whitelisted, tracked production assets from that exact tree. Production bridge source hashes matched its reviewed predecessor before mutation. Published portal assets, `pair.html/css/js`, website privacy/terms and the bridge sales module/route. Bridge health and validation of an empty sales request pass (400, no lead created).
- Private server-side backup: `/var/backups/pigeonpost/enterprise-20261024T195415Z` on raijin, mode 0700, outside web roots. The deployment script and validation log are retained in the private release directory. Origin checks compare the deployed HTML/JS bytes to the staged commit and verify CSP, checkout nonce policy and clean `/app/*`, `/invite`, `/auth/callback` routing.
- Public browser/request checks pass for enterprise `/`, `/app` and inbox `/pair.html`. The shared browser initially reused the old placeholder; normal reload retrieves the complete portal, fonts and scripts, with the closed-USD notice. Added HTML revalidation to the enterprise nginx block; static assets retain their five-minute override. An immediate-header probe after the first reload did not observe the new header and triggered rollback; the exact cause was not established. Retried against the exact web-app nginx master with bounded header polling; syntax validation and origin revalidation headers pass. No DNS, identity, global feature or payment configuration was changed.
- Portal visual evidence: `~/.local/share/codex-browser/artifacts/enterprise-live-20261024.png`; public scripts and Geist fonts load. Cloudflare DNS console remains at sign-in; the existing wildcard route works for the portal. TURN DNS verification is still pending.
- #89's integration with #83 required retaining both CLI modules (`agentd_edits`, `device_login_cmd`), both Node test suites, and both privacy disclosures/retention rows. Its copied enterprise design tokens also needed the new inbox receipt colors. Exact-copy test is retained; portal **31/31** and combined inbox **195/195** pass after synchronization. Full CLI integration validation is running.
- #87 at `b7a7eff`: newer iPad passes, iPhone reaches UI and fails only an arbitrary cell-index composer entry and the fifth-plan price assertion. Exported the newer result bundle and recording. Composer now uses the established explicit fixture-thread argument. Purchase controls are revealed fully below the translucent bar and selected-plan accessibility state is checked before payment; failure hierarchy/screenshot capture is added. Local build/targeted tests and fresh newer-runtime CI remain required.
- The public runbook no longer points active deployments at retired `159.69.201.24`. It records current source/runtime paths, both Compose image references, loopback bindings, backup/rollback rules and the existing bounded logs on `45.132.181.100`. This is a runbook correction, not a postbox restart.


## Checkpoint — native controls and account-switch test synchronization

- #87 now commits the exact-thread composer fixture, fully visible purchase-control gestures and selected-plan accessibility value at `339540d`. All three targeted iPhone tests pass locally, including every SKU purchase and the original $40 price assertion. Evidence: private `iphone-selection-visible.xcresult` and `iphone-selection-visible.log`. The newer-runtime iPhone CI result is still required; its previous iPad and StoreKit jobs passed.
- #89's Windows jobs, native call harness and all Rust/platform checks pass at `531eb12`. The remaining portal failure was an asynchronous test checking navigation before native SHA-256 completed. Added a captured-navigation promise and deliberately gated digest regression, preserving all masking, prompt and token-custody assertions; all 31 portal tests pass. Fix pushed at `5755eb7`, fresh CI pending. The first attempt to capture the focused test log used an unsupported run-node `--args` flag; its error is retained and the corrected invocation was run.
- Full integrated #83/#89 CLI validation passes, including the Unix storage-custody integration. No production postbox restart or native store submission occurred.


## Checkpoint — web qualification and integration with the operations runbook

- #89 at `5755eb7` passes every check, including the Windows suite and native call harness. The portal browser harness failed once at Chrome startup before the flow; retry of that failed job passes. This was not treated as a portal application assertion failure.
- Operations PR #91 passes every platform check and merged as `d14f540`. Its progress changes conflicted with #89's separate registry-clock checkpoint. Resolved the document conflict by retaining the current status and both evidence histories (`93e2235`); no application merge conflict or source change. A first merge request supplied an incorrect expected full hash and was correctly refused; the real head was read before retrying.
- #87's current Windows failure is a different existing Loft regression fixture: its repeated health probe allowed connection pooling against the server's intentional one-second transport deadline, and the Windows probe returned connection-aborted. The test now makes each health probe on a fresh transport; all status and six-second release assertions remain, with no production timeout or behavior changes. This isolates request-admission lifetime from idle connection reuse. Focused case and full Loft library **116/116** pass (one existing ignored case); Windows confirmation is still required.
- The latest #87 iPad, StoreKit, both Mac crypto jobs, group interop and native Windows call harness pass. The newer iPhone UI job remains in progress. Postbox rollout remains pending a clean integrated merge.


## Checkpoint — message-actions merged and postbox deployed

- #89 passed every platform and web check at `e5a2843`, including Windows at 20m32s, then merged as `b2a802512822ef504e29ab16699cea536d4ef5f9`. Candidate image `pigeonpost-postbox:p1e-e5a2843` has id `a2b6418c1352…`; its source tree exactly equals the merge. Built on the postbox host with a two-CPU/four-GiB ceiling. Isolated empty-data health check passes; an attempted --help smoke invocation was unsupported and is retained as failed evidence.
- Took a live SQLite backup via SQLite's backup API, checked its integrity, and retained compose/env files under root-only `/var/backups/pigeonpost/postbox-20261024T215411Z`. On a separate network-disabled DB copy, the candidate migrates/starts successfully and the old image still starts afterwards. An initial preparation attempt stopped before mutations because the backup parent directory was absent; the corrected preparation passes. No server backup was copied to the Mac.
- First live attempt used an incorrect anonymous-capabilities 200 assertion, ignoring the baseline's expected 401, and correctly rolled both images back. This was an operator validation error; the live database was retained, including intervening messages. Corrected the probe to assert the existing authenticated boundary and reran. Both postbox and reaper now run the candidate, with zero restarts, and source is at the merged commit. Production KMS/PoW configuration was retained; no ephemeral-key startup warning.
- Public postbox health200 and capabilities401, enterprise200 and approval page200 pass. Registry/directory/Loft remain running and healthy. No feature gate was manually enabled, no store submission occurred. Private scripts and successful/failed logs stay in the continuation release directory.
- #87 integrated main plus both native-call/message-action/pair test entries at `c3a3f0f`; full inbox web **197/197** passes, final CI pending. Native2 checkpoint `6d44cf7` has five local direct/status/privacy UI cases passing and both app builds passing. Newer Apple run `40135545110` is in progress; StoreKit reports an empty entitlement snapshot immediately after restore, while the exact native test passes locally (one actual test, not an empty selection). Reproduce/synchronize the native test before changing billing logic.
- Public cache preflight confirms `integrity.js` and app.js are cacheable for **four hours**, while WASM is no-cache. The #89 web bundle has not yet been deployed. The cache-coupling repair in this branch is a required rollout dependency; keep the pinned byte-integrity failure behavior.


## Checkpoint — coherent web crypto requests

- The loader, its glue/manifest imports, WASM fetch and the HTML group-entry reference now use the same full pinned digest. Reproducible generation stamps these references and checks them alongside the binary; later rebuilds cannot silently leave an old entry URL. Changed HTML asset URLs get a fresh rollout version rather than reusing a URL that may already cache the previous app for four hours.
- Two generator regressions pass, covering digest changes, idempotence, invalid digests and missing/duplicated import markers. Real WASM/MLS suite **10/10** passes, including a browser-style fetch returning stale bytes (refused), then current bytes (accepted), and exact no-cache/same-origin/digest request assertions. Full inbox suite **196/196** passed before the final added HTML-entry assertion; the focused MLS recheck passes **10/10**. WASM bytes remain unchanged at `f185d0bd…`; Linux reproducible rebuild and hosted CI remain required before deployment.
- Local draft supplied the small checked replacement helper and was reviewed; the Node test draft invented APIs and was rejected. Docdex indexed the edited script/test/group entry. Vendor symbols are excluded/missing and shell AST is unsupported, so actual module imports, workflow callers, parser checks and the real WASM runtime tests provide the additional checks.


- PR #92's clean Linux WASM/binding reproduction, multi-language interop, native/AAR builds and audit checks pass. The first web job reached the call-layout harness, but its exact unversioned config.js tag replacement stopped injecting the fake login after the new asset query was added. An audit found the same assumption in the Windows call peer. Both harnesses now preserve the matched config tag (including a query) while prepending their existing fixtures. Parser checks pass; actual layout/native call qualification remains in CI. No production call or authentication code changed for this fixture repair.


## Checkpoint — concurrent retry preflight race

- Hosted Linux at `7ccae5a` failed the real concurrent-retry integration case with `409 msg_ref_in_use`. The early validation read the sender, then the recipient in separate snapshots; an identical request could atomically commit between those reads. The final `enqueue_copies` transaction already distinguishes the identical sender/recipient pair from a competing reference claim under its IMMEDIATE writer lock.
- Removed the redundant recipient preflight read. Sender-side early duplicates/conflicts remain, and recipient-side conflicts remain authoritative in the unchanged atomic transaction. The real-socket test now releases 16 requests together, requires exactly one 201, fifteen duplicate 200s, identical non-null copy ids and one message per mailbox. The competing-sender201/409 test and self-send checks still pass.
- Full postbox **441/441** plus both worker tests pass (`msg-ref-race-postbox.log`); the16-way case also passes30 repeated runs after the change. It also passed30 runs before the change locally, so only the recorded hosted failure is the reproduction evidence; no claim of a deterministic local reproduction. A local draft review contradicted itself and was rejected.
- This fix is included in the pending coherent-web rollout because CI exposed a live-server retry defect during qualification. It is not deployed yet. No duplicate delivery or lost-message incident was observed, and the error already leaves the first successful delivery intact.
