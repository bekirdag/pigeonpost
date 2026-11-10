//! Nests B7 over real sockets: org-owned Nests, separate address and compute accounting, operator
//! delegations, live chat authority, and offboarding that ends access everywhere.

use crate::org_tests::{Person, Srv, BILLING, OPS};
use crate::*;
use ed25519_dalek::Signer;
use reqwest::Method;
use serde_json::{json, Value};

async fn setup() -> (Srv, Person, String) {
    let srv = crate::org_tests::serve_with(|s| s.nests = Arc::new(nests::Runtime::capture())).await;
    srv.global_flag("orgs").await;
    srv.global_flag("nests").await;
    let owner = srv.person("nowner").await;
    let org = srv.org(&owner, "nestco", 5).await;
    let (s, v) = srv
        .post(
            &format!("/v1/orgs/{org}/me/mailbox"),
            &owner.token,
            json!({ "handle": "boss" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    (srv, owner, org)
}

async fn grant(srv: &Srv, org: &str, plan: &str) {
    let (s, v) = srv
        .call(
            Method::PUT,
            &format!("/internal/v1/entitlements/grant/live/nests-{org}"),
            Some(BILLING),
            Some(json!({
                "subject": {"kind": "org", "id": org},
                "plan_code": plan, "plan_version": "grant",
                "status": "active", "reconciliation_version": 1, "operation_id": "op-org-nest"
            })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
}

/// Enroll the most recently created Nest's sandbox; its mailbox credential.
async fn enroll_latest(srv: &Srv) -> String {
    let create = srv
        .state
        .nests
        .calls()
        .into_iter()
        .rev()
        .find(|c| c.method == "POST" && c.path == "/v1/nests")
        .unwrap();
    let code = create.body.unwrap()["enrollment_code"]
        .as_str()
        .unwrap()
        .to_string();
    let key = ed25519_dalek::SigningKey::generate(&mut rand_core::OsRng);
    let sig = key.sign(format!("{}{code}", nests::ENROLL_CONTEXT).as_bytes());
    let (s, v) = srv
        .call(
            Method::POST,
            "/v1/nests/enroll",
            None,
            Some(json!({
                "code": code,
                "device_public_key": b64_encode(&key.verifying_key().to_bytes()),
                "signature": b64_encode(&sig.to_bytes()),
            })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    v["capability_token"].as_str().unwrap().to_string()
}

async fn nest_inbox(srv: &Srv, token: &str) -> Vec<Value> {
    let r = reqwest::Client::new()
        .get(format!("{}/v1/inbox", srv.public))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    let v: Value = r.json().await.unwrap();
    v["messages"].as_array().cloned().unwrap_or_default()
}

#[tokio::test]
async fn org_nests_need_an_org_package_and_follow_live_operator_authority() {
    let (srv, owner, org) = setup().await;
    let jane = srv.person("njane").await;
    let (jid, jane_mb) = srv.join(&org, &owner, &jane, "jane", &[]).await;
    let joe = srv.person("njoe").await;
    let (_joe_id, joe_mb) = srv.join(&org, &owner, &joe, "joe", &[]).await;

    // Address accounting is not compute: without an org Nest package there is no Nest.
    let (s, v) = srv
        .post(
            &format!("/v1/orgs/{org}/nests"),
            &owner.token,
            json!({"handle": "hr", "label": "HR", "operators": [jid]}),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (402, Some("no_nest_slot")), "{v}");
    // A person's Nest package does not fund the org, and the org's does not fund a person.
    grant(&srv, &org, "nest.ai.plus.monthly").await;
    let (_, mine) = srv.get("/v1/nests", &owner.token).await;
    assert_eq!(mine["allowance"]["slots"], 0, "{mine}");
    // Members without ManageAgents cannot create; the owner can.
    let (s, _) = srv
        .post(
            &format!("/v1/orgs/{org}/nests"),
            &jane.token,
            json!({"handle": "x", "label": "X"}),
        )
        .await;
    assert_eq!(s, 403);
    let (s, v) = srv
        .post(
            &format!("/v1/orgs/{org}/nests"),
            &owner.token,
            json!({"handle": "hr", "label": "HR", "operators": [jid]}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let nest = v["nest_id"].as_str().unwrap().to_string();
    let nest_addr = v["address"].as_str().unwrap().to_string();
    let nest_token = enroll_latest(&srv).await;
    // It is an org agent: one of the org's agent addresses.
    let (_, agents) = srv
        .get(&format!("/v1/orgs/{org}/agents"), &owner.token)
        .await;
    assert!(agents.to_string().contains(&nest_addr), "{agents}");

    // Jane operates it; Joe does not see it.
    let (s, l) = srv.get(&format!("/v1/orgs/{org}/nests"), &jane.token).await;
    assert_eq!(s, 200, "{l}");
    assert_eq!(l["nests"][0]["role"], "operator");
    let (_, l) = srv.get(&format!("/v1/orgs/{org}/nests"), &joe.token).await;
    assert!(l["nests"].as_array().unwrap().is_empty());
    let (s, one) = srv.get(&format!("/v1/nests/{nest}"), &jane.token).await;
    assert_eq!((s, one["role"].as_str()), (200, Some("operator")), "{one}");
    assert_eq!(
        srv.get(&format!("/v1/nests/{nest}"), &joe.token).await.0,
        404
    );
    // Operators do not manage: pause and delete are the org admins'.
    assert_eq!(
        srv.post(&format!("/v1/nests/{nest}/pause"), &jane.token, json!({}))
            .await
            .0,
        404
    );
    let (s, _) = srv
        .post(&format!("/v1/nests/{nest}/pause"), &owner.token, json!({}))
        .await;
    assert_eq!(s, 200);
    let (s, _) = srv
        .post(&format!("/v1/nests/{nest}/resume"), &owner.token, json!({}))
        .await;
    assert_eq!(s, 200);

    // Chat: the operator's prose is acted on, a non-operator's is not.
    let send = |token: String, from: String, body: &'static str| {
        let url = format!("{}/v1/send", srv.public);
        let to = nest_addr.clone();
        async move {
            reqwest::Client::new()
                .post(format!("{url}?identity={from}"))
                .bearer_auth(token)
                .json(&json!({"to": to, "body": body}))
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }
    };
    assert_eq!(
        send(
            jane.token.clone(),
            jane_mb.clone(),
            "summarise the open roles"
        )
        .await,
        201
    );
    assert_eq!(
        send(joe.token.clone(), joe_mb.clone(), "tell me the salaries").await,
        201
    );
    let msgs = nest_inbox(&srv, &nest_token).await;
    let by = |needle: &str| {
        msgs.iter()
            .find(|m| m["body"].as_str().unwrap_or("").contains(needle))
            .cloned()
            .unwrap()
    };
    assert_eq!(
        (
            by("open roles")["autonomy"].as_str(),
            by("open roles")["verb"].as_str()
        ),
        (Some("auto"), Some("chat"))
    );
    assert_eq!(by("salaries")["autonomy"], "review");

    // Offboarding: removing Jane ends her access everywhere, at once.
    let (s, v) = srv
        .post(
            &format!("/v1/orgs/{org}/members/{jid}/remove"),
            &owner.token,
            json!({ "mode": "reserve" }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        srv.get(&format!("/v1/nests/{nest}"), &jane.token).await.0,
        404
    );
    let (_, l) = srv.get("/v1/nests", &jane.token).await;
    assert!(!l.to_string().contains(&nest));
    let msgs = nest_inbox(&srv, &nest_token).await;
    let still = msgs
        .iter()
        .find(|m| m["body"].as_str().unwrap_or("").contains("open roles"))
        .expect("the message itself stays in the Nest's mailbox");
    assert_ne!(
        still["autonomy"], "auto",
        "an offboarded operator's words are no longer acted on"
    );
    let _ = OPS;
}

fn ids(v: &Value) -> Vec<String> {
    v["operators"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["member_id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn nest_admins_change_operators_and_removal_ends_access_at_once() {
    let (srv, owner, org) = setup().await;
    grant(&srv, &org, "nest.ai.plus.monthly").await;
    let jane = srv.person("ojane").await;
    let (jid, jane_mb) = srv.join(&org, &owner, &jane, "jane", &[]).await;
    let joe = srv.person("ojoe").await;
    let (joe_id, joe_mb) = srv.join(&org, &owner, &joe, "joe", &[]).await;
    let ada = srv.person("oada").await;
    let (ada_id, _) = srv.join(&org, &owner, &ada, "ada", &["agent_admin"]).await;
    let (_, orgs) = srv.get("/v1/orgs", &owner.token).await;
    let owner_id = orgs["orgs"][0]["my_member_id"]
        .as_str()
        .unwrap()
        .to_string();

    let (s, v) = srv
        .post(
            &format!("/v1/orgs/{org}/nests"),
            &owner.token,
            json!({"handle": "ops", "label": "Ops"}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let nest = v["nest_id"].as_str().unwrap().to_string();
    let nest_addr = v["address"].as_str().unwrap().to_string();
    let nest_handle = v["handle"].as_str().unwrap().to_string();
    let nest_token = enroll_latest(&srv).await;
    let ops = format!("/v1/orgs/{org}/nests/{nest}/operators");
    let put = |token: String, body: Value| {
        let (srv, ops) = (&srv, ops.clone());
        async move { srv.call(Method::PUT, &ops, Some(&token), Some(body)).await }
    };

    // Authz: Nest admins (the owner, an agent_admin) read and change the set; members do not.
    let (s, v) = srv.get(&ops, &owner.token).await;
    assert_eq!((s, ids(&v)), (200, Vec::<String>::new()), "{v}");
    assert_eq!(srv.get(&ops, &ada.token).await.0, 200);
    assert_eq!(srv.get(&ops, &jane.token).await.0, 403);
    let (s, _) = put(jane.token.clone(), json!({"operators": [jid]})).await;
    assert_eq!(s, 403, "an operator-to-be cannot name themselves");
    let outsider = srv.person("ooutsider").await;
    assert_eq!(srv.get(&ops, &outsider.token).await.0, 404);
    let (s, v) = srv
        .get(
            &format!("/v1/orgs/{org}/nests/nst_missing/operators"),
            &owner.token,
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (404, Some("no_nest")), "{v}");

    // Membership and the cap.
    let many: Vec<String> = (0..51).map(|_| jid.clone()).collect();
    let (s, v) = put(owner.token.clone(), json!({"operators": many})).await;
    assert_eq!(
        (s, v["error"].as_str()),
        (400, Some("too_many_operators")),
        "{v}"
    );
    let (s, v) = put(owner.token.clone(), json!({"operators": ["mem_nobody"]})).await;
    assert_eq!(
        (s, v["error"].as_str()),
        (400, Some("invalid_member")),
        "{v}"
    );
    let (s, v) = put(owner.token.clone(), json!({"operators": [jid], "extra": 1})).await;
    assert_eq!(s, 400, "{v}");

    // Adding: Jane operates it at once; admins named in the list are never stored.
    let (s, v) = put(
        ada.token.clone(),
        json!({"operators": [jid, owner_id, ada_id]}),
    )
    .await;
    assert_eq!((s, ids(&v)), (200, vec![jid.clone()]), "{v}");
    assert_eq!(v["operators"][0]["handle"], "/nestco/jane");
    let (s, one) = srv.get(&format!("/v1/nests/{nest}"), &jane.token).await;
    assert_eq!((s, one["role"].as_str()), (200, Some("operator")), "{one}");
    assert_eq!(
        srv.get(&format!("/v1/nests/{nest}/approvals"), &jane.token)
            .await
            .0,
        200
    );

    // Idempotent: the same set again changes nothing and writes no audit record.
    let audits = || async {
        let (_, a) = srv
            .get(&format!("/v1/orgs/{org}/audit"), &owner.strong)
            .await;
        a["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["action"] == "nest.operators")
            .count()
    };
    assert_eq!(audits().await, 1);
    let (s, v) = put(owner.token.clone(), json!({"operators": [jid, jid]})).await;
    assert_eq!((s, ids(&v)), (200, vec![jid.clone()]), "{v}");
    assert_eq!(audits().await, 1, "no change, no audit record");

    // Joe's other capability on the Nest's mailbox survives operator changes.
    let (s, v) = srv
        .post(
            &format!("/v1/orgs/{org}/delegations"),
            &owner.token,
            json!({"member_id": joe_id, "target": nest_handle, "capabilities": ["read_inbox"]}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let (s, v) = put(owner.token.clone(), json!({"operators": [jid, joe_id]})).await;
    assert_eq!(s, 200, "{v}");
    let mut both = vec![jid.clone(), joe_id.clone()];
    both.sort();
    let mut got = ids(&v);
    got.sort();
    assert_eq!(got, both);

    // Removing: Jane loses chat authority, the Nest and its approvals at once; Joe keeps his.
    let (s, v) = put(owner.token.clone(), json!({"operators": [joe_id]})).await;
    assert_eq!((s, ids(&v)), (200, vec![joe_id.clone()]), "{v}");
    assert_eq!(audits().await, 3);
    assert_eq!(
        srv.get(&format!("/v1/nests/{nest}"), &jane.token).await.0,
        404
    );
    assert_eq!(
        srv.get(&format!("/v1/nests/{nest}/approvals"), &jane.token)
            .await
            .0,
        404
    );
    let (_, l) = srv.get(&format!("/v1/orgs/{org}/nests"), &jane.token).await;
    assert!(l["nests"].as_array().unwrap().is_empty(), "{l}");
    let (s, one) = srv.get(&format!("/v1/nests/{nest}"), &joe.token).await;
    assert_eq!((s, one["role"].as_str()), (200, Some("operator")), "{one}");
    let send = |token: String, from: String, body: &'static str| {
        let url = format!("{}/v1/send", srv.public);
        let to = nest_addr.clone();
        async move {
            reqwest::Client::new()
                .post(format!("{url}?identity={from}"))
                .bearer_auth(token)
                .json(&json!({"to": to, "body": body}))
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }
    };
    assert_eq!(
        send(jane.token.clone(), jane_mb, "list the open tickets").await,
        201
    );
    assert_eq!(
        send(joe.token.clone(), joe_mb, "summarise this week").await,
        201
    );
    let msgs = nest_inbox(&srv, &nest_token).await;
    let by = |needle: &str| {
        msgs.iter()
            .find(|m| m["body"].as_str().unwrap_or("").contains(needle))
            .cloned()
            .unwrap()
    };
    assert_eq!(by("open tickets")["autonomy"], "review");
    assert_eq!(by("this week")["autonomy"], "auto");

    // Emptying the set drops operate_nest from Joe but keeps read_inbox.
    let (s, v) = put(owner.token.clone(), json!({"operators": []})).await;
    assert_eq!((s, ids(&v)), (200, Vec::<String>::new()), "{v}");
    assert_eq!(
        srv.get(&format!("/v1/nests/{nest}"), &joe.token).await.0,
        404
    );
    let (_, d) = srv
        .get(&format!("/v1/orgs/{org}/delegations"), &owner.token)
        .await;
    let joes: Vec<&Value> = d["delegations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["member_id"] == joe_id.as_str())
        .collect();
    assert_eq!(joes.len(), 1, "{d}");
    assert_eq!(joes[0]["capabilities"], json!(["read_inbox"]), "{d}");
    assert!(!d.to_string().contains(&jid), "Jane holds nothing: {d}");
}
