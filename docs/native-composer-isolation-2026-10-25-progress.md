# Native composer isolation progress

## Evidence

- PR #96 iPhone CI: group edit text was `Edited group messageraft` instead of `Edited group message`.
- PR #98 iPhone CI: saving an edit left `Revised taskMy unsent draft` in the ordinary draft; the expected saved version was absent.
- `Inbox.beginMessageAction` and group edit entry are synchronous. Direct-message bindings resolve the active edit dynamically, and the input view identity currently omits the edit UUID. The shared group field also spans ordinary and edit state.
- Scoped Docdex symbols/impact inspected before edits. Swift graphs have no edges. A local review draft provided only generic risks, so primary review and actual builds/tests remain necessary.

## Status

Implemented conversation/edit-scoped direct-message input identities and guarded text/focus/attachment callbacks, retired native delegates, and group edit generations. UIKit keeps marked text within one context; edit transitions replace the input view. The Mac input takes focus on its initial presentation as well as after sends.

## Validation

- iOS Debug build and universal arm64/x86_64 Mac build: pass.
- iPhone: all 10 composer cases and both focused group edit/reply cases pass, including consecutive sends, pending autocorrection and keyboard retention.
- iPad: direct-message and group edit/cancel/save cases pass.
- Calendar creation retry: initial test query waited for an offscreen lazy agenda row before scrolling and failed on both devices. The corrected test waits for editor dismissal, scrolls to the actual event button, and passes on both devices. No calendar product change was needed.
- Evidence in the private continuation release directory: `native-composer-isolation-{iphone,ipad}.xcresult`, `native-composer-calendar-{iphone,ipad}.xcresult`, and corresponding build/test logs under `validation/`. The first bundles include the superseded calendar-selector failure; they are not reported as wholly green runs.
- Hosted Mac and full CI qualification pending. No store submission or release qualification claimed.

## Hosted replacement selection follow-up

- At `b0a910a`, hosted iPhone has 67 cases, three existing skips and one assertion failure. All draft preservation and group cases pass. The retained version-history screenshot shows `Revised taskpin the flaky macOS test and push a fix`: Backspace ran at the beginning of the recreated input, so the original edit text was never selected/deleted.
- The UI test now uses the native Select All action and asserts the replacement value before saving. The ordinary-draft and exact version-history assertions remain. This is a test input correction; no product or persistence change.
- Evidence: private `pr96-b0a910a-iphone-ci`, exported edit attachments and `validation/pr96-b0a910a-iphone.log`. Focused Select All/edit/save/version-history cases pass on both owned iPhone and iPad simulators; hosted newer-runtime qualification remains pending.
