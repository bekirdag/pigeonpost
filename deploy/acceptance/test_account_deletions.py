"""Destructive operator tooling must preserve every other account's copies."""
import hashlib
import importlib.util
from pathlib import Path
import sqlite3
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("deletions", Path(__file__).parents[1] / "account-deletions.py")
ops = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ops)


class AccountDeletionTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.db = sqlite3.connect(":memory:")
        self.addCleanup(self.db.close)
        self.db.row_factory = sqlite3.Row
        schemas = {
            "accounts": "id TEXT, oidc_sub TEXT",
            "identities": "address TEXT, account_id TEXT",
            "account_deletion_requests": "request_id TEXT, account_id TEXT, oidc_sub TEXT, contact_address TEXT, requested_at INTEGER, complete_by INTEGER, completed_at INTEGER",
            "erased_member_subjects": "subject_hash BLOB PRIMARY KEY, erased_at INTEGER",
            "attachments": "id TEXT, sha256 TEXT, owner TEXT",
            "messages": "id TEXT, owner TEXT, sender TEXT, recipient TEXT",
            "threads": "id TEXT, owner TEXT",
            "contacts": "owner TEXT, peer TEXT",
            "archived_threads": "owner TEXT, peer TEXT",
            "inbox_policy": "address TEXT",
            "workspace_context": "address TEXT",
            "devices": "token TEXT, mailbox TEXT, account TEXT",
            "mint_events": "id TEXT, address TEXT",
            "api_keys": "key_hash TEXT, account_id TEXT",
            "provider_identities": "provider TEXT, login TEXT, account_id TEXT",
            "apple_subscriptions": "original_transaction_id TEXT, account_id TEXT",
            "google_subscriptions": "purchase_token TEXT, account_id TEXT",
            "namespaces": "namespace TEXT, account_id TEXT, provider_ref TEXT",
            "apple_handle_plans": "account_id TEXT, original_transaction_id TEXT",
            "apple_plan_names": "account_id TEXT, slot INTEGER, namespace TEXT",
            "test_handle_claims": "account_id TEXT, namespace TEXT",
            "spam_reports": "reporter TEXT, sender TEXT",
            "reputation": "subject TEXT",
            "phone_numbers": "id TEXT, account_id TEXT, identity TEXT, tag BLOB",
            "phone_challenges": "id TEXT, account_id TEXT, identity TEXT",
            "sms_invites": "id TEXT, sender_account TEXT, tag BLOB",
            "sms_ledger": "id TEXT, account_id TEXT, tag BLOB",
            "sms_optouts": "tag BLOB, created_at INTEGER",
            "discovery_usage": "account_id TEXT, day INTEGER, new_hashes INTEGER",
            "discovery_marks": "account_id TEXT, mark BLOB, expires_at INTEGER",
            "discovery_lockouts": "account_id TEXT, until INTEGER",
            "profile_names": "address TEXT, account_id TEXT, display_name TEXT",
        }
        for table, cols in schemas.items():
            self.db.execute("CREATE TABLE " + table + " (" + cols + ")")
        self.request = "del_" + "a" * 32
        self.db.execute("INSERT INTO account_deletion_requests VALUES (?,?,?,?,?,?,NULL)", (self.request, "a", "subject-a", "a@example.test", 10, 2592010))
        for who in ("a", "b"):
            address = "/k/" + who
            self.db.execute("INSERT INTO accounts VALUES (?,?)", (who, "subject-" + who))
            self.db.execute("INSERT INTO identities VALUES (?,?)", (address, who))
            self.db.execute("INSERT INTO devices VALUES (?,?,?)", ("token-" + who, address, who))
            self.db.execute("INSERT INTO api_keys VALUES (?,?)", ("key-" + who, who))
            self.db.execute("INSERT INTO messages VALUES (?,?,?,?)", ("message-" + who, address, "/k/a", "/k/b"))
            self.db.execute("INSERT INTO contacts VALUES (?,?)", (address, "/k/peer"))
            self.db.execute("INSERT INTO apple_subscriptions VALUES (?,?)", ("purchase-" + who, who))
            self.db.execute("INSERT INTO apple_handle_plans VALUES (?,?)", (who, "plan-" + who))
            self.db.execute("INSERT INTO apple_plan_names VALUES (?,1,?)", (who, "name-" + who))
            self.db.execute("INSERT INTO namespaces VALUES (?,?,?)", ("name-" + who, who, "plan:" + who))
            self.db.execute("INSERT INTO phone_numbers VALUES (?,?,?,?)", ("ph-" + who, who, address, b"t" + who.encode()))
            self.db.execute("INSERT INTO phone_challenges VALUES (?,?,?)", ("pvc-" + who, who, address))
            self.db.execute("INSERT INTO sms_invites VALUES (?,?,?)", ("inv-" + who, who, b"x"))
            self.db.execute("INSERT INTO sms_ledger VALUES (?,?,?)", ("sms-" + who, who, b"x"))
            self.db.execute("INSERT INTO discovery_usage VALUES (?,1,5)", (who,))
            self.db.execute("INSERT INTO discovery_marks VALUES (?,?,9)", (who, b"m"))
            self.db.execute("INSERT INTO discovery_lockouts VALUES (?,9)", (who,))
            self.db.execute("INSERT INTO profile_names VALUES (?,?,?)", (address, who, "Name " + who))
        self.db.execute("INSERT INTO sms_optouts VALUES (?,1)", (b"opted",))
        # Rows for /k/a whose account column does not name the account still belong to the mailbox.
        self.db.execute("INSERT INTO phone_numbers VALUES (?,?,?,?)", ("ph-stale", None, "/k/a", b"stale"))
        self.db.execute("INSERT INTO phone_challenges VALUES (?,?,?)", ("pvc-stale", None, "/k/a"))
        self.db.execute("INSERT INTO profile_names VALUES (?,?,?)", ("/k/a", None, "Stale name"))
        self.shared = "a" * 64
        self.private = "b" * 64
        for sha in (self.shared, self.private):
            path = self.root / sha[:2] / sha[2:4] / sha
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"fictional attachment")
        for ident, sha, owner in [("shared-a", self.shared, "/k/a"), ("shared-b", self.shared, "/k/b"), ("private-a", self.private, "/k/a")]:
            self.db.execute("INSERT INTO attachments VALUES (?,?,?)", (ident, sha, owner))
        self.db.commit()
        self.evidence = {"request_id": self.request}
        for key in ("identity_deleted", "sessions_revoked", "apple_revoke_reviewed", "billing_retention_reviewed", "registry_retention_reviewed", "services_stopped", "completion_notice_arranged", "backup_expiry_recorded"):
            self.evidence[key] = True

    def test_erases_only_confirmed_accounts_copies_and_unshared_files(self):
        result = ops.erase(self.db, self.request, self.root, self.evidence)
        self.assertEqual(result["mailboxes_erased"], 1)
        self.assertEqual(result["unshared_blobs_erased"], 1)
        self.assertEqual([r[0] for r in self.db.execute("SELECT id FROM accounts")], ["b"])
        self.assertEqual([r[0] for r in self.db.execute("SELECT id FROM messages")], ["message-b"])
        self.assertEqual([r[0] for r in self.db.execute("SELECT id FROM attachments")], ["shared-b"])
        self.assertTrue((self.root / "aa" / "aa" / self.shared).exists())
        self.assertFalse((self.root / "bb" / "bb" / self.private).exists())
        self.assertEqual(self.db.execute("SELECT account_id FROM apple_subscriptions WHERE original_transaction_id='purchase-a'").fetchone()[0], "erased_" + self.request)
        self.assertEqual(self.db.execute("SELECT account_id FROM apple_handle_plans WHERE original_transaction_id='plan-a'").fetchone()[0], "erased_" + self.request)
        self.assertEqual(self.db.execute("SELECT account_id FROM apple_plan_names WHERE namespace='name-a'").fetchone()[0], "erased_" + self.request)
        self.assertEqual(self.db.execute("SELECT provider_ref FROM namespaces WHERE namespace='name-a'").fetchone()[0], "plan:erased_" + self.request)
        self.assertEqual(self.db.execute("SELECT account_id FROM apple_handle_plans WHERE original_transaction_id='plan-b'").fetchone()[0], "b")
        self.assertEqual(self.db.execute("SELECT subject_hash FROM erased_member_subjects").fetchone()[0], hashlib.sha256(b"subject-a").digest())
        self.assertEqual(ops.pending(self.db), [])
        row = self.db.execute("SELECT oidc_sub,contact_address FROM account_deletion_requests").fetchone()
        self.assertEqual(tuple(row), ("", None))
        # Phase 10: the erased account's numbers, codes, invitations, discovery state and names
        # are gone; the other account's are untouched; the SMS ledger keeps an anonymous row.
        for table, column in (("phone_numbers", "account_id"), ("phone_challenges", "account_id"),
                              ("sms_invites", "sender_account"), ("discovery_usage", "account_id"),
                              ("discovery_marks", "account_id"), ("discovery_lockouts", "account_id"),
                              ("profile_names", "account_id")):
            self.assertEqual([r[0] for r in self.db.execute("SELECT " + column + " FROM " + table)], ["b"], table)
        for table, column in (("phone_numbers", "identity"), ("phone_challenges", "identity"), ("profile_names", "address")):
            self.assertEqual([r[0] for r in self.db.execute("SELECT " + column + " FROM " + table)], ["/k/b"], table)
        self.assertEqual(sorted(str(r[0]) for r in self.db.execute("SELECT account_id FROM sms_ledger")), ["None", "b"])
        self.assertEqual(self.db.execute("SELECT COUNT(*) FROM sms_optouts").fetchone()[0], 1)
        with self.assertRaises(ValueError):
            ops.erase(self.db, self.request, self.root, self.evidence)

    def test_external_evidence_is_required_before_any_erasure(self):
        self.evidence["identity_deleted"] = False
        with self.assertRaises(ValueError):
            ops.erase(self.db, self.request, self.root, self.evidence)
        self.assertEqual(self.db.execute("SELECT count(*) FROM accounts").fetchone()[0], 2)
        self.assertEqual(len(ops.pending(self.db)), 1)

    def test_changed_account_ownership_is_rejected(self):
        self.db.execute("UPDATE accounts SET oidc_sub='someone-else' WHERE id='a'")
        self.db.commit()
        with self.assertRaises(ValueError):
            ops.erase(self.db, self.request, self.root, self.evidence)
        self.assertEqual(self.db.execute("SELECT count(*) FROM messages").fetchone()[0], 2)
        self.assertTrue((self.root / "bb" / "bb" / self.private).exists())

    # ---- calendar (audit 2026-11-03 P2-17) ----------------------------------------------------

    def _calendar_tables(self):
        for table, cols in {
            "events": "id TEXT, status TEXT, span_to INTEGER, created_by_account TEXT, organizer TEXT, calendar_id TEXT",
            "event_attendees": "id TEXT, event_id TEXT, address TEXT",
            "event_occurrences": "id TEXT, event_id TEXT",
            "event_overrides": "occurrence_id TEXT",
            "event_occurrence_attendees": "occurrence_id TEXT, attendee_id TEXT",
            "event_join_tickets": "occurrence_id TEXT, attendee_id TEXT",
            "event_reminders": "event_id TEXT, owner_identity TEXT, attendee_id TEXT",
            "calendar_delivery_outbox": "id TEXT, event_id TEXT, workflow TEXT, state TEXT, account_id TEXT",
            "calendars": "id TEXT, owner_identity TEXT",
        }.items():
            self.db.execute("CREATE TABLE " + table + " (" + cols + ")")
        self.db.execute("INSERT INTO events VALUES ('ev1','confirmed',9999999999,'a','/k/a','cal1')")
        self.db.execute("INSERT INTO event_attendees VALUES ('att1','ev1','guest@example.test')")
        # Someone else's event is not the erased person's to cancel.
        self.db.execute("INSERT INTO events VALUES ('ev2','confirmed',9999999999,'b','/k/b','cal2')")
        self.db.commit()

    def test_upcoming_organized_events_block_erasure_until_their_cancellations_leave(self):
        self._calendar_tables()
        with self.assertRaises(ValueError) as caught:
            ops.erase(self.db, self.request, self.root, self.evidence)
        self.assertIn("--cancel-calendar", str(caught.exception))
        self.assertEqual(self.db.execute("SELECT count(*) FROM accounts").fetchone()[0], 2)
        # Cancelled through the postbox; the guest's notice is still in the outbox.
        self.db.execute("UPDATE events SET status='cancelled' WHERE id='ev1'")
        self.db.execute("INSERT INTO calendar_delivery_outbox VALUES ('d1','ev1','cancel','queued','a')")
        self.db.commit()
        with self.assertRaises(ValueError) as caught:
            ops.erase(self.db, self.request, self.root, self.evidence)
        self.assertIn("still waiting", str(caught.exception))
        # Handed to the provider: erasure proceeds and removes the event with its guest.
        self.db.execute("UPDATE calendar_delivery_outbox SET state='accepted'")
        self.db.commit()
        ops.erase(self.db, self.request, self.root, self.evidence)
        self.assertEqual([r[0] for r in self.db.execute("SELECT id FROM events")], ["ev2"])
        self.assertEqual(self.db.execute("SELECT count(*) FROM event_attendees").fetchone()[0], 0)

    def test_past_events_and_an_explicit_waiver_do_not_block(self):
        self._calendar_tables()
        self.evidence["calendar_cancellations_waived"] = True
        ops.erase(self.db, self.request, self.root, self.evidence)
        self.assertEqual([r[0] for r in self.db.execute("SELECT id FROM events")], ["ev2"])

    def test_events_that_already_ended_need_no_cancellation(self):
        self._calendar_tables()
        self.db.execute("UPDATE events SET span_to=5 WHERE id='ev1'")
        self.db.commit()
        ops.erase(self.db, self.request, self.root, self.evidence)
        self.assertEqual([r[0] for r in self.db.execute("SELECT id FROM events")], ["ev2"])

    def test_cancel_calendar_posts_to_the_loopback_internal_api_with_the_ops_token(self):
        token = self.root / "ops-token"
        token.write_text("svc_ops_example\n")
        seen = []

        class Reply:
            def __enter__(self):
                return self

            def __exit__(self, *exc):
                return False

            def read(self):
                return b'{"events_cancelled": 2, "pending_notices": 1}'

        def opener(req, timeout):
            seen.append(req)
            return Reply()

        result = ops.cancel_calendar(self.request, "http://127.0.0.1:8991", token, opener=opener)
        self.assertEqual(result, {"events_cancelled": 2, "pending_notices": 1})
        self.assertTrue(seen[0].full_url.endswith("/internal/v1/account-deletions/" + self.request + "/cancel-calendar"))
        self.assertEqual(seen[0].get_method(), "POST")
        self.assertEqual(seen[0].get_header("Authorization"), "Bearer svc_ops_example")
        for url, request in (("https://postbox.example", self.request), ("http://127.0.0.1:8991", "del_x")):
            with self.assertRaises(ValueError):
                ops.cancel_calendar(request, url, token, opener=opener)
        self.assertEqual(len(seen), 1)


if __name__ == "__main__":
    unittest.main()
