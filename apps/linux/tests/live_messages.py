"""Linux direct-message actions against a real loopback postbox (tests/e2e-messages/linux.sh).

Uses the app's own code (api.Postbox, delivery.Coordinator, binding.Lease, protected store) for
alice, bob and carol from the disposable fixture: capability discovery, protected sends with
committed-response loss and explicit exact retry, replies, folded edits and versions, ack with
edits, delivered/seen receipts, the read-receipts setting, and login binding.
"""
import json
import sys
import tempfile
import time

from live_support import Checks, StaticSession, attempts, fixture, loss, postbox
from test_actions import TestSealer
from pigeonpost.actions import tick_state
from pigeonpost.api import APIError
from pigeonpost.binding import Lease, Retired
from pigeonpost.delivery import Coordinator
from pigeonpost.protected import MemoryKeyring, ProtectedStore


def main():
    setup = fixture()
    check = Checks("Linux message actions (live)")
    people = setup["people"]
    alice_addr, bob_addr = people["alice"]["address"], people["bob"]["address"]
    alice_api, alice_session = postbox(setup, "alice", via_proxy=True)
    bob_api, _ = postbox(setup, "bob")
    features = alice_api.capabilities()
    for flag in ("message_edit", "message_reply", "receipts", "recipient_lookup"):
        check.ok(flag in features, f"capability {flag} is on in the fixture")

    folder = tempfile.TemporaryDirectory(prefix="pp-live-protected-")
    store = ProtectedStore(TestSealer(), MemoryKeyring(), folder.name)
    alice_lease = Lease(alice_session).child(alice_addr)
    coordinator = Coordinator(store, alice_api.bound(alice_lease), alice_lease, alice_addr, features)

    def bob_inbox():
        return bob_api.inbox(bob_addr, actions=True)

    def alice_sent(ref):
        rows = postbox(setup, "alice")[0].inbox(alice_addr, actions=True)
        return next((m for m in rows if m.get("msg_ref") == ref and m.get("direction") == "out"), None)

    # 1. A plain protected send; the recipient is frozen to the /k/ inbox before the POST.
    first = coordinator.prepare(bob_addr, "Hello from Linux", peer=bob_addr)
    check.ok(len(coordinator.pending()) == 1, "the request is saved before its first POST")
    result, cleanup = coordinator.send(first)
    check.ok(result.get("msg_ref") == first.msg_ref and not cleanup, "confirmed send returns the same msg_ref")
    check.ok(coordinator.pending() == [], "a confirmed send leaves nothing pending")

    # 2. Committed but the answer is lost: kept as unconfirmed, never replayed by itself.
    loss(setup, "after")
    lost = coordinator.prepare(bob_addr, "Committed, answer lost", peer=bob_addr, thread=None)
    try:
        coordinator.send(lost)
        check.ok(False, "the lost answer raised")
    except APIError as error:
        check.ok(error.status == 0, "the lost answer is uncertain (no HTTP status)")
    pending = coordinator.pending()
    check.ok([p.state for p in pending] == ["uncertain"] and pending[0].wire == lost.wire, "kept as unconfirmed with its exact bytes")
    check.ok(len(attempts(setup)) == 1, "exactly one attempt reached the postbox")
    try:
        coordinator.prepare(bob_addr, "Another", peer=bob_addr)
        check.ok(False, "a second send to the same conversation waits")
    except Exception as error:
        check.ok(type(error).__name__ == "DeliveryConflict", "a second send to the same conversation waits for review")
    loss(setup, "off")
    result, _ = coordinator.retry(pending[0])
    check.ok(result.get("duplicate") is True and result.get("msg_ref") == lost.msg_ref, "explicit retry recovers the original answer")
    check.ok(sum(1 for m in bob_inbox() if m.get("body") == "Committed, answer lost") == 1, "the recipient has exactly one copy")

    # 3. Lost before reaching the postbox: the explicit retry delivers it once.
    loss(setup, "before")
    early = coordinator.prepare(bob_addr, "Lost on the way", peer=bob_addr)
    try:
        coordinator.send(early)
    except APIError:
        pass
    loss(setup, "off")
    saved = coordinator.pending()
    check.ok(len(saved) == 1, "an unanswered send stays saved")
    result, _ = coordinator.retry(saved[0])
    check.ok(not result.get("duplicate"), "the retry was the first delivery")
    check.ok(sum(1 for m in bob_inbox() if m.get("body") == "Lost on the way") == 1, "delivered once")

    # 4. Reply with a quote; the parent's excerpt comes from the recipient's own copy.
    rows = bob_inbox()
    parent = next(m for m in rows if m.get("body") == "Hello from Linux")
    check.ok(parent.get("msg_ref") == first.msg_ref, "both copies share the msg_ref")
    bob_session = StaticSession(people["bob"]["token"])
    bob_lease = Lease(bob_session).child(bob_addr)
    bob_coordinator = Coordinator(store, postbox(setup, "bob", session=bob_session)[0].bound(bob_lease), bob_lease, bob_addr, features)
    reply = bob_coordinator.prepare(alice_addr, "Got it", peer=alice_addr, reply_to=parent["msg_ref"], quote="Hello from Linux")
    bob_coordinator.send(reply)
    mine = postbox(setup, "alice")[0].inbox(alice_addr, actions=True)
    got = next(m for m in mine if m.get("body") == "Got it")
    check.ok(isinstance(got.get("reply_to"), dict) and got["reply_to"].get("msg_ref") == first.msg_ref, "the reply names its parent")
    check.ok(got["reply_to"].get("excerpt") == "Hello from Linux" and got["reply_to"].get("direction") == "out",
             "the parent's excerpt is from the reader's own copy")

    # 5. Edit, folded view, versions, unread revision and ack with edits.
    edit = coordinator.prepare(bob_addr, "Hello from Linux (edited)", peer=bob_addr, edit_of=first.msg_ref)
    coordinator.send(edit)
    folded = [m for m in bob_inbox() if m.get("msg_ref") == first.msg_ref]
    check.ok(len(folded) == 1 and folded[0]["body"] == "Hello from Linux (edited)", "fold=1 shows the latest text on the original")
    check.ok(folded[0].get("edit_count") == 1 and folded[0].get("edited_at"), "the original carries edited_at/edit_count")
    check.ok(folded[0].get("read") is False, "an unread edit makes the original unread")
    info = bob_api.call("GET", f"/v1/messages/{folded[0]['message_id']}/info", bob_addr)
    check.ok([v["body"] for v in info.get("versions", [])] == ["Hello from Linux", "Hello from Linux (edited)"],
             "message info lists the versions oldest first")
    bob_api.call("POST", "/v1/ack", bob_addr, data={"message_id": folded[0]["message_id"], "with_edits": True})
    check.ok(next(m for m in bob_inbox() if m.get("msg_ref") == first.msg_ref).get("read") is True,
             "ack with_edits acknowledges every version")

    # 6. Receipts: delivered on listing, seen after the ack; masked when read receipts are off.
    sent = alice_sent(first.msg_ref)
    check.ok(tick_state(sent)[0] == "seen", "the sender sees seen (green ✓✓) after the ack")
    bob_api.call("PUT", "/v1/policy", bob_addr, data={"read_receipts": False})
    check.ok(bob_api.call("GET", "/v1/policy", bob_addr).get("read_receipts") is False, "the read-receipts setting is saved")
    quiet = coordinator.prepare(bob_addr, "Read quietly", peer=bob_addr)
    coordinator.send(quiet)
    row = next(m for m in bob_inbox() if m.get("msg_ref") == quiet.msg_ref)
    bob_api.call("POST", "/v1/ack", bob_addr, data={"message_id": row["message_id"], "with_edits": True})
    check.ok(tick_state(alice_sent(quiet.msg_ref))[0] == "delivered", "with read receipts off the sender sees delivered only")
    bob_api.call("PUT", "/v1/policy", bob_addr, data={"read_receipts": True})

    # 7. Login binding: work begun under alice never runs under another login.
    loss(setup, "after")
    held = coordinator.prepare(bob_addr, "Held for review", peer=bob_addr)
    try:
        coordinator.send(held)
    except APIError:
        pass
    loss(setup, "off")
    before = len(attempts(setup))
    alice_session.switch(people["carol"]["token"])
    try:
        coordinator.retry(coordinator.pending()[0])
        check.ok(False, "retired")
    except Retired:
        check.ok(True, "after another account signs in, the old login's retry stops before any request")
    check.ok(len(attempts(setup)) == before, "no request was sent with the newer login")
    carol_lease = Lease(alice_session).child(alice_addr)
    check.ok(Coordinator(store, alice_api.bound(carol_lease), carol_lease, alice_addr, features).pending() == [],
             "another account never sees this account's saved work")
    alice_session.switch(people["alice"]["token"])
    back = Lease(alice_session).child(alice_addr)
    recovered = Coordinator(store, alice_api.bound(back), back, alice_addr, features)
    found = recovered.pending()
    check.ok([p.msg_ref for p in found] == [held.msg_ref], "the same account signing in again finds its unconfirmed message")
    result, _ = recovered.retry(found[0])
    check.ok(result.get("duplicate") is True, "and its explicit retry is not a second delivery")

    folder.cleanup()
    check.done()


if __name__ == "__main__":
    try:
        main()
    except AssertionError as error:
        print("FAILED:", error, file=sys.stderr)
        sys.exit(1)
