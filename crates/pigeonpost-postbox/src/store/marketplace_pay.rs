//! Marketplace payments (paid listings, M3): listing prices as publishers set them, the offers
//! mirrored to the financial authority (saas_be, through the billing bridge), purchases that
//! reserve an installation id for a payment, and the licence each paid installation runs under.
//!
//! Additive tables only. Money itself is not here: no card data, no authorization tokens, no
//! commission, reserve or payout value. A price is the publisher's list price in minor units; what
//! was charged, invoiced and paid out is saas_be's.

pub(crate) const SCHEMA: &str = "
-- The price a publisher asked for a listing. One row per priced listing; no row = free (the M1/M2
-- zero-priced path). Launch terms: one-off or monthly.
CREATE TABLE IF NOT EXISTS nest_listing_prices (
    listing_id   TEXT PRIMARY KEY REFERENCES nest_listings(id),
    amount_minor INTEGER NOT NULL CHECK (amount_minor BETWEEN 1 AND 100000000),
    currency     TEXT NOT NULL CHECK (currency IN ('TRY','USD','EUR')),
    term         TEXT NOT NULL CHECK (term IN ('one_off','monthly')),
    updated_by   TEXT NOT NULL,
    updated_at   INTEGER NOT NULL
);

-- Offers mirrored to saas_be. Price, currency, term, publisher, listing and release line are
-- immutable there, so any change is a new offer id (derived from those fields) and the previous
-- one is retired. `state`: `pending` until saas_be has it, `active`, `retiring` until saas_be has
-- retired it, `retired`.
CREATE TABLE IF NOT EXISTS nest_listing_offers (
    offer_id     TEXT PRIMARY KEY,
    listing_id   TEXT NOT NULL REFERENCES nest_listings(id),
    publisher_id TEXT NOT NULL,
    release_line TEXT NOT NULL,
    term         TEXT NOT NULL CHECK (term IN ('one_off','monthly')),
    currency     TEXT NOT NULL CHECK (currency IN ('TRY','USD','EUR')),
    amount_minor INTEGER NOT NULL CHECK (amount_minor > 0),
    state        TEXT NOT NULL CHECK (state IN ('pending','active','retiring','retired')),
    sellable     INTEGER NOT NULL DEFAULT 0,
    last_error   TEXT,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS nest_listing_offers_by_listing ON nest_listing_offers(listing_id, state);

-- A buyer's purchase of a paid listing: the installation id the payment admits, reserved before
-- any money moves, and the exact intent the payment is bound to. One per (account, intent).
CREATE TABLE IF NOT EXISTS nest_mp_purchases (
    installation_id TEXT PRIMARY KEY,
    account_id      TEXT NOT NULL,
    intent_id       TEXT NOT NULL,
    listing_id      TEXT NOT NULL REFERENCES nest_listings(id),
    offer_id        TEXT NOT NULL REFERENCES nest_listing_offers(offer_id),
    release_line    TEXT NOT NULL,
    owner_kind      TEXT NOT NULL CHECK (owner_kind IN ('account','org')),
    owner_id        TEXT NOT NULL,
    status          TEXT NOT NULL CHECK (status IN ('open','installed')),
    created_at      INTEGER NOT NULL,
    installed_at    INTEGER,
    UNIQUE (account_id, intent_id)
);
CREATE INDEX IF NOT EXISTS nest_mp_purchases_by_listing ON nest_mp_purchases(listing_id, owner_id);

-- The licence a paid installation runs under, as saas_be last reported it (through the bridge's
-- reconciler). `access` is what job admission reads; `runtime` is what the worker host was told:
-- `running` (nothing to do), `stopped` (paused, data kept), `removed` (deletion boundary passed).
CREATE TABLE IF NOT EXISTS nest_installation_licences (
    installation_id      TEXT PRIMARY KEY REFERENCES nest_installations(id),
    licence_id           TEXT NOT NULL UNIQUE,
    offer_id             TEXT NOT NULL,
    term                 TEXT NOT NULL CHECK (term IN ('one_off','monthly')),
    status               TEXT NOT NULL CHECK (status IN
                             ('active','canceling','paused','expired','terminated','revoked')),
    access               INTEGER NOT NULL CHECK (access IN (0,1)),
    paid_through         INTEGER,
    cancel_at_period_end INTEGER NOT NULL DEFAULT 0,
    deletion_due_at      INTEGER,
    revoked_at           INTEGER,
    runtime              TEXT NOT NULL DEFAULT 'running' CHECK (runtime IN ('running','stopped','removed')),
    version              INTEGER NOT NULL DEFAULT 0,
    created_at           INTEGER NOT NULL,
    updated_at           INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS nest_installation_licences_due
    ON nest_installation_licences(access, deletion_due_at);
";

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    #[test]
    fn prices_and_licences_keep_their_shape() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE nest_listings (id TEXT PRIMARY KEY);
             CREATE TABLE nest_installations (id TEXT PRIMARY KEY);
             INSERT INTO nest_listings VALUES ('lst_a');
             INSERT INTO nest_installations VALUES ('ins_a');",
        )
        .unwrap();
        c.execute_batch(super::SCHEMA).unwrap();
        let bad = [
            "INSERT INTO nest_listing_prices VALUES ('lst_a', 0, 'TRY', 'one_off', 'a', 1)",
            "INSERT INTO nest_listing_prices VALUES ('lst_a', 100, 'GBP', 'one_off', 'a', 1)",
            "INSERT INTO nest_listing_prices VALUES ('lst_a', 100, 'TRY', 'yearly', 'a', 1)",
            "INSERT INTO nest_installation_licences (installation_id, licence_id, offer_id, term,
                status, access, created_at, updated_at)
             VALUES ('ins_a', 'l', 'o', 'one_off', 'wallet', 1, 1, 1)",
        ];
        for sql in bad {
            assert!(c.execute(sql, []).is_err(), "{sql}");
        }
        c.execute(
            "INSERT INTO nest_listing_prices VALUES ('lst_a', 100, 'TRY', 'monthly', 'a', 1)",
            [],
        )
        .unwrap();
    }
}
