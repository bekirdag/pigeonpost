# Staging postbox

A staging postbox is the normal `pigeonpost-postbox` image with `POSTBOX_STAGING_MODE=1`. It is
where publishers' hosted test Nests run (build plan R-70) and where the build plane, the analysis
plane and the developer test environment are pointed for now. It runs next to other services on
one Ubuntu host (containerized, rootless podman, gVisor for the test pool), and the host is
configuration: moving it to its own VM later changes the values below, not the code. Paying
buyers' workers stay on the production Nests host.

In staging mode the postbox:

- marks every response with `x-pigeonpost-environment: staging` (public and internal API);
- refuses every route that applies a real payment with `403 staging_no_payments`
  (`POST /v1/claims/plan`, `/v1/claims/apple`, `/v1/claims/google[/assign]`,
  `/v1/nest-purchases/*`, internal entitlement, cloud-plan and org entitlement writes);
- serves `/v1/publishers/{publisher}/test-nests`, which answer `404 staging_only` anywhere else;
- dispatches test Nests only to the test pool (`POSTBOX_MARKETPLACE_TEST_NESTS_URL`), never to a
  production nestd, and expires them after `POSTBOX_MARKETPLACE_TEST_NEST_DAYS` (default 7).

Files here: `compose.yaml` (template), `staging.env.example` (placeholders only), this README.

## What must be created outside the repo

The owner (or an agent with the owner's go-ahead) creates these; none of them is in git.

1. **A domain** `<staging-domain>` with a DNS record for the host, and a TLS certificate for it on
   the host's existing reverse proxy.
2. **A realm client** for staging: a new Keycloak client (or a separate realm) with redirect URI
   `https://<staging-domain>/*`. Never the production client. Its issuer, audience and client id
   go in `OIDC_ISSUER`, `OIDC_AUDIENCE`, `OIDC_CLIENT_ID`.
3. **A fresh vault key file**, never copied from production:
   `openssl rand -hex 32 > ~/pp-staging/secrets/master.key && chmod 600 ~/pp-staging/secrets/master.key`.
4. **An ops service token** for the internal API (build plane, analysis plane, operator):
   `echo "svc_$(openssl rand -hex 32)"` into `POSTBOX_SVC_TOKEN_OPS`.
5. **A test-pool control token** (`openssl rand -hex 32`) for `POSTBOX_MARKETPLACE_TEST_NESTS_TOKEN`;
   its SHA-256 goes to the test nestd.
6. **The owner's staging account id** for `POSTBOX_PLATFORM_OWNER_ACCOUNT` (sign in once on staging,
   then read it from the internal API or the database). The owner is the only staff member for
   now; single-person approvals are logged as overrides.
7. **A `POW_HMAC_SECRET`** (`openssl rand -hex 32`).

## Steps

1. Check the ports are free. Do not change any other service's ports or proxy blocks.

       ss -ltn | grep -E ':1848[01]|:8790'

2. Make the config folder and copy the templates.

       mkdir -p ~/pp-staging/secrets
       cp deploy/staging/staging.env.example ~/pp-staging/staging.env
       chmod 600 ~/pp-staging/staging.env

3. Fill in `~/pp-staging/staging.env` (every `<placeholder>`). Leave `POSTBOX_NESTS_URL` unset:
   staging never talks to the production Nests host.

4. Build and start it on its own network (`pp-staging`) and volume (`pp-staging-data`).

       STAGING_ENV_FILE=~/pp-staging/staging.env \
       STAGING_MASTER_KEY_FILE=~/pp-staging/secrets/master.key \
         podman-compose -f deploy/staging/compose.yaml up -d --build

5. Point the reverse proxy for `<staging-domain>` at `127.0.0.1:18480` (a new server block; never
   proxy `18481`).

6. Turn the marketplace flag on, on staging only.

       curl -s -X PUT -H "authorization: Bearer $POSTBOX_SVC_TOKEN_OPS" \
         -H 'content-type: application/json' \
         -d '{"scope":"global","enabled":true}' \
         http://127.0.0.1:18481/internal/v1/features/marketplace

7. Point the build plane and the analysis plane at the staging postbox: base URL
   `http://127.0.0.1:18481` for the internal API and the staging ops token.

## Pointing the test pool at it

The test pool is a second nestd on the same host, with its own config file, state, network and
volume root, and gVisor (`runsc`) like production. Start from `deploy/nests/host/nestd.toml.example`
and change:

    db = "/var/lib/nests-test/nestd.db"

    [control]
    # The pp-staging bridge gateway: reachable by the staging postbox only.
    bind = "10.89.40.1:8790"
    # SHA-256 (hex) of POSTBOX_MARKETPLACE_TEST_NESTS_TOKEN.
    token_sha256_file = "/etc/nests-test/control-token.sha256"

    [sandbox.marketplace]
    image = "localhost/pigeonpost-marketplace-worker@sha256:<digest>"
    network = "nests-mp-test"
    volume_root = "/var/lib/nests-test/mp"
    broker_ip = "<test-broker-ip>"
    broker_port = 8788
    memory_mib = 256
    cpu_max_millis = 500
    pids = 64
    tmp_mib = 32
    # Admit only test statements (pigeonpost.nest.release.test.v1); refuse production releases.
    test_pool = true

Write the token's hash with
`printf '%s' "$POSTBOX_MARKETPLACE_TEST_NESTS_TOKEN" | sha256sum | cut -d' ' -f1 > /etc/nests-test/control-token.sha256`
and run it under its own systemd unit (a copy of `nestd-control.service` with the test config).
The production nestd keeps `test_pool` unset (false), so it refuses every test statement, and a
test pool refuses every production attestation. The model route for test workers is the
self-hosted one (owner decision 2026-11-08); synthetic data only.

## Checks

    curl -sI https://<staging-domain>/health | grep -i x-pigeonpost-environment   # staging
    curl -s -X POST https://<staging-domain>/v1/claims/plan                       # 403 staging_no_payments (with a token)
    curl -s -H "authorization: Bearer <publisher-token>" \
      https://<staging-domain>/v1/publishers/<publisher-id>/test-nests            # 200

## Tearing down

    podman-compose -f deploy/staging/compose.yaml down
    podman volume rm pigeonpost-staging_pp-staging-data

Then stop the test nestd and delete its `volume_root` and state folder. Test Nests also expire on
their own; at expiry their workers and volumes are removed from the test pool.
