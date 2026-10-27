//! Live streams in groups (Phase 6, SDS §14A) over real sockets: authorization, limits, the key
//! directory, the calls service's internal endpoints and the end-of-stream notice.

use crate::groups_tests::{member, serve, three, Member, Server, CALLS, OPS};
use crate::*;
use reqwest::Method;
use serde_json::{json, Value};

fn key(c: char) -> String {
    format!("B{}", c.to_string().repeat(86))
}

async fn streaming_on(srv: &Server) {
    let (s, v) = srv
        .call(
            Method::PUT,
            "/internal/v1/features/streaming",
            OPS,
            None,
            None,
            Some(json!({ "scope": "global", "enabled": true })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
}

async fn go_live(srv: &Server, m: &Member, gid: &str, k: &str) -> (u16, Value) {
    m.req(
        srv,
        Method::POST,
        &format!("/v1/groups/{gid}/streams"),
        Some(json!({ "title": "Sprint demo", "sources": ["camera", "screen"], "public_key": k })),
    )
    .await
}

async fn internal(srv: &Server, path: &str, body: Value) -> (u16, Value) {
    srv.call(Method::POST, path, CALLS, None, None, Some(body))
        .await
}

async fn set_role(srv: &Server, admin: &Member, gid: &str, who: &str, body: Value) -> u16 {
    let (_, v) = admin
        .req(srv, Method::GET, &format!("/v1/groups/{gid}/members"), None)
        .await;
    let r = v["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["address"] == who)
        .unwrap()["revision"]
        .clone();
    let mut body = body;
    body["expected_revision"] = r;
    let (s, v) = admin
        .req(
            srv,
            Method::PATCH,
            &format!("/v1/groups/{gid}/members/{}", who.replace('/', "%2F")),
            Some(body),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    s
}

async fn stage(srv: &Server, sid: &str, m: &Member, who: &str, on: bool) -> (u16, Value) {
    m.req(
        srv,
        Method::POST,
        &format!("/v1/streams/{sid}/stage"),
        Some(json!({ "address": who, "on": on })),
    )
    .await
}

#[tokio::test]
async fn streams_need_the_flag() {
    let srv = serve().await;
    let (alice, _bob, _carol, gid) = three(&srv).await;
    let (s, v) = go_live(&srv, &alice, &gid, &key('A')).await;
    assert_eq!((s, v["error"].as_str()), (404, Some("feature_disabled")));
}

#[tokio::test]
async fn going_live_follows_the_stream_policy_and_one_stream_per_group() {
    let srv = serve().await;
    streaming_on(&srv).await;
    let (alice, bob, carol, gid) = three(&srv).await;
    // Admins only, by default: Bob is a member.
    let (s, v) = go_live(&srv, &bob, &gid, &key('B')).await;
    assert_eq!(
        (s, v["error"].as_str()),
        (403, Some("stream_restricted")),
        "{v}"
    );
    // Carol declined: not a member at all.
    let (s, v) = go_live(&srv, &carol, &gid, &key('C')).await;
    assert_eq!((s, v["error"].as_str()), (403, Some("not_member")), "{v}");
    // Bad input.
    let (s, _) = alice
        .req(
            &srv,
            Method::POST,
            &format!("/v1/groups/{gid}/streams"),
            Some(json!({ "title": "x", "sources": [], "public_key": key('A') })),
        )
        .await;
    assert_eq!(s, 400);
    let (s, _) = alice
        .req(
            &srv,
            Method::POST,
            &format!("/v1/groups/{gid}/streams"),
            Some(json!({ "title": "x", "sources": ["camera"], "public_key": "nope" })),
        )
        .await;
    assert_eq!(s, 400);
    // Everyone may go live once the owner says so.
    let g = alice.group(&srv, &gid).await;
    assert_eq!(g["stream_policy"], "admins");
    let (s, v) = alice
        .req(
            &srv,
            Method::PATCH,
            &format!("/v1/groups/{gid}"),
            Some(json!({ "expected_revision": g["revision"], "stream_policy": "everyone" })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["group"]["stream_policy"], "everyone");
    let (s, v) = go_live(&srv, &bob, &gid, &key('B')).await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["role"], "host");
    assert_eq!(v["host_key"], key('B'));
    let sid = v["stream"]["id"].as_str().unwrap().to_string();
    assert_eq!(v["media"]["join"], format!("/calls/v1/streams/{sid}/join"));
    assert!(
        v["stream"]["ends_by"].as_i64().unwrap() - v["stream"]["started_at"].as_i64().unwrap()
            == 4 * 3600
    );
    // One at a time.
    let (s, v) = go_live(&srv, &alice, &gid, &key('A')).await;
    assert_eq!((s, v["error"].as_str()), (409, Some("stream_in_progress")));
    assert_eq!(v["stream_id"], sid.as_str());
    // Everyone in the group sees it; nobody else does.
    let (s, v) = alice
        .req(
            &srv,
            Method::GET,
            &format!("/v1/groups/{gid}/streams"),
            None,
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(v["streams"][0]["id"], sid.as_str());
    assert_eq!(v["streams"][0]["by"], bob.address.as_str());
    let (s, _) = carol
        .req(&srv, Method::GET, &format!("/v1/streams/{sid}"), None)
        .await;
    assert_eq!(s, 404);
    let (s, _) = carol
        .req(
            &srv,
            Method::POST,
            &format!("/v1/streams/{sid}/watch"),
            Some(json!({ "public_key": key('C') })),
        )
        .await;
    assert_eq!(s, 404);
    // Agents (API keys) may watch but not go live.
    let (s, v) = srv
        .call(
            Method::POST,
            &format!("/v1/streams/{sid}/watch"),
            &alice.api_key,
            Some(&alice.address),
            None,
            Some(json!({ "public_key": key('K') })),
        )
        .await;
    assert_eq!((s, v["role"].as_str()), (200, Some("viewer")), "{v}");
    let (s, v) = srv
        .call(
            Method::DELETE,
            &format!("/v1/streams/{sid}"),
            &alice.api_key,
            Some(&alice.address),
            None,
            None,
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (403, Some("human_required")),
        "{v}"
    );
}

#[tokio::test]
async fn watching_registers_keys_and_only_the_streamer_reads_the_directory() {
    let srv = serve().await;
    streaming_on(&srv).await;
    let (alice, bob, _carol, gid) = three(&srv).await;
    let (_, v) = go_live(&srv, &alice, &gid, &key('A')).await;
    let sid = v["stream"]["id"].as_str().unwrap().to_string();
    let (s, v) = bob
        .req(
            &srv,
            Method::POST,
            &format!("/v1/streams/{sid}/watch"),
            Some(json!({ "public_key": key('B') })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["role"], "viewer");
    assert_eq!(v["host_key"], key('A'));
    // A second device of Bob's, then too many: the oldest drops out.
    for c in ['C', 'D', 'E', 'F'] {
        let (s, _) = bob
            .req(
                &srv,
                Method::POST,
                &format!("/v1/streams/{sid}/watch"),
                Some(json!({ "public_key": key(c) })),
            )
            .await;
        assert_eq!(s, 200);
    }
    let (s, v) = bob
        .req(
            &srv,
            Method::GET,
            &format!("/v1/streams/{sid}/audience"),
            None,
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (403, Some("not_host")));
    let (s, v) = alice
        .req(
            &srv,
            Method::GET,
            &format!("/v1/streams/{sid}/audience"),
            None,
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let keys: Vec<&str> = v["audience"][0]["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_str().unwrap())
        .collect();
    assert_eq!(v["audience"][0]["address"], bob.address.as_str());
    assert_eq!(keys.len(), 4);
    assert!(!keys.contains(&key('B').as_str()), "{keys:?}");
    // The calls service learns roles and whether a key is attested.
    let (s, v) = internal(
        &srv,
        "/internal/v1/stream-admission",
        json!({ "stream_id": sid, "from": { "mailbox": bob.address }, "public_key": key('F') }),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (
            v["allowed"].as_bool(),
            v["role"].as_str(),
            v["key_attested"].as_bool()
        ),
        (Some(true), Some("viewer"), Some(true))
    );
    let (_, v) = internal(
        &srv,
        "/internal/v1/stream-admission",
        json!({ "stream_id": sid, "from": { "mailbox": bob.address }, "public_key": key('Z') }),
    )
    .await;
    assert_eq!(v["key_attested"], false);
    let (_, v) = internal(
        &srv,
        "/internal/v1/stream-admission",
        json!({ "stream_id": sid, "from": { "mailbox": alice.address }, "public_key": key('A') }),
    )
    .await;
    assert_eq!(
        (v["role"].as_str(), v["key_attested"].as_bool()),
        (Some("host"), Some(true))
    );
    // Only the calls service may ask.
    let (s, _) = srv
        .call(
            Method::POST,
            "/internal/v1/stream-admission",
            OPS,
            None,
            None,
            Some(json!({ "stream_id": sid, "from": { "mailbox": bob.address } })),
        )
        .await;
    assert_eq!(s, 403);
    // The streamer reloads: the new sending key replaces the old one.
    let (s, v) = alice
        .req(
            &srv,
            Method::POST,
            &format!("/v1/streams/{sid}/watch"),
            Some(json!({ "public_key": key('H') })),
        )
        .await;
    assert_eq!(
        (s, v["role"].as_str(), v["host_key"].as_str()),
        (200, Some("host"), Some(key('H').as_str()))
    );
}

#[tokio::test]
async fn the_stage_needs_an_invitation_and_acceptance() {
    let srv = serve().await;
    streaming_on(&srv).await;
    let (alice, bob, _carol, gid) = three(&srv).await;
    let (_, v) = go_live(&srv, &alice, &gid, &key('A')).await;
    let sid = v["stream"]["id"].as_str().unwrap().to_string();
    // Nobody steps on uninvited.
    let (s, v) = stage(&srv, &sid, &bob, &bob.address, true).await;
    assert_eq!((s, v["error"].as_str()), (403, Some("not_invited")), "{v}");
    // A viewer cannot invite.
    let (s, v) = stage(&srv, &sid, &bob, &alice.address, true).await;
    assert_eq!((s, v["error"].as_str()), (403, Some("not_host")), "{v}");
    let (s, v) = stage(&srv, &sid, &alice, &bob.address, true).await;
    assert_eq!((s, v["role"].as_str()), (200, Some("invited")), "{v}");
    let (_, v) = internal(
        &srv,
        "/internal/v1/stream-admission",
        json!({ "stream_id": sid, "from": { "mailbox": bob.address } }),
    )
    .await;
    assert_eq!(
        v["role"], "invited",
        "an invitation alone grants no publishing"
    );
    let (s, v) = stage(&srv, &sid, &bob, &bob.address, true).await;
    assert_eq!((s, v["role"].as_str()), (200, Some("speaker")), "{v}");
    let (_, v) = internal(
        &srv,
        "/internal/v1/stream-check",
        json!({ "stream_id": sid, "viewers": 1, "members": [alice.address, bob.address] }),
    )
    .await;
    assert_eq!(v["roles"][&bob.address], "speaker");
    assert_eq!(v["roles"][&alice.address], "host");
    // Restricting Bob takes the microphone away at once, whatever the stage row says.
    set_role(
        &srv,
        &alice,
        &gid,
        &bob.address,
        json!({ "role": "restricted" }),
    )
    .await;
    let (_, v) = internal(
        &srv,
        "/internal/v1/stream-check",
        json!({ "stream_id": sid, "viewers": 1, "members": [bob.address] }),
    )
    .await;
    assert_eq!(v["roles"][&bob.address], "viewer");
    // The streamer takes someone off the stage.
    set_role(
        &srv,
        &alice,
        &gid,
        &bob.address,
        json!({ "role": "member" }),
    )
    .await;
    let (s, v) = stage(&srv, &sid, &alice, &bob.address, false).await;
    assert_eq!((s, v["role"].as_str()), (200, Some("viewer")), "{v}");
}

#[tokio::test]
async fn removal_revokes_and_stopping_posts_one_notice() {
    let srv = serve().await;
    streaming_on(&srv).await;
    let (mut alice, bob, _carol, gid) = three(&srv).await;
    let (_, v) = go_live(&srv, &alice, &gid, &key('A')).await;
    let sid = v["stream"]["id"].as_str().unwrap().to_string();
    let (s, _) = bob
        .req(
            &srv,
            Method::POST,
            &format!("/v1/streams/{sid}/watch"),
            Some(json!({ "public_key": key('B') })),
        )
        .await;
    assert_eq!(s, 200);
    let (_, v) = internal(
        &srv,
        "/internal/v1/stream-check",
        json!({ "stream_id": sid, "viewers": 1, "members": [bob.address] }),
    )
    .await;
    assert_eq!(v["roles"][&bob.address], "viewer");
    assert_eq!(v["state"], "live");
    // Bob is removed: the calls service hears "no role", the directory drops his keys.
    set_role(
        &srv,
        &alice,
        &gid,
        &bob.address,
        json!({ "state": "removed" }),
    )
    .await;
    alice.process_pending(&srv, &gid).await;
    let (_, v) = internal(
        &srv,
        "/internal/v1/stream-check",
        json!({ "stream_id": sid, "viewers": 1, "members": [bob.address] }),
    )
    .await;
    assert!(v["roles"][&bob.address].is_null(), "{v}");
    let (_, v) = alice
        .req(
            &srv,
            Method::GET,
            &format!("/v1/streams/{sid}/audience"),
            None,
        )
        .await;
    assert_eq!(v["audience"].as_array().unwrap().len(), 0, "{v}");
    let (_, v) = internal(
        &srv,
        "/internal/v1/stream-admission",
        json!({ "stream_id": sid, "from": { "mailbox": bob.address }, "public_key": key('B') }),
    )
    .await;
    assert_eq!(v["allowed"], false);
    let (s, _) = bob
        .req(
            &srv,
            Method::POST,
            &format!("/v1/streams/{sid}/watch"),
            Some(json!({ "public_key": key('C') })),
        )
        .await;
    assert_eq!(s, 404);
    // Stop; stopping twice posts one notice.
    for _ in 0..2 {
        let (s, v) = alice
            .req(&srv, Method::DELETE, &format!("/v1/streams/{sid}"), None)
            .await;
        assert_eq!(
            (s, v["stream"]["state"].as_str()),
            (200, Some("ended")),
            "{v}"
        );
    }
    let (_, v) = alice
        .req(
            &srv,
            Method::GET,
            &format!("/v1/groups/{gid}/messages?after_seq=0"),
            None,
        )
        .await;
    let notices: Vec<&Value> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["kind"] == "stream")
        .collect();
    assert_eq!(notices.len(), 1, "{v}");
    assert_eq!(notices[0]["actor"], alice.address.as_str());
    assert_eq!(notices[0]["metadata"]["viewers"], 1);
    assert_eq!(notices[0]["metadata"]["stream_id"], sid.as_str());
    assert!(
        notices[0]["metadata"].get("title").is_none(),
        "the notice carries no title"
    );
    let (_, v) = internal(
        &srv,
        "/internal/v1/stream-check",
        json!({ "stream_id": sid, "viewers": 0, "members": [alice.address] }),
    )
    .await;
    assert_eq!(v["state"], "ended");
    assert!(v["roles"][&alice.address].is_null());
    // A new stream can start now.
    let (s, _) = go_live(&srv, &alice, &gid, &key('A')).await;
    assert_eq!(s, 201);
}

#[tokio::test]
async fn streams_end_after_four_hours_or_without_reports_and_rows_expire() {
    let srv = serve().await;
    streaming_on(&srv).await;
    let alice = member(&srv, &format!("solo{}", rand_hex(3))).await;
    let mut alice = alice;
    let gid = alice.create(&srv, "solo", json!({})).await;
    let (_, v) = go_live(&srv, &alice, &gid, &key('A')).await;
    let sid = v["stream"]["id"].as_str().unwrap().to_string();
    let now = now_unix();
    // Still reported: nothing happens.
    let s2 = sid.clone();
    let wake = srv
        .state
        .store
        .group_tx(move |tx| streams::sweep_in(tx, now + 60))
        .await
        .unwrap();
    assert!(wake.is_empty());
    let _ = s2;
    // Not reported for two minutes: lost.
    srv.state
        .store
        .group_tx(move |tx| streams::sweep_in(tx, now + streams::BEAT_TIMEOUT_SECS + 1))
        .await
        .unwrap();
    let (_, v) = alice
        .req(&srv, Method::GET, &format!("/v1/streams/{sid}"), None)
        .await;
    assert_eq!(
        (
            v["stream"]["state"].as_str(),
            v["stream"]["end_reason"].as_str()
        ),
        (Some("ended"), Some("lost"))
    );
    // Four hours, even while reported.
    let (_, v) = go_live(&srv, &alice, &gid, &key('A')).await;
    let sid2 = v["stream"]["id"].as_str().unwrap().to_string();
    let later = now + streams::MAX_STREAM_SECS + 5;
    srv.state
        .store
        .group_tx(move |tx| streams::sweep_in(tx, later))
        .await
        .unwrap();
    let (_, v) = alice
        .req(&srv, Method::GET, &format!("/v1/streams/{sid2}"), None)
        .await;
    assert_eq!(v["stream"]["end_reason"], "max_duration");
    // A week later the rows are gone; the notices stay.
    let much_later = later + streams::RETAIN_ENDED_SECS + 5;
    srv.state
        .store
        .group_tx(move |tx| streams::sweep_in(tx, much_later))
        .await
        .unwrap();
    let (s, _) = alice
        .req(&srv, Method::GET, &format!("/v1/streams/{sid2}"), None)
        .await;
    assert_eq!(s, 404);
    let (_, v) = alice
        .req(
            &srv,
            Method::GET,
            &format!("/v1/groups/{gid}/messages?after_seq=0"),
            None,
        )
        .await;
    assert_eq!(
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|x| x["kind"] == "stream")
            .count(),
        2
    );
}

#[tokio::test]
async fn a_policy_change_ends_a_members_stream_and_the_calls_service_can_end_one() {
    let srv = serve().await;
    streaming_on(&srv).await;
    let (alice, bob, _carol, gid) = three(&srv).await;
    let g = alice.group(&srv, &gid).await;
    alice
        .req(
            &srv,
            Method::PATCH,
            &format!("/v1/groups/{gid}"),
            Some(json!({ "expected_revision": g["revision"], "stream_policy": "everyone" })),
        )
        .await;
    let (s, v) = go_live(&srv, &bob, &gid, &key('B')).await;
    assert_eq!(s, 201, "{v}");
    let sid = v["stream"]["id"].as_str().unwrap().to_string();
    let g = alice.group(&srv, &gid).await;
    alice
        .req(
            &srv,
            Method::PATCH,
            &format!("/v1/groups/{gid}"),
            Some(json!({ "expected_revision": g["revision"], "stream_policy": "admins" })),
        )
        .await;
    let (_, v) = internal(
        &srv,
        "/internal/v1/stream-check",
        json!({ "stream_id": sid, "viewers": 0, "members": [bob.address] }),
    )
    .await;
    assert_eq!(
        (v["state"].as_str(), v["reason"].as_str()),
        (Some("ended"), Some("policy")),
        "{v}"
    );
    // The calls service ends the owner's stream when the streamer leaves for good.
    let (_, v) = go_live(&srv, &alice, &gid, &key('A')).await;
    let sid = v["stream"]["id"].as_str().unwrap().to_string();
    let (s, v) = internal(
        &srv,
        "/internal/v1/stream-end",
        json!({ "stream_id": sid, "reason": "host_left" }),
    )
    .await;
    assert_eq!((s, v["state"].as_str()), (200, Some("ended")), "{v}");
    let (s, _) = internal(
        &srv,
        "/internal/v1/stream-end",
        json!({ "stream_id": sid, "reason": "whatever" }),
    )
    .await;
    assert_eq!(s, 400);
}

#[tokio::test]
async fn stream_events_reach_members() {
    let srv = serve().await;
    streaming_on(&srv).await;
    let (alice, bob, _carol, gid) = three(&srv).await;
    let mut stream = srv
        .http
        .get(format!("{}/v1/events?typed=1", srv.public))
        .bearer_auth(&bob.token)
        .send()
        .await
        .unwrap();
    let (_, v) = go_live(&srv, &alice, &gid, &key('A')).await;
    let sid = v["stream"]["id"].as_str().unwrap().to_string();
    alice
        .req(&srv, Method::DELETE, &format!("/v1/streams/{sid}"), None)
        .await;
    let mut seen = String::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    while let Ok(Ok(Some(chunk))) = tokio::time::timeout_at(deadline, stream.chunk()).await {
        seen.push_str(&String::from_utf8_lossy(&chunk));
        if seen.matches("event: group.stream").count() >= 2 {
            break;
        }
    }
    assert!(seen.contains("event: group.stream"), "{seen}");
    assert!(
        seen.contains("\"state\":\"live\"") && seen.contains("\"state\":\"ended\""),
        "{seen}"
    );
}
