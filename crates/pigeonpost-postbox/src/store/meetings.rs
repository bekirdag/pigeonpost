//! Meeting summaries (SDS §14, docs/meeting-summaries.md): the postbox side keeps only metadata,
//! consent records, attendees' public notes keys and the finished brief as ciphertext.
//!
//! The postbox never sees audio, a transcript or a brief in plaintext. The meeting-intel service
//! encrypts the brief with a fresh content key and wraps that key for each registered notes key of
//! each consenting attendee; this database stores the ciphertext and the wraps, and serves them
//! only to those attendees until the retention date.

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meetings (
    id                TEXT PRIMARY KEY,
    room_ref          TEXT NOT NULL,
    kind              TEXT NOT NULL CHECK (kind IN ('direct','room','group','scheduled')),
    host_account      TEXT NOT NULL,
    host_address      TEXT NOT NULL,
    group_id          TEXT,
    calendar_event_id TEXT,
    occurrence_id     TEXT,
    state             TEXT NOT NULL CHECK (state IN ('active','processing','ready','failed')),
    created_at        INTEGER NOT NULL,
    ended_at          INTEGER,
    ready_at          INTEGER,
    expires_at        INTEGER NOT NULL,
    alg               TEXT,
    iv                TEXT,
    ciphertext        TEXT,
    failure           TEXT
);
CREATE INDEX IF NOT EXISTS meetings_expiry ON meetings(expires_at);
CREATE INDEX IF NOT EXISTS meetings_host ON meetings(host_account, created_at);
-- One row per member account that answered the consent prompt. Guests have no account and are
-- never recorded here (they cannot receive a brief).
CREATE TABLE IF NOT EXISTS meeting_attendees (
    meeting_id    TEXT NOT NULL,
    account_id    TEXT NOT NULL,
    address       TEXT NOT NULL,
    consented_at  INTEGER,
    declined_at   INTEGER,
    withdrawn_at  INTEGER,
    removed_at    INTEGER,
    PRIMARY KEY (meeting_id, account_id)
);
CREATE INDEX IF NOT EXISTS meeting_attendees_account ON meeting_attendees(account_id);
-- Public P-256 keys of the devices that may open an account's meeting notes. Private keys never
-- leave the device that made them.
CREATE TABLE IF NOT EXISTS meeting_notes_keys (
    account_id   TEXT NOT NULL,
    key_id       TEXT NOT NULL,
    public_key   TEXT NOT NULL,
    label        TEXT,
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER,
    PRIMARY KEY (account_id, key_id)
);
-- The brief's content key, wrapped for one notes key of one attendee.
CREATE TABLE IF NOT EXISTS meeting_brief_keys (
    meeting_id TEXT NOT NULL,
    account_id TEXT NOT NULL,
    key_id     TEXT NOT NULL,
    epk        TEXT NOT NULL,
    iv         TEXT NOT NULL,
    wrapped    TEXT NOT NULL,
    PRIMARY KEY (meeting_id, account_id, key_id)
);
";
