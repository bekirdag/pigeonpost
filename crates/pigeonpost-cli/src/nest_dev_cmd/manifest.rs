//! What `nest run` reads from an app's source tree: the release manifest, the configuration schema
//! and job files.
//!
//! The manifest struct mirrors `pigeonpost_nests::release::Manifest` field for field (the build
//! plane's parser, `deny_unknown_fields`), so a manifest the emulator accepts has the shape the
//! build plane parses; `crates/pigeonpost-nests` tests every template against the real parser.
//! Full checking of a release before submission is `pigeonpost nest validate`, which is not part
//! of this command.

use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::Path;

pub const MANIFEST_FILE: &str = "nest-release.toml";
pub const CONFIG_SCHEMA_FILE: &str = "config.schema.json";
/// The postbox caps a job request at 256 KiB.
pub const MAX_JOB_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub name: String,
    pub max_chars: usize,
    pub required: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub name: String,
    pub version: String,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // Every field is parsed so an unknown or missing one is refused.
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
    pub licences: BTreeMap<String, String>,
}

impl Manifest {
    pub fn load(dir: &Path) -> Result<Manifest, String> {
        let path = dir.join(MANIFEST_FILE);
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let m: Manifest = toml::from_str(&text).map_err(|e| format!("{MANIFEST_FILE}: {e}"))?;
        if !pigeonpost_nest_kit::valid_name(&m.name) {
            return Err(format!("{MANIFEST_FILE}: name: 1-64 of a-z 0-9 -"));
        }
        if m.operations.is_empty() {
            return Err(format!(
                "{MANIFEST_FILE}: a release offers at least one operation"
            ));
        }
        Ok(m)
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

/// Check a configuration against the app's schema and fill in defaults. The schema language is
/// a small JSON Schema subset: an object of `string`, `integer`, `number` and `boolean`
/// properties with `enum`, `minimum`, `maximum`, `maxLength`, `default`, `required` and
/// `additionalProperties: false`.
pub fn apply_config(schema: &Value, config: Value) -> Result<Value, String> {
    let Value::Object(mut config) = config else {
        return Err("the configuration must be a JSON object".into());
    };
    if schema["type"] != "object" {
        return Err(format!("{CONFIG_SCHEMA_FILE}: type must be \"object\""));
    }
    let empty = Map::new();
    let props = schema["properties"].as_object().unwrap_or(&empty);
    if schema["additionalProperties"] == false {
        if let Some(k) = config.keys().find(|k| !props.contains_key(*k)) {
            return Err(format!("{k}: not in {CONFIG_SCHEMA_FILE}"));
        }
    }
    for (name, p) in props {
        let value = match config.get(name) {
            Some(v) => v.clone(),
            None => match p.get("default") {
                Some(d) => {
                    config.insert(name.clone(), d.clone());
                    d.clone()
                }
                None => continue,
            },
        };
        let ty = p["type"].as_str().unwrap_or("");
        let ok = match ty {
            "string" => value.is_string(),
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            other => return Err(format!("{name}: unsupported type {other:?}")),
        };
        if !ok {
            return Err(format!("{name}: must be {ty}"));
        }
        if let Some(choices) = p["enum"].as_array() {
            if !choices.contains(&value) {
                return Err(format!("{name}: one of {}", Value::Array(choices.clone())));
            }
        }
        if let (Some(n), Some(min)) = (value.as_f64(), p["minimum"].as_f64()) {
            if n < min {
                return Err(format!("{name}: at least {min}"));
            }
        }
        if let (Some(n), Some(max)) = (value.as_f64(), p["maximum"].as_f64()) {
            if n > max {
                return Err(format!("{name}: at most {max}"));
            }
        }
        if let (Some(s), Some(max)) = (value.as_str(), p["maxLength"].as_u64()) {
            if s.chars().count() as u64 > max {
                return Err(format!("{name}: longer than {max} characters"));
            }
        }
    }
    for r in schema["required"].as_array().into_iter().flatten() {
        if let Some(r) = r.as_str() {
            if !config.contains_key(r) {
                return Err(format!("{r}: required"));
            }
        }
    }
    Ok(Value::Object(config))
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
                "sig": {"type": "string", "maxLength": 3}
            },
            "required": []
        });
        assert_eq!(
            apply_config(&schema, json!({})).unwrap(),
            json!({"style": "short", "n": 2})
        );
        assert_eq!(
            apply_config(&schema, json!({"n": 4, "sig": "abc"})).unwrap(),
            json!({"style": "short", "n": 4, "sig": "abc"})
        );
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
    }
}
