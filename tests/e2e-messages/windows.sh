#!/bin/sh
# Opt-in native Windows Core protocol test against a disposable loopback Rust postbox.
set -eu
if [ "$#" -ne 1 ]; then printf '%s\n' 'usage: windows.sh /absolute/pigeonpost-postbox' >&2; exit 2; fi
case "$1" in /*) ;; *) printf '%s\n' 'Use an absolute binary path.' >&2; exit 2 ;; esac
test -x "$1"
task_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
umask 077
task_work=$(mktemp -d "${TMPDIR:-/tmp}/pp-windows-actions.XXXXXX")
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
node "$task_root/tests/e2e-messages/postbox.mjs" "$1" "$task_work/setup.json" &
task_fixture_pid=$!
task_attempt=0
while [ ! -s "$task_work/setup.json" ]; do
  if ! kill -0 "$task_fixture_pid" 2>/dev/null; then printf '%s\n' 'Loopback fixture exited before setup.' >&2; exit 1; fi
  task_attempt=$((task_attempt + 1))
  if [ "$task_attempt" -gt 160 ]; then printf '%s\n' 'Loopback fixture startup timed out.' >&2; exit 1; fi
  sleep 0.25
done
export PIGEONPOST_TEST_FIXTURE="$task_work/setup.json"
cd "$task_root"
"${PIGEONPOST_TEST_DOTNET:-dotnet}" run --project apps/windows/Pigeonpost.Core.Tests/Pigeonpost.Core.Tests.csproj -- --message-actions-postbox
