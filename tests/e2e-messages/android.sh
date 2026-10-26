#!/bin/sh
# Requires the existing Android AAR and a built pigeonpost-postbox; never touches a real account.
set -eu
if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
  printf '%s\n' 'usage: tests/e2e-messages/android.sh /absolute/pigeonpost-postbox [groups|calendar]' >&2
  exit 2
fi
case "$1" in /*) ;; *) printf '%s\n' 'Use an absolute binary path.' >&2; exit 2 ;; esac
test -x "$1"
task_mode=${2:-}
case "$task_mode" in ''|groups|calendar) ;; *) printf '%s\n' 'Unknown fixture mode.' >&2; exit 2 ;; esac
task_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
umask 077
task_work=$(mktemp -d "${TMPDIR:-/tmp}/pp-android-actions.XXXXXX")
task_fixture_pid=
cleanup() {
  if [ -n "$task_fixture_pid" ]; then
    kill "$task_fixture_pid" 2>/dev/null || true
    wait "$task_fixture_pid" 2>/dev/null || true
  fi
  rm -rf -- "$task_work"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
node "$task_root/tests/e2e-messages/postbox.mjs" "$1" "$task_work/setup.json" "$task_mode" &
task_fixture_pid=$!
task_attempt=0
while [ ! -s "$task_work/setup.json" ]; do
  if ! kill -0 "$task_fixture_pid" 2>/dev/null; then
    printf '%s\n' 'Loopback fixture exited before setup.' >&2
    exit 1
  fi
  task_attempt=$((task_attempt + 1))
  if [ "$task_attempt" -gt 160 ]; then printf '%s\n' 'Loopback fixture startup timed out.' >&2; exit 1; fi
  sleep 0.25
done
export PIGEONPOST_TEST_FIXTURE="$task_work/setup.json"
cd "$task_root/apps/android"
case "$task_mode" in
  groups) task_check=:core:groupActionsPostbox ;;
  calendar) task_check=:core:calendarPostbox ;;
  *) task_check=:core:messageActionsPostbox ;;
esac
./gradlew "$task_check" --no-daemon --max-workers=4
