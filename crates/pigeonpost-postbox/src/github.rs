//! Proving control of a GitHub account, so `/github/<login>` can be a real address.
//!
//! A purchased namespace like `/bekir` has one owner and the registry says who. `/github` cannot
//! work that way — nobody owns it, and every name in it already belongs to somebody who has never
//! heard of Pigeonpost. So the namespace is authorised name by name, and the only thing that
//! authorises a name is proof from GitHub itself.
//!
//! The device flow is what fits an agent: the CLI has no browser, no redirect URI to listen on, and
//! frequently no display at all. It asks for a code, prints it, and the person approves it wherever
//! they are. GitHub then hands over a token that answers exactly one question — who are you — which
//! is the entire scope requested (`read:user`).
//!
//! **The client secret never leaves the postbox.** The CLI knows only what GitHub prints on screen.
//! That keeps the OAuth app's credentials out of every agent's home directory and off the wire to
//! anywhere but GitHub, and it means the CLI needs no configuration at all.

const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const USER_URL: &str = "https://api.github.com/user";

/// Read-only, and only the profile. The postbox wants a login and a stable id, and asking for more
/// would be asking every agent's owner to hand over repository access to receive mail.
const SCOPE: &str = "read:user";

#[derive(Debug, thiserror::Error)]
pub enum GithubError {
    #[error("github could not be reached")]
    Unreachable,
    #[error("github refused: {0}")]
    Refused(String),
    /// The person has not finished approving yet. Not a failure — the caller polls again.
    #[error("authorization_pending")]
    Pending,
    /// Polling faster than GitHub allows; back off by the interval it returned.
    #[error("slow_down")]
    SlowDown,
}

/// What the CLI shows the person, and the handle it polls with.
#[derive(serde::Deserialize, serde::Serialize)]
pub struct DeviceGrant {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// Who GitHub says the approver is.
pub struct GithubUser {
    pub login: String,
    /// GitHub's immutable numeric id. Logins are renameable and, once released, reusable — this is
    /// the only field that identifies the same person across a rename.
    pub user_id: String,
}

#[derive(Clone)]
pub struct Github {
    client_id: String,
    client_secret: Option<String>,
    http: reqwest::Client,
}

impl Github {
    /// Configured only when a client id is present; absent, the endpoints refuse rather than
    /// pretending to work.
    pub fn from_env() -> Option<Self> {
        let client_id = std::env::var("GITHUB_OAUTH_CLIENT_ID").ok()?;
        if client_id.trim().is_empty() {
            return None;
        }
        Some(Github {
            client_id,
            // A GitHub *OAuth app* device flow needs no secret; a GitHub *App* does. Optional so
            // either kind of registration works without a second code path.
            client_secret: std::env::var("GITHUB_OAUTH_CLIENT_SECRET")
                .ok()
                .filter(|s| !s.trim().is_empty()),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .user_agent(concat!("pigeonpost-postbox/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("http client"),
        })
    }

    /// Start a device authorization and return what the person has to type in.
    pub async fn start(&self) -> Result<DeviceGrant, GithubError> {
        let response = self
            .http
            .post(DEVICE_CODE_URL)
            .header("accept", "application/json")
            .form(&[("client_id", self.client_id.as_str()), ("scope", SCOPE)])
            .send()
            .await
            .map_err(|_| GithubError::Unreachable)?;
        let status = response.status();
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|_| GithubError::Unreachable)?;

        // GitHub answers a refused device-code request with HTTP 200 and an `error` field, so the
        // status says nothing. Reading it is what separates "this app has Device Flow switched off"
        // — a one-checkbox setup mistake, and the state a fresh OAuth app is in by default — from a
        // network fault. Reporting the former as "unreachable" sends someone looking at DNS.
        if let Some(error) = body.get("error").and_then(|v| v.as_str()) {
            return Err(GithubError::Refused(error.to_string()));
        }
        if !status.is_success() {
            return Err(GithubError::Refused(status.to_string()));
        }
        serde_json::from_value(body).map_err(|_| GithubError::Unreachable)
    }

    /// Exchange an approved device code for a token, then ask GitHub who approved it.
    ///
    /// The token is used once, here, and never stored. The postbox wants the answer to "who is
    /// this", not standing access to the person's account.
    pub async fn identify(&self, device_code: &str) -> Result<GithubUser, GithubError> {
        let mut form = vec![
            ("client_id", self.client_id.as_str()),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ];
        if let Some(secret) = &self.client_secret {
            form.push(("client_secret", secret.as_str()));
        }
        let response = self
            .http
            .post(ACCESS_TOKEN_URL)
            .header("accept", "application/json")
            .form(&form)
            .send()
            .await
            .map_err(|_| GithubError::Unreachable)?;
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|_| GithubError::Unreachable)?;

        // The device flow reports "not yet" as a 200 with an error field, so status alone says
        // nothing. Pending and slow_down are the normal path, not failures.
        if let Some(error) = body.get("error").and_then(|v| v.as_str()) {
            return Err(match error {
                "authorization_pending" => GithubError::Pending,
                "slow_down" => GithubError::SlowDown,
                other => GithubError::Refused(other.to_string()),
            });
        }
        let token = body
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or(GithubError::Unreachable)?;

        let user: serde_json::Value = self
            .http
            .get(USER_URL)
            .header("accept", "application/vnd.github+json")
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| GithubError::Unreachable)?
            .json()
            .await
            .map_err(|_| GithubError::Unreachable)?;

        let login = user
            .get("login")
            .and_then(|v| v.as_str())
            .ok_or(GithubError::Unreachable)?
            .to_ascii_lowercase();
        let user_id = user
            .get("id")
            .map(|v| v.to_string())
            .ok_or(GithubError::Unreachable)?;
        Ok(GithubUser { login, user_id })
    }
}
