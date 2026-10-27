//! Phase 5 end-to-end tests over real sockets: org API keys and their authorization matrix,
//! attempts to exceed authority, conventions (idempotency, pagination, rate limits, errors), the
//! messages:send boundary, audit of every use, and schema-driven contract checks of every response
//! against `openapi/pigeonpost-org.v1.yaml` (through its JSON mirror).

use crate::org_tests::{enc, serve, serve_with, setup, Person, Srv};
use crate::*;
use reqwest::Method;
use serde_json::{json, Value};

const SPEC: &str = include_str!("../../../openapi/pigeonpost-org.v1.json");

fn spec() -> Value {
    serde_json::from_str(SPEC).expect("the org spec's JSON mirror parses")
}

// ---- a small, strict schema checker (the subset the spec uses) ---------------------------------

fn resolve<'a>(spec: &'a Value, schema: &'a Value) -> &'a Value {
    match schema.get("$ref").and_then(Value::as_str) {
        Some(r) => {
            let mut node = spec;
            for part in r.trim_start_matches("#/").split('/') {
                node = &node[part];
            }
            resolve(spec, node)
        }
        None => schema,
    }
}

fn type_ok(t: &str, v: &Value) -> bool {
    match t {
        "null" => v.is_null(),
        "string" => v.is_string(),
        "integer" => v.is_i64() || v.is_u64(),
        "number" => v.is_number(),
        "boolean" => v.is_boolean(),
        "array" => v.is_array(),
        "object" => v.is_object(),
        _ => true,
    }
}

fn check(spec: &Value, schema: &Value, v: &Value, at: &str, errs: &mut Vec<String>) {
    let s = resolve(spec, schema);
    if let Some(all) = s.get("allOf").and_then(Value::as_array) {
        for sub in all {
            check(spec, sub, v, at, errs);
        }
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(any) = s.get(key).and_then(Value::as_array) {
            let ok = any.iter().any(|sub| {
                let mut e = Vec::new();
                check(spec, sub, v, at, &mut e);
                e.is_empty()
            });
            if !ok {
                errs.push(format!("{at}: matches none of {key}"));
            }
        }
    }
    match s.get("type") {
        Some(Value::String(t)) if !type_ok(t, v) => {
            errs.push(format!("{at}: expected {t}, got {v}"));
            return;
        }
        Some(Value::Array(ts)) if !ts.iter().any(|t| t.as_str().is_some_and(|t| type_ok(t, v))) => {
            errs.push(format!("{at}: expected one of {ts:?}, got {v}"));
            return;
        }
        _ => {}
    }
    if let Some(e) = s.get("enum").and_then(Value::as_array) {
        if !v.is_null() && !e.contains(v) {
            errs.push(format!("{at}: {v} is not in {e:?}"));
        }
    }
    if let Some(obj) = v.as_object() {
        if let Some(req) = s.get("required").and_then(Value::as_array) {
            for r in req.iter().filter_map(Value::as_str) {
                if !obj.contains_key(r) {
                    errs.push(format!("{at}: missing required '{r}'"));
                }
            }
        }
        let props = s.get("properties").and_then(Value::as_object);
        for (k, val) in obj {
            match props.and_then(|p| p.get(k)) {
                Some(ps) => check(spec, ps, val, &format!("{at}.{k}"), errs),
                None => {
                    // allOf members each list part of the properties; only judge extras where the
                    // schema itself closes the object.
                    if s.get("additionalProperties") == Some(&Value::Bool(false)) {
                        errs.push(format!("{at}: unexpected property '{k}'"));
                    }
                }
            }
        }
    }
    if let (Some(items), Some(arr)) = (s.get("items"), v.as_array()) {
        for (i, item) in arr.iter().enumerate() {
            check(spec, items, item, &format!("{at}[{i}]"), errs);
        }
    }
}

/// The operation's documented response schema for `status`, checked against `body`.
fn contract(spec: &Value, method: &Method, path: &str, status: u16, body: &Value) {
    let template = template_for(spec, path).unwrap_or_else(|| panic!("no spec path for {path}"));
    let op = &spec["paths"][&template][method.as_str().to_ascii_lowercase()];
    assert!(op.is_object(), "{method} {template} is not in the spec");
    let resp = resolve(spec, &op["responses"][status.to_string()]);
    assert!(
        resp.is_object(),
        "{method} {template} does not document status {status} (body {body})"
    );
    let schema = &resp["content"]["application/json"]["schema"];
    if schema.is_null() {
        return;
    }
    let mut errs = Vec::new();
    check(spec, schema, body, "$", &mut errs);
    assert!(
        errs.is_empty(),
        "{method} {template} {status} does not match the spec:\n{}\nbody: {body}",
        errs.join("\n")
    );
}

/// Match a concrete `/org/v1/...` path to its spec template.
fn template_for(spec: &Value, path: &str) -> Option<String> {
    let path = path.trim_start_matches("/org/v1");
    let path = path.split('?').next().unwrap_or(path);
    let segs: Vec<&str> = path.split('/').collect();
    spec["paths"].as_object()?.keys().find_map(|t| {
        let ts: Vec<&str> = t.split('/').collect();
        (ts.len() == segs.len()
            && ts
                .iter()
                .zip(&segs)
                .all(|(a, b)| a == b || (a.starts_with('{') && a.ends_with('}'))))
        .then(|| t.clone())
    })
}

// ---- the client --------------------------------------------------------------------------------

pub(crate) struct Resp {
    pub status: u16,
    pub headers: reqwest::header::HeaderMap,
    pub body: Value,
}

pub(crate) async fn api(
    srv: &Srv,
    method: Method,
    path: &str,
    key: Option<&str>,
    body: Option<Value>,
    idem: Option<&str>,
) -> Resp {
    let mut req = srv
        .http
        .request(method.clone(), format!("{}{path}", srv.public));
    if let Some(k) = key {
        req = req.bearer_auth(k);
    }
    if let Some(i) = idem {
        req = req.header("Idempotency-Key", i);
    }
    if let Some(b) = body {
        req = req.json(&b);
    }
    let r = req.send().await.unwrap();
    let status = r.status().as_u16();
    let headers = r.headers().clone();
    let text = r.text().await.unwrap();
    let body: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
    // Every answer is in the documented shape: the success schema, or the error schema.
    let spec = spec();
    if status < 300 {
        contract(&spec, &method, path, status, &body);
        assert!(
            headers
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.starts_with("application/json")),
            "{path}: json content type"
        );
    } else if path.starts_with("/org/v1") && status != 405 {
        let mut errs = Vec::new();
        check(
            &spec,
            &json!({ "$ref": "#/components/schemas/Error" }),
            &body,
            "$",
            &mut errs,
        );
        assert!(
            errs.is_empty(),
            "{path} {status} error shape: {errs:?} {body}"
        );
        let code = body["error"]["code"].as_str().unwrap_or_default();
        assert_eq!(
            body["error"]["doc_url"],
            format!("https://developers.pigeonpost.dev/org-api-errors#{code}")
        );
        let catalog = spec["x-error-codes"]
            .as_object()
            .map(|o| o.contains_key(code));
        assert_ne!(catalog, Some(false), "error code {code} is not documented");
    }
    Resp {
        status,
        headers,
        body,
    }
}

pub(crate) async fn get(srv: &Srv, path: &str, key: &str) -> Resp {
    api(
        srv,
        Method::GET,
        &format!("/org/v1{path}"),
        Some(key),
        None,
        None,
    )
    .await
}
pub(crate) async fn post(srv: &Srv, path: &str, key: &str, body: Value) -> Resp {
    api(
        srv,
        Method::POST,
        &format!("/org/v1{path}"),
        Some(key),
        Some(body),
        None,
    )
    .await
}
pub(crate) async fn patch(srv: &Srv, path: &str, key: &str, body: Value) -> Resp {
    api(
        srv,
        Method::PATCH,
        &format!("/org/v1{path}"),
        Some(key),
        Some(body),
        None,
    )
    .await
}
pub(crate) async fn del(srv: &Srv, path: &str, key: &str) -> Resp {
    api(
        srv,
        Method::DELETE,
        &format!("/org/v1{path}"),
        Some(key),
        None,
        None,
    )
    .await
}

pub(crate) fn code(r: &Resp) -> (u16, &str) {
    (r.status, r.body["error"]["code"].as_str().unwrap_or(""))
}

const ALL: &[&str] = &[
    "org:read",
    "members:write",
    "agents:write",
    "rules:write",
    "rooms:write",
    "messages:send",
    "audit:read",
    "webhooks:write",
];

pub(crate) async fn api_setup() -> (Srv, Person, String) {
    let (srv, owner, org) = setup().await;
    srv.global_flag("org_api").await;
    (srv, owner, org)
}

/// Create a key on the dashboard as `who` (strong sign-in). Returns (key id, secret).
pub(crate) async fn make_key(
    srv: &Srv,
    org: &str,
    who: &Person,
    scopes: &[&str],
    extra: Value,
) -> (String, String) {
    let mut body = json!({ "name": format!("key with {} scopes", scopes.len()), "scopes": scopes });
    if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            b.insert(k.clone(), v.clone());
        }
    }
    let (s, v) = srv
        .post(&format!("/v1/orgs/{org}/api-keys"), &who.strong, body)
        .await;
    assert_eq!(s, 201, "create key: {v}");
    (
        v["key"]["id"].as_str().unwrap().to_string(),
        v["secret"].as_str().unwrap().to_string(),
    )
}

pub(crate) async fn make_agent(srv: &Srv, org: &str, owner: &Person, handle: &str) -> String {
    let (s, v) = srv
        .post(
            &format!("/v1/orgs/{org}/agents"),
            &owner.token,
            json!({ "handle": handle }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    v["agent"]["address"].as_str().unwrap().to_string()
}

// ---- keys --------------------------------------------------------------------------------------

#[tokio::test]
async fn keys_need_step_up_are_shown_once_and_hashed_at_rest() {
    let (srv, owner, org) = api_setup().await;
    let path = format!("/v1/orgs/{org}/api-keys");
    // A person, but not a strong recent sign-in: the RFC 9470 challenge.
    let (s, v) = srv
        .post(
            &path,
            &owner.token,
            json!({ "name": "ci", "scopes": ["org:read"] }),
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (401, Some("step_up_required")),
        "{v}"
    );
    // Automation can never create keys, step-up or not.
    let (s, _) = srv
        .post(
            &path,
            "pk_live_nothing",
            json!({ "name": "ci", "scopes": ["org:read"] }),
        )
        .await;
    assert_eq!(s, 401);

    // Validation.
    for (body, want) in [
        (json!({ "name": "x", "scopes": [] }), "invalid_scope"),
        (
            json!({ "name": "x", "scopes": ["org:write"] }),
            "invalid_scope",
        ),
        (
            json!({ "name": "x", "scopes": ["messages:send"] }),
            "send_as_required",
        ),
        (
            json!({ "name": "x", "scopes": ["org:read"], "send_as": "/acme/x" }),
            "bad_request",
        ),
        (
            json!({ "name": "x", "scopes": ["org:read"], "ip_allowlist": ["nope"] }),
            "invalid_ip_allowlist",
        ),
        (
            json!({ "name": "x", "scopes": ["org:read"], "expires_in_days": 0 }),
            "bad_request",
        ),
        (
            json!({ "name": "x", "scopes": ["messages:send"], "send_as": "/acme/nobody" }),
            "invalid_send_as",
        ),
    ] {
        let (s, v) = srv.post(&path, &owner.strong, body.clone()).await;
        assert_eq!((s, v["error"].as_str()), (400, Some(want)), "{body} → {v}");
    }

    let (s, v) = srv
        .post(
            &path,
            &owner.strong,
            json!({ "name": "HR sync", "scopes": ["org:read", "members:write"],
            "expires_in_days": 30, "ip_allowlist": ["127.0.0.1", "2001:db8::/32"] }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let secret = v["secret"].as_str().unwrap().to_string();
    assert!(secret.starts_with("pk_org_") && secret.len() == 39);
    assert_eq!(v["key"]["last4"], secret[secret.len() - 4..]);
    assert_eq!(v["key"]["created_by"], "/acme/boss");
    assert_eq!(
        v["key"]["ip_allowlist"],
        json!(["127.0.0.1/32", "2001:db8::/32"])
    );
    let kid = v["key"]["id"].as_str().unwrap().to_string();

    // Listing never shows the secret again, and the database holds only its SHA-256.
    let (_, list) = srv.get(&path, &owner.token).await;
    assert!(!list.to_string().contains(&secret));
    assert_eq!(list["keys"][0]["id"], kid);
    let (hash, scopes): (Vec<u8>, String) = srv
        .state
        .store
        .group_read({
            let kid = kid.clone();
            move |c: &rusqlite::Connection| -> crate::groups::GResult<(Vec<u8>, String)> {
                Ok(c.query_row(
                    "SELECT key_hash, scopes FROM org_api_keys WHERE id = ?1",
                    [&kid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            }
        })
        .await
        .unwrap();
    assert_eq!(hash, sha256(secret.as_bytes()).to_vec());
    assert_eq!(scopes, r#"["org:read","members:write"]"#);

    // It works, describes itself, and audits creation with the key id.
    let me = get(&srv, "/key", &secret).await;
    assert_eq!(me.status, 200);
    assert_eq!(me.body["data"]["id"], kid);
    assert_eq!(me.body["data"]["org_id"], org);
    let (_, audit) = srv
        .get(
            &format!("/v1/orgs/{org}/audit?action=api_key.create"),
            &owner.token,
        )
        .await;
    assert_eq!(audit["entries"][0]["target"], kid);
    assert!(!audit.to_string().contains(&secret));

    // Revocation on the dashboard is immediate.
    let (s, _) = srv
        .call(
            Method::DELETE,
            &format!("{path}/{kid}"),
            Some(&owner.token),
            None,
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(
        code(&get(&srv, "/key", &secret).await),
        (401, "unauthorized")
    );
    let (s, v) = srv
        .call(
            Method::DELETE,
            &format!("{path}/{kid}"),
            Some(&owner.token),
            None,
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (404, Some("not_found")));
}

#[tokio::test]
async fn key_creation_without_configured_step_up_is_refused_not_bypassed() {
    let srv = serve_with(|s| s.step_up = Arc::new(principal::StepUpPolicy::default())).await;
    srv.global_flag("orgs").await;
    srv.global_flag("org_api").await;
    let owner = srv.person("owner").await;
    let org = srv.org(&owner, "acme", 5).await;
    let (s, v) = srv
        .post(
            &format!("/v1/orgs/{org}/api-keys"),
            &owner.strong,
            json!({ "name": "ci", "scopes": ["org:read"] }),
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (403, Some("step_up_unavailable")),
        "{v}"
    );
}

#[tokio::test]
async fn only_owners_and_admins_manage_keys_and_the_flag_gates_everything() {
    let (srv, owner, org) = setup().await;
    // Flag off: dashboard routes and the API are both closed.
    let (s, v) = srv
        .get(&format!("/v1/orgs/{org}/api-keys"), &owner.token)
        .await;
    assert_eq!((s, v["error"].as_str()), (404, Some("feature_disabled")));
    srv.global_flag("org_api").await;
    let (_, secret) = make_key(&srv, &org, &owner, &["org:read"], json!({})).await;
    srv.call(
        Method::PUT,
        "/internal/v1/features/org_api",
        Some(crate::org_tests::OPS),
        Some(json!({ "scope": "global", "enabled": false })),
    )
    .await;
    assert_eq!(
        code(&get(&srv, "/org", &secret).await),
        (404, "feature_disabled")
    );
    srv.global_flag("org_api").await;
    assert_eq!(get(&srv, "/org", &secret).await.status, 200);

    // A people admin may not see or create keys.
    let pa = srv.person("pa").await;
    srv.join(&org, &owner, &pa, "pa", &["people_admin"]).await;
    let (s, _) = srv
        .get(&format!("/v1/orgs/{org}/api-keys"), &pa.token)
        .await;
    assert_eq!(s, 403);
    let (s, _) = srv
        .post(
            &format!("/v1/orgs/{org}/api-keys"),
            &pa.strong,
            json!({ "name": "x", "scopes": ["org:read"] }),
        )
        .await;
    assert_eq!(s, 403);
}

// ---- the authorization matrix ------------------------------------------------------------------

/// Every scope against every operation: an operation succeeds exactly when the key holds its
/// scope, and otherwise answers `insufficient_scope` before anything happens.
#[tokio::test]
async fn scopes_against_actions() {
    let (srv, owner, org) = api_setup().await;
    let agent = make_agent(&srv, &org, &owner, "bot").await;
    let _ = agent;
    // (method, path, body, scope)
    let ops: Vec<(Method, &str, Option<Value>, &str)> = vec![
        (Method::GET, "/org", None, "org:read"),
        (Method::GET, "/members", None, "org:read"),
        (Method::GET, "/members/boss", None, "org:read"),
        (Method::GET, "/roles", None, "org:read"),
        (Method::GET, "/agents", None, "org:read"),
        (Method::GET, "/agents/bot", None, "org:read"),
        (Method::GET, "/agents/bot/delegations", None, "org:read"),
        (Method::GET, "/agents/bot/enrollments", None, "org:read"),
        (Method::GET, "/rules", None, "org:read"),
        (
            Method::POST,
            "/rules/evaluate",
            Some(json!({ "from": "/acme/bot", "to": "/acme/boss" })),
            "org:read",
        ),
        (Method::GET, "/rooms", None, "org:read"),
        (Method::GET, "/usage", None, "org:read"),
        (Method::GET, "/billing", None, "org:read"),
        (Method::GET, "/webhooks", None, "org:read"),
        (Method::GET, "/audit", None, "audit:read"),
        (
            Method::POST,
            "/members",
            Some(
                json!({ "first_name": "A", "last_name": "B", "contact": "a@example.com", "handle": "aa" }),
            ),
            "members:write",
        ),
        (
            Method::POST,
            "/agents",
            Some(json!({ "handle": "made-by-key" })),
            "agents:write",
        ),
        (
            Method::POST,
            "/rules",
            Some(json!({ "owners": "agents", "peers": "*", "admission": "block" })),
            "rules:write",
        ),
        (
            Method::POST,
            "/rooms",
            Some(json!({ "name": "Builds", "owner": "boss" })),
            "rooms:write",
        ),
        (
            Method::POST,
            "/messages",
            Some(json!({ "to": "/acme/boss", "body": "build green" })),
            "messages:send",
        ),
    ];
    let mut keys = Vec::new();
    for s in ALL {
        let extra = if *s == "messages:send" {
            json!({ "send_as": "bot" })
        } else {
            json!({})
        };
        keys.push((*s, make_key(&srv, &org, &owner, &[s], extra).await.1));
    }
    for (method, path, body, need) in &ops {
        for (scope, key) in &keys {
            let r = api(
                &srv,
                method.clone(),
                &format!("/org/v1{path}"),
                Some(key),
                body.clone(),
                None,
            )
            .await;
            if scope == need {
                assert!(
                    r.status < 300,
                    "{scope} {method} {path}: {} {}",
                    r.status,
                    r.body
                );
            } else {
                assert_eq!(
                    code(&r),
                    (403, "insufficient_scope"),
                    "{scope} {method} {path}: {}",
                    r.body
                );
            }
        }
    }
    // GET /key needs no scope; nothing else is reachable without one.
    for (_, key) in &keys {
        assert_eq!(get(&srv, "/key", key).await.status, 200);
    }
}

/// A key works only within its creator's live authority: demote the creator and every key they
/// made stops; restore them and an unrevoked key works again.
#[tokio::test]
async fn keys_never_exceed_their_creators_live_authority() {
    let (srv, owner, org) = api_setup().await;
    let adm = srv.person("adm").await;
    let (adm_id, _) = srv.join(&org, &owner, &adm, "adm", &[]).await;
    let (s, v) = srv
        .patch(
            &format!("/v1/orgs/{org}/members/{adm_id}"),
            &owner.strong,
            json!({ "roles": ["admin"] }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (_, key) = make_key(&srv, &org, &adm, &["org:read", "agents:write"], json!({})).await;
    assert_eq!(get(&srv, "/agents", &key).await.status, 200);

    // Demoted to agent admin: they may no longer administer keys at all.
    let (s, _) = srv
        .patch(
            &format!("/v1/orgs/{org}/members/{adm_id}"),
            &owner.strong,
            json!({ "roles": ["agent_admin"] }),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(code(&get(&srv, "/agents", &key).await), (403, "forbidden"));
    // Restored: works again (it was never revoked).
    srv.patch(
        &format!("/v1/orgs/{org}/members/{adm_id}"),
        &owner.strong,
        json!({ "roles": ["admin"] }),
    )
    .await;
    assert_eq!(get(&srv, "/agents", &key).await.status, 200);
    // Suspended creator: refused.
    srv.patch(
        &format!("/v1/orgs/{org}/members/{adm_id}"),
        &owner.token,
        json!({ "status": "suspended" }),
    )
    .await;
    assert_eq!(code(&get(&srv, "/agents", &key).await), (403, "forbidden"));

    // An admin cannot mint a key their role does not cover: billing is owner/billing-admin only,
    // but every listed scope maps onto admin authority, so the check is the role gate itself.
    let (s, _) = srv
        .post(
            &format!("/v1/orgs/{org}/api-keys"),
            &adm.strong,
            json!({ "name": "x", "scopes": ["org:read"] }),
        )
        .await;
    assert_eq!(s, 403, "a suspended member has no dashboard at all");
}

#[tokio::test]
async fn revoked_expired_and_wrong_ip_keys_are_refused() {
    let (srv, owner, org) = api_setup().await;
    let (kid, key) = make_key(&srv, &org, &owner, &["org:read"], json!({})).await;
    // Unknown, malformed, account keys and member tokens are all one answer.
    for bad in [
        "pk_org_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "pk_org_short",
        "pk_live_x",
        owner.token.as_str(),
    ] {
        assert_eq!(
            code(&get(&srv, "/org", bad).await),
            (401, "unauthorized"),
            "{bad}"
        );
    }
    let r = api(&srv, Method::GET, "/org/v1/org", None, None, None).await;
    assert_eq!(code(&r), (401, "unauthorized"));
    assert!(r.headers.get("www-authenticate").is_some());

    // Expired.
    {
        let kid = kid.clone();
        srv.state
            .store
            .group_tx(move |tx| -> crate::groups::GResult<()> {
                tx.execute(
                    "UPDATE org_api_keys SET expires_at = 1 WHERE id = ?1",
                    [&kid],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }
    assert_eq!(code(&get(&srv, "/org", &key).await), (401, "unauthorized"));

    // Wrong address: the test client is 127.0.0.1.
    let (_, fenced) = make_key(
        &srv,
        &org,
        &owner,
        &["org:read"],
        json!({ "ip_allowlist": ["203.0.113.0/24"] }),
    )
    .await;
    assert_eq!(
        code(&get(&srv, "/org", &fenced).await),
        (403, "ip_not_allowed")
    );
    let (_, home) = make_key(
        &srv,
        &org,
        &owner,
        &["org:read"],
        json!({ "ip_allowlist": ["127.0.0.0/8"] }),
    )
    .await;
    assert_eq!(get(&srv, "/org", &home).await.status, 200);

    // Self-revocation.
    let r = del(&srv, "/key", &home).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(code(&get(&srv, "/org", &home).await), (401, "unauthorized"));
}

/// The things a key can never do, however it is scoped.
#[tokio::test]
async fn keys_cannot_escalate_read_content_or_take_human_actions() {
    let (srv, owner, org) = api_setup().await;
    make_agent(&srv, &org, &owner, "bot").await;
    let (_, key) = make_key(&srv, &org, &owner, ALL, json!({ "send_as": "bot" })).await;
    let pa = srv.person("pa").await;
    let (pa_id, _) = srv.join(&org, &owner, &pa, "pa", &["people_admin"]).await;
    let jane = srv.person("jane").await;
    let (jane_id, jane_mb) = srv.join(&org, &owner, &jane, "jane", &[]).await;

    // Role grants: on invite, on patch.
    let r = post(
        &srv,
        "/members",
        &key,
        json!({ "first_name": "E", "last_name": "V", "contact": "eve@example.com",
        "handle": "eve", "roles": ["admin"] }),
    )
    .await;
    assert_eq!(code(&r), (403, "human_required"));
    let r = patch(
        &srv,
        &format!("/members/{jane_id}"),
        &key,
        json!({ "roles": ["people_admin"] }),
    )
    .await;
    assert_eq!(code(&r), (403, "human_required"));
    // Admin-ranked people and the owner are out of reach.
    for target in [pa_id.as_str(), "boss"] {
        for path in [format!("/members/{target}/suspend")] {
            assert_eq!(
                code(&post(&srv, &path, &key, json!({})).await),
                (403, "forbidden"),
                "{path}"
            );
        }
        assert_eq!(
            code(&del(&srv, &format!("/members/{target}"), &key).await),
            (403, "forbidden")
        );
        assert_eq!(
            code(
                &patch(
                    &srv,
                    &format!("/members/{target}"),
                    &key,
                    json!({ "first_name": "X" })
                )
                .await
            ),
            (403, "forbidden")
        );
    }
    // So is a pending invitation that will make someone an admin.
    let (s, v) = srv
        .post(
            &format!("/v1/orgs/{org}/invites"),
            &owner.token,
            json!({ "invites": [{
        "first_name": "Ann", "last_name": "Admin", "contact": "ann@example.com", "handle": "ann",
        "roles": ["people_admin"] }]}),
        )
        .await;
    assert_eq!(
        (s, v["results"][0]["status"].as_str()),
        (200, Some("invited")),
        "{v}"
    );
    assert_eq!(
        code(&del(&srv, "/members/ann", &key).await),
        (403, "forbidden")
    );
    assert_eq!(
        code(&post(&srv, "/members/ann/resend-invite", &key, json!({})).await),
        (403, "forbidden")
    );
    // Delegations are human-only.
    let r = post(
        &srv,
        "/agents",
        &key,
        json!({ "handle": "bot2", "operators": [jane_id] }),
    )
    .await;
    assert_eq!(code(&r), (403, "human_required"));
    // Trust-raising rules are human-only; restrictive ones are fine.
    for body in [
        json!({ "owners": "/acme/*", "peers": "*", "admission": "allow" }),
        json!({ "owners": "agents", "peers": "/acme/*", "known": true }),
        json!({ "owners": "agents", "peers": "/acme/jane", "autonomy": "auto" }),
        json!({ "owners": "agents", "peers": "/acme/jane", "verbs": ["status"] }),
        json!({ "owners": "agents", "peers": "*", "admission": "block", "locked": true }),
    ] {
        let r = post(&srv, "/rules", &key, body.clone()).await;
        assert!(
            matches!(code(&r), (403, "human_required") | (400, _)),
            "{body} → {}",
            r.body
        );
    }
    let r = post(
        &srv,
        "/rules",
        &key,
        json!({ "owners": "agents", "peers": "*", "admission": "block", "priority": 10 }),
    )
    .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let key_rule = r.body["data"]["id"].as_str().unwrap().to_string();
    // …and may be widened only within "restrictive".
    let r = patch(
        &srv,
        &format!("/rules/{key_rule}"),
        &key,
        json!({ "admission": "allow" }),
    )
    .await;
    assert_eq!(code(&r), (403, "human_required"));
    let r = patch(
        &srv,
        &format!("/rules/{key_rule}"),
        &key,
        json!({ "autonomy": "review" }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    // A person's rules: their restriction cannot be lifted or changed by a key; their grant may go.
    let (_, block) = srv
        .post(
            &format!("/v1/orgs/{org}/rules"),
            &owner.token,
            json!({ "owners": "agents", "peers": "/acme/jane", "admission": "block" }),
        )
        .await;
    let block = block["rule"]["id"].as_str().unwrap().to_string();
    assert_eq!(
        code(&del(&srv, &format!("/rules/{block}"), &key).await),
        (403, "human_required")
    );
    assert_eq!(
        code(
            &patch(
                &srv,
                &format!("/rules/{block}"),
                &key,
                json!({ "priority": 5 })
            )
            .await
        ),
        (403, "human_required")
    );
    let (_, grant) = srv
        .post(
            &format!("/v1/orgs/{org}/rules"),
            &owner.token,
            json!({ "owners": "agents", "peers": "/acme/jane", "known": true }),
        )
        .await;
    let grant = grant["rule"]["id"].as_str().unwrap().to_string();
    assert_eq!(
        del(&srv, &format!("/rules/{grant}"), &key).await.status,
        200
    );
    assert_eq!(
        del(&srv, &format!("/rules/{key_rule}"), &key).await.status,
        200
    );

    // No enrollment codes or device credentials from a key: there is no such API route, and the
    // dashboard routes refuse the key outright.
    let r = api(
        &srv,
        Method::POST,
        "/org/v1/agents/bot/enrollments",
        Some(&key),
        Some(json!({})),
        None,
    )
    .await;
    assert_eq!(r.status, 405);
    for (method, path) in [
        (
            Method::POST,
            format!("/v1/orgs/{org}/agents/{}/enrollments", enc("/acme/bot")),
        ),
        (Method::POST, format!("/v1/orgs/{org}/api-keys")),
        (Method::POST, format!("/v1/orgs/{org}/delegations")),
        (Method::GET, format!("/v1/orgs/{org}/audit/export")),
        (Method::DELETE, format!("/v1/orgs/{org}")),
        (Method::POST, "/v1/device-pairings/x/approve".to_string()),
    ] {
        let (s, _) = srv
            .call(method.clone(), &path, Some(&key), Some(json!({})))
            .await;
        assert!(
            s == 401 || s == 404,
            "{method} {path} answered {s} to an org key"
        );
    }
    // No message reading: the member API refuses the key, and the org API has no read route.
    for path in [
        "/v1/inbox",
        "/v1/threads",
        &format!("/v1/inbox?identity={jane_mb}"),
        "/v1/groups",
        "/v1/events",
    ] {
        let (s, _) = srv.call(Method::GET, path, Some(&key), None).await;
        assert_eq!(s, 401, "{path}");
    }
    let spec = spec();
    for (path, item) in spec["paths"].as_object().unwrap() {
        if item.get("get").is_some() {
            assert!(
                !path.contains("message") && !path.contains("inbox") && !path.contains("thread"),
                "{path}"
            );
        }
    }
}

// ---- members end to end ------------------------------------------------------------------------

#[tokio::test]
async fn member_lifecycle_through_the_api() {
    let (srv, owner, org) = api_setup().await;
    let (_, key) = make_key(
        &srv,
        &org,
        &owner,
        &["org:read", "members:write"],
        json!({}),
    )
    .await;
    let r = post(
        &srv,
        "/members",
        &key,
        json!({ "first_name": "Jane", "last_name": "Doe",
        "contact": "Jane@Example.com", "handle": "jane" }),
    )
    .await;
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(r.headers.get("cache-control").unwrap(), "no-store");
    let mid = r.body["data"]["member"]["id"].as_str().unwrap().to_string();
    let link = r.body["data"]["invitation"]["link"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(r.body["data"]["member"]["status"], "invited");
    assert_eq!(
        r.body["data"]["member"]["contact_masked"],
        "j***@example.com"
    );
    // Duplicates and capacity are the dashboard's rules.
    let again = post(
        &srv,
        "/members",
        &key,
        json!({ "first_name": "J", "last_name": "D",
        "contact": "jane@example.com", "handle": "jane2" }),
    )
    .await;
    assert_eq!(code(&again), (409, "already_invited"));
    let taken = post(
        &srv,
        "/members",
        &key,
        json!({ "first_name": "J", "last_name": "D",
        "contact": "j2@example.com", "handle": "jane" }),
    )
    .await;
    assert_eq!(code(&taken), (409, "handle_taken"));

    // Accept, then manage by name and by id.
    let jane = srv.person("jane").await;
    let (s, v) = srv
        .post(
            "/v1/org-invites/accept",
            &jane.token,
            json!({ "token": crate::org_tests::token_of(&link) }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        get(&srv, "/members/jane", &key).await.body["data"]["status"],
        "active"
    );
    let r = patch(
        &srv,
        "/members/jane",
        &key,
        json!({ "first_name": "Janet" }),
    )
    .await;
    assert_eq!(r.body["data"]["first_name"], "Janet");
    let r = post(&srv, &format!("/members/{mid}/suspend"), &key, json!({})).await;
    assert_eq!(r.body["data"]["status"], "suspended", "{}", r.body);
    let r = post(&srv, &format!("/members/{mid}/reactivate"), &key, json!({})).await;
    assert_eq!(r.body["data"]["status"], "active");
    let r = post(
        &srv,
        &format!("/members/{mid}/resend-invite"),
        &key,
        json!({}),
    )
    .await;
    assert_eq!(code(&r), (409, "invite_used"));
    let r = del(&srv, "/members/jane?mode=delete", &key).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        code(&get(&srv, &format!("/members/{mid}"), &key).await),
        (404, "not_found")
    );

    // Resend for a pending invitation rotates the link.
    let r = post(
        &srv,
        "/members",
        &key,
        json!({ "first_name": "Kim", "last_name": "Lee",
        "contact": "kim@example.com", "handle": "kim" }),
    )
    .await;
    let old = r.body["data"]["invitation"]["link"]
        .as_str()
        .unwrap()
        .to_string();
    let r = post(&srv, "/members/kim/resend-invite", &key, json!({})).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_ne!(r.body["data"]["invitation"]["link"].as_str().unwrap(), old);
    let (s, _) = srv
        .call(
            Method::POST,
            "/v1/org-invites/preview",
            None,
            Some(json!({ "token": crate::org_tests::token_of(&old) })),
        )
        .await;
    assert_eq!(s, 404, "the old link stopped working");

    // Roles are listed, never written.
    let r = get(&srv, "/roles", &key).await;
    let owners = r.body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["role"] == "owner")
        .unwrap()
        .clone();
    assert_eq!(owners["members"][0]["handle"], "/acme/boss");
}

// ---- conventions -------------------------------------------------------------------------------

#[tokio::test]
async fn idempotency_keys_replay_and_refuse_conflicts() {
    let (srv, owner, org) = api_setup().await;
    let (_, key) = make_key(&srv, &org, &owner, &["org:read", "agents:write"], json!({})).await;
    let body = json!({ "handle": "builder", "label": "CI" });
    let a = api(
        &srv,
        Method::POST,
        "/org/v1/agents",
        Some(&key),
        Some(body.clone()),
        Some("create-builder-1"),
    )
    .await;
    assert_eq!(a.status, 201, "{}", a.body);
    assert!(a.headers.get("idempotent-replayed").is_none());
    let b = api(
        &srv,
        Method::POST,
        "/org/v1/agents",
        Some(&key),
        Some(body.clone()),
        Some("create-builder-1"),
    )
    .await;
    assert_eq!(b.status, 201);
    assert_eq!(b.headers.get("idempotent-replayed").unwrap(), "true");
    assert_eq!(a.body, b.body);
    let list = get(&srv, "/agents", &key).await;
    assert_eq!(
        list.body["data"].as_array().unwrap().len(),
        1,
        "created once"
    );
    // Same key, different request.
    let c = api(
        &srv,
        Method::POST,
        "/org/v1/agents",
        Some(&key),
        Some(json!({ "handle": "other" })),
        Some("create-builder-1"),
    )
    .await;
    assert_eq!(code(&c), (409, "idempotency_conflict"));
    // Another key's record is not this key's.
    let (_, key2) = make_key(&srv, &org, &owner, &["agents:write"], json!({})).await;
    let d = api(
        &srv,
        Method::POST,
        "/org/v1/agents",
        Some(&key2),
        Some(body.clone()),
        Some("create-builder-1"),
    )
    .await;
    assert_eq!(code(&d), (409, "handle_taken"));
    // Failures are not recorded: fixing the request under the same key works.
    let e = api(
        &srv,
        Method::POST,
        "/org/v1/agents",
        Some(&key2),
        Some(json!({ "handle": "fresh" })),
        Some("k2-1"),
    )
    .await;
    assert_eq!(e.status, 201);
    // A malformed key is refused.
    let f = api(
        &srv,
        Method::POST,
        "/org/v1/agents",
        Some(&key),
        Some(json!({ "handle": "x2" })),
        Some(&"k".repeat(300)),
    )
    .await;
    assert_eq!(code(&f), (400, "bad_request"));
    // Records expire after 24 h.
    srv.state
        .store
        .group_tx(|tx| -> crate::groups::GResult<()> {
            tx.execute(
                "UPDATE org_api_idempotency SET created_at = created_at - 90000",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let g = api(
        &srv,
        Method::POST,
        "/org/v1/agents",
        Some(&key),
        Some(body),
        Some("create-builder-1"),
    )
    .await;
    assert_eq!(
        code(&g),
        (409, "handle_taken"),
        "an expired record is not replayed"
    );
}

#[tokio::test]
async fn lists_paginate_with_opaque_cursors() {
    let (srv, owner, org) = api_setup().await;
    for i in 0..5 {
        make_agent(&srv, &org, &owner, &format!("a{i}")).await;
    }
    let (_, key) = make_key(&srv, &org, &owner, &["org:read"], json!({})).await;
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let path = match &cursor {
            Some(c) => format!("/agents?limit=2&cursor={c}"),
            None => "/agents?limit=2".into(),
        };
        let r = get(&srv, &path, &key).await;
        assert_eq!(r.status, 200, "{}", r.body);
        pages += 1;
        for a in r.body["data"].as_array().unwrap() {
            seen.push(a["handle"].as_str().unwrap().to_string());
        }
        match r.body["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    assert_eq!(pages, 3);
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 5);
    assert_eq!(
        code(&get(&srv, "/agents?cursor=zz", &key).await),
        (400, "invalid_cursor")
    );
    assert_eq!(
        code(&get(&srv, "/agents?limit=0", &key).await),
        (400, "bad_request")
    );
    assert_eq!(
        code(&get(&srv, "/agents?limit=201", &key).await),
        (400, "bad_request")
    );
    // Members page the same way.
    assert_eq!(
        get(&srv, "/members?limit=1", &key).await.body["data"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn rate_limits_answer_with_headers_and_retry_after() {
    let (srv, owner, org) = api_setup().await;
    let (_, key) = make_key(&srv, &org, &owner, &["org:read", "rules:write"], json!({})).await;
    let r = get(&srv, "/org", &key).await;
    assert_eq!(r.headers.get("ratelimit-limit").unwrap(), "600");
    assert_eq!(r.headers.get("ratelimit-remaining").unwrap(), "599");
    assert!(r.headers.get("ratelimit-reset").is_some());
    // Writes: 60 a minute. Evaluate is a POST, so it counts as a write.
    let body = json!({ "from": "/acme/boss", "to": "/acme/boss" });
    let mut last = None;
    for i in 0..60 {
        let r = post(&srv, "/rules/evaluate", &key, body.clone()).await;
        assert!(r.status < 300 || r.status == 400, "{i}: {}", r.body);
        assert_eq!(r.headers.get("ratelimit-limit").unwrap(), "60");
        last = Some(r);
    }
    assert_eq!(
        last.unwrap().headers.get("ratelimit-remaining").unwrap(),
        "0"
    );
    let r = post(&srv, "/rules/evaluate", &key, body).await;
    assert_eq!(code(&r), (429, "rate_limited"));
    assert!(r.headers.get("retry-after").is_some());
    assert_eq!(r.headers.get("ratelimit-remaining").unwrap(), "0");
    // Reads keep their own budget.
    assert_eq!(get(&srv, "/org", &key).await.status, 200);
}

#[tokio::test]
async fn every_use_is_audited_with_the_key_id() {
    let (srv, owner, org) = api_setup().await;
    let (kid, key) = make_key(
        &srv,
        &org,
        &owner,
        &["org:read", "agents:write", "audit:read"],
        json!({}),
    )
    .await;
    get(&srv, "/org", &key).await;
    post(&srv, "/agents", &key, json!({ "handle": "audited" })).await;
    get(&srv, "/members/nobody", &key).await;
    let r = get(&srv, "/audit?action=api_key.use&limit=200", &key).await;
    let uses: Vec<&Value> = r.body["data"].as_array().unwrap().iter().collect();
    assert!(uses.len() >= 3, "{}", r.body);
    assert!(uses
        .iter()
        .all(|u| u["actor_kind"] == "api_key" && u["actor"] == kid));
    let routes: Vec<String> = uses
        .iter()
        .map(|u| {
            format!(
                "{} {} {}",
                u["details"]["method"].as_str().unwrap(),
                u["details"]["route"].as_str().unwrap(),
                u["details"]["status"]
            )
        })
        .collect();
    assert!(routes.contains(&"GET /org 200".to_string()), "{routes:?}");
    assert!(
        routes.contains(&"POST /agents 201".to_string()),
        "{routes:?}"
    );
    assert!(
        routes.contains(&"GET /members/{member} 404".to_string()),
        "{routes:?}"
    );
    // The change itself carries the key as its actor, on the API and on the dashboard.
    let r = get(&srv, "/audit?action=agent.create", &key).await;
    assert_eq!(r.body["data"][0]["actor"], kid);
    let (_, dash) = srv
        .get(
            &format!("/v1/orgs/{org}/audit?action=agent.create"),
            &owner.token,
        )
        .await;
    assert_eq!(dash["entries"][0]["actor"], kid);
    assert_eq!(dash["entries"][0]["actor_kind"], "api_key");
    // `since` filters by time; oldest first.
    let later = get(&srv, "/audit?since=2099-01-01T00:00:00Z", &key).await;
    assert_eq!(later.body["data"], json!([]));
    let all = get(&srv, "/audit?limit=200", &key).await;
    let ids: Vec<i64> = all.body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_i64().unwrap())
        .collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "ascending");
    // Never a secret in the log.
    assert!(!all.body.to_string().contains(&key));
}

// ---- agents, rooms, usage ----------------------------------------------------------------------

#[tokio::test]
async fn agents_rooms_and_usage() {
    let (srv, owner, org) = api_setup().await;
    let (_, key) = make_key(
        &srv,
        &org,
        &owner,
        &["org:read", "agents:write", "rooms:write"],
        json!({}),
    )
    .await;
    let r = post(
        &srv,
        "/agents",
        &key,
        json!({ "handle": "ci-bot", "label": "CI" }),
    )
    .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let addr = r.body["data"]["address"].as_str().unwrap().to_string();
    assert_eq!(r.body["data"]["handle"], "/acme/ci-bot");
    // An enrollment code a person issued: the key sees metadata and can revoke it.
    let (s, e) = srv
        .post(
            &format!("/v1/orgs/{org}/agents/{}/enrollments", enc(&addr)),
            &owner.strong,
            json!({ "ttl_hours": 2 }),
        )
        .await;
    assert_eq!(s, 201, "{e}");
    let eid = e["enrollment_id"].as_str().unwrap().to_string();
    let code_value = e["code"].as_str().unwrap().to_string();
    let r = get(&srv, "/agents/ci-bot/enrollments", &key).await;
    assert_eq!(r.body["data"][0]["id"], eid);
    assert!(
        !r.body.to_string().contains(&code_value),
        "codes are never shown"
    );
    let r = del(&srv, &format!("/agents/ci-bot/enrollments/{eid}"), &key).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        code(&del(&srv, &format!("/agents/ci-bot/enrollments/{eid}"), &key).await),
        (404, "not_found")
    );
    let r = post(&srv, "/agents/ci-bot/disconnect", &key, json!({})).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        get(&srv, "/agents/ci-bot/delegations", &key).await.body["data"],
        json!([])
    );

    // Rooms: the owner's device activates encryption; the key handles metadata and invitations.
    let r = post(
        &srv,
        "/rooms",
        &key,
        json!({ "name": "Builds", "owner": "boss", "listed": true }),
    )
    .await;
    assert_eq!(r.status, 201, "{}", r.body);
    let room = r.body["data"]["id"].as_str().unwrap().to_string();
    assert_eq!(r.body["data"]["activation_state"], "provisioning");
    let r = post(
        &srv,
        "/rooms",
        &key,
        json!({ "name": "X", "owner": "/elsewhere/me" }),
    )
    .await;
    assert_eq!(code(&r), (400, "not_org_mailbox"));
    let r = patch(
        &srv,
        &format!("/rooms/{room}"),
        &key,
        json!({ "listed": false }),
    )
    .await;
    assert_eq!(r.body["data"]["listed"], false, "{}", r.body);
    let r = post(
        &srv,
        &format!("/rooms/{room}/members"),
        &key,
        json!({ "handle": "ci-bot" }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["data"]["state"], "invited");
    let r = get(&srv, "/rooms", &key).await;
    assert!(r.body["data"]
        .as_array()
        .unwrap()
        .iter()
        .any(|g| g["id"] == room));
    let r = del(&srv, &format!("/rooms/{room}"), &key).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        code(&del(&srv, &format!("/rooms/{room}"), &key).await),
        (404, "not_found")
    );

    let r = get(&srv, "/usage", &key).await;
    assert_eq!(r.body["data"]["agents_used"], 1);
    assert_eq!(r.body["data"]["seats_used"], 1);
    let r = del(&srv, "/agents/ci-bot", &key).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        get(&srv, "/usage", &key).await.body["data"]["agents_used"],
        0
    );
    let r = get(&srv, "/billing", &key).await;
    assert_eq!(r.body["data"]["status"], "active");
    let r = get(&srv, "/org", &key).await;
    assert_eq!(r.body["data"]["namespace"], "acme");
    assert!(r.body["data"]["created_at"]
        .as_str()
        .unwrap()
        .ends_with('Z'));
}

#[tokio::test]
async fn read_only_orgs_refuse_writes_and_suspended_orgs_refuse_everything() {
    let (srv, owner, org) = api_setup().await;
    let (_, key) = make_key(&srv, &org, &owner, &["org:read", "agents:write"], json!({})).await;
    let set = |status: &'static str| {
        let org = org.clone();
        move |tx: &rusqlite::Transaction<'_>| -> crate::groups::GResult<()> {
            tx.execute(
                "UPDATE orgs SET status = ?2 WHERE id = ?1",
                rusqlite::params![org, status],
            )?;
            Ok(())
        }
    };
    srv.state.store.group_tx(set("read_only")).await.unwrap();
    assert_eq!(get(&srv, "/agents", &key).await.status, 200);
    assert_eq!(
        code(&post(&srv, "/agents", &key, json!({ "handle": "nope" })).await),
        (409, "org_read_only")
    );
    srv.state.store.group_tx(set("suspended")).await.unwrap();
    assert_eq!(
        code(&get(&srv, "/agents", &key).await),
        (409, "org_inactive")
    );
}

// ---- messages ----------------------------------------------------------------------------------

#[tokio::test]
async fn messages_go_only_from_the_keys_agent_to_this_orgs_mailboxes() {
    let (srv, owner, org) = api_setup().await;
    make_agent(&srv, &org, &owner, "notifier").await;
    let jane = srv.person("jane").await;
    let (_, jane_mb) = srv.join(&org, &owner, &jane, "jane", &[]).await;
    let (_, key) = make_key(
        &srv,
        &org,
        &owner,
        &["messages:send"],
        json!({ "send_as": "/acme/notifier" }),
    )
    .await;

    let r = api(
        &srv,
        Method::POST,
        "/org/v1/messages",
        Some(&key),
        Some(json!({ "to": "/acme/jane", "body": "Build 42 passed", "thread_title": "CI" })),
        Some("build-42"),
    )
    .await;
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(r.body["data"]["from"], "/acme/notifier");
    assert_eq!(r.body["data"]["to"], "/acme/jane");
    // Retried with the same key: replayed, delivered once.
    let again = api(
        &srv,
        Method::POST,
        "/org/v1/messages",
        Some(&key),
        Some(json!({ "to": "/acme/jane", "body": "Build 42 passed", "thread_title": "CI" })),
        Some("build-42"),
    )
    .await;
    assert_eq!(again.status, 201);
    assert_eq!(again.headers.get("idempotent-replayed").unwrap(), "true");
    let (s, inbox) = srv
        .get(
            &format!("/v1/inbox?identity={}", enc(&jane_mb)),
            &jane.token,
        )
        .await;
    assert_eq!(s, 200, "{inbox}");
    let n = inbox["messages"].as_array().map(|m| m.len()).unwrap_or(0);
    assert_eq!(n, 1, "delivered exactly once: {inbox}");

    // Rooms are end-to-end encrypted: never through the API.
    let (_, rooms) = srv
        .get(&format!("/v1/orgs/{org}/rooms"), &owner.token)
        .await;
    let general = rooms["rooms"][0]["id"].as_str().unwrap().to_string();
    let r = post(
        &srv,
        "/messages",
        &key,
        json!({ "to": general, "body": "hi" }),
    )
    .await;
    assert_eq!(code(&r), (409, "room_encrypted"));
    // Outside the org: refused, however the recipient's inbox is configured.
    let outsider = srv.person("out").await;
    let (s, id) = srv
        .post("/v1/identities", &outsider.token, json!({ "label": "x" }))
        .await;
    assert!(s == 200 || s == 201, "{id}");
    let out_addr = id["address"].as_str().unwrap().to_string();
    assert_eq!(
        code(
            &post(
                &srv,
                "/messages",
                &key,
                json!({ "to": out_addr, "body": "hi" })
            )
            .await
        ),
        (403, "recipient_not_in_org")
    );
    assert_eq!(
        code(
            &post(
                &srv,
                "/messages",
                &key,
                json!({ "to": "/acme/nobody", "body": "hi" })
            )
            .await
        )
        .0,
        404
    );
    // Size bound.
    let big = "x".repeat(70 * 1024);
    assert_eq!(
        code(
            &post(
                &srv,
                "/messages",
                &key,
                json!({ "to": "/acme/jane", "body": big })
            )
            .await
        ),
        (413, "payload_too_large")
    );
    // A suspended sending agent cannot send.
    srv.patch(
        &format!("/v1/orgs/{org}/agents/{}", enc("/acme/notifier")),
        &owner.token,
        json!({ "status": "suspended" }),
    )
    .await;
    assert_eq!(
        code(
            &post(
                &srv,
                "/messages",
                &key,
                json!({ "to": "/acme/jane", "body": "hi" })
            )
            .await
        ),
        (409, "send_as_unavailable")
    );
}

// ---- spec drift ----

/// The spec's own examples obey its schemas, so the docs never show a shape the API does not send.
#[test]
fn every_example_in_the_org_spec_matches_its_schema() {
    let spec = spec();
    let mut errs = Vec::new();
    for (path, item) in spec["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            let mut media = Vec::new();
            if let Some(rb) = op.get("requestBody") {
                media.push((
                    "request".to_string(),
                    resolve(&spec, rb)["content"]["application/json"].clone(),
                ));
            }
            for (status, resp) in op["responses"].as_object().into_iter().flatten() {
                media.push((
                    status.clone(),
                    resolve(&spec, resp)["content"]["application/json"].clone(),
                ));
            }
            for (what, m) in media {
                if let (Some(schema), Some(example)) = (m.get("schema"), m.get("example")) {
                    check(
                        &spec,
                        schema,
                        example,
                        &format!("{method} {path} {what}"),
                        &mut errs,
                    );
                }
            }
        }
    }
    assert!(errs.is_empty(), "examples off-schema:\n{}", errs.join("\n"));
}

/// Every `/org/v1` route is in `openapi/pigeonpost-org.v1.yaml` and the spec has no route that
/// does not exist; the JSON mirror the tests and docs read is the YAML's exact twin (checked in CI).
#[test]
fn every_org_api_route_is_in_the_org_spec_and_nothing_else_is() {
    use std::collections::BTreeSet;
    let served: BTreeSet<(String, String)> =
        crate::foundations_tests::routes_in(include_str!("org_api.rs"), "pub(crate) fn router(")
            .into_iter()
            .map(|(m, p)| (m, p.trim_start_matches("/org/v1").to_string()))
            .collect();
    assert_eq!(served.len(), 39, "{served:?}");
    let documented = crate::foundations_tests::spec_operations(include_str!(
        "../../../openapi/pigeonpost-org.v1.yaml"
    ));
    let missing: Vec<_> = served.difference(&documented).collect();
    let stale: Vec<_> = documented.difference(&served).collect();
    assert!(
        missing.is_empty(),
        "routes missing from the org spec: {missing:?}"
    );
    assert!(
        stale.is_empty(),
        "org spec operations with no route: {stale:?}"
    );
    // The mirror matches the YAML's operations too.
    let mirror = spec();
    let mut ops = BTreeSet::new();
    for (path, item) in mirror["paths"].as_object().unwrap() {
        for m in ["get", "post", "put", "patch", "delete"] {
            if item.get(m).is_some() {
                ops.insert((m.to_string(), path.clone()));
            }
        }
    }
    assert_eq!(ops, documented);
}

#[tokio::test]
async fn an_org_key_is_never_a_member_api_credential() {
    let srv = serve().await;
    for path in [
        "/v1/whoami",
        "/v1/capabilities",
        "/v1/identities",
        "/v1/orgs",
    ] {
        let (s, v) = srv
            .get(path, "pk_org_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .await;
        assert_eq!(s, 401, "{path} {v}");
    }
}
