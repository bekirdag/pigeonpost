//! Publisher-hosted listings over real sockets: integration submission and service-mailbox
//! ownership, the staff integration review (no attestation), publishing and the catalog's
//! `hosting`, jobs dispatched to the service as one exact message that never names the buyer,
//! result authentication and idempotency, delivery to the buyer with the PDF attached, and the
//! refusal of a leftover card price (credits are in `marketplace_credits_tests`), and the hosted
//! conformance run an integration passes before approval. Synthetic accounts and files only.

use crate::marketplace_hosted::{deliver_once, dispatch_once, JOB_MESSAGE_TYPE};
use crate::marketplace_m2_tests::{certified, code, events_of, publish, reviewer, template};
use crate::org_tests::{serve_with, Person, Srv};
use crate::*;
use reqwest::Method;
use serde_json::{json, Value};

pub(crate) const PDF: &[u8] = b"%PDF-1.4\n1 0 obj << >> endobj\ntrailer << >>\n%%EOF\n";

pub(crate) fn b64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub(crate) struct Mailbox {
    pub(crate) address: String,
    pub(crate) cap: String,
}

async fn mailbox(srv: &Srv, who: &Person, label: &str) -> Mailbox {
    let (s, v) = srv
        .post("/v1/identities", &who.token, json!({ "label": label }))
        .await;
    assert_eq!(s, 201, "{v}");
    Mailbox {
        address: v["address"].as_str().unwrap().to_string(),
        cap: v["capability_token"].as_str().unwrap().to_string(),
    }
}

pub(crate) async fn sql(srv: &Srv, q: &'static str, args: Vec<String>) {
    srv.state
        .store
        .group_tx(move |tx| -> Result<(), crate::groups::GErr> {
            tx.execute(q, rusqlite::params_from_iter(args.iter()))?;
            Ok(())
        })
        .await
        .unwrap();
}

pub(crate) struct World {
    pub(crate) srv: Srv,
    pub(crate) rev: Person,
    pub(crate) ann: Person,
    pub(crate) bob: Person,
    pub(crate) publisher: String,
    pub(crate) template: String,
    pub(crate) service: Mailbox,
    pub(crate) bob_box: Mailbox,
}

async fn world(tag: &str) -> World {
    world_with(tag, None).await
}

/// The hosted world, optionally with a billing bridge stand-in at `bridge` (credits tests).
pub(crate) async fn world_with(tag: &str, bridge: Option<(String, &'static str)>) -> World {
    let srv = serve_with(|s| {
        s.platform_owner = Some(Arc::from("acct_mpowner"));
        s.nests = Arc::new(nests::Runtime::capture());
        s.blobs = Some(Arc::new(blobs::Blobs::for_test("hosted")));
        if let Some((url, token)) = &bridge {
            s.mp_bridge = Arc::new(marketplace_pay::Bridge::http(url, token));
        }
    })
    .await;
    srv.global_flag("marketplace").await;
    let owner = srv.person("mpowner").await;
    // The platform marketplace mailbox: the platform owner's, under its default handle.
    let market = mailbox(&srv, &owner, "marketplace").await;
    sql(
        &srv,
        "UPDATE identities SET handle = '/pigeonpost/marketplace' WHERE address = ?1",
        vec![market.address.clone()],
    )
    .await;
    let rev = reviewer(&srv, &owner, &format!("hrev{tag}")).await;
    let ann = srv.person(&format!("hann{tag}")).await;
    let publisher = certified(&srv, &rev, &ann).await;
    let template = template(&srv, &ann, &publisher, "ai-hr").await;
    let service = mailbox(&srv, &ann, "hr-service").await;
    let bob = srv.person(&format!("hbob{tag}")).await;
    let bob_box = mailbox(&srv, &bob, "bob-inbox").await;
    World {
        srv,
        rev,
        ann,
        bob,
        publisher,
        template,
        service,
        bob_box,
    }
}

fn integration(w: &World, service: &str, version: &str) -> Value {
    json!({
        "template_id": w.template, "version": version, "service_address": service,
        "operations": [{
            "name": "hr.research", "version": "1.0", "title": "HR research",
            "input_schema": { "fields": [
                { "name": "role", "max_chars": 200, "required": true, "label": "Role" },
                { "name": "notes", "max_chars": 2000, "required": false, "multiline": true },
            ]},
            "output": { "kind": "file", "media_type": "application/pdf" },
            "deadline_hours": 48,
        }],
    })
}

/// Submit, approve, list and publish: `(listing id, review id)`.
pub(crate) async fn live_listing(w: &World) -> (String, String) {
    let (s, v) = w
        .srv
        .post(
            &format!("/v1/publishers/{}/integrations", w.publisher),
            &w.ann.token,
            integration(w, &w.service.address, "1.0.0"),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let (release, review, digest) = (
        v["data"]["release_id"].as_str().unwrap().to_string(),
        v["data"]["review_id"].as_str().unwrap().to_string(),
        v["data"]["release_digest"].as_str().unwrap().to_string(),
    );
    let (decision, _) = conform(w, &review, &pdf_result_of(CONFORMING_PDF), &[]).await;
    assert_eq!(decision, "passed");
    let (s, v) = w
        .srv
        .post(
            &format!("/v1/staff/reviews/{review}/decisions"),
            &w.rev.strong,
            json!({ "decision": "approved", "reason": "publisher and integration checked",
                    "release_digest": digest }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["data"]["attestation"], Value::Null);
    let (s, v) = w
        .srv
        .post(
            &format!("/v1/publishers/{}/listings", w.publisher),
            &w.ann.token,
            json!({ "release_id": release, "title": "Wodo AI HR", "summary": "HR research reports",
                    "visibility": "public" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["data"]["hosting"], "publisher");
    assert_eq!(v["data"]["address"], Value::Null);
    let listing = v["data"]["id"].as_str().unwrap().to_string();
    let (s, v) = publish(&w.srv, &w.ann.strong, &w.publisher, &listing, json!({})).await;
    assert_eq!(s, 200, "{v}");
    (listing, review)
}

pub(crate) async fn run(w: &World, listing: &str, input: Value) -> (u16, Value) {
    w.srv
        .post(
            "/v1/marketplace/hosted-jobs",
            &w.bob.token,
            json!({ "listing_id": listing, "operation": "hr.research", "input": input,
                    "deliver_to": w.bob_box.address }),
        )
        .await
}

pub(crate) async fn inbox(srv: &Srv, cap: &str) -> Vec<Value> {
    let (s, v) = srv.get("/v1/inbox?include_read=true", cap).await;
    assert_eq!(s, 200, "{v}");
    v["messages"].as_array().cloned().unwrap_or_default()
}

pub(crate) async fn result(srv: &Srv, token: &str, job: &str, body: Value) -> (u16, Value) {
    srv.post(&format!("/v1/nest-jobs/{job}/result"), token, body)
        .await
}

/// A PDF a buyer can open: objects, a catalog as `/Root`, `startxref` and `%%EOF`.
pub(crate) const CONFORMING_PDF: &[u8] =
    b"%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
2 0 obj << /Type /Pages /Kids [] /Count 0 >> endobj\nxref\n0 3\ntrailer << /Size 3 /Root 1 0 R >>\n\
startxref\n9\n%%EOF\n";

pub(crate) fn pdf_result_of(pdf: &[u8]) -> Value {
    json!({ "status": "succeeded", "summary": "review answer",
            "files": [{ "name": "report.pdf", "media_type": "application/pdf",
                        "content_b64": b64(pdf) }] })
}

/// Run the conformance stage of `review`: the review job reaches the service mailbox, the service
/// posts `answer`, the build plane's scan reports `scan_findings`. Returns the stage's decision
/// and the review job's message.
pub(crate) async fn conform(
    w: &World,
    review: &str,
    answer: &Value,
    scan_findings: &[Value],
) -> (String, Value) {
    crate::marketplace_conformance::work_once(&w.srv.state).await;
    let got = inbox(&w.srv, &w.service.cap).await;
    let msg = got
        .iter()
        .filter_map(|m| m["body"].as_str())
        .filter_map(|b| serde_json::from_str::<Value>(b).ok())
        .rfind(|b| b["review"] == true)
        .unwrap_or_else(|| panic!("no review job: {got:?}"));
    let job = msg["job_id"].as_str().unwrap().to_string();
    let h = &w.srv.state.nests.marketplace;
    h.buildd_answer(202, json!({ "state": "queued" }));
    h.buildd_answer(
        200,
        json!({ "state": "succeeded", "result": { "findings": scan_findings,
                "blocking": scan_findings.len() } }),
    );
    let (s, v) = result(&w.srv, &w.service.cap, &job, answer.clone()).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["review"], true, "{v}");
    crate::marketplace_conformance::work_once(&w.srv.state).await;
    let r = review.to_string();
    let decision = w
        .srv
        .state
        .store
        .group_read(move |c| -> Result<String, crate::groups::GErr> {
            Ok(c.query_row(
                "SELECT decision FROM review_decisions WHERE case_id = ?1
                   AND stage = 'behavioral_test'",
                [&r],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    (decision, msg)
}

pub(crate) fn pdf_result(summary: &str) -> Value {
    json!({ "status": "succeeded", "summary": summary,
            "files": [{ "name": "report.pdf", "media_type": "application/pdf",
                        "content_b64": b64(PDF) }] })
}

/// The service address must be one of the publisher's own mailboxes, and the descriptor is
/// validated; the review is an integration review with one stage.
#[tokio::test]
async fn integrations_name_a_service_mailbox_the_publisher_owns() {
    let w = world("a").await;
    let path = format!("/v1/publishers/{}/integrations", w.publisher);
    let (s, v) = w
        .srv
        .post(
            &path,
            &w.ann.token,
            integration(&w, &w.bob_box.address, "1.0.0"),
        )
        .await;
    assert_eq!((s, code(&v)), (422, "service_address_not_owned"), "{v}");
    let (s, v) = w
        .srv
        .post(
            &path,
            &w.ann.token,
            integration(&w, "/pigeonpost/marketplace", "1.0.0"),
        )
        .await;
    assert_eq!((s, code(&v)), (400, "invalid_integration"), "{v}");
    let mut bad = integration(&w, &w.service.address, "1.0.0");
    bad["operations"][0]["output"]["media_type"] = json!("text/html");
    let (s, v) = w.srv.post(&path, &w.ann.token, bad).await;
    assert_eq!((s, code(&v)), (400, "invalid_integration"), "{v}");
    // Someone off the team cannot submit at all.
    let (s, _) = w
        .srv
        .post(
            &path,
            &w.bob.token,
            integration(&w, &w.service.address, "1.0.0"),
        )
        .await;
    assert_eq!(s, 404);
    let (s, v) = w
        .srv
        .post(
            &path,
            &w.ann.token,
            integration(&w, &w.service.address, "1.0.0"),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let review = v["data"]["review_id"].as_str().unwrap().to_string();
    let (s, v) = w
        .srv
        .get(&format!("/v1/staff/reviews/{review}"), &w.rev.strong)
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["kind"], "integration");
    assert_eq!(v["data"]["integration"]["service_owned"], true);
    let (s, v) = w
        .srv
        .get(
            &format!("/v1/publishers/{}/reviews/{review}", w.publisher),
            &w.ann.token,
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["kind"], "integration");
    let names: Vec<&str> = v["data"]["stages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["behavioral_test", "human_review"]);
    // The analysis plane has no stage to record on it.
    let (s, _) = w
        .srv
        .call(
            Method::POST,
            &format!("/internal/v1/marketplace/reviews/{review}/stages"),
            Some(crate::org_tests::OPS),
            Some(json!({ "stage": "deterministic_scan", "decision": "passed",
                         "evidence": "{}", "evidence_digest": format!("sha256:{}", "0".repeat(64)) })),
        )
        .await;
    assert_ne!(s, 201);
}

/// The whole path: catalog shows `hosting`, install is refused, a job goes to the service as the
/// exact contract message without the buyer, only the service may post its result, the same
/// result is idempotent, and the buyer gets the PDF in their inbox and on the job.
#[tokio::test]
async fn a_hosted_job_goes_to_the_publisher_and_its_pdf_comes_back_to_the_buyer() {
    let w = world("b").await;
    let (listing, _) = live_listing(&w).await;
    let (s, v) = w.srv.get("/v1/marketplace/listings", &w.bob.token).await;
    assert_eq!(s, 200, "{v}");
    let row = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["id"] == listing.as_str())
        .cloned()
        .unwrap();
    assert_eq!(row["hosting"], "publisher");
    let (s, v) = w
        .srv
        .get(&format!("/v1/marketplace/listings/{listing}"), &w.bob.token)
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["hosting"], "publisher");
    assert_eq!(
        v["data"]["operations"][0]["output"]["media_type"],
        "application/pdf"
    );
    let (s, v) = w
        .srv
        .post(
            "/v1/nest-installations",
            &w.bob.token,
            json!({ "listing_id": listing }),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "publisher_hosted"), "{v}");

    let (s, v) = run(&w, &listing, json!({ "notes": "no role" })).await;
    assert_eq!((s, code(&v)), (400, "invalid_operation_payload"), "{v}");
    let (s, v) = run(&w, &listing, json!({ "role": "CTO", "notes": "Istanbul" })).await;
    assert_eq!(s, 201, "{v}");
    let job = v["data"]["job_id"].as_str().unwrap().to_string();
    assert_eq!(v["data"]["execution"], "queued");

    dispatch_once(&w.srv.state).await;
    let got = inbox(&w.srv, &w.service.cap).await;
    let msg = got
        .iter()
        .find(|m| m["body"].as_str().is_some_and(|b| b.contains(&job)))
        .unwrap_or_else(|| panic!("no job message: {got:?}"));
    let body: Value = serde_json::from_str(msg["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["type"], JOB_MESSAGE_TYPE);
    assert_eq!(body["job_id"], job.as_str());
    assert_eq!(body["listing_id"], listing.as_str());
    assert_eq!(body["operation"], "hr.research");
    assert_eq!(body["input"], json!({ "role": "CTO", "notes": "Istanbul" }));
    assert!(body["result_url"]
        .as_str()
        .unwrap()
        .ends_with(&format!("/v1/nest-jobs/{job}/result")));
    assert!(body["deadline"].as_str().is_some());
    assert_eq!(body.as_object().unwrap().len(), 7);
    // The buyer is never named.
    let raw = msg.to_string();
    for secret in [&w.bob.account, &w.bob_box.address, &w.bob.address] {
        assert!(!raw.contains(secret.as_str()), "buyer leaked: {raw}");
    }
    // A second pass sends nothing new.
    dispatch_once(&w.srv.state).await;
    let again = inbox(&w.srv, &w.service.cap).await;
    assert_eq!(
        again
            .iter()
            .filter(|m| m["body"].as_str().is_some_and(|b| b.contains(&job)))
            .count(),
        1
    );
    let (_, v) = w
        .srv
        .get(&format!("/v1/marketplace/hosted-jobs/{job}"), &w.bob.token)
        .await;
    assert_eq!(v["data"]["execution"], "running");

    // Only the service mailbox may answer (its capability token, or the publisher's account token
    // acting as it); the buyer and the buyer's mailbox get 404.
    for token in [&w.bob.token, &w.bob_box.cap] {
        let (s, _) = result(&w.srv, token, &job, pdf_result("done")).await;
        assert_eq!(s, 404);
    }
    let (s, v) = result(
        &w.srv,
        &w.service.cap,
        &job,
        json!({ "status": "succeeded", "files": [{ "name": "x.pdf",
                "media_type": "application/pdf", "content_b64": b64(b"<html>") }] }),
    )
    .await;
    assert_eq!((s, code(&v)), (422, "file_unreadable"), "{v}");
    let (s, v) = result(&w.srv, &w.service.cap, &job, pdf_result("Three candidates")).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["replay"], false);
    let (s, v) = result(&w.srv, &w.service.cap, &job, pdf_result("Three candidates")).await;
    assert_eq!((s, v["replay"].clone()), (200, json!(true)), "{v}");
    let (s, v) = result(&w.srv, &w.service.cap, &job, pdf_result("Other")).await;
    assert_eq!((s, code(&v)), (409, "result_recorded"), "{v}");

    deliver_once(&w.srv.state).await;
    let mail = inbox(&w.srv, &w.bob_box.cap).await;
    let note = mail
        .iter()
        .find(|m| m["body"].as_str().is_some_and(|b| b.contains(&job)))
        .unwrap_or_else(|| panic!("no result message: {mail:?}"));
    assert!(note["body"].as_str().unwrap().contains("Three candidates"));
    assert_eq!(
        note["attachments"][0]["media_type"], "application/pdf",
        "{note}"
    );
    let (_, v) = w
        .srv
        .get(&format!("/v1/marketplace/hosted-jobs/{job}"), &w.bob.token)
        .await;
    assert_eq!(v["data"]["execution"], "succeeded");
    assert_eq!(v["data"]["delivery"], "delivered");
    let path = v["data"]["result"]["files"][0]["download_path"]
        .as_str()
        .unwrap()
        .to_string();
    let resp = w
        .srv
        .http
        .get(format!("{}{path}", w.srv.public))
        .bearer_auth(&w.bob.token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(resp.bytes().await.unwrap().as_ref(), PDF);
    // Another account cannot see the job or its file.
    assert_eq!(
        w.srv
            .get(&format!("/v1/marketplace/hosted-jobs/{job}"), &w.ann.token)
            .await
            .0,
        404
    );
    let kinds: Vec<String> = events_of(&w.srv, &w.bob.account)
        .await
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    assert!(
        kinds.contains(&"nest.job.admitted".to_string()),
        "{kinds:?}"
    );
    assert!(
        kinds.contains(&"nest.job.result_ready".to_string()),
        "{kinds:?}"
    );
    // Staff see it without the input.
    let (s, v) = w.srv.get("/v1/staff/hosted-jobs", &w.rev.strong).await;
    assert_eq!(s, 200, "{v}");
    assert!(!v.to_string().contains("Istanbul"));
    assert_eq!(v["data"][0]["job_id"], job.as_str());
}

/// A leftover card price is not a way to run a publisher-hosted service (they are sold per job,
/// for credits); and a service mailbox that leaves the publisher stops dispatch.
#[tokio::test]
async fn card_priced_listings_take_no_jobs_and_a_lost_service_mailbox_fails_the_job() {
    let w = world("c").await;
    let (listing, _) = live_listing(&w).await;
    let (s, v) = run(&w, &listing, json!({ "role": "CFO" })).await;
    assert_eq!(s, 201, "{v}");
    let job = v["data"]["job_id"].as_str().unwrap().to_string();
    // The mailbox changes hands before dispatch.
    sql(
        &w.srv,
        "UPDATE identities SET account_id = ?2 WHERE address = ?1",
        vec![w.service.address.clone(), w.bob.account.clone()],
    )
    .await;
    dispatch_once(&w.srv.state).await;
    let (_, v) = w
        .srv
        .get(&format!("/v1/marketplace/hosted-jobs/{job}"), &w.bob.token)
        .await;
    assert_eq!(v["data"]["execution"], "failed");
    assert_eq!(v["data"]["error_code"], "service_unavailable");
    sql(
        &w.srv,
        "INSERT INTO nest_listing_prices (listing_id, amount_minor, currency, term, updated_by,
            updated_at) VALUES (?1, 99900, 'TRY', 'one_off', 'acct_x', 1)",
        vec![listing.clone()],
    )
    .await;
    let (s, v) = run(&w, &listing, json!({ "role": "CFO" })).await;
    assert_eq!((s, code(&v)), (409, "not_on_sale"), "{v}");
}

/// The conformance run: one free review job per operation through the hosted-job path, marked
/// `review: true`; staff cannot approve before it passes; a clean PDF that the build plane's
/// scan clears passes, and the publisher sees the stage.
#[tokio::test]
async fn an_integration_passes_the_hosted_conformance_run_before_approval() {
    let w = world("c").await;
    let (s, v) = w
        .srv
        .post(
            &format!("/v1/publishers/{}/integrations", w.publisher),
            &w.ann.token,
            integration(&w, &w.service.address, "1.0.0"),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let review = v["data"]["review_id"].as_str().unwrap().to_string();
    let digest = v["data"]["release_digest"].as_str().unwrap().to_string();
    // Staff cannot approve before the run.
    let decide = |d: &'static str| {
        let (srv, tok, review, digest) =
            (&w.srv, w.rev.strong.clone(), review.clone(), digest.clone());
        async move {
            srv.post(
                &format!("/v1/staff/reviews/{review}/decisions"),
                &tok,
                json!({ "decision": d, "reason": "checked", "release_digest": digest }),
            )
            .await
        }
    };
    let (s, v) = decide("approved").await;
    assert_eq!((s, code(&v)), (409, "conformance_required"), "{v}");

    let (decision, msg) = conform(&w, &review, &pdf_result_of(CONFORMING_PDF), &[]).await;
    assert_eq!(decision, "passed");
    // The review job is the contract's message, marked as a review, with no listing or buyer.
    assert_eq!(msg["type"], JOB_MESSAGE_TYPE);
    assert_eq!(msg["review"], true);
    assert_eq!(msg["listing_id"], Value::Null);
    assert_eq!(msg["operation"], "hr.research");
    assert!(msg["input"]["role"]
        .as_str()
        .unwrap()
        .starts_with("Pigeonpost review job"));
    // The result's files went to the build plane's malware scan.
    let calls = w.srv.state.nests.marketplace.buildd_calls();
    let scan = calls
        .iter()
        .find(|c| c.method == "POST" && c.path == "/v1/scans")
        .unwrap();
    let body = scan.body.as_ref().unwrap();
    assert_eq!(body["kind"], "files");
    assert_eq!(body["files"][0]["content_b64"], b64(CONFORMING_PDF));
    assert!(calls
        .iter()
        .any(|c| c.method == "GET" && c.path.starts_with("/v1/scans/scn_")));
    // Nothing was charged and nothing reached a buyer.
    let jobs: i64 = w
        .srv
        .state
        .store
        .group_read(|c| -> Result<i64, crate::groups::GErr> {
            Ok(c.query_row("SELECT COUNT(*) FROM nest_hosted_jobs", [], |r| r.get(0))?)
        })
        .await
        .unwrap();
    assert_eq!(jobs, 0);
    let (s, v) = w
        .srv
        .get(
            &format!("/v1/publishers/{}/reviews/{review}", w.publisher),
            &w.ann.token,
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["stages"][0]["name"], "behavioral_test");
    assert_eq!(v["data"]["stages"][0]["status"], "passed");
    let (s, v) = decide("approved").await;
    assert_eq!(s, 201, "{v}");
}

/// A result that is not a PDF a buyer can open, a malware hit on the result, or a mailbox the
/// publisher no longer owns fails the stage and blocks the review.
#[tokio::test]
async fn a_non_conformant_integration_fails_the_run_and_is_blocked() {
    for (tag, case) in [("d", "pdf"), ("e", "malware")] {
        let w = world(tag).await;
        let (s, v) = w
            .srv
            .post(
                &format!("/v1/publishers/{}/integrations", w.publisher),
                &w.ann.token,
                integration(&w, &w.service.address, "1.0.0"),
            )
            .await;
        assert_eq!(s, 201, "{v}");
        let review = v["data"]["review_id"].as_str().unwrap().to_string();
        let (answer, hits) = match case {
            // Passes the result route's own check, not the conformance one.
            "pdf" => (pdf_result_of(PDF), vec![]),
            _ => (
                pdf_result_of(CONFORMING_PDF),
                vec![
                    json!({ "id": "clamav:Win.Test.EICAR_HDB-1:result/0.bin", "tool": "clamav",
                             "rule": "malware.signature", "severity": "critical",
                             "message": "matched", "blocking": true }),
                ],
            ),
        };
        if case == "pdf" {
            // No scan is asked for: the job fails on its own findings.
            crate::marketplace_conformance::work_once(&w.srv.state).await;
            let got = inbox(&w.srv, &w.service.cap).await;
            let job = got
                .iter()
                .filter_map(|m| serde_json::from_str::<Value>(m["body"].as_str()?).ok())
                .find(|b| b["review"] == true)
                .unwrap()["job_id"]
                .as_str()
                .unwrap()
                .to_string();
            let (s, v) = result(&w.srv, &w.service.cap, &job, answer).await;
            assert_eq!(s, 200, "{v}");
            assert_eq!(v["data"]["execution"], "failed");
            crate::marketplace_conformance::work_once(&w.srv.state).await;
        } else {
            let (decision, _) = conform(&w, &review, &answer, &hits).await;
            assert_eq!(decision, "failed");
        }
        let (s, v) = w
            .srv
            .get(
                &format!("/v1/publishers/{}/reviews/{review}/findings", w.publisher),
                &w.ann.token,
            )
            .await;
        assert_eq!(s, 200, "{v}");
        let stage = &v["data"]["stages"][0];
        assert_eq!(stage["stage"], "behavioral_test", "{v}");
        assert_eq!(stage["decision"], "failed", "{v}");
        let rules: Vec<&str> = stage["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["rule"].as_str().unwrap())
            .collect();
        match case {
            "pdf" => assert!(
                rules.contains(&"pdf.xref") && rules.contains(&"pdf.root"),
                "{rules:?}"
            ),
            _ => {
                assert_eq!(rules, ["malware.match"], "{v}");
                assert!(
                    !v.to_string().contains("EICAR"),
                    "the signature stays with staff"
                );
            }
        }
        let (s, v) = w
            .srv
            .get(
                &format!("/v1/publishers/{}/reviews/{review}", w.publisher),
                &w.ann.token,
            )
            .await;
        assert_eq!(s, 200);
        assert_eq!(v["data"]["status"], "changes_requested", "{v}");
    }
}
