#!/usr/bin/env python3
"""Provision only the dedicated Linux public client; administrator credentials are environment-only."""
import json
import os
import pathlib
import urllib.parse
import urllib.request


def provision(base, username, password, backup):
    realm = "pigeonpost-prod"
    token_data = urllib.parse.urlencode({"client_id": "admin-cli", "username": username,
                                       "password": password, "grant_type": "password"}).encode()
    with urllib.request.urlopen(base + "/realms/master/protocol/openid-connect/token", token_data, timeout=30) as response:
        token = json.load(response)["access_token"]
    def admin(path, method="GET", data=None):
        request = urllib.request.Request(base + "/admin/realms/" + realm + path,
                                         data=json.dumps(data).encode() if data is not None else None,
                                         headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"}, method=method)
        with urllib.request.urlopen(request, timeout=30) as response:
            body = response.read()
            return json.loads(body) if body else None
    existing = admin("/clients?clientId=pigeonpost-linux")
    if existing:
        backup.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        if backup.exists():
            raise RuntimeError("Choose a new backup path; original client backup already exists")
        backup.write_text(json.dumps(existing[0], indent=2))
        backup.chmod(0o600)
    mobile = admin("/clients?clientId=pigeonpost-mobile")[0]
    payload = {
        "clientId": "pigeonpost-linux", "name": "Pigeonpost Desktop (Linux)",
        "description": "Native Linux desktop using device authorization with explicit consent.",
        "enabled": True, "publicClient": True, "consentRequired": True,
        "standardFlowEnabled": False, "directAccessGrantsEnabled": False,
        "implicitFlowEnabled": False, "serviceAccountsEnabled": False,
        "redirectUris": [], "webOrigins": [],
        "attributes": {"oauth2.device.authorization.grant.enabled": "true", "pkce.code.challenge.method": ""},
        "defaultClientScopes": mobile.get("defaultClientScopes", ["profile", "email", "roles"]),
        "optionalClientScopes": mobile.get("optionalClientScopes", ["offline_access"]),
        "protocolMappers": [{k: v for k, v in mapper.items() if k != "id"} for mapper in mobile.get("protocolMappers", [])],
    }
    if existing:
        admin("/clients/" + existing[0]["id"], "PUT", payload)
    else:
        admin("/clients", "POST", payload)
    checked = admin("/clients?clientId=pigeonpost-linux")[0]
    assert checked["publicClient"] and checked["consentRequired"] and not checked["directAccessGrantsEnabled"]
    print("Verified dedicated Linux OAuth client: public, device flow enabled, explicit consent required.")


if __name__ == "__main__":
    provision(os.environ.get("KC_URL", "https://auth.pigeonpost.dev"), os.environ["KC_ADMIN"],
              os.environ["KC_ADMIN_PASSWORD"], pathlib.Path(os.environ["KC_CLIENT_BACKUP"]))
