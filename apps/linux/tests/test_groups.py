"""Group end-to-end encryption on Linux: groups_core.py through the real UniFFI Python binding and
native library, against an in-memory postbox that enforces docs/groups.md. Covers activation,
invitations, messaging, attachments, removal, a second device without history, concurrent
commits, epoch conflicts, encrypted storage across a restart, expiry and sign-out.

Needs the library in pigeonpost/native/<arch> (./build-native.sh). Set
PIGEONPOST_REQUIRE_GROUPCRYPTO=1 (CI) to fail instead of skipping when it is missing.
"""
import base64
import os
import tempfile
import threading
import time
import unittest
from pathlib import Path

from pigeonpost import groupcrypto
from pigeonpost.groups_core import GroupClient, decode_body, encode_body, timer_label
from pigeonpost.groupstore import GroupStorage, MemoryKeyring, MemoryStorage

gc = groupcrypto.load()
if gc is None and os.environ.get("PIGEONPOST_REQUIRE_GROUPCRYPTO") == "1":
    raise RuntimeError("group crypto library missing: " + str(groupcrypto.reason()))

from fake_groups_postbox import FakePostbox, ed25519_public  # noqa: E402


def texts(client, gid):
    return [x["text"] for x in client.history(gid) if x["kind"] == "message" and not x.get("deleted") and not x.get("undecryptable")]


@unittest.skipIf(gc is None, "group crypto library not built (apps/linux/build-native.sh)")
class GroupTests(unittest.TestCase):
    def setUp(self):
        self.fake = FakePostbox(gc)

    def client(self, address, storage=None, now=None):
        c = GroupClient(gc, self.fake.client_api(address), address, storage or MemoryStorage(),
                        sleep=lambda s: None, jitter=lambda: 0, now=now)
        c.init()
        c.top_up_key_packages()
        return c

    def three_people(self):
        A, B, C = self.fake.mailbox(), self.fake.mailbox(), self.fake.mailbox()
        alice, bob, carol = self.client(A), self.client(B), self.client(C)
        gid = alice.create_group({"name": "Garden"})["group"]["id"]
        for who in (B, C):
            self.fake.invite(gid, who)
            self.fake.accept(gid, who)
        alice.process_pending(gid, no_jitter=True)
        self.assertEqual(bob.process_welcomes(), [gid])
        self.assertEqual(carol.process_welcomes(), [gid])
        return A, B, C, alice, bob, carol, gid

    def test_mailbox_key_derivation_matches_the_library(self):
        seed = os.urandom(32)
        # The library derives /k/ addresses from the key; a wrong key would fail attestation.
        device = gc.GroupDevice.new_with_identity_seed(seed, "probe")
        self.assertEqual(device.mailbox(), gc.mailbox_for_seed(seed))
        self.assertEqual(len(ed25519_public(seed)), 32)

    def test_create_activates_and_an_accepted_invitation_becomes_an_encrypted_member(self):
        A, B, C, alice, bob, carol, gid = self.three_people()
        g = self.fake.groups[gid]
        self.assertEqual(g["activation_state"], "active")
        self.assertEqual(g["epoch"], 2, "activation commit, then one add commit for both invitees")
        self.assertEqual(g["members"][B]["state"], "active")
        self.assertEqual(alice.epoch(gid), 2)
        self.assertEqual(bob.epoch(gid), 2)
        self.assertEqual(alice.safety_number(gid), carol.safety_number(gid), "everyone sees the same devices")
        self.assertEqual(len(alice.members(gid)), 3)
        self.assertTrue(alice.device_id().startswith("linux-"))
        joined = [x for x in bob.history(gid) if x.get("notice") == "you_joined"]
        self.assertEqual(len(joined), 1)

    def test_messages_and_attachments_round_trip_and_the_server_holds_only_ciphertext(self):
        A, B, C, alice, bob, carol, gid = self.three_people()
        alice.send(gid, {"text": "hello garden"})
        bob.sync(gid)
        carol.sync(gid)
        self.assertEqual(texts(bob, gid), ["hello garden"])
        self.assertEqual(texts(carol, gid), ["hello garden"])
        self.assertEqual(texts(alice, gid), ["hello garden"], "the sender keeps its own plaintext")
        stored = next(i for i in self.fake.groups[gid]["items"] if i["type"] == "application")
        self.assertNotIn(b"hello garden", base64.b64decode(stored["ciphertext"]))

        bob.send(gid, {"text": "see attached", "files": [{"name": "plan.txt", "mime": "text/plain", "bytes": b"a secret file"}]})
        carol.sync(gid)
        message = next(x for x in carol.history(gid) if x.get("text") == "see attached")
        self.assertEqual(message["files"][0]["name"], "plan.txt")
        self.assertEqual(carol.open_file(gid, message["files"][0]), b"a secret file")
        self.assertNotIn(b"secret", self.fake.blobs[message["files"][0]["id"]], "attachment stored sealed")

    def test_a_removed_member_loses_access_and_cannot_decrypt_later_messages(self):
        A, B, C, alice, bob, carol, gid = self.three_people()
        alice.send(gid, {"text": "before"})
        carol.sync(gid)
        snapshot = carol.device.export_state(bytes([7]) * 32)
        self.fake.remove(gid, C)
        # The pending removal blocks application sends until it is committed; the client commits it.
        alice.send(gid, {"text": "after"})
        self.assertTrue(any(p.endswith("/commits") for _, _, p in self.fake.calls))
        self.assertNotIn(C + "|" + carol.device_id(), self.fake.groups[gid]["leaves"])
        bob.sync(gid)
        self.assertEqual(texts(bob, gid), ["before", "after"])
        with self.assertRaises(Exception) as caught:
            carol.sync(gid)
        self.assertEqual(getattr(caught.exception, "code", None), "not_member")
        after = [i for i in self.fake.groups[gid]["items"] if i["type"] == "application"][-1]
        stale = gc.GroupDevice.import_state(snapshot, bytes([7]) * 32)
        with self.assertRaises(gc.GroupCryptoError.Failure) as refused:
            stale.process_incoming(gid, base64.b64decode(after["ciphertext"]))
        self.assertIn(refused.exception.code, ("future_epoch", "stale_epoch", "decryption_failed", "not_member"))

    def test_a_new_device_of_a_member_is_added_without_history_and_is_visible(self):
        A, B, C, alice, bob, carol, gid = self.three_people()
        alice.send(gid, {"text": "old news"})
        old = [i for i in self.fake.groups[gid]["items"] if i["type"] == "application"][-1]
        bob2 = self.client(B)
        self.assertNotEqual(bob2.device_id(), bob.device_id())
        bob.process_pending(gid, no_jitter=True)  # a device of the same mailbox adds it
        self.assertEqual(bob2.process_welcomes(), [gid])
        alice.send(gid, {"text": "fresh"})
        bob2.sync(gid)
        self.assertEqual(texts(bob2, gid), ["fresh"], "no pre-join history")
        with self.assertRaises(gc.GroupCryptoError.Failure):
            bob2.device.process_incoming(gid, base64.b64decode(old["ciphertext"]))
        alice.sync(gid)
        commits = [x for x in alice.history(gid) if x["kind"] == "commit"]
        added = [d for x in commits for d in x["added"] if d["device_id"] == bob2.device_id()]
        self.assertEqual(len(added), 1, "the new device appears in a verified commit")
        self.assertEqual(len(alice.fingerprint_digits(added[0]["fingerprint"]).split()), 6)
        self.assertEqual(alice.safety_number(gid), bob2.safety_number(gid))
        self.assertNotEqual(alice.safety_number(gid), None)

    def test_concurrent_commits_the_first_wins_and_the_other_recovers(self):
        A, B, C, alice, bob, carol, gid = self.three_people()
        D = self.fake.mailbox()
        dave = self.client(D)
        self.fake.invite(gid, D)
        self.fake.accept(gid, D)
        before = self.fake.groups[gid]["epoch"]
        # Bob commits while Alice is building: her first post loses and she must recover.
        raced = []

        def race(me, method, path, body):
            if not raced and me == A and method == "POST" and path.endswith("/commits"):
                raced.append(True)
                bob.process_pending(gid, no_jitter=True)
        self.fake.before_handle = race
        alice.process_pending(gid, no_jitter=True)
        self.fake.before_handle = None
        self.assertEqual(self.fake.groups[gid]["epoch"], before + 1, "exactly one add commit landed")
        self.assertTrue(any(code in ("revision_conflict", "not_pending") for _, _, code in self.fake.errors))
        self.assertEqual(dave.process_welcomes(), [gid])
        carol.send(gid, {"text": "welcome dave"})
        for c in (alice, bob, dave):
            c.sync(gid)
        self.assertEqual(texts(dave, gid), ["welcome dave"])
        self.assertEqual(texts(alice, gid), ["welcome dave"])
        self.assertEqual(alice.epoch(gid), bob.epoch(gid))

    def test_epoch_conflict_reencrypts_under_the_same_client_message_id(self):
        A, B, C, alice, bob, carol, gid = self.three_people()
        raced = []

        def race(me, method, path, body):
            if not raced and me == A and method == "POST" and path.endswith("/messages"):
                raced.append(body["client_message_id"])
                bob.self_update(gid)
        self.fake.before_handle = race
        alice.send(gid, {"text": "racing"})
        self.fake.before_handle = None
        self.assertTrue(any(code == "epoch_conflict" for _, _, code in self.fake.errors), "the first post was refused")
        posts = [i for i in self.fake.groups[gid]["items"] if i["type"] == "application"]
        self.assertEqual(len(posts), 1)
        self.assertEqual(posts[0]["client_message_id"], raced[0])
        bob.sync(gid)
        carol.sync(gid)
        self.assertEqual(texts(bob, gid), ["racing"])
        self.assertEqual(texts(carol, gid), ["racing"])

    def test_concurrent_senders_from_threads_stay_consistent(self):
        A, B, C, alice, bob, carol, gid = self.three_people()
        errors = []

        def chatter(client, label):
            try:
                for i in range(4):
                    client.send(gid, {"text": f"{label}{i}"})
            except Exception as exc:  # pragma: no cover - reported below
                errors.append(exc)
        threads = [threading.Thread(target=chatter, args=(c, n)) for c, n in ((alice, "a"), (bob, "b"))]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        self.assertEqual(errors, [])
        carol.sync(gid)
        self.assertEqual(sorted(texts(carol, gid)), sorted([f"a{i}" for i in range(4)] + [f"b{i}" for i in range(4)]))

    def test_encrypted_storage_survives_a_restart_and_holds_no_plaintext(self):
        A, B = self.fake.mailbox(), self.fake.mailbox()
        keyring = MemoryKeyring()
        with tempfile.TemporaryDirectory() as folder:
            storage = GroupStorage(gc, keyring=keyring, directory=folder)
            alice = self.client(A, storage)
            bob = self.client(B)
            gid = alice.create_group({"name": "Persist"})["group"]["id"]
            self.fake.invite(gid, B)
            self.fake.accept(gid, B)
            alice.process_pending(gid, no_jitter=True)
            bob.process_welcomes()
            bob.send(gid, {"text": "one secret line"})
            alice.sync(gid)
            files = list(Path(folder).rglob("store-*.bin"))
            self.assertEqual(len(files), 1, "one sealed record per mailbox; older ones are deleted")
            self.assertEqual(oct(files[0].stat().st_mode & 0o777), "0o600")
            self.assertNotIn(b"one secret line", files[0].read_bytes())
            self.assertNotIn(b"Persist", files[0].read_bytes())

            # The app restarts: a new client over the same files and keyring.
            again = GroupClient(gc, self.fake.client_api(A), A, GroupStorage(gc, keyring=keyring, directory=folder),
                                sleep=lambda s: None, jitter=lambda: 0)
            self.assertEqual(again.init()["device_id"], alice.device_id(), "same device, not a re-enrolment")
            self.assertEqual(texts(again, gid), ["one secret line"])
            bob.send(gid, {"text": "two"})
            again.sync(gid)
            self.assertEqual(texts(again, gid), ["one secret line", "two"])
            again.send(gid, {"text": "three"})
            bob.sync(gid)
            self.assertEqual(texts(bob, gid), ["one secret line", "two", "three"])

            # Without the keyring item the files are useless: the device starts again.
            keyring.items.clear()
            fresh = GroupClient(gc, self.fake.client_api(A), A, GroupStorage(gc, keyring=keyring, directory=folder),
                                sleep=lambda s: None, jitter=lambda: 0)
            self.assertNotEqual(fresh.init()["device_id"], alice.device_id())
            self.assertEqual(fresh.history(gid), [])

    def test_a_tampered_record_is_refused(self):
        A = self.fake.mailbox()
        keyring = MemoryKeyring()
        with tempfile.TemporaryDirectory() as folder:
            storage = GroupStorage(gc, keyring=keyring, directory=folder)
            alice = self.client(A, storage)
            path = next(Path(folder).rglob("store-*.bin"))
            data = bytearray(path.read_bytes())
            data[-1] ^= 1
            path.write_bytes(bytes(data))
            self.assertIsNone(storage.load(A))
            self.assertTrue(alice.device_id())

    def test_expired_items_are_dropped_locally_by_the_device_clock(self):
        A, B, C, alice, bob, carol, gid = self.three_people()
        self.fake.groups[gid]["ephemeral"] = 60
        alice.send(gid, {"text": "brief"})
        self.fake.groups[gid]["ephemeral"] = 0
        alice.send(gid, {"text": "kept"})
        clock = [int(time.time())]
        bob.now = lambda: clock[0]
        bob.sync(gid)
        self.assertEqual(texts(bob, gid), ["brief", "kept"])
        clock[0] += 61
        self.assertEqual(bob.prune_expired(), 1)
        self.assertEqual(texts(bob, gid), ["kept"])
        bob.drop_seqs(gid, [x["seq"] for x in bob.history(gid) if x.get("text") == "kept"])
        self.assertEqual(texts(bob, gid), [])

    def test_weekly_self_update_refreshes_an_idle_leaf(self):
        A, B, C, alice, bob, carol, gid = self.three_people()
        epoch = self.fake.groups[gid]["epoch"]
        self.assertIsNone(carol.maintain(gid), "nothing to do on a fresh leaf")
        clock = [int(time.time()) + 8 * 24 * 3600]
        carol.now = lambda: clock[0]
        carol.maintain(gid)
        self.assertEqual(self.fake.groups[gid]["epoch"], epoch + 1)
        alice.sync(gid)
        self.assertEqual(alice.epoch(gid), epoch + 1)

    def test_sign_out_revokes_the_device_and_deletes_its_keys(self):
        A, B, C, alice, bob, carol, gid = self.three_people()
        keyring = MemoryKeyring()
        with tempfile.TemporaryDirectory() as folder:
            D = self.fake.mailbox()
            dave = self.client(D, GroupStorage(gc, keyring=keyring, directory=folder))
            device = dave.device_id()
            dave.revoke()
            self.assertTrue(self.fake.devices[D][device]["revoked"])
            self.assertEqual(keyring.items, {})
            self.assertEqual(list(Path(folder).rglob("store-*")), [])

    def test_verified_fingerprints_flag_a_change(self):
        A, B, C, alice, bob, carol, gid = self.three_people()
        fingerprints = [d["fingerprint"] for d in alice.members(gid) if d["mailbox"] == B]
        self.assertIsNone(alice.verification(B, fingerprints))
        alice.verify_devices(B, fingerprints)
        self.assertEqual(alice.verification(B, fingerprints), "verified")
        self.assertEqual(alice.verification(B, fingerprints + ["00" * 32]), "changed")

    def test_message_documents_match_the_web_format(self):
        body = encode_body({"text": "hi", "files": [{"id": "gat_1", "name": "a.txt", "mime": "text/plain", "size": 3}]})
        self.assertEqual(body, b'{"v":1,"type":"text","text":"hi","files":[{"id":"gat_1","name":"a.txt","mime":"text/plain","size":3}]}')
        self.assertEqual(decode_body(body)["files"][0]["name"], "a.txt")
        self.assertEqual(decode_body(b"plain words")["text"], "plain words")
        self.assertEqual(timer_label(86400), "1 day")
        self.assertEqual(timer_label(None), "Off")


if __name__ == "__main__":
    unittest.main()
