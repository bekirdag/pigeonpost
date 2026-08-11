//! The client half of the loft protocol.
//!
//! Deliberately stateless — no cursor storage, no outbox, no retry policy. Those belong to
//! `pigeonpost-client` (M2), which owns the SQLite file. This is the transport it will sit on,
//! and what M1 uses to prove offline delivery end to end.

use std::net::{IpAddr, SocketAddr};

use pigeonpost_core::{
    envelope::Wrap,
    network::{is_localhost_name, is_public_network_address as is_public_ip},
    record::{AgentRecord, RotationRecord},
    Address, FetchAuth, Identity, RecipientPolicy,
};

use crate::wire::*;

const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const DNS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
const MAX_RESOLVED_ADDRESSES: usize = 16;
const MAX_CONTROL_RESPONSE_BYTES: usize = 64 * 1024;
/// Public v0.2 fetch-page ceiling. This matches the loft server's bounded default response budget.
/// Clients reserve this full amount before consuming a fetch body, so a missing or dishonest
/// `Content-Length` cannot turn drain concurrency into unbounded buffering.
pub const MAX_FETCH_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_SERVICE_URL_BYTES: usize = 2_048;

/// Coarse, terminal-safe classification of a loft refusal.
///
/// A remote service controls its response body. Keeping only an allowlisted code prevents ANSI,
/// control characters, private data, or log-forging text from crossing into CLI and log output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalCode {
    InvalidRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    TooLarge,
    RateLimited,
    AtCapacity,
    Unavailable,
    Other,
}

impl RefusalCode {
    const fn for_status(status: u16) -> Self {
        match status {
            400 | 405 | 415 | 422 => Self::InvalidRequest,
            401 => Self::Unauthorized,
            403 => Self::Forbidden,
            404 => Self::NotFound,
            409 => Self::Conflict,
            413 => Self::TooLarge,
            429 => Self::RateLimited,
            507 => Self::AtCapacity,
            408 | 425 | 500..=506 | 508..=599 => Self::Unavailable,
            _ => Self::Other,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::TooLarge => "too_large",
            Self::RateLimited => "rate_limited",
            Self::AtCapacity => "at_capacity",
            Self::Unavailable => "unavailable",
            Self::Other => "refused",
        }
    }
}

impl core::fmt::Display for RefusalCode {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

// Preserve source compatibility for callers that constructed test errors with `message: text.into()`.
// Arbitrary text is intentionally discarded rather than classified from attacker-controlled words.
impl From<&str> for RefusalCode {
    fn from(_value: &str) -> Self {
        Self::Other
    }
}

impl From<String> for RefusalCode {
    fn from(_value: String) -> Self {
        Self::Other
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),

    #[error("loft refused the request ({status}): {message}")]
    Refused {
        status: u16,
        /// Legacy field name retained for source compatibility; this is an allowlisted code, not
        /// remote response text.
        message: RefusalCode,
    },

    #[error("loft response exceeded the configured bound")]
    ResponseTooLarge,

    #[error("loft returned an unsupported content encoding")]
    UnsupportedEncoding,

    #[error("loft returned malformed JSON")]
    Decode(#[source] serde_json::Error),

    #[error("invalid loft service URL")]
    InvalidUrl,

    #[error("loft service URL is unsafe for a network-sourced target")]
    UnsafeNetworkTarget,

    #[error("loft service address resolution failed")]
    ResolutionFailed,

    #[error("loft response was bound to a different service origin")]
    OriginMismatch,

    #[error("loft returned an incompatible protocol version")]
    ProtocolMismatch,

    #[error(transparent)]
    Core(#[from] pigeonpost_core::Error),
}

type Result<T> = std::result::Result<T, ClientError>;

/// A validated loft origin and, for network-sourced origins, its DNS-pinned public address.
///
/// Parsing and connection construction live together so callers cannot validate one string and
/// accidentally request another. `network` is the only constructor suitable for an address hint,
/// signed agent record, directory response, or persisted route learned from the network.
#[derive(Debug, Clone)]
pub struct LoftEndpoint {
    base_url: String,
    pin: Option<(String, SocketAddr)>,
    exact_loopback: bool,
}

impl LoftEndpoint {
    /// Validate an explicitly operator-configured local origin. This constructor is deliberately
    /// limited to exact numeric loopback addresses; every public HTTPS origin must pass through
    /// [`Self::network`] so DNS validation and connection pinning cannot be skipped.
    pub fn explicit(input: impl AsRef<str>) -> Result<Self> {
        let url = validated_base_url(input.as_ref(), true)?;
        let exact_loopback = exact_loopback_host(&url);
        if !exact_loopback {
            return Err(ClientError::UnsafeNetworkTarget);
        }
        Ok(Self {
            base_url: canonical_base_url(&url),
            pin: None,
            exact_loopback,
        })
    }

    /// Validate an untrusted routing origin, resolve it under a strict budget, reject every
    /// non-public answer, and pin the selected address while retaining TLS hostname validation.
    pub async fn network(input: impl AsRef<str>) -> Result<Self> {
        let url = validated_base_url(input.as_ref(), false)?;
        let host = url.host_str().ok_or(ClientError::InvalidUrl)?;
        let resolution_host = host.trim_start_matches('[').trim_end_matches(']');
        let port = url.port_or_known_default().ok_or(ClientError::InvalidUrl)?;
        let literal = resolution_host.parse::<IpAddr>().ok();
        let addresses = if let Some(address) = literal {
            vec![SocketAddr::new(address, port)]
        } else {
            tokio::time::timeout(
                DNS_TIMEOUT,
                tokio::net::lookup_host((resolution_host, port)),
            )
            .await
            .map_err(|_| ClientError::ResolutionFailed)?
            .map_err(|_| ClientError::ResolutionFailed)?
            .take(MAX_RESOLVED_ADDRESSES + 1)
            .collect()
        };
        Self::from_network_resolution(url, literal.is_none(), addresses)
    }

    fn from_network_resolution(
        url: reqwest::Url,
        hostname_needs_pin: bool,
        addresses: Vec<SocketAddr>,
    ) -> Result<Self> {
        if addresses.is_empty() || addresses.len() > MAX_RESOLVED_ADDRESSES {
            return Err(ClientError::ResolutionFailed);
        }
        if addresses.iter().any(|address| !is_public_ip(address.ip())) {
            return Err(ClientError::UnsafeNetworkTarget);
        }
        let pin = hostname_needs_pin.then(|| {
            (
                url.host_str()
                    .expect("validated loft origin always has a host")
                    .to_owned(),
                addresses[0],
            )
        });
        Ok(Self {
            base_url: canonical_base_url(&url),
            pin,
            exact_loopback: false,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// True only for a numeric loopback origin. Hostnames such as `localhost` are deliberately not
    /// treated as exact because name service configuration can redirect them.
    pub fn is_exact_loopback(&self) -> bool {
        self.exact_loopback
    }

    fn http_client(&self) -> Result<reqwest::Client> {
        let mut builder = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .pool_max_idle_per_host(4);
        if let Some((host, address)) = self.pin.as_ref() {
            builder = builder.resolve(host, *address);
        }
        Ok(builder.build()?)
    }
}

/// A handle to one loft.
#[derive(Debug, Clone)]
pub struct LoftClient {
    base_url: String,
    http: reqwest::Client,
}

impl LoftClient {
    /// Connect to an explicitly configured exact-loopback origin. Public origins, including
    /// operator-entered ones, must use [`Self::new_untrusted`] so DNS/IP validation is universal.
    pub fn new(base_url: impl AsRef<str>) -> Result<Self> {
        Self::from_endpoint(LoftEndpoint::explicit(base_url)?)
    }

    pub async fn new_untrusted(base_url: impl AsRef<str>) -> Result<Self> {
        Self::from_endpoint(LoftEndpoint::network(base_url).await?)
    }

    pub fn from_endpoint(endpoint: LoftEndpoint) -> Result<Self> {
        let http = endpoint.http_client()?;
        Ok(LoftClient {
            base_url: endpoint.base_url,
            http,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub async fn info(&self) -> Result<InfoResponse> {
        let info: InfoResponse = self
            .get(
                &format!("{}/v1/info", self.base_url),
                MAX_CONTROL_RESPONSE_BYTES,
            )
            .await?;
        if info.origin != self.base_url
            || pigeonpost_core::fetch_auth::validate_loft_origin(&info.origin).is_err()
        {
            return Err(ClientError::OriginMismatch);
        }
        if info.protocol != pigeonpost_core::PROTOCOL_VERSION {
            return Err(ClientError::ProtocolMismatch);
        }
        Ok(info)
    }

    /// Deposit a message. The recipient chose this loft, not the sender — `send` just follows
    /// the loft list published at the address (`docs/network.md`).
    pub async fn publish(&self, wrap: &Wrap, token: Option<String>) -> Result<PublishResponse> {
        self.post(
            &format!("{}/v1/publish", self.base_url),
            &PublishRequest {
                wrap: wrap.clone(),
                token,
            },
            MAX_CONTROL_RESPONSE_BYTES,
        )
        .await
    }

    /// Drain waiting mail. Signs a fresh proof each call — it is one signature, and the proof is
    /// worthless to anyone who intercepts it minutes later.
    pub async fn fetch(
        &self,
        identity: &Identity,
        loft_pubkey: &[u8; 32],
        cursor: u64,
        now_secs: u64,
        limit: Option<usize>,
    ) -> Result<FetchResponse> {
        let auth = FetchAuth::new(identity, loft_pubkey, &self.base_url, now_secs / 60, cursor)?;
        self.post(
            &format!("{}/v1/fetch", self.base_url),
            &FetchRequest { auth, limit },
            MAX_FETCH_RESPONSE_BYTES,
        )
        .await
    }

    pub async fn set_policy(&self, policy: &RecipientPolicy) -> Result<()> {
        self.post_no_content(
            &format!("{}/v1/policy", self.base_url),
            &PolicyRequest {
                policy: policy.clone(),
            },
        )
        .await
    }

    pub async fn put_agent_record(&self, address: &Address, record: &AgentRecord) -> Result<()> {
        let url = format!("{}/v1/agent/{}", self.base_url, encode_address(address));
        let response = self
            .http
            .put(&url)
            .json(&AgentRecordRequest {
                record: record.clone(),
            })
            .send()
            .await?;
        Self::check(response).await?;
        Ok(())
    }

    /// Fetch and **verify** an agent record. Verification against the address is what makes it
    /// safe to ask any loft at all: a wrong answer is arithmetic the caller can catch.
    pub async fn agent_record(&self, address: &Address) -> Result<AgentRecord> {
        let url = format!("{}/v1/agent/{}", self.base_url, encode_address(address));
        let body: AgentRecordRequest = self.get(&url, MAX_CONTROL_RESPONSE_BYTES).await?;
        body.record.verify(address)?;
        Ok(body.record)
    }

    /// Publish the single immutable transition at an old key address. Both key signatures are
    /// checked again by the loft against its pinned source record.
    pub async fn put_rotation_record(
        &self,
        address: &Address,
        record: &RotationRecord,
    ) -> Result<()> {
        record.verify_source_address(address)?;
        let url = format!("{}/v1/rotation/{}", self.base_url, encode_address(address));
        let response = self
            .http
            .put(&url)
            .json(&RotationRecordRequest {
                record: record.clone(),
            })
            .send()
            .await?;
        Self::check(response).await?;
        Ok(())
    }

    /// Fetch an immutable rotation record and at minimum bind it to the requested old address.
    /// The stateful client additionally verifies both signatures against its pinned commitment and
    /// last sequence before following the transition.
    pub async fn rotation_record(&self, address: &Address) -> Result<RotationRecord> {
        let url = format!("{}/v1/rotation/{}", self.base_url, encode_address(address));
        let body: RotationRecordRequest = self.get(&url, MAX_CONTROL_RESPONSE_BYTES).await?;
        body.record.verify_source_address(address)?;
        Ok(body.record)
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, url: &str, limit: usize) -> Result<T> {
        let response = self.http.get(url).send().await?;
        Self::decode(response, limit).await
    }

    async fn post<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        body: &B,
        limit: usize,
    ) -> Result<T> {
        let response = self.http.post(url).json(body).send().await?;
        Self::decode(response, limit).await
    }

    async fn post_no_content<B: serde::Serialize>(&self, url: &str, body: &B) -> Result<()> {
        let response = self.http.post(url).json(body).send().await?;
        Self::check(response).await?;
        Ok(())
    }

    async fn decode<T: serde::de::DeserializeOwned>(
        response: reqwest::Response,
        limit: usize,
    ) -> Result<T> {
        let response = Self::check(response).await?;
        let body = Self::bounded_body(response, limit).await?;
        serde_json::from_slice(&body).map_err(ClientError::Decode)
    }

    async fn check(response: reqwest::Response) -> Result<reqwest::Response> {
        if response.status().is_success() {
            if response
                .headers()
                .get(reqwest::header::CONTENT_ENCODING)
                .is_some_and(|encoding| encoding.as_bytes() != b"identity")
            {
                return Err(ClientError::UnsupportedEncoding);
            }
            return Ok(response);
        }
        let status = response.status().as_u16();
        // Never retain or render the untrusted body. Status is sufficient for retry policy, and a
        // closed allowlist is safe to forward into a terminal, structured output, or ordinary log.
        Err(ClientError::Refused {
            status,
            message: RefusalCode::for_status(status),
        })
    }

    async fn bounded_body(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
        if response
            .content_length()
            .is_some_and(|length| length > limit as u64)
        {
            return Err(ClientError::ResponseTooLarge);
        }
        let mut body = Vec::with_capacity(
            response
                .content_length()
                .and_then(|length| usize::try_from(length).ok())
                .unwrap_or(0)
                .min(limit),
        );
        while let Some(chunk) = response.chunk().await? {
            if chunk.len() > limit.saturating_sub(body.len()) {
                return Err(ClientError::ResponseTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

/// Addresses contain `/`, so the tier prefix has to survive being a path segment.
fn encode_address(address: &Address) -> String {
    address.as_str().trim_start_matches('/').replace('/', "-")
}

fn validated_base_url(input: &str, allow_exact_loopback_http: bool) -> Result<reqwest::Url> {
    if input.is_empty() || input.len() > MAX_SERVICE_URL_BYTES {
        return Err(ClientError::InvalidUrl);
    }
    let url = reqwest::Url::parse(input).map_err(|_| ClientError::InvalidUrl)?;
    if url.cannot_be_a_base()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
        || url.host_str().is_none()
        || url.host_str().is_some_and(is_localhost_name)
        || url.port() == Some(0)
    {
        return Err(ClientError::InvalidUrl);
    }
    if url.scheme() != "https"
        && !(allow_exact_loopback_http && url.scheme() == "http" && exact_loopback_host(&url))
    {
        return Err(ClientError::InvalidUrl);
    }
    Ok(url)
}

fn canonical_base_url(url: &reqwest::Url) -> String {
    url.as_str().trim_end_matches('/').to_string()
}

fn exact_loopback_host(url: &reqwest::Url) -> bool {
    url.host_str()
        .map(|host| host.trim_start_matches('[').trim_end_matches(']'))
        .and_then(|host| host.parse::<IpAddr>().ok())
        .is_some_and(|address| address.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn serve_one_raw_response(response: String) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 2_048];
            let _ = stream.read(&mut request).await;
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        format!("http://{address}")
    }

    #[test]
    fn loft_origins_are_bounded_and_https_except_for_exact_loopback() {
        assert!(LoftClient::new("https://loft.example").is_err());
        assert!(LoftClient::new("http://127.0.0.1:7717").is_ok());
        assert!(LoftClient::new("http://[::1]:7717/").is_ok());
        assert!(LoftClient::new("http://localhost:7717").is_err());
        assert!(LoftClient::new("https://localhost:7717").is_err());
        assert!(LoftClient::new("https://localhost.:7717").is_err());
        assert!(LoftClient::new("https://api.localhost:7717").is_err());
        assert!(LoftClient::new("http://192.0.2.1:7717").is_err());
        assert!(LoftClient::new("https://user@loft.example").is_err());
        assert!(LoftClient::new("https://loft.example/internal").is_err());
        assert!(LoftClient::new("https://loft.example?target=internal").is_err());
        assert!(LoftClient::new("https://loft.example:0").is_err());
    }

    #[tokio::test]
    async fn network_origins_reject_loopback_private_and_reserved_literals() {
        for origin in [
            "http://127.0.0.1:7717",
            "https://127.0.0.1:7717",
            "https://10.0.0.1",
            "https://169.254.169.254",
            "https://172.16.0.1",
            "https://192.168.0.1",
            "https://192.0.2.1",
            "https://198.51.100.1",
            "https://203.0.113.1",
            "https://[::1]",
            "https://[fc00::1]",
            "https://[fe80::1]",
            "https://[2001:db8::1]",
            "https://[::ffff:127.0.0.1]",
            "https://[100::1]",
            "https://[100:0:0:1::1]",
            "https://[3fff::1]",
            "https://[5f00::1]",
        ] {
            assert!(LoftClient::new_untrusted(origin).await.is_err(), "{origin}");
        }
        assert!(LoftClient::new_untrusted("https://8.8.8.8").await.is_ok());
        assert!(LoftClient::new_untrusted("https://[2606:4700:4700::1111]")
            .await
            .is_ok());
    }

    #[test]
    fn mixed_or_excessive_dns_answers_fail_closed_and_public_answer_is_pinned() {
        let url = validated_base_url("https://loft.example:8443", false).unwrap();
        let mixed = vec![
            "93.184.216.34:8443".parse().unwrap(),
            "127.0.0.1:8443".parse().unwrap(),
        ];
        assert!(LoftEndpoint::from_network_resolution(url.clone(), true, mixed).is_err());

        let excessive = (0..=MAX_RESOLVED_ADDRESSES)
            .map(|index| SocketAddr::from(([8, 8, 8, index as u8], 8443)))
            .collect();
        assert!(LoftEndpoint::from_network_resolution(url.clone(), true, excessive).is_err());

        let public = "93.184.216.34:8443".parse().unwrap();
        let endpoint = LoftEndpoint::from_network_resolution(url, true, vec![public]).unwrap();
        assert_eq!(endpoint.pin, Some(("loft.example".into(), public)));
        assert!(!endpoint.is_exact_loopback());
    }

    #[tokio::test]
    async fn control_responses_are_bounded_before_allocation_and_encoding_is_explicit() {
        let oversized = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            MAX_CONTROL_RESPONSE_BYTES + 1
        );
        let origin = serve_one_raw_response(oversized).await;
        let error = LoftClient::new(origin).unwrap().info().await.unwrap_err();
        assert!(matches!(error, ClientError::ResponseTooLarge));

        let compressed = "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-encoding: gzip\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}".to_owned();
        let origin = serve_one_raw_response(compressed).await;
        let error = LoftClient::new(origin).unwrap().info().await.unwrap_err();
        assert!(matches!(error, ClientError::UnsupportedEncoding));
    }

    #[tokio::test]
    async fn refusal_body_cannot_inject_terminal_or_log_controls() {
        let body = r#"{"error":"\u001b[31mREMOTE-CANARY\u0007\nforged-line"}"#;
        let response = format!(
            "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let origin = serve_one_raw_response(response).await;
        let error = LoftClient::new(origin).unwrap().info().await.unwrap_err();

        assert!(matches!(
            &error,
            ClientError::Refused {
                status: 400,
                message: RefusalCode::InvalidRequest,
            }
        ));
        let rendered = error.to_string();
        assert_eq!(rendered, "loft refused the request (400): invalid_request");
        assert!(!rendered.contains("REMOTE-CANARY"));
        assert!(!rendered.chars().any(char::is_control));
        let debug = format!("{error:?}");
        assert!(!debug.contains("REMOTE-CANARY"));
        assert!(!debug.chars().any(|character| character == '\u{1b}'));
    }

    #[tokio::test]
    async fn fetch_rejects_a_response_over_the_eight_mib_protocol_ceiling() {
        assert_eq!(MAX_FETCH_RESPONSE_BYTES, 8 * 1024 * 1024);
        let oversized = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            MAX_FETCH_RESPONSE_BYTES + 1
        );
        let origin = serve_one_raw_response(oversized).await;
        let identity = Identity::from_seed([0x42; 32]);
        let error = LoftClient::new(origin)
            .unwrap()
            .fetch(&identity, &[0x24; 32], 0, 60, None)
            .await
            .unwrap_err();
        assert!(matches!(error, ClientError::ResponseTooLarge));
    }

    #[tokio::test]
    async fn a_chunked_body_cannot_bypass_the_streaming_ceiling() {
        let chunked = concat!(
            "HTTP/1.1 200 OK\r\n",
            "content-type: application/json\r\n",
            "transfer-encoding: chunked\r\n",
            "connection: close\r\n\r\n",
            "10\r\n0123456789abcdef\r\n",
            "1\r\nx\r\n",
            "0\r\n\r\n"
        )
        .to_owned();
        let origin = serve_one_raw_response(chunked).await;
        let client = LoftClient::new(&origin).unwrap();
        let response = client
            .http
            .get(format!("{origin}/v1/fetch"))
            .send()
            .await
            .unwrap();
        let error = LoftClient::bounded_body(response, 16).await.unwrap_err();
        assert!(matches!(error, ClientError::ResponseTooLarge));
    }

    #[tokio::test]
    async fn info_must_name_the_exact_requested_origin() {
        let body = serde_json::json!({
            "software": "pigeonpost-loft",
            "version": "0.2.0",
            "protocol": "pigeonpost/3",
            "pubkey": "11".repeat(32),
            "origin": "http://127.0.0.1:9",
            "capacity_bytes": 1024,
            "used_bytes": 0,
            "utilization": 0.0,
            "retention_days": 30,
            "open": true,
            "pow_floor": 0,
            "max_event_bytes": 1024,
            "event_count": 0,
            "accepting": true
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let origin = serve_one_raw_response(response).await;

        let error = LoftClient::new(origin).unwrap().info().await.unwrap_err();
        assert!(matches!(error, ClientError::OriginMismatch));
    }
}
