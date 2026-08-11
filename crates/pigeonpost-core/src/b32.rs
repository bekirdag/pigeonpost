//! Crockford base32, lowercase, no padding.
//!
//! Chosen over RFC 4648 because addresses get read aloud, typed from a README, and pasted into
//! chat: Crockford excludes `i`, `l`, `o`, and `u`, which removes the 1/l and 0/O confusions and
//! the most common accidental profanity.

use crate::error::{Error, Result};

const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// Encode bytes as Crockford base32. Output length is `ceil(bytes * 8 / 5)`.
pub fn encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(5) * 8);
    let mut buffer: u16 = 0;
    let mut bits: u8 = 0;

    for &byte in input {
        buffer = (buffer << 8) | u16::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let index = ((buffer >> bits) & 0x1f) as usize;
            out.push(ALPHABET[index] as char);
        }
    }

    if bits > 0 {
        let index = ((buffer << (5 - bits)) & 0x1f) as usize;
        out.push(ALPHABET[index] as char);
    }

    out
}

/// Decode Crockford base32, accepting the ambiguous characters the alphabet excludes:
/// `i`/`l` map to `1`, `o` maps to `0`. Case-insensitive.
pub fn decode(input: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 5 / 8);
    let mut buffer: u16 = 0;
    let mut bits: u8 = 0;

    for ch in input.chars() {
        let value = decode_char(ch)?;
        buffer = (buffer << 5) | u16::from(value);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }

    Ok(out)
}

fn decode_char(ch: char) -> Result<u8> {
    let lower = ch.to_ascii_lowercase();
    match lower {
        '0' | 'o' => Ok(0),
        '1' | 'i' | 'l' => Ok(1),
        '2'..='9' => Ok(lower as u8 - b'0'),
        'a'..='h' => Ok(lower as u8 - b'a' + 10),
        'j' | 'k' => Ok(lower as u8 - b'a' + 9),
        'm' | 'n' => Ok(lower as u8 - b'a' + 8),
        'p'..='t' => Ok(lower as u8 - b'a' + 7),
        'v'..='z' => Ok(lower as u8 - b'a' + 6),
        _ => Err(Error::InvalidBase32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alphabet_is_the_crockford_one() {
        assert_eq!(ALPHABET.len(), 32);
        for excluded in b"ilou" {
            assert!(!ALPHABET.contains(excluded), "crockford excludes these");
        }
    }

    #[test]
    fn round_trips() {
        for len in 0..40usize {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 7 + 3) as u8).collect();
            let encoded = encode(&bytes);
            let decoded = decode(&encoded).expect("decodes");
            assert_eq!(&decoded[..bytes.len()], &bytes[..], "len {len}");
        }
    }

    #[test]
    fn every_alphabet_character_decodes_to_its_index() {
        for (index, &ch) in ALPHABET.iter().enumerate() {
            assert_eq!(decode_char(ch as char).unwrap() as usize, index);
        }
    }

    #[test]
    fn ambiguous_characters_map_to_their_lookalikes() {
        assert_eq!(decode_char('i').unwrap(), decode_char('1').unwrap());
        assert_eq!(decode_char('l').unwrap(), decode_char('1').unwrap());
        assert_eq!(decode_char('o').unwrap(), decode_char('0').unwrap());
        assert_eq!(decode_char('I').unwrap(), decode_char('1').unwrap());
    }

    #[test]
    fn rejects_characters_outside_the_alphabet() {
        assert_eq!(decode_char('u'), Err(Error::InvalidBase32));
        assert_eq!(decode_char('-'), Err(Error::InvalidBase32));
        assert_eq!(decode_char(' '), Err(Error::InvalidBase32));
    }
}
