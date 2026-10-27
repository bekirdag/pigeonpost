"""Shared helpers for the opt-in live tests against the disposable loopback postbox.

The fixture is tests/e2e-messages/postbox.mjs (started by tests/e2e-messages/linux.sh). It writes a
private setup file named by PIGEONPOST_TEST_FIXTURE:
    {postbox, lossProxy, people: {alice|bob|carol: {token, address}}, organizationId?}
Only loopback origins are accepted. Tokens are synthetic and never printed.
"""

import hashlib
import json
import os
import threading
import urllib.parse
import urllib.request

from pigeonpost.api import Postbox, Transport


def fixture():
    path = os.environ.get("PIGEONPOST_TEST_FIXTURE")
    if not path:
        raise SystemExit("PIGEONPOST_TEST_FIXTURE is not set; run tests/e2e-messages/linux.sh")
    with open(path) as handle:
        setup = json.load(handle)
    for key in ("postbox", "lossProxy"):
        parts = urllib.parse.urlsplit(setup[key])
        if parts.scheme != "http" or parts.hostname != "127.0.0.1":
            raise SystemExit("The live fixture must be loopback only.")
    return setup


class StaticSession:
    """A signed-in session with one synthetic fixture token. `owner` keeps local work per person;
    switch() simulates a sign-in to another account (a new login epoch)."""

    def __init__(self, token, owner=None):
        self.lock = threading.RLock()
        self.token = token
        self.epoch = 1
        self.owner = owner or hashlib.sha256(token.encode()).hexdigest()

    def access_token(self, rejected=None, lease=None):
        with self.lock:
            if lease is not None:
                lease.require()
            return self.token

    def switch(self, token, owner=None):
        with self.lock:
            self.token = token
            self.epoch += 1
            self.owner = owner or hashlib.sha256(token.encode()).hexdigest()


def postbox(setup, person, via_proxy=False, session=None):
    session = session or StaticSession(setup["people"][person]["token"])
    base = setup["lossProxy"] if via_proxy else setup["postbox"]
    return Postbox(session, Transport(allow_loopback_http=True), base=base), session


def loss(setup, mode):
    """The proxy's committed-response-loss mode: off | before | after."""
    request = urllib.request.Request(setup["lossProxy"] + "/__test/control", data=json.dumps({"mode": mode}).encode(),
                                     method="POST", headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(request, timeout=10) as response:
        response.read()


def attempts(setup):
    with urllib.request.urlopen(setup["lossProxy"] + "/__test/attempts", timeout=10) as response:
        return json.load(response)["attempts"]


class Checks:
    def __init__(self, title):
        self.title, self.count = title, 0

    def ok(self, condition, what):
        if not condition:
            raise AssertionError(what)
        self.count += 1
        print("  ok", self.count, "-", what, flush=True)

    def done(self):
        print(f"{self.title}: {self.count} checks passed", flush=True)
