# Pigeonpost billing bridge (store adapter)

The service behind `https://pigeonpost.dev/api`. The account page (`site/account.js`), and later
the enterprise portal, call it. It:

- forwards member billing calls to saas_be (MASAAS) with the **customer's own OIDC token**, plus
  `x-product-slug: pigeonpost`;
- starts **iyzico abonelik hosted checkouts** through saas_be. iyzico hosts the card form and
  saas_be holds the merchant keys, so **no card data and no gateway key reach this service**;
- activates paid plans in the postbox through the postbox's **internal entitlement API**.

Dependency-free: Node's built-in `http` and `fetch`, Node ≥ 18. `npm test` runs every test
against local fixtures for saas_be and the postbox; nothing calls a real provider.

## Catalog

`catalog/pigeonpost-catalog.json` is the approved price list. It holds:

- 93 plan codes:
  - `cloud-<n>names-yearly-<usd|eur|try>` (n = 1–10);
  - `ent-<seats>-<monthly|yearly>-<usd|eur|try>` (5, 10, 15, 20, 30, 50, 100, 200, 500 seats);
  - `nest-<ai-starter|ai-plus|host-starter>-monthly-<usd|eur|try>`. Their prices are proposals
    pending owner approval. They are provisioned in saas_be (product `PP_NEST`, 2026-11-05) and stay
    unsold while the postbox `nests` flag is off;
  USD and EUR are fixed prices with one edition (2026) that stays on sale every year; TRY has one
  edition per calendar year;
- for each code, the editions provisioned in saas_be: pricing year, amount, local plan id and iyzico
  pricing-plan reference.

saas_be's `scripts/pigeonpost-regional-catalog.cjs` provisions the editions, and its output fills
the ids. Every offer, subscription and access-list row is checked against this file. An edition not
listed here (a new year, a changed price) **fails closed** until someone reviews it and adds it.

## Flows

**Individual (Pigeonpost Cloud)**

1. `POST /v1/cloud/checkout {intent_id, plan_code, billing_profile_id, locale}`. The page generates
   `intent_id` (32 hex characters) once per purchase and reuses it for retries.
2. The bridge resolves the edition through saas_be's regional offer and checks it against the
   catalog.
3. It starts `POST /v1/integrations/iyzico/subscriptions` with:
   - `Idempotency-Key: pp-cloud-<intent>`;
   - `client_reference_id: pp-cloud:<account>:<intent>`;
   - `return_url: https://pigeonpost.dev/account?checkout=<intent>`;
   - `method: hosted`, trial 0.

   It returns `checkout_form_content`, which `/billing/checkout` renders. The `:<intent>` suffix is
   needed because saas_be keeps an abandoned hosted form reserved under its client reference, so a
   bare `pp-cloud:<account>` would block a retry.
4. After the return, `GET /v1/cloud/checkout/<intent>` reads saas_be's authoritative state with
   `?refresh=true`. A provider-confirmed paid period is pushed to the postbox:
   `PUT /internal/v1/entitlements/iyzico/live/<saas subscription id>`, with `names`, `paid_through`
   and a version that only increases.
5. Management, all keyed by a fresh `intent_id` per operation, under `/v1/cloud/subscriptions/<id>/…`:
   - `cancel` — access is kept through the paid period;
   - `retry` — only a failed order of that subscription;
   - `card-update` — hosted form;
   - `upgrade` — same product, currency and interval. A plan below the names already registered is
     refused.

**Nest packages** (Nests Phase B1; behind the postbox's `nests` flag)

The same individual flow as Cloud, under `/v1/nests/…`:

- `GET /v1/nests` — the member's Nest subscriptions, the postbox's Nest view, and the Nest prices.
  `prices` is `null` unless the postbox shows Nests to this account. Nest prices are kept off the
  public `/v1/catalog` while the owner has not approved them.
- `POST /v1/nests/checkout {intent_id, plan_code, billing_profile_id, locale}` with a
  `nest-<ai|host>-<tier>-monthly-<usd|eur|try>` plan. Before any provider call it posts
  `POST /v1/nest-purchases/intents {package, rail:"web"}` to the postbox with the member's own
  token. The postbox refuses when:
  - the flag is off;
  - a package bought in the App Store or Google Play already funds the account;
  - the account already holds that package.

  The checkout then runs with `client_reference_id: pp-nest:<account>:<intent>` and returns to
  `/account?nest_checkout=<intent>`.
- `GET /v1/nests/checkout/<intent>` reads saas_be's authoritative state and activates the account.
- `/v1/nests/subscriptions/<id>/{cancel|retry|card-update|upgrade}` manage the subscription:
  - a larger package applies `NOW`;
  - a smaller one applies `NEXT_PERIOD`. The postbox pauses Nests above a smaller package; it never
    deletes them.

The pushed entitlement has the web plan code and no names. The postbox maps it to the package's
slots, storage and model allowance. The reconciler activates `pp-nest:` references from the access
list, as it does for Cloud.

**Enterprise** (the Phase 4 portal calls these; exercised by tests now)

- `POST /ent/billing-profile` creates the profile.
- `POST /ent/checkout {intent_id, plan_code, billing_profile_id, org:{name, namespace}}`:
  1. creates the postbox org draft;
  2. starts the hosted checkout with `client_reference_id: pp-org:<draft>`;
  3. returns to `https://enterprise.pigeonpost.dev/checkout/return?d=<draft>`.
- `GET /ent/checkout/<draft>` activates the org on a verified payment.
- Both refuse with `403 feature_disabled` while `orgs` is off for the buyer (read from the postbox's
  `/v1/capabilities`); the postbox refuses the draft and the activation too.
- `GET /ent/orgs/<org>/subscription` — the org's card plan, its failed payments and the seat tiers it
  can move to (same currency and interval). The postbox's `/v1/orgs/<org>/billing` decides who sees it.
- `POST /ent/orgs/<org>/subscription/{change|cancel|card-update|retry}` — owner or billing admin
  (`can_manage` from the postbox) **and** the buyer's own token (saas_be scopes the subscription to
  the person who bought it; anyone else gets `403 not_buyer`):
  - `change {intent_id, plan_code}` — iyzico Subscription V2 upgrade through saas_be. More seats
    apply `NOW`; fewer seats apply at `NEXT_PERIOD`. A tier below the seats or agents in use is
    refused with `409 seats_in_use`; a second pending change with `409 change_pending`;
  - `cancel` — renewals stop, the paid period stays;
  - `card-update {intent_id, locale?}` — iyzico's card-update form (subscription scope), returning to
    `https://enterprise.pigeonpost.dev/checkout/return?card=<org>`;
  - `retry {intent_id, order_reference_code}` — a failed order of this subscription only.

**Marketplace purchases** (paid third-party listings; behind the postbox's `marketplace` flag;
`src/marketplace.js`)

Launch terms are one-off and monthly only: no wallet, credits or per-job prices. saas_be's
marketplace finance module is the money authority (offers, quotes, the hosted iyzico form, single-use
payment authorizations, licences, invoices, journal), reached at
`/v1/internal/tenants/<tenant>/marketplace` with a `marketplace:bridge` token. The postbox owns
listings, publisher prices and installations.

- Offers: the postbox calls `PUT /internal/marketplace/offers/{offer_id}` here (private link,
  `BRIDGE_INTERNAL_TOKEN`) with `listing_id, publisher_id, release_line, title, term, currency,
  price_minor, status`. The bridge forwards it to saas_be `PUT /offers/{id}` and provisions an active
  monthly offer once (`POST /offers/{id}/provision`). saas_be's 422 becomes
  `422 price_not_invoiceable` with `nearest` (read from `details.nearest`); a currency outside
  `OPEN_CURRENCIES` is `409 currency_unavailable`. The postbox derives offer ids from the immutable
  fields, so any price, term, currency or release-line change is a new id, and re-sends the old one
  with `status: retired`.
- Buy: `POST /v1/marketplace/checkout {intent_id, listing_id, owner?: {kind: account|org, id},
  billing_profile_id, locale}`:
  1. owner check: the caller's own account, or an org whose billing the caller manages (`can_manage`
     from the postbox's `GET /v1/orgs/{org}/billing`, as for the enterprise plan);
  2. the postbox's `POST /v1/marketplace/purchases` with the member's token: listing checks, the
     installation id the payment admits, the offer and the exact intent;
  3. the EU/UK VAT rule (`src/tax.js`, D-34), exactly as for Nest checkouts;
  4. saas_be `GET /v1/profile` (the buyer's `user_id`), `POST /quotes`, then
     `POST /quotes/{id}/checkout` with `return_url: https://pigeonpost.dev/account?mp_purchase=<intent>`
     and `buyer_ip` (one-off). saas_be's callback returns there with `&mp_checkout=<checkout id>`.

  Same 5-per-hour budget as other checkouts. A replay of the intent returns the same checkout (the
  form is kept in memory only, never on disk).
- `GET /v1/marketplace/checkout/{intent}`: the authorization read with `refresh=true`; `status` is
  `pending`, `paid`, `installed`, `failed`, `expired` or `revoked`, with `licence_id` and the receipt
  wording.
- Install: the web calls the postbox's `POST /v1/nest-installations` with `purchase_id`. The postbox
  calls `POST /internal/marketplace/authorizations/consume {purchase_id, intent, consumer_reference}`
  here. The bridge refuses an intent or reference other than the one the payment was bound to
  (`409 intent_mismatch`), keeps the single-use `mpa_` token itself, reissues it once when a paid
  authorization's window lapsed, and maps unpaid states to `402 payment_pending`, `payment_failed`,
  `payment_expired`, `payment_revoked`. saas_be's consumption is idempotent by the reference (the
  installation id): a replay answers `replay: true`.
- Buyer routes (member token): `GET /v1/marketplace/licences?owner_kind&owner_id&listing_id`;
  `GET /v1/marketplace/installations/{installation}` (licence, payments with `invoice_id` for
  `GET /v1/billing/invoices/{id}/download`, receipt); `POST /v1/marketplace/installations/{installation}/cancel`
  (monthly only; cancels at period end and projects at once).
- Reconciler: on every pass, for each installed purchase, saas_be `GET /licences/{id}`; when it
  changed, or a deletion boundary is due, `PUT /internal/v1/marketplace/licences/{id}` on the postbox
  (`POSTBOX_SERVICE_TOKEN`, increasing `reconciliation_version`). The postbox stops the worker without
  access, lets it resume with access, and uninstalls and removes the worker and its data at
  `deletion_due_at`.
- Subscriptions whose `external_reference` is `mp-checkout:<id>` (or whose plan is named
  `marketplace:…`) are hidden from the Cloud, Nest and enterprise plan logic and from the access list.
- Finance routes (payouts, refunds, chargebacks, statements, journal; scope `marketplace:finance`) are
  staff tools. The bridge holds no finance token and calls none of them.
- The `/internal/marketplace/*` routes answer 404 under the public `/api` prefix.

**Missed returns.** Every 60 s the reconciler reads saas_be's internal access list:
`GET /v1/internal/tenants/pigeonpost/subscription-packages`, using `SAAS_INTERNAL_TOKEN`. It
activates and renews `pp-cloud:` and `pp-org:` subscriptions from that list, and passes the Keycloak
id as `subject_check`. A subscription that leaves the list keeps its verified `paid_through` and is
marked expired once that passes. Nothing here ever extends a paid period.

## Security

- No card data, checkout token or form content is stored. `STORE_STATE_DIR/billing-state.json`
  (mode 600) holds only intents and the last pushed entitlement. `marketplace-state.json` (mode 600)
  holds marketplace purchases, their ids and the single-use payment-authorization token the postbox
  consumes through this service.
- Every saas_be response is validated: ids, statuses, mode, amounts, dates and plausible periods.
  Every mutation sends an idempotency key.
- Limit: 5 new checkouts per account per hour. Replays of an intent do not count.
- Return URLs are built here, on the exact configured origins. The browser sends only a plan code
  and an intent id.
- Request bodies over 64 KiB are refused. Upstream messages are bounded and stripped of tokens.

## Configuration (environment)

| Var | Meaning |
| --- | --- |
| `MASAAS_PRODUCT_APP_URL` / `MASAAS_SAAS_API_URL` | saas_be base, default `https://app-pigeonpost.masaas.org/saas-api` |
| `MASAAS_PRODUCT_SLUG`, `MASAAS_TENANT_ID` | `pigeonpost` |
| `SAAS_INTERNAL_TOKEN` | saas_be's internal adapter token (`API_KEY_INTROSPECTION_TOKEN`), used read-only for the access list |
| `IYZICO_MODE` | `live` (or `test`). Every binding must report this mode. |
| `OIDC_ISSUER`, `OIDC_CLIENT_ID`, `OIDC_CLIENT_SECRET` | Member sign-in (PKCE public client by default) |
| `PIGEONPOST_POSTBOX_URL` | Public postbox, default `https://postbox.pigeonpost.dev` |
| `POSTBOX_INTERNAL_URL`, `POSTBOX_SERVICE_TOKEN` | Postbox internal API (private link) and this service's `svc_` token |
| `STORE_ALLOWED_ORIGINS` | Exact https origins. The first is the individual-checkout return origin. |
| `STORE_PUBLIC_REQUESTS_PER_MINUTE` | Per-address budget for the unauthenticated sign-in exchange/refresh and handle-availability routes (default 60). |
| `ENTERPRISE_ORIGIN` | Enterprise return origin, default `https://enterprise.pigeonpost.dev` |
| `STORE_STATE_DIR` | Durable state, default `/var/lib/pigeonpost-store-adapter` (systemd `StateDirectory=`) |
| `CHECKOUTS_PER_HOUR`, `RECONCILE_INTERVAL_MS` | 5 and 60000 |
| `OPEN_CURRENCIES` | Currencies new checkouts may use, e.g. `TRY` or `USD,EUR,TRY`. **Unset means `TRY`.** Production is `USD,EUR,TRY` since 2026-11-04 (iyzico Multi Currency add-on active; no-charge probe passed). To open or close a currency, back up `/etc/pigeonpost-store-adapter.env`, edit it and restart `pigeonpost-store-adapter`; the one-command runbook is in saas_be `docs/ops/iyzico-subscriptions.md` ("Enable runbook"). The account page and the enterprise portal offer only listed currencies (the portal hides EUR entirely until it is listed). |
| `SALES_NOTIFY_URL`, `SALES_NOTIFY_KEY` | saas_be base URL and the platform notification key (the postbox's `POSTBOX_NOTIFY_KEY`). With both set, each enterprise "Talk to sales" request is emailed through saas_be's `pigeonpost-notice` workflow (Novu → Postmark server "Pigeonpost", no tracking). `https://`, or `http://` to loopback only. Unset: requests are only appended to `sales-requests.jsonl`, which stays the durable record either way. Sending is in the background, up to 4 attempts (`SALES_NOTIFY_DELAYS_MS`, default `0,30000,120000,600000`); `/api/healthz` shows `sales_notify` counters. |
| `SALES_NOTIFY_TO`, `SALES_NOTIFY_FROM` | Recipient (default `info@pigeonpost.dev`) and sender (default `no-reply@pigeonpost.dev`) of the sales notification sent through saas_be. |
| `TAX_EU_UK_GATE`, `TAX_B2C_OPEN_REGIONS`, `TAX_IP_COUNTRY_HEADER`, `HMRC_CLIENT_ID`/`HMRC_CLIENT_SECRET` | EU/UK VAT rule for web sales (`src/tax.js`): EU/UK consumers are refused until a region is opened; EU/UK businesses need a VIES/HMRC-valid VAT number (reverse charge). Defaults: gate on, no region open, `cf-ipcountry`, no HMRC credentials (UK businesses refused). See `docs/billing-tax.md`. |
| `SAAS_MARKETPLACE_TOKEN` / `SAAS_MARKETPLACE_TOKEN_FILE` | saas_be tenant service token for `MASAAS_TENANT_ID`, scope `marketplace:bridge` only (its SHA-256 goes in saas_be's `INTERNAL_TENANT_SERVICE_TOKENS`). Never the `marketplace:finance` token. |
| `BRIDGE_INTERNAL_TOKEN` / `BRIDGE_INTERNAL_TOKEN_FILE` | What the postbox presents on `/internal/marketplace/*` (`POSTBOX_BRIDGE_TOKEN` there), at least 32 characters. The marketplace routes answer 404 until both marketplace tokens are set. |
| `MARKETPLACE_PROVISION_MONTHLY` | `on` (default) provisions monthly offers in saas_be right after mirroring; `off` leaves them unsellable. |

`/healthz` reports `configured:true` when these are present and well formed. It checks local
configuration only, not whether the upstreams are reachable.
