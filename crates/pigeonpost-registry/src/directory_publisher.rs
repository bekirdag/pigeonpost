//! Authentication boundary between an open directory and the shared registry log.
//!
//! Loft signatures authenticate the mutation's subject. This second signature authenticates the
//! directory process that already applied the bounded admission/probe policy before asking the
//! registry to append that exact mutation.

/// Header carrying the pinned directory publisher's lowercase-hex Ed25519 public key.
pub const DIRECTORY_PUBLISHER_KEY_HEADER: &str = "pigeonpost-directory-publisher-key";

/// Header carrying the lowercase-hex Ed25519 signature over [`mutation_request_bytes`].
pub const DIRECTORY_PUBLISHER_SIGNATURE_HEADER: &str = "pigeonpost-directory-publisher-signature";

/// The registry and directory HTTP boundaries both reject larger mutation bodies.
pub const MAX_DIRECTORY_MUTATION_BODY_BYTES: usize = 64 * 1024;

const REQUEST_DOMAIN: &[u8] = b"pigeonpost/registry-directory-publisher/v1\0";

/// Operation tag committed by a directory publisher signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryMutationOperation {
    Add,
    Remove,
}

impl DirectoryMutationOperation {
    const fn tag(self) -> u8 {
        match self {
            Self::Add => 1,
            Self::Remove => 2,
        }
    }
}

/// Strict signing representation for one exact HTTP mutation body.
///
/// The domain and operation tag prevent use as any other signature. The explicit length prevents
/// concatenation ambiguity, while retaining the exact serialized bytes makes retries immutable.
pub fn mutation_request_bytes(
    registry_origin: &str,
    operation: DirectoryMutationOperation,
    body: &[u8],
) -> Option<Vec<u8>> {
    if registry_origin.is_empty()
        || registry_origin.len() > 256
        || registry_origin.bytes().any(|byte| byte.is_ascii_control())
        || body.is_empty()
        || body.len() > MAX_DIRECTORY_MUTATION_BODY_BYTES
    {
        return None;
    }
    let origin_len = u16::try_from(registry_origin.len()).ok()?;
    let body_len = u64::try_from(body.len()).ok()?;
    let mut request =
        Vec::with_capacity(REQUEST_DOMAIN.len() + 2 + registry_origin.len() + 1 + 8 + body.len());
    request.extend_from_slice(REQUEST_DOMAIN);
    request.extend_from_slice(&origin_len.to_be_bytes());
    request.extend_from_slice(registry_origin.as_bytes());
    request.push(operation.tag());
    request.extend_from_slice(&body_len.to_be_bytes());
    request.extend_from_slice(body);
    Some(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_and_exact_body_are_bound() {
        let add = mutation_request_bytes(
            "registry.example/v1",
            DirectoryMutationOperation::Add,
            br#"{"x":1}"#,
        )
        .unwrap();
        let remove = mutation_request_bytes(
            "registry.example/v1",
            DirectoryMutationOperation::Remove,
            br#"{"x":1}"#,
        )
        .unwrap();
        let changed = mutation_request_bytes(
            "registry.example/v1",
            DirectoryMutationOperation::Add,
            br#"{"x":2}"#,
        )
        .unwrap();
        let other_origin = mutation_request_bytes(
            "staging.example/v1",
            DirectoryMutationOperation::Add,
            br#"{"x":1}"#,
        )
        .unwrap();
        assert_ne!(add, remove);
        assert_ne!(add, changed);
        assert_ne!(add, other_origin);
        assert_eq!(
            add,
            mutation_request_bytes(
                "registry.example/v1",
                DirectoryMutationOperation::Add,
                br#"{"x":1}"#
            )
            .unwrap()
        );
    }

    #[test]
    fn body_bounds_are_exact() {
        assert!(mutation_request_bytes("registry", DirectoryMutationOperation::Add, &[]).is_none());
        assert!(mutation_request_bytes(
            "registry",
            DirectoryMutationOperation::Add,
            &vec![0; MAX_DIRECTORY_MUTATION_BODY_BYTES]
        )
        .is_some());
        assert!(mutation_request_bytes(
            "registry",
            DirectoryMutationOperation::Add,
            &vec![0; MAX_DIRECTORY_MUTATION_BODY_BYTES + 1]
        )
        .is_none());
        assert!(mutation_request_bytes("", DirectoryMutationOperation::Add, b"x").is_none());
    }
}
