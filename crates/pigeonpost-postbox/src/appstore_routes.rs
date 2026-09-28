use crate::{appstore, now_unix, AppState};

/// Apple renewals and grace periods must reach the postbox while the phone is closed.
pub fn start_reconciliation(state: AppState) {
    let Some(apple) = state.appstore.clone() else {
        return;
    };
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(std::time::Duration::from_secs(60));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            if let Ok(plans) = state.store.apple_plans_due(now_unix()).await {
                for (account, plan) in plans {
                    let _guard = apple.verification.lock().await;
                    // A foreground purchase may have replaced this snapshot while it waited.
                    if !matches!(state.store.apple_plan(account.clone(), now_unix()).await,
                        Ok(Some(current)) if current.original_transaction_id == plan.original_transaction_id)
                    {
                        continue;
                    }
                    match apple
                        .subscription_status(&plan.original_transaction_id, &plan.environment)
                        .await
                    {
                        Ok(status) => {
                            if state
                                .store
                                .apply_apple_plan(account, status, None, now_unix())
                                .await
                                .is_err()
                            {
                                tracing::warn!("Apple plan reconciliation could not save status");
                            }
                        }
                        Err(_) => tracing::warn!(
                            "Apple plan reconciliation failed; retaining last verified entitlement"
                        ),
                    }
                }
            }
            let due = match state.store.apple_due(now_unix()).await {
                Ok(rows) => rows,
                Err(_) => {
                    tracing::warn!("Apple reconciliation could not read subscriptions");
                    continue;
                }
            };
            for row in due {
                let _guard = apple.verification.lock().await;
                let status = match apple
                    .subscription_status(&row.original, &row.environment)
                    .await
                {
                    Ok(status) => status,
                    // An outage, rejected credentials, or missing transaction is not verified expiry.
                    Err(_) => {
                        tracing::warn!("Apple reconciliation failed; keeping last confirmed expiry and reserving name");
                        continue;
                    }
                };
                let token = status.entitlement.app_account_token.as_deref();
                if token.is_some_and(|v| {
                    !v.eq_ignore_ascii_case(&appstore::account_token(&row.account))
                }) || (token.is_none() && status.entitlement.product_id != apple.product_id())
                {
                    tracing::warn!("Apple reconciliation account binding mismatch");
                    continue;
                }
                if let Err(e) = state.store.refresh_apple(row, status, now_unix()).await {
                    tracing::warn!(error = %e, "Apple reconciliation could not save status");
                }
            }
        }
    });
}

pub async fn claim_plan(
    state: &AppState,
    account: String,
    entitlement: appstore::Entitlement,
    raw: Option<String>,
) -> axum::response::Response {
    use crate::{store::AppleClaim, ApiError};
    use axum::{http::StatusCode, response::IntoResponse, Json};
    let namespace = match raw.as_deref().filter(|v| !v.trim().is_empty()) {
        None => None,
        Some(raw) => {
            let normalized = format!("/{}", raw.trim().trim_matches('/'));
            let Some(name) = pigeonpost_core::address::namespace_root(&normalized) else {
                return ApiError::bad(
                    "invalid_namespace",
                    "Choose a name of 1–32 letters, numbers, dots, underscores or hyphens",
                )
                .into_response();
            };
            let name = name.trim_start_matches('/').to_owned();
            let Some(reserved) = state.reserved_names.as_ref() else {
                return ApiError::server("reserved_names_unavailable").into_response();
            };
            if reserved.contains(&name)
                || crate::PROVIDER_NAMESPACES.contains(&name.as_str())
                || crate::OPEN_NAMESPACES.contains(&name.as_str())
            {
                return ApiError::new(
                    StatusCode::CONFLICT,
                    "name_reserved",
                    "That name is reserved",
                )
                .into_response();
            }
            Some(name)
        }
    };
    let status = appstore::SubscriptionStatus {
        entitlement,
        active: true,
        terminal: false,
        revoked: false,
    };
    match state
        .store
        .apply_apple_plan(account.clone(), status, namespace.clone(), now_unix())
        .await
    {
        Ok(AppleClaim::Granted | AppleClaim::Renewed) => {
            let mailbox = if let Some(name) = &namespace {
                crate::ensure_namespace_mailbox(state, &account, name).await
            } else {
                None
            };
            match state.store.apple_plan(account,now_unix()).await {
                Ok(plan) => Json(serde_json::json!({"plan":plan,"namespace":namespace.map(|v|format!("/{v}")),"mailbox":mailbox})).into_response(),
                Err(_) => ApiError::server("store_error").into_response(),
            }
        }
        Ok(AppleClaim::SubscriptionBoundElsewhere) => ApiError::new(
            StatusCode::CONFLICT,
            "purchase_already_used",
            "This subscription belongs to another account",
        )
        .into_response(),
        Ok(AppleClaim::LimitReached) => ApiError::new(
            StatusCode::CONFLICT,
            "plan_capacity_reached",
            "All names included in this plan are registered. Choose a larger plan to add more.",
        )
        .into_response(),
        Ok(AppleClaim::NamespaceAlreadySubscribed) => ApiError::new(
            StatusCode::CONFLICT,
            "handle_already_subscribed",
            "An active subscription already covers this account or name. Restore purchases.",
        )
        .into_response(),
        Ok(_) => ApiError::new(
            StatusCode::CONFLICT,
            "namespace_taken",
            "That name is already held",
        )
        .into_response(),
        Err(_) => ApiError::server("store_error").into_response(),
    }
}
