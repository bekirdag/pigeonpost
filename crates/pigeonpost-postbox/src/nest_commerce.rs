//! Nests Phase B1: buying Nest packages and the slots they fund (build plan §8 B1).
//!
//! Member routes (all behind the `nests` flag, per account):
//!
//! | Route | Action | What |
//! |---|---|---|
//! | `GET /v1/nest-packages` | `NestView` | the catalog, which rails are available, the account's store binding tokens |
//! | `GET /v1/nests` | `NestView` | allowance (funding package, slots, duplicates), Nests, paid setups |
//! | `POST /v1/nests` | `NestOwn` | create a Nest in a free slot |
//! | `POST /v1/nest-purchases/intents` | `NestPurchase` | record the intent before a store sheet; refuses a second charge |
//! | `POST /v1/nest-purchases/apple` | `NestPurchase` | claim or restore an App Store Nest subscription |
//! | `POST /v1/nest-purchases/google` | `NestPurchase` | claim or restore a Google Play Nest subscription |
//! | `POST /v1/nest-setups/{id}/retry` | `NestPurchase` | retry a paid-but-unprovisioned setup now |
//!
//! Web (iyzico) purchases go through the billing bridge, which writes `nest-…` entitlements with
//! the internal API; everything after that (setup, slots, sweep) is the same as for the stores.
//!
//! # Rules
//!
//! - Access comes only from a provider-verified purchase mirrored into `entitlements`. The flag
//!   gates the feature; it never grants a slot. A client never says what it bought.
//! - One funding package per account. A second live package never adds slots; it is reported as a
//!   duplicate (Google duplicates are left unacknowledged, so Google refunds them itself).
//! - A verified purchase never disappears because setup failed: it becomes a durable setup that
//!   the reconciler retries and the owner can retry ("paid, setup pending").
//! - Renewals, grace, refunds and upgrades are re-read from the provider by the reconciler while
//!   the app is closed. A refund or expiry pauses the Nests at once (no unfunded compute); after a
//!   30-day export window without funding they are deleted. A downgrade pauses the newest excess
//!   Nests and never deletes them.

use crate::groups::{GErr, GResult};
use crate::nest_catalog::{self, Package};
use crate::policy::{authorize, Action, Actor, Facts, Resource};
use crate::store::nest_funding::{self, FundingAction, IntentOutcome, StoreClaim, StorePurchase};
use crate::{
    appstore, capabilities, googleplay, now_unix, principal_for_token, AppState, Principal,
};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/nest-packages", get(packages))
        .route("/v1/nests", get(list).post(create))
        .route(
            "/v1/nests/{nest}",
            get(one)
                .patch(crate::nest_manage::rename)
                .delete(crate::nest_manage::remove),
        )
        .route("/v1/nest-purchases/intents", post(intent))
        .route("/v1/nest-purchases/apple", post(claim_apple))
        .route("/v1/nest-purchases/google", post(claim_google))
        .route("/v1/nest-setups/{id}/retry", post(retry_setup))
}

// ---- auth -----------------------------------------------------------------------------------------

/// Resolve the caller's account and authorize `action` on it, then require the `nests` flag.
async fn member(state: &AppState, headers: &HeaderMap, action: Action) -> GResult<String> {
    let principal: Principal = principal_for_token(state, crate::bearer(headers)).await?;
    let account = capabilities::principal_account(&principal)
        .map(str::to_string)
        .ok_or_else(|| {
            GErr::forbidden(
                "credential_not_accepted",
                "an account credential is required",
            )
        })?;
    let facts = Facts {
        now: now_unix(),
        ..Facts::default()
    };
    authorize(
        &Actor::Principal(&principal),
        action,
        &Resource::Account(&account),
        &facts,
    )
    .map_err(|deny| {
        let code = deny.code();
        let status = deny.into_response().status();
        GErr::new(status, code, "this credential may not do that")
    })?;
    if !crate::filtering::feature_on(state, Some(&account), capabilities::NESTS).await {
        return Err(GErr::forbidden(
            "feature_disabled",
            "Nests are not enabled for this account",
        ));
    }
    Ok(account)
}

fn parse<T: serde::de::DeserializeOwned>(body: Result<Json<Value>, JsonRejection>) -> GResult<T> {
    let Json(value) = body.map_err(|_| GErr::bad("invalid_json", "expected a JSON object"))?;
    serde_json::from_value(value).map_err(|e| GErr::bad("invalid_request", e.to_string()))
}

// ---- reads ----------------------------------------------------------------------------------------

fn package_json(p: &Package) -> Value {
    json!({
        "code": p.code,
        "name": p.name,
        "rank": p.rank,
        "slots": p.slots,
        "storage_bytes": p.storage_bytes,
        "active_hours": p.active_hours,
        "input_tokens": p.input_tokens,
        "output_tokens": p.output_tokens,
        "model": p.model,
        "sellable": p.sellable,
        "apple_product_id": p.apple_product,
        "google_product_id": p.google_product,
        "google_base_plan_id": nest_catalog::GOOGLE_BASE_PLAN,
        "web_plan": p.web_plan,
    })
}

/// `GET /v1/nest-packages`
async fn packages(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let result = async {
        let account = member(&state, &headers, Action::NestView).await?;
        Ok::<_, GErr>(
            Json(json!({
                "catalog_version": nest_catalog::CATALOG_VERSION,
                "packages": nest_catalog::PACKAGES.iter().map(package_json).collect::<Vec<_>>(),
                "rails": {
                    "apple": state.appstore.is_some(),
                    "google": state.googleplay.is_some(),
                    "web": true,
                },
                // Bind store purchases to this account: StoreKit `appAccountToken`, Google
                // `obfuscatedAccountId`. Opaque, stable, and checked again by the server.
                "app_account_token": appstore::account_token(&account),
                "google_account_id": googleplay::account_id(&account),
            }))
            .into_response(),
        )
    }
    .await;
    result.unwrap_or_else(|e| e.into_response())
}

/// Everything the account page/Products page needs about the account's Nests.
async fn snapshot(state: &AppState, account: String) -> GResult<Value> {
    let now = now_unix();
    state
        .store
        .group_read(move |c| -> GResult<Value> {
            let allowance = nest_funding::allowance(c, &account, now)?;
            let setups = nest_funding::setups_for(c, &account)?;
            let mut stmt = c.prepare(
                "SELECT id, address, label, state, funding, paused_reason, unfunded_since, created_at
                   FROM nests WHERE account_id = ?1 AND deleted_at IS NULL ORDER BY created_at, id",
            )?;
            let nests = stmt
                .query_map([&account], |r| {
                    let unfunded_since: Option<i64> = r.get(6)?;
                    let st: String = r.get(3)?;
                    let reason: Option<String> = r.get(5)?;
                    let working = match r.get::<_, Option<String>>(1)? {
                        Some(a) => working_in(c, &a, now)?,
                        None => false,
                    };
                    let (status, why) = nest_status(&st, reason.as_deref(), working);
                    Ok(json!({
                        "status": status,
                        "status_reason": why,
                        "nest_id": r.get::<_, String>(0)?,
                        "address": r.get::<_, Option<String>>(1)?,
                        "label": r.get::<_, String>(2)?,
                        "state": r.get::<_, String>(3)?,
                        "funding": r.get::<_, String>(4)?,
                        "paused_reason": r.get::<_, Option<String>>(5)?,
                        "delete_after": unfunded_since.map(|s| s + nest_funding::EXPORT_WINDOW_SECS),
                        "created_at": r.get::<_, i64>(7)?,
                    }))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let package = allowance
                .funding
                .as_ref()
                .and_then(|f| nest_catalog::by_code(f.package))
                .map(package_json);
            Ok(json!({
                "allowance": allowance,
                "package": package,
                "nests": nests,
                "setups": setups,
            }))
        })
        .await
}

/// `GET /v1/nests`
async fn list(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let result = async {
        let account = member(&state, &headers, Action::NestView).await?;
        Ok::<_, GErr>(Json(snapshot(&state, account).await?).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e.into_response())
}

// ---- status header (B3) --------------------------------------------------------------------------

/// How long after a `working` receipt a Nest still counts as working (a run that died without
/// reporting must not spin forever).
const WORKING_WINDOW_SECS: i64 = 900;

/// The header a client shows above a Nest conversation (build plan B3): `ready`, `working`,
/// `needs_you`, `paused` or `unavailable`, with a machine-readable reason. Pure, so every state is
/// tested; the caller supplies the stored row and whether a run is in progress.
pub(crate) fn nest_status(
    state: &str,
    paused_reason: Option<&str>,
    working: bool,
) -> (&'static str, Option<&'static str>) {
    match state {
        "creating" | "enrolling" | "provisioning" => ("unavailable", Some("setting_up")),
        "failed" => ("unavailable", Some("failed")),
        "deleted" => ("unavailable", Some("deleted")),
        "paused" => match paused_reason {
            // The owner has to do something: pay (or restore) to bring it back.
            Some("unfunded") => ("needs_you", Some("payment")),
            Some("over_capacity") => ("paused", Some("over_capacity")),
            _ => ("paused", None),
        },
        // An idle-stopped Nest is woken by the next message: to the person it is ready.
        "ready" | "running" | "stopped" => {
            if working {
                ("working", None)
            } else {
                ("ready", None)
            }
        }
        _ => ("unavailable", Some("unknown")),
    }
}

fn working_in(c: &rusqlite::Connection, address: &str, now: u64) -> rusqlite::Result<bool> {
    c.query_row(
        "SELECT EXISTS(SELECT 1 FROM message_meta mm JOIN messages m ON m.id = mm.id
           WHERE m.direction = 'out' AND m.recipient = ?1 AND mm.agent_status = 'working'
             AND mm.agent_status_at > ?2)",
        rusqlite::params![address, now as i64 - WORKING_WINDOW_SECS],
        |r| r.get(0),
    )
}

/// `GET /v1/nests/{nest}` — one of the caller's Nests with its status header. Another account's
/// Nest answers 404, the same as one that does not exist.
async fn one(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(nest): Path<String>,
) -> Response {
    let result = async {
        let (_caller, role, n) = crate::nest_access::authorize_nest(
            &state,
            &headers,
            Action::NestView,
            &nest,
            crate::nest_access::Role::Operator,
        )
        .await?;
        let now = now_unix();
        let id = nest.clone();
        let found = state
            .store
            .group_read(move |c| -> GResult<Value> {
                let (unfunded_since, created): (Option<i64>, i64) = c.query_row(
                    "SELECT unfunded_since, created_at FROM nests WHERE id = ?1",
                    [&id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                let working = match &n.address {
                    Some(a) => working_in(c, a, now)?,
                    None => false,
                };
                let (status, why) = nest_status(&n.state, n.paused_reason.as_deref(), working);
                Ok(json!({
                    "nest_id": id,
                    "address": n.address,
                    "label": n.label,
                    "state": n.state,
                    "funding": n.funding,
                    "org_id": n.org_id,
                    "role": if role == crate::nest_access::Role::Owner { "owner" } else { "operator" },
                    "status": status,
                    "status_reason": why,
                    "paused_reason": n.paused_reason,
                    "delete_after": unfunded_since.map(|s| s + nest_funding::EXPORT_WINDOW_SECS),
                    "created_at": created,
                }))
            })
            .await?;
        Ok::<_, GErr>(Json(found).into_response())
    }
    .await;
    result.unwrap_or_else(|e: GErr| e.into_response())
}

// ---- create -------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateReq {
    label: String,
}

fn valid_label(label: &str) -> Option<String> {
    let label = label.trim();
    (!label.is_empty() && label.chars().count() <= 64 && !label.chars().any(char::is_control))
        .then(|| label.to_string())
}

/// The account's own person mailboxes a new Nest acts for (`answer_question`): never another
/// Nest's mailbox, at most eight, oldest first.
async fn owner_mailboxes(state: &AppState, account: &str) -> GResult<Vec<String>> {
    let account = account.to_string();
    state
        .store
        .group_read(move |c| -> GResult<Vec<String>> {
            let mut stmt = c.prepare(
                "SELECT address FROM identities WHERE account_id = ?1
                   AND address NOT IN (SELECT address FROM nests WHERE address IS NOT NULL)
                   AND (label IS NULL OR label NOT LIKE 'nest: %')
                 ORDER BY created_at, address LIMIT 8",
            )?;
            let rows = stmt
                .query_map([&account], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
}

/// Create one package-funded Nest for `account`, sized by its funding package.
pub(crate) async fn create_funded(
    state: &AppState,
    account: &str,
    label: String,
) -> GResult<(String, String, &'static str)> {
    let now = now_unix();
    let acct = account.to_string();
    let package = state
        .store
        .group_read(move |c| -> GResult<Option<&'static Package>> {
            Ok(nest_funding::allowance(c, &acct, now)?
                .funding
                .and_then(|f| nest_catalog::by_code(f.package)))
        })
        .await?
        .ok_or_else(|| {
            GErr::new(
                StatusCode::PAYMENT_REQUIRED,
                "no_nest_package",
                "buy a Nest package first",
            )
        })?;
    let senders = owner_mailboxes(state, account).await?;
    let gib = (package.per_nest_storage_bytes() / (1024 * 1024 * 1024)) as u64;
    crate::nests::provision(
        state,
        crate::nests::Provision {
            account: account.to_string(),
            label,
            senders,
            funding: crate::nests::Funding::Package,
            budgets: package.per_nest_tokens(),
            quota_gib: Some(gib.max(1)),
            audit_service: None,
            address: None,
        },
        None,
    )
    .await
}

/// `POST /v1/nests {label}` — a Nest in a free slot of the account's package.
async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let account = member(&state, &headers, Action::NestOwn).await?;
        let req: CreateReq = parse(body)?;
        let label = valid_label(&req.label)
            .ok_or_else(|| GErr::bad("bad_label", "label: 1-64 printable characters"))?;
        let (nest_id, address, new_state) = create_funded(&state, &account, label).await?;
        Ok::<_, GErr>(
            (
                StatusCode::CREATED,
                Json(json!({ "nest_id": nest_id, "address": address, "state": new_state })),
            )
                .into_response(),
        )
    }
    .await;
    result.unwrap_or_else(|e| e.into_response())
}

// ---- purchases ------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IntentReq {
    package: String,
    rail: String,
}

/// `POST /v1/nest-purchases/intents {package, rail}` — before any store sheet or web checkout.
async fn intent(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let account = member(&state, &headers, Action::NestPurchase).await?;
        let req: IntentReq = parse(body)?;
        let package = nest_catalog::by_code(&req.package)
            .filter(|p| p.sellable)
            .ok_or_else(|| GErr::bad("unknown_package", "that Nest package is not for sale"))?;
        let rail = match req.rail.as_str() {
            "apple" if state.appstore.is_some() => "apple",
            "google" if state.googleplay.is_some() => "google",
            "web" => "web",
            "apple" | "google" => {
                return Err(GErr::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "billing_unavailable",
                    "store purchases are not configured",
                ))
            }
            _ => return Err(GErr::bad("unknown_rail", "rail must be apple, google or web")),
        };
        let now = now_unix();
        let acct = account.clone();
        let outcome = state
            .store
            .group_tx(move |tx| -> GResult<IntentOutcome> {
                Ok(nest_funding::create_intent(tx, &acct, package, rail, now)?)
            })
            .await?;
        match outcome {
            IntentOutcome::Created { id, expires_at } => Ok(Json(json!({
                "intent_id": id,
                "expires_at": expires_at,
                "package": package.code,
                "rail": rail,
                "catalog_version": nest_catalog::CATALOG_VERSION,
                "apple_product_id": package.apple_product,
                "google_product_id": package.google_product,
                "google_base_plan_id": nest_catalog::GOOGLE_BASE_PLAN,
                "web_plan": package.web_plan,
                "app_account_token": appstore::account_token(&account),
                "google_account_id": googleplay::account_id(&account),
            }))
            .into_response()),
            IntentOutcome::ActiveElsewhere { source, package } => Err(GErr::conflict(
                "nest_package_active",
                "a Nest package bought elsewhere already funds this account; change it where it was bought",
            )
            .with("source", source)
            .with("package", package)),
            IntentOutcome::AlreadyOwned { package } => Err(GErr::conflict(
                "nest_package_owned",
                "this account already has that Nest package; restore purchases instead",
            )
            .with("package", package)),
            IntentOutcome::RateLimited => Err(GErr::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "too many purchase attempts; try again later",
            )),
        }
    }
    .await;
    result.unwrap_or_else(|e| e.into_response())
}

fn default_label(package: &str) -> String {
    nest_catalog::by_code(package)
        .map(|p| p.name.to_string())
        .unwrap_or_else(|| "Nest".to_string())
}

/// After a store purchase was recorded: answer the client, queue the first Nest's setup and try
/// it once right away (the reconciler retries it if this attempt fails).
async fn after_claim(state: &AppState, account: &str, claim: StoreClaim) -> GResult<Response> {
    match claim {
        StoreClaim::UnknownProduct => Err(GErr::bad(
            "invalid_purchase",
            "that purchase is not a Nest package",
        )),
        StoreClaim::BoundElsewhere => Err(GErr::conflict(
            "purchase_already_used",
            "this subscription belongs to another Pigeonpost account",
        )),
        StoreClaim::Saved {
            entitlement_id,
            package,
            active,
            duplicate,
        } => {
            if active && !duplicate {
                let (acct, ent, label) = (
                    account.to_string(),
                    entitlement_id.clone(),
                    default_label(package),
                );
                let setup = state
                    .store
                    .group_tx(move |tx| -> GResult<Option<nest_funding::Setup>> {
                        Ok(nest_funding::ensure_setup(
                            tx,
                            &acct,
                            &ent,
                            &label,
                            now_unix(),
                        )?)
                    })
                    .await?;
                if let Some(setup) = setup.filter(|s| s.status == "pending") {
                    run_setup(state, account, &setup).await;
                }
            }
            let mut body = snapshot(state, account.to_string()).await?;
            body["claimed"] = json!({
                "entitlement_id": entitlement_id,
                "package": package,
                "active": active,
                "duplicate": duplicate,
            });
            Ok(Json(body).into_response())
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AppleReq {
    transaction_id: String,
}

fn apple_state(status: &appstore::SubscriptionStatus) -> &'static str {
    if status.revoked {
        "revoked"
    } else if status.active {
        "active"
    } else {
        "expired"
    }
}

fn apple_env(environment: &str) -> &'static str {
    if environment == "Production" {
        "live"
    } else {
        "sandbox"
    }
}

/// `POST /v1/nest-purchases/apple {transaction_id}` — claim or restore.
async fn claim_apple(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let account = member(&state, &headers, Action::NestPurchase).await?;
        let req: AppleReq = parse(body)?;
        let Some(apple) = state.appstore.clone() else {
            return Err(GErr::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "billing_unavailable",
                "App Store purchases are not configured",
            ));
        };
        let _guard = apple.verification.lock().await;
        let status = apple
            .status_for(req.transaction_id.trim(), &nest_catalog::apple_products())
            .await
            .map_err(apple_error)?;
        // The purchase must have been made for this account. Restores of a subscription bought
        // before the binding existed are not possible: every Nest purchase carries it.
        let token = status.entitlement.app_account_token.as_deref();
        if !token.is_some_and(|t| t.eq_ignore_ascii_case(&appstore::account_token(&account))) {
            return Err(GErr::forbidden(
                "purchase_account_mismatch",
                "this purchase was made for a different Pigeonpost account",
            ));
        }
        if !status.active {
            return Err(GErr::conflict(
                "purchase_inactive",
                if status.revoked {
                    "this subscription was refunded"
                } else {
                    "this subscription is not active"
                },
            ));
        }
        let purchase = StorePurchase {
            source: "apple",
            environment: apple_env(&status.entitlement.environment).into(),
            reference: status.entitlement.original_transaction_id.clone(),
            product_id: status.entitlement.product_id.clone(),
            expires_at: status.entitlement.expires_at,
            state: apple_state(&status).into(),
            acknowledged: true,
            replaces: None,
        };
        let acct = account.clone();
        let claim = state
            .store
            .group_tx(move |tx| -> GResult<StoreClaim> {
                let now = now_unix();
                let claim = nest_funding::record_store_purchase(tx, &acct, &purchase, now)?;
                if let StoreClaim::Saved { package, .. } = &claim {
                    nest_funding::consume_intent(
                        tx,
                        &acct,
                        package,
                        "apple",
                        &purchase.reference,
                        now,
                    )?;
                }
                Ok(claim)
            })
            .await?;
        drop(_guard);
        after_claim(&state, &account, claim).await
    }
    .await;
    result.unwrap_or_else(|e| e.into_response())
}

fn apple_error(e: appstore::AppStoreError) -> GErr {
    use appstore::AppStoreError as E;
    match e {
        E::NotFound => GErr::bad("purchase_not_found", "Apple has no such transaction"),
        E::NotOurs(_) => GErr::bad("invalid_purchase", "that purchase is not a Nest package"),
        E::Expired => GErr::conflict("purchase_inactive", "this subscription is not active"),
        E::Revoked => GErr::conflict("purchase_inactive", "this subscription was refunded"),
        E::Unauthorized | E::Unreachable(_) | E::Malformed => GErr::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "billing_unavailable",
            "the App Store could not be checked; try again",
        ),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GoogleReq {
    purchase_token: String,
}

fn google_state(p: &googleplay::VerifiedPurchase) -> &'static str {
    if !p.active {
        return "expired";
    }
    match p.state.as_str() {
        "SUBSCRIPTION_STATE_IN_GRACE_PERIOD" => "grace",
        "SUBSCRIPTION_STATE_CANCELED" => "canceled",
        _ => "active",
    }
}

/// `POST /v1/nest-purchases/google {purchase_token}` — claim or restore. Acknowledged only after
/// the purchase is durable, and never when it duplicates a live package (Google then refunds it).
async fn claim_google(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let account = member(&state, &headers, Action::NestPurchase).await?;
        let req: GoogleReq = parse(body)?;
        let Some(play) = state.googleplay.clone() else {
            return Err(GErr::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "billing_unavailable",
                "Google Play purchases are not configured",
            ));
        };
        let token = req.purchase_token.trim().to_string();
        let _guard = play.verification.lock().await;
        let verified = play
            .verify_for(
                &token,
                &account,
                &nest_catalog::google_products(),
                nest_catalog::GOOGLE_BASE_PLAN,
            )
            .await
            .map_err(google_error)?;
        if !verified.active {
            return Err(GErr::conflict(
                "purchase_inactive",
                "Google Play has not confirmed an active payment",
            ));
        }
        let env = if verified.test_purchase {
            "test"
        } else {
            "live"
        };
        let purchase = StorePurchase {
            source: "google",
            environment: env.into(),
            reference: token.clone(),
            product_id: verified.product_id.clone(),
            expires_at: verified.expires_at as i64,
            state: google_state(&verified).into(),
            acknowledged: verified.acknowledged,
            replaces: verified.linked_token.clone(),
        };
        let acct = account.clone();
        let claim = state
            .store
            .group_tx(move |tx| -> GResult<StoreClaim> {
                let now = now_unix();
                let claim = nest_funding::record_store_purchase(tx, &acct, &purchase, now)?;
                if let StoreClaim::Saved { package, .. } = &claim {
                    nest_funding::consume_intent(
                        tx,
                        &acct,
                        package,
                        "google",
                        &purchase.reference,
                        now,
                    )?;
                }
                Ok(claim)
            })
            .await?;
        if let StoreClaim::Saved {
            active: true,
            duplicate: false,
            ..
        } = &claim
        {
            if !verified.acknowledged
                && play.acknowledge(&token, &verified.product_id).await.is_ok()
            {
                let (env, token) = (env.to_string(), token.clone());
                let _ = state
                    .store
                    .group_tx(move |tx| -> GResult<()> {
                        Ok(nest_funding::mark_acknowledged(tx, &env, &token)?)
                    })
                    .await;
            }
        }
        drop(_guard);
        after_claim(&state, &account, claim).await
    }
    .await;
    result.unwrap_or_else(|e| e.into_response())
}

fn google_error(e: googleplay::PlayError) -> GErr {
    use googleplay::PlayError as E;
    match e {
        E::AccountMismatch => GErr::forbidden(
            "purchase_account_mismatch",
            "this purchase was made for a different Pigeonpost account",
        ),
        E::NotFound => GErr::bad("purchase_not_found", "Google Play has no such purchase"),
        E::Invalid => GErr::bad("invalid_purchase", "that purchase is not a Nest package"),
        E::Unavailable => GErr::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "billing_unavailable",
            "Google Play could not be checked; try again",
        ),
    }
}

// ---- setups ---------------------------------------------------------------------------------------

/// One setup attempt: provision the first Nest of a paid package. A setup whose funding is no
/// longer live is canceled instead (no unfunded Nest is ever created).
pub(crate) async fn run_setup(state: &AppState, account: &str, setup: &nest_funding::Setup) {
    let now = now_unix();
    let acct = account.to_string();
    // Any live package funds the setup: the one it was queued for, or the one that replaced it
    // (an upgrade or a restore on another device).
    let funded = state
        .store
        .group_read(move |c| -> GResult<bool> {
            let a = nest_funding::allowance(c, &acct, now)?;
            Ok(a.funding.is_some() && a.used < a.slots)
        })
        .await
        .unwrap_or(false);
    let id = setup.setup_id.clone();
    if !funded {
        let _ = state
            .store
            .group_tx(move |tx| -> GResult<()> {
                Ok(nest_funding::cancel_setup(tx, &id, now_unix())?)
            })
            .await;
        return;
    }
    if !crate::filtering::feature_on(state, Some(account), capabilities::NESTS).await {
        // The flag is off for this account: keep the setup pending without spending an attempt.
        let _ = state
            .store
            .group_tx(move |tx| -> GResult<()> {
                Ok(nest_funding::defer_setup(tx, &id, now_unix())?)
            })
            .await;
        return;
    }
    let outcome = create_funded(state, account, setup.label.clone()).await;
    let result = match &outcome {
        Ok((nest_id, _, _)) => Ok(nest_id.clone()),
        Err(e) => Err(e.code.to_string()),
    };
    if let Err(code) = &result {
        tracing::warn!(setup = %setup.setup_id, code, "Nest setup attempt failed; will retry");
    }
    let _ = state
        .store
        .group_tx(move |tx| -> GResult<()> {
            Ok(nest_funding::finish_setup(
                tx,
                &id,
                result.as_deref().map_err(String::as_str),
                now_unix(),
            )?)
        })
        .await;
}

/// `POST /v1/nest-setups/{id}/retry`
async fn retry_setup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        let account = member(&state, &headers, Action::NestPurchase).await?;
        if !id.starts_with("nsu_") || id.len() > 64 {
            return Err(GErr::new(
                StatusCode::NOT_FOUND,
                "no_setup",
                "no such setup",
            ));
        }
        let (acct, sid) = (account.clone(), id.clone());
        let setup = state
            .store
            .group_tx(move |tx| -> GResult<Option<nest_funding::Setup>> {
                if !nest_funding::retry_setup(tx, &acct, &sid, now_unix())? {
                    return Ok(None);
                }
                Ok(nest_funding::setups_for(tx, &acct)?
                    .into_iter()
                    .find(|s| s.setup_id == sid))
            })
            .await?
            .ok_or_else(|| GErr::new(StatusCode::NOT_FOUND, "no_setup", "no such setup"))?;
        run_setup(&state, &account, &setup).await;
        Ok(Json(snapshot(&state, account).await?).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e.into_response())
}

// ---- reconciliation -------------------------------------------------------------------------------

/// Re-verify store purchases at most this often each (renewals land within a day; refunds are
/// caught within this window).
const RECHECK_SECS: i64 = 6 * 3600;

async fn reconcile_purchases(state: &AppState) {
    let now = now_unix();
    let Ok(due) = state
        .store
        .group_read(move |c| -> GResult<Vec<nest_funding::DuePurchase>> {
            Ok(nest_funding::due_purchases(c, now, RECHECK_SECS)?)
        })
        .await
    else {
        tracing::warn!("Nest purchase reconciliation could not read purchases");
        return;
    };
    for row in due {
        let fresh: Option<StorePurchase> = match row.source.as_str() {
            "apple" => {
                let Some(apple) = state.appstore.clone() else {
                    continue;
                };
                let _guard = apple.verification.lock().await;
                let env = if row.environment == "live" {
                    "Production"
                } else {
                    "Sandbox"
                };
                match apple
                    .subscription_status_for(&row.reference, env, &nest_catalog::apple_products())
                    .await
                {
                    Ok(status) => {
                        let token = status.entitlement.app_account_token.as_deref();
                        if !token.is_some_and(|t| {
                            t.eq_ignore_ascii_case(&appstore::account_token(&row.account_id))
                        }) {
                            tracing::warn!("Nest reconciliation: Apple account binding mismatch");
                            None
                        } else {
                            Some(StorePurchase {
                                source: "apple",
                                environment: row.environment.clone(),
                                reference: row.reference.clone(),
                                product_id: status.entitlement.product_id.clone(),
                                expires_at: status.entitlement.expires_at,
                                state: apple_state(&status).into(),
                                acknowledged: true,
                                replaces: None,
                            })
                        }
                    }
                    // An outage is not a verified expiry: keep the last confirmed state.
                    Err(_) => None,
                }
            }
            _ => {
                let Some(play) = state.googleplay.clone() else {
                    continue;
                };
                let _guard = play.verification.lock().await;
                match play
                    .verify_for(
                        &row.reference,
                        &row.account_id,
                        &nest_catalog::google_products(),
                        nest_catalog::GOOGLE_BASE_PLAN,
                    )
                    .await
                {
                    Ok(v) => {
                        let state_now = google_state(&v);
                        let acknowledged = v.acknowledged || row.acknowledged;
                        Some(StorePurchase {
                            source: "google",
                            environment: row.environment.clone(),
                            reference: row.reference.clone(),
                            product_id: v.product_id.clone(),
                            expires_at: v.expires_at as i64,
                            state: state_now.into(),
                            acknowledged,
                            replaces: v.linked_token.clone(),
                        })
                    }
                    Err(googleplay::PlayError::NotFound) => Some(StorePurchase {
                        source: "google",
                        environment: row.environment.clone(),
                        reference: row.reference.clone(),
                        product_id: row.product_id.clone(),
                        expires_at: now as i64,
                        state: "expired".into(),
                        acknowledged: row.acknowledged,
                        replaces: None,
                    }),
                    Err(_) => None,
                }
            }
        };
        let (source, env, reference, account) = (
            row.source.clone(),
            row.environment.clone(),
            row.reference.clone(),
            row.account_id.clone(),
        );
        let saved = state
            .store
            .group_tx(move |tx| -> GResult<Option<StoreClaim>> {
                let now = now_unix();
                match fresh {
                    Some(p) => Ok(Some(nest_funding::record_store_purchase(
                        tx, &account, &p, now,
                    )?)),
                    None => {
                        nest_funding::touch_purchase(tx, &source, &env, &reference, now)?;
                        Ok(None)
                    }
                }
            })
            .await;
        // A Google purchase saved while Google was unreachable is acknowledged here, unless it is
        // a duplicate (left unacknowledged so Google refunds it).
        if let (
            Ok(Some(StoreClaim::Saved {
                active: true,
                duplicate: false,
                ..
            })),
            "google",
        ) = (&saved, row.source.as_str())
        {
            if !row.acknowledged {
                if let Some(play) = state.googleplay.clone() {
                    if play
                        .acknowledge(&row.reference, &row.product_id)
                        .await
                        .is_ok()
                    {
                        let (env, token) = (row.environment.clone(), row.reference.clone());
                        let _ = state
                            .store
                            .group_tx(move |tx| -> GResult<()> {
                                Ok(nest_funding::mark_acknowledged(tx, &env, &token)?)
                            })
                            .await;
                    }
                }
            }
        }
    }
}

async fn reconcile_setups(state: &AppState) {
    let now = now_unix();
    let Ok(due) = state
        .store
        .group_read(move |c| -> GResult<Vec<(String, nest_funding::Setup)>> {
            Ok(nest_funding::due_setups(c, now)?)
        })
        .await
    else {
        return;
    };
    for (account, setup) in due {
        run_setup(state, &account, &setup).await;
    }
}

/// Carry out one account's funding actions through nestd, recording each one that nestd
/// confirmed. A refused call is retried on the next sweep; nothing is recorded for it.
pub(crate) async fn sweep_account(state: &AppState, account: &str) {
    let now = now_unix();
    let acct = account.to_string();
    let Ok((actions, paid_through)) = state
        .store
        .group_read(move |c| -> GResult<(Vec<FundingAction>, Option<i64>)> {
            Ok((
                nest_funding::funding_actions(c, &acct, now)?,
                nest_funding::last_paid_through(c, &acct)?,
            ))
        })
        .await
    else {
        return;
    };
    for action in actions {
        let outcome = match &action {
            FundingAction::Pause { nest_id, .. } => {
                crate::nests::runtime_action(state, nest_id, "pause")
                    .await
                    .map(|_| ())
            }
            FundingAction::Resume { nest_id } => {
                if !crate::filtering::feature_on(state, Some(account), capabilities::NESTS).await {
                    continue;
                }
                crate::nests::runtime_action(state, nest_id, "resume")
                    .await
                    .map(|_| ())
            }
            FundingAction::Delete { nest_id } => {
                let address = crate::nests::load_nest(state, nest_id.clone())
                    .await
                    .ok()
                    .flatten()
                    .and_then(|r| r.address);
                let audit = state.auditor.row(
                    crate::audit::ActorKind::System,
                    Some(account.to_string()),
                    "nest.delete",
                    Some(format!("nest:{nest_id}")),
                    json!({ "reason": "unfunded_export_window_ended" }),
                    None,
                    None,
                    now_unix(),
                );
                crate::nests::teardown(state, nest_id, address, audit).await
            }
        };
        match outcome {
            Ok(()) => {
                let audit = state.auditor.row(
                    crate::audit::ActorKind::System,
                    Some(account.to_string()),
                    match &action {
                        FundingAction::Pause { .. } => "nest.funding_pause",
                        FundingAction::Resume { .. } => "nest.funding_resume",
                        FundingAction::Delete { .. } => "nest.funding_delete",
                    },
                    Some(format!(
                        "nest:{}",
                        match &action {
                            FundingAction::Pause { nest_id, .. }
                            | FundingAction::Resume { nest_id }
                            | FundingAction::Delete { nest_id } => nest_id,
                        }
                    )),
                    json!({}),
                    None,
                    None,
                    now_unix(),
                );
                let recorded = action.clone();
                let _ = state
                    .store
                    .group_tx(move |tx| -> GResult<()> {
                        nest_funding::apply_funding(tx, &recorded, paid_through, now_unix())?;
                        crate::store::audit::append_in(tx, &audit)?;
                        Ok(())
                    })
                    .await;
            }
            Err(e) => tracing::warn!(
                account,
                code = e.code,
                "Nest funding action failed; will retry"
            ),
        }
    }
}

async fn sweep(state: &AppState) {
    let Ok(accounts) = state
        .store
        .group_read(|c| -> GResult<Vec<String>> { Ok(nest_funding::funded_accounts(c)?) })
        .await
    else {
        return;
    };
    for account in accounts {
        let acct = account.clone();
        let _ = state
            .store
            .group_tx(move |tx| -> GResult<Option<nest_funding::Setup>> {
                Ok(nest_funding::ensure_funded_setup(tx, &acct, now_unix())?)
            })
            .await;
        sweep_account(state, &account).await;
    }
}

/// Renewals, refunds, missed claims and failed setups are handled while every app is closed.
pub(crate) fn start_reconciliation(state: AppState) {
    if !state.nests.configured() {
        return;
    }
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(std::time::Duration::from_secs(60));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            reconcile_purchases(&state).await;
            sweep(&state).await;
            reconcile_setups(&state).await;
        }
    });
}

/// Tests: one reconciliation pass.
#[cfg(test)]
pub(crate) async fn reconcile_once(state: &AppState) {
    reconcile_purchases(state).await;
    sweep(state).await;
    reconcile_setups(state).await;
}

#[cfg(test)]
mod status_tests {
    use super::nest_status;

    #[test]
    fn every_state_maps_to_one_header() {
        assert_eq!(nest_status("ready", None, false), ("ready", None));
        assert_eq!(nest_status("stopped", None, false), ("ready", None));
        assert_eq!(nest_status("running", None, true), ("working", None));
        assert_eq!(
            nest_status("paused", Some("unfunded"), true),
            ("needs_you", Some("payment"))
        );
        assert_eq!(
            nest_status("paused", Some("over_capacity"), false),
            ("paused", Some("over_capacity"))
        );
        assert_eq!(nest_status("paused", None, false), ("paused", None));
        assert_eq!(
            nest_status("provisioning", None, false),
            ("unavailable", Some("setting_up"))
        );
        assert_eq!(
            nest_status("failed", None, false),
            ("unavailable", Some("failed"))
        );
        assert_eq!(
            nest_status("weird", None, false),
            ("unavailable", Some("unknown"))
        );
    }
}
