//! Prometheus metrics and the Alertmanager relay for the Nests host (Phase B6, R-73).
//!
//! - [`render`] is the `/metrics` page nestd serves on a loopback address (`[control]
//!   metrics_bind`): counts of Nests by state, the lock, admission headroom, this month's model
//!   tokens, open reservations, volume allocation, the thin pool's fill and the last backup.
//!   No Nest id, account, address or content appears in it.
//! - [`relay`] receives Alertmanager's webhook on loopback and sends one Pigeonpost message per
//!   notification to the operator from a dedicated alerts mailbox. Alert names and summaries only.

use crate::control::Ctx;
use crate::util::{now, period_of};
use std::collections::BTreeMap;
use std::fmt::Write;

/// The thin pool's data fill, percent, from `lvs` (production hosts only).
fn thin_pool_percent(ctx: &Ctx) -> Option<f64> {
    if ctx.cfg.storage.backend != "lvm-luks" {
        return None;
    }
    let pool = format!("{}/{}", ctx.cfg.storage.vg, ctx.cfg.storage.pool);
    crate::storage::run("lvs", &["--noheadings", "-o", "data_percent", &pool])
        .ok()
        .and_then(|o| o.trim().replace(',', ".").parse().ok())
}

pub fn render(ctx: &Ctx) -> String {
    let mut out = String::new();
    let nests = ctx.db.nests(false).unwrap_or_default();
    let mut by_state: BTreeMap<String, u64> = BTreeMap::new();
    for state in [
        "running", "paused", "stopped", "failed", "creating", "deleting",
    ] {
        by_state.insert(state.into(), 0);
    }
    let period = period_of(now());
    let (mut tin, mut tout, mut pin, mut pout, mut alloc) = (0i64, 0i64, 0i64, 0i64, 0i64);
    for n in &nests {
        *by_state.entry(n.state.clone()).or_default() += 1;
        alloc += n.quota_gib;
        if let Ok(u) = ctx.db.usage(&n.id, &period) {
            tin += u.input_used;
            tout += u.output_used;
            pin += u.platform_input;
            pout += u.platform_output;
        }
    }
    let _ = writeln!(out, "# HELP nests_total Nests on this host by state (deleted excluded).\n# TYPE nests_total gauge");
    for (state, n) in &by_state {
        let _ = writeln!(out, "nests_total{{state=\"{state}\"}} {n}");
    }
    let _ = writeln!(out, "# HELP nests_locked 1 while the host key has not been released by the postbox.\n# TYPE nests_locked gauge\nnests_locked {}", u8::from(ctx.locked()));
    let _ = writeln!(
        out,
        "# TYPE nests_max_running gauge\nnests_max_running {}",
        ctx.cfg.control.max_running
    );
    let _ = writeln!(out, "# HELP nests_volume_allocated_gib Sum of Nest volume quotas.\n# TYPE nests_volume_allocated_gib gauge\nnests_volume_allocated_gib {alloc}");
    let _ = writeln!(
        out,
        "# TYPE nests_volume_max_gib gauge\nnests_volume_max_gib {}",
        ctx.cfg.storage.max_total_gib
    );
    let _ = writeln!(out, "# HELP nests_model_tokens_month Model tokens settled this month, all Nests.\n# TYPE nests_model_tokens_month gauge");
    let _ = writeln!(out, "nests_model_tokens_month{{direction=\"input\"}} {tin}\nnests_model_tokens_month{{direction=\"output\"}} {tout}");
    let _ = writeln!(out, "# HELP nests_platform_error_tokens_month Tokens charged to the platform (unknown upstream outcomes).\n# TYPE nests_platform_error_tokens_month gauge");
    let _ = writeln!(out, "nests_platform_error_tokens_month{{direction=\"input\"}} {pin}\nnests_platform_error_tokens_month{{direction=\"output\"}} {pout}");
    if let Some(p) = thin_pool_percent(ctx) {
        let _ = writeln!(out, "# HELP nests_thin_pool_data_percent Fill of the Nest volume thin pool.\n# TYPE nests_thin_pool_data_percent gauge\nnests_thin_pool_data_percent {p}");
    }
    if let Some(marker) = &ctx.cfg.control.backup_marker {
        if let Some(t) = std::fs::read_to_string(marker)
            .ok()
            .and_then(|t| t.trim().parse::<i64>().ok())
        {
            let _ = writeln!(out, "# HELP nests_backup_last_success_seconds Unix time of the last successful nightly backup.\n# TYPE nests_backup_last_success_seconds gauge\nnests_backup_last_success_seconds {t}");
        }
    }
    let _ = writeln!(out, "# TYPE nests_up gauge\nnests_up 1");
    out
}

/// One Alertmanager webhook body as a short message: status, alert names and summaries.
pub fn alert_text(body: &serde_json::Value) -> Option<String> {
    let alerts = body["alerts"].as_array()?;
    if alerts.is_empty() {
        return None;
    }
    let mut out = format!(
        "Nests host alert ({}):",
        body["status"].as_str().unwrap_or("firing")
    );
    for a in alerts.iter().take(20) {
        let name = a["labels"]["alertname"].as_str().unwrap_or("alert");
        let status = a["status"].as_str().unwrap_or("");
        let summary = a["annotations"]["summary"].as_str().unwrap_or("");
        let clean = |s: &str| {
            s.chars()
                .filter(|c| !c.is_control())
                .take(300)
                .collect::<String>()
        };
        let _ = write!(
            out,
            "\n- [{}] {}: {}",
            clean(status),
            clean(name),
            clean(summary)
        );
    }
    Some(out)
}

/// `pigeonpost-nestd alert-relay`: receive Alertmanager's webhook on loopback, send it on as a
/// Pigeonpost message from the alerts mailbox.
pub async fn relay(cfg: crate::config::AlertsConfig) -> Result<(), String> {
    use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
    let token = crate::config::read_secret_file(&cfg.token_file)?;
    let cfg = std::sync::Arc::new((cfg, token));
    async fn hook(
        State(cfg): State<std::sync::Arc<(crate::config::AlertsConfig, String)>>,
        Json(body): Json<serde_json::Value>,
    ) -> StatusCode {
        let Some(text) = alert_text(&body) else {
            return StatusCode::OK;
        };
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
        {
            Ok(c) => c,
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR,
        };
        let r = client
            .post(format!(
                "{}/v1/send",
                cfg.0.postbox_url.trim_end_matches('/')
            ))
            .bearer_auth(&cfg.1)
            .json(&serde_json::json!({ "to": cfg.0.to, "body": text }))
            .send()
            .await;
        match r {
            Ok(r) if r.status().is_success() => StatusCode::OK,
            // Alertmanager retries on a 5xx.
            _ => StatusCode::BAD_GATEWAY,
        }
    }
    let bind = cfg.0.bind.clone();
    if !bind.starts_with("127.") && !bind.starts_with("[::1]") {
        return Err("alert-relay binds to loopback only".into());
    }
    let app = Router::new().route("/alert", post(hook)).with_state(cfg);
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .map_err(|e| format!("bind {bind}: {e}"))?;
    tracing::info!(%bind, "alert relay listening");
    axum::serve(listener, app).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alerts_become_short_messages_without_control_characters() {
        let body = serde_json::json!({"status":"firing","alerts":[
            {"status":"firing","labels":{"alertname":"NestsLocked"},"annotations":{"summary":"locked for 5m\u{7}"}}
        ]});
        let t = alert_text(&body).unwrap();
        assert!(t.contains("NestsLocked") && t.contains("locked for 5m") && !t.contains('\u{7}'));
        assert!(alert_text(&serde_json::json!({"alerts":[]})).is_none());
    }
}
