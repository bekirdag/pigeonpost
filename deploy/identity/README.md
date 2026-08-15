# Identity provider setup for `pigeonpost login`

`pigeonpost login` and `pigeonpost login --device` authenticate as the **public** OAuth client
`pigeonpost-cli`. Until that client exists in the realm, both fail at the provider with
`invalid_client` — nothing in the CLI can work around it.

## Applying it

```sh
KC_URL=https://auth.pigeonpost.dev \
KC_REALM=pigeonpost-prod \
KC_ADMIN=<admin user> \
KC_ADMIN_PASSWORD=<admin password> \
./create-cli-client.sh
```

Idempotent — safe to re-run. It creates or updates the client and then proves the result by asking
the device endpoint for a user code, so a silent misconfiguration fails the script rather than the
next person to try logging in.

## Two things that are easy to get wrong

**Do not set `pkce.code.challenge.method` on this client.** Keycloak applies that mandate to every
flow the client can use, including the device grant — which under RFC 8628 has no redirect and
therefore no PKCE. Setting it makes Keycloak reject its own device requests with
`Missing parameter: code_challenge_method`. The CLI always sends S256 for the authorization-code
flow regardless (pinned against RFC 7636's published test vector), which is where the protection
is needed. Enforcing it server-side would require giving the device flow a second client id.

**Redirect URIs must wildcard the port** (`http://127.0.0.1/*`). The CLI binds `127.0.0.1:0` and
the OS chooses the port, so it cannot be registered ahead of time.

Also note Keycloak *merges* client attributes on update, so clearing an attribute requires setting
it to `""` rather than omitting it — the script does this, which is what makes re-running it
actually converge.

## Verified

Both the script and the CLI were exercised against a real Keycloak 26 before this was written:
`login --device` completes a full device-code + consent round trip, writes `auth.json` at 0600 with
refresh and access tokens, `whoami` reports the session without printing a token, and `logout`
removes it.


## Consent is required, deliberately (2026-08-15)

`pigeonpost-cli` sets `consentRequired: true`. Without it the device grant approves itself: opening
`verification_uri_complete` while signed in supplies the user code *and* the session, and with no
consent step left, the flow completes without anyone approving anything. Observed on prod — the
browser went straight to `/device/status` and the CLI reported a successful sign-in.

The completion URI exists so a code need not be retyped, not so it can replace the user's decision
(RFC 8628 §5.4). Leave consent on. The cost is one approval screen the first time a person uses
the CLI, which is what an approval is.
