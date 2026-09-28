# Annual handle plans

Pigeonpost sells one auto-renewable yearly subscription with ten capacity levels in the **Pigeonpost handle plans** subscription group. Product IDs are `dev.pigeonpost.inbox.handles.1.yearly` through `dev.pigeonpost.inbox.handles.10.yearly`. The number is total included names, not an additional independent subscription. US annual prices are $8, $16, $24, $32, $40, $48, $56, $64, $72 and $80. StoreKit supplies localized names and prices. Level 1 is the ten-name plan; level 10 is the one-name plan.

Settings → Get a handle lists every plan and supports direct selection. Subscribe first and register included names, or enter an available name and subscribe/register in one flow. Included additional names never invoke another Apple payment. Upgrades replace the current plan immediately. Downgrades preserve existing capacity until Apple confirms the lower level at renewal. At that point, the first registered names remain active up to the lower capacity, and excess names enter the existing 30-day recovery period. Mailbox keys and message history remain with their account.

## Server contract and compatibility

`GET /v1/claims/apple` returns the sale catalog, stable account UUID, optional `plan` and all retained Apple names. `POST /v1/claims/apple` accepts an Apple transaction ID and optional name. With no name, it restores/synchronizes the plan. With a name, it registers a slot included in the verified capacity. Only Apple's authenticated server response establishes capacity; neither the app's selected level nor a client-supplied receipt can grant it. The `appAccountToken` must match the signed-in Pigeonpost account.

`apple_handle_plans` and `apple_plan_names` are additive tables, separate from legacy `apple_subscriptions`. One account has one capacity plan. Serialized verification and SQLite transactions prevent concurrent registration from exceeding the plan. Background reconciliation applies renewals, downgrades, expiry and revocation while the phone is closed. Provider failures retain the last confirmed entitlement and keep names reserved. Restoring a resold name never steals it from its new owner.

The old `dev.pigeonpost.inbox.handle.yearly` and `handle2.yearly` through `handle10.yearly` products are restore-only in the new app and must be removed from the new review submission and sale availability. Existing receipts and namespaces remain supported. Preserve the legacy `PIGEONPOST_APPSTORE_PRODUCT_ID` and `PIGEONPOST_APPSTORE_PRODUCT_IDS` configuration so those receipts remain verifiable. New capacity IDs are an exact compiled allowlist in both server and app; arbitrary server product IDs are not added to the purchase screen.

## Catalog and validation

`.github/scripts/ios_handle_catalog.py` audits by default and provisions with `APPLY=true`. It uses the legacy primary product as an initial storefront reference and the established first capacity plan after legacy retirement. It creates all new products in one group, verifies expected prices, and restricts purchasing to individual App Store purchases. `SKIP_REVIEW_SCREENSHOT=true` permits preparing pricing before capturing the new screenshot; the final apply must omit it and verify every uploaded review image. Never submit an incomplete catalog or the old independent-slot screenshot.

Run `sh apps/ios/Tests/run.sh`, the `NativeHandleStoreKitTests` and `HandlePurchaseTests` XCUITest suites, `cargo test -p pigeonpost-postbox`, and `cargo clippy -p pigeonpost-postbox --all-targets -- -D warnings`. The local StoreKit configuration has one group and tests replacement on upgrade, scheduled downgrade, restoration and expiry. It is not physical sandbox evidence.

Hosted CI runs the UI suite on the current iOS runtime and the native StoreKit test on iOS 18. The iOS 26.5 simulator has an [Apple-confirmed CLI StoreKit configuration failure](https://developer.apple.com/forums/thread/826971) that prevents `SKTestSession` from loading any products. The older-runtime job preserves every transaction assertion until the hosted image includes a fixed runtime.

Deploy the server first using the existing runtime secrets and mounts, backing up the database and keeping the old image for rollback. Upload a fresh signed build through `ios-testflight.yml` only after validation. Do not overwrite a live database with an older snapshot during rollback. Capture the matching real-device sandbox video and submit the app plus all ten new levels and their single group.
