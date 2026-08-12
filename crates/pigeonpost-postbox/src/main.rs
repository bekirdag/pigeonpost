//! # pigeonpost-postbox (scaffold)
//!
//! The hosted plane for mass adoption: a Dockerized box on `159.69.201.24`
//! (`postbox.pigeonpost.dev` / `mcp.pigeonpost.dev`) that will serve a remote MCP connector, a
//! zero-terminal web inbox, key custody, and hosted lofts.
//!
//! This binary is a **scaffold**. It stands up the process skeleton — config from env, structured
//! logging, graceful shutdown, a container healthcheck, the reaper entrypoint, and the HTTP surface
//! (`/health`, `/mcp`, `/v1/*`) — with the two API surfaces returning `501 Not Implemented`. The
//! real P0 logic (anonymous `/k/` creation with proof-of-work, `send`/`inbox`/`read`, the key vault,
//! accounts, quotas) is intentionally not here yet.
//!
//! Design: `docs/planning/hosted-postbox-architecture-2026-08-12.md`.
//!
//! Entry points:
//! - `pigeonpost-postbox`               — run the HTTP server (default).
//! - `pigeonpost-postbox --reaper`      — run the ephemeral-retention sweep loop.
//! - `pigeonpost-postbox --healthcheck` — TCP-probe the bind port; exit 0/1 (Docker HEALTHCHECK).

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{any, get, post},
    Json, Router,
};
use serde_json::json;
use std::sync::Arc;

mod pow;

/// How long a proof-of-work challenge stays valid (also bounds the spent-challenge set).
const POW_TTL_SECS: u64 = 120;

/// Shared, cheaply-cloneable handler state.
#[derive(Clone)]
struct AppState {
    pow: Arc<pow::Pow>,
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
    db_url: String,
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
            db_url: env_or("POSTBOX_DB_URL", "sqlite::memory:"),
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

    // Note: db_url is intentionally omitted — it may embed a password.
    tracing::info!(
        bind = %cfg.bind,
        public_url = %cfg.public_url,
        registry = %cfg.registry_url,
        loft_dir = %cfg.loft_dir,
        pow_bits = format!("{}..{}", cfg.pow_min_bits, cfg.pow_max_bits),
        "pigeonpost-postbox listening (scaffold — /mcp and /v1 return 501)"
    );

    if let Err(e) = axum::serve(listener, build_router(build_state(&cfg)))
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
fn build_state(cfg: &Config) -> AppState {
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
    AppState {
        pow: Arc::new(pow::Pow::new(
            secret,
            cfg.pow_min_bits,
            cfg.pow_max_bits,
            POW_TTL_SECS,
        )),
    }
}

fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/mcp", post(mcp_stub))
        .route("/v1/pow/challenge", get(pow_challenge))
        .route("/v1/identities", post(create_identity))
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

/// `POST /v1/identities` — anonymous `/k/` creation, gated on proof-of-work. The PoW gate is live;
/// the actual keypair minting + capability token land with the key-vault increment, so a *passing*
/// proof currently returns 501 (and burns the challenge, proving single-use end to end).
async fn create_identity(
    State(state): State<AppState>,
    Json(req): Json<CreateIdentityReq>,
) -> impl IntoResponse {
    let now = now_unix();
    match state
        .pow
        .consume(&req.pow_challenge, &req.pow_solution, now)
    {
        Ok(v) => {
            tracing::debug!(bits = v.bits, "proof-of-work accepted for create_identity");
            (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({
                    "error": "not_implemented",
                    "detail": "proof-of-work accepted; /k/ identity minting lands with the key-vault increment",
                })),
            )
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "pow_required",
                "detail": e.to_string(),
                "hint": "GET /v1/pow/challenge, solve it, and resubmit as pow_challenge + pow_solution",
            })),
        ),
    }
}

async fn mcp_stub() -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "error": "not_implemented",
            "detail": "MCP endpoint scaffolded; P0 build pending (see hosted-postbox-architecture plan).",
        })),
    )
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
async fn reaper(cfg: Config) {
    tracing::info!(
        interval_s = cfg.reaper_interval_secs,
        retention_days = cfg.ephemeral_retention_days,
        "reaper started (scaffold — no-op sweeps)"
    );
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(
        cfg.reaper_interval_secs.max(1),
    ));
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                tracing::info!("reaper tick (stub — expired ephemeral inbox/key sweep pending)");
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
        };
        let _ = build_router(state);
    }
}
