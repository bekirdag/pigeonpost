//! End-to-end tests for the Phase 0 foundations, over real sockets: the event stream's wire
//! format, the internal API's exposure and token checks, capabilities, and audit records.

use crate::store::StreamCursor;
use crate::*;
use std::time::Duration;

const OPS: &str = "svc_ops_0123456789abcdef0123456789abcdef";
const CALLS: &str = "svc_calls_0123456789abcdef0123456789abcdef";
const GRANT: &str = "grant-secret-for-tests";

fn state() -> AppState {
    let mut state = crate::tests::test_state();
    state.service_tokens = Arc::new(internal_api::tokens_for_test(&[
        ("POSTBOX_SVC_TOKEN_OPS", OPS),
        ("POSTBOX_SVC_TOKEN_CALLS", CALLS),
    ]));
    state.namespace_grant_hash = Some(sha256(GRANT.as_bytes()));
    state
}

/// Serve the public and internal routers on two loopback ports.
async fn serve(state: AppState) -> (String, String) {
    let public = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let internal = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let public_url = format!("http://{}", public.local_addr().unwrap());
    let internal_url = format!("http://{}", internal.local_addr().unwrap());
    let app =
        build_router(state.clone()).into_make_service_with_connect_info::<std::net::SocketAddr>();
    let internal_app =
        internal_api::router(state).into_make_service_with_connect_info::<std::net::SocketAddr>();
    tokio::spawn(async move { axum::serve(public, app).await.unwrap() });
    tokio::spawn(async move { axum::serve(internal, internal_app).await.unwrap() });
    (public_url, internal_url)
}

/// An account with an API key and two mailboxes.
async fn account(
    state: &AppState,
    id: &str,
) -> (String, store::StoredIdentity, store::StoredIdentity) {
    let (token, key) = new_api_key();
    state
        .store
        .create_account(id.into(), key, now_unix())
        .await
        .unwrap();
    let mut mailboxes = Vec::new();
    for _ in 0..2 {
        let v = do_create_identity(state, Some(id.into()), None, None)
            .await
            .unwrap();
        let address = v["address"].as_str().unwrap().to_string();
        mailboxes.push(state.store.get(address).await.unwrap().unwrap());
    }
    let b = mailboxes.pop().unwrap();
    let a = mailboxes.pop().unwrap();
    (token, a, b)
}

/// Reads SSE frames off a response, ignoring keep-alive comments.
struct Frames {
    response: reqwest::Response,
    buffer: String,
}

impl Frames {
    fn new(response: reqwest::Response) -> Self {
        Frames {
            response,
            buffer: String::new(),
        }
    }

    /// The next non-comment frame, or `None` if nothing arrives within `wait`.
    async fn next(&mut self, wait: Duration) -> Option<String> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            while let Some(split) = self.buffer.find("\n\n") {
                let frame = self.buffer[..split + 2].to_string();
                self.buffer.drain(..split + 2);
                if frame.lines().all(|l| l.starts_with(':')) {
                    continue;
                }
                return Some(frame);
            }
            let chunk = tokio::time::timeout_at(deadline, self.response.chunk())
                .await
                .ok()?
                .unwrap()?;
            self.buffer.push_str(&String::from_utf8_lossy(&chunk));
        }
    }
}

async fn open_stream(base: &str, token: &str, cursor: Option<&str>) -> Frames {
    let mut req = reqwest::Client::new()
        .get(format!("{base}/v1/events"))
        .bearer_auth(token)
        .header("accept", "text/event-stream");
    if let Some(cursor) = cursor {
        req = req.header("last-event-id", cursor);
    }
    let response = req.send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    Frames::new(response)
}

async fn set_flag(internal: &str, token: &str, body: serde_json::Value) -> reqwest::Response {
    reqwest::Client::new()
        .put(format!(
            "{internal}/internal/v1/features/{}",
            capabilities::ACCOUNT_EVENTS
        ))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap()
}

const WAIT: Duration = Duration::from_secs(3);
const QUIET: Duration = Duration::from_millis(400);

#[tokio::test]
async fn mail_events_are_byte_compatible_and_typed_events_follow_the_flag() {
    let state = state();
    let (token, a, b) = account(&state, "acct_stream").await;
    let (public, internal) = serve(state.clone()).await;

    let mut stream = open_stream(&public, &token, None).await;
    do_send(&state, &b, &a.address, "hello", ThreadRequest::Default)
        .await
        .unwrap();
    let frame = stream.next(WAIT).await.expect("a mail event");

    // The exact bytes shipped clients parse: id first, then the name, then alphabetical JSON.
    let rows = state
        .store
        .stream_after(
            "acct_stream".into(),
            vec![a.address.clone()],
            StreamCursor::default(),
            false,
            8,
        )
        .await
        .unwrap();
    let (rowid, message) = &rows.mail[0];
    let expected = format!(
        "id: {rowid}\nevent: mail\ndata: {{\"created_at\":{},\"event_id\":{rowid},\"mailbox\":\"{}\",\"message_id\":\"{}\",\"sender\":\"{}\"}}\n\n",
        message.created_at, a.address, message.id, b.address
    );
    assert_eq!(frame, expected);

    // With the flag off, a flag change for someone else and nothing typed reaches this stream.
    assert_eq!(
        set_flag(
            &internal,
            OPS,
            json!({"scope": "account", "account_id": "someone_else", "enabled": true})
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert!(stream.next(QUIET).await.is_none());

    // Turning it on for this account delivers its own `features.changed`, live, with a typed id.
    assert_eq!(
        set_flag(
            &internal,
            OPS,
            json!({"scope": "account", "account_id": "acct_stream", "enabled": true})
        )
        .await
        .status(),
        StatusCode::OK
    );
    let typed = stream.next(WAIT).await.expect("a typed event");
    let mut lines = typed.lines();
    let id = lines
        .next()
        .unwrap()
        .strip_prefix("id: ")
        .unwrap()
        .to_string();
    assert!(StreamCursor::parse(&id).unwrap().event > 0);
    assert!(id.contains('.'), "typed ids never parse as a mail cursor");
    assert_eq!(lines.next().unwrap(), "event: features.changed");
    let data: serde_json::Value =
        serde_json::from_str(lines.next().unwrap().strip_prefix("data: ").unwrap()).unwrap();
    assert_eq!(data["type"], "features.changed");
    assert_eq!(data["event_id"], id.as_str());
    assert_eq!(
        data["data"],
        json!({"feature": "account_events", "enabled": true})
    );
    assert!(data.get("message_id").is_none() && data.get("sender").is_none());
    drop(stream);

    // A legacy integer cursor at the mail resumes with the typed event that followed it…
    let mut legacy = open_stream(&public, &token, Some(&rowid.to_string())).await;
    let replay = legacy.next(WAIT).await.expect("replayed typed event");
    assert!(replay.starts_with(&format!("id: {id}\n")), "{replay}");
    assert!(legacy.next(QUIET).await.is_none(), "and nothing twice");
    drop(legacy);

    // …and the typed cursor resumes with nothing.
    let mut current = open_stream(&public, &token, Some(&id)).await;
    assert!(current.next(QUIET).await.is_none());

    // Mail after a typed event still arrives with a plain integer id.
    do_send(&state, &b, &a.address, "again", ThreadRequest::Default)
        .await
        .unwrap();
    let mail = current.next(WAIT).await.expect("mail after typed");
    let mail_id = mail.lines().next().unwrap().strip_prefix("id: ").unwrap();
    assert!(mail_id.parse::<i64>().is_ok(), "{mail}");
    assert!(mail.contains("\nevent: mail\n"));
}

#[tokio::test]
async fn an_expired_cursor_gets_resync_required_without_moving_it() {
    let state = state();
    let (token, _a, _b) = account(&state, "acct_resync").await;
    state
        .store
        .set_feature(
            capabilities::ACCOUNT_EVENTS.into(),
            store::FeatureScope::Account("acct_resync".into()),
            Some(true),
            "test".into(),
            state.auditor.row(
                audit::ActorKind::System,
                None,
                "feature.set",
                None,
                json!({}),
                None,
                None,
                1,
            ),
            1,
        )
        .await
        .unwrap();
    for _ in 0..3 {
        state
            .store
            .append_account_event("acct_resync".into(), "test.old".into(), None, json!({}), 2)
            .await
            .unwrap();
    }
    let keep = state
        .store
        .append_account_event(
            "acct_resync".into(),
            "test.kept".into(),
            None,
            json!({}),
            now_unix(),
        )
        .await
        .unwrap();
    assert!(
        state
            .store
            .prune_account_events(now_unix() - 60, 10_000)
            .await
            .unwrap()
            >= 3
    );

    let (public, _) = serve(state).await;
    let mut stream = open_stream(&public, &token, Some("0")).await;
    let first = stream.next(WAIT).await.unwrap();
    assert_eq!(
        first,
        "event: resync_required\ndata: {\"reason\":\"cursor_expired\",\"type\":\"resync_required\"}\n\n"
    );
    let next = stream.next(WAIT).await.unwrap();
    assert!(
        next.starts_with(&format!("id: {}\nevent: test.kept\n", keep.typed_id())),
        "{next}"
    );

    // A cursor inside the window is not told to resync.
    let mut fresh = open_stream(&public, &token, Some(&keep.typed_id())).await;
    assert!(fresh.next(QUIET).await.is_none());
}

#[tokio::test]
async fn a_mailbox_token_still_cannot_open_the_account_stream() {
    let state = state();
    let v = do_create_identity(&state, None, None, None).await.unwrap();
    let cap = v["capability_token"].as_str().unwrap().to_string();
    let (public, _) = serve(state).await;
    let response = reqwest::Client::new()
        .get(format!("{public}/v1/events"))
        .bearer_auth(cap)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["error"],
        "use_api_key"
    );
}

#[tokio::test]
async fn the_stream_count_per_account_is_bounded() {
    let mut state = state();
    state.event_hub = Arc::new(account_events::EventHub::new(1));
    let (token, _, _) = account(&state, "acct_bound").await;
    let (public, _) = serve(state).await;
    let _held = open_stream(&public, &token, None).await;
    let refused = reqwest::Client::new()
        .get(format!("{public}/v1/events"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(refused.headers().get(header::RETRY_AFTER).is_some());
}

#[tokio::test]
async fn the_internal_api_is_not_on_the_public_listener() {
    let state = state();
    let (public, internal) = serve(state).await;
    let client = reqwest::Client::new();
    let public_try = client
        .get(format!("{public}/internal/v1/health"))
        .bearer_auth(OPS)
        .send()
        .await
        .unwrap();
    assert_eq!(public_try.status(), StatusCode::NOT_FOUND);
    let ok = client
        .get(format!("{internal}/internal/v1/health"))
        .bearer_auth(OPS)
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
    assert_eq!(
        ok.json::<serde_json::Value>().await.unwrap()["service"],
        "ops"
    );
    // And the member API is not on the internal listener.
    let member = client
        .get(format!("{internal}/v1/capabilities"))
        .send()
        .await
        .unwrap();
    assert_eq!(member.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn service_tokens_are_checked_per_service_and_per_endpoint() {
    let state = state();
    let (_, internal) = serve(state.clone()).await;
    let client = reqwest::Client::new();
    let health = format!("{internal}/internal/v1/health");

    let missing = client.get(&health).send().await.unwrap();
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    assert!(missing.headers().get(header::WWW_AUTHENTICATE).is_some());
    for wrong in [
        "svc_ops_0123456789abcdef0123456789abcdeX",
        GRANT,
        "pk_live_nope",
    ] {
        let r = client.get(&health).bearer_auth(wrong).send().await.unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED, "{wrong}");
        assert_eq!(
            r.json::<serde_json::Value>().await.unwrap()["error"],
            "invalid_service_token"
        );
    }

    // The calls service may check health but not touch flags.
    let calls = client.get(&health).bearer_auth(CALLS).send().await.unwrap();
    assert_eq!(calls.status(), StatusCode::OK);
    assert_eq!(
        calls.json::<serde_json::Value>().await.unwrap()["service"],
        "calls"
    );
    let denied = set_flag(
        &internal,
        CALLS,
        json!({"scope": "global", "enabled": true}),
    )
    .await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        denied.json::<serde_json::Value>().await.unwrap()["error"],
        "endpoint_not_allowed"
    );
    let listing = client
        .get(format!("{internal}/internal/v1/features"))
        .bearer_auth(CALLS)
        .send()
        .await
        .unwrap();
    assert_eq!(listing.status(), StatusCode::FORBIDDEN);
    assert!(state
        .store
        .enabled_features(None, capabilities::known_names())
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn flag_writes_are_validated_and_audited() {
    let state = state();
    let (_, internal) = serve(state.clone()).await;
    let client = reqwest::Client::new();

    let unknown = client
        .put(format!("{internal}/internal/v1/features/no_such_flag"))
        .bearer_auth(OPS)
        .json(&json!({"scope": "global", "enabled": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        unknown.json::<serde_json::Value>().await.unwrap()["error"],
        "unknown_feature"
    );
    for (body, code) in [
        (
            json!({"scope": "org", "account_id": "o", "enabled": true}),
            "scope_unsupported",
        ),
        (json!({"scope": "account", "enabled": true}), "bad_request"),
        (
            json!({"scope": "global", "account_id": "a", "enabled": true}),
            "bad_request",
        ),
        (json!({"nope": 1}), "bad_request"),
    ] {
        let r = set_flag(&internal, OPS, body.clone()).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(r.json::<serde_json::Value>().await.unwrap()["error"], code);
    }

    let ok = set_flag(&internal, OPS, json!({"scope": "global", "enabled": true})).await;
    assert_eq!(ok.status(), StatusCode::OK);
    let listing: serde_json::Value = client
        .get(format!("{internal}/internal/v1/features"))
        .bearer_auth(OPS)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listing["flags"][0]["name"], "account_events");
    assert_eq!(listing["flags"][0]["updated_by"], "service:ops");
    assert_eq!(listing["known"][0]["name"], "account_events");

    let audit = state.store.audit_for_actor(None, 10).await.unwrap();
    assert_eq!(audit.len(), 1, "only the successful change is recorded");
    let row = &audit[0];
    assert_eq!(row.actor_kind, "system");
    assert_eq!(row.action, "feature.set");
    assert_eq!(row.target.as_deref(), Some("feature:account_events"));
    let details: serde_json::Value = serde_json::from_str(&row.details).unwrap();
    assert_eq!(details["service"], "ops");
    let ip_hash = row.ip_hash.as_deref().unwrap();
    assert!(!ip_hash.contains("127.0.0.1"));
    assert_eq!(ip_hash, state.auditor.ip_hash("127.0.0.1".parse().unwrap()));

    // Clearing with null removes the value.
    let cleared = set_flag(&internal, OPS, json!({"scope": "global", "enabled": null})).await;
    assert_eq!(cleared.status(), StatusCode::OK);
    assert!(state.store.list_features().await.unwrap().is_empty());
}

#[tokio::test]
async fn capabilities_report_flags_and_credential_provenance() {
    let state = state();
    let (token, _, _) = account(&state, "acct_caps").await;
    let v = do_create_identity(&state, None, None, None).await.unwrap();
    let cap = v["capability_token"].as_str().unwrap().to_string();
    let (public, internal) = serve(state).await;
    let client = reqwest::Client::new();
    let caps = |t: String| {
        let client = client.clone();
        let url = format!("{public}/v1/capabilities");
        async move {
            let r = client.get(url).bearer_auth(t).send().await.unwrap();
            (r.status(), r.json::<serde_json::Value>().await.unwrap())
        }
    };

    let (status, body) = caps(token.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["api_version"], capabilities::API_VERSION);
    assert_eq!(body["features"], json!([]));
    assert_eq!(body["event_types"], json!([]));
    assert_eq!(body["credential"]["kind"], "api_key");
    assert_eq!(body["credential"]["human"], false);
    assert_eq!(body["credential"]["step_up"]["configured"], false);
    assert_eq!(body["credential"]["step_up"]["satisfied"], false);

    set_flag(
        &internal,
        OPS,
        json!({"scope": "account", "account_id": "acct_caps", "enabled": true}),
    )
    .await;
    let (_, body) = caps(token).await;
    assert_eq!(body["features"], json!(["account_events"]));
    assert!(body["event_types"]
        .as_array()
        .unwrap()
        .contains(&json!("features.changed")));

    // An anonymous mailbox sees only global flags.
    let (status, body) = caps(cap).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["credential"]["kind"], "mailbox");
    assert_eq!(body["features"], json!([]));

    let anonymous = client
        .get(format!("{public}/v1/capabilities"))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn api_key_changes_and_namespace_grants_are_audited() {
    let state = state();
    let (token, _, _) = account(&state, "acct_audit").await;
    let (public, _) = serve(state.clone()).await;
    let client = reqwest::Client::new();

    let created = client
        .post(format!("{public}/v1/api-keys"))
        .bearer_auth(&token)
        .header("user-agent", "audit-test/1")
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let new_key = created.json::<serde_json::Value>().await.unwrap()["api_key"]
        .as_str()
        .unwrap()
        .to_string();
    let rows = state
        .store
        .audit_for_actor(Some("acct_audit".into()), 10)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].action, "api_key.create");
    assert_eq!(rows[0].actor_kind, "api_key");
    assert_eq!(rows[0].user_agent.as_deref(), Some("audit-test/1"));
    assert!(
        !rows[0].details.contains("pk_live_"),
        "the key is never logged"
    );
    assert!(!rows[0].details.contains(&new_key));

    // The grant route keeps working with its shared secret, and is recorded.
    let wrong = client
        .put(format!("{public}/v1/namespaces"))
        .bearer_auth("not-the-secret")
        .json(&json!({"namespace": "auditco", "account_id": "acct_audit"}))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    let granted = client
        .put(format!("{public}/v1/namespaces"))
        .bearer_auth(GRANT)
        .json(&json!({"namespace": "auditco", "account_id": "acct_audit"}))
        .send()
        .await
        .unwrap();
    assert_eq!(granted.status(), StatusCode::OK);
    let system = state.store.audit_for_actor(None, 10).await.unwrap();
    assert_eq!(system.len(), 1);
    assert_eq!(system[0].action, "namespace.grant");
    assert_eq!(system[0].target.as_deref(), Some("namespace:auditco"));
    assert!(!system[0].details.contains(GRANT));
}

/// Every route the routers serve is in `openapi/pigeonpost.v1.yaml`, and the spec documents no
/// route that does not exist. Parsing the router source keeps this honest without a second,
/// hand-maintained list that could drift on its own.
#[test]
fn every_route_is_in_the_openapi_spec_and_nothing_else_is() {
    use std::collections::BTreeSet;
    let mut served: BTreeSet<(String, String)> = BTreeSet::new();
    for (source, router_fn) in [
        (include_str!("main.rs"), "fn build_router("),
        (include_str!("internal_api.rs"), "pub(crate) fn router("),
        (include_str!("org_routes.rs"), "pub(crate) fn router("),
        (include_str!("org_agents.rs"), "pub(crate) fn router("),
        (include_str!("org_api_keys.rs"), "pub(crate) fn router("),
        (include_str!("webhooks.rs"), "pub(crate) fn router("),
        (include_str!("meetings.rs"), "pub(crate) fn router("),
        (
            include_str!("meetings.rs"),
            "pub(crate) fn internal_router(",
        ),
    ] {
        served.extend(routes_in(source, router_fn));
    }
    assert!(served.len() > 50, "parser found {} routes", served.len());

    let spec = include_str!("../../../openapi/pigeonpost.v1.yaml");
    let documented = spec_operations(spec);

    let missing: Vec<_> = served.difference(&documented).collect();
    let stale: Vec<_> = documented.difference(&served).collect();
    assert!(
        missing.is_empty(),
        "routes missing from the OpenAPI spec: {missing:?}"
    );
    assert!(
        stale.is_empty(),
        "OpenAPI operations with no route: {stale:?}"
    );
}

/// `(method, path)` for every `.route("…", …)` in the body of `router_fn`.
pub(crate) fn routes_in(source: &str, router_fn: &str) -> Vec<(String, String)> {
    // A Windows checkout may have CRLF line endings.
    let source = source.replace("\r\n", "\n");
    let start = source.find(router_fn).expect("router function");
    let body = &source[start..];
    let end = body.find("\n}\n").expect("end of router function");
    let body = &body[..end];
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(at) = rest.find(".route(") {
        rest = &rest[at + ".route(".len()..];
        // The path literal.
        let open = rest.find('"').unwrap();
        let close = open + 1 + rest[open + 1..].find('"').unwrap();
        let path = &rest[open + 1..close];
        // The method-router expression up to the matching close paren of `.route(`.
        let mut depth = 1usize;
        let mut expr_end = close;
        for (i, ch) in rest[close..].char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        expr_end = close + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let expr = &rest[close + 1..expr_end];
        if path.contains("{*") {
            continue; // the catch-all stub
        }
        // Method constructors at the expression's own nesting level.
        let mut level = 0usize;
        let bytes = expr.as_bytes();
        for (i, ch) in expr.char_indices() {
            match ch {
                '(' => level += 1,
                ')' => level = level.saturating_sub(1),
                _ => {}
            }
            if level != 0 {
                continue;
            }
            for method in ["get", "post", "put", "delete", "patch"] {
                if expr[i..].starts_with(&format!("{method}(")) {
                    let prev = if i == 0 { b' ' } else { bytes[i - 1] };
                    if !(prev.is_ascii_alphanumeric() || prev == b'_') {
                        out.push((method.to_string(), path.to_string()));
                    }
                }
            }
        }
    }
    out
}

/// `(method, path)` for every operation under `paths:`.
pub(crate) fn spec_operations(spec: &str) -> std::collections::BTreeSet<(String, String)> {
    let mut out = std::collections::BTreeSet::new();
    let mut in_paths = false;
    let mut path: Option<String> = None;
    for line in spec.lines() {
        if !line.starts_with(' ') && !line.trim().is_empty() {
            in_paths = line.trim_end() == "paths:";
            path = None;
            continue;
        }
        if !in_paths {
            continue;
        }
        if let Some(key) = line.strip_prefix("  ").filter(|l| !l.starts_with(' ')) {
            if let Some(p) = key.trim_end().strip_suffix(':') {
                path = Some(p.trim_matches(|c| c == '"' || c == '\'').to_string());
            }
        } else if let Some(key) = line.strip_prefix("    ").filter(|l| !l.starts_with(' ')) {
            let key = key.trim_end().trim_end_matches(':');
            if ["get", "post", "put", "delete", "patch"].contains(&key) {
                if let Some(p) = &path {
                    out.insert((key.to_string(), p.clone()));
                }
            }
        }
    }
    out
}

#[test]
fn the_route_parser_reads_chained_and_layered_method_routers() {
    let src = r#"fn build_router(s: S) -> Router {
    Router::new()
        .route("/a", get(x).post(y))
        .route("/b", post(z).layer(axum::extract::DefaultBodyLimit::max(get_limit(1))))
        .route("/c/{id}", axum::routing::patch(p).delete(d))
        .route("/v1/{*rest}", any(stub))
}
"#;
    let mut got = routes_in(src, "fn build_router(");
    got.sort();
    assert_eq!(
        got,
        vec![
            ("delete".to_string(), "/c/{id}".to_string()),
            ("get".to_string(), "/a".to_string()),
            ("patch".to_string(), "/c/{id}".to_string()),
            ("post".to_string(), "/a".to_string()),
            ("post".to_string(), "/b".to_string()),
        ]
    );
}

/// The storage settings the A4 review found, pinned so they cannot silently regress: WAL (the
/// server and the separate reaper process share the file) and a busy timeout (a writer waits for
/// the other process instead of failing with SQLITE_BUSY).
#[tokio::test]
async fn file_databases_use_wal_and_a_busy_timeout() {
    let dir = std::env::temp_dir().join(format!("pp-store-{}", rand_hex(6)));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("postbox.db");
    let store = store::Store::open(path.to_str().unwrap()).unwrap();
    assert_eq!(store.pragma("journal_mode").await.unwrap(), "wal");
    assert_eq!(store.pragma("busy_timeout").await.unwrap(), "5000");
    // A second connection, as the reaper opens, sees the same mode and the new tables.
    let reaper = store::Store::open(path.to_str().unwrap()).unwrap();
    assert_eq!(reaper.pragma("journal_mode").await.unwrap(), "wal");
    assert_eq!(reaper.prune_account_events(0, 10).await.unwrap(), 0);
    drop((store, reaper));
    let _ = std::fs::remove_dir_all(dir);
}

/// Phase 9: a stream opened with an API key ends soon after the key is revoked, instead of living
/// until the client disconnects.
#[tokio::test]
async fn a_revoked_api_keys_stream_is_closed() {
    let state = state();
    let (token, _, _) = account(&state, "acct_revoke_stream").await;
    let (public, _) = serve(state).await;
    let mut stream = open_stream(&public, &token, None).await;
    let client = reqwest::Client::new();
    let keys: serde_json::Value = client
        .get(format!("{public}/v1/api-keys"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = keys["keys"][0]["id"].as_str().unwrap().to_string();
    let revoked = client
        .delete(format!("{public}/v1/api-keys/{id}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::OK);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    loop {
        match tokio::time::timeout_at(deadline, stream.response.chunk()).await {
            Err(_) => panic!("the stream outlived its revoked key"),
            Ok(Ok(None)) | Ok(Err(_)) => break,
            Ok(Ok(Some(_))) => continue,
        }
    }
}

/// Phase 9: the ops-only purge of an empty account — dry run, refusal with content, removal, audit.
#[tokio::test]
async fn ops_can_purge_an_empty_account_and_nothing_else() {
    let state = state();
    let (_token, a, b) = account(&state, "acct_purge_me").await;
    let (_t2, c, _) = account(&state, "acct_purge_busy").await;
    do_send(&state, &a, &c.address, "hello", ThreadRequest::Default)
        .await
        .unwrap();
    let (_, internal) = serve(state.clone()).await;
    let client = reqwest::Client::new();
    let purge = |id: &str, token: &str, body: serde_json::Value| {
        client
            .post(format!("{internal}/internal/v1/accounts/{id}/purge"))
            .bearer_auth(token)
            .json(&body)
            .send()
    };
    let other_service = purge("acct_purge_me", CALLS, json!({"reason": "x"}))
        .await
        .unwrap();
    assert_eq!(other_service.status(), StatusCode::FORBIDDEN);

    // The sender's mailbox holds its sent copy: not empty.
    let refused = purge(
        "acct_purge_me",
        OPS,
        json!({"reason": "test_cleanup", "confirm": true}),
    )
    .await
    .unwrap();
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["error"], "account_not_empty");
    assert!(state.store.get(a.address.clone()).await.unwrap().is_some());

    // Empty it the way a member would, then purge.
    for m in [&a, &b] {
        state
            .store
            .delete_identity(Some("acct_purge_me".into()), m.address.clone())
            .await
            .unwrap();
    }
    let dry = purge("acct_purge_me", OPS, json!({"reason": "test_cleanup"}))
        .await
        .unwrap();
    assert_eq!(dry.status(), StatusCode::OK);
    let dry: serde_json::Value = dry.json().await.unwrap();
    assert_eq!(dry["report"]["purged"], false);
    let done: serde_json::Value = purge(
        "acct_purge_me",
        OPS,
        json!({"reason": "test_cleanup", "confirm": true}),
    )
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(done["report"]["purged"], true);
    let gone = purge("acct_purge_me", OPS, json!({"reason": "test_cleanup"}))
        .await
        .unwrap();
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);
    // The busy account is untouched.
    assert!(state.store.get(c.address.clone()).await.unwrap().is_some());
}

/// Phase 9: the refused-credential throttle counts per address and only refusals.
#[test]
fn auth_failures_throttle_an_address() {
    let key = format!("authfail:198.51.100.{}", rand_hex(2));
    for _ in 0..5 {
        assert!(!groups::rate_limit_peek(&key, 5, 600));
        let _ = groups::rate_limit(&key, usize::MAX, 600);
    }
    assert!(groups::rate_limit_peek(&key, 5, 600));
    assert!(!groups::rate_limit_peek("authfail:203.0.113.9", 5, 600));
}
