//! Reputation: what a mailbox has *earned*, as distinct from what a human has *granted* it.
//!
//! Phases 2 and 3 are about deliberate trust — a person naming a peer and the verbs it may drive.
//! That works between agents who already know each other and says nothing about a stranger. This
//! module covers the other case: an inbox nobody has vouched for, arriving unannounced.
//!
//! Two subjects are scored, because a flood has two handles on it:
//!
//! * the **sender** — the address that sent the mail, and
//! * the **mint IP** — the address that brought that sender into existence.
//!
//! Scoring only the sender is what makes a mint-flood cheap: burn the reported inbox, mint
//! another, keep going. `mint_events` already records who minted what from where (Phase 1), so a
//! report against a sender can also be charged to the source that produced it, and an IP that
//! keeps producing reported inboxes eventually stops being allowed to produce them.
//!
//! Nothing here silently drops mail. A low score slows a stranger down and is stamped on their
//! messages so the recipient can weigh them; the recipient's own `block` is what stops delivery.

/// Where a mailbox starts in life, before it has done anything.
///
/// The ladder is "what did this cost to obtain, and what does it stake". An anonymous mint costs
/// milliseconds of hashing, so it starts at the bottom; anything tied to an external identity has
/// something to lose and starts higher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// Proof-of-work only. Cheap by design — that is the point of self-serve — so it earns the
    /// least benefit of the doubt.
    Anonymous,
    /// Backed by an API-key account. Someone went through account creation for it.
    Account,
    /// A handle proving control of an external identity (`/github/…`, a domain address).
    ///
    /// Not reachable from the postbox yet: handles live in the registry, and this server only
    /// ever sees `/k/` addresses. Defined now because the ladder is the design and leaving a hole
    /// in the middle would invite scoring the tiers against each other wrongly later.
    #[allow(dead_code)]
    VerifiedHandle,
    /// A paid handle. Costs money, so a flood costs money. Unreachable for the same reason.
    #[allow(dead_code)]
    PaidHandle,
}

impl Tier {
    /// Start-of-life score.
    pub fn prior(self) -> i64 {
        match self {
            Tier::Anonymous => 0,
            Tier::Account => 20,
            Tier::VerifiedHandle => 50,
            Tier::PaidHandle => 80,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Anonymous => "anonymous",
            Tier::Account => "account",
            Tier::VerifiedHandle => "verified_handle",
            Tier::PaidHandle => "paid_handle",
        }
    }
}

/// What one upheld spam report costs its subject.
pub const REPORT_PENALTY: i64 = 25;

/// Scores are clamped to this band. A floor stops an address being buried so deep that returning
/// to good standing is impossible; a ceiling stops age alone buying immunity.
pub const SCORE_FLOOR: i64 = -100;
pub const SCORE_CEILING: i64 = 100;

/// At or below this, a sender nobody has vouched for is slowed into a stranger's inbox.
pub const THROTTLE_BELOW: i64 = 0;

/// A reporter at or below this has its reports ignored. Without it, minting inboxes would be a way
/// to manufacture downvotes — the very flood this is meant to price, pointed at reputations
/// instead of at inboxes.
pub const REPORTER_IGNORED_AT_OR_BELOW: i64 = -50;

/// At or below this, an IP mints nothing further. Reached only by repeatedly producing inboxes
/// that recipients reported.
pub const MINT_HALT_AT_OR_BELOW: i64 = -75;

/// Below this, an IP keeps minting but at a trickle.
pub const MINT_THROTTLE_BELOW: i64 = -25;

/// Mints allowed per window once an IP is throttled.
pub const THROTTLED_MINT_ALLOWANCE: usize = 1;

/// How many messages an un-vouched-for, low-scoring sender may put in one stranger's inbox per
/// window. Generous enough for a real introduction, mean enough that a broadcast is uneconomic.
pub const STRANGER_MESSAGES_PER_WINDOW: usize = 3;
pub const STRANGER_WINDOW_SECS: u64 = 3600;

pub fn clamp(score: i64) -> i64 {
    score.clamp(SCORE_FLOOR, SCORE_CEILING)
}

/// Apply `n` reports to a starting score.
pub fn after_reports(prior: i64, reports: u32) -> i64 {
    clamp(prior - REPORT_PENALTY * i64::from(reports))
}

/// What the mint budget should be for an IP at `score`, given the configured allowance.
///
/// `None` means "no mints at all".
pub fn mint_allowance(configured: usize, score: i64) -> Option<usize> {
    if score <= MINT_HALT_AT_OR_BELOW {
        None
    } else if score < MINT_THROTTLE_BELOW {
        Some(THROTTLED_MINT_ALLOWANCE.min(configured))
    } else {
        Some(configured)
    }
}

/// Whether a sender's messages into a *stranger's* inbox should be rate-limited.
///
/// Only strangers: once a recipient has added someone as a contact, that decision outranks
/// anything this module inferred. A human saying "I know them" is better evidence than a score.
pub fn throttles_strangers(score: i64) -> bool {
    score <= THROTTLE_BELOW
}

/// Whether a report from an account at `score` counts.
pub fn report_counts(reporter_score: i64) -> bool {
    reporter_score > REPORTER_IGNORED_AT_OR_BELOW
}

/// A plain-language standing, stamped on inbox messages so an agent has something to weigh that
/// isn't a bare number whose scale it has to guess.
pub fn standing(score: i64) -> &'static str {
    if score <= MINT_HALT_AT_OR_BELOW {
        "reported_repeatedly"
    } else if score < 0 {
        "reported"
    } else if score == 0 {
        "unproven"
    } else {
        "established"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paid_handle_starts_above_an_anonymous_mint() {
        // The plan's acceptance criterion, and the whole point of the ladder.
        assert!(Tier::PaidHandle.prior() > Tier::VerifiedHandle.prior());
        assert!(Tier::VerifiedHandle.prior() > Tier::Account.prior());
        assert!(Tier::Account.prior() > Tier::Anonymous.prior());
    }

    #[test]
    fn reports_measurably_drop_a_score() {
        let start = Tier::Account.prior();
        let once = after_reports(start, 1);
        let twice = after_reports(start, 2);
        assert!(once < start, "a report has to cost something");
        assert!(twice < once, "reports accumulate");
    }

    #[test]
    fn scores_stay_inside_the_band() {
        assert_eq!(after_reports(Tier::Anonymous.prior(), 1000), SCORE_FLOOR);
        assert_eq!(clamp(i64::MAX), SCORE_CEILING);
        assert_eq!(clamp(i64::MIN), SCORE_FLOOR);
    }

    #[test]
    fn an_anonymous_mint_is_throttled_into_a_strangers_inbox_but_an_account_is_not() {
        assert!(throttles_strangers(Tier::Anonymous.prior()));
        assert!(!throttles_strangers(Tier::Account.prior()));
        assert!(!throttles_strangers(Tier::PaidHandle.prior()));
    }

    #[test]
    fn a_flooding_ip_is_throttled_then_halted() {
        let configured = 5;
        assert_eq!(
            mint_allowance(configured, 0),
            Some(5),
            "a clean IP is unaffected"
        );
        assert_eq!(
            mint_allowance(configured, MINT_THROTTLE_BELOW - 1),
            Some(THROTTLED_MINT_ALLOWANCE),
            "a reported IP keeps minting, at a trickle"
        );
        assert_eq!(
            mint_allowance(configured, MINT_HALT_AT_OR_BELOW),
            None,
            "an IP that keeps producing reported inboxes stops producing them"
        );
    }

    #[test]
    fn the_throttled_allowance_never_exceeds_what_was_configured() {
        // An operator who set a tighter budget than the throttle must not have it loosened by
        // being throttled, which would make a bad IP better off than a good one.
        assert_eq!(mint_allowance(0, MINT_THROTTLE_BELOW - 1), Some(0));
    }

    #[test]
    fn buried_reporters_cannot_manufacture_downvotes() {
        assert!(report_counts(Tier::Anonymous.prior()));
        assert!(!report_counts(REPORTER_IGNORED_AT_OR_BELOW));
        assert!(!report_counts(SCORE_FLOOR));
    }

    #[test]
    fn standing_reads_as_words_at_every_step_of_the_band() {
        assert_eq!(standing(Tier::Anonymous.prior()), "unproven");
        assert_eq!(standing(Tier::Account.prior()), "established");
        assert_eq!(
            standing(after_reports(Tier::Account.prior(), 1)),
            "reported"
        );
        assert_eq!(standing(SCORE_FLOOR), "reported_repeatedly");
    }
}
