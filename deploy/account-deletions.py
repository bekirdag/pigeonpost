#!/usr/bin/env python3
"""Private operator queue and final postbox erasure; see account-deletions.md.

Default operation is read-only. Never expose this script as a public HTTP handler.
External identity/billing/provider work is reviewed manually before --erase.
"""
import argparse
import datetime
import hashlib
import json
from pathlib import Path
import re
import sqlite3
import subprocess
import time
from email.message import EmailMessage


def pending(conn):
    return [dict(r) for r in conn.execute(
        "SELECT request_id, requested_at, complete_by FROM account_deletion_requests "
        "WHERE completed_at IS NULL ORDER BY complete_by")]


def erase(conn, request_id, blob_root, evidence):
    """Erase only a consented account's copies; services must be stopped by the operator."""
    if not re.fullmatch(r"del_[a-f0-9]{32}", request_id):
        raise ValueError("Invalid request reference")
    required = ("identity_deleted", "sessions_revoked", "apple_revoke_reviewed",
                "billing_retention_reviewed", "registry_retention_reviewed", "services_stopped",
                "completion_notice_arranged", "backup_expiry_recorded")
    if evidence.get("request_id") != request_id or not all(evidence.get(k) is True for k in required):
        raise ValueError("Complete the external fulfillment evidence for this request first")
    root = Path(blob_root).resolve(strict=True)
    now = int(time.time())
    conn.execute("BEGIN IMMEDIATE")
    try:
        r = conn.execute("SELECT * FROM account_deletion_requests WHERE request_id=?", (request_id,)).fetchone()
        if r is None or r["completed_at"] is not None:
            raise ValueError("No pending confirmed request")
        account, subject = r["account_id"], r["oidc_sub"]
        if not subject or conn.execute("SELECT id FROM accounts WHERE id=? AND oidc_sub=?", (account, subject)).fetchone() is None:
            raise ValueError("Request no longer matches account ownership")
        addresses = [v[0] for v in conn.execute("SELECT address FROM identities WHERE account_id=?", (account,))]
        orphan_candidates = set()
        for address in addresses:
            orphan_candidates.update(v[0] for v in conn.execute("SELECT sha256 FROM attachments WHERE owner=?", (address,)))
            for table in ("attachments", "messages", "threads", "contacts", "archived_threads"):
                conn.execute("DELETE FROM " + table + " WHERE owner=?", (address,))
            for table in ("inbox_policy", "workspace_context"):
                conn.execute("DELETE FROM " + table + " WHERE address=?", (address,))
            conn.execute("DELETE FROM devices WHERE mailbox=?", (address,))
            conn.execute("DELETE FROM mint_events WHERE address=?", (address,))
        conn.execute("DELETE FROM devices WHERE account=?", (account,))
        for table in ("api_keys", "provider_identities", "identities"):
            conn.execute("DELETE FROM " + table + " WHERE account_id=?", (account,))
        # Keep purchase uniqueness and the current public lease from being reassigned prematurely.
        # These rows no longer identify or authenticate the erased person. Retention is reviewed
        # separately; app-store transaction IDs remain necessary for refund/replay handling.
        for table in ("apple_subscriptions", "google_subscriptions", "namespaces", "test_handle_claims"):
            conn.execute("UPDATE " + table + " SET account_id=? WHERE account_id=?", ("erased_" + request_id, account))
        conn.execute("INSERT OR IGNORE INTO erased_member_subjects VALUES (?,?)", (hashlib.sha256(subject.encode()).digest(), now))
        conn.execute("DELETE FROM accounts WHERE id=?", (account,))
        # Report/evidence records are retained only where the operator identified a security need.
        if not evidence.get("retain_abuse_evidence", False):
            for address in addresses:
                conn.execute("DELETE FROM spam_reports WHERE reporter=? OR sender=?", (address, address))
                conn.execute("DELETE FROM reputation WHERE subject=?", (address,))
        orphaned = [sha for sha in orphan_candidates if conn.execute("SELECT 1 FROM attachments WHERE sha256=? LIMIT 1", (sha,)).fetchone() is None]
        for sha in orphaned:
            if not re.fullmatch(r"[a-f0-9]{64}", sha):
                raise ValueError("Invalid blob digest; stop for operator inspection")
        # Keep the request pending until every unshared blob is also removed. A failed unlink
        # rolls back SQLite; the offline operator can retry after fixing storage permissions.
        for sha in orphaned:
            blob = root / sha[:2] / sha[2:4] / sha
            if root not in blob.resolve().parents:
                raise ValueError("Blob path escapes storage")
            try:
                blob.unlink()
            except FileNotFoundError:
                pass
        conn.execute("UPDATE account_deletion_requests SET completed_at=?,oidc_sub='',contact_address=NULL WHERE request_id=?", (now, request_id))
        conn.commit()
    except BaseException:
        conn.rollback()
        raise
    return {"request_id": request_id, "completed_at": now, "mailboxes_erased": len(addresses), "unshared_blobs_erased": len(orphaned)}


def notify(rows, recipient, sendmail):
    if not rows:
        return
    if not re.fullmatch(r"[^\s@<>]+@[^\s@<>]+", recipient):
        raise ValueError("A single operator mailbox is required")
    message = EmailMessage()
    message["From"] = "Pigeonpost Privacy <privacy@pigeonpost.dev>"
    message["To"] = recipient
    message["Subject"] = "Pigeonpost: account deletion requests awaiting fulfillment"
    lines = ["The authenticated deletion queue contains confirmed requests. Fulfill each by its deadline using deploy/account-deletions.md. Do not treat submission as completed erasure.", ""]
    for row in rows:
        date = datetime.datetime.fromtimestamp(row["complete_by"], datetime.timezone.utc).isoformat()
        lines.append("{} — complete by {}".format(row["request_id"], date))
    message.set_content("\n".join(lines))
    subprocess.run([sendmail, "-t", "-oi"], input=message.as_bytes(), check=True, timeout=30)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--db", required=True)
    p.add_argument("--notify-to")
    p.add_argument("--sendmail", default="/usr/sbin/sendmail")
    p.add_argument("--erase", metavar="REQUEST_ID")
    p.add_argument("--evidence", type=Path)
    p.add_argument("--blobs", type=Path)
    args = p.parse_args()
    db = Path(args.db).resolve(strict=True)
    conn = sqlite3.connect(db.as_uri() + ("?mode=rw" if args.erase else "?mode=ro"), uri=True, timeout=10)
    conn.row_factory = sqlite3.Row
    try:
        if args.erase:
            if not args.evidence or not args.blobs or args.notify_to:
                p.error("--erase requires --evidence and --blobs and cannot be combined with notifications")
            print(json.dumps(erase(conn, args.erase, args.blobs, json.loads(args.evidence.read_text()))))
        else:
            rows = pending(conn)
            print(json.dumps({"pending": rows}))
            if args.notify_to:
                notify(rows, args.notify_to, args.sendmail)
    finally:
        conn.close()


if __name__ == "__main__":
    main()
