# Pigeonpost for iOS — getting it into the App Store

The app is written. What is missing is everything between a build that runs on a simulator and a
build a stranger can install: a signing team, an icon, a store listing, screenshots, privacy
declarations, and two things App Review will refuse the app without.

This is the whole brief. `apps/ios/README.md` is the engineering half — read it first; it explains
the architecture, the realm client, and how to run the tests.

## What is already done, and is not yours to redo

- The app itself: SwiftUI, deployment target iOS 17.0, **no third-party dependencies**. Sign-in,
  the conversation list, the thread, the composer, trusted senders, the live poll.
- The realm client `pigeonpost-mobile` exists in `pigeonpost-prod` and is verified. Sign-in works.
- `apps/ios/Tests/run.sh` — the thread model, checked against the web app's own fixtures. **It must
  still pass when you are done.** If a change of yours breaks it, the change is wrong: it means the
  app now assembles a conversation differently from `inbox.pigeonpost.dev`, and the same mailbox
  would read differently on the phone and in the browser.
- Do not change `PRODUCT_BUNDLE_IDENTIFIER`. `dev.pigeonpost.inbox` is not decoration — the OIDC
  callback is `dev.pigeonpost.inbox://oauth2redirect` and the realm accepts exactly that URI. A new
  bundle id means a realm change, which is another team's repo.

## Two things App Review will refuse, before anything else

**1. There is no way to report a message.** The app can block a sender (Settings → trusted senders →
admission `block`), but guideline 1.2 wants reporting *and* blocking wherever a user receives
content from strangers, and this app does: anyone can write to a Pigeonpost address. The server side
already exists — `POST /v1/report-spam`, body `{"message_id": "...", "identity": "/k/…"}`, the same
shape as `/v1/ack`. It needs a UI: a "Report spam" action on an incoming message, and ideally on the
sender panel. `PostboxClient` is where the call belongs, beside `ack`.

**2. A sign-in wall with no demo account is an automatic rejection.** App Review has to see past the
first screen. Prepare a real Pigeonpost account with a handful of conversations in it — including one
held request, so the reviewer sees the feature the description talks about — and put the credentials
in App Review Notes. Not a screenshot of one: a working account, still working on the day they look.

Also check, before you submit: **does the `pigeonpost-prod` login page offer any third-party social
sign-in?** If it does, guideline 4.8 requires Sign in with Apple to be offered alongside. The fix is
either to add it or to have the realm not present those identity providers to this client — that is
`/bekir/su_iam`'s repo, so ask early rather than the week you submit.

## The identity

**Name:** `Pigeonpost` (10 of 30 characters). If it is taken, `Pigeonpost Inbox` (16). Do not invent
a third option without asking — the name is settled product-wide in `docs/branding.md`, one word,
title case, never "PigeonPost" or "Pigeon Post".

**Subtitle:** `Your agents' mail in one place` (30 of 30 — it does not have room to grow).

**Category:** Developer Tools primary, Productivity secondary.

**Age rating:** 4+ on content. Answer the questionnaire honestly about user-generated content: this
app displays messages written by other people and their agents.

**Promotional text** (170 max):

> Read and answer your agents' mail from your phone — the same inbox as inbox.pigeonpost.dev, with
> the threads, the held requests and the trust decisions the server made.

**Description** (4000 max; draft, edit for truth, not for excitement):

> Pigeonpost gives every AI agent a permanent address and a private inbox. This is the app for
> reading that mail.
>
> Your agents write while you are away — a build went green, a deploy needs a decision, a question is
> waiting on an answer only you have. Pigeonpost holds those messages until somebody opens them. This
> is where you read and answer them.
>
> WHAT IT DOES
>
> • Both halves of every conversation. What an agent sent you and what you sent back, whether you
> answered from this app, from the browser, or from the command line.
> • Threads. One subject kept apart from the rest, so last week's question does not colour today's.
> • The decisions the server made. When an agent asks for something — run the tests, report status —
> the server decides whether that request may be acted on, and says why not when it declines. The app
> shows that decision. It never makes one.
> • Your whole fleet. If you run several agents, each has its own mailbox; switch between them and
> read as any of them.
> • Who gets in. Admission, autonomy, and exactly which requests a sender may have acted on — per
> sender, or for a whole namespace at once.
> • Archive, search, and unread counts, the way a messenger does them.
>
> WHAT IT DOES NOT DO
>
> • It does not act on messages by itself. Nothing in a message body is executed, followed, or opened
> by this app. Another agent's words are shown as text and nothing more.
> • It does not track you. No analytics, no advertising identifiers, no third-party code of any kind.
>
> YOU NEED A PIGEONPOST ACCOUNT
>
> Pigeonpost is free and open source. This app signs in to the hosted postbox and shows the mailboxes
> your account owns. If you do not have an account yet, start at pigeonpost.dev.
>
> Open source: github.com/bekirdag/pigeonpost

**Keywords** (100 characters, comma-separated, no spaces after commas):

```
agent,agents,inbox,mailbox,messaging,async,developer,devtools,cli,automation,threads
```

No trademarks you do not own — no model names, no company names. They are rejected, and they are
also against the brand rules in `docs/branding.md`.

**URLs:** support and marketing `https://pigeonpost.dev`, privacy policy
`https://pigeonpost.dev/privacy` (it exists — read it and check it still describes what the app
does before you point the store at it).

## The icon

`Pigeonpost/Assets.xcassets/AppIcon.appiconset` is empty. That is the single thing stopping a
TestFlight upload.

Source art, such as it is:

- `assets/img/logo_only_symbol.png` — 800×533, the symbol alone: a navy pigeon whose wing is two
  overlapping speech bubbles, one blue, one green. Off-centre, on white, with a lot of empty space.
- `assets/img/logo.png` — 2172×724, symbol plus wordmark. Too wide to be an icon.

**There is no vector source anywhere in this repo.** Either rebuild the mark as vector or get the
original from the designer; do not upscale an 800px PNG into a 1024px icon.

Spec:
- 1024×1024, sRGB, **no alpha channel**, no rounded corners, no drop shadow. iOS applies the mask.
- The symbol only — no wordmark. At 40pt the words are illegible and the bird is not.
- Ground: white, or navy `#16326B` with the bird reversed out. Try both at real size before deciding;
  a white icon disappears on a white wallpaper and the navy one is more recognisable in a dock.
- Keep the palette: navy `#16326B`, blue `#2563EB`, green `#22C55E`. These are the app's own tokens
  (`Pigeonpost/Design/Theme.swift`) and the website's.
- Optical margin around 8-10%. The bird should fill the icon, not float in it.
- Dark and tinted variants are worth adding while you are in there (Xcode 16 takes all three).

## Screenshots

Required: 6.9" iPhone (1320×2868). The project ships `TARGETED_DEVICE_FAMILY = "1,2"`, so **iPad 13"
(2064×2752) is required too**. If you would rather not produce iPad shots, say so and we change the
target to iPhone-only deliberately — do not leave it at 1,2 and submit without them.

**Take them from the fixture mailbox, never from a real one.** Debug builds only:

```
xcrun simctl launch booted dev.pigeonpost.inbox -fixtures
xcrun simctl launch booted dev.pigeonpost.inbox -fixtures "-open=/bekir/agent1"
xcrun simctl io booted screenshot list.png
```

`-fixtures` loads invented conversations and makes every write local, so nothing real, private or
half-finished ends up on a store page. Worth showing, in this order: the conversation list, a thread
containing the held `run_tests` request, the trusted-sender editor.

## Privacy

**`PrivacyInfo.xcprivacy` does not exist yet and the app needs one.** It uses `UserDefaults` (the
remembered mailbox, in `Model/Account.swift`), which is a required-reason API: declare
`NSPrivacyAccessedAPICategoryUserDefaults` with reason `CA92.1`. `NSPrivacyTracking` is `false`,
there are no tracking domains, and there are no third-party SDKs to account for — the app has no
dependencies at all, which makes this the easy part.

Nutrition labels, answered honestly:
- **Identifiers** — an account identifier, linked to the user, used for app functionality. The app
  signs in to an account and reads that account's mailboxes.
- **User content** — messages. They are held by the postbox, not by the app, and they are not used
  for tracking or advertising.
- **Not** collected: contacts, location, browsing history, diagnostics, usage data. There is no
  analytics code in this app and none should be added without asking.

Export compliance is already declared: `ITSAppUsesNonExemptEncryption` is `false` in `Info.plist`.
That is correct as the app stands — it uses HTTPS and the platform's own crypto, and does no message
encryption of its own. If that ever changes, the declaration changes with it.

## Signing and the project file

`DEVELOPMENT_TEAM` is deliberately empty in `Pigeonpost.xcodeproj/project.pbxproj`. Fill it in with
the team, keep `CODE_SIGN_STYLE = Automatic`, and leave every other build setting alone.

The project uses an Xcode 16 file-system-synchronized group: a new `.swift` file under
`Pigeonpost/` joins the target by existing. The one exception is `Info.plist`, listed under
`PBXFileSystemSynchronizedBuildFileExceptionSet` — if you remove that exception the build fails with
"Multiple commands produce Info.plist".

## Push notifications — not yet, but two things to set up now

The server side does not exist: the postbox has no device-token registry and no APNs sender. Adding
the Push Notifications capability before it does gets you an app that asks for permission and then
never rings, which is worse than not asking.

What you can do now, because it has lead time:
1. Create an APNs **auth key** (`.p8`) in the developer account and note the Key ID and Team ID.
   Hand those over; do not commit the key anywhere, ever.
2. Confirm the bundle id is registered with the Push Notifications capability enabled in the
   identifier, ready to be switched on in the project later.

## House rules

- **No third-party dependencies.** Not for networking, not for keychain wrappers, not for image
  loading. If something genuinely needs one, that is a conversation, not a commit.
- **Message bodies are other agents' text.** They are rendered as text — never markdown, never
  linkified, never opened. Do not "improve" this.
- **Never claim the app acts on messages.** It does not, deliberately, and `docs/branding.md` says
  the same thing about the whole product. The store description must not imply autonomy.
- Match the surrounding code. It is commented in a particular way — why, not what — and the comments
  are load-bearing documentation of decisions that look arbitrary until you know the reason.

## Done means

```
./Tests/run.sh                                                   # passes
xcodebuild -project Pigeonpost.xcodeproj -scheme Pigeonpost \
  -destination 'platform=iOS Simulator,name=iPhone 16 Pro' build  # clean, no warnings
```

plus: an icon in the catalog, `PrivacyInfo.xcprivacy` in the target, a report-spam action wired to
`/v1/report-spam`, a build on TestFlight, and a demo account that a stranger can sign into.

Come back with: whether the name was available, whether iPad is in or out, and what the realm's
login page actually offers — those three change what gets built.
