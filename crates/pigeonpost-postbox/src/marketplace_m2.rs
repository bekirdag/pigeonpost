//! Marketplace M2 (build plan §8, "publisher beta"): third-party publishers, their builds and
//! reviews, listings and visibility, and installations with configuration, update, rollback and
//! uninstall. Free only: no money moves, installations are funded by test credits.
//!
//! | Route | Action | Who |
//! |---|---|---|
//! | `GET /v1/publishers` | `PublisherManage` | a person: the publishers they act for |
//! | `GET /v1/publishers/{publisher}` | `PublisherManage` | the publisher's team (any role) |
//! | `GET\|POST /v1/publishers/{publisher}/roles`, `DELETE …/roles/{role}` | `PublisherManage` | read: the team; change: an org publisher's org owner or admin |
//! | `GET\|POST /v1/publishers/{publisher}/templates` | `PublisherManage` | read: the team; create: a certified publisher's admins |
//! | `GET\|POST /v1/publishers/{publisher}/builds`, `GET …/builds/{build}` | `PublisherManage` | same |
//! | `GET /v1/publishers/{publisher}/reviews[/{review}[/findings]]` | `PublisherManage` | the team; findings are redacted |
//! | `POST /v1/publishers/{publisher}/reviews/{review}/withdraw` | `PublisherManage` | the publisher's admins |
//! | `GET\|POST /v1/publishers/{publisher}/listings`, `PATCH …/listings/{listing}`, `POST …/withdraw` | `PublisherManage` | read: the team; change: a certified publisher's admins |
//! | `POST /v1/publishers/{publisher}/listings/{listing}/publish` | `NestPublish` | a certified publisher's admin, with step-up |
//! | `GET /v1/marketplace/listings` | `NestDiscover` | anyone: published public listings only |
//! | `GET\|POST /v1/nest-installations` | `NestInstall` | a person, for their own account |
//! | `GET\|DELETE /v1/nest-installations/{installation}` | `NestManage` | the installing account, a person |
//! | `PUT /v1/nest-installations/{installation}/configuration` | `NestConfigure` | same |
//! | `POST /v1/nest-installations/{installation}/{update,rollback}` | `NestManage` | same |
//! | `GET /v1/staff/publisher-applications`, `GET /v1/staff/reviews` | `NestReview` | a reviewer with step-up |
//! | `POST /v1/staff/publisher-applications/{application}/decisions` | `NestReview` | a reviewer with step-up and no stake |
//! | `POST /v1/staff/publishers/{publisher}/revocation` | `NestQuarantine` | same |
//! | `POST /v1/staff/listings/{listing}/moderation` | `NestQuarantine` | same |
//! | `GET /internal/v1/marketplace/build-requests`, `POST …/{build}/failure` | ops service | the build plane |
//!
//! The review pipeline is M1's (`marketplace_m1`): a publisher's build request is answered by the
//! build plane through `POST /internal/v1/marketplace/builds` with `build_id`, and the stages,
//! staff decision and attestation are the same routes and rules. This module adds the publisher's
//! side of it and the steps after an approval.
//!
//! Org publisher roles (`publisher_admin`, `publisher_finance`) are kept in `publisher_roles`, not
//! as new `org_roles` values, so the live org table is not rebuilt. An org's owner and full admins
//! always act for its publisher.

use crate::groups::{GErr, GResult};
use crate::marketplace::{caller, deny, facts, into_response, ip_of, iso, not_found, parse};
use crate::marketplace_m1::{
    attestation_in, buyer_context, conflict_code, digest_of, flag_gate, service_gate, user_agent,
    BuildReq, AI_REVIEW_MODEL, STATEMENT_TYPE,
};
use crate::org::{permits, Perm};
use crate::policy::{authorize, Action, Actor, DenyReason, Facts, Resource, StaffRole};
use crate::store::marketplace as store;
use crate::{now_unix, AppState, ClientIp, Principal};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{delete, get, patch, post, put};
use axum::{Extension, Json, Router};
use pigeonpost_core::nest_package as pkg;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap};

/// The review stages in order, as the contract lists them.
pub(crate) const STAGES: [&str; 6] = [
    "intake",
    "deterministic_scan",
    "ai_analysis",
    "behavioral_test",
    "human_review",
    "attestation",
];

/// What every review is held to. Its digest is the contract's `policy_digest`; a change to the
/// pipeline changes this text and so the digest every publisher sees.
const REVIEW_POLICY: &str = "pigeonpost-nest-review-policy-v1\n\
     intake: built by Pigeonpost from the pinned source, SBOM and release digest\n\
     deterministic_scan: secrets, manifest limits, licences; a blocking finding fails\n\
     ai_analysis: advisory only, self-hosted model qwen-latest-flash-next\n\
     behavioral_test: honeytokens; any leak or unanswered job fails\n\
     human_review: named staff, step-up, no stake in the publisher or the release\n\
     attestation: over the release digest\n";

/// Vault label for sealed configuration secrets.
const SECRET_LABEL: &str = "marketplace-config-secret-v1";
/// Most rows a list returns.
pub(crate) const PAGE: usize = 100;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/publishers", get(list_publishers))
        .route("/v1/publishers/{publisher}", get(read_publisher))
        .route(
            "/v1/publishers/{publisher}/roles",
            get(list_roles).post(grant_role),
        )
        .route(
            "/v1/publishers/{publisher}/roles/{role}",
            delete(revoke_role),
        )
        .route(
            "/v1/publishers/{publisher}/templates",
            get(list_templates).post(create_template),
        )
        .route(
            "/v1/publishers/{publisher}/builds",
            get(list_builds).post(request_build),
        )
        .route("/v1/publishers/{publisher}/builds/{build}", get(read_build))
        .route("/v1/publishers/{publisher}/reviews", get(list_reviews))
        .route(
            "/v1/publishers/{publisher}/reviews/{review}",
            get(read_review),
        )
        .route(
            "/v1/publishers/{publisher}/reviews/{review}/findings",
            get(read_findings),
        )
        .route(
            "/v1/publishers/{publisher}/reviews/{review}/withdraw",
            post(withdraw_review),
        )
        .route(
            "/v1/publishers/{publisher}/listings",
            get(list_listings).post(create_listing),
        )
        .route(
            "/v1/publishers/{publisher}/listings/{listing}",
            patch(edit_listing),
        )
        .route(
            "/v1/publishers/{publisher}/listings/{listing}/publish",
            post(publish_listing),
        )
        .route(
            "/v1/publishers/{publisher}/listings/{listing}/withdraw",
            post(withdraw_listing),
        )
        .route("/v1/marketplace/listings", get(catalog))
        .route(
            "/v1/nest-installations",
            get(list_installations).post(install),
        )
        .route(
            "/v1/nest-installations/{installation}",
            get(read_installation).delete(uninstall),
        )
        .route(
            "/v1/nest-installations/{installation}/configuration",
            put(configure),
        )
        .route(
            "/v1/nest-installations/{installation}/update",
            post(update_installation),
        )
        .route(
            "/v1/nest-installations/{installation}/rollback",
            post(rollback_installation),
        )
        .route(
            "/v1/staff/publisher-applications",
            get(staff_list_applications),
        )
        .route(
            "/v1/staff/publisher-applications/{application}/decisions",
            post(staff_certify),
        )
        .route(
            "/v1/staff/publishers/{publisher}/revocation",
            post(staff_revoke_publisher),
        )
        .route("/v1/staff/reviews", get(staff_list_reviews))
        .route(
            "/v1/staff/listings/{listing}/moderation",
            post(staff_moderate),
        )
}

pub(crate) fn internal_router() -> Router<AppState> {
    Router::new()
        .route(
            "/internal/v1/marketplace/build-requests",
            get(pending_builds),
        )
        .route(
            "/internal/v1/marketplace/build-requests/{build}/failure",
            post(build_failed),
        )
}

// ---- shared pieces used by the M1 pipeline ---------------------------------------------------------

/// The stage a stage needs before it (M2: no skipping). `None` for the first.
pub(crate) fn stage_before(stage: &str) -> Option<&'static str> {
    let i = STAGES.iter().position(|s| *s == stage)?;
    i.checked_sub(1).map(|j| STAGES[j])
}

/// The input schema (`{"fields": […]}`) of one operation of a release manifest.
pub(crate) fn operation_schema(manifest: &Value, operation: &str, version: &str) -> Option<String> {
    manifest["operations"]
        .as_array()?
        .iter()
        .find(|o| o["name"] == operation && o["version"] == version)
        .map(|o| json!({ "fields": o["fields"] }).to_string())
}

/// Replace a listing's operations with its release manifest's.
pub(crate) fn operations_in(
    c: &Connection,
    listing: &str,
    manifest: &Value,
) -> rusqlite::Result<()> {
    c.execute(
        "DELETE FROM nest_operations WHERE listing_id = ?1",
        [listing],
    )?;
    for op in manifest["operations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        c.execute(
            "INSERT INTO nest_operations (listing_id, operation, operation_version, input_schema)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                listing,
                op["name"].as_str().unwrap_or(""),
                op["version"].as_str().unwrap_or(""),
                json!({ "fields": op["fields"] }).to_string()
            ],
        )?;
    }
    Ok(())
}

/// The configuration a worker receives with a job: the stored (non-secret) values plus the
/// release schema's defaults. Null for a job outside any installation.
pub(crate) fn delivered_config(manifest: Option<&str>, config: Option<&str>) -> Value {
    let Some(config) = config else {
        return Value::Null;
    };
    let plain: Map<String, Value> = serde_json::from_str(config).unwrap_or_default();
    let schema = manifest
        .and_then(|m| serde_json::from_str::<Value>(m).ok())
        .map(|m| schema_of(&m))
        .unwrap_or_else(pkg::empty_config_schema);
    Value::Object(pkg::with_defaults(&schema, &plain))
}

pub(crate) fn schema_of(manifest: &Value) -> Value {
    manifest
        .get("config_schema")
        .cloned()
        .unwrap_or_else(pkg::empty_config_schema)
}

/// `POST /internal/v1/marketplace/builds` with `build_id`: the build plane answers a publisher's
/// build request. The release is recorded under the publisher, authored by the person who asked
/// (so the conflict rule sees them), with an open review case and its intake stage.
pub(crate) fn third_party_build_in(
    tx: &Transaction<'_>,
    b: &BuildReq,
    now: i64,
) -> GResult<(Value, Vec<String>)> {
    let build = b.build_id.clone().unwrap_or_default();
    let row: Option<(String, String, String, String, String, String)> = tx
        .query_row(
            "SELECT b.publisher_id, b.template_id, b.version, b.status, b.requested_by, t.slug
               FROM nest_builds b JOIN nest_templates t ON t.id = b.template_id WHERE b.id = ?1",
            [&build],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((publisher, template, version, status, author, slug)) = row else {
        return Err(not_found());
    };
    if status != "queued" {
        return Err(conflict_code(
            "build_not_queued",
            "this build request is not queued",
        ));
    }
    let publisher_status: String = tx.query_row(
        "SELECT status FROM publisher_profiles WHERE id = ?1",
        [&publisher],
        |r| r.get(0),
    )?;
    if publisher_status != "approved" {
        return Err(conflict_code(
            "publisher_not_certified",
            "the publisher is not certified",
        ));
    }
    let m = &b.manifest;
    if m["name"] != slug.as_str() || m["version"] != version.as_str() {
        return Err(GErr::bad(
            "invalid_manifest",
            "the manifest's name and version are the build request's template and version",
        ));
    }
    let address = m["listing_address"].as_str().unwrap_or("");
    if address.starts_with("/pigeonpost/") {
        return Err(GErr::bad(
            "invalid_manifest",
            "listing addresses under /pigeonpost/ are Pigeonpost's own",
        ));
    }
    let held: Option<String> = tx
        .query_row(
            "SELECT publisher_id FROM nest_listings WHERE address = ?1",
            [address],
            |r| r.get(0),
        )
        .optional()?;
    if held.is_some_and(|p| p != publisher) {
        return Err(conflict_code(
            "listing_address_taken",
            "another publisher holds this address",
        ));
    }
    let release = format!("rel_{}", crate::groups::rand_b32(16));
    tx.execute(
        "INSERT INTO nest_releases (id, template_id, publisher_id, version, image_digest, status,
            created_by, created_at, release_digest, source_digest, vendor_digest, manifest_digest,
            sbom_digest, sbom, manifest, image, build_isolation)
         VALUES (?1, ?2, ?3, ?4, ?5, 'submitted', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            release,
            template,
            publisher,
            version,
            b.image_digest,
            author,
            now,
            b.release_digest,
            b.source_digest,
            b.vendor_digest,
            b.manifest_digest,
            b.sbom_digest,
            b.sbom,
            b.manifest.to_string(),
            b.image,
            b.isolation.to_string()
        ],
    )?;
    let previous: Option<String> = tx
        .query_row(
            "SELECT c.id FROM review_cases c JOIN nest_releases r ON r.id = c.release_id
              WHERE r.template_id = ?1 AND r.id <> ?2 ORDER BY c.created_at DESC, c.rowid DESC LIMIT 1",
            params![template, release],
            |r| r.get(0),
        )
        .optional()?;
    let case = format!("rvw_{}", crate::groups::rand_b32(16));
    tx.execute(
        "INSERT INTO review_cases (id, release_id, publisher_id, status, previous_id, created_at,
            updated_at, build_id)
         VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?5, ?6)",
        params![case, release, publisher, previous, now, build],
    )?;
    let evidence = crate::marketplace_m1::canonical_json(&json!({
        "stage": "intake",
        "decision": "passed",
        "isolation": b.isolation,
        "release_digest": b.release_digest,
        "source_digest": b.source_digest,
        "vendor_digest": b.vendor_digest,
        "sbom_digest": b.sbom_digest,
        "image_digest": b.image_digest,
        "build_id": build,
    }));
    tx.execute(
        "INSERT INTO review_decisions (id, case_id, reviewer, decision, reason, created_at, stage,
            evidence, evidence_digest)
         VALUES (?1, ?2, 'system:build-plane', 'passed', 'built by Pigeonpost from the requested commit',
            ?3, 'intake', ?4, ?5)",
        params![
            format!("dec_{}", crate::groups::rand_b32(16)),
            case,
            now,
            evidence,
            digest_of(evidence.as_bytes())
        ],
    )?;
    tx.execute(
        "UPDATE nest_builds SET status = 'built', release_id = ?2, review_id = ?3, updated_at = ?4
          WHERE id = ?1",
        params![build, release, case, now],
    )?;
    let wake = store::publisher_event_in(
        tx,
        &publisher,
        "nest.review.stage_changed",
        &json!({ "review_id": case, "release_id": release, "stage": "intake",
                 "status": "passed", "revision": 1, "build_id": build }),
        now as u64,
    )?;
    Ok((
        json!({ "release_id": release, "review_id": case, "publisher_id": publisher,
                "build_id": build, "listing_id": null }),
        wake,
    ))
}

// ---- small helpers -----------------------------------------------------------------------------------

pub(crate) fn wake(state: &AppState, accounts: Vec<String>) {
    for a in accounts {
        state.event_hub.wake(&a);
    }
}

pub(crate) fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", crate::groups::rand_b32(16))
}

pub(crate) fn text(v: &str, field: &str, min: usize, max: usize) -> GResult<String> {
    let t = v.trim().to_string();
    let n = t.chars().count();
    if n < min || n > max || t.chars().any(char::is_control) {
        return Err(GErr::bad(
            "invalid_request",
            format!("{field}: {min}-{max} printable characters"),
        ));
    }
    Ok(t)
}

pub(crate) fn reason_of(v: &str) -> GResult<String> {
    text(v, "reason", 1, 2000)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn audit_row(
    state: &AppState,
    principal: &Principal,
    account: &str,
    action: &str,
    target: String,
    details: Value,
    client_ip: &Option<Extension<ClientIp>>,
    headers: &HeaderMap,
) -> crate::store::audit::AuditRow {
    state.auditor.row(
        crate::audit::ActorKind::of(principal),
        Some(account.to_string()),
        action,
        Some(target),
        details,
        ip_of(client_ip),
        user_agent(headers),
        now_unix(),
    )
}

/// A release, as the M2 routes decide on it.
#[derive(Debug, Clone)]
pub(crate) struct Rel {
    pub(crate) id: String,
    pub(crate) template_id: String,
    pub(crate) publisher_id: String,
    pub(crate) version: String,
    pub(crate) status: String,
    pub(crate) security_state: String,
    pub(crate) release_digest: Option<String>,
    pub(crate) image: Option<String>,
    pub(crate) manifest: Value,
}

pub(crate) fn release_in(c: &Connection, id: &str) -> rusqlite::Result<Option<Rel>> {
    c.query_row(
        "SELECT id, template_id, publisher_id, version, status, security_state, release_digest,
                image, manifest FROM nest_releases WHERE id = ?1",
        [id],
        |r| {
            Ok(Rel {
                id: r.get(0)?,
                template_id: r.get(1)?,
                publisher_id: r.get(2)?,
                version: r.get(3)?,
                status: r.get(4)?,
                security_state: r.get(5)?,
                release_digest: r.get(6)?,
                image: r.get(7)?,
                manifest: r
                    .get::<_, Option<String>>(8)?
                    .and_then(|t| serde_json::from_str(&t).ok())
                    .unwrap_or(Value::Null),
            })
        },
    )
    .optional()
}

/// Approved, live, and carrying a staff attestation that still matches its digest: the only kind
/// of release a listing may offer or an installation may run.
pub(crate) fn live_attested_in(c: &Connection, r: &Rel) -> rusqlite::Result<bool> {
    if r.status != "approved" || r.security_state != "normal" {
        return Ok(false);
    }
    let (Some(digest), Some(image)) = (&r.release_digest, &r.image) else {
        return Ok(false);
    };
    let Some((statement, statement_digest)) = attestation_in(c, &r.id)? else {
        return Ok(false);
    };
    Ok(digest_of(statement.as_bytes()) == statement_digest
        && serde_json::from_str::<Value>(&statement).is_ok_and(|s| {
            s["type"] == STATEMENT_TYPE
                && s["release_id"] == r.id.as_str()
                && s["release_digest"] == digest.as_str()
                && s["image"] == image.as_str()
                && s["decision"] == "approved"
        }))
}

/// Why a release cannot be run, as an error.
pub(crate) fn not_runnable(r: &Rel) -> GErr {
    match r.security_state.as_str() {
        "revoked" => conflict_code("release_revoked", "this release is revoked"),
        "quarantined" => conflict_code("security_quarantined", "this release is quarantined"),
        _ => conflict_code(
            "review_required",
            "this release has no valid staff attestation",
        ),
    }
}

// ---- publishers and who acts for them -----------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct Pub {
    pub(crate) id: String,
    pub(crate) owner_kind: String,
    pub(crate) owner_id: String,
    pub(crate) display_name: String,
    pub(crate) status: String,
    pub(crate) decision_reason: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

impl Pub {
    fn json(&self) -> Value {
        json!({
            "id": self.id,
            "owner_kind": self.owner_kind,
            "owner_id": self.owner_id,
            "display_name": self.display_name,
            "status": self.status,
            "decision_reason": self.decision_reason,
            "created_at": iso(self.created_at),
            "updated_at": iso(self.updated_at),
        })
    }
}

const PUB_COLS: &str =
    "id, owner_kind, owner_id, display_name, status, decision_reason, created_at, updated_at";

fn map_pub(r: &rusqlite::Row<'_>) -> rusqlite::Result<Pub> {
    Ok(Pub {
        id: r.get(0)?,
        owner_kind: r.get(1)?,
        owner_id: r.get(2)?,
        display_name: r.get(3)?,
        status: r.get(4)?,
        decision_reason: r.get(5)?,
        created_at: r.get(6)?,
        updated_at: r.get(7)?,
    })
}

pub(crate) fn pub_in(c: &Connection, id: &str) -> rusqlite::Result<Option<Pub>> {
    c.query_row(
        &format!("SELECT {PUB_COLS} FROM publisher_profiles WHERE id = ?1"),
        [id],
        map_pub,
    )
    .optional()
}

/// What the caller may do for one publisher.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Access {
    /// See the publisher, its builds, reviews and listings.
    pub(crate) read: bool,
    /// Act for it: templates, builds, listings, publishing (with step-up).
    pub(crate) admin: bool,
    /// Hand out and take back its publisher roles (org publishers only).
    pub(crate) grant: bool,
    pub(crate) role: &'static str,
}

pub(crate) async fn access_of(
    state: &AppState,
    headers: &HeaderMap,
    account: &str,
    p: &Pub,
) -> Access {
    match p.owner_kind.as_str() {
        "account" if p.owner_id == account => Access {
            read: true,
            admin: true,
            grant: false,
            role: "owner",
        },
        "org" => {
            let Ok(ctx) = crate::org::org_ctx(state, headers, &p.owner_id).await else {
                return Access::default();
            };
            if permits(&ctx.roles, Perm::Security, ctx.rooms_everyone()) {
                return Access {
                    read: true,
                    admin: true,
                    grant: true,
                    role: "org_admin",
                };
            }
            let (pid, who) = (p.id.clone(), account.to_string());
            let roles: Vec<String> = state
                .store
                .group_read(move |c| -> GResult<Vec<String>> {
                    let mut stmt = c.prepare(
                        "SELECT role FROM publisher_roles
                          WHERE publisher_id = ?1 AND account_id = ?2 AND revoked_at IS NULL",
                    )?;
                    let rows = stmt
                        .query_map(params![pid, who], |r| r.get(0))?
                        .collect::<rusqlite::Result<Vec<String>>>()?;
                    Ok(rows)
                })
                .await
                .unwrap_or_default();
            if roles.iter().any(|r| r == "publisher_admin") {
                Access {
                    read: true,
                    admin: true,
                    grant: false,
                    role: "publisher_admin",
                }
            } else if roles.iter().any(|r| r == "publisher_finance") {
                Access {
                    read: true,
                    admin: false,
                    grant: false,
                    role: "publisher_finance",
                }
            } else {
                Access::default()
            }
        }
        _ => Access::default(),
    }
}

/// Whether the caller is on this publisher's team (M0's listing and application reads use it).
pub(crate) async fn publisher_read(
    state: &AppState,
    headers: &HeaderMap,
    account: &str,
    publisher: &str,
) -> bool {
    let id = publisher.to_string();
    let Ok(Some(p)) = state
        .store
        .group_read(move |c| -> GResult<Option<Pub>> { Ok(pub_in(c, &id)?) })
        .await
    else {
        return false;
    };
    access_of(state, headers, account, &p).await.read
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Need {
    Read,
    Admin,
    Grant,
}

pub(crate) struct Ctx {
    pub(crate) principal: Principal,
    pub(crate) account: String,
    pub(crate) p: Pub,
    pub(crate) access: Access,
}

/// The caller, their publisher and their access, authorized for `action`. Someone off the team
/// gets `404`, exactly as for a publisher that does not exist; a team member whose role does not
/// cover the action gets `403 publisher_role_required`.
pub(crate) async fn gate(
    state: &AppState,
    headers: &HeaderMap,
    publisher: &str,
    action: Action,
    need: Need,
) -> GResult<Ctx> {
    let (principal, account) = caller(state, headers).await?;
    let id = publisher.to_string();
    let p = state
        .store
        .group_read(move |c| -> GResult<Option<Pub>> { Ok(pub_in(c, &id)?) })
        .await?;
    let access = match &p {
        Some(p) => access_of(state, headers, &account, p).await,
        None => Access::default(),
    };
    let permitted = match need {
        Need::Read => access.read,
        Need::Admin => access.admin,
        Need::Grant => access.grant,
    };
    let f = Facts {
        publisher_permitted: permitted,
        ..facts(state)
    };
    authorize(
        &Actor::Principal(&principal),
        action,
        &Resource::Publisher(publisher),
        &f,
    )
    .map_err(|d| match d {
        DenyReason::NotPermitted if access.read => GErr::forbidden(
            "publisher_role_required",
            "your publisher role does not cover this action",
        ),
        other => deny(other, true),
    })?;
    let p = p.ok_or_else(not_found)?;
    Ok(Ctx {
        principal,
        account,
        p,
        access,
    })
}

/// Certified, and not revoked: what creating anything as a publisher needs.
pub(crate) fn certified(p: &Pub) -> GResult<()> {
    match p.status.as_str() {
        "approved" => Ok(()),
        "revoked" => Err(GErr::forbidden(
            "publisher_revoked",
            "this publisher's certification is revoked",
        )),
        _ => Err(GErr::forbidden(
            "publisher_not_certified",
            "this publisher is not certified yet",
        )),
    }
}

/// `GET /v1/publishers` — the publishers the caller acts for: their own, and those of the orgs
/// they administer or hold a publisher role in.
async fn list_publishers(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let result = async {
        let (principal, account) = caller(&state, &headers).await?;
        let who = account.clone();
        let candidates = state
            .store
            .group_read(move |c| -> GResult<Vec<Pub>> {
                let mut stmt = c.prepare(&format!(
                    "SELECT {PUB_COLS} FROM publisher_profiles
                      WHERE (owner_kind = 'account' AND owner_id = ?1)
                         OR (owner_kind = 'org' AND owner_id IN (
                               SELECT org_id FROM org_members
                                WHERE account_id = ?1 AND status = 'active'))
                      ORDER BY created_at, id LIMIT 200"
                ))?;
                let rows = stmt
                    .query_map([&who], map_pub)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await?;
        let f = Facts {
            publisher_permitted: true,
            ..facts(&state)
        };
        authorize(
            &Actor::Principal(&principal),
            Action::PublisherManage,
            &Resource::Account(&account),
            &f,
        )
        .map_err(|d| deny(d, false))?;
        let mut out = Vec::new();
        for p in candidates {
            let a = access_of(&state, &headers, &account, &p).await;
            if a.read {
                let mut v = p.json();
                v["role"] = json!(a.role);
                out.push(v);
            }
        }
        Ok(Json(json!({ "data": out, "next_cursor": null })))
    }
    .await;
    into_response(result)
}

/// `GET /v1/publishers/{publisher}`
async fn read_publisher(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(publisher): Path<String>,
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
        let mut v = ctx.p.json();
        v["role"] = json!(ctx.access.role);
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

// ---- org publisher roles ---------------------------------------------------------------------------

fn roles_in(c: &Connection, publisher: &str) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = c.prepare(
        "SELECT id, account_id, role, granted_by, granted_at FROM publisher_roles
          WHERE publisher_id = ?1 AND revoked_at IS NULL ORDER BY granted_at, id",
    )?;
    let rows = stmt
        .query_map([publisher], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "account_id": r.get::<_, String>(1)?,
                "role": r.get::<_, String>(2)?,
                "granted_by": r.get::<_, String>(3)?,
                "granted_at": iso(r.get(4)?),
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// `GET /v1/publishers/{publisher}/roles`
async fn list_roles(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(publisher): Path<String>,
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
        let id = ctx.p.id.clone();
        let rows = state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> { Ok(roles_in(c, &id)?) })
            .await?;
        Ok(Json(json!({ "data": rows, "next_cursor": null })))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantReq {
    /// The member's account, or (M2b) `member_id`, the org member id the org's member list shows.
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    member_id: Option<String>,
    role: String,
}

/// `POST /v1/publishers/{publisher}/roles` — an org owner or admin lets an active member act
/// for the org's publisher (`publisher_admin`) or see it (`publisher_finance`).
async fn grant_role(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path(publisher): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let ctx = gate(&state, &headers, &publisher, Action::PublisherManage, Need::Grant).await?;
        let mut req: GrantReq = parse(body)?;
        if !matches!(req.role.as_str(), "publisher_admin" | "publisher_finance") {
            return Err(GErr::bad(
                "invalid_role",
                "role must be publisher_admin or publisher_finance",
            ));
        }
        req.account_id = Some(
            crate::marketplace_listings::role_target(&state, &ctx.p.owner_id, &req.account_id, &req.member_id)
                .await?,
        );
        let now = now_unix() as i64;
        let id = new_id("prl");
        let audit = audit_row(
            &state,
            &ctx.principal,
            &ctx.account,
            "marketplace.publisher.role.grant",
            format!("publisher:{}", ctx.p.id),
            json!({ "account_id": req.account_id, "role": req.role }),
            &client_ip,
            &headers,
        );
        let (rid, pid, org, by) = (id.clone(), ctx.p.id.clone(), ctx.p.owner_id.clone(), ctx.account.clone());
        let (target, role) = (req.account_id.clone().unwrap_or_default(), req.role.clone());
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                let member: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM org_members
                        WHERE org_id = ?1 AND account_id = ?2 AND status = 'active')",
                    params![org, target],
                    |r| r.get(0),
                )?;
                if !member {
                    return Err(GErr::bad(
                        "not_org_member",
                        "a publisher role goes to an active member of the org",
                    ));
                }
                let held: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM publisher_roles WHERE publisher_id = ?1
                        AND account_id = ?2 AND role = ?3 AND revoked_at IS NULL)",
                    params![pid, target, role],
                    |r| r.get(0),
                )?;
                if held {
                    return Err(GErr::conflict(
                        "role_already_held",
                        "this member already holds this role",
                    ));
                }
                tx.execute(
                    "INSERT INTO publisher_roles (id, publisher_id, account_id, role, granted_by, granted_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![rid, pid, target, role, by, now],
                )?;
                crate::store::audit::append_in(tx, &audit)?;
                Ok(())
            })
            .await?;
        Ok((
            StatusCode::CREATED,
            Json(json!({ "data": {
                "id": id, "account_id": req.account_id, "role": req.role,
                "granted_by": ctx.account, "granted_at": iso(now),
            }})),
        ))
    }
    .await;
    into_response(result)
}

/// `DELETE /v1/publishers/{publisher}/roles/{role}`
async fn revoke_role(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path((publisher, role)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = gate(
            &state,
            &headers,
            &publisher,
            Action::PublisherManage,
            Need::Grant,
        )
        .await?;
        let now = now_unix() as i64;
        let audit = audit_row(
            &state,
            &ctx.principal,
            &ctx.account,
            "marketplace.publisher.role.revoke",
            format!("publisher_role:{role}"),
            json!({}),
            &client_ip,
            &headers,
        );
        let (pid, by) = (ctx.p.id.clone(), ctx.account.clone());
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                let n = tx.execute(
                    "UPDATE publisher_roles SET revoked_by = ?3, revoked_at = ?4
                      WHERE id = ?1 AND publisher_id = ?2 AND revoked_at IS NULL",
                    params![role, pid, by, now],
                )?;
                if n == 0 {
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

// ---- templates ---------------------------------------------------------------------------------------

fn template_json(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": r.get::<_, String>(0)?,
        "publisher_id": r.get::<_, String>(1)?,
        "slug": r.get::<_, String>(2)?,
        "name": r.get::<_, String>(3)?,
        "created_at": iso(r.get(4)?),
    }))
}

/// `GET /v1/publishers/{publisher}/templates`
async fn list_templates(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(publisher): Path<String>,
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
        let id = ctx.p.id.clone();
        let rows = state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> {
                let mut stmt = c.prepare(
                    "SELECT id, publisher_id, slug, name, created_at FROM nest_templates
                      WHERE publisher_id = ?1 ORDER BY created_at, id",
                )?;
                let rows = stmt
                    .query_map([&id], template_json)?
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
struct TemplateReq {
    slug: String,
    name: String,
}

/// `POST /v1/publishers/{publisher}/templates`
async fn create_template(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(publisher): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let ctx = gate(&state, &headers, &publisher, Action::PublisherManage, Need::Admin).await?;
        certified(&ctx.p)?;
        let req: TemplateReq = parse(body)?;
        if !pkg::slug_ok(&req.slug) {
            return Err(GErr::bad("invalid_request", "slug: 1-64 of a-z 0-9 -"));
        }
        let name = text(&req.name, "name", 2, 80)?;
        let now = now_unix() as i64;
        let id = new_id("tpl");
        let (tid, pid, slug, by) = (id.clone(), ctx.p.id.clone(), req.slug.clone(), ctx.account.clone());
        let n = name.clone();
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM nest_templates WHERE publisher_id = ?1 AND slug = ?2)",
                    params![pid, slug],
                    |r| r.get(0),
                )?;
                if exists {
                    return Err(GErr::conflict("template_exists", "this slug is taken"));
                }
                tx.execute(
                    "INSERT INTO nest_templates (id, publisher_id, slug, name, created_by, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![tid, pid, slug, n, by, now],
                )?;
                Ok(())
            })
            .await?;
        Ok((
            StatusCode::CREATED,
            Json(json!({ "data": {
                "id": id, "publisher_id": ctx.p.id, "slug": req.slug, "name": name,
                "created_at": iso(now),
            }})),
        ))
    }
    .await;
    into_response(result)
}

// ---- builds ------------------------------------------------------------------------------------------

const BUILD_COLS: &str =
    "id, publisher_id, template_id, version, source_repository, source_commit, \
     status, release_id, review_id, failure_code, failure_reason, created_at, updated_at, source_id";

fn build_json(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let failure_code: Option<String> = r.get(9)?;
    Ok(json!({
        "id": r.get::<_, String>(0)?,
        "publisher_id": r.get::<_, String>(1)?,
        "template_id": r.get::<_, String>(2)?,
        "version": r.get::<_, String>(3)?,
        "source": { "repository": r.get::<_, String>(4)?, "commit": r.get::<_, String>(5)?,
                    "source_id": r.get::<_, Option<String>>(13)? },
        "status": r.get::<_, String>(6)?,
        "release_id": r.get::<_, Option<String>>(7)?,
        "review_id": r.get::<_, Option<String>>(8)?,
        "failure": failure_code.map(|code| json!({ "code": code, "reason": r.get::<_, Option<String>>(10).ok().flatten() })),
        "created_at": iso(r.get(11)?),
        "updated_at": iso(r.get(12)?),
    }))
}

/// A source repository the build plane can fetch read-only: `https://`, a host, a path, no
/// credentials, no query.
pub(crate) fn repository_ok(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split('/').next().unwrap_or("");
    url.len() <= 300
        && !host.is_empty()
        && !host.contains('@')
        && !rest.contains('?')
        && !rest.contains('#')
        && rest.contains('/')
        && url.chars().all(|c| c.is_ascii_graphic())
}

fn commit_ok(c: &str) -> bool {
    (c.len() == 40 || c.len() == 64)
        && c.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// `GET /v1/publishers/{publisher}/builds`
async fn list_builds(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(publisher): Path<String>,
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
        let id = ctx.p.id.clone();
        let rows = state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> {
                let mut stmt = c.prepare(&format!(
                    "SELECT {BUILD_COLS} FROM nest_builds WHERE publisher_id = ?1
                      ORDER BY created_at DESC, id LIMIT {PAGE}"
                ))?;
                let rows = stmt
                    .query_map([&id], build_json)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await?;
        Ok(Json(json!({ "data": rows, "next_cursor": null })))
    }
    .await;
    into_response(result)
}

/// `GET /v1/publishers/{publisher}/builds/{build}`
async fn read_build(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((publisher, build)): Path<(String, String)>,
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
        let id = ctx.p.id.clone();
        let row = state
            .store
            .group_read(move |c| -> GResult<Option<Value>> {
                Ok(c.query_row(
                    &format!(
                        "SELECT {BUILD_COLS} FROM nest_builds WHERE id = ?1 AND publisher_id = ?2"
                    ),
                    params![build, id],
                    build_json,
                )
                .optional()?)
            })
            .await?;
        Ok(Json(json!({ "data": row.ok_or_else(not_found)? })))
    }
    .await;
    into_response(result)
}

/// The source of a build: a public https repository named inline, or a registered source
/// connection (M2b, `marketplace_sources`) by id. Either way the host must be on the allowlist.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceReq {
    #[serde(default)]
    repository: Option<String>,
    #[serde(default)]
    source_id: Option<String>,
    commit: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildRequest {
    template_id: String,
    version: String,
    source: SourceReq,
}

/// `POST /v1/publishers/{publisher}/builds` — ask Pigeonpost to build a release from a pinned
/// commit (R-62: no image-only production). The build plane picks it up; its review follows.
async fn request_build(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(publisher): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let ctx = gate(&state, &headers, &publisher, Action::PublisherManage, Need::Admin).await?;
        certified(&ctx.p)?;
        let req: BuildRequest = parse(body)?;
        if !pkg::semver_ok(&req.version) {
            return Err(GErr::bad("invalid_request", "version: major.minor.patch"));
        }
        let inline = match (&req.source.repository, &req.source.source_id) {
            (Some(repo), None) => {
                if !repository_ok(repo) {
                    return Err(GErr::bad(
                        "invalid_request",
                        "source.repository: an https URL with no credentials",
                    ));
                }
                crate::marketplace_sources::host_allowed(&state, repo)?;
                Some(repo.clone())
            }
            (None, Some(_)) => None,
            _ => {
                return Err(GErr::bad(
                    "invalid_request",
                    "source: either repository or source_id",
                ))
            }
        };
        if !commit_ok(&req.source.commit) {
            return Err(GErr::bad(
                "invalid_request",
                "source.commit: a full lower-case commit hash",
            ));
        }
        let now = now_unix() as i64;
        let id = new_id("bld");
        let (bid, pid, by) = (id.clone(), ctx.p.id.clone(), ctx.account.clone());
        let row = state
            .store
            .group_tx(move |tx| -> GResult<Value> {
                let owner: Option<String> = tx
                    .query_row(
                        "SELECT publisher_id FROM nest_templates WHERE id = ?1",
                        [&req.template_id],
                        |r| r.get(0),
                    )
                    .optional()?;
                if owner.as_deref() != Some(pid.as_str()) {
                    return Err(GErr::new(StatusCode::NOT_FOUND, "not_found", "no such template"));
                }
                let taken: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM nest_builds WHERE template_id = ?1 AND version = ?2)
                         OR EXISTS (SELECT 1 FROM nest_releases WHERE template_id = ?1 AND version = ?2)",
                    params![req.template_id, req.version],
                    |r| r.get(0),
                )?;
                if taken {
                    return Err(GErr::conflict(
                        "version_exists",
                        "this template already has this version",
                    ));
                }
                let repository = match (&inline, &req.source.source_id) {
                    (Some(r), _) => r.clone(),
                    (None, Some(sid)) => crate::marketplace_sources::active_repository_in(tx, &pid, sid)?
                        .ok_or_else(|| GErr::new(StatusCode::NOT_FOUND, "not_found", "no such source"))?,
                    (None, None) => return Err(not_found()),
                };
                tx.execute(
                    "INSERT INTO nest_builds (id, publisher_id, template_id, version, source_repository,
                        source_commit, status, requested_by, created_at, updated_at, source_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'queued', ?7, ?8, ?8, ?9)",
                    params![
                        bid,
                        pid,
                        req.template_id,
                        req.version,
                        repository,
                        req.source.commit,
                        by,
                        now,
                        req.source.source_id
                    ],
                )?;
                Ok(tx.query_row(
                    &format!("SELECT {BUILD_COLS} FROM nest_builds WHERE id = ?1"),
                    [&bid],
                    build_json,
                )?)
            })
            .await?;
        Ok((StatusCode::CREATED, Json(json!({ "data": row }))))
    }
    .await;
    into_response(result)
}

/// `GET /internal/v1/marketplace/build-requests` — queued build requests, oldest first, for the
/// build plane.
async fn pending_builds(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = service_gate(&state, &headers) {
        return *r;
    }
    let result = async {
        flag_gate(&state).await?;
        let rows = state
            .store
            .group_read(|c| -> GResult<Vec<Value>> {
                let mut stmt = c.prepare(&format!(
                    "SELECT b.id, b.publisher_id, b.template_id, b.version, b.source_repository,
                            b.source_commit, b.status, b.release_id, b.review_id, b.failure_code,
                            b.failure_reason, b.created_at, b.updated_at, b.source_id, t.slug,
                            (SELECT s.kind FROM publisher_sources s WHERE s.id = b.source_id)
                       FROM nest_builds b JOIN nest_templates t ON t.id = b.template_id
                       JOIN publisher_profiles p ON p.id = b.publisher_id
                      WHERE b.status = 'queued' AND p.status = 'approved'
                      ORDER BY b.created_at, b.id LIMIT {PAGE}"
                ))?;
                let rows = stmt
                    .query_map([], |r| {
                        let mut v = build_json(r)?;
                        v["template_slug"] = json!(r.get::<_, String>(14)?);
                        // M2b: a deploy-key source's key is fetched from
                        // `…/build-requests/{build}/source-credential`.
                        v["source"]["kind"] = json!(r
                            .get::<_, Option<String>>(15)?
                            .unwrap_or_else(|| "https".into()));
                        Ok(v)
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
struct FailureReq {
    code: String,
    /// What the publisher reads. The build plane redacts it: no host paths, no internal addresses.
    reason: String,
}

/// `POST /internal/v1/marketplace/build-requests/{build}/failure`
async fn build_failed(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(build): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    if let Err(r) = service_gate(&state, &headers) {
        return *r;
    }
    let result = async {
        flag_gate(&state).await?;
        let req: FailureReq = parse(body)?;
        if !pkg::field_name_ok(&req.code) {
            return Err(GErr::bad("invalid_request", "code: [a-z][a-z0-9_]*"));
        }
        let reason = reason_of(&req.reason)?;
        let now = now_unix() as i64;
        let wake_list = state
            .store
            .group_tx(move |tx| -> GResult<Vec<String>> {
                let row: Option<(String, String)> = tx
                    .query_row(
                        "SELECT publisher_id, status FROM nest_builds WHERE id = ?1",
                        [&build],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                let Some((publisher, status)) = row else {
                    return Err(not_found());
                };
                if status != "queued" {
                    return Err(conflict_code("build_not_queued", "this build request is not queued"));
                }
                tx.execute(
                    "UPDATE nest_builds SET status = 'failed', failure_code = ?2, failure_reason = ?3,
                        updated_at = ?4 WHERE id = ?1",
                    params![build, req.code, reason, now],
                )?;
                Ok(store::publisher_event_in(
                    tx,
                    &publisher,
                    "nest.build.failed",
                    &json!({ "build_id": build, "code": req.code }),
                    now as u64,
                )?)
            })
            .await?;
        wake(&state, wake_list);
        Ok(Json(json!({ "data": { "status": "failed" } })))
    }
    .await;
    into_response(result)
}

// ---- reviews, the publisher's side ---------------------------------------------------------------

type DecisionRow = (String, String, String, String, Option<String>, i64);

fn decisions_in(c: &Connection, case: &str) -> rusqlite::Result<Vec<DecisionRow>> {
    let mut stmt = c.prepare(
        "SELECT id, stage, decision, reason, evidence_digest, created_at FROM review_decisions
          WHERE case_id = ?1 ORDER BY created_at, rowid",
    )?;
    let rows = stmt
        .query_map([case], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The contract's `NestReviewStatus`: a redacted projection for the publisher's team.
#[allow(clippy::type_complexity)]
fn review_status_in(
    c: &Connection,
    case: &str,
    publisher: &str,
) -> rusqlite::Result<Option<Value>> {
    let row: Option<(
        String,
        String,
        Option<String>,
        i64,
        i64,
        Option<String>,
        String,
        String,
    )> = c
        .query_row(
            "SELECT c.status, c.release_id, c.previous_id, c.revision, c.updated_at,
                    r.release_digest, r.security_state, p.status
               FROM review_cases c JOIN nest_releases r ON r.id = c.release_id
               JOIN publisher_profiles p ON p.id = c.publisher_id
              WHERE c.id = ?1 AND c.publisher_id = ?2",
            params![case, publisher],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .optional()?;
    let Some((case_status, release, previous, revision, updated, digest, security, pub_status)) =
        row
    else {
        return Ok(None);
    };
    let rows = decisions_in(c, case)?;
    let mut stages = Vec::new();
    for name in STAGES {
        let mine: Vec<&DecisionRow> = rows.iter().filter(|r| r.1 == name).collect();
        let stage = match mine.last() {
            None => json!({
                "name": name, "status": "pending", "attempt": 0, "started_at": null,
                "finished_at": null, "blocker_code": null,
            }),
            Some(d) => {
                let (status, blocker) = match (name, d.2.as_str()) {
                    (_, "passed" | "advisory") | ("human_review", "approved") => ("passed", None),
                    ("human_review", "changes_requested") => ("blocked", Some("changes_requested")),
                    ("human_review", "rejected") => ("failed", Some("rejected")),
                    _ => ("failed", Some("stage_failed")),
                };
                let mut v = json!({
                    "name": name, "status": status, "attempt": mine.len(),
                    "started_at": iso(d.5), "finished_at": iso(d.5), "blocker_code": blocker,
                });
                let reason: String = d.3.chars().take(2000).collect();
                if !reason.trim().is_empty() {
                    v["reason"] = json!(reason);
                } else if blocker.is_some() {
                    v["reason"] = json!("see the findings");
                }
                if let Some(e) = &d.4 {
                    v["evidence_ref"] = json!(e);
                }
                v
            }
        };
        stages.push(stage);
    }
    let human = rows.iter().rev().find(|r| r.1 == "human_review");
    let failed = rows.iter().find(|r| r.2 == "failed");
    let status = match (case_status.as_str(), human) {
        ("withdrawn", _) => "withdrawn",
        (_, Some(h)) => match h.2.as_str() {
            "approved" => "approved",
            "rejected" => "rejected",
            _ => "changes_requested",
        },
        ("blocked", None) => "changes_requested",
        _ if rows.iter().any(|r| r.1 != "intake") => "in_review",
        _ => "submitted",
    };
    let findings_ref = format!("/v1/publishers/{publisher}/reviews/{case}/findings");
    let mut out = json!({
        "review_id": case,
        "publisher_id": publisher,
        "release_id": release,
        "artifact_digest": digest.unwrap_or_default(),
        "policy_digest": digest_of(REVIEW_POLICY.as_bytes()),
        "revision": revision,
        "status": status,
        "stages": stages,
        "findings_ref": findings_ref,
        "updated_at": iso(updated),
    });
    if let Some(p) = previous {
        out["previous_review_id"] = json!(p);
    }
    if let Some(h) = human {
        out["decision_ref"] = json!(h.0);
    } else if let Some(f) = failed {
        out["decision_ref"] = json!(f.0);
    }
    if status == "approved" {
        let att: Option<String> = c
            .query_row(
                "SELECT id FROM release_attestations WHERE case_id = ?1
                  ORDER BY created_at DESC, id LIMIT 1",
                [case],
                |r| r.get(0),
            )
            .optional()?;
        out["approval_ref"] = json!(att.unwrap_or_default());
    }
    let mut actions = vec!["view_findings"];
    if matches!(status, "changes_requested" | "rejected") {
        actions.push("resubmit");
    }
    if matches!(status, "submitted" | "in_review" | "changes_requested") && case_status != "decided"
    {
        actions.push("withdraw");
    }
    if status == "approved" && security == "normal" && pub_status == "approved" {
        actions.push("publish_listing");
    }
    out["allowed_actions"] = json!(actions);
    Ok(Some(out))
}

/// `GET /v1/publishers/{publisher}/reviews`
async fn list_reviews(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(publisher): Path<String>,
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
        let id = ctx.p.id.clone();
        let rows = state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> {
                let ids: Vec<String> = {
                    let mut stmt = c.prepare(&format!(
                        "SELECT id FROM review_cases WHERE publisher_id = ?1
                          ORDER BY created_at DESC, rowid DESC LIMIT {PAGE}"
                    ))?;
                    let rows = stmt
                        .query_map([&id], |r| r.get(0))?
                        .collect::<rusqlite::Result<Vec<String>>>()?;
                    rows
                };
                let mut out = Vec::new();
                for case in ids {
                    if let Some(v) = review_status_in(c, &case, &id)? {
                        out.push(v);
                    }
                }
                Ok(out)
            })
            .await?;
        Ok(Json(json!({ "data": rows, "next_cursor": null })))
    }
    .await;
    into_response(result)
}

/// `GET /v1/publishers/{publisher}/reviews/{review}` (contract `getNestReview`).
async fn read_review(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((publisher, review)): Path<(String, String)>,
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
        let id = ctx.p.id.clone();
        let v = state
            .store
            .group_read(move |c| -> GResult<Option<Value>> {
                Ok(review_status_in(c, &review, &id)?)
            })
            .await?;
        Ok(Json(json!({ "data": v.ok_or_else(not_found)? })))
    }
    .await;
    into_response(result)
}

/// The publisher's view of one stage's evidence. Their own scan findings are theirs to see in
/// full; the behaviour test shows only which rules fired (the honeytokens and capture setup stay
/// with Pigeonpost); the AI review is advisory evidence for staff and shows only a count.
fn redact(stage: &str, evidence: &Value) -> Value {
    let findings = evidence["findings"].as_array().cloned().unwrap_or_default();
    match stage {
        "deterministic_scan" => json!(findings
            .iter()
            .map(|f| json!({
                "rule": f["rule"], "path": f["path"], "detail": f["detail"], "blocker": f["blocker"],
            }))
            .collect::<Vec<_>>()),
        "behavioral_test" => json!(findings
            .iter()
            .map(|f| json!({ "rule": f["rule"], "blocker": f["blocker"] }))
            .collect::<Vec<_>>()),
        _ => json!([]),
    }
}

/// `GET /v1/publishers/{publisher}/reviews/{review}/findings` — redacted findings per stage.
async fn read_findings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((publisher, review)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = gate(&state, &headers, &publisher, Action::PublisherManage, Need::Read).await?;
        let id = ctx.p.id.clone();
        let v = state
            .store
            .group_read(move |c| -> GResult<Option<Value>> {
                let mine: bool = c.query_row(
                    "SELECT EXISTS (SELECT 1 FROM review_cases WHERE id = ?1 AND publisher_id = ?2)",
                    params![review, id],
                    |r| r.get(0),
                )?;
                if !mine {
                    return Ok(None);
                }
                let mut stmt = c.prepare(
                    "SELECT stage, decision, reason, evidence, created_at FROM review_decisions
                      WHERE case_id = ?1 ORDER BY created_at, rowid",
                )?;
                let rows = stmt
                    .query_map([&review], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, Option<String>>(3)?,
                            r.get::<_, i64>(4)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let stages: Vec<Value> = rows
                    .iter()
                    .map(|(stage, decision, reason, evidence, at)| {
                        let ev: Value = evidence
                            .as_deref()
                            .and_then(|t| serde_json::from_str(t).ok())
                            .unwrap_or(Value::Null);
                        let mut v = json!({
                            "stage": stage,
                            "decision": decision,
                            "reason": reason,
                            "findings": redact(stage, &ev),
                            "recorded_at": iso(*at),
                        });
                        if stage == "ai_analysis" {
                            v["advisory_findings"] =
                                json!(ev["findings"].as_array().map(Vec::len).unwrap_or(0));
                            v["model"] = json!(AI_REVIEW_MODEL);
                        }
                        v
                    })
                    .collect();
                Ok(Some(json!({ "review_id": review, "stages": stages })))
            })
            .await?;
        Ok(Json(json!({ "data": v.ok_or_else(not_found)? })))
    }
    .await;
    into_response(result)
}

/// `POST /v1/publishers/{publisher}/reviews/{review}/withdraw` — the publisher takes a release
/// out of review. A decided review stays decided.
async fn withdraw_review(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((publisher, review)): Path<(String, String)>,
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
        let id = ctx.p.id.clone();
        let now = now_unix() as i64;
        let (v, wake_list) = state
            .store
            .group_tx(move |tx| -> GResult<(Value, Vec<String>)> {
                let status: Option<String> = tx
                    .query_row(
                        "SELECT status FROM review_cases WHERE id = ?1 AND publisher_id = ?2",
                        params![review, id],
                        |r| r.get(0),
                    )
                    .optional()?;
                match status.as_deref() {
                    None => return Err(not_found()),
                    Some("open" | "blocked") => {}
                    Some(_) => {
                        return Err(conflict_code(
                            "review_closed",
                            "this review is decided or withdrawn",
                        ))
                    }
                }
                tx.execute(
                    "UPDATE review_cases SET status = 'withdrawn', revision = revision + 1,
                        updated_at = ?2 WHERE id = ?1",
                    params![review, now],
                )?;
                let w = store::publisher_event_in(
                    tx,
                    &id,
                    "nest.review.completed",
                    &json!({ "review_id": review, "decision": "withdrawn" }),
                    now as u64,
                )?;
                Ok((
                    review_status_in(tx, &review, &id)?.unwrap_or(Value::Null),
                    w,
                ))
            })
            .await?;
        wake(&state, wake_list);
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

// ---- listings ----------------------------------------------------------------------------------------

const LISTING_COLS: &str =
    "l.id, l.publisher_id, l.release_id, l.template_id, l.title, l.summary, \
     l.description, l.visibility, l.status, l.address, l.moderation_reason, l.created_at, \
     l.updated_at, l.published_at, r.version, l.price_currency, l.price_term";

fn listing_json(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": r.get::<_, String>(0)?,
        "publisher_id": r.get::<_, String>(1)?,
        "release_id": r.get::<_, String>(2)?,
        "template_id": r.get::<_, Option<String>>(3)?,
        "title": r.get::<_, String>(4)?,
        "summary": r.get::<_, String>(5)?,
        "description": r.get::<_, String>(6)?,
        "visibility": r.get::<_, String>(7)?,
        "status": r.get::<_, String>(8)?,
        "address": r.get::<_, Option<String>>(9)?,
        "moderation_reason": r.get::<_, Option<String>>(10)?,
        "created_at": iso(r.get(11)?),
        "updated_at": iso(r.get(12)?),
        "published_at": r.get::<_, Option<i64>>(13)?.map(iso),
        "version": r.get::<_, String>(14)?,
        "price": crate::marketplace_listings::price_json(r.get(15)?, r.get(16)?),
    }))
}

pub(crate) fn listing_in(
    c: &Connection,
    id: &str,
    publisher: &str,
) -> rusqlite::Result<Option<Value>> {
    c.query_row(
        &format!(
            "SELECT {LISTING_COLS} FROM nest_listings l JOIN nest_releases r ON r.id = l.release_id
              WHERE l.id = ?1 AND l.publisher_id = ?2"
        ),
        params![id, publisher],
        listing_json,
    )
    .optional()
}

/// The visibility classes. `invite_only` and `organization` listings are seen, installed and
/// invoked through their grants (`marketplace_grants`, M2b).
fn visibility_ok(v: &str) -> GResult<()> {
    match v {
        "public" | "unlisted" | "private" | "invite_only" | "organization" => Ok(()),
        _ => Err(GErr::bad(
            "invalid_request",
            "visibility: public, unlisted, private, invite_only or organization",
        )),
    }
}

/// `GET /v1/publishers/{publisher}/listings`
async fn list_listings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(publisher): Path<String>,
) -> Response {
    let result = async {
        let ctx = gate(&state, &headers, &publisher, Action::PublisherManage, Need::Read).await?;
        let id = ctx.p.id.clone();
        let rows = state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> {
                let mut stmt = c.prepare(&format!(
                    "SELECT {LISTING_COLS} FROM nest_listings l JOIN nest_releases r ON r.id = l.release_id
                      WHERE l.publisher_id = ?1 ORDER BY l.created_at, l.id"
                ))?;
                let rows = stmt
                    .query_map([&id], listing_json)?
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
struct ListingReq {
    release_id: String,
    title: String,
    summary: String,
    #[serde(default)]
    description: Option<String>,
    visibility: String,
    /// M2b: `{amount_minor: 0, currency, term}`.
    #[serde(default)]
    price: Option<crate::marketplace_listings::PriceReq>,
}

/// `POST /v1/publishers/{publisher}/listings` — a draft listing for one of the publisher's
/// releases. Its address is the release manifest's `listing_address`.
async fn create_listing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(publisher): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let ctx = gate(&state, &headers, &publisher, Action::PublisherManage, Need::Admin).await?;
        certified(&ctx.p)?;
        let req: ListingReq = parse(body)?;
        visibility_ok(&req.visibility)?;
        let (currency, term) = crate::marketplace_listings::price_terms(req.price.as_ref())?;
        let currency = currency.unwrap_or("TRY");
        let term = term.unwrap_or("one_off");
        let title = text(&req.title, "title", 2, 120)?;
        let summary = text(&req.summary, "summary", 2, 300)?;
        let description = match &req.description {
            Some(d) if d.chars().count() > 5000 => {
                return Err(GErr::bad("invalid_request", "description: at most 5000 characters"))
            }
            Some(d) => d.clone(),
            None => String::new(),
        };
        let now = now_unix() as i64;
        let id = new_id("lst");
        let (lid, pid) = (id.clone(), ctx.p.id.clone());
        let v = state
            .store
            .group_tx(move |tx| -> GResult<Value> {
                let rel = release_in(tx, &req.release_id)?
                    .filter(|r| r.publisher_id == pid)
                    .ok_or_else(|| GErr::new(StatusCode::NOT_FOUND, "not_found", "no such release"))?;
                let address = rel.manifest["listing_address"].as_str().unwrap_or("").to_string();
                let held: Option<String> = tx
                    .query_row(
                        "SELECT id FROM nest_listings WHERE address = ?1",
                        [&address],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(existing) = held {
                    return Err(GErr::conflict(
                        "listing_address_taken",
                        "a listing already serves this address",
                    )
                    .with("listing_id", existing));
                }
                tx.execute(
                    "INSERT INTO nest_listings (id, publisher_id, release_id, title, summary,
                        visibility, status, created_at, updated_at, address, description, template_id,
                        price_currency, price_term)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'draft', ?7, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        lid,
                        pid,
                        rel.id,
                        title,
                        summary,
                        req.visibility,
                        now,
                        address,
                        description,
                        rel.template_id,
                        currency,
                        term
                    ],
                )?;
                Ok(listing_in(tx, &lid, &pid)?.unwrap_or(Value::Null))
            })
            .await?;
        Ok((StatusCode::CREATED, Json(json!({ "data": v }))))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListingPatch {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    visibility: Option<String>,
    /// M2b: price terms; the amount stays zero.
    #[serde(default)]
    price: Option<crate::marketplace_listings::PriceReq>,
}

/// `PATCH /v1/publishers/{publisher}/listings/{listing}` — publisher-editable metadata only.
/// `status`, `release_id`, `address` and moderation are not fields here: publishing takes
/// `…/publish` (step-up), and moderation is staff's.
async fn edit_listing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((publisher, listing)): Path<(String, String)>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let ctx = gate(&state, &headers, &publisher, Action::PublisherManage, Need::Admin).await?;
        certified(&ctx.p)?;
        let req: ListingPatch = parse(body)?;
        let title = req.title.as_deref().map(|t| text(t, "title", 2, 120)).transpose()?;
        let summary = req.summary.as_deref().map(|t| text(t, "summary", 2, 300)).transpose()?;
        if req.description.as_ref().is_some_and(|d| d.chars().count() > 5000) {
            return Err(GErr::bad("invalid_request", "description: at most 5000 characters"));
        }
        if let Some(v) = &req.visibility {
            visibility_ok(v)?;
        }
        let (currency, term) = crate::marketplace_listings::price_terms(req.price.as_ref())?;
        let now = now_unix() as i64;
        let pid = ctx.p.id.clone();
        let (v, wake_list) = state
            .store
            .group_tx(move |tx| -> GResult<(Value, Vec<String>)> {
                if listing_in(tx, &listing, &pid)?.is_none() {
                    return Err(not_found());
                }
                tx.execute(
                    "UPDATE nest_listings SET title = COALESCE(?2, title), summary = COALESCE(?3, summary),
                        description = COALESCE(?4, description), visibility = COALESCE(?5, visibility),
                        updated_at = ?6, price_currency = COALESCE(?7, price_currency),
                        price_term = COALESCE(?8, price_term) WHERE id = ?1",
                    params![listing, title, summary, req.description, req.visibility, now, currency, term],
                )?;
                let v = listing_in(tx, &listing, &pid)?.unwrap_or(Value::Null);
                let w = store::publisher_event_in(
                    tx,
                    &pid,
                    "nest.listing.updated",
                    &json!({ "listing_id": listing, "status": v["status"], "visibility": v["visibility"] }),
                    now as u64,
                )?;
                Ok((v, w))
            })
            .await?;
        wake(&state, wake_list);
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PublishReq {
    /// Which attested release of the listing's template new installs get. Default: the
    /// listing's current release.
    #[serde(default)]
    release_id: Option<String>,
}

/// `POST /v1/publishers/{publisher}/listings/{listing}/publish` — make a listing live on an
/// attested release (R-65: only what staff approved), or move a live listing to a newer one.
/// Existing installations stay on their release until their owner updates.
async fn publish_listing(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path((publisher, listing)): Path<(String, String)>,
    body: Option<Json<Value>>,
) -> Response {
    let result = async {
        let ctx = gate(
            &state,
            &headers,
            &publisher,
            Action::NestPublish,
            Need::Admin,
        )
        .await?;
        certified(&ctx.p)?;
        let req: PublishReq = match body {
            Some(Json(v)) => serde_json::from_value(v)
                .map_err(|e| GErr::bad("invalid_request", e.to_string()))?,
            None => PublishReq::default(),
        };
        let now = now_unix() as i64;
        let audit = audit_row(
            &state,
            &ctx.principal,
            &ctx.account,
            "marketplace.listing.publish",
            format!("listing:{listing}"),
            json!({ "release_id": req.release_id }),
            &client_ip,
            &headers,
        );
        let pid = ctx.p.id.clone();
        let (v, wake_list) = state
            .store
            .group_tx(move |tx| -> GResult<(Value, Vec<String>)> {
                let row: Option<(String, String, Option<String>, Option<String>)> = tx
                    .query_row(
                        "SELECT release_id, status, template_id, address FROM nest_listings
                          WHERE id = ?1 AND publisher_id = ?2",
                        params![listing, pid],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )
                    .optional()?;
                let Some((current, status, template, address)) = row else {
                    return Err(not_found());
                };
                if status == "suspended" {
                    return Err(conflict_code(
                        "listing_suspended",
                        "staff suspended this listing; only staff can reinstate it",
                    ));
                }
                let target = req.release_id.unwrap_or(current);
                let rel = release_in(tx, &target)?
                    .filter(|r| r.publisher_id == pid && Some(&r.template_id) == template.as_ref())
                    .ok_or_else(|| {
                        GErr::new(StatusCode::NOT_FOUND, "not_found", "no such release")
                    })?;
                if !live_attested_in(tx, &rel)? {
                    return Err(not_runnable(&rel));
                }
                if rel.manifest["listing_address"].as_str() != address.as_deref() {
                    return Err(conflict_code(
                        "listing_address_changed",
                        "this release serves another address; give it its own listing",
                    ));
                }
                tx.execute(
                    "UPDATE nest_listings SET release_id = ?2, status = 'published',
                        published_at = COALESCE(published_at, ?3), updated_at = ?3 WHERE id = ?1",
                    params![listing, rel.id, now],
                )?;
                operations_in(tx, &listing, &rel.manifest)?;
                crate::store::audit::append_in(tx, &audit)?;
                let v = listing_in(tx, &listing, &pid)?.unwrap_or(Value::Null);
                let w = store::publisher_event_in(
                    tx,
                    &pid,
                    "nest.listing.updated",
                    &json!({ "listing_id": listing, "status": "published", "release_id": rel.id }),
                    now as u64,
                )?;
                Ok((v, w))
            })
            .await?;
        wake(&state, wake_list);
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

/// `POST /v1/publishers/{publisher}/listings/{listing}/withdraw` — take a listing off the
/// marketplace. Its installations stop taking jobs; they can still be read and uninstalled.
async fn withdraw_listing(
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
            Need::Admin,
        )
        .await?;
        let now = now_unix() as i64;
        let pid = ctx.p.id.clone();
        let (v, wake_list) = state
            .store
            .group_tx(move |tx| -> GResult<(Value, Vec<String>)> {
                let status: Option<String> = tx
                    .query_row(
                        "SELECT status FROM nest_listings WHERE id = ?1 AND publisher_id = ?2",
                        params![listing, pid],
                        |r| r.get(0),
                    )
                    .optional()?;
                match status.as_deref() {
                    None => return Err(not_found()),
                    Some("suspended") => {
                        return Err(conflict_code(
                            "listing_suspended",
                            "staff suspended this listing; only staff can change it",
                        ))
                    }
                    _ => {}
                }
                tx.execute(
                    "UPDATE nest_listings SET status = 'withdrawn', updated_at = ?2 WHERE id = ?1",
                    params![listing, now],
                )?;
                let v = listing_in(tx, &listing, &pid)?.unwrap_or(Value::Null);
                let w = store::publisher_event_in(
                    tx,
                    &pid,
                    "nest.listing.updated",
                    &json!({ "listing_id": listing, "status": "withdrawn" }),
                    now as u64,
                )?;
                Ok((v, w))
            })
            .await?;
        wake(&state, wake_list);
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

/// `GET /v1/marketplace/listings?q=&cursor=` — the catalog: published, public listings, and the
/// `invite_only` and `organization` listings whose grants let the caller discover them (M2b).
/// Unlisted listings are reachable by id, never searchable.
async fn catalog(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let result = async {
        let principal = match crate::bearer(&headers) {
            None => None,
            Some(t) => Some(crate::principal_for_token(&state, Some(t)).await?),
        };
        let account = principal
            .as_ref()
            .and_then(crate::capabilities::principal_account)
            .map(str::to_string);
        if !crate::filtering::feature_on(
            &state,
            account.as_deref(),
            crate::capabilities::MARKETPLACE,
        )
        .await
        {
            return Err(crate::marketplace::feature_off());
        }
        let actor = match &principal {
            Some(p) => Actor::Principal(p),
            None => Actor::Anonymous,
        };
        let f = Facts {
            listing_visible: true,
            ..facts(&state)
        };
        authorize(
            &actor,
            Action::NestDiscover,
            &Resource::NestListing("catalog"),
            &f,
        )
        .map_err(|d| deny(d, true))?;
        let term = q.get("q").map(|s| s.trim().to_string()).unwrap_or_default();
        if term.chars().count() > 100 {
            return Err(GErr::bad("invalid_request", "q: at most 100 characters"));
        }
        let offset: usize = match q.get("cursor") {
            None => 0,
            Some(c) => c
                .parse()
                .map_err(|_| GErr::bad("invalid_cursor", "not a cursor from this list"))?,
        };
        let pattern = format!(
            "%{}%",
            term.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        // Granted listings (M2b) for a signed-in caller; an anonymous one holds no grant.
        let viewer = account.clone().unwrap_or_default();
        let rows = state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> {
                let mut stmt = c.prepare(&format!(
                    "SELECT {LISTING_COLS}, p.display_name, p.owner_kind
                       FROM nest_listings l JOIN nest_releases r ON r.id = l.release_id
                       JOIN publisher_profiles p ON p.id = l.publisher_id
                      WHERE l.status = 'published'
                        AND (l.visibility = 'public' OR {granted})
                        AND r.security_state = 'normal'
                        AND p.status = 'approved'
                        AND (l.title LIKE ?1 ESCAPE '\\' OR l.summary LIKE ?1 ESCAPE '\\')
                      ORDER BY l.published_at DESC, l.id LIMIT ?2 OFFSET ?3",
                    granted = crate::marketplace_grants::DISCOVER_SQL
                ))?;
                let rows = stmt
                    .query_map(
                        params![pattern, (PAGE + 1) as i64, offset as i64, viewer],
                        |r| {
                            let mut v = listing_json(r)?;
                            v["publisher"] = json!({
                                "display_name": r.get::<_, String>(17)?,
                                "first_party": r.get::<_, String>(18)? == "platform",
                            });
                            if let Some(o) = v.as_object_mut() {
                                o.remove("moderation_reason");
                            }
                            Ok(v)
                        },
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await?;
        let mut rows = rows;
        let next = (rows.len() > PAGE).then(|| (offset + PAGE).to_string());
        rows.truncate(PAGE);
        Ok(Json(json!({ "data": rows, "next_cursor": next })))
    }
    .await;
    into_response(result)
}

// ---- installations -----------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct Inst {
    pub(crate) id: String,
    pub(crate) listing_id: String,
    /// `account` or `org` (M2b).
    pub(crate) owner_kind: String,
    pub(crate) owner_id: String,
    pub(crate) buyer_context: String,
    pub(crate) release_id: String,
    pub(crate) previous_release_id: Option<String>,
    pub(crate) config: String,
    pub(crate) status: String,
    /// An org installation's agent mailbox (M2b).
    pub(crate) agent_address: Option<String>,
}

pub(crate) fn inst_in(c: &Connection, id: &str) -> rusqlite::Result<Option<Inst>> {
    c.query_row(
        "SELECT id, listing_id, owner_id, buyer_context, release_id, previous_release_id, config,
                status, owner_kind, agent_address FROM nest_installations WHERE id = ?1",
        [id],
        |r| {
            Ok(Inst {
                id: r.get(0)?,
                listing_id: r.get(1)?,
                owner_id: r.get(2)?,
                buyer_context: r.get(3)?,
                release_id: r.get(4)?,
                previous_release_id: r.get(5)?,
                config: r.get(6)?,
                status: r.get(7)?,
                owner_kind: r.get(8)?,
                agent_address: r.get(9)?,
            })
        },
    )
    .optional()
}

pub(crate) fn secret_names_in(
    c: &Connection,
    installation: &str,
) -> rusqlite::Result<BTreeSet<String>> {
    let mut stmt =
        c.prepare("SELECT name FROM nest_installation_secrets WHERE installation_id = ?1")?;
    let rows = stmt
        .query_map([installation], |r| r.get(0))?
        .collect::<rusqlite::Result<BTreeSet<String>>>()?;
    Ok(rows)
}

/// An installation as its owner sees it: plain configuration, which secrets are set (never their
/// values), and whether the listing offers a newer attested release.
#[allow(clippy::type_complexity)]
pub(crate) fn inst_json_in(c: &Connection, id: &str) -> rusqlite::Result<Value> {
    let row: (
        String,
        String,
        String,
        Option<String>,
        String,
        i64,
        String,
        String,
        i64,
        i64,
        Option<i64>,
    ) = c.query_row(
        "SELECT i.id, i.listing_id, i.release_id, i.previous_release_id, i.config,
                    i.config_revision, i.status, i.funding, i.created_at, i.updated_at,
                    i.uninstalled_at
               FROM nest_installations i WHERE i.id = ?1",
        [id],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
                r.get(10)?,
            ))
        },
    )?;
    let rel = release_in(c, &row.2)?;
    let schema = rel
        .as_ref()
        .map(|r| schema_of(&r.manifest))
        .unwrap_or_else(pkg::empty_config_schema);
    let set = secret_names_in(c, id)?;
    let secrets: Vec<Value> = pkg::secret_fields(&schema)
        .into_iter()
        .map(|n| json!({ "name": n, "set": set.contains(&n) }))
        .collect();
    let (listing_release, listing_status, address): (String, String, Option<String>) = c
        .query_row(
            "SELECT release_id, status, address FROM nest_listings WHERE id = ?1",
            [&row.1],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
    let mut update = Value::Null;
    if listing_release != row.2 {
        if let (Some(target), Some(current)) = (release_in(c, &listing_release)?, &rel) {
            let newer = pkg::semver_cmp(&target.version, &current.version)
                == Some(std::cmp::Ordering::Greater);
            if newer && live_attested_in(c, &target)? {
                update = json!({ "release_id": target.id, "version": target.version });
            }
        }
    }
    Ok(json!({
        "id": row.0,
        "listing_id": row.1,
        "address": address,
        "listing_status": listing_status,
        "release_id": row.2,
        "version": rel.as_ref().map(|r| r.version.clone()),
        "security_state": rel.as_ref().map(|r| r.security_state.clone()),
        "previous_release_id": row.3,
        "configuration": serde_json::from_str::<Value>(&row.4).unwrap_or(json!({})),
        "config_schema": schema,
        "secrets": secrets,
        "config_revision": row.5,
        "status": row.6,
        "funding": row.7,
        "update_available": update,
        "created_at": iso(row.8),
        "updated_at": iso(row.9),
        "uninstalled_at": row.10.map(iso),
    }))
}

pub(crate) fn inst_event_in(
    c: &Connection,
    installation: &str,
    kind: &str,
    from: Option<&str>,
    to: Option<&str>,
    actor: &str,
    now: i64,
) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO nest_installation_events (installation_id, kind, from_release, to_release,
            actor, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![installation, kind, from, to, actor, now],
    )?;
    // An org's installation is announced to its webhooks (M2b): ids and versions only.
    let org: Option<(String, String)> = c
        .query_row(
            "SELECT owner_id, listing_id FROM nest_installations WHERE id = ?1 AND owner_kind = 'org'",
            [installation],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((org, listing)) = org {
        store::org_webhook_in(
            c,
            &org,
            "nest.installation.changed",
            &json!({ "installation_id": installation, "listing_id": listing, "change": kind,
                     "from_release": from, "to_release": to }),
            now as u64,
        )?;
    }
    Ok(())
}

/// Secret changes: a sealed value to store, or `None` to clear.
type SecretChanges = Vec<(String, Option<Vec<u8>>)>;

/// Validate a configuration for a release and seal its secrets. Returns the stored part and the
/// secret changes (`None` = clear).
pub(crate) fn prepare_config(
    state: &AppState,
    schema: &Value,
    config: &Value,
    installation: &str,
    already_set: &BTreeSet<String>,
) -> GResult<(String, SecretChanges)> {
    let issues = pkg::validate_config_with(schema, config, already_set);
    if !issues.is_empty() {
        return Err(GErr::bad(
            "invalid_configuration",
            "the configuration does not match the release's schema",
        )
        .with("issues", json!(issues)));
    }
    let (plain, secrets) = pkg::split_config(schema, config);
    let mut sealed = Vec::new();
    for (name, v) in secrets {
        match v.as_str() {
            None => sealed.push((name, None)),
            Some(s) => {
                let aad = format!("{installation}\0{name}");
                let blob = state
                    .vault
                    .seal_labeled(SECRET_LABEL, aad.as_bytes(), s.as_bytes())
                    .map_err(|_| {
                        GErr::new(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "store_error",
                            "seal failed",
                        )
                    })?;
                sealed.push((name, Some(blob)));
            }
        }
    }
    Ok((Value::Object(plain).to_string(), sealed))
}

pub(crate) fn store_secrets_in(
    c: &Connection,
    installation: &str,
    sealed: &[(String, Option<Vec<u8>>)],
    now: i64,
) -> rusqlite::Result<()> {
    for (name, blob) in sealed {
        match blob {
            None => {
                c.execute(
                    "DELETE FROM nest_installation_secrets WHERE installation_id = ?1 AND name = ?2",
                    params![installation, name],
                )?;
            }
            Some(b) => {
                c.execute(
                    "INSERT INTO nest_installation_secrets (installation_id, name, sealed, updated_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(installation_id, name) DO UPDATE SET sealed = excluded.sealed,
                        updated_at = excluded.updated_at",
                    params![installation, name, b, now],
                )?;
            }
        }
    }
    Ok(())
}

/// The caller's own installation, authorized for `action`. Anyone else's is `404`. An org's
/// installation (M2b) is its Nest admins' to manage; its operators may read it (`NestOperate`)
/// and get `403 nest_admin_required` for anything else.
async fn inst_gate(
    state: &AppState,
    headers: &HeaderMap,
    id: &str,
    action: Action,
) -> GResult<(Principal, String, Inst)> {
    use crate::nest_access::Role;
    let (principal, account) = caller(state, headers).await?;
    let iid = id.to_string();
    let who = account.clone();
    let (inst, role) = state
        .store
        .group_read(move |c| -> GResult<(Option<Inst>, Option<Role>)> {
            let inst = inst_in(c, &iid)?;
            let role = match &inst {
                Some(i) if i.owner_kind == "org" => {
                    crate::marketplace_org::org_role_in(c, i, &who, now_unix())?
                }
                _ => None,
            };
            Ok((inst, role))
        })
        .await?;
    if action != Action::NestOperate && role == Some(Role::Operator) {
        return Err(GErr::forbidden(
            "nest_admin_required",
            "only the organization's Nest admins manage its installations",
        ));
    }
    let f = Facts {
        nest_permitted: inst.as_ref().is_some_and(|i| {
            (i.owner_kind == "account" && i.owner_id == account) || role.is_some()
        }),
        ..facts(state)
    };
    authorize(
        &Actor::Principal(&principal),
        action,
        &Resource::InstalledNest(id),
        &f,
    )
    .map_err(|d| deny(d, true))?;
    let inst = inst.ok_or_else(not_found)?;
    Ok((principal, account, inst))
}

fn active(i: &Inst) -> GResult<()> {
    if i.status != "active" {
        return Err(conflict_code(
            "installation_removed",
            "this installation was uninstalled",
        ));
    }
    Ok(())
}

/// `GET /v1/nest-installations`
async fn list_installations(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let result = async {
        let (principal, account) = caller(&state, &headers).await?;
        authorize(
            &Actor::Principal(&principal),
            Action::NestInstall,
            &Resource::Account(&account),
            &facts(&state),
        )
        .map_err(|d| deny(d, false))?;
        let rows = state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> {
                let ids: Vec<String> = {
                    let mut stmt = c.prepare(&format!(
                        "SELECT id FROM nest_installations
                          WHERE owner_kind = 'account' AND owner_id = ?1
                          ORDER BY status = 'active' DESC, created_at DESC, id LIMIT {PAGE}"
                    ))?;
                    let rows = stmt
                        .query_map([&account], |r| r.get(0))?
                        .collect::<rusqlite::Result<Vec<String>>>()?;
                    rows
                };
                let mut out = Vec::new();
                for id in ids {
                    out.push(inst_json_in(c, &id)?);
                }
                Ok(out)
            })
            .await?;
        Ok(Json(json!({ "data": rows, "next_cursor": null })))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallReq {
    listing_id: String,
    #[serde(default)]
    configuration: Option<Value>,
}

/// `POST /v1/nest-installations` — install a listing for the caller's account, on the listing's
/// current attested release. Free (test credits only).
async fn install(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let (principal, account) = caller(&state, &headers).await?;
        authorize(
            &Actor::Principal(&principal),
            Action::NestInstall,
            &Resource::Account(&account),
            &facts(&state),
        )
        .map_err(|d| deny(d, false))?;
        let req: InstallReq = parse(body)?;
        let lid = req.listing_id.clone();
        type Found = (String, String, String, String, Option<Rel>);
        let found = state
            .store
            .group_read(move |c| -> GResult<Option<Found>> {
                let row: Option<(String, String, String, String)> = c
                    .query_row(
                        "SELECT l.publisher_id, l.visibility, l.status, l.release_id FROM nest_listings l
                          WHERE l.id = ?1",
                        [&lid],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )
                    .optional()?;
                let Some((publisher, visibility, status, release)) = row else {
                    return Ok(None);
                };
                let rel = release_in(c, &release)?;
                Ok(Some((publisher, visibility, status, release, rel)))
            })
            .await?;
        let Some((publisher, visibility, status, _, rel)) = found else {
            return Err(not_found());
        };
        // Visible to this caller, or it does not exist for them. A granted listing also needs
        // the grant's install switch (M2b).
        let visible = status == "published"
            && match visibility.as_str() {
                "public" | "unlisted" => true,
                "private" => publisher_read(&state, &headers, &account, &publisher).await,
                _ => {
                    crate::marketplace_grants::install_allowed(
                        &state,
                        &req.listing_id,
                        &visibility,
                        &account,
                    )
                    .await?
                }
            };
        if !visible {
            return Err(not_found());
        }
        let rel = rel.ok_or_else(not_found)?;
        let installation = new_id("ins");
        let (iid, acct, listing) = (installation.clone(), account.clone(), req.listing_id.clone());
        let check = rel.clone();
        let (attested, pub_ok) = state
            .store
            .group_read(move |c| -> GResult<(bool, bool)> {
                let p: String = c.query_row(
                    "SELECT status FROM publisher_profiles WHERE id = ?1",
                    [&check.publisher_id],
                    |r| r.get(0),
                )?;
                Ok((live_attested_in(c, &check)?, p == "approved"))
            })
            .await?;
        if !pub_ok {
            return Err(not_found());
        }
        if !attested {
            return Err(not_runnable(&rel));
        }
        let schema = schema_of(&rel.manifest);
        let config = req.configuration.unwrap_or_else(|| json!({}));
        let (plain, sealed) =
            prepare_config(&state, &schema, &config, &installation, &BTreeSet::new())?;
        let now = now_unix() as i64;
        let context = buyer_context(&listing, &account);
        let release = rel.id.clone();
        let v = state
            .store
            .group_tx(move |tx| -> GResult<Value> {
                let live: Option<String> = tx
                    .query_row(
                        "SELECT id FROM nest_installations WHERE listing_id = ?1
                           AND owner_kind = 'account' AND owner_id = ?2 AND status = 'active'",
                        params![listing, acct],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(existing) = live {
                    return Err(GErr::conflict(
                        "already_installed",
                        "this account already has this listing installed",
                    )
                    .with("installation_id", existing));
                }
                tx.execute(
                    "INSERT INTO nest_installations (id, listing_id, owner_kind, owner_id, buyer_context,
                        release_id, config, status, installed_by, created_at, updated_at)
                     VALUES (?1, ?2, 'account', ?3, ?4, ?5, ?6, 'active', ?3, ?7, ?7)",
                    params![iid, listing, acct, context, release, plain, now],
                )?;
                store_secrets_in(tx, &iid, &sealed, now)?;
                inst_event_in(tx, &iid, "installed", None, Some(&release), &acct, now)?;
                Ok(inst_json_in(tx, &iid)?)
            })
            .await?;
        Ok((StatusCode::CREATED, Json(json!({ "data": v }))))
    }
    .await;
    into_response(result)
}

/// `GET /v1/nest-installations/{installation}`
async fn read_installation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        let (_, _, inst) = inst_gate(&state, &headers, &id, Action::NestOperate).await?;
        let v = state
            .store
            .group_read(move |c| -> GResult<Value> { Ok(inst_json_in(c, &inst.id)?) })
            .await?;
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigReq {
    configuration: Value,
}

/// `PUT /v1/nest-installations/{installation}/configuration` — replace the configuration. A
/// secret left out keeps its stored value; a secret sent as `null` is cleared.
async fn configure(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let (_, account, inst) = inst_gate(&state, &headers, &id, Action::NestConfigure).await?;
        active(&inst)?;
        let req: ConfigReq = parse(body)?;
        let (rid, iid) = (inst.release_id.clone(), inst.id.clone());
        let (rel, set) = state
            .store
            .group_read(move |c| -> GResult<(Option<Rel>, BTreeSet<String>)> {
                Ok((release_in(c, &rid)?, secret_names_in(c, &iid)?))
            })
            .await?;
        let schema = rel.map(|r| schema_of(&r.manifest)).unwrap_or_else(pkg::empty_config_schema);
        let (plain, sealed) = prepare_config(&state, &schema, &req.configuration, &inst.id, &set)?;
        let now = now_unix() as i64;
        let v = state
            .store
            .group_tx(move |tx| -> GResult<Value> {
                let n = tx.execute(
                    "UPDATE nest_installations SET config = ?2, config_revision = config_revision + 1,
                        updated_at = ?3 WHERE id = ?1 AND status = 'active' AND release_id = ?4",
                    params![inst.id, plain, now, inst.release_id],
                )?;
                if n == 0 {
                    return Err(conflict_code(
                        "installation_changed",
                        "the installation changed meanwhile; read it and try again",
                    ));
                }
                store_secrets_in(tx, &inst.id, &sealed, now)?;
                inst_event_in(tx, &inst.id, "configured", None, None, &account, now)?;
                Ok(inst_json_in(tx, &inst.id)?)
            })
            .await?;
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct MoveReq {
    #[serde(default)]
    release_id: Option<String>,
    /// A configuration for the target release, when the current one does not fit its schema.
    #[serde(default)]
    configuration: Option<Value>,
}

fn move_body(body: Option<Json<Value>>) -> GResult<MoveReq> {
    match body {
        Some(Json(v)) => {
            serde_json::from_value(v).map_err(|e| GErr::bad("invalid_request", e.to_string()))
        }
        None => Ok(MoveReq::default()),
    }
}

/// Move an installation to `target` (an update or a rollback): the target must be an attested,
/// live release of the same template, and the configuration must fit its schema.
#[allow(clippy::type_complexity)]
async fn move_installation(
    state: &AppState,
    account: &str,
    inst: Inst,
    target: String,
    configuration: Option<Value>,
    kind: &'static str,
) -> GResult<Value> {
    let (rid, iid) = (inst.release_id.clone(), inst.id.clone());
    let t = target.clone();
    let (current, next, set) = state
        .store
        .group_read(
            move |c| -> GResult<(Option<Rel>, Option<(Rel, bool)>, BTreeSet<String>)> {
                let next = match release_in(c, &t)? {
                    Some(r) => {
                        let ok = live_attested_in(c, &r)?;
                        Some((r, ok))
                    }
                    None => None,
                };
                Ok((release_in(c, &rid)?, next, secret_names_in(c, &iid)?))
            },
        )
        .await?;
    let current = current.ok_or_else(not_found)?;
    let Some((next, attested)) = next.filter(|(r, _)| r.template_id == current.template_id) else {
        return Err(GErr::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such release",
        ));
    };
    if !attested {
        return Err(not_runnable(&next));
    }
    let order = pkg::semver_cmp(&next.version, &current.version);
    match kind {
        "updated" if order != Some(std::cmp::Ordering::Greater) => {
            return Err(conflict_code(
                "not_newer",
                "an update moves to a newer release; use rollback to go back",
            ))
        }
        _ => {}
    }
    let schema = schema_of(&next.manifest);
    let config = match configuration {
        Some(c) => c,
        None => serde_json::from_str(&inst.config).unwrap_or(json!({})),
    };
    let (plain, sealed) = prepare_config(state, &schema, &config, &inst.id, &set).map_err(|e| {
        if e.code == "invalid_configuration" {
            GErr {
                code: "configuration_invalid_for_release",
                message: "the configuration does not fit the target release; send one that does"
                    .into(),
                ..e
            }
        } else {
            e
        }
    })?;
    // Secrets the target release no longer declares are dropped with the move.
    let keep: BTreeSet<String> = pkg::secret_fields(&schema).into_iter().collect();
    let now = now_unix() as i64;
    let who = account.to_string();
    let previous = if kind == "updated" {
        Some(current.id.clone())
    } else {
        None
    };
    state
        .store
        .group_tx(move |tx| -> GResult<Value> {
            let n = tx.execute(
                "UPDATE nest_installations SET release_id = ?2, previous_release_id = ?3, config = ?4,
                    config_revision = config_revision + 1, updated_at = ?5
                  WHERE id = ?1 AND status = 'active' AND release_id = ?6",
                params![inst.id, next.id, previous, plain, now, current.id],
            )?;
            if n == 0 {
                return Err(conflict_code(
                    "installation_changed",
                    "the installation changed meanwhile; read it and try again",
                ));
            }
            store_secrets_in(tx, &inst.id, &sealed, now)?;
            for name in secret_names_in(tx, &inst.id)? {
                if !keep.contains(&name) {
                    tx.execute(
                        "DELETE FROM nest_installation_secrets WHERE installation_id = ?1 AND name = ?2",
                        params![inst.id, name],
                    )?;
                }
            }
            inst_event_in(tx, &inst.id, kind, Some(&current.id), Some(&next.id), &who, now)?;
            Ok(inst_json_in(tx, &inst.id)?)
        })
        .await
}

/// `POST /v1/nest-installations/{installation}/update` — move to a newer attested release: the
/// listing's current one, or the one named.
async fn update_installation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let result = async {
        let (_, account, inst) = inst_gate(&state, &headers, &id, Action::NestManage).await?;
        active(&inst)?;
        let req = move_body(body)?;
        let target = match req.release_id {
            Some(t) => t,
            None => {
                let lid = inst.listing_id.clone();
                state
                    .store
                    .group_read(move |c| -> GResult<String> {
                        Ok(c.query_row(
                            "SELECT release_id FROM nest_listings WHERE id = ?1",
                            [&lid],
                            |r| r.get(0),
                        )?)
                    })
                    .await?
            }
        };
        if target == inst.release_id {
            return Err(conflict_code(
                "up_to_date",
                "this installation runs that release",
            ));
        }
        let v =
            move_installation(&state, &account, inst, target, req.configuration, "updated").await?;
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

/// `POST /v1/nest-installations/{installation}/rollback` — back to the release this
/// installation ran before its last update, if that release is still attested and live.
async fn rollback_installation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let result = async {
        let (_, account, inst) = inst_gate(&state, &headers, &id, Action::NestManage).await?;
        active(&inst)?;
        let req = move_body(body)?;
        if req.release_id.is_some() {
            return Err(GErr::bad(
                "invalid_request",
                "a rollback goes to the previous release; it names none",
            ));
        }
        let Some(previous) = inst.previous_release_id.clone() else {
            return Err(conflict_code(
                "no_previous_release",
                "this installation has no release to roll back to",
            ));
        };
        let v = move_installation(
            &state,
            &account,
            inst,
            previous,
            req.configuration,
            "rolled_back",
        )
        .await?;
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

/// `DELETE /v1/nest-installations/{installation}` — uninstall: the configuration and its
/// secrets are deleted, open jobs are cancelled, and the buyer's worker and its volume are
/// removed on the Nests host.
async fn uninstall(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        let (_, account, inst) = inst_gate(&state, &headers, &id, Action::NestManage).await?;
        active(&inst)?;
        let now = now_unix() as i64;
        let (iid, who) = (inst.id.clone(), account.clone());
        let release = inst.release_id.clone();
        let cancelled = state
            .store
            .group_tx(move |tx| -> GResult<i64> {
                let n = tx.execute(
                    "UPDATE nest_installations SET status = 'uninstalled', config = '{}',
                        uninstalled_at = ?2, updated_at = ?2 WHERE id = ?1 AND status = 'active'",
                    params![iid, now],
                )?;
                if n == 0 {
                    return Err(conflict_code(
                        "installation_removed",
                        "this installation was uninstalled",
                    ));
                }
                tx.execute(
                    "DELETE FROM nest_installation_secrets WHERE installation_id = ?1",
                    [&iid],
                )?;
                let jobs: Vec<String> = {
                    let mut stmt = tx.prepare(
                        "SELECT id FROM nest_jobs WHERE installation_id = ?1
                           AND execution IN ('queued','running','waiting_input')",
                    )?;
                    let rows = stmt
                        .query_map([&iid], |r| r.get(0))?
                        .collect::<rusqlite::Result<Vec<String>>>()?;
                    rows
                };
                // An org installation's agent mailbox goes with it, and every operator's
                // delegation on it (M2b).
                crate::marketplace_org::retire_agent_in(tx, &iid, now as u64)?;
                for job in &jobs {
                    tx.execute(
                        "UPDATE nest_jobs SET execution = 'cancelled', error_code = 'uninstalled',
                            revision = revision + 1, updated_at = ?2 WHERE id = ?1",
                        params![job, now],
                    )?;
                    tx.execute(
                        "INSERT INTO nest_job_events (job_id, kind, detail, created_at)
                         VALUES (?1, 'nest.job.cancelled', ?2, ?3)",
                        params![job, json!({ "execution": "cancelled" }).to_string(), now],
                    )?;
                }
                inst_event_in(tx, &iid, "uninstalled", Some(&release), None, &who, now)?;
                Ok(jobs.len() as i64)
            })
            .await?;
        // The worker host deletes the buyer's worker and data. A host that cannot be reached is
        // reported, not hidden: the installation is already gone here and takes no jobs.
        let runtime = state
            .nests
            .call(
                reqwest::Method::POST,
                "/v1/mp/workers/remove",
                Some(json!({ "listing_id": inst.listing_id, "buyer_context": inst.buyer_context })),
            )
            .await;
        Ok(Json(json!({ "data": {
            "id": inst.id,
            "status": "uninstalled",
            "jobs_cancelled": cancelled,
            "runtime_removed": matches!(runtime, Ok((200, _))),
        }})))
    }
    .await;
    into_response(result)
}

// ---- staff -------------------------------------------------------------------------------------------

/// Role, step-up, then the conflict rule against `publisher` when there is one.
pub(crate) async fn staff_gate(
    state: &AppState,
    headers: &HeaderMap,
    action: Action,
    subject: &str,
    publisher: Option<String>,
) -> GResult<(Principal, String)> {
    let (principal, account) = caller(state, headers).await?;
    let who = account.clone();
    let (roles, conflict) = state
        .store
        .group_read(move |c| -> GResult<(Vec<StaffRole>, bool)> {
            let roles = store::live_staff_roles_in(c, &who)?;
            let conflict = match &publisher {
                Some(p) => store::publisher_conflict_in(c, &who, p)?,
                None => false,
            };
            Ok((roles, conflict))
        })
        .await?;
    let f = Facts {
        staff_roles: roles,
        staff_conflict: conflict,
        ..facts(state)
    };
    authorize(
        &Actor::Principal(&principal),
        action,
        &Resource::StaffSubject(subject),
        &f,
    )
    .map_err(|d| deny(d, false))?;
    Ok((principal, account))
}

/// `GET /v1/staff/publisher-applications?status=` — the certification queue (default: applied).
/// Each row says whether the reviewer has a stake in it.
async fn staff_list_applications(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let result = async {
        let (_, account) = staff_gate(
            &state,
            &headers,
            Action::NestReview,
            "publisher-applications",
            None,
        )
        .await?;
        let status = q.get("status").cloned().unwrap_or_else(|| "applied".into());
        if !matches!(
            status.as_str(),
            "applied" | "approved" | "rejected" | "revoked"
        ) {
            return Err(GErr::bad(
                "invalid_request",
                "status: applied, approved, rejected or revoked",
            ));
        }
        let rows = state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> {
                let list: Vec<Pub> = {
                    let mut stmt = c.prepare(&format!(
                        "SELECT {PUB_COLS} FROM publisher_profiles
                          WHERE status = ?1 AND owner_kind <> 'platform'
                          ORDER BY created_at, id LIMIT {PAGE}"
                    ))?;
                    let rows = stmt
                        .query_map([&status], map_pub)?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    rows
                };
                let mut out = Vec::new();
                for p in list {
                    let mut v = p.json();
                    v["conflict"] = json!(store::publisher_conflict_in(c, &account, &p.id)?);
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CertifyReq {
    decision: String,
    reason: String,
}

/// `POST /v1/staff/publisher-applications/{application}/decisions` — certify or reject.
async fn staff_certify(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path(application): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let req: CertifyReq = parse(body)?;
        if !matches!(req.decision.as_str(), "approved" | "rejected") {
            return Err(GErr::bad("invalid_request", "decision: approved or rejected"));
        }
        let reason = reason_of(&req.reason)?;
        let aid = application.clone();
        let exists = state
            .store
            .group_read(move |c| -> GResult<bool> { Ok(pub_in(c, &aid)?.is_some()) })
            .await?;
        let (principal, account) = staff_gate(
            &state,
            &headers,
            Action::NestReview,
            &application,
            exists.then(|| application.clone()),
        )
        .await?;
        if !exists {
            return Err(not_found());
        }
        let now = now_unix() as i64;
        let audit = audit_row(
            &state,
            &principal,
            &account,
            "marketplace.publisher.decision",
            format!("publisher:{application}"),
            json!({ "decision": req.decision }),
            &client_ip,
            &headers,
        );
        let (v, wake_list) = state
            .store
            .group_tx(move |tx| -> GResult<(Value, Vec<String>)> {
                let n = tx.execute(
                    "UPDATE publisher_profiles SET status = ?2, decided_by = ?3, decided_at = ?4,
                        decision_reason = ?5, updated_at = ?4 WHERE id = ?1 AND status = 'applied'",
                    params![application, req.decision, account, now, reason],
                )?;
                if n == 0 {
                    return Err(conflict_code(
                        "application_decided",
                        "this application is not waiting for a decision",
                    ));
                }
                tx.execute(
                    "INSERT INTO publisher_certifications (id, publisher_id, decision, reason, decided_by, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![new_id("crt"), application, req.decision, reason, account, now],
                )?;
                crate::store::audit::append_in(tx, &audit)?;
                let w = store::publisher_event_in(
                    tx,
                    &application,
                    "nest.publisher.decision",
                    &json!({ "publisher_id": application, "status": req.decision }),
                    now as u64,
                )?;
                Ok((pub_in(tx, &application)?.map(|p| p.json()).unwrap_or(Value::Null), w))
            })
            .await?;
        wake(&state, wake_list);
        Ok((StatusCode::CREATED, Json(json!({ "data": v }))))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReasonReq {
    reason: String,
}

/// `POST /v1/staff/publishers/{publisher}/revocation` — revoke a certification. Its listings are
/// suspended (so their installations stop taking jobs), queued builds are cancelled, and open
/// reviews can no longer be decided. Releases keep their own security state; quarantine them
/// separately if their code is the problem.
async fn staff_revoke_publisher(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path(publisher): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let req: ReasonReq = parse(body)?;
        let reason = reason_of(&req.reason)?;
        let pid = publisher.clone();
        let found = state
            .store
            .group_read(move |c| -> GResult<Option<Pub>> { Ok(pub_in(c, &pid)?) })
            .await?;
        let (principal, account) = staff_gate(
            &state,
            &headers,
            Action::NestQuarantine,
            &publisher,
            found.as_ref().map(|p| p.id.clone()),
        )
        .await?;
        let p = found.ok_or_else(not_found)?;
        if p.owner_kind == "platform" {
            return Err(GErr::bad(
                "invalid_request",
                "Pigeonpost's own publisher is not revoked; quarantine its releases instead",
            ));
        }
        let now = now_unix() as i64;
        let audit = audit_row(
            &state,
            &principal,
            &account,
            "marketplace.publisher.revoke",
            format!("publisher:{publisher}"),
            json!({}),
            &client_ip,
            &headers,
        );
        let (v, wake_list) = state
            .store
            .group_tx(move |tx| -> GResult<(Value, Vec<String>)> {
                let n = tx.execute(
                    "UPDATE publisher_profiles SET status = 'revoked', decided_by = ?2, decided_at = ?3,
                        decision_reason = ?4, updated_at = ?3
                      WHERE id = ?1 AND status IN ('applied','approved')",
                    params![publisher, account, now, reason],
                )?;
                if n == 0 {
                    return Err(conflict_code("publisher_revoked", "this publisher is not certified"));
                }
                let suspended = tx.execute(
                    "UPDATE nest_listings SET status = 'suspended', moderation_reason = 'publisher revoked',
                        updated_at = ?2 WHERE publisher_id = ?1 AND status <> 'withdrawn'",
                    params![publisher, now],
                )?;
                let cancelled = tx.execute(
                    "UPDATE nest_builds SET status = 'cancelled', updated_at = ?2
                      WHERE publisher_id = ?1 AND status = 'queued'",
                    params![publisher, now],
                )?;
                tx.execute(
                    "INSERT INTO publisher_certifications (id, publisher_id, decision, reason, decided_by, created_at)
                     VALUES (?1, ?2, 'revoked', ?3, ?4, ?5)",
                    params![new_id("crt"), publisher, reason, account, now],
                )?;
                crate::store::audit::append_in(tx, &audit)?;
                let w = store::publisher_event_in(
                    tx,
                    &publisher,
                    "nest.publisher.decision",
                    &json!({ "publisher_id": publisher, "status": "revoked" }),
                    now as u64,
                )?;
                Ok((
                    json!({ "publisher_id": publisher, "status": "revoked",
                            "listings_suspended": suspended, "builds_cancelled": cancelled }),
                    w,
                ))
            })
            .await?;
        wake(&state, wake_list);
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

/// `GET /v1/staff/reviews?status=` — the review queue (default: open), newest last.
async fn staff_list_reviews(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let result = async {
        let (_, account) = staff_gate(&state, &headers, Action::NestReview, "reviews", None).await?;
        let status = q.get("status").cloned().unwrap_or_else(|| "open".into());
        if !matches!(status.as_str(), "open" | "blocked" | "decided" | "withdrawn") {
            return Err(GErr::bad(
                "invalid_request",
                "status: open, blocked, decided or withdrawn",
            ));
        }
        let rows = state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> {
                let list: Vec<(String, String, String, String, i64, i64, String)> = {
                    let mut stmt = c.prepare(&format!(
                        "SELECT c.id, c.release_id, c.publisher_id, r.version, c.revision, c.updated_at,
                                p.display_name
                           FROM review_cases c JOIN nest_releases r ON r.id = c.release_id
                           JOIN publisher_profiles p ON p.id = c.publisher_id
                          WHERE c.status = ?1 ORDER BY c.created_at, c.rowid LIMIT {PAGE}"
                    ))?;
                    let rows = stmt
                        .query_map([&status], |r| {
                            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    rows
                };
                let mut out = Vec::new();
                for (case, release, publisher, version, revision, updated, name) in list {
                    let conflict = store::review_conflict_in(c, &account, &release)?;
                    let done: Vec<String> = decisions_in(c, &case)?
                        .into_iter()
                        .map(|d| d.1)
                        .collect();
                    out.push(json!({
                        "id": case, "release_id": release, "publisher_id": publisher,
                        "publisher_name": name, "version": version, "status": status,
                        "revision": revision, "updated_at": iso(updated),
                        "stages_recorded": done,
                        "first_party": conflict.first_party, "conflict": conflict.conflicted,
                    }));
                }
                Ok(out)
            })
            .await?;
        Ok(Json(json!({ "data": rows, "next_cursor": null })))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModerationReq {
    action: String,
    reason: String,
}

/// `POST /v1/staff/listings/{listing}/moderation` — suspend a listing, or reinstate one staff
/// suspended. A reinstated listing goes back to published only if its release is still attested
/// and live and its publisher certified; otherwise to draft.
async fn staff_moderate(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path(listing): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let req: ModerationReq = parse(body)?;
        if !matches!(req.action.as_str(), "suspend" | "reinstate") {
            return Err(GErr::bad("invalid_request", "action: suspend or reinstate"));
        }
        let reason = reason_of(&req.reason)?;
        let lid = listing.clone();
        let publisher = state
            .store
            .group_read(move |c| -> GResult<Option<String>> {
                Ok(c.query_row(
                    "SELECT publisher_id FROM nest_listings WHERE id = ?1",
                    [&lid],
                    |r| r.get(0),
                )
                .optional()?)
            })
            .await?;
        let (principal, account) = staff_gate(
            &state,
            &headers,
            Action::NestQuarantine,
            &listing,
            publisher.clone(),
        )
        .await?;
        let publisher = publisher.ok_or_else(not_found)?;
        let now = now_unix() as i64;
        let audit = audit_row(
            &state,
            &principal,
            &account,
            &format!("marketplace.listing.{}", req.action),
            format!("listing:{listing}"),
            json!({}),
            &client_ip,
            &headers,
        );
        let (v, wake_list) =
            state
                .store
                .group_tx(move |tx| -> GResult<(Value, Vec<String>)> {
                    let (status, release, published_at): (String, String, Option<i64>) = tx
                        .query_row(
                        "SELECT status, release_id, published_at FROM nest_listings WHERE id = ?1",
                        [&listing],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )?;
                    let next = if req.action == "suspend" {
                        if status == "suspended" {
                            return Err(conflict_code(
                                "listing_suspended",
                                "this listing is suspended",
                            ));
                        }
                        "suspended"
                    } else {
                        if status != "suspended" {
                            return Err(conflict_code(
                                "listing_not_suspended",
                                "this listing is not suspended",
                            ));
                        }
                        let rel = release_in(tx, &release)?;
                        let pub_ok: bool = tx.query_row(
                            "SELECT status IN ('approved') FROM publisher_profiles WHERE id = ?1",
                            [&publisher],
                            |r| r.get(0),
                        )?;
                        let live = match &rel {
                            Some(r) => live_attested_in(tx, r)?,
                            None => false,
                        };
                        if live && pub_ok && published_at.is_some() {
                            "published"
                        } else {
                            "draft"
                        }
                    };
                    tx.execute(
                    "UPDATE nest_listings SET status = ?2, moderation_reason = ?3, updated_at = ?4
                      WHERE id = ?1",
                    params![
                        listing,
                        next,
                        if next == "suspended" { Some(reason.clone()) } else { None },
                        now
                    ],
                )?;
                    crate::store::audit::append_in(tx, &audit)?;
                    let w = store::publisher_event_in(
                        tx,
                        &publisher,
                        "nest.listing.updated",
                        &json!({ "listing_id": listing, "status": next }),
                        now as u64,
                    )?;
                    Ok((json!({ "listing_id": listing, "status": next }), w))
                })
                .await?;
        wake(&state, wake_list);
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_follow_the_contract_order() {
        assert_eq!(stage_before("intake"), None);
        assert_eq!(stage_before("deterministic_scan"), Some("intake"));
        assert_eq!(stage_before("ai_analysis"), Some("deterministic_scan"));
        assert_eq!(stage_before("behavioral_test"), Some("ai_analysis"));
        assert_eq!(stage_before("human_review"), Some("behavioral_test"));
        assert_eq!(stage_before("nonsense"), None);
    }

    #[test]
    fn sources_are_pinned_https_without_credentials() {
        assert!(repository_ok("https://git.example.org/acme/nest.git"));
        for bad in [
            "http://git.example.org/acme/nest",
            "https://user:pw@git.example.org/acme/nest",
            "https://git.example.org",
            "https://git.example.org/acme/nest?ref=main",
            "https://git.example.org/acme/ nest",
            "file:///etc/passwd",
        ] {
            assert!(!repository_ok(bad), "{bad}");
        }
        assert!(commit_ok(&"a".repeat(40)));
        assert!(commit_ok(&"0".repeat(64)));
        assert!(!commit_ok(&"A".repeat(40)));
        assert!(!commit_ok("main"));
    }

    #[test]
    fn findings_are_redacted_per_stage() {
        let ev = json!({ "findings": [
            { "rule": "behaviour.exfiltration", "detail": "a planted honeytoken left through stdout", "blocker": true }
        ]});
        let b = redact("behavioral_test", &ev);
        assert_eq!(b[0]["rule"], "behaviour.exfiltration");
        assert!(b[0].get("detail").is_none());
        assert_eq!(redact("ai_analysis", &ev), json!([]));
        let s = redact("deterministic_scan", &ev);
        assert_eq!(s[0]["detail"], "a planted honeytoken left through stdout");
    }

    #[test]
    fn delivered_configuration_carries_defaults_and_never_secrets() {
        let manifest = json!({ "config_schema": {
            "type": "object", "additionalProperties": false,
            "properties": {
                "team": {"type": "string", "maxLength": 20},
                "size": {"type": "integer", "default": 3},
                "token": {"type": "string", "maxLength": 50, "writeOnly": true},
            },
        }})
        .to_string();
        let v = delivered_config(Some(&manifest), Some(r#"{"team":"ops"}"#));
        assert_eq!(v, json!({ "team": "ops", "size": 3 }));
        assert_eq!(delivered_config(Some(&manifest), None), Value::Null);
        assert_eq!(delivered_config(None, Some("{}")), json!({}));
    }
}
