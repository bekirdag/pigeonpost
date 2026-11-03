//! The Nest control service: lifecycle, admission, idle stop and reconciliation, behind a private
//! API that only the postbox can call.

use crate::config::Config;
use crate::db::Db;
use crate::sandbox::{RuntimeState, Sandbox, Spec};
use crate::storage::{write_for_sandbox, Volumes};
use crate::util::{ct_eq, now, period_of, random_hex, sha256_hex, valid_nest_id};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct Ctx {
    pub cfg: Arc<Config>,
    pub db: Db,
    pub sandbox: Arc<dyn Sandbox>,
    pub volumes: Arc<dyn Volumes>,
    /// SHA-256 (hex) of the one bearer token allowed to call this API.
    pub token_sha256: Arc<String>,
    /// Who holds the leases this process takes (host name + boot id).
    pub holder: Arc<String>,
    locks: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

impl Ctx {
    pub fn new(
        cfg: Config,
        db: Db,
        sandbox: Arc<dyn Sandbox>,
        volumes: Arc<dyn Volumes>,
        token_sha256: String,
        holder: String,
    ) -> Self {
        Ctx {
            cfg: Arc::new(cfg),
            db,
            sandbox,
            volumes,
            token_sha256: Arc::new(token_sha256),
            holder: Arc::new(holder),
            locks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// One operation per Nest at a time; different Nests proceed in parallel.
    fn lock_for(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut map = self.locks.lock().expect("lock map");
        map.entry(id.to_string()).or_default().clone()
    }
}

#[derive(Debug)]
pub struct ApiError(StatusCode, &'static str, String);

impl ApiError {
    fn new(status: StatusCode, code: &'static str, detail: impl Into<String>) -> Self {
        ApiError(status, code, detail.into())
    }
    fn internal(detail: impl Into<String>) -> Self {
        let detail = detail.into();
        tracing::error!(%detail, "control operation failed");
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal", detail)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1, "detail": self.2}))).into_response()
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("task: {e}"))?
}

// ---- Lifecycle ---------------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct CreateNest {
    pub nest_id: String,
    pub account_id: String,
    pub address: String,
    pub label: String,
    pub enrollment_code: String,
    #[serde(default)]
    pub budgets: Option<Budgets>,
    #[serde(default)]
    pub quota_gib: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Budgets {
    pub input_tokens: i64,
    pub output_tokens: i64,
}

fn valid_code(code: &str) -> bool {
    code.strip_prefix("nenr_").is_some_and(|rest| {
        (32..=128).contains(&rest.len()) && rest.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

pub async fn create(ctx: &Ctx, req: CreateNest) -> ApiResult {
    if !valid_nest_id(&req.nest_id) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_nest_id",
            "nest_id",
        ));
    }
    if req.label.is_empty() || req.label.chars().count() > 64 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_label",
            "1–64 characters",
        ));
    }
    if req.account_id.is_empty() || req.account_id.len() > 128 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_account",
            "account_id",
        ));
    }
    if !req.address.starts_with('/') || req.address.len() > 256 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_address",
            "address",
        ));
    }
    if !valid_code(&req.enrollment_code) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_code",
            "enrollment_code",
        ));
    }
    let lock = ctx.lock_for(&req.nest_id);
    let _guard = lock.lock().await;

    if let Some(existing) = ctx.db.nest(&req.nest_id).map_err(ApiError::internal)? {
        // Idempotent retry of the same create; anything else is a conflict.
        if existing.account_id == req.account_id && existing.address == req.address {
            return Ok(Json(status_json(ctx, &existing)));
        }
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "nest_exists",
            "different owner",
        ));
    }

    let gib = req
        .quota_gib
        .unwrap_or(ctx.cfg.storage.default_gib)
        .clamp(1, 64);
    let allocated: i64 = ctx
        .db
        .nests(false)
        .map_err(ApiError::internal)?
        .iter()
        .map(|n| n.quota_gib)
        .sum();
    if allocated + gib as i64 > ctx.cfg.storage.max_total_gib as i64 {
        return Err(ApiError::new(
            StatusCode::INSUFFICIENT_STORAGE,
            "no_storage",
            "this host has no room for another Nest volume",
        ));
    }
    let budgets = req.budgets.clone().unwrap_or(Budgets {
        input_tokens: 1_000_000,
        output_tokens: 100_000,
    });
    if budgets.input_tokens < 0 || budgets.output_tokens < 0 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_budget",
            "negative",
        ));
    }
    ctx.db
        .insert_nest(
            &req.nest_id,
            &req.account_id,
            &req.address,
            &req.label,
            gib as i64,
            budgets.input_tokens,
            budgets.output_tokens,
        )
        .map_err(ApiError::internal)?;
    ctx.db.event(Some(&req.nest_id), "created", &req.account_id);

    let volumes = ctx.volumes.clone();
    let id = req.nest_id.clone();
    let uid = ctx.cfg.storage.sandbox_uid;
    let enroll = json!({
        "nest_id": req.nest_id,
        "address": req.address,
        "postbox_url": ctx.cfg.sandbox.postbox_url,
        "code": req.enrollment_code,
    })
    .to_string();
    let provisioned = blocking(move || {
        let root = volumes.create(&id, gib)?;
        // The single-use enrollment code goes to the one place that redeems it: the sandbox. It
        // deletes the file once the mailbox credential is in hand.
        write_for_sandbox(&root.join("run/enroll.json"), enroll.as_bytes(), uid)?;
        Ok(root)
    })
    .await;
    if let Err(e) = provisioned {
        let _ = ctx.db.set_state(
            &req.nest_id,
            "failed",
            "stopped",
            Some("provision"),
            Some(&e),
        );
        return Err(ApiError::internal(format!("provision: {e}")));
    }
    if let Err(e) = start_locked(ctx, &req.nest_id).await {
        if e.1 == "at_capacity" {
            // Provisioned and wanted running: reconciliation starts it when a slot frees up.
            let _ = ctx
                .db
                .set_state(&req.nest_id, "stopped", "running", Some("capacity"), None);
        }
        return Err(e);
    }
    nest_json(ctx, &req.nest_id)
}

/// Start (or restart) a Nest as a new generation. Caller holds the Nest's lock.
async fn start_locked(ctx: &Ctx, id: &str) -> Result<(), ApiError> {
    let nest = ctx
        .db
        .nest(id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no_nest", id.to_string()))?;
    if matches!(nest.state.as_str(), "deleted" | "deleting") {
        return Err(ApiError::new(StatusCode::GONE, "deleted", id.to_string()));
    }
    let resident = ctx
        .db
        .nests(false)
        .map_err(ApiError::internal)?
        .iter()
        .filter(|n| n.id != id && matches!(n.state.as_str(), "running" | "paused"))
        .count();
    if resident >= ctx.cfg.control.max_running {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "at_capacity",
            "this host is running its maximum number of Nests",
        ));
    }
    if !ctx
        .db
        .acquire_lease(id, &ctx.holder)
        .map_err(ApiError::internal)?
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "leased",
            "another worker holds this Nest",
        ));
    }
    let generation = ctx.db.next_generation(id).map_err(ApiError::internal)?;
    let token = format!("nbt_{}", random_hex(32));
    ctx.db
        .set_broker_token(id, &sha256_hex(token.as_bytes()), generation)
        .map_err(ApiError::internal)?;

    let volumes = ctx.volumes.clone();
    let sandbox = ctx.sandbox.clone();
    let uid = ctx.cfg.storage.sandbox_uid;
    let nest_id = id.to_string();
    let started = std::time::Instant::now();
    let result = blocking(move || {
        let root = volumes.open(&nest_id)?;
        write_for_sandbox(&root.join("run/broker-token"), token.as_bytes(), uid)?;
        sandbox.start(&Spec {
            nest_id: nest_id.clone(),
            generation,
            volume: root,
        })
    })
    .await;
    let ms = started.elapsed().as_millis() as i64;
    match result {
        Ok(()) => {
            ctx.db
                .set_state(id, "running", "running", None, None)
                .map_err(ApiError::internal)?;
            ctx.db
                .open_run(id, generation, ms)
                .map_err(ApiError::internal)?;
            ctx.db.event(
                Some(id),
                "started",
                &format!("generation {generation}, {ms} ms"),
            );
            tracing::info!(nest = id, generation, ms, "nest started");
            Ok(())
        }
        Err(e) => {
            let _ = ctx.db.revoke_broker_token(id);
            let _ = ctx
                .db
                .set_state(id, "failed", "stopped", Some("start_failed"), Some(&e));
            Err(ApiError::internal(format!("start: {e}")))
        }
    }
}

/// Stop, release RAM and close the volume. Caller holds the lock.
async fn stop_locked(ctx: &Ctx, id: &str, reason: &str, desired: &str) -> Result<(), ApiError> {
    let sandbox = ctx.sandbox.clone();
    let volumes = ctx.volumes.clone();
    let nest_id = id.to_string();
    // Fence first: the broker refuses this Nest's token from this instant.
    ctx.db.revoke_broker_token(id).map_err(ApiError::internal)?;
    let r = blocking(move || {
        sandbox.stop(&nest_id)?;
        if let Ok(root) = volumes.open(&nest_id) {
            let _ = std::fs::remove_file(root.join("run/broker-token"));
        }
        volumes.close(&nest_id)
    })
    .await;
    let _ = ctx.db.close_runs(id, reason);
    match r {
        Ok(()) => {
            ctx.db
                .set_state(id, "stopped", desired, Some(reason), None)
                .map_err(ApiError::internal)?;
            ctx.db.event(Some(id), "stopped", reason);
            Ok(())
        }
        Err(e) => {
            let _ = ctx
                .db
                .set_state(id, "failed", desired, Some(reason), Some(&e));
            Err(ApiError::internal(format!("stop: {e}")))
        }
    }
}

pub async fn action(ctx: &Ctx, id: &str, action: &str) -> ApiResult {
    if !valid_nest_id(id) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_nest_id",
            "nest_id",
        ));
    }
    let lock = ctx.lock_for(id);
    let _guard = lock.lock().await;
    let nest = ctx
        .db
        .nest(id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no_nest", id.to_string()))?;
    if matches!(nest.state.as_str(), "deleted" | "deleting") {
        return Err(ApiError::new(StatusCode::GONE, "deleted", id.to_string()));
    }
    let runtime = {
        let s = ctx.sandbox.clone();
        let n = id.to_string();
        blocking(move || s.state(&n))
            .await
            .map_err(ApiError::internal)?
    };
    match action {
        "start" => {
            if runtime != RuntimeState::Running {
                start_locked(ctx, id).await?;
            }
        }
        "stop" => stop_locked(ctx, id, "operator", "stopped").await?,
        "pause" => {
            if runtime == RuntimeState::Running {
                let s = ctx.sandbox.clone();
                let n = id.to_string();
                blocking(move || s.pause(&n))
                    .await
                    .map_err(ApiError::internal)?;
            }
            ctx.db
                .set_state(id, "paused", "paused", Some("operator"), None)
                .map_err(ApiError::internal)?;
            ctx.db.event(Some(id), "paused", "");
        }
        "resume" => match runtime {
            RuntimeState::Paused => {
                let s = ctx.sandbox.clone();
                let n = id.to_string();
                blocking(move || s.resume(&n))
                    .await
                    .map_err(ApiError::internal)?;
                ctx.db
                    .set_state(id, "running", "running", None, None)
                    .map_err(ApiError::internal)?;
                ctx.db.touch_active(id);
                ctx.db.event(Some(id), "resumed", "");
            }
            RuntimeState::Running => {
                ctx.db
                    .set_state(id, "running", "running", None, None)
                    .map_err(ApiError::internal)?;
            }
            _ => start_locked(ctx, id).await?,
        },
        // Mail arrived: start a Nest that idle-stop put to sleep. A Nest its owner paused or
        // stopped stays as it is.
        "wake" => match (nest.state.as_str(), nest.stop_reason.as_deref()) {
            ("stopped", Some("idle")) => start_locked(ctx, id).await?,
            ("running", _) => ctx.db.touch_active(id),
            _ => {}
        },
        _ => {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "no_action",
                action.to_string(),
            ));
        }
    }
    nest_json(ctx, id)
}

pub async fn delete(ctx: &Ctx, id: &str) -> ApiResult {
    if !valid_nest_id(id) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_nest_id",
            "nest_id",
        ));
    }
    let lock = ctx.lock_for(id);
    let _guard = lock.lock().await;
    let Some(nest) = ctx.db.nest(id).map_err(ApiError::internal)? else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "no_nest",
            id.to_string(),
        ));
    };
    if nest.state == "deleted" {
        return Ok(Json(status_json(ctx, &nest)));
    }
    ctx.db
        .set_state(id, "deleting", "deleted", Some("delete"), None)
        .map_err(ApiError::internal)?;
    ctx.db.revoke_broker_token(id).map_err(ApiError::internal)?;
    let sandbox = ctx.sandbox.clone();
    let volumes = ctx.volumes.clone();
    let nest_id = id.to_string();
    blocking(move || {
        sandbox.stop(&nest_id)?;
        volumes.destroy(&nest_id)
    })
    .await
    .map_err(|e| ApiError::internal(format!("delete: {e}")))?;
    let _ = ctx.db.close_runs(id, "deleted");
    ctx.db
        .set_state(id, "deleted", "deleted", Some("delete"), None)
        .map_err(ApiError::internal)?;
    ctx.db.release_lease(id);
    ctx.db
        .event(Some(id), "deleted", "volume and key destroyed");
    nest_json(ctx, id)
}

fn nest_json(ctx: &Ctx, id: &str) -> ApiResult {
    let nest = ctx
        .db
        .nest(id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no_nest", id.to_string()))?;
    Ok(Json(status_json(ctx, &nest)))
}

fn status_json(ctx: &Ctx, nest: &crate::db::Nest) -> Value {
    let t = now();
    let period = period_of(t);
    let usage = ctx.db.usage(&nest.id, &period).unwrap_or_default();
    let month_start = month_start(t);
    let running_secs = ctx.db.running_secs(&nest.id, month_start).unwrap_or(0);
    json!({
        "nest_id": nest.id,
        "account_id": nest.account_id,
        "address": nest.address,
        "label": nest.label,
        "state": nest.state,
        "desired": nest.desired,
        "stop_reason": nest.stop_reason,
        "generation": nest.generation,
        "runtime": ctx.sandbox.kind(),
        "quota_gib": nest.quota_gib,
        "created_at": nest.created_at,
        "started_at": nest.started_at,
        "last_active_at": nest.last_active_at,
        "last_error": nest.last_error,
        "usage": {
            "period": usage.period,
            "input_tokens": {"used": usage.input_used, "reserved": usage.input_reserved, "limit": nest.input_limit},
            "output_tokens": {"used": usage.output_used, "reserved": usage.output_reserved, "limit": nest.output_limit},
            "requests": usage.requests,
            "platform_error_tokens": {"input": usage.platform_input, "output": usage.platform_output},
            "running_secs": running_secs,
        },
    })
}

/// Unix time of the first second of the current UTC month.
fn month_start(t: i64) -> i64 {
    let period = period_of(t);
    let mut day = t - t.rem_euclid(86_400);
    while period_of(day - 1) == period {
        day -= 86_400;
    }
    day
}

// ---- Reconciliation and idle stop --------------------------------------------------------------

/// Bring the host in line with the ledger. Runs at start (after a reboot nothing is running and
/// every volume is closed) and periodically.
pub async fn reconcile(ctx: &Ctx) -> Result<(), String> {
    let nests = ctx.db.nests(true)?;
    let present = {
        let s = ctx.sandbox.clone();
        blocking(move || s.list()).await?
    };
    // Sandboxes the ledger does not know, or of a stale generation, are removed.
    for (id, _state, generation) in &present {
        let known = nests.iter().find(|n| &n.id == id);
        let stale = match known {
            None => true,
            Some(n) => n.state == "deleted" || n.generation != *generation,
        };
        if stale {
            let s = ctx.sandbox.clone();
            let n = id.clone();
            let _ = blocking(move || s.stop(&n)).await;
            ctx.db
                .event(Some(id), "reconcile_removed", "orphan or stale sandbox");
            tracing::warn!(nest = %id, "removed orphan or stale sandbox");
        }
    }
    let present: HashMap<String, RuntimeState> = {
        let s = ctx.sandbox.clone();
        blocking(move || s.list())
            .await?
            .into_iter()
            .map(|(id, st, _)| (id, st))
            .collect()
    };
    for nest in nests.iter().filter(|n| n.state != "deleted") {
        let lock = ctx.lock_for(&nest.id);
        let _guard = lock.lock().await;
        // Re-read under the lock: an API call may have changed it.
        let Some(nest) = ctx.db.nest(&nest.id)? else {
            continue;
        };
        if !ctx.db.acquire_lease(&nest.id, &ctx.holder)? {
            continue;
        }
        let runtime = present
            .get(&nest.id)
            .copied()
            .unwrap_or(RuntimeState::Absent);
        let result = match (nest.desired.as_str(), runtime) {
            ("deleted", _) => {
                drop(_guard);
                delete(ctx, &nest.id).await.map(|_| ())
            }
            ("running", RuntimeState::Running) => Ok(()),
            ("running", RuntimeState::Paused) => {
                let s = ctx.sandbox.clone();
                let n = nest.id.clone();
                blocking(move || s.resume(&n))
                    .await
                    .map_err(ApiError::internal)
            }
            ("running", _) if nest.state == "failed" => Ok(()),
            ("running", _) => {
                // Crash loop guard: a sandbox that died within two minutes of starting is marked
                // failed instead of restarted forever.
                let recent = nest.started_at.is_some_and(|s| now() - s < 120);
                if runtime == RuntimeState::Exited && recent {
                    let s = ctx.sandbox.clone();
                    let n = nest.id.clone();
                    let _ = blocking(move || s.stop(&n)).await;
                    let _ = ctx.db.revoke_broker_token(&nest.id);
                    let _ = ctx.db.close_runs(&nest.id, "crashed");
                    ctx.db
                        .set_state(
                            &nest.id,
                            "failed",
                            "running",
                            Some("crashed"),
                            Some("sandbox exited soon after start"),
                        )
                        .map_err(ApiError::internal)
                } else {
                    let _ = ctx.db.close_runs(&nest.id, "reconcile_restart");
                    ctx.db
                        .event(Some(&nest.id), "reconcile_start", &format!("{runtime:?}"));
                    start_locked(ctx, &nest.id).await
                }
            }
            ("paused", RuntimeState::Running) => {
                let s = ctx.sandbox.clone();
                let n = nest.id.clone();
                blocking(move || s.pause(&n))
                    .await
                    .map_err(ApiError::internal)
            }
            ("paused", RuntimeState::Absent | RuntimeState::Exited) => {
                // A paused Nest whose sandbox is gone (reboot) stays paused; resume starts it.
                if nest.state != "paused" || runtime == RuntimeState::Exited {
                    let s = ctx.sandbox.clone();
                    let n = nest.id.clone();
                    let _ = blocking(move || s.stop(&n)).await;
                    let _ = ctx.db.close_runs(&nest.id, "reconcile");
                }
                Ok(())
            }
            ("stopped", RuntimeState::Absent) => Ok(()),
            ("stopped", _) => {
                stop_locked(
                    ctx,
                    &nest.id,
                    nest.stop_reason.as_deref().unwrap_or("operator"),
                    "stopped",
                )
                .await
            }
            _ => Ok(()),
        };
        if let Err(e) = result {
            tracing::warn!(nest = %nest.id, error = ?e, "reconcile step failed");
        }
    }
    let expired = ctx.db.expire_reservations(600)?;
    if expired > 0 {
        tracing::warn!(expired, "settled abandoned reservations as unknown");
    }
    idle_stop(ctx).await;
    Ok(())
}

/// Stop running Nests with no model traffic for `idle_stop_secs`. Their files stay; mail wakes
/// them (the postbox calls `wake`).
pub async fn idle_stop(ctx: &Ctx) {
    let limit = ctx.cfg.control.idle_stop_secs as i64;
    if limit == 0 {
        return;
    }
    let Ok(nests) = ctx.db.nests(false) else {
        return;
    };
    let t = now();
    for nest in nests.into_iter().filter(|n| n.state == "running") {
        let last = nest
            .last_active_at
            .unwrap_or(0)
            .max(nest.started_at.unwrap_or(0));
        if t - last < limit {
            continue;
        }
        let lock = ctx.lock_for(&nest.id);
        let _guard = lock.lock().await;
        // A reservation in flight means a request is running right now.
        let busy = ctx
            .db
            .usage(&nest.id, &period_of(t))
            .map(|u| u.input_reserved > 0)
            .unwrap_or(true);
        if busy {
            continue;
        }
        if let Err(e) = stop_locked(ctx, &nest.id, "idle", "stopped").await {
            tracing::warn!(nest = %nest.id, error = ?e, "idle stop failed");
        } else {
            tracing::info!(nest = %nest.id, "idle stop");
        }
    }
}

// ---- HTTP --------------------------------------------------------------------------------------

fn authorized(ctx: &Ctx, headers: &HeaderMap) -> Result<(), ApiError> {
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if !presented.is_empty() && ct_eq(&sha256_hex(presented.as_bytes()), &ctx.token_sha256) {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "service token required",
        ))
    }
}

pub fn router(ctx: Ctx) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/nests", post(http_create).get(http_list))
        .route("/v1/nests/{id}", get(http_get).delete(http_delete))
        .route("/v1/nests/{id}/{action}", post(http_action))
        .layer(axum::extract::DefaultBodyLimit::max(16 * 1024))
        .with_state(ctx)
}

async fn health(State(ctx): State<Ctx>, headers: HeaderMap) -> ApiResult {
    authorized(&ctx, &headers)?;
    let nests = ctx.db.nests(false).map_err(ApiError::internal)?;
    let running = nests.iter().filter(|n| n.state == "running").count();
    Ok(Json(json!({
        "ok": true,
        "runtime": ctx.sandbox.kind(),
        "nests": nests.len(),
        "running": running,
        "max_running": ctx.cfg.control.max_running,
    })))
}

async fn http_create(
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    Json(req): Json<CreateNest>,
) -> ApiResult {
    authorized(&ctx, &headers)?;
    create(&ctx, req).await
}

async fn http_list(State(ctx): State<Ctx>, headers: HeaderMap) -> ApiResult {
    authorized(&ctx, &headers)?;
    let nests = ctx.db.nests(false).map_err(ApiError::internal)?;
    Ok(Json(json!({
        "nests": nests.iter().map(|n| status_json(&ctx, n)).collect::<Vec<_>>()
    })))
}

async fn http_get(
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    authorized(&ctx, &headers)?;
    let mut v = nest_json(&ctx, &id)?.0;
    let s = ctx.sandbox.clone();
    let vols = ctx.volumes.clone();
    let n = id.clone();
    if let Ok((stats, disk)) = blocking(move || Ok((s.stats(&n), vols.usage(&n)))).await {
        v["stats"] = json!(stats);
        if let Some((used, size)) = disk {
            v["disk"] = json!({"used_bytes": used, "size_bytes": size});
        }
    }
    Ok(Json(v).into_response())
}

async fn http_action(
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    Path((id, act)): Path<(String, String)>,
) -> ApiResult {
    authorized(&ctx, &headers)?;
    action(&ctx, &id, &act).await
}

async fn http_delete(
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    authorized(&ctx, &headers)?;
    delete(&ctx, &id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::Fake;
    use crate::storage::DirVolumes;

    fn ctx() -> (Ctx, Arc<Fake>) {
        let dir = std::env::temp_dir().join(format!("nestd-test-{}", random_hex(6)));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = crate::config::test_config(&dir);
        let fake = Arc::new(Fake::default());
        let vols = Arc::new(DirVolumes::new(dir.join("mnt"), 10001));
        let c = Ctx::new(
            cfg,
            Db::open_memory().unwrap(),
            fake.clone(),
            vols,
            sha256_hex(b"nst_secret_token"),
            "test-host".into(),
        );
        (c, fake)
    }

    fn req(id: &str) -> CreateNest {
        CreateNest {
            nest_id: id.into(),
            account_id: "acc_1".into(),
            address: "/k/abc".into(),
            label: "Mira".into(),
            enrollment_code: format!("nenr_{}", "a".repeat(64)),
            budgets: None,
            quota_gib: None,
        }
    }

    #[tokio::test]
    async fn lifecycle_end_to_end_on_the_fake_runtime() {
        let (c, fake) = ctx();
        let id = "nst_00000000000000c1";
        let v = create(&c, req(id)).await.unwrap().0;
        assert_eq!(v["state"], "running");
        assert_eq!(v["generation"], 1);
        assert_eq!(fake.state(id).unwrap(), RuntimeState::Running);
        // The code and the token are on the volume for the sandbox, nowhere in the response.
        let root = c.volumes.open(id).unwrap();
        assert!(std::fs::read_to_string(root.join("run/enroll.json"))
            .unwrap()
            .contains("nenr_"));
        let token = std::fs::read_to_string(root.join("run/broker-token")).unwrap();
        assert!(!v.to_string().contains(&token));
        // Idempotent create.
        assert_eq!(create(&c, req(id)).await.unwrap().0["generation"], 1);

        let v = action(&c, id, "pause").await.unwrap().0;
        assert_eq!(v["state"], "paused");
        assert_eq!(fake.state(id).unwrap(), RuntimeState::Paused);
        let v = action(&c, id, "resume").await.unwrap().0;
        assert_eq!(v["state"], "running");

        let v = action(&c, id, "stop").await.unwrap().0;
        assert_eq!(v["state"], "stopped");
        // An operator stop is not woken by mail.
        assert_eq!(action(&c, id, "wake").await.unwrap().0["state"], "stopped");
        let v = action(&c, id, "start").await.unwrap().0;
        assert_eq!(v["generation"], 2);

        let v = delete(&c, id).await.unwrap().0;
        assert_eq!(v["state"], "deleted");
        assert_eq!(fake.state(id).unwrap(), RuntimeState::Absent);
        assert!(c.volumes.open(id).is_err());
        assert_eq!(delete(&c, id).await.unwrap().0["state"], "deleted");
        assert!(action(&c, id, "start").await.is_err());
    }

    #[tokio::test]
    async fn admission_limits_storage_and_running_count() {
        let (c, _) = ctx();
        // test config: 1 GiB each, 3 GiB total, 2 running.
        let _ = create(&c, req("nst_00000000000000d1")).await.unwrap();
        let _ = create(&c, req("nst_00000000000000d2")).await.unwrap();
        let e = create(&c, req("nst_00000000000000d3")).await.unwrap_err();
        assert_eq!(e.1, "at_capacity"); // provisioned, waits for a free slot
        let e = create(&c, req("nst_00000000000000d4")).await.unwrap_err();
        assert_eq!(e.1, "no_storage");
    }

    #[tokio::test]
    async fn idle_nests_stop_and_wake() {
        let (mut c, fake) = ctx();
        let id = "nst_00000000000000e1";
        let _ = create(&c, req(id)).await.unwrap();
        let mut cfg = (*c.cfg).clone();
        cfg.control.idle_stop_secs = 1;
        c.cfg = Arc::new(cfg);
        tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
        idle_stop(&c).await;
        let n = c.db.nest(id).unwrap().unwrap();
        assert_eq!(
            (n.state.as_str(), n.stop_reason.as_deref()),
            ("stopped", Some("idle"))
        );
        assert_eq!(fake.state(id).unwrap(), RuntimeState::Absent);
        let v = action(&c, id, "wake").await.unwrap().0;
        assert_eq!(v["state"], "running");
    }

    #[tokio::test]
    async fn reconcile_restarts_after_a_reboot_and_removes_orphans() {
        let (c, fake) = ctx();
        let id = "nst_00000000000000f1";
        let _ = create(&c, req(id)).await.unwrap();
        // "Reboot": every sandbox is gone; an unknown one appears.
        fake.boxes.lock().unwrap().clear();
        fake.boxes
            .lock()
            .unwrap()
            .insert("nst_00000000000000ff".into(), (RuntimeState::Running, 1));
        // Started long enough ago not to look like a crash loop.
        c.db.with_test_started_at(id, now() - 3600);
        reconcile(&c).await.unwrap();
        assert_eq!(fake.state(id).unwrap(), RuntimeState::Running);
        assert_eq!(
            fake.state("nst_00000000000000ff").unwrap(),
            RuntimeState::Absent
        );
        assert_eq!(c.db.nest(id).unwrap().unwrap().generation, 2);
    }

    // Drafted by the local model (docdex delegation), corrected and reviewed.
    #[test]
    fn enrollment_codes_are_strict() {
        assert!(valid_code(&format!("nenr_{}", "a".repeat(32))));
        assert!(valid_code(&format!("nenr_{}", "f".repeat(128))));
        assert!(!valid_code(&format!("nenr_{}", "a".repeat(31))));
        assert!(!valid_code(&format!("nenr_{}", "a".repeat(129))));
        assert!(!valid_code(&format!("nenr_{}", "g".repeat(32))));
        assert!(!valid_code(&"a".repeat(37)));
    }

    #[test]
    fn month_start_is_the_first_second_of_the_utc_month() {
        // 2026-11-03T06:00Z -> 2026-11-01T00:00Z
        assert_eq!(month_start(1_793_685_600), 1_793_491_200);
        // 2027-01-01T00:00Z is its own month start.
        assert_eq!(month_start(1_798_761_600), 1_798_761_600);
        let t = 1_793_685_600;
        assert_eq!(period_of(month_start(t)), period_of(t));
        assert_ne!(period_of(month_start(t) - 1), period_of(t));
    }

    #[tokio::test]
    async fn the_api_requires_the_service_token() {
        let (c, _) = ctx();
        let mut h = HeaderMap::new();
        assert!(authorized(&c, &h).is_err());
        h.insert("authorization", "Bearer wrong".parse().unwrap());
        assert!(authorized(&c, &h).is_err());
        h.insert("authorization", "Bearer nst_secret_token".parse().unwrap());
        assert!(authorized(&c, &h).is_ok());
    }
}
