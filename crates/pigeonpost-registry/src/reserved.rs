//! Reserved names for the flat `/handle` tier.
//!
//! Flat handles (`/wodo`) are the one namespace the registry *allocates* rather than reflects, so it
//! is the one namespace that can be squatted — and the one that needs a reserved set. The normative
//! source is `docs/reserved-names.md`; this module is the enforcement, and it must stay a strict
//! subset-or-equal of that document: a name this rejects must be reserved there too.
//!
//! Provider handles (`/github/…`, `/google/…`) are unaffected — a name there is gated on proving the
//! upstream account, so `admin` is only claimable by the account literally named `admin`.

/// Why a flat handle name is refused. `None` means the name is allocatable.
pub fn reserved_reason(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    // Every one- and two-character string is reserved: namespaces are short, and blanket-reserving
    // this space is cheaper than guessing which pairs matter (docs/reserved-names.md §1).
    if lower.chars().count() <= 2 {
        return Some("one- and two-character names are reserved");
    }
    let folded = fold_confusables(&lower);
    for candidate in [lower.as_str(), folded.as_str()] {
        if contains(NAMESPACES, candidate) {
            return Some("reserved for a provider namespace");
        }
        if contains(OPERATIONAL, candidate) {
            return Some("reserved operational name");
        }
        if contains(BRAND, candidate) {
            return Some("reserved brand or trademark");
        }
        if contains(ABUSE, candidate) {
            return Some("reserved (too useful for fraud)");
        }
    }
    None
}

pub fn is_reserved(name: &str) -> bool {
    reserved_reason(name).is_some()
}

/// Map digit look-alikes to letters so `g00gle` and `paypa1` collide with the real thing.
fn fold_confusables(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '0' => 'o',
            '1' => 'l',
            '5' => 's',
            '3' => 'e',
            other => other,
        })
        .collect()
}

/// Membership over a sorted slice. Kept sorted so the tables read cleanly and a `binary_search` is
/// correct; the sets are small enough that speed never matters.
fn contains(table: &[&str], name: &str) -> bool {
    table.binary_search(&name).is_ok()
}

// The tables below MUST stay sorted (a test asserts it) so `binary_search` is correct.

/// Live and candidate provider namespaces — spelled out, because abbreviations are exactly the
/// strings most wanted as flat handles.
const NAMESPACES: &[&str] = &[
    "amazon", "apple", "auth0", "bitbucket", "bluesky", "bsky", "discord", "email", "facebook",
    "github", "gitlab", "google", "instagram", "k", "keybase", "linkedin", "mastodon", "meta",
    "microsoft", "npm", "okta", "orcid", "pypi", "reddit", "signal", "slack", "stackoverflow",
    "telegram", "threads", "tiktok", "twitch", "twitter", "whatsapp", "x", "youtube",
];

/// Confusable with service endpoints, support channels, or system accounts.
const OPERATIONAL: &[&str] = &[
    "about", "abuse", "admin", "administrator", "api", "app", "apps", "auth", "billing", "blog",
    "checkout", "compliance", "contact", "daemon", "default", "demo", "dmca", "docs",
    "documentation", "example", "help", "helpdesk", "home", "hostmaster", "index", "info",
    "internal", "legal", "login", "mail", "moderator", "news", "noreply", "operator", "owner",
    "payment", "postmaster", "press", "pricing", "privacy", "public", "register", "reset", "root",
    "sales", "sandbox", "search", "security", "service", "signin", "signup", "sso", "staging",
    "status", "support", "sys", "system", "terms", "test", "token", "webmaster", "www",
];

/// Pigeonpost's own terms plus the brands most likely targeted for impersonation. Not exhaustive —
/// a trademark register has millions of entries; the dispute policy in `architecture.md` covers the
/// rest.
const BRAND: &[&str] = &[
    "anthropic", "apple", "chatgpt", "claude", "copilot", "directory", "gemini", "google", "loft",
    "mastercard", "meta", "microsoft", "netflix", "nvidia", "official", "openai", "paypal",
    "pigeon", "pigeonpost", "piyote", "post", "registry", "spotify", "staff", "stripe", "team",
    "tesla", "uber", "verified", "visa", "witness", "wodo",
];

/// Terms that make a handle useful for fraud regardless of who holds it.
const ABUSE: &[&str] = &[
    "activate", "airdrop", "alert", "bonus", "claim", "confirm", "free", "giveaway", "invoice",
    "prize", "recovery", "refund", "renew", "reward", "secure", "unlock", "update", "validate",
    "verification", "verify", "wallet", "winner",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn is_sorted_unique(table: &[&str]) -> bool {
        table.windows(2).all(|w| w[0] < w[1])
    }

    #[test]
    fn tables_are_sorted_and_unique() {
        // binary_search is only correct on sorted tables; the tables are hand-maintained.
        for (label, table) in [
            ("namespaces", NAMESPACES),
            ("operational", OPERATIONAL),
            ("brand", BRAND),
            ("abuse", ABUSE),
        ] {
            assert!(is_sorted_unique(table), "{label} table must be sorted and unique");
        }
    }

    #[test]
    fn allocatable_names_pass() {
        for name in ["superaidev", "myagent", "buildbot", "n7k2", "quill"] {
            assert!(reserved_reason(name).is_none(), "{name} should be allocatable");
        }
    }

    #[test]
    fn short_names_are_reserved() {
        for name in ["a", "ab", "x", "gh", "42"] {
            assert!(is_reserved(name), "{name} (<=2 chars) must be reserved");
        }
    }

    #[test]
    fn namespaces_operational_brand_abuse_are_reserved() {
        for name in ["github", "google", "admin", "support", "pigeonpost", "paypal", "airdrop"] {
            assert!(is_reserved(name), "{name} must be reserved");
        }
    }

    #[test]
    fn digit_lookalikes_collide_with_the_real_thing() {
        // g00gle -> google, paypa1 -> paypal, micr0soft -> microsoft, 0penai -> openai
        for name in ["g00gle", "paypa1", "micr0soft", "0penai"] {
            assert!(is_reserved(name), "{name} must fold onto a reserved name");
        }
    }

    #[test]
    fn folding_does_not_over_reserve_a_clean_name() {
        // A name that only *contains* a folded digit but isn't a reserved word stays allocatable.
        assert!(reserved_reason("robot1").is_none());
    }
}
