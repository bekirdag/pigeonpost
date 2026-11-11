//! The analysis plane (build plan M1, R-63/R-64): what a release goes through between its build
//! and a staff decision. Each step produces a [`StageReport`] that the postbox stores as a
//! `review_decisions` row with its evidence.
//!
//! - **deterministic_scan**, offline: secrets in the source tree; capabilities, syscalls, egress
//!   and external writes in the manifest beyond what the pool allows; every locked package's
//!   licence against [`LICENCE_ALLOWLIST`], for Rust (`Cargo.lock`), Node (`package-lock.json`,
//!   whose own `license` fields must agree with the manifest) and Python (`requirements.txt`).
//!   Node and Python sources are also read for process spawning, native code loading and literal
//!   network endpoints: advisory findings for the reviewer (the pool's network and seccomp policy
//!   is what enforces), never a pass or fail on their own.
//! - **behavioral_test**: honeytokens are planted where the release has no business looking, the
//!   release runs its operations, and every channel out of it (the broker, its job results, its
//!   logs) is searched for the tokens in plain and encoded forms. One hit fails the stage.
//! - **ai_analysis**: a review by the self-hosted model route, model [`AI_REVIEW_MODEL`] and no
//!   other. It is **advisory**: its decision is always `advisory`, it never passes or fails a
//!   release, and its findings are evidence for the staff reviewer (R-64, R-65: AI never approves).
//!
//! Publisher source and evidence never leave this host except as the redacted report to the
//! postbox; the model route is self-hosted.

use crate::ecosystem::{self, Ecosystem};
use crate::release::{
    canonical_json, digest_of, read_tree, LockedPackage, Manifest, MANIFEST_FILE,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// The one model the AI review may use: mswarm's self-hosted route. Never a cloud model.
pub const AI_REVIEW_MODEL: &str = "qwen-latest-flash-next";

/// Licences a release and each of its dependencies may carry (FOSS, permissive or weak copyleft
/// that does not reach the platform).
pub const LICENCE_ALLOWLIST: &[&str] = &[
    "MIT",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Zlib",
    "0BSD",
    "CC0-1.0",
    "Unlicense",
    "BSL-1.0",
    "Unicode-3.0",
    "Unicode-DFS-2016",
    "MPL-2.0",
];

/// Linux capabilities a release may ask for: none.
pub const ALLOWED_CAPABILITIES: &[&str] = &[];

/// Syscalls no release may ask the pool to allow, whatever else changes.
pub const FORBIDDEN_SYSCALLS: &[&str] = &[
    "ptrace",
    "mount",
    "umount2",
    "pivot_root",
    "chroot",
    "unshare",
    "setns",
    "bpf",
    "perf_event_open",
    "keyctl",
    "add_key",
    "request_key",
    "kexec_load",
    "kexec_file_load",
    "init_module",
    "finit_module",
    "delete_module",
    "reboot",
    "swapon",
    "swapoff",
    "iopl",
    "ioperm",
    "process_vm_readv",
    "process_vm_writev",
    "open_by_handle_at",
    "userfaultfd",
];

/// The only egress a marketplace release may declare.
pub const ALLOWED_EGRESS: &[&str] = &["broker"];

/// How bad a finding is. Tools that grade (OSV, Grype, Semgrep) map onto this scale; the
/// platform's own rules are `high` when they block and `low` when they only inform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    pub fn name(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Low => "low",
            Severity::Medium => "medium",
            Severity::High => "high",
            Severity::Critical => "critical",
        }
    }

    /// A CVSS base score (`"7.5"`) or a database's word (`HIGH`, `MODERATE`, …).
    pub fn parse(text: &str) -> Option<Severity> {
        let t = text.trim();
        if let Ok(score) = t.parse::<f64>() {
            return Some(match score {
                s if s >= 9.0 => Severity::Critical,
                s if s >= 7.0 => Severity::High,
                s if s >= 4.0 => Severity::Medium,
                s if s > 0.0 => Severity::Low,
                _ => Severity::Info,
            });
        }
        match t.to_ascii_lowercase().as_str() {
            "critical" => Some(Severity::Critical),
            "high" | "error" => Some(Severity::High),
            "medium" | "moderate" | "warning" => Some(Severity::Medium),
            "low" => Some(Severity::Low),
            "info" | "negligible" | "none" => Some(Severity::Info),
            _ => None,
        }
    }
}

/// One finding, as the staff reviewer sees it (the publisher sees the postbox's redaction of it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Stable across runs for the same issue (`osv:GHSA-…:serde@1.0.0`), so a re-scan can tell a
    /// new finding from a known one.
    pub id: String,
    pub severity: Severity,
    /// `pigeonpost` (the built-in rules), `osv-scanner`, `grype`, `clamav`, `yara`, `semgrep`,
    /// `gitleaks`, `heuristics`, `gvisor` (egress) or `cgroup` (resources).
    pub tool: String,
    pub rule: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// `name@version` for a dependency or image package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    pub message: String,
    pub blocking: bool,
}

/// Longest id or message a finding keeps.
const FINDING_ID_MAX: usize = 300;
const FINDING_MESSAGE_MAX: usize = 1000;

fn clip(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect()
}

impl Finding {
    /// A finding of `tool`. The id is `tool:key`, clipped.
    pub fn of(
        tool: &str,
        rule: &str,
        severity: Severity,
        key: &str,
        message: impl Into<String>,
        blocking: bool,
    ) -> Finding {
        Finding {
            id: clip(&format!("{tool}:{key}"), FINDING_ID_MAX),
            severity,
            tool: tool.into(),
            rule: rule.into(),
            path: None,
            package: None,
            message: clip(&message.into(), FINDING_MESSAGE_MAX),
            blocking,
        }
    }

    pub fn at(mut self, path: Option<&str>) -> Finding {
        self.path = path.map(|p| clip(p, 500));
        self
    }

    pub fn package(mut self, package: Option<String>) -> Finding {
        self.package = package.map(|p| clip(&p, 200));
        self
    }

    fn platform(rule: &str, path: Option<&str>, detail: String, blocking: bool) -> Finding {
        let key = match path {
            Some(p) => format!("{rule}:{p}"),
            None => rule.to_string(),
        };
        let severity = if blocking {
            Severity::High
        } else {
            Severity::Low
        };
        Finding::of("pigeonpost", rule, severity, &key, detail, blocking).at(path)
    }

    fn blocker(rule: &str, path: Option<&str>, detail: impl Into<String>) -> Finding {
        Finding::platform(rule, path, detail.into(), true)
    }

    fn advisory(rule: &str, path: Option<&str>, detail: impl Into<String>) -> Finding {
        Finding::platform(rule, path, detail.into(), false)
    }
}

/// One analysis stage's outcome, as the postbox records it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StageReport {
    /// `deterministic_scan`, `behavioral_test` or `ai_analysis`.
    pub stage: &'static str,
    /// `passed` or `failed`; always `advisory` for `ai_analysis`.
    pub decision: &'static str,
    pub reason: String,
    pub findings: Vec<Finding>,
    /// Stage-specific facts (what ran, against what, with which model).
    pub evidence: Value,
}

impl StageReport {
    fn decided(stage: &'static str, findings: Vec<Finding>, evidence: Value) -> StageReport {
        let blockers = findings.iter().filter(|f| f.blocking).count();
        let (decision, reason) = if blockers == 0 {
            ("passed", "no blocking findings".to_string())
        } else {
            ("failed", format!("{blockers} blocking finding(s)"))
        };
        StageReport {
            stage,
            decision,
            reason,
            findings,
            evidence,
        }
    }

    /// The evidence document exactly as hashed and stored: the stage, decision, findings and
    /// facts in canonical JSON.
    pub fn evidence_json(&self) -> String {
        canonical_json(&json!({
            "stage": self.stage,
            "decision": self.decision,
            "reason": self.reason,
            "findings": self.findings,
            "evidence": self.evidence,
        }))
    }

    pub fn evidence_digest(&self) -> String {
        digest_of(self.evidence_json().as_bytes())
    }
}

// ---- deterministic scan ---------------------------------------------------------------------

/// Patterns that are a credential wherever they appear: a fixed prefix and the shortest run of
/// token characters after it that counts.
const SECRET_PREFIXES: &[(&str, &str, usize)] = &[
    ("private_key", "-----BEGIN ", 0),
    ("aws_access_key", "AKIA", 16),
    ("github_token", "ghp_", 30),
    ("github_token", "github_pat_", 30),
    ("slack_token", "xoxb-", 20),
    ("slack_token", "xoxp-", 20),
    ("openai_style_key", "sk-", 32),
    ("pigeonpost_api_key", "pk_live_", 16),
    ("pigeonpost_org_key", "pk_org_", 16),
    ("pigeonpost_capability", "cap_", 32),
    ("pigeonpost_service_token", "svc_", 32),
    ("nests_broker_token", "nbt_", 32),
];

fn token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// Secrets in one file's bytes.
pub fn secrets_in(path: &str, bytes: &[u8]) -> Vec<Finding> {
    let mut out = Vec::new();
    for (rule, prefix, min) in SECRET_PREFIXES {
        let p = prefix.as_bytes();
        let mut i = 0;
        while i + p.len() <= bytes.len() {
            if &bytes[i..i + p.len()] != p {
                i += 1;
                continue;
            }
            let hit = if *rule == "private_key" {
                bytes[i + p.len()..]
                    .iter()
                    .take(40)
                    .copied()
                    .collect::<Vec<u8>>()
                    .windows(11)
                    .any(|w| w == b"PRIVATE KEY")
            } else {
                // A word boundary before the prefix, then enough token characters.
                let boundary = i == 0 || !token_char(bytes[i - 1]);
                let run = bytes[i + p.len()..]
                    .iter()
                    .take_while(|b| token_char(**b))
                    .count();
                boundary && run >= *min
            };
            if hit {
                out.push(Finding::blocker(
                    &format!("secret.{rule}"),
                    Some(path),
                    "a credential is committed to the source tree",
                ));
                break;
            }
            i += p.len();
        }
    }
    out
}

/// The manifest against the pool's limits.
pub fn check_manifest(m: &Manifest) -> Vec<Finding> {
    let mut out = Vec::new();
    for cap in &m.capabilities {
        if !ALLOWED_CAPABILITIES.contains(&cap.as_str()) {
            out.push(Finding::blocker(
                "manifest.capability",
                Some(MANIFEST_FILE),
                format!("capability {cap} is not available in the marketplace pool"),
            ));
        }
    }
    for call in &m.syscalls {
        let rule = if FORBIDDEN_SYSCALLS.contains(&call.as_str()) {
            "manifest.forbidden_syscall"
        } else {
            "manifest.syscall"
        };
        // Any extra syscall is refused in M1; the forbidden ones are named as such.
        out.push(Finding::blocker(
            rule,
            Some(MANIFEST_FILE),
            format!("syscall {call} is not available in the marketplace pool"),
        ));
    }
    for dest in &m.egress {
        if !ALLOWED_EGRESS.contains(&dest.as_str()) {
            out.push(Finding::blocker(
                "manifest.egress",
                Some(MANIFEST_FILE),
                format!("egress to {dest:?}: only the broker is reachable"),
            ));
        }
    }
    if m.external_writes {
        out.push(Finding::blocker(
            "manifest.external_writes",
            Some(MANIFEST_FILE),
            "external writes are not offered in M1",
        ));
    }
    out
}

/// Whether an SPDX expression can be satisfied from the allowlist. `OR` needs one side, `AND`
/// both; `WITH` exceptions are refused (none is on the list).
pub fn licence_allowed(expr: &str) -> Result<bool, String> {
    let spaced = expr.replace('(', " ( ").replace(')', " ) ");
    let tokens: Vec<&str> = spaced.split_whitespace().collect();
    let mut pos = 0;
    let value = or_expr(&tokens, &mut pos)?;
    if pos != tokens.len() {
        return Err(format!("unexpected {:?}", tokens[pos]));
    }
    Ok(value)
}

fn or_expr(t: &[&str], pos: &mut usize) -> Result<bool, String> {
    let mut v = and_expr(t, pos)?;
    while t.get(*pos) == Some(&"OR") {
        *pos += 1;
        let rhs = and_expr(t, pos)?;
        v = v || rhs;
    }
    Ok(v)
}

fn and_expr(t: &[&str], pos: &mut usize) -> Result<bool, String> {
    let mut v = atom(t, pos)?;
    while t.get(*pos) == Some(&"AND") {
        *pos += 1;
        let rhs = atom(t, pos)?;
        v = v && rhs;
    }
    Ok(v)
}

fn atom(t: &[&str], pos: &mut usize) -> Result<bool, String> {
    match t.get(*pos) {
        Some(&"(") => {
            *pos += 1;
            let v = or_expr(t, pos)?;
            if t.get(*pos) != Some(&")") {
                return Err("unbalanced parentheses".into());
            }
            *pos += 1;
            Ok(v)
        }
        Some(id) if !matches!(*id, "OR" | "AND" | "WITH" | ")") => {
            *pos += 1;
            if t.get(*pos) == Some(&"WITH") {
                *pos += 2;
                return Ok(false);
            }
            Ok(LICENCE_ALLOWLIST.contains(id))
        }
        other => Err(format!("expected a licence, found {other:?}")),
    }
}

/// Every locked package needs a declared licence, and every declaration must be allowed.
pub fn check_licences(m: &Manifest, packages: &[LockedPackage]) -> Vec<Finding> {
    check_licences_in(m, packages, ecosystem::CARGO_LOCK)
}

/// [`check_licences`] for the packages of `lockfile`.
pub fn check_licences_in(m: &Manifest, packages: &[LockedPackage], lockfile: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut check = |name: &str, expr: Option<&String>| match expr {
        None => out.push(Finding::blocker(
            "licence.undeclared",
            Some(lockfile),
            format!("{name}: no licence declared in {MANIFEST_FILE}"),
        )),
        Some(expr) => match licence_allowed(expr) {
            Ok(true) => {}
            Ok(false) => out.push(Finding::blocker(
                "licence.not_allowed",
                Some(lockfile),
                format!("{name}: {expr} is not on the allowlist"),
            )),
            Err(e) => out.push(Finding::blocker(
                "licence.malformed",
                Some(MANIFEST_FILE),
                format!("{name}: {e}"),
            )),
        },
    };
    check(&m.name, Some(&m.licence));
    for p in packages {
        check(&p.name, m.licences.get(&p.name));
    }
    out
}

/// A Node lockfile's own `license` fields against the manifest's declarations: a package whose
/// published licence is not what the manifest says is a blocker (a declaration cannot launder a
/// licence).
pub fn check_npm_declared(m: &Manifest, lock_text: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    let Ok(lock) = serde_json::from_str::<Value>(lock_text) else {
        return out;
    };
    for (path, p) in lock["packages"].as_object().into_iter().flatten() {
        if path.is_empty() || p["dev"] == true {
            continue;
        }
        let name = path.rsplit("node_modules/").next().unwrap_or_default();
        if let (Some(published), Some(declared)) = (p["license"].as_str(), m.licences.get(name)) {
            if published != declared {
                out.push(Finding::blocker(
                    "licence.mismatch",
                    Some(ecosystem::NPM_LOCK),
                    format!("{name}: published as {published}, declared as {declared}"),
                ));
            }
        }
        if p["hasInstallScript"] == true {
            out.push(Finding::advisory(
                "node.install_script",
                Some(ecosystem::NPM_LOCK),
                format!("{name} has an install script; the build plane never runs it"),
            ));
        }
    }
    out
}

/// What Node and Python source asks of the host beyond computing: spawning processes, loading
/// native code, and literal network endpoints. Advisory: the pool's seccomp profile and network
/// policy enforce, these tell the reviewer where to look.
pub fn source_signals(path: &str, text: &str) -> Vec<Finding> {
    let lang = if path.ends_with(".js") || path.ends_with(".mjs") || path.ends_with(".cjs") {
        "node"
    } else if path.ends_with(".py") {
        "python"
    } else {
        return Vec::new();
    };
    let spawn: &[&str] = match lang {
        "node" => &[
            "child_process",
            "worker_threads",
            "process.dlopen",
            "node:cluster",
        ],
        _ => &[
            "subprocess",
            "os.system",
            "os.popen",
            "ctypes",
            "multiprocessing",
            "os.exec",
        ],
    };
    let mut out = Vec::new();
    for needle in spawn {
        if text.contains(needle) {
            out.push(Finding::advisory(
                "capability.process_or_native",
                Some(path),
                format!("uses {needle}"),
            ));
        }
    }
    for scheme in ["http://", "https://", "ws://", "wss://"] {
        for (i, _) in text.match_indices(scheme) {
            let host: String = text[i + scheme.len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'))
                .collect();
            // The broker's address comes from the environment, never from the source.
            if !host.is_empty() && !matches!(host.as_str(), "localhost" | "127.0.0.1") {
                out.push(Finding::advisory(
                    "egress.literal_endpoint",
                    Some(path),
                    format!("names {scheme}{host}; only the broker is reachable"),
                ));
                break;
            }
        }
    }
    out
}

/// The whole deterministic stage over a source tree.
pub fn deterministic_scan(src: &Path) -> Result<StageReport, String> {
    let files = read_tree(src)?;
    let manifest = Manifest::parse(
        &std::fs::read_to_string(src.join(MANIFEST_FILE))
            .map_err(|e| format!("{MANIFEST_FILE}: {e}"))?,
    )?;
    let eco = ecosystem::detect(src)?;
    let packages = ecosystem::locked(eco, src)?;
    let mut findings = Vec::new();
    for f in &files {
        let bytes = std::fs::read(src.join(&f.path)).map_err(|e| e.to_string())?;
        findings.extend(secrets_in(&f.path, &bytes));
        if eco != Ecosystem::Rust {
            findings.extend(source_signals(&f.path, &String::from_utf8_lossy(&bytes)));
        }
    }
    findings.extend(check_manifest(&manifest));
    findings.extend(check_licences_in(&manifest, &packages, eco.lockfile()));
    if eco == Ecosystem::Node {
        let lock =
            std::fs::read_to_string(src.join(ecosystem::NPM_LOCK)).map_err(|e| e.to_string())?;
        findings.extend(check_npm_declared(&manifest, &lock));
    }
    Ok(StageReport::decided(
        "deterministic_scan",
        findings,
        json!({
            "files": files.len(),
            "source_digest": crate::release::tree_digest(&files),
            "ecosystem": eco.name(),
            "packages": packages.len(),
            "rules": ["secrets", "capabilities", "syscalls", "egress", "external_writes", "licences"],
        }),
    ))
}

// ---- behaviour test with honeytokens ------------------------------------------------------------

/// Tokens planted where a release has no business looking.
#[derive(Debug, Clone)]
pub struct Honeytokens {
    pub tokens: Vec<String>,
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(bytes: &[u8], url: bool) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..=chunk.len() {
            let c = B64[((n >> (18 - 6 * i)) & 63) as usize];
            out.push(match (url, c) {
                (true, b'+') => '-',
                (true, b'/') => '_',
                _ => c as char,
            });
        }
    }
    out
}

impl Honeytokens {
    /// `n` fresh tokens. Each looks like a credential worth stealing.
    pub fn new(n: usize) -> Honeytokens {
        Honeytokens {
            tokens: (0..n)
                .map(|_| format!("pphoney_{}", crate::util::random_hex(16)))
                .collect(),
        }
    }

    /// Plant them in a Nest volume: a fake credentials file, a fake export and a fake config.
    pub fn plant(&self, volume: &Path) -> Result<Vec<PathBuf>, String> {
        let places = [
            ".config/credentials",
            "export/customers.csv",
            "run/upstream.env",
        ];
        let mut planted = Vec::new();
        for (i, place) in places.iter().enumerate() {
            let token = &self.tokens[i % self.tokens.len()];
            let path = volume.join(place);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(&path, format!("api_key={token}\n")).map_err(|e| e.to_string())?;
            planted.push(path);
        }
        Ok(planted)
    }

    /// Each token as it might leave: raw, base64 (standard and URL-safe, from any alignment),
    /// hex in both cases, and reversed.
    pub fn needles(&self) -> Vec<String> {
        self.tokens.iter().flat_map(|t| encodings(t)).collect()
    }

    /// Findings for every channel that carries any needle.
    pub fn leaks(&self, channels: &[(String, Vec<u8>)]) -> Vec<Finding> {
        let needles = self.needles();
        let mut out = Vec::new();
        for (channel, bytes) in channels {
            if carries(bytes, &needles) {
                out.push(Finding::blocker(
                    "behaviour.exfiltration",
                    None,
                    format!("a planted honeytoken left through {channel}"),
                ));
            }
        }
        out
    }
}

/// A secret as it might leave: raw, reversed, hex in both cases, URL-encoded (every byte, both
/// cases), and base64 (standard and URL-safe, from any alignment: the middle of an encoding is
/// stable whatever precedes the token).
pub fn encodings(t: &str) -> Vec<String> {
    let mut out = vec![t.to_string(), t.chars().rev().collect()];
    let hex = crate::util::hex(t.as_bytes());
    out.push(hex.to_uppercase());
    out.push(hex);
    let pct: String = t.bytes().map(|b| format!("%{b:02X}")).collect();
    out.push(pct.to_lowercase());
    out.push(pct);
    for skip in 0..3 {
        let mut padded = vec![b'x'; skip];
        padded.extend_from_slice(t.as_bytes());
        for url in [false, true] {
            let enc = base64(&padded, url);
            let start = (skip * 4).div_ceil(3) + 2;
            let end = enc.len().saturating_sub(4);
            if end > start + 16 {
                out.push(enc[start..end].to_string());
            }
        }
    }
    out
}

/// Whether `bytes` carry any needle: as they are, and with all whitespace (and escaped line
/// breaks) removed, so an encoding wrapped or split across lines is still found.
pub fn carries(bytes: &[u8], needles: &[String]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    if needles.iter().any(|n| text.contains(n.as_str())) {
        return true;
    }
    let joined: String = text
        .replace("\\r\\n", "")
        .replace("\\n", "")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    needles.iter().any(|n| joined.contains(n.as_str()))
}

/// The behaviour stage: the release ran `jobs` operations, every channel out of it was captured,
/// and none may carry a honeytoken. A run in which the release did not answer its jobs fails too,
/// because silence proves nothing.
pub fn behaviour_report(
    tokens: &Honeytokens,
    channels: &[(String, Vec<u8>)],
    jobs: usize,
    answered: usize,
    runtime: &str,
) -> StageReport {
    let mut findings = tokens.leaks(channels);
    if jobs == 0 || answered < jobs {
        findings.push(Finding::blocker(
            "behaviour.unanswered",
            None,
            format!("{answered} of {jobs} test jobs answered"),
        ));
    }
    StageReport::decided(
        "behavioral_test",
        findings,
        json!({
            "runtime": runtime,
            "honeytokens": tokens.tokens.len(),
            "channels": channels.iter().map(|(c, b)| json!({"name": c, "bytes": b.len()})).collect::<Vec<_>>(),
            "jobs": jobs,
            "answered": answered,
        }),
    )
}

impl StageReport {
    /// This stage with more findings and one more fact under `key`, decided again. The AI stage
    /// stays advisory whatever is added.
    pub fn extended(mut self, more: Vec<Finding>, key: &str, facts: Value) -> StageReport {
        self.findings.extend(more);
        if let Some(obj) = self.evidence.as_object_mut() {
            obj.insert(key.to_string(), facts);
        }
        if self.decision == "advisory" {
            return self;
        }
        StageReport::decided(self.stage, self.findings, self.evidence)
    }
}

// ---- egress diff (behaviour test) -----------------------------------------------------------------

/// One network attempt the sandbox saw: the syscall, the address family and the destination.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Attempt {
    pub syscall: String,
    pub family: String,
    pub addr: String,
    pub port: Option<u16>,
}

/// The syscalls the behaviour run's gVisor strace records (`runsc-behaviour`).
pub const TRACED_SYSCALLS: [&str; 4] = ["connect", "sendto", "sendmsg", "sendmmsg"];

fn field_after<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let i = line.find(key)? + key.len();
    let rest = &line[i..];
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    Some(rest[..end].trim().trim_matches('"'))
}

/// Network attempts in gVisor's strace output (the sentry's debug log): every traced syscall
/// that names a socket address, e.g.
/// `… python3 E connect(0x3 socket:[1], 0x7f… {Family: AF_INET, Addr: 93.184.216.34, Port: 443}, 0x10)`.
/// Entry and exit lines of one call are the same attempt.
pub fn parse_strace(text: &str) -> Vec<Attempt> {
    let mut out = std::collections::BTreeSet::new();
    for line in text.lines() {
        let Some(syscall) = TRACED_SYSCALLS
            .iter()
            .find(|s| line.contains(&format!(" {s}(")))
        else {
            continue;
        };
        let Some(family) = field_after(line, "Family: ") else {
            continue;
        };
        let addr = field_after(line, "Addr: ").unwrap_or("").to_string();
        let port = field_after(line, "Port: ").and_then(|p| p.parse::<u16>().ok());
        out.insert(Attempt {
            syscall: syscall.to_string(),
            family: family.to_string(),
            addr,
            port,
        });
    }
    out.into_iter().collect()
}

fn loopback(addr: &str) -> bool {
    addr.starts_with("127.")
        || addr == "::1"
        || addr == "0.0.0.0"
        || addr == "::"
        || addr.starts_with("::ffff:127.")
}

/// Most egress findings one run reports; the evidence keeps the count.
const MAX_EGRESS_FINDINGS: usize = 20;

/// Declared egress against what the run attempted. Unix and netlink sockets stay inside the
/// sandbox (`--host-uds=none`) and so do loopback connections, except DNS: a name lookup is a
/// way out. The build plane has no broker, so any other address is undeclared, whatever the
/// manifest declares (only `broker` is ever allowed). Every undeclared attempt blocks.
pub fn egress_findings(declared: &[String], attempts: &[Attempt]) -> Vec<Finding> {
    let mut out = Vec::new();
    for a in attempts {
        if !matches!(a.family.as_str(), "AF_INET" | "AF_INET6") {
            continue;
        }
        let port = a.port.map(|p| p.to_string()).unwrap_or_else(|| "?".into());
        let (rule, message) = if a.port == Some(53) {
            (
                "egress.dns",
                format!("a DNS lookup ({}) to {}:53", a.syscall, a.addr),
            )
        } else if loopback(&a.addr) {
            continue;
        } else {
            (
                "egress.undeclared",
                format!(
                    "{} to {}:{port}; the release declares {}",
                    a.syscall,
                    a.addr,
                    if declared.is_empty() {
                        "no egress".to_string()
                    } else {
                        declared.join(", ")
                    }
                ),
            )
        };
        let key = format!("{}:{port}", a.addr);
        if out.iter().any(|f: &Finding| f.id.ends_with(&key)) {
            continue;
        }
        out.push(Finding::of(
            "gvisor",
            rule,
            Severity::High,
            &key,
            message,
            true,
        ));
        if out.len() >= MAX_EGRESS_FINDINGS {
            break;
        }
    }
    out
}

/// The egress monitor saw nothing it could read. Blocking when it was configured (a missing log
/// is not a clean run); advisory when this build plane has no monitor.
pub fn egress_unobserved(configured: bool) -> Finding {
    if configured {
        Finding::blocker(
            "egress.unobserved",
            None,
            "the sandbox's network trace is missing; the run proves nothing",
        )
    } else {
        Finding::advisory(
            "egress.unobserved",
            None,
            "this build plane does not trace network attempts",
        )
    }
}

// ---- resource use (behaviour test) ----------------------------------------------------------------

/// What one Nest may use: the marketplace pool's per-Nest limits. A manifest declares no
/// resources of its own today, so the pool's limits are what it is held to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub memory_mib: u64,
    pub cpu_millis: u64,
    /// The longest one job may take.
    pub job_seconds: u64,
}

/// What the behaviour run used, measured on the sandbox's cgroup (sentry and gofer included).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    pub peak_memory_bytes: u64,
    /// CPU time from the start of the run to its end.
    pub cpu_usec: u64,
    pub active_ms: u64,
    /// The longest time between one answer and the next (the first from the start).
    pub slowest_job_ms: Option<u64>,
}

/// Over the budget is advisory; this many times over blocks.
pub const FAR_OVER: f64 = 2.0;

/// Usage against the budget, per dimension.
pub fn resource_findings(b: &Budget, u: &Usage) -> Vec<Finding> {
    let mut out = Vec::new();
    let millis = if u.active_ms == 0 {
        0
    } else {
        u.cpu_usec / u.active_ms
    };
    let dims = [
        (
            "resources.memory",
            (u.peak_memory_bytes >> 20) as f64,
            b.memory_mib as f64,
            format!("peak memory {} MiB", u.peak_memory_bytes >> 20),
            format!("{} MiB", b.memory_mib),
        ),
        (
            "resources.cpu",
            millis as f64,
            b.cpu_millis as f64,
            format!("average CPU {millis} millicores while working"),
            format!("{} millicores", b.cpu_millis),
        ),
        (
            "resources.time",
            u.slowest_job_ms.unwrap_or(0) as f64 / 1000.0,
            b.job_seconds as f64,
            format!(
                "slowest job {:.1} s",
                u.slowest_job_ms.unwrap_or(0) as f64 / 1000.0
            ),
            format!("{} s", b.job_seconds),
        ),
    ];
    for (rule, used, limit, what, of) in dims {
        if limit <= 0.0 || used <= limit {
            continue;
        }
        let ratio = used / limit;
        let far = ratio > FAR_OVER;
        out.push(Finding::of(
            "cgroup",
            rule,
            if far {
                Severity::High
            } else {
                Severity::Medium
            },
            rule,
            format!("{what}, {ratio:.1}x the limit of {of}"),
            far,
        ));
    }
    out
}

// ---- buyer data (behaviour test) ------------------------------------------------------------------

/// A per-job marker planted in each test job's input, standing in for one buyer's data: random
/// and high-entropy, with no fixed prefix a release could look for.
pub fn canary() -> String {
    const A: &[u8; 62] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let hex = crate::util::random_hex(24);
    hex.as_bytes()
        .chunks(2)
        .map(|c| {
            let v = u8::from_str_radix(std::str::from_utf8(c).unwrap_or("00"), 16).unwrap_or(0);
            A[(v as usize) % A.len()] as char
        })
        .collect()
}

/// Buyer data must stay with its own job: a job's canary in another job's result, or any canary
/// in the release's own output (its logs), blocks. `jobs` is `(job id, canary)`; `results` is
/// `(job id, result bytes)`.
pub fn canary_findings(
    jobs: &[(String, Option<String>)],
    results: &[(String, Vec<u8>)],
    logs: &[u8],
) -> Vec<Finding> {
    let mut out = Vec::new();
    for (job, canary) in jobs {
        let Some(c) = canary else { continue };
        let needles = encodings(c);
        if carries(logs, &needles) {
            out.push(Finding::blocker(
                "data.in_logs",
                None,
                format!("the input of {job} appears in the release's output"),
            ));
        }
        for (other, bytes) in results {
            if other != job && carries(bytes, &needles) {
                out.push(Finding::blocker(
                    "data.cross_job",
                    None,
                    format!("the input of {job} appears in the result of {other}"),
                ));
            }
        }
    }
    out
}

// ---- AI review --------------------------------------------------------------------------------

/// Where the AI review sends its prompt: an OpenAI-compatible chat route on mswarm's self-hosted
/// load balancer.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRouteConfig {
    pub url: String,
    /// File holding the route's key; read by the analysis process only.
    pub key_file: PathBuf,
    pub model: String,
}

/// Hosts the review may never be sent to, whatever the configuration says: publisher source stays
/// on self-hosted models (OD-10).
const CLOUD_MODEL_HOSTS: &[&str] = &[
    "openrouter",
    "openai.com",
    "anthropic.com",
    "deepseek.com",
    "googleapis.com",
    "mistral.ai",
    "together.xyz",
    "groq.com",
];

impl ReviewRouteConfig {
    pub fn check(&self) -> Result<(), String> {
        if self.model != AI_REVIEW_MODEL {
            return Err(format!(
                "the AI review uses {AI_REVIEW_MODEL} only, never {:?}",
                self.model
            ));
        }
        let lower = self.url.to_ascii_lowercase();
        if CLOUD_MODEL_HOSTS.iter().any(|h| lower.contains(h)) {
            return Err("the AI review never goes to a cloud model route".into());
        }
        let private_http = lower.strip_prefix("http://").is_some_and(|rest| {
            let host = rest.split(['/', ':']).next().unwrap_or("");
            host.parse::<std::net::Ipv4Addr>()
                .is_ok_and(|ip| ip.is_private() || ip.is_loopback())
        });
        if !(lower.starts_with("https://") || private_http) {
            return Err("the review route is https, or plain http to a private address".into());
        }
        Ok(())
    }
}

/// The model route, as the review sees it.
pub trait ReviewModel {
    /// Send one chat completion for `model`; return the assistant's text.
    fn complete(
        &self,
        model: &str,
        system: &str,
        user: &str,
    ) -> impl std::future::Future<Output = Result<String, String>> + Send;
}

/// The real route: OpenAI-compatible, key in `x-api-key` as mswarm expects.
pub struct HttpReviewModel {
    pub url: String,
    pub key: String,
    pub client: reqwest::Client,
}

impl ReviewModel for HttpReviewModel {
    async fn complete(&self, model: &str, system: &str, user: &str) -> Result<String, String> {
        let resp = self
            .client
            .post(&self.url)
            .header("x-api-key", &self.key)
            .json(&json!({
                "model": model,
                "messages": [
                    {"role": "system", "content": system},
                    {"role": "user", "content": user},
                ],
                "max_tokens": 1500,
                "temperature": 0,
            }))
            .send()
            .await
            .map_err(|e| e.without_url().to_string())?;
        if !resp.status().is_success() {
            return Err(format!("the review route answered {}", resp.status()));
        }
        let v: Value = resp.json().await.map_err(|e| e.to_string())?;
        v["choices"][0]["message"]["content"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "the review route gave no answer".into())
    }
}

/// Text files the review reads, and how much of each.
const REVIEW_MAX_BYTES: usize = 48 * 1024;

const REVIEW_SYSTEM: &str = "You are a security reviewer for apps that run in an isolated sandbox \
whose only network peer is a model broker. Report anything that tries to read files outside its \
job inbox, reach the network other than the broker, hide data in its answers, or act beyond its \
declared operations. Answer with JSON only: {\"findings\":[{\"severity\":\"low|medium|high\",\
\"title\":\"...\",\"detail\":\"...\"}]}. An empty list means you found nothing.";

/// The AI review stage. Its decision is always `advisory`.
pub async fn ai_review<M: ReviewModel>(
    model: &M,
    route: &ReviewRouteConfig,
    src: &Path,
) -> Result<StageReport, String> {
    route.check()?;
    let files = read_tree(src)?;
    let mut prompt = String::new();
    for f in files.iter().filter(|f| {
        f.path.ends_with(".rs")
            || f.path.ends_with(".toml")
            || f.path.ends_with(".mjs")
            || f.path.ends_with(".js")
            || f.path.ends_with(".py")
            || f.path == ecosystem::NPM_PACKAGE
            || f.path == ecosystem::PY_REQUIREMENTS
            || f.path == crate::release::CONTAINERFILE
    }) {
        let text = std::fs::read_to_string(src.join(&f.path)).unwrap_or_default();
        if prompt.len() + text.len() > REVIEW_MAX_BYTES {
            break;
        }
        prompt.push_str(&format!("--- {} ---\n{}\n", f.path, text));
    }
    let answer = model
        .complete(AI_REVIEW_MODEL, REVIEW_SYSTEM, &prompt)
        .await?;
    let parsed: Option<Value> = answer
        .find('{')
        .and_then(|start| answer.rfind('}').map(|end| &answer[start..=end]))
        .and_then(|s| serde_json::from_str(s).ok());
    let findings: Vec<Finding> = parsed
        .as_ref()
        .and_then(|v| v["findings"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .take(50)
        .enumerate()
        .map(|(i, f)| {
            let (level, severity) = match f["severity"].as_str() {
                Some("high") => ("high", Severity::High),
                Some("medium") => ("medium", Severity::Medium),
                _ => ("low", Severity::Low),
            };
            let title: String = f["title"]
                .as_str()
                .unwrap_or("finding")
                .chars()
                .take(200)
                .collect();
            let detail: String = f["detail"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(700)
                .collect();
            // Advisory: the staff reviewer weighs these; they block nothing by themselves.
            Finding::of(
                "ai-review",
                &format!("ai.{level}"),
                severity,
                &format!("{i}"),
                format!("{title}: {detail}"),
                false,
            )
        })
        .collect();
    let high = findings.iter().filter(|f| f.rule == "ai.high").count();
    Ok(StageReport {
        stage: "ai_analysis",
        decision: "advisory",
        reason: if parsed.is_some() {
            format!("{} finding(s), {high} high; advisory only", findings.len())
        } else {
            "the model's answer was not the requested JSON; advisory only".into()
        },
        findings,
        evidence: json!({
            "model": AI_REVIEW_MODEL,
            "route": "mswarm self-hosted",
            "prompt_digest": digest_of(prompt.as_bytes()),
            "answer_digest": digest_of(answer.as_bytes()),
            "answer_parsed": parsed.is_some(),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release::reference_source;
    use std::sync::Mutex;

    #[test]
    fn the_reference_release_passes_the_deterministic_scan() {
        let report = deterministic_scan(&reference_source()).unwrap();
        assert_eq!(report.decision, "passed", "{:?}", report.findings);
        assert!(report.findings.is_empty());
        assert_eq!(
            report.evidence_digest(),
            digest_of(report.evidence_json().as_bytes())
        );
    }

    #[test]
    fn seeded_secrets_are_found() {
        let key = format!("-----BEGIN OPENSSH {}-----\nabc\n", "PRIVATE KEY");
        let cases = [
            (key.as_str(), "secret.private_key"),
            ("aws = \"AKIAABCDEFGHIJKLMNOP\"", "secret.aws_access_key"),
            (
                "let k = \"pk_live_0123456789abcdef01\";",
                "secret.pigeonpost_api_key",
            ),
            (
                "token: nbt_0123456789abcdef0123456789abcdef",
                "secret.nests_broker_token",
            ),
            (
                "Authorization: cap_0123456789abcdef0123456789abcdef",
                "secret.pigeonpost_capability",
            ),
        ];
        for (text, rule) in cases {
            let found = secrets_in("src/x.rs", text.as_bytes());
            assert!(
                found.iter().any(|f| f.rule == rule && f.blocking),
                "{text}: {found:?}"
            );
        }
        // Words that merely contain a prefix are not secrets.
        for text in [
            "fn task_runner() {}",
            "risk-free",
            "escape_hatch",
            "pk_live_",
        ] {
            assert!(secrets_in("src/x.rs", text.as_bytes()).is_empty(), "{text}");
        }
    }

    fn manifest() -> Manifest {
        Manifest::parse(&std::fs::read_to_string(reference_source().join(MANIFEST_FILE)).unwrap())
            .unwrap()
    }

    #[test]
    fn capabilities_syscalls_egress_and_writes_beyond_the_pool_are_blockers() {
        assert!(check_manifest(&manifest()).is_empty());
        let mut m = manifest();
        m.capabilities = vec!["CAP_NET_RAW".into()];
        m.syscalls = vec!["ptrace".into(), "clock_adjtime".into()];
        m.egress = vec!["broker".into(), "internet".into()];
        m.external_writes = true;
        let rules: Vec<String> = check_manifest(&m).into_iter().map(|f| f.rule).collect();
        for rule in [
            "manifest.capability",
            "manifest.forbidden_syscall",
            "manifest.syscall",
            "manifest.egress",
            "manifest.external_writes",
        ] {
            assert!(rules.iter().any(|r| r == rule), "{rule}: {rules:?}");
        }
    }

    #[test]
    fn licences_follow_the_allowlist_and_spdx_logic() {
        for (expr, ok) in [
            ("MIT", true),
            ("MIT OR Apache-2.0", true),
            ("(MIT OR Apache-2.0) AND Unicode-3.0", true),
            ("GPL-3.0-only", false),
            ("GPL-3.0-only OR MIT", true),
            ("GPL-3.0-only AND MIT", false),
            ("AGPL-3.0-or-later", false),
            ("Apache-2.0 WITH LLVM-exception", false),
        ] {
            assert_eq!(licence_allowed(expr), Ok(ok), "{expr}");
        }
        assert!(licence_allowed("(MIT").is_err());
        assert!(licence_allowed("MIT OR").is_err());
        let mut m = manifest();
        m.licences.remove("serde_json");
        m.licences.insert("zmij".into(), "SSPL-1.0".into());
        let packages = crate::release::locked_packages(
            &std::fs::read_to_string(reference_source().join("Cargo.lock")).unwrap(),
        )
        .unwrap();
        let rules: Vec<String> = check_licences(&m, &packages)
            .into_iter()
            .map(|f| f.rule)
            .collect();
        assert!(
            rules.contains(&"licence.undeclared".to_string()),
            "{rules:?}"
        );
        assert!(
            rules.contains(&"licence.not_allowed".to_string()),
            "{rules:?}"
        );
    }

    #[test]
    fn honeytoken_needles_catch_plain_and_encoded_leaks() {
        let tokens = Honeytokens::new(1);
        let t = tokens.tokens[0].clone();
        let encoded = [
            t.clone(),
            format!("prefix {t} suffix"),
            base64(t.as_bytes(), false),
            base64(format!("k={t}").as_bytes(), true),
            base64(format!("ab{t}").as_bytes(), false),
            crate::util::hex(t.as_bytes()),
            crate::util::hex(t.as_bytes()).to_uppercase(),
            t.chars().rev().collect(),
        ];
        for leak in encoded {
            let found = tokens.leaks(&[("broker".into(), leak.clone().into_bytes())]);
            assert_eq!(found.len(), 1, "{leak}");
        }
        assert!(tokens
            .leaks(&[(
                "broker".into(),
                b"an ordinary answer about annual leave".to_vec()
            )])
            .is_empty());
    }

    #[test]
    fn honeytokens_are_planted_on_the_volume() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = Honeytokens::new(2);
        let planted = tokens.plant(dir.path()).unwrap();
        assert_eq!(planted.len(), 3);
        let all: String = planted
            .iter()
            .map(|p| std::fs::read_to_string(p).unwrap())
            .collect();
        assert!(tokens.tokens.iter().all(|t| all.contains(t.as_str())));
    }

    #[test]
    fn a_silent_release_fails_the_behaviour_stage() {
        let tokens = Honeytokens::new(1);
        let report = behaviour_report(&tokens, &[], 3, 0, "in-process");
        assert_eq!(report.decision, "failed");
        let report = behaviour_report(&tokens, &[], 3, 3, "in-process");
        assert_eq!(report.decision, "passed");
    }

    #[test]
    fn the_strace_parser_finds_each_network_attempt_once() {
        let attempts = parse_strace(include_str!("../tests/fixtures/scanners/strace.log"));
        let shown: Vec<String> = attempts
            .iter()
            .map(|a| format!("{} {} {} {:?}", a.syscall, a.family, a.addr, a.port))
            .collect();
        assert_eq!(attempts.len(), 6, "{shown:#?}");
        assert!(shown.contains(&"connect AF_INET 93.184.216.34 Some(443)".to_string()));
        assert!(shown.contains(&"sendto AF_INET 127.0.0.53 Some(53)".to_string()));
        assert!(shown.contains(&"connect AF_INET6 2001:db8::7 Some(8443)".to_string()));
        assert!(shown.contains(&"connect AF_UNIX /tmp/app.sock None".to_string()));
    }

    #[test]
    fn undeclared_egress_and_dns_block_while_in_sandbox_traffic_does_not() {
        let attempts = parse_strace(include_str!("../tests/fixtures/scanners/strace.log"));
        let f = egress_findings(&["broker".into()], &attempts);
        let mut ids: Vec<&str> = f.iter().map(|x| x.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(
            ids,
            vec![
                "gvisor:127.0.0.53:53",
                "gvisor:2001:db8::7:8443",
                "gvisor:93.184.216.34:443"
            ],
            "{f:#?}"
        );
        assert!(f.iter().all(|x| x.blocking && x.tool == "gvisor"));
        let by = |id: &str| f.iter().find(|x| x.id == id).unwrap();
        assert_eq!(by("gvisor:127.0.0.53:53").rule, "egress.dns");
        let web = by("gvisor:93.184.216.34:443");
        assert_eq!(web.rule, "egress.undeclared");
        assert!(web.message.contains("declares broker"), "{}", web.message);
        // Loopback, Unix and netlink only: nothing.
        let quiet = [
            Attempt {
                syscall: "connect".into(),
                family: "AF_INET".into(),
                addr: "127.0.0.1".into(),
                port: Some(8080),
            },
            Attempt {
                syscall: "connect".into(),
                family: "AF_UNIX".into(),
                addr: "/tmp/x".into(),
                port: None,
            },
        ];
        assert!(egress_findings(&["broker".into()], &quiet).is_empty());
        assert!(egress_unobserved(true).blocking);
        assert!(!egress_unobserved(false).blocking);
    }

    #[test]
    fn resource_use_over_the_budget_is_advisory_and_far_over_blocks() {
        let b = Budget {
            memory_mib: 512,
            cpu_millis: 1000,
            job_seconds: 30,
        };
        let within = Usage {
            peak_memory_bytes: 400 << 20,
            cpu_usec: 5_000_000,
            active_ms: 10_000,
            slowest_job_ms: Some(9_000),
        };
        assert!(resource_findings(&b, &within).is_empty());
        let over = Usage {
            peak_memory_bytes: 700 << 20,
            cpu_usec: 15_000_000,
            active_ms: 10_000,
            slowest_job_ms: Some(40_000),
        };
        let f = resource_findings(&b, &over);
        assert_eq!(f.len(), 3, "{f:#?}");
        assert!(f
            .iter()
            .all(|x| !x.blocking && x.severity == Severity::Medium));
        let far = Usage {
            peak_memory_bytes: 1100 << 20,
            cpu_usec: 25_000_000,
            active_ms: 10_000,
            slowest_job_ms: Some(61_000),
        };
        let f = resource_findings(&b, &far);
        let rules: Vec<&str> = f
            .iter()
            .filter(|x| x.blocking)
            .map(|x| x.rule.as_str())
            .collect();
        assert_eq!(
            rules,
            vec!["resources.memory", "resources.cpu", "resources.time"]
        );
    }

    #[test]
    fn buyer_data_must_stay_in_its_own_job() {
        let (a, b) = (canary(), canary());
        let jobs = vec![
            ("job_a".to_string(), Some(a.clone())),
            ("job_b".to_string(), Some(b.clone())),
        ];
        let own = vec![
            ("job_a".to_string(), format!("answer for {a}").into_bytes()),
            ("job_b".to_string(), b"answer".to_vec()),
        ];
        assert!(canary_findings(&jobs, &own, b"worker started").is_empty());
        let crossed = vec![("job_b".to_string(), format!("leaked {a}").into_bytes())];
        let f = canary_findings(&jobs, &crossed, format!("log: {b}").as_bytes());
        let rules: Vec<&str> = f.iter().map(|x| x.rule.as_str()).collect();
        assert_eq!(rules, vec!["data.cross_job", "data.in_logs"], "{f:#?}");
        assert!(f.iter().all(|x| x.blocking));
    }

    #[test]
    fn markers_are_found_hex_url_and_base64_encoded_and_split_across_lines() {
        let c = canary();
        let needles = encodings(&c);
        let b64 = base64(format!("input={c}&x=1").as_bytes(), false);
        let split: String = b64
            .as_bytes()
            .chunks(10)
            .map(|ch| format!("{}\n", String::from_utf8_lossy(ch)))
            .collect();
        let pct: String = c.bytes().map(|b| format!("%{b:02x}")).collect();
        for hay in [
            format!("x {} y", crate::util::hex(c.as_bytes())),
            format!("q={pct}"),
            split,
            format!("\"{}\"\\n\"{}\"", &c[..10], &c[10..]).replace("\"\\n\"", "\\n"),
        ] {
            assert!(carries(hay.as_bytes(), &needles), "{hay}");
        }
        assert!(!carries(b"nothing to see", &needles));
    }

    #[test]
    fn an_extended_stage_is_decided_again_but_the_ai_stays_advisory() {
        let r = deterministic_scan(&reference_source()).unwrap();
        assert_eq!(r.decision, "passed");
        let blocker = Finding::of(
            "clamav",
            "malware.signature",
            Severity::Critical,
            "x",
            "m",
            true,
        );
        let r = r.extended(
            vec![blocker.clone()],
            "security_tools",
            json!({ "ran": true }),
        );
        assert_eq!(r.decision, "failed");
        assert_eq!(r.evidence["security_tools"]["ran"], true);
        let ai = StageReport {
            stage: "ai_analysis",
            decision: "advisory",
            reason: String::new(),
            findings: vec![],
            evidence: json!({}),
        };
        assert_eq!(
            ai.extended(vec![blocker], "x", json!(1)).decision,
            "advisory"
        );
    }

    #[test]
    fn findings_serialize_with_the_structured_fields() {
        let f = Finding::of(
            "osv-scanner",
            "vulnerability.dependency",
            Severity::High,
            "GHSA-1:x@1",
            "m",
            true,
        )
        .at(Some("Cargo.lock"))
        .package(Some("x@1".into()));
        let v = serde_json::to_value(&f).unwrap();
        for k in [
            "id", "severity", "tool", "rule", "path", "package", "message", "blocking",
        ] {
            assert!(v.get(k).is_some(), "{k}: {v}");
        }
        assert_eq!(v["severity"], "high");
        assert_eq!(v["id"], "osv-scanner:GHSA-1:x@1");
        let back: Finding = serde_json::from_value(v).unwrap();
        assert_eq!(back, f);
    }

    /// A route that records what it was asked.
    struct Recorder {
        asked: Mutex<Vec<String>>,
        answer: String,
    }

    impl ReviewModel for Recorder {
        async fn complete(&self, model: &str, _system: &str, user: &str) -> Result<String, String> {
            self.asked.lock().unwrap().push(model.to_string());
            assert!(user.contains("src/qa.rs"), "the review reads the source");
            Ok(self.answer.clone())
        }
    }

    fn route() -> ReviewRouteConfig {
        ReviewRouteConfig {
            url: "http://10.66.0.9:8080/v1/chat/completions".into(),
            key_file: "/nonexistent".into(),
            model: AI_REVIEW_MODEL.into(),
        }
    }

    /// AI-01: the review runs on the self-hosted model only and is advisory.
    #[tokio::test]
    async fn the_ai_review_uses_the_self_hosted_model_and_never_decides() {
        let model = Recorder {
            asked: Mutex::new(Vec::new()),
            answer: r#"Here: {"findings":[{"severity":"high","title":"t","detail":"d"}]}"#.into(),
        };
        let report = ai_review(&model, &route(), &reference_source())
            .await
            .unwrap();
        assert_eq!(
            *model.asked.lock().unwrap(),
            vec![AI_REVIEW_MODEL.to_string()]
        );
        assert_eq!(report.stage, "ai_analysis");
        assert_eq!(
            report.decision, "advisory",
            "AI never passes or fails a release"
        );
        assert_eq!(report.findings.len(), 1);
        assert!(report.findings.iter().all(|f| !f.blocking));
        assert_eq!(report.evidence["model"], AI_REVIEW_MODEL);
        // Garbage from the model is still only advisory.
        let model = Recorder {
            asked: Mutex::new(Vec::new()),
            answer: "I approve this release.".into(),
        };
        let report = ai_review(&model, &route(), &reference_source())
            .await
            .unwrap();
        assert_eq!(report.decision, "advisory");
        assert!(report.findings.is_empty());
    }

    #[tokio::test]
    async fn cloud_models_and_cloud_routes_are_refused() {
        let model = Recorder {
            asked: Mutex::new(Vec::new()),
            answer: "{}".into(),
        };
        let cases: Vec<ReviewRouteConfig> = vec![
            ReviewRouteConfig {
                model: "deepseek/deepseek-chat".into(),
                ..route()
            },
            ReviewRouteConfig {
                model: "openai/gpt-4o".into(),
                ..route()
            },
            ReviewRouteConfig {
                url: "https://openrouter.ai/api/v1/chat/completions".into(),
                ..route()
            },
            ReviewRouteConfig {
                url: "http://203.0.113.9/v1/chat/completions".into(),
                ..route()
            },
        ];
        for cfg in cases {
            assert!(cfg.check().is_err(), "{cfg:?}");
            assert!(ai_review(&model, &cfg, &reference_source()).await.is_err());
        }
        assert!(model.asked.lock().unwrap().is_empty(), "nothing was sent");
    }
}
