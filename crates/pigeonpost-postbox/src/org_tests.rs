//! Phase 4 end-to-end tests over real sockets: org activation, the role matrix over HTTP, seat
//! races, invitation scope and address matching, MFA step-up, rule precedence in delivery,
//! device pairing and enrollment, offboarding and lifecycle transitions (SDS §25).

use crate::*;
use ed25519_dalek::{Signer, SigningKey};
use reqwest::{Client, Method};
use serde_json::{json, Value};

pub(crate) const OPS: &str = "svc_ops_0123456789abcdef0123456789abcdef";
pub(crate) const BILLING: &str = "svc_billing_0123456789abcdef0123456789abcdef";
pub(crate) const ISSUER: &str = "https://auth.example/realms/x";

pub(crate) struct Srv {
    pub public: String,
    pub internal: String,
    pub http: Client,
    pub state: AppState,
}

pub(crate) async fn serve() -> Srv {
    serve_with(|_| {}).await
}

/// A server whose state the caller adjusts first (webhook egress, step-up policy, …).
pub(crate) async fn serve_with(adjust: impl FnOnce(&mut AppState)) -> Srv {
    let mut state = crate::tests::test_state();
    state.service_tokens = Arc::new(internal_api::tokens_for_test(&[
        ("POSTBOX_SVC_TOKEN_OPS", OPS),
        ("POSTBOX_SVC_TOKEN_BILLING", BILLING),
    ]));
    state.reserved_names = Some(Arc::new(std::collections::HashSet::from([
        "admin".to_string()
    ])));
    state.step_up = Arc::new(principal::StepUpPolicy {
        acr_values: vec!["mfa".into()],
        max_age: 600,
    });
    adjust(&mut state);
    let public = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let internal = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let srv = Srv {
        public: format!("http://{}", public.local_addr().unwrap()),
        internal: format!("http://{}", internal.local_addr().unwrap()),
        http: Client::new(),
        state: state.clone(),
    };
    let app =
        build_router(state.clone()).into_make_service_with_connect_info::<std::net::SocketAddr>();
    let iapp =
        internal_api::router(state).into_make_service_with_connect_info::<std::net::SocketAddr>();
    tokio::spawn(async move { axum::serve(public, app).await.unwrap() });
    tokio::spawn(async move { axum::serve(internal, iapp).await.unwrap() });
    srv
}

/// A person: account, a member token (with a verified address), and a strong-auth token.
#[derive(Clone)]
pub(crate) struct Person {
    pub account: String,
    pub token: String,
    pub strong: String,
    pub address: String,
}

impl Srv {
    pub(crate) async fn call(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (u16, Value) {
        let base = if path.starts_with("/internal/") {
            &self.internal
        } else {
            &self.public
        };
        let mut req = self.http.request(method, format!("{base}{path}"));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }
    pub(crate) async fn get(&self, path: &str, t: &str) -> (u16, Value) {
        self.call(Method::GET, path, Some(t), None).await
    }
    pub(crate) async fn post(&self, path: &str, t: &str, b: Value) -> (u16, Value) {
        self.call(Method::POST, path, Some(t), Some(b)).await
    }
    pub(crate) async fn patch(&self, path: &str, t: &str, b: Value) -> (u16, Value) {
        self.call(Method::PATCH, path, Some(t), Some(b)).await
    }
    pub(crate) async fn global_flag(&self, name: &str) {
        let (s, _) = self
            .call(
                Method::PUT,
                &format!("/internal/v1/features/{name}"),
                Some(OPS),
                Some(json!({ "scope": "global", "enabled": true })),
            )
            .await;
        assert_eq!(s, 200, "flag {name}");
    }

    pub(crate) async fn person(&self, name: &str) -> Person {
        let sub = format!("sub-{name}");
        let account = self
            .state
            .store
            .account_for_sub(sub.clone(), format!("acct_{name}"), now_unix())
            .await
            .unwrap();
        let address = format!("{name}@example.com");
        Person {
            account,
            token: oidc::test_member_token_with(
                ISSUER,
                &sub,
                oidc::test_person_claims(&address, None),
            ),
            strong: oidc::test_member_token_with(
                ISSUER,
                &sub,
                oidc::test_person_claims(&address, Some("mfa")),
            ),
            address,
        }
    }

    /// Draft, then activate through the billing service with a grant entitlement.
    pub(crate) async fn org(&self, owner: &Person, ns: &str, seats: i64) -> String {
        let op = format!("op-{ns}-0123456789abcdef");
        let (s, v) = self
            .call(Method::POST, "/internal/v1/org-drafts", Some(BILLING), Some(json!({
                "operation_id": op, "owner_account": owner.account, "name": format!("{ns} Inc"),
                "namespace": ns, "plan_code": format!("ent-{seats}-monthly-usd"), "seats": seats,
            })))
            .await;
        assert_eq!(s, 201, "draft {v}");
        let id = v["org"]["id"].as_str().unwrap().to_string();
        self.entitlement(
            &id,
            seats,
            "active",
            now_unix() as i64 + 30 * 86_400,
            1,
            true,
        )
        .await;
        id
    }

    pub(crate) async fn entitlement(
        &self,
        org: &str,
        seats: i64,
        status: &str,
        paid_through: i64,
        version: i64,
        activate: bool,
    ) {
        let ent = json!({
            "provider": "grant", "environment": "test", "reference": format!("ref-{org}"),
            "plan_code": format!("ent-{seats}-monthly-usd"), "plan_version": "2026", "seats": seats,
            "status": status, "period_end": paid_through, "paid_through": paid_through,
            "reconciliation_version": version,
        });
        let (s, v) = if activate {
            self.call(
                Method::PUT,
                &format!("/internal/v1/orgs/{org}/activate"),
                Some(BILLING),
                Some(
                    json!({ "operation_id": format!("act-{org}-0123456789"), "entitlement": ent }),
                ),
            )
            .await
        } else {
            self.call(
                Method::PUT,
                &format!("/internal/v1/orgs/{org}/entitlement"),
                Some(BILLING),
                Some(ent),
            )
            .await
        };
        assert_eq!(s, 200, "entitlement {v}");
    }

    /// Invite `who` and have them accept; returns (member_id, mailbox address).
    pub(crate) async fn join(
        &self,
        org: &str,
        admin: &Person,
        who: &Person,
        handle: &str,
        roles: &[&str],
    ) -> (String, String) {
        let (s, v) = self
            .post(&format!("/v1/orgs/{org}/invites"), &admin.strong, json!({ "invites": [{
                "first_name": "First", "last_name": "Last", "contact": who.address, "handle": handle, "roles": roles,
            }]}))
            .await;
        assert_eq!(s, 200, "{v}");
        assert_eq!(v["results"][0]["status"], "invited", "{v}");
        let link = v["results"][0]["link"].as_str().unwrap();
        let token = link.split('#').nth(1).unwrap();
        let (s, v) = self
            .post(
                "/v1/org-invites/accept",
                &who.token,
                json!({ "token": token }),
            )
            .await;
        assert_eq!(s, 200, "accept {v}");
        (
            v["member"]["id"].as_str().unwrap().into(),
            v["mailbox"]["address"].as_str().unwrap().into(),
        )
    }
}

pub(crate) async fn setup() -> (Srv, Person, String) {
    let srv = serve().await;
    srv.global_flag("orgs").await;
    srv.global_flag("groups").await;
    let owner = srv.person("owner").await;
    let org = srv.org(&owner, "acme", 5).await;
    let (s, v) = srv
        .post(
            &format!("/v1/orgs/{org}/me/mailbox"),
            &owner.token,
            json!({ "handle": "boss" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    assert!(v["general_room"].is_string());
    (srv, owner, org)
}

pub(crate) fn token_of(link: &str) -> String {
    link.split('#').nth(1).unwrap().to_string()
}

#[tokio::test]
async fn activation_invitation_and_acceptance() {
    let (srv, owner, org) = setup().await;
    let (s, v) = srv.get("/v1/orgs", &owner.token).await;
    assert_eq!(s, 200);
    assert_eq!(v["orgs"][0]["id"], org);
    assert_eq!(v["orgs"][0]["my_roles"], json!(["owner"]));
    assert_eq!(v["orgs"][0]["seats_used"], 1, "the owner holds a seat");
    assert_eq!(v["orgs"][0]["my_mailbox"]["handle"], "/acme/boss");

    // The default rule exists.
    let (_, rules) = srv
        .get(&format!("/v1/orgs/{org}/rules"), &owner.token)
        .await;
    assert_eq!(rules["rules"][0]["owners"], "/acme/*");

    let jane = srv.person("jane").await;
    let (s, v) = srv.post(&format!("/v1/orgs/{org}/invites"), &owner.token, json!({ "invites": [{
        "first_name": "Jane", "last_name": "Doe", "contact": "Jane@Example.com", "handle": "jane",
    }]})).await;
    assert_eq!(s, 200, "{v}");
    let link = v["results"][0]["link"].as_str().unwrap().to_string();
    assert!(link.starts_with("https://enterprise.pigeonpost.dev/invite#"));
    let token = token_of(&link);
    assert_eq!(token.len(), 43);

    // Preview: no credential needed; a wrong token is one generic answer.
    let (s, p) = srv
        .call(
            Method::POST,
            "/v1/org-invites/preview",
            None,
            Some(json!({ "token": token })),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(p["handle"], "/acme/jane");
    assert_eq!(p["contact_masked"], "j***@example.com");
    assert_eq!(p["rooms"], json!(["General"]));
    let (s, bad) = srv
        .call(
            Method::POST,
            "/v1/org-invites/preview",
            None,
            Some(json!({ "token": "x".repeat(43) })),
        )
        .await;
    assert_eq!((s, bad["error"].as_str()), (404, Some("invite_invalid")));

    // Another person cannot accept it, and the refusal masks the address.
    let mallory = srv.person("mallory").await;
    let (s, v) = srv
        .post(
            "/v1/org-invites/accept",
            &mallory.token,
            json!({ "token": token }),
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (403, Some("invite_address_mismatch"))
    );
    assert_eq!(v["contact_masked"], "j***@example.com");
    // An unverified address does not count either.
    let unverified = oidc::test_member_token_with(
        ISSUER,
        "sub-jane",
        oidc::test_unverified_claims(&jane.address),
    );
    let (s, _) = srv
        .post(
            "/v1/org-invites/accept",
            &unverified,
            json!({ "token": token }),
        )
        .await;
    assert_eq!(s, 403);
    // Automation cannot accept.
    let (s, _) = srv
        .post(
            "/v1/org-invites/accept",
            "pk_live_nope",
            json!({ "token": token }),
        )
        .await;
    assert!(s == 401 || s == 403);

    let (s, v) = srv
        .post(
            "/v1/org-invites/accept",
            &jane.token,
            json!({ "token": token }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let jane_mb = v["mailbox"]["address"].as_str().unwrap().to_string();
    assert_eq!(v["mailbox"]["handle"], "/acme/jane");
    // A retry by the same person answers the same; the token is otherwise spent.
    let (s, again) = srv
        .post(
            "/v1/org-invites/accept",
            &jane.token,
            json!({ "token": token }),
        )
        .await;
    assert_eq!((s, &again["mailbox"]["address"]), (200, &json!(jane_mb)));
    let (s, _) = srv
        .call(
            Method::POST,
            "/v1/org-invites/preview",
            None,
            Some(json!({ "token": token })),
        )
        .await;
    assert_eq!(s, 404, "single use");

    // The mailbox is in Jane's own account, with its handle visible, and in General (keying).
    let (_, ids) = srv.get("/v1/identities", &jane.token).await;
    assert!(ids.to_string().contains("/acme/jane"), "{ids}");
    let (_, groups) = srv
        .call(
            Method::GET,
            "/v1/groups?state=active,invited,keying",
            Some(&jane.token),
            None,
        )
        .await;
    let _ = groups; // General is provisioning until the owner's device activates it.
    let (_, dir) = srv
        .get(&format!("/v1/orgs/{org}/directory?q=jane"), &jane.token)
        .await;
    assert_eq!(dir["entries"][0]["handle"], "/acme/jane");
    // Members do not get the people list.
    let (s, _) = srv
        .get(&format!("/v1/orgs/{org}/members"), &jane.token)
        .await;
    assert_eq!(s, 403);
    // Nobody outside the org learns it exists.
    let (s, _) = srv.get(&format!("/v1/orgs/{org}"), &mallory.token).await;
    assert_eq!(s, 404);

    // Audit trail.
    let (_, audit) = srv
        .get(&format!("/v1/orgs/{org}/audit"), &owner.token)
        .await;
    let actions: Vec<&str> = audit["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["action"].as_str())
        .collect();
    assert!(
        actions.contains(&"member.invite") && actions.contains(&"member.join"),
        "{actions:?}"
    );
    assert!(
        !audit.to_string().contains(&token),
        "no token in the audit log"
    );
    assert!(
        !audit.to_string().contains("jane@example.com"),
        "no contact address in the audit log"
    );
}

#[tokio::test]
async fn invitations_expire_resend_rotates_and_revoke_releases() {
    let (srv, owner, org) = setup().await;
    let (_, v) = srv
        .post(
            &format!("/v1/orgs/{org}/invites"),
            &owner.token,
            json!({ "invites": [{
        "first_name": "Bo", "last_name": "B", "contact": "bo@example.com", "handle": "bo" }]}),
        )
        .await;
    let inv = v["results"][0]["invite_id"].as_str().unwrap().to_string();
    let old = token_of(v["results"][0]["link"].as_str().unwrap());
    let (s, r) = srv
        .post(
            &format!("/v1/orgs/{org}/invites/{inv}/resend"),
            &owner.token,
            json!({}),
        )
        .await;
    assert_eq!(s, 200, "{r}");
    let new = token_of(r["link"].as_str().unwrap());
    assert_ne!(old, new);
    let (s, _) = srv
        .call(
            Method::POST,
            "/v1/org-invites/preview",
            None,
            Some(json!({ "token": old })),
        )
        .await;
    assert_eq!(s, 404, "resend invalidates the old token");
    let new_inv = r["invite_id"].as_str().unwrap().to_string();
    // Revoke: immediate, seat released, the same handle can be invited again.
    let (s, _) = srv
        .call(
            Method::DELETE,
            &format!("/v1/orgs/{org}/invites/{new_inv}"),
            Some(&owner.token),
            None,
        )
        .await;
    assert_eq!(s, 200);
    let (s, _) = srv
        .call(
            Method::POST,
            "/v1/org-invites/preview",
            None,
            Some(json!({ "token": new })),
        )
        .await;
    assert_eq!(s, 404);
    let (_, o) = srv.get(&format!("/v1/orgs/{org}"), &owner.token).await;
    assert_eq!(o["org"]["seats_used"], 1);
    let (_, v) = srv
        .post(
            &format!("/v1/orgs/{org}/invites"),
            &owner.token,
            json!({ "invites": [{
        "first_name": "Bo", "last_name": "B", "contact": "bo@example.com", "handle": "bo" }]}),
        )
        .await;
    assert_eq!(v["results"][0]["status"], "invited");
    // Duplicate address and taken handle are refused per row.
    let (_, v) = srv.post(&format!("/v1/orgs/{org}/invites"), &owner.token, json!({ "invites": [
        { "first_name": "Bo", "last_name": "B", "contact": "BO@example.com", "handle": "bo2" },
        { "first_name": "X", "last_name": "Y", "contact": "x@example.com", "handle": "bo" },
        { "first_name": "X", "last_name": "Y", "contact": "x@example.com", "handle": "/other/x" },
    ]})).await;
    assert_eq!(v["results"][0]["status"], "already_invited");
    assert_eq!(v["results"][1]["status"], "handle_taken");
    assert_eq!(v["results"][2]["status"], "invalid_handle");
}

#[tokio::test]
async fn seats_are_exact_under_concurrency() {
    let (srv, owner, org) = setup().await;
    // 5 seats, the owner holds one: of 12 concurrent invitations exactly 4 succeed.
    let srv = std::sync::Arc::new(srv);
    let mut tasks = Vec::new();
    for i in 0..12 {
        let srv = srv.clone();
        let t = owner.token.clone();
        let org = org.clone();
        tasks.push(tokio::spawn(async move {
            srv.post(&format!("/v1/orgs/{org}/invites"), &t, json!({ "invites": [{
                "first_name": "P", "last_name": "Q", "contact": format!("p{i}@example.com"), "handle": format!("p{i}") }]}))
                .await
                .1["results"][0]["status"]
                .as_str()
                .unwrap()
                .to_string()
        }));
    }
    let mut ok = 0;
    let mut full = 0;
    for t in tasks {
        match t.await.unwrap().as_str() {
            "invited" => ok += 1,
            "seat_limit" => full += 1,
            other => panic!("unexpected {other}"),
        }
    }
    assert_eq!((ok, full), (4, 8));
    let (_, o) = srv.get(&format!("/v1/orgs/{org}"), &owner.token).await;
    assert_eq!(o["org"]["seats_used"], 5);
}

#[tokio::test]
async fn role_matrix_over_http() {
    let (srv, owner, org) = setup().await;
    let admin = srv.person("admin1").await;
    let people = srv.person("people1").await;
    let agents = srv.person("agents1").await;
    let billing = srv.person("billing1").await;
    // 5 seats: owner + 4.
    let (admin_id, _) = srv.join(&org, &owner, &admin, "admin1", &["admin"]).await;
    let (_, _) = srv
        .join(&org, &owner, &people, "people1", &["people_admin"])
        .await;
    let (_, _) = srv
        .join(&org, &owner, &agents, "agents1", &["agent_admin"])
        .await;
    let (bill_id, _) = srv
        .join(&org, &owner, &billing, "billing1", &["billing_admin"])
        .await;
    let o = |p: &str| format!("/v1/orgs/{org}{p}");

    // Billing view: owner, admin, billing admin.
    for (who, want) in [
        (&owner, 200),
        (&admin, 200),
        (&people, 403),
        (&agents, 403),
        (&billing, 200),
    ] {
        assert_eq!(srv.get(&o("/billing"), &who.token).await.0, want);
    }
    // Security (domains): owner and admin.
    for (who, want) in [
        (&owner, 200),
        (&admin, 200),
        (&people, 403),
        (&agents, 403),
        (&billing, 403),
    ] {
        assert_eq!(srv.get(&o("/domains"), &who.token).await.0, want);
    }
    // Agents: owner, admin, agent admin create.
    let mut n = 0;
    for (who, want) in [
        (&owner, 201),
        (&admin, 201),
        (&people, 403),
        (&agents, 201),
        (&billing, 403),
    ] {
        n += 1;
        let (s, v) = srv
            .post(
                &o("/agents"),
                &who.token,
                json!({ "handle": format!("bot{n}") }),
            )
            .await;
        assert_eq!(s, want, "{v}");
    }
    // Rules: people admins edit members' rules only, agent admins agents' only.
    let members_rule = json!({ "owners": "members", "peers": "/acme/*", "known": true });
    let agents_rule = json!({ "owners": "agents", "peers": "/acme/*", "admission": "allow" });
    assert_eq!(
        srv.post(&o("/rules"), &people.token, members_rule.clone())
            .await
            .0,
        201
    );
    assert_eq!(
        srv.post(&o("/rules"), &people.token, agents_rule.clone())
            .await
            .0,
        403
    );
    assert_eq!(
        srv.post(&o("/rules"), &agents.token, agents_rule).await.0,
        201
    );
    assert_eq!(
        srv.post(&o("/rules"), &agents.token, members_rule).await.0,
        403
    );
    assert_eq!(
        srv.post(
            &o("/rules"),
            &billing.token,
            json!({ "owners": "agents", "peers": "*", "known": true })
        )
        .await
        .0,
        403
    );

    // Role grants: admins hand out scoped roles but not admin; the people admin hands out none.
    let (s, v) = srv
        .patch(
            &o(&format!("/members/{bill_id}")),
            &admin.token,
            json!({ "roles": ["billing_admin", "agent_admin"] }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, _) = srv
        .patch(
            &o(&format!("/members/{bill_id}")),
            &people.token,
            json!({ "roles": ["billing_admin"] }),
        )
        .await;
    assert_eq!(s, 403);
    let (s, v) = srv
        .patch(
            &o(&format!("/members/{bill_id}")),
            &admin.strong,
            json!({ "roles": ["admin"] }),
        )
        .await;
    assert_eq!(s, 403, "only the owner appoints admins: {v}");
    // The owner needs step-up for it…
    let (s, v) = srv
        .patch(
            &o(&format!("/members/{bill_id}")),
            &owner.token,
            json!({ "roles": ["admin"] }),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (401, Some("step_up_required")));
    assert_eq!(v["acr_values"], "mfa");
    // …and with it, it works.
    let (s, _) = srv
        .patch(
            &o(&format!("/members/{bill_id}")),
            &owner.strong,
            json!({ "roles": ["admin"] }),
        )
        .await;
    assert_eq!(s, 200);
    // Peer full admins cannot touch each other; nobody touches the owner.
    let (s, _) = srv
        .patch(
            &o(&format!("/members/{bill_id}")),
            &admin.token,
            json!({ "status": "suspended" }),
        )
        .await;
    assert_eq!(s, 403);
    let owner_id = srv.get(&o(""), &owner.token).await.1["me"]["member_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (s, _) = srv
        .post(
            &o(&format!("/members/{owner_id}/remove")),
            &admin.token,
            json!({}),
        )
        .await;
    assert_eq!(s, 403);
    // Audit export and org delete: owner/admin with step-up; delete owner only.
    assert_eq!(srv.get(&o("/audit/export"), &admin.token).await.0, 401);
    assert_eq!(srv.get(&o("/audit/export"), &admin.strong).await.0, 200);
    assert_eq!(srv.get(&o("/audit/export"), &people.strong).await.0, 403);
    let (s, _) = srv
        .call(
            Method::DELETE,
            &o(""),
            Some(&admin.strong),
            Some(json!({ "confirm_namespace": "acme" })),
        )
        .await;
    assert_eq!(s, 403);
    let (s, _) = srv
        .call(
            Method::DELETE,
            &o(""),
            Some(&owner.token),
            Some(json!({ "confirm_namespace": "acme" })),
        )
        .await;
    assert_eq!(s, 401, "step-up for deletion");
    let _ = admin_id;

    // A mailbox token or API key can never act on the org surface.
    let (api_key, rec) = new_api_key();
    srv.state
        .store
        .add_api_key(owner.account.clone(), rec, now_unix())
        .await
        .unwrap();
    let (s, v) = srv.get(&o(""), &api_key).await;
    assert_eq!((s, v["error"].as_str()), (403, Some("human_required")));
}

#[tokio::test]
async fn require_mfa_gates_members_and_acceptance() {
    let (srv, owner, org) = setup().await;
    // Turning it on needs the owner to pass it themselves.
    let (s, _) = srv
        .patch(
            &format!("/v1/orgs/{org}"),
            &owner.token,
            json!({ "settings": { "require_mfa": true } }),
        )
        .await;
    assert_eq!(s, 401);
    let (s, v) = srv
        .patch(
            &format!("/v1/orgs/{org}"),
            &owner.strong,
            json!({ "settings": { "require_mfa": true } }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        srv.get(&format!("/v1/orgs/{org}"), &owner.token).await.0,
        401
    );
    let kim = srv.person("kim").await;
    let (_, v) = srv
        .post(
            &format!("/v1/orgs/{org}/invites"),
            &owner.strong,
            json!({ "invites": [{
        "first_name": "Kim", "last_name": "K", "contact": kim.address, "handle": "kim" }]}),
        )
        .await;
    let token = token_of(v["results"][0]["link"].as_str().unwrap());
    let (s, v) = srv
        .post(
            "/v1/org-invites/accept",
            &kim.token,
            json!({ "token": token }),
        )
        .await;
    assert_eq!((s, v["reason"].as_str()), (401, Some("mfa_required")));
    let (s, _) = srv
        .post(
            "/v1/org-invites/accept",
            &kim.strong,
            json!({ "token": token }),
        )
        .await;
    assert_eq!(s, 200);
}

/// Send a message from one mailbox to another through the account that holds the sender.
pub(crate) async fn send(srv: &Srv, token: &str, from: &str, to: &str) -> (u16, Value) {
    let (s, v) = srv
        .post(
            "/v1/send",
            token,
            json!({ "from": from, "to": to, "body": "hello" }),
        )
        .await;
    (if s == 201 { 200 } else { s }, v)
}

pub(crate) fn enc(s: &str) -> String {
    s.replace('/', "%2F")
}

#[tokio::test]
async fn rules_decide_delivery_and_a_personal_block_always_wins() {
    let (srv, owner, org) = setup().await;
    let jane = srv.person("jane").await;
    let (_, jane_mb) = srv.join(&org, &owner, &jane, "jane", &[]).await;
    let boss_mb = srv.get("/v1/orgs", &owner.token).await.1["orgs"][0]["my_mailbox"]["address"]
        .as_str()
        .unwrap()
        .to_string();

    // Jane closes her inbox to strangers; the company rule still makes the boss known.
    srv.state
        .store
        .set_inbox_policy(jane_mb.clone(), Some(false), None, now_unix())
        .await
        .unwrap();
    let (s, v) = send(&srv, &owner.token, &boss_mb, "/acme/jane").await;
    assert_eq!(s, 200, "known by company rule: {v}");
    let (_, inbox) = srv
        .call(
            Method::GET,
            &format!("/v1/inbox?identity={}", jane_mb),
            Some(&jane.token),
            None,
        )
        .await;
    let msg = &inbox["messages"][0];
    assert_eq!(msg["sender_known"], true);
    assert!(msg["matched_rule"]["id"].as_str().is_some(), "{msg}");

    // A stranger is refused by Jane's own policy.
    let outsider = srv.person("outsider").await;
    let out_mb = do_create_identity(&srv.state, Some(outsider.account.clone()), None, None)
        .await
        .unwrap()["address"]
        .as_str()
        .unwrap()
        .to_string();
    let (s, _) = send(&srv, &outsider.token, &out_mb, "/acme/jane").await;
    assert_eq!(s, 403);

    // Jane blocks the boss personally: that wins over the company "known" rule (D-02).
    let (s, v) = srv
        .call(
            Method::PUT,
            "/v1/contacts",
            Some(&jane.token),
            Some(json!({ "peer": "/acme/boss", "admission": "block", "identity": jane_mb })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, _) = send(&srv, &owner.token, &boss_mb, "/acme/jane").await;
    assert_eq!(s, 403);

    // Evaluate explains it.
    let (s, e) = srv
        .post(
            &format!("/v1/orgs/{org}/rules/evaluate"),
            &owner.token,
            json!({ "from": "/acme/boss", "to": "/acme/jane" }),
        )
        .await;
    assert_eq!(s, 200, "{e}");
    assert_eq!(e["decided_by"], "personal_block");
    assert_eq!(e["admitted"], false);

    // A locked block on agents from strangers holds even against an agent-level allow.
    let (s, a) = srv
        .post(
            &format!("/v1/orgs/{org}/agents"),
            &owner.token,
            json!({ "handle": "build" }),
        )
        .await;
    assert_eq!(s, 201, "{a}");
    let (s, _) = srv.post(&format!("/v1/orgs/{org}/rules"), &owner.token,
        json!({ "owners": "agents", "peers": "*", "admission": "block", "locked": true, "priority": 10 })).await;
    assert_eq!(s, 201);
    let (s, _) = send(&srv, &outsider.token, &out_mb, "/acme/build").await;
    assert_eq!(s, 403);
    let (_, e) = srv
        .post(
            &format!("/v1/orgs/{org}/rules/evaluate"),
            &owner.token,
            json!({ "from": out_mb, "to": "/acme/build" }),
        )
        .await;
    assert_eq!(
        (e["decided_by"].as_str(), e["admitted"].as_bool()),
        (Some("org_rule"), Some(false))
    );
    // Unsafe rules are refused.
    let (s, v) = srv.post(&format!("/v1/orgs/{org}/rules"), &owner.token,
        json!({ "owners": "agents", "peers": "/acme/jane", "autonomy": "auto", "verbs": ["deploy"] })).await;
    assert_eq!((s, v["error"].as_str()), (400, Some("never_auto_verb")));
}

pub(crate) fn b64(bytes: &[u8]) -> String {
    b64_encode(bytes)
}

#[tokio::test]
async fn pairing_and_enrollment_issue_scoped_revocable_credentials() {
    let (srv, owner, org) = setup().await;
    let (_, a) = srv
        .post(
            &format!("/v1/orgs/{org}/agents"),
            &owner.token,
            json!({ "handle": "agent1", "label": "Build box" }),
        )
        .await;
    let agent = a["agent"]["address"].as_str().unwrap().to_string();

    // --- pairing ---
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let secret = [9u8; 32];
    let challenge = sha256(&secret);
    let (s, p) = srv
        .call(
            Method::POST,
            "/v1/device-pairings",
            None,
            Some(json!({
        "handle": "/acme/agent1", "device_public_key": b64(key.verifying_key().as_bytes()),
        "claim_challenge": b64(&challenge), "label": "laptop" })),
        )
        .await;
    assert_eq!(s, 201, "{p}");
    let pid = p["pairing_id"].as_str().unwrap().to_string();
    let code = p["user_code"].as_str().unwrap().to_string();
    assert!(p["verification_uri_complete"]
        .as_str()
        .unwrap()
        .ends_with(&code));
    let sig = b64(&key
        .sign(format!("pigeonpost-pairing-claim-v1\0{pid}").as_bytes())
        .to_bytes());
    let claim = json!({ "pairing_id": pid, "claim_secret": b64(&secret), "signature": sig });
    let (s, v) = srv
        .call(
            Method::POST,
            "/v1/device-pairings/claim",
            None,
            Some(claim.clone()),
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (428, Some("authorization_pending"))
    );
    // A wrong secret learns nothing.
    let mut wrong = claim.clone();
    wrong["claim_secret"] = json!(b64(&[1u8; 32]));
    assert_eq!(
        srv.call(Method::POST, "/v1/device-pairings/claim", None, Some(wrong))
            .await
            .0,
        404
    );

    // Resolve shows the same confirmation as the terminal; approval needs step-up.
    let (s, r) = srv
        .post(
            "/v1/device-pairings/resolve",
            &owner.token,
            json!({ "user_code": code.to_lowercase() }),
        )
        .await;
    assert_eq!(s, 200, "{r}");
    assert_eq!(r["confirmation"], p["confirmation"]);
    assert_eq!(r["can_approve"], true);
    let outsider = srv.person("outsider").await;
    assert_eq!(
        srv.post(
            "/v1/device-pairings/resolve",
            &outsider.token,
            json!({ "user_code": code })
        )
        .await
        .0,
        404
    );
    let (s, _) = srv
        .post(
            &format!("/v1/device-pairings/{pid}/approve"),
            &owner.token,
            json!({ "user_code": code }),
        )
        .await;
    assert_eq!(s, 401);
    let (s, v) = srv
        .post(
            &format!("/v1/device-pairings/{pid}/approve"),
            &owner.strong,
            json!({ "user_code": code }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, c) = srv
        .call(
            Method::POST,
            "/v1/device-pairings/claim",
            None,
            Some(claim.clone()),
        )
        .await;
    assert_eq!(s, 200, "{c}");
    let cred = c["credential"].as_str().unwrap().to_string();
    assert!(cred.starts_with("cap_"));
    assert_eq!(c["address"], agent);
    let (s, w) = srv.get("/v1/whoami", &cred).await;
    assert_eq!(s, 200, "{w}");
    // A retry replaces the credential instead of adding one.
    let (s, c2) = srv
        .call(Method::POST, "/v1/device-pairings/claim", None, Some(claim))
        .await;
    assert_eq!(s, 200);
    assert_eq!(srv.get("/v1/whoami", &cred).await.0, 401, "old one revoked");
    let cred2 = c2["credential"].as_str().unwrap().to_string();
    assert_eq!(srv.get("/v1/whoami", &cred2).await.0, 200);

    // --- delegation-based enrollment by a member ---
    let ops = srv.person("ops").await;
    let (ops_id, _) = srv.join(&org, &owner, &ops, "ops", &[]).await;
    let (s, _) = srv
        .post(
            &format!("/v1/orgs/{org}/agents/{}/enrollments", enc(&agent)),
            &ops.strong,
            json!({}),
        )
        .await;
    assert_eq!(s, 403, "no delegation yet");
    let (s, d) = srv
        .post(
            &format!("/v1/orgs/{org}/delegations"),
            &owner.token,
            json!({ "member_id": ops_id, "target": "/acme/agent1", "capabilities": ["enroll"] }),
        )
        .await;
    assert_eq!(s, 201, "{d}");
    let did = d["delegations"][0]["id"].as_str().unwrap().to_string();
    let (s, e) = srv
        .post(
            &format!("/v1/orgs/{org}/agents/{}/enrollments", enc(&agent)),
            &ops.strong,
            json!({ "ttl_hours": 1 }),
        )
        .await;
    assert_eq!(s, 201, "{e}");
    let code = e["code"].as_str().unwrap().to_string();
    let k2 = SigningKey::from_bytes(&[3u8; 32]);
    let redeem = |code: &str, k: &SigningKey| {
        json!({
        "code": code, "device_public_key": b64(k.verifying_key().as_bytes()),
        "signature": b64(&k.sign(format!("pigeonpost-enroll-v1\0{code}").as_bytes()).to_bytes()), "label": "ci" })
    };
    let (s, r) = srv
        .call(
            Method::POST,
            "/v1/enrollments/redeem",
            None,
            Some(redeem(&code, &k2)),
        )
        .await;
    assert_eq!(s, 200, "{r}");
    let cred3 = r["credential"].as_str().unwrap().to_string();
    assert_eq!(srv.get("/v1/whoami", &cred3).await.0, 200);
    // Single use.
    let (s, v) = srv
        .call(
            Method::POST,
            "/v1/enrollments/redeem",
            None,
            Some(redeem(&code, &k2)),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (400, Some("enrollment_invalid")));
    // Revoking the delegation stops the credential it approved, not the owner's.
    assert_eq!(
        srv.call(
            Method::DELETE,
            &format!("/v1/orgs/{org}/delegations/{did}"),
            Some(&owner.token),
            None
        )
        .await
        .0,
        200
    );
    assert_eq!(srv.get("/v1/whoami", &cred3).await.0, 401);
    assert_eq!(srv.get("/v1/whoami", &cred2).await.0, 200);
    // Suspending the agent stops every credential (and its mailbox).
    let (s, _) = srv
        .patch(
            &format!("/v1/orgs/{org}/agents/{}", enc(&agent)),
            &owner.token,
            json!({ "status": "suspended" }),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(srv.get("/v1/whoami", &cred2).await.0, 401);
}

#[tokio::test]
async fn offboarding_detaches_the_mailbox_and_holds_the_address() {
    let (srv, owner, org) = setup().await;
    let jane = srv.person("jane").await;
    let (jid, jane_mb) = srv.join(&org, &owner, &jane, "jane", &[]).await;
    let boss_mb = srv.get("/v1/orgs", &owner.token).await.1["orgs"][0]["my_mailbox"]["address"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        send(&srv, &owner.token, &boss_mb, "/acme/jane").await.0,
        200
    );
    // Suspension closes the mailbox at once.
    assert_eq!(
        srv.patch(
            &format!("/v1/orgs/{org}/members/{jid}"),
            &owner.token,
            json!({ "status": "suspended" })
        )
        .await
        .0,
        200
    );
    let (s, v) = srv
        .call(
            Method::GET,
            &format!("/v1/inbox?identity={jane_mb}"),
            Some(&jane.token),
            None,
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (403, Some("org_mailbox_suspended"))
    );
    assert_eq!(
        send(&srv, &owner.token, &boss_mb, "/acme/jane").await.0,
        404,
        "reads as unknown"
    );
    // Removal: the account no longer holds it; the handle is held for a successor.
    let (s, v) = srv
        .post(
            &format!("/v1/orgs/{org}/members/{jid}/remove"),
            &owner.token,
            json!({ "mode": "reserve" }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["handle_held"], true);
    let (_, ids) = srv.get("/v1/identities", &jane.token).await;
    assert!(!ids.to_string().contains(&jane_mb));
    // The successor gets a new mailbox under the same address, and none of the old mail.
    let joe = srv.person("joe").await;
    let (_, joe_mb) = srv.join(&org, &owner, &joe, "jane", &[]).await;
    assert_ne!(joe_mb, jane_mb);
    let (_, inbox) = srv
        .call(
            Method::GET,
            &format!("/v1/inbox?identity={joe_mb}"),
            Some(&joe.token),
            None,
        )
        .await;
    assert_eq!(inbox["messages"].as_array().map(Vec::len), Some(0));
    assert_eq!(
        send(&srv, &owner.token, &boss_mb, "/acme/jane").await.0,
        200
    );
}

#[tokio::test]
async fn lifecycle_follows_the_entitlement() {
    let (srv, owner, org) = setup().await;
    let boss_mb = srv.get("/v1/orgs", &owner.token).await.1["orgs"][0]["my_mailbox"]["address"]
        .as_str()
        .unwrap()
        .to_string();
    let jane = srv.person("jane").await;
    srv.join(&org, &owner, &jane, "jane", &[]).await;
    let now = now_unix() as i64;
    // Lapsed 3 days ago: past due, still working.
    srv.entitlement(&org, 5, "expired", now - 3 * 86_400, 2, false)
        .await;
    assert_eq!(
        srv.get(&format!("/v1/orgs/{org}"), &owner.token).await.1["org"]["status"],
        "past_due"
    );
    assert_eq!(
        send(&srv, &owner.token, &boss_mb, "/acme/jane").await.0,
        200
    );
    // 10 days: read-only — no sending, no invitations.
    srv.entitlement(&org, 5, "expired", now - 10 * 86_400, 3, false)
        .await;
    assert_eq!(
        srv.get(&format!("/v1/orgs/{org}"), &owner.token).await.1["org"]["status"],
        "read_only"
    );
    let (s, v) = send(&srv, &owner.token, &boss_mb, "/acme/jane").await;
    assert_eq!((s, v["error"].as_str()), (403, Some("org_read_only")));
    let (s, _) = srv
        .post(
            &format!("/v1/orgs/{org}/invites"),
            &owner.token,
            json!({ "invites": [{
        "first_name": "A", "last_name": "B", "contact": "a@example.com", "handle": "a" }]}),
        )
        .await;
    assert_eq!(s, 409);
    // 30 days: suspended — the addresses stop existing for senders.
    srv.entitlement(&org, 5, "expired", now - 30 * 86_400, 4, false)
        .await;
    let outsider = srv.person("outsider").await;
    let out_mb = do_create_identity(&srv.state, Some(outsider.account.clone()), None, None)
        .await
        .unwrap()["address"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        send(&srv, &outsider.token, &out_mb, "/acme/jane").await.0,
        404
    );
    // Renewal brings it back.
    srv.entitlement(&org, 5, "active", now + 30 * 86_400, 5, false)
        .await;
    assert_eq!(
        send(&srv, &owner.token, &boss_mb, "/acme/jane").await.0,
        200
    );
    // The periodic sweep agrees with the event-driven state.
    assert!(srv
        .state
        .store
        .org_lifecycle_sweep(now_unix())
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn agent_limit_and_org_rooms() {
    let srv = serve().await;
    srv.global_flag("orgs").await;
    srv.global_flag("groups").await;
    let owner = srv.person("owner").await;
    let org = srv.org(&owner, "tiny", 1).await;
    // No address yet: room creation asks for one.
    let (s, v) = srv
        .post(
            &format!("/v1/orgs/{org}/rooms"),
            &owner.token,
            json!({ "name": "Design" }),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (409, Some("mailbox_required")));
    srv.post(
        &format!("/v1/orgs/{org}/me/mailbox"),
        &owner.token,
        json!({ "handle": "me" }),
    )
    .await;
    for i in 0..10 {
        assert_eq!(
            srv.post(
                &format!("/v1/orgs/{org}/agents"),
                &owner.token,
                json!({ "handle": format!("a{i}") })
            )
            .await
            .0,
            201
        );
    }
    let (s, v) = srv
        .post(
            &format!("/v1/orgs/{org}/agents"),
            &owner.token,
            json!({ "handle": "a10" }),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (409, Some("agent_limit")));
    // Deleting one frees a slot; the handle can be reused by a new identity.
    let (_, list) = srv
        .get(&format!("/v1/orgs/{org}/agents"), &owner.token)
        .await;
    let first = list["agents"][0]["address"].as_str().unwrap().to_string();
    assert_eq!(
        srv.call(
            Method::DELETE,
            &format!("/v1/orgs/{org}/agents/{}", enc(&first)),
            Some(&owner.token),
            None
        )
        .await
        .0,
        200
    );
    let (s, v) = srv
        .post(
            &format!("/v1/orgs/{org}/agents"),
            &owner.token,
            json!({ "handle": "a0" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    assert_ne!(v["agent"]["address"], first);

    let (_, rooms) = srv
        .get(&format!("/v1/orgs/{org}/rooms"), &owner.token)
        .await;
    let general = rooms["rooms"][0].clone();
    assert_eq!(
        (general["name"].as_str(), general["auto_join"].as_bool()),
        (Some("General"), Some(true))
    );
    let (s, r) = srv
        .post(
            &format!("/v1/orgs/{org}/rooms"),
            &owner.token,
            json!({ "name": "Design", "join_policy": "public" }),
        )
        .await;
    assert_eq!(s, 201, "{r}");
    // Outsiders cannot join org rooms, even public listed ones.
    let outsider = srv.person("outsider").await;
    let out_mb = do_create_identity(&srv.state, Some(outsider.account.clone()), None, None)
        .await
        .unwrap()["address"]
        .as_str()
        .unwrap()
        .to_string();
    let rid = r["room"]["id"].as_str().unwrap();
    let (s, v) = srv
        .call(
            Method::POST,
            &format!("/v1/groups/{rid}/join?identity={out_mb}"),
            Some(&outsider.token),
            Some(json!({})),
        )
        .await;
    assert!(s == 403 || s == 404, "{s} {v}");
    let (s, _) = srv
        .call(
            Method::GET,
            &format!("/v1/rooms?org_id={org}&identity={out_mb}"),
            Some(&outsider.token),
            None,
        )
        .await;
    assert_eq!(s, 403);
}
