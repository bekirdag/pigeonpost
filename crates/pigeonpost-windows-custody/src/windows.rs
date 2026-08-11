use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::mem::{size_of, MaybeUninit};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
use std::path::{Component, Path, Prefix};
use std::ptr;

use windows_permissions::{LocalBox, SecurityDescriptor};
use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    NtCreateFile, FILE_CREATE, FILE_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT,
    FILE_SYNCHRONOUS_IO_NONALERT,
};
use windows_sys::Win32::Foundation::{
    RtlNtStatusToDosError, HANDLE, INVALID_HANDLE_VALUE, OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE,
    STATUS_OBJECT_NAME_COLLISION, UNICODE_STRING,
};
use windows_sys::Win32::Storage::FileSystem::{
    FileAttributeTagInfo, FileIdInfo, FileRemoteProtocolInfo, GetFileInformationByHandleEx,
    GetFileType, MoveFileExW, FILE_ADD_SUBDIRECTORY, FILE_ATTRIBUTE_NORMAL,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FILE_ID_INFO, FILE_LIST_DIRECTORY,
    FILE_READ_ATTRIBUTES, FILE_REMOTE_PROTOCOL_INFO, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_TRAVERSE, FILE_TYPE_DISK, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, READ_CONTROL,
    SYNCHRONIZE,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

const ERROR_INVALID_FUNCTION: i32 = 1;
const ERROR_NOT_SUPPORTED: i32 = 50;
const ERROR_INVALID_PARAMETER: i32 = 87;
const ERROR_FILE_EXISTS: i32 = 80;
const ERROR_ALREADY_EXISTS: i32 = 183;

/// Stable Windows identity for an opened object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    volume_serial_number: u64,
    file_id: [u8; 16],
}

impl FileIdentity {
    pub fn volume_serial_number(self) -> u64 {
        self.volume_serial_number
    }

    pub fn file_id(self) -> [u8; 16] {
        self.file_id
    }
}

/// A no-delete-share handle and the identity read from that exact handle.
#[derive(Debug)]
pub struct LockedDirectory {
    file: File,
    identity: FileIdentity,
}

impl LockedDirectory {
    pub fn file(&self) -> &File {
        &self.file
    }

    pub fn identity(&self) -> FileIdentity {
        self.identity
    }

    pub fn into_parts(self) -> (File, FileIdentity) {
        (self.file, self.identity)
    }
}

/// Result of an unambiguous `FILE_CREATE` operation.
#[derive(Debug)]
pub enum CreateDirectory {
    Created(LockedDirectory),
    AlreadyExists,
}

/// Validate one child component before it reaches the native object namespace.
pub fn validate_component(name: &OsStr) -> io::Result<()> {
    let name = name.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows custody names must be losslessly Unicode",
        )
    })?;
    if name.is_empty() || name == "." || name == ".." {
        return Err(invalid_name("empty and dot path components are forbidden"));
    }
    if name.chars().any(|character| {
        character <= '\u{1f}'
            || matches!(
                character,
                '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*'
            )
    }) {
        return Err(invalid_name(
            "control characters, separators, alternate streams, and wildcard syntax are forbidden",
        ));
    }
    if name.ends_with(['.', ' ']) {
        return Err(invalid_name(
            "Windows custody names must not end in a dot or space",
        ));
    }
    let utf16_len = name.encode_utf16().count();
    if utf16_len > 255 {
        return Err(invalid_name(
            "Windows custody path components may not exceed 255 UTF-16 units",
        ));
    }
    let device_stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    let upper = device_stem.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
    ) || upper.strip_prefix("COM").is_some_and(|suffix| {
        matches!(
            suffix,
            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
        )
    }) || upper.strip_prefix("LPT").is_some_and(|suffix| {
        matches!(
            suffix,
            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
        )
    }) {
        return Err(invalid_name("reserved DOS device names are forbidden"));
    }
    Ok(())
}

/// Atomically create one private child directory relative to a retained parent handle.
pub fn create_private_directory(parent: &File, name: &OsStr) -> io::Result<CreateDirectory> {
    let current = windows_permissions::utilities::current_process_sid()?;
    let descriptor: LocalBox<SecurityDescriptor> =
        format!("O:{current}D:P(A;OICI;FA;;;{current})").parse()?;
    match open_at(parent, name, FILE_CREATE, Some(&descriptor), true) {
        Ok(directory) => Ok(CreateDirectory::Created(directory)),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            Ok(CreateDirectory::AlreadyExists)
        }
        Err(error) => Err(error),
    }
}

/// Open one existing child directory relative to a retained parent handle.
pub fn open_directory(parent: &File, name: &OsStr) -> io::Result<LockedDirectory> {
    open_at(parent, name, FILE_OPEN, None, false)
}

/// Open an existing child with the additional right needed to create its direct child.
pub fn open_directory_for_child(parent: &File, name: &OsStr) -> io::Result<LockedDirectory> {
    open_at(parent, name, FILE_OPEN, None, true)
}

/// Bind an already-open no-delete-share root/volume handle to its full stable identity.
pub fn lock_directory(file: File) -> io::Result<LockedDirectory> {
    validate_directory_handle(&file)?;
    let identity = read_identity(&file)?;
    Ok(LockedDirectory { file, identity })
}

/// Read the full stable identity of an exact local-disk file or directory handle.
pub fn file_identity(file: &File) -> io::Result<FileIdentity> {
    validate_disk_handle(file)?;
    reject_remote_handle(file)?;
    read_identity(file)
}

/// Atomically publish a file name without clobbering and wait for the move to reach disk.
pub fn move_file_noclobber_write_through(source: &Path, destination: &Path) -> io::Result<()> {
    move_file_write_through(source, destination, false)
}

/// Atomically replace a file name and wait for the move to reach disk.
pub fn replace_file_write_through(source: &Path, destination: &Path) -> io::Result<()> {
    move_file_write_through(source, destination, true)
}

fn move_file_write_through(source: &Path, destination: &Path, replace: bool) -> io::Result<()> {
    let source = wide_absolute_path(source)?;
    let destination = wide_absolute_path(destination)?;
    let mut flags = MOVEFILE_WRITE_THROUGH;
    if replace {
        flags |= MOVEFILE_REPLACE_EXISTING;
    }
    // SAFETY: both paths are validated absolute local-disk paths backed by live NUL-terminated
    // UTF-16 buffers. No pointers escape this call. `MOVEFILE_WRITE_THROUGH` makes successful
    // publication wait until the move has reached disk; replacement is enabled only by the
    // explicit replacement entry point.
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), flags) } != 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if !replace
        && matches!(
            error.raw_os_error(),
            Some(ERROR_FILE_EXISTS) | Some(ERROR_ALREADY_EXISTS)
        )
    {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "write-through publication destination already exists",
        ));
    }
    Err(error)
}

fn wide_absolute_path(path: &Path) -> io::Result<Vec<u16>> {
    if !path.is_absolute() {
        return Err(invalid_name(
            "write-through publication requires an absolute path",
        ));
    }
    let mut ordinary_disk = false;
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => match prefix.kind() {
                Prefix::Disk(_) => ordinary_disk = true,
                Prefix::VerbatimDisk(_) => {}
                _ => {
                    return Err(invalid_name(
                        "write-through publication refuses UNC and device namespaces",
                    ));
                }
            },
            Component::RootDir => {}
            Component::Normal(name) => validate_component(name)?,
            Component::CurDir | Component::ParentDir => {
                return Err(invalid_name(
                    "write-through publication requires normalized components",
                ));
            }
        }
    }
    path.to_str().ok_or_else(|| {
        invalid_name("write-through publication paths must be losslessly Unicode")
    })?;
    let mut encoded = Vec::new();
    if ordinary_disk {
        encoded.extend([b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16]);
    }
    encoded.extend(path.as_os_str().encode_wide());
    if encoded.len() >= 32_767 {
        return Err(invalid_name(
            "write-through publication paths exceed the Windows path limit",
        ));
    }
    encoded.push(0);
    Ok(encoded)
}

fn open_at(
    parent: &File,
    name: &OsStr,
    disposition: u32,
    descriptor: Option<&SecurityDescriptor>,
    can_create_child: bool,
) -> io::Result<LockedDirectory> {
    validate_component(name)?;
    let mut encoded: Vec<u16> = name.encode_wide().collect();
    let byte_len = encoded
        .len()
        .checked_mul(size_of::<u16>())
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| invalid_name("Windows custody name is too long"))?;
    let object_name = UNICODE_STRING {
        Length: byte_len,
        MaximumLength: byte_len,
        Buffer: encoded.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle() as HANDLE,
        ObjectName: &object_name,
        Attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        SecurityDescriptor: descriptor
            .map_or(ptr::null(), |descriptor| ptr::from_ref(descriptor).cast()),
        SecurityQualityOfService: ptr::null(),
    };
    let mut handle = INVALID_HANDLE_VALUE;
    let mut status = IO_STATUS_BLOCK::default();

    // SAFETY: every pointer refers to a live stack/local allocation for the duration of the call;
    // `object_name` is a counted UTF-16 buffer, `RootDirectory` is borrowed and remains open, the
    // optional self-relative security descriptor remains alive, and `handle`/`status` are writable
    // out-parameters. On success ownership of the unique returned handle is transferred exactly
    // once into `File` below.
    let mut desired_access =
        FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE;
    if can_create_child {
        desired_access |= FILE_ADD_SUBDIRECTORY;
    }
    let ntstatus = unsafe {
        NtCreateFile(
            &mut handle,
            desired_access,
            &attributes,
            &mut status,
            ptr::null(),
            FILE_ATTRIBUTE_NORMAL,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            disposition,
            FILE_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
            ptr::null(),
            0,
        )
    };
    if ntstatus < 0 {
        return Err(ntstatus_error(ntstatus));
    }
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::other(
            "NtCreateFile succeeded without returning a valid directory handle",
        ));
    }

    // SAFETY: successful `NtCreateFile` returned a uniquely owned kernel handle. This is the only
    // ownership conversion, and `File` closes it exactly once on every later success/error path.
    let file = unsafe { File::from_raw_handle(handle as RawHandle) };
    lock_directory(file)
}

fn validate_directory_handle(file: &File) -> io::Result<()> {
    validate_disk_handle(file)?;

    let mut attributes = MaybeUninit::<FILE_ATTRIBUTE_TAG_INFO>::uninit();
    // SAFETY: the output buffer is correctly sized and writable, and the borrowed handle remains
    // valid for the call.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            FileAttributeTagInfo,
            attributes.as_mut_ptr().cast(),
            size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful query initialized the entire fixed-size output structure.
    let attributes = unsafe { attributes.assume_init() };
    if attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows custody refuses reparse-point directories",
        ));
    }

    reject_remote_handle(file)
}

fn validate_disk_handle(file: &File) -> io::Result<()> {
    // SAFETY: `file` owns a valid handle for the duration of this non-mutating query.
    if unsafe { GetFileType(file.as_raw_handle() as HANDLE) } != FILE_TYPE_DISK {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows custody accepts only disk handles",
        ));
    }
    Ok(())
}

fn reject_remote_handle(file: &File) -> io::Result<()> {
    let mut remote = MaybeUninit::<FILE_REMOTE_PROTOCOL_INFO>::uninit();
    // SAFETY: the output buffer is correctly sized and writable, and the borrowed handle remains
    // valid for the call.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            FileRemoteProtocolInfo,
            remote.as_mut_ptr().cast(),
            size_of::<FILE_REMOTE_PROTOCOL_INFO>() as u32,
        )
    };
    if ok != 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows custody refuses remote filesystem directories",
        ));
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(ERROR_INVALID_FUNCTION)
        | Some(ERROR_NOT_SUPPORTED)
        | Some(ERROR_INVALID_PARAMETER) => Ok(()),
        _ => Err(error),
    }
}

fn read_identity(file: &File) -> io::Result<FileIdentity> {
    let mut identity = MaybeUninit::<FILE_ID_INFO>::uninit();
    // SAFETY: the output buffer is correctly sized and writable, and the borrowed handle remains
    // valid for the call.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            FileIdInfo,
            identity.as_mut_ptr().cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful query initialized the entire fixed-size output structure.
    let identity = unsafe { identity.assume_init() };
    Ok(FileIdentity {
        volume_serial_number: identity.VolumeSerialNumber,
        file_id: identity.FileId.Identifier,
    })
}

fn ntstatus_error(status: i32) -> io::Error {
    if status == STATUS_OBJECT_NAME_COLLISION {
        return io::Error::new(
            io::ErrorKind::AlreadyExists,
            "NtCreateFile reported that the child already exists",
        );
    }
    // SAFETY: `RtlNtStatusToDosError` is a pure conversion for any NTSTATUS value.
    let dos_error = unsafe { RtlNtStatusToDosError(status) };
    io::Error::from_raw_os_error(dos_error as i32)
}

fn invalid_name(reason: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;
    use windows_permissions::constants::{
        AccessRights, AceFlags, AceType, SeObjectType, SecurityInformation,
    };
    use windows_permissions::wrappers;

    fn open_test_parent(path: &std::path::Path) -> File {
        let mut options = std::fs::OpenOptions::new();
        options
            .read(true)
            .access_mode(
                FILE_LIST_DIRECTORY
                    | FILE_TRAVERSE
                    | FILE_READ_ATTRIBUTES
                    | FILE_ADD_SUBDIRECTORY
                    | READ_CONTROL,
            )
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(0x0200_0000 | FILE_OPEN_REPARSE_POINT);
        options.open(path).unwrap()
    }

    #[test]
    fn create_is_unambiguous_private_and_identity_bound() {
        let root = tempfile::tempdir().unwrap();
        let parent = open_test_parent(root.path());
        let name = OsStr::new("private");
        let created = match create_private_directory(&parent, name).unwrap() {
            CreateDirectory::Created(created) => created,
            CreateDirectory::AlreadyExists => panic!("first create unexpectedly collided"),
        };
        assert!(matches!(
            create_private_directory(&parent, name).unwrap(),
            CreateDirectory::AlreadyExists
        ));
        let reopened = open_directory(&parent, name).unwrap();
        assert_eq!(created.identity(), reopened.identity());
        assert!(created.file().metadata().unwrap().is_dir());
        assert_eq!(created.identity().file_id().len(), 16);

        let descriptor = wrappers::GetSecurityInfo(
            created.file(),
            SeObjectType::SE_FILE_OBJECT,
            SecurityInformation::Owner | SecurityInformation::Dacl,
        )
        .unwrap();
        let current = windows_permissions::utilities::current_process_sid().unwrap();
        assert_eq!(descriptor.owner(), Some(&*current));
        let dacl = descriptor.dacl().unwrap();
        assert_eq!(dacl.len(), 1);
        let ace = dacl.get_ace(0).unwrap();
        assert_eq!(ace.ace_type(), AceType::ACCESS_ALLOWED_ACE_TYPE);
        assert_eq!(ace.sid(), Some(&*current));
        assert!(ace.mask().contains(AccessRights::FileAllAccess));
        assert!(ace
            .flags()
            .contains(AceFlags::ObjectInherit | AceFlags::ContainerInherit));
        let encoded = wrappers::ConvertSecurityDescriptorToStringSecurityDescriptor(
            &descriptor,
            SecurityInformation::Owner | SecurityInformation::Dacl,
        )
        .unwrap();
        assert!(encoded.to_string_lossy().contains("D:P"));
    }

    #[test]
    fn held_directory_handle_excludes_rename_and_delete() {
        let root = tempfile::tempdir().unwrap();
        let parent = open_test_parent(root.path());
        let name = OsStr::new("locked");
        let locked = match create_private_directory(&parent, name).unwrap() {
            CreateDirectory::Created(created) => created,
            CreateDirectory::AlreadyExists => panic!("first create unexpectedly collided"),
        };
        let path = root.path().join(name);
        let moved = root.path().join("moved");
        assert!(std::fs::rename(&path, &moved).is_err());
        assert!(std::fs::remove_dir(&path).is_err());
        drop(locked);
        std::fs::rename(&path, &moved).unwrap();
    }

    #[test]
    fn rejects_ambiguous_components() {
        for name in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "stream:name",
            "wild*card",
            "control\u{1f}",
            "NUL",
            "COM¹.txt",
            "CONOUT$",
            "trail.",
            "trail ",
        ] {
            assert!(
                validate_component(OsStr::new(name)).is_err(),
                "accepted {name:?}"
            );
        }
    }

    #[test]
    fn collision_status_maps_to_already_exists() {
        assert_eq!(
            ntstatus_error(STATUS_OBJECT_NAME_COLLISION).kind(),
            io::ErrorKind::AlreadyExists
        );
    }

    #[test]
    fn full_file_identity_and_write_through_publication_are_exact() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.key");
        let destination = root.path().join("destination.key");
        std::fs::write(&source, b"source").unwrap();
        std::fs::write(&destination, b"destination").unwrap();
        let first = File::open(&source).unwrap();
        let second = File::open(&source).unwrap();
        assert_eq!(
            file_identity(&first).unwrap(),
            file_identity(&second).unwrap()
        );
        assert_eq!(file_identity(&first).unwrap().file_id().len(), 16);
        drop(first);
        drop(second);

        let collision = move_file_noclobber_write_through(&source, &destination).unwrap_err();
        assert_eq!(collision.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&source).unwrap(), b"source");
        assert_eq!(std::fs::read(&destination).unwrap(), b"destination");

        replace_file_write_through(&source, &destination).unwrap();
        assert!(!source.exists());
        assert_eq!(std::fs::read(&destination).unwrap(), b"source");
    }
}
