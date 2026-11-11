//! The remote build and analysis plane (hardening, W1.3 and W1.5): `pigeonpost-nest-buildd`.
//!
//! Publisher source is untrusted, so it is never fetched, built or analysed on the worker host
//! that holds customer Nests and the KEK-unlocked volume keys. This service runs on a separate
//! host (a shared host for now, owner decision 2026-11-10; a dedicated VM later: the host is
//! configuration, never code), as an unprivileged user with rootless podman under gVisor.
//!
//! # API (WireGuard only, one bearer token per caller)
//!
//! | Route | Token | What |
//! |---|---|---|
//! | `POST /v1/builds` | submit (postbox) | queue `{build_id, publisher_id, repo_url, commit, credential?}`; idempotent by `build_id` |
//! | `GET /v1/builds/{build_id}` | submit (postbox) | state; when done the build record, the SBOM, the scan evidence and the reproducibility check |
//! | `GET /v1/artifacts/{image_digest}?offset=N` | artifact (nestd) | the release image as an OCI archive, in chunks |
//! | `GET /healthz` | none | liveness |
//!
//! The postbox pulls results. This host holds no credential that can act on the postbox or nestd.
//! The one exception is narrow and optional: for a publisher source connected with a deploy key
//! (M2b), the build names its build request (`credential: bld_…`) and this service fetches the
//! key from `GET /internal/v1/marketplace/build-requests/{build}/source-credential` with a token
//! scoped to that route alone (`[postbox] credential_token_file`; the postbox accepts it nowhere
//! else). The key lives in memory and in a 0600 file mounted read-only into the git container for
//! the fetch only, is removed right after, and is never logged or stored in the queue.
//!
//! # One build
//!
//! 1. **Source intake (R-62)**: `repo_url` must be `https://` on an allowlisted git host and
//!    `commit` a full object id. `git init/fetch --depth=1 <url> <commit>/checkout` run inside a
//!    throwaway container on the git network (nftables lets it reach the allowlisted hosts on 443
//!    and nothing else), with no capabilities and a read-only root. `HEAD` must be `commit`;
//!    submodules are refused; `.git` is removed before anything else reads the tree.
//! 2. **Build** ([`crate::release::Builder`]): vendored dependencies fetched on the fetch network,
//!    the release's compile step (`nest-build.toml`) in a gVisor `podman run` with no network,
//!    then the image assembled by `podman build --network=none --timestamp=0` from a Containerfile
//!    with no `RUN` step, so publisher code never runs inside buildah.
//! 3. **Reproducibility**: the same tree is built a second time from scratch; both builds must give
//!    the same release digest, or the build fails.
//! 4. **Artifact**: `podman save --format oci-archive`; the manifest digest in the archive's
//!    `index.json` is the image digest the release record (and so the attestation) names.
//! 5. **Analysis**: the deterministic scan, the advisory AI review on the self-hosted model when a
//!    review route is configured, and the behaviour test: the built image runs under the build
//!    runtime with no network, honeytokens planted on its volume, the release's own example jobs
//!    (`jobs/*.json`, or one synthetic job per operation) in its inbox, and every channel out of it
//!    (job results and its output) searched for the tokens.
//!
//! One build runs at a time; the queue is bounded.

use crate::analysis::{self, Budget, Finding, ReviewRouteConfig, StageReport};
use crate::artifacts::{
    digest_ok, oci_archive_manifest_digest, CHUNK_BYTES, SHA256_HEADER, SIZE_HEADER,
};
use crate::release::{build_tag, release_digest, BuildConfig, BuildRecord, Builder};
use crate::sandbox::Podman;
use crate::scanners::{self, ScannersConfig, Target};
use crate::util::{ct_eq, now, sha256_hex};
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// `buildd.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuilddConfig {
    /// The host's WireGuard address and port. Never every interface.
    pub bind: String,
    /// SQLite job queue.
    pub db: PathBuf,
    /// Scratch space for one build at a time (source tree, two build contexts).
    pub work_root: PathBuf,
    /// Saved release images, one OCI archive per image digest.
    pub artifact_root: PathBuf,
    /// SHA-256 (hex) of the postbox's submit token.
    pub submit_token_sha256_file: PathBuf,
    /// SHA-256 (hex) of the worker host's artifact-read token.
    pub artifact_token_sha256_file: PathBuf,
    pub git: GitConfig,
    pub build: BuildConfig,
    /// Build every release twice and require the same digest.
    #[serde(default = "default_true")]
    pub reproducible: bool,
    /// Most builds waiting at once (all publishers together).
    #[serde(default = "default_max_queued")]
    pub max_queued: i64,
    /// Saved images older than this are deleted (a pulled image lives on the worker host) once a
    /// re-scan has named them.
    #[serde(default = "default_keep_days")]
    pub keep_days: i64,
    /// An image no re-scan has named yet is kept this long, so the first re-scan of a release
    /// (the postbox sends one as soon as it is published) still finds it.
    #[serde(default = "default_keep_unscanned_days")]
    pub keep_unscanned_days: i64,
    /// The AI review route file (`analysis::ReviewRouteConfig`), when the advisory review runs here.
    #[serde(default)]
    pub ai_review_route: Option<PathBuf>,
    /// Run the behaviour test after the scan (default on).
    #[serde(default = "default_true")]
    pub behaviour_test: bool,
    /// How long the behaviour run may take.
    #[serde(default = "default_behaviour_secs")]
    pub behaviour_timeout_secs: u64,
    /// Where deploy-key credentials come from (M2b sources). Unset: deploy-key builds are refused.
    #[serde(default)]
    pub postbox: Option<PostboxConfig>,
    /// `[scanners]`: the security tools of the deterministic scan (OSV-Scanner, Grype, ClamAV,
    /// YARA, Semgrep, gitleaks). Unset: only the built-in rules run, and the scan says so in an
    /// advisory finding (development only; production sets it).
    #[serde(default)]
    pub scanners: Option<ScannersConfig>,
    /// `[behaviour]`: how the behaviour run is observed (egress trace, resource use). Unset: the
    /// run is not observed, and the stage says so in advisory findings.
    #[serde(default)]
    pub behaviour: Option<BehaviourConfig>,
}

/// `[behaviour]`: the observed behaviour run.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BehaviourConfig {
    /// The OCI runtime name mapped to `runsc-behaviour` in containers.conf: gVisor with strace of
    /// the network syscalls into a debug log per container.
    pub runtime: String,
    /// Where `runsc-behaviour` writes those logs (`<trace_root>/<container id>/`).
    pub trace_root: PathBuf,
    /// The cgroup v2 directory nestbuild's containers run under
    /// (`/sys/fs/cgroup/user.slice/user-<uid>.slice/user@<uid>.service`). Unset: resource use is
    /// not measured.
    #[serde(default)]
    pub cgroup: Option<PathBuf>,
    /// The marketplace pool's per-Nest limits.
    pub budget: Budget,
}

/// `[postbox]`: the postbox's source-credential route, over WireGuard.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostboxConfig {
    /// `https://…` or `http://<private address>:<port>`.
    pub url: String,
    /// File holding the credential-route token (0400). It opens that one route and nothing else.
    pub credential_token_file: PathBuf,
}

/// `[git]`: how publisher source is fetched.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitConfig {
    /// An image with `git`, pinned by digest.
    pub image: String,
    /// The podman network git runs on; nftables limits it to the allowlisted hosts.
    pub network: String,
    /// Git hosts a repository may be on, exact names (`github.com`, `codeberg.org`, …).
    pub allowed_hosts: Vec<String>,
    /// SSH host keys of the allowed hosts (`known_hosts` format), for deploy-key sources. Host
    /// keys are pinned: an unknown key fails the fetch. Unset: deploy-key builds are refused.
    #[serde(default)]
    pub known_hosts: Option<PathBuf>,
}

fn default_true() -> bool {
    true
}
fn default_max_queued() -> i64 {
    20
}
fn default_keep_days() -> i64 {
    14
}
fn default_keep_unscanned_days() -> i64 {
    120
}
fn default_behaviour_secs() -> u64 {
    60
}

pub fn pinned(image: &str) -> bool {
    image.split_once("@sha256:").is_some_and(|(name, d)| {
        !name.is_empty() && d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

fn hostname_ok(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && h.contains('.')
        && h.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
        && !h.starts_with(['.', '-'])
}

impl BuilddConfig {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let cfg: BuilddConfig =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        cfg.check()?;
        Ok(cfg)
    }

    pub fn check(&self) -> Result<(), String> {
        if self.bind.starts_with("0.0.0.0") || self.bind.starts_with("[::]") {
            return Err("bind a specific private address, not every interface".into());
        }
        self.build.check()?;
        if !pinned(&self.git.image) {
            return Err("git.image must be pinned by digest".into());
        }
        if self.git.network.is_empty() || matches!(self.git.network.as_str(), "host" | "none") {
            return Err("git.network: a named podman network of its own".into());
        }
        if self.git.allowed_hosts.is_empty()
            || !self.git.allowed_hosts.iter().all(|h| hostname_ok(h))
        {
            return Err("git.allowed_hosts: one or more lower-case host names".into());
        }
        if let Some(p) = &self.postbox {
            if !crate::config::private_url_ok(&p.url) {
                return Err("postbox.url: https, or http to a private address".into());
            }
        }
        if let Some(sc) = &self.scanners {
            sc.check()?;
        }
        if let Some(b) = &self.behaviour {
            if b.runtime.is_empty() || !b.trace_root.is_absolute() {
                return Err("behaviour: a runtime name and an absolute trace_root".into());
            }
            if b.cgroup
                .as_ref()
                .is_some_and(|c| !c.starts_with("/sys/fs/cgroup"))
            {
                return Err("behaviour.cgroup: a directory under /sys/fs/cgroup".into());
            }
            if b.budget.memory_mib == 0 || b.budget.cpu_millis == 0 || b.budget.job_seconds == 0 {
                return Err("behaviour.budget: positive limits".into());
            }
        }
        if !(5..=600).contains(&self.behaviour_timeout_secs) {
            return Err("behaviour_timeout_secs: 5-600".into());
        }
        if self.max_queued < 1 || self.keep_days < 1 {
            return Err("max_queued and keep_days must be positive".into());
        }
        if self.keep_unscanned_days < self.keep_days.max(45) {
            return Err(
                "keep_unscanned_days: at least keep_days and 45 (a release reviewed and \
                 published in that time is re-scanned with its image)"
                    .into(),
            );
        }
        for p in [&self.work_root, &self.artifact_root] {
            if !p.is_absolute() {
                return Err(format!("{}: an absolute path", p.display()));
            }
        }
        Ok(())
    }
}

/// A build id as the postbox mints it: `sbd_` and 26 lower-case base32 characters.
pub fn valid_build_id(id: &str) -> bool {
    id.strip_prefix("sbd_").is_some_and(|rest| {
        rest.len() == 26
            && rest
                .bytes()
                .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
    })
}

fn valid_ref(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// R-62: an `https://` URL on an allowlisted git host with a plain path, and a full commit id.
pub fn source_ok(git: &GitConfig, repo_url: &str, commit: &str) -> Result<(), String> {
    source_ok_with(git, repo_url, commit, false)
}

/// The host and path of a deploy-key repository: `git@host:path` or `ssh://git@host/path`.
fn ssh_parts(repo_url: &str) -> Option<(&str, &str)> {
    if let Some(rest) = repo_url.strip_prefix("ssh://git@") {
        return rest.split_once('/');
    }
    let rest = repo_url.strip_prefix("git@")?;
    if rest.contains("://") {
        return None;
    }
    rest.split_once(':')
}

/// [`source_ok`], also taking an SSH repository when the build carries a deploy-key credential.
pub fn source_ok_with(
    git: &GitConfig,
    repo_url: &str,
    commit: &str,
    ssh: bool,
) -> Result<(), String> {
    let full_hex = |s: &str| {
        (s.len() == 40 || s.len() == 64)
            && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    };
    if !full_hex(commit) {
        return Err("commit: a full lower-case object id (40 or 64 hex digits)".into());
    }
    let (host, path) = match repo_url.strip_prefix("https://") {
        Some(rest) => rest.split_once('/').ok_or("repo_url: host and path")?,
        None if ssh => {
            ssh_parts(repo_url).ok_or("repo_url: git@host:path or ssh://git@host/path")?
        }
        None => return Err("repo_url: https only".into()),
    };
    if !git.allowed_hosts.iter().any(|h| h == host) {
        return Err("repo_url: this git host is not on the allowlist".into());
    }
    if path.is_empty()
        || repo_url.len() > 300
        || path.contains("..")
        || path.contains("//")
        || !path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'/' | b'-'))
    {
        return Err("repo_url: a plain repository path".into());
    }
    Ok(())
}

/// One `git` invocation in a throwaway container on the git network.
pub fn git_args(cfg: &BuilddConfig, src: &Path, git: &[&str]) -> Vec<String> {
    git_args_with(cfg, src, None, git)
}

/// [`git_args`] with a deploy key: `cred` (holding `id` and `known_hosts`) is mounted read-only
/// and ssh uses that key only, against pinned host keys, never prompting.
pub fn git_args_with(
    cfg: &BuilddConfig,
    src: &Path,
    cred: Option<&Path>,
    git: &[&str],
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--pull=never".into(),
        format!("--runtime={}", cfg.build.runtime),
        format!("--network={}", cfg.git.network),
        "--cap-drop=ALL".into(),
        "--security-opt=no-new-privileges".into(),
        "--read-only".into(),
        "--tmpfs=/tmp:rw,nosuid,nodev,size=64m".into(),
        "--env=HOME=/tmp".into(),
        "--env=GIT_TERMINAL_PROMPT=0".into(),
        "--env=GIT_CONFIG_NOSYSTEM=1".into(),
        format!("--volume={}:/src:rw", src.display()),
        "--workdir=/src".into(),
    ];
    if let Some(c) = cred {
        args.push(format!("--volume={}:/cred:ro", c.display()));
        args.push(
            "--env=GIT_SSH_COMMAND=ssh -i /cred/id -o IdentitiesOnly=yes -o BatchMode=yes \
             -o StrictHostKeyChecking=yes -o UserKnownHostsFile=/cred/known_hosts"
                .into(),
        );
    }
    args.extend(cfg.build.limit_args());
    // Explicit, so an image whose own ENTRYPOINT is `git` (alpine/git) does not run `git git …`.
    args.push(crate::release::entrypoint("git"));
    args.push(cfg.git.image.clone());
    args.extend(git.iter().map(|s| s.to_string()));
    args
}

fn run_args(podman: &dyn Podman, args: &[String]) -> Result<String, String> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    podman.run(&refs)
}

/// Fetch `commit` of `repo_url` into `src` (an empty directory), inside the git container.
pub fn fetch_source(
    podman: &dyn Podman,
    cfg: &BuilddConfig,
    repo_url: &str,
    commit: &str,
    src: &Path,
) -> Result<(), String> {
    fetch_source_with(podman, cfg, repo_url, commit, src, None)
}

/// A deploy key on disk for one fetch: removed when dropped, whatever happened.
struct CredDir(PathBuf);

impl Drop for CredDir {
    fn drop(&mut self) {
        let key = self.0.join("id");
        if let Ok(len) = std::fs::metadata(&key).map(|m| m.len()) {
            let _ = std::fs::write(&key, vec![0u8; len as usize]);
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path).map_err(|e| e.to_string())?;
    f.write_all(bytes).map_err(|e| e.to_string())
}

/// [`fetch_source`] with an optional deploy key (an OpenSSH private key), used for this fetch only.
pub fn fetch_source_with(
    podman: &dyn Podman,
    cfg: &BuilddConfig,
    repo_url: &str,
    commit: &str,
    src: &Path,
    key: Option<&str>,
) -> Result<(), String> {
    source_ok_with(&cfg.git, repo_url, commit, key.is_some())?;
    std::fs::create_dir_all(src).map_err(|e| e.to_string())?;
    let cred = match key {
        None => None,
        Some(k) => {
            let hosts = cfg
                .git
                .known_hosts
                .as_ref()
                .ok_or("deploy-key sources need git.known_hosts (pinned host keys)")?;
            let dir = src.with_file_name("cred");
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let guard = CredDir(dir.clone());
            let pinned = std::fs::read(hosts).map_err(|e| format!("git.known_hosts: {e}"))?;
            write_private(&dir.join("known_hosts"), &pinned)?;
            write_private(&dir.join("id"), k.as_bytes())?;
            Some(guard)
        }
    };
    let cred_dir = cred.as_ref().map(|c| c.0.as_path());
    let protocol = if cred.is_some() {
        "protocol.ssh.allow=always"
    } else {
        "protocol.https.allow=always"
    };
    run_args(podman, &git_args(cfg, src, &["init", "--quiet", "."]))?;
    run_args(
        podman,
        &git_args_with(
            cfg,
            src,
            cred_dir,
            &[
                "-c",
                "protocol.allow=never",
                "-c",
                protocol,
                "-c",
                "http.followRedirects=false",
                "-c",
                "core.hooksPath=/dev/null",
                "fetch",
                "--quiet",
                "--depth=1",
                "--no-tags",
                "--no-recurse-submodules",
                repo_url,
                commit,
            ],
        ),
    )?;
    run_args(
        podman,
        &git_args(
            cfg,
            src,
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "advice.detachedHead=false",
                "checkout",
                "--quiet",
                "--detach",
                "FETCH_HEAD",
            ],
        ),
    )?;
    // The key is gone before anything reads the tree.
    drop(cred);
    let head = run_args(podman, &git_args(cfg, src, &["rev-parse", "HEAD"]))?;
    if head.trim() != commit {
        return Err("the fetched tree is not the requested commit".into());
    }
    if src.join(".gitmodules").exists() {
        return Err("submodules are not supported: vendor them into the repository".into());
    }
    std::fs::remove_dir_all(src.join(".git")).map_err(|e| format!(".git: {e}"))?;
    Ok(())
}

/// `podman` through a shared reference, for the [`Builder`].
struct ByRef<'a>(&'a dyn Podman);

impl Podman for ByRef<'_> {
    fn run(&self, args: &[&str]) -> Result<String, String> {
        self.0.run(args)
    }

    fn run_capture(&self, args: &[&str]) -> (bool, String, String) {
        self.0.run_capture(args)
    }
}

/// What one finished build produced, before the AI review.
pub struct Built {
    pub record: BuildRecord,
    pub scan: StageReport,
    /// The behaviour test, when it ran.
    pub behaviour: Option<StageReport>,
    pub reproducibility: Value,
    pub archive: PathBuf,
    pub archive_sha256: String,
    pub archive_size: u64,
}

fn sha256_file(path: &Path) -> Result<(String, u64), String> {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut size = 0u64;
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        size += n as u64;
        h.update(&buf[..n]);
    }
    Ok((crate::util::hex(&h.finalize()), size))
}

/// Steps 1–4 and the deterministic scan. Blocking; `work` is this build's own scratch directory.
pub fn build_from_source(
    podman: &dyn Podman,
    cfg: &BuilddConfig,
    repo_url: &str,
    commit: &str,
    work: &Path,
) -> Result<Built, String> {
    build_from_source_with(podman, cfg, repo_url, commit, work, None)
}

/// [`build_from_source`] with an optional deploy key for the fetch.
pub fn build_from_source_with(
    podman: &dyn Podman,
    cfg: &BuilddConfig,
    repo_url: &str,
    commit: &str,
    work: &Path,
    key: Option<&str>,
) -> Result<Built, String> {
    let src = work.join("src");
    fetch_source_with(podman, cfg, repo_url, commit, &src, key)?;
    let builder = Builder {
        cfg: cfg.build.clone(),
        podman: ByRef(podman),
    };
    let mut record = builder.build(&src, &work.join("first"))?;
    let mut digests = vec![record.release_digest.clone()];
    if cfg.reproducible {
        let again = builder.build(&src, &work.join("second"))?;
        digests.push(again.release_digest.clone());
        if again.release_digest != record.release_digest {
            return Err(format!(
                "not reproducible: two builds of the same tree gave {} and {}",
                record.release_digest, again.release_digest
            ));
        }
    }
    // The artifact, and the digest the attestation will name: the archive's own manifest.
    std::fs::create_dir_all(&cfg.artifact_root).map_err(|e| e.to_string())?;
    let part = cfg.artifact_root.join(format!(
        "{}.part",
        &sha256_hex(work.to_string_lossy().as_bytes())[..16]
    ));
    let _ = std::fs::remove_file(&part);
    let tag = build_tag(&cfg.build, &record.source_digest);
    run_args(
        podman,
        &[
            "save".into(),
            "--format".into(),
            "oci-archive".into(),
            "--output".into(),
            part.display().to_string(),
            tag,
        ],
    )?;
    let manifest = oci_archive_manifest_digest(&part)?;
    if manifest != record.image_digest {
        record.image = format!("{}@{manifest}", cfg.build.image_name);
        record.image_digest = manifest;
        record.release_digest = release_digest(
            &record.source_digest,
            &record.vendor_digest,
            &record.manifest_digest,
            &record.sbom_digest,
            &record.image_digest,
        );
    }
    let archive = cfg.artifact_root.join(format!(
        "{}.tar",
        record.image_digest.trim_start_matches("sha256:")
    ));
    std::fs::rename(&part, &archive).map_err(|e| e.to_string())?;
    let (archive_sha256, archive_size) = sha256_file(&archive)?;
    let ecosystem = record.isolation["ecosystem"].clone();
    let compile = record.isolation["compile"].clone();
    record.isolation = json!({
        "plane": "build host (pigeonpost-nest-buildd), separate from the worker host",
        "ecosystem": ecosystem,
        "compile": compile,
        "runtime": cfg.build.runtime,
        "rootless": true,
        "git_network": cfg.git.network,
        "fetch_network": cfg.build.fetch_network,
        "build_network": "none",
        "source": { "repo_url": repo_url, "commit": commit },
        "reproducible": cfg.reproducible,
    });
    let scan = analysis::deterministic_scan(&src)?;
    let scan = match &cfg.scanners {
        Some(sc) => {
            let eco = crate::ecosystem::detect(&src)?;
            let artefacts = work.join("first").join("context");
            let outcome = scanners::security_scan(
                podman,
                sc,
                &cfg.build.runtime,
                &Target {
                    src: Some(&src),
                    artefacts: artefacts.is_dir().then_some(artefacts.as_path()),
                    image_archive: Some(&archive),
                    sbom: Some(&record.sbom),
                    lockfile: Some(eco.lockfile()),
                    files: None,
                },
                &work.join("scan"),
                now(),
            );
            scan.extended(outcome.findings, "security_tools", outcome.evidence)
        }
        None => scan.extended(
            vec![Finding::of(
                "pigeonpost",
                "scanner.not_configured",
                analysis::Severity::Medium,
                "scanners",
                "this build plane runs no vulnerability, malware, SAST or gitleaks scan",
                false,
            )],
            "security_tools",
            json!({ "configured": false }),
        ),
    };
    let behaviour = if cfg.behaviour_test {
        Some(behaviour_test(
            podman,
            cfg,
            &build_tag(&cfg.build, &record.source_digest),
            &record.manifest,
            &src,
            &work.join("behaviour"),
        )?)
    } else {
        None
    };
    Ok(Built {
        record,
        scan,
        behaviour,
        reproducibility: json!({
            "builds": digests.len(),
            "release_digests": digests,
            "equal": true,
        }),
        archive,
        archive_sha256,
        archive_size,
    })
}

/// The jobs the behaviour test delivers: the release's own example jobs (`jobs/*.json`, as
/// `pigeonpost nest run` reads them), or one per operation with every field filled. Each job's
/// first text field that has room also carries that job's buyer-data canary.
fn behaviour_jobs(manifest: &crate::release::Manifest, src: &Path) -> Vec<(Value, Option<String>)> {
    let mut jobs = Vec::new();
    if let Ok(dir) = std::fs::read_dir(src.join("jobs")) {
        let mut files: Vec<PathBuf> = dir
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        files.sort();
        for f in files.into_iter().take(8) {
            if let Some(v) = std::fs::read_to_string(&f)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .filter(|v| v["operation"].is_string() && v["payload"].is_object())
            {
                jobs.push(json!({
                    "operation": v["operation"],
                    "operation_version": v["operation_version"],
                    "payload": v["payload"],
                }));
            }
        }
    }
    if jobs.is_empty() {
        for op in &manifest.operations {
            let payload: serde_json::Map<String, Value> = op
                .fields
                .iter()
                .map(|f| (f.name.clone(), json!("behaviour test")))
                .collect();
            jobs.push(json!({
                "operation": op.name,
                "operation_version": op.version,
                "payload": payload,
            }));
        }
    }
    jobs.into_iter()
        .enumerate()
        .map(|(i, mut j)| {
            j["job_id"] = json!(format!("job_behaviour{i:02}"));
            j["config"] = json!({});
            let canary = plant_canary(&mut j, manifest);
            (j, canary)
        })
        .collect()
}

/// Append a fresh canary to the job's first string field whose `max_chars` leaves room for it.
fn plant_canary(job: &mut Value, manifest: &crate::release::Manifest) -> Option<String> {
    let op = manifest
        .operations
        .iter()
        .find(|o| job["operation"] == o.name.as_str())?;
    let c = analysis::canary();
    for f in &op.fields {
        let Some(text) = job["payload"][&f.name].as_str() else {
            continue;
        };
        let with = format!("{text} {c}");
        if with.chars().count() <= f.max_chars {
            job["payload"][&f.name] = json!(with);
            return Some(c);
        }
    }
    None
}

/// Memory now (bytes) and CPU time so far (µs) of a cgroup v2 directory.
pub fn cgroup_sample(dir: &Path) -> Option<(u64, u64)> {
    let mem = std::fs::read_to_string(dir.join("memory.current"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let cpu = std::fs::read_to_string(dir.join("cpu.stat"))
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("usage_usec "))?
        .trim()
        .parse()
        .ok()?;
    Some((mem, cpu))
}

/// The gVisor strace logs of one behaviour container, read and removed.
/// The most trace one behaviour run may leave (`runsc-behaviour`'s debug logs) before it counts as
/// a flood. `netlog/` itself is a size-limited tmpfs (README); this is the reader's own cap.
pub const TRACE_MAX_BYTES: u64 = 64 << 20;

/// What the egress trace of one behaviour run holds.
#[derive(Debug, PartialEq, Eq)]
pub enum Trace {
    /// No container id, no directory, or no readable boot (sentry) log: nothing was observed.
    Missing,
    /// More than [`TRACE_MAX_BYTES`]: the release drowned its own trace.
    Flood(u64),
    Text(String),
}

/// The gVisor strace logs of one behaviour container, read (bytes, so a non-UTF-8 process name
/// cannot hide a line) with a cap, and removed. The sentry's boot log must be among them: the
/// strace lines are there, and without it a run proves nothing.
pub fn take_trace(trace_root: &Path, cid_file: &Path) -> Trace {
    let Some(id) = std::fs::read_to_string(cid_file).ok() else {
        return Trace::Missing;
    };
    let id = id.trim();
    if id.len() < 12 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Trace::Missing;
    }
    let dir = trace_root.join(id);
    let out = read_trace_dir(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn read_trace_dir(dir: &Path) -> Trace {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Trace::Missing;
    };
    let mut text = String::new();
    let mut total = 0u64;
    let mut boot = false;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let size = e.metadata().map(|m| m.len()).unwrap_or(0);
        total = total.saturating_add(size);
        if total > TRACE_MAX_BYTES {
            return Trace::Flood(total);
        }
        let mut bytes = Vec::new();
        let read = std::fs::File::open(e.path())
            .and_then(|f| f.take(TRACE_MAX_BYTES + 1).read_to_end(&mut bytes));
        if read.is_err() {
            if name.contains(".boot") {
                return Trace::Missing;
            }
            continue;
        }
        if name.contains(".boot") && !bytes.is_empty() {
            boot = true;
        }
        text.push_str(&String::from_utf8_lossy(&bytes));
        text.push('\n');
    }
    if boot {
        Trace::Text(text)
    } else {
        Trace::Missing
    }
}

/// The behaviour test (R-63) on the build plane, after the scan: the image just built runs under
/// the build runtime with **no network**, no capabilities, a read-only root and the worker's
/// user, honeytokens planted on its volume and test jobs in its inbox. Its job results and its
/// output are searched for the tokens; a job left unanswered fails the stage too. There is no
/// broker on the build plane, so a model call ends in the SDK's `no_broker` error: an answered
/// job is any result, succeeded or failed.
///
/// With `[behaviour]` set the run is also observed: it runs under the `runsc-behaviour` runtime,
/// whose strace of the network syscalls is diffed against the declared egress (an undeclared
/// attempt blocks), the sandbox's cgroup is sampled for memory and CPU and the answers are timed
/// against the pool's budget, and each job's buyer-data canary must stay in that job's result.
pub fn behaviour_test(
    podman: &dyn Podman,
    cfg: &BuilddConfig,
    tag: &str,
    manifest: &crate::release::Manifest,
    src: &Path,
    volume: &Path,
) -> Result<StageReport, String> {
    let _ = std::fs::remove_dir_all(volume);
    for d in ["jobs/in", "jobs/out", "run"] {
        std::fs::create_dir_all(volume.join(d)).map_err(|e| e.to_string())?;
    }
    let tokens = analysis::Honeytokens::new(3);
    tokens.plant(volume)?;
    let jobs = behaviour_jobs(manifest, src);
    for (j, _) in &jobs {
        let id = j["job_id"].as_str().unwrap_or_default();
        std::fs::write(volume.join(format!("jobs/in/{id}.json")), j.to_string())
            .map_err(|e| e.to_string())?;
    }
    #[cfg(unix)]
    {
        // The sandbox user owns nothing on the host; the volume is the one place it may write.
        use std::os::unix::fs::PermissionsExt;
        for d in ["", "jobs", "jobs/in", "jobs/out", "run"] {
            let _ =
                std::fs::set_permissions(volume.join(d), std::fs::Permissions::from_mode(0o777));
        }
    }
    let observe = cfg.behaviour.as_ref();
    let cid_file = volume.with_extension("cid");
    let _ = std::fs::remove_file(&cid_file);
    let runtime = observe.map_or(cfg.build.runtime.as_str(), |o| o.runtime.as_str());
    let mut args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--pull=never".into(),
        format!("--runtime={runtime}"),
        "--network=none".into(),
        "--cap-drop=ALL".into(),
        "--security-opt=no-new-privileges".into(),
        "--read-only".into(),
        "--tmpfs=/tmp:rw,nosuid,nodev,size=64m".into(),
        "--user=10001:10001".into(),
        "--env=NEST_ROOT=/nest".into(),
        "--env=NEST_ID=nst_behaviour".into(),
        format!("--volume={}:/nest:rw", volume.display()),
        format!("--timeout={}", cfg.behaviour_timeout_secs),
    ];
    if observe.is_some() {
        args.push(format!("--cidfile={}", cid_file.display()));
    }
    args.extend(cfg.build.limit_args());
    args.push(tag.to_string());

    // A worker serves forever; the timeout ends it. Its exit status says nothing here. While it
    // runs, the cgroup is sampled and each answer's arrival is timed.
    let cgroup = observe.and_then(|o| o.cgroup.as_deref());
    let started = std::time::Instant::now();
    let base = cgroup.and_then(cgroup_sample);
    let mut peak = 0u64;
    let mut answered_at: std::collections::BTreeMap<String, u64> = Default::default();
    let out_dir = volume.join("jobs/out");
    let poll = |peak: &mut u64, answered_at: &mut std::collections::BTreeMap<String, u64>| {
        if let Some((mem, _)) = cgroup.and_then(cgroup_sample) {
            *peak = (*peak).max(mem);
        }
        // An answer counts from when it is a complete, valid result, not when a file appears:
        // a release cannot start the clock with an empty or partial file.
        if let Ok(dir) = std::fs::read_dir(&out_dir) {
            for e in dir.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if answered_at.contains_key(&name) {
                    continue;
                }
                let valid = std::fs::read(e.path())
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                    .is_some_and(|v| v["status"].is_string());
                if valid {
                    answered_at.insert(name, started.elapsed().as_millis() as u64);
                }
            }
        }
    };
    // Both streams, whatever the exit status (the timeout ends a worker, so it never exits 0).
    let output = std::thread::scope(|s| {
        let run = s.spawn(|| {
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let (_, stdout, stderr) = podman.run_capture(&refs);
            format!("{stdout}\n{stderr}")
        });
        while !run.is_finished() {
            poll(&mut peak, &mut answered_at);
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        run.join().unwrap_or_default()
    });
    poll(&mut peak, &mut answered_at);
    let total_ms = started.elapsed().as_millis() as u64;
    let end = cgroup.and_then(cgroup_sample);

    let mut results = Vec::new();
    let mut per_job = Vec::new();
    let mut answered = 0;
    if let Ok(dir) = std::fs::read_dir(&out_dir) {
        for entry in dir.flatten() {
            let bytes = std::fs::read(entry.path()).unwrap_or_default();
            if serde_json::from_slice::<Value>(&bytes)
                .ok()
                .is_some_and(|v| v["status"].is_string())
            {
                answered += 1;
            }
            let job = entry
                .file_name()
                .to_string_lossy()
                .trim_end_matches(".json")
                .to_string();
            results.extend_from_slice(&bytes);
            per_job.push((job, bytes));
        }
    }

    // What the run did beyond its answers.
    let mut seen = Vec::new();
    let canaries: Vec<(String, Option<String>)> = jobs
        .iter()
        .map(|(j, c)| (j["job_id"].as_str().unwrap_or("").to_string(), c.clone()))
        .collect();
    seen.extend(analysis::canary_findings(
        &canaries,
        &per_job,
        output.as_bytes(),
    ));
    let (egress_facts, attempts) = match observe {
        Some(o) => match take_trace(&o.trace_root, &cid_file) {
            Trace::Flood(bytes) => {
                seen.push(Finding::of(
                    "gvisor",
                    "egress.flood",
                    analysis::Severity::High,
                    "flood",
                    format!(
                        "the run left {} MiB of network trace (limit {} MiB); its attempts \
                         cannot all be read",
                        bytes >> 20,
                        TRACE_MAX_BYTES >> 20
                    ),
                    true,
                ));
                (json!({ "traced": false, "flood": true }), Vec::new())
            }
            Trace::Text(trace) => {
                let attempts = analysis::parse_strace(&trace);
                let found = analysis::egress_findings(&manifest.egress, &attempts);
                let n = found.len();
                seen.extend(found);
                (
                    json!({ "traced": true, "attempts": attempts.len(), "undeclared": n }),
                    attempts,
                )
            }
            Trace::Missing => {
                seen.push(analysis::egress_unobserved(true));
                (json!({ "traced": false }), Vec::new())
            }
        },
        None => {
            seen.push(analysis::egress_unobserved(false));
            (json!({ "traced": false, "configured": false }), Vec::new())
        }
    };
    let _ = std::fs::remove_file(&cid_file);
    let resource_facts = match (observe, base, end) {
        (Some(o), Some((mem0, cpu0)), Some((_, cpu1))) => {
            let mut times: Vec<u64> = answered_at.values().copied().collect();
            times.sort_unstable();
            let slowest = times
                .iter()
                .scan(0u64, |prev, t| {
                    let d = t.saturating_sub(*prev);
                    *prev = *t;
                    Some(d)
                })
                .max();
            // CPU counts until the run ends: work done after the last answer is still the
            // release's.
            let active_ms = total_ms.max(1);
            let usage = analysis::Usage {
                // The cgroup's own baseline (the user's systemd and podman) is not the release's.
                peak_memory_bytes: peak.max(mem0).saturating_sub(mem0),
                cpu_usec: cpu1.saturating_sub(cpu0),
                active_ms,
                slowest_job_ms: slowest,
            };
            seen.extend(analysis::resource_findings(&o.budget, &usage));
            json!({
                "measured": true,
                "peak_memory_mib": usage.peak_memory_bytes >> 20,
                "cpu_millis": usage.cpu_usec / active_ms,
                "slowest_job_ms": usage.slowest_job_ms,
                "budget": { "memory_mib": o.budget.memory_mib, "cpu_millis": o.budget.cpu_millis,
                            "job_seconds": o.budget.job_seconds },
            })
        }
        _ => {
            seen.push(Finding::of(
                "pigeonpost",
                "resources.unmeasured",
                analysis::Severity::Low,
                "resources",
                "this build plane does not measure the behaviour run's resource use",
                false,
            ));
            json!({ "measured": false })
        }
    };
    let report = analysis::behaviour_report(
        &tokens,
        &[
            ("job results".into(), results),
            ("output".into(), output.into_bytes()),
        ],
        jobs.len(),
        answered,
        &format!("{runtime} on the build plane, network none"),
    )
    .extended(
        seen,
        "observed",
        json!({
            "egress": egress_facts,
            "attempts": attempts.iter().take(50).map(|a| json!({
                "syscall": a.syscall, "family": a.family, "addr": a.addr, "port": a.port,
            })).collect::<Vec<_>>(),
            "resources": resource_facts,
            "buyer_data_canaries": canaries.iter().filter(|(_, c)| c.is_some()).count(),
        }),
    );
    let _ = std::fs::remove_dir_all(volume);
    Ok(report)
}

/// A stage as the postbox's stage route takes it.
pub fn stage_body(r: &StageReport) -> Value {
    json!({
        "stage": r.stage,
        "decision": r.decision,
        "reason": r.reason,
        "evidence": r.evidence_json(),
        "evidence_digest": r.evidence_digest(),
    })
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS builds (
    id             TEXT PRIMARY KEY,
    publisher_id   TEXT NOT NULL,
    repo_url       TEXT NOT NULL,
    commit_sha     TEXT NOT NULL,
    state          TEXT NOT NULL CHECK (state IN ('queued','running','succeeded','failed')),
    error          TEXT,
    result         TEXT,
    image_digest   TEXT,
    archive        TEXT,
    archive_sha256 TEXT,
    archive_size   INTEGER,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL,
    -- The build request whose deploy key the fetch uses (never the key).
    credential     TEXT
);
CREATE INDEX IF NOT EXISTS builds_by_state ON builds(state, created_at);
CREATE INDEX IF NOT EXISTS builds_by_image ON builds(image_digest);
-- Scans the postbox asks for outside a build: a hosted service's result files (the conformance
-- run) and a published release again (the monthly re-scan). The request is kept without file
-- contents; the files wait in the work root until the scan runs.
CREATE TABLE IF NOT EXISTS scans (
    id         TEXT PRIMARY KEY,
    kind       TEXT NOT NULL CHECK (kind IN ('files','release')),
    state      TEXT NOT NULL CHECK (state IN ('queued','running','succeeded','failed')),
    request    TEXT NOT NULL,
    result     TEXT,
    error      TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS scans_by_state ON scans(state, created_at);
-- Images of published releases: kept past keep_days so the monthly re-scan can open them again.
-- Refreshed by every re-scan request; dropped when the postbox stops naming them.
CREATE TABLE IF NOT EXISTS retained_images (
    image_digest TEXT PRIMARY KEY,
    release_id   TEXT NOT NULL,
    updated_at   INTEGER NOT NULL
);
-- Every image a re-scan has ever named. Until an image is here, its archive is kept for
-- keep_unscanned_days (not keep_days), so the first re-scan of a release always finds it.
CREATE TABLE IF NOT EXISTS rescanned_images (
    image_digest TEXT PRIMARY KEY,
    first_at     INTEGER NOT NULL
);
";

/// Files one result scan may carry, and the largest (decoded).
pub const SCAN_MAX_FILES: usize = 5;
pub const SCAN_MAX_FILE_BYTES: usize = 20 * 1024 * 1024;
/// The largest SBOM a re-scan may carry.
pub const SCAN_MAX_SBOM_BYTES: usize = 8 * 1024 * 1024;
/// A retained image is released this long after the last re-scan that named it.
const RETAIN_SECS: i64 = 62 * 86_400;
/// Finished scans (and their files) are kept this long for the postbox to read.
const SCAN_KEEP_SECS: i64 = 30 * 86_400;

/// A scan id as the postbox mints it: `scn_` and 26 lower-case base32 characters.
pub fn valid_scan_id(id: &str) -> bool {
    id.strip_prefix("scn_").is_some_and(|rest| {
        rest.len() == 26
            && rest
                .bytes()
                .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
    })
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanFile {
    pub name: String,
    pub media_type: String,
    pub content_b64: String,
}

/// `POST /v1/scans`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanReq {
    pub scan_id: String,
    /// `files` (ClamAV and YARA on the files) or `release` (OSV and Grype on the SBOM, and
    /// Grype, ClamAV and YARA on the image when this host still has it).
    pub kind: String,
    #[serde(default)]
    pub files: Vec<ScanFile>,
    #[serde(default)]
    pub release_id: Option<String>,
    #[serde(default)]
    pub image_digest: Option<String>,
    #[serde(default)]
    pub sbom: Option<String>,
}

/// The service.
pub struct Buildd {
    pub cfg: BuilddConfig,
    pub podman: Arc<dyn Podman>,
    conn: Mutex<Connection>,
    submit_sha256: String,
    artifact_sha256: String,
    review: Option<(ReviewRouteConfig, String)>,
    /// Deploy keys for M2b sources (see the module docs).
    pub credentials: Credentials,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildReq {
    pub build_id: String,
    pub publisher_id: String,
    pub repo_url: String,
    pub commit: String,
    /// A deploy-key source (M2b): the publisher's build request (`bld_…`) whose key this service
    /// fetches from the postbox for the fetch step.
    #[serde(default)]
    pub credential: Option<String>,
}

/// Where deploy keys come from.
pub enum Credentials {
    /// Deploy-key builds are refused.
    None,
    Http {
        url: String,
        token: zeroize::Zeroizing<String>,
        client: reqwest::Client,
    },
    /// Tests: a fixed `(repository, key)` for every request, and the requests made.
    #[cfg(test)]
    Fixed(String, String, Mutex<Vec<String>>),
}

impl Credentials {
    /// The deploy key of build request `build` for `repo_url`. The repository the postbox names
    /// must be the one being fetched.
    async fn fetch(
        &self,
        build: &str,
        repo_url: &str,
    ) -> Result<zeroize::Zeroizing<String>, String> {
        let (repository, key) = match self {
            Credentials::None => {
                return Err("this build plane takes no deploy-key sources ([postbox] unset)".into())
            }
            Credentials::Http { url, token, client } => {
                let r = client
                    .get(format!(
                        "{url}/internal/v1/marketplace/build-requests/{build}/source-credential"
                    ))
                    .bearer_auth(token.as_str())
                    .send()
                    .await
                    .map_err(|_| "the source credential could not be fetched".to_string())?;
                if !r.status().is_success() {
                    return Err(format!(
                        "the postbox refused the source credential ({})",
                        r.status().as_u16()
                    ));
                }
                let mut v: Value = r
                    .json()
                    .await
                    .map_err(|_| "the source credential was malformed".to_string())?;
                let key = zeroize::Zeroizing::new(
                    v["data"]["private_key"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                );
                if let Some(k) = v["data"].get_mut("private_key") {
                    *k = Value::Null;
                }
                (
                    v["data"]["repository"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    key,
                )
            }
            #[cfg(test)]
            Credentials::Fixed(repo, key, seen) => {
                seen.lock().unwrap().push(build.to_string());
                (repo.clone(), zeroize::Zeroizing::new(key.clone()))
            }
        };
        if repository != repo_url {
            return Err("the source credential is for another repository".into());
        }
        if !key.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----") {
            return Err("the source credential is not an OpenSSH private key".into());
        }
        Ok(key)
    }
}

type ApiErr = (StatusCode, &'static str, String);

fn db_err(e: rusqlite::Error) -> ApiErr {
    (StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string())
}

impl Buildd {
    pub fn open(
        cfg: BuilddConfig,
        podman: Arc<dyn Podman>,
        submit_sha256: String,
        artifact_sha256: String,
        review: Option<(ReviewRouteConfig, String)>,
    ) -> Result<Self, String> {
        let conn = if cfg.db.as_os_str() == ":memory:" {
            Connection::open_in_memory()
        } else {
            Connection::open(&cfg.db)
        }
        .map_err(|e| e.to_string())?;
        conn.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
        // A build that was running when the service stopped did not finish.
        conn.execute(
            "UPDATE builds SET state = 'failed', error = 'interrupted', updated_at = ?1
              WHERE state = 'running'",
            params![now()],
        )
        .map_err(|e| e.to_string())?;
        // A scan that was running is run again (its files are still in the work root).
        conn.execute(
            "UPDATE scans SET state = 'queued', updated_at = ?1 WHERE state = 'running'",
            params![now()],
        )
        .map_err(|e| e.to_string())?;
        for h in [&submit_sha256, &artifact_sha256] {
            if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("token files must hold SHA-256 hex digests".into());
            }
        }
        if ct_eq(
            &submit_sha256.to_lowercase(),
            &artifact_sha256.to_lowercase(),
        ) {
            return Err("the submit and artifact tokens must differ".into());
        }
        Ok(Buildd {
            cfg,
            podman,
            conn: Mutex::new(conn),
            submit_sha256: submit_sha256.to_lowercase(),
            artifact_sha256: artifact_sha256.to_lowercase(),
            review,
            credentials: Credentials::None,
        })
    }

    /// Fetch deploy keys from the postbox's credential route.
    pub fn with_credentials(mut self, credentials: Credentials) -> Self {
        self.credentials = credentials;
        self
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, ApiErr> {
        let c = self.conn.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "db lock".to_string(),
            )
        })?;
        f(&c).map_err(db_err)
    }

    /// Queue a build. The same `build_id` again is the same build.
    pub fn submit(&self, req: &BuildReq) -> Result<Value, ApiErr> {
        let bad = |m: String| (StatusCode::BAD_REQUEST, "invalid", m);
        if !valid_build_id(&req.build_id) || !valid_ref(&req.publisher_id) {
            return Err(bad("malformed build_id or publisher_id".into()));
        }
        if req
            .credential
            .as_deref()
            .is_some_and(|c| !c.starts_with("bld_") || !valid_ref(c))
        {
            return Err(bad("credential: a build request id".into()));
        }
        source_ok_with(
            &self.cfg.git,
            &req.repo_url,
            &req.commit,
            req.credential.is_some(),
        )
        .map_err(bad)?;
        if let Some(v) = self.read(&req.build_id)? {
            if v["repo_url"] != req.repo_url.as_str() || v["commit"] != req.commit.as_str() {
                return Err((
                    StatusCode::CONFLICT,
                    "build_conflict",
                    "this build id names another source".into(),
                ));
            }
            return Ok(v);
        }
        let queued: i64 = self.with(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM builds WHERE state IN ('queued','running')",
                [],
                |r| r.get(0),
            )
        })?;
        if queued >= self.cfg.max_queued {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                "queue_full",
                "the build queue is full; try again later".into(),
            ));
        }
        let t = now();
        self.with(|c| {
            c.execute(
                "INSERT INTO builds (id, publisher_id, repo_url, commit_sha, state, created_at,
                    updated_at, credential)
                 VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?5, ?6)",
                params![
                    req.build_id,
                    req.publisher_id,
                    req.repo_url,
                    req.commit,
                    t,
                    req.credential
                ],
            )
        })?;
        Ok(self.read(&req.build_id)?.unwrap_or(Value::Null))
    }

    /// A build as the postbox sees it.
    pub fn read(&self, id: &str) -> Result<Option<Value>, ApiErr> {
        self.with(|c| {
            c.query_row(
                "SELECT id, publisher_id, repo_url, commit_sha, state, error, result FROM builds WHERE id = ?1",
                params![id],
                |r| {
                    let result: Option<String> = r.get(6)?;
                    let mut v = json!({
                        "build_id": r.get::<_, String>(0)?,
                        "publisher_id": r.get::<_, String>(1)?,
                        "repo_url": r.get::<_, String>(2)?,
                        "commit": r.get::<_, String>(3)?,
                        "state": r.get::<_, String>(4)?,
                        "error": r.get::<_, Option<String>>(5)?,
                    });
                    if let Some(res) = result.and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
                        for k in ["record", "stages", "reproducibility", "artifact"] {
                            v[k] = res[k].clone();
                        }
                    }
                    Ok(v)
                },
            )
            .optional()
        })
    }

    /// Claim the oldest queued build: `(id, repo_url, commit, credential)`.
    #[allow(clippy::type_complexity)]
    fn claim(&self) -> Result<Option<(String, String, String, Option<String>)>, ApiErr> {
        self.with(|c| {
            let row: Option<(String, String, String, Option<String>)> = c
                .query_row(
                    "SELECT id, repo_url, commit_sha, credential FROM builds WHERE state = 'queued'
                      ORDER BY created_at, id LIMIT 1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            if let Some((id, _, _, _)) = &row {
                c.execute(
                    "UPDATE builds SET state = 'running', updated_at = ?2 WHERE id = ?1",
                    params![id, now()],
                )?;
            }
            Ok(row)
        })
    }

    fn finish(&self, id: &str, outcome: Result<(Built, Vec<Value>), String>) -> Result<(), ApiErr> {
        let t = now();
        match outcome {
            Ok((b, stages)) => {
                let result = json!({
                    "record": b.record,
                    "stages": stages,
                    "reproducibility": b.reproducibility,
                    "artifact": {
                        "image_digest": b.record.image_digest,
                        "sha256": format!("sha256:{}", b.archive_sha256),
                        "size": b.archive_size,
                    },
                });
                self.with(|c| {
                    c.execute(
                        "UPDATE builds SET state = 'succeeded', result = ?2, image_digest = ?3,
                            archive = ?4, archive_sha256 = ?5, archive_size = ?6, updated_at = ?7
                          WHERE id = ?1",
                        params![
                            id,
                            result.to_string(),
                            b.record.image_digest,
                            b.archive.display().to_string(),
                            b.archive_sha256,
                            b.archive_size as i64,
                            t
                        ],
                    )
                })?;
            }
            Err(e) => {
                let e: String = e.chars().take(2000).collect();
                self.with(|c| {
                    c.execute(
                        "UPDATE builds SET state = 'failed', error = ?2, updated_at = ?3 WHERE id = ?1",
                        params![id, e, t],
                    )
                })?;
            }
        }
        Ok(())
    }

    fn has_queued_build(&self) -> bool {
        self.with(|c| {
            c.query_row(
                "SELECT EXISTS (SELECT 1 FROM builds WHERE state = 'queued')",
                [],
                |r| r.get(0),
            )
        })
        .unwrap_or(false)
    }

    /// Run the oldest queued build to its end. `false` when there was none.
    pub async fn run_one(self: &Arc<Self>) -> bool {
        let Ok(Some((id, url, commit, credential))) = self.claim() else {
            return false;
        };
        let work = self.cfg.work_root.join(&id);
        let key = match &credential {
            None => Ok(None),
            Some(bld) => self.credentials.fetch(bld, &url).await.map(Some),
        };
        let built = match key {
            Err(e) => Err(e),
            Ok(key) => {
                let me = self.clone();
                let w = work.clone();
                tokio::task::spawn_blocking(move || {
                    let _ = std::fs::remove_dir_all(&w);
                    std::fs::create_dir_all(&w).map_err(|e| e.to_string())?;
                    build_from_source_with(
                        me.podman.as_ref(),
                        &me.cfg,
                        &url,
                        &commit,
                        &w,
                        key.as_deref().map(|k| k.as_str()),
                    )
                })
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r)
            }
        };
        let outcome = match built {
            Err(e) => Err(e),
            Ok(b) => {
                let mut stages = vec![stage_body(&b.scan)];
                if let Some((route, key)) = &self.review {
                    let model = analysis::HttpReviewModel {
                        url: route.url.clone(),
                        key: key.clone(),
                        client: reqwest::Client::builder()
                            .timeout(std::time::Duration::from_secs(300))
                            .redirect(reqwest::redirect::Policy::none())
                            .build()
                            .expect("http client"),
                    };
                    match analysis::ai_review(&model, route, &work.join("src")).await {
                        Ok(r) => stages.push(stage_body(&r)),
                        Err(e) => tracing::warn!(build = %id, error = %e, "AI review did not run"),
                    }
                }
                // The contract's order: scan, AI review, behaviour test. Without an AI review the
                // postbox holds the behaviour stage until one is recorded.
                if let Some(r) = &b.behaviour {
                    stages.push(stage_body(r));
                }
                Ok((b, stages))
            }
        };
        if let Err(e) = &outcome {
            tracing::warn!(build = %id, error = %e, "build failed");
        } else {
            tracing::info!(build = %id, "build succeeded");
        }
        let _ = self.finish(&id, outcome);
        let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(work)).await;
        true
    }

    fn scan_dir(&self, id: &str) -> PathBuf {
        self.cfg.work_root.join("scans").join(id)
    }

    /// Queue a scan. The same `scan_id` again is the same scan.
    pub fn submit_scan(&self, req: &ScanReq) -> Result<Value, ApiErr> {
        let bad = |m: &str| (StatusCode::BAD_REQUEST, "invalid", m.to_string());
        if !valid_scan_id(&req.scan_id) {
            return Err(bad("malformed scan_id"));
        }
        if let Some(v) = self.read_scan(&req.scan_id)? {
            return Ok(v);
        }
        let stored = match req.kind.as_str() {
            "files" => {
                if req.files.is_empty() || req.files.len() > SCAN_MAX_FILES {
                    return Err(bad("files: 1 to 5"));
                }
                let dir = self.scan_dir(&req.scan_id).join("files");
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir)
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string()))?;
                let mut names = Vec::new();
                for (i, f) in req.files.iter().enumerate() {
                    let bytes = crate::util::base64_decode(&f.content_b64)
                        .filter(|b| !b.is_empty() && b.len() <= SCAN_MAX_FILE_BYTES)
                        .ok_or_else(|| bad("files[].content_b64: base64, 1 byte to 20 MiB"))?;
                    // Stored under an index: the publisher's file name never reaches a path.
                    std::fs::write(dir.join(format!("{i}.bin")), &bytes).map_err(|e| {
                        (StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string())
                    })?;
                    names.push(json!({
                        "index": i,
                        "name": f.name.chars().take(200).collect::<String>(),
                        "media_type": f.media_type.chars().take(100).collect::<String>(),
                        "bytes": bytes.len(),
                        "sha256": format!("sha256:{}", sha256_hex(&bytes)),
                    }));
                }
                json!({ "kind": "files", "files": names })
            }
            "release" => {
                let release = req
                    .release_id
                    .as_deref()
                    .filter(|r| valid_ref(r))
                    .ok_or_else(|| bad("release_id"))?;
                let digest = req
                    .image_digest
                    .as_deref()
                    .filter(|d| digest_ok(d))
                    .ok_or_else(|| bad("image_digest: sha256:<64 hex>"))?;
                let sbom = req
                    .sbom
                    .as_deref()
                    .filter(|t| t.len() <= SCAN_MAX_SBOM_BYTES)
                    .filter(|t| serde_json::from_str::<Value>(t).is_ok())
                    .ok_or_else(|| bad("sbom: the release's CycloneDX JSON"))?;
                self.with(|c| {
                    c.execute(
                        "INSERT INTO retained_images (image_digest, release_id, updated_at)
                         VALUES (?1, ?2, ?3)
                         ON CONFLICT (image_digest) DO UPDATE SET updated_at = excluded.updated_at",
                        params![digest, release, now()],
                    )?;
                    c.execute(
                        "INSERT OR IGNORE INTO rescanned_images (image_digest, first_at)
                         VALUES (?1, ?2)",
                        params![digest, now()],
                    )
                })?;
                json!({ "kind": "release", "release_id": release, "image_digest": digest,
                        "sbom": sbom })
            }
            _ => return Err(bad("kind: files or release")),
        };
        let t = now();
        self.with(|c| {
            c.execute(
                "INSERT INTO scans (id, kind, state, request, created_at, updated_at)
                 VALUES (?1, ?2, 'queued', ?3, ?4, ?4)",
                params![req.scan_id, req.kind, stored.to_string(), t],
            )
        })?;
        Ok(self.read_scan(&req.scan_id)?.unwrap_or(Value::Null))
    }

    /// A scan as the postbox sees it: its state and, when done, its findings.
    pub fn read_scan(&self, id: &str) -> Result<Option<Value>, ApiErr> {
        self.with(|c| {
            c.query_row(
                "SELECT id, kind, state, error, result FROM scans WHERE id = ?1",
                params![id],
                |r| {
                    let result: Option<String> = r.get(4)?;
                    Ok(json!({
                        "scan_id": r.get::<_, String>(0)?,
                        "kind": r.get::<_, String>(1)?,
                        "state": r.get::<_, String>(2)?,
                        "error": r.get::<_, Option<String>>(3)?,
                        "result": result.and_then(|t| serde_json::from_str::<Value>(&t).ok()),
                    }))
                },
            )
            .optional()
        })
    }

    /// Run the oldest queued scan. `false` when there was none.
    pub async fn run_one_scan(self: &Arc<Self>) -> bool {
        let claimed: Result<Option<(String, String)>, ApiErr> = self.with(|c| {
            let row: Option<(String, String)> = c
                .query_row(
                    "SELECT id, request FROM scans WHERE state = 'queued'
                      ORDER BY created_at, id LIMIT 1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((id, _)) = &row {
                c.execute(
                    "UPDATE scans SET state = 'running', updated_at = ?2 WHERE id = ?1",
                    params![id, now()],
                )?;
            }
            Ok(row)
        });
        let Ok(Some((id, request))) = claimed else {
            return false;
        };
        let me = self.clone();
        let sid = id.clone();
        let outcome = tokio::task::spawn_blocking(move || me.scan_now(&sid, &request))
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r);
        let t = now();
        let _ = self.with(|c| match &outcome {
            Ok(v) => c.execute(
                "UPDATE scans SET state = 'succeeded', result = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, v.to_string(), t],
            ),
            Err(e) => c.execute(
                "UPDATE scans SET state = 'failed', error = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, e.chars().take(500).collect::<String>(), t],
            ),
        });
        let files = self.scan_dir(&id).join("files");
        let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(files)).await;
        true
    }

    /// One scan, blocking: the tools of [`scanners::security_scan`] for what the request names.
    fn scan_now(&self, id: &str, request: &str) -> Result<Value, String> {
        let sc = self
            .cfg
            .scanners
            .as_ref()
            .ok_or("this build plane has no scanners configured")?;
        let req: Value = serde_json::from_str(request).map_err(|e| e.to_string())?;
        let dir = self.scan_dir(id);
        let files = dir.join("files");
        let out = dir.join("out");
        let outcome = match req["kind"].as_str() {
            Some("files") => scanners::security_scan(
                self.podman.as_ref(),
                sc,
                &self.cfg.build.runtime,
                &Target {
                    files: Some(&files),
                    ..Target::default()
                },
                &out,
                now(),
            ),
            Some("release") => {
                let digest = req["image_digest"].as_str().unwrap_or_default().to_string();
                let archive: Option<String> = self
                    .with(|c| {
                        c.query_row(
                            "SELECT archive FROM builds WHERE image_digest = ?1
                               AND state = 'succeeded' AND archive IS NOT NULL
                             ORDER BY updated_at DESC LIMIT 1",
                            params![digest],
                            |r| r.get(0),
                        )
                        .optional()
                    })
                    .map_err(|e| e.2)?;
                let archive = archive.map(PathBuf::from).filter(|p| p.is_file());
                let mut o = scanners::security_scan(
                    self.podman.as_ref(),
                    sc,
                    &self.cfg.build.runtime,
                    &Target {
                        image_archive: archive.as_deref(),
                        sbom: req["sbom"].as_str(),
                        ..Target::default()
                    },
                    &out,
                    now(),
                );
                o.evidence["image_scanned"] = json!(archive.is_some());
                if archive.is_none() {
                    // The image is gone: only the SBOM was scanned. Never a quiet pass; unique to
                    // this run, so every re-scan without it says so again.
                    o.findings.push(Finding::of(
                        "pigeonpost",
                        "scanner.image_unavailable",
                        analysis::Severity::High,
                        &format!("image_unavailable:{}", crate::util::random_hex(6)),
                        "the release image is not on the build plane; only its SBOM was \
                         re-scanned (no image CVE, ClamAV or YARA scan)",
                        true,
                    ));
                }
                o
            }
            _ => return Err("unknown scan kind".into()),
        };
        let _ = std::fs::remove_dir_all(&dir);
        let blocking = outcome.findings.iter().filter(|f| f.blocking).count();
        Ok(json!({
            "findings": outcome.findings,
            "blocking": blocking,
            "evidence": outcome.evidence,
        }))
    }

    /// Delete saved images older than `keep_days` once a re-scan has named them (or older than
    /// `keep_unscanned_days` when none ever has), unless a published release still names them;
    /// retained images no re-scan has named for two months; and finished scans after 30 days.
    pub fn prune(&self) -> Result<usize, ApiErr> {
        let t = now();
        self.with(|c| {
            c.execute(
                "DELETE FROM retained_images WHERE updated_at < ?1",
                params![t - RETAIN_SECS],
            )?;
            c.execute(
                "DELETE FROM scans WHERE state IN ('succeeded','failed') AND updated_at < ?1",
                params![t - SCAN_KEEP_SECS],
            )
        })?;
        let cutoff = t - self.cfg.keep_days * 86_400;
        let unscanned_cutoff = t - self.cfg.keep_unscanned_days * 86_400;
        let old: Vec<(String, String)> = self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id, archive FROM builds WHERE archive IS NOT NULL AND updated_at < ?1
                   AND image_digest NOT IN (SELECT image_digest FROM retained_images)
                   AND (updated_at < ?2
                        OR image_digest IN (SELECT image_digest FROM rescanned_images))",
            )?;
            let rows = stmt.query_map(params![cutoff, unscanned_cutoff], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
            rows.collect()
        })?;
        for (id, archive) in &old {
            let _ = std::fs::remove_file(archive);
            self.with(|c| {
                c.execute(
                    "UPDATE builds SET archive = NULL WHERE id = ?1",
                    params![id],
                )
            })?;
        }
        Ok(old.len())
    }

    /// One chunk of a saved image: `(bytes, total size, sha256 hex)`.
    pub fn artifact_chunk(
        &self,
        digest: &str,
        offset: u64,
    ) -> Result<(Vec<u8>, u64, String), ApiErr> {
        let not_found = || {
            (
                StatusCode::NOT_FOUND,
                "not_found",
                "no such artifact".to_string(),
            )
        };
        if !digest_ok(digest) {
            return Err(not_found());
        }
        let row: Option<(String, String, i64)> = self.with(|c| {
            c.query_row(
                "SELECT archive, archive_sha256, archive_size FROM builds
                  WHERE image_digest = ?1 AND state = 'succeeded' AND archive IS NOT NULL
                  ORDER BY updated_at DESC LIMIT 1",
                params![digest],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
        })?;
        let (archive, sha, size) = row.ok_or_else(not_found)?;
        let size = size as u64;
        let mut f = std::fs::File::open(&archive).map_err(|_| not_found())?;
        let start = offset.min(size);
        f.seek(std::io::SeekFrom::Start(start))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string()))?;
        let mut buf = Vec::new();
        (&mut f)
            .take(CHUNK_BYTES.min(size - start))
            .read_to_end(&mut buf)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string()))?;
        Ok((buf, size, sha))
    }
}

// ---- HTTP ---------------------------------------------------------------------------------------

fn bearer_ok(headers: &HeaderMap, sha256: &str) -> bool {
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    !presented.is_empty() && ct_eq(&sha256_hex(presented.as_bytes()), sha256)
}

fn error(e: ApiErr) -> Response {
    (e.0, Json(json!({ "error": e.1, "detail": e.2 }))).into_response()
}

fn unauthorized() -> Response {
    error((
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "token required".into(),
    ))
}

async fn http_submit(
    State(b): State<Arc<Buildd>>,
    headers: HeaderMap,
    Json(req): Json<BuildReq>,
) -> Response {
    if !bearer_ok(&headers, &b.submit_sha256) {
        return unauthorized();
    }
    let d = b.clone();
    match tokio::task::spawn_blocking(move || d.submit(&req)).await {
        Ok(Ok(v)) => (StatusCode::ACCEPTED, Json(v)).into_response(),
        Ok(Err(e)) => error(e),
        Err(e) => error((StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string())),
    }
}

async fn http_submit_scan(
    State(b): State<Arc<Buildd>>,
    headers: HeaderMap,
    Json(req): Json<ScanReq>,
) -> Response {
    if !bearer_ok(&headers, &b.submit_sha256) {
        return unauthorized();
    }
    let d = b.clone();
    match tokio::task::spawn_blocking(move || d.submit_scan(&req)).await {
        Ok(Ok(v)) => (StatusCode::ACCEPTED, Json(v)).into_response(),
        Ok(Err(e)) => error(e),
        Err(e) => error((StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string())),
    }
}

async fn http_read_scan(
    State(b): State<Arc<Buildd>>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
) -> Response {
    if !bearer_ok(&headers, &b.submit_sha256) {
        return unauthorized();
    }
    let not_found = || error((StatusCode::NOT_FOUND, "not_found", "no such scan".into()));
    if !valid_scan_id(&id) {
        return not_found();
    }
    let d = b.clone();
    match tokio::task::spawn_blocking(move || d.read_scan(&id)).await {
        Ok(Ok(Some(v))) => (StatusCode::OK, Json(v)).into_response(),
        Ok(Ok(None)) => not_found(),
        Ok(Err(e)) => error(e),
        Err(e) => error((StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string())),
    }
}

async fn http_read(
    State(b): State<Arc<Buildd>>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
) -> Response {
    if !bearer_ok(&headers, &b.submit_sha256) {
        return unauthorized();
    }
    if !valid_build_id(&id) {
        return error((StatusCode::NOT_FOUND, "not_found", "no such build".into()));
    }
    let d = b.clone();
    match tokio::task::spawn_blocking(move || d.read(&id)).await {
        Ok(Ok(Some(v))) => (StatusCode::OK, Json(v)).into_response(),
        Ok(Ok(None)) => error((StatusCode::NOT_FOUND, "not_found", "no such build".into())),
        Ok(Err(e)) => error(e),
        Err(e) => error((StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string())),
    }
}

#[derive(Deserialize)]
struct OffsetQ {
    #[serde(default)]
    offset: u64,
}

async fn http_artifact(
    State(b): State<Arc<Buildd>>,
    headers: HeaderMap,
    UrlPath(digest): UrlPath<String>,
    Query(q): Query<OffsetQ>,
) -> Response {
    if !bearer_ok(&headers, &b.artifact_sha256) {
        return unauthorized();
    }
    let d = b.clone();
    match tokio::task::spawn_blocking(move || d.artifact_chunk(&digest, q.offset)).await {
        Ok(Ok((bytes, size, sha))) => {
            let mut r = (StatusCode::OK, bytes).into_response();
            let h = r.headers_mut();
            h.insert(
                axum::http::header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            );
            if let Ok(v) = HeaderValue::from_str(&size.to_string()) {
                h.insert(SIZE_HEADER, v);
            }
            if let Ok(v) = HeaderValue::from_str(&format!("sha256:{sha}")) {
                h.insert(SHA256_HEADER, v);
            }
            r
        }
        Ok(Err(e)) => error(e),
        Err(e) => error((StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string())),
    }
}

pub fn router(b: Arc<Buildd>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/builds", post(http_submit))
        .route("/v1/builds/{id}", get(http_read))
        .route("/v1/artifacts/{digest}", get(http_artifact))
        .route("/v1/scans/{id}", get(http_read_scan))
        .layer(axum::extract::DefaultBodyLimit::max(16 * 1024))
        // Five result files of 20 MiB in base64, or one SBOM.
        .route(
            "/v1/scans",
            post(http_submit_scan).layer(axum::extract::DefaultBodyLimit::max(
                SCAN_MAX_FILES * SCAN_MAX_FILE_BYTES * 4 / 3 + 1024 * 1024,
            )),
        )
        .with_state(b)
}

/// The queue loop: run builds one at a time, prune old images daily.
pub fn start_worker(b: Arc<Buildd>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(3));
        let mut last_prune = 0i64;
        loop {
            tick.tick().await;
            // Builds first; a scan runs when no build waits.
            while b.run_one().await {}
            while !b.has_queued_build() && b.run_one_scan().await {}
            if now() - last_prune > 86_400 {
                last_prune = now();
                let p = b.clone();
                let _ = tokio::task::spawn_blocking(move || p.prune()).await;
            }
        }
    });
}

#[cfg(test)]
pub(crate) fn test_config(dir: &Path) -> BuilddConfig {
    BuilddConfig {
        bind: "127.0.0.1:0".into(),
        db: ":memory:".into(),
        work_root: dir.join("work"),
        artifact_root: dir.join("artifacts"),
        submit_token_sha256_file: dir.join("submit.sha256"),
        artifact_token_sha256_file: dir.join("artifact.sha256"),
        git: GitConfig {
            image: format!("localhost/git@sha256:{}", "c".repeat(64)),
            network: "buildd-git".into(),
            allowed_hosts: vec!["git.example.org".into()],
            known_hosts: None,
        },
        build: BuildConfig {
            runtime: "runsc".into(),
            fetch_network: "buildd-fetch".into(),
            fetch_image: format!("localhost/rust@sha256:{}", "d".repeat(64)),
            image_name: "localhost/pigeonpost-release".into(),
            node_fetch_image: Some(format!("localhost/node@sha256:{}", "e".repeat(64))),
            python_fetch_image: Some(format!("localhost/python@sha256:{}", "f".repeat(64))),
            memory_mib: Some(2048),
            cpu_millis: Some(2000),
            pids: Some(512),
            // The toolchain the reference release's nest-build.toml names.
            toolchain_images: vec!["docker.io/library/rust:1.91.1-slim-bookworm@sha256:8514999d4786ef12efe89239e86b3d0a021b94b9d35108c8efe6c79ca7dc1a65".into()],
            compile_timeout_secs: 600,
        },
        reproducible: true,
        max_queued: 2,
        keep_days: 14,
        keep_unscanned_days: 120,
        ai_review_route: None,
        behaviour_test: true,
        behaviour_timeout_secs: 30,
        postbox: None,
        scanners: None,
        behaviour: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    /// How the fetch and git steps name their program (`--entrypoint`, never an argument).
    const NPM: &str = "--entrypoint=[\"npm\"]";
    const GIT: &str = "--entrypoint=[\"git\"]";

    #[test]
    fn source_intake_takes_allowlisted_https_and_full_commits_only() {
        let g = test_config(Path::new("/tmp")).git;
        assert!(source_ok(&g, "https://git.example.org/acme/hr-bot.git", COMMIT).is_ok());
        for (url, commit) in [
            ("http://git.example.org/acme/x", COMMIT),
            ("https://evil.example.com/acme/x", COMMIT),
            ("https://git.example.org.evil.com/acme/x", COMMIT),
            ("https://user@git.example.org/acme/x", COMMIT),
            ("https://git.example.org:8443/acme/x", COMMIT),
            ("https://git.example.org/acme/../x", COMMIT),
            ("https://git.example.org/acme/x;rm", COMMIT),
            ("https://git.example.org/", COMMIT),
            ("ssh://git.example.org/acme/x", COMMIT),
            ("https://git.example.org/acme/x", "main"),
            ("https://git.example.org/acme/x", "0123456"),
            ("https://git.example.org/acme/x", &COMMIT.to_uppercase()),
        ] {
            assert!(source_ok(&g, url, commit).is_err(), "{url} {commit}");
        }
    }

    #[test]
    fn git_runs_contained_on_its_own_network() {
        let cfg = test_config(Path::new("/tmp"));
        let a = git_args(&cfg, Path::new("/w/src"), &["rev-parse", "HEAD"]);
        for must in [
            "--network=buildd-git",
            "--runtime=runsc",
            "--cap-drop=ALL",
            "--read-only",
            "--security-opt=no-new-privileges",
            "--memory=2048m",
            "--pids-limit=512",
            "--pull=never",
        ] {
            assert!(a.contains(&must.to_string()), "missing {must}: {a:?}");
        }
        assert!(a.iter().any(|x| x == "--volume=/w/src:/src:rw"));
        assert!(!a
            .iter()
            .any(|x| x.contains("privileged") || x == "--network=host"));
        // git is the entrypoint, never an argument after an image whose own entrypoint is git.
        let image = a.iter().position(|x| *x == cfg.git.image).unwrap();
        assert_eq!(a[image - 1], "--entrypoint=[\"git\"]");
        assert_eq!(&a[image + 1..], ["rev-parse", "HEAD"]);
        assert!(!a.iter().any(|x| x == "git"));
    }

    // Linux-only daemon: the checks require absolute Unix paths (`/tmp`, `/etc/...`).
    #[cfg(unix)]
    #[test]
    fn the_config_refuses_loose_settings() {
        let ok = || test_config(Path::new("/tmp"));
        assert!(ok().check().is_ok());
        type Tweak = fn(&mut BuilddConfig);
        let cases: Vec<(&str, Tweak)> = vec![
            ("wildcard bind", |c| c.bind = "0.0.0.0:8795".into()),
            ("unpinned git image", |c| {
                c.git.image = "alpine/git:latest".into()
            }),
            ("host network", |c| c.git.network = "host".into()),
            ("no hosts", |c| c.git.allowed_hosts.clear()),
            ("a pattern host", |c| {
                c.git.allowed_hosts = vec!["*.example.org".into()]
            }),
            ("relative work", |c| c.work_root = "work".into()),
            ("runc", |c| c.build.runtime = "runc".into()),
        ];
        for (what, tweak) in cases {
            let mut c = ok();
            tweak(&mut c);
            assert!(c.check().is_err(), "{what} must be refused");
        }
    }

    /// A podman that is a git host, a build host and an image store at once: git checks out the
    /// reference release, `build` succeeds, `save` writes an OCI archive whose index names
    /// `MANIFEST`.
    #[derive(Default)]
    struct FakeBuildPodman {
        calls: Mutex<Vec<Vec<String>>>,
        /// The tree git checks out; the reference release when unset.
        source: Option<PathBuf>,
        /// The behaviour run: the release copies a planted credential into its results.
        leak: bool,
        /// The behaviour run: the release answers nothing.
        silent: bool,
        /// What the deploy-key file held while git ran (the fetch's only reader).
        key_seen: Mutex<Option<String>>,
        /// The scanners: ClamAV reports a hit.
        malware: bool,
        /// The observed behaviour run: the gVisor trace it leaves under this root.
        trace: Option<(PathBuf, String)>,
        /// The behaviour run prints its jobs' input on stdout, then is ended by its timeout.
        log_input: bool,
        /// The behaviour run burns CPU after answering: this cgroup's cpu.stat jumps.
        burn: Option<PathBuf>,
        /// The behaviour run leaves only partial result files.
        partial: bool,
    }

    /// What the fake `pip download` writes for `name==version`; tests pin its hash.
    fn fake_wheel(name: &str, version: &str) -> Vec<u8> {
        format!("synthetic wheel {name} {version}").into_bytes()
    }

    const MANIFEST: &str =
        "sha256:5555555555555555555555555555555555555555555555555555555555555555";

    pub(crate) fn tar_entry(name: &str, body: &[u8]) -> Vec<u8> {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[124..136].copy_from_slice(format!("{:011o}\0", body.len()).as_bytes());
        h[156] = b'0';
        let mut out = h.to_vec();
        out.extend_from_slice(body);
        out.resize(out.len().div_ceil(512) * 512, 0);
        out
    }

    impl Podman for FakeBuildPodman {
        fn run_capture(&self, args: &[&str]) -> (bool, String, String) {
            let behaviour = args.iter().any(|a| a.ends_with(":/nest:rw"));
            match self.run(args) {
                Ok(out) if behaviour && self.log_input => {
                    let nest = args
                        .iter()
                        .find_map(|a| a.strip_prefix("--volume=")?.strip_suffix(":/nest:rw"))
                        .map(PathBuf::from)
                        .unwrap();
                    let mut dump = out;
                    for e in std::fs::read_dir(nest.join("jobs/in")).unwrap().flatten() {
                        // Logged base64-wrapped, as a careless release might.
                        let text = std::fs::read_to_string(e.path()).unwrap();
                        let enc = crate::analysis::encodings(&text)
                            .into_iter()
                            .nth(6)
                            .unwrap_or_default();
                        let wrapped: Vec<String> = enc
                            .as_bytes()
                            .chunks(40)
                            .map(|c| String::from_utf8_lossy(c).into_owned())
                            .collect();
                        dump.push_str(&format!("received:\n{}\n", wrapped.join("\n")));
                    }
                    // The timeout ends the worker: a failure, with both streams.
                    (false, dump, "timed out".into())
                }
                Ok(out) => (true, out, String::new()),
                Err(e) => (false, String::new(), e),
            }
        }

        fn run(&self, args: &[&str]) -> Result<String, String> {
            let argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
            self.calls.lock().unwrap().push(argv.clone());
            let volume = |suffix: &str| {
                argv.iter()
                    .find_map(|a| a.strip_prefix("--volume=")?.strip_suffix(suffix))
                    .map(PathBuf::from)
            };
            match args.first().copied() {
                Some("info") => Ok("true".into()),
                Some("image") => Ok(format!("sha256:{}\n", "ab".repeat(32))),
                Some("save") => {
                    let out = &argv[argv.iter().position(|a| a == "--output").unwrap() + 1];
                    let mut tar = tar_entry("oci-layout", b"{}");
                    tar.extend(tar_entry(
                        "index.json",
                        format!("{{\"manifests\":[{{\"digest\":\"{MANIFEST}\"}}]}}").as_bytes(),
                    ));
                    tar.extend(vec![7u8; 3 * 512]);
                    tar.extend([0u8; 1024]);
                    std::fs::write(out, tar).map_err(|e| e.to_string())?;
                    Ok(String::new())
                }
                // A security tool: an empty report, or a ClamAV hit.
                Some("run") if volume(":/out:rw").is_some() => {
                    let out = volume(":/out:rw").unwrap();
                    let tool = argv
                        .iter()
                        .find_map(|a| a.strip_prefix("--entrypoint="))
                        .unwrap_or_default();
                    let report = match tool {
                        "osv-scanner" => ("report.json", "{\"results\":[]}"),
                        "grype" => ("report.json", "{\"matches\":[]}"),
                        "semgrep" => ("report.json", "{\"results\":[],\"errors\":[]}"),
                        "gitleaks" => ("report.json", "[]"),
                        "clamscan" if self.malware => {
                            std::fs::write(
                                out.join("report.log"),
                                "/scan/artefacts/target/release/hr-policy-qa: Unix.Trojan.Agent-1 FOUND\n",
                            )
                            .unwrap();
                            return Err("clamscan: exit status 1".into());
                        }
                        "clamscan" => ("report.log", ""),
                        _ => return Ok(String::new()),
                    };
                    std::fs::write(out.join(report.0), report.1).unwrap();
                    Ok(String::new())
                }
                // The behaviour run: the release answers its jobs (or leaks, or stays silent).
                Some("run") if volume(":/nest:rw").is_some() => {
                    let nest = volume(":/nest:rw").unwrap();
                    if let Some(cid) = argv.iter().find_map(|a| a.strip_prefix("--cidfile=")) {
                        let id = "0123456789abcdef0123456789abcdef";
                        std::fs::write(cid, id).unwrap();
                        if let Some((root, text)) = &self.trace {
                            std::fs::create_dir_all(root.join(id)).unwrap();
                            std::fs::write(root.join(id).join("runsc.log.boot.txt"), text).unwrap();
                        }
                    }
                    if self.silent {
                        return Err("timed out".into());
                    }
                    if self.partial {
                        std::fs::write(nest.join("jobs/out/job_behaviour00.json"), "{\"sta")
                            .unwrap();
                        return Err("timed out".into());
                    }
                    let secret = std::fs::read_to_string(nest.join(".config/credentials"))
                        .unwrap_or_default();
                    for entry in std::fs::read_dir(nest.join("jobs/in")).unwrap().flatten() {
                        let job: Value =
                            serde_json::from_str(&std::fs::read_to_string(entry.path()).unwrap())
                                .unwrap();
                        let answer = if self.leak {
                            secret.clone()
                        } else {
                            "ok".into()
                        };
                        std::fs::write(
                            nest.join("jobs/out").join(entry.file_name()),
                            json!({ "job_id": job["job_id"], "status": "succeeded", "answer": answer })
                                .to_string(),
                        )
                        .unwrap();
                    }
                    if let Some(cg) = &self.burn {
                        std::fs::write(cg.join("cpu.stat"), "usage_usec 900000000\n").unwrap();
                    }
                    Ok("worker stopped\n".into())
                }
                // `npm ci`: every locked package lands where the lockfile says.
                Some("run") if argv.iter().any(|a| a == NPM) => {
                    let vendor = volume(":/vendor:rw").unwrap();
                    let lock: Value = serde_json::from_str(
                        &std::fs::read_to_string(vendor.join("package-lock.json"))
                            .map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| e.to_string())?;
                    for (path, p) in lock["packages"].as_object().unwrap() {
                        if path.is_empty() || p["dev"] == true {
                            continue;
                        }
                        let dir = vendor.join(path);
                        std::fs::create_dir_all(&dir).unwrap();
                        std::fs::write(
                            dir.join("package.json"),
                            json!({ "version": p["version"] }).to_string(),
                        )
                        .unwrap();
                        std::fs::write(dir.join("index.js"), "export default 1;\n").unwrap();
                    }
                    Ok(String::new())
                }
                // `pip download`: one wheel per pinned requirement.
                Some("run") if argv.iter().any(|a| a == "pip") => {
                    let src = volume(":/src:ro").unwrap();
                    let vendor = volume(":/vendor:rw").unwrap();
                    let text = std::fs::read_to_string(src.join("requirements.txt")).unwrap();
                    for p in crate::ecosystem::pip_locked(&text)? {
                        std::fs::write(
                            vendor.join(format!("{}-{}-py3-none-any.whl", p.name, p.version)),
                            fake_wheel(&p.name, &p.version),
                        )
                        .unwrap();
                    }
                    Ok(String::new())
                }
                Some("run") if argv.iter().any(|a| a == GIT) => {
                    let src = volume(":/src:rw").unwrap();
                    if let Some(cred) = volume(":/cred:ro") {
                        *self.key_seen.lock().unwrap() =
                            std::fs::read_to_string(cred.join("id")).ok();
                    }
                    if argv.iter().any(|a| a == "checkout") {
                        let root = self
                            .source
                            .clone()
                            .unwrap_or_else(crate::release::reference_source);
                        for f in crate::release::read_tree(&root)? {
                            let dest = src.join(&f.path);
                            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
                            std::fs::copy(root.join(&f.path), dest).unwrap();
                        }
                        std::fs::create_dir_all(src.join(".git/objects")).unwrap();
                    }
                    if argv.iter().any(|a| a == "rev-parse") {
                        return Ok(format!("{COMMIT}\n"));
                    }
                    Ok(String::new())
                }
                // The compile step: what the reference and template commands leave behind.
                Some("run") if volume(":/src:rw").is_some() => {
                    let src = volume(":/src:rw").unwrap();
                    let script = argv.last().cloned().unwrap_or_default();
                    if script.contains("cargo build") {
                        std::fs::create_dir_all(src.join("target/release")).unwrap();
                        std::fs::write(src.join("target/release/hr-policy-qa"), b"\x7fELF")
                            .unwrap();
                    }
                    if script.contains("pip install") {
                        std::fs::create_dir_all(src.join("deps")).unwrap();
                        let _ = std::fs::remove_dir_all(src.join("wheelhouse"));
                    }
                    // The command may rewrite its tree; the assembly must not read this.
                    std::fs::write(src.join("Containerfile"), "FROM x\nRUN evil\n").unwrap();
                    Ok(String::new())
                }
                // The assembly: every COPY source exists in the context, and the Containerfile
                // read is the one from the source tree.
                Some("build") => {
                    let file = argv
                        .iter()
                        .find_map(|a| a.strip_prefix("--file="))
                        .ok_or("no --file")?;
                    let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
                    if text.lines().any(|l| l.trim_start().starts_with("RUN")) {
                        return Err("the assembly read a Containerfile with RUN".into());
                    }
                    let context = PathBuf::from(argv.last().unwrap());
                    for line in text.lines().filter(|l| l.starts_with("COPY ")) {
                        let words: Vec<&str> = line
                            .split_whitespace()
                            .skip(1)
                            .filter(|w| !w.starts_with("--"))
                            .collect();
                        for from in &words[..words.len() - 1] {
                            if !context.join(from).exists() {
                                return Err(format!("COPY {from}: no such file in the context"));
                            }
                        }
                    }
                    Ok(String::new())
                }
                _ => Ok(String::new()),
            }
        }
    }

    /// Every starter template (`pigeonpost nest init`), in both languages, builds through the real
    /// build-plane path: git intake, the language's fetch step, two builds with the same digest,
    /// the artifact, and the deterministic scan. Only the template that declares an external
    /// write is blocked, by that rule alone.
    #[test]
    fn every_template_builds_on_the_build_plane_in_both_languages() {
        use pigeonpost_nest_kit::{render, Lang, TEMPLATES};
        for t in TEMPLATES {
            for (lang, eco) in [(Lang::Node, "node"), (Lang::Python, "python")] {
                let app = tempfile::tempdir().unwrap();
                for (path, body) in render(t, lang, "my-app", "acme") {
                    let dest = app.path().join(&path);
                    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
                    std::fs::write(dest, body).unwrap();
                }
                let dir = tempfile::tempdir().unwrap();
                let cfg = test_config(dir.path());
                let podman = FakeBuildPodman {
                    source: Some(app.path().to_path_buf()),
                    ..Default::default()
                };
                let work = dir.path().join("work");
                let built = build_from_source(
                    &podman,
                    &cfg,
                    "https://git.example.org/acme/my-app.git",
                    COMMIT,
                    &work,
                )
                .unwrap_or_else(|e| panic!("{} {eco}: {e}", t.name));
                assert_eq!(built.record.isolation["ecosystem"], eco, "{}", t.name);
                assert_eq!(built.reproducibility["equal"], true, "{} {eco}", t.name);
                assert_eq!(built.reproducibility["builds"], 2);
                assert_eq!(built.scan.evidence["ecosystem"], eco, "{}", t.name);
                let behaviour = built.behaviour.as_ref().expect("the behaviour test ran");
                assert_eq!(
                    behaviour.decision, "passed",
                    "{} {eco}: {:?}",
                    t.name, behaviour.findings
                );
                let blockers: Vec<&str> = built
                    .scan
                    .findings
                    .iter()
                    .filter(|f| f.blocking)
                    .map(|f| f.rule.as_str())
                    .collect();
                if built.record.manifest.external_writes {
                    assert_eq!(blockers, ["manifest.external_writes"], "{} {eco}", t.name);
                } else {
                    assert!(blockers.is_empty(), "{} {eco}: {blockers:?}", t.name);
                }
                // No dependencies: no fetch step ran, and nothing ran with a network but git.
                let calls = podman.calls.lock().unwrap();
                assert!(!calls
                    .iter()
                    .any(|c| c.iter().any(|a| a == NPM || a == "pip")));
                let builds: Vec<&Vec<String>> = calls.iter().filter(|c| c[0] == "build").collect();
                assert_eq!(builds.len(), 2);
                assert!(builds
                    .iter()
                    .all(|c| c.contains(&"--network=none".to_string())));
                // The behaviour run: the image just built, no network, no capabilities.
                let run = calls
                    .iter()
                    .find(|c| c.iter().any(|a| a.ends_with(":/nest:rw")))
                    .unwrap();
                for must in [
                    "--network=none",
                    "--cap-drop=ALL",
                    "--read-only",
                    "--user=10001:10001",
                ] {
                    assert!(run.contains(&must.to_string()), "{must}: {run:?}");
                }
            }
        }
    }

    /// A Node release with a dependency: `npm ci --ignore-scripts --omit=dev` on the fetch
    /// network, the vendored `node_modules/` in the build context, npm purls in the SBOM, and the
    /// lockfile's published licence held to the manifest's declaration.
    #[test]
    fn a_node_release_vendors_node_modules_from_its_lockfile() {
        use pigeonpost_nest_kit::{render, template, Lang};
        let app = tempfile::tempdir().unwrap();
        for (path, body) in render(template("tool").unwrap(), Lang::Node, "my-app", "acme") {
            let dest = app.path().join(&path);
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            std::fs::write(dest, body).unwrap();
        }
        // sha512 of the empty string, a well-formed integrity.
        let sri = "sha512-z4PhNX7vuL3xVChQ1m2AB9Yg5AULVxXcg/SpIdNs6c5H0NE8XYXysP+DGNKHfuwvY7kxvUdBeoGlODJ6+SfaPg==";
        let lock = json!({
            "name": "nest-app", "lockfileVersion": 3, "requires": true,
            "packages": {
                "": { "name": "nest-app", "dependencies": { "tiny-csv": "^1.2.0" } },
                "node_modules/tiny-csv": {
                    "version": "1.2.0",
                    "resolved": "https://registry.npmjs.org/tiny-csv/-/tiny-csv-1.2.0.tgz",
                    "integrity": sri, "license": "MIT",
                },
                "node_modules/test-only": { "version": "9.0.0", "dev": true },
            },
        });
        std::fs::write(app.path().join("package-lock.json"), lock.to_string()).unwrap();
        let manifest = app.path().join("nest-release.toml");
        let text = std::fs::read_to_string(&manifest).unwrap();
        std::fs::write(
            &manifest,
            text.replace("[licences]\n", "[licences]\ntiny-csv = \"MIT\"\n"),
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_config(dir.path());
        let podman = FakeBuildPodman {
            source: Some(app.path().to_path_buf()),
            ..Default::default()
        };
        let work = dir.path().join("work");
        let built = build_from_source(
            &podman,
            &cfg,
            "https://git.example.org/acme/my-app.git",
            COMMIT,
            &work,
        )
        .unwrap();
        assert_eq!(built.reproducibility["equal"], true);
        assert!(
            built.scan.findings.iter().all(|f| !f.blocking),
            "{:?}",
            built.scan.findings
        );
        let calls = podman.calls.lock().unwrap();
        let npm = calls.iter().find(|c| c.iter().any(|a| a == NPM)).unwrap();
        for must in [
            "--ignore-scripts",
            "--omit=dev",
            "--network=buildd-fetch",
            "--read-only",
        ] {
            assert!(npm.contains(&must.to_string()), "{must}: {npm:?}");
        }
        assert!(npm.contains(&format!("localhost/node@sha256:{}", "e".repeat(64))));
        assert!(work
            .join("first/context/node_modules/tiny-csv/package.json")
            .is_file());
        assert!(!work.join("first/context/node_modules/test-only").exists());
        let bom: Value = serde_json::from_str(&built.record.sbom).unwrap();
        let lib = bom["components"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["type"] == "library")
            .unwrap()
            .clone();
        assert_eq!(lib["purl"], "pkg:npm/tiny-csv@1.2.0");
        assert_eq!(lib["hashes"][0]["alg"], "SHA-512");
        // A published licence the manifest misstates is a blocker.
        std::fs::write(
            &manifest,
            text.replace("[licences]\n", "[licences]\ntiny-csv = \"ISC\"\n"),
        )
        .unwrap();
        let scan = crate::analysis::deterministic_scan(app.path()).unwrap();
        assert!(scan
            .findings
            .iter()
            .any(|f| f.rule == "licence.mismatch" && f.blocking));
        // A lockfile entry from outside the registry is refused at intake.
        let mut git = lock.clone();
        git["packages"]["node_modules/tiny-csv"]["resolved"] =
            json!("git+https://git.example.org/x.git");
        std::fs::write(app.path().join("package-lock.json"), git.to_string()).unwrap();
        assert!(crate::analysis::deterministic_scan(app.path()).is_err());
    }

    /// A Python release with a dependency: `pip download --require-hashes --only-binary=:all:`
    /// on the fetch network into `wheelhouse/`, each wheel held to its pinned hash, installed
    /// offline by the runner Containerfile.
    #[test]
    fn a_python_release_downloads_hash_checked_wheels() {
        use pigeonpost_nest_kit::{render, template, Lang};
        let app = tempfile::tempdir().unwrap();
        for (path, body) in render(template("docs-qa").unwrap(), Lang::Python, "my-app", "acme") {
            let dest = app.path().join(&path);
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            std::fs::write(dest, body).unwrap();
        }
        let hash = sha256_hex(&fake_wheel("tinyparse", "1.0.2"));
        std::fs::write(
            app.path().join("requirements.txt"),
            format!("TinyParse==1.0.2 \\\n    --hash=sha256:{hash}\n"),
        )
        .unwrap();
        let manifest = app.path().join("nest-release.toml");
        let text = std::fs::read_to_string(&manifest).unwrap();
        std::fs::write(
            &manifest,
            text.replace("[licences]\n", "[licences]\ntinyparse = \"BSD-3-Clause\"\n"),
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_config(dir.path());
        let podman = FakeBuildPodman {
            source: Some(app.path().to_path_buf()),
            ..Default::default()
        };
        let work = dir.path().join("work");
        let built = build_from_source(
            &podman,
            &cfg,
            "https://git.example.org/acme/my-app.git",
            COMMIT,
            &work,
        )
        .unwrap();
        assert_eq!(built.reproducibility["equal"], true);
        assert!(
            built.scan.findings.iter().all(|f| !f.blocking),
            "{:?}",
            built.scan.findings
        );
        let calls = podman.calls.lock().unwrap();
        let pip = calls.iter().find(|c| c.iter().any(|a| a == "pip")).unwrap();
        for must in [
            "--require-hashes",
            "--only-binary=:all:",
            "--no-deps",
            "--network=buildd-fetch",
        ] {
            assert!(pip.contains(&must.to_string()), "{must}: {pip:?}");
        }
        // The wheels are installed offline, hash-checked, by the compile step (no network),
        // which leaves deps/ and removes the wheelhouse before the image is assembled.
        let compile = calls
            .iter()
            .find(|c| {
                c.iter().any(|a| a.ends_with(":/src:rw"))
                    && c.contains(&"--network=none".to_string())
            })
            .expect("a compile step");
        let script = compile.last().unwrap();
        for must in [
            "--no-index",
            "--require-hashes",
            "--no-compile",
            "--only-binary=:all:",
        ] {
            assert!(script.contains(must), "{must}: {script}");
        }
        assert!(compile.contains(&cfg.build.python_fetch_image.clone().unwrap()));
        assert!(work.join("first/context/deps").is_dir());
        assert!(!work.join("first/context/wheelhouse").exists());
        // Undeclared licence: a blocker, exactly as for crates.
        std::fs::write(&manifest, &text).unwrap();
        let scan = crate::analysis::deterministic_scan(app.path()).unwrap();
        assert!(scan
            .findings
            .iter()
            .any(|f| f.rule == "licence.undeclared" && f.blocking));
    }

    /// The behaviour test on the build plane fails a release that leaks a planted token or answers
    /// nothing, and the build plane hands that stage to the postbox after the scan.
    #[tokio::test]
    async fn the_behaviour_test_runs_on_the_build_plane_and_catches_leaks() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_config(dir.path());
        let leaky = FakeBuildPodman {
            leak: true,
            ..Default::default()
        };
        let work = dir.path().join("leaky");
        let built = build_from_source(
            &leaky,
            &cfg,
            "https://git.example.org/acme/x.git",
            COMMIT,
            &work,
        )
        .unwrap();
        let b = built.behaviour.unwrap();
        assert_eq!(b.decision, "failed");
        assert!(b
            .findings
            .iter()
            .any(|f| f.rule == "behaviour.exfiltration"));
        let silent = FakeBuildPodman {
            silent: true,
            ..Default::default()
        };
        let built = build_from_source(
            &silent,
            &cfg,
            "https://git.example.org/acme/x.git",
            COMMIT,
            &dir.path().join("silent"),
        )
        .unwrap();
        let b = built.behaviour.unwrap();
        assert_eq!(b.decision, "failed");
        assert!(b.findings.iter().any(|f| f.rule == "behaviour.unanswered"));

        // Through the service: the stages are the scan, then the behaviour test.
        let podman = Arc::new(FakeBuildPodman::default());
        let b = Arc::new(
            Buildd::open(
                cfg.clone(),
                podman,
                sha256_hex(b"s"),
                sha256_hex(b"a"),
                None,
            )
            .unwrap(),
        );
        b.submit(&BuildReq {
            build_id: "sbd_aaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            publisher_id: "pub_acme".into(),
            repo_url: "https://git.example.org/acme/x.git".into(),
            commit: COMMIT.into(),
            credential: None,
        })
        .unwrap();
        assert!(b.run_one().await);
        let v = b.read("sbd_aaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap().unwrap();
        let stages: Vec<&str> = v["stages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["stage"].as_str().unwrap())
            .collect();
        assert_eq!(stages, ["deterministic_scan", "behavioral_test"], "{v}");
        assert_eq!(v["stages"][1]["decision"], "passed");
    }

    fn scanners_config(dir: &Path) -> ScannersConfig {
        let rules = dir.join("semgrep.yml");
        std::fs::write(&rules, "rules: []\n").unwrap();
        let db = dir.join("scandb");
        std::fs::create_dir_all(&db).unwrap();
        let t = now() - 60;
        std::fs::write(
            db.join("manifest.json"),
            json!({ "osv": {"updated_at": t}, "grype": {"updated_at": t},
                    "clamav": {"updated_at": t}, "yara": {"updated_at": t} })
            .to_string(),
        )
        .unwrap();
        ScannersConfig {
            image: format!("localhost/pigeonpost-scanners@sha256:{}", "1".repeat(64)),
            clamav_image: format!("docker.io/clamav/clamav@sha256:{}", "2".repeat(64)),
            semgrep_image: format!("docker.io/semgrep/semgrep@sha256:{}", "3".repeat(64)),
            semgrep_rules_sha256: scanners::file_sha256(&rules).unwrap(),
            semgrep_rules: rules,
            db_root: db,
            max_db_age_hours: 72,
            timeout_secs: 600,
            memory_mib: 4096,
        }
    }

    /// The security tools scan the source, the compiled artefacts and the saved image, each in
    /// gVisor with no network, and their findings join the deterministic scan: a ClamAV hit on
    /// the compiled binary fails it.
    #[test]
    fn the_security_tools_join_the_deterministic_scan() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = test_config(dir.path());
        cfg.scanners = Some(scanners_config(dir.path()));
        let clean = FakeBuildPodman::default();
        let built = build_from_source(
            &clean,
            &cfg,
            "https://git.example.org/acme/x.git",
            COMMIT,
            &dir.path().join("clean"),
        )
        .unwrap();
        assert_eq!(built.scan.decision, "passed", "{:#?}", built.scan.findings);
        let tools = &built.scan.evidence["security_tools"]["tools"];
        for t in [
            "osv-scanner",
            "grype",
            "clamav",
            "yara",
            "semgrep",
            "gitleaks",
            "heuristics",
        ] {
            assert_eq!(tools[t]["ran"], true, "{t}: {tools}");
        }
        let calls = clean.calls.lock().unwrap().clone();
        let scans: Vec<&Vec<String>> = calls
            .iter()
            .filter(|c| c.iter().any(|a| a.ends_with(":/out:rw")))
            .collect();
        // The layer unpack, then OSV, Grype, ClamAV, YARA (source, artefacts, layers), Semgrep
        // and gitleaks.
        assert_eq!(scans.len(), 9, "{scans:#?}");
        for c in &scans {
            assert!(c.contains(&"--network=none".to_string()), "{c:?}");
            assert!(c.contains(&"--runtime=runsc".to_string()), "{c:?}");
        }
        let clam = scans
            .iter()
            .find(|c| c.contains(&"--entrypoint=clamscan".to_string()))
            .unwrap();
        for mount in [
            ":/scan/src:ro",
            ":/scan/artefacts:ro",
            ":/scan/layers:ro",
            ":/db/clamav:ro",
        ] {
            assert!(clam.iter().any(|a| a.ends_with(mount)), "{mount}: {clam:?}");
        }
        let dirty = FakeBuildPodman {
            malware: true,
            ..Default::default()
        };
        let built = build_from_source(
            &dirty,
            &cfg,
            "https://git.example.org/acme/x.git",
            COMMIT,
            &dir.path().join("dirty"),
        )
        .unwrap();
        assert_eq!(built.scan.decision, "failed");
        let hit = built
            .scan
            .findings
            .iter()
            .find(|f| f.tool == "clamav")
            .unwrap();
        assert!(hit.blocking);
        assert_eq!(
            hit.path.as_deref(),
            Some("artefacts/target/release/hr-policy-qa")
        );
        // Without [scanners] the scan says it ran none of them (advisory).
        let plain = build_from_source(
            &FakeBuildPodman::default(),
            &test_config(dir.path()),
            "https://git.example.org/acme/x.git",
            COMMIT,
            &dir.path().join("plain"),
        )
        .unwrap();
        assert!(plain
            .scan
            .findings
            .iter()
            .any(|f| f.rule == "scanner.not_configured" && !f.blocking));
    }

    /// The observed behaviour run: gVisor's network trace is diffed against the declared egress,
    /// the cgroup is sampled, and a missing trace is not a clean run.
    #[test]
    fn the_behaviour_run_is_observed_for_egress_and_resources() {
        let dir = tempfile::tempdir().unwrap();
        let trace_root = dir.path().join("trace");
        let cgroup = dir.path().join("cgroup");
        std::fs::create_dir_all(&cgroup).unwrap();
        std::fs::write(cgroup.join("memory.current"), "52428800\n").unwrap();
        std::fs::write(cgroup.join("cpu.stat"), "usage_usec 1000\nuser_usec 800\n").unwrap();
        let mut cfg = test_config(dir.path());
        cfg.behaviour = Some(BehaviourConfig {
            runtime: "runsc-behaviour".into(),
            trace_root: trace_root.clone(),
            cgroup: Some(cgroup),
            budget: Budget {
                memory_mib: 512,
                cpu_millis: 1000,
                job_seconds: 30,
            },
        });
        let build = |podman: &FakeBuildPodman, name: &str| {
            build_from_source(
                podman,
                &cfg,
                "https://git.example.org/acme/x.git",
                COMMIT,
                &dir.path().join(name),
            )
            .unwrap()
            .behaviour
            .unwrap()
        };
        let leaky = FakeBuildPodman {
            trace: Some((
                trace_root.clone(),
                include_str!("../tests/fixtures/scanners/strace.log").into(),
            )),
            ..Default::default()
        };
        let b = build(&leaky, "egress");
        assert_eq!(b.decision, "failed", "{:#?}", b.findings);
        assert!(b
            .findings
            .iter()
            .any(|f| f.rule == "egress.undeclared" && f.blocking));
        assert!(b
            .findings
            .iter()
            .any(|f| f.rule == "egress.dns" && f.blocking));
        assert_eq!(b.evidence["observed"]["egress"]["traced"], true);
        assert_eq!(b.evidence["observed"]["resources"]["measured"], true);
        assert!(
            b.evidence["observed"]["buyer_data_canaries"]
                .as_u64()
                .unwrap()
                >= 1
        );
        assert!(
            !trace_root.join("0123456789abcdef0123456789abcdef").exists(),
            "trace removed"
        );
        let run = leaky
            .calls
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.iter().any(|a| a.ends_with(":/nest:rw")))
            .cloned()
            .unwrap();
        assert!(
            run.contains(&"--runtime=runsc-behaviour".to_string()),
            "{run:?}"
        );
        assert!(run.contains(&"--network=none".to_string()));
        assert!(run.iter().any(|a| a.starts_with("--cidfile=")));

        let quiet = FakeBuildPodman {
            trace: Some((trace_root.clone(), "I1111 strace.go:614] [ 1: 1] app E connect(0x3 socket:[1], 0x1 {Family: AF_INET, Addr: 127.0.0.1, Port: 9000}, 0x10)\n".into())),
            ..Default::default()
        };
        let b = build(&quiet, "quiet");
        assert_eq!(b.decision, "passed", "{:#?}", b.findings);
        // No trace at all: the run proves nothing.
        let b = build(&FakeBuildPodman::default(), "untraced");
        assert_eq!(b.decision, "failed");
        assert!(b
            .findings
            .iter()
            .any(|f| f.rule == "egress.unobserved" && f.blocking));
    }

    /// The egress trace is read as bytes (a non-UTF-8 process name cannot hide its lines), needs
    /// the sentry's boot log, and a trace over the cap is a flood, not a clean run.
    #[test]
    fn the_trace_reader_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("netlog");
        let id = "0123456789abcdef0123456789abcdef";
        let cid = dir.path().join("cid");
        std::fs::write(&cid, id).unwrap();
        let logs = root.join(id);
        std::fs::create_dir_all(&logs).unwrap();
        let mut boot = b"I1111 strace.go:614] [ 3: 3] \xff\xfeevil E connect(0x3 socket:[1], 0x1 {Family: AF_INET, Addr: 93.184.216.34, Port: 443}, 0x10)\n".to_vec();
        boot.extend_from_slice(b"\xc3\x28 more invalid UTF-8\n");
        std::fs::write(logs.join("runsc.log.20261111-100000.boot.txt"), &boot).unwrap();
        let Trace::Text(t) = take_trace(&root, &cid) else {
            panic!("boot log present")
        };
        let attempts = crate::analysis::parse_strace(&t);
        assert!(
            attempts.iter().any(|a| a.addr == "93.184.216.34"),
            "{attempts:?}"
        );
        assert!(!logs.exists(), "removed after reading");
        // Only a gofer log: no boot log, nothing observed.
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("runsc.log.20261111-100000.gofer.txt"), "x").unwrap();
        assert_eq!(take_trace(&root, &cid), Trace::Missing);
        // Over the cap.
        std::fs::create_dir_all(&logs).unwrap();
        let f = std::fs::File::create(logs.join("runsc.log.boot.txt")).unwrap();
        f.set_len(TRACE_MAX_BYTES + 1).unwrap();
        assert!(matches!(take_trace(&root, &cid), Trace::Flood(_)));
        std::fs::remove_file(&cid).unwrap();
        assert_eq!(take_trace(&root, &cid), Trace::Missing);
    }

    fn observed(dir: &Path, trace_root: &Path, cgroup: &Path) -> BuilddConfig {
        std::fs::create_dir_all(cgroup).unwrap();
        std::fs::write(cgroup.join("memory.current"), "52428800\n").unwrap();
        std::fs::write(cgroup.join("cpu.stat"), "usage_usec 1000\n").unwrap();
        let mut cfg = test_config(dir);
        cfg.behaviour_timeout_secs = 5;
        cfg.behaviour = Some(BehaviourConfig {
            runtime: "runsc-behaviour".into(),
            trace_root: trace_root.to_path_buf(),
            cgroup: Some(cgroup.to_path_buf()),
            budget: Budget {
                memory_mib: 512,
                cpu_millis: 1000,
                job_seconds: 30,
            },
        });
        cfg
    }

    /// Buyer-data markers are found in the output even when the run is ended by its timeout
    /// (both streams are kept) and when they are base64-wrapped across lines; they carry no fixed
    /// prefix.
    #[test]
    fn buyer_data_in_the_output_is_found_whatever_the_exit_and_encoding() {
        let dir = tempfile::tempdir().unwrap();
        let trace = dir.path().join("trace");
        let cfg = observed(dir.path(), &trace, &dir.path().join("cg"));
        let quiet = "I1111 strace.go:614] app E connect(0x3 socket:[1], 0x1 {Family: AF_UNIX, Addr: \"/x\"}, 0x10)\n";
        let podman = FakeBuildPodman {
            log_input: true,
            trace: Some((trace.clone(), quiet.into())),
            ..Default::default()
        };
        let b = build_from_source(
            &podman,
            &cfg,
            "https://git.example.org/acme/x.git",
            COMMIT,
            &dir.path().join("w"),
        )
        .unwrap()
        .behaviour
        .unwrap();
        assert!(
            b.findings
                .iter()
                .any(|f| f.rule == "data.in_logs" && f.blocking),
            "{:#?}",
            b.findings
        );
        let (a, c) = (crate::analysis::canary(), crate::analysis::canary());
        assert_eq!(a.len(), 24);
        assert!(a.bytes().all(|x| x.is_ascii_alphanumeric()));
        assert_ne!(a[..4], c[..4], "no fixed prefix");
    }

    /// CPU counts until the run ends (burning after the last answer does not escape the budget),
    /// and only complete, valid result files are timed as answers.
    #[test]
    fn resource_use_counts_the_whole_run_and_only_valid_answers() {
        let dir = tempfile::tempdir().unwrap();
        let trace = dir.path().join("trace");
        let cg = dir.path().join("cg");
        let cfg = observed(dir.path(), &trace, &cg);
        let quiet = "I1111 strace.go:614] app E connect(0x3 socket:[1], 0x1 {Family: AF_UNIX, Addr: \"/x\"}, 0x10)\n";
        let burner = FakeBuildPodman {
            burn: Some(cg.clone()),
            trace: Some((trace.clone(), quiet.into())),
            ..Default::default()
        };
        let b = build_from_source(
            &burner,
            &cfg,
            "https://git.example.org/acme/x.git",
            COMMIT,
            &dir.path().join("burn"),
        )
        .unwrap()
        .behaviour
        .unwrap();
        assert!(
            b.findings
                .iter()
                .any(|f| f.rule == "resources.cpu" && f.blocking),
            "{:#?}",
            b.findings
        );
        std::fs::write(cg.join("cpu.stat"), "usage_usec 1000\n").unwrap();
        let partial = FakeBuildPodman {
            partial: true,
            trace: Some((trace.clone(), quiet.into())),
            ..Default::default()
        };
        let b = build_from_source(
            &partial,
            &cfg,
            "https://git.example.org/acme/x.git",
            COMMIT,
            &dir.path().join("partial"),
        )
        .unwrap()
        .behaviour
        .unwrap();
        assert_eq!(
            b.evidence["observed"]["resources"]["slowest_job_ms"],
            Value::Null,
            "{}",
            b.evidence
        );
        assert!(b.findings.iter().any(|f| f.rule == "behaviour.unanswered"));
    }

    /// A scan running when the service stopped runs again; a release re-scan without its image
    /// says so (blocking, unique to the run); an image no re-scan has named yet outlives
    /// `keep_days`.
    #[tokio::test]
    async fn scans_survive_a_restart_and_images_wait_for_their_first_rescan() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = test_config(dir.path());
        cfg.db = dir.path().join("buildd.db");
        cfg.scanners = Some(scanners_config(dir.path()));
        let open = || {
            Arc::new(
                Buildd::open(
                    cfg.clone(),
                    Arc::new(FakeBuildPodman::default()),
                    sha256_hex(b"s"),
                    sha256_hex(b"a"),
                    None,
                )
                .unwrap(),
            )
        };
        let b = open();
        let req = ScanReq {
            scan_id: "scn_cccccccccccccccccccccccccc".into(),
            kind: "release".into(),
            files: vec![],
            release_id: Some("rel_gone".into()),
            image_digest: Some(format!("sha256:{}", "7".repeat(64))),
            sbom: Some("{\"bomFormat\":\"CycloneDX\"}".into()),
        };
        b.submit_scan(&req).unwrap();
        b.with(|c| c.execute("UPDATE scans SET state = 'running'", []))
            .unwrap();
        drop(b);
        let b = open();
        assert_eq!(
            b.read_scan(&req.scan_id).unwrap().unwrap()["state"],
            "queued"
        );
        assert!(b.run_one_scan().await);
        let v = b.read_scan(&req.scan_id).unwrap().unwrap();
        let gone: Vec<&Value> = v["result"]["findings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["rule"] == "scanner.image_unavailable")
            .collect();
        assert_eq!(gone.len(), 1, "{v}");
        assert_eq!(gone[0]["blocking"], true);
        assert_eq!(v["result"]["evidence"]["image_scanned"], false);

        // keep_days (14) has passed for both; only the one a re-scan has named goes.
        let old = now() - 30 * 86_400;
        for (id, digest, archive) in [("sbd_a", "1", "a.tar"), ("sbd_b", "2", "b.tar")] {
            let path = dir.path().join(archive);
            std::fs::write(&path, b"tar").unwrap();
            b.with(|c| {
                c.execute(
                    "INSERT INTO builds (id, publisher_id, repo_url, commit_sha, state,
                        image_digest, archive, archive_sha256, archive_size, created_at, updated_at)
                     VALUES (?1, 'pub_a', 'x', 'y', 'succeeded', ?2, ?3, 'z', 3, ?4, ?4)",
                    params![
                        id,
                        format!("sha256:{}", digest.repeat(64)),
                        path.display().to_string(),
                        old
                    ],
                )
            })
            .unwrap();
        }
        b.with(|c| {
            c.execute(
                "INSERT INTO rescanned_images (image_digest, first_at) VALUES (?1, ?2)",
                params![format!("sha256:{}", "2".repeat(64)), old],
            )
        })
        .unwrap();
        b.prune().unwrap();
        assert!(dir.path().join("a.tar").exists(), "never re-scanned: kept");
        assert!(
            !dir.path().join("b.tar").exists(),
            "re-scanned and no longer retained: pruned"
        );
    }

    /// The postbox's scans: result files get ClamAV and YARA, a release re-scan keeps its image
    /// past `keep_days`, and malformed requests are refused.
    #[tokio::test]
    async fn result_and_release_scans_run_from_the_queue() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = test_config(dir.path());
        cfg.scanners = Some(scanners_config(dir.path()));
        let podman = Arc::new(FakeBuildPodman {
            malware: true,
            ..Default::default()
        });
        let b = Arc::new(
            Buildd::open(
                cfg,
                podman.clone(),
                sha256_hex(b"s"),
                sha256_hex(b"a"),
                None,
            )
            .unwrap(),
        );
        let id = "scn_aaaaaaaaaaaaaaaaaaaaaaaaaa";
        let bad = |r: ScanReq| b.submit_scan(&r).unwrap_err().0;
        let files = |n: usize, body: &str| ScanReq {
            scan_id: id.into(),
            kind: "files".into(),
            files: (0..n)
                .map(|i| ScanFile {
                    name: format!("../../etc/r{i}.pdf"),
                    media_type: "application/pdf".into(),
                    content_b64: body.into(),
                })
                .collect(),
            release_id: None,
            image_digest: None,
            sbom: None,
        };
        assert_eq!(
            bad(ScanReq {
                scan_id: "scn_x".into(),
                ..files(1, "JVBERi0=")
            }),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(bad(files(6, "JVBERi0=")), StatusCode::BAD_REQUEST);
        assert_eq!(bad(files(1, "not*base64")), StatusCode::BAD_REQUEST);
        let v = b.submit_scan(&files(1, "JVBERi0xLjQKJSVFT0YK")).unwrap();
        assert_eq!(v["state"], "queued");
        assert_eq!(
            b.submit_scan(&files(1, "JVBERi0=")).unwrap()["scan_id"],
            id,
            "same id, same scan"
        );
        assert!(b.run_one_scan().await);
        let v = b.read_scan(id).unwrap().unwrap();
        assert_eq!(v["state"], "succeeded", "{v}");
        assert_eq!(v["result"]["blocking"], 1, "{v}");
        let calls = podman.calls.lock().unwrap().clone();
        let tools: Vec<&str> = calls
            .iter()
            .filter_map(|c| c.iter().find_map(|a| a.strip_prefix("--entrypoint=")))
            .collect();
        assert_eq!(tools, vec!["clamscan", "yara"]);
        assert!(
            !b.scan_dir(id).exists(),
            "the files are gone after the scan"
        );

        // A release re-scan: the SBOM always, the image while this host has it; retained.
        let digest = format!("sha256:{}", "9".repeat(64));
        let archive = dir.path().join("old.tar");
        std::fs::write(&archive, b"tar").unwrap();
        b.with(|c| {
            c.execute(
                "INSERT INTO builds (id, publisher_id, repo_url, commit_sha, state, image_digest,
                    archive, archive_sha256, archive_size, created_at, updated_at)
                 VALUES ('sbd_old', 'pub_a', 'x', 'y', 'succeeded', ?1, ?2, 'z', 3, 0, 0)",
                params![digest, archive.display().to_string()],
            )
        })
        .unwrap();
        let rid = "scn_bbbbbbbbbbbbbbbbbbbbbbbbbb";
        let release = ScanReq {
            scan_id: rid.into(),
            kind: "release".into(),
            files: vec![],
            release_id: Some("rel_abc".into()),
            image_digest: Some(digest.clone()),
            sbom: Some("{\"bomFormat\":\"CycloneDX\",\"components\":[]}".into()),
        };
        assert_eq!(
            bad(ScanReq {
                sbom: Some("not json".into()),
                ..release.clone()
            }),
            StatusCode::BAD_REQUEST
        );
        b.submit_scan(&release).unwrap();
        assert!(b.run_one_scan().await);
        let v = b.read_scan(rid).unwrap().unwrap();
        assert_eq!(v["state"], "succeeded", "{v}");
        assert_eq!(v["result"]["evidence"]["image_scanned"], true);
        b.prune().unwrap();
        assert!(archive.exists(), "a published release's image is kept");
        b.with(|c| c.execute("DELETE FROM retained_images", []))
            .unwrap();
        b.prune().unwrap();
        assert!(!archive.exists());
    }

    /// A deploy-key source (M2b): the key is fetched for the named build request only, must be for
    /// the repository being fetched, reaches git through a read-only mount against pinned host
    /// keys, and is gone from disk (and the queue) once the fetch is done.
    #[tokio::test]
    async fn deploy_key_sources_fetch_their_key_for_the_fetch_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = test_config(dir.path());
        let hosts = dir.path().join("known_hosts");
        std::fs::write(&hosts, "git.example.org ssh-ed25519 AAAAsynthetic\n").unwrap();
        cfg.git.known_hosts = Some(hosts);
        let key =
            "-----BEGIN OPENSSH PRIVATE KEY-----\nsynthetic\n-----END OPENSSH PRIVATE KEY-----\n";
        let repo = "git@git.example.org:acme/private-app.git";
        let podman = Arc::new(FakeBuildPodman::default());
        let b = Arc::new(
            Buildd::open(
                cfg.clone(),
                podman.clone(),
                sha256_hex(b"s"),
                sha256_hex(b"a"),
                None,
            )
            .unwrap()
            .with_credentials(Credentials::Fixed(
                repo.into(),
                key.into(),
                Mutex::new(Vec::new()),
            )),
        );
        let mut req = BuildReq {
            build_id: "sbd_aaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            publisher_id: "pub_acme".into(),
            repo_url: repo.into(),
            commit: COMMIT.into(),
            credential: None,
        };
        // An SSH repository without a credential is refused at the door.
        assert_eq!(b.submit(&req).unwrap_err().0, StatusCode::BAD_REQUEST);
        req.credential = Some("not-a-build".into());
        assert_eq!(b.submit(&req).unwrap_err().0, StatusCode::BAD_REQUEST);
        req.credential = Some("bld_aaaaaaaaaaaaaaaaaaaaaaaaaa".into());
        b.submit(&req).unwrap();
        assert!(b.run_one().await);
        let v = b.read("sbd_aaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap().unwrap();
        assert_eq!(v["state"], "succeeded", "{v}");
        assert!(
            !v.to_string().contains("PRIVATE KEY"),
            "the key is not kept"
        );
        if let Credentials::Fixed(_, _, seen) = &b.credentials {
            assert_eq!(*seen.lock().unwrap(), ["bld_aaaaaaaaaaaaaaaaaaaaaaaaaa"]);
        }
        assert_eq!(podman.key_seen.lock().unwrap().as_deref(), Some(key));
        let calls = podman.calls.lock().unwrap().clone();
        let fetch = calls
            .iter()
            .find(|c| c.iter().any(|a| a == "fetch"))
            .unwrap();
        assert!(fetch.iter().any(|a| a.ends_with(":/cred:ro")), "{fetch:?}");
        assert!(fetch
            .iter()
            .any(|a| a.contains("StrictHostKeyChecking=yes")));
        assert!(fetch.iter().any(|a| a == "protocol.ssh.allow=always"));
        // Only the fetch sees the key; the rest of git does not.
        assert_eq!(
            calls
                .iter()
                .filter(|c| c.iter().any(|a| a.ends_with(":/cred:ro")))
                .count(),
            1
        );
        assert!(!cfg
            .work_root
            .join("sbd_aaaaaaaaaaaaaaaaaaaaaaaaaa")
            .exists());

        // A credential for another repository is not used.
        let other = Arc::new(
            Buildd::open(
                cfg.clone(),
                Arc::new(FakeBuildPodman::default()),
                sha256_hex(b"s"),
                sha256_hex(b"a"),
                None,
            )
            .unwrap()
            .with_credentials(Credentials::Fixed(
                "git@git.example.org:acme/other.git".into(),
                key.into(),
                Mutex::new(Vec::new()),
            )),
        );
        other.submit(&req).unwrap();
        assert!(other.run_one().await);
        let v = other
            .read("sbd_aaaaaaaaaaaaaaaaaaaaaaaaaa")
            .unwrap()
            .unwrap();
        assert_eq!(v["state"], "failed");
        assert!(v["error"].as_str().unwrap().contains("another repository"));
        // No pinned host keys: refused.
        let mut loose = cfg.clone();
        loose.git.known_hosts = None;
        let e = fetch_source_with(
            &FakeBuildPodman::default(),
            &loose,
            repo,
            COMMIT,
            &dir.path().join("x/src"),
            Some(key),
        )
        .unwrap_err();
        assert!(e.contains("known_hosts"), "{e}");
    }

    /// The worker host's podman: has nothing until `load`, then has what the archive names.
    #[derive(Default)]
    struct FakeHostPodman {
        loaded: Mutex<Option<String>>,
    }

    impl Podman for FakeHostPodman {
        fn run(&self, args: &[&str]) -> Result<String, String> {
            match args.first().copied() {
                Some("load") => {
                    let input = args[args.iter().position(|a| *a == "--input").unwrap() + 1];
                    *self.loaded.lock().unwrap() =
                        Some(oci_archive_manifest_digest(Path::new(input))?);
                    Ok(String::new())
                }
                Some("image") => self
                    .loaded
                    .lock()
                    .unwrap()
                    .clone()
                    .ok_or("no such image".into()),
                _ => Ok(String::new()),
            }
        }
    }

    async fn post(url: &str, token: &str, body: Value) -> (u16, Value) {
        let r = reqwest::Client::new()
            .post(format!("{url}/v1/builds"))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
    }

    /// W1.3 end to end: the postbox queues a git source build, buildd fetches, builds twice,
    /// saves and scans it, the postbox reads the record and evidence, and the worker host pulls
    /// the image by digest with its own read-only token.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_git_build_becomes_an_artifact_the_worker_host_pulls_by_digest() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = test_config(dir.path());
        let podman = Arc::new(FakeBuildPodman::default());
        let b = Arc::new(
            Buildd::open(
                cfg,
                podman.clone(),
                sha256_hex(b"submit-token"),
                sha256_hex(b"artifact-token"),
                None,
            )
            .unwrap(),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = router(b.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let req = json!({
            "build_id": "sbd_aaaaaaaaaaaaaaaaaaaaaaaaaa",
            "publisher_id": "pub_acme",
            "repo_url": "https://git.example.org/acme/hr-policy-qa.git",
            "commit": COMMIT,
        });
        assert_eq!(post(&url, "artifact-token", req.clone()).await.0, 401);
        assert_eq!(post(&url, "wrong", req.clone()).await.0, 401);
        let (s, v) = post(&url, "submit-token", req.clone()).await;
        assert_eq!(s, 202, "{v}");
        assert_eq!(v["state"], "queued");
        // Idempotent; another source under the same id is a conflict; a bad host is refused.
        assert_eq!(post(&url, "submit-token", req.clone()).await.0, 202);
        let mut other = req.clone();
        other["commit"] = json!("f".repeat(40));
        assert_eq!(post(&url, "submit-token", other).await.0, 409);
        let mut evil = req.clone();
        evil["build_id"] = json!("sbd_bbbbbbbbbbbbbbbbbbbbbbbbbb");
        evil["repo_url"] = json!("https://evil.example.com/x/y");
        assert_eq!(post(&url, "submit-token", evil).await.0, 400);

        assert!(b.run_one().await);
        assert!(!b.run_one().await, "the queue is empty");
        let read = reqwest::Client::new()
            .get(format!("{url}/v1/builds/sbd_aaaaaaaaaaaaaaaaaaaaaaaaaa"))
            .bearer_auth("submit-token")
            .send()
            .await
            .unwrap();
        assert_eq!(read.status(), 200);
        let v: Value = read.json().await.unwrap();
        assert_eq!(v["state"], "succeeded", "{v}");
        let record = &v["record"];
        // The archive's manifest is the attested image digest, and the release digest covers it.
        assert_eq!(record["image_digest"], MANIFEST);
        assert_eq!(
            record["image"],
            format!("localhost/pigeonpost-release@{MANIFEST}")
        );
        assert_eq!(
            record["release_digest"].as_str().unwrap(),
            release_digest(
                record["source_digest"].as_str().unwrap(),
                record["vendor_digest"].as_str().unwrap(),
                record["manifest_digest"].as_str().unwrap(),
                record["sbom_digest"].as_str().unwrap(),
                MANIFEST,
            )
        );
        assert_eq!(record["isolation"]["source"]["commit"], COMMIT);
        assert_eq!(v["reproducibility"]["builds"], 2);
        assert_eq!(v["stages"][0]["stage"], "deterministic_scan");
        assert!(v["stages"][0]["evidence_digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:"));
        // Two builds, each without a network; git ran on the git network only.
        let calls = podman.calls.lock().unwrap().clone();
        let builds: Vec<_> = calls.iter().filter(|c| c[0] == "build").collect();
        assert_eq!(builds.len(), 2);
        assert!(builds
            .iter()
            .all(|c| c.contains(&"--network=none".to_string())
                && c.contains(&"--timestamp=0".to_string())));
        assert!(calls
            .iter()
            .filter(|c| c.iter().any(|a| a == GIT))
            .all(|c| c.contains(&"--network=buildd-git".to_string())));
        // Publisher code ran in the compile step only: no network, gVisor, nothing writable but
        // the tree it compiles in.
        let compile: Vec<&Vec<String>> = calls
            .iter()
            .filter(|c| c[0] == "run" && c.iter().any(|a| a.ends_with(":/src:rw")))
            .filter(|c| !c.iter().any(|a| a == GIT))
            .collect();
        assert_eq!(compile.len(), 2, "one per build");
        for c in compile {
            for must in [
                "--network=none",
                "--runtime=runsc",
                "--read-only",
                "--cap-drop=ALL",
            ] {
                assert!(c.contains(&must.to_string()), "{must}: {c:?}");
            }
        }
        assert!(!dir
            .path()
            .join("work/sbd_aaaaaaaaaaaaaaaaaaaaaaaaaa")
            .exists());

        // The worker host pulls it with the artifact token, and only it.
        let host = Arc::new(FakeHostPodman::default());
        let images = |token: &str| {
            crate::artifacts::BuilddImages::new(
                crate::config::ArtifactsConfig {
                    url: url.clone(),
                    token_file: dir.path().join("unused"),
                    cache_dir: dir.path().join("cache"),
                    max_bytes: 1 << 20,
                },
                token.into(),
                HostRef(host.clone()),
            )
            .unwrap()
        };
        let image = format!("localhost/pigeonpost-release@{MANIFEST}");
        let wrong = images("submit-token");
        let i2 = image.clone();
        let r =
            tokio::task::spawn_blocking(move || crate::dispatch::ImageSource::ensure(&wrong, &i2))
                .await
                .unwrap();
        assert!(r.is_err(), "the submit token cannot read artifacts");
        let right = images("artifact-token");
        let i2 = image.clone();
        tokio::task::spawn_blocking(move || crate::dispatch::ImageSource::ensure(&right, &i2))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(host.loaded.lock().unwrap().as_deref(), Some(MANIFEST));
        // An image the build plane never made is not there to pull.
        let missing = images("artifact-token");
        let r = tokio::task::spawn_blocking(move || {
            crate::dispatch::ImageSource::ensure(
                &missing,
                &format!("localhost/x@sha256:{}", "9".repeat(64)),
            )
        })
        .await
        .unwrap();
        assert!(r.is_err());
    }

    struct HostRef(Arc<FakeHostPodman>);
    impl Podman for HostRef {
        fn run(&self, args: &[&str]) -> Result<String, String> {
            self.0.run(args)
        }
    }

    #[test]
    fn the_queue_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let b = Buildd::open(
            test_config(dir.path()),
            Arc::new(FakeBuildPodman::default()),
            sha256_hex(b"a"),
            sha256_hex(b"b"),
            None,
        )
        .unwrap();
        let req = |id: &str| BuildReq {
            build_id: id.into(),
            publisher_id: "pub_acme".into(),
            repo_url: "https://git.example.org/acme/x".into(),
            commit: COMMIT.into(),
            credential: None,
        };
        b.submit(&req("sbd_aaaaaaaaaaaaaaaaaaaaaaaaaa")).unwrap();
        b.submit(&req("sbd_bbbbbbbbbbbbbbbbbbbbbbbbbb")).unwrap();
        let e = b
            .submit(&req("sbd_cccccccccccccccccccccccccc"))
            .unwrap_err();
        assert_eq!(e.1, "queue_full");
        // The same token for both callers is refused.
        assert!(Buildd::open(
            test_config(dir.path()),
            Arc::new(FakeBuildPodman::default()),
            sha256_hex(b"a"),
            sha256_hex(b"a"),
            None,
        )
        .is_err());
    }

    #[test]
    fn build_ids_are_strict() {
        assert!(valid_build_id("sbd_abcdefghijklmnopqrstuvwxyz"));
        assert!(!valid_build_id("sbd_ABCDEFGHIJKLMNOPQRSTUVWXYZ"));
        assert!(!valid_build_id("sbd_short"));
        assert!(!valid_build_id("job_abcdefghijklmnopqrstuvwxyz"));
    }
}
