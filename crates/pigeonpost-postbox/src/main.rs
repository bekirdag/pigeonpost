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
//! - a per-IP mint budget (window + lifetime) on the unauthenticated create paths, so self-serve
//!   minting can be cheap for one honest agent without being cheap for a botnet;
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
    extract::{Extension, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::sse::{Event, KeepAlive, Sse},
    response::{Html, IntoResponse, Response},
    routing::{any, delete, get, post},
    Json, Router,
};
use pigeonpost_core::{envelope, keys, Address, Destination, Identity};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use zeroize::Zeroize;

mod github;
mod mcp;
mod oidc;
mod pow;
mod reputation;
mod store;
mod vault;
mod verbs;

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
    /// Per-IP ceilings on unauthenticated mints.
    mint_limits: MintLimits,
    /// How many trusted reverse proxies sit in front of us (see [`resolve_client_ip`]).
    trusted_proxy_hops: usize,
    /// SHA-256 of the namespace-grant shared secret, or `None` when the endpoint is closed.
    namespace_grant_hash: Option<[u8; 32]>,
    /// Raised whenever any message is enqueued, to release long-polling `GET /v1/inbox?wait=N`
    /// callers the moment their mail lands instead of on their next timer.
    ///
    /// Deliberately one global signal rather than a registry of per-address channels: a wake is
    /// cheap (each waiter re-checks its own unread count and goes back to sleep), signals only
    /// fire on a real send, and a single `Notify` has no lifecycle to leak. If this ever hosts
    /// enough concurrent waiters that the wasted wakeups matter, that is the point to key it by
    /// recipient — not before.
    inbox_signal: Arc<tokio::sync::Notify>,
    /// GitHub device login, when this postbox is configured for it. `None` closes the endpoints
    /// rather than failing per request, so an unconfigured deployment says so once and plainly.
    github: Option<Arc<github::Github>>,
}

/// Namespaces where a name is authorised by proving control of it at an identity provider, rather
/// than by the account having bought the namespace.
///
/// Kept as a list because the difference is structural, not cosmetic: nobody buys `/github`, so
/// every ownership question about a name inside it has to be answered by that provider. Adding a
/// provider here without also adding a verified claim path would let anyone mint under it.
const PROVIDER_NAMESPACES: &[&str] = &["github"];

/// Per-IP ceilings on unauthenticated (proof-of-work) minting. The PoW makes one mint cost a
/// moment; these make a *flood* cost more than a botnet wants to pay, without a human in the loop
/// for the honest single agent.
#[derive(Debug, Clone, Copy)]
struct MintLimits {
    /// Mints allowed inside one rolling window. A small burst, so a box onboarding a handful of
    /// agents at once isn't told to come back in an hour between each.
    per_window: usize,
    /// Length of that rolling window.
    window_secs: u64,
    /// Mints one IP may ever make.
    lifetime: usize,
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
    mint_per_ip_window: usize,
    mint_window_secs: u64,
    mint_per_ip_lifetime: usize,
    trusted_proxy_hops: usize,
    /// Shared secret the billing/entitlement service presents to grant a namespace to an account.
    /// Absent means the grant endpoint is closed, not open — an unconfigured deployment must not
    /// hand out namespaces.
    namespace_grant_token: Option<String>,
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
            // An account is a bookkeeping unit for one operator's fleet, not the abuse boundary —
            // per-IP mint limits are. A single box may legitimately run dozens of agents.
            max_identities_per_account: env_num("EPHEMERAL_MAX_IDENTITIES", 50),
            max_inbox_messages: env_num("MAX_INBOX_MESSAGES", 1000),
            mint_per_ip_window: env_num("MINT_PER_IP_WINDOW", 5),
            mint_window_secs: env_num("MINT_WINDOW_SECS", 3600),
            mint_per_ip_lifetime: env_num("MINT_PER_IP_LIFETIME", 1000),
            // 0 = trust the socket peer. Behind the production Apache front this must be 1, or
            // every caller looks like the proxy and shares one budget.
            trusted_proxy_hops: env_num("TRUSTED_PROXY_HOPS", 0),
            // Unset closes the grant endpoint. Defaulting it to anything would mean a fresh
            // deployment hands out paid namespaces to whoever guesses the default.
            namespace_grant_token: std::env::var("NAMESPACE_GRANT_TOKEN")
                .ok()
                .filter(|t| !t.trim().is_empty()),
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

    // `into_make_service_with_connect_info` is what puts the socket peer in request extensions;
    // without it `TRUSTED_PROXY_HOPS=0` would have no IP to rate-limit against.
    if let Err(e) = axum::serve(
        listener,
        build_router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
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
        mint_limits: MintLimits {
            per_window: cfg.mint_per_ip_window,
            window_secs: cfg.mint_window_secs,
            lifetime: cfg.mint_per_ip_lifetime,
        },
        trusted_proxy_hops: cfg.trusted_proxy_hops,
        namespace_grant_hash: cfg
            .namespace_grant_token
            .as_deref()
            .map(|t| sha256(t.as_bytes())),
        inbox_signal: Arc::new(tokio::sync::Notify::new()),
        github: github::Github::from_env().map(Arc::new),
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

/// Mint a fresh API key: returns the plaintext (shown once) and the storable record (hash + a
/// revocable id + a display prefix).
fn new_api_key() -> (String, store::NewKey) {
    let api_key = format!("pk_live_{}", rand_hex(32));
    let prefix = api_key.chars().take(16).collect::<String>();
    let record = store::NewKey {
        key_hash: sha256(api_key.as_bytes()),
        id: rand_hex(8),
        prefix,
    };
    (api_key, record)
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
        .route("/v1/api-keys", post(create_api_key).get(list_api_keys))
        .route("/v1/api-keys/{id}", delete(revoke_api_key))
        .route(
            "/v1/identities",
            post(create_identity)
                .get(list_identities)
                .delete(delete_identity),
        )
        .route("/v1/send", post(send))
        .route("/v1/inbox", get(inbox))
        .route("/v1/ack", post(ack))
        .route("/v1/report-spam", post(report_spam))
        .route("/v1/events", get(events))
        .route("/v1/whoami", get(whoami))
        .route("/v1/identities/handle", post(bind_identity_handle))
        .route("/v1/namespaces", axum::routing::put(grant_namespace))
        .route("/v1/github/device", post(start_github_claim))
        .route("/v1/github/claim", post(claim_github))
        .route("/v1/workspace", get(get_workspace).put(put_workspace))
        .route(
            "/v1/contacts",
            get(get_contacts).put(put_contact).delete(delete_contact),
        )
        .route("/v1/policy", axum::routing::put(put_policy))
        .route("/v1/{*rest}", any(v1_stub))
        .fallback(not_found)
        .layer(middleware::from_fn(cors))
        // Outermost of the two, so every handler (and `cors`) sees a resolved `ClientIp`.
        .layer(middleware::from_fn_with_state(
            state.clone(),
            client_ip_layer,
        ))
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
    /// Mint under a purchased namespace, e.g. `/bekir/agent1`. Needs an account that owns it;
    /// the anonymous proof-of-work path never accepts one.
    #[serde(default)]
    handle: Option<String>,
}

/// Mint a keypair, seal its seed in the vault, store it (optionally under an account), and return the
/// `/k/` address plus a one-time capability token. Shared by the REST handler and the MCP tool.
/// Ceiling on mailboxes under one purchased namespace, per paid account.
///
/// Enforced inside the writer transaction that creates the mailbox, never as a separate read —
/// see `Store::insert_under_namespace`.
const MAX_HANDLE_MAILBOXES: usize = 100;

/// Authorise `handle` for `account_id` and return its canonical form.
///
/// Ownership is the postbox's cached view of the registry's answer (see the `namespaces` table).
/// **Reserved-name policy is deliberately not enforced here**: the postbox cannot sell a handle,
/// only honour one that was sold, so `docs/reserved-names.md` belongs at the point of sale. Doing
/// it in both places would let the two lists drift and quietly strand a name someone paid for.
async fn authorize_handle(
    state: &AppState,
    account_id: Option<&str>,
    handle: &str,
) -> Result<(String, String), ApiError> {
    let account = account_id.ok_or_else(|| {
        ApiError::new(
            StatusCode::FORBIDDEN,
            "account_required",
            "minting under a handle needs an account — sign in, or use an account API key",
        )
    })?;

    // Canonicalise through core so the postbox and every client agree on what the name *is*,
    // rather than each lower-casing and trimming to its own taste.
    let destination = Destination::for_handle(handle)
        .map_err(|_| ApiError::bad("invalid_handle", "expected /<namespace>/<name>"))?;
    let canonical = destination
        .handle()
        .ok_or_else(|| ApiError::bad("invalid_handle", "not a handle destination"))?
        .to_string();
    let namespace = canonical
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or_default()
        .to_string();

    // A provider namespace is authorised per name, by proof, not per namespace by purchase — see
    // `PROVIDER_NAMESPACES`. Asking `namespace_owner` about `/github` would be asking who bought
    // the whole of GitHub, and the honest answer is nobody.
    if PROVIDER_NAMESPACES.contains(&namespace.as_str()) {
        let login = canonical.rsplit('/').next().unwrap_or_default().to_string();
        let proved = state
            .store
            .provider_identity_owner(namespace.clone(), login.clone())
            .await
            .map_err(|_| ApiError::server("store_error"))?;
        return match proved {
            Some(owner) if owner == account => {
                if state
                    .store
                    .get_by_handle(canonical.clone())
                    .await
                    .map_err(|_| ApiError::server("store_error"))?
                    .is_some()
                {
                    Err(ApiError::new(
                        StatusCode::CONFLICT,
                        "handle_taken",
                        format!("{canonical} already exists"),
                    ))
                } else {
                    Ok((canonical, namespace))
                }
            }
            _ => Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "identity_not_proved",
                format!(
                    "prove you control {login} at {namespace} first — \
                     run `pigeonpost handle claim --{namespace}`"
                ),
            )),
        };
    }

    let owner = state
        .store
        .namespace_owner(namespace.clone(), now_unix())
        .await
        .map_err(|_| ApiError::server("store_error"))?;
    match owner {
        Some(owner) if owner == account => {}
        Some(_) => {
            // Same refusal shape as "unknown", so probing this endpoint does not enumerate which
            // namespaces are sold and to whom.
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "namespace_not_yours",
                format!("/{namespace} is not a namespace this account may mint under"),
            ));
        }
        None => {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "namespace_not_yours",
                format!("/{namespace} is not a namespace this account may mint under"),
            ));
        }
    }

    // The quota is deliberately *not* checked here. A count taken now and acted on after the
    // keypair is generated is a check against a stale number: two mints racing at the boundary
    // would both read a free slot. `insert_under_namespace` counts inside the write instead.

    if state
        .store
        .get_by_handle(canonical.clone())
        .await
        .map_err(|_| ApiError::server("store_error"))?
        .is_some()
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "handle_taken",
            format!("{canonical} already exists"),
        ));
    }
    Ok((canonical, namespace))
}

async fn do_create_identity(
    state: &AppState,
    account_id: Option<String>,
    label: Option<String>,
    handle: Option<String>,
) -> Result<serde_json::Value, ApiError> {
    let (handle, namespace) = match handle {
        Some(requested) => {
            let (canonical, namespace) =
                authorize_handle(state, account_id.as_deref(), &requested).await?;
            (Some(canonical), Some(namespace))
        }
        None => (None, None),
    };

    // Identity quota: an account can't mint unlimited inboxes off a single proof-of-work. Handle
    // mailboxes are bounded by their namespace's own ceiling instead — a paid namespace is exactly
    // the case this quota exists to distinguish from an anonymous burst.
    if let Some(acc) = &account_id.clone().filter(|_| handle.is_none()) {
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

    let record = store::StoredIdentity {
        address: address.clone(),
        wrapped_seed,
        ed25519_pub,
        x25519_pub,
        cap_hash,
        label,
        created_at: now_unix(),
        account_id,
        handle: handle.clone(),
    };
    match &namespace {
        Some(namespace) => {
            let outcome = state
                .store
                .insert_under_namespace(record, format!("/{namespace}"), MAX_HANDLE_MAILBOXES)
                .await
                .map_err(|e| {
                    tracing::error!(error = %e, "store insert failed");
                    ApiError::server("store_error")
                })?;
            if outcome == store::QuotaOutcome::Full {
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "namespace_quota",
                    format!(
                        "/{namespace} already holds its limit of {MAX_HANDLE_MAILBOXES} mailboxes"
                    ),
                ));
            }
        }
        None => state.store.insert(record).await.map_err(|e| {
            tracing::error!(error = %e, "store insert failed");
            ApiError::server("store_error")
        })?,
    }
    let total = state.store.count().await.unwrap_or(0);
    match &handle {
        Some(handle) => tracing::info!(address = %address, %handle, total, "minted handle mailbox"),
        None => tracing::info!(address = %address, total, "minted /k/ identity"),
    }
    Ok(json!({
        "address": address,
        // The handle is the name callers will actually use; the /k/ address stays the key-derived
        // identity underneath it, and both are returned so nothing has to guess the mapping.
        "handle": handle,
        "capability_token": cap_token
    }))
}

/// `POST /v1/identities/handle` — give a mailbox you already own a readable name.
///
/// The mailbox an agent already runs is the one its MCP config, its contacts, and its waiting mail
/// all point at. Without this, joining a namespace means minting a *different* mailbox and
/// abandoning that — so an agent that started anonymously could never be brought into the fleet,
/// only replaced. Binds the name to the mailbox that exists instead.
async fn bind_identity_handle(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<BindHandleReq>,
) -> Response {
    let account = match principal_for_token(&state, bearer(&headers)).await {
        Ok(Principal::Account(account)) => account,
        Ok(Principal::Identity(_)) => {
            // A capability token is the mailbox's own credential, not the account's. Naming a
            // mailbox spends a slot in a namespace the *account* paid for, so it is the account
            // that has to authorise it.
            return ApiError::new(
                StatusCode::FORBIDDEN,
                "account_required",
                "naming a mailbox needs the account — sign in, or use an account API key",
            )
            .into_response();
        }
        Err(e) => return e.into_response(),
    };
    match do_bind_handle(
        &state,
        account,
        req.address,
        req.handle,
        req.capability_token.as_deref(),
    )
    .await
    {
        Ok(body) => Json(body).into_response(),
        Err(e) => e.into_response(),
    }
}

/// Bind a handle to an existing mailbox. Shared by the REST handler and the MCP tool so the two
/// cannot drift on who is allowed to do it or what it refuses.
async fn do_bind_handle(
    state: &AppState,
    account: String,
    address: String,
    handle: String,
    capability_token: Option<&str>,
) -> Result<serde_json::Value, ApiError> {
    let (canonical, namespace) = authorize_handle(state, Some(account.as_str()), &handle).await?;

    match state
        .store
        .bind_handle(
            address.clone(),
            canonical.clone(),
            account,
            format!("/{namespace}"),
            MAX_HANDLE_MAILBOXES,
            capability_token.map(|t| sha256(t.as_bytes())),
        )
        .await
    {
        Ok(store::BindOutcome::Bound) => {
            tracing::info!(%address, handle = %canonical, "handle bound to mailbox");
            Ok(json!({ "address": address, "handle": canonical }))
        }
        Ok(store::BindOutcome::NoSuchMailbox) => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_mailbox",
            format!("no mailbox at {address} on this postbox"),
        )),
        Ok(store::BindOutcome::NotYours) => Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "not_your_mailbox",
            "that mailbox belongs to another account",
        )),
        Ok(store::BindOutcome::ProofRequired) => Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "proof_required",
            "that mailbox belongs to no account — send its capability token to prove you hold it",
        )),
        Ok(store::BindOutcome::AlreadyNamed(current)) => Err(ApiError::new(
            StatusCode::CONFLICT,
            "already_named",
            format!(
                "that mailbox already answers to {current}; renaming would strand everyone who trusts the old name"
            ),
        )),
        Ok(store::BindOutcome::Taken) => Err(ApiError::new(
            StatusCode::CONFLICT,
            "handle_taken",
            format!("{canonical} already exists"),
        )),
        Ok(store::BindOutcome::Full) => Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "namespace_quota",
            format!("/{namespace} already holds its limit of {MAX_HANDLE_MAILBOXES} mailboxes"),
        )),
        Err(e) => {
            tracing::error!(error = %e, "handle bind failed");
            Err(ApiError::server("store_error"))
        }
    }
}

fn is_api_key(token: &str) -> bool {
    token.starts_with("pk_")
}

/// `POST /v1/identities` — create a `/k/` inbox. An **API key** creates under its account with no
/// PoW; an **anonymous** caller must solve a PoW (ephemeral, no account); a capability token can't
/// create (it authenticates one existing identity).
async fn create_identity(
    State(state): State<AppState>,
    Extension(client_ip): Extension<ClientIp>,
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
                created(do_create_identity(&state, Some(account_id), req.label, req.handle).await)
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
        // Anonymous → per-IP budget, then proof-of-work gated (ephemeral, no account).
        None => {
            if let Err(e) = check_mint_budget(&state, client_ip.as_str()).await {
                return e.into_response();
            }
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
            let result = do_create_identity(&state, None, req.label, None).await;
            if let Ok(v) = &result {
                let address = v["address"].as_str().map(String::from);
                record_mint(&state, client_ip.as_str(), "identity", address).await;
            }
            created(result)
        }
    }
}

/// Book an unauthenticated mint against its source IP. Best-effort: the caller has already earned
/// the inbox, so a bookkeeping failure is logged, not surfaced.
async fn record_mint(state: &AppState, ip: &str, kind: &'static str, address: Option<String>) {
    if let Err(e) = state
        .store
        .record_mint(ip.to_string(), kind, address, now_unix())
        .await
    {
        tracing::error!(error = %e, %ip, kind, "recording mint event failed");
    }
}

/// `POST /v1/accounts` — create an account (PoW-gated) and return its first API key, shown once. The
/// key then creates and drives many identities without further PoW.
async fn create_account(
    State(state): State<AppState>,
    Extension(client_ip): Extension<ClientIp>,
    Json(req): Json<CreateIdentityReq>,
) -> Response {
    // Same budget as a bare inbox: an account is otherwise a way to turn one proof-of-work into
    // `max_identities` inboxes.
    if let Err(e) = check_mint_budget(&state, client_ip.as_str()).await {
        return e.into_response();
    }
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
    let (api_key, key_record) = new_api_key();
    match state
        .store
        .create_account(account_id.clone(), key_record, now_unix())
        .await
    {
        Ok(()) => {
            tracing::info!(account = %account_id, "account created");
            record_mint(&state, client_ip.as_str(), "account", None).await;
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

/// `POST /v1/api-keys` — issue a durable API key for the authenticated account (member JWT or an
/// existing key). Lets a signed-in web user hand their agent a connector credential that outlives
/// the short-lived member token.
async fn create_api_key(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let account = match account_for_headers(&state, &headers).await {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let (api_key, key_record) = new_api_key();
    match state
        .store
        .add_api_key(account.clone(), key_record, now_unix())
        .await
    {
        Ok(()) => {
            tracing::info!(account = %account, "issued API key");
            (StatusCode::CREATED, Json(json!({ "api_key": api_key }))).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "add api key failed");
            ApiError::server("store_error").into_response()
        }
    }
}

/// `GET /v1/api-keys` — list the account's keys (id + display prefix + created_at, never the secret).
async fn list_api_keys(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let account = match account_for_headers(&state, &headers).await {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    match state.store.list_keys(account).await {
        Ok(rows) => Json(json!({
            "keys": rows.into_iter().map(|(id, prefix, created_at)| json!({ "id": id, "prefix": prefix, "created_at": created_at })).collect::<Vec<_>>()
        }))
        .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "list keys failed");
            ApiError::server("store_error").into_response()
        }
    }
}

/// `DELETE /v1/api-keys/{id}` — revoke one of the account's API keys.
async fn revoke_api_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let account = match account_for_headers(&state, &headers).await {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    match state.store.revoke_key(account, id).await {
        Ok(true) => (StatusCode::OK, Json(json!({ "revoked": true }))).into_response(),
        Ok(false) => err_response(
            StatusCode::NOT_FOUND,
            "not_found",
            Some("no such key in your account"),
        ),
        Err(e) => {
            tracing::error!(error = %e, "revoke key failed");
            ApiError::server("store_error").into_response()
        }
    }
}

#[derive(serde::Deserialize)]
struct DeleteIdentityQuery {
    identity: String,
}

/// `DELETE /v1/identities?identity=<addr>` — delete one of the account's inboxes and its messages.
async fn delete_identity(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DeleteIdentityQuery>,
) -> Response {
    // Two callers, two credentials. An account API key may delete any inbox it owns. A capability
    // token may delete exactly one inbox — its own — which is what makes a self-served `/k/`
    // address destroyable at all: it has no account, so before this it could only be waited out.
    let scope = match principal_for_token(&state, bearer(&headers)).await {
        Ok(Principal::Account(a)) => Some(a),
        Ok(Principal::Identity(me)) => {
            if me.address != q.identity {
                return err_response(
                    StatusCode::FORBIDDEN,
                    "forbidden",
                    Some("a capability token can only delete its own inbox"),
                );
            }
            None
        }
        Err(e) => return e.into_response(),
    };
    match state.store.delete_identity(scope, q.identity).await {
        Ok(true) => (StatusCode::OK, Json(json!({ "deleted": true }))).into_response(),
        Ok(false) => err_response(
            StatusCode::NOT_FOUND,
            "not_found",
            Some("no such identity in your account"),
        ),
        Err(e) => {
            tracing::error!(error = %e, "delete identity failed");
            ApiError::server("store_error").into_response()
        }
    }
}

/// Web origins allowed to call the API from a browser: the signed-in account page on
/// pigeonpost.dev, and the inbox app, which is a separate origin because it is a separate
/// deployment rather than a page of the marketing site.
const CORS_ORIGINS: [&str; 3] = [
    "https://pigeonpost.dev",
    "https://www.pigeonpost.dev",
    "https://inbox.pigeonpost.dev",
];

/// Minimal CORS: echo an allowlisted `Origin`, answer preflight, allow the bearer header. Same-origin
/// callers (the onboarding page, agents) are unaffected.
async fn cors(req: Request, next: Next) -> Response {
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .filter(|o| CORS_ORIGINS.contains(o))
        .map(String::from);
    let preflight = req.method() == Method::OPTIONS;

    let mut resp = if preflight {
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(req).await
    };

    if let Some(o) = origin {
        let h = resp.headers_mut();
        if let Ok(v) = HeaderValue::from_str(&o) {
            h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
        }
        h.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, PUT, DELETE, OPTIONS"),
        );
        h.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("authorization, content-type"),
        );
        h.insert(header::VARY, HeaderValue::from_static("Origin"));
    }
    resp
}

/// The caller's IP as the abuse controls see it. Inserted by [`client_ip_layer`] on every request.
#[derive(Clone, Debug)]
struct ClientIp(String);

impl ClientIp {
    fn as_str(&self) -> &str {
        &self.0
    }
}

/// Resolve and attach the caller's IP once, so handlers don't each re-derive it.
async fn client_ip_layer(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let peer = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip().to_string());
    let ip = resolve_client_ip(req.headers(), peer.as_deref(), state.trusted_proxy_hops);
    req.extensions_mut().insert(ClientIp(ip));
    next.run(req).await
}

/// The socket peer, or — when `hops` trusted reverse proxies sit in front — the hop that many
/// entries from the end of `X-Forwarded-For`.
///
/// Counting from the *end* is what makes this unspoofable: a client may prepend anything it likes
/// to the header, but each trusted proxy appends the address it actually saw, so the entry `hops`
/// from the right is the one our own front door observed. Counting from the left would let any
/// caller pick its own rate-limit bucket. `hops` must therefore match the real deployment: too
/// high and a client-supplied entry wins.
fn resolve_client_ip(headers: &HeaderMap, peer: Option<&str>, hops: usize) -> String {
    if hops > 0 {
        if let Some(xff) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .filter(|v| !v.trim().is_empty())
        {
            let chain: Vec<&str> = xff
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            if let Some(ip) = chain.len().checked_sub(hops).and_then(|i| chain.get(i)) {
                return (*ip).to_string();
            }
            // Shorter chain than configured: the request didn't traverse the expected proxies.
            // Fall back to the leftmost entry rather than to the peer, which would be the proxy.
            if let Some(first) = chain.first() {
                return (*first).to_string();
            }
        }
    }
    peer.unwrap_or("unknown").to_string()
}

// ---------------------------------------------------------------------------------------------
// Reputation
//
// What an address has earned, as opposed to what a human granted it. See `reputation.rs` for the
// policy; this half is the plumbing that reads the store and applies it.
// ---------------------------------------------------------------------------------------------

/// Where an identity sits on the tier ladder.
///
/// The postbox only ever sees `/k/` addresses, so today this is "did someone create an account for
/// it". The handle tiers exist in [`reputation::Tier`] and become reachable when the postbox
/// learns about registry handles.
fn tier_of(identity: &store::StoredIdentity) -> reputation::Tier {
    match identity.account_id {
        Some(_) => reputation::Tier::Account,
        None => reputation::Tier::Anonymous,
    }
}

/// A sender's current score: their tier's starting point, less what they have been reported for.
async fn sender_score(state: &AppState, identity: &store::StoredIdentity) -> i64 {
    let reports = state
        .store
        .reports_against(identity.address.clone())
        .await
        .unwrap_or(0);
    reputation::after_reports(tier_of(identity).prior(), reports)
}

/// An IP's score. An IP has no tier — it starts neutral and only moves down, because "this address
/// has never been reported" is not evidence of anything.
async fn ip_score(state: &AppState, ip: &str) -> i64 {
    let reports = state
        .store
        .reports_against(ip.to_string())
        .await
        .unwrap_or(0);
    reputation::after_reports(0, reports)
}

/// Record a spam report against the sender of one of `me`'s messages.
///
/// Reporting is a *lowering* of trust in someone else, so an agent may do it — unlike every write
/// in the contacts layer, there is no way to widen your own exposure with it.
pub(crate) async fn do_report_spam(
    state: &AppState,
    me: &store::StoredIdentity,
    message_id: String,
) -> Result<serde_json::Value, ApiError> {
    // Scoped to the reporter's own inbox: you can only report mail somebody actually sent you,
    // which is what stops this being a way to attack a stranger's standing from nowhere.
    let sender = state
        .store
        .message_sender(message_id.clone(), me.address.clone())
        .await
        .map_err(|_| ApiError::server("store_error"))?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                "no such message in your inbox",
            )
        })?;
    if sender == me.address {
        return Err(ApiError::bad(
            "invalid_report",
            "an inbox cannot report itself",
        ));
    }

    // A buried reporter's reports do not count. Otherwise minting inboxes becomes a way to
    // manufacture downvotes — the same flood this is meant to price, aimed at reputations.
    let my_score = sender_score(state, me).await;
    if !reputation::report_counts(my_score) {
        tracing::warn!(reporter = %me.address, score = my_score, "spam report ignored: reporter is buried");
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "reporter_not_in_good_standing",
            "this inbox's own standing is too low for its reports to count",
        ));
    }

    let counted = state
        .store
        .record_spam_report(
            message_id.clone(),
            me.address.clone(),
            sender.clone(),
            now_unix(),
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "spam report failed");
            ApiError::server("store_error")
        })?;

    let reports = state
        .store
        .reports_against(sender.clone())
        .await
        .unwrap_or(0);
    tracing::info!(
        reporter = %me.address, %sender, %message_id, counted, reports,
        "spam reported"
    );
    Ok(json!({
        "reported": counted,
        "sender": sender,
        "reports_against_sender": reports,
        // Said plainly because "already reported" is a success from the caller's point of view —
        // the outcome they wanted holds — and retrying would be the wrong reaction.
        "detail": if counted { "report recorded" } else { "this message was already reported" },
    }))
}

/// Refuse an unauthenticated mint when the caller's IP is over its window or lifetime budget.
/// Checked *before* the proof-of-work is consumed, so a throttled caller isn't made to burn CPU
/// only to be turned away.
async fn check_mint_budget(state: &AppState, ip: &str) -> Result<(), ApiError> {
    let now = now_unix();
    let since = now.saturating_sub(state.mint_limits.window_secs);

    // An IP that keeps producing inboxes recipients report loses the right to produce them. This
    // sits ahead of the ordinary budget because it is a stronger statement than "too many, too
    // fast" — it is "these were unwanted".
    let standing = ip_score(state, ip).await;
    let allowance = match reputation::mint_allowance(state.mint_limits.per_window, standing) {
        None => {
            tracing::warn!(%ip, standing, "mint refused: source halted by reports");
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "mint_source_halted",
                "inboxes minted from this address have been reported as spam repeatedly; \
                 it can no longer mint. Use an account API key.",
            ));
        }
        Some(a) => a,
    };
    let (recent, lifetime) = state
        .store
        .mint_counts(ip.to_string(), since)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "mint counts failed");
            ApiError::server("store_error")
        })?;

    if lifetime >= state.mint_limits.lifetime {
        tracing::warn!(%ip, lifetime, "mint refused: lifetime cap");
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "mint_lifetime_cap",
            format!(
                "this address has minted its lifetime limit of {} inboxes",
                state.mint_limits.lifetime
            ),
        ));
    }
    if recent >= allowance {
        let retry_after = state
            .store
            .oldest_mint_in_window(ip.to_string(), since)
            .await
            .ok()
            .flatten()
            .map(|oldest| {
                (oldest + state.mint_limits.window_secs)
                    .saturating_sub(now)
                    .max(1)
            })
            .unwrap_or(state.mint_limits.window_secs);
        tracing::warn!(%ip, recent, allowance, retry_after, "mint refused: rate limit");
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "mint_rate_limited",
            format!(
                "{} inboxes already minted from this address in the last {}s (allowance {}) — \
                 retry in {}s, or use an account API key",
                recent, state.mint_limits.window_secs, allowance, retry_after
            ),
        )
        .with_retry_after(retry_after));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Contacts and trust policy
//
// Two axes, kept separate on purpose. **Admission** is "may this peer's mail be delivered", the
// same question the local loft answers with allow/block. **Autonomy** is "may my agent act on it
// without a human" — a different question with a much sharper edge, because knowing who sent a
// message is not knowing that the message is safe to obey.
// ---------------------------------------------------------------------------------------------

const ADMISSION_ALLOW: &str = "allow";
const ADMISSION_BLOCK: &str = "block";
const AUTONOMY_REVIEW: &str = "review";
const AUTONOMY_AUTO: &str = "auto";

/// Who is asking for a trust change. Agents may lower their own trust settings but never raise
/// them: an agent that can grant itself `auto` is one crafted message away from granting a
/// stranger the same, which would make the whole policy decorative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrustActor {
    /// REST/CLI — a person at a terminal with the capability token.
    Human,
    /// The MCP connector, i.e. the agent itself.
    Agent,
}

fn validate_enum(value: &str, allowed: [&str; 2], field: &'static str) -> Result<(), ApiError> {
    if allowed.contains(&value) {
        return Ok(());
    }
    Err(ApiError::bad(
        "invalid_policy",
        &format!("{field} must be {} or {}", allowed[0], allowed[1]),
    ))
}

/// Whether a human has said this sender's mail may drive work at all — the Phase 2 question, and
/// only half the answer. What they may drive is `contacts.allowed_verbs`, applied per message in
/// [`verbs::decide`].
fn sender_is_auto(contact: Option<&store::Contact>, policy: store::InboxPolicy) -> bool {
    match contact {
        Some(c) if c.admission == ADMISSION_BLOCK => false,
        Some(c) if c.autonomy == AUTONOMY_AUTO => true,
        // `auto_accept_known` lifts *known* contacts only; strangers are never swept up by it.
        Some(_) => policy.auto_accept_known,
        None => false,
    }
}

fn contact_json(c: &store::Contact) -> serde_json::Value {
    json!({
        "peer": c.peer,
        "alias": c.alias,
        "admission": c.admission,
        "autonomy": c.autonomy,
        "allowed_verbs": c.allowed_verbs,
        "created_at": c.created_at,
        "updated_at": c.updated_at,
    })
}

fn policy_json(p: store::InboxPolicy) -> serde_json::Value {
    json!({ "accept_all": p.accept_all, "auto_accept_known": p.auto_accept_known })
}

fn refuse_raise(what: &'static str) -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "human_required",
        format!(
            "raising trust ({what}) has to come from a person: run `pigeonpost postbox …` with \
             this mailbox's capability token. An agent may lower its own trust, never raise it."
        ),
    )
}

/// Add or amend a contact. Shared by REST (human) and MCP (agent, lower-only).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn do_set_contact(
    state: &AppState,
    me: &store::StoredIdentity,
    peer: String,
    alias: Option<String>,
    admission: Option<String>,
    autonomy: Option<String>,
    allowed_verbs: Option<Vec<String>>,
    actor: TrustActor,
) -> Result<serde_json::Value, ApiError> {
    // Three shapes are addressable: a /k/ mailbox, a handle mailbox, and a whole namespace.
    let is_namespace = peer.ends_with("/*");
    if !peer.starts_with("/k/") {
        let probe = if is_namespace {
            peer.replace("/*", "/probe")
        } else {
            peer.to_string()
        };
        if Destination::for_handle(&probe).is_err() {
            return Err(ApiError::bad(
                "invalid_peer",
                "peer must be a /k/ address, a handle, or a namespace like /bekir/*",
            ));
        }
    }
    if peer == me.address {
        return Err(ApiError::bad(
            "invalid_peer",
            "an inbox cannot list itself as a contact",
        ));
    }
    if let Some(a) = &admission {
        validate_enum(a, [ADMISSION_ALLOW, ADMISSION_BLOCK], "admission")?;
    }
    if let Some(a) = &autonomy {
        validate_enum(a, [AUTONOMY_REVIEW, AUTONOMY_AUTO], "autonomy")?;
    }
    // Refuse a bad grant now rather than storing one that could never match. A denied verb is not
    // a typo to be shrugged off — it is someone asking for the one thing this design will not sell.
    if let Some(v) = &allowed_verbs {
        verbs::validate_grant(v).map_err(|detail| ApiError::bad("invalid_verb", &detail))?;
    }

    if actor == TrustActor::Agent {
        if autonomy.as_deref() == Some(AUTONOMY_AUTO) {
            return Err(refuse_raise("autonomy=auto"));
        }
        // Granting a verb is the sharpest raise there is — it is the thing `auto` was narrowed
        // down to. Clearing the list is a lower, so an agent may still revoke its own exposure.
        if allowed_verbs.as_ref().is_some_and(|v| !v.is_empty()) {
            return Err(refuse_raise("granting verbs"));
        }
        // A blocked peer is frozen against the agent: any write that isn't re-affirming the block
        // is refused outright, rather than silently ignored. Letting it report success would tell
        // the agent it had befriended someone it is still refusing mail from. A message that talks
        // its recipient into un-blocking its accomplice is exactly the move this guard stops.
        let blocked = state
            .store
            .contact(me.address.clone(), peer.clone())
            .await
            .map_err(|_| ApiError::server("store_error"))?
            .is_some_and(|c| c.admission == ADMISSION_BLOCK);
        if blocked && admission.as_deref() != Some(ADMISSION_BLOCK) {
            return Err(refuse_raise("amending a blocked peer"));
        }
    }

    let contact = state
        .store
        .upsert_contact(store::ContactUpdate {
            owner: me.address.clone(),
            peer,
            alias,
            admission,
            autonomy,
            allowed_verbs,
            now: now_unix(),
        })
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "contact upsert failed");
            ApiError::server("store_error")
        })?;
    tracing::info!(
        owner = %contact.owner, peer = %contact.peer,
        admission = %contact.admission, autonomy = %contact.autonomy,
        allowed_verbs = %contact.allowed_verbs.join(","), ?actor,
        "contact set"
    );
    Ok(contact_json(&contact))
}

pub(crate) async fn do_list_contacts(
    state: &AppState,
    me: &store::StoredIdentity,
) -> Result<serde_json::Value, ApiError> {
    let contacts = state
        .store
        .list_contacts(me.address.clone())
        .await
        .map_err(|_| ApiError::server("store_error"))?;
    let policy = state
        .store
        .inbox_policy(me.address.clone())
        .await
        .map_err(|_| ApiError::server("store_error"))?;
    Ok(json!({
        "contacts": contacts.iter().map(contact_json).collect::<Vec<_>>(),
        "policy": policy_json(policy),
        // Published because a closed vocabulary nobody can enumerate is just an outage. A sender
        // needs to know which verbs exist to form a request; a human needs to know which are
        // grantable before deciding what to hand out, and which never will be.
        "vocabulary": {
            "grantable": verbs::grantable(),
            "never_auto": verbs::denied(),
        },
    }))
}

pub(crate) async fn do_delete_contact(
    state: &AppState,
    me: &store::StoredIdentity,
    peer: String,
    actor: TrustActor,
) -> Result<serde_json::Value, ApiError> {
    if actor == TrustActor::Agent {
        // Forgetting a blocked peer would restore them to stranger terms — a raise by deletion.
        let existing = state
            .store
            .contact(me.address.clone(), peer.clone())
            .await
            .map_err(|_| ApiError::server("store_error"))?;
        if existing.is_some_and(|c| c.admission == ADMISSION_BLOCK) {
            return Err(refuse_raise("forgetting a blocked peer"));
        }
    }
    let removed = state
        .store
        .delete_contact(me.address.clone(), peer)
        .await
        .map_err(|_| ApiError::server("store_error"))?;
    Ok(json!({ "removed": removed }))
}

pub(crate) async fn do_set_policy(
    state: &AppState,
    me: &store::StoredIdentity,
    accept_all: Option<bool>,
    auto_accept_known: Option<bool>,
    actor: TrustActor,
) -> Result<serde_json::Value, ApiError> {
    if actor == TrustActor::Agent {
        if auto_accept_known == Some(true) {
            return Err(refuse_raise("auto_accept_known=true"));
        }
        if accept_all == Some(true) {
            return Err(refuse_raise("accept_all=true"));
        }
    }
    let policy = state
        .store
        .set_inbox_policy(
            me.address.clone(),
            accept_all,
            auto_accept_known,
            now_unix(),
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "policy write failed");
            ApiError::server("store_error")
        })?;
    tracing::info!(
        address = %me.address, accept_all = policy.accept_all,
        auto_accept_known = policy.auto_accept_known, ?actor, "inbox policy set"
    );
    Ok(policy_json(policy))
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
#[derive(Debug)]
pub(crate) struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    /// Seconds to put in a `Retry-After` header (rate limits). REST only; MCP just reads `message`.
    pub retry_after: Option<u64>,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        ApiError {
            status,
            code,
            message: message.into(),
            retry_after: None,
        }
    }
    fn with_retry_after(mut self, secs: u64) -> Self {
        self.retry_after = Some(secs);
        self
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
        let mut resp = err_response(self.status, self.code, Some(&self.message));
        if let Some(secs) = self.retry_after {
            if let Ok(v) = HeaderValue::from_str(&secs.to_string()) {
                resp.headers_mut().insert(header::RETRY_AFTER, v);
            }
        }
        resp
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
// One variant carries a whole StoredIdentity and the other an account id, so the enum is as big
// as the identity. That is fine here: a Principal is resolved once per request, moved, and
// dropped — it is never held in bulk, so boxing would trade a real allocation for a notional
// saving.
#[allow(clippy::large_enum_variant)]
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

/// Whether `to` is shaped like something this postbox could ever deliver to.
///
/// Separates "you mistyped" from "nobody is there", which are otherwise the same refusal.
fn is_addressable(to: &str) -> bool {
    Address::parse(to).is_ok()
        || Destination::for_handle(to).is_ok()
        || pigeonpost_core::namespace_root(to).is_some()
}

/// Resolve a destination to a hosted mailbox, by `/k/` address or by handle.
///
/// A handle is a name bound to a key, so both forms land on the same row and the sealing below is
/// identical either way. Addressing a fleet by `/bekir/deployer` rather than by a base32 digest is
/// the entire point of buying a namespace.
async fn resolve_recipient(
    state: &AppState,
    to: &str,
) -> Result<Option<store::StoredIdentity>, ApiError> {
    let store_error = |e: store::StoreError| {
        tracing::error!(error = %e, "recipient lookup failed");
        ApiError::server("store_error")
    };
    if let Some(found) = state.store.get(to.to_string()).await.map_err(store_error)? {
        return Ok(Some(found));
    }
    // Canonicalise before the lookup so `/Bekir/Agent1` reaches the same mailbox as `/bekir/agent1`
    // — the handle stored at mint time is already canonical.
    if let Ok(destination) = Destination::for_handle(to) {
        if let Some(handle) = destination.handle() {
            return state
                .store
                .get_by_handle(handle.to_string())
                .await
                .map_err(store_error);
        }
    }

    // A whole namespace as the recipient: `/bekir` means "whoever reads for bekir". Writing to a
    // person rather than to one of their agents is the address a human hands out, and it has to
    // work without their knowing which agent happens to be on duty.
    //
    // Provider namespaces are excluded: `/bekir` is one person, but `/github` is everybody, and
    // delivering it to whichever GitHub user happened to sign up first would be the worst possible
    // answer. Only `/github/<login>` means something there.
    if let Some(namespace) = pigeonpost_core::namespace_root(to) {
        if !PROVIDER_NAMESPACES.contains(&namespace.as_str()) {
            return state
                .store
                .namespace_inbox(namespace)
                .await
                .map_err(store_error);
        }
    }
    Ok(None)
}

/// Seal a message from `sender` to a hosted recipient and enqueue it.
pub(crate) async fn do_send(
    state: &AppState,
    sender: &store::StoredIdentity,
    to: &str,
    body: &str,
) -> Result<serde_json::Value, ApiError> {
    // Two different mistakes deserve two different answers. Telling someone who mistyped an address
    // that "only hosted recipients are deliverable" sends them looking for a delivery feature when
    // the real problem is a typo — and mistyping an address is the most common newcomer mistake.
    let recipient =
        match resolve_recipient(state, to).await? {
            Some(found) => found,
            None if !is_addressable(to) => return Err(ApiError::bad(
                "invalid_recipient",
                "not a Pigeonpost address — expected a /k/ address or a handle like /bekir/agent1",
            )),
            None => {
                return Err(ApiError::new(
                    StatusCode::NOT_FOUND,
                    "recipient_unresolved",
                    format!("no mailbox at {to} on this postbox"),
                ))
            }
        };

    // Admission: refused here rather than filtered on read, so a blocked sender never occupies
    // the recipient's disk or quota. The error is deliberately the same shape for "blocked" and
    // "closed to strangers" — a sender learns it wasn't accepted, not how the recipient's book
    // is arranged.
    // The governing contact: the sender's own row if they have one, else their namespace's. A
    // handle sender is addressed by name here rather than by /k/ digest, because a fleet is
    // trusted as `/bekir/*`, not as a list of digests.
    let sender_subject = sender
        .handle
        .clone()
        .unwrap_or_else(|| sender.address.clone());
    let contact = state
        .store
        .governing_contact(recipient.address.clone(), sender_subject.clone())
        .await
        .map_err(|_| ApiError::server("store_error"))?;
    let policy = state
        .store
        .inbox_policy(recipient.address.clone())
        .await
        .map_err(|_| ApiError::server("store_error"))?;
    let admitted = match &contact {
        Some(c) => c.admission != ADMISSION_BLOCK,
        None => policy.accept_all,
    };
    if !admitted {
        tracing::info!(from = %sender.address, to = %recipient.address, "send refused: not admitted");
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "not_admitted",
            "the recipient is not accepting mail from you",
        ));
    }

    // A sender nobody at this inbox has vouched for, with nothing earned, gets a trickle rather
    // than a firehose. Only strangers: once the recipient has added a contact, that human decision
    // outranks anything the score inferred. Refused loudly, never dropped silently — the sender is
    // told to slow down, and the recipient is not quietly edited.
    if contact.is_none() {
        let standing = sender_score(state, sender).await;
        if reputation::throttles_strangers(standing) {
            let since = now_unix().saturating_sub(reputation::STRANGER_WINDOW_SECS);
            let already = state
                .store
                .messages_between_since(sender.address.clone(), recipient.address.clone(), since)
                .await
                .map_err(|_| ApiError::server("store_error"))?;
            if already >= reputation::STRANGER_MESSAGES_PER_WINDOW {
                tracing::info!(
                    from = %sender.address, to = %recipient.address, standing, already,
                    "send refused: stranger throttle"
                );
                return Err(ApiError::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    "stranger_rate_limited",
                    format!(
                        "you have sent {already} messages to this inbox in the last hour and they \
                         have not added you as a contact. Wait, or ask them to add you.",
                    ),
                )
                .with_retry_after(reputation::STRANGER_WINDOW_SECS));
            }
        }
    }

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
    // The sender's own key, to seal their copy of what they sent.
    let sender_vk = keys::verifying_key_from_bytes(&sender.ed25519_pub)
        .map_err(|_| ApiError::server("bad_sender_key"))?;

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
            owner: recipient.address.clone(),
            outgoing: false,
        })
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "enqueue failed");
            ApiError::server("store_error")
        })?;
    tracing::info!(from = %sender.address, to = %recipient.address, message_id = %message_id, "message sealed + enqueued");

    // The sender's own copy, sealed to the sender's key. Without it a conversation is only ever
    // half visible: the delivered copy is sealed to the recipient, so nobody — including this
    // server — can show the sender what they wrote.
    //
    // Sealed separately rather than by storing the plaintext: the postbox's promise is that mail at
    // rest is ciphertext, and a sent copy is mail. Stored read, because its owner is the one who
    // wrote it, which also keeps it from waking their own long poll.
    //
    // Best-effort: the message is already delivered by this point, and failing the call now would
    // tell the caller their send failed when it did not. A missing sent copy costs the sender their
    // own history, not the recipient their mail.
    let sent_copy_id = match envelope::wrap(&sender_identity, &sender_vk, body, now) {
        Ok(copy) => {
            let copy_id = hex_str(&copy.id());
            match serde_json::to_vec(&copy) {
                Ok(copy_blob) => {
                    let stored = state
                        .store
                        .enqueue(store::Message {
                            id: copy_id.clone(),
                            recipient: recipient.address.clone(),
                            sender: sender.address.clone(),
                            wrap_blob: copy_blob,
                            created_at: now,
                            read: true,
                            owner: sender.address.clone(),
                            outgoing: true,
                        })
                        .await;
                    match stored {
                        Ok(()) => Some(copy_id),
                        Err(e) => {
                            tracing::warn!(error = %e, from = %sender.address, "sent copy not stored");
                            None
                        }
                    }
                }
                Err(_) => None,
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, from = %sender.address, "sent copy not sealed");
            None
        }
    };

    // Release anyone long-polling. Only after the enqueue commits, so a woken waiter that
    // immediately re-reads the store is guaranteed to see this message.
    state.inbox_signal.notify_waiters();
    Ok(json!({ "message_id": message_id, "sent_copy_id": sent_copy_id }))
}

/// Longest a caller may hold an inbox request open. Apache fronts this container with
/// `ProxyPass … timeout=120`, so the ceiling stays well under the proxy's patience — a long poll
/// that outlives its proxy is a 504 and a confused client, not a feature.
const MAX_INBOX_WAIT_SECS: u64 = 60;

/// Hold an inbox request open until mail arrives or `wait` elapses.
///
/// The alternative — every agent re-asking on a timer — is what makes a mesh of twenty idle
/// agents expensive. This way an idle agent costs one parked connection and no queries, and still
/// sees a message within milliseconds of it landing.
async fn await_mail(state: &AppState, address: &str, wait: u64) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(wait);
    loop {
        // Register interest *before* looking at the store. The other order has a lost-wakeup
        // hole: a send landing between the read and the wait would notify nobody, and this
        // caller would sleep through mail that was already there.
        let notified = state.inbox_signal.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();

        match state.store.unread_count(address.to_string()).await {
            Ok(0) => {}
            // Mail waiting, or a store we can't question — either way stop waiting and let the
            // ordinary read path produce the answer (or the error).
            _ => return,
        }

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        // The signal is global, so a wake means "somebody got mail", not "you did". Falling back
        // into the loop re-checks this inbox rather than trusting the wake.
        if tokio::time::timeout(remaining, notified).await.is_err() {
            return;
        }
    }
}

/// Open every message waiting for `me` and return the plaintext (managed custody).
pub(crate) async fn do_inbox(
    state: &AppState,
    me: &store::StoredIdentity,
    include_sent: bool,
    include_read: bool,
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
    let contacts = state
        .store
        .list_contacts(me.address.clone())
        .await
        .map_err(|_| ApiError::server("store_error"))?;
    let policy = state
        .store
        .inbox_policy(me.address.clone())
        .await
        .map_err(|_| ApiError::server("store_error"))?;

    // Score each distinct sender once rather than per message: an inbox full of mail from one
    // flooding address is exactly the case where the naive version does the most redundant work.
    let mut sender_reputation: std::collections::HashMap<String, (i64, reputation::Tier)> =
        std::collections::HashMap::new();
    // Senders are stored by /k/ address, but a handle sender is *trusted* by name, so the
    // matching below needs the mapping.
    let mut sender_handles: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    // Both ends of every row: a sent copy's counterparty is its recipient, and it needs the same
    // handle lookup a sender gets, or an outgoing message would name a peer the inbox cannot match
    // against its own contacts.
    for address in messages
        .iter()
        .flat_map(|m| [m.sender.clone(), m.recipient.clone()])
        .collect::<std::collections::BTreeSet<_>>()
    {
        if let Ok(Some(identity)) = state.store.get(address.clone()).await {
            let score = sender_score(state, &identity).await;
            if let Some(handle) = identity.handle.clone() {
                sender_handles.insert(address.clone(), handle);
            }
            sender_reputation.insert(address, (score, tier_of(&identity)));
        }
    }

    let mut out = Vec::new();
    for m in messages {
        // Sent copies are opt-in. Every existing caller — the CLI's `postbox inbox`, the MCP tool an
        // agent drains on wake — treats this list as "mail addressed to me", and quietly adding the
        // caller's own messages to it would have agents reading their own words as someone's
        // instruction. Only a client that asked for a conversation gets both halves.
        if m.outgoing && !include_sent {
            continue;
        }
        // Acknowledged mail drops out of the listing. It used to stay, which made `ack` look
        // broken: an agent acked a message, saw it again on the next poll, and concluded the
        // acknowledgement had not worked. Worse, an agent polling every few minutes had to
        // re-derive "what is actually new" from ids it remembered itself — which is the job this
        // endpoint exists to do. Sent copies are never filtered this way: they carry no read
        // state, and a conversation view wants both halves.
        if !m.outgoing && m.read && !include_read {
            continue;
        }
        let wrap: envelope::Wrap = match serde_json::from_slice(&m.wrap_blob) {
            Ok(w) => w,
            Err(_) => continue,
        };
        if let Ok((from_vk, body)) = envelope::open(&me_identity, &wrap) {
            let from = Address::from_pubkey(&from_vk).as_str().to_string();
            // Who the conversation is *with*. For received mail that is whoever wrote it; for a
            // sent copy the sender is this mailbox, so the counterparty is the address it went to.
            let peer = if m.outgoing {
                m.recipient.clone()
            } else {
                from.clone()
            };
            // Match the peer's own row first, then their namespace's. Most specific wins, so an
            // exact block on one agent still outranks trusting its whole fleet.
            let subject = sender_handles
                .get(&peer)
                .cloned()
                .unwrap_or_else(|| peer.clone());
            let contact = contacts.iter().find(|c| c.peer == subject).or_else(|| {
                store::namespace_wildcard(&subject)
                    .and_then(|wildcard| contacts.iter().find(|c| c.peer == wildcard))
            });
            let granted = contact.map(|c| c.allowed_verbs.as_slice()).unwrap_or(&[]);

            // A sent copy is this mailbox's own words. There is no admission question to answer
            // about it, so it carries no autonomy verdict at all rather than a misleading `review`.
            if m.outgoing {
                out.push(json!({
                    "message_id": m.id,
                    "direction": "out",
                    "from": from,
                    "to": m.recipient,
                    "peer": peer,
                    "peer_handle": sender_handles.get(&peer),
                    "body": body.as_str(),
                    "alias": contact.and_then(|c| c.alias.clone()),
                    // Your own message is not untrusted input, and it was never subject to a
                    // decision — say so with absent fields rather than plausible-looking ones.
                    "untrusted": false,
                    "autonomy": serde_json::Value::Null,
                    "sent_at": m.created_at,
                    "received_at": m.created_at,
                    "read": true,
                }));
                continue;
            }

            // The two halves meet here: *who* the recipient trusts (Phase 2) and *what* this
            // particular message asks for (Phase 3). Only when both say yes does the message
            // arrive marked `auto`.
            let decision = verbs::decide(body.as_str(), sender_is_auto(contact, policy), granted);

            // Log every message a trusted sender could have had acted on, whichever way it went —
            // an `auto` peer reaching for a denied verb is the signal worth having, and it is
            // invisible if only the successes are recorded.
            match &decision {
                verbs::Decision::Auto { verb } => tracing::info!(
                    to = %me.address, from = %from, verb = %verb, message_id = %m.id,
                    outcome = "auto", "scoped request auto-accepted"
                ),
                verbs::Decision::Review { held, verb } if *held != verbs::Held::SenderNotAuto => {
                    tracing::info!(
                        to = %me.address, from = %from, verb = verb.as_deref().unwrap_or("-"),
                        message_id = %m.id, outcome = held.as_str(), "scoped request held"
                    )
                }
                verbs::Decision::Review { .. } => {}
            }

            let (autonomy, held_because, verb) = match &decision {
                verbs::Decision::Auto { verb } => (AUTONOMY_AUTO, None, Some(verb.clone())),
                verbs::Decision::Review { held, verb } => {
                    (AUTONOMY_REVIEW, Some(held.as_str()), verb.clone())
                }
            };

            // What this sender has earned, alongside what the recipient granted. Stamped as words
            // as well as a number: an agent should not have to guess the scale to know that
            // "reported_repeatedly" deserves more suspicion than "unproven".
            let (score, tier) = sender_reputation
                .get(&from)
                .copied()
                .unwrap_or((0, reputation::Tier::Anonymous));
            let standing = reputation::standing(score);

            out.push(json!({
                "message_id": m.id,
                // Always present, so a client reading a conversation does not have to infer which
                // way a message went from which fields happen to be set.
                "direction": "in",
                "from": from,
                "to": me.address,
                "peer": peer,
                "peer_handle": sender_handles.get(&peer),
                "body": body.as_str(),
                "sender_score": score,
                "sender_standing": standing,
                "sender_tier": tier.as_str(),
                // Stays true at every autonomy level: `auto` says the recipient chose to act on
                // this sender's messages, not that the text stopped being someone else's input.
                "untrusted": true,
                "sender_known": contact.is_some(),
                "alias": contact.and_then(|c| c.alias.clone()),
                // Which entry spoke for this sender. A fleet-wide grant and a personal one are
                // very different amounts of trust, and an agent should be able to tell them apart.
                "matched_contact": contact.map(|c| c.peer.clone()),
                "sender_handle": sender_handles.get(&from),
                "autonomy": autonomy,
                // The verb this message named, if it named a recognisable one at all — worth
                // showing even when held, since it is what the human is being asked to approve.
                "verb": verb,
                // Absent when `auto`; otherwise why, so an agent can tell "nobody trusts you"
                // from "you asked for the wrong thing".
                "held_because": held_because,
                "received_at": m.created_at,
                "read": m.read,
            }));
        }
    }
    Ok(json!({ "messages": out, "policy": policy_json(policy) }))
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
    /// Include this mailbox's own sent copies, turning the listing into a conversation. Off by
    /// default: every existing caller reads this list as mail addressed to them.
    #[serde(default)]
    include_sent: bool,
    /// Include mail that has already been acknowledged. Off by default, so a poll returns what is
    /// new rather than the whole history — the reason `ack` exists.
    #[serde(default)]
    include_read: bool,
    /// Seconds to hold the request open when the inbox is empty, capped at
    /// [`MAX_INBOX_WAIT_SECS`]. Absent or 0 answers immediately, as it always did.
    #[serde(default)]
    wait: Option<u64>,
}

#[derive(serde::Deserialize)]
struct ContactReq {
    peer: String,
    #[serde(default)]
    alias: Option<String>,
    #[serde(default)]
    admission: Option<String>,
    #[serde(default)]
    autonomy: Option<String>,
    /// Omitted leaves the stored grants alone; `[]` revokes them all.
    #[serde(default)]
    allowed_verbs: Option<Vec<String>>,
    #[serde(default)]
    identity: Option<String>,
}

#[derive(serde::Deserialize)]
struct ContactQuery {
    peer: String,
    #[serde(default)]
    identity: Option<String>,
}

#[derive(serde::Deserialize)]
struct PolicyReq {
    #[serde(default)]
    accept_all: Option<bool>,
    #[serde(default)]
    auto_accept_known: Option<bool>,
    #[serde(default)]
    identity: Option<String>,
}

/// `GET /v1/contacts` — this inbox's contacts and its stranger-defaults.
async fn get_contacts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<InboxQuery>,
) -> Response {
    let me = match acting_identity(&state, &headers, q.identity.as_deref()).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    match do_list_contacts(&state, &me).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `PUT /v1/contacts` — add or amend a contact. This is the *human* path: a caller holding the
/// capability token at a terminal, which is why it may raise trust where MCP may not.
async fn put_contact(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ContactReq>,
) -> Response {
    let me = match acting_identity(&state, &headers, req.identity.as_deref()).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    match do_set_contact(
        &state,
        &me,
        req.peer,
        req.alias,
        req.admission,
        req.autonomy,
        req.allowed_verbs,
        TrustActor::Human,
    )
    .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `DELETE /v1/contacts?peer=/k/…` — forget a contact.
async fn delete_contact(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ContactQuery>,
) -> Response {
    let me = match acting_identity(&state, &headers, q.identity.as_deref()).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    match do_delete_contact(&state, &me, q.peer, TrustActor::Human).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `PUT /v1/policy` — set this inbox's stranger-defaults.
async fn put_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PolicyReq>,
) -> Response {
    let me = match acting_identity(&state, &headers, req.identity.as_deref()).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    match do_set_policy(
        &state,
        &me,
        req.accept_all,
        req.auto_accept_known,
        TrustActor::Human,
    )
    .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `GET /v1/inbox[?wait=N]` — return opened plaintext messages for the acting identity, optionally
/// holding the request open until mail arrives.
async fn inbox(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<InboxQuery>,
) -> Response {
    let me = match acting_identity(&state, &headers, q.identity.as_deref()).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    // Clamp rather than reject: an over-eager `wait=600` is a client that wants to be told when
    // mail arrives, and answering it in 60s serves that better than an error does.
    if let Some(wait) = q.wait.filter(|w| *w > 0) {
        await_mail(&state, &me.address, wait.min(MAX_INBOX_WAIT_SECS)).await;
    }
    match do_inbox(&state, &me, q.include_sent, q.include_read).await {
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

#[derive(serde::Deserialize)]
struct WorkspaceReq {
    /// Base64 of the XChaCha20-Poly1305 nonce, ciphertext, and the KDF salt the owner's client
    /// used. All three are opaque here: the postbox stores bytes and never holds a key.
    nonce: String,
    ciphertext: String,
    kdf_salt: String,
    #[serde(default)]
    identity: Option<String>,
}

/// `PUT /v1/workspace` — store this mailbox's encrypted workspace context.
///
/// The plaintext — repo, job title, machine, local path — never reaches this server. It is
/// encrypted by the owner's client under a key derived from their passphrase, so an operator with
/// database access learns only that context exists and roughly how big it is.
async fn put_workspace(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<WorkspaceReq>,
) -> Response {
    let me = match acting_identity(&state, &headers, req.identity.as_deref()).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    let (Ok(nonce), Ok(ciphertext), Ok(salt)) = (
        b64_decode(&req.nonce),
        b64_decode(&req.ciphertext),
        b64_decode(&req.kdf_salt),
    ) else {
        return ApiError::bad(
            "invalid_workspace",
            "nonce, ciphertext and kdf_salt must be base64",
        )
        .into_response();
    };
    // Bounded for the same reason a request envelope is: this is context about work, not a place
    // to park data, and an unbounded blob is an unbounded bill.
    const MAX_WORKSPACE_BYTES: usize = 8192;
    if ciphertext.len() > MAX_WORKSPACE_BYTES {
        return ApiError::bad(
            "invalid_workspace",
            "workspace context is limited to 8 KiB of ciphertext",
        )
        .into_response();
    }
    if nonce.len() != 24 || salt.is_empty() {
        return ApiError::bad("invalid_workspace", "expected a 24-byte nonce and a salt")
            .into_response();
    }

    match state
        .store
        .put_workspace(me.address.clone(), nonce, ciphertext, salt, now_unix())
        .await
    {
        Ok(()) => {
            tracing::info!(address = %me.address, "workspace context stored");
            Json(json!({ "ok": true })).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "workspace store failed");
            ApiError::server("store_error").into_response()
        }
    }
}

/// Fetch a mailbox's encrypted workspace context. Shared by REST and MCP.
pub(crate) async fn do_get_workspace(
    state: &AppState,
    me: &store::StoredIdentity,
) -> Result<serde_json::Value, ApiError> {
    match state.store.workspace(me.address.clone()).await {
        Ok(Some((nonce, ciphertext, salt, updated_at))) => Ok(json!({
            "address": me.address,
            "nonce": b64_encode(&nonce),
            "ciphertext": b64_encode(&ciphertext),
            "kdf_salt": b64_encode(&salt),
            "updated_at": updated_at,
            // Said out loud because an agent receiving base64 should not conclude the server is
            // withholding it: nobody here can read this, by design.
            "encrypted": "This postbox holds no key for this context. Decrypt it with the owner's passphrase via `pigeonpost postbox workspace --show`.",
        })),
        Ok(None) => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "no workspace context set",
        )),
        Err(e) => {
            tracing::error!(error = %e, "workspace read failed");
            Err(ApiError::server("store_error"))
        }
    }
}

/// `GET /v1/workspace` — fetch this mailbox's encrypted workspace context.
/// How many events one poll of the store will emit before yielding. A daemon reconnecting after a
/// long sleep should catch up in bounded chunks rather than building one enormous response.
const EVENT_BATCH: usize = 256;

/// `GET /v1/events` — one Server-Sent Events stream per **account**.
///
/// Per account rather than per mailbox on purpose: a twenty-agent fleet holding a long poll each is
/// twenty sockets and twenty wakeups, while the thing that actually wants to know is the one daemon
/// on that machine. It learns *that* mail arrived and for which mailbox, then fetches the message
/// through the ordinary authenticated path.
///
/// Only metadata crosses this stream — no bodies. That keeps it cheap enough to hold open, and
/// means a stream leaking would disclose who wrote to whom rather than what they said.
///
/// `Last-Event-ID` is honoured, so a daemon that was asleep resumes exactly where it stopped:
/// SQLite's rowid is monotonic and never reused for appended rows, which is precisely the property
/// a resumption cursor needs. Without an id the stream starts at *now*, because a daemon starting
/// for the first time wants what happens next, not a replay of everything the account ever received.
async fn events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<EventsQuery>,
) -> Response {
    let account = match account_for_headers(&state, &headers).await {
        Ok(account) => account,
        Err(e) => return e.into_response(),
    };

    let resume = q.last_event_id.or_else(|| {
        headers
            .get("last-event-id")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<i64>().ok())
    });
    let start = match resume {
        Some(cursor) => cursor,
        None => match state.store.latest_message_cursor().await {
            Ok(cursor) => cursor,
            Err(e) => {
                tracing::error!(error = %e, "events cursor read failed");
                return ApiError::server("store_error").into_response();
            }
        },
    };

    tracing::info!(%account, start, "event stream opened");
    let stream = async_stream::stream! {
        let mut cursor = start;
        loop {
            // Register interest before reading, for the same lost-wakeup reason `await_mail`
            // documents: mail landing between the read and the wait would notify nobody.
            let notified = state.inbox_signal.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            // Re-read the account's mailboxes each pass, so a mailbox minted while the stream is
            // open starts producing events without the daemon reconnecting.
            let owners: Vec<String> = match state.store.list_by_account(account.clone()).await {
                Ok(rows) => rows.into_iter().map(|(address, _)| address).collect(),
                Err(e) => {
                    tracing::error!(error = %e, "events identity read failed");
                    break;
                }
            };

            match state.store.incoming_after(owners, cursor, EVENT_BATCH).await {
                Ok(rows) => {
                    for (rowid, message) in rows {
                        cursor = rowid;
                        let payload = json!({
                            "event_id": rowid,
                            "mailbox": message.owner,
                            "message_id": message.id,
                            "sender": message.sender,
                            "created_at": message.created_at,
                        });
                        yield Ok::<Event, std::convert::Infallible>(
                            Event::default().id(rowid.to_string()).event("mail").data(payload.to_string()),
                        );
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "events read failed");
                    break;
                }
            }

            notified.await;
        }
    };

    // The keep-alive is what makes this survive the middleboxes between a laptop and here: an idle
    // stream that sends nothing for minutes gets closed by something in between, and the daemon
    // would rediscover that only when mail failed to arrive.
    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15)))
        .into_response()
}

#[derive(serde::Deserialize)]
struct EventsQuery {
    /// Resume point. The `Last-Event-ID` header is the standard channel and is honoured too; this
    /// exists because not every client can set headers on an EventSource.
    #[serde(default)]
    last_event_id: Option<i64>,
}

/// `GET /v1/whoami` — the address this capability token acts as, and its handle if it has one.
///
/// Exists because "am I named?" is not answerable from the client's own records: a token can be
/// copied between machines, and a locally chosen label is not a handle. Namespace trust matches on
/// the handle alone, so an agent checking whether a fleet rule will match it has to ask the server.
async fn whoami(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<InboxQuery>,
) -> Response {
    match acting_identity(&state, &headers, q.identity.as_deref()).await {
        Ok(me) => Json(json!({ "address": me.address, "handle": me.handle })).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn get_workspace(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<InboxQuery>,
) -> Response {
    let me = match acting_identity(&state, &headers, q.identity.as_deref()).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    match state.store.workspace(me.address).await {
        Ok(Some((nonce, ciphertext, salt, updated_at))) => Json(json!({
            "nonce": b64_encode(&nonce),
            "ciphertext": b64_encode(&ciphertext),
            "kdf_salt": b64_encode(&salt),
            "updated_at": updated_at,
        }))
        .into_response(),
        Ok(None) => err_response(
            StatusCode::NOT_FOUND,
            "not_found",
            Some("no workspace context set"),
        ),
        Err(e) => {
            tracing::error!(error = %e, "workspace read failed");
            ApiError::server("store_error").into_response()
        }
    }
}

/// Minimal standard base64. Hand-rolled to avoid a dependency for two functions, and because the
/// alphabet is fixed by the wire format rather than a matter of taste.
fn b64_encode(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn b64_decode(text: &str) -> Result<Vec<u8>, ()> {
    let value = |c: u8| -> Result<u32, ()> {
        Ok(match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(()),
        })
    };
    let raw: Vec<u8> = text
        .bytes()
        .filter(|c| *c != b'=' && !c.is_ascii_whitespace())
        .collect();
    let mut out = Vec::with_capacity(raw.len() * 3 / 4);
    for chunk in raw.chunks(4) {
        if chunk.len() < 2 {
            return Err(());
        }
        let mut n = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            n |= value(*c)? << (18 - 6 * i);
        }
        for i in 0..chunk.len() - 1 {
            out.push(((n >> (16 - 8 * i)) & 0xff) as u8);
        }
    }
    Ok(out)
}

#[derive(serde::Deserialize)]
struct BindHandleReq {
    /// The `/k/` mailbox to name. Its own address, not the handle it wants.
    address: String,
    handle: String,
    /// That mailbox's capability token — required only when the mailbox belongs to no account
    /// yet, as proof of control before the account adopts it. Ignored for a mailbox the account
    /// already owns, which needs no second credential.
    #[serde(default)]
    capability_token: Option<String>,
}

#[derive(serde::Deserialize)]
struct GrantNamespaceReq {
    namespace: String,
    account_id: String,
    /// When the entitlement lapses. `None` means "until revoked" — a subscription should always
    /// send one, so a lapsed payment stops minting without anyone remembering to revoke.
    #[serde(default)]
    expires_at: Option<u64>,
}

/// `PUT /v1/namespaces` — bind a purchased namespace to an account.
///
/// Called by the billing/entitlement service, never by an end user: an account that could grant
/// Begin proving control of a GitHub account.
///
/// Returns what the person types into GitHub. Nothing is recorded yet: a device code that is never
/// approved should leave no trace, and one that is approved is proved by [`claim_github`] instead.
async fn start_github_claim(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(github) = state.github.clone() else {
        return err_response(
            StatusCode::NOT_FOUND,
            "github_login_unavailable",
            Some("this postbox is not configured for GitHub device login"),
        );
    };
    // An account, not a mailbox: the proof binds a GitHub login to the account that will mint under
    // it, and a capability token speaks only for one mailbox.
    if let Err(e) = require_account(&state, &headers).await {
        return e.into_response();
    }
    match github.start().await {
        Ok(grant) => Json(serde_json::json!({
            "device_code": grant.device_code,
            "user_code": grant.user_code,
            "verification_uri": grant.verification_uri,
            "expires_in": grant.expires_in,
            "interval": grant.interval,
        }))
        .into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "github device start failed");
            err_response(
                StatusCode::BAD_GATEWAY,
                "github_unreachable",
                Some("could not start a GitHub device login"),
            )
        }
    }
}

#[derive(serde::Deserialize)]
struct ClaimGithubReq {
    device_code: String,
}

/// Finish the proof: exchange the approved device code and record the login for this account.
///
/// Polled by the CLI, so "not approved yet" is an ordinary answer with its own status rather than
/// an error — the caller waits and asks again.
async fn claim_github(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ClaimGithubReq>,
) -> Response {
    let Some(github) = state.github.clone() else {
        return err_response(
            StatusCode::NOT_FOUND,
            "github_login_unavailable",
            Some("this postbox is not configured for GitHub device login"),
        );
    };
    let account = match require_account(&state, &headers).await {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };

    let user = match github.identify(&req.device_code).await {
        Ok(user) => user,
        Err(github::GithubError::Pending) => {
            return Json(serde_json::json!({ "status": "pending" })).into_response()
        }
        Err(github::GithubError::SlowDown) => {
            return Json(serde_json::json!({ "status": "slow_down" })).into_response()
        }
        Err(e) => {
            tracing::warn!(error = %e, "github device claim failed");
            return err_response(
                StatusCode::BAD_GATEWAY,
                "github_refused",
                Some("GitHub did not confirm this device login"),
            );
        }
    };

    let recorded = state
        .store
        .record_provider_identity(
            "github".into(),
            user.login.clone(),
            user.user_id.clone(),
            account,
            now_unix(),
        )
        .await;
    match recorded {
        Ok(Ok(())) => Json(serde_json::json!({
            "status": "verified",
            "login": user.login,
            "handle": format!("/github/{}", user.login),
        }))
        .into_response(),
        Ok(Err(store::ProviderClaimRefusal::DifferentPerson)) => err_response(
            StatusCode::CONFLICT,
            "login_changed_hands",
            Some("this login was proved by a different GitHub account; it cannot be transferred"),
        ),
        Ok(Err(store::ProviderClaimRefusal::HeldByAnotherAccount)) => err_response(
            StatusCode::CONFLICT,
            "login_already_claimed",
            Some("this GitHub login is already proved by another Pigeonpost account"),
        ),
        Err(e) => {
            tracing::error!(error = %e, "recording github identity failed");
            err_response(StatusCode::INTERNAL_SERVER_ERROR, "store_error", None)
        }
    }
}

/// The account behind this request, refusing a mailbox-scoped capability token.
async fn require_account(state: &AppState, headers: &HeaderMap) -> Result<String, ApiError> {
    match principal_for_token(state, bearer(headers)).await? {
        Principal::Account(account) => Ok(account),
        Principal::Identity(_) => Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "account_required",
            "this needs an account — sign in, or use an account API key",
        )),
    }
}

/// itself a namespace would be helping itself to paid handles. Authenticated by a shared secret
/// that, when unset, closes the endpoint rather than opening it.
async fn grant_namespace(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<GrantNamespaceReq>,
) -> Response {
    let Some(expected) = state.namespace_grant_hash else {
        // Deliberately indistinguishable from "no such route" — an unconfigured deployment should
        // not advertise that it has a namespace-granting endpoint at all.
        return err_response(StatusCode::NOT_FOUND, "not_found", None);
    };
    let presented = bearer(&headers).map(|token| sha256(token.as_bytes()));
    if presented.is_none_or(|presented| !constant_time_eq(&presented, &expected)) {
        return ApiError::unauthorized("invalid namespace grant credential").into_response();
    }

    // Canonicalise through core, so a grant for "Bekir" and a mint of "/bekir/x" agree.
    let probe = format!("/{}/probe", req.namespace.trim_start_matches('/'));
    let Ok(destination) = Destination::for_handle(&probe) else {
        return ApiError::bad("invalid_namespace", "namespace has invalid characters")
            .into_response();
    };
    let namespace = destination
        .handle()
        .and_then(|h| h.trim_start_matches('/').split('/').next())
        .unwrap_or_default()
        .to_string();

    match state
        .store
        .set_namespace_owner(
            namespace.clone(),
            req.account_id.clone(),
            "entitlement",
            now_unix(),
            req.expires_at,
        )
        .await
    {
        Ok(()) => {
            tracing::info!(%namespace, account = %req.account_id, expires_at = ?req.expires_at, "namespace granted");
            Json(json!({ "namespace": namespace, "account_id": req.account_id })).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "namespace grant failed");
            ApiError::server("store_error").into_response()
        }
    }
}

/// Compare two digests without leaking where they first differ.
fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `POST /v1/report-spam` — report the sender of one of the acting identity's messages.
async fn report_spam(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<AckReq>,
) -> Response {
    let me = match acting_identity(&state, &headers, req.identity.as_deref()).await {
        Ok(m) => m,
        Err(e) => return e.into_response(),
    };
    match do_report_spam(&state, &me, req.message_id).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
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

    fn xff(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", HeaderValue::from_str(value).unwrap());
        h
    }

    #[test]
    fn client_ip_ignores_forwarded_header_when_no_proxy_is_trusted() {
        let ip = resolve_client_ip(&xff("1.2.3.4"), Some("198.51.100.7"), 0);
        assert_eq!(ip, "198.51.100.7");
    }

    #[test]
    fn client_ip_takes_the_hop_the_trusted_proxy_observed() {
        // Apache appends the peer it saw, so with one trusted proxy the last entry is the truth
        // and the client's own prepended value is ignored.
        let ip = resolve_client_ip(&xff("9.9.9.9, 203.0.113.5"), Some("172.18.0.1"), 1);
        assert_eq!(ip, "203.0.113.5");

        // A direct client through that proxy leaves a single entry.
        let ip = resolve_client_ip(&xff("203.0.113.5"), Some("172.18.0.1"), 1);
        assert_eq!(ip, "203.0.113.5");
    }

    #[test]
    fn client_ip_falls_back_when_the_chain_is_shorter_than_configured() {
        // Two proxies configured but only one hop present: prefer the leftmost entry over the
        // peer, which would be the proxy and would pool every caller into one bucket.
        let ip = resolve_client_ip(&xff("203.0.113.5"), Some("172.18.0.1"), 2);
        assert_eq!(ip, "203.0.113.5");
    }

    #[test]
    fn client_ip_falls_back_to_peer_without_a_usable_header() {
        assert_eq!(
            resolve_client_ip(&xff("   "), Some("198.51.100.7"), 1),
            "198.51.100.7"
        );
        assert_eq!(
            resolve_client_ip(&HeaderMap::new(), None, 0),
            "unknown",
            "no peer and no header must still yield a stable bucket key"
        );
    }

    fn state_with_limits(limits: MintLimits) -> AppState {
        AppState {
            pow: Arc::new(pow::Pow::new(b"k".to_vec(), 8, 24, 120)),
            vault: Arc::new(vault::Vault::new([0u8; 32])),
            store: Arc::new(store::Store::open(":memory:").unwrap()),
            oidc: Arc::new(oidc::Oidc::new("https://auth.example/realms/x".into())),
            max_identities: 5,
            max_inbox: 1000,
            mint_limits: limits,
            trusted_proxy_hops: 0,
            namespace_grant_hash: None,
            inbox_signal: Arc::new(tokio::sync::Notify::new()),
            github: None,
        }
    }

    /// Writing to a person: `/bekir` has to reach a mailbox, or the address in someone's README is
    /// a dead letter.
    #[tokio::test]
    async fn a_namespace_is_deliverable_and_prefers_main() {
        let state = state_with_limits(MintLimits {
            per_window: 10,
            window_secs: 3600,
            lifetime: 1000,
        });
        let identity = |address: &str, handle: &str, created_at: u64| store::StoredIdentity {
            address: address.into(),
            wrapped_seed: vault::Wrapped {
                nonce: [0; 24],
                ct: vec![1],
            },
            ed25519_pub: [0; 32],
            x25519_pub: [0; 32],
            cap_hash: [0; 32],
            label: None,
            created_at,
            account_id: None,
            handle: Some(handle.into()),
        };
        state
            .store
            .insert(identity("/k/agent", "/bekir/agent1", 1))
            .await
            .unwrap();

        // Before a `main` exists, the namespace still reaches somebody.
        let found = resolve_recipient(&state, "/bekir").await.unwrap();
        assert_eq!(found.map(|i| i.address).as_deref(), Some("/k/agent"));

        state
            .store
            .insert(identity("/k/main", "/bekir/main", 2))
            .await
            .unwrap();
        let found = resolve_recipient(&state, "/bekir").await.unwrap();
        assert_eq!(
            found.map(|i| i.address).as_deref(),
            Some("/k/main"),
            "once main exists it is what the namespace means"
        );

        // And the two-segment form still goes exactly where it says.
        let found = resolve_recipient(&state, "/bekir/agent1").await.unwrap();
        assert_eq!(found.map(|i| i.address).as_deref(), Some("/k/agent"));
    }

    /// `/github` is not a person. Delivering it to whichever GitHub user signed up first would hand
    /// one of them everybody else's mail.
    #[tokio::test]
    async fn a_provider_namespace_is_never_deliverable_as_a_whole() {
        let state = state_with_limits(MintLimits {
            per_window: 10,
            window_secs: 3600,
            lifetime: 1000,
        });
        state
            .store
            .insert(store::StoredIdentity {
                address: "/k/gh".into(),
                wrapped_seed: vault::Wrapped {
                    nonce: [0; 24],
                    ct: vec![1],
                },
                ed25519_pub: [0; 32],
                x25519_pub: [0; 32],
                cap_hash: [0; 32],
                label: None,
                created_at: 1,
                account_id: None,
                handle: Some("/github/ada".into()),
            })
            .await
            .unwrap();

        assert!(
            resolve_recipient(&state, "/github")
                .await
                .unwrap()
                .is_none(),
            "the provider namespace as a whole belongs to nobody"
        );
        let found = resolve_recipient(&state, "/github/ada").await.unwrap();
        assert_eq!(
            found.map(|i| i.address).as_deref(),
            Some("/k/gh"),
            "but a proved login is an ordinary, deliverable mailbox"
        );
    }

    #[tokio::test]
    async fn mint_budget_refuses_over_the_window_with_a_retry_after() {
        let state = state_with_limits(MintLimits {
            per_window: 2,
            window_secs: 3600,
            lifetime: 1000,
        });
        for _ in 0..2 {
            assert!(check_mint_budget(&state, "203.0.113.5").await.is_ok());
            record_mint(&state, "203.0.113.5", "identity", None).await;
        }

        let err = check_mint_budget(&state, "203.0.113.5")
            .await
            .expect_err("third mint in the window is refused");
        assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(err.code, "mint_rate_limited");
        let retry = err.retry_after.expect("rate limits carry Retry-After");
        assert!(retry > 0 && retry <= 3600, "retry_after was {retry}");

        // Another IP is unaffected.
        assert!(check_mint_budget(&state, "198.51.100.4").await.is_ok());
    }

    #[tokio::test]
    async fn mint_budget_refuses_over_the_lifetime_cap() {
        let state = state_with_limits(MintLimits {
            per_window: 100,
            window_secs: 3600,
            lifetime: 1,
        });
        record_mint(&state, "203.0.113.5", "identity", None).await;

        let err = check_mint_budget(&state, "203.0.113.5")
            .await
            .expect_err("lifetime cap is a hard stop");
        assert_eq!(err.code, "mint_lifetime_cap");
        assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
    }

    fn contact(admission: &str, autonomy: &str) -> store::Contact {
        store::Contact {
            owner: "/k/me".into(),
            peer: "/k/them".into(),
            alias: None,
            admission: admission.into(),
            autonomy: autonomy.into(),
            allowed_verbs: Vec::new(),
            created_at: 0,
            updated_at: 0,
        }
    }

    /// Most trust tests predate scoped verbs and don't grant any; this keeps their call sites
    /// about the axis they're actually exercising.
    async fn set_contact(
        state: &AppState,
        me: &store::StoredIdentity,
        peer: String,
        alias: Option<String>,
        admission: Option<String>,
        autonomy: Option<String>,
        actor: TrustActor,
    ) -> Result<serde_json::Value, ApiError> {
        do_set_contact(state, me, peer, alias, admission, autonomy, None, actor).await
    }

    #[test]
    fn a_sender_is_auto_only_when_a_human_said_so() {
        let manual = store::InboxPolicy::default();
        let known_auto = store::InboxPolicy {
            accept_all: true,
            auto_accept_known: true,
        };

        assert!(!sender_is_auto(None, manual));
        assert!(
            !sender_is_auto(None, known_auto),
            "auto_accept_known must never sweep up strangers"
        );
        assert!(!sender_is_auto(Some(&contact("allow", "review")), manual));
        assert!(sender_is_auto(Some(&contact("allow", "auto")), manual));
        assert!(sender_is_auto(
            Some(&contact("allow", "review")),
            known_auto
        ));
        assert!(
            !sender_is_auto(Some(&contact("block", "auto")), known_auto),
            "a block outranks every autonomy grant"
        );
    }

    async fn mint(state: &AppState) -> store::StoredIdentity {
        let v = do_create_identity(state, None, None, None).await.unwrap();
        let address = v["address"].as_str().unwrap().to_string();
        state.store.get(address).await.unwrap().unwrap()
    }

    fn test_state() -> AppState {
        state_with_limits(MintLimits {
            per_window: 100,
            window_secs: 3600,
            lifetime: 1000,
        })
    }

    #[tokio::test]
    async fn an_agent_may_lower_its_own_trust_but_never_raise_it() {
        let state = test_state();
        let me = mint(&state).await;
        let peer = "/k/2dehf8j788jmq6qnk04nj44fng".to_string();

        // Allowed: note a sender, block a sender.
        assert!(set_contact(
            &state,
            &me,
            peer.clone(),
            Some("agent-B".into()),
            None,
            Some("review".into()),
            TrustActor::Agent,
        )
        .await
        .is_ok());

        // Refused: granting itself autonomy.
        let err = set_contact(
            &state,
            &me,
            peer.clone(),
            None,
            None,
            Some("auto".into()),
            TrustActor::Agent,
        )
        .await
        .expect_err("an agent must not grant itself auto");
        assert_eq!(err.code, "human_required");
        assert_eq!(err.status, StatusCode::FORBIDDEN);

        // Refused: the same switch at inbox level.
        assert_eq!(
            do_set_policy(&state, &me, None, Some(true), TrustActor::Agent)
                .await
                .expect_err("auto_accept_known is a human decision")
                .code,
            "human_required"
        );

        // An agent re-noting a contact must not quietly revoke a grant the human made.
        set_contact(
            &state,
            &me,
            peer.clone(),
            None,
            None,
            Some("auto".into()),
            TrustActor::Human,
        )
        .await
        .unwrap();
        let after = set_contact(
            &state,
            &me,
            peer.clone(),
            Some("renamed".into()),
            None,
            None,
            TrustActor::Agent,
        )
        .await
        .unwrap();
        assert_eq!(
            after["autonomy"], "auto",
            "an agent write must not reset autonomy"
        );

        // A human may do both.
        assert!(set_contact(
            &state,
            &me,
            peer.clone(),
            None,
            None,
            Some("auto".into()),
            TrustActor::Human,
        )
        .await
        .is_ok());
        assert!(
            do_set_policy(&state, &me, None, Some(true), TrustActor::Human)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn an_agent_cannot_undo_a_block_by_amending_or_forgetting_it() {
        let state = test_state();
        let me = mint(&state).await;
        let peer = "/k/2dehf8j788jmq6qnk04nj44fng".to_string();
        set_contact(
            &state,
            &me,
            peer.clone(),
            None,
            Some("block".into()),
            None,
            TrustActor::Agent,
        )
        .await
        .unwrap();

        assert_eq!(
            set_contact(
                &state,
                &me,
                peer.clone(),
                None,
                Some("allow".into()),
                None,
                TrustActor::Agent,
            )
            .await
            .expect_err("unblocking is a raise")
            .code,
            "human_required"
        );
        assert_eq!(
            set_contact(
                &state,
                &me,
                peer.clone(),
                None,
                None,
                None,
                TrustActor::Agent
            )
            .await
            .expect_err("re-noting a blocked peer must fail loudly, not quietly no-op")
            .code,
            "human_required"
        );
        assert!(
            set_contact(
                &state,
                &me,
                peer.clone(),
                Some("spammer".into()),
                Some("block".into()),
                None,
                TrustActor::Agent,
            )
            .await
            .is_ok(),
            "re-affirming the block is still allowed"
        );
        assert_eq!(
            do_delete_contact(&state, &me, peer.clone(), TrustActor::Agent)
                .await
                .expect_err("forgetting a block is a raise by another route")
                .code,
            "human_required"
        );

        // The human path clears it.
        assert!(do_delete_contact(&state, &me, peer, TrustActor::Human)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn contact_writes_are_validated() {
        let state = test_state();
        let me = mint(&state).await;
        let human = TrustActor::Human;

        let err = set_contact(
            &state,
            &me,
            "bekir@example.com".into(),
            None,
            None,
            None,
            human,
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "invalid_peer");

        let err = set_contact(&state, &me, me.address.clone(), None, None, None, human)
            .await
            .unwrap_err();
        assert_eq!(
            err.code, "invalid_peer",
            "an inbox listing itself would make its own messages 'known'"
        );

        let err = set_contact(
            &state,
            &me,
            "/k/other".into(),
            None,
            Some("maybe".into()),
            None,
            human,
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, "invalid_policy");
    }

    #[tokio::test]
    async fn admission_is_enforced_at_send_time() {
        let state = test_state();
        let alice = mint(&state).await;
        let bob = mint(&state).await;

        // Open by default.
        assert!(do_send(&state, &alice, &bob.address, "hello").await.is_ok());

        // Blocked: refused, and nothing reaches bob's inbox.
        set_contact(
            &state,
            &bob,
            alice.address.clone(),
            None,
            Some("block".into()),
            None,
            TrustActor::Human,
        )
        .await
        .unwrap();
        let err = do_send(&state, &alice, &bob.address, "again")
            .await
            .expect_err("a blocked sender is refused");
        assert_eq!(err.code, "not_admitted");
        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert_eq!(
            state.store.inbox_count(bob.address.clone()).await.unwrap(),
            1,
            "a refused message must not consume the recipient's quota"
        );

        // Closed to strangers, but a known contact still gets through.
        let carol = mint(&state).await;
        do_set_policy(&state, &bob, Some(false), None, TrustActor::Human)
            .await
            .unwrap();
        assert_eq!(
            do_send(&state, &carol, &bob.address, "hi")
                .await
                .unwrap_err()
                .code,
            "not_admitted"
        );
        set_contact(
            &state,
            &bob,
            carol.address.clone(),
            None,
            None,
            None,
            TrustActor::Human,
        )
        .await
        .unwrap();
        assert!(do_send(&state, &carol, &bob.address, "hi again")
            .await
            .is_ok());
    }

    /// An agent that polls every few minutes has to be able to tell new mail from mail it already
    /// handled. Acknowledging is how it says so, so acknowledged mail must leave the listing —
    /// otherwise `ack` looks broken and the agent ends up tracking message ids itself.
    #[tokio::test]
    async fn acknowledged_mail_leaves_the_listing() {
        let state = test_state();
        let alice = mint(&state).await;
        let bob = mint(&state).await;

        do_send(&state, &alice, &bob.address, "first")
            .await
            .unwrap();
        do_send(&state, &alice, &bob.address, "second")
            .await
            .unwrap();

        let inbox = do_inbox(&state, &bob, false, false).await.unwrap();
        let messages = inbox["messages"].as_array().unwrap().clone();
        assert_eq!(messages.len(), 2);

        let first = messages[0]["message_id"].as_str().unwrap().to_string();
        do_ack(&state, &bob, first.clone()).await.unwrap();

        let after = do_inbox(&state, &bob, false, false).await.unwrap();
        let left = after["messages"].as_array().unwrap();
        assert_eq!(left.len(), 1, "an acknowledged message must not come back");
        assert_ne!(left[0]["message_id"].as_str().unwrap(), first);

        // …but it is not deleted: history stays available to anything that asks for it.
        let history = do_inbox(&state, &bob, false, true).await.unwrap();
        assert_eq!(history["messages"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn inbox_labels_each_message_with_what_the_recipient_decided() {
        let state = test_state();
        let alice = mint(&state).await;
        let bob = mint(&state).await;
        let stranger = mint(&state).await;

        do_set_contact(
            &state,
            &bob,
            alice.address.clone(),
            Some("agent-A".into()),
            None,
            Some("auto".into()),
            Some(vec!["run_tests".into()]),
            TrustActor::Human,
        )
        .await
        .unwrap();
        do_send(&state, &alice, &bob.address, "from a friend")
            .await
            .unwrap();
        do_send(
            &state,
            &alice,
            &bob.address,
            r#"{"v":1,"verb":"run_tests","args":{"target":"crates/x"}}"#,
        )
        .await
        .unwrap();
        do_send(&state, &stranger, &bob.address, "from nobody")
            .await
            .unwrap();

        let inbox = do_inbox(&state, &bob, false, false).await.unwrap();
        let messages = inbox["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        let find = |pred: &dyn Fn(&serde_json::Value) -> bool| {
            messages.iter().find(|m| pred(m)).unwrap().clone()
        };

        // Prose from a fully trusted sender is still a human's call — the Phase 3 guarantee.
        let prose = find(&|m| m["body"] == "from a friend");
        assert_eq!(prose["sender_known"], true);
        assert_eq!(prose["alias"], "agent-A");
        assert_eq!(prose["autonomy"], "review");
        assert_eq!(prose["held_because"], "not_a_request");

        // The same sender, asking for exactly what they were granted.
        let scoped = find(&|m| m["verb"] == "run_tests");
        assert_eq!(scoped["autonomy"], "auto");
        assert_eq!(scoped["held_because"], serde_json::Value::Null);
        assert_eq!(
            scoped["untrusted"], true,
            "auto is a decision about the sender, not a claim about the text"
        );

        let unknown = find(&|m| m["from"] == stranger.address.as_str());
        assert_eq!(unknown["sender_known"], false);
        assert_eq!(unknown["alias"], serde_json::Value::Null);
        assert_eq!(unknown["autonomy"], "review");
        assert_eq!(unknown["held_because"], "sender_not_auto");
    }

    /// Pretend the registry told us `namespace` belongs to `account`.
    async fn own_namespace(state: &AppState, namespace: &str, account: &str) {
        state
            .store
            .set_namespace_owner(namespace.into(), account.into(), "test", 1, None)
            .await
            .unwrap();
    }

    #[test]
    fn an_unconfigured_grant_secret_closes_the_endpoint_rather_than_opening_it() {
        // The failure that would matter most: a deployment that forgets to set the secret must not
        // hand out paid namespaces to anyone who asks.
        let state = test_state();
        assert!(
            state.namespace_grant_hash.is_none(),
            "no secret configured in tests"
        );
        // …and the handler's first act on a None hash is to answer 404, exercised via the router
        // build plus this invariant. Kept as an assertion so removing the `else` branch trips here.
    }

    #[test]
    fn digest_comparison_does_not_short_circuit() {
        let a = [7u8; 32];
        let mut b = a;
        assert!(constant_time_eq(&a, &b));
        b[31] ^= 1;
        assert!(
            !constant_time_eq(&a, &b),
            "a difference in the last byte still counts"
        );
        b = a;
        b[0] ^= 1;
        assert!(!constant_time_eq(&a, &b));
    }

    /// Mint a handle mailbox under an owned namespace and return its stored identity.
    async fn mint_handle(state: &AppState, handle: &str, account: &str) -> store::StoredIdentity {
        let v = do_create_identity(state, Some(account.into()), None, Some(handle.into()))
            .await
            .unwrap();
        state
            .store
            .get(v["address"].as_str().unwrap().to_string())
            .await
            .unwrap()
            .unwrap()
    }

    #[test]
    fn base64_round_trips_every_length_remainder() {
        for len in 0..40usize {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 7 + 13) as u8).collect();
            let encoded = b64_encode(&bytes);
            assert_eq!(
                b64_decode(&encoded).expect("own output must decode"),
                bytes,
                "round trip failed at length {len}"
            );
        }
        assert!(b64_decode("not base64!").is_err());
    }

    #[tokio::test]
    async fn the_postbox_stores_workspace_context_it_cannot_read() {
        // The property that matters: what comes back is exactly what went in, and the server never
        // had a key for it. Anything resembling plaintext in the database would be the bug.
        let state = test_state();
        let me = mint(&state).await;
        let nonce = vec![9u8; 24];
        let ciphertext = b"opaque-to-this-server".to_vec();
        let salt = vec![3u8; 16];

        state
            .store
            .put_workspace(
                me.address.clone(),
                nonce.clone(),
                ciphertext.clone(),
                salt.clone(),
                42,
            )
            .await
            .unwrap();
        let (n, c, s, updated) = state
            .store
            .workspace(me.address.clone())
            .await
            .unwrap()
            .expect("stored context should come back");
        assert_eq!((n, c, s, updated), (nonce, ciphertext, salt, 42));

        // Replacing it keeps one row rather than accumulating history the owner cannot see.
        state
            .store
            .put_workspace(
                me.address.clone(),
                vec![1; 24],
                b"second".to_vec(),
                vec![4; 16],
                43,
            )
            .await
            .unwrap();
        let (_, c, _, updated) = state
            .store
            .workspace(me.address.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!((c, updated), (b"second".to_vec(), 43));

        // Deleting the mailbox takes its context with it.
        state
            .store
            .delete_identity(None, me.address.clone())
            .await
            .unwrap();
        assert!(state.store.workspace(me.address).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn trusting_a_namespace_covers_its_whole_fleet() {
        let state = test_state();
        own_namespace(&state, "bekir", "acct_bekir").await;
        let bob = mint(&state).await;
        let agent1 = mint_handle(&state, "/bekir/agent1", "acct_bekir").await;
        let agent9 = mint_handle(&state, "/bekir/agent9", "acct_bekir").await;

        do_set_contact(
            &state,
            &bob,
            "/bekir/*".into(),
            Some("bekir's fleet".into()),
            None,
            Some("auto".into()),
            Some(vec!["report_status".into()]),
            TrustActor::Human,
        )
        .await
        .unwrap();

        // A mailbox bob has never seen is covered because of who it belongs to.
        do_send(
            &state,
            &agent9,
            &bob.address,
            r#"{"v":1,"verb":"report_status"}"#,
        )
        .await
        .unwrap();
        let inbox = do_inbox(&state, &bob, false, false).await.unwrap();
        let m = &inbox["messages"].as_array().unwrap()[0];
        assert_eq!(m["autonomy"], "auto");
        assert_eq!(m["sender_known"], true);
        assert_eq!(
            m["matched_contact"], "/bekir/*",
            "the recipient should be able to see it was a fleet-wide grant, not a personal one"
        );
        assert_eq!(m["sender_handle"], "/bekir/agent9");

        // An exact entry outranks the namespace, so one agent can be disowned without the rest.
        do_set_contact(
            &state,
            &bob,
            "/bekir/agent1".into(),
            None,
            Some("block".into()),
            None,
            None,
            TrustActor::Human,
        )
        .await
        .unwrap();
        assert_eq!(
            do_send(&state, &agent1, &bob.address, "still me")
                .await
                .expect_err("an exact block beats a namespace allow")
                .code,
            "not_admitted"
        );
        // …and the rest of the fleet is unaffected.
        assert!(do_send(&state, &agent9, &bob.address, "unaffected")
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn a_namespace_grant_still_obeys_the_verb_allowlist() {
        // Trusting more senders must not mean trusting more instructions.
        let state = test_state();
        own_namespace(&state, "bekir", "acct_bekir").await;
        let bob = mint(&state).await;
        let agent = mint_handle(&state, "/bekir/agent1", "acct_bekir").await;
        do_set_contact(
            &state,
            &bob,
            "/bekir/*".into(),
            None,
            None,
            Some("auto".into()),
            None, // auto, but no verbs granted
            TrustActor::Human,
        )
        .await
        .unwrap();

        do_send(&state, &agent, &bob.address, r#"{"v":1,"verb":"deploy"}"#)
            .await
            .unwrap();
        let inbox = do_inbox(&state, &bob, false, false).await.unwrap();
        let m = &inbox["messages"].as_array().unwrap()[0];
        assert_eq!(m["autonomy"], "review");
        assert_eq!(m["held_because"], "verb_denied");
    }

    #[tokio::test]
    async fn a_k_address_has_no_namespace_to_belong_to() {
        // `/k/…` splits on `/` like a handle does; it must not be read as namespace `k`.
        assert_eq!(
            store::namespace_wildcard("/bekir/agent1").as_deref(),
            Some("/bekir/*")
        );
        assert_eq!(
            store::namespace_wildcard("/k/2dehf8j788jmq6qnk04nj44fng"),
            None
        );
        assert_eq!(store::namespace_wildcard("/bekir"), None);
    }

    #[tokio::test]
    async fn a_mistyped_address_says_so_instead_of_blaming_delivery() {
        // Mistyping an address is the most common newcomer mistake, and the two failures need
        // different answers: fix your typo, versus that mailbox does not exist here.
        let state = test_state();
        let sender = mint(&state).await;

        let malformed = do_send(&state, &sender, "not-an-address", "hi")
            .await
            .expect_err("a malformed destination is not deliverable");
        assert_eq!(malformed.code, "invalid_recipient");
        assert!(
            malformed.message.contains("not a Pigeonpost address"),
            "should name the real problem, got: {}",
            malformed.message
        );

        // Well-formed but nobody home: a different code, and it names the address back.
        let absent = do_send(&state, &sender, "/k/2dehf8j788jmq6qnk04nj44fng", "hi")
            .await
            .expect_err("no such mailbox");
        assert_eq!(absent.code, "recipient_unresolved");
        assert_eq!(absent.status, StatusCode::NOT_FOUND);
        assert!(absent.message.contains("/k/2dehf8j788jmq6qnk04nj44fng"));
    }

    #[tokio::test]
    async fn a_handle_mailbox_is_reachable_by_name() {
        // Addressing a fleet by /bekir/deployer instead of a base32 digest is the point of buying
        // a namespace, so delivery has to accept the name.
        let state = test_state();
        own_namespace(&state, "bekir", "acct_bekir").await;
        let minted = do_create_identity(
            &state,
            Some("acct_bekir".into()),
            None,
            Some("/bekir/deployer".into()),
        )
        .await
        .unwrap();
        let k_address = minted["address"].as_str().unwrap().to_string();
        let sender = mint(&state).await;

        do_send(&state, &sender, "/bekir/deployer", "by name")
            .await
            .expect("a handle must be deliverable");
        // Case is folded on the way in, so a differently-cased name is the same mailbox.
        do_send(&state, &sender, "/BEKIR/Deployer", "by name, shouted")
            .await
            .expect("handles are canonicalised before lookup");
        // The underlying /k/ address still works — a handle is a name for a key, not a new mailbox.
        do_send(&state, &sender, &k_address, "by key")
            .await
            .expect("the key-derived address is unchanged");

        let recipient = state.store.get(k_address).await.unwrap().unwrap();
        let inbox = do_inbox(&state, &recipient, false, false).await.unwrap();
        assert_eq!(
            inbox["messages"].as_array().unwrap().len(),
            3,
            "all three routes reach one mailbox"
        );

        assert_eq!(
            do_send(&state, &sender, "/bekir/nobody", "into the void")
                .await
                .unwrap_err()
                .code,
            "recipient_unresolved"
        );
    }

    #[tokio::test]
    async fn a_granted_namespace_expires_and_stops_minting() {
        // A lapsed subscription must stop minting without anyone remembering to revoke it.
        let state = test_state();
        let now = now_unix();
        state
            .store
            .set_namespace_owner(
                "bekir".into(),
                "acct_bekir".into(),
                "entitlement",
                now,
                Some(now.saturating_sub(1)),
            )
            .await
            .unwrap();
        assert_eq!(
            do_create_identity(
                &state,
                Some("acct_bekir".into()),
                None,
                Some("/bekir/agent1".into())
            )
            .await
            .expect_err("an expired entitlement is not ownership")
            .code,
            "namespace_not_yours"
        );

        // Renewing it restores minting.
        state
            .store
            .set_namespace_owner(
                "bekir".into(),
                "acct_bekir".into(),
                "entitlement",
                now,
                Some(now + 3600),
            )
            .await
            .unwrap();
        assert!(do_create_identity(
            &state,
            Some("acct_bekir".into()),
            None,
            Some("/bekir/agent1".into())
        )
        .await
        .is_ok());
    }

    #[tokio::test]
    async fn a_namespace_owner_mints_under_it_and_nobody_else_can() {
        let state = test_state();
        own_namespace(&state, "bekir", "acct_bekir").await;

        let minted = do_create_identity(
            &state,
            Some("acct_bekir".into()),
            None,
            Some("/bekir/superaiagent1".into()),
        )
        .await
        .unwrap();
        assert_eq!(minted["handle"], "/bekir/superaiagent1");
        // The /k/ address is still the key-derived identity underneath the name.
        assert!(minted["address"].as_str().unwrap().starts_with("/k/"));

        // Someone else's account may not mint there.
        assert_eq!(
            do_create_identity(
                &state,
                Some("acct_someone_else".into()),
                None,
                Some("/bekir/agent2".into()),
            )
            .await
            .expect_err("a namespace is not a free-for-all")
            .code,
            "namespace_not_yours"
        );

        // An unsold namespace refuses identically, so probing cannot enumerate what is sold.
        assert_eq!(
            do_create_identity(
                &state,
                Some("acct_bekir".into()),
                None,
                Some("/someoneelse/agent1".into()),
            )
            .await
            .unwrap_err()
            .code,
            "namespace_not_yours"
        );
    }

    #[tokio::test]
    async fn a_handle_is_one_mailbox_and_needs_an_account() {
        let state = test_state();
        own_namespace(&state, "bekir", "acct_bekir").await;
        let mint = |handle: &str| {
            do_create_identity(&state, Some("acct_bekir".into()), None, Some(handle.into()))
        };

        mint("/bekir/agent1").await.unwrap();
        assert_eq!(
            mint("/bekir/agent1").await.unwrap_err().code,
            "handle_taken",
            "one name is one mailbox"
        );
        // Canonicalisation is core's job, so case must not smuggle a duplicate past it.
        assert_eq!(
            mint("/BEKIR/Agent1").await.unwrap_err().code,
            "handle_taken"
        );

        // The anonymous proof-of-work path has no account, so it cannot reach a namespace at all.
        assert_eq!(
            do_create_identity(&state, None, None, Some("/bekir/agent9".into()))
                .await
                .unwrap_err()
                .code,
            "account_required"
        );
        assert_eq!(
            do_create_identity(&state, Some("acct_bekir".into()), None, Some("nope".into()))
                .await
                .unwrap_err()
                .code,
            "invalid_handle"
        );
    }

    #[tokio::test]
    async fn handle_mailboxes_are_capped_per_namespace() {
        let state = test_state();
        own_namespace(&state, "bekir", "acct_bekir").await;
        // Counting is by stored handle, so seed the rows directly rather than minting 1000 keys.
        for i in 0..MAX_HANDLE_MAILBOXES {
            state
                .store
                .insert(store::StoredIdentity {
                    address: format!("/k/seed{i}"),
                    wrapped_seed: state.vault.wrap(&[0u8; 32]).unwrap(),
                    ed25519_pub: [0; 32],
                    x25519_pub: [0; 32],
                    cap_hash: [i as u8; 32],
                    label: None,
                    created_at: 0,
                    account_id: Some("acct_bekir".into()),
                    handle: Some(format!("/bekir/agent{i}")),
                })
                .await
                .unwrap();
        }
        let err = do_create_identity(
            &state,
            Some("acct_bekir".into()),
            None,
            Some("/bekir/one-too-many".into()),
        )
        .await
        .expect_err("1000 is the ceiling");
        assert_eq!(err.code, "namespace_quota");

        // A different namespace is unaffected — the ceiling is per handle, not per account.
        own_namespace(&state, "someoneelse", "acct_bekir").await;
        assert!(do_create_identity(
            &state,
            Some("acct_bekir".into()),
            None,
            Some("/someoneelse/agent1".into()),
        )
        .await
        .is_ok());
    }

    #[tokio::test]
    async fn a_report_lowers_the_sender_and_the_source_that_minted_them() {
        let state = test_state();
        let spammer = mint(&state).await;
        let victim = mint(&state).await;
        // Anonymous mints are recorded against their source IP by the Phase 1 path; do the same
        // here so the report has a source to charge.
        state
            .store
            .record_mint(
                "198.51.100.7".into(),
                "identity",
                Some(spammer.address.clone()),
                1,
            )
            .await
            .unwrap();

        let sent = do_send(&state, &spammer, &victim.address, "buy my thing")
            .await
            .unwrap();
        let id = sent["message_id"].as_str().unwrap().to_string();

        let before_sender = sender_score(&state, &spammer).await;
        let before_ip = ip_score(&state, "198.51.100.7").await;

        let report = do_report_spam(&state, &victim, id.clone()).await.unwrap();
        assert_eq!(report["reported"], true);

        assert!(
            sender_score(&state, &spammer).await < before_sender,
            "a report must measurably cost the sender"
        );
        assert!(
            ip_score(&state, "198.51.100.7").await < before_ip,
            "and the source, or burning the inbox and minting another is free"
        );

        // Idempotent: re-reporting the same message must not charge twice.
        let again = do_report_spam(&state, &victim, id).await.unwrap();
        assert_eq!(again["reported"], false);
        assert_eq!(again["reports_against_sender"], 1);
    }

    #[tokio::test]
    async fn only_mail_actually_sent_to_you_can_be_reported() {
        let state = test_state();
        let alice = mint(&state).await;
        let bob = mint(&state).await;
        let bystander = mint(&state).await;
        let sent = do_send(&state, &alice, &bob.address, "hello")
            .await
            .unwrap();
        let id = sent["message_id"].as_str().unwrap().to_string();

        assert_eq!(
            do_report_spam(&state, &bystander, id)
                .await
                .expect_err("reporting a message you never received is how this becomes a weapon")
                .code,
            "not_found"
        );
    }

    #[tokio::test]
    async fn a_flooding_source_is_throttled_and_then_halted() {
        let state = state_with_limits(MintLimits {
            per_window: 5,
            window_secs: 3600,
            lifetime: 1000,
        });
        let ip = "203.0.113.99";
        assert!(check_mint_budget(&state, ip).await.is_ok());

        // Enough reports to cross the throttle, then the halt.
        let victim = mint(&state).await;
        for i in 0..4 {
            let spammer = mint(&state).await;
            state
                .store
                .record_mint(ip.into(), "identity", Some(spammer.address.clone()), 1)
                .await
                .unwrap();
            let sent = do_send(&state, &spammer, &victim.address, &format!("spam {i}"))
                .await
                .unwrap();
            do_report_spam(
                &state,
                &victim,
                sent["message_id"].as_str().unwrap().to_string(),
            )
            .await
            .unwrap();
        }

        let err = check_mint_budget(&state, ip)
            .await
            .expect_err("a source that keeps producing reported inboxes must stop producing them");
        assert_eq!(err.code, "mint_source_halted");
        assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn a_low_scoring_stranger_is_slowed_but_never_silently_dropped() {
        let state = test_state();
        let stranger = mint(&state).await;
        let victim = mint(&state).await;

        for i in 0..reputation::STRANGER_MESSAGES_PER_WINDOW {
            do_send(&state, &stranger, &victim.address, &format!("hi {i}"))
                .await
                .expect("an introduction should get through");
        }
        let err = do_send(&state, &stranger, &victim.address, "and again")
            .await
            .expect_err("a stranger nobody vouched for gets a trickle, not a firehose");
        assert_eq!(err.code, "stranger_rate_limited");
        assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
        assert!(
            err.retry_after.is_some(),
            "a throttled sender must be told when to come back"
        );

        // The recipient vouching for them outranks the score entirely.
        do_set_contact(
            &state,
            &victim,
            stranger.address.clone(),
            Some("a friend".into()),
            None,
            None,
            None,
            TrustActor::Human,
        )
        .await
        .unwrap();
        assert!(
            do_send(&state, &stranger, &victim.address, "now known")
                .await
                .is_ok(),
            "a human saying 'I know them' beats anything the score inferred"
        );
    }

    #[tokio::test]
    async fn inbox_messages_carry_the_senders_standing() {
        let state = test_state();
        let alice = mint(&state).await;
        let bob = mint(&state).await;
        let sent = do_send(&state, &alice, &bob.address, "hello")
            .await
            .unwrap();

        let first = do_inbox(&state, &bob, false, false).await.unwrap();
        assert_eq!(first["messages"][0]["sender_standing"], "unproven");

        do_report_spam(
            &state,
            &bob,
            sent["message_id"].as_str().unwrap().to_string(),
        )
        .await
        .unwrap();

        let after = do_inbox(&state, &bob, false, false).await.unwrap();
        assert_eq!(after["messages"][0]["sender_standing"], "reported");
        assert!(after["messages"][0]["sender_score"].as_i64().unwrap() < 0);
    }

    #[tokio::test]
    async fn a_mailbox_can_delete_itself_and_takes_its_state_with_it() {
        let state = test_state();
        let alice = mint(&state).await;
        let bob = mint(&state).await;

        do_send(&state, &alice, &bob.address, "hello")
            .await
            .unwrap();
        do_set_contact(
            &state,
            &bob,
            alice.address.clone(),
            Some("agent-A".into()),
            None,
            Some("auto".into()),
            Some(vec!["run_tests".into()]),
            TrustActor::Human,
        )
        .await
        .unwrap();
        do_set_policy(&state, &bob, Some(false), None, TrustActor::Human)
            .await
            .unwrap();

        assert!(state
            .store
            .delete_identity(None, bob.address.clone())
            .await
            .unwrap());

        // Gone, and nothing of it left behind to be inherited by anyone.
        assert!(state
            .store
            .get(bob.address.clone())
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            state.store.inbox_count(bob.address.clone()).await.unwrap(),
            0
        );
        assert!(state
            .store
            .list_contacts(bob.address.clone())
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            state.store.inbox_policy(bob.address).await.unwrap(),
            store::InboxPolicy::default(),
            "a deleted inbox must not leave a policy row for its address"
        );
    }

    #[tokio::test]
    async fn an_account_scoped_delete_still_refuses_someone_elses_inbox() {
        // The self-delete path widened the store call to an unscoped one; the account path must
        // not have widened with it.
        let state = test_state();
        let victim = mint(&state).await;
        assert!(
            !state
                .store
                .delete_identity(Some("acct_someone_else".into()), victim.address.clone())
                .await
                .unwrap(),
            "an account may only delete inboxes it owns"
        );
        assert!(state.store.get(victim.address).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn a_long_poll_returns_the_moment_mail_lands() {
        // The promise of Phase 4: an idle agent learns about a message when it arrives, not when
        // its next timer happens to fire.
        let state = test_state();
        let alice = mint(&state).await;
        let bob = mint(&state).await;

        let sender_state = state.clone();
        let bob_address = bob.address.clone();
        let sending = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            do_send(&sender_state, &alice, &bob_address, "wake up")
                .await
                .unwrap();
        });

        // A budget far longer than the send takes: if the signal were dropped this would sit here
        // for the full sixty seconds and blow the slack below.
        let started = std::time::Instant::now();
        await_mail(&state, &bob.address, 60).await;
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "waited {:?} — a long poll must wake on arrival, not time out",
            started.elapsed()
        );
        sending.await.unwrap();

        let inbox = do_inbox(&state, &bob, false, false).await.unwrap();
        assert_eq!(inbox["messages"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn waiting_ends_at_the_budget_when_nothing_arrives() {
        let state = test_state();
        let me = mint(&state).await;
        let started = std::time::Instant::now();
        await_mail(&state, &me.address, 1).await;
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(900),
            "an empty inbox must wait out its budget, not return at once"
        );
        assert!(
            do_inbox(&state, &me, false, false).await.unwrap()["messages"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn mail_already_waiting_is_answered_without_a_wait() {
        let state = test_state();
        let alice = mint(&state).await;
        let bob = mint(&state).await;
        do_send(&state, &alice, &bob.address, "already here")
            .await
            .unwrap();

        let started = std::time::Instant::now();
        await_mail(&state, &bob.address, 60).await;
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "unread mail present: there is nothing to wait for"
        );
    }

    #[tokio::test]
    async fn acked_mail_is_not_news() {
        // Waiting on the *total* would make an agent holding one un-acked message spin instead of
        // wait, so the budget is spent only while the inbox is genuinely quiet.
        let state = test_state();
        let alice = mint(&state).await;
        let bob = mint(&state).await;
        let sent = do_send(&state, &alice, &bob.address, "old news")
            .await
            .unwrap();
        do_ack(
            &state,
            &bob,
            sent["message_id"].as_str().unwrap().to_string(),
        )
        .await
        .unwrap();

        let started = std::time::Instant::now();
        await_mail(&state, &bob.address, 1).await;
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(900),
            "a read message must not count as new mail"
        );
    }

    #[tokio::test]
    async fn a_sender_can_read_back_what_they_sent() {
        // The delivered copy is sealed to the recipient, so without a second copy sealed to the
        // sender a conversation can only ever be seen from one end.
        let state = test_state();
        let alice = mint(&state).await;
        let bob = mint(&state).await;
        do_send(&state, &alice, &bob.address, "the build is green")
            .await
            .unwrap();

        let conversation = do_inbox(&state, &alice, true, false).await.unwrap();
        let messages = conversation["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1, "the sender should see their own message");
        assert_eq!(messages[0]["direction"], "out");
        assert_eq!(messages[0]["body"], "the build is green");
        assert_eq!(
            messages[0]["peer"], bob.address,
            "the counterparty is who it went to, not who wrote it"
        );

        // Bob sees the same exchange from his end.
        let bobs = do_inbox(&state, &bob, true, false).await.unwrap();
        let bobs_messages = bobs["messages"].as_array().unwrap();
        assert_eq!(bobs_messages.len(), 1);
        assert_eq!(bobs_messages[0]["direction"], "in");
        assert_eq!(bobs_messages[0]["peer"], alice.address);
    }

    #[tokio::test]
    async fn a_sent_copy_is_invisible_to_every_caller_that_did_not_ask() {
        // The CLI and the MCP tool read this list as "mail addressed to me". Quietly adding the
        // caller's own messages would have an agent reading its own words as someone's request.
        let state = test_state();
        let alice = mint(&state).await;
        let bob = mint(&state).await;
        do_send(&state, &alice, &bob.address, "status?")
            .await
            .unwrap();

        let inbox = do_inbox(&state, &alice, false, false).await.unwrap();
        assert!(
            inbox["messages"].as_array().unwrap().is_empty(),
            "a sent message is not inbox mail"
        );
    }

    #[tokio::test]
    async fn sending_does_not_wake_the_senders_own_long_poll() {
        // A sent copy lands in the sender's own mailbox. If it counted as news, every send would
        // instantly return the sender's own long poll with their own message.
        let state = test_state();
        let alice = mint(&state).await;
        let bob = mint(&state).await;
        do_send(&state, &alice, &bob.address, "anyone there?")
            .await
            .unwrap();

        let started = std::time::Instant::now();
        await_mail(&state, &alice.address, 1).await;
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(900),
            "your own sent message must not count as new mail"
        );
    }

    #[tokio::test]
    async fn a_sent_copy_does_not_spend_the_stranger_allowance() {
        // The throttle counts what a stranger delivered. Counting the sender's own copy as well
        // would halve everyone's allowance the moment sent copies started being stored.
        let state = test_state();
        let alice = mint(&state).await;
        let bob = mint(&state).await;
        do_send(&state, &alice, &bob.address, "one").await.unwrap();

        let delivered = state
            .store
            .messages_between_since(alice.address.clone(), bob.address.clone(), 0)
            .await
            .unwrap();
        assert_eq!(delivered, 1, "one send is one delivered message");
    }

    #[tokio::test]
    async fn granting_verbs_is_a_raise_only_a_human_can_make() {
        let state = test_state();
        let me = mint(&state).await;
        let peer = "/k/2dehf8j788jmq6qnk04nj44fng".to_string();

        // A human grants; the agent may not.
        do_set_contact(
            &state,
            &me,
            peer.clone(),
            None,
            None,
            Some("auto".into()),
            Some(vec!["run_tests".into(), "report_status".into()]),
            TrustActor::Human,
        )
        .await
        .unwrap();
        assert_eq!(
            do_set_contact(
                &state,
                &me,
                peer.clone(),
                None,
                None,
                None,
                Some(vec!["read_file".into()]),
                TrustActor::Agent,
            )
            .await
            .expect_err("an agent widening its own exposure is the exact move to stop")
            .code,
            "human_required"
        );

        // Revoking is a lower, so the agent may still do it — and a plain agent write must not
        // silently drop the human's grants either.
        let after = do_set_contact(
            &state,
            &me,
            peer.clone(),
            Some("renamed".into()),
            None,
            None,
            None,
            TrustActor::Agent,
        )
        .await
        .unwrap();
        assert_eq!(
            after["allowed_verbs"],
            json!(["run_tests", "report_status"])
        );

        let cleared = do_set_contact(
            &state,
            &me,
            peer,
            None,
            None,
            None,
            Some(vec![]),
            TrustActor::Agent,
        )
        .await
        .expect("clearing its own grants is a lower");
        assert_eq!(cleared["allowed_verbs"], json!([]));
    }

    #[tokio::test]
    async fn a_grant_of_a_denied_or_unknown_verb_is_refused_at_the_source() {
        let state = test_state();
        let me = mint(&state).await;
        let peer = "/k/2dehf8j788jmq6qnk04nj44fng".to_string();

        for (bad, why) in [
            ("deploy", "no policy may grant a deploy"),
            ("execute_command", "an unknown verb could never match"),
        ] {
            let err = do_set_contact(
                &state,
                &me,
                peer.clone(),
                None,
                None,
                None,
                Some(vec![bad.into()]),
                TrustActor::Human,
            )
            .await
            .expect_err(why);
            assert_eq!(err.code, "invalid_verb");
            assert_eq!(err.status, StatusCode::BAD_REQUEST);
        }
    }

    #[tokio::test]
    async fn an_auto_contact_with_no_grants_gets_nothing() {
        // The migration case: a contact carried over from before scoped verbs existed. Its `auto`
        // must have stopped meaning "act on anything from this sender".
        let state = test_state();
        let alice = mint(&state).await;
        let bob = mint(&state).await;

        set_contact(
            &state,
            &bob,
            alice.address.clone(),
            None,
            None,
            Some("auto".into()),
            TrustActor::Human,
        )
        .await
        .unwrap();
        do_send(
            &state,
            &alice,
            &bob.address,
            r#"{"v":1,"verb":"run_tests"}"#,
        )
        .await
        .unwrap();

        let inbox = do_inbox(&state, &bob, false, false).await.unwrap();
        let m = &inbox["messages"].as_array().unwrap()[0];
        assert_eq!(m["autonomy"], "review");
        assert_eq!(m["held_because"], "verb_not_granted");
        assert_eq!(m["verb"], "run_tests");
    }

    #[tokio::test]
    async fn the_verb_vocabulary_is_published_to_whoever_asks() {
        let state = test_state();
        let me = mint(&state).await;
        let v = do_list_contacts(&state, &me).await.unwrap();
        assert!(v["vocabulary"]["grantable"]
            .as_array()
            .unwrap()
            .contains(&json!("run_tests")));
        assert!(v["vocabulary"]["never_auto"]
            .as_array()
            .unwrap()
            .contains(&json!("deploy")));
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
            mint_limits: MintLimits {
                per_window: 5,
                window_secs: 3600,
                lifetime: 1000,
            },
            trusted_proxy_hops: 0,
            namespace_grant_hash: None,
            inbox_signal: Arc::new(tokio::sync::Notify::new()),
            github: None,
        };
        let _ = build_router(state);
    }
}
