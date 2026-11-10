//! Marketplace M2b: invite-only and organization visibility (build plan R-52).
//!
//! | Route | Action | Who |
//! |---|---|---|
//! | `GET\|POST /v1/publishers/{publisher}/listings/{listing}/grants` | `PublisherManage` | read: the team; create: a certified publisher's admins |
//! | `DELETE /v1/publishers/{publisher}/listings/{listing}/grants/{grant}` | `PublisherManage` | the publisher's admins |
//! | `GET /v1/marketplace/invites` | `NestInstall` | a person: invites to their mailboxes |
//! | `POST /v1/marketplace/invites/{grant}/accept`, `…/decline` | `NestInstall` | the person holding the invited mailbox |
//!
//! A grant matches its listing's visibility: `invite` grants serve `invite_only` listings,
//! `organization` grants serve `organization` listings. Each grant has three switches, decided
//! separately and checked live: `discover` (the catalog and the listing page), `install` (a new
//! installation) and `invoke` (every job, so a revoke stops use at once). An invite names a
//! mailbox address and binds to the account that accepts it; whether the address exists is never
//! revealed to the publisher. An organization grant holds for the org's active members while the
//! org is live, and for the org's own installations.

use crate::groups::{GErr, GResult};
use crate::marketplace::{caller, deny, facts, into_response, iso, not_found, parse};
use crate::marketplace_m2::{certified, gate, new_id, wake, Need};
use crate::policy::{authorize, Action, Actor, Resource};
use crate::store::marketplace as store;
use crate::{now_unix, AppState};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

/// Grants one listing may hold at once (live ones).
const MAX_GRANTS: i64 = 500;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/publishers/{publisher}/listings/{listing}/grants",
            get(list_grants).post(create_grant),
        )
        .route(
            "/v1/publishers/{publisher}/listings/{listing}/grants/{grant}",
            delete(revoke_grant),
        )
        .route("/v1/marketplace/invites", get(my_invites))
        .route("/v1/marketplace/invites/{grant}/accept", post(accept))
        .route("/v1/marketplace/invites/{grant}/decline", post(decline))
}

/// One of a grant's three switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Use {
    Discover,
    Install,
    Invoke,
}

impl Use {
    fn column(self) -> &'static str {
        match self {
            Use::Discover => "can_discover",
            Use::Install => "can_install",
            Use::Invoke => "can_invoke",
        }
    }
}

/// Orgs the account is an active member of, while the org is live.
const LIVE_ORGS: &str = "SELECT m.org_id FROM org_members m JOIN orgs o ON o.id = m.org_id
     WHERE m.account_id = ?1 AND m.status = 'active' AND o.status IN ('active','past_due','read_only')";

/// For the catalog: a published listing `l` the account in `?4` may discover through a grant.
pub(crate) const DISCOVER_SQL: &str = "EXISTS (SELECT 1 FROM nest_listing_grants g
     WHERE g.listing_id = l.id AND g.status = 'active' AND g.can_discover = 1 AND ?4 <> ''
       AND ((l.visibility = 'invite_only' AND g.kind = 'invite' AND g.account_id = ?4)
         OR (l.visibility = 'organization' AND g.kind = 'organization'
             AND g.org_id IN (SELECT m.org_id FROM org_members m JOIN orgs o ON o.id = m.org_id
                               WHERE m.account_id = ?4 AND m.status = 'active'
                                 AND o.status IN ('active','past_due','read_only')))))";

/// Whether `account` holds a live grant switch `what` on a listing of `visibility`. Public,
/// unlisted and private listings have no grants: false.
pub(crate) fn account_grant_in(
    c: &Connection,
    listing: &str,
    visibility: &str,
    account: &str,
    what: Use,
) -> rusqlite::Result<bool> {
    let col = what.column();
    match visibility {
        "invite_only" => c.query_row(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM nest_listing_grants WHERE listing_id = ?2
                   AND kind = 'invite' AND status = 'active' AND account_id = ?1 AND {col} = 1)"
            ),
            params![account, listing],
            |r| r.get(0),
        ),
        "organization" => c.query_row(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM nest_listing_grants WHERE listing_id = ?2
                   AND kind = 'organization' AND status = 'active' AND {col} = 1
                   AND org_id IN ({LIVE_ORGS}))"
            ),
            params![account, listing],
            |r| r.get(0),
        ),
        _ => Ok(false),
    }
}

/// Whether the org itself holds a live grant switch `what` (its own installations). Only
/// `organization` grants name an org; an invite is a person's.
pub(crate) fn org_grant_in(
    c: &Connection,
    listing: &str,
    visibility: &str,
    org: &str,
    what: Use,
) -> rusqlite::Result<bool> {
    if visibility != "organization" {
        return Ok(false);
    }
    c.query_row(
        &format!(
            "SELECT EXISTS (SELECT 1 FROM nest_listing_grants g JOIN orgs o ON o.id = g.org_id
               WHERE g.listing_id = ?1 AND g.kind = 'organization' AND g.status = 'active'
                 AND g.org_id = ?2 AND g.{} = 1
                 AND o.status IN ('active','past_due','read_only'))",
            what.column()
        ),
        params![listing, org],
        |r| r.get(0),
    )
}

/// `install` on a granted listing for an account: `Ok(true)` may install; `Ok(false)` sees
/// nothing (404 upstream); `403 grant_required` when the caller may see it but not install it.
pub(crate) async fn install_allowed(
    state: &AppState,
    listing: &str,
    visibility: &str,
    account: &str,
) -> GResult<bool> {
    let (l, v, a) = (
        listing.to_string(),
        visibility.to_string(),
        account.to_string(),
    );
    let (see, may) = state
        .store
        .group_read(move |c| -> GResult<(bool, bool)> {
            Ok((
                account_grant_in(c, &l, &v, &a, Use::Discover)?,
                account_grant_in(c, &l, &v, &a, Use::Install)?,
            ))
        })
        .await?;
    if see && !may {
        return Err(GErr::forbidden(
            "grant_required",
            "your access to this listing does not include installing it",
        ));
    }
    Ok(may)
}

fn grant_json(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": r.get::<_, String>(0)?,
        "listing_id": r.get::<_, String>(1)?,
        "kind": r.get::<_, String>(2)?,
        "address": r.get::<_, Option<String>>(3)?,
        "org_id": r.get::<_, Option<String>>(4)?,
        "discover": r.get::<_, i64>(5)? == 1,
        "install": r.get::<_, i64>(6)? == 1,
        "invoke": r.get::<_, i64>(7)? == 1,
        "status": r.get::<_, String>(8)?,
        "created_at": iso(r.get(9)?),
        "updated_at": iso(r.get(10)?),
        "accepted_at": r.get::<_, Option<i64>>(11)?.map(iso),
        "revoked_at": r.get::<_, Option<i64>>(12)?.map(iso),
    }))
}

const GRANT_COLS: &str =
    "id, listing_id, kind, invitee_address, org_id, can_discover, can_install, \
     can_invoke, status, created_at, updated_at, accepted_at, revoked_at";

/// The listing's visibility, if it is this publisher's.
fn listing_visibility_in(
    c: &Connection,
    listing: &str,
    publisher: &str,
) -> rusqlite::Result<Option<String>> {
    c.query_row(
        "SELECT visibility FROM nest_listings WHERE id = ?1 AND publisher_id = ?2",
        params![listing, publisher],
        |r| r.get(0),
    )
    .optional()
}

/// `GET /v1/publishers/{publisher}/listings/{listing}/grants`
async fn list_grants(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((publisher, listing)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = gate(
            &state,
            &headers,
            &publisher,
            Action::PublisherManage,
            Need::Read,
        )
        .await?;
        let pid = ctx.p.id.clone();
        let rows = state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> {
                if listing_visibility_in(c, &listing, &pid)?.is_none() {
                    return Err(not_found());
                }
                let mut stmt = c.prepare(&format!(
                    "SELECT {GRANT_COLS} FROM nest_listing_grants WHERE listing_id = ?1
                      ORDER BY created_at, id LIMIT 500"
                ))?;
                let rows = stmt
                    .query_map([&listing], grant_json)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await?;
        Ok(Json(json!({ "data": rows, "next_cursor": null })))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantReq {
    kind: String,
    #[serde(default)]
    address: Option<String>,
    #[serde(default)]
    org_id: Option<String>,
    #[serde(default = "yes")]
    discover: bool,
    #[serde(default = "yes")]
    install: bool,
    #[serde(default = "yes")]
    invoke: bool,
}

fn yes() -> bool {
    true
}

/// A mailbox address or handle as Pigeonpost writes them (`/k/<key>`, `/namespace/name[/agent]`),
/// lower case.
fn address_ok(a: &str) -> bool {
    a.len() <= 200
        && a.starts_with('/')
        && a.split('/').skip(1).all(|seg| {
            !seg.is_empty()
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_.".contains(&b))
        })
        && a.matches('/').count() <= 3
}

/// The accounts that hold `address` now: its own account, and the member behind an org mailbox.
fn holders_in(c: &Connection, address: &str) -> rusqlite::Result<Vec<String>> {
    let mut out = Vec::new();
    if let Some(Some(a)) = c
        .query_row(
            "SELECT account_id FROM identities WHERE address = ?1 OR handle = ?1 LIMIT 1",
            [address],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
    {
        out.push(a);
    }
    let mut stmt = c.prepare(
        "SELECT m.account_id FROM org_members m WHERE m.identity = ?1 AND m.status = 'active'
            AND m.account_id IS NOT NULL",
    )?;
    for a in stmt.query_map([address], |r| r.get::<_, String>(0))? {
        out.push(a?);
    }
    Ok(out)
}

/// `POST /v1/publishers/{publisher}/listings/{listing}/grants` — invite a mailbox address to an
/// `invite_only` listing, or grant an org an `organization` listing. The response is the same
/// whether or not the address exists.
async fn create_grant(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((publisher, listing)): Path<(String, String)>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let ctx = gate(
            &state,
            &headers,
            &publisher,
            Action::PublisherManage,
            Need::Admin,
        )
        .await?;
        certified(&ctx.p)?;
        let req: GrantReq = parse(body)?;
        if !(req.discover || req.install || req.invoke) {
            return Err(GErr::bad(
                "invalid_request",
                "a grant allows at least one of discover, install or invoke",
            ));
        }
        let (address, org) = match (req.kind.as_str(), &req.address, &req.org_id) {
            ("invite", Some(a), None) => {
                let a = a.trim().to_ascii_lowercase();
                if !address_ok(&a) {
                    return Err(GErr::bad("invalid_request", "address: a mailbox address"));
                }
                (Some(a), None)
            }
            ("organization", None, Some(o)) if o.starts_with("org_") && o.len() <= 64 => {
                (None, Some(o.clone()))
            }
            _ => {
                return Err(GErr::bad(
                    "invalid_request",
                    "kind invite with an address, or kind organization with an org_id",
                ))
            }
        };
        let now = now_unix() as i64;
        let id = new_id("gnt");
        let (gid, pid, by) = (id.clone(), ctx.p.id.clone(), ctx.account.clone());
        let (v, wake_list) = state
            .store
            .group_tx(move |tx| -> GResult<(Value, Vec<String>)> {
                let visibility = listing_visibility_in(tx, &listing, &pid)?.ok_or_else(not_found)?;
                let wanted = if address.is_some() { "invite_only" } else { "organization" };
                if visibility != wanted {
                    return Err(GErr::conflict(
                        "grant_kind_mismatch",
                        format!("this listing's visibility is {visibility}; {} grants serve {wanted} listings", req.kind),
                    ));
                }
                if let Some(o) = &org {
                    let live: bool = tx.query_row(
                        "SELECT EXISTS (SELECT 1 FROM orgs WHERE id = ?1)",
                        [o],
                        |r| r.get(0),
                    )?;
                    if !live {
                        return Err(GErr::new(StatusCode::NOT_FOUND, "not_found", "no such organization"));
                    }
                }
                let live_count: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM nest_listing_grants WHERE listing_id = ?1
                       AND status IN ('pending','active')",
                    [&listing],
                    |r| r.get(0),
                )?;
                if live_count >= MAX_GRANTS {
                    return Err(GErr::conflict("too_many_grants", "this listing holds the most grants it may"));
                }
                let held: Option<String> = tx
                    .query_row(
                        "SELECT id FROM nest_listing_grants WHERE listing_id = ?1
                           AND ((kind = 'invite' AND invitee_address = ?2 AND status IN ('pending','active'))
                             OR (kind = 'organization' AND org_id = ?3 AND status = 'active'))",
                        params![listing, address, org],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(existing) = held {
                    return Err(GErr::conflict("grant_exists", "this listing already has that grant")
                        .with("grant_id", existing));
                }
                let status = if address.is_some() { "pending" } else { "active" };
                tx.execute(
                    "INSERT INTO nest_listing_grants (id, listing_id, kind, invitee_address, org_id,
                        can_discover, can_install, can_invoke, status, created_by, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
                    params![
                        gid, listing, req.kind, address, org, req.discover as i64,
                        req.install as i64, req.invoke as i64, status, by, now
                    ],
                )?;
                // The invitee hears of it on `/v1/events`; the publisher learns nothing about
                // whether the address exists.
                let mut woken = Vec::new();
                if let Some(a) = &address {
                    for holder in holders_in(tx, a)? {
                        store::account_event_in(
                            tx,
                            &holder,
                            "nest.invite.received",
                            &json!({ "grant_id": gid, "listing_id": listing }),
                            now as u64,
                        )?;
                        woken.push(holder);
                    }
                }
                woken.extend(store::publisher_event_in(
                    tx,
                    &pid,
                    "nest.listing.grant_changed",
                    &json!({ "listing_id": listing, "grant_id": gid, "status": status }),
                    now as u64,
                )?);
                let v = tx.query_row(
                    &format!("SELECT {GRANT_COLS} FROM nest_listing_grants WHERE id = ?1"),
                    [&gid],
                    grant_json,
                )?;
                Ok((v, woken))
            })
            .await?;
        wake(&state, wake_list);
        Ok((StatusCode::CREATED, Json(json!({ "data": v }))))
    }
    .await;
    into_response(result)
}

/// `DELETE /v1/publishers/{publisher}/listings/{listing}/grants/{grant}` — revoke: discovery,
/// new installs and every later job through it stop at once.
async fn revoke_grant(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((publisher, listing, grant)): Path<(String, String, String)>,
) -> Response {
    let result = async {
        let ctx = gate(
            &state,
            &headers,
            &publisher,
            Action::PublisherManage,
            Need::Admin,
        )
        .await?;
        let now = now_unix() as i64;
        let (pid, by) = (ctx.p.id.clone(), ctx.account.clone());
        let (v, wake_list) = state
            .store
            .group_tx(move |tx| -> GResult<(Value, Vec<String>)> {
                if listing_visibility_in(tx, &listing, &pid)?.is_none() {
                    return Err(not_found());
                }
                let row: Option<(String, Option<String>)> = tx
                    .query_row(
                        "SELECT status, account_id FROM nest_listing_grants WHERE id = ?1 AND listing_id = ?2",
                        params![grant, listing],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                let Some((status, holder)) = row else {
                    return Err(not_found());
                };
                if !matches!(status.as_str(), "pending" | "active") {
                    return Err(GErr::conflict("grant_not_live", "this grant is not live"));
                }
                tx.execute(
                    "UPDATE nest_listing_grants SET status = 'revoked', revoked_by = ?2, revoked_at = ?3,
                        updated_at = ?3 WHERE id = ?1",
                    params![grant, by, now],
                )?;
                let mut woken = store::publisher_event_in(
                    tx,
                    &pid,
                    "nest.listing.grant_changed",
                    &json!({ "listing_id": listing, "grant_id": grant, "status": "revoked" }),
                    now as u64,
                )?;
                if let Some(h) = holder {
                    store::account_event_in(
                        tx,
                        &h,
                        "nest.invite.revoked",
                        &json!({ "grant_id": grant, "listing_id": listing }),
                        now as u64,
                    )?;
                    woken.push(h);
                }
                let v = tx.query_row(
                    &format!("SELECT {GRANT_COLS} FROM nest_listing_grants WHERE id = ?1"),
                    [&grant],
                    grant_json,
                )?;
                Ok((v, woken))
            })
            .await?;
        wake(&state, wake_list);
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

/// The caller, a person, for the invitee routes.
async fn invitee(state: &AppState, headers: &HeaderMap) -> GResult<String> {
    let (principal, account) = caller(state, headers).await?;
    authorize(
        &Actor::Principal(&principal),
        Action::NestInstall,
        &Resource::Account(&account),
        &facts(state),
    )
    .map_err(|d| deny(d, false))?;
    Ok(account)
}

/// Whether the invite is addressed to a mailbox `account` holds now, or was accepted by it.
fn addressed_to_in(c: &Connection, grant: &str, account: &str) -> rusqlite::Result<bool> {
    let row: Option<(String, Option<String>)> = c
        .query_row(
            "SELECT invitee_address, account_id FROM nest_listing_grants WHERE id = ?1 AND kind = 'invite'",
            [grant],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((address, accepted_by)) = row else {
        return Ok(false);
    };
    if accepted_by.as_deref() == Some(account) {
        return Ok(true);
    }
    Ok(accepted_by.is_none() && holders_in(c, &address)?.iter().any(|a| a == account))
}

/// `GET /v1/marketplace/invites` — pending and active invites to the caller's mailboxes, with
/// enough of the listing to decide.
async fn my_invites(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let result = async {
        let account = invitee(&state, &headers).await?;
        let rows = state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> {
                let mut stmt = c.prepare(
                    "SELECT g.id FROM nest_listing_grants g
                      WHERE g.kind = 'invite' AND g.status IN ('pending','active')
                        AND (g.account_id = ?1 OR (g.account_id IS NULL AND (
                              g.invitee_address IN (SELECT address FROM identities WHERE account_id = ?1)
                           OR g.invitee_address IN (SELECT handle FROM identities
                                                     WHERE account_id = ?1 AND handle IS NOT NULL)
                           OR g.invitee_address IN (SELECT identity FROM org_members
                                                     WHERE account_id = ?1 AND status = 'active'))))
                      ORDER BY g.created_at DESC, g.id LIMIT 200",
                )?;
                let ids = stmt
                    .query_map([&account], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let mut out = Vec::new();
                for id in ids {
                    let mut v = c.query_row(
                        &format!("SELECT {GRANT_COLS} FROM nest_listing_grants WHERE id = ?1"),
                        [&id],
                        grant_json,
                    )?;
                    let listing: (String, String, String, String) = c.query_row(
                        "SELECT l.title, l.summary, l.status, p.display_name FROM nest_listing_grants g
                           JOIN nest_listings l ON l.id = g.listing_id
                           JOIN publisher_profiles p ON p.id = l.publisher_id WHERE g.id = ?1",
                        [&id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )?;
                    v["listing"] = json!({
                        "title": listing.0, "summary": listing.1, "status": listing.2,
                        "publisher": { "display_name": listing.3 },
                    });
                    out.push(v);
                }
                Ok(out)
            })
            .await?;
        Ok(Json(json!({ "data": rows, "next_cursor": null })))
    }
    .await;
    into_response(result)
}

async fn answer(
    state: &AppState,
    headers: &HeaderMap,
    grant: String,
    accept: bool,
) -> GResult<Value> {
    let account = invitee(state, headers).await?;
    let now = now_unix() as i64;
    let (v, wake_list) = state
        .store
        .group_tx(move |tx| -> GResult<(Value, Vec<String>)> {
            if !addressed_to_in(tx, &grant, &account)? {
                return Err(not_found());
            }
            let (status, listing, publisher): (String, String, String) = tx.query_row(
                "SELECT g.status, g.listing_id, l.publisher_id FROM nest_listing_grants g
                   JOIN nest_listings l ON l.id = g.listing_id WHERE g.id = ?1",
                [&grant],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            match (status.as_str(), accept) {
                ("pending", true) => {
                    tx.execute(
                        "UPDATE nest_listing_grants SET status = 'active', account_id = ?2,
                            accepted_at = ?3, updated_at = ?3 WHERE id = ?1",
                        params![grant, account, now],
                    )?;
                }
                ("active", true) => {}
                ("pending" | "active", false) => {
                    tx.execute(
                        "UPDATE nest_listing_grants SET status = 'declined', updated_at = ?2 WHERE id = ?1",
                        params![grant, now],
                    )?;
                }
                _ => return Err(GErr::conflict("grant_not_live", "this invite is no longer open")),
            }
            let new_status = if accept { "active" } else { "declined" };
            let w = if status == new_status {
                Vec::new()
            } else {
                store::publisher_event_in(
                    tx,
                    &publisher,
                    "nest.listing.grant_changed",
                    &json!({ "listing_id": listing, "grant_id": grant, "status": new_status }),
                    now as u64,
                )?
            };
            let v = tx.query_row(
                &format!("SELECT {GRANT_COLS} FROM nest_listing_grants WHERE id = ?1"),
                [&grant],
                grant_json,
            )?;
            Ok((v, w))
        })
        .await?;
    wake(state, wake_list);
    Ok(json!({ "data": v }))
}

/// `POST /v1/marketplace/invites/{grant}/accept` — bind the invite to the caller's account.
async fn accept(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(grant): Path<String>,
) -> Response {
    into_response(answer(&state, &headers, grant, true).await.map(Json))
}

/// `POST /v1/marketplace/invites/{grant}/decline`
async fn decline(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(grant): Path<String>,
) -> Response {
    into_response(answer(&state, &headers, grant, false).await.map(Json))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_mailbox_paths() {
        for ok in ["/pp/jane", "/acme/bob/agent-1", "/x/y.z"] {
            assert!(address_ok(ok), "{ok}");
        }
        for bad in [
            "",
            "pp/jane",
            "/pp/",
            "/PP/jane",
            "/a/b/c/d",
            "/pp/ja ne",
            "//x",
        ] {
            assert!(!address_ok(bad), "{bad}");
        }
    }
}
