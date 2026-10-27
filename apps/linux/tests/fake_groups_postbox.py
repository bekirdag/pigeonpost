"""An in-memory postbox implementing the group and MLS rules of docs/groups.md §1–§3.

A Python port of `site-inbox/test/fake-groups-postbox.mjs`, using the real library's server-side
helpers (inspect_key_package, inspect_envelope, verify_envelope_signature, welcome_recipients) for
the checks the real postbox makes with the Rust crate. Small on purpose: it is the contract the
Linux client is tested against, not a second server. Tests only.
"""

import base64
import copy
import hashlib
import itertools
import json
import re
import secrets
import time
import urllib.parse

from pigeonpost.groups_core import GroupError


def b64(data):
    return base64.b64encode(bytes(data)).decode()


def unb64(text):
    return base64.b64decode(str(text))


# ---- Ed25519 public key from a seed (RFC 8032 §5.1.5), for the mailbox key the postbox returns --

_P = 2 ** 255 - 19
_D = -121665 * pow(121666, _P - 2, _P) % _P
_I = pow(2, (_P - 1) // 4, _P)


def _xrecover(y):
    xx = (y * y - 1) * pow(_D * y * y + 1, _P - 2, _P)
    x = pow(xx, (_P + 3) // 8, _P)
    if (x * x - xx) % _P:
        x = x * _I % _P
    return _P - x if x % 2 else x


_BY = 4 * pow(5, _P - 2, _P) % _P
_B = (_xrecover(_BY), _BY, 1, _xrecover(_BY) * _BY % _P)


def _add(p, q):
    x1, y1, z1, t1 = p
    x2, y2, z2, t2 = q
    a = (y1 - x1) * (y2 - x2) % _P
    b = (y1 + x1) * (y2 + x2) % _P
    c = t1 * 2 * _D * t2 % _P
    d = z1 * 2 * z2 % _P
    e, f, g, h = b - a, d - c, d + c, b + a
    return (e * f % _P, g * h % _P, f * g % _P, e * h % _P)


def _mul(s, p):
    q = (0, 1, 1, 0)
    while s:
        if s & 1:
            q = _add(q, p)
        p = _add(p, p)
        s >>= 1
    return q


def ed25519_public(seed):
    h = hashlib.sha512(seed).digest()
    a = int.from_bytes(h[:32], "little")
    a &= (1 << 254) - 8
    a |= 1 << 254
    x, y, z, _ = _mul(a, _B)
    zi = pow(z, _P - 2, _P)
    x, y = x * zi % _P, y * zi % _P
    return (y | ((x & 1) << 255)).to_bytes(32, "little")


def attestation_payload(mailbox, device_id, sig_key):
    m, d = mailbox.encode(), device_id.encode()
    u16 = lambda n: n.to_bytes(2, "big")
    return (b"pigeonpost/groupcrypto/device-attestation/v1\0" + u16(len(m)) + m + u16(len(d)) + d
            + u16(1) + u16(len(sig_key)) + bytes(sig_key))


class FakePostbox:
    def __init__(self, gc):
        self.gc = gc
        self.seeds, self.devices, self.kps, self.groups = {}, {}, {}, {}
        self.welcomes, self.blobs, self.calls, self.errors = [], {}, [], []
        self.before_handle = None
        self.ids = itertools.count(1)
        # Receipt cursors: (gid, address) -> seq; addresses that turned read receipts off.
        self.delivered, self.seen, self.receipts_off = {}, {}, set()
        self.claimed = set()

    def new_id(self, prefix):
        return f"{prefix}_{next(self.ids)}"

    def mailbox(self):
        seed = secrets.token_bytes(32)
        address = self.gc.mailbox_for_seed(seed)
        self.seeds[address] = seed
        self.devices[address] = {}
        return address

    def dev_of(self, me, device_id):
        d = self.devices.get(me, {}).get(device_id)
        if not d:
            raise GroupError(403, "unknown_device")
        if d["revoked"]:
            raise GroupError(403, "device_revoked")
        return d

    def group(self, gid):
        g = self.groups.get(gid)
        if not g:
            raise GroupError(404, "not_found")
        return g

    @staticmethod
    def key(m, d):
        return m + "|" + d

    def pending(self, g):
        out = []
        for m, mem in g["members"].items():
            if mem["state"] not in ("active", "keying"):
                continue
            if g["activation_state"] == "provisioning" and m != g["creator"]:
                continue
            for d, dev in self.devices.get(m, {}).items():
                if dev["revoked"]:
                    continue
                if self.key(m, d) not in g["leaves"]:
                    out.append({"kind": "add", "mailbox": m, "device_id": d, "since": 0})
        for k in sorted(g["leaves"]):
            m, d = k.split("|")
            mem = g["members"].get(m)
            dev = self.devices.get(m, {}).get(d)
            if not mem or mem["state"] not in ("active", "keying") or not dev or dev["revoked"] or k in g["stale"]:
                out.append({"kind": "remove", "mailbox": m, "device_id": d, "since": 0})
        return out

    def push(self, g, item):
        g["last_seq"] += 1
        item["seq"] = g["last_seq"]
        item["created_at"] = int(time.time())
        g["items"].append(item)
        return item

    def versions(self, g, seq):
        """The same-sender chain containing `seq`, oldest first (the server's view)."""
        by_seq = {i["seq"]: i for i in g["items"] if i["type"] == "application"}
        item = by_seq.get(seq)
        if not item:
            return []
        while item.get("edit_of") is not None and by_seq.get(item["edit_of"], {}).get("sender") == item["sender"]:
            item = by_seq[item["edit_of"]]
        chain = [item]
        while item.get("replaced_by") is not None and item["replaced_by"] in by_seq:
            item = by_seq[item["replaced_by"]]
            chain.append(item)
        return chain

    # -- what tests do on the server side --

    def invite(self, gid, address):
        g = self.group(gid)
        g["members"][address] = {"role": "member", "state": "invited", "first_seq": None}
        self.push(g, {"type": "notice", "kind": "member_invited", "metadata": {"address": address}})

    def accept(self, gid, address):
        m = self.group(gid)["members"].get(address)
        if not m or m["state"] != "invited":
            raise GroupError(403, "invite_required")
        m["state"] = "keying"

    def remove(self, gid, address, state="removed"):
        g = self.group(gid)
        g["members"][address]["state"] = state
        g["revision"] += 1
        self.push(g, {"type": "notice", "kind": "member_left" if state == "left" else "member_removed", "metadata": {"address": address}})

    def revoke(self, address, device_id):
        self.devices[address][device_id]["revoked"] = True

    # -- the routes --

    def handle(self, me, method, path, body, device, raw):
        url = urllib.parse.urlsplit(path)
        p, q = url.path, dict(urllib.parse.parse_qsl(url.query))
        body = body or {}
        self.calls.append((me, method, p))
        if self.before_handle:
            self.before_handle(me, method, p, body)
        gc = self.gc

        if p == "/v1/mls/devices" and method == "POST":
            devs = self.devices[me]
            if body["device_id"] in devs:
                raise GroupError(409, "device_exists")
            sig = unb64(body["signature_key"])
            devs[body["device_id"]] = {"sig": sig, "revoked": False}
            att = gc.sign_attestation_with_seed(self.seeds[me], attestation_payload(me, body["device_id"], sig))
            return {"device_id": body["device_id"], "attestation": b64(att), "mailbox_public_key": b64(ed25519_public(self.seeds[me])),
                    "fingerprint_digits": ""}
        m = re.fullmatch(r"/v1/mls/devices/([^/]+)", p)
        if m and method == "DELETE":
            d = self.devices[me].get(urllib.parse.unquote(m.group(1)))
            if not d:
                raise GroupError(404, "not_found")
            d["revoked"] = True
            return {"revoked": True}
        if p == "/v1/mls/key-packages/count":
            self.dev_of(me, device)
            unclaimed, last_resort = 0, False
            for k in self.kps.values():
                if k["mailbox"] == me and k["device_id"] == device:
                    if k["last_resort"]:
                        last_resort = True
                    elif not k["claimed_by"]:
                        unclaimed += 1
            return {"unclaimed": unclaimed, "last_resort": last_resort}
        if p == "/v1/mls/key-packages" and method == "POST":
            d = self.dev_of(me, device)
            packages = [(k, False) for k in body.get("key_packages") or []] + ([(body["last_resort"], True)] if body.get("last_resort") else [])
            for k, lr in packages:
                info = gc.inspect_key_package(unb64(k))
                if info.mailbox != me or info.device_id != device or bytes(info.signature_key) != d["sig"]:
                    raise GroupError(400, "invalid_key_package")
                if lr:
                    for ref in [r for r, v in self.kps.items() if v["mailbox"] == me and v["device_id"] == device and v["last_resort"]]:
                        del self.kps[ref]
                self.kps[bytes(info.key_package_ref).hex()] = {"mailbox": me, "device_id": device, "bytes": k, "last_resort": lr,
                                                              "ref": b64(info.key_package_ref), "claimed_by": None}
            return {"stored": len(packages)}
        if p == "/v1/mls/welcomes":
            self.dev_of(me, device)
            return {"welcomes": [{k: w[k] for k in ("welcome_id", "group_id", "epoch", "seq", "welcome")}
                                 for w in self.welcomes if w["mailbox"] == me and w["device_id"] == device and not w["acked"]]}
        m = re.fullmatch(r"/v1/mls/welcomes/([^/]+)/ack", p)
        if m:
            w = next((x for x in self.welcomes if x["welcome_id"] == urllib.parse.unquote(m.group(1)) and x["mailbox"] == me), None)
            if not w:
                raise GroupError(404, "not_found")
            w["acked"] = True
            if not body.get("joined"):
                self.groups[w["group_id"]]["stale"].add(self.key(me, w["device_id"]))
            return {"ok": True}
        if p == "/v1/groups" and method == "POST":
            gid = "grp_" + secrets.token_hex(6)
            self.groups[gid] = {"id": gid, "name": body.get("name"), "creator": me, "revision": 1, "epoch": 0,
                                "activation_state": "provisioning", "last_seq": 0, "items": [],
                                "members": {me: {"role": "owner", "state": "keying", "first_seq": None}},
                                "leaves": set(), "stale": set(), "claims": {}, "post_policy": body.get("post_policy") or "everyone",
                                "ephemeral": 0}
            return {"group": {"id": gid, "name": body.get("name"), "activation_state": "provisioning", "revision": 1, "epoch": 0}, "invitations": []}
        m = re.fullmatch(r"/v1/groups/([^/]+)(/.*)?", p)
        if not m:
            raise GroupError(404, "not_found")
        g = self.group(urllib.parse.unquote(m.group(1)))
        rest = m.group(2) or ""
        mem = g["members"].get(me)

        if rest == "/crypto":
            self.dev_of(me, device)
            return {"epoch": g["epoch"], "revision": g["revision"], "activation_state": g["activation_state"],
                    "in_group": self.key(me, device) in g["leaves"], "pending": self.pending(g), "lagging_devices": []}
        if rest == "/key-packages/claim":
            self.dev_of(me, device)
            if not mem or not (mem["state"] == "active" or (g["activation_state"] == "provisioning" and me == g["creator"])):
                raise GroupError(403, "not_member")
            claims, exhausted = [], []
            for target in body["mailboxes"]:
                for pnd in [x for x in self.pending(g) if x["kind"] == "add" and x["mailbox"] == target]:
                    pick = next((k for k in self.kps.values() if k["mailbox"] == target and k["device_id"] == pnd["device_id"]
                                 and not k["last_resort"] and not k["claimed_by"]), None)
                    if not pick:
                        pick = next((k for k in self.kps.values() if k["mailbox"] == target and k["device_id"] == pnd["device_id"]
                                     and k["last_resort"]), None)
                    if not pick:
                        exhausted.append({"mailbox": target, "device_id": pnd["device_id"]})
                        continue
                    if not pick["last_resort"]:
                        pick["claimed_by"] = self.key(me, device)
                    g["claims"][pick["ref"]] = {"by": self.key(me, device), "mailbox": target, "device_id": pnd["device_id"]}
                    claims.append({"mailbox": target, "device_id": pnd["device_id"], "key_package": pick["bytes"], "key_package_ref": pick["ref"]})
            return {"claims": claims, "exhausted": exhausted}
        if rest == "/commits":
            d = self.dev_of(me, device)
            creator_activation = g["activation_state"] == "provisioning" and me == g["creator"]
            if not mem or not (mem["state"] == "active" or creator_activation):
                raise GroupError(403, "not_member")
            if body["epoch"] != g["epoch"] or body["expected_revision"] != g["revision"]:
                raise GroupError(409, "revision_conflict", {"current_epoch": g["epoch"], "revision": g["revision"]})
            commit = unb64(body["commit"])
            gc.verify_envelope_signature(d["sig"], commit, unb64(body["envelope_signature"]))
            env = gc.inspect_envelope(commit)
            if (env.kind != "commit" or env.group_id != g["id"] or env.sender_mailbox != me or env.sender_device != device
                    or env.client_message_id != body["commit_id"] or env.epoch != g["epoch"]):
                raise GroupError(400, "invalid_commit")
            pend = self.pending(g)
            for a in body["adds"]:
                if not any(x["kind"] == "add" and x["mailbox"] == a["mailbox"] and x["device_id"] == a["device_id"] for x in pend):
                    raise GroupError(403, "not_pending")
                c = g["claims"].get(a["key_package_ref"])
                if not c or c["by"] != self.key(me, device) or c["mailbox"] != a["mailbox"] or c["device_id"] != a["device_id"]:
                    raise GroupError(403, "unclaimed")
            for r in body["removes"]:
                if not (any(x["kind"] == "remove" and x["mailbox"] == r["mailbox"] and x["device_id"] == r["device_id"] for x in pend)
                        or r["mailbox"] == me):
                    raise GroupError(403, "not_pending")
            if body["adds"]:
                if not body.get("welcome"):
                    raise GroupError(400, "welcome_required")
                recipients = sorted(b64(r) for r in gc.welcome_recipients(unb64(body["welcome"])))
                if recipients != sorted(a["key_package_ref"] for a in body["adds"]):
                    raise GroupError(400, "welcome_mismatch")
            item = self.push(g, {"type": "commit", "epoch": g["epoch"], "sender": me, "sender_device": device, "commit_id": body["commit_id"],
                                 "commit": body["commit"], "adds": [{"mailbox": a["mailbox"], "device_id": a["device_id"]} for a in body["adds"]],
                                 "removes": body["removes"]})
            if creator_activation:
                g["leaves"].add(self.key(me, device))
                g["activation_state"] = "active"
                mem["state"] = "active"
                mem["first_seq"] = item["seq"] + 1
            for r in body["removes"]:
                g["leaves"].discard(self.key(r["mailbox"], r["device_id"]))
                g["stale"].discard(self.key(r["mailbox"], r["device_id"]))
            for a in body["adds"]:
                g["leaves"].add(self.key(a["mailbox"], a["device_id"]))
                am = g["members"][a["mailbox"]]
                if am["state"] == "keying":
                    am["state"] = "active"
                    am["first_seq"] = item["seq"] + 1
                self.welcomes.append({"welcome_id": self.new_id("wel"), "group_id": g["id"], "epoch": g["epoch"] + 1, "seq": item["seq"],
                                      "welcome": body["welcome"], "mailbox": a["mailbox"], "device_id": a["device_id"], "acked": False})
            g["epoch"] += 1
            g["revision"] += 1
            return {"seq": item["seq"], "epoch": g["epoch"], "revision": g["revision"]}
        if rest == "/epoch-ack":
            return {"ok": True}
        if rest == "/resync":
            g["stale"].add(self.key(me, device))
            return {"ok": True}
        if rest == "/messages" and method == "GET":
            self.dev_of(me, device)
            if not mem or mem["state"] != "active":
                raise GroupError(403, "not_member")
            after, limit = int(q.get("after_seq") or 0), min(100, int(q.get("limit") or 100))
            now = int(time.time())
            visible = [i for i in g["items"] if i["seq"] > after and i["seq"] >= (mem["first_seq"] or 0) - 1
                       and not (i.get("expires_at") and i["expires_at"] <= now)]
            page = copy.deepcopy(visible[:limit])
            if q.get("metadata_only") == "true":
                for i in page:
                    if i["type"] == "application":
                        i["ciphertext"] = None
                    elif i["type"] == "commit":
                        i["commit"] = None
            elif page:
                key = (g["id"], me)
                self.delivered[key] = max(self.delivered.get(key, 0), page[-1]["seq"])
            return {"items": page, "last_seq": g["last_seq"], "epoch": g["epoch"], "more": len(visible) > limit}
        m = re.fullmatch(r"/messages/(\d+)", rest)
        if m and method == "DELETE":
            if not mem or mem["state"] != "active":
                raise GroupError(403, "not_member")
            chain = self.versions(g, int(m.group(1)))
            if not chain:
                raise GroupError(404, "not_found")
            seqs = [i["seq"] for i in chain]
            if all(i.get("deleted_at") for i in chain):
                return {"deleted": True, "seqs": seqs}
            root = chain[0]
            if not (root["sender"] == me and time.time() - root["created_at"] <= 24 * 3600) and mem["role"] not in ("owner", "admin"):
                raise GroupError(403, "not_admin")
            for i in chain:
                i["deleted_at"], i["ciphertext"] = int(time.time()), None
                for a in i.get("attachments") or []:
                    self.blobs.pop(a["id"], None)
            return {"deleted": True, "seqs": seqs}
        if rest == "/read" and method == "POST":
            key = (g["id"], me)
            self.seen[key] = max(self.seen.get(key, 0), int(body.get("seq") or 0))
            return {"ok": True}
        if rest == "/receipts" and method == "GET":
            if not mem or mem["state"] != "active":
                raise GroupError(403, "not_member")
            mine = me not in self.receipts_off
            members = []
            for address, other in g["members"].items():
                if address == me or other["state"] != "active":
                    continue
                read = self.seen.get((g["id"], address), 0)
                members.append({"address": address, "handle": None,
                                "delivered_seq": max(self.delivered.get((g["id"], address), 0), read),
                                "seen_seq": read if mine and address not in self.receipts_off else None})
            return {"members": members, "read_receipts": mine}
        if rest == "/messages" and method == "POST":
            d = self.dev_of(me, device)
            if not mem or mem["state"] != "active":
                raise GroupError(403, "not_member")
            if g["post_policy"] == "admins" and mem["role"] not in ("owner", "admin"):
                raise GroupError(403, "posting_restricted")
            dup = next((i for i in g["items"] if i["type"] == "application" and i["sender"] == me
                        and i["client_message_id"] == body["client_message_id"]), None)
            if dup:
                return {"seq": dup["seq"], "id": dup["id"], "expires_at": dup.get("expires_at")}
            if body["epoch"] != g["epoch"]:
                raise GroupError(409, "epoch_conflict", {"current_epoch": g["epoch"]})
            if any(x["kind"] == "remove" for x in self.pending(g)):
                raise GroupError(409, "key_activation_pending")
            if body.get("enc_scheme") != "mls-rfc9420-x25519-aes128gcm-ed25519+ppgc1":
                raise GroupError(400, "encryption_unsupported")
            ct = unb64(body["ciphertext"])
            gc.verify_envelope_signature(d["sig"], ct, unb64(body["envelope_signature"]))
            env = gc.inspect_envelope(ct)
            if (env.kind != "application" or env.group_id != g["id"] or env.sender_mailbox != me or env.sender_device != device
                    or env.client_message_id != body["client_message_id"]):
                raise GroupError(400, "invalid_envelope")
            expires = int(time.time()) + g["ephemeral"] if g["ephemeral"] else None
            target = None
            if body.get("edit_of") is not None:
                chain = self.versions(g, int(body["edit_of"]))
                target = chain[-1] if chain else None
                if (not chain or target["seq"] != int(body["edit_of"]) or chain[0]["sender"] != me or chain[0].get("edit_of") is not None
                        or time.time() - chain[0]["created_at"] > 24 * 3600
                        or any(v.get("deleted_at") or (v.get("expires_at") and v["expires_at"] <= time.time()) for v in chain)):
                    raise GroupError(403, "not_editable")
                expiries = [v["expires_at"] for v in chain if v.get("expires_at")]
                expires = min(expiries) if expiries else None
            kept = {a["id"] for a in (target or {}).get("attachments") or []}
            for a in body.get("attachments") or []:
                if a not in self.blobs or (a in self.claimed and a not in kept):
                    raise GroupError(400, "bad_attachment")
            item = self.push(g, {"type": "application", "id": self.new_id("gm"), "epoch": g["epoch"], "sender": me, "sender_device": device,
                                 "client_message_id": body["client_message_id"], "ciphertext": body["ciphertext"],
                                 "attachments": [{"id": a, "bytes": len(self.blobs.get(a, b""))} for a in body.get("attachments") or []],
                                 "edit_of": body.get("edit_of"), "reply_to": body.get("reply_to"), "replaced_by": None,
                                 "expires_at": expires})
            self.claimed.update(body.get("attachments") or [])
            if target is not None:
                target["replaced_by"], target["ciphertext"] = item["seq"], None
                target["attachments"] = []
            return {"seq": item["seq"], "id": item["id"], "expires_at": item["expires_at"]}
        if rest == "/attachments" and method == "POST":
            self.dev_of(me, device)
            att = self.new_id("gat")
            self.blobs[att] = bytes(raw)
            return {"id": att, "bytes": len(raw)}
        m = re.fullmatch(r"/attachments/(.+)", rest)
        if m and method == "GET":
            if not mem or mem["state"] != "active":
                raise GroupError(403, "not_member")
            blob = self.blobs.get(urllib.parse.unquote(m.group(1)))
            if blob is None:
                raise GroupError(404, "not_found")
            return blob
        raise GroupError(404, f"not_found:{method} {p}")

    def client_api(self, me):
        """The `api` callable a GroupClient acting as `me` uses."""
        def api(method, path, body=None, device=None, raw=None, binary=False):
            try:
                out = self.handle(me, method, path, copy.deepcopy(body), device, raw)
            except GroupError as exc:
                self.errors.append((me, path, exc.code))
                raise
            return out if binary else json.loads(json.dumps(out))
        return api
