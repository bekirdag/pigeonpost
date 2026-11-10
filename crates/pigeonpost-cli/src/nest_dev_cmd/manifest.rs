//! What `nest run` reads from an app's source tree: the release manifest (with its configuration
//! schema) and job files.
//!
//! The manifest is checked with `pigeonpost_core::nest_package`, the rules the postbox and the
//! build plane apply (`pigeonpost nest validate` prints the full report). `nest run` refuses a
//! malformed manifest and runs one the deterministic scan would block (external writes, for
//! example), so such a flow can still be tried locally.

use pigeonpost_core::nest_package::{self, Severity, MANIFEST_FILE};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::Path;

/// The postbox caps a job request at 256 KiB.
pub const MAX_JOB_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, Deserialize)]
pub struct Field {
    pub name: String,
    pub max_chars: usize,
    pub required: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Operation {
    pub name: String,
    pub version: String,
    pub fields: Vec<Field>,
}

/// The parts of a manifest `nest run` uses. Every key was already checked by the shared rules.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub egress: Vec<String>,
    pub external_writes: bool,
    pub operations: Vec<Operation>,
    pub licences: BTreeMap<String, String>,
    #[serde(default)]
    pub config_schema: Option<Value>,
}

impl Manifest {
    pub fn load(dir: &Path) -> Result<Manifest, String> {
        let path = dir.join(MANIFEST_FILE);
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let raw: toml::Value =
            toml::from_str(&text).map_err(|e| format!("{MANIFEST_FILE}: {e}"))?;
        let json = serde_json::to_value(raw).map_err(|e| format!("{MANIFEST_FILE}: {e}"))?;
        let errors: Vec<String> = nest_package::validate_manifest(&json)
            .into_iter()
            .filter(|i| i.severity == Severity::Error)
            .map(|i| i.to_string())
            .collect();
        if !errors.is_empty() {
            return Err(format!("{MANIFEST_FILE}:\n  {}", errors.join("\n  ")));
        }
        serde_json::from_value(json).map_err(|e| format!("{MANIFEST_FILE}: {e}"))
    }

    /// The configuration schema, or the empty one (only `{}` is valid) when there is none.
    pub fn schema(&self) -> Value {
        self.config_schema
            .clone()
            .unwrap_or_else(nest_package::empty_config_schema)
    }

    pub fn operation(&self, name: &str, version: &str) -> Option<&Operation> {
        self.operations
            .iter()
            .find(|o| o.name == name && o.version == version)
    }

    pub fn uses_broker(&self) -> bool {
        self.egress.iter().any(|e| e == "broker")
    }
}

/// The payload against the operation's fields, exactly as admission checks it
/// (`marketplace_m1::validate_payload`): declared string fields only, within their bounds, the
/// required ones present and not blank.
pub fn check_payload(op: &Operation, payload: &Map<String, Value>) -> Result<(), String> {
    for key in payload.keys() {
        if !op.fields.iter().any(|f| &f.name == key) {
            return Err(format!("{key}: not a field of this operation"));
        }
    }
    for f in &op.fields {
        match payload.get(&f.name) {
            None | Some(Value::Null) if f.required => return Err(format!("{}: required", f.name)),
            None | Some(Value::Null) => {}
            Some(Value::String(s)) => {
                if s.trim().is_empty() && f.required {
                    return Err(format!("{}: required", f.name));
                }
                if s.chars().count() > f.max_chars {
                    return Err(format!(
                        "{}: longer than {} characters",
                        f.name, f.max_chars
                    ));
                }
            }
            Some(_) => return Err(format!("{}: must be a string", f.name)),
        }
    }
    Ok(())
}

/// One job file as a developer writes it: the operation, its version and the payload, optionally
/// with a fixed job id.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobFile {
    #[serde(default)]
    pub job_id: Option<String>,
    pub operation: String,
    pub operation_version: String,
    pub payload: Map<String, Value>,
}

/// `job_` and 26 lower-case base32 characters, as the postbox mints them.
pub fn valid_job_id(id: &str) -> bool {
    id.strip_prefix("job_").is_some_and(|rest| {
        rest.len() == 26
            && rest
                .bytes()
                .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
    })
}

/// A job id derived from the job file, so the same file always gets the same id (the dispatcher's
/// dedupe is by job id).
pub fn derived_job_id(bytes: &[u8], index: usize) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"pigeonpost-nest-run\0");
    h.update(index.to_le_bytes());
    h.update(bytes);
    let digest = h.finalize();
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::from("job_");
    let mut acc: u32 = 0;
    let mut bits = 0;
    for b in digest.iter() {
        acc = (acc << 8) | u32::from(*b);
        bits += 8;
        while bits >= 5 && out.len() < 30 {
            bits -= 5;
            out.push(ALPHABET[((acc >> bits) & 31) as usize] as char);
        }
    }
    out
}

/// A checked installation configuration: what the app reads as `job.config` (secrets left out,
/// the schema's defaults filled in) and the secrets, which only the broker hands out.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub plain: Value,
    pub secrets: Map<String, Value>,
}

/// Check a configuration against the release's schema with the shared rules, exactly as an
/// installation is checked, and split off its secrets.
pub fn apply_config(schema: &Value, config: Value) -> Result<Config, String> {
    let issues = nest_package::validate_config_schema(schema);
    if !issues.is_empty() {
        let list: Vec<String> = issues.iter().map(|i| i.to_string()).collect();
        return Err(format!("config_schema: {}", list.join("; ")));
    }
    let issues = nest_package::validate_config(schema, &config);
    if !issues.is_empty() {
        let list: Vec<String> = issues.iter().map(|i| i.to_string()).collect();
        return Err(list.join("; "));
    }
    let (plain, secrets) = nest_package::split_config(schema, &config);
    let secrets = secrets.into_iter().filter(|(_, v)| !v.is_null()).collect();
    Ok(Config {
        plain: Value::Object(nest_package::with_defaults(schema, &plain)),
        secrets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn op() -> Operation {
        Operation {
            name: "docs.ask".into(),
            version: "1.0".into(),
            fields: vec![
                Field {
                    name: "question".into(),
                    max_chars: 5,
                    required: true,
                },
                Field {
                    name: "lang".into(),
                    max_chars: 2,
                    required: false,
                },
            ],
        }
    }

    fn map(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn payloads_are_checked_like_admission() {
        assert!(check_payload(&op(), &map(json!({"question": "hi"}))).is_ok());
        assert!(check_payload(&op(), &map(json!({"question": "hi", "lang": "tr"}))).is_ok());
        for bad in [
            json!({}),
            json!({"question": "  "}),
            json!({"question": "toolong"}),
            json!({"question": 1}),
            json!({"question": "hi", "extra": "x"}),
        ] {
            assert!(check_payload(&op(), &map(bad.clone())).is_err(), "{bad}");
        }
    }

    #[test]
    fn derived_job_ids_are_valid_and_stable() {
        let a = derived_job_id(b"{}", 0);
        assert!(valid_job_id(&a), "{a}");
        assert_eq!(a, derived_job_id(b"{}", 0));
        assert_ne!(a, derived_job_id(b"{}", 1));
        assert!(!valid_job_id("job_ABC"));
    }

    #[test]
    fn config_gets_defaults_and_is_checked() {
        let schema = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "style": {"type": "string", "enum": ["short", "long"], "default": "short"},
                "n": {"type": "integer", "minimum": 1, "maximum": 4, "default": 2},
                "sig": {"type": "string", "maxLength": 3},
                "api_key": {"type": "string", "maxLength": 64, "writeOnly": true}
            },
            "required": []
        });
        assert_eq!(
            apply_config(&schema, json!({})).unwrap().plain,
            json!({"style": "short", "n": 2})
        );
        let c = apply_config(&schema, json!({"n": 4, "sig": "abc", "api_key": "k1"})).unwrap();
        assert_eq!(c.plain, json!({"style": "short", "n": 4, "sig": "abc"}));
        assert_eq!(c.secrets.get("api_key"), Some(&json!("k1")));
        for bad in [
            json!({"style": "medium"}),
            json!({"n": 0}),
            json!({"n": "2"}),
            json!({"sig": "abcd"}),
            json!({"other": 1}),
            json!([]),
        ] {
            assert!(apply_config(&schema, bad.clone()).is_err(), "{bad}");
        }
        // A keyword outside the shared subset is refused by name.
        let mut pattern = schema.clone();
        pattern["properties"]["sig"]["pattern"] = json!("^a");
        assert!(apply_config(&pattern, json!({}))
            .unwrap_err()
            .contains("unsupported_keyword"));
    }
}
