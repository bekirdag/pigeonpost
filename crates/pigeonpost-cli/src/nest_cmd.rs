//! `pigeonpost nest validate <dir>` — check a Nest package offline (marketplace M2, R-70).
//!
//! The rules are `pigeonpost_core::nest_package`, the same code the postbox runs on a build
//! record's manifest and on every installation's configuration, so a package that passes here is
//! refused there only for server-side facts (who owns the listing address, the template's slug,
//! the version already taken). Passing is not approval: the build, the scans, the behaviour test
//! and a staff decision still follow.
//!
//! Nothing here touches the network, an identity or the agent's home directory.

use pigeonpost_core::nest_package::{
    self, Issue, Severity, BUILD_FILE, CONTAINERFILE, MANIFEST_FILE,
};
use serde_json::{json, Value};
use std::path::Path;

fn issue(path: &str, code: &'static str, message: impl Into<String>) -> Issue {
    Issue {
        path: path.to_string(),
        code,
        message: message.into(),
        severity: Severity::Error,
    }
}

/// What `validate` found: the manifest as JSON (when it parsed) and every issue.
#[derive(Debug)]
pub(crate) struct Report {
    pub manifest: Option<Value>,
    pub issues: Vec<Issue>,
}

/// Validate the package directory, and optionally an example configuration against the package's
/// configuration schema.
pub(crate) fn check(dir: &Path, config: Option<&Path>) -> Report {
    let mut issues = Vec::new();
    let manifest_path = dir.join(MANIFEST_FILE);
    let manifest = match std::fs::read_to_string(&manifest_path) {
        Err(e) => {
            issues.push(issue(
                "",
                "missing",
                format!("{}: {e}", manifest_path.display()),
            ));
            None
        }
        Ok(text) => match toml::from_str::<toml::Value>(&text) {
            Err(e) => {
                issues.push(issue("", "bad_value", format!("{MANIFEST_FILE}: {e}")));
                None
            }
            Ok(raw) => match serde_json::to_value(raw) {
                Ok(v) => Some(v),
                Err(e) => {
                    issues.push(issue("", "bad_value", format!("{MANIFEST_FILE}: {e}")));
                    None
                }
            },
        },
    };
    if let Some(m) = &manifest {
        issues.extend(nest_package::validate_manifest(m));
    }
    // The image assembly: one pinned base and file copies, nothing that runs a program.
    match std::fs::read_to_string(dir.join(CONTAINERFILE)) {
        Err(_) => issues.push(issue(
            "",
            "missing",
            format!("{CONTAINERFILE}: the build plane assembles the image from it"),
        )),
        Ok(text) => issues.extend(nest_package::validate_containerfile(&text)),
    }
    // The compile step, when there is one.
    if let Ok(text) = std::fs::read_to_string(dir.join(BUILD_FILE)) {
        match toml::from_str::<toml::Value>(&text)
            .map_err(|e| e.to_string())
            .and_then(|raw| serde_json::to_value(raw).map_err(|e| e.to_string()))
        {
            Err(e) => issues.push(issue("", "bad_value", format!("{BUILD_FILE}: {e}"))),
            Ok(spec) => {
                for mut i in nest_package::validate_build_spec(&spec) {
                    i.path = format!(
                        "{BUILD_FILE}:{}",
                        if i.path.is_empty() { "/" } else { &i.path }
                    );
                    issues.push(i);
                }
            }
        }
    }
    if let Some(path) = config {
        let schema = manifest
            .as_ref()
            .and_then(|m| m.get("config_schema").cloned())
            .unwrap_or_else(nest_package::empty_config_schema);
        let schema_ok = nest_package::validate_config_schema(&schema).is_empty();
        match std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()))
        {
            Err(e) => issues.push(issue("", "bad_value", format!("{}: {e}", path.display()))),
            // A broken schema is already reported; checking against it would only add noise.
            Ok(_) if !schema_ok => {}
            Ok(cfg) => {
                for mut i in nest_package::validate_config(&schema, &cfg) {
                    i.path = format!("config:{}", if i.path.is_empty() { "/" } else { &i.path });
                    issues.push(i);
                }
            }
        }
    }
    Report { manifest, issues }
}

/// `pigeonpost nest validate`: print the report; fail when anything was found.
pub(crate) fn validate(
    dir: &Path,
    config: Option<&Path>,
    json_out: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let report = check(dir, config);
    let errors = report
        .issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .count();
    let blocked = report.issues.len() - errors;
    if json_out {
        println!(
            "{}",
            json!({
                "valid": report.issues.is_empty(),
                "name": report.manifest.as_ref().and_then(|m| m.get("name")),
                "version": report.manifest.as_ref().and_then(|m| m.get("version")),
                "errors": errors,
                "blocked": blocked,
                "issues": report.issues,
            })
        );
    } else {
        for i in &report.issues {
            println!("{i}");
        }
        if report.issues.is_empty() {
            let name = report
                .manifest
                .as_ref()
                .and_then(|m| m.get("name").and_then(Value::as_str))
                .unwrap_or("?");
            let version = report
                .manifest
                .as_ref()
                .and_then(|m| m.get("version").and_then(Value::as_str))
                .unwrap_or("?");
            println!("ok: {name} {version} passes the package rules (not yet reviewed)");
        }
    }
    if report.issues.is_empty() {
        Ok(())
    } else {
        Err(format!("{errors} error(s), {blocked} blocked by the marketplace pool's limits").into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const REFERENCE: &str =
        include_str!("../../../deploy/nests/reference/hr-policy-qa/nest-release.toml");
    const REFERENCE_CONTAINERFILE: &str =
        include_str!("../../../deploy/nests/reference/hr-policy-qa/Containerfile");
    const REFERENCE_BUILD: &str =
        include_str!("../../../deploy/nests/reference/hr-policy-qa/nest-build.toml");
    const FIXTURES: &str =
        include_str!("../../../docs/nests/contracts/examples/nest-config-fixtures.json");

    fn package(manifest: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MANIFEST_FILE), manifest).unwrap();
        std::fs::write(dir.path().join(CONTAINERFILE), REFERENCE_CONTAINERFILE).unwrap();
        std::fs::write(dir.path().join(BUILD_FILE), REFERENCE_BUILD).unwrap();
        dir
    }

    /// The build files are held to the build plane's rules: a Containerfile that runs a program
    /// or an unpinned base, and a malformed compile step, are errors here already.
    #[test]
    fn the_build_files_are_checked_offline() {
        let dir = package(REFERENCE);
        std::fs::write(
            dir.path().join(CONTAINERFILE),
            format!("{REFERENCE_CONTAINERFILE}RUN cargo build\n"),
        )
        .unwrap();
        std::fs::write(dir.path().join(BUILD_FILE), "command = \"make\"\n").unwrap();
        let r = check(dir.path(), None);
        let found: Vec<(&str, &str)> = r.issues.iter().map(|i| (i.path.as_str(), i.code)).collect();
        assert!(
            found
                .iter()
                .any(|(p, c)| p.starts_with("Containerfile:") && *c == "not_allowed"),
            "{found:?}"
        );
        assert!(
            found.contains(&("nest-build.toml:/command", "wrong_type")),
            "{found:?}"
        );
        std::fs::write(
            dir.path().join(CONTAINERFILE),
            "FROM debian:bookworm-slim\n",
        )
        .unwrap();
        std::fs::remove_file(dir.path().join(BUILD_FILE)).unwrap();
        let r = check(dir.path(), None);
        assert_eq!(r.issues.len(), 1, "{:?}", r.issues);
        assert_eq!(r.issues[0].code, "bad_value");
    }

    #[test]
    fn the_reference_release_passes() {
        let dir = package(REFERENCE);
        let r = check(dir.path(), None);
        assert!(r.issues.is_empty(), "{:?}", r.issues);
        assert_eq!(r.manifest.unwrap()["name"], "hr-policy-qa");
    }

    #[test]
    fn a_missing_package_and_bad_toml_are_clear() {
        let empty = tempfile::tempdir().unwrap();
        let r = check(empty.path(), None);
        assert_eq!(r.issues.len(), 2, "{:?}", r.issues);
        assert!(r.issues.iter().all(|i| i.code == "missing"));
        let dir = package("name = \"x\"\nversion = ");
        let r = check(dir.path(), None);
        assert_eq!(r.issues[0].code, "bad_value");
        assert!(r.issues[0].message.contains(MANIFEST_FILE));
    }

    #[test]
    fn pool_limits_are_reported_as_blocked() {
        let dir = package(&REFERENCE.replace("capabilities = []", "capabilities = [\"NET_RAW\"]"));
        let r = check(dir.path(), None);
        assert_eq!(r.issues.len(), 1, "{:?}", r.issues);
        assert_eq!(r.issues[0].severity, Severity::Blocker);
        assert_eq!(r.issues[0].path, "/capabilities/0");
        assert!(validate(dir.path(), None, true).is_err());
    }

    /// The CLI and the server agree: each shared fixture schema, written into a manifest as TOML
    /// and read back the way a publisher's package is, gives exactly the issues the fixtures
    /// list, and each fixture configuration likewise.
    #[test]
    fn the_shared_fixtures_hold_through_toml() {
        let fx: Value = serde_json::from_str(FIXTURES).unwrap();
        let pairs = |v: &Value, prefix: &str| -> BTreeSet<(String, String)> {
            v.as_array()
                .unwrap()
                .iter()
                .map(|p| {
                    let path = p[0].as_str().unwrap();
                    let path = if prefix == "config:" && path.is_empty() {
                        "/"
                    } else {
                        path
                    };
                    (
                        format!("{prefix}{path}"),
                        p[1].as_str().unwrap().to_string(),
                    )
                })
                .collect()
        };
        let mut ran = 0;
        for s in fx["schemas"].as_array().unwrap() {
            let mut m: toml::Table = toml::from_str(REFERENCE).unwrap();
            m.insert(
                "config_schema".into(),
                toml::Value::try_from(&s["schema"]).unwrap(),
            );
            let dir = package(&toml::to_string(&m).unwrap());
            let r = check(dir.path(), None);
            let got: BTreeSet<(String, String)> = r
                .issues
                .iter()
                .map(|i| (i.path.clone(), i.code.to_string()))
                .collect();
            assert_eq!(
                got,
                pairs(&s["schema_issues"], "/config_schema"),
                "{}",
                s["name"]
            );
            for c in s["configs"].as_array().unwrap() {
                let cfg = dir.path().join("config.json");
                std::fs::write(&cfg, c["config"].to_string()).unwrap();
                let r = check(dir.path(), Some(&cfg));
                let got: BTreeSet<(String, String)> = r
                    .issues
                    .iter()
                    .map(|i| (i.path.clone(), i.code.to_string()))
                    .collect();
                assert_eq!(
                    got,
                    pairs(&c["config_issues"], "config:"),
                    "{}: {}",
                    s["name"],
                    c["name"]
                );
                ran += 1;
            }
        }
        assert!(ran >= 10);
    }
}
