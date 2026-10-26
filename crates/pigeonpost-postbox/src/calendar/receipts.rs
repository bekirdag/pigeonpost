//! Creation replies are custodial calendar content, including when retained for safe retries.

use super::*;
use rusqlite::types::ValueRef;

const MIGRATION_BATCH: usize = 64;

enum SavedResponse {
    Legacy(Vec<u8>),
    Sealed(Vec<u8>),
    Invalid,
}

fn saved_response(row: &rusqlite::Row<'_>, column: usize) -> rusqlite::Result<SavedResponse> {
    Ok(match row.get_ref(column)? {
        ValueRef::Text(bytes) => SavedResponse::Legacy(bytes.to_vec()),
        ValueRef::Blob(bytes) => SavedResponse::Sealed(bytes.to_vec()),
        _ => SavedResponse::Invalid,
    })
}

fn unavailable() -> GErr {
    GErr::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "calendar_receipt_unavailable",
        "the saved calendar result is unavailable; retry later",
    )
}

fn aad(scope: &str, action: &str, key: &str, hash: &str, status: i64, created_at: i64) -> String {
    // JSON framing prevents delimiter ambiguity; the prefix separates this from other fields.
    format!(
        "calendar-creation-receipt-v1:{}",
        json!([scope, action, key, hash, status, created_at])
    )
}

fn seal(vault: &crate::vault::Vault, context: &str, bytes: &[u8]) -> GResult<Vec<u8>> {
    vault
        .seal_labeled(SEAL_LABEL, context.as_bytes(), bytes)
        .map_err(|_| {
            GErr::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store_error",
                "seal failed",
            )
        })
}

/// Replay inside the caller's transaction, keeping the original response and suppressing effects.
#[allow(clippy::too_many_arguments)]
pub(crate) fn idempotent(
    c: &Connection,
    vault: &crate::vault::Vault,
    scope: &str,
    action: &str,
    key: &str,
    body_hash: &str,
    now: u64,
    f: impl FnOnce() -> GResult<(u16, Value, Effects)>,
) -> GResult<(u16, Value, Effects)> {
    if !crate::groups::valid_key(key) {
        return Err(GErr::bad(
            "bad_request",
            "request_key must be 1-64 characters of A-Z a-z 0-9 _ -",
        ));
    }
    let prior = c
        .query_row(
            "SELECT body_hash, status, response, created_at FROM calendar_idempotency
              WHERE scope = ?1 AND action = ?2 AND request_key = ?3 AND created_at > ?4",
            params![
                scope,
                action,
                key,
                now.saturating_sub(IDEMPOTENCY_SECS) as i64
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    saved_response(row, 2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?;
    if let Some((hash, status, saved, created_at)) = prior {
        if hash != body_hash {
            return Err(GErr::conflict(
                "idempotency_conflict",
                "that request_key was used with a different request",
            ));
        }
        let code = u16::try_from(status).map_err(|_| unavailable())?;
        if !(100..600).contains(&code) {
            return Err(unavailable());
        }
        let context = aad(scope, action, key, &hash, status, created_at);
        let (bytes, legacy) = match saved {
            SavedResponse::Legacy(bytes) => (bytes, true),
            SavedResponse::Sealed(blob) => (
                vault
                    .open_labeled(SEAL_LABEL, context.as_bytes(), &blob)
                    .map_err(|_| unavailable())?,
                false,
            ),
            SavedResponse::Invalid => return Err(unavailable()),
        };
        let response = serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
        if legacy {
            let encrypted = seal(vault, &context, &bytes)?;
            c.execute(
                "UPDATE calendar_idempotency SET response = ?1
                  WHERE scope = ?2 AND action = ?3 AND request_key = ?4
                    AND body_hash = ?5 AND status = ?6 AND created_at = ?7",
                params![encrypted, scope, action, key, hash, status, created_at],
            )?;
        }
        return Ok((code, response, Effects::default()));
    }
    let out = f()?;
    let created_at = now as i64;
    let response = seal(
        vault,
        &aad(scope, action, key, body_hash, i64::from(out.0), created_at),
        out.1.to_string().as_bytes(),
    )?;
    c.execute(
        "INSERT OR REPLACE INTO calendar_idempotency
            (scope, action, request_key, body_hash, status, response, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            scope,
            action,
            key,
            body_hash,
            i64::from(out.0),
            response,
            created_at
        ],
    )?;
    Ok(out)
}

struct LegacyReceipt {
    scope: String,
    action: String,
    key: String,
    hash: String,
    status: i64,
    response: Vec<u8>,
    created_at: i64,
}

fn seal_legacy_batch(
    c: &Connection,
    vault: &crate::vault::Vault,
    persistent_master_key: bool,
) -> GResult<usize> {
    let rows = {
        let mut statement = c.prepare(
            "SELECT scope, action, request_key, body_hash, status, response, created_at
               FROM calendar_idempotency
              WHERE action IN ('create_calendar', 'create_event') AND typeof(response) = 'text'
              ORDER BY scope, action, request_key LIMIT ?1",
        )?;
        let values = statement.query_map(params![MIGRATION_BATCH as i64], |row| {
            let response = match saved_response(row, 5)? {
                SavedResponse::Legacy(bytes) => bytes,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            Ok(LegacyReceipt {
                scope: row.get(0)?,
                action: row.get(1)?,
                key: row.get(2)?,
                hash: row.get(3)?,
                status: row.get(4)?,
                response,
                created_at: row.get(6)?,
            })
        })?;
        values.collect::<Result<Vec<_>, _>>()?
    };
    if !rows.is_empty() && !persistent_master_key {
        return Err(GErr::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "calendar_receipt_key_unavailable",
            "a persistent master key is required to protect saved calendar results",
        ));
    }
    for row in &rows {
        let context = aad(
            &row.scope,
            &row.action,
            &row.key,
            &row.hash,
            row.status,
            row.created_at,
        );
        // Preserve even malformed legacy bytes: replay fails closed, never repeats a creation.
        let encrypted = seal(vault, &context, &row.response)?;
        c.execute(
            "UPDATE calendar_idempotency SET response = ?1
              WHERE scope = ?2 AND action = ?3 AND request_key = ?4
                AND body_hash = ?5 AND status = ?6 AND created_at = ?7 AND typeof(response) = 'text'",
            params![encrypted, row.scope, row.action, row.key, row.hash, row.status, row.created_at],
        )?;
    }
    Ok(rows.len())
}

/// Complete the bounded legacy transition before any listener or delivery worker serves requests.
pub(crate) async fn protect_legacy_receipts(
    state: &AppState,
    persistent_master_key: bool,
) -> GResult<usize> {
    let mut total = 0;
    loop {
        let vault = state.vault.clone();
        let changed = state
            .store
            .group_tx(move |tx: &rusqlite::Transaction<'_>| {
                seal_legacy_batch(tx, &vault, persistent_master_key)
            })
            .await?;
        total += changed;
        if changed < MIGRATION_BATCH {
            return Ok(total);
        }
    }
}
