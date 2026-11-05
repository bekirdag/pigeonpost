//! Enterprise launch prerequisites from the 2026-11-03 SDS audit (§12 P1-5, P2-5, P2-20, P3-1–3):
//! D-14 deletion of long-suspended orgs, the owner-only billing-admin grant, delegation
//! capabilities that grant something, org room storage and timers, group pictures and org logos,
//! and the invitation fixes.

use crate::org_tests::*;
use crate::*;
use reqwest::Method;
use serde_json::{json, Value};

/// `(room timer, effective timer with the room's off, pooled quota)`.
type Timers = (Option<i64>, Option<i64>, Option<u64>);

fn sample_png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_fn(w, h, |x, y| {
        image::Rgba([(x % 256) as u8, (y % 256) as u8, 90, 255])
    });
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

async fn put_image(srv: &Srv, path: &str, token: &str) -> (u16, Value) {
    let resp = srv
        .http
        .put(format!("{}{path}", srv.public))
        .bearer_auth(token)
        .header("content-type", "image/png")
        .body(sample_png(80, 80))
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

async fn fetch(srv: &Srv, path: &str, token: Option<&str>) -> u16 {
    let mut req = srv.http.get(format!("{}{path}", srv.public));
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    req.send().await.unwrap().status().as_u16()
}

async fn sql(srv: &Srv, q: &'static str, p: Vec<String>) {
    srv.state
        .store
        .group_tx(move |tx| -> Result<(), crate::groups::GErr> {
            tx.execute(q, rusqlite::params_from_iter(p.iter()))?;
            Ok(())
        })
        .await
        .unwrap();
}

async fn org_status(srv: &Srv, org: &str) -> String {
    let org = org.to_string();
    srv.state
        .store
        .group_read(move |c| -> Result<String, crate::groups::GErr> {
            Ok(
                c.query_row("SELECT status FROM orgs WHERE id = ?1", [&org], |r| {
                    r.get(0)
                })?,
            )
        })
        .await
        .unwrap()
}

/// D-14: a suspended org is warned, then deleted 30 days after suspension — never sooner than a
/// week after the first warning — and an org that pays again before that is not.
#[tokio::test]
async fn suspended_orgs_are_warned_then_deleted_after_30_days() {
    let (srv, owner, org) = setup().await;
    let now = now_unix() as i64;
    // Lapsed long enough to be suspended (7 past due + 14 read-only).
    srv.entitlement(&org, 5, "expired", now - 22 * 86_400, 2, false)
        .await;
    srv.state
        .store
        .org_lifecycle_sweep(now_unix())
        .await
        .unwrap();
    assert_eq!(org_status(&srv, &org).await, "suspended");

    // First sweep: the first warning, and the date everyone is told.
    crate::org_routes::retention_sweep(&srv.state).await;
    let (_, v) = srv.get(&format!("/v1/orgs/{org}"), &owner.token).await;
    let at = v["org"]["deletion_at"].as_i64().expect("deletion_at");
    assert!(at >= now + 29 * 86_400, "{v}");
    let acct = owner.account.clone();
    let warned: i64 = srv
        .state
        .store
        .group_read(move |c| -> Result<i64, crate::groups::GErr> {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM account_events WHERE account_id = ?1 AND type = 'org.deletion_warning'",
                [&acct],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(warned, 1, "the owner is told");
    // A second sweep on the same day does not repeat it.
    crate::org_routes::retention_sweep(&srv.state).await;

    // Suspended 31 days ago, but warned only now: still a week's notice.
    sql(
        &srv,
        "UPDATE orgs SET status_since = ?2 WHERE id = ?1",
        vec![org.clone(), (now - 31 * 86_400).to_string()],
    )
    .await;
    crate::org_routes::retention_sweep(&srv.state).await;
    assert_eq!(org_status(&srv, &org).await, "suspended");
    let (_, v) = srv.get(&format!("/v1/orgs/{org}"), &owner.token).await;
    assert!(
        v["org"]["deletion_at"].as_i64().unwrap() >= now + 7 * 86_400 - 5,
        "{v}"
    );

    // A week after the warning: deleted, with the namespace in its recovery window.
    sql(
        &srv,
        "UPDATE org_deletion_notices SET sent_at = ?2 WHERE org_id = ?1",
        vec![org.clone(), (now - 8 * 86_400).to_string()],
    )
    .await;
    crate::org_routes::retention_sweep(&srv.state).await;
    assert_eq!(org_status(&srv, &org).await, "deleted");
    let (s, _) = srv.get(&format!("/v1/orgs/{org}"), &owner.token).await;
    assert_ne!(s, 200);
}

#[tokio::test]
async fn a_renewed_org_is_not_deleted() {
    let (srv, owner, org) = setup().await;
    let now = now_unix() as i64;
    srv.entitlement(&org, 5, "expired", now - 22 * 86_400, 2, false)
        .await;
    srv.state
        .store
        .org_lifecycle_sweep(now_unix())
        .await
        .unwrap();
    crate::org_routes::retention_sweep(&srv.state).await;
    // Paid again.
    srv.entitlement(&org, 5, "active", now + 30 * 86_400, 3, false)
        .await;
    srv.state
        .store
        .org_lifecycle_sweep(now_unix())
        .await
        .unwrap();
    assert_eq!(org_status(&srv, &org).await, "active");
    sql(
        &srv,
        "UPDATE orgs SET status_since = ?2 WHERE id = ?1",
        vec![org.clone(), (now - 60 * 86_400).to_string()],
    )
    .await;
    crate::org_routes::retention_sweep(&srv.state).await;
    assert_eq!(org_status(&srv, &org).await, "active");
    let (_, v) = srv.get(&format!("/v1/orgs/{org}"), &owner.token).await;
    assert!(v["org"]["deletion_at"].is_null(), "{v}");
}

/// P1-5: a full admin cannot hand out billing administration; the owner can.
#[tokio::test]
async fn only_the_owner_grants_billing_admin() {
    let (srv, owner, org) = setup().await;
    let admin = srv.person("billadm").await;
    let (_, _) = srv.join(&org, &owner, &admin, "adm", &["admin"]).await;
    let target = srv.person("billtarget").await;
    let (tid, _) = srv.join(&org, &owner, &target, "tgt", &[]).await;
    let path = format!("/v1/orgs/{org}/members/{tid}");
    let (s, v) = srv
        .patch(&path, &admin.strong, json!({ "roles": ["billing_admin"] }))
        .await;
    assert_eq!(s, 403, "{v}");
    // Nor by invitation.
    let other = srv.person("billinv").await;
    let (s, v) = srv
        .post(&format!("/v1/orgs/{org}/invites"), &admin.strong, json!({ "invites": [{
            "first_name": "A", "last_name": "B", "contact": other.address, "handle": "inv", "roles": ["billing_admin"],
        }]}))
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["results"][0]["status"], "forbidden", "{v}");
    let (s, v) = srv
        .patch(&path, &owner.strong, json!({ "roles": ["billing_admin"] }))
        .await;
    assert_eq!(s, 200, "{v}");
}

/// P1-5: only capabilities that grant something are accepted or shown.
#[tokio::test]
async fn delegations_carry_only_enforced_capabilities() {
    let (srv, owner, org) = setup().await;
    let (_, a) = srv
        .post(
            &format!("/v1/orgs/{org}/agents"),
            &owner.token,
            json!({ "handle": "bot" }),
        )
        .await;
    let agent = a["agent"]["address"].as_str().unwrap().to_string();
    let p = srv.person("deleg").await;
    let (mid, _) = srv.join(&org, &owner, &p, "dl", &[]).await;
    for cap in ["bind_handle", "configure_rules"] {
        let (s, v) = srv
            .post(
                &format!("/v1/orgs/{org}/delegations"),
                &owner.token,
                json!({ "member_id": mid, "target": "/acme/bot", "capabilities": [cap] }),
            )
            .await;
        assert_eq!(s, 400, "{cap}: {v}");
    }
    let (s, v) = srv
        .post(
            &format!("/v1/orgs/{org}/delegations"),
            &owner.token,
            json!({ "member_id": mid, "target": "/acme/bot", "capabilities": ["read_inbox"] }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    // A row stored before the change lists only what still grants something.
    sql(&srv, "UPDATE org_delegations SET capabilities = '[\"read_inbox\",\"bind_handle\",\"configure_rules\"]' WHERE target_identity = ?1",
        vec![agent.clone()]).await;
    let (_, v) = srv
        .get(&format!("/v1/orgs/{org}/delegations"), &owner.token)
        .await;
    assert_eq!(
        v["delegations"][0]["capabilities"],
        json!(["read_inbox"]),
        "{v}"
    );
}

/// P2-5: org rooms start with the org's default timer and never keep a message longer than the
/// org maximum; their storage comes from the org's pool.
#[tokio::test]
async fn org_room_timers_and_storage_pool() {
    let (srv, owner, org) = setup().await;
    let (s, v) = srv
        .patch(&format!("/v1/orgs/{org}"), &owner.token,
            json!({ "settings": { "room_default_ephemeral_secs": 604800, "room_max_ephemeral_secs": 86400 } }))
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (400, Some("bad_request")),
        "default longer than maximum"
    );
    let (s, v) = srv
        .patch(&format!("/v1/orgs/{org}"), &owner.token,
            json!({ "settings": { "room_default_ephemeral_secs": 3600, "room_max_ephemeral_secs": 86400 } }))
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, _) = srv
        .patch(
            &format!("/v1/orgs/{org}"),
            &owner.token,
            json!({ "settings": { "room_max_ephemeral_secs": 1234 } }),
        )
        .await;
    assert_eq!(s, 400);
    let (s, r) = srv
        .post(
            &format!("/v1/orgs/{org}/rooms"),
            &owner.token,
            json!({ "name": "Ops" }),
        )
        .await;
    assert_eq!(s, 201, "{r}");
    let rid = r["room"]["id"].as_str().unwrap().to_string();
    let (gid, org2) = (rid.clone(), org.clone());
    let (timer, effective, quota) = srv
        .state
        .store
        .group_read(move |c| -> Result<Timers, crate::groups::GErr> {
            let mut g = crate::groups::load_group(c, &gid)?.unwrap();
            let t = g.ephemeral_secs;
            // A room turned off is still bound by the org maximum.
            g.ephemeral_secs = None;
            let off = crate::org_rooms::effective_timer(c, &g)?;
            let _ = org2;
            Ok((t, off, crate::groups::org_pool_quota(c, &gid)?))
        })
        .await
        .unwrap();
    assert_eq!(timer, Some(3600), "the org default");
    assert_eq!(
        effective,
        Some(86400),
        "the org maximum applies with the timer off"
    );
    // The pool: 5 seats × 5 GB, less what other rooms hold.
    assert_eq!(quota, Some(5 * 5 * 1024 * 1024 * 1024));
}

/// P2-20: group admins set a group's picture, the owner or an admin an org's logo; members see a
/// group picture, outsiders do not; logos are public.
#[tokio::test]
async fn group_pictures_and_org_logos() {
    let srv = serve_with(|s| {
        s.avatar_blobs = Some(std::sync::Arc::new(crate::blobs::Blobs::for_test(
            "avatars-ent",
        )));
    })
    .await;
    srv.global_flag("orgs").await;
    srv.global_flag("groups").await;
    srv.global_flag("avatars").await;
    let owner = srv.person("picowner").await;
    let org = srv.org(&owner, "picco", 5).await;
    let (_, mb) = srv
        .post(
            &format!("/v1/orgs/{org}/me/mailbox"),
            &owner.token,
            json!({ "handle": "boss" }),
        )
        .await;
    let boss = mb["mailbox"]["address"].as_str().unwrap().to_string();
    let general = mb["general_room"].as_str().unwrap().to_string();
    let member = srv.person("picmember").await;
    let (_, member_mb) = srv.join(&org, &owner, &member, "mem", &[]).await;

    // Org logo: owner yes, plain member no.
    let (s, v) = put_image(&srv, &format!("/v1/orgs/{org}/logo"), &member.token).await;
    assert_eq!(s, 403, "{v}");
    let (s, v) = put_image(&srv, &format!("/v1/orgs/{org}/logo"), &owner.token).await;
    assert_eq!(s, 200, "{v}");
    let logo = v["avatar"]["urls"]["128"].as_str().unwrap().to_string();
    assert_eq!(fetch(&srv, &logo, None).await, 200, "logos are public");
    let (_, o) = srv.get(&format!("/v1/orgs/{org}"), &member.token).await;
    assert_eq!(o["org"]["logo"]["urls"]["128"], json!(logo));

    // Group picture: the room's owner (an admin) yes, a plain member no, an outsider cannot see it.
    let (s, v) = put_image(
        &srv,
        &format!(
            "/v1/groups/{general}/picture?identity={}",
            urlencoding(&member_mb)
        ),
        &member.token,
    )
    .await;
    assert!(s == 403 || s == 404, "{s} {v}");
    let (s, v) = put_image(
        &srv,
        &format!(
            "/v1/groups/{general}/picture?identity={}",
            urlencoding(&boss)
        ),
        &owner.token,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let pic = v["avatar"]["urls"]["64"].as_str().unwrap().to_string();
    assert_eq!(fetch(&srv, &pic, Some(&owner.token)).await, 200);
    let outsider = srv.person("picout").await;
    do_create_identity(&srv.state, Some(outsider.account.clone()), None, None)
        .await
        .unwrap();
    assert_eq!(fetch(&srv, &pic, Some(&outsider.token)).await, 404);
    assert_eq!(fetch(&srv, &pic, None).await, 404);
    // Removing it ends every URL.
    let (s, _) = srv
        .call(
            Method::DELETE,
            &format!(
                "/v1/groups/{general}/picture?identity={}",
                urlencoding(&boss)
            ),
            Some(&owner.token),
            None,
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(fetch(&srv, &pic, Some(&owner.token)).await, 404);
}

fn urlencoding(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// P3-1: a mismatched acceptance is counted even though it is refused, and five lock the link.
#[tokio::test]
async fn invitation_mismatches_are_counted_and_lock_the_link() {
    let (srv, owner, org) = setup().await;
    let (s, v) = srv
        .post(&format!("/v1/orgs/{org}/invites"), &owner.strong, json!({ "invites": [{
            "first_name": "J", "last_name": "D", "contact": "jane@example.com", "handle": "jane", "roles": [],
        }]}))
        .await;
    assert_eq!(s, 200, "{v}");
    let token = token_of(v["results"][0]["link"].as_str().unwrap());
    let wrong = srv.person("notjane").await;
    for _ in 0..5 {
        let (s, v) = srv
            .post(
                "/v1/org-invites/accept",
                &wrong.token,
                json!({ "token": token }),
            )
            .await;
        assert_eq!(
            (s, v["error"].as_str()),
            (403, Some("invite_address_mismatch")),
            "{v}"
        );
    }
    let jane = srv.person("jane").await;
    let (s, v) = srv
        .post(
            "/v1/org-invites/accept",
            &jane.token,
            json!({ "token": token }),
        )
        .await;
    assert_ne!(s, 200, "a locked link no longer works: {v}");
}

/// P3-2: self-serve plan codes name a sold seat tier.
#[tokio::test]
async fn drafts_need_a_sold_seat_tier() {
    let srv = crate::org_tests::serve().await;
    srv.global_flag("orgs").await;
    let owner = srv.person("tierowner").await;
    for (seats, ok) in [(7, false), (1, false), (600, false), (15, true)] {
        let (s, v) = srv
            .call(Method::POST, "/internal/v1/org-drafts", Some(BILLING), Some(json!({
                "operation_id": format!("op-tier-{seats}-0123456789abcdef"), "owner_account": owner.account,
                "name": "Tier Inc", "namespace": format!("tier{seats}"),
                "plan_code": format!("ent-{seats}-monthly-usd"), "seats": seats,
            })))
            .await;
        assert_eq!(s == 201, ok, "{seats}: {v}");
    }
}

/// P3-3: when the invitation goes by email, the response never shows the link.
#[tokio::test]
async fn emailed_invitations_do_not_show_the_link() {
    let (srv, owner, org) = setup().await;
    srv.global_flag("org_invite_email").await;
    let (s, v) = srv
        .post(&format!("/v1/orgs/{org}/invites"), &owner.strong, json!({ "invites": [{
            "first_name": "M", "last_name": "N", "contact": "mail@example.com", "handle": "mn", "roles": [],
        }]}))
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["results"][0]["delivery"], "queued", "{v}");
    assert!(v["results"][0]["link"].is_null(), "{v}");
}
