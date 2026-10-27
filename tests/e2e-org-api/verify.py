#!/usr/bin/env python3
"""Disposable, loopback-only end-to-end check of the enterprise API (SDS §18) against a real
postbox binary or image. Never uses real accounts, real sign-in or a configured database.

    python3 verify.py --binary /abs/path/pigeonpost-postbox
    python3 verify.py --image pigeonpost-postbox:<tag>        # docker run --network host

It starts a synthetic OIDC issuer (an RSA key made for this run), a postbox with a fresh database
in a temporary directory, then walks the org API through its real HTTP contracts: org activation
through the billing service with a grant entitlement (no payment), key creation with and without
step-up, scoped calls, idempotency, a restrictive and a refused trust-raising rule, a room, a
direct message, the audit trail, webhook registration (a private URL refused, a public one
accepted) and one real signed delivery attempt to --webhook-url, then revocation. Everything it
created is deleted with the temporary directory.

Requires Python 3.9+ and the `cryptography` package (RS256 signing).
"""

import argparse
import base64
import http.server
import json
import os
import secrets
import shutil
import socket
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request

from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import padding, rsa


def b64u(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Issuer:
    """A JWKS endpoint and RS256 tokens for synthetic people."""

    def __init__(self):
        self.key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        nums = self.key.public_key().public_numbers()
        jwk = {
            "kty": "RSA", "kid": "fixture", "alg": "RS256", "use": "sig",
            "n": b64u(nums.n.to_bytes((nums.n.bit_length() + 7) // 8, "big")),
            "e": b64u(nums.e.to_bytes(3, "big")),
        }
        body = json.dumps({"keys": [jwk]}).encode()

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):  # noqa: N802
                if self.path.endswith("/protocol/openid-connect/certs"):
                    self.send_response(200)
                    self.send_header("content-type", "application/json")
                    self.end_headers()
                    self.wfile.write(body)
                else:
                    self.send_response(404)
                    self.end_headers()

            def log_message(self, *args):
                pass

        self.port = free_port()
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", self.port), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self.port}/realms/fixture"

    def token(self, subject: str, strong: bool) -> str:
        now = int(time.time())
        header = b64u(json.dumps({"alg": "RS256", "typ": "JWT", "kid": "fixture"}).encode())
        claims = {"iss": self.url, "sub": subject, "iat": now, "exp": now + 3600, "auth_time": now,
                  "azp": "pigeonpost-web", "typ": "Bearer", "aud": ["account"],
                  "preferred_username": subject}
        if strong:
            claims["acr"] = "fixture-mfa"
        payload = b64u(json.dumps(claims).encode())
        sig = self.key.sign(f"{header}.{payload}".encode(), padding.PKCS1v15(), hashes.SHA256())
        return f"{header}.{payload}.{b64u(sig)}"


class Checks:
    def __init__(self):
        self.passed = 0

    def ok(self, cond, what):
        if not cond:
            raise SystemExit(f"FAIL: {what}")
        self.passed += 1
        print(f"  ok  {what}")


def call(base, method, path, token=None, body=None, headers=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(base + path, data=data, method=method)
    if token:
        req.add_header("authorization", f"Bearer {token}")
    if data is not None:
        req.add_header("content-type", "application/json")
    for k, v in (headers or {}).items():
        req.add_header(k, v)
    try:
        with urllib.request.urlopen(req, timeout=20) as r:
            status, hdrs, raw = r.status, dict(r.headers), r.read()
    except urllib.error.HTTPError as e:
        status, hdrs, raw = e.code, dict(e.headers), e.read()
    try:
        parsed = json.loads(raw or b"null")
    except ValueError:
        parsed = raw.decode(errors="replace")
    return status, {k.lower(): v for k, v in hdrs.items()}, parsed


def main():
    ap = argparse.ArgumentParser()
    src = ap.add_mutually_exclusive_group(required=True)
    src.add_argument("--binary")
    src.add_argument("--image")
    ap.add_argument("--webhook-url", default="https://example.com/pigeonpost-webhook-check",
                    help="a public https URL for one real delivery attempt (any answer is fine)")
    args = ap.parse_args()

    work = tempfile.mkdtemp(prefix="pp-org-api-")
    os.chmod(work, 0o700)
    proc = None
    container = f"pp-org-api-verify-{secrets.token_hex(4)}"
    try:
        issuer = Issuer()
        port, internal_port = free_port(), free_port()
        base, internal = f"http://127.0.0.1:{port}", f"http://127.0.0.1:{internal_port}"
        ops = "svc_ops_" + secrets.token_hex(20)
        billing = "svc_billing_" + secrets.token_hex(20)
        data_dir = "/work" if args.image else work
        with open(os.path.join(work, "kms"), "wb") as f:
            f.write(secrets.token_bytes(32))
        with open(os.path.join(work, "reserved"), "w") as f:
            f.write("admin\n")
        env = {
            "RUST_LOG": "warn", "POSTBOX_BIND": f"127.0.0.1:{port}",
            "POSTBOX_INTERNAL_BIND": f"127.0.0.1:{internal_port}", "POSTBOX_PUBLIC_URL": base,
            "POSTBOX_DB_PATH": f"{data_dir}/postbox.db", "POSTBOX_KMS": f"sealed-file:{data_dir}/kms",
            "POSTBOX_LOFT_DIR": f"{data_dir}/loft", "PIGEONPOST_BLOB_DIR": f"{data_dir}/blobs",
            "PIGEONPOST_BLOB_MIN_FREE_MB": "1", "REGISTRY_URL": f"http://127.0.0.1:{issuer.port}",
            "OIDC_ISSUER": issuer.url, "POSTBOX_SVC_TOKEN_OPS": ops, "POSTBOX_SVC_TOKEN_BILLING": billing,
            "POSTBOX_STEP_UP_ACR_VALUES": "fixture-mfa", "PIGEONPOST_RESERVED_NAMES": f"{data_dir}/reserved",
        }
        if args.image:
            os.chown(work, 65532, 65532)
            for name in os.listdir(work):
                os.chown(os.path.join(work, name), 65532, 65532)
            cmd = ["docker", "run", "--rm", "--name", container, "--network", "host",
                   "-v", f"{work}:/work"]
            for k, v in env.items():
                cmd += ["-e", f"{k}={v}"]
            proc = subprocess.Popen(cmd + [args.image], stdout=subprocess.DEVNULL)
        else:
            proc = subprocess.Popen([args.binary], env={"PATH": "/usr/bin:/bin", **env},
                                    stdout=subprocess.DEVNULL)
        for _ in range(240):
            if proc.poll() is not None:
                raise SystemExit("FAIL: the postbox exited during startup")
            try:
                if call(base, "GET", "/health")[0] == 200:
                    break
            except OSError:
                pass
            time.sleep(0.25)
        else:
            raise SystemExit("FAIL: the postbox did not start")

        c = Checks()
        print("enterprise API verification (synthetic fixtures, loopback only)")
        for flag in ("orgs", "groups", "org_api"):
            s, _, _ = call(internal, "PUT", f"/internal/v1/features/{flag}", ops, {"scope": "global", "enabled": True})
            c.ok(s == 200, f"flag {flag} on")

        run = secrets.token_hex(4)
        owner = issuer.token(f"p5-owner-{run}", False)
        owner_strong = issuer.token(f"p5-owner-{run}", True)
        jane = issuer.token(f"p5-jane-{run}", False)
        s, _, mb = call(base, "POST", "/v1/identities", owner, {"label": "owner"})
        c.ok(s == 201, "owner account created")
        call(base, "POST", "/v1/identities", jane, {"label": "jane"})
        db = sqlite3.connect(f"file:{work}/postbox.db?mode=ro", uri=True)
        owner_account = db.execute("SELECT account_id FROM identities WHERE address = ?", (mb["address"],)).fetchone()[0]
        db.close()

        s, _, d = call(internal, "POST", "/internal/v1/org-drafts", billing, {
            "operation_id": f"op-{run}-0123456789abcdef", "owner_account": owner_account,
            "name": "Fixture Ltd", "namespace": f"fixture{run}", "plan_code": "ent-5-monthly-usd", "seats": 5})
        c.ok(s == 201, "org drafted")
        org = d["org"]["id"]
        paid = int(time.time()) + 30 * 86400
        s, _, _ = call(internal, "PUT", f"/internal/v1/orgs/{org}/activate", billing, {
            "operation_id": f"act-{run}-0123456789", "entitlement": {
                "provider": "grant", "environment": "test", "reference": f"ref-{run}",
                "plan_code": "ent-5-monthly-usd", "plan_version": "2026", "seats": 5, "status": "active",
                "period_end": paid, "paid_through": paid, "reconciliation_version": 1}})
        c.ok(s == 200, "org activated with a grant entitlement (no payment)")
        s, _, _ = call(base, "POST", f"/v1/orgs/{org}/me/mailbox", owner, {"handle": "boss"})
        c.ok(s == 201, "owner chose an org mailbox")
        s, _, _ = call(base, "POST", f"/v1/orgs/{org}/agents", owner, {"handle": "notifier"})
        c.ok(s == 201, "agent created on the dashboard")

        keys = f"/v1/orgs/{org}/api-keys"
        body = {"name": "verify", "scopes": ["org:read", "members:write", "agents:write", "rules:write",
                                             "rooms:write", "messages:send", "audit:read", "webhooks:write"],
                "send_as": "notifier", "expires_in_days": 1}
        s, h, e = call(base, "POST", keys, owner, body)
        c.ok(s == 401 and e.get("error") == "step_up_required" and "insufficient_user_authentication" in h.get("www-authenticate", ""),
             "key creation without step-up answers the RFC 9470 challenge")
        s, h, k = call(base, "POST", keys, owner_strong, body)
        c.ok(s == 201 and k["secret"].startswith("pk_org_") and h.get("cache-control") == "no-store",
             "key created with step-up, secret shown once, no-store")
        key, kid = k["secret"], k["key"]["id"]
        s, _, lst = call(base, "GET", keys, owner)
        c.ok(s == 200 and key not in json.dumps(lst), "the key list never shows the secret")

        api = f"{base}/org/v1"
        s, _, e = call(api, "GET", "/org", "pk_org_" + "a" * 32)
        c.ok(s == 401 and e["error"]["code"] == "unauthorized", "an unknown key is refused in the error schema")
        s, h, o = call(api, "GET", "/org", key)
        c.ok(s == 200 and o["data"]["id"] == org and h.get("ratelimit-limit") == "600", "GET /org with rate-limit headers")
        s, _, _ = call(base, "GET", "/v1/whoami", key)
        c.ok(s == 401, "an org key is not a member-API credential")
        s, _, inv = call(api, "POST", "/members", key, {"first_name": "Jane", "last_name": "Doe",
                                                         "contact": "jane@fixture.invalid", "handle": "jane"})
        c.ok(s == 201 and inv["data"]["member"]["status"] == "invited", "ordinary member invited")
        s, _, e = call(api, "POST", "/members", key, {"first_name": "E", "last_name": "V", "contact": "eve@fixture.invalid",
                                                       "handle": "eve", "roles": ["admin"]})
        c.ok(s == 403 and e["error"]["code"] == "human_required", "role grants are refused (human_required)")
        idem = {"idempotency-key": f"agent-{run}"}
        s1, _, a1 = call(api, "POST", "/agents", key, {"handle": "builder"}, idem)
        s2, h2, a2 = call(api, "POST", "/agents", key, {"handle": "builder"}, idem)
        c.ok(s1 == 201 and s2 == 201 and a1 == a2 and h2.get("idempotent-replayed") == "true", "Idempotency-Key replays")
        s, _, e = call(api, "POST", "/rules", key, {"owners": "agents", "peers": "*", "admission": "allow"})
        c.ok(s == 403 and e["error"]["code"] == "human_required", "trust-raising rules are refused")
        s, _, _ = call(api, "POST", "/rules", key, {"owners": "agents", "peers": "*", "admission": "block", "priority": 900})
        c.ok(s == 201, "a restrictive rule is accepted")
        s, _, r = call(api, "POST", "/rooms", key, {"name": "Builds", "owner": "boss"})
        c.ok(s == 201 and r["data"]["activation_state"] == "provisioning", "room created for the owner's device to activate")
        s, _, e = call(api, "POST", "/messages", key, {"to": r["data"]["id"], "body": "x"})
        c.ok(s == 409 and e["error"]["code"] == "room_encrypted", "rooms are never posted to")
        s, _, m = call(api, "POST", "/messages", key, {"to": "/" + f"fixture{run}" + "/boss", "body": "verification"})
        c.ok(s == 201 and m["data"]["from"].endswith("/notifier"), "direct message from the key's agent to an org mailbox")

        s, _, e = call(api, "POST", "/webhooks", key, {"url": "https://10.0.0.1/x", "events": ["*"]})
        c.ok(s == 400 and e["error"]["code"] == "invalid_webhook_url", "a private webhook URL is refused")
        s, _, e = call(api, "POST", "/webhooks", key, {"url": "https://169.254.169.254/latest", "events": ["*"]})
        c.ok(s == 400, "the metadata address is refused")
        s, _, w = call(api, "POST", "/webhooks", key, {"url": args.webhook_url, "events": ["*"]})
        c.ok(s == 201 and w["data"]["secret"].startswith("whsec_"), f"public webhook registered ({args.webhook_url})")
        hook = w["data"]["webhook"]["id"]
        call(api, "POST", "/members", key, {"first_name": "Kim", "last_name": "Lee", "contact": "kim@fixture.invalid", "handle": "kim"})
        attempt = None
        for _ in range(40):
            s, _, dl = call(api, "GET", f"/webhooks/{hook}/deliveries", key)
            if s == 200 and dl["data"]:
                attempt = dl["data"][0]
                break
            time.sleep(1)
        c.ok(attempt is not None and attempt["status"] in ("delivered", "retrying"),
             f"a signed delivery reached the public endpoint ({attempt and attempt['status']}, HTTP {attempt and attempt['response_status']})")

        s, _, au = call(api, "GET", "/audit?action=api_key.use&limit=200", key)
        c.ok(s == 200 and au["data"] and all(x["actor"] == kid for x in au["data"]), "every use is audited with the key id")
        s, _, _ = call(api, "DELETE", "/key", key)
        c.ok(s == 200, "the key revoked itself")
        s, _, _ = call(api, "GET", "/org", key)
        c.ok(s == 401, "a revoked key is refused")
        print(f"PASS: {c.passed} checks")
    finally:
        if args.image:
            subprocess.run(["docker", "rm", "-f", container], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if proc and proc.poll() is None:
            proc.kill()
            proc.wait()
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
