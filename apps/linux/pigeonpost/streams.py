"""Phase 6 live streams in groups, as the Linux app shows them (docs/streaming.md §6). No GUI.

WebKitGTK has no WebRTC (no RTCPeerConnection in any distribution or Flathub runtime), so this
app never plays or sends media: it shows that a group is live, who may go live, and a stage
invitation, and opens the web inbox at that group for watching, going live or joining the stage.
Who may do any of it is decided by the postbox and the calls service, never here; these rules only
choose which buttons to offer, mirroring `site-inbox/groups.js`.
"""

import re

STREAMING = "streaming"
INBOX = "https://inbox.pigeonpost.dev/"
POLL_SECONDS = 10

GROUP_ID = re.compile(r"^grp_[a-z2-7]{26}$")
STREAM_ID = re.compile(r"^str_[a-z2-7]{26}$")


def _text(value, limit):
    if not isinstance(value, str):
        return ""
    return "".join(c for c in value if c >= " " and c != "\x7f")[:limit].strip()


def live_stream(body, gid):
    """The group's live stream from `GET /v1/groups/{id}/streams`, checked, or None.

    Anything that does not belong to this group, is not live or is malformed is ignored: an old
    answer for another group can never put a banner on this one."""
    rows = body.get("streams") if isinstance(body, dict) else None
    if not isinstance(rows, list):
        return None
    for row in rows:
        if not isinstance(row, dict) or row.get("state") != "live":
            continue
        if row.get("group_id") != gid or not isinstance(row.get("id"), str) or not STREAM_ID.match(row["id"]):
            continue
        if not isinstance(row.get("by"), str) or not row["by"]:
            continue
        stage = []
        for entry in row.get("stage") if isinstance(row.get("stage"), list) else []:
            if isinstance(entry, dict) and isinstance(entry.get("address"), str) and entry.get("state") in ("invited", "speaker"):
                stage.append({"address": entry["address"], "state": entry["state"]})
        return {"id": row["id"], "group_id": gid, "by": row["by"], "title": _text(row.get("title"), 100) or "Live stream",
                "sources": [s for s in row.get("sources") or [] if s in ("camera", "screen")] if isinstance(row.get("sources"), list) else [],
                "stage": stage[:7]}
    return None


def is_admin(group):
    return ((group or {}).get("my") or {}).get("role") in ("owner", "admin")


def can_go_live(group):
    """The same rule as the web inbox: an active, non-restricted member of an activated group, and
    an admin unless the group lets everyone go live. The postbox decides for real."""
    if not isinstance(group, dict):
        return False
    my = group.get("my") or {}
    if my.get("state") != "active" or my.get("role") == "restricted" or group.get("activation_state") != "active":
        return False
    if group.get("deleted_at"):
        return False
    return group.get("stream_policy") == "everyone" or is_admin(group)


def stage_role(live, me):
    """`invited`, `speaker` or None for this mailbox on the stream's stage."""
    for entry in (live or {}).get("stage") or ():
        if entry.get("address") == me:
            return entry.get("state")
    return None


def banner(live, me, name_of):
    """What the LIVE banner says: {text, watch, stage}. `name_of` gives a display name."""
    if not live:
        return None
    mine = live["by"] == me
    who = "You are" if mine else f"{name_of(live['by'])} is"
    role = stage_role(live, me)
    stage = None
    if role == "invited":
        stage = "You’re invited on stage — open the stream in your browser to accept."
    elif role == "speaker":
        stage = "You’re on stage in this stream."
    return {"text": f"{who} live: {live['title']}", "watch": "Open in browser" if mine else "Watch in browser", "stage": stage}


def group_link(gid):
    """The web inbox, opened at this group (site-inbox/groups.js reads `#group=`)."""
    if not isinstance(gid, str) or not GROUP_ID.match(gid):
        return INBOX
    return INBOX + "#group=" + gid


def stream_notice(meta, actor):
    """The `stream` notice, as the web inbox words it: "Jane streamed for 34 min · 18 viewers"."""
    meta = meta if isinstance(meta, dict) else {}
    try:
        minutes = max(1, round(float(meta.get("duration") or 0) / 60))
    except (TypeError, ValueError):
        minutes = 1
    try:
        viewers = max(0, int(meta.get("viewers") or 0))
    except (TypeError, ValueError):
        viewers = 0
    return f"{actor} streamed for {minutes} min · {viewers} {'viewer' if viewers == 1 else 'viewers'}"


POLICY_CHOICES = [("admins", "Only admins"), ("everyone", "Everyone")]
