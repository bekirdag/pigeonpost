# Physical-device recording for iOS build 42

Apple requires this evidence for the September 20 guideline 2.1(b) rejection. Use a physical iPhone or iPad with **Pigeonpost Inbox 1.0 (42)** installed from TestFlight. Simulator recordings and local StoreKit fixtures do not satisfy the request.

## Before recording

- Sign into Pigeonpost with the demo account in App Store Connect's App Review Information fields. Keep its password out of the recording and out of Git.
- Wodo already has a US Sandbox Apple Account under Users and Access → Sandbox. Use that account for the requested sandbox-account test. Follow [Apple's TestFlight sandbox-account setup](https://developer.apple.com/documentation/storekit/testing-in-app-purchases-with-sandbox): install the TestFlight app first, then use the Sandbox Apple Account in Developer settings. Signing out of Media & Purchases for this setup can temporarily affect access to purchased content on that device; do not sign out of the device's main iCloud account.
- TestFlight purchases operate in Apple's sandbox and do not charge real money. The recording must show a successful Apple purchase confirmation and the resulting registration in the app.

## Record the flow

1. Start the device's screen recording on the Home Screen, then launch Pigeonpost.
2. Show the signed-in demo account. Choose `/ppappreview` from the mailbox picker, open a sample conversation, and demonstrate reading and composing a message. Send any demonstration message to the demo mailbox itself at `/ppappreview/main`.
3. Open Settings → Get a handle. Show the Apple product name and localized yearly price. Enter a unique available test name and tap Check availability.
4. Tap Buy for the displayed yearly price, complete Apple's sandbox purchase sheet, and wait until registration succeeds.
5. Open Settings → Handles, show the registered name, and open its inbox.
6. Return to Get a handle and demonstrate purchasing a second independent name. Both names should remain listed under Handles. Leave unused slots available for the reviewer rather than filling the demo account's ten-name limit.
7. Demonstrate Restore purchases and show that both registrations remain available. Show Manage subscriptions. If an expired test subscription is present, its renewal control is under Handles.
8. Return to Get a handle to show the next available subscription. Stop recording and save the original video on this Mac, for example in Downloads.

## Before public submission

Verify the original video is from the physical device and shows successful sandbox confirmation and account registration. Verify the purchased names are tied to the same demo account on the live service. Attach the video to App Review Information, update the review notes with its filename and the observed result, then submit draft `6b5a1281-f265-46dc-ac15-0cb74da89b5a` with all 21 items. Do not claim the recording or sandbox purchase is complete until actually verified.
