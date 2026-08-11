#![forbid(unsafe_code)]

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
use rustix::fs::RenameFlags;
use rustix::fs::{self, AtFlags, Dir, FileType, Mode, OFlags, Stat};
use rustix::io::Errno;
use rustix::process::geteuid;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

const MAX_PATH_BYTES: usize = 4096;
const MAX_PATH_COMPONENTS: usize = 128;
const MAX_COMPONENT_BYTES: usize = 255;
const MAX_DIRECTORY_ENTRIES: usize = 1_000_000;

/// Result type for custody operations.
pub type Result<T> = std::result::Result<T, CustodyError>;

/// A deliberately non-path-bearing error from a custody operation.
#[derive(Debug)]
pub enum CustodyError {
    InvalidPath(&'static str),
    UnsafeAncestor(&'static str),
    UnsafeDirectory(&'static str),
    UnsafeFile(&'static str),
    NotFound,
    AlreadyExists,
    LimitExceeded(&'static str),
    Unsupported(&'static str),
    Io(io::Error),
}

impl fmt::Display for CustodyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath(reason) => write!(formatter, "invalid custody path: {reason}"),
            Self::UnsafeAncestor(reason) => write!(formatter, "unsafe ancestor: {reason}"),
            Self::UnsafeDirectory(reason) => write!(formatter, "unsafe directory: {reason}"),
            Self::UnsafeFile(reason) => write!(formatter, "unsafe file: {reason}"),
            Self::NotFound => formatter.write_str("custody object was not found"),
            Self::AlreadyExists => formatter.write_str("custody object already exists"),
            Self::LimitExceeded(reason) => write!(formatter, "custody limit exceeded: {reason}"),
            Self::Unsupported(reason) => {
                write!(formatter, "unsupported custody operation: {reason}")
            }
            Self::Io(error) => write!(formatter, "custody I/O failed: {error}"),
        }
    }
}

impl Error for CustodyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for CustodyError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// A normalized, bounded absolute Unix path with no `.` or `..` components.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct NormalizedPath {
    path: PathBuf,
    components: Vec<OsString>,
}

impl NormalizedPath {
    /// Parses the complete path before any filesystem mutation can occur.
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let supplied = path.as_ref();
        if supplied.as_os_str().as_bytes().is_empty() {
            return Err(CustodyError::InvalidPath("path is empty"));
        }
        if supplied.as_os_str().as_bytes().contains(&0) {
            return Err(CustodyError::InvalidPath("path contains NUL"));
        }

        let absolute = if supplied.is_absolute() {
            supplied.to_path_buf()
        } else {
            std::env::current_dir()?.join(supplied)
        };
        Self::from_absolute(absolute)
    }

    #[cfg(target_os = "macos")]
    fn from_components(components: Vec<OsString>) -> Result<Self> {
        let mut path = PathBuf::from("/");
        for component in &components {
            path.push(component);
        }
        Self::from_absolute(path)
    }

    fn from_absolute(path: PathBuf) -> Result<Self> {
        let bytes = path.as_os_str().as_bytes();
        if bytes.len() > MAX_PATH_BYTES {
            return Err(CustodyError::LimitExceeded("path is too long"));
        }
        if bytes.contains(&0) {
            return Err(CustodyError::InvalidPath("path contains NUL"));
        }

        let mut components = Vec::new();
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(value) => {
                    validate_component_bytes(value.as_bytes())?;
                    components.push(value.to_os_string());
                    if components.len() > MAX_PATH_COMPONENTS {
                        return Err(CustodyError::LimitExceeded("path has too many components"));
                    }
                }
                Component::CurDir => {
                    return Err(CustodyError::InvalidPath("dot components are forbidden"));
                }
                Component::ParentDir => {
                    return Err(CustodyError::InvalidPath("parent components are forbidden"));
                }
                Component::Prefix(_) => {
                    return Err(CustodyError::InvalidPath("non-Unix path prefix"));
                }
            }
        }

        let mut normalized = PathBuf::from("/");
        for component in &components {
            normalized.push(component);
        }
        Ok(Self {
            path: normalized,
            components,
        })
    }

    pub fn as_path(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for NormalizedPath {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

/// One bounded directory-entry name, never a path.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LeafName(OsString);

impl LeafName {
    pub fn new(name: impl AsRef<OsStr>) -> Result<Self> {
        let name = name.as_ref();
        validate_component_bytes(name.as_bytes())?;
        Ok(Self(name.to_os_string()))
    }

    pub fn as_os_str(&self) -> &OsStr {
        &self.0
    }
}

impl AsRef<OsStr> for LeafName {
    fn as_ref(&self) -> &OsStr {
        self.as_os_str()
    }
}

fn validate_component_bytes(bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() {
        return Err(CustodyError::InvalidPath("empty component"));
    }
    if bytes.len() > MAX_COMPONENT_BYTES {
        return Err(CustodyError::LimitExceeded("component is too long"));
    }
    if bytes == b"." || bytes == b".." {
        return Err(CustodyError::InvalidPath("dot component is forbidden"));
    }
    if bytes.contains(&b'/') || bytes.contains(&0) {
        return Err(CustodyError::InvalidPath(
            "component contains a path separator or NUL",
        ));
    }
    Ok(())
}

/// Controls the one platform-defined exception to the no-symlink walk.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AncestorPolicy {
    /// On macOS only, accept the exact root-owned first-component aliases `/etc`, `/tmp`, and
    /// `/var`; all other symlinks remain forbidden.
    #[default]
    PlatformDefault,
    /// Reject every symlink component, including the standard macOS aliases.
    NoSymlinks,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OwnerRule {
    EffectiveUser,
    RootOrEffectiveUser,
}

/// Validation policy for the final directory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirPolicy {
    owner: OwnerRule,
    exact_mode: Option<u32>,
    private_mode: bool,
    reject_group_world_write: bool,
}

impl DirPolicy {
    /// Effective-user-owned, with no permissions outside the owner bits.
    pub const fn private() -> Self {
        Self {
            owner: OwnerRule::EffectiveUser,
            exact_mode: None,
            private_mode: true,
            reject_group_world_write: true,
        }
    }

    /// Effective-user-owned and exactly mode `0700`.
    pub const fn private_mutable() -> Self {
        Self {
            owner: OwnerRule::EffectiveUser,
            exact_mode: Some(0o700),
            private_mode: true,
            reject_group_world_write: true,
        }
    }

    /// Root- or effective-user-owned, without group/world write access.
    pub const fn trusted() -> Self {
        Self {
            owner: OwnerRule::RootOrEffectiveUser,
            exact_mode: None,
            private_mode: false,
            reject_group_world_write: true,
        }
    }
}

/// Validation policy for a regular file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FilePolicy {
    owner: OwnerRule,
    max_len: u64,
    exact_len: Option<u64>,
    require_single_link: bool,
    private_mode: bool,
}

impl FilePolicy {
    /// Effective-user-owned, single-link, no permissions outside `0600`, and bounded in size.
    pub const fn private(max_len: u64) -> Self {
        Self {
            owner: OwnerRule::EffectiveUser,
            max_len,
            exact_len: None,
            require_single_link: true,
            private_mode: true,
        }
    }

    /// The private policy with an exact expected byte length.
    pub const fn private_exact(len: u64) -> Self {
        Self {
            owner: OwnerRule::EffectiveUser,
            max_len: len,
            exact_len: Some(len),
            require_single_link: true,
            private_mode: true,
        }
    }

    /// Root- or effective-user-owned regular file, without group/world write access.
    pub const fn trusted(max_len: u64) -> Self {
        Self {
            owner: OwnerRule::RootOrEffectiveUser,
            max_len,
            exact_len: None,
            require_single_link: false,
            private_mode: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ObjectIdentity {
    pub device: u64,
    pub inode: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    RegularFile,
    Directory,
    Symlink,
    Fifo,
    Socket,
    CharacterDevice,
    BlockDevice,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntryMetadata {
    pub identity: ObjectIdentity,
    pub kind: EntryKind,
    pub uid: u32,
    pub mode: u32,
    pub links: u64,
    pub len: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryEntry {
    pub name: LeafName,
    pub metadata: EntryMetadata,
}

/// Opaque, constant-memory directory stream that retains the validated directory guard.
pub struct DirectoryEntries {
    directory: GuardedDir,
    stream: Dir,
    limit: usize,
    emitted: usize,
    finished: bool,
}

impl fmt::Debug for DirectoryEntries {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DirectoryEntries")
            .field("directory", &self.directory)
            .field("limit", &self.limit)
            .field("emitted", &self.emitted)
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

impl Iterator for DirectoryEntries {
    type Item = Result<DirectoryEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        loop {
            let raw_entry = match self.stream.next() {
                Some(Ok(entry)) => entry,
                Some(Err(error)) => {
                    self.finished = true;
                    return Some(Err(errno(error)));
                }
                None => {
                    self.finished = true;
                    return None;
                }
            };
            let bytes = raw_entry.file_name().to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            if self.emitted == self.limit {
                self.finished = true;
                return Some(Err(CustodyError::LimitExceeded(
                    "directory contains more entries than allowed",
                )));
            }
            let name = match LeafName::new(OsString::from_vec(bytes.to_vec())) {
                Ok(name) => name,
                Err(error) => {
                    self.finished = true;
                    return Some(Err(error));
                }
            };
            let metadata = match self.directory.entry_metadata(&name) {
                Ok(Some(metadata)) => metadata,
                Ok(None) => {
                    self.finished = true;
                    return Some(Err(CustodyError::NotFound));
                }
                Err(error) => {
                    self.finished = true;
                    return Some(Err(error));
                }
            };
            self.emitted += 1;
            return Some(Ok(DirectoryEntry { name, metadata }));
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.limit.saturating_sub(self.emitted);
        (0, remaining.checked_add(1))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenAccess {
    ReadOnly,
    ReadWrite,
}

struct GuardedDirInner {
    file: File,
    path: NormalizedPath,
    identity: ObjectIdentity,
    policy: DirPolicy,
    ancestors: AncestorPolicy,
    created: bool,
}

/// A retained directory descriptor whose identity and policy were validated at open time.
#[derive(Clone)]
pub struct GuardedDir {
    inner: Arc<GuardedDirInner>,
}

impl fmt::Debug for GuardedDir {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GuardedDir")
            .field("identity", &self.inner.identity)
            .field("created", &self.inner.created)
            .finish_non_exhaustive()
    }
}

impl AsFd for GuardedDir {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.inner.file.as_fd()
    }
}

impl GuardedDir {
    pub fn open_existing(path: impl AsRef<Path>, policy: DirPolicy) -> Result<Self> {
        Self::open_existing_with(path, policy, AncestorPolicy::PlatformDefault)
    }

    pub fn open_existing_with(
        path: impl AsRef<Path>,
        policy: DirPolicy,
        ancestors: AncestorPolicy,
    ) -> Result<Self> {
        let path = NormalizedPath::new(path)?;
        let (file, identity, resolved, created) = walk_directory(path, policy, ancestors, false)?;
        Ok(Self::from_parts(
            file, identity, resolved, policy, ancestors, created,
        ))
    }

    /// Creates every missing component as `0700`; existing objects are validated and never
    /// repaired or silently tightened.
    pub fn create_private(path: impl AsRef<Path>) -> Result<Self> {
        Self::create_private_with(path, AncestorPolicy::PlatformDefault)
    }

    pub fn create_private_with(path: impl AsRef<Path>, ancestors: AncestorPolicy) -> Result<Self> {
        let path = NormalizedPath::new(path)?;
        let policy = DirPolicy::private_mutable();
        let (file, identity, resolved, created) = walk_directory(path, policy, ancestors, true)?;
        Ok(Self::from_parts(
            file, identity, resolved, policy, ancestors, created,
        ))
    }

    fn from_parts(
        file: File,
        identity: ObjectIdentity,
        path: NormalizedPath,
        policy: DirPolicy,
        ancestors: AncestorPolicy,
        created: bool,
    ) -> Self {
        Self {
            inner: Arc::new(GuardedDirInner {
                file,
                path,
                identity,
                policy,
                ancestors,
                created,
            }),
        }
    }

    pub fn absolute_path(&self) -> &Path {
        self.inner.path.as_path()
    }

    pub fn identity(&self) -> ObjectIdentity {
        self.inner.identity
    }

    pub fn was_created(&self) -> bool {
        self.inner.created
    }

    pub fn sync(&self) -> Result<()> {
        self.verify_live()?;
        syscall(fs::fsync(&self.inner.file))
    }

    pub fn verify_live(&self) -> Result<EntryMetadata> {
        let stat = syscall(fs::fstat(&self.inner.file))?;
        let metadata = metadata_from_stat(&stat)?;
        if metadata.identity != self.inner.identity {
            return Err(CustodyError::UnsafeDirectory(
                "open directory identity changed",
            ));
        }
        validate_directory(&self.inner.file, &stat, self.inner.policy)?;
        Ok(metadata)
    }

    /// Re-walks the stored absolute name and proves it still names this retained descriptor.
    pub fn verify_named(&self) -> Result<()> {
        let (file, identity, _, _) = walk_directory(
            self.inner.path.clone(),
            self.inner.policy,
            self.inner.ancestors,
            false,
        )?;
        if identity != self.inner.identity {
            return Err(CustodyError::UnsafeDirectory(
                "directory name no longer identifies retained object",
            ));
        }
        drop(file);
        Ok(())
    }

    pub fn entry_metadata(&self, name: &LeafName) -> Result<Option<EntryMetadata>> {
        self.verify_live()?;
        match fs::statat(
            &self.inner.file,
            name.as_os_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Ok(stat) => metadata_from_stat(&stat).map(Some),
            Err(Errno::NOENT) => Ok(None),
            Err(error) => Err(errno(error)),
        }
    }

    /// Streams and lstat-validates at most `limit` entries without joining paths or accumulating
    /// names in memory. The iterator retains this directory guard for its complete lifetime.
    pub fn entries_bounded(&self, limit: usize) -> Result<DirectoryEntries> {
        if limit > MAX_DIRECTORY_ENTRIES {
            return Err(CustodyError::LimitExceeded(
                "directory streaming limit is too large",
            ));
        }
        self.verify_live()?;
        let stream = syscall(Dir::read_from(&self.inner.file))?;
        Ok(DirectoryEntries {
            directory: self.clone(),
            stream,
            limit,
            emitted: 0,
            finished: false,
        })
    }

    /// Collects [`Self::entries_bounded`] when a caller explicitly needs an in-memory snapshot.
    pub fn list_bounded(&self, limit: usize) -> Result<Vec<DirectoryEntry>> {
        self.entries_bounded(limit)?.collect()
    }

    pub fn validate_file(
        &self,
        name: &LeafName,
        policy: FilePolicy,
    ) -> Result<Option<EntryMetadata>> {
        self.open_file_optional(name, OpenAccess::ReadOnly, policy)
            .map(|file| file.map(|file| file.opened_metadata()))
    }

    pub fn open_file(
        &self,
        name: &LeafName,
        access: OpenAccess,
        policy: FilePolicy,
    ) -> Result<GuardedFile> {
        self.open_file_optional(name, access, policy)?
            .ok_or(CustodyError::NotFound)
    }

    pub fn open_file_optional(
        &self,
        name: &LeafName,
        access: OpenAccess,
        policy: FilePolicy,
    ) -> Result<Option<GuardedFile>> {
        self.verify_live()?;
        let access_flag = match access {
            OpenAccess::ReadOnly => OFlags::RDONLY,
            OpenAccess::ReadWrite => OFlags::RDWR,
        };
        let flags =
            access_flag | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY;
        let fd = match fs::openat(&self.inner.file, name.as_os_str(), flags, Mode::empty()) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return Ok(None),
            Err(error) => return Err(errno(error)),
        };
        let file = File::from(fd);
        let stat = syscall(fs::fstat(&file))?;
        validate_file(&file, &stat, policy)?;
        let metadata = metadata_from_stat(&stat)?;
        Ok(Some(GuardedFile::from_parts(
            self.clone(),
            file,
            name.clone(),
            metadata,
            policy,
            false,
        )))
    }

    /// Exclusively creates a private `0600` regular file and syncs both file and directory.
    pub fn create_file(&self, name: &LeafName, policy: FilePolicy) -> Result<GuardedFile> {
        self.verify_live()?;
        let flags = OFlags::RDWR
            | OFlags::CREATE
            | OFlags::EXCL
            | OFlags::CLOEXEC
            | OFlags::NOFOLLOW
            | OFlags::NONBLOCK
            | OFlags::NOCTTY;
        let fd = match fs::openat(
            &self.inner.file,
            name.as_os_str(),
            flags,
            Mode::from_raw_mode(0o600),
        ) {
            Ok(fd) => fd,
            Err(Errno::EXIST) => return Err(CustodyError::AlreadyExists),
            Err(error) => return Err(errno(error)),
        };
        let file = File::from(fd);
        let initial_stat = syscall(fs::fstat(&file))?;
        let identity = identity_from_stat(&initial_stat);

        let result = (|| {
            syscall(fs::fchmod(&file, Mode::from_raw_mode(0o600)))?;
            let stat = syscall(fs::fstat(&file))?;
            validate_file(&file, &stat, policy)?;
            syscall(fs::fsync(&file))?;
            syscall(fs::fsync(&self.inner.file))?;
            let metadata = metadata_from_stat(&stat)?;
            Ok(GuardedFile::from_parts(
                self.clone(),
                file,
                name.clone(),
                metadata,
                policy,
                true,
            ))
        })();

        if result.is_err() {
            unlink_if_identity(&self.inner.file, name, identity, false);
        }
        result
    }

    pub fn open_or_create_file(
        &self,
        name: &LeafName,
        access: OpenAccess,
        policy: FilePolicy,
    ) -> Result<GuardedFile> {
        match self.open_file_optional(name, access, policy)? {
            Some(file) => Ok(file),
            None => match self.create_file(name, policy) {
                Ok(file) if access == OpenAccess::ReadOnly => {
                    let identity = file.identity();
                    let mut reopened = self.open_file(name, OpenAccess::ReadOnly, policy)?;
                    if reopened.identity() != identity {
                        return Err(CustodyError::UnsafeFile(
                            "new file changed identity while reopening read-only",
                        ));
                    }
                    reopened.created = true;
                    Ok(reopened)
                }
                Ok(file) => Ok(file),
                Err(CustodyError::AlreadyExists) => self.open_file(name, access, policy),
                Err(error) => Err(error),
            },
        }
    }

    /// Unlinks only the still-named, still-policy-compliant file represented by `file`.
    pub fn unlink_file(&self, file: GuardedFile) -> Result<()> {
        self.require_child(&file)?;
        file.verify_named_in_parent()?;
        syscall(fs::unlinkat(
            &self.inner.file,
            file.name.as_os_str(),
            AtFlags::empty(),
        ))?;
        syscall(fs::fsync(&self.inner.file))?;
        let stat = syscall(fs::fstat(&file.file))?;
        if file.policy.require_single_link && stat.st_nlink != 0 {
            return Err(CustodyError::UnsafeFile(
                "unlinked private file retained unexpected links",
            ));
        }
        Ok(())
    }

    /// Atomically replaces a destination after validating any existing object under the source
    /// file's policy.
    pub fn rename_replace(
        &self,
        file: GuardedFile,
        destination: &GuardedDir,
        destination_name: &LeafName,
    ) -> Result<GuardedFile> {
        self.require_child(&file)?;
        file.verify_named_in_parent()?;
        file.sync_all()?;
        destination.verify_live()?;
        if destination
            .open_file_optional(destination_name, OpenAccess::ReadOnly, file.policy)?
            .is_none()
        {
            // Absence is allowed. Any non-regular or unsafe existing entry fails in `open_file`.
        }
        syscall(fs::renameat(
            &self.inner.file,
            file.name.as_os_str(),
            &destination.inner.file,
            destination_name.as_os_str(),
        ))?;
        sync_after_rename(self, destination)?;
        Ok(file.reparent(destination.clone(), destination_name.clone()))
    }

    /// Atomically publishes without replacing an existing destination where the platform exposes
    /// a kernel no-replace rename primitive.
    pub fn publish_no_replace(
        &self,
        file: GuardedFile,
        destination: &GuardedDir,
        destination_name: &LeafName,
    ) -> Result<GuardedFile> {
        self.require_child(&file)?;
        file.verify_named_in_parent()?;
        file.sync_all()?;
        destination.verify_live()?;

        #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
        {
            match fs::renameat_with(
                &self.inner.file,
                file.name.as_os_str(),
                &destination.inner.file,
                destination_name.as_os_str(),
                RenameFlags::NOREPLACE,
            ) {
                Ok(()) => {}
                Err(Errno::EXIST) => return Err(CustodyError::AlreadyExists),
                Err(error) => return Err(errno(error)),
            }
            sync_after_rename(self, destination)?;
            Ok(file.reparent(destination.clone(), destination_name.clone()))
        }

        #[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
        {
            let _ = (file, destination, destination_name);
            Err(CustodyError::Unsupported(
                "atomic no-replace rename is unavailable",
            ))
        }
    }

    fn require_child(&self, file: &GuardedFile) -> Result<()> {
        if !Arc::ptr_eq(&self.inner, &file.parent.inner) {
            return Err(CustodyError::UnsafeFile(
                "file does not belong to this retained directory",
            ));
        }
        Ok(())
    }
}

/// A retained regular-file descriptor tied to a retained parent directory and leaf name.
pub struct GuardedFile {
    parent: GuardedDir,
    file: File,
    name: LeafName,
    metadata: EntryMetadata,
    policy: FilePolicy,
    created: bool,
}

impl fmt::Debug for GuardedFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GuardedFile")
            .field("name", &self.name)
            .field("identity", &self.metadata.identity)
            .field("created", &self.created)
            .finish_non_exhaustive()
    }
}

impl AsFd for GuardedFile {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

impl GuardedFile {
    fn from_parts(
        parent: GuardedDir,
        file: File,
        name: LeafName,
        metadata: EntryMetadata,
        policy: FilePolicy,
        created: bool,
    ) -> Self {
        Self {
            parent,
            file,
            name,
            metadata,
            policy,
            created,
        }
    }

    fn reparent(mut self, parent: GuardedDir, name: LeafName) -> Self {
        self.parent = parent;
        self.name = name;
        self
    }

    pub fn parent(&self) -> &GuardedDir {
        &self.parent
    }

    pub fn name(&self) -> &LeafName {
        &self.name
    }

    pub fn identity(&self) -> ObjectIdentity {
        self.metadata.identity
    }

    /// Returns the metadata snapshot validated when this descriptor was opened.
    pub fn opened_metadata(&self) -> EntryMetadata {
        self.metadata
    }

    /// Revalidates the live descriptor and returns current metadata.
    pub fn metadata(&self) -> Result<EntryMetadata> {
        self.verify_live()
    }

    pub fn was_created(&self) -> bool {
        self.created
    }

    pub fn file(&self) -> &File {
        &self.file
    }

    pub fn verify_live(&self) -> Result<EntryMetadata> {
        let stat = syscall(fs::fstat(&self.file))?;
        validate_file(&self.file, &stat, self.policy)?;
        let metadata = metadata_from_stat(&stat)?;
        if metadata.identity != self.metadata.identity {
            return Err(CustodyError::UnsafeFile("open file identity changed"));
        }
        Ok(metadata)
    }

    pub fn verify_named(&self) -> Result<()> {
        self.parent.verify_named()?;
        self.verify_named_in_parent()
    }

    fn verify_named_in_parent(&self) -> Result<()> {
        self.parent.verify_live()?;
        self.verify_live()?;
        let stat = match fs::statat(
            &self.parent.inner.file,
            self.name.as_os_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Ok(stat) => stat,
            Err(Errno::NOENT) => return Err(CustodyError::NotFound),
            Err(error) => return Err(errno(error)),
        };
        validate_file(&self.file, &stat, self.policy)?;
        if identity_from_stat(&stat) != self.metadata.identity {
            return Err(CustodyError::UnsafeFile(
                "file name no longer identifies retained object",
            ));
        }
        Ok(())
    }

    pub fn sync_all(&self) -> Result<()> {
        self.verify_live()?;
        syscall(fs::fsync(&self.file))
    }
}

impl Read for GuardedFile {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.file.read(buffer)
    }
}

impl Write for GuardedFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.file.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Seek for GuardedFile {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.file.seek(position)
    }
}

struct CleanupDirectory {
    parent: File,
    name: LeafName,
    identity: ObjectIdentity,
}

fn walk_directory(
    path: NormalizedPath,
    final_policy: DirPolicy,
    ancestors: AncestorPolicy,
    create_private: bool,
) -> Result<(File, ObjectIdentity, NormalizedPath, bool)> {
    let root_fd = syscall(fs::open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    ))?;
    let root = File::from(root_fd);
    let root_stat = syscall(fs::fstat(&root))?;
    validate_ancestor(&root, &root_stat)?;

    let resolved = resolve_platform_alias(&root, path, ancestors)?;
    let mut cleanup = Vec::new();
    let result = (|| {
        let mut current = root;
        let mut final_created = false;

        if resolved.components.is_empty() {
            let stat = syscall(fs::fstat(&current))?;
            validate_directory(&current, &stat, final_policy)?;
            return Ok((current, identity_from_stat(&stat), resolved.clone(), false));
        }

        for (index, raw_name) in resolved.components.iter().enumerate() {
            let is_final = index + 1 == resolved.components.len();
            let name = LeafName::new(raw_name)?;
            let mut created_here = false;
            let mut raced_here = false;
            let fd = match open_directory_component(&current, &name) {
                Ok(fd) => fd,
                Err(CustodyError::NotFound) if create_private => {
                    run_before_mkdir_hook(&current, name.as_os_str());
                    match fs::mkdirat(&current, name.as_os_str(), Mode::from_raw_mode(0o700)) {
                        Ok(()) => created_here = true,
                        Err(Errno::EXIST) => raced_here = true,
                        Err(error) => return Err(errno(error)),
                    }
                    open_directory_component(&current, &name)?
                }
                Err(error) => return Err(error),
            };
            let next = File::from(fd);
            let initial_stat = syscall(fs::fstat(&next))?;

            if created_here {
                let identity = identity_from_stat(&initial_stat);
                cleanup.push(CleanupDirectory {
                    parent: current.try_clone()?,
                    name: name.clone(),
                    identity,
                });
                syscall(fs::fchmod(&next, Mode::from_raw_mode(0o700)))?;
                let stat = syscall(fs::fstat(&next))?;
                validate_directory(&next, &stat, DirPolicy::private_mutable())?;
                syscall(fs::fsync(&next))?;
                syscall(fs::fsync(&current))?;
            } else if raced_here {
                validate_directory(&next, &initial_stat, DirPolicy::private_mutable())?;
            } else if is_final {
                validate_directory(&next, &initial_stat, final_policy)?;
            } else {
                validate_ancestor(&next, &initial_stat)?;
            }

            if is_final {
                final_created = created_here;
            }
            current = next;
        }

        let stat = syscall(fs::fstat(&current))?;
        validate_directory(&current, &stat, final_policy)?;
        Ok((
            current,
            identity_from_stat(&stat),
            resolved.clone(),
            final_created,
        ))
    })();

    if result.is_err() {
        cleanup_created_directories(&cleanup);
    }
    result
}

fn open_directory_component(parent: &File, name: &LeafName) -> Result<std::os::fd::OwnedFd> {
    match fs::openat(
        parent,
        name.as_os_str(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(fd) => Ok(fd),
        Err(Errno::NOENT) => Err(CustodyError::NotFound),
        Err(error) => Err(errno(error)),
    }
}

fn permission_bits<T: Into<u64>>(mode: T) -> u32 {
    (mode.into() & 0o7777) as u32
}

fn validate_ancestor(file: &File, stat: &Stat) -> Result<()> {
    if !FileType::from_raw_mode(stat.st_mode).is_dir() {
        return Err(CustodyError::UnsafeAncestor("component is not a directory"));
    }
    let euid = geteuid().as_raw();
    if stat.st_uid != 0 && stat.st_uid != euid {
        return Err(CustodyError::UnsafeAncestor(
            "component has an untrusted owner",
        ));
    }
    let mode = permission_bits(stat.st_mode);
    if mode & 0o022 != 0 && !(stat.st_uid == 0 && mode & 0o1000 != 0) {
        return Err(CustodyError::UnsafeAncestor(
            "component is mutable by another user",
        ));
    }
    validate_acl(file)
        .map_err(|_| CustodyError::UnsafeAncestor("component ACL grants additional access"))
}

fn validate_directory(file: &File, stat: &Stat, policy: DirPolicy) -> Result<()> {
    if !FileType::from_raw_mode(stat.st_mode).is_dir() {
        return Err(CustodyError::UnsafeDirectory("object is not a directory"));
    }
    validate_owner(stat.st_uid, policy.owner)
        .map_err(|_| CustodyError::UnsafeDirectory("directory has an untrusted owner"))?;
    let mode = permission_bits(stat.st_mode);
    if let Some(expected) = policy.exact_mode {
        if mode != expected {
            return Err(CustodyError::UnsafeDirectory("directory mode is not exact"));
        }
    } else if policy.private_mode && mode & !0o700 != 0 {
        return Err(CustodyError::UnsafeDirectory(
            "directory grants permissions outside the owner bits",
        ));
    }
    if policy.reject_group_world_write && mode & 0o022 != 0 {
        return Err(CustodyError::UnsafeDirectory(
            "directory is group/world writable",
        ));
    }
    validate_acl(file)
        .map_err(|_| CustodyError::UnsafeDirectory("directory ACL grants additional access"))
}

fn validate_file(file: &File, stat: &Stat, policy: FilePolicy) -> Result<()> {
    if !FileType::from_raw_mode(stat.st_mode).is_file() {
        return Err(CustodyError::UnsafeFile("object is not a regular file"));
    }
    validate_owner(stat.st_uid, policy.owner)
        .map_err(|_| CustodyError::UnsafeFile("file has an untrusted owner"))?;
    let mode = permission_bits(stat.st_mode);
    if policy.private_mode {
        if mode & !0o600 != 0 {
            return Err(CustodyError::UnsafeFile(
                "file grants permissions outside private read/write",
            ));
        }
    } else if mode & 0o022 != 0 {
        return Err(CustodyError::UnsafeFile("file is group/world writable"));
    }
    if policy.require_single_link && stat.st_nlink != 1 {
        return Err(CustodyError::UnsafeFile(
            "private file does not have exactly one link",
        ));
    }
    let len = u64::try_from(stat.st_size)
        .map_err(|_| CustodyError::UnsafeFile("file length is invalid"))?;
    if len > policy.max_len {
        return Err(CustodyError::LimitExceeded("file is too large"));
    }
    if policy.exact_len.is_some_and(|expected| len != expected) {
        return Err(CustodyError::UnsafeFile(
            "file length does not match the required length",
        ));
    }
    validate_acl(file).map_err(|_| CustodyError::UnsafeFile("file ACL grants additional access"))
}

fn validate_owner(uid: u32, rule: OwnerRule) -> std::result::Result<(), ()> {
    let euid = geteuid().as_raw();
    match rule {
        OwnerRule::EffectiveUser if uid == euid => Ok(()),
        OwnerRule::RootOrEffectiveUser if uid == 0 || uid == euid => Ok(()),
        _ => Err(()),
    }
}

fn metadata_from_stat(stat: &Stat) -> Result<EntryMetadata> {
    let file_type = FileType::from_raw_mode(stat.st_mode);
    let kind = if file_type.is_file() {
        EntryKind::RegularFile
    } else if file_type.is_dir() {
        EntryKind::Directory
    } else if file_type.is_symlink() {
        EntryKind::Symlink
    } else if file_type.is_fifo() {
        EntryKind::Fifo
    } else if file_type.is_socket() {
        EntryKind::Socket
    } else if file_type.is_char_device() {
        EntryKind::CharacterDevice
    } else if file_type.is_block_device() {
        EntryKind::BlockDevice
    } else {
        EntryKind::Other
    };
    let len = u64::try_from(stat.st_size)
        .map_err(|_| CustodyError::UnsafeFile("object length is invalid"))?;
    Ok(EntryMetadata {
        identity: identity_from_stat(stat),
        kind,
        uid: stat.st_uid,
        mode: permission_bits(stat.st_mode),
        links: stat_links(stat),
        len,
    })
}

fn identity_from_stat(stat: &Stat) -> ObjectIdentity {
    ObjectIdentity {
        device: stat_device(stat),
        inode: stat.st_ino,
    }
}

#[cfg(target_vendor = "apple")]
fn stat_device(stat: &Stat) -> u64 {
    u64::from(u32::from_ne_bytes(stat.st_dev.to_ne_bytes()))
}

#[cfg(not(target_vendor = "apple"))]
fn stat_device(stat: &Stat) -> u64 {
    stat.st_dev
}

#[cfg(target_vendor = "apple")]
fn stat_links(stat: &Stat) -> u64 {
    u64::from(stat.st_nlink)
}

#[cfg(not(target_vendor = "apple"))]
fn stat_links(stat: &Stat) -> u64 {
    stat.st_nlink
}

fn cleanup_created_directories(cleanup: &[CleanupDirectory]) {
    for directory in cleanup.iter().rev() {
        unlink_if_identity(&directory.parent, &directory.name, directory.identity, true);
    }
}

fn unlink_if_identity(parent: &File, name: &LeafName, expected: ObjectIdentity, directory: bool) {
    let Ok(stat) = fs::statat(parent, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW) else {
        return;
    };
    if identity_from_stat(&stat) != expected {
        return;
    }
    let flags = if directory {
        AtFlags::REMOVEDIR
    } else {
        AtFlags::empty()
    };
    let _ = fs::unlinkat(parent, name.as_os_str(), flags);
    let _ = fs::fsync(parent);
}

fn sync_after_rename(source: &GuardedDir, destination: &GuardedDir) -> Result<()> {
    syscall(fs::fsync(&source.inner.file))?;
    if source.identity() != destination.identity() {
        syscall(fs::fsync(&destination.inner.file))?;
    }
    Ok(())
}

fn syscall<T>(result: rustix::io::Result<T>) -> Result<T> {
    result.map_err(errno)
}

fn errno(error: Errno) -> CustodyError {
    CustodyError::Io(io::Error::from_raw_os_error(error.raw_os_error()))
}

#[cfg(not(target_os = "macos"))]
fn validate_acl(_file: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn validate_acl(file: &File) -> io::Result<()> {
    crate::macos_acl::validate_no_extended_allow(file)
}

#[cfg(not(target_os = "macos"))]
fn resolve_platform_alias(
    _root: &File,
    path: NormalizedPath,
    _policy: AncestorPolicy,
) -> Result<NormalizedPath> {
    Ok(path)
}

#[cfg(target_os = "macos")]
fn resolve_platform_alias(
    root: &File,
    path: NormalizedPath,
    policy: AncestorPolicy,
) -> Result<NormalizedPath> {
    let Some(first) = path.components.first() else {
        return Ok(path);
    };
    let Some(expected_target) = macos_alias_target(first.as_bytes()) else {
        return Ok(path);
    };
    if policy == AncestorPolicy::NoSymlinks {
        return Err(CustodyError::UnsafeAncestor(
            "platform alias is disabled by policy",
        ));
    }

    let before = syscall(fs::statat(root, first, AtFlags::SYMLINK_NOFOLLOW))?;
    if !FileType::from_raw_mode(before.st_mode).is_symlink() || before.st_uid != 0 {
        return Err(CustodyError::UnsafeAncestor(
            "platform alias is not a root-owned symlink",
        ));
    }
    let target = syscall(fs::readlinkat(root, first, Vec::new()))?;
    if target.to_bytes() != expected_target {
        return Err(CustodyError::UnsafeAncestor(
            "platform alias target is not exact",
        ));
    }

    let private_name = LeafName::new("private")?;
    let private = File::from(open_directory_component(root, &private_name)?);
    let private_stat = syscall(fs::fstat(&private))?;
    validate_ancestor(&private, &private_stat)?;
    let target_name = LeafName::new(first)?;
    let target_directory = File::from(open_directory_component(&private, &target_name)?);
    let target_stat = syscall(fs::fstat(&target_directory))?;
    validate_ancestor(&target_directory, &target_stat)?;

    let followed = syscall(fs::statat(root, first, AtFlags::empty()))?;
    if identity_from_stat(&followed) != identity_from_stat(&target_stat) {
        return Err(CustodyError::UnsafeAncestor(
            "platform alias does not identify its expected target",
        ));
    }
    let after = syscall(fs::statat(root, first, AtFlags::SYMLINK_NOFOLLOW))?;
    let target_after = syscall(fs::readlinkat(root, first, Vec::new()))?;
    if identity_from_stat(&before) != identity_from_stat(&after)
        || target_after.to_bytes() != expected_target
    {
        return Err(CustodyError::UnsafeAncestor(
            "platform alias changed during validation",
        ));
    }

    let mut components = Vec::with_capacity(path.components.len() + 1);
    components.push(OsString::from("private"));
    components.extend(path.components);
    NormalizedPath::from_components(components)
}

#[cfg(target_os = "macos")]
fn macos_alias_target(component: &[u8]) -> Option<&'static [u8]> {
    match component {
        b"var" => Some(b"private/var"),
        b"tmp" => Some(b"private/tmp"),
        b"etc" => Some(b"private/etc"),
        _ => None,
    }
}

#[cfg(test)]
type BeforeMkdirHook = Box<dyn FnOnce(&File, &OsStr)>;

#[cfg(test)]
thread_local! {
    static BEFORE_MKDIR_HOOK: std::cell::RefCell<Option<BeforeMkdirHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn install_before_mkdir_hook(hook: BeforeMkdirHook) {
    BEFORE_MKDIR_HOOK.with(|slot| *slot.borrow_mut() = Some(hook));
}

#[cfg(test)]
fn run_before_mkdir_hook(parent: &File, name: &OsStr) {
    BEFORE_MKDIR_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook(parent, name);
        }
    });
}

#[cfg(not(test))]
fn run_before_mkdir_hook(_parent: &File, _name: &OsStr) {}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::fs as rustix_fs;
    use std::fs as std_fs;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::sync::{Arc, Barrier};
    use std::thread;

    const TEST_MAX_FILE: u64 = 1024 * 1024;

    fn vault() -> (tempfile::TempDir, GuardedDir) {
        let temporary = tempfile::tempdir().expect("tempdir");
        let guarded = GuardedDir::create_private(temporary.path().join("vault"))
            .expect("create guarded directory");
        (temporary, guarded)
    }

    #[test]
    fn lexical_rejection_happens_before_side_effects() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let target = temporary.path().join("created").join("..").join("escape");
        let error = GuardedDir::create_private(&target).expect_err("parent component must fail");
        assert!(matches!(error, CustodyError::InvalidPath(_)));
        assert!(!temporary.path().join("created").exists());
    }

    #[test]
    fn ancestor_symlink_is_rejected() {
        let temporary = tempfile::tempdir().expect("tempdir");
        std_fs::create_dir(temporary.path().join("actual")).expect("actual directory");
        std_fs::create_dir(temporary.path().join("actual/child")).expect("child directory");
        std_fs::set_permissions(
            temporary.path().join("actual/child"),
            std_fs::Permissions::from_mode(0o700),
        )
        .expect("chmod child");
        symlink("actual", temporary.path().join("alias")).expect("symlink");

        let error = GuardedDir::open_existing(
            temporary.path().join("alias/child"),
            DirPolicy::private_mutable(),
        )
        .expect_err("symlink ancestor must fail");
        assert!(matches!(
            error,
            CustodyError::Io(_) | CustodyError::UnsafeAncestor(_)
        ));
    }

    #[test]
    fn mutable_and_user_owned_sticky_ancestors_are_rejected() {
        let temporary = tempfile::tempdir().expect("tempdir");
        for (name, mode) in [("mutable", 0o777), ("sticky", 0o1777)] {
            let ancestor = temporary.path().join(name);
            std_fs::create_dir(&ancestor).expect("ancestor");
            std_fs::set_permissions(&ancestor, std_fs::Permissions::from_mode(mode))
                .expect("chmod");
            let child = ancestor.join("child");
            std_fs::create_dir(&child).expect("child");
            std_fs::set_permissions(&child, std_fs::Permissions::from_mode(0o700))
                .expect("chmod child");
            let error = GuardedDir::open_existing(&child, DirPolicy::private_mutable())
                .expect_err("unsafe ancestor must fail");
            assert!(matches!(error, CustodyError::UnsafeAncestor(_)));
        }
    }

    #[test]
    fn trusted_directory_allows_public_read_but_rejects_public_write() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let trusted = temporary.path().join("trusted");
        std_fs::create_dir(&trusted).expect("trusted directory");
        std_fs::set_permissions(&trusted, std_fs::Permissions::from_mode(0o755))
            .expect("chmod trusted");
        GuardedDir::open_existing(&trusted, DirPolicy::trusted())
            .expect("trusted policy allows group/world read and traversal");

        std_fs::set_permissions(&trusted, std_fs::Permissions::from_mode(0o775))
            .expect("make trusted directory group writable");
        assert!(matches!(
            GuardedDir::open_existing(&trusted, DirPolicy::trusted()),
            Err(CustodyError::UnsafeDirectory(_))
        ));
    }

    #[test]
    fn root_owned_sticky_temporary_ancestor_is_accepted() {
        let unique = format!(
            "pigeonpost-unix-custody-{}-{}",
            std::process::id(),
            thread::current().name().unwrap_or("test")
        );
        let target = Path::new("/tmp").join(unique);
        let guard = GuardedDir::create_private(&target).expect("root sticky /tmp is trusted");
        assert_eq!(guard.verify_live().expect("live").mode, 0o700);
        drop(guard);
        std_fs::remove_dir(&target).expect("cleanup");
    }

    #[test]
    fn losing_mkdir_race_accepts_only_an_exact_private_winner() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let good = temporary.path().join("winner");
        install_before_mkdir_hook(Box::new(|parent, name| {
            rustix_fs::mkdirat(parent, name, Mode::from_raw_mode(0o700)).expect("racing mkdir");
        }));
        GuardedDir::create_private(&good).expect("safe race winner");

        let bad = temporary.path().join("unsafe-winner");
        install_before_mkdir_hook(Box::new(|parent, name| {
            rustix_fs::mkdirat(parent, name, Mode::from_raw_mode(0o700)).expect("racing mkdir");
            let fd = open_directory_component(parent, &LeafName::new(name).expect("leaf"))
                .expect("open winner");
            rustix_fs::fchmod(&fd, Mode::from_raw_mode(0o755)).expect("make winner unsafe");
        }));
        let error = GuardedDir::create_private(&bad).expect_err("unsafe winner must fail");
        assert!(matches!(error, CustodyError::UnsafeDirectory(_)));
        assert_eq!(
            std_fs::metadata(&bad)
                .expect("winner remains")
                .permissions()
                .mode()
                & 0o777,
            0o755,
            "an existing race winner must never be tightened"
        );

        let bad_ancestor = temporary.path().join("unsafe-ancestor/child");
        install_before_mkdir_hook(Box::new(|parent, name| {
            rustix_fs::mkdirat(parent, name, Mode::from_raw_mode(0o700))
                .expect("racing ancestor mkdir");
            let fd = open_directory_component(parent, &LeafName::new(name).expect("leaf"))
                .expect("open ancestor winner");
            rustix_fs::fchmod(&fd, Mode::from_raw_mode(0o755))
                .expect("make ancestor winner unsafe");
        }));
        assert!(matches!(
            GuardedDir::create_private(&bad_ancestor),
            Err(CustodyError::UnsafeDirectory(_))
        ));
        assert_eq!(
            std_fs::metadata(temporary.path().join("unsafe-ancestor"))
                .expect("ancestor winner remains")
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }

    #[test]
    fn retained_guard_never_redirects_to_name_replacement() {
        let (temporary, guard) = vault();
        let original = temporary.path().join("vault");
        let renamed = temporary.path().join("renamed");
        std_fs::rename(&original, &renamed).expect("rename guarded directory");
        std_fs::create_dir(&original).expect("replacement");
        std_fs::set_permissions(&original, std_fs::Permissions::from_mode(0o700)).expect("chmod");

        let marker = LeafName::new("marker").expect("leaf");
        guard
            .create_file(&marker, FilePolicy::private(TEST_MAX_FILE))
            .expect("fd-relative create targets original object");
        assert!(renamed.join("marker").is_file());
        assert!(!original.join("marker").exists());
        assert!(guard.verify_named().is_err());
    }

    #[test]
    fn fifo_and_hard_link_are_rejected() {
        let (temporary, guard) = vault();
        let fifo = LeafName::new("pipe").expect("leaf");
        let status = std::process::Command::new("mkfifo")
            .arg(temporary.path().join("vault/pipe"))
            .status()
            .expect("run mkfifo");
        assert!(status.success());
        let error = guard
            .open_file(
                &fifo,
                OpenAccess::ReadOnly,
                FilePolicy::private(TEST_MAX_FILE),
            )
            .expect_err("fifo must fail without blocking");
        assert!(matches!(error, CustodyError::UnsafeFile(_)));

        let linked = LeafName::new("linked").expect("leaf");
        guard
            .create_file(&linked, FilePolicy::private(TEST_MAX_FILE))
            .expect("file");
        std_fs::hard_link(
            temporary.path().join("vault/linked"),
            temporary.path().join("vault/second-link"),
        )
        .expect("hard link");
        let error = guard
            .open_file(
                &linked,
                OpenAccess::ReadOnly,
                FilePolicy::private(TEST_MAX_FILE),
            )
            .expect_err("multiple links must fail");
        assert!(matches!(error, CustodyError::UnsafeFile(_)));
    }

    #[test]
    fn concurrent_no_replace_publication_has_one_winner() {
        let (_temporary, guard) = vault();
        let writers = 8;
        let barrier = Arc::new(Barrier::new(writers));
        let mut handles = Vec::new();

        for index in 0..writers {
            let guard = guard.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                let temporary_name = LeafName::new(format!("candidate-{index}"))?;
                let mut file =
                    guard.create_file(&temporary_name, FilePolicy::private(TEST_MAX_FILE))?;
                file.write_all(format!("writer-{index}").as_bytes())?;
                barrier.wait();
                guard.publish_no_replace(file, &guard, &LeafName::new("published")?)
            }));
        }

        let mut winners = Vec::new();
        for handle in handles {
            match handle.join().expect("thread") {
                Ok(file) => winners.push(file),
                Err(CustodyError::AlreadyExists) => {}
                Err(error) => panic!("unexpected publication error: {error}"),
            }
        }
        assert_eq!(winners.len(), 1);

        let published = guard
            .open_file(
                &LeafName::new("published").expect("leaf"),
                OpenAccess::ReadOnly,
                FilePolicy::private(TEST_MAX_FILE),
            )
            .expect("published file");
        assert_eq!(published.metadata().expect("current metadata").links, 1);
        let mut contents = String::new();
        published
            .file()
            .read_to_string(&mut contents)
            .expect("read published");
        assert!(contents.starts_with("writer-"));
    }

    #[test]
    fn publication_revalidates_dirty_contents_and_read_only_create_is_read_only() {
        let (_temporary, guard) = vault();
        let source_name = LeafName::new("oversized-candidate").expect("leaf");
        let mut source = guard
            .create_file(&source_name, FilePolicy::private(4))
            .expect("source");
        source.write_all(b"12345").expect("write oversized source");
        let error = guard
            .publish_no_replace(
                source,
                &guard,
                &LeafName::new("must-not-publish").expect("leaf"),
            )
            .expect_err("publication must revalidate current content");
        assert!(matches!(error, CustodyError::LimitExceeded(_)));
        assert!(guard
            .entry_metadata(&LeafName::new("must-not-publish").expect("leaf"))
            .expect("metadata")
            .is_none());

        let mut read_only = guard
            .open_or_create_file(
                &LeafName::new("read-only").expect("leaf"),
                OpenAccess::ReadOnly,
                FilePolicy::private(TEST_MAX_FILE),
            )
            .expect("create read-only handle");
        assert!(read_only.was_created());
        assert!(read_only.write_all(b"not writable").is_err());
    }

    #[test]
    fn leaf_names_and_listing_are_bounded() {
        assert!(LeafName::new("../escape").is_err());
        let (_temporary, guard) = vault();
        guard
            .create_file(
                &LeafName::new("one").expect("leaf"),
                FilePolicy::private(TEST_MAX_FILE),
            )
            .expect("create");
        assert!(matches!(
            guard.list_bounded(0),
            Err(CustodyError::LimitExceeded(_))
        ));
        assert_eq!(guard.list_bounded(1).expect("one entry").len(), 1);
    }

    #[test]
    fn streaming_directory_entries_exceed_the_old_collecting_limit_in_constant_memory() {
        const ENTRY_COUNT: usize = 16_385;

        let (temporary, guard) = vault();
        for index in 0..ENTRY_COUNT {
            std_fs::File::create(temporary.path().join("vault").join(index.to_string()))
                .expect("create streamed entry");
        }

        let entries = guard
            .entries_bounded(ENTRY_COUNT)
            .expect("open bounded stream");
        drop(guard);
        assert_eq!(
            entries.fold(0, |count, entry| {
                entry.expect("validated streamed entry");
                count + 1
            }),
            ENTRY_COUNT,
            "the opaque iterator must retain its guard and stream without collecting"
        );

        let guard =
            GuardedDir::open_existing(temporary.path().join("vault"), DirPolicy::private_mutable())
                .expect("reopen guard");
        assert!(guard
            .entries_bounded(ENTRY_COUNT - 1)
            .expect("open limited stream")
            .collect::<Result<Vec<_>>>()
            .is_err());
        assert!(matches!(
            guard.entries_bounded(MAX_DIRECTORY_ENTRIES + 1),
            Err(CustodyError::LimitExceeded(_))
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn exact_macos_alias_is_rewritten_and_nested_aliases_are_not_special() {
        assert_eq!(macos_alias_target(b"tmp"), Some(&b"private/tmp"[..]));
        assert_eq!(macos_alias_target(b"tmp/child"), None);
        assert_ne!(macos_alias_target(b"var"), Some(&b"private/tmp"[..]));

        let unique = format!("pigeonpost-alias-test-{}", std::process::id());
        let logical = Path::new("/tmp").join(unique);
        let guard = GuardedDir::create_private(&logical).expect("create through exact alias");
        assert!(guard.absolute_path().starts_with("/private/tmp"));
        drop(guard);
        std_fs::remove_dir(&logical).expect("cleanup");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_allow_acl_is_rejected_and_deny_only_acl_is_accepted() {
        use std::process::Command;

        let temporary = tempfile::tempdir().expect("tempdir");
        let allowed = temporary.path().join("allowed");
        std_fs::create_dir(&allowed).expect("allowed dir");
        std_fs::set_permissions(&allowed, std_fs::Permissions::from_mode(0o700)).expect("chmod");
        let status = Command::new("/bin/chmod")
            .args(["+a", "everyone allow read"])
            .arg(&allowed)
            .status()
            .expect("run chmod");
        if status.success() {
            assert!(GuardedDir::open_existing(&allowed, DirPolicy::private_mutable()).is_err());
        }

        let denied = temporary.path().join("denied");
        std_fs::create_dir(&denied).expect("denied dir");
        std_fs::set_permissions(&denied, std_fs::Permissions::from_mode(0o700)).expect("chmod");
        let status = Command::new("/bin/chmod")
            .args(["+a", "everyone deny delete"])
            .arg(&denied)
            .status()
            .expect("run chmod");
        if status.success() {
            GuardedDir::open_existing(&denied, DirPolicy::private_mutable())
                .expect("deny-only ACL remains restrictive");
        }
    }
}
