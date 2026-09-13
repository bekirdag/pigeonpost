"""OAuth device authorization and serialized refresh-token rotation."""

import threading
import time
from urllib.parse import urlsplit

from .api import APIError, CLIENT_ID, ISSUER, Transport


class Session:
    def __init__(self, vault, transport=None):
        self.vault, self.transport = vault, transport or Transport()
        self.lock = threading.RLock()
        self.token = self.refresh = None
        self.expires = 0

    def form(self, endpoint, **values):
        return self.transport.request("POST", ISSUER + "/protocol/openid-connect/" + endpoint,
                                      form={"client_id": CLIENT_ID, **values}, timeout=20)

    def _accept(self, result):
        if not result.get("access_token") or not result.get("refresh_token", self.refresh):
            raise APIError(0, "invalid_response")
        refresh = result.get("refresh_token", self.refresh)
        # Persist first. A keyring failure must not silently create a supposedly remembered session.
        self.vault.save(refresh)
        self.refresh, self.token = refresh, result["access_token"]
        self.expires = time.monotonic() + max(0, int(result.get("expires_in", 60)) - 30)

    def restore(self):
        with self.lock:
            self.refresh = self.vault.load()
            if not self.refresh:
                return False
            self.access_token()
            return True

    def access_token(self, rejected=None):
        with self.lock:
            if self.token and time.monotonic() < self.expires and self.token != rejected:
                return self.token
            if not self.refresh:
                raise APIError(401, "invalid_grant")
            try:
                self._accept(self.form("token", grant_type="refresh_token", refresh_token=self.refresh))
            except APIError as exc:
                if exc.code == "invalid_grant":
                    self.vault.clear()
                    self.token = self.refresh = None
                raise
            return self.token

    def begin(self):
        result = self.form("auth/device", scope="openid profile offline_access")
        # Only launch this issuer's verification page, never an arbitrary server-supplied URL.
        for key in ("verification_uri", "verification_uri_complete"):
            if result.get(key):
                url = urlsplit(result[key])
                if url.scheme != "https" or url.netloc != "auth.pigeonpost.dev" or not url.path.startswith("/realms/pigeonpost-prod/"):
                    raise APIError(0, "invalid_response")
        if not all(result.get(k) for k in ("device_code", "user_code", "verification_uri", "expires_in")):
            raise APIError(0, "invalid_response")
        return result

    def complete(self, device, cancel):
        until = time.monotonic() + int(device["expires_in"])
        interval = max(1, int(device.get("interval", 5)))
        while time.monotonic() < until:
            if cancel.wait(interval):
                return False
            try:
                result = self.form("token", grant_type="urn:ietf:params:oauth:grant-type:device_code",
                                   device_code=device["device_code"])
                with self.lock:
                    if cancel.is_set():
                        return False
                    self._accept(result)
                    if cancel.is_set():
                        self.sign_out()
                        return False
                return True
            except APIError as exc:
                if exc.code == "authorization_pending":
                    continue
                if exc.code == "slow_down":
                    interval += 5
                    continue
                if exc.code == "network_error":
                    interval = min(30, interval + 5)
                    continue
                raise
        raise APIError(0, "expired_token")

    def sign_out(self):
        with self.lock:
            refresh = self.refresh
            self.vault.clear()  # Do not claim local sign-out if credential removal fails.
            self.token = self.refresh = None
            self.expires = 0
        if refresh:
            try:
                self.form("logout", refresh_token=refresh)
            except APIError:
                pass  # Local credentials are gone even when the server cannot be reached.
