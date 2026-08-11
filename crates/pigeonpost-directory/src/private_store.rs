//! Descriptor-level custody for long-lived server secrets and secret-bearing SQLite files.
//!
//! The path is only a lookup hint. Every accepted object is opened without following links,
//! validated through the opened descriptor/handle, compared with a second lookup of its name, and
//! kept beside a stable descriptor for its private parent directory. This is deliberately shared by
//! the directory database and the CLI's loft, registry, directory, and prober seed files.

use std::ffi::OsString;
#[cfg(any(test, not(any(unix, windows))))]
use std::fs;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(any(unix, windows))]
use std::sync::Mutex;

#[cfg(unix)]
use pigeonpost_unix_custody::{
    CustodyError, DirPolicy, FilePolicy, GuardedDir, GuardedFile, LeafName, NormalizedPath,
    OpenAccess,
};
use zeroize::Zeroizing;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
#[cfg(unix)]
const MAX_PRIVATE_FILE_BYTES: u64 = 64 * 1024;
#[cfg(any(unix, windows))]
const MAX_SQLITE_FILE_BYTES: u64 = 1 << 40;

/// An open private file plus the descriptor/handle that protects its parent lookup.
#[cfg(unix)]
#[derive(Debug)]
pub struct PrivateFile {
    file: GuardedFile,
    path: PathBuf,
}

/// An open private file plus the descriptor/handle that protects its parent lookup.
#[cfg(not(unix))]
#[derive(Debug)]
pub struct PrivateFile {
    file: File,
    path: PathBuf,
    parent: PrivateDirectory,
}

/// SQLite-specific custody retains the guarded main file and both persistent WAL-mode sidecars.
#[cfg(unix)]
#[derive(Debug)]
pub struct PrivateDatabase {
    directory: GuardedDir,
    main: GuardedFile,
    main_name: LeafName,
    path: PathBuf,
    sidecar_names: [LeafName; 2],
    journal_name: LeafName,
    retained_sidecars: Mutex<[Option<GuardedFile>; 2]>,
}

#[cfg(windows)]
#[derive(Debug)]
pub struct PrivateDatabase {
    main: PrivateFile,
    path: PathBuf,
    sidecar_paths: [PathBuf; 2],
    journal_path: PathBuf,
    retained_sidecars: Mutex<[Option<PrivateFile>; 2]>,
    journal_preexisted: bool,
}

#[cfg(not(any(unix, windows)))]
pub type PrivateDatabase = PrivateFile;

#[cfg(unix)]
#[derive(Debug)]
pub struct PrivateDirectory {
    guarded: GuardedDir,
}

#[cfg(not(unix))]
#[derive(Debug)]
pub struct PrivateDirectory {
    file: File,
    path: PathBuf,
    #[cfg(windows)]
    ancestors: platform::ParentGuard,
}

#[cfg(unix)]
impl PrivateFile {
    /// Open an existing private regular file without following any path component.
    pub fn open_existing(path: &Path) -> io::Result<Self> {
        let (parent, name) = PrivateDirectory::open_for_file(path, false)?;
        let file = parent
            .guarded
            .open_file(
                &name,
                OpenAccess::ReadOnly,
                FilePolicy::private(MAX_PRIVATE_FILE_BYTES),
            )
            .map_err(custody_io_error)?;
        let path = parent.guarded.absolute_path().join(name.as_os_str());
        let private = Self { file, path };
        private.verify_named()?;
        Ok(private)
    }

    /// Open or create an empty private regular file without blessing an unsafe existing object.
    pub fn open_or_create(path: &Path) -> io::Result<(Self, bool)> {
        let (parent, name) = PrivateDirectory::open_for_file(path, true)?;
        let file = parent
            .guarded
            .open_or_create_file(
                &name,
                OpenAccess::ReadWrite,
                FilePolicy::private(MAX_PRIVATE_FILE_BYTES),
            )
            .map_err(custody_io_error)?;
        let created = file.was_created();
        let path = parent.guarded.absolute_path().join(name.as_os_str());
        let private = Self { file, path };
        private.verify_named()?;
        parent.sync()?;
        Ok((private, created))
    }

    /// Re-prove that the name and every ancestor still identify the held private object.
    pub fn verify_named(&self) -> io::Result<()> {
        self.file.verify_named().map_err(custody_io_error)
    }

    /// Borrow the already-validated descriptor without transferring custody or reopening its path.
    pub fn descriptor(&self) -> &File {
        self.file.file()
    }

    /// Return the normalized lookup name retained by this custody handle.
    pub fn normalized_path(&self) -> &Path {
        &self.path
    }

    /// Prove both retained names, compare their filesystem identities, then prove the names again.
    pub fn same_object(&self, other: &Self) -> io::Result<bool> {
        self.verify_named()?;
        other.verify_named()?;
        let same = self.file.identity() == other.file.identity();
        self.verify_named()?;
        other.verify_named()?;
        Ok(same)
    }
}

#[cfg(not(unix))]
impl PrivateFile {
    /// Open an existing private regular file without following a link.
    pub fn open_existing(path: &Path) -> io::Result<Self> {
        let path = normalized_private_path(path)?;
        let parent = PrivateDirectory::secure_for(&path)?;
        let file = platform::open_existing_file(&path)?;
        let private = Self { file, path, parent };
        private.verify_named()?;
        Ok(private)
    }

    /// Open or create an empty private regular file without blessing an unsafe existing object.
    pub fn open_or_create(path: &Path) -> io::Result<(Self, bool)> {
        let path = normalized_private_path(path)?;
        let parent = PrivateDirectory::secure_for(&path)?;
        let (file, created) = platform::open_or_create_file(&path)?;
        let private = Self { file, path, parent };
        private.verify_named()?;
        private.parent.sync()?;
        Ok((private, created))
    }

    fn create_new(path: &Path) -> io::Result<Self> {
        let path = normalized_private_path(path)?;
        let parent = PrivateDirectory::secure_for(&path)?;
        let file = platform::create_new_file(&path)?;
        let private = Self { file, path, parent };
        private.verify_named()?;
        Ok(private)
    }

    /// Retain and protect a file that SQLite created inside an already-guarded private parent.
    #[cfg(windows)]
    fn retain_and_protect_subsystem_file(path: &Path) -> io::Result<Self> {
        let path = normalized_private_path(path)?;
        let parent = PrivateDirectory::secure_for(&path)?;
        let file = platform::open_and_protect_subsystem_file(&path)?;
        let private = Self { file, path, parent };
        private.verify_named()?;
        Ok(private)
    }

    /// Re-prove that the name and parent still identify the held private objects.
    pub fn verify_named(&self) -> io::Result<()> {
        self.parent.verify_named()?;
        platform::verify_file_named(&self.file, &self.path)?;
        self.parent.verify_named()
    }

    /// Borrow the already-validated descriptor without transferring custody or reopening its path.
    pub fn descriptor(&self) -> &File {
        &self.file
    }

    /// Return the normalized lookup name retained by this custody handle.
    pub fn normalized_path(&self) -> &Path {
        &self.path
    }

    /// Prove both retained names, compare full platform identities, then prove the names again.
    pub fn same_object(&self, other: &Self) -> io::Result<bool> {
        self.verify_named()?;
        other.verify_named()?;
        #[cfg(windows)]
        {
            let same = pigeonpost_windows_custody::file_identity(&self.file)?
                == pigeonpost_windows_custody::file_identity(&other.file)?;
            self.verify_named()?;
            other.verify_named()?;
            Ok(same)
        }
        #[cfg(not(windows))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "private file identity comparison is unsupported on this platform",
            ))
        }
    }

    #[cfg(not(unix))]
    pub fn sqlite_path(&self) -> &Path {
        &self.path
    }
}

#[cfg(unix)]
impl PrivateDatabase {
    pub fn open_existing(path: &Path) -> io::Result<Self> {
        Self::open(path, false).map(|(database, _)| database)
    }

    pub fn open_or_create(path: &Path) -> io::Result<(Self, bool)> {
        Self::open(path, true)
    }

    fn open(requested: &Path, create: bool) -> io::Result<(Self, bool)> {
        let normalized = NormalizedPath::new(requested).map_err(custody_io_error)?;
        let main_name = normalized
            .as_path()
            .file_name()
            .ok_or_else(|| invalid(requested, "must name a database file"))?;
        let main_name = LeafName::new(main_name).map_err(custody_io_error)?;
        let parent = normalized
            .as_path()
            .parent()
            .ok_or_else(|| invalid(requested, "has no parent directory"))?;
        let directory = match GuardedDir::open_existing(parent, DirPolicy::trusted()) {
            Ok(directory) => directory,
            Err(CustodyError::NotFound) if create => {
                GuardedDir::create_private(parent).map_err(custody_io_error)?
            }
            Err(error) => return Err(custody_io_error(error)),
        };
        let path = directory.absolute_path().join(main_name.as_os_str());
        let sidecar_names = [
            database_suffixed_leaf(&main_name, "-wal")?,
            database_suffixed_leaf(&main_name, "-shm")?,
        ];
        let journal_name = database_suffixed_leaf(&main_name, "-journal")?;

        for sidecar in &sidecar_names {
            directory
                .validate_file(sidecar, database_file_policy())
                .map_err(custody_io_error)?;
        }
        directory
            .validate_file(&journal_name, database_file_policy())
            .map_err(custody_io_error)?;
        let main = if create {
            directory.open_or_create_file(&main_name, OpenAccess::ReadWrite, database_file_policy())
        } else {
            directory.open_file(&main_name, OpenAccess::ReadWrite, database_file_policy())
        }
        .map_err(custody_io_error)?;
        let created = main.was_created();
        let database = Self {
            directory,
            main,
            main_name,
            path,
            sidecar_names,
            journal_name,
            retained_sidecars: Mutex::new(std::array::from_fn(|_| None)),
        };
        database.verify_main_named()?;
        database.verify_sidecars(false)?;
        Ok((database, created))
    }

    pub fn sqlite_path(&self) -> &Path {
        &self.path
    }

    pub fn verify_main_named(&self) -> io::Result<()> {
        self.directory.verify_named().map_err(custody_io_error)?;
        self.main.verify_named().map_err(custody_io_error)?;
        let named = self
            .directory
            .validate_file(&self.main_name, database_file_policy())
            .map_err(custody_io_error)?
            .ok_or_else(|| custody_io_error(CustodyError::NotFound))?;
        if named.identity != self.main.identity() {
            return Err(custody_io_error(CustodyError::UnsafeFile(
                "database name no longer identifies retained main file",
            )));
        }
        Ok(())
    }

    pub fn verify_named(&self) -> io::Result<()> {
        self.verify_main_named()?;
        self.verify_sidecars(true)
    }

    fn verify_sidecars(&self, require_wal_and_shm: bool) -> io::Result<()> {
        let mut retained = self
            .retained_sidecars
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for (name, retained_file) in self.sidecar_names.iter().zip(retained.iter_mut()) {
            if let Some(file) = retained_file {
                file.verify_named().map_err(custody_io_error)?;
                continue;
            }
            match self
                .directory
                .open_file_optional(name, OpenAccess::ReadOnly, database_file_policy())
                .map_err(custody_io_error)?
            {
                Some(file) => *retained_file = Some(file),
                None if require_wal_and_shm => {
                    return Err(custody_io_error(CustodyError::UnsafeFile(
                        "required SQLite WAL or SHM sidecar is missing",
                    )));
                }
                None => {}
            }
        }
        self.directory
            .validate_file(&self.journal_name, database_file_policy())
            .map_err(custody_io_error)?;
        Ok(())
    }
}

#[cfg(windows)]
impl PrivateDatabase {
    pub fn open_existing(path: &Path) -> io::Result<Self> {
        Self::open(path, false).map(|(database, _)| database)
    }

    pub fn open_or_create(path: &Path) -> io::Result<(Self, bool)> {
        Self::open(path, true)
    }

    fn open(requested: &Path, create: bool) -> io::Result<(Self, bool)> {
        let path = normalized_private_path(requested)?;
        let parent = path
            .parent()
            .ok_or_else(|| invalid(requested, "has no parent directory"))?;
        if path.file_name().is_none() {
            return Err(invalid(requested, "must name a database file"));
        }

        // Existing-only callers prove the main file before any directory creation. Creating
        // callers establish the private parent first, then reject every hostile sidecar before
        // publishing the main file or allowing SQLite to inspect the namespace.
        let existing_main = if create {
            let directory = PrivateDirectory::open_or_create(parent)?;
            directory.verify_named()?;
            None
        } else {
            Some(PrivateFile::open_existing(&path)?)
        };
        let sidecar_paths = [
            windows_database_sidecar_path(&path, "-wal")?,
            windows_database_sidecar_path(&path, "-shm")?,
        ];
        let journal_path = windows_database_sidecar_path(&path, "-journal")?;
        let preexisting_wal = windows_private_file_optional(&sidecar_paths[0])?;
        if let Some(file) = preexisting_wal.as_ref() {
            validate_windows_database_file(file)?;
        }
        let preexisting_shm = windows_private_file_optional(&sidecar_paths[1])?;
        if let Some(file) = preexisting_shm.as_ref() {
            validate_windows_database_file(file)?;
        }
        let journal = windows_private_file_optional(&journal_path)?;
        if let Some(file) = journal.as_ref() {
            validate_windows_database_file(file)?;
        }
        let journal_preexisted = journal.is_some();
        drop(journal);

        let (main, created) = match existing_main {
            Some(main) => (main, false),
            None => PrivateFile::open_or_create(&path)?,
        };
        validate_windows_database_file(&main)?;
        let database = Self {
            main,
            path,
            sidecar_paths,
            journal_path,
            retained_sidecars: Mutex::new([preexisting_wal, preexisting_shm]),
            journal_preexisted,
        };
        database.verify_main_named()?;
        database.verify_sidecars(false)?;
        Ok((database, created))
    }

    pub fn sqlite_path(&self) -> &Path {
        &self.path
    }

    pub fn verify_main_named(&self) -> io::Result<()> {
        validate_windows_database_file(&self.main)
    }

    pub fn verify_named(&self) -> io::Result<()> {
        self.verify_main_named()?;
        self.verify_sidecars(true)
    }

    fn verify_sidecars(&self, require_wal_and_shm: bool) -> io::Result<()> {
        let mut retained = self
            .retained_sidecars
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for (path, retained_file) in self.sidecar_paths.iter().zip(retained.iter_mut()) {
            if let Some(file) = retained_file {
                validate_windows_database_file(file)?;
                continue;
            }
            if require_wal_and_shm {
                let file =
                    PrivateFile::retain_and_protect_subsystem_file(path).map_err(|error| {
                        if error.kind() == io::ErrorKind::NotFound {
                            invalid(path, "required SQLite WAL or SHM sidecar is missing")
                        } else {
                            error
                        }
                    })?;
                validate_windows_database_file(&file)?;
                *retained_file = Some(file);
            }
        }

        if self.journal_preexisted {
            if let Some(journal) = windows_private_file_optional(&self.journal_path)? {
                validate_windows_database_file(&journal)?;
            }
        } else if require_wal_and_shm {
            match PrivateFile::retain_and_protect_subsystem_file(&self.journal_path) {
                Ok(journal) => validate_windows_database_file(&journal)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
fn windows_private_file_optional(path: &Path) -> io::Result<Option<PrivateFile>> {
    match PrivateFile::open_existing(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn validate_windows_database_file(file: &PrivateFile) -> io::Result<()> {
    file.verify_named()?;
    if file.descriptor().metadata()?.len() > MAX_SQLITE_FILE_BYTES {
        return Err(invalid(
            file.normalized_path(),
            "exceeds the supported SQLite size bound",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn windows_database_sidecar_path(path: &Path, suffix: &str) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| invalid(path, "must name a database file"))?;
    let mut sidecar_name = name.to_os_string();
    sidecar_name.push(suffix);
    pigeonpost_windows_custody::validate_component(&sidecar_name)?;
    let parent = path
        .parent()
        .ok_or_else(|| invalid(path, "has no parent directory"))?;
    let sidecar = parent.join(sidecar_name);
    if normalized_private_path(&sidecar)? != sidecar {
        return Err(invalid(&sidecar, "sidecar path is not exactly normalized"));
    }
    Ok(sidecar)
}

#[cfg(unix)]
fn database_file_policy() -> FilePolicy {
    FilePolicy::private(MAX_SQLITE_FILE_BYTES)
}

#[cfg(unix)]
fn database_suffixed_leaf(name: &LeafName, suffix: &str) -> io::Result<LeafName> {
    let mut value = name.as_os_str().to_os_string();
    value.push(suffix);
    LeafName::new(value).map_err(custody_io_error)
}

#[cfg(unix)]
fn custody_io_error(error: CustodyError) -> io::Error {
    match error {
        error @ CustodyError::NotFound => io::Error::new(io::ErrorKind::NotFound, error),
        error @ CustodyError::AlreadyExists => io::Error::new(io::ErrorKind::AlreadyExists, error),
        CustodyError::Io(error) if custody_io_is_policy_failure(&error) => {
            io::Error::new(io::ErrorKind::PermissionDenied, error)
        }
        CustodyError::Io(error) => error,
        error => io::Error::new(io::ErrorKind::PermissionDenied, error),
    }
}

#[cfg(unix)]
fn custody_io_is_policy_failure(error: &io::Error) -> bool {
    error.raw_os_error().is_some_and(|raw| {
        [
            rustix::io::Errno::LOOP,
            rustix::io::Errno::ISDIR,
            rustix::io::Errno::NOTDIR,
        ]
        .into_iter()
        .any(|candidate| candidate.raw_os_error() == raw)
    })
}

#[cfg(unix)]
impl PrivateDirectory {
    /// Open or create one exact private directory through a root-to-leaf descriptor walk.
    pub fn open_or_create(path: &Path) -> io::Result<Self> {
        let normalized = NormalizedPath::new(path).map_err(custody_io_error)?;
        let guarded =
            match GuardedDir::open_existing(normalized.as_path(), DirPolicy::private_mutable()) {
                Ok(guarded) => guarded,
                Err(CustodyError::NotFound) => {
                    GuardedDir::create_private(normalized.as_path()).map_err(custody_io_error)?
                }
                Err(error) => return Err(custody_io_error(error)),
            };
        let directory = Self { guarded };
        directory.verify_named()?;
        Ok(directory)
    }

    fn open_for_file(path: &Path, create: bool) -> io::Result<(Self, LeafName)> {
        let normalized = NormalizedPath::new(path).map_err(custody_io_error)?;
        let name = normalized
            .as_path()
            .file_name()
            .ok_or_else(|| invalid(path, "has no filename"))?;
        let name = LeafName::new(name).map_err(custody_io_error)?;
        let parent = normalized
            .as_path()
            .parent()
            .ok_or_else(|| invalid(path, "has no parent directory"))?;
        let directory = if create {
            Self::open_or_create(parent)?
        } else {
            let guarded = GuardedDir::open_existing(parent, DirPolicy::private())
                .map_err(custody_io_error)?;
            Self { guarded }
        };
        directory.verify_named()?;
        Ok((directory, name))
    }

    pub fn verify_named(&self) -> io::Result<()> {
        self.guarded.verify_named().map_err(custody_io_error)
    }

    /// Return the normalized lookup name retained by this directory custody handle.
    pub fn normalized_path(&self) -> &Path {
        self.guarded.absolute_path()
    }

    pub fn sync(&self) -> io::Result<()> {
        self.guarded.sync().map_err(custody_io_error)
    }
}

#[cfg(not(unix))]
impl PrivateDirectory {
    /// Open or create one exact private directory through the platform custody implementation.
    pub fn open_or_create(path: &Path) -> io::Result<Self> {
        let path = normalized_private_path(path)?;
        #[cfg(windows)]
        let (file, path, ancestors) = platform::secure_directory(&path)?;
        #[cfg(not(windows))]
        let file = platform::secure_directory(&path, false)?;
        let directory = Self {
            file,
            path,
            #[cfg(windows)]
            ancestors,
        };
        directory.verify_named()?;
        Ok(directory)
    }

    fn secure_for(path: &Path) -> io::Result<Self> {
        validate_lexical_path(path)?;
        let path = parent_path(path)?;
        Self::open_or_create(&path)
    }

    pub fn verify_named(&self) -> io::Result<()> {
        #[cfg(windows)]
        self.ancestors.verify()?;
        platform::verify_directory_named(&self.file, &self.path)?;
        #[cfg(windows)]
        self.ancestors.verify()?;
        Ok(())
    }

    /// Return the normalized lookup name retained by this directory custody handle.
    pub fn normalized_path(&self) -> &Path {
        &self.path
    }

    pub fn sync(&self) -> io::Result<()> {
        platform::sync_directory(&self.file)
    }
}

/// Atomically replace one bounded private file through a retained private parent.
///
/// The temporary file is private from creation, synced before publication, and removed through
/// the retained parent on pre-publication failure. Existing destinations are accepted only after
/// passing the same private-file policy as the replacement.
pub fn write_private_file_atomic(path: &Path, bytes: &[u8], max_bytes: u64) -> io::Result<()> {
    let byte_len =
        u64::try_from(bytes.len()).map_err(|_| invalid(path, "is too large for this platform"))?;
    if byte_len > max_bytes {
        return Err(invalid(path, "exceeds its fixed write bound"));
    }

    #[cfg(unix)]
    {
        let (parent, destination_name) = PrivateDirectory::open_for_file(path, true)?;
        let policy = FilePolicy::private(max_bytes);
        let existing = parent
            .guarded
            .open_file_optional(&destination_name, OpenAccess::ReadOnly, policy)
            .map_err(custody_io_error)?;

        let (temp_name, mut temporary_file) = (0..128)
            .find_map(|_| {
                let mut name = OsString::from(".");
                name.push(destination_name.as_os_str());
                name.push(format!(
                    ".{}.{}.tmp",
                    std::process::id(),
                    TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
                ));
                let name = match LeafName::new(name) {
                    Ok(name) => name,
                    Err(error) => return Some(Err(custody_io_error(error))),
                };
                match parent.guarded.create_file(&name, policy) {
                    Ok(file) => Some(Ok((name, file))),
                    Err(CustodyError::AlreadyExists) => None,
                    Err(error) => Some(Err(custody_io_error(error))),
                }
            })
            .transpose()?
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "could not allocate a private atomic-write temporary file",
                )
            })?;
        let cleanup = parent
            .guarded
            .open_file(&temp_name, OpenAccess::ReadOnly, policy)
            .map_err(custody_io_error)?;

        let publication = (|| -> io::Result<()> {
            temporary_file.write_all(bytes)?;
            temporary_file.sync_all().map_err(custody_io_error)?;
            if temporary_file.metadata().map_err(custody_io_error)?.len != byte_len {
                return Err(invalid(path, "was not written completely"));
            }
            temporary_file.verify_named().map_err(custody_io_error)?;
            if let Some(existing) = existing.as_ref() {
                existing.verify_named().map_err(custody_io_error)?;
            }
            let published = match existing {
                Some(_) => parent.guarded.rename_replace(
                    temporary_file,
                    &parent.guarded,
                    &destination_name,
                ),
                None => parent.guarded.publish_no_replace(
                    temporary_file,
                    &parent.guarded,
                    &destination_name,
                ),
            }
            .map_err(custody_io_error)?;
            published.verify_named().map_err(custody_io_error)?;
            if published.metadata().map_err(custody_io_error)?.len != byte_len {
                return Err(invalid(path, "changed length during publication"));
            }
            let reopened = parent
                .guarded
                .open_file(
                    &destination_name,
                    OpenAccess::ReadOnly,
                    FilePolicy::private_exact(byte_len),
                )
                .map_err(custody_io_error)?;
            if reopened.identity() != published.identity() {
                return Err(invalid(path, "changed identity during publication"));
            }
            reopened.verify_named().map_err(custody_io_error)?;
            parent.verify_named()?;
            parent.sync()
        })();

        if let Err(error) = publication {
            if parent
                .guarded
                .entry_metadata(&temp_name)
                .map_err(custody_io_error)?
                .is_some()
            {
                parent
                    .guarded
                    .unlink_file(cleanup)
                    .map_err(custody_io_error)?;
            }
            parent.verify_named()?;
            return Err(error);
        }
        Ok(())
    }

    #[cfg(not(unix))]
    {
        write_private_file_atomic_nonunix(path, bytes, byte_len, max_bytes)
    }
}

#[cfg(not(unix))]
fn write_private_file_atomic_nonunix(
    path: &Path,
    bytes: &[u8],
    byte_len: u64,
    max_bytes: u64,
) -> io::Result<()> {
    let path = normalized_private_path(path)?;
    let parent_path = parent_path(&path)?;
    let parent = PrivateDirectory::secure_for(&path)?;
    let destination_exists = match PrivateFile::open_existing(&path) {
        Ok(existing) => {
            if existing.descriptor().metadata()?.len() > max_bytes {
                return Err(invalid(
                    &path,
                    "existing value exceeds its fixed write bound",
                ));
            }
            existing.verify_named()?;
            true
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error),
    };

    let (temporary, mut temporary_file) = (0..128)
        .find_map(|_| {
            let mut name = OsString::from(".");
            name.push(path.file_name()?);
            name.push(format!(
                ".{}.{}.tmp",
                std::process::id(),
                TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            let temporary = parent_path.join(name);
            match PrivateFile::create_new(&temporary) {
                Ok(file) => Some(Ok((temporary, file))),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(error)),
            }
        })
        .transpose()?
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "could not allocate a private atomic-write temporary file",
            )
        })?;
    let write_result = (|| -> io::Result<()> {
        temporary_file.file.write_all(bytes)?;
        temporary_file.file.sync_all()?;
        if temporary_file.file.metadata()?.len() != byte_len {
            return Err(invalid(&path, "was not written completely"));
        }
        temporary_file.verify_named()
    })();
    drop(temporary_file);
    if let Err(error) = write_result {
        remove_private_name(&parent, &temporary)?;
        parent.verify_named()?;
        return Err(error);
    }

    parent.verify_named()?;
    let publication = if destination_exists {
        platform::replace_write_through(&parent.file, &temporary, &path)
    } else {
        platform::publish_noclobber(&parent.file, &temporary, &path)
    };
    if let Err(error) = publication {
        remove_private_name(&parent, &temporary)?;
        parent.verify_named()?;
        return Err(error);
    }
    parent.verify_named()?;
    parent.sync()?;
    let published = PrivateFile::open_existing(&path)?;
    if published.descriptor().metadata()?.len() != byte_len {
        return Err(invalid(&path, "changed length during publication"));
    }
    published.verify_named()
}

#[cfg(not(unix))]
fn validate_lexical_path(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty() {
        return Err(invalid(path, "must not be empty"));
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(invalid(
            path,
            "must not contain a parent-directory component",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn normalized_private_path(path: &Path) -> io::Result<PathBuf> {
    platform::normalized_absolute(path)
}

#[cfg(not(any(unix, windows)))]
fn normalized_private_path(path: &Path) -> io::Result<PathBuf> {
    validate_lexical_path(path)?;
    Ok(path.to_path_buf())
}

#[cfg(not(unix))]
fn parent_path(path: &Path) -> io::Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid(path, "has no parent directory"))?;
    if parent.as_os_str().is_empty() {
        Ok(PathBuf::from("."))
    } else {
        Ok(parent.to_path_buf())
    }
}

/// Read a bounded trusted configuration file from one verified descriptor.
///
/// Unix accepts a root- or effective-user-owned file and ancestry with no group/world mutation
/// authority. Windows deliberately uses the stricter private-store boundary rather than falling
/// back to an unchecked path open. Every successful read revalidates the retained name after EOF.
pub fn read_trusted_file_bounded(path: &Path, max_bytes: u64) -> io::Result<Zeroizing<Vec<u8>>> {
    #[cfg(unix)]
    {
        read_unix_file_bounded(
            path,
            max_bytes,
            DirPolicy::trusted(),
            FilePolicy::trusted(max_bytes),
            false,
        )
    }
    #[cfg(not(unix))]
    {
        read_nonunix_private_file_bounded(path, max_bytes)
    }
}

/// Read a bounded private, single-link file from one verified descriptor.
///
/// On Unix the file must be effective-user-owned with exact mode `0400` or `0600`; its parent and
/// complete ancestry must be root- or effective-user-owned and immutable by group/other users.
pub fn read_private_file_bounded(path: &Path, max_bytes: u64) -> io::Result<Zeroizing<Vec<u8>>> {
    #[cfg(unix)]
    {
        read_unix_file_bounded(
            path,
            max_bytes,
            DirPolicy::trusted(),
            FilePolicy::private(max_bytes),
            true,
        )
    }
    #[cfg(not(unix))]
    {
        read_nonunix_private_file_bounded(path, max_bytes)
    }
}

#[cfg(unix)]
fn read_unix_file_bounded(
    path: &Path,
    max_bytes: u64,
    directory_policy: DirPolicy,
    file_policy: FilePolicy,
    exact_private_mode: bool,
) -> io::Result<Zeroizing<Vec<u8>>> {
    let read_limit = max_bytes
        .checked_add(1)
        .ok_or_else(|| invalid(path, "has an invalid read bound"))?;
    let normalized = NormalizedPath::new(path).map_err(custody_io_error)?;
    let name = normalized
        .as_path()
        .file_name()
        .ok_or_else(|| invalid(path, "has no filename"))?;
    let name = LeafName::new(name).map_err(custody_io_error)?;
    let parent = normalized
        .as_path()
        .parent()
        .ok_or_else(|| invalid(path, "has no parent directory"))?;
    let directory =
        GuardedDir::open_existing(parent, directory_policy).map_err(custody_io_error)?;
    let mut file = directory
        .open_file(&name, OpenAccess::ReadOnly, file_policy)
        .map_err(custody_io_error)?;
    let opened = file.metadata().map_err(custody_io_error)?;
    if exact_private_mode && !matches!(opened.mode & 0o7777, 0o400 | 0o600) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private file mode must be exactly 0400 or 0600",
        ));
    }
    let capacity = usize::try_from(opened.len.min(max_bytes))
        .unwrap_or(usize::MAX)
        .saturating_add(1);
    let mut bytes = Zeroizing::new(Vec::with_capacity(capacity));
    Read::by_ref(&mut file)
        .take(read_limit)
        .read_to_end(&mut bytes)?;
    file.verify_named().map_err(custody_io_error)?;
    if bytes.len() as u64 > max_bytes {
        return Err(invalid(path, "exceeds its fixed read bound"));
    }
    Ok(bytes)
}

#[cfg(not(unix))]
fn read_nonunix_private_file_bounded(
    path: &Path,
    max_bytes: u64,
) -> io::Result<Zeroizing<Vec<u8>>> {
    let read_limit = max_bytes
        .checked_add(1)
        .ok_or_else(|| invalid(path, "has an invalid read bound"))?;
    let mut private = PrivateFile::open_existing(path)?;
    let opened = private.file.metadata()?;
    if opened.len() > max_bytes {
        return Err(invalid(path, "exceeds its fixed read bound"));
    }
    let capacity = usize::try_from(opened.len())
        .unwrap_or(usize::MAX)
        .saturating_add(1);
    let mut bytes = Zeroizing::new(Vec::with_capacity(capacity));
    Read::by_ref(&mut private.file)
        .take(read_limit)
        .read_to_end(&mut bytes)?;
    private.verify_named()?;
    if bytes.len() as u64 > max_bytes {
        return Err(invalid(path, "exceeds its fixed read bound"));
    }
    Ok(bytes)
}

/// Read one exactly-32-byte, nonzero secret from the same descriptor that passed custody checks.
pub fn load_secret32(path: &Path) -> io::Result<Zeroizing<[u8; 32]>> {
    #[cfg(unix)]
    let encoded = read_unix_file_bounded(
        path,
        MAX_PRIVATE_FILE_BYTES,
        DirPolicy::private(),
        FilePolicy::private(MAX_PRIVATE_FILE_BYTES),
        true,
    )?;
    #[cfg(not(unix))]
    let encoded = read_private_file_bounded(path, 64 * 1024)?;
    if encoded.len() != 32 {
        return Err(invalid(path, "must contain exactly 32 bytes"));
    }
    let mut seed = Zeroizing::new([0u8; 32]);
    seed.copy_from_slice(&encoded);
    if *seed == [0u8; 32] {
        return Err(invalid(path, "must not contain the all-zero seed"));
    }
    Ok(seed)
}

/// Publish a fully written private seed once, or load the independently verified concurrent winner.
pub fn load_or_create_secret32(
    path: &Path,
    candidate: &[u8; 32],
) -> io::Result<(Zeroizing<[u8; 32]>, bool)> {
    #[cfg(unix)]
    {
        load_or_create_secret32_unix(path, candidate)
    }
    #[cfg(not(unix))]
    {
        load_or_create_secret32_nonunix(path, candidate)
    }
}

#[cfg(unix)]
fn load_or_create_secret32_unix(
    path: &Path,
    candidate: &[u8; 32],
) -> io::Result<(Zeroizing<[u8; 32]>, bool)> {
    let normalized = NormalizedPath::new(path).map_err(custody_io_error)?;
    match load_secret32(normalized.as_path()) {
        Ok(seed) => return Ok((seed, false)),
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        Err(_) => {}
    }
    if *candidate == [0u8; 32] {
        return Err(invalid(path, "must not contain the all-zero seed"));
    }

    let (parent, destination_name) = PrivateDirectory::open_for_file(normalized.as_path(), true)?;
    let mut temp_name = OsString::from(".");
    temp_name.push(destination_name.as_os_str());
    temp_name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let temp_name = LeafName::new(temp_name).map_err(custody_io_error)?;
    let mut temporary_file = parent
        .guarded
        .create_file(&temp_name, FilePolicy::private(32))
        .map_err(custody_io_error)?;
    let write_result = (|| -> io::Result<()> {
        temporary_file.write_all(candidate)?;
        temporary_file.sync_all().map_err(custody_io_error)?;
        temporary_file.verify_named().map_err(custody_io_error)
    })();
    if let Err(error) = write_result {
        let cleanup = parent
            .guarded
            .unlink_file(temporary_file)
            .map_err(custody_io_error);
        parent.verify_named()?;
        cleanup?;
        return Err(error);
    }

    let published =
        match parent
            .guarded
            .publish_no_replace(temporary_file, &parent.guarded, &destination_name)
        {
            Ok(file) => {
                file.verify_named().map_err(custody_io_error)?;
                true
            }
            Err(CustodyError::AlreadyExists) => {
                remove_unix_private_name(&parent, &temp_name)?;
                false
            }
            Err(error) => {
                let cleanup = remove_unix_private_name(&parent, &temp_name);
                parent.verify_named()?;
                cleanup?;
                return Err(custody_io_error(error));
            }
        };
    parent.verify_named()?;
    parent.sync()?;
    let seed = load_secret32(normalized.as_path())?;
    Ok((seed, published))
}

#[cfg(unix)]
fn remove_unix_private_name(parent: &PrivateDirectory, name: &LeafName) -> io::Result<()> {
    let file = parent
        .guarded
        .open_file_optional(
            name,
            OpenAccess::ReadWrite,
            FilePolicy::private(MAX_PRIVATE_FILE_BYTES),
        )
        .map_err(custody_io_error)?;
    if let Some(file) = file {
        parent.guarded.unlink_file(file).map_err(custody_io_error)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn load_or_create_secret32_nonunix(
    path: &Path,
    candidate: &[u8; 32],
) -> io::Result<(Zeroizing<[u8; 32]>, bool)> {
    let path = normalized_private_path(path)?;
    match load_secret32(&path) {
        Ok(seed) => return Ok((seed, false)),
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        Err(_) => {}
    }
    if *candidate == [0u8; 32] {
        return Err(invalid(&path, "must not contain the all-zero seed"));
    }

    let parent_path = parent_path(&path)?;
    let parent = PrivateDirectory::secure_for(&path)?;
    let filename = path
        .file_name()
        .ok_or_else(|| invalid(&path, "has no filename"))?;
    let mut temp_name = OsString::from(".");
    temp_name.push(filename);
    temp_name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let temporary = parent_path.join(temp_name);
    let mut temporary_file = PrivateFile::create_new(&temporary)?;
    let write_result = (|| -> io::Result<()> {
        temporary_file.file.write_all(candidate)?;
        temporary_file.file.sync_all()?;
        temporary_file.verify_named()
    })();
    if let Err(error) = write_result {
        drop(temporary_file);
        let cleanup = remove_private_name(&parent, &temporary);
        parent.verify_named()?;
        cleanup?;
        return Err(error);
    }
    drop(temporary_file);

    let published = match platform::publish_noclobber(&parent.file, &temporary, &path) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            remove_private_name(&parent, &temporary)?;
            parent.verify_named()?;
            false
        }
        Err(error) => {
            let cleanup = remove_private_name(&parent, &temporary);
            parent.verify_named()?;
            cleanup?;
            return Err(error);
        }
    };
    parent.verify_named()?;
    parent.sync()?;
    let seed = load_secret32(&path)?;
    Ok((seed, published))
}

fn invalid(path: &Path, reason: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("private storage {} {reason}", path.display()),
    )
}

#[cfg(not(unix))]
fn remove_private_name(parent: &PrivateDirectory, path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        platform::remove_private_file(&parent.file, path)
    }
    #[cfg(not(windows))]
    {
        let _ = parent;
        fs::remove_file(path)
    }
}

#[cfg(windows)]
mod platform {
    use std::ffi::OsString;
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::path::{Component, Path, PathBuf, Prefix};

    use winapi_util::file::{information, typ};
    use windows_permissions::constants::{
        AccessRights, AceType, SeObjectType, SecurityInformation,
    };
    use windows_permissions::{wrappers, LocalBox, SecurityDescriptor, Sid};

    use super::invalid;

    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_ADD_SUBDIRECTORY: u32 = 0x0000_0004;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const READ_CONTROL: u32 = 0x0002_0000;
    const WRITE_DAC: u32 = 0x0004_0000;
    const WRITE_OWNER: u32 = 0x0008_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;

    #[derive(Debug)]
    struct GuardedParent {
        path: PathBuf,
        name: Option<OsString>,
        file: File,
        identity: pigeonpost_windows_custody::FileIdentity,
        guards_target_name: bool,
    }

    /// No-delete-share handles for the complete path from the volume root through the immediate
    /// parent. Keeping every component open prevents a name swap above the direct parent from
    /// redirecting a later path-based CreateFile/MoveFile operation.
    #[derive(Debug)]
    pub(super) struct ParentGuard {
        target: PathBuf,
        components: Vec<GuardedParent>,
    }

    impl ParentGuard {
        fn acquire(path: &Path) -> io::Result<Self> {
            let target = normalized_absolute(path)?;
            let parent = target
                .parent()
                .ok_or_else(|| invalid(path, "has no parent directory"))?;
            let (anchor_path, names) = split_absolute_parent(parent)?;
            let anchor_guards_target = names.is_empty();
            let anchor = pigeonpost_windows_custody::lock_directory(open_root_anchor(
                &anchor_path,
                anchor_guards_target,
            )?)?;
            verify_parent_descriptor(anchor.file(), &anchor_path, anchor_guards_target)?;
            let (anchor_file, anchor_identity) = anchor.into_parts();
            let name_count = names.len();
            let mut components = Vec::with_capacity(name_count + 1);
            let mut component_path = anchor_path;
            components.push(GuardedParent {
                path: component_path.clone(),
                name: None,
                file: anchor_file,
                identity: anchor_identity,
                guards_target_name: anchor_guards_target,
            });
            for (index, name) in names.into_iter().enumerate() {
                let guards_target_name = index + 1 == name_count;
                let preceding = components
                    .last()
                    .ok_or_else(|| invalid(path, "has no root anchor"))?;
                let locked = if guards_target_name {
                    pigeonpost_windows_custody::open_directory_for_child(&preceding.file, &name)?
                } else {
                    pigeonpost_windows_custody::open_directory(&preceding.file, &name)?
                };
                component_path.push(&name);
                verify_parent_descriptor(locked.file(), &component_path, guards_target_name)?;
                let (file, identity) = locked.into_parts();
                components.push(GuardedParent {
                    path: component_path.clone(),
                    name: Some(name),
                    file,
                    identity,
                    guards_target_name,
                });
            }
            if components.is_empty() {
                return Err(invalid(path, "has no guardable parent directory"));
            }
            Ok(Self { target, components })
        }

        fn target(&self) -> &Path {
            &self.target
        }

        fn immediate_parent(&self) -> io::Result<&File> {
            self.components
                .last()
                .map(|component| &component.file)
                .ok_or_else(|| invalid(&self.target, "has no guarded parent directory"))
        }

        pub(super) fn verify(&self) -> io::Result<()> {
            for (index, component) in self.components.iter().enumerate() {
                verify_disk_object(&component.file, &component.path, true)?;
                let reopened = if let Some(name) = &component.name {
                    let preceding = index
                        .checked_sub(1)
                        .and_then(|preceding| self.components.get(preceding))
                        .ok_or_else(|| {
                            invalid(&component.path, "has no retained preceding ancestor")
                        })?;
                    if component.guards_target_name {
                        pigeonpost_windows_custody::open_directory_for_child(&preceding.file, name)?
                    } else {
                        pigeonpost_windows_custody::open_directory(&preceding.file, name)?
                    }
                } else {
                    pigeonpost_windows_custody::lock_directory(open_root_anchor(
                        &component.path,
                        component.guards_target_name,
                    )?)?
                };
                if reopened.identity() != component.identity {
                    return Err(invalid(
                        &component.path,
                        "changed while custody checks were running",
                    ));
                }
                verify_parent_descriptor(
                    &component.file,
                    &component.path,
                    component.guards_target_name,
                )?;
            }
            Ok(())
        }
    }

    pub(super) fn open_existing_file(path: &Path) -> io::Result<File> {
        let file = open_existing(path, false)?;
        verify_file_named(&file, path)?;
        Ok(file)
    }

    pub(super) fn open_or_create_file(path: &Path) -> io::Result<(File, bool)> {
        let mut create = OpenOptions::new();
        create
            .read(true)
            .write(true)
            .create_new(true)
            .access_mode(GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC | WRITE_OWNER)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        let (mut file, created) = match create.open(path) {
            Ok(file) => (file, true),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                (open_existing(path, true)?, false)
            }
            Err(error) => return Err(error),
        };
        if created {
            protect_private_file(&mut file, path)?;
        }
        verify_file_named(&file, path)?;
        Ok((file, created))
    }

    pub(super) fn create_new_file(path: &Path) -> io::Result<File> {
        let mut create = OpenOptions::new();
        create
            .read(true)
            .write(true)
            .create_new(true)
            .access_mode(GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC | WRITE_OWNER)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        let mut file = create.open(path)?;
        protect_private_file(&mut file, path)?;
        verify_file_named(&file, path)?;
        Ok(file)
    }

    pub(super) fn open_and_protect_subsystem_file(path: &Path) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .access_mode(GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC | WRITE_OWNER)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        let mut file = options.open(path)?;
        verify_disk_object(&file, path, false)?;
        let information = information(&file).map_err(other)?;
        if information.number_of_links() != 1 {
            return Err(invalid(path, "must have exactly one hard link"));
        }
        protect_private_file(&mut file, path)?;
        verify_file_named(&file, path)?;
        Ok(file)
    }

    pub(super) fn publish_noclobber(
        _parent: &File,
        temporary: &Path,
        destination: &Path,
    ) -> io::Result<()> {
        // The caller retains and revalidates the complete ancestor chain. The shared primitive
        // supplies both atomic no-clobber semantics and `MOVEFILE_WRITE_THROUGH` durability.
        pigeonpost_windows_custody::move_file_noclobber_write_through(temporary, destination)
    }

    pub(super) fn replace_write_through(
        _parent: &File,
        temporary: &Path,
        destination: &Path,
    ) -> io::Result<()> {
        // The retained ancestor chain prevents parent replacement while the shared primitive
        // provides atomic replacement plus write-through durability.
        pigeonpost_windows_custody::replace_file_write_through(temporary, destination)
    }

    pub(super) fn remove_private_file(_parent: &File, path: &Path) -> io::Result<()> {
        let file = open_existing(path, false)?;
        verify_private_handle(&file, path, false)?;
        verify_same_named_object(&file, path, false)?;
        drop(file);
        std::fs::remove_file(path)
    }

    pub(super) fn secure_directory(path: &Path) -> io::Result<(File, PathBuf, ParentGuard)> {
        let parents = ParentGuard::acquire(path)?;
        let path = parents.target().to_path_buf();
        let name = path
            .file_name()
            .ok_or_else(|| invalid(&path, "has no final directory name"))?;
        let locked = match pigeonpost_windows_custody::create_private_directory(
            parents.immediate_parent()?,
            name,
        )? {
            pigeonpost_windows_custody::CreateDirectory::Created(directory) => directory,
            pigeonpost_windows_custody::CreateDirectory::AlreadyExists => {
                pigeonpost_windows_custody::open_directory(parents.immediate_parent()?, name)?
            }
        };
        let bound_identity = locked.identity();
        let (directory, opened_identity) = locked.into_parts();
        if opened_identity != bound_identity {
            return Err(invalid(&path, "creation identity changed before custody"));
        }
        verify_private_handle(&directory, &path, true)?;
        let reopened =
            pigeonpost_windows_custody::open_directory(parents.immediate_parent()?, name)?;
        if reopened.identity() != bound_identity {
            return Err(invalid(&path, "changed while custody checks were running"));
        }
        parents.verify()?;
        Ok((directory, path, parents))
    }

    pub(super) fn verify_file_named(file: &File, path: &Path) -> io::Result<()> {
        verify_private_handle(file, path, false)?;
        verify_same_named_object(file, path, false)
    }

    pub(super) fn verify_directory_named(file: &File, path: &Path) -> io::Result<()> {
        verify_private_handle(file, path, true)?;
        verify_same_named_object(file, path, true)
    }

    pub(super) fn sync_directory(_file: &File) -> io::Result<()> {
        Ok(())
    }

    fn protect_private_file(file: &mut File, path: &Path) -> io::Result<()> {
        let current = windows_permissions::utilities::current_process_sid().map_err(other)?;
        let descriptor = private_descriptor(&current, false)?;
        let dacl = descriptor
            .dacl()
            .ok_or_else(|| invalid(path, "private file descriptor has no DACL"))?;
        wrappers::SetSecurityInfo(
            file,
            SeObjectType::SE_FILE_OBJECT,
            SecurityInformation::Owner
                | SecurityInformation::Dacl
                | SecurityInformation::ProtectedDacl,
            Some(&current),
            None,
            Some(dacl),
            None,
        )
        .map_err(other)?;
        verify_private_handle(file, path, false)
    }

    fn open_existing(path: &Path, writable: bool) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        if writable {
            options
                .write(true)
                .access_mode(GENERIC_READ | GENERIC_WRITE | READ_CONTROL);
        } else {
            options.access_mode(GENERIC_READ | READ_CONTROL);
        }
        options.open(path)
    }

    fn open_directory(path: &Path, writable_dacl: bool) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
        if writable_dacl {
            options.access_mode(GENERIC_READ | READ_CONTROL | WRITE_DAC);
        } else {
            options.access_mode(GENERIC_READ | READ_CONTROL);
        }
        options.open(path)
    }

    fn open_root_anchor(path: &Path, can_create_child: bool) -> io::Result<File> {
        let mut options = OpenOptions::new();
        let mut access = GENERIC_READ | READ_CONTROL;
        if can_create_child {
            access |= FILE_ADD_SUBDIRECTORY;
        }
        options
            .read(true)
            .access_mode(access)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
        options.open(path)
    }

    fn split_absolute_parent(path: &Path) -> io::Result<(PathBuf, Vec<OsString>)> {
        let mut anchor = PathBuf::new();
        let mut names = Vec::new();
        for component in path.components() {
            match component {
                Component::Prefix(_) | Component::RootDir => anchor.push(component.as_os_str()),
                Component::Normal(name) => names.push(name.to_os_string()),
                Component::CurDir | Component::ParentDir => {
                    return Err(invalid(
                        path,
                        "contains a non-normalized ancestor component",
                    ));
                }
            }
        }
        if !anchor.is_absolute() {
            return Err(invalid(path, "has no local volume/root anchor"));
        }
        Ok((anchor, names))
    }

    fn private_descriptor(
        current: &Sid,
        directory: bool,
    ) -> io::Result<LocalBox<SecurityDescriptor>> {
        let ace_flags = if directory { "OICI" } else { "" };
        format!("O:{current}D:P(A;{ace_flags};FA;;;{current})")
            .parse()
            .map_err(other)
    }

    fn security_descriptor(file: &File) -> io::Result<LocalBox<SecurityDescriptor>> {
        wrappers::GetSecurityInfo(
            file,
            SeObjectType::SE_FILE_OBJECT,
            SecurityInformation::Owner | SecurityInformation::Dacl,
        )
        .map_err(other)
    }

    fn verify_private_handle(file: &File, path: &Path, directory: bool) -> io::Result<()> {
        verify_disk_object(file, path, directory)?;
        let info = information(file).map_err(other)?;
        if !directory && info.number_of_links() != 1 {
            return Err(invalid(path, "must have exactly one hard link"));
        }
        let descriptor = security_descriptor(file)?;
        let current = windows_permissions::utilities::current_process_sid().map_err(other)?;
        if descriptor.owner() != Some(&*current) {
            return Err(invalid(path, "must be owned by the current user"));
        }
        let dacl = descriptor
            .dacl()
            .ok_or_else(|| invalid(path, "must have a non-null private DACL"))?;
        if dacl.len() != 1 {
            return Err(invalid(path, "must grant access only to the current user"));
        }
        let ace = dacl
            .get_ace(0)
            .ok_or_else(|| invalid(path, "private DACL is malformed"))?;
        let inheritance_is_private = !directory
            || ace.flags().contains(
                windows_permissions::constants::AceFlags::ObjectInherit
                    | windows_permissions::constants::AceFlags::ContainerInherit,
            );
        if ace.ace_type() != AceType::ACCESS_ALLOWED_ACE_TYPE
            || ace.sid() != Some(&*current)
            || !ace.mask().contains(AccessRights::FileAllAccess)
            || !inheritance_is_private
        {
            return Err(invalid(
                path,
                "must grant full access only to the current user",
            ));
        }
        let sddl = wrappers::ConvertSecurityDescriptorToStringSecurityDescriptor(
            &descriptor,
            SecurityInformation::Owner | SecurityInformation::Dacl,
        )
        .map_err(other)?;
        if !sddl.to_string_lossy().contains("D:P") {
            return Err(invalid(path, "must have a protected DACL"));
        }
        Ok(())
    }

    fn verify_disk_object(file: &File, path: &Path, directory: bool) -> io::Result<()> {
        let metadata = file.metadata()?;
        let attributes = metadata.file_attributes();
        let is_directory = attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || is_directory != directory
            || !typ(file).map_err(other)?.is_disk()
        {
            let expected = if directory {
                "directory"
            } else {
                "regular file"
            };
            return Err(invalid(
                path,
                &format!("must be a disk {expected}, not a reparse point"),
            ));
        }
        Ok(())
    }

    fn verify_same_named_object(file: &File, path: &Path, directory: bool) -> io::Result<()> {
        let named = if directory {
            open_directory(path, false)?
        } else {
            open_existing(path, false)?
        };
        verify_disk_object(&named, path, directory)?;
        let opened_identity = pigeonpost_windows_custody::file_identity(file)?;
        let named_identity = pigeonpost_windows_custody::file_identity(&named)?;
        let named_information = information(&named).map_err(other)?;
        if opened_identity != named_identity
            || (!directory && named_information.number_of_links() != 1)
        {
            return Err(invalid(path, "changed while custody checks were running"));
        }
        Ok(())
    }

    pub(super) fn normalized_absolute(path: &Path) -> io::Result<PathBuf> {
        if path.as_os_str().is_empty() {
            return Err(invalid(path, "must not be empty"));
        }
        let first = path.components().next();
        if matches!(first, Some(Component::Prefix(_))) && !path.has_root() {
            return Err(invalid(path, "must not be drive-relative"));
        }
        if path.has_root() && !matches!(first, Some(Component::Prefix(_))) {
            return Err(invalid(path, "must include an explicit drive prefix"));
        }
        let input = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        let mut normalized = PathBuf::new();
        for component in input.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    return Err(invalid(
                        path,
                        "must not contain a parent-directory component",
                    ));
                }
                Component::Prefix(prefix) => match prefix.kind() {
                    Prefix::Disk(_) | Prefix::VerbatimDisk(_) => {
                        normalized.push(prefix.as_os_str())
                    }
                    _ => {
                        return Err(invalid(
                            path,
                            "must use a local disk path, not UNC or a device namespace",
                        ));
                    }
                },
                Component::RootDir => normalized.push(component.as_os_str()),
                Component::Normal(part) => {
                    pigeonpost_windows_custody::validate_component(part)
                        .map_err(|error| invalid(path, &error.to_string()))?;
                    normalized.push(part);
                }
            }
        }
        if !normalized.is_absolute() {
            return Err(invalid(path, "must resolve to an absolute local disk path"));
        }
        let encoded = normalized
            .to_str()
            .ok_or_else(|| invalid(path, "must be losslessly Unicode on Windows"))?;
        if encoded.contains('\0') || encoded.encode_utf16().count() > 32_767 {
            return Err(invalid(
                path,
                "must contain no embedded NUL and fit the Windows path limit",
            ));
        }
        Ok(normalized)
    }

    fn verify_parent_descriptor(
        directory: &File,
        path: &Path,
        guards_target_name: bool,
    ) -> io::Result<()> {
        let descriptor = security_descriptor(directory)?;
        let current = windows_permissions::utilities::current_process_sid().map_err(other)?;
        let owner = descriptor
            .owner()
            .ok_or_else(|| invalid(path, "parent component has no owner"))?;
        if !trusted_principal(owner, &current) {
            return Err(invalid(path, "parent component has an untrusted owner"));
        }
        let dacl = descriptor
            .dacl()
            .ok_or_else(|| invalid(path, "parent component has a null DACL"))?;
        for index in 0..dacl.len() {
            let ace = dacl
                .get_ace(index)
                .ok_or_else(|| invalid(path, "parent DACL is malformed"))?;
            if ace
                .flags()
                .contains(windows_permissions::constants::AceFlags::InheritOnly)
                || !is_allow_ace(ace.ace_type())
            {
                continue;
            }
            let sid = ace
                .sid()
                .ok_or_else(|| invalid(path, "parent allow ACE has no SID"))?;
            if !trusted_principal(sid, &current)
                && dangerous_parent_rights(ace.mask(), guards_target_name)
            {
                return Err(invalid(
                    path,
                    "parent grants mutation rights to an untrusted principal",
                ));
            }
        }
        Ok(())
    }

    fn is_allow_ace(ace_type: AceType) -> bool {
        matches!(
            ace_type,
            AceType::ACCESS_ALLOWED_ACE_TYPE
                | AceType::ACCESS_ALLOWED_CALLBACK_ACE_TYPE
                | AceType::ACCESS_ALLOWED_CALLBACK_OBJECT_ACE_TYPE
                | AceType::ACCESS_ALLOWED_OBJECT_ACE_TYPE
        )
    }

    fn dangerous_parent_rights(rights: AccessRights, guards_target_name: bool) -> bool {
        rights.intersects(
            AccessRights::GenericAll
                | AccessRights::GenericWrite
                | AccessRights::Delete
                | AccessRights::WriteDac
                | AccessRights::WriteOwner
                // FILE_WRITE_EA, FILE_DELETE_CHILD, and FILE_WRITE_ATTRIBUTES can alter or replace
                // an existing traversed component.
                | AccessRights::Bit4
                | AccessRights::Bit6
                | AccessRights::Bit8,
        ) || (guards_target_name
            // FILE_ADD_FILE and FILE_ADD_SUBDIRECTORY can squat the one direct child name this
            // directory guards. On a grandparent they can create only an unrelated sibling unless
            // combined with one of the replacement rights above.
            && rights.intersects(AccessRights::Bit1 | AccessRights::Bit2))
    }

    fn trusted_principal(sid: &Sid, current: &Sid) -> bool {
        sid == current
            || matches!(
                sid.to_string().as_str(),
                "S-1-5-18"
                    | "S-1-5-32-544"
                    | "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"
            )
    }

    fn other(error: impl std::fmt::Display) -> io::Error {
        io::Error::other(error.to_string())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn creation_identity_is_bound_to_the_exact_opened_directory() {
            let root = tempfile::tempdir().unwrap();
            let created_path = root.path().join("created");
            let replacement_path = root.path().join("replacement");
            std::fs::create_dir(&created_path).unwrap();
            std::fs::create_dir(&replacement_path).unwrap();

            let parent = pigeonpost_windows_custody::lock_directory(
                open_root_anchor(root.path(), true).unwrap(),
            )
            .unwrap();
            let created = pigeonpost_windows_custody::open_directory(
                parent.file(),
                std::ffi::OsStr::new("created"),
            )
            .unwrap();
            let replacement = pigeonpost_windows_custody::open_directory(
                parent.file(),
                std::ffi::OsStr::new("replacement"),
            )
            .unwrap();
            let identity = created.identity();
            assert_eq!(identity, created.identity());
            assert_ne!(
                identity,
                replacement.identity(),
                "creation provenance must not authorize a different directory handle"
            );
        }

        #[test]
        fn secure_creation_rejects_embedded_nul_without_mutating_the_parent() {
            let root = tempfile::tempdir().unwrap();
            let parent = open_root_anchor(root.path(), true).unwrap();
            let before = std::fs::read_dir(root.path()).unwrap().count();

            assert!(pigeonpost_windows_custody::create_private_directory(
                &parent,
                std::ffi::OsStr::new("invalid\0directory")
            )
            .is_err());
            assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), before);
        }

        #[test]
        fn child_creation_rights_are_dangerous_only_on_the_immediate_parent() {
            let child_creation = AccessRights::Bit1 | AccessRights::Bit2;
            assert!(dangerous_parent_rights(child_creation, true));
            assert!(!dangerous_parent_rights(child_creation, false));
            assert!(dangerous_parent_rights(AccessRights::Bit6, false));
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use std::fs::File;
    use std::io;
    use std::path::Path;

    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "private file custody is unsupported on this platform",
        )
    }

    pub(super) fn open_existing_file(_path: &Path) -> io::Result<File> {
        Err(unsupported())
    }
    pub(super) fn open_or_create_file(_path: &Path) -> io::Result<(File, bool)> {
        Err(unsupported())
    }
    pub(super) fn create_new_file(_path: &Path) -> io::Result<File> {
        Err(unsupported())
    }
    pub(super) fn publish_noclobber(
        _parent: &File,
        _temporary: &Path,
        _destination: &Path,
    ) -> io::Result<()> {
        Err(unsupported())
    }
    pub(super) fn replace_write_through(
        _parent: &File,
        _temporary: &Path,
        _destination: &Path,
    ) -> io::Result<()> {
        Err(unsupported())
    }
    pub(super) fn secure_directory(_path: &Path, _created_by_this_call: bool) -> io::Result<File> {
        Err(unsupported())
    }
    pub(super) fn verify_file_named(_file: &File, _path: &Path) -> io::Result<()> {
        Err(unsupported())
    }
    pub(super) fn verify_directory_named(_file: &File, _path: &Path) -> io::Result<()> {
        Err(unsupported())
    }
    pub(super) fn sync_directory(_file: &File) -> io::Result<()> {
        Err(unsupported())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_atomic_private_write_replaces_the_complete_value_without_temp_debris() {
        let directory = tempfile::tempdir().unwrap();
        let private = directory.path().join("private");
        let path = private.join("operator.toml");

        write_private_file_atomic(&path, b"version = 1\n", 64).unwrap();
        assert_eq!(
            &*read_private_file_bounded(&path, 64).unwrap(),
            b"version = 1\n"
        );
        write_private_file_atomic(&path, b"version = 2\nenabled = true\n", 64).unwrap();
        assert_eq!(
            &*read_private_file_bounded(&path, 64).unwrap(),
            b"version = 2\nenabled = true\n"
        );

        let names = fs::read_dir(&private)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, vec![OsString::from("operator.toml")]);

        let absent = directory.path().join("absent").join("too-large");
        assert!(write_private_file_atomic(&absent, &[0; 65], 64).is_err());
        assert!(!absent.parent().unwrap().exists());
    }

    #[cfg(unix)]
    #[test]
    fn atomic_private_write_rejects_a_link_without_touching_its_target() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let directory = tempfile::tempdir().unwrap();
        let private = directory.path().join("private");
        fs::create_dir(&private).unwrap();
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
        let target = private.join("target");
        fs::write(&target, b"sentinel").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let destination = private.join("operator.toml");
        symlink(&target, &destination).unwrap();

        assert!(write_private_file_atomic(&destination, b"replacement", 64).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"sentinel");
        assert_eq!(
            fs::read_dir(&private).unwrap().count(),
            2,
            "a rejected destination must not leave a temporary file"
        );
    }

    #[test]
    fn private_seed_creation_is_atomic_and_stable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("private").join("identity.key");
        let (first, created) = load_or_create_secret32(&path, &[7; 32]).unwrap();
        assert!(created);
        assert_eq!(*first, [7; 32]);
        let (second, created) = load_or_create_secret32(&path, &[8; 32]).unwrap();
        assert!(!created);
        assert_eq!(*second, [7; 32]);
    }

    #[test]
    fn concurrent_seed_creators_never_observe_a_transient_hardlink() {
        use std::sync::{Arc, Barrier};

        let directory = tempfile::tempdir().unwrap();
        let path = Arc::new(directory.path().join("private").join("identity.key"));
        let barrier = Arc::new(Barrier::new(16));
        let threads = (0_u8..16)
            .map(|index| {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    load_or_create_secret32(&path, &[index + 1; 32]).unwrap()
                })
            })
            .collect::<Vec<_>>();
        let results = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(results.iter().filter(|(_, created)| *created).count(), 1);
        assert!(results.iter().all(|(seed, _)| seed == &results[0].0));
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(fs::metadata(path.as_ref()).unwrap().nlink(), 1);
        }
    }

    #[cfg(windows)]
    #[test]
    fn no_clobber_publication_preserves_an_existing_destination() {
        let directory = tempfile::tempdir().unwrap();
        let private = directory.path().join("private");
        let destination = private.join("identity.key");
        let temporary = private.join(".identity.key.tmp");
        let parent = PrivateDirectory::secure_for(&destination).unwrap();

        let mut destination_file = PrivateFile::create_new(&destination).unwrap();
        destination_file.file.write_all(&[7; 32]).unwrap();
        destination_file.file.sync_all().unwrap();
        drop(destination_file);

        let mut temporary_file = PrivateFile::create_new(&temporary).unwrap();
        temporary_file.file.write_all(&[8; 32]).unwrap();
        temporary_file.file.sync_all().unwrap();
        drop(temporary_file);

        let error =
            platform::publish_noclobber(&parent.file, &temporary, &destination).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&destination).unwrap(), [7; 32]);
        assert_eq!(fs::read(&temporary).unwrap(), [8; 32]);
    }

    #[cfg(unix)]
    #[test]
    fn unix_no_clobber_publication_preserves_an_existing_destination() {
        let directory = tempfile::tempdir().unwrap();
        let private = directory.path().join("private");
        let destination = private.join("identity.key");
        let temporary = private.join(".identity.key.tmp");
        let parent = PrivateDirectory::open_or_create(&private).unwrap();
        let destination_name = LeafName::new("identity.key").unwrap();
        let temporary_name = LeafName::new(".identity.key.tmp").unwrap();

        let mut destination_file = parent
            .guarded
            .create_file(&destination_name, FilePolicy::private(32))
            .unwrap();
        destination_file.write_all(&[7; 32]).unwrap();
        destination_file.sync_all().unwrap();

        let mut temporary_file = parent
            .guarded
            .create_file(&temporary_name, FilePolicy::private(32))
            .unwrap();
        temporary_file.write_all(&[8; 32]).unwrap();
        temporary_file.sync_all().unwrap();

        let error = parent
            .guarded
            .publish_no_replace(temporary_file, &parent.guarded, &destination_name)
            .unwrap_err();
        assert!(matches!(error, CustodyError::AlreadyExists));
        assert_eq!(fs::read(&destination).unwrap(), [7; 32]);
        assert_eq!(fs::read(&temporary).unwrap(), [8; 32]);
    }

    #[cfg(unix)]
    #[test]
    fn unix_rejects_symlink_hardlink_and_public_mode() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let directory = tempfile::tempdir().unwrap();
        let private = directory.path().join("private");
        fs::create_dir(&private).unwrap();
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();

        let target = private.join("target.key");
        fs::write(&target, [7; 32]).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let linked = private.join("linked.key");
        fs::hard_link(&target, &linked).unwrap();
        assert!(load_secret32(&target).is_err());
        assert!(load_secret32(&linked).is_err());

        fs::remove_file(&linked).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
        assert_eq!(
            load_secret32(&target).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );

        let symlinked = private.join("symlink.key");
        symlink(&target, &symlinked).unwrap();
        assert!(load_secret32(&symlinked).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unix_detects_file_and_parent_replacement() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let private = root.path().join("private");
        let path = private.join("directory.db");
        let (custody, _) = PrivateFile::open_or_create(&path).unwrap();

        let moved = private.join("moved.db");
        fs::rename(&path, &moved).unwrap();
        fs::write(&path, []).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(custody.verify_named().is_err());

        fs::remove_file(&path).unwrap();
        fs::rename(&moved, &path).unwrap();
        let (custody, _) = PrivateFile::open_or_create(&path).unwrap();
        let old_parent = root.path().join("private-old");
        fs::rename(&private, &old_parent).unwrap();
        fs::create_dir(&private).unwrap();
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(&path, []).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(custody.verify_named().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unix_rejects_a_symlink_parent() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real");
        fs::create_dir(&real).unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o700)).unwrap();
        let linked = root.path().join("linked");
        symlink(&real, &linked).unwrap();
        assert!(PrivateFile::open_or_create(&linked.join("directory.db")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unix_rejects_lexical_escape_intermediate_links_and_mutable_ancestors_without_effects() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = tempfile::tempdir().unwrap();
        let escaped = root.path().join("escaped");
        let lexical = root
            .path()
            .join("would-be-created")
            .join("..")
            .join("escaped")
            .join("identity.key");
        assert!(load_or_create_secret32(&lexical, &[7; 32]).is_err());
        assert!(!root.path().join("would-be-created").exists());
        assert!(!escaped.exists());

        let actual = root.path().join("actual");
        fs::create_dir(&actual).unwrap();
        fs::set_permissions(&actual, fs::Permissions::from_mode(0o700)).unwrap();
        let alias = root.path().join("alias");
        symlink(&actual, &alias).unwrap();
        assert!(load_or_create_secret32(&alias.join("child/identity.key"), &[7; 32]).is_err());
        assert!(!actual.join("child").exists());

        let mutable = root.path().join("mutable");
        fs::create_dir(&mutable).unwrap();
        fs::set_permissions(&mutable, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(load_or_create_secret32(&mutable.join("private/identity.key"), &[7; 32]).is_err());
        assert!(!mutable.join("private").exists());
    }

    #[cfg(unix)]
    #[test]
    fn bounded_reads_use_guarded_descriptors_and_proportional_capacity() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let trusted = root.path().join("trusted.toml");
        fs::write(&trusted, b"value = 7\n").unwrap();
        fs::set_permissions(&trusted, fs::Permissions::from_mode(0o644)).unwrap();
        let bytes = read_trusted_file_bounded(&trusted, 64).unwrap();
        assert_eq!(&*bytes, b"value = 7\n");
        assert!(bytes.capacity() <= 65);
        assert!(read_trusted_file_bounded(&trusted, 4).is_err());

        let private_dir = root.path().join("private");
        fs::create_dir(&private_dir).unwrap();
        fs::set_permissions(&private_dir, fs::Permissions::from_mode(0o700)).unwrap();
        let secret = private_dir.join("credential");
        fs::write(&secret, b"secret").unwrap();
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o400)).unwrap();
        assert_eq!(&*read_private_file_bounded(&secret, 6).unwrap(), b"secret");
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o200)).unwrap();
        assert!(read_private_file_bounded(&secret, 6).is_err());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn retained_private_files_compare_full_identity_without_reopening_for_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let private = directory.path().join("private");
        let first_path = private.join("first.key");
        let second_path = private.join("second.key");
        load_or_create_secret32(&first_path, &[1; 32]).unwrap();
        load_or_create_secret32(&second_path, &[2; 32]).unwrap();
        let first = PrivateFile::open_existing(&first_path).unwrap();
        let first_again = PrivateFile::open_existing(&first_path).unwrap();
        let second = PrivateFile::open_existing(&second_path).unwrap();

        assert!(first.same_object(&first_again).unwrap());
        assert!(!first.same_object(&second).unwrap());
        assert!(first.normalized_path().is_absolute());
        assert_eq!(first.normalized_path(), first_again.normalized_path());
        assert_ne!(first.normalized_path(), second.normalized_path());
        assert_eq!(first.descriptor().metadata().unwrap().len(), 32);
    }

    #[cfg(windows)]
    #[test]
    fn windows_rejects_hardlinked_private_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("private").join("identity.key");
        load_or_create_secret32(&path, &[7; 32]).unwrap();
        fs::hard_link(&path, path.with_extension("linked")).unwrap();
        assert!(load_secret32(&path).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_parent_components_are_rejected_before_any_creation() {
        let root = tempfile::tempdir().unwrap();
        let escaped = root.path().join("escaped");
        let attempted = root
            .path()
            .join("would-be-created")
            .join("..")
            .join("escaped")
            .join("identity.key");

        assert!(load_or_create_secret32(&attempted, &[7; 32]).is_err());
        assert!(!root.path().join("would-be-created").exists());
        assert!(!escaped.exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_reparse_parent_is_rejected_without_touching_its_target() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("outside");
        fs::create_dir(&target).unwrap();
        let linked = root.path().join("linked");
        let junction = std::process::Command::new("cmd.exe")
            .args(["/C", "mklink", "/J"])
            .arg(&linked)
            .arg(&target)
            .output()
            .unwrap();
        assert!(
            junction.status.success(),
            "failed to create test junction: {}",
            String::from_utf8_lossy(&junction.stderr)
        );

        assert!(load_or_create_secret32(&linked.join("identity.key"), &[7; 32]).is_err());
        assert!(!target.join("identity.key").exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_open_or_create_keeps_an_existing_private_file_writable() {
        use std::io::Write;

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("private").join("operator.toml");
        let (first, created) = PrivateFile::open_or_create(&path).unwrap();
        assert!(created);
        let mut descriptor = first.descriptor();
        descriptor.write_all(b"first").unwrap();
        descriptor.sync_all().unwrap();
        drop(first);

        let (second, created) = PrivateFile::open_or_create(&path).unwrap();
        assert!(!created);
        second.descriptor().set_len(0).unwrap();
        let mut descriptor = second.descriptor();
        descriptor.write_all(b"second").unwrap();
        descriptor.sync_all().unwrap();
        drop(second);

        assert_eq!(&*read_private_file_bounded(&path, 16).unwrap(), b"second");
    }

    #[cfg(windows)]
    #[test]
    fn windows_retained_custody_blocks_parent_rename() {
        let root = tempfile::tempdir().unwrap();
        let private = root.path().join("private");
        let path = private.join("identity.key");
        let (custody, _) = PrivateFile::open_or_create(&path).unwrap();
        let moved = root.path().join("moved");

        assert!(fs::rename(&private, &moved).is_err());
        drop(custody);
        fs::rename(&private, &moved).unwrap();
    }
}
