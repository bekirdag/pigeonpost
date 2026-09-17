#!/usr/bin/env python3
"""Provision only the dedicated ChatGPT OAuth client. Credentials remain in environment/private files.

Required: KC_ADMIN, KC_ADMIN_PASSWORD, KC_CLIENT_BACKUP, KC_CLIENT_SECRET_FILE,
KC_REDIRECT_URIS_JSON (exact portal-provided callback URIs; no wildcards).
KC_URL defaults to the public issuer host. KC_LOOPBACK_TLS=1 is only accepted for HTTPS loopback
when running on the identity host through authenticated SSH and its local certificate is private.
An empty callback list provisions a disabled draft, never an open redirect client.
"""

import json
import os
import pathlib
import ssl
import urllib.parse
import urllib.request

CLIENT = "pigeonpost-chatgpt"
RESOURCE = "https://mcp.pigeonpost.dev/chatgpt"
REALM = "pigeonpost-prod"
SCOPES = {
    "pigeonpost:read": "Read your Pigeonpost inbox addresses and conversations",
    "pigeonpost:write": "Create inboxes, send messages and mark messages read in your Pigeonpost account",
}


def private_write(path, value):
    path = pathlib.Path(path)
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    # Never overwrite an earlier backup or silently rotate an existing secret file.
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w") as stream:
        stream.write(value)


def callbacks(raw):
    values = json.loads(raw)
    if not isinstance(values, list) or len(values) > 8:
        raise ValueError("callbacks must be a short JSON list")
    for value in values:
        parsed = urllib.parse.urlparse(value)
        loopback_test = parsed.hostname in ("127.0.0.1", "localhost") and os.environ.get("KC_ALLOW_LOOPBACK_TEST") == "1"
        if ("*" in value or parsed.fragment or parsed.username or parsed.password
                or (not loopback_test and (parsed.scheme != "https" or parsed.hostname not in ("chatgpt.com", "chat.openai.com")))):
            raise ValueError("use exact HTTPS ChatGPT callbacks from the plugin portal")
        if loopback_test and parsed.scheme not in ("http", "https"):
            raise ValueError("invalid loopback callback")
    return values


def provision():
    redirect_uris = callbacks(os.environ["KC_REDIRECT_URIS_JSON"])
    base = os.environ.get("KC_URL", "https://auth.pigeonpost.dev").rstrip("/")
    context = ssl.create_default_context()
    if os.environ.get("KC_LOOPBACK_TLS") == "1":
        parsed = urllib.parse.urlparse(base)
        if parsed.scheme != "https" or parsed.hostname not in ("127.0.0.1", "localhost"):
            raise ValueError("private-certificate override is restricted to local HTTPS")
        context = ssl._create_unverified_context()
    token_data = urllib.parse.urlencode({
        "client_id": "admin-cli", "grant_type": "password",
        "username": os.environ["KC_ADMIN"], "password": os.environ["KC_ADMIN_PASSWORD"],
    }).encode()
    request = urllib.request.Request(base + "/realms/master/protocol/openid-connect/token", token_data,
                                     headers={"User-Agent": "Pigeonpost-OAuth-Provisioner/1.0"})
    with urllib.request.urlopen(request, context=context, timeout=30) as response:
        token = json.load(response)["access_token"]

    def admin(path, method="GET", data=None):
        request = urllib.request.Request(base + "/admin/realms/" + REALM + path,
            data=json.dumps(data).encode() if data is not None else None,
            headers={"Authorization": "Bearer " + token, "Content-Type": "application/json",
                     "User-Agent": "Pigeonpost-OAuth-Provisioner/1.0"}, method=method)
        with urllib.request.urlopen(request, context=context, timeout=30) as response:
            body = response.read()
            return json.loads(body) if body else None

    existing = admin("/clients?clientId=" + CLIENT)
    known_scopes = {scope["name"]: scope for scope in admin("/client-scopes")}
    private_write(os.environ["KC_CLIENT_BACKUP"], json.dumps({
        "client": existing[0] if existing else None,
        "scopes": {name: known_scopes.get(name) for name in SCOPES},
    }, indent=2))
    if pathlib.Path(os.environ["KC_CLIENT_SECRET_FILE"]).exists():
        raise ValueError("choose a new private secret output file; the existing file is preserved")
    for name, description in SCOPES.items():
        if name in known_scopes:
            if known_scopes[name].get("attributes", {}).get("include.in.token.scope") != "true":
                raise ValueError("existing permission scope needs manual review: " + name)
        else:
            admin("/client-scopes", "POST", {
                "name": name, "description": description, "protocol": "openid-connect",
                "attributes": {"include.in.token.scope": "true", "display.on.consent.screen": "true",
                               "consent.screen.text": description},
            })
    payload = {
        "clientId": CLIENT, "name": "Pigeonpost for ChatGPT", "protocol": "openid-connect",
        "description": "Account-linked inbox and messaging tools with explicit read/write consent.",
        "enabled": bool(redirect_uris), "publicClient": False, "clientAuthenticatorType": "client-secret",
        "consentRequired": True, "standardFlowEnabled": True,
        "directAccessGrantsEnabled": False, "implicitFlowEnabled": False,
        "serviceAccountsEnabled": False, "fullScopeAllowed": False,
        # Keycloak 24 synthesizes rootUrl/* when creating a client with an empty callback list.
        # Keep these empty so a disabled draft has no implicit wildcard callback.
        "redirectUris": redirect_uris, "webOrigins": [], "rootUrl": "",
        "baseUrl": "", "frontchannelLogout": False,
        "attributes": {"pkce.code.challenge.method": "S256", "oauth2.device.authorization.grant.enabled": "false",
                       "access.token.lifespan": "300", "exclude.issuer.from.auth.response": "false"},
        "defaultClientScopes": [], "optionalClientScopes": ["profile", "email", *SCOPES.keys()],
        "protocolMappers": [{
            "name": "Pigeonpost ChatGPT resource", "protocol": "openid-connect", "protocolMapper": "oidc-audience-mapper",
            "consentRequired": False, "config": {"included.custom.audience": RESOURCE,
                                                   "access.token.claim": "true", "id.token.claim": "false"},
        }],
    }
    if existing:
        # Preserve the existing client secret; PUT does not regenerate it.
        admin("/clients/" + existing[0]["id"], "PUT", payload)
    else:
        admin("/clients", "POST", payload)
    checked = admin("/clients?clientId=" + CLIENT)[0]
    assert checked["redirectUris"] == redirect_uris and checked["consentRequired"]
    assert checked["attributes"]["pkce.code.challenge.method"] == "S256"
    assert not checked["publicClient"] and not checked["directAccessGrantsEnabled"]
    assert not checked["serviceAccountsEnabled"] and not checked["implicitFlowEnabled"]
    optional = admin("/clients/" + checked["id"] + "/optional-client-scopes")
    assert all(name in {scope["name"] for scope in optional} for name in SCOPES)
    secret = admin("/clients/" + checked["id"] + "/client-secret")["value"]
    private_write(os.environ["KC_CLIENT_SECRET_FILE"], secret)
    print(json.dumps({"client": CLIENT, "enabled": checked["enabled"], "pkce": "S256",
                      "consent": True, "callbacks": redirect_uris, "secret": "saved to private file"}))


if __name__ == "__main__":
    provision()
