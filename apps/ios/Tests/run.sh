#!/bin/sh
# The thread model, compiled for the mac it is run on — no simulator, no project, no XCTest.
# Everything under test is a pure function over its inputs, and keeping it runnable in one command
# is what makes it get run.
set -e
cd "$(dirname "$0")/.."
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
swiftc -O -o "$out/auth" Tests/AuthTests.swift Shared/Config.swift
"$out/auth"
swiftc -O -o "$out/thread-model" \
  Tests/main.swift \
  Shared/API/Models.swift \
  Shared/Model/Conversation.swift \
  Shared/Design/PeerFace.swift \
  Shared/Design/Theme.swift \
  Shared/Views/Markdown.swift
"$out/thread-model"
swiftc -O -o "$out/handles" \
  Tests/HandleStoreTests.swift Shared/API/Models.swift Shared/API/APIError.swift \
  Shared/Store/HandlePurchases.swift Shared/Store/HandleStore.swift
"$out/handles"
