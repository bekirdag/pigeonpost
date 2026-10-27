"""Message actions, protected delivery, login binding and the sealed work store. No GUI."""
import base64
import hashlib
import json
import os
import tempfile
import threading
import time
import unittest

from pigeonpost.actions import (build_send, can_reply, editable, group_tick, group_tick_names, new_ref, parse_wire,
                                quote_text, ref_of, tick_state, valid_ref, wire)
from pigeonpost.api import APIError, Postbox
from pigeonpost.auth import Session
from pigeonpost.binding import Lease, Retired, owner_key
from pigeonpost.delivery import (Coordinator, DeliveryConflict, draft_slot, load_drafts, save_draft, slot_for,
                                 uncertain)
from pigeonpost.protected import MemoryKeyring, MemoryProtectedStore, ProtectedStore, ProtectedUnavailable

ME, YOU = "/k/" + "a" * 26, "/k/" + "b" * 26


def jwt(sub, iss="https://issuer.invalid/realms/x"):
    def part(doc):
        return base64.urlsafe_b64encode(json.dumps(doc).encode()).decode().rstrip("=")
    return part({"alg": "none"}) + "." + part({"sub": sub, "iss": iss}) + ".sig"


class FakeSession:
    def __init__(self, owner="o" * 64):
        self.lock = threading.RLock()
        self.epoch, self.owner = 1, owner

    def access_token(self, rejected=None, lease=None):
        if lease is not None:
            lease.require()
        return "token"

    def switch(self, owner):
        self.epoch += 1
        self.owner = owner


class TestSealer:
    """Tests only: an HMAC-checked XOR stream under a fresh random key per save."""

    def seal(self, plain):
        key = os.urandom(32)
        stream = hashlib.shake_256(key).digest(len(plain))
        sealed = bytes(a ^ b for a, b in zip(plain, stream))
        return sealed, {"key": base64.b64encode(key).decode(), "sha": base64.b64encode(hashlib.sha256(sealed).digest()).decode(),
                        "len": len(plain)}

    def open(self, sealed, ref):
        if hashlib.sha256(sealed).digest() != base64.b64decode(ref["sha"]) or len(sealed) != ref["len"]:
            raise ValueError("tampered")
        stream = hashlib.shake_256(base64.b64decode(ref["key"])).digest(len(sealed))
        return bytes(a ^ b for a, b in zip(sealed, stream))


class FakeAPI:
    def __init__(self, results=None):
        self.results = list(results or [])
        self.posts, self.calls = [], []

    def post_exact(self, path, data):
        self.posts.append((path, bytes(data)))
        result = self.results.pop(0) if self.results else {"message_id": "m1", "msg_ref": json.loads(data)["msg_ref"]}
        if isinstance(result, Exception):
            raise result
        return result

    def call(self, method, path, identity=None, data=None, **kwargs):
        self.calls.append((method, path, data))
        if path == "/v1/recipients/resolve":
            return {"from": ME, "address": YOU, "handle": "/you"}
        return {}


class ActionsTests(unittest.TestCase):
    def test_reference_format_and_validation(self):
        refs = {new_ref() for _ in range(200)}
        self.assertEqual(len(refs), 200)
        for ref in refs:
            self.assertTrue(valid_ref(ref), ref)
            self.assertEqual(len(ref), 31)
            # 128 random bits then two zero padding bits: the last character is a multiple of 4.
            self.assertEqual("abcdefghijklmnopqrstuvwxyz234567".index(ref[-1]) % 4, 0)
        for bad in ("mref_" + "A" * 26, "mref_" + "a" * 25, "ref_" + "a" * 27, None, 5, "mref_" + "1" * 26):
            self.assertFalse(valid_ref(bad), bad)

    def test_build_send_contract(self):
        body = build_send(ME, "/alex", "hi", "t1", ["att_1"], reply_to="m_1", quote="parent")
        self.assertEqual(body["reply_to"], "m_1")
        self.assertEqual(body["quote"], "parent")
        self.assertEqual(body["thread_id"], "t1")
        self.assertTrue(valid_ref(body["msg_ref"]))
        edit = build_send(ME, "/alex", "fixed", "t1", edit_of="m_2")
        self.assertNotIn("thread_id", edit, "thread is ignored for an edit; never sent")
        with self.assertRaises(ValueError):
            build_send(ME, "/alex", "x", reply_to="a", edit_of="b")
        with self.assertRaises(ValueError):
            build_send(ME, "/alex", "x", edit_of="m", attachments=["att"])
        with self.assertRaises(ValueError):
            build_send(ME, "/alex", "x", quote="q")
        with self.assertRaises(ValueError):
            build_send(ME, "/alex", "x", reply_to="m", quote="q" * 201)
        with self.assertRaises(ValueError):
            build_send(ME, "/alex\n", "x")
        with self.assertRaises(ValueError):
            build_send(ME, "/alex", "   ", edit_of="m")
        with self.assertRaises(ValueError):
            build_send(ME, "/alex", "x", reply_to="bad ref with spaces")

    def test_wire_round_trip_refuses_changes(self):
        data = wire(build_send(ME, "/alex", "hello ✓", "t"))
        self.assertEqual(wire(parse_wire(data)), data)
        doc = json.loads(data)
        doc["extra"] = 1
        with self.assertRaises(ValueError):
            parse_wire(wire(doc))
        with self.assertRaises(ValueError):
            parse_wire(data.replace(b'"body"', b'"body":"x","body"', 1))
        with self.assertRaises(ValueError):
            parse_wire(json.dumps(json.loads(data), indent=1).encode())  # not the exact bytes

    def test_editable_window_and_flags(self):
        now = 1_800_000_000
        mine = {"direction": "out", "sent_at": now - 3600, "message_id": "m"}
        self.assertTrue(editable(mine, {"message_edit"}, now))
        self.assertFalse(editable(mine, set(), now))
        self.assertFalse(editable({**mine, "sent_at": now - 24 * 3600 - 1}, {"message_edit"}, now))
        self.assertTrue(editable({**mine, "sent_at": (now - 60) * 1000}, {"message_edit"}, now), "milliseconds")
        self.assertFalse(editable({**mine, "expires_at": now - 1}, {"message_edit"}, now))
        self.assertFalse(editable({**mine, "direction": None}, {"message_edit"}, now))
        self.assertFalse(editable({**mine, "verb": "deploy"}, {"message_edit"}, now))
        self.assertTrue(can_reply({"message_id": "x"}, {"message_reply"}))
        self.assertFalse(can_reply({"message_id": "x"}, set()))
        self.assertEqual(ref_of({"msg_ref": "mref_" + "a" * 26, "message_id": "x"}), "mref_" + "a" * 26)
        self.assertEqual(ref_of({"msg_ref": "nope", "message_id": "x"}), "x")
        self.assertEqual(quote_text("\n\n  first line  \nsecond"), "first line")
        self.assertEqual(len(quote_text("x" * 500)), 200)

    def test_ticks(self):
        self.assertEqual(tick_state({"pending": True})[0], "sending")
        self.assertEqual(tick_state({}), ("sent", "Sent"))
        self.assertEqual(tick_state({"receipt": {"state": "delivered"}})[0], "delivered")
        state, text = tick_state({"receipt": {"state": "seen", "seen_at": 1_800_000_000}})
        self.assertEqual(state, "seen")
        self.assertTrue(text.startswith("Seen "))
        self.assertEqual(tick_state({"receipt": {"state": "seen"}, "agent_status": {"state": "working"}}),
                         ("working", "Agent working on this"))
        self.assertEqual(tick_state({"agent_status": {"state": "failed"}}), ("failed", "Agent run failed"))
        self.assertEqual(tick_state({"receipt": {"state": "delivered"}, "agent_status": {"state": "done"}})[0], "delivered")
        self.assertEqual(tick_state({"receipt": "seen"})[0], "sent", "malformed receipt is never a stronger state")

    def test_group_ticks(self):
        receipts = {"members": [{"address": "/k/a", "delivered_seq": 5, "seen_seq": 5}, {"address": "/k/b", "delivered_seq": 7, "seen_seq": None}]}
        self.assertEqual(group_tick(5, receipts)[0], "seen")
        self.assertEqual(group_tick(6, receipts)[0], "sent")
        self.assertEqual(group_tick(None, receipts)[0], "sending")
        self.assertEqual(group_tick(5, {"members": [{"delivered_seq": 5, "seen_seq": None}]})[0], "delivered",
                         "seen needs at least one visible read cursor")
        self.assertEqual(group_tick(3, {"members": []})[0], "sent")
        seen, delivered, waiting = group_tick_names(6, receipts, lambda a: a)
        self.assertEqual((seen, delivered, waiting), ([], ["/k/b"], ["/k/a"]))


class BindingTests(unittest.TestCase):
    def test_owner_key_from_claims(self):
        self.assertEqual(owner_key(jwt("u1")), hashlib.sha256(b"https://issuer.invalid/realms/x\0u1").hexdigest())
        self.assertNotEqual(owner_key(jwt("u1")), owner_key(jwt("u2")))
        for bad in ("access", "a.b", "a.!!!.c", None, jwt("")):
            self.assertIsNone(owner_key(bad))

    def test_lease_retires_on_new_login_and_children(self):
        session = FakeSession()
        lease = Lease(session)
        child = lease.child(ME)
        self.assertTrue(child.current)
        child.retire()
        self.assertFalse(child.current)
        self.assertTrue(lease.current)
        other = lease.child(ME)
        session.switch("p" * 64)
        self.assertFalse(lease.current)
        self.assertFalse(other.current)
        with self.assertRaises(Retired):
            other.require()

    def test_session_epochs_and_bound_requests(self):
        from test_core import MemoryVault, StubTransport
        transport = StubTransport([
            {"access_token": jwt("alice"), "refresh_token": "r1", "expires_in": 300},
            {"ok": True},
            {"access_token": jwt("bob"), "refresh_token": "r2", "expires_in": 300},
        ])
        vault = MemoryVault()
        vault.value = "r0"
        session = Session(vault, transport)
        self.assertTrue(session.restore())
        alice = Lease(session)
        self.assertEqual(alice.owner, owner_key(jwt("alice")))
        api = Postbox(session, transport).bound(alice)
        self.assertEqual(api.call("GET", "/v1/whoami"), {"ok": True})
        # A renewal that names another account is a new login: the old lease stops before any request.
        session.token = None
        session.access_token()
        self.assertFalse(alice.current)
        with self.assertRaises(Retired):
            api.call("GET", "/v1/whoami")
        self.assertEqual(len(transport.calls), 3, "no request was made with the newer login's token")

    def test_sign_out_retires(self):
        from test_core import MemoryVault, StubTransport
        transport = StubTransport([{"access_token": jwt("a"), "refresh_token": "r1", "expires_in": 300}, {}])
        vault = MemoryVault()
        vault.value = "r0"
        session = Session(vault, transport)
        session.restore()
        lease = Lease(session)
        session.sign_out()
        self.assertFalse(lease.current)


class StoreTests(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory(prefix="pp-protected-")
        self.keyring = MemoryKeyring()
        self.store = ProtectedStore(TestSealer(), self.keyring, self.folder.name)
        self.session = FakeSession()
        self.lease = Lease(self.session)

    def tearDown(self):
        self.folder.cleanup()

    def test_round_trip_is_sealed_and_private(self):
        self.store.update(self.lease, ME, lambda r: r.setdefault("drafts", {}).update(x={"peer": "/a", "text": "secret words"}))
        self.assertEqual(self.store.read(self.lease, ME)["drafts"]["x"]["text"], "secret words")
        files = [p for p in __import__("pathlib").Path(self.folder.name).rglob("*") if p.is_file()]
        self.assertEqual(len(files), 1)
        self.assertNotIn(b"secret words", files[0].read_bytes())
        self.assertEqual(files[0].stat().st_mode & 0o777, 0o600)
        self.assertEqual(files[0].parent.stat().st_mode & 0o777, 0o700)
        # Another account's lease sees its own (empty) record, never this one.
        self.session.switch("q" * 64)
        other = Lease(self.session)
        self.assertNotIn("drafts", self.store.read(other, ME))
        with self.assertRaises(Retired):
            self.store.read(self.lease, ME)

    def test_tampering_and_mismatch_are_refused(self):
        self.store.update(self.lease, ME, lambda r: r.update(drafts={}))
        path = next(p for p in __import__("pathlib").Path(self.folder.name).rglob("work-*"))
        data = bytearray(path.read_bytes())
        data[0] ^= 1
        path.write_bytes(bytes(data))
        with self.assertRaises(ProtectedUnavailable):
            self.store.read(self.lease, ME)
        # A record moved into another mailbox's slot is refused, not adopted.
        store = ProtectedStore(TestSealer(), MemoryKeyring(), self.folder.name + "-b")
        store.update(self.lease, ME, lambda r: None)
        owner, slot = self.lease.owner[:32], hashlib.sha256(ME.encode()).hexdigest()[:32]
        target = hashlib.sha256(YOU.encode()).hexdigest()[:32]
        store.keyring.items[(owner, target)] = store.keyring.items[(owner, slot)]
        os.makedirs(os.path.join(self.folder.name + "-b", owner, target))
        for name in os.listdir(os.path.join(self.folder.name + "-b", owner, slot)):
            os.rename(os.path.join(self.folder.name + "-b", owner, slot, name), os.path.join(self.folder.name + "-b", owner, target, name))
        with self.assertRaises(ProtectedUnavailable):
            store.read(self.lease, YOU)

    def test_group_library_sealer(self):
        """The shipped sealer: the group library's XChaCha20-Poly1305 attachment sealing."""
        from pigeonpost import groupcrypto
        from pigeonpost.protected import GroupCryptoSealer
        gc = groupcrypto.load()
        if gc is None:
            if os.environ.get("PIGEONPOST_REQUIRE_GROUPCRYPTO"):
                self.fail(groupcrypto.reason())
            self.skipTest("group library not built here")
        store = ProtectedStore(GroupCryptoSealer(gc), MemoryKeyring(), self.folder.name + "-gc")
        store.update(self.lease, ME, lambda r: r.update(drafts={"k": {"peer": "/a", "subject": None, "text": "sealed draft"}}))
        self.assertEqual(store.read(self.lease, ME)["drafts"]["k"]["text"], "sealed draft")
        path = next(__import__("pathlib").Path(self.folder.name + "-gc").rglob("work-*"))
        self.assertNotIn(b"sealed draft", path.read_bytes())
        data = bytearray(path.read_bytes())
        data[-1] ^= 1
        path.write_bytes(bytes(data))
        with self.assertRaises(ProtectedUnavailable):
            store.read(self.lease, ME)

    def test_forget_owner_and_no_owner(self):
        self.store.update(self.lease, ME, lambda r: r.update(drafts={"a": {"peer": "/a", "text": "t"}}))
        self.store.forget_owner(self.lease.owner)
        self.assertFalse(self.keyring.items)
        self.assertNotIn("drafts", self.store.read(self.lease, ME))
        nobody = Lease(FakeSession(owner=None))
        with self.assertRaises(ProtectedUnavailable):
            self.store.read(nobody, ME)


class DeliveryTests(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory(prefix="pp-delivery-")
        self.store = ProtectedStore(TestSealer(), MemoryKeyring(), self.folder.name)
        self.session = FakeSession()
        self.lease = Lease(self.session).child(ME)

    def tearDown(self):
        self.folder.cleanup()

    def coordinator(self, api, features=frozenset(), store=None):
        return Coordinator(store or self.store, api, self.lease, ME, features)

    def test_confirmed_send_removes_saved_request(self):
        api = FakeAPI()
        c = self.coordinator(api)
        attempt = c.prepare("/alex", "hello", thread="t1")
        self.assertEqual(len(c.pending()), 1, "saved before the first POST")
        result, cleanup = c.send(attempt)
        self.assertFalse(cleanup)
        self.assertEqual(c.pending(), [])
        self.assertEqual(api.posts[0][1], attempt.wire)

    def test_uncertain_keeps_exact_bytes_for_explicit_retry(self):
        api = FakeAPI([APIError(0, "network_error"), APIError(502, "bad_gateway"), {"message_id": "m", "duplicate": True}])
        c = self.coordinator(api)
        attempt = c.prepare("/alex", "important", thread=None, reply_to="m0", quote="parent")
        with self.assertRaises(APIError):
            c.send(attempt)
        saved = c.pending()
        self.assertEqual([a.state for a in saved], ["uncertain"])
        self.assertEqual(saved[0].wire, attempt.wire)
        # Nothing replays by itself; another send to the same conversation waits.
        with self.assertRaises(DeliveryConflict):
            c.prepare("/alex", "another")
        with self.assertRaises(APIError):
            c.retry(saved[0])
        c.retry(c.pending()[0])
        self.assertEqual([p[1] for p in api.posts], [attempt.wire] * 3, "every retry posts the identical bytes")
        self.assertEqual(c.pending(), [])
        # Another conversation was never blocked.
        self.assertTrue(c.prepare("/other", "fine"))

    def test_definite_refusal_clears_and_mismatched_reply_is_uncertain(self):
        api = FakeAPI([APIError(403, "feature_disabled"), {"message_id": "m", "msg_ref": "mref_" + "z" * 26}])
        c = self.coordinator(api)
        with self.assertRaises(APIError):
            c.send(c.prepare("/alex", "x"))
        self.assertEqual(c.pending(), [])
        attempt = c.prepare("/alex", "y")
        with self.assertRaises(APIError):
            c.send(attempt)
        self.assertEqual(len(c.pending()), 1, "a reply for another reference never confirms this one")
        self.assertTrue(uncertain(APIError(0)))
        self.assertTrue(uncertain(APIError(503)))
        self.assertFalse(uncertain(APIError(400)))
        self.assertFalse(uncertain(APIError(429)))

    def test_recipient_frozen_by_lookup(self):
        api = FakeAPI()
        c = self.coordinator(api, {"recipient_lookup"})
        attempt = c.prepare("/you", "hi", peer="/you")
        self.assertEqual(attempt.request["to"], YOU)
        self.assertEqual(attempt.peer, "/you")
        self.assertEqual(attempt.slot, slot_for(YOU, None))

        class Wrong(FakeAPI):
            def call(self, method, path, identity=None, data=None, **kwargs):
                return {"from": "/k/" + "c" * 26, "address": YOU}
        with self.assertRaises(APIError):
            self.coordinator(Wrong(), {"recipient_lookup"}).prepare("/other", "hi")

    def test_retired_login_cannot_send_or_retry(self):
        api = FakeAPI([APIError(0)])
        c = self.coordinator(api)
        attempt = c.prepare("/alex", "x")
        with self.assertRaises(APIError):
            c.send(attempt)
        self.session.switch("n" * 64)
        with self.assertRaises(Retired):
            c.retry(attempt)
        with self.assertRaises(Retired):
            c.pending()
        self.assertEqual(len(api.posts), 1)
        # The same account signing in again (a new lease for it) finds its saved request.
        self.session.owner = "o" * 64
        again = Coordinator(self.store, FakeAPI(), Lease(self.session).child(ME), ME)
        self.assertEqual([a.msg_ref for a in again.pending()], [attempt.msg_ref])

    def test_discard_is_conditional_on_reference(self):
        c = self.coordinator(FakeAPI([APIError(0)]))
        attempt = c.prepare("/alex", "x")
        with self.assertRaises(APIError):
            c.send(attempt)
        self.assertTrue(c.discard(attempt))
        self.assertFalse(c.discard(attempt))
        newer = c.prepare("/alex", "y")
        with self.assertRaises(DeliveryConflict):
            c.discard(type("Old", (), {"slot": newer.slot, "msg_ref": attempt.msg_ref})())
        self.assertEqual(len(c.pending()), 1)

    def test_memory_store_and_drafts(self):
        store = MemoryProtectedStore()
        lease = Lease(FakeSession(owner=None)).child(ME)
        save_draft(store, lease, ME, "/alex", "t1", "half a thought")
        save_draft(store, lease, ME, "/alex", None, "all")
        self.assertEqual(load_drafts(store, lease, ME), {("/alex", "t1"): "half a thought", ("/alex", None): "all"})
        save_draft(store, lease, ME, "/alex", "t1", "  ")
        self.assertEqual(load_drafts(store, lease, ME), {("/alex", None): "all"})
        self.assertNotEqual(draft_slot("/a", None), draft_slot("/a", ""))


if __name__ == "__main__":
    unittest.main()
