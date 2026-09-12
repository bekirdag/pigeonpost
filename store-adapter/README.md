# Pigeonpost store adapter

The small service that sits between the account page (`site/account.js`) and MASAAS. It follows the
same pattern as the `theneuralledger` adapter: the browser holds the customer's own OIDC token, and
this service **forwards member billing operations to MASAAS with that token** plus an
`x-product-slug` header. The catalog is public. An optional **runtime token** supports entitlement
reads; it is not required by the current account and checkout routes.

**No payment-gateway keys live here.** MASAAS holds the Stripe (or iyzico) keys and hosts the card
capture. The buy flow creates a subscription in MASAAS and redirects the browser to the hosted
payment URL MASAAS returns.

## Run

```bash
node src/server.js
```

Dependency-free — Node's built-in `http` and `fetch`, Node ≥ 18.

## Configuration (environment)

Mirrors the theneuralledger adapter so an operator wires it the same way.

| Var | Meaning |
| --- | --- |
| `MASAAS_API_URL` | Control-plane API, default `https://api.masaas.org/v1` |
| `MASAAS_PRODUCT_APP_URL` | Member app base, default `https://app-pigeonpost.masaas.org` |
| `MASAAS_SAAS_API_URL` | Member SaaS backend; defaults to `<app url>/saas-api` |
| `MASAAS_PRODUCT_SLUG` | `pigeonpost` |
| `MASAAS_RUNTIME_TOKEN` | Runtime/service token for entitlement reads; member billing uses the customer's token |
| `MASAAS_PLAN_SLUG` | Handle price plan, default `handle-yearly-annual-usd` |
| `OIDC_ISSUER` | `https://sso.sealunit.com/realms/pigeonpost` |
| `OIDC_CLIENT_ID` | The store's OIDC client |
| `OIDC_CLIENT_SECRET` | Empty for a public (PKCE) client |
| `PIGEONPOST_POSTBOX_URL` | Handle availability and ownership, default `https://postbox.pigeonpost.dev` |
| `PIGEONPOST_NAMESPACE_GRANT` | Service credential matching the postbox's `NAMESPACE_GRANT_TOKEN`; required before checkout |
| `STORE_ALLOWED_ORIGINS` | CORS allowlist; first origin is the payment return origin, default `https://pigeonpost.dev` |

`/healthz` reports `configured:true` when the API, postbox, OIDC and browser-origin URLs are valid,
the product/plan/client identifiers are present, and `PIGEONPOST_NAMESPACE_GRANT` is set so paid
handles can be delivered. The optional runtime token does not affect this flag. This checks local
configuration completeness; it does not verify upstream availability, credentials or payments.

## Routes the store calls

| Method | Path | Does |
| --- | --- | --- |
| GET | `/healthz` | Liveness + whether live wiring is present |
| GET | `/v1/packages` | Public catalog from MASAAS |
| GET | `/v1/handles/:name/availability` | Authoritative postbox availability read |
| POST | `/v1/auth/exchange` | OIDC code → member session (client secret, if any, stays here) |
| GET | `/v1/subscriptions` | The signed-in customer's handles |
| POST | `/v1/checkout` | Start or resume payment for `{handle, operationId}`; operationId is 32 random hex characters retained for retries |
| GET/POST | `/v1/checkout/callback` | Bridge the bank's return to the account page; this does not authorize delivery |
| POST | `/v1/checkout/complete` | Verify `{subscriptionId, paymentId}` with the member's token, complete payment and deliver the handle |
| POST | `/v1/handles/claim` | Reconcile an already paid handle with postbox ownership |
| POST | `/v1/subscriptions/:id/cancel` | Cancel |

## Checkout contract and deployment

Deploy the backend's `20260910120000_subscription_external_reference` migration and API support
before this adapter. Subscriptions store `external_reference: pigeonpost:handle:<name>` at creation.
The backend rejects arbitrary `metadata`; card setup is separate from subscription payment.

The create response either supplies `payment_action_required` with a hosted bank URL or a paid
subscription. Existing pending payments are resumed from their member-scoped payment records;
they are never passed to `retry-payment`, which would start a second charge. Bank completion
re-reads the subscription and its payment, resolving the session from the stored payment record.

Delivery requires the exact reference and configured plan, `active` status, and a future paid
period. `past_due`, `trialing`, missing status, and expired terms cannot grant a handle. The
postbox grant uses that paid term as the namespace expiry and can be retried without a payment.

Run `node --test test/*.test.mjs` from this directory. The tests use local HTTP fixtures
and exercise the actual adapter routes without contacting a payment gateway.
