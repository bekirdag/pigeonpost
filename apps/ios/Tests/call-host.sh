#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
out=$(mktemp -d)
pid=""
trap '[ -z "$pid" ] || kill "$pid" 2>/dev/null; rm -rf "$out"' EXIT
swiftc -O -parse-as-library -D DEBUG -o "$out/call-host" \
  Tests/CallHostTests.swift Shared/API/Models.swift Shared/Design/Theme.swift Shared/Calls/CallHost.swift
python3 Tests/call-host-page.py "$out/port" &
pid=$!
i=0
while [ ! -s "$out/port" ]; do
  i=$((i + 1))
  [ "$i" -le 40 ] && kill -0 "$pid" 2>/dev/null || exit 1
  sleep .1
done
"$out/call-host" -calls-page "http://127.0.0.1:$(cat "$out/port")/native-call.html"
