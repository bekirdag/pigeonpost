"""Production REST transport. No GUI dependency; credentials never cross redirects."""

import json
import urllib.error
import urllib.parse
import urllib.request
from . import VERSION

POSTBOX = "https://postbox.pigeonpost.dev"
ISSUER = "https://auth.pigeonpost.dev/realms/pigeonpost-prod"
CLIENT_ID = "pigeonpost-linux"
MAX_FILE = 25 * 1024 * 1024


class APIError(Exception):
    def __init__(self, status=0, code="network_error", detail="", body=None):
        self.status, self.code = status, code
        self.body = body if isinstance(body, dict) else {}
        messages = {
            "network_error": "Could not reach Pigeonpost. Check your connection and try again.",
            "invalid_grant": "Your session has expired. Please sign in again.",
            "access_denied": "Sign-in was declined. You can try again.",
            "expired_token": "This sign-in code expired. Please start again.",
            "invalid_response": "Pigeonpost returned an unexpected response. Please try again.",
            "redirect_refused": "An unexpected server redirect was refused.",
        }
        message = messages.get(code) or (detail[:300] if isinstance(detail, str) else "")
        super().__init__(message or f"Pigeonpost could not complete this request ({code or status}).")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise APIError(code, "redirect_refused")


class Transport:
    def __init__(self, allow_loopback_http=False):
        self.opener = urllib.request.build_opener(NoRedirect())
        # Tests against a locally started postbox only; the app never sets this.
        self.allow_loopback_http = allow_loopback_http

    def request(self, method, url, *, data=None, form=None, token=None, headers=None,
                raw=None, binary=False, timeout=35):
        parts = urllib.parse.urlsplit(url)
        loopback = self.allow_loopback_http and parts.scheme == "http" and parts.hostname == "127.0.0.1"
        if parts.scheme != "https" and not loopback:
            raise APIError(0, "invalid_response", "HTTPS is required.")
        hdr = {"Accept": "application/json", "User-Agent": "Pigeonpost-Linux/" + VERSION}
        if token:
            hdr["Authorization"] = "Bearer " + token
        if data is not None:
            raw = json.dumps(data).encode()
            hdr["Content-Type"] = "application/json"
        if form is not None:
            raw = urllib.parse.urlencode(form).encode()
            hdr["Content-Type"] = "application/x-www-form-urlencoded"
        hdr.update(headers or {})
        req = urllib.request.Request(url, data=raw, method=method, headers=hdr)
        try:
            with self.opener.open(req, timeout=timeout) as response:
                limit = MAX_FILE if binary else 16 * 1024 * 1024
                body = response.read(limit + 1)
                if len(body) > limit:
                    raise APIError(0, "response_too_large", "This response is too large to open.")
                if binary:
                    return body
                result = json.loads(body) if body else {}
                if not isinstance(result, dict):
                    raise APIError(0, "invalid_response")
                return result
        except urllib.error.HTTPError as exc:
            with exc:
                try:
                    error = json.loads(exc.read(4096))
                    if not isinstance(error, dict):
                        error = {}
                except (ValueError, UnicodeError):
                    error = {}
            raise APIError(exc.code, error.get("error", f"http_{exc.code}"),
                           error.get("detail") or error.get("message") or "", error) from None
        except (urllib.error.URLError, OSError, TimeoutError):
            raise APIError() from None
        except (ValueError, UnicodeError):
            raise APIError(0, "invalid_response") from None


class Postbox:
    def __init__(self, session, transport=None, base=POSTBOX):
        self.session = session
        self.transport = transport or Transport()
        self.base = base.rstrip("/")

    def call(self, method, path, identity=None, data=None, query=None, **kwargs):
        params = dict(query or {})
        if identity:
            # Send both supported selectors. Attachments use headers; other mailbox endpoints
            # parse query/body scoping. This also supports quota's explicit selector fallback.
            kwargs["headers"] = {**kwargs.get("headers", {}), "x-pigeonpost-identity": identity}
        if identity and method in ("GET", "DELETE"):
            params["identity"] = identity
        elif identity and data is not None:
            data = dict(data, identity=identity)
        url = self.base + path + ("?" + urllib.parse.urlencode(params) if params else "")
        return self._send(method, url, data=data, **kwargs)

    def _send(self, method, url, **kwargs):
        token = self.session.access_token()
        try:
            return self.transport.request(method, url, token=token, **kwargs)
        except APIError as exc:
            if exc.status != 401:
                raise
            # A 401 rejects the operation before it is processed. Other failures never replay writes.
            token = self.session.access_token(rejected=token)
            return self.transport.request(method, url, token=token, **kwargs)

    def scoped(self, method, path, identity, data=None, *, device=None, raw=None,
               content_type="application/octet-stream", binary=False, timeout=35):
        """Group and MLS routes: the acting mailbox and device travel only as headers, so the JSON
        body (and with it the idempotency hash of `request_key`) is exactly what the caller built."""
        headers = {"x-pigeonpost-identity": identity}
        if device:
            headers["x-pigeonpost-device"] = device
        if raw is not None:
            headers["Content-Type"] = content_type
        return self._send(method, self.base + path, data=data, raw=raw, headers=headers,
                          binary=binary, timeout=timeout)

    def capabilities(self):
        """Features this postbox switched on for the account; an empty set when it cannot say."""
        try:
            result = self.call("GET", "/v1/capabilities")
        except APIError as exc:
            if exc.code == "invalid_grant":
                raise
            return frozenset()
        features = result.get("features")
        return frozenset(str(f) for f in features) if isinstance(features, list) else frozenset()

    def identities(self):
        rows = self.call("GET", "/v1/identities").get("identities", [])
        for row in rows:
            try:
                row["handle"] = self.call("GET", "/v1/whoami", row["address"]).get("handle")
            except APIError:
                pass  # A failed name lookup must not hide a working mailbox.
        return rows

    def inbox(self, identity, wait=0):
        return self.call("GET", "/v1/inbox", identity, query={
            "include_sent": "true", "include_read": "true", "wait": str(wait)
        }).get("messages", [])

    def upload(self, identity, name, media_type, content):
        if len(content) > MAX_FILE:
            raise APIError(0, "file_too_large", "Choose a file smaller than 25 MiB.")
        def header(value):
            return "".join(c for c in value if 32 <= ord(c) <= 126 and c not in '\\"')[:120]
        return self.call("POST", "/v1/attachments", raw=content, timeout=120, headers={
            "Content-Type": "application/octet-stream", "x-pigeonpost-identity": identity,
            "x-pigeonpost-filename": header(name) or "attachment", "x-pigeonpost-media-type": header(media_type),
        })

    def download(self, identity, attachment_id):
        return self.call("GET", "/v1/attachments/" + urllib.parse.quote(attachment_id, safe=""),
                         binary=True, timeout=120, headers={"x-pigeonpost-identity": identity})
