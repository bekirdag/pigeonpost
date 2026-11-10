//! Marketplace payments over real sockets, with a stand-in for the billing bridge's internal
//! routes: prices mirrored as offers (and the `nearest` refusal), purchases, paid installs that
//! consume exactly the purchase's intent, and licence projection (pause, resume, deletion
//! boundary). Synthetic accounts, tokens and amounts only.

use crate::marketplace_m2_tests::{code, publish, published_with_schema, setup as m2_setup};
use crate::org_tests::{serve_with, Person, Srv, BILLING, OPS};
use crate::*;
use axum::extract::{Path as AxPath, State as AxState};
use axum::http::HeaderMap as AxHeaders;
use reqwest::Method;
use serde_json::{json, Value};
use std::sync::Mutex;

const BRIDGE_TOKEN: &str = "bridge-internal-fixture-token-0123456789";
const LICENCE: &str = "00000000-0000-4000-8000-000000000703";

/// What the bridge stand-in saw and how it answers.
#[derive(Default)]
struct Mock {
    offers: Vec<(String, Value)>,
    consumes: Vec<Value>,
    paid: bool,
    /// The installation each consumed reference was first bound to (saas_be's single use).
    consumed: Option<String>,
    /// Refuse every consume as bound to another intent.
    mismatch: bool,
}

type Shared = Arc<Mutex<Mock>>;

fn authed(h: &AxHeaders) -> bool {
    h.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {BRIDGE_TOKEN}"))
}

async fn offer(
    AxState(m): AxState<Shared>,
    h: AxHeaders,
    AxPath(id): AxPath<String>,
    axum::Json(b): axum::Json<Value>,
) -> (axum::http::StatusCode, axum::Json<Value>) {
    use axum::http::StatusCode as S;
    if !authed(&h) {
        return (
            S::UNAUTHORIZED,
            axum::Json(json!({ "code": "invalid_service_token" })),
        );
    }
    m.lock().unwrap().offers.push((id.clone(), b.clone()));
    let price = b["price_minor"].as_i64().unwrap_or(0);
    if price % 7 == 0 {
        return (
            S::UNPROCESSABLE_ENTITY,
            axum::Json(
                json!({ "error": "cannot be invoiced", "code": "price_not_invoiceable",
                               "nearest": [price - 1, price + 1] }),
            ),
        );
    }
    let status = b["status"].as_str().unwrap_or("active").to_string();
    (
        S::OK,
        axum::Json(json!({ "offer": {
            "offer_id": id, "price_minor": price, "term": b["term"], "currency": b["currency"],
            "status": status, "sellable": status == "active",
        }})),
    )
}

async fn consume(
    AxState(m): AxState<Shared>,
    h: AxHeaders,
    axum::Json(b): axum::Json<Value>,
) -> (axum::http::StatusCode, axum::Json<Value>) {
    use axum::http::StatusCode as S;
    if !authed(&h) {
        return (
            S::UNAUTHORIZED,
            axum::Json(json!({ "code": "invalid_service_token" })),
        );
    }
    let mut m = m.lock().unwrap();
    m.consumes.push(b.clone());
    if m.mismatch {
        return (
            S::CONFLICT,
            axum::Json(json!({ "error": "other intent", "code": "intent_mismatch" })),
        );
    }
    if !m.paid {
        return (
            S::PAYMENT_REQUIRED,
            axum::Json(json!({ "error": "not paid", "code": "payment_pending" })),
        );
    }
    let reference = b["consumer_reference"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let replay = match &m.consumed {
        Some(r) if *r == reference => true,
        Some(_) => {
            return (
                S::CONFLICT,
                axum::Json(json!({ "error": "used", "code": "authorization_used" })),
            )
        }
        None => false,
    };
    m.consumed = Some(reference);
    let i = &b["intent"];
    (
        S::OK,
        axum::Json(json!({ "replay": replay, "licence": {
            "licence_id": LICENCE, "listing_id": i["listing_id"], "owner_kind": i["owner_kind"],
            "owner_id": i["owner_id"], "term": "monthly", "status": "active", "access": true,
            "paid_through": "2030-01-01T00:00:00Z", "cancel_at_period_end": false,
            "deletion_due_at": null, "revoked_at": null,
        }})),
    )
}

async fn bridge() -> (String, Shared) {
    let m: Shared = Arc::new(Mutex::new(Mock::default()));
    let app = axum::Router::new()
        .route(
            "/internal/marketplace/offers/{id}",
            axum::routing::put(offer),
        )
        .route(
            "/internal/marketplace/authorizations/consume",
            axum::routing::post(consume),
        )
        .with_state(m.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (url, m)
}

async fn setup() -> (Srv, Person, Shared) {
    let (url, m) = bridge().await;
    let srv = serve_with(|s| {
        s.platform_owner = Some(Arc::from("acct_mpowner"));
        s.nests = Arc::new(nests::Runtime::capture());
        s.mp_bridge = Arc::new(marketplace_pay::Bridge::http(&url, BRIDGE_TOKEN));
    })
    .await;
    srv.global_flag("marketplace").await;
    let owner = srv.person("mpowner").await;
    (srv, owner, m)
}

fn price_path(p: &str, l: &str) -> String {
    format!("/v1/publishers/{p}/listings/{l}/price")
}

async fn set_price(srv: &Srv, who: &Person, p: &str, l: &str, body: Value) -> (u16, Value) {
    srv.call(
        Method::PUT,
        &price_path(p, l),
        Some(&who.strong),
        Some(body),
    )
    .await
}

fn intent(n: u8) -> String {
    format!("{:032x}", n as u128 + 0xabc0)
}

const CFG: &str = r#"{ "team_name": "Ops", "api_token": "synthetic-secret-value-9" }"#;

async fn quote(srv: &Srv, who: &Person) -> (u16, Value) {
    srv.post(
        "/v1/nest-jobs/quote",
        &who.token,
        json!({ "to": "/acme/team-digest", "operation": "team.digest.ask", "operation_version": "1.0" }),
    )
    .await
}

/// A published listing with a monthly price that is on sale, and a buyer.
async fn priced(tag: &str) -> (Srv, Shared, Person, String, String, Person) {
    let (srv, owner, m) = setup().await;
    let (_, ann, p, _, l, _) = published_with_schema(&srv, &owner, tag).await;
    let (s, v) = set_price(
        &srv,
        &ann,
        &p,
        &l,
        json!({ "amount_minor": 12000, "currency": "TRY", "term": "monthly" }),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let bob = srv.person(&format!("mpbob{tag}")).await;
    (srv, m, ann, p, l, bob)
}

async fn buy(srv: &Srv, who: &Person, l: &str, n: u8) -> (u16, Value) {
    srv.post(
        "/v1/marketplace/purchases",
        &who.token,
        json!({ "listing_id": l, "owner_kind": "account", "owner_id": who.account, "intent_id": intent(n) }),
    )
    .await
}

async fn install(srv: &Srv, who: &Person, l: &str, purchase: Option<&str>) -> (u16, Value) {
    let mut body =
        json!({ "listing_id": l, "configuration": serde_json::from_str::<Value>(CFG).unwrap() });
    if let Some(p) = purchase {
        body["purchase_id"] = json!(p);
    }
    srv.post("/v1/nest-installations", &who.token, body).await
}

/// A paid, installed purchase: `(installation id, purchase intent)`.
async fn installed(srv: &Srv, m: &Shared, bob: &Person, l: &str) -> (String, Value) {
    let (s, v) = buy(srv, bob, l, 1).await;
    assert_eq!(s, 201, "{v}");
    let id = v["data"]["installation_id"].as_str().unwrap().to_string();
    m.lock().unwrap().paid = true;
    let (s, v2) = install(srv, bob, l, Some(&id)).await;
    assert_eq!(s, 201, "{v2}");
    (id, v["data"]["intent"].clone())
}

#[test]
fn release_lines_and_offer_ids_are_stable_and_change_with_any_immutable_field() {
    assert_eq!(marketplace_pay::release_line("1.4.2"), "v1");
    assert_eq!(marketplace_pay::release_line("12.0.0-rc.1"), "v12");
    let a = marketplace_pay::offer_id_for("lst_a", "pub_a", "v1", "one_off", "TRY", 100);
    assert_eq!(
        a,
        marketplace_pay::offer_id_for("lst_a", "pub_a", "v1", "one_off", "TRY", 100)
    );
    for b in [
        marketplace_pay::offer_id_for("lst_b", "pub_a", "v1", "one_off", "TRY", 100),
        marketplace_pay::offer_id_for("lst_a", "pub_a", "v2", "one_off", "TRY", 100),
        marketplace_pay::offer_id_for("lst_a", "pub_a", "v1", "monthly", "TRY", 100),
        marketplace_pay::offer_id_for("lst_a", "pub_a", "v1", "one_off", "USD", 100),
        marketplace_pay::offer_id_for("lst_a", "pub_a", "v1", "one_off", "TRY", 101),
    ] {
        assert_ne!(a, b);
    }
    assert!(a.starts_with("mpo_") && a.len() == 36);
}

/// A publisher's price is mirrored as an offer before it stands; a price that cannot be invoiced
/// exactly is refused with the nearest ones that can; a change is a new offer and the old one is
/// retired; the catalog shows the price.
#[tokio::test]
async fn prices_are_mirrored_as_offers_and_refusals_name_the_nearest_price() {
    let (srv, owner, m) = setup().await;
    let (_, ann, p, _, l, _) = published_with_schema(&srv, &owner, "p1").await;
    let (s, v) = set_price(
        &srv,
        &ann,
        &p,
        &l,
        json!({ "amount_minor": 12005, "currency": "TRY", "term": "one_off" }),
    )
    .await;
    assert_eq!((s, code(&v)), (422, "price_not_invoiceable"), "{v}");
    assert_eq!(v["error"]["details"]["nearest"], json!([12004, 12006]));
    let (s, v) = srv.get(&price_path(&p, &l), &ann.token).await;
    assert_eq!(s, 200, "{v}");
    assert!(v["data"]["price"].is_null(), "a refused price never stands");

    let (s, v) = set_price(
        &srv,
        &ann,
        &p,
        &l,
        json!({ "amount_minor": 12000, "currency": "TRY", "term": "one_off" }),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["offer"]["on_sale"], true);
    let first = v["data"]["offer"]["offer_id"].as_str().unwrap().to_string();
    {
        let m = m.lock().unwrap();
        let (id, body) = m.offers.last().unwrap();
        assert_eq!(id, &first);
        assert_eq!(body["listing_id"], l.as_str());
        assert_eq!(body["publisher_id"], p.as_str());
        assert_eq!(body["release_line"], "v1");
        assert_eq!(body["price_minor"], 12000);
        assert_eq!(body["status"], "active");
    }
    let (s, v) = srv
        .get(&format!("/v1/marketplace/listings/{l}"), &ann.token)
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["price"]["amount_minor"], 12000);
    assert_eq!(v["data"]["price"]["funding"], "card");

    // A new term is a new offer; the old one is retired with the same immutable fields.
    let (s, v) = set_price(
        &srv,
        &ann,
        &p,
        &l,
        json!({ "amount_minor": 12000, "currency": "TRY", "term": "monthly" }),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_ne!(v["data"]["offer"]["offer_id"], first.as_str());
    let retired = m
        .lock()
        .unwrap()
        .offers
        .iter()
        .any(|(id, b)| *id == first && b["status"] == "retired" && b["term"] == "one_off");
    assert!(retired);

    // Only the publisher's admins, with step-up; nothing wallet-like or out of range.
    let (s, _) = srv
        .call(
            Method::PUT,
            &price_path(&p, &l),
            Some(&ann.token),
            Some(json!({ "amount_minor": 100, "currency": "TRY", "term": "one_off" })),
        )
        .await;
    assert_eq!(s, 401, "no step-up");
    let eve = srv.person("mpevep1").await;
    let (s, _) = set_price(
        &srv,
        &eve,
        &p,
        &l,
        json!({ "amount_minor": 100, "currency": "TRY", "term": "one_off" }),
    )
    .await;
    assert_eq!(s, 404);
    for bad in [
        json!({ "amount_minor": 0, "currency": "TRY", "term": "one_off" }),
        json!({ "amount_minor": 100, "currency": "GBP", "term": "one_off" }),
        json!({ "amount_minor": 100, "currency": "TRY", "term": "per_job" }),
        json!({ "amount_minor": 100, "currency": "TRY", "term": "one_off", "wallet": true }),
    ] {
        let (s, v) = set_price(&srv, &ann, &p, &l, bad.clone()).await;
        assert_eq!(s, 400, "{bad}: {v}");
    }
    // Free again: the offer is retired, the catalog says test credits.
    let (s, v) = srv
        .call(Method::DELETE, &price_path(&p, &l), Some(&ann.strong), None)
        .await;
    assert_eq!(s, 200, "{v}");
    assert!(v["data"]["price"].is_null());
    let (_, v) = srv
        .get(&format!("/v1/marketplace/listings/{l}"), &ann.token)
        .await;
    assert_eq!(v["data"]["price"]["funding"], "test_credits");
    // Republishing a free listing mirrors nothing.
    let before = m.lock().unwrap().offers.len();
    let (s, _) = publish(&srv, &ann.strong, &p, &l, json!({})).await;
    assert_eq!(s, 200);
    assert_eq!(m.lock().unwrap().offers.len(), before);
}

/// Installing a priced listing needs a paid purchase of the caller's; the payment is consumed for
/// exactly the purchase's intent with the installation id as reference; a replay installs nothing
/// new; another buyer, an org owner or a refused intent get nothing.
#[tokio::test]
async fn a_paid_listing_installs_only_with_its_own_paid_purchase() {
    let (srv, m, _, _, l, bob) = priced("p2").await;
    let (s, v) = install(&srv, &bob, &l, None).await;
    assert_eq!((s, code(&v)), (402, "payment_required"), "{v}");
    assert_eq!(v["error"]["details"]["price"]["amount_minor"], 12000);

    let (s, v) = buy(&srv, &bob, &l, 1).await;
    assert_eq!(s, 201, "{v}");
    let ins = v["data"]["installation_id"].as_str().unwrap().to_string();
    let purchase_intent = v["data"]["intent"].clone();
    assert_eq!(
        purchase_intent,
        json!({ "action": "install", "listing_id": l, "release_line": "v1", "owner_kind": "account",
                "owner_id": bob.account, "installation_id": ins })
    );
    assert_eq!(v["data"]["offer"]["term"], "monthly");
    // The same intent again is the same purchase; another listing under it is refused.
    let (s, v) = buy(&srv, &bob, &l, 1).await;
    assert_eq!(
        (s, v["data"]["installation_id"].as_str()),
        (200, Some(ins.as_str()))
    );
    let (s, v) = srv
        .post(
            "/v1/marketplace/purchases",
            &bob.token,
            json!({ "listing_id": "lst_other", "owner_kind": "account", "owner_id": bob.account, "intent_id": intent(1) }),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "intent_conflict"), "{v}");
    let (s, v) = srv
        .post(
            "/v1/marketplace/purchases",
            &bob.token,
            json!({ "listing_id": l, "owner_kind": "org", "owner_id": "org_fixture01", "intent_id": intent(2) }),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "owner_not_supported"), "{v}");
    let (s, _) = srv
        .post(
            "/v1/marketplace/purchases",
            &bob.token,
            json!({ "listing_id": l, "owner_kind": "account", "owner_id": "acct_someoneelse", "intent_id": intent(3) }),
        )
        .await;
    assert_eq!(s, 403);

    // Not paid yet: nothing is installed.
    let (s, v) = install(&srv, &bob, &l, Some(&ins)).await;
    assert_eq!((s, code(&v)), (402, "payment_pending"), "{v}");
    let (_, v) = srv.get("/v1/nest-installations", &bob.token).await;
    assert_eq!(v["data"].as_array().unwrap().len(), 0);
    // Someone else cannot use bob's purchase.
    let eve = srv.person("mpevep2").await;
    let (s, _) = install(&srv, &eve, &l, Some(&ins)).await;
    assert_eq!(s, 404);
    // The bridge refuses an intent the payment was not bound to: nothing is installed.
    m.lock().unwrap().mismatch = true;
    let (s, v) = install(&srv, &bob, &l, Some(&ins)).await;
    assert_eq!((s, code(&v)), (409, "intent_mismatch"), "{v}");
    m.lock().unwrap().mismatch = false;

    m.lock().unwrap().paid = true;
    let (s, v) = install(&srv, &bob, &l, Some(&ins)).await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["data"]["id"], ins.as_str());
    assert_eq!(v["data"]["funding"], "licence");
    assert_eq!(v["data"]["licence"]["licence_id"], LICENCE);
    assert_eq!(v["data"]["licence"]["status"], "active");
    let last = m.lock().unwrap().consumes.last().cloned().unwrap();
    assert_eq!(
        last,
        json!({ "purchase_id": ins, "intent": purchase_intent, "consumer_reference": ins })
    );
    // A replay returns the installation and consumes nothing new.
    let n = m.lock().unwrap().consumes.len();
    let (s, v) = install(&srv, &bob, &l, Some(&ins)).await;
    assert_eq!((s, v["data"]["id"].as_str()), (200, Some(ins.as_str())));
    assert_eq!(m.lock().unwrap().consumes.len(), n);
    // Bob's jobs are admitted under the licence; a second purchase is refused.
    let (s, v) = quote(&srv, &bob).await;
    assert_eq!(s, 201, "{v}");
    let (s, v) = buy(&srv, &bob, &l, 4).await;
    assert_eq!((s, code(&v)), (409, "already_installed"), "{v}");
}

/// Licence projection: no access stops the worker and refuses jobs; access again resumes; older
/// versions change nothing; the deletion boundary uninstalls and removes the worker and its data.
#[tokio::test]
async fn licences_pause_resume_and_remove_at_the_deletion_boundary() {
    let (srv, m, _, _, l, bob) = priced("p3").await;
    let (ins, _) = installed(&srv, &m, &bob, &l).await;
    let path = format!("/internal/v1/marketplace/licences/{LICENCE}");
    let body = |status: &str, access: bool, due: Option<&str>, version: i64| {
        json!({
            "installation_id": ins, "listing_id": l, "owner_kind": "account", "owner_id": bob.account,
            "term": "monthly", "status": status, "access": access, "paid_through": "2026-11-01T00:00:00Z",
            "cancel_at_period_end": false, "deletion_due_at": due, "revoked_at": null,
            "reconciliation_version": version,
        })
    };
    let calls = |p: &str| -> usize {
        srv.state
            .nests
            .calls()
            .into_iter()
            .filter(|c| c.path == p)
            .count()
    };
    // Only the billing service.
    let (s, _) = srv
        .call(
            Method::PUT,
            &path,
            Some(OPS),
            Some(body("paused", false, None, 1)),
        )
        .await;
    assert_eq!(s, 403);

    let future = "2099-01-01T00:00:00Z";
    let (s, v) = srv
        .call(
            Method::PUT,
            &path,
            Some(BILLING),
            Some(body("paused", false, Some(future), 1)),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["runtime"], "stopped");
    assert_eq!(calls("/v1/mp/workers/stop"), 1);
    let (s, v) = quote(&srv, &bob).await;
    assert_eq!((s, code(&v)), (402, "licence_inactive"), "{v}");
    let (_, v) = srv
        .get(&format!("/v1/nest-installations/{ins}"), &bob.token)
        .await;
    assert_eq!(v["data"]["licence"]["status"], "paused");
    assert_eq!(v["data"]["licence"]["runtime"], "stopped");
    // The same state again does nothing new.
    let (s, _) = srv
        .call(
            Method::PUT,
            &path,
            Some(BILLING),
            Some(body("paused", false, Some(future), 1)),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(calls("/v1/mp/workers/stop"), 1);

    // Paid again: jobs are admitted; the worker restarts with the next one.
    let (s, v) = srv
        .call(
            Method::PUT,
            &path,
            Some(BILLING),
            Some(body("active", true, None, 2)),
        )
        .await;
    assert_eq!(
        (s, v["data"]["runtime"].as_str()),
        (200, Some("running")),
        "{v}"
    );
    let (s, v) = quote(&srv, &bob).await;
    assert_eq!(s, 201, "{v}");
    // An older projection arriving late changes nothing.
    let (s, v) = srv
        .call(
            Method::PUT,
            &path,
            Some(BILLING),
            Some(body("paused", false, Some(future), 1)),
        )
        .await;
    assert_eq!(
        (s, v["data"]["runtime"].as_str()),
        (200, Some("running")),
        "{v}"
    );
    let (s, _) = quote(&srv, &bob).await;
    assert_eq!(s, 201);
    // Another installation's licence id is refused.
    let mut wrong = body("active", true, None, 3);
    wrong["installation_id"] = json!("ins_aaaaaaaaaaaaaaaa");
    let (s, v) = srv
        .call(Method::PUT, &path, Some(BILLING), Some(wrong))
        .await;
    assert_eq!((s, code(&v)), (409, "licence_mismatch"), "{v}");
    let (s, _) = srv
        .call(
            Method::PUT,
            "/internal/v1/marketplace/licences/00000000-0000-4000-8000-000000000999",
            Some(BILLING),
            Some(body("active", true, None, 3)),
        )
        .await;
    assert_eq!(s, 404);

    // Terminated past the deletion boundary: uninstalled, worker and data removed.
    let (s, v) = srv
        .call(
            Method::PUT,
            &path,
            Some(BILLING),
            Some(body("terminated", false, Some("2026-01-01T00:00:00Z"), 4)),
        )
        .await;
    assert_eq!(
        (s, v["data"]["runtime"].as_str()),
        (200, Some("removed")),
        "{v}"
    );
    let removed = srv
        .state
        .nests
        .calls()
        .into_iter()
        .find(|c| c.path == "/v1/mp/workers/remove")
        .unwrap();
    assert_eq!(removed.body.unwrap()["listing_id"], l.as_str());
    let (_, v) = srv
        .get(&format!("/v1/nest-installations/{ins}"), &bob.token)
        .await;
    assert_eq!(v["data"]["status"], "uninstalled");
    assert_eq!(v["data"]["secrets"][0]["set"], false);
    let (s, _) = srv
        .call(
            Method::PUT,
            &path,
            Some(BILLING),
            Some(body("terminated", false, Some("2026-01-01T00:00:00Z"), 5)),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(calls("/v1/mp/workers/remove"), 1, "removed once");
}

/// The deletion boundary is a time: the sweep removes a paused installation once it passes, even
/// with no new projection.
#[tokio::test]
async fn the_sweep_enforces_a_deletion_boundary_without_a_new_projection() {
    let (srv, m, _, _, l, bob) = priced("p4").await;
    let (ins, _) = installed(&srv, &m, &bob, &l).await;
    let (s, v) = srv
        .call(
            Method::PUT,
            &format!("/internal/v1/marketplace/licences/{LICENCE}"),
            Some(BILLING),
            Some(json!({
                "installation_id": ins, "listing_id": l, "owner_kind": "account", "owner_id": bob.account,
                "term": "monthly", "status": "paused", "access": false, "paid_through": null,
                "cancel_at_period_end": false, "deletion_due_at": "2099-01-01T00:00:00Z",
                "revoked_at": null, "reconciliation_version": 1,
            })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(marketplace_pay::sweep_once(&srv.state).await, 0);
    let iid = ins.clone();
    srv.state
        .store
        .group_tx(move |tx| -> Result<(), crate::groups::GErr> {
            tx.execute(
                "UPDATE nest_installation_licences SET deletion_due_at = 1 WHERE installation_id = ?1",
                [&iid],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(marketplace_pay::sweep_once(&srv.state).await, 1);
    let (_, v) = srv
        .get(&format!("/v1/nest-installations/{ins}"), &bob.token)
        .await;
    assert_eq!(v["data"]["status"], "uninstalled");
    assert_eq!(v["data"]["licence"]["runtime"], "removed");
    assert_eq!(marketplace_pay::sweep_once(&srv.state).await, 0, "once");
}

/// Without a bridge nothing is priced: the publisher hears that payments are unavailable.
#[tokio::test]
async fn without_the_bridge_prices_are_refused() {
    let (srv, owner) = m2_setup().await;
    srv.global_flag("marketplace").await;
    let (_, ann, p, _, l, _) = published_with_schema(&srv, &owner, "p5").await;
    let (s, v) = set_price(
        &srv,
        &ann,
        &p,
        &l,
        json!({ "amount_minor": 12000, "currency": "TRY", "term": "one_off" }),
    )
    .await;
    assert_eq!((s, code(&v)), (503, "billing_unavailable"), "{v}");
    let bob = srv.person("mpbobp5").await;
    let (s, v) = install(&srv, &bob, &l, None).await;
    assert_eq!(s, 201, "the listing stays free: {v}");
}
