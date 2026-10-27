"""Organization rooms (SDS §4, docs/orgs.md §2 and §5): the rules, no GUI.

The postbox is the only source of truth. What this module keeps (the catalog, the directory, a
room's preview) is a display hint: every join, acceptance and creation asks the server again, and a
403/404 from it clears what was cached. Owned member inboxes (an org's `my_mailbox`) are kept apart
from agents delegated to the person (`operated`): those are never treated as personal mailboxes.

Room creation keeps custody of its exact request. The body, with its `request_key`, is saved in the
protected store (protected.py, bound to the login and the org mailbox) *before* it is posted. When
the answer is lost, the saved bytes stay for an explicit retry (the server returns the original
room for the same key and body) or an explicit discard; nothing is ever re-created silently.
"""

import base64
import json
import re
import secrets
import time
import urllib.parse

from .actions import canonical_mailbox
from .api import APIError

ORGS = "orgs"
GROUPS = "groups"

IDENTIFIER = re.compile(r"[A-Za-z0-9_-]{1,128}")
BIDI = set(range(0x202A, 0x202F)) | set(range(0x2066, 0x206A)) | {0x200E, 0x200F, 0x061C}
CREATOR_ROLES = {"owner", "admin", "people_admin", "agent_admin", "billing_admin"}
MAX_PENDING_ROOMS = 8
ROOM_RECORD = "orgs"


class OrgError(Exception):
    """A response that does not match the contract. Nothing from it is shown or kept."""


def identifier(value):
    return isinstance(value, str) and IDENTIFIER.fullmatch(value) is not None


def safe_text(value, limit, multiline=False):
    if not isinstance(value, str) or len(value) > limit:
        return False
    for ch in value:
        cp = ord(ch)
        if cp in BIDI or 0xD800 <= cp <= 0xDFFF:
            return False
        if (cp < 32 or 0x7F <= cp < 0xA0) and not (multiline and ch in "\n\t"):
            return False
    return True


def _string(value, limit, optional=True, multiline=False):
    if value is None:
        if optional:
            return None
        raise OrgError("missing")
    if not safe_text(value, limit, multiline):
        raise OrgError("text")
    return value


def _mailbox(value):
    if value is None:
        return None
    if not isinstance(value, dict) or not canonical_mailbox(value.get("address")):
        raise OrgError("mailbox")
    return {"address": value["address"], "handle": _string(value.get("handle"), 256)}


def _list_of_ids(value, limit=16):
    if value is None:
        return []
    if not isinstance(value, list) or len(value) > limit or not all(identifier(v) for v in value) or len(set(value)) != len(value):
        raise OrgError("list")
    return list(value)


def parse_org(raw):
    if not isinstance(raw, dict) or not identifier(raw.get("id")) or not identifier(raw.get("my_member_id")):
        raise OrgError("org")
    name = _string(raw.get("name"), 256, optional=False)
    if not name.strip():
        raise OrgError("name")
    status = raw.get("status")
    if not identifier(status):
        raise OrgError("status")
    settings = raw.get("settings") if isinstance(raw.get("settings"), dict) else {}
    creation = settings.get("rooms_creation")
    return {
        "id": raw["id"], "name": name, "status": status, "member_id": raw["my_member_id"],
        "namespace": _string(raw.get("namespace"), 128),
        "rooms_creation": creation if creation in ("everyone", "admins") else None,
        "roles": _list_of_ids(raw.get("my_roles")),
        "mailbox": _mailbox(raw.get("my_mailbox")),
    }


def parse_catalog(raw):
    """`GET /v1/orgs` → {orgs, operated}. Strict: one bad row refuses the whole answer."""
    if not isinstance(raw, dict) or not isinstance(raw.get("orgs"), list) or len(raw["orgs"]) > 1000:
        raise OrgError("catalog")
    orgs = [parse_org(o) for o in raw["orgs"]]
    if len({o["id"] for o in orgs}) != len(orgs):
        raise OrgError("duplicate")
    operated = []
    rows = raw.get("operated") or []
    if not isinstance(rows, list) or len(rows) > 10000:
        raise OrgError("operated")
    ids = {o["id"] for o in orgs}
    for row in rows:
        if not isinstance(row, dict) or not canonical_mailbox(row.get("address")) or row.get("org_id") not in ids:
            raise OrgError("operated row")
        operated.append({"address": row["address"], "org_id": row["org_id"], "handle": _string(row.get("handle"), 256),
                         "capabilities": _list_of_ids(row.get("capabilities"))})
    return {"orgs": orgs, "operated": operated}


def readable(org):
    return org["status"] in ("active", "past_due", "read_only")


def writable(org):
    return org["status"] in ("active", "past_due")


def may_create(org, acting):
    """A display hint only; the server rechecks membership, status and policy on creation."""
    if not writable(org) or not org["mailbox"] or org["mailbox"]["address"] != acting:
        return False
    if org["rooms_creation"] == "everyone":
        return True
    if org["rooms_creation"] == "admins":
        return any(r in CREATOR_ROLES for r in org["roles"])
    return False


def owned_org_mailboxes(catalog, mailboxes):
    """Org inboxes that are this account's own (listed by /v1/identities). Operated agents are
    never included, even when their address happens to be listed elsewhere."""
    own = {row.get("address") for row in mailboxes or []}
    return {o["id"]: o["mailbox"]["address"] for o in catalog["orgs"]
            if o["mailbox"] and o["mailbox"]["address"] in own}


def operated_agents(catalog):
    """Agents delegated to this person: shown apart, with what the delegation allows."""
    names = {o["id"]: o["name"] for o in catalog["orgs"]}
    return [{**a, "org_name": names.get(a["org_id"], "")} for a in catalog["operated"]]


def parse_rooms(raw):
    if not isinstance(raw, dict) or not isinstance(raw.get("rooms"), list) or len(raw["rooms"]) > 50:
        raise OrgError("rooms")
    rooms = []
    for row in raw["rooms"]:
        if not isinstance(row, dict) or not identifier(row.get("id")) or not identifier(row.get("join_policy")):
            raise OrgError("room")
        count = row.get("member_count")
        if not isinstance(count, int) or isinstance(count, bool) or count < 0:
            raise OrgError("count")
        state = row.get("my_state")
        if state is not None and not identifier(state):
            raise OrgError("state")
        name = _string(row.get("name"), 256, optional=False)
        if not name.strip():
            raise OrgError("name")
        rooms.append({"id": row["id"], "name": name, "description": _string(row.get("description"), 2000, multiline=True),
                      "member_count": count, "join_policy": row["join_policy"], "my_state": state})
    if len({r["id"] for r in rooms}) != len(rooms):
        raise OrgError("duplicate")
    return rooms


def room_action(room, org=None):
    """open | accept | join | unavailable — from the server's own state for this mailbox."""
    state = room.get("my_state")
    if state in ("active", "keying"):
        return "open"
    if org is not None and not writable(org):
        return "unavailable"
    if state == "invited":
        return "accept"
    if state in (None, "left", "removed"):
        return "join" if room.get("join_policy") == "public" else "unavailable"
    return "unavailable"


def check_preview(raw, gid):
    if not isinstance(raw, dict) or raw.get("id") != gid:
        raise OrgError("preview")
    _string(raw.get("name"), 256, optional=False)
    _string(raw.get("description"), 2000, multiline=True)
    return raw


def check_created(raw, mailbox, org_id):
    """`POST /v1/groups` answer for an org room this mailbox just created (or the original one for
    the same request key)."""
    group = raw.get("group") if isinstance(raw, dict) else None
    my = group.get("my") if isinstance(group, dict) else None
    if (not isinstance(group, dict) or not identifier(group.get("id")) or group.get("org_id") != org_id
            or group.get("deleted_at") or not isinstance(my, dict) or my.get("role") != "owner"
            or my.get("state") not in ("active", "keying") or group.get("created_by") not in (None, mailbox)):
        raise OrgError("created")
    return group


# ---- room creation custody ----------------------------------------------------------------------

def new_request_key():
    return "rk_" + secrets.token_hex(16)


def room_request(org_id, name, description="", join_policy="public", post_policy="everyone", listed=True, request_key=None):
    """The exact `POST /v1/groups` body for an org room, as bytes. Raises ValueError."""
    if not identifier(org_id):
        raise ValueError("Choose a valid organization.")
    name, description = (name or "").strip(), (description or "").strip()
    if not name or not safe_text(name, 80) or not safe_text(description, 500, multiline=True):
        raise ValueError("Use a name of 1–80 characters and a description of at most 500 characters.")
    if join_policy not in ("public", "private") or post_policy not in ("everyone", "admins"):
        raise ValueError("Choose who can join and post.")
    key = request_key or new_request_key()
    if not identifier(key) or len(key) < 8:
        raise ValueError("This creation attempt is invalid.")
    body = {"request_key": key, "org_id": org_id, "name": name, "join_policy": join_policy, "post_policy": post_policy,
            "visibility": "listed" if listed else "unlisted", "history_visible_to_new_members": False}
    if description:
        body["description"] = description
    return json.dumps(body, separators=(",", ":"), ensure_ascii=False, sort_keys=True).encode()


def parse_room_request(wire):
    doc = json.loads(wire)
    rebuilt = room_request(doc.get("org_id"), doc.get("name"), doc.get("description") or "", doc.get("join_policy"),
                           doc.get("post_policy"), doc.get("visibility") == "listed", doc.get("request_key"))
    if rebuilt != bytes(wire):
        raise ValueError("saved request")
    return doc


def pending_rooms(record):
    """Saved, unconfirmed room creations in a protected record, validated again; corrupt → OrgError."""
    rows = ((record or {}).get(ROOM_RECORD) or {}).get("pending_rooms") or []
    if not isinstance(rows, list) or len(rows) > MAX_PENDING_ROOMS:
        raise OrgError("pending")
    out = []
    for row in rows:
        try:
            wire = base64.b64decode(row["wire"], validate=True)
            doc = parse_room_request(wire)
        except (KeyError, TypeError, ValueError):
            raise OrgError("pending") from None
        if doc["request_key"] != row.get("request_key") or doc["org_id"] != row.get("org_id"):
            raise OrgError("pending")
        out.append({"request_key": doc["request_key"], "org_id": doc["org_id"], "name": doc["name"], "wire": wire,
                    "created_at": row.get("created_at") if isinstance(row.get("created_at"), int) else 0,
                    "group_id": row.get("group_id") if identifier(row.get("group_id")) else None})
    return out


def save_pending(record, wire, now=None):
    """Add one creation to the record before it is posted. Same key + same bytes is a no-op;
    the same key with other bytes, or another pending room of the same org, is refused."""
    doc = parse_room_request(wire)
    rows = pending_rooms(record)
    for row in rows:
        if row["request_key"] == doc["request_key"]:
            if row["wire"] == bytes(wire):
                return
            raise ValueError("Check or discard the earlier creation before changing its details.")
        if row["org_id"] == doc["org_id"]:
            raise ValueError("A room creation in this organization is not confirmed yet. Retry or discard it first.")
    if len(rows) >= MAX_PENDING_ROOMS:
        raise ValueError("Review unconfirmed room creations before starting another.")
    section = record.setdefault(ROOM_RECORD, {})
    section.setdefault("pending_rooms", []).append({
        "request_key": doc["request_key"], "org_id": doc["org_id"], "wire": base64.b64encode(bytes(wire)).decode(),
        "created_at": int(now if now is not None else time.time())})


def remove_pending(record, request_key):
    """Remove exactly this creation (by its key). Returns whether it was there."""
    pending_rooms(record)
    rows = (record.get(ROOM_RECORD) or {}).get("pending_rooms") or []
    kept = [r for r in rows if r.get("request_key") != request_key]
    if len(kept) == len(rows):
        return False
    record[ROOM_RECORD]["pending_rooms"] = kept
    return True


def uncertain(error):
    """Could the server have committed this write without us seeing the answer?"""
    status = getattr(error, "status", None)
    if not isinstance(error, APIError):
        return not isinstance(error, (ValueError, OrgError))
    if getattr(error, "code", "") == "retired":
        return True  # stopped locally; the saved request stays for that account
    return status == 0 or status == 408 or status >= 500


ACCESS_LOST = {"not_org_member", "forbidden", "org_inactive", "not_found", "feature_disabled"}


def explain(error):
    code = getattr(error, "code", "")
    status = getattr(error, "status", 0)
    if code == "rate_limited" or status == 429:
        return "Too many requests. Wait before trying again."
    if code == "not_org_member":
        return "Act as your organization inbox to use its rooms."
    if code in ("forbidden", "posting_restricted"):
        return "Your organization doesn’t allow this."
    if code == "org_read_only":
        return "This organization is read-only right now."
    if code in ("org_inactive", "not_found"):
        return "This organization or room is not available any more."
    if code == "idempotency_conflict":
        return "This creation attempt conflicts with an earlier one. Discard it and start again."
    if isinstance(error, OrgError):
        return "Pigeonpost returned an unexpected response. Refresh and try again."
    return str(error) or "This action could not be completed."


def retry_after(error, default=30):
    try:
        value = int((getattr(error, "headers", {}) or {}).get("retry-after", default))
    except (TypeError, ValueError):
        value = default
    return max(1, min(3600, value))


class OrgsAPI:
    """The org routes, through a postbox bound to the accepted login (api.Postbox.bound)."""

    def __init__(self, api):
        self.api = api

    def catalog(self):
        return parse_catalog(self.api.call("GET", "/v1/orgs"))

    def rooms(self, mailbox, org_id, search=""):
        if not identifier(org_id):
            raise OrgError("org")
        if not safe_text(search, 80):
            raise ValueError("Use at most 80 characters to search.")
        # The org_id is required: without it this would be the personal public directory.
        query = urllib.parse.urlencode({"org_id": org_id, "q": search})
        return parse_rooms(self.api.scoped("GET", "/v1/rooms?" + query, mailbox))

    def preview(self, mailbox, gid):
        if not identifier(gid):
            raise OrgError("room")
        return check_preview(self.api.scoped("POST", "/v1/groups/resolve", mailbox, {"group_id": gid}), gid)

    def join(self, mailbox, gid):
        if not identifier(gid):
            raise OrgError("room")
        return self.api.scoped("POST", f"/v1/groups/{gid}/join", mailbox, {})

    def accept(self, mailbox, gid):
        if not identifier(gid):
            raise OrgError("room")
        return self.api.scoped("POST", f"/v1/groups/{gid}/invitation", mailbox, {"accept": True})

    def create_exact(self, mailbox, wire):
        doc = parse_room_request(wire)
        result = self.api.scoped("POST", "/v1/groups", mailbox, raw=bytes(wire), content_type="application/json")
        return check_created(result, mailbox, doc["org_id"])
