//! Google Play subscriptions are verified by the publisher API, never by device flags.
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use tokio::sync::Mutex;

pub const MAX_HANDLES: usize = 10;
pub const PACKAGE: &str = "dev.pigeonpost.inbox";
pub const BASE_PLAN: &str = "annual";
const OAUTH: &str = "https://oauth2.googleapis.com/token";

pub fn products() -> Vec<String> {
    (1..=MAX_HANDLES)
        .map(|slot| format!("pigeonpost.handle.{slot:02}"))
        .collect()
}

pub fn account_id(account: &str) -> String {
    crate::hex_str(&crate::sha256(
        format!("pigeonpost-google-account:{account}").as_bytes(),
    ))
}

#[derive(Debug, thiserror::Error)]
pub enum PlayError {
    #[error("Google Play verification is temporarily unavailable")]
    Unavailable,
    #[error("Google Play could not find this purchase")]
    NotFound,
    #[error("This purchase is not a supported annual handle subscription")]
    Invalid,
    #[error("This purchase belongs to a different Pigeonpost account")]
    AccountMismatch,
}

#[derive(Deserialize)]
struct ServiceKey {
    client_email: String,
    private_key: String,
    private_key_id: String,
}

pub struct GooglePlay {
    client: reqwest::Client,
    email: String,
    key: EncodingKey,
    key_id: String,
    bearer: Mutex<Option<(String, u64)>>,
    /// Serialize API observation + persistence so a delayed active response cannot undo a refund.
    pub verification: Mutex<()>,
}

impl GooglePlay {
    pub fn from_env() -> Option<Arc<Self>> {
        let path = std::env::var("GOOGLE_PLAY_SERVICE_ACCOUNT_FILE").ok()?;
        let configured = (|| {
            let mut bytes = std::fs::read(path).ok()?;
            let parsed = serde_json::from_slice::<ServiceKey>(&bytes);
            zeroize::Zeroize::zeroize(&mut bytes);
            let mut credential = parsed.ok()?;
            let key = EncodingKey::from_rsa_pem(credential.private_key.as_bytes()).ok();
            zeroize::Zeroize::zeroize(&mut credential.private_key);
            let key = key?;
            Some(Self {
                client: reqwest::Client::builder()
                    .https_only(true)
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(Duration::from_secs(20))
                    .build()
                    .ok()?,
                email: credential.client_email,
                key,
                key_id: credential.private_key_id,
                bearer: Mutex::new(None),
                verification: Mutex::new(()),
            })
        })();
        if configured.is_none() {
            tracing::error!("Google Play service identity is invalid; purchases disabled");
        }
        configured.map(Arc::new)
    }

    async fn access_token(&self) -> Result<String, PlayError> {
        let mut cached = self.bearer.lock().await;
        let now = crate::now_unix();
        if let Some((token, expiry)) = &*cached {
            if *expiry > now + 60 {
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
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(self.key_id.clone());
        let assertion = encode(
            &header,
            &Claims {
                iss: &self.email,
                scope: "https://www.googleapis.com/auth/androidpublisher",
                aud: OAUTH,
                iat: now,
                exp: now + 3600,
            },
            &self.key,
        )
        .map_err(|_| PlayError::Unavailable)?;
        #[derive(Deserialize)]
        struct Token {
            access_token: String,
            expires_in: u64,
        }
        let response = self
            .client
            .post(OAUTH)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", &assertion),
            ])
            .send()
            .await
            .map_err(|_| PlayError::Unavailable)?;
        if !response.status().is_success() {
            return Err(PlayError::Unavailable);
        }
        let token: Token = response.json().await.map_err(|_| PlayError::Unavailable)?;
        if token.access_token.is_empty() || token.expires_in < 120 {
            return Err(PlayError::Unavailable);
        }
        *cached = Some((token.access_token.clone(), now + token.expires_in.min(3600)));
        Ok(token.access_token)
    }

    fn url(segments: &[&str]) -> reqwest::Url {
        let mut url =
            reqwest::Url::parse("https://androidpublisher.googleapis.com/").expect("static URL");
        url.path_segments_mut()
            .expect("HTTPS URL")
            .pop_if_empty()
            .extend(["androidpublisher", "v3", "applications", PACKAGE])
            .extend(segments);
        url
    }

    pub async fn verify(
        &self,
        purchase_token: &str,
        account: &str,
    ) -> Result<VerifiedPurchase, PlayError> {
        if purchase_token.is_empty()
            || purchase_token.len() > 4096
            || purchase_token.chars().any(char::is_whitespace)
        {
            return Err(PlayError::Invalid);
        }
        let response = self
            .client
            .get(Self::url(&[
                "purchases",
                "subscriptionsv2",
                "tokens",
                purchase_token,
            ]))
            .bearer_auth(self.access_token().await?)
            .send()
            .await
            .map_err(|_| PlayError::Unavailable)?;
        match response.status().as_u16() {
            200 => {}
            404 | 410 => return Err(PlayError::NotFound),
            401 => {
                *self.bearer.lock().await = None;
                return Err(PlayError::Unavailable);
            }
            _ => return Err(PlayError::Unavailable),
        }
        let purchase: Subscription = response.json().await.map_err(|_| PlayError::Unavailable)?;
        purchase.validate(account, crate::now_unix())
    }

    pub async fn acknowledge(&self, purchase_token: &str, product: &str) -> Result<(), PlayError> {
        let action = format!("{purchase_token}:acknowledge");
        let response = self
            .client
            .post(Self::url(&[
                "purchases",
                "subscriptions",
                product,
                "tokens",
                &action,
            ]))
            .bearer_auth(self.access_token().await?)
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|_| PlayError::Unavailable)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(PlayError::Unavailable)
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Subscription {
    subscription_state: String,
    acknowledgement_state: String,
    #[serde(default)]
    line_items: Vec<LineItem>,
    external_account_identifiers: Option<ExternalAccount>,
    linked_purchase_token: Option<String>,
    test_purchase: Option<serde_json::Value>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExternalAccount {
    obfuscated_external_account_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LineItem {
    product_id: String,
    expiry_time: Option<String>,
    auto_renewing_plan: Option<AutoRenewing>,
    offer_details: Option<Offer>,
    latest_successful_order_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AutoRenewing {
    #[serde(default)]
    auto_renew_enabled: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Offer {
    base_plan_id: String,
}

#[derive(Clone)]
pub struct VerifiedPurchase {
    pub product_id: String,
    pub linked_token: Option<String>,
    pub expires_at: u64,
    pub state: String,
    pub active: bool,
    pub auto_renewing: bool,
    pub acknowledged: bool,
    pub test_purchase: bool,
}

impl Subscription {
    fn validate(self, account: &str, now: u64) -> Result<VerifiedPurchase, PlayError> {
        if self
            .external_account_identifiers
            .and_then(|a| a.obfuscated_external_account_id)
            .as_deref()
            != Some(&account_id(account))
        {
            return Err(PlayError::AccountMismatch);
        }
        if self.line_items.len() != 1 {
            return Err(PlayError::Invalid);
        }
        let item = self
            .line_items
            .into_iter()
            .next()
            .ok_or(PlayError::Invalid)?;
        if !products().contains(&item.product_id)
            || item.offer_details.as_ref().map(|o| o.base_plan_id.as_str()) != Some(BASE_PLAN)
            || item.auto_renewing_plan.is_none()
        {
            return Err(PlayError::Invalid);
        }
        let expiry = item
            .expiry_time
            .as_deref()
            .and_then(|t| OffsetDateTime::parse(t, &Rfc3339).ok())
            .and_then(|t| u64::try_from(t.unix_timestamp()).ok())
            .unwrap_or(0);
        let active = expiry > now
            && item
                .latest_successful_order_id
                .as_ref()
                .is_some_and(|s| !s.is_empty())
            && matches!(
                self.subscription_state.as_str(),
                "SUBSCRIPTION_STATE_ACTIVE"
                    | "SUBSCRIPTION_STATE_CANCELED"
                    | "SUBSCRIPTION_STATE_IN_GRACE_PERIOD"
            );
        Ok(VerifiedPurchase {
            product_id: item.product_id,
            linked_token: self.linked_purchase_token,
            expires_at: expiry,
            state: self.subscription_state,
            active,
            auto_renewing: item
                .auto_renewing_plan
                .is_some_and(|p| p.auto_renew_enabled),
            acknowledged: self.acknowledgement_state == "ACKNOWLEDGEMENT_STATE_ACKNOWLEDGED",
            test_purchase: self.test_purchase.is_some(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> serde_json::Value {
        json!({ "subscriptionState": "SUBSCRIPTION_STATE_ACTIVE", "acknowledgementState": "ACKNOWLEDGEMENT_STATE_PENDING",
            "externalAccountIdentifiers": { "obfuscatedExternalAccountId": account_id("acct_a") },
            "lineItems": [{ "productId": "pigeonpost.handle.01", "expiryTime": "2030-01-01T03:00:00.000000000+03:00",
                "autoRenewingPlan": { "autoRenewEnabled": true }, "offerDetails": { "basePlanId": "annual" },
                "latestSuccessfulOrderId": "test-order" }] })
    }
    fn verify(value: serde_json::Value) -> Result<VerifiedPurchase, PlayError> {
        serde_json::from_value::<Subscription>(value)
            .unwrap()
            .validate("acct_a", 100)
    }
    #[test]
    fn google_paid_purchase_binds_to_account_and_parses_expiry() {
        let good = verify(fixture()).unwrap();
        assert!(good.active);
        assert!(!good.acknowledged);
        assert_eq!(good.expires_at, 1893456000);
        assert_eq!(account_id("acct_a").len(), 64);
        let subscription: Subscription = serde_json::from_value(fixture()).unwrap();
        assert!(matches!(
            subscription.validate("acct_b", 100),
            Err(PlayError::AccountMismatch)
        ));
    }
    #[test]
    fn google_payment_states_do_not_confuse_cancellation_with_revocation() {
        for state in [
            "SUBSCRIPTION_STATE_ACTIVE",
            "SUBSCRIPTION_STATE_CANCELED",
            "SUBSCRIPTION_STATE_IN_GRACE_PERIOD",
        ] {
            let mut value = fixture();
            value["subscriptionState"] = json!(state);
            assert!(verify(value).unwrap().active, "{state}");
        }
        for state in [
            "SUBSCRIPTION_STATE_PENDING",
            "SUBSCRIPTION_STATE_ON_HOLD",
            "SUBSCRIPTION_STATE_PAUSED",
            "SUBSCRIPTION_STATE_EXPIRED",
            "future_unknown_state",
        ] {
            let mut value = fixture();
            value["subscriptionState"] = json!(state);
            assert!(!verify(value).unwrap().active, "{state}");
        }
        for field in ["expiryTime", "latestSuccessfulOrderId"] {
            let mut value = fixture();
            value["lineItems"][0].as_object_mut().unwrap().remove(field);
            assert!(!verify(value).unwrap().active);
        }
    }
    #[test]
    fn google_rejects_wrong_products_plans_quantities_and_missing_account_binding() {
        let mut value = fixture();
        value["lineItems"][0]["productId"] = json!("pigeonpost.handle.11");
        assert!(matches!(verify(value), Err(PlayError::Invalid)));
        let mut value = fixture();
        value["lineItems"][0]["offerDetails"]["basePlanId"] = json!("monthly");
        assert!(matches!(verify(value), Err(PlayError::Invalid)));
        let mut value = fixture();
        value["lineItems"] = json!([value["lineItems"][0], value["lineItems"][0]]);
        assert!(matches!(verify(value), Err(PlayError::Invalid)));
        let mut value = fixture();
        value
            .as_object_mut()
            .unwrap()
            .remove("externalAccountIdentifiers");
        assert!(matches!(verify(value), Err(PlayError::AccountMismatch)));
    }
}
