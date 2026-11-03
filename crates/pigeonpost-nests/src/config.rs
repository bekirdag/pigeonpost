//! `nestd.toml`: one file for the control service and the broker. Secrets are never in it; it
//! names files that hold them (or their hashes), each readable only by the service that needs it.

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// SQLite ledger shared by the control service and the broker (WAL mode).
    pub db: PathBuf,
    pub control: ControlConfig,
    pub sandbox: SandboxConfig,
    pub storage: StorageConfig,
    pub broker: BrokerConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlConfig {
    /// Private address only: the WireGuard address facing the postbox host. Never a public one.
    pub bind: String,
    /// File holding the SHA-256 (hex) of the postbox's bearer token. The token itself lives only on
    /// the postbox host.
    pub token_sha256_file: PathBuf,
    /// Stop a running Nest after this long without model traffic. 0 disables idle stop.
    #[serde(default = "default_idle")]
    pub idle_stop_secs: u64,
    /// Admission: at most this many Nests running at once on this host.
    #[serde(default = "default_max_running")]
    pub max_running: usize,
    /// Reconcile the host against the ledger this often (and once at start).
    #[serde(default = "default_reconcile")]
    pub reconcile_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxConfig {
    /// `gvisor` (podman + runsc) in production; `fake` only in tests.
    pub runtime: String,
    /// The podman runtime name configured in containers.conf (a wrapper that fixes runsc's flags).
    #[serde(default = "default_oci_runtime")]
    pub oci_runtime: String,
    /// The runner image, pinned by digest (`name@sha256:…`). Anything else is refused at start.
    pub image: String,
    /// The podman network every Nest joins (bridge `nests0`, egress policed by nftables).
    pub network: String,
    pub memory_mib: u64,
    /// cgroup v2 CPU weight (1–10000). Shares the host fairly; no core is pinned to a Nest.
    #[serde(default = "default_cpu_weight")]
    pub cpu_weight: u64,
    /// CPU bandwidth cap in thousandths of one core (1000 = one full core).
    #[serde(default = "default_cpu_millis")]
    pub cpu_max_millis: u64,
    #[serde(default = "default_pids")]
    pub pids: u64,
    /// What the sandbox calls the postbox, and the address that name is pinned to (the sandbox has
    /// no DNS; nftables allows only this address).
    pub postbox_host: String,
    pub postbox_ip: String,
    pub postbox_url: String,
    /// The broker's service address, reachable from the sandbox on one port.
    pub broker_ip: String,
    pub broker_port: u16,
    /// Size limit of the sandbox's private /tmp.
    #[serde(default = "default_tmp_mib")]
    pub tmp_mib: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    /// `lvm-luks` in production; `dir` for tests and development (no quota, no encryption).
    pub backend: String,
    #[serde(default = "default_vg")]
    pub vg: String,
    #[serde(default = "default_pool")]
    pub pool: String,
    /// Where each Nest's volume is mounted on the host, `<mount_root>/<nest_id>`.
    pub mount_root: PathBuf,
    /// One LUKS key file per Nest, root-only. Deleting it is the crypto-erase.
    pub key_dir: PathBuf,
    /// Volume size of a new Nest (its disk quota).
    pub default_gib: u64,
    /// Admission: the sum of all Nest volume sizes may not exceed this (no thin overcommit).
    pub max_total_gib: u64,
    /// The uid/gid the sandbox runs as; the volume's top directories are owned by it.
    #[serde(default = "default_uid")]
    pub sandbox_uid: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerConfig {
    /// The host's service address on the dummy interface, e.g. `10.67.255.1:8788`.
    pub bind: String,
    /// OpenAI-compatible chat completions URL of the upstream (mswarm's Pigeonpost tenant).
    pub upstream_url: String,
    /// File holding the upstream API key. Read by the broker only; never sent to a sandbox.
    pub upstream_key_file: PathBuf,
    /// The one model the broker requests. A sandbox cannot choose another.
    pub model: String,
    /// Hard caps per request.
    pub max_input_tokens: u64,
    pub max_output_tokens: u64,
    /// Upstream timeout. A timeout after the request was sent is "outcome unknown", not zero cost.
    #[serde(default = "default_upstream_timeout")]
    pub upstream_timeout_secs: u64,
}

fn default_idle() -> u64 {
    900
}
fn default_max_running() -> usize {
    8
}
fn default_reconcile() -> u64 {
    60
}
fn default_oci_runtime() -> String {
    "runsc-nests".into()
}
fn default_cpu_weight() -> u64 {
    100
}
fn default_cpu_millis() -> u64 {
    1000
}
fn default_pids() -> u64 {
    128
}
fn default_tmp_mib() -> u64 {
    64
}
fn default_vg() -> String {
    "nests".into()
}
fn default_pool() -> String {
    "pool".into()
}
fn default_uid() -> u32 {
    10001
}
fn default_upstream_timeout() -> u64 {
    120
}

impl Config {
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let config: Config =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        config.check()?;
        Ok(config)
    }

    /// Refuse configurations that would quietly weaken the sandbox.
    pub fn check(&self) -> Result<(), String> {
        if self.sandbox.runtime == "gvisor" && !self.sandbox.image.contains("@sha256:") {
            return Err("sandbox.image must be pinned by digest (name@sha256:…)".into());
        }
        if !(1..=10_000).contains(&self.sandbox.cpu_weight) {
            return Err("sandbox.cpu_weight must be 1–10000".into());
        }
        if self.sandbox.memory_mib < 128 {
            return Err("sandbox.memory_mib below 128 cannot run the runner".into());
        }
        if self.storage.default_gib == 0 || self.storage.max_total_gib < self.storage.default_gib {
            return Err("storage sizes are inconsistent".into());
        }
        if self.broker.max_output_tokens == 0 || self.broker.max_input_tokens == 0 {
            return Err("broker token caps must be positive".into());
        }
        // The control API must never listen on every interface.
        for bind in [&self.control.bind, &self.broker.bind] {
            if bind.starts_with("0.0.0.0") || bind.starts_with("[::]") {
                return Err(format!(
                    "{bind}: bind a specific private address, not every interface"
                ));
            }
        }
        Ok(())
    }
}

/// Read a one-line secret (or hash) file, trimmed. Refuses group/world-readable files on Unix.
pub fn read_secret_file(path: &std::path::Path) -> Result<String, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta =
            std::fs::metadata(path).map_err(|e| format!("cannot stat {}: {e}", path.display()))?;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(format!(
                "{} is readable by group or others; chmod 600 it",
                path.display()
            ));
        }
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let value = text.trim().to_string();
    if value.is_empty() {
        return Err(format!("{} is empty", path.display()));
    }
    Ok(value)
}

#[cfg(test)]
pub(crate) fn test_config(dir: &std::path::Path) -> Config {
    Config {
        db: dir.join("nestd.db"),
        control: ControlConfig {
            bind: "127.0.0.1:0".into(),
            token_sha256_file: dir.join("control.sha256"),
            idle_stop_secs: 900,
            max_running: 2,
            reconcile_secs: 60,
        },
        sandbox: SandboxConfig {
            runtime: "fake".into(),
            oci_runtime: "runsc-nests".into(),
            image: "localhost/pigeonpost-nest-runner@sha256:00".into(),
            network: "nests".into(),
            memory_mib: 512,
            cpu_weight: 100,
            cpu_max_millis: 1000,
            pids: 128,
            postbox_host: "postbox.pigeonpost.dev".into(),
            postbox_ip: "45.132.181.100".into(),
            postbox_url: "https://postbox.pigeonpost.dev".into(),
            broker_ip: "10.67.255.1".into(),
            broker_port: 8788,
            tmp_mib: 64,
        },
        storage: StorageConfig {
            backend: "dir".into(),
            vg: "nests".into(),
            pool: "pool".into(),
            mount_root: dir.join("mnt"),
            key_dir: dir.join("keys"),
            default_gib: 1,
            max_total_gib: 3,
            sandbox_uid: 10001,
        },
        broker: BrokerConfig {
            bind: "127.0.0.1:0".into(),
            upstream_url: "http://127.0.0.1:9/v1/chat/completions".into(),
            upstream_key_file: dir.join("upstream.key"),
            model: "deepseek/test".into(),
            max_input_tokens: 4000,
            max_output_tokens: 500,
            upstream_timeout_secs: 5,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpinned_images_and_wildcard_binds_are_refused() {
        let dir = std::env::temp_dir();
        let mut c = test_config(&dir);
        c.sandbox.runtime = "gvisor".into();
        c.sandbox.image = "localhost/pigeonpost-nest-runner:latest".into();
        assert!(c.check().is_err());
        c.sandbox.image = "localhost/x@sha256:ab".into();
        assert!(c.check().is_ok());
        c.control.bind = "0.0.0.0:8790".into();
        assert!(c.check().is_err());
    }
}
