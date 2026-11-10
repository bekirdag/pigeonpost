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
    /// Phase B6: where `pigeonpost-nestd alert-relay` sends Alertmanager notifications.
    #[serde(default)]
    pub alerts: Option<AlertsConfig>,
}

/// Alertmanager's webhook, relayed as a Pigeonpost message to the operator.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertsConfig {
    /// Loopback address Alertmanager posts to, e.g. `127.0.0.1:9796`.
    pub bind: String,
    /// The postbox's public base URL.
    pub postbox_url: String,
    /// File holding the capability token of the dedicated alerts mailbox (0400 root).
    pub token_file: PathBuf,
    /// The operator's Pigeonpost address.
    pub to: String,
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
    /// Phase B6: Prometheus metrics on a loopback address (`127.0.0.1:9790`); unset = off.
    #[serde(default)]
    pub metrics_bind: Option<String>,
    /// File whose content is the Unix time of the last successful backup (written by
    /// `nests-backup`), exported as `nests_backup_last_success_seconds`.
    #[serde(default)]
    pub backup_marker: Option<PathBuf>,
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
    /// The marketplace pool (build plan M0): a separate, stricter configuration on this host. Off
    /// when absent.
    #[serde(default)]
    pub marketplace: Option<MarketplacePoolConfig>,
}

/// `[sandbox.marketplace]`: one sandbox per buyer context, no shared mounts, network only to the
/// broker. gVisor on this host, because it has no KVM (owner decision 2026-11-09).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketplacePoolConfig {
    /// The publisher image, pinned by digest.
    pub image: String,
    /// A podman network of its own: never the first-party one.
    pub network: String,
    /// Each buyer context's volumes live under `<volume_root>/<context>/<nest>`. A sibling of
    /// `storage.mount_root`, never inside it.
    pub volume_root: PathBuf,
    /// The broker address reachable from this pool's bridge, on one port.
    pub broker_ip: String,
    pub broker_port: u16,
    pub memory_mib: u64,
    #[serde(default = "default_cpu_weight")]
    pub cpu_weight: u64,
    #[serde(default = "default_cpu_millis")]
    pub cpu_max_millis: u64,
    pub pids: u64,
    pub tmp_mib: u64,
    /// Hardening (W1.1): the postbox attestation public keys this host trusts, 64 hex digits
    /// each. Every admission and every dispatch must verify under one of them; more than one is
    /// a key rotation in progress. Required outside tests.
    #[serde(default)]
    pub attestation_keys: Vec<String>,
    /// Hardening (W1.2): size of each worker's encrypted volume. Workers count against
    /// `storage.max_total_gib` like any Nest.
    #[serde(default = "default_worker_gib")]
    pub worker_gib: u64,
    /// Hardening (W1.3): where release images this host does not have are pulled from (the
    /// build plane's artifact API). Unset: images must already be on the host.
    #[serde(default)]
    pub artifacts: Option<ArtifactsConfig>,
    /// Marketplace M2b: a staging postbox's **test pool** for hosted test Nests. It admits only
    /// test statements (`pigeonpost.nest.release.test.v1`) and refuses production attestations;
    /// a production pool (the default) does the opposite.
    #[serde(default)]
    pub test_pool: bool,
}

/// `[sandbox.marketplace.artifacts]`: the build plane's read-only artifact API over WireGuard.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactsConfig {
    /// `http://<build host WireGuard address>:<port>`: plain HTTP only to a private address.
    pub url: String,
    /// File holding the artifact-read token (0400 root). It can read images and nothing else.
    pub token_file: PathBuf,
    /// Scratch directory for downloaded archives, root-only; each is removed after loading.
    pub cache_dir: PathBuf,
    /// Largest image archive accepted.
    #[serde(default = "default_artifact_bytes")]
    pub max_bytes: u64,
}

fn default_worker_gib() -> u64 {
    1
}

fn default_artifact_bytes() -> u64 {
    4 * 1024 * 1024 * 1024
}

/// `https://…`, or plain `http://` only to loopback or an RFC 1918 IPv4 literal (a WireGuard
/// address: the tunnel encrypts the hop).
pub fn private_url_ok(url: &str) -> bool {
    if let Some(rest) = url.strip_prefix("https://") {
        return !rest.is_empty();
    }
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let host = rest
        .split('/')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");
    host.parse::<std::net::Ipv4Addr>()
        .is_ok_and(|ip| ip.is_loopback() || ip.is_private())
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
    /// Phase B6: store volume keys wrapped under the KEK the postbox releases (`POST /v1/unlock`).
    /// On by default; `false` only for development hosts.
    #[serde(default = "default_true")]
    pub key_wrap: bool,
}

fn default_true() -> bool {
    true
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
    /// Operator spend caps (monthly tokens). A Nest's allowance is the lower of what the postbox
    /// granted and `nest_monthly_*_cap`; all Nests together may not exceed `global_monthly_*_cap`
    /// (counting reservations in flight and unknown-outcome requests). Unset means no cap.
    #[serde(default)]
    pub nest_monthly_input_cap: Option<u64>,
    #[serde(default)]
    pub nest_monthly_output_cap: Option<u64>,
    #[serde(default)]
    pub global_monthly_input_cap: Option<u64>,
    #[serde(default)]
    pub global_monthly_output_cap: Option<u64>,
}

impl BrokerConfig {
    pub fn caps(&self) -> crate::db::Caps {
        let c = |v: Option<u64>| v.map(|n| i64::try_from(n).unwrap_or(i64::MAX));
        crate::db::Caps {
            nest_input: c(self.nest_monthly_input_cap),
            nest_output: c(self.nest_monthly_output_cap),
            global_input: c(self.global_monthly_input_cap),
            global_output: c(self.global_monthly_output_cap),
        }
    }
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
        if let Some(pool) = &self.sandbox.marketplace {
            self.check_marketplace(pool)?;
        }
        if self.storage.default_gib == 0 || self.storage.max_total_gib < self.storage.default_gib {
            return Err("storage sizes are inconsistent".into());
        }
        if self.broker.max_output_tokens == 0 || self.broker.max_input_tokens == 0 {
            return Err("broker token caps must be positive".into());
        }
        for cap in [
            self.broker.nest_monthly_input_cap,
            self.broker.nest_monthly_output_cap,
            self.broker.global_monthly_input_cap,
            self.broker.global_monthly_output_cap,
        ]
        .into_iter()
        .flatten()
        {
            if cap == 0 {
                return Err("broker monthly caps must be positive (omit one for no cap)".into());
            }
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

    /// The marketplace pool may only be stricter than the first-party one, and may share nothing
    /// with it that a misconfiguration could turn into a shared mount or a shared network.
    fn check_marketplace(&self, pool: &MarketplacePoolConfig) -> Result<(), String> {
        if !pool.image.contains("@sha256:") {
            return Err(
                "sandbox.marketplace.image must be pinned by digest (name@sha256:…)".into(),
            );
        }
        if pool.image == self.sandbox.image {
            return Err("sandbox.marketplace.image must differ from the first-party image".into());
        }
        if pool.network == self.sandbox.network {
            return Err(
                "sandbox.marketplace.network must differ from the first-party network".into(),
            );
        }
        if !self.sandbox.oci_runtime.starts_with("runsc") && self.sandbox.runtime != "fake" {
            return Err("the marketplace pool runs under gVisor only".into());
        }
        let (root, mounts) = (&pool.volume_root, &self.storage.mount_root);
        if root == mounts || root.starts_with(mounts) || mounts.starts_with(root) {
            return Err(
                "sandbox.marketplace.volume_root must not overlap storage.mount_root".into(),
            );
        }
        if !root.is_absolute() {
            return Err("sandbox.marketplace.volume_root must be absolute".into());
        }
        if !(1..=10_000).contains(&pool.cpu_weight) || pool.cpu_max_millis == 0 {
            return Err("sandbox.marketplace CPU limits are out of range".into());
        }
        if pool.memory_mib == 0 || pool.memory_mib > self.sandbox.memory_mib {
            return Err(
                "sandbox.marketplace.memory_mib must be positive and at most the first-party limit"
                    .into(),
            );
        }
        if pool.pids == 0 || pool.pids > self.sandbox.pids {
            return Err(
                "sandbox.marketplace.pids must be positive and at most the first-party limit"
                    .into(),
            );
        }
        if pool.cpu_max_millis > self.sandbox.cpu_max_millis {
            return Err(
                "sandbox.marketplace.cpu_max_millis must be at most the first-party limit".into(),
            );
        }
        // Hardening: a production pool runs only signed attestations, from encrypted volumes.
        crate::attest::TrustedKeys::from_hex(&pool.attestation_keys)
            .map_err(|e| format!("sandbox.marketplace.attestation_keys: {e}"))?;
        if self.sandbox.runtime != "fake" {
            if pool.attestation_keys.is_empty() {
                return Err("sandbox.marketplace.attestation_keys: at least one key".into());
            }
            if self.storage.backend != "lvm-luks" {
                return Err("the marketplace pool needs storage.backend = \"lvm-luks\"".into());
            }
        }
        if pool.worker_gib == 0 || pool.worker_gib > 64 {
            return Err("sandbox.marketplace.worker_gib must be 1-64".into());
        }
        if let Some(a) = &pool.artifacts {
            if !private_url_ok(&a.url) {
                return Err(
                    "sandbox.marketplace.artifacts.url: https, or http to a private address".into(),
                );
            }
            if !a.cache_dir.is_absolute() || a.max_bytes == 0 {
                return Err(
                    "sandbox.marketplace.artifacts: absolute cache_dir, positive max_bytes".into(),
                );
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
        // Group read is allowed: systemd's LoadCredential hands a service its credential 0440,
        // owned by the service's own user and group.
        if meta.permissions().mode() & 0o037 != 0 {
            return Err(format!(
                "{} is writable by group or readable by others; chmod 600 it",
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
pub(crate) fn test_marketplace_pool(dir: &std::path::Path) -> MarketplacePoolConfig {
    MarketplacePoolConfig {
        image: "localhost/pigeonpost-marketplace-worker@sha256:11".into(),
        network: "nests-mp".into(),
        volume_root: dir.join("mp"),
        broker_ip: "10.67.255.1".into(),
        broker_port: 8788,
        memory_mib: 256,
        cpu_weight: 100,
        cpu_max_millis: 500,
        pids: 64,
        tmp_mib: 32,
        attestation_keys: vec![crate::util::hex(
            crate::attest::test_key().verifying_key().as_bytes(),
        )],
        worker_gib: 1,
        artifacts: None,
        test_pool: false,
    }
}

#[cfg(test)]
pub(crate) fn test_config(dir: &std::path::Path) -> Config {
    Config {
        db: dir.join("nestd.db"),
        control: ControlConfig {
            bind: "127.0.0.1:0".into(),
            token_sha256_file: dir.join("control.sha256"),
            metrics_bind: None,
            backup_marker: None,
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
            marketplace: None,
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
            key_wrap: false,
        },
        broker: BrokerConfig {
            bind: "127.0.0.1:0".into(),
            upstream_url: "http://127.0.0.1:9/v1/chat/completions".into(),
            upstream_key_file: dir.join("upstream.key"),
            model: "deepseek/test".into(),
            max_input_tokens: 4000,
            max_output_tokens: 500,
            upstream_timeout_secs: 5,
            nest_monthly_input_cap: None,
            nest_monthly_output_cap: None,
            global_monthly_input_cap: None,
            global_monthly_output_cap: None,
        },
        alerts: None,
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

    #[test]
    fn the_marketplace_pool_must_be_stricter_and_share_nothing() {
        let dir = std::env::temp_dir();
        let ok = || {
            let mut c = test_config(&dir);
            c.sandbox.runtime = "gvisor".into();
            c.sandbox.image = "localhost/x@sha256:ab".into();
            c.storage.backend = "lvm-luks".into();
            c.sandbox.marketplace = Some(test_marketplace_pool(&dir));
            c
        };
        assert!(ok().check().is_ok());
        type Tweak = fn(&mut MarketplacePoolConfig);
        let cases: Vec<(&str, Tweak)> = vec![
            ("unpinned image", |p| p.image = "localhost/x:latest".into()),
            ("first-party image", |p| {
                p.image = "localhost/x@sha256:ab".into()
            }),
            ("first-party network", |p| p.network = "nests".into()),
            ("relative volume root", |p| {
                p.volume_root = "mp".into();
            }),
            ("memory above first-party", |p| p.memory_mib = 1024),
            ("pids above first-party", |p| p.pids = 1024),
            ("cpu above first-party", |p| p.cpu_max_millis = 2000),
            ("zero memory", |p| p.memory_mib = 0),
            ("zero pids", |p| p.pids = 0),
            ("no attestation key", |p| p.attestation_keys.clear()),
            ("a malformed attestation key", |p| {
                p.attestation_keys.push("not-a-key".into())
            }),
            ("a zero worker volume", |p| p.worker_gib = 0),
            ("a public plain-HTTP artifact source", |p| {
                p.artifacts = Some(ArtifactsConfig {
                    url: "http://203.0.113.9:8795".into(),
                    token_file: "/etc/nests/artifacts.token".into(),
                    cache_dir: "/var/lib/nests/artifacts".into(),
                    max_bytes: 1,
                })
            }),
        ];
        for (what, change) in cases {
            let mut c = ok();
            change(c.sandbox.marketplace.as_mut().unwrap());
            assert!(c.check().is_err(), "{what} must be refused");
        }
        // A volume root that overlaps the first-party mounts, either way round.
        for volume_root in [dir.join("mnt"), dir.join("mnt/mp"), dir.clone()] {
            let mut c = ok();
            c.sandbox.marketplace.as_mut().unwrap().volume_root = volume_root;
            assert!(c.check().is_err());
        }
        // A runtime other than gVisor is refused for this pool.
        let mut c = ok();
        c.sandbox.oci_runtime = "crun".into();
        assert!(c.check().is_err());
        // Worker volumes are encrypted in production: plain directories are refused.
        let mut c = ok();
        c.storage.backend = "dir".into();
        assert!(c.check().is_err());
        // A WireGuard artifact source is fine.
        let mut c = ok();
        c.sandbox.marketplace.as_mut().unwrap().artifacts = Some(ArtifactsConfig {
            url: "http://10.68.0.2:8795".into(),
            token_file: "/etc/nests/artifacts.token".into(),
            cache_dir: "/var/lib/nests/artifacts".into(),
            max_bytes: 1 << 30,
        });
        assert!(c.check().is_ok());
    }

    #[test]
    fn the_pool_is_optional_and_the_shipped_example_still_parses() {
        let example = include_str!("../../../deploy/nests/host/nestd.toml.example");
        let parsed: Config = toml::from_str(example).expect("the example parses");
        assert!(parsed.sandbox.marketplace.is_none());
    }
}
