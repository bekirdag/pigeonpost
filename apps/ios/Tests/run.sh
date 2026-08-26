#!/bin/sh
# The thread model, compiled for the mac it is run on — no simulator, no project, no XCTest.
# Everything under test is a pure function over its inputs, and keeping it runnable in one command
# is what makes it get run.
set -e
cd "$(dirname "$0")/.."
out=$(mktemp -d)
swiftc -O -o "$out/thread-model" \
  Tests/main.swift \
  Shared/API/Models.swift \
  Shared/Model/Conversation.swift \
  Pigeonpost/Design/PeerFace.swift \
  Pigeonpost/Design/Theme.swift \
  Pigeonpost/Views/Markdown.swift
"$out/thread-model"
