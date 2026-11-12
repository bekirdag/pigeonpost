//! Marketplace look (developer-program plan §1): listing logos, categories, the featured row,
//! publisher links, `coming_soon`, and the first-party sample listings.
//!
//! Additive only. Each column is added to the live `nest_listings` table after `PRAGMA
//! table_info` says it is missing; no existing CHECK is changed and no table is rebuilt.
//!
//! The samples are not seeded at startup (every test store and every staging postbox would grow
//! nine listings). Ops seed them once with `POST /internal/v1/marketplace/samples`
//! (`deploy/postbox/marketplace-samples.sh`), which is idempotent; `DELETE` on the same route
//! withdraws them.

use rusqlite::{params, Connection, OptionalExtension, Transaction};

/// `(column, definition)` added to `nest_listings`.
const COLUMNS: &[(&str, &str)] = &[
    // The 512 px WebP's sha256: the logo's identity and, shortened, part of its public URL.
    ("logo_hash", "TEXT"),
    // `{"512": digest, "128": digest, "64": digest}` in the logo blob store.
    ("logo_variants", "TEXT"),
    // A listing shown in the catalog that cannot be bought, installed or run yet (409 `coming_soon`).
    (
        "coming_soon",
        "INTEGER NOT NULL DEFAULT 0 CHECK (coming_soon IN (0, 1))",
    ),
    // One of `CATEGORIES` (checked in code, so the list can grow without a table rebuild).
    ("category", "TEXT"),
    // Lower ranks first in the catalog's featured row; NULL is not featured. Set by staff.
    ("featured_rank", "INTEGER"),
    // The publisher's own support and privacy pages (https).
    ("support_url", "TEXT"),
    ("privacy_url", "TEXT"),
];

/// The catalog's categories: `(code, English label)`. The web catalog keeps the same labels.
pub(crate) const CATEGORIES: &[(&str, &str)] = &[
    ("productivity", "Productivity"),
    ("legal", "Legal"),
    ("finance", "Finance"),
    ("support", "Customer support"),
    ("marketing", "Marketing"),
    ("sales", "Sales"),
    ("language", "Language"),
    ("research", "Research"),
    ("developer", "Developer tools"),
    ("hr", "HR"),
    ("recruiting", "Recruiting"),
    ("other", "Other"),
];

fn has_column(c: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut stmt = c.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(names.iter().any(|n| n == column))
}

/// Run after the marketplace schemas. Idempotent.
pub(crate) fn migrate(c: &Connection) -> rusqlite::Result<()> {
    let exists: bool = c.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = 'nest_listings'",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(());
    }
    for (column, def) in COLUMNS {
        if !has_column(c, "nest_listings", column)? {
            c.execute_batch(&format!(
                "ALTER TABLE nest_listings ADD COLUMN {column} {def}"
            ))?;
        }
    }
    c.execute_batch(
        "CREATE INDEX IF NOT EXISTS nest_listings_featured ON nest_listings(featured_rank)
           WHERE featured_rank IS NOT NULL;",
    )
}

// ---- the coming-soon samples -----------------------------------------------------------------------

/// The template and release every sample points at. The release is a `draft` that is never
/// attested, so even without the `coming_soon` refusal nothing could install or run it.
pub(crate) const SAMPLE_TEMPLATE: &str = "tpl_samples";
pub(crate) const SAMPLE_RELEASE: &str = "rel_samples";
/// Every sample's id starts with this; reads mark such a listing `sample: true`.
pub(crate) const SAMPLE_PREFIX: &str = "lst_sample_";
const SAMPLE_ACTOR: &str = "system:marketplace-samples";

/// One sample: `(id suffix, title, category, summary, description, featured rank)`.
type Sample = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    Option<i64>,
);

/// The nine samples of the developer-program plan §1, in its order.
pub(crate) const SAMPLES: &[Sample] = &[
    (
        "contract_reviewer",
        "Contract reviewer",
        "legal",
        "Flags risky clauses, missing terms and deadlines in a contract, with a plain-language summary.",
        "Send a contract and get back the clauses worth a second look, the terms that are missing, \
         every date and notice period, and a short summary you can forward. It points out what to \
         ask a lawyer about; it is not legal advice.",
        Some(1),
    ),
    (
        "invoice_reader",
        "Invoice reader",
        "finance",
        "Turns e-Fatura and PDF invoices into clean data: seller, lines, tax and totals.",
        "Forward e-Fatura XML or PDF invoices and receive structured data ready for your books or \
         a spreadsheet: seller and buyer, invoice number and date, each line with its tax, and the \
         totals, with anything it could not read marked for you to check.",
        None,
    ),
    (
        "meeting_minutes",
        "Meeting minutes writer",
        "productivity",
        "Writes minutes with decisions, owners and due dates from a meeting transcript.",
        "Give it a transcript or your notes and it writes minutes: who attended, what was decided, \
         the action items with an owner and a due date each, and the open questions for next time.",
        Some(2),
    ),
    (
        "support_agent",
        "Customer support agent",
        "support",
        "Answers customer questions from your help articles and hands hard cases to a person.",
        "Answers your customers' messages from the help articles and policies you give it, in your \
         tone, and passes anything it is unsure about, or anything sensitive, to a person on your \
         team with a summary.",
        None,
    ),
    (
        "social_planner",
        "Social media planner",
        "marketing",
        "Plans a month of posts across your channels, with drafts you approve first.",
        "Tell it about your product and audience and it plans a month of posts per channel, with \
         drafts, suggested images and posting times. Nothing is published: you approve and post.",
        None,
    ),
    (
        "translator",
        "Translator (TR/EN/DE)",
        "language",
        "Translates documents and messages between Turkish, English and German, keeping your terms.",
        "Translates documents and messages between Turkish, English and German. Give it a glossary \
         and it keeps your product names and terms the same in every language.",
        Some(3),
    ),
    (
        "market_research",
        "Market research brief",
        "research",
        "A sourced brief on a market: its size, the main competitors and the trends.",
        "Name a market and a region and get a brief with sources: how big it is, who the main \
         competitors are and how they position themselves, the trends, and the questions worth \
         researching further.",
        None,
    ),
    (
        "code_review",
        "Code review assistant",
        "developer",
        "Reviews a change for bugs, security issues and readability, with suggested fixes.",
        "Send a diff or a pull request and get a review: likely bugs, security issues, missing \
         tests and hard-to-read code, each with the line it is about and a suggested fix.",
        None,
    ),
    (
        "recruitment_screening",
        "Recruitment screening",
        "recruiting",
        "Screens applications against a role's requirements and drafts a shortlist with reasons.",
        "Give it a role's requirements and the applications and it drafts a shortlist with the \
         reasons for each candidate and the questions to ask in the interview. A person makes \
         every decision.",
        None,
    ),
];

const SAMPLE_NOTE: &str = "\n\nThis is a sample listing from Pigeonpost to show what is coming. \
     It is not available yet, and nothing can be bought or run from it.";

/// What a seed run did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Seeded {
    pub created: usize,
    pub existing: usize,
}

/// Seed the samples under `publisher` (the platform publisher). Rows that already exist are left
/// exactly as they are, so a withdrawn sample stays withdrawn and a re-run never duplicates.
pub(crate) fn seed_samples_in(
    tx: &Transaction<'_>,
    publisher: &str,
    now: i64,
) -> rusqlite::Result<Seeded> {
    tx.execute(
        "INSERT OR IGNORE INTO nest_templates (id, publisher_id, slug, name, created_by, created_at)
         VALUES (?1, ?2, 'coming-soon-samples', 'Coming soon samples', ?3, ?4)",
        params![SAMPLE_TEMPLATE, publisher, SAMPLE_ACTOR, now],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO nest_releases (id, template_id, publisher_id, version, image_digest,
            status, created_by, created_at)
         VALUES (?1, ?2, ?3, '0.0.0', ?4, 'draft', ?5, ?6)",
        params![
            SAMPLE_RELEASE,
            SAMPLE_TEMPLATE,
            publisher,
            format!("sha256:{}", "0".repeat(64)),
            SAMPLE_ACTOR,
            now
        ],
    )?;
    let mut out = Seeded {
        created: 0,
        existing: 0,
    };
    for (i, (suffix, title, category, summary, description, featured)) in SAMPLES.iter().enumerate()
    {
        let id = format!("{SAMPLE_PREFIX}{suffix}");
        // Newest first in the catalog's default order, so keep the plan's order with timestamps.
        let at = now - i as i64;
        let n = tx.execute(
            "INSERT OR IGNORE INTO nest_listings (id, publisher_id, release_id, title, summary,
                visibility, status, created_at, updated_at, description, template_id, published_at,
                hosting, coming_soon, category, featured_rank)
             VALUES (?1, ?2, ?3, ?4, ?5, 'public', 'published', ?6, ?6, ?7, ?8, ?6,
                'pigeonpost', 1, ?9, ?10)",
            params![
                id,
                publisher,
                SAMPLE_RELEASE,
                title,
                summary,
                at,
                format!("{description}{SAMPLE_NOTE}"),
                SAMPLE_TEMPLATE,
                category,
                featured
            ],
        )?;
        if n == 1 {
            out.created += 1;
        } else {
            out.existing += 1;
        }
    }
    Ok(out)
}

/// Withdraw every sample still listed: how many were taken down.
pub(crate) fn withdraw_samples_in(tx: &Transaction<'_>, now: i64) -> rusqlite::Result<usize> {
    tx.execute(
        "UPDATE nest_listings SET status = 'withdrawn', updated_at = ?2, featured_rank = NULL
          WHERE substr(id, 1, length(?1)) = ?1 AND status <> 'withdrawn'",
        params![SAMPLE_PREFIX, now],
    )
}

/// Whether `listing` is a published, public listing marked `coming_soon`. A listing the caller
/// may not see is not answered here; the caller's own visibility rules decide that.
pub(crate) fn coming_soon_in(c: &Connection, listing: &str) -> rusqlite::Result<bool> {
    Ok(c.query_row(
        "SELECT coming_soon FROM nest_listings WHERE id = ?1 AND status = 'published'
           AND visibility = 'public'",
        [listing],
        |r| r.get::<_, i64>(0),
    )
    .optional()?
        == Some(1))
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    #[test]
    fn the_migration_adds_columns_in_place_and_keeps_rows() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE nest_listings (id TEXT PRIMARY KEY, title TEXT);
             INSERT INTO nest_listings VALUES ('lst_a', 'A');",
        )
        .unwrap();
        super::migrate(&c).unwrap();
        super::migrate(&c).unwrap();
        let (soon, logo): (i64, Option<String>) = c
            .query_row(
                "SELECT coming_soon, logo_hash FROM nest_listings WHERE id = 'lst_a'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((soon, logo), (0, None));
        assert!(c
            .execute("UPDATE nest_listings SET coming_soon = 2", [])
            .is_err());
    }

    #[test]
    fn categories_are_unique_and_samples_use_them() {
        let codes: std::collections::BTreeSet<_> =
            super::CATEGORIES.iter().map(|(c, _)| *c).collect();
        assert_eq!(codes.len(), super::CATEGORIES.len());
        assert_eq!(super::SAMPLES.len(), 9);
        for s in super::SAMPLES {
            assert!(codes.contains(s.2), "{}", s.1);
            assert!(s.3.chars().count() <= 300 && s.1.chars().count() <= 120);
        }
    }
}
