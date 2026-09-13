//! Durable, account-scoped requests for operator-assisted account erasure.

use super::{params, random_hex, OptionalExtension, Store, StoreError};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS account_deletion_requests (
    request_id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL UNIQUE,
    oidc_sub TEXT NOT NULL,
    contact_address TEXT,
    requested_at INTEGER NOT NULL,
    complete_by INTEGER NOT NULL,
    completed_at INTEGER
);
CREATE INDEX IF NOT EXISTS account_deletions_due
    ON account_deletion_requests(completed_at, complete_by);
CREATE TABLE IF NOT EXISTS erased_member_subjects (
    subject_hash BLOB PRIMARY KEY,
    erased_at INTEGER NOT NULL
);
";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DeletionRequest {
    pub request_id: String,
    pub requested_at: u64,
    pub complete_by: u64,
    pub completed_at: Option<u64>,
}

impl Store {
    /// Record consent only for the account bound to the validated member subject.
    /// Retries return the original receipt and cannot postpone its deadline.
    pub async fn request_account_deletion(
        &self,
        account_id: String,
        subject: String,
        contact_address: Option<String>,
        now: u64,
    ) -> Result<DeletionRequest, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<DeletionRequest, StoreError> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction()?;
            let owns = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM accounts WHERE id = ?1 AND oidc_sub = ?2)",
                params![account_id, subject],
                |r| r.get::<_, bool>(0),
            )?;
            if !owns {
                return Err(StoreError::Corrupt("deletion account ownership mismatch"));
            }
            tx.execute(
                "INSERT OR IGNORE INTO account_deletion_requests
                    (request_id, account_id, oidc_sub, contact_address, requested_at, complete_by)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    format!("del_{}", random_hex(16)),
                    account_id,
                    subject,
                    contact_address,
                    now as i64,
                    now.saturating_add(30 * 24 * 60 * 60) as i64,
                ],
            )?;
            let result = tx.query_row(
                "SELECT request_id, requested_at, complete_by, completed_at
                   FROM account_deletion_requests WHERE account_id = ?1",
                params![account_id],
                read_request,
            )?;
            tx.commit()?;
            Ok(result)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    pub async fn account_deletion_request(
        &self,
        account_id: String,
    ) -> Result<Option<DeletionRequest>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let c = conn.lock().expect("store lock");
            c.query_row(
                "SELECT request_id, requested_at, complete_by, completed_at
                   FROM account_deletion_requests WHERE account_id = ?1",
                params![account_id],
                read_request,
            )
            .optional()
            .map_err(StoreError::from)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }
}

fn read_request(r: &rusqlite::Row<'_>) -> rusqlite::Result<DeletionRequest> {
    Ok(DeletionRequest {
        request_id: r.get(0)?,
        requested_at: r.get::<_, i64>(1)? as u64,
        complete_by: r.get::<_, i64>(2)? as u64,
        completed_at: r.get::<_, Option<i64>>(3)?.map(|n| n as u64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn requests_are_owned_durable_and_retry_safe() {
        let dir = std::env::temp_dir().join(format!("pigeonpost-deletion-test-{}", random_hex(12)));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("store.db");
        let store = Store::open(path.to_str().unwrap()).unwrap();
        let account = store
            .account_for_sub("member-a".into(), "a".into(), 1)
            .await
            .unwrap();
        store
            .account_for_sub("member-b".into(), "b".into(), 1)
            .await
            .unwrap();
        assert!(store
            .request_account_deletion(account.clone(), "member-b".into(), None, 5)
            .await
            .is_err());
        let receipt = store
            .request_account_deletion(account.clone(), "member-a".into(), None, 10)
            .await
            .unwrap();
        let retry = store
            .request_account_deletion(account.clone(), "member-a".into(), None, 99)
            .await
            .unwrap();
        assert_eq!(receipt, retry);
        assert_eq!(receipt.complete_by, 10 + 30 * 24 * 60 * 60);
        assert!(store
            .account_deletion_request("b".into())
            .await
            .unwrap()
            .is_none());
        drop(store);
        let reopened = Store::open(path.to_str().unwrap()).unwrap();
        assert_eq!(
            reopened
                .account_deletion_request(account.clone())
                .await
                .unwrap(),
            Some(receipt)
        );
        // Requesting erasure is not a claim that data has already been erased.
        assert_eq!(
            reopened
                .account_for_sub("member-a".into(), "unused".into(), 100)
                .await
                .unwrap(),
            account
        );
        // The operator's final erasure leaves only a hash to reject still-valid old JWTs.
        use sha2::{Digest, Sha256};
        reopened
            .conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO erased_member_subjects VALUES (?1, ?2)",
                params![Sha256::digest(b"member-a").as_slice(), 101],
            )
            .unwrap();
        assert!(reopened
            .account_for_sub("member-a".into(), "resurrected".into(), 102)
            .await
            .is_err());
        drop(reopened);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
