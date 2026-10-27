"""Protected direct-message delivery with explicit recovery (docs/message-actions.md §1–2).

Every send or edit becomes an immutable request with its own `msg_ref`, saved (sealed, per account
and mailbox: protected.py) before its first POST. Then:

* confirmed (201/200 with the same msg_ref): the saved request is removed;
* refused (a definite 4xx): it is removed and the draft stays for the person to fix;
* uncertain (no answer, 5xx, 408): it stays as an *unconfirmed delivery*. Nothing replays it
  automatically, not even after a restart. The person reviews it and chooses Retry, which posts the
  exact saved bytes (the server returns the original ids for a repeated msg_ref, so a retry never
  sends twice), or Discard, which only forgets the local copy and cannot undo a delivery.

One unconfirmed request per recipient and subject: another send there waits until it is resolved.
Recipients are frozen before the first POST: with `recipient_lookup` the typed handle is resolved
once to its `/k/…` inbox (checked against this mailbox) and retries go to that inbox, never to
whoever holds the handle later. Everything is bound to the login (binding.Lease) that began it;
after a sign-out or another account's sign-in it stops with `Retired`.
"""

import hashlib
import json
import time

from .actions import RECIPIENT_LOOKUP, build_send, canonical_mailbox, parse_wire, valid_ref, wire
from .api import APIError
from .binding import Retired

MAX_PENDING = 64
MAX_WIRE = 2 * 1024 * 1024
MAX_DRAFTS = 200
MAX_DRAFT = 64 * 1024


class DeliveryConflict(Exception):
    def __init__(self):
        super().__init__("Review the unconfirmed message to this conversation before sending another.")


class DeliveryLimit(Exception):
    def __init__(self):
        super().__init__("Review unconfirmed messages in this inbox before sending more.")


def slot_for(recipient, thread):
    return hashlib.sha256(((recipient or "") + "\0" + (thread or "")).encode()).hexdigest()


def uncertain(error):
    """True when the request may have been processed: no answer, a server error or a timeout."""
    if isinstance(error, Retired):
        return True
    if isinstance(error, APIError):
        return error.status == 0 or error.status >= 500 or error.status == 408
    return True


class Attempt:
    """One saved request (read-only view of a protected entry)."""

    def __init__(self, entry):
        self.entry = entry
        self.wire = entry["wire"].encode()
        self.request = parse_wire(self.wire)
        if hashlib.sha256(self.wire).hexdigest() != entry.get("wire_sha"):
            raise ValueError("saved request")
        self.msg_ref = self.request["msg_ref"]
        self.slot = entry["slot"]
        self.review = entry.get("review") if isinstance(entry.get("review"), dict) else {}
        self.created_at = entry.get("created_at") or 0
        self.state = entry.get("state") or "prepared"

    @property
    def body(self):
        return self.request["body"]

    @property
    def peer(self):
        return self.review.get("peer") or self.request["to"]

    @property
    def thread(self):
        return self.review.get("thread")

    @property
    def is_edit(self):
        return "edit_of" in self.request

    def __repr__(self):
        return "Attempt(redacted)"


class Coordinator:
    """Prepare, send, retry and discard protected deliveries for one login (`lease`)."""

    def __init__(self, store, api, lease, mailbox, features=frozenset(), now=time.time):
        self.store, self.api, self.lease, self.mailbox = store, api, lease, mailbox
        self.features, self.now = frozenset(features), now

    # ---- records --------------------------------------------------------------------------

    def _entries(self, record):
        entries = record.setdefault("deliveries", {})
        if not isinstance(entries, dict) or len(entries) > MAX_PENDING:
            raise DeliveryLimit()
        return entries

    def pending(self):
        """Every unconfirmed delivery of this mailbox, oldest first. Unreadable entries raise."""
        record = self.store.read(self.lease, self.mailbox)
        attempts = []
        for slot, entry in self._entries(record).items():
            attempt = Attempt(entry)
            if attempt.slot != slot or attempt.request["from"] != self.mailbox:
                raise ValueError("saved request")
            attempts.append(attempt)
        self.lease.require()
        return sorted(attempts, key=lambda a: (a.created_at, a.msg_ref))

    def check_free(self, peer, thread):
        """Before uploading attachments: refuse early when this conversation is waiting."""
        record = self.store.read(self.lease, self.mailbox)
        if any((e.get("review") or {}).get("peer") == peer and (e.get("review") or {}).get("thread") == (thread or None)
               for e in self._entries(record).values()):
            raise DeliveryConflict()

    # ---- the flow -------------------------------------------------------------------------

    def prepare(self, to, body, *, peer=None, thread=None, attachments=None, reply_to=None, quote=None, edit_of=None,
                subject_label=None):
        """Freeze the recipient, build the exact request and save it before anything is posted."""
        self.lease.require()
        peer = peer or to
        self.check_free(peer, thread)
        recipient = to
        if RECIPIENT_LOOKUP in self.features and not canonical_mailbox(to):
            route = self.api.call("POST", "/v1/recipients/resolve", data={"identity": self.mailbox, "to": to})
            self.lease.require()
            if route.get("from") != self.mailbox or not canonical_mailbox(route.get("address")):
                raise APIError(0, "invalid_response")
            recipient = route["address"]
        payload = build_send(self.mailbox, recipient, body, thread, attachments, None, reply_to, quote, edit_of)
        data = wire(payload)
        if len(data) > MAX_WIRE:
            raise ValueError("This message is too long.")
        entry = {"slot": slot_for(recipient, thread), "wire": data.decode(), "wire_sha": hashlib.sha256(data).hexdigest(),
                 "created_at": int(self.now()), "state": "prepared",
                 "review": {"peer": peer, "typed": to, "thread": thread or None, "subject": (subject_label or "")[:200]}}

        def save(record):
            entries = self._entries(record)
            if entry["slot"] in entries or any((e.get("review") or {}).get("peer") == peer and
                                               (e.get("review") or {}).get("thread") == (thread or None) for e in entries.values()):
                raise DeliveryConflict()
            if len(entries) >= MAX_PENDING:
                raise DeliveryLimit()
            entries[entry["slot"]] = entry
        self.store.update(self.lease, self.mailbox, save)
        return Attempt(entry)

    def send(self, attempt):
        """Post the saved bytes once. Returns (result, cleanup_failed). Raises on failure; an
        uncertain failure leaves the request saved (marked unconfirmed) for explicit review."""
        saved = self._load(attempt)
        try:
            result = self._post(saved.wire)
        except Exception as error:
            if uncertain(error) and not isinstance(error, Retired):
                self._mark(saved, "uncertain")
            elif not uncertain(error):
                self._remove(saved)
            raise
        self.lease.require()
        if result.get("msg_ref") not in (None, saved.msg_ref) or (result.get("msg_ref") is None and not result.get("message_id") and not result.get("sent_copy_id")):
            self._mark(saved, "uncertain")
            raise APIError(0, "invalid_response")
        try:
            self._remove(saved)
            return result, False
        except Exception:
            # Confirmed stays confirmed even when the local copy could not be removed.
            self.lease.require()
            return result, True

    retry = send

    def _post(self, data):
        exact = getattr(self.api, "post_exact", None)
        if callable(exact):
            return exact("/v1/send", data)
        return self.api.call("POST", "/v1/send", data=json.loads(data))  # injected test APIs

    def discard(self, attempt):
        """Forget the local copy only. It cannot undo a delivery that already happened."""
        return self._remove(attempt)

    # ---- helpers --------------------------------------------------------------------------

    def _load(self, attempt):
        record = self.store.read(self.lease, self.mailbox)
        entry = self._entries(record).get(attempt.slot)
        if not entry:
            raise APIError(0, "not_pending", "This message is no longer waiting to be sent.")
        saved = Attempt(entry)
        if saved.wire != attempt.wire or saved.request["from"] != self.mailbox:
            raise DeliveryConflict()
        return saved

    def _mark(self, attempt, state):
        def change(record):
            entry = self._entries(record).get(attempt.slot)
            if entry and entry.get("wire") == attempt.wire.decode():
                entry["state"] = state
        try:
            self.store.update(self.lease, self.mailbox, change)
        except Exception:
            pass  # still saved as prepared; it is reviewed the same way

    def _remove(self, attempt):
        if not valid_ref(attempt.msg_ref):
            raise ValueError("saved request")

        def change(record):
            entries = self._entries(record)
            entry = entries.get(attempt.slot)
            if not entry:
                return False
            if Attempt(entry).msg_ref != attempt.msg_ref:
                raise DeliveryConflict()  # never remove a newer request in the same slot
            entries.pop(attempt.slot)
            return True
        return self.store.update(self.lease, self.mailbox, change)


# ---- drafts -------------------------------------------------------------------------------------

def draft_slot(peer, subject):
    return hashlib.sha256(((peer or "") + "\0" + ("*" if subject is None else subject)).encode()).hexdigest()[:40]


def load_drafts(store, lease, mailbox):
    """{(peer, subject): text} saved for this mailbox (subject None = All messages)."""
    record = store.read(lease, mailbox)
    drafts = {}
    for entry in (record.get("drafts") or {}).values():
        if isinstance(entry, dict) and isinstance(entry.get("peer"), str) and isinstance(entry.get("text"), str):
            subject = entry.get("subject")
            if subject is None or isinstance(subject, str):
                drafts[(entry["peer"], subject)] = entry["text"][:MAX_DRAFT]
    return drafts


def save_draft(store, lease, mailbox, peer, subject, text, now=time.time):
    def change(record):
        drafts = record.setdefault("drafts", {})
        key = draft_slot(peer, subject)
        if text.strip():
            drafts[key] = {"peer": peer, "subject": subject, "text": text[:MAX_DRAFT], "at": int(now())}
            if len(drafts) > MAX_DRAFTS:
                for old in sorted(drafts, key=lambda k: drafts[k].get("at") or 0)[:len(drafts) - MAX_DRAFTS]:
                    drafts.pop(old)
        else:
            drafts.pop(key, None)
    store.update(lease, mailbox, change)
