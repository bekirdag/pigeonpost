#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
app="$out/Mac Inbox Tests.app"
mkdir -p "$app/Contents/MacOS"
cat > "$app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>mac-inbox</string>
<key>CFBundleIdentifier</key><string>dev.pigeonpost.mac-inbox-tests</string>
<key>CFBundleName</key><string>Mac Inbox Tests</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>NSPrincipalClass</key><string>NSApplication</string>
</dict></plist>
PLIST
swiftc -O -parse-as-library -D MAC_INBOX_UI_TESTS -o "$app/Contents/MacOS/mac-inbox" \
  Tests/InboxLoadingTests.swift Tests/MacInboxLoadingTests.swift \
  Shared/Config.swift Shared/API/Models.swift Shared/API/APIError.swift Shared/API/PostboxClient.swift \
  Shared/Model/Inbox.swift Shared/Model/Conversation.swift Shared/Model/StagedFile.swift \
  Shared/Design/PeerFace.swift Shared/Design/Theme.swift Shared/Design/Platform.swift Shared/Design/Time.swift \
  Shared/Views/Markdown.swift Shared/Views/Components.swift Shared/Views/MessageBubble.swift \
  Shared/Views/AttachmentViews.swift PigeonpostMac/MacInboxView.swift PigeonpostMac/MacThreadView.swift
# Run as a normal macOS app and keep its output. `open` does not propagate the app's exit
# status, so require its explicit successful-completion line after it terminates.
open -n -W --stdout "$out/stdout" --stderr "$out/stderr" \
  --env "PIGEONPOST_UI_TEST_OUTPUT=${PIGEONPOST_UI_TEST_OUTPUT:-}" "$app"
cat "$out/stdout"
cat "$out/stderr" >&2
grep -Eq '^Mac inbox integration: [0-9]+ checks, 0 failures$' "$out/stdout"
