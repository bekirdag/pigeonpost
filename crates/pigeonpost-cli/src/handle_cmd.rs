//! `pigeonpost handle claim`, `pigeonpost handle rotate`, and `pigeonpost handle resolve`.
//!
//! The client half of M3. `resolve` verifies inclusion, a fresh strict-majority witness quorum, and
//! append-only continuity from durable imported trust rather than believing the registry's answer.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use pigeonpost_client::{Agent, State};
use pigeonpost_core::{
    keys,
    network::{is_localhost_name, is_numeric_loopback_host},
};
use pigeonpost_registry::{
    entry::claim_payload, Checkpoint, Handle, HandlePublication, RegistryClient, RegistryError,
    VerifiedHandle, GITHUB_AUTHORIZATION_ENDPOINT, GOOGLE_AUTHORIZATION_ENDPOINT,
};
use rand_core::{OsRng, RngCore};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zeroize::Zeroizing;

const REGISTRATION_PUBLICATION_WAIT: Duration = Duration::from_secs(60);
const REGISTRATION_POLL_INTERVAL: Duration = Duration::from_millis(250);
const CALLBACK_LISTEN: &str = "127.0.0.1:8765";
const CALLBACK_URL: &str = "http://127.0.0.1:8765/callback";
const CALLBACK_ORIGIN: &str = "http://127.0.0.1:8765";
const CALLBACK_HOST: &str = "127.0.0.1:8765";
const CALLBACK_HEADER_LIMIT: usize = 8 * 1024;
const CALLBACK_FORM_LIMIT: usize = 20 * 1024;
const CALLBACK_REQUEST_LIMIT: usize = CALLBACK_HEADER_LIMIT + 4 + CALLBACK_FORM_LIMIT;
const MANUAL_CALLBACK_URL_LIMIT: usize = CALLBACK_FORM_LIMIT + 128;
const CHALLENGE_RESPONSE_LIMIT: usize = 256 * 1024;
const CHALLENGE_BYTES: usize = 64;
const MAX_CHALLENGE_LIFETIME_MS: u64 = 15 * 60 * 1_000;
const MAX_CLIENT_ID_BYTES: usize = 512;
const MAX_OAUTH_CODE_BYTES: usize = 2 * 1024;
const MAX_ID_TOKEN_BYTES: usize = 16 * 1024;

pub struct ClaimProof {
    pub mock_name: Option<String>,
    pub no_browser: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandleBindingMode {
    Claim,
    Rotate,
}

impl HandleBindingMode {
    fn endpoint(self) -> &'static str {
        match self {
            Self::Claim => "v1/register",
            Self::Rotate => "v1/rotate",
        }
    }

    fn entry_kind(self) -> &'static str {
        match self {
            Self::Claim => "handle_bind",
            Self::Rotate => "handle_rotate",
        }
    }

    fn action(self) -> &'static str {
        match self {
            Self::Claim => "claim",
            Self::Rotate => "rotation",
        }
    }

    fn past_tense(self) -> &'static str {
        match self {
            Self::Claim => "claimed",
            Self::Rotate => "rotated",
        }
    }
}

#[cfg(test)]
struct ClaimProofTestPause {
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(test)]
static CLAIM_PROOF_TEST_PAUSE: std::sync::Mutex<Option<std::sync::Arc<ClaimProofTestPause>>> =
    std::sync::Mutex::new(None);

pub async fn claim(
    agent: &Agent,
    registry_url: &str,
    handle: &str,
    proof: ClaimProof,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    bind_handle(
        agent,
        registry_url,
        handle,
        proof,
        json,
        HandleBindingMode::Claim,
    )
    .await
}

/// Rebind an existing provider-backed handle to this agent's current identity.
///
/// This is also the recovery path after total local key loss. It restores future handle routing;
/// it cannot recreate the retired address, local state, or ciphertext encrypted to the lost key.
pub async fn rotate(
    agent: &Agent,
    registry_url: &str,
    handle: &str,
    proof: ClaimProof,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    bind_handle(
        agent,
        registry_url,
        handle,
        proof,
        json,
        HandleBindingMode::Rotate,
    )
    .await
}

async fn bind_handle(
    agent: &Agent,
    registry_url: &str,
    handle: &str,
    proof: ClaimProof,
    json: bool,
    mode: HandleBindingMode,
) -> Result<(), Box<dyn std::error::Error>> {
    let handle = Handle::parse(handle)?;
    let base = registry_base(registry_url)?;
    let http = http_client()?;
    let configured = agent
        .state()
        .registry_configuration()?
        .ok_or("configure witnessed registry trust before binding a handle")?;
    if registry_base(&configured.url)? != base || configured.trust.witness_threshold() == 0 {
        return Err(
            "the requested registry does not match the configured witnessed trust root".into(),
        );
    }
    let registry_client = RegistryClient::new(&configured.url, configured.trust)?;

    // Sign the challenge request under a short lease, then release it during the human browser
    // wait. If local identity rotation happens while the operator authenticates, the second lease
    // below detects it and aborts before the selected endpoint can append under a retired key.
    let challenge_operation = agent.identity_operation()?;
    let identity_address = challenge_operation.address();
    let pubkey = challenge_operation.verifying_key().to_bytes();
    let challenge_signature = challenge_operation
        .sign(&claim_payload(&handle.as_path(), &pubkey))
        .to_bytes();
    drop(challenge_operation);
    let proof_value =
        claim_proof_value(&base, &http, &handle, &pubkey, &challenge_signature, proof).await?;

    // This is the irreversible boundary. Retain the reacquired identity lease through registry
    // append and witnessed publication confirmation.
    let identity_operation = agent.identity_operation()?;
    if identity_operation.verifying_key().to_bytes() != pubkey
        || identity_operation.address() != identity_address
    {
        return Err(format!(
            "Pigeonpost identity changed during handle authorization; restart the {}",
            mode.action()
        )
        .into());
    }
    let signature = identity_operation
        .sign(&claim_payload(&handle.as_path(), &pubkey))
        .to_bytes();
    let body = serde_json::json!({
        "handle": handle.as_path(),
        "pubkey": hex(&pubkey),
        "signature": hex(&signature),
        "proof": proof_value,
    });
    let deadline = Instant::now() + REGISTRATION_PUBLICATION_WAIT;
    let response = http
        .post(format!(
            "{}/{}",
            base.trim_end_matches('/'),
            mode.endpoint()
        ))
        .json(&body)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        return Err(format!("registry refused the {} ({status})", mode.action()).into());
    }

    let result: serde_json::Value = bounded_json(response, 1024 * 1024).await?;
    let returned_handle = result["handle"]
        .as_str()
        .ok_or("registry returned a malformed handle binding receipt")?;
    let log_index = result["log_index"]
        .as_u64()
        .ok_or("registry returned a malformed handle binding receipt")?;
    let appended = result["appended"]
        .as_bool()
        .ok_or("registry returned a malformed handle binding receipt")?;
    if returned_handle != handle.as_path() {
        return Err("registry binding receipt changed the requested handle".into());
    }
    let (verified, address) = await_handle_publication(
        agent.state(),
        &registry_client,
        &handle,
        &pubkey,
        log_index,
        mode.entry_kind(),
        deadline,
    )
    .await?;
    if address != identity_address {
        return Err("published handle resolved to a different Pigeonpost identity".into());
    }
    drop(identity_operation);
    if json {
        println!(
            "{}",
            serde_json::json!({
                "handle": handle.as_path(),
                "log_index": log_index,
                "appended": appended,
                "tree_size": verified.checkpoint().size,
                "witnessed_at": verified.witnessed_at(),
                "inclusion_verified": true,
                "witness_quorum_verified": true,
                "latest_binding_audited": true,
                "entry_kind": mode.entry_kind(),
            })
        );
    } else {
        println!(
            "{} {} at log index {log_index}",
            mode.past_tense(),
            handle.as_path()
        );
        println!("bound to {identity_address}");
        println!(
            "witnessed publication and latest binding audited at tree size {}",
            verified.checkpoint().size
        );
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BrowserProvider {
    Github,
    Google,
}

impl BrowserProvider {
    fn for_handle(handle: &Handle) -> Result<Self, Box<dyn std::error::Error>> {
        match handle.namespace() {
            "github" => Ok(Self::Github),
            "google" => Ok(Self::Google),
            _ => Err("the handle namespace has no supported identity flow".into()),
        }
    }

    const fn slug(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Google => "google",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserChallenge {
    provider: String,
    challenge: String,
    expires_at_ms: u64,
    client_id: String,
    authorization_endpoint: String,
    response_type: String,
    response_mode: String,
    scopes: Vec<String>,
    challenge_parameter: String,
    pkce_method: Option<String>,
}

impl core::fmt::Debug for BrowserChallenge {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // The challenge value is a one-shot OAuth state or OIDC nonce. Keep the entire browser
        // handoff out of generic diagnostics so future fields are fail-safe too.
        f.write_str("BrowserChallenge(<withheld>)")
    }
}

#[derive(PartialEq, Eq)]
enum CallbackProof {
    Github { code: String },
    Google { id_token: String },
}

impl core::fmt::Debug for CallbackProof {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Github { .. } => f.write_str("CallbackProof::Github(<withheld>)"),
            Self::Google { .. } => f.write_str("CallbackProof::Google(<withheld>)"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum CallbackEvent {
    RelayFragment,
    Complete(CallbackProof),
}

async fn claim_proof_value(
    registry_base: &str,
    http: &reqwest::Client,
    handle: &Handle,
    pubkey: &[u8; 32],
    signature: &[u8; 64],
    proof: ClaimProof,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let ClaimProof {
        mock_name,
        no_browser,
    } = proof;

    match mock_name {
        Some(name) => {
            if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
                return Err("the mock identity is malformed".into());
            }
            #[cfg(test)]
            let test_pause = {
                let configured = CLAIM_PROOF_TEST_PAUSE.lock().unwrap();
                configured.clone()
            };
            #[cfg(test)]
            if let Some(pause) = test_pause {
                pause.reached.notify_one();
                pause.release.notified().await;
            }
            Ok(serde_json::json!({ "provider": "mock", "name": name }))
        }
        None => {
            interactive_claim_proof(
                registry_base,
                http,
                handle,
                pubkey,
                signature,
                BrowserProvider::for_handle(handle)?,
                no_browser,
            )
            .await
        }
    }
}

async fn interactive_claim_proof(
    registry_base: &str,
    http: &reqwest::Client,
    handle: &Handle,
    pubkey: &[u8; 32],
    signature: &[u8; 64],
    provider: BrowserProvider,
    no_browser: bool,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    // Automatic mode binds before issuing a challenge so a provider redirect cannot race listener
    // startup. Manual/headless mode intentionally binds no local port.
    let listener = if no_browser {
        None
    } else {
        Some(
            tokio::net::TcpListener::bind(CALLBACK_LISTEN)
                .await
                .map_err(|_| {
                    "cannot start the one-shot Pigeonpost identity callback on 127.0.0.1:8765"
                })?,
        )
    };

    let (code_verifier, pkce_challenge) = if provider == BrowserProvider::Github {
        let mut random = [0u8; 32];
        OsRng.fill_bytes(&mut random);
        let verifier = base64url_no_pad(&random);
        let digest = Sha256::digest(verifier.as_bytes());
        (
            Some(Zeroizing::new(verifier)),
            Some(base64url_no_pad(&digest)),
        )
    } else {
        (None, None)
    };
    let challenge_request = match pkce_challenge.as_deref() {
        Some(pkce) => serde_json::json!({
            "provider": provider.slug(),
            "handle": handle.as_path(),
            "pubkey": hex(pubkey),
            "signature": hex(signature),
            "pkce_challenge": pkce,
        }),
        None => serde_json::json!({
            "provider": provider.slug(),
            "handle": handle.as_path(),
            "pubkey": hex(pubkey),
            "signature": hex(signature),
            "pkce_challenge": serde_json::Value::Null,
        }),
    };
    let response = http
        .post(format!(
            "{}/v1/identity/challenge",
            registry_base.trim_end_matches('/')
        ))
        .json(&challenge_request)
        .send()
        .await
        .map_err(|_| "the registry identity-challenge service is unavailable")?;
    if !response.status().is_success() {
        return Err("the registry refused to issue an identity challenge".into());
    }
    let challenge: BrowserChallenge = bounded_json(response, CHALLENGE_RESPONSE_LIMIT)
        .await
        .map_err(|_| "the registry returned a malformed identity challenge")?;
    let issued_at_ms = now_millis();
    validate_browser_challenge(&challenge, provider, issued_at_ms)?;
    let authorization_url =
        browser_authorization_url(&challenge, provider, pkce_challenge.as_deref())?;

    eprintln!("Authorize this Pigeonpost handle in your browser:");
    eprintln!("{authorization_url}");
    if !no_browser && !open_browser(authorization_url.as_str()) {
        eprintln!("The browser could not be opened automatically; open the URL above.");
    } else if no_browser {
        eprintln!(
            "After authorization redirects to {CALLBACK_URL}, copy the full address-bar URL and paste it at the hidden prompt. No local callback port is open."
        );
    }

    let wait_ms = challenge.expires_at_ms.saturating_sub(now_millis());
    if wait_ms == 0 || wait_ms > MAX_CHALLENGE_LIFETIME_MS {
        return Err("the registry identity challenge is expired".into());
    }
    let callback = match listener {
        Some(listener) => {
            wait_for_callback(
                listener,
                provider,
                &challenge.challenge,
                Duration::from_millis(wait_ms),
            )
            .await?
        }
        None => read_manual_callback(provider, &challenge.challenge).await?,
    };
    if now_millis() >= challenge.expires_at_ms {
        return Err("the registry identity challenge expired before completion".into());
    }
    match (callback, code_verifier) {
        (CallbackProof::Github { code }, Some(code_verifier)) => Ok(serde_json::json!({
            "provider": "github",
            "code": code,
            "code_verifier": code_verifier.as_str(),
            "state": challenge.challenge,
        })),
        (CallbackProof::Google { id_token }, None) => Ok(serde_json::json!({
            "provider": "google",
            "id_token": id_token,
            "nonce": challenge.challenge,
        })),
        _ => Err("the local identity callback returned the wrong proof kind".into()),
    }
}

fn validate_browser_challenge(
    challenge: &BrowserChallenge,
    provider: BrowserProvider,
    now_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let lifetime = challenge.expires_at_ms.saturating_sub(now_ms);
    let common_valid = challenge.provider == provider.slug()
        && valid_challenge(&challenge.challenge)
        && lifetime > 0
        && lifetime <= MAX_CHALLENGE_LIFETIME_MS
        && !challenge.client_id.is_empty()
        && challenge.client_id.len() <= MAX_CLIENT_ID_BYTES
        && !challenge.client_id.chars().any(char::is_control);
    let provider_valid = match provider {
        BrowserProvider::Github => {
            challenge.authorization_endpoint == GITHUB_AUTHORIZATION_ENDPOINT
                && challenge.response_type == "code"
                && challenge.response_mode == "query"
                && challenge.scopes.is_empty()
                && challenge.challenge_parameter == "state"
                && challenge.pkce_method.as_deref() == Some("S256")
        }
        BrowserProvider::Google => {
            challenge.authorization_endpoint == GOOGLE_AUTHORIZATION_ENDPOINT
                && challenge.response_type == "id_token"
                && challenge.response_mode == "fragment"
                && challenge.scopes == ["openid", "profile"]
                && challenge.challenge_parameter == "nonce"
                && challenge.pkce_method.is_none()
        }
    };
    if common_valid && provider_valid {
        Ok(())
    } else {
        Err("the registry returned unsafe identity authorization metadata".into())
    }
}

fn browser_authorization_url(
    challenge: &BrowserChallenge,
    provider: BrowserProvider,
    pkce_challenge: Option<&str>,
) -> Result<reqwest::Url, Box<dyn std::error::Error>> {
    let mut url = reqwest::Url::parse(&challenge.authorization_endpoint)
        .map_err(|_| "the identity authorization endpoint is malformed")?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("client_id", &challenge.client_id);
        query.append_pair("response_type", &challenge.response_type);
        match provider {
            BrowserProvider::Github => {
                let pkce = pkce_challenge
                    .filter(|value| value.len() == 43)
                    .ok_or("the GitHub PKCE challenge is malformed")?;
                // GitHub uses the callback configured on the OAuth App when redirect_uri is
                // omitted. Its token exchange then correctly need not repeat that URI.
                query.append_pair("state", &challenge.challenge);
                query.append_pair("code_challenge", pkce);
                query.append_pair("code_challenge_method", "S256");
            }
            BrowserProvider::Google => {
                query.append_pair("redirect_uri", CALLBACK_URL);
                query.append_pair("response_mode", "fragment");
                query.append_pair("scope", "openid profile");
                query.append_pair("nonce", &challenge.challenge);
                // `nonce` is verified inside the signed ID token. A parallel OAuth state value
                // lets the local callback reject cross-flow browser posts before forwarding it.
                query.append_pair("state", &challenge.challenge);
            }
        }
    }
    Ok(url)
}

async fn read_manual_callback(
    provider: BrowserProvider,
    expected_state: &str,
) -> Result<CallbackProof, Box<dyn std::error::Error>> {
    let pasted = tokio::task::spawn_blocking(|| {
        rpassword::prompt_password("Full callback URL (input hidden): ")
    })
    .await
    .map_err(|_| "could not read the callback URL from the terminal")?
    .map_err(|_| "could not read the callback URL from the terminal")?;
    let pasted = Zeroizing::new(pasted);
    parse_manual_callback_url(pasted.trim(), expected_state, provider).map_err(Into::into)
}

fn parse_manual_callback_url(
    input: &str,
    expected_state: &str,
    provider: BrowserProvider,
) -> Result<CallbackProof, &'static str> {
    if input.is_empty() || input.len() > MANUAL_CALLBACK_URL_LIMIT {
        return Err("invalid identity callback URL");
    }
    let url = reqwest::Url::parse(input).map_err(|_| "invalid identity callback URL")?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port() != Some(8765)
        || url.path() != "/callback"
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("invalid identity callback URL");
    }

    let encoded = match provider {
        BrowserProvider::Github if url.fragment().is_none() => {
            url.query().ok_or("invalid identity callback URL")?
        }
        BrowserProvider::Google if url.query().is_none() => {
            url.fragment().ok_or("invalid identity callback URL")?
        }
        _ => return Err("invalid identity callback URL"),
    };
    parse_callback_form(encoded.as_bytes(), expected_state, provider)
}

async fn wait_for_callback(
    listener: tokio::net::TcpListener,
    provider: BrowserProvider,
    expected_state: &str,
    timeout: Duration,
) -> Result<CallbackProof, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + timeout;
    for _ in 0..64 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let accepted = tokio::time::timeout(remaining, listener.accept())
            .await
            .map_err(|_| "the local identity callback timed out")?
            .map_err(|_| "the local identity callback failed")?;
        let (mut stream, peer) = accepted;
        if !peer.ip().is_loopback() {
            continue;
        }
        let read_budget = remaining.min(Duration::from_secs(5));
        let raw = match tokio::time::timeout(read_budget, read_local_request(&mut stream)).await {
            Ok(Ok(raw)) => raw,
            _ => continue,
        };
        match parse_callback_request(&raw, expected_state, provider) {
            Ok(CallbackEvent::RelayFragment) => {
                write_local_response(&mut stream, 200, GOOGLE_RELAY_PAGE).await;
            }
            Ok(CallbackEvent::Complete(proof)) => {
                write_local_response(&mut stream, 200, CALLBACK_SUCCESS_PAGE).await;
                return Ok(proof);
            }
            Err(_) => {
                write_local_response(&mut stream, 400, CALLBACK_FAILURE_PAGE).await;
            }
        }
    }
    Err("the local identity callback did not receive a valid proof".into())
}

async fn read_local_request(stream: &mut tokio::net::TcpStream) -> Result<Vec<u8>, &'static str> {
    let mut raw = Vec::with_capacity(2 * 1024);
    let mut buffer = [0u8; 2 * 1024];
    loop {
        if let Some(header_end) = find_header_end(&raw) {
            if header_end > CALLBACK_HEADER_LIMIT {
                return Err("invalid local callback request");
            }
            let body_len = declared_body_len(&raw[..header_end])?;
            if body_len > CALLBACK_FORM_LIMIT {
                return Err("invalid local callback request");
            }
            let total = header_end
                .checked_add(4)
                .and_then(|value| value.checked_add(body_len))
                .ok_or("invalid local callback request")?;
            if total > CALLBACK_REQUEST_LIMIT {
                return Err("invalid local callback request");
            }
            if raw.len() == total {
                return Ok(raw);
            }
            if raw.len() > total {
                return Err("invalid local callback request");
            }
        } else if raw.len() > CALLBACK_HEADER_LIMIT.saturating_add(3) {
            return Err("invalid local callback request");
        }
        let read = stream
            .read(&mut buffer)
            .await
            .map_err(|_| "invalid local callback request")?;
        if read == 0 || read > CALLBACK_REQUEST_LIMIT.saturating_sub(raw.len()) {
            return Err("invalid local callback request");
        }
        raw.extend_from_slice(&buffer[..read]);
    }
}

fn parse_callback_request(
    raw: &[u8],
    expected_state: &str,
    provider: BrowserProvider,
) -> Result<CallbackEvent, &'static str> {
    if raw.len() > CALLBACK_REQUEST_LIMIT {
        return Err("invalid local callback request");
    }
    let header_end = find_header_end(raw).ok_or("invalid local callback request")?;
    if header_end > CALLBACK_HEADER_LIMIT {
        return Err("invalid local callback request");
    }
    let head =
        std::str::from_utf8(&raw[..header_end]).map_err(|_| "invalid local callback request")?;
    let mut lines = head.split("\r\n");
    let request_line = lines.next().ok_or("invalid local callback request")?;
    let mut request_parts = request_line.split(' ');
    let method = request_parts
        .next()
        .ok_or("invalid local callback request")?;
    let target = request_parts
        .next()
        .ok_or("invalid local callback request")?;
    let version = request_parts
        .next()
        .ok_or("invalid local callback request")?;
    if request_parts.next().is_some()
        || !matches!(version, "HTTP/1.0" | "HTTP/1.1")
        || target.len() > CALLBACK_REQUEST_LIMIT
    {
        return Err("invalid local callback request");
    }

    let mut content_length = None;
    let mut content_type = None;
    let mut host = None;
    let mut origin = None;
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or("invalid local callback request")?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                return Err("invalid local callback request");
            }
            content_length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| "invalid local callback request")?,
            );
        } else if name.eq_ignore_ascii_case("content-type") {
            if content_type.replace(value).is_some() {
                return Err("invalid local callback request");
            }
        } else if name.eq_ignore_ascii_case("host") {
            if host.replace(value).is_some() {
                return Err("invalid local callback request");
            }
        } else if name.eq_ignore_ascii_case("origin") {
            if origin.replace(value).is_some() {
                return Err("invalid local callback request");
            }
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err("invalid local callback request");
        }
    }
    let body = &raw[header_end + 4..];
    if host != Some(CALLBACK_HOST)
        || body.len() > CALLBACK_FORM_LIMIT
        || body.len() != content_length.unwrap_or(0)
    {
        return Err("invalid local callback request");
    }

    match provider {
        BrowserProvider::Github => {
            if method != "GET" || !body.is_empty() {
                return Err("invalid local callback request");
            }
            let (path, query) = target
                .split_once('?')
                .ok_or("invalid local callback request")?;
            if path != "/callback" {
                return Err("invalid local callback request");
            }
            parse_callback_form(query.as_bytes(), expected_state, provider)
                .map(CallbackEvent::Complete)
        }
        BrowserProvider::Google => {
            if method == "GET" && target == "/callback" && body.is_empty() {
                return Ok(CallbackEvent::RelayFragment);
            }
            let media_type = content_type
                .and_then(|value| value.split(';').next())
                .map(str::trim);
            if method != "POST"
                || target != "/capture"
                || media_type != Some("application/x-www-form-urlencoded")
                || content_length.is_none()
                || origin != Some(CALLBACK_ORIGIN)
            {
                return Err("invalid local callback request");
            }
            parse_callback_form(body, expected_state, provider).map(CallbackEvent::Complete)
        }
    }
}

fn parse_callback_form(
    encoded: &[u8],
    expected_state: &str,
    provider: BrowserProvider,
) -> Result<CallbackProof, &'static str> {
    if encoded.len() > CALLBACK_FORM_LIMIT {
        return Err("invalid local callback proof");
    }
    let mut credential = None;
    let mut state = None;
    let mut refused = false;
    for (name, value) in url::form_urlencoded::parse(encoded) {
        let is_credential = match provider {
            BrowserProvider::Github => name == "code",
            BrowserProvider::Google => name == "id_token",
        };
        if is_credential {
            if credential.replace(value.into_owned()).is_some() {
                return Err("invalid local callback proof");
            }
        } else if name == "state" {
            if state.replace(value.into_owned()).is_some() {
                return Err("invalid local callback proof");
            }
        } else if name == "error" {
            refused = true;
        }
    }
    if refused || state.as_deref() != Some(expected_state) {
        return Err("invalid local callback proof");
    }
    let credential = credential.ok_or("invalid local callback proof")?;
    match provider {
        BrowserProvider::Github if valid_oauth_code(&credential) => {
            Ok(CallbackProof::Github { code: credential })
        }
        BrowserProvider::Google if valid_id_token(&credential) => Ok(CallbackProof::Google {
            id_token: credential,
        }),
        _ => Err("invalid local callback proof"),
    }
}

fn declared_body_len(head: &[u8]) -> Result<usize, &'static str> {
    let head = std::str::from_utf8(head).map_err(|_| "invalid local callback request")?;
    let mut length = None;
    for line in head.split("\r\n").skip(1) {
        let (name, value) = line
            .split_once(':')
            .ok_or("invalid local callback request")?;
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err("invalid local callback request");
        }
        if name.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                return Err("invalid local callback request");
            }
            length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| "invalid local callback request")?,
            );
        }
    }
    Ok(length.unwrap_or(0))
}

fn find_header_end(raw: &[u8]) -> Option<usize> {
    raw.windows(4).position(|window| window == b"\r\n\r\n")
}

async fn write_local_response(stream: &mut tokio::net::TcpStream, status: u16, body: &str) {
    let reason = if status == 200 { "OK" } else { "Bad Request" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nPragma: no-cache\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nContent-Security-Policy: {CALLBACK_CSP}\r\nConnection: close\r\n\r\n{body}",
        body.len(),
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

const CALLBACK_CSP: &str = "default-src 'none'; script-src 'sha256-g73jIZyYAu1i9WuXAZ+D32oK+aXTXetbbbaI3g7L0rc=' 'sha256-4pkJtWOV3vDkzDOBPsD2+h4idESu5wgROxuhDhSEK/A='; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
const CALLBACK_SUCCESS_PAGE: &str = r#"<!doctype html><meta charset="utf-8"><title>Pigeonpost</title><script>history.replaceState(null,"","/callback")</script><p>Pigeonpost captured the identity response. Return to the terminal while it verifies and publishes the handle.</p>"#;
const CALLBACK_FAILURE_PAGE: &str = r#"<!doctype html><meta charset="utf-8"><title>Pigeonpost</title><script>history.replaceState(null,"","/callback")</script><p>Pigeonpost rejected the identity response. Return to the terminal and try again.</p>"#;
const GOOGLE_RELAY_PAGE: &str = r#"<!doctype html><meta charset="utf-8"><title>Pigeonpost</title><p id="status">Pigeonpost is capturing the identity response…</p><script>const status=document.getElementById("status");const body=location.hash.slice(1);history.replaceState(null,"","/callback");if(!body||body.length>20480){status.textContent="The identity response was empty or too large. Return to the terminal and try again."}else{fetch("/capture",{method:"POST",headers:{"Content-Type":"application/x-www-form-urlencoded"},body}).then(r=>{status.textContent=r.ok?"Pigeonpost captured the identity response. You may close this tab.":"Pigeonpost rejected the identity response. Return to the terminal and try again."}).catch(()=>{status.textContent="Pigeonpost could not reach its local callback. Return to the terminal and try again."})}</script>"#;

fn valid_challenge(value: &str) -> bool {
    value.len() == CHALLENGE_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_oauth_code(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_OAUTH_CODE_BYTES && !value.chars().any(char::is_control)
}

fn valid_id_token(value: &str) -> bool {
    if value.is_empty() || value.len() > MAX_ID_TOKEN_BYTES || !value.is_ascii() {
        return false;
    }
    let mut segments = value.split('.');
    let valid_segment = |segment: &str| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    };
    matches!(
        (segments.next(), segments.next(), segments.next(), segments.next()),
        (Some(header), Some(payload), Some(signature), None)
            if valid_segment(header) && valid_segment(payload) && valid_segment(signature)
    )
}

fn base64url_no_pad(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut output = String::with_capacity(bytes.len().saturating_mul(4).div_ceil(3));
    let mut chunks = bytes.chunks_exact(3);
    for chunk in &mut chunks {
        output.push(ALPHABET[(chunk[0] >> 2) as usize] as char);
        output.push(ALPHABET[(((chunk[0] & 0x03) << 4) | (chunk[1] >> 4)) as usize] as char);
        output.push(ALPHABET[(((chunk[1] & 0x0f) << 2) | (chunk[2] >> 6)) as usize] as char);
        output.push(ALPHABET[(chunk[2] & 0x3f) as usize] as char);
    }
    match chunks.remainder() {
        [first] => {
            output.push(ALPHABET[(first >> 2) as usize] as char);
            output.push(ALPHABET[((first & 0x03) << 4) as usize] as char);
        }
        [first, second] => {
            output.push(ALPHABET[(first >> 2) as usize] as char);
            output.push(ALPHABET[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
            output.push(ALPHABET[((second & 0x0f) << 2) as usize] as char);
        }
        [] => {}
        _ => unreachable!(),
    }
    output
}

fn open_browser(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(target_os = "linux")]
    let mut command = Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    return false;

    command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

async fn await_handle_publication(
    state: &State,
    client: &RegistryClient,
    handle: &Handle,
    expected_pubkey: &[u8; 32],
    expected_index: u64,
    expected_entry_kind: &str,
    deadline: Instant,
) -> Result<(VerifiedHandle, pigeonpost_core::Address), Box<dyn std::error::Error>> {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err("registry witness publication timed out".into());
        }
        let remaining = deadline.saturating_duration_since(now);
        let attempt = tokio::time::timeout(
            remaining,
            state.resolve_handle_audited(client, handle, now_secs()),
        )
        .await;
        match attempt {
            Ok(Ok((verified, address))) => {
                if verified.handle() != handle {
                    return Err("published handle binding changed the requested handle".into());
                }
                match verified.publication_against(
                    expected_index,
                    expected_pubkey,
                    expected_entry_kind,
                ) {
                    HandlePublication::Pending => {}
                    HandlePublication::Ready => return Ok((verified, address)),
                    HandlePublication::Mismatch => {
                        return Err(
                            "published handle binding differs from the immutable binding receipt"
                                .into(),
                        )
                    }
                }
            }
            Ok(Err(pigeonpost_client::ClientError::Registry(
                RegistryError::NotFound | RegistryError::RegistryUnavailable,
            ))) => {}
            Ok(Err(error)) => return Err(error.into()),
            Err(_) => return Err("registry witness publication timed out".into()),
        }
        let now = Instant::now();
        if now >= deadline {
            return Err("registry witness publication timed out".into());
        }
        tokio::time::sleep(REGISTRATION_POLL_INTERVAL.min(deadline.saturating_duration_since(now)))
            .await;
    }
}

/// Resolve a handle through the durable, witnessed registry trust installed for this agent home.
///
/// This deliberately opens only the existing state database: a lookup never creates an identity.
pub async fn resolve(
    home: &Path,
    registry_url: &str,
    handle: &str,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let handle = Handle::parse(handle)?;
    let state_path = home.join("state.db");
    let metadata = std::fs::symlink_metadata(&state_path)
        .map_err(|_| "no existing Pigeonpost state; import witnessed registry trust first")?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("the existing Pigeonpost state is not a regular file".into());
    }
    let state = State::open(&state_path)?;
    let configured = state
        .registry_configuration()?
        .ok_or("import witnessed registry trust before resolving a handle")?;
    if configured.trust.witness_threshold() == 0
        || registry_base(&configured.url)? != registry_base(registry_url)?
    {
        return Err(
            "the requested registry does not match the configured witnessed trust root".into(),
        );
    }
    let client = RegistryClient::new(&configured.url, configured.trust.clone())?;
    let (verified, address) = state
        .resolve_handle_audited(&client, &handle, now_secs())
        .await?;
    let pubkey = hex(verified.pubkey());
    let index = verified.log_index();
    let size = verified.checkpoint().size;
    let witnessed_at = verified
        .witnessed_at()
        .ok_or("registry resolution did not carry a witnessed checkpoint")?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "handle": handle.as_path(),
                "pubkey": pubkey,
                "address": address.as_str(),
                "log_index": index,
                "tree_size": size,
                "inclusion_verified": true,
                "latest_binding_audited": true,
                "witness_threshold": configured.trust.witness_threshold(),
                "witnessed_at": witnessed_at,
                "continuity_persisted": true,
            })
        );
    } else {
        println!("{}  ->  {pubkey}", handle.as_path());
        println!("address {address}");
        println!("log index {index} of {size}");
        println!(
            "inclusion, latest-binding audit, fresh {}/{} witness quorum, and checkpoint continuity verified",
            configured.trust.witness_threshold(),
            configured.trust.witnesses().len()
        );
    }
    Ok(())
}

/// Verify a checkpoint fetched from a registry against a known key.
pub async fn checkpoint(
    registry_url: &str,
    key_hex: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let base = registry_base(registry_url)?;
    let response = http_client()?
        .get(format!("{}/v1/log/checkpoint", base.trim_end_matches('/')))
        .send()
        .await?
        .error_for_status()?;
    let text = String::from_utf8(bounded_body(response, 64 * 1024).await?)?;

    print!("{text}");

    match key_hex {
        Some(hex) => {
            let bytes = parse_hex32(hex).ok_or("--key must be 32 hex bytes")?;
            let key = keys::verifying_key_from_bytes(&bytes)?;
            match Checkpoint::verify(&text, &key) {
                Ok(cp) => println!("\nsignature verified: {} entries", cp.size),
                Err(error) => return Err(format!("SIGNATURE INVALID: {error}").into()),
            }
        }
        None => println!("\n(pass --key <hex> to verify the signature)"),
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_hex32(input: &str) -> Option<[u8; 32]> {
    if input.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in input.as_bytes().chunks(2).enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    Some(out)
}

fn registry_base(input: &str) -> Result<String, Box<dyn std::error::Error>> {
    if input.is_empty() || input.len() > 2_048 {
        return Err("registry URL is empty or too long".into());
    }
    let url = reqwest::Url::parse(input)?;
    if url.cannot_be_a_base()
        || url.host_str().is_none()
        || url.host_str().is_some_and(is_localhost_name)
        || url.port() == Some(0)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || (url.path() != "/" && !url.path().is_empty())
    {
        return Err("registry URL must not contain credentials, query, or fragment".into());
    }
    let loopback = url.host_str().is_some_and(is_numeric_loopback_host);
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err("registry URL must use HTTPS (HTTP is loopback-only)".into());
    }
    Ok(input.trim_end_matches('/').to_string())
}

fn http_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .pool_max_idle_per_host(2)
        .build()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

async fn bounded_json<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
    limit: usize,
) -> Result<T, Box<dyn std::error::Error>> {
    Ok(serde_json::from_slice(
        &bounded_body(response, limit).await?,
    )?)
}

async fn bounded_body(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err("registry response exceeds the configured limit".into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > limit.saturating_sub(body.len()) {
            return Err("registry response exceeds the configured limit".into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    use axum::extract::{Query, State};
    use axum::http::StatusCode;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use ed25519_dalek::SigningKey;
    use pigeonpost_registry::{CheckpointPin, LogEntry, MerkleLog, RegistryTrust, WitnessKey};

    use super::*;

    const ORIGIN: &str = "pigeonpost.test/registration";
    static CLAIM_TEST_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[test]
    fn browser_identity_debug_output_withholds_every_credential() {
        let challenge = BrowserChallenge {
            provider: "provider-debug-canary-4567".to_owned(),
            challenge: "challenge-debug-canary-4567".to_owned(),
            expires_at_ms: 4_567,
            client_id: "client-debug-canary-4567".to_owned(),
            authorization_endpoint: "https://endpoint-debug-canary-4567.example".to_owned(),
            response_type: "response-debug-canary-4567".to_owned(),
            response_mode: "mode-debug-canary-4567".to_owned(),
            scopes: vec!["scope-debug-canary-4567".to_owned()],
            challenge_parameter: "parameter-debug-canary-4567".to_owned(),
            pkce_method: Some("pkce-debug-canary-4567".to_owned()),
        };
        assert_eq!(format!("{challenge:?}"), "BrowserChallenge(<withheld>)");

        let github = CallbackProof::Github {
            code: "github-code-debug-canary-4567".to_owned(),
        };
        assert_eq!(format!("{github:?}"), "CallbackProof::Github(<withheld>)");
        let google = CallbackProof::Google {
            id_token: "google-token-debug-canary-4567".to_owned(),
        };
        assert_eq!(format!("{google:?}"), "CallbackProof::Google(<withheld>)");
    }

    #[test]
    fn registry_url_is_an_https_or_numeric_loopback_origin() {
        for accepted in [
            "https://registry.example",
            "http://127.0.0.1:7718",
            "http://[::1]:7718",
        ] {
            assert!(registry_base(accepted).is_ok(), "rejected {accepted}");
        }
        for rejected in [
            "http://localhost:7718",
            "http://localhost.:7718",
            "https://localhost:7718",
            "https://localhost.:7718",
            "https://api.localhost:7718",
            "http://192.0.2.1:7718",
            "https://registry.example:0",
            "https://user@registry.example",
            "https://registry.example/prefix",
            "https://registry.example?query=1",
        ] {
            assert!(registry_base(rejected).is_err(), "accepted {rejected}");
        }
    }

    #[derive(Clone)]
    struct PublishedState {
        resolved: serde_json::Value,
        entries: serde_json::Value,
    }

    async fn resolved(State(state): State<Arc<PublishedState>>) -> Json<serde_json::Value> {
        Json(state.resolved.clone())
    }

    #[derive(Deserialize)]
    struct EntryRange {
        from: u64,
        to: u64,
    }

    #[derive(Deserialize)]
    struct ConsistencyRange {
        from: u64,
    }

    async fn entries(
        State(state): State<Arc<PublishedState>>,
        Query(range): Query<EntryRange>,
    ) -> Json<serde_json::Value> {
        Json(ranged_entries(&state, &range))
    }

    fn ranged_entries(state: &PublishedState, range: &EntryRange) -> serde_json::Value {
        let mut response = state.entries.clone();
        let base = state.entries["from"].as_u64().unwrap();
        let all = state.entries["entries"].as_array().unwrap();
        let start = usize::try_from(range.from - base).unwrap();
        let end = usize::try_from(range.to - base).unwrap();
        response["from"] = serde_json::json!(range.from);
        response["to"] = serde_json::json!(range.to);
        response["entries"] = serde_json::Value::Array(all[start..end].to_vec());
        response
    }

    struct DelayedPublicationState {
        older: Arc<PublishedState>,
        current: Arc<PublishedState>,
        resolve_calls: AtomicUsize,
        phase: AtomicUsize,
    }

    async fn delayed_resolved(
        State(state): State<Arc<DelayedPublicationState>>,
    ) -> Json<serde_json::Value> {
        let phase = state.resolve_calls.fetch_add(1, Ordering::SeqCst).min(1);
        state.phase.store(phase, Ordering::SeqCst);
        let selected = if phase == 0 {
            &state.older
        } else {
            &state.current
        };
        Json(selected.resolved.clone())
    }

    async fn delayed_entries(
        State(state): State<Arc<DelayedPublicationState>>,
        Query(range): Query<EntryRange>,
    ) -> Json<serde_json::Value> {
        let selected = if state.phase.load(Ordering::SeqCst) == 0 {
            &state.older
        } else {
            &state.current
        };
        Json(ranged_entries(selected, &range))
    }

    async fn delayed_consistency(
        State(state): State<Arc<DelayedPublicationState>>,
        Query(range): Query<ConsistencyRange>,
    ) -> Json<serde_json::Value> {
        let mut log = MerkleLog::new();
        for value in state.current.entries["entries"].as_array().unwrap() {
            let entry: LogEntry = serde_json::from_value(value.clone()).unwrap();
            log.append(&entry.leaf_bytes().unwrap());
        }
        Json(serde_json::json!({
            "from": range.from,
            "to": 2,
            "root": state.current.entries["root"],
            "path": log.consistency_proof(range.from, 2).unwrap()
                .iter().map(|hash| hex(hash)).collect::<Vec<_>>(),
        }))
    }

    struct ClaimRaceState {
        published: Arc<PublishedState>,
        append_received: tokio::sync::Notify,
        release_append: tokio::sync::Notify,
        pause_publication_audit: AtomicBool,
        publication_audit_received: tokio::sync::Notify,
        release_publication_audit: tokio::sync::Notify,
    }

    async fn race_resolved(State(state): State<Arc<ClaimRaceState>>) -> Json<serde_json::Value> {
        Json(state.published.resolved.clone())
    }

    async fn race_entries(State(state): State<Arc<ClaimRaceState>>) -> Json<serde_json::Value> {
        if state.pause_publication_audit.swap(false, Ordering::SeqCst) {
            state.publication_audit_received.notify_one();
            state.release_publication_audit.notified().await;
        }
        Json(state.published.entries.clone())
    }

    async fn paused_register(State(state): State<Arc<ClaimRaceState>>) -> Json<serde_json::Value> {
        state.append_received.notify_one();
        state.release_append.notified().await;
        Json(serde_json::json!({
            "handle": "/github/alice",
            "log_index": 0,
            "appended": true,
        }))
    }

    async fn spawn(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{address}")
    }

    async fn spawn_test_loft() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let key = pigeonpost_core::Identity::from_seed([0xA7; 32]);
        let store: Arc<dyn pigeonpost_loft::LoftStore> =
            Arc::new(pigeonpost_loft::SqliteStore::in_memory().unwrap());
        let loft = Arc::new(
            pigeonpost_loft::Loft::new(
                pigeonpost_loft::LoftConfig::new(key.verifying_key().to_bytes(), &url),
                store,
            )
            .unwrap(),
        );
        tokio::spawn(async move {
            pigeonpost_loft::serve(listener, loft, std::future::pending()).await
        });
        url
    }

    async fn count_register(State(calls): State<Arc<AtomicUsize>>) -> Json<serde_json::Value> {
        calls.fetch_add(1, Ordering::SeqCst);
        Json(serde_json::json!({ "unexpected": true }))
    }

    async fn rotate_receipt() -> Json<serde_json::Value> {
        Json(serde_json::json!({
            "handle": "/github/alice",
            "log_index": 1,
            "appended": true,
        }))
    }

    fn witnessed_state(
        pubkey: [u8; 32],
        tamper_entry: bool,
    ) -> (Arc<PublishedState>, RegistryTrust) {
        let operator = SigningKey::from_bytes(&[71; 32]);
        let witness = SigningKey::from_bytes(&[72; 32]);
        let handle = "/github/alice";
        let entry =
            LogEntry::handle_claim(0, handle.into(), hex(&pubkey), "github:opaque".into(), 1);
        let mut log = MerkleLog::new();
        log.append(&entry.leaf_bytes().unwrap());
        let checkpoint = Checkpoint {
            origin: ORIGIN.into(),
            size: 1,
            root: log.root(),
        };
        let mut note = checkpoint.sign(&operator);
        note.push_str(
            &checkpoint
                .cosignature_line("witness.test/registration", &witness, now_secs())
                .unwrap(),
        );
        let returned_entry = if tamper_entry {
            LogEntry::handle_claim(0, handle.into(), "00".repeat(32), "github:opaque".into(), 1)
        } else {
            entry
        };
        let state = Arc::new(PublishedState {
            resolved: serde_json::json!({
                "handle": handle,
                "pubkey": hex(&pubkey),
                "log_index": 0,
                "inclusion_proof": {
                    "tree_size": 1,
                    "root": hex(&log.root()),
                    "path": [],
                    "checkpoint": note.clone(),
                }
            }),
            entries: serde_json::json!({
                "from": 0,
                "to": 1,
                "tree_size": 1,
                "root": hex(&log.root()),
                "checkpoint": note,
                "entries": [returned_entry],
            }),
        });
        let trust = RegistryTrust::new(
            ORIGIN,
            operator.verifying_key().to_bytes(),
            vec![WitnessKey::new("witness.test/registration", witness.verifying_key()).unwrap()],
            1,
            CheckpointPin {
                size: 0,
                root: pigeonpost_registry::log::empty_root(),
            },
            60,
            5,
        )
        .unwrap();
        (state, trust)
    }

    fn witnessed_rotation_state(
        old_pubkey: [u8; 32],
        new_pubkey: [u8; 32],
    ) -> (Arc<PublishedState>, Arc<PublishedState>, RegistryTrust) {
        let operator = SigningKey::from_bytes(&[81; 32]);
        let witness = SigningKey::from_bytes(&[82; 32]);
        let handle = "/github/alice";
        let claim = LogEntry::handle_claim(
            0,
            handle.into(),
            hex(&old_pubkey),
            "github:stable-subject".into(),
            1,
        );
        let rotation = LogEntry::handle_rotation(
            1,
            handle.into(),
            hex(&new_pubkey),
            "github:stable-subject".into(),
            2,
        );
        let entries = vec![claim.clone(), rotation];
        let mut log = MerkleLog::new();
        log.append(&claim.leaf_bytes().unwrap());
        let older_checkpoint = Checkpoint {
            origin: ORIGIN.into(),
            size: 1,
            root: log.root(),
        };
        let mut older_note = older_checkpoint.sign(&operator);
        older_note.push_str(
            &older_checkpoint
                .cosignature_line("witness.test/registration", &witness, now_secs())
                .unwrap(),
        );
        let older = Arc::new(PublishedState {
            resolved: serde_json::json!({
                "handle": handle,
                "pubkey": hex(&old_pubkey),
                "log_index": 0,
                "inclusion_proof": {
                    "tree_size": 1,
                    "root": hex(&older_checkpoint.root),
                    "path": [],
                    "checkpoint": older_note.clone(),
                }
            }),
            entries: serde_json::json!({
                "from": 0,
                "to": 1,
                "tree_size": 1,
                "root": hex(&older_checkpoint.root),
                "checkpoint": older_note,
                "entries": [claim],
            }),
        });
        log.append(&entries[1].leaf_bytes().unwrap());
        let checkpoint = Checkpoint {
            origin: ORIGIN.into(),
            size: 2,
            root: log.root(),
        };
        let mut note = checkpoint.sign(&operator);
        note.push_str(
            &checkpoint
                .cosignature_line("witness.test/registration", &witness, now_secs())
                .unwrap(),
        );
        let state = Arc::new(PublishedState {
            resolved: serde_json::json!({
                "handle": handle,
                "pubkey": hex(&new_pubkey),
                "log_index": 1,
                "inclusion_proof": {
                    "tree_size": 2,
                    "root": hex(&log.root()),
                    "path": log.inclusion_proof(1, 2).unwrap()
                        .iter().map(|hash| hex(hash)).collect::<Vec<_>>(),
                    "checkpoint": note.clone(),
                }
            }),
            entries: serde_json::json!({
                "from": 0,
                "to": 2,
                "tree_size": 2,
                "root": hex(&log.root()),
                "checkpoint": note,
                "entries": entries,
            }),
        });
        let trust = RegistryTrust::new(
            ORIGIN,
            operator.verifying_key().to_bytes(),
            vec![WitnessKey::new("witness.test/registration", witness.verifying_key()).unwrap()],
            1,
            CheckpointPin {
                size: 0,
                root: pigeonpost_registry::log::empty_root(),
            },
            60,
            5,
        )
        .unwrap();
        (older, state, trust)
    }

    #[tokio::test]
    async fn publication_waiter_polls_an_older_witnessed_binding_until_rotation_is_visible() {
        let old_identity = SigningKey::from_bytes(&[84; 32]);
        let new_identity = SigningKey::from_bytes(&[85; 32]);
        let (older, current, trust) = witnessed_rotation_state(
            old_identity.verifying_key().to_bytes(),
            new_identity.verifying_key().to_bytes(),
        );
        let state = Arc::new(DelayedPublicationState {
            older,
            current,
            resolve_calls: AtomicUsize::new(0),
            phase: AtomicUsize::new(0),
        });
        let url = spawn(
            Router::new()
                .route("/v1/resolve/github/alice", get(delayed_resolved))
                .route("/v1/log/entries", get(delayed_entries))
                .route("/v1/log/consistency", get(delayed_consistency))
                .with_state(Arc::clone(&state)),
        )
        .await;
        let temporary = crate::test_support::private_tempdir();
        let home = temporary.path().join("agent");
        let agent = Agent::open(&home).unwrap();
        let bundle =
            pigeonpost_client::RegistryTrustBundle::from_registry_trust(&url, &trust).unwrap();
        agent
            .import_registry_trust(
                pigeonpost_client::RegistryTrustInput::from_json(
                    &serde_json::to_vec(&bundle).unwrap(),
                )
                .unwrap(),
            )
            .unwrap();
        let client = RegistryClient::new(&url, trust).unwrap();

        let (verified, _) = await_handle_publication(
            agent.state(),
            &client,
            &Handle::parse("/github/alice").unwrap(),
            &new_identity.verifying_key().to_bytes(),
            1,
            "handle_rotate",
            Instant::now() + Duration::from_secs(60),
        )
        .await
        .unwrap();
        assert_eq!(verified.log_index(), 1);
        assert_eq!(verified.entry_kind().as_str(), "handle_rotate");
        assert!(state.resolve_calls.load(Ordering::SeqCst) >= 2);
    }

    #[tokio::test]
    async fn publication_poll_rejects_index_drift_and_invalid_entries() {
        let identity = SigningKey::from_bytes(&[73; 32]);
        for (expected_pubkey, tamper_entry, expected_text) in [
            ([76; 32], false, "immutable binding receipt"),
            (
                identity.verifying_key().to_bytes(),
                true,
                "resolved projection",
            ),
        ] {
            let (state, trust) = witnessed_state(identity.verifying_key().to_bytes(), tamper_entry);
            let url = spawn(
                Router::new()
                    .route("/v1/resolve/github/alice", get(resolved))
                    .route("/v1/log/entries", get(entries))
                    .with_state(state),
            )
            .await;
            let temporary = crate::test_support::private_tempdir();
            let home = temporary.path().join("agent");
            let test_agent = Agent::open(&home).unwrap();
            let bundle =
                pigeonpost_client::RegistryTrustBundle::from_registry_trust(&url, &trust).unwrap();
            let input = pigeonpost_client::RegistryTrustInput::from_json(
                &serde_json::to_vec(&bundle).unwrap(),
            )
            .unwrap();
            test_agent.import_registry_trust(input).unwrap();
            let client = RegistryClient::new(&url, trust).unwrap();
            let error = await_handle_publication(
                test_agent.state(),
                &client,
                &Handle::parse("/github/alice").unwrap(),
                &expected_pubkey,
                0,
                "handle_bind",
                Instant::now() + Duration::from_secs(60),
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(error.contains(expected_text), "{error}");
        }
    }

    #[tokio::test]
    async fn fresh_home_can_rebind_a_lost_handle_and_receive_future_pigeonpost() {
        let _serial = CLAIM_TEST_SERIAL.lock().await;
        let old_identity = SigningKey::from_bytes(&[83; 32]);
        let recipient_directory = crate::test_support::private_tempdir();
        let recipient_home = recipient_directory.path().join("agent");
        let recipient = Agent::open(&recipient_home).unwrap();
        assert_ne!(
            recipient.verifying_key().to_bytes(),
            old_identity.verifying_key().to_bytes()
        );
        let (_, published, trust) = witnessed_rotation_state(
            old_identity.verifying_key().to_bytes(),
            recipient.verifying_key().to_bytes(),
        );
        let registry_url = spawn(
            Router::new()
                .route("/v1/rotate", post(rotate_receipt))
                .route("/v1/resolve/github/alice", get(resolved))
                .route("/v1/log/entries", get(entries))
                .route(
                    "/v1/compliance-keys",
                    get(|| async { StatusCode::SERVICE_UNAVAILABLE }),
                )
                .with_state(published),
        )
        .await;
        let bundle =
            pigeonpost_client::RegistryTrustBundle::from_registry_trust(&registry_url, &trust)
                .unwrap();
        let trust_json = serde_json::to_vec(&bundle).unwrap();
        recipient
            .import_registry_trust(
                pigeonpost_client::RegistryTrustInput::from_json(&trust_json).unwrap(),
            )
            .unwrap();

        rotate(
            &recipient,
            &registry_url,
            "/github/alice",
            ClaimProof {
                mock_name: Some("alice".into()),
                no_browser: true,
            },
            true,
        )
        .await
        .unwrap();

        let sender_directory = crate::test_support::private_tempdir();
        let sender_home = sender_directory.path().join("agent");
        let sender = Agent::open(&sender_home).unwrap();
        sender
            .import_registry_trust(
                pigeonpost_client::RegistryTrustInput::from_json(&trust_json).unwrap(),
            )
            .unwrap();
        let loft = spawn_test_loft().await;
        sender.add_loft(&loft).await.unwrap();
        recipient.add_loft(&loft).await.unwrap();
        recipient
            .allow_sender(&sender.verifying_key().to_bytes(), "recovery test")
            .unwrap();
        let destination =
            pigeonpost_core::Destination::parse(&format!("/github/alice?l={loft}")).unwrap();
        let sent = sender
            .send_to(&destination, "future delivery after total key loss")
            .await
            .unwrap();
        assert_eq!((sent.delivered, sent.queued), (1, 0));
        assert_eq!(recipient.drain().await.unwrap().new_messages, 1);
        assert!(recipient
            .inbox(false, 10)
            .unwrap()
            .iter()
            .any(|message| message.body.as_str() == "future delivery after total key loss"));
    }

    #[tokio::test]
    async fn publication_poll_timeout_is_total_and_bounded() {
        let operator = SigningKey::from_bytes(&[74; 32]);
        let witness = SigningKey::from_bytes(&[75; 32]);
        let trust = RegistryTrust::new(
            ORIGIN,
            operator.verifying_key().to_bytes(),
            vec![WitnessKey::new("witness.test/timeout", witness.verifying_key()).unwrap()],
            1,
            CheckpointPin {
                size: 0,
                root: pigeonpost_registry::log::empty_root(),
            },
            60,
            5,
        )
        .unwrap();
        let url = spawn(Router::new().route(
            "/v1/resolve/github/alice",
            get(|| async { StatusCode::NOT_FOUND }),
        ))
        .await;
        let temporary = crate::test_support::private_tempdir();
        let home = temporary.path().join("agent");
        let test_agent = Agent::open(&home).unwrap();
        let bundle =
            pigeonpost_client::RegistryTrustBundle::from_registry_trust(&url, &trust).unwrap();
        let input =
            pigeonpost_client::RegistryTrustInput::from_json(&serde_json::to_vec(&bundle).unwrap())
                .unwrap();
        test_agent.import_registry_trust(input).unwrap();
        let client = RegistryClient::new(&url, trust).unwrap();
        let error = tokio::time::timeout(
            Duration::from_secs(1),
            await_handle_publication(
                test_agent.state(),
                &client,
                &Handle::parse("/github/alice").unwrap(),
                &operator.verifying_key().to_bytes(),
                0,
                "handle_bind",
                Instant::now() + Duration::from_millis(100),
            ),
        )
        .await
        .expect("publication polling must honor its total deadline")
        .unwrap_err()
        .to_string();
        assert!(error.contains("timed out"));
    }

    #[tokio::test]
    async fn claim_holds_identity_lease_from_signature_through_witnessed_publication() {
        let _serial = CLAIM_TEST_SERIAL.lock().await;
        let temporary = crate::test_support::private_tempdir();
        let home = temporary.path().join("agent");
        let publisher = Agent::open(&home).unwrap();
        let mut rotator = Agent::open(&home).unwrap();
        let signed_key = publisher.verifying_key();
        let (published, trust) = witnessed_state(signed_key.to_bytes(), false);
        let race = Arc::new(ClaimRaceState {
            published,
            append_received: tokio::sync::Notify::new(),
            release_append: tokio::sync::Notify::new(),
            pause_publication_audit: AtomicBool::new(true),
            publication_audit_received: tokio::sync::Notify::new(),
            release_publication_audit: tokio::sync::Notify::new(),
        });
        let url = spawn(
            Router::new()
                .route("/v1/register", post(paused_register))
                .route("/v1/resolve/github/alice", get(race_resolved))
                .route("/v1/log/entries", get(race_entries))
                .with_state(Arc::clone(&race)),
        )
        .await;
        let bundle =
            pigeonpost_client::RegistryTrustBundle::from_registry_trust(&url, &trust).unwrap();
        let input =
            pigeonpost_client::RegistryTrustInput::from_json(&serde_json::to_vec(&bundle).unwrap())
                .unwrap();
        publisher.import_registry_trust(input).unwrap();

        let claiming = claim(
            &publisher,
            &url,
            "/github/alice",
            ClaimProof {
                mock_name: Some("alice".into()),
                no_browser: true,
            },
            true,
        );
        tokio::pin!(claiming);
        tokio::select! {
            () = race.append_received.notified() => {}
            result = &mut claiming => panic!("claim completed before append pause: {result:?}"),
        }

        let error = tokio::time::timeout(Duration::from_secs(10), rotator.rotate())
            .await
            .expect("rotation must not wait at the registry append boundary")
            .unwrap_err();
        assert!(matches!(
            error,
            pigeonpost_client::ClientError::Config(message)
                if message.contains("identity is busy")
        ));

        race.release_append.notify_one();
        tokio::select! {
            () = race.publication_audit_received.notified() => {}
            result = &mut claiming => panic!("claim completed before publication audit pause: {result:?}"),
        }

        let error = tokio::time::timeout(Duration::from_secs(10), rotator.rotate())
            .await
            .expect("rotation must not wait during witnessed publication audit")
            .unwrap_err();
        assert!(matches!(
            error,
            pigeonpost_client::ClientError::Config(message)
                if message.contains("identity is busy")
        ));

        race.release_publication_audit.notify_one();
        claiming.await.unwrap();
        assert_eq!(publisher.verifying_key(), signed_key);
        assert!(publisher.state().own_rotations().unwrap().is_empty());
        let reopened = Agent::open(&home).unwrap();
        assert_eq!(reopened.verifying_key(), signed_key);
        let released = publisher.identity_operation().unwrap();
        assert_eq!(released.verifying_key(), signed_key);
    }

    #[tokio::test]
    async fn rotation_during_proof_wait_aborts_before_registry_append() {
        let _serial = CLAIM_TEST_SERIAL.lock().await;
        let temporary = crate::test_support::private_tempdir();
        let home = temporary.path().join("agent");
        let publisher = Agent::open(&home).unwrap();
        let mut rotator = Agent::open(&home).unwrap();
        let signed_key = publisher.verifying_key();
        let loft = spawn_test_loft().await;
        publisher.add_loft(&loft).await.unwrap();

        let register_calls = Arc::new(AtomicUsize::new(0));
        let url = spawn(
            Router::new()
                .route("/v1/register", post(count_register))
                .with_state(Arc::clone(&register_calls)),
        )
        .await;
        let (_, trust) = witnessed_state(signed_key.to_bytes(), false);
        let bundle =
            pigeonpost_client::RegistryTrustBundle::from_registry_trust(&url, &trust).unwrap();
        let input =
            pigeonpost_client::RegistryTrustInput::from_json(&serde_json::to_vec(&bundle).unwrap())
                .unwrap();
        publisher.import_registry_trust(input).unwrap();

        let pause = Arc::new(ClaimProofTestPause {
            reached: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        *CLAIM_PROOF_TEST_PAUSE.lock().unwrap() = Some(Arc::clone(&pause));
        let claiming = claim(
            &publisher,
            &url,
            "/github/alice",
            ClaimProof {
                mock_name: Some("alice".into()),
                no_browser: true,
            },
            true,
        );
        tokio::pin!(claiming);
        tokio::select! {
            () = pause.reached.notified() => {}
            result = &mut claiming => panic!("claim completed before proof pause: {result:?}"),
        }

        // The claim reaches its proof pause *around* the point it releases the identity lease, so
        // whether the lease is free the instant the pause fires is a race the test has no way to
        // observe directly. Retry while it is still held rather than assuming: "busy" here is the
        // precondition not being ready yet, not the behaviour under test failing.
        let rotation_deadline = std::time::Instant::now() + Duration::from_secs(60);
        let rotation = loop {
            match rotator.rotate().await {
                Ok(rotation) => break rotation,
                Err(error)
                    if error.to_string().contains("identity is busy")
                        && std::time::Instant::now() < rotation_deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(error) => panic!("rotation during the proof pause failed: {error}"),
            }
        };
        assert_eq!(rotation.from, publisher.address());
        assert_ne!(rotation.to, rotation.from);

        pause.release.notify_one();
        let error = claiming.await.unwrap_err().to_string();
        *CLAIM_PROOF_TEST_PAUSE.lock().unwrap() = None;
        // Two refusals are correct here and which one appears is a timing detail: if the rotation
        // has already released its lease the claim sees the key changed underneath it, and if it
        // has not, the claim sees the identity still busy. Pinning one made this fail on loaded
        // runners while the behaviour under test was working.
        //
        // The property this test is named for is the assertion below — that neither refusal
        // reaches the registry. That stays exact.
        assert!(
            error.contains("identity changed") || error.contains("identity is busy"),
            "expected the claim to be refused for a changed or busy identity, got: {error}"
        );
        assert_eq!(register_calls.load(Ordering::SeqCst), 0);
        let reopened = Agent::open(&home).unwrap();
        assert_ne!(reopened.verifying_key(), signed_key);
    }

    #[tokio::test]
    async fn standalone_resolve_requires_and_persists_witnessed_trust() {
        let identity = SigningKey::from_bytes(&[76; 32]);
        let (published, trust) = witnessed_state(identity.verifying_key().to_bytes(), false);
        let url = spawn(
            Router::new()
                .route("/v1/resolve/github/alice", get(resolved))
                .route("/v1/log/entries", get(entries))
                .with_state(published),
        )
        .await;
        let temporary = crate::test_support::private_tempdir();
        let directory = temporary.path().join("agent");

        let error = resolve(&directory, &url, "/github/alice", true)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("import witnessed registry trust"), "{error}");
        assert!(!directory.join("identity.key").exists());

        let agent = Agent::open(&directory).unwrap();
        let bundle =
            pigeonpost_client::RegistryTrustBundle::from_registry_trust(&url, &trust).unwrap();
        let input =
            pigeonpost_client::RegistryTrustInput::from_json(&serde_json::to_vec(&bundle).unwrap())
                .unwrap();
        agent.import_registry_trust(input).unwrap();
        drop(agent);

        resolve(&directory, &url, "/github/alice", true)
            .await
            .unwrap();
        let state = pigeonpost_client::State::open(&directory.join("state.db")).unwrap();
        let configured = state.registry_configuration().unwrap().unwrap();
        assert_eq!(configured.checkpoint.unwrap().size, 1);
        assert!(state.handle_resolution("/github/alice").unwrap().is_some());

        let error = resolve(&directory, "http://127.0.0.1:9", "/github/alice", true)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("configured witnessed trust root"), "{error}");
    }

    #[test]
    fn pkce_encoding_matches_the_rfc_7636_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            base64url_no_pad(&Sha256::digest(verifier.as_bytes())),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(base64url_no_pad(&[0; 32]), "A".repeat(43));
    }

    #[test]
    fn github_callback_requires_one_matching_state_and_one_bounded_code() {
        let state = "a".repeat(CHALLENGE_BYTES);
        let request = format!(
            "GET /callback?code=abc%2Bdef%2F%3D%3D&state={state} HTTP/1.1\r\nHost: 127.0.0.1:8765\r\n\r\n"
        );
        assert_eq!(
            parse_callback_request(request.as_bytes(), &state, BrowserProvider::Github).unwrap(),
            CallbackEvent::Complete(CallbackProof::Github {
                code: "abc+def/==".into()
            })
        );

        for invalid in [
            format!(
                "GET /callback?code=secret&state={} HTTP/1.1\r\nHost: local\r\n\r\n",
                "b".repeat(CHALLENGE_BYTES)
            ),
            format!(
                "GET /callback?code=secret&code=second&state={state} HTTP/1.1\r\nHost: local\r\n\r\n"
            ),
            format!(
                "GET /wrong?code=secret&state={state} HTTP/1.1\r\nHost: local\r\n\r\n"
            ),
            format!(
                "POST /callback?code=secret&state={state} HTTP/1.1\r\nHost: local\r\nContent-Length: 0\r\n\r\n"
            ),
        ] {
            let error = parse_callback_request(
                invalid.as_bytes(),
                &state,
                BrowserProvider::Github,
            )
            .unwrap_err();
            assert!(matches!(
                error,
                "invalid local callback request" | "invalid local callback proof"
            ));
            assert!(!error.contains("secret"));
        }
    }

    #[test]
    fn google_fragment_relay_accepts_only_one_same_flow_capture() {
        let state = "c".repeat(CHALLENGE_BYTES);
        let relay = b"GET /callback HTTP/1.1\r\nHost: 127.0.0.1:8765\r\n\r\n";
        assert_eq!(
            parse_callback_request(relay, &state, BrowserProvider::Google).unwrap(),
            CallbackEvent::RelayFragment
        );

        let body = format!("id_token=header.payload.signature&state={state}");
        let capture = format!(
            "POST /capture HTTP/1.1\r\nHost: 127.0.0.1:8765\r\nOrigin: http://127.0.0.1:8765\r\nContent-Type: application/x-www-form-urlencoded; charset=UTF-8\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        assert_eq!(
            parse_callback_request(capture.as_bytes(), &state, BrowserProvider::Google).unwrap(),
            CallbackEvent::Complete(CallbackProof::Google {
                id_token: "header.payload.signature".into()
            })
        );

        for body in [
            format!("state={state}"),
            format!("id_token=secret&id_token=second&state={state}"),
            format!("id_token=secret&state={state}&state={state}"),
        ] {
            let invalid = format!(
                "POST /capture HTTP/1.1\r\nHost: 127.0.0.1:8765\r\nOrigin: http://127.0.0.1:8765\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let error = parse_callback_request(invalid.as_bytes(), &state, BrowserProvider::Google)
                .unwrap_err();
            assert_eq!(error, "invalid local callback proof");
            assert!(!error.contains("secret"));
        }
    }

    #[test]
    fn callback_parser_rejects_oversize_and_ambiguous_http_framing() {
        let state = "d".repeat(CHALLENGE_BYTES);
        assert_eq!(
            parse_callback_request(
                &vec![b'x'; CALLBACK_REQUEST_LIMIT + 1],
                &state,
                BrowserProvider::Github
            ),
            Err("invalid local callback request")
        );
        let ambiguous = "POST /capture HTTP/1.1\r\nHost: local\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n";
        assert_eq!(
            parse_callback_request(ambiguous.as_bytes(), &state, BrowserProvider::Google),
            Err("invalid local callback request")
        );
        let chunked =
            b"POST /capture HTTP/1.1\r\nHost: local\r\nTransfer-Encoding: chunked\r\n\r\n";
        assert_eq!(
            parse_callback_request(chunked, &state, BrowserProvider::Google),
            Err("invalid local callback request")
        );
    }

    #[test]
    fn manual_mode_accepts_only_the_full_fixed_callback_url() {
        let state = "1".repeat(CHALLENGE_BYTES);
        let github = format!("{CALLBACK_URL}?code=authorization-code&state={state}");
        assert_eq!(
            parse_manual_callback_url(&github, &state, BrowserProvider::Github).unwrap(),
            CallbackProof::Github {
                code: "authorization-code".into()
            }
        );

        let google = format!("{CALLBACK_URL}#id_token=header.payload.signature&state={state}");
        assert_eq!(
            parse_manual_callback_url(&google, &state, BrowserProvider::Google).unwrap(),
            CallbackProof::Google {
                id_token: "header.payload.signature".into()
            }
        );
        assert!(parse_manual_callback_url(&google, &state, BrowserProvider::Github).is_err());
        assert!(parse_manual_callback_url(
            &github.replace("127.0.0.1", "localhost"),
            &state,
            BrowserProvider::Github
        )
        .is_err());
    }

    #[test]
    fn callback_requires_one_exact_host_and_google_post_origin() {
        let state = "2".repeat(CHALLENGE_BYTES);
        let target = format!("/callback?code=code&state={state}");
        for head in [
            format!("GET {target} HTTP/1.1\r\n\r\n"),
            format!("GET {target} HTTP/1.1\r\nHost: localhost:8765\r\n\r\n"),
            format!(
                "GET {target} HTTP/1.1\r\nHost: {CALLBACK_HOST}\r\nHost: {CALLBACK_HOST}\r\n\r\n"
            ),
        ] {
            assert_eq!(
                parse_callback_request(head.as_bytes(), &state, BrowserProvider::Github),
                Err("invalid local callback request")
            );
        }

        let body = format!("id_token=header.payload.signature&state={state}");
        for origin_headers in [
            String::new(),
            "Origin: https://attacker.invalid\r\n".into(),
            format!("Origin: {CALLBACK_ORIGIN}\r\nOrigin: {CALLBACK_ORIGIN}\r\n"),
        ] {
            let request = format!(
                "POST /capture HTTP/1.1\r\nHost: {CALLBACK_HOST}\r\n{origin_headers}Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            assert_eq!(
                parse_callback_request(request.as_bytes(), &state, BrowserProvider::Google),
                Err("invalid local callback request")
            );
        }
    }

    #[test]
    fn callback_pages_use_hash_csp_and_never_claim_premature_verification() {
        assert!(!CALLBACK_CSP.contains("unsafe-inline"));
        assert!(CALLBACK_CSP.contains("frame-ancestors 'none'"));
        assert!(CALLBACK_CSP.matches("sha256-").count() >= 2);
        for page in [CALLBACK_SUCCESS_PAGE, GOOGLE_RELAY_PAGE] {
            let script = page
                .split_once("<script>")
                .and_then(|(_, rest)| rest.split_once("</script>"))
                .map(|(script, _)| script)
                .unwrap();
            let source = format!("'sha256-{}'", standard_base64(&Sha256::digest(script)));
            assert!(
                CALLBACK_CSP.contains(&source),
                "CSP hash drifted for {script}"
            );
        }
        assert!(!CALLBACK_SUCCESS_PAGE
            .to_ascii_lowercase()
            .contains("verified"));
        assert!(!GOOGLE_RELAY_PAGE.to_ascii_lowercase().contains("verified"));
        assert!(GOOGLE_RELAY_PAGE.contains("body.length>20480"));
    }

    fn standard_base64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            output.push(ALPHABET[(chunk[0] >> 2) as usize] as char);
            output.push(
                ALPHABET
                    [(((chunk[0] & 0x03) << 4) | chunk.get(1).copied().unwrap_or(0) >> 4) as usize]
                    as char,
            );
            if let Some(second) = chunk.get(1) {
                output.push(
                    ALPHABET[(((second & 0x0f) << 2) | chunk.get(2).copied().unwrap_or(0) >> 6)
                        as usize] as char,
                );
            } else {
                output.push('=');
            }
            if let Some(third) = chunk.get(2) {
                output.push(ALPHABET[(third & 0x3f) as usize] as char);
            } else {
                output.push('=');
            }
        }
        output
    }

    #[test]
    fn maximum_valid_id_token_fits_the_callback_request_bounds() {
        let state = "3".repeat(CHALLENGE_BYTES);
        let token = format!("a.b.{}", "c".repeat(MAX_ID_TOKEN_BYTES - 4));
        assert_eq!(token.len(), MAX_ID_TOKEN_BYTES);
        assert!(valid_id_token(&token));
        let body = format!("id_token={token}&state={state}");
        assert!(body.len() <= CALLBACK_FORM_LIMIT);
        let request = format!(
            "POST /capture HTTP/1.1\r\nHost: {CALLBACK_HOST}\r\nOrigin: {CALLBACK_ORIGIN}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        assert!(request.len() <= CALLBACK_REQUEST_LIMIT);
        assert!(matches!(
            parse_callback_request(request.as_bytes(), &state, BrowserProvider::Google),
            Ok(CallbackEvent::Complete(CallbackProof::Google { id_token })) if id_token == token
        ));
    }

    #[test]
    fn authorization_metadata_is_pinned_before_a_browser_is_opened() {
        let now = 10_000;
        let github = BrowserChallenge {
            provider: "github".into(),
            challenge: "e".repeat(CHALLENGE_BYTES),
            expires_at_ms: now + 60_000,
            client_id: "github-client".into(),
            authorization_endpoint: GITHUB_AUTHORIZATION_ENDPOINT.into(),
            response_type: "code".into(),
            response_mode: "query".into(),
            scopes: vec![],
            challenge_parameter: "state".into(),
            pkce_method: Some("S256".into()),
        };
        validate_browser_challenge(&github, BrowserProvider::Github, now).unwrap();
        let pkce = "x".repeat(43);
        let url = browser_authorization_url(&github, BrowserProvider::Github, Some(&pkce)).unwrap();
        let pairs = url
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            pairs.get("state").map(|value| value.as_ref()),
            Some(github.challenge.as_str())
        );
        assert_eq!(
            pairs
                .get("code_challenge_method")
                .map(|value| value.as_ref()),
            Some("S256")
        );
        assert!(!pairs.contains_key("redirect_uri"));

        let mut unsafe_metadata = github;
        unsafe_metadata.authorization_endpoint = "https://attacker.invalid/authorize".into();
        assert!(
            validate_browser_challenge(&unsafe_metadata, BrowserProvider::Github, now).is_err()
        );

        let google = BrowserChallenge {
            provider: "google".into(),
            challenge: "f".repeat(CHALLENGE_BYTES),
            expires_at_ms: now + 60_000,
            client_id: "client.apps.googleusercontent.com".into(),
            authorization_endpoint: GOOGLE_AUTHORIZATION_ENDPOINT.into(),
            response_type: "id_token".into(),
            response_mode: "fragment".into(),
            scopes: vec!["openid".into(), "profile".into()],
            challenge_parameter: "nonce".into(),
            pkce_method: None,
        };
        validate_browser_challenge(&google, BrowserProvider::Google, now).unwrap();
        let url = browser_authorization_url(&google, BrowserProvider::Google, None).unwrap();
        let pairs = url
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            pairs.get("redirect_uri").map(|value| value.as_ref()),
            Some(CALLBACK_URL)
        );
        assert_eq!(
            pairs.get("nonce").map(|value| value.as_ref()),
            Some(google.challenge.as_str())
        );
        assert_eq!(
            pairs.get("state").map(|value| value.as_ref()),
            Some(google.challenge.as_str())
        );
        assert_eq!(
            pairs.get("scope").map(|value| value.as_ref()),
            Some("openid profile")
        );
    }
}
