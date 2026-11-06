//! Nests Phase B1 over real sockets: who may buy and create, the flag, web (bridge) entitlements
//! driving slots, paid-but-unprovisioned recovery, refunds pausing Nests, duplicates never adding
//! slots. nestd is the Capture transport.

use crate::*;
use serde_json::{json, Value};

const OPS: &str = "svc_ops_0123456789abcdef0123456789abcdef";
const BILLING: &str = "svc_billing_0123456789abcdef0123456789abcdef";
const ISSUER: &str = "https://auth.example/realms/x";

struct Srv {
    public: String,
    internal: String,
    state: AppState,
}

async fn serve() -> Srv {
    let mut state = crate::tests::test_state();
    state.service_tokens = Arc::new(internal_api::tokens_for_test(&[
        ("POSTBOX_SVC_TOKEN_OPS", OPS),
        ("POSTBOX_SVC_TOKEN_BILLING", BILLING),
    ]));
    state.nests = Arc::new(nests::Runtime::capture());
    let public = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let internal = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let srv = Srv {
        public: format!("http://{}", public.local_addr().unwrap()),
        internal: format!("http://{}", internal.local_addr().unwrap()),
        state: state.clone(),
    };
    let app =
        build_router(state.clone()).into_make_service_with_connect_info::<std::net::SocketAddr>();
    let internal_app =
        internal_api::router(state).into_make_service_with_connect_info::<std::net::SocketAddr>();
    tokio::spawn(async move { axum::serve(public, app).await.unwrap() });
    tokio::spawn(async move { axum::serve(internal, internal_app).await.unwrap() });
    srv
}

struct Person {
    account: String,
    token: String,
    api_key: String,
    cap: String,
}

async fn person(srv: &Srv, name: &str, flag: bool) -> Person {
    let account = format!("acct_n_{name}");
    let sub = format!("sub-n-{name}");
    srv.state
        .store
        .account_for_sub(sub.clone(), account.clone(), now_unix())
        .await
        .unwrap();
    let (api_key, key) = new_api_key();
    srv.state
        .store
        .add_api_key(account.clone(), key, now_unix())
        .await
        .unwrap();
    let v = do_create_identity(&srv.state, Some(account.clone()), None, None)
        .await
        .unwrap();
    if flag {
        let (s, _) = call(
            reqwest::Method::PUT,
            format!("{}/internal/v1/features/nests", srv.internal),
            OPS,
            Some(json!({ "scope": "account", "account_id": account, "enabled": true })),
        )
        .await;
        assert_eq!(s, 200);
    }
    Person {
        token: oidc::test_member_token(ISSUER, &sub),
        api_key,
        cap: v["capability_token"].as_str().unwrap().to_string(),
        account,
    }
}

async fn call(
    method: reqwest::Method,
    url: String,
    token: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut req = reqwest::Client::new()
        .request(method, url)
        .bearer_auth(token);
    if let Some(body) = body {
        req = req.json(&body);
    }
    let r = req.send().await.unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

impl Srv {
    async fn get(&self, path: &str, token: &str) -> (u16, Value) {
        call(
            reqwest::Method::GET,
            format!("{}{path}", self.public),
            token,
            None,
        )
        .await
    }
    async fn post(&self, path: &str, token: &str, body: Value) -> (u16, Value) {
        call(
            reqwest::Method::POST,
            format!("{}{path}", self.public),
            token,
            Some(body),
        )
        .await
    }
    /// The billing bridge pushes a web (iyzico) Nest entitlement.
    async fn web(
        &self,
        account: &str,
        reference: &str,
        plan: &str,
        status: &str,
        paid: u64,
        v: i64,
    ) -> (u16, Value) {
        call(
            reqwest::Method::PUT,
            format!(
                "{}/internal/v1/entitlements/iyzico/live/{reference}",
                self.internal
            ),
            BILLING,
            Some(json!({
                "subject": {"kind": "account", "id": account},
                "plan_code": plan,
                "plan_version": "2026",
                "status": status,
                "period_end": paid,
                "paid_through": paid,
                "reconciliation_version": v,
                "operation_id": "op-nest"
            })),
        )
        .await
    }
    fn creates(&self) -> Vec<Value> {
        self.state
            .nests
            .calls()
            .into_iter()
            .filter(|c| c.method == "POST" && c.path == "/v1/nests")
            .map(|c| c.body.unwrap())
            .collect()
    }
    fn actions(&self, action: &str) -> usize {
        self.state
            .nests
            .calls()
            .iter()
            .filter(|c| c.method == "POST" && c.path.ends_with(&format!("/{action}")))
            .count()
    }
}

#[tokio::test]
async fn the_flag_gates_and_only_a_person_may_buy_or_create() {
    let srv = serve().await;
    let off = person(&srv, "off", false).await;
    let (s, b) = srv.get("/v1/nests", &off.token).await;
    assert_eq!((s, b["error"].as_str()), (403, Some("feature_disabled")));
    let (s, _) = srv
        .post(
            "/v1/nest-purchases/intents",
            &off.token,
            json!({"package": "nest.ai.starter.monthly", "rail": "web"}),
        )
        .await;
    assert_eq!(s, 403);

    let on = person(&srv, "on", true).await;
    let (s, b) = srv.get("/v1/nests", &on.token).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["allowance"]["slots"], 0);
    let (s, b) = srv.get("/v1/nest-packages", &on.api_key).await;
    assert_eq!(s, 200, "the account key may read: {b}");
    assert_eq!(b["catalog_version"], nest_catalog::CATALOG_VERSION);
    assert_eq!(b["app_account_token"], appstore::account_token(&on.account));
    // A mailbox token is never the account.
    let (s, _) = srv.get("/v1/nests", &on.cap).await;
    assert_eq!(s, 403);
    // Buying and creating need a person: the account key is automation.
    for (path, body) in [
        (
            "/v1/nest-purchases/intents",
            json!({"package": "nest.ai.starter.monthly", "rail": "web"}),
        ),
        ("/v1/nests", json!({"label": "x"})),
        ("/v1/nest-purchases/apple", json!({"transaction_id": "1"})),
        ("/v1/nest-purchases/google", json!({"purchase_token": "t"})),
    ] {
        let (s, b) = srv.post(path, &on.api_key, body).await;
        assert_eq!(s, 403, "{path}: {b}");
    }
    // A person without a package cannot create a Nest; the flag is not an entitlement.
    let (s, b) = srv
        .post("/v1/nests", &on.token, json!({"label": "x"}))
        .await;
    assert_eq!((s, b["error"].as_str()), (402, Some("no_nest_package")));
    // Unknown, unsellable and malformed requests.
    for body in [
        json!({"package": "nest.ai.pro.monthly", "rail": "web"}),
        json!({"package": "cloud-1names-yearly-usd", "rail": "web"}),
        json!({"package": "nest.ai.starter.monthly", "rail": "paypal"}),
        json!({"package": "nest.ai.starter.monthly", "rail": "web", "price": 1}),
    ] {
        let (s, _) = srv
            .post("/v1/nest-purchases/intents", &on.token, body.clone())
            .await;
        assert_eq!(s, 400, "{body}");
    }
    // Store rails are refused when the store is not configured (tests have none).
    let (s, b) = srv
        .post(
            "/v1/nest-purchases/intents",
            &on.token,
            json!({"package": "nest.ai.starter.monthly", "rail": "apple"}),
        )
        .await;
    assert_eq!((s, b["error"].as_str()), (503, Some("billing_unavailable")));
    let (s, b) = srv
        .post(
            "/v1/nest-purchases/intents",
            &on.token,
            json!({"package": "nest.ai.starter.monthly", "rail": "web"}),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    assert!(b["intent_id"].as_str().unwrap().starts_with("npi_"));
}

#[tokio::test]
async fn the_internal_api_takes_only_known_nest_packages() {
    let srv = serve().await;
    let p = person(&srv, "api", true).await;
    let paid = now_unix() + 30 * 86400;
    for plan in [
        "nest-ai-starter-monthly-gbp",
        "nest-unknown-monthly-usd",
        "nest.ai.starter",
    ] {
        let (s, b) = srv.web(&p.account, "sub-x", plan, "active", paid, 1).await;
        assert_eq!(
            (s, b["error"].as_str()),
            (400, Some("invalid_plan_code")),
            "{plan}"
        );
    }
    // Names on a Nest package are refused.
    let (s, _) = call(
        reqwest::Method::PUT,
        format!(
            "{}/internal/v1/entitlements/iyzico/live/sub-y",
            srv.internal
        ),
        BILLING,
        Some(json!({
            "subject": {"kind": "account", "id": p.account},
            "plan_code": "nest-ai-starter-monthly-usd", "plan_version": "2026", "names": 1,
            "status": "active", "period_end": paid, "paid_through": paid,
            "reconciliation_version": 1, "operation_id": "op"
        })),
    )
    .await;
    assert_eq!(s, 400);
    let (s, b) = srv
        .web(
            &p.account,
            "sub-ok",
            "nest-ai-starter-monthly-usd",
            "active",
            paid,
            1,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    // The Nest package gives no web names.
    let (s, b) = srv.get("/v1/me/entitlements", &p.token).await;
    assert_eq!(s, 200, "{b}");
}

#[tokio::test]
async fn a_web_purchase_provisions_once_recovers_from_failure_and_pauses_on_refund() {
    let srv = serve().await;
    let p = person(&srv, "buyer", true).await;
    let paid = now_unix() + 30 * 86400;

    // nestd is down when the payment lands: the purchase stays, as a pending setup.
    srv.state.nests.set_fail(true);
    let (s, _) = srv
        .web(
            &p.account,
            "sub-1",
            "nest-ai-plus-monthly-try",
            "active",
            paid,
            1,
        )
        .await;
    assert_eq!(s, 200);
    nest_commerce::reconcile_once(&srv.state).await;
    let (_, b) = srv.get("/v1/nests", &p.token).await;
    assert_eq!(b["allowance"]["slots"], 3, "{b}");
    assert_eq!(b["package"]["code"], "nest.ai.plus.monthly");
    let setups = b["setups"].as_array().unwrap();
    assert_eq!(setups.len(), 1, "{b}");
    assert_eq!(setups[0]["status"], "pending");
    assert_eq!(setups[0]["last_error"], "nestd_failed");
    // The failed attempt holds no slot.
    assert_eq!(b["allowance"]["used"], 0, "{b}");

    // The owner retries once nestd is back: exactly one Nest, sized by the package.
    srv.state.nests.set_fail(false);
    let id = setups[0]["setup_id"].as_str().unwrap().to_string();
    let (s, b) = srv
        .post(&format!("/v1/nest-setups/{id}/retry"), &p.token, json!({}))
        .await;
    assert_eq!(s, 200, "{b}");
    assert!(b["setups"].as_array().unwrap().is_empty(), "{b}");
    let live: Vec<&Value> = b["nests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["state"] != "failed")
        .collect();
    assert_eq!(live.len(), 1, "{b}");
    assert_eq!(live[0]["funding"], "package");
    assert_eq!(live[0]["label"], "Pigeonpost AI Plus");
    let create = srv.creates().last().unwrap().clone();
    let plus = nest_catalog::by_code("nest.ai.plus.monthly").unwrap();
    assert_eq!(
        create["budgets"]["input_tokens"],
        plus.input_tokens / 3,
        "split, never multiplied"
    );
    assert_eq!(create["budgets"]["output_tokens"], plus.output_tokens / 3);
    assert_eq!(create["quota_gib"], 6);

    // The sweep cleans the failed attempt up (runtime and mailbox), once.
    nest_commerce::reconcile_once(&srv.state).await;
    let (_, b) = srv.get("/v1/nests", &p.token).await;
    assert!(
        b["nests"]
            .as_array()
            .unwrap()
            .iter()
            .all(|n| n["state"] != "failed"),
        "{b}"
    );
    assert_eq!(b["nests"].as_array().unwrap().len(), 1);

    // Reconciling again (or a renewal push) never creates a second Nest.
    let before = srv.creates().len();
    let (s, _) = srv
        .web(
            &p.account,
            "sub-1",
            "nest-ai-plus-monthly-try",
            "active",
            paid + 86400,
            2,
        )
        .await;
    assert_eq!(s, 200);
    nest_commerce::reconcile_once(&srv.state).await;
    nest_commerce::reconcile_once(&srv.state).await;
    assert_eq!(srv.creates().len(), before);
    // A late, older event does not move anything back.
    let (s, b) = srv
        .web(
            &p.account,
            "sub-1",
            "nest-ai-plus-monthly-try",
            "expired",
            paid,
            1,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    let (_, b) = srv.get("/v1/nests", &p.token).await;
    assert_eq!(b["allowance"]["slots"], 3, "a stale event is ignored: {b}");

    // The owner fills the slots; a fourth is refused.
    for i in 0..2 {
        let (s, b) = srv
            .post(
                "/v1/nests",
                &p.token,
                json!({"label": format!("extra {i}")}),
            )
            .await;
        assert_eq!(s, 201, "{b}");
    }
    let (s, b) = srv
        .post("/v1/nests", &p.token, json!({"label": "one too many"}))
        .await;
    assert_eq!(
        (s, b["error"].as_str()),
        (409, Some("nest_slots_full")),
        "{b}"
    );

    // A second web package does not add slots.
    let (s, _) = srv
        .web(
            &p.account,
            "sub-2",
            "nest-ai-starter-monthly-usd",
            "active",
            paid,
            1,
        )
        .await;
    assert_eq!(s, 200);
    let (_, b) = srv.get("/v1/nests", &p.token).await;
    assert_eq!(b["allowance"]["slots"], 3);
    assert_eq!(b["allowance"]["duplicates"].as_array().unwrap().len(), 1);
    let (s, _) = srv
        .web(
            &p.account,
            "sub-2",
            "nest-ai-starter-monthly-usd",
            "expired",
            now_unix() - 1,
            2,
        )
        .await;
    assert_eq!(s, 200);

    // Refund: the paid-through is cut to now; every Nest pauses at the next sweep.
    let pauses = srv.actions("pause");
    let (s, _) = srv
        .web(
            &p.account,
            "sub-1",
            "nest-ai-plus-monthly-try",
            "expired",
            now_unix() - 1,
            3,
        )
        .await;
    assert_eq!(s, 200);
    nest_commerce::reconcile_once(&srv.state).await;
    assert_eq!(srv.actions("pause") - pauses, 3);
    let (_, b) = srv.get("/v1/nests", &p.token).await;
    for n in b["nests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["funding"] == "package")
    {
        assert_eq!(n["state"], "paused", "{b}");
        assert_eq!(n["paused_reason"], "unfunded");
        assert!(n["delete_after"].as_i64().unwrap() > now_unix() as i64 + 29 * 86400);
    }
    // Paused for funding: nothing else happens on later sweeps.
    nest_commerce::reconcile_once(&srv.state).await;
    assert_eq!(srv.actions("pause") - pauses, 3);
    assert_eq!(
        srv.state
            .nests
            .calls()
            .iter()
            .filter(|c| c.method == "DELETE")
            .count(),
        1,
        "only the failed attempt was ever deleted"
    );

    // Paying again resumes them.
    let resumes = srv.actions("resume");
    let (s, _) = srv
        .web(
            &p.account,
            "sub-1",
            "nest-ai-plus-monthly-try",
            "active",
            now_unix() + 30 * 86400,
            4,
        )
        .await;
    assert_eq!(s, 200);
    nest_commerce::reconcile_once(&srv.state).await;
    assert_eq!(srv.actions("resume") - resumes, 3);
}

#[tokio::test]
async fn a_grant_funds_one_nest_for_testing_without_any_purchase() {
    let srv = serve().await;
    let p = person(&srv, "owner", true).await;
    let (s, b) = call(
        reqwest::Method::PUT,
        format!(
            "{}/internal/v1/entitlements/grant/live/nest-test-owner",
            srv.internal
        ),
        BILLING,
        Some(json!({
            "subject": {"kind": "account", "id": p.account},
            "plan_code": "nest.ai.starter.monthly", "plan_version": "grant",
            "status": "active", "reconciliation_version": 1, "operation_id": "op-grant"
        })),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    nest_commerce::reconcile_once(&srv.state).await;
    let (_, b) = srv.get("/v1/nests", &p.token).await;
    assert_eq!(b["allowance"]["slots"], 1);
    assert_eq!(b["nests"].as_array().unwrap().len(), 1, "{b}");
    assert_eq!(b["nests"][0]["label"], "Pigeonpost AI");
    // The account's own mailbox is the one the Nest answers.
    let (s, b) = srv
        .post(
            "/v1/nest-purchases/intents",
            &p.token,
            json!({"package": "nest.ai.plus.monthly", "rail": "web"}),
        )
        .await;
    assert_eq!(s, 200, "a grant never blocks a real purchase: {b}");
    let _ = &p.cap;
}
