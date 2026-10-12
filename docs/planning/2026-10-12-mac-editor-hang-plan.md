# Mac editor hang repair and release

User authorization: fix the Mac desktop hang in Downloads/pp_mac_crash_1.txt and deploy a new version. Base main: 8a3cb14602a540a9fdf783eba8050223ef6dfc48. The existing iOS checkout is preserved.

## Acceptance and implementation

1. Isolate the 37-second build 44 hang from the target-process stack, without copying the full system report or private user data into the repo. Trace MacMessageEditor.sizeThatFits and its AppKit/SwiftUI dependencies.
2. Reproduce the sizing failure with a native macOS regression, then make proposed-size measurement independent of the live editable text container. Keep text, IME, selection, undo, scrolling, Return/Shift-Return and staged attachments working. Bound long-draft measurement and handle narrow/unspecified width proposals.
3. Run focused native editor checks on Sonoma, existing model/native inbox tests, and release compilation for Apple silicon and Intel. Review native screenshots; verify the release excludes fixtures. Use AST/impact/DAG evidence before code changes, recording any tool limitations.
4. Increment only the Mac build, commit/push the scoped fix, and validate the relevant GitHub checks. Build a universal Developer ID release, publish a draft artifact, notarize/staple it through the existing controlled workflow, and verify the exact downloaded archive.
5. Publish the verified Mac release, update the public Mac download link through the established deployment process, and verify the live download and hashes. Preserve build 44 as the rollback reference; no backend, Windows or mobile release is in scope.

## Evidence and authorization

Keep a separate progress log with root cause, regression result, build/test output, release commit, notarization result, archive hash and live URL. The user's instruction already authorizes versioning, git publication and deployment after validation. Clone-inferred approval gates do not require another confirmation. No messages are to be sent to other people.
