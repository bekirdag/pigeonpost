//! `pigeonpost-nest-buildd` — the remote build and analysis plane (hardening, W1.3).
//!
//! ```text
//! pigeonpost-nest-buildd serve --config /etc/pigeonpost-buildd/buildd.toml
//! pigeonpost-nest-buildd check --config FILE     (validate the config, the pinned Semgrep
//!                                                 ruleset and the scanner databases' ages)
//! pigeonpost-nest-buildd hash-token              (reads a token on stdin, prints its SHA-256)
//! pigeonpost-nest-buildd public-key              (reads an attestation seed on stdin, prints
//!                                                 the public key for nestd and its key id)
//! ```
//!
//! Runs as an unprivileged user with rootless podman (gVisor `runsc`); refuses rootful podman.
//! See `deploy/nests/buildd/README.md`.

use pigeonpost_nests::buildd::{self, Buildd, BuilddConfig};
use pigeonpost_nests::config::read_secret_file;
use pigeonpost_nests::sandbox::HostPodman;
use std::path::PathBuf;
use std::sync::Arc;

fn usage() -> ! {
    eprintln!(
        "usage: pigeonpost-nest-buildd serve|check --config FILE\n       \
         pigeonpost-nest-buildd hash-token < token\n       \
         pigeonpost-nest-buildd public-key < attestation-seed"
    );
    std::process::exit(2)
}

fn config(args: &[String]) -> Result<BuilddConfig, String> {
    let path = args
        .iter()
        .position(|a| a == "--config")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| usage());
    BuilddConfig::load(&path)
}

async fn serve(cfg: BuilddConfig) -> Result<(), String> {
    let submit = read_secret_file(&cfg.submit_token_sha256_file)?;
    let artifact = read_secret_file(&cfg.artifact_token_sha256_file)?;
    let review = match &cfg.ai_review_route {
        None => None,
        Some(p) => {
            let route: pigeonpost_nests::analysis::ReviewRouteConfig = toml::from_str(
                &std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?,
            )
            .map_err(|e| format!("{}: {e}", p.display()))?;
            route.check()?;
            let key = read_secret_file(&route.key_file)?;
            Some((route, key))
        }
    };
    for dir in [&cfg.work_root, &cfg.artifact_root] {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    // Deploy keys (M2b sources), from the postbox's credential route with its scoped token.
    let credentials = match &cfg.postbox {
        None => buildd::Credentials::None,
        Some(p) => buildd::Credentials::Http {
            url: p.url.trim_end_matches('/').to_string(),
            token: zeroize::Zeroizing::new(read_secret_file(&p.credential_token_file)?),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| e.to_string())?,
        },
    };
    if cfg.scanners.is_none() {
        tracing::warn!("no [scanners]: builds get the built-in rules only (not for production)");
    }
    let bind = cfg.bind.clone();
    let b = Arc::new(
        Buildd::open(cfg, Arc::new(HostPodman), submit, artifact, review)?
            .with_credentials(credentials),
    );
    buildd::start_worker(b.clone());
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .map_err(|e| format!("bind {bind}: {e}"))?;
    tracing::info!(%bind, "build plane listening");
    axum::serve(listener, buildd::router(b))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|e| e.to_string())
}

/// `check`: the configuration, and what the security scan will find on this host.
fn check(cfg: &BuilddConfig) -> Result<(), String> {
    let Some(sc) = &cfg.scanners else {
        println!("ok (no [scanners]: only the built-in rules run; not for production)");
        return Ok(());
    };
    let rules = pigeonpost_nests::scanners::file_sha256(&sc.semgrep_rules)?;
    if !rules.eq_ignore_ascii_case(&sc.semgrep_rules_sha256) {
        return Err(format!(
            "{}: sha256 {rules}, not the pinned {}",
            sc.semgrep_rules.display(),
            sc.semgrep_rules_sha256
        ));
    }
    let manifest: Option<serde_json::Value> =
        std::fs::read_to_string(sc.db_root.join("manifest.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok());
    let problems = pigeonpost_nests::scanners::db_findings(
        manifest.as_ref(),
        sc.max_db_age_hours,
        pigeonpost_nests::util::now(),
    );
    for p in &problems {
        println!("{}: {}", p.rule, p.message);
    }
    if cfg.behaviour.is_none() {
        println!("note: no [behaviour]: egress and resource use are not observed");
    }
    if problems.is_empty() {
        println!("ok");
        Ok(())
    } else {
        Err("every build would fail its scan until the databases are fresh".into())
    }
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
    let result = match args.first().map(String::as_str) {
        Some("serve") => match config(&args) {
            Ok(cfg) => serve(cfg).await,
            Err(e) => Err(e),
        },
        Some("check") => config(&args).and_then(|cfg| check(&cfg)),
        Some("hash-token") => {
            let mut s = String::new();
            let _ = std::io::stdin().read_line(&mut s);
            println!(
                "{}",
                pigeonpost_nests::util::sha256_hex(s.trim().as_bytes())
            );
            Ok(())
        }
        Some("public-key") => {
            let mut s = String::new();
            let _ = std::io::stdin().read_line(&mut s);
            let key = pigeonpost_nests::attest::signing_key_from_hex(s.trim());
            s.clear();
            match key {
                Some(k) => {
                    let public = k.verifying_key();
                    println!(
                        "{} {}",
                        pigeonpost_nests::util::hex(public.as_bytes()),
                        pigeonpost_nests::attest::key_id(&public)
                    );
                    Ok(())
                }
                None => Err("stdin must hold a 64-hex-digit Ed25519 seed".into()),
            }
        }
        _ => usage(),
    };
    if let Err(e) = result {
        tracing::error!(error = %e, "exiting");
        std::process::exit(1);
    }
}
