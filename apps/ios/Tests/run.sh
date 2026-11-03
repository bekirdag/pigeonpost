#!/bin/sh
# The thread model, compiled for the mac it is run on — no simulator, no project, no XCTest.
# Everything under test is a pure function over its inputs, and keeping it runnable in one command
# is what makes it get run.
set -e
cd "$(dirname "$0")/.."
out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
swiftc -O -parse-as-library -o "$out/authorization-lease" Tests/AuthorizationLeaseTests.swift Shared/Auth/AuthorizationLease.swift
"$out/authorization-lease"
swiftc -O -parse-as-library -o "$out/account-scope" Tests/AccountScopeTests.swift Shared/Config.swift \
  Shared/API/Models.swift Shared/API/APIError.swift Shared/API/PostboxClient.swift \
  Shared/Auth/AuthorizationLease.swift Shared/Model/Account.swift Shared/Model/Features.swift
"$out/account-scope"
swiftc -O -parse-as-library -o "$out/group-store-scope" Tests/GroupStoreScopeTests.swift Shared/Config.swift \
  Shared/API/Models.swift Shared/API/APIError.swift Shared/API/PostboxClient.swift Shared/Auth/AuthorizationLease.swift \
  Shared/Model/Account.swift Shared/Model/Features.swift Shared/Groups/GroupModels.swift Shared/Groups/GroupAPI.swift \
  Shared/Groups/GroupVault.swift Shared/Groups/GroupEngine.swift Shared/Groups/GroupText.swift Shared/Groups/GroupStreams.swift Shared/Groups/GroupStore.swift
"$out/group-store-scope"
swiftc -O -DDEBUG -parse-as-library -o "$out/people" Tests/PeopleTests.swift Shared/Config.swift \
  Shared/API/Models.swift Shared/API/APIError.swift Shared/API/PostboxClient.swift \
  Shared/People/PeopleModels.swift Shared/People/PeopleAPI.swift Shared/People/PeopleStore.swift Shared/People/PeopleFixtures.swift Shared/People/PeopleFiles.swift
"$out/people"
swiftc -O -o "$out/auth" Tests/AuthTests.swift Shared/Config.swift
"$out/auth"
swiftc -O -DDEBUG -parse-as-library -o "$out/session-lifetime" Tests/SessionLifetimeTests.swift Shared/Config.swift \
  Shared/API/APIError.swift Shared/Auth/AuthorizationLease.swift Shared/Auth/StrongSignIn.swift Shared/Auth/Session.swift
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
swiftc -O -parse-as-library -o "$out/organization-rooms" Tests/OrganizationRoomTests.swift Shared/Config.swift \
  Shared/API/Models.swift Shared/API/APIError.swift Shared/API/PostboxClient.swift \
  Shared/Groups/GroupModels.swift Shared/Groups/GroupAPI.swift \
  Shared/Enterprise/OrganizationRoomModels.swift Shared/Enterprise/OrganizationRoomAPI.swift
"$out/organization-rooms"
swiftc -O -parse-as-library -o "$out/organization-room-store" Tests/OrganizationRoomStoreTests.swift Shared/Config.swift \
  Shared/API/Models.swift Shared/API/APIError.swift Shared/API/PostboxClient.swift \
  Shared/Groups/GroupModels.swift Shared/Groups/GroupAPI.swift \
  Shared/Enterprise/OrganizationRoomModels.swift Shared/Enterprise/OrganizationRoomAPI.swift Shared/Enterprise/OrganizationRoomStore.swift
"$out/organization-room-store"
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
  Shared/Auth/AuthorizationLease.swift \
  Shared/API/Models.swift Shared/API/APIError.swift Shared/API/PostboxClient.swift \
  Shared/Model/Inbox.swift Shared/Model/Conversation.swift \
  Shared/Design/PeerFace.swift Shared/Design/Theme.swift Shared/Views/Markdown.swift Shared/Views/SelectableText.swift
"$out/inbox-loading"
swiftc -O -o "$out/ios-navigation" Tests/IOSNavigationTests.swift Shared/API/Models.swift \
  Pigeonpost/Views/MailboxOrder.swift Pigeonpost/Views/HistoryWindow.swift
"$out/ios-navigation"
swiftc -O -parse-as-library -o "$out/streams-notes" Tests/StreamNotesTests.swift Shared/Config.swift \
  Shared/API/Models.swift Shared/API/APIError.swift Shared/API/PostboxClient.swift Shared/Groups/GroupModels.swift \
  Shared/Groups/GroupAPI.swift Shared/Groups/GroupStreams.swift Shared/Meetings/MeetingNotesModels.swift Shared/Meetings/MeetingNotesStore.swift
"$out/streams-notes"
swiftc -O -parse-as-library -o "$out/call-rings" Tests/CallRingTests.swift Shared/Calls/CallRing.swift
"$out/call-rings"
mkdir -p "$out/group-logic-src" && cp Tests/GroupLogicTests.swift "$out/group-logic-src/main.swift"
swiftc -O -o "$out/group-logic" "$out/group-logic-src/main.swift" Shared/API/APIError.swift \
  Shared/Auth/AuthorizationLease.swift Shared/Groups/GroupModels.swift Shared/Groups/GroupText.swift Shared/Groups/GroupVault.swift
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
