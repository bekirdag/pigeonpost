use std::path::Path;

pub(crate) fn private_tempdir() -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("create private test directory");
    harden_existing(directory.path(), true).expect("harden private test directory");
    directory
}

pub(crate) fn write_private(
    path: impl AsRef<Path>,
    contents: impl AsRef<[u8]>,
) -> std::io::Result<()> {
    let path = path.as_ref();
    std::fs::write(path, contents)?;
    harden_existing(path, false)
}

#[cfg(unix)]
fn harden_existing(path: &Path, directory: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = if directory { 0o700 } else { 0o600 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(windows)]
fn harden_existing(path: &Path, directory: bool) -> std::io::Result<()> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;

    use windows_permissions::constants::{SeObjectType, SecurityInformation};
    use windows_permissions::{wrappers, LocalBox, SecurityDescriptor};

    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const READ_CONTROL: u32 = 0x0002_0000;
    const WRITE_DAC: u32 = 0x0004_0000;
    const WRITE_OWNER: u32 = 0x0008_0000;

    let mut options = OpenOptions::new();
    options
        .read(true)
        .access_mode(READ_CONTROL | WRITE_DAC | WRITE_OWNER)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    if directory {
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
    }
    let mut object = options.open(path)?;
    let current = windows_permissions::utilities::current_process_sid()?;
    let inheritance = if directory { "OICI" } else { "" };
    let descriptor: LocalBox<SecurityDescriptor> =
        format!("O:{current}D:P(A;{inheritance};FA;;;{current})").parse()?;
    let dacl = descriptor.dacl().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "private test descriptor has no DACL",
        )
    })?;
    wrappers::SetSecurityInfo(
        &mut object,
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Owner | SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
        Some(&current),
        None,
        Some(dacl),
        None,
    )
}

#[cfg(not(any(unix, windows)))]
fn harden_existing(_path: &Path, _directory: bool) -> std::io::Result<()> {
    Ok(())
}
