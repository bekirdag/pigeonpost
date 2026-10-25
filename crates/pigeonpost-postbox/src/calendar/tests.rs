//! End-to-end tests for Phase 3B over real sockets (public and internal listeners).

use crate::*;
use reqwest::{Client, Method};
use serde_json::{json, Value};

const OPS: &str = "svc_ops_0123456789abcdef0123456789abcdef";
const CALLS: &str = "svc_calls_0123456789abcdef0123456789abcdef";
const ISSUER: &str = "https://auth.example/realms/x";

struct Srv {
    public: String,
    internal: String,
    http: Client,
    state: AppState,
}

async fn serve() -> Srv {
    let mut state = crate::tests::test_state();
    state.service_tokens = Arc::new(internal_api::tokens_for_test(&[
        ("POSTBOX_SVC_TOKEN_OPS", OPS),
        ("POSTBOX_SVC_TOKEN_CALLS", CALLS),
    ]));
    let public = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let internal = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let public_url = format!("http://{}", public.local_addr().unwrap());
    let internal_url = format!("http://{}", internal.local_addr().unwrap());
    let app =
        build_router(state.clone()).into_make_service_with_connect_info::<std::net::SocketAddr>();
    let iapp = internal_api::router(state.clone())
        .into_make_service_with_connect_info::<std::net::SocketAddr>();
    tokio::spawn(async move { axum::serve(public, app).await.unwrap() });
    tokio::spawn(async move { axum::serve(internal, iapp).await.unwrap() });
    Srv {
        public: public_url,
        internal: internal_url,
        http: Client::new(),
        state,
    }
}

impl Srv {
    async fn raw(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        mailbox: Option<&str>,
        body: Option<Value>,
    ) -> (u16, String) {
        let base = if path.starts_with("/internal/") {
            &self.internal
        } else {
            &self.public
        };
        let mut req = self.http.request(method, format!("{base}{path}"));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        if let Some(m) = mailbox {
            req = req.header("x-pigeonpost-identity", m);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.unwrap();
        (resp.status().as_u16(), resp.text().await.unwrap())
    }
    async fn call(&self, method: Method, path: &str, who: &P, body: Option<Value>) -> (u16, Value) {
        let (s, t) = self
            .raw(method, path, Some(&who.token), Some(&who.address), body)
            .await;
        (s, serde_json::from_str(&t).unwrap_or(Value::Null))
    }
    async fn anon(&self, path: &str, body: Value) -> (u16, Value) {
        let (s, t) = self.raw(Method::POST, path, None, None, Some(body)).await;
        (s, serde_json::from_str(&t).unwrap_or(Value::Null))
    }
    async fn access(&self, body: Value) -> Value {
        let (s, t) = self
            .raw(
                Method::POST,
                "/internal/v1/calendar/meeting-access",
                Some(CALLS),
                None,
                Some(body),
            )
            .await;
        assert_eq!(s, 200, "{t}");
        serde_json::from_str(&t).unwrap()
    }
    async fn flag(&self, name: &str, account: &str) {
        let (s, _) = self
            .raw(
                Method::PUT,
                &format!("/internal/v1/features/{name}"),
                Some(OPS),
                None,
                Some(json!({ "scope": "account", "account_id": account, "enabled": true })),
            )
            .await;
        assert_eq!(s, 200);
    }
    async fn events_of(&self, account: &str) -> Vec<(String, Value)> {
        let conn = self.state.store.clone();
        let account = account.to_string();
        conn.group_read(
            move |c: &rusqlite::Connection| -> Result<Vec<(String, Value)>, crate::groups::GErr> {
                let mut stmt = c.prepare(
                    "SELECT type, data FROM account_events WHERE account_id = ?1 ORDER BY id",
                )?;
                let rows = stmt
                    .query_map([account], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows
                    .into_iter()
                    .map(|(t, d)| (t, serde_json::from_str(&d).unwrap()))
                    .collect())
            },
        )
        .await
        .unwrap()
    }
}

/// A person: member session (with a verified sign-in address), account API key, one mailbox
/// with a handle.
struct P {
    token: String,
    api_key: String,
    account: String,
    address: String,
    handle: String,
}

async fn person(srv: &Srv, name: &str) -> P {
    let account = format!("acct_{name}");
    let sub = format!("sub-{name}");
    srv.state
        .store
        .account_for_sub(sub.clone(), account.clone(), now_unix())
        .await
        .unwrap();
    let (api_key, key) = new_api_key();
    srv.state
        .store
        .add_api_key(account.clone(), key, now_unix())
        .await
        .unwrap();
    srv.state
        .store
        .set_namespace_owner(name.into(), account.clone(), "test", 1, None)
        .await
        .unwrap();
    let handle = format!("/{name}/main");
    let v = do_create_identity(
        &srv.state,
        Some(account.clone()),
        None,
        Some(handle.clone()),
    )
    .await
    .unwrap();
    for f in ["calendar", "account_events", "message_filtering"] {
        srv.flag(f, &account).await;
    }
    P {
        token: oidc::test_member_token_with_address(ISSUER, &sub, &format!("{name}@example.com")),
        api_key,
        account,
        address: v["address"].as_str().unwrap().to_string(),
        handle,
    }
}

/// Wall time `mins` from now (UTC), minute precision.
fn soon(mins: i64) -> String {
    let t = (now_unix() as i64 / 60 + mins) * 60;
    chrono::DateTime::<chrono::Utc>::from_timestamp(t, 0)
        .unwrap()
        .format("%Y-%m-%dT%H:%M")
        .to_string()
}

async fn create(srv: &Srv, who: &P, body: Value) -> Value {
    let (s, v) = srv
        .call(Method::POST, "/v1/calendar/events", who, Some(body))
        .await;
    assert_eq!(s, 201, "{v}");
    v["event"].clone()
}

fn meeting_body(attendees: Value) -> Value {
    json!({
        "request_key": format!("k{}", rand_hex(6)), "title": "Design review",
        "start": soon(5), "end": soon(35), "tzid": "UTC",
        "attendees": attendees,
        "meeting": { "enabled": true, "waiting": "guests", "admit": "hosts" },
    })
}

#[tokio::test]
async fn subminute_event_times_survive_save_read_and_edits() {
    let srv = serve().await;
    let who = person(&srv, "precise").await;
    let ev = create(
        &srv,
        &who,
        json!({"request_key":"precision", "title":"Precise",
            "start":"2026-10-22T09:00:17", "end":"2026-10-22T09:01:47",
            "tzid":"Europe/Istanbul", "rrule":"FREQ=DAILY;COUNT=2"}),
    )
    .await;
    let id = ev["id"].as_str().unwrap();
    let start = ev["starts_at"].as_i64().unwrap();
    let list_path = format!(
        "/v1/calendar/events?from={}&to={}",
        start - 1,
        start + 3 * 86_400
    );
    let (status, listed) = srv.call(Method::GET, &list_path, &who, None).await;
    assert_eq!(status, 200, "{listed}");
    let instances = listed["occurrences"].as_array().unwrap();
    assert_eq!(instances.len(), 2);
    assert_eq!(
        instances[0]["starts_at"], start,
        "persisting must not shift the schedule"
    );
    assert_eq!(instances[0]["ends_at"].as_i64().unwrap() - start, 90);
    let occurrence = instances[0]["occurrence_id"].as_str().unwrap();
    assert!(occurrence.ends_with("T090017"));
    let detail_path = format!("/v1/calendar/events/{id}?occurrence_id={occurrence}");
    let (status, body) = srv.call(Method::GET, &detail_path, &who, None).await;
    assert_eq!(status, 200);
    assert_eq!(body["event"]["start"], "2026-10-22T09:00:17");
    assert_eq!(body["event"]["end"], "2026-10-22T09:01:47");
    let (status, changed) = srv
        .call(
            Method::PATCH,
            &format!("/v1/calendar/events/{id}"),
            &who,
            Some(json!({"expected_revision":body["event"]["revision"], "scope":"all", "title":"Renamed"})),
        )
        .await;
    assert_eq!(status, 200, "{changed}");
    let (status, after) = srv.call(Method::GET, &list_path, &who, None).await;
    assert_eq!(status, 200);
    assert_eq!(after["occurrences"][0]["starts_at"], start);
    assert_eq!(after["occurrences"][0]["occurrence_id"], occurrence);
    let (status, moved) = srv
        .call(
            Method::PATCH,
            &format!("/v1/calendar/events/{id}"),
            &who,
            Some(json!({"expected_revision":changed["event"]["revision"], "scope":"this",
                "occurrence_id":occurrence, "start":"2026-10-24T14:30:23", "end":"2026-10-24T14:31:53", "tzid":"Europe/Istanbul"})),
        )
        .await;
    assert_eq!(status, 200, "{moved}");
    let (status, moved_read) = srv.call(Method::GET, &detail_path, &who, None).await;
    assert_eq!(status, 200);
    assert_eq!(moved_read["occurrence"]["start"], "2026-10-24T14:30:23");
    assert_eq!(moved_read["occurrence"]["end"], "2026-10-24T14:31:53");
    assert_eq!(moved_read["occurrence"]["occurrence_id"], occurrence);
}

#[tokio::test]
async fn subminute_ics_times_survive_preview_confirm_and_export() {
    let srv = serve().await;
    let who = person(&srv, "preciseics").await;
    let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:precision@example.test\r\nSUMMARY:Precise import\r\nDTSTART;TZID=Europe/Istanbul:20261022T090017\r\nDTEND;TZID=Europe/Istanbul:20261022T090147\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let response = srv
        .http
        .post(format!("{}/v1/calendar/import/preview", srv.public))
        .bearer_auth(&who.token)
        .header("x-pigeonpost-identity", &who.address)
        .header("content-type", "text/calendar")
        .body(ics)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let preview: Value = response.json().await.unwrap();
    assert_eq!(preview["events"][0]["start"], "2026-10-22T09:00:17");
    let draft = preview["draft_id"].as_str().unwrap();
    let (status, confirmed) = srv
        .call(
            Method::POST,
            &format!("/v1/calendar/import/{draft}/confirm"),
            &who,
            Some(json!({"select":[0]})),
        )
        .await;
    assert_eq!(status, 200, "{confirmed}");
    let id = confirmed["created"][0]["id"].as_str().unwrap();
    let (status, detail) = srv
        .call(
            Method::GET,
            &format!("/v1/calendar/events/{id}"),
            &who,
            None,
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(detail["event"]["start"], "2026-10-22T09:00:17");
    assert_eq!(detail["event"]["end"], "2026-10-22T09:01:47");
    let (status, exported) = srv
        .raw(
            Method::GET,
            &format!("/v1/calendar/events/{id}.ics"),
            Some(&who.token),
            Some(&who.address),
            None,
        )
        .await;
    assert_eq!(status, 200);
    assert!(exported.contains("20261022T090017") && exported.contains("20261022T090147"));
}

#[tokio::test]
async fn invitations_follow_admission_and_projections_hide_contacts() {
    let srv = serve().await;
    let org = person(&srv, "orga").await;
    let alice = person(&srv, "alicea").await;
    let bob = person(&srv, "boba").await;
    let carol = person(&srv, "carola").await;
    // Bob blocked the organizer; Carol filters strangers.
    srv.call(
        Method::PUT,
        "/v1/contacts",
        &bob,
        Some(json!({ "peer": org.address, "admission": "block" })),
    )
    .await;
    let (s, _) = srv
        .call(
            Method::PUT,
            "/v1/policy",
            &carol,
            Some(json!({ "unknown_messages": "filter" })),
        )
        .await;
    assert_eq!(s, 200);
    let ev = create(
        &srv,
        &org,
        meeting_body(json!([
            {"handle": alice.handle}, {"handle": bob.handle}, {"handle": carol.handle},
            {"contact": "Guest.Person@Example.COM", "name": "Guest"}
        ])),
    )
    .await;
    let id = ev["id"].as_str().unwrap().to_string();
    // Organizer sees four invitees, the guest's address, and nothing that reveals Bob's block.
    let atts = ev["attendees"].as_array().unwrap();
    assert_eq!(atts.len(), 4);
    let bob_row = atts.iter().find(|a| a["handle"] == bob.handle).unwrap();
    assert_eq!(bob_row["rsvp"], "needs_action");
    assert_eq!(bob_row["delivery"], "delivered");
    let guest_row = atts.iter().find(|a| a["kind"] == "external").unwrap();
    assert_eq!(guest_row["contact"], "Guest.Person@example.com");
    assert!(ev["meeting"]["code"].as_str().unwrap().len() == 10);

    // Alice: pending invitation, event detail without the guest's address.
    let (_, inv) = srv
        .call(Method::GET, "/v1/calendar/invitations", &alice, None)
        .await;
    assert_eq!(inv["invitations"].as_array().unwrap().len(), 1);
    let (s, detail) = srv
        .call(
            Method::GET,
            &format!("/v1/calendar/events/{id}"),
            &alice,
            None,
        )
        .await;
    assert_eq!(s, 200);
    assert!(!detail.to_string().contains("Guest.Person"));
    assert!(detail["event"]["attendees"]
        .as_array()
        .unwrap()
        .iter()
        .all(|a| a["handle"] != bob.handle));
    // Bob: nothing at all.
    let (s, _) = srv
        .call(
            Method::GET,
            &format!("/v1/calendar/events/{id}"),
            &bob,
            None,
        )
        .await;
    assert_eq!(s, 404);
    let (_, inv) = srv
        .call(Method::GET, "/v1/calendar/invitations", &bob, None)
        .await;
    assert!(inv["invitations"].as_array().unwrap().is_empty());
    assert!(srv
        .events_of(&bob.account)
        .await
        .iter()
        .all(|(t, _)| !t.starts_with("event.")));
    // Carol: only under Filtered, until she answers.
    let (_, inv) = srv
        .call(Method::GET, "/v1/calendar/invitations", &carol, None)
        .await;
    assert!(inv["invitations"].as_array().unwrap().is_empty());
    let (_, inv) = srv
        .call(
            Method::GET,
            "/v1/calendar/invitations?state=filtered",
            &carol,
            None,
        )
        .await;
    assert_eq!(inv["invitations"].as_array().unwrap().len(), 1);
    let (s, _) = srv
        .call(
            Method::POST,
            &format!("/v1/calendar/events/{id}/rsvp"),
            &carol,
            Some(json!({"status": "tentative"})),
        )
        .await;
    assert_eq!(s, 200);
    let (_, inv) = srv
        .call(
            Method::GET,
            "/v1/calendar/invitations?state=filtered",
            &carol,
            None,
        )
        .await;
    assert!(inv["invitations"].as_array().unwrap().is_empty());
    // A stranger cannot see it, edit it, or learn it exists.
    let eve = person(&srv, "evea").await;
    let (s, _) = srv
        .call(
            Method::GET,
            &format!("/v1/calendar/events/{id}"),
            &eve,
            None,
        )
        .await;
    assert_eq!(s, 404);
    // An attendee cannot edit.
    let (s, _) = srv
        .call(
            Method::PATCH,
            &format!("/v1/calendar/events/{id}"),
            &alice,
            Some(json!({"expected_revision": 1, "title": "x"})),
        )
        .await;
    assert_eq!(s, 403);
    // Organizer RSVP event recorded.
    assert!(srv
        .events_of(&org.account)
        .await
        .iter()
        .any(|(t, _)| t == "event.rsvp"));
}

#[tokio::test]
async fn contact_guests_need_a_person_with_a_verified_address_and_limits_hold() {
    let srv = serve().await;
    let org = person(&srv, "orgb").await;
    // API key: refused for contact addresses, fine for handle-only.
    let body = meeting_body(json!([{ "contact": "x@example.com" }]));
    let (s, t) = srv
        .raw(
            Method::POST,
            "/v1/calendar/events",
            Some(&org.api_key),
            Some(&org.address),
            Some(body),
        )
        .await;
    assert_eq!(s, 403, "{t}");
    assert!(t.contains("human_required"));
    let mut many = Vec::new();
    for i in 0..51 {
        many.push(json!({ "contact": format!("g{i}@example.com") }));
    }
    let mut body = meeting_body(Value::Array(many));
    body["meeting"] = json!({ "enabled": false });
    let (s, v) = srv
        .call(Method::POST, "/v1/calendar/events", &org, Some(body))
        .await;
    assert_eq!((s, v["error"].as_str()), (400, Some("attendee_limit")));
    // Six invitees plus the organizer do not fit a mesh meeting.
    let mut six = Vec::new();
    for i in 0..6 {
        six.push(json!({ "contact": format!("m{i}@example.com") }));
    }
    let (s, v) = srv
        .call(
            Method::POST,
            "/v1/calendar/events",
            &org,
            Some(meeting_body(Value::Array(six.clone()))),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (400, Some("meeting_capacity")));
    let mut body = meeting_body(Value::Array(six));
    body["meeting"]["transport"] = json!("sfu");
    let (s, _) = srv
        .call(Method::POST, "/v1/calendar/events", &org, Some(body))
        .await;
    assert_eq!(s, 201);
    // Unknown handle.
    let (s, v) = srv
        .call(
            Method::POST,
            "/v1/calendar/events",
            &org,
            Some(meeting_body(json!([{"handle": "/nobody_here"}]))),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (400, Some("attendee_unknown")));
    // All-day meetings are refused.
    let body = json!({ "request_key": "ad1", "title": "Offsite", "all_day": true, "start_date": "2026-12-01",
                       "end_date": "2026-12-02", "meeting": {"enabled": true} });
    let (s, _) = srv
        .call(Method::POST, "/v1/calendar/events", &org, Some(body))
        .await;
    assert_eq!(s, 400);
    // Idempotent create.
    let body = json!({ "request_key": "same", "title": "One", "start": "2026-12-01T10:00", "end": "2026-12-01T11:00", "tzid": "Europe/Istanbul" });
    let (_, a) = srv
        .call(
            Method::POST,
            "/v1/calendar/events",
            &org,
            Some(body.clone()),
        )
        .await;
    let (_, b) = srv
        .call(Method::POST, "/v1/calendar/events", &org, Some(body))
        .await;
    assert_eq!(a["event"]["id"], b["event"]["id"]);
}

#[tokio::test]
async fn gaps_and_folds_are_refused_and_series_keep_wall_time_across_dst() {
    let srv = serve().await;
    let org = person(&srv, "orgc").await;
    let base = json!({ "title": "Standup", "tzid": "America/New_York" });
    let mut gap = base.clone();
    gap["request_key"] = json!("g1");
    gap["start"] = json!("2026-03-08T02:30");
    gap["end"] = json!("2026-03-08T03:30");
    let (s, v) = srv
        .call(Method::POST, "/v1/calendar/events", &org, Some(gap))
        .await;
    assert_eq!((s, v["kind"].as_str()), (400, Some("nonexistent")));
    let mut fold = base.clone();
    fold["request_key"] = json!("f1");
    fold["start"] = json!("2026-11-01T01:30");
    fold["end"] = json!("2026-11-01T01:45");
    let (s, v) = srv
        .call(
            Method::POST,
            "/v1/calendar/events",
            &org,
            Some(fold.clone()),
        )
        .await;
    assert_eq!((s, v["kind"].as_str()), (400, Some("ambiguous")));
    fold["fold"] = json!("later");
    fold["request_key"] = json!("f2");
    let (s, _) = srv
        .call(Method::POST, "/v1/calendar/events", &org, Some(fold))
        .await;
    assert_eq!(s, 201);

    // Weekly Thursday 09:00 New York, across 2026-11-01.
    let mut series = base.clone();
    series["request_key"] = json!("s1");
    series["start"] = json!("2026-10-22T09:00");
    series["end"] = json!("2026-10-22T09:30");
    series["rrule"] = json!("FREQ=WEEKLY;COUNT=6");
    let ev = create(&srv, &org, series).await;
    let id = ev["id"].as_str().unwrap();
    let from = 1_792_627_200i64; // 2026-10-21T00:00Z
    let (s, list) = srv
        .call(
            Method::GET,
            &format!("/v1/calendar/events?from={from}&to={}", from + 40 * 86_400),
            &org,
            None,
        )
        .await;
    assert_eq!(s, 200, "{list}");
    let occ: Vec<&Value> = list["occurrences"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| o["event_id"] == id)
        .collect();
    assert_eq!(occ.len(), 6);
    let hours: Vec<i64> = occ
        .iter()
        .map(|o| (o["starts_at"].as_i64().unwrap() % 86_400) / 3600)
        .collect();
    assert_eq!(hours, vec![13, 13, 14, 14, 14, 14]); // 09:00 EDT then 09:00 EST
    let third = occ[2]["occurrence_id"].as_str().unwrap().to_string();
    assert!(third.ends_with("20261105T090000"));

    // "This event": move the third to 11:00; it keeps its id and key.
    let (s, v) = srv.call(Method::PATCH, &format!("/v1/calendar/events/{id}"), &org, Some(json!({
        "expected_revision": 1, "scope": "this", "occurrence_id": third, "start": "2026-11-05T11:00", "end": "2026-11-05T11:30"
    }))).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["occurrence"]["occurrence_id"], third);
    assert_eq!(
        v["occurrence"]["starts_at"].as_i64().unwrap() % 86_400 / 3600,
        16
    );
    // A window containing only the new time finds the moved occurrence.
    let new_start = v["occurrence"]["starts_at"].as_i64().unwrap();
    let (_, list) = srv
        .call(
            Method::GET,
            &format!(
                "/v1/calendar/events?from={}&to={}",
                new_start - 60,
                new_start + 60
            ),
            &org,
            None,
        )
        .await;
    assert_eq!(list["occurrences"].as_array().unwrap().len(), 1);
    // Attendee-only window before it: the original time is empty now.
    let orig = occ[2]["starts_at"].as_i64().unwrap();
    let (_, list) = srv
        .call(
            Method::GET,
            &format!("/v1/calendar/events?from={}&to={}", orig - 60, orig + 60),
            &org,
            None,
        )
        .await;
    assert!(list["occurrences"]
        .as_array()
        .unwrap()
        .iter()
        .all(|o| o["event_id"] != id));

    // "This and following" from the fifth: two series, same meeting reservation semantics, COUNT split.
    let fifth = occ[4]["occurrence_id"].as_str().unwrap().to_string();
    let (s, v) = srv.call(Method::PATCH, &format!("/v1/calendar/events/{id}"), &org, Some(json!({
        "expected_revision": 2, "scope": "following", "occurrence_id": fifth, "title": "Standup (new)"
    }))).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["split_from"]["event_id"], id);
    let new_id = v["event"]["id"].as_str().unwrap();
    assert_eq!(v["event"]["rrule"], "FREQ=WEEKLY;COUNT=2");
    let (_, list) = srv
        .call(
            Method::GET,
            &format!("/v1/calendar/events?from={from}&to={}", from + 50 * 86_400),
            &org,
            None,
        )
        .await;
    let all = list["occurrences"].as_array().unwrap();
    assert_eq!(all.iter().filter(|o| o["event_id"] == id).count(), 4);
    assert_eq!(all.iter().filter(|o| o["event_id"] == new_id).count(), 2);
    // Cancel one occurrence of the new series.
    let first_new = all.iter().find(|o| o["event_id"] == new_id).unwrap()["occurrence_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (s, _) = srv
        .call(
            Method::POST,
            &format!("/v1/calendar/events/{new_id}/cancel"),
            &org,
            Some(json!({
        "expected_revision": 1, "scope": "this", "occurrence_id": first_new })),
        )
        .await;
    assert_eq!(s, 200);
    let (_, list) = srv
        .call(
            Method::GET,
            &format!("/v1/calendar/events?from={from}&to={}", from + 50 * 86_400),
            &org,
            None,
        )
        .await;
    assert_eq!(
        list["occurrences"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|o| o["event_id"] == new_id)
            .count(),
        1
    );
    // Stale revision → conflict.
    let (s, v) = srv
        .call(
            Method::PATCH,
            &format!("/v1/calendar/events/{id}"),
            &org,
            Some(json!({"expected_revision": 1, "title": "x"})),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (409, Some("revision_conflict")));
}

#[tokio::test]
async fn meeting_access_resolves_roles_and_refuses_strangers() {
    let srv = serve().await;
    let org = person(&srv, "orgd").await;
    let alice = person(&srv, "aliced").await;
    let bob = person(&srv, "bobd").await;
    let dan = person(&srv, "dand").await;
    let stranger = person(&srv, "strangerd").await;
    let ev = create(&srv, &org, meeting_body(json!([
        {"handle": alice.handle}, {"handle": bob.handle, "role": "cohost"}, {"handle": dan.handle},
        {"contact": "guest@example.com", "name": "Guest"}
    ]))).await;
    let code = ev["meeting"]["code"].as_str().unwrap().to_string();
    let id = ev["id"].as_str().unwrap().to_string();
    let mb = |p: &P| json!({ "kind": "member", "mailboxes": [{ "address": p.address }] });
    let a = srv.access(json!({ "code": code, "actor": mb(&org) })).await;
    assert_eq!(
        (
            a["allowed"].as_bool(),
            a["role"].as_str(),
            a["admission"].as_str()
        ),
        (Some(true), Some("host"), Some("direct"))
    );
    assert_eq!(a["can_admit"], true);
    let a = srv.access(json!({ "code": code, "actor": mb(&bob) })).await;
    assert_eq!(
        (a["role"].as_str(), a["can_admit"].as_bool()),
        (Some("cohost"), Some(true))
    );
    let a = srv
        .access(json!({ "code": code, "actor": mb(&alice) }))
        .await;
    assert_eq!(
        (
            a["role"].as_str(),
            a["admission"].as_str(),
            a["can_admit"].as_bool()
        ),
        (Some("attendee"), Some("direct"), Some(false))
    );
    assert!(a["admitters"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x == &json!(bob.address)));
    let a = srv
        .access(json!({ "code": code, "actor": mb(&stranger) }))
        .await;
    assert_eq!(
        (a["allowed"].as_bool(), a["reason"].as_str()),
        (Some(false), Some("not_invited"))
    );
    // Dan declines: no direct entry.
    srv.call(
        Method::POST,
        &format!("/v1/calendar/events/{id}/rsvp"),
        &dan,
        Some(json!({"status": "declined"})),
    )
    .await;
    let a = srv.access(json!({ "code": code, "actor": mb(&dan) })).await;
    assert_eq!(a["reason"], "declined");
    // Anonymous link holders: nothing while knocking is off.
    let a = srv
        .access(json!({ "code": code, "actor": {"kind": "anonymous"} }))
        .await;
    assert_eq!(a["reason"], "unavailable");
    let a = srv
        .access(json!({ "code": "ZZZZZZZZZZ", "actor": mb(&org) }))
        .await;
    assert_eq!(a["reason"], "unavailable");
    // Extend: host only, bounded.
    let a = srv
        .access(
            json!({ "code": code, "actor": mb(&alice), "purpose": "extend", "extend_minutes": 30 }),
        )
        .await;
    assert_eq!(a["allowed"], false);
    let before = srv.access(json!({ "code": code, "actor": mb(&org) })).await["closes_at"]
        .as_i64()
        .unwrap();
    let a = srv
        .access(
            json!({ "code": code, "actor": mb(&org), "purpose": "extend", "extend_minutes": 30 }),
        )
        .await;
    assert_eq!(a["closes_at"].as_i64().unwrap(), before + 1800);
    // Removing Alice revokes her.
    let alice_att = ev["attendees"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["handle"] == alice.handle)
        .unwrap()["attendee_id"]
        .clone();
    let (s, _) = srv
        .call(
            Method::POST,
            &format!("/v1/calendar/events/{id}/attendees"),
            &org,
            Some(json!({"expected_revision": 1, "remove": [alice_att]})),
        )
        .await;
    assert_eq!(s, 200);
    let a = srv
        .access(json!({ "code": code, "actor": mb(&alice) }))
        .await;
    assert_eq!(a["reason"], "not_invited");
    // Out of window: a meeting tomorrow is window_closed for invitees, unavailable for others.
    let mut later = meeting_body(json!([{"handle": bob.handle}]));
    later["start"] = json!(soon(24 * 60));
    later["end"] = json!(soon(24 * 60 + 30));
    let ev2 = create(&srv, &org, later).await;
    let code2 = ev2["meeting"]["code"].as_str().unwrap();
    let a = srv
        .access(json!({ "code": code2, "actor": mb(&bob) }))
        .await;
    assert_eq!(a["reason"], "window_closed");
    assert_eq!(a["occurrences"].as_array().unwrap().len(), 1);
    let a = srv
        .access(json!({ "code": code2, "actor": mb(&stranger) }))
        .await;
    assert_eq!(a["reason"], "unavailable");
    // Codes are distinct per meeting and ten characters (never an ad-hoc six).
    assert_ne!(code, code2);
}

fn signer() -> (p256::ecdsa::SigningKey, String) {
    let sk = p256::ecdsa::SigningKey::random(&mut rand_core::OsRng);
    let pk = sk.verifying_key().to_encoded_point(false);
    (sk, super::b64url(pk.as_bytes()))
}

fn prove(sk: &p256::ecdsa::SigningKey, purpose: &str, binding: &str, ts: i64) -> String {
    use p256::ecdsa::signature::Signer;
    let sig: p256::ecdsa::Signature =
        sk.sign(format!("pigeonpost-guest-proof-v1\n{purpose}\n{binding}\n{ts}").as_bytes());
    super::b64url(&sig.to_bytes())
}

#[tokio::test]
async fn guests_preview_answer_verify_and_join_with_scoped_credentials() {
    let srv = serve().await;
    let org = person(&srv, "orge").await;
    srv.flag("calendar_email", &org.account).await;
    let mut body = meeting_body(json!([{"contact": "guest@example.com", "name": "Guest"}]));
    body["meeting"]["waiting"] = json!("none");
    let ev = create(&srv, &org, body).await;
    let id = ev["id"].as_str().unwrap().to_string();
    let code = ev["meeting"]["code"].as_str().unwrap().to_string();
    let att = ev["attendees"][0]["attendee_id"]
        .as_str()
        .unwrap()
        .to_string();

    // The worker sends the invitation, with a calendar file and links carrying a fresh token.
    super::worker::run_once(&srv.state, now_unix())
        .await
        .unwrap();
    let sent = srv.state.calendar.notifier.sent();
    assert_eq!(sent.len(), 1);
    let m = &sent[0];
    assert_eq!(m.to, "guest@example.com");
    assert_eq!(m.workflow, "pigeonpost-meeting-invite");
    let (_, ctype, ics) = &m.attachments[0];
    assert_eq!(ctype, "text/calendar; charset=utf-8; method=REQUEST");
    let ics = String::from_utf8(ics.clone()).unwrap();
    assert!(ics.contains("METHOD:REQUEST") && ics.contains(&format!("UID:{id}@pigeonpost.dev")));
    assert!(ics.contains("BEGIN:VTIMEZONE") && ics.contains("RSVP=FALSE"));
    assert_eq!(ics.matches("ATTENDEE").count(), 1);
    assert!(!ics.contains("#t="), "no credentials in calendar files");
    let token = m
        .text
        .split("#t=")
        .nth(1)
        .unwrap()
        .split(|c: char| c == '&' || c.is_whitespace())
        .next()
        .unwrap()
        .to_string();
    // Organizer sees it delivered.
    let (_, d) = srv
        .call(
            Method::GET,
            &format!("/v1/calendar/events/{id}"),
            &org,
            None,
        )
        .await;
    assert_eq!(d["event"]["attendees"][0]["delivery"], "delivered");

    // Preview reveals only the limited projection.
    let (s, p) = srv
        .anon("/v1/calendar/guest/preview", json!({ "token": token }))
        .await;
    assert_eq!(s, 200, "{p}");
    assert_eq!(p["event"]["title"], "Design review");
    assert!(p["guests"].is_null());
    assert_eq!(p["verification_available"], true);
    let occ = p["occurrences"][0]["occurrence_id"]
        .as_str()
        .unwrap()
        .to_string();
    // A forged token learns nothing.
    let (s, v) = srv
        .anon(
            "/v1/calendar/guest/preview",
            json!({ "token": super::new_secret() }),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (403, Some("guest_token_invalid")));
    // Answering through the link records the proof level.
    let (s, r) = srv
        .anon(
            "/v1/calendar/guest/rsvp",
            json!({ "token": token, "status": "accepted" }),
        )
        .await;
    assert_eq!((s, r["proof"].as_str()), (200, Some("link")));
    // The link alone only knocks.
    let (s, j) = srv
        .anon(
            "/v1/calendar/guest/join",
            json!({ "token": token, "occurrence_id": occ }),
        )
        .await;
    assert_eq!((s, j["admission"].as_str()), (200, Some("knock")));
    let a = srv
        .access(json!({ "code": code, "actor": {"kind": "guest", "ticket": j["ticket"]} }))
        .await;
    assert_eq!(
        (a["role"].as_str(), a["admission"].as_str()),
        (Some("guest"), Some("knock"))
    );
    // Tickets are single use.
    let a = srv
        .access(json!({ "code": code, "actor": {"kind": "guest", "ticket": j["ticket"]} }))
        .await;
    assert_eq!(a["reason"], "ticket_invalid");

    // Verification: a code goes to the stored address only.
    let (s, c) = srv
        .anon("/v1/calendar/guest/verify/start", json!({ "token": token }))
        .await;
    assert_eq!(s, 200, "{c}");
    let cid = c["challenge_id"].as_str().unwrap().to_string();
    super::worker::run_once(&srv.state, now_unix())
        .await
        .unwrap();
    let sent = srv.state.calendar.notifier.sent();
    let verify = sent
        .iter()
        .find(|m| m.workflow == "pigeonpost-meeting-verify")
        .unwrap();
    assert_eq!(verify.to, "guest@example.com");
    let code6: String = verify
        .text
        .lines()
        .find(|l| l.len() == 6 && l.chars().all(|c| c.is_ascii_digit()))
        .unwrap()
        .to_string();
    let (sk, pk) = signer();
    let ts = now_unix() as i64;
    // Wrong code, then right code; a repeat returns the same session.
    let (s, _) = srv
        .anon(
            "/v1/calendar/guest/verify/complete",
            json!({ "challenge_id": cid, "code": "000000x",
        "device_public_key": pk, "device_proof": prove(&sk, "verify", &cid, ts), "ts": ts }),
        )
        .await;
    assert_eq!(s, 403);
    let done = json!({ "challenge_id": cid, "code": code6, "device_public_key": pk,
                       "device_proof": prove(&sk, "verify", &cid, ts), "ts": ts });
    let (s, sess) = srv
        .anon("/v1/calendar/guest/verify/complete", done.clone())
        .await;
    assert_eq!(s, 200, "{sess}");
    let (_, again) = srv.anon("/v1/calendar/guest/verify/complete", done).await;
    assert_eq!(again["guest_session"], sess["guest_session"]);
    let gs = sess["guest_session"].as_str().unwrap().to_string();
    let sid = sess["session_id"].as_str().unwrap().to_string();
    // A proof from another key fails; the bound key gets direct admission (waiting = none).
    let (other, _) = signer();
    let (s, _) = srv
        .anon(
            "/v1/calendar/guest/join",
            json!({ "guest_session": gs, "occurrence_id": occ, "ts": ts,
        "device_proof": prove(&other, "join", &format!("{sid}\n{occ}"), ts) }),
        )
        .await;
    assert_eq!(s, 403);
    let (s, j) = srv
        .anon(
            "/v1/calendar/guest/join",
            json!({ "guest_session": gs, "occurrence_id": occ, "ts": ts,
        "device_proof": prove(&sk, "join", &format!("{sid}\n{occ}"), ts) }),
        )
        .await;
    assert_eq!((s, j["admission"].as_str()), (200, Some("direct")));
    let a = srv
        .access(json!({ "code": code, "actor": {"kind": "guest", "ticket": j["ticket"]} }))
        .await;
    assert_eq!(
        (
            a["admission"].as_str(),
            a["participant"]["verified"].as_bool()
        ),
        (Some("direct"), Some(true))
    );
    // A guest credential is no use on member routes.
    let (s, _) = srv
        .raw(
            Method::GET,
            &format!("/v1/calendar/events/{id}"),
            Some(&gs),
            None,
            None,
        )
        .await;
    assert_eq!(s, 401);

    // Removal revokes the link and the session; recheck denies.
    let (s, _) = srv
        .call(
            Method::POST,
            &format!("/v1/calendar/events/{id}/attendees"),
            &org,
            Some(json!({"expected_revision": 1, "remove": [att]})),
        )
        .await;
    assert_eq!(s, 200);
    let (s, _) = srv
        .anon("/v1/calendar/guest/preview", json!({ "token": token }))
        .await;
    assert_eq!(s, 403);
    let (s, _) = srv
        .anon("/v1/calendar/guest/preview", json!({ "guest_session": gs }))
        .await;
    assert_eq!(s, 403);
    let a = srv.access(json!({ "code": code, "purpose": "recheck", "actor": {"kind": "guest", "attendee_id": att} })).await;
    assert_eq!(a["reason"], "not_invited");
    // The removed guest is told with a cancellation.
    super::worker::run_once(&srv.state, now_unix())
        .await
        .unwrap();
    let cancel = srv
        .state
        .calendar
        .notifier
        .sent()
        .into_iter()
        .find(|m| m.workflow == "pigeonpost-meeting-cancel")
        .unwrap();
    let ics = String::from_utf8(cancel.attachments[0].2.clone()).unwrap();
    assert!(ics.contains("METHOD:CANCEL") && ics.contains("STATUS:CANCELLED"));
}

#[tokio::test]
async fn external_messages_wait_for_the_flag_and_respect_unsubscribe() {
    let srv = serve().await;
    let org = person(&srv, "orgf").await;
    let mut body = meeting_body(json!([{"contact": "held@example.com"}]));
    body["meeting"] = json!({ "enabled": false });
    body["start"] = json!(soon(3 * 24 * 60));
    body["end"] = json!(soon(3 * 24 * 60 + 30));
    let ev = create(&srv, &org, body).await;
    let id = ev["id"].as_str().unwrap().to_string();
    // Flag off: nothing leaves, the row stays queued; the organizer can still copy a link.
    super::worker::run_once(&srv.state, now_unix())
        .await
        .unwrap();
    assert!(srv.state.calendar.notifier.sent().is_empty());
    let (_, d) = srv
        .call(
            Method::GET,
            &format!("/v1/calendar/events/{id}"),
            &org,
            None,
        )
        .await;
    assert_eq!(d["event"]["attendees"][0]["delivery"], "queued");
    let att = d["event"]["attendees"][0]["attendee_id"].as_str().unwrap();
    let (s, l) = srv
        .call(
            Method::POST,
            &format!("/v1/calendar/events/{id}/attendees/{att}/link"),
            &org,
            None,
        )
        .await;
    assert_eq!(s, 200);
    let token = l["url"]
        .as_str()
        .unwrap()
        .split("#t=")
        .nth(1)
        .unwrap()
        .to_string();
    let (s, p) = srv
        .anon("/v1/calendar/guest/preview", json!({ "token": token }))
        .await;
    assert_eq!(
        (s, p["verification_available"].as_bool()),
        (200, Some(false))
    );
    let (s, v) = srv
        .anon("/v1/calendar/guest/verify/start", json!({ "token": token }))
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (503, Some("external_unavailable"))
    );

    // An API key cannot make a change that would message the contact guest.
    let (s, t) = srv
        .raw(
            Method::PATCH,
            &format!("/v1/calendar/events/{id}"),
            Some(&org.api_key),
            Some(&org.address),
            Some(json!({"expected_revision": 1, "title": "Agent edit"})),
        )
        .await;
    assert_eq!(s, 403, "{t}");
    // Flag on: the held invitation goes out once the hold expires.
    srv.flag("calendar_email", &org.account).await;
    super::worker::run_once(&srv.state, now_unix() + 3700)
        .await
        .unwrap();
    let sent = srv.state.calendar.notifier.sent();
    assert_eq!(sent.len(), 1);
    let unsub = sent[0]
        .text
        .split("#u=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string();
    let (s, _) = srv
        .anon(
            "/v1/calendar/guest/unsubscribe",
            json!({ "token": unsub, "scope": "organizer" }),
        )
        .await;
    assert_eq!(s, 200);
    // The next update to that address is suppressed.
    let (s, _) = srv
        .call(
            Method::PATCH,
            &format!("/v1/calendar/events/{id}"),
            &org,
            Some(json!({"expected_revision": 1, "title": "Moved"})),
        )
        .await;
    assert_eq!(s, 200);
    super::worker::run_once(&srv.state, now_unix() + 3700)
        .await
        .unwrap();
    assert_eq!(srv.state.calendar.notifier.sent().len(), 1);
    let (_, d) = srv
        .call(
            Method::GET,
            &format!("/v1/calendar/events/{id}"),
            &org,
            None,
        )
        .await;
    assert_eq!(d["event"]["attendees"][0]["delivery"], "suppressed");
}

#[tokio::test]
async fn reminders_fire_once_for_eligible_people_only() {
    let srv = serve().await;
    let org = person(&srv, "orgg").await;
    let alice = person(&srv, "aliceg").await;
    let bob = person(&srv, "bobg").await;
    let mut body = meeting_body(json!([{"handle": alice.handle}, {"handle": bob.handle}]));
    body["start"] = json!(soon(10));
    body["end"] = json!(soon(40));
    let ev = create(&srv, &org, body).await;
    let id = ev["id"].as_str().unwrap();
    srv.call(
        Method::POST,
        &format!("/v1/calendar/events/{id}/rsvp"),
        &alice,
        Some(json!({"status": "accepted"})),
    )
    .await;
    srv.call(
        Method::POST,
        &format!("/v1/calendar/events/{id}/rsvp"),
        &bob,
        Some(json!({"status": "declined"})),
    )
    .await;
    let now = now_unix();
    super::worker::run_once(&srv.state, now).await.unwrap();
    super::worker::run_once(&srv.state, now + 5).await.unwrap();
    let count =
        |evs: &Vec<(String, Value)>| evs.iter().filter(|(t, _)| t == "event.reminder").count();
    assert_eq!(count(&srv.events_of(&alice.account).await), 1);
    assert_eq!(count(&srv.events_of(&org.account).await), 1);
    assert_eq!(count(&srv.events_of(&bob.account).await), 0);
}

#[tokio::test]
async fn feeds_redact_private_events_and_reset_revokes() {
    let srv = serve().await;
    let org = person(&srv, "orgh").await;
    let mut a = json!({ "request_key": "p1", "title": "Secret thing", "start": soon(60), "end": soon(90), "tzid": "UTC", "privacy": "private" });
    create(&srv, &org, a.clone()).await;
    a["request_key"] = json!("p2");
    a["title"] = json!("Lunch");
    a["privacy"] = json!("default");
    a["location"] = json!("Cafe");
    let ev = create(&srv, &org, a).await;
    let cal = ev["calendar_id"].as_str().unwrap();
    let (s, f) = srv
        .call(
            Method::POST,
            &format!("/v1/calendar/feeds/{cal}"),
            &org,
            Some(json!({"enabled": true})),
        )
        .await;
    assert_eq!(s, 200, "{f}");
    let url = f["url"]
        .as_str()
        .unwrap()
        .replace("http://postbox.test", "");
    let (s, text) = srv.raw(Method::GET, &url, None, None, None).await;
    assert_eq!(s, 200);
    assert!(text.contains("SUMMARY:Lunch") && text.contains("SUMMARY:Busy"));
    assert!(!text.contains("Secret thing") && !text.contains("/m/"));
    let (s, f2) = srv
        .call(
            Method::POST,
            &format!("/v1/calendar/feeds/{cal}"),
            &org,
            Some(json!({"enabled": true})),
        )
        .await;
    assert_eq!(s, 200);
    let (s, _) = srv.raw(Method::GET, &url, None, None, None).await;
    assert_eq!(s, 404);
    let url2 = f2["url"]
        .as_str()
        .unwrap()
        .replace("http://postbox.test", "");
    assert_eq!(srv.raw(Method::GET, &url2, None, None, None).await.0, 200);
    // An API key cannot open a feed.
    let (s, _) = srv
        .raw(
            Method::POST,
            &format!("/v1/calendar/feeds/{cal}"),
            Some(&org.api_key),
            Some(&org.address),
            Some(json!({"enabled": true})),
        )
        .await;
    assert_eq!(s, 403);
}

#[tokio::test]
async fn import_previews_without_side_effects_and_confirms_private_copies() {
    let srv = serve().await;
    let org = person(&srv, "orgi").await;
    let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:ext-1@example.com\r\nSUMMARY:Imported\r\n\
DTSTART;TZID=Europe/Istanbul:20261201T100000\r\nDTEND;TZID=Europe/Istanbul:20261201T110000\r\n\
ORGANIZER:mailto:boss@example.com\r\nATTENDEE:mailto:x@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let resp = srv
        .http
        .post(format!("{}/v1/calendar/import/preview", srv.public))
        .bearer_auth(&org.token)
        .header("x-pigeonpost-identity", &org.address)
        .header("content-type", "text/calendar")
        .body(ics)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    let draft = v["draft_id"].as_str().unwrap();
    assert_eq!(v["events"].as_array().unwrap().len(), 1);
    let (s, c) = srv
        .call(
            Method::POST,
            &format!("/v1/calendar/import/{draft}/confirm"),
            &org,
            Some(json!({"select": [0]})),
        )
        .await;
    assert_eq!(s, 200, "{c}");
    let e = &c["created"][0];
    assert_eq!(e["title"], "Imported");
    assert!(e["attendees"].as_array().unwrap().is_empty());
    assert_ne!(e["uid"], "ext-1@example.com");
    assert!(srv.state.calendar.notifier.sent().is_empty());
}

#[tokio::test]
async fn ics_export_round_trips_through_the_parser() {
    let srv = serve().await;
    let org = person(&srv, "orgj").await;
    let ev = create(&srv, &org, json!({ "request_key": "x1", "title": "Weekly; sync, notes", "description": "line1\nline2",
        "start": "2026-10-22T09:00", "end": "2026-10-22T09:30", "tzid": "Europe/Berlin", "rrule": "FREQ=WEEKLY;COUNT=4" })).await;
    let id = ev["id"].as_str().unwrap();
    let (s, text) = srv
        .raw(
            Method::GET,
            &format!("/v1/calendar/events/{id}.ics"),
            Some(&org.token),
            Some(&org.address),
            None,
        )
        .await;
    assert_eq!(s, 200);
    assert!(
        text.contains("BEGIN:VTIMEZONE")
            && text.contains("TZID:Europe/Berlin")
            && text.contains("TZOFFSETTO:+0100")
    );
    let (events, _) = super::ics::parse_import(text.as_bytes(), chrono_tz::Tz::UTC).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].title, "Weekly; sync, notes");
    assert_eq!(events[0].description, "line1\nline2");
    assert_eq!(events[0].start.as_deref(), Some("2026-10-22T09:00"));
    assert_eq!(events[0].rrule.as_deref(), Some("FREQ=WEEKLY;COUNT=4"));
}

const IMPORT_RETRY_ICS: &str = concat!(
    "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n",
    "BEGIN:VEVENT\r\nUID:retry-first@example.test\r\nSUMMARY:IMPORT_PRIVATE_TITLE_DO_NOT_STORE_CLEAR\r\n",
    "DTSTART;TZID=Europe/Istanbul:20261201T100000\r\nDTEND;TZID=Europe/Istanbul:20261201T110000\r\nEND:VEVENT\r\n",
    "BEGIN:VEVENT\r\nUID:retry-second@example.test\r\nSUMMARY:Second private copy\r\n",
    "DTSTART;TZID=Europe/Istanbul:20261202T100000\r\nDTEND;TZID=Europe/Istanbul:20261202T110000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
);

async fn preview_import(srv: &Srv, who: &P, ics: &str) -> Value {
    let response = srv
        .http
        .post(format!("{}/v1/calendar/import/preview", srv.public))
        .bearer_auth(&who.token)
        .header("x-pigeonpost-identity", &who.address)
        .header("content-type", "text/calendar")
        .body(ics.to_owned())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}

#[tokio::test]
async fn import_confirmation_replays_committed_result() {
    let srv = serve().await;
    let who = person(&srv, "importretry").await;
    let preview = preview_import(&srv, &who, IMPORT_RETRY_ICS).await;
    let draft = preview["draft_id"].as_str().unwrap();
    let path = format!("/v1/calendar/import/{draft}/confirm");
    let body = json!({"select":[0]});
    let (status, first) = srv
        .call(Method::POST, &path, &who, Some(body.clone()))
        .await;
    assert_eq!(status, 200, "{first}");
    let notices = srv.events_of(&who.account).await.len();
    let (status, retry) = srv
        .call(Method::POST, &path, &who, Some(body.clone()))
        .await;
    assert_eq!(status, 200, "a committed import must be retryable: {retry}");
    assert_eq!(retry, first);
    let explicit = json!({"select":[0], "calendar_id":first["created"][0]["calendar_id"]});
    let (status, same_target) = srv.call(Method::POST, &path, &who, Some(explicit)).await;
    assert_eq!(
        status, 200,
        "explicit default calendar is the same destination: {same_target}"
    );
    assert_eq!(same_target, first);
    assert_eq!(srv.events_of(&who.account).await.len(), notices);
    let mailbox = who.address.clone();
    let key = draft.to_owned();
    let (kind, bytes, event_count, reminder_count) = srv.state.store.group_read(
        move |c: &rusqlite::Connection| -> crate::groups::GResult<(String, Vec<u8>, i64, i64)> {
            let (kind, bytes) = c.query_row(
                "SELECT typeof(response), response FROM calendar_idempotency WHERE scope = ?1 AND action = 'import_confirm' AND request_key = ?2",
                rusqlite::params![mailbox, key], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)),
            )?;
            let events = c.query_row("SELECT COUNT(*) FROM events WHERE organizer = ?1", [&mailbox], |r| r.get(0))?;
            let reminders = c.query_row("SELECT COUNT(*) FROM event_reminders WHERE owner_identity = ?1", [&mailbox], |r| r.get(0))?;
            Ok((kind, bytes, events, reminders))
        },
    ).await.unwrap();
    assert_eq!(kind, "blob");
    let private_title = b"IMPORT_PRIVATE_TITLE_DO_NOT_STORE_CLEAR";
    assert!(!bytes
        .windows(private_title.len())
        .any(|part| part == private_title));
    assert_eq!((event_count, reminder_count), (1, 1));
    assert!(srv.state.calendar.notifier.sent().is_empty());
    let other = person(&srv, "importother").await;
    let (status, denied) = srv.call(Method::POST, &path, &other, Some(body)).await;
    assert_eq!(status, 404);
    assert!(denied.get("created").is_none());
}

#[tokio::test]
async fn import_confirmation_treats_selected_indices_as_a_set() {
    let srv = serve().await;
    let who = person(&srv, "importset").await;
    let preview = preview_import(&srv, &who, IMPORT_RETRY_ICS).await;
    let draft = preview["draft_id"].as_str().unwrap();
    let path = format!("/v1/calendar/import/{draft}/confirm");
    let (status, first) = srv
        .call(
            Method::POST,
            &path,
            &who,
            Some(json!({"select":[1, 0, 1, 0]})),
        )
        .await;
    assert_eq!(status, 200, "{first}");
    assert_eq!(first["created"].as_array().unwrap().len(), 2);
    let (status, retry) = srv
        .call(Method::POST, &path, &who, Some(json!({"select":[0, 1]})))
        .await;
    assert_eq!(status, 200, "{retry}");
    assert_eq!(retry, first);
    let (status, conflict) = srv
        .call(Method::POST, &path, &who, Some(json!({"select":[0]})))
        .await;
    assert_eq!(status, 409);
    assert_eq!(conflict["error"], "idempotency_conflict");
    let (status, _) = srv
        .call(
            Method::POST,
            &path,
            &who,
            Some(json!({"select":vec![0;201]})),
        )
        .await;
    assert_eq!(status, 400);
}

#[tokio::test]
async fn import_confirmation_replay_expires_without_recreating_events() {
    let srv = serve().await;
    let who = person(&srv, "importexpires").await;
    let preview = preview_import(&srv, &who, IMPORT_RETRY_ICS).await;
    let draft = preview["draft_id"].as_str().unwrap().to_owned();
    let path = format!("/v1/calendar/import/{draft}/confirm");
    let (status, _) = srv
        .call(Method::POST, &path, &who, Some(json!({"select":[0]})))
        .await;
    assert_eq!(status, 200);
    let mailbox = who.address.clone();
    srv.state.store.group_tx(move |tx: &rusqlite::Transaction<'_>| -> crate::groups::GResult<()> {
        tx.execute("UPDATE calendar_idempotency SET created_at = ?3 WHERE scope = ?1 AND action = 'import_confirm' AND request_key = ?2",
            rusqlite::params![mailbox, draft, now_unix().saturating_sub(super::IDEMPOTENCY_SECS + 1) as i64])?;
        Ok(())
    }).await.unwrap();
    let (status, _) = srv
        .call(Method::POST, &path, &who, Some(json!({"select":[0]})))
        .await;
    assert_eq!(status, 404);
    let mailbox = who.address.clone();
    let count = srv
        .state
        .store
        .group_read(
            move |c: &rusqlite::Connection| -> crate::groups::GResult<i64> {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM events WHERE organizer = ?1",
                    [mailbox],
                    |r| r.get(0),
                )?)
            },
        )
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn import_confirmation_replay_checks_current_calendar_access() {
    let srv = serve().await;
    let who = person(&srv, "importcalendar").await;
    let preview = preview_import(&srv, &who, IMPORT_RETRY_ICS).await;
    let draft = preview["draft_id"].as_str().unwrap();
    let (status, folder) = srv
        .call(
            Method::POST,
            "/v1/calendars",
            &who,
            Some(json!({"request_key":"importtarget", "name":"Import target"})),
        )
        .await;
    assert_eq!(status, 201);
    let calendar = folder["calendar"]["id"].as_str().unwrap();
    let path = format!("/v1/calendar/import/{draft}/confirm");
    let body = json!({"select":[0], "calendar_id":calendar});
    let (status, _) = srv
        .call(Method::POST, &path, &who, Some(body.clone()))
        .await;
    assert_eq!(status, 200);
    let (status, _) = srv
        .call(
            Method::POST,
            &path,
            &who,
            Some(json!({"select":[0], "calendar_id":preview["calendar_id"]})),
        )
        .await;
    assert_eq!(status, 409);
    let (status, deleted) = srv
        .call(
            Method::DELETE,
            &format!("/v1/calendars/{calendar}"),
            &who,
            Some(json!({"expected_revision":folder["calendar"]["revision"]})),
        )
        .await;
    assert_eq!(status, 200, "{deleted}");
    let (status, unavailable) = srv.call(Method::POST, &path, &who, Some(body)).await;
    assert_eq!(status, 404);
    assert_eq!(unavailable["error"], "calendar_unavailable");
}

#[tokio::test]
async fn calendar_routes_are_off_without_the_flag() {
    let srv = serve().await;
    let org = person(&srv, "orgk").await;
    let (s, _) = srv
        .raw(
            Method::PUT,
            "/internal/v1/features/calendar",
            Some(OPS),
            None,
            Some(json!({ "scope": "account", "account_id": org.account, "enabled": false })),
        )
        .await;
    assert_eq!(s, 200);
    let (s, v) = srv.call(Method::GET, "/v1/calendars", &org, None).await;
    assert_eq!((s, v["error"].as_str()), (404, Some("feature_disabled")));
}
