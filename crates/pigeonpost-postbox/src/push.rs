//! Apple Push Notification service, spoken directly.
//!
//! No SDK and no third party: FCM on iOS is a wrapper around this same service and needs the same
//! key, so an SDK would buy a dependency, a second privacy disclosure, and a copy of who is
//! messaging whom on somebody else's servers. The postbox already carries `jsonwebtoken` for member
//! tokens and `reqwest` for the realm's JWKS; APNs needs those two and HTTP/2.
//!
//! Everything here is configured from the environment and nothing is compiled in. With no key
//! configured [`Apns::from_env`] returns `None` and the postbox behaves exactly as it did before
//! push existed — which is the state it ships in, because only a human in the developer portal can
//! create an APNs key.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde_json::json;
use tokio::sync::Mutex;

use crate::store::{Device, Store};

/// How much of a message travels in the payload. A lock screen is a small place and APNs refuses
/// payloads over 4 KB; 180 characters is a sentence, which is all a notification is for.
const PREVIEW_CHARS: usize = 180;

/// APNs rejects a token older than an hour and throttles clients that mint one per push. Re-signing
/// every 45 minutes sits inside both rules.
const TOKEN_LIFETIME: Duration = Duration::from_secs(45 * 60);

pub struct Apns {
    key: EncodingKey,
    key_id: String,
    team_id: String,
    topic: String,
    /// Whether the message itself travels in the payload. See the plan: this ships on, because a
    /// notification that says only "a message arrived" makes people open the app to find out
    /// whether it mattered.
    preview: bool,
    http: reqwest::Client,
    bearer: Mutex<Option<(String, Instant)>>,
}

/// What one device is told. The postbox knows more than this and deliberately sends no more.
pub struct Notification {
    /// Who wrote — their handle if they have one, their address if they do not.
    pub title: String,
    /// Which of the account's mailboxes it arrived in, so a fleet's owner can tell them apart.
    pub subtitle: String,
    pub body: String,
    pub message_id: String,
    pub peer: String,
    pub unread: i64,
}

impl Apns {
    /// Read the configuration, or decide push is off. Absence is not an error: most deployments of
    /// this code have no Apple developer account and should not be nagged about it.
    pub fn from_env() -> Option<Arc<Self>> {
        let (key_id, team_id) = match (
            non_empty("PIGEONPOST_APNS_KEY_ID"),
            non_empty("PIGEONPOST_APNS_TEAM_ID"),
        ) {
            (Some(key_id), Some(team_id)) => (key_id, team_id),
            _ => {
                // Said out loud, because the alternative is what happened the first time somebody
                // asked why their phone stayed quiet: a registered device, a delivered message, and
                // nothing anywhere saying the last mile was never connected.
                tracing::info!(
                    "APNs not configured — push disabled. Set PIGEONPOST_APNS_KEY_ID, \
                     PIGEONPOST_APNS_TEAM_ID and PIGEONPOST_APNS_KEY_PATH to switch it on."
                );
                return None;
            }
        };
        let topic = non_empty("PIGEONPOST_APNS_TOPIC")
            .unwrap_or_else(|| "dev.pigeonpost.inbox".to_string());

        // The key itself, from a mounted file or straight from the environment. Never from the
        // source tree, and never logged.
        let pem = match non_empty("PIGEONPOST_APNS_KEY_PATH") {
            Some(path) => match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(e) => {
                    tracing::error!(error = %e, %path, "APNs key unreadable — push disabled");
                    return None;
                }
            },
            None => non_empty("PIGEONPOST_APNS_KEY")?.into_bytes(),
        };
        let key = match EncodingKey::from_ec_pem(&pem) {
            Ok(key) => key,
            Err(e) => {
                tracing::error!(error = %e, "APNs key is not an EC private key — push disabled");
                return None;
            }
        };

        let http = match reqwest::Client::builder()
            .http2_prior_knowledge()
            .timeout(Duration::from_secs(20))
            .build()
        {
            Ok(client) => client,
            Err(e) => {
                tracing::error!(error = %e, "APNs HTTP client — push disabled");
                return None;
            }
        };

        let preview = std::env::var("PIGEONPOST_APNS_PREVIEW")
            .map(|v| !matches!(v.trim(), "0" | "false" | "no"))
            .unwrap_or(true);

        tracing::info!(%topic, preview, "APNs configured");
        Some(Arc::new(Self {
            key,
            key_id,
            team_id,
            topic,
            preview,
            http,
            bearer: Mutex::new(None),
        }))
    }

    /// The provider token, minted at most every 45 minutes. Held rather than re-signed per push:
    /// APNs treats a client that mints one per notification as misbehaving.
    async fn bearer(&self) -> Result<String, jsonwebtoken::errors::Error> {
        let mut held = self.bearer.lock().await;
        if let Some((token, minted)) = held.as_ref() {
            if minted.elapsed() < TOKEN_LIFETIME {
                return Ok(token.clone());
            }
        }
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(self.key_id.clone());
        let issued = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let token = encode(
            &header,
            &json!({ "iss": self.team_id, "iat": issued }),
            &self.key,
        )?;
        *held = Some((token.clone(), Instant::now()));
        Ok(token)
    }

    fn payload(&self, note: &Notification) -> serde_json::Value {
        let alert = if self.preview {
            json!({ "title": note.title, "subtitle": note.subtitle, "body": note.body })
        } else {
            // Metadata only. Says who, not what.
            json!({ "title": note.title, "subtitle": note.subtitle, "body": "New message" })
        };
        json!({
            "aps": {
                "alert": alert,
                "sound": "default",
                "badge": note.unread,
                // Groups a peer's notifications together on the lock screen the way a conversation
                // is grouped in the app.
                "thread-id": note.peer,
                "interruption-level": "active",
            },
            // What the app needs to open the right conversation when the notification is tapped.
            "peer": note.peer,
            "mailbox": note.subtitle,
            "message_id": note.message_id,
        })
    }

    /// Deliver to one device. Returns `true` when the token is dead and its row should go.
    pub async fn deliver(&self, device: &Device, note: &Notification) -> bool {
        let bearer = match self.bearer().await {
            Ok(token) => token,
            Err(e) => {
                tracing::error!(error = %e, "APNs token could not be signed");
                return false;
            }
        };
        // Which Apple, decided per device. A token minted by a TestFlight or App Store build is
        // production; one from a build run out of Xcode is sandbox, and each is meaningless to the
        // other.
        let host = if device.environment == "sandbox" {
            "https://api.sandbox.push.apple.com"
        } else {
            "https://api.push.apple.com"
        };
        let url = format!("{host}/3/device/{}", device.token);

        let sent = self
            .http
            .post(&url)
            .bearer_auth(bearer)
            .header("apns-topic", &self.topic)
            .header("apns-push-type", "alert")
            .header("apns-priority", "10")
            .json(&self.payload(note))
            .send()
            .await;

        match sent {
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
                    return false;
                }
                let detail = response.text().await.unwrap_or_default();
                // 410 is Apple saying this device no longer wants us; 400 BadDeviceToken is a token
                // that never made sense. Both mean the row is stale, and keeping it means retrying
                // a dead address forever.
                let dead = status.as_u16() == 410 || detail.contains("BadDeviceToken");
                if dead {
                    tracing::info!(%status, "APNs token retired");
                } else {
                    tracing::warn!(%status, detail = %detail, "APNs refused a notification");
                }
                dead
            }
            Err(e) => {
                // A network fault is not a dead token. Losing a notification is survivable — the
                // spool and the inbox are the delivery guarantee, not this.
                tracing::warn!(error = %e, "APNs unreachable");
                false
            }
        }
    }
}

/// Notify every device watching one mailbox, and forget the ones Apple says are gone.
///
/// Spawned rather than awaited by the sender: a message is delivered when it is in the recipient's
/// inbox, and whether Apple was reachable afterwards must not decide whether `POST /v1/send`
/// succeeds.
pub async fn fan_out(apns: Arc<Apns>, store: Arc<Store>, mailbox: String, note: Notification) {
    let devices = match store.devices_for(mailbox.clone()).await {
        Ok(devices) => devices,
        Err(e) => {
            tracing::warn!(error = %e, %mailbox, "device lookup failed");
            return;
        }
    };
    for device in devices {
        // Android will register against this same table, and an FCM token means nothing to Apple.
        // Skipping by platform here is what keeps that from being a bug the day it lands.
        if device.platform != "apns" {
            continue;
        }
        if apns.deliver(&device, &note).await {
            if let Err(e) = store.delete_device(device.token.clone()).await {
                tracing::warn!(error = %e, mailbox = %device.mailbox, "stale device row could not be removed");
            }
        }
    }
}

/// The first line of a message, short enough for a lock screen.
pub fn preview_of(body: &str) -> String {
    // A scoped request is JSON on the wire and reads as noise on a lock screen. Say what it asks
    // for, exactly as both clients do in a conversation list.
    if body.starts_with('{') {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(body) {
            if parsed.get("v").and_then(|v| v.as_i64()) == Some(1) {
                if let Some(verb) = parsed.get("verb").and_then(|v| v.as_str()) {
                    return format!("asks to {}", verb.replace('_', " "));
                }
            }
        }
    }
    let flattened = body.split_whitespace().collect::<Vec<_>>().join(" ");
    match flattened.char_indices().nth(PREVIEW_CHARS) {
        Some((cut, _)) => format!("{}…", &flattened[..cut]),
        None => flattened,
    }
}

fn non_empty(key: &str) -> Option<String> {
    std::env::var(key).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_previews_as_what_it_asks_for() {
        let body = r#"{"v":1,"verb":"run_tests","args":{"suite":"unit"},"note":"before the tag"}"#;
        assert_eq!(preview_of(body), "asks to run tests");
    }

    #[test]
    fn prose_that_starts_with_a_brace_is_prose() {
        assert_eq!(preview_of("{not json at all"), "{not json at all");
    }

    #[test]
    fn a_long_message_is_cut_to_a_sentence() {
        let long = "word ".repeat(200);
        let preview = preview_of(&long);
        assert!(preview.chars().count() <= PREVIEW_CHARS + 1, "{preview}");
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn whitespace_is_flattened_so_a_lock_screen_reads_one_line() {
        assert_eq!(preview_of("the build\n\nis   green"), "the build is green");
    }
}
