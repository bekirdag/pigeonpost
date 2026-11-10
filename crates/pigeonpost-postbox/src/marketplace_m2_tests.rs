//! Marketplace M2 over real sockets: publisher certification, org publisher roles and revocation;
//! third-party builds through the M1 review pipeline with ordered, idempotent stages, events and
//! redacted findings; listings and visibility; installations with configuration, secrets, update,
//! rollback and uninstall. Synthetic accounts and synthetic values only.
//!
//! Plan exit IDs touched here: SEC-03 (no publisher code is admitted without staff attestation),
//! SEC-04 (revocation and moderation stop use), SEC-06 (redacted findings), SEC-07 (staff conflict
//! rule for third parties), CFG-01 (configuration schema, server and CLI on the same fixtures).

use crate::marketplace_m1::{canonical_json, digest_of, dispatch_once, release_digest};
use crate::org_tests::{serve_with, Person, Srv, OPS};
use crate::*;
use reqwest::Method;
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub(crate) const OWNER_ACCOUNT: &str = "acct_mpowner";
const FIXTURES: &str =
    include_str!("../../../docs/nests/contracts/examples/nest-config-fixtures.json");

pub(crate) fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or_default()
}

pub(crate) async fn setup() -> (Srv, Person) {
    let srv = serve_with(|s| {
        s.platform_owner = Some(Arc::from(OWNER_ACCOUNT));
        let mut rt = nests::Runtime::capture();
        rt.mp.git_hosts.push("git.example.org".into());
        s.nests = Arc::new(rt);
    })
    .await;
    let owner = srv.person("mpowner").await;
    assert_eq!(owner.account, OWNER_ACCOUNT);
    (srv, owner)
}

/// The platform owner names `reviewer` a staff reviewer.
pub(crate) async fn reviewer(srv: &Srv, owner: &Person, name: &str) -> Person {
    let rev = srv.person(name).await;
    let (s, v) = srv
        .post(
            "/v1/staff/roles",
            &owner.strong,
            json!({ "account_id": rev.account, "role": "reviewer" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    rev
}

pub(crate) async fn apply(srv: &Srv, who: &Person, body: Value) -> String {
    let (s, v) = srv
        .post("/v1/publisher-applications", &who.token, body)
        .await;
    assert_eq!(s, 201, "{v}");
    v["data"]["id"].as_str().unwrap().to_string()
}

pub(crate) async fn certify(
    srv: &Srv,
    rev: &Person,
    publisher: &str,
    decision: &str,
) -> (u16, Value) {
    srv.post(
        &format!("/v1/staff/publisher-applications/{publisher}/decisions"),
        &rev.strong,
        json!({ "decision": decision, "reason": "synthetic check" }),
    )
    .await
}

/// A certified personal publisher for `who`.
pub(crate) async fn certified(srv: &Srv, rev: &Person, who: &Person) -> String {
    let p = apply(srv, who, json!({ "display_name": "Acme Nests" })).await;
    let (s, v) = certify(srv, rev, &p, "approved").await;
    assert_eq!(s, 201, "{v}");
    p
}

pub(crate) fn d(c: char) -> String {
    format!("sha256:{}", c.to_string().repeat(64))
}

pub(crate) fn manifest(slug: &str, version: &str, schema: Option<Value>) -> Value {
    let mut m = json!({
        "name": slug,
        "version": version,
        "title": "Team digest",
        "summary": "Summarises synthetic team notes.",
        "licence": "MIT",
        "entrypoint": "/usr/local/bin/team-digest",
        "listing_address": format!("/acme/{slug}"),
        "egress": ["broker"],
        "capabilities": [],
        "syscalls": [],
        "external_writes": false,
        "operations": [{
            "name": "team.digest.ask",
            "version": "1.0",
            "fields": [{"name": "question", "max_chars": 500, "required": true}],
        }],
        "licences": {slug: "MIT"},
    });
    if let Some(s) = schema {
        m["config_schema"] = s;
    }
    m
}

pub(crate) fn build_record(build: &str, manifest: Value, image_char: char) -> Value {
    let sbom =
        canonical_json(&json!({"bomFormat": "CycloneDX", "specVersion": "1.5", "version": 1}));
    let sbom_digest = digest_of(sbom.as_bytes());
    let image_digest = d(image_char);
    let (source, vendor, mdig) = (d('1'), d('2'), d('3'));
    json!({
        "manifest": manifest,
        "manifest_digest": mdig,
        "source_digest": source,
        "vendor_digest": vendor,
        "sbom": sbom,
        "sbom_digest": sbom_digest,
        "image": format!("localhost/acme-team-digest@{image_digest}"),
        "image_digest": image_digest,
        "release_digest": release_digest(&source, &vendor, &mdig, &sbom_digest, &image_digest),
        "isolation": {"runtime": "runsc", "rootless": true, "build_network": "none"},
        "build_id": build,
    })
}

pub(crate) async fn template(srv: &Srv, who: &Person, publisher: &str, slug: &str) -> String {
    let (s, v) = srv
        .post(
            &format!("/v1/publishers/{publisher}/templates"),
            &who.token,
            json!({ "slug": slug, "name": "Team digest" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    v["data"]["id"].as_str().unwrap().to_string()
}

pub(crate) async fn request_build(
    srv: &Srv,
    who: &Person,
    publisher: &str,
    template: &str,
    version: &str,
) -> (u16, Value) {
    srv.post(
        &format!("/v1/publishers/{publisher}/builds"),
        &who.token,
        json!({
            "template_id": template,
            "version": version,
            "source": { "repository": "https://git.example.org/acme/team-digest.git",
                        "commit": "a".repeat(40) },
        }),
    )
    .await
}

pub(crate) struct Built {
    pub(crate) release: String,
    pub(crate) review: String,
    pub(crate) digest: String,
}

/// Request a build and answer it as the build plane would.
pub(crate) async fn build(
    srv: &Srv,
    who: &Person,
    publisher: &str,
    template: &str,
    version: &str,
    schema: Option<Value>,
    image_char: char,
) -> Built {
    let (s, v) = request_build(srv, who, publisher, template, version).await;
    assert_eq!(s, 201, "{v}");
    let id = v["data"]["id"].as_str().unwrap().to_string();
    let record = build_record(&id, manifest("team-digest", version, schema), image_char);
    let (s, v) = srv
        .call(
            Method::POST,
            "/internal/v1/marketplace/builds",
            Some(OPS),
            Some(record.clone()),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    Built {
        release: v["data"]["release_id"].as_str().unwrap().into(),
        review: v["data"]["review_id"].as_str().unwrap().into(),
        digest: record["release_digest"].as_str().unwrap().into(),
    }
}

pub(crate) fn evidence(stage: &str, decision: &str, findings: Value) -> String {
    let mut facts = json!({ "ran": "synthetic" });
    if stage == "ai_analysis" {
        facts["model"] = json!("qwen-latest-flash-next");
    }
    canonical_json(&json!({
        "stage": stage, "decision": decision, "reason": "synthetic", "findings": findings,
        "evidence": facts,
    }))
}

pub(crate) async fn stage_with(
    srv: &Srv,
    review: &str,
    stage: &str,
    decision: &str,
    findings: Value,
) -> (u16, Value) {
    let ev = evidence(stage, decision, findings);
    srv.call(
        Method::POST,
        &format!("/internal/v1/marketplace/reviews/{review}/stages"),
        Some(OPS),
        Some(json!({
            "stage": stage, "decision": decision, "reason": "synthetic",
            "evidence_digest": digest_of(ev.as_bytes()), "evidence": ev,
        })),
    )
    .await
}

pub(crate) async fn stage(srv: &Srv, review: &str, stage: &str, decision: &str) -> (u16, Value) {
    stage_with(srv, review, stage, decision, json!([])).await
}

pub(crate) async fn pass_analysis(srv: &Srv, review: &str) {
    for (st, dec) in [
        ("deterministic_scan", "passed"),
        ("ai_analysis", "advisory"),
        ("behavioral_test", "passed"),
    ] {
        let (s, v) = stage(srv, review, st, dec).await;
        assert_eq!(s, 201, "{st}: {v}");
    }
}

pub(crate) async fn approve(srv: &Srv, rev: &Person, b: &Built) -> (u16, Value) {
    srv.post(
        &format!("/v1/staff/reviews/{}/decisions", b.review),
        &rev.strong,
        json!({ "decision": "approved", "reason": "reviewed", "release_digest": b.digest }),
    )
    .await
}

/// A built, analysed and attested release.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn attested(
    srv: &Srv,
    rev: &Person,
    who: &Person,
    publisher: &str,
    template: &str,
    version: &str,
    schema: Option<Value>,
    image_char: char,
) -> Built {
    let b = build(srv, who, publisher, template, version, schema, image_char).await;
    pass_analysis(srv, &b.review).await;
    let (s, v) = approve(srv, rev, &b).await;
    assert_eq!(s, 201, "{v}");
    b
}

pub(crate) async fn listing(
    srv: &Srv,
    who: &Person,
    publisher: &str,
    release: &str,
    vis: &str,
) -> String {
    let (s, v) = srv
        .post(
            &format!("/v1/publishers/{publisher}/listings"),
            &who.token,
            json!({ "release_id": release, "title": "Team digest", "summary": "Digests of notes",
                    "visibility": vis }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["data"]["status"], "draft");
    v["data"]["id"].as_str().unwrap().to_string()
}

pub(crate) async fn publish(
    srv: &Srv,
    token: &str,
    publisher: &str,
    listing: &str,
    body: Value,
) -> (u16, Value) {
    srv.post(
        &format!("/v1/publishers/{publisher}/listings/{listing}/publish"),
        token,
        body,
    )
    .await
}

pub(crate) fn schema_team() -> Value {
    let fx: Value = serde_json::from_str(FIXTURES).unwrap();
    fx["schemas"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "team settings with a secret")
        .unwrap()["schema"]
        .clone()
}

fn pairs(v: &Value) -> BTreeSet<(String, String)> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|p| (p[0].as_str().unwrap().into(), p[1].as_str().unwrap().into()))
        .collect()
}

fn issues(v: &Value) -> BTreeSet<(String, String)> {
    v["error"]["details"]["issues"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|i| {
            (
                i["path"].as_str().unwrap().into(),
                i["code"].as_str().unwrap().into(),
            )
        })
        .collect()
}

/// The typed events an account has been sent, as stored for `/v1/events`.
pub(crate) async fn events_of(srv: &Srv, account: &str) -> Vec<(String, Value)> {
    let who = account.to_string();
    srv.state
        .store
        .group_read(
            move |c| -> Result<Vec<(String, Value)>, crate::groups::GErr> {
                let mut stmt = c.prepare(
                    "SELECT type, data FROM account_events WHERE account_id = ?1 ORDER BY id",
                )?;
                let rows = stmt
                    .query_map([&who], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            serde_json::from_str(&r.get::<_, String>(1)?).unwrap_or(Value::Null),
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            },
        )
        .await
        .unwrap()
}

// ---- the flag ---------------------------------------------------------------------------------------

/// Every M2 route answers 404 feature_disabled while the flag is off, for the platform owner too;
/// so do the build plane's.
#[tokio::test]
async fn every_m2_route_is_hidden_while_the_flag_is_off() {
    let (srv, owner) = setup().await;
    let p = "pub_aaaaaaaaaaaaaaaaaaaaaaaaaa";
    let i = "ins_aaaaaaaaaaaaaaaaaaaaaaaaaa";
    for (method, path, body) in [
        (Method::GET, "/v1/publishers".to_string(), None),
        (Method::GET, format!("/v1/publishers/{p}"), None),
        (Method::GET, format!("/v1/publishers/{p}/roles"), None),
        (
            Method::POST,
            format!("/v1/publishers/{p}/roles"),
            Some(json!({ "account_id": "acct_x", "role": "publisher_admin" })),
        ),
        (
            Method::DELETE,
            format!("/v1/publishers/{p}/roles/prl_aaaaaaaaaaaaaaaaaaaaaaaaaa"),
            None,
        ),
        (Method::GET, format!("/v1/publishers/{p}/templates"), None),
        (
            Method::POST,
            format!("/v1/publishers/{p}/templates"),
            Some(json!({ "slug": "x", "name": "Xx" })),
        ),
        (Method::GET, format!("/v1/publishers/{p}/builds"), None),
        (Method::GET, format!("/v1/publishers/{p}/reviews"), None),
        (Method::GET, format!("/v1/publishers/{p}/listings"), None),
        (Method::GET, "/v1/marketplace/listings".to_string(), None),
        (Method::GET, "/v1/nest-installations".to_string(), None),
        (
            Method::POST,
            "/v1/nest-installations".to_string(),
            Some(json!({ "listing_id": "lst_x" })),
        ),
        (Method::GET, format!("/v1/nest-installations/{i}"), None),
        (Method::DELETE, format!("/v1/nest-installations/{i}"), None),
        (
            Method::PUT,
            format!("/v1/nest-installations/{i}/configuration"),
            Some(json!({ "configuration": {} })),
        ),
        (
            Method::GET,
            "/v1/staff/publisher-applications".to_string(),
            None,
        ),
        (Method::GET, "/v1/staff/reviews".to_string(), None),
        (
            Method::POST,
            "/v1/staff/listings/lst_x/moderation".to_string(),
            Some(json!({ "action": "suspend", "reason": "x" })),
        ),
    ] {
        let (s, v) = srv
            .call(method.clone(), &path, Some(&owner.strong), body)
            .await;
        assert_eq!(
            (s, code(&v)),
            (404, "feature_disabled"),
            "{method} {path}: {v}"
        );
    }
    // The public catalog, without a credential, too.
    let (s, v) = srv
        .call(Method::GET, "/v1/marketplace/listings", None, None)
        .await;
    assert_eq!((s, code(&v)), (404, "feature_disabled"), "{v}");
    let (s, v) = srv
        .call(
            Method::GET,
            "/internal/v1/marketplace/build-requests",
            Some(OPS),
            None,
        )
        .await;
    assert_eq!((s, code(&v)), (404, "feature_disabled"), "{v}");
}

// ---- certification, roles, revocation -------------------------------------------------------------

/// R-47: staff certify an application (the owner is the reviewer); nobody else can, nor a reviewer
/// with a stake, nor without step-up; a decision is final; an uncertified publisher creates nothing.
#[tokio::test]
async fn staff_certify_publishers_and_only_staff_without_a_stake() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    let rev = reviewer(&srv, &owner, "mprev1").await;
    let ann = srv.person("mpann1").await;
    let p = apply(&srv, &ann, json!({ "display_name": "Ann Apps" })).await;

    // Not yet certified: no templates.
    let (s, v) = srv
        .post(
            &format!("/v1/publishers/{p}/templates"),
            &ann.token,
            json!({ "slug": "x", "name": "Xx" }),
        )
        .await;
    assert_eq!((s, code(&v)), (403, "publisher_not_certified"), "{v}");

    // The queue: staff only, with step-up.
    let (s, v) = srv
        .get("/v1/staff/publisher-applications", &ann.strong)
        .await;
    assert_eq!((s, code(&v)), (403, "staff_required"), "{v}");
    let (s, v) = srv
        .get("/v1/staff/publisher-applications", &rev.token)
        .await;
    assert_eq!((s, code(&v)), (401, "step_up_required"), "{v}");
    let (s, v) = srv
        .get("/v1/staff/publisher-applications", &rev.strong)
        .await;
    assert_eq!(s, 200, "{v}");
    let row = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == p.as_str())
        .unwrap()
        .clone();
    assert_eq!(row["conflict"], false);

    // Who may not decide.
    let (s, v) = certify(&srv, &ann, &p, "approved").await;
    assert_eq!((s, code(&v)), (403, "staff_required"), "{v}");
    let (s, v) = srv
        .post(
            &format!("/v1/staff/publisher-applications/{p}/decisions"),
            &rev.token,
            json!({ "decision": "approved", "reason": "x" }),
        )
        .await;
    assert_eq!((s, code(&v)), (401, "step_up_required"), "{v}");
    // A reviewer who applied for a publisher may not certify it.
    let own = apply(&srv, &rev, json!({ "display_name": "Rev Apps" })).await;
    let (s, v) = certify(&srv, &rev, &own, "approved").await;
    assert_eq!((s, code(&v)), (403, "staff_conflict"), "{v}");
    let (s, v) = certify(&srv, &rev, "pub_missingaaaaaaaaaaaaaaaaaa", "approved").await;
    assert_eq!((s, code(&v)), (404, "not_found"), "{v}");

    let (s, v) = certify(&srv, &rev, &p, "approved").await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["data"]["status"], "approved");
    assert_eq!(v["data"]["decision_reason"], "synthetic check");
    let (s, v) = certify(&srv, &rev, &p, "rejected").await;
    assert_eq!((s, code(&v)), (409, "application_decided"), "{v}");

    // The publisher sees it; others get 404.
    let (s, v) = srv.get(&format!("/v1/publishers/{p}"), &ann.token).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (v["data"]["status"].as_str(), v["data"]["role"].as_str()),
        (Some("approved"), Some("owner"))
    );
    let bob = srv.person("mpbob1").await;
    let (s, v) = srv.get(&format!("/v1/publishers/{p}"), &bob.token).await;
    assert_eq!((s, code(&v)), (404, "not_found"), "{v}");
    let (s, v) = srv.get("/v1/publishers", &ann.token).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"].as_array().unwrap().len(), 1);
    let (_, v) = srv.get("/v1/publishers", &bob.token).await;
    assert!(v["data"].as_array().unwrap().is_empty());
    // The decision reached the publisher's event stream.
    assert!(events_of(&srv, &ann.account)
        .await
        .iter()
        .any(|(k, d)| k == "nest.publisher.decision" && d["status"] == "approved"));
    // Now the publisher may create a template; slugs are unique per publisher.
    template(&srv, &ann, &p, "team-digest").await;
    let (s, v) = srv
        .post(
            &format!("/v1/publishers/{p}/templates"),
            &ann.token,
            json!({ "slug": "team-digest", "name": "Again" }),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "template_exists"), "{v}");
}

/// Org publishers: the org's owner and admins act for it and hand out publisher roles to active
/// members; `publisher_admin` acts, `publisher_finance` only reads; a role ends with revocation.
#[tokio::test]
async fn org_publisher_roles_decide_who_may_publish() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    srv.global_flag("orgs").await;
    let rev = reviewer(&srv, &owner, "mprev2").await;
    let boss = srv.person("mpboss2").await;
    let org = srv.org(&boss, "acmepub", 5).await;
    let (dev, fin, plain, outsider) = (
        srv.person("mpdev2").await,
        srv.person("mpfin2").await,
        srv.person("mpplain2").await,
        srv.person("mpout2").await,
    );
    for (who, handle) in [(&dev, "dev2"), (&fin, "fin2"), (&plain, "plain2")] {
        srv.join(&org, &boss, who, handle, &[]).await;
    }
    let p = apply(
        &srv,
        &boss,
        json!({ "display_name": "Acme Org Nests", "org_id": org }),
    )
    .await;
    let (s, v) = certify(&srv, &rev, &p, "approved").await;
    assert_eq!(s, 201, "{v}");
    let roles = format!("/v1/publishers/{p}/roles");

    // A plain member is not on the team; nor is an outsider.
    let (s, v) = srv.get(&format!("/v1/publishers/{p}"), &plain.token).await;
    assert_eq!((s, code(&v)), (404, "not_found"), "{v}");
    let (s, _) = srv
        .get(&format!("/v1/publishers/{p}"), &outsider.token)
        .await;
    assert_eq!(s, 404);
    // Only the org's owner or admin grants; only to an active member.
    let (s, v) = srv
        .post(
            &roles,
            &outsider.token,
            json!({ "account_id": dev.account, "role": "publisher_admin" }),
        )
        .await;
    assert_eq!((s, code(&v)), (404, "not_found"), "{v}");
    let (s, v) = srv
        .post(
            &roles,
            &boss.token,
            json!({ "account_id": outsider.account, "role": "publisher_admin" }),
        )
        .await;
    assert_eq!((s, code(&v)), (400, "not_org_member"), "{v}");
    let (s, v) = srv
        .post(
            &roles,
            &boss.token,
            json!({ "account_id": dev.account, "role": "owner" }),
        )
        .await;
    assert_eq!((s, code(&v)), (400, "invalid_role"), "{v}");
    let (s, v) = srv
        .post(
            &roles,
            &boss.token,
            json!({ "account_id": dev.account, "role": "publisher_admin" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let dev_role = v["data"]["id"].as_str().unwrap().to_string();
    let (s, v) = srv
        .post(
            &roles,
            &boss.token,
            json!({ "account_id": dev.account, "role": "publisher_admin" }),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "role_already_held"), "{v}");
    let (s, v) = srv
        .post(
            &roles,
            &boss.token,
            json!({ "account_id": fin.account, "role": "publisher_finance" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");

    // publisher_admin acts; finance reads but may not act or grant.
    let (s, v) = srv.get(&format!("/v1/publishers/{p}"), &dev.token).await;
    assert_eq!(
        (s, v["data"]["role"].as_str()),
        (200, Some("publisher_admin")),
        "{v}"
    );
    template(&srv, &dev, &p, "team-digest").await;
    let (s, v) = srv
        .get(&format!("/v1/publishers/{p}/templates"), &fin.token)
        .await;
    assert_eq!(
        (s, v["data"].as_array().map(Vec::len)),
        (200, Some(1)),
        "{v}"
    );
    let (s, v) = srv
        .post(
            &format!("/v1/publishers/{p}/templates"),
            &fin.token,
            json!({ "slug": "other", "name": "Other" }),
        )
        .await;
    assert_eq!((s, code(&v)), (403, "publisher_role_required"), "{v}");
    let (s, v) = srv
        .post(
            &roles,
            &dev.token,
            json!({ "account_id": plain.account, "role": "publisher_admin" }),
        )
        .await;
    assert_eq!((s, code(&v)), (403, "publisher_role_required"), "{v}");
    let (s, v) = srv.get(&roles, &fin.token).await;
    assert_eq!(
        (s, v["data"].as_array().map(Vec::len)),
        (200, Some(2)),
        "{v}"
    );
    // The team can ask for builds.
    let (_, v) = srv
        .get(&format!("/v1/publishers/{p}/templates"), &dev.token)
        .await;
    let tid = v["data"][0]["id"].as_str().unwrap().to_string();
    let (s, v) = request_build(&srv, &dev, &p, &tid, "1.0.0").await;
    assert_eq!(s, 201, "{v}");
    // A revoked role is no role.
    let (s, _) = srv
        .call(
            Method::DELETE,
            &format!("{roles}/{dev_role}"),
            Some(&boss.token),
            None,
        )
        .await;
    assert_eq!(s, 204);
    let (s, _) = srv.get(&format!("/v1/publishers/{p}"), &dev.token).await;
    assert_eq!(s, 404);
    let (s, _) = srv
        .call(
            Method::DELETE,
            &format!("{roles}/{dev_role}"),
            Some(&boss.token),
            None,
        )
        .await;
    assert_eq!(s, 404);
}

/// R-47 revocation, SEC-04: a revoked publisher's listings are suspended (their installations stop
/// taking jobs), queued builds are cancelled, it creates nothing more, and staff can no longer
/// decide its reviews. Pigeonpost's own publisher is never revoked this way.
#[tokio::test]
async fn revocation_suspends_listings_and_stops_new_work() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    let rev = reviewer(&srv, &owner, "mprev3").await;
    let ann = srv.person("mpann3").await;
    let p = certified(&srv, &rev, &ann).await;
    let t = template(&srv, &ann, &p, "team-digest").await;
    let b1 = attested(&srv, &rev, &ann, &p, &t, "1.0.0", None, 'a').await;
    let l = listing(&srv, &ann, &p, &b1.release, "public").await;
    let (s, v) = publish(&srv, &ann.strong, &p, &l, json!({})).await;
    assert_eq!(s, 200, "{v}");
    let b2 = build(&srv, &ann, &p, &t, "1.1.0", None, 'b').await;
    let (s, v) = request_build(&srv, &ann, &p, &t, "1.2.0").await;
    assert_eq!(s, 201, "{v}");
    let queued = v["data"]["id"].as_str().unwrap().to_string();

    let path = format!("/v1/staff/publishers/{p}/revocation");
    let (s, v) = srv.post(&path, &ann.strong, json!({ "reason": "x" })).await;
    assert_eq!((s, code(&v)), (403, "staff_required"), "{v}");
    let (s, v) = srv
        .post(
            &path,
            &rev.strong,
            json!({ "reason": "synthetic abuse report" }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (
            v["data"]["listings_suspended"].as_i64(),
            v["data"]["builds_cancelled"].as_i64()
        ),
        (Some(1), Some(1))
    );
    let (s, v) = srv
        .post(&path, &rev.strong, json!({ "reason": "again" }))
        .await;
    assert_eq!((s, code(&v)), (409, "publisher_revoked"), "{v}");

    let (s, v) = srv
        .get(&format!("/v1/publishers/{p}/builds/{queued}"), &ann.token)
        .await;
    assert_eq!(
        (s, v["data"]["status"].as_str()),
        (200, Some("cancelled")),
        "{v}"
    );
    let (s, v) = request_build(&srv, &ann, &p, &t, "1.3.0").await;
    assert_eq!((s, code(&v)), (403, "publisher_revoked"), "{v}");
    let (s, v) = srv
        .get(&format!("/v1/publishers/{p}/listings"), &ann.token)
        .await;
    assert_eq!(
        (s, v["data"][0]["status"].as_str()),
        (200, Some("suspended")),
        "{v}"
    );
    let (s, v) = srv
        .get(&format!("/v1/marketplace/listings/{l}"), &ann.token)
        .await;
    assert_eq!(s, 200, "the team still reads it: {v}");
    let bob = srv.person("mpbob3").await;
    let (s, _) = srv
        .get(&format!("/v1/marketplace/listings/{l}"), &bob.token)
        .await;
    assert_eq!(s, 404);
    // Its open review can no longer be decided.
    pass_analysis(&srv, &b2.review).await;
    let (s, v) = approve(&srv, &rev, &b2).await;
    assert_eq!((s, code(&v)), (409, "publisher_not_certified"), "{v}");
    // A build answered for a revoked publisher is refused.
    let (s, v) = srv
        .call(
            Method::POST,
            "/internal/v1/marketplace/builds",
            Some(OPS),
            Some(build_record(
                &queued,
                manifest("team-digest", "1.2.0", None),
                'c',
            )),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "build_not_queued"), "{v}");
}

// ---- the review pipeline, third party ------------------------------------------------------------

/// The publisher's build goes through M1's pipeline: intake from the build plane, stages in order
/// with no skipping, idempotent replays, events, a publisher view with redacted findings, the staff
/// conflict rule for third parties, and an approval that does not publish anything by itself.
#[tokio::test]
async fn third_party_releases_go_through_the_review_stages_in_order() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    let rev = reviewer(&srv, &owner, "mprev4").await;
    let ann = srv.person("mpann4").await;
    let p = certified(&srv, &rev, &ann).await;
    let t = template(&srv, &ann, &p, "team-digest").await;

    // Build requests: pinned https sources only, one per version.
    let (s, v) = srv
        .post(
            &format!("/v1/publishers/{p}/builds"),
            &ann.token,
            json!({ "template_id": t, "version": "1.0.0",
                    "source": { "repository": "https://user:pw@git.example.org/a/b", "commit": "a".repeat(40) } }),
        )
        .await;
    assert_eq!((s, code(&v)), (400, "invalid_request"), "{v}");
    let (s, v) = request_build(&srv, &ann, &p, &t, "1.0.0").await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["data"]["status"], "queued");
    let bid = v["data"]["id"].as_str().unwrap().to_string();
    let (s, v) = request_build(&srv, &ann, &p, &t, "1.0.0").await;
    assert_eq!((s, code(&v)), (409, "version_exists"), "{v}");
    let (s, v) = srv
        .call(
            Method::GET,
            "/internal/v1/marketplace/build-requests",
            Some(OPS),
            None,
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert!(v["data"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b["id"] == bid.as_str() && b["template_slug"] == "team-digest"));

    // The build plane's answer must match the request and stay off Pigeonpost's addresses.
    for (m, why) in [
        (manifest("other-name", "1.0.0", None), "another name"),
        (manifest("team-digest", "2.0.0", None), "another version"),
        (
            {
                let mut m = manifest("team-digest", "1.0.0", None);
                m["listing_address"] = json!("/pigeonpost/team-digest");
                m
            },
            "a platform address",
        ),
    ] {
        let (s, v) = srv
            .call(
                Method::POST,
                "/internal/v1/marketplace/builds",
                Some(OPS),
                Some(build_record(&bid, m, 'a')),
            )
            .await;
        assert_eq!(s, 400, "{why}: {v}");
    }
    let record = build_record(&bid, manifest("team-digest", "1.0.0", None), 'a');
    let (s, v) = srv
        .call(
            Method::POST,
            "/internal/v1/marketplace/builds",
            Some(OPS),
            Some(record.clone()),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    assert!(
        v["data"]["listing_id"].is_null(),
        "a third party's listing is its own to make"
    );
    let b = Built {
        release: v["data"]["release_id"].as_str().unwrap().into(),
        review: v["data"]["review_id"].as_str().unwrap().into(),
        digest: record["release_digest"].as_str().unwrap().into(),
    };
    let (s, v) = srv
        .call(
            Method::POST,
            "/internal/v1/marketplace/builds",
            Some(OPS),
            Some(record),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "build_not_queued"), "{v}");
    let (_, v) = srv
        .get(&format!("/v1/publishers/{p}/builds/{bid}"), &ann.token)
        .await;
    assert_eq!(
        (
            v["data"]["status"].as_str(),
            v["data"]["review_id"].as_str()
        ),
        (Some("built"), Some(b.review.as_str()))
    );

    let rpath = format!("/v1/publishers/{p}/reviews/{}", b.review);
    let (s, v) = srv.get(&rpath, &ann.token).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["status"], "submitted");
    assert_eq!(v["data"]["artifact_digest"], b.digest.as_str());
    assert_eq!(v["data"]["stages"].as_array().unwrap().len(), 6);
    assert_eq!(v["data"]["stages"][0]["status"], "passed");
    assert_eq!(v["data"]["stages"][1]["status"], "pending");
    let first_revision = v["data"]["revision"].as_i64().unwrap();

    // No skipping.
    for st in ["ai_analysis", "behavioral_test"] {
        let dec = if st == "ai_analysis" {
            "advisory"
        } else {
            "passed"
        };
        let (s, v) = stage(&srv, &b.review, st, dec).await;
        assert_eq!((s, code(&v)), (409, "stage_out_of_order"), "{st}: {v}");
    }
    let scan_findings = json!([{ "rule": "secret.github_token", "path": "src/x.rs", "detail": "a credential is committed", "blocker": false }]);
    let (s, v) = stage_with(
        &srv,
        &b.review,
        "deterministic_scan",
        "passed",
        scan_findings.clone(),
    )
    .await;
    assert_eq!(s, 201, "{v}");
    let decision = v["data"]["decision_id"].clone();
    // A replay is the same decision: no new row, no new revision, no new event.
    let events_before = events_of(&srv, &ann.account).await.len();
    let (s, v) = stage_with(
        &srv,
        &b.review,
        "deterministic_scan",
        "passed",
        scan_findings,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (
            v["data"]["decision_id"].clone(),
            v["data"]["replayed"].clone()
        ),
        (decision, json!(true))
    );
    assert_eq!(events_of(&srv, &ann.account).await.len(), events_before);
    // A second, different record of a recorded stage is refused.
    let (s, v) = stage_with(
        &srv,
        &b.review,
        "deterministic_scan",
        "passed",
        json!([{"rule": "x", "blocker": false}]),
    )
    .await;
    assert_eq!((s, code(&v)), (409, "stage_already_recorded"), "{v}");
    let (s, _) = stage_with(
        &srv,
        &b.review,
        "ai_analysis",
        "advisory",
        json!([{ "rule": "ai.high", "detail": "model text for staff only", "blocker": false }]),
    )
    .await;
    assert_eq!(s, 201);
    let (s, _) = stage_with(
        &srv,
        &b.review,
        "behavioral_test",
        "passed",
        json!([{ "rule": "behaviour.note", "detail": "honeytoken layout for staff only", "blocker": false }]),
    )
    .await;
    assert_eq!(s, 201);
    let (_, v) = srv.get(&rpath, &ann.token).await;
    assert_eq!(v["data"]["status"], "in_review");
    assert!(v["data"]["revision"].as_i64().unwrap() > first_revision);
    assert!(v["data"]["allowed_actions"]
        .as_array()
        .unwrap()
        .contains(&json!("withdraw")));

    // SEC-06: redacted findings.
    let (s, v) = srv.get(&format!("{rpath}/findings"), &ann.token).await;
    assert_eq!(s, 200, "{v}");
    let text = v.to_string();
    assert!(
        text.contains("a credential is committed"),
        "own scan findings shown: {v}"
    );
    assert!(
        !text.contains("model text for staff only") && !text.contains("honeytoken layout"),
        "{v}"
    );
    let ai = v["data"]["stages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["stage"] == "ai_analysis")
        .unwrap()
        .clone();
    assert_eq!(ai["advisory_findings"], 1);
    let bob = srv.person("mpbob4").await;
    let (s, _) = srv.get(&format!("{rpath}/findings"), &bob.token).await;
    assert_eq!(s, 404);
    let (s, _) = srv.get(&rpath, &bob.token).await;
    assert_eq!(s, 404);

    // SEC-07: the publisher's owner is not an unconflicted reviewer even if made staff.
    let (s, v) = srv
        .post(
            "/v1/staff/roles",
            &owner.strong,
            json!({ "account_id": ann.account, "role": "reviewer" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let (s, v) = approve(&srv, &ann, &b).await;
    assert_eq!((s, code(&v)), (403, "staff_conflict"), "{v}");
    let (s, v) = srv.get("/v1/staff/reviews", &rev.strong).await;
    assert_eq!(s, 200, "{v}");
    let row = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == b.review.as_str())
        .unwrap()
        .clone();
    assert_eq!(
        (row["first_party"].as_bool(), row["conflict"].as_bool()),
        (Some(false), Some(false))
    );
    let (s, v) = srv.get("/v1/staff/reviews", &ann.strong).await;
    assert_eq!(s, 200);
    let row = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == b.review.as_str())
        .unwrap()
        .clone();
    assert_eq!(row["conflict"], true);

    let (s, v) = approve(&srv, &rev, &b).await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["data"]["first_party"], false);
    let (_, v) = srv.get(&rpath, &ann.token).await;
    assert_eq!(v["data"]["status"], "approved", "{v}");
    assert!(v["data"]["approval_ref"]
        .as_str()
        .unwrap()
        .starts_with("att_"));
    assert!(v["data"]["allowed_actions"]
        .as_array()
        .unwrap()
        .contains(&json!("publish_listing")));
    let (s, v) = srv
        .post(&format!("{rpath}/withdraw"), &ann.token, json!({}))
        .await;
    assert_eq!((s, code(&v)), (409, "review_closed"), "{v}");
    // Nothing was listed by the approval.
    let (_, v) = srv
        .get(&format!("/v1/publishers/{p}/listings"), &ann.token)
        .await;
    assert!(v["data"].as_array().unwrap().is_empty());
    // The publisher heard each step, ids only.
    let kinds: Vec<String> = events_of(&srv, &ann.account)
        .await
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    for k in [
        "nest.review.stage_changed",
        "nest.review.decision",
        "nest.review.completed",
    ] {
        assert!(kinds.iter().any(|x| x == k), "{k} in {kinds:?}");
    }
    let (s, v) = srv
        .get(&format!("/v1/publishers/{p}/reviews"), &ann.token)
        .await;
    assert_eq!((s, v["data"].as_array().map(Vec::len)), (200, Some(1)));
}

/// A failed stage blocks the review (shown as changes requested, with the failed stage as the
/// decision); a build failure is recorded for the publisher; a review can be withdrawn.
#[tokio::test]
async fn failures_and_withdrawals_are_visible_to_the_publisher() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    let rev = reviewer(&srv, &owner, "mprev5").await;
    let ann = srv.person("mpann5").await;
    let p = certified(&srv, &rev, &ann).await;
    let t = template(&srv, &ann, &p, "team-digest").await;
    let b = build(&srv, &ann, &p, &t, "1.0.0", None, 'a').await;
    let (s, _) = stage_with(
        &srv,
        &b.review,
        "deterministic_scan",
        "failed",
        json!([{ "rule": "manifest.egress", "path": "nest-release.toml", "detail": "egress to internet", "blocker": true }]),
    )
    .await;
    assert_eq!(s, 201);
    let (_, v) = srv
        .get(
            &format!("/v1/publishers/{p}/reviews/{}", b.review),
            &ann.token,
        )
        .await;
    assert_eq!(v["data"]["status"], "changes_requested", "{v}");
    assert_eq!(v["data"]["stages"][1]["status"], "failed");
    assert_eq!(v["data"]["stages"][1]["blocker_code"], "stage_failed");
    assert!(v["data"]["allowed_actions"]
        .as_array()
        .unwrap()
        .contains(&json!("resubmit")));
    assert!(events_of(&srv, &ann.account)
        .await
        .iter()
        .any(|(k, _)| k == "nest.review.blocked"));
    let (s, v) = srv
        .post(
            &format!("/v1/publishers/{p}/reviews/{}/withdraw", b.review),
            &ann.token,
            json!({}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["status"], "withdrawn");

    let (s, v) = request_build(&srv, &ann, &p, &t, "1.0.1").await;
    assert_eq!(s, 201);
    let bid = v["data"]["id"].as_str().unwrap().to_string();
    let path = format!("/internal/v1/marketplace/build-requests/{bid}/failure");
    let (s, v) = srv
        .call(
            Method::POST,
            &path,
            Some(OPS),
            Some(json!({ "code": "fetch_failed", "reason": "the commit was not found" })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, _) = srv
        .call(
            Method::POST,
            &path,
            Some(OPS),
            Some(json!({ "code": "fetch_failed", "reason": "again" })),
        )
        .await;
    assert_eq!(s, 409);
    let (_, v) = srv
        .get(&format!("/v1/publishers/{p}/builds/{bid}"), &ann.token)
        .await;
    assert_eq!(v["data"]["status"], "failed");
    assert_eq!(v["data"]["failure"]["code"], "fetch_failed");
    // The internal routes are the ops service's only.
    let (s, _) = srv
        .call(
            Method::GET,
            "/internal/v1/marketplace/build-requests",
            Some(&ann.strong),
            None,
        )
        .await;
    assert!(s == 401 || s == 403, "{s}");
}

// ---- listings ----------------------------------------------------------------------------------------

/// R-52: visibility is public (searchable), unlisted (by id) or private (the team); publishing
/// needs step-up and an attested release; staff moderation outranks the publisher.
#[tokio::test]
async fn listings_publish_only_attested_releases_and_honour_visibility() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    let rev = reviewer(&srv, &owner, "mprev6").await;
    let (ann, bob) = (srv.person("mpann6").await, srv.person("mpbob6").await);
    let p = certified(&srv, &rev, &ann).await;
    let t = template(&srv, &ann, &p, "team-digest").await;
    let unreviewed = build(&srv, &ann, &p, &t, "0.9.0", None, 'f').await;
    let l = listing(&srv, &ann, &p, &unreviewed.release, "private").await;
    let (s, v) = srv
        .post(
            &format!("/v1/publishers/{p}/listings"),
            &ann.token,
            json!({ "release_id": unreviewed.release, "title": "Again", "summary": "Again", "visibility": "public" }),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "listing_address_taken"), "{v}");
    let (s, v) = srv
        .post(
            &format!("/v1/publishers/{p}/listings"),
            &ann.token,
            json!({ "release_id": unreviewed.release, "title": "X", "summary": "Again", "visibility": "secret" }),
        )
        .await;
    assert_eq!((s, code(&v)), (400, "invalid_request"), "{v}");

    // SEC-03: an unattested release is never published; publishing needs step-up.
    let (s, v) = publish(&srv, &ann.strong, &p, &l, json!({})).await;
    assert_eq!((s, code(&v)), (409, "review_required"), "{v}");
    let b = attested(&srv, &rev, &ann, &p, &t, "1.0.0", None, 'a').await;
    let (s, v) = publish(&srv, &ann.token, &p, &l, json!({ "release_id": b.release })).await;
    assert_eq!((s, code(&v)), (401, "step_up_required"), "{v}");
    let (s, v) = publish(
        &srv,
        &bob.strong,
        &p,
        &l,
        json!({ "release_id": b.release }),
    )
    .await;
    assert_eq!((s, code(&v)), (404, "not_found"), "{v}");
    let (s, v) = publish(
        &srv,
        &ann.strong,
        &p,
        &l,
        json!({ "release_id": b.release }),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (
            v["data"]["status"].as_str(),
            v["data"]["release_id"].as_str()
        ),
        (Some("published"), Some(b.release.as_str()))
    );

    let read = |token: Option<String>| {
        let path = format!("/v1/marketplace/listings/{l}");
        let srv = &srv;
        async move { srv.call(Method::GET, &path, token.as_deref(), None).await }
    };
    let search = || async {
        let (s, v) = srv
            .call(Method::GET, "/v1/marketplace/listings?q=digest", None, None)
            .await;
        assert_eq!(s, 200, "{v}");
        v["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["id"] == l.as_str())
    };
    // Private: the team only, never in the catalog.
    assert_eq!(read(Some(ann.token.clone())).await.0, 200);
    assert_eq!(read(Some(bob.token.clone())).await.0, 404);
    assert_eq!(read(None).await.0, 404);
    assert!(!search().await);
    // Unlisted: anyone with the id, still not in the catalog.
    let edit = format!("/v1/publishers/{p}/listings/{l}");
    let (s, v) = srv
        .patch(
            &edit,
            &ann.token,
            json!({ "visibility": "unlisted", "description": "Synthetic." }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = read(None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["description"], "Synthetic.");
    assert_eq!(v["data"]["operations"][0]["name"], "team.digest.ask");
    assert_eq!(v["data"]["price"]["amount_minor"], 0);
    assert!(!search().await);
    // Public: searchable, by anyone.
    let (s, _) = srv
        .patch(&edit, &ann.token, json!({ "visibility": "public" }))
        .await;
    assert_eq!(s, 200);
    assert!(search().await);
    let (s, v) = srv
        .call(
            Method::GET,
            "/v1/marketplace/listings?q=nothing-matches-this",
            None,
            None,
        )
        .await;
    assert_eq!((s, v["data"].as_array().map(Vec::len)), (200, Some(0)));
    // Staff-controlled fields are not the publisher's.
    let (s, v) = srv
        .patch(&edit, &ann.token, json!({ "status": "published" }))
        .await;
    assert_eq!((s, code(&v)), (400, "invalid_request"), "{v}");
    let (s, v) = srv
        .patch(&edit, &bob.token, json!({ "title": "Mine now" }))
        .await;
    assert_eq!((s, code(&v)), (404, "not_found"), "{v}");

    // Staff suspend: gone from the catalog, and the publisher cannot undo it.
    let moderate = format!("/v1/staff/listings/{l}/moderation");
    let (s, v) = srv
        .post(
            &moderate,
            &bob.strong,
            json!({ "action": "suspend", "reason": "x" }),
        )
        .await;
    assert_eq!((s, code(&v)), (403, "staff_required"), "{v}");
    let (s, v) = srv
        .post(
            &moderate,
            &rev.strong,
            json!({ "action": "suspend", "reason": "synthetic report" }),
        )
        .await;
    assert_eq!(
        (s, v["data"]["status"].as_str()),
        (200, Some("suspended")),
        "{v}"
    );
    assert!(!search().await);
    let (s, v) = publish(&srv, &ann.strong, &p, &l, json!({})).await;
    assert_eq!((s, code(&v)), (409, "listing_suspended"), "{v}");
    let (s, v) = srv
        .post(&format!("{edit}/withdraw"), &ann.token, json!({}))
        .await;
    assert_eq!((s, code(&v)), (409, "listing_suspended"), "{v}");
    let (s, v) = srv
        .post(
            &moderate,
            &rev.strong,
            json!({ "action": "reinstate", "reason": "resolved" }),
        )
        .await;
    assert_eq!(
        (s, v["data"]["status"].as_str()),
        (200, Some("published")),
        "{v}"
    );
    assert!(search().await);
    // Withdrawn by the publisher: gone again.
    let (s, _) = srv
        .post(&format!("{edit}/withdraw"), &ann.token, json!({}))
        .await;
    assert_eq!(s, 200);
    assert!(!search().await);
}

// ---- installations -----------------------------------------------------------------------------------

/// A published listing with the shared fixture's configuration schema.
pub(crate) async fn published_with_schema(
    srv: &Srv,
    owner: &Person,
    tag: &str,
) -> (Person, Person, String, String, String, Built) {
    let rev = reviewer(srv, owner, &format!("mprev{tag}")).await;
    let ann = srv.person(&format!("mpann{tag}")).await;
    let p = certified(srv, &rev, &ann).await;
    let t = template(srv, &ann, &p, "team-digest").await;
    let b = attested(srv, &rev, &ann, &p, &t, "1.0.0", Some(schema_team()), 'a').await;
    let l = listing(srv, &ann, &p, &b.release, "public").await;
    let (s, v) = publish(srv, &ann.strong, &p, &l, json!({})).await;
    assert_eq!(s, 200, "{v}");
    (rev, ann, p, t, l, b)
}

/// CFG-01: the server answers the shared fixtures exactly as `pigeonpost-core` and the CLI do,
/// for schemas (at build intake) and for configurations (at install and configure).
#[tokio::test]
async fn the_server_agrees_with_the_shared_configuration_fixtures() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    let (_, ann, p, t, l, _) = published_with_schema(&srv, &owner, "7").await;
    let fx: Value = serde_json::from_str(FIXTURES).unwrap();
    let mut version = 0;
    for s in fx["schemas"].as_array().unwrap() {
        let want: BTreeSet<(String, String)> = pairs(&s["schema_issues"])
            .into_iter()
            .map(|(path, code)| (format!("/config_schema{path}"), code))
            .collect();
        version += 1;
        let (st, v) = request_build(&srv, &ann, &p, &t, &format!("2.0.{version}")).await;
        assert_eq!(st, 201, "{v}");
        let bid = v["data"]["id"].as_str().unwrap().to_string();
        let record = build_record(
            &bid,
            manifest(
                "team-digest",
                &format!("2.0.{version}"),
                Some(s["schema"].clone()),
            ),
            'e',
        );
        let (st, v) = srv
            .call(
                Method::POST,
                "/internal/v1/marketplace/builds",
                Some(OPS),
                Some(record),
            )
            .await;
        if want.is_empty() {
            assert_eq!(st, 201, "{}: {v}", s["name"]);
        } else {
            assert_eq!(st, 400, "{}: {v}", s["name"]);
            assert_eq!(issues(&v), want, "{}", s["name"]);
        }
    }
    // Configurations against the installed release's schema.
    let team = fx["schemas"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "team settings with a secret")
        .unwrap()
        .clone();
    let mut installed: Option<String> = None;
    for c in team["configs"].as_array().unwrap() {
        let want = pairs(&c["config_issues"]);
        let (s, v) = srv
            .post(
                "/v1/nest-installations",
                &ann.token,
                json!({ "listing_id": l, "configuration": c["config"] }),
            )
            .await;
        if want.is_empty() {
            assert_eq!(s, 201, "{}: {v}", c["name"]);
            let id = v["data"]["id"].as_str().unwrap().to_string();
            let (s, _) = srv
                .call(
                    Method::DELETE,
                    &format!("/v1/nest-installations/{id}"),
                    Some(&ann.token),
                    None,
                )
                .await;
            assert_eq!(s, 200);
            installed = Some(id);
        } else {
            assert_eq!(
                (s, code(&v)),
                (400, "invalid_configuration"),
                "{}: {v}",
                c["name"]
            );
            assert_eq!(issues(&v), want, "{}", c["name"]);
        }
    }
    assert!(installed.is_some());
}

/// R-51, R-68: install (free, test credits), configuration with sealed secrets, jobs pinned to the
/// installed release with its configuration, update, rollback, and uninstall; nobody else's.
#[tokio::test]
async fn installations_pin_configure_update_roll_back_and_uninstall() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    let (rev, ann, p, t, l, b1) = published_with_schema(&srv, &owner, "8").await;
    let (bob, eve) = (srv.person("mpbob8").await, srv.person("mpeve8").await);

    // Not visible, not installable; nothing for an API key either (a person installs).
    let (s, v) = srv
        .post(
            "/v1/nest-installations",
            &bob.token,
            json!({ "listing_id": "lst_missing" }),
        )
        .await;
    assert_eq!((s, code(&v)), (404, "not_found"), "{v}");
    let (key, stored) = new_api_key();
    srv.state
        .store
        .add_api_key(bob.account.clone(), stored, now_unix())
        .await
        .unwrap();
    let (s, v) = srv
        .post("/v1/nest-installations", &key, json!({ "listing_id": l }))
        .await;
    assert_eq!((s, code(&v)), (403, "human_required"), "{v}");

    let cfg = json!({ "team_name": "Ops", "api_token": "synthetic-secret-value-1" });
    let (s, v) = srv
        .post(
            "/v1/nest-installations",
            &bob.token,
            json!({ "listing_id": l, "configuration": cfg.clone() }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let inst = v["data"]["id"].as_str().unwrap().to_string();
    assert_eq!(v["data"]["funding"], "test_credits");
    assert_eq!(v["data"]["release_id"], b1.release.as_str());
    assert_eq!(v["data"]["configuration"], json!({ "team_name": "Ops" }));
    assert_eq!(
        v["data"]["secrets"],
        json!([{ "name": "api_token", "set": true }])
    );
    assert!(!v.to_string().contains("synthetic-secret-value-1"));
    let (s, v) = srv
        .post(
            "/v1/nest-installations",
            &bob.token,
            json!({ "listing_id": l, "configuration": cfg }),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "already_installed"), "{v}");
    // At rest: the secret is sealed in its own table, never in the configuration.
    let i2 = inst.clone();
    let (plain, sealed): (String, Vec<u8>) = srv
        .state
        .store
        .group_read(move |c| -> Result<(String, Vec<u8>), crate::groups::GErr> {
            Ok((
                c.query_row(
                    "SELECT config FROM nest_installations WHERE id = ?1",
                    [&i2],
                    |r| r.get(0),
                )?,
                c.query_row(
                    "SELECT sealed FROM nest_installation_secrets WHERE installation_id = ?1",
                    [&i2],
                    |r| r.get(0),
                )?,
            ))
        })
        .await
        .unwrap();
    assert!(!plain.contains("synthetic-secret"));
    assert!(!String::from_utf8_lossy(&sealed).contains("synthetic-secret"));

    // Someone else's installation is not there for them.
    let ipath = format!("/v1/nest-installations/{inst}");
    for (m, path, body) in [
        (Method::GET, ipath.clone(), None),
        (Method::DELETE, ipath.clone(), None),
        (
            Method::PUT,
            format!("{ipath}/configuration"),
            Some(json!({ "configuration": {} })),
        ),
        (Method::POST, format!("{ipath}/update"), None),
        (Method::POST, format!("{ipath}/rollback"), None),
    ] {
        let (s, v) = srv.call(m.clone(), &path, Some(&eve.token), body).await;
        assert_eq!((s, code(&v)), (404, "not_found"), "{m} {path}: {v}");
    }
    let (_, v) = srv.get("/v1/nest-installations", &eve.token).await;
    assert!(v["data"].as_array().unwrap().is_empty());

    // Configure: a left-out secret is kept; null clears it, and then it is missing.
    let (s, v) = srv
        .call(
            Method::PUT,
            &format!("{ipath}/configuration"),
            Some(&bob.token),
            Some(json!({ "configuration": { "team_name": "Platform", "tone": "casual" } })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["configuration"]["tone"], "casual");
    assert_eq!(v["data"]["secrets"][0]["set"], true);
    let (s, v) = srv
        .call(
            Method::PUT,
            &format!("{ipath}/configuration"),
            Some(&bob.token),
            Some(json!({ "configuration": { "team_name": "Platform", "api_token": null } })),
        )
        .await;
    assert_eq!((s, code(&v)), (400, "invalid_configuration"), "{v}");
    assert_eq!(
        issues(&v),
        [("/api_token".to_string(), "required".to_string())].into()
    );

    // A third party's listing takes jobs only through an installation; Bob's run his release with
    // his configuration (defaults applied, no secret).
    let quote = |token: String| {
        let srv = &srv;
        async move {
            srv.post("/v1/nest-jobs/quote", &token, json!({ "to": "/acme/team-digest", "operation": "team.digest.ask", "operation_version": "1.0" })).await
        }
    };
    let (s, v) = quote(eve.token.clone()).await;
    assert_eq!((s, code(&v)), (409, "installation_required"), "{v}");
    let (s, q) = quote(bob.token.clone()).await;
    assert_eq!(s, 201, "{q}");
    let qid = q["data"]["quote_id"].as_str().unwrap().to_string();
    let auth = srv
        .http
        .post(format!("{}/v1/nest-payment-authorizations", srv.public))
        .header("x-forwarded-for", &srv.ip)
        .header("idempotency-key", "bob8-auth-0000000001")
        .bearer_auth(&bob.token)
        .json(&json!({ "quote_id": qid }))
        .send()
        .await
        .unwrap();
    assert_eq!(auth.status().as_u16(), 201);
    let a: Value = auth.json().await.unwrap();
    let job = json!({
        "type": "nest.job.request", "version": 1, "to": "/acme/team-digest",
        "operation": "team.digest.ask", "operation_version": "1.0", "quote_id": qid,
        "payment_authorization": a["data"]["payment_authorization"],
        "deadline_at": crate::marketplace::iso(now_unix() as i64 + 3600),
        "payload": { "question": "What happened this week?" },
    });
    let resp = srv
        .http
        .post(format!("{}/v1/nest-jobs", srv.public))
        .header("x-forwarded-for", &srv.ip)
        .header("idempotency-key", "bob8-job-00000000001")
        .header("content-type", "application/json")
        .bearer_auth(&bob.token)
        .body(job.to_string())
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let jv: Value = resp.json().await.unwrap();
    assert_eq!(status, 202, "{jv}");
    assert_eq!(dispatch_once(&srv.state).await, 1);
    let call = srv
        .state
        .nests
        .calls()
        .into_iter()
        .find(|c| c.path == "/v1/mp/jobs")
        .unwrap();
    let body = call.body.unwrap();
    assert_eq!(body["release"]["release_id"], b1.release.as_str());
    assert_eq!(
        body["config"],
        json!({ "team_name": "Platform", "tone": "casual", "max_answers": 5, "cite_sources": true })
    );
    assert!(!body.to_string().contains("synthetic-secret"));

    // A newer attested release: offered as an update, not forced.
    let b2 = attested(&srv, &rev, &ann, &p, &t, "1.1.0", Some(schema_team()), 'b').await;
    let (s, v) = publish(
        &srv,
        &ann.strong,
        &p,
        &l,
        json!({ "release_id": b2.release }),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let (_, v) = srv.get(&ipath, &bob.token).await;
    assert_eq!(
        v["data"]["release_id"],
        b1.release.as_str(),
        "installations stay pinned"
    );
    assert_eq!(
        v["data"]["update_available"]["release_id"],
        b2.release.as_str()
    );
    let (s, v) = srv
        .post(&format!("{ipath}/rollback"), &bob.token, json!({}))
        .await;
    assert_eq!((s, code(&v)), (409, "no_previous_release"), "{v}");
    let (s, v) = srv
        .post(
            &format!("{ipath}/update"),
            &bob.token,
            json!({ "release_id": b1.release }),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "up_to_date"), "{v}");
    let (s, v) = srv
        .post(&format!("{ipath}/update"), &bob.token, json!({}))
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (
            v["data"]["release_id"].as_str(),
            v["data"]["previous_release_id"].as_str()
        ),
        (Some(b2.release.as_str()), Some(b1.release.as_str()))
    );
    assert_eq!(
        v["data"]["secrets"][0]["set"], true,
        "the secret moves with the update"
    );
    // An older release is not an update.
    let (s, v) = srv
        .post(
            &format!("{ipath}/update"),
            &bob.token,
            json!({ "release_id": b1.release }),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "not_newer"), "{v}");
    // Rollback goes back, once.
    let (s, v) = srv
        .post(&format!("{ipath}/rollback"), &bob.token, json!({}))
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["release_id"], b1.release.as_str());
    assert!(v["data"]["previous_release_id"].is_null());
    let (s, v) = srv
        .post(&format!("{ipath}/rollback"), &bob.token, json!({}))
        .await;
    assert_eq!((s, code(&v)), (409, "no_previous_release"), "{v}");
    // A quarantined target is refused (R-68: only approved, non-revoked releases).
    let (s, v) = srv
        .post(
            &format!("/v1/staff/releases/{}/security", b2.release),
            &rev.strong,
            json!({ "action": "quarantine", "reason": "synthetic" }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = srv
        .post(
            &format!("{ipath}/update"),
            &bob.token,
            json!({ "release_id": b2.release }),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "security_quarantined"), "{v}");

    // Uninstall: configuration and secrets go, the worker is removed on the host.
    let (s, v) = srv
        .call(Method::DELETE, &ipath, Some(&bob.token), None)
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (
            v["data"]["status"].as_str(),
            v["data"]["runtime_removed"].as_bool()
        ),
        (Some("uninstalled"), Some(true))
    );
    let removed = srv
        .state
        .nests
        .calls()
        .into_iter()
        .find(|c| c.path == "/v1/mp/workers/remove")
        .unwrap();
    assert_eq!(removed.body.unwrap()["listing_id"], l.as_str());
    let (s, v) = srv
        .call(Method::DELETE, &ipath, Some(&bob.token), None)
        .await;
    assert_eq!((s, code(&v)), (409, "installation_removed"), "{v}");
    let (s, v) = srv.get(&ipath, &bob.token).await;
    assert_eq!(
        (s, v["data"]["status"].as_str()),
        (200, Some("uninstalled"))
    );
    assert_eq!(v["data"]["configuration"], json!({}));
    assert_eq!(v["data"]["secrets"][0]["set"], false);
    let (s, v) = quote(bob.token.clone()).await;
    assert_eq!((s, code(&v)), (409, "installation_required"), "{v}");
    // The listing now offers the quarantined release: nothing installs it until staff lift it.
    let again = json!({ "listing_id": l, "configuration": { "team_name": "Again", "api_token": "synthetic-2" } });
    let (s, v) = srv
        .post("/v1/nest-installations", &bob.token, again.clone())
        .await;
    assert_eq!((s, code(&v)), (409, "security_quarantined"), "{v}");
    let (s, v) = srv
        .post(
            &format!("/v1/staff/releases/{}/security", b2.release),
            &rev.strong,
            json!({ "action": "lift", "reason": "synthetic" }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    // Installable again, from scratch, on the listing's current release.
    let (s, v) = srv.post("/v1/nest-installations", &bob.token, again).await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["data"]["release_id"], b2.release.as_str());
    assert!(v["data"]["previous_release_id"].is_null());
}
