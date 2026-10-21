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
            if item.get("deleted_at") or item.get("ciphertext") is None:
                self.remember(gid, {"kind": "message", "seq": item["seq"], "sender": item.get("sender"), "deleted": True,
                                    "created_at": item.get("created_at"), "expires_at": item.get("expires_at"),
                                    "replaced_by": item.get("replaced_by")})
                return "message"
            if self.is_own(item):
                # Recorded when it was sent; the server copy only confirms its seq.
                for mine in self.history_of(gid):
                    if mine.get("kind") == "message" and mine.get("cmid") == item.get("client_message_id"):
                        mine.update(seq=item["seq"], created_at=item.get("created_at"), expires_at=item.get("expires_at"), pending=False)
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
            edit_of = doc["edit_of"] or (str(item["edit_of"]) if item.get("edit_of") is not None else None)
            self.remember(gid, {"kind": "message", "seq": item["seq"], "sender": item.get("sender"), "sender_device": item.get("sender_device"),
                                "cmid": item.get("client_message_id"), "text": doc["text"], "files": files, "reply_to": doc["reply_to"],
                                "edit_of": edit_of, "created_at": item.get("created_at"), "expires_at": item.get("expires_at")})
            if item.get("edit_of") is not None:
                for target in self.history_of(gid):
                    if target.get("kind") == "message" and target.get("seq") == int(item["edit_of"]):
                        target["replaced_by"] = item["seq"]
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

    def send(self, gid, doc):
        cmid = new_client_message_id()
        files, refs = self.upload_files(gid, doc.get("files"))
        body = encode_body({**doc, "files": files})
        local_files = [{**f, "ref": ref_json(refs[i])} for i, f in enumerate(files)]
        with self.lock:
            local = {"kind": "message", "seq": None, "pending": True, "sender": self.mailbox, "sender_device": self.record["device_id"],
                     "cmid": cmid, "text": doc.get("text") or "", "files": local_files, "reply_to": doc.get("reply_to"),
                     "edit_of": str(doc["edit_of"]) if doc.get("edit_of") is not None else None, "created_at": self.now(), "mine": True}
            remembered = False
            try:
                for attempt in range(MAX_RETRIES):
                    self._sync(gid)
                    if not self.group_rec(gid).get("in_group"):
                        raise GroupError(403, "not_member")
                    out = self.device.encrypt_message(gid, cmid, body, refs)
                    if not remembered:
                        self.history_of(gid).append(local)
                        remembered = True
                    self.persist()  # the sending ratchet moved: never reuse its key after a crash
                    payload = {"request_key": "rk_" + cmid, "client_message_id": cmid, "epoch": out.epoch, "enc_scheme": ENC_SCHEME,
                               "ciphertext": b64(out.ciphertext), "envelope_signature": b64(out.envelope_signature),
                               "attachments": [f["id"] for f in files]}
                    if doc.get("reply_to") is not None:
                        payload["reply_to"] = int(doc["reply_to"])
                    if doc.get("edit_of") is not None:
                        payload["edit_of"] = int(doc["edit_of"])
                    if doc.get("mentions"):
                        payload["mentions"] = list(doc["mentions"])
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
                    local.update(seq=res["seq"], pending=False, expires_at=res.get("expires_at"))
                    self.history_of(gid).sort(key=sort_key)
                    if doc.get("edit_of") is not None:
                        for target in self.history_of(gid):
                            if target.get("kind") == "message" and target.get("seq") == int(doc["edit_of"]):
                                target["replaced_by"] = res["seq"]
                    self.persist()
                    self.emit({"type": "history", "group_id": gid})
                    return res
                raise GroupError(409, "epoch_conflict")
            except Exception:
                if remembered and local in self.history_of(gid) and local.get("pending"):
                    self.history_of(gid).remove(local)
                    try:
                        self.persist()
                    except Exception:
                        pass
                raise

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
                kept = [x for x in items if not (x.get("expires_at") and x["expires_at"] <= t)]
                dropped += len(items) - len(kept)
                self.record["history"][gid] = kept
            if dropped:
                self.persist()
            return dropped

    def drop_seqs(self, gid, seqs):
        wanted = {int(s) for s in seqs or []}
        with self.lock:
            self.record.setdefault("history", {})[gid] = [x for x in self.history_of(gid)
                                                          if not (x.get("kind") == "message" and x.get("seq") in wanted)]
            self.persist()

    def forget(self, gid):
        with self.lock:
            try:
                self.device.forget_group(gid)
            except Exception:
                pass
            (self.record.get("groups") or {}).pop(gid, None)
            (self.record.get("history") or {}).pop(gid, None)
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
