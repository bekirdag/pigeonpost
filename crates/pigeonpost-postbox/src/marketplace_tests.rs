//! Marketplace foundations (M0) over real sockets: the flag, owner-only staff role assignment, the
//! staff requirement and its conflict check, publisher applications and listing visibility.
//! Synthetic accounts only.

use crate::org_tests::{serve_with, Person, Srv, OPS};
use crate::*;
use reqwest::Method;
use serde_json::{json, Value};

const OWNER_ACCOUNT: &str = "acct_mpowner";

async fn setup() -> (Srv, Person) {
    let srv = serve_with(|s| s.platform_owner = Some(Arc::from(OWNER_ACCOUNT))).await;
    let owner = srv.person("mpowner").await;
    assert_eq!(owner.account, OWNER_ACCOUNT);
    (srv, owner)
}

async fn account_flag(srv: &Srv, account: &str) {
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

async fn api_key_of(srv: &Srv, account: &str) -> String {
    let (key, stored) = new_api_key();
    srv.state
        .store
        .add_api_key(account.to_string(), stored, now_unix())
        .await
        .unwrap();
    key
}

async fn mailbox_of(srv: &Srv, person: &Person, label: &str) -> String {
    let (s, v) = srv
        .post("/v1/identities", &person.token, json!({ "label": label }))
        .await;
    assert_eq!(s, 201, "{v}");
    v["capability_token"].as_str().unwrap().to_string()
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or_default()
}

async fn assign(srv: &Srv, owner: &Person, account: &str, role: &str) -> (u16, Value) {
    srv.post(
        "/v1/staff/roles",
        &owner.strong,
        json!({ "account_id": account, "role": role }),
    )
    .await
}

async fn apply(srv: &Srv, who: &Person, name: &str) -> String {
    let (s, v) = srv
        .post(
            "/v1/publisher-applications",
            &who.token,
            json!({ "display_name": name }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    v["data"]["id"].as_str().unwrap().to_string()
}

/// Every route of the milestone answers 404 feature_disabled while the flag is off, for the
/// platform owner included.
#[tokio::test]
async fn nothing_works_while_the_flag_is_off() {
    let (srv, owner) = setup().await;
    for (method, path, body) in [
        (Method::GET, "/v1/staff/roles", None),
        (
            Method::POST,
            "/v1/staff/roles",
            Some(json!({"account_id": OWNER_ACCOUNT, "role": "reviewer"})),
        ),
        (Method::DELETE, "/v1/staff/roles/stf_x", None),
        (
            Method::POST,
            "/v1/publisher-applications",
            Some(json!({"display_name": "Nope"})),
        ),
        (Method::GET, "/v1/publisher-applications/pub_x", None),
        (Method::GET, "/v1/staff/publisher-applications/pub_x", None),
        (Method::GET, "/v1/marketplace/listings/lst_x", None),
    ] {
        let (s, v) = srv
            .call(method.clone(), path, Some(&owner.strong), body)
            .await;
        assert_eq!(
            (s, code(&v)),
            (404, "feature_disabled"),
            "{method} {path}: {v}"
        );
    }
    let (s, v) = srv
        .call(Method::GET, "/v1/marketplace/listings/lst_x", None, None)
        .await;
    assert_eq!((s, code(&v)), (404, "feature_disabled"), "{v}");
    // A flag turned on for one account does not reach another.
    let other = srv.person("mpother").await;
    account_flag(&srv, &owner.account).await;
    let (s, _) = srv.get("/v1/staff/roles", &owner.strong).await;
    assert_eq!(s, 200);
    let (s, v) = srv.get("/v1/staff/roles", &other.strong).await;
    assert_eq!((s, code(&v)), (404, "feature_disabled"), "{v}");
}

#[tokio::test]
async fn only_the_owner_with_step_up_assigns_and_lists_staff_roles() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    let reviewer = srv.person("mprev").await;
    let stranger = srv.person("mpstr").await;
    let list = "/v1/staff/roles";
    let body = || json!({ "account_id": reviewer.account, "role": "reviewer" });

    // The owner, but without a recent strong sign-in.
    let (s, v) = srv.post(list, &owner.token, body()).await;
    assert_eq!((s, code(&v)), (401, "step_up_required"), "{v}");
    let (s, v) = srv.get(list, &owner.token).await;
    assert_eq!((s, code(&v)), (401, "step_up_required"), "{v}");

    // The owner's own automation is not the owner.
    let key = api_key_of(&srv, &owner.account).await;
    let (s, v) = srv.post(list, &key, body()).await;
    assert_eq!((s, code(&v)), (403, "human_required"), "{v}");
    let cap = mailbox_of(&srv, &owner, "mp-owner-mailbox").await;
    let (s, v) = srv.get(list, &cap).await;
    assert_eq!((s, code(&v)), (403, "human_required"), "{v}");

    // A strong stranger is refused both ways and no row appears.
    let (s, v) = srv.post(list, &stranger.strong, body()).await;
    assert_eq!((s, code(&v)), (403, "forbidden"), "{v}");
    let (s, v) = srv.get(list, &stranger.strong).await;
    assert_eq!((s, code(&v)), (403, "forbidden"), "{v}");
    let (_, v) = srv.get(list, &owner.strong).await;
    assert_eq!(v["data"], json!([]), "{v}");

    // Assignment works, once, for a real account and role; extras are refused.
    let (s, v) = assign(&srv, &owner, &reviewer.account, "reviewer").await;
    assert_eq!(s, 201, "{v}");
    let role_id = v["data"]["id"].as_str().unwrap().to_string();
    assert!(role_id.starts_with("stf_"), "{v}");
    let (s, v) = assign(&srv, &owner, &reviewer.account, "reviewer").await;
    assert_eq!((s, code(&v)), (409, "role_already_held"), "{v}");
    let (s, v) = assign(&srv, &owner, "acct_nobody", "reviewer").await;
    assert_eq!((s, code(&v)), (404, "account_not_found"), "{v}");
    let (s, v) = assign(&srv, &owner, &reviewer.account, "owner").await;
    assert_eq!((s, code(&v)), (400, "invalid_role"), "{v}");
    let (s, v) = srv
        .post(
            list,
            &owner.strong,
            json!({"account_id": reviewer.account, "role": "finance", "extra": true}),
        )
        .await;
    assert_eq!((s, code(&v)), (400, "invalid_request"), "{v}");

    // A role does not make anyone able to assign roles.
    let (s, v) = srv.post(list, &reviewer.strong, body()).await;
    assert_eq!((s, code(&v)), (403, "forbidden"), "{v}");

    let (s, v) = srv.get(list, &owner.strong).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["data"][0]["account_id"], reviewer.account.as_str());
    assert!(v["next_cursor"].is_null(), "{v}");

    // The grant is on the record, with the action and the actor kind.
    let audit = srv
        .state
        .store
        .audit_for_actor(Some(owner.account.clone()), 10)
        .await
        .unwrap();
    assert_eq!(audit.len(), 1, "{audit:?}");
    assert_eq!(audit[0].action, "staff.role.grant");
    assert_eq!(audit[0].actor_kind, "human");

    // Revocation is the owner's alone too.
    let path = format!("/v1/staff/roles/{role_id}");
    let (s, v) = srv
        .call(Method::DELETE, &path, Some(&reviewer.strong), None)
        .await;
    assert_eq!((s, code(&v)), (403, "forbidden"), "{v}");
    let (s, v) = srv
        .call(Method::DELETE, &path, Some(&owner.token), None)
        .await;
    assert_eq!((s, code(&v)), (401, "step_up_required"), "{v}");
    let (s, _) = srv
        .call(Method::DELETE, &path, Some(&owner.strong), None)
        .await;
    assert_eq!(s, 204);
    let (s, v) = srv
        .call(Method::DELETE, &path, Some(&owner.strong), None)
        .await;
    assert_eq!((s, code(&v)), (404, "not_found"), "{v}");
    let (_, v) = srv.get(list, &owner.strong).await;
    assert_eq!(v["data"], json!([]), "{v}");
    // A revoked role can be granted again.
    let (s, v) = assign(&srv, &owner, &reviewer.account, "reviewer").await;
    assert_eq!(s, 201, "{v}");
}

/// With no owner configured nobody can assign a role, however strong the sign-in.
#[tokio::test]
async fn an_unconfigured_owner_means_no_one() {
    let srv = serve_with(|_| {}).await;
    srv.global_flag("marketplace").await;
    let someone = srv.person("mpnone").await;
    let (s, v) = assign(&srv, &someone, &someone.account, "reviewer").await;
    assert_eq!((s, code(&v)), (403, "forbidden"), "{v}");
}

/// The staff requirement end to end: role, then step-up, then the conflict check.
#[tokio::test]
async fn staff_read_needs_role_step_up_and_no_stake() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    let (reviewer, finance, plain, applicant) = (
        srv.person("mprev2").await,
        srv.person("mpfin2").await,
        srv.person("mpplain2").await,
        srv.person("mpappl2").await,
    );
    assert_eq!(
        assign(&srv, &owner, &reviewer.account, "reviewer").await.0,
        201
    );
    assert_eq!(
        assign(&srv, &owner, &finance.account, "finance").await.0,
        201
    );
    let application = apply(&srv, &applicant, "Applicant Ltd").await;
    let path = format!("/v1/staff/publisher-applications/{application}");

    let (s, v) = srv.get(&path, &reviewer.strong).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["id"], application.as_str());

    // Staff without step-up; a non-staff person, strong or not, is not offered step-up.
    let (s, v) = srv.get(&path, &reviewer.token).await;
    assert_eq!((s, code(&v)), (401, "step_up_required"), "{v}");
    for t in [&plain.strong, &plain.token] {
        let (s, v) = srv.get(&path, t).await;
        assert_eq!((s, code(&v)), (403, "staff_required"), "{v}");
    }
    // The wrong staff role, the applicant, and the owner, who holds no role.
    let (s, v) = srv.get(&path, &finance.strong).await;
    assert_eq!((s, code(&v)), (403, "staff_required"), "{v}");
    let (s, v) = srv.get(&path, &applicant.strong).await;
    assert_eq!((s, code(&v)), (403, "staff_required"), "{v}");
    let (s, v) = srv.get(&path, &owner.strong).await;
    assert_eq!((s, code(&v)), (403, "staff_required"), "{v}");

    // Automation holding a reviewer's account never counts as the reviewer.
    let key = api_key_of(&srv, &reviewer.account).await;
    let (s, v) = srv.get(&path, &key).await;
    assert_eq!((s, code(&v)), (403, "human_required"), "{v}");
    let cap = mailbox_of(&srv, &reviewer, "mp-reviewer-mailbox").await;
    let (s, v) = srv.get(&path, &cap).await;
    assert_eq!((s, code(&v)), (403, "human_required"), "{v}");

    // A reviewer who applied as a publisher cannot read their own application as staff.
    let own = apply(&srv, &reviewer, "Reviewer Side Project").await;
    let (s, v) = srv
        .get(
            &format!("/v1/staff/publisher-applications/{own}"),
            &reviewer.strong,
        )
        .await;
    assert_eq!((s, code(&v)), (403, "staff_conflict"), "{v}");
    // Their other work is unaffected, and a missing record is a plain 404, never a conflict.
    assert_eq!(srv.get(&path, &reviewer.strong).await.0, 200);
    let (s, v) = srv
        .get(
            "/v1/staff/publisher-applications/pub_missing",
            &reviewer.strong,
        )
        .await;
    assert_eq!((s, code(&v)), (404, "not_found"), "{v}");

    // The role is read live: revoking it ends access at once.
    let (_, list) = srv.get("/v1/staff/roles", &owner.strong).await;
    let row = list["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["account_id"] == reviewer.account.as_str())
        .unwrap()
        .clone();
    let (s, _) = srv
        .call(
            Method::DELETE,
            &format!("/v1/staff/roles/{}", row["id"].as_str().unwrap()),
            Some(&owner.strong),
            None,
        )
        .await;
    assert_eq!(s, 204);
    let (s, v) = srv.get(&path, &reviewer.strong).await;
    assert_eq!((s, code(&v)), (403, "staff_required"), "{v}");
}

#[tokio::test]
async fn publisher_applications_are_a_persons_and_stay_private() {
    let (srv, _owner) = setup().await;
    srv.global_flag("marketplace").await;
    let (ann, bob) = (srv.person("mpann").await, srv.person("mpbob").await);
    let path = "/v1/publisher-applications";

    // Automation cannot apply, even for its own account.
    let key = api_key_of(&srv, &ann.account).await;
    let (s, v) = srv
        .post(path, &key, json!({"display_name": "Bots Inc"}))
        .await;
    assert_eq!((s, code(&v)), (403, "human_required"), "{v}");
    let cap = mailbox_of(&srv, &ann, "mp-ann-mailbox").await;
    let (s, v) = srv
        .post(path, &cap, json!({"display_name": "Bots Inc"}))
        .await;
    assert_eq!((s, code(&v)), (403, "human_required"), "{v}");
    // An anonymous caller is not one.
    let (s, _) = srv
        .call(
            Method::POST,
            path,
            None,
            Some(json!({"display_name": "Nobody"})),
        )
        .await;
    assert_eq!(s, 401);

    // Validation; the client can never state terms the server decides.
    let (s, v) = srv
        .post(path, &ann.token, json!({"display_name": "x"}))
        .await;
    assert_eq!((s, code(&v)), (400, "bad_display_name"), "{v}");
    let (s, v) = srv
        .post(
            path,
            &ann.token,
            json!({"display_name": "Valid", "status": "approved"}),
        )
        .await;
    assert_eq!((s, code(&v)), (400, "invalid_request"), "{v}");

    let id = apply(&srv, &ann, "Ann's Workshop").await;
    assert!(id.starts_with("pub_"), "{id}");
    let (s, v) = srv
        .post(path, &ann.token, json!({"display_name": "Again"}))
        .await;
    assert_eq!((s, code(&v)), (409, "application_exists"), "{v}");
    assert_eq!(v["error"]["details"]["application_id"], id.as_str(), "{v}");

    let one = format!("{path}/{id}");
    let (s, v) = srv.get(&one, &ann.token).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (
            v["data"]["owner_kind"].as_str(),
            v["data"]["status"].as_str()
        ),
        (Some("account"), Some("applied"))
    );
    assert!(
        !v.to_string().contains("applied_by"),
        "internal fields stay internal: {v}"
    );
    // A stranger gets the same answer as for a missing record.
    let (s, stranger) = srv.get(&one, &bob.token).await;
    let (s2, missing) = srv.get(&format!("{path}/pub_missing"), &bob.token).await;
    assert_eq!((s, s2), (404, 404));
    assert_eq!(stranger, missing);
    // Reading is a person's too, not a key's.
    let (s, v) = srv.get(&one, &key).await;
    assert_eq!((s, code(&v)), (403, "human_required"), "{v}");
}

#[tokio::test]
async fn an_org_applies_through_its_owner_or_admin_only() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    srv.global_flag("orgs").await;
    let boss = srv.person("mpboss").await;
    let org = srv.org(&boss, "mpco", 5).await;
    let (s, v) = srv
        .post(
            &format!("/v1/orgs/{org}/me/mailbox"),
            &boss.token,
            json!({"handle": "boss"}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let (jim, out) = (srv.person("mpjim").await, srv.person("mpout").await);
    srv.join(&org, &boss, &jim, "jim", &[]).await;
    let path = "/v1/publisher-applications";
    let body = json!({ "display_name": "MPCo", "org_id": org });

    let (s, v) = srv.post(path, &jim.token, body.clone()).await;
    assert_eq!((s, code(&v)), (403, "forbidden"), "{v}");
    let (s, v) = srv.post(path, &out.token, body.clone()).await;
    assert!(s == 403 || s == 404, "an outsider is refused: {s} {v}");
    let (s, v) = srv.post(path, &boss.token, body.clone()).await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["data"]["owner_kind"], "org");
    let id = v["data"]["id"].as_str().unwrap().to_string();

    let one = format!("{path}/{id}");
    assert_eq!(srv.get(&one, &boss.token).await.0, 200);
    assert_eq!(
        srv.get(&one, &jim.token).await.0,
        404,
        "a plain member is not an admin"
    );
    assert_eq!(srv.get(&one, &out.token).await.0, 404);

    // Anyone in the org has a stake: a plain member who reviews is conflicted, an outsider is not.
    assert_eq!(assign(&srv, &owner, &jim.account, "reviewer").await.0, 201);
    assert_eq!(assign(&srv, &owner, &out.account, "reviewer").await.0, 201);
    let staff = format!("/v1/staff/publisher-applications/{id}");
    let (s, v) = srv.get(&staff, &jim.strong).await;
    assert_eq!((s, code(&v)), (403, "staff_conflict"), "{v}");
    let (s, v) = srv.get(&staff, &out.strong).await;
    assert_eq!(s, 200, "{v}");
}

async fn listing(srv: &Srv, publisher: &str, id: &str, visibility: &str, status: &str) {
    let (p, i, v, st) = (
        publisher.to_string(),
        id.to_string(),
        visibility.to_string(),
        status.to_string(),
    );
    srv.state
        .store
        .group_tx(move |tx| -> Result<(), crate::groups::GErr> {
            let n = now_unix() as i64;
            tx.execute(
                "INSERT OR IGNORE INTO nest_templates (id, publisher_id, slug, name, created_by, created_at)
                 VALUES ('tpl_' || ?1, ?1, 'demo', 'Demo', 'author', ?2)",
                rusqlite::params![p, n],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO nest_releases
                   (id, template_id, publisher_id, version, image_digest, status, created_by, created_at)
                 VALUES ('rel_' || ?1, 'tpl_' || ?1, ?1, '1.0.0', 'sha256:00', 'approved', 'author', ?2)",
                rusqlite::params![p, n],
            )?;
            tx.execute(
                "INSERT INTO nest_listings
                   (id, publisher_id, release_id, title, summary, visibility, status, created_at, updated_at)
                 VALUES (?1, ?2, 'rel_' || ?2, 'Demo', 'A demo', ?3, ?4, ?5, ?5)",
                rusqlite::params![i, p, v, st, n],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn listings_show_only_what_the_caller_may_see() {
    let (srv, _owner) = setup().await;
    srv.global_flag("marketplace").await;
    let (ann, bob) = (srv.person("mplist").await, srv.person("mplisto").await);
    let publisher = apply(&srv, &ann, "Listing Maker").await;
    for (id, visibility, status) in [
        ("lst_pub", "public", "published"),
        ("lst_unl", "unlisted", "published"),
        ("lst_pri", "private", "published"),
        ("lst_inv", "invite_only", "published"),
        ("lst_org", "organization", "published"),
        ("lst_dra", "public", "draft"),
        ("lst_sus", "public", "suspended"),
    ] {
        listing(&srv, &publisher, id, visibility, status).await;
    }
    let path = |id: &str| format!("/v1/marketplace/listings/{id}");
    let get = |path: String, token: Option<String>| {
        let srv = &srv;
        async move { srv.call(Method::GET, &path, token.as_deref(), None).await }
    };

    // Open to anyone, with or without a credential; discovery exposes no build internals.
    for id in ["lst_pub", "lst_unl"] {
        let (s, v) = get(path(id), None).await;
        assert_eq!(s, 200, "{id}: {v}");
        assert_eq!(v["data"]["id"], id);
        assert!(!v.to_string().contains("sha256"), "{v}");
        assert_eq!(get(path(id), Some(bob.token.clone())).await.0, 200);
    }
    // Hidden: a stranger cannot tell it from a missing listing; the publisher can see its own.
    let (s, missing) = get(path("lst_absent"), None).await;
    assert_eq!(s, 404);
    for id in ["lst_pri", "lst_inv", "lst_org", "lst_dra", "lst_sus"] {
        let (s, v) = get(path(id), None).await;
        assert_eq!((s, &v), (404, &missing), "{id}");
        let (s, v) = get(path(id), Some(bob.token.clone())).await;
        assert_eq!((s, code(&v)), (404, "not_found"), "{id}");
        assert_eq!(get(path(id), Some(ann.token.clone())).await.0, 200, "{id}");
    }
    // A credential that was presented has to be valid.
    let (s, _) = get(path("lst_pub"), Some("pp_not_a_real_token".into())).await;
    assert_eq!(s, 401);
}

/// Every route this module serves is a contract operation marked implemented, and the reverse,
/// and each operation names the policy action its handler really uses.
#[test]
fn the_routes_and_the_contract_match() {
    use std::collections::BTreeSet;
    let source = include_str!("marketplace.rs");
    let served: BTreeSet<(String, String)> =
        crate::foundations_tests::routes_in(source, "pub(crate) fn router(")
            .into_iter()
            .collect();
    let spec: Value = serde_json::from_str(include_str!(
        "../../../docs/nests/contracts/pigeonpost-nests.v1.json"
    ))
    .unwrap();
    let mut documented = BTreeSet::new();
    for (path, item) in spec["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            if op["x-pigeonpost-status"] != "implemented" {
                continue;
            }
            let action = op["x-pigeonpost-action"].as_str().unwrap_or_default();
            assert!(!action.is_empty(), "{method} {path} names no policy action");
            assert!(
                source.contains(&format!("Action::{action}")),
                "{method} {path}: the handlers never use Action::{action}"
            );
            documented.insert((method.clone(), path.clone()));
        }
    }
    assert!(served.len() >= 7, "parser found {served:?}");
    let missing: Vec<_> = served.difference(&documented).collect();
    let stale: Vec<_> = documented.difference(&served).collect();
    assert!(
        missing.is_empty(),
        "routes with no implemented contract operation: {missing:?}"
    );
    assert!(
        stale.is_empty(),
        "implemented contract operations with no route: {stale:?}"
    );
}
