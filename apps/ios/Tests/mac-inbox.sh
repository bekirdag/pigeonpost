#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
swiftc -O -parse-as-library -D MAC_INBOX_UI_TESTS -o "$out/mac-inbox" \
  Tests/InboxLoadingTests.swift Tests/MacInboxLoadingTests.swift \
  Shared/Config.swift Shared/API/Models.swift Shared/API/APIError.swift Shared/API/PostboxClient.swift \
  Shared/Model/Inbox.swift Shared/Model/Conversation.swift \
  Shared/Design/PeerFace.swift Shared/Design/Theme.swift Shared/Design/Platform.swift Shared/Design/Time.swift \
  Shared/Views/Markdown.swift PigeonpostMac/MacInboxView.swift
"$out/mac-inbox"
