//! The Nest control service: lifecycle, admission, idle stop and reconciliation, behind a private
//! API that only the postbox can call.

use crate::config::Config;
use crate::db::Db;
use crate::sandbox::{RuntimeState, Sandbox, Spec};
use crate::storage::{dir_for_sandbox, write_for_sandbox, Volumes};
use crate::util::{ct_eq, now, period_of, random_hex, sha256_hex, valid_nest_id};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
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
    /// The off-host KEK, once the postbox released it (Phase B6).
    pub vault: Arc<crate::keywrap::Vault>,
    /// Unix time of the last successful `POST /v1/unlock` (0 = none since start), exported as
    /// `nests_postbox_last_unlock_seconds`: the postbox calls every minute, so silence means the
    /// postbox (or the tunnel to it) is down.
    pub postbox_last_unlock: Arc<std::sync::atomic::AtomicI64>,
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
            vault: Arc::new(crate::keywrap::Vault::new()),
            postbox_last_unlock: Arc::new(std::sync::atomic::AtomicI64::new(0)),
            locks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Share `vault` with the volume backend (production: both hold the same KEK).
    pub fn with_vault(mut self, vault: Arc<crate::keywrap::Vault>) -> Self {
        self.vault = vault;
        self
    }

    /// Whether volumes cannot be opened yet: keys are wrapped and the KEK not released.
    pub fn locked(&self) -> bool {
        self.cfg.storage.backend == "lvm-luks" && self.cfg.storage.key_wrap && self.vault.locked()
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

fn refuse_locked(ctx: &Ctx) -> Result<(), ApiError> {
    if ctx.locked() {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "locked",
            "the host key has not been released yet",
        ));
    }
    Ok(())
}

pub async fn create(ctx: &Ctx, req: CreateNest) -> ApiResult {
    refuse_locked(ctx)?;
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
            buyer_context: None,
            image: None,
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
    // Locked after a restart: nothing that needs a volume key can happen yet, and failing every
    // start would mark healthy Nests failed. Running sandboxes keep running meanwhile.
    if ctx.locked() {
        tracing::warn!("locked: waiting for the postbox to release the host key");
        return Ok(());
    }
    // Marketplace workers belong to the dispatcher and its pool (M1): never started, stopped or
    // adopted here.
    let workers = ctx.db.mp_worker_ids()?;
    let nests: Vec<_> = ctx
        .db
        .nests(true)?
        .into_iter()
        .filter(|n| !workers.contains(&n.id))
        .collect();
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
    let Ok(workers) = ctx.db.mp_worker_ids() else {
        return;
    };
    let t = now();
    for nest in nests
        .into_iter()
        .filter(|n| n.state == "running" && !workers.contains(&n.id))
    {
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
    let files = Router::new()
        .route(
            "/v1/nests/{id}/files/{file}",
            axum::routing::put(http_upload).delete(http_remove_file),
        )
        .route("/v1/nests/{id}/export", get(http_export))
        .layer(axum::extract::DefaultBodyLimit::max(
            crate::parse::MAX_INPUT_BYTES + 1,
        ));
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/unlock", post(http_unlock))
        .route("/v1/nests", post(http_create).get(http_list))
        .route("/v1/nests/{id}", get(http_get).delete(http_delete))
        .route("/v1/nests/{id}/{action}", post(http_action))
        .layer(axum::extract::DefaultBodyLimit::max(16 * 1024))
        .merge(files)
        .with_state(ctx)
}

// ---- unlock and tombstones (Phase B6) -----------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnlockReq {
    /// The host KEK, 64 hex characters.
    pub kek: String,
    /// Nests the postbox has deleted. Any of them still present here (a restored ledger or volume,
    /// a missed delete) is destroyed now, so a revoked Nest stays revoked after a restore.
    #[serde(default)]
    pub tombstones: Vec<String>,
}

fn fpr_path(ctx: &Ctx) -> PathBuf {
    ctx.cfg
        .storage
        .key_dir
        .parent()
        .map(|p| p.join("kek.fpr"))
        .unwrap_or_else(|| PathBuf::from("kek.fpr"))
}

/// `POST /v1/unlock` — the postbox releases the KEK (idempotent; a different KEK is refused) and
/// replays deletion tombstones.
pub async fn unlock(ctx: &Ctx, req: UnlockReq) -> ApiResult {
    let mut kek = crate::keywrap::parse_kek(&req.kek)
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "bad_kek", "64 hex characters"))?;
    let path = fpr_path(ctx);
    let expected = std::fs::read_to_string(&path)
        .ok()
        .map(|s| s.trim().to_string());
    let result = ctx.vault.unlock(kek, expected.as_deref());
    zeroize::Zeroize::zeroize(&mut kek);
    let fpr = result.map_err(|e| ApiError::new(StatusCode::FORBIDDEN, "wrong_kek", e))?;
    if expected.is_none() {
        crate::storage::write_private(&path, fpr.as_bytes()).map_err(ApiError::internal)?;
    }
    let mut destroyed = Vec::new();
    for id in req.tombstones.iter().take(10_000) {
        if !valid_nest_id(id) {
            continue;
        }
        let present = ctx
            .db
            .nest(id)
            .map_err(ApiError::internal)?
            .is_some_and(|n| n.state != "deleted");
        if present {
            match delete(ctx, id).await {
                Ok(_) => {
                    ctx.db.event(
                        Some(id),
                        "tombstone",
                        "deleted by the postbox; destroyed here",
                    );
                    destroyed.push(id.clone());
                }
                Err(e) => {
                    tracing::warn!(nest = %id, error = ?e, "tombstone replay failed; will retry")
                }
            }
        }
    }
    ctx.postbox_last_unlock
        .store(crate::util::now(), std::sync::atomic::Ordering::Relaxed);
    Ok(Json(
        json!({ "locked": ctx.locked(), "fingerprint": fpr, "tombstoned": destroyed }),
    ))
}

async fn http_unlock(
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    Json(req): Json<UnlockReq>,
) -> ApiResult {
    authorized(&ctx, &headers)?;
    let first = ctx.locked();
    let out = unlock(&ctx, req).await?;
    if first && !ctx.locked() {
        tracing::info!("unlocked by the postbox; reconciling");
        let c = ctx.clone();
        tokio::spawn(async move {
            if let Err(e) = reconcile(&c).await {
                tracing::warn!(error = %e, "reconcile after unlock failed");
            }
        });
    }
    Ok(out)
}

// ---- files (Phase B4) --------------------------------------------------------------------------

/// How long one parse may take before its sandbox is killed.
const PARSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Run `f` with the Nest's volume open, closing it again afterwards when the Nest was not running
/// (an idle-stopped Nest's volume stays locked between uses). Caller holds the Nest's lock.
async fn with_volume<T: Send + 'static>(
    ctx: &Ctx,
    id: &str,
    f: impl FnOnce(PathBuf) -> Result<T, String> + Send + 'static,
) -> Result<T, ApiError> {
    let nest = ctx
        .db
        .nest(id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no_nest", id.to_string()))?;
    if matches!(nest.state.as_str(), "deleted" | "deleting" | "creating") {
        return Err(ApiError::new(StatusCode::CONFLICT, "not_ready", nest.state));
    }
    let open_now = matches!(nest.state.as_str(), "running" | "paused");
    let volumes = ctx.volumes.clone();
    let nest_id = id.to_string();
    blocking(move || {
        let root = volumes.open(&nest_id)?;
        let out = f(root);
        if !open_now {
            let _ = volumes.close(&nest_id);
        }
        out
    })
    .await
    .map_err(|e| {
        if e.starts_with("no_space") {
            ApiError::new(
                StatusCode::INSUFFICIENT_STORAGE,
                "no_space",
                "this Nest's storage is full",
            )
        } else {
            ApiError::internal(e)
        }
    })
}

/// `PUT /v1/nests/{id}/files/{file}` — store one upload in quarantine, parse it in its own
/// sandbox, and keep only verified output. Answers the manifest without any text.
pub async fn upload(ctx: &Ctx, id: &str, file_id: &str, name: String, data: Vec<u8>) -> ApiResult {
    if !valid_nest_id(id) || !crate::files::valid_file_id(file_id) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_id",
            "nest or file id",
        ));
    }
    if data.is_empty() || data.len() > crate::parse::MAX_INPUT_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            "1 byte to 25 MiB",
        ));
    }
    let lock = ctx.lock_for(id);
    let _guard = lock.lock().await;
    let uid = ctx.cfg.storage.sandbox_uid;
    let sandbox = ctx.sandbox.clone();
    let volumes = ctx.volumes.clone();
    let (nest, file) = (id.to_string(), file_id.to_string());
    let sha = sha256_hex(&data);
    let bytes = data.len();
    let manifest = with_volume(ctx, id, move |root| {
        // Room for the original, its quarantine copy and the extracted text.
        if let Some((used, size)) = volumes.usage(&nest) {
            if used + 3 * bytes as u64 > size {
                return Err("no_space".into());
            }
        }
        let p = crate::files::paths(&root, &file);
        for d in ["files", "files/quarantine", "files/originals", "files/parsed"] {
            dir_for_sandbox(&root.join(d), uid, 0o700)?;
        }
        if p.parsed.exists() || p.original.exists() {
            return Err("file id already used".into());
        }
        write_for_sandbox(&p.quarantine, &data, uid)?;
        let work = p.parsed.with_extension("tmp");
        let _ = std::fs::remove_dir_all(&work);
        dir_for_sandbox(&work, uid, 0o700)?;
        let parsed = sandbox
            .run_parser(&p.quarantine, &work, PARSE_TIMEOUT)
            .and_then(|()| crate::files::verify_output(&work));
        let manifest = match parsed {
            Ok(m) => {
                std::fs::rename(&work, &p.parsed).map_err(|e| e.to_string())?;
                let meta = json!({"name": crate::parse::display_name(&name), "bytes": bytes, "sha256": sha, "uploaded_at": now()});
                write_for_sandbox(&p.parsed.join("meta.json"), meta.to_string().as_bytes(), uid)?;
                m
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(&work);
                tracing::warn!(nest = %nest, file = %file, error = %e, "file parse failed");
                let mut m = crate::parse::parse(b"");
                m.reason = Some("the file could not be parsed safely".into());
                m.bytes = bytes;
                m
            }
        };
        std::fs::rename(&p.quarantine, &p.original).map_err(|e| e.to_string())?;
        Ok(manifest)
    })
    .await?;
    ctx.db.event(
        Some(id),
        "file",
        &format!("{file_id} {:?}", manifest.status),
    );
    Ok(Json(json!({
        "file_id": file_id,
        "kind": manifest.kind,
        "status": manifest.status,
        "reason": manifest.reason,
        "bytes": manifest.bytes,
        "coverage": manifest.coverage,
        "parts": manifest.parts.len(),
        "chars": manifest.parts.iter().map(|p| p.chars).sum::<usize>(),
        "warnings": manifest.warnings,
    })))
}

/// `DELETE /v1/nests/{id}/files/{file}` — remove the original and everything parsed from it.
/// Idempotent.
pub async fn remove_file(ctx: &Ctx, id: &str, file_id: &str) -> ApiResult {
    if !valid_nest_id(id) || !crate::files::valid_file_id(file_id) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_id",
            "nest or file id",
        ));
    }
    let lock = ctx.lock_for(id);
    let _guard = lock.lock().await;
    let file = file_id.to_string();
    with_volume(ctx, id, move |root| {
        let p = crate::files::paths(&root, &file);
        let _ = std::fs::remove_file(&p.quarantine);
        let _ = std::fs::remove_file(&p.original);
        let _ = std::fs::remove_dir_all(&p.parsed);
        let _ = std::fs::remove_dir_all(p.parsed.with_extension("tmp"));
        Ok(())
    })
    .await?;
    ctx.db.event(Some(id), "file_removed", file_id);
    Ok(Json(json!({"file_id": file_id, "removed": true})))
}

async fn http_upload(
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    Path((id, file)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> ApiResult {
    authorized(&ctx, &headers)?;
    // The display name travels percent-encoded in a header; it is only ever shown, never a path.
    let name = headers
        .get("x-nest-file-name")
        .and_then(|v| v.to_str().ok())
        .map(percent_decode)
        .unwrap_or_default();
    upload(&ctx, &id, &file, name, body.to_vec()).await
}

async fn http_remove_file(
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    Path((id, file)): Path<(String, String)>,
) -> ApiResult {
    authorized(&ctx, &headers)?;
    remove_file(&ctx, &id, &file).await
}

// ---- export (Phase B5) -------------------------------------------------------------------------

/// Build the owner's export of one Nest: every original file under its display name, the text
/// parsed from it, and each conversation's history, as one zip held in memory (never written to
/// the host disk, which is not encrypted). Bounded by [`MAX_EXPORT_BYTES`] of originals and one
/// export at a time on the host; a larger Nest is exported by ops from its volume.
pub fn build_export(root: &FsPath) -> Result<Vec<u8>, String> {
    use std::io::Write;
    let mut used = std::collections::HashSet::new();
    let mut unique = |wanted: String| -> String {
        let clean: String = wanted
            .chars()
            .map(|c| {
                if c.is_control() || matches!(c, '/' | '\\' | ':') {
                    '_'
                } else {
                    c
                }
            })
            .take(120)
            .collect::<String>()
            .replace("..", "_");
        let clean = if clean.trim().is_empty() || clean.starts_with('.') {
            format!("file{clean}")
        } else {
            clean
        };
        let mut name = clean.clone();
        let mut n = 2;
        while !used.insert(name.clone()) {
            name = format!("{n}-{clean}");
            n += 1;
        }
        name
    };
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .large_file(true);
        let mut index = Vec::new();
        let files = root.join("files");
        if let Ok(entries) = std::fs::read_dir(files.join("originals")) {
            let mut ids: Vec<String> = entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            ids.sort();
            for id in ids {
                if !crate::files::valid_file_id(&id) {
                    continue;
                }
                let p = crate::files::paths(root, &id);
                let meta: serde_json::Value = std::fs::read(p.parsed.join("meta.json"))
                    .ok()
                    .and_then(|m| serde_json::from_slice(&m).ok())
                    .unwrap_or_default();
                let name = unique(meta["name"].as_str().unwrap_or(&id).to_string());
                let data = std::fs::read(&p.original).map_err(|e| e.to_string())?;
                zip.start_file(format!("files/{name}"), opts)
                    .map_err(|e| e.to_string())?;
                zip.write_all(&data).map_err(|e| e.to_string())?;
                let mut parts: Vec<PathBuf> = std::fs::read_dir(&p.parsed)
                    .map(|d| {
                        d.flatten()
                            .map(|e| e.path())
                            .filter(|q| q.extension().is_some_and(|x| x == "txt"))
                            .collect()
                    })
                    .unwrap_or_default();
                parts.sort();
                if !parts.is_empty() {
                    let mut text = String::new();
                    for part in parts {
                        text.push_str(&std::fs::read_to_string(part).unwrap_or_default());
                        text.push('\n');
                    }
                    zip.start_file(format!("text/{name}.txt"), opts)
                        .map_err(|e| e.to_string())?;
                    zip.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
                }
                index.push(json!({"file": format!("files/{name}"), "bytes": data.len(), "sha256": meta["sha256"], "uploaded_at": meta["uploaded_at"]}));
            }
        }
        if let Ok(entries) = std::fs::read_dir(root.join("chat")) {
            let mut chats: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
                .collect();
            chats.sort();
            for (i, chat) in chats.iter().enumerate() {
                let data = std::fs::read(chat).map_err(|e| e.to_string())?;
                zip.start_file(format!("conversations/conversation-{}.jsonl", i + 1), opts)
                    .map_err(|e| e.to_string())?;
                zip.write_all(&data).map_err(|e| e.to_string())?;
            }
        }
        zip.start_file("index.json", opts)
            .map_err(|e| e.to_string())?;
        zip.write_all(
            json!({"exported_at": now(), "files": index})
                .to_string()
                .as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        zip.finish().map_err(|e| e.to_string())?;
    }
    Ok(buf.into_inner())
}

/// Most bytes of originals one download may carry (it is built in memory on a host with ~4.8 GiB).
const MAX_EXPORT_BYTES: u64 = 512 * 1024 * 1024;
/// One export at a time on the host.
static EXPORTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

fn originals_bytes(root: &FsPath) -> u64 {
    std::fs::read_dir(root.join("files").join("originals"))
        .map(|d| {
            d.flatten()
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

pub async fn export(ctx: &Ctx, id: &str) -> Result<Vec<u8>, ApiError> {
    if !valid_nest_id(id) {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "bad_id", "nest id"));
    }
    let _one = EXPORTS
        .acquire()
        .await
        .map_err(|_| ApiError::internal("export semaphore closed"))?;
    let lock = ctx.lock_for(id);
    let _guard = lock.lock().await;
    with_volume(ctx, id, move |root| {
        if originals_bytes(&root) > MAX_EXPORT_BYTES {
            return Err("too_large_for_download".into());
        }
        build_export(&root)
    })
    .await
    .map_err(|e| {
        if e.2.contains("too_large_for_download") {
            ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "export_too_large",
                "this Nest is larger than one download; contact support",
            )
        } else {
            e
        }
    })
}

async fn http_export(
    State(ctx): State<Ctx>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(e) = authorized(&ctx, &headers) {
        return e.into_response();
    }
    match export(&ctx, &id).await {
        Ok(bytes) => (
            [(axum::http::header::CONTENT_TYPE, "application/zip")],
            bytes,
        )
            .into_response(),
        Err(e) => e.into_response(),
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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
        "locked": ctx.locked(),
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
    let workers = ctx.db.mp_worker_ids().map_err(ApiError::internal)?;
    let nests = ctx.db.nests(false).map_err(ApiError::internal)?;
    Ok(Json(json!({
        "nests": nests
            .iter()
            .filter(|n| !workers.contains(&n.id))
            .map(|n| status_json(&ctx, n))
            .collect::<Vec<_>>()
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

    /// Marketplace workers (M1) live in the `nests` ledger so the broker meters them, but the
    /// first-party lifecycle never starts, stops or lists them: they belong to the dispatcher.
    #[tokio::test]
    async fn the_first_party_lifecycle_leaves_marketplace_workers_alone() {
        let (c, fake) = ctx();
        let worker = crate::db::MpWorker {
            nest_id: "nst_00000000000000e1".into(),
            listing_id: "lst_1".into(),
            buyer_context: "ctx-a".into(),
            release_id: "rel_1".into(),
        };
        c.db.mp_admit("rel_1", "x@sha256:00", "sha256:00", "sha256:00")
            .unwrap();
        c.db.mp_insert_worker(&worker, 10, 10).unwrap();
        c.db.set_state(&worker.nest_id, "running", "running", None, None)
            .unwrap();
        reconcile(&c).await.unwrap();
        assert_eq!(fake.state(&worker.nest_id).unwrap(), RuntimeState::Absent);
        let row = c.db.nest(&worker.nest_id).unwrap().unwrap();
        assert_eq!((row.state.as_str(), row.generation), ("running", 0));
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer nst_secret_token".parse().unwrap(),
        );
        let listed = http_list(State(c.clone()), headers).await.unwrap().0;
        assert_eq!(listed["nests"], json!([]), "{listed}");
    }

    #[tokio::test]
    async fn unlock_records_the_kek_refuses_another_and_replays_tombstones() {
        let (c, fake) = ctx();
        let id = "nst_00000000000000d1";
        let _ = create(&c, req(id)).await.unwrap();
        let kek = "11".repeat(32);
        let v = unlock(
            &c,
            UnlockReq {
                kek: kek.clone(),
                tombstones: vec![id.into(), "bad/../id".into()],
            },
        )
        .await
        .unwrap()
        .0;
        assert_eq!(v["tombstoned"], json!([id]));
        assert_eq!(c.db.nest(id).unwrap().unwrap().state, "deleted");
        assert!(fake.state(id).unwrap() == RuntimeState::Absent);
        // The same key again is fine; another one is refused, also after a restart.
        assert!(unlock(
            &c,
            UnlockReq {
                kek: kek.clone(),
                tombstones: vec![]
            }
        )
        .await
        .is_ok());
        assert!(unlock(
            &c,
            UnlockReq {
                kek: "22".repeat(32),
                tombstones: vec![]
            }
        )
        .await
        .is_err());
        let restarted = Ctx::new(
            (*c.cfg).clone(),
            Db::open_memory().unwrap(),
            fake.clone(),
            c.volumes.clone(),
            sha256_hex(b"x"),
            "h".into(),
        );
        assert!(unlock(
            &restarted,
            UnlockReq {
                kek: "22".repeat(32),
                tombstones: vec![]
            }
        )
        .await
        .is_err());
        assert!(unlock(
            &restarted,
            UnlockReq {
                kek,
                tombstones: vec![]
            }
        )
        .await
        .is_ok());
    }

    #[tokio::test]
    async fn metrics_export_the_last_successful_postbox_unlock() {
        let (c, _fake) = ctx();
        let m = crate::metrics::render(&c);
        assert!(m.contains("\nnests_postbox_last_unlock_seconds 0\n"), "{m}");
        // A refused key is not a successful call.
        let _ = unlock(
            &c,
            UnlockReq {
                kek: "zz".into(),
                tombstones: vec![],
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            c.postbox_last_unlock
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        let before = crate::util::now();
        let _ = unlock(
            &c,
            UnlockReq {
                kek: "33".repeat(32),
                tombstones: vec![],
            },
        )
        .await
        .unwrap();
        let t = c
            .postbox_last_unlock
            .load(std::sync::atomic::Ordering::Relaxed);
        assert!(t >= before && t <= crate::util::now(), "{t}");
        let m = crate::metrics::render(&c);
        assert!(
            m.contains(&format!("\nnests_postbox_last_unlock_seconds {t}\n")),
            "{m}"
        );
    }

    #[tokio::test]
    async fn metrics_count_states_and_never_name_a_nest() {
        let (c, _fake) = ctx();
        let _ = create(&c, req("nst_00000000000000a7")).await.unwrap();
        let m = crate::metrics::render(&c);
        assert!(m.contains("nests_total{state=\"running\"} 1"), "{m}");
        assert!(m.contains("nests_locked 0"));
        assert!(m.contains("nests_volume_allocated_gib 1"));
        assert!(
            !m.contains("nst_") && !m.contains("acc_1") && !m.contains("/k/"),
            "{m}"
        );
    }

    #[tokio::test]
    async fn a_locked_host_creates_and_starts_nothing() {
        let (c, _fake) = ctx();
        let mut cfg = (*c.cfg).clone();
        cfg.storage.backend = "lvm-luks".into();
        cfg.storage.key_wrap = true;
        let locked = Ctx::new(
            cfg,
            Db::open_memory().unwrap(),
            Arc::new(Fake::default()),
            c.volumes.clone(),
            sha256_hex(b"x"),
            "h".into(),
        );
        assert!(locked.locked());
        let e = create(&locked, req("nst_00000000000000d2"))
            .await
            .unwrap_err();
        assert_eq!(e.1, "locked");
        assert!(
            reconcile(&locked).await.is_ok(),
            "waits rather than failing Nests"
        );
        let _ = unlock(
            &locked,
            UnlockReq {
                kek: "33".repeat(32),
                tombstones: vec![],
            },
        )
        .await
        .unwrap();
        assert!(!locked.locked());
    }

    #[tokio::test]
    async fn an_export_carries_originals_text_and_conversations_under_safe_names() {
        let (c, _fake) = ctx();
        let id = "nst_00000000000000e1";
        let _ = create(&c, req(id)).await.unwrap();
        for (fid, name, data) in [
            (
                "nfl_0123456789abcdef0123456b",
                "../../etc/passwd",
                &b"one"[..],
            ),
            ("nfl_0123456789abcdef0123456c", "notes.txt", b"two"),
            ("nfl_0123456789abcdef0123456d", "notes.txt", b"three"),
        ] {
            let _ = upload(&c, id, fid, name.into(), data.to_vec())
                .await
                .unwrap();
        }
        let root = c.volumes.open(id).unwrap();
        std::fs::create_dir_all(root.join("chat")).unwrap();
        std::fs::write(
            root.join("chat/abc.jsonl"),
            "{\"u\":\"hi\",\"a\":\"hello\"}\n",
        )
        .unwrap();
        let zip_bytes = export(&c, id).await.unwrap();
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).unwrap();
        let names: Vec<String> = (0..z.len())
            .map(|i| z.by_index(i).unwrap().name().unwrap().to_string())
            .collect();
        assert!(
            names
                .iter()
                .all(|n| !n.contains("..") && !n.starts_with('/')),
            "{names:?}"
        );
        assert!(
            names.contains(&"files/notes.txt".to_string())
                && names.contains(&"files/2-notes.txt".to_string()),
            "{names:?}"
        );
        assert!(names
            .iter()
            .any(|n| n.starts_with("files/") && n.contains("etc_passwd")));
        assert!(names.contains(&"conversations/conversation-1.jsonl".to_string()));
        assert!(names.contains(&"index.json".to_string()));
        assert!(names.contains(&"text/notes.txt.txt".to_string()));
    }

    #[tokio::test]
    async fn uploads_are_quarantined_parsed_verified_and_removable() {
        let (c, _fake) = ctx();
        let id = "nst_00000000000000f1";
        let _ = create(&c, req(id)).await.unwrap();
        let fid = "nfl_0123456789abcdef01234567";
        let v = upload(&c, id, fid, "Q3 plan.txt".into(), b"budget is 42".to_vec())
            .await
            .unwrap()
            .0;
        assert_eq!(v["status"], "parsed");
        assert_eq!(v["kind"], "text");
        assert!(
            !v.to_string().contains("budget is 42"),
            "no text in the answer"
        );
        let root = c.volumes.open(id).unwrap();
        let p = crate::files::paths(&root, fid);
        assert!(p.original.exists() && !p.quarantine.exists());
        assert_eq!(
            std::fs::read_to_string(p.parsed.join("part-0001.txt")).unwrap(),
            "budget is 42"
        );
        let meta: Value =
            serde_json::from_slice(&std::fs::read(p.parsed.join("meta.json")).unwrap()).unwrap();
        assert_eq!(meta["name"], "Q3 plan.txt");
        // The same id twice is refused; bad ids and empty bodies are refused.
        assert!(upload(&c, id, fid, "x".into(), b"again".to_vec())
            .await
            .is_err());
        assert!(upload(&c, id, "nfl_../../x", "x".into(), b"a".to_vec())
            .await
            .is_err());
        assert!(upload(
            &c,
            id,
            "nfl_0123456789abcdef01234568",
            "x".into(),
            Vec::new()
        )
        .await
        .is_err());
        // An unsupported file is kept as an original but yields nothing to read.
        let bin = "nfl_0123456789abcdef0123456a";
        let v = upload(&c, id, bin, "a.bin".into(), vec![0xff, 0xfe, 0x00, 0x9b])
            .await
            .unwrap()
            .0;
        assert_eq!(v["status"], "rejected");
        // Remove is complete and idempotent.
        let _ = remove_file(&c, id, fid).await.unwrap();
        assert!(!p.original.exists() && !p.parsed.exists());
        let _ = remove_file(&c, id, fid).await.unwrap();
        assert_eq!(percent_decode("Q3%20plan%2F..%2F.txt"), "Q3 plan/../.txt");
    }

    #[test]
    fn the_parser_sandbox_has_no_network_and_reads_its_input_read_only() {
        let dir = std::env::temp_dir();
        let cfg = crate::config::test_config(&dir);
        let a = crate::sandbox::GvisorPodman::parser_args(
            &cfg.sandbox,
            "nest-parse-x",
            std::path::Path::new("/v/files/quarantine/f"),
            std::path::Path::new("/v/files/parsed/f.tmp"),
        );
        let j = a.join(" ");
        for must in [
            "--network none",
            "--read-only",
            "--cap-drop ALL",
            "--user 10001:10001",
            "no-new-privileges",
            "/v/files/quarantine/f:/in:ro,nosuid,nodev,noexec",
            "/v/files/parsed/f.tmp:/out:rw,nosuid,nodev,noexec",
            "--memory 256m",
            "--pids-limit 16",
            "--pull never",
        ] {
            assert!(j.contains(must), "missing {must}: {j}");
        }
        assert!(j.contains(&format!("--runtime {}", cfg.sandbox.oci_runtime)));
        assert!(
            !j.contains("--add-host") && !j.contains("broker"),
            "no route to anything"
        );
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
