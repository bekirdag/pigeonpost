//! Publisher-hosted listings (build plan M6, "external-hosted listings with integration-only
//! assurance", pulled forward for one listing kind).
//!
//! A publisher-hosted listing is run by the publisher, not by Pigeonpost: its jobs are delivered
//! as Pigeonpost messages to a **service mailbox** the publisher owns, and the service posts the
//! result back. Pigeonpost reviews the publisher and the integration (its service address and
//! operations), never the code, and says so before a buyer runs it.
//!
//! | Route | Action | Who |
//! |---|---|---|
//! | `POST /v1/publishers/{publisher}/integrations` | `PublisherManage` | a certified publisher's admins |
//! | `POST /v1/marketplace/hosted-jobs` | `NestInstall` | a person, for their own account |
//! | `GET /v1/marketplace/hosted-jobs`, `GET …/{job}`, `GET …/{job}/files/{index}` | `NestJobRead` | the account that ran the job |
//! | `POST /v1/nest-jobs/{job}/result` | the service mailbox's own credential | the listing's service mailbox |
//! | `GET /v1/staff/hosted-jobs?state=` | `NestReview` | a reviewer with step-up |
//!
//! - **Integration**: a release row whose manifest is the integration descriptor
//!   (`kind: publisher_hosted`, `service_address`, `operations`) and `build_isolation =
//!   'publisher_hosted'`. It is immutable like any release, so a new service address or operation
//!   is a new version and a new review. Its review case has `kind = 'integration'` and one stage,
//!   the staff decision (`POST /v1/staff/reviews/{review}/decisions`, step-up), with no
//!   attestation. The service mailbox must belong to the publisher's account (or, for an org
//!   publisher, the org's internal account) at submission, at publish and at every dispatch.
//! - **Job**: the buyer runs an operation with input that matches its fields. The job goes from
//!   the platform marketplace mailbox (`POSTBOX_MARKETPLACE_MAILBOX`, default
//!   `/pigeonpost/marketplace`, owned by the platform owner's account) to the service mailbox as
//!   one message whose body is exactly [`job_message`]. Retries reuse a `msg_ref` derived from the
//!   job id, so a resend is never a second message. The buyer is never named to the publisher.
//! - **Result**: the service posts `{status, summary, files}`; a PDF result is kept in the
//!   attachment store and sent to the buyer's mailbox from the marketplace mailbox, attached.
//!   Retrying the same result is answered with the stored one; a different one is refused.
//!
//! **No money moves here.** Paid third-party jobs will be funded by prepaid credits, designed
//! separately: until then a publisher-hosted listing with a price takes no jobs
//! (`402 payment_unavailable`).

use crate::groups::{GErr, GResult};
use crate::marketplace::{caller, deny, facts, into_response, iso, not_found, parse};
use crate::marketplace_m1::{canonical_json, conflict_code, digest_of, flag_gate};
use crate::marketplace_m2::{
    certified, gate, new_id, publisher_read, release_in, staff_gate, text, wake, Need, Rel,
};
use crate::policy::{authorize, Action, Actor, Facts, Resource};
use crate::store::marketplace as store;
use crate::{now_unix, AppState};
use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap};

/// The body type of a job message to a publisher's service.
pub(crate) const JOB_MESSAGE_TYPE: &str = "pigeonpost.nest_job.v1";
/// The one kind of output a publisher-hosted operation returns today.
const OUTPUT_MEDIA_TYPES: [&str; 1] = ["application/pdf"];
/// Largest result file, decoded.
pub(crate) const MAX_FILE_BYTES: usize = 20 * 1024 * 1024;
/// Most files in one result.
const MAX_FILES: usize = 5;
/// The result route's body limit: base64 of the largest result, with room for the JSON.
const RESULT_BODY_MAX: usize = 30 * 1024 * 1024;
/// A job's deadline unless its operation names one, and the furthest one may be.
const DEFAULT_DEADLINE_HOURS: i64 = 72;
const MAX_DEADLINE_HOURS: i64 = 14 * 24;
/// Open (queued or running) jobs one account may have at once.
const MAX_OPEN_JOBS: i64 = 10;
/// Delivery attempts before a message is given up (the result stays on the job).
const MAX_DELIVERIES: i64 = 8;
/// Largest job input, as JSON.
const MAX_INPUT_BYTES: usize = 64 * 1024;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/publishers/{publisher}/integrations",
            post(submit_integration),
        )
        .route("/v1/marketplace/hosted-jobs", get(list_jobs).post(run_job))
        .route("/v1/marketplace/hosted-jobs/{job}", get(read_job))
        .route(
            "/v1/marketplace/hosted-jobs/{job}/files/{index}",
            get(download),
        )
        .route(
            "/v1/nest-jobs/{job}/result",
            post(post_result).layer(DefaultBodyLimit::max(RESULT_BODY_MAX)),
        )
        .route("/v1/staff/hosted-jobs", get(staff_jobs))
}

// ---- integrations -------------------------------------------------------------------------------

/// `Rel::hosted` is set from `build_isolation`; this is the refusal an install gets.
pub(crate) fn not_installable() -> GErr {
    conflict_code(
        "publisher_hosted",
        "this service is run by its publisher; run its operations instead of installing it",
    )
}

/// The mailbox an address names: `(address, account_id)`, by `/k/` address or handle.
fn mailbox_in(c: &Connection, addr: &str) -> rusqlite::Result<Option<(String, Option<String>)>> {
    let a = addr.trim().to_ascii_lowercase();
    c.query_row(
        "SELECT address, account_id FROM identities WHERE address = ?1 OR handle = ?1
          ORDER BY address = ?1 DESC LIMIT 1",
        [&a],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
}

/// The account a publisher's service mailbox must belong to: the owning account, or an org
/// publisher's internal account (where its agent mailboxes live).
fn owner_account_in(c: &Connection, publisher: &str) -> rusqlite::Result<Option<String>> {
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT owner_kind, owner_id FROM publisher_profiles WHERE id = ?1",
            [publisher],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(match row {
        Some((k, id)) if k == "account" => Some(id),
        Some((k, id)) if k == "org" => c
            .query_row(
                "SELECT internal_account_id FROM orgs WHERE id = ?1",
                [&id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten(),
        _ => None,
    })
}

/// Whether `mailbox` (a `/k/` address) belongs to the publisher now.
fn service_owned_in(c: &Connection, publisher: &str, mailbox: &str) -> rusqlite::Result<bool> {
    let Some(owner) = owner_account_in(c, publisher)? else {
        return Ok(false);
    };
    Ok(mailbox_in(c, mailbox)?
        .is_some_and(|(addr, acct)| addr == mailbox && acct.as_deref() == Some(owner.as_str())))
}

fn service_not_owned() -> GErr {
    GErr::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "service_address_not_owned",
        "the service address must be a mailbox of this publisher's account (or its organization)",
    )
}

/// What publishing (or reinstating) a publisher-hosted listing needs: an approved integration, not
/// stopped for security, whose service mailbox is still the publisher's.
pub(crate) fn publishable_in(c: &Connection, rel: &Rel) -> GResult<()> {
    match rel.security_state.as_str() {
        "revoked" => return Err(conflict_code("release_revoked", "this release is revoked")),
        "quarantined" => {
            return Err(conflict_code(
                "security_quarantined",
                "this release is quarantined",
            ))
        }
        _ => {}
    }
    if !rel.hosted || rel.status != "approved" {
        return Err(conflict_code(
            "review_required",
            "this integration has not been approved by Pigeonpost review",
        ));
    }
    let mailbox = rel.manifest["service_mailbox"].as_str().unwrap_or("");
    if !service_owned_in(c, &rel.publisher_id, mailbox)? {
        return Err(service_not_owned());
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FieldReq {
    name: String,
    max_chars: u64,
    required: bool,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    multiline: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputSchemaReq {
    fields: Vec<FieldReq>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputReq {
    kind: String,
    media_type: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationReq {
    name: String,
    version: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    input_schema: InputSchemaReq,
    output: OutputReq,
    #[serde(default)]
    deadline_hours: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IntegrationReq {
    template_id: String,
    version: String,
    service_address: String,
    operations: Vec<OperationReq>,
}

/// The descriptor's operations, validated, in manifest form (`fields` as the M1 field shape,
/// plus a label and multiline hint for the form).
fn operations_of(ops: &[OperationReq]) -> GResult<Vec<Value>> {
    let bad = |m: String| GErr::bad("invalid_integration", m);
    if ops.is_empty() || ops.len() > 20 {
        return Err(bad("operations: 1 to 20".into()));
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for (i, o) in ops.iter().enumerate() {
        let at = format!("operations[{i}]");
        if !pigeonpost_core::nest_package::operation_name_ok(&o.name) {
            return Err(bad(format!("{at}.name: lower-case dotted words")));
        }
        if !pigeonpost_core::nest_package::operation_version_ok(&o.version) {
            return Err(bad(format!("{at}.version: major.minor")));
        }
        if !seen.insert((o.name.clone(), o.version.clone())) {
            return Err(bad(format!("{at}: declared twice")));
        }
        if o.output.kind != "file" || !OUTPUT_MEDIA_TYPES.contains(&o.output.media_type.as_str()) {
            return Err(bad(format!(
                "{at}.output: {{kind: file, media_type: application/pdf}}"
            )));
        }
        let hours = o.deadline_hours.unwrap_or(DEFAULT_DEADLINE_HOURS);
        if !(1..=MAX_DEADLINE_HOURS).contains(&hours) {
            return Err(bad(format!(
                "{at}.deadline_hours: 1 to {MAX_DEADLINE_HOURS}"
            )));
        }
        let f = &o.input_schema.fields;
        if f.is_empty() || f.len() > 32 {
            return Err(bad(format!("{at}.input_schema.fields: 1 to 32")));
        }
        let mut names = BTreeSet::new();
        let mut fields = Vec::new();
        for (j, x) in f.iter().enumerate() {
            let fat = format!("{at}.input_schema.fields[{j}]");
            if !pigeonpost_core::nest_package::field_name_ok(&x.name) || !names.insert(&x.name) {
                return Err(bad(format!("{fat}.name: [a-z][a-z0-9_]*, unique")));
            }
            if !(1..=16_000).contains(&x.max_chars) {
                return Err(bad(format!("{fat}.max_chars: 1 to 16000")));
            }
            let mut v = json!({ "name": x.name, "max_chars": x.max_chars, "required": x.required });
            if let Some(l) = &x.label {
                v["label"] = json!(text(l, &format!("{fat}.label"), 1, 80)?);
            }
            if x.multiline == Some(true) {
                v["multiline"] = json!(true);
            }
            fields.push(v);
        }
        let mut op = json!({
            "name": o.name, "version": o.version, "fields": fields,
            "output": { "kind": "file", "media_type": o.output.media_type },
            "deadline_hours": hours,
        });
        if let Some(t) = &o.title {
            op["title"] = json!(text(t, &format!("{at}.title"), 1, 120)?);
        }
        if let Some(d) = &o.description {
            op["description"] = json!(text(d, &format!("{at}.description"), 1, 1000)?);
        }
        out.push(op);
    }
    Ok(out)
}

/// `POST /v1/publishers/{publisher}/integrations` — submit a publisher-hosted integration for
/// review: a version of one of the publisher's templates, naming the service mailbox and its
/// operations. Answers with the release and its open review.
async fn submit_integration(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(publisher): Path<String>,
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
        let req: IntegrationReq = parse(body)?;
        if !pigeonpost_core::nest_package::semver_ok(&req.version) {
            return Err(GErr::bad("invalid_integration", "version: semver"));
        }
        let operations = operations_of(&req.operations)?;
        let service = req.service_address.trim().to_ascii_lowercase();
        if !service.starts_with('/') || service.len() > 200 {
            return Err(GErr::bad(
                "invalid_integration",
                "service_address: a Pigeonpost address such as /acme/hr",
            ));
        }
        if service.starts_with("/pigeonpost/") {
            return Err(GErr::bad(
                "invalid_integration",
                "addresses under /pigeonpost/ are Pigeonpost's own",
            ));
        }
        let (pid, who) = (ctx.p.id.clone(), ctx.account.clone());
        let now = now_unix() as i64;
        let (v, wake_list) = state
            .store
            .group_tx(move |tx| -> GResult<(Value, Vec<String>)> {
                let slug: Option<String> = tx
                    .query_row(
                        "SELECT slug FROM nest_templates WHERE id = ?1 AND publisher_id = ?2",
                        params![req.template_id, pid],
                        |r| r.get(0),
                    )
                    .optional()?;
                let slug = slug.ok_or_else(|| {
                    GErr::new(StatusCode::NOT_FOUND, "not_found", "no such template")
                })?;
                let taken: bool = tx
                    .query_row(
                        "SELECT 1 FROM nest_releases WHERE template_id = ?1 AND version = ?2",
                        params![req.template_id, req.version],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some();
                if taken {
                    return Err(conflict_code(
                        "version_exists",
                        "this template already has that version",
                    ));
                }
                let (mailbox, _) = mailbox_in(tx, &service)?.ok_or_else(service_not_owned)?;
                if !service_owned_in(tx, &pid, &mailbox)? {
                    return Err(service_not_owned());
                }
                let manifest = json!({
                    "kind": "publisher_hosted",
                    "name": slug,
                    "version": req.version,
                    "service_address": service,
                    "service_mailbox": mailbox,
                    "operations": operations,
                });
                let digest = digest_of(canonical_json(&manifest).as_bytes());
                let release = new_id("rel");
                tx.execute(
                    "INSERT INTO nest_releases (id, template_id, publisher_id, version, image_digest,
                        status, created_by, created_at, release_digest, manifest_digest, manifest,
                        build_isolation)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'submitted', ?6, ?7, ?5, ?5, ?8, 'publisher_hosted')",
                    params![
                        release,
                        req.template_id,
                        pid,
                        req.version,
                        digest,
                        who,
                        now,
                        manifest.to_string()
                    ],
                )?;
                let previous: Option<String> = tx
                    .query_row(
                        "SELECT c.id FROM review_cases c JOIN nest_releases r ON r.id = c.release_id
                          WHERE r.template_id = ?1 AND r.id <> ?2
                          ORDER BY c.created_at DESC, c.rowid DESC LIMIT 1",
                        params![req.template_id, release],
                        |r| r.get(0),
                    )
                    .optional()?;
                let case = new_id("rvw");
                tx.execute(
                    "INSERT INTO review_cases (id, release_id, publisher_id, status, previous_id,
                        created_at, updated_at, kind)
                     VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?5, 'integration')",
                    params![case, release, pid, previous, now],
                )?;
                let w = store::publisher_event_in(
                    tx,
                    &pid,
                    "nest.review.stage_changed",
                    &json!({ "review_id": case, "release_id": release, "stage": "human_review",
                             "status": "pending", "revision": 1 }),
                    now as u64,
                )?;
                Ok((
                    json!({ "release_id": release, "review_id": case, "release_digest": digest,
                            "kind": "integration", "manifest": manifest }),
                    w,
                ))
            })
            .await?;
        wake(&state, wake_list);
        Ok((StatusCode::CREATED, Json(json!({ "data": v }))))
    }
    .await;
    into_response(result)
}

// ---- staff review (called from M1's staff routes) -----------------------------------------------

pub(crate) async fn is_integration_case(state: &AppState, review: &str) -> GResult<bool> {
    let r = review.to_string();
    state
        .store
        .group_read(move |c| -> GResult<bool> {
            Ok(c.query_row(
                "SELECT kind = 'integration' FROM review_cases WHERE id = ?1",
                [&r],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false))
        })
        .await
}

/// For `GET /v1/staff/reviews/{review}`: the case kind and, for an integration, its descriptor and
/// whether the service mailbox is still the publisher's.
pub(crate) fn staff_view_in(c: &Connection, case: &str, release: &str) -> GResult<(String, Value)> {
    let kind: String = c.query_row("SELECT kind FROM review_cases WHERE id = ?1", [case], |r| {
        r.get(0)
    })?;
    if kind != "integration" {
        return Ok((kind, Value::Null));
    }
    let rel = release_in(c, release)?.ok_or_else(not_found)?;
    let mailbox = rel.manifest["service_mailbox"].as_str().unwrap_or("");
    let owned = service_owned_in(c, &rel.publisher_id, mailbox)?;
    Ok((
        kind,
        json!({
            "service_address": rel.manifest["service_address"],
            "service_mailbox": mailbox,
            "service_owned": owned,
            "operations": rel.manifest["operations"],
        }),
    ))
}

/// One staff decision on an integration review.
pub(crate) struct Decision<'a> {
    pub(crate) case: &'a str,
    pub(crate) release: &'a str,
    pub(crate) decision: &'a str,
    pub(crate) reason: &'a str,
    pub(crate) reviewer: &'a str,
    pub(crate) decision_id: &'a str,
    pub(crate) presented_digest: &'a str,
    pub(crate) stored_digest: Option<&'a str>,
    pub(crate) platform_owner: Option<&'a str>,
}

/// The integration branch of `POST /v1/staff/reviews/{review}/decisions`: the case is open, the
/// publisher certified and the release not stopped (checked by the caller); the reviewer names
/// the digest they looked at. An approval needs the service mailbox to be the publisher's now.
/// No attestation: Pigeonpost runs none of this code.
pub(crate) fn decide_in(
    tx: &Transaction<'_>,
    d: &Decision<'_>,
    audit: &crate::store::audit::AuditRow,
    now: i64,
) -> GResult<Value> {
    if d.stored_digest != Some(d.presented_digest) {
        return Err(conflict_code(
            "release_digest_mismatch",
            "the release digest is not the one under review",
        ));
    }
    if d.decision == "approved" {
        let rel = release_in(tx, d.release)?.ok_or_else(not_found)?;
        let mailbox = rel.manifest["service_mailbox"].as_str().unwrap_or("");
        if !service_owned_in(tx, &rel.publisher_id, mailbox)? {
            return Err(service_not_owned());
        }
    }
    tx.execute(
        "INSERT INTO review_decisions (id, case_id, reviewer, decision, reason, created_at, stage,
            first_party)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'human_review', 0)",
        params![d.decision_id, d.case, d.reviewer, d.decision, d.reason, now],
    )?;
    match d.decision {
        "approved" => tx.execute(
            "UPDATE nest_releases SET status = 'approved' WHERE id = ?1",
            [d.release],
        )?,
        "rejected" => tx.execute(
            "UPDATE nest_releases SET status = 'rejected' WHERE id = ?1",
            [d.release],
        )?,
        _ => 0,
    };
    tx.execute(
        "UPDATE review_cases SET status = 'decided', updated_at = ?2, revision = revision + 1
          WHERE id = ?1",
        params![d.case, now],
    )?;
    let kinds: &[&str] = if d.decision == "changes_requested" {
        &["decision"]
    } else {
        &["decision", "completed"]
    };
    for kind in kinds {
        crate::marketplace_hardening::review_event_in(
            tx,
            d.case,
            kind,
            json!({ "decision": d.decision }),
            d.platform_owner,
            now,
        )?;
    }
    crate::store::audit::append_in(tx, audit)?;
    Ok(json!({
        "decision_id": d.decision_id,
        "decision": d.decision,
        "first_party": false,
        "kind": "integration",
        "attestation": null,
    }))
}

// ---- the platform marketplace mailbox ---------------------------------------------------------

/// The mailbox jobs and results are sent from: `POSTBOX_MARKETPLACE_MAILBOX` (default
/// `/pigeonpost/marketplace`). It must exist and belong to the platform owner's account.
async fn platform_mailbox(state: &AppState) -> GResult<crate::store::StoredIdentity> {
    let unavailable = || {
        GErr::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "marketplace_mailbox_unavailable",
            "the marketplace mailbox is not set up on this postbox",
        )
    };
    let name = std::env::var("POSTBOX_MARKETPLACE_MAILBOX")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "/pigeonpost/marketplace".into());
    let n = name.clone();
    let found = state
        .store
        .group_read(move |c| -> GResult<Option<String>> { Ok(mailbox_in(c, &n)?.map(|(a, _)| a)) })
        .await?
        .ok_or_else(unavailable)?;
    let id = state.store.get(found).await?.ok_or_else(unavailable)?;
    match (&id.account_id, state.platform_owner.as_deref()) {
        (Some(a), Some(owner)) if a == owner => Ok(id),
        _ => Err(unavailable()),
    }
}

/// A `msg_ref` derived from the job and purpose, so a resend is recognised as the same message.
fn msg_ref_for(job: &str, purpose: &str) -> String {
    const B32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let d = digest_of(format!("pigeonpost-hosted-job\0{purpose}\0{job}").as_bytes());
    let hex = d.trim_start_matches("sha256:");
    let bytes: Vec<u8> = (0..32)
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0))
        .collect();
    let mut out = String::from("mref_");
    let (mut buf, mut bits) = (0u32, 0u32);
    for b in bytes {
        buf = (buf << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            out.push(B32[((buf >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        out.push(B32[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// The exact body of the message that hands a job to the publisher's service. Nothing about the
/// buyer is in it: the job id is the only reference.
pub(crate) fn job_message(
    public_url: &str,
    job: &str,
    listing: &str,
    operation: &str,
    input: &Value,
    deadline_at: i64,
) -> String {
    json!({
        "type": JOB_MESSAGE_TYPE,
        "job_id": job,
        "listing_id": listing,
        "operation": operation,
        "input": input,
        "result_url": format!("{}/v1/nest-jobs/{job}/result", public_url.trim_end_matches('/')),
        "deadline": iso(deadline_at),
    })
    .to_string()
}

// ---- running a job ------------------------------------------------------------------------------

/// A listing as running a job decides on it.
struct Listing {
    publisher: String,
    visibility: String,
    granted: bool,
    rel: Option<Rel>,
    priced: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunReq {
    listing_id: String,
    operation: String,
    #[serde(default)]
    operation_version: Option<String>,
    input: Map<String, Value>,
    /// One of the caller's own mailboxes; default their first.
    #[serde(default)]
    deliver_to: Option<String>,
}

fn job_json_in(c: &Connection, id: &str) -> rusqlite::Result<Option<Value>> {
    #[allow(clippy::type_complexity)]
    let row: Option<(
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
        i64,
        i64,
        i64,
        Option<i64>,
        String,
        Option<String>,
    )> = c
        .query_row(
            "SELECT j.listing_id, j.operation, j.operation_version, j.state, j.deliver_to,
                    j.delivery, j.summary, j.error_code, j.revision, j.deadline_at, j.created_at,
                    j.updated_at, j.finished_at, l.title, l.service_address
               FROM nest_hosted_jobs j JOIN nest_listings l ON l.id = j.listing_id
              WHERE j.id = ?1",
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
                    r.get(11)?,
                    r.get(12)?,
                    r.get(13)?,
                    r.get(14)?,
                ))
            },
        )
        .optional()?;
    let Some((
        listing,
        op,
        ver,
        st,
        to,
        delivery,
        summary,
        error,
        rev,
        deadline,
        created,
        updated,
        finished,
        title,
        service,
    )) = row
    else {
        return Ok(None);
    };
    let mut stmt = c.prepare(
        "SELECT idx, name, media_type, bytes FROM nest_hosted_job_files WHERE job_id = ?1
          ORDER BY idx",
    )?;
    let files = stmt
        .query_map([id], |r| {
            let idx: i64 = r.get(0)?;
            Ok(json!({
                "index": idx, "name": r.get::<_, String>(1)?,
                "media_type": r.get::<_, String>(2)?, "bytes": r.get::<_, i64>(3)?,
                "download_path": format!("/v1/marketplace/hosted-jobs/{id}/files/{idx}"),
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let execution = st.as_str();
    Ok(Some(json!({
        "job_id": id,
        "listing_id": listing,
        "listing_title": title,
        "hosting": "publisher",
        "service_address": service,
        "operation": op,
        "operation_version": ver,
        "execution": execution,
        "deliver_to": to,
        "delivery": delivery,
        "result": if summary.is_some() || !files.is_empty() {
            json!({ "summary": summary, "files": files })
        } else {
            Value::Null
        },
        "error_code": error,
        "revision": rev,
        "deadline_at": iso(deadline),
        "created_at": iso(created),
        "updated_at": iso(updated),
        "finished_at": finished.map(iso),
    })))
}

fn event_in(c: &Connection, job: &str, kind: &str, now: i64) -> GResult<Option<String>> {
    let row: Option<(String, String, String)> = c
        .query_row(
            "SELECT account_id, listing_id, state FROM nest_hosted_jobs WHERE id = ?1",
            [job],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((account, listing, st)) = row else {
        return Ok(None);
    };
    crate::store::append_event(
        c,
        &account,
        kind,
        None,
        &json!({ "job_id": job, "listing_id": listing, "execution": st, "hosting": "publisher" }),
        now.max(0) as u64,
    )?;
    Ok(Some(account))
}

/// `POST /v1/marketplace/hosted-jobs` — run one operation of a publisher-hosted listing. The input
/// must match the operation's fields. The job is handed to the publisher's service at once.
async fn run_job(
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
        let req: RunReq = parse(body)?;
        let input = Value::Object(req.input.clone());
        if input.to_string().len() > MAX_INPUT_BYTES {
            return Err(GErr::bad("invalid_operation_payload", "input: at most 64 KiB"));
        }
        let (lid, acct) = (req.listing_id.clone(), account.clone());
        let found = state
            .store
            .group_read(move |c| -> GResult<Option<Listing>> {
                let row: Option<(String, String, String, String, String, String)> = c
                    .query_row(
                        "SELECT l.publisher_id, l.visibility, l.status, l.release_id, l.hosting,
                                p.status
                           FROM nest_listings l JOIN publisher_profiles p ON p.id = l.publisher_id
                          WHERE l.id = ?1",
                        [&lid],
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
                let Some((publisher, visibility, status, release, hosting, pstatus)) = row else {
                    return Ok(None);
                };
                if hosting != "publisher" || status != "published" || pstatus != "approved" {
                    return Ok(None);
                }
                // An invite or organization grant needs its invoke switch (M2b).
                let granted = matches!(visibility.as_str(), "invite_only" | "organization")
                    && crate::marketplace_grants::account_grant_in(
                        c,
                        &lid,
                        &visibility,
                        &acct,
                        crate::marketplace_grants::Use::Invoke,
                    )?;
                Ok(Some(Listing {
                    publisher,
                    visibility,
                    granted,
                    rel: release_in(c, &release)?,
                    priced: crate::marketplace_pay::is_priced_in(c, &lid)?,
                }))
            })
            .await?;
        let Some(Listing {
            publisher,
            visibility,
            granted,
            rel,
            priced,
        }) = found
        else {
            return Err(not_found());
        };
        let visible = match visibility.as_str() {
            "public" | "unlisted" => true,
            "private" => publisher_read(&state, &headers, &account, &publisher).await,
            _ => granted,
        };
        if !visible {
            return Err(not_found());
        }
        let rel = rel.ok_or_else(not_found)?;
        // TODO(prepaid credits): paid third-party jobs are funded by prepaid USD credits, designed
        // separately. Until that lands no charge path exists, so a priced listing takes no jobs.
        if priced {
            return Err(GErr::new(
                StatusCode::PAYMENT_REQUIRED,
                "payment_unavailable",
                "paid publisher-hosted services cannot be run yet",
            ));
        }
        let op = rel.manifest["operations"]
            .as_array()
            .and_then(|ops| {
                ops.iter().find(|o| {
                    o["name"] == req.operation.as_str()
                        && req
                            .operation_version
                            .as_deref()
                            .is_none_or(|v| o["version"] == v)
                })
            })
            .cloned()
            .ok_or_else(|| GErr::bad("unknown_operation", "this listing has no such operation"))?;
        let schema = json!({ "fields": op["fields"] }).to_string();
        crate::marketplace_m1::validate_payload(&schema, &req.input)?;
        let hours = op["deadline_hours"]
            .as_i64()
            .unwrap_or(DEFAULT_DEADLINE_HOURS);
        let now = now_unix() as i64;
        let id = new_id("job");
        let (jid, acct, want) = (id.clone(), account.clone(), req.deliver_to.clone());
        let (lid, oname, over) = (
            req.listing_id.clone(),
            req.operation.clone(),
            op["version"].as_str().unwrap_or("").to_string(),
        );
        let (v, wake_to) = state
            .store
            .group_tx(move |tx| -> GResult<(Value, Option<String>)> {
                publishable_in(tx, &rel)?;
                let deliver: Option<String> = match &want {
                    Some(w) => mailbox_in(tx, w)?
                        .filter(|(_, a)| a.as_deref() == Some(acct.as_str()))
                        .map(|(a, _)| a),
                    None => tx
                        .query_row(
                            "SELECT address FROM identities WHERE account_id = ?1
                              ORDER BY created_at, address LIMIT 1",
                            [&acct],
                            |r| r.get(0),
                        )
                        .optional()?,
                };
                let deliver = deliver.ok_or_else(|| {
                    GErr::conflict(
                        "no_mailbox",
                        "results are sent to one of your mailboxes; create one first",
                    )
                })?;
                let open: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM nest_hosted_jobs WHERE account_id = ?1
                       AND state IN ('queued','running')",
                    [&acct],
                    |r| r.get(0),
                )?;
                if open >= MAX_OPEN_JOBS {
                    return Err(GErr::new(
                        StatusCode::TOO_MANY_REQUESTS,
                        "rate_limited",
                        "too many jobs are still running; wait for one to finish",
                    ));
                }
                tx.execute(
                    "INSERT INTO nest_hosted_jobs (id, account_id, listing_id, release_id,
                        publisher_id, operation, operation_version, input, deliver_to,
                        service_mailbox, state, next_attempt_at, deadline_at, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'queued', ?11, ?12, ?11, ?11)",
                    params![
                        jid,
                        acct,
                        lid,
                        rel.id,
                        rel.publisher_id,
                        oname,
                        over,
                        input.to_string(),
                        deliver,
                        rel.manifest["service_mailbox"].as_str().unwrap_or(""),
                        now,
                        now + hours * 3600
                    ],
                )?;
                let w = event_in(tx, &jid, "nest.job.admitted", now)?;
                Ok((job_json_in(tx, &jid)?.unwrap_or(Value::Null), w))
            })
            .await?;
        if let Some(a) = wake_to {
            state.event_hub.wake(&a);
        }
        let s = state.clone();
        tokio::spawn(async move {
            dispatch_once(&s).await;
        });
        Ok((StatusCode::CREATED, Json(json!({ "data": v }))))
    }
    .await;
    into_response(result)
}

async fn own_job(state: &AppState, headers: &HeaderMap, job: &str) -> GResult<Value> {
    let (principal, account) = caller(state, headers).await?;
    let j = job.to_string();
    let found = state
        .store
        .group_read(move |c| -> GResult<Option<(String, Value)>> {
            let owner: Option<String> = c
                .query_row(
                    "SELECT account_id FROM nest_hosted_jobs WHERE id = ?1",
                    [&j],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(match owner {
                Some(o) => job_json_in(c, &j)?.map(|v| (o, v)),
                None => None,
            })
        })
        .await?;
    let f = Facts {
        job_visible: found.as_ref().is_some_and(|(o, _)| *o == account),
        ..facts(state)
    };
    authorize(
        &Actor::Principal(&principal),
        Action::NestJobRead,
        &Resource::NestJob(job),
        &f,
    )
    .map_err(|d| deny(d, true))?;
    Ok(found.ok_or_else(not_found)?.1)
}

/// `GET /v1/marketplace/hosted-jobs/{job}`
async fn read_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(job): Path<String>,
) -> Response {
    into_response(
        own_job(&state, &headers, &job)
            .await
            .map(|v| Json(json!({ "data": v }))),
    )
}

/// `GET /v1/marketplace/hosted-jobs` — the caller's hosted jobs, newest first.
async fn list_jobs(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let result = async {
        let (principal, account) = caller(&state, &headers).await?;
        authorize(
            &Actor::Principal(&principal),
            Action::NestJobRead,
            &Resource::NestJob("list"),
            &Facts {
                job_visible: true,
                ..facts(&state)
            },
        )
        .map_err(|d| deny(d, false))?;
        let rows = state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> {
                let mut stmt = c.prepare(
                    "SELECT id FROM nest_hosted_jobs WHERE account_id = ?1
                      ORDER BY created_at DESC, id LIMIT 100",
                )?;
                let ids = stmt
                    .query_map([&account], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let mut out = Vec::new();
                for id in ids {
                    if let Some(v) = job_json_in(c, &id)? {
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

/// `GET /v1/marketplace/hosted-jobs/{job}/files/{index}` — a result file, to the job's account.
async fn download(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((job, index)): Path<(String, i64)>,
) -> Response {
    let result = async {
        own_job(&state, &headers, &job).await?;
        let blobs = state.blobs.clone().ok_or_else(not_found)?;
        let j = job.clone();
        let (name, media, sha): (String, String, String) = state
            .store
            .group_read(move |c| -> GResult<Option<(String, String, String)>> {
                Ok(c.query_row(
                    "SELECT name, media_type, sha256 FROM nest_hosted_job_files
                      WHERE job_id = ?1 AND idx = ?2",
                    params![j, index],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?)
            })
            .await?
            .ok_or_else(not_found)?;
        let bytes = blobs.get(&sha).ok_or_else(not_found)?;
        let mut resp = (StatusCode::OK, bytes).into_response();
        let h = resp.headers_mut();
        h.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(crate::blobs::safe_content_type(&media)),
        );
        if let Ok(v) = HeaderValue::from_str(&format!(
            "attachment; filename=\"{}\"",
            crate::blobs::safe_filename(&name)
        )) {
            h.insert(header::CONTENT_DISPOSITION, v);
        }
        h.insert(
            header::HeaderName::from_static("x-content-type-options"),
            HeaderValue::from_static("nosniff"),
        );
        Ok(resp)
    }
    .await;
    into_response(result)
}

// ---- dispatch -----------------------------------------------------------------------------------

fn backoff(attempts: i64) -> i64 {
    (60_i64 << attempts.clamp(0, 6)).min(3600)
}

/// Hand queued jobs to their services. A send that fails is retried with backoff; a job whose
/// service mailbox is no longer the publisher's, or whose integration was stopped, fails.
pub(crate) async fn dispatch_once(state: &AppState) -> usize {
    let now = now_unix() as i64;
    type Due = (String, String, String, String, String, String, i64, i64);
    let due: Vec<Due> = match state
        .store
        .group_read(move |c| -> GResult<Vec<Due>> {
            let mut stmt = c.prepare(
                "SELECT id, listing_id, operation, input, service_mailbox, release_id, deadline_at,
                        attempts
                   FROM nest_hosted_jobs WHERE state = 'queued' AND next_attempt_at <= ?1
                  ORDER BY next_attempt_at LIMIT 50",
            )?;
            let rows = stmt
                .query_map([now], |r| {
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
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    {
        Ok(d) => d,
        Err(_) => return 0,
    };
    if due.is_empty() {
        return 0;
    }
    let from = match platform_mailbox(state).await {
        Ok(f) => f,
        Err(_) => {
            tracing::warn!("hosted marketplace jobs waiting: no marketplace mailbox");
            return 0;
        }
    };
    let mut n = 0;
    for (job, listing, op, input, mailbox, release, deadline, attempts) in due {
        let (r, m) = (release.clone(), mailbox.clone());
        let live = state
            .store
            .group_read(move |c| -> GResult<bool> {
                let rel = release_in(c, &r)?.ok_or_else(not_found)?;
                Ok(publishable_in(c, &rel).is_ok()
                    && rel.manifest["service_mailbox"].as_str() == Some(m.as_str()))
            })
            .await
            .unwrap_or(false);
        if !live {
            finish(state, &job, "failed", Some("service_unavailable"), now).await;
            continue;
        }
        let input: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
        let body = job_message(&state.public_url, &job, &listing, &op, &input, deadline);
        let extras = crate::msgactions::SendExtras {
            msg_ref: Some(msg_ref_for(&job, "dispatch")),
            ..Default::default()
        };
        let sent = crate::do_send_with(
            state,
            &from,
            &mailbox,
            &body,
            crate::ThreadRequest::Default,
            &extras,
            false,
        )
        .await;
        let (jid, retry_at) = (job.clone(), now + backoff(attempts));
        let outcome = match &sent {
            Ok(v) => Ok(v["message_id"].as_str().unwrap_or_default().to_string()),
            Err(e) => Err(e.code),
        };
        let _ = state
            .store
            .group_tx(move |tx| -> GResult<Option<String>> {
                match outcome {
                    Ok(message) => {
                        tx.execute(
                            "UPDATE nest_hosted_jobs SET state = 'running', dispatch_message = ?2,
                                attempts = attempts + 1, last_error = NULL,
                                revision = revision + 1, updated_at = ?3
                              WHERE id = ?1 AND state = 'queued'",
                            params![jid, message, now],
                        )?;
                        event_in(tx, &jid, "nest.job.progress", now)
                    }
                    Err(code) => {
                        tx.execute(
                            "UPDATE nest_hosted_jobs SET attempts = attempts + 1, last_error = ?2,
                                next_attempt_at = ?3, updated_at = ?4
                              WHERE id = ?1 AND state = 'queued'",
                            params![jid, code, retry_at, now],
                        )?;
                        Ok(None)
                    }
                }
            })
            .await
            .map(|w| {
                if let Some(a) = w {
                    state.event_hub.wake(&a);
                }
            });
        if sent.is_ok() {
            n += 1;
        } else if let Err(e) = &sent {
            tracing::warn!(job, code = e.code, "hosted job not dispatched; will retry");
        }
    }
    n
}

/// End a job without a result (failed, expired): the buyer is told by message.
async fn finish(
    state: &AppState,
    job: &str,
    st: &'static str,
    code: Option<&'static str>,
    now: i64,
) {
    let j = job.to_string();
    let w = state
        .store
        .group_tx(move |tx| -> GResult<Option<String>> {
            let changed = tx.execute(
                "UPDATE nest_hosted_jobs SET state = ?2, error_code = ?3, finished_at = ?4,
                    delivery = 'pending', revision = revision + 1, updated_at = ?4
                  WHERE id = ?1 AND state IN ('queued','running')",
                params![j, st, code, now],
            )?;
            if changed == 0 {
                return Ok(None);
            }
            event_in(tx, &j, "nest.job.progress", now)
        })
        .await;
    if let Ok(Some(a)) = w {
        state.event_hub.wake(&a);
    }
}

/// Jobs past their deadline with no result expire.
pub(crate) async fn expire_once(state: &AppState) -> usize {
    let now = now_unix() as i64;
    let due: Vec<String> = state
        .store
        .group_read(move |c| -> GResult<Vec<String>> {
            let mut stmt = c.prepare(
                "SELECT id FROM nest_hosted_jobs WHERE state IN ('queued','running')
                   AND deadline_at <= ?1 LIMIT 100",
            )?;
            let rows = stmt
                .query_map([now], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
        .unwrap_or_default();
    for j in &due {
        finish(state, j, "expired", Some("deadline_passed"), now).await;
    }
    due.len()
}

// ---- results ------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileReq {
    name: String,
    media_type: String,
    content_b64: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultReq {
    status: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    files: Vec<FileReq>,
}

/// Standard base64 (RFC 4648 §4), padding optional, whitespace ignored.
pub(crate) fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut buf, mut bits) = (0u32, 0u32);
    let mut ended = false;
    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                ended = true;
                continue;
            }
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        if ended {
            return None;
        }
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

/// A file a buyer can open as the operation's declared output.
fn file_ok(media_type: &str, bytes: &[u8]) -> bool {
    match media_type {
        "application/pdf" => {
            bytes.starts_with(b"%PDF-")
                && bytes[bytes.len().saturating_sub(1024)..]
                    .windows(5)
                    .any(|w| w == b"%%EOF")
        }
        _ => false,
    }
}

/// `POST /v1/nest-jobs/{job}/result` — the listing's service mailbox reports the job's outcome,
/// authenticated with that mailbox's own credential (its capability token, or an account token
/// with `x-pigeonpost-identity` naming it). Anyone else gets `404`. The same result again answers
/// with the stored job; a different one after a result is `409 result_recorded`.
async fn post_result(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(job): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let me = crate::acting_identity(
            &state,
            &headers,
            headers
                .get("x-pigeonpost-identity")
                .and_then(|v| v.to_str().ok()),
        )
        .await?;
        flag_gate(&state).await?;
        let j = job.clone();
        let row: Option<(String, String, String)> = state
            .store
            .group_read(move |c| -> GResult<Option<(String, String, String)>> {
                Ok(c.query_row(
                    "SELECT service_mailbox, state, COALESCE(result_digest, '')
                       FROM nest_hosted_jobs WHERE id = ?1",
                    [&j],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?)
            })
            .await?;
        let Some((mailbox, st, recorded)) = row else {
            return Err(not_found());
        };
        // The service mailbox's own credential, never a connector (policy `NestOperate`).
        let principal = crate::principal_for_token(&state, crate::bearer(&headers)).await?;
        authorize(
            &Actor::Principal(&principal),
            Action::NestOperate,
            &Resource::InstalledNest(&job),
            &Facts {
                nest_permitted: mailbox == me.address,
                ..facts(&state)
            },
        )
        .map_err(|d| deny(d, true))?;
        let req: ResultReq = parse(body)?;
        if !matches!(req.status.as_str(), "succeeded" | "failed") {
            return Err(GErr::bad("invalid_request", "status: succeeded or failed"));
        }
        let summary = match &req.summary {
            Some(s) if s.chars().count() > 4000 => {
                return Err(GErr::bad(
                    "invalid_request",
                    "summary: at most 4000 characters",
                ))
            }
            Some(s) => Some(
                s.chars()
                    .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
                    .collect::<String>()
                    .trim()
                    .to_string(),
            ),
            None => None,
        };
        if req.files.len() > MAX_FILES {
            return Err(GErr::bad("invalid_request", "files: at most 5"));
        }
        if req.status == "succeeded" && req.files.is_empty() {
            return Err(GErr::bad(
                "invalid_request",
                "a succeeded job returns its file",
            ));
        }
        let mut files = Vec::new();
        for (i, f) in req.files.iter().enumerate() {
            if !OUTPUT_MEDIA_TYPES.contains(&f.media_type.as_str()) {
                return Err(GErr::new(
                    StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    "unsupported_media_type",
                    format!("files[{i}].media_type: application/pdf"),
                ));
            }
            let bytes = base64_decode(&f.content_b64)
                .ok_or_else(|| GErr::bad("invalid_request", format!("files[{i}]: not base64")))?;
            if bytes.is_empty() || bytes.len() > MAX_FILE_BYTES {
                return Err(GErr::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "file_too_large",
                    format!("files[{i}]: 1 byte to 20 MB"),
                ));
            }
            if !file_ok(&f.media_type, &bytes) {
                return Err(GErr::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "file_unreadable",
                    format!("files[{i}]: not a PDF"),
                ));
            }
            files.push((
                crate::blobs::safe_filename(&f.name),
                f.media_type.clone(),
                bytes,
            ));
        }
        let shas: Vec<String> = files.iter().map(|(_, _, b)| digest_of(b)).collect();
        let digest = digest_of(
            canonical_json(&json!({
                "status": req.status, "summary": summary,
                "files": files.iter().zip(&shas).map(|((n, m, _), s)| json!([n, m, s]))
                    .collect::<Vec<_>>(),
            }))
            .as_bytes(),
        );
        let replay_or_conflict = |recorded: &str| -> GResult<()> {
            if recorded == digest {
                Ok(())
            } else {
                Err(conflict_code(
                    "result_recorded",
                    "this job already has a different result",
                ))
            }
        };
        match st.as_str() {
            "succeeded" | "failed" if !recorded.is_empty() => {
                replay_or_conflict(&recorded)?;
                let j = job.clone();
                let v = state
                    .store
                    .group_read(move |c| -> GResult<Value> {
                        Ok(job_json_in(c, &j)?.unwrap_or(Value::Null))
                    })
                    .await?;
                return Ok((StatusCode::OK, Json(json!({ "data": v, "replay": true }))));
            }
            "queued" | "running" => {}
            _ => return Err(conflict_code("job_closed", "this job is no longer open")),
        }
        let blobs = state.blobs.clone().ok_or_else(|| {
            GErr::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "storage_unavailable",
                "this postbox stores no files",
            )
        })?;
        let mut stored = Vec::new();
        for (name, media, bytes) in &files {
            let sha = blobs.put(bytes).map_err(|_| {
                GErr::new(
                    StatusCode::INSUFFICIENT_STORAGE,
                    "storage_full",
                    "this postbox has no room for the result",
                )
            })?;
            stored.push((name.clone(), media.clone(), bytes.len() as i64, sha));
        }
        let now = now_unix() as i64;
        let (j, status) = (job.clone(), req.status.clone());
        let (v, w) = state
            .store
            .group_tx(move |tx| -> GResult<(Value, Option<String>)> {
                let changed = tx.execute(
                    "UPDATE nest_hosted_jobs SET state = ?2, summary = ?3, result_digest = ?4,
                        error_code = CASE WHEN ?2 = 'failed' THEN 'publisher_failed' ELSE NULL END,
                        delivery = 'pending', finished_at = ?5, revision = revision + 1,
                        updated_at = ?5
                      WHERE id = ?1 AND state IN ('queued','running')",
                    params![j, status, summary, digest, now],
                )?;
                if changed == 0 {
                    return Err(conflict_code("job_closed", "this job is no longer open"));
                }
                for (i, (name, media, size, sha)) in stored.iter().enumerate() {
                    tx.execute(
                        "INSERT INTO nest_hosted_job_files (job_id, idx, name, media_type, bytes,
                            sha256, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![j, i as i64, name, media, size, sha, now],
                    )?;
                }
                let kind = if status == "succeeded" {
                    "nest.job.result_ready"
                } else {
                    "nest.job.progress"
                };
                let w = event_in(tx, &j, kind, now)?;
                Ok((job_json_in(tx, &j)?.unwrap_or(Value::Null), w))
            })
            .await?;
        if let Some(a) = w {
            state.event_hub.wake(&a);
        }
        let s = state.clone();
        tokio::spawn(async move {
            deliver_once(&s).await;
        });
        Ok((StatusCode::OK, Json(json!({ "data": v, "replay": false }))))
    }
    .await;
    into_response(result)
}

// ---- delivery to the buyer ----------------------------------------------------------------------

/// The message a buyer gets when a job ends. The summary is the publisher's text.
fn buyer_message(title: &str, job: &str, st: &str, summary: Option<&str>, files: usize) -> String {
    let head = match st {
        "succeeded" => format!("{title}: your result for job {job} is ready."),
        "expired" => format!("{title}: job {job} ran out of time and has no result."),
        _ => format!("{title}: job {job} could not be completed."),
    };
    let mut out = head;
    if let Some(s) = summary.filter(|s| !s.is_empty()) {
        out.push_str("\n\n");
        out.push_str(s);
    }
    if files > 0 {
        out.push_str(
            "\n\nThe file is attached, and it is on your account page under Services → Jobs.",
        );
    }
    out.push_str("\n\nThis service is run by its publisher, not by Pigeonpost.");
    out
}

/// Send finished jobs' outcomes to their buyers, from the marketplace mailbox, with the files
/// attached. A refusal is retried; after the last attempt the result stays on the job.
pub(crate) async fn deliver_once(state: &AppState) -> usize {
    type Due = (String, String, String, Option<String>, String, i64);
    let due: Vec<Due> = state
        .store
        .group_read(move |c| -> GResult<Vec<Due>> {
            let mut stmt = c.prepare(
                "SELECT j.id, j.deliver_to, j.state, j.summary, l.title, j.delivery_attempts
                   FROM nest_hosted_jobs j JOIN nest_listings l ON l.id = j.listing_id
                  WHERE j.delivery = 'pending' LIMIT 50",
            )?;
            let rows = stmt
                .query_map([], |r| {
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
        })
        .await
        .unwrap_or_default();
    if due.is_empty() {
        return 0;
    }
    let Ok(from) = platform_mailbox(state).await else {
        return 0;
    };
    let mut n = 0;
    for (job, to, st, summary, title, attempts) in due {
        let j = job.clone();
        let files: Vec<(String, String, i64, String)> = state
            .store
            .group_read(move |c| -> GResult<Vec<(String, String, i64, String)>> {
                let mut stmt = c.prepare(
                    "SELECT name, media_type, bytes, sha256 FROM nest_hosted_job_files
                      WHERE job_id = ?1 ORDER BY idx",
                )?;
                let rows = stmt
                    .query_map([&j], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await
            .unwrap_or_default();
        let mut ids = Vec::new();
        for (name, media, size, sha) in &files {
            let id = format!("att_{}", crate::rand_hex(12));
            if state
                .store
                .add_attachment(
                    id.clone(),
                    sha.clone(),
                    *size,
                    media.clone(),
                    name.clone(),
                    from.address.clone(),
                    now_unix(),
                )
                .await
                .is_ok()
            {
                ids.push(id);
            }
        }
        let body = buyer_message(&title, &job, &st, summary.as_deref(), files.len());
        let extras = crate::msgactions::SendExtras {
            msg_ref: Some(msg_ref_for(&job, "deliver")),
            ..Default::default()
        };
        let sent = crate::do_send_with(
            state,
            &from,
            &to,
            &body,
            crate::ThreadRequest::Default,
            &extras,
            !ids.is_empty(),
        )
        .await;
        let now = now_unix() as i64;
        let delivered = match &sent {
            Ok(v) => {
                if v["duplicate"].as_bool() != Some(true) && !ids.is_empty() {
                    let message = v["message_id"].as_str().unwrap_or_default().to_string();
                    let copy = v["sent_copy_id"].as_str().map(str::to_string);
                    let recipient = v["recipient"].as_str().unwrap_or_default().to_string();
                    let _ = state
                        .store
                        .attach_to_message(
                            ids.clone(),
                            from.address.clone(),
                            copy.unwrap_or_else(|| message.clone()),
                            Some((recipient, message)),
                            now as u64,
                        )
                        .await;
                }
                Some(v["message_id"].as_str().unwrap_or_default().to_string())
            }
            Err(e) => {
                tracing::warn!(
                    job,
                    code = e.code,
                    "hosted job result not delivered; will retry"
                );
                None
            }
        };
        let j = job.clone();
        let _ = state
            .store
            .group_tx(move |tx| -> GResult<()> {
                match delivered {
                    Some(m) => tx.execute(
                        "UPDATE nest_hosted_jobs SET delivery = 'delivered', delivery_message = ?2,
                            delivery_attempts = delivery_attempts + 1, updated_at = ?3 WHERE id = ?1",
                        params![j, m, now],
                    )?,
                    None => tx.execute(
                        "UPDATE nest_hosted_jobs SET delivery = CASE WHEN ?2 + 1 >= ?3
                            THEN 'undeliverable' ELSE 'pending' END,
                            delivery_attempts = delivery_attempts + 1, updated_at = ?4 WHERE id = ?1",
                        params![j, attempts, MAX_DELIVERIES, now],
                    )?,
                };
                Ok(())
            })
            .await;
        n += 1;
    }
    n
}

/// Every 20 seconds while the flag is on: dispatch, expire, deliver.
pub(crate) fn start_worker(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(20));
        loop {
            tick.tick().await;
            if flag_gate(&state).await.is_ok() {
                dispatch_once(&state).await;
                expire_once(&state).await;
                deliver_once(&state).await;
            }
        }
    });
}

// ---- staff ---------------------------------------------------------------------------------------

/// `GET /v1/staff/hosted-jobs?state=` — publisher-hosted jobs for support: ids, states, delivery
/// and error codes. Never the input or the result.
async fn staff_jobs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let result = async {
        staff_gate(&state, &headers, Action::NestReview, "hosted-jobs", None).await?;
        let filter = q.get("state").cloned();
        if filter.as_deref().is_some_and(|s| {
            ![
                "queued",
                "running",
                "succeeded",
                "failed",
                "expired",
                "cancelled",
            ]
            .contains(&s)
        }) {
            return Err(GErr::bad("invalid_request", "unknown state"));
        }
        let rows = state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> {
                let mut stmt = c.prepare(
                    "SELECT j.id, j.listing_id, l.title, j.publisher_id, j.operation, j.state,
                            j.delivery, j.error_code, j.last_error, j.attempts, j.created_at,
                            j.updated_at
                       FROM nest_hosted_jobs j JOIN nest_listings l ON l.id = j.listing_id
                      WHERE ?1 IS NULL OR j.state = ?1
                      ORDER BY j.created_at DESC LIMIT 100",
                )?;
                let rows = stmt
                    .query_map([&filter], |r| {
                        Ok(json!({
                            "job_id": r.get::<_, String>(0)?,
                            "listing_id": r.get::<_, String>(1)?,
                            "listing_title": r.get::<_, String>(2)?,
                            "publisher_id": r.get::<_, String>(3)?,
                            "operation": r.get::<_, String>(4)?,
                            "execution": r.get::<_, String>(5)?,
                            "delivery": r.get::<_, Option<String>>(6)?,
                            "error_code": r.get::<_, Option<String>>(7)?,
                            "last_dispatch_error": r.get::<_, Option<String>>(8)?,
                            "dispatch_attempts": r.get::<_, i64>(9)?,
                            "created_at": iso(r.get(10)?),
                            "updated_at": iso(r.get(11)?),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_the_standard_alphabet() {
        assert_eq!(base64_decode("JVBERi0=").unwrap(), b"%PDF-");
        assert_eq!(base64_decode("JVBERi0").unwrap(), b"%PDF-");
        assert_eq!(base64_decode("aGVsbG8gd29ybGQ=").unwrap(), b"hello world");
        assert!(base64_decode("a*b").is_none());
        assert!(base64_decode("aa=a").is_none());
    }

    #[test]
    fn only_a_real_pdf_passes() {
        assert!(file_ok("application/pdf", b"%PDF-1.7\n...\n%%EOF\n"));
        assert!(!file_ok("application/pdf", b"<html>%%EOF"));
        assert!(!file_ok("application/pdf", b"%PDF-1.7 truncated"));
        assert!(!file_ok("text/html", b"%PDF-1.7\n%%EOF"));
    }

    #[test]
    fn the_job_message_has_exactly_the_contract_fields() {
        let body = job_message(
            "https://postbox.pigeonpost.dev/",
            "job_abc",
            "lst_x",
            "hr.research",
            &json!({ "role": "CTO" }),
            1_800_000_000,
        );
        let v: Value = serde_json::from_str(&body).unwrap();
        let keys: BTreeSet<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            BTreeSet::from([
                "type",
                "job_id",
                "listing_id",
                "operation",
                "input",
                "result_url",
                "deadline"
            ])
        );
        assert_eq!(v["type"], JOB_MESSAGE_TYPE);
        assert_eq!(
            v["result_url"],
            "https://postbox.pigeonpost.dev/v1/nest-jobs/job_abc/result"
        );
        assert_eq!(v["deadline"], "2027-01-15T08:00:00Z");
    }

    #[test]
    fn msg_refs_are_stable_valid_and_differ_by_purpose() {
        let a = msg_ref_for("job_a", "dispatch");
        assert_eq!(a, msg_ref_for("job_a", "dispatch"));
        assert_ne!(a, msg_ref_for("job_a", "deliver"));
        assert_ne!(a, msg_ref_for("job_b", "dispatch"));
        assert_eq!(a.len(), 5 + 26);
        assert!(a[5..]
            .bytes()
            .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b)));
        assert_eq!(backoff(0), 60);
        assert_eq!(backoff(10), 3600);
    }
}
