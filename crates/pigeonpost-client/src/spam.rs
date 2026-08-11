//! Client-side spam control — layers 4 and 5 of `docs/spam.md`.
//!
//! These live here, not in the loft, and not by accident: gift wrapping hides the sender from the
//! loft, so **anything keyed on sender identity is necessarily client-side**. A loft enforces a
//! flat floor; the gradient is applied after unwrap, where the sender is finally known.
//!
//! Two mechanisms:
//!
//! - **`acceptAll = false`** (the default): mail from a stranger lands in a *pending* queue rather
//!   than the inbox. An agent that never opens its inbox to strangers has no spam problem at all.
//! - **A local sender score**, decremented by mark-as-spam. Never shared, never published, never
//!   consulted by infrastructure — so a spammer must earn its reputation separately with every
//!   victim, and learns nothing about why it was dropped.

use serde::{Deserialize, Serialize};

/// Score given to a sender the operator has mailed. Implies allowlisting.
pub const SCORE_CORRESPONDED: i64 = 100;
/// Small credit for mail that arrived and was not flagged.
pub const SCORE_ACCEPTED: i64 = 5;
/// Small credit for holding an OIDC-backed handle — cheap to create, but not free.
pub const SCORE_HAS_HANDLE: i64 = 10;
/// What one mark-as-spam costs.
pub const SCORE_MARKED_SPAM: i64 = -40;

/// Highest proof-of-work floor supported by the v0.2 client.
///
/// Eighteen bits preserves the protocol/conformance baseline while keeping one genuine send
/// within a bounded interactive budget. Signed recipient records above this are refused before
/// encryption, trust mutation, or durable queueing.
pub const MAX_SUPPORTED_POW_BITS: u32 = 18;

/// At or below this, mail is dropped silently at unwrap.
pub const DROP_THRESHOLD: i64 = -60;

/// Scores drift back toward neutral at this many points per day.
///
/// A mistaken flag must not be a life sentence, and a compromised-then-recovered key has to be
/// able to come back. Two marks still take roughly a month to forgive.
pub const DECAY_PER_DAY: i64 = 2;

const SECONDS_PER_DAY: u64 = 86_400;

/// What to do with a message once the sender is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    /// Straight to the inbox: a known correspondent.
    Accept,
    /// Held for the operator to look at. The default for strangers.
    Pending,
    /// Discarded at unwrap. The sender is told nothing.
    Drop,
}

/// Everything known about a sender at the moment a message is opened.
#[derive(Debug, Clone, Copy)]
pub struct SenderContext {
    pub allowlisted: bool,
    pub raw_score: i64,
    pub score_updated_at: u64,
    pub has_handle: bool,
}

/// A score aged toward neutral. Decay is computed on read rather than written by a sweep, so
/// there is no background job and no daemon (requirement 7).
pub fn effective_score(raw: i64, updated_at: u64, now: u64) -> i64 {
    if raw == 0 || updated_at == 0 || now <= updated_at {
        return raw;
    }
    let days = (now - updated_at) / SECONDS_PER_DAY;
    let decay = (days as i64).saturating_mul(DECAY_PER_DAY);

    if raw > 0 {
        (raw - decay).max(0)
    } else {
        (raw + decay).min(0)
    }
}

/// Decide what happens to a message.
///
/// `accept_all` is the `acceptAll` setting: false by default, which is what makes a published
/// address safe to put in a README.
pub fn decide(context: &SenderContext, accept_all: bool, now: u64) -> Disposition {
    // A current witnessed handle is a small client-side advantage, never an acceptance bypass.
    // It can rescue a borderline sender from silent drop into the ordinary pending review path;
    // unknown first contacts still require allowlisting or an explicitly open inbox.
    let score = effective_score(context.raw_score, context.score_updated_at, now).saturating_add(
        if context.has_handle {
            SCORE_HAS_HANDLE
        } else {
            0
        },
    );

    // Dropping wins over allowlisting: an explicit, repeated "this is spam" from the operator is
    // a stronger signal than a stale allowlist entry.
    if score <= DROP_THRESHOLD {
        return Disposition::Drop;
    }
    if context.allowlisted {
        return Disposition::Accept;
    }
    if accept_all {
        return Disposition::Accept;
    }
    Disposition::Pending
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_786_105_721;

    fn stranger() -> SenderContext {
        SenderContext {
            allowlisted: false,
            raw_score: 0,
            score_updated_at: 0,
            has_handle: false,
        }
    }

    #[test]
    fn a_stranger_is_held_pending_by_default() {
        assert_eq!(decide(&stranger(), false, NOW), Disposition::Pending);
    }

    #[test]
    fn accept_all_lets_strangers_through() {
        assert_eq!(decide(&stranger(), true, NOW), Disposition::Accept);
    }

    #[test]
    fn a_correspondent_is_accepted() {
        let known = SenderContext {
            allowlisted: true,
            ..stranger()
        };
        assert_eq!(decide(&known, false, NOW), Disposition::Accept);
    }

    #[test]
    fn a_repeatedly_flagged_sender_is_dropped_silently() {
        let spammer = SenderContext {
            raw_score: SCORE_MARKED_SPAM * 2,
            score_updated_at: NOW,
            ..stranger()
        };
        assert_eq!(decide(&spammer, false, NOW), Disposition::Drop);
        assert_eq!(
            decide(&spammer, true, NOW),
            Disposition::Drop,
            "opening the inbox must not un-drop someone the operator flagged"
        );
    }

    #[test]
    fn dropping_overrides_a_stale_allowlist_entry() {
        let turned = SenderContext {
            allowlisted: true,
            raw_score: SCORE_MARKED_SPAM * 2,
            score_updated_at: NOW,
            has_handle: false,
        };
        assert_eq!(decide(&turned, false, NOW), Disposition::Drop);
    }

    #[test]
    fn one_flag_is_not_enough_to_drop() {
        // Marking a single message must not silently blackhole a correspondent.
        let flagged = SenderContext {
            raw_score: SCORE_MARKED_SPAM,
            score_updated_at: NOW,
            ..stranger()
        };
        assert_eq!(decide(&flagged, false, NOW), Disposition::Pending);
    }

    #[test]
    fn a_verified_handle_only_rescues_a_borderline_drop_to_pending() {
        let borderline = SenderContext {
            raw_score: DROP_THRESHOLD,
            score_updated_at: NOW,
            has_handle: true,
            ..stranger()
        };
        assert_eq!(decide(&borderline, false, NOW), Disposition::Pending);
        assert_eq!(
            decide(&borderline, true, NOW),
            Disposition::Accept,
            "acceptance still comes from the explicit open-inbox setting"
        );

        let deeply_negative = SenderContext {
            raw_score: DROP_THRESHOLD - SCORE_HAS_HANDLE,
            ..borderline
        };
        assert_eq!(decide(&deeply_negative, false, NOW), Disposition::Drop);

        let first_contact = SenderContext {
            has_handle: true,
            ..stranger()
        };
        assert_eq!(decide(&first_contact, false, NOW), Disposition::Pending);
    }

    #[test]
    fn a_negative_score_decays_back_toward_neutral() {
        let raw = SCORE_MARKED_SPAM * 2; // -80
        assert_eq!(effective_score(raw, NOW, NOW), -80);
        assert_eq!(effective_score(raw, NOW, NOW + 10 * SECONDS_PER_DAY), -60);
        assert_eq!(effective_score(raw, NOW, NOW + 40 * SECONDS_PER_DAY), 0);
    }

    #[test]
    fn decay_never_overshoots_past_neutral() {
        assert_eq!(effective_score(-10, NOW, NOW + 3650 * SECONDS_PER_DAY), 0);
        assert_eq!(effective_score(50, NOW, NOW + 3650 * SECONDS_PER_DAY), 0);
    }

    #[test]
    fn a_forgiven_spammer_can_be_heard_again() {
        // A mistaken flag is not a life sentence.
        let context = SenderContext {
            raw_score: SCORE_MARKED_SPAM * 2,
            score_updated_at: NOW,
            ..stranger()
        };
        assert_eq!(decide(&context, false, NOW), Disposition::Drop);
        assert_eq!(
            decide(&context, false, NOW + 30 * SECONDS_PER_DAY),
            Disposition::Pending,
            "a month later the sender gets another hearing"
        );
    }

    #[test]
    fn clock_going_backwards_does_not_inflate_a_score() {
        assert_eq!(effective_score(-80, NOW, NOW - 10_000), -80);
    }
}
