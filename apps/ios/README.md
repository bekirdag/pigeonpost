# Pigeonpost for iOS

`inbox.pigeonpost.dev` as an app. Same server, same words on screen — where this and the web app
disagree, `site-inbox/` is the specification.

```
Pigeonpost/
  Config.swift          endpoints and the OIDC client. No secret
  Auth/                 PKCE sign-in, token renewal, the Keychain
  API/                  the postbox's routes and response shapes
  Model/                the mailboxes, the thread model, the live inbox
  Design/               the palette, the avatar tone hash, the clock
  Views/                the screens
Tests/                  the thread model, run by hand — ./Tests/run.sh
```

SwiftUI, deployment target iOS 17.0, **no third-party dependencies** — URLSession,
ASWebAuthenticationSession, Keychain. Adding one is a decision to make on the record.

The Xcode project uses a file-system-synchronized group, so a new `.swift` file under `Pigeonpost/`
is in the target the moment it exists. `Info.plist` is the one exception, listed as such, because a
synchronized group would otherwise copy it into the bundle as a resource *and* process it as the
Info.plist, and the build fails with "Multiple commands produce".

## The realm client — done, 2026-08-20

The app has its own realm client — deliberately not the browser's `pigeonpost-web`, since a native
app redirects to a custom scheme rather than to an origin. `pigeonpost-mobile` **exists** in realm
`pigeonpost-prod` (wodomini, behind `auth.pigeonpost.dev`), built by `/bekir/su_iam` from
`scripts/configure-pigeonpost-realm.js` in the su_iam repo rather than by console clicks. It is
configured as:

1. New client `pigeonpost-mobile`, **public**, standard flow on, direct access grants off.
2. PKCE `S256`, required.
3. Valid redirect URI: `dev.pigeonpost.inbox://oauth2redirect`
4. Web origins: none. A native client makes no CORS-checked requests, which is also why
   `CORS_ORIGINS` on the postbox needs no change for this app.
5. `offline_access` among the optional scopes, as `pigeonpost-web` has it.

It also carries `post.logout.redirect.uris` set to the same URI — without it, RP-initiated logout
with a `post_logout_redirect_uri` is rejected as unregistered and the app cannot get back from the
logout page — and the `masaas-api-audience` and `tenant-id` (`pigeonpost`) mappers copied from
`pigeonpost-web`, for consistency with the web inbox.

Those two mappers are **not** a dependency of this app, whatever a Keycloak-side reading suggests:
the postbox does not validate `aud` at all (`crates/pigeonpost-postbox/src/oidc.rs:115`,
`validate_aud = false`, deliberately — "the postbox isn't the token's audience"). Worth knowing
before anyone explains a sign-in failure with them.

Verified against the live realm, unauthenticated: the exact URI with S256 gives the login page;
without `code_challenge_method` it refuses with `Missing parameter: code_challenge_method`; and
`dev.pigeonpost.inbox://evil` gives `Invalid parameter: redirect_uri`.

## Building

```
xcodebuild -project Pigeonpost.xcodeproj -scheme Pigeonpost \
  -destination 'platform=iOS Simulator,name=iPhone 15 Pro' build
```

**This machine cannot run it yet.** Xcode 16.2 carries the iOS 18.2 SDK, and the only simulator
runtimes installed are 17.4 and 17.5. `actool` refuses the mismatch outright — "No simulator runtime
version … available to use with iphonesimulator SDK version 22C146" — so the build fails at the
asset catalog even though every Swift file compiles. Swift errors still surface before that point,
so the code can be iterated on; producing or running the `.app` needs the runtime.

Fix: `xcodebuild -downloadPlatform iOS` (~7 GB). It needs free disk, and this machine had 5.2 GiB
free when this was written — `target/` alone was 74 GB.

## Looking at it without an account

```
xcrun simctl launch booted dev.pigeonpost.inbox -fixtures
xcrun simctl launch booted dev.pigeonpost.inbox -fixtures "-open=/bekir/main"
```

Debug builds only, and inert unless asked for by name. `-fixtures` loads the same response shapes
the thread-model test uses instead of signing in, and makes every write local — a screenshot of an
empty list proves nothing about the screens that matter, and the two things that gate a real session
(a realm client, a postbox account) are not always to hand.

`-open=` takes one token on purpose. As two (`-open /bekir/main`) a bare `-key value` pair is read
by UserDefaults as a default, and passing two of them was enough to stop `-fixtures` being seen at
all.

## Tests

```
./Tests/run.sh
```

`swiftc` over the pure model files and a fixture set copied from the web app's tests — no simulator,
no XCTest target, no project. Both clients assemble the same conversations from the same bytes, and
that is the property worth holding on to: a thread must read the same in the browser and on the
phone. It asserts what is quiet when it breaks — thread assembly and ordering, an own agent that has
never written staying out of the list, a scoped request reading as a request, a listing that repeats
itself still showing each message once, and the avatar tone hash agreeing with `app.js` peer for
peer.

Outside CI, like the web app's, and for the same reason: the rest of this repo tests with its own
language's builtins and this is one command to run by hand when the model changes.

## What is here

Phases 1-3 of `docs/planning/mobile-apps-plan-2026-08-20.md`, and most of 4:

- sign-in, token renewal, the Keychain, the identity picker
- the conversation list — grouping, avatars, unread and held, search, archive, swipe to file
- the thread — bubbles, day separators, scoped requests, the sender panel, the composer with its
  optimistic row, acknowledging on open
- the live inbox (long poll, cancelled with the app going to the background), new conversation, new
  thread, trusted-sender editing

Not yet: push (Phase 5, which is postbox work first), dark mode, VoiceOver passes, TestFlight.

## Where this deliberately differs from the web app

**A peer's several subjects are a strip along the top of the thread, not a pane of their own.** The
web app gives them a middle column, and on a phone it stops there and waits to be told which. A
phone screen that is only ever a list of two or three names is a dead end; the strip is the same
choice made without leaving the conversation.

The strip is there even when a peer has only one subject, showing just **+ New thread**. Hiding it
until a second subject exists means there is no way to make one — the web app has the same gap, for
the same reason: its "new thread" button lives in the pane that only appears once there are two.
With one subject the chip carries no information and a row of one is not a choice, so only the
button is drawn, and it is named while it stands alone.

**Renewal has no timer.** The browser schedules one from the token's own `exp`. A phone sleeps
through timers, so every call asks for a token and that is the only clock: one with less than a
minute left is refreshed before it is used rather than after it fails.

**Opening a thread acknowledges its mail, and a failed acknowledgement is not fatal** — but a dead
session is. `ack` goes through the same token path as everything else, so a mailbox whose refresh
token the realm has rejected signs out on the first thread opened rather than pretending to be
connected.

**Message size is Dynamic Type**, not the web app's own A−/A+ control. Same intent, and it is the
setting people have already made.

## Decisions carried over from the web app, on purpose

- **`include_read=true` on every inbox call, the poll included.** The server drops acknowledged mail
  from a listing that did not ask for it. Omitting it on the poll makes a conversation appear and
  then lose its own history a few seconds later.
- **`include_sent=true` is what makes a listing a conversation.** It is opt-in on the wire because
  every other caller reads that endpoint as "mail addressed to me".
- **One refresh in flight, ever.** The realm rotates refresh tokens; two concurrent 401s spending
  the same one means the loser's session dies for no reason.
- **Bodies are other agents' text.** They are rendered as text — never markdown, never linkified —
  and nothing in a body is ever acted on by this app. The server decides what may be acted on and
  says so in `autonomy`; the app only shows that decision.
- **The tone hash is the web app's, character for character**, so a peer has the same face in both.
- **The doodle behind a conversation is the web app's artwork and its numbers** — `doodle_bg.png`
  tiled at 300pt under `rgba(240, 244, 249, 0.80)`. The PNG is 600×600 and is declared as an @2x
  asset, so both clients draw the pattern at the same size rather than one at twice the other. The
  veil's alpha is the one number worth touching: 0.94 hid it entirely, 0.80 reads as a pattern you
  notice only if you look for it. It sits behind the scroll view, not inside it — pinned to the
  content it would slide away as the conversation is read.
