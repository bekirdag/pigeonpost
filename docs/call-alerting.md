# Incoming-call alerting on every device (Phase 1c)

Before this, a call rang only on devices whose app (or inbox tab) was open: the calls service rings
the event-stream sessions it holds, and nothing else. This document describes how a call now reaches
a phone on the lock screen, a Mac with the app closed, a Windows tray, a Linux desktop and a hidden
browser tab, and what each one does with it.

Everything here is behind two switches, both off by default:

| Switch | Where | Effect |
| --- | --- | --- |
| `CALLS_RING_PUSH=1` | calls service env (`/etc/pigeonpost-calls/calls.env`) | the calls service asks the postbox to push rings and ring ends |
| feature flag `call_push` | postbox (`PUT /internal/v1/features/call_push`, global or per account) | the postbox actually pushes; off = `{"decision":"disabled"}` and no push |

## 1. The flow

```
caller ──POST /calls/v1/calls──► calls service ── ring() ──► open sessions (SSE "incoming")
                                     │
                                     ├─POST /internal/v1/call-ring ─────► postbox ──► APNs VoIP (iPhone, iPad)
                                     │   (svc token, private tunnel)              ──► APNs alert (Mac, older iOS builds)
                                     │                                            ──► FCM data (Android)
                                     │                                            ──► WNS toast (Windows, when configured)
                                     │
   ring ends (answered, declined, ──►├─POST /internal/v1/call-ring-cancel ─► postbox ──► FCM "call_cancel",
   timeout, caller hung up)          │                                               APNs alert replacement (Mac),
                                     │                                               WNS toast removal
device woken by a push ──────────────┤
   GET  /calls/v1/rings/{room}       │  ring state, by ring token (no member token needed)
   POST /calls/v1/rings/{room}/decline  decline, by ring token
   page opens /calls/v1/events ──────┘  a late session of the callee joins the ring ("incoming")
   POST /calls/v1/rooms/{room}/accept {session, ring_token}
```

The calls service only pushes when it would ring: after admission (Phase 1b) says `ring`, and only
when the callee is not already in a call (busy looks like no answer, as before). The postbox
re-checks the callee's own block list and inbox policy before pushing anything, so a blocked or
filtered caller never reaches a lock screen even if the calls service were wrong.

## 2. The ring token

One per ring: 32 random bytes, base64url (43 characters), minted by the calls service when it rings,
held there only as a SHA-256, and valid until the ring ends (answer, decline, timeout, caller
cancel). It travels in the push payload so a woken device can act on *this* ring without racing a
new event-stream session into the ringing set.

What it allows, and nothing more:

- `GET /calls/v1/rings/{room}` with header `x-ring-token: <token>` → `{"state":"ringing","kind",
  "expires_at"}` or `{"state":"ended","reason"}`. Unknown room and wrong token are the same 404
  (`no_ring`).
- `POST /calls/v1/rings/{room}/decline` `{"ring_token", "reason"?: "declined"|"busy"}` → `{"ok":true}`.
  Declining from a lock screen must work with an expired member token, so the token is the
  credential here. Anyone holding it could also only decline this one ring.
- Accepting still needs the member: `POST /calls/v1/rooms/{room}/accept` `{"session","ring_token"}`
  from an event-stream session whose account owns the called mailbox. With a valid ring token the
  session need not have been rung itself. The first accept wins; the token dies with the ring.

The token is not a secret from Apple, Google or Microsoft, whose servers carry the push. The worst
any of them could do with it is decline one call.

## 3. Calls service

- `ring()` mints the token, records `expires_at` (now + `CALLS_RING_MS`, 45 s), and calls
  `POST {POSTBOX_INTERNAL_URL}/internal/v1/call-ring` with `POSTBOX_SERVICE_TOKEN` — fire and forget,
  3 s timeout, no retries (a ring is useless once it is late; the expiry is the retry policy).
- When the ring stops for any reason it calls `/internal/v1/call-ring-cancel` with the reason:
  `answered_elsewhere`, `declined`, `busy`, `no_answer`, `cancelled`, `ended`.
- A new event-stream session whose mailboxes include the called mailbox, opened while the ring is
  live, is added to the ring and sent `incoming` — that is how an app launched by a push (or a
  second tab) rings.
- Code: `calls-service/src/ring-alerts.js` (token, postbox client), hooks in `server.js`.

## 4. Postbox

### Endpoints (internal listener only; `calls` service token)

`POST /internal/v1/call-ring`

```json
{ "room_id": "…22 chars…", "code": "ABC234", "kind": "audio|video", "ring_token": "…43…",
  "from": { "address": "/k/…", "handle": "/alp", "display": "Alp" },
  "to": "/bekir or /k/…", "expires_at": 1792500045 }
```

→ `{"decision": "ring"|"silence"|"block"|"disabled"|"unknown", "devices": n}`. The caller's handle is
looked up by the postbox rather than trusted; `display` is cut to 60 characters and stripped of
control and bidi characters.

`POST /internal/v1/call-ring-cancel`

```json
{ "room_id": "…", "to": "/bekir", "reason": "answered_elsewhere", "kind": "video",
  "from": { "address": "/k/…", "handle": "/alp", "display": "Alp" } }
```

→ `{"devices": n}`.

### Who is woken

The called mailbox is resolved (handle or address) to a hosted identity. Its devices are every row
in `devices` registered for that mailbox **or** for the account that owns it — the calls service
already rings every open session of the owning account, and a phone reading another of the owner's
mailboxes should ring too.

| Device row | Ring | Ring end |
| --- | --- | --- |
| `apns`, `kind=voip` | VoIP push, topic `<bundle>.voip`, priority 10, `apns-expiration` = ring end | nothing (the phone polls the ring state; see §5) |
| `apns`, `kind=alert`, `os=macos` | alert push, category `CALL_RING`, time-sensitive, `apns-collapse-id` = `call-<room>` | alert with the same collapse id, passive: "Missed call" / "Answered on another device" |
| `apns`, `kind=alert`, `os` unset (builds before 1c) | the same alert (tap opens the app, which then rings in-app) | the same replacement |
| `apns`, `kind=alert`, `os=ios`/`ipados` | nothing — that build rings through its VoIP token | nothing |
| `fcm` | data message `type=call_ring`, priority high, TTL = seconds left in the ring, `collapse_key` per room | data message `type=call_cancel` |
| `wns` | `wns/toast` with `scenario="incomingCall"`, tag per room, group `calls`, TTL | `DELETE` matching the tag and group |

Message pushes (`push::fan_out`) only ever go to `kind=alert` rows on `apns`/`fcm`, so a VoIP token
can never receive an alert and a WNS channel never receives mail notices.

### Filtering

No push is sent when:

- the callee's contact row for the caller (by handle, else address) has `admission=block`;
- the caller is a stranger to the callee and the callee's inbox policy does not accept strangers
  (`accept_all=false`) — the calls-service admission (Phase 1b, §13.3 of the SDS) decides first and
  this is the postbox's own last check;
- `call_push` is off for the callee's account (account value overrides global).

### Payloads

Nothing beyond what a ringing screen needs. No message text, no member token, no device list.

APNs VoIP (iPhone, iPad):

```json
{ "aps": {}, "type": "call_ring", "room": "…", "code": "ABC234", "kind": "video",
  "ring_token": "…", "from": { "address": "/k/…", "handle": "/alp", "display": "Alp" },
  "to": "/bekir", "identity": "/k/…callee address…", "expires_at": 1792500045 }
```

APNs alert (Mac): the same fields plus
`"aps": {"alert": {"title": "Alp", "body": "Incoming video call"}, "sound": "default",
"category": "CALL_RING", "interruption-level": "time-sensitive", "thread-id": "calls"}`.

FCM data (all values strings): `type, room, code, kind, ring_token, from_address, from_handle,
from_display, to, identity, expires_at`. Cancel: `type=call_cancel, room, reason, identity`. Builds
before 1c ignore both (they require `peer` and `message_id`).

WNS toast launch arguments: `action=answer|decline&room=…&ring=…&kind=…` (URL-encoded). The toast
XML is built with every value escaped.

### Device registration

`POST /v1/devices` gains two optional fields:

```json
{ "token": "…", "platform": "apns|fcm|wns", "kind": "alert|voip", "os": "ios|ipados|macos|android|windows",
  "environment": "production|sandbox", "identity": "/k/…" }
```

- `kind` defaults to `alert`; `voip` is only valid with `apns`.
- `os` is optional and informational except for the rule above (`ios`/`ipados` alert rows are not
  sent call alerts).
- A `wns` token is the channel URI, accepted only as `https://<host>.notify.windows.com/…` (at most
  2048 characters). Anything else is `invalid_device_token` — the postbox will only ever POST to
  Microsoft.

### Configuration

| Variable | Meaning |
| --- | --- |
| `PIGEONPOST_APNS_*` | unchanged; VoIP uses the same token-based key. Topic defaults to `PIGEONPOST_APNS_TOPIC` + `.voip` (`PIGEONPOST_APNS_VOIP_TOPIC` overrides). |
| `PIGEONPOST_FCM_SERVICE_ACCOUNT_FILE` | unchanged |
| `PIGEONPOST_WNS_TENANT_ID`, `PIGEONPOST_WNS_CLIENT_ID`, `PIGEONPOST_WNS_CLIENT_SECRET` (or `…_SECRET_FILE`) | WNS through the Microsoft identity platform (Entra client credentials, scope `https://wns.windows.com/.default`); unset = Windows rings in-app only |

## 5. Clients

### Bridge between the apps and the call page (`site-inbox/native-call.js`)

App → page:

| Call | Effect |
| --- | --- |
| `PigeonpostNative.configure({token, mailbox, known, nativeRing})` | `nativeRing: true` = the app shows incoming calls itself (CallKit, a notification); the page keeps the ring state but shows no ring screen and plays no ringtone |
| `PigeonpostNative.answer(room, ringToken)` | accept that ring as soon as the page has a session (with the token, even if the page never saw the ring) |
| `PigeonpostNative.decline(room, ringToken)` | decline it |
| `PigeonpostNative.setMuted(bool)`, `PigeonpostNative.hangUp()` | mute/unmute the microphone; leave the call |

Page → app (`{type: …}` on the existing channel):

| Message | When |
| --- | --- |
| `{type:"ring", room, code, kind, from:{address,handle}, to}` | a ring arrived on the event stream |
| `{type:"ring_ended", room, reason}` | it stopped ringing (answered here or elsewhere, declined, timed out, cancelled) |
| `{type:"call", state:"connecting"|"active"|"ended", room, kind}` | the call's own lifecycle |
| `{type:"muted", muted}` | the microphone was muted or unmuted in the page |

The web inbox also honours `https://inbox.pigeonpost.dev/?answer=<room>`: when a ring for that room
arrives, it is answered without another click (the Linux app opens this).

### iPhone and iPad — CallKit and PushKit

- `PKPushRegistry` (`.voIP`) registers at launch; its token goes to `/v1/devices` with
  `kind: voip, os: ios|ipados`. The alert token is registered with `os` too.
- Every VoIP push is reported to `CXProvider.reportNewIncomingCall` immediately, before anything
  else — iOS ends an app that does not, and stops delivering VoIP pushes to it. A push that is late
  (past `expires_at`) or for another account is still reported, then ended at once as unanswered.
- The system call screen shows the caller's display name and handle (lock screen, home screen,
  Recents with the app icon).
- Answer: the app makes sure the call page is loaded and signed in, then calls
  `PigeonpostNative.answer`. The audio session is configured for a call and activated by CallKit
  (`didActivate`); WebKit's capture starts after that.
- Decline: `POST /calls/v1/rings/{room}/decline` with the ring token (works without a member token).
- While the system screen rings, the app polls `GET /calls/v1/rings/{room}` every 2 s; an ended
  ring is reported with `reportCall(with:endedAt:reason:)` (`.answeredElsewhere`, `.declinedElsewhere`,
  `.remoteEnded`, `.unanswered`).
- Mute and end in the system UI map to `setMuted`/`hangUp`; mute and end in the page are reported
  back to CallKit.
- In the foreground the page runs with `nativeRing: true`, so a ring arriving over the event stream
  is reported to CallKit too; the room id is the dedupe key with the push.

### Mac

- An alert push with category `CALL_RING` (Answer, Decline) shows a time-sensitive banner when the
  app is closed. Answer opens the app into the call; Decline declines with the ring token.
- While the app runs, a ring on the event stream posts the same notification locally, with the same
  identifier as the push's collapse id, so the two replace rather than duplicate each other. When
  the window is frontmost the in-app ring screen is enough and no notification is posted.
- Remote push on the Mac needs the `com.apple.developer.aps-environment` entitlement, which only the
  Mac App Store build can carry. The Developer ID build (website download) has no provisioning
  profile for push and alerts only while it runs.

### Android

- FCM `call_ring` → a `NotificationCompat.CallStyle.forIncomingCall` notification on the `calls`
  channel (ringtone, vibration, `CATEGORY_CALL`), with a full-screen intent to an incoming-call
  activity that shows over the lock screen (`setShowWhenLocked`, `setTurnScreenOn`).
- `USE_FULL_SCREEN_INTENT`: on Android 14+ `canUseFullScreenIntent()` is checked and the person is
  sent to the system setting when it is off; the notification still rings without it.
- Answer opens the app's call view and calls `PigeonpostNative.answer`; Decline uses the ring-token
  decline; `call_cancel` (or the ring's expiry) dismisses it.
- In the foreground the in-app ring screen is used and no notification is posted.

### Windows

- While the app or its tray icon runs: a ring on the event stream shows a toast with
  `scenario="incomingCall"` (stays on screen, rings, Answer and Decline), bottom right.
- With the app closed: the same toast from WNS. Windows App SDK push: the app asks
  `PushNotificationManager` for a channel with the Entra app's object ID, and the postbox
  authenticates as that Entra app. Microsoft must first map the package family name to the Entra
  app (a mapping request sent to Win_App_SDK_Push@microsoft.com, handled weekly); until then the channel
  request fails, the app logs it and rings only while running. The package-SID flow against
  `login.live.com` is refused for new apps ("Client credential flows against login.live.com are no
  longer supported").

### Linux

- While the app runs, it listens on the calls event stream and shows a desktop notification with
  Answer and Decline. Answer opens `https://inbox.pigeonpost.dev/?answer=<room>` in the browser
  (WebKitGTK has no WebRTC); Decline declines from the app's own session.

### Web

- A ring while the inbox tab is hidden raises a browser notification (permission permitting) that
  stays until the ring ends; clicking it focuses the tab on the ring screen.

## 6. Verifying on real devices (owner)

See the Phase 1c section of `docs/planning/2026-10-20-big-features-implementation-progress.md` for
the exact steps; in short: install the TestFlight/internal builds, sign in on the phone, lock it,
call it from the web inbox, and check the lock-screen call screen, answer with audio both ways,
decline, caller hang-up while ringing, and a call while the app is force-quit.
