//! Marketplace M1 over real sockets: build intake, analysis stages, staff decision and
//! attestation (with the first-party exception), native jobs with zero-priced authorizations,
//! the shared Nest dispatcher, quarantine and revocation. Synthetic accounts only.
//!
//! Plan exit IDs covered here: SEC-01 (review and attestation), SEC-02 (behaviour stage blocks),
//! SEC-05 (quarantine and revocation), JOB-01…06, AI-01 (advisory, self-hosted), ISO-01 on a shared
//! listing (one buyer context per account). The worker-host side is in
//! `crates/pigeonpost-nests/tests/marketplace_m1.rs`.

use crate::marketplace_m1::{canonical_json, digest_of, dispatch_once, poll_once, release_digest};
use crate::org_tests::{serve_with, Person, Srv, OPS};
use crate::*;
use reqwest::Method;
use serde_json::{json, Value};

pub(super) const OWNER_ACCOUNT: &str = "acct_mpowner";
pub(super) const ADDRESS: &str = "/pigeonpost/hr-policy-qa";
pub(super) const OP: &str = "hr.policy.ask";

pub(super) fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or_default()
}

pub(super) async fn setup() -> (Srv, Person) {
    let srv = serve_with(|s| {
        s.platform_owner = Some(Arc::from(OWNER_ACCOUNT));
        s.nests = Arc::new(nests::Runtime::capture());
    })
    .await;
    let owner = srv.person("mpowner").await;
    assert_eq!(owner.account, OWNER_ACCOUNT);
    (srv, owner)
}

/// One request with extra headers; the raw body is sent as given.
pub(super) async fn send(
    srv: &Srv,
    method: Method,
    path: &str,
    token: Option<&str>,
    headers: &[(&str, &str)],
    body: Option<String>,
) -> (u16, reqwest::header::HeaderMap, Value) {
    let base = if path.starts_with("/internal/") {
        &srv.internal
    } else {
        &srv.public
    };
    let mut req = srv
        .http
        .request(method, format!("{base}{path}"))
        .header("x-forwarded-for", &srv.ip)
        .header("content-type", "application/json");
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    if let Some(b) = body {
        req = req.body(b);
    }
    let resp = req.send().await.unwrap();
    let status = resp.status().as_u16();
    let h = resp.headers().clone();
    let text = resp.text().await.unwrap();
    (
        status,
        h,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    )
}

pub(super) fn d(c: char) -> String {
    format!("sha256:{}", c.to_string().repeat(64))
}

pub(super) fn build_record(version: &str, image_char: char) -> Value {
    let sbom =
        canonical_json(&json!({"bomFormat": "CycloneDX", "specVersion": "1.5", "version": 1}));
    let sbom_digest = digest_of(sbom.as_bytes());
    let image_digest = d(image_char);
    let (source, vendor, manifest) = (d('1'), d('2'), d('3'));
    json!({
        "manifest": {
            "name": "hr-policy-qa",
            "version": version,
            "title": "HR policy Q&A",
            "summary": "Answers from a bundled, synthetic handbook.",
            "licence": "MIT",
            "entrypoint": "/usr/local/bin/hr-policy-qa",
            "listing_address": ADDRESS,
            "egress": ["broker"],
            "capabilities": [],
            "syscalls": [],
            "external_writes": false,
            "operations": [{
                "name": OP,
                "version": "1.0",
                "fields": [{"name": "question", "max_chars": 2000, "required": true}],
            }],
            "licences": {"hr-policy-qa": "MIT"},
        },
        "manifest_digest": manifest,
        "source_digest": source,
        "vendor_digest": vendor,
        "sbom": sbom,
        "sbom_digest": sbom_digest,
        "image": format!("localhost/pigeonpost-release-hr-policy-qa@{image_digest}"),
        "image_digest": image_digest,
        "release_digest": release_digest(&source, &vendor, &manifest, &sbom_digest, &image_digest),
        "isolation": {"runtime": "runsc", "rootless": true, "build_network": "none"},
    })
}

pub(super) struct Release {
    pub(super) release: String,
    pub(super) review: String,
    pub(super) listing: String,
    pub(super) digest: String,
}

pub(super) async fn register(srv: &Srv, version: &str, image_char: char) -> Release {
    let record = build_record(version, image_char);
    let (s, v) = srv
        .call(
            Method::POST,
            "/internal/v1/marketplace/builds",
            Some(OPS),
            Some(record.clone()),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    Release {
        release: v["data"]["release_id"].as_str().unwrap().into(),
        review: v["data"]["review_id"].as_str().unwrap().into(),
        listing: v["data"]["listing_id"].as_str().unwrap().into(),
        digest: record["release_digest"].as_str().unwrap().into(),
    }
}

pub(super) fn evidence(stage: &str, decision: &str, model: Option<&str>) -> String {
    let mut facts = json!({ "ran": "synthetic" });
    if let Some(m) = model {
        facts["model"] = json!(m);
    }
    canonical_json(&json!({
        "stage": stage,
        "decision": decision,
        "reason": "test",
        "findings": [],
        "evidence": facts,
    }))
}

pub(super) async fn stage(
    srv: &Srv,
    review: &str,
    stage: &str,
    decision: &str,
    model: Option<&str>,
) -> (u16, Value) {
    let ev = evidence(stage, decision, model);
    srv.call(
        Method::POST,
        &format!("/internal/v1/marketplace/reviews/{review}/stages"),
        Some(OPS),
        Some(json!({
            "stage": stage,
            "decision": decision,
            "reason": "test",
            "evidence_digest": digest_of(ev.as_bytes()),
            "evidence": ev,
        })),
    )
    .await
}

pub(super) async fn pass_analysis(srv: &Srv, review: &str) {
    for (st, dec, model) in [
        ("deterministic_scan", "passed", None),
        ("behavioral_test", "passed", None),
        ("ai_analysis", "advisory", Some("qwen-latest-flash-next")),
    ] {
        let (s, v) = stage(srv, review, st, dec, model).await;
        assert_eq!(s, 201, "{st}: {v}");
    }
}

pub(super) async fn assign_reviewer(srv: &Srv, owner: &Person, account: &str) {
    let (s, v) = srv
        .post(
            "/v1/staff/roles",
            &owner.strong,
            json!({ "account_id": account, "role": "reviewer" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
}

pub(super) async fn decide(srv: &Srv, who: &str, r: &Release, decision: &str) -> (u16, Value) {
    srv.post(
        &format!("/v1/staff/reviews/{}/decisions", r.review),
        who,
        json!({ "decision": decision, "reason": "reviewed", "release_digest": r.digest }),
    )
    .await
}

/// A released, attested first-party release with its published listing.
pub(super) async fn attested(srv: &Srv, owner: &Person) -> Release {
    assign_reviewer(srv, owner, &owner.account).await;
    let r = register(srv, "1.0.0", 'a').await;
    pass_analysis(srv, &r.review).await;
    let (s, v) = decide(srv, &owner.strong, &r, "approved").await;
    assert_eq!(s, 201, "{v}");
    r
}

pub(super) fn deadline() -> String {
    iso_secs(now_unix() + 3600)
}

fn iso_secs(t: u64) -> String {
    crate::marketplace::iso(t as i64)
}

pub(super) async fn quote(srv: &Srv, token: &str) -> (u16, Value) {
    srv.post(
        "/v1/nest-jobs/quote",
        token,
        json!({ "to": ADDRESS, "operation": OP, "operation_version": "1.0" }),
    )
    .await
}

pub(super) async fn authorization(srv: &Srv, token: &str, quote: &str, key: &str) -> (u16, Value) {
    let (s, _, v) = send(
        srv,
        Method::POST,
        "/v1/nest-payment-authorizations",
        Some(token),
        &[("idempotency-key", key)],
        Some(json!({ "quote_id": quote }).to_string()),
    )
    .await;
    (s, v)
}

/// Quote and authorize, then return a ready job body.
pub(super) async fn job_body(srv: &Srv, token: &str, key: &str, question: &str) -> Value {
    let (s, q) = quote(srv, token).await;
    assert_eq!(s, 201, "{q}");
    assert_eq!(q["data"]["amount_minor"], 0);
    let qid = q["data"]["quote_id"].as_str().unwrap().to_string();
    let (s, a) = authorization(srv, token, &qid, key).await;
    assert_eq!(s, 201, "{a}");
    json!({
        "type": "nest.job.request",
        "version": 1,
        "to": ADDRESS,
        "operation": OP,
        "operation_version": "1.0",
        "quote_id": qid,
        "payment_authorization": a["data"]["payment_authorization"],
        "payload": { "question": question },
        "deadline_at": deadline(),
    })
}

pub(super) async fn submit(
    srv: &Srv,
    token: &str,
    key: &str,
    body: &Value,
) -> (u16, reqwest::header::HeaderMap, Value) {
    send(
        srv,
        Method::POST,
        "/v1/nest-jobs",
        Some(token),
        &[("idempotency-key", key)],
        Some(body.to_string()),
    )
    .await
}

pub(super) async fn flag_for(srv: &Srv, account: &str) {
    let (s, v) = srv
        .call(
            Method::PUT,
            "/internal/v1/features/marketplace",
            Some(OPS),
            Some(json!({ "scope": "account", "account_id": account, "enabled": true })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
}

// ---- the flag ------------------------------------------------------------------------------------

#[tokio::test]
async fn nothing_in_m1_works_while_the_flag_is_off() {
    let (srv, owner) = setup().await;
    for (method, path) in [
        (Method::POST, "/v1/nest-jobs/quote"),
        (Method::POST, "/v1/nest-payment-authorizations"),
        (Method::POST, "/v1/nest-jobs"),
        (Method::GET, "/v1/nest-jobs/job_aaaaaaaaaaaaaaaaaaaaaaaaaa"),
        (
            Method::GET,
            "/v1/staff/reviews/rvw_aaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
        (
            Method::POST,
            "/v1/staff/reviews/rvw_aaaaaaaaaaaaaaaaaaaaaaaaaa/decisions",
        ),
        (
            Method::POST,
            "/v1/staff/releases/rel_aaaaaaaaaaaaaaaaaaaaaaaaaa/security",
        ),
    ] {
        let (s, _, v) = send(
            &srv,
            method.clone(),
            path,
            Some(&owner.strong),
            &[("idempotency-key", "flag-off-0123456789")],
            Some("{}".into()),
        )
        .await;
        assert_eq!(
            (s, code(&v)),
            (404, "feature_disabled"),
            "{method} {path}: {v}"
        );
    }
    // The build plane cannot record anything until the owner's account has the flag.
    let (s, v) = srv
        .call(
            Method::POST,
            "/internal/v1/marketplace/builds",
            Some(OPS),
            Some(build_record("1.0.0", 'a')),
        )
        .await;
    assert_eq!((s, code(&v)), (404, "feature_disabled"), "{v}");
    // And without the ops token it is not reachable at all.
    flag_for(&srv, OWNER_ACCOUNT).await;
    let (s, _) = srv
        .call(
            Method::POST,
            "/internal/v1/marketplace/builds",
            Some("svc_wrong_0123456789abcdef0123456789abcdef"),
            Some(build_record("1.0.0", 'a')),
        )
        .await;
    assert!(s == 401 || s == 403, "{s}");
}

// ---- build intake and analysis -------------------------------------------------------------------

#[tokio::test]
async fn the_build_record_must_be_internally_consistent() {
    let (srv, _owner) = setup().await;
    srv.global_flag("marketplace").await;
    type Tweak = fn(&mut Value);
    let cases: Vec<(&str, Tweak)> = vec![
        ("sbom changed after hashing", |r| {
            r["sbom"] = json!("{\"bomFormat\":\"CycloneDX\"}")
        }),
        ("release digest over other digests", |r| {
            r["source_digest"] = json!(d('9'))
        }),
        ("image not at its digest", |r| {
            r["image"] = json!("localhost/x:latest")
        }),
        ("no operations", |r| r["manifest"]["operations"] = json!([])),
        ("an unknown field", |r| r["status"] = json!("approved")),
    ];
    for (what, tweak) in cases {
        let mut record = build_record("1.0.0", 'a');
        tweak(&mut record);
        let (s, v) = srv
            .call(
                Method::POST,
                "/internal/v1/marketplace/builds",
                Some(OPS),
                Some(record),
            )
            .await;
        assert_eq!((s, code(&v)), (400, "invalid_request"), "{what}: {v}");
    }
    let r = register(&srv, "1.0.0", 'a').await;
    assert!(r.release.starts_with("rel_") && r.review.starts_with("rvw_"));
    // The same version twice is refused: a release is immutable.
    let (s, v) = srv
        .call(
            Method::POST,
            "/internal/v1/marketplace/builds",
            Some(OPS),
            Some(build_record("1.0.0", 'b')),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "release_exists"), "{v}");
    // The publisher is Pigeonpost's own, made by the build plane, and its listing is a draft.
    let rel = r.release.clone();
    let (kind, status, listing): (String, String, String) = srv
        .state
        .store
        .group_read(move |c| -> Result<_, crate::groups::GErr> {
            Ok(c.query_row(
                "SELECT p.owner_kind, p.status, l.status FROM nest_releases r
                   JOIN publisher_profiles p ON p.id = r.publisher_id
                   JOIN nest_listings l ON l.release_id = r.id WHERE r.id = ?1",
                [&rel],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(
        (kind.as_str(), status.as_str(), listing.as_str()),
        ("platform", "approved", "draft")
    );
}

/// AI-01: the AI review is advisory and runs on the self-hosted model only. SEC-02: a failed
/// behaviour test (or scan) blocks the release.
#[tokio::test]
async fn analysis_stages_are_honest_and_a_failure_blocks_the_review() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    assign_reviewer(&srv, &owner, &owner.account).await;
    let r = register(&srv, "1.0.0", 'a').await;
    // AI never passes, fails or approves; only the self-hosted model counts.
    for (decision, model) in [
        ("passed", Some("qwen-latest-flash-next")),
        ("failed", Some("qwen-latest-flash-next")),
        ("approved", Some("qwen-latest-flash-next")),
        ("advisory", Some("openai/gpt-4o")),
        ("advisory", Some("deepseek/deepseek-chat")),
        ("advisory", None),
    ] {
        let (s, v) = stage(&srv, &r.review, "ai_analysis", decision, model).await;
        assert_eq!(
            (s, code(&v)),
            (400, "invalid_request"),
            "{decision} {model:?}: {v}"
        );
    }
    // A human decision is not an analysis stage, and evidence must match its digest.
    let (s, _) = stage(&srv, &r.review, "human_review", "approved", None).await;
    assert_eq!(s, 400);
    let ev = evidence("deterministic_scan", "passed", None);
    let (s, v) = srv
        .call(
            Method::POST,
            &format!("/internal/v1/marketplace/reviews/{}/stages", r.review),
            Some(OPS),
            Some(json!({
                "stage": "deterministic_scan", "decision": "passed", "reason": "x",
                "evidence": ev, "evidence_digest": d('0'),
            })),
        )
        .await;
    assert_eq!((s, code(&v)), (400, "invalid_request"), "{v}");
    // Approval needs every stage: refused, and told which are missing.
    let (s, v) = decide(&srv, &owner.strong, &r, "approved").await;
    assert_eq!((s, code(&v)), (409, "stages_incomplete"), "{v}");
    assert!(
        v["error"]["details"]["stages"].as_array().unwrap().len() >= 3,
        "{v}"
    );

    let (s, _) = stage(&srv, &r.review, "deterministic_scan", "passed", None).await;
    assert_eq!(s, 201);
    let (s, _) = stage(
        &srv,
        &r.review,
        "ai_analysis",
        "advisory",
        Some("qwen-latest-flash-next"),
    )
    .await;
    assert_eq!(s, 201);
    // SEC-02: the honeytoken behaviour test failed. The review is blocked and cannot be approved.
    let (s, _) = stage(&srv, &r.review, "behavioral_test", "failed", None).await;
    assert_eq!(s, 201);
    let (s, v) = decide(&srv, &owner.strong, &r, "approved").await;
    assert_eq!((s, code(&v)), (409, "review_closed"), "{v}");
    let (s, v) = stage(&srv, &r.review, "behavioral_test", "passed", None).await;
    assert_eq!(
        (s, code(&v)),
        (409, "review_closed"),
        "a blocked review takes no more evidence: {v}"
    );
}

// ---- staff decision and attestation ---------------------------------------------------------------

/// SEC-01: a named staff member approves with step-up; the decision is recorded as first party
/// and an attestation is written over the release digest.
#[tokio::test]
async fn staff_approve_and_attest_a_first_party_release() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    assign_reviewer(&srv, &owner, &owner.account).await;
    let plain = srv.person("mpplain").await;
    let r = register(&srv, "1.0.0", 'a').await;
    pass_analysis(&srv, &r.review).await;
    let path = format!("/v1/staff/reviews/{}", r.review);

    // The review is visible to the reviewer, first party, with every stage and its evidence.
    let (s, v) = srv.get(&path, &owner.strong).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["first_party"], true);
    assert_eq!(v["data"]["release_digest"], r.digest.as_str());
    let stages: Vec<&str> = v["data"]["stages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["stage"].as_str().unwrap())
        .collect();
    assert_eq!(
        stages,
        [
            "intake",
            "deterministic_scan",
            "behavioral_test",
            "ai_analysis"
        ]
    );

    // Who may not decide: not staff, staff without step-up, automation.
    let (s, v) = decide(&srv, &plain.strong, &r, "approved").await;
    assert_eq!((s, code(&v)), (403, "staff_required"), "{v}");
    let (s, v) = decide(&srv, &owner.token, &r, "approved").await;
    assert_eq!((s, code(&v)), (401, "step_up_required"), "{v}");
    let (key, stored) = new_api_key();
    srv.state
        .store
        .add_api_key(owner.account.clone(), stored, now_unix())
        .await
        .unwrap();
    let (s, v) = decide(&srv, &key, &r, "approved").await;
    assert_eq!((s, code(&v)), (403, "human_required"), "{v}");
    // A digest other than the one under review is refused.
    let (s, v) = srv
        .post(
            &format!("{path}/decisions"),
            &owner.strong,
            json!({ "decision": "approved", "reason": "ok", "release_digest": d('f') }),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "release_digest_mismatch"), "{v}");

    let (s, v) = decide(&srv, &owner.strong, &r, "approved").await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["data"]["first_party"], true);
    let statement_digest = v["data"]["attestation"]["statement_digest"]
        .as_str()
        .unwrap()
        .to_string();
    // The attestation covers this release digest and this decision, and hashes to its digest.
    let rel = r.release.clone();
    let (statement, stored_digest, first_party): (String, String, i64) = srv
        .state
        .store
        .group_read(move |c| -> Result<_, crate::groups::GErr> {
            Ok(c.query_row(
                "SELECT statement, statement_digest, first_party FROM release_attestations WHERE release_id = ?1",
                [&rel],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(stored_digest, statement_digest);
    assert_eq!(digest_of(statement.as_bytes()), statement_digest);
    let s: Value = serde_json::from_str(&statement).unwrap();
    assert_eq!(s["release_digest"], r.digest.as_str());
    assert_eq!(s["attester"], owner.account.as_str());
    assert_eq!(s["decision_id"], v["data"]["decision_id"]);
    assert_eq!(first_party, 1);
    // The listing is published; a second decision is refused; the decision is on the record.
    let (s, v) = srv
        .get(
            &format!("/v1/marketplace/listings/{}", r.listing),
            &owner.token,
        )
        .await;
    assert_eq!(
        (s, v["data"]["status"].as_str()),
        (200, Some("published")),
        "{v}"
    );
    let (s, v) = decide(&srv, &owner.strong, &r, "approved").await;
    assert_eq!((s, code(&v)), (409, "review_closed"), "{v}");
    let audit = srv
        .state
        .store
        .audit_for_actor(Some(owner.account.clone()), 10)
        .await
        .unwrap();
    assert!(audit
        .iter()
        .any(|a| a.action == "marketplace.review.decision"));
}

/// The other side of the owner decision: a third-party publisher keeps the conflict rule, and its
/// review is never first party.
#[tokio::test]
async fn third_party_publishers_keep_the_conflict_rule() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    let (rev, other) = (srv.person("mprev3").await, srv.person("mpother3").await);
    assign_reviewer(&srv, &owner, &rev.account).await;
    assign_reviewer(&srv, &owner, &other.account).await;
    // A publisher the reviewer owns, with a release under review.
    let (s, v) = srv
        .post(
            "/v1/publisher-applications",
            &rev.token,
            json!({ "display_name": "Rev Apps" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let publisher = v["data"]["id"].as_str().unwrap().to_string();
    let p = publisher.clone();
    srv.state
        .store
        .group_tx(move |tx| -> Result<(), crate::groups::GErr> {
            tx.execute_batch(&format!(
                "INSERT INTO nest_templates (id, publisher_id, slug, name, created_by, created_at)
                   VALUES ('tpl_third', '{p}', 'third', 'Third', 'author', 0);
                 INSERT INTO nest_releases (id, template_id, publisher_id, version, image_digest, status, created_by, created_at, release_digest)
                   VALUES ('rel_third', 'tpl_third', '{p}', '1.0.0', 'sha256:00', 'submitted', 'author', 0, 'sha256:{z}');
                 INSERT INTO review_cases (id, release_id, publisher_id, status, created_at, updated_at)
                   VALUES ('rvw_thirdaaaaaaaaaaaaaaaaaaaaa', 'rel_third', '{p}', 'open', 0, 0);",
                z = "0".repeat(64)
            ))?;
            Ok(())
        })
        .await
        .unwrap();
    let path = "/v1/staff/reviews/rvw_thirdaaaaaaaaaaaaaaaaaaaaa";
    let (s, v) = srv.get(path, &rev.strong).await;
    assert_eq!((s, code(&v)), (403, "staff_conflict"), "{v}");
    let (s, v) = srv
        .post(
            &format!("{path}/decisions"),
            &rev.strong,
            json!({ "decision": "approved", "reason": "mine", "release_digest": format!("sha256:{}", "0".repeat(64)) }),
        )
        .await;
    assert_eq!((s, code(&v)), (403, "staff_conflict"), "{v}");
    let (s, v) = srv
        .post(
            "/v1/staff/releases/rel_third/security",
            &rev.strong,
            json!({ "action": "quarantine", "reason": "mine" }),
        )
        .await;
    assert_eq!((s, code(&v)), (403, "staff_conflict"), "{v}");
    // An unrelated reviewer may read it, and it is not first party.
    let (s, v) = srv.get(path, &other.strong).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["first_party"], false);
    // The schema refuses a first-party decision on a third-party case.
    let refused = srv
        .state
        .store
        .group_tx(|tx| -> Result<(), crate::groups::GErr> {
            tx.execute(
                "INSERT INTO review_decisions (id, case_id, reviewer, decision, reason, created_at, stage, first_party)
                 VALUES ('dec_x', 'rvw_thirdaaaaaaaaaaaaaaaaaaaaa', 'acct_mpother3', 'approved', 'x', 0, 'human_review', 1)",
                [],
            )?;
            Ok(())
        })
        .await;
    assert!(refused.is_err());
    // A review that does not exist is a plain 404 for staff, never a conflict.
    let (s, v) = srv
        .get(
            "/v1/staff/reviews/rvw_missingaaaaaaaaaaaaaaaaaa",
            &rev.strong,
        )
        .await;
    assert_eq!((s, code(&v)), (404, "not_found"), "{v}");
}

// ---- native jobs ------------------------------------------------------------------------------------

/// JOB-01 and ISO-01: quote, authorize, submit, dispatch to the buyer's own worker, collect.
#[tokio::test]
async fn a_job_runs_end_to_end_on_the_shared_listing() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    let r = attested(&srv, &owner).await;
    let (ann, bob) = (srv.person("mpann1").await, srv.person("mpbob1").await);

    let body = job_body(
        &srv,
        &ann.token,
        "ann-auth-0000000001",
        "How many days of annual leave?",
    )
    .await;
    let (s, h, v) = submit(&srv, &ann.token, "ann-job-00000000001", &body).await;
    assert_eq!(s, 202, "{v}");
    assert!(h.get("idempotent-replayed").is_none());
    let job = v["data"]["job_id"].as_str().unwrap().to_string();
    assert!(job.starts_with("job_") && v["data"]["admission"] == "admitted");
    let path = format!("/v1/nest-jobs/{job}");
    let (s, v) = srv.get(&path, &ann.token).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (
            v["data"]["admission"].as_str(),
            v["data"]["execution"].as_str(),
            v["data"]["settlement"].as_str()
        ),
        (Some("admitted"), Some("queued"), Some("none")),
        "zero-priced: nothing reserved, nothing settled"
    );
    assert_eq!(v["data"]["to"], ADDRESS);

    // A second buyer of the same listing.
    let body_b = job_body(
        &srv,
        &bob.token,
        "bob-auth-0000000001",
        "Remote work rules?",
    )
    .await;
    let (s, _, v) = submit(&srv, &bob.token, "bob-job-00000000001", &body_b).await;
    assert_eq!(s, 202, "{v}");

    assert_eq!(dispatch_once(&srv.state).await, 2);
    let calls: Vec<_> = srv
        .state
        .nests
        .calls()
        .into_iter()
        .filter(|c| c.path == "/v1/mp/jobs")
        .collect();
    assert_eq!(calls.len(), 2);
    let ctx: Vec<String> = calls
        .iter()
        .map(|c| {
            c.body.as_ref().unwrap()["buyer_context"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_ne!(
        ctx[0], ctx[1],
        "ISO-01: each buyer has its own context on one listing"
    );
    for c in &calls {
        let b = c.body.as_ref().unwrap();
        assert_eq!(b["listing_id"], r.listing.as_str());
        assert_eq!(b["release"]["release_digest"], r.digest.as_str());
        let statement = b["release"]["statement"].as_str().unwrap();
        assert_eq!(
            digest_of(statement.as_bytes()),
            b["release"]["statement_digest"].as_str().unwrap()
        );
        // The account is never handed to the worker host, only its opaque context.
        assert!(!b.to_string().contains(&ann.account) && !b.to_string().contains(&bob.account));
    }
    let (_, v) = srv.get(&path, &ann.token).await;
    assert_eq!(v["data"]["execution"], "running");

    assert_eq!(poll_once(&srv.state).await, 2);
    let (_, v) = srv.get(&path, &ann.token).await;
    assert_eq!(v["data"]["execution"], "succeeded", "{v}");
    assert_eq!(v["data"]["result"]["answer"], "captured");
    assert!(v["data"]["revision"].as_i64().unwrap() >= 3);
    // Nothing more to do: dispatch and poll are idempotent.
    assert_eq!(dispatch_once(&srv.state).await, 0);
    assert_eq!(poll_once(&srv.state).await, 0);
}

/// JOB-02: the Idempotency-Key is required, replays are exact, a reused key with another request
/// is a conflict.
#[tokio::test]
async fn idempotency_keys_are_required_and_bind_the_request() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    attested(&srv, &owner).await;
    let ann = srv.person("mpann2").await;
    let body = job_body(&srv, &ann.token, "ann-auth-0000000002", "Sick leave?").await;
    let (s, _, v) = send(
        &srv,
        Method::POST,
        "/v1/nest-jobs",
        Some(&ann.token),
        &[],
        Some(body.to_string()),
    )
    .await;
    assert_eq!((s, code(&v)), (400, "invalid_request"), "{v}");
    let (s, _, v) = submit(&srv, &ann.token, "short", &body).await;
    assert_eq!((s, code(&v)), (400, "invalid_request"), "{v}");
    let (s, _, first) = submit(&srv, &ann.token, "ann-job-00000000002", &body).await;
    assert_eq!(s, 202, "{first}");
    // The same request again: the recorded answer, marked as a replay, and still one job.
    let (s, h, again) = submit(&srv, &ann.token, "ann-job-00000000002", &body).await;
    assert_eq!((s, &again), (202, &first));
    assert_eq!(h.get("idempotent-replayed").unwrap(), "true");
    // The same key with a different request.
    let mut other = body.clone();
    other["payload"]["question"] = json!("Expenses?");
    let (s, _, v) = submit(&srv, &ann.token, "ann-job-00000000002", &other).await;
    assert_eq!((s, code(&v)), (409, "idempotency_conflict"), "{v}");
    let n: i64 = srv
        .state
        .store
        .group_read(|c| -> Result<i64, crate::groups::GErr> {
            Ok(c.query_row("SELECT COUNT(*) FROM nest_jobs", [], |r| r.get(0))?)
        })
        .await
        .unwrap();
    assert_eq!(n, 1);
}

/// JOB-03: authorizations are zero-priced, single-use and bound to the caller and the quote.
#[tokio::test]
async fn payment_authorizations_are_single_use_and_bound() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    attested(&srv, &owner).await;
    let (ann, bob) = (srv.person("mpann3").await, srv.person("mpbob3").await);

    let (_, q) = quote(&srv, &ann.token).await;
    let qid = q["data"]["quote_id"].as_str().unwrap().to_string();
    // No mandates, no other account's quote, and a key is required.
    let (s, _, v) = send(
        &srv,
        Method::POST,
        "/v1/nest-payment-authorizations",
        Some(&ann.token),
        &[("idempotency-key", "ann-auth-mandate-001")],
        Some(
            json!({ "quote_id": qid, "mandate_id": "mnd_aaaaaaaaaaaaaaaaaaaaaaaaaa" }).to_string(),
        ),
    )
    .await;
    assert_eq!((s, code(&v)), (400, "invalid_request"), "{v}");
    let (s, v) = authorization(&srv, &bob.token, &qid, "bob-auth-0000000003").await;
    assert_eq!((s, code(&v)), (404, "not_found"), "{v}");
    let (s, _, v) = send(
        &srv,
        Method::POST,
        "/v1/nest-payment-authorizations",
        Some(&ann.token),
        &[],
        Some(json!({ "quote_id": qid }).to_string()),
    )
    .await;
    assert_eq!((s, code(&v)), (400, "invalid_request"), "{v}");

    let (s, h, a) = send(
        &srv,
        Method::POST,
        "/v1/nest-payment-authorizations",
        Some(&ann.token),
        &[("idempotency-key", "ann-auth-0000000003")],
        Some(json!({ "quote_id": qid }).to_string()),
    )
    .await;
    assert_eq!(s, 201, "{a}");
    assert_eq!(h.get("cache-control").unwrap(), "no-store");
    assert_eq!(
        (
            a["data"]["amount_minor"].as_i64(),
            a["data"]["reservation_id"].is_null()
        ),
        (Some(0), true)
    );
    let first_ref = a["data"]["payment_authorization"]
        .as_str()
        .unwrap()
        .to_string();
    // Only the hash is stored.
    let found: i64 = srv
        .state
        .store
        .group_read(move |c| -> Result<i64, crate::groups::GErr> {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM nest_payment_authorizations WHERE ref_sha256 = ?1 OR ref_sha256 = ?2",
                rusqlite::params![first_ref, first_ref],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(found, 0, "the reference itself is never stored");
    // A retry with the same key rotates the reference: the old one stops working.
    let (s, h, again) = send(
        &srv,
        Method::POST,
        "/v1/nest-payment-authorizations",
        Some(&ann.token),
        &[("idempotency-key", "ann-auth-0000000003")],
        Some(json!({ "quote_id": qid }).to_string()),
    )
    .await;
    assert_eq!(s, 201, "{again}");
    assert_eq!(h.get("idempotent-replayed").unwrap(), "true");
    assert_ne!(
        again["data"]["payment_authorization"],
        a["data"]["payment_authorization"]
    );
    // A second authorization for the same quote with another key is refused.
    let (s, v) = authorization(&srv, &ann.token, &qid, "ann-auth-0000000004").await;
    assert_eq!((s, code(&v)), (409, "authorization_reused"), "{v}");

    let mut body = json!({
        "type": "nest.job.request", "version": 1, "to": ADDRESS, "operation": OP,
        "operation_version": "1.0", "quote_id": qid,
        "payment_authorization": a["data"]["payment_authorization"],
        "payload": { "question": "Equipment?" }, "deadline_at": deadline(),
    });
    let (s, _, v) = submit(&srv, &ann.token, "ann-job-00000000003", &body).await;
    assert_eq!(
        (s, code(&v)),
        (409, "authorization_intent_mismatch"),
        "the rotated-out reference: {v}"
    );
    body["payment_authorization"] = again["data"]["payment_authorization"].clone();
    // Bob cannot use Ann's quote and authorization.
    let (s, _, v) = submit(&srv, &bob.token, "bob-job-00000000003", &body).await;
    assert_eq!((s, code(&v)), (404, "not_found"), "{v}");
    let (s, _, v) = submit(&srv, &ann.token, "ann-job-00000000003", &body).await;
    assert_eq!(s, 202, "{v}");
    // Used once: a new job with the same authorization is refused.
    let (s, _, v) = submit(&srv, &ann.token, "ann-job-00000000004", &body).await;
    assert_eq!((s, code(&v)), (409, "authorization_reused"), "{v}");
    // An expired quote can neither be authorized nor used.
    let (_, q2) = quote(&srv, &ann.token).await;
    let q2 = q2["data"]["quote_id"].as_str().unwrap().to_string();
    let q2c = q2.clone();
    srv.state
        .store
        .group_tx(move |tx| -> Result<(), crate::groups::GErr> {
            tx.execute(
                "UPDATE nest_quotes SET expires_at = 1 WHERE id = ?1",
                [&q2c],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let (s, v) = authorization(&srv, &ann.token, &q2, "ann-auth-0000000005").await;
    assert_eq!((s, code(&v)), (409, "quote_expired"), "{v}");
}

/// JOB-04: the request is strict. Server-owned fields, duplicates, depth, size, payload, deadline.
#[tokio::test]
async fn job_requests_are_strict() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    attested(&srv, &owner).await;
    let ann = srv.person("mpann4").await;
    let body = job_body(&srv, &ann.token, "ann-auth-0000000006", "Leave?").await;
    let mut n = 0;
    let mut check = |b: String, status: u16, want: &'static str| {
        n += 1;
        let key = format!("ann-strict-{n:010}");
        let srv = &srv;
        let token = ann.token.clone();
        async move {
            let (s, _, v) = send(
                srv,
                Method::POST,
                "/v1/nest-jobs",
                Some(&token),
                &[("idempotency-key", &key)],
                Some(b.clone()),
            )
            .await;
            assert_eq!((s, code(&v)), (status, want), "{b}: {v}");
        }
    };
    for field in ["payer", "amount_minor", "from", "customer_id", "approval"] {
        let mut b = body.clone();
        b[field] = json!("x");
        check(b.to_string(), 400, "invalid_request").await;
    }
    let dup = body.to_string().replacen('{', "{\"version\":1,", 1);
    check(dup, 400, "invalid_json").await;
    let mut deep = body.clone();
    deep["payload"]["question"] =
        serde_json::from_str(&format!("{}1{}", "[".repeat(40), "]".repeat(40))).unwrap();
    check(deep.to_string(), 413, "payload_too_large").await;
    let mut big = body.clone();
    big["payload"]["question"] = json!("x".repeat(300 * 1024));
    check(big.to_string(), 413, "payload_too_large").await;
    let mut b = body.clone();
    b["payload"] = json!({ "question": "x".repeat(2001) });
    check(b.to_string(), 400, "invalid_operation_payload").await;
    let mut b = body.clone();
    b["payload"] = json!({ "question": "ok", "admin": true });
    check(b.to_string(), 400, "invalid_operation_payload").await;
    let mut b = body.clone();
    b["deadline_at"] = json!("2020-01-01T00:00:00Z");
    check(b.to_string(), 400, "deadline_invalid").await;
    let mut b = body.clone();
    b["attachments"] = json!(["att_1"]);
    check(b.to_string(), 400, "invalid_request").await;
    let mut b = body.clone();
    b["type"] = json!("pp.nest.job.request");
    check(b.to_string(), 400, "invalid_request").await;
    let mut b = body.clone();
    b["operation"] = json!("hr.policy.delete");
    check(b.to_string(), 404, "not_found").await;
    let mut b = body.clone();
    b["reply_to"] = json!("/k/someone-else");
    check(b.to_string(), 400, "invalid_request").await;
    // None of that consumed the authorization: the valid request still goes through.
    check(body.to_string(), 202, "").await;
}

/// JOB-05: who may submit and read. Agents carry jobs with their own token; a job is its
/// submitter's alone.
#[tokio::test]
async fn jobs_are_for_their_submitter_and_agents_may_carry_them() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    attested(&srv, &owner).await;
    let (ann, bob) = (srv.person("mpann5").await, srv.person("mpbob5").await);
    // A mailbox capability token (an agent's own credential) quotes, authorizes and submits.
    let (s, v) = srv
        .post(
            "/v1/identities",
            &ann.token,
            json!({ "label": "ann-agent" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let cap = v["capability_token"].as_str().unwrap().to_string();
    let body = job_body(&srv, &cap, "ann-cap-auth-000001", "Expenses for meals?").await;
    let (s, _, v) = submit(&srv, &cap, "ann-cap-job-0000001", &body).await;
    assert_eq!(s, 202, "{v}");
    let job = v["data"]["job_id"].as_str().unwrap().to_string();
    let path = format!("/v1/nest-jobs/{job}");
    // The account reads it through any of its credentials; another account cannot tell it exists.
    assert_eq!(srv.get(&path, &ann.token).await.0, 200);
    assert_eq!(srv.get(&path, &cap).await.0, 200);
    let (s, stranger) = srv.get(&path, &bob.token).await;
    let (s2, missing) = srv
        .get("/v1/nest-jobs/job_aaaaaaaaaaaaaaaaaaaaaaaaaa", &bob.token)
        .await;
    assert_eq!((s, s2), (404, 404));
    assert_eq!(stranger, missing);
    let (s, _) = srv.call(Method::GET, &path, None, None).await;
    assert_eq!(s, 401);
    // The capability token acts as its own mailbox only.
    let body2 = job_body(&srv, &cap, "ann-cap-auth-000002", "Leave?").await;
    let (s, _, v) = send(
        &srv,
        Method::POST,
        "/v1/nest-jobs",
        Some(&cap),
        &[
            ("idempotency-key", "ann-cap-job-0000002"),
            ("x-pigeonpost-identity", "/k/not-mine"),
        ],
        Some(body2.to_string()),
    )
    .await;
    assert_eq!((s, code(&v)), (403, "forbidden"), "{v}");
}

/// JOB-06: only a published listing whose release is attested and live admits jobs.
#[tokio::test]
async fn only_attested_live_releases_take_jobs() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    assign_reviewer(&srv, &owner, &owner.account).await;
    let ann = srv.person("mpann6").await;
    let r = register(&srv, "1.0.0", 'a').await;
    pass_analysis(&srv, &r.review).await;
    // Built and analysed, not attested: the draft listing is invisible.
    let (s, v) = quote(&srv, &ann.token).await;
    assert_eq!((s, code(&v)), (404, "not_found"), "{v}");
    // Published but with a tampered attestation: refused.
    let (s, v) = decide(&srv, &owner.strong, &r, "approved").await;
    assert_eq!(s, 201, "{v}");
    let (s, _) = quote(&srv, &ann.token).await;
    assert_eq!(s, 201);
    let rel = r.release.clone();
    srv.state
        .store
        .group_tx(move |tx| -> Result<(), crate::groups::GErr> {
            // Attestations are append-only: a forged later one is the only way to "change" it.
            tx.execute(
                "INSERT INTO release_attestations (id, release_id, case_id, attester, statement_digest, created_at, statement)
                 SELECT 'att_forged', release_id, case_id, attester, 'sha256:forged', created_at + 1,
                        statement
                   FROM release_attestations WHERE release_id = ?1",
                [&rel],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let (s, v) = quote(&srv, &ann.token).await;
    assert_eq!((s, code(&v)), (409, "review_required"), "{v}");
}

/// SEC-05: quarantine stops running jobs and refuses new ones, with an event; it can be lifted.
/// Revocation is permanent.
#[tokio::test]
async fn quarantine_and_revocation() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    let r = attested(&srv, &owner).await;
    let ann = srv.person("mpann7").await;
    let body = job_body(&srv, &ann.token, "ann-auth-0000000007", "Leave?").await;
    let (_, _, v) = submit(&srv, &ann.token, "ann-job-00000000007", &body).await;
    let job = v["data"]["job_id"].as_str().unwrap().to_string();
    dispatch_once(&srv.state).await;
    let security = format!("/v1/staff/releases/{}/security", r.release);

    // Not staff, and staff without step-up.
    let (s, v) = srv
        .post(
            &security,
            &ann.strong,
            json!({ "action": "quarantine", "reason": "x" }),
        )
        .await;
    assert_eq!((s, code(&v)), (403, "staff_required"), "{v}");
    let (s, v) = srv
        .post(
            &security,
            &owner.token,
            json!({ "action": "quarantine", "reason": "x" }),
        )
        .await;
    assert_eq!((s, code(&v)), (401, "step_up_required"), "{v}");

    let (s, v) = srv
        .post(
            &security,
            &owner.strong,
            json!({ "action": "quarantine", "reason": "suspicious traffic" }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (
            v["data"]["security_state"].as_str(),
            v["data"]["jobs_stopped"].as_i64()
        ),
        (Some("quarantined"), Some(1))
    );
    assert_eq!(v["data"]["runtime_updated"], true);
    let call = srv
        .state
        .nests
        .calls()
        .into_iter()
        .find(|c| c.path == format!("/v1/mp/releases/{}/quarantine", r.release))
        .expect("the worker host was told");
    assert_eq!(call.body.unwrap()["revoke"], false);
    let (_, v) = srv.get(&format!("/v1/nest-jobs/{job}"), &ann.token).await;
    assert_eq!(v["data"]["execution"], "stopped_security", "{v}");
    // New work is refused at every step.
    let (s, v) = quote(&srv, &ann.token).await;
    assert_eq!((s, code(&v)), (409, "security_quarantined"), "{v}");

    // Lifted: jobs are taken again.
    let (s, v) = srv
        .post(
            &security,
            &owner.strong,
            json!({ "action": "lift", "reason": "cleared" }),
        )
        .await;
    assert_eq!(
        (s, v["data"]["security_state"].as_str()),
        (200, Some("normal")),
        "{v}"
    );
    let (s, _) = quote(&srv, &ann.token).await;
    assert_eq!(s, 201);

    // Revoked: permanent.
    let (s, v) = srv
        .post(
            &security,
            &owner.strong,
            json!({ "action": "revoke", "reason": "malicious" }),
        )
        .await;
    assert_eq!(
        (s, v["data"]["security_state"].as_str()),
        (200, Some("revoked")),
        "{v}"
    );
    let revoke_call = srv
        .state
        .nests
        .calls()
        .into_iter()
        .rfind(|c| c.path == format!("/v1/mp/releases/{}/quarantine", r.release))
        .unwrap();
    assert_eq!(revoke_call.body.unwrap()["revoke"], true);
    for action in ["lift", "quarantine", "revoke"] {
        let (s, v) = srv
            .post(
                &security,
                &owner.strong,
                json!({ "action": action, "reason": "again" }),
            )
            .await;
        assert_eq!((s, code(&v)), (409, "release_revoked"), "{action}: {v}");
    }
    let (s, v) = quote(&srv, &ann.token).await;
    assert_eq!(s, 404, "the listing is suspended with its release: {v}");
    let rel = r.release.clone();
    let events: Vec<String> = srv
        .state
        .store
        .group_read(move |c| -> Result<Vec<String>, crate::groups::GErr> {
            let mut stmt = c.prepare(
                "SELECT action FROM release_security_events WHERE release_id = ?1 ORDER BY created_at, rowid",
            )?;
            let rows = stmt
                .query_map([&rel], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok(rows)
        })
        .await
        .unwrap();
    assert_eq!(events, ["quarantined", "released", "revoked"]);
}
