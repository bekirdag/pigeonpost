# Android home-screen widgets

Four Jetpack Glance widgets for the Android app (`apps/android`): **Inbox**, **Quick actions**,
**Nests** and **Agenda**, on phones and tablets, in the design kit's light and dark colours. They
draw a small snapshot that the app writes, and nothing else: no tokens, no keys, no crypto state,
no network.

Shared plan: `docs/widgets-plan.md` (iOS and the cross-platform rules). This file is the Android
half; where the two differ, the difference is stated here.

## 1. Scope

| In | Out (see §10) |
| --- | --- |
| Inbox, Quick actions, Nests, Agenda widgets | Lock-screen (keyguard) widgets |
| Resizable layouts from 1x1/2x1 to tablet sizes | Choosing a mailbox per widget |
| "Show message previews in widgets" setting, off by default | Geist in widgets (RemoteViews cannot load bundled fonts) |
| Picker previews: static (Android 8-14) and generated (Android 15+) | Any fetch made for a widget alone |

Library: `androidx.glance:glance-appwidget` 1.2.0, added through the new version catalog
`apps/android/gradle/libs.versions.toml` (`glance-appwidget`, plus `glance-appwidget-testing` and
`glance-testing` for tests). Other dependencies stay inline until they are next touched.

## 2. The four widgets

| Widget | Default | Sizes (dp, Glance `SizeMode.Responsive`) | Shows | Taps |
| --- | --- | --- | --- | --- |
| Inbox | 4x2 | 110x48, 250x48, 110x110, 250x110, 250x250, 400x300 | Unread count, latest conversations and groups | Row: that conversation or group. Elsewhere: the inbox. Pencil: new message |
| Quick actions | 2x1 | 48x48, 110x48 | New message; Inbox with an unread dot | New message sheet; inbox |
| Nests | 4x2 | 110x110, 250x110, 250x250, 400x300 | Each Nest: name, status, running hours | Row: that Nest. Elsewhere / "Get a Nest": Products |
| Agenda | 4x2 | 110x110, 250x110, 250x250, 400x300 | Today and the next 14 days | Event: that event. Elsewhere: the calendar |

### Inbox

- **2x1 strip**: count and "unread". **4x1 strip**: also the latest unread conversation and a New
  message button.
- **2x2**: header, then compact rows: 24 dp initials, name, unread dot.
- **4x2 / 4x4 / tablet**: header with the unread pill and New message; rows with initials on the
  peer's kit tone (36 dp, 40 dp on tablets), name or handle, time, second line, unread dot.
- Second line: the message's first line **only if previews are on**. Otherwise "New message" /
  "N new messages" when unread, the handle when read ("No new messages" for `/k/` addresses,
  "Group" for groups). Group text is never shown: it is only ever decrypted inside the app.
- States: signed out ("Sign in to Pigeonpost"), locked (counts only), empty ("No conversations yet").

### Quick actions

1x1 is New message. 2x1 adds Inbox with an unread dot. Signed out, both open the app (sign-in).
New message opens the inbox's new-conversation sheet (`InboxViewModel.composeRequested`).

### Nests

Each Nest's name, status and a kit-coloured dot, with the app's own labels (`nestStatusLabel`:
Ready, Working…, Needs you, Paused, Unavailable, Setting up…). Running hours show only what the app
has read:

- "46 h of 120 h" and a meter, when the Nest's usage (`running_secs`) was read and the package's
  pooled `active_hours` is known. Usage is fetched only when a Nest is opened in the app, so it is
  kept from the last time it was opened.
- "46 h this month" or "120 h a month" when only one of the two is known.

Empty: "No Nests yet" and **Get a Nest**, which opens Products in the app (Play billing lives
there). Nests off for the account: "Nests are not available". Not loaded yet: "Open Pigeonpost".

### Agenda

Today's date as the header; events grouped Today, Tomorrow, then "Mon 17 Nov". Each event: an
accent bar, title, time range or "All day", location (not at 2x2), a video icon for a Pigeonpost
meeting. Next 14 days, at most 12 events; cancelled and declined events are dropped.

The widget redraws from the stored snapshot every 30 minutes (`updatePeriodMillis`), so "Today"
turns over and past events drop off without any fetch.

## 3. The snapshot

`core/src/main/kotlin/dev/pigeonpost/core/widgets/WidgetSnapshot.kt`: plain Kotlin, JVM-tested.

| Field | Content | Limit |
| --- | --- | --- |
| `version` | `1`. Any other version decodes to nothing. | |
| `mailbox` | The acting mailbox address | must be a valid address |
| `generated_at` | Epoch seconds | |
| `locked`, `previews` | Redaction flags | |
| `unread` | Unread across direct conversations and groups | |
| `conversations` | `peer`, `name`, `at`, `unread`, `preview` (null unless previews on), `group` | 12 rows; name 64, preview 120 chars |
| `nests` | `enabled`, `nests` (`id`, `label`, `status`, `status_reason`, `running_secs`), `count`, `hours_limit` | 8 Nests |
| `agenda` | `enabled`, `zone`, `events` (`event_id`, `occurrence_id`, `title`, times or dates, `location`, `meeting`), `count` | 12 events; title 80 chars |
| `push_ids` | Push message ids already counted | last 32 |

Rules (`WidgetSnapshots`):

- `build` applies the limits, drops invalid peers, strips control characters, and only fills
  `preview` for direct conversations when the setting is on.
- `decode` rejects other versions, unreadable JSON and invalid mailboxes, then sanitises again:
  limits, previews dropped unless `previews` is true, locked snapshots redacted again.
- `redact` keeps counts only: conversations emptied, Nest and event lists emptied with their counts
  kept.
- No token, key, crypto state, URL or authorization id is ever in it.

Storage (`WidgetStore`): one file, `noBackupFilesDir/widgets/snapshot.enc`, sealed with
AES-256-GCM under a non-exportable Android Keystore key (`pigeonpost.widgets.v1`): format byte,
12-byte IV, ciphertext, fixed AAD, written with `AtomicFile`. This is the pattern the app already
uses for group state (`KeystoreGroupStorage`). A copied file is unreadable on any other device.

Widgets run in the app's process and read the snapshot through a process-wide `StateFlow`
(`WidgetStore.snapshots`): an open widget session redraws on every write, and `updateAll` wakes the
others.

## 4. Data flow

```
stores the app already has ──▶ WidgetSync (InboxViewModel) ──▶ WidgetStore (sealed file) ──▶ widgets
 session, inbox, groups,        build + coalesce 400 ms          + updateAll                 draw only
 nests, calendar, people
 + previews setting
FCM message push (app closed) ──▶ WidgetPush.bump ─────────────┘
```

- **Writer**: `WidgetSync`, started by `InboxViewModel`. It writes only when the content changed.
- **Only data the app already fetched.** A section not loaded yet in this session (Nests, Agenda)
  keeps the previous snapshot's section for the same mailbox, so widgets do not flash empty while
  the app starts.
- **Calendar**: the app holds one month window; only the window that contains today feeds the
  Agenda, and only after a refresh has finished. The calendar normally loads only when its pane is
  open, so when an Agenda widget is placed the app makes one ordinary calendar refresh per calendar
  context. The widget itself still fetches nothing.
- **Push while closed**: `displayPush` hands the push's locators (mailbox, peer, message id, a
  group's name) to `WidgetPush`. The Inbox widget counts it and moves that conversation up, as
  "New message", because pushes carry no text. A push for another mailbox, or an id already
  counted, changes nothing. When the app is in front it rewrites the snapshot itself.
- **Sign-out**: `InboxViewModel.signOut` clears at once (file, Keystore key, in-memory copy) and
  turns previews back off. The sync also clears whenever the session is signed out or terms are not
  accepted.
- **Multi-account and multi-mailbox**: the snapshot belongs to the acting mailbox only. Switching
  mailbox rebuilds it, and nothing from another mailbox is carried over. Taps and push updates name
  the snapshot's mailbox, and the app checks that mailbox belongs to the signed-in account before
  it navigates.
- **Taps** reuse the notification extras (`push_identity` + `push_peer`, or `push_type=calendar`
  with event ids), so they go through `openNotification` / `openCalendarNotice` validation. The
  rest use `widget_target` (`inbox`, `compose`, `nests`, `nest`) handled by
  `InboxViewModel.openWidgetTarget`. All are explicit, immutable intents with
  `CLEAR_TOP | SINGLE_TOP`.

## 5. Privacy rules

- **Previews off by default.** Settings > Inbox and storage > Home screen widgets > "Show message
  previews in widgets": "Off: widgets show who wrote and “New message”, never what was written."
- **App lock**: Android Pigeonpost has no PIN or app lock today. The snapshot has a tested locked
  mode (counts only); `WidgetSync` passes `locked = false` with a comment where a future lock's
  state goes.
- **Lock screen**: widgets are `home_screen` only, never `keyguard`.
- **Backups**: the manifest already sets `allowBackup="false"` and `data_extraction_rules` exclude
  every domain; the snapshot is also in no-backup storage and sealed with a device-bound key.
- **No network** from widget code, and no new permission.

## 6. Design

- Colours are the design kit's tokens (`PPLight` / `PPDark`: surface, text, muted, link, accent,
  onAccent, accentSubtle, success, info, warning, danger) as Glance day/night colours, resolved by
  the launcher's night mode. No Material You dynamic colour.
- Initials use `PeerFace.tone` and `ppToneColors`, so a peer has the same face in the app, the web
  and the widget.
- Type: the kit sizes (`PPType` h4 16, body 15, bodyS 13.5, caption 12, overline 11.5). RemoteViews
  cannot load bundled fonts, so widgets use the system sans serif, not Geist.
- Card radius 24 dp (`PPRadius.x2xl`). Lucide icons from the kit (`pp_ic_*`). Shapes are tinted
  drawables, so corners and circles look the same from Android 8 to 15.
- TalkBack: every row has a description ("Maya Okafor, 2 new messages"), as do the icon buttons.

## 7. Widget picker

- `android:description` and a label for each widget.
- Android 12-14: `previewLayout` (static XML with kit colours, light and night).
- Android 8-11: `previewImage`, PNGs taken from the render test.
- Android 15+: generated previews from `GlanceAppWidget.providePreview` with made-up
  `WidgetSamples` data, published once per `versionCode` by `WidgetPreviews.publish`
  (rate-limited by the system; retried on the next launch).

## 8. Tests

- `core`: `WidgetSnapshotTest`, 23 JVM tests. Previews off by default; the first line and the
  120-character cut; groups never previewed; encode/decode round trip; other versions and garbage
  rejected; locked redaction, also when read back; previews dropped on read when off; ordering and
  cap; invalid peers; push rules; agenda filtering, sorting and all-day events; mailbox switch.
- `app` JVM: `WidgetLayoutTest`, 26 Glance unit tests (`runGlanceAppWidgetUnitTest`, no
  Robolectric). Each widget at its sizes in signed-out, locked, empty and full states, TalkBack
  descriptions, and no message text unless previews are on. The Android workflow's phone leg now
  runs `:app:testDebugUnitTest`.
- Instrumented: `WidgetRenderTest.everyWidgetRendersAtEverySizeInBothThemes` composes each widget
  and inflates its RemoteViews in an `AppWidgetHostView` on the device, in light and dark, for the
  full, empty and signed-out states. It runs on the phone and tablet CI emulators.
- Screenshots (opt-in), on a phone or tablet emulator. The current boards are in
  `docs/widgets-android/`:

  ```
  ./gradlew :app:assembleDebug :app:assembleDebugAndroidTest
  adb install -r -t app/build/outputs/apk/debug/app-debug.apk
  adb install -r -t app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk
  adb shell am instrument -w -e class dev.pigeonpost.inbox.WidgetRenderTest#capture -e widgetShots <name> \
    dev.pigeonpost.inbox.debug.test/dev.pigeonpost.inbox.PigeonpostTestRunner
  adb pull /sdcard/Android/data/dev.pigeonpost.inbox.debug/files/widgets/<name>
  ```

## 9. Play listing

- **Data safety: unchanged.** Nothing new is collected or shared, nothing leaves the device, and
  there is no new permission. The widget snapshot stays on the device, sealed, out of backups.
- Screenshots: optional widget shots for the phone and tablet galleries (the `capture` boards).
- Release notes: "Home-screen widgets: Inbox, Quick actions, Nests and Agenda."

## 10. Not in this release

- Lock-screen widgets.
- Choosing the mailbox per widget (a configuration activity).
- Group activity while the app is closed beyond push counts.
- Nest running hours for Nests never opened in the app.
- Calendar events beyond the month window the app holds.
- Geist in widgets.
