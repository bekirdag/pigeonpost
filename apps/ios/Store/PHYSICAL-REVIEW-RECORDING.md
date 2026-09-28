# Physical recording for the September 28 replacement

The build-42 recording below is historical evidence for the retired independent-slot model. It does not validate the new capacity plans. The replacement is **1.0 (44)**; record it from TestFlight after the live server and catalog are ready.

1. Use the App Review demo Pigeonpost account and the configured Sandbox Apple Account. Keep passwords outside the recording.
2. Begin recording on the physical iPhone Home Screen, then launch Pigeonpost.
3. Show the demo mailbox and reading/composing a message. Send demonstration messages only to the same demo mailbox `/ppappreview/main`.
4. Open Settings → Get a handle. Show the list of annual plans and the selected total price.
5. Select the one-name plan, enter an available new name, tap Check availability, then use the purchase-and-register button. Complete Apple's sandbox purchase and open the resulting inbox under Settings → Handles. This demonstrates the combined purchase/registration route.
6. Return to Get a handle, select the two-name plan, and complete the Apple upgrade. Register a second name using **Register name — included in your plan**, demonstrating no second payment for that included registration. Show both names under Handles.
7. Use **Restore purchases**, then show the same plan and names. Open **Manage subscriptions** and demonstrate that these levels belong to one subscription; show how a downgrade is scheduled. Return to the app.
8. Save the continuous original recording on this Mac. Record timestamps for the successful Apple confirmations and resulting names; verify live account ownership before attaching it to App Review Information.

A simulator recording or local StoreKit test is not a substitute. Do not claim successful physical sandbox validation before observing it. Leave capacity available for the reviewer where practical; sandbox renewal/expiry is accelerated.

---

# Physical-device recording for iOS build 42

Apple requires this evidence for the September 20 guideline 2.1(b) rejection. Use a physical iPhone or iPad with **Pigeonpost Inbox 1.0 (42)** installed from TestFlight. Simulator recordings and local StoreKit fixtures do not satisfy the request.

## Evidence submitted on 21 September 2026

The user captured the physical iPhone through QuickTime and saved `~/Downloads/pp_ss.mov` (4:13.6). The original remains unchanged. `Pigeonpost-1.0-42-iPhone-Sandbox-Purchases.mp4` retains its first continuous 3:41, compressed to 13,821,549 bytes without internal cuts. It ends after the owned-handle list and before the external App Store sign-in screen.

Verified content: Home Screen and app launch; demo-account reading, composing and sending around 0:40–0:50; successful Apple sandbox purchase confirmations at 1:44 and 2:40; both independent registrations and their inboxes. A separate live authenticated API read confirmed the two Apple handles belong to the reviewer demo account.

The review attachment `ca746064-4a0f-4f85-b8ef-f6c4680e79c4` is COMPLETE, with MD5 `7c19986c3048d840950f7db9c58b4d5b` matching the local review copy. Public submission `6b5a1281-f265-46dc-ac15-0cb74da89b5a` was sent at 08:34 UTC and read back **WAITING_FOR_REVIEW**, containing build 42, ten subscription versions and ten subscription-group versions. Review notes include the video filename, timestamps and the demo account's two existing test names. Credentials were preserved.

The video demonstrates **Refresh account handles**, not **Restore purchases**. Physical restoration and completed external subscription management were not captured; no success is claimed for them. The first and additional independent purchase flows are recorded. The steps below remain the checklist for future captures.

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

For future submissions, verify the original video is from the physical device and shows successful sandbox confirmation and account registration. Verify the purchased names are tied to the same demo account on the live service. Attach the video to App Review Information, update the review notes with its filename and the observed result, then submit the current draft with its app and purchase items. Do not claim the recording or sandbox purchase is complete until actually verified. The September 21 submission above is already Waiting for Review; read its current state before making further changes.
