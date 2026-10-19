#!/bin/sh
# Partial selection of message text on the Mac: the shared MarkdownText/SelectableText hosted in a
# real window, selected with posted mouse events, copied to a private pasteboard.
#
#   Tests/mac-selection.sh [snapshot.png]
set -e
cd "$(dirname "$0")/.."
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
swiftc -parse-as-library -o "$out/mac-selection" Tests/MacSelectionTests.swift \
  Shared/Design/Theme.swift Shared/Views/Markdown.swift Shared/Views/SelectableText.swift
"$out/mac-selection" "$@"
