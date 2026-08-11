//! Exclusive lifetime lease for one online trace directory.
//!
//! Segment recovery mutates an open file and therefore must never race another process. The lock
//! artifact is deliberately persistent: unlinking it on drop would let a renamed inode and a new
//! inode carry independent advisory locks. Offline bundles copy only a terminal manifest and its
//! declared segments; their exact-layout verifier rejects this runtime artifact if it is copied.

use std::path::Path;

#[cfg(unix)]
use fs2::FileExt;
#[cfg(unix)]
use pigeonpost_unix_custody::{
    CustodyError, FilePolicy, GuardedDir, GuardedFile, LeafName, OpenAccess,
};

use crate::error::{Result, SealError};
use crate::segment::{require_persistent_writer_platform, PersistentWriterPlatform};

/// Fixed, purpose-directory-local coordination artifact.
pub const TRACE_WRITER_LEASE_NAME: &str = ".pigeonpost-trace-writer-v1.lock";

/// Opaque exclusive lease retained by an online trace sink for its complete lifetime.
///
/// Acquisition is nonblocking. A live writer, unsafe artifact, or unsupported filesystem fails
/// closed before segment recovery or live-key mutation can begin.
pub struct TraceWriterLease {
    #[cfg(unix)]
    file: GuardedFile,
}

impl core::fmt::Debug for TraceWriterLease {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TraceWriterLease")
            .field("path", &"<withheld>")
            .finish_non_exhaustive()
    }
}

impl TraceWriterLease {
    /// Secure the directory and acquire its one exclusive writer lease without waiting.
    pub fn acquire(directory: impl AsRef<Path>) -> Result<Self> {
        Self::acquire_for_platform(PersistentWriterPlatform::current(), directory)
    }

    fn acquire_for_platform(
        platform: PersistentWriterPlatform,
        directory: impl AsRef<Path>,
    ) -> Result<Self> {
        require_persistent_writer_platform(platform)?;
        let directory = directory.as_ref();

        #[cfg(unix)]
        {
            let directory = GuardedDir::create_private(directory).map_err(map_lease_error)?;
            let name = LeafName::new(TRACE_WRITER_LEASE_NAME).map_err(map_lease_error)?;
            let file = directory
                .open_or_create_file(&name, OpenAccess::ReadWrite, FilePolicy::private_exact(0))
                .map_err(map_lease_error)?;
            FileExt::try_lock_exclusive(file.file())
                .map_err(|_| SealError::WriterLeaseUnavailable)?;
            if let Err(error) = validate_lease_file(&file) {
                let _ = FileExt::unlock(file.file());
                return Err(error);
            }
            Ok(Self { file })
        }

        // Production trace capture is already refused on Windows. Test capture also fails closed
        // here until the lease can prove an owner-only DACL, no reparse point, one link, stable file
        // identity, and delete-sharing denial together. Other platforms have the same posture.
        #[cfg(not(unix))]
        {
            let _ = directory;
            Err(SealError::WriterLeaseUnavailable)
        }
    }

    /// Revalidate that the retained descriptor is still the one named coordination artifact.
    pub fn assert_stable(&self) -> Result<()> {
        #[cfg(unix)]
        {
            validate_lease_file(&self.file)
        }

        #[cfg(not(unix))]
        {
            Err(SealError::WriterLeaseUnavailable)
        }
    }
}

#[cfg(unix)]
impl Drop for TraceWriterLease {
    fn drop(&mut self) {
        // Closing the descriptor also releases the advisory lock. Explicit unlock makes the
        // lifetime boundary clear; the named artifact is intentionally never unlinked or replaced.
        let _ = FileExt::unlock(self.file.file());
    }
}

#[cfg(unix)]
fn validate_lease_file(file: &GuardedFile) -> Result<()> {
    file.verify_named().map_err(map_lease_error)?;
    let metadata = file.metadata().map_err(map_lease_error)?;
    if metadata.len != 0 {
        return Err(SealError::UnsafeWriterLease);
    }
    Ok(())
}

#[cfg(unix)]
fn map_lease_error(error: CustodyError) -> SealError {
    match error {
        CustodyError::Io(error) => SealError::Io(error),
        CustodyError::NotFound
        | CustodyError::AlreadyExists
        | CustodyError::LimitExceeded(_)
        | CustodyError::InvalidPath(_)
        | CustodyError::UnsafeAncestor(_)
        | CustodyError::UnsafeDirectory(_)
        | CustodyError::UnsafeFile(_)
        | CustodyError::Unsupported(_) => SealError::UnsafeWriterLease,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::os::unix::fs::{symlink, OpenOptionsExt, PermissionsExt};

    use tempfile::tempdir;

    use super::*;

    struct PanicPath;

    impl AsRef<Path> for PanicPath {
        fn as_ref(&self) -> &Path {
            panic!("unsupported writer lease must not inspect its directory")
        }
    }

    #[test]
    fn unsupported_platform_rejects_lease_before_path_access() {
        assert!(matches!(
            TraceWriterLease::acquire_for_platform(
                PersistentWriterPlatform::unsupported_for_test(),
                PanicPath,
            ),
            Err(SealError::Io(error)) if error.kind() == std::io::ErrorKind::Unsupported
        ));
    }

    #[test]
    fn lease_is_nonblocking_directory_scoped_and_reopens_after_drop() {
        let root = tempdir().unwrap();
        let first_directory = root.path().join("first");
        let second_directory = root.path().join("second");
        let first = TraceWriterLease::acquire(&first_directory).unwrap();

        assert!(TraceWriterLease::acquire(&first_directory).is_err());

        let disjoint = TraceWriterLease::acquire(&second_directory).unwrap();
        disjoint.assert_stable().unwrap();
        drop(disjoint);

        let lease_path = first_directory.join(TRACE_WRITER_LEASE_NAME);
        drop(first);
        assert!(
            lease_path.is_file(),
            "drop must retain the stable lock inode"
        );
        TraceWriterLease::acquire(first_directory).unwrap();
    }

    #[test]
    fn unsafe_artifacts_and_live_replacement_fail_closed() {
        let root = tempdir().unwrap();
        let directory = root.path().join("trace");
        crate::segment::secure_artifact_parent(&directory).unwrap();
        let lease_path = directory.join(TRACE_WRITER_LEASE_NAME);
        let target = root.path().join("target");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&target)
            .unwrap();

        symlink(&target, &lease_path).unwrap();
        assert!(TraceWriterLease::acquire(&directory).is_err());
        fs::remove_file(&lease_path).unwrap();

        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&lease_path)
            .unwrap();
        let alias = directory.join("lease-alias");
        fs::hard_link(&lease_path, &alias).unwrap();
        assert!(TraceWriterLease::acquire(&directory).is_err());
        fs::remove_file(&alias).unwrap();

        fs::set_permissions(&lease_path, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(TraceWriterLease::acquire(&directory).is_err());
        assert_eq!(
            fs::metadata(&lease_path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::set_permissions(&lease_path, fs::Permissions::from_mode(0o600)).unwrap();

        let lease = TraceWriterLease::acquire(&directory).unwrap();
        let displaced = directory.join("displaced-lock");
        fs::rename(&lease_path, &displaced).unwrap();
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&lease_path)
            .unwrap();
        assert!(lease.assert_stable().is_err());
        drop(lease);
        assert!(displaced.exists(), "drop must not remove a displaced inode");
        assert!(
            lease_path.exists(),
            "drop must not remove a replacement inode"
        );
    }

    #[test]
    fn live_directory_replacement_invalidates_the_retained_lease() {
        let root = tempdir().unwrap();
        let directory = root.path().join("trace");
        let lease = TraceWriterLease::acquire(&directory).unwrap();

        let displaced = root.path().join("displaced-trace");
        fs::rename(&directory, &displaced).unwrap();
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let replacement_lock = directory.join(TRACE_WRITER_LEASE_NAME);
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&replacement_lock)
            .unwrap();

        assert!(matches!(
            lease.assert_stable(),
            Err(SealError::UnsafeWriterLease)
        ));
        drop(lease);
        assert!(replacement_lock.exists());
        assert!(displaced.join(TRACE_WRITER_LEASE_NAME).exists());
    }
}
