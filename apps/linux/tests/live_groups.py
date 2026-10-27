"""Linux group message actions against a real loopback postbox (tests/e2e-messages/linux.sh groups).

Three Linux group devices (groups_core + groupstore + api.Postbox.scoped, the real MLS library) for
the fixture's alice, bob and carol: encrypted replies and edits with the reference checked inside
the body, retained attachment keys on a text edit, versions and missing history, forged and
cross-sender edits refused, member receipts and the read-receipts privacy setting, committed
response loss kept across a restart and recovered only by an explicit exact retry, a request that
never arrived, and whole-chain deletion reconciled from metadata-only pages. Synthetic only.
"""
import tempfile

from live_support import Checks, attempts, fixture, loss, postbox
from pigeonpost import groupcrypto
from pigeonpost.actions import group_tick, group_tick_names
from pigeonpost.groups_core import ENC_SCHEME, GroupClient, UncertainSend, b64, encode_body
from pigeonpost.groupstore import GroupStorage, MemoryKeyring


def main():
    gc = groupcrypto.load()
    if gc is None:
        raise SystemExit("group crypto library missing: " + str(groupcrypto.reason()))
    setup = fixture()
    check = Checks("Linux group actions (live)")
    people = setup["people"]
    address = {name: people[name]["address"] for name in ("alice", "bob", "carol")}
    apis = {"alice": postbox(setup, "alice", via_proxy=True)[0], "bob": postbox(setup, "bob")[0], "carol": postbox(setup, "carol")[0]}
    folders = {name: tempfile.mkdtemp(prefix=f"pp-live-group-{name}-") for name in apis}
    keyrings = {name: MemoryKeyring() for name in apis}

    def scoped(name):
        def call(method, path, body=None, device=None, raw=None, binary=False):
            return apis[name].scoped(method, path, address[name], body, device=device, raw=raw, binary=binary)
        return call

    def client(name):
        c = GroupClient(gc, scoped(name), address[name], GroupStorage(gc, keyring=keyrings[name], directory=folders[name]),
                        sleep=lambda s: None, jitter=lambda: 0)
        c.init()
        return c

    alice, bob, carol = client("alice"), client("bob"), client("carol")
    for c in (alice, bob, carol):
        c.top_up_key_packages()
    features = apis["alice"].capabilities()
    for flag in ("groups", "message_edit", "message_reply", "receipts"):
        check.ok(flag in features, f"capability {flag} is on in the fixture")

    gid = alice.create_group({"name": "Linux actions", "invite": [address["bob"], address["carol"]]})["group"]["id"]
    for name in ("bob", "carol"):
        apis[name].scoped("POST", f"/v1/groups/{gid}/invitation", address[name], {"accept": True})
    alice.process_pending(gid, no_jitter=True)
    check.ok(bob.process_welcomes() == [gid] and carol.process_welcomes() == [gid], "both members joined through verified Welcomes")

    def seq_of(c, text):
        return next(x["seq"] for x in c.history(gid) if x.get("kind") == "message" and x.get("text") == text)

    def visible(c):
        return [x["text"] for x in c.history(gid) if x.get("kind") == "message" and x.get("replaced_by") is None
                and not x.get("deleted") and not x.get("missing") and not x.get("undecryptable")]

    # Replies: the parent seq travels on the envelope and inside the encryption.
    alice.send(gid, {"text": "Agenda?", "files": [{"name": "agenda.txt", "mime": "text/plain", "bytes": b"1. ship"}]})
    bob.sync(gid)
    parent = seq_of(bob, "Agenda?")
    bob.send(gid, {"text": "Looks good", "reply_to": parent})
    carol.sync(gid)
    reply = next(x for x in carol.history(gid) if x.get("text") == "Looks good")
    check.ok(reply["reply_to"] == str(parent), "a verified reply names its parent")

    # Edits: the current version, files retained without a new upload, versions on this device.
    first = seq_of(alice, "Agenda?")
    alice.edit(gid, first, "Agenda (v2)")
    alice.edit(gid, seq_of(alice, "Agenda (v2)"), "Agenda (v3)")
    bob.sync(gid)
    carol.sync(gid)
    check.ok(visible(bob) == ["Agenda (v3)", "Looks good"], "the edit chain folds into its latest version")
    latest = bob.current(gid, parent)
    check.ok(bob.open_file(gid, latest["files"][0]) == b"1. ship", "a text edit keeps the original file's key")
    info = bob.versions(gid, parent)
    check.ok([v["text"] for v in info["versions"]] == ["Agenda?", "Agenda (v3)"] and info["missing"],
             "versions this device never received are reported missing, not invented")
    check.ok(not alice.editable(gid, first), "an older version is not editable")
    check.ok(not bob.editable(gid, seq_of(bob, "Agenda (v3)")), "only the sender may edit")

    # A body that claims to edit someone else's message is never folded.
    with carol.lock:
        carol.sync(gid)
        out = carol.device.encrypt_message(gid, "forged-live", encode_body({"text": "Carol's rewrite", "edit_of": str(first)}), [])
        carol.persist()
        carol.api("POST", f"/v1/groups/{gid}/messages", {"request_key": "rk_forged-live", "client_message_id": "forged-live",
                  "epoch": out.epoch, "enc_scheme": ENC_SCHEME, "ciphertext": b64(out.ciphertext),
                  "envelope_signature": b64(out.envelope_signature), "attachments": []}, device=True)
    bob.sync(gid)
    check.ok("Agenda (v3)" in visible(bob) and "Carol's rewrite" in visible(bob), "a forged edit stays an ordinary message")
    check.ok(any(x.get("warning") == "edit_unverified" for x in bob.history(gid)), "and is flagged")
    try:
        carol.edit(gid, seq_of(carol, "Agenda (v3)"), "nope")
        check.ok(False, "cross-sender edit refused")
    except Exception as error:
        check.ok(getattr(error, "code", None) == "not_editable", "a cross-sender edit is refused locally")

    # Receipts: delivered when listed, seen from the read cursor, hidden when a member opts out.
    alice.send(gid, {"text": "Tick check"})
    tick = seq_of(alice, "Tick check")
    bob.sync(gid)
    carol.sync(gid)
    check.ok(group_tick(tick, alice.receipts(gid))[0] == "delivered", "delivered once every member listed it")
    for name in ("bob", "carol"):
        apis[name].scoped("POST", f"/v1/groups/{gid}/read", address[name], {"seq": tick})
    receipts = alice.receipts(gid)
    check.ok(group_tick(tick, receipts)[0] == "seen", "seen (green) once every visible member read it")
    seen, _, _ = group_tick_names(tick, receipts, lambda a: a)
    check.ok(sorted(seen) == sorted([address["bob"], address["carol"]]), "message info names who saw it")
    apis["carol"].call("PUT", "/v1/policy", address["carol"], data={"read_receipts": False})
    receipts = alice.receipts(gid)
    hidden = next(m for m in receipts["members"] if m["address"] == address["carol"])
    check.ok(hidden["seen_seq"] is None, "a member who turned read receipts off shares no read cursor")

    # Committed, answer lost: kept sealed across a restart, recovered only by an explicit exact retry.
    loss(setup, "after")
    try:
        alice.send(gid, {"text": "Lost answer"})
        check.ok(False, "the lost answer raised")
    except UncertainSend:
        check.ok(True, "a lost answer is unconfirmed, not failed")
    loss(setup, "off")
    first_try = attempts(setup)
    check.ok(len(first_try) == 1 and first_try[0]["status"] == 201, "the postbox committed the first attempt once")
    alice = client("alice")  # restart over the same sealed record
    [entry] = alice.outbox(gid)
    check.ok(entry["state"] == "uncertain" and entry["text"] == "Lost answer", "after a restart it is listed, unconfirmed")
    check.ok(len(attempts(setup)) == 1, "nothing was replayed by the restart")
    result = alice.retry_outbox(entry["cmid"])
    retried = attempts(setup)
    check.ok(len(retried) == 2 and retried[1]["sha"] == retried[0]["sha"] and retried[1]["cmid"] == retried[0]["cmid"],
             "the explicit retry sent the exact same bytes")
    check.ok(result["seq"] == seq_of(alice, "Lost answer") and alice.outbox(gid) == [], "the duplicate answer confirms the original")
    bob.sync(gid)
    check.ok(visible(bob).count("Lost answer") == 1, "members see it once")

    # Never arrived: the retry posts it.
    loss(setup, "before")
    try:
        alice.send(gid, {"text": "Never arrived"})
    except UncertainSend:
        pass
    loss(setup, "off")
    [entry] = alice.outbox(gid)
    alice.retry_outbox(entry["cmid"])
    bob.sync(gid)
    check.ok(visible(bob).count("Never arrived") == 1, "a request that never arrived is posted by the retry")

    # Deleting any version erases the chain here, and on other devices after reconciliation.
    v3 = seq_of(alice, "Agenda (v3)")
    seqs = alice.delete_message(gid, first)
    check.ok(first in seqs and v3 in seqs, "the delete answer names every version")
    check.ok(not any(x.get("text", "").startswith("Agenda") for x in alice.history(gid)), "every local version is erased")
    bob.reconcile(gid)
    carol.reconcile(gid)
    check.ok(not any((x.get("text") or "").startswith("Agenda") for x in bob.history(gid) + carol.history(gid)),
             "metadata-only reconciliation erases the chain on the other devices")
    check.ok("Looks good" in visible(bob), "unrelated messages stay")
    check.done()


if __name__ == "__main__":
    main()
