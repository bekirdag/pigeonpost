//! The shared Nest dispatcher on the worker host (build plan M1, R-53/R-55).
//!
//! One listing serves many buyers. Each `(listing, buyer context)` pair gets its own worker in the
//! marketplace pool ([`crate::sandbox::MarketplacePool`]), so two buyers never share a sandbox, a
//! mount, a label or a broker token. The postbox decides who may run a job; this side only checks
//! that what it is asked to run is an attested, live release and keeps the pool's isolation.
//!
//! A job reaches its worker as a file in the worker's own volume (`jobs/in/<job>.json`) and comes
//! back as `jobs/out/<job>.json`: a worker's only network peer is the broker, so the dispatcher
//! never opens a connection into a sandbox and a sandbox never reaches the postbox.
//!
//! Quarantine and revocation stop every worker of the release at once, fence its broker tokens,
//! mark its running jobs `stopped_security` and refuse new ones. Revocation is permanent.

use crate::db::{Db, MpWorker};
use crate::sandbox::{pinned_image, valid_buyer_context, RuntimeState, Sandbox, Spec};
use crate::storage::{dir_for_sandbox, write_for_sandbox};
use crate::util::{ct_eq, random_hex, sha256_hex};
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Monthly model allowance of one marketplace worker. Jobs are free in M1; this bounds what a
/// shared listing can cost the platform per buyer.
pub const WORKER_INPUT_TOKENS: i64 = 200_000;
pub const WORKER_OUTPUT_TOKENS: i64 = 20_000;
/// Largest result file read back from a worker.
const MAX_RESULT_BYTES: u64 = 512 * 1024;

/// What the postbox sends to admit a release with a job: its attestation statement, verbatim.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Admission {
    pub release_id: String,
    pub image: String,
    pub release_digest: String,
    /// The canonical JSON statement the staff attestation covers.
    pub statement: String,
    /// `sha256:` of `statement`.
    pub statement_digest: String,
}

/// One job, as the postbox's dispatcher hands it over.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobIn {
    pub job_id: String,
    pub listing_id: String,
    pub buyer_context: String,
    pub operation: String,
    pub operation_version: String,
    pub payload: Value,
    pub release: Admission,
}

#[derive(Debug, PartialEq, Eq)]
pub struct DispatchError {
    pub code: &'static str,
    pub detail: String,
}

impl DispatchError {
    fn new(code: &'static str, detail: impl Into<String>) -> Self {
        DispatchError {
            code,
            detail: detail.into(),
        }
    }
    fn status(&self) -> StatusCode {
        match self.code {
            "release_quarantined" | "release_revoked" | "release_mismatch" => StatusCode::CONFLICT,
            "not_attested" | "invalid" => StatusCode::BAD_REQUEST,
            "not_found" => StatusCode::NOT_FOUND,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

fn internal(e: impl Into<String>) -> DispatchError {
    DispatchError::new("internal", e)
}

/// `job_` and 26 lower-case base32 characters, as the postbox mints them.
pub fn valid_job_id(id: &str) -> bool {
    id.strip_prefix("job_").is_some_and(|rest| {
        rest.len() == 26
            && rest
                .bytes()
                .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
    })
}

fn valid_ref(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The attestation statement must be exactly what was hashed, and must name this release, this
/// image and this digest with an approving staff decision.
pub fn verify_admission(a: &Admission) -> Result<(), DispatchError> {
    let refuse = |why: &str| Err(DispatchError::new("not_attested", why.to_string()));
    if !valid_ref(&a.release_id) || !pinned_image(&a.image) {
        return refuse("malformed release");
    }
    if a.statement_digest != format!("sha256:{}", sha256_hex(a.statement.as_bytes())) {
        return refuse("the statement does not match its digest");
    }
    let Ok(s) = serde_json::from_str::<Value>(&a.statement) else {
        return refuse("the statement is not JSON");
    };
    if s["type"] != "pigeonpost.nest.release.attestation.v1"
        || s["release_id"] != a.release_id.as_str()
        || s["release_digest"] != a.release_digest.as_str()
        || s["image"] != a.image.as_str()
        || s["decision"] != "approved"
        || !s["attester"].is_string()
    {
        return refuse("the statement does not attest this release");
    }
    if !a.image.ends_with(s["image_digest"].as_str().unwrap_or("-")) {
        return refuse("the image is not the attested digest");
    }
    Ok(())
}

/// The worker that serves one buyer context of one listing. Derived, so the same pair always
/// lands on the same worker and two pairs never share one.
pub fn worker_id(listing_id: &str, context: &str) -> String {
    let h = sha256_hex(format!("pigeonpost-mp-worker\0{listing_id}\0{context}").as_bytes());
    format!("nst_{}", &h[..32])
}

pub struct Dispatcher {
    pub pool: Arc<dyn Sandbox>,
    pub db: Db,
    /// The marketplace pool's `volume_root`.
    pub volume_root: PathBuf,
    /// The uid sandboxes run as (`storage.sandbox_uid`).
    pub uid: u32,
    /// One dispatch decision at a time: worker start and quarantine never interleave.
    lock: Mutex<()>,
}

impl Dispatcher {
    pub fn new(pool: Arc<dyn Sandbox>, db: Db, volume_root: PathBuf, uid: u32) -> Self {
        Dispatcher {
            pool,
            db,
            volume_root,
            uid,
            lock: Mutex::new(()),
        }
    }

    fn volume(&self, w: &MpWorker) -> PathBuf {
        self.volume_root.join(&w.buyer_context).join(&w.nest_id)
    }

    fn release_state_ok(&self, release_id: &str) -> Result<(), DispatchError> {
        match self.db.mp_release(release_id).map_err(internal)? {
            Some((_, _, state)) if state == "quarantined" => Err(DispatchError::new(
                "release_quarantined",
                "this release is quarantined",
            )),
            Some((_, _, state)) if state == "revoked" => Err(DispatchError::new(
                "release_revoked",
                "this release is revoked",
            )),
            _ => Ok(()),
        }
    }

    /// Start (or restart) a worker on `image` with a fresh generation and broker token.
    fn start_worker(&self, w: &MpWorker, image: &str) -> Result<(), DispatchError> {
        let volume = self.volume(w);
        for dir in [
            volume.clone(),
            volume.join("run"),
            volume.join("jobs"),
            volume.join("jobs/in"),
            volume.join("jobs/out"),
        ] {
            dir_for_sandbox(&dir, self.uid, 0o700).map_err(internal)?;
        }
        let generation = self.db.next_generation(&w.nest_id).map_err(internal)?;
        let token = format!("nbt_{}", random_hex(32));
        self.db
            .set_broker_token(&w.nest_id, &sha256_hex(token.as_bytes()), generation)
            .map_err(internal)?;
        write_for_sandbox(&volume.join("run/broker-token"), token.as_bytes(), self.uid)
            .map_err(internal)?;
        let started = self.pool.start(&Spec {
            nest_id: w.nest_id.clone(),
            generation,
            volume,
            buyer_context: Some(w.buyer_context.clone()),
            image: Some(image.to_string()),
        });
        match started {
            Ok(()) => {
                self.db
                    .set_state(&w.nest_id, "running", "running", None, None)
                    .map_err(internal)?;
                self.db.event(
                    Some(&w.nest_id),
                    "mp_worker_started",
                    &format!("release {}, generation {generation}", w.release_id),
                );
                Ok(())
            }
            Err(e) => {
                let _ = self.db.revoke_broker_token(&w.nest_id);
                let _ = self.db.set_state(
                    &w.nest_id,
                    "failed",
                    "stopped",
                    Some("start_failed"),
                    Some(&e),
                );
                Err(internal(format!("start: {e}")))
            }
        }
    }

    /// Hand one job to its buyer context's worker. At-least-once: the same job id twice is one
    /// job, and its current state is returned.
    pub fn submit(&self, job: &JobIn) -> Result<Value, DispatchError> {
        if !valid_job_id(&job.job_id)
            || !valid_ref(&job.listing_id)
            || !valid_buyer_context(&job.buyer_context)
            || !crate::release::operation_name_ok(&job.operation)
            || !crate::release::operation_version_ok(&job.operation_version)
            || !job.payload.is_object()
        {
            return Err(DispatchError::new("invalid", "malformed job"));
        }
        verify_admission(&job.release)?;
        let _one = self.lock.lock().map_err(|_| internal("dispatch lock"))?;
        let a = &job.release;
        self.release_state_ok(&a.release_id)?;
        if let Some(existing) = self.db.mp_job(&job.job_id).map_err(internal)? {
            return Ok(json!({ "job_id": job.job_id, "state": existing.1, "worker": existing.0 }));
        }
        self.db
            .mp_admit(
                &a.release_id,
                &a.image,
                &a.release_digest,
                &a.statement_digest,
            )
            .map_err(internal)?;
        match self.db.mp_release(&a.release_id).map_err(internal)? {
            Some((image, digest, _)) if image == a.image && digest == a.release_digest => {}
            _ => {
                return Err(DispatchError::new(
                    "release_mismatch",
                    "this release id is known here with another image",
                ))
            }
        }

        let id = worker_id(&job.listing_id, &job.buyer_context);
        let worker = match self
            .db
            .mp_worker_for(&job.listing_id, &job.buyer_context)
            .map_err(internal)?
        {
            Some(w) => w,
            None => {
                let w = MpWorker {
                    nest_id: id,
                    listing_id: job.listing_id.clone(),
                    buyer_context: job.buyer_context.clone(),
                    release_id: a.release_id.clone(),
                };
                self.db
                    .mp_insert_worker(&w, WORKER_INPUT_TOKENS, WORKER_OUTPUT_TOKENS)
                    .map_err(internal)?;
                w
            }
        };
        let running = matches!(
            self.pool.state(&worker.nest_id).map_err(internal)?,
            RuntimeState::Running
        ) && self
            .db
            .nest(&worker.nest_id)
            .map_err(internal)?
            .is_some_and(|n| n.state == "running");
        let moved = worker.release_id != a.release_id;
        if moved {
            // An update: the worker moves to the new release on a new generation.
            self.db
                .mp_set_worker_release(&worker.nest_id, &a.release_id)
                .map_err(internal)?;
        }
        let worker = MpWorker {
            release_id: a.release_id.clone(),
            ..worker
        };
        if moved || !running {
            self.start_worker(&worker, &a.image)?;
        }
        let body = json!({
            "job_id": job.job_id,
            "operation": job.operation,
            "operation_version": job.operation_version,
            "payload": job.payload,
        });
        let inbox = self
            .volume(&worker)
            .join("jobs/in")
            .join(format!("{}.json", job.job_id));
        write_for_sandbox(&inbox, body.to_string().as_bytes(), self.uid).map_err(internal)?;
        self.db
            .mp_insert_job(&job.job_id, &worker.nest_id)
            .map_err(internal)?;
        self.db.touch_active(&worker.nest_id);
        Ok(json!({ "job_id": job.job_id, "state": "running", "worker": worker.nest_id }))
    }

    /// A job's state, collecting its result from the worker's outbox once it is there.
    pub fn poll(&self, job_id: &str) -> Result<Value, DispatchError> {
        if !valid_job_id(job_id) {
            return Err(DispatchError::new("invalid", "malformed job id"));
        }
        let Some((nest_id, state, result)) = self.db.mp_job(job_id).map_err(internal)? else {
            return Err(DispatchError::new("not_found", "no such job"));
        };
        if state != "running" {
            return Ok(json!({
                "job_id": job_id,
                "state": state,
                "result": result.and_then(|r| serde_json::from_str::<Value>(&r).ok()),
            }));
        }
        let Some(worker) = self.db.mp_worker(&nest_id).map_err(internal)? else {
            return Err(internal("a job without its worker"));
        };
        let out = self
            .volume(&worker)
            .join("jobs/out")
            .join(format!("{job_id}.json"));
        let Ok(meta) = std::fs::symlink_metadata(&out) else {
            return Ok(json!({ "job_id": job_id, "state": "running" }));
        };
        // The outbox is the worker's to write: read only a bounded regular file, never a link.
        let result: Value = if meta.is_file() && meta.len() <= MAX_RESULT_BYTES {
            std::fs::read_to_string(&out)
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        let _ = std::fs::remove_file(&out);
        let ok = result["status"] == "succeeded" && result["job_id"] == job_id;
        let kept = if ok {
            json!({
                "answer": result["answer"],
                "sources": result["sources"],
                "model_used": result["model_used"],
            })
        } else {
            json!({ "error_code": result["error_code"].as_str().unwrap_or("worker_failed") })
        };
        let state = if ok { "succeeded" } else { "failed" };
        self.db
            .mp_finish_job(job_id, state, Some(&kept.to_string()))
            .map_err(internal)?;
        Ok(json!({ "job_id": job_id, "state": state, "result": kept }))
    }

    /// Lift a quarantine: the release takes jobs again, on fresh workers. A revoked release stays
    /// revoked; one this host never saw has nothing to lift.
    pub fn lift(&self, release_id: &str) -> Result<Value, DispatchError> {
        if !valid_ref(release_id) {
            return Err(DispatchError::new("invalid", "malformed release id"));
        }
        let _one = self.lock.lock().map_err(|_| internal("dispatch lock"))?;
        match self.db.mp_release(release_id).map_err(internal)? {
            Some((_, _, state)) if state == "revoked" => Err(DispatchError::new(
                "release_revoked",
                "a revoked release stays revoked",
            )),
            Some((image, _, state)) if state == "quarantined" => {
                // A quarantine recorded before this host ever ran the release has no image: lifting
                // forgets it, so the next attested job admits it normally.
                if image.is_empty() {
                    self.db.mp_forget_unadmitted(release_id).map_err(internal)?;
                    return Ok(json!({ "release_id": release_id, "state": "unknown" }));
                }
                self.db
                    .mp_set_release_state(release_id, "admitted")
                    .map_err(internal)?;
                Ok(json!({ "release_id": release_id, "state": "admitted" }))
            }
            Some((_, _, state)) => Ok(json!({ "release_id": release_id, "state": state })),
            None => Ok(json!({ "release_id": release_id, "state": "unknown" })),
        }
    }

    /// Stop every worker of a release, fence its tokens, stop its running jobs, refuse new ones.
    pub fn quarantine(&self, release_id: &str, revoke: bool) -> Result<Value, DispatchError> {
        if !valid_ref(release_id) {
            return Err(DispatchError::new("invalid", "malformed release id"));
        }
        let _one = self.lock.lock().map_err(|_| internal("dispatch lock"))?;
        let current = self.db.mp_release(release_id).map_err(internal)?;
        let state = match (revoke, current.as_ref().map(|c| c.2.as_str())) {
            (true, _) | (false, Some("revoked")) => "revoked",
            (false, _) => "quarantined",
        };
        self.db
            .mp_set_release_state(release_id, state)
            .map_err(internal)?;
        let mut stopped = Vec::new();
        for w in self
            .db
            .mp_workers_of_release(release_id)
            .map_err(internal)?
        {
            // Fence first: from this instant the broker refuses the worker's token.
            self.db.revoke_broker_token(&w.nest_id).map_err(internal)?;
            self.pool.stop(&w.nest_id).map_err(internal)?;
            let _ = std::fs::remove_file(self.volume(&w).join("run/broker-token"));
            self.db
                .set_state(&w.nest_id, "stopped", "stopped", Some(state), None)
                .map_err(internal)?;
            self.db.mp_stop_jobs_of(&w.nest_id).map_err(internal)?;
            self.db.event(Some(&w.nest_id), "mp_worker_stopped", state);
            stopped.push(w.nest_id);
        }
        Ok(json!({ "release_id": release_id, "state": state, "stopped": stopped }))
    }
}

// ---- HTTP, behind the control token ---------------------------------------------------------------

#[derive(Clone)]
pub struct Http {
    pub dispatcher: Arc<Dispatcher>,
    /// SHA-256 (hex) of the control API's one bearer token.
    pub token_sha256: Arc<String>,
}

type Reply = (StatusCode, Json<Value>);

fn authorized(h: &Http, headers: &HeaderMap) -> bool {
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    !presented.is_empty() && ct_eq(&sha256_hex(presented.as_bytes()), &h.token_sha256)
}

fn answer(r: Result<Value, DispatchError>) -> Reply {
    match r {
        Ok(v) => (StatusCode::OK, Json(v)),
        Err(e) => (
            e.status(),
            Json(json!({ "error": e.code, "detail": e.detail })),
        ),
    }
}

async fn on_blocking<F>(h: &Http, headers: &HeaderMap, f: F) -> Reply
where
    F: FnOnce(&Dispatcher) -> Result<Value, DispatchError> + Send + 'static,
{
    if !authorized(h, headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized", "detail": "service token required" })),
        );
    }
    let d = h.dispatcher.clone();
    match tokio::task::spawn_blocking(move || f(&d)).await {
        Ok(r) => answer(r),
        Err(e) => answer(Err(internal(e.to_string()))),
    }
}

async fn http_submit(State(h): State<Http>, headers: HeaderMap, Json(job): Json<JobIn>) -> Reply {
    on_blocking(&h, &headers, move |d| d.submit(&job)).await
}

async fn http_poll(
    State(h): State<Http>,
    headers: HeaderMap,
    UrlPath(job): UrlPath<String>,
) -> Reply {
    on_blocking(&h, &headers, move |d| d.poll(&job)).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuarantineReq {
    #[serde(default)]
    revoke: bool,
    /// Lift a quarantine instead (never a revocation).
    #[serde(default)]
    lift: bool,
}

async fn http_quarantine(
    State(h): State<Http>,
    headers: HeaderMap,
    UrlPath(release): UrlPath<String>,
    Json(req): Json<QuarantineReq>,
) -> Reply {
    if req.lift && req.revoke {
        return answer(Err(DispatchError::new(
            "invalid",
            "lift or revoke, not both",
        )));
    }
    on_blocking(&h, &headers, move |d| {
        if req.lift {
            d.lift(&release)
        } else {
            d.quarantine(&release, req.revoke)
        }
    })
    .await
}

/// `POST /v1/mp/jobs`, `GET /v1/mp/jobs/{job}`, `POST /v1/mp/releases/{release}/quarantine`.
pub fn router(h: Http) -> Router {
    Router::new()
        .route("/v1/mp/jobs", post(http_submit))
        .route("/v1/mp/jobs/{job}", get(http_poll))
        .route(
            "/v1/mp/releases/{release}/quarantine",
            post(http_quarantine),
        )
        .layer(axum::extract::DefaultBodyLimit::max(300 * 1024))
        .with_state(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn serve() -> (String, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let d = Dispatcher::new(
            Arc::new(crate::sandbox::Fake::default()),
            Db::open_memory().unwrap(),
            dir.path().to_path_buf(),
            10001,
        );
        let app = router(Http {
            dispatcher: Arc::new(d),
            token_sha256: Arc::new(sha256_hex(b"nst_secret_token")),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, dir)
    }

    /// The bodies the postbox sends (`marketplace_m1::dispatch_once`, `staff_security`) are the
    /// ones this router reads, and nothing answers without the control token.
    #[tokio::test]
    async fn the_router_takes_the_postbox_shapes_behind_the_token() {
        let (url, _dir) = serve().await;
        let http = reqwest::Client::new();
        let post = |path: &str, token: Option<&str>, body: Value| {
            let mut r = http.post(format!("{url}{path}")).json(&body);
            if let Some(t) = token {
                r = r.bearer_auth(t);
            }
            r.send()
        };
        let lift = json!({ "revoke": false, "lift": true });
        let r = post("/v1/mp/releases/rel_x/quarantine", None, lift.clone())
            .await
            .unwrap();
        assert_eq!(r.status(), 401);
        let r = post(
            "/v1/mp/releases/rel_x/quarantine",
            Some("nst_secret_token"),
            lift,
        )
        .await
        .unwrap();
        assert_eq!(r.status(), 200);
        let r = post(
            "/v1/mp/releases/rel_x/quarantine",
            Some("nst_secret_token"),
            json!({ "revoke": true, "lift": false }),
        )
        .await
        .unwrap();
        assert_eq!(r.status(), 200);
        // The job shape deserializes; an unattested release is refused for what it is.
        let job = json!({
            "job_id": "job_aaaaaaaaaaaaaaaaaaaaaaaaaa",
            "listing_id": "lst_1",
            "buyer_context": "ctx-a",
            "operation": "hr.policy.ask",
            "operation_version": "1.0",
            "payload": { "question": "?" },
            "release": {
                "release_id": "rel_y",
                "image": format!("x@sha256:{}", "a".repeat(64)),
                "release_digest": format!("sha256:{}", "b".repeat(64)),
                "statement": "{}",
                "statement_digest": "sha256:00",
            },
        });
        let r = post("/v1/mp/jobs", Some("nst_secret_token"), job.clone())
            .await
            .unwrap();
        assert_eq!(r.status(), 400);
        let v: Value = r.json().await.unwrap();
        assert_eq!(v["error"], "not_attested");
        let r = post("/v1/mp/jobs", Some("wrong"), job).await.unwrap();
        assert_eq!(r.status(), 401);
    }
}
