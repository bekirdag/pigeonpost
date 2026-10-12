# Mac editor hang — progress

- The user-provided report identifies Pigeonpost Desktop 1.0 (44), macOS 14.7.4 arm64, Event: hang, Duration: 37.30 seconds. All 15 target main-thread samples pass through MacMessageEditor.sizeThatFits and NSLayoutManager.ensureLayout. The resizing path includes NSTextView frame changes, NSClipView notifications and NSScrollView tiling.
- The current implementation changes the live NSTextContainer width during SwiftUI proposed-size measurement while widthTracksTextView and vertical resizing are enabled. Investigating this layout feedback cycle; no evidence of a network or authentication failure in the sampled target main thread.
- Isolated worktree: /Users/bekirdag/Documents/apps/pigeonpost-mac-hang-20261012; branch codex/mac-editor-hang-20261012 from main 8a3cb14. Original iOS checkout remains untouched.
- Latest public Mac release confirmed as macos-1.0-44; universal signed/notarized ZIP SHA-256 2ed7c530561bc1a81297f62fe464b568867c0bd0a7998afd9ac38fd1ae2e63f2. Existing mac-notarize.yml accepts a draft release and verifies its input hash before Apple submission.
- Docdex initialized the worktree; tree/open work. Search/index/graph readiness is still being checked because the first stats calls reported an index writer lock.

- Index initialization completed (506 documents); Swift symbols and retrieval DAG are available. Impact graphs for the editor and native tests contain no edges, so production MacThreadView and shell-compiled test references were checked manually.
- New native regression against the old sizing algorithm produced 6 failures: proposed-size measurement changed the live container/frame for five width probes, and an infinite proposal returned non-finite dimensions. Window resizing alone did not reproduce the full 37-second hang, so the evidence is the actual sampled call chain plus the deterministic live-layout mutation regression.
- The first full native run hit an unrelated test focus issue (Paste sent to no active responder); explicitly activate the fixture window before that clipboard check. The focused sizing run avoids clipboard interactions and reproduced the sizing failures independently.
- Docdex run-tests selected Cargo and rejected the Swift shell target. Using the repository workflow's native mac-inbox.sh runner. Local delegated test suggestions included irrelevant network/image input, so only the applicable resize/long-draft scenarios were retained.
- Implemented detached, height-bounded TextKit measurement with a four-width cache and finite zero/unspecified/infinite proposal handling. Live text, selection and scroll geometry are not changed by measuring. Retesting now.

- Fixed focused regression passes 36/36 checks; shared model suite passes (including 85 purchase-controller checks and 61 inbox checks). Long/short composer bitmap screenshots inspected. Screen-recording capture is unavailable locally, so the existing NSView bitmap fallback provides visual evidence.
- Universal Developer ID archive build 45 succeeded for x86_64 and arm64, macOS 14 minimum. Signed, pre-notarization ZIP SHA-256: 870dc71c556bb91cacbe5ac652f7b067a0a28df64a8df432884913e4c40ba9b8.
- Full native suite reached picker cycle 6 before an interaction timeout while building concurrently. Re-running without the concurrent archive build before deciding whether the app or test needs further correction.

- Full native suite retry passed (exit 0) without concurrent archive compilation: picker stress, conversation/details navigation, clipboard/attachment sending and composer sizing. No picker production change was needed. Docdex staged-change hook passed.
