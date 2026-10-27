//! The internal API (`/internal/v1/*`, SDS §3.2, §21.2): narrow endpoints for Pigeonpost's own
//! services, authenticated by per-service bearer tokens.
//!
//! # Exposure
//!
//! These routes are **not** on the public router. They are served only by a second listener bound
//! to `POSTBOX_INTERNAL_BIND` (unset = no internal listener at all). Deployments bind it to
//! loopback or a private interface — in the container, publish it as `127.0.0.1:<port>` only — and
//! never route it through the public reverse proxy. A request for `/internal/…` on the public port
//! is an ordinary 404.
//!
//! # Tokens
//!
//! Each service has its own token in its own environment variable (`POSTBOX_SVC_TOKEN_<NAME>`),
//! mapped here to a service name and a fixed set of endpoints. Tokens are compared by SHA-256 in
//! constant time against every configured service, so neither a match nor a near miss is visible
//! in timing. A token that is not `svc_` followed by at least 32 characters is refused at startup:
//! a short shared secret is how a "private" API becomes a guessable one.
//!
//! `PUT /v1/namespaces` with `NAMESPACE_GRANT_TOKEN` keeps working on the public router for the
//! billing bridge until it moves to the internal entitlement routes.

use crate::audit::ActorKind;
use crate::policy::{authorize, Action, Actor, Facts, Resource};
use crate::store::{FeatureRow, FeatureScope};
use crate::{
    bearer, billing_internal, capabilities, constant_time_eq, err_response, now_unix, sha256,
    ApiError, AppState,
};
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
use serde_json::json;

/// An internal endpoint a service token may be allowed to call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Endpoint {
    Health,
    FeaturesRead,
    FeaturesWrite,
    /// Entitlement upserts and reads (SDS §8.8, §21.2).
    Entitlements,
    /// Org drafts, activation and org entitlements (SDS §8.5, §21.2).
    Orgs,
    /// `POST /internal/v1/call-admission`.
    CallAdmission,
    /// `POST /internal/v1/call-events`.
    CallEvents,
    /// Avatar reports and takedowns.
    ModerateAvatars,
    /// `POST /internal/v1/group-call-admission` (Phase 3a).
    GroupCallAdmission,
    /// `POST /internal/v1/call-ring` and `/call-ring-cancel` (Phase 1c).
    CallRing,
    /// `POST /internal/v1/calendar/meeting-access` (Phase 3B).
    CalendarMeetingAccess,
    /// `POST /internal/v1/stream-admission`, `/stream-check`, `/stream-end` (Phase 6).
    StreamAdmission,
}

/// The services this postbox knows, the variable each one's token lives in, and what each may
/// call. Later phases add endpoints (call events, admission, entitlements) to the right service
/// here, and nowhere else.
const SERVICES: &[(&str, &str, &[Endpoint])] = &[
    (
        "calls",
        "POSTBOX_SVC_TOKEN_CALLS",
        &[
            Endpoint::Health,
            Endpoint::CallAdmission,
            Endpoint::CallEvents,
            Endpoint::GroupCallAdmission,
            Endpoint::CallRing,
            Endpoint::CalendarMeetingAccess,
            Endpoint::StreamAdmission,
        ],
    ),
    (
        "billing",
        "POSTBOX_SVC_TOKEN_BILLING",
        &[Endpoint::Health, Endpoint::Entitlements, Endpoint::Orgs],
    ),
    (
        "meeting_intel",
        "POSTBOX_SVC_TOKEN_MEETING_INTEL",
        &[Endpoint::Health],
    ),
    (
        "ops",
        "POSTBOX_SVC_TOKEN_OPS",
        &[
            Endpoint::Health,
            Endpoint::FeaturesRead,
            Endpoint::FeaturesWrite,
            Endpoint::ModerateAvatars,
        ],
    ),
];

/// Shortest acceptable token body after `svc_`.
const MIN_TOKEN_CHARS: usize = 32;

struct ServiceEntry {
    name: &'static str,
    hash: [u8; 32],
    allowed: &'static [Endpoint],
}

/// The configured service tokens.
#[derive(Default)]
pub(crate) struct ServiceTokens {
    entries: Vec<ServiceEntry>,
}

impl ServiceTokens {
    pub(crate) fn from_env() -> Self {
        Self::from_lookup(|var| std::env::var(var).ok())
    }

    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let mut entries = Vec::new();
        for (name, var, allowed) in SERVICES {
            let Some(token) = lookup(var).map(|t| t.trim().to_string()) else {
                continue;
            };
            if token.is_empty() {
                continue;
            }
            if !well_formed(&token) {
                tracing::error!(
                    service = name,
                    var,
                    "service token ignored: must be svc_ followed by at least 32 characters"
                );
                continue;
            }
            entries.push(ServiceEntry {
                name,
                hash: sha256(token.as_bytes()),
                allowed,
            });
        }
        if !entries.is_empty() {
            tracing::info!(
                services = ?entries.iter().map(|e| e.name).collect::<Vec<_>>(),
                "internal service tokens loaded"
            );
        }
        ServiceTokens { entries }
    }

    /// The service a presented token belongs to. Every configured entry is compared, match or
    /// not, so timing does not reveal which (if any) matched.
    fn authenticate(&self, token: &str) -> Option<&ServiceEntry> {
        let presented = sha256(token.as_bytes());
        let mut found = None;
        for entry in &self.entries {
            if constant_time_eq(&presented, &entry.hash) && found.is_none() {
                found = Some(entry);
            }
        }
        found
    }
}

fn well_formed(token: &str) -> bool {
    token.strip_prefix("svc_").is_some_and(|rest| {
        rest.len() >= MIN_TOKEN_CHARS && rest.chars().all(|c| c.is_ascii_graphic())
    })
}

/// Authenticate the caller and check it may call `endpoint`.
fn service_for(
    state: &AppState,
    headers: &HeaderMap,
    endpoint: Endpoint,
) -> Result<&'static str, Box<Response>> {
    let entry = bearer(headers).and_then(|t| state.service_tokens.authenticate(t));
    let Some(entry) = entry else {
        let mut resp = err_response(
            StatusCode::UNAUTHORIZED,
            "invalid_service_token",
            Some("a valid internal service token is required"),
        );
        resp.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer error=\"invalid_token\""),
        );
        return Err(Box::new(resp));
    };
    if !entry.allowed.contains(&endpoint) {
        return Err(Box::new(err_response(
            StatusCode::FORBIDDEN,
            "endpoint_not_allowed",
            Some("this service token may not call this endpoint"),
        )));
    }
    Ok(entry.name)
}

/// Authenticate, check the endpoint grant, then ask the policy.
pub(crate) fn authorize_service(
    state: &AppState,
    headers: &HeaderMap,
    endpoint: Endpoint,
    action: Action,
) -> Result<&'static str, Box<Response>> {
    let service = service_for(state, headers, endpoint)?;
    let facts = Facts {
        now: now_unix(),
        ..Facts::default()
    };
    authorize(
        &Actor::Service(service),
        action,
        &Resource::Platform,
        &facts,
    )
    .map_err(|deny| Box::new(deny.into_response()))?;
    Ok(service)
}

/// The internal router. Mounted only on the internal listener.
pub(crate) fn router(state: AppState) -> Router {
    Router::new()
        .route("/internal/v1/health", get(health))
        .route("/internal/v1/features", get(list_features))
        .route("/internal/v1/features/{name}", put(set_feature))
        .route(
            "/internal/v1/entitlements/{provider}/{environment}/{reference}",
            put(billing_internal::put_entitlement),
        )
        .route(
            "/internal/v1/accounts/{id}/cloud-plan",
            put(billing_internal::put_cloud_plan),
        )
        .route(
            "/internal/v1/accounts/{id}/entitlements",
            get(billing_internal::account_entitlements),
        )
        .route(
            "/internal/v1/org-drafts",
            post(billing_internal::create_org_draft),
        )
        .route("/internal/v1/orgs/{id}", get(billing_internal::get_org))
        .route(
            "/internal/v1/orgs/{id}/activate",
            put(billing_internal::activate_org),
        )
        .route(
            "/internal/v1/orgs/{id}/entitlement",
            put(billing_internal::put_org_entitlement),
        )
        .route(
            "/internal/v1/call-admission",
            post(crate::filtering::call_admission),
        )
        .route("/internal/v1/call-events", post(crate::calllog::ingest))
        .route(
            "/internal/v1/group-call-admission",
            post(crate::mls::group_call_admission),
        )
        .route(
            "/internal/v1/calendar/meeting-access",
            post(crate::calendar::meeting::meeting_access),
        )
        .route(
            "/internal/v1/stream-admission",
            post(crate::streams::admission),
        )
        .route("/internal/v1/stream-check", post(crate::streams::check))
        .route(
            "/internal/v1/stream-end",
            post(crate::streams::end_internal),
        )
        .route("/internal/v1/avatar-reports", get(crate::avatars::reports))
        .route(
            "/internal/v1/avatars/{id}/remove",
            post(crate::avatars::moderate_remove),
        )
        .route("/internal/v1/call-ring", post(crate::call_alerts::ring))
        .route(
            "/internal/v1/call-ring-cancel",
            post(crate::call_alerts::cancel),
        )
        .fallback(|| async { err_response(StatusCode::NOT_FOUND, "not_found", None) })
        .with_state(state)
}

/// `GET /internal/v1/health` — proves a service's token and wiring.
async fn health(State(state): State<AppState>, headers: HeaderMap) -> Response {
    match authorize_service(&state, &headers, Endpoint::Health, Action::ServiceHealth) {
        Ok(service) => Json(json!({
            "status": "ok",
            "service": service,
            "server_version": env!("CARGO_PKG_VERSION"),
        }))
        .into_response(),
        Err(resp) => *resp,
    }
}

/// `GET /internal/v1/features` — the known flags and every stored value.
async fn list_features(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(resp) = authorize_service(
        &state,
        &headers,
        Endpoint::FeaturesRead,
        Action::ReadFeatureFlags,
    ) {
        return *resp;
    }
    let flags: Vec<FeatureRow> = match state.store.list_features().await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!(error = %e, "feature list failed");
            return ApiError::server("store_error").into_response();
        }
    };
    let known: Vec<_> = capabilities::KNOWN_FEATURES
        .iter()
        .map(|(name, description)| json!({ "name": name, "description": description }))
        .collect();
    Json(json!({ "known": known, "flags": flags })).into_response()
}

#[derive(serde::Deserialize)]
struct SetFeatureReq {
    scope: String,
    #[serde(default)]
    account_id: Option<String>,
    /// `null` clears the value at this scope.
    enabled: Option<bool>,
}

/// `PUT /internal/v1/features/{name}` — set or clear one flag, globally or for one account.
async fn set_feature(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Option<Extension<ConnectInfo<std::net::SocketAddr>>>,
    Path(name): Path<String>,
    body: Result<Json<SetFeatureReq>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let service = match authorize_service(
        &state,
        &headers,
        Endpoint::FeaturesWrite,
        Action::WriteFeatureFlags,
    ) {
        Ok(service) => service,
        Err(resp) => return *resp,
    };
    let Ok(Json(req)) = body else {
        return ApiError::bad("bad_request", "expected {scope, account_id?, enabled}")
            .into_response();
    };
    let Some(&(known, _)) = capabilities::KNOWN_FEATURES
        .iter()
        .find(|(n, _)| *n == name)
    else {
        return ApiError::bad("unknown_feature", "no such feature flag").into_response();
    };
    let scope = match (req.scope.as_str(), req.account_id) {
        ("global", None) => FeatureScope::Global,
        ("account", Some(id)) if !id.trim().is_empty() && id.len() <= 128 => {
            FeatureScope::Account(id)
        }
        ("global" | "account", _) => {
            return ApiError::bad(
                "bad_request",
                "account scope needs account_id; global scope must not have one",
            )
            .into_response()
        }
        _ => {
            return ApiError::bad("scope_unsupported", "scope must be global or account")
                .into_response()
        }
    };
    let (scope_name, scope_id) = match &scope {
        FeatureScope::Global => ("global", String::new()),
        FeatureScope::Account(id) => ("account", id.clone()),
    };
    let now = now_unix();
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok());
    let audit_row = state.auditor.row(
        ActorKind::System,
        None,
        "feature.set",
        Some(format!("feature:{known}")),
        json!({
            "service": service,
            "scope": scope_name,
            "scope_id": scope_id,
            "enabled": req.enabled,
        }),
        peer.map(|Extension(ConnectInfo(addr))| addr.ip()),
        user_agent,
        now,
    );
    match state
        .store
        .set_feature(
            known.to_string(),
            scope.clone(),
            req.enabled,
            format!("service:{service}"),
            audit_row,
            now,
        )
        .await
    {
        Ok(()) => {
            if let FeatureScope::Account(account) = &scope {
                state.event_hub.wake(account);
            }
            Json(json!({
                "name": known,
                "scope": scope_name,
                "scope_id": scope_id,
                "enabled": req.enabled,
            }))
            .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "feature set failed");
            ApiError::server("store_error").into_response()
        }
    }
}

#[cfg(test)]
pub(crate) fn tokens_for_test(pairs: &[(&str, &str)]) -> ServiceTokens {
    let pairs: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    ServiceTokens::from_lookup(move |var| {
        pairs.iter().find(|(k, _)| k == var).map(|(_, v)| v.clone())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPS: &str = "svc_ops_0123456789abcdef0123456789abcdef";
    const CALLS: &str = "svc_calls_0123456789abcdef0123456789abcdef";

    #[test]
    fn malformed_tokens_are_refused_at_startup() {
        let tokens = tokens_for_test(&[
            ("POSTBOX_SVC_TOKEN_OPS", "short"),
            ("POSTBOX_SVC_TOKEN_CALLS", "svc_tooshort"),
            (
                "POSTBOX_SVC_TOKEN_BILLING",
                "svc_has a space in it 0123456789abcdef",
            ),
        ]);
        assert!(tokens.entries.is_empty());
    }

    #[test]
    fn tokens_map_to_their_own_service_only() {
        let tokens = tokens_for_test(&[
            ("POSTBOX_SVC_TOKEN_OPS", OPS),
            ("POSTBOX_SVC_TOKEN_CALLS", CALLS),
        ]);
        assert_eq!(tokens.authenticate(OPS).unwrap().name, "ops");
        assert_eq!(tokens.authenticate(CALLS).unwrap().name, "calls");
        assert!(tokens
            .authenticate("svc_ops_0123456789abcdef0123456789abcdeX")
            .is_none());
        assert!(tokens.authenticate("").is_none());
        assert!(!tokens
            .authenticate(CALLS)
            .unwrap()
            .allowed
            .contains(&Endpoint::FeaturesWrite));
    }

    #[test]
    fn the_namespace_grant_secret_is_not_a_service_token() {
        // Unconfigured services authenticate nobody, whatever is presented.
        let tokens = ServiceTokens::default();
        assert!(tokens.authenticate(OPS).is_none());
    }
}
