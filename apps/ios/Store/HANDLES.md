# Annual handle subscriptions

Each name has its own auto-renewable, one-year subscription. The US price is $8 per
name per year; ten active subscriptions total $80 per year. Other storefronts use
Apple's equalized local prices, shown by StoreKit. Each name is managed and cancelled
separately in Apple's subscription settings. A subscription remains associated with
the name it registered.

The existing product is `dev.pigeonpost.inbox.handle.yearly`. Slots 2 through 10 use
`dev.pigeonpost.inbox.handle2.yearly` through `dev.pigeonpost.inbox.handle10.yearly`.
Every slot has a separate subscription group so buying another name does not replace
the first subscription. `.github/scripts/ios_handle_catalog.py` provisions missing
slots for App Store Connect app `6803521541`, preserves the original product and
existing prices, and verifies an exact $8 US annual price. It does not submit an app
or subscription for public review.

The postbox must list these ten IDs, in order, in `PIGEONPOST_APPSTORE_PRODUCT_IDS`.
Keep `PIGEONPOST_APPSTORE_PRODUCT_ID=dev.pigeonpost.inbox.handle.yearly` for older
clients and migrated purchases. Without the list, only the original product is
accepted. Existing App Store credentials and bundle configuration still apply.

`GET /v1/claims/apple` returns the legacy first-active `namespace`/`expires_at` plus
the catalog, limit, stable account UUID and all owned subscriptions. The app passes
that UUID to Apple's `appAccountToken`; the server checks it against the signed-in
account. New additional products require this association. Older original-product
purchases remain restorable by their stored account and original transaction ID.

`POST /v1/claims/apple` accepts a transaction ID and an optional namespace. A bound
transaction always restores its existing name. An unbound purchase with no name
returns `name_required`; the client asks for a name and completes registration
without charging again. Names are checked before opening Apple's purchase sheet,
and checked again transactionally on the server. A race for a name can still occur
while the Apple sheet is open; the paid transaction remains unfinished until another
available name is registered. Pending approval stores the intended name per account
and product on the device. Reinstalling before registration may require choosing the
name again; restoring never invents a placeholder name.

The server enforces at most ten active Apple names in one SQLite transaction.
Renewals do not consume another slot. The schema adds `apple_subscriptions.product_id`;
legacy rows retain ownership and default to the original product. Older receipts
cannot shorten the paid-through date. Apple bearer JWTs are cached for 15 minutes,
shorter than their 20-minute validity.

Validation commands:

- `sh apps/ios/Tests/run.sh`: existing model/auth tests and purchase controller tests.
- `xcodebuild test -project apps/ios/UITests/PPUITests.xcodeproj -scheme PPUITests -destination 'id=<dedicated-simulator>'` after installing a Debug app on that simulator.
- `cargo test -p pigeonpost-postbox` and `cargo clippy -p pigeonpost-postbox --all-targets -- -D warnings`.

The UI tests include a local StoreKit configuration and exercise the shipped Apple
adapter through `StoreKitTest`: ten independent purchases, account token propagation,
restoration and expiry of one subscription. The local configuration and deterministic
UI fixtures do not override StoreKit in a Release build. TestFlight uses Apple's
sandbox purchase environment and does not charge real money.

For rollout, audit/provision the catalog, back up the server database, validate the
new binary against a database copy, deploy the postbox and preserve its Docker health
probe, then upload the signed iOS build. Keep the previous containers for rollback;
the additive migration is compatible with the previous binary. Never replace the
live database with an older snapshot during a routine binary rollback.
