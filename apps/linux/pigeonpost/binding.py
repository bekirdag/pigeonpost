"""Work stays tied to the login, account and mailbox that started it.

A `Lease` captures the session's accepted-login epoch and owner when work begins. Every request a
bound postbox makes, and every protected local write, first checks that the same login is still the
accepted one: after a sign-out, a sign-in to another account or a failed renewal the lease is
retired and the work stops with `Retired` instead of running under the newer login. Results are
published only while the lease is still current, so an old account's answer never lands in a newer
one. A lease never grants anything by itself; the postbox still authorizes every request.
"""

import base64
import hashlib
import json
import threading

from .api import APIError


class Retired(APIError):
    """The login (or mailbox selection) this work belonged to is no longer the accepted one."""

    def __init__(self):
        super().__init__(0, "retired", "This action belonged to an earlier sign-in and was stopped.")


def owner_key(access_token):
    """SHA-256 of issuer and subject from the access token's claims, or None.

    Used only to keep this account's local work apart from another account's on the same
    computer; it is not a credential and authorizes nothing (the postbox checks the token)."""
    if not isinstance(access_token, str) or access_token.count(".") != 2 or len(access_token) > 16384:
        return None
    try:
        part = access_token.split(".")[1]
        claims = json.loads(base64.urlsafe_b64decode(part + "=" * (-len(part) % 4)))
    except (ValueError, UnicodeError):
        return None
    if not isinstance(claims, dict):
        return None
    issuer, subject = claims.get("iss"), claims.get("sub")
    if not isinstance(issuer, str) or not isinstance(subject, str) or not issuer or not subject:
        return None
    return hashlib.sha256((issuer + "\0" + subject).encode()).hexdigest()


class Lease:
    """One accepted login (and optionally one mailbox). Cheap to make; check it often."""

    def __init__(self, session, mailbox=None, parent=None):
        self.session = session
        if parent is not None:
            self.epoch, self.owner = parent.epoch, parent.owner
        else:
            with session.lock:
                self.epoch, self.owner = session.epoch, session.owner
        self.mailbox = mailbox
        self.parent = parent
        self._retired = threading.Event()

    def child(self, mailbox):
        """The same login, narrowed to one mailbox. Retiring the parent retires the child."""
        return Lease(self.session, mailbox, parent=self)

    def retire(self):
        self._retired.set()

    @property
    def current(self):
        lease = self
        while lease is not None:
            if lease._retired.is_set():
                return False
            lease = lease.parent
        return self.session.epoch == self.epoch and self.session.owner == self.owner

    def require(self):
        if not self.current:
            raise Retired()
        return self

    def __repr__(self):
        return "Lease(redacted)"
