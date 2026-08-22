//! Where an attachment's bytes live.
//!
//! Content-addressed by SHA-256: the digest *is* the path, so the same file sent twice is stored
//! once, a corrupted read is detectable, and nothing a caller says about a file can decide where it
//! lands on disk. Filenames arrive from other agents and are kept only as a label to show a person.
//! They never touch the filesystem — a name is data, not a path.
//!
//! Blobs are immutable. There is no update, and delete exists only for retention: a mailbox
//! deleting a message releases its claim, and the bytes go when the last claim does.

use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Two levels of fan-out before the file. A single directory of a million entries is slow to read
/// on every filesystem worth using, and `ext4` in particular degrades badly.
fn shard(hex: &str) -> PathBuf {
    Path::new(&hex[0..2]).join(&hex[2..4]).join(hex)
}

pub struct Blobs {
    root: PathBuf,
    /// Ceiling on one upload. The volume is finite and a single request should not be able to fill
    /// it; the per-mailbox quota bounds the total, this bounds the blast radius of one call.
    max_bytes: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    #[error("attachment is larger than this postbox accepts")]
    TooLarge,
    #[error("attachment storage is unavailable")]
    Io(#[from] std::io::Error),
}

impl Blobs {
    /// Read the configuration, or decide attachments are off.
    ///
    /// Absence is not an error: a deployment with nowhere to put bytes should refuse attachments
    /// clearly rather than accept them and lose them.
    pub fn from_env() -> Option<Self> {
        let root = non_empty("PIGEONPOST_BLOB_DIR")?;
        let root = PathBuf::from(root);
        if let Err(e) = std::fs::create_dir_all(&root) {
            tracing::error!(error = %e, path = %root.display(), "blob directory unusable — attachments disabled");
            return None;
        }
        let max_bytes = std::env::var("PIGEONPOST_BLOB_MAX_MB")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(100)
            * 1024
            * 1024;
        tracing::info!(path = %root.display(), max_mb = max_bytes / 1024 / 1024, "attachments configured");
        Some(Self { root, max_bytes })
    }

    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Store bytes, returning their digest. Writing the same content twice is a no-op the second
    /// time — the digest already names a file that is byte-for-byte what was asked for.
    pub fn put(&self, bytes: &[u8]) -> Result<String, BlobError> {
        if bytes.len() > self.max_bytes {
            return Err(BlobError::TooLarge);
        }
        let hex = hex_digest(bytes);
        let target = self.root.join(shard(&hex));
        if target.is_file() {
            return Ok(hex);
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Write beside the target and rename, so a reader never sees a half-written blob and an
        // interrupted upload leaves a temp file rather than a file that lies about its digest.
        let temp = target.with_extension(format!("part-{}", std::process::id()));
        {
            let mut file = std::fs::File::create(&temp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        std::fs::rename(&temp, &target)?;
        Ok(hex)
    }

    /// Read bytes back, refusing anything that is not a digest this module could have written.
    ///
    /// The check is not decoration: `sha256` reaches this from a database row, and a row is only
    /// as trustworthy as everything that can write to it. Forty hex characters cannot contain
    /// `..`, a separator, or anything else that leaves the root.
    pub fn get(&self, sha256: &str) -> Option<Vec<u8>> {
        if !is_digest(sha256) {
            return None;
        }
        std::fs::read(self.root.join(shard(sha256))).ok()
    }

    /// Remove a blob. Only ever called once nothing refers to it.
    pub fn remove(&self, sha256: &str) {
        if !is_digest(sha256) {
            return;
        }
        let _ = std::fs::remove_file(self.root.join(shard(sha256)));
    }
}

fn is_digest(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().fold(String::with_capacity(64), |mut out, b| {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// What to answer `Content-Type` with.
///
/// An allowlist, and everything else is `application/octet-stream`. The reason is narrow and
/// important: these files are uploaded by one agent and downloaded by another, from the postbox's
/// own origin. Serving an attacker-supplied `text/html` there would be stored cross-site
/// scripting against every other caller of this API. Nothing in this list can execute in a
/// browsing context.
///
/// The type is also never taken from the filename — a caller controls that too.
pub fn safe_content_type(claimed: &str) -> &'static str {
    match claimed.trim().to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => "image/jpeg",
        "image/png" => "image/png",
        "image/gif" => "image/gif",
        "image/webp" => "image/webp",
        "image/heic" => "image/heic",
        "video/mp4" => "video/mp4",
        "video/quicktime" => "video/quicktime",
        "audio/mpeg" => "audio/mpeg",
        "audio/mp4" | "audio/m4a" => "audio/mp4",
        "application/pdf" => "application/pdf",
        // Everything else — archives, documents, source, the unknown — is bytes to save, not
        // content to render. That includes `text/plain`, which a browser will happily display and
        // which can carry a payload for whatever reads it next.
        _ => "application/octet-stream",
    }
}

/// A filename fit to put in a header.
///
/// Kept for display only, and stripped of everything that could make it a path or break out of
/// the header it travels in. An empty result becomes a generic name rather than nothing.
pub fn safe_filename(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | '"' | '\r' | '\n' | '\0'))
        .take(120)
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').to_string();
    if trimmed.is_empty() {
        "attachment".to_string()
    } else {
        trimmed
    }
}

fn non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of this test's own. Same shape the store's own tests use — no dev-dependency
    /// added for a temp path.
    fn store() -> Blobs {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("pp-blobs-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Blobs {
            root,
            max_bytes: 1024,
        }
    }

    #[test]
    fn the_same_content_is_stored_once() {
        let blobs = store();
        let a = blobs.put(b"hello").unwrap();
        let b = blobs.put(b"hello").unwrap();
        assert_eq!(a, b);
        assert_eq!(blobs.get(&a).unwrap(), b"hello");
    }

    #[test]
    fn different_content_gets_a_different_address() {
        let blobs = store();
        assert_ne!(blobs.put(b"one").unwrap(), blobs.put(b"two").unwrap());
    }

    #[test]
    fn an_oversized_upload_is_refused_rather_than_truncated() {
        let blobs = store();
        assert!(matches!(
            blobs.put(&vec![0u8; 2048]),
            Err(BlobError::TooLarge)
        ));
    }

    /// The digest reaches `get` from a database row, and a row is only as trustworthy as
    /// everything that can write to it.
    #[test]
    fn a_digest_that_is_not_a_digest_reads_nothing() {
        let blobs = store();
        blobs.put(b"secret").unwrap();
        for bad in [
            "../../../etc/passwd",
            "..",
            "",
            "abc",
            &"g".repeat(64),
            &"A".repeat(64),
            "aa/bb/../../../etc/passwd",
        ] {
            assert!(blobs.get(bad).is_none(), "{bad} must not resolve");
        }
    }

    /// Stored cross-site scripting is the failure this exists to prevent: these bytes come from
    /// another agent and are served from the postbox's own origin.
    #[test]
    fn nothing_renderable_survives_the_content_type_allowlist() {
        for hostile in [
            "text/html",
            "image/svg+xml",
            "application/xhtml+xml",
            "text/plain",
            "application/javascript",
            "TEXT/HTML",
        ] {
            assert_eq!(
                safe_content_type(hostile),
                "application/octet-stream",
                "{hostile}"
            );
        }
        assert_eq!(safe_content_type("image/png"), "image/png");
        assert_eq!(safe_content_type(" IMAGE/JPEG "), "image/jpeg");
    }

    #[test]
    fn a_filename_cannot_become_a_path_or_break_a_header() {
        // Separators are removed rather than escaped, so what is left cannot address a path.
        let cleaned = safe_filename("../../etc/passwd");
        assert!(!cleaned.contains('/'), "{cleaned}");
        assert!(!cleaned.starts_with('.'), "{cleaned}");
        assert_eq!(safe_filename("report.pdf"), "report.pdf");
        assert_eq!(safe_filename("a\"b\r\nc"), "abc");
        assert_eq!(safe_filename("   "), "attachment");
        assert_eq!(safe_filename(""), "attachment");
        assert!(safe_filename(&"x".repeat(500)).len() <= 120);
    }
}
