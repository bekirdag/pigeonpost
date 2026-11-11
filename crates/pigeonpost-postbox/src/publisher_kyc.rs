//! Shufti Pro KYC for publisher registration (developer program plan §2 "Shufti Pro").
//!
//! Individuals, sole proprietors and the authorised signatory of a company verify themselves in
//! Shufti Pro's hosted flow: document + face + AML screening, address optional
//! (`POSTBOX_SHUFTI_ADDRESS=on`). The postbox creates the verification request, the page shows the
//! returned `verification_url`, and Shufti reports the outcome to
//! `POST /v1/integrations/shufti/callback`. The worker polls `POST /status` as a fallback for
//! checks that stay pending or whose status changed at Shufti.
//!
//! **Configuration.** With both Shufti credentials the provider is live. Without them KYC is
//! **unavailable**: nobody who needs it can submit, and the page says verification is temporarily
//! unavailable. The **sandbox** (mock) provider, whose "simulate accepted/declined" goes through the
//! same signed-message path, exists only with `POSTBOX_KYC_SANDBOX=on` on a staging postbox
//! (`POSTBOX_STAGING_MODE=1`); on production the flag is ignored. A credential file that is named but
//! unreadable or empty, or only one of the two credentials, is a configuration error: the postbox
//! refuses to start ([`check_config`]) and, should it change underneath, KYC is unavailable.
//!
//! | Variable | Meaning |
//! |---|---|
//! | `POSTBOX_SHUFTI_CLIENT_ID_FILE` / `POSTBOX_SHUFTI_CLIENT_ID` | the account's client id |
//! | `POSTBOX_SHUFTI_SECRET_KEY_FILE` / `POSTBOX_SHUFTI_SECRET_KEY` | the secret key (HTTP Basic password, and the signature key) |
//! | `POSTBOX_SHUFTI_BASE_URL` | default `https://api.shuftipro.com` |
//! | `POSTBOX_SHUFTI_SIGNATURE` | `hashed_secret` (default: `sha256(body + sha256(secret))`, accounts registered from 2023-03-15) or `plain_secret` (`sha256(body + secret)`, older accounts) |
//! | `POSTBOX_SHUFTI_ADDRESS` | `on` adds the address check |
//! | `POSTBOX_SHUFTI_REDIRECT_URL` | where the hosted flow returns; default `https://pigeonpost.dev/account#publish` |
//!
//! The callback URL is `{POSTBOX_PUBLIC_URL}/v1/integrations/shufti/callback`; register it in the
//! Shufti back office too.
//!
//! **What is stored** (and nothing else): the reference, the latest event and result, declined
//! reasons and codes, the AML outcome, and the verified name, date of birth and country. Images
//! and the rest of `verification_data` are never written down.
//!
//! **Authenticity and replay**: every callback and every API response must carry a valid
//! `Signature` header (constant-time compared). A body already seen (by digest) changes nothing.
//! Only references this postbox created are accepted, and a final result never regresses: a
//! later `verification.status.changed` only schedules a status read, which is itself signed.

use crate::groups::{GErr, GResult};
use crate::{now_unix, AppState};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Vault label of the mock provider's signing key.
const MOCK_LABEL: &str = "publisher-kyc-mock-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scheme {
    /// `sha256(body + sha256_hex(secret))`, accounts registered from 2023-03-15.
    HashedSecret,
    /// `sha256(body + secret)`, older accounts.
    PlainSecret,
}

#[derive(Clone)]
pub(crate) enum Provider {
    Shufti {
        client_id: String,
        secret: String,
        base_url: String,
        scheme: Scheme,
        address: bool,
        redirect_url: String,
    },
    /// The sandbox: staging postboxes with `POSTBOX_KYC_SANDBOX=on` only.
    Mock { secret: String },
    /// No usable provider: nothing can be verified, and no message is accepted.
    Unavailable { why: &'static str },
}

impl Provider {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Provider::Shufti { .. } => "shufti",
            Provider::Mock { .. } => "mock",
            Provider::Unavailable { .. } => "unavailable",
        }
    }

    /// The key a provider message is signed with; none while unavailable.
    fn signing_secret(&self) -> Option<&str> {
        match self {
            Provider::Shufti { secret, .. } | Provider::Mock { secret } => Some(secret),
            Provider::Unavailable { .. } => None,
        }
    }

    fn scheme(&self) -> Scheme {
        match self {
            Provider::Shufti { scheme, .. } => *scheme,
            _ => Scheme::HashedSecret,
        }
    }
}

/// A credential from `<NAME>_FILE` (preferred) or `<NAME>`. `Ok(None)`: not configured.
/// `Err`: a file is named but cannot be read or is empty. Never silently "not configured".
pub(crate) fn secret_from_env(name: &str) -> Result<Option<String>, String> {
    let file = std::env::var(format!("{name}_FILE")).unwrap_or_default();
    if !file.trim().is_empty() {
        let v = std::fs::read_to_string(file.trim())
            .map_err(|e| format!("{name}_FILE could not be read: {e}"))?;
        let v = v.trim().to_string();
        if v.is_empty() {
            return Err(format!("{name}_FILE is empty"));
        }
        return Ok(Some(v));
    }
    let v = std::env::var(name).unwrap_or_default().trim().to_string();
    Ok((!v.is_empty()).then_some(v))
}

/// Whether this postbox may run the sandbox provider: staging only, and only when asked for.
pub(crate) fn sandbox_allowed(state: &AppState) -> bool {
    state.nests.mp.staging && state.nests.mp.kyc_sandbox
}

/// The live credentials, `Ok(None)` when none are configured, `Err` when they are broken.
fn credentials() -> Result<Option<(String, String)>, String> {
    match (
        secret_from_env("POSTBOX_SHUFTI_CLIENT_ID")?,
        secret_from_env("POSTBOX_SHUFTI_SECRET_KEY")?,
    ) {
        (Some(id), Some(secret)) => Ok(Some((id, secret))),
        (None, None) => Ok(None),
        _ => Err("only one of the Shufti Pro client id and secret key is configured".into()),
    }
}

/// At startup: a broken Shufti configuration stops the postbox instead of degrading quietly.
pub(crate) fn check_config() -> Result<(), String> {
    credentials().map(|_| ())
}

/// The configured provider: Shufti Pro with both credentials; the sandbox where allowed; else
/// unavailable.
pub(crate) fn provider(state: &AppState) -> Provider {
    match credentials() {
        Ok(Some((client_id, secret))) => Provider::Shufti {
            client_id,
            secret,
            base_url: std::env::var("POSTBOX_SHUFTI_BASE_URL")
                .ok()
                .map(|v| v.trim().trim_end_matches('/').to_string())
                .filter(|v| v.starts_with("https://"))
                .unwrap_or_else(|| "https://api.shuftipro.com".into()),
            scheme: match std::env::var("POSTBOX_SHUFTI_SIGNATURE").as_deref() {
                Ok("plain_secret") => Scheme::PlainSecret,
                _ => Scheme::HashedSecret,
            },
            address: std::env::var("POSTBOX_SHUFTI_ADDRESS").is_ok_and(|v| v == "on"),
            redirect_url: std::env::var("POSTBOX_SHUFTI_REDIRECT_URL")
                .ok()
                .filter(|v| v.starts_with("https://"))
                .unwrap_or_else(|| "https://pigeonpost.dev/account#publish".into()),
        },
        Ok(None) if sandbox_allowed(state) => Provider::Mock {
            secret: hex(&state.vault.subkey(MOCK_LABEL)),
        },
        Ok(None) => Provider::Unavailable {
            why: "not_configured",
        },
        Err(e) => {
            tracing::error!(error = %e, "shufti: credentials unusable; KYC unavailable");
            Provider::Unavailable {
                why: "misconfigured",
            }
        }
    }
}

pub(crate) fn kyc_unavailable() -> GErr {
    GErr::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "kyc_unavailable",
        "identity verification is temporarily unavailable",
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(64), |mut out, b| {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
        out
    })
}

fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

/// The `Signature` Shufti Pro sends with a response or callback body.
pub(crate) fn signature(body: &[u8], secret: &str, scheme: Scheme) -> String {
    let key = match scheme {
        Scheme::HashedSecret => sha256_hex(secret.as_bytes()),
        Scheme::PlainSecret => secret.to_string(),
    };
    let mut h = Sha256::new();
    h.update(body);
    h.update(key.as_bytes());
    hex(&h.finalize())
}

/// Constant-time comparison of a received signature (hex, any case) with the expected one.
pub(crate) fn shufti_signature_ok(
    body: &[u8],
    received: Option<&str>,
    secret: &str,
    scheme: Scheme,
) -> bool {
    let Some(received) = received else {
        return false;
    };
    let expected = signature(body, secret, scheme);
    let received = received.trim().to_ascii_lowercase();
    if received.len() != expected.len() {
        return false;
    }
    received
        .bytes()
        .zip(expected.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// Our result for a Shufti event; `None` for events that say nothing about the outcome.
pub(crate) fn result_of(event: &str) -> Option<&'static str> {
    match event {
        "verification.accepted" => Some("accepted"),
        "verification.declined" => Some("declined"),
        "verification.cancelled" | "request.timeout" => Some("cancelled"),
        "request.invalid" | "request.unauthorized" => Some("error"),
        "request.pending" | "request.received" => Some("pending"),
        _ => None,
    }
}

fn is_final(result: &str) -> bool {
    matches!(result, "accepted" | "declined" | "cancelled" | "error")
}

/// What a signed message lets us keep: the outcome fields and nothing else.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Outcome {
    pub reference: String,
    pub event: String,
    pub declined_reason: Option<String>,
    pub declined_codes: Option<String>,
    pub aml: Option<String>,
    pub name: Option<String>,
    pub dob: Option<String>,
    pub country: Option<String>,
    pub verification_url: Option<String>,
}

fn short(v: &Value, max: usize) -> Option<String> {
    v.as_str()
        .map(|s| {
            s.trim()
                .chars()
                .filter(|c| !c.is_control())
                .take(max)
                .collect::<String>()
        })
        .filter(|s| !s.is_empty())
}

/// Pick the allowed fields out of a Shufti response or callback.
pub(crate) fn outcome(v: &Value) -> Outcome {
    let doc = &v["verification_data"]["document"];
    let name = match short(&doc["name"]["full_name"], 200) {
        Some(n) => Some(n),
        None => {
            let parts: Vec<String> = ["first_name", "middle_name", "last_name"]
                .iter()
                .filter_map(|k| short(&doc["name"][*k], 80))
                .collect();
            (!parts.is_empty()).then(|| parts.join(" "))
        }
    };
    let mut codes: Vec<String> = v["declined_codes"]
        .as_array()
        .map(|a| a.iter().filter_map(|c| short(c, 40)).collect())
        .unwrap_or_default();
    if let Some(m) = v["services_declined_codes"].as_object() {
        for (service, list) in m {
            for c in list.as_array().into_iter().flatten() {
                if let Some(c) = short(c, 40) {
                    codes.push(format!(
                        "{}:{c}",
                        service.chars().take(24).collect::<String>()
                    ));
                }
            }
        }
    }
    codes.truncate(32);
    let aml = match &v["verification_result"]["background_checks"] {
        Value::Number(n) if n.as_i64() == Some(1) => Some("clear".to_string()),
        Value::Number(n) if n.as_i64() == Some(0) => Some("match".to_string()),
        Value::Null => None,
        _ => Some("review".to_string()),
    };
    Outcome {
        reference: short(&v["reference"], 100).unwrap_or_default(),
        event: short(&v["event"], 60).unwrap_or_default(),
        declined_reason: short(&v["declined_reason"], 500),
        declined_codes: (!codes.is_empty()).then(|| codes.join(",")),
        aml,
        name,
        dob: short(&doc["dob"], 10).filter(|d| d.len() == 10 && d.as_bytes()[4] == b'-'),
        country: short(&doc["country"], 2)
            .or_else(|| short(&v["country"], 2))
            .map(|c| c.to_ascii_uppercase())
            .filter(|c| crate::publisher_validate::country_ok(c)),
        verification_url: short(&v["verification_url"], 2000).filter(|u| u.starts_with("https://")),
    }
}

/// Apply one signed provider message to its check, in the caller's transaction. Returns the
/// application id when the check's result changed, `None` for a replay or a message that changes
/// nothing. Unknown references are refused.
pub(crate) fn apply_kyc_message_in(
    tx: &Transaction<'_>,
    body: &[u8],
    o: &Outcome,
    source: &str,
    now: i64,
) -> GResult<Option<String>> {
    let digest = sha256_hex(body);
    let check: Option<(String, String, String)> = tx
        .query_row(
            "SELECT id, application_id, result FROM publisher_kyc_checks WHERE reference = ?1",
            [&o.reference],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((check, application, current)) = check else {
        return Err(GErr::new(
            StatusCode::NOT_FOUND,
            "unknown_reference",
            "no such verification",
        ));
    };
    let fresh = tx.execute(
        "INSERT OR IGNORE INTO publisher_kyc_events (body_digest, check_id, event, source, received_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![digest, check, o.event, source, now],
    )?;
    if fresh == 0 {
        return Ok(None);
    }
    if o.event == "verification.status.changed" {
        // Shufti changed the outcome in its back office: read the status (signed) again.
        tx.execute(
            "UPDATE publisher_kyc_checks SET polled_at = NULL, updated_at = ?2 WHERE id = ?1",
            params![check, now],
        )?;
        return Ok(None);
    }
    let Some(result) = result_of(&o.event) else {
        return Ok(None);
    };
    // A final outcome stays; a status read after `verification.status.changed` is the one way to
    // change it, and it arrives as source `status`.
    if is_final(&current) && source != "status" {
        return Ok(None);
    }
    if current == result && result == "pending" {
        return Ok(None);
    }
    tx.execute(
        "UPDATE publisher_kyc_checks SET event = ?2, result = ?3,
            declined_reason = ?4, declined_codes = ?5, aml_result = COALESCE(?6, aml_result),
            verified_name = COALESCE(?7, verified_name), verified_dob = COALESCE(?8, verified_dob),
            verified_country = COALESCE(?9, verified_country),
            final_at = CASE WHEN ?10 THEN ?11 ELSE final_at END,
            verification_url = CASE WHEN ?10 THEN NULL ELSE verification_url END,
            polled_at = ?11, updated_at = ?11
          WHERE id = ?1",
        params![
            check,
            o.event,
            result,
            o.declined_reason,
            o.declined_codes,
            o.aml,
            o.name,
            o.dob,
            o.country,
            is_final(result),
            now
        ],
    )?;
    Ok(Some(application))
}

// ---- the provider API ------------------------------------------------------------------------------

fn client() -> Option<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .ok()
}

/// One signed request to Shufti Pro. A response without a valid signature is not trusted.
async fn shufti_call(
    p: &Provider,
    path: &str,
    body: &Value,
) -> Result<(Vec<u8>, Value), &'static str> {
    let Provider::Shufti {
        client_id,
        secret,
        base_url,
        scheme,
        ..
    } = p
    else {
        return Err("not_configured");
    };
    let client = client().ok_or("client")?;
    let resp = client
        .post(format!("{base_url}{path}"))
        .basic_auth(client_id, Some(secret))
        .json(body)
        .send()
        .await
        .map_err(|_| "unreachable")?;
    let status = resp.status();
    let sig = resp
        .headers()
        .get("signature")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = resp.bytes().await.map_err(|_| "unreachable")?.to_vec();
    if !shufti_signature_ok(&bytes, sig.as_deref(), secret, *scheme) {
        return Err("bad_signature");
    }
    let v: Value = serde_json::from_slice(&bytes).map_err(|_| "bad_body")?;
    if !status.is_success() && v["event"].as_str().is_none() {
        return Err("refused");
    }
    Ok((bytes, v))
}

/// What a new verification request asks for.
pub(crate) struct Start<'a> {
    pub reference: &'a str,
    pub email: Option<&'a str>,
    pub country: Option<&'a str>,
    pub language: &'a str,
    pub callback_url: String,
}

/// The request body for a hosted (onsite) verification: no proofs, so Shufti captures them, and
/// empty fields, so its OCR returns the verified name and date of birth.
pub(crate) fn request_body(p: &Provider, s: &Start<'_>) -> Value {
    let (address, redirect) = match p {
        Provider::Shufti {
            address,
            redirect_url,
            ..
        } => (*address, redirect_url.clone()),
        _ => (false, String::new()),
    };
    let mut body = json!({
        "reference": s.reference,
        "callback_url": s.callback_url,
        "redirect_url": redirect,
        "language": s.language,
        "verification_mode": "any",
        "allow_retry": "1",
        "ttl": 60,
        "document": {
            "proof": "",
            "supported_types": ["id_card", "passport", "driving_license"],
            "name": "",
            "dob": "",
            "document_number": "",
            "expiry_date": "",
            "issue_date": "",
        },
        "face": { "proof": "" },
        "background_checks": { "name": "", "dob": "" },
    });
    if let Some(e) = s.email {
        body["email"] = json!(e);
    }
    if let Some(c) = s.country {
        body["country"] = json!(c);
    }
    if address {
        body["address"] = json!({
            "proof": "",
            "supported_types": ["id_card", "utility_bill", "bank_statement"],
            "full_address": "",
            "name": "",
        });
    }
    body
}

/// Create the verification at the provider. Mock: nothing leaves the postbox and there is no URL.
pub(crate) async fn create(p: &Provider, s: &Start<'_>) -> GResult<Outcome> {
    match p {
        Provider::Unavailable { .. } => Err(kyc_unavailable()),
        Provider::Mock { .. } => Ok(Outcome {
            reference: s.reference.to_string(),
            event: "request.pending".into(),
            ..Outcome::default()
        }),
        Provider::Shufti { .. } => {
            let (_, v) = shufti_call(p, "/", &request_body(p, s))
                .await
                .map_err(|why| {
                    tracing::warn!(why, "shufti: verification request failed");
                    GErr::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "kyc_unavailable",
                        "identity verification is not answering right now; try again shortly",
                    )
                })?;
            let o = outcome(&v);
            if o.reference != s.reference {
                return Err(GErr::new(
                    StatusCode::BAD_GATEWAY,
                    "kyc_unavailable",
                    "identity verification answered for another request",
                ));
            }
            Ok(o)
        }
    }
}

/// A signed message the mock provider sends for `outcome` (`accepted` or `declined`): the same
/// shape and signature as Shufti's, so the callback path is the one exercised.
pub(crate) fn mock_message(
    p: &Provider,
    reference: &str,
    outcome: &str,
    name: &str,
    country: &str,
) -> (Vec<u8>, String) {
    let body = if outcome == "accepted" {
        json!({
            "reference": reference,
            "event": "verification.accepted",
            "verification_result": { "document": 1, "face": 1, "background_checks": 1 },
            "verification_data": { "document": { "name": { "full_name": name }, "dob": "1990-01-01", "country": country } },
        })
    } else {
        json!({
            "reference": reference,
            "event": "verification.declined",
            "declined_reason": "Sandbox: the document could not be verified",
            "declined_codes": ["SPDR01"],
            "verification_result": { "document": 0, "face": 1, "background_checks": 1 },
        })
    };
    let bytes = body.to_string().into_bytes();
    let sig = signature(&bytes, p.signing_secret().unwrap_or_default(), p.scheme());
    (bytes, sig)
}

/// `POST /v1/integrations/shufti/callback` — authenticated by the `Signature` header alone.
/// Answers 200 for anything correctly signed (Shufti retries otherwise), including replays and
/// references it does not know, which change nothing.
pub(crate) async fn handle_callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let p = provider(&state);
    let sig = headers.get("signature").and_then(|v| v.to_str().ok());
    let verified = p
        .signing_secret()
        .is_some_and(|secret| shufti_signature_ok(&body, sig, secret, p.scheme()));
    if !verified {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": { "code": "bad_signature", "message": "signature mismatch" } })),
        )
            .into_response();
    }
    let Ok(v) = serde_json::from_slice::<Value>(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": { "code": "invalid_json", "message": "expected JSON" } })),
        )
            .into_response();
    };
    let o = outcome(&v);
    let now = now_unix() as i64;
    let bytes = body.to_vec();
    let applied = state
        .store
        .group_tx(move |tx| -> GResult<Option<String>> {
            match apply_kyc_message_in(tx, &bytes, &o, "callback", now) {
                Err(e) if e.code == "unknown_reference" => Ok(None),
                other => other,
            }
        })
        .await;
    match applied {
        Ok(changed) => {
            if let Some(app) = changed {
                crate::publisher_registration::kyc_changed(&state, &app).await;
            }
            (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
        }
        Err(e) => crate::org_api::api_error(e),
    }
}

/// Read the status of checks that are still pending (older than a minute) or whose status
/// changed at Shufti. Live provider only; the mock has nothing to ask.
pub(crate) async fn poll_once(state: &AppState) {
    let p = provider(state);
    if !matches!(p, Provider::Shufti { .. }) {
        return;
    }
    let now = now_unix() as i64;
    let due: Vec<String> = state
        .store
        .group_read(move |c: &Connection| -> GResult<Vec<String>> {
            let mut stmt = c.prepare(
                "SELECT reference FROM publisher_kyc_checks
                  WHERE provider = 'shufti'
                    AND ((result = 'pending' AND created_at < ?1 - 60 AND created_at > ?1 - 86400 * 7
                          AND COALESCE(polled_at, 0) < ?1 - 300)
                         OR polled_at IS NULL AND final_at IS NOT NULL)
                  ORDER BY updated_at LIMIT 20",
            )?;
            let rows = stmt
                .query_map([now], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok(rows)
        })
        .await
        .unwrap_or_default();
    for reference in due {
        let r = reference.clone();
        let _ = state
            .store
            .group_tx(move |tx| -> GResult<()> {
                tx.execute(
                    "UPDATE publisher_kyc_checks SET polled_at = ?2 WHERE reference = ?1",
                    params![r, now],
                )?;
                Ok(())
            })
            .await;
        match shufti_call(&p, "/status", &json!({ "reference": reference })).await {
            Ok((bytes, v)) => {
                let o = outcome(&v);
                if o.reference != reference {
                    continue;
                }
                let changed = state
                    .store
                    .group_tx(move |tx| -> GResult<Option<String>> {
                        apply_kyc_message_in(tx, &bytes, &o, "status", now)
                    })
                    .await;
                if let Ok(Some(app)) = changed {
                    crate::publisher_registration::kyc_changed(state, &app).await;
                }
            }
            Err(why) => tracing::warn!(why, "shufti: status read failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_follow_both_schemes_and_compare_exactly() {
        let body = br#"{"reference":"r1","event":"verification.accepted"}"#;
        let hashed = signature(body, "secret", Scheme::HashedSecret);
        let mut h = Sha256::new();
        h.update(body);
        h.update(sha256_hex(b"secret").as_bytes());
        assert_eq!(hashed, hex(&h.finalize()));
        let plain = signature(body, "secret", Scheme::PlainSecret);
        let mut h = Sha256::new();
        h.update(body);
        h.update(b"secret");
        assert_eq!(plain, hex(&h.finalize()));
        assert_ne!(hashed, plain);

        assert!(shufti_signature_ok(
            body,
            Some(&hashed),
            "secret",
            Scheme::HashedSecret
        ));
        assert!(shufti_signature_ok(
            body,
            Some(&hashed.to_ascii_uppercase()),
            "secret",
            Scheme::HashedSecret
        ));
        assert!(!shufti_signature_ok(
            body,
            Some(&plain),
            "secret",
            Scheme::HashedSecret
        ));
        assert!(!shufti_signature_ok(
            body,
            Some(&hashed),
            "other",
            Scheme::HashedSecret
        ));
        assert!(!shufti_signature_ok(
            b"{}",
            Some(&hashed),
            "secret",
            Scheme::HashedSecret
        ));
        assert!(!shufti_signature_ok(
            body,
            None,
            "secret",
            Scheme::HashedSecret
        ));
        assert!(!shufti_signature_ok(
            body,
            Some(&hashed[..63]),
            "secret",
            Scheme::HashedSecret
        ));
    }

    #[test]
    fn only_the_allowed_fields_are_taken_from_a_message() {
        let v = json!({
            "reference": "pp_kyc_1",
            "event": "verification.declined",
            "declined_reason": "Face did not match",
            "declined_codes": ["SPDR07"],
            "services_declined_codes": { "face": ["SPDR19"] },
            "verification_result": { "background_checks": 0 },
            "verification_data": { "document": {
                "name": { "first_name": "Ada", "last_name": "Lovelace" },
                "dob": "1990-12-10", "country": "gb", "document_number": "X123",
                "proof": "base64-image-bytes" } },
            "proofs": { "document": { "proof": "https://example/img" } },
        });
        let o = outcome(&v);
        assert_eq!(o.reference, "pp_kyc_1");
        assert_eq!(o.name.as_deref(), Some("Ada Lovelace"));
        assert_eq!(o.dob.as_deref(), Some("1990-12-10"));
        assert_eq!(o.country.as_deref(), Some("GB"));
        assert_eq!(o.aml.as_deref(), Some("match"));
        assert_eq!(o.declined_codes.as_deref(), Some("SPDR07,face:SPDR19"));
        let debug = format!("{o:?}");
        assert!(
            !debug.contains("X123") && !debug.contains("base64") && !debug.contains("example/img")
        );
    }

    #[test]
    fn events_map_to_results() {
        assert_eq!(result_of("verification.accepted"), Some("accepted"));
        assert_eq!(result_of("verification.declined"), Some("declined"));
        assert_eq!(result_of("request.timeout"), Some("cancelled"));
        assert_eq!(result_of("request.pending"), Some("pending"));
        assert_eq!(result_of("verification.status.changed"), None);
        assert_eq!(result_of("request.deleted"), None);
    }

    #[test]
    fn the_hosted_request_asks_for_document_face_and_aml_without_proofs() {
        let p = Provider::Mock { secret: "x".into() };
        let b = request_body(
            &p,
            &Start {
                reference: "r",
                email: Some("dev@example.com"),
                country: Some("TR"),
                language: "TR",
                callback_url: "https://postbox.example/v1/integrations/shufti/callback".into(),
            },
        );
        assert_eq!(b["document"]["proof"], "");
        assert_eq!(b["face"]["proof"], "");
        assert!(b["background_checks"].is_object());
        assert!(b.get("address").is_none());
        assert_eq!(b["country"], "TR");
    }
}
