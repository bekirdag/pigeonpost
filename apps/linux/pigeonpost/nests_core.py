"""Nests (hosted agents) for the Linux client: the rules, no GUI (docs/nests/phase-b.md).

Everything here is a display hint over the postbox's own answers: packages, allowance, a Nest's
status, its files, its approval cards and its usage. Every action asks the server again. Purchases
happen on the website (desktop has no in-app payment); this client shows what the account holds
and opens https://pigeonpost.dev/account for buying, upgrading and managing a subscription.

Two rules matter for safety:
- An approval is decided on exactly the call that was shown: the body echoes the card's
  `args_hash`, never one computed here. A changed call answers `approval_changed`.
- A Nest is deleted only when its name is typed back exactly; the server checks it again.
"""

import re
import time
import unicodedata
import urllib.parse

from .api import APIError

NESTS = "nests"
WEB_PRODUCTS = "https://pigeonpost.dev/account#products"
MAX_UPLOAD = 25 * 1024 * 1024
# The Nests host builds an export from at most 512 MiB of files plus their text and conversations.
MAX_EXPORT = 1024 * 1024 * 1024
NEST_ADMIN_ROLES = {"owner", "admin", "agent_admin"}
NEST_ID = re.compile(r"nst_[0-9a-f]{1,60}")
FILE_ID = re.compile(r"nfl_[0-9a-f]{24}")
APPROVAL_ID = re.compile(r"nap_[0-9a-f]{24}")
SETUP_ID = re.compile(r"nsu_[0-9a-f]{1,60}")

STATUS_TEXT = {
    "ready": "Ready",
    "working": "Working…",
    "needs_you": "Needs you",
    "paused": "Paused",
    "unavailable": "Unavailable",
}
REASON_TEXT = {
    "payment": "Its package has ended. Renew it on the website to bring the Nest back.",
    "over_capacity": "Your package has fewer Nests than you have. Delete one or upgrade.",
    "setting_up": "Setting up…",
    "failed": "Setup failed. Retry from Products.",
}
# Paused for money: only the funding sweep brings these back, never the owner's Resume.
FUNDING_PAUSES = ("unfunded", "over_capacity")
SERVICES = [
    ("Personal assistant", "Tasks, reminders and scheduling drafts from your own documents."),
    ("AI CRM", "Contacts, pipeline stages and follow-up drafts."),
    ("AI HR", "Job descriptions, candidate intake and interview briefs."),
    ("Shopping assistant", "Wish lists and comparisons from sources you approve."),
    ("Marketing assistant", "Brand knowledge, campaign plans and drafts for review."),
]


class NestError(Exception):
    """An answer that does not match the contract; nothing from it is shown."""


def nests_on(features):
    return NESTS in (features or ())


def _text(value, limit=300, optional=True):
    if value is None and optional:
        return None
    if not isinstance(value, str) or len(value) > limit:
        raise NestError("text")
    return "".join(ch for ch in value if ch >= " " or ch in "\n\t")


def _int(value, optional=True):
    if value is None and optional:
        return None
    if isinstance(value, bool) or not isinstance(value, int):
        raise NestError("int")
    return value


def parse_packages(raw):
    """`GET /v1/nest-packages`: the offered packages, in display order."""
    if not isinstance(raw, dict) or not isinstance(raw.get("packages"), list):
        raise NestError("packages")
    out = []
    for p in raw["packages"]:
        if not isinstance(p, dict):
            raise NestError("package")
        out.append({
            "code": _text(p.get("code"), 64, False),
            "name": _text(p.get("name"), 80, False),
            "slots": _int(p.get("slots"), False),
            "storage_bytes": _int(p.get("storage_bytes"), False),
            "active_hours": _int(p.get("active_hours")) or 0,
            "input_tokens": _int(p.get("input_tokens")) or 0,
            "output_tokens": _int(p.get("output_tokens")) or 0,
            "model": p.get("model") if p.get("model") in ("managed", "none") else "none",
            "sellable": p.get("sellable") is True,
            "rank": _int(p.get("rank")) or 0,
        })
    return {"catalog_version": _text(raw.get("catalog_version"), 64), "packages": [p for p in out if p["sellable"]]}


def parse_nest(raw):
    if not isinstance(raw, dict) or not isinstance(raw.get("nest_id"), str) or not NEST_ID.fullmatch(raw["nest_id"]):
        raise NestError("nest")
    return {
        "nest_id": raw["nest_id"],
        "address": _text(raw.get("address"), 256),
        "label": _text(raw.get("label"), 64, False),
        "state": _text(raw.get("state"), 32) or "unknown",
        "status": raw.get("status") if raw.get("status") in STATUS_TEXT else "unavailable",
        "status_reason": _text(raw.get("status_reason"), 64),
        "role": raw.get("role") if raw.get("role") in ("owner", "operator") else "owner",
        "funding": _text(raw.get("funding"), 16),
        "paused_reason": _text(raw.get("paused_reason"), 32),
        "delete_after": _int(raw.get("delete_after")),
    }


def parse_snapshot(raw):
    """`GET /v1/nests`: allowance, Nests and paid setups."""
    if not isinstance(raw, dict):
        raise NestError("snapshot")
    allowance = raw.get("allowance") if isinstance(raw.get("allowance"), dict) else {}
    funding = allowance.get("funding") if isinstance(allowance.get("funding"), dict) else None
    setups = []
    for s in raw.get("setups") or []:
        if isinstance(s, dict) and isinstance(s.get("setup_id"), str) and SETUP_ID.fullmatch(s["setup_id"]):
            setups.append({"setup_id": s["setup_id"], "label": _text(s.get("label"), 64) or "Nest",
                           "status": "failed" if s.get("status") == "failed" else "pending",
                           "last_error": _text(s.get("last_error"), 200)})
    return {
        "slots": _int(allowance.get("slots")) or 0,
        "used": _int(allowance.get("used")) or 0,
        "package": _text(funding.get("package"), 64) if funding else None,
        "source": _text(funding.get("source"), 16) if funding else None,
        "duplicates": len(allowance.get("duplicates") or []),
        "nests": [parse_nest(n) for n in raw.get("nests") or []],
        "setups": setups,
    }


def status_line(nest):
    """The conversation header: a short status and, when useful, why."""
    text = STATUS_TEXT.get(nest["status"], "Unavailable")
    reason = REASON_TEXT.get(nest.get("status_reason") or "")
    return text, reason


def can_resume(nest):
    """Resume is offered only for a pause the owner can lift; the server refuses the others."""
    return nest["state"] == "paused" and nest.get("paused_reason") not in FUNDING_PAUSES


def can_pause(nest):
    return nest["state"] in ("ready", "running", "stopped")


def deletion_notice(nest):
    """An unfunded Nest is kept for the export window, then deleted: say when, in local time."""
    when = nest.get("delete_after")
    if not when:
        return None
    day = time.strftime("%-d %B %Y", time.localtime(when))
    return f"It will be deleted on {day} unless its package is renewed. Export it before then to keep its files."


def label_problem(value):
    """Why this cannot be a Nest's name (the server's rule: 1-64 printable characters), or None."""
    if not value or len(value) > 64 or any(unicodedata.category(ch) == "Cc" for ch in value):
        return "Use 1 to 64 characters."
    return None


def can_add_nest(snapshot):
    return bool(snapshot and snapshot["package"]) and snapshot["used"] < snapshot["slots"]


def org_can_manage(org):
    """A display hint for the admin-only organization actions; the server decides."""
    return bool(NEST_ADMIN_ROLES.intersection((org or {}).get("roles") or ()))


def peer_addresses(peer, messages):
    """Every address this conversation may be known by: conversations are keyed by handle when
    the peer has one, while a Nest is known by its mailbox address."""
    found = {peer} if peer else set()
    for message in messages or ():
        if peer and message.get("peer_handle") == peer and message.get("peer"):
            found.add(message["peer"])
    return found


def upload_header(name):
    """The display name as the `x-file-name` header: percent-encoded UTF-8, controls removed."""
    clean = "".join(ch for ch in (name or "") if ch >= " " and ch != "\x7f")[:200].strip()
    if not clean:
        raise NestError("name")
    return urllib.parse.quote(clean, safe="")


def parse_file(f):
    if not isinstance(f, dict) or not isinstance(f.get("file_id"), str) or not FILE_ID.fullmatch(f["file_id"]):
        raise NestError("file")
    cov = f.get("coverage") if isinstance(f.get("coverage"), dict) else {}
    warnings = f.get("warnings") if isinstance(f.get("warnings"), list) else []
    return {"file_id": f["file_id"], "name": _text(f.get("name"), 200, False), "bytes": _int(f.get("bytes")) or 0,
            "status": _text(f.get("status"), 16) or "failed", "reason": _text(f.get("reason"), 300),
            "units_total": _int(cov.get("units_total")), "units_text": _int(cov.get("units_text")),
            # Warnings come from a parser reading hostile files: shortened, never a reason to fail.
            "warnings": [_text(w[:300], 300) for w in warnings if isinstance(w, str)][:20]}


def parse_files(raw):
    if not isinstance(raw, dict) or not isinstance(raw.get("files"), list):
        raise NestError("files")
    files = [parse_file(f) for f in raw["files"]]
    return {"files": files, "used_bytes": _int(raw.get("used_bytes")) or 0, "quota_bytes": _int(raw.get("quota_bytes")) or 0}


def upload_outcome(raw):
    """What to tell the person after an upload: a stored file may still be unreadable."""
    f = parse_file(raw)
    if f["status"] == "rejected":
        return f"{f['name']} could not be read: {f['reason'] or 'unsupported file'}"
    if f["status"] == "partial":
        return f"{f['name']} added. {coverage_text(f)}."
    return f"{f['name']} added."


def coverage_text(f):
    if f["status"] == "rejected":
        return f["reason"] or "Could not be read"
    if f["units_total"]:
        return f"Read {f['units_text'] or 0} of {f['units_total']}"
    return "Read" if f["status"] == "parsed" else f["status"].capitalize()


def parse_approvals(raw):
    if not isinstance(raw, dict) or not isinstance(raw.get("approvals"), list):
        raise NestError("approvals")
    cards = []
    for c in raw["approvals"]:
        if not isinstance(c, dict) or not isinstance(c.get("approval_id"), str) or not APPROVAL_ID.fullmatch(c["approval_id"]):
            raise NestError("approval")
        if not isinstance(c.get("args_hash"), str) or not re.fullmatch(r"[0-9a-f]{64}", c["args_hash"]):
            raise NestError("hash")
        call = c.get("call") if isinstance(c.get("call"), dict) else {}
        args = call.get("args") if isinstance(call.get("args"), dict) else {}
        cards.append({"approval_id": c["approval_id"], "tool": _text(c.get("tool"), 64, False), "args_hash": c["args_hash"],
                      "status": _text(c.get("status"), 16) or "pending",
                      "to": _text(args.get("to"), 200), "body": _text(args.get("body"), 4000),
                      "expires_at": _int(c.get("expires_at")), "result": _text(c.get("result"), 200)})
    return cards


def can_approve(card):
    """Approve is offered only for a call shown in full. The `to` must also be free of invisible
    format characters, which could make one address look like another; such a card can be rejected."""
    return (card["tool"] == "send_message" and bool(card["to"]) and bool(card["body"])
            and not any(unicodedata.category(ch) == "Cf" for ch in card["to"]))


def pending_cards(cards, now=None):
    moment = time.time() if now is None else now
    return [c for c in cards or () if c["status"] == "pending" and (c["expires_at"] is None or c["expires_at"] > moment)]


def expiry_text(card, now=None):
    if card["expires_at"] is None:
        return None
    left = int(card["expires_at"] - (time.time() if now is None else now))
    if left <= 0:
        return "Expired"
    return f"Expires in {left // 3600} h" if left >= 3600 else f"Expires in {max(1, left // 60)} min"


def approval_body(card):
    """What a decision sends: the hash of the call the person saw, unchanged."""
    return {"args_hash": card["args_hash"]}


def card_text(card):
    if card["tool"] == "send_message" and card["to"] and card["body"]:
        return f"Send a message to {card['to']}:\n{card['body']}"
    return f"An action this app cannot show in full ({card['tool']}). Reject it, or update Pigeonpost to review it."


def delete_confirmed(label, typed):
    return isinstance(typed, str) and typed == label


def _count(value):
    """A ledger number for display: anything else counts as nothing rather than breaking the page."""
    return value if isinstance(value, int) and not isinstance(value, bool) and value >= 0 else 0


def usage_lines(raw):
    if not isinstance(raw, dict):
        raise NestError("usage")
    tin = raw.get("input_tokens") if isinstance(raw.get("input_tokens"), dict) else {}
    tout = raw.get("output_tokens") if isinstance(raw.get("output_tokens"), dict) else {}
    secs = _count(raw.get("running_secs"))
    package = raw.get("package") if isinstance(raw.get("package"), dict) else {}
    hours = _count(package.get("active_hours_pooled"))
    if _count(tin.get("limit")) or _count(tout.get("limit")):
        ai = (f"AI this month: {_count(tin.get('used')):,} of {_count(tin.get('limit')):,} input tokens, "
              f"{_count(tout.get('used')):,} of {_count(tout.get('limit')):,} output tokens")
    else:
        ai = "AI this month: no AI is connected to this Nest"
    running = f"Running time this month: {secs // 3600} h {secs % 3600 // 60} min"
    if hours:
        running += f" (the package includes {hours} h, shared by its Nests)"
    return [ai, running, f"Files: {_count(raw.get('files_bytes')) / 1048576:.1f} MiB",
            f"AI requests this month: {_count(raw.get('requests')):,}"]


def explain(error):
    """Words for the person. Anything but a postbox answer is a generic line, never exception text."""
    code = getattr(error, "code", "")
    known = {
        "feature_disabled": "Nests are not available for this account yet.",
        "no_nest_package": "Buy a Nest package on the website first.",
        "nest_slots_full": "Every Nest in your package is in use.",
        "approval_changed": "The action changed since you saw it. Review it again.",
        "approval_closed": "This approval was already decided, replaced or expired.",
        "recipient_not_allowed": "A Nest may only message you or people you have allowed.",
        "needs_payment": "This Nest is paused until its package is paid for.",
        "over_capacity": "Your package has fewer Nests than you have. Delete one or upgrade.",
        "storage_full": "This Nest’s storage is full.",
        "too_large": "Choose a file smaller than 25 MiB.",
        "confirm_required": "Type the Nest’s name exactly to delete it.",
        "export_too_large": "This Nest is larger than one download. Contact support.",
        "no_nest_slot": "The organization has no free Nest slot. Buy or upgrade a package on the website.",
        "nest_limit": "This account already holds its limit of Nests.",
        "bad_label": "Use 1 to 64 characters for the name.",
        "bad_name": "This file needs a name.",
        "empty_file": "This file is empty.",
        "too_many_files": "This Nest holds its maximum number of files. Delete one first.",
        "upload_failed": "The file was not stored. Try again.",
        "no_file": "This file is gone.",
        "no_nest": "This Nest is no longer available.",
        "no_approval": "This approval is gone.",
        "tool_failed": "The action could not be carried out.",
        "nests_unavailable": "The Nests service is not available right now.",
        "nestd_failed": "The Nests service did not answer. Try again.",
        "response_too_large": "This Nest is larger than one download. Contact support.",
    }
    if code in known:
        return known[code]
    return str(error) if isinstance(error, APIError) else "This could not be completed. Please try again."


class NestsAPI:
    """The Nest routes over a `Postbox` (or a test double with the same `call`)."""

    def __init__(self, postbox):
        self.p = postbox

    def packages(self):
        return parse_packages(self.p.call("GET", "/v1/nest-packages"))

    def snapshot(self):
        return parse_snapshot(self.p.call("GET", "/v1/nests"))

    def nest(self, nest_id):
        return parse_nest(self.p.call("GET", "/v1/nests/" + nest_id))

    def create(self, label):
        # Provisioning runs inside the request.
        return self.p.call("POST", "/v1/nests", data={"label": label}, timeout=180)

    def retry_setup(self, setup_id):
        if not SETUP_ID.fullmatch(setup_id or ""):
            raise NestError("setup")
        return parse_snapshot(self.p.call("POST", f"/v1/nest-setups/{setup_id}/retry", data={}))

    def files(self, nest_id):
        return parse_files(self.p.call("GET", f"/v1/nests/{nest_id}/files"))

    def upload(self, nest_id, name, content):
        if not content:
            raise APIError(0, "empty_file", "This file is empty.")
        if len(content) > MAX_UPLOAD:
            raise APIError(0, "too_large", "Choose a file smaller than 25 MiB.")
        return self.p.call("POST", f"/v1/nests/{nest_id}/files", raw=content, timeout=180, headers={
            "Content-Type": "application/octet-stream", "x-file-name": upload_header(name)})

    def delete_file(self, nest_id, file_id):
        if not FILE_ID.fullmatch(file_id or ""):
            raise NestError("file")
        return self.p.call("DELETE", f"/v1/nests/{nest_id}/files/{file_id}")

    def approvals(self, nest_id):
        return parse_approvals(self.p.call("GET", f"/v1/nests/{nest_id}/approvals"))

    def decide(self, nest_id, card, approve):
        what = "approve" if approve else "reject"
        return self.p.call("POST", f"/v1/nests/{nest_id}/approvals/{card['approval_id']}/{what}", data=approval_body(card))

    def usage(self, nest_id):
        return usage_lines(self.p.call("GET", f"/v1/nests/{nest_id}/usage"))

    def pause(self, nest_id, resume=False):
        return self.p.call("POST", f"/v1/nests/{nest_id}/{'resume' if resume else 'pause'}", data={})

    def rename(self, nest_id, label):
        return self.p.call("PATCH", f"/v1/nests/{nest_id}", data={"label": label})

    def export(self, nest_id):
        return self.p.call("GET", f"/v1/nests/{nest_id}/export", binary=True, timeout=600, max_bytes=MAX_EXPORT)

    def delete(self, nest_id, label, typed):
        if not delete_confirmed(label, typed):
            raise APIError(0, "confirm_required", "Type the Nest’s name exactly to delete it.")
        return self.p.call("DELETE", f"/v1/nests/{nest_id}", data={"confirm": typed})

    def org_nests(self, org_id):
        raw = self.p.call("GET", f"/v1/orgs/{urllib.parse.quote(org_id, safe='')}/nests")
        if not isinstance(raw, dict) or not isinstance(raw.get("nests") or [], list):
            raise NestError("org nests")
        nests = []
        for n in raw.get("nests") or []:
            if isinstance(n, dict) and isinstance(n.get("nest_id"), str) and NEST_ID.fullmatch(n["nest_id"]):
                nests.append({"nest_id": n["nest_id"], "label": _text(n.get("label"), 64) or "Nest",
                              "role": "owner" if n.get("role") == "owner" else "operator",
                              "address": _text(n.get("address"), 256)})
        allowance = raw.get("allowance") if isinstance(raw.get("allowance"), dict) else {}
        return {"nests": nests, "slots": _int(allowance.get("slots")) or 0, "used": _int(allowance.get("used")) or 0}

    def create_org_nest(self, org_id, handle, label):
        return self.p.call("POST", f"/v1/orgs/{urllib.parse.quote(org_id, safe='')}/nests",
                           data={"handle": handle, "label": label}, timeout=180)
