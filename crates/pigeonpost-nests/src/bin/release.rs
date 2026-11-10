//! `pigeonpost-nest-release` — the build and analysis planes (marketplace M1) on nests-1.
//!
//! ```text
//! pigeonpost-nest-release build     --config build.toml --src DIR --work DIR
//! pigeonpost-nest-release scan      --src DIR
//! pigeonpost-nest-release ai-review --route review.toml --src DIR
//! ```
//!
//! Each prints one JSON document on stdout, shaped as the postbox's internal routes take it:
//! `build` → `POST /internal/v1/marketplace/builds`; `scan` and `ai-review` →
//! `POST /internal/v1/marketplace/reviews/{review}/stages`. The behaviour test needs a running
//! sandbox and lives in `tests/marketplace_m1.rs` (`live_behaviour_test_on_gvisor`).
//!
//! Run as an unprivileged user: the build refuses rootful podman.

use pigeonpost_nests::analysis::{self, StageReport};
use pigeonpost_nests::release::{BuildConfig, Builder};
use pigeonpost_nests::sandbox::HostPodman;
use serde_json::json;
use std::path::PathBuf;

fn arg(args: &[String], name: &str) -> Option<PathBuf> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
}

fn usage() -> ! {
    eprintln!(
        "usage: pigeonpost-nest-release build --config FILE --src DIR --work DIR\n       \
         pigeonpost-nest-release scan --src DIR\n       \
         pigeonpost-nest-release ai-review --route FILE --src DIR"
    );
    std::process::exit(2)
}

fn stage_body(r: &StageReport) -> serde_json::Value {
    json!({
        "stage": r.stage,
        "decision": r.decision,
        "reason": r.reason,
        "evidence": r.evidence_json(),
        "evidence_digest": r.evidence_digest(),
    })
}

fn need(args: &[String], name: &str) -> PathBuf {
    arg(args, name).unwrap_or_else(|| usage())
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let result: Result<serde_json::Value, String> = match args.get(1).map(String::as_str) {
        Some("build") => (|| {
            let cfg: BuildConfig = toml::from_str(
                &std::fs::read_to_string(need(&args, "--config")).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let builder = Builder {
                cfg,
                podman: HostPodman,
            };
            let record = builder.build(&need(&args, "--src"), &need(&args, "--work"))?;
            serde_json::to_value(record).map_err(|e| e.to_string())
        })(),
        Some("scan") => analysis::deterministic_scan(&need(&args, "--src")).map(|r| stage_body(&r)),
        Some("ai-review") => {
            let route: Result<analysis::ReviewRouteConfig, String> =
                std::fs::read_to_string(need(&args, "--route"))
                    .map_err(|e| e.to_string())
                    .and_then(|t| toml::from_str(&t).map_err(|e| e.to_string()));
            match route {
                Err(e) => Err(e),
                Ok(route) => match route
                    .check()
                    .and_then(|_| pigeonpost_nests::config::read_secret_file(&route.key_file))
                {
                    Err(e) => Err(e),
                    Ok(key) => {
                        let model = analysis::HttpReviewModel {
                            url: route.url.clone(),
                            key,
                            client: reqwest::Client::builder()
                                .timeout(std::time::Duration::from_secs(300))
                                .redirect(reqwest::redirect::Policy::none())
                                .build()
                                .expect("http client"),
                        };
                        analysis::ai_review(&model, &route, &need(&args, "--src"))
                            .await
                            .map(|r| stage_body(&r))
                    }
                },
            }
        }
        _ => usage(),
    };
    match result {
        Ok(v) => println!("{v}"),
        Err(e) => {
            eprintln!("pigeonpost-nest-release: {e}");
            std::process::exit(1);
        }
    }
}
