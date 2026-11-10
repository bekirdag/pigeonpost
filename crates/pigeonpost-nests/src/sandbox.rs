//! The pluggable sandbox runtime.
//!
//! The control service only ever talks to [`Sandbox`]. Phase A implements it with podman and
//! gVisor ([`GvisorPodman`]); a Firecracker/KVM worker implements the same trait on future
//! hardware, and nothing above this module changes (docs/nests/phase-a-plan.md §6).

use crate::config::SandboxConfig;
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
            // gVisor's sandbox threads count against this limit too: at 16 the sandbox dies before
            // the parser starts (podman exit 126, seen on nests-1 2026-11-10); 64 leaves room.
            "--pids-limit".into(),
            "64".into(),
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

    #[test]
    fn sizes_parse() {
        assert_eq!(parse_size("12.5MB"), 12_500_000);
        assert_eq!(parse_size("1GB"), 1_000_000_000);
        assert_eq!(parse_size("512kB"), 512_000);
        assert_eq!(parse_size(" 2MiB "), 2 * 1024 * 1024);
    }
}
