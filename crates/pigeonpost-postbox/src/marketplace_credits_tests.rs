//! Credits v1 over real sockets, with a stand-in for the billing bridge that behaves like
//! saas_be's credit routes (holds idempotent by key, capture and release idempotent, a captured
//! hold never released): per-job prices mirrored as `per_job` USD offers, the hold placed before
//! a job is queued, `402 insufficient_credit` with nothing queued, capture once a succeeded result
//! is delivered, release on every other ending, retries that never charge twice, and free
//! operations that never reach billing. Synthetic accounts, tokens and amounts only.

use crate::marketplace_credits::settle_once;
use crate::marketplace_hosted::{deliver_once, dispatch_once, expire_once};
use crate::marketplace_hosted_tests::{
    live_listing, pdf_result, result, run, sql, world_with, World,
};
use crate::marketplace_m2_tests::code;
use crate::*;
use axum::extract::{Path as AxPath, State as AxState};
use axum::http::{HeaderMap as AxHeaders, StatusCode as S};
use reqwest::Method;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;

const BRIDGE_TOKEN: &str = "bridge-internal-fixture-token-0123456789";

#[derive(Clone)]
struct HoldRow {
    id: String,
    amount: i64,
    state: &'static str,
}

/// What the bridge stand-in saw and how it answers.
#[derive(Default)]
struct Mock {
    offers: HashMap<String, Value>,
    available: i64,
    /// Holds by idempotency key.
    holds: HashMap<String, HoldRow>,
    sales: Vec<String>,
    hold_calls: usize,
    capture_calls: usize,
    release_calls: usize,
    /// Answer this many hold requests with 502 after placing the hold (a lost answer).
    lose_hold_answers: usize,
    /// Answer this many captures with 503 before doing anything.
    fail_captures: usize,
    /// Answer this many captures, then holds, with a 404 that carries no saas_be code (credits
    /// switched off, a config rewrite): transient, never final.
    off_captures: usize,
    off_holds: usize,
    /// The `deadline` of each hold request.
    deadlines: Vec<Option<String>>,
}

type Shared = Arc<Mutex<Mock>>;
type Reply = (S, axum::Json<Value>);

fn authed(h: &AxHeaders) -> bool {
    h.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {BRIDGE_TOKEN}"))
}

fn denied() -> Reply {
    (
        S::UNAUTHORIZED,
        axum::Json(json!({ "code": "invalid_service_token" })),
    )
}

async fn offer(
    AxState(m): AxState<Shared>,
    h: AxHeaders,
    AxPath(id): AxPath<String>,
    axum::Json(b): axum::Json<Value>,
) -> Reply {
    if !authed(&h) {
        return denied();
    }
    m.lock().unwrap().offers.insert(id.clone(), b.clone());
    (
        S::OK,
        axum::Json(json!({ "offer": {
            "offer_id": id, "price_minor": b["price_minor"], "term": b["term"],
            "currency": b["currency"], "status": b["status"], "sellable": b["status"] == "active",
        }})),
    )
}

async fn hold(
    AxState(m): AxState<Shared>,
    h: AxHeaders,
    axum::Json(b): axum::Json<Value>,
) -> Reply {
    if !authed(&h) {
        return denied();
    }
    let mut m = m.lock().unwrap();
    m.hold_calls += 1;
    m.deadlines.push(b["deadline"].as_str().map(str::to_string));
    if m.off_holds > 0 {
        m.off_holds -= 1;
        return (
            S::NOT_FOUND,
            axum::Json(
                json!({ "error": "Marketplace is not enabled.", "code": "upstream_rejected" }),
            ),
        );
    }
    let key = b["idempotency_key"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        b["job_ref"],
        key.as_str(),
        "the job is the hold's reference"
    );
    assert_eq!(b["owner"]["owner_kind"], "account");
    let reply = |r: &HoldRow| {
        (
            S::CREATED,
            axum::Json(json!({ "hold_id": r.id, "amount_cents": r.amount,
                               "expires_at": "2030-01-01T00:00:00Z" })),
        )
    };
    if let Some(r) = m.holds.get(&key).cloned() {
        return reply(&r);
    }
    let price = m.offers[b["offer_id"].as_str().unwrap()]["price_minor"]
        .as_i64()
        .unwrap();
    if m.available < price {
        return (
            S::PAYMENT_REQUIRED,
            axum::Json(
                json!({ "error": "not enough credit", "code": "insufficient_credit",
                               "available_cents": m.available, "required_cents": price }),
            ),
        );
    }
    m.available -= price;
    let r = HoldRow {
        id: format!("hold_{}", m.holds.len() + 1),
        amount: price,
        state: "held",
    };
    m.holds.insert(key, r.clone());
    if m.lose_hold_answers > 0 {
        m.lose_hold_answers -= 1;
        return (S::BAD_GATEWAY, axum::Json(json!({ "code": "upstream" })));
    }
    reply(&r)
}

async fn settle(
    AxState(m): AxState<Shared>,
    h: AxHeaders,
    AxPath((id, action)): AxPath<(String, String)>,
    axum::Json(_): axum::Json<Value>,
) -> Reply {
    if !authed(&h) {
        return denied();
    }
    let mut m = m.lock().unwrap();
    if action == "capture" {
        m.capture_calls += 1;
        if m.fail_captures > 0 {
            m.fail_captures -= 1;
            return (S::SERVICE_UNAVAILABLE, axum::Json(json!({})));
        }
        if m.off_captures > 0 {
            m.off_captures -= 1;
            return (
                S::NOT_FOUND,
                axum::Json(json!({ "error": "not enabled", "code": "upstream_rejected" })),
            );
        }
    } else {
        m.release_calls += 1;
    }
    let Some(key) = m
        .holds
        .iter()
        .find(|(_, r)| r.id == id)
        .map(|(k, _)| k.clone())
    else {
        return (S::NOT_FOUND, axum::Json(json!({ "code": "not_found" })));
    };
    let r = m.holds[&key].clone();
    match (action.as_str(), r.state) {
        ("capture", "held") => {
            m.sales.push(id.clone());
            m.holds.get_mut(&key).unwrap().state = "captured";
        }
        ("capture", "captured") => {}
        ("release", "held") => {
            m.available += r.amount;
            m.holds.get_mut(&key).unwrap().state = "released";
        }
        ("release", "released" | "expired") => {}
        // saas_be's own codes for a closed hold.
        ("capture", st @ ("released" | "expired")) => {
            let code = if st == "expired" {
                "hold_expired"
            } else {
                "hold_released"
            };
            return (S::CONFLICT, axum::Json(json!({ "code": code })));
        }
        _ => return (S::CONFLICT, axum::Json(json!({ "code": "hold_captured" }))),
    }
    if action == "capture" {
        let commission = (r.amount * 1000 + 5000) / 10_000;
        (
            S::OK,
            axum::Json(
                json!({ "sale_id": format!("sale_{id}"), "amount_cents": r.amount,
                "commission_bps": 1000, "commission_cents": commission,
                "publisher_cents": r.amount - commission }),
            ),
        )
    } else {
        (S::OK, axum::Json(json!({ "released": true })))
    }
}

async fn bridge(available: i64) -> (String, Shared) {
    let m: Shared = Arc::new(Mutex::new(Mock {
        available,
        ..Mock::default()
    }));
    let app = axum::Router::new()
        .route(
            "/internal/marketplace/offers/{id}",
            axum::routing::put(offer),
        )
        .route("/internal/credits/holds", axum::routing::post(hold))
        .route(
            "/internal/credits/holds/{id}/{action}",
            axum::routing::post(settle),
        )
        .with_state(m.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (url, m)
}

fn price_path(w: &World, listing: &str, op: &str) -> String {
    format!(
        "/v1/publishers/{}/listings/{listing}/operations/{op}/price",
        w.publisher
    )
}

/// A live publisher-hosted listing whose `hr.research` costs $25 a job, and a buyer with
/// `available` cents of credit.
async fn priced(tag: &str, available: i64) -> (World, Shared, String) {
    let (url, m) = bridge(available).await;
    let w = world_with(tag, Some((url, BRIDGE_TOKEN))).await;
    let (listing, _) = live_listing(&w).await;
    let (s, v) = w
        .srv
        .call(
            Method::PUT,
            &price_path(&w, &listing, "hr.research"),
            Some(&w.ann.strong),
            Some(json!({ "amount_cents": 2500, "currency": "USD" })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["offer"]["on_sale"], true, "{v}");
    (w, m, listing)
}

async fn job(w: &World, id: &str) -> Value {
    let (s, v) = w
        .srv
        .get(&format!("/v1/marketplace/hosted-jobs/{id}"), &w.bob.token)
        .await;
    assert_eq!(s, 200, "{v}");
    v["data"].clone()
}

async fn charge_state(w: &World, id: &str) -> String {
    let id = id.to_string();
    w.srv
        .state
        .store
        .group_read(move |c| -> Result<String, crate::groups::GErr> {
            Ok(c.query_row(
                "SELECT state FROM nest_hosted_charges WHERE job_id = ?1",
                [&id],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap()
}

/// Run `hr.research` and dispatch it: the job id.
async fn started(w: &World, listing: &str) -> String {
    let (s, v) = run(w, listing, json!({ "role": "CTO" })).await;
    assert_eq!(s, 201, "{v}");
    let id = v["data"]["job_id"].as_str().unwrap().to_string();
    dispatch_once(&w.srv.state).await;
    id
}

/// The publisher prices an operation per job in USD; the price is mirrored as a `per_job` offer,
/// staff see it in review, buyers see it on the operation, and a card price is refused.
#[tokio::test]
async fn a_per_job_price_is_mirrored_and_shown_to_staff_and_buyers() {
    let (w, m, listing) = priced("p", 0).await;
    {
        let m = m.lock().unwrap();
        let (id, body) = m.offers.iter().next().unwrap();
        assert!(id.starts_with("mpo_"));
        assert_eq!(body["term"], "per_job");
        assert_eq!(body["currency"], "USD");
        assert_eq!(body["price_minor"], 2500);
        assert_eq!(body["listing_id"], listing.as_str());
        assert_eq!(body["status"], "active");
    }
    // The buyer sees the price on the operation.
    let (_, v) = w
        .srv
        .get(&format!("/v1/marketplace/listings/{listing}"), &w.bob.token)
        .await;
    let price = &v["data"]["operations"][0]["price"];
    assert_eq!(
        (
            price["amount_cents"].clone(),
            price["term"].clone(),
            price["on_sale"].clone()
        ),
        (json!(2500), json!("per_job"), json!(true)),
        "{v}"
    );
    // Staff see it on the integration's review.
    let review: String = {
        let l = listing.clone();
        w.srv
            .state
            .store
            .group_read(move |c| -> Result<String, crate::groups::GErr> {
                Ok(c.query_row(
                    "SELECT c.id FROM review_cases c JOIN nest_listings l
                       ON l.release_id = c.release_id WHERE l.id = ?1",
                    [&l],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap()
    };
    let (s, v) = w
        .srv
        .get(&format!("/v1/staff/reviews/{review}"), &w.rev.strong)
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        v["data"]["integration"]["prices"][0]["amount_cents"], 2500,
        "{v}"
    );
    // A new price is a new offer; the old one is retired.
    let (s, v) = w
        .srv
        .call(
            Method::PUT,
            &price_path(&w, &listing, "hr.research"),
            Some(&w.ann.strong),
            Some(json!({ "amount_cents": 3000 })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    {
        let m = m.lock().unwrap();
        assert_eq!(m.offers.len(), 2);
        assert_eq!(
            m.offers
                .values()
                .filter(|o| o["status"] == "retired")
                .count(),
            1
        );
    }
    // Card prices, other currencies and unknown operations are refused; without step-up too.
    let (s, v) = w
        .srv
        .call(
            Method::PUT,
            &format!("/v1/publishers/{}/listings/{listing}/price", w.publisher),
            Some(&w.ann.strong),
            Some(json!({ "amount_minor": 99900, "currency": "TRY", "term": "one_off" })),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "per_job_only"), "{v}");
    let (s, v) = w
        .srv
        .call(
            Method::PUT,
            &price_path(&w, &listing, "hr.research"),
            Some(&w.ann.strong),
            Some(json!({ "amount_cents": 3000, "currency": "TRY" })),
        )
        .await;
    assert_eq!((s, code(&v)), (400, "invalid_request"), "{v}");
    let (s, v) = w
        .srv
        .call(
            Method::PUT,
            &price_path(&w, &listing, "hr.other"),
            Some(&w.ann.strong),
            Some(json!({ "amount_cents": 3000 })),
        )
        .await;
    assert_eq!((s, code(&v)), (404, "unknown_operation"), "{v}");
    let (s, _) = w
        .srv
        .call(
            Method::PUT,
            &price_path(&w, &listing, "hr.research"),
            Some(&w.ann.token),
            Some(json!({ "amount_cents": 3000 })),
        )
        .await;
    assert_eq!(s, 401);
    let (s, v) = w
        .srv
        .get(&price_path(&w, &listing, "hr.research"), &w.ann.token)
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["price"]["amount_cents"], 3000);
    // Free again: the offer is retired.
    let (s, v) = w
        .srv
        .call(
            Method::DELETE,
            &price_path(&w, &listing, "hr.research"),
            Some(&w.ann.strong),
            None,
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["price"], Value::Null);
    assert!(m
        .lock()
        .unwrap()
        .offers
        .values()
        .all(|o| o["status"] == "retired"));
}

/// Too little credit: 402 with the balance, the price and where to buy; nothing is queued, held
/// or left behind.
#[tokio::test]
async fn too_little_credit_is_402_and_queues_nothing() {
    let (w, m, listing) = priced("i", 1000).await;
    let (s, v) = run(&w, &listing, json!({ "role": "CTO" })).await;
    assert_eq!((s, code(&v)), (402, "insufficient_credit"), "{v}");
    let d = &v["error"]["details"];
    assert_eq!(d["available_cents"], 1000);
    assert_eq!(d["required_cents"], 2500);
    assert!(d["buy_url"].as_str().unwrap().contains("#credits"), "{v}");
    let (_, v) = w.srv.get("/v1/marketplace/hosted-jobs", &w.bob.token).await;
    assert_eq!(v["data"], json!([]));
    let rows: i64 = w
        .srv
        .state
        .store
        .group_read(|c| -> Result<i64, crate::groups::GErr> {
            Ok(c.query_row("SELECT COUNT(*) FROM nest_hosted_charges", [], |r| r.get(0))?)
        })
        .await
        .unwrap();
    assert_eq!(rows, 0);
    assert!(m.lock().unwrap().holds.is_empty());
}

/// The hold comes before the job exists; a succeeded result delivered to the buyer is captured
/// once (10% commission), and settling again changes nothing.
#[tokio::test]
async fn the_hold_comes_first_and_a_delivered_result_is_captured_once() {
    let (w, m, listing) = priced("c", 10_000).await;
    let (s, v) = run(&w, &listing, json!({ "role": "CTO" })).await;
    assert_eq!(s, 201, "{v}");
    let id = v["data"]["job_id"].as_str().unwrap().to_string();
    assert_eq!(
        v["data"]["charge"],
        json!({ "amount_cents": 2500, "currency": "USD", "status": "held" })
    );
    assert_eq!(m.lock().unwrap().available, 7500);
    assert!(m.lock().unwrap().holds.contains_key(&id));
    dispatch_once(&w.srv.state).await;
    // Not captured before the result reaches the buyer.
    settle_once(&w.srv.state).await;
    assert_eq!(charge_state(&w, &id).await, "held");
    let (s, v) = result(&w.srv, &w.service.cap, &id, pdf_result("Three candidates")).await;
    assert_eq!(s, 200, "{v}");
    settle_once(&w.srv.state).await;
    assert_eq!(charge_state(&w, &id).await, "held", "not delivered yet");
    deliver_once(&w.srv.state).await;
    assert_eq!(job(&w, &id).await["delivery"], "delivered");
    settle_once(&w.srv.state).await;
    assert_eq!(charge_state(&w, &id).await, "captured");
    assert_eq!(job(&w, &id).await["charge"]["status"], "charged");
    settle_once(&w.srv.state).await;
    {
        let m = m.lock().unwrap();
        assert_eq!(m.sales.len(), 1);
        assert_eq!(m.capture_calls, 1);
        assert_eq!(m.release_calls, 0);
        assert_eq!(m.available, 7500);
    }
    let (commission, publisher): (i64, i64) = {
        let j = id.clone();
        w.srv
            .state
            .store
            .group_read(move |c| -> Result<(i64, i64), crate::groups::GErr> {
                Ok(c.query_row(
                    "SELECT commission_cents, publisher_cents FROM nest_hosted_charges
                      WHERE job_id = ?1",
                    [&j],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .await
            .unwrap()
    };
    assert_eq!((commission, publisher), (250, 2250));
}

/// Every ending without a delivered success releases the hold: the publisher's failure, the
/// deadline, the service mailbox leaving the publisher, and the buyer's cancel.
#[tokio::test]
async fn every_other_ending_releases_the_hold() {
    let (w, m, listing) = priced("r", 100_000).await;
    // The publisher reports a failure.
    let failed = started(&w, &listing).await;
    let (s, v) = result(
        &w.srv,
        &w.service.cap,
        &failed,
        json!({ "status": "failed", "summary": "no data" }),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    // The deadline passes.
    let expired = started(&w, &listing).await;
    sql(
        &w.srv,
        "UPDATE nest_hosted_jobs SET deadline_at = 1 WHERE id = ?1",
        vec![expired.clone()],
    )
    .await;
    expire_once(&w.srv.state).await;
    // The buyer cancels before the result arrives; a late result is refused.
    let cancelled = started(&w, &listing).await;
    let (s, v) = w
        .srv
        .post(
            &format!("/v1/marketplace/hosted-jobs/{cancelled}/cancel"),
            &w.bob.token,
            json!({}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["execution"], "cancelled");
    let (s, v) = result(&w.srv, &w.service.cap, &cancelled, pdf_result("late")).await;
    assert_eq!((s, code(&v)), (409, "job_closed"), "{v}");
    let (s, _) = w
        .srv
        .post(
            &format!("/v1/marketplace/hosted-jobs/{cancelled}/cancel"),
            &w.ann.token,
            json!({}),
        )
        .await;
    assert_eq!(s, 404, "only the buyer cancels");
    // The service mailbox leaves the publisher before dispatch.
    let (s, v) = run(&w, &listing, json!({ "role": "CFO" })).await;
    assert_eq!(s, 201, "{v}");
    let lost = v["data"]["job_id"].as_str().unwrap().to_string();
    // Running a job also kicks off a dispatch in the background, which races this test's
    // mailbox move (Windows schedules it differently). Let that dispatch finish first, then
    // queue the job again: the dispatch below is the one that meets the moved mailbox.
    for _ in 0..150 {
        if job(&w, &lost).await["execution"] != "queued" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    sql(
        &w.srv,
        "UPDATE identities SET account_id = ?2 WHERE address = ?1",
        vec![w.service.address.clone(), w.bob.account.clone()],
    )
    .await;
    sql(
        &w.srv,
        "UPDATE nest_hosted_jobs SET state = 'queued', next_attempt_at = 0 WHERE id = ?1",
        vec![lost.clone()],
    )
    .await;
    dispatch_once(&w.srv.state).await;
    assert_eq!(job(&w, &lost).await["error_code"], "service_unavailable");
    assert_eq!(m.lock().unwrap().available, 90_000);
    settle_once(&w.srv.state).await;
    for (id, reason) in [
        (&failed, "publisher_failed"),
        (&expired, "deadline_passed"),
        (&cancelled, "cancelled"),
        (&lost, "service_unavailable"),
    ] {
        assert_eq!(charge_state(&w, id).await, "released", "{id}");
        assert_eq!(job(&w, id).await["charge"]["status"], "released");
        let j = id.clone();
        let why: String = w
            .srv
            .state
            .store
            .group_read(move |c| -> Result<String, crate::groups::GErr> {
                Ok(c.query_row(
                    "SELECT release_reason FROM nest_hosted_charges WHERE job_id = ?1",
                    [&j],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(why, reason);
    }
    let m = m.lock().unwrap();
    assert_eq!(m.available, 100_000, "every hold came back");
    assert!(m.sales.is_empty());
}

/// Retries: a capture that fails is retried until saas_be answers and charges once; a hold whose
/// answer was lost is found again by its key and released, so the buyer is not left short.
#[tokio::test]
async fn retries_never_charge_twice_or_leak_a_hold() {
    let (w, m, listing) = priced("t", 10_000).await;
    m.lock().unwrap().fail_captures = 2;
    let id = started(&w, &listing).await;
    let (s, _) = result(&w.srv, &w.service.cap, &id, pdf_result("ok")).await;
    assert_eq!(s, 200);
    deliver_once(&w.srv.state).await;
    for _ in 0..4 {
        settle_once(&w.srv.state).await;
        // Skip the backoff.
        sql(
            &w.srv,
            "UPDATE nest_hosted_charges SET next_attempt_at = 0 WHERE job_id = ?1",
            vec![id.clone()],
        )
        .await;
    }
    assert_eq!(charge_state(&w, &id).await, "captured");
    {
        let m = m.lock().unwrap();
        assert_eq!(m.capture_calls, 3);
        assert_eq!(m.sales.len(), 1);
        assert_eq!(m.available, 7500);
    }
    // The bridge places a hold but its answer is lost: the buyer gets 503 and no job, and the
    // worker releases that hold.
    m.lock().unwrap().lose_hold_answers = 1;
    let (s, v) = run(&w, &listing, json!({ "role": "CFO" })).await;
    assert_eq!((s, code(&v)), (503, "billing_unavailable"), "{v}");
    assert_eq!(m.lock().unwrap().available, 5000);
    settle_once(&w.srv.state).await;
    let m = m.lock().unwrap();
    assert_eq!(m.available, 7500, "the lost hold was released");
    assert_eq!(m.sales.len(), 1);
    assert_eq!(
        m.holds.values().filter(|h| h.state == "released").count(),
        1
    );
}

/// A free operation never reaches billing; and with the bridge down a priced one is refused
/// before anything is queued.
#[tokio::test]
async fn free_operations_skip_billing_and_priced_ones_need_it() {
    let (url, m) = bridge(0).await;
    let w = world_with("f", Some((url, BRIDGE_TOKEN))).await;
    let (listing, _) = live_listing(&w).await;
    let (s, v) = run(&w, &listing, json!({ "role": "CTO" })).await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["data"]["charge"], Value::Null);
    let (_, v) = w
        .srv
        .get(&format!("/v1/marketplace/listings/{listing}"), &w.bob.token)
        .await;
    assert_eq!(v["data"]["operations"][0]["price"], Value::Null);
    assert_eq!(m.lock().unwrap().hold_calls, 0);

    // No bridge at all: a priced operation is not run for free.
    let w = world_with("g", None).await;
    let (listing, _) = live_listing(&w).await;
    sql(
        &w.srv,
        "INSERT INTO nest_hosted_prices (listing_id, operation, amount_cents, currency,
            updated_by, updated_at) VALUES (?1, 'hr.research', 2500, 'USD', 'acct_x', 1)",
        vec![listing.clone()],
    )
    .await;
    let (s, v) = run(&w, &listing, json!({ "role": "CTO" })).await;
    assert_eq!((s, code(&v)), (503, "not_on_sale"), "{v}");
    let (_, v) = w.srv.get("/v1/marketplace/hosted-jobs", &w.bob.token).await;
    assert_eq!(v["data"], json!([]));
}

async fn skip_backoff(w: &World, id: &str) {
    sql(
        &w.srv,
        "UPDATE nest_hosted_charges SET next_attempt_at = 0 WHERE job_id = ?1",
        vec![id.to_string()],
    )
    .await;
}

async fn charge_error(w: &World, id: &str) -> Option<String> {
    let id = id.to_string();
    w.srv
        .state
        .store
        .group_read(move |c| -> Result<Option<String>, crate::groups::GErr> {
            Ok(c.query_row(
                "SELECT last_error FROM nest_hosted_charges WHERE job_id = ?1",
                [&id],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap()
}

/// A 4xx that is not one of saas_be's definite codes (credits switched off, a config rewrite, a
/// token rotation) is retried, never taken as final: the job is captured once it clears; and a
/// hold replay never concludes "nothing held" from such an answer.
#[tokio::test]
async fn transient_refusals_are_retried_not_final() {
    let (w, m, listing) = priced("x", 10_000).await;
    m.lock().unwrap().off_captures = 3;
    let id = started(&w, &listing).await;
    let (s, _) = result(&w.srv, &w.service.cap, &id, pdf_result("ok")).await;
    assert_eq!(s, 200);
    deliver_once(&w.srv.state).await;
    for _ in 0..3 {
        settle_once(&w.srv.state).await;
        assert_eq!(charge_state(&w, &id).await, "capture_pending");
        let err = charge_error(&w, &id).await.unwrap_or_default();
        assert!(err.starts_with("bridge_404"), "{err}");
        skip_backoff(&w, &id).await;
    }
    settle_once(&w.srv.state).await;
    assert_eq!(charge_state(&w, &id).await, "captured");
    assert_eq!(m.lock().unwrap().sales.len(), 1);

    // A hold answered with a non-definite 404: the buyer gets 503 and the row stays to be settled.
    m.lock().unwrap().off_holds = 1;
    let (s, v) = run(&w, &listing, json!({ "role": "CFO" })).await;
    assert_eq!((s, code(&v)), (503, "billing_unavailable"), "{v}");
    let lost: String = w
        .srv
        .state
        .store
        .group_read(|c| -> Result<String, crate::groups::GErr> {
            Ok(c.query_row(
                "SELECT job_id FROM nest_hosted_charges WHERE state = 'release_pending'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    // The replay meets the same answer twice before billing answers.
    m.lock().unwrap().off_holds = 2;
    for _ in 0..2 {
        settle_once(&w.srv.state).await;
        assert_eq!(charge_state(&w, &lost).await, "release_pending");
        skip_backoff(&w, &lost).await;
    }
    settle_once(&w.srv.state).await;
    assert_eq!(charge_state(&w, &lost).await, "released");
    assert_eq!(
        m.lock().unwrap().available,
        7500,
        "the replayed hold was released"
    );
}

/// A capture saas_be refuses for good (the hold expired) charges nothing and queues a release.
#[tokio::test]
async fn a_refused_capture_queues_a_release() {
    let (w, m, listing) = priced("e", 10_000).await;
    let id = started(&w, &listing).await;
    m.lock().unwrap().holds.get_mut(&id).unwrap().state = "expired";
    let (s, _) = result(&w.srv, &w.service.cap, &id, pdf_result("ok")).await;
    assert_eq!(s, 200);
    deliver_once(&w.srv.state).await;
    settle_once(&w.srv.state).await;
    assert_eq!(charge_state(&w, &id).await, "release_pending");
    assert_eq!(charge_error(&w, &id).await.as_deref(), Some("hold_expired"));
    settle_once(&w.srv.state).await;
    assert_eq!(charge_state(&w, &id).await, "released");
    assert_eq!(job(&w, &id).await["charge"]["status"], "released");
    let m = m.lock().unwrap();
    assert!(m.sales.is_empty());
    assert_eq!(m.release_calls, 1);
}

/// The job's deadline travels with the hold (saas_be expires the hold a day after it).
#[tokio::test]
async fn the_hold_carries_the_job_deadline() {
    let (w, m, listing) = priced("d", 10_000).await;
    let (s, v) = run(&w, &listing, json!({ "role": "CTO" })).await;
    assert_eq!(s, 201, "{v}");
    let deadline = v["data"]["deadline_at"].as_str().unwrap().to_string();
    let sent = m.lock().unwrap().deadlines.last().cloned().flatten();
    assert_eq!(sent, Some(deadline.clone()));
    let at = chrono::DateTime::parse_from_rfc3339(&deadline)
        .unwrap()
        .timestamp();
    // The operation asks for 48 hours (under the 7-day cap).
    assert!(
        (at - now_unix() as i64 - 48 * 3600).abs() < 120,
        "{deadline}"
    );
}

/// Switching the marketplace off refuses new jobs only: a job already running still gets its
/// result, delivers it and is captured.
#[tokio::test]
async fn the_marketplace_flag_stops_new_jobs_not_running_ones() {
    let (w, m, listing) = priced("o", 10_000).await;
    let id = started(&w, &listing).await;
    let (s, _) = w
        .srv
        .call(
            Method::PUT,
            "/internal/v1/features/marketplace",
            Some(crate::org_tests::OPS),
            Some(json!({ "scope": "global", "enabled": false })),
        )
        .await;
    assert_eq!(s, 200);
    let (s, v) = run(&w, &listing, json!({ "role": "CFO" })).await;
    assert_ne!(s, 201, "{v}");
    let (s, v) = result(&w.srv, &w.service.cap, &id, pdf_result("ok")).await;
    assert_eq!(s, 200, "{v}");
    deliver_once(&w.srv.state).await;
    settle_once(&w.srv.state).await;
    assert_eq!(charge_state(&w, &id).await, "captured");
    assert_eq!(m.lock().unwrap().sales.len(), 1);
}

async fn job_int(w: &World, q: &'static str, id: &str) -> i64 {
    let id = id.to_string();
    w.srv
        .state
        .store
        .group_read(move |c| -> Result<i64, crate::groups::GErr> {
            Ok(c.query_row(q, [&id], |r| r.get(0))?)
        })
        .await
        .unwrap()
}

/// A result that cannot reach the buyer's inbox is retried with backoff, not on every pass.
#[tokio::test]
async fn delivery_backs_off_between_attempts() {
    let (url, _m) = bridge(0).await;
    let w = world_with("b", Some((url, BRIDGE_TOKEN))).await;
    let (listing, _) = live_listing(&w).await;
    let id = started(&w, &listing).await;
    let (s, _) = result(&w.srv, &w.service.cap, &id, pdf_result("ok")).await;
    assert_eq!(s, 200);
    // The buyer's mailbox is gone: the send fails.
    sql(
        &w.srv,
        "UPDATE nest_hosted_jobs SET deliver_to = '/k/nobody' WHERE id = ?1",
        vec![id.clone()],
    )
    .await;
    deliver_once(&w.srv.state).await;
    let next = job_int(
        &w,
        "SELECT COALESCE(delivery_next_at, 0) FROM nest_hosted_jobs WHERE id = ?1",
        &id,
    )
    .await;
    assert!(next >= now_unix() as i64 + 50, "backed off: {next}");
    deliver_once(&w.srv.state).await;
    let tries = job_int(
        &w,
        "SELECT delivery_attempts FROM nest_hosted_jobs WHERE id = ?1",
        &id,
    )
    .await;
    assert_eq!(tries, 1, "not tried again before the backoff");
}
