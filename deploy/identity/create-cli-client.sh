#!/usr/bin/env bash
# Create the pigeonpost-cli public client. Idempotent: safe to re-run.
#
# Usage:  KC_URL=https://auth.pigeonpost.dev KC_REALM=pigeonpost-prod \
#         KC_ADMIN=<user> KC_ADMIN_PASSWORD=<pass> ./kc-setup.sh
set -euo pipefail
KC_URL="${KC_URL:-http://127.0.0.1:8080}"
KC_REALM="${KC_REALM:-pigeonpost-prod}"
KC_ADMIN="${KC_ADMIN:-admin}"
KC_ADMIN_PASSWORD="${KC_ADMIN_PASSWORD:-admin}"
KC_ADMIN_REALM="${KC_ADMIN_REALM:-master}"

token=$(curl -sf --max-time 30 -X POST \
  "$KC_URL/realms/$KC_ADMIN_REALM/protocol/openid-connect/token" \
  -d client_id=admin-cli -d "username=$KC_ADMIN" \
  -d "password=$KC_ADMIN_PASSWORD" -d grant_type=password \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["access_token"])')

# The realm must exist; create it only if absent so this never disturbs a live one.
if ! curl -sf --max-time 30 -H "Authorization: Bearer $token" "$KC_URL/admin/realms/$KC_REALM" >/dev/null 2>&1; then
  curl -sf --max-time 30 -X POST "$KC_URL/admin/realms" \
    -H "Authorization: Bearer $token" -H "Content-Type: application/json" \
    -d "{\"realm\":\"$KC_REALM\",\"enabled\":true}" >/dev/null
  echo "created realm $KC_REALM"
fi

# publicClient + no secret: a secret shipped in every copy of a CLI is not a secret. PKCE is what
# makes that safe, and the CLI always sends S256 (pinned against RFC 7636's own test vector).
#
# `pkce.code.challenge.method` is S256 for every flow on this client. The strong sign-in for SSH
# needs it, and Keycloak applies it to the device grant too, so a device request without PKCE is
# refused with "Missing parameter: code_challenge_method". CLI 0.7.20 and later send S256 on both;
# older CLIs can use only the browser sign-in.
#
# `consentRequired` is on, and the device grant is why. Keycloak needs three things to approve a
# device: the user code, an authenticated session, and consent. Turn consent off and the first two
# are already satisfied the moment someone opens `verification_uri_complete` while signed in --
# the link carries the code, so the flow completes with no approval step at all and possession of
# that URL becomes the entire authorisation. RFC 8628 §5.4 describes exactly this: the completion
# URI is meant for convenience, not to stand in for the user saying yes. It also costs the
# authorization-code flow one screen on first use, which is the correct price.
#
# The loopback redirect must be port-wildcarded: the CLI binds 127.0.0.1:0 and the OS
# chooses the port, so it cannot be registered ahead of time.
payload='{
  "clientId": "pigeonpost-cli",
  "name": "Pigeonpost CLI",
  "description": "Public client for `pigeonpost login` (authorization code + PKCE) and `pigeonpost login --device`.",
  "enabled": true,
  "publicClient": true,
  "consentRequired": true,
  "standardFlowEnabled": true,
  "directAccessGrantsEnabled": false,
  "serviceAccountsEnabled": false,
  "implicitFlowEnabled": false,
  "redirectUris": ["http://127.0.0.1/*", "http://localhost/*"],
  "webOrigins": [],
  "attributes": {
    "oauth2.device.authorization.grant.enabled": "true",
    "pkce.code.challenge.method": "S256"
  }
}'

existing=$(curl -sf --max-time 30 -H "Authorization: Bearer $token" \
  "$KC_URL/admin/realms/$KC_REALM/clients?clientId=pigeonpost-cli" \
  | python3 -c 'import json,sys; c=json.load(sys.stdin); print(c[0]["id"] if c else "")')

if [ -n "$existing" ]; then
  curl -sf --max-time 30 -X PUT "$KC_URL/admin/realms/$KC_REALM/clients/$existing" \
    -H "Authorization: Bearer $token" -H "Content-Type: application/json" -d "$payload" >/dev/null
  echo "updated existing pigeonpost-cli client"
else
  curl -sf --max-time 30 -X POST "$KC_URL/admin/realms/$KC_REALM/clients" \
    -H "Authorization: Bearer $token" -H "Content-Type: application/json" -d "$payload" >/dev/null
  echo "created pigeonpost-cli client"
fi

# Prove it: a device-code request must now be accepted rather than refused.
probe=$(curl -s --max-time 30 -X POST \
  "$KC_URL/realms/$KC_REALM/protocol/openid-connect/auth/device" \
  -d client_id=pigeonpost-cli -d "scope=openid profile offline_access" \
  -d code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM -d code_challenge_method=S256)
if printf '%s' "$probe" | grep -q user_code; then
  echo "verified: the device grant now issues a user code"
else
  echo "FAILED to verify: $probe" >&2
  exit 1
fi
