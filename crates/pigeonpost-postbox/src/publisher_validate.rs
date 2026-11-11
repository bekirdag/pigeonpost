//! Format and check-digit validation for publisher registration (developer program plan §2–§3):
//! IBAN (ISO 13616, per-country length), SWIFT/BIC (ISO 9362), US ABA routing numbers, Turkish
//! TCKN (identity number) and VKN (tax number), MERSİS numbers and D-U-N-S numbers; and the masks
//! every read of bank and tax details shows.
//!
//! A passing check means the number is well formed, not that it exists or belongs to the
//! applicant: that is what the documents, KYC and the reviewer's checklist are for.

/// IBAN lengths by country (SWIFT IBAN registry). A country not listed has no IBAN.
const IBAN_LENGTHS: &[(&str, usize)] = &[
    ("AD", 24),
    ("AE", 23),
    ("AL", 28),
    ("AT", 20),
    ("AZ", 28),
    ("BA", 20),
    ("BE", 16),
    ("BG", 22),
    ("BH", 22),
    ("BR", 29),
    ("BY", 28),
    ("CH", 21),
    ("CR", 22),
    ("CY", 28),
    ("CZ", 24),
    ("DE", 22),
    ("DK", 18),
    ("DO", 28),
    ("EE", 20),
    ("EG", 29),
    ("ES", 24),
    ("FI", 18),
    ("FO", 18),
    ("FR", 27),
    ("GB", 22),
    ("GE", 22),
    ("GI", 23),
    ("GL", 18),
    ("GR", 27),
    ("GT", 28),
    ("HR", 21),
    ("HU", 28),
    ("IE", 22),
    ("IL", 23),
    ("IQ", 23),
    ("IS", 26),
    ("IT", 27),
    ("JO", 30),
    ("KW", 30),
    ("KZ", 20),
    ("LB", 28),
    ("LC", 32),
    ("LI", 21),
    ("LT", 20),
    ("LU", 20),
    ("LV", 21),
    ("LY", 25),
    ("MC", 27),
    ("MD", 24),
    ("ME", 22),
    ("MK", 19),
    ("MR", 27),
    ("MT", 31),
    ("MU", 30),
    ("NL", 18),
    ("NO", 15),
    ("PK", 24),
    ("PL", 28),
    ("PS", 29),
    ("PT", 25),
    ("QA", 29),
    ("RO", 24),
    ("RS", 22),
    ("SA", 24),
    ("SC", 31),
    ("SD", 18),
    ("SE", 24),
    ("SI", 19),
    ("SK", 24),
    ("SM", 27),
    ("ST", 25),
    ("SV", 28),
    ("TL", 23),
    ("TN", 24),
    ("TR", 26),
    ("UA", 29),
    ("VA", 22),
    ("VG", 24),
    ("XK", 20),
];

/// ISO 3166-1 alpha-2 codes (officially assigned), for country fields and BIC country parts.
const COUNTRIES: &str =
    "AD AE AF AG AI AL AM AO AQ AR AS AT AU AW AX AZ BA BB BD BE BF BG BH BI BJ \
BL BM BN BO BQ BR BS BT BV BW BY BZ CA CC CD CF CG CH CI CK CL CM CN CO CR CU CV CW CX CY CZ DE DJ \
DK DM DO DZ EC EE EG EH ER ES ET FI FJ FK FM FO FR GA GB GD GE GF GG GH GI GL GM GN GP GQ GR GS GT \
GU GW GY HK HM HN HR HT HU ID IE IL IM IN IO IQ IR IS IT JE JM JO JP KE KG KH KI KM KN KP KR KW KY \
KZ LA LB LC LI LK LR LS LT LU LV LY MA MC MD ME MF MG MH MK ML MM MN MO MP MQ MR MS MT MU MV MW MX \
MY MZ NA NC NE NF NG NI NL NO NP NR NU NZ OM PA PE PF PG PH PK PL PM PN PR PS PT PW PY QA RE RO RS \
RU RW SA SB SC SD SE SG SH SI SJ SK SL SM SN SO SR SS ST SV SX SY SZ TC TD TF TG TH TJ TK TL TM TN \
TO TR TT TV TW TZ UA UG UM US UY UZ VA VC VE VG VI VN VU WF WS XK YE YT ZA ZM ZW";

pub(crate) fn country_ok(code: &str) -> bool {
    code.len() == 2
        && code.bytes().all(|b| b.is_ascii_uppercase())
        && COUNTRIES.split_whitespace().any(|c| c == code)
}

/// Spaces removed and upper-cased: how an IBAN, BIC or tax id is stored and compared.
pub(crate) fn compact(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect::<String>()
        .to_ascii_uppercase()
}

/// ISO 7064 mod 97-10 over an IBAN's rearranged characters (letters as 10..35).
fn mod97(iban: &str) -> u32 {
    let rearranged = iban[4..].chars().chain(iban[..4].chars());
    let mut rem: u32 = 0;
    for c in rearranged {
        let v = match c {
            '0'..='9' => c as u32 - '0' as u32,
            'A'..='Z' => c as u32 - 'A' as u32 + 10,
            _ => return 0,
        };
        rem = if v >= 10 {
            (rem * 100 + v) % 97
        } else {
            (rem * 10 + v) % 97
        };
    }
    rem
}

/// A well-formed IBAN for a country that issues them: known country, the registry length,
/// alphanumeric, and the mod-97 check equal to 1. Returns the compact form.
pub(crate) fn iban(raw: &str) -> Result<String, &'static str> {
    let s = compact(raw);
    if s.len() < 15 || !s.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err("iban_format");
    }
    let country = &s[..2];
    let Some(&(_, len)) = IBAN_LENGTHS.iter().find(|(c, _)| *c == country) else {
        return Err("iban_country");
    };
    if s.len() != len || !s[2..4].bytes().all(|b| b.is_ascii_digit()) {
        return Err("iban_length");
    }
    // Turkey: TR + 2 check digits + 5-digit bank code + 1 reserved '0' + 16 account characters,
    // all numeric in practice.
    if country == "TR" && !s[2..].bytes().all(|b| b.is_ascii_digit()) {
        return Err("iban_format");
    }
    if mod97(&s) != 1 {
        return Err("iban_checksum");
    }
    Ok(s)
}

/// A BIC: 4-letter institution, ISO country, 2-character location, optional 3-character branch.
pub(crate) fn swift_bic(raw: &str) -> Result<String, &'static str> {
    let s = compact(raw);
    let b = s.as_bytes();
    if !(s.len() == 8 || s.len() == 11)
        || !b[..4].iter().all(u8::is_ascii_uppercase)
        || !b[4..]
            .iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return Err("swift_format");
    }
    if !country_ok(&s[4..6]) {
        return Err("swift_country");
    }
    Ok(s)
}

/// A US ABA routing number: nine digits, 3·(d1+d4+d7) + 7·(d2+d5+d8) + (d3+d6+d9) ≡ 0 (mod 10).
pub(crate) fn aba_routing(raw: &str) -> Result<String, &'static str> {
    let s = compact(raw);
    let d: Vec<u32> = s.chars().filter_map(|c| c.to_digit(10)).collect();
    if s.len() != 9 || d.len() != 9 {
        return Err("routing_format");
    }
    let sum = 3 * (d[0] + d[3] + d[6]) + 7 * (d[1] + d[4] + d[7]) + (d[2] + d[5] + d[8]);
    if !sum.is_multiple_of(10) {
        return Err("routing_checksum");
    }
    Ok(s)
}

/// A bank account number for SWIFT or ACH payouts: 4–34 letters and digits (ACH: 4–17 digits).
pub(crate) fn account_number(raw: &str, ach: bool) -> Result<String, &'static str> {
    let s = compact(raw);
    let ok = if ach {
        (4..=17).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
    } else {
        (4..=34).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric())
    };
    if ok {
        Ok(s)
    } else {
        Err("account_number_format")
    }
}

fn digits(s: &str, n: usize) -> Option<Vec<u32>> {
    let d: Vec<u32> = s.chars().filter_map(|c| c.to_digit(10)).collect();
    (s.len() == n && d.len() == n).then_some(d)
}

/// T.C. Kimlik No: 11 digits, the first not 0;
/// d10 = ((d1+d3+d5+d7+d9)·7 − (d2+d4+d6+d8)) mod 10 and d11 = (d1+…+d10) mod 10.
pub(crate) fn tckn(raw: &str) -> Result<String, &'static str> {
    let s = compact(raw);
    let Some(d) = digits(&s, 11) else {
        return Err("tckn_format");
    };
    if d[0] == 0 {
        return Err("tckn_format");
    }
    let odd = d[0] + d[2] + d[4] + d[6] + d[8];
    let even = d[1] + d[3] + d[5] + d[7];
    let d10 = ((odd * 7) as i64 - even as i64).rem_euclid(10) as u32;
    let d11 = d[..10].iter().sum::<u32>() % 10;
    if d[9] != d10 || d[10] != d11 {
        return Err("tckn_checksum");
    }
    Ok(s)
}

/// Vergi Kimlik No (10 digits): for each of the first nine digits, p = (d + 9 − i) mod 10 and
/// q = p·2^(9−i) mod 9, with q = 9 when p ≠ 0 and q = 0; the tenth digit is (10 − Σq mod 10) mod 10.
pub(crate) fn vkn(raw: &str) -> Result<String, &'static str> {
    let s = compact(raw);
    let Some(d) = digits(&s, 10) else {
        return Err("vkn_format");
    };
    let mut sum = 0u32;
    for (i, &digit) in d.iter().take(9).enumerate() {
        let p = (digit + 9 - i as u32) % 10;
        let mut q = (p * (1u32 << (9 - i))) % 9;
        if p != 0 && q == 0 {
            q = 9;
        }
        sum += q;
    }
    if d[9] != (10 - sum % 10) % 10 {
        return Err("vkn_checksum");
    }
    Ok(s)
}

/// MERSİS number: 16 digits (format only; the registry has no public check digit).
pub(crate) fn mersis(raw: &str) -> Result<String, &'static str> {
    let s = compact(raw);
    digits(&s, 16).map(|_| s.clone()).ok_or("mersis_format")
}

/// D-U-N-S number: 9 digits (format only).
pub(crate) fn duns(raw: &str) -> Result<String, &'static str> {
    let s = compact(raw);
    digits(&s, 9).map(|_| s.clone()).ok_or("duns_format")
}

/// A foreign tax id: 2–32 letters, digits and dashes (EIN, VAT, TIN …), stored compact.
pub(crate) fn foreign_tax_id(raw: &str) -> Result<String, &'static str> {
    let s: String = raw.trim().to_ascii_uppercase();
    if (2..=32).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        Ok(s)
    } else {
        Err("tax_id_format")
    }
}

// ---- masks ---------------------------------------------------------------------------------------

/// Only the last `keep` characters; everything before them is a bullet.
pub(crate) fn mask_tail(s: &str, keep: usize) -> String {
    let n = s.chars().count();
    let keep = keep.min(n.saturating_sub(1));
    s.chars()
        .enumerate()
        .map(|(i, c)| if i + keep >= n { c } else { '•' })
        .collect()
}

/// `TR33 •••• •••• •••• •••• •••• 26`: the country and check digits (needed to tell accounts
/// apart, and not secret) and the last four characters.
pub(crate) fn mask_iban(iban: &str) -> String {
    let n = iban.len();
    if n < 8 {
        return mask_tail(iban, 0);
    }
    let middle: String = iban[4..n - 4].chars().map(|_| '•').collect();
    let grouped = format!("{}{}{}", &iban[..4], middle, &iban[n - 4..]);
    grouped
        .chars()
        .collect::<Vec<_>>()
        .chunks(4)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ibans() {
        for ok in [
            "TR330006100519786457841326",
            "TR33 0006 1005 1978 6457 8413 26",
            "GB82WEST12345698765432",
            "DE89370400440532013000",
            "NL91ABNA0417164300",
            "FR1420041010050500013M02606",
        ] {
            assert!(iban(ok).is_ok(), "{ok}");
        }
        assert_eq!(iban("TR330006100519786457841327"), Err("iban_checksum"));
        assert_eq!(iban("GB82WEST1234569876543"), Err("iban_length"));
        assert_eq!(iban("US12345678901234567890"), Err("iban_country"));
        assert_eq!(iban("TR33000610051978645784132A"), Err("iban_format"));
        assert_eq!(iban("short"), Err("iban_format"));
        assert_eq!(
            iban("DE89 3704 0044 0532 0130 00").unwrap(),
            "DE89370400440532013000"
        );
    }

    #[test]
    fn swift_codes() {
        assert_eq!(swift_bic("deut de ff").unwrap(), "DEUTDEFF");
        assert!(swift_bic("TGBATRIS").is_ok());
        assert!(swift_bic("CHASUS33XXX").is_ok());
        assert_eq!(swift_bic("DEUTDEF"), Err("swift_format"));
        assert_eq!(swift_bic("DEU1DEFF"), Err("swift_format"));
        assert_eq!(swift_bic("DEUTQQFF"), Err("swift_country"));
    }

    #[test]
    fn routing_numbers() {
        assert!(aba_routing("021000021").is_ok());
        assert!(aba_routing("011000015").is_ok());
        assert_eq!(aba_routing("021000022"), Err("routing_checksum"));
        assert_eq!(aba_routing("02100002"), Err("routing_format"));
        assert!(account_number("000123456789", true).is_ok());
        assert_eq!(account_number("12AB", true), Err("account_number_format"));
        assert!(account_number("12AB34", false).is_ok());
    }

    #[test]
    fn turkish_identity_numbers() {
        assert!(tckn("10000000146").is_ok());
        assert_eq!(tckn("10000000147"), Err("tckn_checksum"));
        assert_eq!(tckn("01000000146"), Err("tckn_format"));
        assert_eq!(tckn("1000000014"), Err("tckn_format"));
        assert_eq!(tckn("1000000014a"), Err("tckn_format"));
    }

    /// Every VKN has exactly one check digit that passes; the algorithm is cross-checked against
    /// a direct transcription of the published `(d + 10 − i) mod 10` form.
    #[test]
    fn turkish_tax_numbers() {
        fn reference(nine: &str) -> u32 {
            let mut sum = 0u32;
            for (i, c) in nine.chars().enumerate() {
                let tmp = (c.to_digit(10).unwrap() + 10 - (i as u32 + 1)) % 10;
                sum += if tmp == 9 {
                    9
                } else {
                    (tmp * 2u32.pow(10 - (i as u32 + 1))) % 9
                };
            }
            (10 - sum % 10) % 10
        }
        for nine in [
            "123456789",
            "000000000",
            "999999999",
            "313055766",
            "476004123",
        ] {
            let check = reference(nine);
            let good = format!("{nine}{check}");
            assert!(vkn(&good).is_ok(), "{good}");
            let bad = format!("{nine}{}", (check + 1) % 10);
            assert_eq!(vkn(&bad), Err("vkn_checksum"), "{bad}");
        }
        assert_eq!(vkn("12345"), Err("vkn_format"));
    }

    #[test]
    fn registry_numbers_and_tax_ids() {
        assert!(mersis("0123456789012345").is_ok());
        assert_eq!(mersis("012345678901234"), Err("mersis_format"));
        assert!(duns("150483782").is_ok());
        assert_eq!(duns("15-048-3782").unwrap(), "150483782");
        assert!(foreign_tax_id("12-3456789").is_ok());
        assert_eq!(foreign_tax_id("x"), Err("tax_id_format"));
        assert!(country_ok("TR") && country_ok("US") && !country_ok("QQ") && !country_ok("tr"));
    }

    #[test]
    fn masks_show_only_the_tail() {
        let m = mask_iban("TR330006100519786457841326");
        assert_eq!(m, "TR33 •••• •••• •••• •••• ••13 26");
        assert!(!m.contains("0006") && !m.contains("8457"));
        assert_eq!(mask_tail("10000000146", 2), "•••••••••46");
        assert_eq!(mask_tail("ab", 4), "•b");
    }
}
