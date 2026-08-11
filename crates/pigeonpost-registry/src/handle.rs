//! Handles: `/github/superaidev` and `/google/…`.
//!
//! Mirrored namespaces, per `docs/architecture.md`. The scarce thing gating a registration is an
//! identity that already exists somewhere that fought spam for a decade — so a handle is never
//! *allocated* here, only *reflected*. That is what makes squatting structurally impossible
//! rather than merely discouraged.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{RegistryError, Result};

/// Provider namespaces we *mirror*. Adding one means adding an identity adapter that can prove
/// control of a name in it — never a policy decision made here.
pub const NAMESPACES: &[&str] = &["github", "google"];

/// The internal namespace for flat, unprefixed handles — `/wodo`. Unlike the provider namespaces
/// this one is *allocated*, not reflected: it is the paid tier, gated on an entitlement rather than
/// an upstream proof, and it is the only namespace that carries a reserved set. Stored under this
/// tag; displayed bare.
pub const FLAT_NAMESPACE: &str = "handle";

/// Longest provider name we reflect. GitHub's own limit is 39; matching it means we never reject a
/// name the upstream namespace considers valid.
pub const MAX_NAME: usize = 39;

/// Flat handles are deliberately tighter than provider handles: alphanumeric only (no `-` `_` `.`),
/// 3..=32 characters. The floor keeps single/double characters — which are namespace-shaped — out of
/// the salable pool; the ceiling and charset kill the punctuation and homograph tricks a chosen name
/// invites (`docs/architecture.md`, `docs/reserved-names.md`).
pub const FLAT_MIN: usize = 3;
pub const FLAT_MAX: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Handle {
    namespace: String,
    name: String,
}

impl Handle {
    pub fn new(namespace: &str, name: &str) -> Result<Self> {
        let namespace = namespace.to_ascii_lowercase();
        if namespace == FLAT_NAMESPACE {
            return Ok(Handle {
                namespace,
                name: validate_flat_name(name)?,
            });
        }
        if !NAMESPACES.contains(&namespace.as_str()) {
            return Err(RegistryError::UnknownNamespace(namespace));
        }
        Ok(Handle {
            namespace,
            name: validate_name(name)?,
        })
    }

    /// Accepts both the provider form `/github/superaidev` and the flat form `/wodo`. A bare segment
    /// with no second `/` is a flat handle; the reserved set (which includes every namespace name)
    /// is what stops `/github` being claimed as the flat name "github".
    pub fn parse(input: &str) -> Result<Self> {
        let trimmed = input.strip_prefix('/').unwrap_or(input);
        match trimmed.split_once('/') {
            Some((namespace, name)) => Handle::new(namespace, name),
            None => Handle::new(FLAT_NAMESPACE, trimmed),
        }
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// True for the paid, allocated tier. Callers gate the entitlement check on this.
    pub fn is_flat(&self) -> bool {
        self.namespace == FLAT_NAMESPACE
    }

    /// Canonical stored key — always `/{namespace}/{name}`, including flat (`/handle/wodo`), so the
    /// storage layer has one shape. Use [`Handle::display`] for what a human types.
    pub fn as_path(&self) -> String {
        format!("/{}/{}", self.namespace, self.name)
    }

    /// What a human writes and reads: flat handles are bare (`/wodo`), provider handles keep their
    /// prefix (`/github/superaidev`).
    pub fn display(&self) -> String {
        if self.is_flat() {
            format!("/{}", self.name)
        } else {
            self.as_path()
        }
    }
}

impl fmt::Display for Handle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_path())
    }
}

impl TryFrom<String> for Handle {
    type Error = RegistryError;
    fn try_from(value: String) -> Result<Self> {
        Handle::parse(&value)
    }
}

impl From<Handle> for String {
    fn from(value: Handle) -> Self {
        value.as_path()
    }
}

/// Case-folded to lowercase, because `/github/Alice` and `/github/alice` must not be two
/// different Pigeonpost destinations that a human cannot tell apart.
fn validate_name(name: &str) -> Result<String> {
    if name.is_empty() || name.len() > MAX_NAME {
        return Err(RegistryError::MalformedHandle(format!(
            "name must be 1..={MAX_NAME} characters"
        )));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(RegistryError::MalformedHandle(
            "name may contain only letters, digits, '-', '_', and '.'".into(),
        ));
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err(RegistryError::MalformedHandle(
            "name may not start or end with '-'".into(),
        ));
    }
    Ok(name.to_ascii_lowercase())
}

/// Flat handles are the allocated, salable tier, so the rules are strict: lowercase ASCII
/// alphanumerics only, `FLAT_MIN..=FLAT_MAX` characters, and never a reserved name. The charset is
/// what stops a flat handle rendering as a provider path (`/gh/x` via `@`, `/` or `.`) or a
/// homograph; the reserved set is what stops squatting a namespace, brand, or fraud word.
fn validate_flat_name(name: &str) -> Result<String> {
    let lower = name.to_ascii_lowercase();
    let len = lower.chars().count();
    if len < FLAT_MIN || len > FLAT_MAX {
        return Err(RegistryError::MalformedHandle(format!(
            "a handle must be {FLAT_MIN}..={FLAT_MAX} characters"
        )));
    }
    if !lower.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(RegistryError::MalformedHandle(
            "a handle may contain only letters a-z and digits 0-9".into(),
        ));
    }
    if let Some(reason) = crate::reserved::reserved_reason(&lower) {
        return Err(RegistryError::MalformedHandle(format!("that handle is {reason}")));
    }
    Ok(lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_published_form() {
        let handle = Handle::parse("/github/superaidev").unwrap();
        assert_eq!(handle.namespace(), "github");
        assert_eq!(handle.name(), "superaidev");
        assert_eq!(handle.to_string(), "/github/superaidev");
    }

    #[test]
    fn accepts_the_leading_slash_being_omitted() {
        assert_eq!(
            Handle::parse("github/superaidev").unwrap(),
            Handle::parse("/github/superaidev").unwrap()
        );
    }

    #[test]
    fn a_bare_segment_parses_as_a_flat_handle() {
        let handle = Handle::parse("/wodoagent").unwrap();
        assert!(handle.is_flat());
        assert_eq!(handle.namespace(), FLAT_NAMESPACE);
        assert_eq!(handle.name(), "wodoagent");
        assert_eq!(handle.display(), "/wodoagent"); // bare for humans
        assert_eq!(handle.as_path(), "/handle/wodoagent"); // canonical for storage
    }

    #[test]
    fn a_flat_handle_round_trips_through_its_canonical_path() {
        let claimed = Handle::parse("/wodoagent").unwrap();
        let stored = Handle::parse(&claimed.as_path()).unwrap();
        assert_eq!(claimed, stored);
    }

    #[test]
    fn flat_handles_reject_punctuation_that_could_fake_a_provider_path() {
        // The whole point of the alphanumeric-only rule: no `@`, `/`, `.`, `-`, `_`.
        for bad in ["/bekir@gmail.com", "/gh.evil", "/my-agent", "/a_b", "/x/y/z"] {
            assert!(Handle::parse(bad).is_err(), "{bad} must be rejected");
        }
    }

    #[test]
    fn flat_handles_enforce_length_and_reserved_set() {
        assert!(Handle::parse("/ab").is_err()); // too short / reserved
        assert!(Handle::parse(&format!("/{}", "x".repeat(FLAT_MAX + 1))).is_err()); // too long
        assert!(Handle::parse("/github").is_err()); // reserved namespace name
        assert!(Handle::parse("/paypal").is_err()); // reserved brand
        assert!(Handle::parse("/g00gle").is_err()); // confusable fold
        assert!(Handle::parse("/superaidev").is_ok()); // allocatable
    }

    #[test]
    fn provider_handles_are_unaffected_by_the_flat_rules() {
        // '-' '_' '.' still fine in a provider name; reserved words don't apply there.
        assert!(Handle::parse("/github/my-agent_1.0").is_ok());
        assert!(Handle::parse("/github/admin").is_ok()); // only the GitHub user 'admin' can prove it
    }

    #[test]
    fn case_folds_so_lookalikes_are_one_handle() {
        assert_eq!(
            Handle::parse("/github/SuperAIDev").unwrap(),
            Handle::parse("/github/superaidev").unwrap(),
            "two handles a human cannot tell apart must not be two destinations"
        );
    }

    #[test]
    fn rejects_the_pre_1_0_github_abbreviation() {
        assert!(matches!(
            Handle::parse("/gh/superaidev"),
            Err(RegistryError::UnknownNamespace(namespace)) if namespace == "gh"
        ));
        assert!(Handle::new("gh", "superaidev").is_err());
    }

    #[test]
    fn rejects_namespaces_we_cannot_prove() {
        // Adding a namespace requires an adapter that can prove control of a name in it.
        assert!(matches!(
            Handle::parse("/twitter/someone"),
            Err(RegistryError::UnknownNamespace(_))
        ));
    }

    #[test]
    fn rejects_malformed_names() {
        assert!(Handle::parse("/github/").is_err());
        assert!(Handle::parse("/github/-leading").is_err());
        assert!(Handle::parse("/github/trailing-").is_err());
        assert!(Handle::parse("/github/has space").is_err());
        assert!(Handle::parse("/github/has/slash").is_err());
        assert!(Handle::parse(&format!("/github/{}", "x".repeat(MAX_NAME + 1))).is_err());
        assert!(Handle::parse("no-namespace").is_err());
    }

    #[test]
    fn accepts_names_upstream_namespaces_allow() {
        for name in ["a", "with-hyphen", "with_underscore", "with.dot", "n0mbers"] {
            assert!(Handle::new("github", name).is_ok(), "{name}");
        }
    }
}
