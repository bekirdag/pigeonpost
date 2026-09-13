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
