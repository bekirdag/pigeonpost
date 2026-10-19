# Android app artwork

`icon-512.png` is the 512 × 512 sRGB PNG exported from the iOS `Assets.xcassets/AppIcon.appiconset/icon-1024.png`. The same file is installed as `drawable-nodpi/ic_pigeonpost.png` for the Android sign-in and onboarding screens and uploaded as the main Google Play store listing icon.

Regenerate from the repository root with ImageMagick:

```sh
magick apps/ios/Pigeonpost/Assets.xcassets/AppIcon.appiconset/icon-1024.png -resize 512x512 -colorspace sRGB -define png:color-type=6 apps/android/store/icon-512.png
cp apps/android/store/icon-512.png apps/android/app/src/main/res/drawable-nodpi/ic_pigeonpost.png
```

The adaptive launcher foreground already uses the identical three paths and colours from `apps/ios/Icon/pigeonpost-mark-light.svg`. Its white background and safe inset allow Android to apply the device mask. The monochrome resource lets Android apply the user's themed icon colour. Do not bake rounded corners or an outer shadow into the Play artwork: Google Play applies these itself.

Specification: https://developer.android.com/distribute/google-play/resources/icon-design-specifications

## Screenshots

`screenshots/` holds the Google Play captures for 0.3.0, all from the debug `store` fixture (invented addresses and messages, no account data), status bar pinned to 9:41 with demo mode:

- `phone/` — 1080 × 2160 (18:9; Play rejects a long side more than twice the short side, so not the emulator's native 1080 × 2400). Inbox (dark), conversation (dark), conversation (light), Settings with the appearance choice (light).
- `tablet-10/` — 2560 × 1600, Pixel Tablet emulator, landscape: the three-column layout in dark and light.
- `tablet-7/` — 1920 × 1200 at 240 dpi on the same emulator (`adb shell wm size 1920x1200; adb shell wm density 240`).

Recreate them with the `StoreScreenshots` instrumentation test (see the app README). Before capturing: `adb shell settings put global sysui_demo_allowed 1`, then the `com.android.systemui.demo` broadcasts `enter`, `clock -e hhmm 0941`, `notifications -e visible false`, `battery -e level 100 -e plugged false`, and `adb shell cmd uimode night yes|no` to match the appearance being captured.
