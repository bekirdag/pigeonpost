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
//! 2. **build**: `podman build --network=none --pull=never` from the source tree plus the vendored
//!    sources, with no capabilities, base images pinned by digest and nothing cached. Nothing in
//!    this step can reach a network.
//!
//! What comes out is a [`BuildRecord`]: the digests of the source tree, the vendored sources, the
//! manifest, the SBOM (CycloneDX 1.5 JSON, written here from `Cargo.lock` and the file list, so it
//! is deterministic and needs no network) and the image, and one **release digest** over all of
//! them. Staff attest that digest; nothing else identifies a release.

use crate::sandbox::Podman;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const MANIFEST_FILE: &str = "nest-release.toml";
pub const CONTAINERFILE: &str = "Containerfile";
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
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Manifest, String> {
        let m: Manifest = toml::from_str(text).map_err(|e| format!("{MANIFEST_FILE}: {e}"))?;
        let slug_ok = |s: &str| {
            !s.is_empty()
                && s.len() <= 64
                && s.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        };
        if !slug_ok(&m.name) {
            return Err("name: 1-64 of a-z 0-9 -".into());
        }
        if !semver_ok(&m.version) {
            return Err("version: major.minor.patch".into());
        }
        if m.operations.is_empty() {
            return Err("a release offers at least one operation".into());
        }
        for op in &m.operations {
            if !operation_name_ok(&op.name) || !operation_version_ok(&op.version) {
                return Err(format!("operation {:?} is malformed", op.name));
            }
            if op.fields.is_empty() || op.fields.iter().any(|f| f.max_chars == 0) {
                return Err(format!("operation {:?} needs bounded fields", op.name));
            }
        }
        if !m.listing_address.starts_with('/') || m.listing_address.len() > 128 {
            return Err("listing_address: an absolute address".into());
        }
        Ok(m)
    }
}

fn semver_ok(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 9 && p.bytes().all(|b| b.is_ascii_digit()))
}

/// The contract's `operation` pattern: lower-case dotted words.
pub fn operation_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 80
        && name.split('.').all(|w| {
            w.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
                && w.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
}

/// The contract's `operation_version` pattern: `major.minor[.patch]`.
pub fn operation_version_ok(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    (2..=3).contains(&parts.len())
        && v.len() <= 32
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
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
    image.split_once("@sha256:").is_some_and(|(name, digest)| {
        !name.is_empty() && digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())
    })
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
                "bom-ref": format!("pkg:cargo/{}@{}", p.name, p.version),
                "name": p.name,
                "version": p.version,
                "purl": format!("pkg:cargo/{}@{}", p.name, p.version),
                "licenses": licence(&p.name),
            });
            if let Some(sum) = &p.checksum {
                c["hashes"] = json!([{ "alg": "SHA-256", "content": sum }]);
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
                "bom-ref": format!("pkg:cargo/{}@{}", manifest.name, manifest.version),
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
}

impl BuildConfig {
    pub fn check(&self) -> Result<(), String> {
        if !matches!(self.runtime.as_str(), "runsc" | "crun") {
            return Err("build runtime: runsc or crun".into());
        }
        if !pinned(&self.fetch_image) {
            return Err("fetch_image must be pinned by digest".into());
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
    vec![
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
        cfg.fetch_image.clone(),
        "cargo".into(),
        "vendor".into(),
        "--locked".into(),
        "--versioned-dirs".into(),
        "/vendor".into(),
    ]
}

/// The build step. `--network=none` is the line that must not move.
pub fn build_args(
    cfg: &BuildConfig,
    context: &Path,
    tag: &str,
    source_digest: &str,
) -> Vec<String> {
    vec![
        "build".into(),
        "--pull=never".into(),
        "--network=none".into(),
        format!("--runtime={}", cfg.runtime),
        "--no-cache".into(),
        "--cap-drop=ALL".into(),
        "--security-opt=no-new-privileges".into(),
        format!("--label=dev.pigeonpost.release.source={source_digest}"),
        format!("--file={}", context.join(CONTAINERFILE).display()),
        format!("--tag={tag}"),
        context.display().to_string(),
    ]
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
        containerfile_pinned(&containerfile)?;
        let lock = std::fs::read_to_string(src.join("Cargo.lock"))
            .map_err(|e| format!("Cargo.lock: {e}"))?;
        let packages = locked_packages(&lock)?;
        let files = read_tree(src)?;
        if files.iter().any(|f| {
            f.path == "vendor"
                || f.path.starts_with("vendor/")
                || f.path.starts_with("target/")
                || f.path.starts_with(".cargo/")
        }) {
            return Err("vendor/, target/ and .cargo/ are made by the build plane".into());
        }
        let source_digest = tree_digest(&files);

        // 1. fetch, the only step with a network.
        let vendor = work.join("vendor");
        std::fs::create_dir_all(&vendor).map_err(|e| e.to_string())?;
        if packages.iter().any(|p| p.source.is_some()) {
            let args = fetch_args(&self.cfg, src, &vendor);
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            self.podman.run(&refs)?;
        }
        let vendored = read_tree(&vendor)?;
        let vendor_digest = tree_digest(&vendored);

        // 2. build, with none.
        let context = work.join("context");
        copy_tree(src, &files, &context)?;
        copy_tree(&vendor, &vendored, &context.join("vendor"))?;
        std::fs::create_dir_all(context.join(".cargo")).map_err(|e| e.to_string())?;
        std::fs::write(context.join(".cargo/config.toml"), CARGO_VENDOR_CONFIG)
            .map_err(|e| e.to_string())?;
        let tag = format!(
            "{}:{}",
            self.cfg.image_name,
            &source_digest.trim_start_matches("sha256:")[..12]
        );
        let args = build_args(&self.cfg, &context, &tag, &source_digest);
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

        let sbom = sbom(&manifest, &packages, &files, &source_digest);
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
            }),
        })
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
        }
    }

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
