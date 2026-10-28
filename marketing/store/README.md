# Store visuals

Marketing screenshots for the App Store (iPhone, iPad) and Google Play (phone, 7" and 10" tablets,
feature graphic). Everything on screen is invented: the people, agents, groups, messages, calls and
meetings come from [`seed/demo-world.json`](seed/demo-world.json), never from a real account.

```
copy.json            headlines, sub-lines, order, light/dark per scene
seed/demo-world.json the fictional world every capture is populated with
capture/ios/         simulator captures (Debug fixture `-fixtures -marketing` + a UI test)
capture/android/     emulator captures (debug fixture mode `marketing`)
capture/web/         call, live stream and meeting-notes pages (the WebView content of the apps),
                     with synthetic, canvas-drawn video — never a real camera
captures/<class>/    raw app screens: ios-phone, ios-tablet, android-phone, android-tablet
render/render.mjs    composes headline + device frame + capture, writes PNGs and index.html
render/validate.mjs  checks sizes, RGB without alpha, file size and counts
render/assets/       Geist (SIL OFL, see OFL.txt), the Pigeonpost logo and mark
```

## Rebuild

1. Captures (only when the app's screens change): run `capture/ios/capture.sh`,
   `capture/android/capture.sh` and `capture/web/capture.sh`. Each creates its own simulator,
   emulator or headless Chrome and removes it afterwards. The iOS `live` and `meeting-notes` screens
   were taken on the Apple round-three branch (PR #148) with `capture/ios/r3-live-notes.patch`; on
   `main` after #148 merges, apply that patch first. The web capture (the call) runs a real
   seven-person call through `calls-service` and a local LiveKit, with canvas-drawn avatars as
   cameras; it needs `livekit-server` on PATH and `puppeteer-core` (`npm i --no-save puppeteer-core`).
2. Compose and check:

```sh
node marketing/store/render/render.mjs ~/Documents/releases/pigeonpost/store-visuals-<date>
node marketing/store/render/validate.mjs ~/Documents/releases/pigeonpost/store-visuals-<date>
```

Needs Google Chrome and ImageMagick. `--only=ios-6.9,ipad-13` and `--scene=03-calls` narrow a run;
`PP_CAPTURES=<dir>` renders from another captures folder.

## Output sets

| Folder | Size | Store slot |
| --- | --- | --- |
| `ios-6.9` | 1320×2868 | App Store iPhone 6.9" |
| `ios-6.5` | 1284×2778 | App Store iPhone 6.5" |
| `ipad-13` | 2752×2064 (landscape, upright pixels) | App Store iPad 13" |
| `android-phone` | 1080×1920 | Google Play phone |
| `android-7in` | 1920×1200 | Google Play 7" tablet |
| `android-10in` | 2560×1600 | Google Play 10" tablet |
| `android-feature` | 1024×500 | Google Play feature graphic |

All PNG, 8-bit RGB, no alpha, sRGB.
