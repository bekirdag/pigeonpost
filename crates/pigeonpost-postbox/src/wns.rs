//! Windows Push Notification Services, for incoming calls on Windows (Phase 1c,
//! `docs/call-alerting.md`).
//!
//! A Windows device registers its push channel URI as its token. The postbox only ever sends to a
//! channel whose host is Microsoft's (`*.notify.windows.com`), so a registration cannot point it at
//! anything else.
//!
//! Authentication is the Microsoft identity platform client-credentials flow (Windows App SDK
//! push): an Entra app registration whose Application (client) ID is mapped to the Store package's
//! family name, a client secret of that registration, and its directory (tenant) ID. The token
//! comes from `login.microsoftonline.com/{tenant}/oauth2/v2.0/token` with the scope
//! `https://wns.windows.com/.default`. The older package-SID flow against `login.live.com` is
//! refused by Microsoft for new apps. With nothing configured [`Wns::from_env`] returns `None` and
//! Windows rings only while the app runs.

use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::Mutex;

const AUTHORITY: &str = "https://login.microsoftonline.com";
const SCOPE: &str = "https://wns.windows.com/.default";
/// WNS tags are at most 16 characters.
const TAG_CHARS: usize = 16;
pub const CALL_GROUP: &str = "calls";
/// A cached token is replaced this long before Entra says it expires.
const TOKEN_MARGIN_SECS: u64 = 300;
/// After Entra refuses the credentials, rings do not ask again for this long.
const REFUSAL_BACKOFF_SECS: u64 = 60;

/// Whether `token` is a WNS channel URI this postbox will send to.
pub fn valid_channel_uri(token: &str) -> bool {
    if token.is_empty() || token.len() > 2048 {
        return false;
    }
    let Ok(url) = reqwest::Url::parse(token) else {
        return false;
    };
    let host_ok = url.host_str().is_some_and(|h| {
        let h = h.to_ascii_lowercase();
        h.ends_with(".notify.windows.com")
            && h.len() > ".notify.windows.com".len()
            && h.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-')
    });
    url.scheme() == "https"
        && host_ok
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.fragment().is_none()
}

/// The Entra app registration the postbox signs in as.
#[derive(Clone)]
pub struct Credentials {
    pub tenant: String,
    pub client_id: String,
    pub secret: String,
}

impl Credentials {
    fn from_env() -> Option<Self> {
        let tenant = non_empty("PIGEONPOST_WNS_TENANT_ID");
        let client_id = non_empty("PIGEONPOST_WNS_CLIENT_ID");
        let secret = non_empty("PIGEONPOST_WNS_CLIENT_SECRET").or_else(|| {
            non_empty("PIGEONPOST_WNS_CLIENT_SECRET_FILE")
                .and_then(|path| std::fs::read_to_string(path).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });
        if tenant.is_none()
            && client_id.is_none()
            && non_empty("PIGEONPOST_WNS_PACKAGE_SID").is_some()
        {
            tracing::warn!(
                "PIGEONPOST_WNS_PACKAGE_SID is no longer used: Microsoft retired the login.live.com \
                 WNS flow; set PIGEONPOST_WNS_TENANT_ID and PIGEONPOST_WNS_CLIENT_ID"
            );
        }
        let (Some(tenant), Some(client_id), Some(secret)) = (tenant, client_id, secret) else {
            return None;
        };
        if !valid_tenant(&tenant) || !is_guid(&client_id) {
            tracing::warn!(
                "WNS not configured: PIGEONPOST_WNS_TENANT_ID must be a tenant id or domain and \
                 PIGEONPOST_WNS_CLIENT_ID an application (client) id"
            );
            return None;
        }
        Some(Self {
            tenant,
            client_id,
            secret,
        })
    }
}

struct Bearer {
    token: Option<(String, u64)>,
    refused_until: u64,
}

pub struct Wns {
    credentials: Credentials,
    token_url: String,
    http: reqwest::Client,
    bearer: Mutex<Bearer>,
    /// Tests send to a local WNS stand-in instead of the channel's host.
    #[cfg(test)]
    channel_base: Option<String>,
}

impl Wns {
    pub fn from_env() -> Option<Arc<Self>> {
        let Some(credentials) = Credentials::from_env() else {
            tracing::info!("WNS not configured — Windows calls ring only while the app runs");
            return None;
        };
        let token_url = format!("{AUTHORITY}/{}/oauth2/v2.0/token", credentials.tenant);
        let wns = Self::new(credentials, token_url)?;
        tracing::info!(
            client_id = %wns.credentials.client_id,
            "WNS configured (Microsoft identity platform)"
        );
        Some(Arc::new(wns))
    }

    fn new(credentials: Credentials, token_url: String) -> Option<Self> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .build()
            .ok()?;
        Some(Self {
            credentials,
            token_url,
            http,
            bearer: Mutex::new(Bearer {
                token: None,
                refused_until: 0,
            }),
            #[cfg(test)]
            channel_base: None,
        })
    }

    /// A bearer token for WNS: the cached one while it has more than [`TOKEN_MARGIN_SECS`] left,
    /// otherwise a new one. `fresh` discards the cache (WNS said the token is no longer good).
    /// One request at a time: concurrent rings wait for the same token.
    async fn access_token(&self, fresh: bool) -> Option<String> {
        let mut held = self.bearer.lock().await;
        let now = crate::now_unix();
        if fresh {
            held.token = None;
        }
        if let Some((token, expires)) = held.token.as_ref() {
            if *expires > now + TOKEN_MARGIN_SECS {
                return Some(token.clone());
            }
        }
        if held.refused_until > now {
            return None;
        }
        let response = self
            .http
            .post(&self.token_url)
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", self.credentials.client_id.as_str()),
                ("client_secret", self.credentials.secret.as_str()),
                ("scope", SCOPE),
            ])
            .send()
            .await;
        let response = match response {
            Ok(r) => r,
            Err(_) => {
                tracing::warn!("WNS token endpoint unreachable");
                return None;
            }
        };
        let status = response.status().as_u16();
        if !response.status().is_success() {
            // Entra's error code (for example AADSTS7000215, a wrong secret) says what to fix.
            // The description can echo request values, so only the code is logged.
            #[derive(Deserialize, Default)]
            struct Refusal {
                #[serde(default)]
                error: String,
                #[serde(default)]
                error_codes: Vec<u64>,
            }
            let refusal: Refusal = response.json().await.unwrap_or_default();
            tracing::warn!(
                status,
                error = %refusal.error,
                code = refusal.error_codes.first().copied().unwrap_or(0),
                "WNS authentication refused"
            );
            held.refused_until = now + REFUSAL_BACKOFF_SECS;
            return None;
        }
        #[derive(Deserialize)]
        struct Token {
            access_token: String,
            #[serde(default)]
            token_type: String,
            #[serde(default, deserialize_with = "seconds")]
            expires_in: u64,
        }
        let Ok(token) = response.json::<Token>().await else {
            tracing::warn!("WNS token response unreadable");
            return None;
        };
        if token.access_token.is_empty()
            || !(token.token_type.is_empty() || token.token_type.eq_ignore_ascii_case("bearer"))
        {
            tracing::warn!("WNS token response without a bearer token");
            return None;
        }
        let lifetime = if token.expires_in == 0 {
            3600
        } else {
            token.expires_in.min(86_400)
        };
        held.token = Some((token.access_token.clone(), now + lifetime));
        held.refused_until = 0;
        Some(token.access_token)
    }

    fn target(&self, channel: &str) -> String {
        #[cfg(test)]
        if let Some(base) = &self.channel_base {
            let url = reqwest::Url::parse(channel).expect("channel");
            return format!(
                "{base}{}{}",
                url.path(),
                url.query().map(|q| format!("?{q}")).unwrap_or_default()
            );
        }
        channel.to_string()
    }

    /// Show a toast on one channel. Returns `true` when the channel is gone for good.
    pub async fn toast(&self, channel: &str, xml: String, tag: &str, ttl: u64) -> bool {
        self.post(
            channel,
            &[
                ("content-type", "text/xml".to_string()),
                ("x-wns-type", "wns/toast".to_string()),
                ("x-wns-tag", tag.to_string()),
                ("x-wns-group", CALL_GROUP.to_string()),
                ("x-wns-ttl", ttl.max(1).to_string()),
            ],
            xml.into_bytes(),
        )
        .await
    }

    /// A raw notification for the app itself (a ring that ended: the app removes its toast).
    /// Not cached for an offline PC: a late one has nothing left to do. Returns `true` when the
    /// channel is gone for good.
    ///
    /// Desktop channels cannot have a toast removed from the server: WNS answers an `X-WNS-Match`
    /// DELETE with 400 "Invalid filter value" (Microsoft documents it for Windows Phone only).
    pub async fn raw(&self, channel: &str, payload: Vec<u8>, ttl: u64) -> bool {
        self.post(
            channel,
            &[
                ("content-type", "application/octet-stream".to_string()),
                ("x-wns-type", "wns/raw".to_string()),
                ("x-wns-cache-policy", "no-cache".to_string()),
                ("x-wns-ttl", ttl.max(1).to_string()),
            ],
            payload,
        )
        .await
    }

    async fn post(&self, channel: &str, headers: &[(&str, String)], body: Vec<u8>) -> bool {
        if !valid_channel_uri(channel) {
            return true;
        }
        for attempt in 0..2 {
            let Some(token) = self.access_token(attempt == 1).await else {
                return false;
            };
            let mut request = self.http.post(self.target(channel)).bearer_auth(token);
            for (name, value) in headers {
                request = request.header(*name, value);
            }
            match request.body(body.clone()).send().await {
                Ok(r) if r.status().is_success() => return false,
                Ok(r) if r.status().as_u16() == 401 && attempt == 0 => continue,
                Ok(r) => return channel_gone(&r),
                Err(_) => {
                    tracing::warn!("WNS unreachable");
                    return false;
                }
            }
        }
        false
    }
}

/// `expires_in` is a number from Entra's v2 endpoint and a string in some of Microsoft's samples.
fn seconds<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Value {
        Number(u64),
        Text(String),
    }
    Ok(match Value::deserialize(d)? {
        Value::Number(n) => n,
        Value::Text(s) => s.trim().parse().unwrap_or(0),
    })
}

fn is_guid(value: &str) -> bool {
    let parts: Vec<&str> = value.split('-').collect();
    parts.len() == 5
        && parts.iter().map(|p| p.len()).eq([8, 4, 4, 4, 12])
        && parts
            .iter()
            .all(|p| p.bytes().all(|c| c.is_ascii_hexdigit()))
}

/// A directory (tenant) ID, or a verified domain of the tenant. It becomes a URL path segment.
fn valid_tenant(value: &str) -> bool {
    is_guid(value)
        || (value.len() <= 253
            && value.contains('.')
            && value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-'))
}

/// 404 and 410 mean the channel expired or was never valid; anything else is not the channel's
/// fault. WNS says why in `X-WNS-Error-Description`, which carries no personal data.
fn channel_gone(response: &reqwest::Response) -> bool {
    let status = response.status().as_u16();
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .chars()
            .take(120)
            .collect::<String>()
    };
    if matches!(status, 404 | 410) {
        tracing::info!(status, "WNS channel retired");
        true
    } else {
        tracing::warn!(
            status,
            wns_status = %header("x-wns-status"),
            description = %header("x-wns-error-description"),
            "WNS refused a notification"
        );
        false
    }
}

/// The toast tag for a ring: WNS allows 16 characters, and a room id is longer.
pub fn tag_for(room: &str) -> String {
    room.chars().take(TAG_CHARS).collect()
}

/// XML text and attribute escaping. Every value that reaches a toast passes through here.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

fn non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_microsoft_channels_are_accepted() {
        assert!(valid_channel_uri(
            "https://db5p.notify.windows.com/?token=AwYAAAB%2bQ%3d"
        ));
        assert!(valid_channel_uri(
            "https://wns2-db5p.notify.windows.com/w/?token=abc"
        ));
        for bad in [
            "",
            "http://db5p.notify.windows.com/?token=a",
            "https://notify.windows.com/?token=a",
            "https://db5p.notify.windows.com.evil.example/?token=a",
            "https://evil.example/?x=.notify.windows.com",
            "https://user@db5p.notify.windows.com/?token=a",
            "https://db5p.notify.windows.com:8443/?token=a",
            "https://127.0.0.1/?token=a",
            "javascript:alert(1)",
        ] {
            assert!(!valid_channel_uri(bad), "{bad}");
        }
        assert!(!valid_channel_uri(&format!(
            "https://db5p.notify.windows.com/?token={}",
            "a".repeat(2048)
        )));
    }

    #[test]
    fn escaping_leaves_no_markup() {
        assert_eq!(
            escape(r#"<a href="x">Tom & 'Jerry'</a>"#),
            "&lt;a href=&quot;x&quot;&gt;Tom &amp; &apos;Jerry&apos;&lt;/a&gt;"
        );
        assert_eq!(escape("a\u{0}b\u{1b}c"), "abc");
    }

    #[test]
    fn tags_fit_in_sixteen_characters() {
        assert_eq!(tag_for("abcdefghijklmnopqrstuv"), "abcdefghijklmnop");
        assert_eq!(tag_for("short"), "short");
    }

    // ---- Token endpoint and WNS stand-ins ------------------------------------------------------

    use axum::extract::{Form, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::post;
    use axum::Router;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    type Pushed = (String, HashMap<String, String>, String);

    #[derive(Default)]
    struct Mock {
        token_calls: AtomicUsize,
        token_forms: StdMutex<Vec<HashMap<String, String>>>,
        /// Tokens handed out, in order; the WNS stand-in accepts only `accepted`.
        issued: AtomicUsize,
        accepted: StdMutex<String>,
        refuse_token: std::sync::atomic::AtomicBool,
        expires_in: StdMutex<serde_json::Value>,
        pushes: StdMutex<Vec<Pushed>>,
        /// Status for the next WNS request when the token is good.
        wns_status: StdMutex<u16>,
    }

    async fn token(
        State(mock): State<Arc<Mock>>,
        Form(form): Form<HashMap<String, String>>,
    ) -> axum::response::Response {
        mock.token_calls.fetch_add(1, Ordering::SeqCst);
        mock.token_forms.lock().unwrap().push(form);
        if mock.refuse_token.load(Ordering::SeqCst) {
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(serde_json::json!({
                    "error": "invalid_client",
                    "error_description": "AADSTS7000215: Invalid client secret provided.",
                    "error_codes": [7000215]
                })),
            )
                .into_response();
        }
        let n = mock.issued.fetch_add(1, Ordering::SeqCst) + 1;
        let token = format!("token-{n}");
        *mock.accepted.lock().unwrap() = token.clone();
        axum::Json(serde_json::json!({
            "token_type": "Bearer",
            "expires_in": mock.expires_in.lock().unwrap().clone(),
            "ext_expires_in": 3599,
            "access_token": token
        }))
        .into_response()
    }

    async fn push(
        State(mock): State<Arc<Mock>>,
        method: axum::http::Method,
        headers: HeaderMap,
        body: String,
    ) -> axum::response::Response {
        let headers: HashMap<String, String> = headers
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let good = headers.get("authorization").map(String::as_str)
            == Some(&format!("Bearer {}", mock.accepted.lock().unwrap()));
        mock.pushes
            .lock()
            .unwrap()
            .push((method.to_string(), headers, body));
        if !good {
            return (
                StatusCode::UNAUTHORIZED,
                [("x-wns-error-description", "Token expired")],
            )
                .into_response();
        }
        let status = *mock.wns_status.lock().unwrap();
        StatusCode::from_u16(status).unwrap().into_response()
    }

    async fn serve() -> (Arc<Mock>, Wns) {
        let mock = Arc::new(Mock::default());
        *mock.expires_in.lock().unwrap() = serde_json::json!(3599);
        *mock.wns_status.lock().unwrap() = 200;
        let app = Router::new()
            .route("/tenant-guid/oauth2/v2.0/token", post(token))
            .route("/w/", post(push))
            .with_state(mock.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut wns = Wns::new(
            Credentials {
                tenant: "tenant-guid".into(),
                client_id: "6b717f08-17c7-4433-8ea8-25f06ed63e60".into(),
                secret: "s3cret~value".into(),
            },
            format!("http://{addr}/tenant-guid/oauth2/v2.0/token"),
        )
        .unwrap();
        wns.channel_base = Some(format!("http://{addr}"));
        (mock, wns)
    }

    const CHANNEL: &str = "https://wns2-db5p.notify.windows.com/w/?token=AwYAAABq";

    #[tokio::test]
    async fn client_credentials_request_follows_the_identity_platform() {
        let (mock, wns) = serve().await;
        assert!(!wns.toast(CHANNEL, "<toast/>".into(), "room-tag", 45).await);
        let forms = mock.token_forms.lock().unwrap().clone();
        assert_eq!(forms.len(), 1);
        let form = &forms[0];
        assert_eq!(form["grant_type"], "client_credentials");
        assert_eq!(form["client_id"], "6b717f08-17c7-4433-8ea8-25f06ed63e60");
        assert_eq!(form["client_secret"], "s3cret~value");
        assert_eq!(form["scope"], "https://wns.windows.com/.default");
    }

    #[tokio::test]
    async fn toast_carries_the_wns_headers_and_body() {
        let (mock, wns) = serve().await;
        assert!(
            !wns.toast(CHANNEL, "<toast>x</toast>".into(), "AbCdEfGhIjKlMnOp", 40)
                .await
        );
        let pushes = mock.pushes.lock().unwrap().clone();
        assert_eq!(pushes.len(), 1);
        let (method, headers, body) = &pushes[0];
        assert_eq!(method, "POST");
        assert_eq!(headers["authorization"], "Bearer token-1");
        assert_eq!(headers["content-type"], "text/xml");
        assert_eq!(headers["x-wns-type"], "wns/toast");
        assert_eq!(headers["x-wns-tag"], "AbCdEfGhIjKlMnOp");
        assert_eq!(headers["x-wns-group"], "calls");
        assert_eq!(headers["x-wns-ttl"], "40");
        assert_eq!(body, "<toast>x</toast>");
    }

    #[tokio::test]
    async fn tokens_are_cached_until_shortly_before_expiry() {
        let (mock, wns) = serve().await;
        for _ in 0..3 {
            assert!(!wns.toast(CHANNEL, "<toast/>".into(), "t", 45).await);
        }
        assert!(!wns.raw(CHANNEL, b"{}".to_vec(), 60).await);
        assert_eq!(
            mock.token_calls.load(Ordering::SeqCst),
            1,
            "one token for four requests"
        );

        // A token inside the safety margin is replaced before use. Entra's samples send the
        // lifetime as a string; both forms are read.
        *mock.expires_in.lock().unwrap() = serde_json::json!("200");
        assert!(wns.access_token(true).await.is_some());
        let before = mock.token_calls.load(Ordering::SeqCst);
        assert!(!wns.toast(CHANNEL, "<toast/>".into(), "t", 45).await);
        assert_eq!(
            mock.token_calls.load(Ordering::SeqCst),
            before + 1,
            "a 200-second token is inside the 300-second margin"
        );
    }

    #[tokio::test]
    async fn concurrent_rings_share_one_token_request() {
        let (mock, wns) = serve().await;
        let wns = Arc::new(wns);
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let wns = wns.clone();
                tokio::spawn(async move { wns.toast(CHANNEL, "<toast/>".into(), "t", 45).await })
            })
            .collect();
        for t in tasks {
            assert!(!t.await.unwrap());
        }
        assert_eq!(mock.token_calls.load(Ordering::SeqCst), 1);
        assert_eq!(mock.pushes.lock().unwrap().len(), 8);
    }

    #[tokio::test]
    async fn a_rejected_token_is_renewed_once() {
        let (mock, wns) = serve().await;
        assert!(!wns.toast(CHANNEL, "<toast/>".into(), "t", 45).await);
        // WNS stops accepting the cached token (revoked or expired early).
        *mock.accepted.lock().unwrap() = "something-else".into();
        mock.issued.store(1, Ordering::SeqCst);
        assert!(!wns.toast(CHANNEL, "<toast/>".into(), "t", 45).await);
        let pushes = mock.pushes.lock().unwrap().clone();
        assert_eq!(pushes.len(), 3, "first push, refused push, retried push");
        assert_eq!(pushes[2].1["authorization"], "Bearer token-2");
        assert_eq!(mock.token_calls.load(Ordering::SeqCst), 2);

        // The same for a raw notification.
        *mock.accepted.lock().unwrap() = "rotated".into();
        assert!(
            !wns.raw(CHANNEL, b"{\"type\":\"call_cancel\"}".to_vec(), 60)
                .await
        );
        let pushes = mock.pushes.lock().unwrap().clone();
        let (method, headers, body) = pushes.last().unwrap();
        assert_eq!(method, "POST");
        assert_eq!(headers["authorization"], "Bearer token-3");
        assert_eq!(headers["x-wns-type"], "wns/raw");
        assert_eq!(headers["content-type"], "application/octet-stream");
        assert_eq!(headers["x-wns-cache-policy"], "no-cache");
        assert_eq!(headers["x-wns-ttl"], "60");
        assert!(!headers.contains_key("x-wns-match"));
        assert_eq!(body, "{\"type\":\"call_cancel\"}");
    }

    #[tokio::test]
    async fn expired_channels_are_retired_and_other_errors_are_not() {
        let (mock, wns) = serve().await;
        *mock.wns_status.lock().unwrap() = 410;
        assert!(wns.toast(CHANNEL, "<toast/>".into(), "t", 45).await);
        *mock.wns_status.lock().unwrap() = 404;
        assert!(wns.raw(CHANNEL, b"{}".to_vec(), 60).await);
        for status in [400, 403, 406, 413, 500, 503] {
            *mock.wns_status.lock().unwrap() = status;
            assert!(
                !wns.toast(CHANNEL, "<toast/>".into(), "t", 45).await,
                "{status}"
            );
        }
        // A channel that is not Microsoft's is never contacted.
        let before = mock.pushes.lock().unwrap().len();
        assert!(
            wns.toast(
                "https://evil.example/w/?token=a",
                "<toast/>".into(),
                "t",
                45
            )
            .await
        );
        assert_eq!(mock.pushes.lock().unwrap().len(), before);
    }

    #[tokio::test]
    async fn refused_credentials_back_off_and_send_nothing() {
        let (mock, wns) = serve().await;
        mock.refuse_token.store(true, Ordering::SeqCst);
        assert!(!wns.toast(CHANNEL, "<toast/>".into(), "t", 45).await);
        assert!(!wns.toast(CHANNEL, "<toast/>".into(), "t", 45).await);
        assert!(!wns.raw(CHANNEL, b"{}".to_vec(), 60).await);
        assert_eq!(
            mock.token_calls.load(Ordering::SeqCst),
            1,
            "a refusal is not retried for every ring"
        );
        assert!(mock.pushes.lock().unwrap().is_empty());
    }

    #[test]
    fn identifiers_are_validated() {
        assert!(is_guid("48477bf1-ca44-4687-a97c-3d39d7ef7572"));
        assert!(!is_guid("48477bf1ca444687a97c3d39d7ef7572"));
        assert!(!is_guid("S-1-15-2-4171558016-4184177771"));
        assert!(!is_guid("48477bf1-ca44-4687-a97c-3d39d7ef757g"));
        assert!(valid_tenant("48477bf1-ca44-4687-a97c-3d39d7ef7572"));
        assert!(valid_tenant("wodonetwork.onmicrosoft.com"));
        for bad in ["", "common/../x", "a b.com", "consumers", "x/y.com"] {
            assert!(!valid_tenant(bad), "{bad}");
        }
    }
}
