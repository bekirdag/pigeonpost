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
    let _ = writeln!(out, "# HELP nests_postbox_last_unlock_seconds Unix time of the postbox's last successful unlock call (every minute; 0 = none since nestd started).\n# TYPE nests_postbox_last_unlock_seconds gauge\nnests_postbox_last_unlock_seconds {}", ctx.postbox_last_unlock.load(std::sync::atomic::Ordering::Relaxed));
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

/// How one attempt to send through the postbox ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostboxSend {
    Delivered,
    /// Connection error, timeout or 5xx: the postbox itself may be down.
    Unavailable,
    /// Any other refusal (4xx): the postbox answered, so it is up.
    Refused,
}

/// Whether a notification also goes out through Postmark: always for `Postbox*` alerts (a postbox outage
/// would swallow them, and a later recovery would deliver them late), and whenever the postbox
/// could not be reached.
pub fn needs_email(body: &serde_json::Value, postbox: PostboxSend) -> bool {
    let about_postbox = body["alerts"].as_array().is_some_and(|alerts| {
        alerts.iter().any(|a| {
            a["labels"]["alertname"]
                .as_str()
                .is_some_and(|n| n.starts_with("Postbox"))
        })
    });
    about_postbox || postbox == PostboxSend::Unavailable
}

/// The relay's configuration with its secrets loaded.
pub struct Relay {
    cfg: crate::config::AlertsConfig,
    token: String,
    postmark_token: Option<String>,
    client: reqwest::Client,
}

impl Relay {
    /// Read the alerts mailbox token and, when `[alerts.postmark]` is set, the Postmark token.
    pub fn load(cfg: crate::config::AlertsConfig) -> Result<Self, String> {
        let token = crate::config::read_secret_file(&cfg.token_file)?;
        let postmark_token = match &cfg.postmark {
            Some(e) => Some(crate::config::read_secret_file(&e.token_file)?),
            None => None,
        };
        Self::with_tokens(cfg, token, postmark_token)
    }

    pub fn with_tokens(
        cfg: crate::config::AlertsConfig,
        token: String,
        postmark_token: Option<String>,
    ) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Relay {
            cfg,
            token,
            postmark_token,
            client,
        })
    }

    async fn send_postbox(&self, text: &str) -> PostboxSend {
        let r = self
            .client
            .post(format!(
                "{}/v1/send",
                self.cfg.postbox_url.trim_end_matches('/')
            ))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "to": self.cfg.to, "body": text }))
            .send()
            .await;
        match r {
            Ok(r) if r.status().is_success() => PostboxSend::Delivered,
            Ok(r) if r.status().is_server_error() => {
                tracing::warn!(status = %r.status(), "alert relay: postbox send failed");
                PostboxSend::Unavailable
            }
            Ok(r) => {
                tracing::warn!(status = %r.status(), "alert relay: postbox refused the send");
                PostboxSend::Refused
            }
            Err(e) => {
                tracing::warn!(
                    timeout = e.is_timeout(),
                    connect = e.is_connect(),
                    "alert relay: postbox unreachable"
                );
                PostboxSend::Unavailable
            }
        }
    }

    /// Send one message through Postmark. `false` when not configured or not accepted.
    async fn send_email(&self, body: &serde_json::Value, text: &str) -> bool {
        let (Some(e), Some(token)) = (&self.cfg.postmark, &self.postmark_token) else {
            return false;
        };
        let mut names: Vec<&str> = body["alerts"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x["labels"]["alertname"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        names.sort_unstable();
        names.dedup();
        let subject: String = format!(
            "Nests host alert ({}): {}",
            body["status"].as_str().unwrap_or("firing"),
            names.join(", ")
        )
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect();
        let r = self
            .client
            .post(&e.api_url)
            .header("Accept", "application/json")
            .header("X-Postmark-Server-Token", token)
            .json(&serde_json::json!({
                "From": e.from,
                "To": e.to,
                "Subject": subject,
                "TextBody": text,
                "MessageStream": e.message_stream,
            }))
            .send()
            .await;
        match r {
            Ok(r) if r.status().is_success() => true,
            Ok(r) => {
                tracing::warn!(status = %r.status(), "alert relay: Postmark fallback refused");
                false
            }
            Err(e) => {
                tracing::warn!(
                    timeout = e.is_timeout(),
                    connect = e.is_connect(),
                    "alert relay: Postmark fallback unreachable"
                );
                false
            }
        }
    }

    /// Relay one webhook body. 200 once the operator was reached by either channel; 502 otherwise,
    /// so Alertmanager retries.
    pub async fn deliver(&self, body: &serde_json::Value) -> axum::http::StatusCode {
        use axum::http::StatusCode;
        let Some(text) = alert_text(body) else {
            return StatusCode::OK;
        };
        let postbox = self.send_postbox(&text).await;
        let emailed = needs_email(body, postbox) && self.send_email(body, &text).await;
        if postbox == PostboxSend::Delivered || emailed {
            StatusCode::OK
        } else {
            StatusCode::BAD_GATEWAY
        }
    }
}

/// `pigeonpost-nestd alert-relay`: receive Alertmanager's webhook on loopback, send it on as a
/// Pigeonpost message from the alerts mailbox, with an optional Postmark fallback.
pub async fn relay(cfg: crate::config::AlertsConfig) -> Result<(), String> {
    use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
    let bind = cfg.bind.clone();
    if !bind.starts_with("127.") && !bind.starts_with("[::1]") {
        return Err("alert-relay binds to loopback only".into());
    }
    let relay = std::sync::Arc::new(Relay::load(cfg)?);
    async fn hook(
        State(relay): State<std::sync::Arc<Relay>>,
        Json(body): Json<serde_json::Value>,
    ) -> StatusCode {
        relay.deliver(&body).await
    }
    let app = Router::new().route("/alert", post(hook)).with_state(relay);
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

    use crate::config::{AlertsConfig, PostmarkFallbackConfig};
    use axum::http::{HeaderMap, StatusCode};
    use std::sync::{Arc, Mutex};

    async fn mock(status: u16) -> (String, Arc<Mutex<Vec<(HeaderMap, serde_json::Value)>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let rec_clone = Arc::clone(&recorded);

        let router = axum::Router::new().fallback(
            move |headers: HeaderMap, axum::Json(body): axum::Json<serde_json::Value>| {
                let rec = Arc::clone(&rec_clone);
                async move {
                    rec.lock().unwrap().push((headers, body));
                    StatusCode::from_u16(status).unwrap()
                }
            },
        );

        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        (format!("http://{}", addr), recorded)
    }

    fn cfg(postbox_url: String, postmark_url: Option<String>) -> AlertsConfig {
        let postmark = postmark_url.map(|url| PostmarkFallbackConfig {
            token_file: std::path::PathBuf::from("unused"),
            from: "alerts@example.test".to_string(),
            to: "ops@example.test".to_string(),
            message_stream: "outbound".to_string(),
            api_url: url,
        });

        AlertsConfig {
            bind: "127.0.0.1:0".to_string(),
            postbox_url,
            token_file: std::path::PathBuf::from("unused"),
            to: "/bekir".to_string(),
            postmark,
        }
    }

    fn body(name: &str) -> serde_json::Value {
        serde_json::json!({
            "status": "firing",
            "alerts": [
                {
                    "status": "firing",
                    "labels": { "alertname": name },
                    "annotations": { "summary": "s" }
                }
            ]
        })
    }

    #[test]
    fn email_is_needed_for_postbox_alerts_and_unreachable_postboxes() {
        assert!(needs_email(&body("PostboxSilent"), PostboxSend::Delivered));
        assert!(!needs_email(&body("NestsLocked"), PostboxSend::Delivered));
        assert!(needs_email(&body("NestsLocked"), PostboxSend::Unavailable));
        assert!(!needs_email(&body("NestsLocked"), PostboxSend::Refused));
    }

    #[tokio::test]
    async fn relay_falls_back_to_email_when_the_postbox_fails() {
        // Case 1: postbox fails, Postmark succeeds for NestsLocked
        let (postbox_url, _) = mock(503).await;
        let (postmark_url, email_rec) = mock(200).await;
        let config = cfg(postbox_url, Some(postmark_url));
        let relay = Relay::with_tokens(
            config,
            "pbx-token".to_string(),
            Some("pm-token".to_string()),
        )
        .unwrap();

        let res = relay.deliver(&body("NestsLocked")).await;
        assert_eq!(res, StatusCode::OK);

        {
            let recs = email_rec.lock().unwrap();
            assert_eq!(recs.len(), 1);
            let (headers, body) = &recs[0];
            assert_eq!(
                headers
                    .get("x-postmark-server-token")
                    .and_then(|v| v.to_str().ok()),
                Some("pm-token")
            );
            assert_eq!(body["MessageStream"], "outbound");
            assert_eq!(body["To"], "ops@example.test");
            assert!(body["Subject"]
                .as_str()
                .unwrap_or("")
                .contains("NestsLocked"));
        }

        // Case 2: postbox succeeds, no Postmark message for NestsLocked; Postmark message for PostboxPublicDown
        let (postbox_url2, _) = mock(200).await;
        let (email_url2, email_rec2) = mock(200).await;
        let config2 = cfg(postbox_url2, Some(email_url2));
        let relay2 = Relay::with_tokens(
            config2,
            "pbx-token".to_string(),
            Some("pm-token".to_string()),
        )
        .unwrap();

        let res_nests = relay2.deliver(&body("NestsLocked")).await;
        assert_eq!(res_nests, StatusCode::OK);
        {
            let recs = email_rec2.lock().unwrap();
            assert_eq!(recs.len(), 0);
        }

        let res_postbox = relay2.deliver(&body("PostboxPublicDown")).await;
        assert_eq!(res_postbox, StatusCode::OK);
        {
            let recs = email_rec2.lock().unwrap();
            assert_eq!(recs.len(), 1);
        }

        // Case 3: postbox unreachable, Postmark fails -> BAD_GATEWAY
        let (email_url3, _) = mock(500).await;
        let config3 = cfg("http://127.0.0.1:9".to_string(), Some(email_url3));
        let relay3 = Relay::with_tokens(
            config3,
            "pbx-token".to_string(),
            Some("pm-token".to_string()),
        )
        .unwrap();

        let res_unreach = relay3.deliver(&body("NestsLocked")).await;
        assert_eq!(res_unreach, StatusCode::BAD_GATEWAY);

        // Case 4: postbox fails, no Postmark fallback configured -> BAD_GATEWAY
        let (postbox_url4, _) = mock(503).await;
        let mut config4 = cfg(postbox_url4, None);
        config4.postmark = None;
        let relay4 = Relay::with_tokens(config4, "pbx-token".to_string(), None).unwrap();

        let res_no_email = relay4.deliver(&body("NestsLocked")).await;
        assert_eq!(res_no_email, StatusCode::BAD_GATEWAY);
    }
}
