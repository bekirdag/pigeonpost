//! The member half of web plans (SDS §8.5): a signed-in person registers names within the
//! capacity their web (iyzico) or complimentary plan grants, like the App Store plans already work.
use crate::principal::require_human;
use crate::store::WebClaim;
use crate::{
    bearer, ensure_namespace_mailbox, now_unix, principal_for_token, ApiError, AppState, Principal,
    OPEN_NAMESPACES, PROVIDER_NAMESPACES,
};
use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NameRefusal {
    Invalid,
    Reserved,
    /// The reserved-name list is not loaded, so nothing can be sold safely.
    Unavailable,
}

/// The canonical namespace for `raw`, refusing reserved, provider and open namespaces. The same
/// rules as `claim_apple` and handle availability, so the spellings cannot be walked around.
pub(crate) fn canonical_namespace(state: &AppState, raw: &str) -> Result<String, NameRefusal> {
    let normalized = format!("/{}", raw.trim().trim_matches('/'));
    let name = pigeonpost_core::address::namespace_root(&normalized).ok_or(NameRefusal::Invalid)?;
    let name = name.trim_start_matches('/').to_owned();
    if name.is_empty() || name.contains('/') || name.contains('@') {
        return Err(NameRefusal::Invalid);
    }
    let reserved = state
        .reserved_names
        .as_ref()
        .ok_or(NameRefusal::Unavailable)?;
    if reserved.contains(&name)
        || PROVIDER_NAMESPACES.contains(&name.as_str())
        || OPEN_NAMESPACES.contains(&name.as_str())
    {
        return Err(NameRefusal::Reserved);
    }
    Ok(name)
}

/// The account behind a person's own interactive sign-in. Automation cannot buy or bind names.
async fn member_account(state: &AppState, headers: &HeaderMap) -> Result<String, Response> {
    let Some(token) = bearer(headers) else {
        return Err(ApiError::unauthorized("this endpoint needs a member token").into_response());
    };
    if !token.starts_with("eyJ") {
        return Err(ApiError::bad(
            "member_token_required",
            "this needs the account's own sign-in, not a key or capability token",
        )
        .into_response());
    }
    let principal = principal_for_token(state, Some(token))
        .await
        .map_err(|e| e.into_response())?;
    match &principal {
        Principal::Account(account, _) => {
            require_human(&principal).map_err(IntoResponse::into_response)?;
            Ok(account.clone())
        }
        Principal::Identity(_) => Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "account_required",
            "this needs an account sign-in",
        )
        .into_response()),
    }
}

/// `GET /v1/me/entitlements` — every entitlement of the signed-in account and its web plan.
pub(crate) async fn my_entitlements(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let account = match member_account(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    let now = now_unix();
    match (
        state
            .store
            .entitlements_for("account", account.clone(), now)
            .await,
        state.store.web_plan(account.clone(), now).await,
    ) {
        (Ok(list), Ok(plan)) => (
            [(header::CACHE_CONTROL, "private, no-store")],
            Json(json!({ "account": account, "entitlements": list, "web_plan": plan })),
        )
            .into_response(),
        _ => ApiError::server("store_error").into_response(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaimPlanReq {
    namespace: String,
}

/// `POST /v1/claims/plan` — register a name within the account's web plan.
pub(crate) async fn claim_plan(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<ClaimPlanReq>, JsonRejection>,
) -> Response {
    let account = match member_account(&state, &headers).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Ok(Json(req)) = body else {
        return ApiError::bad("bad_request", "expected {namespace}").into_response();
    };
    let name = match canonical_namespace(&state, &req.namespace) {
        Ok(n) => n,
        Err(NameRefusal::Invalid) => {
            return ApiError::bad(
                "invalid_namespace",
                "Choose a name of 1–32 letters, numbers, dots, underscores or hyphens",
            )
            .into_response()
        }
        Err(NameRefusal::Reserved) => {
            return ApiError::new(
                StatusCode::CONFLICT,
                "name_reserved",
                "That name is reserved",
            )
            .into_response()
        }
        Err(NameRefusal::Unavailable) => {
            return ApiError::server("reserved_names_unavailable").into_response()
        }
    };
    match state
        .store
        .claim_web_name(account.clone(), name.clone(), now_unix())
        .await
    {
        Ok(claim @ (WebClaim::Granted { .. } | WebClaim::AlreadyInPlan { .. })) => {
            let (WebClaim::Granted { capacity, used } | WebClaim::AlreadyInPlan { capacity, used }) =
                claim
            else {
                unreachable!()
            };
            if matches!(claim, WebClaim::Granted { .. }) {
                crate::record_audit(
                    &state,
                    crate::audit::ActorKind::Human,
                    Some(account.clone()),
                    "plan_name.claim",
                    Some(format!("namespace:{name}")),
                    json!({ "capacity": capacity, "used": used }),
                    None,
                    &headers,
                )
                .await;
            }
            let mailbox = ensure_namespace_mailbox(&state, &account, &name).await;
            Json(json!({
                "namespace": format!("/{name}"),
                "mailbox": mailbox,
                "capacity": capacity,
                "used": used,
            }))
            .into_response()
        }
        Ok(WebClaim::NoPlan) => ApiError::new(
            StatusCode::PAYMENT_REQUIRED,
            "no_plan",
            "This account has no active Pigeonpost Cloud plan",
        )
        .into_response(),
        Ok(WebClaim::LimitReached) => ApiError::new(
            StatusCode::CONFLICT,
            "plan_capacity_reached",
            "All names included in this plan are registered. Choose a larger plan to add more.",
        )
        .into_response(),
        Ok(WebClaim::AlreadyHeld) => ApiError::new(
            StatusCode::CONFLICT,
            "handle_already_subscribed",
            "This account already holds that name",
        )
        .into_response(),
        Ok(WebClaim::Taken) => ApiError::new(
            StatusCode::CONFLICT,
            "namespace_taken",
            "That name is already held",
        )
        .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "web plan claim failed");
            ApiError::server("store_error").into_response()
        }
    }
}
