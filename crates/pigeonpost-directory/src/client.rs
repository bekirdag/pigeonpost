//! Hardened client for pinned, signed directory snapshots.

use std::time::Duration;

use pigeonpost_core::network::{is_localhost_name, is_numeric_loopback_host};
use reqwest::header::{ETAG, IF_NONE_MATCH};

use crate::document::{DirectoryDocument, MAX_DIRECTORY_DOCUMENT_BYTES};
use crate::error::{DirectoryError, Result};

const MAX_DOCUMENT_AGE_SECS: u64 = 24 * 60 * 60;
const MAX_FUTURE_SKEW_SECS: u64 = 5 * 60;
const MAX_ETAG_BYTES: usize = 256;

#[derive(Debug, Clone)]
pub enum FetchOutcome {
    Modified {
        document: DirectoryDocument,
        etag: Option<String>,
    },
    NotModified,
}

#[derive(Debug, Clone)]
pub struct DirectoryClient {
    document_url: reqwest::Url,
    expected_key: [u8; 32],
    http: reqwest::Client,
}

impl DirectoryClient {
    pub fn new(base_url: &str, expected_key: [u8; 32]) -> Result<Self> {
        let mut base = validate_base_url(base_url)?;
        if !base.path().ends_with('/') {
            let path = format!("{}/", base.path());
            base.set_path(&path);
        }
        let document_url = base
            .join("directory.json")
            .map_err(|_| DirectoryError::Malformed("invalid directory URL".into()))?;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .user_agent(concat!("pigeonpost-directory/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| DirectoryError::Unavailable)?;
        Ok(Self {
            document_url,
            expected_key,
            http,
        })
    }

    pub fn expected_key(&self) -> &[u8; 32] {
        &self.expected_key
    }

    pub async fn fetch(&self, previous_etag: Option<&str>, now: u64) -> Result<FetchOutcome> {
        let mut request = self.http.get(self.document_url.clone());
        if let Some(etag) = previous_etag.filter(|value| value.len() <= MAX_ETAG_BYTES) {
            request = request.header(IF_NONE_MATCH, etag);
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| DirectoryError::Unavailable)?;
        if response.status() == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(FetchOutcome::NotModified);
        }
        if response.status() != reqwest::StatusCode::OK {
            return Err(DirectoryError::Unavailable);
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_DIRECTORY_DOCUMENT_BYTES as u64)
        {
            return Err(DirectoryError::ResponseTooLarge);
        }

        let etag = response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .filter(|value| value.len() <= MAX_ETAG_BYTES)
            .map(str::to_owned);
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| DirectoryError::Unavailable)?
        {
            if body.len().saturating_add(chunk.len()) > MAX_DIRECTORY_DOCUMENT_BYTES {
                return Err(DirectoryError::ResponseTooLarge);
            }
            body.extend_from_slice(&chunk);
        }

        let document: DirectoryDocument = serde_json::from_slice(&body)?;
        verify_snapshot(&document, &self.expected_key, now)?;
        Ok(FetchOutcome::Modified { document, etag })
    }
}

/// Revalidate a cached document before using it for routing or selection.
pub fn verify_snapshot(
    document: &DirectoryDocument,
    expected_key: &[u8; 32],
    now: u64,
) -> Result<()> {
    document.verify(expected_key)?;
    if document.generated_at > now.saturating_add(MAX_FUTURE_SKEW_SECS)
        || now.saturating_sub(document.generated_at) > MAX_DOCUMENT_AGE_SECS
    {
        return Err(DirectoryError::StaleDocument);
    }
    for loft in &document.lofts {
        validate_loft_endpoint(&loft.endpoint)?;
    }
    Ok(())
}

/// Parse and normalize one directory service origin under the same boundary used for fetching.
///
/// The returned spelling has no trailing slash, so it is suitable as the exact durable pin key
/// and as an operator confirmation value.
pub fn canonical_directory_url(input: &str) -> Result<String> {
    Ok(validate_base_url(input)?
        .as_str()
        .trim_end_matches('/')
        .to_owned())
}

fn validate_base_url(input: &str) -> Result<reqwest::Url> {
    if input.len() > 2_048 {
        return Err(DirectoryError::Malformed(
            "directory URL is too long".into(),
        ));
    }
    let url = reqwest::Url::parse(input)
        .map_err(|_| DirectoryError::Malformed("invalid directory URL".into()))?;
    validate_http_origin(&url, false)?;
    Ok(url)
}

fn validate_loft_endpoint(input: &str) -> Result<()> {
    if input.len() > 2_048 {
        return Err(DirectoryError::Malformed(
            "loft endpoint is too long".into(),
        ));
    }
    let url = reqwest::Url::parse(input)
        .map_err(|_| DirectoryError::Malformed("invalid loft endpoint".into()))?;
    validate_http_origin(&url, false)
}

fn validate_http_origin(url: &reqwest::Url, allow_path: bool) -> Result<()> {
    if url.cannot_be_a_base()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
        || url.port() == Some(0)
        || (!allow_path && url.path() != "/" && !url.path().is_empty())
    {
        return Err(DirectoryError::Malformed(
            "service URL must not contain credentials, query, fragment, or an endpoint path".into(),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| DirectoryError::Malformed("service URL has no host".into()))?;
    if is_localhost_name(host) {
        return Err(DirectoryError::Malformed(
            "service URL cannot use a localhost DNS name".into(),
        ));
    }
    let loopback = is_numeric_loopback_host(host);
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err(DirectoryError::Malformed(
            "service URL must use HTTPS (HTTP is loopback-only)".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "server")]
    use std::sync::Arc;

    #[cfg(feature = "server")]
    use axum::{routing::get, Json, Router};

    #[cfg(feature = "server")]
    use crate::{server, Directory};

    #[cfg(feature = "server")]
    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[cfg(feature = "server")]
    async fn spawn(router: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        (url, task)
    }

    #[test]
    fn urls_fail_closed() {
        for bad in [
            "http://example.com",
            "http://localhost:1234",
            "http://localhost.:1234",
            "https://localhost:1234",
            "https://localhost.:1234",
            "https://api.localhost:1234",
            "https://example.com:0",
            "https://user:secret@example.com",
            "https://example.com?view=other",
            "https://example.com/directory",
            "file:///tmp/directory.json",
        ] {
            assert!(
                DirectoryClient::new(bad, [0; 32]).is_err(),
                "accepted {bad}"
            );
        }
        assert!(DirectoryClient::new("https://directory.example", [0; 32]).is_ok());
        assert!(DirectoryClient::new("http://127.0.0.1:1234", [0; 32]).is_ok());
    }

    #[cfg(feature = "server")]
    #[tokio::test]
    async fn fetch_verifies_the_pin_and_honors_etags() {
        let directory = Arc::new(Directory::in_memory().unwrap());
        directory.mark_probe_sweep(now()).unwrap();
        let key = directory.signing_public_key();
        let (url, task) = spawn(server::router(directory)).await;
        let client = DirectoryClient::new(&url, key).unwrap();

        let first = client.fetch(None, now()).await.unwrap();
        let etag = match first {
            FetchOutcome::Modified { document, etag } => {
                assert_eq!(document.version, 1);
                etag.expect("the cacheable endpoint supplies an ETag")
            }
            FetchOutcome::NotModified => panic!("an unconditional first read cannot be unchanged"),
        };
        assert!(matches!(
            client.fetch(Some(&etag), now()).await.unwrap(),
            FetchOutcome::NotModified
        ));

        let mut wrong = key;
        wrong[0] ^= 0x80;
        assert!(DirectoryClient::new(&url, wrong)
            .unwrap()
            .fetch(None, now())
            .await
            .is_err());
        task.abort();
    }

    #[cfg(feature = "server")]
    #[tokio::test]
    async fn a_tampered_document_is_rejected_before_use() {
        let directory = Arc::new(Directory::in_memory().unwrap());
        directory.mark_probe_sweep(now()).unwrap();
        let key = directory.signing_public_key();
        let (good_url, good_task) = spawn(server::router(directory)).await;
        let good = DirectoryClient::new(&good_url, key)
            .unwrap()
            .fetch(None, now())
            .await
            .unwrap();
        let mut document = match good {
            FetchOutcome::Modified { document, .. } => document,
            FetchOutcome::NotModified => unreachable!(),
        };
        good_task.abort();
        document.generated_at = document.generated_at.saturating_add(1);

        let router = Router::new().route(
            "/directory.json",
            get(move || {
                let document = document.clone();
                async move { Json(document) }
            }),
        );
        let (bad_url, bad_task) = spawn(router).await;
        assert!(DirectoryClient::new(&bad_url, key)
            .unwrap()
            .fetch(None, now())
            .await
            .is_err());
        bad_task.abort();
    }

    #[test]
    fn canonical_directory_urls_have_one_exact_pin_spelling() {
        assert_eq!(
            canonical_directory_url("HTTPS://EXAMPLE.COM:443/").unwrap(),
            "https://example.com"
        );
        assert_eq!(
            canonical_directory_url("http://127.0.0.1:7719/").unwrap(),
            "http://127.0.0.1:7719"
        );
        assert!(canonical_directory_url("https://example.com/path").is_err());
        assert!(canonical_directory_url("http://localhost:7719").is_err());
    }
}
