use crate::{
    googleplay::{self, PlayError},
    store::GoogleBinding,
    *,
};
use serde::Deserialize;

async fn member(state: &AppState, headers: &HeaderMap) -> Result<String, ApiError> {
    let token = bearer(headers)
        .ok_or_else(|| ApiError::unauthorized("Sign in to manage handle purchases."))?;
    let claims = state
        .oidc
        .validate(token)
        .await
        .map_err(|_| ApiError::unauthorized("Sign in again."))?;
    state
        .store
        .account_for_sub(claims.sub, format!("acct_{}", rand_hex(12)), now_unix())
        .await
        .map_err(|_| ApiError::server("store_error"))
}

fn provider_error(error: PlayError) -> Response {
    let (status, code) = match error {
        PlayError::AccountMismatch => (StatusCode::FORBIDDEN, "purchase_account_mismatch"),
        PlayError::NotFound => (StatusCode::BAD_REQUEST, "purchase_not_found"),
        PlayError::Invalid => (StatusCode::BAD_REQUEST, "invalid_purchase"),
        PlayError::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "billing_unavailable"),
    };
    err_response(status, code, Some(&error.to_string()))
}

fn namespace(state: &AppState, value: &str) -> Result<String, ApiError> {
    let value = value.trim().trim_matches('/').to_ascii_lowercase();
    if value.contains('/') || value.is_empty() || value.len() > 32 {
        return Err(ApiError::bad(
            "invalid_namespace",
            "Choose a handle with 1–32 letters, numbers, dots, underscores or hyphens.",
        ));
    }
    let parsed = Destination::for_handle(&format!("/{value}/main"))
        .map_err(|_| ApiError::bad("invalid_namespace", "That handle is not valid."))?;
    let name = parsed
        .handle()
        .and_then(|h| h.trim_start_matches('/').split('/').next())
        .unwrap_or("")
        .to_owned();
    if name != value
        || PROVIDER_NAMESPACES.contains(&name.as_str())
        || OPEN_NAMESPACES.contains(&name.as_str())
        || state
            .reserved_names
            .as_ref()
            .is_none_or(|names| names.contains(&name))
    {
        return Err(ApiError::bad(
            "reserved",
            "That handle is reserved. Choose another.",
        ));
    }
    Ok(name)
}

pub async fn catalog(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let account = match member(&state, &headers).await {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let held = match state
        .store
        .google_subscriptions_for(account.clone(), now_unix())
        .await
    {
        Ok(h) => h,
        Err(_) => return ApiError::server("store_error").into_response(),
    };
    Json(
        json!({ "available": state.googleplay.is_some(), "product_ids": googleplay::products(),
        "base_plan_id": googleplay::BASE_PLAN, "max_handles": googleplay::MAX_HANDLES,
        "account_id": googleplay::account_id(&account), "handles": held }),
    )
    .into_response()
}

#[derive(Deserialize)]
pub struct Claim {
    purchase_token: String,
    namespace: Option<String>,
}

pub async fn claim(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<Claim>,
) -> Response {
    let account = match member(&state, &headers).await {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let name = match request.namespace.map(|n| namespace(&state, &n)).transpose() {
        Ok(n) => n,
        Err(e) => return e.into_response(),
    };
    redeem(&state, &account, request.purchase_token, name).await
}

#[derive(Deserialize)]
pub struct Assign {
    product_id: String,
    namespace: String,
}

pub async fn assign(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<Assign>,
) -> Response {
    let account = match member(&state, &headers).await {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let name = match namespace(&state, &request.namespace) {
        Ok(n) => n,
        Err(e) => return e.into_response(),
    };
    let held = match state
        .store
        .google_subscriptions_for(account.clone(), now_unix())
        .await
    {
        Ok(h) => h,
        Err(_) => return ApiError::server("store_error").into_response(),
    };
    let Some(purchase) = held
        .into_iter()
        .find(|p| p.product_id == request.product_id && p.active && p.namespace.is_none())
    else {
        return ApiError::bad(
            "no_unassigned_purchase",
            "Restore purchases to refresh your available handles.",
        )
        .into_response();
    };
    redeem(&state, &account, purchase.purchase_token, Some(name)).await
}

async fn redeem(state: &AppState, account: &str, token: String, name: Option<String>) -> Response {
    let Some(play) = &state.googleplay else {
        return provider_error(PlayError::Unavailable);
    };
    let _guard = play.verification.lock().await;
    let purchase = match play.verify(&token, account).await {
        Ok(p) => p,
        Err(e) => return provider_error(e),
    };
    match state.store.record_google_purchase(account.to_owned(), token.clone(), purchase, name, now_unix()).await {
        Ok(GoogleBinding::Saved(mut saved)) => {
            // Acknowledge only after the paid slot is durable. Both restore and the reconciliation
            // worker retry if Google is temporarily unavailable at this step.
            if saved.active && !saved.acknowledged && play.acknowledge(&token, &saved.product_id).await.is_ok() {
                saved.acknowledged = state.store.acknowledge_google_purchase(token).await.is_ok();
            }
            let mailbox = if saved.active {
                if let Some(name) = &saved.namespace { ensure_namespace_mailbox(state, account, name).await } else { None }
            } else { None };
            Json(json!({ "purchase": saved, "mailbox": mailbox })).into_response()
        }
        Ok(GoogleBinding::AccountMismatch) => provider_error(PlayError::AccountMismatch),
        Ok(GoogleBinding::DuplicateProduct) => err_response(StatusCode::CONFLICT, "duplicate_subscription",
            Some("This handle slot already has a subscription. Manage the duplicate in Google Play; it will not be acknowledged or charged again here.")),
        Ok(GoogleBinding::LimitReached) => err_response(StatusCode::CONFLICT, "handle_limit", Some("You already have ten active Google Play handle subscriptions.")),
        Ok(GoogleBinding::Inactive) => err_response(StatusCode::CONFLICT, "purchase_inactive", Some("Google Play has not confirmed an active payment. Pending payments do not unlock a handle.")),
        Ok(GoogleBinding::Replaced) => err_response(StatusCode::CONFLICT, "purchase_replaced", Some("This subscription was replaced. Restore its current purchase.")),
        Err(_) => ApiError::server("store_error").into_response(),
    }
}

/// Reconcile renewals, grace periods, holds and revocations without requiring the Android app to
/// be open. The last confirmed expiry is the ceiling during an outage, never an invented renewal.
pub fn start_reconciliation(state: AppState) {
    let Some(play) = state.googleplay.clone() else {
        return;
    };
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(std::time::Duration::from_secs(60));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            let due = match state.store.google_due(now_unix()).await {
                Ok(due) => due,
                Err(_) => {
                    tracing::warn!("Google Play reconciliation could not read subscriptions");
                    continue;
                }
            };
            for row in due {
                let _guard = play.verification.lock().await;
                let purchase = match play.verify(&row.purchase_token, &row.account_id).await {
                    Ok(purchase) => purchase,
                    Err(PlayError::NotFound) => googleplay::VerifiedPurchase {
                        product_id: row.product_id.clone(),
                        linked_token: None,
                        expires_at: row.expires_at.min(now_unix()),
                        state: "SUBSCRIPTION_STATE_EXPIRED".into(),
                        active: false,
                        auto_renewing: false,
                        acknowledged: row.acknowledged,
                        test_purchase: row.test_purchase,
                    },
                    Err(_) => {
                        tracing::warn!("Google Play reconciliation verification failed; keeping last confirmed expiry");
                        continue;
                    }
                };
                if let Ok(GoogleBinding::Saved(saved)) = state
                    .store
                    .record_google_purchase(
                        row.account_id,
                        row.purchase_token.clone(),
                        purchase,
                        None,
                        now_unix(),
                    )
                    .await
                {
                    if saved.active
                        && !saved.acknowledged
                        && play
                            .acknowledge(&row.purchase_token, &saved.product_id)
                            .await
                            .is_ok()
                    {
                        let _ = state
                            .store
                            .acknowledge_google_purchase(row.purchase_token)
                            .await;
                    }
                }
            }
        }
    });
}
