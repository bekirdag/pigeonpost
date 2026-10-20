//! Building audit records (SDS §6.8): who did what to which target.
//!
//! Two promises are kept here rather than at every call site:
//!
//! * **No content, no secrets.** `details` passes through [`sanitize`], which drops any field
//!   whose name says it carries a body, a token or key material, and caps the size. Callers should
//!   not put such things in, but a record that outlives its author must not depend on that.
//! * **No raw IP addresses.** An IP is stored only as a keyed HMAC, so records from one address
//!   can be correlated without the log becoming a list of where people were.

use crate::store::AuditRow;
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Who acted, as §6.8 classifies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActorKind {
    /// A person, signed in interactively.
    Human,
    /// An agent acting through a mailbox capability or a connector.
    Agent,
    /// An account API key.
    ApiKey,
    /// The server itself, or an internal service.
    System,
}

impl ActorKind {
    /// How a principal is classified: a person, a key, or an agent (mailbox token or connector).
    pub(crate) fn of(principal: &crate::Principal) -> Self {
        use crate::{AccountCredential, Principal};
        match principal {
            Principal::Account(_, AccountCredential::Member(session))
                if session.is_interactive() =>
            {
                ActorKind::Human
            }
            Principal::Account(_, AccountCredential::Member(_)) => ActorKind::System,
            Principal::Account(_, AccountCredential::ApiKey) => ActorKind::ApiKey,
            Principal::Account(_, AccountCredential::Connector { .. }) | Principal::Identity(_) => {
                ActorKind::Agent
            }
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            ActorKind::Human => "human",
            ActorKind::Agent => "agent",
            ActorKind::ApiKey => "api_key",
            ActorKind::System => "system",
        }
    }
}

/// Field names never written to the log, matched case-insensitively against each key and each
/// `_`/`-`-separated part of it.
const FORBIDDEN_KEYS: &[&str] = &[
    "body",
    "content",
    "plaintext",
    "ciphertext",
    "message",
    "text",
    "token",
    "secret",
    "password",
    "passphrase",
    "key",
    "seed",
    "authorization",
    "cookie",
    "credential",
    "ip",
];

/// Largest serialised `details` kept. Audit details are identifiers and flags, not documents.
const MAX_DETAILS: usize = 4096;
/// Longest user agent kept.
const MAX_USER_AGENT: usize = 256;

fn forbidden(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    FORBIDDEN_KEYS.contains(&key.as_str())
        || key
            .split(['_', '-'])
            .any(|part| FORBIDDEN_KEYS.contains(&part))
}

/// Drop forbidden fields (recursively) and cap the size.
pub(crate) fn sanitize(details: serde_json::Value) -> serde_json::Value {
    fn walk(v: serde_json::Value) -> serde_json::Value {
        match v {
            serde_json::Value::Object(map) => serde_json::Value::Object(
                map.into_iter()
                    .filter(|(k, _)| !forbidden(k))
                    .map(|(k, v)| (k, walk(v)))
                    .collect(),
            ),
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.into_iter().map(walk).collect())
            }
            other => other,
        }
    }
    let clean = walk(details);
    if clean.to_string().len() > MAX_DETAILS {
        serde_json::json!({ "truncated": true })
    } else {
        clean
    }
}

/// Builds audit rows with a server-side IP hashing key.
pub(crate) struct Auditor {
    ip_key: [u8; 32],
}

impl Auditor {
    pub(crate) fn new(ip_key: [u8; 32]) -> Self {
        Auditor { ip_key }
    }

    /// The keyed hash of an IP address (hex, 128 bits).
    pub(crate) fn ip_hash(&self, ip: std::net::IpAddr) -> String {
        let mut mac =
            <Hmac<Sha256> as Mac>::new_from_slice(&self.ip_key).expect("HMAC accepts any key");
        mac.update(b"pigeonpost audit ip v1\0");
        mac.update(ip.to_string().as_bytes());
        crate::hex_str(&mac.finalize().into_bytes()[..16])
    }

    /// One sanitised record.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn row(
        &self,
        actor_kind: ActorKind,
        actor_account: Option<String>,
        action: &str,
        target: Option<String>,
        details: serde_json::Value,
        ip: Option<std::net::IpAddr>,
        user_agent: Option<&str>,
        now: u64,
    ) -> AuditRow {
        AuditRow {
            org_id: None,
            at: now,
            actor_account,
            actor_kind: actor_kind.as_str(),
            action: action.to_string(),
            target,
            details: sanitize(details).to_string(),
            ip_hash: ip.map(|ip| self.ip_hash(ip)),
            user_agent: user_agent.map(|ua| ua.chars().take(MAX_USER_AGENT).collect()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn content_and_secrets_never_reach_the_log() {
        let clean = sanitize(json!({
            "feature": "account_events",
            "enabled": true,
            "body": "hello",
            "message_body": "hi",
            "api_key": "pk_live_x",
            "Authorization": "Bearer x",
            "nested": { "service": "ops", "token": "svc_x", "seed": [1, 2] },
            "list": [{ "secret": "s", "ok": 1 }],
            "client_ip": "1.2.3.4"
        }));
        assert_eq!(
            clean,
            json!({
                "feature": "account_events",
                "enabled": true,
                "nested": { "service": "ops" },
                "list": [{ "ok": 1 }]
            })
        );
    }

    #[test]
    fn oversized_details_are_truncated() {
        let big = json!({ "note": "x".repeat(MAX_DETAILS * 2) });
        assert_eq!(sanitize(big), json!({ "truncated": true }));
    }

    #[test]
    fn ip_addresses_are_stored_only_as_keyed_hashes() {
        let a = Auditor::new([7; 32]);
        let b = Auditor::new([8; 32]);
        let ip: std::net::IpAddr = "203.0.113.9".parse().unwrap();
        let row = a.row(
            ActorKind::System,
            None,
            "feature.set",
            None,
            json!({}),
            Some(ip),
            Some(&"u".repeat(1000)),
            1,
        );
        let hash = row.ip_hash.clone().unwrap();
        assert_eq!(hash.len(), 32);
        assert!(!hash.contains("203.0.113.9"));
        assert_eq!(hash, a.ip_hash(ip), "stable under one key");
        assert_ne!(hash, b.ip_hash(ip), "depends on the server key");
        assert_eq!(row.user_agent.unwrap().len(), MAX_USER_AGENT);
        assert_eq!(row.actor_kind, "system");
    }
}
