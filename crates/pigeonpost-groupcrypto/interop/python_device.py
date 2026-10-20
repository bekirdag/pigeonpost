#!/usr/bin/env python3
"""Python interop agent: the generated UniFFI Python binding (bindings/python), as the Linux
GTK app uses it. Protocol: interop/run_interop.py. Usage: python_device.py <dir with binding + lib>
"""
import base64
import json
import sys

sys.path.insert(0, sys.argv[1])
import pigeonpost_groupcrypto as gc  # noqa: E402


def b64(b):
    return base64.b64encode(bytes(b)).decode()


def unb64(s):
    return base64.b64decode(s)


def commit_json(b):
    return {"epoch": b.epoch, "commit_id": b.commit_id, "commit": b64(b.commit),
            "welcome": b64(b.welcome) if b.welcome is not None else None,
            "signature": b64(b.envelope_signature)}


def att_json(a):
    return {"key": b64(a.key), "sha": b64(a.sealed_sha256), "len": a.plaintext_len}


def incoming_json(i):
    if i.is_MESSAGE():
        m = i.message
        return {"type": "message", "body": b64(m.body), "sender_mailbox": m.sender.mailbox,
                "sender_device": m.sender.device_id, "client_message_id": m.client_message_id,
                "epoch": m.epoch, "attachments": [att_json(a) for a in m.attachments]}
    if i.is_COMMIT():
        o = i.outcome
        return {"type": "commit", "epoch": o.epoch, "self_removed": o.self_removed,
                "committer_mailbox": o.committer.mailbox, "added": len(o.added), "removed": len(o.removed)}
    if i.is_OWN_COMMIT_CONFIRMED():
        return {"type": "own_commit_confirmed", "epoch": i.outcome.epoch}
    if i.is_HISTORY_OFFER():
        return {"type": "history_offer"}
    return {"type": "own"}


device = None


def handle(r):
    global device
    op, g = r["op"], r.get("group")
    if op == "init":
        device = gc.GroupDevice.new_with_identity_seed(unb64(r["seed"]), r["device"])
        return {"mailbox": device.mailbox(), "signature_key": b64(device.signature_public_key())}
    if op == "key_packages":
        kps = device.generate_key_packages(r["count"]) + [device.generate_last_resort_key_package()]
        return {"key_packages": [b64(k) for k in kps]}
    if op == "create":
        return {"epoch": device.create_group(g).epoch}
    if op == "add":
        return commit_json(device.add_members(g, [unb64(k) for k in r["key_packages"]]))
    if op == "remove_mailbox":
        return commit_json(device.remove_mailbox(g, r["mailbox"]))
    if op == "self_update":
        return commit_json(device.self_update(g))
    if op == "confirm":
        return {"epoch": device.confirm_commit(g).epoch}
    if op == "discard":
        device.discard_commit(g)
        return {}
    if op == "join":
        j = device.join_from_welcome(unb64(r["welcome"]))
        return {"group": j.group_id, "epoch": j.epoch, "members": len(j.members)}
    if op == "encrypt":
        atts, sealed = [], None
        if r.get("attachment"):
            s = gc.seal_attachment(unb64(r["attachment"]))
            sealed = b64(s.sealed)
            atts.append(s.reference)
        m = device.encrypt_message(g, r["id"], unb64(r["body"]), atts)
        return {"ciphertext": b64(m.ciphertext), "signature": b64(m.envelope_signature),
                "epoch": m.epoch, "sealed": sealed}
    if op == "process":
        return incoming_json(device.process_incoming(g, unb64(r["message"])))
    if op == "open_attachment":
        ref = r["ref"]
        a = gc.AttachmentRef(key=unb64(ref["key"]), sealed_sha256=unb64(ref["sha"]), plaintext_len=ref["len"])
        return {"plaintext": b64(gc.open_attachment(unb64(r["sealed"]), a))}
    if op == "status":
        return {"epoch": device.epoch(g), "epoch_authenticator": b64(device.epoch_authenticator(g)),
                "safety_number": device.safety_number(g), "members": len(device.members(g))}
    if op == "persist":
        key = gc.generate_state_key()
        blob = device.export_state(key)
        device = gc.GroupDevice.import_state(blob, key)
        return {"bytes": len(blob)}
    if op == "snapshot":
        key = gc.generate_state_key()
        return {"state": b64(device.export_state(key)), "key": b64(key)}
    if op == "probe_snapshot":
        d = gc.GroupDevice.import_state(unb64(r["state"]), unb64(r["key"]))
        try:
            return {"code": "ok", "result": incoming_json(d.process_incoming(g, unb64(r["message"])))}
        except gc.GroupCryptoError.Failure as e:
            return {"code": e.code}
    raise gc.GroupCryptoError.Failure("invalid_input", f"unknown op {op}")


for line in sys.stdin:
    req = json.loads(line)
    if req["op"] == "quit":
        break
    try:
        resp = {"ok": True, **handle(req)}
    except gc.GroupCryptoError.Failure as e:
        resp = {"ok": False, "code": e.code, "message": e.detail}
    sys.stdout.write(json.dumps(resp) + "\n")
    sys.stdout.flush()
