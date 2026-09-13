# Android handle subscriptions

Pigeonpost sells one handle per annual subscription, with ten independently renewable slots per Pigeonpost account. U.S. pricing is USD 8.00 per slot per year, or USD 80.00 per year for all ten. Google Play supplies localized prices, tax treatment, checkout, cancellation, and payment details. The app never accepts card details.

## Play catalog

- Package: `dev.pigeonpost.inbox`.
- Products: `pigeonpost.handle.01` through `pigeonpost.handle.10`.
- Base plan on every product: `annual`, auto-renewing, `P1Y`.
- Enable all supported purchase regions, including France, and future regions.
- Set the U.S. regional price to exactly USD 8.00. Google's bulk conversion can round an entered USD 8 to USD 7.99, so verify the saved U.S. row on every product.
- Allow normal annual renewal and restoration. Disable expired-subscription resubscription outside the app (`RESUBSCRIBE_STATE_INACTIVE`): a new purchase must start in Pigeonpost so its signed-in account binding is supplied.
- No introductory offer is required. The client selects only the ordinary, undiscounted annual base plan.

Internal-test enrollment does not make purchases free. Add consenting test accounts to Play Console's **license testing** settings and use the Google test payment instrument for purchase acceptance tests. The existing server-approved complimentary handle is a separate flow and remains free.

## Server setup

Enable the Google Play Android Developer API in the company's Cloud project and give a dedicated service account access to this app only: app information, financial data, and orders/subscriptions. The runtime does not need catalog editing, release publishing, or account administration permissions.

Mount its JSON credential read-only into the postbox container and set:

```text
GOOGLE_PLAY_SERVICE_ACCOUNT_FILE=/run/secrets/google-play-service-account.json
```

The file must be readable by the container's runtime user and private on the host. Keep it out of Git, app bundles, CI artifacts, logs, and client configuration. A missing or invalid credential disables the paid catalog; it never grants purchases without verification. Provider failures do not invent an expiry or authorize a name.

The additive SQLite migration creates `google_subscriptions`. Back up the live database, validate the candidate against a copy, and preserve the existing environment, mounts, health checks, log limits, and Apple/tester configuration when replacing the postbox and reaper containers. Roll back the containers and environment if necessary; do not overwrite a live database with an older snapshot after new messages arrive.

## Purchase and recovery behavior

`GET /v1/claims/google` returns the ten product IDs, supported base plan, account hash, and current paid handles. `POST /v1/claims/google` redeems a purchase token. `POST /v1/claims/google/assign` assigns an already-paid, unassigned slot. All three require the member's OIDC token; mailbox capability tokens cannot manage purchases.

The billing flow sends a domain-separated SHA-256 account hash as Google's obfuscated account ID. The server obtains the authoritative subscription from the Publisher API and checks that account binding, package, product, annual plan, payment state, and confirmed expiry. Pending, held, paused, expired, mismatched, or replaced purchases do not grant a new active handle. Cancellation retains service only until Google's confirmed paid-through expiry.

The account/product binding and ten-slot limit are enforced in one SQLite transaction. A successful payment is retained as an unassigned slot if the requested name becomes unavailable. The user can choose another name without another checkout. Reinstalling or process death restores purchases through Google; an interrupted name selection can be completed afterward. Linked purchase tokens replace their predecessor, and a late response for the old token cannot revoke the replacement.

The server acknowledges purchases only after durable entitlement storage. Restoration and background reconciliation retry acknowledgement. The worker checks due subscriptions every minute, with a five-minute minimum between checks of the same purchase, in batches of 100. It reconciles renewal, grace, hold, cancellation, and revocation; provider outages retain only the last confirmed expiry. Purchase tokens are not serialized to the client in catalog responses or logged.

## Validation

Before a public rollout, use a license tester enrolled in the internal release to verify:

1. All ten annual prices load, the U.S. price is USD 8.00, and a French account can see its local price.
2. A test payment registers the requested handle and opens its inbox. Confirm the server stores and acknowledges the verified receipt.
3. Pending payment grants nothing. Canceling checkout grants nothing and allows another attempt.
4. Reinstall and restore retain the same handle. An interrupted purchase/name assignment can be completed without a second charge.
5. A different Pigeonpost account cannot redeem that purchase. Ten active slots prevent an eleventh checkout.
6. Cancellation, accelerated test renewal, expiry, and refund/revocation reconcile to the correct access state.

Automated coverage lives in `PaidHandleStoreTest`, `PaidHandleUiTest`, and the Rust `googleplay`/`store::googleplay` tests. The release build must also pass Android lint and Rust Clippy. Emulator fixtures and synthetic provider payloads do not replace acceptance of a real Google Play test receipt.

References: [Billing integration](https://developer.android.com/google/play/billing/integrate), [server verification](https://developer.android.com/google/play/billing/security), [subscription lifecycle](https://developer.android.com/google/play/billing/lifecycle/subscriptions), [Publisher API setup](https://developers.google.com/android-publisher/getting_started).
