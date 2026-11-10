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
//! | `POST /v1/builds` | submit (postbox) | queue `{build_id, publisher_id, repo_url, commit}`; idempotent by `build_id` |
//! | `GET /v1/builds/{build_id}` | submit (postbox) | state; when done the build record, the SBOM, the scan evidence and the reproducibility check |
//! | `GET /v1/artifacts/{image_digest}?offset=N` | artifact (nestd) | the release image as an OCI archive, in chunks |
//! | `GET /healthz` | none | liveness |
//!
//! The postbox pulls results; this host holds **no** credential for the postbox or for nestd, so
//! publisher code that escaped its container here still could not reach either.
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
//! 5. **Analysis**: the deterministic scan, and the advisory AI review on the self-hosted model
//!    when a review route is configured. The behaviour test needs a sandbox and is recorded
//!    separately.
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
    let full_hex = |s: &str| {
        (s.len() == 40 || s.len() == 64)
            && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    };
    if !full_hex(commit) {
        return Err("commit: a full lower-case object id (40 or 64 hex digits)".into());
    }
    let Some(rest) = repo_url.strip_prefix("https://") else {
        return Err("repo_url: https only".into());
    };
    let (host, path) = rest.split_once('/').ok_or("repo_url: host and path")?;
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
    source_ok(&cfg.git, repo_url, commit)?;
    std::fs::create_dir_all(src).map_err(|e| e.to_string())?;
    run_args(podman, &git_args(cfg, src, &["init", "--quiet", "."]))?;
    run_args(
        podman,
        &git_args(
            cfg,
            src,
            &[
                "-c",
                "protocol.allow=never",
                "-c",
                "protocol.https.allow=always",
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
    let src = work.join("src");
    fetch_source(podman, cfg, repo_url, commit, &src)?;
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
    record.isolation = json!({
        "plane": "build host (pigeonpost-nest-buildd), separate from the worker host",
        "runtime": cfg.build.runtime,
        "rootless": true,
        "git_network": cfg.git.network,
        "fetch_network": cfg.build.fetch_network,
        "build_network": "none",
        "source": { "repo_url": repo_url, "commit": commit },
        "reproducible": cfg.reproducible,
    });
    let scan = analysis::deterministic_scan(&src)?;
    Ok(Built {
        record,
        scan,
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
    updated_at     INTEGER NOT NULL
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
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildReq {
    pub build_id: String,
    pub publisher_id: String,
    pub repo_url: String,
    pub commit: String,
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
        })
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
        source_ok(&self.cfg.git, &req.repo_url, &req.commit).map_err(bad)?;
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
                "INSERT INTO builds (id, publisher_id, repo_url, commit_sha, state, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?5)",
                params![req.build_id, req.publisher_id, req.repo_url, req.commit, t],
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

    /// Claim the oldest queued build.
    fn claim(&self) -> Result<Option<(String, String, String)>, ApiErr> {
        self.with(|c| {
            let row: Option<(String, String, String)> = c
                .query_row(
                    "SELECT id, repo_url, commit_sha FROM builds WHERE state = 'queued'
                      ORDER BY created_at, id LIMIT 1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            if let Some((id, _, _)) = &row {
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
        let Ok(Some((id, url, commit))) = self.claim() else {
            return false;
        };
        let work = self.cfg.work_root.join(&id);
        let me = self.clone();
        let w = work.clone();
        let built = tokio::task::spawn_blocking(move || {
            let _ = std::fs::remove_dir_all(&w);
            std::fs::create_dir_all(&w).map_err(|e| e.to_string())?;
            build_from_source(me.podman.as_ref(), &me.cfg, &url, &commit, &w)
        })
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r);
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
        },
        build: BuildConfig {
            runtime: "runsc".into(),
            fetch_network: "buildd-fetch".into(),
            fetch_image: format!("localhost/rust@sha256:{}", "d".repeat(64)),
            image_name: "localhost/pigeonpost-release".into(),
            memory_mib: Some(2048),
            cpu_millis: Some(2000),
            pids: Some(512),
        },
        reproducible: true,
        max_queued: 2,
        keep_days: 14,
        ai_review_route: None,
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
                Some("run") if argv.iter().any(|a| a == "git") => {
                    let src = volume(":/src:rw").unwrap();
                    if argv.iter().any(|a| a == "checkout") {
                        let root = crate::release::reference_source();
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
