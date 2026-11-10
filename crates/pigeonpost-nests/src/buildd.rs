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
//!    then `podman build --network=none --timestamp=0`.
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

use crate::analysis::{self, ReviewRouteConfig, StageReport};
use crate::artifacts::{
    digest_ok, oci_archive_manifest_digest, CHUNK_BYTES, SHA256_HEADER, SIZE_HEADER,
};
use crate::release::{build_tag, release_digest, BuildConfig, BuildRecord, Builder};
use crate::sandbox::Podman;
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
    /// Saved images older than this are deleted (a pulled image lives on the worker host).
    #[serde(default = "default_keep_days")]
    pub keep_days: i64,
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
fn default_behaviour_secs() -> u64 {
    60
}

fn pinned(image: &str) -> bool {
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
        if !(5..=600).contains(&self.behaviour_timeout_secs) {
            return Err("behaviour_timeout_secs: 5-600".into());
        }
        if self.max_queued < 1 || self.keep_days < 1 {
            return Err("max_queued and keep_days must be positive".into());
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
    args.push(cfg.git.image.clone());
    args.push("git".into());
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
    record.isolation = json!({
        "plane": "build host (pigeonpost-nest-buildd), separate from the worker host",
        "ecosystem": ecosystem,
        "runtime": cfg.build.runtime,
        "rootless": true,
        "git_network": cfg.git.network,
        "fetch_network": cfg.build.fetch_network,
        "build_network": "none",
        "source": { "repo_url": repo_url, "commit": commit },
        "reproducible": cfg.reproducible,
    });
    let scan = analysis::deterministic_scan(&src)?;
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
/// `pigeonpost nest run` reads them), or one per operation with every field filled.
fn behaviour_jobs(manifest: &crate::release::Manifest, src: &Path) -> Vec<Value> {
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
    for (i, j) in jobs.iter_mut().enumerate() {
        j["job_id"] = json!(format!("job_behaviour{i:02}"));
        j["config"] = json!({});
    }
    jobs
}

/// The behaviour test (R-63) on the build plane, after the scan: the image just built runs under
/// the build runtime with **no network**, no capabilities, a read-only root and the worker's
/// user, honeytokens planted on its volume and test jobs in its inbox. Its job results and its
/// output are searched for the tokens; a job left unanswered fails the stage too. There is no
/// broker on the build plane, so a model call ends in the SDK's `no_broker` error: an answered
/// job is any result, succeeded or failed.
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
    for j in &jobs {
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
    let mut args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--pull=never".into(),
        format!("--runtime={}", cfg.build.runtime),
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
    args.extend(cfg.build.limit_args());
    args.push(tag.to_string());
    // A worker serves forever; the timeout ends it. Its exit status says nothing here.
    let output = run_args(podman, &args).unwrap_or_else(|e| e);
    let mut results = Vec::new();
    let mut answered = 0;
    if let Ok(dir) = std::fs::read_dir(volume.join("jobs/out")) {
        for entry in dir.flatten() {
            let bytes = std::fs::read(entry.path()).unwrap_or_default();
            if serde_json::from_slice::<Value>(&bytes)
                .ok()
                .is_some_and(|v| v["status"].is_string())
            {
                answered += 1;
            }
            results.extend_from_slice(&bytes);
        }
    }
    let report = analysis::behaviour_report(
        &tokens,
        &[
            ("job results".into(), results),
            ("output".into(), output.into_bytes()),
        ],
        jobs.len(),
        answered,
        &format!("{} on the build plane, network none", cfg.build.runtime),
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
";

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

    /// Delete saved images older than `keep_days`.
    pub fn prune(&self) -> Result<usize, ApiErr> {
        let cutoff = now() - self.cfg.keep_days * 86_400;
        let old: Vec<(String, String)> = self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id, archive FROM builds WHERE archive IS NOT NULL AND updated_at < ?1",
            )?;
            let rows = stmt.query_map(params![cutoff], |r| Ok((r.get(0)?, r.get(1)?)))?;
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
        .layer(axum::extract::DefaultBodyLimit::max(16 * 1024))
        .with_state(b)
}

/// The queue loop: run builds one at a time, prune old images daily.
pub fn start_worker(b: Arc<Buildd>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(3));
        let mut last_prune = 0i64;
        loop {
            tick.tick().await;
            while b.run_one().await {}
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
        },
        reproducible: true,
        max_queued: 2,
        keep_days: 14,
        ai_review_route: None,
        behaviour_test: true,
        behaviour_timeout_secs: 30,
        postbox: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

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
        let git = a.iter().position(|x| x == "git").unwrap();
        assert_eq!(a[git - 1], cfg.git.image);
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
                // The behaviour run: the release answers its jobs (or leaks, or stays silent).
                Some("run") if volume(":/nest:rw").is_some() => {
                    let nest = volume(":/nest:rw").unwrap();
                    if self.silent {
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
                    Ok("worker stopped\n".into())
                }
                // `npm ci`: every locked package lands where the lockfile says.
                Some("run") if argv.iter().any(|a| a == "npm") => {
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
                Some("run") if argv.iter().any(|a| a == "git") => {
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
                    .filter(|f| f.blocker)
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
                    .any(|c| c.iter().any(|a| a == "npm" || a == "pip")));
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
            built.scan.findings.iter().all(|f| !f.blocker),
            "{:?}",
            built.scan.findings
        );
        let calls = podman.calls.lock().unwrap();
        let npm = calls.iter().find(|c| c.iter().any(|a| a == "npm")).unwrap();
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
            .any(|f| f.rule == "licence.mismatch" && f.blocker));
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
            built.scan.findings.iter().all(|f| !f.blocker),
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
        assert!(work
            .join("first/context/wheelhouse/tinyparse-1.0.2-py3-none-any.whl")
            .is_file());
        let containerfile =
            std::fs::read_to_string(work.join("first/context/Containerfile")).unwrap();
        assert!(containerfile.contains("--no-index") && containerfile.contains("--require-hashes"));
        // Undeclared licence: a blocker, exactly as for crates.
        std::fs::write(&manifest, &text).unwrap();
        let scan = crate::analysis::deterministic_scan(app.path()).unwrap();
        assert!(scan
            .findings
            .iter()
            .any(|f| f.rule == "licence.undeclared" && f.blocker));
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
            .filter(|c| c.iter().any(|a| a == "git"))
            .all(|c| c.contains(&"--network=buildd-git".to_string())));
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
