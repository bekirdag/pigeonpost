//! Who is calling, and how they proved it (SDS §3.2).
//!
//! An account id alone does not say whether a person is at the keyboard: the same account can be
//! driven by its owner's interactive sign-in, by an account API key in a script, or by an OAuth
//! connector acting on the owner's behalf. Those are different principals with different powers,
//! so the credential that resolved the account travels with it.
//!
//! Nothing here changes what an existing endpoint accepts. The provenance is additional
//! information; the new gates ([`require_human`], [`require_step_up`]) are only applied by code
//! that asks for them.

use crate::{oidc, store, ApiError};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

/// How long a strong sign-in counts as recent, by default (SDS §3.4).
pub(crate) const DEFAULT_STEP_UP_MAX_AGE: u64 = 600;

/// Clock skew tolerated on `auth_time`, so a token minted a moment "in the future" by a realm whose
/// clock runs slightly ahead is not refused as nonsense.
const AUTH_TIME_SKEW: u64 = 60;

/// Who a bearer token authenticates: a single identity (capability token) or a whole account,
/// together with the kind of credential that resolved the account.
// One variant carries a whole StoredIdentity and the other an account id, so the enum is as big as
// the identity. That is fine here: a Principal is resolved once per request, moved, and dropped —
// it is never held in bulk, so boxing would trade a real allocation for a notional saving.
#[allow(clippy::large_enum_variant)]
pub(crate) enum Principal {
    /// A mailbox capability token (`cap_…`): exactly one identity, always automation.
    Identity(store::StoredIdentity),
    /// An account, and the credential that proved it.
    Account(String, AccountCredential),
}

/// The credential that resolved an account.
#[derive(Debug, Clone)]
pub(crate) enum AccountCredential {
    /// An account API key (`pk_live_…`). Automation, even when the account's owner holds it.
    ApiKey,
    /// A validated realm JWT.
    Member(Box<MemberSession>),
    /// A resource-bound OAuth connector token (the ChatGPT app). Acts for the member, but is not
    /// the member at a keyboard.
    Connector { client: String },
}

/// What was proved by a validated realm JWT. Only facts the validator checked or the token
/// carried are kept; nothing is inferred.
#[derive(Debug, Clone, Default)]
pub(crate) struct MemberSession {
    pub issuer: String,
    /// `azp`: the client the token was issued to (`pigeonpost-web`, `pigeonpost-cli`, …).
    pub client: Option<String>,
    pub audience: Vec<String>,
    pub auth_time: Option<u64>,
    pub acr: Option<String>,
    pub amr: Vec<String>,
    /// A client-credentials token minted for a Keycloak service account. Signed by the same realm,
    /// but nobody signed in.
    pub service_account: bool,
    /// The token carried a sign-in address the realm marks verified (Phase 3B: needed before a
    /// person may send calendar invitations to contact addresses, SDS §14B.6).
    pub address_verified: bool,
}

impl MemberSession {
    /// Provenance from claims that `oidc::Oidc::validate` has already accepted. `issuer` is the
    /// issuer the validator enforced, which is the one this deployment trusts.
    pub(crate) fn from_claims(claims: &oidc::Claims, issuer: &str) -> Self {
        let service_account = claims.client_id.is_some()
            || claims
                .preferred_username
                .as_deref()
                .is_some_and(|u| u.starts_with("service-account-"));
        MemberSession {
            issuer: claims.iss.clone().unwrap_or_else(|| issuer.to_string()),
            client: claims.azp.clone(),
            audience: claims.aud.clone(),
            auth_time: claims.auth_time,
            acr: claims.acr.clone(),
            amr: claims.amr.clone(),
            service_account,
            address_verified: claims.address().is_some() && claims.address_is_verified(),
        }
    }

    /// A person signed in interactively (or through a device grant they approved).
    pub(crate) fn is_interactive(&self) -> bool {
        !self.service_account
    }
}

/// The short, stable name of a credential kind, as `GET /v1/capabilities` reports it.
pub(crate) fn credential_kind(principal: &Principal) -> &'static str {
    match principal {
        Principal::Identity(_) => "mailbox",
        Principal::Account(_, AccountCredential::ApiKey) => "api_key",
        Principal::Account(_, AccountCredential::Member(_)) => "member",
        Principal::Account(_, AccountCredential::Connector { .. }) => "connector",
    }
}

/// Step-up configuration (SDS §3.4): which `acr` values count as strong authentication for this
/// deployment's issuer, and how recent the sign-in must be.
///
/// `acr` is an identifier, never a number: the allowlist is matched exactly. An empty allowlist —
/// the default — means step-up is not configured, and every gated action is refused rather than
/// waved through.
#[derive(Debug, Clone)]
pub(crate) struct StepUpPolicy {
    pub acr_values: Vec<String>,
    pub max_age: u64,
}

impl Default for StepUpPolicy {
    fn default() -> Self {
        StepUpPolicy {
            acr_values: Vec::new(),
            max_age: DEFAULT_STEP_UP_MAX_AGE,
        }
    }
}

impl StepUpPolicy {
    /// `POSTBOX_STEP_UP_ACR_VALUES` (comma or space separated) and `POSTBOX_STEP_UP_MAX_AGE`.
    pub(crate) fn from_env() -> Self {
        let acr_values = std::env::var("POSTBOX_STEP_UP_ACR_VALUES")
            .ok()
            .map(|v| parse_acr_list(&v))
            .unwrap_or_default();
        let max_age = std::env::var("POSTBOX_STEP_UP_MAX_AGE")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_STEP_UP_MAX_AGE);
        StepUpPolicy {
            acr_values,
            max_age,
        }
    }

    pub(crate) fn configured(&self) -> bool {
        !self.acr_values.is_empty()
    }
}

fn parse_acr_list(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for value in raw.split(|c: char| c == ',' || c.is_whitespace()) {
        let value = value.trim();
        // A quote or backslash would break the challenge header; such a value cannot be a
        // realm's acr identifier anyway.
        if value.is_empty() || value.contains(['"', '\\']) || out.iter().any(|v| v == value) {
            continue;
        }
        out.push(value.to_string());
    }
    out
}

/// Why a human-only or step-up gate refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AuthDenial {
    /// The credential is automation (API key, capability token, connector, service account).
    HumanRequired,
    /// The deployment has no strong-auth `acr` configured, so nothing can satisfy the gate.
    StepUpUnconfigured,
    /// A person, but not recently or not strongly enough. Answered with an RFC 9470 challenge.
    StepUpRequired {
        acr_values: Vec<String>,
        max_age: u64,
    },
}

impl AuthDenial {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            AuthDenial::HumanRequired => "human_required",
            AuthDenial::StepUpUnconfigured => "step_up_unavailable",
            AuthDenial::StepUpRequired { .. } => "step_up_required",
        }
    }

    /// The RFC 9470 `WWW-Authenticate` value for a step-up challenge.
    pub(crate) fn challenge(&self) -> Option<String> {
        match self {
            AuthDenial::StepUpRequired {
                acr_values,
                max_age,
            } => Some(format!(
                "Bearer error=\"insufficient_user_authentication\", \
                 error_description=\"A recent strong sign-in is required\", \
                 acr_values=\"{}\", max_age={max_age}",
                acr_values.join(" ")
            )),
            _ => None,
        }
    }
}

impl IntoResponse for AuthDenial {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AuthDenial::HumanRequired => (
                StatusCode::FORBIDDEN,
                "this action needs a person signed in; API keys, mailbox tokens and connectors cannot perform it",
            ),
            AuthDenial::StepUpUnconfigured => (
                StatusCode::FORBIDDEN,
                "this action needs strong sign-in, which this server has not been configured to accept",
            ),
            AuthDenial::StepUpRequired { .. } => (
                StatusCode::UNAUTHORIZED,
                "sign in again with strong authentication, then retry",
            ),
        };
        let challenge = self.challenge();
        let mut resp = ApiError::new(status, self.code(), message).into_response();
        if let Some(value) = challenge.and_then(|c| HeaderValue::from_str(&c).ok()) {
            resp.headers_mut().insert(header::WWW_AUTHENTICATE, value);
        }
        resp
    }
}

/// The member session behind this principal, if a person signed in interactively.
pub(crate) fn require_human(principal: &Principal) -> Result<&MemberSession, AuthDenial> {
    match principal {
        Principal::Account(_, AccountCredential::Member(session)) if session.is_interactive() => {
            Ok(session)
        }
        _ => Err(AuthDenial::HumanRequired),
    }
}

/// A person who authenticated strongly (an allowlisted `acr`) within `max_age` seconds.
///
/// Automation is refused outright: re-authenticating cannot turn an API key into a person. An
/// empty allowlist refuses everyone, because "no strong method configured" must not read as
/// "any method is strong enough".
pub(crate) fn require_step_up<'a>(
    principal: &'a Principal,
    max_age: u64,
    acr_allowlist: &[String],
    now: u64,
) -> Result<&'a MemberSession, AuthDenial> {
    let session = require_human(principal)?;
    if acr_allowlist.is_empty() {
        return Err(AuthDenial::StepUpUnconfigured);
    }
    let recent = session.auth_time.is_some_and(|t| {
        t <= now.saturating_add(AUTH_TIME_SKEW) && now.saturating_sub(t) <= max_age
    });
    let strong = session
        .acr
        .as_deref()
        .is_some_and(|acr| acr_allowlist.iter().any(|allowed| allowed == acr));
    if recent && strong {
        Ok(session)
    } else {
        Err(AuthDenial::StepUpRequired {
            acr_values: acr_allowlist.to_vec(),
            max_age,
        })
    }
}

#[cfg(test)]
pub(crate) fn member_principal(account: &str, session: MemberSession) -> Principal {
    Principal::Account(account.into(), AccountCredential::Member(Box::new(session)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn strong() -> Vec<String> {
        vec!["gold".into(), "urn:pigeonpost:acr:mfa".into()]
    }

    fn session(acr: Option<&str>, auth_time: Option<u64>) -> MemberSession {
        MemberSession {
            issuer: "https://auth.example/realms/r".into(),
            client: Some("pigeonpost-web".into()),
            auth_time,
            acr: acr.map(str::to_string),
            ..MemberSession::default()
        }
    }

    #[test]
    fn provenance_is_read_from_validated_claims() {
        let claims: oidc::Claims = serde_json::from_value(serde_json::json!({
            "sub": "s", "azp": "pigeonpost-cli", "iss": "https://auth.example/realms/r",
            "aud": "account", "auth_time": 42, "acr": "1", "amr": ["pwd", "otp"]
        }))
        .unwrap();
        let s = MemberSession::from_claims(&claims, "ignored");
        assert_eq!(s.issuer, "https://auth.example/realms/r");
        assert_eq!(s.client.as_deref(), Some("pigeonpost-cli"));
        assert_eq!(s.audience, vec!["account"]);
        assert_eq!(s.auth_time, Some(42));
        assert_eq!(s.acr.as_deref(), Some("1"));
        assert_eq!(s.amr, vec!["pwd", "otp"]);
        assert!(s.is_interactive());

        let many: oidc::Claims =
            serde_json::from_value(serde_json::json!({"sub": "s", "aud": ["a", "b"]})).unwrap();
        assert_eq!(many.aud, vec!["a", "b"]);
    }

    #[test]
    fn service_account_tokens_are_not_people() {
        for claims in [
            serde_json::json!({"sub": "s", "preferred_username": "service-account-billing"}),
            serde_json::json!({"sub": "s", "client_id": "billing"}),
        ] {
            let claims: oidc::Claims = serde_json::from_value(claims).unwrap();
            let s = MemberSession::from_claims(&claims, "iss");
            assert!(!s.is_interactive());
            let p = member_principal("acct", s);
            assert_eq!(require_human(&p).unwrap_err(), AuthDenial::HumanRequired);
        }
    }

    #[test]
    fn automation_credentials_are_never_human() {
        let api = Principal::Account("acct".into(), AccountCredential::ApiKey);
        let conn = Principal::Account(
            "acct".into(),
            AccountCredential::Connector {
                client: "pigeonpost-chatgpt".into(),
            },
        );
        for p in [&api, &conn] {
            assert_eq!(require_human(p).unwrap_err(), AuthDenial::HumanRequired);
            // Even with step-up configured, re-authenticating cannot help an API key.
            assert_eq!(
                require_step_up(p, 600, &strong(), NOW).unwrap_err(),
                AuthDenial::HumanRequired
            );
        }
        assert_eq!(credential_kind(&api), "api_key");
        assert_eq!(credential_kind(&conn), "connector");
        let human = member_principal("acct", session(None, None));
        assert!(require_human(&human).is_ok());
        assert_eq!(credential_kind(&human), "member");
    }

    #[test]
    fn step_up_needs_a_recent_allowlisted_acr() {
        let ok = member_principal("a", session(Some("gold"), Some(NOW - 30)));
        assert!(require_step_up(&ok, 600, &strong(), NOW).is_ok());

        // Stale sign-in.
        let stale = member_principal("a", session(Some("gold"), Some(NOW - 601)));
        // Not an allowlisted method — and acr is matched exactly, never compared as a number.
        let weak = member_principal("a", session(Some("1"), Some(NOW)));
        let goldish = member_principal("a", session(Some("gold2"), Some(NOW)));
        // No auth_time at all.
        let timeless = member_principal("a", session(Some("gold"), None));
        // auth_time far in the future is nonsense, not "very recent".
        let future = member_principal("a", session(Some("gold"), Some(NOW + 3600)));
        for p in [&stale, &weak, &goldish, &timeless, &future] {
            let denial = require_step_up(p, 600, &strong(), NOW).unwrap_err();
            assert_eq!(denial.code(), "step_up_required");
        }
    }

    #[test]
    fn empty_allowlist_refuses_instead_of_allowing() {
        let p = member_principal("a", session(Some("gold"), Some(NOW)));
        assert_eq!(
            require_step_up(&p, 600, &[], NOW).unwrap_err(),
            AuthDenial::StepUpUnconfigured
        );
        assert!(!StepUpPolicy::default().configured());
    }

    #[tokio::test]
    async fn step_up_challenge_follows_rfc_9470() {
        let p = member_principal("a", session(Some("1"), Some(NOW)));
        let denial = require_step_up(&p, 600, &strong(), NOW).unwrap_err();
        let resp = denial.into_response();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let challenge = resp
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(challenge.starts_with("Bearer error=\"insufficient_user_authentication\""));
        assert!(challenge.contains("acr_values=\"gold urn:pigeonpost:acr:mfa\""));
        assert!(challenge.ends_with("max_age=600"));
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"], "step_up_required");

        let unconfigured = AuthDenial::StepUpUnconfigured.into_response();
        assert_eq!(unconfigured.status(), StatusCode::FORBIDDEN);
        assert!(unconfigured
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .is_none());
    }

    #[test]
    fn acr_list_parsing_is_exact_and_safe() {
        assert_eq!(
            parse_acr_list(" gold, urn:x:mfa  gold \"bad\" "),
            vec!["gold".to_string(), "urn:x:mfa".to_string()]
        );
        assert!(parse_acr_list("").is_empty());
    }
}
