//! Complimentary handles for explicitly approved preview participants. No payment or client
//! assertion grants an entitlement: a verified realm identity must match the server allowlist.

use super::*;
use std::collections::HashSet;

pub fn configured_testers() -> Arc<HashSet<String>> {
    Arc::new(
        env_or("POSTBOX_TEST_HANDLE_TESTERS", "")
            .split(',')
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty())
            .collect(),
    )
}

fn eligible(state: &AppState, claims: &oidc::Claims) -> bool {
    claims.address_is_verified()
        && claims.address().is_some_and(|address| {
            state
                .test_handle_testers
                .contains(&address.to_ascii_lowercase())
        })
}

async fn member(state: &AppState, headers: &HeaderMap) -> Result<oidc::Claims, ApiError> {
    let token = bearer(headers)
        .filter(|token| token.starts_with("eyJ"))
        .ok_or_else(|| ApiError::unauthorized("sign in to manage your handle"))?;
    state
        .oidc
        .validate(token)
        .await
        .map_err(|_| ApiError::unauthorized("invalid member token"))
}

async fn account(state: &AppState, claims: &oidc::Claims) -> Result<String, ApiError> {
    state
        .store
        .account_for_sub(
            claims.sub.clone(),
            format!("acct_{}", rand_hex(12)),
            now_unix(),
        )
        .await
        .map_err(|_| ApiError::server("store_error"))
}

async fn offer(state: &AppState, account: &str, allowed: bool) -> Result<Response, ApiError> {
    let holdings = state
        .store
        .namespaces_for_account(account.to_string(), now_unix())
        .await
        .map_err(|_| ApiError::server("store_error"))?;
    let held = holdings
        .iter()
        .find(|held| held.source == "test_preview")
        .or(holdings.first());
    let mailbox = if let Some(held) = held {
        state
            .store
            .handles_in_namespace(account.to_string(), held.namespace.clone())
            .await
            .map_err(|_| ApiError::server("store_error"))?
            .into_iter()
            .next()
    } else {
        None
    };
    Ok(Json(json!({
        "eligible": allowed,
        "namespace": held.map(|held| format!("/{}", held.namespace)),
        "source": held.map(|held| &held.source),
        "expires_at": held.and_then(|held| held.expires_at),
        "mailbox": mailbox,
    }))
    .into_response())
}

pub async fn state(State(state): State<AppState>, headers: HeaderMap) -> Response {
    state_for_member(&state, &headers)
        .await
        .unwrap_or_else(ApiError::into_response)
}

async fn state_for_member(state: &AppState, headers: &HeaderMap) -> Result<Response, ApiError> {
    let claims = member(state, headers).await?;
    let account = account(state, &claims).await?;
    offer(state, &account, eligible(state, &claims)).await
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimRequest {
    namespace: String,
}

pub async fn claim(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ClaimRequest>,
) -> Response {
    match member(&state, &headers).await {
        Ok(claims) => claim_for_member(&state, &claims, &req.namespace)
            .await
            .unwrap_or_else(ApiError::into_response),
        Err(error) => error.into_response(),
    }
}

async fn claim_for_member(
    state: &AppState,
    claims: &oidc::Claims,
    raw: &str,
) -> Result<Response, ApiError> {
    if !eligible(state, claims) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "tester_required",
            "free registration is available to approved testers with a verified sign-in address",
        ));
    }
    let normalized = format!("/{}", raw.trim().trim_matches('/'));
    let name = pigeonpost_core::address::namespace_root(&normalized).ok_or_else(|| {
        ApiError::bad(
            "invalid_namespace",
            "choose a name of 1–32 letters, numbers, dots, underscores or hyphens",
        )
    })?;
    let namespace = name.trim_start_matches('/').to_string();
    let reserved = state
        .reserved_names
        .as_ref()
        .ok_or_else(|| ApiError::server("reserved_names_unavailable"))?;
    if reserved.contains(&namespace)
        || PROVIDER_NAMESPACES.contains(&namespace.as_str())
        || OPEN_NAMESPACES.contains(&namespace.as_str())
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "name_reserved",
            "that name is reserved",
        ));
    }
    let account = account(state, claims).await?;
    match state
        .store
        .claim_test_namespace(account.clone(), namespace.clone(), now_unix())
        .await
        .map_err(|_| ApiError::server("store_error"))?
    {
        store::TestHandleClaim::Granted => {}
        store::TestHandleClaim::AlreadyNamed(name) => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "tester_already_named",
                format!("this account already registered /{name}"),
            ))
        }
        store::TestHandleClaim::NamespaceTaken => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "namespace_taken",
                "someone already has that name; try another",
            ))
        }
    }
    ensure_namespace_mailbox(state, &account, &namespace).await;
    offer(state, &account, true).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tester(verified: bool) -> oidc::Claims {
        oidc::Claims::fixture("tester-sub", "Tester@Example.test", verified)
    }
    fn setup() -> AppState {
        let mut state = crate::tests::state_with_reserved(&["support"]);
        state.test_handle_testers = Arc::new(["tester@example.test".into()].into_iter().collect());
        state
    }
    #[tokio::test]
    async fn requires_an_explicit_verified_tester() {
        let mut state = setup();
        assert_eq!(
            claim_for_member(&state, &tester(false), "alex")
                .await
                .unwrap_err()
                .code,
            "tester_required"
        );
        state.test_handle_testers = Arc::new(HashSet::new());
        assert_eq!(
            claim_for_member(&state, &tester(true), "alex")
                .await
                .unwrap_err()
                .code,
            "tester_required"
        );
        assert!(state
            .store
            .namespace_owner("alex".into(), now_unix())
            .await
            .unwrap()
            .is_none());
    }
    #[tokio::test]
    async fn reserved_and_nested_names_are_refused() {
        let state = setup();
        for raw in ["support", "SUPPORT", "/github", "/pp/"] {
            assert_eq!(
                claim_for_member(&state, &tester(true), raw)
                    .await
                    .unwrap_err()
                    .code,
                "name_reserved"
            );
        }
        for raw in ["a/b", "", "-alex", "alex-", "k"] {
            assert_eq!(
                claim_for_member(&state, &tester(true), raw)
                    .await
                    .unwrap_err()
                    .code,
                "invalid_namespace"
            );
        }
    }
    #[tokio::test]
    async fn a_claim_creates_a_usable_inbox_and_retries_do_not_duplicate_it() {
        let state = setup();
        for raw in [" /Alex/ ", "alex"] {
            let response = claim_for_member(&state, &tester(true), raw).await.unwrap();
            let body = axum::body::to_bytes(response.into_body(), 65536)
                .await
                .unwrap();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(value["namespace"], "/alex");
            assert_eq!(value["mailbox"], "/alex/main");
            assert_eq!(value["source"], "test_preview");
        }
        let owner = account(&state, &tester(true)).await.unwrap();
        assert_eq!(
            state
                .store
                .handles_in_namespace(owner, "alex".into())
                .await
                .unwrap(),
            vec!["/alex/main"]
        );
        assert_eq!(
            claim_for_member(&state, &tester(true), "blake")
                .await
                .unwrap_err()
                .code,
            "tester_already_named"
        );
    }
    #[tokio::test]
    async fn endpoints_require_member_authentication() {
        let state = setup();
        assert_eq!(
            super::state(State(state.clone()), HeaderMap::new())
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            claim(
                State(state),
                HeaderMap::new(),
                Json(ClaimRequest {
                    namespace: "alex".into()
                })
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
    }
}
