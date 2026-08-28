#!/bin/sh
# The composer, driven by a real keyboard. XCUITest is the only way to type into the app —
# `draft = ""` in `send()` can be read by eye, but whether UIKit's field agrees is a question only
# a keypress answers, and ledger item #6 is exactly that question.
#
# Standalone on purpose: this project does not touch Pigeonpost.xcodeproj, so a UI-test harness
# cannot break the app's own build or its archive.
#
#   UITests/run.sh [simulator-udid]
set -e
cd "$(dirname "$0")/.."
sim="${1:-$(xcrun simctl list devices available | awk -F'[()]' '/iPhone 16 Pro \(/ {print $2; exit}')}"
[ -n "$sim" ] || { echo "no iPhone simulator available"; exit 1; }
dd=$(mktemp -d)

xcodebuild -project Pigeonpost.xcodeproj -scheme Pigeonpost -configuration Debug \
  -sdk iphonesimulator -destination "id=$sim" -derivedDataPath "$dd/app" build >"$dd/build.log" 2>&1 ||
  { tail -30 "$dd/build.log"; exit 1; }

xcrun simctl boot "$sim" 2>/dev/null || true
xcrun simctl install "$sim" "$dd/app/Build/Products/Debug-iphonesimulator/Pigeonpost.app"

xcodebuild test -project UITests/PPUITests.xcodeproj -scheme PPUITests \
  -destination "id=$sim" -derivedDataPath "$dd/tests" 2>&1 |
  grep -E "^Test Case|error:|Executed .* tests"
