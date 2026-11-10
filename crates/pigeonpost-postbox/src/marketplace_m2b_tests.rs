//! Marketplace M2b over real sockets: invite-only and organization visibility (R-52), org-owned
//! installations and their offboarding, source connections with sealed deploy keys, hosted test
//! Nests on a staging postbox (R-70), org webhooks for marketplace events, and the console gaps
//! (install counts, reports, staff listing list, price terms, roles by member id). Synthetic
//! accounts and synthetic values only.

use crate::marketplace_m1::buyer_context;
use crate::marketplace_m2_tests::*;
use crate::org_tests::{serve_with, Person, Srv, OPS};
use crate::*;
use reqwest::Method;
use serde_json::{json, Value};

/// A staging postbox: staging mode on, a capture test pool, `git.example.org` allowed.
async fn staging() -> (Srv, Person) {
    let srv = serve_with(|s| {
        s.platform_owner = Some(Arc::from(OWNER_ACCOUNT));
        let mut rt = nests::Runtime::capture();
        rt.mp.git_hosts.push("git.example.org".into());
        rt.mp.staging = true;
        rt.mp.test_pool = Some(Arc::new(nests::Runtime::capture()));
        s.nests = Arc::new(rt);
    })
    .await;
    let owner = srv.person("mpowner").await;
    (srv, owner)
}

/// A published listing of `vis` for a certified personal publisher.
async fn published(
    srv: &Srv,
    owner: &Person,
    tag: &str,
    vis: &str,
) -> (Person, Person, String, String) {
    let rev = reviewer(srv, owner, &format!("mbrev{tag}")).await;
    let ann = srv.person(&format!("mbann{tag}")).await;
    let p = certified(srv, &rev, &ann).await;
    let t = template(srv, &ann, &p, "team-digest").await;
    let b = attested(srv, &rev, &ann, &p, &t, "1.0.0", None, 'a').await;
    let l = listing(srv, &ann, &p, &b.release, vis).await;
    let (s, v) = publish(srv, &ann.strong, &p, &l, json!({})).await;
    assert_eq!(s, 200, "{v}");
    (rev, ann, p, l)
}

/// `POST /v1/nest-jobs/quote`, optionally through an org installation.
async fn quote(srv: &Srv, token: &str, installation: Option<&str>) -> (u16, Value) {
    let mut req = srv
        .http
        .post(format!("{}/v1/nest-jobs/quote", srv.public))
        .header("x-forwarded-for", &srv.ip)
        .bearer_auth(token)
        .json(
            &json!({ "to": "/acme/team-digest", "operation": "team.digest.ask",
                       "operation_version": "1.0" }),
        );
    if let Some(i) = installation {
        req = req.header("x-pigeonpost-installation", i);
    }
    let r = req.send().await.unwrap();
    let s = r.status().as_u16();
    (s, r.json().await.unwrap_or(Value::Null))
}

/// Quote, authorize and submit one job; the job response.
async fn run_job(srv: &Srv, token: &str, installation: Option<&str>, key: &str) -> (u16, Value) {
    let (s, q) = quote(srv, token, installation).await;
    assert_eq!(s, 201, "{q}");
    let qid = q["data"]["quote_id"].as_str().unwrap().to_string();
    let auth = srv
        .http
        .post(format!("{}/v1/nest-payment-authorizations", srv.public))
        .header("x-forwarded-for", &srv.ip)
        .header("idempotency-key", format!("{key}-auth-000000001"))
        .bearer_auth(token)
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
    let mut req = srv
        .http
        .post(format!("{}/v1/nest-jobs", srv.public))
        .header("x-forwarded-for", &srv.ip)
        .header("idempotency-key", format!("{key}-job-0000000001"))
        .header("content-type", "application/json")
        .bearer_auth(token)
        .body(job.to_string());
    if let Some(i) = installation {
        req = req.header("x-pigeonpost-installation", i);
    }
    let r = req.send().await.unwrap();
    let s = r.status().as_u16();
    (s, r.json().await.unwrap_or(Value::Null))
}

/// A personal mailbox for `who`; its address.
async fn mailbox(srv: &Srv, who: &Person) -> String {
    let (s, v) = srv
        .post("/v1/identities", &who.token, json!({ "label": "inbox" }))
        .await;
    assert_eq!(s, 201, "{v}");
    v["address"].as_str().unwrap().to_string()
}

fn ids(v: &Value) -> Vec<String> {
    v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["id"].as_str().unwrap().to_string())
        .collect()
}

async fn sql_i64(srv: &Srv, sql: &'static str, arg: String) -> i64 {
    srv.state
        .store
        .group_read(move |c| -> Result<i64, crate::groups::GErr> {
            Ok(c.query_row(sql, [&arg], |r| r.get(0))?)
        })
        .await
        .unwrap()
}

// ---- visibility grants (R-52) -----------------------------------------------------------------------

#[tokio::test]
async fn invite_grants_decide_discovery_install_and_invocation_separately() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    let (_rev, ann, p, l) = published(&srv, &owner, "1", "invite_only").await;
    let (bob, eve) = (srv.person("mbbob1").await, srv.person("mbeve1").await);
    let (bob_mb, eve_mb) = (mailbox(&srv, &bob).await, mailbox(&srv, &eve).await);
    let gpath = format!("/v1/publishers/{p}/listings/{l}/grants");

    // Not in the catalog, not readable, not installable, before any grant.
    let (_, cat) = srv.get("/v1/marketplace/listings", &bob.token).await;
    assert!(!ids(&cat).contains(&l), "{cat}");
    let (s, _) = srv
        .get(&format!("/v1/marketplace/listings/{l}"), &bob.token)
        .await;
    assert_eq!(s, 404);
    let (s, v) = srv
        .post(
            "/v1/nest-installations",
            &bob.token,
            json!({ "listing_id": l }),
        )
        .await;
    assert_eq!((s, code(&v)), (404, "not_found"), "{v}");

    // Only the team manages grants; an org grant does not fit an invite-only listing.
    let (s, _) = srv.get(&gpath, &eve.token).await;
    assert_eq!(s, 404);
    let (s, v) = srv
        .post(
            &gpath,
            &ann.token,
            json!({ "kind": "organization", "org_id": "org_x" }),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "grant_kind_mismatch"), "{v}");
    // An invite to an address that does not exist looks the same as one that does.
    let (s, ghost) = srv
        .post(
            &gpath,
            &ann.token,
            json!({ "kind": "invite", "address": "/pp/nobody-here" }),
        )
        .await;
    assert_eq!(
        (s, ghost["data"]["status"].as_str()),
        (201, Some("pending")),
        "{ghost}"
    );
    let (s, inv) = srv
        .post(
            &gpath,
            &ann.token,
            json!({ "kind": "invite", "address": bob_mb }),
        )
        .await;
    assert_eq!(s, 201, "{inv}");
    let g = inv["data"]["id"].as_str().unwrap().to_string();
    let (s, v) = srv
        .post(
            &gpath,
            &ann.token,
            json!({ "kind": "invite", "address": bob_mb }),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "grant_exists"), "{v}");
    assert!(events_of(&srv, &bob.account)
        .await
        .iter()
        .any(|(k, d)| k == "nest.invite.received" && d["grant_id"] == g.as_str()));

    // Pending: still nothing. Eve cannot accept Bob's invite.
    let (_, cat) = srv.get("/v1/marketplace/listings", &bob.token).await;
    assert!(!ids(&cat).contains(&l));
    let (s, mine) = srv.get("/v1/marketplace/invites", &bob.token).await;
    assert_eq!(
        (s, mine["data"][0]["id"].as_str()),
        (200, Some(g.as_str())),
        "{mine}"
    );
    let (s, _) = srv
        .post(
            &format!("/v1/marketplace/invites/{g}/accept"),
            &eve.token,
            json!({}),
        )
        .await;
    assert_eq!(s, 404);
    let (s, v) = srv
        .post(
            &format!("/v1/marketplace/invites/{g}/accept"),
            &bob.token,
            json!({}),
        )
        .await;
    assert_eq!(
        (s, v["data"]["status"].as_str()),
        (200, Some("active")),
        "{v}"
    );

    // Accepted: discover, install, invoke.
    let (_, cat) = srv.get("/v1/marketplace/listings", &bob.token).await;
    assert!(ids(&cat).contains(&l), "{cat}");
    let (_, cat) = srv.get("/v1/marketplace/listings", &eve.token).await;
    assert!(!ids(&cat).contains(&l));
    let (s, _) = srv
        .get(&format!("/v1/marketplace/listings/{l}"), &bob.token)
        .await;
    assert_eq!(s, 200);
    let (s, v) = srv
        .post(
            "/v1/nest-installations",
            &bob.token,
            json!({ "listing_id": l }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let (s, v) = quote(&srv, &bob.token, None).await;
    assert_eq!(s, 201, "{v}");

    // Revoked: invocation stops at once, the installation stays readable.
    let (s, v) = srv
        .call(
            Method::DELETE,
            &format!("{gpath}/{g}"),
            Some(&ann.token),
            None,
        )
        .await;
    assert_eq!(
        (s, v["data"]["status"].as_str()),
        (200, Some("revoked")),
        "{v}"
    );
    let (s, v) = quote(&srv, &bob.token, None).await;
    assert_eq!((s, code(&v)), (403, "grant_required"), "{v}");
    let (_, cat) = srv.get("/v1/marketplace/listings", &bob.token).await;
    assert!(!ids(&cat).contains(&l));

    // Each switch on its own: discover only shows it but refuses the install.
    let (s, inv) = srv
        .post(
            &gpath,
            &ann.token,
            json!({ "kind": "invite", "address": eve_mb, "install": false, "invoke": false }),
        )
        .await;
    assert_eq!(s, 201, "{inv}");
    let g2 = inv["data"]["id"].as_str().unwrap().to_string();
    srv.post(
        &format!("/v1/marketplace/invites/{g2}/accept"),
        &eve.token,
        json!({}),
    )
    .await;
    let (_, cat) = srv.get("/v1/marketplace/listings", &eve.token).await;
    assert!(ids(&cat).contains(&l));
    let (s, v) = srv
        .post(
            "/v1/nest-installations",
            &eve.token,
            json!({ "listing_id": l }),
        )
        .await;
    assert_eq!((s, code(&v)), (403, "grant_required"), "{v}");
    let (s, v) = srv
        .post(
            &gpath,
            &ann.token,
            json!({ "kind": "invite", "address": bob_mb,
                                           "discover": false, "install": false, "invoke": false }),
        )
        .await;
    assert_eq!((s, code(&v)), (400, "invalid_request"), "{v}");
    // Declining closes an invite.
    let (_, inv) = srv
        .post(
            &gpath,
            &ann.token,
            json!({ "kind": "invite", "address": bob_mb }),
        )
        .await;
    let g3 = inv["data"]["id"].as_str().unwrap().to_string();
    let (s, v) = srv
        .post(
            &format!("/v1/marketplace/invites/{g3}/decline"),
            &bob.token,
            json!({}),
        )
        .await;
    assert_eq!(
        (s, v["data"]["status"].as_str()),
        (200, Some("declined")),
        "{v}"
    );
    let (s, v) = srv
        .post(
            &format!("/v1/marketplace/invites/{g3}/accept"),
            &bob.token,
            json!({}),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "grant_not_live"), "{v}");
}

#[tokio::test]
async fn organization_grants_follow_live_membership() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    srv.global_flag("orgs").await;
    let (_rev, ann, p, l) = published(&srv, &owner, "2", "organization").await;
    let boss = srv.person("mbboss2").await;
    let org = srv.org(&boss, "buyerco2", 5).await;
    let jane = srv.person("mbjane2").await;
    let (jid, _) = srv.join(&org, &boss, &jane, "jane2", &[]).await;
    let outsider = srv.person("mbout2").await;
    let gpath = format!("/v1/publishers/{p}/listings/{l}/grants");
    let (s, v) = srv
        .post(
            &gpath,
            &ann.token,
            json!({ "kind": "invite", "address": "/pp/jane2" }),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "grant_kind_mismatch"), "{v}");
    let (s, v) = srv
        .post(
            &gpath,
            &ann.token,
            json!({ "kind": "organization", "org_id": "org_missing" }),
        )
        .await;
    assert_eq!(s, 404, "{v}");
    // Members may discover and install, not invoke.
    let (s, v) = srv
        .post(
            &gpath,
            &ann.token,
            json!({ "kind": "organization", "org_id": org, "invoke": false }),
        )
        .await;
    assert_eq!(
        (s, v["data"]["status"].as_str()),
        (201, Some("active")),
        "{v}"
    );
    let (_, cat) = srv.get("/v1/marketplace/listings", &jane.token).await;
    assert!(ids(&cat).contains(&l), "{cat}");
    let (_, cat) = srv.get("/v1/marketplace/listings", &outsider.token).await;
    assert!(!ids(&cat).contains(&l));
    let (s, v) = srv
        .post(
            "/v1/nest-installations",
            &jane.token,
            json!({ "listing_id": l }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let (s, v) = quote(&srv, &jane.token, None).await;
    assert_eq!((s, code(&v)), (403, "grant_required"), "{v}");
    // Offboarded: the org's grant no longer covers her.
    let (s, v) = srv
        .post(
            &format!("/v1/orgs/{org}/members/{jid}/remove"),
            &boss.token,
            json!({ "mode": "reserve" }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (_, cat) = srv.get("/v1/marketplace/listings", &jane.token).await;
    assert!(!ids(&cat).contains(&l));
    let (s, _) = srv
        .get(&format!("/v1/marketplace/listings/{l}"), &jane.token)
        .await;
    assert_eq!(s, 404);
}

// ---- org installations ----------------------------------------------------------------------------

#[tokio::test]
async fn org_installations_follow_admins_and_operators_and_end_with_offboarding() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    srv.global_flag("orgs").await;
    let (_rev, _ann, _p, l) = published(&srv, &owner, "3", "public").await;
    let boss = srv.person("mbboss3").await;
    let org = srv.org(&boss, "buyerco3", 5).await;
    let (jane, joe) = (srv.person("mbjane3").await, srv.person("mbjoe3").await);
    let (jid, _) = srv.join(&org, &boss, &jane, "jane3", &[]).await;
    let (_joe_id, _) = srv.join(&org, &boss, &joe, "joe3", &[]).await;
    let ipath = format!("/v1/orgs/{org}/nest-installations");

    // Only Nest admins install for the org.
    let (s, _) = srv
        .post(
            &ipath,
            &jane.token,
            json!({ "listing_id": l, "handle": "digest" }),
        )
        .await;
    assert_eq!(s, 403);
    let (s, v) = srv
        .post(
            &ipath,
            &boss.token,
            json!({ "listing_id": "lst_missing", "handle": "digest" }),
        )
        .await;
    assert_eq!((s, code(&v)), (404, "not_found"), "{v}");
    let (s, v) = srv
        .post(
            &ipath,
            &boss.token,
            json!({ "listing_id": l, "handle": "digest", "operators": [jid] }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["data"]["owner"]["kind"], "org");
    let inst = v["data"]["id"].as_str().unwrap().to_string();
    let agent = v["data"]["agent_address"].as_str().unwrap().to_string();
    let (s, v) = srv
        .post(
            &ipath,
            &boss.token,
            json!({ "listing_id": l, "handle": "digest2" }),
        )
        .await;
    assert_eq!((s, code(&v)), (409, "already_installed"), "{v}");
    // It holds an org agent address.
    let (_, agents) = srv
        .get(&format!("/v1/orgs/{org}/agents"), &boss.token)
        .await;
    assert!(agents.to_string().contains(&agent), "{agents}");

    // Jane operates it: she reads it and invokes it; she cannot manage it. Joe sees nothing.
    let one = format!("/v1/nest-installations/{inst}");
    assert_eq!(srv.get(&one, &jane.token).await.0, 200);
    assert_eq!(srv.get(&one, &joe.token).await.0, 404);
    let (s, v) = srv
        .call(
            Method::PUT,
            &format!("{one}/configuration"),
            Some(&jane.token),
            Some(json!({ "configuration": {} })),
        )
        .await;
    assert_eq!((s, code(&v)), (403, "nest_admin_required"), "{v}");
    let (s, v) = srv
        .call(Method::DELETE, &one, Some(&jane.token), None)
        .await;
    assert_eq!((s, code(&v)), (403, "nest_admin_required"), "{v}");
    let (_, list) = srv.get(&ipath, &jane.token).await;
    assert_eq!(list["data"][0]["role"], "operator", "{list}");
    let (_, list) = srv.get(&ipath, &joe.token).await;
    assert!(list["data"].as_array().unwrap().is_empty());
    let (_, list) = srv.get("/v1/nest-installations", &jane.token).await;
    assert!(
        list["data"].as_array().unwrap().is_empty(),
        "account list is account-only"
    );

    // Without the header the org installation is not hers; Joe cannot use it.
    let (s, v) = quote(&srv, &jane.token, None).await;
    assert_eq!((s, code(&v)), (409, "installation_required"), "{v}");
    let (s, _) = quote(&srv, &joe.token, Some(&inst)).await;
    assert_eq!(s, 404);
    let (s, v) = run_job(&srv, &jane.token, Some(&inst), "jane3").await;
    assert_eq!(s, 202, "{v}");
    assert_eq!(crate::marketplace_m1::dispatch_once(&srv.state).await, 1);
    let call = srv
        .state
        .nests
        .calls()
        .into_iter()
        .find(|c| c.path == "/v1/mp/jobs")
        .unwrap();
    let body = call.body.unwrap();
    assert_eq!(
        body["buyer_context"],
        buyer_context(&l, &format!("org:{org}")).as_str()
    );
    assert_eq!(body["config"], json!({}));

    // Taking the delegation ends her use; giving it back restores it.
    let op = format!("{ipath}/{inst}/operators/{jid}");
    assert_eq!(
        srv.call(Method::PUT, &op, Some(&jane.token), None).await.0,
        403
    );
    let (s, v) = srv.call(Method::DELETE, &op, Some(&boss.token), None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(quote(&srv, &jane.token, Some(&inst)).await.0, 404);
    assert_eq!(srv.get(&one, &jane.token).await.0, 404);
    let (s, v) = srv.call(Method::PUT, &op, Some(&boss.token), None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(quote(&srv, &jane.token, Some(&inst)).await.0, 201);

    // Offboarding ends it everywhere at once.
    let (s, v) = srv
        .post(
            &format!("/v1/orgs/{org}/members/{jid}/remove"),
            &boss.token,
            json!({ "mode": "reserve" }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(quote(&srv, &jane.token, Some(&inst)).await.0, 404);
    assert_eq!(srv.get(&one, &jane.token).await.0, 404);

    // The admin configures and uninstalls; the agent address and its delegations go with it.
    let (s, v) = srv
        .call(
            Method::PUT,
            &format!("{one}/configuration"),
            Some(&boss.token),
            Some(json!({ "configuration": {} })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, v) = srv
        .call(Method::DELETE, &one, Some(&boss.token), None)
        .await;
    assert_eq!(
        (s, v["data"]["status"].as_str()),
        (200, Some("uninstalled")),
        "{v}"
    );
    assert_eq!(v["data"]["jobs_cancelled"], 1, "{v}");
    let status = sql_i64(
        &srv,
        "SELECT COUNT(*) FROM org_agents WHERE identity = ?1 AND status = 'deleted'",
        agent.clone(),
    )
    .await;
    assert_eq!(status, 1);
    let live = sql_i64(
        &srv,
        "SELECT COUNT(*) FROM org_delegations WHERE target_identity = ?1 AND revoked_at IS NULL",
        agent,
    )
    .await;
    assert_eq!(live, 0);
}

// ---- source connections -------------------------------------------------------------------------

#[tokio::test]
async fn source_connections_seal_deploy_keys_and_feed_builds() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    let rev = reviewer(&srv, &owner, "mbrev4").await;
    let ann = srv.person("mbann4").await;
    let eve = srv.person("mbeve4").await;
    let p = certified(&srv, &rev, &ann).await;
    let t = template(&srv, &ann, &p, "team-digest").await;
    let spath = format!("/v1/publishers/{p}/sources");

    // Hosts off the allowlist are refused, inline and registered.
    let (s, v) = srv
        .post(
            &spath,
            &ann.token,
            json!({ "kind": "https", "repository": "https://evil.example.net/a/b", "label": "x" }),
        )
        .await;
    assert_eq!((s, code(&v)), (403, "git_host_not_allowed"), "{v}");
    let (s, v) = srv
        .post(
            &format!("/v1/publishers/{p}/builds"),
            &ann.token,
            json!({
            "template_id": t, "version": "0.9.0",
            "source": { "repository": "https://evil.example.net/a/b", "commit": "a".repeat(40) } }),
        )
        .await;
    assert_eq!((s, code(&v)), (403, "git_host_not_allowed"), "{v}");
    let (s, v) = srv
        .post(&spath, &ann.token, json!({ "kind": "deploy_key", "repository": "git@git.example.org:-oProxyCommand=x", "label": "x" }))
        .await;
    assert_eq!((s, code(&v)), (400, "invalid_request"), "{v}");
    let (s, _) = srv
        .post(
            &spath,
            &eve.token,
            json!({ "kind": "https", "repository": "https://git.example.org/a/b", "label": "x" }),
        )
        .await;
    assert_eq!(s, 404);

    // A deploy key: the public half comes back, the private half never does.
    let (s, v) = srv
        .post(&spath, &ann.token, json!({ "kind": "deploy_key", "repository": "git@git.example.org:acme/team-digest.git", "label": "private repo" }))
        .await;
    assert_eq!(s, 201, "{v}");
    let src = v["data"]["id"].as_str().unwrap().to_string();
    let public = v["data"]["deploy_key"]["public_key"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(public.starts_with("ssh-ed25519 "), "{v}");
    assert!(!v.to_string().contains("PRIVATE KEY"));
    let (_, list) = srv.get(&spath, &ann.token).await;
    assert!(!list.to_string().contains("PRIVATE KEY") && !list.to_string().contains("sealed"));
    let at_rest = srv
        .state
        .store
        .group_read({
            let src = src.clone();
            move |c| -> Result<Vec<u8>, crate::groups::GErr> {
                Ok(c.query_row(
                    "SELECT private_key_sealed FROM publisher_sources WHERE id = ?1",
                    [&src],
                    |r| r.get(0),
                )?)
            }
        })
        .await
        .unwrap();
    assert!(!String::from_utf8_lossy(&at_rest).contains("OPENSSH"));

    // A build from the source; the build plane fetches the key for it, over the internal API.
    let (s, v) = srv
        .post(&format!("/v1/publishers/{p}/builds"), &ann.token, json!({
            "template_id": t, "version": "1.0.0", "source": { "source_id": src, "commit": "c".repeat(40) } }))
        .await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["data"]["source"]["source_id"], src.as_str());
    let bid = v["data"]["id"].as_str().unwrap().to_string();
    let (s, v) = srv
        .post(
            &format!("/v1/publishers/{p}/builds"),
            &ann.token,
            json!({
            "template_id": t, "version": "1.0.1", "source": { "source_id": src,
            "repository": "https://git.example.org/a/b", "commit": "c".repeat(40) } }),
        )
        .await;
    assert_eq!((s, code(&v)), (400, "invalid_request"), "{v}");
    let (_, pending) = srv
        .call(
            Method::GET,
            "/internal/v1/marketplace/build-requests",
            Some(OPS),
            None,
        )
        .await;
    assert_eq!(
        pending["data"][0]["source"]["kind"], "deploy_key",
        "{pending}"
    );
    let cred = format!("/internal/v1/marketplace/build-requests/{bid}/source-credential");
    assert_eq!(srv.call(Method::GET, &cred, None, None).await.0, 401);
    let (s, v) = srv.call(Method::GET, &cred, Some(OPS), None).await;
    assert_eq!(s, 200, "{v}");
    let pem = v["data"]["private_key"].as_str().unwrap();
    let key = ssh_key::PrivateKey::from_openssh(pem).unwrap();
    assert_eq!(
        key.public_key().to_openssh().unwrap().split(' ').nth(1),
        public.split(' ').nth(1)
    );

    // Deleting the source wipes the key and fails its queued build.
    let (s, v) = srv
        .call(
            Method::DELETE,
            &format!("{spath}/{src}"),
            Some(&ann.token),
            None,
        )
        .await;
    assert_eq!(
        (s, v["data"]["status"].as_str()),
        (200, Some("deleted")),
        "{v}"
    );
    assert_eq!(srv.call(Method::GET, &cred, Some(OPS), None).await.0, 404);
    let (_, b) = srv
        .get(&format!("/v1/publishers/{p}/builds/{bid}"), &ann.token)
        .await;
    assert_eq!(b["data"]["failure"]["code"], "source_deleted", "{b}");
    let left = sql_i64(
        &srv,
        "SELECT COUNT(*) FROM publisher_sources WHERE id = ?1 AND private_key_sealed IS NOT NULL",
        src,
    )
    .await;
    assert_eq!(left, 0);
}

// ---- staging mode and hosted test Nests (R-70) ------------------------------------------------------

#[tokio::test]
async fn test_nests_and_the_staging_marks_exist_only_on_a_staging_postbox() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    let rev = reviewer(&srv, &owner, "mbrev5").await;
    let ann = srv.person("mbann5").await;
    let p = certified(&srv, &rev, &ann).await;
    let (s, v) = srv
        .get(&format!("/v1/publishers/{p}/test-nests"), &ann.token)
        .await;
    assert_eq!((s, code(&v)), (404, "staging_only"), "{v}");
    let r = srv
        .http
        .get(format!("{}/health", srv.public))
        .send()
        .await
        .unwrap();
    assert!(r.headers().get("x-pigeonpost-environment").is_none());

    let (srv, _owner) = staging().await;
    let r = srv
        .http
        .get(format!("{}/health", srv.public))
        .send()
        .await
        .unwrap();
    assert_eq!(r.headers()["x-pigeonpost-environment"], "staging");
    let bob = srv.person("mbbob5").await;
    let (s, v) = srv.post("/v1/claims/plan", &bob.token, json!({})).await;
    assert_eq!((s, code(&v)), (403, "staging_no_payments"), "{v}");
    let (s, v) = srv
        .post("/v1/nest-purchases/intents", &bob.token, json!({}))
        .await;
    assert_eq!((s, code(&v)), (403, "staging_no_payments"), "{v}");
    let (s, v) = srv
        .call(
            Method::PUT,
            "/internal/v1/accounts/acct_x/cloud-plan",
            Some(OPS),
            Some(json!({})),
        )
        .await;
    assert_eq!((s, code(&v)), (403, "staging_no_payments"), "{v}");
    let r = srv
        .http
        .get(format!("{}/internal/v1/health", srv.internal))
        .send()
        .await
        .unwrap();
    assert_eq!(r.headers()["x-pigeonpost-environment"], "staging");
}

#[tokio::test]
async fn test_nests_run_intake_passed_releases_in_the_test_pool_until_they_expire() {
    let (srv, owner) = staging().await;
    srv.global_flag("marketplace").await;
    let rev = reviewer(&srv, &owner, "mbrev6").await;
    let ann = srv.person("mbann6").await;
    let eve = srv.person("mbeve6").await;
    let p = certified(&srv, &rev, &ann).await;
    let t = template(&srv, &ann, &p, "team-digest").await;
    let tpath = format!("/v1/publishers/{p}/test-nests");
    // Passed intake only: not approved, still testable.
    let b = build(&srv, &ann, &p, &t, "1.0.0", None, 'a').await;
    // A release whose scan failed is not.
    let bad = build(&srv, &ann, &p, &t, "1.0.1", None, 'b').await;
    let (s, v) = stage(&srv, &bad.review, "deterministic_scan", "failed").await;
    assert!(s == 200 || s == 201, "{v}");

    let (s, _) = srv
        .post(&tpath, &eve.token, json!({ "release_id": b.release }))
        .await;
    assert_eq!(s, 404);
    let (s, v) = srv
        .post(&tpath, &ann.token, json!({ "release_id": bad.release }))
        .await;
    assert_eq!((s, code(&v)), (409, "release_not_testable"), "{v}");
    let (s, v) = srv
        .post(
            &tpath,
            &ann.token,
            json!({ "release_id": b.release, "days": 30 }),
        )
        .await;
    assert_eq!((s, code(&v)), (400, "invalid_request"), "{v}");
    let (s, v) = srv
        .post(&tpath, &ann.token, json!({ "release_id": "rel_missing" }))
        .await;
    assert_eq!(s, 404, "{v}");
    let bid = sql_text(
        &srv,
        "SELECT build_id FROM review_cases WHERE id = ?1",
        b.review.clone(),
    )
    .await;
    let (s, v) = srv
        .post(
            &tpath,
            &ann.token,
            json!({ "build_id": bid, "label": "smoke" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(
        (
            v["data"]["status"].as_str(),
            v["data"]["environment"].as_str()
        ),
        (Some("active"), Some("staging"))
    );
    let tn = v["data"]["id"].as_str().unwrap().to_string();
    let one = format!("{tpath}/{tn}");

    // A synthetic job: the operation's schema is enforced; it goes to the test pool only, with a
    // test statement, never to the production pool.
    let (s, v) = srv
        .post(&format!("{one}/jobs"), &ann.token, json!({ "operation": "team.digest.ask", "operation_version": "1.0", "payload": { "nope": "x" } }))
        .await;
    assert_eq!(s, 400, "{v}");
    let (s, v) = srv
        .post(&format!("{one}/jobs"), &ann.token, json!({ "operation": "team.digest.ask", "operation_version": "1.0", "payload": { "question": "synthetic?" } }))
        .await;
    assert_eq!(
        (s, v["data"]["state"].as_str()),
        (202, Some("running")),
        "{v}"
    );
    let job = v["data"]["id"].as_str().unwrap().to_string();
    assert!(srv
        .state
        .nests
        .calls()
        .iter()
        .all(|c| c.path != "/v1/mp/jobs"));
    let pool = srv.state.nests.mp.test_pool.clone().unwrap();
    let sent = pool
        .calls()
        .into_iter()
        .find(|c| c.path == "/v1/mp/jobs")
        .unwrap()
        .body
        .unwrap();
    let statement: Value =
        serde_json::from_str(sent["release"]["statement"].as_str().unwrap()).unwrap();
    assert_eq!(statement["type"], "pigeonpost.nest.release.test.v1");
    assert_eq!(statement["decision"], "test_only");
    assert_eq!(sent["listing_id"], tn.as_str());
    let first_context = sent["buyer_context"].as_str().unwrap().to_string();
    let (s, v) = srv.get(&format!("{one}/jobs/{job}"), &ann.token).await;
    assert_eq!(
        (s, v["data"]["state"].as_str()),
        (200, Some("succeeded")),
        "{v}"
    );
    assert_eq!(v["data"]["result"]["answer"], "captured");
    let (s, _) = srv.get(&format!("{one}/jobs/{job}"), &eve.token).await;
    assert_eq!(s, 404);

    // Reset: a fresh worker for the next job, the old one removed.
    let (s, v) = srv
        .post(&format!("{one}/reset"), &ann.token, json!({}))
        .await;
    assert_eq!((s, v["data"]["generation"].as_i64()), (200, Some(2)), "{v}");
    assert!(pool
        .calls()
        .iter()
        .any(|c| c.path == "/v1/mp/workers/destroy"
            && c.body.as_ref().unwrap()["buyer_context"] == first_context.as_str()));
    srv.post(&format!("{one}/jobs"), &ann.token, json!({ "operation": "team.digest.ask", "operation_version": "1.0", "payload": { "question": "again?" } }))
        .await;
    let second = pool
        .calls()
        .into_iter()
        .rev()
        .find(|c| c.path == "/v1/mp/jobs")
        .unwrap()
        .body
        .unwrap();
    assert_ne!(second["buyer_context"], first_context.as_str());
    let (s, logs) = srv.get(&format!("{one}/logs"), &ann.token).await;
    assert_eq!(s, 200, "{logs}");
    let kinds: Vec<&str> = logs["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds.first(), Some(&"created"));
    assert!(
        kinds.contains(&"reset") && kinds.contains(&"job"),
        "{kinds:?}"
    );

    // Expiry: a warning a day ahead, then the end, the worker removed and no more jobs.
    let now = now_unix() as i64;
    set_expiry(&srv, &tn, now + 3_600).await;
    assert_eq!(
        crate::marketplace_test_nests::sweep_once(&srv.state, now).await,
        0
    );
    assert!(events_of(&srv, &ann.account)
        .await
        .iter()
        .any(|(k, d)| k == "nest.test.expiring" && d["test_nest_id"] == tn.as_str()));
    let removed_before = pool
        .calls()
        .iter()
        .filter(|c| c.path == "/v1/mp/workers/destroy")
        .count();
    assert_eq!(
        crate::marketplace_test_nests::sweep_once(&srv.state, now + 7_200).await,
        1
    );
    assert_eq!(
        pool.calls()
            .iter()
            .filter(|c| c.path == "/v1/mp/workers/destroy")
            .count(),
        removed_before + 1
    );
    assert!(events_of(&srv, &ann.account)
        .await
        .iter()
        .any(|(k, _)| k == "nest.test.expired"));
    let (_, v) = srv.get(&one, &ann.token).await;
    assert_eq!(v["data"]["status"], "expired");
    let (s, v) = srv
        .post(&format!("{one}/jobs"), &ann.token, json!({ "operation": "team.digest.ask", "operation_version": "1.0", "payload": { "question": "late?" } }))
        .await;
    assert_eq!((s, code(&v)), (409, "test_nest_ended"), "{v}");

    // Delete another one.
    let (_, v) = srv
        .post(&tpath, &ann.token, json!({ "release_id": b.release }))
        .await;
    let tn2 = v["data"]["id"].as_str().unwrap().to_string();
    let (s, _) = srv
        .call(
            Method::DELETE,
            &format!("{tpath}/{tn2}"),
            Some(&eve.token),
            None,
        )
        .await;
    assert_eq!(s, 404);
    let (s, v) = srv
        .call(
            Method::DELETE,
            &format!("{tpath}/{tn2}"),
            Some(&ann.token),
            None,
        )
        .await;
    assert_eq!(
        (s, v["data"]["status"].as_str()),
        (200, Some("deleted")),
        "{v}"
    );
    let (s, v) = srv
        .call(
            Method::DELETE,
            &format!("{tpath}/{tn2}"),
            Some(&ann.token),
            None,
        )
        .await;
    assert_eq!((s, code(&v)), (409, "test_nest_ended"), "{v}");
}

async fn sql_text(srv: &Srv, sql: &'static str, arg: String) -> String {
    srv.state
        .store
        .group_read(move |c| -> Result<String, crate::groups::GErr> {
            Ok(c.query_row(sql, [&arg], |r| r.get(0))?)
        })
        .await
        .unwrap()
}

async fn set_expiry(srv: &Srv, tn: &str, at: i64) {
    let tn = tn.to_string();
    srv.state
        .store
        .group_tx(move |tx| -> Result<(), crate::groups::GErr> {
            tx.execute(
                "UPDATE nest_test_envs SET expires_at = ?2 WHERE id = ?1",
                rusqlite::params![tn, at],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

// ---- org webhooks -----------------------------------------------------------------------------

async fn hook(srv: &Srv, org: &str) {
    let org = org.to_string();
    srv.state
        .store
        .group_tx(move |tx| -> Result<(), crate::groups::GErr> {
            tx.execute(
                "INSERT INTO webhooks (id, org_id, url, events, secret_sealed, created_at)
                 VALUES ('whk_test', ?1, 'https://hooks.example.org/x', '[\"*\"]', x'00', 0)",
                [&org],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

async fn hook_events(srv: &Srv, org: &str) -> Vec<(String, Value)> {
    let org = org.to_string();
    srv.state
        .store
        .group_read(
            move |c| -> Result<Vec<(String, Value)>, crate::groups::GErr> {
                let mut stmt = c.prepare(
                    "SELECT type, data FROM webhook_events WHERE org_id = ?1 ORDER BY rowid",
                )?;
                let rows = stmt
                    .query_map([&org], |r| {
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

#[tokio::test]
async fn org_webhooks_hear_publisher_and_installation_events_by_id_only() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    srv.global_flag("orgs").await;
    for e in [
        "nest.listing.updated",
        "nest.installation.changed",
        "nest.build.failed",
        "nest.test.expiring",
    ] {
        assert!(crate::webhooks::EVENTS.contains(&e), "{e}");
    }
    // An org publisher's listing events reach its webhook.
    let rev = reviewer(&srv, &owner, "mbrev7").await;
    let boss = srv.person("mbboss7").await;
    let org = srv.org(&boss, "acmepub7", 5).await;
    hook(&srv, &org).await;
    let (s, v) = srv
        .post(
            "/v1/publisher-applications",
            &boss.token,
            json!({ "display_name": "Acme Org", "org_id": org }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let p = v["data"]["id"].as_str().unwrap().to_string();
    let (s, v) = certify(&srv, &rev, &p, "approved").await;
    assert_eq!(s, 201, "{v}");
    let t = template(&srv, &boss, &p, "team-digest").await;
    let b = attested(&srv, &rev, &boss, &p, &t, "1.0.0", None, 'a').await;
    let l = listing(&srv, &boss, &p, &b.release, "public").await;
    let (s, v) = publish(&srv, &boss.strong, &p, &l, json!({})).await;
    assert_eq!(s, 200, "{v}");
    let ev = hook_events(&srv, &org).await;
    let listed = ev
        .iter()
        .find(|(k, _)| k == "nest.listing.updated")
        .expect("listing event");
    assert_eq!(listed.1["publisher_id"], p.as_str());
    assert!(ev.iter().any(|(k, _)| k == "nest.review.stage_changed"));

    // The org's own installation is announced too, ids only.
    let (s, v) = srv
        .post(
            &format!("/v1/orgs/{org}/nest-installations"),
            &boss.token,
            json!({ "listing_id": l, "handle": "digest" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let inst = v["data"]["id"].as_str().unwrap().to_string();
    let ev = hook_events(&srv, &org).await;
    let changed = ev
        .iter()
        .find(|(k, _)| k == "nest.installation.changed")
        .expect("install event");
    assert_eq!(changed.1["installation_id"], inst.as_str());
    assert_eq!(changed.1["change"], "installed");
    assert!(!changed.1.to_string().contains("configuration"));
}

// ---- console gaps --------------------------------------------------------------------------------

#[tokio::test]
async fn install_counts_reports_staff_listings_price_terms_and_roles_by_member() {
    let (srv, owner) = setup().await;
    srv.global_flag("marketplace").await;
    srv.global_flag("orgs").await;
    let (rev, ann, p, l) = published(&srv, &owner, "8", "public").await;
    let (bob, eve) = (srv.person("mbbob8").await, srv.person("mbeve8").await);
    let (s, v) = srv
        .post(
            "/v1/nest-installations",
            &bob.token,
            json!({ "listing_id": l }),
        )
        .await;
    assert_eq!(s, 201, "{v}");

    // Counts only, for the team only.
    let (s, v) = srv
        .get(&format!("/v1/publishers/{p}/listing-stats"), &ann.token)
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"][0]["installations"]["active"], 1, "{v}");
    assert!(!v.to_string().contains(&bob.account));
    assert_eq!(
        srv.get(&format!("/v1/publishers/{p}/listing-stats"), &eve.token)
            .await
            .0,
        404
    );

    // Price terms: shown on reads, editable, never priced here.
    let (_, v) = srv
        .get(&format!("/v1/marketplace/listings/{l}"), &bob.token)
        .await;
    assert_eq!(
        v["data"]["price"],
        json!({ "amount_minor": 0, "currency": "TRY", "term": "one_off", "funding": "test_credits", "on_sale": false })
    );
    let lpath = format!("/v1/publishers/{p}/listings/{l}");
    let (s, v) = srv
        .patch(
            &lpath,
            &ann.token,
            json!({ "price": { "currency": "USD", "term": "monthly" } }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["price"]["term"], "monthly");
    assert_eq!(v["data"]["price"]["currency"], "USD");
    let (s, v) = srv
        .patch(
            &lpath,
            &ann.token,
            json!({ "price": { "amount_minor": 500 } }),
        )
        .await;
    assert_eq!((s, code(&v)), (400, "price_not_available"), "{v}");
    let (s, v) = srv
        .patch(&lpath, &ann.token, json!({ "price": { "term": "yearly" } }))
        .await;
    assert_eq!((s, code(&v)), (400, "invalid_request"), "{v}");
    let (_, cat) = srv.get("/v1/marketplace/listings", &bob.token).await;
    let row = cat["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == l.as_str())
        .unwrap()
        .clone();
    assert_eq!(row["price"]["term"], "monthly");
    assert_eq!(row["publisher"]["display_name"], "Acme Nests");

    // Reports: a person, a known reason, once while open.
    let rpath = format!("/v1/marketplace/listings/{l}/reports");
    let (s, v) = srv
        .post(&rpath, &bob.token, json!({ "reason": "boring" }))
        .await;
    assert_eq!((s, code(&v)), (400, "invalid_request"), "{v}");
    let (s, v) = srv
        .post(
            &rpath,
            &bob.token,
            json!({ "reason": "misleading", "detail": "says weekly, is daily" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let report = v["data"]["id"].as_str().unwrap().to_string();
    let (s, v) = srv
        .post(&rpath, &bob.token, json!({ "reason": "spam" }))
        .await;
    assert_eq!((s, code(&v)), (409, "already_reported"), "{v}");
    let (s, _) = srv
        .post(
            "/v1/marketplace/listings/lst_missing/reports",
            &bob.token,
            json!({ "reason": "spam" }),
        )
        .await;
    assert_eq!(s, 404);

    // Staff: the queue and the listing list need a reviewer with step-up.
    assert_eq!(
        srv.get("/v1/staff/listing-reports", &bob.strong).await.0,
        403
    );
    assert_eq!(
        srv.get("/v1/staff/listing-reports", &rev.token).await.0,
        401
    );
    let (s, q) = srv.get("/v1/staff/listing-reports", &rev.strong).await;
    assert_eq!(s, 200, "{q}");
    assert_eq!(q["data"][0]["id"], report.as_str());
    assert_eq!(q["data"][0]["listing"]["open_reports"], 1);
    assert!(
        !q.to_string().contains(&bob.account),
        "reporters are not named"
    );
    let (s, v) = srv
        .post(
            &format!("/v1/staff/listing-reports/{report}/close"),
            &rev.strong,
            json!({ "resolution": "listing text fixed" }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (_, q) = srv.get("/v1/staff/listing-reports", &rev.strong).await;
    assert!(q["data"].as_array().unwrap().is_empty());
    let (s, v) = srv
        .get(
            &format!("/v1/staff/listings?status=published&publisher={p}"),
            &rev.strong,
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(ids(&v), vec![l.clone()]);
    let (_, v) = srv
        .get("/v1/staff/listings?status=draft", &rev.strong)
        .await;
    assert!(!ids(&v).contains(&l));
    assert_eq!(
        srv.get("/v1/staff/listings?status=bogus", &rev.strong)
            .await
            .0,
        400
    );

    // Publisher roles by org member id.
    let boss = srv.person("mbboss8").await;
    let org = srv.org(&boss, "acmepub8", 5).await;
    let dev = srv.person("mbdev8").await;
    let (dev_member, _) = srv.join(&org, &boss, &dev, "dev8", &[]).await;
    let (s, v) = srv
        .post(
            "/v1/publisher-applications",
            &boss.token,
            json!({ "display_name": "Acme Org 8", "org_id": org }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let op = v["data"]["id"].as_str().unwrap().to_string();
    let (s, v) = srv
        .post(
            &format!("/v1/publishers/{op}/roles"),
            &boss.token,
            json!({ "member_id": dev_member, "role": "publisher_admin" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["data"]["account_id"], dev.account.as_str(), "{v}");
    let (s, v) = srv
        .post(
            &format!("/v1/publishers/{op}/roles"),
            &boss.token,
            json!({ "member_id": "mem_missing", "role": "publisher_admin" }),
        )
        .await;
    assert_eq!((s, code(&v)), (400, "not_org_member"), "{v}");
    let (s, v) = srv
        .post(&format!("/v1/publishers/{op}/roles"), &boss.token, json!({ "member_id": dev_member, "account_id": dev.account, "role": "publisher_finance" }))
        .await;
    assert_eq!((s, code(&v)), (400, "invalid_request"), "{v}");
}
