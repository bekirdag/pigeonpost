//! `pigeonpost-nestd` — the Nests host services.
//!
//! ```text
//! pigeonpost-nestd control       --config /etc/nests/nestd.toml
//! pigeonpost-nestd broker        --config /etc/nests/nestd.toml
//! pigeonpost-nestd mock-upstream --bind 127.0.0.1:8789 --key-file /etc/nests/upstream.key
//! pigeonpost-nestd hash-token    (reads a token on stdin, prints its SHA-256)
//! ```

use pigeonpost_nests::{broker, config, control, db, mock_upstream, sandbox, storage};
use std::path::PathBuf;
use std::sync::Arc;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn usage() -> ! {
    eprintln!(
        "usage: pigeonpost-nestd control|broker --config FILE\n       \
         pigeonpost-nestd mock-upstream --bind ADDR --key-file FILE\n       \
         pigeonpost-nestd hash-token < token"
    );
    std::process::exit(2)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().cloned() else {
        usage()
    };
    let result = match cmd.as_str() {
        "control" => run_control(&args).await,
        "broker" => run_broker(&args).await,
        "mock-upstream" => run_mock(&args).await,
        "hash-token" => {
            let mut s = String::new();
            let _ = std::io::stdin().read_line(&mut s);
            println!(
                "{}",
                pigeonpost_nests::util::sha256_hex(s.trim().as_bytes())
            );
            Ok(())
        }
        _ => usage(),
    };
    if let Err(e) = result {
        tracing::error!(error = %e, "exiting");
        std::process::exit(1);
    }
}

fn load(args: &[String]) -> Result<config::Config, String> {
    let path = arg(args, "--config")
        .map(PathBuf::from)
        .unwrap_or_else(|| usage());
    config::Config::load(&path)
}

async fn shutdown() {
    #[cfg(unix)]
    {
        let mut term =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        tokio::select! {
            _ = term.recv() => {},
            _ = tokio::signal::ctrl_c() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn run_control(args: &[String]) -> Result<(), String> {
    let cfg = load(args)?;
    if cfg.storage.backend != "lvm-luks" || cfg.sandbox.runtime != "gvisor" {
        tracing::warn!("running with a development backend: no quota/encryption or no sandbox");
    }
    let token_hash = config::read_secret_file(&cfg.control.token_sha256_file)?;
    if token_hash.len() != 64 || !token_hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("control token file must hold a SHA-256 hex digest".into());
    }
    let db = db::Db::open(&cfg.db)?;
    let sandbox: Arc<dyn sandbox::Sandbox> = Arc::from(sandbox::from_config(&cfg.sandbox)?);
    let volumes: Arc<dyn storage::Volumes> = Arc::from(storage::from_config(&cfg.storage)?);
    let host = std::fs::read_to_string("/etc/hostname")
        .map(|h| h.trim().to_string())
        .unwrap_or_else(|_| "nests-host".into());
    let bind = cfg.control.bind.clone();
    let every = cfg.control.reconcile_secs.max(10);
    let ctx = control::Ctx::new(cfg, db, sandbox, volumes, token_hash.to_lowercase(), host);

    // Reconcile before serving: after a reboot every sandbox is gone and every volume closed.
    if let Err(e) = control::reconcile(&ctx).await {
        tracing::error!(error = %e, "initial reconcile failed");
    }
    let loop_ctx = ctx.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(every));
        tick.tick().await;
        loop {
            tick.tick().await;
            if let Err(e) = control::reconcile(&loop_ctx).await {
                tracing::warn!(error = %e, "reconcile failed");
            }
        }
    });

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .map_err(|e| format!("bind {bind}: {e}"))?;
    tracing::info!(%bind, "nest control listening");
    // Nests keep running across a control-service restart; reconcile adopts them again.
    axum::serve(listener, control::router(ctx))
        .with_graceful_shutdown(shutdown())
        .await
        .map_err(|e| e.to_string())
}

async fn run_broker(args: &[String]) -> Result<(), String> {
    let cfg = load(args)?;
    let key = config::read_secret_file(&cfg.broker.upstream_key_file)?;
    let db = db::Db::open(&cfg.db)?;
    let bind = cfg.broker.bind.clone();
    let b = broker::Broker::new(cfg.broker, db, key)?;
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .map_err(|e| format!("bind {bind}: {e}"))?;
    tracing::info!(%bind, "model broker listening");
    axum::serve(listener, broker::router(b))
        .with_graceful_shutdown(shutdown())
        .await
        .map_err(|e| e.to_string())
}

async fn run_mock(args: &[String]) -> Result<(), String> {
    let bind = arg(args, "--bind").unwrap_or_else(|| usage());
    if !bind.starts_with("127.0.0.1:") {
        return Err("the mock upstream binds loopback only".into());
    }
    let key_file = arg(args, "--key-file")
        .map(PathBuf::from)
        .unwrap_or_else(|| usage());
    let key = config::read_secret_file(&key_file)?;
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .map_err(|e| format!("bind {bind}: {e}"))?;
    tracing::info!(%bind, "mock upstream listening");
    axum::serve(listener, mock_upstream::router(&key))
        .with_graceful_shutdown(shutdown())
        .await
        .map_err(|e| e.to_string())
}
