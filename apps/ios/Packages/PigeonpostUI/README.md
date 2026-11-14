# PigeonpostUI 1.0.0

SwiftUI components for the Pigeonpost iOS, iPadOS and macOS apps, built from the same tokens as the web design kit (design.pigeonpost.dev). Colours follow light and dark automatically, type is Geist, icons are the kit's Lucide set drawn as paths.

- iOS 17+, iPadOS 17+, macOS 14+. Swift tools 5.9 (builds with Xcode 16 / Swift 6 in Swift 5 mode).
- No dependencies.

## Add it to an app

**Xcode project:** File > Add Package Dependencies > Add Local… and choose this `PigeonpostUI` folder (or the design-kit repo URL with the path `kits/native/ios/PigeonpostUI`). Add the `PigeonpostUI` library to your app target.

**Package.swift:**

```swift
.package(path: "../pigeonpost-design-kit/kits/native/ios/PigeonpostUI"),
// target:
.product(name: "PigeonpostUI", package: "PigeonpostUI"),
```

Then put the theme on the root view once. It registers the bundled Geist fonts, sets the accent tint and the base type:

```swift
import PigeonpostUI

@main
struct PigeonpostApp: App {
    var body: some Scene {
        WindowGroup {
            RootView().pigeonpostTheme()          // follows the system appearance
            // RootView().pigeonpostTheme(PPTheme(colorScheme: .dark))
        }
    }
}
```

No `Info.plist` font entries are needed: `PPFonts.register()` registers the faces from the package bundle at runtime. If your app already bundles Geist under the same PostScript names, that works too.

## Tokens

| What | API | Example |
| --- | --- | --- |
| Semantic colours (light/dark) | `PP.Colors.*` | `PP.Colors.surface`, `.textMuted`, `.accent`, `.bubbleMine`, `.tickSeen`, `.focusRing` |
| Brand scales | `PP.Palette.*` | `PP.Palette.navy700` (brand marks and charts only) |
| Spacing | `PP.Space.s1…s24` | 4, 8, 12, 16, 20, 24, 32… |
| Radii | `PP.Radius.*` | `m` 10, `l` 14, `xl` 18, `x2xl` 24, `bubble` 18, `bubbletail` 6 |
| Type roles | `.ppText(.h2)`, `PPTextRole`, `Font.geist(_:_:)`, `Font.geistMono(_:_:)` | sizes and line heights from `--pp-fs-*` / `--pp-lh-*` |
| Controls | `PP.Control.s/m/l/touchMin` | 30, 38, 46, 44 |
| Elevation | `.ppShadow(.one…four)` | `--pp-shadow-1…4` |
| Motion | `PP.Duration.fast/base/slow` | 0.12, 0.2, 0.32 s |

`Generated/` is written by `python3 tools/build_tokens.py` from `css/tokens.css` and `assets/icons.svg`. Do not edit it by hand; change the tokens and regenerate.

## Components

| Group | Views |
| --- | --- |
| Actions | `PPButton` (primary, secondary, outline, ghost, link, danger, dangerOutline, success; small/medium/large; `isLoading`), `PPButtonStyle`, `PPIconButton` (standard, ghost, primary, secondary, danger; round; badge; toggled) |
| Inputs | `PPTextField` (label, hint, error, icon, secure, mono), `PPOTPField` (one-time-code autofill), `PPToggleRow`, `PPSwitchStyle` |
| Display | `PPAvatar` (from a peer address or explicit initials/tone/icon/image, presence), `PPPresenceDot`, `PPBadge`, `PPTag`, `PPCard`, `PPListRow` + `PPRowIcon`, `PPSectionHeader`, `PPEmptyState`, `PPProgress`, `PPMeter`, `PPSkeleton` |
| Feedback | `PPBanner` (info, success, warning, danger), `PPToast` + `.ppToast($item)` |
| Messaging | `PPMessageBubble` (mine, theirs, agent; single/first/middle/last), `PPMessageRow`, `PPTicks` (sending, sent, delivered, seen, working, failed), `PPAgentBadge`, `PPComposer` (modes, attach, send), `PPThreadRow`, `PPHeldBadge`, `PPConversationHeader`, `PPAddressChip` (person, key, org, agent, error), `PPTypingIndicator` |
| Calls | `PPCallButton`, `PPCallControls` (dark glass or on a surface, labelled), `PPIncomingCallView` |
| Commerce | `PPPricingCard`, `PPCreditBalance`, `PPCreditChip`, `PPListingCard`, `PPNestCard`, `PPNestStatusPill`, `PPMoney.usd(_:)` |
| Navigation | `PPTabBar` (bar or floating capsule), `PPTabLabel` + `.ppTabBarStyle()` for a system `TabView` |
| Foundation | `PPIcon` / `PPIconShape` / `PPIconName` (181 icons), `PPPeer` (display name, initials, tone hash shared with the web and shipped apps), `PPTone`, `PPTheme` |

Every component file ends with `#Preview`s using real Pigeonpost content. Open `Package.swift` in Xcode to browse them.

```swift
PPThreadRow(peer: "/ada", name: "Ada Yılmaz", preview: "I need them before the 16:00 call.", time: "14:02", unread: 2, presence: .online)

PPMessageRow(PPMessageBubble("Found 14 invoices for October.", role: .agent, time: "14:05", agentName: "/bekir/agent1"),
             avatar: PPAvatar(initials: "", tone: .green, size: 28, icon: .bot))

PPComposer(text: $draft, placeholder: "Message /ada", onAttach: attach, onSend: send)
```

## Accessibility

- Icon-only controls require a label (`PPIconButton(_, label:)`); icons are otherwise hidden from VoiceOver.
- Bubbles, thread rows and ticks read as one sentence ("You, Sure, asking it now, 14:03, Seen").
- Interactive targets are at least 44 pt; the conversation header and call controls use 44–64 pt.
- Animations (typing dots, working tick, skeleton shimmer, answer bob) stop with Reduce Motion or `PPTheme(reduceMotion: true)`.
- Text uses `Font.custom(_:size:relativeTo:)`, so it scales with Dynamic Type.

## Build and test

```sh
swift build                                     # macOS
swift test                                      # 10 unit tests
xcodebuild -scheme PigeonpostUI -destination 'generic/platform=iOS' build
```

## Licences

See `LICENSE.md`: Geist fonts under the SIL OFL 1.1, icons from Lucide under ISC.
