#!/bin/sh
# Group E2EE through the Swift binding, against a real postbox on this Mac.
#
#   sh apps/ios/Tests/groups-e2e.sh            # builds the postbox unless POSTBOX_BIN is set
#
# Needs Rust (the crate pins 1.91.1) and Node 20+. Builds the group-crypto library for this Mac with
# the same script the Xcode targets use, starts Tests/groups-postbox.mjs, and runs
# Tests/GroupsE2ETests.swift with the app's own GroupEngine, GroupAPI and GroupVault — beside the web
# inbox's own client (Tests/groups-web-peer.mjs, site-inbox/groups-core.js + WASM) for interop.
set -eu
cd "$(dirname "$0")/.."
root="$(cd ../.. && pwd)"
out=$(mktemp -d)
pid=""
trap '[ -n "$pid" ] && kill "$pid" 2>/dev/null; rm -rf "$out"' EXIT

bin="${POSTBOX_BIN:-}"
if [ -z "$bin" ]; then
  (cd "$root" && cargo build -q -p pigeonpost-postbox)
  bin="$root/target/debug/pigeonpost-postbox"
fi
PLATFORM_NAME=macosx ARCHS="$(uname -m)" ./scripts/build-groupcrypto.sh "$out/lib" >/dev/null
binding="$root/crates/pigeonpost-groupcrypto/bindings/swift"
swiftc -O -parse-as-library -D GROUPCRYPTO -o "$out/groups-e2e" \
  -Xcc -fmodule-map-file="$binding/PigeonpostGroupCryptoFFI.modulemap" "$binding/PigeonpostGroupCrypto.swift" \
  -L "$out/lib" -lpigeonpost_groupcrypto_ffi \
  Tests/GroupsE2ETests.swift Shared/Config.swift Shared/Auth/AuthorizationLease.swift \
  Shared/API/Models.swift Shared/API/APIError.swift Shared/API/PostboxClient.swift \
  Shared/Groups/GroupModels.swift Shared/Groups/GroupAPI.swift Shared/Groups/GroupVault.swift \
  Shared/Groups/GroupEngine.swift Shared/Groups/GroupText.swift 2>"$out/swiftc.log" || { grep -A3 "error:" "$out/swiftc.log" >&2; exit 1; }
[ -x "$out/groups-e2e" ] || { echo "groups-e2e did not compile" >&2; exit 1; }

node Tests/groups-postbox.mjs "$bin" "$out/setup.json" </dev/null &
pid=$!
i=0
while [ ! -s "$out/setup.json" ]; do
  i=$((i + 1))
  [ $i -gt 240 ] && { echo "the test postbox did not start" >&2; exit 1; }
  kill -0 "$pid" 2>/dev/null || { echo "the test postbox exited" >&2; exit 1; }
  sleep 0.25
done
node Tests/groups-web-peer.mjs "$out/setup.json" &
web=$!
status=0
"$out/groups-e2e" "$out/setup.json" || status=1
wait "$web" || { echo "the web client's side failed" >&2; status=1; }
exit $status
