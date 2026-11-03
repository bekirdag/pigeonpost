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
  Shared/People/PeopleModels.swift Shared/People/PeopleAPI.swift Shared/People/PeopleStore.swift \
  Shared/People/PeopleNameViews.swift Shared/People/PeopleLabels.swift \
  Shared/Config.swift Shared/Auth/AuthorizationLease.swift Shared/API/Models.swift Shared/API/APIError.swift Shared/API/PostboxClient.swift \
  Shared/Model/Inbox.swift Shared/Model/Conversation.swift Shared/Model/StagedFile.swift \
  Shared/Design/PeerFace.swift Shared/Design/Theme.swift Shared/Design/Platform.swift Shared/Design/Time.swift \
  Shared/Views/Markdown.swift Shared/Views/SelectableText.swift Shared/Views/Components.swift Shared/Views/MessageBubble.swift Shared/Views/MessageActions.swift \
  Shared/Views/AttachmentViews.swift Shared/Views/AttachmentPreview.swift Shared/Views/Desktop/DesktopInboxView.swift Shared/Views/NewThreadSheet.swift \
  PigeonpostMac/MacInboxView.swift PigeonpostMac/MacThreadView.swift \
  PigeonpostMac/MacMessageEditor.swift Shared/Calls/CallHost.swift \
  Shared/Model/Features.swift Shared/API/Phase1bModels.swift Shared/API/PostboxClient+Phase1b.swift \
  Shared/Model/CallLog.swift Shared/Model/FilteredMail.swift Shared/Model/AvatarStore.swift \
  Shared/Model/Blocking.swift Shared/Model/Blocking+Actions.swift Shared/Design/ImageTools.swift \
  Shared/Views/Phase1b/BlockSheet.swift Shared/Views/Phase1b/CallsView.swift Shared/Views/Phase1b/FilteredViews.swift \
  Shared/Model/PickedImage.swift Shared/Groups/*.swift Shared/Groups/Views/*.swift \
  Shared/Calendar/*.swift Shared/Meetings/*.swift Shared/Enterprise/DevicePairingModels.swift Shared/Enterprise/DeviceApprovalRouting.swift
# Run as a normal macOS app and keep its output. `open` does not propagate the app's exit
# status, so require its explicit successful-completion line after it terminates.
open -n -W --stdout "$out/stdout" --stderr "$out/stderr" \
  --env "PIGEONPOST_UI_TEST_OUTPUT=${PIGEONPOST_UI_TEST_OUTPUT:-}" "$app" --args "$@"
cat "$out/stdout"
cat "$out/stderr" >&2
grep -Eq '^Mac inbox integration: [0-9]+ checks, 0 failures$' "$out/stdout"
