//! Apple's App Store Server API, for turning a purchase into a namespace.
//!
//! The client tells us a transaction id. That is all it is allowed to tell us: a receipt, a signed
//! blob, or an "I bought it" flag from the app would all be the client asserting its own
//! entitlement. Here the client's word is only a *pointer*, and the answer comes from Apple over an
//! authenticated TLS connection to a host we chose.
//!
//! Because of that, the JWS Apple returns is decoded but its signature is not checked. That is
//! deliberate and it is not a shortcut: verifying the JWS would mean carrying Apple's root
//! certificate in the postbox and maintaining an X.509 chain validator, in order to re-establish
//! something TLS already established on the same response. The signature would be worth checking if
//! the JWS reached us by some other route — from the client, say, or from a webhook — and if it ever
//! does, that is the moment to add the chain check, not before.
//!
//! Everything is read from the environment. With no key configured [`AppStore::from_env`] returns
//! `None` and the claim endpoint answers as though it does not exist, the same way the namespace
//! grant does — an unconfigured deployment should not advertise a payment surface it cannot honour.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde_json::json;
use tokio::sync::Mutex;

/// Apple rejects a token older than an hour. Re-signing every 45 minutes sits inside that.
const TOKEN_LIFETIME: Duration = Duration::from_secs(45 * 60);

const PRODUCTION: &str = "https://api.storekit.itunes.apple.com";
const SANDBOX: &str = "https://api.storekit-sandbox.itunes.apple.com";

/// What Apple says about one purchase, reduced to the part that decides anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entitlement {
    /// The identity of the *subscription*, stable across every renewal. This is what a purchase is
    /// bound to; `transactionId` changes each period and would let one subscription be presented
    /// again as if it were new.
    pub original_transaction_id: String,
    pub product_id: String,
    /// Unix seconds. Apple reports milliseconds; converted here so nothing downstream has to know.
    pub expires_at: i64,
    /// `Production` or `Sandbox`, as Apple spells it. Recorded so a sandbox purchase can never be
    /// mistaken for a paid one when reading the table later.
    pub environment: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AppStoreError {
    /// Apple has no such transaction, in either environment.
    #[error("no such transaction")]
    NotFound,
    /// Both environments refused our token. A misconfiguration on our side, not the caller's.
    #[error("Apple rejected the postbox's credentials")]
    Unauthorized,
    /// The transaction is real and belongs to something else entirely.
    #[error("{0}")]
    NotOurs(String),
    /// Real, ours, and over.
    #[error("the subscription has expired")]
    Expired,
    /// Refunded, or revoked by Apple.
    #[error("the purchase was refunded")]
    Revoked,
    #[error("could not reach Apple: {0}")]
    Unreachable(String),
    #[error("Apple's answer was not in the shape this code expects")]
    Malformed,
}

pub struct AppStore {
    key: EncodingKey,
    key_id: String,
    issuer_id: String,
    bundle_id: String,
    /// The one product that buys a namespace. A second product would be a second entitlement with
    /// different terms, so it is named rather than inferred.
    product_id: String,
    http: reqwest::Client,
    bearer: Mutex<Option<(String, Instant)>>,
}

impl AppStore {
    /// Read the configuration, or decide purchases are off.
    pub fn from_env() -> Option<Arc<Self>> {
        let (key_id, issuer_id) = match (
            non_empty("PIGEONPOST_APPSTORE_KEY_ID"),
            non_empty("PIGEONPOST_APPSTORE_ISSUER_ID"),
        ) {
            (Some(key_id), Some(issuer_id)) => (key_id, issuer_id),
            _ => {
                // Said out loud for the same reason APNs says it: a silent "off" here looks
                // identical to a bug in the app, and costs a day to tell apart.
                tracing::info!(
                    "App Store purchases not configured — handle subscriptions disabled. Set \
                     PIGEONPOST_APPSTORE_KEY_ID, PIGEONPOST_APPSTORE_ISSUER_ID and \
                     PIGEONPOST_APPSTORE_KEY_PATH to switch them on."
                );
                return None;
            }
        };
        let bundle_id = non_empty("PIGEONPOST_APPSTORE_BUNDLE_ID")
            .unwrap_or_else(|| "dev.pigeonpost.inbox".to_string());
        let product_id = non_empty("PIGEONPOST_APPSTORE_PRODUCT_ID")
            .unwrap_or_else(|| "dev.pigeonpost.inbox.handle.yearly".to_string());

        let pem = match non_empty("PIGEONPOST_APPSTORE_KEY_PATH") {
            Some(path) => match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(e) => {
                    tracing::error!(error = %e, %path, "App Store key unreadable — purchases disabled");
                    return None;
                }
            },
            None => non_empty("PIGEONPOST_APPSTORE_KEY")?.into_bytes(),
        };
        let key = match EncodingKey::from_ec_pem(&pem) {
            Ok(key) => key,
            Err(e) => {
                tracing::error!(error = %e, "App Store key is not an EC private key — purchases disabled");
                return None;
            }
        };

        let http = match reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
        {
            Ok(client) => client,
            Err(e) => {
                tracing::error!(error = %e, "App Store HTTP client — purchases disabled");
                return None;
            }
        };

        tracing::info!(%bundle_id, %product_id, "App Store purchases configured");
        Some(Arc::new(Self {
            key,
            key_id,
            issuer_id,
            bundle_id,
            product_id,
            http,
            bearer: Mutex::new(None),
        }))
    }

    pub fn product_id(&self) -> &str {
        &self.product_id
    }

    /// The provider token, minted at most every 45 minutes.
    async fn bearer(&self) -> Result<String, jsonwebtoken::errors::Error> {
        let mut held = self.bearer.lock().await;
        if let Some((token, minted)) = held.as_ref() {
            if minted.elapsed() < TOKEN_LIFETIME {
                return Ok(token.clone());
            }
        }
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(self.key_id.clone());
        let issued = now_unix();
        let token = encode(
            &header,
            // `bid` is what scopes this token to one app. Without it a key that can read one of the
            // team's apps could read every one of them.
            &json!({
                "iss": self.issuer_id,
                "iat": issued,
                "exp": issued + 20 * 60,
                "aud": "appstoreconnect-v1",
                "bid": self.bundle_id,
            }),
            &self.key,
        )?;
        *held = Some((token.clone(), Instant::now()));
        Ok(token)
    }

    /// Ask Apple about one transaction and decide whether it entitles anything.
    ///
    /// Production is tried first and sandbox second, because a transaction id does not say which
    /// environment produced it and Apple provides no way to ask. The order matters: trying sandbox
    /// first would mean a real customer's claim waits on a round trip that can only fail.
    pub async fn entitlement(&self, transaction_id: &str) -> Result<Entitlement, AppStoreError> {
        // Apple puts this straight into a URL path. A caller-supplied id with a slash in it would
        // otherwise address a different endpoint entirely.
        if transaction_id.is_empty()
            || transaction_id.len() > 64
            || !transaction_id.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return Err(AppStoreError::NotFound);
        }

        let mut unauthorized = 0;
        for host in [PRODUCTION, SANDBOX] {
            match self.fetch(host, transaction_id).await {
                Ok(payload) => return judge(&payload, &self.bundle_id, &self.product_id),
                Err(AppStoreError::NotFound) => continue,
                Err(AppStoreError::Unauthorized) => {
                    unauthorized += 1;
                    continue;
                }
                Err(other) => return Err(other),
            }
        }
        // Both refused the token: that is our key, not their transaction. Distinguished because the
        // two failures need different people to fix them.
        if unauthorized == 2 {
            tracing::error!(
                "both App Store environments rejected the postbox token — check \
                 PIGEONPOST_APPSTORE_KEY_ID and PIGEONPOST_APPSTORE_ISSUER_ID"
            );
            return Err(AppStoreError::Unauthorized);
        }
        Err(AppStoreError::NotFound)
    }

    /// One environment's answer, decoded but not yet judged.
    async fn fetch(
        &self,
        host: &str,
        transaction_id: &str,
    ) -> Result<serde_json::Value, AppStoreError> {
        let token = self
            .bearer()
            .await
            .map_err(|e| AppStoreError::Unreachable(e.to_string()))?;
        let url = format!("{host}/inApps/v1/transactions/{transaction_id}");
        let response = self
            .http
            .get(&url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| AppStoreError::Unreachable(e.to_string()))?;

        match response.status().as_u16() {
            200 => {}
            401 => return Err(AppStoreError::Unauthorized),
            404 => return Err(AppStoreError::NotFound),
            status => {
                tracing::warn!(%status, %host, "unexpected status from the App Store Server API");
                return Err(AppStoreError::Unreachable(format!("HTTP {status}")));
            }
        }

        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| AppStoreError::Unreachable(e.to_string()))?;
        let jws = body
            .get("signedTransactionInfo")
            .and_then(|v| v.as_str())
            .ok_or(AppStoreError::Malformed)?;
        decode_jws_payload(jws)
    }
}

/// Everything that decides whether this purchase entitles a namespace.
///
/// Free rather than a method so it can be tested without a signing key: what it decides has nothing
/// to do with how the request was authenticated.
fn judge(
    claims: &serde_json::Value,
    bundle_id: &str,
    product_id: &str,
) -> Result<Entitlement, AppStoreError> {
    let string = |key: &str| claims.get(key).and_then(|v| v.as_str()).unwrap_or_default();

    // The two checks that matter most, and the two easiest to leave out. Without them any
    // transaction from any app on the team — or any other product in this app — would buy a
    // namespace, and Apple would have told us the truth about every one of them.
    let bundle = string("bundleId");
    if bundle != bundle_id {
        return Err(AppStoreError::NotOurs(format!(
            "that purchase belongs to {bundle}, not to this app"
        )));
    }
    let product = string("productId");
    if product != product_id {
        return Err(AppStoreError::NotOurs(format!(
            "{product} does not buy a handle"
        )));
    }

    if claims.get("revocationDate").is_some_and(|v| !v.is_null()) {
        return Err(AppStoreError::Revoked);
    }

    let expires_ms = claims
        .get("expiresDate")
        .and_then(|v| v.as_i64())
        .ok_or(AppStoreError::Malformed)?;
    let expires_at = expires_ms / 1000;
    if expires_at <= now_unix() as i64 {
        return Err(AppStoreError::Expired);
    }

    let original = string("originalTransactionId");
    if original.is_empty() {
        return Err(AppStoreError::Malformed);
    }

    Ok(Entitlement {
        original_transaction_id: original.to_string(),
        product_id: product.to_string(),
        expires_at,
        // Absent means production: Apple omits the field on older transactions, and defaulting the
        // other way would file a real purchase as a test one.
        environment: match string("environment") {
            "" => "Production".to_string(),
            other => other.to_string(),
        },
    })
}

/// The middle segment of a JWS, as JSON. See the module note on why the signature is not checked.
fn decode_jws_payload(jws: &str) -> Result<serde_json::Value, AppStoreError> {
    let payload = jws.split('.').nth(1).ok_or(AppStoreError::Malformed)?;
    let bytes = b64url_decode(payload).map_err(|()| AppStoreError::Malformed)?;
    serde_json::from_slice(&bytes).map_err(|_| AppStoreError::Malformed)
}

/// base64url without padding, which is the only alphabet a JWS uses. The postbox's own `b64_decode`
/// is the standard alphabet and would reject `-` and `_`.
fn b64url_decode(text: &str) -> Result<Vec<u8>, ()> {
    let value = |c: u8| -> Result<u32, ()> {
        Ok(match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return Err(()),
        })
    };
    let raw: Vec<u8> = text.bytes().filter(|b| *b != b'=').collect();
    if raw.len() % 4 == 1 {
        return Err(());
    }
    let mut out = Vec::with_capacity(raw.len() * 3 / 4);
    for chunk in raw.chunks(4) {
        let mut n = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            n |= value(*c)? << (18 - 6 * i);
        }
        for i in 0..chunk.len() - 1 {
            out.push(((n >> (16 - 8 * i)) & 0xff) as u8);
        }
    }
    Ok(out)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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
    use serde_json::json;

    const BUNDLE: &str = "dev.pigeonpost.inbox";
    const PRODUCT: &str = "dev.pigeonpost.inbox.handle.yearly";

    fn future_ms() -> i64 {
        (now_unix() as i64 + 86_400) * 1000
    }

    fn good() -> serde_json::Value {
        json!({
            "bundleId": BUNDLE,
            "productId": PRODUCT,
            "originalTransactionId": "2000000900000001",
            "transactionId": "2000000900000009",
            "expiresDate": future_ms(),
            "environment": "Sandbox",
        })
    }

    #[test]
    fn accepts_a_live_purchase_of_the_right_product() {
        let e = judge(&good(), BUNDLE, PRODUCT).expect("should be entitled");
        assert_eq!(e.original_transaction_id, "2000000900000001");
        assert_eq!(e.environment, "Sandbox");
        assert!(e.expires_at > now_unix() as i64);
    }

    /// The renewal identity, not the period identity. Binding to `transactionId` would let the same
    /// subscription be presented as a new purchase every period.
    #[test]
    fn binds_to_the_original_transaction_not_the_current_one() {
        let e = judge(&good(), BUNDLE, PRODUCT).unwrap();
        assert_ne!(e.original_transaction_id, "2000000900000009");
    }

    #[test]
    fn refuses_another_app_on_the_same_team() {
        let mut claims = good();
        claims["bundleId"] = json!("dev.pigeonpost.something-else");
        assert!(matches!(
            judge(&claims, BUNDLE, PRODUCT),
            Err(AppStoreError::NotOurs(_))
        ));
    }

    #[test]
    fn refuses_a_different_product_in_this_app() {
        let mut claims = good();
        claims["productId"] = json!("dev.pigeonpost.inbox.tip.small");
        assert!(matches!(
            judge(&claims, BUNDLE, PRODUCT),
            Err(AppStoreError::NotOurs(_))
        ));
    }

    #[test]
    fn refuses_an_expired_subscription() {
        let mut claims = good();
        claims["expiresDate"] = json!((now_unix() as i64 - 60) * 1000);
        assert_eq!(judge(&claims, BUNDLE, PRODUCT), Err(AppStoreError::Expired));
    }

    #[test]
    fn refuses_a_refunded_purchase_even_before_it_expires() {
        let mut claims = good();
        claims["revocationDate"] = json!(future_ms());
        assert_eq!(judge(&claims, BUNDLE, PRODUCT), Err(AppStoreError::Revoked));
    }

    /// Apple sends `null` rather than omitting the field in some responses; treating that as a
    /// revocation would refuse every good purchase.
    #[test]
    fn a_null_revocation_date_is_not_a_revocation() {
        let mut claims = good();
        claims["revocationDate"] = serde_json::Value::Null;
        assert!(judge(&claims, BUNDLE, PRODUCT).is_ok());
    }

    #[test]
    fn a_missing_expiry_is_malformed_rather_than_forever() {
        let mut claims = good();
        claims.as_object_mut().unwrap().remove("expiresDate");
        assert_eq!(
            judge(&claims, BUNDLE, PRODUCT),
            Err(AppStoreError::Malformed)
        );
    }

    #[test]
    fn an_absent_environment_reads_as_production() {
        let mut claims = good();
        claims.as_object_mut().unwrap().remove("environment");
        assert_eq!(
            judge(&claims, BUNDLE, PRODUCT).unwrap().environment,
            "Production"
        );
    }

    #[test]
    fn decodes_a_jws_payload_with_url_alphabet() {
        // `~` and `?` encode to bytes that use `-` and `_` in the URL alphabet, which the postbox's
        // standard-alphabet decoder would reject.
        let payload = json!({"bundleId": "a~b?c"});
        let raw = serde_json::to_vec(&payload).unwrap();
        let mut encoded = String::new();
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        for chunk in raw.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            for i in 0..=chunk.len() {
                encoded.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
            }
        }
        let jws = format!("header.{encoded}.signature");
        assert_eq!(decode_jws_payload(&jws).unwrap(), payload);
    }

    #[test]
    fn a_jws_without_a_payload_segment_is_malformed() {
        assert_eq!(
            decode_jws_payload("onlyonesegment"),
            Err(AppStoreError::Malformed)
        );
    }
}
