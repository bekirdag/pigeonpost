"""Group end-to-end encryption client: the state machine of docs/groups.md §3 ("Client loop").

No GUI dependency. The window drives it from worker threads; the tests drive it against an
in-memory postbox with the real library. It mirrors the web inbox's `site-inbox/groups-core.js`
step for step, so every client behaves the same against the postbox.

What the postbox sees: who is in which group, which devices exist, ciphertext sizes and timing.
What it cannot see: message text, filenames, attachment keys, group secrets. Everything readable is
produced here by the MLS library and kept only in this device's encrypted local store
(`groupstore.py`): a device cannot decrypt a message twice.

Every operation that touches the MLS state holds one lock: the library keeps one pending commit per
group and the state must be written after each mutation, in order.
"""

import base64
import json
import random
import secrets
import threading
import time
import urllib.parse

ENC_SCHEME = "mls-rfc9420-x25519-aes128gcm-ed25519+ppgc1"
KEY_PACKAGE_TARGET = 20
KEY_PACKAGE_FLOOR = 10
HISTORY_LIMIT = 1000
MAX_RETRIES = 6
SELF_UPDATE_AFTER = 7 * 24 * 3600   # P-2 §13.7: refresh an idle leaf at least weekly
TIMERS = [None, 300, 3600, 28800, 86400, 604800, 2419200]
BASE32 = "abcdefghijklmnopqrstuvwxyz234567"
REMOVE_GONE = ("not_member", "group_deleted", "not_found", "feature_disabled")
EDIT_WINDOW = 24 * 3600              # an edit is allowed within 24 h of the chain's original
OUTBOX_LIMIT = 16                    # unconfirmed sends kept for an explicit retry (docs/groups.md)
OUTBOX_BYTES = 8 * 1024 * 1024
OUTBOX_RETRY_SECONDS = 24 * 3600
RECONCILE_PAGES = 20                 # metadata-only pages per reconciliation (100 items each)


def _int(value):
    """A message seq from the encrypted body (a string) or the envelope (an int); None otherwise."""
    if isinstance(value, bool):
        return None
    if isinstance(value, int):
        return value
    if isinstance(value, str) and value.isdigit() and len(value) <= 18:
        return int(value)
    return None


def b64(data):
    return base64.b64encode(bytes(data)).decode()


def unb64(text):
    return base64.b64decode(str(text or ""))


def random_id(n):
    return "".join(BASE32[b & 31] for b in secrets.token_bytes(n))


def new_device_id():
    return "linux-" + random_id(12)


def new_request_key():
    return "rk_" + random_id(20)


def new_client_message_id():
    return "cm_" + random_id(24)


def quote(value):
    return urllib.parse.quote(str(value), safe="")


def timer_label(secs):
    if not secs:
        return "Off"
    table = {300: "5 minutes", 3600: "1 hour", 28800: "8 hours", 86400: "1 day", 604800: "1 week", 2419200: "4 weeks"}
    return table.get(secs) or f"{round(secs / 60)} minutes"


class GroupError(Exception):
    """A refusal with the postbox's machine code (`revision_conflict`, `not_member`, …)."""

    def __init__(self, status, code, body=None):
        super().__init__(code)
        self.status, self.code, self.body = status, code, body or {}


class UncertainSend(GroupError):
    """The post may or may not have reached the postbox. Its exact request is kept in the sealed
    record for an explicit Retry (same bytes) or Dismiss; it is never replayed automatically."""

    def __init__(self, cmid):
        super().__init__(0, "send_uncertain")
        self.cmid = cmid


def ambiguous(error):
    """No answer, or a server failure: the request may have been processed."""
    if not hasattr(error, "status"):
        return False  # a local failure (library, bug): nothing reached the network layer
    status = status_of(error)
    return status in (0, 408) or (isinstance(status, int) and status >= 500)


def status_of(error):
    return getattr(error, "status", 0) or 0


def code_of(error):
    return getattr(error, "code", None)


# ---- message documents (inside the encryption; the postbox never sees any of it) -----------------

def encode_body(doc):
    out = {"v": 1, "type": doc.get("type") or "text"}
    if doc.get("text") is not None:
        out["text"] = str(doc["text"])[:64000]
    files = doc.get("files") or []
    if files:
        out["files"] = [{"id": str(f["id"]), "name": str(f.get("name") or "file")[:200],
                         "mime": str(f.get("mime") or "application/octet-stream")[:120], "size": int(f.get("size") or 0)}
                        for f in files]
    if doc.get("reply_to"):
        out["reply_to"] = str(doc["reply_to"])
    if doc.get("edit_of"):
        out["edit_of"] = str(doc["edit_of"])
    return json.dumps(out, separators=(",", ":"), ensure_ascii=False).encode()


def decode_body(data):
    try:
        doc = json.loads(bytes(data).decode())
        if not isinstance(doc, dict):
            raise ValueError("not an object")
    except (ValueError, UnicodeDecodeError):
        return {"type": "text", "text": bytes(data).decode("utf-8", "replace"), "files": [], "reply_to": None, "edit_of": None}
    files = []
    for f in (doc.get("files") if isinstance(doc.get("files"), list) else [])[:32]:
        if isinstance(f, dict):
            try:
                size = int(f.get("size") or 0)
            except (TypeError, ValueError):
                size = 0
            files.append({"id": str(f.get("id") or ""), "name": str(f.get("name") or "file"),
                          "mime": str(f.get("mime") or "application/octet-stream"), "size": size})
    return {
        "type": doc["type"] if isinstance(doc.get("type"), str) else "text",
        "text": doc["text"] if isinstance(doc.get("text"), str) else "",
        "files": files,
        "reply_to": doc["reply_to"] if isinstance(doc.get("reply_to"), str) else None,
        "edit_of": doc["edit_of"] if isinstance(doc.get("edit_of"), str) else None,
    }


def member_json(m):
    return {"mailbox": m.mailbox, "device_id": m.device_id, "fingerprint": bytes(m.fingerprint).hex()}


def ref_json(ref):
    return {"key": b64(ref.key), "sha": b64(ref.sealed_sha256), "len": int(ref.plaintext_len)}


def same_devices(verified, declared):
    if not isinstance(declared, list):
        return True  # nothing declared to compare with
    a = sorted(d["mailbox"] + "|" + d["device_id"] for d in verified)
    b = sorted(str(d.get("mailbox")) + "|" + str(d.get("device_id")) for d in declared if isinstance(d, dict))
    return a == b


def sort_key(item):
    seq = item.get("seq")
    return float("inf") if seq is None else float(seq)


class GroupClient:
    """One mailbox's group device. `api(method, path, body=None, device=None, raw=None, binary=False)`
    returns parsed JSON (bytes when `binary`) or raises an error carrying `status` and `code`."""

    def __init__(self, gc, api, mailbox, storage, *, now=None, sleep=None, jitter=None, on_event=None):
        self.gc, self._api, self.mailbox, self.storage = gc, api, mailbox, storage
        self.now = now or (lambda: int(time.time()))
        self.sleep = sleep or time.sleep
        self.jitter = jitter or (lambda: random.uniform(0, 3))
        self.emit = on_event or (lambda event: None)
        self.lock = threading.RLock()
        self.device = None
        self.record = None

    # -- plumbing --------------------------------------------------------------------------------

    def api(self, method, path, body=None, device=False, raw=None, binary=False):
        return self._api(method, path, body=body, device=self.record["device_id"] if device else None, raw=raw, binary=binary)

    def persist(self):
        self.record["state_blob"] = b64(self.device.export_state(unb64(self.record["state_key"])))
        self.storage.save(self.mailbox, self.record)

    def group_rec(self, gid):
        groups = self.record.setdefault("groups", {})
        if gid not in groups:
            groups[gid] = {"cursor": 0, "in_group": False, "acked": 0, "updated_at": 0}
        return groups[gid]

    def history_of(self, gid):
        return self.record.setdefault("history", {}).setdefault(gid, [])

    def _find(self, gid, seq):
        if seq is None:
            return None
        for x in self.history_of(gid):
            if x.get("kind") == "message" and x.get("seq") == seq:
                return x
        return None

    def _root_of(self, gid, item):
        """The original of an edit chain on this device (following verified edit links only)."""
        seen = set()
        while item and item.get("edit_of") and (item.get("edit_verified") or item.get("edit_link") == "server") and item.get("seq") not in seen:
            seen.add(item.get("seq"))
            parent = self._find(gid, _int(item["edit_of"]))
            if not parent:
                break
            item = parent
        return item

    def _chain(self, gid, item):
        """Every version of `item`'s chain this device holds, oldest first."""
        root = self._root_of(gid, item)
        if not root:
            return []
        chain, current, seen = [root], root, {root.get("seq")}
        while current.get("replaced_by") is not None:
            nxt = self._find(gid, current["replaced_by"])
            if not nxt or nxt.get("seq") in seen or nxt.get("sender") != root.get("sender"):
                break
            chain.append(nxt)
            seen.add(nxt.get("seq"))
            current = nxt
        return chain

    def _link_missing(self, gid, placeholder):
        """A verified edit replaced a version this device never received. That version's own
        envelope names the one before it; follow such links only within the same sender, only to
        earlier versions not replaced by anything else, and mark the history as incomplete."""
        seen = set()
        while placeholder and placeholder.get("missing") and placeholder.get("seq") not in seen:
            seen.add(placeholder.get("seq"))
            earlier = self._find(gid, placeholder.get("server_edit_of"))
            if (not earlier or earlier.get("sender") != placeholder.get("sender") or earlier.get("seq") >= placeholder.get("seq")
                    or earlier.get("deleted") or (earlier.get("replaced_by") not in (None, placeholder.get("seq")))):
                return
            earlier["replaced_by"] = placeholder["seq"]
            placeholder.update(edit_of=str(earlier["seq"]), edit_link="server")
            placeholder = earlier

    def remember(self, gid, item):
        items = self.history_of(gid)
        if item.get("seq") is not None:
            for index, existing in enumerate(items):
                if existing.get("seq") == item["seq"] and existing.get("kind") == item.get("kind"):
                    items[index] = {**existing, **item}
                    return
        items.append(item)
        items.sort(key=sort_key)
        if len(items) > HISTORY_LIMIT:
            del items[:len(items) - HISTORY_LIMIT]

    # -- device ----------------------------------------------------------------------------------

    def init(self):
        """Load this mailbox's device, or enrol a new one. Returns {device_id, persistent}."""
        with self.lock:
            self.record = self.storage.load(self.mailbox)
            self.device = None
            if self.record and self.record.get("state_blob") and self.record.get("registered"):
                try:
                    self.device = self.gc.GroupDevice.import_state(unb64(self.record["state_blob"]), unb64(self.record["state_key"]))
                except Exception:
                    self.record = None
            elif self.record and self.record.get("device_id") and not self.record.get("registered"):
                # Enrolment was interrupted after the key was made: the postbox may or may not have
                # it. Retire that id (its attestation is gone) and start clean.
                stale = self.record["device_id"]
                try:
                    self._api("DELETE", "/v1/mls/devices/" + quote(stale), device=None)
                except Exception:
                    pass
                self.record = None
            if not self.record or not self.device:
                self.enrol()
            elif self.record.get("outbox"):
                self.recover_outbox()
            return {"device_id": self.record["device_id"], "persistent": getattr(self.storage, "persistent", True)}

    def enrol(self):
        last = None
        for _ in range(3):
            device_id = new_device_id()
            device = self.gc.GroupDevice(self.mailbox, device_id)
            self.device = device
            self.record = {"device_id": device_id, "registered": False, "state_key": b64(self.gc.generate_state_key()),
                           "state_blob": None, "groups": {}, "history": {}, "verified": {}}
            self.persist()  # the private key exists before the postbox hears of it
            try:
                result = self._api("POST", "/v1/mls/devices", body={"device_id": device_id, "signature_key": b64(device.signature_public_key())})
            except Exception as exc:
                last = exc
                if code_of(exc) == "device_exists":
                    continue
                self.storage.remove(self.mailbox)
                raise
            device.install_attestation(unb64(result["mailbox_public_key"]), unb64(result["attestation"]))
            self.record["registered"] = True
            self.record["fingerprint"] = result.get("fingerprint_digits") or self.gc.fingerprint_digits(device.fingerprint())
            self.persist()
            self.emit({"type": "device_enrolled", "device_id": device_id})
            return
        raise last

    def re_enrol(self):
        with self.lock:
            self.storage.remove(self.mailbox)
            self.record = self.device = None
            self.enrol()

    def revoke(self):
        """Sign-out: retire this device at the postbox and delete its keys here."""
        with self.lock:
            if self.record and self.record.get("registered"):
                try:
                    self.api("DELETE", "/v1/mls/devices/" + quote(self.record["device_id"]), device=True)
                except Exception:
                    pass  # local keys go regardless; co-members see the device removed when it is
            self.storage.remove(self.mailbox)
            self.record = self.device = None

    def top_up_key_packages(self):
        with self.lock:
            count = self.api("GET", "/v1/mls/key-packages/count", device=True)
            unclaimed = int(count.get("unclaimed") or 0)
            if unclaimed >= KEY_PACKAGE_FLOOR and count.get("last_resort"):
                return 0
            need = max(0, KEY_PACKAGE_TARGET - unclaimed)
            packages = list(self.device.generate_key_packages(need)) if need else []
            body = {"request_key": new_request_key(), "key_packages": [b64(p) for p in packages]}
            if not count.get("last_resort"):
                body["last_resort"] = b64(self.device.generate_last_resort_key_package())
            self.persist()  # private init keys before the packages are public
            self.api("POST", "/v1/mls/key-packages", body, device=True)
            return len(packages)

    # -- welcomes --------------------------------------------------------------------------------

    def process_welcomes(self):
        with self.lock:
            welcomes = self.api("GET", "/v1/mls/welcomes", device=True).get("welcomes") or []
            joined = []
            for w in welcomes:
                ok, code = False, None
                gid = w.get("group_id")
                try:
                    if gid in self.device.groups():
                        self.device.forget_group(gid)
                    self.device.join_from_welcome(unb64(w["welcome"]))
                    g = self.group_rec(gid)
                    g.update(in_group=True, cursor=max(int(w.get("seq") or 0), 0), acked=0, updated_at=self.now())
                    ok = True
                    joined.append(gid)
                except Exception as exc:
                    code = code_of(exc) or "join_failed"
                self.persist()
                ack = {"request_key": new_request_key(), "joined": ok}
                if code:
                    ack["error_code"] = code
                try:
                    self.api("POST", "/v1/mls/welcomes/" + quote(w["welcome_id"]) + "/ack", ack, device=True)
                except Exception:
                    pass
                if ok:
                    self.remember(gid, {"kind": "notice", "seq": int(w.get("seq") or 0), "local": True, "notice": "you_joined", "created_at": self.now()})
                    self.persist()
                    self.ack_epoch(gid)
        if joined:
            # A last-resort package may have been reused for this join; a self-update replaces that
            # leaf key at once (P-2 §13.6). Only when no single-use packages are left, since that is
            # exactly when the last-resort one was handed out.
            try:
                count = self.api("GET", "/v1/mls/key-packages/count", device=True)
            except Exception:
                count = None
            if count is not None and not count.get("unclaimed"):
                for gid in joined:
                    try:
                        self.self_update(gid)
                    except Exception:
                        pass
        return joined

    def ack_epoch(self, gid):
        epoch = self.device.epoch(gid)
        g = self.group_rec(gid)
        if epoch <= (g.get("acked") or 0):
            return
        try:
            self.api("POST", f"/v1/groups/{quote(gid)}/epoch-ack", {"epoch": epoch}, device=True)
            g["acked"] = epoch
        except Exception:
            pass  # retried after the next commit

    # -- reading ---------------------------------------------------------------------------------

    def sync(self, gid):
        with self.lock:
            return self._sync(gid)

    def _sync(self, gid, up_to=None):
        g = self.group_rec(gid)
        if not g.get("in_group"):
            return {"processed": 0}
        processed = 0
        for _ in range(1000):
            try:
                res = self.api("GET", f"/v1/groups/{quote(gid)}/messages?after_seq={g['cursor']}&limit=100", device=True)
            except Exception as exc:
                if status_of(exc) == 410:
                    self._resync(gid)
                    return {"processed": processed, "resync": True}
                raise
            items = sorted(res.get("items") or [], key=lambda i: i.get("seq", 0))
            stopped = False
            dirty = False
            for item in items:
                if up_to is not None and item["seq"] > up_to:
                    stopped = True
                    break
                if item["seq"] <= g["cursor"]:
                    continue
                outcome = self._process_item(gid, item)
                g["cursor"] = item["seq"]
                processed += 1
                dirty = True
                if outcome == "removed":
                    self.persist()
                    self.emit({"type": "removed", "group_id": gid})
                    return {"processed": processed, "removed": True}
                if outcome == "gap":
                    self.persist()
                    return {"processed": processed, "gap": True}
                if outcome == "commit":
                    self.persist()  # the new epoch is saved before the postbox is told
                    dirty = False
                    self.ack_epoch(gid)
            if dirty:
                self.persist()
            if stopped or not res.get("more") or not items:
                break
        if processed:
            self.emit({"type": "history", "group_id": gid})
        return {"processed": processed}

    def is_own(self, item):
        return item.get("sender") == self.mailbox and item.get("sender_device") == self.record["device_id"]

    def _process_item(self, gid, item):
        """'message' | 'commit' | 'notice' | 'removed' | 'gap' | 'skipped'."""
        kind = item.get("type")
        if kind == "notice":
            self.remember(gid, {"kind": "notice", "seq": item["seq"], "notice": item.get("kind"), "actor": item.get("actor"),
                                "metadata": item.get("metadata") or {}, "created_at": item.get("created_at"),
                                "expires_at": item.get("expires_at")})
            return "notice"
        if kind == "commit":
            if self.is_own(item) and not self.device.has_pending_commit(gid):
                return "skipped"  # merged when it was accepted
            try:
                result = self.device.process_incoming(gid, unb64(item.get("commit")))
            except Exception as exc:
                if code_of(exc) == "future_epoch":
                    return "gap"
                # A commit this device cannot verify: it stops at this epoch and fails closed (P-2 §3).
                self.remember(gid, {"kind": "warning", "seq": item["seq"], "warning": "commit_rejected",
                                    "code": code_of(exc) or "mls", "created_at": item.get("created_at")})
                return "skipped"
            if result.is_COMMIT() or result.is_OWN_COMMIT_CONFIRMED():
                o = result.outcome
                verified = {"committer": member_json(o.committer), "added": [member_json(m) for m in o.added],
                            "removed": [member_json(m) for m in o.removed], "updated": [member_json(m) for m in o.updated]}
                self.remember(gid, {"kind": "commit", "seq": item["seq"], "epoch": o.epoch, **verified, "created_at": item.get("created_at")})
                if not same_devices(verified["added"], item.get("adds")) or not same_devices(verified["removed"], item.get("removes")):
                    self.remember(gid, {"kind": "warning", "seq": item["seq"] + 0.5, "warning": "membership_mismatch",
                                        "created_at": item.get("created_at")})
                if result.is_OWN_COMMIT_CONFIRMED():
                    self.group_rec(gid)["updated_at"] = self.now()
                if o.self_removed:
                    try:
                        self.device.forget_group(gid)
                    except Exception:
                        pass
                    self.group_rec(gid)["in_group"] = False
                    return "removed"
                return "commit"
            return "skipped"
        if kind == "application":
            if item.get("deleted_at"):
                self._erase_chain(gid, item["seq"])
                self.remember(gid, {"kind": "message", "seq": item["seq"], "sender": item.get("sender"), "deleted": True,
                                    "created_at": item.get("created_at"), "expires_at": item.get("expires_at")})
                return "message"
            if item.get("ciphertext") is None:
                # An earlier version this device never received (the postbox cleared it when it was
                # edited). The server's replaced_by is only a hint: it hides nothing until a verified
                # edit from the same sender folds it.
                self.remember(gid, {"kind": "message", "seq": item["seq"], "sender": item.get("sender"), "missing": True,
                                    "server_replaced_by": item.get("replaced_by"), "server_edit_of": _int(item.get("edit_of")),
                                    "created_at": item.get("created_at"), "expires_at": item.get("expires_at")})
                return "message"
            if self.is_own(item):
                # Recorded when it was sent; the server copy only confirms its seq.
                for mine in self.history_of(gid):
                    if mine.get("kind") == "message" and mine.get("cmid") == item.get("client_message_id"):
                        mine.update(seq=item["seq"], created_at=item.get("created_at"), expires_at=item.get("expires_at"),
                                    pending=False, uncertain=False)
                        self._outbox_drop(item.get("client_message_id"))
                        if mine.get("edit_of") is not None:
                            target = self._find(gid, _int(mine["edit_of"]))
                            if target and target.get("sender") == self.mailbox:
                                target["replaced_by"] = item["seq"]
                        self.history_of(gid).sort(key=sort_key)
                        break
                return "skipped"
            try:
                result = self.device.process_incoming(gid, unb64(item["ciphertext"]))
            except Exception as exc:
                if code_of(exc) == "future_epoch":
                    return "gap"
                self.remember(gid, {"kind": "message", "seq": item["seq"], "sender": item.get("sender"), "undecryptable": True,
                                    "code": code_of(exc) or "mls", "created_at": item.get("created_at"), "expires_at": item.get("expires_at")})
                return "message"
            if not result.is_MESSAGE():
                return "skipped"
            message = result.message
            if message.sender.mailbox != item.get("sender"):
                self.remember(gid, {"kind": "warning", "seq": item["seq"], "warning": "sender_mismatch", "created_at": item.get("created_at")})
                return "skipped"
            doc = decode_body(message.body)
            refs = list(message.attachments or [])
            files = [{**f, "ref": ref_json(refs[i]) if i < len(refs) else None} for i, f in enumerate(doc["files"])]
            sender = item.get("sender")
            entry = {"kind": "message", "seq": item["seq"], "sender": sender, "sender_device": item.get("sender_device"),
                     "cmid": item.get("client_message_id"), "text": doc["text"], "files": files, "reply_to": None,
                     "edit_of": None, "created_at": item.get("created_at"), "expires_at": item.get("expires_at")}
            # A reply is shown only when the reference inside the encrypted body and the sequenced
            # envelope agree, and only to an earlier message.
            body_reply, wire_reply = _int(doc["reply_to"]), _int(item.get("reply_to"))
            if body_reply is not None and body_reply == wire_reply and body_reply < item["seq"]:
                entry["reply_to"] = str(body_reply)
            # An edit folds only a verified, earlier version from the same sender: the encrypted
            # reference must equal the envelope's, and the target must be that sender's current
            # version. Anything else stays an ordinary message with a warning; server-only hints
            # never replace text.
            body_edit, wire_edit = _int(doc["edit_of"]), _int(item.get("edit_of"))
            warning = None
            if body_edit is not None or wire_edit is not None:
                target = self._find(gid, body_edit) if body_edit is not None and body_edit == wire_edit and body_edit < item["seq"] else None
                if body_edit is None or body_edit != wire_edit:
                    warning = "edit_unverified"
                elif target is None:
                    # The earlier version never reached this device: an edit with missing history.
                    entry.update(edit_of=str(body_edit), edit_verified=True, history_missing=True)
                elif (target.get("sender") != sender or target.get("deleted") or target.get("undecryptable")
                      or (target.get("replaced_by") is not None and target.get("replaced_by") != item["seq"])):
                    warning = "edit_unverified"
                else:
                    entry.update(edit_of=str(body_edit), edit_verified=True,
                                 history_missing=bool(target.get("missing") or target.get("history_missing")))
                    target["replaced_by"] = item["seq"]
                    self._link_missing(gid, target)
            self.remember(gid, entry)
            if warning:
                self.remember(gid, {"kind": "warning", "seq": item["seq"] + 0.25, "warning": warning, "created_at": item.get("created_at")})
            return "message"
        return "skipped"

    def _resync(self, gid):
        try:
            self.device.forget_group(gid)
        except Exception:
            pass
        g = self.group_rec(gid)
        g["in_group"] = False
        self.remember(gid, {"kind": "warning", "seq": g["cursor"] + 0.5, "warning": "resynced", "created_at": self.now()})
        self.persist()
        try:
            self.api("POST", f"/v1/groups/{quote(gid)}/resync", {"request_key": new_request_key()}, device=True)
        except Exception:
            pass
        self.emit({"type": "resync", "group_id": gid})

    # -- commits ---------------------------------------------------------------------------------

    def _commit(self, gid, plan, activation=False):
        """Build, post and merge one commit; on a lost race discard, catch up and rebuild."""
        for attempt in range(MAX_RETRIES):
            if not activation:
                self._sync(gid)
            if not self.group_rec(gid).get("in_group"):
                return None
            prepared = plan()
            if not prepared:
                return None
            bundle, adds, removes, revision = prepared["bundle"], prepared["adds"], prepared["removes"], prepared["revision"]
            self.persist()  # the pending commit is part of the state
            try:
                res = self.api("POST", f"/v1/groups/{quote(gid)}/commits", {
                    "request_key": new_request_key(), "epoch": bundle.epoch, "commit_id": bundle.commit_id,
                    "commit": b64(bundle.commit), "envelope_signature": b64(bundle.envelope_signature),
                    "welcome": b64(bundle.welcome) if bundle.welcome is not None else None,
                    "adds": adds, "removes": removes, "expected_revision": revision,
                }, device=True)
            except Exception as exc:
                try:
                    self.device.discard_commit(gid)
                except Exception:
                    pass
                self.persist()
                if status_of(exc) == 409 and code_of(exc) in ("revision_conflict", "epoch_conflict"):
                    self.sleep(min(10.0, 0.2 * 2 ** attempt) + random.random() * 0.1)
                    continue
                raise
            # Anything sequenced before this commit is still in the old epoch: read it before merging.
            if not activation:
                self._sync(gid, res["seq"] - 1)
            o = self.device.confirm_commit(gid)
            g = self.group_rec(gid)
            g["cursor"] = max(g["cursor"], res["seq"])
            g["updated_at"] = self.now()
            self.remember(gid, {"kind": "commit", "seq": res["seq"], "epoch": o.epoch, "committer": member_json(o.committer),
                                "added": [member_json(m) for m in o.added], "removed": [member_json(m) for m in o.removed],
                                "updated": [member_json(m) for m in o.updated], "created_at": self.now()})
            self.persist()
            self.ack_epoch(gid)
            self.emit({"type": "history", "group_id": gid})
            return res
        raise GroupError(409, "revision_conflict")

    def _crypto(self, gid):
        return self.api("GET", f"/v1/groups/{quote(gid)}/crypto", device=True)

    def _claim(self, gid, mailboxes):
        return self.api("POST", f"/v1/groups/{quote(gid)}/key-packages/claim",
                        {"request_key": new_request_key(), "mailboxes": mailboxes}, device=True)

    def _device_ref(self, mailbox, device_id):
        return self.gc.DeviceRef(mailbox=mailbox, device_id=device_id)

    def process_pending(self, gid, no_jitter=False):
        """Commit whatever the postbox says is pending: adds for keying members and new devices,
        removals for departed members and revoked devices."""
        crypto = self._crypto(gid)
        if not crypto.get("in_group") and crypto.get("activation_state") == "active":
            return None
        if not crypto.get("pending"):
            return None
        if not no_jitter:
            self.sleep(self.jitter())
            crypto = self._crypto(gid)
            if not crypto.get("pending"):
                return None

        def plan():
            fresh = self._crypto(gid)
            pending = fresh.get("pending") or []
            if not pending:
                return None
            adds = [p for p in pending if p.get("kind") == "add"]
            removes = [{"mailbox": p["mailbox"], "device_id": p["device_id"]} for p in pending if p.get("kind") == "remove"]
            claims = []
            if adds:
                claimed = self._claim(gid, sorted({p["mailbox"] for p in adds}))
                wanted = {p["mailbox"] + "|" + p["device_id"] for p in adds}
                # Only devices the postbox still lists as pending; never this device itself.
                claims = [c for c in claimed.get("claims") or [] if c["mailbox"] + "|" + c["device_id"] in wanted
                          and not (c["mailbox"] == self.mailbox and c["device_id"] == self.record["device_id"])]
            if not claims and not removes:
                return None
            bundle = self.device.commit(gid, [unb64(c["key_package"]) for c in claims],
                                        [self._device_ref(r["mailbox"], r["device_id"]) for r in removes])
            return {"bundle": bundle, "revision": fresh.get("revision"), "removes": removes,
                    "adds": [{"mailbox": c["mailbox"], "device_id": c["device_id"], "key_package_ref": c["key_package_ref"]} for c in claims]}

        with self.lock:
            return self._commit(gid, plan)

    def self_update(self, gid):
        def plan():
            fresh = self._crypto(gid)
            return {"bundle": self.device.self_update(gid), "adds": [], "removes": [], "revision": fresh.get("revision")}
        with self.lock:
            return self._commit(gid, plan)

    def maintain(self, gid):
        """Weekly self-update so an idle device still refreshes its leaf (P-2 §13.7)."""
        with self.lock:
            g = self.group_rec(gid)
            if not g.get("in_group") or self.now() - (g.get("updated_at") or 0) < SELF_UPDATE_AFTER:
                return None
        return self.self_update(gid)

    # -- creating --------------------------------------------------------------------------------

    def create_group(self, settings):
        """Create the row, create the MLS group at epoch 0 and post the first commit, which
        activates it. The mailbox's other devices, if any, are added in that same commit."""
        created = self._api("POST", "/v1/groups", body={"request_key": new_request_key(), **settings})
        self.activate(created["group"]["id"])
        return created

    def activate(self, gid):
        with self.lock:
            if gid not in self.device.groups():
                self.device.create_group(gid)
                g = self.group_rec(gid)
                g["in_group"] = True
                g["updated_at"] = self.now()
                self.persist()

            def plan():
                fresh = self._crypto(gid)
                if fresh.get("activation_state") == "active" and self.device.epoch(gid) > 0:
                    return None
                adds = [p for p in fresh.get("pending") or [] if p.get("kind") == "add"
                        and not (p["mailbox"] == self.mailbox and p["device_id"] == self.record["device_id"])]
                claims = []
                if adds:
                    claimed = self._claim(gid, sorted({a["mailbox"] for a in adds}))
                    claims = [c for c in claimed.get("claims") or []
                              if not (c["mailbox"] == self.mailbox and c["device_id"] == self.record["device_id"])]
                bundle = (self.device.commit(gid, [unb64(c["key_package"]) for c in claims], [])
                          if claims else self.device.self_update(gid))
                return {"bundle": bundle, "removes": [], "revision": fresh.get("revision"),
                        "adds": [{"mailbox": c["mailbox"], "device_id": c["device_id"], "key_package_ref": c["key_package_ref"]} for c in claims]}

            return self._commit(gid, plan, activation=True)

    # -- sending ---------------------------------------------------------------------------------

    def upload_files(self, gid, files):
        out, refs = [], []
        for f in files or []:
            sealed = self.gc.seal_attachment(bytes(f["bytes"]))
            res = self.api("POST", f"/v1/groups/{quote(gid)}/attachments", raw=bytes(sealed.sealed), device=True)
            out.append({"id": res["id"], "name": f.get("name") or "file", "mime": f.get("mime") or "application/octet-stream",
                        "size": len(f["bytes"])})
            refs.append(sealed.reference)
        return out, refs

    def send(self, gid, doc, cmid=None):
        """Encrypt and post one message. `doc`: text, files (new uploads with bytes), retained
        (already uploaded files of the edited version, with their keys), reply_to, edit_of.

        The exact request is kept in the sealed record before it is posted. An ambiguous failure
        keeps it there and raises UncertainSend: only retry_outbox() posts it again, with the same
        bytes. Only an explicit epoch/key-activation refusal re-encrypts, under the same id."""
        cmid = cmid or new_client_message_id()
        files, refs = self.upload_files(gid, doc.get("files"))
        for kept in doc.get("retained") or []:
            ref = kept.get("ref") or {}
            files.append({"id": str(kept["id"]), "name": kept.get("name") or "file", "mime": kept.get("mime") or "application/octet-stream",
                          "size": int(kept.get("size") or 0)})
            refs.append(self.gc.AttachmentRef(key=unb64(ref.get("key")), sealed_sha256=unb64(ref.get("sha")),
                                              plaintext_len=int(ref.get("len") or 0)))
        plain = {"text": doc.get("text") or "", "files": files,
                 "reply_to": str(doc["reply_to"]) if doc.get("reply_to") is not None else None,
                 "edit_of": str(doc["edit_of"]) if doc.get("edit_of") is not None else None}
        local_files = [{**f, "ref": ref_json(refs[i])} for i, f in enumerate(files)]
        with self.lock:
            self._outbox_room(cmid)
            local = self._local_pending(gid, cmid)
            remembered = local is not None
            if not remembered:
                local = {"kind": "message", "seq": None, "pending": True, "sender": self.mailbox, "sender_device": self.record["device_id"],
                         "cmid": cmid, "text": plain["text"], "files": local_files, "reply_to": plain["reply_to"],
                         "edit_of": plain["edit_of"], "edit_verified": plain["edit_of"] is not None,
                         "created_at": self.now(), "mine": True}
            return self._post_loop(gid, cmid, plain, local, remembered, refs, doc.get("mentions"))

    def _local_pending(self, gid, cmid):
        return next((x for x in self.history_of(gid) if x.get("kind") == "message" and x.get("cmid") == cmid and x.get("pending")), None)

    def _post_loop(self, gid, cmid, plain, local, remembered, refs, mentions=None, retry=False):
        body = encode_body(plain)
        try:
            for attempt in range(MAX_RETRIES):
                self._sync(gid)
                if not self.group_rec(gid).get("in_group"):
                    raise GroupError(403, "not_member")
                out = self.device.encrypt_message(gid, cmid, body, refs)
                if not remembered:
                    self.history_of(gid).append(local)
                    remembered = True
                payload = {"request_key": "rk_" + cmid, "client_message_id": cmid, "epoch": out.epoch, "enc_scheme": ENC_SCHEME,
                           "ciphertext": b64(out.ciphertext), "envelope_signature": b64(out.envelope_signature),
                           "attachments": [f["id"] for f in plain["files"]]}
                if plain.get("reply_to") is not None:
                    payload["reply_to"] = int(plain["reply_to"])
                if plain.get("edit_of") is not None:
                    payload["edit_of"] = int(plain["edit_of"])
                if mentions:
                    payload["mentions"] = list(mentions)
                # The sending ratchet moved and the exact request is kept, in one sealed write,
                # before the postbox hears of it.
                self._outbox_put({"cmid": cmid, "gid": gid, "payload": payload, "doc": plain,
                                  "refs": [ref_json(r) for r in refs], "state": "posting",
                                  "created_at": (self._outbox_get(cmid) or {}).get("created_at") or self.now()})
                self.persist()
                try:
                    res = self.api("POST", f"/v1/groups/{quote(gid)}/messages", payload, device=True)
                except Exception as exc:
                    if status_of(exc) == 409 and code_of(exc) == "epoch_conflict":
                        self.sleep(0.05 * (attempt + 1))
                        continue
                    if status_of(exc) == 409 and code_of(exc) == "key_activation_pending":
                        # A removal is waiting for a commit. Any member device may make it; this one does.
                        self._commit_pending_removals(gid)
                        continue
                    raise
                self._confirmed(gid, cmid, local, res)
                return res
            raise GroupError(409, "epoch_conflict")
        except Exception as exc:
            if ambiguous(exc) and self._outbox_get(cmid):
                entry = self._outbox_get(cmid)
                entry["state"] = "uncertain"
                local.update(pending=True, uncertain=True)
                try:
                    self.persist()
                except Exception:
                    pass
                self.emit({"type": "history", "group_id": gid})
                raise UncertainSend(cmid) from None
            entry = self._outbox_get(cmid)
            if retry and entry:
                # A retried send refused for good stays listed (dismiss only); it is never
                # silently turned into something else.
                entry["state"], entry["refused"] = "refused", code_of(exc) or "refused"
                try:
                    self.persist()
                except Exception:
                    pass
                raise
            # A definite refusal: nothing was accepted; the draft goes back to the person.
            self._outbox_drop(cmid)
            if remembered and local in self.history_of(gid) and local.get("pending"):
                self.history_of(gid).remove(local)
            try:
                self.persist()
            except Exception:
                pass
            raise

    def _confirmed(self, gid, cmid, local, res):
        local.update(seq=res["seq"], pending=False, uncertain=False, expires_at=res.get("expires_at"))
        self.history_of(gid).sort(key=sort_key)
        if local.get("edit_of") is not None:
            target = self._find(gid, _int(local["edit_of"]))
            if target and target.get("sender") == self.mailbox:
                target["replaced_by"] = res["seq"]
        self._outbox_drop(cmid)
        self.persist()
        self.emit({"type": "history", "group_id": gid})

    # -- the protected outbox --------------------------------------------------------------------

    def _outbox(self):
        return self.record.setdefault("outbox", [])

    def _outbox_get(self, cmid):
        return next((e for e in self._outbox() if e.get("cmid") == cmid), None)

    def _outbox_put(self, entry):
        box = self._outbox()
        for i, e in enumerate(box):
            if e.get("cmid") == entry["cmid"]:
                box[i] = entry
                return
        box.append(entry)

    def _outbox_drop(self, cmid):
        if self.record is not None:
            self.record["outbox"] = [e for e in self._outbox() if e.get("cmid") != cmid]

    def _outbox_room(self, cmid):
        others = [e for e in self._outbox() if e.get("cmid") != cmid]
        size = sum(len(json.dumps(e, separators=(",", ":"))) for e in others)
        if len(others) >= OUTBOX_LIMIT or size >= OUTBOX_BYTES:
            raise GroupError(0, "outbox_full")

    def outbox(self, gid=None):
        """Unconfirmed sends kept for review: [{cmid, gid, text, created_at, state, retryable, edit_of}]."""
        with self.lock:
            if not self.record:
                return []
            out = []
            for e in self._outbox():
                if gid is not None and e.get("gid") != gid:
                    continue
                out.append({"cmid": e["cmid"], "gid": e.get("gid"), "text": (e.get("doc") or {}).get("text") or "",
                            "created_at": e.get("created_at"), "state": e.get("state"), "edit_of": (e.get("doc") or {}).get("edit_of"),
                            "refused": e.get("refused"),
                            "retryable": e.get("state") != "refused" and self.now() - (e.get("created_at") or 0) <= OUTBOX_RETRY_SECONDS})
            return out

    def recover_outbox(self):
        """After a restart, a send that was being posted is unconfirmed: shown, never replayed."""
        with self.lock:
            changed = False
            for e in self._outbox():
                if e.get("state") != "uncertain":
                    e["state"] = "uncertain"
                    changed = True
                local = self._local_pending(e.get("gid"), e.get("cmid"))
                if local is not None and not local.get("uncertain"):
                    local["uncertain"] = True
                    changed = True
            if changed:
                self.persist()

    def retry_outbox(self, cmid):
        """Explicit retry of an unconfirmed send: first the exact saved bytes (the postbox answers a
        duplicate with the original), and only after an explicit epoch/key-activation refusal a
        re-encryption under the same client message id."""
        with self.lock:
            entry = self._outbox_get(cmid)
            if not entry:
                raise GroupError(404, "not_found")
            if self.now() - (entry.get("created_at") or 0) > OUTBOX_RETRY_SECONDS:
                raise GroupError(0, "outbox_expired")
            gid = entry["gid"]
            local = self._local_pending(gid, cmid)
            remembered = local is not None
            if not remembered:
                doc = entry.get("doc") or {}
                local = {"kind": "message", "seq": None, "pending": True, "uncertain": True, "sender": self.mailbox,
                         "sender_device": self.record["device_id"], "cmid": cmid, "text": doc.get("text") or "",
                         "files": [{**f, "ref": r} for f, r in zip(doc.get("files") or [], entry.get("refs") or [])],
                         "reply_to": doc.get("reply_to"), "edit_of": doc.get("edit_of"), "edit_verified": doc.get("edit_of") is not None,
                         "created_at": entry.get("created_at"), "mine": True}
            try:
                res = self.api("POST", f"/v1/groups/{quote(gid)}/messages", entry["payload"], device=True)
            except Exception as exc:
                if status_of(exc) == 409 and code_of(exc) in ("epoch_conflict", "key_activation_pending"):
                    if code_of(exc) == "key_activation_pending":
                        self._commit_pending_removals(gid)
                    refs = [self.gc.AttachmentRef(key=unb64(r.get("key")), sealed_sha256=unb64(r.get("sha")),
                                                  plaintext_len=int(r.get("len") or 0)) for r in entry.get("refs") or []]
                    return self._post_loop(gid, cmid, entry["doc"], local, remembered, refs, entry["payload"].get("mentions"), retry=True)
                if ambiguous(exc):
                    raise UncertainSend(cmid) from None
                # Refused for good (not a member any more, edit window closed…): keep it listed so
                # the person decides; they can only dismiss it now.
                entry["state"] = "refused"
                entry["refused"] = code_of(exc) or "refused"
                self.persist()
                raise
            if not remembered:
                self.history_of(gid).append(local)
            self._confirmed(gid, cmid, local, res)
            return res

    def dismiss_outbox(self, cmid):
        """Forget an unconfirmed send on this device. A copy the postbox did accept stays there."""
        with self.lock:
            entry = self._outbox_get(cmid)
            if not entry:
                return False
            self._outbox_drop(cmid)
            local = self._local_pending(entry.get("gid"), cmid)
            if local is not None:
                self.history_of(entry["gid"]).remove(local)
            self.persist()
            return True

    # -- edits, deletion and versions ------------------------------------------------------------

    def editable(self, gid, seq):
        """This mailbox's current version of a live chain, within 24 h of its original."""
        with self.lock:
            item = self._find(gid, seq)
            if (not item or item.get("sender") != self.mailbox or item.get("pending") or item.get("deleted")
                    or item.get("missing") or item.get("replaced_by") is not None or not isinstance(item.get("seq"), int)):
                return False
            chain = self._chain(gid, item)
            if not chain or chain[-1] is not item:
                return False
            root = chain[0]
            now = self.now()
            if root.get("missing") or not root.get("created_at") or now - root["created_at"] > EDIT_WINDOW:
                return False
            if any(v.get("expires_at") and v["expires_at"] <= now for v in chain):
                return False
            return not any(e.get("doc", {}).get("edit_of") == str(seq) for e in self._outbox())

    def edit(self, gid, seq, text):
        """Replace own message `seq` (its current version) with `text`; its files are kept."""
        if not (text or "").strip():
            raise GroupError(400, "empty_edit")
        with self.lock:
            if not self.editable(gid, seq):
                raise GroupError(403, "not_editable")
            target = self._find(gid, seq)
            retained = [f for f in target.get("files") or [] if f.get("id") and f.get("ref")]
            doc = {"text": text, "retained": retained, "edit_of": seq, "reply_to": target.get("reply_to")}
        return self.send(gid, doc)

    def _erase_chain(self, gid, seq):
        """Erase every local version of `seq`'s chain and any queued edit of them."""
        item = self._find(gid, seq)
        seqs = {seq}
        if item:
            seqs |= {v.get("seq") for v in self._chain(gid, item)}
        self._drop_local(gid, seqs)
        return seqs

    def _drop_local(self, gid, seqs):
        wanted = {int(s) for s in seqs if s is not None}
        self.record.setdefault("history", {})[gid] = [x for x in self.history_of(gid)
                                                      if not (x.get("kind") == "message" and x.get("seq") in wanted)]
        for e in list(self._outbox()):
            if e.get("gid") == gid and _int((e.get("doc") or {}).get("edit_of")) in wanted:
                self.dismiss_outbox_locked(e)

    def dismiss_outbox_locked(self, entry):
        self._outbox_drop(entry["cmid"])
        local = self._local_pending(entry.get("gid"), entry["cmid"])
        if local is not None:
            self.history_of(entry["gid"]).remove(local)

    def delete_message(self, gid, seq):
        """Delete a version for everyone: the postbox erases the whole chain and answers with its
        seqs; every local version, cached file and queued edit of them goes too."""
        res = self.api("DELETE", f"/v1/groups/{quote(gid)}/messages/{int(seq)}")
        with self.lock:
            seqs = set(_int(s) for s in (res or {}).get("seqs") or [] if _int(s) is not None) | {int(seq)}
            for s in list(seqs):
                seqs |= self._erase_chain(gid, s)
            self.persist()
        self.emit({"type": "history", "group_id": gid})
        return sorted(s for s in seqs if s is not None)

    def apply_deleted(self, gid, seqs):
        """A `group.message {deleted, seqs}` event (or reconciliation): erase those chains."""
        with self.lock:
            for s in seqs or []:
                if _int(s) is not None:
                    self._erase_chain(gid, _int(s))
            self.persist()

    def reconcile(self, gid):
        """Metadata-only pages over the retained range: versions deleted elsewhere (or gone) are
        erased here. Never used for decryption, and never advances delivered receipts."""
        with self.lock:
            held = sorted(x["seq"] for x in self.history_of(gid) if x.get("kind") == "message" and isinstance(x.get("seq"), int))
            if not held or not self.group_rec(gid).get("in_group"):
                return 0
            after, top = held[0] - 1, held[-1]
        listed, deleted = set(), set()
        for _ in range(RECONCILE_PAGES):
            res = self.api("GET", f"/v1/groups/{quote(gid)}/messages?after_seq={after}&limit=100&metadata_only=true", device=True)
            items = sorted(res.get("items") or [], key=lambda i: i.get("seq", 0))
            for i in items:
                if i.get("type") == "application":
                    listed.add(i["seq"])
                    if i.get("deleted_at"):
                        deleted.add(i["seq"])
            if not items or not res.get("more") or items[-1]["seq"] >= top:
                break
            after = items[-1]["seq"]
        else:
            return 0  # did not reach the end: decide nothing on a partial view
        with self.lock:
            gone = {s for s in held if s in deleted or (s <= top and s not in listed)}
            before = len(self.history_of(gid))
            for s in gone:
                self._erase_chain(gid, s)
            erased = before - len(self.history_of(gid))
            if erased:
                self.persist()
        if erased:
            self.emit({"type": "history", "group_id": gid})
        return erased

    def versions(self, gid, seq):
        """Message info: the versions this device received, oldest first, and whether earlier
        history is missing here."""
        with self.lock:
            item = self._find(gid, seq)
            if not item:
                return {"versions": [], "missing": True}
            chain = self._chain(gid, item)
            missing = any(v.get("missing") or v.get("history_missing") for v in chain)
            return {"versions": [{"seq": v.get("seq"), "text": v.get("text"), "created_at": v.get("created_at")}
                                 for v in chain if not v.get("missing")], "missing": missing}

    def current(self, gid, seq):
        """The visible (latest) version of `seq`'s chain."""
        with self.lock:
            item = self._find(gid, seq)
            chain = self._chain(gid, item) if item else []
            return dict(chain[-1]) if chain else (dict(item) if item else None)

    def receipts(self, gid):
        """GET /v1/groups/{id}/receipts (feature `receipts`)."""
        return self.api("GET", f"/v1/groups/{quote(gid)}/receipts")

    def _commit_pending_removals(self, gid):
        def plan():
            fresh = self._crypto(gid)
            removes = [{"mailbox": p["mailbox"], "device_id": p["device_id"]} for p in fresh.get("pending") or [] if p.get("kind") == "remove"]
            if not removes:
                return None
            return {"bundle": self.device.commit(gid, [], [self._device_ref(r["mailbox"], r["device_id"]) for r in removes]),
                    "adds": [], "removes": removes, "revision": fresh.get("revision")}
        return self._commit(gid, plan)

    def open_file(self, gid, file):
        sealed = self.api("GET", f"/v1/groups/{quote(gid)}/attachments/{quote(file['id'])}", device=True, binary=True)
        ref = file.get("ref") or {}
        reference = self.gc.AttachmentRef(key=unb64(ref.get("key")), sealed_sha256=unb64(ref.get("sha")), plaintext_len=int(ref.get("len") or 0))
        return bytes(self.gc.open_attachment(bytes(sealed), reference))

    # -- local housekeeping ----------------------------------------------------------------------

    def prune_expired(self):
        """Drop expired items by this device's own clock, whether or not the event arrived."""
        with self.lock:
            if not self.record:
                return 0
            t = self.now()
            dropped = 0
            for gid, items in list((self.record.get("history") or {}).items()):
                expired = [x.get("seq") for x in items if x.get("expires_at") and x["expires_at"] <= t and x.get("kind") == "message"]
                before = len(self.history_of(gid))
                for seq in expired:
                    if seq is not None:
                        self._erase_chain(gid, seq)
                kept = [x for x in self.history_of(gid) if not (x.get("expires_at") and x["expires_at"] <= t)]
                self.record["history"][gid] = kept
                dropped += before - len(kept)
            if dropped:
                self.persist()
            return dropped

    def drop_seqs(self, gid, seqs):
        with self.lock:
            self._drop_local(gid, seqs or [])
            self.persist()

    def forget(self, gid):
        with self.lock:
            try:
                self.device.forget_group(gid)
            except Exception:
                pass
            (self.record.get("groups") or {}).pop(gid, None)
            (self.record.get("history") or {}).pop(gid, None)
            self.record["outbox"] = [e for e in self.record.get("outbox") or [] if e.get("gid") != gid]
            self.persist()

    def verify_devices(self, mailbox, fingerprints):
        """Remember the device fingerprints a person compared for `mailbox`."""
        with self.lock:
            self.record.setdefault("verified", {})[mailbox] = sorted(set(fingerprints))
            self.persist()

    def verification(self, mailbox, fingerprints):
        """'verified', 'changed' (devices differ from what was verified) or None (never verified)."""
        with self.lock:
            known = (self.record or {}).get("verified", {}).get(mailbox)
        if known is None:
            return None
        return "verified" if sorted(set(fingerprints)) == known else "changed"

    # -- reading state (no network) --------------------------------------------------------------

    def members(self, gid):
        with self.lock:
            try:
                return [member_json(m) for m in self.device.members(gid)]
            except Exception:
                return []

    def safety_number(self, gid):
        with self.lock:
            try:
                return self.device.safety_number(gid)
            except Exception:
                return None

    def epoch(self, gid):
        with self.lock:
            try:
                return self.device.epoch(gid)
            except Exception:
                return None

    def in_group(self, gid):
        with self.lock:
            return bool(self.record and (self.record.get("groups") or {}).get(gid, {}).get("in_group"))

    def cursor(self, gid):
        with self.lock:
            return (self.record and (self.record.get("groups") or {}).get(gid, {}).get("cursor")) or 0

    def history(self, gid):
        with self.lock:
            return [dict(x) for x in ((self.record or {}).get("history") or {}).get(gid, [])]

    def device_id(self):
        return self.record and self.record.get("device_id")

    def fingerprint(self):
        with self.lock:
            return self.gc.fingerprint_digits(self.device.fingerprint()) if self.device else None

    def fingerprint_digits(self, hex_fingerprint):
        try:
            return self.gc.fingerprint_digits(bytes.fromhex(hex_fingerprint))
        except Exception:
            return str(hex_fingerprint)
