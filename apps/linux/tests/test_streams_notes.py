"""Phase 6 stream rules and Phase 7 meeting notes crypto and storage: no GUI, synthetic data only.

notes_vector.json was sealed by meeting-intel's own code (tests/seal_brief.mjs --vector), so the
Linux reader is checked against the production sealing, not a Python re-implementation of it."""
import base64
import copy
import json
import threading
import unittest
from pathlib import Path

from pigeonpost import meeting_notes as notes
from pigeonpost import streams
from pigeonpost.binding import Lease, Retired
from pigeonpost.features import notice_text
from pigeonpost.protected import MemoryKeyring, MemoryProtectedStore, ProtectedStore

VECTOR = json.loads((Path(__file__).with_name("notes_vector.json")).read_text())
GID = "grp_" + "a" * 26
SID = "str_" + "b" * 26


class FakeSession:
    def __init__(self, owner="owner-a"):
        self.lock = threading.RLock()
        self.epoch, self.owner = 1, owner

    def switch(self, owner):
        self.epoch += 1
        self.owner = owner


class XorSealer:
    """Tests only: reversible, so sealing is observable without the group library."""

    def seal(self, plain):
        return bytes(b ^ 0x5A for b in plain), {"key": "k", "sha": "s", "len": len(plain)}

    def open(self, sealed, ref):
        return bytes(b ^ 0x5A for b in sealed)


def group(role="member", policy="admins", state="active", activation="active"):
    return {"id": GID, "stream_policy": policy, "activation_state": activation, "my": {"role": role, "state": state}}


class StreamTests(unittest.TestCase):
    def live(self, **extra):
        row = {"id": SID, "group_id": GID, "state": "live", "by": "/k/alice", "title": "Sprint demo", "sources": ["camera"], "stage": []}
        row.update(extra)
        return {"streams": [row]}

    def test_only_this_groups_live_stream_counts(self):
        self.assertEqual(streams.live_stream(self.live(), GID)["title"], "Sprint demo")
        self.assertIsNone(streams.live_stream(self.live(group_id="grp_" + "c" * 26), GID), "another group's answer")
        self.assertIsNone(streams.live_stream(self.live(state="ended"), GID))
        self.assertIsNone(streams.live_stream(self.live(id="str_bad"), GID))
        self.assertIsNone(streams.live_stream({"streams": "x"}, GID))
        self.assertEqual(streams.live_stream(self.live(title="a\u0000b" + "x" * 300), GID)["title"], "ab" + "x" * 98)

    def test_who_may_go_live_follows_the_policy_and_role(self):
        self.assertFalse(streams.can_go_live(group()))
        self.assertTrue(streams.can_go_live(group("admin")))
        self.assertTrue(streams.can_go_live(group("owner")))
        self.assertTrue(streams.can_go_live(group(policy="everyone")))
        self.assertFalse(streams.can_go_live(group("restricted", "everyone")))
        self.assertFalse(streams.can_go_live(group("admin", state="keying")))
        self.assertFalse(streams.can_go_live(group("admin", activation="provisioning")))
        self.assertFalse(streams.can_go_live(dict(group("admin"), deleted_at=1)))
        self.assertFalse(streams.can_go_live(None))

    def test_banner_watch_and_stage_invite(self):
        live = streams.live_stream(self.live(stage=[{"address": "/k/me", "state": "invited"}, {"address": 7}]), GID)
        names = {"/k/alice": "Alice"}.get
        b = streams.banner(live, "/k/me", names)
        self.assertEqual((b["text"], b["watch"]), ("Alice is live: Sprint demo", "Watch in browser"))
        self.assertIn("invited on stage", b["stage"])
        own = streams.banner(live, "/k/alice", names)
        self.assertEqual((own["text"], own["watch"], own["stage"]), ("You are live: Sprint demo", "Open in browser", None))
        self.assertIsNone(streams.banner(None, "/k/me", names))

    def test_links_and_notice(self):
        self.assertEqual(streams.group_link(GID), "https://inbox.pigeonpost.dev/#group=" + GID)
        self.assertEqual(streams.group_link("grp_../x"), "https://inbox.pigeonpost.dev/")
        names = lambda a: "You" if a == "/k/me" else "Jane"
        item = {"kind": "notice", "notice": "stream", "actor": "/k/jane", "metadata": {"duration": 2040, "viewers": 18, "stream_id": SID}}
        self.assertEqual(notice_text(item, names), "Jane streamed for 34 min · 18 viewers")
        item = {"kind": "notice", "notice": "stream", "actor": "/k/me", "metadata": {"duration": 5, "viewers": 1}}
        self.assertEqual(notice_text(item, names), "You streamed for 1 min · 1 viewer")
        self.assertEqual(streams.stream_notice({"duration": "x", "viewers": None}, "Jane"), "Jane streamed for 1 min · 0 viewers")


@unittest.skipUnless(notes.crypto_available(), "python3-cryptography not installed")
class NotesCryptoTests(unittest.TestCase):
    def key(self):
        return notes.NotesKey.from_record(VECTOR["key"])

    def test_opens_a_brief_sealed_by_meeting_intel(self):
        brief = notes.open_brief(VECTOR["meeting"], self.key())
        self.assertEqual(brief["title"], "Release review")
        self.assertEqual(brief["decisions"], ["Ship 1.0.11 on Thursday"])
        self.assertEqual(notes.action_text(brief["action_items"][0]), "Alex: Write the release notes (due Wednesday)")
        self.assertEqual(brief["transcript"][1]["text"], "<b>Agreed</b>", "kept as text, never markup")
        text = notes.as_text(brief)
        self.assertTrue(text.startswith("Release review\n\nThe team agreed"))
        self.assertIn("[00:00:04] Alex: Let's ship on Thursday.", text)
        self.assertIn("(whisper.cpp ggml-small, qwen)", notes.footer(brief))

    def test_another_devices_key_and_tampering_are_refused(self):
        other = notes.NotesKey.generate()
        with self.assertRaises(notes.NotesError) as raised:
            notes.open_brief(VECTOR["meeting"], other)
        self.assertEqual(raised.exception.code, "no_key_for_device")
        # Same key id, different key (a wrap for someone else): authentication fails, nothing shown.
        impostor = notes.NotesKey("nkVectorKey01", other.private_key)
        with self.assertRaises(notes.NotesError) as raised:
            notes.open_brief(VECTOR["meeting"], impostor)
        self.assertEqual(raised.exception.code, "cannot_open")
        for field, value in [("ciphertext", None), ("iv", "AAAA"), ("id", "mtg_" + "z" * 26)]:
            meeting = copy.deepcopy(VECTOR["meeting"])
            if value is None:
                raw = bytearray(base64.b64decode(meeting["ciphertext"]))
                raw[3] ^= 1
                meeting["ciphertext"] = base64.b64encode(bytes(raw)).decode()
            else:
                meeting[field] = value
            with self.assertRaises(notes.NotesError, msg=field):
                notes.open_brief(meeting, self.key())
        with self.assertRaises(notes.NotesError):
            notes.open_brief(dict(VECTOR["meeting"], state="processing"), self.key())

    def test_key_records_round_trip_and_reject_foreign_material(self):
        key = notes.NotesKey.generate()
        self.assertTrue(notes.KEY_ID.match(key.key_id) and key.key_id.startswith("nk"))
        self.assertEqual(len(base64.b64decode(key.public_key)), 65)
        again = notes.NotesKey.from_record(key.record())
        self.assertEqual((again.key_id, again.public_key), (key.key_id, key.public_key))
        self.assertNotIn(key.record()["private_pkcs8"], repr(key))
        for broken in [dict(key.record(), key_id="x"), dict(key.record(), private_pkcs8="!!"),
                       dict(key.record(), public_key=notes.NotesKey.generate().public_key)]:
            with self.assertRaises(notes.NotesError):
                notes.NotesKey.from_record(broken)

    def test_brief_fields_are_bounded_plain_text(self):
        b = notes.brief_of({"title": "T\u0007" + "x" * 500, "decisions": ["a", 3, ""], "action_items": [{"task": ""}, {"task": "Do", "owner": 5}],
                            "transcript": [{"text": "hi"}], "duration_s": -4, "draft": "yes"})
        self.assertEqual(len(b["title"]), 120)
        self.assertNotIn("\u0007", b["title"])
        self.assertEqual(b["decisions"], ["a"])
        self.assertEqual(b["action_items"], [{"owner": None, "task": "Do", "due": None}])
        self.assertEqual(b["transcript"][0]["speaker"], "Someone")
        self.assertIsNone(b["duration_s"])
        self.assertFalse(b["draft"])

    def test_meeting_list_is_checked(self):
        rows = notes.meetings({"meetings": [{"id": "mtg_" + "1" * 26, "state": "ready", "created_at": 5, "expires_at": 9, "host": "/k/h"},
                                            {"id": "../x"}, "junk", {"id": "mtg_" + "2" * 26, "state": "weird"}]})
        self.assertEqual([r["id"][-1] for r in rows], ["1", "2"])
        self.assertEqual(rows[1]["state"], "processing")

    def test_register_sends_only_the_public_half(self):
        sent = []

        class API:
            def call(self, method, path, identity=None, data=None, **_):
                sent.append((method, path, identity, data))
                return {"registered": True}
        key = notes.NotesKey.generate()
        notes.register(API(), key)
        self.assertEqual(sent, [("PUT", "/v1/meetings/keys/" + key.key_id, None, {"public_key": key.public_key, "label": "Linux app"})])


@unittest.skipUnless(notes.crypto_available(), "python3-cryptography not installed")
class NotesKeyStoreTests(unittest.TestCase):
    def test_one_key_per_account_sealed_and_forgotten_at_sign_out(self):
        import tempfile
        with tempfile.TemporaryDirectory() as folder:
            keyring = MemoryKeyring()
            store = ProtectedStore(XorSealer(), keyring, folder)
            session = FakeSession("owner-a")
            lease = Lease(session)
            keys = notes.NotesKeys(store)
            first = keys.get(lease)
            self.assertEqual(keys.get(lease).key_id, first.key_id, "made once, then reused")
            sealed = b"".join(p.read_bytes() for p in Path(folder).rglob("work-*"))
            self.assertNotIn(first.record()["private_pkcs8"].encode(), sealed, "never written in the clear")
            # Another account on this computer gets its own key, never account A's.
            session.switch("owner-b")
            self.assertFalse(lease.current)
            with self.assertRaises(Retired):
                keys.get(lease)
            other = keys.get(Lease(session))
            self.assertNotEqual(other.key_id, first.key_id)
            # Sign-out of A removes A's key only.
            store.forget_owner("owner-a")
            session.switch("owner-a")
            renewed = keys.get(Lease(session))
            self.assertNotEqual(renewed.key_id, first.key_id, "a new sign-in makes a new key")
            session.switch("owner-b")
            self.assertEqual(keys.get(Lease(session)).key_id, other.key_id)

    def test_memory_store_is_per_account_and_not_persistent(self):
        store = MemoryProtectedStore()
        session = FakeSession("owner-a")
        keys = notes.NotesKeys(store)
        a = keys.get(Lease(session))
        session.switch("owner-b")
        self.assertNotEqual(keys.get(Lease(session)).key_id, a.key_id)
        self.assertFalse(keys.persistent())


if __name__ == "__main__":
    unittest.main()
