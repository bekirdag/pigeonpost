//! Small shared helpers: identifiers, secrets, hashing and time.

use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Seconds since the Unix epoch.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Lower-case hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `n` random bytes from the OS, as hex.
pub fn random_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    OsRng.fill_bytes(&mut buf);
    let out = hex(&buf);
    zeroize::Zeroize::zeroize(&mut buf);
    out
}

/// SHA-256 as hex.
pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

/// Constant-time comparison of two hex digests (or any two strings of public length).
pub fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

/// A Nest id as the postbox mints it: `nst_` and 16 to 64 lower-case hex digits.
///
/// Every id that reaches a path, a volume name or a container name passes this first, so none of
/// them can carry a separator, an option or a shell character.
pub fn valid_nest_id(id: &str) -> bool {
    match id.strip_prefix("nst_") {
        Some(rest) => {
            (16..=64).contains(&rest.len())
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }
        None => false,
    }
}

/// The current usage period, `YYYY-MM` in UTC. Budgets reset at each period boundary; a
/// reservation is always settled against the period it was taken in.
pub fn period_of(unix: i64) -> String {
    // Civil-from-days (Howard Hinnant), so no date crate is needed for one calendar field.
    let days = unix.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nest_ids_are_strict() {
        assert!(valid_nest_id("nst_0123456789abcdef"));
        assert!(!valid_nest_id("nst_0123456789ABCDEF"));
        assert!(!valid_nest_id("nst_short"));
        assert!(!valid_nest_id("nst_0123456789abcdef/../x"));
        assert!(!valid_nest_id("0123456789abcdef0123"));
        assert!(!valid_nest_id("nst_0123456789abcdef --rm"));
    }

    #[test]
    fn periods_follow_the_utc_calendar() {
        assert_eq!(period_of(0), "1970-01");
        // 2026-11-03T06:00:00Z
        assert_eq!(period_of(1_793_685_600), "2026-11");
        // 2026-12-31T23:59:59Z and the second after it
        assert_eq!(period_of(1_798_761_599), "2026-12");
        assert_eq!(period_of(1_798_761_600), "2027-01");
        // leap day 2028-02-29T12:00:00Z
        assert_eq!(period_of(1_835_438_400), "2028-02");
    }

    #[test]
    fn constant_time_compare() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abcd"));
    }
}
