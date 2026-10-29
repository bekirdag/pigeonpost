//! Operator purge of an **empty** account and its empty mailboxes (Phase 9 hardening).
//!
//! Throwaway accounts made by smoke tests and abandoned sign-ins left account rows behind, because
//! the only deletion path was the consented erasure in `deploy/account-deletions.py`, which needs
//! a member request. This is the narrow alternative: it removes an account only when nothing in it
//! is worth keeping or belongs to someone else.
//!
//! Every table in the schema is classified in [`TABLES`]; a test fails when a new table is added
//! without a decision, so later phases cannot silently leave account data behind.
//!
//! - [`Rule::Block`]: content, money, names, memberships or obligations to other people. Any
//!   matching row refuses the purge, and the report names the table and the count.
//! - [`Rule::Remove`]: residue that only describes the empty account (settings, revoked
//!   credentials, key packages, notification tokens, closed sessions).
//! - [`Rule::Keep`]: rows that mention the account but are retained on purpose (append-only audit,
//!   abuse evidence, other people's history).
//! - [`Rule::Unrelated`]: not account- or mailbox-scoped.
//!
//! The check and the deletion run in one `IMMEDIATE` transaction with the audit record, so a row
//! landing between the check and the delete cannot be lost.

use super::audit::{append_in, AuditRow};
use super::{params, Store, StoreError};

/// What a condition is matched against: the account id, or each of its mailbox addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Key {
    Account,
    Mailbox,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Rule {
    /// Refuse when `SELECT COUNT(*) FROM <table> WHERE <cond>` is non-zero (`?1` = the key).
    Block(Key, &'static str),
    /// Delete `WHERE <cond>` (`?1` = the key).
    Remove(Key, &'static str),
    /// Mentions the account or mailbox; kept, for the stated reason (asserted non-empty by the
    /// classification test; it exists for reviewers).
    #[cfg_attr(not(test), allow(dead_code))]
    Keep(&'static str),
    Unrelated,
}

use Key::{Account as A, Mailbox as M};
use Rule::{Block, Keep, Remove, Unrelated};

/// Every table, in the order removals run: mailbox residue, then account residue, then the
/// mailboxes and the account themselves.
pub(crate) const TABLES: &[(&str, Rule)] = &[
    // ---- blockers: content, membership, money, names, obligations ---------------------------
    ("messages", Block(M, "owner = ?1")),
    ("attachments", Block(M, "owner = ?1")),
    ("group_members", Block(M, "member = ?1")),
    ("group_messages", Block(M, "sender = ?1")),
    ("group_attachments", Block(M, "uploader = ?1")),
    ("event_attendees", Block(M, "address = ?1")),
    (
        "events",
        Block(
            M,
            "organizer = ?1 OR calendar_id IN (SELECT id FROM calendars WHERE owner_identity = ?1)",
        ),
    ),
    ("meeting_attendees", Block(M, "address = ?1")),
    ("org_members", Block(M, "identity = ?1")),
    ("org_agents", Block(M, "identity = ?1")),
    (
        "avatars",
        Block(M, "owner_kind = 'identity' AND owner_id = ?1"),
    ),
    (
        "account_deletion_requests",
        Block(A, "account_id = ?1 AND completed_at IS NULL"),
    ),
    // A verified phone number links a real-world identifier: not an empty account (SDS §14D).
    ("phone_numbers", Block(A, "account_id = ?1")),
    (
        "sms_ledger",
        Keep("SMS cost accounting; 35-day retention, anonymised by erasure"),
    ),
    ("namespaces", Block(A, "account_id = ?1")),
    ("apple_subscriptions", Block(A, "account_id = ?1")),
    ("google_subscriptions", Block(A, "account_id = ?1")),
    ("apple_handle_plans", Block(A, "account_id = ?1")),
    ("apple_plan_names", Block(A, "account_id = ?1")),
    ("plan_names", Block(A, "account_id = ?1")),
    ("test_handle_claims", Block(A, "account_id = ?1")),
    (
        "entitlements",
        Block(A, "subject_kind = 'account' AND subject_id = ?1"),
    ),
    (
        "orgs",
        Block(A, "owner_account = ?1 OR internal_account_id = ?1"),
    ),
    ("org_api_keys", Block(A, "created_by_account = ?1")),
    ("groups", Block(A, "created_by_account = ?1")),
    ("meetings", Block(A, "host_account = ?1")),
    ("meeting_notes_keys", Block(A, "account_id = ?1")),
    ("meeting_brief_keys", Block(A, "account_id = ?1")),
    ("calendar_delivery_outbox", Block(A, "account_id = ?1")),
    // `ssh_sessions` is both: a live session blocks, a closed one is residue (below).
    // ---- mailbox residue --------------------------------------------------------------------
    ("inbox_policy", Remove(M, "address = ?1")),
    ("workspace_context", Remove(M, "address = ?1")),
    ("message_meta", Remove(M, "owner = ?1")),
    ("threads", Remove(M, "owner = ?1")),
    ("archived_threads", Remove(M, "owner = ?1")),
    ("contacts", Remove(M, "owner = ?1")),
    ("call_log", Remove(M, "owner = ?1")),
    ("call_log_cleared", Remove(M, "owner = ?1")),
    ("receipt_settings", Remove(M, "mailbox = ?1")),
    ("calendar_settings", Remove(M, "mailbox = ?1")),
    ("calendar_import_drafts", Remove(M, "mailbox = ?1")),
    ("calendar_feed_grants", Remove(M, "exporter_identity = ?1")),
    ("event_reminders", Remove(M, "owner_identity = ?1")),
    ("calendars", Remove(M, "owner_identity = ?1")),
    ("mls_welcomes", Remove(M, "mailbox = ?1")),
    ("mls_kp_claims", Remove(M, "mailbox = ?1")),
    ("mls_key_packages", Remove(M, "mailbox = ?1")),
    ("mls_devices", Remove(M, "mailbox = ?1")),
    ("agent_shell_policy", Remove(M, "identity = ?1")),
    ("agent_shell_hosts", Remove(M, "identity = ?1")),
    ("device_credentials", Remove(M, "identity = ?1")),
    ("device_grants", Remove(M, "identity = ?1")),
    ("enrollments", Remove(M, "identity = ?1")),
    ("mint_events", Remove(M, "address = ?1")),
    ("profile_names", Remove(M, "address = ?1")),
    // ---- account residue --------------------------------------------------------------------
    ("api_keys", Remove(A, "account_id = ?1")),
    ("provider_identities", Remove(A, "account_id = ?1")),
    (
        "devices",
        Remove(
            A,
            "account = ?1 OR mailbox IN (SELECT address FROM identities WHERE account_id = ?1)",
        ),
    ),
    ("features", Remove(A, "scope = 'account' AND scope_id = ?1")),
    ("account_events", Remove(A, "account_id = ?1")),
    ("account_event_floor", Remove(A, "account_id = ?1")),
    ("ssh_sessions", Remove(A, "requester_account = ?1")),
    ("phone_challenges", Remove(A, "account_id = ?1")),
    ("sms_invites", Remove(A, "sender_account = ?1")),
    ("discovery_usage", Remove(A, "account_id = ?1")),
    ("discovery_marks", Remove(A, "account_id = ?1")),
    ("discovery_lockouts", Remove(A, "account_id = ?1")),
    ("identities", Remove(A, "account_id = ?1")),
    ("accounts", Remove(A, "id = ?1")),
    // ---- kept on purpose --------------------------------------------------------------------
    ("audit_log", Keep("append-only for two years by trigger")),
    (
        "spam_reports",
        Keep("abuse evidence; erasure decides retention"),
    ),
    (
        "reputation",
        Keep("abuse evidence; erasure decides retention"),
    ),
    (
        "avatar_reports",
        Keep("abuse evidence; erasure decides retention"),
    ),
    (
        "erased_member_subjects",
        Keep("erasure tombstones, not account data"),
    ),
    ("group_bans", Keep("group history, swept with the group")),
    ("group_leaves", Keep("group history, swept with the group")),
    (
        "group_receipts",
        Keep("group history, swept with the group"),
    ),
    (
        "group_handshakes",
        Keep("group history, swept with the group"),
    ),
    (
        "group_membership_intervals",
        Keep("group history, swept with the group"),
    ),
    ("group_stream_keys", Keep("stream rows, swept after 7 days")),
    (
        "group_stream_stage",
        Keep("stream rows, swept after 7 days"),
    ),
    ("group_streams", Keep("stream rows, swept after 7 days")),
    (
        "org_invites",
        Keep("org-scoped; needs org membership, which blocks"),
    ),
    (
        "org_rules",
        Keep("org-scoped; needs org membership, which blocks"),
    ),
    (
        "org_delegations",
        Keep("org-scoped; needs org membership, which blocks"),
    ),
    (
        "org_roles",
        Keep("org-scoped; needs org membership, which blocks"),
    ),
    (
        "webhooks",
        Keep("org-scoped; needs org membership, which blocks"),
    ),
    (
        "meeting_reservations",
        Keep("scheduled-call codes, retired with their meeting"),
    ),
    (
        "device_pairings",
        Keep("short-lived pairing requests; expire on their own"),
    ),
    // ---- not account-scoped -----------------------------------------------------------------
    ("account_event_meta", Unrelated),
    ("calendar_idempotency", Unrelated),
    ("call_events_seen", Unrelated),
    ("contact_suppressions", Unrelated),
    ("event_attendee_tokens", Unrelated),
    ("event_guest_challenges", Unrelated),
    ("event_guest_sessions", Unrelated),
    ("event_join_tickets", Unrelated),
    ("event_occurrence_attendees", Unrelated),
    ("event_occurrences", Unrelated),
    ("event_overrides", Unrelated),
    ("group_idempotency", Unrelated),
    ("group_notices", Unrelated),
    ("limiter_state", Unrelated),
    ("org_api_idempotency", Unrelated),
    ("org_domains", Unrelated),
    ("org_handle_holds", Unrelated),
    ("org_invite_deliveries", Unrelated),
    ("sms_optouts", Unrelated),
    ("sqlite_sequence", Unrelated),
    ("thread_backfill_ids", Unrelated),
    ("webhook_deliveries", Unrelated),
    ("webhook_events", Unrelated),
    ("webhook_outbox", Unrelated),
];

/// A live SSH session blocks; a closed one is residue.
const LIVE_SHELL: &str = "requester_account = ?1 AND state <> 'closed'";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PurgeReport {
    pub account_id: String,
    pub mailboxes: usize,
    /// `(table, rows)` that refuse the purge. Empty means it is (or was) allowed.
    pub blockers: Vec<(String, i64)>,
    /// `(table, rows)` removed, or that would be removed on a dry run.
    pub removed: Vec<(String, i64)>,
    pub purged: bool,
}

fn count(c: &rusqlite::Connection, table: &str, cond: &str, key: &str) -> Result<i64, StoreError> {
    // Table names and conditions are compile-time constants from `TABLES`; only `key` is bound.
    let sql = format!("SELECT COUNT(*) FROM {table} WHERE {cond}");
    Ok(c.query_row(&sql, params![key], |r| r.get(0))?)
}

impl Store {
    /// Check, and unless `dry_run` remove, an empty account and its mailboxes. `audit` is written
    /// in the same transaction, and only when something was actually removed.
    pub async fn purge_empty_account(
        &self,
        account_id: String,
        dry_run: bool,
        audit: AuditRow,
    ) -> Result<Option<PurgeReport>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<PurgeReport>, StoreError> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM accounts WHERE id = ?1)",
                params![account_id],
                |r| r.get(0),
            )?;
            if !exists {
                return Ok(None);
            }
            let mailboxes: Vec<String> = {
                let mut st = tx.prepare("SELECT address FROM identities WHERE account_id = ?1")?;
                let rows = st.query_map(params![account_id], |r| r.get::<_, String>(0))?;
                rows.collect::<Result<_, _>>()?
            };
            let sum = |table: &str, key: Key, cond: &str| -> Result<i64, StoreError> {
                match key {
                    Key::Account => count(&tx, table, cond, &account_id),
                    Key::Mailbox => mailboxes
                        .iter()
                        .try_fold(0, |n, m| Ok(n + count(&tx, table, cond, m)?)),
                }
            };
            let mut report = PurgeReport {
                account_id: account_id.clone(),
                mailboxes: mailboxes.len(),
                blockers: Vec::new(),
                removed: Vec::new(),
                purged: false,
            };
            for (table, rule) in TABLES {
                match rule {
                    Rule::Block(key, cond) => {
                        let n = sum(table, *key, cond)?;
                        if n > 0 {
                            report.blockers.push((table.to_string(), n));
                        }
                    }
                    Rule::Remove(key, cond) => {
                        let n = sum(table, *key, cond)?;
                        if n > 0 {
                            report.removed.push((table.to_string(), n));
                        }
                    }
                    Rule::Keep(_) | Rule::Unrelated => {}
                }
            }
            let live_shell = count(&tx, "ssh_sessions", LIVE_SHELL, &account_id)?;
            if live_shell > 0 {
                report.blockers.push(("ssh_sessions".into(), live_shell));
            }
            if dry_run || !report.blockers.is_empty() {
                return Ok(Some(report));
            }
            for (table, rule) in TABLES {
                if let Rule::Remove(key, cond) = rule {
                    let sql = format!("DELETE FROM {table} WHERE {cond}");
                    match key {
                        Key::Account => {
                            tx.execute(&sql, params![account_id])?;
                        }
                        Key::Mailbox => {
                            for m in &mailboxes {
                                tx.execute(&sql, params![m])?;
                            }
                        }
                    }
                }
            }
            let mut audit = audit;
            audit.details = serde_json::json!({
                "mailboxes": report.mailboxes,
                "removed": report.removed.iter().map(|(t, n)| serde_json::json!({"table": t, "rows": n})).collect::<Vec<_>>(),
                "reason": serde_json::from_str::<serde_json::Value>(&audit.details)
                    .ok()
                    .and_then(|v| v.get("reason").cloned()),
            })
            .to_string();
            append_in(&tx, &audit)?;
            tx.commit()?;
            report.purged = true;
            Ok(Some(report))
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Test hook: one integer from arbitrary SQL.
    #[cfg(test)]
    pub async fn scalar_raw(&self, sql: String) -> Result<i64, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<i64, StoreError> {
            let c = conn.lock().expect("store lock");
            Ok(c.query_row(&sql, [], |r| r.get(0))?)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    /// Every table name in the live schema (tests and the classification check).
    #[cfg(test)]
    pub async fn table_names(&self) -> Result<Vec<String>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<String>, StoreError> {
            let c = conn.lock().expect("store lock");
            let mut st =
                c.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")?;
            let rows = st.query_map([], |r| r.get::<_, String>(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audit() -> AuditRow {
        AuditRow {
            org_id: None,
            at: 1_000,
            actor_account: None,
            actor_kind: "system",
            action: "account.purge".into(),
            target: Some("account:acct_t".into()),
            details: r#"{"reason":"test_cleanup"}"#.into(),
            ip_hash: None,
            user_agent: None,
        }
    }

    #[tokio::test]
    async fn every_table_has_a_purge_decision() {
        let store = Store::open(":memory:").unwrap();
        let live = store.table_names().await.unwrap();
        let classified: Vec<&str> = TABLES.iter().map(|(t, _)| *t).collect();
        let missing: Vec<&String> = live
            .iter()
            .filter(|t| !classified.contains(&t.as_str()))
            .collect();
        assert!(
            missing.is_empty(),
            "classify these tables in store/purge.rs TABLES (and handle them in \
             deploy/account-deletions.py if they hold account data): {missing:?}"
        );
        let stale: Vec<&&str> = classified
            .iter()
            .filter(|t| !live.iter().any(|l| l == *t))
            .collect();
        assert!(
            stale.is_empty(),
            "TABLES names tables that do not exist: {stale:?}"
        );
        let mut seen = std::collections::HashSet::new();
        for (t, rule) in TABLES {
            assert!(seen.insert(*t), "{t} classified twice");
            if let Rule::Keep(reason) = rule {
                assert!(!reason.is_empty(), "{t}: say why it is kept");
            }
        }
    }

    /// Erasure must handle every table the purge treats as account data. The Python tool is the
    /// consented erasure path; this keeps the two from drifting apart.
    #[test]
    fn account_erasure_tool_covers_every_account_table() {
        let tool = include_str!("../../../../deploy/account-deletions.py");
        let missing: Vec<&str> = TABLES
            .iter()
            .filter(|(t, r)| {
                matches!(r, Rule::Remove(..) | Rule::Block(..))
                    && !tool.contains(&format!("\"{t}\""))
                    && !tool.contains(&format!("FROM {t} "))
                    && !tool.contains(&format!("UPDATE {t} "))
            })
            .map(|(t, _)| *t)
            .collect();
        assert!(
            missing.is_empty(),
            "deploy/account-deletions.py does not handle: {missing:?}"
        );
    }

    async fn seed(store: &Store) {
        for sql in [
            "INSERT INTO accounts (id, created_at, oidc_sub) VALUES ('acct_t', 1, 'sub-t')",
            "INSERT INTO identities (address, ed25519_pub, x25519_pub, wrapped_nonce, wrapped_ct, cap_hash, created_at, account_id)
             VALUES ('/k/empty', x'00', x'00', x'00', x'00', x'00', 1, 'acct_t')",
            "INSERT INTO inbox_policy (address, updated_at) VALUES ('/k/empty', 1)",
            "INSERT INTO features (name, scope, scope_id, enabled, updated_at, updated_by)
             VALUES ('account_events', 'account', 'acct_t', 1, 1, 'test')",
            "INSERT INTO accounts (id, created_at, oidc_sub) VALUES ('acct_other', 1, 'sub-o')",
        ] {
            store.exec_raw(sql).await.unwrap();
        }
    }

    #[tokio::test]
    async fn dry_run_reports_and_purge_removes_only_that_account() {
        let store = Store::open(":memory:").unwrap();
        seed(&store).await;
        let dry = store
            .purge_empty_account("acct_t".into(), true, audit())
            .await
            .unwrap()
            .unwrap();
        assert!(!dry.purged && dry.blockers.is_empty() && dry.mailboxes == 1);
        assert!(dry.removed.iter().any(|(t, _)| t == "inbox_policy"));
        let done = store
            .purge_empty_account("acct_t".into(), false, audit())
            .await
            .unwrap()
            .unwrap();
        assert!(done.purged);
        assert!(store
            .purge_empty_account("acct_t".into(), true, audit())
            .await
            .unwrap()
            .is_none());
        assert!(store
            .purge_empty_account("acct_other".into(), true, audit())
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn content_refuses_the_purge_and_changes_nothing() {
        let store = Store::open(":memory:").unwrap();
        seed(&store).await;
        store
            .exec_raw(
                "INSERT INTO namespaces (namespace, account_id, source, verified_at) VALUES ('held', 'acct_t', 'test', 1)",
            )
            .await
            .unwrap();
        let r = store
            .purge_empty_account("acct_t".into(), false, audit())
            .await
            .unwrap()
            .unwrap();
        assert!(!r.purged);
        assert_eq!(r.blockers, vec![("namespaces".to_string(), 1)]);
        let again = store
            .purge_empty_account("acct_t".into(), true, audit())
            .await
            .unwrap()
            .unwrap();
        assert!(again.removed.iter().any(|(t, _)| t == "identities"));
    }

    /// The consented erasure tool, run for real against the real schema: every Phase 1b–8 table
    /// it now handles loses the erased account's rows, and the other account keeps its own.
    #[tokio::test]
    async fn erasure_tool_clears_new_tables_against_the_real_schema() {
        if std::process::Command::new("python3")
            .arg("-V")
            .output()
            .is_err()
        {
            eprintln!("python3 missing; skipped");
            return;
        }
        let dir = std::env::temp_dir().join(format!("pp-erase-{}", crate::rand_hex(6)));
        std::fs::create_dir_all(dir.join("blobs/avatars/cc/cc")).unwrap();
        let digest = "c".repeat(64);
        std::fs::write(dir.join("blobs/avatars/cc/cc").join(&digest), b"x").unwrap();
        let db = dir.join("p.db");
        let store = Store::open(db.to_str().unwrap()).unwrap();
        for sql in [
            "INSERT INTO accounts (id, created_at, oidc_sub) VALUES ('acct_e', 1, 'sub-e')",
            "INSERT INTO accounts (id, created_at, oidc_sub) VALUES ('acct_k', 1, 'sub-k')",
            "INSERT INTO identities (address, ed25519_pub, x25519_pub, wrapped_nonce, wrapped_ct, cap_hash, created_at, account_id)
             VALUES ('/k/erase', x'00', x'00', x'00', x'00', x'00', 1, 'acct_e')",
            "INSERT INTO identities (address, ed25519_pub, x25519_pub, wrapped_nonce, wrapped_ct, cap_hash, created_at, account_id)
             VALUES ('/k/keep', x'00', x'00', x'00', x'00', x'00', 1, 'acct_k')",
            "INSERT INTO call_log (owner, room_id, started_at) VALUES ('/k/erase', 'r1', 1), ('/k/keep', 'r1', 1)",
            "INSERT INTO avatars (id, owner_kind, owner_id, sha256, variants, created_at)
             VALUES ('av1', 'identity', '/k/erase', x'00', '{\"256\":\"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc\"}', 1)",
            "INSERT INTO calendars (id, owner_identity, name, created_at) VALUES ('cal_e', '/k/erase', 'c', 1)",
            "INSERT INTO events (id, calendar_id, organizer, title_sealed, value_type, start_date, end_date_exclusive, created_at, updated_at, span_from)
             VALUES ('ev_e', 'cal_e', '/k/erase', x'00', 'date', '2026-11-01', '2026-11-02', 1, 1, 1)",
            "INSERT INTO event_attendees (id, event_id, kind, address, added_at) VALUES ('at_k', 'ev_e', 'handle', '/k/keep', 1)",
            "INSERT INTO event_occurrences (id, event_id, recurrence_key, event_revision, status) VALUES ('oc_e', 'ev_e', 'k', 1, 'confirmed')",
            "INSERT INTO groups (id, name, created_by, created_by_account, created_at, last_at) VALUES ('g1', 'g', '/k/keep', 'acct_k', 1, 1)",
            "INSERT INTO group_members (group_id, member, role, state, updated_at) VALUES ('g1', '/k/erase', 'member', 'active', 1), ('g1', '/k/keep', 'owner', 'active', 1)",
            "INSERT INTO features (name, scope, scope_id, enabled, updated_at) VALUES ('account_events', 'account', 'acct_e', 1, 1)",
            "INSERT INTO ssh_sessions (id, agent, requester_account, via, mode, state, client_key, client_fp, host_fp, cols, rows, created_at, ends_by)
             VALUES ('s1', '/k/a', 'acct_e', 'owner', 'container', 'closed', 'k', 'f', 'h', 80, 24, 1, 2)",
            "INSERT INTO account_deletion_requests (request_id, account_id, oidc_sub, requested_at, complete_by)
             VALUES ('del_dddddddddddddddddddddddddddddddd', 'acct_e', 'sub-e', 1, 2)",
        ] {
            store.exec_raw(sql).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
        }
        let evidence = dir.join("evidence.json");
        let mut ev = serde_json::json!({"request_id": "del_dddddddddddddddddddddddddddddddd"});
        for k in [
            "identity_deleted",
            "sessions_revoked",
            "apple_revoke_reviewed",
            "billing_retention_reviewed",
            "registry_retention_reviewed",
            "services_stopped",
            "completion_notice_arranged",
            "backup_expiry_recorded",
        ] {
            ev[k] = serde_json::Value::Bool(true);
        }
        std::fs::write(&evidence, ev.to_string()).unwrap();
        let tool = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../deploy/account-deletions.py"
        );
        let out = std::process::Command::new("python3")
            .args([
                tool,
                "--erase",
                "del_dddddddddddddddddddddddddddddddd",
                "--evidence",
            ])
            .arg(&evidence)
            .arg("--db")
            .arg(&db)
            .arg("--blobs")
            .arg(dir.join("blobs"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let n = |sql: &str| {
            let store = &store;
            let sql = sql.to_string();
            async move { store.scalar_raw(sql).await.unwrap() }
        };
        assert_eq!(
            n("SELECT COUNT(*) FROM accounts WHERE id='acct_e'").await,
            0
        );
        assert_eq!(
            n("SELECT COUNT(*) FROM call_log WHERE owner='/k/erase'").await,
            0
        );
        assert_eq!(
            n("SELECT COUNT(*) FROM call_log WHERE owner='/k/keep'").await,
            1
        );
        assert_eq!(
            n("SELECT COUNT(*) FROM avatars WHERE owner_id='/k/erase'").await,
            0
        );
        assert!(!dir.join("blobs/avatars/cc/cc").join(&digest).exists());
        assert_eq!(n("SELECT COUNT(*) FROM events").await, 0);
        assert_eq!(n("SELECT COUNT(*) FROM event_attendees").await, 0);
        assert_eq!(n("SELECT COUNT(*) FROM event_occurrences").await, 0);
        assert_eq!(n("SELECT COUNT(*) FROM calendars").await, 0);
        assert_eq!(
            n("SELECT COUNT(*) FROM group_members WHERE member='/k/erase'").await,
            0
        );
        assert_eq!(
            n("SELECT COUNT(*) FROM group_members WHERE member='/k/keep'").await,
            1
        );
        assert_eq!(
            n("SELECT COUNT(*) FROM features WHERE scope_id='acct_e'").await,
            0
        );
        assert_eq!(n("SELECT COUNT(*) FROM ssh_sessions").await, 0);
        assert_eq!(
            n("SELECT COUNT(*) FROM identities WHERE address='/k/keep'").await,
            1
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
