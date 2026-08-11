//! `pigeonpost install` — turn this box into a supervised, bounded loft.

#[cfg(any(test, target_os = "linux", target_os = "macos"))]
use std::ffi::OsStr;
#[cfg(any(test, target_os = "linux", target_os = "macos"))]
use std::fs;
#[cfg(any(test, target_os = "linux", target_os = "macos"))]
use std::io;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::io::{Read, Write};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::net::TcpStream;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::time::Duration;

use serde::Serialize;

use pigeonpost_directory::private_store::{write_private_file_atomic, PrivateDirectory};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use pigeonpost_unix_custody::{
    CustodyError, DirPolicy, FilePolicy, GuardedDir, LeafName, NormalizedPath, OpenAccess,
};

const MAX_DEFAULT_CAPACITY_GB: u64 = 20;
const MAX_CAPACITY_GB: u64 = 1_048_576;
const MAX_RETENTION_DAYS: u64 = 3_650;
const MAX_INSTALL_FILE_BYTES: u64 = 64 * 1024;
const FREE_DISK_SHARE: f64 = 0.20;
const CAPACITY_OVERRIDE_HINT: &str =
    "Change the storage cap by rerunning install with --capacity-gb <1..1048576>.";
const JURISDICTION_NOTICE: &str = "Operating a public loft can create jurisdiction-dependent operator duties. Complete operator-specific legal review and the regulatory, custody, retention, and process-intake prerequisites before public submission. See docs/node.md and docs/law.md.";
#[cfg(any(target_os = "linux", target_os = "macos"))]
const NPM_LAUNCHER_PROTOCOL_ENV: &str = "PIGEONPOST_NPM_LAUNCHER_PROTOCOL";
#[cfg(any(target_os = "linux", target_os = "macos"))]
const NPM_LAUNCHER_NODE_ENV: &str = "PIGEONPOST_NPM_LAUNCHER_NODE";
#[cfg(any(target_os = "linux", target_os = "macos"))]
const NPM_LAUNCHER_ENTRY_ENV: &str = "PIGEONPOST_NPM_LAUNCHER_ENTRY";
#[cfg(any(test, target_os = "linux", target_os = "macos"))]
const NPM_LAUNCHER_PROTOCOL_V1: &str = "npm-v1";
#[cfg(any(test, target_os = "linux"))]
const SYSTEMD_ACTIVATION_STEPS: &[&[&str]] = &[
    &["--user", "daemon-reload"],
    &["--user", "enable", "pigeonpost-loft.service"],
    &["--user", "restart", "pigeonpost-loft.service"],
];

pub struct InstallOptions {
    pub dir: PathBuf,
    pub domain: Option<String>,
    pub capacity_gb: Option<u64>,
    pub retention_days: u64,
    pub bind: Option<String>,
    /// Write the config and keys but do not touch the service manager.
    pub no_service: bool,
}

#[derive(Serialize)]
struct InstalledConfig<'a> {
    loft: InstalledLoft<'a>,
    pool: PoolConfig<'a>,
}

#[derive(Serialize)]
struct InstalledLoft<'a> {
    bind: &'a str,
    storage_path: String,
    capacity_gb: u64,
    retention_days: u64,
    trusted_proxies: Vec<IpAddr>,
    policy: PolicyConfig,
}

#[derive(Serialize)]
struct PolicyConfig {
    open: bool,
    pow_floor: u32,
    max_event_bytes: usize,
}

#[derive(Serialize)]
struct PoolConfig<'a> {
    join: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    domain: Option<&'a str>,
}

/// The persistent service command, separated from the native process that happens to run install.
///
/// The npm launcher downloads a versioned native binary into a cache. When it hands control to
/// that binary, the service must retain the stable launcher entrypoint, not `current_exe()`, so a
/// restart selects the current package and verifies its cached binary again.
#[derive(Debug, Eq, PartialEq)]
#[cfg(any(test, target_os = "linux", target_os = "macos"))]
struct ServiceCommand {
    program: PathBuf,
    prefix_args: Vec<PathBuf>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
enum ServiceUnit {
    #[cfg(target_os = "linux")]
    Systemd(PathBuf),
    #[cfg(target_os = "macos")]
    Launchd(PathBuf),
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl ServiceUnit {
    fn path(&self) -> &Path {
        match self {
            #[cfg(target_os = "linux")]
            Self::Systemd(path) => path,
            #[cfg(target_os = "macos")]
            Self::Launchd(path) => path,
        }
    }
}

pub fn run(mut options: InstallOptions) -> Result<(), Box<dyn std::error::Error>> {
    let domain = options.domain.as_deref().map(validate_domain).transpose()?;
    let public = domain.is_some();
    if public && !options.no_service {
        return Err(
            "public installation requires --no-service until compliance trust is provisioned"
                .into(),
        );
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    if !options.no_service {
        return Err(format!(
            "pigeonpost install service mode is not supported on {}; rerun with --no-service",
            std::env::consts::OS
        )
        .into());
    }

    let bind = validate_bind(options.bind.as_deref().unwrap_or("127.0.0.1:7717"), public)?;

    let installation_directory = prepare_directory(&options.dir)?;
    options.dir = installation_directory.normalized_path().to_path_buf();

    let capacity_gb = match options.capacity_gb {
        Some(capacity_gb) => capacity_gb,
        None => default_capacity_gb(&options.dir)?,
    };
    if !(1..=MAX_CAPACITY_GB).contains(&capacity_gb) {
        return Err(format!("capacity-gb must be between 1 and {MAX_CAPACITY_GB}").into());
    }
    if !(1..=MAX_RETENTION_DAYS).contains(&options.retention_days) {
        return Err(format!("retention-days must be between 1 and {MAX_RETENTION_DAYS}").into());
    }

    let storage_path = options.dir.join("data/loft.db");
    let storage_directory = prepare_directory(
        storage_path
            .parent()
            .ok_or("storage path has no parent directory")?,
    )?;

    println!("pigeonpost install");
    println!();
    println!(
        "  ✓ Platform      {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );

    let key_path = options.dir.join("loft.key");
    let (identity, _) = crate::loft_key::load_or_create(&key_path)?;
    println!("  ✓ Loft keypair  {} (0600)", key_path.display());
    println!(
        "  ✓ Storage       {} — cap {capacity_gb} GB",
        storage_path.display()
    );
    println!("  ℹ Capacity      {CAPACITY_OVERRIDE_HINT}");
    println!("  ✓ Retention     {} days", options.retention_days);

    let config_path = options.dir.join("loft.toml");
    let config = config_toml(
        &storage_path,
        capacity_gb,
        options.retention_days,
        &bind,
        domain.as_deref(),
    )?;
    write_private_file_atomic(&config_path, config.as_bytes(), MAX_INSTALL_FILE_BYTES)?;
    println!("  ✓ Config        {}", config_path.display());

    let caddy_path = if let Some(domain) = domain.as_deref() {
        let path = options.dir.join("loft.Caddyfile");
        write_private_file_atomic(
            &path,
            caddyfile(domain, &bind).as_bytes(),
            MAX_INSTALL_FILE_BYTES,
        )?;
        println!("  ✓ TLS proxy     {}", path.display());
        Some(path)
    } else {
        None
    };

    if public {
        println!("  ✓ Mode          public candidate — loft stays on loopback behind TLS");
    } else {
        println!("  ✓ Mode          private — serving agents on this host only");
    }

    if options.no_service {
        println!("  – Service       skipped (--no-service)");
    } else {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            if let Some(unit) = write_service_unit(&options.dir)? {
                activate_service(&unit)?;
                wait_until_ready(&bind)?;
                println!("  ✓ Service       active ({})", unit.path().display());
                println!("  ✓ Readiness     http://{bind}/ready");
            } else {
                return Err(
                    "no supported user service manager found; rerun with --no-service".into(),
                );
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            return Err(format!(
                "pigeonpost install service mode is not supported on {}; rerun with --no-service",
                std::env::consts::OS
            )
            .into());
        }
    }

    println!();
    println!("  Loft key   {}", hex(identity.verifying_key().as_bytes()));
    if options.no_service {
        println!(
            "  Start it   pigeonpost loft serve --dir {}",
            options.dir.display()
        );
    }
    if let (Some(domain), Some(caddy_path)) = (domain.as_deref(), caddy_path.as_ref()) {
        println!("  Public URL https://{domain}");
        println!();
        println!(
            "  Before directory submission, point DNS at this host and activate the generated"
        );
        println!("  Caddy configuration: {}", caddy_path.display());
        println!("  Then submit only the HTTPS endpoint after it passes an external probe.");
    } else {
        println!("  Point at   pigeonpost loft add http://{bind}");
        println!();
        println!("  To join the public pool, reinstall with --domain loft.example.com.");
    }

    println!();
    println!("  Jurisdiction notice: {JURISDICTION_NOTICE}");

    installation_directory.verify_named()?;
    storage_directory.verify_named()?;

    Ok(())
}

pub(crate) fn prepare_directory(
    dir: &Path,
) -> Result<PrivateDirectory, Box<dyn std::error::Error>> {
    #[cfg(unix)]
    {
        use rustix::fs::Mode;
        let normalized = NormalizedPath::new(dir)?;
        match GuardedDir::open_existing(normalized.as_path(), DirPolicy::private_mutable()) {
            Ok(directory) => {
                directory.verify_named()?;
                drop(directory);
                return Ok(PrivateDirectory::open_or_create(normalized.as_path())?);
            }
            Err(CustodyError::NotFound) => {
                return Ok(PrivateDirectory::open_or_create(normalized.as_path())?);
            }
            Err(_) => {}
        }

        // Legacy installers produced owned 0755 roots. Open the complete path under the trusted
        // policy first, then harden only the already-proved descriptor. Group/world-writable
        // roots and mutable or linked ancestors never reach fchmod.
        let directory = GuardedDir::open_existing(normalized.as_path(), DirPolicy::trusted())?;
        let before = directory.verify_live()?;
        if before.uid != rustix::process::geteuid().as_raw() {
            return Err("loft directories must be owned by the current user".into());
        }
        rustix::fs::fchmod(&directory, Mode::from_raw_mode(0o700)).map_err(io::Error::from)?;
        directory.sync()?;
        directory.verify_named()?;
        let after = directory.verify_live()?;
        if after.identity != before.identity || after.mode & 0o7777 != 0o700 {
            return Err("loft directory changed or is not owner-only".into());
        }
        drop(directory);
        Ok(PrivateDirectory::open_or_create(normalized.as_path())?)
    }

    #[cfg(not(unix))]
    {
        Ok(PrivateDirectory::open_or_create(dir)?)
    }
}

fn validate_bind(value: &str, public: bool) -> Result<String, Box<dyn std::error::Error>> {
    let address: SocketAddr = value
        .parse()
        .map_err(|_| "bind must be an IP socket address such as 127.0.0.1:7717")?;
    if address.port() == 0 {
        return Err("bind port must not be zero".into());
    }
    if public && !address.ip().is_loopback() {
        return Err(
            "public mode must bind to loopback; expose it only through the generated TLS proxy"
                .into(),
        );
    }
    Ok(address.to_string())
}

fn validate_domain(value: &str) -> Result<String, Box<dyn std::error::Error>> {
    let domain = value.trim_end_matches('.').to_ascii_lowercase();
    if domain.len() > 253
        || !domain.contains('.')
        || domain.parse::<IpAddr>().is_ok()
        || pigeonpost_core::network::is_localhost_name(&domain)
    {
        return Err("domain must be a public DNS hostname".into());
    }
    let valid = domain.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label.as_bytes()[0].is_ascii_alphanumeric()
            && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    });
    if !valid {
        return Err("domain contains an invalid DNS label".into());
    }
    Ok(domain)
}

fn default_capacity_gb(dir: &Path) -> Result<u64, Box<dyn std::error::Error>> {
    let Some(free) = free_disk_gb(dir) else {
        return Err(
            "unable to determine free disk space; rerun with an explicit --capacity-gb value"
                .into(),
        );
    };
    Ok(capacity_for_free_disk_gb(free))
}

fn capacity_for_free_disk_gb(free_gb: u64) -> u64 {
    let share = (free_gb as f64 * FREE_DISK_SHARE) as u64;
    share.clamp(1, MAX_DEFAULT_CAPACITY_GB)
}

fn free_disk_gb(dir: &Path) -> Option<u64> {
    fs2::available_space(dir)
        .ok()
        .map(|available_bytes| available_bytes / (1024 * 1024 * 1024))
}

fn config_toml(
    storage_path: &Path,
    capacity_gb: u64,
    retention_days: u64,
    bind: &str,
    domain: Option<&str>,
) -> Result<String, Box<dyn std::error::Error>> {
    let trusted_proxies = match domain {
        Some(_) => vec![bind
            .parse::<SocketAddr>()
            .map_err(|_| "public proxy bind must be an IP socket address")?
            .ip()],
        None => Vec::new(),
    };
    let config = InstalledConfig {
        loft: InstalledLoft {
            bind,
            storage_path: storage_path.display().to_string(),
            capacity_gb,
            retention_days,
            trusted_proxies,
            policy: PolicyConfig {
                open: true,
                pow_floor: 0,
                max_event_bytes: pigeonpost_loft::LoftConfig::new([0u8; 32], "http://127.0.0.1:1")
                    .max_event_bytes,
            },
        },
        pool: PoolConfig {
            join: domain.is_some(),
            domain,
        },
    };
    Ok(format!(
        "# Pigeonpost loft configuration. CLI flags override these values.\n{}",
        toml::to_string_pretty(&config)?
    ))
}

fn caddyfile(domain: &str, bind: &str) -> String {
    format!(
        "# Install this with Caddy only after DNS points at this host.\n# Request/access logging stays disabled. The runtime logger is discarded because\n# debug and proxy failure records can contain client network addresses.\n{{\n\tadmin off\n\tlog default {{\n\t\toutput discard\n\t}}\n}}\n\n# Pigeonpost accepts one unambiguous RFC 7239 source chain and rejects X-Forwarded-For.\n{domain} {{\n\treverse_proxy http://{bind} {{\n\t\theader_up -X-Forwarded-For\n\t\theader_up Forwarded \"for=\\\"{{http.request.remote}}\\\"\"\n\t}}\n}}\n"
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn service_command_from_process() -> io::Result<ServiceCommand> {
    let current_exe = std::env::current_exe()?;
    let protocol = std::env::var_os(NPM_LAUNCHER_PROTOCOL_ENV);
    let node = std::env::var_os(NPM_LAUNCHER_NODE_ENV);
    let entry = std::env::var_os(NPM_LAUNCHER_ENTRY_ENV);
    service_command_from_handoff(
        &current_exe,
        protocol.as_deref(),
        node.as_deref(),
        entry.as_deref(),
    )
}

#[cfg(any(test, target_os = "linux", target_os = "macos"))]
fn service_command_from_handoff(
    current_exe: &Path,
    protocol: Option<&OsStr>,
    node: Option<&OsStr>,
    entry: Option<&OsStr>,
) -> io::Result<ServiceCommand> {
    match (protocol, node, entry) {
        (None, None, None) => Ok(ServiceCommand {
            program: checked_service_path(current_exe, "current executable", true, true)?,
            prefix_args: Vec::new(),
        }),
        (Some(protocol), Some(node), Some(entry))
            if protocol == OsStr::new(NPM_LAUNCHER_PROTOCOL_V1) =>
        {
            Ok(ServiceCommand {
                // Preserve these lexical paths rather than canonicalizing them into an npm store
                // or package target. npm may atomically replace their symlink targets on upgrade.
                program: checked_service_path(Path::new(node), "npm Node runtime", true, false)?,
                prefix_args: vec![checked_npm_launcher_entry(Path::new(entry))?],
            })
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "incomplete or unsupported npm launcher handoff; rerun from a clean Pigeonpost package invocation",
        )),
    }
}

#[cfg(any(test, target_os = "linux", target_os = "macos"))]
fn checked_npm_launcher_entry(path: &Path) -> io::Result<PathBuf> {
    let checked = checked_service_path(path, "npm launcher entrypoint", false, false)?;
    // Deliberate external-path exception: resolve npm's symlink only to reject disposable npx
    // caches, while persisting the stable lexical launcher name returned below.
    let resolved = fs::canonicalize(&checked)?;
    if is_npm_exec_cache_path(&checked) || is_npm_exec_cache_path(&resolved) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "service installation cannot persist a disposable npm-exec/npx cache launcher; install @bekirdag/pigeonpost globally and rerun pigeonpost install",
        ));
    }
    Ok(checked)
}

#[cfg(any(test, target_os = "linux", target_os = "macos"))]
fn is_npm_exec_cache_path(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|value| value.eq_ignore_ascii_case("_npx"))
    })
}

#[cfg(any(test, target_os = "linux", target_os = "macos"))]
fn checked_service_path(
    path: &Path,
    label: &str,
    executable: bool,
    canonicalize: bool,
) -> io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label} must be an absolute path"),
        ));
    }
    path_text(path, label)?;
    // Deliberate external-path exception: executables live outside Pigeonpost state custody. A
    // direct native service pins the resolved binary; npm handoffs validate the resolved target
    // but retain their stable lexical launcher paths so package upgrades keep working.
    let resolved = fs::canonicalize(path)?;
    let metadata = fs::metadata(&resolved)?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label} is not a regular file"),
        ));
    }
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{label} is not executable"),
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = executable;

    if canonicalize {
        path_text(&resolved, label)?;
        Ok(resolved)
    } else {
        Ok(path.to_path_buf())
    }
}

#[cfg(any(test, target_os = "linux", target_os = "macos"))]
fn path_text<'a>(path: &'a Path, label: &str) -> io::Result<&'a str> {
    let value = path.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label} is not valid UTF-8"),
        )
    })?;
    if value.chars().any(char::is_control) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label} contains a control character"),
        ));
    }
    Ok(value)
}

#[cfg(any(test, target_os = "linux", target_os = "macos"))]
fn service_argv(command: &ServiceCommand, dir: &Path) -> io::Result<Vec<String>> {
    let mut argv = Vec::with_capacity(command.prefix_args.len() + 5);
    argv.push(path_text(&command.program, "service program")?.to_owned());
    for argument in &command.prefix_args {
        argv.push(path_text(argument, "service launcher argument")?.to_owned());
    }
    argv.extend([
        "loft".to_owned(),
        "serve".to_owned(),
        "--dir".to_owned(),
        path_text(dir, "loft directory")?.to_owned(),
    ]);
    Ok(argv)
}

#[cfg(any(test, target_os = "linux"))]
fn systemd_unit(command: &ServiceCommand, dir: &Path) -> io::Result<String> {
    let exec_start = service_argv(command, dir)?
        .iter()
        .map(|argument| systemd_quote(argument))
        .collect::<io::Result<Vec<_>>>()?
        .join(" ");
    Ok(format!(
        "[Unit]\nDescription=Pigeonpost loft\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nExecStart={exec_start}\nRestart=on-failure\nRestartSec=5\nUMask=0077\nNoNewPrivileges=true\nPrivateTmp=true\n\n[Install]\nWantedBy=default.target\n"
    ))
}

#[cfg(any(test, target_os = "macos"))]
fn launchd_plist(command: &ServiceCommand, dir: &Path) -> io::Result<String> {
    let arguments = service_argv(command, dir)?
        .iter()
        .map(|argument| format!("    <string>{}</string>", xml_escape(argument)))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>dev.pigeonpost.loft</string>
  <key>ProgramArguments</key>
  <array>
{arguments}
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ProcessType</key><string>Background</string>
  <key>Umask</key><integer>63</integer>
</dict>
</plist>
"#
    ))
}

#[cfg(target_os = "linux")]
fn write_service_unit(dir: &Path) -> io::Result<Option<ServiceUnit>> {
    let command = service_command_from_process()?;
    let Some(home) = std::env::var_os("HOME") else {
        return Ok(None);
    };
    let unit_dir = PathBuf::from(home).join(".config/systemd/user");
    let unit_directory = prepare_service_unit_directory(&unit_dir)?;
    let path = unit_dir.join("pigeonpost-loft.service");
    let content = systemd_unit(&command, dir)?;
    write_service_unit_atomic(
        &unit_directory,
        "pigeonpost-loft.service",
        content.as_bytes(),
    )?;
    Ok(Some(ServiceUnit::Systemd(path)))
}

#[cfg(target_os = "macos")]
fn write_service_unit(dir: &Path) -> io::Result<Option<ServiceUnit>> {
    let command = service_command_from_process()?;
    let Some(home) = std::env::var_os("HOME") else {
        return Ok(None);
    };
    let unit_dir = PathBuf::from(home).join("Library/LaunchAgents");
    let unit_directory = prepare_service_unit_directory(&unit_dir)?;
    let path = unit_dir.join("dev.pigeonpost.loft.plist");
    let content = launchd_plist(&command, dir)?;
    write_service_unit_atomic(
        &unit_directory,
        "dev.pigeonpost.loft.plist",
        content.as_bytes(),
    )?;
    Ok(Some(ServiceUnit::Launchd(path)))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn prepare_service_unit_directory(path: &Path) -> io::Result<GuardedDir> {
    let normalized = NormalizedPath::new(path).map_err(install_custody_error)?;
    let directory = match GuardedDir::open_existing(normalized.as_path(), DirPolicy::trusted()) {
        Ok(directory) => directory,
        Err(CustodyError::NotFound) => {
            GuardedDir::create_private(normalized.as_path()).map_err(install_custody_error)?
        }
        Err(error) => return Err(install_custody_error(error)),
    };
    let metadata = directory.verify_live().map_err(install_custody_error)?;
    if metadata.uid != rustix::process::geteuid().as_raw() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "user service-unit directory must be owned by the current user",
        ));
    }
    directory.verify_named().map_err(install_custody_error)?;
    Ok(directory)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn write_service_unit_atomic(
    directory: &GuardedDir,
    destination: &str,
    bytes: &[u8],
) -> io::Result<()> {
    let byte_len = u64::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "service unit is too large"))?;
    if byte_len > MAX_INSTALL_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "service unit exceeds the 64 KiB limit",
        ));
    }
    let destination = LeafName::new(destination).map_err(install_custody_error)?;
    let policy = FilePolicy::private(MAX_INSTALL_FILE_BYTES);
    let existing = directory
        .open_file_optional(&destination, OpenAccess::ReadOnly, policy)
        .map_err(install_custody_error)?;
    let (temporary_name, mut temporary) = (0..128)
        .find_map(|sequence| {
            let name = LeafName::new(format!(
                ".{}.{}.{}.tmp",
                destination.as_os_str().to_string_lossy(),
                std::process::id(),
                sequence
            ));
            let name = match name {
                Ok(name) => name,
                Err(error) => return Some(Err(install_custody_error(error))),
            };
            match directory.create_file(&name, policy) {
                Ok(file) => Some(Ok((name, file))),
                Err(CustodyError::AlreadyExists) => None,
                Err(error) => Some(Err(install_custody_error(error))),
            }
        })
        .transpose()?
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "could not allocate a service-unit temporary file",
            )
        })?;
    let cleanup = directory
        .open_file(&temporary_name, OpenAccess::ReadOnly, policy)
        .map_err(install_custody_error)?;

    let result = (|| -> io::Result<()> {
        temporary.write_all(bytes)?;
        temporary.sync_all().map_err(install_custody_error)?;
        if temporary.metadata().map_err(install_custody_error)?.len != byte_len {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "service unit was not written completely",
            ));
        }
        temporary.verify_named().map_err(install_custody_error)?;
        if let Some(existing) = existing.as_ref() {
            existing.verify_named().map_err(install_custody_error)?;
        }
        let published = match existing {
            Some(_) => directory.rename_replace(temporary, directory, &destination),
            None => directory.publish_no_replace(temporary, directory, &destination),
        }
        .map_err(install_custody_error)?;
        published.verify_named().map_err(install_custody_error)?;
        if published.metadata().map_err(install_custody_error)?.len != byte_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "service unit changed length during publication",
            ));
        }
        directory.verify_named().map_err(install_custody_error)?;
        directory.sync().map_err(install_custody_error)
    })();
    if let Err(error) = result {
        if directory
            .entry_metadata(&temporary_name)
            .map_err(install_custody_error)?
            .is_some()
        {
            directory
                .unlink_file(cleanup)
                .map_err(install_custody_error)?;
        }
        directory.verify_named().map_err(install_custody_error)?;
        return Err(error);
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn install_custody_error(error: CustodyError) -> io::Error {
    match error {
        CustodyError::Io(error) => error,
        CustodyError::NotFound => io::Error::new(io::ErrorKind::NotFound, error),
        CustodyError::AlreadyExists => io::Error::new(io::ErrorKind::AlreadyExists, error),
        error => io::Error::new(io::ErrorKind::PermissionDenied, error),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn activate_service(unit: &ServiceUnit) -> Result<(), Box<dyn std::error::Error>> {
    match unit {
        #[cfg(target_os = "linux")]
        ServiceUnit::Systemd(_) => {
            for arguments in SYSTEMD_ACTIVATION_STEPS {
                command_ok(Command::new("systemctl").args(arguments.iter().copied()))?;
            }
        }
        #[cfg(target_os = "macos")]
        ServiceUnit::Launchd(path) => {
            let output = Command::new("id").arg("-u").output()?;
            if !output.status.success() {
                return Err("could not determine the launchd user domain".into());
            }
            let uid = String::from_utf8(output.stdout)?.trim().to_string();
            let domain = format!("gui/{uid}");
            let _ = Command::new("launchctl")
                .args(["bootout", &domain, "dev.pigeonpost.loft"])
                .status();
            command_ok(
                Command::new("launchctl")
                    .arg("bootstrap")
                    .arg(&domain)
                    .arg(path),
            )?;
            command_ok(Command::new("launchctl").args([
                "kickstart",
                "-k",
                &format!("{domain}/dev.pigeonpost.loft"),
            ]))?;
        }
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn command_ok(command: &mut Command) -> Result<(), Box<dyn std::error::Error>> {
    let program = command.get_program().to_string_lossy().into_owned();
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} failed with {status}").into())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn wait_until_ready(bind: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut address: SocketAddr = bind.parse()?;
    if address.ip().is_unspecified() {
        address.set_ip(match address {
            SocketAddr::V4(_) => IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            SocketAddr::V6(_) => IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
        });
    }
    for _ in 0..30 {
        if readiness_probe(address).unwrap_or(false) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err("loft service did not become ready within six seconds".into())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn readiness_probe(address: SocketAddr) -> io::Result<bool> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(250))?;
    stream.set_read_timeout(Some(Duration::from_millis(250)))?;
    stream.set_write_timeout(Some(Duration::from_millis(250)))?;
    stream.write_all(b"GET /ready HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
    let mut response = [0_u8; 1024];
    let read = stream.read(&mut response)?;
    let response = String::from_utf8_lossy(&response[..read]);
    Ok(response.starts_with("HTTP/1.1 200") && response.contains("\r\n\r\nready"))
}

#[cfg(any(test, target_os = "linux"))]
fn systemd_quote(value: &str) -> io::Result<String> {
    if value.chars().any(char::is_control) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "service argument contains a control character",
        ));
    }
    Ok(format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('$', "$$")
            .replace('%', "%%")
    ))
}

#[cfg(any(test, target_os = "macos"))]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_test_program(path: &Path) {
        fs::write(path, b"test program").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(path).unwrap().permissions();
            permissions.set_mode(0o700);
            fs::set_permissions(path, permissions).unwrap();
        }
    }

    #[test]
    fn the_default_capacity_is_bounded() {
        let dir = crate::test_support::private_tempdir();
        let capacity = default_capacity_gb(dir.path()).unwrap();
        assert!((1..=MAX_DEFAULT_CAPACITY_GB).contains(&capacity));
        assert_eq!(capacity_for_free_disk_gb(0), 1);
        assert_eq!(capacity_for_free_disk_gb(4), 1);
        assert_eq!(capacity_for_free_disk_gb(10), 2);
        assert_eq!(capacity_for_free_disk_gb(50), 10);
        assert_eq!(capacity_for_free_disk_gb(100), 20);
        assert_eq!(capacity_for_free_disk_gb(u64::MAX), 20);
    }

    #[test]
    fn install_guidance_names_the_capacity_override_and_operator_duties() {
        assert!(CAPACITY_OVERRIDE_HINT.contains("--capacity-gb"));
        assert!(JURISDICTION_NOTICE.contains("jurisdiction-dependent"));
        assert!(JURISDICTION_NOTICE.contains("operator-specific legal review"));
        assert!(JURISDICTION_NOTICE.contains("docs/node.md"));
        assert!(JURISDICTION_NOTICE.contains("docs/law.md"));
    }

    #[test]
    fn generated_config_round_trips_through_the_runtime_schema() {
        let dir = crate::test_support::private_tempdir();
        let storage = dir.path().join("data/loft.db");
        let text = config_toml(&storage, 20, 30, "127.0.0.1:7717", None).unwrap();
        let decoded: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(decoded["loft"]["capacity_gb"].as_integer(), Some(20));
        assert_eq!(decoded["pool"]["join"].as_bool(), Some(false));
    }

    #[test]
    fn public_mode_is_loopback_only_and_generates_https_proxy_config() {
        assert!(validate_bind("0.0.0.0:7717", true).is_err());
        let domain = validate_domain("Loft.Example.com.").unwrap();
        assert_eq!(domain, "loft.example.com");
        let proxy = caddyfile(&domain, "127.0.0.1:7717");
        assert!(proxy.contains("loft.example.com"));
        assert!(proxy.contains("reverse_proxy http://127.0.0.1:7717"));
        assert!(proxy.contains("admin off"));
        assert!(proxy.contains("log default {\n\t\toutput discard"));
        assert_eq!(
            proxy
                .lines()
                .filter(|line| line.trim_start().starts_with("log "))
                .count(),
            1,
            "the site block must not enable request/access logging"
        );
        assert!(!proxy.lines().any(|line| line.trim() == "debug"));
        assert!(proxy.contains("header_up -X-Forwarded-For"));
        assert!(proxy.contains("header_up Forwarded \"for=\\\"{http.request.remote}\\\"\""));

        let config = config_toml(
            Path::new("data/loft.db"),
            20,
            30,
            "127.0.0.1:7717",
            Some(&domain),
        )
        .unwrap();
        let decoded: toml::Value = toml::from_str(&config).unwrap();
        assert_eq!(
            decoded["loft"]["trusted_proxies"][0].as_str(),
            Some("127.0.0.1")
        );
    }

    #[test]
    fn public_install_requires_the_two_phase_no_service_flow() {
        let dir = crate::test_support::private_tempdir();
        let target = dir.path().join("public-loft");
        let error = run(InstallOptions {
            dir: target.clone(),
            domain: Some("loft.example".into()),
            capacity_gb: Some(20),
            retention_days: 30,
            bind: None,
            no_service: false,
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("requires --no-service"));
        assert!(!target.exists());
        assert!(!target.join("loft.key").exists());
        assert!(!target.join("loft.toml").exists());
    }

    #[test]
    fn malicious_domain_and_service_values_are_rejected_or_escaped() {
        assert!(validate_domain("evil.example\nExecStart=/bin/sh").is_err());
        for domain in ["localhost", "localhost.", "api.localhost", "API.LOCALHOST."] {
            assert!(validate_domain(domain).is_err(), "accepted {domain}");
        }
        assert!(systemd_quote("bad\npath").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn install_hardens_owned_preexisting_loft_and_storage_directories() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let temp = crate::test_support::private_tempdir();
        let target = temp.path().join("loft");
        let storage = target.join("data");
        fs::create_dir_all(&storage).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&storage, fs::Permissions::from_mode(0o755)).unwrap();

        run(InstallOptions {
            dir: target.clone(),
            domain: None,
            capacity_gb: Some(1),
            retention_days: 1,
            bind: None,
            no_service: true,
        })
        .unwrap();

        for directory in [&target, &storage] {
            assert_eq!(fs::metadata(directory).unwrap().mode() & 0o777, 0o700);
        }
        for file in [target.join("loft.key"), target.join("loft.toml")] {
            let metadata = fs::metadata(file).unwrap();
            assert_eq!(metadata.mode() & 0o777, 0o600);
            assert_eq!(metadata.nlink(), 1);
        }
    }

    #[cfg(unix)]
    #[test]
    fn install_refuses_a_preexisting_directory_symlink() {
        use std::os::unix::fs::symlink;

        let temp = crate::test_support::private_tempdir();
        let actual = temp.path().join("actual");
        fs::create_dir(&actual).unwrap();
        let linked = temp.path().join("linked");
        symlink(&actual, &linked).unwrap();

        assert!(prepare_directory(&linked).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn install_rejects_mutable_roots_and_ancestors_without_hardening_or_creation() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let temp = crate::test_support::private_tempdir();
        let mutable_root = temp.path().join("mutable-root");
        fs::create_dir(&mutable_root).unwrap();
        fs::set_permissions(&mutable_root, fs::Permissions::from_mode(0o770)).unwrap();
        fs::write(mutable_root.join("sentinel"), b"unchanged").unwrap();

        assert!(prepare_directory(&mutable_root).is_err());
        assert_eq!(fs::metadata(&mutable_root).unwrap().mode() & 0o7777, 0o770);
        assert_eq!(
            fs::read(mutable_root.join("sentinel")).unwrap(),
            b"unchanged"
        );

        let mutable_ancestor = temp.path().join("mutable-ancestor");
        fs::create_dir(&mutable_ancestor).unwrap();
        fs::set_permissions(&mutable_ancestor, fs::Permissions::from_mode(0o777)).unwrap();
        let attempted = mutable_ancestor.join("loft");
        assert!(prepare_directory(&attempted).is_err());
        assert!(!attempted.exists());
    }

    #[test]
    fn npm_service_uses_the_stable_launcher_across_native_cache_versions() {
        let temp = crate::test_support::private_tempdir();
        let node = temp.path().join("node runtime");
        let launcher = temp.path().join("stable pigeonpost launcher.js");
        write_test_program(&node);
        fs::write(&launcher, b"launcher v1").unwrap();
        let v1_cache = temp.path().join("cache/0.1.0/pigeonpost");
        let v2_cache = temp.path().join("cache/0.2.0/pigeonpost");
        let loft_dir = temp.path().join("loft data");

        let v1 = service_command_from_handoff(
            &v1_cache,
            Some(OsStr::new(NPM_LAUNCHER_PROTOCOL_V1)),
            Some(node.as_os_str()),
            Some(launcher.as_os_str()),
        )
        .unwrap();
        let first_systemd = systemd_unit(&v1, &loft_dir).unwrap();
        let first_launchd = launchd_plist(&v1, &loft_dir).unwrap();

        // npm replaces the package at the same stable entrypoint. A new versioned native cache
        // path must not change either service definition, and deleting that cache cannot break the
        // command because the launcher will fetch and verify it again.
        fs::write(&launcher, b"launcher v2").unwrap();
        let v2 = service_command_from_handoff(
            &v2_cache,
            Some(OsStr::new(NPM_LAUNCHER_PROTOCOL_V1)),
            Some(node.as_os_str()),
            Some(launcher.as_os_str()),
        )
        .unwrap();
        assert_eq!(v1, v2);
        assert_eq!(systemd_unit(&v2, &loft_dir).unwrap(), first_systemd);
        assert_eq!(launchd_plist(&v2, &loft_dir).unwrap(), first_launchd);
        assert!(first_systemd.contains(&systemd_quote(path_text(&node, "node").unwrap()).unwrap()));
        assert!(first_systemd
            .contains(&systemd_quote(path_text(&launcher, "launcher").unwrap()).unwrap()));
        assert!(!first_systemd.contains("cache/0.1.0"));
        assert!(!first_systemd.contains("cache/0.2.0"));
        assert!(first_launchd.contains(&xml_escape(path_text(&launcher, "launcher").unwrap())));
        assert!(first_launchd.contains("<key>Umask</key><integer>63</integer>"));
    }

    #[test]
    fn global_npm_launcher_is_accepted_for_service_installation() {
        let temp = crate::test_support::private_tempdir();
        let node = temp.path().join("usr/local/bin/node");
        let launcher = temp
            .path()
            .join("usr/local/lib/node_modules/@bekirdag/pigeonpost/bin/pigeonpost.js");
        fs::create_dir_all(node.parent().unwrap()).unwrap();
        fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        write_test_program(&node);
        fs::write(&launcher, b"global launcher").unwrap();

        let command = service_command_from_handoff(
            &temp.path().join("cache/0.2.0/pigeonpost"),
            Some(OsStr::new(NPM_LAUNCHER_PROTOCOL_V1)),
            Some(node.as_os_str()),
            Some(launcher.as_os_str()),
        )
        .unwrap();

        assert_eq!(command.program, node);
        assert_eq!(command.prefix_args, vec![launcher]);
    }

    #[test]
    fn npm_exec_cache_launcher_is_rejected_for_service_installation() {
        let temp = crate::test_support::private_tempdir();
        let node = temp.path().join("node");
        let launcher = temp
            .path()
            .join(".npm/_npx/deadbeef/node_modules/@bekirdag/pigeonpost/bin/pigeonpost.js");
        fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        write_test_program(&node);
        fs::write(&launcher, b"disposable launcher").unwrap();

        let error = service_command_from_handoff(
            &temp.path().join("cache/0.2.0/pigeonpost"),
            Some(OsStr::new(NPM_LAUNCHER_PROTOCOL_V1)),
            Some(node.as_os_str()),
            Some(launcher.as_os_str()),
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error
            .to_string()
            .contains("install @bekirdag/pigeonpost globally"));
        assert!(!is_npm_exec_cache_path(Path::new(
            "/opt/npm/_npx-tools/pigeonpost.js"
        )));
    }

    #[test]
    fn direct_native_install_pins_the_exact_non_cache_executable() {
        let temp = crate::test_support::private_tempdir();
        let executable = temp.path().join("pigeonpost");
        write_test_program(&executable);

        let command = service_command_from_handoff(&executable, None, None, None).unwrap();
        assert_eq!(command.program, fs::canonicalize(executable).unwrap());
        assert!(command.prefix_args.is_empty());
    }

    #[test]
    fn malformed_launcher_handoffs_fail_closed() {
        let temp = crate::test_support::private_tempdir();
        let executable = temp.path().join("pigeonpost");
        write_test_program(&executable);

        let error = service_command_from_handoff(
            &executable,
            Some(OsStr::new(NPM_LAUNCHER_PROTOCOL_V1)),
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

        let error = service_command_from_handoff(
            &executable,
            Some(OsStr::new("future-protocol")),
            Some(executable.as_os_str()),
            Some(executable.as_os_str()),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn service_renderers_escape_manager_metacharacters() {
        assert_eq!(
            systemd_quote(r#"/tmp/a$b%c\d"e"#).unwrap(),
            r#""/tmp/a$$b%%c\\d\"e""#
        );
        assert_eq!(xml_escape("<node>&\"'"), "&lt;node&gt;&amp;&quot;&apos;");
    }

    #[test]
    fn systemd_activation_reloads_enables_and_restarts_in_that_order() {
        assert_eq!(
            SYSTEMD_ACTIVATION_STEPS,
            &[
                &["--user", "daemon-reload"][..],
                &["--user", "enable", "pigeonpost-loft.service"][..],
                &["--user", "restart", "pigeonpost-loft.service"][..],
            ]
        );
        assert!(!SYSTEMD_ACTIVATION_STEPS
            .iter()
            .flat_map(|arguments| arguments.iter())
            .any(|arg| *arg == "--now"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_service_mode_fails_before_writing_files() {
        let dir = crate::test_support::private_tempdir();
        let target = dir.path().join("windows-loft");
        let error = run(InstallOptions {
            dir: target.clone(),
            domain: None,
            capacity_gb: Some(20),
            retention_days: 30,
            bind: None,
            no_service: false,
        })
        .unwrap_err()
        .to_string();

        assert!(error.contains("service mode is not supported on windows"));
        assert!(!target.exists());
    }
}
