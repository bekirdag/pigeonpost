//! End-to-end tests for the Phase 2 billing endpoints over real sockets: the internal entitlement
//! and org routes (billing token only), and the member web-plan routes' authentication.

use crate::*;
use serde_json::{json, Value};

const OPS: &str = "svc_ops_0123456789abcdef0123456789abcdef";
const BILLING: &str = "svc_billing_0123456789abcdef0123456789abcdef";

fn state() -> AppState {
    let mut state = crate::tests::test_state();
    state.service_tokens = Arc::new(internal_api::tokens_for_test(&[
        ("POSTBOX_SVC_TOKEN_OPS", OPS),
        ("POSTBOX_SVC_TOKEN_BILLING", BILLING),
    ]));
    state.reserved_names = Some(Arc::new(["admin".to_string()].into_iter().collect()));
    state
}

async fn serve(state: AppState) -> (String, String) {
    let public = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let internal = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let public_url = format!("http://{}", public.local_addr().unwrap());
    let internal_url = format!("http://{}", internal.local_addr().unwrap());
    let app =
        build_router(state.clone()).into_make_service_with_connect_info::<std::net::SocketAddr>();
    let internal_app =
        internal_api::router(state).into_make_service_with_connect_info::<std::net::SocketAddr>();
    tokio::spawn(async move { axum::serve(public, app).await.unwrap() });
    tokio::spawn(async move { axum::serve(internal, internal_app).await.unwrap() });
    (public_url, internal_url)
}

async fn account(state: &AppState, id: &str) {
    let (_, key) = new_api_key();
    state
        .store
        .create_account(id.into(), key, now_unix())
        .await
        .unwrap();
}

async fn call(
    method: reqwest::Method,
    url: String,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = reqwest::Client::new().request(method, url);
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }
    if let Some(body) = body {
        req = req.json(&body);
    }
    let res = req.send().await.unwrap();
    let status = StatusCode::from_u16(res.status().as_u16()).unwrap();
    (status, res.json().await.unwrap_or(Value::Null))
}

fn cloud(account: &str, names: i64, paid: u64, version: i64) -> Value {
    json!({
        "subject": {"kind": "account", "id": account},
        "plan_code": format!("cloud-{names}names-yearly-usd"),
        "plan_version": "2026",
        "names": names,
        "status": "active",
        "period_end": paid,
        "paid_through": paid,
        "reconciliation_version": version,
        "operation_id": "op-test"
    })
}

#[tokio::test]
async fn entitlement_routes_take_only_the_billing_token_and_validate_everything() {
    let state = state();
    account(&state, "acct_buyer").await;
    let (public, internal) = serve(state.clone()).await;
    let url = format!("{internal}/internal/v1/entitlements/iyzico/live/sub-1");
    let paid = now_unix() + 86400;
    let put = reqwest::Method::PUT;

    let (s, _) = call(
        put.clone(),
        url.clone(),
        None,
        Some(cloud("acct_buyer", 3, paid, 1)),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, b) = call(
        put.clone(),
        url.clone(),
        Some(OPS),
        Some(cloud("acct_buyer", 3, paid, 1)),
    )
    .await;
    assert_eq!(
        (s, b["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("endpoint_not_allowed"))
    );
    // Not on the public listener at all.
    let (s, _) = call(
        put.clone(),
        format!("{public}/internal/v1/entitlements/iyzico/live/sub-1"),
        Some(BILLING),
        Some(cloud("acct_buyer", 3, paid, 1)),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Store providers are reserved for the postbox's own reconcilers.
    let (s, b) = call(
        put.clone(),
        format!("{internal}/internal/v1/entitlements/apple/live/x"),
        Some(BILLING),
        Some(cloud("acct_buyer", 3, paid, 1)),
    )
    .await;
    assert_eq!(
        (s, b["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("provider_reserved"))
    );
    for (path, body, code) in [
        (
            "iyzico/prod/sub-1",
            cloud("acct_buyer", 3, paid, 1),
            "invalid_environment",
        ),
        (
            "iyzico/live/bad%20ref",
            cloud("acct_buyer", 3, paid, 1),
            "invalid_reference",
        ),
        (
            "iyzico/live/sub-1",
            cloud("acct_buyer", 11, paid, 1),
            "invalid_names",
        ),
        (
            "iyzico/live/sub-1",
            cloud("acct_buyer", 3, paid, 0),
            "invalid_version",
        ),
        (
            "iyzico/live/sub-1",
            json!({"subject": {"kind": "account", "id": "acct_buyer"}, "plan_code": "cloud-1names-yearly-usd", "plan_version": "2026", "names": 1, "status": "active", "reconciliation_version": 1}),
            "paid_through_required",
        ),
        (
            "iyzico/live/sub-1",
            json!({"subject": {"kind": "account", "id": "acct_buyer"}, "plan_code": "Cloud", "plan_version": "2026", "names": 1, "status": "active", "paid_through": paid, "reconciliation_version": 1}),
            "invalid_plan_code",
        ),
    ] {
        let (s, b) = call(
            put.clone(),
            format!("{internal}/internal/v1/entitlements/{path}"),
            Some(BILLING),
            Some(body),
        )
        .await;
        assert_eq!(
            (s, b["error"].as_str()),
            (StatusCode::BAD_REQUEST, Some(code)),
            "{path}"
        );
    }
    let mut unknown = cloud("acct_buyer", 3, paid, 1);
    unknown["surprise"] = json!(true);
    let (s, _) = call(put.clone(), url.clone(), Some(BILLING), Some(unknown)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "unknown fields are refused");
    let (s, b) = call(
        put.clone(),
        url.clone(),
        Some(BILLING),
        Some(cloud("acct_nobody", 3, paid, 1)),
    )
    .await;
    assert_eq!(
        (s, b["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("subject_unknown"))
    );

    // Apply, replay, stale, conflict.
    let (s, b) = call(
        put.clone(),
        url.clone(),
        Some(BILLING),
        Some(cloud("acct_buyer", 3, paid, 5)),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["applied"], true);
    assert_eq!(b["entitlement"]["active"], true);
    assert_eq!(b["capacity"], json!({"names": 3, "used": 0}));
    let (_, b) = call(
        put.clone(),
        url.clone(),
        Some(BILLING),
        Some(cloud("acct_buyer", 3, paid, 5)),
    )
    .await;
    assert_eq!(
        (b["applied"].as_bool(), b["reason"].as_str()),
        (Some(false), Some("duplicate"))
    );
    let (_, b) = call(
        put.clone(),
        url.clone(),
        Some(BILLING),
        Some(cloud("acct_buyer", 1, paid, 4)),
    )
    .await;
    assert_eq!(
        (b["reason"].as_str(), b["entitlement"]["names"].as_i64()),
        (Some("stale"), Some(3))
    );
    let (s, b) = call(
        put.clone(),
        url.clone(),
        Some(BILLING),
        Some(cloud("acct_buyer", 4, paid, 5)),
    )
    .await;
    assert_eq!(
        (s, b["error"].as_str()),
        (StatusCode::CONFLICT, Some("version_conflict"))
    );

    // The audit log records the applied write once.
    let audits = state.store.audit_for_actor(None, 50).await.unwrap();
    assert_eq!(
        audits
            .iter()
            .filter(|a| a.action == "entitlement.upsert")
            .count(),
        1
    );

    // Complimentary plan and the account read.
    let (s, b) = call(put.clone(), format!("{internal}/internal/v1/accounts/acct_buyer/cloud-plan"), Some(BILLING), Some(json!({"names": 5, "paid_through": null, "reconciliation_version": 1, "note": "founder"}))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["entitlement"]["source"], "grant");
    assert_eq!(
        b["capacity"]["names"], 5,
        "the larger plan wins; plans never add up"
    );
    let (s, b) = call(
        reqwest::Method::GET,
        format!("{internal}/internal/v1/accounts/acct_buyer/entitlements"),
        Some(BILLING),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["entitlements"].as_array().unwrap().len(), 2);
    assert_eq!(b["capacity"]["names"], 5);

    // Member routes need a person's sign-in.
    let (s, _) = call(
        reqwest::Method::GET,
        format!("{public}/v1/me/entitlements"),
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, b) = call(
        reqwest::Method::POST,
        format!("{public}/v1/claims/plan"),
        Some("pk_live_not_a_member"),
        Some(json!({"namespace": "x"})),
    )
    .await;
    assert_eq!(
        (s, b["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("member_token_required"))
    );
}

#[tokio::test]
async fn org_drafts_activate_once_and_never_take_a_claimed_name() {
    let state = state();
    account(&state, "acct_owner").await;
    account(&state, "acct_squatter").await;
    let (_, internal) = serve(state.clone()).await;
    let post = reqwest::Method::POST;
    let put = reqwest::Method::PUT;
    let draft = |op: &str, ns: &str| {
        json!({"operation_id": op, "owner_account": "acct_owner", "name": "Acme Inc", "namespace": ns,
               "plan_code": "ent-30-yearly-usd", "seats": 30})
    };
    let drafts = format!("{internal}/internal/v1/org-drafts");
    let (s, b) = call(
        post.clone(),
        drafts.clone(),
        Some(OPS),
        Some(draft("op_aaaaaaaaaaaaaaaa", "acme")),
    )
    .await;
    assert_eq!(
        (s, b["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("endpoint_not_allowed"))
    );
    let (s, b) = call(
        post.clone(),
        drafts.clone(),
        Some(BILLING),
        Some(draft("op_aaaaaaaaaaaaaaaa", "acme")),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let org = b["org"]["id"].as_str().unwrap().to_string();
    assert!(org.starts_with("org_"));
    assert_eq!(
        (
            b["org"]["status"].as_str(),
            b["org"]["agent_limit"].as_i64()
        ),
        (Some("pending"), Some(300))
    );
    let (s, b2) = call(
        post.clone(),
        drafts.clone(),
        Some(BILLING),
        Some(draft("op_aaaaaaaaaaaaaaaa", "acme")),
    )
    .await;
    assert_eq!(
        (s, b2["org"]["id"].as_str()),
        (StatusCode::OK, Some(org.as_str())),
        "a replay returns the same draft"
    );
    let (s, b) = call(
        post.clone(),
        drafts.clone(),
        Some(BILLING),
        Some(draft("op_aaaaaaaaaaaaaaaa", "other")),
    )
    .await;
    assert_eq!(
        (s, b["error"].as_str()),
        (StatusCode::CONFLICT, Some("operation_conflict"))
    );
    let (s, b) = call(
        post.clone(),
        drafts.clone(),
        Some(BILLING),
        Some(draft("op_bbbbbbbbbbbbbbbb", "admin")),
    )
    .await;
    assert_eq!(
        (s, b["error"].as_str()),
        (StatusCode::CONFLICT, Some("namespace_unavailable"))
    );
    let mut bad_plan = draft("op_cccccccccccccccc", "acme2");
    bad_plan["plan_code"] = json!("ent-20-yearly-usd");
    let (s, b) = call(post.clone(), drafts.clone(), Some(BILLING), Some(bad_plan)).await;
    assert_eq!(
        (s, b["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_plan_code"))
    );

    let paid = now_unix() + 86400;
    let ent = |version: i64, status: &str| {
        json!({"provider": "iyzico", "environment": "live", "reference": "sub-org-1",
               "plan_code": "ent-30-yearly-usd", "plan_version": "2026", "seats": 30, "status": status,
               "period_end": paid, "paid_through": paid, "reconciliation_version": version})
    };
    let activate = format!("{internal}/internal/v1/orgs/{org}/activate");
    let (s, b) = call(
        put.clone(),
        activate.clone(),
        Some(BILLING),
        Some(json!({"operation_id": "op_activate_000001", "entitlement": ent(1, "expired")})),
    )
    .await;
    assert_eq!(
        (s, b["error"].as_str()),
        (StatusCode::CONFLICT, Some("entitlement_inactive"))
    );
    let (s, b) = call(
        put.clone(),
        activate.clone(),
        Some(BILLING),
        Some(json!({"operation_id": "op_activate_000001", "entitlement": ent(1, "active")})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["org"]["status"], "active");
    assert_eq!(b["entitlement"]["subject_kind"], "org");
    // Idempotent.
    let (s, b) = call(
        put.clone(),
        activate.clone(),
        Some(BILLING),
        Some(json!({"operation_id": "op_activate_000001", "entitlement": ent(1, "active")})),
    )
    .await;
    assert_eq!(
        (s, b["org"]["status"].as_str()),
        (StatusCode::OK, Some("active"))
    );
    // The org namespace belongs to the org's custodian account, not to the owner.
    assert!(!state
        .store
        .namespace_available("acme".into(), now_unix())
        .await
        .unwrap());
    assert!(state
        .store
        .namespaces_for_account("acct_owner".into(), now_unix())
        .await
        .unwrap()
        .is_empty());

    // Lapse makes it read-only; recovery makes it active again.
    let (s, b) = call(put.clone(), format!("{internal}/internal/v1/orgs/{org}/entitlement"), Some(BILLING), Some(json!({
        "provider": "iyzico", "environment": "live", "reference": "sub-org-1", "plan_code": "ent-30-yearly-usd",
        "plan_version": "2026", "seats": 30, "status": "expired", "period_end": 10, "paid_through": 10, "reconciliation_version": 2}))).await;
    assert_eq!(
        (s, b["org"]["status"].as_str()),
        (StatusCode::OK, Some("read_only"))
    );
    let (s, b) = call(
        reqwest::Method::GET,
        format!("{internal}/internal/v1/orgs/{org}"),
        Some(BILLING),
        None,
    )
    .await;
    assert_eq!(
        (s, b["entitlements"].as_array().map(Vec::len)),
        (StatusCode::OK, Some(1))
    );

    // A draft whose name was claimed before payment completed is paid but unprovisioned.
    let (_, b) = call(
        post.clone(),
        drafts.clone(),
        Some(BILLING),
        Some(draft("op_dddddddddddddddd", "beta")),
    )
    .await;
    let late = b["org"]["id"].as_str().unwrap().to_string();
    assert!(state
        .store
        .set_namespace_owner(
            "beta".into(),
            "acct_squatter".into(),
            "entitlement",
            now_unix(),
            None
        )
        .await
        .unwrap());
    let mut e = ent(1, "active");
    e["reference"] = json!("sub-org-2");
    let (s, b) = call(
        put.clone(),
        format!("{internal}/internal/v1/orgs/{late}/activate"),
        Some(BILLING),
        Some(json!({"operation_id": "op_activate_000002", "entitlement": e})),
    )
    .await;
    assert_eq!(
        (s, b["org"]["status"].as_str()),
        (StatusCode::OK, Some("paid_unprovisioned"))
    );
    let held = state
        .store
        .namespaces_for_account("acct_squatter".into(), now_unix())
        .await
        .unwrap();
    assert_eq!(
        held[0].namespace, "beta",
        "the existing holder keeps the name"
    );
    let (s, _) = call(
        put.clone(),
        format!("{internal}/internal/v1/orgs/org_missing/activate"),
        Some(BILLING),
        Some(json!({"operation_id": "op_activate_000003", "entitlement": ent(1, "active")})),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}
