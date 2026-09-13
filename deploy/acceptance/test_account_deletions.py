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
            "namespaces": "namespace TEXT, account_id TEXT",
            "test_handle_claims": "account_id TEXT, namespace TEXT",
            "spam_reports": "reporter TEXT, sender TEXT",
            "reputation": "subject TEXT",
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
        self.assertEqual(self.db.execute("SELECT subject_hash FROM erased_member_subjects").fetchone()[0], hashlib.sha256(b"subject-a").digest())
        self.assertEqual(ops.pending(self.db), [])
        row = self.db.execute("SELECT oidc_sub,contact_address FROM account_deletion_requests").fetchone()
        self.assertEqual(tuple(row), ("", None))
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


if __name__ == "__main__":
    unittest.main()
