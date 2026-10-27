"""Approving a machine (docs/device-pairing-native.md, docs/orgs.md §8): the rules, no GUI.

`ApprovalStore` belongs to one accepted login (binding.Lease) for its whole life. Every network
answer is applied only while that login is still current and the operation that asked is still the
latest one, so a late answer from an earlier sign-in or an abandoned code never changes the screen.

* The code is parsed strictly (`ABCD-EFGH`, or the fragment of
  https://inbox.pigeonpost.dev/pair.html#ABCD-EFGH), kept in memory only and never shown in a repr,
  a log or a query string.
* Nothing is decided on arrival. Approve and Deny each need the exact review the person saw (a
  proof id that changes with every state change); a decided, expired or unrecognized request never
  offers another approval.
* A lost answer is never resent. An approval whose answer is lost is UNCONFIRMED until the person
  checks the status again (a read). A denial's acknowledgment does not prove that a concurrent
  approval did not win, so the request is resolved again afterwards.
* A strong sign-in (step-up) never approves anything: after it, the request is resolved again and
  needs another explicit hold.
"""

import re
import secrets
import threading
import time
import urllib.parse

from .actions import canonical_mailbox
from .api import APIError
from .strong_signin import StrongSignInRequirement

CODE = re.compile(r"[A-Z0-9]{4}-?[A-Z0-9]{4}")
PAIR_HOST = "inbox.pigeonpost.dev"
PAIR_PAGE = "https://inbox.pigeonpost.dev/pair.html"
IDENT = re.compile(r"[A-Za-z0-9_-]{1,128}")
BIDI = set(range(0x202A, 0x202F)) | set(range(0x2066, 0x206A)) | {0x200E, 0x200F, 0x061C}

IDLE, RESOLVING, REVIEW, APPROVING, DENYING, VERIFYING = "idle", "resolving", "review", "approving", "denying", "verifying"
APPROVED, DENIED, EXPIRED, UNCONFIRMED, UNAVAILABLE = "approved", "denied", "expired", "unconfirmed", "unavailable"
BUSY = {RESOLVING, APPROVING, DENYING, VERIFYING}

REASONS = {
    "delegation_required": "Ask an agent admin to add you as one of its operators.",
    "org_inactive": "This organization is not active.",
}


class PairingCode:
    """A short-lived terminal proof. Its value never appears in repr/str."""
    __slots__ = ("raw",)

    def __init__(self, raw):
        self.raw = raw

    @property
    def display(self):
        return self.raw[:4] + "-" + self.raw[4:]

    def __eq__(self, other):
        return isinstance(other, PairingCode) and other.raw == self.raw

    def __hash__(self):
        return hash(self.raw)

    def __repr__(self):
        return "PairingCode(redacted)"

    __str__ = __repr__


def parse_code(text):
    if not isinstance(text, str) or len(text) > 128 or any(ord(c) > 127 for c in text):
        return None
    value = text.strip(" \t\r\n").upper()
    if not CODE.fullmatch(value):
        return None
    return PairingCode(value.replace("-", ""))


def parse_link(text):
    """Only https://inbox.pigeonpost.dev/pair.html#CODE: no port, user, query or other path."""
    if not isinstance(text, str) or len(text) > 4096 or any(not 32 <= ord(c) <= 126 for c in text):
        return None
    try:
        parts = urllib.parse.urlsplit(text)
    except ValueError:
        return None
    if (parts.scheme.lower() != "https" or parts.netloc.lower() != PAIR_HOST or parts.path != "/pair.html"
            or parts.query or "?" in text or not parts.fragment or parts.fragment != parts.fragment.strip()):
        return None
    return parse_code(parts.fragment)


def parse_input(text):
    """A pasted code or pairing link."""
    text = (text or "").strip()
    return parse_link(text) if text.lower().startswith("https://") else parse_code(text)


def browser_link(code):
    """The web page that can approve with its own strong sign-in. The code goes in the fragment,
    never a query string, so it is not sent to any server by opening the page."""
    return PAIR_PAGE + "#" + code.display


def safe_text(value, limit):
    if not isinstance(value, str) or not value or len(value.encode()) > limit:
        return False
    return not any(ord(c) < 32 or 0x7F <= ord(c) < 0xA0 or ord(c) in BIDI for c in value)


def valid_handle(value):
    if not isinstance(value, str) or len(value) > 160 or value.startswith("/k/") or value.count("/") != 2:
        return False
    parts = value[1:].split("/")
    return value.startswith("/") and all(p and re.fullmatch(r"[a-z0-9][a-z0-9_.-]{0,63}", p) for p in parts)


def check_preview(raw):
    """`POST /v1/device-pairings/resolve` answer → a preview dict, or ValueError for anything off."""
    if not isinstance(raw, dict):
        raise ValueError("preview")
    org = raw.get("org")
    caps = raw.get("capabilities")
    confirmation = raw.get("confirmation")
    address, creates = raw.get("address"), raw.get("creates_agent")
    state = raw.get("state")
    ok = (isinstance(raw.get("pairing_id"), str) and IDENT.fullmatch(raw["pairing_id"])
          and isinstance(org, dict) and isinstance(org.get("id"), str) and IDENT.fullmatch(org["id"]) and safe_text(org.get("name"), 320)
          and valid_handle(raw.get("handle"))
          and ((address is None and creates is True) or (canonical_mailbox(address) and creates is not True))
          and (raw.get("label") is None or safe_text(raw.get("label"), 320))
          and isinstance(confirmation, str) and re.fullmatch(r"\d{3} ?\d{3}", confirmation)
          and isinstance(caps, list) and 1 <= len(caps) <= 32 and len(set(caps)) == len(caps)
          and all(isinstance(c, str) and IDENT.fullmatch(c) for c in caps)
          and (raw.get("ip_country") is None or safe_text(raw.get("ip_country"), 80))
          and (raw.get("reason") is None or safe_text(raw.get("reason"), 160))
          and all(isinstance(raw.get(k), int) and not isinstance(raw.get(k), bool) for k in ("requested_at", "expires_at"))
          and 0 <= raw["requested_at"] < raw["expires_at"] <= 253402300799
          and isinstance(raw.get("can_approve", False), bool)
          and state in (None, "pending", "approved", "denied"))
    if not ok:
        raise ValueError("preview")
    digits = confirmation.replace(" ", "")
    return {"pairing_id": raw["pairing_id"], "org": {"id": org["id"], "name": org["name"]}, "handle": raw["handle"],
            "address": address, "creates_agent": address is None, "label": raw.get("label"),
            "confirmation": digits[:3] + " " + digits[3:], "capabilities": list(caps), "ip_country": raw.get("ip_country"),
            "requested_at": raw["requested_at"], "expires_at": raw["expires_at"], "can_approve": raw.get("can_approve") is True,
            "reason": raw.get("reason"), "state": state}


def allows_decision(preview, now):
    return preview["state"] in (None, "pending") and preview["expires_at"] > now and preview["requested_at"] <= now + 60


def allows_approval(preview, now):
    return allows_decision(preview, now) and preview["can_approve"] and preview["capabilities"] == ["mailbox"]


CAPABILITY_TEXT = {"mailbox": "Send and receive as this agent’s inbox"}


class PairingAPI:
    """Fixed, code-only routes. The person's own session; never a mailbox selector."""

    def __init__(self, api):
        self.api = api

    def resolve(self, code):
        return check_preview(self.api.call("POST", "/v1/device-pairings/resolve", data={"user_code": code.display}))

    def _decide(self, preview, code, verb, expected):
        if not IDENT.fullmatch(preview["pairing_id"]):
            raise ValueError("pairing")
        result = self.api.call("POST", f"/v1/device-pairings/{preview['pairing_id']}/{verb}", data={"user_code": code.display})
        if not isinstance(result, dict) or result.get(expected) is not True:
            raise APIError(0, "invalid_response")
        return result

    def approve(self, preview, code):
        result = self._decide(preview, code, "approve", "approved")
        # The acknowledgment must name exactly the agent the person reviewed.
        if result.get("handle") != preview["handle"] or not canonical_mailbox(result.get("address")) or (
                preview["address"] and result.get("address") != preview["address"]):
            raise APIError(0, "invalid_response")
        return result

    def deny(self, preview, code):
        return self._decide(preview, code, "deny", "denied")


class ApprovalStore:
    """One login's device approvals. Blocking methods run on workers; read `snapshot()` after."""

    def __init__(self, lease, api, now=time.time, strong_available=False):
        self.lease = lease
        self.api = api
        self.now = now
        self.strong_available = strong_available
        self.lock = threading.RLock()
        self.op = None
        self.rate_until = None
        self._reset()

    def _reset(self):
        self.phase = IDLE
        self.code = None
        self.preview = None
        self.challenge = None
        self.message = None
        self.proof = secrets.token_hex(8)

    def __repr__(self):
        return "ApprovalStore(redacted)"

    # ---- state ----------------------------------------------------------------------------------

    @property
    def current(self):
        return self.lease is not None and self.lease.current

    def snapshot(self):
        with self.lock:
            return {"phase": self.phase, "preview": dict(self.preview) if self.preview else None, "message": self.message,
                    "proof": self.proof, "challenge": self.challenge, "retry_at": self.rate_until,
                    "code": self.code.display if self.code else None, "busy": self.phase in BUSY,
                    "can_approve": self._can_approve(self.proof), "can_deny": self._can_deny(self.proof),
                    "needs_browser": self._needs_browser()}

    def _can_approve(self, proof):
        return (self.current and proof == self.proof and self.phase == REVIEW and self.challenge is None
                and self.preview is not None and allows_approval(self.preview, self.now()) and self.strong_available)

    def _needs_browser(self):
        """Native approval needs a strong sign-in this app's OAuth client cannot do: the reviewed,
        approvable request is approved in the browser instead."""
        return (self.current and self.phase == REVIEW and self.preview is not None and not self.strong_available
                and allows_approval(self.preview, self.now()))

    def _can_deny(self, proof):
        return (self.current and proof == self.proof and self.phase == REVIEW and self.preview is not None
                and allows_decision(self.preview, self.now()) and self.preview["can_approve"])

    def refresh_expiry(self):
        with self.lock:
            if self.rate_until is not None and self.rate_until <= self.now():
                self.rate_until = None
            if self.phase == REVIEW and self.preview and self.preview["expires_at"] <= self.now():
                self.phase, self.challenge = EXPIRED, None
                self.proof = secrets.token_hex(8)
                self.message = "This request expired. Start again on the machine."
                return True
            return False

    def clear(self):
        with self.lock:
            if self.phase in BUSY:
                self.op = None  # whatever is in flight is dropped when it answers
            self._reset()

    def retire(self):
        with self.lock:
            self.op = None
            self._reset()
            self.lease = None

    # ---- operations -----------------------------------------------------------------------------

    def _start(self, phase):
        with self.lock:
            if not self.current or self.phase in BUSY:
                return None
            if self.rate_until is not None and self.rate_until > self.now():
                self.message = "Too many attempts. Wait before checking again."
                return None
            self.rate_until = None
            self.op = secrets.token_hex(8)
            self.phase, self.message = phase, None
            self.proof = secrets.token_hex(8)
            return self.op

    def _mine(self, op):
        return op is not None and self.op == op and self.current

    def _run(self, op, work, uncertain=False):
        try:
            work()
        except Exception as error:  # noqa: BLE001 - every failure is classified below
            with self.lock:
                if self._mine(op):
                    self._failed(error, uncertain)
        finally:
            with self.lock:
                if self._mine(op):
                    self.op = None
                    if self.phase in BUSY:
                        self.phase = UNCONFIRMED if uncertain else IDLE

    def resolve(self, text):
        code = parse_input(text)
        if code is None:
            with self.lock:
                if self.phase not in BUSY:
                    self.message = "Enter the eight-character code shown on the machine."
            return False
        op = self._start(RESOLVING)
        if op is None:
            return False
        with self.lock:
            self.code, self.preview, self.challenge = code, None, None

        def work():
            loaded = self.api.resolve(code)
            with self.lock:
                if self._mine(op) and self.code == code:
                    self._apply(loaded)
        self._run(op, work)
        return True

    def check_status(self):
        with self.lock:
            code = self.code
        return self.resolve(code.display) if code else False

    def _apply(self, loaded):
        now = self.now()
        if loaded["state"] == "approved":
            phase = APPROVED
        elif loaded["state"] == "denied":
            phase = DENIED
        elif loaded["expires_at"] <= now:
            phase = EXPIRED
        else:
            phase = REVIEW
        message = None
        if phase == EXPIRED:
            message = "This request expired. Start again on the machine."
        elif phase == REVIEW and not loaded["can_approve"]:
            message = REASONS.get(loaded["reason"], "Your account cannot decide this request.")
        elif phase == REVIEW and loaded["capabilities"] != ["mailbox"]:
            message = "These permissions are not supported by this app. Ask your administrator."
        self.preview, self.phase, self.challenge, self.message = loaded, phase, None, message
        self.proof = secrets.token_hex(8)

    def approve(self, proof):
        self.refresh_expiry()
        with self.lock:
            if not self._can_approve(proof):
                return False
            preview, code = dict(self.preview), self.code
        op = self._start(APPROVING)
        if op is None:
            return False

        def work():
            self.api.approve(preview, code)
            with self.lock:
                if self._mine(op):
                    self.phase, self.challenge = APPROVED, None
                    self.message = "You can close this; the machine is signing in."
        self._run(op, work, uncertain=True)
        return True

    def deny(self, proof):
        self.refresh_expiry()
        with self.lock:
            if not self._can_deny(proof):
                return False
            preview, code = dict(self.preview), self.code
        op = self._start(DENYING)
        if op is None:
            return False

        def work():
            self.api.deny(preview, code)
            # The acknowledgment alone does not prove a concurrent approval did not win.
            loaded = self.api.resolve(code)
            with self.lock:
                if not self._mine(op):
                    return
                if loaded["state"] in ("approved", "denied"):
                    self._apply(loaded)
                else:
                    self.phase = UNCONFIRMED
                    self.message = "The denial was sent, but the final status is not confirmed. Check it again."
        self._run(op, work, uncertain=True)
        return True

    def verify(self, proof, sign_in):
        """Run the strong sign-in `sign_in(requirement)` (blocking; adopts fresh tokens for the same
        person). Afterwards the request is only read again; it never approves."""
        with self.lock:
            if (not self.current or self.phase != REVIEW or proof != self.proof or self.challenge is None
                    or not self.strong_available):
                return False
            requirement, code = self.challenge, self.code
        op = self._start(VERIFYING)
        if op is None:
            return False
        try:
            sign_in(requirement)
        except Exception:  # noqa: BLE001
            with self.lock:
                if self._mine(op):
                    self.op = None
                    self.phase = REVIEW
                    self.proof = secrets.token_hex(8)
                    self.message = "Sign-in was not verified. Try again with the same account."
            return False
        with self.lock:
            if not self._mine(op):
                return False
            self.op = None
            self.phase = IDLE
        return self.resolve(code.display)  # a read, followed by another explicit decision

    def _failed(self, error, uncertain):
        api = error if isinstance(error, APIError) else None
        status, code = (api.status, api.code) if api else (None, None)
        self.proof = secrets.token_hex(8)
        if code == "step_up_required" or (status == 401 and code != "invalid_grant" and api and api.headers.get("www-authenticate")):
            requirement = StrongSignInRequirement.parse(api.body, api.headers.get("www-authenticate"))
            if requirement is None:
                self.phase, self.challenge = UNAVAILABLE, None
                self.message = "The server could not provide a supported verification method."
            elif not self.strong_available:
                self.phase, self.challenge = REVIEW, None
                self.message = ("Approving machines needs a strong sign-in, which the Linux app can’t do yet. "
                                "Approve it in your browser instead.")
            else:
                self.phase, self.challenge = REVIEW, requirement
                self.message = "Verify your sign-in, then review and decide this request again."
            return
        if status == 429:
            try:
                wait = int(api.headers.get("retry-after", 30))
            except (TypeError, ValueError):
                wait = 30
            self.rate_until = self.now() + max(1, min(3600, wait))
        if code == "pairing_expired":
            phase = EXPIRED
        elif code in ("forbidden", "human_required", "delegation_required", "feature_disabled", "step_up_unavailable",
                      "pairing_not_found", "pairing_decided", "org_inactive"):
            phase = UNAVAILABLE
        else:
            phase = UNCONFIRMED if uncertain else IDLE
        if status == 429:
            message = "Too many attempts. Wait before checking again."
        elif code == "pairing_expired":
            message = "This request expired. Start again on the machine."
        elif code == "pairing_not_found":
            message = "No request found for this code. Check it on the machine."
        elif code == "step_up_unavailable":
            message = "Strong sign-in isn’t set up for your organization, so machines can’t be approved."
        elif code == "pairing_decided":
            message = "This request was already decided. Check its status."
        elif phase == UNAVAILABLE:
            message = "Your account cannot decide this request, or the feature is unavailable."
        elif uncertain:
            message = "The decision is not confirmed. Check the request’s status before trying again."
        else:
            message = "The request could not be loaded. Try again."
        self.phase, self.challenge, self.message = phase, None, message
        if status in (403, 404) or code == "pairing_decided":
            self.preview = None if status in (403, 404) else self.preview

