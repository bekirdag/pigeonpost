#!/bin/sh
# Prepares one emulator for store captures: gesture navigation (no taskbar app icons), a pinned
# 9:41 demo-mode status bar and the device clock set to 09:41 today so message times read as morning.
#   device-setup.sh emulator-5640
set -e
D="$1"; A="adb -s $D"
$A root >/dev/null 2>&1 || true; sleep 2
$A shell cmd overlay enable com.android.internal.systemui.navbar.gestural >/dev/null 2>&1 || true
$A shell cmd overlay disable com.android.internal.systemui.navbar.threebutton >/dev/null 2>&1 || true
# Pixel Tablet: the API 34 launcher pins a taskbar showing the hotseat and predicted apps. On this
# throwaway AVD only: empty the hotseat, turn app predictions off and drop the cached ones, so the
# taskbar carries nothing but the all-apps button.
L=com.google.android.apps.nexuslauncher
$A shell am force-stop $L
$A shell "sqlite3 /data/data/$L/databases/launcher_*.db 'delete from favorites where container=-101;'" >/dev/null 2>&1 || true
$A shell rm -f /data/data/$L/files/hotseat_predictions.xml /data/data/$L/files/all_apps_predictions.xml
$A shell pm disable-user --user 0 com.google.android.as >/dev/null 2>&1 || true
$A shell am start -a android.intent.action.MAIN -c android.intent.category.HOME >/dev/null; sleep 4
$A shell settings put global auto_time 0
$A shell date "$(date +%m%d)0941$(date +%Y).00" >/dev/null
$A shell settings put global sysui_demo_allowed 1
$A shell am broadcast -a com.android.systemui.demo -e command enter >/dev/null
$A shell am broadcast -a com.android.systemui.demo -e command clock -e hhmm 0941 >/dev/null
$A shell am broadcast -a com.android.systemui.demo -e command notifications -e visible false >/dev/null
$A shell am broadcast -a com.android.systemui.demo -e command battery -e level 100 -e plugged false >/dev/null
$A shell am broadcast -a com.android.systemui.demo -e command network -e wifi show -e fully true -e level 4 -e mobile show -e datatype none -e level 4 >/dev/null
