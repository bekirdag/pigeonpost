//! Key addresses: `/k/<26 Crockford base32 chars>`, derived from the public key.
//!
//! `addr = "/k/" + base32(SHA-256(pubkey))[..16 bytes]` — 128 bits, sized against *second*
//! preimage (2^128, targeting a specific victim). Birthday collisions between two keys an
//! attacker already holds buy nothing, so 128 bits is the number that matters. See
//! `docs/architecture.md`.

use core::fmt;

use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::b32;
use crate::error::{Error, Result};
use crate::network::{is_localhost_name, is_numeric_loopback_host};
use crate::token::Token;

/// Bytes of truncated hash in an address.
pub const ADDRESS_BYTES: usize = 16;
/// Characters after the `/k/` prefix.
pub const ADDRESS_CHARS: usize = 26;
/// Prefix marking the key-address tier.
pub const KEY_PREFIX: &str = "/k/";
/// A routing hint is user-controlled input and eventually becomes a URL. Keep it bounded here;
/// the HTTP client applies DNS/IP and redirect policy at connection time.
pub const MAX_LOFT_HINT_BYTES: usize = 2_048;
/// Keep human-readable handles small enough to use safely in URLs, state rows, and diagnostics.
pub const MAX_HANDLE_BYTES: usize = 128;
const MAX_HANDLE_NAMESPACE_BYTES: usize = 32;
const MAX_HANDLE_NAME_BYTES: usize = 39;

/// A key address. Always canonical: constructing one validates it.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Address(String);

impl Address {
    /// Derive the address for a public key. Total function — every key has exactly one address.
    pub fn from_pubkey(pubkey: &VerifyingKey) -> Self {
        let digest = Sha256::digest(pubkey.as_bytes());
        let encoded = b32::encode(&digest[..ADDRESS_BYTES]);
        debug_assert_eq!(encoded.len(), ADDRESS_CHARS);
        Address(format!("{KEY_PREFIX}{encoded}"))
    }

    /// Parse and validate. Accepts the ambiguous characters Crockford folds (`i`→`1`, `o`→`0`)
    /// but always stores the canonical form, so equality is not fooled by a transcription.
    pub fn parse(input: &str) -> Result<Self> {
        let body = input
            .strip_prefix(KEY_PREFIX)
            .ok_or(Error::MalformedAddress("missing /k/ prefix"))?;

        if body.len() != ADDRESS_CHARS {
            return Err(Error::MalformedAddress("wrong length"));
        }

        let decoded = b32::decode(body).map_err(|_| Error::MalformedAddress("bad base32"))?;
        if decoded.len() < ADDRESS_BYTES {
            return Err(Error::MalformedAddress("insufficient entropy"));
        }

        // Re-encode rather than lowercasing: this canonicalises folded characters too.
        Ok(Address(format!(
            "{KEY_PREFIX}{}",
            b32::encode(&decoded[..ADDRESS_BYTES])
        )))
    }

    /// True when this address is the one derived from `pubkey`. This is the whole verification
    /// story for tier 1 — no registry, no authority, just arithmetic.
    pub fn matches(&self, pubkey: &VerifyingKey) -> bool {
        // Addresses are public identifiers, not secrets; a plain comparison is fine here.
        *self == Address::from_pubkey(pubkey)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The identity target carried by a [`Destination`].
#[derive(Clone, PartialEq, Eq)]
pub enum DestinationTarget {
    /// A self-certifying key address that needs no registry.
    Key(Address),
    /// A canonical human-readable handle that must be resolved through a trusted registry client.
    Handle(String),
}

/// A destination is an identity target plus optional routing and authorization material.
///
/// [`Address`] deliberately remains the stable identity value. Keeping decorations in this
/// separate type prevents a parser from silently discarding a loft hint or capability token before
/// the client can use it.
#[derive(Clone, PartialEq, Eq)]
pub struct Destination {
    target: DestinationTarget,
    loft_hint: Option<String>,
    token: Option<Token>,
}

impl Destination {
    pub fn for_address(address: Address) -> Self {
        Self {
            target: DestinationTarget::Key(address),
            loft_hint: None,
            token: None,
        }
    }

    /// Build a destination for a human-readable handle. The stored value is canonical lowercase
    /// with one leading slash; supported namespaces remain a registry policy decision. The
    /// pre-1.0 `/gh` abbreviation is rejected rather than treated as an alias for `/github`.
    pub fn for_handle(handle: &str) -> Result<Self> {
        Ok(Self {
            target: DestinationTarget::Handle(canonical_handle(handle)?),
            loft_hint: None,
            token: None,
        })
    }

    /// Parse `/k/...` or `/<namespace>/<name>`, optionally followed by `?l=https://...` and/or a
    /// 32-byte `#t=<hex>` token. Unknown/duplicate decorations fail closed instead of being ignored.
    pub fn parse(input: &str) -> Result<Self> {
        let mut fragments = input.split('#');
        let before_fragment = fragments.next().unwrap_or_default();
        let fragment = fragments.next();
        if fragments.next().is_some() {
            return Err(Error::MalformedAddress("multiple fragments"));
        }

        let token = match fragment {
            Some(value) => {
                let hex = value
                    .strip_prefix("t=")
                    .ok_or(Error::MalformedAddress("unknown fragment"))?;
                Some(Token::from_hex(hex).ok_or(Error::MalformedAddress("bad capability token"))?)
            }
            None => None,
        };

        let mut queries = before_fragment.split('?');
        let bare = queries.next().unwrap_or_default();
        let query = queries.next();
        if queries.next().is_some() {
            return Err(Error::MalformedAddress("multiple queries"));
        }

        let loft_hint = match query {
            Some(value) => {
                let hint = value
                    .strip_prefix("l=")
                    .ok_or(Error::MalformedAddress("unknown query"))?;
                validate_loft_hint(hint)?;
                Some(hint.to_owned())
            }
            None => None,
        };

        let target = if bare.starts_with(KEY_PREFIX) {
            DestinationTarget::Key(Address::parse(bare)?)
        } else {
            DestinationTarget::Handle(canonical_handle(bare)?)
        };

        Ok(Self {
            target,
            loft_hint,
            token,
        })
    }

    /// Return the self-certifying address, or `None` when this destination still needs registry
    /// resolution. Callers must not silently treat a handle as an address.
    pub fn address(&self) -> Option<&Address> {
        match &self.target {
            DestinationTarget::Key(address) => Some(address),
            DestinationTarget::Handle(_) => None,
        }
    }

    pub fn handle(&self) -> Option<&str> {
        match &self.target {
            DestinationTarget::Key(_) => None,
            DestinationTarget::Handle(handle) => Some(handle),
        }
    }

    pub fn target(&self) -> &DestinationTarget {
        &self.target
    }

    pub fn loft_hint(&self) -> Option<&str> {
        self.loft_hint.as_deref()
    }

    pub fn token(&self) -> Option<&Token> {
        self.token.as_ref()
    }
}

impl From<Address> for Destination {
    fn from(address: Address) -> Self {
        Self::for_address(address)
    }
}

impl fmt::Debug for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Destination")
            .field("target", &self.target)
            .field("loft_hint", &self.loft_hint)
            .field("token", &self.token.as_ref().map(|_| "redacted"))
            .finish()
    }
}

impl fmt::Debug for DestinationTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Key(address) => f.debug_tuple("Key").field(address).finish(),
            Self::Handle(handle) => f.debug_tuple("Handle").field(handle).finish(),
        }
    }
}

/// The namespace, when `input` names a whole namespace rather than a mailbox in one — `/bekir`
/// rather than `/bekir/agent1`.
///
/// A namespace is a person or an organisation, and writing to one has an obvious meaning that the
/// two-segment form cannot express: reach whoever reads for them. Resolving that to a mailbox is a
/// server's decision, so this only recognises the shape and canonicalises it; it deliberately does
/// not build a `Destination`, because a namespace is not a delivery target on its own.
///
/// `/bekir/` is the same thing as `/bekir`. A trailing slash is a typo, not a different address.
pub fn namespace_root(input: &str) -> Option<String> {
    if input.is_empty() || input.len() > MAX_HANDLE_BYTES || !input.is_ascii() {
        return None;
    }
    let trimmed = input.strip_prefix('/')?;
    let namespace = trimmed.strip_suffix('/').unwrap_or(trimmed);
    if namespace.is_empty()
        || namespace.len() > MAX_HANDLE_NAMESPACE_BYTES
        || namespace.contains('/')
        || namespace.eq_ignore_ascii_case("gh")
        || namespace.starts_with('-')
        || namespace.ends_with('-')
    {
        return None;
    }
    if !namespace
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return None;
    }
    // A key address is not a namespace, however much `/k` looks like one.
    if namespace.eq_ignore_ascii_case("k") {
        return None;
    }
    Some(namespace.to_ascii_lowercase())
}

/// Canonicalise a handle: `/<namespace>/<name>` or `/<namespace>/<name>/<agent>`.
///
/// Two shapes, one grammar. `/bekir/main` is a name under a namespace somebody owns; `/github/alex`
/// is a name under a namespace nobody owns, earned by proving an identity. Both identify one
/// person, and either may hold a third segment for that person's agents — `/github/alex/agent1` is
/// Alex's, exactly as `/bekir/docdex` is Bekir's.
///
/// `@` is allowed inside a segment so an account that signed in with an address can carry a handle
/// that reads like one: `/pp/alex@example.com`. It is not permitted at the edges of a segment,
/// because a name that begins or ends with it is a name somebody typed wrong.
///
/// Widened 2026-08-21 from a strict two-segment, `@`-free grammar. Older clients treat the new
/// shapes as malformed rather than as unknown, which is the cost of the change and the reason it is
/// written down here.
fn canonical_handle(input: &str) -> Result<String> {
    if input.is_empty() || input.len() > MAX_HANDLE_BYTES || !input.is_ascii() {
        return Err(Error::MalformedAddress("bad handle length"));
    }
    let trimmed = input.strip_prefix('/').unwrap_or(input);
    let segments: Vec<&str> = trimmed.split('/').collect();
    let (namespace, rest) = match segments.as_slice() {
        [namespace, name] => (*namespace, vec![*name]),
        [namespace, name, agent] => (*namespace, vec![*name, *agent]),
        [_] => return Err(Error::MalformedAddress("expected /<namespace>/<name>")),
        _ => return Err(Error::MalformedAddress("too many segments in handle")),
    };
    if namespace.eq_ignore_ascii_case("gh") {
        return Err(Error::MalformedAddress(
            "legacy /gh namespace is not supported; use /github",
        ));
    }
    if namespace.is_empty() || namespace.len() > MAX_HANDLE_NAMESPACE_BYTES {
        return Err(Error::MalformedAddress("bad handle shape"));
    }
    // A namespace never carries an address: it is the part somebody owns or a provider's name, and
    // both are plain words.
    if !namespace.bytes().all(plain_byte) || edge_marked(namespace) {
        return Err(Error::MalformedAddress("bad handle characters"));
    }
    for segment in &rest {
        if segment.is_empty() || segment.len() > MAX_HANDLE_NAME_BYTES {
            return Err(Error::MalformedAddress("bad handle shape"));
        }
        if !segment.bytes().all(name_byte) || edge_marked(segment) {
            return Err(Error::MalformedAddress("bad handle characters"));
        }
        // One `@` at most, and not as the whole name: `alex@example.com` is an address, `a@b@c` is
        // a typo, and `@` alone is nothing at all.
        if segment.bytes().filter(|b| *b == b'@').count() > 1 {
            return Err(Error::MalformedAddress("bad handle characters"));
        }
    }

    let mut canonical = String::with_capacity(input.len() + 1);
    canonical.push('/');
    canonical.push_str(&namespace.to_ascii_lowercase());
    for segment in rest {
        canonical.push('/');
        canonical.push_str(&segment.to_ascii_lowercase());
    }
    Ok(canonical)
}

/// What a namespace may contain.
fn plain_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
}

/// What a name or an agent segment may contain: the same, plus `@` for address-shaped handles.
fn name_byte(byte: u8) -> bool {
    plain_byte(byte) || byte == b'@'
}

/// Whether a segment starts or ends with punctuation. A name is a thing people read out; one that
/// begins with a dash or ends with a dot is a mistake being preserved.
fn edge_marked(segment: &str) -> bool {
    let edges = |c: char| matches!(c, '-' | '.' | '@');
    segment.starts_with(edges) || segment.ends_with(edges)
}

fn validate_loft_hint(hint: &str) -> Result<()> {
    if hint.is_empty() || hint.len() > MAX_LOFT_HINT_BYTES {
        return Err(Error::MalformedAddress("bad loft hint length"));
    }
    if hint.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(Error::MalformedAddress("bad loft hint"));
    }
    let url = url::Url::parse(hint).map_err(|_| Error::MalformedAddress("bad loft hint URL"))?;
    if url.cannot_be_a_base()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host_str().is_none()
        || url.port() == Some(0)
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::MalformedAddress("loft hint must be an origin"));
    }
    let host = url
        .host_str()
        .ok_or(Error::MalformedAddress("loft hint has no host"))?;
    if is_localhost_name(host) {
        return Err(Error::MalformedAddress("loft hint cannot use localhost"));
    }
    if url.scheme() != "https" && !(url.scheme() == "http" && is_numeric_loopback_host(host)) {
        return Err(Error::MalformedAddress("loft hint must use HTTPS"));
    }
    Ok(())
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Address({})", self.0)
    }
}

impl TryFrom<String> for Address {
    type Error = Error;
    fn try_from(value: String) -> Result<Self> {
        Address::parse(&value)
    }
}

impl From<Address> for String {
    fn from(value: Address) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::Identity;

    fn identity(seed: u8) -> Identity {
        Identity::from_seed([seed; 32])
    }

    /// Writing to a person rather than to one of their agents. `/bekir` is the address someone
    /// puts in a README; it has to be recognised as a namespace and not mistaken for a mailbox.
    #[test]
    fn a_bare_namespace_is_recognised_and_canonicalised() {
        assert_eq!(namespace_root("/bekir").as_deref(), Some("bekir"));
        assert_eq!(namespace_root("/Bekir").as_deref(), Some("bekir"));
        // A trailing slash is a typo, not a second address.
        assert_eq!(namespace_root("/bekir/").as_deref(), Some("bekir"));
        assert_eq!(
            namespace_root("bekir"),
            None,
            "a handle always leads with /"
        );
    }

    /// A namespace is not a mailbox and a mailbox is not a namespace. Confusing the two would make
    /// `/bekir/agent1` deliverable as a namespace, or `/bekir` mintable as a handle.
    #[test]
    fn a_mailbox_handle_is_not_a_namespace_and_vice_versa() {
        assert_eq!(namespace_root("/bekir/agent1"), None);
        assert!(canonical_handle("/bekir").is_err());
        // `/k/…` is a key address; reading it as the namespace "k" would route real mail to a
        // namespace inbox.
        assert_eq!(namespace_root("/k"), None);
        assert_eq!(namespace_root("/k/abc"), None);
    }

    /// The same shape rules the two-segment form enforces, so a namespace cannot smuggle in
    /// characters a handle would reject.
    #[test]
    fn a_namespace_obeys_the_handle_character_rules() {
        assert_eq!(namespace_root("/has space"), None);
        assert_eq!(namespace_root("/-leading"), None);
        assert_eq!(namespace_root("/trailing-"), None);
        assert_eq!(namespace_root("/"), None);
        assert_eq!(
            namespace_root("/gh"),
            None,
            "the pre-1.0 abbreviation stays refused"
        );
        assert_eq!(
            namespace_root(&format!("/{}", "x".repeat(MAX_HANDLE_NAMESPACE_BYTES + 1))),
            None
        );
    }

    #[test]
    fn derivation_is_deterministic_and_correctly_shaped() {
        let id = identity(1);
        let a = Address::from_pubkey(&id.verifying_key());
        let b = Address::from_pubkey(&id.verifying_key());
        assert_eq!(a, b);
        assert!(a.as_str().starts_with(KEY_PREFIX));
        assert_eq!(a.as_str().len(), KEY_PREFIX.len() + ADDRESS_CHARS);
    }

    #[test]
    fn distinct_keys_give_distinct_addresses() {
        let a = Address::from_pubkey(&identity(1).verifying_key());
        let b = Address::from_pubkey(&identity(2).verifying_key());
        assert_ne!(a, b);
    }

    #[test]
    fn round_trips_through_parse() {
        let addr = Address::from_pubkey(&identity(7).verifying_key());
        assert_eq!(Address::parse(addr.as_str()).unwrap(), addr);
    }

    #[test]
    fn matches_only_its_own_key() {
        let mine = identity(3);
        let other = identity(4);
        let addr = Address::from_pubkey(&mine.verifying_key());
        assert!(addr.matches(&mine.verifying_key()));
        assert!(!addr.matches(&other.verifying_key()));
    }

    #[test]
    fn folded_characters_canonicalise_to_the_same_address() {
        let addr = Address::from_pubkey(&identity(9).verifying_key());
        let mangled = addr.as_str().replace('1', "l").replace('0', "O");
        assert_eq!(Address::parse(&mangled).unwrap(), addr);
    }

    #[test]
    fn decorations_are_retained_outside_identity() {
        let addr = Address::from_pubkey(&identity(5).verifying_key());
        let token = Token::mint(&[8; 32], "readme");
        let decorated = format!("{addr}?l=https://loft.example.com#t={}", token.to_hex());
        assert!(Address::parse(&decorated).is_err());
        let destination = Destination::parse(&decorated).unwrap();
        assert_eq!(destination.address(), Some(&addr));
        assert_eq!(destination.handle(), None);
        assert_eq!(destination.loft_hint(), Some("https://loft.example.com"));
        assert_eq!(destination.token(), Some(&token));
    }

    #[test]
    fn handle_destinations_are_canonical_and_keep_decorations() {
        let token = Token::mint(&[9; 32], "handle-route");
        let destination = Destination::parse(&format!(
            "/GITHUB/Alice_One?l=https://loft.example.com#t={}",
            token.to_hex()
        ))
        .unwrap();
        assert_eq!(destination.address(), None);
        assert_eq!(destination.handle(), Some("/github/alice_one"));
        assert_eq!(destination.loft_hint(), Some("https://loft.example.com"));
        assert_eq!(destination.token(), Some(&token));
    }

    #[test]
    fn destination_rejects_ignored_or_unsafe_decorations() {
        let addr = Address::from_pubkey(&identity(5).verifying_key());
        assert!(Destination::parse(&format!("{addr}?x=https://loft.example")).is_err());
        assert!(Destination::parse(&format!("{addr}#x=00")).is_err());
        assert!(Destination::parse(&format!("{addr}#t=readme")).is_err());
        assert!(Destination::parse(&format!("{addr}?l=http://example.com")).is_err());
        assert!(Destination::parse(&format!("{addr}?l=http://localhost")).is_err());
        assert!(Destination::parse(&format!("{addr}?l=https://localhost")).is_err());
        assert!(Destination::parse(&format!("{addr}?l=https://localhost.")).is_err());
        assert!(Destination::parse(&format!("{addr}?l=https://api.localhost")).is_err());
        assert!(Destination::parse(&format!("{addr}?l=https://API.LOCALHOST.")).is_err());
        assert!(Destination::parse(&format!("{addr}?l=http://localhost.evil")).is_err());
        assert!(Destination::parse(&format!("{addr}?l=https://loft.example/path")).is_err());
        assert!(Destination::parse(&format!("{addr}?l=https://user:pass@example.com")).is_err());
        assert!(Destination::parse(&format!("{addr}?l=https://a?l=https://b")).is_err());
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(Address::parse("no-prefix").is_err());
        assert!(Address::parse("/k/tooshort").is_err());
        assert!(Address::parse("/k/uuuuuuuuuuuuuuuuuuuuuuuuuu").is_err());
        assert!(Address::parse("/github/someone").is_err());
        assert!(Destination::parse("/github/").is_err());
        assert!(Destination::parse("/github/-leading").is_err());
        assert!(Destination::parse("/gh/someone").is_err());
        // Four segments is not a deeper fleet, it is a typo. Three is the floor and the ceiling.
        assert!(Destination::parse("/github/alex/agent1/extra").is_err());
    }

    /// The grammar widened on 2026-08-21: a third segment for a person's own agents, and `@` inside
    /// a name so an account that signed in with an address can carry a handle that reads like one.
    #[test]
    fn accepts_agent_segments_and_address_shaped_names() {
        let agent = Destination::parse("/github/alex/agent1").expect("an agent under a person");
        assert_eq!(agent.handle(), Some("/github/alex/agent1"));

        let owned = Destination::parse("/bekir/docdex/scratch").expect("an agent under a name");
        assert_eq!(owned.handle(), Some("/bekir/docdex/scratch"));

        let addressed = Destination::parse("/pp/alex@example.com").expect("an address-shaped name");
        assert_eq!(addressed.handle(), Some("/pp/alex@example.com"));

        // Case folds as it always did, third segment included.
        let mixed = Destination::parse("/GitHub/Alex/Agent1").expect("mixed case");
        assert_eq!(mixed.handle(), Some("/github/alex/agent1"));
    }

    #[test]
    fn refuses_addresses_that_are_not_names() {
        // `@` earns its place in the middle of a name and nowhere else.
        assert!(Destination::parse("/pp/@alex").is_err());
        assert!(Destination::parse("/pp/alex@").is_err());
        assert!(Destination::parse("/pp/a@b@c").is_err());
        // A namespace is a word somebody owns or a provider's name; neither is an address.
        assert!(Destination::parse("/alex@example.com/agent").is_err());
    }
}
