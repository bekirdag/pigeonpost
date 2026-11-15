# Widgets — plan (2026-11-15)

Home Screen and Lock Screen widgets for the Pigeonpost mobile apps. This file is shared: the
**Shared spec** binds every platform, the **Apple** section is the iPhone and iPad implementation,
and the **Android** section belongs to the Android work.

## Shared spec (all platforms)

### Widgets

| Widget | Sizes | Shows | Tap |
| --- | --- | --- | --- |
| Inbox | small, medium, large; extra-large on iPad | Unread count, latest conversations: avatar initials, name or handle, time, unread dot | A row opens that conversation; anywhere else opens the inbox |
| Quick actions | small | New message, Inbox, Search | Each action opens its screen |
| Nests | small, medium | The account's Nests: name, status, running-hours meter | Opens the Nests page; empty state links to "Get a Nest" |
| Agenda | medium, large | What is left today and the next upcoming event | An event opens it; anywhere else opens the calendar |
| Unread (Lock Screen, circular) | accessory | Unread count | Opens the inbox |
| Latest (Lock Screen, rectangular) | accessory | Latest conversation | Opens that conversation |

Layout follows the design kit's widget anatomy (`mobile/widgets.html`, `.pp-widget`): a 42pt
number, a muted label, 32pt avatars in rows, one accent line, `--pp-surface` behind, Geist type,
light and dark from the kit tokens. The brand gradient is reserved for calls and is not used here.

### Privacy rules

1. Conversations are end-to-end encrypted, so **message text is off by default**. An unread row
   says "New message" (or "N new messages"); a read row shows the handle.
2. One in-app setting, **"Show message previews in widgets"**, default off. Signing out resets it.
3. Redaction is enforced in the snapshot codec on the way out **and** on the way in: a snapshot with
   `showsPreviews = false` never carries preview text, whoever built it. A preview is one line, at
   most 140 characters, and marked privacy-sensitive so a locked Lock Screen hides it.
4. The widget process holds **no token, key or crypto state and makes no network call**. It reads
   one file the app wrote.
5. The snapshot lives in storage only the app and its widget code can reach, readable after the
   first unlock and never synced or backed up off the device (iOS: a shared keychain item,
   `AfterFirstUnlockThisDeviceOnly`; see the Apple section for why not an App Group).
6. **Active account and acting mailbox only.** The snapshot is removed on sign-out, on session
   expiry, and as soon as a different account is signed in (owner mismatch), before anything is drawn.

### Snapshot (version 1)

JSON, written by the app only. Readers refuse a version newer than their own.

| Field | Type | Notes |
| --- | --- | --- |
| `version` | int | 1 |
| `generatedAt` | epoch seconds | When the app wrote it |
| `owner` | string | First 12 bytes, hex, of SHA-256(`pigeonpost-widget-owner:` + OIDC subject). Never the subject |
| `mailbox` | `{address, title}` | Acting mailbox; `title` is the handle or label |
| `showsPreviews` | bool | The setting above |
| `unreadCount` | int | Unread in non-archived conversations (the badge count) |
| `conversations` | ≤ 8 × `{peer, name, kind, lastAt, unread, preview?}` | `kind` is `person` or `agent` (an own mailbox); newest first; archived and blocked left out |
| `nests` | `{available, items}` or null | null = not loaded yet; `available = false` = the account has no Nests capability. ≤ 6 items `{id, name, tone, status, runningHours?, hoursLimit?}`; `tone` ∈ ready, working, attention, paused, setup, failed, unknown; `status` is the app's own words |
| `agenda` | `{events}` or null | null = calendar off or not loaded. ≤ 8 events not yet ended `{eventId, occurrenceId?, title, startsAt, endsAt, allDay, location?, hasMeeting}` |

### Deep links

Custom scheme `dev.pigeonpost.inbox`, host `widget` (not a universal link: a widget opens this app
and nothing else). Anything else is not a widget link.

| Route | URL | Opens |
| --- | --- | --- |
| inbox | `/inbox` | The inbox (archive closed, nothing selected) |
| conversation | `/conversation?mailbox=…&peer=…` | That conversation, in that mailbox. Switches mailbox when the account owns it; otherwise the inbox |
| compose | `/compose` | New message |
| search | `/search` | The inbox with search open |
| nests | `/nests`, `/get-nest` | The Products / Nests page |
| calendar | `/calendar` | The calendar |
| event | `/event?mailbox=…&event=…&occurrence=…` | That event (if the mailbox is the acting one) |

### Refresh and lifecycle

- The app rewrites the snapshot when the inbox listing, the Nests, the calendar or the preview setting
  change, **only when its content changed**, then asks the system to reload widgets.
- The Agenda timeline also redraws at each event's start and end, and after midnight.
- v1 has no background refresh: while the app is closed, counts can be behind until the app next runs.
  A notification-service-extension update is the natural follow-up (it would need the same redaction).

## Apple (iPhone and iPad)

### Shape

| Piece | Where |
| --- | --- |
| Widget extension target `PigeonpostWidgets` (`dev.pigeonpost.inbox.widgets`), embedded in the iOS app only | `apps/ios/PigeonpostWidgets/` |
| Snapshot model, codec, keychain store, links, fixtures and the widget views — compiled into the app **and** the extension | `apps/ios/WidgetShared/` |
| Writer (`WidgetSync`), builder (pure, tested), router, debug gallery — app only | `apps/ios/Pigeonpost/Widgets/` |
| Setting "Show message previews in widgets" | `Shared/Views/SettingsSheet.swift`, `#if os(iOS)` |
| Route handling | `Pigeonpost/PigeonpostApp.swift` (`onOpenURL`), `Pigeonpost/Views/ConversationsView.swift` |

The Mac target gains nothing: it does not compile `WidgetShared`, does not embed the extension, and
`PigeonpostMac.entitlements` is unchanged (no widget keychain group).

- iOS 17 deployment target, `StaticConfiguration`s, `containerBackground` with the kit surface.
- iOS 18 tinted and clear Home Screens: the background drops out outside `.fullColor`, and the parts
  that should carry the tint (unread number, dots, meters, icons) are `widgetAccentable()`.
- Small widgets use several `Link`s (supported in `systemSmall` on iOS 17+) with a `widgetURL`
  fallback to the inbox.
- Fonts: the extension bundles Geist (`UIAppFonts`) and the kit also registers them at runtime.
- Running-hours meter: from each Nest's usage as the app last read it (`NestUsageLedger`, filled by the
  Nest page); without it the widget shows the package's monthly hours.

### Data flow

`WidgetSnapshots` (a view modifier on the app root) derives a `WidgetSync.State` from the session,
account, inbox, Nests and calendar; `WidgetSync.apply` writes, clears or does nothing. Signed out →
clear. Signed in while the mailbox loads → keep, unless the stored owner is somebody else → clear.
Loaded → write if changed. `Account.forgetLocal` also calls `onForget`, which clears the file and
turns previews back off.

### Signing and portal

- Bundle id `dev.pigeonpost.inbox.widgets`.
- **Transport: a shared keychain group, not an App Group.** App Groups cannot be created or assigned
  to an App ID through the App Store Connect API (its capability settings accept only iCloud, data
  protection and Sign in with Apple keys, and Xcode's API-key provisioning cannot register one either).
  A team-prefixed keychain access group needs no portal object: every profile already grants
  `AH277897AV.*`. The snapshot is one generic-password item in `AH277897AV.dev.pigeonpost.widgets`,
  `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` (the keychain's
  `completeUntilFirstUserAuthentication`), not synchronizable. The app lists its own default group
  first (`…dev.pigeonpost.inbox`, the application identifier it always used), so its tokens do not move;
  the extension lists only the widget group and cannot read them.
- `APP_GROUPS` is switched on for both App IDs (no group assigned), so moving to
  `group.dev.pigeonpost.inbox` later needs only the group created in the developer portal web UI.
- Profiles: "Pigeonpost Inbox App Store" (reissued, same name, cert `Y3Q3ZTYQ5N`) and "Pigeonpost
  Widgets App Store". Secrets `APPLE_PROFILE` and `APPLE_WIDGETS_PROFILE`.
- `ios-testflight.yml` installs both, checks each grants the team keychain groups, signs both Release configs
  manually, checks the extension's version and build match the app's, and exports with both profiles.

### Tests

- `Tests/WidgetSnapshotTests.swift` (in `Tests/run.sh`): redaction in the builder and in the codec
  (including a tampered file), round trip, version and garbage refusal, minimal key set, owner
  fingerprint, limits, the store, every deep link, agenda and Nest mapping.
- `UITests/PPUITests/WidgetGalleryTests.swift`: the debug gallery (`-widget-gallery`) renders every
  face from the fixtures in light and dark, hides message text with previews off, shows it with them
  on, and `/compose` opens the composer. Its screenshots are the ones in the pull request.

### Not in v1

- No background refresh (see above). No group conversations in the Inbox widget. No configurable
  (intent) widgets. No Control Center controls.

## Android

Owned by the Android work; to be filled in there against the shared spec above.
