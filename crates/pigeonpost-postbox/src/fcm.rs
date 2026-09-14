//! Android wake-ups through FCM HTTP v1. Service credentials stay on the postbox.
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;

use crate::{push::Notification, store::Device};

const OAUTH: &str = "https://oauth2.googleapis.com/token";
const SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";

pub struct Fcm {
    project: String,
    email: String,
    key: EncodingKey,
    client: reqwest::Client,
    bearer: Mutex<Option<(String, u64)>>,
}

impl Fcm {
    pub fn from_env() -> Option<Arc<Self>> {
        let path = std::env::var("PIGEONPOST_FCM_SERVICE_ACCOUNT_FILE").ok()?;
        let loaded = std::fs::read(path)
            .ok()
            .and_then(|bytes| Self::from_json(&bytes));
        match loaded {
            Some(fcm) => {
                tracing::info!("FCM configured");
                Some(Arc::new(fcm))
            }
            None => {
                tracing::error!("FCM service account could not be loaded — Android push disabled");
                None
            }
        }
    }

    fn from_json(bytes: &[u8]) -> Option<Self> {
        #[derive(Deserialize)]
        struct ServiceAccount {
            project_id: String,
            client_email: String,
            private_key: String,
        }
        let account: ServiceAccount = serde_json::from_slice(bytes).ok()?;
        if account.project_id.is_empty()
            || !account
                .project_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
            || account.client_email.is_empty()
        {
            return None;
        }
        Some(Self {
            project: account.project_id,
            email: account.client_email,
            key: EncodingKey::from_rsa_pem(account.private_key.as_bytes()).ok()?,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(20))
                .build()
                .ok()?,
            bearer: Mutex::new(None),
        })
    }

    async fn access_token(&self) -> Result<String, ()> {
        let mut cached = self.bearer.lock().await;
        let now = crate::now_unix();
        if let Some((token, expires)) = cached.as_ref() {
            if *expires > now + 60 {
                return Ok(token.clone());
            }
        }
        #[derive(Serialize)]
        struct Claims<'a> {
            iss: &'a str,
            scope: &'a str,
            aud: &'a str,
            iat: u64,
            exp: u64,
        }
        let assertion = encode(
            &Header::new(Algorithm::RS256),
            &Claims {
                iss: &self.email,
                scope: SCOPE,
                aud: OAUTH,
                iat: now,
                exp: now + 3600,
            },
            &self.key,
        )
        .map_err(|_| ())?;
        let response = self
            .client
            .post(OAUTH)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ])
            .send()
            .await
            .map_err(|_| ())?;
        if !response.status().is_success() {
            return Err(());
        }
        #[derive(Deserialize)]
        struct Token {
            access_token: String,
            expires_in: u64,
        }
        let token: Token = response.json().await.map_err(|_| ())?;
        if token.access_token.is_empty() || token.expires_in < 120 {
            return Err(());
        }
        *cached = Some((token.access_token.clone(), now + token.expires_in.min(3600)));
        Ok(token.access_token)
    }

    /// Only an explicit UNREGISTERED response retires a token. Payload/configuration failures
    /// must not erase an otherwise valid device registration.
    pub async fn deliver(&self, device: &Device, note: &Notification) -> bool {
        let url = format!(
            "https://fcm.googleapis.com/v1/projects/{}/messages:send",
            self.project
        );
        for attempt in 0..2 {
            let token = match self.access_token().await {
                Ok(token) => token,
                Err(()) => {
                    tracing::warn!("FCM authentication unavailable");
                    return false;
                }
            };
            let response = match self
                .client
                .post(&url)
                .bearer_auth(token)
                .json(&payload(device, note))
                .send()
                .await
            {
                Ok(response) => response,
                Err(_) => {
                    tracing::warn!("FCM delivery unreachable");
                    return false;
                }
            };
            let status = response.status();
            if status.is_success() {
                tracing::info!("FCM notification accepted");
                return false;
            }
            if status == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
                *self.bearer.lock().await = None;
                continue;
            }
            let error = response.json::<Value>().await.unwrap_or(Value::Null);
            if unregistered(&error) {
                tracing::info!("FCM token retired");
                return true;
            }
            tracing::warn!(status = status.as_u16(), "FCM notification refused");
            return false;
        }
        false
    }
}

fn payload(device: &Device, note: &Notification) -> Value {
    // Data messages are displayed by the app only after checking the active account/identity.
    // The message text stays in the encrypted inbox instead of travelling through Google.
    json!({"message": {
        "token": device.token,
        "android": {"priority": "high", "ttl": "86400s"},
        "data": {"title": note.title, "body": "You have a new message.", "peer": note.peer,
            "identity": device.mailbox, "mailbox": note.subtitle, "message_id": note.message_id}
    }})
}

fn unregistered(value: &Value) -> bool {
    value["error"]["details"].as_array().is_some_and(|details| {
        details.iter().any(|detail| {
            detail["@type"] == "type.googleapis.com/google.firebase.fcm.v1.FcmError"
                && detail["errorCode"] == "UNREGISTERED"
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_payload_can_be_checked_before_display_and_has_no_message_text() {
        let device = Device {
            token: "opaque:FCM-token".into(),
            mailbox: "/k/receiver".into(),
            platform: "fcm".into(),
            environment: "production".into(),
        };
        let note = Notification {
            title: "/bekir/main".into(),
            subtitle: "/alp/main".into(),
            body: "private message".into(),
            message_id: "message-1".into(),
            peer: "/bekir/main".into(),
            unread: 2,
        };
        let value = payload(&device, &note);
        assert_eq!(value["message"]["data"]["identity"], "/k/receiver");
        assert_eq!(value["message"]["data"]["peer"], "/bekir/main");
        assert_eq!(value["message"]["android"]["priority"], "high");
        assert!(value["message"].get("notification").is_none());
        assert!(!value.to_string().contains("private message"));
    }

    #[test]
    fn only_a_provider_unregistered_error_retires_a_device() {
        assert!(unregistered(
            &json!({"error":{"details":[{"@type":"type.googleapis.com/google.firebase.fcm.v1.FcmError","errorCode":"UNREGISTERED"}]}})
        ));
        assert!(!unregistered(
            &json!({"error":{"status":"INVALID_ARGUMENT"}})
        ));
        assert!(!unregistered(
            &json!({"error":{"details":[{"@type":"other","errorCode":"UNREGISTERED"}]}})
        ));
        assert!(!unregistered(&Value::Null));
    }
}
