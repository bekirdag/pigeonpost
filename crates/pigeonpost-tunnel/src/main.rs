//! `pigeonpost-tunnel` — the agent SSH tunnel broker (see the library docs and `docs/agent-ssh.md`).
//!
//! Environment:
//!
//! * `TUNNEL_BIND` — listen address (default `127.0.0.1:8781`; it belongs behind the TLS proxy), or
//!   `unix:/run/pigeonpost-tunnel/broker.sock` (Phase 9, recommended): a socket created `0660` with
//!   group `TUNNEL_SOCKET_GID` (the proxy's group), the only listener whose `X-Real-IP` is trusted;
//! * `TUNNEL_POSTBOX_INTERNAL_URL` — the postbox's internal API;
//! * `TUNNEL_SERVICE_TOKEN` — this service's `svc_…` token;
//! * optional limits: `TUNNEL_MAX_SESSIONS`, `TUNNEL_IDLE_SECS`, `TUNNEL_CHECK_SECS`,
//!   `TUNNEL_FAIL_CLOSED_SECS`, `TUNNEL_PER_IP_PER_MINUTE`, and `TUNNEL_TRUST_PROXY=1` to take
//!   the client address from `X-Real-IP` set by a loopback proxy.

use pigeonpost_tunnel::{Broker, Config};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cfg = match Config::from_env() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("pigeonpost-tunnel: {e}");
            std::process::exit(2);
        }
    };
    let bind = std::env::var("TUNNEL_BIND").unwrap_or_else(|_| "127.0.0.1:8781".into());
    let broker = Broker::new(cfg);
    broker.start_heartbeat();
    let app = broker
        .router()
        .into_make_service_with_connect_info::<pigeonpost_tunnel::Peer>();
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let shutdown = async {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("signal");
        tokio::select! { _ = shutdown => {}, _ = term.recv() => {} }
    };
    let served = match bind.strip_prefix("unix:") {
        #[cfg(unix)]
        Some(path) => {
            let listener = match unix_listener(path) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("pigeonpost-tunnel: cannot bind {bind}: {e}");
                    std::process::exit(1);
                }
            };
            tracing::info!(%bind, "tunnel broker listening (proxy socket)");
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown)
                .await
        }
        #[cfg(not(unix))]
        Some(_) => {
            eprintln!("pigeonpost-tunnel: unix sockets need a unix host");
            std::process::exit(2);
        }
        None => {
            let listener = match tokio::net::TcpListener::bind(&bind).await {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("pigeonpost-tunnel: cannot bind {bind}: {e}");
                    std::process::exit(1);
                }
            };
            if std::env::var("TUNNEL_TRUST_PROXY").as_deref() == Ok("1") {
                tracing::warn!(
                    "TUNNEL_TRUST_PROXY ignored on a TCP listener; bind unix:… behind the proxy"
                );
            }
            tracing::info!(%bind, "tunnel broker listening");
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown)
                .await
        }
    };
    if let Err(e) = served {
        eprintln!("pigeonpost-tunnel: {e}");
        std::process::exit(1);
    }
}

/// The proxy socket: stale file removed, then `0660` and the proxy's group, so only the proxy (and
/// root) can connect and claim a client address.
#[cfg(unix)]
fn unix_listener(path: &str) -> std::io::Result<tokio::net::UnixListener> {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = tokio::net::UnixListener::bind(path)?;
    if let Some(gid) = std::env::var("TUNNEL_SOCKET_GID")
        .ok()
        .and_then(|g| g.trim().parse::<u32>().ok())
    {
        std::os::unix::fs::chown(path, None, Some(gid))?;
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
    Ok(listener)
}
