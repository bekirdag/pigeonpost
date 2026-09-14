# Android app artwork

`icon-512.png` is the 512 × 512 sRGB PNG exported from the iOS `Assets.xcassets/AppIcon.appiconset/icon-1024.png`. The same file is installed as `drawable-nodpi/ic_pigeonpost.png` for the Android sign-in and onboarding screens and uploaded as the main Google Play store listing icon.

Regenerate from the repository root with ImageMagick:

```sh
magick apps/ios/Pigeonpost/Assets.xcassets/AppIcon.appiconset/icon-1024.png -resize 512x512 -colorspace sRGB -define png:color-type=6 apps/android/store/icon-512.png
cp apps/android/store/icon-512.png apps/android/app/src/main/res/drawable-nodpi/ic_pigeonpost.png
```

The adaptive launcher foreground already uses the identical three paths and colours from `apps/ios/Icon/pigeonpost-mark-light.svg`. Its white background and safe inset allow Android to apply the device mask. The monochrome resource lets Android apply the user's themed icon colour. Do not bake rounded corners or an outer shadow into the Play artwork: Google Play applies these itself.

Specification: https://developer.android.com/distribute/google-play/resources/icon-design-specifications
