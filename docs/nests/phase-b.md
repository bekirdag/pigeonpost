# Pigeonpost Nests — Phase B (first-party Nests)

**Status:** in progress behind the `nests` flag (off in production). No sales: commercial launch
needs the owner's approval of the catalog and prices, updated privacy pages and store review.
**Plan:** `docs/planning/2026-11-03-nests-marketplace-build-plan.md` §8 (local planning folder).
**Before:** [phase-a-plan.md](phase-a-plan.md) (runtime, broker, enrollment), [backups.md](backups.md).

## B1 — purchases and entitlements

### Catalog

`crates/pigeonpost-postbox/src/nest_catalog.rs`, edition `nests-2026-11`. A package is one monthly
subscription; every rail maps onto the same table.

| Package | Slots | Storage (pooled) | Hours/month (pooled) | Managed AI / month (pooled) | Offered |
|---|---|---|---|---|---|
| `nest.ai.starter.monthly` — Pigeonpost AI | 1 | 5 GiB | 10 | 1M input + 100k output | yes |
| `nest.ai.plus.monthly` — Pigeonpost AI Plus | 3 | 20 GiB | 40 | 5M + 500k | yes |
| `nest.ai.pro.monthly` — Pigeonpost AI Pro | 10 | 100 GiB | 120 | 15M + 1.5M | no: does not fit nests-1 |
| `nest.host.starter.monthly` — Empty Nest | 1 | 5 GiB | 10 | none (no model connected) | yes |

| Rail | Product |
|---|---|
| App Store | `dev.pigeonpost.inbox.nests.<ai\|host>.<tier>.monthly` in the **Nests** subscription group (separate from the handle group) |
| Google Play | subscription `pigeonpost.nests.<ai\|host>.<tier>`, base plan `monthly` |
| Web (iyzico Subscription V2 via the billing bridge) | plan codes `nest-<ai\|host>-<tier>-monthly-<usd\|eur\|try>` |

Prices are not in the code. Store prices live in App Store Connect and Play Console, web prices in
the billing bridge catalog; all are **proposals pending owner approval**. Model allowances are split
evenly across a package's slots (the broker enforces per-Nest budgets), never multiplied by them.
Workspace hours are shown, not yet metered (B5).

### Flow

1. The client reads `GET /v1/nest-packages` (catalog, available rails, the account's StoreKit
   `appAccountToken` and Google `obfuscatedAccountId`).
2. Before any purchase sheet: `POST /v1/nest-purchases/intents {package, rail}`. It refuses a second
   charge: a live package bought on another rail (`nest_package_active`) or the same package again
   (`nest_package_owned`). Upgrades on the funding rail are allowed.
3. The store purchase carries the binding token. The client sends the transaction id
   (`POST /v1/nest-purchases/apple`) or purchase token (`POST /v1/nest-purchases/google`); the
   postbox verifies it with Apple/Google, checks the binding, records it in `nest_store_purchases`
   and mirrors it into `entitlements` (plan code = package code). Google purchases are acknowledged
   only after they are durable, and **not** when they duplicate a live package (Google refunds an
   unacknowledged purchase after three days). Web purchases go through the bridge, which writes
   `nest-…` entitlements with the internal API.
4. The first Nest is set up at once. If that fails (nestd down, no disk), the purchase stays as a
   **paid setup** (`nest_setups`), retried with backoff by the reconciler and by the owner
   (`POST /v1/nest-setups/{id}/retry`). One setup per funding entitlement, ever.

### Allowance and lifecycle

- Slots come only from live Nest entitlements; the flag gates the feature but never grants a slot.
  One package funds an account (highest rank wins); others are listed as `duplicates` and never add.
- A failed provisioning attempt holds no slot; the sweep tears its half-made runtime and mailbox down.
- Every 60 s the reconciler re-reads store subscriptions (each at most every 6 h) so renewals, grace,
  refunds and upgrades land while every app is closed; then the funding sweep:
  - over the allowance after a **downgrade**: the newest excess Nests pause (`over_capacity`), never deleted;
  - **no live package** (expiry, refund, revocation): every package Nest pauses (`unfunded`) at once;
    after a 30-day export window from the last paid-through it is deleted;
  - funding back (renewal, restore, upgrade): Nests paused for funding resume.
- Phase A Nests created through the ops path (`funding = ops`) are never touched by the sweep.

### Security notes

- Buying, restoring, creating and retrying need a person (`Requirement::Human`): never an API key,
  a mailbox token, a connector or an org key. Reading needs the account's own credential.
- The client never says what it bought or what it costs: product ids and plan codes are mapped by
  the server, unknown `nest…` codes are refused by the internal API.
- A store subscription is bound to the first account that proves it (`purchase_already_used`);
  Apple purchases must carry the account's `appAccountToken`, Google purchases its obfuscated id.
- Replaced (upgraded) Google tokens are retired and cannot be revived by a late verification;
  refunds cut the paid-through to the refund time.
