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
from pigeonpost.actions import group_tick, group_tick_names
from pigeonpost.groups_core import ENC_SCHEME, GroupClient, GroupError, UncertainSend, b64, decode_body, encode_body, timer_label
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

    # ---- message actions: edits, replies, deletion, receipts, unconfirmed sends ---------------

    def seq_of(self, client, gid, text):
        return next(x["seq"] for x in client.history(gid) if x.get("kind") == "message" and x.get("text") == text)

    def visible(self, client, gid):
        return [x["text"] for x in client.history(gid) if x.get("kind") == "message" and not x.get("replaced_by")
                and not x.get("deleted") and not x.get("missing") and not x.get("undecryptable")]

    def test_an_edit_folds_its_versions_and_keeps_the_files(self):
        A, B, C, alice, bob, carol, gid = self.three_people()
        alice.send(gid, {"text": "draft plan", "files": [{"name": "plan.txt", "mime": "text/plain", "bytes": b"the plan"}]})
        bob.sync(gid)
        first = self.seq_of(alice, gid, "draft plan")
        self.assertTrue(alice.editable(gid, first))
        self.assertFalse(bob.editable(gid, first), "only the sender edits")
        uploads = len(self.fake.blobs)
        alice.edit(gid, first, "final plan")
        self.assertEqual(len(self.fake.blobs), uploads, "a text edit keeps its files without a new upload")
        second = self.seq_of(alice, gid, "final plan")
        bob.sync(gid)
        self.assertFalse(alice.editable(gid, first), "an older version is not editable")
        alice.edit(gid, second, "final plan v2")
        bob.sync(gid)
        self.assertEqual(self.visible(bob, gid), ["final plan v2"])
        latest = bob.current(gid, first)
        self.assertEqual(latest["text"], "final plan v2")
        self.assertEqual(bob.open_file(gid, latest["files"][0]), b"the plan")
        info = bob.versions(gid, first)
        self.assertEqual([v["text"] for v in info["versions"]], ["draft plan", "final plan", "final plan v2"])
        self.assertFalse(info["missing"])
        # A device that joins later gets only the current version; its history says so.
        carol2 = self.client(C)
        carol.process_pending(gid, no_jitter=True)
        carol2.process_welcomes()
        alice.send(gid, {"text": "after"})
        carol2.sync(gid)
        self.assertEqual(self.visible(carol2, gid), ["after"])

    def test_late_device_sees_missing_history_explicitly(self):
        A, B, C, alice, bob, carol, gid = self.three_people()
        alice.send(gid, {"text": "one"})
        bob.sync(gid)
        first = self.seq_of(alice, gid, "one")
        alice.edit(gid, first, "two")
        # Carol never synced before the edit: the original's ciphertext is gone.
        carol.sync(gid)
        self.assertEqual(self.visible(carol, gid), ["two"])
        info = carol.versions(gid, self.seq_of(carol, gid, "two"))
        self.assertTrue(info["missing"])
        self.assertEqual([v["text"] for v in info["versions"]], ["two"])
        # Bob saw "two", missed "three" (cleared by "four"): the gap is linked within Alice's chain.
        bob.sync(gid)
        alice.edit(gid, self.seq_of(alice, gid, "two"), "three")
        alice.edit(gid, self.seq_of(alice, gid, "three"), "four")
        bob.sync(gid)
        self.assertEqual(self.visible(bob, gid), ["four"])
        info = bob.versions(gid, self.seq_of(bob, gid, "four"))
        self.assertTrue(info["missing"])
        self.assertEqual([v["text"] for v in info["versions"]], ["one", "two", "four"])

    def test_a_forged_or_cross_sender_edit_never_replaces_text(self):
        A, B, C, alice, bob, carol, gid = self.three_people()
        alice.send(gid, {"text": "alice says"})
        target = self.seq_of(alice, gid, "alice says")
        # Carol's encrypted body claims to edit Alice's message; the envelope does not.
        with carol.lock:
            carol.sync(gid)
            cmid = "forged-1"
            out = carol.device.encrypt_message(gid, cmid, encode_body({"text": "carol rewrote it", "edit_of": str(target)}), [])
            carol.persist()
            carol.api("POST", f"/v1/groups/{gid}/messages", {"request_key": "rk_" + cmid, "client_message_id": cmid, "epoch": out.epoch,
                      "enc_scheme": ENC_SCHEME, "ciphertext": b64(out.ciphertext), "envelope_signature": b64(out.envelope_signature),
                      "attachments": []}, device=True)
        # A server-only hint naming Alice's message on Carol's next post.
        carol.send(gid, {"text": "plain words"})
        hinted = next(i for i in self.fake.groups[gid]["items"] if i.get("client_message_id") and i["sender"] == C and i["seq"] != target
                      and i.get("client_message_id") != cmid)
        hinted["edit_of"] = target
        bob.sync(gid)
        self.assertEqual(self.visible(bob, gid), ["alice says", "carol rewrote it", "plain words"])
        self.assertEqual(sum(1 for x in bob.history(gid) if x.get("warning") == "edit_unverified"), 2)
        # Nor can Carol edit Alice's message through the postbox.
        with self.assertRaises(GroupError):
            carol.edit(gid, target, "nope")

    def test_edit_window_and_expiry(self):
        A, B, C, alice, bob, carol, gid = self.three_people()
        alice.send(gid, {"text": "old"})
        seq = self.seq_of(alice, gid, "old")
        clock = [int(time.time()) + 25 * 3600]
        alice.now = lambda: clock[0]
        self.assertFalse(alice.editable(gid, seq))
        with self.assertRaises(GroupError) as refused:
            alice.edit(gid, seq, "too late")
        self.assertEqual(refused.exception.code, "not_editable")

    def test_replies_need_matching_references(self):
        A, B, C, alice, bob, carol, gid = self.three_people()
        alice.send(gid, {"text": "question"})
        bob.sync(gid)
        parent = self.seq_of(bob, gid, "question")
        bob.send(gid, {"text": "answer", "reply_to": parent})
        carol.sync(gid)
        answer = next(x for x in carol.history(gid) if x.get("text") == "answer")
        self.assertEqual(answer["reply_to"], str(parent))
        # A body that claims a reply the envelope does not carry shows no quote.
        with carol.lock:
            out = carol.device.encrypt_message(gid, "fake-reply", encode_body({"text": "sneaky", "reply_to": str(parent)}), [])
            carol.persist()
            carol.api("POST", f"/v1/groups/{gid}/messages", {"request_key": "rk_x", "client_message_id": "fake-reply", "epoch": out.epoch,
                      "enc_scheme": ENC_SCHEME, "ciphertext": b64(out.ciphertext), "envelope_signature": b64(out.envelope_signature),
                      "attachments": []}, device=True)
        bob.sync(gid)
        self.assertIsNone(next(x for x in bob.history(gid) if x.get("text") == "sneaky")["reply_to"])

    def test_deleting_any_version_erases_the_whole_chain_everywhere(self):
        A, B, C, alice, bob, carol, gid = self.three_people()
        alice.send(gid, {"text": "v1"})
        bob.sync(gid)
        first = self.seq_of(alice, gid, "v1")
        alice.edit(gid, first, "v2")
        alice.send(gid, {"text": "unrelated"})
        bob.sync(gid)
        second = self.seq_of(alice, gid, "v2")
        self.assertEqual(sorted(alice.delete_message(gid, first)), sorted([first, second]))
        self.assertEqual(self.visible(alice, gid), ["unrelated"])
        self.assertEqual(self.visible(bob, gid), ["v2", "unrelated"])
        bob.reconcile(gid)
        self.assertEqual(self.visible(bob, gid), ["unrelated"])
        self.assertFalse(any(x.get("text") in ("v1", "v2") for x in bob.history(gid)), "every local version is erased")

    def test_receipts_turn_ticks_and_respect_privacy(self):
        A, B, C, alice, bob, carol, gid = self.three_people()
        alice.send(gid, {"text": "tick"})
        seq = self.seq_of(alice, gid, "tick")
        self.assertEqual(group_tick(seq, alice.receipts(gid))[0], "sent")
        bob.sync(gid)
        carol.sync(gid)
        self.assertEqual(group_tick(seq, alice.receipts(gid))[0], "delivered")
        self.fake.handle(B, "POST", f"/v1/groups/{gid}/read", {"seq": seq}, None, None)
        self.assertEqual(group_tick(seq, alice.receipts(gid))[0], "delivered", "Carol has not read it")
        self.fake.handle(C, "POST", f"/v1/groups/{gid}/read", {"seq": seq}, None, None)
        receipts = alice.receipts(gid)
        self.assertEqual(group_tick(seq, receipts)[0], "seen")
        seen, delivered, waiting = group_tick_names(seq, receipts, lambda a: a)
        self.assertEqual(sorted(seen), sorted([B, C]))
        self.fake.receipts_off.add(A)
        self.assertTrue(all(m["seen_seq"] is None for m in alice.receipts(gid)["members"]))
        self.assertEqual(group_tick(seq, alice.receipts(gid))[0], "delivered", "no green ticks for someone who shares none")

    def lossy(self, address, mode):
        """An api for `address` whose next message post loses its answer (after) or never arrives (before)."""
        inner = self.fake.client_api(address)
        state = {"armed": True}

        def api(method, path, body=None, device=None, raw=None, binary=False):
            if state["armed"] and method == "POST" and path.endswith("/messages"):
                state["armed"] = False
                if mode == "before":
                    raise GroupError(0, "network_error")
                inner(method, path, body=body, device=device, raw=raw, binary=binary)
                raise GroupError(502, "http_502")
            return inner(method, path, body=body, device=device, raw=raw, binary=binary)
        api.state = state
        return api

    def test_a_lost_answer_is_kept_and_only_an_explicit_retry_resends_the_same_bytes(self):
        A, B, C, alice, bob, carol, gid = self.three_people()
        keyring = MemoryKeyring()
        with tempfile.TemporaryDirectory() as folder:
            D = self.fake.mailbox()
            dave = self.client(D, GroupStorage(gc, keyring=keyring, directory=folder))
            self.fake.invite(gid, D)
            self.fake.accept(gid, D)
            alice.process_pending(gid, no_jitter=True)
            dave.process_welcomes()
            dave._api = self.lossy(D, "after")
            with self.assertRaises(UncertainSend):
                dave.send(gid, {"text": "did it land?"})
            posted = [i for i in self.fake.groups[gid]["items"] if i["type"] == "application" and i["sender"] == D]
            self.assertEqual(len(posted), 1, "the first attempt reached the postbox once")
            [entry] = dave.outbox(gid)
            self.assertEqual(entry["state"], "uncertain")
            sent = next(x for x in dave.history(gid) if x.get("text") == "did it land?")
            self.assertTrue(sent["uncertain"])
            # Restart: the request is still there, unconfirmed, and nothing is replayed.
            again = GroupClient(gc, self.fake.client_api(D), D, GroupStorage(gc, keyring=keyring, directory=folder),
                                sleep=lambda s: None, jitter=lambda: 0)
            again.init()
            self.assertEqual([e["state"] for e in again.outbox(gid)], ["uncertain"])
            self.assertEqual(len([i for i in self.fake.groups[gid]["items"] if i.get("sender") == D and i["type"] == "application"]), 1)
            stored = next(f for f in Path(folder).rglob("store-*.bin"))
            self.assertNotIn(b"did it land?", stored.read_bytes())
            result = again.retry_outbox(entry["cmid"])
            self.assertEqual(result["seq"], posted[0]["seq"], "the postbox answered the duplicate with the original")
            self.assertEqual(again.outbox(gid), [])
            self.assertFalse(next(x for x in again.history(gid) if x.get("text") == "did it land?").get("pending"))
            bob.sync(gid)
            self.assertEqual(self.visible(bob, gid).count("did it land?"), 1)

    def test_a_request_that_never_arrived_is_posted_by_retry_and_can_be_dismissed(self):
        A, B, C, alice, bob, carol, gid = self.three_people()
        bob._api = self.lossy(B, "before")
        with self.assertRaises(UncertainSend):
            bob.send(gid, {"text": "first try"})
        self.assertEqual([i for i in self.fake.groups[gid]["items"] if i["type"] == "application"], [])
        [entry] = bob.outbox(gid)
        bob.retry_outbox(entry["cmid"])
        alice.sync(gid)
        self.assertEqual(self.visible(alice, gid), ["first try"])
        bob._api = self.lossy(B, "before")
        with self.assertRaises(UncertainSend):
            bob.send(gid, {"text": "never mind"})
        [entry] = bob.outbox(gid)
        self.assertTrue(bob.dismiss_outbox(entry["cmid"]))
        self.assertEqual(bob.outbox(gid), [])
        self.assertNotIn("never mind", self.visible(bob, gid))
        # A definite refusal is not kept: the caller gets the draft back.
        bob._api = self.fake.client_api(B)
        self.fake.groups[gid]["post_policy"] = "admins"
        with self.assertRaises(GroupError) as refused:
            bob.send(gid, {"text": "refused"})
        self.assertEqual(refused.exception.code, "posting_restricted")
        self.assertEqual(bob.outbox(gid), [])

    def test_the_outbox_is_bounded_and_deletion_drops_queued_edits(self):
        A, B, C, alice, bob, carol, gid = self.three_people()
        alice.send(gid, {"text": "base"})
        seq = self.seq_of(alice, gid, "base")
        alice._api = self.lossy(A, "before")
        with self.assertRaises(UncertainSend):
            alice.edit(gid, seq, "base edited")
        self.assertEqual(len(alice.outbox(gid)), 1)
        self.assertFalse(alice.editable(gid, seq), "one queued edit at a time")
        alice._api = self.fake.client_api(A)
        alice.delete_message(gid, seq)
        self.assertEqual(alice.outbox(gid), [], "deleting the version drops its queued edit")
        for i in range(16):
            alice.record.setdefault("outbox", []).append({"cmid": f"x{i}", "gid": gid, "payload": {}, "doc": {"text": ""},
                                                          "state": "uncertain", "created_at": alice.now()})
        with self.assertRaises(GroupError) as full:
            alice.send(gid, {"text": "one too many"})
        self.assertEqual(full.exception.code, "outbox_full")

    def test_message_documents_match_the_web_format(self):
        body = encode_body({"text": "hi", "files": [{"id": "gat_1", "name": "a.txt", "mime": "text/plain", "size": 3}]})
        self.assertEqual(body, b'{"v":1,"type":"text","text":"hi","files":[{"id":"gat_1","name":"a.txt","mime":"text/plain","size":3}]}')
        self.assertEqual(decode_body(body)["files"][0]["name"], "a.txt")
        self.assertEqual(decode_body(b"plain words")["text"], "plain words")
        self.assertEqual(timer_label(86400), "1 day")
        self.assertEqual(timer_label(None), "Off")


if __name__ == "__main__":
    unittest.main()
