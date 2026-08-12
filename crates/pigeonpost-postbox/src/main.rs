//! # pigeonpost-postbox (scaffold)
//!
//! The hosted plane for mass adoption: a Dockerized box on `159.69.201.24`
//! (`postbox.pigeonpost.dev` / `mcp.pigeonpost.dev`) that will serve a remote MCP connector, a
//! zero-terminal web inbox, key custody, and hosted lofts.
//!
//! Built so far (P0, incremental):
//! - process skeleton: config from env, structured logging, graceful shutdown, container
//!   healthcheck, reaper entrypoint;
//! - proof-of-work anti-abuse (`GET /v1/pow/challenge`) — `pow`;
//! - anonymous `/k/` identity creation (`POST /v1/identities`), PoW-gated: mints a keypair, seals
//!   the seed in the `vault`, persists it to SQLite (`store`), returns the address + a one-time
//!   capability token;
//! - the messaging loop, capability-token authed: seal + deliver (`POST /v1/send`, hosted→hosted),
//!   open (`GET /v1/inbox`, managed custody unwraps in-session), and `POST /v1/ack`;
//! - the MCP connector (`POST /mcp`, JSON-RPC) — `mcp` — exposing whoami / send / check-inbox / ack
//!   as tools, so a Claude/ChatGPT client can drive a hosted mailbox.
//!
//! Not yet here: cross-box delivery (resolving external recipient keys), accounts/OAuth, quotas, and
//! Postgres. See the design doc below.
//!
//! Design: `docs/planning/hosted-postbox-architecture-2026-08-12.md`.
//!
//! Entry points:
//! - `pigeonpost-postbox`               — run the HTTP server (default).
//! - `pigeonpost-postbox --reaper`      — run the ephemeral-retention sweep loop.
//! - `pigeonpost-postbox --healthcheck` — TCP-probe the bind port; exit 0/1 (Docker HEALTHCHECK).

use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get, post},
    Json, Router,
};
use pigeonpost_core::{envelope, keys, Address, Identity};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use zeroize::Zeroize;

mod mcp;
mod pow;
mod store;
mod vault;

/// How long a proof-of-work challenge stays valid (also bounds the spent-challenge set).
const POW_TTL_SECS: u64 = 120;

/// Shared, cheaply-cloneable handler state.
#[derive(Clone)]
struct AppState {
    pow: Arc<pow::Pow>,
    vault: Arc<vault::Vault>,
    store: Arc<store::Store>,
}

/// Runtime configuration, entirely from environment (see the plan's Appendix A). Secrets
/// (`POW_HMAC_SECRET`, `CAPTCHA_SECRET`, DB password inside `POSTBOX_DB_URL`) are deliberately never
/// logged.
// Fields the scaffold parses but doesn't consume yet (db_url, quotas) get wired in during the P0
// build; allow dead_code until then rather than dropping config the operator has already set.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct Config {
    bind: String,
    public_url: String,
    db_path: String,
    loft_dir: String,
    registry_url: String,
    pow_min_bits: u32,
    pow_max_bits: u32,
    ephemeral_retention_days: u64,
    reaper_interval_secs: u64,
}

impl Config {
    fn from_env() -> Self {
        Config {
            bind: env_or("POSTBOX_BIND", "0.0.0.0:8990"),
            public_url: env_or("POSTBOX_PUBLIC_URL", "http://localhost:8990"),
            db_path: env_or("POSTBOX_DB_PATH", "/data/postbox.db"),
            loft_dir: env_or("POSTBOX_LOFT_DIR", "/data/loft"),
            registry_url: env_or("REGISTRY_URL", "https://registry.pigeonpost.dev"),
            pow_min_bits: env_num("POW_MIN_BITS", 18),
            pow_max_bits: env_num("POW_MAX_BITS", 26),
            ephemeral_retention_days: env_num("EPHEMERAL_RETENTION_DAYS", 30),
            reaper_interval_secs: env_num("REAPER_INTERVAL_SECS", 3600),
        }
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_num<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Port from a `host:port` bind string, defaulting to 8990 if it can't be parsed.
fn port_from_bind(bind: &str) -> u16 {
    bind.rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8990)
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Container HEALTHCHECK: probe the port and exit, before we spin up anything else.
    if args.iter().any(|a| a == "--healthcheck") {
        std::process::exit(run_healthcheck());
    }

    init_tracing();
    let cfg = Config::from_env();

    if args.iter().any(|a| a == "--reaper") {
        reaper(cfg).await;
        return;
    }

    serve(cfg).await;
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_env("RUST_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).with_target(false).init();
}

/// TCP-probe `127.0.0.1:<port>`; 0 = reachable, 1 = not. Dependency-free so the healthcheck adds no
/// HTTP client to the image.
fn run_healthcheck() -> i32 {
    let bind = std::env::var("POSTBOX_BIND").unwrap_or_else(|_| "0.0.0.0:8990".to_string());
    let port = port_from_bind(&bind);
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    match std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(2)) {
        Ok(_) => 0,
        Err(_) => 1,
    }
}

async fn serve(cfg: Config) {
    let listener = match tokio::net::TcpListener::bind(&cfg.bind).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, bind = %cfg.bind, "failed to bind");
            std::process::exit(1);
        }
    };

    let state = match build_state(&cfg) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, db_path = %cfg.db_path, "failed to open the identity store");
            std::process::exit(1);
        }
    };

    tracing::info!(
        bind = %cfg.bind,
        public_url = %cfg.public_url,
        registry = %cfg.registry_url,
        db_path = %cfg.db_path,
        pow_bits = format!("{}..{}", cfg.pow_min_bits, cfg.pow_max_bits),
        "pigeonpost-postbox listening (/v1 REST + /mcp connector live)"
    );

    if let Err(e) = axum::serve(listener, build_router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
    {
        tracing::error!(error = %e, "server error");
        std::process::exit(1);
    }
}

/// Assemble shared state. The PoW HMAC secret comes from `POW_HMAC_SECRET`; if it's unset (or the
/// placeholder), we fall back to an ephemeral random secret and warn — fine for dev, but challenges
/// won't survive a restart and won't validate across replicas.
fn build_state(cfg: &Config) -> Result<AppState, store::StoreError> {
    let secret = match std::env::var("POW_HMAC_SECRET") {
        Ok(s) if !s.is_empty() && s != "CHANGEME" => s.into_bytes(),
        _ => {
            let mut b = [0u8; 32];
            rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut b);
            tracing::warn!(
                "POW_HMAC_SECRET unset — using an ephemeral secret (challenges won't survive a restart)"
            );
            b.to_vec()
        }
    };
    Ok(AppState {
        pow: Arc::new(pow::Pow::new(
            secret,
            cfg.pow_min_bits,
            cfg.pow_max_bits,
            POW_TTL_SECS,
        )),
        vault: Arc::new(vault::Vault::new(load_master_key())),
        store: Arc::new(store::Store::open(&cfg.db_path)?),
    })
}

/// The vault master key. P0 derives it as `SHA-256` of the sealed file named by
/// `POSTBOX_KMS=sealed-file:/path` (the file *is* the secret; disk perms protect it). Production
/// replaces this with an age envelope or a KMS/HSM. Missing/unreadable → ephemeral key + warn: fine
/// for dev, but managed keys won't survive a restart or validate across replicas.
fn load_master_key() -> [u8; 32] {
    if let Some(path) = std::env::var("POSTBOX_KMS")
        .ok()
        .and_then(|s| s.strip_prefix("sealed-file:").map(str::to_string))
    {
        match std::fs::read(&path) {
            Ok(bytes) if !bytes.is_empty() => return sha256(&bytes),
            _ => tracing::warn!(
                path,
                "POSTBOX_KMS sealed-file unreadable/empty — using an ephemeral vault key"
            ),
        }
    }
    let mut key = [0u8; 32];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut key);
    tracing::warn!("no POSTBOX_KMS master key — using an ephemeral vault key (managed keys won't survive a restart)");
    key
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

fn hex_str(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// A one-time capability token bound to an ephemeral identity (plan §11).
fn gen_cap_token() -> String {
    let mut raw = [0u8; 32];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut raw);
    format!("cap_{}", hex_str(&raw))
}

fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/mcp", post(mcp_handler))
        .route("/v1/pow/challenge", get(pow_challenge))
        .route("/v1/identities", post(create_identity))
        .route("/v1/send", post(send))
        .route("/v1/inbox", get(inbox))
        .route("/v1/ack", post(ack))
        .route("/v1/{*rest}", any(v1_stub))
        .fallback(not_found)
        .with_state(state)
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

async fn health() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "pigeonpost-postbox",
        "version": env!("CARGO_PKG_VERSION"),
        "stage": "scaffold",
    }))
}

/// Recent anonymous creations before PoW difficulty climbs one bit (§14.1). Fixed input of `0` for
/// now — the real per-window rate signal lands with the accounts/metering work.
const POW_RATE_THRESHOLD: u64 = 100;

/// Issue a proof-of-work challenge for anonymous identity creation (§14.1). The client solves it and
/// submits the solution to `create_identity`.
async fn pow_challenge(State(state): State<AppState>) -> impl IntoResponse {
    let now = now_unix();
    let bits = state.pow.difficulty_for_rate(0, POW_RATE_THRESHOLD);
    let challenge = state.pow.issue(bits, now);
    let expires_at = pow::Pow::exp_of(&challenge).unwrap_or(now + POW_TTL_SECS);
    Json(json!({ "challenge": challenge, "bits": bits, "expires_at": expires_at }))
}

/// Anonymous identity creation request. `pow_*` are required; `label` is an optional agent tag.
#[derive(serde::Deserialize)]
#[allow(dead_code)] // label is consumed once real /k/ minting lands
struct CreateIdentityReq {
    #[serde(default)]
    pow_challenge: String,
    #[serde(default)]
    pow_solution: String,
    #[serde(default)]
    label: Option<String>,
}

/// `POST /v1/identities` — anonymous `/k/` creation, gated on proof-of-work. Mints a keypair, seals
/// its seed in the vault, stores the identity, and returns the `/k/` address plus a one-time
/// capability token (the only credential for this ephemeral identity).
///
/// P0 storage is in-memory (see `store`), so identities don't yet survive a restart; that's the
/// next increment. Custody here is *managed* — the seed is sealed at rest and only ever unwrapped
/// into short-lived memory.
async fn create_identity(
    State(state): State<AppState>,
    Json(req): Json<CreateIdentityReq>,
) -> Response {
    let now = now_unix();
    if let Err(e) = state
        .pow
        .consume(&req.pow_challenge, &req.pow_solution, now)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "pow_required",
                "detail": e.to_string(),
                "hint": "GET /v1/pow/challenge, solve it, and resubmit as pow_challenge + pow_solution",
            })),
        )
            .into_response();
    }

    let identity = Identity::generate();
    let address = identity.address().as_str().to_string();
    let ed25519_pub = identity.verifying_key().to_bytes();
    let x25519_pub = keys::x25519_public(&identity);

    let mut seed = identity.to_seed();
    let wrapped_seed = match state.vault.wrap(&seed) {
        Ok(w) => w,
        Err(e) => {
            seed.zeroize();
            tracing::error!(error = %e, "vault seal failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "vault_error" })),
            )
                .into_response();
        }
    };
    seed.zeroize();

    let cap_token = gen_cap_token();
    let cap_hash = sha256(cap_token.as_bytes());

    if let Err(e) = state
        .store
        .insert(store::StoredIdentity {
            address: address.clone(),
            wrapped_seed,
            ed25519_pub,
            x25519_pub,
            cap_hash,
            label: req.label.clone(),
            created_at: now,
        })
        .await
    {
        tracing::error!(error = %e, "store insert failed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "store_error" })),
        )
            .into_response();
    }
    let total = state.store.count().await.unwrap_or(0);
    tracing::info!(address = %address, total, "minted /k/ identity");

    (
        StatusCode::CREATED,
        Json(json!({ "address": address, "capability_token": cap_token })),
    )
        .into_response()
}

fn err_response(status: StatusCode, error: &str, detail: Option<&str>) -> Response {
    let mut body = json!({ "error": error });
    if let Some(d) = detail {
        body["detail"] = json!(d);
    }
    (status, Json(body)).into_response()
}

/// A failed operation. Carries the HTTP status the REST layer uses; the MCP layer ignores the status
/// and surfaces `message` as an error tool-result. Small, so it's fine as a `Result` error type.
pub(crate) struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        ApiError {
            status,
            code,
            message: message.into(),
        }
    }
    fn unauthorized(msg: &str) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", msg)
    }
    fn bad(code: &'static str, msg: &str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, msg)
    }
    fn server(code: &'static str) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, code, code)
    }
    fn into_response(self) -> Response {
        err_response(self.status, self.code, Some(&self.message))
    }
}

/// The bearer token from an `Authorization: Bearer …` header, if present and non-empty.
pub(crate) fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .filter(|s| !s.is_empty())
}

/// Resolve the identity a capability token authenticates. Shared by REST (header) and MCP.
pub(crate) async fn identity_for_token(
    state: &AppState,
    token: Option<&str>,
) -> Result<store::StoredIdentity, ApiError> {
    let token = token.ok_or_else(|| ApiError::unauthorized("missing bearer capability token"))?;
    match state.store.get_by_cap(sha256(token.as_bytes())).await {
        Ok(Some(id)) => Ok(id),
        Ok(None) => Err(ApiError::unauthorized(
            "unknown or revoked capability token",
        )),
        Err(e) => {
            tracing::error!(error = %e, "auth lookup failed");
            Err(ApiError::server("store_error"))
        }
    }
}

/// Load an identity's signing key from the vault into memory (zeroizing the seed once built).
fn open_identity(state: &AppState, id: &store::StoredIdentity) -> Result<Identity, ApiError> {
    match state.vault.unwrap(&id.wrapped_seed) {
        Ok(mut seed) => {
            let identity = Identity::from_seed(seed);
            seed.zeroize();
            Ok(identity)
        }
        Err(e) => {
            tracing::error!(error = %e, "vault open failed");
            Err(ApiError::server("vault_error"))
        }
    }
}

// ---- core operations, shared by the REST handlers and the MCP tools ----

/// Seal a message from `sender` to a hosted recipient and enqueue it.
pub(crate) async fn do_send(
    state: &AppState,
    sender: &store::StoredIdentity,
    to: &str,
    body: &str,
) -> Result<serde_json::Value, ApiError> {
    let recipient = state
        .store
        .get(to.to_string())
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "recipient lookup failed");
            ApiError::server("store_error")
        })?
        .ok_or_else(|| {
            ApiError::bad(
                "recipient_unresolved",
                "only hosted recipients are deliverable in this build",
            )
        })?;

    let sender_identity = open_identity(state, sender)?;
    let recipient_vk = keys::verifying_key_from_bytes(&recipient.ed25519_pub)
        .map_err(|_| ApiError::server("bad_recipient_key"))?;

    let now = now_unix();
    let wrap = envelope::wrap(&sender_identity, &recipient_vk, body, now).map_err(|e| {
        tracing::error!(error = %e, "seal failed");
        ApiError::server("seal_error")
    })?;
    let message_id = hex_str(&wrap.id());
    let blob = serde_json::to_vec(&wrap).map_err(|_| ApiError::server("encode_error"))?;

    state
        .store
        .enqueue(store::Message {
            id: message_id.clone(),
            recipient: recipient.address.clone(),
            sender: sender.address.clone(),
            wrap_blob: blob,
            created_at: now,
            read: false,
        })
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "enqueue failed");
            ApiError::server("store_error")
        })?;
    tracing::info!(from = %sender.address, to = %recipient.address, message_id = %message_id, "message sealed + enqueued");
    Ok(json!({ "message_id": message_id }))
}

/// Open every message waiting for `me` and return the plaintext (managed custody).
pub(crate) async fn do_inbox(
    state: &AppState,
    me: &store::StoredIdentity,
) -> Result<serde_json::Value, ApiError> {
    let messages = state
        .store
        .list_for(me.address.clone())
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "inbox list failed");
            ApiError::server("store_error")
        })?;
    let me_identity = open_identity(state, me)?;

    let mut out = Vec::new();
    for m in messages {
        let wrap: envelope::Wrap = match serde_json::from_slice(&m.wrap_blob) {
            Ok(w) => w,
            Err(_) => continue,
        };
        if let Ok((from_vk, body)) = envelope::open(&me_identity, &wrap) {
            out.push(json!({
                "message_id": m.id,
                "from": Address::from_pubkey(&from_vk).as_str(),
                "body": body.as_str(),
                "untrusted": true,
                "received_at": m.created_at,
                "read": m.read,
            }));
        }
    }
    Ok(json!({ "messages": out }))
}

/// Mark one of `me`'s messages read (scoped to the recipient).
pub(crate) async fn do_ack(
    state: &AppState,
    me: &store::StoredIdentity,
    message_id: String,
) -> Result<serde_json::Value, ApiError> {
    match state.store.mark_read(message_id, me.address.clone()).await {
        Ok(true) => Ok(json!({ "ok": true })),
        Ok(false) => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such message in your inbox",
        )),
        Err(e) => {
            tracing::error!(error = %e, "ack failed");
            Err(ApiError::server("store_error"))
        }
    }
}

#[derive(serde::Deserialize)]
struct SendReq {
    to: String,
    body: String,
}

/// `POST /v1/send` — seal a message to a hosted recipient and enqueue it.
async fn send(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SendReq>,
) -> Response {
    let me = match identity_for_token(&state, bearer(&headers)).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    match do_send(&state, &me, &req.to, &req.body).await {
        Ok(v) => (StatusCode::CREATED, Json(v)).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `GET /v1/inbox` — return opened plaintext messages for the authenticated identity.
async fn inbox(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let me = match identity_for_token(&state, bearer(&headers)).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    match do_inbox(&state, &me).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

#[derive(serde::Deserialize)]
struct AckReq {
    message_id: String,
}

/// `POST /v1/ack` — mark one of the authenticated identity's messages read.
async fn ack(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<AckReq>,
) -> Response {
    let me = match identity_for_token(&state, bearer(&headers)).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    match do_ack(&state, &me, req.message_id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `POST /mcp` — MCP over Streamable HTTP JSON-RPC (see the `mcp` module). A JSON-RPC notification
/// (no `id`) gets a bodyless 202; a request gets its JSON-RPC response.
async fn mcp_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<serde_json::Value>>,
) -> Response {
    let token = bearer(&headers).map(str::to_string);
    let request = match body {
        Some(Json(v)) => v,
        None => {
            return err_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                Some("expected a JSON-RPC request body"),
            )
        }
    };
    match mcp::handle(&state, token, request).await {
        Some(response) => Json(response).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

async fn v1_stub() -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "error": "not_implemented",
            "detail": "REST API scaffolded; P0 build pending.",
        })),
    )
}

async fn not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" })))
}

/// The ephemeral-retention sweep. Stub: ticks on an interval and does nothing yet.
/// Retention sweep loop. Opens the same store as the server (WAL makes that safe) and, each tick,
/// drops ephemeral identities and messages older than `EPHEMERAL_RETENTION_DAYS`. In P0 every
/// identity is ephemeral; when durable (paid) identities land they'll be excluded by a plan flag.
async fn reaper(cfg: Config) {
    let store = match store::Store::open(&cfg.db_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, db_path = %cfg.db_path, "reaper failed to open the store");
            std::process::exit(1);
        }
    };
    let retention_secs = cfg.ephemeral_retention_days.saturating_mul(86_400);
    tracing::info!(
        interval_s = cfg.reaper_interval_secs,
        retention_days = cfg.ephemeral_retention_days,
        "reaper started"
    );
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(
        cfg.reaper_interval_secs.max(1),
    ));
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let cutoff = now_unix().saturating_sub(retention_secs);
                match store.reap(cutoff).await {
                    Ok(s) if s.identities > 0 || s.messages > 0 => tracing::info!(
                        identities = s.identities, messages = s.messages, cutoff,
                        "reaped expired ephemeral data"
                    ),
                    Ok(_) => tracing::debug!(cutoff, "reaper: nothing to sweep"),
                    Err(e) => tracing::error!(error = %e, "reap failed"),
                }
            }
            _ = shutdown_signal() => {
                tracing::info!("reaper stopping");
                break;
            }
        }
    }
}

/// Resolve on SIGINT (Ctrl-C) or, on Unix, SIGTERM (what Docker sends on stop).
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!("shutdown signal received");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_port_from_bind() {
        assert_eq!(port_from_bind("0.0.0.0:8990"), 8990);
        assert_eq!(port_from_bind("127.0.0.1:1234"), 1234);
        assert_eq!(port_from_bind("[::]:443"), 443);
        assert_eq!(port_from_bind("garbage"), 8990);
    }

    /// Guards against a matchit route conflict between `/v1/pow/challenge` and the `/v1/{*rest}`
    /// catch-all — Router construction panics on conflict, so building it here is the assertion.
    #[test]
    fn router_builds_without_route_conflict() {
        let state = AppState {
            pow: Arc::new(pow::Pow::new(b"k".to_vec(), 8, 24, 120)),
            vault: Arc::new(vault::Vault::new([0u8; 32])),
            store: Arc::new(store::Store::open(":memory:").unwrap()),
        };
        let _ = build_router(state);
    }
}
