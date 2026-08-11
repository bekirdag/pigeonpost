//! Handles: `/github/superaidev` and `/google/…`.
//!
//! Mirrored namespaces, per `docs/architecture.md`. The scarce thing gating a registration is an
//! identity that already exists somewhere that fought spam for a decade — so a handle is never
//! *allocated* here, only *reflected*. That is what makes squatting structurally impossible
//! rather than merely discouraged.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{RegistryError, Result};

/// Namespaces we mirror. Adding one means adding an identity adapter that can prove control of a
/// name in it — never a policy decision made here.
pub const NAMESPACES: &[&str] = &["github", "google"];

/// Longest name we reflect. GitHub's own limit is 39; matching it means we never have to reject a
/// name the upstream namespace considers valid.
pub const MAX_NAME: usize = 39;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Handle {
    namespace: String,
    name: String,
}

impl Handle {
    pub fn new(namespace: &str, name: &str) -> Result<Self> {
        let namespace = namespace.to_ascii_lowercase();
        if !NAMESPACES.contains(&namespace.as_str()) {
            return Err(RegistryError::UnknownNamespace(namespace));
        }
        Ok(Handle {
            namespace,
            name: validate_name(name)?,
        })
    }

    pub fn parse(input: &str) -> Result<Self> {
        let trimmed = input.strip_prefix('/').unwrap_or(input);
        let (namespace, name) = trimmed
            .split_once('/')
            .ok_or_else(|| RegistryError::MalformedHandle("expected /<namespace>/<name>".into()))?;
        Handle::new(namespace, name)
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn as_path(&self) -> String {
        format!("/{}/{}", self.namespace, self.name)
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
