# App Store screenshots

`6.9-inch/` — 1320×2868, iPhone 16 Pro Max simulator, portrait.
`13-inch-ipad/` — 2752×2064, iPad Pro 13-inch (M4) simulator, landscape. Required since the app runs
natively on iPad.

`mac/` — 2880×1800, the Mac app's window at 1440×900 points on a Retina screen, dark, from the
same fixtures. A Debug build (`-window=1440x900` sizes the window; it is Debug-only):

```
APP="<DerivedData>/Build/Products/Debug/Pigeonpost Desktop.app/Contents/MacOS/Pigeonpost Desktop"
"$APP" -fixtures -appearance=dark -window=1440x900 "-open=/bekir/agent1" &   # store-1-conversation
"$APP" -fixtures -appearance=dark -window=1440x900 -sheet=settings &          # store-2-settings
screencapture -x -o -l<window id of that process> out.png
```

The capture has transparent rounded corners; flatten it onto an opaque background (no alpha
channel, same pixel size) before uploading.

The iPhone and iPad sets are taken by one UI test, in the dark appearance, with the status bar pinned to 9:41:

```
xcrun simctl status_bar <udid> override --time 9:41 --batteryState charged --batteryLevel 100
TEST_RUNNER_PP_STORE_SHOTS=<out dir> xcodebuild test -project UITests/PPUITests.xcodeproj \
  -scheme PPUITests -destination id=<udid> -only-testing:PPUITests/IOSUXTests/testStoreScreenshots
```

The captures carry an orientation tag over portrait pixels; App Store Connect checks pixel size, so
bake the rotation in before uploading (an ImageIO thumbnail "with transform" at full size does it).
The older `simctl io screenshot` steps below still work for portrait iPhone shots.

**Taken from the fixture mailbox, never a real one.** Everything on screen is invented in
`Pigeonpost/Debug/Fixtures.swift`: the agents, the addresses, the messages, the held `run_tests`
request. Nothing private, half-finished, or belonging to a real correspondent ends up on a store
page.

```
xcrun simctl boot "iPhone 16 Pro Max"
xcrun simctl install "iPhone 16 Pro Max" <path to Pigeonpost.app>
xcrun simctl launch "iPhone 16 Pro Max" dev.pigeonpost.inbox -fixtures
xcrun simctl io "iPhone 16 Pro Max" screenshot 6.9-inch/store-1-list.png
xcrun simctl terminate "iPhone 16 Pro Max" dev.pigeonpost.inbox
xcrun simctl launch "iPhone 16 Pro Max" dev.pigeonpost.inbox -fixtures "-open=/bekir/agent1"
xcrun simctl io "iPhone 16 Pro Max" screenshot 6.9-inch/store-2-thread.png
```

1. **store-1-list** — the conversation list: an agent's fleet, an unread stranger, a held request.
2. **store-2-thread** — a conversation with two subjects, both halves of it, and a scoped request
   rendered as what it asks for with the reason it was held.

A third worth having is the trusted-sender editor, which is what makes the product's argument about
admission and autonomy. It needs two taps to reach, so it is not scriptable through launch arguments
the way these two are — take it by hand: Settings → a sender under Trusted senders.
