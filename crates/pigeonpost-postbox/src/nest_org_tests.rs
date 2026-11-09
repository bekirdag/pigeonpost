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
