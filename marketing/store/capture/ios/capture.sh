#!/bin/sh
# Native iOS store captures from the invented world (`-fixtures -marketing`, Debug build only).
#
#   marketing/store/capture/ios/capture.sh
#
# Creates its own two simulators (never touches anyone else's), builds the Debug app, runs the
# MarketingShots UI test on each, bakes the iPad landscape rotation into the pixels, writes
# marketing/store/captures/ios-phone/*.png (1320x2868) and ios-tablet/*.png (2752x2064), and deletes
# the simulators. Needs Xcode 16 with the iOS 18.3 runtime and ImageMagick (`magick`).
#
# live.png and meeting-notes.png come from native screens that exist only from Apple round three
# (PR #148, branch claude/apple-round3-20261027; main once it merges). Take them from a checkout of
# that branch with r3-live-notes.patch applied on top of this branch's apps/ios fixture changes:
#
#   git worktree add --detach /tmp/r3 e0555922
#   git diff a101d29e -- apps/ios | git -C /tmp/r3 apply     # this branch's -marketing fixtures (until merged)
#   git -C /tmp/r3 apply "$PWD/marketing/store/capture/ios/r3-live-notes.patch"
#   PP_APP_SOURCE=/tmp/r3/apps/ios PP_MARKETING_SCENES=live,meeting-notes marketing/store/capture/ios/capture.sh
set -eu
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../../.." && pwd)
runtime=${PP_SIM_RUNTIME:-com.apple.CoreSimulator.SimRuntime.iOS-18-3}
work=$(mktemp -d)
source=${PP_APP_SOURCE:-$repo/apps/ios}
scenes=${PP_MARKETING_SCENES:-inbox,agent-thread,group,calendar,teams}
want=$(echo "$scenes" | tr ',' '\n' | grep -c .)
phone=$(xcrun simctl create PP-mkt-iPhone "iPhone 16 Pro Max" "$runtime")
pad=$(xcrun simctl create PP-mkt-iPad13 "iPad Pro 13-inch (M4)" "$runtime")
cleanup() {
  for id in "$phone" "$pad"; do xcrun simctl shutdown "$id" 2>/dev/null || true; xcrun simctl delete "$id" 2>/dev/null || true; done
}
trap cleanup EXIT

xcodebuild -project "$source/Pigeonpost.xcodeproj" -scheme Pigeonpost -configuration Debug \
  -sdk iphonesimulator -destination "id=$phone" -derivedDataPath "$work/app" build >"$work/build.log" 2>&1 ||
  { tail -30 "$work/build.log"; exit 1; }
app="$work/app/Build/Products/Debug-iphonesimulator/Pigeonpost.app"

shoot() { # <udid> <out dir>
  xcrun simctl boot "$1" 2>/dev/null || true
  xcrun simctl bootstatus "$1" -b >/dev/null
  xcrun simctl status_bar "$1" override --time 9:41 --batteryState charged --batteryLevel 100 --cellularBars 4 --wifiBars 3
  xcrun simctl install "$1" "$app"
  # Warm the first launch; a cold one can outlast XCUITest's launch timeout on a busy machine.
  xcrun simctl launch "$1" dev.pigeonpost.inbox -fixtures -marketing >/dev/null || true; sleep 10
  xcrun simctl terminate "$1" dev.pigeonpost.inbox || true
  # Each run skips the scenes already captured, so a launch timeout costs one retry, not the set.
  for attempt in 1 2 3 4; do
    TEST_RUNNER_PP_MARKETING_SHOTS="$2" TEST_RUNNER_PP_MARKETING_SCENES="$scenes" xcodebuild test -project "$here/MarketingShots.xcodeproj" -scheme MarketingShots \
      -destination "id=$1" -derivedDataPath "$work/tests" 2>&1 | grep -E "error:|Test Case .* (passed|failed)" || true
    [ "$(ls "$2"/*.png 2>/dev/null | wc -l)" -ge "$want" ] && return 0
    echo "attempt $attempt left $(ls "$2"/*.png 2>/dev/null | wc -l) of $want scenes; retrying"
  done
  echo "could not capture every scene on $1"; return 1
}

shoot "$phone" "$work/phone"
shoot "$pad" "$work/pad"

mkdir -p "$repo/marketing/store/captures/ios-phone" "$repo/marketing/store/captures/ios-tablet"
for f in "$work"/phone/*.png; do
  magick "$f" -strip -colorspace sRGB -alpha off "$repo/marketing/store/captures/ios-phone/$(basename "$f")"
done
# The UI test redraws each shot upright, so iPad files should already be 2752x2064. Anything still
# portrait-shaped is turned a quarter counterclockwise (how landscapeLeft comes back untransformed).
for f in "$work"/pad/*.png; do
  rotate=0; [ "$(sips -g pixelWidth "$f" | awk '/pixelWidth/{print $2}')" -lt "$(sips -g pixelHeight "$f" | awk '/pixelHeight/{print $2}')" ] && rotate=270
  magick "$f" -strip -rotate "$rotate" -colorspace sRGB -alpha off "$repo/marketing/store/captures/ios-tablet/$(basename "$f")"
done
sips -g pixelWidth -g pixelHeight "$repo"/marketing/store/captures/ios-*/*.png | paste - - -
