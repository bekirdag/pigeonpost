#!/bin/sh
# Does a long conversation open on its newest message?
#
# The one question the screen recording asked, answered without a person watching. It builds the
# app, opens the `-long` fixture thread on a simulator, and asks the app where the floor of the
# thread ended up relative to the composer — see `LandingReport`. Floor at or above the composer is
# a conversation open at its end; anything else is the defect.
#
# Not an XCUITest, and that is deliberate: reading the accessibility hierarchy builds every row of a
# `LazyVStack`, which took the fixture thread's content height from 47,538 to 447,630 in the middle
# of the landing being measured. The same build then passed one run and failed the next three. This
# reads two numbers the app itself reports and touches nothing.
#
#   apps/ios/Tests/landing.sh [simulator-udid]
#
# The recording's geometry is an iPhone 13 Pro; any iPhone simulator shows it, and a narrower screen
# shows it harder, because every markdown row is taller.
set -e
cd "$(dirname "$0")/.."

sim="${1:-$(xcrun simctl list devices available | awk -F'[()]' '/iPhone 1[3-6].*\(/ {print $2; exit}')}"
[ -n "$sim" ] || { echo "no iPhone simulator available"; exit 1; }
dd=$(mktemp -d)
bundle=dev.pigeonpost.inbox

xcodebuild -project Pigeonpost.xcodeproj -scheme Pigeonpost -configuration Debug \
  -sdk iphonesimulator -destination "id=$sim" -derivedDataPath "$dd/app" build >"$dd/build.log" 2>&1 ||
  { tail -30 "$dd/build.log"; exit 1; }

xcrun simctl boot "$sim" 2>/dev/null || true
xcrun simctl bootstatus "$sim" -b >/dev/null 2>&1 || true
xcrun simctl install "$sim" "$dd/app/Build/Products/Debug-iphonesimulator/Pigeonpost.app"

fails=0

# Two lengths and one shape. Ninety messages is the recording; four hundred is the same thread with
# more behind it, and it has to behave the same or the fix is a threshold rather than a fix. The
# short mailbox is there so this cannot be passed by a change that only long threads survive.
for run in "ninety messages:-long" "four hundred messages:-long -long=400" "six messages:"; do
  what=${run%%:*}
  args=${run#*:}

  xcrun simctl terminate "$sim" "$bundle" 2>/dev/null || true
  xcrun simctl spawn "$sim" log stream --style compact \
    --predicate 'eventMessage CONTAINS "PIGEONPOST-LANDING"' >"$dd/log.txt" 2>&1 &
  streamer=$!
  sleep 2

  # shellcheck disable=SC2086
  xcrun simctl launch "$sim" "$bundle" -fixtures -report-landing -open=/bekir/agent1 $args >/dev/null
  sleep 12
  kill "$streamer" 2>/dev/null || true
  wait "$streamer" 2>/dev/null || true

  floor=$(grep -o 'floor=[-0-9]*' "$dd/log.txt" | tail -1 | cut -d= -f2)
  composer=$(grep -o 'composer=[-0-9]*' "$dd/log.txt" | tail -1 | cut -d= -f2)

  if [ -z "$composer" ]; then
    echo "  FAIL $what — the app never reported a composer; did it open the conversation?"
    fails=$((fails + 1))
  elif [ -z "$floor" ]; then
    # A lazy stack does not build rows it is nowhere near, so silence here is itself the answer:
    # the thread stopped far enough short of its end that the floor was never drawn at all.
    echo "  FAIL $what — the floor of the thread was never drawn: it opened short of its end"
    fails=$((fails + 1))
  # The thread is scrolled so that the *newest message* sits against the composer, which leaves the
  # few points of padding beneath it below the fold. That is the layout working; anything genuinely
  # short of the end is a screen or more, never a dozen points.
  elif [ "$floor" -gt "$((composer + 16))" ]; then
    echo "  FAIL $what — floor at y=$floor, composer at y=$composer:" \
      "$((floor - composer))pt of conversation left below the screen"
    fails=$((fails + 1))
  else
    echo "  ok   $what — floor at y=$floor, composer at y=$composer"
  fi
done

xcrun simctl terminate "$sim" "$bundle" 2>/dev/null || true
[ "$fails" -eq 0 ] || { echo "$fails failed"; exit 1; }
echo "all landed"
