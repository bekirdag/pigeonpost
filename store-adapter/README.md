# Pigeonpost store adapter

The small service that sits between the handle store (`site-store/`) and MASAAS. It follows the
same pattern as the `theneuralledger` adapter: the browser holds the customer's own OIDC token, and
this service **forwards member billing operations to MASAAS with that token** plus an
`x-product-slug` header. It uses a privileged **runtime token** only for catalog and entitlement
reads.

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
| `MASAAS_RUNTIME_TOKEN` | Runtime/service token — catalog + entitlement reads only. **Required for live mode.** |
| `OIDC_ISSUER` | `https://sso.sealunit.com/realms/pigeonpost` |
| `OIDC_CLIENT_ID` | The store's OIDC client |
| `OIDC_CLIENT_SECRET` | Empty for a public (PKCE) client |
| `PIGEONPOST_REGISTRY_URL` | For handle availability reads |
| `STORE_ALLOWED_ORIGINS` | CORS allowlist, default `https://store.pigeonpost.dev` |

With no `MASAAS_RUNTIME_TOKEN` the adapter runs in preview: `/healthz` reports `configured:false`,
catalog returns empty, member routes still require a token.

## Routes the store calls

| Method | Path | Does |
| --- | --- | --- |
| GET | `/healthz` | Liveness + whether live wiring is present |
| GET | `/v1/packages` | Public catalog from MASAAS |
| GET | `/v1/handles/:name/availability` | Registry read; registry is authoritative on rules |
| POST | `/v1/auth/exchange` | OIDC code → member session (client secret, if any, stays here) |
| GET | `/v1/subscriptions` | The signed-in customer's handles |
| POST | `/v1/checkout/session` | Create subscription in MASAAS, return the hosted payment URL |
| POST | `/v1/subscriptions/:id/cancel` | Cancel |

## What it does not do

Issue the handle on payment. The registry has no flat-handle namespace yet, and the
billing→registry binding is unbuilt. This proves the money path; name issuance is the separate
registry build.
