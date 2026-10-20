//! The billing bridge's internal endpoints (SDS §8.5, §8.8, §21.2): provider-bound entitlement
//! upserts, a complimentary-grant convenience, and minimal org drafts/activation. Served only on
//! the internal listener; every route requires the `billing` service token.
// Handlers return ready `Response`s as their error type, like the rest of the router.
#![allow(clippy::result_large_err)]
use crate::internal_api::{authorize_service, Endpoint};
use crate::policy::Action;
use crate::store::{
    ActivateOutcome, DraftOutcome, Entitlement, EntitlementInput, Org, OrgDraft, UpsertOutcome,
};
use crate::{audit::ActorKind, now_unix, record_audit, ApiError, AppState};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

type Body<T> = Result<Json<T>, JsonRejection>;

const STATUSES: &[&str] = &["active", "past_due", "grace", "expired", "canceled"];
const ENVIRONMENTS: &[&str] = &["live", "test", "sandbox"];
const MAX_SEATS: i64 = 100_000;

fn ascii_matches(value: &str, first: fn(char) -> bool, rest: fn(char) -> bool, max: usize) -> bool {
    let mut chars = value.chars();
    chars.next().is_some_and(first) && value.len() <= max && chars.all(rest)
}

pub(crate) fn valid_reference(v: &str) -> bool {
    ascii_matches(
        v,
        |c| c.is_ascii_alphanumeric(),
        |c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'),
        128,
    )
}
fn valid_plan_code(v: &str) -> bool {
    ascii_matches(
        v,
        |c| c.is_ascii_lowercase() || c.is_ascii_digit(),
        |c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-'),
        64,
    )
}
fn valid_plan_version(v: &str) -> bool {
    ascii_matches(
        v,
        |c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'),
        |c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'),
        32,
    )
}
fn valid_operation(v: &str) -> bool {
    (16..=128).contains(&v.len())
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}
fn valid_id(v: &str) -> bool {
    ascii_matches(
        v,
        |c| c.is_ascii_alphanumeric(),
        |c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'),
        128,
    )
}

fn bad(code: &'static str, detail: &str) -> Response {
    ApiError::bad(code, detail).into_response()
}

fn store_error(e: impl std::fmt::Display) -> Response {
    tracing::error!(error = %e, "billing internal store error");
    ApiError::server("store_error").into_response()
}

fn guard(
    state: &AppState,
    headers: &HeaderMap,
    endpoint: Endpoint,
) -> Result<&'static str, Response> {
    authorize_service(state, headers, endpoint, Action::ManageBilling).map_err(|r| *r)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubjectBody {
    kind: String,
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubjectCheck {
    oidc_sub: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EntitlementBody {
    subject: SubjectBody,
    #[serde(default)]
    subject_check: Option<SubjectCheck>,
    plan_code: String,
    plan_version: String,
    #[serde(default)]
    names: Option<i64>,
    #[serde(default)]
    seats: Option<i64>,
    #[serde(default)]
    agent_limit: Option<i64>,
    #[serde(default)]
    storage_bytes: Option<i64>,
    status: String,
    #[serde(default)]
    period_end: Option<i64>,
    #[serde(default)]
    paid_through: Option<i64>,
    #[serde(default)]
    cancel_requested_at: Option<i64>,
    #[serde(default)]
    pending_plan_code: Option<String>,
    #[serde(default)]
    pending_effective_at: Option<i64>,
    reconciliation_version: i64,
    #[serde(default)]
    operation_id: Option<String>,
}

/// The entitlement object nested in the org activate/entitlement bodies.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OrgEntitlementBody {
    provider: String,
    environment: String,
    reference: String,
    plan_code: String,
    plan_version: String,
    seats: i64,
    #[serde(default)]
    agent_limit: Option<i64>,
    #[serde(default)]
    storage_bytes: Option<i64>,
    status: String,
    #[serde(default)]
    period_end: Option<i64>,
    #[serde(default)]
    paid_through: Option<i64>,
    #[serde(default)]
    cancel_requested_at: Option<i64>,
    #[serde(default)]
    pending_plan_code: Option<String>,
    #[serde(default)]
    pending_effective_at: Option<i64>,
    reconciliation_version: i64,
}

fn check_provider(provider: &str, environment: &str, reference: &str) -> Result<(), Response> {
    match provider {
        "iyzico" | "grant" | "test" => {}
        "apple" | "google" => {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "provider_reserved",
                "store entitlements are written only by the postbox's own verified reconcilers",
            )
            .into_response())
        }
        _ => {
            return Err(bad(
                "invalid_provider",
                "provider must be iyzico, grant or test",
            ))
        }
    }
    if !ENVIRONMENTS.contains(&environment) {
        return Err(bad(
            "invalid_environment",
            "environment must be live, test or sandbox",
        ));
    }
    if !valid_reference(reference) {
        return Err(bad(
            "invalid_reference",
            "reference has invalid characters or length",
        ));
    }
    Ok(())
}

/// Shared validation for every entitlement written through this API.
fn validate(input: &EntitlementInput) -> Result<(), Response> {
    if !valid_plan_code(&input.plan_code) {
        return Err(bad(
            "invalid_plan_code",
            "plan_code must match ^[a-z0-9][a-z0-9.-]{0,63}$",
        ));
    }
    if !valid_plan_version(&input.plan_version) {
        return Err(bad(
            "invalid_plan_version",
            "plan_version must be 1-32 of [A-Za-z0-9._-]",
        ));
    }
    if input
        .pending_plan_code
        .as_deref()
        .is_some_and(|p| !valid_plan_code(p))
    {
        return Err(bad(
            "invalid_plan_code",
            "pending_plan_code is not a plan code",
        ));
    }
    if !STATUSES.contains(&input.status.as_str()) {
        return Err(bad(
            "invalid_status",
            "status must be active, past_due, grace, expired or canceled",
        ));
    }
    if input.reconciliation_version <= 0 {
        return Err(bad(
            "invalid_version",
            "reconciliation_version must be positive",
        ));
    }
    for value in [
        input.period_end,
        input.paid_through,
        input.cancel_requested_at,
        input.pending_effective_at,
        input.storage_bytes,
    ]
    .into_iter()
    .flatten()
    {
        if value < 0 {
            return Err(bad(
                "invalid_value",
                "timestamps and sizes must not be negative",
            ));
        }
    }
    if input.paid_through.is_none() && input.source != "grant" && input.status != "expired" {
        return Err(bad(
            "paid_through_required",
            "only a complimentary grant may have no paid_through",
        ));
    }
    match input.subject_kind.as_str() {
        "account" => {
            if input.seats.is_some() || input.agent_limit.is_some() {
                return Err(bad(
                    "invalid_subject",
                    "account entitlements carry names, not seats",
                ));
            }
            match input.names {
                Some(n) if (1..=10).contains(&n) => {}
                Some(0) if input.source == "grant" && input.status == "expired" => {}
                _ => return Err(bad("invalid_names", "names must be 1 to 10")),
            }
        }
        "org" => {
            if input.names.is_some() {
                return Err(bad(
                    "invalid_subject",
                    "org entitlements carry seats, not names",
                ));
            }
            let Some(seats) = input.seats.filter(|s| (1..=MAX_SEATS).contains(s)) else {
                return Err(bad("invalid_seats", "seats must be 1 to 100000"));
            };
            if input.agent_limit != Some(seats * 10) {
                return Err(bad(
                    "invalid_agent_limit",
                    "agent_limit must equal seats × 10",
                ));
            }
        }
        _ => {
            return Err(bad(
                "invalid_subject",
                "subject kind must be account or org",
            ))
        }
    }
    if !valid_id(&input.subject_id) {
        return Err(bad("invalid_subject", "subject id has invalid characters"));
    }
    if input
        .expected_oidc_sub
        .as_deref()
        .is_some_and(|s| s.is_empty() || s.len() > 255)
    {
        return Err(bad(
            "invalid_subject_check",
            "oidc_sub must be 1 to 255 characters",
        ));
    }
    Ok(())
}

fn org_input(e: OrgEntitlementBody, org: &str) -> Result<EntitlementInput, Response> {
    check_provider(&e.provider, &e.environment, &e.reference)?;
    let input = EntitlementInput {
        source: e.provider,
        environment: e.environment,
        reference: e.reference,
        subject_kind: "org".into(),
        subject_id: org.into(),
        plan_code: e.plan_code,
        plan_version: e.plan_version,
        names: None,
        seats: Some(e.seats),
        agent_limit: Some(e.agent_limit.unwrap_or(e.seats.saturating_mul(10))),
        storage_bytes: e.storage_bytes,
        status: e.status,
        period_end: e.period_end,
        paid_through: e.paid_through,
        cancel_requested_at: e.cancel_requested_at,
        pending_plan_code: e.pending_plan_code,
        pending_effective_at: e.pending_effective_at,
        reconciliation_version: e.reconciliation_version,
        expected_oidc_sub: None,
    };
    validate(&input)?;
    Ok(input)
}

async fn capacity_json(state: &AppState, e: &Entitlement) -> Value {
    if e.subject_kind != "account" {
        return Value::Null;
    }
    match state.store.web_plan(e.subject_id.clone(), now_unix()).await {
        Ok(plan) => json!({ "names": plan.capacity, "used": plan.used }),
        Err(_) => Value::Null,
    }
}

/// Upsert, audit an applied change, and report `(applied, reason, entitlement)`.
async fn do_upsert(
    state: &AppState,
    headers: &HeaderMap,
    service: &str,
    input: EntitlementInput,
    operation: Option<String>,
) -> Result<(bool, Option<&'static str>, Entitlement), Response> {
    let target = format!(
        "entitlement:{}/{}/{}",
        input.source, input.environment, input.reference
    );
    let details = json!({
        "service": service,
        "subject_kind": input.subject_kind,
        "subject_id": input.subject_id,
        "plan_code": input.plan_code,
        "plan_version": input.plan_version,
        "status": input.status,
        "paid_through": input.paid_through,
        "version": input.reconciliation_version,
        "operation_id": operation,
    });
    let outcome = state
        .store
        .upsert_entitlement(input, now_unix())
        .await
        .map_err(store_error)?;
    match outcome {
        UpsertOutcome::Applied(e) => {
            record_audit(
                state,
                ActorKind::System,
                None,
                "entitlement.upsert",
                Some(target),
                details,
                None,
                headers,
            )
            .await;
            Ok((true, None, e))
        }
        UpsertOutcome::Stale(e) => Ok((false, Some("stale"), e)),
        UpsertOutcome::Duplicate(e) => Ok((false, Some("duplicate"), e)),
        other => Err(outcome_error(other)),
    }
}

async fn upsert_response(
    state: &AppState,
    headers: &HeaderMap,
    service: &str,
    input: EntitlementInput,
    operation: Option<String>,
) -> Response {
    match do_upsert(state, headers, service, input, operation).await {
        Ok((applied, reason, e)) => {
            let capacity = capacity_json(state, &e).await;
            let mut body = json!({ "applied": applied, "entitlement": e, "capacity": capacity });
            if let Some(reason) = reason {
                body["reason"] = json!(reason);
            }
            Json(body).into_response()
        }
        Err(r) => r,
    }
}

/// `PUT /internal/v1/entitlements/{provider}/{environment}/{reference}`
pub(crate) async fn put_entitlement(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((provider, environment, reference)): Path<(String, String, String)>,
    body: Body<EntitlementBody>,
) -> Response {
    let service = match guard(&state, &headers, Endpoint::Entitlements) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(r) = check_provider(&provider, &environment, &reference) {
        return r;
    }
    let Ok(Json(b)) = body else {
        return bad(
            "bad_request",
            "expected an entitlement body (see the internal API contract)",
        );
    };
    let input = EntitlementInput {
        source: provider,
        environment,
        reference,
        subject_kind: b.subject.kind,
        subject_id: b.subject.id,
        plan_code: b.plan_code,
        plan_version: b.plan_version,
        names: b.names,
        seats: b.seats,
        agent_limit: b.agent_limit,
        storage_bytes: b.storage_bytes,
        status: b.status,
        period_end: b.period_end,
        paid_through: b.paid_through,
        cancel_requested_at: b.cancel_requested_at,
        pending_plan_code: b.pending_plan_code,
        pending_effective_at: b.pending_effective_at,
        reconciliation_version: b.reconciliation_version,
        expected_oidc_sub: b.subject_check.map(|c| c.oidc_sub),
    };
    if input.expected_oidc_sub.is_some() && input.subject_kind != "account" {
        return bad(
            "invalid_subject_check",
            "subject_check applies to accounts only",
        );
    }
    if let Err(r) = validate(&input) {
        return r;
    }
    upsert_response(&state, &headers, service, input, b.operation_id).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloudPlanBody {
    names: i64,
    #[serde(default)]
    paid_through: Option<i64>,
    reconciliation_version: i64,
    #[serde(default)]
    note: Option<String>,
}

/// `PUT /internal/v1/accounts/{id}/cloud-plan` — a complimentary web plan of `names` names.
pub(crate) async fn put_cloud_plan(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Body<CloudPlanBody>,
) -> Response {
    let service = match guard(&state, &headers, Endpoint::Entitlements) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let Ok(Json(b)) = body else {
        return bad(
            "bad_request",
            "expected {names, paid_through, reconciliation_version, note?}",
        );
    };
    if !(0..=10).contains(&b.names) {
        return bad("invalid_names", "names must be 0 to 10");
    }
    if b.note.as_deref().is_some_and(|n| n.chars().count() > 200) {
        return bad("invalid_note", "note is at most 200 characters");
    }
    if !valid_id(&id) {
        return bad("invalid_subject", "account id has invalid characters");
    }
    let input = EntitlementInput {
        source: "grant".into(),
        environment: "live".into(),
        reference: format!("account:{id}"),
        subject_kind: "account".into(),
        subject_id: id,
        plan_code: format!("cloud-{}names-grant", b.names),
        plan_version: "grant".into(),
        names: Some(b.names),
        seats: None,
        agent_limit: None,
        storage_bytes: None,
        status: if b.names == 0 { "expired" } else { "active" }.into(),
        period_end: b.paid_through,
        paid_through: b.paid_through,
        cancel_requested_at: None,
        pending_plan_code: None,
        pending_effective_at: None,
        reconciliation_version: b.reconciliation_version,
        expected_oidc_sub: None,
    };
    if let Err(r) = validate(&input) {
        return r;
    }
    upsert_response(&state, &headers, service, input, b.note).await
}

/// `GET /internal/v1/accounts/{id}/entitlements`
pub(crate) async fn account_entitlements(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = guard(&state, &headers, Endpoint::Entitlements) {
        return r;
    }
    match state.store.account_exists(id.clone()).await {
        Ok(true) => {}
        Ok(false) => {
            return ApiError::new(StatusCode::NOT_FOUND, "subject_unknown", "no such account")
                .into_response()
        }
        Err(e) => return store_error(e),
    }
    let now = now_unix();
    let (list, plan) = match (
        state
            .store
            .entitlements_for("account", id.clone(), now)
            .await,
        state.store.web_plan(id.clone(), now).await,
    ) {
        (Ok(l), Ok(p)) => (l, p),
        (Err(e), _) | (_, Err(e)) => return store_error(e),
    };
    Json(json!({
        "account": id,
        "entitlements": list,
        "capacity": { "names": plan.capacity, "used": plan.used },
        "web_plan": plan,
    }))
    .into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DraftBody {
    operation_id: String,
    owner_account: String,
    name: String,
    namespace: String,
    plan_code: String,
    seats: i64,
}

/// `ent-<seats>-<monthly|yearly>-<usd|try|eur>`, with the seat count matching `seats`.
fn valid_ent_plan(code: &str, seats: i64) -> bool {
    let parts: Vec<&str> = code.split('-').collect();
    parts.len() == 4
        && parts[0] == "ent"
        && parts[1].parse::<i64>().ok() == Some(seats)
        && !parts[1].starts_with('0')
        && ["monthly", "yearly"].contains(&parts[2])
        && ["usd", "try", "eur"].contains(&parts[3])
}

fn org_json(org: &Org) -> Value {
    json!(org)
}

/// `POST /internal/v1/org-drafts`
pub(crate) async fn create_org_draft(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body<DraftBody>,
) -> Response {
    let service = match guard(&state, &headers, Endpoint::Orgs) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let Ok(Json(b)) = body else {
        return bad(
            "bad_request",
            "expected {operation_id, owner_account, name, namespace, plan_code, seats}",
        );
    };
    if !valid_operation(&b.operation_id) {
        return bad(
            "invalid_operation_id",
            "operation_id must be 16-128 of [A-Za-z0-9_-]",
        );
    }
    if !valid_id(&b.owner_account) {
        return bad("invalid_owner", "owner_account has invalid characters");
    }
    let name = b.name.trim().to_string();
    if name.is_empty() || name.chars().count() > 120 || name.chars().any(char::is_control) {
        return bad("invalid_name", "name must be 1 to 120 printable characters");
    }
    if !(1..=MAX_SEATS).contains(&b.seats) || !valid_ent_plan(&b.plan_code, b.seats) {
        return bad(
            "invalid_plan_code",
            "plan_code must be ent-<seats>-<monthly|yearly>-<usd|try|eur> matching seats",
        );
    }
    let namespace = match crate::web_plan::canonical_namespace(&state, &b.namespace) {
        Ok(n) => n,
        Err(crate::web_plan::NameRefusal::Invalid) => {
            return bad("invalid_namespace", "namespace has invalid characters")
        }
        Err(crate::web_plan::NameRefusal::Reserved) => {
            return ApiError::new(
                StatusCode::CONFLICT,
                "namespace_unavailable",
                "that namespace is reserved",
            )
            .into_response()
        }
        Err(crate::web_plan::NameRefusal::Unavailable) => {
            return ApiError::server("reserved_names_unavailable").into_response()
        }
    };
    let draft = OrgDraft {
        operation_id: b.operation_id.clone(),
        owner_account: b.owner_account,
        name,
        namespace,
        plan_code: b.plan_code,
        seats: b.seats,
    };
    match state.store.create_org_draft(draft, now_unix()).await {
        Ok(DraftOutcome::Created(org)) => {
            record_audit(
                &state,
                ActorKind::System,
                Some(org.owner_account.clone()),
                "org.draft",
                Some(format!("org:{}", org.id)),
                json!({
                    "service": service,
                    "namespace": org.namespace,
                    "plan_code": org.plan_code,
                    "seats": org.seats,
                    "operation_id": b.operation_id,
                }),
                None,
                &headers,
            )
            .await;
            (StatusCode::CREATED, Json(json!({ "org": org_json(&org) }))).into_response()
        }
        Ok(DraftOutcome::Replayed(org)) => Json(json!({ "org": org_json(&org) })).into_response(),
        Ok(DraftOutcome::OperationConflict) => ApiError::new(
            StatusCode::CONFLICT,
            "operation_conflict",
            "this operation_id was used for a different draft",
        )
        .into_response(),
        Ok(DraftOutcome::OwnerUnknown) => ApiError::new(
            StatusCode::NOT_FOUND,
            "subject_unknown",
            "no such owner account",
        )
        .into_response(),
        Ok(DraftOutcome::NamespaceUnavailable) => ApiError::new(
            StatusCode::CONFLICT,
            "namespace_unavailable",
            "that namespace is held by another account or org",
        )
        .into_response(),
        Err(e) => store_error(e),
    }
}

async fn org_with_entitlements(state: &AppState, id: &str) -> Response {
    let now = now_unix();
    match (
        state.store.org(id.to_string()).await,
        state
            .store
            .entitlements_for("org", id.to_string(), now)
            .await,
    ) {
        (Ok(Some(org)), Ok(list)) => {
            Json(json!({ "org": org_json(&org), "entitlements": list })).into_response()
        }
        (Ok(None), _) => {
            ApiError::new(StatusCode::NOT_FOUND, "org_unknown", "no such org").into_response()
        }
        (Err(e), _) | (_, Err(e)) => store_error(e),
    }
}

/// `GET /internal/v1/orgs/{id}`
pub(crate) async fn get_org(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = guard(&state, &headers, Endpoint::Orgs) {
        return r;
    }
    org_with_entitlements(&state, &id).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivateBody {
    operation_id: String,
    entitlement: OrgEntitlementBody,
}

/// `PUT /internal/v1/orgs/{id}/activate`
pub(crate) async fn activate_org(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Body<ActivateBody>,
) -> Response {
    let service = match guard(&state, &headers, Endpoint::Orgs) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let Ok(Json(b)) = body else {
        return bad("bad_request", "expected {operation_id, entitlement}");
    };
    if !valid_operation(&b.operation_id) {
        return bad(
            "invalid_operation_id",
            "operation_id must be 16-128 of [A-Za-z0-9_-]",
        );
    }
    let input = match org_input(b.entitlement, &id) {
        Ok(i) => i,
        Err(r) => return r,
    };
    let reference = input.reference.clone();
    match state
        .store
        .activate_org(id.clone(), input, now_unix())
        .await
    {
        Ok(ActivateOutcome::Done(org, entitlement)) => {
            record_audit(
                &state,
                ActorKind::System,
                Some(org.owner_account.clone()),
                "org.activate",
                Some(format!("org:{}", org.id)),
                json!({
                    "service": service,
                    "status": org.status,
                    "billing_ref": reference,
                    "operation_id": b.operation_id,
                }),
                None,
                &headers,
            )
            .await;
            Json(json!({ "org": org_json(&org), "entitlement": entitlement })).into_response()
        }
        Ok(ActivateOutcome::OrgUnknown) => {
            ApiError::new(StatusCode::NOT_FOUND, "org_unknown", "no such org").into_response()
        }
        Ok(ActivateOutcome::EntitlementInactive) => ApiError::new(
            StatusCode::CONFLICT,
            "entitlement_inactive",
            "activation needs a verified, unexpired paid entitlement",
        )
        .into_response(),
        Ok(ActivateOutcome::Entitlement(other)) => outcome_error(other),
        Err(e) => store_error(e),
    }
}

fn outcome_error(outcome: UpsertOutcome) -> Response {
    match outcome {
        UpsertOutcome::SubjectMismatch => ApiError::new(
            StatusCode::CONFLICT,
            "subject_mismatch",
            "this provider reference is bound to another subject, or the account's sign-in subject differs",
        )
        .into_response(),
        UpsertOutcome::VersionConflict => ApiError::new(
            StatusCode::CONFLICT,
            "version_conflict",
            "this reconciliation version was already applied with different content",
        )
        .into_response(),
        _ => ApiError::new(
            StatusCode::NOT_FOUND,
            "subject_unknown",
            "no such account or org",
        )
        .into_response(),
    }
}

/// `PUT /internal/v1/orgs/{id}/entitlement`
pub(crate) async fn put_org_entitlement(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Body<OrgEntitlementBody>,
) -> Response {
    let service = match guard(&state, &headers, Endpoint::Orgs) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let Ok(Json(b)) = body else {
        return bad("bad_request", "expected an org entitlement body");
    };
    let input = match org_input(b, &id) {
        Ok(i) => i,
        Err(r) => return r,
    };
    match do_upsert(&state, &headers, service, input, None).await {
        Ok((applied, reason, entitlement)) => match state.store.org(id).await {
            Ok(Some(org)) => {
                let mut body = json!({ "applied": applied, "org": org_json(&org), "entitlement": entitlement });
                if let Some(reason) = reason {
                    body["reason"] = json!(reason);
                }
                Json(body).into_response()
            }
            Ok(None) => {
                ApiError::new(StatusCode::NOT_FOUND, "org_unknown", "no such org").into_response()
            }
            Err(e) => store_error(e),
        },
        Err(r) => r,
    }
}
