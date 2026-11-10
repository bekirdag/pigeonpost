//! Marketplace M2b deployment settings: staging mode, the hosted test pool and the git host
//! allowlist (build plan R-62, R-70, OD-9).
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `POSTBOX_STAGING_MODE` | off | `1`: this postbox is a staging postbox. Every response carries `x-pigeonpost-environment: staging`, every route that applies a real payment is refused, and the hosted test Nest routes exist. |
//! | `POSTBOX_MARKETPLACE_TEST_NESTS_URL`, `…_TOKEN` | unset | the test pool's nestd (staging only; never the production one) |
//! | `POSTBOX_MARKETPLACE_TEST_NEST_DAYS` | 7 | how long a test Nest lives (1-30) |
//! | `POSTBOX_MARKETPLACE_GIT_HOSTS` | `github.com,gitlab.com,codeberg.org,bitbucket.org` | hosts a build's source may come from |
//!
//! Hosts are configuration: the same binary is a production or a staging postbox depending on
//! these values alone, and a staging postbox has its own domain, realm client, database and vault
//! key (`deploy/staging/README.md`).

use crate::groups::GErr;
use crate::AppState;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use std::sync::Arc;

/// The response header every staging response carries.
pub(crate) const STAGING_HEADER: &str = "x-pigeonpost-environment";
const DEFAULT_GIT_HOSTS: &[&str] = &["github.com", "gitlab.com", "codeberg.org", "bitbucket.org"];
const DEFAULT_TEST_DAYS: u64 = 7;

pub(crate) struct MpEnv {
    /// `POSTBOX_STAGING_MODE=1`.
    pub(crate) staging: bool,
    /// The test pool's nestd: staging only, and never the production pool's URL.
    pub(crate) test_pool: Option<Arc<crate::nests::Runtime>>,
    /// How long a new test Nest lives, in seconds.
    pub(crate) test_nest_secs: u64,
    /// Lower-case host names a build's source may come from.
    pub(crate) git_hosts: Vec<String>,
}

impl Default for MpEnv {
    fn default() -> Self {
        MpEnv {
            staging: false,
            test_pool: None,
            test_nest_secs: DEFAULT_TEST_DAYS * 86_400,
            git_hosts: DEFAULT_GIT_HOSTS.iter().map(|h| h.to_string()).collect(),
        }
    }
}

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_default().trim().to_string()
}

impl MpEnv {
    /// From the environment. `production_url` is the production nestd's URL, which the test pool
    /// may never be.
    pub(crate) fn from_env(production_url: &str) -> Self {
        let staging = matches!(var("POSTBOX_STAGING_MODE").as_str(), "1" | "true" | "yes");
        let days = var("POSTBOX_MARKETPLACE_TEST_NEST_DAYS")
            .parse::<u64>()
            .unwrap_or(DEFAULT_TEST_DAYS)
            .clamp(1, 30);
        let hosts = var("POSTBOX_MARKETPLACE_GIT_HOSTS");
        let git_hosts = if hosts.is_empty() {
            DEFAULT_GIT_HOSTS.iter().map(|h| h.to_string()).collect()
        } else {
            hosts
                .split(',')
                .map(|h| h.trim().to_ascii_lowercase())
                .filter(|h| host_ok(h))
                .collect()
        };
        let url = var("POSTBOX_MARKETPLACE_TEST_NESTS_URL")
            .trim_end_matches('/')
            .to_string();
        let token = var("POSTBOX_MARKETPLACE_TEST_NESTS_TOKEN");
        let test_pool = if url.is_empty() && token.is_empty() {
            None
        } else if !staging {
            tracing::warn!(
                "a test Nest pool is configured but this is not a staging postbox; ignored"
            );
            None
        } else if !production_url.is_empty() && url == production_url.trim_end_matches('/') {
            tracing::error!("the test Nest pool is the production pool; test Nests disabled");
            None
        } else {
            let transport = crate::nests::http_transport(
                &url,
                &token,
                "POSTBOX_MARKETPLACE_TEST_NESTS_URL/POSTBOX_MARKETPLACE_TEST_NESTS_TOKEN",
            );
            matches!(transport, crate::nests::Transport::Http { .. }).then(|| {
                Arc::new(crate::nests::Runtime {
                    transport,
                    ..crate::nests::Runtime::default()
                })
            })
        };
        if staging {
            tracing::warn!(
                test_pool = test_pool.is_some(),
                "STAGING postbox: real payments are refused and every response is marked"
            );
        }
        MpEnv {
            staging,
            test_pool,
            test_nest_secs: days * 86_400,
            git_hosts,
        }
    }

    /// Whether `host` (any case) is on the allowlist.
    pub(crate) fn git_host_allowed(&self, host: &str) -> bool {
        let h = host.to_ascii_lowercase();
        self.git_hosts.contains(&h)
    }
}

/// A plain DNS name: labels of letters, digits and hyphens, at least one dot, no port.
pub(crate) fn host_ok(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && h.contains('.')
        && h.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// Routes that apply a real payment or its result: refused on a staging postbox. Reads stay open.
pub(crate) fn applies_payment(method: &Method, path: &str) -> bool {
    if *method == Method::GET || *method == Method::HEAD || *method == Method::OPTIONS {
        return false;
    }
    let seg: Vec<&str> = path.trim_matches('/').split('/').collect();
    matches!(
        seg.as_slice(),
        ["v1", "claims", "plan"]
            | ["v1", "claims", "apple"]
            | ["v1", "claims", "google"]
            | ["v1", "claims", "google", "assign"]
            | ["v1", "nest-purchases", ..]
            | ["internal", "v1", "entitlements", ..]
            | ["internal", "v1", "accounts", _, "cloud-plan"]
            | ["internal", "v1", "orgs", _, "entitlement"]
    )
}

/// Middleware on the public and internal routers. Off: does nothing. Staging: refuses payment
/// routes with `403 staging_no_payments` and marks every response.
pub(crate) async fn staging_layer(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    if !state.nests.mp.staging {
        return next.run(req).await;
    }
    let mut resp = if applies_payment(req.method(), req.uri().path()) {
        crate::marketplace::into_response::<()>(Err(GErr::new(
            StatusCode::FORBIDDEN,
            "staging_no_payments",
            "this is a staging postbox: no real payment is taken or applied here",
        )))
    } else {
        next.run(req).await
    };
    resp.headers_mut()
        .insert(STAGING_HEADER, HeaderValue::from_static("staging"));
    resp
}

/// `404 staging_only` unless this is a staging postbox: the hosted test environment does not
/// exist anywhere else.
pub(crate) fn staging_only(state: &AppState) -> Result<(), GErr> {
    if state.nests.mp.staging {
        Ok(())
    } else {
        Err(GErr::new(
            StatusCode::NOT_FOUND,
            "staging_only",
            "hosted test Nests exist on a staging postbox only",
        ))
    }
}

/// The test pool, or `503 test_pool_unavailable`.
pub(crate) fn test_pool(state: &AppState) -> Result<Arc<crate::nests::Runtime>, GErr> {
    state.nests.mp.test_pool.clone().ok_or_else(|| {
        GErr::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "test_pool_unavailable",
            "no test Nest pool is configured on this staging postbox",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payment_routes_are_recognised_and_reads_are_not() {
        for (m, p) in [
            (Method::POST, "/v1/claims/plan"),
            (Method::POST, "/v1/claims/apple"),
            (Method::POST, "/v1/claims/google"),
            (Method::POST, "/v1/claims/google/assign"),
            (Method::POST, "/v1/nest-purchases/intents"),
            (Method::PUT, "/internal/v1/entitlements/iyzico/live/ref-1"),
            (Method::PUT, "/internal/v1/accounts/acct_1/cloud-plan"),
            (Method::PUT, "/internal/v1/orgs/org_1/entitlement"),
        ] {
            assert!(applies_payment(&m, p), "{m} {p}");
        }
        for (m, p) in [
            (Method::GET, "/v1/claims/google"),
            (Method::GET, "/v1/claims/apple"),
            (Method::POST, "/v1/nest-payment-authorizations"),
            (Method::POST, "/v1/claims/address"),
            (Method::PUT, "/internal/v1/orgs/org_1/activate"),
            (Method::GET, "/internal/v1/accounts/acct_1/entitlements"),
        ] {
            assert!(!applies_payment(&m, p), "{m} {p}");
        }
    }

    #[test]
    fn git_hosts_are_plain_names_on_the_list() {
        let env = MpEnv::default();
        assert!(env.git_host_allowed("GitHub.com"));
        assert!(!env.git_host_allowed("github.com.evil.example"));
        assert!(host_ok("git.example.org"));
        for bad in [
            "",
            "localhost",
            "a..b",
            "-a.b",
            "a.b:22",
            "user@a.b",
            "10.0.0.1:80",
        ] {
            assert!(!host_ok(bad), "{bad}");
        }
    }
}
