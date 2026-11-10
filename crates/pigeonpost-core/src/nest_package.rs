//! Nest package rules (marketplace M2): the release manifest and its configuration schema.
//!
//! One implementation, three callers: the postbox checks a build record's manifest and every
//! installation's configuration with it, the build plane (`pigeonpost_nests::release`) parses
//! manifests with it, and `pigeonpost nest validate` runs it offline on a package directory. A
//! package that passes here is not approved: it still goes through the build, the scans, the
//! behaviour test and a staff decision.
//!
//! Everything here works on JSON values. A manifest is written as TOML (`nest-release.toml`) and
//! read into JSON by its caller; the rules do not depend on the source format.
//!
//! # Configuration schema
//!
//! A release may declare `config_schema`, a small subset of JSON Schema (draft 2020-12 keywords,
//! nothing else): one flat object of string, integer, number and boolean properties.
//!
//! - The root is `{"type": "object", "additionalProperties": false, "properties": {…}}`, with
//!   optional `required`, `title`, `description` and `$schema`.
//! - A property has a `type` and may have `title`, `description` and `default`. Strings need
//!   `maxLength` (at most [`MAX_STRING_CHARS`]) or an `enum`, and may have `minLength`. Numbers
//!   and integers may have `minimum` and `maximum`.
//! - `writeOnly: true` marks a **secret**: a string with no `default` and no `enum`. Secrets are
//!   never part of the stored configuration; [`split_config`] separates them so the caller can
//!   seal them.
//! - Any other keyword (`pattern`, `format`, `$ref`, nested objects, arrays, combinators) is
//!   refused by name, so a schema never means more here than it says.
//!
//! # Issues
//!
//! Every check returns a list of [`Issue`]s with a JSON pointer, a stable code and a message. An
//! [`Severity::Error`] means the document is malformed and is refused at intake. A
//! [`Severity::Blocker`] is well-formed but asks for something the marketplace pool does not
//! offer (a capability, a syscall, egress beyond the broker, external writes): the deterministic
//! scan would block it, so the local validator reports it early.

use serde::Serialize;
use serde_json::{Map, Value};
use std::cmp::Ordering;
use std::collections::BTreeSet;

/// The manifest's file name in a package directory.
pub const MANIFEST_FILE: &str = "nest-release.toml";
/// Longest string a configuration property may allow.
pub const MAX_STRING_CHARS: u64 = 4096;
/// Most properties a configuration schema may declare.
pub const MAX_PROPERTIES: usize = 32;
/// Largest configuration document, serialized.
pub const MAX_CONFIG_BYTES: usize = 16 * 1024;
/// Most operations one release may offer, and most fields one operation may take.
pub const MAX_OPERATIONS: usize = 32;
pub const MAX_FIELDS: usize = 32;
/// Longest input field an operation may declare.
pub const MAX_FIELD_CHARS: u64 = 65_536;
/// The only egress a marketplace release may declare.
pub const ALLOWED_EGRESS: &[&str] = &["broker"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Malformed: refused at intake.
    Error,
    /// Well-formed, but the deterministic scan blocks it.
    Blocker,
}

/// One problem with a document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Issue {
    /// JSON pointer into the document (`""` is the whole document).
    pub path: String,
    /// Stable, machine-readable: `missing`, `unknown_field`, `wrong_type`, `bad_value`,
    /// `too_long`, `too_short`, `too_many`, `duplicate`, `unsupported_keyword`, `required`,
    /// `not_in_enum`, `below_minimum`, `above_maximum`, `not_allowed`.
    pub code: &'static str,
    pub message: String,
    pub severity: Severity,
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let path = if self.path.is_empty() {
            "/"
        } else {
            &self.path
        };
        let tag = match self.severity {
            Severity::Error => "error",
            Severity::Blocker => "blocked",
        };
        write!(f, "{tag}: {path}: {} ({})", self.message, self.code)
    }
}

fn err(path: impl Into<String>, code: &'static str, message: impl Into<String>) -> Issue {
    Issue {
        path: path.into(),
        code,
        message: message.into(),
        severity: Severity::Error,
    }
}

fn blocker(path: impl Into<String>, message: impl Into<String>) -> Issue {
    Issue {
        path: path.into(),
        code: "not_allowed",
        message: message.into(),
        severity: Severity::Blocker,
    }
}

/// Only the issues that refuse a document at intake.
pub fn errors(issues: &[Issue]) -> Vec<Issue> {
    issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .cloned()
        .collect()
}

/// A JSON pointer segment (RFC 6901 escaping).
fn seg(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}

// ---- small shared patterns ---------------------------------------------------------------------

/// A package name or template slug: 1-64 of `a-z 0-9 -`.
pub fn slug_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// `major.minor.patch`, digits only.
pub fn semver_ok(v: &str) -> bool {
    semver_parts(v).is_some()
}

fn semver_parts(v: &str) -> Option<[u64; 3]> {
    let parts: Vec<&str> = v.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let mut out = [0u64; 3];
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() || p.len() > 9 || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        out[i] = p.parse().ok()?;
    }
    Some(out)
}

/// Compare two release versions numerically. `None` if either is not `major.minor.patch`.
pub fn semver_cmp(a: &str, b: &str) -> Option<Ordering> {
    Some(semver_parts(a)?.cmp(&semver_parts(b)?))
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

/// A field or property name: `[a-z][a-z0-9_]{0,63}`.
pub fn field_name_ok(s: &str) -> bool {
    s.len() <= 64
        && s.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// A shared listing address: `/segment/segment…`, each `a-z 0-9 -`, at least two, at most 128
/// bytes.
pub fn listing_address_ok(a: &str) -> bool {
    let Some(rest) = a.strip_prefix('/') else {
        return false;
    };
    let segments: Vec<&str> = rest.split('/').collect();
    a.len() <= 128 && segments.len() >= 2 && segments.iter().all(|s| slug_ok(s))
}

fn printable(s: &str) -> bool {
    !s.chars().any(char::is_control)
}

// ---- the manifest ------------------------------------------------------------------------------

const MANIFEST_KEYS: &[&str] = &[
    "name",
    "version",
    "title",
    "summary",
    "licence",
    "entrypoint",
    "listing_address",
    "egress",
    "capabilities",
    "syscalls",
    "external_writes",
    "operations",
    "licences",
    "config_schema",
];

fn text_field(m: &Map<String, Value>, key: &str, max: usize, out: &mut Vec<Issue>) {
    let path = format!("/{key}");
    match m.get(key) {
        None => out.push(err(path, "missing", format!("{key} is required"))),
        Some(Value::String(s)) => {
            let n = s.chars().count();
            if n == 0 || s.trim().is_empty() {
                out.push(err(path, "too_short", format!("{key} may not be empty")));
            } else if n > max {
                out.push(err(path, "too_long", format!("at most {max} characters")));
            } else if !printable(s) {
                out.push(err(path, "bad_value", "control characters are not allowed"));
            }
        }
        Some(_) => out.push(err(path, "wrong_type", "must be a string")),
    }
}

fn string_list<'a>(
    m: &'a Map<String, Value>,
    key: &str,
    out: &mut Vec<Issue>,
) -> Option<Vec<&'a str>> {
    let path = format!("/{key}");
    match m.get(key) {
        None => {
            out.push(err(path, "missing", format!("{key} is required")));
            None
        }
        Some(Value::Array(items)) => {
            let mut list = Vec::new();
            for (i, item) in items.iter().enumerate() {
                match item.as_str() {
                    Some(s) => list.push(s),
                    None => out.push(err(format!("{path}/{i}"), "wrong_type", "must be a string")),
                }
            }
            Some(list)
        }
        Some(_) => {
            out.push(err(path, "wrong_type", "must be a list of strings"));
            None
        }
    }
}

/// Every issue with a release manifest (as JSON), errors and blockers alike.
pub fn validate_manifest(manifest: &Value) -> Vec<Issue> {
    let mut out = Vec::new();
    let Some(m) = manifest.as_object() else {
        return vec![err("", "wrong_type", "the manifest is a table of keys")];
    };
    for key in m.keys() {
        if !MANIFEST_KEYS.contains(&key.as_str()) {
            out.push(err(
                format!("/{}", seg(key)),
                "unknown_field",
                format!("{key} is not a manifest key"),
            ));
        }
    }
    match m.get("name") {
        None => out.push(err("/name", "missing", "name is required")),
        Some(Value::String(s)) if slug_ok(s) => {}
        Some(Value::String(_)) => out.push(err("/name", "bad_value", "1-64 of a-z 0-9 -")),
        Some(_) => out.push(err("/name", "wrong_type", "must be a string")),
    }
    match m.get("version") {
        None => out.push(err("/version", "missing", "version is required")),
        Some(Value::String(s)) if semver_ok(s) => {}
        Some(Value::String(_)) => out.push(err(
            "/version",
            "bad_value",
            "major.minor.patch, digits only",
        )),
        Some(_) => out.push(err("/version", "wrong_type", "must be a string")),
    }
    text_field(m, "title", 200, &mut out);
    text_field(m, "summary", 200, &mut out);
    text_field(m, "licence", 200, &mut out);
    match m.get("entrypoint") {
        None => out.push(err("/entrypoint", "missing", "entrypoint is required")),
        Some(Value::String(s))
            if s.starts_with('/')
                && s.len() <= 256
                && !s.split('/').any(|p| p == "..")
                && !s.chars().any(|c| c.is_whitespace() || c.is_control()) => {}
        Some(Value::String(_)) => out.push(err(
            "/entrypoint",
            "bad_value",
            "an absolute path in the image, no spaces or ..",
        )),
        Some(_) => out.push(err("/entrypoint", "wrong_type", "must be a string")),
    }
    match m.get("listing_address") {
        None => out.push(err(
            "/listing_address",
            "missing",
            "listing_address is required",
        )),
        Some(Value::String(s)) if listing_address_ok(s) => {}
        Some(Value::String(_)) => out.push(err(
            "/listing_address",
            "bad_value",
            "/publisher/name: at least two segments of a-z 0-9 -, at most 128 bytes",
        )),
        Some(_) => out.push(err("/listing_address", "wrong_type", "must be a string")),
    }
    if let Some(egress) = string_list(m, "egress", &mut out) {
        for (i, dest) in egress.iter().enumerate() {
            if !ALLOWED_EGRESS.contains(dest) {
                out.push(blocker(
                    format!("/egress/{i}"),
                    format!("egress to {dest:?}: only the broker is reachable"),
                ));
            }
        }
    }
    if let Some(caps) = string_list(m, "capabilities", &mut out) {
        for (i, cap) in caps.iter().enumerate() {
            out.push(blocker(
                format!("/capabilities/{i}"),
                format!("capability {cap} is not available in the marketplace pool"),
            ));
        }
    }
    if let Some(calls) = string_list(m, "syscalls", &mut out) {
        for (i, call) in calls.iter().enumerate() {
            out.push(blocker(
                format!("/syscalls/{i}"),
                format!("syscall {call} is not available in the marketplace pool"),
            ));
        }
    }
    match m.get("external_writes") {
        None => out.push(err(
            "/external_writes",
            "missing",
            "external_writes is required",
        )),
        Some(Value::Bool(false)) => {}
        Some(Value::Bool(true)) => out.push(blocker(
            "/external_writes",
            "external writes are not offered in the marketplace pool",
        )),
        Some(_) => out.push(err(
            "/external_writes",
            "wrong_type",
            "must be true or false",
        )),
    }
    operations(m.get("operations"), &mut out);
    match m.get("licences") {
        None => out.push(err("/licences", "missing", "licences is required")),
        Some(Value::Object(l)) => {
            for (pkg, expr) in l {
                let path = format!("/licences/{}", seg(pkg));
                match expr.as_str() {
                    Some(e) if !e.trim().is_empty() && e.len() <= 200 => {}
                    Some(_) => out.push(err(path, "bad_value", "an SPDX expression")),
                    None => out.push(err(path, "wrong_type", "must be a string")),
                }
            }
        }
        Some(_) => out.push(err(
            "/licences",
            "wrong_type",
            "a table of package = SPDX expression",
        )),
    }
    if let Some(schema) = m.get("config_schema") {
        for mut i in validate_config_schema(schema) {
            i.path = format!("/config_schema{}", i.path);
            out.push(i);
        }
    }
    out
}

fn operations(ops: Option<&Value>, out: &mut Vec<Issue>) {
    let ops = match ops {
        None => {
            out.push(err("/operations", "missing", "operations is required"));
            return;
        }
        Some(Value::Array(ops)) => ops,
        Some(_) => {
            out.push(err("/operations", "wrong_type", "a list of operations"));
            return;
        }
    };
    if ops.is_empty() {
        out.push(err(
            "/operations",
            "too_short",
            "a release offers at least one operation",
        ));
    }
    if ops.len() > MAX_OPERATIONS {
        out.push(err(
            "/operations",
            "too_many",
            format!("at most {MAX_OPERATIONS} operations"),
        ));
    }
    let mut seen = BTreeSet::new();
    for (i, op) in ops.iter().enumerate() {
        let path = format!("/operations/{i}");
        let Some(o) = op.as_object() else {
            out.push(err(path, "wrong_type", "an operation is a table"));
            continue;
        };
        for key in o.keys() {
            if !["name", "version", "fields"].contains(&key.as_str()) {
                out.push(err(
                    format!("{path}/{}", seg(key)),
                    "unknown_field",
                    format!("{key} is not an operation key"),
                ));
            }
        }
        let name = o.get("name").and_then(Value::as_str);
        let version = o.get("version").and_then(Value::as_str);
        match name {
            Some(n) if operation_name_ok(n) => {}
            Some(_) => out.push(err(
                format!("{path}/name"),
                "bad_value",
                "lower-case dotted words, at most 80 bytes",
            )),
            None => out.push(err(format!("{path}/name"), "missing", "name is required")),
        }
        match version {
            Some(v) if operation_version_ok(v) => {}
            Some(_) => out.push(err(
                format!("{path}/version"),
                "bad_value",
                "major.minor or major.minor.patch",
            )),
            None => out.push(err(
                format!("{path}/version"),
                "missing",
                "version is required",
            )),
        }
        if let (Some(n), Some(v)) = (name, version) {
            if !seen.insert((n.to_string(), v.to_string())) {
                out.push(err(
                    path.clone(),
                    "duplicate",
                    format!("{n} {v} is declared twice"),
                ));
            }
        }
        let fields = match o.get("fields") {
            Some(Value::Array(f)) => f,
            Some(_) => {
                out.push(err(
                    format!("{path}/fields"),
                    "wrong_type",
                    "a list of fields",
                ));
                continue;
            }
            None => {
                out.push(err(
                    format!("{path}/fields"),
                    "missing",
                    "an operation declares its input fields",
                ));
                continue;
            }
        };
        if fields.is_empty() {
            out.push(err(
                format!("{path}/fields"),
                "too_short",
                "an operation needs bounded input fields",
            ));
        }
        if fields.len() > MAX_FIELDS {
            out.push(err(
                format!("{path}/fields"),
                "too_many",
                format!("at most {MAX_FIELDS} fields"),
            ));
        }
        let mut names = BTreeSet::new();
        for (j, f) in fields.iter().enumerate() {
            let fp = format!("{path}/fields/{j}");
            let Some(f) = f.as_object() else {
                out.push(err(fp, "wrong_type", "a field is a table"));
                continue;
            };
            for key in f.keys() {
                if !["name", "max_chars", "required"].contains(&key.as_str()) {
                    out.push(err(
                        format!("{fp}/{}", seg(key)),
                        "unknown_field",
                        format!("{key} is not a field key"),
                    ));
                }
            }
            match f.get("name").and_then(Value::as_str) {
                Some(n) if field_name_ok(n) => {
                    if !names.insert(n.to_string()) {
                        out.push(err(format!("{fp}/name"), "duplicate", format!("{n} twice")));
                    }
                }
                Some(_) => out.push(err(format!("{fp}/name"), "bad_value", "[a-z][a-z0-9_]*")),
                None => out.push(err(format!("{fp}/name"), "missing", "name is required")),
            }
            match f.get("max_chars").map(Value::as_u64) {
                Some(Some(n)) if (1..=MAX_FIELD_CHARS).contains(&n) => {}
                Some(_) => out.push(err(
                    format!("{fp}/max_chars"),
                    "bad_value",
                    format!("an integer, 1-{MAX_FIELD_CHARS}"),
                )),
                None => out.push(err(
                    format!("{fp}/max_chars"),
                    "missing",
                    "every field is bounded",
                )),
            }
            match f.get("required") {
                Some(Value::Bool(_)) => {}
                Some(_) => out.push(err(
                    format!("{fp}/required"),
                    "wrong_type",
                    "must be true or false",
                )),
                None => out.push(err(
                    format!("{fp}/required"),
                    "missing",
                    "required is required",
                )),
            }
        }
    }
}

// ---- the configuration schema ------------------------------------------------------------------

/// Keywords that mean something in JSON Schema but are outside this subset. Named so the message
/// can say so instead of calling them unknown.
const UNSUPPORTED: &[&str] = &[
    "pattern",
    "format",
    "$ref",
    "$defs",
    "definitions",
    "items",
    "prefixItems",
    "oneOf",
    "anyOf",
    "allOf",
    "not",
    "if",
    "then",
    "else",
    "const",
    "patternProperties",
    "propertyNames",
    "dependentRequired",
    "dependentSchemas",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "minItems",
    "maxItems",
    "uniqueItems",
    "minProperties",
    "maxProperties",
    "readOnly",
    "contentEncoding",
    "contentMediaType",
    "examples",
    "deprecated",
];

fn keyword_issue(path: String, key: &str) -> Issue {
    if UNSUPPORTED.contains(&key) {
        err(
            path,
            "unsupported_keyword",
            format!("{key} is not in the Pigeonpost configuration subset"),
        )
    } else {
        err(
            path,
            "unsupported_keyword",
            format!("{key} is not a configuration schema keyword"),
        )
    }
}

/// Every issue with a configuration schema. Paths are relative to the schema.
pub fn validate_config_schema(schema: &Value) -> Vec<Issue> {
    let mut out = Vec::new();
    let Some(s) = schema.as_object() else {
        return vec![err(
            "",
            "wrong_type",
            "the configuration schema is an object",
        )];
    };
    for key in s.keys() {
        if ![
            "type",
            "properties",
            "required",
            "additionalProperties",
            "title",
            "description",
            "$schema",
        ]
        .contains(&key.as_str())
        {
            out.push(keyword_issue(format!("/{}", seg(key)), key));
        }
    }
    if s.get("type") != Some(&Value::String("object".into())) {
        out.push(err("/type", "bad_value", "the root type is \"object\""));
    }
    if s.get("additionalProperties") != Some(&Value::Bool(false)) {
        out.push(err(
            "/additionalProperties",
            "bad_value",
            "must be false: a configuration has only the properties it declares",
        ));
    }
    for key in ["title", "description", "$schema"] {
        if let Some(v) = s.get(key) {
            let max = if key == "description" { 500 } else { 200 };
            match v.as_str() {
                Some(t) if t.chars().count() <= max && printable(t) => {}
                Some(_) => out.push(err(
                    format!("/{}", seg(key)),
                    "too_long",
                    format!("at most {max} printable characters"),
                )),
                None => out.push(err(
                    format!("/{}", seg(key)),
                    "wrong_type",
                    "must be a string",
                )),
            }
        }
    }
    let props = match s.get("properties") {
        Some(Value::Object(p)) => p,
        Some(_) => {
            out.push(err("/properties", "wrong_type", "an object of properties"));
            return out;
        }
        None => {
            out.push(err("/properties", "missing", "properties is required"));
            return out;
        }
    };
    if props.len() > MAX_PROPERTIES {
        out.push(err(
            "/properties",
            "too_many",
            format!("at most {MAX_PROPERTIES} properties"),
        ));
    }
    for (name, prop) in props {
        let path = format!("/properties/{}", seg(name));
        if !field_name_ok(name) {
            out.push(err(
                path.clone(),
                "bad_value",
                "a property name is [a-z][a-z0-9_]*, at most 64 bytes",
            ));
        }
        property(&path, prop, &mut out);
    }
    if let Some(req) = s.get("required") {
        match req.as_array() {
            None => out.push(err("/required", "wrong_type", "a list of property names")),
            Some(list) => {
                let mut seen = BTreeSet::new();
                for (i, r) in list.iter().enumerate() {
                    match r.as_str() {
                        Some(n) if !props.contains_key(n) => out.push(err(
                            format!("/required/{i}"),
                            "bad_value",
                            format!("{n} is not a declared property"),
                        )),
                        Some(n) if !seen.insert(n) => out.push(err(
                            format!("/required/{i}"),
                            "duplicate",
                            format!("{n} twice"),
                        )),
                        Some(_) => {}
                        None => out.push(err(
                            format!("/required/{i}"),
                            "wrong_type",
                            "must be a string",
                        )),
                    }
                }
            }
        }
    }
    out
}

const PROPERTY_KEYS: &[&str] = &[
    "type",
    "title",
    "description",
    "default",
    "enum",
    "minLength",
    "maxLength",
    "minimum",
    "maximum",
    "writeOnly",
];

fn property(path: &str, prop: &Value, out: &mut Vec<Issue>) {
    let Some(p) = prop.as_object() else {
        out.push(err(path, "wrong_type", "a property is an object"));
        return;
    };
    for key in p.keys() {
        if !PROPERTY_KEYS.contains(&key.as_str()) {
            out.push(keyword_issue(format!("{path}/{}", seg(key)), key));
        }
    }
    let kind = match p.get("type").and_then(Value::as_str) {
        Some(t @ ("string" | "integer" | "number" | "boolean")) => t,
        Some("object" | "array" | "null") => {
            out.push(err(
                format!("{path}/type"),
                "unsupported_keyword",
                "only string, integer, number and boolean properties are in the subset",
            ));
            return;
        }
        Some(_) => {
            out.push(err(
                format!("{path}/type"),
                "bad_value",
                "string, integer, number or boolean",
            ));
            return;
        }
        None => {
            out.push(err(format!("{path}/type"), "missing", "type is required"));
            return;
        }
    };
    for key in ["title", "description"] {
        if let Some(v) = p.get(key) {
            let max = if key == "title" { 100 } else { 500 };
            match v.as_str() {
                Some(t) if t.chars().count() <= max && printable(t) => {}
                Some(_) => out.push(err(
                    format!("{path}/{key}"),
                    "too_long",
                    format!("at most {max} printable characters"),
                )),
                None => out.push(err(
                    format!("{path}/{key}"),
                    "wrong_type",
                    "must be a string",
                )),
            }
        }
    }
    let secret = match p.get("writeOnly") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => {
            out.push(err(
                format!("{path}/writeOnly"),
                "wrong_type",
                "must be true or false",
            ));
            false
        }
    };
    let only_for = |key: &str, kinds: &[&str], out: &mut Vec<Issue>| {
        if p.contains_key(key) && !kinds.contains(&kind) {
            out.push(err(
                format!("{path}/{key}"),
                "unsupported_keyword",
                format!("{key} does not apply to a {kind} property"),
            ));
        }
    };
    only_for("minLength", &["string"], out);
    only_for("maxLength", &["string"], out);
    only_for("enum", &["string"], out);
    only_for("minimum", &["integer", "number"], out);
    only_for("maximum", &["integer", "number"], out);
    only_for("writeOnly", &["string"], out);
    if kind == "string" {
        let len = |key: &str, out: &mut Vec<Issue>| -> Option<u64> {
            match p.get(key) {
                None => None,
                Some(v) => match v.as_u64() {
                    Some(n) if n <= MAX_STRING_CHARS => Some(n),
                    _ => {
                        out.push(err(
                            format!("{path}/{key}"),
                            "bad_value",
                            format!("an integer, 0-{MAX_STRING_CHARS}"),
                        ));
                        None
                    }
                },
            }
        };
        let min = len("minLength", out);
        let max = len("maxLength", out);
        if let (Some(a), Some(b)) = (min, max) {
            if a > b {
                out.push(err(
                    format!("{path}/minLength"),
                    "bad_value",
                    "minLength is above maxLength",
                ));
            }
        }
        if let Some(e) = p.get("enum") {
            match e.as_array() {
                Some(items) if !items.is_empty() && items.len() <= 64 => {
                    let mut seen = BTreeSet::new();
                    for (i, item) in items.iter().enumerate() {
                        match item.as_str() {
                            Some(s) if s.chars().count() as u64 <= MAX_STRING_CHARS => {
                                if !seen.insert(s) {
                                    out.push(err(
                                        format!("{path}/enum/{i}"),
                                        "duplicate",
                                        format!("{s:?} twice"),
                                    ));
                                }
                            }
                            _ => out.push(err(
                                format!("{path}/enum/{i}"),
                                "wrong_type",
                                "enum values are strings",
                            )),
                        }
                    }
                }
                _ => out.push(err(
                    format!("{path}/enum"),
                    "bad_value",
                    "a list of 1-64 strings",
                )),
            }
        }
        if !p.contains_key("maxLength") && !p.contains_key("enum") {
            out.push(err(
                format!("{path}/maxLength"),
                "missing",
                "a string property is bounded by maxLength or an enum",
            ));
        }
        if secret {
            for key in ["default", "enum"] {
                if p.contains_key(key) {
                    out.push(err(
                        format!("{path}/{key}"),
                        "bad_value",
                        format!("a secret (writeOnly) property has no {key}"),
                    ));
                }
            }
        }
    }
    if kind == "integer" || kind == "number" {
        let bound = |key: &str, out: &mut Vec<Issue>| -> Option<f64> {
            let v = p.get(key)?;
            let ok = if kind == "integer" {
                v.as_i64().is_some()
            } else {
                v.is_number()
            };
            if ok {
                v.as_f64()
            } else {
                out.push(err(
                    format!("{path}/{key}"),
                    "wrong_type",
                    format!("must be a {kind}"),
                ));
                None
            }
        };
        if let (Some(a), Some(b)) = (bound("minimum", out), bound("maximum", out)) {
            if a > b {
                out.push(err(
                    format!("{path}/minimum"),
                    "bad_value",
                    "minimum is above maximum",
                ));
            }
        }
    }
    if let Some(default) = p.get("default") {
        let mut inner = Vec::new();
        value(&format!("{path}/default"), p, default, &mut inner);
        out.extend(inner);
    }
}

/// Check one value against one (already valid) property.
fn value(path: &str, p: &Map<String, Value>, v: &Value, out: &mut Vec<Issue>) {
    match p.get("type").and_then(Value::as_str).unwrap_or("") {
        "string" => {
            let Some(s) = v.as_str() else {
                out.push(err(path, "wrong_type", "must be a string"));
                return;
            };
            let n = s.chars().count() as u64;
            if let Some(max) = p.get("maxLength").and_then(Value::as_u64) {
                if n > max {
                    out.push(err(path, "too_long", format!("at most {max} characters")));
                }
            }
            if let Some(min) = p.get("minLength").and_then(Value::as_u64) {
                if n < min {
                    out.push(err(path, "too_short", format!("at least {min} characters")));
                }
            }
            if let Some(items) = p.get("enum").and_then(Value::as_array) {
                if !items.iter().any(|i| i.as_str() == Some(s)) {
                    out.push(err(path, "not_in_enum", "not one of the allowed values"));
                }
            }
            if !printable(s) {
                out.push(err(path, "bad_value", "control characters are not allowed"));
            }
        }
        "boolean" => {
            if !v.is_boolean() {
                out.push(err(path, "wrong_type", "must be true or false"));
            }
        }
        kind @ ("integer" | "number") => {
            let ok = if kind == "integer" {
                v.as_i64().is_some()
            } else {
                v.is_number()
            };
            let Some(x) = v.as_f64().filter(|_| ok) else {
                out.push(err(path, "wrong_type", format!("must be a {kind}")));
                return;
            };
            if let Some(min) = p.get("minimum").and_then(Value::as_f64) {
                if x < min {
                    out.push(err(path, "below_minimum", format!("at least {min}")));
                }
            }
            if let Some(max) = p.get("maximum").and_then(Value::as_f64) {
                if x > max {
                    out.push(err(path, "above_maximum", format!("at most {max}")));
                }
            }
        }
        _ => out.push(err(path, "bad_value", "the property's type is unknown")),
    }
}

/// Whether a property of an (already valid) schema is a secret.
fn is_secret(p: &Value) -> bool {
    p.get("writeOnly") == Some(&Value::Bool(true))
}

/// The secret (writeOnly) property names of a schema.
pub fn secret_fields(schema: &Value) -> Vec<String> {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|props| {
            props
                .iter()
                .filter(|(_, p)| is_secret(p))
                .map(|(k, _)| k.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Check a configuration against a schema that [`validate_config_schema`] accepted.
pub fn validate_config(schema: &Value, config: &Value) -> Vec<Issue> {
    validate_config_with(schema, config, &BTreeSet::new())
}

/// [`validate_config`], where `secrets_already_set` are secrets stored earlier that this document
/// leaves out: a required secret is satisfied by one. A secret given as `null` is being cleared.
pub fn validate_config_with(
    schema: &Value,
    config: &Value,
    secrets_already_set: &BTreeSet<String>,
) -> Vec<Issue> {
    let mut out = Vec::new();
    let Some(c) = config.as_object() else {
        return vec![err("", "wrong_type", "a configuration is a JSON object")];
    };
    if config.to_string().len() > MAX_CONFIG_BYTES {
        out.push(err(
            "",
            "too_long",
            format!("a configuration is at most {MAX_CONFIG_BYTES} bytes"),
        ));
    }
    let empty = Map::new();
    let props = schema
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    for (key, v) in c {
        let path = format!("/{}", seg(key));
        let Some(p) = props.get(key).and_then(Value::as_object) else {
            out.push(err(
                path,
                "unknown_field",
                format!("{key} is not a configuration property"),
            ));
            continue;
        };
        if v.is_null() && is_secret(&Value::Object(p.clone())) {
            continue;
        }
        value(&path, p, v, &mut out);
    }
    for r in schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let given = c.get(r).is_some_and(|v| !v.is_null());
        let kept = !c.contains_key(r) && secrets_already_set.contains(r);
        if !given && !kept {
            out.push(err(
                format!("/{}", seg(r)),
                "required",
                format!("{r} is required"),
            ));
        }
    }
    out
}

/// Split a configuration that passed [`validate_config`] into the stored part and its secrets.
/// Secret values are returned as given (`null` meaning "clear"); nothing secret is left in the
/// stored part.
pub fn split_config(schema: &Value, config: &Value) -> (Map<String, Value>, Map<String, Value>) {
    let secrets: BTreeSet<String> = secret_fields(schema).into_iter().collect();
    let mut plain = Map::new();
    let mut secret = Map::new();
    if let Some(c) = config.as_object() {
        for (k, v) in c {
            if secrets.contains(k) {
                secret.insert(k.clone(), v.clone());
            } else {
                plain.insert(k.clone(), v.clone());
            }
        }
    }
    (plain, secret)
}

/// Defaults the schema declares for properties the configuration leaves out.
pub fn with_defaults(schema: &Value, plain: &Map<String, Value>) -> Map<String, Value> {
    let mut out = plain.clone();
    if let Some(props) = schema.get("properties").and_then(Value::as_object) {
        for (k, p) in props {
            if let Some(d) = p.get("default") {
                out.entry(k.clone()).or_insert_with(|| d.clone());
            }
        }
    }
    out
}

/// The configuration schema of a release with none: no properties, so only `{}` is valid.
pub fn empty_config_schema() -> Value {
    serde_json::json!({ "type": "object", "additionalProperties": false, "properties": {} })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest() -> Value {
        json!({
            "name": "hr-policy-qa",
            "version": "1.0.0",
            "title": "HR policy Q&A",
            "summary": "Answers from a bundled, synthetic handbook.",
            "licence": "MIT",
            "entrypoint": "/usr/local/bin/hr-policy-qa",
            "listing_address": "/pigeonpost/hr-policy-qa",
            "egress": ["broker"],
            "capabilities": [],
            "syscalls": [],
            "external_writes": false,
            "operations": [{
                "name": "hr.policy.ask",
                "version": "1.0",
                "fields": [{"name": "question", "max_chars": 2000, "required": true}],
            }],
            "licences": {"hr-policy-qa": "MIT"},
        })
    }

    fn codes(issues: &[Issue]) -> Vec<(&str, &str)> {
        issues.iter().map(|i| (i.path.as_str(), i.code)).collect()
    }

    /// The shared fixtures (`docs/nests/contracts/examples/nest-config-fixtures.json`) that the
    /// CLI and the postbox run too.
    #[test]
    fn the_shared_fixtures_hold() {
        let fx: Value = serde_json::from_str(FIXTURES).unwrap();
        let pairs = |v: &Value| -> BTreeSet<(String, String)> {
            v.as_array()
                .unwrap()
                .iter()
                .map(|p| (p[0].as_str().unwrap().into(), p[1].as_str().unwrap().into()))
                .collect()
        };
        let got = |issues: Vec<Issue>| -> BTreeSet<(String, String)> {
            issues
                .into_iter()
                .map(|i| (i.path, i.code.to_string()))
                .collect()
        };
        let mut ran = 0;
        for s in fx["schemas"].as_array().unwrap() {
            let name = s["name"].as_str().unwrap();
            assert_eq!(
                got(validate_config_schema(&s["schema"])),
                pairs(&s["schema_issues"]),
                "schema {name}"
            );
            for c in s["configs"].as_array().unwrap() {
                assert_eq!(
                    got(validate_config(&s["schema"], &c["config"])),
                    pairs(&c["config_issues"]),
                    "{name}: {}",
                    c["name"]
                );
                ran += 1;
            }
        }
        assert!(ran >= 10);
    }

    pub(crate) const FIXTURES: &str =
        include_str!("../../../docs/nests/contracts/examples/nest-config-fixtures.json");

    #[test]
    fn the_reference_manifest_is_clean() {
        assert_eq!(validate_manifest(&manifest()), vec![]);
    }

    #[test]
    fn manifest_errors_name_their_place() {
        let mut m = manifest();
        m["name"] = json!("HR");
        m["version"] = json!("1.0");
        m["listing_address"] = json!("/one");
        m["surprise"] = json!(1);
        m["operations"][0]["fields"][0]["max_chars"] = json!(0);
        let first = m["operations"][0].clone();
        m["operations"].as_array_mut().unwrap().push(first);
        let issues = validate_manifest(&m);
        let c = codes(&issues);
        for want in [
            ("/name", "bad_value"),
            ("/version", "bad_value"),
            ("/listing_address", "bad_value"),
            ("/surprise", "unknown_field"),
            ("/operations/0/fields/0/max_chars", "bad_value"),
            ("/operations/1", "duplicate"),
        ] {
            assert!(c.contains(&want), "{want:?} in {c:?}");
        }
        assert!(issues.iter().all(|i| i.severity == Severity::Error));
    }

    #[test]
    fn pool_limits_are_blockers_not_errors() {
        let mut m = manifest();
        m["egress"] = json!(["broker", "internet"]);
        m["capabilities"] = json!(["NET_ADMIN"]);
        m["syscalls"] = json!(["ptrace"]);
        m["external_writes"] = json!(true);
        let issues = validate_manifest(&m);
        assert_eq!(issues.len(), 4, "{issues:?}");
        assert!(issues.iter().all(|i| i.severity == Severity::Blocker));
        assert!(errors(&issues).is_empty());
    }

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(semver_cmp("1.10.0", "1.9.9"), Some(Ordering::Greater));
        assert_eq!(semver_cmp("1.0.0", "1.0.0"), Some(Ordering::Equal));
        assert_eq!(semver_cmp("1.0", "1.0.0"), None);
    }

    #[test]
    fn secrets_are_split_out_and_kept_by_omission() {
        let schema = json!({
            "type": "object", "additionalProperties": false,
            "required": ["team", "token"],
            "properties": {
                "team": {"type": "string", "maxLength": 40},
                "token": {"type": "string", "maxLength": 200, "writeOnly": true},
                "limit": {"type": "integer", "minimum": 1, "maximum": 10, "default": 5},
            },
        });
        assert!(validate_config_schema(&schema).is_empty());
        let cfg = json!({"team": "ops", "token": "s3cret"});
        assert!(validate_config(&schema, &cfg).is_empty());
        let (plain, secret) = split_config(&schema, &cfg);
        assert_eq!(Value::Object(plain.clone()), json!({"team": "ops"}));
        assert_eq!(Value::Object(secret), json!({"token": "s3cret"}));
        assert_eq!(
            Value::Object(with_defaults(&schema, &plain)),
            json!({"team": "ops", "limit": 5})
        );
        // Left out: satisfied by a stored secret only.
        let partial = json!({"team": "ops"});
        assert_eq!(
            codes(&validate_config(&schema, &partial)),
            vec![("/token", "required")]
        );
        let set: BTreeSet<String> = ["token".to_string()].into();
        assert!(validate_config_with(&schema, &partial, &set).is_empty());
        // Cleared with null: required again.
        let cleared = json!({"team": "ops", "token": null});
        assert_eq!(
            codes(&validate_config_with(&schema, &cleared, &set)),
            vec![("/token", "required")]
        );
    }
}
