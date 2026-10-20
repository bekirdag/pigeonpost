"""Pure macOS-compatible mailbox grouping and display helpers."""

import json
import re
from pathlib import PurePosixPath


def valid_peer(value, wildcard=False):
    if not 2 <= len(value) <= 512 or not value.startswith("/") or not value.isascii():
        return False
    parts = value[1:].split("/")
    return all((wildcard and i == len(parts) - 1 and len(parts) > 1) if part == "*" else
               bool(part) and part not in (".", "..") and ("*" not in part or "@" in part)
               and all(c.isalnum() or c in "!$&'*+-=^_`{|}~.@" for c in part)
               for i, part in enumerate(parts))


def conversation_address_input(value):
    value = value.strip()
    return value if value.startswith("/") else "/" + value


# Short segments that read as acronyms. The same list as ACRONYMS in site-inbox/names.js.
NAME_ACRONYMS = ("hr", "it", "qa", "ui", "ux", "pr", "ai", "ceo", "cto", "cfo", "ops", "dev")


def display_name(value):
    """A handle as a person's name: /bekir/main and /bekir are "Bekir", /wodo/hr is "Wodo HR",
    /acme/john-doe is "Acme John Doe". Key addresses, address-style handles and GitHub logins keep
    their old display; anything that is not a handle is returned unchanged. A contact alias wins."""
    legacy = value[:-5] if value.startswith("/") and not value.startswith("/k/") and value.endswith("/main") and value.count("/") > 1 else value
    parts = [p for p in value.split("/") if p]
    if not value.startswith("/") or value.startswith("/k/") or not parts or "@" in value or parts[0] == "github":
        return legacy
    if len(parts) > 1 and parts[-1] == "main":
        parts.pop()
    words = [w for part in parts for w in re.split(r"[-_]+", part) if w]
    name = " ".join(w.upper() if w.lower() in NAME_ACRONYMS else w[:1].upper() + w[1:].lower() for w in words)
    return name or value


def initials(name):
    """Two letters from a shown name: first and last words ("Wodo HR" is "WH"), or a one-word name's first two."""
    base = next((p for p in reversed(name.split("/")) if p), "") if "/" in name else name
    words = [w for w in ("".join(c for c in w if c.isalnum()) for w in re.split(r"[\s\-_]+", base)) if w]
    if not words:
        return "··"
    return (words[0][:1] + words[-1][:1] if len(words) > 1 else words[0][:2]).upper()


def peer_of(message):
    return next((message[k] for k in ("peer_handle", "peer", "sender_handle", "from") if message.get(k)), "unknown")


def normalize(peer, messages):
    for message in messages:
        if peer in (message.get("peer"), message.get("from")):
            return message.get("peer_handle") or message.get("sender_handle") or peer
    return peer


def timestamp(message):
    keys = ("sent_at", "received_at") if message.get("direction") == "out" else ("received_at", "sent_at")
    return next((message[k] for k in keys if isinstance(message.get(k), (int, float))), 0)


def contact_for(peer, contacts):
    exact = next((c for c in contacts if c.get("peer") == peer), None)
    if exact:
        return exact
    namespace = peer.strip("/").split("/")[0]
    return next((c for c in contacts if c.get("peer") == f"/{namespace}/*"), {})


def conversations(messages, contacts=()):
    result, seen = {}, set()
    for message in sorted(messages, key=timestamp):
        mid = message.get("message_id")
        if not mid or mid in seen:
            continue
        seen.add(mid)
        result.setdefault(normalize(peer_of(message), messages), []).append(message)
    for contact in contacts:
        if contact.get("peer") and not contact["peer"].endswith("/*"):
            result.setdefault(normalize(contact["peer"], messages), [])
    return dict(sorted(result.items(), key=lambda item: (-timestamp(item[1][-1]) if item[1] else 0, item[0])))


def unread_count(messages):
    return len({m.get("message_id") for m in messages
                if m.get("message_id") and m.get("direction") != "out" and not m.get("read")})


def subjects(peer, messages, threads):
    rows = {t["thread_id"]: dict(t) for t in threads if normalize(t.get("peer", ""), messages) == peer}
    for message in conversations(messages).get(peer, []):
        key = message.get("thread_id") or ""
        rows.setdefault(key, {"thread_id": key, "title": "General" if not key else "Untitled", "is_default": not key})
    return list(rows.values())


def target_thread(selected, rows):
    if selected is not None:
        return selected or None
    default = next((row for row in rows if row.get("is_default")), None)
    if default:
        return default.get("thread_id") or None
    return rows[0].get("thread_id") if len(rows) == 1 else None


def message_text(message):
    body = message.get("body", "")
    try:
        envelope = json.loads(body)
        if isinstance(envelope, dict) and isinstance(envelope.get("args"), dict) and envelope.get("verb"):
            text = envelope["args"].get("task") or envelope["args"].get("question")
            if isinstance(text, str):
                return text
    except (ValueError, TypeError):
        pass
    return body


def safe_filename(name):
    name = PurePosixPath(str(name).replace("\\", "/")).name
    name = "".join(c for c in name if c.isprintable() and c not in "/\\")[:180]
    return name if name not in ("", ".", "..") else "attachment"


def size_text(value):
    for unit in ("B", "KiB", "MiB", "GiB"):
        if value < 1024 or unit == "GiB":
            return f"{value:.1f} {unit}" if unit != "B" else f"{int(value)} B"
        value /= 1024
