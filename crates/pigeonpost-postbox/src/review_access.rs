//! A repeatable demonstration of the real device authorization flow for app reviewers.
//! Only the verification URL is retained. This service never polls for or receives user tokens.

use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};
use qrcodegen::{QrCode, QrCodeEcc};
use serde::Deserialize;
use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

const ISSUER: &str = "https://auth.pigeonpost.dev/realms/pigeonpost-prod";

#[derive(Deserialize)]
struct Challenge {
    verification_uri_complete: String,
    user_code: String,
    expires_in: u64,
}

fn document(body: &str, status: StatusCode) -> Response {
    let html = format!(
        r#"<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="robots" content="noindex"><title>Pigeonpost sign-in review</title><style>body{{font:17px system-ui;max-width:680px;margin:3rem auto;padding:0 1rem;line-height:1.5}}svg{{display:block;width:min(100%,360px);height:auto;margin:1rem 0}}button{{font:inherit;padding:.7rem 1rem}}code{{font-size:1.3rem}}</style><main><h1>Pigeonpost sign-in review</h1><p>The app's normal sign-in and handle registration do not require a QR code. This page demonstrates Settings → Scan a sign-in code, which approves a terminal's sign-in.</p><p>Open this page on a second screen. Generate a code, then scan it in the iOS app and sign in with the review account supplied in App Store Connect. Check the code shown below before approving. On the success page, tap Done to return to the app.</p><p>This demonstration uses the real sign-in service but discards the device credential. It never retrieves an access token or signs any machine into your account.</p>{body}<form method="post"><button>Generate a fresh QR code</button></form><p>For the complete terminal workflow, run <code>pigeonpost login</code> on your own computer and scan its QR instead.</p><p><a href="https://pigeonpost.dev/app-privacy.html">Privacy policy</a> · <a href="https://pigeonpost.dev/app-terms.html">Terms of use</a></p></main></html>"#
    );
    (status, [
        ("cache-control", "no-store"),
        ("referrer-policy", "no-referrer"),
        ("x-content-type-options", "nosniff"),
        ("content-security-policy", "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'"),
    ], Html(html)).into_response()
}

pub async fn page() -> Response {
    document("", StatusCode::OK)
}

pub async fn generate() -> Response {
    // A bounded, global gate avoids turning the review page into a device-code flood proxy.
    static LAST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    let mut last = LAST.get_or_init(|| Mutex::new(None)).lock().await;
    if last.is_some_and(|at| at.elapsed() < Duration::from_secs(15)) {
        return document(
            "<p>Please wait 15 seconds before generating another code.</p>",
            StatusCode::TOO_MANY_REQUESTS,
        );
    }
    *last = Some(Instant::now());
    drop(last);
    match challenge().await.and_then(render_challenge) {
        Ok(body) => document(&body, StatusCode::OK),
        Err(()) => document("<p>The sign-in service is unavailable. Please try again shortly. You can still sign into the app normally.</p>", StatusCode::BAD_GATEWAY),
    }
}

async fn challenge() -> Result<Challenge, ()> {
    reqwest::Client::new()
        .post(format!("{ISSUER}/protocol/openid-connect/auth/device"))
        .header("user-agent", "Pigeonpost/review-sign-in")
        .header("content-type", "application/x-www-form-urlencoded")
        .body("client_id=pigeonpost-cli&scope=openid%20profile%20email")
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|_| ())?
        .error_for_status()
        .map_err(|_| ())?
        .json()
        .await
        .map_err(|_| ())
}

fn render_challenge(c: Challenge) -> Result<String, ()> {
    let url = reqwest::Url::parse(&c.verification_uri_complete).map_err(|_| ())?;
    if url.scheme() != "https"
        || url.host_str() != Some("auth.pigeonpost.dev")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/realms/pigeonpost-prod/device"
        || url.fragment().is_some()
        || c.user_code.is_empty()
        || c.user_code.len() > 32
        || !c
            .user_code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        || !(1..=900).contains(&c.expires_in)
        || url.query_pairs().count() != 1
        || url
            .query_pairs()
            .next()
            .map(|(k, v)| k != "user_code" || v != c.user_code)
            .unwrap_or(true)
    {
        return Err(());
    }
    let qr = QrCode::encode_text(url.as_str(), QrCodeEcc::Medium).map_err(|_| ())?;
    let size = qr.size() + 8;
    let mut path = String::new();
    for y in 0..qr.size() {
        for x in 0..qr.size() {
            if qr.get_module(x, y) {
                path.push_str(&format!("M{},{}h1v1h-1z", x + 4, y + 4));
            }
        }
    }
    Ok(format!(
        r#"<h2>Scan this code</h2><svg xmlns="http://www.w3.org/2000/svg" role="img" aria-label="Pigeonpost sign-in QR code" viewBox="0 0 {size} {size}" shape-rendering="crispEdges"><rect width="100%" height="100%" fill="white"/><path d="{path}" fill="black"/></svg><p>Confirmation code: <code>{}</code></p><p>This code expires in {} minutes. Generate a fresh code if it expires or has already been approved.</p>"#,
        c.user_code,
        c.expires_in.div_ceil(60)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(url: &str) -> Challenge {
        Challenge {
            verification_uri_complete: url.into(),
            user_code: "ABCD-EFGH".into(),
            expires_in: 600,
        }
    }
    #[test]
    fn review_qr_uses_only_the_real_verification_flow_and_escapes_no_untrusted_markup() {
        let url = format!("{ISSUER}/device?user_code=ABCD-EFGH");
        let body = render_challenge(sample(&url)).unwrap();
        assert!(body.contains("<svg"));
        assert!(body.contains("ABCD-EFGH"));
        for bad in ["https://example.test/device?user_code=ABCD-EFGH", "https://auth.pigeonpost.dev/other?user_code=ABCD-EFGH", "https://auth.pigeonpost.dev/realms/pigeonpost-prod/device?user_code=DIFFERENT", "https://auth.pigeonpost.dev/realms/pigeonpost-prod/device?user_code=ABCD-EFGH&redirect_uri=https://example.test"] {
            assert!(render_challenge(sample(bad)).is_err());
        }
        let mut markup = sample(&url);
        markup.user_code = "<script>".into();
        assert!(render_challenge(markup).is_err());
        let mut expired = sample(&url);
        expired.expires_in = 0;
        assert!(render_challenge(expired).is_err());
    }
}
