//! Windows Push Notification Services, for incoming calls on Windows (Phase 1c,
//! `docs/call-alerting.md`).
//!
//! A Windows device registers its push channel URI as its token. The postbox only ever sends to a
//! channel whose host is Microsoft's (`*.notify.windows.com`), so a registration cannot point it at
//! anything else. Authentication is the Store app's package SID and client secret from Partner
//! Center; with neither configured [`Wns::from_env`] returns `None` and Windows rings only while the
//! app runs.

use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::Mutex;

const TOKEN_URL: &str = "https://login.live.com/accesstoken.srf";
/// WNS tags are at most 16 characters.
const TAG_CHARS: usize = 16;
pub const CALL_GROUP: &str = "calls";

/// Whether `token` is a WNS channel URI this postbox will send to.
pub fn valid_channel_uri(token: &str) -> bool {
    if token.is_empty() || token.len() > 2048 {
        return false;
    }
    let Ok(url) = reqwest::Url::parse(token) else {
        return false;
    };
    let host_ok = url.host_str().is_some_and(|h| {
        let h = h.to_ascii_lowercase();
        h.ends_with(".notify.windows.com")
            && h.len() > ".notify.windows.com".len()
            && h.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-')
    });
    url.scheme() == "https"
        && host_ok
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.fragment().is_none()
}

pub struct Wns {
    sid: String,
    secret: String,
    http: reqwest::Client,
    bearer: Mutex<Option<(String, u64)>>,
}

impl Wns {
    pub fn from_env() -> Option<Arc<Self>> {
        let sid = non_empty("PIGEONPOST_WNS_PACKAGE_SID");
        let secret = non_empty("PIGEONPOST_WNS_CLIENT_SECRET").or_else(|| {
            non_empty("PIGEONPOST_WNS_CLIENT_SECRET_FILE")
                .and_then(|path| std::fs::read_to_string(path).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });
        let (Some(sid), Some(secret)) = (sid, secret) else {
            tracing::info!("WNS not configured — Windows calls ring only while the app runs");
            return None;
        };
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .build()
            .ok()?;
        tracing::info!("WNS configured");
        Some(Arc::new(Self {
            sid,
            secret,
            http,
            bearer: Mutex::new(None),
        }))
    }

    async fn access_token(&self, fresh: bool) -> Option<String> {
        let mut held = self.bearer.lock().await;
        let now = crate::now_unix();
        if !fresh {
            if let Some((token, expires)) = held.as_ref() {
                if *expires > now + 60 {
                    return Some(token.clone());
                }
            }
        }
        #[derive(Deserialize)]
        struct Token {
            access_token: String,
            #[serde(default)]
            expires_in: u64,
        }
        let response = self
            .http
            .post(TOKEN_URL)
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", self.sid.as_str()),
                ("client_secret", self.secret.as_str()),
                ("scope", "notify.windows.com"),
            ])
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            tracing::warn!(
                status = response.status().as_u16(),
                "WNS authentication refused"
            );
            return None;
        }
        let token: Token = response.json().await.ok()?;
        let lifetime = if token.expires_in == 0 {
            3600
        } else {
            token.expires_in.min(86_400)
        };
        *held = Some((token.access_token.clone(), now + lifetime));
        Some(token.access_token)
    }

    /// Show a toast on one channel. Returns `true` when the channel is gone for good.
    pub async fn toast(&self, channel: &str, xml: String, tag: &str, ttl: u64) -> bool {
        if !valid_channel_uri(channel) {
            return true;
        }
        for attempt in 0..2 {
            let Some(token) = self.access_token(attempt == 1).await else {
                return false;
            };
            let sent = self
                .http
                .post(channel)
                .bearer_auth(token)
                .header("content-type", "text/xml")
                .header("x-wns-type", "wns/toast")
                .header("x-wns-tag", tag)
                .header("x-wns-group", CALL_GROUP)
                .header("x-wns-ttl", ttl.max(1).to_string())
                .body(xml.clone())
                .send()
                .await;
            match sent {
                Ok(r) if r.status().is_success() => return false,
                Ok(r) if r.status().as_u16() == 401 && attempt == 0 => continue,
                Ok(r) => return channel_gone(r.status().as_u16()),
                Err(_) => {
                    tracing::warn!("WNS unreachable");
                    return false;
                }
            }
        }
        false
    }

    /// Remove an earlier toast (a ring that ended) from the screen and the notification centre.
    pub async fn remove(&self, channel: &str, tag: &str) -> bool {
        if !valid_channel_uri(channel) {
            return true;
        }
        let Some(token) = self.access_token(false).await else {
            return false;
        };
        let sent = self
            .http
            .delete(channel)
            .bearer_auth(token)
            .header(
                "x-wns-match",
                format!("type:wns/toast;group={CALL_GROUP};tag={tag}"),
            )
            .send()
            .await;
        match sent {
            Ok(r) if r.status().is_success() => false,
            Ok(r) => channel_gone(r.status().as_u16()),
            Err(_) => false,
        }
    }
}

/// 404 and 410 mean the channel expired or was never valid; anything else is not the channel's fault.
fn channel_gone(status: u16) -> bool {
    if matches!(status, 404 | 410) {
        tracing::info!(status, "WNS channel retired");
        true
    } else {
        tracing::warn!(status, "WNS refused a notification");
        false
    }
}

/// The toast tag for a ring: WNS allows 16 characters, and a room id is longer.
pub fn tag_for(room: &str) -> String {
    room.chars().take(TAG_CHARS).collect()
}

/// XML text and attribute escaping. Every value that reaches a toast passes through here.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

fn non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_microsoft_channels_are_accepted() {
        assert!(valid_channel_uri(
            "https://db5p.notify.windows.com/?token=AwYAAAB%2bQ%3d"
        ));
        assert!(valid_channel_uri(
            "https://wns2-db5p.notify.windows.com/w/?token=abc"
        ));
        for bad in [
            "",
            "http://db5p.notify.windows.com/?token=a",
            "https://notify.windows.com/?token=a",
            "https://db5p.notify.windows.com.evil.example/?token=a",
            "https://evil.example/?x=.notify.windows.com",
            "https://user@db5p.notify.windows.com/?token=a",
            "https://db5p.notify.windows.com:8443/?token=a",
            "https://127.0.0.1/?token=a",
            "javascript:alert(1)",
        ] {
            assert!(!valid_channel_uri(bad), "{bad}");
        }
        assert!(!valid_channel_uri(&format!(
            "https://db5p.notify.windows.com/?token={}",
            "a".repeat(2048)
        )));
    }

    #[test]
    fn escaping_leaves_no_markup() {
        assert_eq!(
            escape(r#"<a href="x">Tom & 'Jerry'</a>"#),
            "&lt;a href=&quot;x&quot;&gt;Tom &amp; &apos;Jerry&apos;&lt;/a&gt;"
        );
        assert_eq!(escape("a\u{0}b\u{1b}c"), "abc");
    }

    #[test]
    fn tags_fit_in_sixteen_characters() {
        assert_eq!(tag_for("abcdefghijklmnopqrstuv"), "abcdefghijklmnop");
        assert_eq!(tag_for("short"), "short");
    }
}
