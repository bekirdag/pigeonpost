#!/usr/bin/env python3
"""Create the two things a Mac App Store upload needs that the iOS one does not.

An iOS upload needs one signing identity. A Mac App Store upload needs two: an **Apple
Distribution** certificate to sign the `.app`, which this project already has and which is not
platform-specific, and a **Mac Installer Distribution** certificate to sign the `.pkg` that actually
gets uploaded. It also needs a macOS provisioning profile, which is a different profile type from
the iOS one even when the bundle id is identical.

This makes both, and is deliberately meant to be run *once* rather than per build. Apple caps
distribution certificates per team at a small number, and the iOS workflow already learned what
happens when something mints one on every run: ten builds in, the eleventh fails with "Choose a
certificate to revoke." So this refuses to create a second certificate of a type that already
exists unless told to, and says what it found instead.

The private key never leaves this machine. Apple only ever sees a certificate signing request.

Usage:

    KEY_ID=... ISSUER_ID=... KEY=@~/.appstore/AuthKey_XXXX.p8 \\
      python3 .github/scripts/mac_signing_assets.py --out ~/pigeonpost-mac-signing

Then store what it wrote, with `gh secret set`. It prints the exact commands.

Standard library only, matching `testflight_status.py`, so it runs on a bare runner as well as here.
"""

import argparse
import base64
import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

API = "https://api.appstoreconnect.apple.com/v1"

BUNDLE_ID = os.environ.get("BUNDLE_ID", "dev.pigeonpost.inbox")
PROFILE_NAME = os.environ.get("PROFILE_NAME", "Pigeonpost Desktop Mac App Store")

KEY_ID = os.environ["KEY_ID"]
ISSUER_ID = os.environ["ISSUER_ID"]


def _read_key() -> str:
    """The key itself, or a path to it. A path is easier to pass without it landing in shell history."""
    raw = os.environ["KEY"]
    if raw.startswith("@"):
        with open(os.path.expanduser(raw[1:]), "r") as fh:
            return fh.read()
    return raw


KEY = _read_key()


def _b64url(raw: bytes) -> str:
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode()


def _der_to_raw(der: bytes) -> bytes:
    """An ES256 JWT signature is r||s, 32 bytes each. openssl emits DER. Convert."""
    assert der[0] == 0x30
    body = der[2:] if der[1] < 0x80 else der[2 + (der[1] & 0x7F):]
    out = b""
    while body:
        assert body[0] == 0x02
        length = body[1]
        value = body[2:2 + length].lstrip(b"\x00")
        out += value.rjust(32, b"\x00")
        body = body[2 + length:]
    return out


def mint_token() -> str:
    header = _b64url(json.dumps({"alg": "ES256", "kid": KEY_ID, "typ": "JWT"}).encode())
    now = int(time.time())
    payload = _b64url(
        json.dumps(
            {"iss": ISSUER_ID, "iat": now, "exp": now + 1200, "aud": "appstoreconnect-v1"}
        ).encode()
    )
    signing_input = f"{header}.{payload}".encode()
    with tempfile.TemporaryDirectory() as tmp:
        key_path = os.path.join(tmp, "key.p8")
        with open(os.open(key_path, os.O_WRONLY | os.O_CREAT, 0o600), "wb") as fh:
            fh.write(KEY.encode() if KEY.endswith("\n") else (KEY + "\n").encode())
        der = subprocess.run(
            ["openssl", "dgst", "-sha256", "-sign", key_path],
            input=signing_input,
            capture_output=True,
            check=True,
        ).stdout
    return f"{header}.{payload}.{_b64url(_der_to_raw(der))}"


TOKEN = mint_token()


def call(method, path, body=None, **params):
    url = path if path.startswith("http") else API + path
    if params:
        url += "?" + urllib.parse.urlencode(params)
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Authorization", f"Bearer {TOKEN}")
    if data:
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            raw = resp.read()
        return json.loads(raw) if raw else {}
    except urllib.error.HTTPError as exc:
        detail = exc.read().decode(errors="replace")[:900]
        raise RuntimeError(f"{method} {url} -> {exc.code} {detail}") from None


def get(path, **params):
    return call("GET", path, None, **params)


def pem_from_der_b64(content: str) -> str:
    body = "\n".join(content[i:i + 64] for i in range(0, len(content), 64))
    return f"-----BEGIN CERTIFICATE-----\n{body}\n-----END CERTIFICATE-----\n"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", required=True, help="directory to write the .p12 and profile into")
    parser.add_argument(
        "--replace-installer-certificate",
        action="store_true",
        help="create a Mac Installer Distribution certificate even though one exists. Only do this "
             "when the existing one's private key is genuinely lost — the old one keeps counting "
             "against the team's limit until it is revoked.",
    )
    args = parser.parse_args()
    out = os.path.expanduser(args.out)
    os.makedirs(out, mode=0o700, exist_ok=True)

    # ---- the bundle id, and whether Apple thinks it is a Mac app -------------------------------
    found = get("/bundleIds", **{"filter[identifier]": BUNDLE_ID, "limit": 200})["data"]
    exact = [b for b in found if b["attributes"]["identifier"] == BUNDLE_ID]
    if not exact:
        print(f"No bundle id {BUNDLE_ID} on this team.", file=sys.stderr)
        return 1
    bundle = exact[0]
    platform = bundle["attributes"].get("platform")
    print(f"Bundle id {BUNDLE_ID}: id={bundle['id']} platform={platform}")
    if platform not in ("UNIVERSAL", "MAC_OS"):
        print(
            f"\n  This App ID is registered for {platform}. A macOS app sharing the iPhone app's\n"
            "  bundle id — which is what Universal Purchase requires — needs it set to Universal.\n"
            "  The API will not change an existing App ID's platform; it is one checkbox in the\n"
            "  developer portal: Certificates, Identifiers & Profiles → Identifiers →\n"
            f"  {BUNDLE_ID} → tick macOS → Save.\n"
            "  Everything below still runs; the profile is what will fail without it.\n"
        )

    # ---- certificates ---------------------------------------------------------------------------
    certs = get("/certificates", limit=200)["data"]
    by_type: dict[str, list] = {}
    for cert in certs:
        by_type.setdefault(cert["attributes"]["certificateType"], []).append(cert)
    print("\nCertificates on this team")
    print("-" * 78)
    for kind, items in sorted(by_type.items()):
        for cert in items:
            a = cert["attributes"]
            print(f"  {kind:34} {a.get('displayName', '?'):28} expires {a.get('expirationDate', '?')[:10]}")

    distribution = by_type.get("DISTRIBUTION") or by_type.get("IOS_DISTRIBUTION") or []
    if not distribution:
        print(
            "\nNo Apple Distribution certificate. That is the one that signs the .app, and the iOS "
            "pipeline already uses it — if it is missing here, something else is wrong.",
            file=sys.stderr,
        )
        return 1
    signing_cert = distribution[0]
    print(f"\nApp will be signed with: {signing_cert['attributes'].get('displayName')} ({signing_cert['id']})")

    installer = by_type.get("MAC_INSTALLER_DISTRIBUTION", [])
    installer_p12 = os.path.join(out, "mac_installer.p12")
    p12_password = None
    if installer and not args.replace_installer_certificate:
        print(
            f"\nA Mac Installer Distribution certificate already exists "
            f"({installer[0]['attributes'].get('displayName')}, expires "
            f"{installer[0]['attributes'].get('expirationDate','?')[:10]}).\n"
            "  Not creating a second one: Apple caps these per team, and a certificate whose\n"
            "  private key nobody has is worse than none.\n"
            "  If its key is on this Mac, export it from Keychain Access (right-click the private\n"
            "  key → Export) and use that .p12. If the key is genuinely lost, revoke the old\n"
            "  certificate in the portal and re-run with --replace-installer-certificate."
        )
    else:
        print("\nCreating a Mac Installer Distribution certificate…")
        key_path = os.path.join(out, "mac_installer_key.pem")
        csr_path = os.path.join(out, "mac_installer.csr")
        subprocess.run(
            ["openssl", "req", "-new", "-newkey", "rsa:2048", "-nodes",
             "-keyout", key_path, "-out", csr_path,
             "-subj", f"/CN=Pigeonpost Mac Installer/O={BUNDLE_ID}/C=US"],
            check=True, capture_output=True,
        )
        os.chmod(key_path, 0o600)
        with open(csr_path) as fh:
            csr = fh.read()
        created = call("POST", "/certificates", {
            "data": {
                "type": "certificates",
                "attributes": {"certificateType": "MAC_INSTALLER_DISTRIBUTION", "csrContent": csr},
            }
        })["data"]
        cert_pem = os.path.join(out, "mac_installer.pem")
        with open(cert_pem, "w") as fh:
            fh.write(pem_from_der_b64(created["attributes"]["certificateContent"]))
        # A password the .p12 format requires and that protects nothing beyond this directory; it is
        # stored beside it and set as a secret in the same breath.
        p12_password = base64.urlsafe_b64encode(os.urandom(18)).decode().rstrip("=")
        subprocess.run(
            ["openssl", "pkcs12", "-export", "-legacy",
             "-inkey", key_path, "-in", cert_pem, "-out", installer_p12,
             "-passout", f"pass:{p12_password}"],
            check=True, capture_output=True,
        )
        os.chmod(installer_p12, 0o600)
        with open(os.open(os.path.join(out, "mac_installer.p12.password"),
                         os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600), "w") as fh:
            fh.write(p12_password + "\n")
        print(f"  wrote {installer_p12}")

    # ---- the profile ------------------------------------------------------------------------------
    existing = [
        p for p in get("/profiles", limit=200, include="bundleId")["data"]
        if p["attributes"]["name"] == PROFILE_NAME
    ]
    for stale in existing:
        # A profile is free to recreate and pins the certificate list at creation time, so an old
        # one silently signs with a certificate that may since have been replaced.
        print(f"\nRemoving the previous profile {PROFILE_NAME} ({stale['id']})")
        call("DELETE", f"/profiles/{stale['id']}")

    print(f"Creating profile {PROFILE_NAME} (MAC_APP_STORE)…")
    profile = call("POST", "/profiles", {
        "data": {
            "type": "profiles",
            "attributes": {"name": PROFILE_NAME, "profileType": "MAC_APP_STORE"},
            "relationships": {
                "bundleId": {"data": {"type": "bundleIds", "id": bundle["id"]}},
                "certificates": {"data": [{"type": "certificates", "id": signing_cert["id"]}]},
            },
        }
    })["data"]
    profile_path = os.path.join(out, "pigeonpost_mac.provisionprofile")
    with open(os.open(profile_path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600), "wb") as fh:
        fh.write(base64.b64decode(profile["attributes"]["profileContent"]))
    print(f"  wrote {profile_path}")

    # ---- what to do with it -----------------------------------------------------------------------
    print("\nStore these, then delete the directory:")
    print("-" * 78)
    print(f'  gh secret set APPLE_MAC_PROFILE --repo bekirdag/pigeonpost \\\n'
          f'    --body "$(base64 < {profile_path})"')
    if p12_password is not None:
        print(f'  gh secret set APPLE_MAC_INSTALLER_P12 --repo bekirdag/pigeonpost \\\n'
              f'    --body "$(base64 < {installer_p12})"')
        print(f'  gh secret set APPLE_MAC_INSTALLER_P12_PASSWORD --repo bekirdag/pigeonpost \\\n'
              f'    --body "$(cat {os.path.join(out, "mac_installer.p12.password")})"')
    else:
        print("  APPLE_MAC_INSTALLER_P12 / _PASSWORD: from the .p12 you exported from Keychain Access.")
    print(f"\n  rm -rf {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
