#!/bin/sh
# Native Android store captures from the debug `marketing` fixture (apps/android/app/src/debug/.../MarketingFixture.kt):
# the invented world of marketing/store/seed/demo-world.json, real group encryption on an in-memory postbox,
# no network and no account. Writes marketing/store/captures/android-{phone,tablet}/<scene>.png.
#
#   marketing/store/capture/android/capture.sh            # both devices
#   marketing/store/capture/android/capture.sh phone      # or: tablet
#   SCENES=inbox,group marketing/store/capture/android/capture.sh phone
#
# Uses its own throwaway AVDs on their own ports (never another agent's emulator), created on first use:
#   PP_mkt_phone  pixel_6       1080x2400 portrait   port 5640
#   PP_mkt_tablet pixel_tablet  2560x1600 landscape  port 5642
# Needs the group-crypto AAR at crates/pigeonpost-groupcrypto/target/android/ (see apps/android/README.md).
# Delete the AVDs afterwards with: avdmanager delete avd -n PP_mkt_phone (and PP_mkt_tablet).
set -e
ROOT=$(cd "$(dirname "$0")/../../../.." && pwd)
SDK=${ANDROID_HOME:-$HOME/Library/Android/sdk}
AVDM=$SDK/cmdline-tools/latest/bin/avdmanager
IMAGE="system-images;android-34;google_apis;arm64-v8a"
OUT=$ROOT/marketing/store/captures
TARGETS=${1:-phone tablet}

start() { # name device port
  "$SDK/emulator/emulator" -list-avds | grep -qx "$1" || {
    echo no | "$AVDM" create avd -n "$1" -k "$IMAGE" -d "$2" >/dev/null
    sed -i '' 's/^hw.gpu.enabled=.*/hw.gpu.enabled=yes/; s/^hw.gpu.mode=.*/hw.gpu.mode=host/' "$HOME/.android/avd/$1.avd/config.ini"
  }
  adb devices | grep -q "emulator-$3" || {
    nohup "$SDK/emulator/emulator" -avd "$1" -port "$3" -no-audio -camera-front none -camera-back none -no-snapshot -no-window -gpu host >/dev/null 2>&1 &
    adb -s "emulator-$3" wait-for-device
    until [ "$(adb -s "emulator-$3" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = 1 ]; do sleep 3; done
    sleep 10
  }
}

for t in $TARGETS; do
  case $t in
    phone) avd=PP_mkt_phone; device=pixel_6; port=5640 ;;
    tablet) avd=PP_mkt_tablet; device=pixel_tablet; port=5642 ;;
    *) echo "unknown target $t"; exit 1 ;;
  esac
  start $avd $device $port
  serial=emulator-$port
  "$ROOT/marketing/store/capture/android/device-setup.sh" $serial
  adb -s $serial shell rm -rf /sdcard/Android/data/dev.pigeonpost.inbox.debug/files/marketing
  (cd "$ROOT/apps/android" && ANDROID_SERIAL=$serial ./gradlew :app:connectedDebugAndroidTest --console=plain \
    -Pandroid.injected.androidTest.leaveApksInstalledAfterRun=true \
    -Pandroid.testInstrumentationRunnerArguments.class=dev.pigeonpost.inbox.MarketingScreenshots \
    -Pandroid.testInstrumentationRunnerArguments.marketingShots=$t \
    ${SCENES:+-Pandroid.testInstrumentationRunnerArguments.scenes=$SCENES})
  tmp=$(mktemp -d)
  adb -s $serial pull /sdcard/Android/data/dev.pigeonpost.inbox.debug/files/marketing/$t "$tmp" >/dev/null
  mkdir -p "$OUT/android-$t"
  cp "$tmp/$t"/*.png "$OUT/android-$t/"
  rm -rf "$tmp"
  echo "android-$t:"; ls "$OUT/android-$t"
done
