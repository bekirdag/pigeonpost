//! Calendar and scheduled meetings (SDS §14B.4). Business rules live in `crate::calendar`; every
//! mutation runs in one IMMEDIATE transaction through `Store::group_tx`, so permission checks,
//! revision checks, occurrence changes, the delivery outbox and account events commit together.
//!
//! Naming: a guest without a Pigeonpost handle is reached at a **contact address**. Those
//! addresses are stored sealed under the vault (`contact_sealed`) and matched by a keyed HMAC of
//! the canonical form (`contact_hash`), never by a plain digest that a dictionary could reverse.
//!
//! Titles, descriptions, locations and RSVP notes are sealed at rest under the vault (§14B.11). They
//! are server-readable by design: reminders and invitations are rendered from them.

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS calendars (
    id             TEXT PRIMARY KEY,
    owner_identity TEXT,
    group_id       TEXT,
    name           TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 80),
    color          TEXT NOT NULL DEFAULT 'blue',
    is_default     INTEGER NOT NULL DEFAULT 0,
    revision       INTEGER NOT NULL DEFAULT 1,
    created_at     INTEGER NOT NULL,
    deleted_at     INTEGER,
    CHECK ((owner_identity IS NOT NULL AND group_id IS NULL) OR
           (owner_identity IS NULL AND group_id IS NOT NULL))
);
CREATE UNIQUE INDEX IF NOT EXISTS one_default_calendar ON calendars(owner_identity)
    WHERE is_default = 1 AND deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS calendars_owner ON calendars(owner_identity) WHERE deleted_at IS NULL;

CREATE TABLE IF NOT EXISTS meeting_reservations (
    id         TEXT PRIMARY KEY,
    code       TEXT NOT NULL UNIQUE,
    created_by TEXT NOT NULL,
    org_id     TEXT,
    group_id   TEXT,
    created_at INTEGER NOT NULL,
    retired_at INTEGER
);

CREATE TABLE IF NOT EXISTS events (
    id                 TEXT PRIMARY KEY,
    calendar_id        TEXT NOT NULL REFERENCES calendars(id),
    organizer          TEXT NOT NULL,
    revision           INTEGER NOT NULL DEFAULT 1,
    sequence           INTEGER NOT NULL DEFAULT 0,
    title_sealed       BLOB NOT NULL,
    details_sealed     BLOB,
    value_type         TEXT NOT NULL CHECK (value_type IN ('date','date_time')),
    start_date         TEXT,
    end_date_exclusive TEXT,
    local_start        TEXT,
    local_end          TEXT,
    tzid               TEXT,
    starts_at          INTEGER,
    ends_at            INTEGER,
    tzdb_version       TEXT,
    rrule              TEXT,
    exdates            TEXT,
    recur_until        INTEGER,
    transparency       TEXT NOT NULL DEFAULT 'opaque' CHECK (transparency IN ('opaque','transparent')),
    privacy            TEXT NOT NULL DEFAULT 'default' CHECK (privacy IN ('default','private')),
    meeting_id         TEXT REFERENCES meeting_reservations(id),
    meeting_options    TEXT,
    meeting_policy_version INTEGER NOT NULL DEFAULT 1,
    transport_profile  TEXT,
    participant_limit  INTEGER,
    linked_conversation TEXT,
    locale             TEXT NOT NULL DEFAULT 'en',
    split_from         TEXT,
    import_uid         TEXT,
    status             TEXT NOT NULL DEFAULT 'confirmed' CHECK (status IN ('confirmed','cancelled')),
    created_by_account TEXT,
    created_at         INTEGER NOT NULL,
    updated_at         INTEGER NOT NULL,
    span_from          INTEGER NOT NULL,
    span_to            INTEGER,
    CHECK ((value_type = 'date' AND start_date IS NOT NULL AND end_date_exclusive IS NOT NULL
            AND local_start IS NULL AND local_end IS NULL AND starts_at IS NULL AND ends_at IS NULL
            AND meeting_id IS NULL)
        OR (value_type = 'date_time' AND local_start IS NOT NULL AND local_end IS NOT NULL
            AND tzid IS NOT NULL AND starts_at IS NOT NULL AND ends_at IS NOT NULL AND ends_at > starts_at
            AND start_date IS NULL AND end_date_exclusive IS NULL))
);
CREATE INDEX IF NOT EXISTS events_calendar_span ON events(calendar_id, span_from);
CREATE INDEX IF NOT EXISTS events_organizer ON events(organizer);
CREATE INDEX IF NOT EXISTS events_meeting ON events(meeting_id) WHERE meeting_id IS NOT NULL;

-- Stored only when something is attached to one occurrence (an exception, an answer, a delivery,
-- a call). Ids are deterministic from (event, recurrence_key), so read-time expansion and stored
-- rows always agree.
CREATE TABLE IF NOT EXISTS event_occurrences (
    id             TEXT PRIMARY KEY,
    event_id       TEXT NOT NULL REFERENCES events(id),
    recurrence_key TEXT NOT NULL,
    event_revision INTEGER NOT NULL,
    starts_at      INTEGER,
    ends_at        INTEGER,
    start_date     TEXT,
    end_date_exclusive TEXT,
    status         TEXT NOT NULL CHECK (status IN ('confirmed','cancelled')),
    extended_until INTEGER,
    UNIQUE (event_id, recurrence_key)
);
CREATE INDEX IF NOT EXISTS event_occurrences_time ON event_occurrences(starts_at, ends_at);

CREATE TABLE IF NOT EXISTS event_overrides (
    occurrence_id  TEXT PRIMARY KEY REFERENCES event_occurrences(id),
    local_start    TEXT,
    local_end      TEXT,
    tzid           TEXT,
    start_date     TEXT,
    end_date_exclusive TEXT,
    title_sealed   BLOB,
    details_sealed BLOB,
    status         TEXT CHECK (status IN ('confirmed','cancelled')),
    meeting_options TEXT,
    meeting_policy_version INTEGER,
    revision       INTEGER NOT NULL,
    sequence       INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS event_attendees (
    id                 TEXT PRIMARY KEY,
    event_id           TEXT NOT NULL REFERENCES events(id),
    kind               TEXT NOT NULL CHECK (kind IN ('handle','external')),
    address            TEXT,
    contact_hash       BLOB,
    contact_sealed     BLOB,
    display_name       TEXT,
    role               TEXT NOT NULL DEFAULT 'attendee' CHECK (role IN ('attendee','cohost')),
    optional           INTEGER NOT NULL DEFAULT 0,
    base_participation TEXT NOT NULL DEFAULT 'invited' CHECK (base_participation IN ('invited','excluded')),
    rsvp               TEXT NOT NULL DEFAULT 'needs_action'
                       CHECK (rsvp IN ('needs_action','accepted','tentative','declined')),
    rsvp_note_sealed   BLOB,
    rsvp_at            INTEGER,
    rsvp_proof         TEXT CHECK (rsvp_proof IS NULL OR rsvp_proof IN ('member','link','verified')),
    revision           INTEGER NOT NULL DEFAULT 1,
    grant_version      INTEGER NOT NULL DEFAULT 1,
    calendar_id        TEXT REFERENCES calendars(id),
    filtered           INTEGER NOT NULL DEFAULT 0,
    -- The recipient blocked the organizer: nothing reaches them, and the organizer cannot tell.
    suppressed         INTEGER NOT NULL DEFAULT 0,
    can_admit          INTEGER NOT NULL DEFAULT 0,
    added_at           INTEGER NOT NULL,
    removed_at         INTEGER,
    redacted_at        INTEGER,
    CHECK ((kind = 'handle' AND address IS NOT NULL AND contact_hash IS NULL AND contact_sealed IS NULL)
        OR (kind = 'external' AND address IS NULL AND contact_hash IS NOT NULL
            AND (contact_sealed IS NOT NULL OR redacted_at IS NOT NULL) AND role = 'attendee'))
);
CREATE UNIQUE INDEX IF NOT EXISTS event_attendee_handle ON event_attendees(event_id, address)
    WHERE kind = 'handle' AND removed_at IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS event_attendee_contact ON event_attendees(event_id, contact_hash)
    WHERE kind = 'external' AND removed_at IS NULL;
CREATE INDEX IF NOT EXISTS event_attendees_address ON event_attendees(address) WHERE removed_at IS NULL;
CREATE INDEX IF NOT EXISTS event_attendees_event ON event_attendees(event_id);

CREATE TABLE IF NOT EXISTS event_occurrence_attendees (
    occurrence_id    TEXT NOT NULL REFERENCES event_occurrences(id),
    attendee_id      TEXT NOT NULL REFERENCES event_attendees(id),
    participation    TEXT CHECK (participation IN ('invited','removed')),
    rsvp             TEXT CHECK (rsvp IN ('needs_action','accepted','tentative','declined')),
    role             TEXT CHECK (role IN ('attendee','cohost')),
    rsvp_note_sealed BLOB,
    rsvp_at          INTEGER,
    revision         INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (occurrence_id, attendee_id)
);

CREATE TABLE IF NOT EXISTS event_attendee_tokens (
    id                     TEXT PRIMARY KEY,
    token_hash             BLOB NOT NULL UNIQUE,
    attendee_id            TEXT NOT NULL REFERENCES event_attendees(id),
    purpose                TEXT NOT NULL CHECK (purpose IN ('invite_manage','unsubscribe')),
    audience               TEXT NOT NULL,
    attendee_grant_version INTEGER NOT NULL,
    created_at             INTEGER NOT NULL,
    expires_at             INTEGER NOT NULL,
    consumed_at            INTEGER,
    revoked_at             INTEGER
);
CREATE INDEX IF NOT EXISTS event_attendee_tokens_attendee ON event_attendee_tokens(attendee_id);

-- Single-use, 10-minute verification codes, stored as a keyed HMAC (a 6-digit code's plain hash
-- would fall to an offline guess).
CREATE TABLE IF NOT EXISTS event_guest_challenges (
    id            TEXT PRIMARY KEY,
    attendee_id   TEXT NOT NULL REFERENCES event_attendees(id),
    invitation_id TEXT NOT NULL REFERENCES event_attendee_tokens(id),
    code_mac      BLOB NOT NULL,
    attempts      INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL,
    expires_at    INTEGER NOT NULL,
    consumed_at   INTEGER,
    session_id    TEXT
);
CREATE INDEX IF NOT EXISTS event_guest_challenges_attendee ON event_guest_challenges(attendee_id, created_at);

CREATE TABLE IF NOT EXISTS event_guest_sessions (
    id                     TEXT PRIMARY KEY,
    token_hash             BLOB NOT NULL UNIQUE,
    attendee_id            TEXT NOT NULL REFERENCES event_attendees(id),
    invitation_id          TEXT NOT NULL REFERENCES event_attendee_tokens(id),
    device_key             BLOB NOT NULL,
    attendee_grant_version INTEGER NOT NULL,
    verified_at            INTEGER NOT NULL,
    created_at             INTEGER NOT NULL,
    expires_at             INTEGER NOT NULL,
    revoked_at             INTEGER,
    -- The session's bearer value sealed under the vault, so a repeated verify/complete returns the
    -- same session instead of minting a second one.
    token_sealed           BLOB
);

-- Short-lived single-use admission tickets handed to the calls service.
CREATE TABLE IF NOT EXISTS event_join_tickets (
    id            TEXT PRIMARY KEY,
    token_hash    BLOB NOT NULL UNIQUE,
    attendee_id   TEXT NOT NULL REFERENCES event_attendees(id),
    occurrence_id TEXT NOT NULL,
    meeting_id    TEXT NOT NULL,
    verified      INTEGER NOT NULL,
    grant_version INTEGER NOT NULL,
    created_at    INTEGER NOT NULL,
    expires_at    INTEGER NOT NULL,
    redeemed_at   INTEGER
);

CREATE TABLE IF NOT EXISTS event_reminders (
    id             TEXT PRIMARY KEY,
    event_id       TEXT NOT NULL REFERENCES events(id),
    attendee_id    TEXT REFERENCES event_attendees(id),
    owner_identity TEXT,
    minutes_before INTEGER NOT NULL CHECK (minutes_before BETWEEN 0 AND 40320),
    channel        TEXT NOT NULL CHECK (channel IN ('push','external')),
    revision       INTEGER NOT NULL DEFAULT 1,
    enabled        INTEGER NOT NULL DEFAULT 1,
    CHECK ((attendee_id IS NOT NULL AND owner_identity IS NULL) OR
           (attendee_id IS NULL AND owner_identity IS NOT NULL))
);
CREATE INDEX IF NOT EXISTS event_reminders_event ON event_reminders(event_id) WHERE enabled = 1;

CREATE TABLE IF NOT EXISTS calendar_delivery_outbox (
    id                 TEXT PRIMARY KEY,
    event_id           TEXT NOT NULL REFERENCES events(id),
    occurrence_id      TEXT,
    recipient_id       TEXT NOT NULL,
    workflow           TEXT NOT NULL,
    event_revision     INTEGER NOT NULL,
    recipient_revision INTEGER NOT NULL,
    reminder_id        TEXT,
    reminder_revision  INTEGER,
    dedupe_key         TEXT NOT NULL UNIQUE,
    due_at             INTEGER NOT NULL,
    state              TEXT NOT NULL CHECK (state IN ('queued','leased','accepted','delivered','failed',
                                                       'suppressed','obsolete','uncertain')),
    lease_until        INTEGER,
    attempt_count      INTEGER NOT NULL DEFAULT 0,
    provider_reference TEXT,
    delivered_at       INTEGER,
    created_at         INTEGER NOT NULL,
    account_id         TEXT,
    -- A cancellation for someone already removed keeps the minimum it needs to be sent.
    snapshot_sealed    BLOB,
    last_error         TEXT
);
CREATE INDEX IF NOT EXISTS calendar_delivery_due ON calendar_delivery_outbox(due_at) WHERE state = 'queued';
CREATE INDEX IF NOT EXISTS calendar_delivery_event ON calendar_delivery_outbox(event_id, recipient_id);
CREATE INDEX IF NOT EXISTS calendar_delivery_account_day ON calendar_delivery_outbox(account_id, created_at);

CREATE TABLE IF NOT EXISTS calendar_feed_grants (
    id                TEXT PRIMARY KEY,
    calendar_id       TEXT NOT NULL REFERENCES calendars(id),
    exporter_identity TEXT NOT NULL,
    token_hash        BLOB NOT NULL UNIQUE,
    projection        TEXT NOT NULL CHECK (projection IN ('busy','personal_redacted')),
    created_at        INTEGER NOT NULL,
    expires_at        INTEGER NOT NULL,
    revoked_at        INTEGER
);

CREATE TABLE IF NOT EXISTS contact_suppressions (
    contact_hash BLOB NOT NULL,
    scope        TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    PRIMARY KEY (contact_hash, scope)
);

CREATE TABLE IF NOT EXISTS calendar_settings (
    mailbox       TEXT PRIMARY KEY,
    previews      INTEGER NOT NULL DEFAULT 0,
    timezone      TEXT,
    week_start    INTEGER NOT NULL DEFAULT 1,
    working_hours TEXT,
    updated_at    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS calendar_idempotency (
    scope       TEXT NOT NULL,
    action      TEXT NOT NULL,
    request_key TEXT NOT NULL,
    body_hash   TEXT NOT NULL,
    status      INTEGER NOT NULL,
    response    TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (scope, action, request_key)
);

CREATE TABLE IF NOT EXISTS calendar_import_drafts (
    id         TEXT PRIMARY KEY,
    mailbox    TEXT NOT NULL,
    calendar_id TEXT NOT NULL,
    payload_sealed BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);
";
