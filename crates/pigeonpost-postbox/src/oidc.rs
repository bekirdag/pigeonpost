//! OIDC member-token validation (plan §11 — OAuth-backed accounts).
//!
//! Accepts a `pigeonpost-prod` member JWT (RS256) as a third auth method alongside capability tokens
//! and API keys. Validates signature against the realm's JWKS (fetched over HTTPS and cached by
//! `kid`), plus issuer and expiry. **Audience is not required** — the postbox isn't the token's
//! audience; any valid member token from the configured realm identifies its `sub`, which maps to an
//! account. Tokens never grant more than "you are this Keycloak subject".

use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use std::collections::HashMap;
use tokio::sync::RwLock;

#[derive(Debug, thiserror::Error)]
pub enum OidcError {
    #[error("malformed token")]
    Malformed,
    #[error("token failed validation")]
    Invalid,
    #[error("unknown signing key")]
    UnknownKid,
    #[error("bad signing key")]
    Key,
    #[error("could not fetch JWKS")]
    Fetch,
}

/// The claims we consume — just the subject; issuer and expiry are validated by `jsonwebtoken`.
#[derive(serde::Deserialize)]
pub struct Claims {
    pub sub: String,
    #[serde(default)]
    pub azp: Option<String>,
    /// The address this account signs in with, when the realm sends one. Its key is the spec's
    /// spelling, which is why this file is excluded from the vocabulary guard in CI.
    #[serde(default)]
    pub email: Option<String>,
    /// Whether the realm considers that address verified.
    ///
    /// Worth knowing exactly what this means before trusting it: for a Google-brokered account the
    /// realm sets it from Google's assertion alone, because the provider is configured with
    /// `trustEmail`. It is "Google says so, and we trust Google", not an independent check — which
    /// is a strong statement for a mailbox Google controls and a weaker one than it looks in
    /// general. Providers without `trustEmail` are verified by the realm itself.
    #[serde(default)]
    pub email_verified: Option<bool>,
}

/// Resource-bound OAuth access-token claims. Kept separate from native-client claims so stricter
/// connector validation cannot silently change the authentication contract of shipped apps.
#[derive(serde::Deserialize)]
pub struct ResourceClaims {
    pub sub: String,
    pub azp: String,
    pub scope: String,
    pub typ: String,
}

pub const CHATGPT_CLIENT_ID: &str = "pigeonpost-chatgpt";

impl ResourceClaims {
    pub fn has_scope(&self, required: &str) -> bool {
        self.scope
            .split_ascii_whitespace()
            .any(|scope| scope == required)
    }
}

impl Claims {
    /// The address this account signs in with, whatever the realm's spelling of the claim.
    ///
    /// The rest of the postbox reads addresses through here rather than through the field, so the
    /// spec's word for it stays in the one file that exists to speak the spec.
    pub fn address(&self) -> Option<&str> {
        self.email
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty())
    }

    /// Whether the realm considers that address verified. See the field's own note for exactly how
    /// much that is worth for a brokered account.
    pub fn address_is_verified(&self) -> bool {
        self.email_verified == Some(true)
    }
}

#[derive(serde::Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(serde::Deserialize)]
struct Jwk {
    kty: String,
    kid: Option<String>,
    n: Option<String>,
    e: Option<String>,
}

/// Validates realm member JWTs, caching signing keys by `kid`.
pub struct Oidc {
    issuer: String,
    jwks_url: String,
    http: reqwest::Client,
    keys: RwLock<HashMap<String, (String, String)>>, // kid -> (n, e), base64url
}

impl Oidc {
    pub fn new(issuer: String) -> Self {
        let jwks_url = format!(
            "{}/protocol/openid-connect/certs",
            issuer.trim_end_matches('/')
        );
        Oidc {
            issuer,
            jwks_url,
            http: reqwest::Client::new(),
            keys: RwLock::new(HashMap::new()),
        }
    }

    /// Validate a member token and return its claims.
    pub async fn validate(&self, token: &str) -> Result<Claims, OidcError> {
        let kid = decode_header(token)
            .map_err(|_| OidcError::Malformed)?
            .kid
            .ok_or(OidcError::Malformed)?;
        let key = self.key_for_kid(&kid).await?;
        validate_token(token, &key, &self.issuer)
    }

    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    pub async fn validate_resource(
        &self,
        token: &str,
        resource: &str,
        client: &str,
    ) -> Result<ResourceClaims, OidcError> {
        let kid = decode_header(token)
            .map_err(|_| OidcError::Malformed)?
            .kid
            .ok_or(OidcError::Malformed)?;
        let key = self.key_for_kid(&kid).await?;
        validate_resource_token(token, &key, &self.issuer, resource, client)
    }

    async fn key_for_kid(&self, kid: &str) -> Result<DecodingKey, OidcError> {
        if let Some((n, e)) = self.keys.read().await.get(kid).cloned() {
            return DecodingKey::from_rsa_components(&n, &e).map_err(|_| OidcError::Key);
        }
        // Cache miss — the realm may have rotated keys; refresh once and retry.
        self.refresh_jwks().await?;
        let guard = self.keys.read().await;
        let (n, e) = guard.get(kid).ok_or(OidcError::UnknownKid)?;
        DecodingKey::from_rsa_components(n, e).map_err(|_| OidcError::Key)
    }

    async fn refresh_jwks(&self) -> Result<(), OidcError> {
        let jwks: Jwks = self
            .http
            .get(&self.jwks_url)
            .send()
            .await
            .map_err(|_| OidcError::Fetch)?
            .json()
            .await
            .map_err(|_| OidcError::Fetch)?;
        let mut cache = self.keys.write().await;
        for k in jwks.keys {
            if k.kty == "RSA" {
                if let (Some(kid), Some(n), Some(e)) = (k.kid, k.n, k.e) {
                    cache.insert(kid, (n, e));
                }
            }
        }
        Ok(())
    }
}

/// Pure validation: signature (via `key`), issuer, and expiry. Audience is deliberately not required.
fn validate_token(token: &str, key: &DecodingKey, issuer: &str) -> Result<Claims, OidcError> {
    let mut v = Validation::new(Algorithm::RS256);
    v.set_issuer(&[issuer]);
    v.validate_aud = false;
    let claims = decode::<Claims>(token, key, &v)
        .map(|d| d.claims)
        .map_err(|_| OidcError::Invalid)?;
    // A limited connector token must not bypass its scopes through the general REST/MCP routes.
    // Every other shipped client retains the existing realm-token validation behavior.
    if claims.azp.as_deref() == Some(CHATGPT_CLIENT_ID) {
        return Err(OidcError::Invalid);
    }
    Ok(claims)
}

fn validate_resource_token(
    token: &str,
    key: &DecodingKey,
    issuer: &str,
    resource: &str,
    client: &str,
) -> Result<ResourceClaims, OidcError> {
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(&[issuer]);
    validation.set_audience(&[resource]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    validation.validate_nbf = true;
    let claims = decode::<ResourceClaims>(token, key, &validation)
        .map_err(|_| OidcError::Invalid)?
        .claims;
    // Keycloak access tokens use typ=Bearer; an ID token must never authorize mailbox tools.
    if claims.sub.trim().is_empty() || claims.azp != client || claims.typ != "Bearer" {
        return Err(OidcError::Invalid);
    }
    Ok(claims)
}

#[cfg(test)]
impl Claims {
    pub(crate) fn fixture(sub: &str, address: &str, verified: bool) -> Self {
        Self {
            sub: sub.into(),
            azp: None,
            email: Some(address.into()),
            email_verified: Some(verified),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{encode, EncodingKey, Header};
    use std::time::{SystemTime, UNIX_EPOCH};

    const PRIV: &str = "-----BEGIN PRIVATE KEY-----
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQDak6176XTrm5oT
Hfo6ke+fUgrQhhoaU0YxUy8sf23hhFE2QYoV1/Vfbs0dqFBz9Qmqq0t/Vsh8VqIX
V4G41qp5ELhGOeFan8SNXPXYBDwUudMHaJSHxtcKJCAEY8zMDmoyO2uuamwS+gzm
Q977tpamCNmfUqtlF1rdUi/5sj6bWSOfvKlUnEpPtt0jJ5x6MlAbLFi12NpAXpU8
m71S0XNonzvdB1wa9IeGrg4J1k69uekU3aU+JkEuc9W8GKuN1XGwY6m06qBDDaLc
SQA15ZJyndMPDQFvF6uftXcSaBkvOQVhkOj0RbU6wf/H+vqxdhRo8mAHg1b3VsZg
GVqN0QqzAgMBAAECggEAAxhEn8cFpv7uhHl9r3J40OC/bk8fxeuBf0e4dDMx+yn0
YLOkboV1oAUT9fsunG/8r0QoWCRwNUQywTG1x2y1iVm4U12nJfpo/hux6yJwNw17
XadGEEAW2Keyt397GzVl5fZSa8dJiypie6hzEd+/vnPxMpepIgbLahBiV6fzgI1s
MzMHskmp6j/2XCg2owhqHJ5xLAB+K3MdB3SUH5Z7wT62gUS8Jobokgb0CpGBffBM
dXr1R2Ce2GW1Aq/oGyBw6oSnfkUjqmWO5NDbaXWH1Ru8s+PdzBmjtJ/tnSn2T3sy
JSTkXNzHrcDcOtvbEFdEW6SPL+v1Ef+1JSVu40j8xQKBgQD6Q7LOpDm/JVXBnROV
edfW/pKYe6Chu/khxG7Hkg7KYigDE4zkP2NH+V9Ipk4ymPQ2+eGvkw1JoaVsdcnD
0cMOo5YUIg9XX7p/9XghEShzBmnVrC5HJi913q1tpGF0ow7jk3U0+Pq30OsOUP8P
UJo9ArP05hgXaW4RnEEJjqmq7wKBgQDflhFzOVIcrayrUxQksH2gxUTHBY8s2O9/
QRtweaHBW7PtD+uLKAendaJiXERTFFYM5IAddetTX/X3gIDHaF4f8VpcRRmIqoye
BQ3vyWDMez0+ngIbdszLIY28KDTcO4GhizdKHDOvpQF+NA8JLbqkITKA/VtJ5HCj
MC+4++CsfQKBgQCJwzJupeBT5E1sovbg1Y2G/+PapFMsNLlTaTpCCZiyt42nA+AO
1QXl3NQivclV+PSWPr+JUr2BxsW1CrHiZVmmeU5oDse7JSsYvRs/uJ43k1Q3Fuzy
pYaCr+1v6YjsF8ZeaBGg812wSgTagKOm3ovJAe/l47NnT9YTQ5xZknq7aQKBgC9C
zGt7uVSgjXgldoOO3u9F45TiIvKK5I0UmRU8UKnLlYvNqq9ehcerAOkjsbmR+eJ9
xmrzywtzpE1t10rPT94WqVAJty0BR/n6/YgrHA/9GOQMiEt/4Cgr7obQROQsm+km
wUgkD/TXvyoLHQaGqQYakk9bvpku9XQ5Mk06yLINAoGBAPLffdt6UiTN42pPEDpn
XRAUj4P7Zv9S/s0JuVDaSovG50QIpxt8Sp/aXPB0Kf9AlSe78b2haunlalnMpxDh
8E3OczNySSSJa9KLcIVZIDKWrJj5XSLhgvSlANOKb/9XVNMja76vXdL4WvI1qeer
2unJ/wCh/b+GdbaHbMlJt0d4
-----END PRIVATE KEY-----
";

    const PUB: &str = "-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA2pOte+l065uaEx36OpHv
n1IK0IYaGlNGMVMvLH9t4YRRNkGKFdf1X27NHahQc/UJqqtLf1bIfFaiF1eBuNaq
eRC4RjnhWp/EjVz12AQ8FLnTB2iUh8bXCiQgBGPMzA5qMjtrrmpsEvoM5kPe+7aW
pgjZn1KrZRda3VIv+bI+m1kjn7ypVJxKT7bdIyecejJQGyxYtdjaQF6VPJu9UtFz
aJ873QdcGvSHhq4OCdZOvbnpFN2lPiZBLnPVvBirjdVxsGOptOqgQw2i3EkANeWS
cp3TDw0Bbxern7V3EmgZLzkFYZDo9EW1OsH/x/r6sXYUaPJgB4NW91bGYBlajdEK
swIDAQAB
-----END PUBLIC KEY-----
";

    #[derive(serde::Serialize)]
    struct TestClaims {
        sub: String,
        iss: String,
        exp: usize,
    }

    fn sign(sub: &str, iss: &str, exp: usize) -> String {
        let mut h = Header::new(Algorithm::RS256);
        h.kid = Some("test-kid".into());
        let claims = TestClaims {
            sub: sub.into(),
            iss: iss.into(),
            exp,
        };
        encode(
            &h,
            &claims,
            &EncodingKey::from_rsa_pem(PRIV.as_bytes()).unwrap(),
        )
        .unwrap()
    }

    fn now() -> usize {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as usize
    }

    #[test]
    fn accepts_valid_rejects_bad() {
        let key = DecodingKey::from_rsa_pem(PUB.as_bytes()).unwrap();
        let iss = "https://auth.pigeonpost.dev/realms/pigeonpost-prod";

        let good = sign("user-123", iss, now() + 3600);
        assert_eq!(validate_token(&good, &key, iss).unwrap().sub, "user-123");

        // wrong issuer
        assert!(validate_token(&good, &key, "https://evil.example/realms/x").is_err());
        // expired (well beyond jsonwebtoken's default 60s leeway)
        let expired = sign("user-123", iss, now() - 3600);
        assert!(validate_token(&expired, &key, iss).is_err());
        // tampered signature: flip a character inside the signature segment
        let mut chars: Vec<char> = good.chars().collect();
        let i = chars.len() - 5;
        chars[i] = if chars[i] == 'A' { 'B' } else { 'A' };
        let tampered: String = chars.into_iter().collect();
        assert!(validate_token(&tampered, &key, iss).is_err());
    }

    #[test]
    fn chatgpt_tokens_are_resource_client_and_time_bound() {
        let key = DecodingKey::from_rsa_pem(PUB.as_bytes()).unwrap();
        let signing = EncodingKey::from_rsa_pem(PRIV.as_bytes()).unwrap();
        let issuer = "https://auth.example/realms/test";
        let resource = crate::chatgpt::RESOURCE;
        let base = serde_json::json!({
            "sub": "member-one", "iss": issuer, "exp": now() + 3600,
            "aud": [resource], "azp": CHATGPT_CLIENT_ID,
            "scope": "openid pigeonpost:read", "typ": "Bearer",
        });
        let sign_value = |value: &serde_json::Value| {
            encode(&Header::new(Algorithm::RS256), value, &signing).unwrap()
        };
        let valid = sign_value(&base);
        let claims =
            validate_resource_token(&valid, &key, issuer, resource, CHATGPT_CLIENT_ID).unwrap();
        assert_eq!(claims.sub, "member-one");
        assert!(claims.has_scope("pigeonpost:read"));
        assert!(!claims.has_scope("pigeonpost:write"));
        assert!(!claims.has_scope("read"));
        assert!(
            validate_token(&valid, &key, issuer).is_err(),
            "scoped tokens cannot enter generic APIs"
        );

        for (field, value) in [
            ("aud", serde_json::json!("another-resource")),
            ("azp", serde_json::json!("pigeonpost-web")),
            ("iss", serde_json::json!("https://wrong-issuer.example")),
            ("exp", serde_json::json!(now() - 120)),
            ("nbf", serde_json::json!(now() + 3600)),
            ("sub", serde_json::json!("")),
            ("typ", serde_json::json!("ID")),
        ] {
            let mut value_to_sign = base.clone();
            value_to_sign[field] = value;
            assert!(
                validate_resource_token(
                    &sign_value(&value_to_sign),
                    &key,
                    issuer,
                    resource,
                    CHATGPT_CLIENT_ID
                )
                .is_err(),
                "accepted invalid {field}"
            );
        }
        for field in ["exp", "iss", "aud", "sub", "azp", "scope", "typ"] {
            let mut missing = base.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(
                validate_resource_token(
                    &sign_value(&missing),
                    &key,
                    issuer,
                    resource,
                    CHATGPT_CLIENT_ID
                )
                .is_err(),
                "accepted missing {field}"
            );
        }
        let hs = encode(
            &Header::new(Algorithm::HS256),
            &base,
            &EncodingKey::from_secret(b"fixture-only"),
        )
        .unwrap();
        assert!(validate_resource_token(&hs, &key, issuer, resource, CHATGPT_CLIENT_ID).is_err());
        let mut normal = base.clone();
        normal["azp"] = serde_json::json!("pigeonpost-web");
        assert!(
            validate_token(&sign_value(&normal), &key, issuer).is_ok(),
            "native/web token behavior stays unchanged"
        );
    }
}
