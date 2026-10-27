#!/bin/sh
# Opt-in Linux desktop protocol tests against a disposable loopback Rust postbox.
#   tests/e2e-messages/linux.sh /absolute/pigeonpost-postbox [groups|calendar|orgs]
# Runs apps/linux/tests/live_<messages|groups|calendar|orgs>.py with the app's own Python code.
set -eu
if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then printf '%s\n' 'usage: linux.sh /absolute/pigeonpost-postbox [groups|calendar|orgs]' >&2; exit 2; fi
case "$1" in /*) ;; *) printf '%s\n' 'Use an absolute binary path.' >&2; exit 2 ;; esac
test -x "$1"
task_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
umask 077
task_work=$(mktemp -d "${TMPDIR:-/tmp}/pp-linux-actions.XXXXXX")
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
task_mode=${2:-}
case "$task_mode" in ""|groups|calendar|orgs) ;; *) printf '%s\n' 'Unknown fixture mode.' >&2; exit 2 ;; esac
node "$task_root/tests/e2e-messages/postbox.mjs" "$1" "$task_work/setup.json" $task_mode &
task_fixture_pid=$!
task_attempt=0
while [ ! -s "$task_work/setup.json" ]; do
  if ! kill -0 "$task_fixture_pid" 2>/dev/null; then printf '%s\n' 'Loopback fixture exited before setup.' >&2; exit 1; fi
  task_attempt=$((task_attempt + 1))
  if [ "$task_attempt" -gt 160 ]; then printf '%s\n' 'Loopback fixture startup timed out.' >&2; exit 1; fi
  sleep 0.25
done
export PIGEONPOST_TEST_FIXTURE="$task_work/setup.json"
export XDG_DATA_HOME="$task_work/data"
cd "$task_root/apps/linux"
"${PYTHON:-python3}" -I -c 'import sys; sys.path.insert(0, "."); sys.path.insert(0, "tests"); import runpy; runpy.run_path(sys.argv[1], run_name="__main__")' "tests/live_${task_mode:-messages}.py"
