//! Read-only canonical recipient lookup for protected native delivery/recovery.

use crate::filtering::feature_on;
use crate::{
    acting_identity, capabilities, groups, is_addressable, resolve_recipient, ApiError, AppState,
};
use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
pub(crate) struct LookupReq {
    #[serde(default)]
    identity: Option<String>,
    to: String,
}

/// Resolve exactly as send does, without delivering or changing trust/threads.
pub(crate) async fn resolve(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<LookupReq>, JsonRejection>,
) -> Response {
    let req = match body {
        Ok(Json(req)) => req,
        Err(_) => {
            return ApiError::bad("bad_request", "expected a recipient and optional identity")
                .into_response()
        }
    };
    let me = match acting_identity(&state, &headers, req.identity.as_deref()).await {
        Ok(me) => me,
        Err(error) => return error.into_response(),
    };
    if !feature_on(
        &state,
        me.account_id.as_deref(),
        capabilities::RECIPIENT_LOOKUP,
    )
    .await
    {
        return ApiError::new(
            StatusCode::NOT_FOUND,
            "feature_disabled",
            "recipient lookup is not enabled",
        )
        .into_response();
    }
    if let Err(error) = groups::rate_limit(&format!("recipient_lookup:{}", me.address), 60, 60) {
        return error.into_response();
    }
    if req.to.len() > 512 || req.to.chars().any(char::is_control) || !is_addressable(&req.to) {
        return ApiError::bad(
            "invalid_recipient",
            "use a Pigeonpost handle or canonical inbox address",
        )
        .into_response();
    }
    match resolve_recipient(&state, &req.to).await {
        Ok(Some(recipient)) => Json(json!({
            "from": me.address,
            "address": recipient.address,
            "handle": recipient.handle,
        }))
        .into_response(),
        Ok(None) => ApiError::new(
            StatusCode::NOT_FOUND,
            "recipient_unresolved",
            "the recipient is unavailable",
        )
        .into_response(),
        Err(error) => error.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{build_router, do_create_identity, internal_api, new_api_key, now_unix, store};
    use reqwest::Client;
    use serde_json::Value;
    use std::sync::Arc;

    const OPS: &str = "svc_ops_0123456789abcdef0123456789abcdef";
    struct Person {
        token: String,
        account: String,
        me: store::StoredIdentity,
    }
    struct Server {
        public: String,
        internal: String,
        http: Client,
        state: AppState,
    }
    impl Server {
        async fn call(&self, token: Option<&str>, body: Value) -> (StatusCode, Value) {
            let mut req = self
                .http
                .post(format!("{}/v1/recipients/resolve", self.public))
                .json(&body);
            if let Some(token) = token {
                req = req.bearer_auth(token);
            }
            let response = req.send().await.unwrap();
            let status = response.status();
            (
                StatusCode::from_u16(status.as_u16()).unwrap(),
                response.json().await.unwrap(),
            )
        }
        async fn flag(&self, account: &str) {
            let response = self
                .http
                .put(format!(
                    "{}/internal/v1/features/recipient_lookup",
                    self.internal
                ))
                .bearer_auth(OPS)
                .json(&json!({"scope":"account","account_id":account,"enabled":true}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), 200);
        }
        async fn get(&self, path: &str, token: &str) -> Value {
            self.http
                .get(format!("{}{path}", self.public))
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap()
        }
    }
    async fn person(state: &AppState, account: &str, handle: Option<&str>) -> Person {
        let (token, hash) = new_api_key();
        state
            .store
            .create_account(account.into(), hash, now_unix())
            .await
            .unwrap();
        if let Some(handle) = handle {
            let namespace = handle.trim_start_matches('/').split('/').next().unwrap();
            assert!(state
                .store
                .set_namespace_owner(namespace.into(), account.into(), "test", now_unix(), None)
                .await
                .unwrap());
        }
        let created = do_create_identity(
            state,
            Some(account.into()),
            None,
            handle.map(str::to_string),
        )
        .await
        .unwrap();
        let me = state
            .store
            .get(created["address"].as_str().unwrap().into())
            .await
            .unwrap()
            .unwrap();
        Person {
            token,
            account: account.into(),
            me,
        }
    }
    async fn fixture() -> (Server, Person, Person) {
        let mut state = crate::tests::test_state();
        state.service_tokens = Arc::new(internal_api::tokens_for_test(&[(
            "POSTBOX_SVC_TOKEN_OPS",
            OPS,
        )]));
        let alice = person(&state, "acct_lookup_alice", None).await;
        let bob = person(&state, "acct_lookup_bob", Some("/lookup-bob/main")).await;
        let public = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let internal = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = Server {
            public: format!("http://{}", public.local_addr().unwrap()),
            internal: format!("http://{}", internal.local_addr().unwrap()),
            http: Client::new(),
            state: state.clone(),
        };
        let app = build_router(state.clone())
            .into_make_service_with_connect_info::<std::net::SocketAddr>();
        let ops = internal_api::router(state)
            .into_make_service_with_connect_info::<std::net::SocketAddr>();
        tokio::spawn(async move { axum::serve(public, app).await.unwrap() });
        tokio::spawn(async move { axum::serve(internal, ops).await.unwrap() });
        (server, alice, bob)
    }

    #[tokio::test]
    async fn lookup_requires_authentication_and_the_original_owned_sender() {
        let (srv, alice, bob) = fixture().await;
        assert_eq!(
            srv.call(None, json!({"to":bob.me.address})).await.0,
            StatusCode::UNAUTHORIZED
        );
        let status = srv
            .call(
                Some(&alice.token),
                json!({"identity":bob.me.address,"to":alice.me.address}),
            )
            .await
            .0;
        assert!(matches!(
            status,
            StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
        ));
    }

    #[tokio::test]
    async fn lookup_is_flagged_and_returns_only_canonical_route_without_mutations() {
        let (srv, alice, bob) = fixture().await;
        let (_, off) = srv
            .call(Some(&alice.token), json!({"to":bob.me.address}))
            .await;
        assert_eq!(off["error"], "feature_disabled");
        srv.flag(&alice.account).await;
        let caps = srv.get("/v1/capabilities", &alice.token).await;
        assert!(caps["features"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "recipient_lookup"));
        assert_eq!(caps["api_version"], "1.2");
        for to in [&bob.me.address, "/lookup-bob/main", "/lookup-bob"] {
            let (status, resolved) = srv
                .call(
                    Some(&alice.token),
                    json!({"identity":alice.me.address,"to":to}),
                )
                .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                resolved,
                json!({"from":alice.me.address,"address":bob.me.address,"handle":"/lookup-bob/main"})
            );
        }
        for who in [&alice, &bob] {
            assert!(srv
                .get("/v1/inbox?include_sent=true&include_read=true", &who.token)
                .await["messages"]
                .as_array()
                .unwrap()
                .is_empty());
            assert!(srv.get("/v1/threads", &who.token).await["threads"]
                .as_array()
                .unwrap()
                .is_empty());
            assert!(srv.get("/v1/contacts", &who.token).await["contacts"]
                .as_array()
                .unwrap()
                .is_empty());
        }
    }

    #[tokio::test]
    async fn lookup_rejects_unavailable_invalid_and_malformed_input_without_echoing_it() {
        let (srv, alice, _) = fixture().await;
        srv.flag(&alice.account).await;
        let (status, missing) = srv
            .call(Some(&alice.token), json!({"to":"/unclaimed-name/main"}))
            .await;
        assert_eq!(
            (status, missing["error"].as_str()),
            (StatusCode::NOT_FOUND, Some("recipient_unresolved"))
        );
        for to in [
            "not an address".to_string(),
            "/bad\nprivate".to_string(),
            format!("/{}", "a".repeat(513)),
        ] {
            let (status, error) = srv.call(Some(&alice.token), json!({"to":to})).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(error["error"], "invalid_recipient");
            assert!(!error.to_string().contains("private"));
        }
        let (status, error) = srv.call(Some(&alice.token), json!({"to":null})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["error"], "bad_request");
    }

    #[tokio::test]
    async fn lookup_is_rate_limited_without_spending_the_sender_message_quota() {
        let (srv, alice, bob) = fixture().await;
        srv.flag(&alice.account).await;
        for _ in 0..60 {
            assert_eq!(
                srv.call(Some(&alice.token), json!({"to":bob.me.address}))
                    .await
                    .0,
                StatusCode::OK
            );
        }
        let response = srv
            .http
            .post(format!("{}/v1/recipients/resolve", srv.public))
            .bearer_auth(&alice.token)
            .json(&json!({"to":bob.me.address}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 429);
        assert!(response.headers().get("retry-after").is_some());
        assert!(
            srv.get("/v1/inbox?include_sent=true", &alice.token).await["messages"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn lookup_masks_a_closed_org_recipient_like_delivery_does() {
        let (srv, alice, bob) = fixture().await;
        srv.flag(&alice.account).await;
        let address = bob.me.address.clone();
        let account = bob.account.clone();
        srv.state.store.group_tx(move |c| -> crate::groups::GResult<()> {
            c.execute("INSERT INTO orgs(id,namespace,name,owner_account,status,seats,agent_limit,storage_bytes,created_at,create_operation_id,create_hash)
                VALUES ('org_lookup_closed','lookup-org','Lookup fixture',?1,'active',1,10,1,1,'lookup-operation','lookup-hash')", [&account])?;
            c.execute("INSERT INTO org_members(id,org_id,account_id,identity,handle,first_name,last_name,status)
                VALUES ('member_lookup_closed','org_lookup_closed',?1,?2,'/lookup-bob/main','Fixture','Member','suspended')",
                rusqlite::params![account, address])?;
            Ok(())
        }).await.unwrap();
        for to in [&bob.me.address, "/lookup-bob/main"] {
            let (status, result) = srv.call(Some(&alice.token), json!({"to":to})).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            assert_eq!(result["error"], "recipient_unresolved");
            assert!(result.get("address").is_none());
        }
    }
}
