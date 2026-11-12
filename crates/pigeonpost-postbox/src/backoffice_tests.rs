//! The back office (Phase 1) over real sockets: the role gate of every route, step-up windows, the
//! audit record of every read and write, paging and search, masking, the saas_be finance proxy,
//! and the `staff_roles` rebuild on a database that already holds rows. Synthetic accounts only.

use crate::backoffice_finance::Finance;
use crate::groups::GErr;
use crate::org_tests::{serve_with, Person, Srv, ISSUER};
use crate::*;
use reqwest::Method;
use serde_json::{json, Value};
use std::sync::Mutex;

const OWNER_ACCOUNT: &str = "acct_bo_owner";

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or_default()
}

/// What the fake saas_be saw: method, path and query, the bearer, and the JSON body.
type Seen = Arc<Mutex<Vec<(String, String, String, Value)>>>;

const FINANCE_TOKEN: &str = "fin_0123456789abcdef0123456789abcdef";
const PURCHASE: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";
const REFUND: &str = "7c9e6679-7425-40de-944b-e07fc1f90ae7";
const SLOW_PURCHASE: &str = "11111111-2222-4333-8444-555555555555";

/// A stand-in for saas_be marketplace finance on loopback.
async fn fake_saas() -> (String, Seen) {
    use axum::extract::{OriginalUri, Request};
    use axum::http::HeaderMap;
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let app = axum::Router::new().fallback(move |OriginalUri(uri): OriginalUri, headers: HeaderMap, req: Request| {
        let log = log.clone();
        async move {
            let method = req.method().to_string();
            let bytes = axum::body::to_bytes(req.into_body(), 1 << 20).await.unwrap_or_default();
            let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            let auth = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let path = uri.to_string();
            log.lock().unwrap().push((method, path.clone(), auth, body.clone()));
            let p = uri.path();
            let (status, out) = if p.ends_with("/balances/account/acct_bo_buyer") {
                (200, json!({ "owner_kind": "account", "owner_id": "acct_bo_buyer", "currency": "USD", "available_cents": 2500, "held_cents": 100 }))
            } else if p.contains("/history/") {
                (200, json!({ "items": [{ "type": "purchase", "id": PURCHASE, "amount_cents": 2500, "status": "succeeded",
                    "charge": { "currency": "TRY", "amount_minor": 99000, "tax_minor": 0 }, "fx": { "rate": "39.6" } }],
                    "next_cursor": "c1" }))
            } else if p.ends_with("/finance/credit-sales") {
                (200, json!({ "items": (0..3).map(|i| json!({ "sale_id": format!("s{i}"), "amount_cents": 100, "commission_cents": 10 })).collect::<Vec<_>>() }))
            } else if p.ends_with("/finance/credit-refunds") {
                (200, json!({ "items": [{ "refund_id": REFUND, "status": "uncertain" }] }))
            } else if p.ends_with("/payout-profile") {
                (200, json!({ "publisher_id": "pub_x", "iban_last4": "1234", "bank_name": "Bank", "country": "TR", "currency": "TRY", "status": "verified", "iban": "TR000000000000000000001234" }))
            } else if p.ends_with(&format!("/purchases/{SLOW_PURCHASE}/refund")) {
                tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                (200, json!({ "refund_id": REFUND, "status": "pending" }))
            } else if p.ends_with(&format!("/purchases/{PURCHASE}/refund")) {
                (200, json!({ "refund_id": REFUND, "status": "pending" }))
            } else if p.ends_with("/resolve") {
                (409, json!({ "message": "This refund is already closed." }))
            } else if p.ends_with("/credits/adjustments") {
                (201, json!({ "adjustment_id": "adj_1", "amount_cents": body["amount_cents"].clone() }))
            } else {
                (404, json!({ "message": "no" }))
            };
            (axum::http::StatusCode::from_u16(status).unwrap(), axum::Json(out))
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        format!("http://{addr}/v1/internal/tenants/t1/marketplace"),
        seen,
    )
}

struct Bo {
    srv: Srv,
    owner: Person,
    seen: Seen,
}

async fn setup() -> Bo {
    let (url, seen) = fake_saas().await;
    let srv = serve_with(move |s| {
        s.platform_owner = Some(Arc::from(OWNER_ACCOUNT));
        s.staff_finance = Arc::new(Finance::http(&url, FINANCE_TOKEN));
    })
    .await;
    let owner = srv.person("bo_owner").await;
    assert_eq!(owner.account, OWNER_ACCOUNT);
    Bo { srv, owner, seen }
}

async fn grant(bo: &Bo, account: &str, role: &str) {
    let (s, v) = bo
        .srv
        .post(
            "/v1/staff/roles",
            &bo.owner.strong,
            json!({ "account_id": account, "role": role }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
}

/// A strong sign-in made `age` seconds ago.
fn strong_from(name: &str, age: u64) -> String {
    let mut claims = oidc::test_person_claims(&format!("{name}@example.com"), Some("mfa"));
    claims["auth_time"] = json!(now_unix() - age);
    oidc::test_member_token_with(ISSUER, &format!("sub-{name}"), claims)
}

async fn sql(srv: &Srv, statements: String) {
    srv.state
        .store
        .group_tx(move |tx| -> Result<(), GErr> {
            tx.execute_batch(&statements)?;
            Ok(())
        })
        .await
        .unwrap();
}

async fn last_audit(srv: &Srv, account: &str) -> store::AuditRow {
    srv.state
        .store
        .audit_for_actor(Some(account.to_string()), 1)
        .await
        .unwrap()
        .pop()
        .expect("an audit row")
}

async fn audit_count(srv: &Srv, account: &str) -> usize {
    srv.state
        .store
        .audit_for_actor(Some(account.to_string()), 10_000)
        .await
        .unwrap()
        .len()
}

/// Synthetic rows the read routes find: a buyer account with a sign-in identity, two mailboxes,
/// a handle, a device, an entitlement and an App Store subscription.
async fn seed(srv: &Srv) {
    let now = now_unix() as i64;
    sql(
        srv,
        format!(
            "INSERT INTO accounts (id, created_at, oidc_sub) VALUES ('acct_bo_buyer', {now}, 'sub-buyer');
             INSERT INTO provider_identities (provider, login, provider_user_id, account_id, verified_at)
                 VALUES ('realm', 'buyer@shop.example', 'sub-buyer', 'acct_bo_buyer', {now});
             INSERT INTO identities (address, ed25519_pub, x25519_pub, wrapped_nonce, wrapped_ct, cap_hash, label, created_at, account_id, handle)
                 VALUES ('/k/bobuyer1', x'00', x'00', x'00', x'00', x'00', 'phone', {now}, 'acct_bo_buyer', '/shopkeep'),
                        ('/k/bobuyer2', x'00', x'00', x'00', x'00', x'00', 'agent', {now}, 'acct_bo_buyer', '/shopkeep/helper');
             INSERT INTO namespaces (namespace, account_id, source, verified_at, expires_at)
                 VALUES ('shopkeep', 'acct_bo_buyer', 'apple', {now}, {exp});
             INSERT INTO devices (token, mailbox, account, platform, environment, updated_at)
                 VALUES ('apns-device-token-0123456789abcdef', '/k/bobuyer1', 'acct_bo_buyer', 'apns', 'production', {now});
             INSERT INTO apple_subscriptions (original_transaction_id, account_id, namespace, environment, product_id, expires_at, created_at, updated_at)
                 VALUES ('2000000123456789', 'acct_bo_buyer', 'shopkeep', 'Production', 'dev.pigeonpost.handle1', {exp}, {now}, {now});",
            exp = now + 86_400 * 30
        ),
    )
    .await;
}

// ---- the role gate --------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
enum Need {
    Any,
    People,
    Plans,
    Finance,
    Audit,
}

fn allowed(need: Need, role: &str) -> bool {
    match need {
        Need::Any => role != "none",
        Need::People => matches!(role, "owner" | "support"),
        Need::Plans => matches!(role, "support" | "finance"),
        Need::Finance => role == "finance",
        Need::Audit => role == "owner",
    }
}

/// Every back office route, each role on its own: allowed (with the audit record a personal read
/// writes) or `403 staff_required`. The owner role here is a `staff_roles` row, not the platform
/// owner.
#[tokio::test]
async fn every_route_admits_exactly_its_roles_and_records_each_read() {
    let bo = setup().await;
    seed(&bo.srv).await;
    bo.srv.global_flag("orgs").await;
    let org = bo.srv.org(&bo.owner, "boorg", 5).await;
    let mut people = Vec::new();
    for role in ["owner", "support", "finance", "reviewer", "ops", "none"] {
        let p = bo.srv.person(&format!("bo_{role}")).await;
        if role != "none" {
            grant(&bo, &p.account, role).await;
        }
        people.push((role, p));
    }
    let gets: Vec<(String, Need, Option<&str>)> = vec![
        ("/v1/staff/me".into(), Need::Any, None),
        ("/v1/staff/dashboard".into(), Need::Any, None),
        (
            "/v1/staff/accounts?q=shop".into(),
            Need::People,
            Some("staff.accounts.search"),
        ),
        (
            "/v1/staff/accounts/acct_bo_buyer".into(),
            Need::People,
            Some("staff.accounts.read"),
        ),
        (
            "/v1/staff/handles".into(),
            Need::People,
            Some("staff.handles.search"),
        ),
        (
            "/v1/staff/handles/shopkeep".into(),
            Need::People,
            Some("staff.handles.read"),
        ),
        (
            "/v1/staff/mailboxes?kind=account".into(),
            Need::People,
            Some("staff.mailboxes.search"),
        ),
        (
            "/v1/staff/orgs".into(),
            Need::People,
            Some("staff.orgs.search"),
        ),
        (
            format!("/v1/staff/orgs/{org}"),
            Need::People,
            Some("staff.orgs.read"),
        ),
        (
            "/v1/staff/entitlements?source=grant".into(),
            Need::Plans,
            Some("staff.plans.search"),
        ),
        (
            "/v1/staff/subscriptions?store=apple".into(),
            Need::Plans,
            Some("staff.plans.subscriptions"),
        ),
        (
            "/v1/staff/audit?action=staff.".into(),
            Need::Audit,
            Some("staff.audit.search"),
        ),
        (
            "/v1/staff/credits/owners/account/acct_bo_buyer/balance".into(),
            Need::Finance,
            Some("staff.credits.balance"),
        ),
        (
            "/v1/staff/credits/owners/account/acct_bo_buyer/history".into(),
            Need::Finance,
            Some("staff.credits.history"),
        ),
        (
            "/v1/staff/credits/sales".into(),
            Need::Finance,
            Some("staff.credits.sales"),
        ),
        (
            "/v1/staff/credits/refunds?status=uncertain".into(),
            Need::Finance,
            Some("staff.credits.refunds"),
        ),
        (
            "/v1/staff/publishers/pub_x/payout-profile".into(),
            Need::Finance,
            Some("staff.publishers.payout_profile"),
        ),
    ];
    for (role, p) in &people {
        for (path, need, verb) in &gets {
            let before = audit_count(&bo.srv, &p.account).await;
            let (s, v) = bo.srv.get(path, &p.strong).await;
            if allowed(*need, role) {
                assert_eq!(s, 200, "{role} GET {path}: {v}");
                assert!(v.get("data").is_some(), "{role} GET {path}: {v}");
                let after = audit_count(&bo.srv, &p.account).await;
                match verb {
                    Some(verb) => {
                        assert_eq!(after, before + 1, "{role} GET {path}: one audit row");
                        let row = last_audit(&bo.srv, &p.account).await;
                        assert_eq!(row.action, *verb, "{path}");
                        assert_eq!(row.actor_kind, "human");
                    }
                    None => assert_eq!(after, before, "{path}: no personal data, no row"),
                }
            } else {
                assert_eq!(
                    (s, code(&v)),
                    (403, "staff_required"),
                    "{role} GET {path}: {v}"
                );
                assert_eq!(
                    audit_count(&bo.srv, &p.account).await,
                    before,
                    "a refusal reads nothing"
                );
            }
        }
        // Writes: finance only.
        let (s, v) = bo
            .srv
            .post(
                &format!("/v1/staff/credits/purchases/{PURCHASE}/refund"),
                &p.strong,
                json!({ "reason": "duplicate purchase" }),
            )
            .await;
        if *role == "finance" {
            assert_eq!(s, 200, "{v}");
        } else {
            assert_eq!((s, code(&v)), (403, "staff_required"), "{role}: {v}");
        }
    }
    // A mailbox token, an API key and nobody at all are never staff.
    let (_, owner_role) = &people[0];
    let (key, stored) = new_api_key();
    bo.srv
        .state
        .store
        .add_api_key(owner_role.account.clone(), stored, now_unix())
        .await
        .unwrap();
    let (s, v) = bo.srv.get("/v1/staff/accounts", &key).await;
    assert_eq!((s, code(&v)), (403, "human_required"), "{v}");
    let (s, _) = bo.srv.call(Method::GET, "/v1/staff/me", None, None).await;
    assert_eq!(s, 401);
}

/// The platform owner holds `owner` without a row, and none of this needs the marketplace flag;
/// granting and revoking the new roles works without it too.
#[tokio::test]
async fn the_platform_owner_is_owner_and_manages_every_role_without_the_flag() {
    let bo = setup().await;
    let (s, v) = bo.srv.get("/v1/staff/me", &bo.owner.token).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["roles"], json!(["owner"]));
    assert_eq!(v["data"]["platform_owner"], json!(true));
    assert_eq!(v["data"]["finance_configured"], json!(true));
    assert_eq!(v["data"]["step_up"]["read_max_age"], json!(3600));
    assert_eq!(v["data"]["step_up"]["write_max_age"], json!(600));
    let (s, _) = bo.srv.get("/v1/staff/audit", &bo.owner.strong).await;
    assert_eq!(s, 200);
    // Owner does not imply finance.
    let (s, v) = bo
        .srv
        .get("/v1/staff/credits/sales", &bo.owner.strong)
        .await;
    assert_eq!((s, code(&v)), (403, "staff_required"), "{v}");

    let helper = bo.srv.person("bo_helper").await;
    for role in ["owner", "support", "finance", "reviewer", "ops"] {
        grant(&bo, &helper.account, role).await;
    }
    let (s, v) = bo
        .srv
        .post(
            "/v1/staff/roles",
            &bo.owner.strong,
            json!({ "account_id": helper.account, "role": "admin" }),
        )
        .await;
    assert_eq!((s, code(&v)), (400, "invalid_role"), "{v}");
    let (s, v) = bo.srv.get("/v1/staff/me", &helper.token).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        v["data"]["roles"],
        json!(["owner", "support", "finance", "reviewer", "ops"])
    );
    // An `owner` role row is not the platform owner: it cannot manage roles.
    let (s, v) = bo.srv.get("/v1/staff/roles", &helper.strong).await;
    assert_eq!((s, code(&v)), (403, "forbidden"), "{v}");
    // Revoke one; the list and the gate follow at once.
    let (_, list) = bo.srv.get("/v1/staff/roles", &bo.owner.strong).await;
    let row = list["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["account_id"] == json!(helper.account) && r["role"] == "support")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (s, _) = bo
        .srv
        .call(
            Method::DELETE,
            &format!("/v1/staff/roles/{row}"),
            Some(&bo.owner.strong),
            None,
        )
        .await;
    assert_eq!(s, 204);
    let (_, v) = bo.srv.get("/v1/staff/me", &helper.token).await;
    assert_eq!(
        v["data"]["roles"],
        json!(["owner", "finance", "reviewer", "ops"])
    );
    let row = last_audit(&bo.srv, OWNER_ACCOUNT).await;
    assert_eq!(row.action, "staff.role.revoke");
}

// ---- step-up ------------------------------------------------------------------------------------

#[tokio::test]
async fn personal_reads_need_an_hourly_step_up_and_writes_ten_minutes() {
    let bo = setup().await;
    seed(&bo.srv).await;
    let fin = bo.srv.person("bo_fin").await;
    grant(&bo, &fin.account, "finance").await;
    grant(&bo, &fin.account, "support").await;

    // No step-up: the overview works, a personal read does not, and it names the hour window.
    let (s, _) = bo.srv.get("/v1/staff/dashboard", &fin.token).await;
    assert_eq!(s, 200);
    let before = audit_count(&bo.srv, &fin.account).await;
    let (s, v) = bo.srv.get("/v1/staff/accounts", &fin.token).await;
    assert_eq!((s, code(&v)), (401, "step_up_required"), "{v}");
    assert_eq!(v["error"]["details"]["max_age"], json!(3600), "{v}");
    assert_eq!(audit_count(&bo.srv, &fin.account).await, before);

    // Thirty minutes old: reads yes, money no.
    let half = strong_from("bo_fin", 1800);
    let (s, v) = bo.srv.get("/v1/staff/accounts", &half).await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = bo
        .srv
        .get(
            "/v1/staff/credits/owners/account/acct_bo_buyer/balance",
            &half,
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let calls = bo.seen.lock().unwrap().len();
    let (s, v) = bo
        .srv
        .post(
            &format!("/v1/staff/credits/purchases/{PURCHASE}/refund"),
            &half,
            json!({ "reason": "x" }),
        )
        .await;
    assert_eq!((s, code(&v)), (401, "step_up_required"), "{v}");
    assert_eq!(v["error"]["details"]["max_age"], json!(600), "{v}");
    assert_eq!(
        bo.seen.lock().unwrap().len(),
        calls,
        "saas_be was never asked"
    );

    // Over an hour: nothing personal.
    let stale = strong_from("bo_fin", 3601);
    let (s, v) = bo.srv.get("/v1/staff/handles", &stale).await;
    assert_eq!((s, code(&v)), (401, "step_up_required"), "{v}");
}

// ---- search, paging, masking --------------------------------------------------------------------

#[tokio::test]
async fn search_finds_accounts_by_every_key_pages_and_masks() {
    let bo = setup().await;
    seed(&bo.srv).await;
    let sup = bo.srv.person("bo_sup").await;
    grant(&bo, &sup.account, "support").await;
    for q in [
        "acct_bo_buyer",
        "sub-buyer",
        "buyer@shop",
        "/shopkeep",
        "shopkeep/helper",
        "/k/bobuyer2",
        "SHOPKEEP",
    ] {
        let (s, v) = bo
            .srv
            .get(
                &format!("/v1/staff/accounts?q={}", urlencode(q)),
                &sup.strong,
            )
            .await;
        assert_eq!(s, 200, "{v}");
        let ids: Vec<_> = v["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].clone())
            .collect();
        assert_eq!(ids, vec![json!("acct_bo_buyer")], "q={q}: {v}");
    }
    // A wildcard is literal.
    let (_, v) = bo.srv.get("/v1/staff/accounts?q=%25", &sup.strong).await;
    assert_eq!(v["data"], json!([]), "{v}");
    let row = last_audit(&bo.srv, &sup.account).await;
    assert!(
        row.details.contains("\"q\":\"%\""),
        "the filter is recorded: {}",
        row.details
    );

    // Paging: every account, two at a time, no row twice, then the end.
    let total = bo
        .srv
        .get("/v1/staff/accounts?limit=100", &sup.strong)
        .await
        .1["data"]
        .as_array()
        .unwrap()
        .len();
    assert!(total >= 3);
    let mut seen = std::collections::BTreeSet::new();
    let mut cursor: Option<String> = None;
    loop {
        let path = match &cursor {
            Some(c) => format!("/v1/staff/accounts?limit=2&cursor={c}"),
            None => "/v1/staff/accounts?limit=2".into(),
        };
        let (s, v) = bo.srv.get(&path, &sup.strong).await;
        assert_eq!(s, 200, "{v}");
        for r in v["data"].as_array().unwrap() {
            assert!(seen.insert(r["id"].as_str().unwrap().to_string()));
        }
        match v["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    assert_eq!(seen.len(), total);
    for bad in ["limit=0", "limit=101", "cursor=12", "cursor=o-3"] {
        let (s, _) = bo
            .srv
            .get(&format!("/v1/staff/accounts?{bad}"), &sup.strong)
            .await;
        assert_eq!(s, 400, "{bad}");
    }

    // Detail: tokens and references masked, keys never selected.
    let (s, v) = bo
        .srv
        .get("/v1/staff/accounts/acct_bo_buyer", &sup.strong)
        .await;
    assert_eq!(s, 200, "{v}");
    let d = &v["data"];
    assert_eq!(d["sign_in_identities"][0]["login"], "buyer@shop.example");
    assert_eq!(d["devices"][0]["push_token"], "••••abcdef");
    assert_eq!(d["subscriptions"]["apple"][0]["reference"], "••••6789");
    assert_eq!(d["handles"][0]["namespace"], "shopkeep");
    assert_eq!(d["mailboxes"].as_array().unwrap().len(), 2);
    let text = v.to_string();
    for secret in [
        "apns-device-token",
        "2000000123456789",
        "cap_hash",
        "ed25519",
        "wrapped",
    ] {
        assert!(!text.contains(secret), "{secret} leaked: {text}");
    }
    let (s, v) = bo
        .srv
        .get("/v1/staff/accounts/acct_nope", &sup.strong)
        .await;
    assert_eq!((s, code(&v)), (404, "not_found"));

    // Handles: holder, mailboxes under the namespace, kind filter on mailboxes.
    let (_, v) = bo.srv.get("/v1/staff/handles?q=shop", &sup.strong).await;
    assert_eq!(v["data"][0]["namespace"], "shopkeep");
    assert_eq!(v["data"][0]["mailboxes"], 2);
    let (_, v) = bo.srv.get("/v1/staff/handles/shopkeep", &sup.strong).await;
    assert_eq!(v["data"]["holder"]["account_id"], "acct_bo_buyer");
    assert_eq!(v["data"]["mailboxes"].as_array().unwrap().len(), 2);
    let (_, v) = bo
        .srv
        .get("/v1/staff/mailboxes?q=bobuyer", &sup.strong)
        .await;
    assert_eq!(v["data"].as_array().unwrap().len(), 2, "{v}");
    assert_eq!(v["data"][0]["kind"], "account");
    let (_, v) = bo
        .srv
        .get("/v1/staff/mailboxes?q=bobuyer&kind=nest", &sup.strong)
        .await;
    assert_eq!(v["data"], json!([]));
    let (s, _) = bo
        .srv
        .get("/v1/staff/mailboxes?kind=robot", &sup.strong)
        .await;
    assert_eq!(s, 400);
}

#[tokio::test]
async fn org_detail_masks_keys_and_never_sends_domain_tokens() {
    let bo = setup().await;
    let sup = bo.srv.person("bo_orgsup").await;
    grant(&bo, &sup.account, "support").await;
    bo.srv.global_flag("orgs").await;
    let org = bo.srv.org(&bo.owner, "masked", 5).await;
    let now = now_unix();
    sql(
        &bo.srv,
        format!(
            "INSERT INTO org_domains (org_id, domain, token, created_at) VALUES ('{org}', 'masked.example', 'pp-verify-SECRETTOKEN', {now});
             INSERT INTO org_api_keys (id, org_id, name, key_hash, last4, scopes, created_by_member, created_by_account, created_at)
                 VALUES ('ok_1', '{org}', 'CI', x'0102', 'wxyz', '[\"messages:send\"]', 'm1', '{OWNER_ACCOUNT}', {now});"
        ),
    )
    .await;
    let (s, v) = bo
        .srv
        .get(&format!("/v1/staff/orgs/{org}"), &sup.strong)
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["api_keys"][0]["masked"], "pk_org_••••wxyz");
    assert_eq!(v["data"]["api_keys"][0]["scopes"], json!(["messages:send"]));
    assert_eq!(v["data"]["domains"][0]["domain"], "masked.example");
    assert!(!v.to_string().contains("SECRETTOKEN"));
    assert!(!v.to_string().contains("key_hash"));
    let row = last_audit(&bo.srv, &sup.account).await;
    assert_eq!(row.target.as_deref(), Some(format!("org:{org}").as_str()));
}

#[tokio::test]
async fn the_audit_reader_filters_and_pages_by_id() {
    let bo = setup().await;
    for i in 0..5 {
        bo.srv
            .state
            .store
            .append_audit(bo.srv.state.auditor.row(
                audit::ActorKind::System,
                Some(format!("acct_x{}", i % 2)),
                if i < 3 {
                    "nest.create"
                } else {
                    "staff.accounts.read"
                },
                Some(format!("account:acct_t{i}")),
                json!({ "n": i }),
                None,
                None,
                1_800_000_000 + i,
            ))
            .await
            .unwrap();
    }
    let get = |q: &str| {
        let path = format!("/v1/staff/audit?{q}");
        let t = bo.owner.strong.clone();
        let srv = &bo.srv;
        async move { srv.get(&path, &t).await }
    };
    let (_, v) = get("actor=acct_x0&action=nest.create").await;
    assert_eq!(v["data"].as_array().unwrap().len(), 2, "{v}");
    let (_, v) = get("target=account:acct_t4").await;
    assert_eq!(v["data"][0]["details"], json!({ "n": 4 }));
    let (_, v) = get("target=account:&from=1800000003&to=1800000004").await;
    assert_eq!(v["data"].as_array().unwrap().len(), 2, "{v}");
    let (_, v) = get("action=nest.&limit=2").await;
    assert_eq!(v["data"].as_array().unwrap().len(), 2);
    let next = v["next_cursor"].as_str().unwrap().to_string();
    let (_, v2) = get(&format!("action=nest.&limit=2&cursor={next}")).await;
    assert_eq!(v2["data"].as_array().unwrap().len(), 1);
    assert!(v2["next_cursor"].is_null());
    assert!(v["data"][1]["id"].as_i64() > v2["data"][0]["id"].as_i64());
    let (s, _) = get("from=yesterday").await;
    assert_eq!(s, 400);
    let (s, v) = get("export=1&limit=500").await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        last_audit(&bo.srv, OWNER_ACCOUNT).await.action,
        "staff.audit.export"
    );
}

// ---- the finance proxy --------------------------------------------------------------------------

#[tokio::test]
async fn finance_calls_saas_be_with_the_server_token_and_records_both_ends() {
    let bo = setup().await;
    let fin = bo.srv.person("bo_money").await;
    grant(&bo, &fin.account, "finance").await;

    let (s, v) = bo
        .srv
        .get(
            "/v1/staff/credits/owners/account/acct_bo_buyer/balance",
            &fin.strong,
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["available_cents"], 2500);
    let (_, v) = bo
        .srv
        .get(
            "/v1/staff/credits/owners/account/acct_bo_buyer/history?limit=10&cursor=c0",
            &fin.strong,
        )
        .await;
    assert_eq!(v["data"][0]["type"], "purchase");
    assert_eq!(v["next_cursor"], "c1");
    let (_, v) = bo
        .srv
        .get("/v1/staff/credits/sales?limit=2", &fin.strong)
        .await;
    assert_eq!(v["data"].as_array().unwrap().len(), 2);
    assert_eq!(v["next_cursor"], "o2");
    let (_, v) = bo
        .srv
        .get("/v1/staff/publishers/pub_x/payout-profile", &fin.strong)
        .await;
    assert!(
        !v.to_string().contains("TR000000"),
        "never the full IBAN: {v}"
    );
    assert!(v["data"]["iban"].as_str().unwrap().ends_with("1234"));

    let (s, v) = bo
        .srv
        .post(
            &format!("/v1/staff/credits/purchases/{PURCHASE}/refund"),
            &fin.strong,
            json!({ "reason": "charged twice" }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = bo
        .srv
        .post(
            &format!("/v1/staff/credits/refunds/{REFUND}/resolve"),
            &fin.strong,
            json!({ "outcome": "failed", "reason": "iyzico shows no refund", "provider_reference": "ref-9" }),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "finance_conflict"), "{v}");
    let (s, v) = bo
        .srv
        .post(
            "/v1/staff/credits/adjustments",
            &fin.strong,
            json!({ "owner_kind": "account", "owner_id": "acct_bo_buyer", "amount_cents": -500,
                    "reason": "goodwill reversal", "idempotency_key": "form-1" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");

    let seen = bo.seen.lock().unwrap().clone();
    for (_, _, auth, _) in &seen {
        assert_eq!(auth, &format!("Bearer {FINANCE_TOKEN}"));
    }
    let paths: Vec<_> = seen.iter().map(|(m, p, _, _)| format!("{m} {p}")).collect();
    assert_eq!(
        paths,
        vec![
            "GET /v1/internal/tenants/t1/marketplace/credits/balances/account/acct_bo_buyer".to_string(),
            "GET /v1/internal/tenants/t1/marketplace/credits/history/account/acct_bo_buyer?limit=10&cursor=c0".into(),
            "GET /v1/internal/tenants/t1/marketplace/finance/credit-sales".into(),
            "GET /v1/internal/tenants/t1/marketplace/finance/publishers/pub_x/payout-profile".into(),
            format!("POST /v1/internal/tenants/t1/marketplace/credits/purchases/{PURCHASE}/refund"),
            format!("POST /v1/internal/tenants/t1/marketplace/credits/refunds/{REFUND}/resolve"),
            "POST /v1/internal/tenants/t1/marketplace/credits/adjustments".into(),
        ]
    );
    let actor = format!("staff:{}", fin.account);
    assert_eq!(
        seen[4].3,
        json!({ "reason": "charged twice", "requested_by": actor })
    );
    assert_eq!(
        seen[5].3,
        json!({ "outcome": "failed", "reason": "iyzico shows no refund", "actor": actor, "provider_reference": "ref-9" })
    );
    assert_eq!(
        seen[6].3,
        json!({ "owner": { "owner_kind": "account", "owner_id": "acct_bo_buyer" }, "amount_cents": -500,
                "reason": "goodwill reversal", "actor": actor, "idempotency_key": crate::backoffice_finance::adjustment_key(&actor, "account", "acct_bo_buyer", -500, "form-1") })
    );

    // Each write: `requested`, then its outcome, both naming the target.
    let rows = bo
        .srv
        .state
        .store
        .audit_for_actor(Some(fin.account.clone()), 6)
        .await
        .unwrap();
    let got: Vec<_> = rows
        .iter()
        .rev()
        .map(|r| {
            let d: Value = serde_json::from_str(&r.details).unwrap();
            format!(
                "{} {} {}",
                r.action,
                r.target.clone().unwrap_or_default(),
                d["phase"].as_str().unwrap_or("")
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            format!("staff.credits.refund credit_purchase:{PURCHASE} requested"),
            format!("staff.credits.refund credit_purchase:{PURCHASE} completed"),
            format!("staff.credits.resolve credit_refund:{REFUND} requested"),
            format!("staff.credits.resolve credit_refund:{REFUND} refused"),
            "staff.credits.adjust account:acct_bo_buyer requested".to_string(),
            "staff.credits.adjust account:acct_bo_buyer completed".to_string(),
        ]
    );

    // Bad input never reaches saas_be.
    let n = bo.seen.lock().unwrap().len();
    for (path, body) in [
        (
            "/v1/staff/credits/purchases/not-a-uuid/refund".to_string(),
            json!({ "reason": "x" }),
        ),
        (
            format!("/v1/staff/credits/purchases/{PURCHASE}/refund"),
            json!({ "reason": "" }),
        ),
        (
            format!("/v1/staff/credits/purchases/{PURCHASE}/refund"),
            json!({ "reason": "x", "requested_by": "someone" }),
        ),
        (
            format!("/v1/staff/credits/refunds/{REFUND}/resolve"),
            json!({ "outcome": "maybe", "reason": "x" }),
        ),
        (
            "/v1/staff/credits/adjustments".to_string(),
            json!({ "owner_kind": "account", "owner_id": "a", "amount_cents": 0, "reason": "x", "idempotency_key": "k" }),
        ),
        (
            "/v1/staff/credits/adjustments".to_string(),
            json!({ "owner_kind": "account", "owner_id": "a", "amount_cents": 1_000_001, "reason": "x", "idempotency_key": "k" }),
        ),
        (
            "/v1/staff/credits/adjustments".to_string(),
            json!({ "owner_kind": "user", "owner_id": "a", "amount_cents": 5, "reason": "x", "idempotency_key": "k" }),
        ),
    ] {
        let (s, v) = bo.srv.post(&path, &fin.strong, body).await;
        assert!(s == 400 || s == 404, "{path}: {s} {v}");
    }
    assert_eq!(bo.seen.lock().unwrap().len(), n);
}

#[tokio::test]
async fn finance_without_configuration_says_so() {
    let srv = serve_with(|s| s.platform_owner = Some(Arc::from(OWNER_ACCOUNT))).await;
    let owner = srv.person("bo_owner").await;
    let fin = srv.person("bo_nofin").await;
    let (s, _) = srv
        .post(
            "/v1/staff/roles",
            &owner.strong,
            json!({ "account_id": fin.account, "role": "finance" }),
        )
        .await;
    assert_eq!(s, 201);
    let (s, v) = srv.get("/v1/staff/credits/refunds", &fin.strong).await;
    assert_eq!((s, code(&v)), (503, "finance_unavailable"), "{v}");
    let (_, v) = srv.get("/v1/staff/dashboard", &fin.token).await;
    assert_eq!(v["data"]["alerts"][0]["code"], "finance_unconfigured");
}

// ---- marketplace operations ---------------------------------------------------------------------

/// Listings, reports, kills, quotas, builds and hosted jobs are reviewers' and ops'; reviews and
/// release security stay reviewers' only.
#[tokio::test]
async fn ops_reaches_marketplace_operations_but_not_reviews() {
    let bo = setup().await;
    bo.srv.global_flag("marketplace").await;
    let ops = bo.srv.person("bo_ops").await;
    let rev = bo.srv.person("bo_rev").await;
    let fin = bo.srv.person("bo_mpfin").await;
    grant(&bo, &ops.account, "ops").await;
    grant(&bo, &rev.account, "reviewer").await;
    grant(&bo, &fin.account, "finance").await;
    for path in [
        "/v1/staff/listings",
        "/v1/staff/listing-reports",
        "/v1/staff/hosted-jobs",
    ] {
        for who in [&ops, &rev] {
            let (s, v) = bo.srv.get(path, &who.strong).await;
            assert_eq!(s, 200, "{path}: {v}");
        }
        let (s, v) = bo.srv.get(path, &fin.strong).await;
        assert_eq!((s, code(&v)), (403, "staff_required"), "{path}: {v}");
    }
    for path in ["/v1/staff/reviews", "/v1/staff/publisher-applications"] {
        let (s, v) = bo.srv.get(path, &ops.strong).await;
        assert_eq!((s, code(&v)), (403, "staff_required"), "{path}: {v}");
        let (s, v) = bo.srv.get(path, &rev.strong).await;
        assert_eq!(s, 200, "{path}: {v}");
    }
    let (s, v) = bo
        .srv
        .call(
            Method::PUT,
            "/v1/staff/marketplace/publishers/pub_none/quotas",
            Some(&fin.strong),
            Some(
                json!({ "builds_per_day": 1, "max_workers": 1, "monthly_broker_tokens": 1,
                         "jobs_per_minute": 1, "reason": "test" }),
            ),
        )
        .await;
    assert_eq!((s, code(&v)), (403, "staff_required"), "{v}");
}

// ---- the staff_roles rebuild --------------------------------------------------------------------

/// A database written by the previous binary (narrow CHECK, live and revoked rows), copied, then
/// opened by this one: every row survives, the new roles are accepted, the one-live-row rule
/// still holds, the original file is untouched and reopening changes nothing.
#[tokio::test]
async fn the_staff_roles_rebuild_keeps_every_row_of_an_existing_database() {
    let dir = std::env::temp_dir().join(format!("pp-bo-migrate-{}", rand_hex(6)));
    std::fs::create_dir_all(&dir).unwrap();
    let old = dir.join("old.db");
    {
        let c = rusqlite::Connection::open(&old).unwrap();
        c.execute_batch(store::marketplace::SCHEMA).unwrap();
        c.execute_batch(
            "INSERT INTO staff_roles (id, account_id, role, granted_by, granted_at) VALUES
                ('stf_a', 'acct_1', 'reviewer', 'acct_o', 10),
                ('stf_b', 'acct_1', 'finance', 'acct_o', 11),
                ('stf_c', 'acct_2', 'reviewer', 'acct_o', 12);
             UPDATE staff_roles SET revoked_by = 'acct_o', revoked_at = 20 WHERE id = 'stf_c';
             INSERT INTO staff_roles (id, account_id, role, granted_by, granted_at) VALUES
                ('stf_d', 'acct_2', 'reviewer', 'acct_o', 21);",
        )
        .unwrap();
        assert!(c
            .execute(
                "INSERT INTO staff_roles (id, account_id, role, granted_by, granted_at) VALUES ('x', 'a', 'ops', 'o', 1)",
                [],
            )
            .is_err(), "the old table refuses the new roles");
    }
    let copy = dir.join("copy.db");
    std::fs::copy(&old, &copy).unwrap();
    let dump = |path: &std::path::Path| -> Vec<(String, String, String, Option<i64>)> {
        let c = rusqlite::Connection::open(path).unwrap();
        let mut stmt = c
            .prepare("SELECT id, account_id, role, revoked_at FROM staff_roles ORDER BY id")
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows
    };
    let before = dump(&copy);
    assert_eq!(before.len(), 4);

    let s = store::Store::open(copy.to_str().unwrap()).unwrap();
    s.group_tx(|tx| -> Result<(), GErr> {
        for role in ["owner", "support", "ops"] {
            tx.execute(
                "INSERT INTO staff_roles (id, account_id, role, granted_by, granted_at) VALUES (?1, 'acct_3', ?2, 'acct_o', 30)",
                rusqlite::params![format!("stf_{role}"), role],
            )?;
        }
        assert!(tx
            .execute(
                "INSERT INTO staff_roles (id, account_id, role, granted_by, granted_at) VALUES ('dup', 'acct_1', 'reviewer', 'o', 1)",
                [],
            )
            .is_err(), "still one live row per account and role");
        assert!(tx
            .execute(
                "INSERT INTO staff_roles (id, account_id, role, granted_by, granted_at) VALUES ('bad', 'a', 'admin', 'o', 1)",
                [],
            )
            .is_err(), "still a CHECK");
        let mut roles = store::marketplace::live_staff_roles_in(tx, "acct_1")?;
        roles.sort();
        assert_eq!(roles, vec![policy::StaffRole::Finance, policy::StaffRole::Reviewer]);
        Ok(())
    })
    .await
    .unwrap();
    drop(s);
    let after = dump(&copy);
    assert_eq!(&after[..4], &before[..], "every old row, unchanged");
    assert_eq!(after.len(), 7);
    assert_eq!(dump(&old), before, "the original file was not touched");
    // Reopening is a no-op: the guard sees the wide table.
    let s = store::Store::open(copy.to_str().unwrap()).unwrap();
    drop(s);
    assert_eq!(dump(&copy), after);
    let _ = std::fs::remove_dir_all(&dir);
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The two back office routers and `docs/backoffice/contracts/pigeonpost-staff.v1.json` match
/// exactly, and each operation's `x-pigeonpost-roles` is what the policy grants its action.
#[test]
fn the_routes_the_contract_and_the_role_matrix_match() {
    use std::collections::BTreeSet;
    let served: BTreeSet<(String, String)> =
        crate::foundations_tests::routes_in(include_str!("backoffice.rs"), "pub(crate) fn router(")
            .into_iter()
            .chain(crate::foundations_tests::routes_in(
                include_str!("backoffice_finance.rs"),
                "pub(crate) fn router(",
            ))
            .collect();
    let spec: Value = serde_json::from_str(include_str!(
        "../../../docs/backoffice/contracts/pigeonpost-staff.v1.json"
    ))
    .unwrap();
    let action_of = |name: &str| match name {
        "StaffOverview" => policy::Action::StaffOverview,
        "StaffPeopleRead" => policy::Action::StaffPeopleRead,
        "StaffPlansRead" => policy::Action::StaffPlansRead,
        "StaffFinanceRead" => policy::Action::StaffFinanceRead,
        "StaffFinanceWrite" => policy::Action::StaffFinanceWrite,
        "StaffAuditRead" => policy::Action::StaffAuditRead,
        other => panic!("unknown back office action {other}"),
    };
    let mut documented = BTreeSet::new();
    for (path, item) in spec["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            assert_eq!(op["x-pigeonpost-status"], "implemented", "{method} {path}");
            let action = action_of(op["x-pigeonpost-action"].as_str().unwrap());
            let mut want: Vec<&str> = action.staff_roles().iter().map(|r| r.as_str()).collect();
            let mut got: Vec<&str> = op["x-pigeonpost-roles"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r.as_str().unwrap())
                .collect();
            want.sort();
            got.sort();
            assert_eq!(got, want, "{method} {path}: roles");
            documented.insert((method.clone(), path.clone()));
        }
    }
    assert!(served.len() >= 20, "parser found {served:?}");
    let missing: Vec<_> = served.difference(&documented).collect();
    let stale: Vec<_> = documented.difference(&served).collect();
    assert!(
        missing.is_empty(),
        "routes missing from the contract: {missing:?}"
    );
    assert!(
        stale.is_empty(),
        "contract operations with no route: {stale:?}"
    );
}

// ---- review fixes (security review of #291) ------------------------------------------------------

async fn finance_person(bo: &Bo, name: &str) -> Person {
    let p = bo.srv.person(name).await;
    grant(bo, &p.account, "finance").await;
    p
}

/// `i64::MIN` has no absolute value; it must be refused like any other out-of-range amount, and
/// never reach saas_be.
#[tokio::test]
async fn an_adjustment_of_i64_min_is_refused() {
    let bo = setup().await;
    let fin = finance_person(&bo, "bo_min").await;
    for cents in [i64::MIN, i64::MAX, -1_000_001, 1_000_001] {
        let (s, v) = bo
            .srv
            .post(
                "/v1/staff/credits/adjustments",
                &fin.strong,
                json!({ "owner_kind": "account", "owner_id": "acct_bo_buyer", "amount_cents": cents,
                        "reason": "x", "idempotency_key": "k1" }),
            )
            .await;
        assert_eq!((s, code(&v)), (400, "invalid_request"), "{cents}: {v}");
    }
    let (s, _) = bo
        .srv
        .post(
            "/v1/staff/credits/adjustments",
            &fin.strong,
            json!({ "owner_kind": "account", "owner_id": "acct_bo_buyer", "amount_cents": -1_000_000,
                    "reason": "x", "idempotency_key": "k1" }),
        )
        .await;
    assert_eq!(s, 201, "the limit itself is allowed");
    assert_eq!(
        bo.seen.lock().unwrap().len(),
        1,
        "only the valid one reached saas_be"
    );
}

/// The key saas_be sees binds the staff member, the owner, the amount and the form's key: a
/// reused form key with another payload is a new adjustment there, never a replay.
#[tokio::test]
async fn a_reused_form_key_never_replays_another_adjustment() {
    let bo = setup().await;
    let a = finance_person(&bo, "bo_keya").await;
    let b = finance_person(&bo, "bo_keyb").await;
    let adjust = |who: &Person, cents: i64, owner: &str| {
        let body = json!({ "owner_kind": "account", "owner_id": owner, "amount_cents": cents,
                           "reason": "x", "idempotency_key": "form-1" });
        let (srv, t) = (&bo.srv, who.strong.clone());
        async move { srv.post("/v1/staff/credits/adjustments", &t, body).await }
    };
    for (who, cents, owner) in [
        (&a, 500, "acct_bo_buyer"),
        (&a, 500, "acct_bo_buyer"),
        (&a, 700, "acct_bo_buyer"),
        (&a, 500, "acct_bo_other"),
        (&b, 500, "acct_bo_buyer"),
    ] {
        let (s, v) = adjust(who, cents, owner).await;
        assert_eq!(s, 201, "{v}");
    }
    let keys: Vec<String> = bo
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|(_, _, _, b)| b["idempotency_key"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(keys.len(), 5);
    assert_eq!(
        keys[0], keys[1],
        "the same adjustment resubmitted is one adjustment"
    );
    let distinct: std::collections::BTreeSet<_> = keys.iter().collect();
    assert_eq!(distinct.len(), 4, "{keys:?}");
    for k in &keys {
        assert!(k.starts_with("bo:") && !k.contains("form-1"), "{k}");
    }
}

/// Account detail and handle history show records about that account or namespace only, and
/// other staff members' `staff.*` records only to `owner` (who reads the audit log anyway).
#[tokio::test]
async fn detail_audit_trails_hide_staff_records_from_non_owners() {
    let bo = setup().await;
    seed(&bo.srv).await;
    let sup = bo.srv.person("bo_trail_sup").await;
    grant(&bo, &sup.account, "support").await;
    let own = bo.srv.person("bo_trail_own").await;
    grant(&bo, &own.account, "owner").await;
    for (actor, action, target) in [
        (
            "acct_other_staff",
            "staff.credits.adjust",
            "account:acct_bo_buyer",
        ),
        ("acct_bo_buyer", "phone.verified", "account:acct_bo_buyer"),
        ("acct_bo_buyer", "nest.create", "nest:nst_1"),
        (
            "acct_other_staff",
            "staff.handles.read",
            "namespace:shopkeep",
        ),
        ("acct_ops", "namespace.grant", "namespace:shopkeep"),
    ] {
        bo.srv
            .state
            .store
            .append_audit(bo.srv.state.auditor.row(
                audit::ActorKind::Human,
                Some(actor.into()),
                action,
                Some(target.into()),
                json!({}),
                None,
                None,
                now_unix(),
            ))
            .await
            .unwrap();
    }
    let actions = |v: &Value, key: &str| -> Vec<String> {
        v["data"][key]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["action"].as_str().unwrap().to_string())
            .collect()
    };
    let (_, v) = bo
        .srv
        .get("/v1/staff/accounts/acct_bo_buyer", &sup.strong)
        .await;
    assert_eq!(actions(&v, "recent_audit"), vec!["phone.verified"], "{v}");
    let (_, v) = bo.srv.get("/v1/staff/handles/shopkeep", &sup.strong).await;
    assert_eq!(actions(&v, "history"), vec!["namespace.grant"]);
    let (_, v) = bo
        .srv
        .get("/v1/staff/accounts/acct_bo_buyer", &own.strong)
        .await;
    let got = actions(&v, "recent_audit");
    assert!(got.contains(&"staff.credits.adjust".to_string()), "{got:?}");
    assert!(
        !got.contains(&"nest.create".to_string()),
        "only records about this account"
    );
    let (_, v) = bo.srv.get("/v1/staff/handles/shopkeep", &own.strong).await;
    assert!(actions(&v, "history").contains(&"staff.handles.read".to_string()));
}

/// `%` and `_` in a namespace are literal: `/shop_keep` does not see `/shopxkeep`'s mailboxes.
#[tokio::test]
async fn handle_detail_matches_its_own_namespace_only() {
    let bo = setup().await;
    let sup = bo.srv.person("bo_ns_sup").await;
    grant(&bo, &sup.account, "support").await;
    let now = now_unix();
    sql(
        &bo.srv,
        format!(
            "INSERT INTO namespaces (namespace, account_id, source, verified_at) VALUES ('shop_keep', 'acct_x', 'grant', {now}),
                                                                                  ('shopxkeep', 'acct_y', 'grant', {now});
             INSERT INTO identities (address, ed25519_pub, x25519_pub, wrapped_nonce, wrapped_ct, cap_hash, created_at, account_id, handle)
                 VALUES ('/k/ns1', x'00', x'00', x'00', x'00', x'00', {now}, 'acct_x', '/shop_keep/a'),
                        ('/k/ns2', x'00', x'00', x'00', x'00', x'00', {now}, 'acct_y', '/shopxkeep/b'),
                        ('/k/ns3', x'00', x'00', x'00', x'00', x'00', {now}, 'acct_y', '/shop_keeper');"
        ),
    )
    .await;
    let (s, v) = bo.srv.get("/v1/staff/handles/shop_keep", &sup.strong).await;
    assert_eq!(s, 200, "{v}");
    let handles: Vec<_> = v["data"]["mailboxes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["handle"].clone())
        .collect();
    assert_eq!(handles, vec![json!("/shop_keep/a")]);
    let (_, v) = bo.srv.get("/v1/staff/handles?q=shop", &sup.strong).await;
    for row in v["data"].as_array().unwrap() {
        assert_eq!(row["mailboxes"], 1, "{row}");
    }
}

/// A client that hangs up after the `requested` row does not lose the outcome row.
#[tokio::test]
async fn a_finance_write_records_its_outcome_after_the_client_left() {
    let bo = setup().await;
    let fin = finance_person(&bo, "bo_gone").await;
    let impatient = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(150))
        .build()
        .unwrap();
    let r = impatient
        .post(format!(
            "{}/v1/staff/credits/purchases/{SLOW_PURCHASE}/refund",
            bo.srv.public
        ))
        .bearer_auth(&fin.strong)
        .json(&json!({ "reason": "late" }))
        .send()
        .await;
    assert!(r.is_err(), "the client gave up first");
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let rows = bo
        .srv
        .state
        .store
        .audit_for_actor(Some(fin.account.clone()), 2)
        .await
        .unwrap();
    let phases: Vec<String> = rows
        .iter()
        .rev()
        .map(|r| {
            serde_json::from_str::<Value>(&r.details).unwrap()["phase"]
                .as_str()
                .unwrap_or("")
                .to_string()
        })
        .collect();
    assert_eq!(phases, vec!["requested", "completed"]);
}

/// The rebuild takes the write lock before it looks again: while another connection holds it,
/// it waits (here: fails busy) instead of rebuilding under that writer; afterwards it runs once.
#[test]
fn the_staff_roles_rebuild_takes_the_write_lock_first() {
    let dir = std::env::temp_dir().join(format!("pp-bo-lock-{}", rand_hex(6)));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("db.sqlite");
    let a = rusqlite::Connection::open(&path).unwrap();
    a.execute_batch(store::marketplace::SCHEMA).unwrap();
    a.execute_batch("INSERT INTO staff_roles (id, account_id, role, granted_by, granted_at) VALUES ('s1', 'a', 'reviewer', 'o', 1);").unwrap();
    let b = rusqlite::Connection::open(&path).unwrap();
    b.pragma_update(None, "busy_timeout", 0).unwrap();
    a.execute_batch("BEGIN IMMEDIATE").unwrap();
    let busy = store::marketplace::migrate_staff_roles(&b);
    assert!(
        busy.is_err(),
        "no rebuild while another writer holds the lock"
    );
    a.execute_batch("COMMIT").unwrap();
    store::marketplace::migrate_staff_roles(&b).unwrap();
    store::marketplace::migrate_staff_roles(&a).unwrap();
    let n: i64 = a
        .query_row("SELECT COUNT(*) FROM staff_roles", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
    a.execute("INSERT INTO staff_roles (id, account_id, role, granted_by, granted_at) VALUES ('s2', 'a', 'ops', 'o', 1)", []).unwrap();
    drop((a, b));
    let _ = std::fs::remove_dir_all(&dir);
}
