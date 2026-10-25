#!/bin/sh
# Native calendar API/store against a temporary, loopback-only postbox and synthetic member JWTs.
set -eu
umask 077
cd "$(dirname "$0")/.."
root="$(cd ../.. && pwd)"
out=$(mktemp -d)
pid=""
cleanup() {
  if [ -n "$pid" ]; then kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; fi
  rm -rf "$out"
}
trap cleanup EXIT
bin="${POSTBOX_BIN:-}"
if [ -z "$bin" ]; then
  (cd "$root" && cargo build -q -p pigeonpost-postbox)
  bin="$root/target/debug/pigeonpost-postbox"
fi
swiftc -O -parse-as-library -o "$out/calendar-e2e" \
  Tests/CalendarE2ETests.swift Shared/Config.swift \
  Shared/API/Models.swift Shared/API/APIError.swift Shared/API/PostboxClient.swift \
  Shared/Calendar/CalendarModels.swift Shared/Calendar/CalendarTime.swift Shared/Calendar/CalendarAPI.swift \
  Shared/Calendar/CalendarDraft.swift Shared/Calendar/CalendarImport.swift Shared/Calendar/CalendarStore.swift
node Tests/groups-postbox.mjs "$bin" "$out/setup.json" calendar </dev/null &
pid=$!
i=0
while [ ! -s "$out/setup.json" ]; do
  i=$((i + 1))
  [ "$i" -gt 240 ] && { echo "The test postbox did not start." >&2; exit 1; }
  kill -0 "$pid" 2>/dev/null || { echo "The test postbox exited." >&2; exit 1; }
  sleep 0.25
done
"$out/calendar-e2e" "$out/setup.json"
