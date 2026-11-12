//! The security tools of the deterministic scan (developer program plan §4, "Automated security
//! checks"). Each tool runs on the analysis plane in a gVisor container with **no network**, a
//! read-only root, no capabilities and read-only mounts of what it scans and of its database.
//! The databases are fetched by a separate updater (`deploy/nests/buildd/nestscan-update`, user
//! `nestscan`, its own nftables allowlist) and are only ever read here.
//!
//! | Tool | Scans | Blocks |
//! |---|---|---|
//! | OSV-Scanner | the lockfile and the SBOM, against the local OSV mirror | critical/high with a fix available |
//! | Grype | the built image (OCI archive), against its local database | the same |
//! | ClamAV | source, build artefacts and the image archive | any hit |
//! | YARA | the same, with the vendored rules and the updater's feed | any hit |
//! | Semgrep | the source, with the pinned, vendored ruleset | `ERROR` with `confidence: HIGH` |
//! | gitleaks | the source, with its built-in rules (on top of the platform's own secret rules) | any hit |
//! | heuristics | entropy, packed or minified JavaScript, large base64 blobs, binaries | never (advisory) |
//!
//! A tool that cannot run, a database that is missing or older than `max_db_age_hours`, or a
//! ruleset whose digest is not the pinned one is itself a blocking finding: silence proves
//! nothing.
//!
//! Every finding is a [`Finding`] (id, severity, tool, path or package, message, blocking). The
//! raw tool reports stay on this host and are deleted with the build's scratch directory; gitleaks
//! is run with `--redact` and its matched text is never read.

use crate::analysis::{Finding, Severity};
use crate::sandbox::Podman;
use crate::util::sha256_hex;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// `[scanners]` in `buildd.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScannersConfig {
    /// `localhost/pigeonpost-scanners@sha256:…`, built from `deploy/nests/buildd/scanners/
    /// Containerfile`: osv-scanner, grype, gitleaks and yara, each binary checked against its
    /// pinned SHA-256 at build time.
    pub image: String,
    /// `docker.io/clamav/clamav:<version>_base@sha256:…` (no bundled signatures).
    pub clamav_image: String,
    /// `docker.io/semgrep/semgrep:<version>@sha256:…`.
    pub semgrep_image: String,
    /// The vendored Semgrep ruleset (`deploy/nests/buildd/scanners/semgrep-rules.yml`).
    pub semgrep_rules: PathBuf,
    /// Its SHA-256 (hex). A ruleset with another digest is refused.
    pub semgrep_rules_sha256: String,
    /// The updater's current snapshot: `osv/`, `grype/`, `clamav/`, `yara/` and `manifest.json`.
    pub db_root: PathBuf,
    /// A database older than this fails the scan.
    #[serde(default = "default_max_age")]
    pub max_db_age_hours: u64,
    /// Each tool is stopped after this long.
    #[serde(default = "default_tool_timeout")]
    pub timeout_secs: u64,
    /// Memory for one tool's container (ClamAV holds its signatures in memory: about 1.5 GiB).
    #[serde(default = "default_tool_memory")]
    pub memory_mib: u64,
}

fn default_max_age() -> u64 {
    72
}
fn default_tool_timeout() -> u64 {
    900
}
fn default_tool_memory() -> u64 {
    4096
}

/// The databases a scan reads, by the updater's manifest key and directory name.
pub const DATABASES: [&str; 4] = ["osv", "grype", "clamav", "yara"];

impl ScannersConfig {
    pub fn check(&self) -> Result<(), String> {
        for (name, image) in [
            ("scanners.image", &self.image),
            ("scanners.clamav_image", &self.clamav_image),
            ("scanners.semgrep_image", &self.semgrep_image),
        ] {
            if !crate::buildd::pinned(image) {
                return Err(format!("{name} must be pinned by digest"));
            }
        }
        let sha = &self.semgrep_rules_sha256;
        if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("scanners.semgrep_rules_sha256: 64 hex digits".into());
        }
        for p in [&self.db_root, &self.semgrep_rules] {
            if !p.is_absolute() {
                return Err(format!("{}: an absolute path", p.display()));
            }
        }
        if !(1..=24 * 14).contains(&self.max_db_age_hours) {
            return Err("scanners.max_db_age_hours: 1-336".into());
        }
        if !(30..=3600).contains(&self.timeout_secs) {
            return Err("scanners.timeout_secs: 30-3600".into());
        }
        if self.memory_mib < 1024 {
            return Err("scanners.memory_mib: at least 1024 (ClamAV)".into());
        }
        Ok(())
    }
}

/// What one scan looks at. Paths are on this host; each is mounted read-only.
#[derive(Debug, Clone, Default)]
pub struct Target<'a> {
    /// The publisher's source tree (no `.git`).
    pub src: Option<&'a Path>,
    /// The compiled build context: what the image's publisher layer is made of.
    pub artefacts: Option<&'a Path>,
    /// The release image as an OCI archive (layers stored uncompressed by `podman save`).
    pub image_archive: Option<&'a Path>,
    /// The release SBOM (CycloneDX JSON).
    pub sbom: Option<&'a str>,
    /// The lockfile in `src` (`Cargo.lock`, `package-lock.json`, `requirements.txt`).
    pub lockfile: Option<&'a str>,
    /// Loose files (a hosted service's results): ClamAV and YARA only.
    pub files: Option<&'a Path>,
}

/// Which tools a target gets.
fn wanted(t: &Target<'_>) -> Vec<&'static str> {
    let mut out = Vec::new();
    if t.lockfile.is_some() || t.sbom.is_some() {
        out.push("osv-scanner");
    }
    if t.image_archive.is_some() || t.sbom.is_some() {
        out.push("grype");
    }
    if t.src.is_some() || t.artefacts.is_some() || t.image_archive.is_some() || t.files.is_some() {
        out.push("clamav");
        out.push("yara");
    }
    if t.src.is_some() {
        out.push("semgrep");
        out.push("gitleaks");
    }
    out
}

// ---- blocking policy ------------------------------------------------------------------------------

/// A dependency or image vulnerability blocks when it is critical or high **and** a fixed
/// version exists (the publisher can act on it); everything else is advisory.
pub fn vulnerability_blocks(severity: Severity, fix_available: bool) -> bool {
    severity >= Severity::High && fix_available
}

/// A vulnerability nobody has graded yet: high when a fix exists (so it blocks, rather than
/// slipping through as advisory), medium otherwise.
pub fn unknown_severity(fix_available: bool) -> Severity {
    if fix_available {
        Severity::High
    } else {
        Severity::Medium
    }
}

/// A Semgrep result blocks when its rule says `ERROR` and `confidence: HIGH`.
pub fn sast_blocks(severity: Severity, confidence: &str) -> bool {
    severity >= Severity::High && confidence.eq_ignore_ascii_case("high")
}

// ---- output parsers -------------------------------------------------------------------------------

/// A path inside a scanner container, as the reviewer reads it.
pub fn display_path(p: &str) -> String {
    for (prefix, shown) in [
        ("/scan/src/", ""),
        ("/scan/artefacts/", "artefacts/"),
        ("/scan/files/", "result/"),
        ("/scan/sbom/", "sbom/"),
    ] {
        if let Some(rest) = p.strip_prefix(prefix) {
            return format!("{shown}{rest}");
        }
    }
    if let Some(rest) = p.strip_prefix("/scan/layers/") {
        return match rest.split_once('/') {
            Some((n, inner)) => format!("image/layer{n}/{inner}"),
            None => format!("image/layer{rest}"),
        };
    }
    if p.starts_with("/scan/image/") {
        return "image".into();
    }
    p.trim_start_matches('/').to_string()
}

fn has_fix(vuln: &Value, package: &str) -> Option<String> {
    for affected in vuln["affected"].as_array().into_iter().flatten() {
        let name = affected["package"]["name"].as_str().unwrap_or("");
        if !name.eq_ignore_ascii_case(package) {
            continue;
        }
        for range in affected["ranges"].as_array().into_iter().flatten() {
            for ev in range["events"].as_array().into_iter().flatten() {
                if let Some(f) = ev["fixed"].as_str() {
                    return Some(f.to_string());
                }
            }
        }
    }
    None
}

/// OSV-Scanner v2 `--format=json`: one finding per vulnerability group per package.
pub fn parse_osv(text: &str) -> Result<Vec<Finding>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("osv-scanner report: {e}"))?;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for result in v["results"].as_array().into_iter().flatten() {
        let source = display_path(result["source"]["path"].as_str().unwrap_or(""));
        for pkg in result["packages"].as_array().into_iter().flatten() {
            let name = pkg["package"]["name"].as_str().unwrap_or("?");
            let version = pkg["package"]["version"].as_str().unwrap_or("?");
            let vulns: Vec<&Value> = pkg["vulnerabilities"]
                .as_array()
                .into_iter()
                .flatten()
                .collect();
            let mut groups: Vec<(Vec<String>, String)> = pkg["groups"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|g| {
                    let ids = g["ids"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|i| i.as_str().map(str::to_string))
                        .collect();
                    (ids, g["max_severity"].as_str().unwrap_or("").to_string())
                })
                .collect();
            if groups.is_empty() {
                groups = vulns
                    .iter()
                    .filter_map(|x| x["id"].as_str())
                    .map(|i| (vec![i.to_string()], String::new()))
                    .collect();
            }
            for (ids, max) in groups {
                let Some(first) = ids.first() else { continue };
                let members: Vec<&&Value> = vulns
                    .iter()
                    .filter(|x| x["id"].as_str().is_some_and(|i| ids.iter().any(|g| g == i)))
                    .collect();
                let fixed = members.iter().find_map(|x| has_fix(x, name));
                let severity = Severity::parse(&max)
                    .or_else(|| {
                        members.iter().find_map(|x| {
                            x["database_specific"]["severity"]
                                .as_str()
                                .and_then(Severity::parse)
                        })
                    })
                    .unwrap_or_else(|| unknown_severity(fixed.is_some()));
                let summary = members
                    .iter()
                    .find_map(|x| x["summary"].as_str())
                    .unwrap_or("a known vulnerability");
                let key = format!("{first}:{name}@{version}");
                if !seen.insert(key.clone()) {
                    continue;
                }
                let message = match &fixed {
                    Some(f) => format!("{first}: {summary} (fixed in {f})"),
                    None => format!("{first}: {summary} (no fixed version)"),
                };
                out.push(
                    Finding::of(
                        "osv-scanner",
                        "vulnerability.dependency",
                        severity,
                        &key,
                        message,
                        vulnerability_blocks(severity, fixed.is_some()),
                    )
                    .at((!source.is_empty()).then_some(source.as_str()))
                    .package(Some(format!("{name}@{version}"))),
                );
            }
        }
    }
    Ok(out)
}

/// Grype `-o json`: one finding per match.
pub fn parse_grype(text: &str) -> Result<Vec<Finding>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("grype report: {e}"))?;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for m in v["matches"].as_array().into_iter().flatten() {
        let vuln = &m["vulnerability"];
        let art = &m["artifact"];
        let id = vuln["id"].as_str().unwrap_or("?");
        let name = art["name"].as_str().unwrap_or("?");
        let version = art["version"].as_str().unwrap_or("?");
        let key = format!("{id}:{name}@{version}");
        if !seen.insert(key.clone()) {
            continue;
        }
        let fixed = vuln["fix"]["state"] == "fixed";
        let severity = vuln["severity"]
            .as_str()
            .and_then(Severity::parse)
            .unwrap_or_else(|| unknown_severity(fixed));
        let versions: Vec<&str> = vuln["fix"]["versions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let kind = art["type"].as_str().unwrap_or("package");
        let message = if fixed {
            format!(
                "{id} in {name} {version} ({kind}), fixed in {}",
                versions.join(", ")
            )
        } else {
            format!("{id} in {name} {version} ({kind}), no fixed version")
        };
        let path = art["locations"][0]["path"].as_str();
        out.push(
            Finding::of(
                "grype",
                "vulnerability.image",
                severity,
                &key,
                message,
                vulnerability_blocks(severity, fixed),
            )
            .at(path)
            .package(Some(format!("{name}@{version}"))),
        );
    }
    Ok(out)
}

/// `clamscan --infected --log=…`: `path: Signature FOUND` lines. Every hit blocks.
pub fn parse_clamav(text: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim().strip_suffix(" FOUND") else {
            continue;
        };
        let Some((path, sig)) = rest.rsplit_once(": ") else {
            continue;
        };
        let shown = display_path(path);
        // With --alert-exceeds-max/--alert-encrypted/--alert-broken, content ClamAV could not
        // read in full is reported as a heuristic hit: unscannable is not clean.
        let (rule, message) = if sig.starts_with("Heuristics.Limits.Exceeded") {
            (
                "malware.limits_exceeded",
                format!("too large or too deeply nested to scan in full ({sig})"),
            )
        } else if sig.starts_with("Heuristics.Encrypted") {
            (
                "malware.encrypted",
                format!("encrypted content that cannot be scanned ({sig})"),
            )
        } else if sig.starts_with("Heuristics.Broken") {
            (
                "malware.broken",
                format!("a malformed file that cannot be scanned ({sig})"),
            )
        } else {
            (
                "malware.signature",
                format!("matched the malware signature {sig}"),
            )
        };
        out.push(
            Finding::of(
                "clamav",
                rule,
                Severity::Critical,
                &format!("{sig}:{shown}"),
                message,
                true,
            )
            .at(Some(&shown)),
        );
    }
    out
}

/// `yara -r -w …`: `RuleName path` lines. Every hit blocks.
pub fn parse_yara(text: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for line in text.lines() {
        let Some((rule, path)) = line.trim().split_once(' ') else {
            continue;
        };
        if rule.is_empty()
            || !rule
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b':')
            || !path.starts_with('/')
        {
            continue;
        }
        let shown = display_path(path);
        if !seen.insert((rule.to_string(), shown.clone())) {
            continue;
        }
        out.push(
            Finding::of(
                "yara",
                "malware.yara",
                Severity::Critical,
                &format!("{rule}:{shown}"),
                format!("matched the YARA rule {rule}"),
                true,
            )
            .at(Some(&shown)),
        );
    }
    out
}

/// Semgrep `--json`. A rule's id is shown without the config path Semgrep prefixes it with.
pub fn parse_semgrep(text: &str) -> Result<Vec<Finding>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("semgrep report: {e}"))?;
    let mut out = Vec::new();
    for r in v["results"].as_array().into_iter().flatten() {
        let check = r["check_id"].as_str().unwrap_or("?");
        let rule = check
            .find("pigeonpost.")
            .map(|i| &check[i..])
            .unwrap_or(check);
        let path = display_path(r["path"].as_str().unwrap_or(""));
        let line = r["start"]["line"].as_u64().unwrap_or(0);
        let severity = match r["extra"]["severity"].as_str().unwrap_or("") {
            "ERROR" => Severity::High,
            "WARNING" => Severity::Medium,
            _ => Severity::Low,
        };
        let confidence = r["extra"]["metadata"]["confidence"].as_str().unwrap_or("");
        let message = r["extra"]["message"].as_str().unwrap_or("").to_string();
        out.push(
            Finding::of(
                "semgrep",
                &format!("sast.{rule}"),
                severity,
                &format!("{rule}:{path}:{line}"),
                format!("line {line}: {message}"),
                sast_blocks(severity, confidence),
            )
            .at(Some(&path)),
        );
    }
    Ok(out)
}

/// What Semgrep did not analyse. Every error and every skipped target blocks: a file too big,
/// too slow or unparseable must not hide code from the rules (fail closed).
pub fn semgrep_gaps(text: &str) -> Result<Vec<Finding>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("semgrep report: {e}"))?;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for e in v["errors"].as_array().into_iter().flatten() {
        let path = e["path"]
            .as_str()
            .map(display_path)
            .unwrap_or_else(|| "(no file)".into());
        let kind = match &e["type"] {
            Value::String(t) => t.clone(),
            Value::Array(a) => a.first().and_then(Value::as_str).unwrap_or("error").into(),
            _ => "error".into(),
        };
        if !seen.insert(format!("{path}:{kind}")) {
            continue;
        }
        out.push(
            Finding::of(
                "semgrep",
                "sast.not_analysed",
                Severity::High,
                &format!("error:{path}:{kind}"),
                format!("Semgrep could not analyse this file ({kind}); it blocks until it can"),
                true,
            )
            .at(Some(&path)),
        );
    }
    for t in v["paths"]["skipped"].as_array().into_iter().flatten() {
        let path = display_path(t["path"].as_str().unwrap_or(""));
        let reason = t["reason"].as_str().unwrap_or("skipped");
        if !seen.insert(format!("{path}:{reason}")) {
            continue;
        }
        out.push(
            Finding::of(
                "semgrep",
                "sast.not_analysed",
                Severity::High,
                &format!("skipped:{path}:{reason}"),
                format!("Semgrep skipped this file ({reason}); it blocks until it is analysed"),
                true,
            )
            .at(Some(&path)),
        );
    }
    Ok(out)
}

/// Lines on a YARA run's stderr that mean a file was not scanned in full: an error or a timeout.
pub fn yara_errors(stderr: &str, root: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    for line in stderr.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let lower = line.to_ascii_lowercase();
        if !(lower.contains("error") || lower.contains("timeout") || lower.contains("timed out")) {
            continue;
        }
        let path = line
            .split_whitespace()
            .find(|w| w.starts_with("/scan/"))
            .map(|w| display_path(w.trim_end_matches(':')));
        out.push(
            Finding::of(
                "pigeonpost",
                "scanner.error",
                Severity::High,
                &format!("yara:{}", path.as_deref().unwrap_or(root)),
                "yara did not complete: a file was not scanned in full (an error or a timeout)",
                true,
            )
            .at(path.as_deref()),
        );
        if out.len() >= 20 {
            break;
        }
    }
    out
}

// ---- platform-owned scanner configuration -------------------------------------------------------

/// Files (and one directory) a publisher could use to configure a scanner away: gitleaks
/// allowlists and ignore files, OSV-Scanner overrides, Semgrep ignore files and configs. They are
/// left out of the copy gitleaks, OSV-Scanner and Semgrep scan; the platform's own configuration
/// is passed to each tool instead, and their inline opt-outs (`gitleaks:allow`, `nosemgrep`) are
/// switched off by flag.
pub const PUBLISHER_SCANNER_CONFIG: &[&str] = &[
    ".gitleaks.toml",
    "gitleaks.toml",
    ".gitleaksignore",
    "osv-scanner.toml",
    ".osv-scanner.toml",
    ".semgrepignore",
    ".semgrep.yml",
    ".semgrep.yaml",
    ".semgrep",
    ".gitignore",
];

/// gitleaks: its built-in rules and nothing else (no publisher allowlist can extend this).
pub const GITLEAKS_CONFIG: &str =
    "title = \"pigeonpost analysis plane\"\n\n[extend]\nuseDefault = true\n";
/// OSV-Scanner: no package overrides, no ignored vulnerabilities.
pub const OSV_CONFIG: &str = "# pigeonpost analysis plane: no overrides, nothing ignored\n";
/// Semgrep: an explicit ignore file that ignores nothing (without one, Semgrep's defaults apply).
pub const SEMGREPIGNORE: &str = "# pigeonpost analysis plane: nothing is ignored\n";

/// Copy `src` into `dest` without any publisher scanner configuration, and put the platform's
/// `.semgrepignore` at its root. Returns the paths left out.
pub fn scan_copy(src: &Path, dest: &Path) -> Result<Vec<String>, String> {
    let _ = std::fs::remove_dir_all(dest);
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let mut dropped = Vec::new();
    for f in crate::release::read_tree(src)? {
        if f.path
            .split('/')
            .any(|c| PUBLISHER_SCANNER_CONFIG.contains(&c))
        {
            dropped.push(f.path);
            continue;
        }
        let to = dest.join(&f.path);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::copy(src.join(&f.path), &to).map_err(|e| e.to_string())?;
    }
    std::fs::write(dest.join(".semgrepignore"), SEMGREPIGNORE).map_err(|e| e.to_string())?;
    Ok(dropped)
}

/// The platform's configuration files, mounted read-only at `/rules`.
fn platform_rules(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir.join("gitleaksignore"))?;
    std::fs::write(dir.join("gitleaks.toml"), GITLEAKS_CONFIG)?;
    std::fs::write(dir.join("osv-scanner.toml"), OSV_CONFIG)
}

/// Unpack the OCI archive's layers into `/out/layers/<n>/`, inside the scanner image's sandbox
/// (the archive is untrusted: nothing of it is unpacked on the host). Non-layer blobs (the
/// manifest, the config) are skipped; a layer that does not unpack fails the step.
pub const UNPACK_SCRIPT: &str = r#"set -u
mkdir -p /out/oci /out/layers || exit 3
tar -xf /scan/image/image.tar -C /out/oci || { echo "the image archive does not unpack" >&2; exit 4; }
n=0; bad=0
for b in /out/oci/blobs/sha256/*; do
  [ -f "$b" ] || continue
  tar -tf "$b" >/dev/null 2>&1 || continue
  n=$((n+1)); d=/out/layers/$n; mkdir -p "$d"
  tar -xf "$b" -C "$d" --no-same-owner --no-same-permissions --exclude='./dev/*' --exclude='dev/*'     || { echo "layer $n did not unpack" >&2; bad=1; }
done
chmod -R u+rwX /out/layers 2>/dev/null
rm -rf /out/oci
[ "$n" -gt 0 ] || { echo "no layer in the archive" >&2; exit 5; }
exit $bad
"#;

/// gitleaks `--report-format=json --redact`: only the rule, file and line are read, never the
/// match or the secret. Every hit blocks.
pub fn parse_gitleaks(text: &str) -> Result<Vec<Finding>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("gitleaks report: {e}"))?;
    let mut out = Vec::new();
    for r in v.as_array().into_iter().flatten() {
        let rule = r["RuleID"].as_str().unwrap_or("secret");
        let path = display_path(r["File"].as_str().unwrap_or(""));
        let line = r["StartLine"].as_u64().unwrap_or(0);
        let what = r["Description"].as_str().unwrap_or("a credential");
        out.push(
            Finding::of(
                "gitleaks",
                &format!("secret.gitleaks.{rule}"),
                Severity::High,
                &format!("{rule}:{path}:{line}"),
                format!("line {line}: {what}; a credential is committed to the source tree"),
                true,
            )
            .at(Some(&path)),
        );
    }
    Ok(out)
}

// ---- obfuscation heuristics (advisory) ----------------------------------------------------------

/// Shannon entropy in bits per byte.
pub fn entropy(bytes: &[u8]) -> f64 {
    if bytes.is_empty() {
        return 0.0;
    }
    let mut counts = [0u64; 256];
    for b in bytes {
        counts[*b as usize] += 1;
    }
    let n = bytes.len() as f64;
    counts
        .iter()
        .filter(|c| **c > 0)
        .map(|c| {
            let p = *c as f64 / n;
            -p * p.log2()
        })
        .sum()
}

/// Source text above this entropy reads as encoded or packed, not written.
pub const ENTROPY_THRESHOLD: f64 = 5.6;
/// The shortest run of base64 characters reported as an embedded blob.
pub const BASE64_BLOB_CHARS: usize = 4096;
/// The longest line a hand-written source file is expected to have.
pub const MINIFIED_LINE_CHARS: usize = 10_000;

const SOURCE_EXTS: &[&str] = &[
    ".js", ".mjs", ".cjs", ".ts", ".py", ".rs", ".sh", ".json", ".toml", ".html",
];

fn longest_base64_run(bytes: &[u8]) -> usize {
    let (mut best, mut run) = (0, 0);
    for b in bytes {
        if b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'-' | b'_') {
            run += 1;
            best = best.max(run);
        } else {
            run = 0;
        }
    }
    best
}

/// Advisory findings for one file: things a human reviewer should open. Never blocking.
pub fn heuristics(path: &str, bytes: &[u8]) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut flag = |rule: &str, severity: Severity, message: String| {
        out.push(
            Finding::of(
                "heuristics",
                rule,
                severity,
                &format!("{rule}:{path}"),
                message,
                false,
            )
            .at(Some(path)),
        );
    };
    if bytes.starts_with(b"\x7fELF")
        || bytes.starts_with(b"MZ")
        || bytes.starts_with(b"\xcf\xfa\xed\xfe")
    {
        flag(
            "obfuscation.binary",
            Severity::Medium,
            "a compiled binary is committed to the source tree".into(),
        );
        return out;
    }
    if bytes.len() > 8 << 20 || bytes.iter().take(8000).any(|b| *b == 0) {
        return out;
    }
    let lockfile = path.ends_with(".lock") || path.ends_with("-lock.json");
    let run = longest_base64_run(bytes);
    if run >= BASE64_BLOB_CHARS && !lockfile {
        flag(
            "obfuscation.base64_blob",
            Severity::Medium,
            format!("a {run}-character base64 run"),
        );
    }
    let text = String::from_utf8_lossy(bytes);
    let js = path.ends_with(".js") || path.ends_with(".mjs") || path.ends_with(".cjs");
    if js {
        let packer = text.contains("eval(function(p,a,c,k,e,");
        let hex_names = text.matches("_0x").count();
        if packer || hex_names >= 30 {
            flag(
                "obfuscation.packed_js",
                Severity::Medium,
                if packer {
                    "a packed (p,a,c,k,e,d) script".into()
                } else {
                    format!("{hex_names} obfuscator-style _0x identifiers")
                },
            );
        }
    }
    if js || path.ends_with(".py") {
        if let Some(n) = text
            .lines()
            .map(str::len)
            .max()
            .filter(|n| *n > MINIFIED_LINE_CHARS)
        {
            flag(
                "obfuscation.minified",
                Severity::Low,
                format!("a {n}-character line (minified or generated)"),
            );
        }
    }
    if path.ends_with(".py")
        && (text.contains("exec(") || text.contains("eval("))
        && [
            "b64decode",
            "zlib.decompress",
            "marshal.loads",
            "codecs.decode",
        ]
        .iter()
        .any(|n| text.contains(n))
    {
        flag(
            "obfuscation.encoded_exec",
            Severity::Medium,
            "decodes data and executes it".into(),
        );
    }
    if bytes.len() >= 4096 && !lockfile && SOURCE_EXTS.iter().any(|e| path.ends_with(e)) {
        let e = entropy(bytes);
        if e > ENTROPY_THRESHOLD {
            flag(
                "obfuscation.high_entropy",
                Severity::Low,
                format!("entropy {e:.2} bits per byte"),
            );
        }
    }
    out
}

// ---- running the tools --------------------------------------------------------------------------

/// The updater's `manifest.json`: when each database was last refreshed, and what it holds.
pub fn db_findings(manifest: Option<&Value>, max_age_hours: u64, now: i64) -> Vec<Finding> {
    let mut out = Vec::new();
    for db in DATABASES {
        let updated = manifest.and_then(|m| m[db]["updated_at"].as_i64());
        match updated {
            None => out.push(Finding::of(
                "pigeonpost",
                "scanner.db_missing",
                Severity::High,
                db,
                format!("the {db} database is not on this build plane"),
                true,
            )),
            Some(t) if now - t > (max_age_hours as i64) * 3600 => out.push(Finding::of(
                "pigeonpost",
                "scanner.db_stale",
                Severity::High,
                db,
                format!(
                    "the {db} database is {} hours old (limit {max_age_hours})",
                    (now - t) / 3600
                ),
                true,
            )),
            Some(_) => {}
        }
    }
    out
}

fn tool_failed(tool: &str, why: &str) -> Finding {
    Finding::of(
        "pigeonpost",
        "scanner.error",
        Severity::High,
        tool,
        format!("{tool} did not complete: {why}"),
        true,
    )
}

/// One tool's container: gVisor, no network, nothing writable but `/out` and a small `/tmp`.
pub fn tool_args(
    cfg: &ScannersConfig,
    runtime: &str,
    image: &str,
    entrypoint: &str,
    mounts: &[(PathBuf, &str, bool)],
    env: &[(&str, &str)],
    cmd: &[String],
) -> Vec<String> {
    tool_args_in(cfg, runtime, image, entrypoint, mounts, env, None, cmd)
}

/// [`tool_args`] with a working directory.
#[allow(clippy::too_many_arguments)]
pub fn tool_args_in(
    cfg: &ScannersConfig,
    runtime: &str,
    image: &str,
    entrypoint: &str,
    mounts: &[(PathBuf, &str, bool)],
    env: &[(&str, &str)],
    workdir: Option<&str>,
    cmd: &[String],
) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--pull=never".into(),
        format!("--runtime={runtime}"),
        "--network=none".into(),
        "--cap-drop=ALL".into(),
        "--security-opt=no-new-privileges".into(),
        "--read-only".into(),
        "--tmpfs=/tmp:rw,nosuid,nodev,size=1024m".into(),
        format!("--memory={}m", cfg.memory_mib),
        format!("--memory-swap={}m", cfg.memory_mib),
        "--pids-limit=256".into(),
        format!("--timeout={}", cfg.timeout_secs),
        format!("--entrypoint={entrypoint}"),
        "--env=HOME=/tmp".into(),
    ];
    if let Some(w) = workdir {
        a.push(format!("--workdir={w}"));
    }
    for (k, v) in env {
        a.push(format!("--env={k}={v}"));
    }
    for (host, inside, writable) in mounts {
        a.push(format!(
            "--volume={}:{inside}:{}",
            host.display(),
            if *writable { "rw" } else { "ro" }
        ));
    }
    a.push(image.into());
    a.extend(cmd.iter().cloned());
    a
}

fn run(podman: &dyn Podman, args: &[String]) -> Result<String, String> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    podman.run(&refs)
}

/// What one scan found, and how it ran.
pub struct ScanOutcome {
    pub findings: Vec<Finding>,
    pub evidence: Value,
}

/// Run every tool `target` calls for. `out` is a scratch directory of this scan's own (wiped).
pub fn security_scan(
    podman: &dyn Podman,
    cfg: &ScannersConfig,
    runtime: &str,
    target: &Target<'_>,
    out: &Path,
    now: i64,
) -> ScanOutcome {
    let _ = std::fs::remove_dir_all(out);
    let mut findings = Vec::new();
    let mut tools = serde_json::Map::new();
    let manifest: Option<Value> = std::fs::read_to_string(cfg.db_root.join("manifest.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());
    findings.extend(db_findings(manifest.as_ref(), cfg.max_db_age_hours, now));

    // The platform's own configuration, and a copy of the source without the publisher's.
    let rules_dir = out.join("rules");
    if let Err(e) = platform_rules(&rules_dir) {
        findings.push(tool_failed("configuration", &e.to_string()));
    }
    let clean_src = out.join("src");
    let mut stripped = Vec::new();
    if let Some(src) = target.src {
        match scan_copy(src, &clean_src) {
            Ok(d) => stripped = d,
            Err(e) => findings.push(tool_failed("source copy", &redact_error(&e))),
        }
    }
    for p in &stripped {
        findings.push(
            Finding::of(
                "pigeonpost",
                "evasion.scanner_config",
                Severity::Medium,
                &format!("scanner_config:{p}"),
                "a scanner configuration or ignore file in the tree; the scan ignored it and used \
                 the platform's",
                false,
            )
            .at(Some(p)),
        );
    }

    // The image's layers, unpacked inside the sandbox, are what ClamAV and YARA read.
    let unpacked = out.join("unpack");
    let mut layers: Option<PathBuf> = None;
    if let Some(archive) = target.image_archive {
        if std::fs::create_dir_all(&unpacked).is_ok() {
            let args = tool_args(
                cfg,
                runtime,
                &cfg.image,
                "/bin/sh",
                &[
                    (archive.to_path_buf(), "/scan/image/image.tar", false),
                    (unpacked.clone(), "/out", true),
                ],
                &[],
                &["-c".into(), UNPACK_SCRIPT.into()],
            );
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let (ok, _, err) = podman.run_capture(&refs);
            if ok {
                layers = Some(unpacked.join("layers"));
                tools.insert("unpack".into(), json!({ "ran": true }));
            } else {
                tools.insert("unpack".into(), json!({ "ran": false }));
                findings.push(tool_failed("image unpack", &redact_error(&err)));
            }
        }
    }

    let mut scan_mounts: Vec<(PathBuf, &str, bool)> = Vec::new();
    // Where the configurable tools (gitleaks, OSV-Scanner, Semgrep) read the source.
    let mut config_free_mounts: Vec<(PathBuf, &str, bool)> = Vec::new();
    if let Some(p) = target.src {
        scan_mounts.push((p.to_path_buf(), "/scan/src", false));
        config_free_mounts.push((clean_src.clone(), "/scan/src", false));
    }
    if let Some(p) = target.artefacts {
        scan_mounts.push((p.to_path_buf(), "/scan/artefacts", false));
    }
    match (&layers, target.image_archive) {
        (Some(l), _) => scan_mounts.push((l.clone(), "/scan/layers", false)),
        (None, Some(p)) => scan_mounts.push((p.to_path_buf(), "/scan/image/image.tar", false)),
        _ => {}
    }
    if let Some(p) = target.files {
        scan_mounts.push((p.to_path_buf(), "/scan/files", false));
    }
    let sbom_dir = out.join("sbom");
    if let Some(sbom) = target.sbom {
        let ok = std::fs::create_dir_all(&sbom_dir)
            .and_then(|_| std::fs::write(sbom_dir.join("sbom.cdx.json"), sbom))
            .is_ok();
        if ok {
            scan_mounts.push((sbom_dir.clone(), "/scan/sbom", false));
            config_free_mounts.push((sbom_dir.clone(), "/scan/sbom", false));
        }
    }
    config_free_mounts.push((rules_dir.clone(), "/rules", false));
    let roots = scan_roots(target, layers.is_some());
    let db = |name: &str| cfg.db_root.join(name);

    for tool in wanted(target) {
        let dir = out.join(tool);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            findings.push(tool_failed(tool, &e.to_string()));
            continue;
        }
        let configurable = matches!(tool, "osv-scanner" | "semgrep" | "gitleaks");
        let mut mounts = if configurable {
            config_free_mounts.clone()
        } else {
            scan_mounts.clone()
        };
        mounts.push((dir.clone(), "/out", true));
        let result: Result<Vec<Finding>, String> = match tool {
            "osv-scanner" => {
                mounts.push((db("osv"), "/db/osv", false));
                let mut cmd: Vec<String> = vec![
                    "scan".into(),
                    "source".into(),
                    "--offline-vulnerabilities".into(),
                    // The platform's (empty) configuration: no osv-scanner.toml of the tree applies.
                    "--config=/rules/osv-scanner.toml".into(),
                    "--format=json".into(),
                    "--output=/out/report.json".into(),
                ];
                if let Some(l) = target.lockfile.filter(|_| target.src.is_some()) {
                    cmd.push(format!("--lockfile=/scan/src/{l}"));
                }
                if target.sbom.is_some() {
                    cmd.push("--lockfile=/scan/sbom/sbom.cdx.json".into());
                }
                let args = tool_args(
                    cfg,
                    runtime,
                    &cfg.image,
                    "osv-scanner",
                    &mounts,
                    &[("OSV_SCANNER_LOCAL_DB_CACHE_DIRECTORY", "/db/osv")],
                    &cmd,
                );
                // osv-scanner exits 1 when it found something: the report decides.
                let ran = run(podman, &args);
                read_report(&dir.join("report.json"), ran).and_then(|t| parse_osv(&t))
            }
            "grype" => {
                mounts.push((db("grype"), "/db/grype", false));
                let source = if target.image_archive.is_some() {
                    "oci-archive:/scan/image/image.tar"
                } else {
                    "sbom:/scan/sbom/sbom.cdx.json"
                };
                if let Some(p) = target.image_archive {
                    if layers.is_some() {
                        mounts.push((p.to_path_buf(), "/scan/image/image.tar", false));
                    }
                }
                let args = tool_args(
                    cfg,
                    runtime,
                    &cfg.image,
                    "grype",
                    &mounts,
                    &[
                        ("GRYPE_DB_CACHE_DIR", "/db/grype"),
                        ("GRYPE_DB_AUTO_UPDATE", "false"),
                        ("GRYPE_DB_VALIDATE_AGE", "false"),
                        ("GRYPE_CHECK_FOR_APP_UPDATE", "false"),
                    ],
                    &[
                        source.into(),
                        "--output=json".into(),
                        "--file=/out/report.json".into(),
                        "--quiet".into(),
                    ],
                );
                // The image when it is here (Grype catalogues it itself), else the SBOM.
                let ran = run(podman, &args);
                read_report(&dir.join("report.json"), ran).and_then(|t| parse_grype(&t))
            }
            "clamav" => {
                mounts.push((db("clamav"), "/db/clamav", false));
                let mut cmd: Vec<String> = vec![
                    "--database=/db/clamav".into(),
                    "--recursive".into(),
                    "--infected".into(),
                    "--no-summary".into(),
                    "--log=/out/report.log".into(),
                    "--max-filesize=2000M".into(),
                    "--max-scansize=4000M".into(),
                    "--max-recursion=16".into(),
                    "--max-files=200000".into(),
                    // What ClamAV cannot read in full is a hit, never silently clean.
                    "--alert-exceeds-max=yes".into(),
                    "--alert-encrypted=yes".into(),
                    "--alert-broken=yes".into(),
                ];
                cmd.extend(roots.iter().cloned());
                let args = tool_args(
                    cfg,
                    runtime,
                    &cfg.clamav_image,
                    "clamscan",
                    &mounts,
                    &[],
                    &cmd,
                );
                // clamscan exits 1 when it found something, 2 on an error: an error leaves no
                // log, or a log without our lines; a hit is in the log.
                let ran = run(podman, &args);
                let log = std::fs::read_to_string(dir.join("report.log")).unwrap_or_default();
                let hits = parse_clamav(&log);
                match ran {
                    Ok(_) => Ok(hits),
                    Err(_) if !hits.is_empty() => Ok(hits),
                    Err(e) => Err(redact_error(&e)),
                }
            }
            "yara" => {
                mounts.push((db("yara"), "/db/yara", false));
                // One run per root (yara takes one target); stderr is read too, because yara
                // exits 0 when a file timed out or could not be read.
                let mut found = Vec::new();
                let mut failed = None;
                for root in &roots {
                    let cmd: Vec<String> = vec![
                        "--compiled-rules".into(),
                        "--recursive".into(),
                        "--no-warnings".into(),
                        "--timeout=120".into(),
                        "/db/yara/rules.yarc".into(),
                        root.clone(),
                    ];
                    let args = tool_args(cfg, runtime, &cfg.image, "yara", &mounts, &[], &cmd);
                    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
                    let (ok, stdout, stderr) = podman.run_capture(&refs);
                    found.extend(parse_yara(&stdout));
                    found.extend(yara_errors(&stderr, root));
                    if !ok {
                        failed = Some(redact_error(&stderr));
                    }
                }
                match failed {
                    Some(e) if found.iter().all(|f| f.tool != "yara") => Err(e),
                    Some(e) => {
                        found.push(tool_failed("yara", &e));
                        Ok(found)
                    }
                    None => Ok(found),
                }
            }
            "semgrep" => {
                match crate::util::sha256_hex(
                    &std::fs::read(&cfg.semgrep_rules).unwrap_or_default(),
                ) {
                    d if d.eq_ignore_ascii_case(&cfg.semgrep_rules_sha256) => {
                        mounts.push((
                            cfg.semgrep_rules.clone(),
                            "/rules-semgrep/semgrep.yml",
                            false,
                        ));
                        let args = tool_args_in(
                            cfg,
                            runtime,
                            &cfg.semgrep_image,
                            "semgrep",
                            &mounts,
                            &[
                                ("SEMGREP_SEND_METRICS", "off"),
                                ("SEMGREP_ENABLE_VERSION_CHECK", "0"),
                            ],
                            // The platform's .semgrepignore at the copy's root is the one read.
                            Some("/scan/src"),
                            &[
                                "scan".into(),
                                "--config=/rules-semgrep/semgrep.yml".into(),
                                "--json".into(),
                                "--output=/out/report.json".into(),
                                "--metrics=off".into(),
                                "--disable-version-check".into(),
                                "--oss-only".into(),
                                "--no-git-ignore".into(),
                                // No inline opt-outs, no size or time cut-offs: every file is
                                // analysed, or reported as not analysed (which blocks).
                                "--disable-nosem".into(),
                                "--max-target-bytes=0".into(),
                                "--timeout=0".into(),
                                "--timeout-threshold=0".into(),
                                "--verbose".into(),
                                "/scan/src".into(),
                            ],
                        );
                        let ran = run(podman, &args);
                        read_report(&dir.join("report.json"), ran).and_then(|t| {
                            let mut f = parse_semgrep(&t)?;
                            f.extend(semgrep_gaps(&t)?);
                            Ok(f)
                        })
                    }
                    _ => Err("the Semgrep ruleset is not the pinned one".into()),
                }
            }
            "gitleaks" => {
                let args = tool_args(
                    cfg,
                    runtime,
                    &cfg.image,
                    "gitleaks",
                    &mounts,
                    &[],
                    &[
                        "dir".into(),
                        "/scan/src".into(),
                        // The platform's rules, an empty ignore folder, and no gitleaks:allow.
                        "--config=/rules/gitleaks.toml".into(),
                        "--gitleaks-ignore-path=/rules/gitleaksignore".into(),
                        "--ignore-gitleaks-allow".into(),
                        "--report-format=json".into(),
                        "--report-path=/out/report.json".into(),
                        "--redact".into(),
                        "--exit-code=0".into(),
                        "--no-banner".into(),
                        "--log-level=error".into(),
                    ],
                );
                let ran = run(podman, &args);
                read_report(&dir.join("report.json"), ran).and_then(|t| parse_gitleaks(&t))
            }
            _ => Ok(Vec::new()),
        };
        match result {
            Ok(found) => {
                tools.insert(
                    tool.into(),
                    json!({
                        "ran": true,
                        "findings": found.len(),
                        "blocking": found.iter().filter(|f| f.blocking).count(),
                    }),
                );
                findings.extend(found);
            }
            Err(e) => {
                tools.insert(tool.into(), json!({ "ran": false }));
                findings.push(tool_failed(tool, &e));
            }
        }
    }
    // The heuristics read the source in-process (bytes only, nothing runs).
    if let Some(src) = target.src {
        let mut n = 0;
        if let Ok(files) = crate::release::read_tree(src) {
            for f in files {
                if let Ok(bytes) = std::fs::read(src.join(&f.path)) {
                    let found = heuristics(&f.path, &bytes);
                    n += found.len();
                    findings.extend(found);
                }
            }
        }
        tools.insert(
            "heuristics".into(),
            json!({ "ran": true, "findings": n, "blocking": 0 }),
        );
    }
    let databases: Value = match &manifest {
        Some(m) => DATABASES
            .iter()
            .map(|d| (d.to_string(), m[*d].clone()))
            .collect::<serde_json::Map<_, _>>()
            .into(),
        None => Value::Null,
    };
    // A tool or database failure is this run's, never "the same finding as last time": a re-scan
    // that fails must not be taken for one already answered.
    let run_id = crate::util::random_hex(6);
    for f in findings
        .iter_mut()
        .filter(|f| f.rule.starts_with("scanner."))
    {
        f.id = format!("{}:{run_id}", f.id);
    }
    let _ = std::fs::remove_dir_all(out);
    ScanOutcome {
        findings,
        evidence: json!({
            "network": "none",
            "publisher_scanner_config_ignored": stripped,
            "image_layers_unpacked": layers.is_some(),
            "runtime": runtime,
            "tools": tools,
            "images": {
                "scanners": cfg.image,
                "clamav": cfg.clamav_image,
                "semgrep": cfg.semgrep_image,
            },
            "semgrep_rules_sha256": cfg.semgrep_rules_sha256.to_ascii_lowercase(),
            "databases": databases,
            "policy": {
                "vulnerabilities": "critical or high with a fix available blocks",
                "malware": "any ClamAV or YARA hit blocks",
                "sast": "ERROR with HIGH confidence blocks",
                "secrets": "any gitleaks hit blocks",
                "heuristics": "advisory",
            },
        }),
    }
}

fn scan_roots(t: &Target<'_>, layers: bool) -> Vec<String> {
    let mut out = Vec::new();
    if t.src.is_some() {
        out.push("/scan/src".into());
    }
    if t.artefacts.is_some() {
        out.push("/scan/artefacts".into());
    }
    if layers {
        out.push("/scan/layers".into());
    } else if t.image_archive.is_some() {
        out.push("/scan/image".into());
    }
    if t.files.is_some() {
        out.push("/scan/files".into());
    }
    out
}

/// A tool's report file: the tool may exit non-zero for "found something", so the report, not
/// the exit status, says whether it ran.
fn read_report(path: &Path, ran: Result<String, String>) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(t) if !t.trim().is_empty() => Ok(t),
        _ => Err(match ran {
            Ok(_) => "it wrote no report".into(),
            Err(e) => redact_error(&e),
        }),
    }
}

/// A tool's error as evidence: the first line, without host paths.
fn redact_error(e: &str) -> String {
    let first = e.lines().next().unwrap_or("").trim();
    let cleaned: String = first
        .split_whitespace()
        .map(|w| {
            if w.contains("/var/") || w.contains("/home/") || w.contains("/etc/") {
                "<path>"
            } else {
                w
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    cleaned.chars().take(300).collect()
}

/// The digest a vendored file is pinned by (`sha256sum` of it).
pub fn file_sha256(path: &Path) -> Result<String, String> {
    std::fs::read(path)
        .map(|b| sha256_hex(&b))
        .map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    const OSV: &str = include_str!("../tests/fixtures/scanners/osv.json");
    const GRYPE: &str = include_str!("../tests/fixtures/scanners/grype.json");
    const SEMGREP: &str = include_str!("../tests/fixtures/scanners/semgrep.json");
    const GITLEAKS: &str = include_str!("../tests/fixtures/scanners/gitleaks.json");
    const CLAMAV: &str = include_str!("../tests/fixtures/scanners/clamav.txt");
    const YARA: &str = include_str!("../tests/fixtures/scanners/yara.txt");

    fn by_id<'a>(f: &'a [Finding], id: &str) -> &'a Finding {
        f.iter()
            .find(|x| x.id == id)
            .unwrap_or_else(|| panic!("{id} in {f:#?}"))
    }

    #[test]
    fn severities_parse_from_scores_and_words() {
        assert_eq!(Severity::parse("9.8"), Some(Severity::Critical));
        assert_eq!(Severity::parse("7.5"), Some(Severity::High));
        assert_eq!(Severity::parse("5.3"), Some(Severity::Medium));
        assert_eq!(Severity::parse("2.0"), Some(Severity::Low));
        assert_eq!(Severity::parse("MODERATE"), Some(Severity::Medium));
        assert_eq!(Severity::parse("Negligible"), Some(Severity::Info));
        assert_eq!(Severity::parse(""), None);
        assert_eq!(Severity::parse("Unknown"), None);
    }

    #[test]
    fn the_vulnerability_policy_blocks_only_fixable_critical_and_high() {
        assert!(vulnerability_blocks(Severity::Critical, true));
        assert!(vulnerability_blocks(Severity::High, true));
        assert!(!vulnerability_blocks(Severity::High, false));
        assert!(!vulnerability_blocks(Severity::Critical, false));
        assert!(!vulnerability_blocks(Severity::Medium, true));
        assert!(sast_blocks(Severity::High, "HIGH"));
        assert!(!sast_blocks(Severity::High, "MEDIUM"));
        assert!(!sast_blocks(Severity::Medium, "HIGH"));
    }

    #[test]
    fn osv_reports_parse_into_grouped_findings_with_the_fix_policy() {
        let f = parse_osv(OSV).unwrap();
        assert_eq!(f.len(), 4, "{f:#?}");
        // Critical, fixed: blocks. The group is one finding, named by its first id.
        let a = by_id(&f, "osv-scanner:RUSTSEC-2024-0001:example-tls@0.4.1");
        assert_eq!(a.severity, Severity::Critical);
        assert!(a.blocking);
        assert_eq!(a.package.as_deref(), Some("example-tls@0.4.1"));
        assert_eq!(a.path.as_deref(), Some("Cargo.lock"));
        assert!(a.message.contains("fixed in 0.4.2"), "{}", a.message);
        // High, no fix: advisory.
        let b = by_id(&f, "osv-scanner:GHSA-aaaa-bbbb-cccc:left-pad@1.0.0");
        assert_eq!(b.severity, Severity::High);
        assert!(!b.blocking);
        // Moderate from the database's word, with a fix: advisory.
        let c = by_id(&f, "osv-scanner:PYSEC-2025-12:requests@2.30.0");
        assert_eq!(c.severity, Severity::Medium);
        assert!(!c.blocking);
        assert!(f.iter().all(|x| x.tool == "osv-scanner"));
    }

    #[test]
    fn grype_reports_parse_with_the_same_policy() {
        let f = parse_grype(GRYPE).unwrap();
        assert_eq!(f.len(), 5, "{f:#?}");
        let crit = by_id(&f, "grype:CVE-2024-45337:golang.org/x/crypto@0.27.0");
        assert!(crit.blocking && crit.severity == Severity::Critical);
        let high = by_id(&f, "grype:CVE-2025-0001:openssl@3.0.15-1~deb12u1");
        assert!(!high.blocking, "high with no fix is advisory");
        let medium = by_id(&f, "grype:GHSA-9wx4-h78v-vm56:requests@2.31.0");
        assert!(!medium.blocking);
        let neg = by_id(&f, "grype:CVE-2023-99999:zlib1g@1:1.2.13.dfsg-1");
        assert_eq!(neg.severity, Severity::Info);
        assert!(!neg.blocking);
    }

    #[test]
    fn every_clamav_and_yara_hit_blocks_and_paths_are_shown_relative() {
        let c = parse_clamav(CLAMAV);
        assert_eq!(c.len(), 5, "{c:#?}");
        assert!(c
            .iter()
            .all(|f| f.blocking && f.severity == Severity::Critical));
        assert_eq!(c[0].path.as_deref(), Some("assets/eicar.com"));
        assert_eq!(c[1].path.as_deref(), Some("image"));
        let y = parse_yara(YARA);
        assert_eq!(y.len(), 2, "{y:#?}");
        assert!(y.iter().all(|f| f.blocking && f.tool == "yara"));
        assert_eq!(y[0].rule, "malware.yara");
        assert_eq!(y[0].path.as_deref(), Some("src/miner.js"));
        assert_eq!(y[1].path.as_deref(), Some("artefacts/bin/helper"));
    }

    #[test]
    fn semgrep_blocks_only_high_confidence_errors() {
        let f = parse_semgrep(SEMGREP).unwrap();
        assert_eq!(f.len(), 3);
        let eval = f
            .iter()
            .find(|x| x.rule == "sast.pigeonpost.js.eval-dynamic")
            .unwrap();
        assert!(eval.blocking);
        assert_eq!(eval.path.as_deref(), Some("src/index.mjs"));
        let pickle = f
            .iter()
            .find(|x| x.rule == "sast.pigeonpost.py.pickle-load")
            .unwrap();
        assert!(!pickle.blocking, "ERROR with MEDIUM confidence is advisory");
        let ssrf = f
            .iter()
            .find(|x| x.rule == "sast.pigeonpost.js.ssrf-fetch")
            .unwrap();
        assert!(!ssrf.blocking, "WARNING is advisory");
    }

    #[test]
    fn gitleaks_hits_block_and_the_secret_is_never_kept() {
        let f = parse_gitleaks(GITLEAKS).unwrap();
        assert_eq!(f.len(), 2);
        assert!(f.iter().all(|x| x.blocking && x.tool == "gitleaks"));
        assert_eq!(f[0].path.as_deref(), Some("src/config.js"));
        let all = serde_json::to_string(&f).unwrap();
        assert!(!all.contains("REDACTED") && !all.contains("AKIA"), "{all}");
        assert!(parse_gitleaks("[]").unwrap().is_empty());
        assert!(parse_gitleaks("not json").is_err());
    }

    #[test]
    fn obfuscation_heuristics_flag_but_never_block() {
        let packed = "eval(function(p,a,c,k,e,d){return p}('0',1,1,''.split('|'),0,{}))";
        let blob = format!("const x = \"{}\";", "QUJD".repeat(1500));
        let minified = format!("var a=1;{}", "a+=1;".repeat(3000));
        let hexy = (0..40)
            .map(|i| format!("var _0x{i:04x}=1;"))
            .collect::<String>();
        let encoded = "import base64\nexec(base64.b64decode('cHJpbnQoMSk='))\n";
        let cases = [
            (
                "src/a.js",
                packed.as_bytes().to_vec(),
                "obfuscation.packed_js",
            ),
            (
                "src/b.js",
                blob.as_bytes().to_vec(),
                "obfuscation.base64_blob",
            ),
            (
                "src/c.js",
                minified.as_bytes().to_vec(),
                "obfuscation.minified",
            ),
            (
                "src/d.js",
                hexy.as_bytes().to_vec(),
                "obfuscation.packed_js",
            ),
            (
                "src/e.py",
                encoded.as_bytes().to_vec(),
                "obfuscation.encoded_exec",
            ),
            (
                "bin/tool",
                b"\x7fELF\x02\x01\x01rest".to_vec(),
                "obfuscation.binary",
            ),
        ];
        for (path, bytes, rule) in cases {
            let f = heuristics(path, &bytes);
            assert!(f.iter().any(|x| x.rule == rule), "{path}: {f:#?}");
            assert!(f.iter().all(|x| !x.blocking), "{path}");
        }
        // Random bytes in a source file read as encoded.
        let mut x = 0x2545_f491_u64;
        let noisy: Vec<u8> = (0..8192)
            .map(|_| {
                x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                0x21 + ((x >> 33) % 94) as u8
            })
            .collect();
        assert!(entropy(&noisy) > ENTROPY_THRESHOLD, "{}", entropy(&noisy));
        assert!(heuristics("src/x.json", &noisy)
            .iter()
            .any(|x| x.rule == "obfuscation.high_entropy"));
        // Ordinary code is quiet, and lockfile integrity hashes are not blobs.
        let code = "fn main() {\n    println!(\"hello\");\n}\n".repeat(200);
        assert!(heuristics("src/main.rs", code.as_bytes()).is_empty());
        assert!(heuristics("package-lock.json", blob.as_bytes()).is_empty());
    }

    #[test]
    fn missing_or_stale_databases_block() {
        let now = 1_800_000_000;
        let fresh = json!({
            "osv": {"updated_at": now - 3600}, "grype": {"updated_at": now - 3600},
            "clamav": {"updated_at": now - 3600}, "yara": {"updated_at": now - 3600},
        });
        assert!(db_findings(Some(&fresh), 72, now).is_empty());
        let mut stale = fresh.clone();
        stale["grype"]["updated_at"] = json!(now - 100 * 3600);
        stale.as_object_mut().unwrap().remove("yara");
        let f = db_findings(Some(&stale), 72, now);
        let rules: Vec<&str> = f.iter().map(|x| x.rule.as_str()).collect();
        assert_eq!(rules, vec!["scanner.db_stale", "scanner.db_missing"]);
        assert!(f.iter().all(|x| x.blocking));
        assert_eq!(db_findings(None, 72, now).len(), 4);
    }

    /// A podman that records each tool's argv and writes the fixture report into its `/out`.
    #[derive(Default)]
    struct Tools {
        calls: Mutex<Vec<Vec<String>>>,
        fail: Option<&'static str>,
        /// What yara prints on stderr (it exits 0 all the same).
        yara_stderr: &'static str,
    }

    impl Podman for Tools {
        fn run_capture(&self, args: &[&str]) -> (bool, String, String) {
            let is_yara = args.contains(&"--entrypoint=yara");
            match self.run(args) {
                Ok(out) if is_yara => (true, out, self.yara_stderr.into()),
                Ok(out) => (true, out, String::new()),
                Err(e) => (false, String::new(), e),
            }
        }

        fn run(&self, args: &[&str]) -> Result<String, String> {
            let argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
            self.calls.lock().unwrap().push(argv.clone());
            let entry = argv
                .iter()
                .find_map(|a| a.strip_prefix("--entrypoint="))
                .unwrap()
                .to_string();
            if self.fail == Some(entry.as_str()) {
                return Err("podman run: exit status 125".into());
            }
            let out = argv
                .iter()
                .find_map(|a| {
                    a.strip_prefix("--volume=")
                        .and_then(|v| v.strip_suffix(":/out:rw"))
                })
                .map(PathBuf::from)
                .unwrap();
            match entry.as_str() {
                "osv-scanner" => {
                    std::fs::write(out.join("report.json"), OSV).unwrap();
                    Err("osv-scanner: exit status 1".into())
                }
                "grype" => {
                    std::fs::write(out.join("report.json"), GRYPE).unwrap();
                    Ok(String::new())
                }
                "clamscan" => {
                    std::fs::write(out.join("report.log"), CLAMAV).unwrap();
                    Err("clamscan: exit status 1".into())
                }
                "yara" => Ok(YARA.into()),
                "/bin/sh" => Ok(String::new()),
                "semgrep" => {
                    std::fs::write(out.join("report.json"), SEMGREP).unwrap();
                    Ok(String::new())
                }
                "gitleaks" => {
                    std::fs::write(out.join("report.json"), GITLEAKS).unwrap();
                    Ok(String::new())
                }
                other => panic!("unexpected tool {other}"),
            }
        }
    }

    fn config(dir: &Path) -> ScannersConfig {
        let rules = dir.join("semgrep.yml");
        std::fs::write(&rules, "rules: []\n").unwrap();
        let db = dir.join("db");
        std::fs::create_dir_all(&db).unwrap();
        std::fs::write(
            db.join("manifest.json"),
            json!({
                "osv": {"updated_at": 1_800_000_000 - 60}, "grype": {"updated_at": 1_800_000_000 - 60},
                "clamav": {"updated_at": 1_800_000_000 - 60, "daily": "27811"},
                "yara": {"updated_at": 1_800_000_000 - 60},
            })
            .to_string(),
        )
        .unwrap();
        ScannersConfig {
            image: format!("localhost/pigeonpost-scanners@sha256:{}", "a".repeat(64)),
            clamav_image: format!("docker.io/clamav/clamav@sha256:{}", "b".repeat(64)),
            semgrep_image: format!("docker.io/semgrep/semgrep@sha256:{}", "c".repeat(64)),
            semgrep_rules_sha256: file_sha256(&rules).unwrap(),
            semgrep_rules: rules,
            db_root: db,
            max_db_age_hours: 72,
            timeout_secs: 900,
            memory_mib: 4096,
        }
    }

    #[test]
    fn every_tool_runs_in_gvisor_with_no_network_and_read_only_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        cfg.check().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("Cargo.lock"), "# lock\n").unwrap();
        let archive = dir.path().join("image.tar");
        std::fs::write(&archive, b"tar").unwrap();
        let podman = Tools::default();
        let outcome = security_scan(
            &podman,
            &cfg,
            "runsc",
            &Target {
                src: Some(&src),
                artefacts: Some(&src),
                image_archive: Some(&archive),
                sbom: Some("{\"bomFormat\":\"CycloneDX\"}"),
                lockfile: Some("Cargo.lock"),
                files: None,
            },
            &dir.path().join("out"),
            1_800_000_000,
        );
        let calls = podman.calls.lock().unwrap().clone();
        let tools: Vec<String> = calls
            .iter()
            .map(|c| {
                c.iter()
                    .find_map(|a| a.strip_prefix("--entrypoint="))
                    .unwrap()
                    .to_string()
            })
            .collect();
        for t in [
            "osv-scanner",
            "grype",
            "clamscan",
            "yara",
            "semgrep",
            "gitleaks",
        ] {
            assert!(tools.iter().any(|x| x == t), "{t}: {tools:?}");
        }
        for c in &calls {
            for flag in [
                "--runtime=runsc",
                "--network=none",
                "--cap-drop=ALL",
                "--read-only",
                "--pull=never",
            ] {
                assert!(c.iter().any(|a| a == flag), "{flag}: {c:?}");
            }
            // Only /out is writable.
            let rw: Vec<&String> = c
                .iter()
                .filter(|a| a.starts_with("--volume=") && a.ends_with(":rw"))
                .collect();
            assert_eq!(rw.len(), 1, "{c:?}");
            assert!(rw[0].ends_with(":/out:rw"));
            let image = c.iter().find(|a| a.contains("@sha256:")).unwrap();
            assert!(crate::buildd::pinned(image), "{image}");
        }
        let blocking: Vec<&str> = outcome
            .findings
            .iter()
            .filter(|f| f.blocking)
            .map(|f| f.tool.as_str())
            .collect();
        for t in [
            "osv-scanner",
            "grype",
            "clamav",
            "yara",
            "semgrep",
            "gitleaks",
        ] {
            assert!(blocking.contains(&t), "{t}: {blocking:?}");
        }
        assert_eq!(outcome.evidence["network"], "none");
        assert_eq!(outcome.evidence["tools"]["clamav"]["ran"], true);
        assert_eq!(outcome.evidence["databases"]["clamav"]["daily"], "27811");
        assert!(!dir.path().join("out").exists(), "raw reports are not kept");
    }

    #[test]
    fn a_tool_that_cannot_run_or_a_tampered_ruleset_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path());
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let podman = Tools {
            fail: Some("gitleaks"),
            ..Tools::default()
        };
        cfg.semgrep_rules_sha256 = "0".repeat(64);
        let outcome = security_scan(
            &podman,
            &cfg,
            "runsc",
            &Target {
                src: Some(&src),
                ..Target::default()
            },
            &dir.path().join("out"),
            1_800_000_000,
        );
        let errors: Vec<&str> = outcome
            .findings
            .iter()
            .filter(|f| f.rule == "scanner.error")
            .map(|f| f.id.as_str())
            .collect();
        assert!(
            errors.iter().any(|e| e.starts_with("pigeonpost:gitleaks:")),
            "{errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.starts_with("pigeonpost:semgrep:")),
            "{errors:?}"
        );
        assert!(outcome
            .findings
            .iter()
            .filter(|f| f.rule == "scanner.error")
            .all(|f| f.blocking));
        // Semgrep never ran with the wrong rules.
        let calls = podman.calls.lock().unwrap();
        assert!(!calls
            .iter()
            .any(|c| c.iter().any(|a| a == "--entrypoint=semgrep")));
    }

    #[test]
    fn a_result_file_scan_runs_only_the_malware_tools() {
        let t = Target {
            files: Some(Path::new("/x")),
            ..Target::default()
        };
        assert_eq!(wanted(&t), vec!["clamav", "yara"]);
        let t = Target {
            sbom: Some("{}"),
            ..Target::default()
        };
        assert_eq!(wanted(&t), vec!["osv-scanner", "grype"]);
    }

    #[test]
    fn unknown_severity_with_a_fix_is_high_and_blocks() {
        let osv = parse_osv(OSV).unwrap();
        let f = by_id(&osv, "osv-scanner:GHSA-zzzz-yyyy-xxxx:tar-stream@2.0.0");
        assert_eq!(f.severity, Severity::High);
        assert!(f.blocking);
        let grype = parse_grype(GRYPE).unwrap();
        let f = by_id(&grype, "grype:CVE-2026-55555:libexample@1.2.3");
        assert_eq!(f.severity, Severity::High);
        assert!(f.blocking);
        assert_eq!(unknown_severity(false), Severity::Medium);
    }

    #[test]
    fn clamav_alerts_for_unscannable_content_block() {
        let c = parse_clamav(CLAMAV);
        let rule = |p: &str| c.iter().find(|f| f.path.as_deref() == Some(p)).unwrap();
        let big = rule("image/layer2/usr/share/blob.bin");
        assert_eq!(big.rule, "malware.limits_exceeded");
        assert_eq!(rule("assets/secret.zip").rule, "malware.encrypted");
        assert_eq!(rule("artefacts/bin/tool").rule, "malware.broken");
        assert!(c.iter().all(|f| f.blocking));
    }

    #[test]
    fn semgrep_errors_and_skipped_files_block() {
        let f = semgrep_gaps(include_str!("../tests/fixtures/scanners/semgrep-gaps.json")).unwrap();
        let paths: BTreeSet<&str> = f.iter().filter_map(|x| x.path.as_deref()).collect();
        assert_eq!(
            paths,
            BTreeSet::from(["src/broken.js", "dist/app.js", "vendor/huge.js"]),
            "{f:#?}"
        );
        assert!(f
            .iter()
            .all(|x| x.blocking && x.rule == "sast.not_analysed"));
        assert!(semgrep_gaps(SEMGREP).unwrap().is_empty());
    }

    #[test]
    fn yara_stderr_errors_and_timeouts_block() {
        let f = yara_errors(
            "error scanning /scan/src/big.bin: timeout\nWARN[0000] podman noise\n\
             error scanning /scan/layers/1/etc/x: could not open file\n",
            "/scan/src",
        );
        assert_eq!(f.len(), 2, "{f:#?}");
        assert!(f.iter().all(|x| x.blocking && x.rule == "scanner.error"));
        assert_eq!(f[0].path.as_deref(), Some("big.bin"));
        assert_eq!(f[1].path.as_deref(), Some("image/layer1/etc/x"));
        // Through the scan: yara exited 0, the stderr line still fails the scan.
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let podman = Tools {
            yara_stderr: "error scanning /scan/src/x.js: timeout",
            ..Tools::default()
        };
        let o = security_scan(
            &podman,
            &cfg,
            "runsc",
            &Target {
                src: Some(&src),
                ..Target::default()
            },
            &dir.path().join("out"),
            1_800_000_000,
        );
        assert!(o
            .findings
            .iter()
            .any(|x| x.rule == "scanner.error" && x.id.starts_with("pigeonpost:yara:x.js")));
    }

    /// Publisher-owned scanner configuration never reaches gitleaks, OSV-Scanner or Semgrep:
    /// they scan a copy without it, with the platform's configuration and inline opt-outs off.
    #[test]
    fn publisher_scanner_configuration_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("sub/.semgrep")).unwrap();
        for (p, body) in [
            ("Cargo.lock", "# lock\n"),
            (".gitleaks.toml", "[allowlist]\npaths = ['.*']\n"),
            (".gitleaksignore", "*\n"),
            ("osv-scanner.toml", "[[PackageOverrides]]\nignore = true\n"),
            ("sub/.semgrepignore", "*\n"),
            ("sub/.semgrep/rules.yml", "rules: []\n"),
            ("src/main.rs", "fn main() {} // nosemgrep gitleaks:allow\n"),
        ] {
            std::fs::create_dir_all(src.join(p).parent().unwrap()).unwrap();
            std::fs::write(src.join(p), body).unwrap();
        }
        let copy = dir.path().join("copy");
        let mut dropped = scan_copy(&src, &copy).unwrap();
        dropped.sort();
        assert_eq!(
            dropped,
            [
                ".gitleaks.toml",
                ".gitleaksignore",
                "osv-scanner.toml",
                "sub/.semgrep/rules.yml",
                "sub/.semgrepignore"
            ]
        );
        assert!(copy.join("src/main.rs").exists() && copy.join("Cargo.lock").exists());
        assert_eq!(
            std::fs::read_to_string(copy.join(".semgrepignore")).unwrap(),
            SEMGREPIGNORE
        );

        let podman = Tools::default();
        let o = security_scan(
            &podman,
            &cfg,
            "runsc",
            &Target {
                src: Some(&src),
                lockfile: Some("Cargo.lock"),
                ..Target::default()
            },
            &dir.path().join("out"),
            1_800_000_000,
        );
        let calls = podman.calls.lock().unwrap().clone();
        let call = |tool: &str| {
            calls
                .iter()
                .find(|c| c.contains(&format!("--entrypoint={tool}")))
                .unwrap()
                .clone()
        };
        let original = format!("--volume={}:/scan/src:ro", src.display());
        for (tool, flags) in [
            (
                "gitleaks",
                &[
                    "--config=/rules/gitleaks.toml",
                    "--gitleaks-ignore-path=/rules/gitleaksignore",
                    "--ignore-gitleaks-allow",
                ][..],
            ),
            ("osv-scanner", &["--config=/rules/osv-scanner.toml"][..]),
            (
                "semgrep",
                &[
                    "--disable-nosem",
                    "--max-target-bytes=0",
                    "--timeout=0",
                    "--timeout-threshold=0",
                    "--workdir=/scan/src",
                ][..],
            ),
        ] {
            let c = call(tool);
            for f in flags {
                assert!(c.iter().any(|a| a == f), "{tool} {f}: {c:?}");
            }
            assert!(
                !c.contains(&original),
                "{tool} reads the publisher's own tree"
            );
            assert!(c.iter().any(|a| a.ends_with(":/scan/src:ro")), "{tool}");
        }
        assert!(!call("gitleaks")
            .iter()
            .any(|a| a.starts_with("--max-target-megabytes")));
        // ClamAV and YARA still read the tree as published.
        assert!(call("clamscan").contains(&original));
        let flagged: Vec<&str> = o
            .findings
            .iter()
            .filter(|f| f.rule == "evasion.scanner_config")
            .filter_map(|f| f.path.as_deref())
            .collect();
        assert_eq!(flagged.len(), 5, "{flagged:?}");
        assert_eq!(
            o.evidence["publisher_scanner_config_ignored"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
    }

    /// ClamAV and YARA read the image's unpacked layers, with ClamAV's alert flags; an unpack
    /// that fails falls back to the archive and blocks.
    #[test]
    fn image_layers_are_unpacked_in_the_sandbox_and_scanned() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let archive = dir.path().join("image.tar");
        std::fs::write(&archive, b"tar").unwrap();
        let target = Target {
            image_archive: Some(&archive),
            ..Target::default()
        };
        let podman = Tools::default();
        let o = security_scan(
            &podman,
            &cfg,
            "runsc",
            &target,
            &dir.path().join("out"),
            1_800_000_000,
        );
        let calls = podman.calls.lock().unwrap().clone();
        let unpack = calls
            .iter()
            .find(|c| c.contains(&"--entrypoint=/bin/sh".to_string()))
            .unwrap();
        assert!(unpack.contains(&"--network=none".to_string()));
        assert!(unpack.contains(&UNPACK_SCRIPT.to_string()));
        let clam = calls
            .iter()
            .find(|c| c.contains(&"--entrypoint=clamscan".to_string()))
            .unwrap();
        for f in [
            "--alert-exceeds-max=yes",
            "--alert-encrypted=yes",
            "--alert-broken=yes",
            "/scan/layers",
        ] {
            assert!(clam.contains(&f.to_string()), "{f}: {clam:?}");
        }
        assert!(!clam.contains(&"/scan/image".to_string()));
        let yara = calls
            .iter()
            .find(|c| c.contains(&"--entrypoint=yara".to_string()))
            .unwrap();
        assert_eq!(yara.last().unwrap(), "/scan/layers");
        assert_eq!(o.evidence["image_layers_unpacked"], true);

        let failing = Tools {
            fail: Some("/bin/sh"),
            ..Tools::default()
        };
        let o = security_scan(
            &failing,
            &cfg,
            "runsc",
            &target,
            &dir.path().join("out"),
            1_800_000_000,
        );
        assert!(o
            .findings
            .iter()
            .any(|f| f.rule == "scanner.error" && f.id.starts_with("pigeonpost:image unpack:")));
        let calls = failing.calls.lock().unwrap().clone();
        let clam = calls
            .iter()
            .find(|c| c.contains(&"--entrypoint=clamscan".to_string()))
            .unwrap();
        assert!(clam.contains(&"/scan/image".to_string()));
    }

    #[test]
    fn failure_findings_are_unique_to_their_run() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path());
        cfg.db_root = dir.path().join("nowhere");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let ids = || {
            let o = security_scan(
                &Tools::default(),
                &cfg,
                "runsc",
                &Target {
                    src: Some(&src),
                    ..Target::default()
                },
                &dir.path().join("out"),
                1_800_000_000,
            );
            o.findings
                .into_iter()
                .filter(|f| f.rule == "scanner.db_missing")
                .map(|f| f.id)
                .collect::<BTreeSet<_>>()
        };
        let (a, b) = (ids(), ids());
        assert_eq!(a.len(), 4);
        assert!(a.is_disjoint(&b), "{a:?} {b:?}");
    }
}
