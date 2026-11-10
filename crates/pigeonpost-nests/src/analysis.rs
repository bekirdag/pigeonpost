//! The analysis plane (build plan M1, R-63/R-64): what a release goes through between its build
//! and a staff decision. Each step produces a [`StageReport`] that the postbox stores as a
//! `review_decisions` row with its evidence.
//!
//! - **deterministic_scan**, offline: secrets in the source tree; capabilities, syscalls, egress
//!   and external writes in the manifest beyond what the pool allows; every locked package's
//!   licence against [`LICENCE_ALLOWLIST`].
//! - **behavioral_test**: honeytokens are planted where the release has no business looking, the
//!   release runs its operations, and every channel out of it (the broker, its job results, its
//!   logs) is searched for the tokens in plain and encoded forms. One hit fails the stage.
//! - **ai_analysis**: a review by the self-hosted model route, model [`AI_REVIEW_MODEL`] and no
//!   other. It is **advisory**: its decision is always `advisory`, it never passes or fails a
//!   release, and its findings are evidence for the staff reviewer (R-64, R-65: AI never approves).
//!
//! Publisher source and evidence never leave this host except as the redacted report to the
//! postbox; the model route is self-hosted.

use crate::release::{
    canonical_json, digest_of, locked_packages, read_tree, LockedPackage, Manifest, MANIFEST_FILE,
};
use serde::Serialize;
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub rule: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub detail: String,
    pub blocker: bool,
}

impl Finding {
    fn blocker(rule: &str, path: Option<&str>, detail: impl Into<String>) -> Finding {
        Finding {
            rule: rule.into(),
            path: path.map(str::to_string),
            detail: detail.into(),
            blocker: true,
        }
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
        let blockers = findings.iter().filter(|f| f.blocker).count();
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
    let mut out = Vec::new();
    let mut check = |name: &str, expr: Option<&String>| match expr {
        None => out.push(Finding::blocker(
            "licence.undeclared",
            Some("Cargo.lock"),
            format!("{name}: no licence declared in {MANIFEST_FILE}"),
        )),
        Some(expr) => match licence_allowed(expr) {
            Ok(true) => {}
            Ok(false) => out.push(Finding::blocker(
                "licence.not_allowed",
                Some("Cargo.lock"),
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

/// The whole deterministic stage over a source tree.
pub fn deterministic_scan(src: &Path) -> Result<StageReport, String> {
    let files = read_tree(src)?;
    let manifest = Manifest::parse(
        &std::fs::read_to_string(src.join(MANIFEST_FILE))
            .map_err(|e| format!("{MANIFEST_FILE}: {e}"))?,
    )?;
    let packages = locked_packages(
        &std::fs::read_to_string(src.join("Cargo.lock")).map_err(|e| format!("Cargo.lock: {e}"))?,
    )?;
    let mut findings = Vec::new();
    for f in &files {
        let bytes = std::fs::read(src.join(&f.path)).map_err(|e| e.to_string())?;
        findings.extend(secrets_in(&f.path, &bytes));
    }
    findings.extend(check_manifest(&manifest));
    findings.extend(check_licences(&manifest, &packages));
    Ok(StageReport::decided(
        "deterministic_scan",
        findings,
        json!({
            "files": files.len(),
            "source_digest": crate::release::tree_digest(&files),
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
        let mut out = Vec::new();
        for t in &self.tokens {
            out.push(t.clone());
            out.push(t.chars().rev().collect());
            let hex = crate::util::hex(t.as_bytes());
            out.push(hex.to_uppercase());
            out.push(hex);
            for skip in 0..3 {
                // The middle of an encoding is stable whatever precedes the token.
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
        }
        out
    }

    /// Findings for every channel that carries any needle.
    pub fn leaks(&self, channels: &[(String, Vec<u8>)]) -> Vec<Finding> {
        let needles = self.needles();
        let mut out = Vec::new();
        for (channel, bytes) in channels {
            let text = String::from_utf8_lossy(bytes);
            if needles.iter().any(|n| text.contains(n.as_str())) {
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
        .map(|f| Finding {
            rule: format!(
                "ai.{}",
                match f["severity"].as_str() {
                    Some("high") => "high",
                    Some("medium") => "medium",
                    _ => "low",
                }
            ),
            path: None,
            detail: format!(
                "{}: {}",
                f["title"]
                    .as_str()
                    .unwrap_or("finding")
                    .chars()
                    .take(200)
                    .collect::<String>(),
                f["detail"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(1000)
                    .collect::<String>()
            ),
            // Advisory: the staff reviewer weighs these; they block nothing by themselves.
            blocker: false,
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
                found.iter().any(|f| f.rule == rule && f.blocker),
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
        let packages = locked_packages(
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
        assert!(report.findings.iter().all(|f| !f.blocker));
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
