//! Marketplace foundations (build plan §4, M0): the routes that exercise the policy end to end.
//!
//! | Route | Action | Who |
//! |---|---|---|
//! | `GET|POST /v1/staff/roles`, `DELETE /v1/staff/roles/{role}` | `StaffRolesManage` | the platform owner, a person, with step-up |
//! | `POST /v1/publisher-applications` | `PublisherApply` | a person, for their account or an org they own or administer |
//! | `GET /v1/publisher-applications/{application}` | `PublisherManage` | the applicant's account, or the org's owner or admins |
//! | `GET /v1/staff/publisher-applications/{application}` | `NestReview` | a reviewer with step-up and no stake in the publisher |
//! | `GET /v1/marketplace/listings/{listing}` | `NestDiscover` | anyone, for a published public or unlisted listing; the publisher for its own |
//!
//! Everything answers `404 feature_disabled` unless the `marketplace` flag is on for the caller's
//! account. Schemas are in `docs/nests/contracts/pigeonpost-nests.v1.yaml`; errors and successes
//! use the envelope that contract defines. The platform owner is the account named by
//! `POSTBOX_PLATFORM_OWNER_ACCOUNT`; unset means nobody can assign a staff role.

use crate::groups::{GErr, GResult};
use crate::org::{permits, Perm};
use crate::org_api::api_error;
use crate::policy::{authorize, Action, Actor, DenyReason, Facts, Resource, StaffRole};
use crate::store::marketplace as store;
use crate::{capabilities, now_unix, principal_for_token, AppState, ClientIp, Principal};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/staff/roles",
            get(list_staff_roles).post(assign_staff_role),
        )
        .route("/v1/staff/roles/{role}", delete(revoke_staff_role))
        .route("/v1/publisher-applications", post(apply_publisher))
        .route(
            "/v1/publisher-applications/{application}",
            get(read_application),
        )
        .route(
            "/v1/staff/publisher-applications/{application}",
            get(staff_read_application),
        )
        .route("/v1/marketplace/listings/{listing}", get(read_listing))
}

pub(crate) fn not_found() -> GErr {
    GErr::new(StatusCode::NOT_FOUND, "not_found", "no such record")
}

pub(crate) fn feature_off() -> GErr {
    GErr::new(
        StatusCode::NOT_FOUND,
        "feature_disabled",
        "the marketplace is not enabled for this account",
    )
}

pub(crate) fn iso(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

/// Map a policy refusal onto the contract's error codes. `hide` is for records whose existence is
/// private: no grant reads exactly like no such record.
pub(crate) fn deny(reason: DenyReason, hide: bool) -> GErr {
    match reason {
        DenyReason::Auth(denial) => crate::org::auth_denial(denial),
        DenyReason::NotPermitted if hide => not_found(),
        DenyReason::NotPermitted => {
            GErr::forbidden("forbidden", "this account may not perform this action")
        }
        other => GErr::forbidden(other.code(), "this action is not allowed"),
    }
}

pub(crate) fn facts(state: &AppState) -> Facts {
    Facts {
        now: now_unix(),
        step_up: (*state.step_up).clone(),
        ..Facts::default()
    }
}

/// The signed-in caller and their account, once the `marketplace` flag is on for it.
pub(crate) async fn caller(state: &AppState, headers: &HeaderMap) -> GResult<(Principal, String)> {
    let principal = principal_for_token(state, crate::bearer(headers)).await?;
    let account = capabilities::principal_account(&principal)
        .map(str::to_string)
        .ok_or_else(feature_off)?;
    if !crate::filtering::feature_on(state, Some(&account), capabilities::MARKETPLACE).await {
        return Err(feature_off());
    }
    Ok((principal, account))
}

pub(crate) fn ip_of(client_ip: &Option<Extension<ClientIp>>) -> Option<std::net::IpAddr> {
    client_ip
        .as_ref()
        .and_then(|Extension(ip)| ip.as_str().parse().ok())
}

pub(crate) fn parse<T: serde::de::DeserializeOwned>(
    body: Result<Json<Value>, JsonRejection>,
) -> GResult<T> {
    let Json(value) = body.map_err(|_| GErr::bad("invalid_json", "expected a JSON object"))?;
    serde_json::from_value(value).map_err(|e| GErr::bad("invalid_request", e.to_string()))
}

pub(crate) fn into_response<T: IntoResponse>(result: GResult<T>) -> Response {
    match result {
        Ok(out) => out.into_response(),
        Err(e) => api_error(e),
    }
}

// ---- staff roles ----------------------------------------------------------------------------------

/// The platform owner, with a recent strong sign-in. Who that is comes from server configuration
/// only, never from the request or the staff table, so no staff row can promote itself.
async fn owner(state: &AppState, headers: &HeaderMap) -> GResult<(Principal, String)> {
    let (principal, account) = caller(state, headers).await?;
    let mut f = facts(state);
    f.platform_owner = state.platform_owner.as_deref() == Some(account.as_str());
    authorize(
        &Actor::Principal(&principal),
        Action::StaffRolesManage,
        &Resource::StaffRoles,
        &f,
    )
    .map_err(|d| deny(d, false))?;
    Ok((principal, account))
}

/// `GET /v1/staff/roles`
async fn list_staff_roles(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let result = async {
        owner(&state, &headers).await?;
        let rows = state
            .store
            .group_read(|c| -> GResult<Vec<Value>> {
                let mut stmt = c.prepare(
                    "SELECT id, account_id, role, granted_by, granted_at FROM staff_roles
                      WHERE revoked_at IS NULL ORDER BY granted_at, id",
                )?;
                let rows = stmt
                    .query_map([], |r| {
                        Ok(json!({
                            "id": r.get::<_, String>(0)?,
                            "account_id": r.get::<_, String>(1)?,
                            "role": r.get::<_, String>(2)?,
                            "granted_by": r.get::<_, String>(3)?,
                            "granted_at": iso(r.get::<_, i64>(4)?),
                        }))
                    })?
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
struct AssignReq {
    account_id: String,
    role: String,
}

/// `POST /v1/staff/roles`
async fn assign_staff_role(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let (principal, account) = owner(&state, &headers).await?;
        let req: AssignReq = parse(body)?;
        let role = StaffRole::parse(&req.role)
            .ok_or_else(|| GErr::bad("invalid_role", "role must be reviewer or finance"))?;
        let now = now_unix();
        let id = format!("stf_{}", crate::groups::rand_b32(16));
        let (rid, target, by) = (id.clone(), req.account_id.clone(), account.clone());
        let audit = state.auditor.row(
            crate::audit::ActorKind::of(&principal),
            Some(account.clone()),
            "staff.role.grant",
            Some(format!("account:{}", req.account_id)),
            json!({ "role": role.as_str() }),
            ip_of(&client_ip),
            headers.get(axum::http::header::USER_AGENT).and_then(|v| v.to_str().ok()),
            now,
        );
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                let exists: Option<i64> = tx
                    .query_row("SELECT 1 FROM accounts WHERE id = ?1", [&target], |r| r.get(0))
                    .optional()?;
                if exists.is_none() {
                    return Err(GErr::new(
                        StatusCode::NOT_FOUND,
                        "account_not_found",
                        "no such account",
                    ));
                }
                let live: Option<String> = tx
                    .query_row(
                        "SELECT id FROM staff_roles WHERE account_id = ?1 AND role = ?2 AND revoked_at IS NULL",
                        params![target, role.as_str()],
                        |r| r.get(0),
                    )
                    .optional()?;
                if live.is_some() {
                    return Err(GErr::conflict("role_already_held", "this account already holds this role"));
                }
                tx.execute(
                    "INSERT INTO staff_roles (id, account_id, role, granted_by, granted_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![rid, target, role.as_str(), by, now as i64],
                )?;
                crate::store::audit::append_in(tx, &audit)?;
                Ok(())
            })
            .await?;
        Ok((
            StatusCode::CREATED,
            Json(json!({ "data": {
                "id": id,
                "account_id": req.account_id,
                "role": role.as_str(),
                "granted_by": account,
                "granted_at": iso(now as i64),
            }})),
        ))
    }
    .await;
    into_response(result)
}

/// `DELETE /v1/staff/roles/{role}`
async fn revoke_staff_role(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path(role): Path<String>,
) -> Response {
    let result = async {
        let (principal, account) = owner(&state, &headers).await?;
        let now = now_unix();
        let audit = state.auditor.row(
            crate::audit::ActorKind::of(&principal),
            Some(account.clone()),
            "staff.role.revoke",
            Some(format!("staff_role:{role}")),
            json!({}),
            ip_of(&client_ip),
            headers
                .get(axum::http::header::USER_AGENT)
                .and_then(|v| v.to_str().ok()),
            now,
        );
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                let changed = tx.execute(
                    "UPDATE staff_roles SET revoked_by = ?2, revoked_at = ?3
                      WHERE id = ?1 AND revoked_at IS NULL",
                    params![role, account, now as i64],
                )?;
                if changed == 0 {
                    return Err(not_found());
                }
                crate::store::audit::append_in(tx, &audit)?;
                Ok(())
            })
            .await?;
        Ok(StatusCode::NO_CONTENT)
    }
    .await;
    into_response(result)
}

// ---- publisher applications -----------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Application {
    id: String,
    owner_kind: String,
    owner_id: String,
    display_name: String,
    status: String,
    created_at: i64,
    updated_at: i64,
}

impl Application {
    fn json(&self) -> Value {
        json!({
            "id": self.id,
            "owner_kind": self.owner_kind,
            "owner_id": self.owner_id,
            "display_name": self.display_name,
            "status": self.status,
            "created_at": iso(self.created_at),
            "updated_at": iso(self.updated_at),
        })
    }
}

async fn load_application(state: &AppState, id: &str) -> GResult<Option<Application>> {
    let id = id.to_string();
    state
        .store
        .group_read(move |c| -> GResult<Option<Application>> {
            Ok(c.query_row(
                "SELECT id, owner_kind, owner_id, display_name, status, created_at, updated_at
                   FROM publisher_profiles WHERE id = ?1",
                [&id],
                |r| {
                    Ok(Application {
                        id: r.get(0)?,
                        owner_kind: r.get(1)?,
                        owner_id: r.get(2)?,
                        display_name: r.get(3)?,
                        status: r.get(4)?,
                        created_at: r.get(5)?,
                        updated_at: r.get(6)?,
                    })
                },
            )
            .optional()?)
        })
        .await
}

/// Whether this caller is on the publisher's team: the owning account, an org owner or admin,
/// or (M2) an active org member holding a live publisher role. A failed lookup is no grant.
async fn holds_publisher(
    state: &AppState,
    headers: &HeaderMap,
    account: &str,
    publisher: &str,
) -> bool {
    crate::marketplace_m2::publisher_read(state, headers, account, publisher).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyReq {
    display_name: String,
    #[serde(default)]
    org_id: Option<String>,
}

/// `POST /v1/publisher-applications`
async fn apply_publisher(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let (principal, account) = caller(&state, &headers).await?;
        let req: ApplyReq = parse(body)?;
        let display_name = req.display_name.trim().to_string();
        if display_name.chars().count() < 2
            || display_name.chars().count() > 80
            || display_name.chars().any(char::is_control)
        {
            return Err(GErr::bad(
                "bad_display_name",
                "display_name: 2-80 printable characters",
            ));
        }
        let (kind, owner) = match &req.org_id {
            Some(org) => ("org", org.clone()),
            None => ("account", account.clone()),
        };
        let mut f = facts(&state);
        // The role is resolved here, and the decision is the policy's, for the person and the org
        // paths alike.
        let resource = if kind == "org" {
            let ctx = crate::org::org_ctx(&state, &headers, &owner).await?;
            ctx.require_writable()?;
            f.org_permitted = permits(&ctx.roles, Perm::Security, ctx.rooms_everyone());
            Resource::Org(&owner)
        } else {
            Resource::Account(&owner)
        };
        authorize(
            &Actor::Principal(&principal),
            Action::PublisherApply,
            &resource,
            &f,
        )
        .map_err(|d| deny(d, false))?;
        let now = now_unix() as i64;
        let id = format!("pub_{}", crate::groups::rand_b32(16));
        let (rid, oid, name, by) = (id.clone(), owner.clone(), display_name.clone(), account.clone());
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                let live: Option<String> = tx
                    .query_row(
                        "SELECT id FROM publisher_profiles
                          WHERE owner_kind = ?1 AND owner_id = ?2 AND status IN ('applied','approved')",
                        params![kind, oid],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(existing) = live {
                    return Err(GErr::conflict(
                        "application_exists",
                        "this owner already has a live publisher application",
                    )
                    .with("application_id", existing));
                }
                tx.execute(
                    "INSERT INTO publisher_profiles
                        (id, owner_kind, owner_id, display_name, status, applied_by, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, 'applied', ?5, ?6, ?6)",
                    params![rid, kind, oid, name, by, now],
                )?;
                Ok(())
            })
            .await?;
        let application = Application {
            id,
            owner_kind: kind.to_string(),
            owner_id: owner,
            display_name,
            status: "applied".to_string(),
            created_at: now,
            updated_at: now,
        };
        Ok((
            StatusCode::CREATED,
            Json(json!({ "data": application.json() })),
        ))
    }
    .await;
    into_response(result)
}

/// `GET /v1/publisher-applications/{application}`
async fn read_application(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        let (principal, account) = caller(&state, &headers).await?;
        let application = load_application(&state, &id).await?;
        let mut f = facts(&state);
        if let Some(a) = &application {
            f.publisher_permitted = holds_publisher(&state, &headers, &account, &a.id).await;
        }
        authorize(
            &Actor::Principal(&principal),
            Action::PublisherManage,
            &Resource::Publisher(&id),
            &f,
        )
        .map_err(|d| deny(d, true))?;
        let a = application.ok_or_else(not_found)?;
        Ok(Json(json!({ "data": a.json() })))
    }
    .await;
    into_response(result)
}

/// `GET /v1/staff/publisher-applications/{application}`
async fn staff_read_application(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        let (principal, account) = caller(&state, &headers).await?;
        let application = load_application(&state, &id).await?;
        let (who, publisher) = (account.clone(), id.clone());
        let found = application.is_some();
        let (roles, conflict) = state
            .store
            .group_read(move |c| -> GResult<(Vec<StaffRole>, bool)> {
                Ok((
                    store::live_staff_roles_in(c, &who)?,
                    store::publisher_conflict_in(c, &who, &publisher)?,
                ))
            })
            .await?;
        let mut f = facts(&state);
        f.staff_roles = roles;
        // A record that does not exist is answered 404 below, not as a conflict with nothing.
        f.staff_conflict = found && conflict;
        authorize(
            &Actor::Principal(&principal),
            Action::NestReview,
            &Resource::StaffSubject(&id),
            &f,
        )
        .map_err(|d| deny(d, false))?;
        let a = application.ok_or_else(not_found)?;
        Ok(Json(json!({ "data": a.json() })))
    }
    .await;
    into_response(result)
}

// ---- listings -------------------------------------------------------------------------------------

struct ListingRow {
    body: Value,
    publisher_id: String,
    visibility: String,
    status: String,
}

/// Published and open to whoever has the link. Unlisted listings are reachable but not searchable;
/// private ones only by the publisher's team (M2); invite-only and organization listings need
/// grants that are not built yet.
fn publicly_visible(status: &str, visibility: &str) -> bool {
    status == "published" && matches!(visibility, "public" | "unlisted")
}

/// `GET /v1/marketplace/listings/{listing}` — public metadata, what the listing's release offers
/// (operations and configuration schema, M2) and who publishes it.
async fn read_listing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        // A credential is optional here, but one that was presented has to be valid.
        let principal = match crate::bearer(&headers) {
            None => None,
            Some(t) => Some(principal_for_token(&state, Some(t)).await?),
        };
        let account = principal
            .as_ref()
            .and_then(capabilities::principal_account)
            .map(str::to_string);
        if !crate::filtering::feature_on(&state, account.as_deref(), capabilities::MARKETPLACE)
            .await
        {
            return Err(feature_off());
        }
        let lid = id.clone();
        let row = state
            .store
            .group_read(move |c| -> GResult<Option<ListingRow>> {
                let found = c
                    .query_row(
                        "SELECT l.id, l.publisher_id, l.release_id, l.title, l.summary, l.visibility,
                                l.status, p.display_name, p.owner_kind, l.description, l.address,
                                l.published_at, r.version, r.manifest, l.price_currency, l.price_term,
                                l.hosting, l.service_address
                           FROM nest_listings l JOIN publisher_profiles p ON p.id = l.publisher_id
                           JOIN nest_releases r ON r.id = l.release_id
                          WHERE l.id = ?1",
                        [&lid],
                        |r| {
                            let visibility: String = r.get(5)?;
                            let status: String = r.get(6)?;
                            let manifest: Value = r
                                .get::<_, Option<String>>(13)?
                                .and_then(|t| serde_json::from_str(&t).ok())
                                .unwrap_or(Value::Null);
                            Ok(ListingRow {
                                body: json!({
                                    "id": r.get::<_, String>(0)?,
                                    "publisher_id": r.get::<_, String>(1)?,
                                    "release_id": r.get::<_, String>(2)?,
                                    "title": r.get::<_, String>(3)?,
                                    "summary": r.get::<_, String>(4)?,
                                    "visibility": visibility,
                                    "status": status,
                                    "publisher": {
                                        "display_name": r.get::<_, String>(7)?,
                                        "first_party": r.get::<_, String>(8)? == "platform",
                                    },
                                    "description": r.get::<_, String>(9)?,
                                    "address": r.get::<_, Option<String>>(10)?,
                                    "published_at": r.get::<_, Option<i64>>(11)?.map(iso),
                                    // `publisher`: the publisher runs it; Pigeonpost reviewed the
                                    // publisher and the integration, not the code.
                                    "hosting": r.get::<_, String>(16)?,
                                    "service_address": r.get::<_, Option<String>>(17)?,
                                    "version": r.get::<_, String>(12)?,
                                    "config_schema": manifest.get("config_schema").cloned()
                                        .unwrap_or_else(pigeonpost_core::nest_package::empty_config_schema),
                                    "operations": manifest["operations"].as_array().cloned()
                                        .unwrap_or_default(),
                                    "price": {
                                        "amount_minor": 0,
                                        "currency": r.get::<_, String>(14)?,
                                        "term": r.get::<_, String>(15)?,
                                        "funding": "test_credits",
                                    },
                                }),
                                publisher_id: r.get(1)?,
                                visibility,
                                status,
                            })
                        },
                    )
                    .optional()?;
                // The one price model: the publisher's price, or zero with the free terms (M2b).
                let mut found = found;
                if let Some(row) = found.as_mut() {
                    row.body["price"] = crate::marketplace_pay::price_json_in(c, &lid)?;
                }
                Ok(found)
            })
            .await?;
        let mut f = facts(&state);
        if let Some(listing) = &row {
            f.listing_visible = publicly_visible(&listing.status, &listing.visibility);
            if !f.listing_visible {
                if let Some(account) = &account {
                    f.listing_visible =
                        holds_publisher(&state, &headers, account, &listing.publisher_id).await;
                    // M2b: an invite or organization grant that allows discovery.
                    if !f.listing_visible && listing.status == "published" {
                        let (l, v, a) = (id.clone(), listing.visibility.clone(), account.clone());
                        f.listing_visible = state
                            .store
                            .group_read(move |c| -> GResult<bool> {
                                Ok(crate::marketplace_grants::account_grant_in(
                                    c,
                                    &l,
                                    &v,
                                    &a,
                                    crate::marketplace_grants::Use::Discover,
                                )?)
                            })
                            .await?;
                    }
                }
            }
        }
        let actor = match &principal {
            Some(p) => Actor::Principal(p),
            None => Actor::Anonymous,
        };
        authorize(
            &actor,
            Action::NestDiscover,
            &Resource::NestListing(&id),
            &f,
        )
        .map_err(|d| deny(d, true))?;
        let listing = row.ok_or_else(not_found)?;
        Ok(Json(json!({ "data": listing.body })))
    }
    .await;
    into_response(result)
}
