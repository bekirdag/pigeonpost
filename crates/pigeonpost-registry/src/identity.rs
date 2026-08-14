//! Identity proof adapters with bounded OAuth/OIDC verification.
//!
//! GitHub user identity is OAuth authorization-code exchange only.  A submitted code is never
//! treated as a bearer assertion. Google is OIDC: its JWT signature and every security-relevant
//! claim are checked against an explicit issuer allowlist and a bounded, cached JWKS.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use reqwest::header::{CACHE_CONTROL, ETAG, IF_NONE_MATCH};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use zeroize::Zeroizing;

use crate::error::{RegistryError, Result};
use crate::log::Hash;

const PROVIDER_RESPONSE_LIMIT: usize = 64 * 1024;
const JWKS_RESPONSE_LIMIT: usize = 256 * 1024;
const MAX_JWKS_KEYS: usize = 32;
const DEFAULT_JWKS_TTL: Duration = Duration::from_secs(300);
const MIN_JWKS_TTL: Duration = Duration::from_secs(30);
const MAX_JWKS_TTL: Duration = Duration::from_secs(3_600);
const MAX_ID_TOKEN_AGE: u64 = 10 * 60;
const MAX_ID_TOKEN_LIFETIME: u64 = 2 * 60 * 60;
const CLOCK_SKEW: u64 = 30;
const MAX_CONCURRENT_OIDC_VERIFICATIONS: usize = 4;

/// Authorization endpoints are protocol constants, not provider-supplied discovery results.
#[derive(Clone, PartialEq, Eq)]
pub struct Subject {
    pub namespace: &'static str,
    /// Current human-facing provider name used to authorize the requested handle.
    pub name: String,
    /// Stable opaque provider subject used in the permanent log and uniqueness projection.
    pub opaque_id: String,
}

impl std::fmt::Debug for Subject {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Subject")
            .field("identity", &"<redacted>")
            .finish_non_exhaustive()
    }
}

/// Provider-tagged proof. All fields are mandatory and unknown fields fail closed.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProofPayload {
    /// Authorization code from GitHub plus the RFC 7636 verifier and server-created state.
    Github {
        code: String,
        code_verifier: String,
        state: String,
    },
    /// Google ID token plus the server-created nonce embedded in that token.
    Google { id_token: String, nonce: String },
    /// Test-only and absent from production deserialization/code generation.
    #[cfg(any(test, feature = "test-utilities"))]
    Mock { name: String },
}

impl std::fmt::Debug for ProofPayload {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Github { .. } => formatter.write_str("ProofPayload::Github { .. }"),
            Self::Google { .. } => formatter.write_str("ProofPayload::Google { .. }"),
            #[cfg(any(test, feature = "test-utilities"))]
            Self::Mock { .. } => formatter.write_str("ProofPayload::Mock { .. }"),
        }
    }
}

impl ProofPayload {
    pub(crate) fn provider_slug(&self) -> &'static str {
        match self {
            Self::Github { .. } => "github",
            Self::Google { .. } => "google",
            #[cfg(any(test, feature = "test-utilities"))]
            Self::Mock { .. } => "mock",
        }
    }

    pub(crate) fn challenge_token(&self) -> Option<&str> {
        match self {
            Self::Github { state, .. } => Some(state),
            Self::Google { nonce, .. } => Some(nonce),
            #[cfg(any(test, feature = "test-utilities"))]
            Self::Mock { .. } => None,
        }
    }

    pub(crate) fn expected_pkce_challenge(&self) -> Result<Option<String>> {
        match self {
            Self::Github { code_verifier, .. } => Ok(Some(pkce_s256(code_verifier)?)),
            Self::Google { .. } => Ok(None),
            #[cfg(any(test, feature = "test-utilities"))]
            Self::Mock { .. } => Ok(None),
        }
    }

    pub(crate) fn is_test_mock(&self) -> bool {
        #[cfg(any(test, feature = "test-utilities"))]
        if matches!(self, Self::Mock { .. }) {
            return true;
        }
        false
    }
}

#[async_trait]
pub trait IdentityProvider: Send + Sync {
    fn namespace(&self) -> &'static str;
    /// Public OAuth/OIDC client identifier. This is intentionally the only provider
    /// configuration exposed to challenge callers; client secrets never cross this boundary.
    fn public_client_id(&self) -> Option<&str> {
        None
    }
    async fn verify(&self, proof: &ProofPayload) -> Result<Subject>;
}

// ---- GitHub OAuth ---------------------------------------------------------------------------

pub struct GithubProvider {
    client_id: String,
    client_secret: Zeroizing<String>,
    http: reqwest::Client,
    token_url: String,
    user_url: String,
}

impl std::fmt::Debug for GithubProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never expose the client secret through Debug.
        f.debug_struct("GithubProvider")
            .field("client_id", &self.client_id)
            .field("token_url", &self.token_url)
            .field("user_url", &self.user_url)
            .finish_non_exhaustive()
    }
}

impl GithubProvider {
    pub fn new(client_id: impl Into<String>, client_secret: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            client_secret: Zeroizing::new(client_secret.into()),
            http: bounded_client(),
            token_url: "https://github.com/login/oauth/access_token".into(),
            user_url: "https://api.github.com/user".into(),
        }
    }

    /// Test-only endpoint override. Production construction keeps both endpoints pinned.
    #[cfg(any(test, feature = "test-utilities"))]
    pub fn with_endpoints(mut self, token_url: String, user_url: String) -> Self {
        self.token_url = token_url;
        self.user_url = user_url;
        self
    }
}

#[derive(Deserialize)]
struct GithubToken {
    access_token: Option<String>,
    token_type: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct GithubUser {
    login: String,
    id: u64,
}

#[async_trait]
impl IdentityProvider for GithubProvider {
    fn namespace(&self) -> &'static str {
        "github"
    }

    fn public_client_id(&self) -> Option<&str> {
        Some(&self.client_id)
    }

    async fn verify(&self, proof: &ProofPayload) -> Result<Subject> {
        let ProofPayload::Github {
            code,
            code_verifier,
            state: _,
        } = proof
        else {
            return Err(RegistryError::WrongProvider);
        };
        validate_oauth_code(code)?;
        validate_pkce_verifier(code_verifier)?;

        // Authorization-code exchange is mandatory. There is intentionally no branch that sends
        // `code` as a bearer credential.
        let response = self
            .http
            .post(&self.token_url)
            .header("accept", "application/json")
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("code", code.as_str()),
                ("code_verifier", code_verifier.as_str()),
            ])
            .send()
            .await
            .map_err(|_| provider_unreachable("GitHub token exchange"))?;
        if response.status().is_client_error() {
            return Err(RegistryError::ProofRejected(
                "GitHub authorization code was rejected".into(),
            ));
        }
        if !response.status().is_success() {
            return Err(provider_unreachable("GitHub token exchange"));
        }
        let token: GithubToken =
            bounded_json(response, PROVIDER_RESPONSE_LIMIT, "GitHub token").await?;
        if token.error.is_some() {
            return Err(RegistryError::ProofRejected(
                "GitHub authorization code was rejected".into(),
            ));
        }
        if !token
            .token_type
            .as_deref()
            .is_some_and(|kind| kind.eq_ignore_ascii_case("bearer"))
        {
            return Err(RegistryError::ProofRejected(
                "GitHub returned an unsupported token type".into(),
            ));
        }
        let access_token = token.access_token.filter(|value| {
            !value.is_empty()
                && value.len() <= 2_048
                && !value.bytes().any(|b| b.is_ascii_control())
        });
        let access_token = access_token.ok_or_else(|| {
            RegistryError::ProofRejected("GitHub did not return a usable access token".into())
        })?;

        let response = self
            .http
            .get(&self.user_url)
            .header("accept", "application/vnd.github+json")
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|_| provider_unreachable("GitHub user lookup"))?;
        if response.status().is_client_error() {
            return Err(RegistryError::ProofRejected(
                "GitHub user lookup rejected the exchanged token".into(),
            ));
        }
        if !response.status().is_success() {
            return Err(provider_unreachable("GitHub user lookup"));
        }
        let user: GithubUser =
            bounded_json(response, PROVIDER_RESPONSE_LIMIT, "GitHub user").await?;
        validate_github_login(&user.login)?;
        if user.id == 0 {
            return Err(RegistryError::ProofRejected(
                "GitHub returned a malformed account id".into(),
            ));
        }
        Ok(Subject {
            namespace: "github",
            name: user.login,
            opaque_id: user.id.to_string(),
        })
    }
}

// ---- Google OIDC ---------------------------------------------------------------------------

pub struct GoogleProvider {
    client_id: String,
    issuers: Vec<String>,
    jwks_url: String,
    http: reqwest::Client,
    jwks: Mutex<Option<CachedJwks>>,
    verification_lane: OidcVerificationLane,
}

#[derive(Clone)]
struct OidcVerificationLane {
    permits: Arc<Semaphore>,
}

impl OidcVerificationLane {
    fn new(max_concurrent: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(max_concurrent)),
        }
    }

    async fn run<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        let permit = Arc::clone(&self.permits)
            .try_acquire_owned()
            .map_err(|_| RegistryError::Overloaded)?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            operation()
        })
        .await
        .map_err(|_| RegistryError::Overloaded)?
    }
}

impl std::fmt::Debug for GoogleProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleProvider")
            .field("client_id", &self.client_id)
            .field("issuers", &self.issuers)
            .field("jwks_url", &self.jwks_url)
            .finish_non_exhaustive()
    }
}

impl GoogleProvider {
    pub fn new(client_id: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            issuers: vec![
                "https://accounts.google.com".into(),
                "accounts.google.com".into(),
            ],
            jwks_url: "https://www.googleapis.com/oauth2/v3/certs".into(),
            http: bounded_client(),
            jwks: Mutex::new(None),
            verification_lane: OidcVerificationLane::new(MAX_CONCURRENT_OIDC_VERIFICATIONS),
        }
    }

    /// Test-only JWKS endpoint override. Issuers remain independently pinned.
    #[cfg(any(test, feature = "test-utilities"))]
    pub fn with_jwks_url(mut self, url: String) -> Self {
        self.jwks_url = url;
        self
    }

    /// Replace the exact issuer allowlist. Empty, duplicate, non-HTTPS, or fragment/query issuers
    /// are rejected so configuration cannot silently enable dynamic issuer selection.
    pub fn with_issuers(mut self, issuers: Vec<String>) -> Result<Self> {
        validate_issuer_allowlist(&issuers)?;
        self.issuers = issuers;
        Ok(self)
    }

    async fn signing_key(&self, kid: &str) -> Result<Jwk> {
        if let Some(key) = self.cached_key(kid, true) {
            return Ok(key);
        }
        self.refresh_jwks(false).await?;
        if let Some(key) = self.cached_key(kid, false) {
            return Ok(key);
        }
        // A fresh-but-missing kid can be a key rotation. One unconditional bounded refresh is
        // allowed; an attacker cannot turn arbitrary kids into unbounded fetch loops.
        self.refresh_jwks(true).await?;
        self.cached_key(kid, false)
            .ok_or_else(|| RegistryError::ProofRejected("unknown OIDC signing key".into()))
    }

    fn cached_key(&self, kid: &str, require_fresh: bool) -> Option<Jwk> {
        let cache = self.jwks.lock().unwrap_or_else(|error| error.into_inner());
        let cached = cache.as_ref()?;
        if require_fresh && Instant::now() >= cached.expires_at {
            return None;
        }
        cached.keys.iter().find(|key| key.kid == kid).cloned()
    }

    async fn refresh_jwks(&self, unconditional: bool) -> Result<()> {
        let snapshot = self
            .jwks
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if !unconditional
            && snapshot
                .as_ref()
                .is_some_and(|cache| Instant::now() < cache.expires_at)
        {
            return Ok(());
        }

        let mut request = self
            .http
            .get(&self.jwks_url)
            .header("accept", "application/json");
        if !unconditional {
            if let Some(etag) = snapshot.as_ref().and_then(|cache| cache.etag.as_deref()) {
                request = request.header(IF_NONE_MATCH, etag);
            }
        }
        let response = request
            .send()
            .await
            .map_err(|_| provider_unreachable("OIDC JWKS fetch"))?;
        let ttl = cache_ttl(
            response
                .headers()
                .get(CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
        );
        if response.status() == reqwest::StatusCode::NOT_MODIFIED {
            let mut cache = self.jwks.lock().unwrap_or_else(|error| error.into_inner());
            let existing = cache.as_mut().ok_or_else(|| {
                RegistryError::ProviderUnreachable("OIDC JWKS returned an invalid response".into())
            })?;
            existing.expires_at = Instant::now() + ttl;
            return Ok(());
        }
        if !response.status().is_success() {
            return Err(provider_unreachable("OIDC JWKS fetch"));
        }
        let etag = response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .filter(|value| value.len() <= 256)
            .map(ToOwned::to_owned);
        let document: Jwks = bounded_json(response, JWKS_RESPONSE_LIMIT, "OIDC JWKS").await?;
        let keys = validate_jwks(document)?;
        *self.jwks.lock().unwrap_or_else(|error| error.into_inner()) = Some(CachedJwks {
            keys,
            etag,
            expires_at: Instant::now() + ttl,
        });
        Ok(())
    }
}

#[derive(Clone)]
struct CachedJwks {
    keys: Vec<Jwk>,
    etag: Option<String>,
    expires_at: Instant,
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Clone, Deserialize)]
struct Jwk {
    kid: String,
    kty: String,
    alg: Option<String>,
    #[serde(rename = "use")]
    key_use: Option<String>,
    n: String,
    e: String,
}

#[derive(Deserialize)]
struct GoogleClaims {
    iss: String,
    aud: Audience,
    sub: String,
    exp: u64,
    iat: u64,
    nonce: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

impl Audience {
    fn is_exactly(&self, expected: &str) -> bool {
        match self {
            Self::One(value) => value == expected,
            Self::Many(values) => values.len() == 1 && values[0] == expected,
        }
    }
}

#[async_trait]
impl IdentityProvider for GoogleProvider {
    fn namespace(&self) -> &'static str {
        "google"
    }

    fn public_client_id(&self) -> Option<&str> {
        Some(&self.client_id)
    }

    async fn verify(&self, proof: &ProofPayload) -> Result<Subject> {
        let ProofPayload::Google { id_token, nonce } = proof else {
            return Err(RegistryError::WrongProvider);
        };
        if id_token.is_empty() || id_token.len() > 16 * 1024 {
            return Err(RegistryError::ProofRejected(
                "OIDC token length is invalid".into(),
            ));
        }
        validate_challenge_token(nonce)?;

        let header_token = id_token.clone();
        let kid = self
            .verification_lane
            .run(move || oidc_key_id(&header_token))
            .await?;
        let jwk = self.signing_key(&kid).await?;
        let token = id_token.clone();
        let expected_nonce = nonce.clone();
        let client_id = self.client_id.clone();
        let issuers = self.issuers.clone();
        self.verification_lane
            .run(move || verify_google_token(&token, &expected_nonce, &client_id, &issuers, &jwk))
            .await
    }
}

fn oidc_key_id(id_token: &str) -> Result<String> {
    let header = jsonwebtoken::decode_header(id_token)
        .map_err(|_| RegistryError::ProofRejected("malformed OIDC token".into()))?;
    if header.alg != jsonwebtoken::Algorithm::RS256 {
        return Err(RegistryError::ProofRejected(
            "OIDC token uses an unsupported algorithm".into(),
        ));
    }
    header
        .kid
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        })
        .ok_or_else(|| RegistryError::ProofRejected("OIDC token has an invalid key id".into()))
}

fn verify_google_token(
    id_token: &str,
    nonce: &str,
    client_id: &str,
    issuers: &[String],
    jwk: &Jwk,
) -> Result<Subject> {
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
    validation.algorithms = vec![jsonwebtoken::Algorithm::RS256];
    validation.set_audience(&[client_id]);
    validation.set_issuer(&issuers.iter().map(String::as_str).collect::<Vec<_>>());
    validation.required_spec_claims = HashSet::from_iter(
        ["iss", "aud", "sub", "exp", "iat", "nonce"]
            .into_iter()
            .map(ToOwned::to_owned),
    );
    validation.leeway = CLOCK_SKEW;
    validation.validate_exp = true;
    validation.validate_aud = true;

    let key = jsonwebtoken::DecodingKey::from_rsa_components(&jwk.n, &jwk.e)
        .map_err(|_| RegistryError::ProofRejected("OIDC signing key is malformed".into()))?;
    let token = jsonwebtoken::decode::<GoogleClaims>(id_token, &key, &validation)
        .map_err(|_| RegistryError::ProofRejected("OIDC token verification failed".into()))?;
    let claims = token.claims;
    if !issuers.iter().any(|issuer| issuer == &claims.iss)
        || !claims.aud.is_exactly(client_id)
        || claims.nonce != nonce
    {
        return Err(RegistryError::ProofRejected(
            "OIDC token claims do not match this registration".into(),
        ));
    }
    let now = unix_seconds();
    if claims.exp <= now.saturating_sub(CLOCK_SKEW)
        || claims.iat > now.saturating_add(CLOCK_SKEW)
        || now.saturating_sub(claims.iat) > MAX_ID_TOKEN_AGE
        || claims.exp <= claims.iat
        || claims.exp - claims.iat > MAX_ID_TOKEN_LIFETIME
    {
        return Err(RegistryError::ProofRejected(
            "OIDC token validity window is unacceptable".into(),
        ));
    }
    if claims.sub.is_empty()
        || claims.sub.len() > 255
        || claims.sub.bytes().any(|b| b.is_ascii_control())
    {
        return Err(RegistryError::ProofRejected(
            "OIDC subject is malformed".into(),
        ));
    }
    Ok(Subject {
        namespace: "google",
        name: claims.sub.clone(),
        opaque_id: claims.sub,
    })
}

// ---- Mock -----------------------------------------------------------------------------------

#[cfg(any(test, feature = "test-utilities"))]
#[derive(Debug)]
pub struct MockProvider;

#[cfg(any(test, feature = "test-utilities"))]
#[async_trait]
impl IdentityProvider for MockProvider {
    fn namespace(&self) -> &'static str {
        "github"
    }

    async fn verify(&self, proof: &ProofPayload) -> Result<Subject> {
        match proof {
            ProofPayload::Mock { name }
                if !name.is_empty()
                    && name.len() <= 255
                    && !name.bytes().any(|b| b.is_ascii_control()) =>
            {
                Ok(Subject {
                    namespace: "github",
                    name: name.clone(),
                    opaque_id: name.clone(),
                })
            }
            ProofPayload::Mock { .. } => Err(RegistryError::ProofRejected(
                "mock subject is malformed".into(),
            )),
            _ => Err(RegistryError::WrongProvider),
        }
    }
}

pub(crate) fn challenge_hash(provider: &str, token: &str) -> Hash {
    let mut hasher = Sha256::new();
    hasher.update(b"pigeonpost/identity-challenge/v1\0");
    hasher.update(provider.as_bytes());
    hasher.update([0]);
    hasher.update(token.as_bytes());
    hasher.finalize().into()
}

pub fn pkce_s256(verifier: &str) -> Result<String> {
    validate_pkce_verifier(verifier)?;
    Ok(base64url(&Sha256::digest(verifier.as_bytes())))
}

pub(crate) fn validate_pkce_challenge(challenge: &str) -> Result<()> {
    if challenge.len() != 43
        || !challenge
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(RegistryError::ProofRejected(
            "PKCE S256 challenge is malformed".into(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_challenge_token(token: &str) -> Result<()> {
    if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(RegistryError::ProofRejected(
            "identity challenge is malformed".into(),
        ));
    }
    Ok(())
}

fn validate_pkce_verifier(verifier: &str) -> Result<()> {
    if !(43..=128).contains(&verifier.len())
        || !verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
    {
        return Err(RegistryError::ProofRejected(
            "PKCE verifier is malformed".into(),
        ));
    }
    Ok(())
}

fn validate_oauth_code(code: &str) -> Result<()> {
    if code.is_empty() || code.len() > 2_048 || code.bytes().any(|b| b.is_ascii_control()) {
        return Err(RegistryError::ProofRejected(
            "OAuth authorization code is malformed".into(),
        ));
    }
    Ok(())
}

fn validate_github_login(login: &str) -> Result<()> {
    if login.is_empty()
        || login.len() > 39
        || login.starts_with('-')
        || login.ends_with('-')
        || !login
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(RegistryError::ProofRejected(
            "GitHub returned a malformed login".into(),
        ));
    }
    Ok(())
}

fn validate_issuer_allowlist(issuers: &[String]) -> Result<()> {
    if issuers.is_empty() || issuers.len() > 8 {
        return Err(RegistryError::ProofRejected(
            "OIDC issuer allowlist must contain 1..=8 issuers".into(),
        ));
    }
    let mut unique = HashSet::new();
    for issuer in issuers {
        if issuer.len() > 256
            || !issuer.starts_with("https://")
            || issuer.contains('?')
            || issuer.contains('#')
            || issuer.ends_with('/')
            || !unique.insert(issuer)
        {
            return Err(RegistryError::ProofRejected(
                "OIDC issuer allowlist contains an invalid issuer".into(),
            ));
        }
    }
    Ok(())
}

fn validate_jwks(document: Jwks) -> Result<Vec<Jwk>> {
    if document.keys.is_empty() || document.keys.len() > MAX_JWKS_KEYS {
        return Err(provider_unreachable("OIDC JWKS validation"));
    }
    let mut seen = HashSet::new();
    let mut keys = Vec::new();
    for key in document.keys {
        if key.kty != "RSA"
            || key.alg.as_deref().is_some_and(|alg| alg != "RS256")
            || key.key_use.as_deref().is_some_and(|usage| usage != "sig")
            || key.kid.is_empty()
            || key.kid.len() > 128
            || !seen.insert(key.kid.clone())
            || key.n.is_empty()
            || key.n.len() > 1_024
            || key.e.is_empty()
            || key.e.len() > 16
            || !key
                .n
                .bytes()
                .chain(key.e.bytes())
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(provider_unreachable("OIDC JWKS validation"));
        }
        keys.push(key);
    }
    Ok(keys)
}

const PROVIDER_USER_AGENT: &str = concat!("pigeonpost-registry/", env!("CARGO_PKG_VERSION"));

fn bounded_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::none())
        // Provider exchanges carry identity assertions and, for GitHub, the configured client
        // secret. Ambient proxy variables must never redirect that trust boundary.
        .no_proxy()
        .user_agent(PROVIDER_USER_AGENT)
        .build()
        .expect("static bounded HTTP client configuration is valid")
}

async fn bounded_json<T: DeserializeOwned>(
    mut response: reqwest::Response,
    limit: usize,
    context: &'static str,
) -> Result<T> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(provider_unreachable(context));
    }
    let mut body =
        Vec::with_capacity(response.content_length().unwrap_or(0).min(limit as u64) as usize);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| provider_unreachable(context))?
    {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(provider_unreachable(context));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| provider_unreachable(context))
}

fn cache_ttl(cache_control: Option<&str>) -> Duration {
    let seconds = cache_control
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .find_map(|directive| directive.strip_prefix("max-age=")?.parse::<u64>().ok());
    Duration::from_secs(
        seconds
            .unwrap_or(DEFAULT_JWKS_TTL.as_secs())
            .clamp(MIN_JWKS_TTL.as_secs(), MAX_JWKS_TTL.as_secs()),
    )
}

fn provider_unreachable(context: &str) -> RegistryError {
    // Static context only: never propagate provider bodies, tokens, codes, URLs with queries, or
    // client secrets into public errors or logs.
    RegistryError::ProviderUnreachable(format!("{context} failed"))
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn base64url(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let bytes = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let value = (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]);
        out.push(ALPHABET[((value >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((value >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((value >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(value & 63) as usize] as char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_user_agent_tracks_the_package_release() {
        // Compared against the package version rather than a literal: the property under test is
        // that the UA *tracks* the release, and a pinned string asserts the opposite — it fails on
        // every bump for no reason, which is exactly what it did on 0.3.0.
        assert_eq!(
            PROVIDER_USER_AGENT,
            format!("pigeonpost-registry/{}", env!("CARGO_PKG_VERSION"))
        );
        assert!(PROVIDER_USER_AGENT.starts_with("pigeonpost-registry/"));
    }

    #[test]
    fn pkce_s256_matches_rfc_7636_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            pkce_s256(verifier).unwrap(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn proof_decoder_rejects_missing_and_unknown_security_fields() {
        let missing_pkce = serde_json::json!({
            "provider": "github", "code": "code", "state": "11".repeat(32)
        });
        let unknown = serde_json::json!({
            "provider": "google", "id_token": "token", "nonce": "11".repeat(32),
            "issuer": "https://attacker.invalid"
        });
        assert!(serde_json::from_value::<ProofPayload>(missing_pkce).is_err());
        assert!(serde_json::from_value::<ProofPayload>(unknown).is_err());
    }

    #[test]
    fn github_provider_debug_redacts_the_client_secret() {
        let canary = "github-secret-canary-2519";
        let rendered = format!("{:?}", GithubProvider::new("public-client-id", canary));
        assert!(rendered.contains("GithubProvider"));
        assert!(!rendered.contains(canary));
    }

    #[test]
    fn proof_and_subject_debug_never_expose_identity_material() {
        let github_canaries = [
            "github-code-canary-9831",
            "github-verifier-canary-9831",
            "github-state-canary-9831",
        ];
        let github = format!(
            "{:?}",
            ProofPayload::Github {
                code: github_canaries[0].into(),
                code_verifier: github_canaries[1].into(),
                state: github_canaries[2].into(),
            }
        );
        assert!(github.contains("Github"));
        assert!(github_canaries
            .iter()
            .all(|canary| !github.contains(canary)));

        let google_canaries = ["google-token-canary-9831", "google-nonce-canary-9831"];
        let google = format!(
            "{:?}",
            ProofPayload::Google {
                id_token: google_canaries[0].into(),
                nonce: google_canaries[1].into(),
            }
        );
        assert!(google.contains("Google"));
        assert!(google_canaries
            .iter()
            .all(|canary| !google.contains(canary)));

        let subject_canaries = [
            "subject-provider-canary-9831",
            "subject-name-canary-9831",
            "subject-opaque-id-canary-9831",
        ];
        let subject = format!(
            "{:?}",
            Subject {
                namespace: subject_canaries[0],
                name: subject_canaries[1].into(),
                opaque_id: subject_canaries[2].into(),
            }
        );
        assert!(subject_canaries
            .iter()
            .all(|canary| !subject.contains(canary)));
    }

    #[tokio::test]
    async fn github_never_accepts_an_assertion_as_a_bearer_fallback() {
        let provider = GithubProvider::new("id", "secret").with_endpoints(
            "http://127.0.0.1:1/token".into(),
            "http://127.0.0.1:1/user".into(),
        );
        let error = provider
            .verify(&ProofPayload::Github {
                code: "looks-like-a-token".into(),
                code_verifier: "a".repeat(43),
                state: "11".repeat(32),
            })
            .await
            .unwrap_err();
        assert!(matches!(error, RegistryError::ProviderUnreachable(_)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn oidc_cpu_lane_is_fail_fast_and_keeps_the_executor_responsive() {
        let lane = OidcVerificationLane::new(1);
        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let occupied = tokio::spawn({
            let lane = lane.clone();
            async move {
                lane.run(move || {
                    let _ = reached_tx.send(());
                    let _ = release_rx.recv_timeout(Duration::from_secs(1));
                    Ok(())
                })
                .await
            }
        });
        tokio::time::timeout(Duration::from_secs(60), reached_rx)
            .await
            .expect("the blocking verification must start")
            .unwrap();

        tokio::time::timeout(Duration::from_millis(100), tokio::task::yield_now())
            .await
            .expect("OIDC verification must not occupy the current-thread executor");
        assert!(matches!(
            lane.run(|| Ok(())).await,
            Err(RegistryError::Overloaded)
        ));

        release_tx.send(()).unwrap();
        occupied.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn providers_refuse_proofs_for_another_adapter() {
        let github = GithubProvider::new("id", "secret");
        assert!(matches!(
            github
                .verify(&ProofPayload::Google {
                    id_token: "irrelevant".into(),
                    nonce: "11".repeat(32),
                })
                .await,
            Err(RegistryError::WrongProvider)
        ));
    }
}
