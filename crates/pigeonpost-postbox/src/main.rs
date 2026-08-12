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
//! - the MCP connector (`POST /mcp`, JSON-RPC) — `mcp` — with identity-management and messaging
//!   tools, so a Claude/ChatGPT client can drive one or many hosted mailboxes;
//! - a self-serve onboarding page at `/` (in-browser PoW → mint → connector config);
//! - accounts + API keys (`POST /v1/accounts`): one key creates and drives many identities without
//!   per-inbox PoW (the multi-mailbox model), with `identity`/`from` selection;
//! - quotas: a per-account identity cap and a per-inbox message cap, so one proof-of-work can't be
//!   parlayed into unbounded identities/messages (disk protection);
//! - OAuth-backed accounts (`oidc`): a pigeonpost-prod member JWT (validated against the realm JWKS,
//!   issuer + expiry) is a third auth method — its subject maps to an account, tying inboxes to a
//!   real login.
//!
//! Not yet here: cross-box delivery (resolving external recipient keys) and Postgres. See the design
//! doc below.
//!
//! Design: `docs/planning/hosted-postbox-architecture-2026-08-12.md`.
//!
//! Entry points:
//! - `pigeonpost-postbox`               — run the HTTP server (default).
//! - `pigeonpost-postbox --reaper`      — run the ephemeral-retention sweep loop.
//! - `pigeonpost-postbox --healthcheck` — TCP-probe the bind port; exit 0/1 (Docker HEALTHCHECK).

use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{any, get, post},
    Json, Router,
};
use pigeonpost_core::{envelope, keys, Address, Identity};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use zeroize::Zeroize;

mod mcp;
mod oidc;
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
    oidc: Arc<oidc::Oidc>,
    /// Max identities one account may hold (identity quota).
    max_identities: usize,
    /// Max messages one inbox may hold before senders are refused (inbox quota).
    max_inbox: usize,
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
    max_identities_per_account: usize,
    max_inbox_messages: usize,
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
            max_identities_per_account: env_num("EPHEMERAL_MAX_IDENTITIES", 5),
            max_inbox_messages: env_num("MAX_INBOX_MESSAGES", 1000),
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
        oidc: Arc::new(oidc::Oidc::new(env_or(
            "OIDC_ISSUER",
            "https://auth.pigeonpost.dev/realms/pigeonpost-prod",
        ))),
        max_identities: cfg.max_identities_per_account,
        max_inbox: cfg.max_inbox_messages,
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

/// Hex of `n` cryptographically-random bytes.
fn rand_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut b);
    hex_str(&b)
}

/// A one-time capability token bound to an ephemeral identity (plan §11).
fn gen_cap_token() -> String {
    let mut raw = [0u8; 32];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut raw);
    format!("cap_{}", hex_str(&raw))
}

fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(onboard))
        .route("/health", get(health))
        .route("/mcp", post(mcp_handler))
        .route("/v1/pow/challenge", get(pow_challenge))
        .route("/v1/accounts", post(create_account))
        .route("/v1/identities", post(create_identity).get(list_identities))
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

/// `GET /` — the self-serve onboarding page: solves the PoW in-browser, mints an inbox, and hands
/// back a paste-ready MCP connector config. Static, embedded in the binary.
async fn onboard() -> Html<&'static str> {
    Html(include_str!("onboard.html"))
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

/// Identity creation request. Anonymous callers supply `pow_*`; API-key callers omit them.
#[derive(serde::Deserialize)]
struct CreateIdentityReq {
    #[serde(default)]
    pow_challenge: String,
    #[serde(default)]
    pow_solution: String,
    #[serde(default)]
    label: Option<String>,
}

/// Mint a keypair, seal its seed in the vault, store it (optionally under an account), and return the
/// `/k/` address plus a one-time capability token. Shared by the REST handler and the MCP tool.
async fn do_create_identity(
    state: &AppState,
    account_id: Option<String>,
    label: Option<String>,
) -> Result<serde_json::Value, ApiError> {
    // Identity quota: an account can't mint unlimited inboxes off a single proof-of-work.
    if let Some(acc) = &account_id {
        let held = state
            .store
            .count_for_account(acc.clone())
            .await
            .map_err(|_| ApiError::server("store_error"))?;
        if held >= state.max_identities {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "identity_quota",
                format!(
                    "account is at its limit of {} identities",
                    state.max_identities
                ),
            ));
        }
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
            return Err(ApiError::server("vault_error"));
        }
    };
    seed.zeroize();

    let cap_token = gen_cap_token();
    let cap_hash = sha256(cap_token.as_bytes());

    state
        .store
        .insert(store::StoredIdentity {
            address: address.clone(),
            wrapped_seed,
            ed25519_pub,
            x25519_pub,
            cap_hash,
            label,
            created_at: now_unix(),
            account_id,
        })
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "store insert failed");
            ApiError::server("store_error")
        })?;
    let total = state.store.count().await.unwrap_or(0);
    tracing::info!(address = %address, total, "minted /k/ identity");
    Ok(json!({ "address": address, "capability_token": cap_token }))
}

fn is_api_key(token: &str) -> bool {
    token.starts_with("pk_")
}

/// `POST /v1/identities` — create a `/k/` inbox. An **API key** creates under its account with no
/// PoW; an **anonymous** caller must solve a PoW (ephemeral, no account); a capability token can't
/// create (it authenticates one existing identity).
async fn create_identity(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateIdentityReq>,
) -> Response {
    let created = |r: Result<serde_json::Value, ApiError>| match r {
        Ok(v) => (StatusCode::CREATED, Json(v)).into_response(),
        Err(e) => e.into_response(),
    };
    match bearer(&headers) {
        // Authenticated (API key or member JWT) → create under that account, no PoW.
        Some(tok) => match principal_for_token(&state, Some(tok)).await {
            Ok(Principal::Account(account_id)) => {
                created(do_create_identity(&state, Some(account_id), req.label).await)
            }
            Ok(Principal::Identity(_)) => err_response(
                StatusCode::BAD_REQUEST,
                "use_api_key",
                Some(
                    "capability tokens can't create identities — use an account API key or sign in",
                ),
            ),
            Err(e) => e.into_response(),
        },
        // Anonymous → proof-of-work gated (ephemeral, no account).
        None => {
            if let Err(e) = state
                .pow
                .consume(&req.pow_challenge, &req.pow_solution, now_unix())
            {
                return err_response(
                    StatusCode::BAD_REQUEST,
                    "pow_required",
                    Some(&format!("{e}. GET /v1/pow/challenge, solve it, resubmit as pow_challenge + pow_solution")),
                );
            }
            created(do_create_identity(&state, None, req.label).await)
        }
    }
}

/// `POST /v1/accounts` — create an account (PoW-gated) and return its first API key, shown once. The
/// key then creates and drives many identities without further PoW.
async fn create_account(
    State(state): State<AppState>,
    Json(req): Json<CreateIdentityReq>,
) -> Response {
    if let Err(e) = state
        .pow
        .consume(&req.pow_challenge, &req.pow_solution, now_unix())
    {
        return err_response(
            StatusCode::BAD_REQUEST,
            "pow_required",
            Some(&format!(
                "{e}. GET /v1/pow/challenge, solve it, resubmit as pow_challenge + pow_solution"
            )),
        );
    }
    let account_id = format!("acct_{}", rand_hex(12));
    let api_key = format!("pk_live_{}", rand_hex(32));
    match state
        .store
        .create_account(account_id.clone(), sha256(api_key.as_bytes()), now_unix())
        .await
    {
        Ok(()) => {
            tracing::info!(account = %account_id, "account created");
            (
                StatusCode::CREATED,
                Json(json!({ "account_id": account_id, "api_key": api_key })),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "account create failed");
            ApiError::server("store_error").into_response()
        }
    }
}

/// List an account's identities. Shared by REST and the MCP tool.
pub(crate) async fn do_list_identities(
    state: &AppState,
    account: String,
) -> Result<serde_json::Value, ApiError> {
    let rows = state.store.list_by_account(account).await.map_err(|e| {
        tracing::error!(error = %e, "list identities failed");
        ApiError::server("store_error")
    })?;
    Ok(json!({
        "identities": rows.into_iter().map(|(address, label)| json!({ "address": address, "label": label })).collect::<Vec<_>>()
    }))
}

/// `GET /v1/identities` — list the API-key account's identities.
async fn list_identities(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let account_id = match account_for_headers(&state, &headers).await {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    match do_list_identities(&state, account_id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
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

/// Who a bearer token authenticates: a single identity (capability token) or a whole account
/// (API key), which may own many identities.
pub(crate) enum Principal {
    Identity(store::StoredIdentity),
    Account(String),
}

/// Resolve a bearer token to a principal. `pk_…` = API key → account; otherwise a capability token.
pub(crate) async fn principal_for_token(
    state: &AppState,
    token: Option<&str>,
) -> Result<Principal, ApiError> {
    let token = token.ok_or_else(|| ApiError::unauthorized("missing bearer token"))?;
    if is_api_key(token) {
        match state.store.account_for_key(sha256(token.as_bytes())).await {
            Ok(Some(a)) => Ok(Principal::Account(a)),
            Ok(None) => Err(ApiError::unauthorized("unknown API key")),
            Err(e) => {
                tracing::error!(error = %e, "api-key lookup failed");
                Err(ApiError::server("store_error"))
            }
        }
    } else if token.starts_with("eyJ") {
        // A pigeonpost-prod member JWT → validate → subject → get-or-create that subject's account.
        let claims = state.oidc.validate(token).await.map_err(|e| {
            tracing::debug!(error = %e, "member-token validation failed");
            ApiError::unauthorized("invalid member token")
        })?;
        let account = state
            .store
            .account_for_sub(claims.sub, format!("acct_{}", rand_hex(12)), now_unix())
            .await
            .map_err(|_| ApiError::server("store_error"))?;
        Ok(Principal::Account(account))
    } else {
        identity_for_token(state, Some(token))
            .await
            .map(Principal::Identity)
    }
}

/// Require an API-key account (for account-management endpoints).
async fn account_for_headers(state: &AppState, headers: &HeaderMap) -> Result<String, ApiError> {
    match principal_for_token(state, bearer(headers)).await? {
        Principal::Account(a) => Ok(a),
        Principal::Identity(_) => Err(ApiError::bad(
            "use_api_key",
            "this endpoint needs an account API key, not a capability token",
        )),
    }
}

/// Pick which identity a messaging request acts as. A capability token names exactly one. An account
/// uses `explicit` if given (ownership-checked), else its sole identity — and refuses when ambiguous.
async fn resolve_acting_identity(
    state: &AppState,
    principal: Principal,
    explicit: Option<&str>,
) -> Result<store::StoredIdentity, ApiError> {
    match principal {
        Principal::Identity(id) => {
            if explicit.is_some_and(|a| a != id.address) {
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "wrong_identity",
                    "this capability token is bound to a different identity",
                ));
            }
            Ok(id)
        }
        Principal::Account(account) => {
            if let Some(addr) = explicit {
                return state
                    .store
                    .get_in_account(account, addr.to_string())
                    .await
                    .map_err(|_| ApiError::server("store_error"))?
                    .ok_or_else(|| {
                        ApiError::new(
                            StatusCode::FORBIDDEN,
                            "not_in_account",
                            "that identity is not in your account",
                        )
                    });
            }
            let mut owned = state
                .store
                .list_by_account(account.clone())
                .await
                .map_err(|_| ApiError::server("store_error"))?;
            match owned.len() {
                1 => state
                    .store
                    .get_in_account(account, owned.remove(0).0)
                    .await
                    .map_err(|_| ApiError::server("store_error"))?
                    .ok_or_else(|| ApiError::server("store_error")),
                0 => Err(ApiError::bad(
                    "no_identity",
                    "your account has no identities yet — create one first",
                )),
                _ => Err(ApiError::bad(
                    "identity_required",
                    "your account has multiple identities — pass `identity` (or `from`) to choose one",
                )),
            }
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

    // Inbox quota: don't let one recipient's inbox grow without bound (disk protection).
    let held = state
        .store
        .inbox_count(recipient.address.clone())
        .await
        .map_err(|_| ApiError::server("store_error"))?;
    if held >= state.max_inbox {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "recipient_inbox_full",
            format!("recipient inbox is full ({} messages)", state.max_inbox),
        ));
    }

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

/// Resolve which identity a messaging request acts as, from the header token + an optional selector.
async fn acting_identity(
    state: &AppState,
    headers: &HeaderMap,
    explicit: Option<&str>,
) -> Result<store::StoredIdentity, ApiError> {
    let principal = principal_for_token(state, bearer(headers)).await?;
    resolve_acting_identity(state, principal, explicit).await
}

#[derive(serde::Deserialize)]
struct SendReq {
    to: String,
    body: String,
    /// Which of your identities to send as (API-key accounts with more than one).
    #[serde(default)]
    from: Option<String>,
}

/// `POST /v1/send` — seal a message to a hosted recipient and enqueue it.
async fn send(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SendReq>,
) -> Response {
    let me = match acting_identity(&state, &headers, req.from.as_deref()).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    match do_send(&state, &me, &req.to, &req.body).await {
        Ok(v) => (StatusCode::CREATED, Json(v)).into_response(),
        Err(e) => e.into_response(),
    }
}

#[derive(serde::Deserialize)]
struct InboxQuery {
    #[serde(default)]
    identity: Option<String>,
}

/// `GET /v1/inbox` — return opened plaintext messages for the acting identity.
async fn inbox(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<InboxQuery>,
) -> Response {
    let me = match acting_identity(&state, &headers, q.identity.as_deref()).await {
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
    #[serde(default)]
    identity: Option<String>,
}

/// `POST /v1/ack` — mark one of the acting identity's messages read.
async fn ack(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<AckReq>,
) -> Response {
    let me = match acting_identity(&state, &headers, req.identity.as_deref()).await {
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
            oidc: Arc::new(oidc::Oidc::new("https://auth.example/realms/x".into())),
            max_identities: 5,
            max_inbox: 1000,
        };
        let _ = build_router(state);
    }
}
