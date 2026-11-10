//! Marketplace hardening (W1) over real sockets: signed attestations, typed events, the kill
//! switch, publisher quotas and rate limits, source builds on the remote build plane, worker
//! tombstones. Synthetic accounts only; the worker host and the build plane are recorded.

use crate::marketplace_hardening::Hardening;
use crate::marketplace_m1::{dispatch_once, poll_once};
use crate::marketplace_m1_tests::{
    assign_reviewer, attested, build_record, code, decide, evidence, job_body, pass_analysis,
    quote, register, setup, submit, Release, OWNER_ACCOUNT,
};
use crate::org_tests::{serve_with, Person, Srv};
use crate::*;
use serde_json::{json, Value};

/// Typed events of `account`, oldest first: `(type, data)`.
async fn events_of(srv: &Srv, account: &str) -> Vec<(String, Value)> {
    let a = account.to_string();
    srv.state
        .store
        .group_read(
            move |c| -> Result<Vec<(String, Value)>, crate::groups::GErr> {
                let mut stmt = c.prepare(
                    "SELECT type, data FROM account_events WHERE account_id = ?1 ORDER BY id",
                )?;
                let rows = stmt
                    .query_map([&a], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            serde_json::from_str::<Value>(&r.get::<_, String>(1)?)
                                .unwrap_or_default(),
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            },
        )
        .await
        .unwrap()
}

fn types(events: &[(String, Value)]) -> Vec<&str> {
    events.iter().map(|(t, _)| t.as_str()).collect()
}

async fn sql(srv: &Srv, statement: String) {
    srv.state
        .store
        .group_tx(move |tx| -> Result<(), crate::groups::GErr> {
            tx.execute_batch(&statement)?;
            Ok(())
        })
        .await
        .unwrap();
}

async fn scalar(srv: &Srv, query: &str) -> Value {
    let q = query.to_string();
    srv.state
        .store
        .group_read(move |c| -> Result<Value, crate::groups::GErr> {
            Ok(c.query_row(&q, [], |r| {
                Ok(match r.get_ref(0)? {
                    rusqlite::types::ValueRef::Integer(i) => json!(i),
                    rusqlite::types::ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                    _ => Value::Null,
                })
            })?)
        })
        .await
        .unwrap()
}

// ---- W1.1 signed attestations ---------------------------------------------------------------------

/// An approval is signed with the postbox key, the statement names that key, the signature is
/// stored beside the attestation, and every dispatch hands nestd the statement, the signature and
/// the key id.
#[tokio::test]
async fn approvals_are_signed_and_every_dispatch_carries_the_signature() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    assign_reviewer(&srv, &owner, &owner.account).await;
    let r = register(&srv, "1.0.0", 'a').await;
    pass_analysis(&srv, &r.review).await;
    let (s, v) = decide(&srv, &owner.strong, &r, "approved").await;
    assert_eq!(s, 201, "{v}");
    let att = &v["data"]["attestation"];
    let key_id = att["key_id"].as_str().unwrap().to_string();
    assert_eq!(key_id, "ppk_fe812c12f3ab4ce6", "the test key");
    assert_eq!(att["signature"].as_str().unwrap().len(), 128);
    let stored = scalar(&srv, "SELECT COUNT(*) FROM release_attestation_signatures").await;
    assert_eq!(stored, 1);
    // Append-only, like the attestation it signs.
    let changed = srv
        .state
        .store
        .group_tx(|tx| -> Result<(), crate::groups::GErr> {
            tx.execute(
                "UPDATE release_attestation_signatures SET signature = 'x'",
                [],
            )?;
            Ok(())
        })
        .await;
    assert!(changed.is_err());

    let ann = srv.person("hdann1").await;
    let body = job_body(&srv, &ann.token, "hd-auth-000000001", "Annual leave?").await;
    let (s, _, v) = submit(&srv, &ann.token, "hd-job-0000000001", &body).await;
    assert_eq!(s, 202, "{v}");
    assert_eq!(dispatch_once(&srv.state).await, 1);
    let call = srv
        .state
        .nests
        .calls()
        .into_iter()
        .find(|c| c.path == "/v1/mp/jobs")
        .unwrap();
    let rel = &call.body.as_ref().unwrap()["release"];
    let statement = rel["statement"].as_str().unwrap();
    let s: Value = serde_json::from_str(statement).unwrap();
    assert_eq!(s["key_id"], key_id.as_str());
    assert_eq!(rel["key_id"], key_id.as_str());
    assert!(Hardening::for_test().verify(statement, &key_id, rel["signature"].as_str().unwrap()));
    // First party: no publisher limits travel with the job.
    assert!(call.body.as_ref().unwrap()["limits"].is_null());
}

/// Without an attestation key nothing can be approved; a newer attestation written without the
/// key (someone with database access) makes the release unadmittable rather than trusted.
#[tokio::test]
async fn no_key_no_approval_and_an_unsigned_attestation_is_not_trusted() {
    let srv = serve_with(|s| {
        s.platform_owner = Some(Arc::from(OWNER_ACCOUNT));
        let mut rt = nests::Runtime::capture();
        rt.marketplace = Hardening::default();
        s.nests = Arc::new(rt);
    })
    .await;
    let owner = srv.person("mpowner").await;
    srv.global_flag("marketplace").await;
    assign_reviewer(&srv, &owner, &owner.account).await;
    let r = register(&srv, "1.0.0", 'a').await;
    pass_analysis(&srv, &r.review).await;
    let (s, v) = decide(&srv, &owner.strong, &r, "approved").await;
    assert_eq!((s, code(&v)), (503, "attestation_key_missing"), "{v}");
    // Rejections need no key.
    let (s, v) = decide(&srv, &owner.strong, &r, "rejected").await;
    assert_eq!(s, 201, "{v}");

    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    let r = attested(&srv, &owner).await;
    let ann = srv.person("hdann2").await;
    let (s, v) = quote(&srv, &ann.token).await;
    assert_eq!(s, 201, "{v}");
    // A forged, newer attestation: a statement whose digest matches, but no signature.
    let statement = format!(
        "{{\"decision\":\"approved\",\"image\":\"x\",\"release_digest\":\"{}\",\"release_id\":\"{}\",\"type\":\"pigeonpost.nest.release.attestation.v1\"}}",
        r.digest, r.release
    );
    let digest = crate::marketplace_m1::digest_of(statement.as_bytes());
    let case = r.review.clone();
    sql(
        &srv,
        format!(
            "INSERT INTO release_attestations (id, release_id, case_id, attester, statement_digest,
                created_at, release_digest, statement, first_party)
             VALUES ('att_forged', '{}', '{case}', 'acct_mallory', '{digest}', 9999999999, '{}',
                '{}', 1)",
            r.release,
            r.digest,
            statement.replace('\'', "''")
        ),
    )
    .await;
    let (s, v) = quote(&srv, &ann.token).await;
    assert_eq!((s, code(&v)), (409, "review_required"), "{v}");
}

// ---- W1.7 events ------------------------------------------------------------------------------------

/// Review, job and security changes are typed events on the right accounts' streams, with ids
/// and states only: never the question, the answer or a reason.
#[tokio::test]
async fn review_job_and_security_changes_are_typed_events_without_content() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    // A blocked review first.
    let blocked = register(&srv, "0.9.0", 'c').await;
    let ev = evidence("deterministic_scan", "failed", None);
    let (s, v) = srv
        .call(
            reqwest::Method::POST,
            &format!("/internal/v1/marketplace/reviews/{}/stages", blocked.review),
            Some(crate::org_tests::OPS),
            Some(json!({
                "stage": "deterministic_scan",
                "decision": "failed",
                "reason": "secret",
                "evidence_digest": crate::marketplace_m1::digest_of(ev.as_bytes()),
                "evidence": ev,
            })),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let r = attested(&srv, &owner).await;
    let ann = srv.person("hdann3").await;
    let body = job_body(
        &srv,
        &ann.token,
        "hd-auth-000000003",
        "SECRET-QUESTION-TEXT",
    )
    .await;
    let (s, _, v) = submit(&srv, &ann.token, "hd-job-0000000003", &body).await;
    assert_eq!(s, 202, "{v}");
    dispatch_once(&srv.state).await;
    poll_once(&srv.state).await;
    let (s, v) = srv
        .post(
            &format!("/v1/staff/releases/{}/security", r.release),
            &owner.strong,
            json!({ "action": "quarantine", "reason": "SECRET-REASON-TEXT" }),
        )
        .await;
    assert_eq!(s, 200, "{v}");

    // The platform publisher's events go to the platform owner.
    let mine = events_of(&srv, OWNER_ACCOUNT).await;
    let t = types(&mine);
    for want in [
        "nest.review.stage_changed",
        "nest.review.blocked",
        "nest.review.decision",
        "nest.review.completed",
        "nest.security.quarantined",
    ] {
        assert!(t.contains(&want), "{want} missing from {t:?}");
    }
    let buyer = events_of(&srv, &ann.account).await;
    let t = types(&buyer);
    for want in [
        "nest.job.admitted",
        "nest.job.progress",
        "nest.job.result_ready",
        "nest.security.quarantined",
    ] {
        assert!(t.contains(&want), "{want} missing from {t:?}");
    }
    assert!(!t.iter().any(|x| x.starts_with("nest.review.")), "{t:?}");
    for (_, data) in mine.iter().chain(buyer.iter()) {
        let text = data.to_string();
        for secret in ["SECRET-QUESTION-TEXT", "SECRET-REASON-TEXT", "captured"] {
            assert!(!text.contains(secret), "{text}");
        }
    }
    let admitted = buyer
        .iter()
        .find(|(t, _)| t == "nest.job.admitted")
        .unwrap();
    assert!(admitted.1["job_id"].as_str().unwrap().starts_with("job_"));
    assert_eq!(admitted.1["listing_id"], r.listing.as_str());
}

// ---- W1.8 abuse controls ---------------------------------------------------------------------------

async fn platform_publisher(srv: &Srv) -> String {
    scalar(
        srv,
        "SELECT id FROM publisher_profiles WHERE owner_kind = 'platform'",
    )
    .await
    .as_str()
    .unwrap()
    .to_string()
}

#[tokio::test]
async fn buyers_are_rate_limited_per_minute_by_the_publishers_quota() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    attested(&srv, &owner).await;
    let publisher = platform_publisher(&srv).await;
    let quotas = format!("/v1/staff/marketplace/publishers/{publisher}/quotas");
    let q = json!({
        "builds_per_day": 1, "max_workers": 2, "monthly_broker_tokens": 1000,
        "jobs_per_minute": 1, "reason": "load test"
    });
    // Staff with step-up only.
    let ann = srv.person("hdann4").await;
    let put = |token: String, body: Value| {
        let srv = &srv;
        let quotas = quotas.clone();
        async move {
            let r = srv
                .http
                .put(format!("{}{quotas}", srv.public))
                .header("x-forwarded-for", &srv.ip)
                .bearer_auth(token)
                .json(&body)
                .send()
                .await
                .unwrap();
            (
                r.status().as_u16(),
                r.json::<Value>().await.unwrap_or_default(),
            )
        }
    };
    let (s, _) = put(ann.strong.clone(), q.clone()).await;
    assert_eq!(s, 403);
    let (s, v) = put(owner.token.clone(), q.clone()).await;
    assert_eq!((s, code(&v)), (401, "step_up_required"), "{v}");
    let (s, v) = put(owner.strong.clone(), q.clone()).await;
    assert_eq!(s, 200, "{v}");
    let audited = scalar(
        &srv,
        "SELECT COUNT(*) FROM audit_log WHERE action = 'marketplace.publisher.quotas'",
    )
    .await;
    assert_eq!(audited, 1);

    let body = job_body(&srv, &ann.token, "hd-auth-000000004", "One?").await;
    let (s, _, v) = submit(&srv, &ann.token, "hd-job-0000000004", &body).await;
    assert_eq!(s, 202, "{v}");
    let body = job_body(&srv, &ann.token, "hd-auth-000000005", "Two?").await;
    let (s, h, v) = submit(&srv, &ann.token, "hd-job-0000000005", &body).await;
    assert_eq!((s, code(&v)), (429, "rate_limited"), "{v}");
    assert!(h.get("retry-after").is_some());
    // Another buyer has their own minute.
    let bob = srv.person("hdbob4").await;
    let body = job_body(&srv, &bob.token, "hd-auth-000000006", "Three?").await;
    let (s, _, v) = submit(&srv, &bob.token, "hd-job-0000000006", &body).await;
    assert_eq!(s, 202, "{v}");
}

async fn kill(srv: &Srv, who: &Person, path: &str, action: &str) -> (u16, Value) {
    srv.post(
        path,
        &who.strong,
        json!({ "action": action, "reason": "abuse report" }),
    )
    .await
}

/// The kill switch per listing and per publisher: jobs stop at once, nestd is told to stop the
/// listing's workers (and told again until it answers), new jobs are refused, everything is
/// audited, and a restore undoes it.
#[tokio::test]
async fn the_kill_switch_stops_a_listing_or_a_publisher_until_restored() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    let r: Release = attested(&srv, &owner).await;
    let ann = srv.person("hdann5").await;
    let body = job_body(&srv, &ann.token, "hd-auth-000000007", "Leave?").await;
    let (s, _, v) = submit(&srv, &ann.token, "hd-job-0000000007", &body).await;
    assert_eq!(s, 202, "{v}");
    let job = v["data"]["job_id"].as_str().unwrap().to_string();

    let path = format!("/v1/staff/marketplace/listings/{}/kill", r.listing);
    // Not staff, and staff without step-up, are refused.
    let (s, _) = kill(&srv, &ann, &path, "kill").await;
    assert_eq!(s, 403);
    let (s, v) = srv
        .post(
            &path,
            &owner.token,
            json!({ "action": "kill", "reason": "x" }),
        )
        .await;
    assert_eq!((s, code(&v)), (401, "step_up_required"), "{v}");
    let (s, v) = kill(&srv, &owner, &path, "kill").await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["jobs_stopped"], 1);
    assert_eq!(v["data"]["runtime_updated"], true);
    let stop = srv
        .state
        .nests
        .calls()
        .into_iter()
        .rfind(|c| c.path == format!("/v1/mp/listings/{}/stop", r.listing))
        .expect("nestd told");
    assert_eq!(stop.body, Some(json!({ "stop": true })));
    let (_, v) = srv.get(&format!("/v1/nest-jobs/{job}"), &ann.token).await;
    assert_eq!(v["data"]["execution"], "stopped_security", "{v}");
    let (s, v) = quote(&srv, &ann.token).await;
    assert_eq!((s, code(&v)), (409, "listing_suspended"), "{v}");
    assert_eq!(
        scalar(
            &srv,
            "SELECT COUNT(*) FROM audit_log WHERE action = 'marketplace.listing.kill'"
        )
        .await,
        1
    );
    // A worker host that did not answer is told again on the next tick.
    srv.state.nests.set_fail(true);
    let (s, v) = kill(&srv, &owner, &path, "restore").await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["runtime_updated"], false);
    srv.state.nests.set_fail(false);
    assert_eq!(
        crate::marketplace_hardening::reconcile_runtime_stops(&srv.state).await,
        1
    );
    let (s, v) = quote(&srv, &ann.token).await;
    assert_eq!(s, 201, "{v}");

    // A publisher kill covers every listing of the publisher.
    let publisher = platform_publisher(&srv).await;
    let ppath = format!("/v1/staff/marketplace/publishers/{publisher}/kill");
    let (s, v) = kill(&srv, &owner, &ppath, "kill").await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["listings"], json!([r.listing]));
    let (s, v) = quote(&srv, &ann.token).await;
    assert_eq!((s, code(&v)), (409, "listing_suspended"), "{v}");
    // Jobs already queued for it are not dispatched.
    let (s, v) = kill(&srv, &owner, &ppath, "restore").await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = quote(&srv, &ann.token).await;
    assert_eq!(s, 201, "{v}");
    // An unknown target is a 404 after the staff check.
    let (s, _) = kill(
        &srv,
        &owner,
        "/v1/staff/marketplace/listings/lst_missing/kill",
        "kill",
    )
    .await;
    assert_eq!(s, 404);
}

// ---- W1.3/W1.5 source builds on the remote build plane ---------------------------------------------

async fn third_party_publisher(srv: &Srv, owner_of: &Person) -> String {
    let (s, v) = srv
        .post(
            "/v1/publisher-applications",
            &owner_of.token,
            json!({ "display_name": "Acme Apps" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let p = v["data"]["id"].as_str().unwrap().to_string();
    sql(
        srv,
        format!("UPDATE publisher_profiles SET status = 'approved' WHERE id = '{p}'"),
    )
    .await;
    p
}

fn stage_body(stage: &str, decision: &str, model: Option<&str>) -> Value {
    let ev = evidence(stage, decision, model);
    json!({
        "stage": stage,
        "decision": decision,
        "reason": "test",
        "evidence_digest": crate::marketplace_m1::digest_of(ev.as_bytes()),
        "evidence": ev,
    })
}

/// A third-party release from git: staff (or M2's publisher route) asks for a source build, the
/// postbox submits it to the build plane, pulls the finished record with its scan evidence,
/// registers it for review under the publisher, and once approved its jobs carry the
/// publisher's limits to nestd. Builds per day are capped.
#[tokio::test]
async fn a_source_build_becomes_a_third_party_release_under_review() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    assign_reviewer(&srv, &owner, &owner.account).await;
    let acme = srv.person("hdacme").await;
    let publisher = third_party_publisher(&srv, &acme).await;
    let commit = "0123456789abcdef0123456789abcdef01234567";
    let req = json!({
        "publisher_id": publisher,
        "repo_url": "https://git.example.org/acme/hr-policy-qa.git",
        "commit": commit,
    });
    // Staff only, with step-up.
    let (s, _) = srv
        .post(
            "/v1/staff/marketplace/source-builds",
            &acme.strong,
            req.clone(),
        )
        .await;
    assert_eq!(s, 403);
    let (s, v) = srv
        .post(
            "/v1/staff/marketplace/source-builds",
            &owner.strong,
            json!({
                "publisher_id": publisher, "repo_url": "http://git.example.org/x", "commit": commit
            }),
        )
        .await;
    assert_eq!((s, code(&v)), (400, "invalid_request"), "{v}");
    let h = &srv.state.nests.marketplace;
    h.buildd_answer(202, json!({ "state": "queued" }));
    let (s, v) = srv
        .post(
            "/v1/staff/marketplace/source-builds",
            &owner.strong,
            req.clone(),
        )
        .await;
    assert_eq!(s, 202, "{v}");
    assert_eq!(v["data"]["state"], "submitted");
    let build = v["data"]["build_id"].as_str().unwrap().to_string();
    let sent = h.buildd_calls();
    assert_eq!(sent[0].path, "/v1/builds");
    assert_eq!(sent[0].body.as_ref().unwrap()["commit"], commit);
    assert_eq!(sent[0].body.as_ref().unwrap()["build_id"], build.as_str());

    // The build plane finishes: record, the scan stage, a reproducible build.
    h.buildd_answer(
        200,
        json!({
            "state": "succeeded",
            "record": build_record("2.0.0", 'e'),
            "stages": [stage_body("deterministic_scan", "passed", None)],
            "reproducibility": { "builds": 2, "equal": true },
        }),
    );
    assert_eq!(
        crate::marketplace_hardening::source_builds_once(&srv.state).await,
        1
    );
    let (s, v) = srv
        .get(
            &format!("/v1/staff/marketplace/source-builds/{build}"),
            &owner.strong,
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["state"], "registered", "{v}");
    let release = v["data"]["release_id"].as_str().unwrap().to_string();
    let review = v["data"]["review_id"].as_str().unwrap().to_string();
    let owner_of = scalar(
        &srv,
        &format!("SELECT publisher_id FROM nest_releases WHERE id = '{release}'"),
    )
    .await;
    assert_eq!(
        owner_of,
        publisher.as_str(),
        "the publisher's release, not Pigeonpost's"
    );
    // The publisher's account sees its review move.
    let t = events_of(&srv, &acme.account).await;
    assert!(types(&t).contains(&"nest.review.stage_changed"), "{t:?}");

    // Review it to approval; then a job carries the publisher's limits.
    let (s, v) = srv
        .call(
            reqwest::Method::POST,
            &format!("/internal/v1/marketplace/reviews/{review}/stages"),
            Some(crate::org_tests::OPS),
            Some(stage_body("behavioral_test", "passed", None)),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let (s, v) = srv
        .call(
            reqwest::Method::POST,
            &format!("/internal/v1/marketplace/reviews/{review}/stages"),
            Some(crate::org_tests::OPS),
            Some(stage_body(
                "ai_analysis",
                "advisory",
                Some("qwen-latest-flash-next"),
            )),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let rec = build_record("2.0.0", 'e');
    let r = Release {
        release: release.clone(),
        review: review.clone(),
        listing: String::new(),
        digest: rec["release_digest"].as_str().unwrap().into(),
    };
    let (s, v) = decide(&srv, &owner.strong, &r, "approved").await;
    assert_eq!(s, 201, "{v}");
    let ann = srv.person("hdann6").await;
    let body = job_body(&srv, &ann.token, "hd-auth-000000008", "Leave?").await;
    let (s, _, v) = submit(&srv, &ann.token, "hd-job-0000000008", &body).await;
    assert_eq!(s, 202, "{v}");
    dispatch_once(&srv.state).await;
    let call = srv
        .state
        .nests
        .calls()
        .into_iter()
        .find(|c| c.path == "/v1/mp/jobs")
        .unwrap();
    assert_eq!(
        call.body.as_ref().unwrap()["limits"],
        json!({ "publisher_id": publisher, "max_workers": 20, "monthly_tokens": 2_000_000 })
    );

    // Builds per day: the default is five; a sixth is refused.
    for _ in 0..4 {
        let (s, v) = srv
            .post(
                "/v1/staff/marketplace/source-builds",
                &owner.strong,
                req.clone(),
            )
            .await;
        assert_eq!(s, 202, "{v}");
    }
    let (s, v) = srv
        .post(
            "/v1/staff/marketplace/source-builds",
            &owner.strong,
            req.clone(),
        )
        .await;
    assert_eq!((s, code(&v)), (429, "quota_exceeded"), "{v}");
    // A publisher that is not approved cannot build at all.
    sql(
        &srv,
        format!("UPDATE publisher_profiles SET status = 'revoked' WHERE id = '{publisher}'"),
    )
    .await;
    let (s, v) = srv
        .post("/v1/staff/marketplace/source-builds", &owner.strong, req)
        .await;
    assert_eq!((s, code(&v)), (409, "publisher_not_approved"), "{v}");
}

/// A failed or unreproducible build never becomes a release.
#[tokio::test]
async fn a_failed_or_unreproducible_build_is_not_registered() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    assign_reviewer(&srv, &owner, &owner.account).await;
    let acme = srv.person("hdacme2").await;
    let publisher = third_party_publisher(&srv, &acme).await;
    let req = json!({
        "publisher_id": publisher,
        "repo_url": "https://git.example.org/acme/x.git",
        "commit": "f".repeat(40),
    });
    let h = &srv.state.nests.marketplace;
    let (s, v) = srv
        .post(
            "/v1/staff/marketplace/source-builds",
            &owner.strong,
            req.clone(),
        )
        .await;
    assert_eq!(s, 202, "{v}");
    let first = v["data"]["build_id"].as_str().unwrap().to_string();
    h.buildd_answer(
        200,
        json!({
            "state": "succeeded",
            "record": build_record("3.0.0", 'f'),
            "stages": [],
            "reproducibility": { "builds": 2, "equal": false },
        }),
    );
    crate::marketplace_hardening::source_builds_once(&srv.state).await;
    let (_, v) = srv
        .get(
            &format!("/v1/staff/marketplace/source-builds/{first}"),
            &owner.strong,
        )
        .await;
    assert_eq!(v["data"]["state"], "failed", "{v}");
    assert!(v["data"]["error"]
        .as_str()
        .unwrap()
        .contains("not_reproducible"));
    assert_eq!(
        scalar(&srv, "SELECT COUNT(*) FROM nest_releases").await,
        0,
        "nothing registered"
    );
    let (s, v) = srv
        .post("/v1/staff/marketplace/source-builds", &owner.strong, req)
        .await;
    assert_eq!(s, 202, "{v}");
    let second = v["data"]["build_id"].as_str().unwrap().to_string();
    h.buildd_answer(
        200,
        json!({ "state": "failed", "error": "the fetched tree is not the requested commit" }),
    );
    crate::marketplace_hardening::source_builds_once(&srv.state).await;
    let (_, v) = srv
        .get(
            &format!("/v1/staff/marketplace/source-builds/{second}"),
            &owner.strong,
        )
        .await;
    assert_eq!(v["data"]["state"], "failed");
}

// ---- W1.2 worker tombstones ---------------------------------------------------------------------------

/// A destroyed buyer worker's id is kept and replayed to nestd with the first-party tombstones
/// on unlock, so a restored backup cannot bring the buyer's volume back.
#[tokio::test]
async fn destroyed_workers_are_replayed_as_tombstones_on_unlock() {
    let srv = serve_with(|s| {
        s.platform_owner = Some(Arc::from(OWNER_ACCOUNT));
        let mut rt = nests::Runtime::capture();
        rt.kek = Some(zeroize::Zeroizing::new("ab".repeat(32)));
        s.nests = Arc::new(rt);
    })
    .await;
    let destroyed =
        crate::marketplace_hardening::destroy_worker(&srv.state, "lst_x", "ctx-buyer-a")
            .await
            .unwrap()
            .expect("the capture destroys one");
    nests::unlock_once(&srv.state).await.unwrap();
    let unlock = srv
        .state
        .nests
        .calls()
        .into_iter()
        .find(|c| c.path == "/v1/unlock")
        .unwrap();
    let tombstones = &unlock.body.as_ref().unwrap()["tombstones"];
    assert!(
        tombstones
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t == destroyed.as_str()),
        "{tombstones}"
    );
}
