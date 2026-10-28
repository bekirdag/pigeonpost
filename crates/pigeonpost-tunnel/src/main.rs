//! `pigeonpost-tunnel` — the agent SSH tunnel broker (see the library docs and `docs/agent-ssh.md`).
//!
//! Environment:
//!
//! * `TUNNEL_BIND` — listen address (default `127.0.0.1:8781`; it belongs behind the TLS proxy);
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
    let listener = match tokio::net::TcpListener::bind(&bind).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("pigeonpost-tunnel: cannot bind {bind}: {e}");
            std::process::exit(1);
        }
    };
    tracing::info!(%bind, "tunnel broker listening");
    let app = broker
        .router()
        .into_make_service_with_connect_info::<std::net::SocketAddr>();
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let shutdown = async {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("signal");
        tokio::select! { _ = shutdown => {}, _ = term.recv() => {} }
    };
    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
    {
        eprintln!("pigeonpost-tunnel: {e}");
        std::process::exit(1);
    }
}
