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
swiftc -O -DDEBUG -parse-as-library -o "$out/session-lifetime" Tests/SessionLifetimeTests.swift Shared/Config.swift \
  Shared/API/APIError.swift Shared/Auth/StrongSignIn.swift Shared/Auth/Session.swift
"$out/session-lifetime"
swiftc -O -parse-as-library -o "$out/strong-signin" Tests/StrongSignInTests.swift Shared/Config.swift Shared/API/APIError.swift Shared/Auth/StrongSignIn.swift
"$out/strong-signin"
swiftc -O -parse-as-library -o "$out/device-pairing" Tests/DevicePairingTests.swift Shared/Enterprise/DevicePairingModels.swift
"$out/device-pairing"
swiftc -O -parse-as-library -o "$out/postbox-auth" Tests/PostboxAuthenticationTests.swift Shared/Config.swift \
  Shared/API/Models.swift Shared/API/APIError.swift Shared/API/PostboxClient.swift \
  Shared/Enterprise/DevicePairingModels.swift Shared/Enterprise/DevicePairingAPI.swift
"$out/postbox-auth"
swiftc -O -parse-as-library -o "$out/device-approval" Tests/DeviceApprovalStoreTests.swift Shared/Config.swift \
  Shared/API/Models.swift Shared/API/APIError.swift Shared/API/PostboxClient.swift \
  Shared/Enterprise/DevicePairingModels.swift Shared/Enterprise/DevicePairingAPI.swift Shared/Enterprise/DeviceApprovalStore.swift
"$out/device-approval"
swiftc -O -o "$out/thread-model" \
  Tests/main.swift \
  Shared/API/Models.swift \
  Shared/Model/Conversation.swift \
  Shared/Design/PeerFace.swift \
  Shared/Design/Theme.swift \
  Shared/Views/Markdown.swift Shared/Views/SelectableText.swift
"$out/thread-model"
swiftc -O -o "$out/handles" \
  Tests/HandleStoreTests.swift Shared/API/Models.swift Shared/API/APIError.swift \
  Shared/Store/HandlePurchases.swift Shared/Store/HandleStore.swift
"$out/handles"
swiftc -O -parse-as-library -o "$out/inbox-loading" \
  Tests/InboxLoadingTests.swift Shared/Config.swift \
  Shared/API/Models.swift Shared/API/APIError.swift Shared/API/PostboxClient.swift \
  Shared/Model/Inbox.swift Shared/Model/Conversation.swift \
  Shared/Design/PeerFace.swift Shared/Design/Theme.swift Shared/Views/Markdown.swift Shared/Views/SelectableText.swift
"$out/inbox-loading"
swiftc -O -o "$out/ios-navigation" Tests/IOSNavigationTests.swift Shared/API/Models.swift \
  Pigeonpost/Views/MailboxOrder.swift Pigeonpost/Views/HistoryWindow.swift
"$out/ios-navigation"
swiftc -O -parse-as-library -o "$out/call-rings" Tests/CallRingTests.swift Shared/Calls/CallRing.swift
"$out/call-rings"
mkdir -p "$out/group-logic-src" && cp Tests/GroupLogicTests.swift "$out/group-logic-src/main.swift"
swiftc -O -o "$out/group-logic" "$out/group-logic-src/main.swift" Shared/API/APIError.swift \
  Shared/Groups/GroupModels.swift Shared/Groups/GroupText.swift Shared/Groups/GroupVault.swift
"$out/group-logic"
swiftc -O -parse-as-library -o "$out/calendar" Tests/CalendarTests.swift \
  Shared/Calendar/CalendarModels.swift Shared/Calendar/CalendarTime.swift
"$out/calendar"
swiftc -O -parse-as-library -o "$out/calendar-notices" Tests/CalendarNoticeTests.swift \
  Shared/Calendar/CalendarModels.swift Shared/Calendar/CalendarNotice.swift
"$out/calendar-notices"
swiftc -O -parse-as-library -o "$out/calendar-links" Tests/CalendarLinkTests.swift \
  Shared/Calendar/CalendarLink.swift Shared/Calendar/CalendarMeetingModels.swift
"$out/calendar-links"
swiftc -O -parse-as-library -o "$out/calendar-meeting-lookup" Tests/CalendarMeetingLookupTests.swift Shared/Config.swift \
  Shared/API/Models.swift Shared/API/APIError.swift Shared/API/PostboxClient.swift \
  Shared/Calendar/CalendarLink.swift Shared/Calendar/CalendarMeetingModels.swift Shared/Calendar/CalendarMeetingLookup.swift
"$out/calendar-meeting-lookup"
swiftc -O -parse-as-library -o "$out/calendar-draft" Tests/CalendarDraftTests.swift Shared/API/APIError.swift \
  Shared/Calendar/CalendarModels.swift Shared/Calendar/CalendarTime.swift Shared/Calendar/CalendarDraft.swift
"$out/calendar-draft"
swiftc -O -parse-as-library -o "$out/calendar-import" Tests/CalendarImportTests.swift \
  Shared/Calendar/CalendarModels.swift Shared/Calendar/CalendarImport.swift
"$out/calendar-import"
swiftc -O -parse-as-library -o "$out/calendar-api" Tests/CalendarAPITests.swift Shared/Config.swift \
  Shared/API/Models.swift Shared/API/APIError.swift Shared/API/PostboxClient.swift \
  Shared/Calendar/CalendarModels.swift Shared/Calendar/CalendarAPI.swift
"$out/calendar-api"
swiftc -O -parse-as-library -o "$out/calendar-store" Tests/CalendarStoreTests.swift Shared/Config.swift \
  Shared/API/Models.swift Shared/API/APIError.swift Shared/API/PostboxClient.swift \
  Shared/Calendar/CalendarModels.swift Shared/Calendar/CalendarTime.swift \
  Shared/Calendar/CalendarAPI.swift Shared/Calendar/CalendarStore.swift
"$out/calendar-store"
swiftc -O -o "$out/phase1b" Tests/phase1b/main.swift \
  Shared/API/Models.swift Shared/API/Phase1bModels.swift Shared/Model/Blocking.swift Shared/Model/AvatarCrop.swift \
  Shared/Model/Conversation.swift Shared/Design/PeerFace.swift Shared/Design/Theme.swift Shared/Design/Time.swift \
  Shared/Views/Markdown.swift Shared/Views/SelectableText.swift
"$out/phase1b"
