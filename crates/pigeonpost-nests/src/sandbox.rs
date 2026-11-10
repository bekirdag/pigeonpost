//! The pluggable sandbox runtime.
//!
//! The control service only ever talks to [`Sandbox`]. Phase A implements it with podman and
//! gVisor ([`GvisorPodman`]); a Firecracker/KVM worker implements the same trait on future
//! hardware, and nothing above this module changes (docs/nests/phase-a-plan.md §6).

use crate::config::{MarketplacePoolConfig, SandboxConfig};
use crate::util::valid_nest_id;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Everything one sandbox start needs.
#[derive(Debug, Clone)]
pub struct Spec {
    pub nest_id: String,
    pub generation: i64,
    /// Host path of the Nest's mounted volume; appears at `/nest` inside.
    pub volume: PathBuf,
    /// The buyer this sandbox serves, for the marketplace pool only. First-party Nests have none.
    pub buyer_context: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeState {
    Absent,
    Running,
    Paused,
    Exited,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Stats {
    pub memory_bytes: u64,
    pub memory_limit_bytes: u64,
    pub cpu_percent: f64,
    pub pids: u64,
}

pub trait Sandbox: Send + Sync {
    /// Start a fresh sandbox for this generation. Any previous one for the Nest is removed first.
    fn start(&self, spec: &Spec) -> Result<(), String>;
    /// Stop and remove. Idempotent.
    fn stop(&self, nest_id: &str) -> Result<(), String>;
    /// Freeze every process; memory stays resident.
    fn pause(&self, nest_id: &str) -> Result<(), String>;
    fn resume(&self, nest_id: &str) -> Result<(), String>;
    fn state(&self, nest_id: &str) -> Result<RuntimeState, String>;
    /// Every Nest sandbox present on the host, with its state and generation.
    fn list(&self) -> Result<Vec<(String, RuntimeState, i64)>, String>;
    fn stats(&self, nest_id: &str) -> Option<Stats>;
    /// A short name for status output ("gvisor-systrap", "firecracker", …).
    fn kind(&self) -> &'static str;
    /// Parse one uploaded file (Phase B4) in a sandbox of its own: no network, `input` read-only,
    /// only `out` writable, bounded memory, processes and `timeout`. Never inside the Nest's own
    /// sandbox, so a parser exploit cannot reach the Nest's mailbox or broker credentials.
    fn run_parser(
        &self,
        input: &Path,
        out: &Path,
        timeout: std::time::Duration,
    ) -> Result<(), String>;
}

pub const LABEL: &str = "dev.pigeonpost.nest";
pub const GEN_LABEL: &str = "dev.pigeonpost.nest.generation";

fn container_name(nest_id: &str) -> String {
    format!("nest-{nest_id}")
}

/// gVisor under podman. Rootful podman, daemonless: there is no API socket to expose, and the
/// control service is the only thing that ever runs `podman`.
pub struct GvisorPodman {
    cfg: SandboxConfig,
}

impl GvisorPodman {
    pub fn new(cfg: SandboxConfig) -> Self {
        GvisorPodman { cfg }
    }

    /// The exact `podman run` arguments for a Nest. Pure, so the isolation profile is asserted in
    /// tests rather than discovered from a process listing.
    pub fn run_args(cfg: &SandboxConfig, spec: &Spec) -> Vec<String> {
        let mem = format!("{}m", cfg.memory_mib);
        let mut a: Vec<String> = vec![
            "run".into(),
            "--detach".into(),
            "--name".into(),
            container_name(&spec.nest_id),
            "--label".into(),
            format!("{LABEL}={}", spec.nest_id),
            "--label".into(),
            format!("{GEN_LABEL}={}", spec.generation),
            // The sandbox boundary: gVisor's user-space kernel (systrap platform, no KVM here).
            "--runtime".into(),
            cfg.oci_runtime.clone(),
            "--pull".into(),
            "never".into(),
            "--hostname".into(),
            "nest".into(),
            // Unprivileged inside, too: no capabilities, no setuid escalation.
            "--user".into(),
            "10001:10001".into(),
            "--cap-drop".into(),
            "ALL".into(),
            "--security-opt".into(),
            "no-new-privileges".into(),
            // Immutable system image; the only writable places are the volume and a small /tmp.
            "--read-only".into(),
            "--tmpfs".into(),
            format!(
                "/tmp:rw,nosuid,nodev,noexec,size={}m,mode=1777",
                cfg.tmp_mib
            ),
            "--volume".into(),
            format!(
                "{}:/nest:rw,nosuid,nodev,noexec",
                spec.volume.to_string_lossy()
            ),
            // Resources: memory hard limit with no swap, fair CPU weight and a bandwidth cap (no
            // pinned core), bounded process count.
            "--memory".into(),
            mem.clone(),
            "--memory-swap".into(),
            mem,
            "--cpu-shares".into(),
            // podman maps shares to cgroup v2 weight: weight = 1 + (shares - 2) * 9999 / 262142.
            shares_for_weight(cfg.cpu_weight).to_string(),
            "--cpus".into(),
            format!("{:.3}", cfg.cpu_max_millis as f64 / 1000.0),
            "--pids-limit".into(),
            cfg.pids.to_string(),
            // Network: the policed bridge, no DNS, two pinned names and nothing else.
            "--network".into(),
            cfg.network.clone(),
            "--dns".into(),
            "none".into(),
            "--add-host".into(),
            format!("{}:{}", cfg.postbox_host, cfg.postbox_ip),
            "--add-host".into(),
            format!("broker.nests.internal:{}", cfg.broker_ip),
            // Bounded logs; the runner logs metadata only.
            "--log-driver".into(),
            "k8s-file".into(),
            "--log-opt".into(),
            "max-size=1mb".into(),
            "--stop-timeout".into(),
            "10".into(),
            "--env".into(),
            "PIGEONPOST_HOME=/nest/home".into(),
            "--env".into(),
            "PIGEONPOST_AGENTD_LISTEN=mailbox".into(),
            "--env".into(),
            format!("PIGEONPOST_POSTBOX={}", cfg.postbox_url),
            "--env".into(),
            format!(
                "NEST_BROKER_URL=http://broker.nests.internal:{}",
                cfg.broker_port
            ),
            "--env".into(),
            format!("NEST_ID={}", spec.nest_id),
            cfg.image.clone(),
        ];
        a.shrink_to_fit();
        a
    }

    fn podman(&self, args: &[&str]) -> Result<String, String> {
        crate::storage::run("podman", args)
    }
}

impl GvisorPodman {
    /// The exact `podman run` arguments for one parse. Pure, so the profile is asserted in tests.
    pub fn parser_args(cfg: &SandboxConfig, name: &str, input: &Path, out: &Path) -> Vec<String> {
        vec![
            "run".into(),
            "--rm".into(),
            "--name".into(),
            name.into(),
            "--runtime".into(),
            cfg.oci_runtime.clone(),
            "--pull".into(),
            "never".into(),
            // No network at all: a parser has nobody to talk to.
            "--network".into(),
            "none".into(),
            "--user".into(),
            "10001:10001".into(),
            "--cap-drop".into(),
            "ALL".into(),
            "--security-opt".into(),
            "no-new-privileges".into(),
            "--read-only".into(),
            "--tmpfs".into(),
            "/tmp:rw,nosuid,nodev,noexec,size=16m,mode=1777".into(),
            "--volume".into(),
            format!("{}:/in:ro,nosuid,nodev,noexec", input.to_string_lossy()),
            "--volume".into(),
            format!("{}:/out:rw,nosuid,nodev,noexec", out.to_string_lossy()),
            "--memory".into(),
            "256m".into(),
            "--memory-swap".into(),
            "256m".into(),
            "--cpus".into(),
            "1.000".into(),
            "--pids-limit".into(),
            "16".into(),
            "--log-driver".into(),
            "none".into(),
            "--entrypoint".into(),
            "/usr/local/bin/pigeonpost-nest-parser".into(),
            cfg.image.clone(),
            "/in".into(),
            "/out".into(),
        ]
    }
}

/// Inverse of podman's shares → weight mapping, so the configured value is the weight the cgroup
/// actually gets.
pub fn shares_for_weight(weight: u64) -> u64 {
    let w = weight.clamp(1, 10_000);
    // weight = 1 + (shares - 2) * 9999 / 262142  =>  shares = 2 + (weight - 1) * 262142 / 9999
    2 + ((w - 1) * 262_142).div_ceil(9_999)
}

impl Sandbox for GvisorPodman {
    fn start(&self, spec: &Spec) -> Result<(), String> {
        if !valid_nest_id(&spec.nest_id) {
            return Err("invalid nest id".into());
        }
        // Publisher code never lands in the first-party pool by a wrong code path.
        if spec.buyer_context.is_some() {
            return Err("a buyer context belongs in the marketplace pool".into());
        }
        self.stop(&spec.nest_id)?;
        let args = Self::run_args(&self.cfg, spec);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        self.podman(&refs).map(|_| ())
    }

    fn stop(&self, nest_id: &str) -> Result<(), String> {
        if !valid_nest_id(nest_id) {
            return Err("invalid nest id".into());
        }
        let name = container_name(nest_id);
        match self.podman(&["rm", "--force", "--time", "10", "--ignore", &name]) {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn pause(&self, nest_id: &str) -> Result<(), String> {
        self.podman(&["pause", &container_name(nest_id)])
            .map(|_| ())
    }

    fn resume(&self, nest_id: &str) -> Result<(), String> {
        self.podman(&["unpause", &container_name(nest_id)])
            .map(|_| ())
    }

    fn state(&self, nest_id: &str) -> Result<RuntimeState, String> {
        Ok(self
            .list()?
            .into_iter()
            .find(|(id, _, _)| id == nest_id)
            .map(|(_, s, _)| s)
            .unwrap_or(RuntimeState::Absent))
    }

    fn list(&self) -> Result<Vec<(String, RuntimeState, i64)>, String> {
        let out = self.podman(&[
            "ps",
            "--all",
            "--filter",
            &format!("label={LABEL}"),
            "--format",
            "json",
        ])?;
        let rows: serde_json::Value =
            serde_json::from_str(out.trim()).unwrap_or(serde_json::Value::Array(vec![]));
        let mut found = Vec::new();
        for row in rows.as_array().cloned().unwrap_or_default() {
            let labels = &row["Labels"];
            let Some(id) = labels[LABEL].as_str() else {
                continue;
            };
            if !valid_nest_id(id) {
                continue;
            }
            let generation = labels[GEN_LABEL]
                .as_str()
                .and_then(|g| g.parse().ok())
                .unwrap_or(0);
            let state = match row["State"].as_str().unwrap_or("") {
                "running" => RuntimeState::Running,
                "paused" => RuntimeState::Paused,
                _ => RuntimeState::Exited,
            };
            found.push((id.to_string(), state, generation));
        }
        Ok(found)
    }

    fn stats(&self, nest_id: &str) -> Option<Stats> {
        let out = self
            .podman(&[
                "stats",
                "--no-stream",
                "--no-reset",
                "--format",
                "json",
                &container_name(nest_id),
            ])
            .ok()?;
        let v: serde_json::Value = serde_json::from_str(out.trim()).ok()?;
        let row = v.as_array()?.first()?.clone();
        Some(Stats {
            memory_bytes: parse_size(row["mem_usage"].as_str().unwrap_or("").split('/').next()?),
            memory_limit_bytes: parse_size(
                row["mem_usage"].as_str().unwrap_or("").split('/').nth(1)?,
            ),
            cpu_percent: row["cpu_percent"]
                .as_str()
                .unwrap_or("0")
                .trim_end_matches('%')
                .parse()
                .unwrap_or(0.0),
            pids: row["pids"].as_str().unwrap_or("0").parse().unwrap_or(0),
        })
    }

    fn kind(&self) -> &'static str {
        "gvisor-systrap"
    }

    fn run_parser(
        &self,
        input: &Path,
        out: &Path,
        timeout: std::time::Duration,
    ) -> Result<(), String> {
        let name = format!("nest-parse-{}", crate::util::random_hex(8));
        let args = Self::parser_args(&self.cfg, &name, input, out);
        let mut child = std::process::Command::new("podman")
            .args(&args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("podman: {e}"))?;
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(status)) => return Err(format!("parser exited with {status}")),
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(100))
                }
                _ => {
                    let _ = self.podman(&["rm", "--force", "--time", "0", "--ignore", &name]);
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("parser timed out".into());
                }
            }
        }
    }
}

/// "12.5MB", "1.2GB", "512kB" → bytes (podman's decimal units).
fn parse_size(text: &str) -> u64 {
    let t = text.trim();
    let split = t.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let n: f64 = num.trim().parse().unwrap_or(0.0);
    let mult = match unit.trim() {
        "kB" | "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        _ => 1.0,
    };
    (n * mult) as u64
}

/// In-memory runtime for tests.
#[derive(Default)]
pub struct Fake {
    pub boxes: Mutex<HashMap<String, (RuntimeState, i64)>>,
}

impl Sandbox for Fake {
    fn start(&self, spec: &Spec) -> Result<(), String> {
        if !Path::new(&spec.volume).exists() {
            return Err("volume missing".into());
        }
        self.boxes.lock().unwrap().insert(
            spec.nest_id.clone(),
            (RuntimeState::Running, spec.generation),
        );
        Ok(())
    }
    fn stop(&self, nest_id: &str) -> Result<(), String> {
        self.boxes.lock().unwrap().remove(nest_id);
        Ok(())
    }
    fn pause(&self, nest_id: &str) -> Result<(), String> {
        match self.boxes.lock().unwrap().get_mut(nest_id) {
            Some(b) => {
                b.0 = RuntimeState::Paused;
                Ok(())
            }
            None => Err("absent".into()),
        }
    }
    fn resume(&self, nest_id: &str) -> Result<(), String> {
        match self.boxes.lock().unwrap().get_mut(nest_id) {
            Some(b) => {
                b.0 = RuntimeState::Running;
                Ok(())
            }
            None => Err("absent".into()),
        }
    }
    fn state(&self, nest_id: &str) -> Result<RuntimeState, String> {
        Ok(self
            .boxes
            .lock()
            .unwrap()
            .get(nest_id)
            .map(|b| b.0)
            .unwrap_or(RuntimeState::Absent))
    }
    fn list(&self) -> Result<Vec<(String, RuntimeState, i64)>, String> {
        Ok(self
            .boxes
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.0, v.1))
            .collect())
    }
    fn stats(&self, _nest_id: &str) -> Option<Stats> {
        None
    }
    fn kind(&self) -> &'static str {
        "fake"
    }

    /// Tests: the same parser, in process (the profile itself is asserted on `parser_args`).
    fn run_parser(
        &self,
        input: &Path,
        out: &Path,
        _timeout: std::time::Duration,
    ) -> Result<(), String> {
        let data = std::fs::read(input).map_err(|e| e.to_string())?;
        let m = crate::parse::parse(&data);
        for p in &m.parts {
            std::fs::write(out.join(&p.name), &p.text).map_err(|e| e.to_string())?;
        }
        std::fs::write(
            out.join("manifest.json"),
            serde_json::to_vec(&m).unwrap_or_default(),
        )
        .map_err(|e| e.to_string())
    }
}

// ---- the marketplace pool -----------------------------------------------------------------------

/// Label of a marketplace sandbox. Deliberately different from [`LABEL`]: each pool's `list` sees
/// only its own containers, so the first-party reconciler can never adopt or remove one of these.
pub const MP_LABEL: &str = "dev.pigeonpost.marketplace.nest";
pub const MP_GEN_LABEL: &str = "dev.pigeonpost.marketplace.generation";
pub const MP_CONTEXT_LABEL: &str = "dev.pigeonpost.marketplace.context";

fn mp_container_name(nest_id: &str) -> String {
    format!("nest-mp-{nest_id}")
}

/// A buyer context id: what a directory and a label may safely be named.
pub fn valid_buyer_context(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// How the pool runs `podman`. A trait so the isolation rules run in tests with no container
/// runtime on the machine.
pub trait Podman: Send + Sync {
    fn run(&self, args: &[&str]) -> Result<String, String>;
}

/// The real thing.
pub struct HostPodman;

impl Podman for HostPodman {
    fn run(&self, args: &[&str]) -> Result<String, String> {
        crate::storage::run("podman", args)
    }
}

/// One marketplace sandbox as `podman ps` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PoolBox {
    nest_id: String,
    context: String,
    state: RuntimeState,
    generation: i64,
}

/// The marketplace pool: gVisor on the same host as the first-party pool, under a separate and
/// stricter configuration (owner decision 2026-11-09: no KVM, so gVisor stands in for the plan's
/// Firecracker).
///
/// What it guarantees, each checked here before anything starts:
/// - one running sandbox per buyer context, and a Nest never moves between contexts;
/// - the only mount is that context's own directory, `<volume_root>/<context>/<nest>`, so two
///   contexts can never share one;
/// - network only to the broker: no postbox address, no DNS, no other names.
pub struct MarketplacePool<P: Podman = HostPodman> {
    base: SandboxConfig,
    pool: MarketplacePoolConfig,
    podman: P,
    /// Serializes `start`, so two starts for one context cannot both pass the one-sandbox check.
    start_lock: Mutex<()>,
}

impl MarketplacePool<HostPodman> {
    pub fn new(base: SandboxConfig, pool: MarketplacePoolConfig) -> Self {
        Self::with_podman(base, pool, HostPodman)
    }
}

impl<P: Podman> MarketplacePool<P> {
    pub fn with_podman(base: SandboxConfig, pool: MarketplacePoolConfig, podman: P) -> Self {
        MarketplacePool {
            base,
            pool,
            podman,
            start_lock: Mutex::new(()),
        }
    }

    /// The exact `podman run` arguments. Pure, so the profile is asserted in tests.
    pub fn run_args(
        base: &SandboxConfig,
        pool: &MarketplacePoolConfig,
        spec: &Spec,
        context: &str,
    ) -> Vec<String> {
        let mem = format!("{}m", pool.memory_mib);
        vec![
            "run".into(),
            "--detach".into(),
            "--name".into(),
            mp_container_name(&spec.nest_id),
            "--label".into(),
            format!("{MP_LABEL}={}", spec.nest_id),
            "--label".into(),
            format!("{MP_GEN_LABEL}={}", spec.generation),
            "--label".into(),
            format!("{MP_CONTEXT_LABEL}={context}"),
            "--runtime".into(),
            base.oci_runtime.clone(),
            "--pull".into(),
            "never".into(),
            "--hostname".into(),
            "nest".into(),
            "--user".into(),
            "10001:10001".into(),
            "--cap-drop".into(),
            "ALL".into(),
            "--security-opt".into(),
            "no-new-privileges".into(),
            // Immutable image, and none of podman's implicit writable tmpfs mounts: /tmp below is
            // the only one. An image cannot bring a volume of its own either.
            "--read-only".into(),
            "--read-only-tmpfs=false".into(),
            "--image-volume".into(),
            "ignore".into(),
            "--ipc".into(),
            "private".into(),
            "--cgroupns".into(),
            "private".into(),
            "--tmpfs".into(),
            format!(
                "/tmp:rw,nosuid,nodev,noexec,size={}m,mode=1777",
                pool.tmp_mib
            ),
            "--volume".into(),
            format!(
                "{}:/nest:rw,nosuid,nodev,noexec",
                spec.volume.to_string_lossy()
            ),
            "--memory".into(),
            mem.clone(),
            "--memory-swap".into(),
            mem,
            "--cpu-shares".into(),
            shares_for_weight(pool.cpu_weight).to_string(),
            "--cpus".into(),
            format!("{:.3}", pool.cpu_max_millis as f64 / 1000.0),
            "--pids-limit".into(),
            pool.pids.to_string(),
            // Network: the broker and nothing else. There is no postbox address and no name to
            // resolve; the broker is reached by the address given in the environment below, and
            // nftables refuses everything else on this bridge.
            "--network".into(),
            pool.network.clone(),
            "--dns".into(),
            "none".into(),
            "--no-hosts".into(),
            // No container logs: publisher code must not write a buyer's data to the host disk.
            "--log-driver".into(),
            "none".into(),
            "--stop-timeout".into(),
            "10".into(),
            "--env".into(),
            format!(
                "NEST_BROKER_URL=http://{}:{}",
                pool.broker_ip, pool.broker_port
            ),
            "--env".into(),
            format!("NEST_ID={}", spec.nest_id),
            pool.image.clone(),
        ]
    }

    /// Check a spec against the pool's isolation rules. Returns the buyer context.
    fn check_spec<'a>(&self, spec: &'a Spec) -> Result<&'a str, String> {
        if !valid_nest_id(&spec.nest_id) {
            return Err("invalid nest id".into());
        }
        let context = spec
            .buyer_context
            .as_deref()
            .ok_or("the marketplace pool needs a buyer context")?;
        if !valid_buyer_context(context) {
            return Err("invalid buyer context".into());
        }
        // The real paths, so a symlink cannot make one context's directory another's.
        let root = std::fs::canonicalize(&self.pool.volume_root)
            .map_err(|e| format!("volume_root: {e}"))?;
        let volume = std::fs::canonicalize(&spec.volume).map_err(|e| format!("volume: {e}"))?;
        if volume != root.join(context).join(&spec.nest_id) {
            return Err("the volume is not this buyer context's own directory".into());
        }
        Ok(context)
    }

    fn boxes(&self) -> Result<Vec<PoolBox>, String> {
        let out = self.podman.run(&[
            "ps",
            "--all",
            "--filter",
            &format!("label={MP_LABEL}"),
            "--format",
            "json",
        ])?;
        Ok(parse_pool_boxes(&out))
    }
}

/// `podman ps --format json` → this pool's sandboxes, skipping anything malformed.
fn parse_pool_boxes(out: &str) -> Vec<PoolBox> {
    let rows: serde_json::Value =
        serde_json::from_str(out.trim()).unwrap_or(serde_json::Value::Array(vec![]));
    let mut found = Vec::new();
    for row in rows.as_array().cloned().unwrap_or_default() {
        let labels = &row["Labels"];
        let (Some(id), Some(context)) =
            (labels[MP_LABEL].as_str(), labels[MP_CONTEXT_LABEL].as_str())
        else {
            continue;
        };
        if !valid_nest_id(id) || !valid_buyer_context(context) {
            continue;
        }
        found.push(PoolBox {
            nest_id: id.to_string(),
            context: context.to_string(),
            state: match row["State"].as_str().unwrap_or("") {
                "running" => RuntimeState::Running,
                "paused" => RuntimeState::Paused,
                _ => RuntimeState::Exited,
            },
            generation: labels[MP_GEN_LABEL]
                .as_str()
                .and_then(|g| g.parse().ok())
                .unwrap_or(0),
        });
    }
    found
}

impl<P: Podman> Sandbox for MarketplacePool<P> {
    fn start(&self, spec: &Spec) -> Result<(), String> {
        let context = self.check_spec(spec)?;
        let _one_at_a_time = self
            .start_lock
            .lock()
            .map_err(|_| "pool lock poisoned".to_string())?;
        for existing in self.boxes()? {
            if existing.nest_id == spec.nest_id {
                if existing.context != context {
                    return Err("this Nest belongs to another buyer context".into());
                }
                continue;
            }
            // An exited box still counts: it holds its context until it is removed.
            if existing.context == context {
                return Err("this buyer context already has a sandbox".into());
            }
        }
        self.stop(&spec.nest_id)?;
        let args = Self::run_args(&self.base, &self.pool, spec, context);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        self.podman.run(&refs).map(|_| ())
    }

    fn stop(&self, nest_id: &str) -> Result<(), String> {
        if !valid_nest_id(nest_id) {
            return Err("invalid nest id".into());
        }
        let name = mp_container_name(nest_id);
        self.podman
            .run(&["rm", "--force", "--time", "10", "--ignore", &name])
            .map(|_| ())
    }

    fn pause(&self, nest_id: &str) -> Result<(), String> {
        self.podman
            .run(&["pause", &mp_container_name(nest_id)])
            .map(|_| ())
    }

    fn resume(&self, nest_id: &str) -> Result<(), String> {
        self.podman
            .run(&["unpause", &mp_container_name(nest_id)])
            .map(|_| ())
    }

    fn state(&self, nest_id: &str) -> Result<RuntimeState, String> {
        Ok(self
            .boxes()?
            .into_iter()
            .find(|b| b.nest_id == nest_id)
            .map(|b| b.state)
            .unwrap_or(RuntimeState::Absent))
    }

    fn list(&self) -> Result<Vec<(String, RuntimeState, i64)>, String> {
        Ok(self
            .boxes()?
            .into_iter()
            .map(|b| (b.nest_id, b.state, b.generation))
            .collect())
    }

    fn stats(&self, _nest_id: &str) -> Option<Stats> {
        None
    }

    fn kind(&self) -> &'static str {
        "gvisor-marketplace"
    }

    /// Not offered in this pool yet: a parser here would need its own review as a place publisher
    /// input and host files meet.
    fn run_parser(
        &self,
        _input: &Path,
        _out: &Path,
        _timeout: std::time::Duration,
    ) -> Result<(), String> {
        Err("file parsing is not available in the marketplace pool".into())
    }
}

/// The marketplace pool, when `[sandbox.marketplace]` is configured. `fake` runtimes have none.
pub fn marketplace_from_config(cfg: &SandboxConfig) -> Result<Option<Box<dyn Sandbox>>, String> {
    let Some(pool) = cfg.marketplace.clone() else {
        return Ok(None);
    };
    match cfg.runtime.as_str() {
        "gvisor" => Ok(Some(Box::new(MarketplacePool::new(cfg.clone(), pool)))),
        "fake" => Ok(None),
        other => Err(format!("unknown sandbox runtime {other:?}")),
    }
}

pub fn from_config(cfg: &SandboxConfig) -> Result<Box<dyn Sandbox>, String> {
    match cfg.runtime.as_str() {
        "gvisor" => Ok(Box::new(GvisorPodman::new(cfg.clone()))),
        "fake" => Ok(Box::new(Fake::default())),
        // The Firecracker/KVM worker plugs in here on hardware that has /dev/kvm.
        other => Err(format!("unknown sandbox runtime {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_isolation_profile_is_fixed() {
        let dir = std::env::temp_dir();
        let cfg = crate::config::test_config(&dir).sandbox;
        let spec = Spec {
            nest_id: "nst_00000000000000bb".into(),
            generation: 7,
            volume: PathBuf::from("/var/lib/nests/mnt/nst_00000000000000bb"),
            buyer_context: None,
        };
        let a = GvisorPodman::run_args(&cfg, &spec).join(" ");
        for must in [
            "--runtime runsc-nests",
            "--user 10001:10001",
            "--cap-drop ALL",
            "--security-opt no-new-privileges",
            "--read-only",
            "--memory 512m --memory-swap 512m",
            "--pids-limit 128",
            "--network nests",
            "--dns none",
            "--pull never",
            "--add-host postbox.pigeonpost.dev:45.132.181.100",
            "/var/lib/nests/mnt/nst_00000000000000bb:/nest:rw,nosuid,nodev,noexec",
            "dev.pigeonpost.nest.generation=7",
        ] {
            assert!(a.contains(must), "missing {must} in {a}");
        }
        for never in [
            "--privileged",
            "--cap-add",
            "docker.sock",
            "podman.sock",
            "--pid host",
            "--network host",
        ] {
            assert!(!a.contains(never), "found {never}");
        }
        assert!(a.ends_with(&cfg.image));
    }

    #[test]
    fn weight_round_trips_through_podman_shares() {
        for w in [1u64, 50, 100, 1000, 10_000] {
            let s = shares_for_weight(w);
            let back = 1 + (s - 2) * 9999 / 262_142;
            assert_eq!(back, w, "weight {w} -> shares {s}");
        }
    }

    // ---- the marketplace pool: isolation between buyer contexts ---------------------------------

    const NEST_A: &str = "nst_00000000000000aa";
    const NEST_B: &str = "nst_00000000000000bb";
    const NEST_A2: &str = "nst_00000000000000a2";

    /// A podman that remembers what ran, enough for `start` to see its own sandboxes.
    #[derive(Default, Clone)]
    struct FakePodman {
        rows: std::sync::Arc<Mutex<Vec<serde_json::Value>>>,
        calls: std::sync::Arc<Mutex<Vec<Vec<String>>>>,
    }

    impl Podman for FakePodman {
        fn run(&self, args: &[&str]) -> Result<String, String> {
            let argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
            self.calls.lock().unwrap().push(argv.clone());
            match args.first().copied() {
                Some("run") => {
                    let label = |key: &str| {
                        argv.iter()
                            .find_map(|a| a.strip_prefix(&format!("{key}=")))
                            .unwrap()
                            .to_string()
                    };
                    let name = argv[argv.iter().position(|a| a == "--name").unwrap() + 1].clone();
                    self.rows.lock().unwrap().push(serde_json::json!({
                        "Names": [name],
                        "State": "running",
                        "Labels": {
                            MP_LABEL: label(MP_LABEL),
                            MP_GEN_LABEL: label(MP_GEN_LABEL),
                            MP_CONTEXT_LABEL: label(MP_CONTEXT_LABEL),
                        },
                    }));
                    Ok(String::new())
                }
                Some("rm") => {
                    let name = args.last().unwrap();
                    self.rows
                        .lock()
                        .unwrap()
                        .retain(|r| r["Names"][0].as_str() != Some(name));
                    Ok(String::new())
                }
                Some("ps") => {
                    Ok(serde_json::Value::Array(self.rows.lock().unwrap().clone()).to_string())
                }
                _ => Ok(String::new()),
            }
        }
    }

    struct Rig {
        pool: MarketplacePool<FakePodman>,
        podman: FakePodman,
        root: PathBuf,
    }

    fn rig() -> Rig {
        let root = std::env::temp_dir().join(format!(
            "pp-mp-{}-{}",
            std::process::id(),
            crate::util::random_hex(6)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let mut base = crate::config::test_config(&root).sandbox;
        base.runtime = "gvisor".into();
        let pool_cfg = crate::config::test_marketplace_pool(&root);
        let podman = FakePodman::default();
        Rig {
            pool: MarketplacePool::with_podman(base, pool_cfg, podman.clone()),
            podman,
            root,
        }
    }

    impl Rig {
        fn volume(&self, context: &str, nest: &str) -> PathBuf {
            let path = self.root.join("mp").join(context).join(nest);
            std::fs::create_dir_all(&path).unwrap();
            path
        }
        fn spec(&self, context: &str, nest: &str) -> Spec {
            Spec {
                nest_id: nest.into(),
                generation: 1,
                volume: self.volume(context, nest),
                buyer_context: Some(context.into()),
            }
        }
        fn runs(&self) -> Vec<Vec<String>> {
            self.podman
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|c| c[0] == "run")
                .cloned()
                .collect()
        }
    }

    fn mounts_in(argv: &[String]) -> Vec<String> {
        argv.iter()
            .enumerate()
            .filter(|(_, a)| *a == "--volume" || *a == "--mount")
            .map(|(i, _)| argv[i + 1].clone())
            .collect()
    }

    #[test]
    fn the_marketplace_profile_is_fixed_and_stricter_than_first_party() {
        let rig = rig();
        let spec = rig.spec("buyer-a", NEST_A);
        rig.pool.start(&spec).unwrap();
        let runs = rig.runs();
        assert_eq!(runs.len(), 1);
        let argv = &runs[0];
        let a = argv.join(" ");
        let cfg = &rig.pool.pool;
        for must in [
            "--runtime runsc-nests",
            "--user 10001:10001",
            "--cap-drop ALL",
            "--security-opt no-new-privileges",
            "--read-only",
            "--read-only-tmpfs=false",
            "--image-volume ignore",
            "--network nests-mp",
            "--dns none",
            "--no-hosts",
            "--log-driver none",
            "--pull never",
            "--ipc private",
            "--cgroupns private",
            "dev.pigeonpost.marketplace.context=buyer-a",
        ] {
            assert!(a.contains(must), "missing {must} in {a}");
        }
        for never in [
            "--privileged",
            "--cap-add",
            "docker.sock",
            "podman.sock",
            "--pid host",
            "--network host",
            "--add-host",
            "PIGEONPOST_POSTBOX",
            "PIGEONPOST_AGENTD_LISTEN",
            "postbox.pigeonpost.dev",
            "45.132.181.100",
        ] {
            assert!(!a.contains(never), "found {never} in {a}");
        }
        assert!(a.ends_with(&cfg.image));
        // One mount, this context's own directory, and that is all the host it can touch.
        assert_eq!(mounts_in(argv).len(), 1, "{a}");
        assert!(
            mounts_in(argv)[0].contains("/mp/buyer-a/nst_00000000000000aa:/nest:"),
            "{a}"
        );
        // The pool's limits, not the first-party ones.
        assert!(a.contains("--memory 256m --memory-swap 256m"), "{a}");
        assert!(a.contains("--pids-limit 64"), "{a}");
        assert!(a.contains("NEST_BROKER_URL=http://10.67.255.1:8788"), "{a}");
    }

    #[test]
    fn two_contexts_share_no_mount_no_container_and_no_label() {
        let rig = rig();
        rig.pool.start(&rig.spec("buyer-a", NEST_A)).unwrap();
        rig.pool.start(&rig.spec("buyer-b", NEST_B)).unwrap();
        let runs = rig.runs();
        assert_eq!(runs.len(), 2);
        let mounts: Vec<Vec<String>> = runs.iter().map(|r| mounts_in(r)).collect();
        assert_eq!(mounts[0].len(), 1);
        assert_eq!(mounts[1].len(), 1);
        assert_ne!(mounts[0], mounts[1]);
        let host_path = |m: &str| m.split(':').next().unwrap().to_string();
        let (a, b) = (host_path(&mounts[0][0]), host_path(&mounts[1][0]));
        assert!(!a.starts_with(&b) && !b.starts_with(&a), "{a} {b}");
        let label = |argv: &[String]| {
            argv.iter()
                .find_map(|x| x.strip_prefix(&format!("{MP_CONTEXT_LABEL}=")))
                .unwrap()
                .to_string()
        };
        assert_eq!(label(&runs[0]), "buyer-a");
        assert_eq!(label(&runs[1]), "buyer-b");
        let listed = rig.pool.list().unwrap();
        assert_eq!(listed.len(), 2);
    }

    #[test]
    fn one_buyer_context_has_one_sandbox() {
        let rig = rig();
        rig.pool.start(&rig.spec("buyer-a", NEST_A)).unwrap();
        // A second Nest in the same context is refused, and nothing was started for it.
        let err = rig.pool.start(&rig.spec("buyer-a", NEST_A2)).unwrap_err();
        assert!(err.contains("already has a sandbox"), "{err}");
        assert_eq!(rig.runs().len(), 1);
        // The same Nest restarting is one sandbox, replaced.
        rig.pool.start(&rig.spec("buyer-a", NEST_A)).unwrap();
        assert_eq!(rig.pool.list().unwrap().len(), 1);
        // Once it is gone the context is free again.
        rig.pool.stop(NEST_A).unwrap();
        rig.pool.start(&rig.spec("buyer-a", NEST_A2)).unwrap();
        assert_eq!(rig.pool.list().unwrap().len(), 1);
    }

    #[test]
    fn a_nest_never_moves_between_buyer_contexts() {
        let rig = rig();
        rig.pool.start(&rig.spec("buyer-a", NEST_A)).unwrap();
        let moved = Spec {
            nest_id: NEST_A.into(),
            generation: 2,
            volume: rig.volume("buyer-b", NEST_A),
            buyer_context: Some("buyer-b".into()),
        };
        let err = rig.pool.start(&moved).unwrap_err();
        assert!(err.contains("another buyer context"), "{err}");
        assert_eq!(rig.runs().len(), 1);
    }

    #[test]
    fn one_buyer_context_cannot_mount_another_contexts_volume() {
        let rig = rig();
        // The other context's directory, an unrelated directory, a path outside the root, an
        // escape through `..`, and a symlink that points into the other context.
        let theirs = rig.volume("buyer-b", NEST_B);
        let other_nest = rig.volume("buyer-a", NEST_B);
        let outside = rig.root.join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        let link = rig.root.join("mp").join("buyer-a").join(NEST_A);
        std::os::unix::fs::symlink(&theirs, &link).unwrap();
        for volume in [
            theirs.clone(),
            other_nest,
            outside,
            rig.root.join("mp/buyer-a/../buyer-b").join(NEST_B),
            link,
        ] {
            let spec = Spec {
                nest_id: NEST_A.into(),
                generation: 1,
                volume: volume.clone(),
                buyer_context: Some("buyer-a".into()),
            };
            assert!(rig.pool.start(&spec).is_err(), "{volume:?} must be refused");
        }
        assert!(rig.runs().is_empty(), "nothing may start");
        // Traversal in the context id itself.
        for context in ["", "../buyer-b", "Buyer", "a/b", "a.b"] {
            let spec = Spec {
                nest_id: NEST_A.into(),
                generation: 1,
                volume: rig.root.join("mp"),
                buyer_context: Some(context.into()),
            };
            assert!(rig.pool.start(&spec).is_err(), "{context:?}");
        }
    }

    #[test]
    fn the_pools_do_not_stand_in_for_each_other() {
        let rig = rig();
        // Without a buyer context the marketplace pool refuses to start.
        let mut spec = rig.spec("buyer-a", NEST_A);
        spec.buyer_context = None;
        assert!(rig.pool.start(&spec).is_err());
        // With one, the first-party pool refuses before touching podman.
        let base = rig.pool.base.clone();
        let first_party = GvisorPodman::new(base);
        let marketplace_spec = rig.spec("buyer-a", NEST_A);
        let err = first_party.start(&marketplace_spec).unwrap_err();
        assert!(err.contains("marketplace pool"), "{err}");
        // Their labels and names differ, so neither pool's `list` can see the other's sandboxes.
        assert_ne!(LABEL, MP_LABEL);
        assert_ne!(GEN_LABEL, MP_GEN_LABEL);
        assert!(!mp_container_name(NEST_A).starts_with(&container_name(NEST_A)));
        assert_eq!(rig.pool.kind(), "gvisor-marketplace");
        assert!(rig
            .pool
            .run_parser(&rig.root, &rig.root, std::time::Duration::from_secs(1))
            .is_err());
    }

    /// The firewall that makes "network only to the broker" true. The bridge is separate from the
    /// first-party one and the policy allows the broker and nothing else, with no postbox path.
    #[test]
    fn the_marketplace_bridge_is_policed_to_the_broker_only() {
        let nft = include_str!("../../../deploy/nests/host/nests.nft");
        let table = nft
            .split("table inet nests_marketplace {")
            .nth(1)
            .expect("the marketplace table is in deploy/nests/host/nests.nft");
        let table = &table[..table.find("\n}\n").unwrap()];
        for must in [
            r#"iifname != "nestsmp0" return"#,
            "10.67.255.1",
            "tcp dport 8788",
            r#"iifname "nestsmp0""#,
            "counter drop",
        ] {
            assert!(table.contains(must), "missing {must}");
        }
        for never in ["@postbox_v4", "45.132.181.100", "dport 443"] {
            assert!(!table.contains(never), "found {never}");
        }
        assert!(
            nft.contains(r#"ibrname "nestsmp0" counter drop"#),
            "no port-to-port traffic between buyer contexts on the bridge"
        );
    }

    #[test]
    fn sizes_parse() {
        assert_eq!(parse_size("12.5MB"), 12_500_000);
        assert_eq!(parse_size("1GB"), 1_000_000_000);
        assert_eq!(parse_size("512kB"), 512_000);
        assert_eq!(parse_size(" 2MiB "), 2 * 1024 * 1024);
    }
}
