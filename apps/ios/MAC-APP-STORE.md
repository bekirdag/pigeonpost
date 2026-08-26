# Pigeonpost Desktop — getting it into the Mac App Store

The iOS runbook is `APP-STORE.md` and most of it still applies: same team, same App Store Connect
API key, same "an upload is not a delivery" lesson. This covers only what is different on the Mac,
which is more than it looks.

## The decision that cannot be undone

The Mac app uses **`dev.pigeonpost.inbox`** — the same bundle id as the iPhone app. That is what
Apple requires for **Universal Purchase**, and it buys three things:

- one App Store listing covering both platforms, rather than two records to describe and maintain;
- the $8 handle subscription bought on a phone works on the Mac, with no second in-app purchase to
  configure and no second set of prices across 175 territories;
- one review queue instead of two.

Apple matches the platforms on bundle id alone, and a record's bundle id is fixed once created. It
was `dev.pigeonpost.inbox.mac` until 2026-08-26; if anything still refers to that, it is stale.

## What has to be true before the first upload

1. **The App ID must be Universal.** In the developer portal an App ID is registered for iOS *or*
   macOS *or* both, and the API will not change an existing one. Certificates, Identifiers &
   Profiles → Identifiers → `dev.pigeonpost.inbox` → tick **macOS** → Save. Without this the
   provisioning profile cannot be created and the archive will not sign.
2. **The app record needs a macOS platform.** App Store Connect → Pigeonpost → the `+` beside the
   platforms → macOS. This is where the Mac screenshots and description go; the app's name, privacy
   answers and subscription carry over.
3. **Two signing identities must exist.** See below. This is the part with no iOS equivalent.

## Signing: why two certificates

An iOS upload is an `.ipa` signed once. A Mac App Store upload is a **`.pkg`** — an installer with
the app inside it — and the two layers are signed by different certificates:

| Layer | Certificate | Where it comes from |
| --- | --- | --- |
| `Pigeonpost Desktop.app` | **Apple Distribution** | already held; `APPLE_DIST_P12`, shared with iOS |
| the `.pkg` around it | **Mac Installer Distribution** (`3rd Party Mac Developer Installer`) | new; `APPLE_MAC_INSTALLER_P12` |

The Apple Distribution certificate is not platform-specific, which is why the existing one is
reused. The installer certificate has no iOS counterpart at all, so it has to be made once:

```sh
KEY_ID=<the App Store Connect key id> \
ISSUER_ID=<the issuer id from the Integrations page> \
KEY=@~/.appstore/AuthKey_<key id>.p8 \
  python3 .github/scripts/mac_signing_assets.py --out ~/pigeonpost-mac-signing
```

It creates the installer certificate and the `MAC_APP_STORE` provisioning profile, prints the
`gh secret set` commands for both, and then the directory should be deleted. The private key is
generated locally and never sent to Apple — only a certificate signing request is.

**It refuses to create a second installer certificate if one already exists**, and says so instead.
Apple caps distribution certificates per team at a small number, and the iOS pipeline has already
been through what happens when something mints one per build: ten builds in, the eleventh fails with
"Choose a certificate to revoke." If the existing certificate's private key is still in Keychain
Access, export it from there (right-click the private key → Export) rather than making a new one.

## Secrets

Existing, shared with iOS: `APPLE_API_PRIVATE_KEY`, `APPLE_API_KEY_ID`, `APPLE_API_ISSUER_ID`,
`APPLE_DIST_P12`, `APPLE_DIST_P12_PASSWORD`.

New, Mac only:

| Secret | What |
| --- | --- |
| `APPLE_MAC_INSTALLER_P12` | base64 of the Mac Installer Distribution `.p12` |
| `APPLE_MAC_INSTALLER_P12_PASSWORD` | its password |
| `APPLE_MAC_PROFILE` | base64 of `pigeonpost_mac.provisionprofile` |

`APPLE_PROFILE` is the iOS profile and is not used here; the profile types differ even though the
bundle id does not.

## Uploading — `.github/workflows/mac-appstore.yml`

Manual, with the build number as an input, for the same reason as the iOS one: App Store Connect
refuses a build number it has seen before. Run it from Actions → Mac App Store → Run workflow. With
`upload` off it archives, signs and packages without sending anything, which is the cheap way to
find out whether the certificates are right.

The build runs on a GitHub runner rather than locally because Apple refuses uploads built against an
SDK older than the current one, and that needs an Xcode newer than macOS 14.6 will install — which
is what the machine the agent fleet runs on has.

## The sandbox

`PigeonpostMac.entitlements` has been sandboxed since the first build, which the store requires:

- `com.apple.security.app-sandbox`
- `com.apple.security.network.client` — the postbox and the realm. Nothing listens.
- `com.apple.security.files.user-selected.read-write` — attachments, chosen through the system
  picker and written where the person says.

Nothing else. Adding an entitlement is a review question, so the answer to "do we need this" should
stay no.

## Notifications

The app announces mail itself while it is running — `Shared/Push/LocalNotifier.swift`, fed by
`Inbox.onArrival` — which needs no entitlement and no server involvement, and is what a desktop app
that stays open all day actually wants.

Remote push, for when it is closed, is **not** wired up on the Mac yet and needs two things that do
not exist: the Push Notifications capability on the App ID, and a per-device APNs topic in the
postbox. `crates/pigeonpost-postbox/src/push.rs` reads one topic from `PIGEONPOST_APNS_TOPIC` for
the whole server and the `devices` table has no column for it, which was fine while every device was
an iPhone. Sharing the bundle id with iOS makes this smaller than it was — the topic is now the same
string — but the app still has to register, and it cannot until the App ID carries the capability.

## Icon

`PigeonpostMac/Assets.xcassets/AppIcon.appiconset`, generated from the iOS artwork onto the macOS
canvas: a rounded body filling 824 of a 1024 square with the rest transparent, so it lines up in the
Dock with icons of other shapes. The phone's full-bleed square is wrong here and looks it.

The generator is in the session scratch rather than the repo because it is a one-shot; the recipe is
the two ratios above, `CGPath(roundedRect:cornerWidth:cornerHeight:)`, and the ten sizes macOS asks
for from 16 through 512@2x.

## Screenshots

The Mac listing needs its own — the iPhone ones do not carry over. 1280×800, 1440×900, 2560×1600 or
2880×1800; one size, at least one shot, and the same rule as the phone applies: they are read as a
promise about what the app does.

## Done means

A build in App Store Connect under the **macOS** platform of the existing Pigeonpost record, not a
second record; sandboxed; signed by the two certificates above; and a Universal Purchase badge on
the listing, which is the visible proof the bundle ids match.
