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
use std::sync::atomic::{AtomicU64, Ordering};

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
    /// Ceiling on everything stored here together.
    ///
    /// Per-mailbox quotas bound one holder; nothing bounds the sum of them, and blobs are the one
    /// thing on this box that grows without a limit written down in the code. The volume is shared
    /// with other people's data, so the store has to stop before the filesystem does.
    total_bytes: u64,
    /// What is left for everyone else. Below this, uploads are refused however much of the store's
    /// own ceiling is unspent — a volume filled to the last block takes down more than attachments.
    min_free_bytes: u64,
    /// Uploads this process refused or lost. Exported at `/metrics`, because a store that has
    /// quietly stopped accepting files looks exactly like a store nobody is sending files to.
    uploads_failed: AtomicU64,
}

#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    #[error("attachment is larger than this postbox accepts")]
    TooLarge,
    #[error("attachment storage is full")]
    StoreFull,
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
        let max_bytes = env_mb("PIGEONPOST_BLOB_MAX_MB", 100) as usize;
        // 12 GiB of a 40 GB volume that already holds somebody else's data, and 2 GiB kept free
        // under it. Both are deployment facts, so both are settable; the defaults are the live
        // box's, and they are deliberately far below the volume rather than near it.
        let total_bytes = env_mb("PIGEONPOST_BLOB_TOTAL_MB", 12 * 1024);
        let min_free_bytes = env_mb("PIGEONPOST_BLOB_MIN_FREE_MB", 2 * 1024);
        tracing::info!(
            path = %root.display(),
            max_mb = max_bytes / 1024 / 1024,
            total_mb = total_bytes / 1024 / 1024,
            min_free_mb = min_free_bytes / 1024 / 1024,
            "attachments configured"
        );
        Some(Self {
            root,
            max_bytes,
            total_bytes,
            min_free_bytes,
            uploads_failed: AtomicU64::new(0),
        })
    }

    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    pub fn min_free_bytes(&self) -> u64 {
        self.min_free_bytes
    }

    /// Free space on the volume the blobs live on, when the platform can say.
    ///
    /// `None` is not "full": a `statvfs` that fails should not stop a postbox accepting files, so
    /// the caller treats an unknown as unconstrained and leans on the store ceiling instead.
    #[cfg(unix)]
    pub fn free_bytes(&self) -> Option<u64> {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(self.root.as_os_str().as_bytes()).ok()?;
        // SAFETY: `path` is a NUL-terminated C string that outlives the call, and `stat` is
        // written only by `statvfs` and only on success.
        unsafe {
            let mut stat: libc::statvfs = std::mem::zeroed();
            if libc::statvfs(path.as_ptr(), &mut stat) != 0 {
                return None;
            }
            // `f_frsize` is the fragment size blocks are counted in; `f_bsize` is a hint about
            // efficient I/O and is the wrong multiplier here.
            // `From` rather than `as`: these fields are 64-bit on the platforms this runs on and
            // 32-bit on others, and a widening conversion is right on both. It is a no-op here,
            // which is what the lint is objecting to and why it is allowed rather than obeyed —
            // `as` would silently truncate if this ever built somewhere they are wider.
            #[allow(clippy::useless_conversion)]
            Some(u64::from(stat.f_bavail).saturating_mul(u64::from(stat.f_frsize)))
        }
    }

    #[cfg(not(unix))]
    pub fn free_bytes(&self) -> Option<u64> {
        None
    }

    /// Whether `incoming` more bytes can be taken, given what the store already holds.
    ///
    /// Two separate limits, and the reasons differ. The ceiling is a promise about how much of a
    /// shared volume attachments may claim. The free-space floor is about the volume itself: a
    /// full disk is somebody else's outage, and the eleven other vhosts on this box did not agree
    /// to it.
    pub fn headroom(&self, stored: u64, incoming: u64) -> Result<(), BlobError> {
        if stored.saturating_add(incoming) > self.total_bytes {
            return Err(BlobError::StoreFull);
        }
        if let Some(free) = self.free_bytes() {
            if free.saturating_sub(incoming) < self.min_free_bytes {
                return Err(BlobError::StoreFull);
            }
        }
        Ok(())
    }

    /// Count an upload that did not end with bytes on disk, whatever refused it.
    pub fn note_failed_upload(&self) {
        self.uploads_failed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn uploads_failed(&self) -> u64 {
        self.uploads_failed.load(Ordering::Relaxed)
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

/// A megabyte figure from the environment, in bytes. An unparseable value is the default rather
/// than a startup failure — attachments are an optional feature and a typo should not take the
/// postbox down with it.
fn env_mb(key: &str, default_mb: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default_mb)
        .saturating_mul(1024 * 1024)
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
            total_bytes: 4096,
            min_free_bytes: 0,
            uploads_failed: AtomicU64::new(0),
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

    /// The per-mailbox quota bounds one holder. Nothing else bounds the sum, which is what the
    /// store ceiling is for.
    #[test]
    fn the_store_stops_at_its_ceiling_however_many_mailboxes_are_filling_it() {
        let blobs = store();
        assert!(blobs.headroom(0, 1024).is_ok());
        assert!(blobs.headroom(3_000, 1_000).is_ok());
        assert!(matches!(
            blobs.headroom(4_000, 1_000),
            Err(BlobError::StoreFull)
        ));
        // Already over, because the ceiling can be lowered under a store that is already full.
        assert!(matches!(
            blobs.headroom(9_000, 1),
            Err(BlobError::StoreFull)
        ));
    }

    /// A volume with room in the store's own budget but none left on disk is still full. The other
    /// tenants of that filesystem did not agree to the outage.
    #[test]
    fn a_nearly_full_volume_refuses_even_when_the_ceiling_has_room() {
        let mut blobs = store();
        blobs.min_free_bytes = u64::MAX;
        match blobs.free_bytes() {
            // The floor is above anything a real volume has free, so nothing fits.
            Some(_) => assert!(matches!(blobs.headroom(0, 1), Err(BlobError::StoreFull))),
            // Where free space cannot be read, an unknown must not become a refusal.
            None => assert!(blobs.headroom(0, 1).is_ok()),
        }
    }

    #[test]
    fn failed_uploads_are_counted_so_a_store_that_stopped_taking_files_can_be_seen() {
        let blobs = store();
        assert_eq!(blobs.uploads_failed(), 0);
        blobs.note_failed_upload();
        blobs.note_failed_upload();
        assert_eq!(blobs.uploads_failed(), 2);
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
