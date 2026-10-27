"""Strong sign-in (step-up) for sensitive approvals: Authorization Code + PKCE (S256) in the system
browser with an exact loopback redirect (RFC 8252), bound to the login that asked for it.

* `StrongSignInRequirement` reads `acr_values`/`max_age` from a `401 step_up_required` JSON body or
  its `WWW-Authenticate: Bearer …` challenge. Challenge data can ask for a method and a recency,
  never an issuer, client or redirect. Quoted strings are parsed strictly; duplicates, controls and
  oversized values are refused.
* `LoopbackReceiver` listens on 127.0.0.1 only, on a port the system chooses, for one redirect.
  Requests are bounded (16 KiB of header, 8 KiB of target, two-second reads, ten minutes overall);
  anything that is not exactly `GET /?code=…&state=…` with the expected state is answered 400 and
  does not consume the pending return. The fixed browser page carries no code or state and is sent
  with `Cache-Control: no-store` and `Referrer-Policy: no-referrer`.
* `StrongSignIn.run` makes a fresh state, nonce and verifier (kept in memory), opens the browser,
  waits for the redirect, exchanges the code once at the fixed token endpoint and checks the
  result: issuer, audience/azp, nonce, the requested method and recency, valid clocks, and — before
  anything is adopted — that the new tokens belong to the same person as the accepted login
  (binding.owner_key). Only then are they handed to the session, under its lock, while the lease is
  still current. It never approves anything by itself.

Availability is a capability of the OAuth client, not of this code: the dedicated Linux client
`pigeonpost-linux` is configured for device authorization only (no standard flow, no redirect
URIs), so `supported(CLIENT_ID)` is False and the app asks the person to approve in the browser.
"""

import base64
import hashlib
import json
import secrets
import socket
import threading
import time
import urllib.parse

from .api import APIError, CLIENT_ID, ISSUER
from .binding import Retired, owner_key

# OAuth clients configured for Authorization Code + PKCE S256 with the native loopback redirect
# http://127.0.0.1/* (any port). `pigeonpost-linux` is not: it has device flow only.
STRONG_SIGNIN_CLIENTS = frozenset()

MAX_HEADER = 16 * 1024
MAX_TARGET = 8 * 1024
READ_TIMEOUT = 2.0
OVERALL_TIMEOUT = 600.0
CLOCK_SKEW = 30
PAGE = (b"<!doctype html><meta charset=utf-8><title>Pigeonpost</title>"
        b"<p>You can close this tab and return to Pigeonpost.</p>")


def supported(client_id=CLIENT_ID):
    return client_id in STRONG_SIGNIN_CLIENTS


class StrongSignInError(Exception):
    """Fixed messages; never a token, code or subject."""


class StrongSignInRequirement:
    __slots__ = ("acr_values", "max_age")

    def __init__(self, acr_values, max_age):
        self.acr_values, self.max_age = tuple(acr_values), max_age

    def __eq__(self, other):
        return isinstance(other, StrongSignInRequirement) and (other.acr_values, other.max_age) == (self.acr_values, self.max_age)

    def __repr__(self):
        return f"StrongSignInRequirement(methods={len(self.acr_values)}, max_age={self.max_age})"

    @property
    def requested_acr(self):
        return " ".join(self.acr_values)

    @classmethod
    def create(cls, methods, max_age):
        values = list(methods)[:9]
        if (isinstance(max_age, bool) or not isinstance(max_age, int) or not 0 <= max_age <= 600
                or not 1 <= len(values) <= 8 or len(set(values)) != len(values)):
            return None
        for v in values:
            if not isinstance(v, str) or not 1 <= len(v) <= 128 or not all(c.isascii() and (c.isalnum() or c in ":/._-") for c in v):
                return None
        return cls(values, max_age)

    @classmethod
    def parse(cls, body, header):
        params = bearer_parameters(header)
        if params is not None and "max_age" in params and "acr_values" in params:
            age = params["max_age"]
            if 1 <= len(age) <= 3 and age.isascii() and age.isdigit():
                found = cls.create(params["acr_values"].split(), int(age))
                if found:
                    return found
        if not isinstance(body, dict):
            return None
        age, raw = body.get("max_age"), body.get("acr_values")
        if isinstance(age, bool) or not isinstance(age, int):
            return None
        if isinstance(raw, str):
            return cls.create(raw.split(), age)
        if isinstance(raw, list) and len(raw) <= 8 and all(isinstance(v, str) for v in raw):
            return cls.create(raw, age)
        return None


def bearer_parameters(header):
    """RFC 6750 challenge parameters; quoted strings may not introduce another parameter."""
    if (not isinstance(header, str) or len(header) > 2048 or not header.lower().startswith("bearer ")
            or any(ord(c) < 32 or ord(c) == 127 for c in header)):
        return None
    fields, i, result = header[7:], 0, {}
    n = len(fields)
    while i < n:
        while i < n and fields[i] == " ":
            i += 1
        start = i
        while i < n and fields[i].isascii() and (fields[i].isalnum() or fields[i] in "_-"):
            i += 1
        if i == start:
            return None
        name = fields[start:i].lower()
        while i < n and fields[i] == " ":
            i += 1
        if i >= n or fields[i] != "=":
            return None
        i += 1
        while i < n and fields[i] == " ":
            i += 1
        if i >= n:
            return None
        value = []
        if fields[i] == '"':
            i += 1
            closed = False
            while i < n:
                c = fields[i]
                i += 1
                if c == '"':
                    closed = True
                    break
                if c == "\\":
                    if i >= n or fields[i] not in '\\"':
                        return None
                    value.append(fields[i])
                    i += 1
                else:
                    value.append(c)
            if not closed:
                return None
        else:
            begin = i
            while i < n and fields[i] not in ", ":
                if fields[i] in '"\\' or ord(fields[i]) > 126:
                    return None
                value.append(fields[i])
                i += 1
            if i == begin:
                return None
        if name in result:
            return None
        result[name] = "".join(value)
        while i < n and fields[i] == " ":
            i += 1
        if i == n:
            return result
        if fields[i] != "," or i + 1 == n:
            return None
        i += 1
    return None


def b64url(data):
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def claims(token):
    """Payload of a JWT that came straight from the fixed token endpoint over TLS (not a standalone
    signature check; this binds claims of a trusted exchange)."""
    if not isinstance(token, str) or token.count(".") != 2 or len(token) > 16384:
        raise StrongSignInError("The sign-in returned an unexpected token.")
    part = token.split(".")[1]
    try:
        value = json.loads(base64.urlsafe_b64decode(part + "=" * (-len(part) % 4)))
    except (ValueError, UnicodeError):
        raise StrongSignInError("The sign-in returned an unexpected token.") from None
    if not isinstance(value, dict):
        raise StrongSignInError("The sign-in returned an unexpected token.")
    return value


class LoopbackReceiver:
    """One redirect to http://127.0.0.1:<port>/ for one expected state."""

    def __init__(self, expected_state, overall=OVERALL_TIMEOUT, read_timeout=READ_TIMEOUT):
        self.expected_state = expected_state
        self.overall = overall
        self.read_timeout = read_timeout
        self.cancelled = threading.Event()
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.sock.bind(("127.0.0.1", 0))
        self.sock.listen(4)
        self.sock.settimeout(0.25)
        self.port = self.sock.getsockname()[1]

    @property
    def redirect_uri(self):
        return f"http://127.0.0.1:{self.port}/"

    def cancel(self):
        self.cancelled.set()

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass

    def wait(self):
        """The authorization code, or StrongSignInError. Always releases the listener."""
        deadline = time.monotonic() + self.overall
        try:
            while not self.cancelled.is_set():
                if time.monotonic() > deadline:
                    raise StrongSignInError("The sign-in took too long. Try again.")
                try:
                    conn, _ = self.sock.accept()
                except socket.timeout:
                    continue
                except OSError:
                    raise StrongSignInError("The sign-in could not be completed.") from None
                with conn:
                    outcome = self._handle(conn)
                if outcome is not None:
                    return outcome
            raise StrongSignInError("Sign-in was cancelled.")
        finally:
            self.close()

    def _reply(self, conn, status, body):
        head = (f"HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {len(body)}\r\n"
                "Cache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n").encode()
        try:
            conn.sendall(head + body)
        except OSError:
            pass

    def _handle(self, conn):
        conn.settimeout(self.read_timeout)
        data = b""
        try:
            while b"\r\n\r\n" not in data:
                chunk = conn.recv(4096)
                if not chunk:
                    return None
                data += chunk
                if len(data) > MAX_HEADER:
                    self._reply(conn, "431 Request Header Fields Too Large", b"")
                    return None
        except (socket.timeout, OSError):
            return None
        line = data.split(b"\r\n", 1)[0]
        parts = line.split(b" ")
        if len(parts) != 3 or parts[0] != b"GET" or not parts[2].startswith(b"HTTP/1.") or len(parts[1]) > MAX_TARGET:
            self._reply(conn, "400 Bad Request", b"")
            return None
        try:
            target = parts[1].decode("ascii")
        except UnicodeDecodeError:
            self._reply(conn, "400 Bad Request", b"")
            return None
        path, _, query = target.partition("?")
        if path != "/" or "#" in target:
            self._reply(conn, "404 Not Found", b"")
            return None
        pairs = urllib.parse.parse_qsl(query, keep_blank_values=True, strict_parsing=False)
        names = [k for k, _ in pairs]
        fields = dict(pairs)
        if len(names) != len(set(names)) or fields.get("state") != self.expected_state:
            self._reply(conn, "400 Bad Request", b"")
            return None
        if "error" in fields:
            self._reply(conn, "200 OK", PAGE)
            raise StrongSignInError("The sign-in was not completed.")
        code = fields.get("code")
        if not code or len(code) > 2048 or set(fields) - {"code", "state", "iss", "session_state"}:
            self._reply(conn, "400 Bad Request", b"")
            return None
        self._reply(conn, "200 OK", PAGE)
        return {"code": code, "iss": fields.get("iss")}


class StrongSignIn:
    """The system-browser strong sign-in for one session. Not available for `pigeonpost-linux`."""

    def __init__(self, session, transport=None, issuer=ISSUER, client_id=CLIENT_ID, open_url=None,
                 now=time.time, clients=None, overall=OVERALL_TIMEOUT):
        self.session = session
        self.transport = transport or getattr(session, "transport", None)
        self.issuer = issuer.rstrip("/")
        self.client_id = client_id
        self.open_url = open_url
        self.now = now
        self.clients = STRONG_SIGNIN_CLIENTS if clients is None else frozenset(clients)
        self.overall = overall
        self.lock = threading.Lock()
        self.pending = None

    @property
    def available(self):
        return self.client_id in self.clients

    def cancel(self):
        with self.lock:
            if self.pending is not None:
                self.pending.cancel()

    def authorize_url(self, requirement, redirect, state, nonce, challenge):
        query = {"client_id": self.client_id, "response_type": "code", "scope": "openid",
                 "redirect_uri": redirect, "state": state, "nonce": nonce, "code_challenge": challenge,
                 "code_challenge_method": "S256", "acr_values": requirement.requested_acr,
                 "max_age": str(requirement.max_age), "prompt": "login"}
        return self.issuer + "/protocol/openid-connect/auth?" + urllib.parse.urlencode(query)

    def run(self, requirement, lease):
        """Blocking. Adopts the verified tokens into the session (same person, same login) and
        returns True; raises StrongSignInError or Retired otherwise. Never approves anything."""
        if not self.available:
            raise StrongSignInError("Strong sign-in is not available in this app.")
        if not isinstance(requirement, StrongSignInRequirement):
            raise StrongSignInError("No verification was requested.")
        lease.require()
        if not lease.owner:
            raise StrongSignInError("Finish signing in before verifying.")
        state, nonce = secrets.token_urlsafe(32), secrets.token_urlsafe(32)
        verifier = secrets.token_urlsafe(64)
        challenge = b64url(hashlib.sha256(verifier.encode()).digest())
        receiver = LoopbackReceiver(state, overall=self.overall)
        with self.lock:
            if self.pending is not None:
                receiver.close()
                raise StrongSignInError("A verification is already open in your browser.")
            self.pending = receiver
        try:
            started = self.now()
            url = self.authorize_url(requirement, receiver.redirect_uri, state, nonce, challenge)
            if self.open_url:
                self.open_url(url)
            returned = receiver.wait()
            lease.require()
            if returned.get("iss") is not None and returned["iss"] != self.issuer:
                raise StrongSignInError("The sign-in came from an unexpected server.")
            result = self.transport.request("POST", self.issuer + "/protocol/openid-connect/token", timeout=20, form={
                "client_id": self.client_id, "grant_type": "authorization_code", "code": returned["code"],
                "redirect_uri": receiver.redirect_uri, "code_verifier": verifier})
            self.check(result, requirement, nonce, lease, started)
            self.adopt(result, lease)
            return True
        except APIError as error:
            if isinstance(error, Retired):
                raise
            raise StrongSignInError("The sign-in could not be verified. Try again.") from None
        finally:
            with self.lock:
                if self.pending is receiver:
                    self.pending = None
            receiver.close()

    def check(self, result, requirement, nonce, lease, started):
        if not isinstance(result, dict) or not result.get("access_token") or not result.get("id_token"):
            raise StrongSignInError("The sign-in returned an unexpected response.")
        id_claims = claims(result["id_token"])
        access_claims = claims(result["access_token"])
        now = self.now()
        audience = id_claims.get("aud")
        audiences = audience if isinstance(audience, list) else [audience]
        acr = id_claims.get("acr")
        auth_time = id_claims.get("auth_time")
        ok = (id_claims.get("iss") == self.issuer and access_claims.get("iss") == self.issuer
              and self.client_id in audiences and id_claims.get("azp", self.client_id) == self.client_id
              and id_claims.get("nonce") == nonce
              and isinstance(acr, str) and acr in requirement.acr_values
              and isinstance(auth_time, int) and auth_time >= started - CLOCK_SKEW
              and now - auth_time <= requirement.max_age + CLOCK_SKEW
              and isinstance(id_claims.get("exp"), int) and id_claims["exp"] > now - CLOCK_SKEW
              and isinstance(access_claims.get("exp"), int) and access_claims["exp"] > now - CLOCK_SKEW
              and isinstance(id_claims.get("iat"), int) and id_claims["iat"] <= now + CLOCK_SKEW
              and id_claims.get("sub") == access_claims.get("sub"))
        if not ok:
            raise StrongSignInError("The sign-in could not be verified. Try again.")
        if owner_key(result["access_token"]) != lease.owner:
            raise StrongSignInError("Verify with the same account that is signed in here.")

    def adopt(self, result, lease):
        """Hand the tokens to the session for the same login. The renewal keeps the epoch because
        the owner is unchanged; anything else is refused before it is stored."""
        with self.session.lock:
            lease.require()
            if owner_key(result["access_token"]) != self.session.owner:
                raise StrongSignInError("Verify with the same account that is signed in here.")
            self.session._accept(result)
            lease.require()
