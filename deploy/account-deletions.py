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


def has(conn, table):
    return conn.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?", (table,)).fetchone() is not None


# Mailbox-scoped rows that only describe the erased person's own mailboxes (Phase 1b-8 tables).
# Each entry is (table, column holding the mailbox address). Absent tables are skipped.
MAILBOX_ROWS = (
    ("message_meta", "owner"), ("call_log", "owner"), ("call_log_cleared", "owner"),
    ("receipt_settings", "mailbox"), ("calendar_settings", "mailbox"),
    ("calendar_import_drafts", "mailbox"), ("calendar_feed_grants", "exporter_identity"),
    ("mls_welcomes", "mailbox"), ("mls_kp_claims", "mailbox"), ("mls_key_packages", "mailbox"),
    ("mls_devices", "mailbox"), ("agent_shell_policy", "identity"), ("agent_shell_hosts", "identity"),
    ("device_credentials", "identity"), ("device_grants", "identity"), ("enrollments", "identity"),
    ("group_receipts", "member"), ("group_leaves", "mailbox"),
)

# Rows keyed by an attendee id; removed with the attendee.
ATTENDEE_ROWS = ("event_occurrence_attendees", "event_attendee_tokens", "event_guest_challenges",
                 "event_guest_sessions", "event_join_tickets", "event_reminders")


def erase_calendar(conn, addresses, account):
    """Events the person organized go entirely; their attendance elsewhere is removed."""
    if not has(conn, "events"):
        return
    marks = ",".join("?" * len(addresses)) or "''"
    events = [r[0] for r in conn.execute(
        "SELECT id FROM events WHERE created_by_account=? OR organizer IN (" + marks + ") OR calendar_id IN "
        "(SELECT id FROM calendars WHERE owner_identity IN (" + marks + "))", (account, *addresses, *addresses))]
    attendees = [r[0] for r in conn.execute(
        "SELECT id FROM event_attendees WHERE address IN (" + marks + ")", tuple(addresses))]
    for event in events:
        attendees += [r[0] for r in conn.execute("SELECT id FROM event_attendees WHERE event_id=?", (event,))]
    for attendee in set(attendees):
        for table in ATTENDEE_ROWS:
            if has(conn, table):
                conn.execute("DELETE FROM " + table + " WHERE attendee_id=?", (attendee,))
        conn.execute("DELETE FROM event_attendees WHERE id=?", (attendee,))
    for event in events:
        occurrences = [r[0] for r in conn.execute("SELECT id FROM event_occurrences WHERE event_id=?", (event,))]
        for occurrence in occurrences:
            conn.execute("DELETE FROM event_overrides WHERE occurrence_id=?", (occurrence,))
            conn.execute("DELETE FROM event_occurrence_attendees WHERE occurrence_id=?", (occurrence,))
            conn.execute("DELETE FROM event_join_tickets WHERE occurrence_id=?", (occurrence,))
        conn.execute("DELETE FROM event_occurrences WHERE event_id=?", (event,))
        conn.execute("DELETE FROM event_reminders WHERE event_id=?", (event,))
        conn.execute("DELETE FROM calendar_delivery_outbox WHERE event_id=?", (event,))
        conn.execute("DELETE FROM events WHERE id=?", (event,))
    conn.execute("DELETE FROM calendar_delivery_outbox WHERE account_id=?", (account,))
    for address in addresses:
        conn.execute("DELETE FROM event_reminders WHERE owner_identity=?", (address,))
        conn.execute("DELETE FROM calendars WHERE owner_identity=?", (address,))


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
        # Organizations own their members' org data and audit trail; offboard first (portal or
        # org API), so ownership transfer and the org's own records are decided by the org.
        marks = ",".join("?" * len(addresses)) or "''"
        if has(conn, "orgs") and conn.execute(
                "SELECT 1 FROM orgs WHERE owner_account=? OR internal_account_id=?", (account, account)).fetchone():
            raise ValueError("Account owns an organization; transfer or close it first")
        if has(conn, "org_members") and conn.execute(
                "SELECT 1 FROM org_members WHERE status <> 'removed' AND (account_id=? OR identity IN (" + marks + "))",
                (account, *addresses)).fetchone():
            raise ValueError("Account is an active organization member; offboard it first")
        if has(conn, "org_agents") and conn.execute(
                "SELECT 1 FROM org_agents WHERE deleted_at IS NULL AND identity IN (" + marks + ")", tuple(addresses)).fetchone():
            raise ValueError("A mailbox is a live organization agent; remove it from the organization first")
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
        for address in addresses:
            for table, column in MAILBOX_ROWS:
                if has(conn, table):
                    conn.execute("DELETE FROM " + table + " WHERE " + column + "=?", (address,))
            # Groups: the person leaves. Ciphertext they sent stays with the other members, as
            # delivered direct-message copies do; membership, receipts and keys go.
            if has(conn, "group_members"):
                conn.execute("DELETE FROM group_members WHERE member=?", (address,))
                conn.execute("DELETE FROM group_membership_intervals WHERE member=?", (address,))
                conn.execute("DELETE FROM group_stream_keys WHERE member=?", (address,))
                conn.execute("DELETE FROM group_stream_stage WHERE member=?", (address,))
                for table in ("group_messages", "group_attachments"):
                    column = "sender" if table == "group_messages" else "uploader"
                    conn.execute("UPDATE " + table + " SET " + column + "='erased' WHERE " + column + "=?", (address,))
            if has(conn, "org_agents"):
                conn.execute("DELETE FROM org_agents WHERE identity=? AND deleted_at IS NOT NULL", (address,))
            if has(conn, "org_members"):
                conn.execute("UPDATE org_members SET identity=NULL WHERE identity=? AND status='removed'", (address,))
        if has(conn, "org_members"):
            conn.execute("UPDATE org_members SET account_id=? WHERE account_id=? AND status='removed'",
                         ("erased_" + request_id, account))
        if has(conn, "groups"):
            conn.execute("UPDATE groups SET created_by_account=? WHERE created_by_account=?", ("erased_" + request_id, account))
        if has(conn, "org_api_keys"):
            # Org keys belong to the org; with the creator gone they fail its live-role check.
            conn.execute("UPDATE org_api_keys SET created_by_account=? WHERE created_by_account=?", ("erased_" + request_id, account))
        erase_calendar(conn, addresses, account)
        avatar_digests = set()
        if has(conn, "avatars"):
            for address in addresses:
                for (variants,) in conn.execute("SELECT variants FROM avatars WHERE owner_kind='identity' AND owner_id=?", (address,)):
                    avatar_digests.update(json.loads(variants or "{}").values())
                conn.execute("DELETE FROM avatars WHERE owner_kind='identity' AND owner_id=?", (address,))
        for table, column, value in (("features", "scope_id", account), ("account_events", "account_id", account),
                                     ("account_event_floor", "account_id", account),
                                     ("ssh_sessions", "requester_account", account)):
            if has(conn, table):
                extra = " AND scope='account'" if table == "features" else ""
                conn.execute("DELETE FROM " + table + " WHERE " + column + "=?" + extra, (value,))
        # Billing records are kept for refunds and law, no longer naming the person.
        if has(conn, "entitlements"):
            conn.execute("UPDATE entitlements SET subject_id=? WHERE subject_kind='account' AND subject_id=?",
                         ("erased_" + request_id, account))
        if has(conn, "plan_names"):
            conn.execute("UPDATE plan_names SET account_id=? WHERE account_id=?", ("erased_" + request_id, account))
        for table in ("api_keys", "provider_identities", "identities"):
            conn.execute("DELETE FROM " + table + " WHERE account_id=?", (account,))
        # Keep purchase uniqueness and the current public lease from being reassigned prematurely.
        # These rows no longer identify or authenticate the erased person. Retention is reviewed
        # separately; app-store transaction IDs remain necessary for refund/replay handling.
        for table in ("apple_subscriptions", "google_subscriptions", "namespaces", "test_handle_claims"):
            conn.execute("UPDATE " + table + " SET account_id=? WHERE account_id=?", ("erased_" + request_id, account))
        # Additive plan tables may be absent on an older deployment.
        for table in ("apple_handle_plans", "apple_plan_names"):
            if conn.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?", (table,)).fetchone():
                conn.execute("UPDATE " + table + " SET account_id=? WHERE account_id=?", ("erased_" + request_id, account))
        conn.execute("UPDATE namespaces SET provider_ref=? WHERE provider_ref=?", ("plan:erased_" + request_id, "plan:" + account))
        # Meeting notes (Phase 7): the account's notes keys, its wrapped copies and its attendance go.
        # A meeting it hosted keeps the other attendees' copies but no longer names the host.
        if conn.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name='meeting_notes_keys'").fetchone():
            for table in ("meeting_notes_keys", "meeting_brief_keys", "meeting_attendees"):
                conn.execute("DELETE FROM " + table + " WHERE account_id=?", (account,))
            conn.execute("UPDATE meetings SET host_account=?, host_address='erased' WHERE host_account=?", ("erased_" + request_id, account))
        conn.execute("INSERT OR IGNORE INTO erased_member_subjects VALUES (?,?)", (hashlib.sha256(subject.encode()).digest(), now))
        conn.execute("DELETE FROM accounts WHERE id=?", (account,))
        # Report/evidence records are retained only where the operator identified a security need.
        if not evidence.get("retain_abuse_evidence", False):
            for address in addresses:
                conn.execute("DELETE FROM spam_reports WHERE reporter=? OR sender=?", (address, address))
                conn.execute("DELETE FROM reputation WHERE subject=?", (address,))
        orphaned = [sha for sha in orphan_candidates if conn.execute("SELECT 1 FROM attachments WHERE sha256=? LIMIT 1", (sha,)).fetchone() is None]
        orphaned_avatars = [d for d in avatar_digests
                            if conn.execute("SELECT 1 FROM avatars WHERE instr(variants, ?) > 0 LIMIT 1", (d,)).fetchone() is None]
        for sha in orphaned + orphaned_avatars:
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
        avatar_root = root / "avatars"
        for sha in orphaned_avatars:
            blob = avatar_root / sha[:2] / sha[2:4] / sha
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
