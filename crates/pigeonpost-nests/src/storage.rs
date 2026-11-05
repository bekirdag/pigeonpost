//! Per-Nest volumes.
//!
//! Production ([`LvmLuks`]): one thin logical volume per Nest in a loop-backed LVM thin pool (this
//! host has a single disk), each formatted inside its own LUKS2 container with its own random key.
//! The LV size is the Nest's hard disk quota. Deleting a Nest wipes its LUKS header and removes its
//! key file, so its blocks are unreadable even before the thin pool reuses them. A stopped Nest's
//! volume is unmounted and its mapping closed.
//!
//! Every command is run with explicit arguments (no shell) and every Nest id is validated by
//! [`crate::util::valid_nest_id`] before it reaches a path or a device name.

use crate::config::StorageConfig;
use crate::util::valid_nest_id;
use std::path::{Path, PathBuf};
use std::process::Command;

pub trait Volumes: Send + Sync {
    /// Create, format and mount a new volume. Idempotent for an existing, healthy volume.
    fn create(&self, nest_id: &str, gib: u64) -> Result<PathBuf, String>;
    /// Open and mount an existing volume; returns the host mount path.
    fn open(&self, nest_id: &str) -> Result<PathBuf, String>;
    /// Unmount and close. Idempotent.
    fn close(&self, nest_id: &str) -> Result<(), String>;
    /// Erase: close, destroy the key and the volume. Idempotent.
    fn destroy(&self, nest_id: &str) -> Result<(), String>;
    /// Nest ids that have a volume on this host.
    fn list(&self) -> Result<Vec<String>, String>;
    /// (used bytes, size bytes) of a mounted volume.
    fn usage(&self, nest_id: &str) -> Option<(u64, u64)>;
}

fn check_id(nest_id: &str) -> Result<(), String> {
    if valid_nest_id(nest_id) {
        Ok(())
    } else {
        Err(format!("invalid nest id {nest_id:?}"))
    }
}

/// Run a command, returning stdout, or an error naming the program and its stderr.
pub(crate) fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "{program} {}: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Run with a secret on stdin (cryptsetup `--key-file -`), so it never appears in argv or a file
/// other than the root-only key file.
fn run_with_stdin(program: &str, args: &[&str], stdin: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut child = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    if let Some(mut input) = child.stdin.take() {
        input
            .write_all(stdin)
            .map_err(|e| format!("{program}: {e}"))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("{program}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{program} {}: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

pub struct LvmLuks {
    cfg: StorageConfig,
}

impl LvmLuks {
    pub fn new(cfg: StorageConfig) -> Self {
        LvmLuks { cfg }
    }
    fn lv(&self, id: &str) -> String {
        format!("{}/nest-{id}", self.cfg.vg)
    }
    fn lv_dev(&self, id: &str) -> String {
        format!("/dev/{}/nest-{id}", self.cfg.vg)
    }
    fn mapper(&self, id: &str) -> String {
        format!("nestc-{id}")
    }
    fn mapper_dev(&self, id: &str) -> String {
        format!("/dev/mapper/nestc-{id}")
    }
    fn key_path(&self, id: &str) -> PathBuf {
        self.cfg.key_dir.join(format!("{id}.key"))
    }
    fn mount_path(&self, id: &str) -> PathBuf {
        self.cfg.mount_root.join(id)
    }
    fn lv_exists(&self, id: &str) -> bool {
        Path::new(&self.lv_dev(id)).exists() || run("lvs", &["--noheadings", &self.lv(id)]).is_ok()
    }
    fn mounted(&self, id: &str) -> bool {
        let target = self.mount_path(id);
        std::fs::read_to_string("/proc/self/mounts")
            .map(|m| {
                m.lines()
                    .any(|l| l.split_whitespace().nth(1) == Some(&*target.to_string_lossy()))
            })
            .unwrap_or(false)
    }
    fn read_key(&self, id: &str) -> Result<Vec<u8>, String> {
        std::fs::read(self.key_path(id)).map_err(|e| format!("key for {id}: {e}"))
    }
    fn map(&self, id: &str) -> Result<(), String> {
        if Path::new(&self.mapper_dev(id)).exists() {
            return Ok(());
        }
        let key = self.read_key(id)?;
        let r = run_with_stdin(
            "cryptsetup",
            &[
                "open",
                "--type",
                "luks2",
                "--key-file",
                "-",
                &self.lv_dev(id),
                &self.mapper(id),
            ],
            &key,
        );
        let mut key = key;
        zeroize::Zeroize::zeroize(&mut key);
        r
    }
    fn mount(&self, id: &str) -> Result<PathBuf, String> {
        let target = self.mount_path(id);
        if !self.mounted(id) {
            std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
            run(
                "mount",
                &[
                    "-o",
                    "nosuid,nodev,noexec,noatime",
                    &self.mapper_dev(id),
                    &target.to_string_lossy(),
                ],
            )?;
        }
        Ok(target)
    }
}

impl Volumes for LvmLuks {
    fn create(&self, nest_id: &str, gib: u64) -> Result<PathBuf, String> {
        check_id(nest_id)?;
        if self.lv_exists(nest_id) && self.key_path(nest_id).exists() {
            return self.open(nest_id);
        }
        std::fs::create_dir_all(&self.cfg.key_dir).map_err(|e| e.to_string())?;
        let size = format!("{gib}G");
        let pool = format!("{}/{}", self.cfg.vg, self.cfg.pool);
        if !self.lv_exists(nest_id) {
            run(
                "lvcreate",
                &[
                    "-q",
                    "-V",
                    &size,
                    "-T",
                    &pool,
                    "-n",
                    &format!("nest-{nest_id}"),
                ],
            )?;
        }
        // 64 random bytes; no passphrase stretching is needed for a full-entropy key, and a cheap
        // PBKDF keeps start-up fast and memory-light.
        let mut key = [0u8; 64];
        rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut key);
        write_private(&self.key_path(nest_id), &key)?;
        let format = run_with_stdin(
            "cryptsetup",
            &[
                "luksFormat",
                "--batch-mode",
                "--type",
                "luks2",
                "--pbkdf",
                "pbkdf2",
                "--pbkdf-force-iterations",
                "1000",
                "--key-file",
                "-",
                &self.lv_dev(nest_id),
            ],
            &key,
        );
        zeroize::Zeroize::zeroize(&mut key);
        format?;
        self.map(nest_id)?;
        run(
            "mkfs.ext4",
            &[
                "-q",
                "-m",
                "0",
                "-E",
                "nodiscard",
                "-L",
                "nest",
                &self.mapper_dev(nest_id),
            ],
        )?;
        let target = self.mount(nest_id)?;
        prepare_layout(&target, self.cfg.sandbox_uid)?;
        Ok(target)
    }

    fn open(&self, nest_id: &str) -> Result<PathBuf, String> {
        check_id(nest_id)?;
        self.map(nest_id)?;
        let target = self.mount(nest_id)?;
        prepare_layout(&target, self.cfg.sandbox_uid)?;
        Ok(target)
    }

    fn close(&self, nest_id: &str) -> Result<(), String> {
        check_id(nest_id)?;
        if self.mounted(nest_id) {
            run("umount", &[&self.mount_path(nest_id).to_string_lossy()])?;
        }
        if Path::new(&self.mapper_dev(nest_id)).exists() {
            run("cryptsetup", &["close", &self.mapper(nest_id)])?;
        }
        Ok(())
    }

    fn destroy(&self, nest_id: &str) -> Result<(), String> {
        check_id(nest_id)?;
        self.close(nest_id)?;
        if self.lv_exists(nest_id) {
            // Wipe every keyslot first: from here the data is unrecoverable, whatever happens to
            // the blocks afterwards.
            let _ = run(
                "cryptsetup",
                &["erase", "--batch-mode", &self.lv_dev(nest_id)],
            );
            run("lvremove", &["-q", "-y", &self.lv(nest_id)])?;
        }
        let key = self.key_path(nest_id);
        if key.exists() {
            // Overwrite before unlinking; the filesystem may keep old blocks, which is why the
            // LUKS header erase above is the real guarantee.
            let _ = std::fs::write(&key, [0u8; 64]);
            std::fs::remove_file(&key).map_err(|e| e.to_string())?;
        }
        let _ = std::fs::remove_dir(self.mount_path(nest_id));
        Ok(())
    }

    fn list(&self) -> Result<Vec<String>, String> {
        let out = run("lvs", &["--noheadings", "-o", "lv_name", &self.cfg.vg])?;
        Ok(out
            .lines()
            .filter_map(|l| l.trim().strip_prefix("nest-"))
            .filter(|id| valid_nest_id(id))
            .map(str::to_string)
            .collect())
    }

    fn usage(&self, nest_id: &str) -> Option<(u64, u64)> {
        statvfs_usage(&self.mount_path(nest_id))
    }
}

/// Development/test backend: a plain directory per Nest. No quota, no encryption. Refused by the
/// production configuration check.
pub struct DirVolumes {
    root: PathBuf,
    uid: u32,
}

impl DirVolumes {
    pub fn new(root: PathBuf, uid: u32) -> Self {
        DirVolumes { root, uid }
    }
}

impl Volumes for DirVolumes {
    fn create(&self, nest_id: &str, _gib: u64) -> Result<PathBuf, String> {
        check_id(nest_id)?;
        let p = self.root.join(nest_id);
        std::fs::create_dir_all(&p).map_err(|e| e.to_string())?;
        prepare_layout(&p, self.uid).ok();
        Ok(p)
    }
    fn open(&self, nest_id: &str) -> Result<PathBuf, String> {
        check_id(nest_id)?;
        let p = self.root.join(nest_id);
        if p.is_dir() {
            Ok(p)
        } else {
            Err(format!("no volume for {nest_id}"))
        }
    }
    fn close(&self, nest_id: &str) -> Result<(), String> {
        check_id(nest_id)
    }
    fn destroy(&self, nest_id: &str) -> Result<(), String> {
        check_id(nest_id)?;
        let p = self.root.join(nest_id);
        if p.exists() {
            std::fs::remove_dir_all(p).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    fn list(&self) -> Result<Vec<String>, String> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Ok(Vec::new());
        };
        Ok(entries
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| valid_nest_id(n))
            .collect())
    }
    fn usage(&self, _nest_id: &str) -> Option<(u64, u64)> {
        None
    }
}

/// The volume's fixed layout. `home` is the agent's PIGEONPOST_HOME (mailbox credential, device
/// key, agentd config and spool), `workspace` is where its work happens, `run` holds per-start
/// files written by the control service (the broker token).
pub const LAYOUT: &[&str] = &["home", "workspace", "run"];

fn prepare_layout(root: &Path, uid: u32) -> Result<(), String> {
    for dir in LAYOUT {
        let p = root.join(dir);
        std::fs::create_dir_all(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        set_owner_mode(&p, uid, 0o700)?;
    }
    set_owner_mode(root, uid, 0o700)?;
    // ext4's lost+found belongs to root and stays out of the sandbox's reach.
    Ok(())
}

/// Write a file only its owner can read (0400, root).
pub(crate) fn write_private(path: &Path, data: &[u8]) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let _ = std::fs::remove_file(path);
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o400)
            .open(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        f.write_all(data)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, data).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// Write a file owned by the sandbox user, readable only by it.
pub(crate) fn write_for_sandbox(path: &Path, data: &[u8], uid: u32) -> Result<(), String> {
    let _ = std::fs::remove_file(path);
    std::fs::write(path, data).map_err(|e| format!("{}: {e}", path.display()))?;
    set_owner_mode(path, uid, 0o400)
}

/// Create a directory (and parents) owned by the sandbox user with `mode`.
pub(crate) fn dir_for_sandbox(path: &Path, uid: u32, mode: u32) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|e| format!("{}: {e}", path.display()))?;
    set_owner_mode(path, uid, mode)
}

#[cfg(unix)]
fn set_owner_mode(path: &Path, uid: u32, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    // chown only works as root; tests run unprivileged and skip it.
    if is_root() {
        std::os::unix::fs::chown(path, Some(uid), Some(uid))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_mode(_path: &Path, _uid: u32, _mode: u32) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn is_root() -> bool {
    // Effective uid from procfs (Linux, where this runs as a service); elsewhere, never root.
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Uid:"))
                .and_then(|v| v.split_whitespace().nth(1).map(|e| e == "0"))
        })
        .unwrap_or(false)
}

fn statvfs_usage(path: &Path) -> Option<(u64, u64)> {
    // `df` keeps this free of unsafe libc calls.
    let out = run(
        "df",
        &["-B1", "--output=used,size", &path.to_string_lossy()],
    )
    .ok()?;
    let line = out.lines().nth(1)?;
    let mut it = line.split_whitespace().map(|v| v.parse::<u64>().ok());
    Some((it.next()??, it.next()??))
}

pub fn from_config(cfg: &StorageConfig) -> Result<Box<dyn Volumes>, String> {
    match cfg.backend.as_str() {
        "lvm-luks" => Ok(Box::new(LvmLuks::new(cfg.clone()))),
        "dir" => Ok(Box::new(DirVolumes::new(
            cfg.mount_root.clone(),
            cfg.sandbox_uid,
        ))),
        other => Err(format!("unknown storage backend {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_volumes_round_trip_and_refuse_bad_ids() {
        let root = std::env::temp_dir().join(format!("nests-vol-{}", crate::util::random_hex(6)));
        let v = DirVolumes::new(root.clone(), 10001);
        let id = "nst_00000000000000aa";
        let p = v.create(id, 1).unwrap();
        for d in LAYOUT {
            assert!(p.join(d).is_dir());
        }
        assert_eq!(v.list().unwrap(), vec![id.to_string()]);
        assert!(v.create("../etc", 1).is_err());
        assert!(v.open("nst_00000000000000aa/..").is_err());
        v.destroy(id).unwrap();
        v.destroy(id).unwrap();
        assert!(v.list().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }
}
