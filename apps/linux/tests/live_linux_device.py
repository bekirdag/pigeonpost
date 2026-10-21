"""The Linux client's group code (groups_core + groupstore + api.Postbox.scoped) as a JSON-lines
process, driven by tests/live_interop.mjs against a real, locally started postbox. Tests only.

Usage: live_linux_device.py <postbox base url> <member token> <mailbox address>
"""
import base64
import json
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from pigeonpost import groupcrypto  # noqa: E402
from pigeonpost.api import APIError, Postbox, Transport  # noqa: E402
from pigeonpost.groups_core import GroupClient, new_request_key  # noqa: E402
from pigeonpost.groupstore import GroupStorage, MemoryKeyring  # noqa: E402

base, token, mailbox = sys.argv[1:4]


class StaticSession:
    def access_token(self, rejected=None):
        return token


gc = groupcrypto.load()
if gc is None:
    raise SystemExit("group crypto library missing: " + str(groupcrypto.reason()))
api = Postbox(StaticSession(), Transport(allow_loopback_http=True), base=base)
folder = tempfile.mkdtemp(prefix="pp-linux-device-")
keyring = MemoryKeyring()


def scoped(method, path, body=None, device=None, raw=None, binary=False):
    return api.scoped(method, path, mailbox, body, device=device, raw=raw, binary=binary)


client = None


def new_client():
    return GroupClient(gc, scoped, mailbox, GroupStorage(gc, keyring=keyring, directory=folder), sleep=lambda s: None, jitter=lambda: 0)


def handle(r):
    global client
    op, gid = r["op"], r.get("group")
    if op == "init":
        client = new_client()
        info = client.init()
        client.top_up_key_packages()
        return info
    if op == "restart":  # a new process over the same encrypted files and keyring
        client = new_client()
        return client.init()
    if op == "welcomes":
        return {"joined": client.process_welcomes()}
    if op == "sync":
        return client.sync(gid)
    if op == "pending":
        client.process_pending(gid, no_jitter=True)
        return {"epoch": client.epoch(gid)}
    if op == "create":
        created = client.create_group({"name": r["name"], "invite": r.get("invite") or []})
        return {"group": created["group"]["id"], "invitations": created.get("invitations")}
    if op == "send":
        files = [{"name": f["name"], "mime": f["mime"], "bytes": base64.b64decode(f["b64"])} for f in r.get("files") or []]
        res = client.send(gid, {"text": r["text"], "files": files})
        return {"seq": res["seq"], "expires_at": res.get("expires_at")}
    if op == "history":
        return {"items": client.history(gid)}
    if op == "open_file":
        item = next(x for x in client.history(gid) if x.get("kind") == "message" and x.get("files"))
        return {"b64": base64.b64encode(client.open_file(gid, item["files"][0])).decode()}
    if op == "status":
        return {"epoch": client.epoch(gid), "safety_number": client.safety_number(gid), "members": len(client.members(gid)),
                "device": client.device_id(), "in_group": client.in_group(gid)}
    if op == "rest":
        return {"body": api.scoped(r["method"], r["path"], mailbox, r.get("body"))}
    if op == "set_timer":
        group = api.scoped("GET", f"/v1/groups/{gid}", mailbox)
        revision = (group.get("group") or group)["revision"]
        return {"body": api.scoped("PATCH", f"/v1/groups/{gid}", mailbox, {"expected_revision": revision, "ephemeral_secs": r["secs"]})}
    if op == "call":  # the window's own path for Phase 1b routes (identity in query/body)
        out = api.call(r["method"], r["path"], mailbox, r.get("data"), query=r.get("query"), binary=bool(r.get("binary")))
        return {"body": base64.b64encode(out).decode() if r.get("binary") else out}
    if op == "avatar_upload":
        import gi
        gi.require_version("GdkPixbuf", "2.0")
        from gi.repository import GdkPixbuf
        from urllib.parse import urlencode
        pixbuf = GdkPixbuf.Pixbuf.new(GdkPixbuf.Colorspace.RGB, False, 8, 300, 300)
        pixbuf.fill(0x2266aaff)
        ok, data = pixbuf.save_to_bufferv("jpeg", ["quality"], ["90"])
        query = {"identity": mailbox, "crop": "0,0,300,300", "visibility": r.get("visibility", "everyone")}
        out = api.call("PUT", "/v1/profile/avatar?" + urlencode(query), raw=bytes(data), timeout=60,
                       headers={"Content-Type": "image/jpeg", "x-pigeonpost-identity": mailbox})
        return {"body": out}
    if op == "revoke":
        client.revoke()
        return {}
    if op == "request_key":
        return {"key": new_request_key()}
    raise ValueError("unknown op " + op)


for line in sys.stdin:
    request = json.loads(line)
    if request["op"] == "quit":
        break
    try:
        response = {"ok": True, **handle(request)}
    except APIError as exc:
        response = {"ok": False, "status": exc.status, "code": exc.code, "message": str(exc)}
    except Exception as exc:  # reported to the driver
        response = {"ok": False, "code": getattr(exc, "code", type(exc).__name__), "message": str(exc)}
    sys.stdout.write(json.dumps(response) + "\n")
    sys.stdout.flush()
