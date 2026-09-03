# Cubemeld Gold IAP review-metadata handoff

Last verified: 2026-09-03

## Finding

The evidence isolates **App Review Screenshot** as the only missing required field for the five
Cubemeld Gold consumables. Pigeonpost workflow
[run 33656773190](https://github.com/bekirdag/pigeonpost/actions/runs/33656773190) was a read-only
audit of the Wodo App Store Connect account. It found all five products in `MISSING_METADATA`,
while independently
verifying every other broker-managed requirement:

| Field | Read-only evidence |
| --- | --- |
| App identity | `Cubemeld`, bundle `com.wodo.gamehub` |
| Product identity and type | All five exact product IDs exist as `CONSUMABLE` |
| Reference name and review note | Exact for all five; no drift finding |
| Customer-facing metadata | Exact `en-US` name and description on version 1 |
| Availability | All 175 returned territories and future territories |
| Price | Exact current USA prices: USD 0.99, 4.99, 19.99, 49.99, and 99.99 |
| App Review Screenshot | No repository artifact exists and the old broker did not manage this relationship |

The old audit did not query Apple's screenshot relationship, so this conclusion is exhaustive
elimination rather than a captured 404 response: every other required field was exact, and no
review artifact exists in the release repository. Apple identifies the App Review Screenshot as
the review-only image that must clearly show the
in-app item or service. Apple also describes uploading that image as the first step after an IAP is
configured and before it can be submitted. The enhanced audit now reads
`GET /v2/inAppPurchases/{id}/appStoreReviewScreenshot` for each product, so its next credentialed,
read-only run will distinguish `missing`, an incomplete/failed upload, and a fully processed asset
directly. No credentialed rerun was dispatched as part of this code-only audit.

Official references:

- [In-App Purchase information](https://developer.apple.com/help/app-store-connect/reference/in-app-purchases-and-subscriptions/in-app-purchase-information)
- [Managing in-app purchases](https://developer.apple.com/documentation/appstoreconnectapi/managing-in-app-purchases)
- [Read review screenshot information for an in-app purchase](https://developer.apple.com/documentation/appstoreconnectapi/get-v2-inapppurchases-_id_-appstorereviewscreenshot)
- [Uploading assets to App Store Connect](https://developer.apple.com/documentation/appstoreconnectapi/uploading-assets-to-app-store-connect)

## Human-owned artifact still required

There is no legitimate Cubemeld review screenshot in the current
`bekirdag/cubeio_ios_game` release branch. The broker must not invent one. A person needs to
capture the real, shipping purchase flow with the Gold packages visible and confirm that the image
clearly shows every product for which it will be used.

The recommended artifact is an unedited, flattened RGB PNG captured from the release app in an
Apple-supported iPhone screenshot size. It must contain no alpha channel or transparency. Apple
publishes the current dimensions in its
[screenshot specification](https://developer.apple.com/help/app-store-connect/reference/app-information/screenshot-specifications/).

Commit the capture in the Cubemeld repository as, for example:

```text
fastlane/review/cubemeld-gold-purchase.png
```

Then commit `fastlane/iap_review_screenshot.json` alongside the existing IAP catalog:

```json
{
  "schemaVersion": 1,
  "file": "review/cubemeld-gold-purchase.png",
  "sha256": "<lowercase SHA-256 of the exact PNG bytes>",
  "productIDs": [
    "com.bekirdag.cubeio.gold.100",
    "com.bekirdag.cubeio.gold.1000",
    "com.bekirdag.cubeio.gold.10000",
    "com.bekirdag.cubeio.gold.100000",
    "com.bekirdag.cubeio.gold.1000000"
  ]
}
```

The one-image strategy is legitimate only if that single real purchase screen clearly shows all
five packages. Otherwise, the broker contract must be intentionally extended to map a separate
capture to each product; do not claim coverage in the manifest that the pixels do not provide.

## Broker behavior

The broker now fails closed:

- Audit mode uses GET requests only and exits nonzero for a missing or incomplete required field.
- Apply mode inventories every screenshot relationship before its first mutation.
- When an upload is needed, apply mode requires the manifest and PNG to be regular files tracked at
  the checked-out Cubemeld `HEAD`, byte-identical to Git, and SHA-256 pinned.
- The PNG parser verifies the signature, chunk CRCs, decoded image length, RGB/no-alpha encoding,
  and an Apple-supported iPhone dimension.
- The broker reserves one screenshot resource per IAP, validates Apple's complete nonoverlapping
  upload-operation byte cover, sends the chunks without the App Store Connect bearer token,
  commits Apple's required MD5 checksum, and waits for asset state `COMPLETE`.
- Upload URLs are restricted to Apple's HTTPS blob-store host. Signed query strings and asset
  error descriptions are never logged.
- An existing `AWAITING_UPLOAD`, `UPLOAD_COMPLETE`, or `FAILED` resource is never silently deleted,
  replaced, or reported ready.

The protected `Cubemeld App Store Gold Catalog` workflow continues to require a manual `apply`
choice and its `production-release` environment. It now also requires the exact lowercase
40-character Cubemeld source commit, then verifies that the nested checkout resolved to that SHA.
This change does not dispatch that workflow and does not mutate App Store Connect.

## Submission boundary

Uploading the missing screenshot makes the metadata complete; it does not submit a product for
review. Apple requires the **first in-app purchase** to be submitted with a new app version through
App Store Connect. That first-version association is an external human release action and is not
automated by this broker. Apple documents that boundary in
[Submit an In-App Purchase](https://developer.apple.com/help/app-store-connect/manage-submissions-to-app-review/submit-an-in-app-purchase)
and in its API's [Managing in-app purchases](https://developer.apple.com/documentation/appstoreconnectapi/managing-in-app-purchases)
guide.

Therefore the exact remaining sequence is:

1. Capture, inspect, and commit the real Cubemeld Gold purchase-flow screenshot plus checksum
   manifest in `cubeio_ios_game`.
2. Review and manually authorize the protected broker workflow with `source_ref` set to that exact
   commit and `apply=true`; verify that all five assets reach `COMPLETE` and a subsequent read-only
   audit is green.
3. If these are Cubemeld's first IAPs, attach them to the next app-version submission in App Store
   Connect and submit that version for review.
