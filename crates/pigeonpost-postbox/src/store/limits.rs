//! One row holding the rate limiters' state across restarts (`crate::limits`).

use super::{params, OptionalExtension, Store, StoreError};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS limiter_state (
    id       INTEGER PRIMARY KEY CHECK (id = 1),
    saved_at INTEGER NOT NULL,
    data     TEXT NOT NULL
);
";

impl Store {
    pub async fn save_limiter_state(&self, data: String, now: u64) -> Result<(), StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<(), StoreError> {
            let c = conn.lock().expect("store lock");
            c.execute(
                "INSERT INTO limiter_state (id, saved_at, data) VALUES (1, ?1, ?2)
                 ON CONFLICT(id) DO UPDATE SET saved_at = excluded.saved_at, data = excluded.data",
                params![now as i64, data],
            )?;
            Ok(())
        })
        .await
        .map_err(|_| StoreError::Join)?
    }

    pub async fn load_limiter_state(&self) -> Result<Option<(u64, String)>, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<(u64, String)>, StoreError> {
            let c = conn.lock().expect("store lock");
            Ok(c.query_row(
                "SELECT saved_at, data FROM limiter_state WHERE id = 1",
                [],
                |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, String>(1)?)),
            )
            .optional()?)
        })
        .await
        .map_err(|_| StoreError::Join)?
    }
}
