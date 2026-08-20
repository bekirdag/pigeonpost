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

## iPad

**Out, deliberately, as of 1.0 (1).** The app builds and runs on iPad — `NavigationSplitView` gives
it a real two-pane layout — but nothing about that layout has been tuned: on a 13" screen the thread
fills the width with a composer stretched across it and a great deal of empty paper, and the sidebar
collapses as soon as a conversation is opened programmatically. Shipping that would be shipping a
stretched iPhone app.

Adding iPad later is a normal thing to do in a later version and costs nothing now beyond a
`TARGETED_DEVICE_FAMILY` change plus the layout work and the 13" screenshots. iPad owners can still
install the iPhone build in the meantime.

## The icon

**Done** — see `Icon/README.md`. The mark was traced back into vector rather than upscaled, and the
result is `AppIcon.appiconset/icon-1024.png`, 1024×1024, sRGB, no alpha. The rest of this section is
kept because it is what the icon has to satisfy, and because the dark and tinted variants are still
open.

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
- Keep the mark's own palette: navy `#05368B`, blue `#086CFF`, green `#26CE2D`. These are the
  artwork's actual colours, and they are more saturated than the app's UI tokens in
  `Design/Theme.swift` (`#16326B`, `#2563EB`, `#22C55E`). The logo is the logo.
- Optical margin around 8-10%. The bird should fill the icon, not float in it.
- Dark and tinted variants are worth adding while you are in there (Xcode 16 takes all three).

## Screenshots

Required: 6.9" iPhone (1320×2868). **iPad shots are not needed** — the target is
`TARGETED_DEVICE_FAMILY = 1`, iPhone only, as of the first TestFlight build. See "iPad" below.

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

## Push notifications — built and shipped in 1.0 (4), inert until a key exists

The capability is on the App ID, the entitlement is in the target, the app registers its token and
opens the right conversation when a notification is tapped, and the postbox (0.6.2, live) has the
device registry and the APNs sender.

**One thing is missing and only a human can make it:** the APNs auth key. Apple Developer →
Certificates, Identifiers & Profiles → **Keys** → **+** → enable **Apple Push Notifications service
(APNs)** → download the `.p8` once. Then on the postbox host, put the file at
`/opt/pigeonpost-postbox/apns.p8` (chmod 600, owned by uid 65532) and add to `postbox.env`:

```
PIGEONPOST_APNS_KEY_PATH=/data/apns.p8
PIGEONPOST_APNS_KEY_ID=<the key's 10-character id>
PIGEONPOST_APNS_TEAM_ID=<the team id>
PIGEONPOST_APNS_TOPIC=dev.pigeonpost.inbox
```

then recreate the container. Phones already registered start being woken immediately — no app
update. Until then the postbox sends nothing and logs nothing about it.

## The old note, kept because the second half still applies

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

## State, 2026-08-20 — everything but the upload

Done in the repo: `DEVELOPMENT_TEAM = AH277897AV` on both configurations, version 1.0 build 1,
iPhone-only, the traced 1024 icon, `PrivacyInfo.xcprivacy`, report-spam on every incoming message and
block on the sender panel, and `ExportOptions.plist` for the export.

Verified in an unsigned Release build for a real device (`CODE_SIGNING_ALLOWED=NO`), reading the
built `Info.plist` rather than trusting the source: bundle id `dev.pigeonpost.inbox`,
`CFBundleShortVersionString` 1.0, `CFBundleVersion` 1, `MinimumOSVersion` 17.0, `UIDeviceFamily`
`[1]`, `CFBundleIconName` `AppIcon`, the `dev.pigeonpost.inbox` URL scheme, and
`ITSAppUsesNonExemptEncryption` `false`. `PrivacyInfo.xcprivacy` is in the bundle with the
`UserDefaults`/`CA92.1` declaration intact.

**Blocked on credentials, not on code.** The archive fails here with two errors:

```
error: No profiles for 'dev.pigeonpost.inbox' were found
error: Unable to log in with account 'orioncabbar@gmail.com'. The login details were rejected.
```

This machine holds one signing identity — `Developer ID Application`, which signs software
distributed outside the App Store — and no Apple Distribution certificate or App Store profile. With
the Xcode account re-authenticated, automatic signing creates both on the first archive.

Either finish it in Xcode — Settings → Accounts, sign in again, then Product → Archive → Distribute
App → App Store Connect — or hand over an App Store Connect API key (`.p8`, key id, issuer id) and
the whole thing runs headlessly:

```
xcodebuild -project Pigeonpost.xcodeproj -scheme Pigeonpost -configuration Release \
  -destination 'generic/platform=iOS' -archivePath build/Pigeonpost.xcarchive archive \
  -allowProvisioningUpdates -authenticationKeyPath /abs/AuthKey_XXXX.p8 \
  -authenticationKeyID XXXX -authenticationKeyIssuerID <uuid>
xcodebuild -exportArchive -archivePath build/Pigeonpost.xcarchive \
  -exportOptionsPlist ExportOptions.plist -exportPath build/export -allowProvisioningUpdates …
```

## Shipped to TestFlight, 2026-08-20 18:08

**1.0 (1) is on TestFlight**, in the `Pigeonpost Internal` group, `VALID`. Built on a GitHub macOS
runner with Xcode 26.6 against `iphoneos26.5`, minimum iOS 17.0, uploaded by
`.github/workflows/ios-testflight.yml` (run 32401268297).

It cleared export compliance without anyone being asked, because `ITSAppUsesNonExemptEncryption` is
declared in `Info.plist` — that is what that key buys.

The first run of the workflow failed, and the reason is worth keeping: the Xcode picker compared
major versions only, so every 26.x tied and it kept whichever the glob listed first — 26.0.1, whose
SDK is older than every simulator runtime installed beside it. `actool` refuses that combination
outright, and it is the same error this app hit on the first day, for the same reason. Fixed by
comparing full versions with `sort -V`.

Still ahead of a public release: the listing metadata, and the demo account for App Review.

## Uploading from CI — `.github/workflows/ios-testflight.yml`

The upload runs on a GitHub macOS runner with Xcode 26, so the machine the agent daemon and the
fleet live on never has to be upgraded for it. Manual dispatch only: a TestFlight build is the same
commit uploaded repeatedly with a new build number, and App Store Connect refuses a number it has
already seen, so the number is an input and nothing fires on its own.

Three repository secrets, all from one App Store Connect API key:

| Secret | Where it comes from |
| --- | --- |
| `APPLE_API_KEY_ID` | the 10-character Key ID beside the key |
| `APPLE_API_ISSUER_ID` | the Issuer ID at the top of the Integrations page — one per team |
| `APPLE_API_PRIVATE_KEY` | the exact contents of `AuthKey_XXXXXXXXXX.p8` |

Make the key at App Store Connect → **Users and Access → Integrations → App Store Connect API →
Team Keys**. Give it the **Admin** role: `-allowProvisioningUpdates` has to *create* the
distribution certificate the first time, and App Manager can manage profiles and builds but not
certificates. The `.p8` downloads once and never again.

```
gh secret set APPLE_API_KEY_ID     --repo bekirdag/pigeonpost --body "XXXXXXXXXX"
gh secret set APPLE_API_ISSUER_ID  --repo bekirdag/pigeonpost --body "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
gh secret set APPLE_API_PRIVATE_KEY --repo bekirdag/pigeonpost < ~/Downloads/AuthKey_XXXXXXXXXX.p8
gh workflow run "iOS TestFlight" --repo bekirdag/pigeonpost -f build_number=1 -f upload=true
```

The workflow refuses to continue on a runner whose newest iOS SDK is older than 26, quoting the
error Apple gave, rather than discovering it after a full archive and an upload attempt — which is
how it was discovered the first time. If `macos-26` is not a label this account can schedule, re-run
with `-f runner=<image>`; the SDK check will say whether that image can do the job.

`-f upload=false` archives, signs and exports without sending anything, which is the way to prove
the signing chain works without spending a build number.

## Second attempt, 2026-08-20 17:07 — signing solved, stopped by the SDK

With `bekir.dag@wodonetwork.com` signed in to Xcode, **the archive succeeded** and signing assets
were created automatically (`Apple Development: BEKIR DAG (2K9FY38P69)`). The export built
`Pigeonpost.ipa` and reached App Store Connect, which rejected it during validation:

```
SDK version issue. This app was built with the iOS 18.2 SDK. All iOS and iPadOS apps must be
built with the iOS 26 SDK or later, included in Xcode 26 or later, in order to be uploaded to
App Store Connect or submitted for distribution.
```

That is not something the project can fix. This machine runs **macOS 14.7.4** with **Xcode 16.2**
(iOS 18.2 SDK), and Xcode 26 requires **macOS Sequoia 15.6 or later** — so the upload needs a macOS
upgrade followed by an Xcode upgrade, or a machine that already has both.

Three ways out:

1. **Upgrade this Mac** — macOS 15.6+ then Xcode 26 (~20 GB). Simplest to reason about, and the
   heaviest: this is the machine the agent daemon, the fleet and the whole local toolchain run on.
2. **Build on a GitHub Actions macOS runner** with Xcode 26 and an App Store Connect API key in
   repository secrets. Nothing local changes, the upload becomes reproducible, and `release.yml`
   already runs macOS jobs (`macos-14`, `macos-15-intel`) so the shape is familiar. Interactive
   accounts do not work on CI, so this needs the API key either way.
3. **Any Mac already on macOS 15.6+ with Xcode 26** — open the project, Product → Archive.

**Whichever route: expect a visual pass afterwards.** An app built against the iOS 26 SDK adopts that
release's system appearance, and this app leans on system chrome — navigation bars with a deliberate
opaque background, sheets, the list style, the composer inset. None of that is broken by the change;
all of it should be looked at again once it is built against the new SDK.

## First attempt, 2026-08-20 — stopped at the credential

The archive was run again and fails before anything is built:

```
error: No profiles for 'dev.pigeonpost.inbox' were found
error: Unable to log in with account 'orioncabbar@gmail.com'. The login details were rejected.
```

Nothing had changed since the first attempt: this machine holds one signing identity,
`Developer ID Application` — which signs software distributed *outside* the store and cannot sign an
App Store build — and there is no App Store Connect API key under `~/.appstoreconnect`,
`~/private_keys` or the project. No provisioning profiles are installed.

**Nothing in the repository is blocking this.** The Release build is clean and the built app carries
everything the store checks. What is missing is permission to talk to Apple as this team.

Two ways to unblock, either is enough:

1. **Re-authenticate in Xcode** — Settings → Accounts → the Apple ID for team `AH277897AV`, then
   Product → Archive → Distribute App → App Store Connect. Automatic signing creates the
   distribution certificate and the profile on the first archive.
2. **An App Store Connect API key** — App Store Connect → Users and Access → Integrations → App Store
   Connect API, with the *App Manager* role. Put `AuthKey_XXXX.p8` in
   `~/.appstoreconnect/private_keys/` and pass the key id and issuer id; the whole thing then runs
   without a browser, using the commands above.

Screenshots are done either way: `Screenshots/6.9-inch/`, 1320×2868, from the fixture mailbox.

Still outstanding for **review** rather than for upload: the demo account. A realm user has to be
created by whoever owns `pigeonpost-prod` — `/bekir/su_iam` — and then seeded with a few
conversations and one held request.

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
