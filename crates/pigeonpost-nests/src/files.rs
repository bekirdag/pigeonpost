//! Files on a Nest (Phase B4, R-72): upload → quarantine → parse in a separate sandbox → the
//! Nest's knowledge area.
//!
//! Layout on the Nest's encrypted volume (`/nest` inside the sandbox):
//!
//! ```text
//! files/quarantine/<file_id>        the original bytes, before and during parsing (0400)
//! files/originals/<file_id>         the original bytes after parsing (kept for export)
//! files/parsed/<file_id>/manifest.json, part-NNNN.txt, meta.json   what the Nest may read
//! ```
//!
//! The Nest's agent never opens an original; the runner reads only `files/parsed/`. The parser
//! runs in its own gVisor sandbox with no network ([`crate::sandbox::Sandbox::run_parser`]).
//! Its output is checked here before it is trusted: the manifest must parse, every part must be a
//! numbered `part-NNNN.txt`, and the total size is bounded. Anything else is discarded.

use crate::parse::{display_name, Manifest, MAX_OUTPUT_CHARS};
use std::path::{Path, PathBuf};

/// Upload ids are minted by the postbox: `nfl_` and 24 hex characters.
pub fn valid_file_id(id: &str) -> bool {
    id.strip_prefix("nfl_")
        .is_some_and(|h| h.len() == 24 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Most bytes a parser's output directory may hold (text is bounded by characters, so allow
/// four bytes per character, plus the manifest).
const MAX_OUTPUT_BYTES: u64 = MAX_OUTPUT_CHARS as u64 * 4 + 1024 * 1024;

pub struct Paths {
    pub quarantine: PathBuf,
    pub original: PathBuf,
    pub parsed: PathBuf,
}

pub fn paths(root: &Path, file_id: &str) -> Paths {
    let f = root.join("files");
    Paths {
        quarantine: f.join("quarantine").join(file_id),
        original: f.join("originals").join(file_id),
        parsed: f.join("parsed").join(file_id),
    }
}

/// Check a parser's output directory and return its manifest. Refuses (and the caller deletes)
/// anything the parser was not supposed to produce: unexpected names, links, oversized output.
pub fn verify_output(dir: &Path) -> Result<Manifest, String> {
    let mut total: u64 = 0;
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let meta = std::fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?;
        if !meta.is_file() {
            return Err("the parser produced something that is not a file".into());
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let numbered = name
            .strip_prefix("part-")
            .and_then(|r| r.strip_suffix(".txt"))
            .is_some_and(|n| n.len() == 4 && n.bytes().all(|b| b.is_ascii_digit()));
        if name != "manifest.json" && !numbered {
            return Err("the parser produced an unexpected file".into());
        }
        total += meta.len();
        if total > MAX_OUTPUT_BYTES {
            return Err("the parser produced too much output".into());
        }
        names.push(name);
    }
    let raw = std::fs::read(dir.join("manifest.json")).map_err(|_| "no manifest".to_string())?;
    let v: serde_json::Value =
        serde_json::from_slice(&raw).map_err(|_| "the manifest is not JSON".to_string())?;
    let manifest = manifest_from(&v).ok_or("the manifest is malformed")?;
    for p in &manifest.parts {
        if !names.contains(&p.name) {
            return Err("the manifest names a part that does not exist".into());
        }
    }
    Ok(manifest)
}

/// Rebuild a manifest from the parser's JSON, accepting only the fields and values it may have.
fn manifest_from(v: &serde_json::Value) -> Option<Manifest> {
    use crate::parse::{Coverage, Kind, Part, Status};
    let kind = match v["kind"].as_str()? {
        "text" => Kind::Text,
        "csv" => Kind::Csv,
        "pdf" => Kind::Pdf,
        "docx" => Kind::Docx,
        "xlsx" => Kind::Xlsx,
        "zip" => Kind::Zip,
        "unknown" => Kind::Unknown,
        _ => return None,
    };
    let status = match v["status"].as_str()? {
        "parsed" => Status::Parsed,
        "partial" => Status::Partial,
        "rejected" => Status::Rejected,
        _ => return None,
    };
    let parts = v["parts"]
        .as_array()?
        .iter()
        .take(10_000)
        .map(|p| {
            Some(Part {
                name: p["name"].as_str()?.to_string(),
                source: display_name(p["source"].as_str().unwrap_or("")),
                chars: p["chars"].as_u64()? as usize,
                text: String::new(),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Manifest {
        kind,
        status,
        reason: v["reason"].as_str().map(display_name),
        bytes: v["bytes"].as_u64()? as usize,
        coverage: Coverage {
            units_total: v["coverage"]["units_total"].as_u64()? as usize,
            units_text: v["coverage"]["units_text"].as_u64()? as usize,
        },
        parts,
        warnings: v["warnings"]
            .as_array()?
            .iter()
            .take(200)
            .filter_map(|w| w.as_str().map(display_name))
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_ids_are_strict() {
        assert!(valid_file_id("nfl_0123456789abcdef01234567"));
        for bad in [
            "nfl_",
            "nfl_../../etc",
            "nfl_0123456789abcdef0123456",
            "x_0123456789abcdef01234567",
            "nfl_0123456789abcdef0123456g",
        ] {
            assert!(!valid_file_id(bad), "{bad}");
        }
    }

    #[test]
    fn parser_output_is_verified_before_it_is_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let m = crate::parse::parse(b"hello there");
        std::fs::write(dir.path().join("part-0001.txt"), "hello there").unwrap();
        std::fs::write(
            dir.path().join("manifest.json"),
            serde_json::to_vec(&m).unwrap(),
        )
        .unwrap();
        let ok = verify_output(dir.path()).unwrap();
        assert_eq!(ok.parts.len(), 1);
        // A stray file (or a name from the input) makes the whole output untrusted.
        std::fs::write(dir.path().join("authorized_keys"), "x").unwrap();
        assert!(verify_output(dir.path()).is_err());
        std::fs::remove_file(dir.path().join("authorized_keys")).unwrap();
        // A link is refused.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/passwd", dir.path().join("part-0002.txt")).unwrap();
            assert!(verify_output(dir.path()).is_err());
            std::fs::remove_file(dir.path().join("part-0002.txt")).unwrap();
        }
        // A manifest naming a missing part, or malformed, is refused.
        let mut bad: serde_json::Value = serde_json::to_value(&m).unwrap();
        bad["parts"][0]["name"] = "part-0009.txt".into();
        std::fs::write(dir.path().join("manifest.json"), bad.to_string()).unwrap();
        assert!(verify_output(dir.path()).is_err());
        std::fs::write(dir.path().join("manifest.json"), r#"{"kind":"exe"}"#).unwrap();
        assert!(verify_output(dir.path()).is_err());
    }
}
