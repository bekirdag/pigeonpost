//! Explicit member consent for account deletion, without a support-message prerequisite.

use crate::{bearer, now_unix, rand_hex, ApiError, AppState};
use axum::{
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Consent {
    confirm: bool,
}

async fn member(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(String, crate::oidc::Claims), ApiError> {
    let token = bearer(headers)
        .filter(|t| t.starts_with("eyJ"))
        .ok_or_else(|| ApiError::unauthorized("sign in to request account deletion"))?;
    let claims = state
        .oidc
        .validate(token)
        .await
        .map_err(|_| ApiError::unauthorized("invalid member token"))?;
    let account = state
        .store
        .account_for_sub(
            claims.sub.clone(),
            format!("acct_{}", rand_hex(12)),
            now_unix(),
        )
        .await
        .map_err(|_| ApiError::server("store_error"))?;
    Ok((account, claims))
}

fn response(value: serde_json::Value) -> Response {
    (
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        Json(value),
    )
        .into_response()
}

pub(crate) async fn status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let (account, claims) = match member(&state, &headers).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let label = claims.address().unwrap_or(&account).to_owned();
    match state.store.account_deletion_request(account).await {
        Ok(request) => response(json!({"request": request, "account": {"label": label}})),
        Err(_) => ApiError::server("store_error").into_response(),
    }
}

pub(crate) async fn request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(consent): Json<Consent>,
) -> Response {
    let (account, claims) = match member(&state, &headers).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    if !consent.confirm {
        return ApiError::bad(
            "confirmation_required",
            "confirm that you want your account and associated data deleted",
        )
        .into_response();
    }
    let contact = claims
        .address()
        .filter(|_| claims.address_is_verified())
        .map(str::to_owned);
    match state
        .store
        .request_account_deletion(account, claims.sub, contact, now_unix())
        .await
    {
        Ok(request) => {
            tracing::warn!(request_id = %request.request_id, complete_by = request.complete_by, "account deletion request awaiting fulfillment");
            response(json!({"request": request}))
        }
        Err(_) => ApiError::server("store_error").into_response(),
    }
}
