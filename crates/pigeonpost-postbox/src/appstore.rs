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

/// Refresh before the provider JWT's twenty-minute expiry, including clock skew and request time.
const TOKEN_CACHE_LIFETIME: Duration = Duration::from_secs(15 * 60);
const TOKEN_VALIDITY_SECONDS: u64 = 20 * 60;
pub const MAX_HANDLES: usize = 10;

const PRODUCTION: &str = "https://api.storekit.apple.com";
const SANDBOX: &str = "https://api.storekit-sandbox.apple.com";

/// What Apple says about one purchase, reduced to the part that decides anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entitlement {
    /// The identity of the *subscription*, stable across every renewal. This is what a purchase is
    /// bound to; `transactionId` changes each period and would let one subscription be presented
    /// again as if it were new.
    pub original_transaction_id: String,
    pub product_id: String,
    /// Set by newer clients to bind a purchase to the signed-in Pigeonpost account.
    pub app_account_token: Option<String>,
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
    /// Each explicitly configured product buys one independently renewable name.
    product_ids: Vec<String>,
    http: reqwest::Client,
    bearer: Mutex<Option<(String, Instant)>>,
    /// Serialize provider reads through durable writes, including background renewal checks.
    pub verification: Mutex<()>,
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
        let product_ids = match product_catalog(
            &product_id,
            non_empty("PIGEONPOST_APPSTORE_PRODUCT_IDS").as_deref(),
        ) {
            Some(ids) => ids,
            None => {
                tracing::error!("invalid App Store handle catalog — purchases disabled");
                return None;
            }
        };

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
            .redirect(reqwest::redirect::Policy::none())
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
            product_ids,
            http,
            bearer: Mutex::new(None),
            verification: Mutex::new(()),
        }))
    }

    pub fn product_id(&self) -> &str {
        &self.product_ids[0]
    }

    pub fn product_ids(&self) -> &[String] {
        &self.product_ids
    }

    /// The provider token, refreshed before it expires.
    async fn bearer(&self) -> Result<String, jsonwebtoken::errors::Error> {
        let mut held = self.bearer.lock().await;
        if let Some((token, minted)) = held.as_ref() {
            if minted.elapsed() < TOKEN_CACHE_LIFETIME {
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
                "exp": issued + TOKEN_VALIDITY_SECONDS,
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
                Ok(payload) => {
                    let product = payload
                        .get("productId")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    if !self.product_ids.iter().any(|id| id == product) {
                        return Err(AppStoreError::NotOurs(
                            "that product does not buy a handle".into(),
                        ));
                    }
                    let original = payload
                        .get("originalTransactionId")
                        .and_then(|v| v.as_str())
                        .ok_or(AppStoreError::Malformed)?;
                    let environment = if host == SANDBOX {
                        "Sandbox"
                    } else {
                        "Production"
                    };
                    let status = self.subscription_status(original, environment).await?;
                    return if status.active {
                        Ok(status.entitlement)
                    } else if status.revoked {
                        Err(AppStoreError::Revoked)
                    } else {
                        Err(AppStoreError::Expired)
                    };
                }
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

    /// Reads current renewal state, including billing grace, without a receipt from an open app.
    pub async fn subscription_status(
        &self,
        original: &str,
        environment: &str,
    ) -> Result<SubscriptionStatus, AppStoreError> {
        if original.is_empty()
            || original.len() > 64
            || !original.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return Err(AppStoreError::NotFound);
        }
        let host = match environment {
            "Production" => PRODUCTION,
            "Sandbox" => SANDBOX,
            _ => return Err(AppStoreError::Malformed),
        };
        let body = self
            .fetch_json(host, &format!("/inApps/v1/subscriptions/{original}"))
            .await?;
        judge_status(
            &body,
            original,
            environment,
            &self.bundle_id,
            &self.product_ids,
            now_unix() as i64,
        )
    }

    async fn fetch(
        &self,
        host: &str,
        transaction_id: &str,
    ) -> Result<serde_json::Value, AppStoreError> {
        let body = self
            .fetch_json(host, &format!("/inApps/v1/transactions/{transaction_id}"))
            .await?;
        let jws = body
            .get("signedTransactionInfo")
            .and_then(|v| v.as_str())
            .ok_or(AppStoreError::Malformed)?;
        decode_jws_payload(jws)
    }

    async fn fetch_json(&self, host: &str, path: &str) -> Result<serde_json::Value, AppStoreError> {
        let token = self
            .bearer()
            .await
            .map_err(|e| AppStoreError::Unreachable(e.to_string()))?;
        let response = self
            .http
            .get(format!("{host}{path}"))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| AppStoreError::Unreachable(e.to_string()))?;
        match response.status().as_u16() {
            200 => {}
            401 => return Err(AppStoreError::Unauthorized),
            404 => return Err(AppStoreError::NotFound),
            status => return Err(AppStoreError::Unreachable(format!("HTTP {status}"))),
        }
        response.json().await.map_err(|_| AppStoreError::Malformed)
    }
}

/// Everything that decides whether this purchase entitles a namespace.
///
/// Free rather than a method so it can be tested without a signing key: what it decides has nothing
/// to do with how the request was authenticated.
fn parse_entitlement(
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

    let expires_ms = claims
        .get("expiresDate")
        .and_then(|v| v.as_i64())
        .ok_or(AppStoreError::Malformed)?;
    let expires_at = expires_ms / 1000;

    let original = string("originalTransactionId");
    if original.is_empty() {
        return Err(AppStoreError::Malformed);
    }

    Ok(Entitlement {
        original_transaction_id: original.to_string(),
        product_id: product.to_string(),
        app_account_token: claims
            .get("appAccountToken")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        expires_at,
        // Absent means production: Apple omits the field on older transactions, and defaulting the
        // other way would file a real purchase as a test one.
        environment: match string("environment") {
            "" => "Production".to_string(),
            other => other.to_string(),
        },
    })
}

#[derive(Debug)]
pub struct SubscriptionStatus {
    pub entitlement: Entitlement,
    pub active: bool,
    pub terminal: bool,
    pub revoked: bool,
}

/// These JWS payloads are accepted only from the authenticated, fixed-host status API above.
fn judge_status(
    body: &serde_json::Value,
    original: &str,
    environment: &str,
    bundle: &str,
    products: &[String],
    now: i64,
) -> Result<SubscriptionStatus, AppStoreError> {
    if body.get("bundleId").and_then(|v| v.as_str()) != Some(bundle)
        || body.get("environment").and_then(|v| v.as_str()) != Some(environment)
    {
        return Err(AppStoreError::NotOurs(
            "subscription response app or environment mismatch".into(),
        ));
    }
    let groups = body
        .get("data")
        .and_then(|v| v.as_array())
        .ok_or(AppStoreError::Malformed)?;
    let mut found = None;
    for group in groups {
        for item in group
            .get("lastTransactions")
            .and_then(|v| v.as_array())
            .ok_or(AppStoreError::Malformed)?
        {
            if item.get("originalTransactionId").and_then(|v| v.as_str()) != Some(original) {
                continue;
            }
            if found.is_some() {
                return Err(AppStoreError::Malformed);
            }
            let claims = decode_jws_payload(
                item.get("signedTransactionInfo")
                    .and_then(|v| v.as_str())
                    .ok_or(AppStoreError::Malformed)?,
            )?;
            let product = claims
                .get("productId")
                .and_then(|v| v.as_str())
                .ok_or(AppStoreError::Malformed)?;
            if !products.iter().any(|p| p == product) {
                return Err(AppStoreError::NotOurs(
                    "that product does not buy a handle".into(),
                ));
            }
            let mut entitlement = parse_entitlement(&claims, bundle, product)?;
            if entitlement.original_transaction_id != original
                || entitlement.environment != environment
                || entitlement.expires_at < 0
            {
                return Err(AppStoreError::Malformed);
            }
            let status = item
                .get("status")
                .and_then(|v| v.as_u64())
                .ok_or(AppStoreError::Malformed)?;
            if !(1..=5).contains(&status) {
                return Err(AppStoreError::Malformed);
            }
            let revoked_at = claims
                .get("revocationDate")
                .filter(|v| !v.is_null())
                .map(|v| {
                    v.as_i64()
                        .map(|ms| ms / 1000)
                        .ok_or(AppStoreError::Malformed)
                })
                .transpose()?;
            let revoked = status == 5 || revoked_at.is_some();
            if status == 4 && !revoked {
                let renewal = decode_jws_payload(
                    item.get("signedRenewalInfo")
                        .and_then(|v| v.as_str())
                        .ok_or(AppStoreError::Malformed)?,
                )?;
                if renewal
                    .get("originalTransactionId")
                    .and_then(|v| v.as_str())
                    != Some(original)
                    || renewal.get("environment").and_then(|v| v.as_str()) != Some(environment)
                {
                    return Err(AppStoreError::Malformed);
                }
                entitlement.expires_at = renewal
                    .get("gracePeriodExpiresDate")
                    .and_then(|v| v.as_i64())
                    .ok_or(AppStoreError::Malformed)?
                    / 1000;
            }
            let active = !revoked && matches!(status, 1 | 4) && entitlement.expires_at > now;
            // Retry is recoverable and never opens resale, even after the local recovery window.
            let terminal = revoked || (status == 2 && entitlement.expires_at <= now);
            if !active {
                entitlement.expires_at = entitlement
                    .expires_at
                    .min(revoked_at.unwrap_or(now))
                    .min(now)
                    .max(0);
            }
            found = Some(SubscriptionStatus {
                entitlement,
                active,
                terminal,
                revoked,
            });
        }
    }
    found.ok_or(AppStoreError::NotFound)
}

#[cfg(test)]
fn judge(
    claims: &serde_json::Value,
    bundle: &str,
    product: &str,
) -> Result<Entitlement, AppStoreError> {
    let entitlement = parse_entitlement(claims, bundle, product)?;
    if claims.get("revocationDate").is_some_and(|v| !v.is_null()) {
        return Err(AppStoreError::Revoked);
    }
    if entitlement.expires_at <= now_unix() as i64 {
        return Err(AppStoreError::Expired);
    }
    Ok(entitlement)
}

fn product_catalog(primary: &str, configured: Option<&str>) -> Option<Vec<String>> {
    let ids: Vec<String> = configured
        .unwrap_or(primary)
        .split(',')
        .map(|id| id.trim().to_owned())
        .collect();
    if ids.is_empty()
        || ids.len() > MAX_HANDLES
        || ids.first().map(String::as_str) != Some(primary)
        || ids.iter().any(|id| {
            id.is_empty()
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
        })
        || ids.iter().collect::<std::collections::HashSet<_>>().len() != ids.len()
    {
        return None;
    }
    Some(ids)
}

/// An opaque, stable UUID for StoreKit's appAccountToken; the server verifies it after Apple.
pub fn account_token(account: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut bytes = Sha256::digest(format!("pigeonpost.appstore.account.v1:{account}"));
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = bytes[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
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

    #[test]
    fn catalog_is_explicit_bounded_and_keeps_the_legacy_product_first() {
        assert_eq!(product_catalog(PRODUCT, None).unwrap(), vec![PRODUCT]);
        let ids = std::iter::once(PRODUCT.to_owned())
            .chain((2..=10).map(|n| format!("dev.pigeonpost.inbox.handle{n}.yearly")))
            .collect::<Vec<_>>();
        assert_eq!(product_catalog(PRODUCT, Some(&ids.join(","))).unwrap(), ids);
        for invalid in [
            format!("{PRODUCT},{PRODUCT}"),
            format!("{PRODUCT},bad/id"),
            format!("other,{PRODUCT}"),
            format!("{PRODUCT},"),
            format!("{},extra", ids.join(",")),
        ] {
            assert!(product_catalog(PRODUCT, Some(&invalid)).is_none());
        }
    }

    #[test]
    fn apple_account_token_is_stable_scoped_and_uuid_shaped() {
        let token = account_token("acct_one");
        assert_eq!(token, account_token("acct_one"));
        assert_ne!(token, account_token("acct_two"));
        assert_eq!(token.len(), 36);
        assert_eq!(token.chars().nth(14), Some('8'));
        assert!(matches!(token.chars().nth(19), Some('8' | '9' | 'a' | 'b')));
    }

    #[test]
    fn signed_transaction_preserves_the_pigeonpost_account_token() {
        let mut payload = good();
        payload["appAccountToken"] = json!(account_token("acct_one"));
        assert_eq!(
            judge(&payload, BUNDLE, PRODUCT).unwrap().app_account_token,
            Some(account_token("acct_one"))
        );
        assert_eq!(
            judge(&good(), BUNDLE, PRODUCT).unwrap().app_account_token,
            None
        );
    }

    #[test]
    fn cached_bearer_expires_before_its_apple_jwt() {
        assert!(TOKEN_CACHE_LIFETIME.as_secs() < TOKEN_VALIDITY_SECONDS);
    }

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

    fn status_body(
        status: u64,
        claims: serde_json::Value,
        renewal: serde_json::Value,
    ) -> serde_json::Value {
        let jws = |payload: serde_json::Value| {
            let encoded = crate::b64_encode(&serde_json::to_vec(&payload).unwrap())
                .trim_end_matches('=')
                .replace('+', "-")
                .replace('/', "_");
            format!("header.{encoded}.signature")
        };
        json!({"bundleId": BUNDLE, "environment": "Sandbox", "data": [{"lastTransactions": [{
            "originalTransactionId": "2000000900000001", "status": status,
            "signedTransactionInfo": jws(claims), "signedRenewalInfo": jws(renewal)
        }]}]})
    }

    fn current_status(body: &serde_json::Value) -> Result<SubscriptionStatus, AppStoreError> {
        judge_status(
            body,
            "2000000900000001",
            "Sandbox",
            BUNDLE,
            &[PRODUCT.into()],
            now_unix() as i64,
        )
    }

    #[test]
    fn lifecycle_apple_current_status_honors_renewal_grace_and_terminal_expiry() {
        let active = current_status(&status_body(1, good(), json!({}))).unwrap();
        assert!(active.active && !active.terminal);
        let mut expired = good();
        expired["expiresDate"] = json!((now_unix() as i64 - 60) * 1000);
        let ended = current_status(&status_body(2, expired.clone(), json!({}))).unwrap();
        assert!(!ended.active && ended.terminal);
        let retry = current_status(&status_body(3, expired.clone(), json!({}))).unwrap();
        assert!(
            !retry.active && !retry.terminal,
            "billing retry cannot authorize resale"
        );
        let renewal = json!({"originalTransactionId": "2000000900000001", "environment": "Sandbox", "gracePeriodExpiresDate": future_ms()});
        let grace = current_status(&status_body(4, expired, renewal)).unwrap();
        assert!(grace.active && !grace.terminal);
        assert!(grace.entitlement.expires_at > now_unix() as i64);
        let mut refunded = good();
        refunded["revocationDate"] = json!((now_unix() as i64 - 30) * 1000);
        let revoked = current_status(&status_body(5, refunded, json!({}))).unwrap();
        assert!(!revoked.active && revoked.terminal && revoked.revoked);
        assert!(revoked.entitlement.expires_at < now_unix() as i64);
    }

    #[test]
    fn lifecycle_apple_status_rejects_cross_app_environment_and_subscription_data() {
        for field in [
            "bundleId",
            "environment",
            "originalTransactionId",
            "productId",
        ] {
            let mut wrong = good();
            wrong[field] = json!("other");
            assert!(
                current_status(&status_body(1, wrong, json!({}))).is_err(),
                "{field}"
            );
        }
        let mut wrong = status_body(1, good(), json!({}));
        wrong["environment"] = json!("Production");
        assert!(current_status(&wrong).is_err());
        assert!(
            current_status(&status_body(4, good(), json!({}))).is_err(),
            "grace needs verified renewal data"
        );
        assert!(
            current_status(&status_body(99, good(), json!({}))).is_err(),
            "unknown states fail closed"
        );
        let mut absent = status_body(1, good(), json!({}));
        absent["data"] = json!([]);
        assert!(matches!(
            current_status(&absent),
            Err(AppStoreError::NotFound)
        ));
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
