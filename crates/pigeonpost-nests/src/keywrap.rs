//! Off-host LUKS unlock (Nests Phase B6, R-73).
//!
//! Each Nest's volume key (64 random bytes, LUKS2) is stored on this host only **wrapped** under a
//! host key-encryption key (KEK). The KEK is never written here: the postbox holds it and releases
//! it to nestd over the private WireGuard link after every start (`POST /v1/unlock`). Until then
//! nestd is *locked*: no volume can be opened, no Nest started or created.
//!
//! What this changes, stated plainly: a copy of this host's disk, a VMware snapshot of it or a
//! backup archive no longer contains a usable volume key, so Nest data at rest is protected
//! against everything that is not a live compromise of the running nestd (whose memory holds the
//! KEK) or of the postbox host (which holds it on disk).
//!
//! Format of a wrapped key file: `NWK1` ‖ 12-byte nonce ‖ ChaCha20-Poly1305(KEK, nonce, key,
//! aad = "pigeonpost-nest-key-v1:" ‖ nest_id). Binding the Nest id means a key file moved to
//! another Nest's name does not open anything.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use std::sync::Mutex;
use zeroize::Zeroize;

const MAGIC: &[u8; 4] = b"NWK1";
const AAD: &str = "pigeonpost-nest-key-v1:";

/// The KEK held in memory, once released.
#[derive(Default)]
pub struct Vault {
    kek: Mutex<Option<[u8; 32]>>,
}

impl Drop for Vault {
    fn drop(&mut self) {
        if let Ok(mut k) = self.kek.lock() {
            if let Some(k) = k.as_mut() {
                k.zeroize();
            }
        }
    }
}

/// A short, non-secret fingerprint of a KEK, recorded on the host so a wrong KEK is refused
/// instead of failing later on every volume.
pub fn fingerprint(kek: &[u8; 32]) -> String {
    use sha2::Digest;
    let d = sha2::Sha256::digest([b"pigeonpost-nest-kek-fpr-v1:".as_slice(), kek].concat());
    d[..16].iter().map(|b| format!("{b:02x}")).collect()
}

impl Vault {
    pub fn new() -> Vault {
        Vault::default()
    }

    pub fn locked(&self) -> bool {
        self.kek.lock().map(|k| k.is_none()).unwrap_or(true)
    }

    /// Accept the KEK. `expected` is the fingerprint recorded on first unlock: a different KEK is
    /// refused. Returns the fingerprint to record (first unlock) or the existing one.
    pub fn unlock(&self, mut kek: [u8; 32], expected: Option<&str>) -> Result<String, String> {
        let fpr = fingerprint(&kek);
        if expected.is_some_and(|e| e != fpr) {
            kek.zeroize();
            return Err("this is not the key this host was set up with".into());
        }
        let mut slot = self.kek.lock().map_err(|_| "vault poisoned".to_string())?;
        if let Some(existing) = slot.as_ref() {
            let same = fingerprint(existing) == fpr;
            kek.zeroize();
            return if same {
                Ok(fpr)
            } else {
                Err("already unlocked with a different key".into())
            };
        }
        *slot = Some(kek);
        kek.zeroize();
        Ok(fpr)
    }

    fn cipher(&self) -> Result<ChaCha20Poly1305, String> {
        let slot = self.kek.lock().map_err(|_| "vault poisoned".to_string())?;
        let kek = slot
            .as_ref()
            .ok_or("locked: the host key has not been released")?;
        Ok(ChaCha20Poly1305::new(Key::from_slice(kek)))
    }

    pub fn wrap(&self, nest_id: &str, key: &[u8]) -> Result<Vec<u8>, String> {
        let mut nonce = [0u8; 12];
        rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut nonce);
        let aad = format!("{AAD}{nest_id}");
        let ct = self
            .cipher()?
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: key,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| "wrap failed".to_string())?;
        let mut out = MAGIC.to_vec();
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    pub fn unwrap(&self, nest_id: &str, file: &[u8]) -> Result<Vec<u8>, String> {
        if !is_wrapped(file) || file.len() < 4 + 12 + 16 {
            return Err("not a wrapped key".into());
        }
        let aad = format!("{AAD}{nest_id}");
        self.cipher()?
            .decrypt(
                Nonce::from_slice(&file[4..16]),
                Payload {
                    msg: &file[16..],
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| "this key file does not belong to this Nest or this host".to_string())
    }
}

pub fn is_wrapped(file: &[u8]) -> bool {
    file.starts_with(MAGIC)
}

/// Parse a KEK given as 64 hex characters.
pub fn parse_kek(hex: &str) -> Option<[u8; 32]> {
    let hex = hex.trim();
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_wrap_to_their_nest_and_need_the_right_kek() {
        let v = Vault::new();
        assert!(v.locked());
        assert!(
            v.wrap("nst_1", b"k").is_err(),
            "nothing is wrapped while locked"
        );
        let kek = [7u8; 32];
        let fpr = v.unlock(kek, None).unwrap();
        assert!(!v.locked());
        let w = v.wrap("nst_1", &[9u8; 64]).unwrap();
        assert!(is_wrapped(&w));
        assert_eq!(v.unwrap("nst_1", &w).unwrap(), vec![9u8; 64]);
        assert!(v.unwrap("nst_2", &w).is_err(), "bound to its Nest");
        let mut tampered = w.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(v.unwrap("nst_1", &tampered).is_err());
        // Same KEK again is fine; a different one is refused, on a fresh vault too.
        assert!(v.unlock(kek, Some(&fpr)).is_ok());
        assert!(v.unlock([8u8; 32], None).is_err());
        let other = Vault::new();
        assert!(other.unlock([8u8; 32], Some(&fpr)).is_err());
        assert!(other.locked());
        let other = Vault::new();
        other.unlock([8u8; 32], None).unwrap();
        assert!(
            other.unwrap("nst_1", &w).is_err(),
            "another host's key does not open it"
        );
    }

    #[test]
    fn kek_hex_is_strict() {
        assert_eq!(parse_kek(&"ab".repeat(32)), Some([0xab; 32]));
        assert!(parse_kek("ab").is_none());
        assert!(parse_kek(&"zz".repeat(32)).is_none());
    }
}
