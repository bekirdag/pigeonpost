//! The build plane (build plan M1, R-62/R-63): a release built from a pinned source tree into a
//! runner image, with an SBOM and a content digest.
//!
//! The plan puts this on a separate KVM build VM. That host does not exist (owner decision
//! 2026-11-09: nests-1 only), so the build runs on nests-1 as a **separate rootless podman build**
//! under the configured OCI runtime (gVisor `runsc`, or `crun`), in two steps:
//!
//! 1. **fetch**: `cargo vendor --locked` in a throwaway container on a fetch-only network. The
//!    vendored tree is hashed into the record, and cargo itself checks every crate against the
//!    checksums pinned in `Cargo.lock`.
//! 2. **compile** (`nest-build.toml`, optional): the release's one command in a `podman run`
//!    under the build runtime with `--network=none`, a read-only root, no capabilities and a
//!    writable copy of the source tree plus the vendored sources at `/src`. This is the only step
//!    in which publisher code runs (build scripts, proc macros, its own tools), and it runs in
//!    gVisor.
//! 3. **assemble**: `podman build --network=none --pull=never --timestamp=0` from that tree with a
//!    Containerfile that only copies files onto one pinned base
//!    ([`nest_package::validate_containerfile`]: no `RUN`, `ADD` or `COPY --from`), so nothing
//!    runs while the image is assembled and the same files always give the same image digest.
//!
//! Why the compile is not a `RUN` step: under rootless podman with gVisor, a `RUN` step's writes
//! land in gVisor's in-sandbox root overlay and never reach buildah's layer (the default), or are
//! refused on the 0555 root directory when the overlay is off and no capability is granted. A
//! plain `podman run` with a bind-mounted tree has neither problem and is the path the fetch step
//! and the behaviour test already use.
//!
//! Node and Python releases follow the same two steps with their own lockfile and fetch tool
//! ([`crate::ecosystem`]): `npm ci --ignore-scripts` into a vendored `node_modules/`, or
//! `pip download --require-hashes` into a `wheelhouse/` the image installs from offline.
//!
//! What comes out is a [`BuildRecord`]: the digests of the source tree, the vendored sources, the
//! manifest, the SBOM (CycloneDX 1.5 JSON, written here from `Cargo.lock` and the file list, so it
//! is deterministic and needs no network) and the image, and one **release digest** over all of
//! them. Staff attest that digest; nothing else identifies a release.

use crate::ecosystem::{self, Ecosystem};
use crate::sandbox::Podman;
use pigeonpost_core::nest_package;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const MANIFEST_FILE: &str = "nest-release.toml";
pub const CONTAINERFILE: &str = nest_package::CONTAINERFILE;
/// The compile step (optional): see [`BuildSpec`].
pub const BUILD_FILE: &str = nest_package::BUILD_FILE;
/// Where the compile step sees the tree it compiles in.
pub const COMPILE_DIR: &str = "/src";
/// The default compile timeout.
const DEFAULT_COMPILE_SECS: u64 = 1800;
/// Domain separator of the release digest. The postbox computes the same digest
/// (`marketplace_m1::release_digest`); a shared test vector keeps the two in step.
pub const RELEASE_DIGEST_CONTEXT: &str = "pigeonpost-nest-release-v1";
/// Largest single file the build plane reads from a source tree.
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Most files a source tree (or a vendored tree) may hold.
const MAX_FILES: usize = 20_000;

/// One operation a release offers, and the fields of its input. Every field is a string.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub name: String,
    pub version: String,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub name: String,
    pub max_chars: usize,
    pub required: bool,
}

/// `nest-release.toml`: what a release says it is and may do. The static scans hold it to this.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub title: String,
    pub summary: String,
    pub licence: String,
    pub entrypoint: String,
    pub listing_address: String,
    pub egress: Vec<String>,
    pub capabilities: Vec<String>,
    pub syscalls: Vec<String>,
    pub external_writes: bool,
    pub operations: Vec<Operation>,
    /// Package name → the SPDX expression its own manifest declares.
    pub licences: BTreeMap<String, String>,
    /// The installation configuration schema (marketplace M2), a JSON Schema subset checked by
    /// `pigeonpost_core::nest_package`. Absent: the release takes no configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_schema: Option<Value>,
}

impl Manifest {
    /// Parse `nest-release.toml` and hold it to the shared package rules
    /// (`pigeonpost_core::nest_package::validate_manifest`). Only malformed manifests are refused
    /// here; what the pool does not offer (capabilities, syscalls, egress, external writes) is the
    /// deterministic scan's to block, with evidence.
    pub fn parse(text: &str) -> Result<Manifest, String> {
        let raw: toml::Value = toml::from_str(text).map_err(|e| format!("{MANIFEST_FILE}: {e}"))?;
        let json = serde_json::to_value(&raw).map_err(|e| format!("{MANIFEST_FILE}: {e}"))?;
        let errors = nest_package::errors(&nest_package::validate_manifest(&json));
        if !errors.is_empty() {
            let lines: Vec<String> = errors.iter().map(|i| i.to_string()).collect();
            return Err(format!("{MANIFEST_FILE}: {}", lines.join("; ")));
        }
        toml::from_str(text).map_err(|e| format!("{MANIFEST_FILE}: {e}"))
    }
}

/// The contract's `operation` pattern: lower-case dotted words.
pub fn operation_name_ok(name: &str) -> bool {
    nest_package::operation_name_ok(name)
}

/// The contract's `operation_version` pattern: `major.minor[.patch]`.
pub fn operation_version_ok(v: &str) -> bool {
    nest_package::operation_version_ok(v)
}

/// `nest-build.toml`: the compile step. `command` runs in [`COMPILE_DIR`] under the build runtime
/// with no network, a read-only root and no capabilities; `image` is the toolchain, one the build
/// plane offers (unset: its toolchain for the release's language).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildSpec {
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

impl BuildSpec {
    /// Parse `nest-build.toml` and hold it to `pigeonpost_core::nest_package::validate_build_spec`.
    pub fn parse(text: &str) -> Result<BuildSpec, String> {
        let raw: toml::Value = toml::from_str(text).map_err(|e| format!("{BUILD_FILE}: {e}"))?;
        let json = serde_json::to_value(&raw).map_err(|e| format!("{BUILD_FILE}: {e}"))?;
        let errors = nest_package::validate_build_spec(&json);
        if !errors.is_empty() {
            let lines: Vec<String> = errors.iter().map(|i| i.to_string()).collect();
            return Err(format!("{BUILD_FILE}: {}", lines.join("; ")));
        }
        serde_json::from_value(json).map_err(|e| format!("{BUILD_FILE}: {e}"))
    }

    /// The release's compile step, if it has one.
    pub fn read(src: &Path) -> Result<Option<BuildSpec>, String> {
        match std::fs::read_to_string(src.join(BUILD_FILE)) {
            Ok(text) => BuildSpec::parse(&text).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{BUILD_FILE}: {e}")),
        }
    }
}

/// The Containerfile as an image assembly ([`nest_package::validate_containerfile`]), on a base
/// that is not itself a release image of this build plane (`release_images`).
pub fn containerfile_assembly(text: &str, release_images: &str) -> Result<(), String> {
    let issues = nest_package::validate_containerfile(text);
    if !issues.is_empty() {
        let lines: Vec<String> = issues.iter().map(|i| i.to_string()).collect();
        return Err(lines.join("; "));
    }
    let pinned = format!("{release_images}@");
    let tagged = format!("{release_images}:");
    if text
        .lines()
        .any(|l| l.contains(&pinned) || l.contains(&tagged))
    {
        return Err(format!(
            "{CONTAINERFILE}: a release image is not a base image; use a pinned runtime base"
        ));
    }
    Ok(())
}

/// One regular file of a tree, by its path relative to the root (with `/` separators).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub path: String,
    pub sha256: String,
    pub len: u64,
}

/// Every regular file under `root`, sorted by path. Symlinks, devices and sockets are refused,
/// so the digest always describes bytes that are really in the tree.
pub fn read_tree(root: &Path) -> Result<Vec<SourceFile>, String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            let rel = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_str()
                .ok_or("a path that is not UTF-8")?
                .replace('\\', "/");
            if kind.is_symlink() {
                return Err(format!("{rel}: symlinks are not allowed in a source tree"));
            } else if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() {
                let len = entry.metadata().map_err(|e| e.to_string())?.len();
                if len > MAX_FILE_BYTES {
                    return Err(format!("{rel}: larger than {MAX_FILE_BYTES} bytes"));
                }
                let bytes = std::fs::read(&path).map_err(|e| format!("{rel}: {e}"))?;
                out.push(SourceFile {
                    path: rel,
                    sha256: crate::util::sha256_hex(&bytes),
                    len,
                });
                if out.len() > MAX_FILES {
                    return Err("too many files".into());
                }
            } else {
                return Err(format!("{rel}: not a regular file"));
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// `sha256:` over `path NUL sha256 NUL len LF` for each file in path order.
pub fn tree_digest(files: &[SourceFile]) -> String {
    let mut h = Sha256::new();
    for f in files {
        h.update(f.path.as_bytes());
        h.update([0]);
        h.update(f.sha256.as_bytes());
        h.update([0]);
        h.update(f.len.to_string().as_bytes());
        h.update(b"\n");
    }
    format!("sha256:{}", crate::util::hex(&h.finalize()))
}

pub fn digest_of(bytes: &[u8]) -> String {
    format!("sha256:{}", crate::util::sha256_hex(bytes))
}

/// One `[[package]]` of `Cargo.lock`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LockedPackage {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub checksum: Option<String>,
}

#[derive(Deserialize)]
struct Lock {
    #[serde(default)]
    package: Vec<LockedPackage>,
}

pub fn locked_packages(cargo_lock: &str) -> Result<Vec<LockedPackage>, String> {
    let lock: Lock = toml::from_str(cargo_lock).map_err(|e| format!("Cargo.lock: {e}"))?;
    for p in &lock.package {
        if p.source.is_some() && p.checksum.is_none() {
            return Err(format!(
                "Cargo.lock: {} has no checksum; git and path sources are not built",
                p.name
            ));
        }
        if let Some(source) = &p.source {
            if source != "registry+https://github.com/rust-lang/crates.io-index" {
                return Err(format!(
                    "Cargo.lock: {} comes from {source}, not crates.io",
                    p.name
                ));
            }
        }
    }
    Ok(lock.package)
}

/// Every `FROM` names a base image pinned by digest, directly or through an `ARG` default, or an
/// earlier stage of the same file.
pub fn containerfile_pinned(text: &str) -> Result<(), String> {
    let mut args: BTreeMap<String, String> = BTreeMap::new();
    let mut stages: Vec<String> = Vec::new();
    let mut froms = 0;
    for raw in text.lines() {
        let line = raw.trim();
        let mut words = line.split_whitespace();
        let Some(keyword) = words.next() else {
            continue;
        };
        match keyword.to_ascii_uppercase().as_str() {
            "ARG" => {
                if let Some((k, v)) = words.next().and_then(|kv| kv.split_once('=')) {
                    args.insert(k.to_string(), v.to_string());
                }
            }
            "FROM" => {
                froms += 1;
                let rest: Vec<&str> = words.filter(|w| !w.starts_with("--")).collect();
                let image = rest.first().copied().ok_or("FROM without an image")?;
                let resolved = match image
                    .strip_prefix("${")
                    .and_then(|v| v.strip_suffix('}'))
                    .or_else(|| image.strip_prefix('$'))
                {
                    Some(var) => args
                        .get(var)
                        .cloned()
                        .ok_or(format!("FROM {image}: no ARG default"))?,
                    None => image.to_string(),
                };
                if !stages.contains(&resolved) && !pinned(&resolved) {
                    return Err(format!("FROM {resolved}: not pinned by digest"));
                }
                if let (Some(as_kw), Some(name)) = (rest.get(1), rest.get(2)) {
                    if as_kw.eq_ignore_ascii_case("as") {
                        stages.push((*name).to_string());
                    }
                }
            }
            _ => {}
        }
    }
    if froms == 0 {
        return Err("no FROM".into());
    }
    Ok(())
}

fn pinned(image: &str) -> bool {
    nest_package::image_pinned(image)
}

/// The SBOM, CycloneDX 1.5 JSON: the application, every locked package with its declared licence
/// and checksum, and every source file with its SHA-256. No timestamps, so the same tree always
/// gives the same bytes.
pub fn sbom(
    manifest: &Manifest,
    packages: &[LockedPackage],
    files: &[SourceFile],
    source_digest: &str,
) -> String {
    sbom_for(Ecosystem::Rust, manifest, packages, files, source_digest)
}

/// [`sbom`] for any ecosystem: packages carry its purl type and their pinned hashes (cargo and
/// pip: SHA-256 hex; npm: the lockfile's SHA-512 integrity, as hex).
pub fn sbom_for(
    eco: Ecosystem,
    manifest: &Manifest,
    packages: &[LockedPackage],
    files: &[SourceFile],
    source_digest: &str,
) -> String {
    let purl = eco.purl_type();
    let serial = {
        let h = crate::util::sha256_hex(source_digest.as_bytes());
        format!(
            "urn:uuid:{}-{}-{}-{}-{}",
            &h[0..8],
            &h[8..12],
            &h[12..16],
            &h[16..20],
            &h[20..32]
        )
    };
    let licence = |name: &str| -> Value {
        match manifest.licences.get(name) {
            Some(expr) => json!([{ "expression": expr }]),
            None => json!([]),
        }
    };
    let mut components: Vec<Value> = packages
        .iter()
        .filter(|p| p.source.is_some())
        .map(|p| {
            let mut c = json!({
                "type": "library",
                "bom-ref": format!("pkg:{purl}/{}@{}", p.name, p.version),
                "name": p.name,
                "version": p.version,
                "purl": format!("pkg:{purl}/{}@{}", p.name, p.version),
                "licenses": licence(&p.name),
            });
            if let Some(sum) = &p.checksum {
                c["hashes"] = ecosystem::sbom_hashes(eco, sum);
            }
            c
        })
        .collect();
    components.extend(files.iter().map(|f| {
        json!({
            "type": "file",
            "bom-ref": format!("file:{}", f.path),
            "name": f.path,
            "hashes": [{ "alg": "SHA-256", "content": f.sha256 }],
        })
    }));
    let bom = json!({
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "serialNumber": serial,
        "version": 1,
        "metadata": {
            "tools": { "components": [{ "type": "application", "name": "pigeonpost-nest-release" }] },
            "component": {
                "type": "application",
                "bom-ref": format!("pkg:{purl}/{}@{}", manifest.name, manifest.version),
                "name": manifest.name,
                "version": manifest.version,
                "licenses": [{ "expression": manifest.licence }],
                "hashes": [{ "alg": "SHA-256", "content": source_digest.trim_start_matches("sha256:") }],
            },
        },
        "components": components,
    });
    canonical_json(&bom)
}

/// JSON with object keys sorted at every level, no whitespace. Both crates use this form for
/// anything that is hashed, so a digest never depends on map ordering.
pub fn canonical_json(v: &Value) -> String {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let inner: Vec<String> = keys
                .iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        Value::String((*k).clone()),
                        canonical_json(&map[k.as_str()])
                    )
                })
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", inner.join(","))
        }
        other => other.to_string(),
    }
}

/// The one digest that identifies a release.
pub fn release_digest(
    source: &str,
    vendor: &str,
    manifest: &str,
    sbom: &str,
    image: &str,
) -> String {
    let text = format!(
        "{RELEASE_DIGEST_CONTEXT}\nsource={source}\nvendor={vendor}\nmanifest={manifest}\nsbom={sbom}\nimage={image}\n"
    );
    digest_of(text.as_bytes())
}

/// What the build plane hands to the postbox (`POST /internal/v1/marketplace/builds`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BuildRecord {
    pub manifest: Manifest,
    pub manifest_digest: String,
    pub source_digest: String,
    pub vendor_digest: String,
    /// The SBOM bytes exactly as hashed.
    pub sbom: String,
    pub sbom_digest: String,
    /// `name@sha256:…`, as the marketplace pool runs it.
    pub image: String,
    pub image_digest: String,
    pub release_digest: String,
    /// How it was built, for the evidence: runtime, rootless, network.
    pub isolation: Value,
}

/// How the build plane runs.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildConfig {
    /// OCI runtime for both steps: `runsc` (gVisor) or `crun`.
    pub runtime: String,
    /// The only network the fetch step may use. The build step has none.
    pub fetch_network: String,
    /// The toolchain image for the fetch step, pinned by digest (the release's own build stage
    /// uses the image its Containerfile pins).
    pub fetch_image: String,
    /// Local image name the build is tagged with; the digest comes from podman.
    pub image_name: String,
    /// The fetch image for Node releases (`npm` inside), pinned by digest. Unset: Node releases
    /// are refused. Use the same Node as the runner images' base so `node_modules` matches.
    #[serde(default)]
    pub node_fetch_image: Option<String>,
    /// The fetch image for Python releases (`pip` inside), pinned by digest. Unset: Python
    /// releases are refused. Use the same Python as the runner images' base so the wheels match.
    #[serde(default)]
    pub python_fetch_image: Option<String>,
    /// Hardening: resource limits for both steps, so a build cannot starve a shared host (the
    /// build plane may share its host with model serving). Unset: no podman-level limit.
    #[serde(default)]
    pub memory_mib: Option<u64>,
    /// Thousandths of one core.
    #[serde(default)]
    pub cpu_millis: Option<u64>,
    #[serde(default)]
    pub pids: Option<u64>,
    /// More toolchain images a release's `nest-build.toml` may name, each pinned by digest and
    /// loaded into the build user's storage. The fetch images are always offered.
    #[serde(default)]
    pub toolchain_images: Vec<String>,
    /// The compile step's time limit.
    #[serde(default = "default_compile_secs")]
    pub compile_timeout_secs: u64,
}

fn default_compile_secs() -> u64 {
    DEFAULT_COMPILE_SECS
}

impl BuildConfig {
    /// `--memory`, CPU quota and `--pids-limit` flags for `podman run`.
    pub fn limit_args(&self) -> Vec<String> {
        let mut out = self.build_limit_args();
        if let Some(p) = self.pids {
            out.push(format!("--pids-limit={p}"));
        }
        out
    }

    /// [`Self::limit_args`] for `podman build`, which has no `--pids-limit`.
    pub fn build_limit_args(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(m) = self.memory_mib {
            out.push(format!("--memory={m}m"));
            out.push(format!("--memory-swap={m}m"));
        }
        if let Some(c) = self.cpu_millis {
            out.push("--cpu-period=100000".into());
            out.push(format!("--cpu-quota={}", c.max(10) * 100));
        }
        out
    }

    /// The toolchain image the compile step of an `eco` release runs in: the one it names, if this
    /// build plane offers it, or the build plane's own for the language.
    pub fn toolchain_image(&self, eco: Ecosystem, named: Option<&str>) -> Result<String, String> {
        let default = match eco {
            Ecosystem::Rust => Some(&self.fetch_image),
            Ecosystem::Node => self.node_fetch_image.as_ref(),
            Ecosystem::Python => self.python_fetch_image.as_ref(),
        };
        match named {
            None => default
                .cloned()
                .ok_or_else(|| format!("no toolchain for {} releases here", eco.name())),
            Some(image) => {
                let offered = std::iter::once(&self.fetch_image)
                    .chain(self.node_fetch_image.iter())
                    .chain(self.python_fetch_image.iter())
                    .chain(self.toolchain_images.iter())
                    .any(|i| i == image);
                if offered {
                    Ok(image.to_string())
                } else {
                    Err(format!(
                        "{BUILD_FILE}: image {image} is not a toolchain this build plane offers"
                    ))
                }
            }
        }
    }
}

impl BuildConfig {
    pub fn check(&self) -> Result<(), String> {
        if !matches!(self.runtime.as_str(), "runsc" | "crun") {
            return Err("build runtime: runsc or crun".into());
        }
        if !pinned(&self.fetch_image) {
            return Err("fetch_image must be pinned by digest".into());
        }
        for (key, image) in [
            ("node_fetch_image", &self.node_fetch_image),
            ("python_fetch_image", &self.python_fetch_image),
        ] {
            if image.as_deref().is_some_and(|i| !pinned(i)) {
                return Err(format!("{key} must be pinned by digest"));
            }
        }
        if !self.toolchain_images.iter().all(|i| pinned(i)) {
            return Err("toolchain_images must be pinned by digest".into());
        }
        if self.compile_timeout_secs == 0 {
            return Err("compile_timeout_secs: more than zero".into());
        }
        if self.fetch_network.is_empty() || matches!(self.fetch_network.as_str(), "host" | "none") {
            return Err("fetch_network: a named podman network of its own".into());
        }
        if self.image_name.is_empty()
            || self.image_name.contains('@')
            || self.image_name.contains(':')
        {
            return Err("image_name: a repository name without tag or digest".into());
        }
        Ok(())
    }
}

/// The fetch step: `cargo vendor` from a read-only copy of the source tree into `vendor`.
pub fn fetch_args(cfg: &BuildConfig, src: &Path, vendor: &Path) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--pull=never".into(),
        format!("--runtime={}", cfg.runtime),
        format!("--network={}", cfg.fetch_network),
        "--cap-drop=ALL".into(),
        "--security-opt=no-new-privileges".into(),
        "--read-only".into(),
        "--tmpfs=/tmp:rw,nosuid,nodev,size=512m".into(),
        "--env=CARGO_HOME=/tmp/cargo".into(),
        format!("--volume={}:/src:ro", src.display()),
        format!("--volume={}:/vendor:rw", vendor.display()),
        "--workdir=/src".into(),
    ];
    args.extend(cfg.limit_args());
    args.extend([
        entrypoint("cargo"),
        cfg.fetch_image.clone(),
        "vendor".into(),
        "--locked".into(),
        "--versioned-dirs".into(),
        "/vendor".into(),
    ]);
    args
}

/// The fetch step for a Node release: `npm ci` from the copied `package.json` and
/// `package-lock.json` in `vendor`, lifecycle scripts off, dev dependencies left out, no audit or
/// telemetry. It writes `vendor/node_modules` and nothing else that is kept.
pub fn npm_fetch_args(cfg: &BuildConfig, image: &str, vendor: &Path) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--pull=never".into(),
        format!("--runtime={}", cfg.runtime),
        format!("--network={}", cfg.fetch_network),
        "--cap-drop=ALL".into(),
        "--security-opt=no-new-privileges".into(),
        "--read-only".into(),
        "--tmpfs=/tmp:rw,nosuid,nodev,size=512m".into(),
        "--env=HOME=/tmp".into(),
        "--env=npm_config_cache=/tmp/npm-cache".into(),
        "--env=npm_config_update_notifier=false".into(),
        format!("--volume={}:/vendor:rw", vendor.display()),
        "--workdir=/vendor".into(),
    ];
    args.extend(cfg.limit_args());
    args.extend([
        entrypoint("npm"),
        image.to_string(),
        "ci".into(),
        "--ignore-scripts".into(),
        "--omit=dev".into(),
        "--no-audit".into(),
        "--no-fund".into(),
        "--registry=https://registry.npmjs.org/".into(),
    ]);
    args
}

/// The fetch step for a Python release: `pip download` in hash-checking mode, binary wheels only
/// (no sdist builds, so no publisher code runs here), no dependency resolution beyond the list.
pub fn pip_fetch_args(cfg: &BuildConfig, image: &str, src: &Path, vendor: &Path) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--pull=never".into(),
        format!("--runtime={}", cfg.runtime),
        format!("--network={}", cfg.fetch_network),
        "--cap-drop=ALL".into(),
        "--security-opt=no-new-privileges".into(),
        "--read-only".into(),
        "--tmpfs=/tmp:rw,nosuid,nodev,size=512m".into(),
        "--env=HOME=/tmp".into(),
        "--env=PIP_DISABLE_PIP_VERSION_CHECK=1".into(),
        format!("--volume={}:/src:ro", src.display()),
        format!("--volume={}:/vendor:rw", vendor.display()),
        "--workdir=/src".into(),
    ];
    args.extend(cfg.limit_args());
    args.extend([
        entrypoint("python3"),
        image.to_string(),
        "-m".into(),
        "pip".into(),
        "download".into(),
        "--require-hashes".into(),
        "--only-binary=:all:".into(),
        "--no-deps".into(),
        "--no-cache-dir".into(),
        "--index-url=https://pypi.org/simple".into(),
        "--requirement=/src/requirements.txt".into(),
        "--dest=/vendor".into(),
    ]);
    args
}

/// `--entrypoint` for one program, in podman's JSON form, so an image's own `ENTRYPOINT` (for
/// example `["git"]` in alpine/git) never runs in front of the command.
pub fn entrypoint(program: &str) -> String {
    format!("--entrypoint={}", json!([program]))
}

/// The compile step: the release's command in `context` (mounted at [`COMPILE_DIR`]), under the
/// build runtime with **no network**, a read-only root, no capabilities and a time limit.
pub fn compile_args(
    cfg: &BuildConfig,
    image: &str,
    context: &Path,
    command: &[String],
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--pull=never".into(),
        format!("--runtime={}", cfg.runtime),
        "--network=none".into(),
        "--cap-drop=ALL".into(),
        "--security-opt=no-new-privileges".into(),
        "--read-only".into(),
        "--tmpfs=/tmp:rw,nosuid,nodev,size=1024m".into(),
        "--env=HOME=/tmp".into(),
        "--env=TMPDIR=/tmp".into(),
        "--env=CARGO_HOME=/tmp/cargo".into(),
        "--env=SOURCE_DATE_EPOCH=0".into(),
        "--env=PYTHONDONTWRITEBYTECODE=1".into(),
        format!("--volume={}:{COMPILE_DIR}:rw", context.display()),
        format!("--workdir={COMPILE_DIR}"),
        format!("--timeout={}", cfg.compile_timeout_secs),
    ];
    args.extend(cfg.limit_args());
    args.push(entrypoint(&command[0]));
    args.push(image.to_string());
    args.extend(command[1..].iter().cloned());
    args
}

/// The assembly step. `--network=none` is the line that must not move. `containerfile` lives
/// outside `context`, so the compile step cannot rewrite it.
pub fn build_args(
    cfg: &BuildConfig,
    context: &Path,
    containerfile: &Path,
    tag: &str,
    source_digest: &str,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "build".into(),
        "--pull=never".into(),
        "--network=none".into(),
        format!("--runtime={}", cfg.runtime),
        "--no-cache".into(),
        "--cap-drop=ALL".into(),
        "--security-opt=no-new-privileges".into(),
        // Reproducibility (R-62): a fixed creation time for the image and every file in its
        // layers, so the same source and vendored tree give the same image digest.
        "--timestamp=0".into(),
        format!("--label=dev.pigeonpost.release.source={source_digest}"),
        format!("--file={}", containerfile.display()),
        format!("--tag={tag}"),
    ];
    args.extend(cfg.build_limit_args());
    args.push(context.display().to_string());
    args
}

/// The local tag a build of `source_digest` gets.
pub fn build_tag(cfg: &BuildConfig, source_digest: &str) -> String {
    format!(
        "{}:{}",
        cfg.image_name,
        &source_digest.trim_start_matches("sha256:")[..12]
    )
}

/// The vendored-source configuration written into the build context.
pub const CARGO_VENDOR_CONFIG: &str = "[source.crates-io]\nreplace-with = \"vendored-sources\"\n\n[source.vendored-sources]\ndirectory = \"vendor\"\n\n[net]\noffline = true\n";

pub struct Builder<P: Podman> {
    pub cfg: BuildConfig,
    pub podman: P,
}

fn copy_tree(from: &Path, files: &[SourceFile], to: &Path) -> Result<(), String> {
    for f in files {
        let dest = to.join(&f.path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::copy(from.join(&f.path), &dest).map_err(|e| format!("{}: {e}", f.path))?;
    }
    Ok(())
}

/// Most entries the compile step may leave in the tree the image is assembled from.
const MAX_COMPILED_ENTRIES: usize = 500_000;

/// After the compile step: no special files, and no setuid, setgid or sticky bits, in what the
/// image will be assembled from; every directory readable by the build plane again.
#[cfg(unix)]
pub fn seal_tree(root: &Path) -> Result<(), String> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    let mut stack = vec![root.to_path_buf()];
    let mut seen = 0usize;
    while let Some(dir) = stack.pop() {
        let mode = std::fs::symlink_metadata(&dir)
            .map_err(|e| e.to_string())?
            .permissions()
            .mode();
        std::fs::set_permissions(
            &dir,
            std::fs::Permissions::from_mode((mode & 0o777) | 0o700),
        )
        .map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let entry = entry.map_err(|e| e.to_string())?;
            seen += 1;
            if seen > MAX_COMPILED_ENTRIES {
                return Err("the compile step left too many files".into());
            }
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            let kind = meta.file_type();
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() {
                let mode = meta.permissions().mode();
                if mode & 0o7000 != 0 {
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode & 0o777))
                        .map_err(|e| e.to_string())?;
                }
            } else if kind.is_fifo()
                || kind.is_socket()
                || kind.is_block_device()
                || kind.is_char_device()
            {
                return Err(format!(
                    "{}: the compile step left a special file",
                    path.strip_prefix(root).unwrap_or(&path).display()
                ));
            }
            // Symlinks stay: buildah resolves COPY sources inside the context only.
        }
    }
    Ok(())
}

/// The build plane runs on Linux; elsewhere (tests on other hosts) there is nothing to seal.
#[cfg(not(unix))]
pub fn seal_tree(_root: &Path) -> Result<(), String> {
    Ok(())
}

impl<P: Podman> Builder<P> {
    /// Build `src` into an image. `work` is an empty scratch directory for this build only.
    pub fn build(&self, src: &Path, work: &Path) -> Result<BuildRecord, String> {
        self.cfg.check()?;
        // Rootless: the build never runs with root on the host.
        let rootless = self
            .podman
            .run(&["info", "--format", "{{.Host.Security.Rootless}}"])?;
        if rootless.trim() != "true" {
            return Err("the build plane runs rootless podman only".into());
        }
        let manifest_text = std::fs::read_to_string(src.join(MANIFEST_FILE))
            .map_err(|e| format!("{MANIFEST_FILE}: {e}"))?;
        let manifest = Manifest::parse(&manifest_text)?;
        let containerfile = std::fs::read_to_string(src.join(CONTAINERFILE))
            .map_err(|e| format!("{CONTAINERFILE}: {e}"))?;
        containerfile_assembly(&containerfile, &self.cfg.image_name)?;
        let eco = ecosystem::detect(src)?;
        let spec = BuildSpec::read(src)?;
        let toolchain = match &spec {
            Some(s) => Some(self.cfg.toolchain_image(eco, s.image.as_deref())?),
            None => None,
        };
        let packages = ecosystem::locked(eco, src)?;
        let files = read_tree(src)?;
        if files.iter().any(|f| Ecosystem::reserved(&f.path)) {
            return Err(
                "vendor/, target/, .cargo/, node_modules/ and wheelhouse/ are made by the build plane"
                    .into(),
            );
        }
        let source_digest = tree_digest(&files);

        // 1. fetch, the only step with a network.
        let vendor = work.join("vendor");
        std::fs::create_dir_all(&vendor).map_err(|e| e.to_string())?;
        let fetched = if packages.iter().any(|p| p.source.is_some()) {
            self.fetch(eco, src, &vendor)?
        } else {
            vendor.clone()
        };
        let vendored = read_tree(&fetched)?;
        let vendor_digest = tree_digest(&vendored);

        // 2. compile, with none, in gVisor: the only step that runs publisher code.
        let context = work.join("context");
        copy_tree(src, &files, &context)?;
        copy_tree(&fetched, &vendored, &context.join(eco.vendor_dir()))?;
        if eco == Ecosystem::Rust {
            std::fs::create_dir_all(context.join(".cargo")).map_err(|e| e.to_string())?;
            std::fs::write(context.join(".cargo/config.toml"), CARGO_VENDOR_CONFIG)
                .map_err(|e| e.to_string())?;
        } else {
            std::fs::create_dir_all(context.join(eco.vendor_dir())).map_err(|e| e.to_string())?;
        }
        if let (Some(spec), Some(image)) = (&spec, &toolchain) {
            let args = compile_args(&self.cfg, image, &context, &spec.command);
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            self.podman
                .run(&refs)
                .map_err(|e| format!("the compile step ({BUILD_FILE}) failed: {e}"))?;
            seal_tree(&context)?;
        }

        // 3. assemble, with none and nothing running: the Containerfile as it is in the source
        // tree, kept outside the context the compile step could write to.
        let assembly = work.join("Containerfile.assemble");
        std::fs::write(&assembly, &containerfile).map_err(|e| e.to_string())?;
        let tag = build_tag(&self.cfg, &source_digest);
        let args = build_args(&self.cfg, &context, &assembly, &tag, &source_digest);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        self.podman.run(&refs)?;
        let image_digest = self
            .podman
            .run(&["image", "inspect", "--format", "{{.Digest}}", &tag])?
            .trim()
            .to_string();
        if !image_digest
            .strip_prefix("sha256:")
            .is_some_and(|d| d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(format!("podman reported no digest for {tag}"));
        }

        let sbom = sbom_for(eco, &manifest, &packages, &files, &source_digest);
        let sbom_digest = digest_of(sbom.as_bytes());
        let manifest_digest = digest_of(manifest_text.as_bytes());
        let release_digest = release_digest(
            &source_digest,
            &vendor_digest,
            &manifest_digest,
            &sbom_digest,
            &image_digest,
        );
        Ok(BuildRecord {
            image: format!("{}@{image_digest}", self.cfg.image_name),
            manifest,
            manifest_digest,
            source_digest,
            vendor_digest,
            sbom,
            sbom_digest,
            image_digest,
            release_digest,
            isolation: json!({
                "plane": "nests-1, separate rootless podman build",
                "runtime": self.cfg.runtime,
                "rootless": true,
                "fetch_network": self.cfg.fetch_network,
                "build_network": "none",
                "ecosystem": eco.name(),
                "compile": compile_evidence(&self.cfg, toolchain.as_deref()),
            }),
        })
    }

    /// The fetch step of `eco` into `vendor`; returns the directory whose tree is the vendored
    /// dependencies (`vendor` itself, or `vendor/node_modules`).
    fn fetch(&self, eco: Ecosystem, src: &Path, vendor: &Path) -> Result<PathBuf, String> {
        let run = |args: Vec<String>| -> Result<String, String> {
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            self.podman.run(&refs)
        };
        match eco {
            Ecosystem::Rust => {
                run(fetch_args(&self.cfg, src, vendor))?;
                Ok(vendor.to_path_buf())
            }
            Ecosystem::Node => {
                let image =
                    self.cfg.node_fetch_image.as_deref().ok_or(
                        "this build plane does not build Node releases (no node_fetch_image)",
                    )?;
                // npm ci reads both files from its working directory; only node_modules is kept.
                for f in [ecosystem::NPM_PACKAGE, ecosystem::NPM_LOCK] {
                    std::fs::copy(src.join(f), vendor.join(f)).map_err(|e| format!("{f}: {e}"))?;
                }
                run(npm_fetch_args(&self.cfg, image, vendor))?;
                for f in [ecosystem::NPM_PACKAGE, ecosystem::NPM_LOCK] {
                    let _ = std::fs::remove_file(vendor.join(f));
                }
                let modules = vendor.join("node_modules");
                std::fs::create_dir_all(&modules).map_err(|e| e.to_string())?;
                let lock = std::fs::read_to_string(src.join(ecosystem::NPM_LOCK))
                    .map_err(|e| e.to_string())?;
                ecosystem::check_node_modules(vendor, &lock)?;
                Ok(modules)
            }
            Ecosystem::Python => {
                let image = self.cfg.python_fetch_image.as_deref().ok_or(
                    "this build plane does not build Python releases (no python_fetch_image)",
                )?;
                run(pip_fetch_args(&self.cfg, image, src, vendor))?;
                ecosystem::check_wheelhouse(vendor, &ecosystem::locked(eco, src)?)?;
                Ok(vendor.to_path_buf())
            }
        }
    }
}

/// How the compile step ran, for the evidence (`null`: the release has none).
pub fn compile_evidence(cfg: &BuildConfig, toolchain: Option<&str>) -> Value {
    match toolchain {
        None => Value::Null,
        Some(image) => json!({
            "image": image,
            "runtime": cfg.runtime,
            "network": "none",
            "root": "read-only",
            "capabilities": [],
            "assembly": "no RUN steps",
        }),
    }
}

/// Where the reference release's source tree lives in this repository.
pub fn reference_source() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../deploy/nests/reference/hr-policy-qa")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A podman that answers `info` and `image inspect` and records the rest.
    #[derive(Clone, Default)]
    struct FakePodman {
        calls: Arc<Mutex<Vec<Vec<String>>>>,
        rootful: bool,
    }

    impl Podman for FakePodman {
        fn run(&self, args: &[&str]) -> Result<String, String> {
            self.calls
                .lock()
                .unwrap()
                .push(args.iter().map(|s| s.to_string()).collect());
            match args.first().copied() {
                Some("info") => Ok(if self.rootful { "false" } else { "true" }.into()),
                Some("image") => Ok(format!("sha256:{}\n", "ab".repeat(32))),
                _ => Ok(String::new()),
            }
        }
    }

    fn config() -> BuildConfig {
        BuildConfig {
            runtime: "runsc".into(),
            fetch_network: "nests-fetch".into(),
            fetch_image: format!(
                "docker.io/library/rust:1.91.1-slim@sha256:{}",
                "1".repeat(64)
            ),
            image_name: "localhost/pigeonpost-release-hr-policy-qa".into(),
            node_fetch_image: Some(format!(
                "docker.io/library/node:22.21.1-bookworm-slim@sha256:{}",
                "2".repeat(64)
            )),
            python_fetch_image: Some(format!(
                "docker.io/library/python:3.12.12-slim-bookworm@sha256:{}",
                "3".repeat(64)
            )),
            memory_mib: None,
            cpu_millis: None,
            pids: Some(256),
            toolchain_images: vec![REFERENCE_TOOLCHAIN.into()],
            compile_timeout_secs: 900,
        }
    }

    /// The toolchain the reference release's `nest-build.toml` names.
    const REFERENCE_TOOLCHAIN: &str = "docker.io/library/rust:1.91.1-slim-bookworm@sha256:8514999d4786ef12efe89239e86b3d0a021b94b9d35108c8efe6c79ca7dc1a65";

    fn scratch() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn the_reference_manifest_parses_and_is_minimal() {
        let text = std::fs::read_to_string(reference_source().join(MANIFEST_FILE)).unwrap();
        let m = Manifest::parse(&text).unwrap();
        assert_eq!(m.name, "hr-policy-qa");
        assert_eq!(m.egress, vec!["broker".to_string()]);
        assert!(m.capabilities.is_empty() && m.syscalls.is_empty() && !m.external_writes);
        assert_eq!(m.operations[0].name, "hr.policy.ask");
    }

    #[test]
    fn manifests_refuse_unknown_fields_and_malformed_operations() {
        let text = std::fs::read_to_string(reference_source().join(MANIFEST_FILE)).unwrap();
        assert!(Manifest::parse(&format!("privileged = true\n{text}")).is_err());
        let bad = text.replace("name = \"hr.policy.ask\"", "name = \"HR Ask\"");
        assert!(Manifest::parse(&bad).is_err());
        let unbounded = text.replace("max_chars = 2000", "max_chars = 0");
        assert!(Manifest::parse(&unbounded).is_err());
    }

    #[test]
    fn the_reference_containerfile_pins_every_base_image() {
        let text = std::fs::read_to_string(reference_source().join(CONTAINERFILE)).unwrap();
        containerfile_pinned(&text).unwrap();
        for bad in [
            "FROM debian:bookworm-slim\n",
            "ARG X=debian:latest\nFROM ${X}\n",
            "FROM ${UNSET}\n",
            "RUN true\n",
        ] {
            assert!(containerfile_pinned(bad).is_err(), "{bad}");
        }
        let staged = format!("FROM a@sha256:{} AS build\nFROM build\n", "0".repeat(64));
        containerfile_pinned(&staged).unwrap();
    }

    #[test]
    fn the_lockfile_must_be_crates_io_with_checksums() {
        let lock = std::fs::read_to_string(reference_source().join("Cargo.lock")).unwrap();
        let packages = locked_packages(&lock).unwrap();
        assert!(packages.iter().any(|p| p.name == "serde_json"));
        let git = "[[package]]\nname = \"x\"\nversion = \"1.0.0\"\nsource = \"git+https://example.invalid/x\"\nchecksum = \"00\"\n";
        assert!(locked_packages(git).is_err());
        let unsummed = "[[package]]\nname = \"x\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n";
        assert!(locked_packages(unsummed).is_err());
    }

    // Uses a symlink, which needs `std::os::unix`.
    #[cfg(unix)]
    #[test]
    fn tree_digests_are_stable_and_refuse_symlinks() {
        let dir = scratch();
        std::fs::create_dir_all(dir.path().join("a")).unwrap();
        std::fs::write(dir.path().join("a/x.txt"), "one").unwrap();
        std::fs::write(dir.path().join("b.txt"), "two").unwrap();
        let first = tree_digest(&read_tree(dir.path()).unwrap());
        assert_eq!(first, tree_digest(&read_tree(dir.path()).unwrap()));
        std::fs::write(dir.path().join("b.txt"), "changed").unwrap();
        assert_ne!(first, tree_digest(&read_tree(dir.path()).unwrap()));
        std::os::unix::fs::symlink("/etc/hostname", dir.path().join("link")).unwrap();
        assert!(read_tree(dir.path()).is_err());
    }

    #[test]
    fn the_release_digest_matches_the_postbox_test_vector() {
        // The same vector is asserted in the postbox (`marketplace_m1`).
        let d = release_digest(
            "sha256:01",
            "sha256:02",
            "sha256:03",
            "sha256:04",
            "sha256:05",
        );
        assert_eq!(
            d,
            digest_of(
                b"pigeonpost-nest-release-v1\nsource=sha256:01\nvendor=sha256:02\nmanifest=sha256:03\nsbom=sha256:04\nimage=sha256:05\n"
            )
        );
    }

    #[test]
    fn a_build_fetches_with_a_network_then_builds_without_one() {
        let work = scratch();
        let podman = FakePodman::default();
        let builder = Builder {
            cfg: config(),
            podman: podman.clone(),
        };
        let record = builder.build(&reference_source(), work.path()).unwrap();
        let calls = podman.calls.lock().unwrap().clone();
        let fetch = calls.iter().find(|c| c[0] == "run").expect("a fetch step");
        let build = calls
            .iter()
            .find(|c| c[0] == "build")
            .expect("a build step");
        assert!(fetch.contains(&"--network=nests-fetch".to_string()));
        assert!(fetch.contains(&"--runtime=runsc".to_string()));
        assert!(fetch.iter().any(|a| a.ends_with(":/src:ro")), "{fetch:?}");
        for must in [
            "--network=none",
            "--pull=never",
            "--runtime=runsc",
            "--no-cache",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
        ] {
            assert!(
                build.contains(&must.to_string()),
                "missing {must}: {build:?}"
            );
        }
        assert!(!build
            .iter()
            .any(|a| a.starts_with("--network=") && a != "--network=none"));
        assert!(!build
            .iter()
            .any(|a| a.contains("privileged") || a.contains("--cap-add")));
        // The vendored-source configuration is in the context, so cargo cannot reach a registry.
        let cfg = std::fs::read_to_string(work.path().join("context/.cargo/config.toml")).unwrap();
        assert!(cfg.contains("offline = true"), "{cfg}");

        assert_eq!(record.image_digest, format!("sha256:{}", "ab".repeat(32)));
        assert_eq!(
            record.image,
            format!(
                "localhost/pigeonpost-release-hr-policy-qa@{}",
                record.image_digest
            )
        );
        assert_eq!(record.sbom_digest, digest_of(record.sbom.as_bytes()));
        assert_eq!(
            record.release_digest,
            release_digest(
                &record.source_digest,
                &record.vendor_digest,
                &record.manifest_digest,
                &record.sbom_digest,
                &record.image_digest
            )
        );
        // The SBOM is CycloneDX and lists every locked registry package with its licence.
        let bom: Value = serde_json::from_str(&record.sbom).unwrap();
        assert_eq!(bom["bomFormat"], "CycloneDX");
        assert_eq!(bom["specVersion"], "1.5");
        let names: Vec<&str> = bom["components"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["type"] == "library")
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"serde_json"), "{names:?}");
        assert!(bom["components"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["type"] == "library")
            .all(|c| !c["licenses"].as_array().unwrap().is_empty()));
        // The same tree builds to the same record (the fake image digest is fixed).
        let again = Builder {
            cfg: config(),
            podman: FakePodman::default(),
        }
        .build(&reference_source(), scratch().path())
        .unwrap();
        assert_eq!(again.release_digest, record.release_digest);
        assert_eq!(again.sbom, record.sbom);
    }

    /// Publisher code runs in one place only: the compile step, a `podman run` under gVisor with
    /// no network, a read-only root and no capabilities. The assembly that follows reads a copy
    /// of the Containerfile kept outside the tree the compile step could write, and `podman
    /// build` gets no `--pids-limit` (it has none).
    #[test]
    fn publisher_code_runs_only_in_the_compile_step_without_a_network() {
        let work = scratch();
        let podman = FakePodman::default();
        let record = Builder {
            cfg: config(),
            podman: podman.clone(),
        }
        .build(&reference_source(), work.path())
        .unwrap();
        let calls = podman.calls.lock().unwrap().clone();
        let order: Vec<&str> = calls.iter().map(|c| c[0].as_str()).collect();
        assert_eq!(order, ["info", "run", "run", "build", "image"], "{calls:?}");
        let compile = &calls[2];
        for must in [
            "--network=none",
            "--runtime=runsc",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--read-only",
            "--pull=never",
            "--timeout=900",
            "--pids-limit=256",
            "--entrypoint=[\"sh\"]",
        ] {
            assert!(compile.contains(&must.to_string()), "{must}: {compile:?}");
        }
        let context = work.path().join("context");
        assert!(compile.contains(&format!("--volume={}:/src:rw", context.display())));
        // The command follows the toolchain image, with its program as the entrypoint.
        let image = compile
            .iter()
            .position(|a| a == REFERENCE_TOOLCHAIN)
            .unwrap();
        assert_eq!(compile[image + 1], "-ec");
        assert!(compile[image + 2].starts_with("cargo build --release --locked --offline"));
        assert!(!compile
            .iter()
            .any(|a| a.contains("privileged") || a.contains("--cap-add")));

        let build = &calls[3];
        let assembly = work.path().join("Containerfile.assemble");
        assert!(build.contains(&format!("--file={}", assembly.display())));
        assert!(
            !build.iter().any(|a| a.starts_with("--pids-limit")),
            "{build:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&assembly).unwrap(),
            std::fs::read_to_string(reference_source().join(CONTAINERFILE)).unwrap()
        );
        assert_eq!(record.isolation["compile"]["image"], REFERENCE_TOOLCHAIN);
        assert_eq!(record.isolation["compile"]["network"], "none");
    }

    #[test]
    fn a_containerfile_that_runs_anything_or_an_unoffered_toolchain_is_refused() {
        let base = format!(
            "docker.io/library/debian:bookworm-slim@sha256:{}",
            "9".repeat(64)
        );
        let release = format!(
            "localhost/pigeonpost-release-hr-policy-qa@sha256:{}",
            "a".repeat(64)
        );
        let reference = std::fs::read_to_string(reference_source().join(CONTAINERFILE)).unwrap();
        containerfile_assembly(&reference, &config().image_name).unwrap();
        for bad in [
            format!("FROM {base}\nRUN cargo build --release\n"),
            format!("FROM {base}\nCOPY --from=build /x /x\n"),
            format!("FROM {release}\n"),
            format!("FROM {base}\nADD x /x\n"),
        ] {
            assert!(
                containerfile_assembly(&bad, &config().image_name).is_err(),
                "{bad}"
            );
        }
        // Through the builder: the old multi-stage form with a RUN step is refused before
        // anything runs.
        let src = scratch();
        for f in read_tree(&reference_source()).unwrap() {
            let dest = src.path().join(&f.path);
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            std::fs::copy(reference_source().join(&f.path), dest).unwrap();
        }
        let multi = format!(
            "FROM {REFERENCE_TOOLCHAIN} AS build\nRUN cargo build\nFROM {base}\n\
             COPY --from=build /src/target/release/hr-policy-qa /usr/local/bin/hr-policy-qa\n"
        );
        std::fs::write(src.path().join(CONTAINERFILE), multi).unwrap();
        let podman = FakePodman::default();
        let builder = Builder {
            cfg: config(),
            podman: podman.clone(),
        };
        let err = builder.build(src.path(), scratch().path()).unwrap_err();
        assert!(err.contains("RUN"), "{err}");
        // A toolchain the build plane does not offer (another release's image, say).
        std::fs::copy(
            reference_source().join(CONTAINERFILE),
            src.path().join(CONTAINERFILE),
        )
        .unwrap();
        std::fs::write(
            src.path().join(BUILD_FILE),
            format!("image = \"{release}\"\ncommand = [\"true\"]\n"),
        )
        .unwrap();
        let err = builder.build(src.path(), scratch().path()).unwrap_err();
        assert!(err.contains("not a toolchain"), "{err}");
        // Only `info` ran: nothing was fetched or compiled for either.
        assert!(podman.calls.lock().unwrap().iter().all(|c| c[0] == "info"));
        // Unset image: the build plane's toolchain for the language.
        assert_eq!(
            config().toolchain_image(Ecosystem::Rust, None).unwrap(),
            config().fetch_image
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_compiled_tree_is_sealed_before_assembly() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch();
        std::fs::create_dir_all(dir.path().join("target/release")).unwrap();
        let bin = dir.path().join("target/release/app");
        std::fs::write(&bin, "x").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o4755)).unwrap();
        std::fs::set_permissions(
            dir.path().join("target"),
            std::fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        seal_tree(dir.path()).unwrap();
        assert_eq!(
            std::fs::metadata(&bin).unwrap().permissions().mode() & 0o7777,
            0o755
        );
        let fifo = dir.path().join("pipe");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(seal_tree(dir.path()).unwrap_err().contains("special file"));
    }

    #[test]
    fn a_rootful_podman_or_a_loose_config_is_refused() {
        let rootful = Builder {
            cfg: config(),
            podman: FakePodman {
                rootful: true,
                ..FakePodman::default()
            },
        };
        let err = rootful
            .build(&reference_source(), scratch().path())
            .unwrap_err();
        assert!(err.contains("rootless"), "{err}");
        type Tweak = fn(&mut BuildConfig);
        let cases: Vec<Tweak> = vec![
            |c| c.runtime = "runc".into(),
            |c| c.fetch_network = "host".into(),
            |c| c.fetch_network = "none".into(),
            |c| c.fetch_image = "rust:latest".into(),
            |c| c.image_name = "x:latest".into(),
        ];
        for tweak in cases {
            let mut cfg = config();
            tweak(&mut cfg);
            assert!(cfg.check().is_err());
        }
    }

    #[test]
    fn canonical_json_sorts_keys_at_every_level() {
        let v = json!({"b": 1, "a": {"d": [ {"z": 1, "y": 2} ], "c": "x"}});
        assert_eq!(
            canonical_json(&v),
            r#"{"a":{"c":"x","d":[{"y":2,"z":1}]},"b":1}"#
        );
    }
}
