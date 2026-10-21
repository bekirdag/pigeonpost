"""Phase 1b and 3 rules shared by the window: no GUI dependency, so they are unit tested.

Calls history, filtering, blocking, profile pictures, disappearing messages and the group texts
follow the web inbox (`site-inbox/app.js`, `site-inbox/groups.js`), which is the reference.
"""

import datetime
import time

from .groups_core import TIMERS, timer_label  # noqa: F401  (re-exported for the window)

# Feature flags from GET /v1/capabilities.
MESSAGE_FILTERING = "message_filtering"
CALL_ADMISSION = "call_admission"
CALL_HISTORY = "call_history"
AVATARS = "avatars"
GROUPS = "groups"
DISAPPEARING = "disappearing"

POLICY_ROWS = [
    {"field": "unknown_calls", "flag": CALL_ADMISSION, "title": "Calls from people you don’t know",
     "note": "Silence keeps the call out of your way and lists it under Filtered calls.",
     "options": [("ring", "Ring"), ("silence", "Silence"), ("block", "Block")]},
    {"field": "unnamed_messages", "flag": MESSAGE_FILTERING, "title": "Messages from addresses without a name",
     "note": "Addresses like /k/… that have no handle.",
     "options": [("show", "Show"), ("filter", "Move to Filtered"), ("block", "Block")]},
    {"field": "unknown_messages", "flag": MESSAGE_FILTERING, "title": "Messages from people you don’t know",
     "note": "People you know are your contacts and anyone you have written to first.",
     "options": [("show", "Show"), ("filter", "Move to Filtered"), ("block", "Block")]},
    {"field": "unnamed_calls", "flag": CALL_ADMISSION, "title": "Calls from addresses without a name",
     "note": None, "options": [("ring", "Ring"), ("silence", "Silence"), ("block", "Block")]},
    {"field": "low_reputation", "flag": MESSAGE_FILTERING, "title": "Low-reputation senders",
     "note": "Senders others have reported as spam.", "options": [("filter", "Filter"), ("allow", "Allow")]},
    {"field": "call_history_days", "flag": CALL_HISTORY, "title": "Keep call history",
     "note": "Off stops keeping new calls. Entries older than this are deleted.",
     "options": [("0", "Off"), ("30", "30 days"), ("180", "180 days"), ("365", "1 year")]},
]

BLOCK_BODY = "They won’t be able to message or call you, and won’t be told."
LIMITS_NOTE = ("Recipients can still take screenshots or copy text, notifications already shown may stay on screen, "
               "and copies made outside Pigeonpost are not affected.")
AVATAR_VISIBILITY = [("everyone", "Everyone"), ("known", "People I know"), ("nobody", "Nobody")]


def policy_rows(features):
    return [row for row in POLICY_ROWS if row["flag"] in features]


def policy_value(field, raw):
    return int(raw) if field == "call_history_days" else raw


def wildcard_for(peer):
    """The rule covering a whole namespace, or None where nobody owns the namespace (a provider or
    an open one) and a wildcard would block strangers who merely share it."""
    if not peer or peer.startswith("/k/") or peer.endswith("/*"):
        return None
    parts = [p for p in peer.split("/") if p]
    if not parts:
        return None
    parent = parts[0] if len(parts) == 1 else "/".join(parts[:-1])
    if parent in ("github", "pp", "k"):
        return None
    return "/" + parent + "/*"


def is_blocked(peer, contacts):
    """True when an exact or namespace-wide block covers `peer`."""
    if not peer:
        return False
    for contact in contacts or ():
        rule = contact.get("peer") or ""
        if contact.get("admission") != "block":
            continue
        if rule == peer:
            return True
        if rule.endswith("/*") and peer.startswith(rule[:-1]) and not peer.startswith("/k/"):
            return True
    return False


# ---- call history -------------------------------------------------------------------------------

def call_peer_key(entry):
    return entry.get("peer_handle") or entry.get("peer") or None


def kind_word(kind):
    return "video" if kind == "video" else "voice"


def call_outcome_label(entry):
    incoming = entry.get("direction") == "incoming"
    return {
        "answered": "Incoming" if incoming else "Outgoing",
        "missed": "Missed" if incoming else "No answer",
        "declined": "Declined",
        "cancelled": "Missed" if incoming else "Cancelled",
        "busy": "Busy",
        "filtered": "Filtered",
        "failed": "Failed",
    }.get(entry.get("outcome"), "In progress")


def is_missed(entry):
    return entry.get("direction") == "incoming" and entry.get("outcome") in ("missed", "cancelled")


def format_duration(seconds):
    total = max(0, round(float(seconds or 0)))
    h, m, s = total // 3600, (total % 3600) // 60, total % 60
    return f"{h}:{m:02d}:{s:02d}" if h else f"{m}:{s:02d}"


def call_detail(entry):
    """'↙ Missed · Video · 2 people' style second line."""
    bits = [call_outcome_label(entry), "Video" if entry.get("kind") == "video" else "Voice"]
    duration = entry.get("duration")
    if duration is None and entry.get("answered_at") and entry.get("ended_at"):
        duration = entry["ended_at"] - entry["answered_at"]
    if entry.get("outcome") == "answered" and duration is not None:
        bits.append(format_duration(duration))
    people = len(entry.get("participants") or [])
    if entry.get("group_id") or people > 2:
        bits.append(f"{people} people")
    arrow = {"incoming": "↙", "outgoing": "↗"}.get(entry.get("direction"), "")
    return (arrow + " " + " · ".join(bits)).strip()


def clock_time(seconds):
    try:
        return datetime.datetime.fromtimestamp(seconds).strftime("%H:%M")
    except (TypeError, ValueError, OSError, OverflowError):
        return ""


def list_time(seconds, now=None):
    """Relative and short: 14:32 today, Yesterday, Mon, 12 Oct."""
    try:
        when = datetime.datetime.fromtimestamp(seconds)
    except (TypeError, ValueError, OSError, OverflowError):
        return ""
    today = datetime.datetime.fromtimestamp(now if now is not None else time.time()).date()
    days = (today - when.date()).days
    if days <= 0:
        return when.strftime("%H:%M")
    if days == 1:
        return "Yesterday"
    if days < 7:
        return when.strftime("%a")
    return when.strftime("%d %b").lstrip("0")


def missed_call_lines(entries, keys):
    """Missed incoming one-to-one calls from `keys` (handle/address of the conversation) as system
    lines: [{at, text, id}] sorted by time."""
    lines = []
    for entry in entries or ():
        if entry.get("direction") != "incoming" or entry.get("outcome") != "missed" or entry.get("group_id"):
            continue
        if entry.get("peer_handle") in keys or entry.get("peer") in keys:
            lines.append({"id": "call:" + str(entry.get("id")), "at": entry.get("started_at") or 0,
                          "text": f"Missed {kind_word(entry.get('kind'))} call · {clock_time(entry.get('started_at'))}"})
    return sorted(lines, key=lambda line: line["at"])


# ---- disappearing messages ----------------------------------------------------------------------

def expired(message, now=None):
    at = message.get("expires_at")
    return bool(at) and at <= (now if now is not None else time.time())


def live_messages(messages, now=None):
    """Drop expired messages by this device's own clock, before rendering (SDS §17.2)."""
    t = now if now is not None else time.time()
    return [m for m in messages if not expired(m, t)]


def direct_thread(peer, selected, threads):
    """The thread a one-to-one timer applies to: the chosen subject, else the conversation's default
    thread, else its only one."""
    rows = [t for t in threads or () if t.get("peer") == peer and t.get("thread_id")]
    if selected:
        return next((t for t in rows if t["thread_id"] == selected), {"thread_id": selected, "peer": peer})
    default = next((t for t in rows if t.get("is_default")), None)
    if default:
        return default
    return rows[0] if len(rows) == 1 else None


# ---- profile pictures ---------------------------------------------------------------------------

def avatar_url(meta):
    """The JPEG variant path of an avatar object (GdkPixbuf always decodes JPEG; WebP needs an
    optional loader), or None. Only postbox-relative paths are accepted."""
    if not isinstance(meta, dict):
        return None
    urls = meta.get("urls") or {}
    url = urls.get("jpeg") or urls.get("256") or urls.get("128")
    if not isinstance(url, str) or not url.startswith("/v1/avatar-blobs/") or ".." in url:
        return None
    return url


def avatar_hash(meta):
    return str(meta.get("hash") or f"{meta.get('id')}:{meta.get('version')}")


def square_crop(width, height, zoom, offset_x, offset_y):
    """The source rectangle (x, y, side) a square view shows at `zoom` (≥ 1) when the image is
    scaled to cover the square and shifted by `offset_*` in [-1, 1] of the free movement."""
    side = min(width, height) / max(1.0, zoom)
    free_x, free_y = (width - side) / 2, (height - side) / 2
    x = free_x + max(-1.0, min(1.0, offset_x)) * free_x
    y = free_y + max(-1.0, min(1.0, offset_y)) * free_y
    return int(round(x)), int(round(y)), max(1, int(round(side)))


def avatar_error(status, code):
    if status == 429 or code == "avatar_rate_limited":
        return "You can change your picture five times a day. Try again tomorrow."
    if status == 413:
        return "That picture is too large. Choose one under 10 MB."
    if status in (415, 422):
        return "That picture could not be used. Try a JPEG or PNG."
    return "Could not save your picture. Please try again."


# ---- groups -------------------------------------------------------------------------------------

ROLE_LABEL = {"owner": "Owner", "admin": "Admin", "member": "Member", "restricted": "Restricted"}

GROUP_ERRORS = {
    "not_admin": "Only admins can do that.", "owner_only": "Only the owner can do that.",
    "human_required": "This needs you to be signed in yourself, not an agent key.",
    "posting_restricted": "Only admins can send messages here.", "banned": "That mailbox is banned from this group.",
    "group_full": "The group is full.", "invalid_link": "This invite link is no longer valid.",
    "invite_required": "This group is invite only.", "already_member": "Already a member.",
    "group_deleted": "This group was deleted.", "revision_conflict": "Someone else changed this at the same time. Try again.",
    "not_member": "You are not a member of this group.", "rate_limited": "Too many requests. Wait a moment and try again.",
    "quota_exceeded": "This group is out of storage.", "feature_disabled": "Groups are not enabled for this account.",
    "not_found": "Not found.", "key_activation_pending": "Waiting for a member’s device to finish a security update. Try again in a moment.",
    "network_error": "Could not reach Pigeonpost. Check your connection and try again.",
}

WARNINGS = {
    "membership_mismatch": "Security warning: a membership change did not match the postbox’s record. Compare safety numbers in group info.",
    "commit_rejected": "Security warning: this device refused an invalid group change.",
    "sender_mismatch": "Security warning: a message’s sender did not match its device.",
    "resynced": "This device lost the group’s keys and is being added again. Earlier messages stay unreadable here.",
}

SAFETY_NOTE = ("Compare this safety number with another member in person or on a call. If it matches, you both see "
               "the same devices in this group.")
HOSTED_NOTE = ("Hosted mailboxes’ keys are held by the postbox, so it could add a device to one. Every new device is shown "
               "here and in the conversation; check fingerprints with people you trust.")
E2EE_NOTE = "Group messages are end-to-end encrypted. People you add see messages from when they join, not before."


def explain(error):
    code = getattr(error, "code", None)
    body = getattr(error, "body", None) or {}
    return GROUP_ERRORS.get(code) or body.get("message") or str(error) or "Something went wrong."


def can_post(group):
    my = group.get("my") or {}
    if my.get("state") != "active" or my.get("role") == "restricted":
        return False
    if group.get("post_policy") == "admins":
        return my.get("role") in ("owner", "admin")
    return True


def is_admin(group):
    return (group.get("my") or {}).get("role") in ("owner", "admin")


def spaced(digits):
    digits = "".join(c for c in str(digits or "") if c.isdigit())
    return " ".join(digits[i:i + 5] for i in range(0, len(digits), 5))


def notice_text(item, name_of):
    meta = item.get("metadata") or {}
    actor = name_of(item["actor"]) if item.get("actor") else "Someone"
    who = name_of(meta["address"]) if meta.get("address") else "someone"
    kind = item.get("notice")
    if kind == "group_created":
        return f"{actor} created the group"
    if kind == "member_invited":
        return f"{actor} invited {who}"
    if kind == "member_left":
        return f"{who} left"
    if kind == "member_removed":
        return f"{actor} removed {who}"
    if kind == "member_banned":
        return f"{actor} removed and banned {who}"
    if kind == "role_changed":
        role = meta.get("role")
        return f"{actor} made {who} an admin" if role == "admin" else f"{actor} restricted {who}" if role == "restricted" else f"{who} is now a member"
    if kind == "settings_changed":
        changed = meta.get("changed") or []
        if "post_policy" in changed:
            return f"{actor} set posting to admins only" if meta.get("post_policy") == "admins" else f"{actor} let everyone post"
        if "name" in changed:
            return f"{actor} renamed the group to “{meta.get('name') or ''}”"
        if "join_policy" in changed:
            return f"{actor} made the group public" if meta.get("join_policy") == "public" else f"{actor} made the group invite only"
        return f"{actor} changed the group settings"
    if kind == "timer_changed":
        secs = meta.get("ephemeral_secs")
        return f"{actor} set messages to disappear after {timer_label(secs)}" if secs else f"{actor} turned off disappearing messages"
    if kind == "ownership_transferred":
        return f"{actor} made {who} the owner"
    if kind == "call_started":
        return f"{actor} started a {'video' if meta.get('kind') == 'video' else 'voice'} call"
    if kind == "call_ended":
        return f"Call ended · {round(meta['duration'] / 60)} min" if meta.get("duration") else "Call ended"
    if kind == "link_reset":
        return f"{actor} reset the invite link"
    if kind == "you_joined":
        return "You joined this group. Earlier messages are not available on this device."
    if kind == "device_removed":
        return f"{who}'s device was removed"
    return None  # member_joined and device_added are shown from the verified commit


def commit_lines(items, commit, me, my_device, name_of):
    """Lines derived from a verified commit: who really joined, and which existing members added a
    new device. The latter is the visible defence against a postbox minting devices (P-2 §9, §12).
    Returns [{text, fingerprint?, security?}]."""
    known = {me}
    for other in items:
        if other is commit:
            break
        if other.get("kind") == "commit":
            known.add(other["committer"]["mailbox"])
            known.update(d["mailbox"] for d in other.get("added") or [])
        if other.get("kind") == "message" and other.get("sender"):
            known.add(other["sender"])
    by_mailbox = {}
    for device in commit.get("added") or []:
        by_mailbox.setdefault(device["mailbox"], []).append(device)
    out = []
    for mailbox, devices in by_mailbox.items():
        if mailbox in known or mailbox == commit["committer"]["mailbox"]:
            for device in devices:
                if mailbox == me and device["device_id"] == my_device:
                    continue
                out.append({"text": f"{name_of(mailbox)} added a new device", "fingerprint": device["fingerprint"], "security": True})
        else:
            out.append({"text": f"{name_of(mailbox)} joined"})
    return out


def preview_of(items, me, name_of):
    for item in reversed(items):
        if item.get("kind") != "message" or item.get("replaced_by"):
            continue
        if item.get("deleted"):
            return "Message deleted"
        if item.get("undecryptable"):
            return "Encrypted message"
        who = "You" if item.get("sender") == me else (name_of(item.get("sender")).replace("/", " ").split() or ["Someone"])[0]
        files = item.get("files") or []
        what = item.get("text") or ("📎 " + files[0]["name"] if files else "")
        return f"{who}: {what}"
    return ""


def parse_invite_link(text):
    """(group_id, link_token|None) from `https://inbox.pigeonpost.dev/g/<id>#<token>`, or None."""
    import re
    match = re.fullmatch(r"\s*https://inbox\.pigeonpost\.dev/g/(grp_[A-Za-z0-9]{1,60})/?(?:#([A-Za-z0-9_-]{16,128}))?\s*", text or "")
    if not match:
        return None
    return match.group(1), match.group(2)
