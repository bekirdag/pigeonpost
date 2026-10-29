//! Phone numbers, contact discovery, SMS invitations and display names (SDS §14D). The logic is
//! in `crate::contacts` and `crate::profile_names`; this is the schema and the retention sweep.
//!
//! Every table is additive. No uploaded contact hash is stored anywhere: discovery keeps a daily
//! count and per-account keyed marks with a 7-day expiry, and numbers appear only as keyed tags
//! and vault-sealed ciphertext.

use super::{params, Store, StoreError};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS phone_numbers (
    id            TEXT PRIMARY KEY,
    account_id    TEXT NOT NULL,
    identity      TEXT NOT NULL UNIQUE,
    tag           BLOB NOT NULL UNIQUE,
    number_sealed BLOB NOT NULL,
    country_code  TEXT NOT NULL,
    discoverable  INTEGER NOT NULL DEFAULT 0,
    verified_at   INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS phone_numbers_by_account ON phone_numbers(account_id);
CREATE TABLE IF NOT EXISTS phone_challenges (
    id            TEXT PRIMARY KEY,
    account_id    TEXT NOT NULL,
    identity      TEXT NOT NULL,
    tag           BLOB NOT NULL,
    number_sealed BLOB NOT NULL,
    country_code  TEXT NOT NULL,
    code_mac      BLOB NOT NULL,
    attempts      INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL,
    expires_at    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS phone_challenges_by_account ON phone_challenges(account_id, expires_at);
CREATE TABLE IF NOT EXISTS sms_ledger (
    id           TEXT PRIMARY KEY,
    purpose      TEXT NOT NULL CHECK (purpose IN ('verify','invite')),
    account_id   TEXT,
    tag          BLOB NOT NULL,
    country_code TEXT NOT NULL,
    status       TEXT NOT NULL CHECK (status IN ('pending','sent','failed')),
    created_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS sms_ledger_by_time ON sms_ledger(created_at);
CREATE INDEX IF NOT EXISTS sms_ledger_by_tag ON sms_ledger(tag, purpose, created_at);
CREATE INDEX IF NOT EXISTS sms_ledger_by_account ON sms_ledger(account_id, purpose, created_at);
CREATE TABLE IF NOT EXISTS sms_invites (
    id              TEXT PRIMARY KEY,
    code            TEXT NOT NULL UNIQUE,
    sender_account  TEXT NOT NULL,
    sender_identity TEXT NOT NULL,
    tag             BLOB NOT NULL,
    created_at      INTEGER NOT NULL,
    expires_at      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS sms_invites_by_sender ON sms_invites(sender_account, created_at);
CREATE INDEX IF NOT EXISTS sms_invites_by_tag ON sms_invites(tag, created_at);
CREATE TABLE IF NOT EXISTS sms_optouts (
    tag        BLOB PRIMARY KEY,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS discovery_usage (
    account_id TEXT NOT NULL,
    day        INTEGER NOT NULL,
    new_hashes INTEGER NOT NULL DEFAULT 0,
    capped     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (account_id, day)
);
CREATE TABLE IF NOT EXISTS discovery_marks (
    account_id TEXT NOT NULL,
    mark       BLOB NOT NULL,
    expires_at INTEGER NOT NULL,
    PRIMARY KEY (account_id, mark)
);
CREATE INDEX IF NOT EXISTS discovery_marks_by_expiry ON discovery_marks(expires_at);
CREATE TABLE IF NOT EXISTS discovery_lockouts (
    account_id TEXT PRIMARY KEY,
    until      INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS profile_names (
    address      TEXT PRIMARY KEY,
    account_id   TEXT,
    display_name TEXT NOT NULL,
    visibility   TEXT NOT NULL DEFAULT 'known' CHECK (visibility IN ('everyone','known','nobody')),
    updated_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS profile_names_by_account ON profile_names(account_id);
";

/// How long the SMS cost ledger keeps a row (the longest throttle window is 30 days).
pub const LEDGER_RETENTION_SECS: u64 = 35 * 86_400;
/// How long a day's discovery count is kept (the monthly budget looks back 30 days).
pub const USAGE_RETENTION_DAYS: u64 = 31;

/// What one retention pass removed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ContactsSweep {
    pub challenges: usize,
    pub marks: usize,
    pub usage: usize,
    pub invites: usize,
    pub ledger: usize,
    pub lockouts: usize,
}

impl ContactsSweep {
    pub fn total(&self) -> usize {
        self.challenges + self.marks + self.usage + self.invites + self.ledger + self.lockouts
    }
}

impl Store {
    /// Drop everything past its retention: expired codes, 7-day discovery marks, old daily
    /// counts, invitations past 30 days, ledger rows past 35 days and ended lockouts.
    pub async fn sweep_contacts(&self, now: u64) -> Result<ContactsSweep, StoreError> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<ContactsSweep, StoreError> {
            let c = conn.lock().expect("store lock");
            let now_i = now as i64;
            let day = (now / 86_400) as i64;
            Ok(ContactsSweep {
                challenges: c.execute(
                    "DELETE FROM phone_challenges WHERE expires_at <= ?1",
                    params![now_i],
                )?,
                marks: c.execute(
                    "DELETE FROM discovery_marks WHERE expires_at <= ?1",
                    params![now_i],
                )?,
                usage: c.execute(
                    "DELETE FROM discovery_usage WHERE day < ?1",
                    params![day - USAGE_RETENTION_DAYS as i64],
                )?,
                invites: c.execute(
                    "DELETE FROM sms_invites WHERE expires_at <= ?1",
                    params![now_i],
                )?,
                ledger: c.execute(
                    "DELETE FROM sms_ledger WHERE created_at <= ?1",
                    params![now_i - LEDGER_RETENTION_SECS as i64],
                )?,
                lockouts: c.execute(
                    "DELETE FROM discovery_lockouts WHERE until <= ?1",
                    params![now_i],
                )?,
            })
        })
        .await
        .map_err(|_| StoreError::Join)?
    }
}
