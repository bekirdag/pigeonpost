//! The languages the build plane builds (marketplace hardening follow-up): Rust, Node and Python.
//!
//! Each has one lockfile that pins every dependency by content hash, and the build plane refuses
//! a tree without it:
//!
//! | Ecosystem | Lockfile | Fetch (network: the fetch network only) | Build (network: none) |
//! |---|---|---|---|
//! | Rust | `Cargo.lock` (crates.io, checksums) | `cargo vendor --locked` | `cargo build --offline` from `vendor/` |
//! | Node | `package-lock.json` v2/v3 (registry.npmjs.org, `sha512` integrity) | `npm ci --ignore-scripts --omit=dev` into `node_modules/` | the vendored `node_modules/` is copied in; no install, no scripts |
//! | Python | `requirements.txt`, every line `name==version --hash=sha256:…` | `pip download --require-hashes --only-binary=:all: --no-deps` into `wheelhouse/` | `pip install --no-index --find-links wheelhouse --require-hashes` |
//!
//! The fetched tree is hashed into the release (`vendor_digest`), the package list goes into the
//! SBOM with its purl type, and the deterministic scan checks every package's licence against the
//! allowlist exactly as it does for crates. Build twice, same digest (the reproducibility rule)
//! holds for all three: the fetch result is content only (no caches or timestamps are kept).

use crate::release::LockedPackage;
use serde_json::{json, Value};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ecosystem {
    Rust,
    Node,
    Python,
}

pub const CARGO_LOCK: &str = "Cargo.lock";
pub const NPM_LOCK: &str = "package-lock.json";
pub const NPM_PACKAGE: &str = "package.json";
pub const PY_REQUIREMENTS: &str = "requirements.txt";

/// The npm registry every Node dependency must come from.
pub const NPM_REGISTRY: &str = "https://registry.npmjs.org/";
/// The `source` a Node package gets in [`LockedPackage`].
pub const NPM_SOURCE: &str = "registry+https://registry.npmjs.org";
/// The `source` a Python package gets in [`LockedPackage`].
pub const PYPI_SOURCE: &str = "registry+https://pypi.org/simple";

impl Ecosystem {
    pub fn name(self) -> &'static str {
        match self {
            Ecosystem::Rust => "rust",
            Ecosystem::Node => "node",
            Ecosystem::Python => "python",
        }
    }

    /// The lockfile the ecosystem is built from.
    pub fn lockfile(self) -> &'static str {
        match self {
            Ecosystem::Rust => CARGO_LOCK,
            Ecosystem::Node => NPM_LOCK,
            Ecosystem::Python => PY_REQUIREMENTS,
        }
    }

    /// The purl type of its packages (`pkg:<type>/name@version`).
    pub fn purl_type(self) -> &'static str {
        match self {
            Ecosystem::Rust => "cargo",
            Ecosystem::Node => "npm",
            Ecosystem::Python => "pypi",
        }
    }

    /// Where the fetched dependencies go in the build context.
    pub fn vendor_dir(self) -> &'static str {
        match self {
            Ecosystem::Rust => "vendor",
            Ecosystem::Node => "node_modules",
            Ecosystem::Python => "wheelhouse",
        }
    }

    /// Paths only the build plane may write; a source tree that carries them is refused.
    pub fn reserved(path: &str) -> bool {
        ["vendor", "target", ".cargo", "node_modules", "wheelhouse"]
            .iter()
            .any(|r| path == *r || path.starts_with(&format!("{r}/")))
    }
}

/// Which ecosystem a source tree is: exactly one lockfile.
pub fn detect(src: &Path) -> Result<Ecosystem, String> {
    let found: Vec<Ecosystem> = [Ecosystem::Rust, Ecosystem::Node, Ecosystem::Python]
        .into_iter()
        .filter(|e| src.join(e.lockfile()).is_file())
        .collect();
    match found.as_slice() {
        [one] => {
            if *one == Ecosystem::Node && !src.join(NPM_PACKAGE).is_file() {
                return Err(format!("{NPM_PACKAGE}: required next to {NPM_LOCK}"));
            }
            Ok(*one)
        }
        [] => Err(format!(
            "no lockfile: a release pins its dependencies in {CARGO_LOCK}, {NPM_LOCK} or \
             {PY_REQUIREMENTS} (with hashes)"
        )),
        _ => Err("more than one lockfile: a release is one language".into()),
    }
}

/// The locked packages of `src` in its ecosystem, each with its pinned hash.
pub fn locked(eco: Ecosystem, src: &Path) -> Result<Vec<LockedPackage>, String> {
    let text = std::fs::read_to_string(src.join(eco.lockfile()))
        .map_err(|e| format!("{}: {e}", eco.lockfile()))?;
    match eco {
        Ecosystem::Rust => crate::release::locked_packages(&text),
        Ecosystem::Node => npm_locked(&text),
        Ecosystem::Python => pip_locked(&text),
    }
}

/// `package-lock.json` (lockfile v2 or v3): every installed package (dev dependencies are not
/// installed) from the npm registry with a `sha512` integrity; no links, git or file sources.
pub fn npm_locked(text: &str) -> Result<Vec<LockedPackage>, String> {
    let lock: Value = serde_json::from_str(text).map_err(|e| format!("{NPM_LOCK}: {e}"))?;
    let version = lock["lockfileVersion"].as_u64().unwrap_or(0);
    if version < 2 {
        return Err(format!(
            "{NPM_LOCK}: lockfileVersion 2 or 3 (npm 7 or later)"
        ));
    }
    let packages = lock["packages"]
        .as_object()
        .ok_or(format!("{NPM_LOCK}: no packages"))?;
    let mut out = Vec::new();
    for (path, p) in packages {
        if path.is_empty() {
            continue;
        }
        if p["dev"] == true {
            continue;
        }
        let name = path
            .rsplit("node_modules/")
            .next()
            .filter(|n| !n.is_empty())
            .ok_or(format!("{NPM_LOCK}: {path}: not a node_modules path"))?;
        if !path.starts_with("node_modules/") || p["link"] == true {
            return Err(format!(
                "{NPM_LOCK}: {name}: links and workspace packages are not built"
            ));
        }
        let version = p["version"]
            .as_str()
            .ok_or(format!("{NPM_LOCK}: {name}: no version"))?;
        let resolved = p["resolved"].as_str().unwrap_or("");
        if !resolved.starts_with(NPM_REGISTRY) {
            return Err(format!(
                "{NPM_LOCK}: {name} comes from {resolved:?}, not the npm registry"
            ));
        }
        let integrity = p["integrity"].as_str().unwrap_or("");
        if !integrity.starts_with("sha512-") {
            return Err(format!(
                "{NPM_LOCK}: {name} has no sha512 integrity; every package is pinned by hash"
            ));
        }
        out.push(LockedPackage {
            name: name.to_string(),
            version: version.to_string(),
            source: Some(NPM_SOURCE.into()),
            checksum: Some(integrity.to_string()),
        });
    }
    out.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
    Ok(out)
}

/// A PEP 503 normalized project name.
fn normalize(name: &str) -> String {
    let mut out = String::new();
    let mut sep = false;
    for c in name.chars() {
        if matches!(c, '-' | '_' | '.') {
            sep = true;
            continue;
        }
        if sep && !out.is_empty() {
            out.push('-');
        }
        sep = false;
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// `requirements.txt` in hash-checking mode: every requirement is `name==version` with one or
/// more `--hash=sha256:<64 hex>`. Options that change where packages come from (`-i`,
/// `--index-url`, `--extra-index-url`, `-f`, `-e`, `-r`, `-c`), URLs and environment markers are
/// refused, so what is fetched is exactly what is listed.
pub fn pip_locked(text: &str) -> Result<Vec<LockedPackage>, String> {
    let joined = text.replace("\\\r\n", " ").replace("\\\n", " ");
    let mut out = Vec::new();
    for (n, raw) in joined.lines().enumerate() {
        let line = raw.split(" #").next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = |m: &str| format!("{PY_REQUIREMENTS}:{}: {m}", n + 1);
        let mut words = line.split_whitespace();
        let req = words.next().unwrap_or("");
        if req.starts_with('-') {
            return Err(at("options are not allowed; list pinned requirements only"));
        }
        if req.contains(';') || req.contains('@') || req.contains("://") {
            return Err(at("markers, URLs and direct references are not built"));
        }
        let (name, version) = req
            .split_once("==")
            .ok_or_else(|| at("pin every requirement exactly: name==version"))?;
        let name_ok = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        let version_ok = !version.is_empty()
            && version
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '!' | '-' | '_'));
        if !name_ok || !version_ok || name.contains('[') {
            return Err(at("a plain name==version (no extras)"));
        }
        let mut hashes = Vec::new();
        for w in words {
            let Some(h) = w.strip_prefix("--hash=sha256:") else {
                return Err(at("only --hash=sha256:<digest> may follow a requirement"));
            };
            if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(at("a sha256 hash is 64 hex digits"));
            }
            hashes.push(h.to_ascii_lowercase());
        }
        if hashes.is_empty() {
            return Err(at(
                "every requirement needs --hash=sha256:… (hash-checking mode)",
            ));
        }
        hashes.sort();
        out.push(LockedPackage {
            name: normalize(name),
            version: version.to_string(),
            source: Some(PYPI_SOURCE.into()),
            checksum: Some(hashes.join(",")),
        });
    }
    out.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
    let mut names: Vec<&str> = out.iter().map(|p| p.name.as_str()).collect();
    names.dedup();
    if names.len() != out.len() {
        return Err(format!("{PY_REQUIREMENTS}: a project is listed twice"));
    }
    Ok(out)
}

/// Standard base64 (RFC 4648, padded) to bytes; `None` for anything else.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let bytes = text.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in bytes {
        acc = (acc << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// The SBOM `hashes` of a locked package, from the hash its lockfile pins.
pub fn sbom_hashes(eco: Ecosystem, checksum: &str) -> Value {
    match eco {
        Ecosystem::Rust => json!([{ "alg": "SHA-256", "content": checksum }]),
        Ecosystem::Python => Value::Array(
            checksum
                .split(',')
                .map(|h| json!({ "alg": "SHA-256", "content": h }))
                .collect(),
        ),
        Ecosystem::Node => match checksum
            .strip_prefix("sha512-")
            .and_then(base64_decode)
            .filter(|b| b.len() == 64)
        {
            Some(b) => json!([{ "alg": "SHA-512", "content": crate::util::hex(&b) }]),
            None => json!([]),
        },
    }
}

/// After `npm ci`: every installed package of the lockfile is where the lockfile says, at the
/// locked version. `vendor` holds `node_modules`.
pub fn check_node_modules(vendor: &Path, lock_text: &str) -> Result<(), String> {
    let lock: Value = serde_json::from_str(lock_text).map_err(|e| format!("{NPM_LOCK}: {e}"))?;
    for (path, p) in lock["packages"].as_object().into_iter().flatten() {
        if path.is_empty() || p["dev"] == true {
            continue;
        }
        let manifest = vendor.join(path).join(NPM_PACKAGE);
        let text = std::fs::read_to_string(&manifest)
            .map_err(|_| format!("npm ci did not install {path}"))?;
        let installed: Value =
            serde_json::from_str(&text).map_err(|e| format!("{path}/{NPM_PACKAGE}: {e}"))?;
        if installed["version"] != p["version"] {
            return Err(format!("{path}: installed version is not the locked one"));
        }
    }
    Ok(())
}

/// After `pip download`: only wheels, one per locked project at its version, each matching one of
/// the project's pinned hashes.
pub fn check_wheelhouse(vendor: &Path, packages: &[LockedPackage]) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(vendor).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        let file = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or_default()
            .to_string();
        if !path.is_file() || !file.ends_with(".whl") {
            return Err(format!("wheelhouse: {file}: only wheels are fetched"));
        }
        let mut parts = file.splitn(3, '-');
        let (name, version) = (
            normalize(parts.next().unwrap_or_default()),
            parts.next().unwrap_or_default().to_string(),
        );
        let p = packages
            .iter()
            .find(|p| p.name == name && p.version == version)
            .ok_or(format!("wheelhouse: {file} is not in {PY_REQUIREMENTS}"))?;
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        let digest = crate::util::sha256_hex(&bytes);
        if !p
            .checksum
            .as_deref()
            .unwrap_or_default()
            .split(',')
            .any(|h| h == digest)
        {
            return Err(format!("wheelhouse: {file} does not match its pinned hash"));
        }
        seen.insert(name);
    }
    if let Some(missing) = packages.iter().find(|p| !seen.contains(&p.name)) {
        return Err(format!("wheelhouse: no wheel for {}", missing.name));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn exactly_one_lockfile_names_the_ecosystem() {
        let d = tempfile::tempdir().unwrap();
        assert!(detect(d.path()).is_err());
        std::fs::write(d.path().join(NPM_LOCK), "{}").unwrap();
        assert!(detect(d.path()).unwrap_err().contains(NPM_PACKAGE));
        std::fs::write(d.path().join(NPM_PACKAGE), "{}").unwrap();
        assert_eq!(detect(d.path()).unwrap(), Ecosystem::Node);
        std::fs::write(d.path().join(PY_REQUIREMENTS), "").unwrap();
        assert!(detect(d.path()).unwrap_err().contains("more than one"));
    }

    #[test]
    fn npm_locks_are_registry_packages_pinned_by_sha512() {
        let ok = r#"{"lockfileVersion":3,"packages":{
            "":{"name":"app"},
            "node_modules/left-pad":{"version":"1.3.0","resolved":"https://registry.npmjs.org/left-pad/-/left-pad-1.3.0.tgz","integrity":"sha512-abc","license":"WTFPL"},
            "node_modules/@scope/x":{"version":"2.0.0","resolved":"https://registry.npmjs.org/@scope/x/-/x-2.0.0.tgz","integrity":"sha512-def"},
            "node_modules/devonly":{"version":"1.0.0","dev":true}
        }}"#;
        let p = npm_locked(ok).unwrap();
        let names: Vec<&str> = p.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["@scope/x", "left-pad"]);
        assert_eq!(p[1].checksum.as_deref(), Some("sha512-abc"));
        for bad in [
            r#"{"lockfileVersion":1,"dependencies":{}}"#,
            r#"{"lockfileVersion":3,"packages":{"node_modules/x":{"version":"1.0.0","resolved":"git+https://example.org/x.git","integrity":"sha512-a"}}}"#,
            r#"{"lockfileVersion":3,"packages":{"node_modules/x":{"version":"1.0.0","resolved":"https://registry.npmjs.org/x/-/x-1.0.0.tgz"}}}"#,
            r#"{"lockfileVersion":3,"packages":{"node_modules/x":{"version":"1.0.0","resolved":"https://registry.npmjs.org/x/-/x-1.0.0.tgz","integrity":"sha1-a"}}}"#,
            r#"{"lockfileVersion":3,"packages":{"node_modules/x":{"link":true,"resolved":"../x"}}}"#,
        ] {
            assert!(npm_locked(bad).is_err(), "{bad}");
        }
        assert!(npm_locked(r#"{"lockfileVersion":3,"packages":{"":{}}}"#)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn requirements_are_pinned_and_hashed() {
        let ok = format!(
            "# app deps\nRequests==2.32.3 \\\n    --hash=sha256:{SHA}\nidna==3.10 --hash=sha256:{SHA} --hash=sha256:{}\n",
            SHA.to_uppercase()
        );
        let p = pip_locked(&ok).unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!(p[1].name, "requests");
        assert_eq!(p[0].name, "idna");
        assert!(pip_locked("# nothing\n\n").unwrap().is_empty());
        for bad in [
            "requests==2.32.3".to_string(),
            format!("requests>=2 --hash=sha256:{SHA}"),
            format!("-i https://example.org/simple\nrequests==2.32.3 --hash=sha256:{SHA}"),
            format!("requests @ https://example.org/r.whl --hash=sha256:{SHA}"),
            format!("requests==2.32.3; python_version<'3.9' --hash=sha256:{SHA}"),
            "requests==2.32.3 --hash=md5:abc".to_string(),
            format!("requests==2.32.3 --hash=sha256:{SHA}\nRequests==2.32.3 --hash=sha256:{SHA}"),
            format!("requests[socks]==2.32.3 --hash=sha256:{SHA}"),
        ] {
            assert!(pip_locked(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn sbom_hashes_follow_the_lockfile() {
        // sha512 of the empty string, as an npm integrity.
        let sri = "sha512-z4PhNX7vuL3xVChQ1m2AB9Yg5AULVxXcg/SpIdNs6c5H0NE8XYXysP+DGNKHfuwvY7kxvUdBeoGlODJ6+SfaPg==";
        assert_eq!(
            sbom_hashes(Ecosystem::Node, sri)[0]["content"],
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
        );
        assert_eq!(sbom_hashes(Ecosystem::Node, "sha512-short"), json!([]));
        assert_eq!(
            sbom_hashes(Ecosystem::Python, "aa,bb")
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn fetched_trees_are_checked_against_the_lock() {
        let d = tempfile::tempdir().unwrap();
        let lock = r#"{"lockfileVersion":3,"packages":{"":{},"node_modules/a":{"version":"1.0.0","resolved":"https://registry.npmjs.org/a/-/a-1.0.0.tgz","integrity":"sha512-x"}}}"#;
        assert!(check_node_modules(d.path(), lock).is_err());
        std::fs::create_dir_all(d.path().join("node_modules/a")).unwrap();
        std::fs::write(
            d.path().join("node_modules/a/package.json"),
            r#"{"version":"1.0.1"}"#,
        )
        .unwrap();
        assert!(check_node_modules(d.path(), lock).is_err());
        std::fs::write(
            d.path().join("node_modules/a/package.json"),
            r#"{"version":"1.0.0"}"#,
        )
        .unwrap();
        check_node_modules(d.path(), lock).unwrap();

        let w = tempfile::tempdir().unwrap();
        let wheel = b"synthetic wheel";
        let hash = crate::util::sha256_hex(wheel);
        let pkgs = pip_locked(&format!("Tiny_Pkg==1.0 --hash=sha256:{hash}\n")).unwrap();
        assert!(check_wheelhouse(w.path(), &pkgs).is_err(), "missing wheel");
        std::fs::write(w.path().join("tiny_pkg-1.0-py3-none-any.whl"), wheel).unwrap();
        check_wheelhouse(w.path(), &pkgs).unwrap();
        std::fs::write(w.path().join("tiny_pkg-1.0-py3-none-any.whl"), b"other").unwrap();
        assert!(check_wheelhouse(w.path(), &pkgs).is_err(), "hash mismatch");
        std::fs::remove_file(w.path().join("tiny_pkg-1.0-py3-none-any.whl")).unwrap();
        std::fs::write(w.path().join("tiny_pkg-1.0.tar.gz"), wheel).unwrap();
        assert!(check_wheelhouse(w.path(), &pkgs).is_err(), "sdist");
    }

    #[test]
    fn reserved_paths_are_the_build_planes() {
        for p in [
            "vendor",
            "vendor/x",
            "node_modules/a",
            "wheelhouse/a.whl",
            ".cargo/config.toml",
        ] {
            assert!(Ecosystem::reserved(p), "{p}");
        }
        for p in ["src/vendor.rs", "node_modules_note.md", "app.mjs"] {
            assert!(!Ecosystem::reserved(p), "{p}");
        }
    }
}
