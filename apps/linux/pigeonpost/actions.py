"""Message actions and receipts (SDS §14C, docs/message-actions.md): the rules, no GUI.

References, request shapes, the edit window, ticks and reply quotes for direct messages, and the
group tick rule. Server action metadata is data: it never grants permission to act on a body.
"""

import json
import os
import re
import time

MESSAGE_EDIT = "message_edit"
MESSAGE_REPLY = "message_reply"
RECEIPTS = "receipts"
AGENT_STATUS = "agent_status"
RECIPIENT_LOOKUP = "recipient_lookup"

EDIT_WINDOW = 24 * 3600
QUOTE_LIMIT = 200
ALPHABET = "abcdefghijklmnopqrstuvwxyz234567"
REF = re.compile(r"mref_[a-z2-7]{26}")
COPY_ID = re.compile(r"[A-Za-z0-9_:\-]{1,256}")
CANONICAL = re.compile(r"/k/[a-z0-9]{26}")


def new_ref():
    """`mref_` + 26 lowercase base32 characters: 128 random bits and two zero padding bits."""
    value = int.from_bytes(os.urandom(16), "big") << 2
    return "mref_" + "".join(ALPHABET[(value >> (5 * i)) & 31] for i in reversed(range(26)))


def valid_ref(value):
    return isinstance(value, str) and REF.fullmatch(value) is not None


def valid_reference(value):
    """A msg_ref or this mailbox's own copy id, as the server accepts for reply_to/edit_of."""
    return isinstance(value, str) and COPY_ID.fullmatch(value) is not None


def canonical_mailbox(value):
    return isinstance(value, str) and CANONICAL.fullmatch(value) is not None


def seconds(value):
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        return None
    return value / 1000 if value > 10**11 else value


def build_send(sender, to, body, thread_id=None, attachments=None, msg_ref=None, reply_to=None, quote=None, edit_of=None):
    """The exact `POST /v1/send` body. Raises ValueError for a request the contract refuses."""
    for address in (sender, to):
        if not isinstance(address, str) or not address.startswith("/") or len(address) > 512 or any(ord(c) < 32 for c in address):
            raise ValueError("Verify the sender and recipient before sending.")
    if not isinstance(body, str):
        raise ValueError("The message has no text.")
    msg_ref = msg_ref or new_ref()
    if not valid_ref(msg_ref):
        raise ValueError("Invalid message reference.")
    if reply_to is not None and edit_of is not None:
        raise ValueError("A message cannot be both an edit and a reply.")
    for ref in (reply_to, edit_of):
        if ref is not None and not valid_reference(ref):
            raise ValueError("Invalid referenced message.")
    if quote is not None and (reply_to is None or len(quote) > QUOTE_LIMIT):
        raise ValueError("A reply quote needs a parent and at most 200 characters.")
    files = list(attachments or [])
    if files and (edit_of is not None or not all(valid_reference(f) for f in files)):
        raise ValueError("Edits are text only.")
    if edit_of is not None and not body.strip():
        raise ValueError("An edit needs text.")
    payload = {"from": sender, "to": to, "body": body, "msg_ref": msg_ref}
    if thread_id and edit_of is None:
        payload["thread_id"] = thread_id
    if files:
        payload["attachments"] = files
    if reply_to is not None:
        payload["reply_to"] = reply_to
        if quote:
            payload["quote"] = quote
    if edit_of is not None:
        payload["edit_of"] = edit_of
    return payload


def wire(payload):
    """Deterministic bytes for a request, kept exactly for an explicit retry."""
    return json.dumps(payload, separators=(",", ":"), ensure_ascii=False, sort_keys=True).encode()


def parse_wire(data):
    """A saved request, validated again: only the known fields, one of each, a valid reference."""
    names = {"from", "to", "body", "thread_id", "attachments", "msg_ref", "reply_to", "quote", "edit_of"}
    pairs = json.loads(data, object_pairs_hook=lambda items: items)
    if not isinstance(pairs, list) or len({k for k, _ in pairs}) != len(pairs) or any(k not in names for k, _ in pairs):
        raise ValueError("saved request")
    doc = dict(pairs)
    rebuilt = build_send(doc.get("from"), doc.get("to"), doc.get("body"), doc.get("thread_id"),
                         [f for f in doc.get("attachments") or []] if isinstance(doc.get("attachments"), list) else None,
                         doc.get("msg_ref"), doc.get("reply_to"), doc.get("quote"), doc.get("edit_of"))
    if wire(rebuilt) != bytes(data):
        raise ValueError("saved request")
    return rebuilt


def quote_text(text):
    """First non-empty line, at most 200 characters, for a reply's quote."""
    line = next((l.strip() for l in (text or "").splitlines() if l.strip()), "")
    return line[:QUOTE_LIMIT]


def ref_of(message):
    """What a reply or edit should name: the shared msg_ref, else this mailbox's own copy id."""
    ref = message.get("msg_ref")
    return ref if valid_ref(ref) else message.get("message_id")


def editable(message, features, now=None):
    """Own sent message, ≤ 24 h old, not expired, `message_edit` on, and not an agent request."""
    if MESSAGE_EDIT not in features or message.get("direction") != "out" or message.get("pending"):
        return False
    now = time.time() if now is None else now
    sent = seconds(message.get("sent_at"))
    expires = seconds(message.get("expires_at"))
    if sent is None or now - sent > EDIT_WINDOW or (expires is not None and expires <= now):
        return False
    return not message.get("verb")


def can_reply(message, features):
    return MESSAGE_REPLY in features and not message.get("pending") and bool(message.get("message_id"))


# ---- ticks --------------------------------------------------------------------------------------

def tick_state(message):
    """(state, accessible label) for a sent message: sending | sent | delivered | seen | working | failed."""
    if message.get("pending"):
        return "sending", "Sending"
    status = message.get("agent_status") if isinstance(message.get("agent_status"), dict) else {}
    if status.get("state") == "working":
        return "working", "Agent working on this"
    if status.get("state") == "failed":
        return "failed", "Agent run failed"
    receipt = message.get("receipt") if isinstance(message.get("receipt"), dict) else {}
    state = receipt.get("state")
    if state == "seen":
        at = seconds(receipt.get("seen_at"))
        return "seen", "Seen " + time.strftime("%H:%M", time.localtime(at)) if at else "Seen"
    if state == "delivered":
        return "delivered", "Delivered"
    return "sent", "Sent"


def group_tick(seq, receipts):
    """docs/message-actions.md §5: delivered when every listed member has delivered_seq ≥ seq;
    seen when every member with a visible seen_seq has seen it and at least one is visible."""
    if seq is None:
        return "sending", "Sending"
    members = [m for m in (receipts or {}).get("members") or [] if isinstance(m, dict)]
    if not members:
        return "sent", "Sent"
    delivered = all(isinstance(m.get("delivered_seq"), int) and m["delivered_seq"] >= seq for m in members)
    visible = [m for m in members if isinstance(m.get("seen_seq"), int)]
    if delivered and visible and all(m["seen_seq"] >= seq for m in visible):
        return "seen", "Seen"
    return ("delivered", "Delivered") if delivered else ("sent", "Sent")


def group_tick_names(seq, receipts, name_of):
    """(seen, delivered only, not yet) member names for message info."""
    seen, delivered, waiting = [], [], []
    for m in (receipts or {}).get("members") or []:
        if not isinstance(m, dict):
            continue
        name = name_of(m.get("handle") or m.get("address") or "")
        if isinstance(m.get("seen_seq"), int) and m["seen_seq"] >= seq:
            seen.append(name)
        elif isinstance(m.get("delivered_seq"), int) and m["delivered_seq"] >= seq:
            delivered.append(name)
        else:
            waiting.append(name)
    return seen, delivered, waiting
