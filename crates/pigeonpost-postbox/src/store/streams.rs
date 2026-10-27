//! Live streams in groups (SDS §14A, `docs/streaming.md`).
//!
//! The postbox keeps what a stream is and who may take part: the group, the streamer, the title,
//! the stage, and the public halves of the per-stream key pairs that watching devices registered.
//! It never sees media or the stream key: frames are encrypted on the streamer's device and the
//! stream key travels wrapped to those registered public keys, through the calls service.
//!
//! Rows are short-lived: an ended stream's rows are deleted a week after it ends
//! (`crate::streams::sweep_in`); what stays is the group's `stream` notice.

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS group_streams (
    id            TEXT PRIMARY KEY,
    group_id      TEXT NOT NULL,
    host          TEXT NOT NULL,
    title         TEXT NOT NULL CHECK (length(title) BETWEEN 1 AND 100),
    sources       TEXT NOT NULL,
    host_key      TEXT,
    state         TEXT NOT NULL CHECK (state IN ('live','ended')),
    started_at    INTEGER NOT NULL,
    ends_by       INTEGER NOT NULL,
    ended_at      INTEGER,
    end_reason    TEXT,
    ended_by      TEXT,
    last_beat     INTEGER NOT NULL,
    viewers_now   INTEGER NOT NULL DEFAULT 0,
    viewers_peak  INTEGER NOT NULL DEFAULT 0,
    revision      INTEGER NOT NULL DEFAULT 1
);
CREATE UNIQUE INDEX IF NOT EXISTS group_streams_one_live ON group_streams(group_id) WHERE state = 'live';
CREATE INDEX IF NOT EXISTS group_streams_state ON group_streams(state, ends_by);

-- Who is on the stage, or invited onto it. The streamer is not listed: they are `host`.
CREATE TABLE IF NOT EXISTS group_stream_stage (
    stream_id   TEXT NOT NULL,
    member      TEXT NOT NULL,
    state       TEXT NOT NULL CHECK (state IN ('invited','speaker')),
    invited_by  TEXT NOT NULL,
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (stream_id, member)
);

-- The public half of each key pair a member's device made to receive this stream's key. A
-- member may watch on a few devices at once; the oldest key is dropped beyond that.
CREATE TABLE IF NOT EXISTS group_stream_keys (
    stream_id   TEXT NOT NULL,
    member      TEXT NOT NULL,
    public_key  TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (stream_id, member, public_key)
);
";
