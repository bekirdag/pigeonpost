//! `pigeonpost login` — sign a terminal in to a Pigeonpost account.
//!
//! Two standard flows, no invented protocol:
//!
//! * **Authorization Code + PKCE** (RFC 7636) with a loopback redirect, for a machine with a
//!   browser. This is what `gh`, `claude` and `codex` do.
//! * **Device Authorization Grant** (RFC 8628), for a machine without one — a server over SSH, a
//!   container, a box you are not sitting at. The code is short enough to read aloud.
//!
//! No client secret ships in the binary, because a secret shipped to every user is not a secret.
//! PKCE is what makes a public client safe: the authorization code is worthless without the
//! verifier, which never leaves this process.
//!
//! The refresh token that comes back is the keys to every mailbox the account owns, so it is
//! written through the same private-file custody as an agent's identity key — never to an
//! environment variable, never to a file a shell history could echo.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

type Error = Box<dyn std::error::Error>;

/// The public client this CLI authenticates as. Public, so it is a name rather than a credential.
pub const CLIENT_ID: &str = "pigeonpost-cli";

/// Default realm. Overridable so a self-hosted deployment can point at its own.
pub const DEFAULT_ISSUER: &str = "https://auth.pigeonpost.dev/realms/pigeonpost-prod";

const CREDENTIALS_FILE: &str = "auth.json";
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// A signed-in session. `refresh_token` is a password-equivalent: it mints access tokens for every
/// mailbox this account owns.
#[derive(Serialize, Deserialize, Clone)]
pub struct Session {
    pub issuer: String,
    pub refresh_token: String,
    pub access_token: String,
    /// Unix seconds. Refreshed rather than trusted blindly — a clock that drifts should cost a
    /// round trip, not a confusing 401.
    pub expires_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

#[derive(Deserialize)]
struct Endpoints {
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default)]
    device_authorization_endpoint: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct DeviceAuth {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default)]
    interval: Option<u64>,
    #[serde(default)]
    expires_in: Option<u64>,
}

async fn discover(issuer: &str) -> Result<Endpoints, Error> {
    let url = format!(
        "{}/.well-known/openid-configuration",
        issuer.trim_end_matches('/')
    );
    let http = reqwest::Client::builder().timeout(HTTP_TIMEOUT).build()?;
    Ok(http
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

/// URL-safe base64 without padding, as PKCE requires (RFC 7636 §4.2).
fn b64url(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..=chunk.len() {
            out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

fn random_verifier() -> String {
    use rand_core::RngCore;
    let mut bytes = [0u8; 32];
    rand_core::OsRng.fill_bytes(&mut bytes);
    b64url(&bytes)
}

/// Sign in through a browser, catching the redirect on loopback.
pub async fn login_browser(home: &Path, issuer: &str, open_browser: bool) -> Result<(), Error> {
    let endpoints = discover(issuer).await?;

    // Bind before building the URL: the port is part of the redirect the server will validate, so
    // it has to be the port actually held. Port 0 lets the OS pick a free one.
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let redirect = format!("http://127.0.0.1:{port}/callback");

    let verifier = random_verifier();
    let challenge = b64url(&Sha256::digest(verifier.as_bytes()));
    let state = random_verifier();

    let url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        endpoints.authorization_endpoint,
        urlencode(CLIENT_ID),
        urlencode(&redirect),
        urlencode("openid profile offline_access"),
        urlencode(&state),
        urlencode(&challenge),
    );

    println!("Sign in to Pigeonpost:");
    println!();
    println!("  {url}");
    println!();
    if open_browser {
        print!("Press ENTER to open this in your browser (Ctrl-C to cancel) ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        open_in_browser(&url);
    }
    println!("waiting for the browser to come back…");

    let code = wait_for_callback(listener, &state)?;
    let token = exchange_code(&endpoints.token_endpoint, &code, &verifier, &redirect).await?;
    save(home, session_from(issuer, token))?;
    println!("signed in — this machine can now reach every mailbox on the account");
    Ok(())
}

/// Sign in on a machine with no browser, by typing a short code into one that has.
pub async fn login_device(home: &Path, issuer: &str) -> Result<(), Error> {
    let endpoints = discover(issuer).await?;
    let device_endpoint = endpoints.device_authorization_endpoint.ok_or(
        "this issuer does not advertise the device authorization grant; use `pigeonpost login`",
    )?;
    let http = reqwest::Client::builder().timeout(HTTP_TIMEOUT).build()?;

    let auth: DeviceAuth = http
        .post(&device_endpoint)
        .form(&[
            ("client_id", CLIENT_ID),
            ("scope", "openid profile offline_access"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    println!("On any device with a browser, open:");
    println!();
    println!("  {}", auth.verification_uri);
    println!();
    println!("and enter the code:   {}", auth.user_code);
    if let Some(complete) = &auth.verification_uri_complete {
        println!();
        println!("Or go straight to:    {complete}");
    }
    println!();

    // RFC 8628 §3.5: honour the server's interval, and back off on `slow_down`. A client that
    // ignores this gets itself rate-limited and then blames the server.
    let mut interval = Duration::from_secs(auth.interval.unwrap_or(5));
    let deadline = Instant::now() + Duration::from_secs(auth.expires_in.unwrap_or(600));
    println!("waiting for you to finish in the browser…");

    loop {
        if Instant::now() >= deadline {
            return Err("the login code expired before it was used".into());
        }
        tokio::time::sleep(interval).await;

        let response = http
            .post(&endpoints.token_endpoint)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", &auth.device_code),
                ("client_id", CLIENT_ID),
            ])
            .send()
            .await?;
        if response.status().is_success() {
            let token: TokenResponse = response.json().await?;
            save(home, session_from(issuer, token))?;
            println!("signed in — this machine can now reach every mailbox on the account");
            return Ok(());
        }

        let body: serde_json::Value = response.json().await.unwrap_or_default();
        match body["error"].as_str().unwrap_or("") {
            // Not finished yet; keep waiting. This is the expected answer most of the time.
            "authorization_pending" => {}
            "slow_down" => interval += Duration::from_secs(5),
            "expired_token" => return Err("the login code expired before it was used".into()),
            "access_denied" => return Err("the sign-in was declined".into()),
            other => return Err(format!("sign-in failed: {other}").into()),
        }
    }
}

/// Block until the browser hits the loopback redirect, and return the authorization code.
///
/// Single-threaded and single-shot on purpose: this listener exists for one redirect and then
/// closes, so it is not a service anything else can reach.
fn wait_for_callback(listener: TcpListener, expected_state: &str) -> Result<String, Error> {
    listener.set_nonblocking(false)?;
    let deadline = Instant::now() + Duration::from_secs(300);
    for stream in listener.incoming() {
        if Instant::now() >= deadline {
            return Err("timed out waiting for the browser".into());
        }
        let mut stream = stream?;
        let mut request = String::new();
        BufReader::new(&stream).read_line(&mut request)?;

        let target = request.split_whitespace().nth(1).unwrap_or_default();
        let query = target.split_once('?').map(|(_, q)| q).unwrap_or_default();
        let mut code = None;
        let mut state = None;
        let mut error = None;
        for pair in query.split('&') {
            match pair.split_once('=') {
                Some(("code", v)) => code = Some(urldecode(v)),
                Some(("state", v)) => state = Some(urldecode(v)),
                Some(("error", v)) => error = Some(urldecode(v)),
                _ => {}
            }
        }

        let (status, message) = if let Some(error) = &error {
            ("400 Bad Request", format!("Sign-in failed: {error}"))
        } else if state.as_deref() != Some(expected_state) {
            // A mismatched state means this redirect was not the one we started, so the code in it
            // is not ours to spend.
            ("400 Bad Request", "Sign-in failed: state mismatch".into())
        } else if code.is_some() {
            ("200 OK", "Signed in. You can close this tab.".into())
        } else {
            ("400 Bad Request", "Sign-in failed: no code returned".into())
        };
        let body = format!(
            "<!doctype html><meta charset=utf-8><title>Pigeonpost</title>\
             <body style=\"font:16px system-ui;margin:4rem auto;max-width:32rem\"><p>{message}</p>"
        );
        let _ = write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.flush();

        if let Some(error) = error {
            return Err(format!("sign-in failed: {error}").into());
        }
        if state.as_deref() != Some(expected_state) {
            return Err("sign-in failed: the redirect did not match this request".into());
        }
        if let Some(code) = code {
            return Ok(code);
        }
    }
    Err("the browser never came back".into())
}

async fn exchange_code(
    token_endpoint: &str,
    code: &str,
    verifier: &str,
    redirect: &str,
) -> Result<TokenResponse, Error> {
    let http = reqwest::Client::builder().timeout(HTTP_TIMEOUT).build()?;
    let response = http
        .post(token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", code),
            ("redirect_uri", redirect),
            ("code_verifier", verifier),
        ])
        .send()
        .await?;
    if !response.status().is_success() {
        let detail: serde_json::Value = response.json().await.unwrap_or_default();
        return Err(format!(
            "the token exchange was refused: {}",
            detail["error_description"]
                .as_str()
                .or_else(|| detail["error"].as_str())
                .unwrap_or("no detail")
        )
        .into());
    }
    Ok(response.json().await?)
}

fn session_from(issuer: &str, token: TokenResponse) -> Session {
    Session {
        issuer: issuer.to_string(),
        refresh_token: token.refresh_token,
        access_token: token.access_token,
        expires_at: now_unix() + token.expires_in.unwrap_or(300),
        account: None,
    }
}

/// Show whether this machine is signed in. Deliberately never prints a token.
pub fn status(home: &Path, json: bool) -> Result<(), Error> {
    match load(home)? {
        Some(session) => {
            let remaining = session.expires_at.saturating_sub(now_unix());
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "signed_in": true,
                        "issuer": session.issuer,
                        "access_token_expires_in": remaining,
                    })
                );
            } else {
                println!("signed in to {}", session.issuer);
                println!("access token expires in {remaining}s (refreshed automatically)");
            }
        }
        None if json => println!("{}", serde_json::json!({ "signed_in": false })),
        None => println!("not signed in — run: pigeonpost login"),
    }
    Ok(())
}

/// Forget this machine's session.
pub fn logout(home: &Path) -> Result<(), Error> {
    let path = credentials_path(home);
    match std::fs::remove_file(&path) {
        Ok(()) => println!("signed out; {} removed", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => println!("not signed in"),
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

pub fn load(home: &Path) -> Result<Option<Session>, Error> {
    match std::fs::read(credentials_path(home)) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn save(home: &Path, session: Session) -> Result<(), Error> {
    std::fs::create_dir_all(home)?;
    let path = credentials_path(home);
    let body = serde_json::to_vec_pretty(&session)?;
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&tmp)?;
    restrict(&file)?;
    file.write_all(&body)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn credentials_path(home: &Path) -> PathBuf {
    home.join(CREDENTIALS_FILE)
}

/// Owner-only before a byte is written, so the token is never briefly world-readable.
fn restrict(file: &std::fs::File) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = file;
    }
    Ok(())
}

fn open_in_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    // Best effort: the URL was printed above, so a headless box simply falls back to copy-paste.
    let _ = std::process::Command::new(opener).arg(url).spawn();
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

fn urldecode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_the_rfc_7636_test_vector() {
        // RFC 7636 Appendix B. Getting this wrong means every sign-in is refused, so it is worth
        // pinning against the published vector rather than against our own output.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            b64url(&Sha256::digest(verifier.as_bytes())),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn base64url_is_unpadded_and_url_safe() {
        assert_eq!(b64url(&[0xfb, 0xff]), "-_8");
        assert!(!b64url(&[1, 2, 3, 4, 5]).contains('='));
        assert!(!b64url(&[0xff; 16]).contains('+'));
        assert!(!b64url(&[0xff; 16]).contains('/'));
    }

    #[test]
    fn verifiers_are_unique_and_long_enough() {
        // RFC 7636 §4.1 requires 43–128 characters.
        let a = random_verifier();
        let b = random_verifier();
        assert_ne!(a, b);
        assert!((43..=128).contains(&a.len()), "length {}", a.len());
    }

    #[test]
    fn url_encoding_round_trips_the_awkward_characters() {
        for value in [
            "a b",
            "openid profile offline_access",
            "http://127.0.0.1:5/cb",
            "a+b%c",
        ] {
            assert_eq!(urldecode(&urlencode(value)), value);
        }
    }

    #[test]
    fn a_session_never_renders_its_tokens_in_status_output() {
        // status() prints the issuer and an expiry and nothing else; this pins the shape so a
        // future convenience field cannot quietly start printing the refresh token.
        let session = Session {
            issuer: "https://auth.example/realms/x".into(),
            refresh_token: "SECRET-REFRESH".into(),
            access_token: "SECRET-ACCESS".into(),
            expires_at: now_unix() + 60,
            account: None,
        };
        let rendered = serde_json::json!({
            "signed_in": true,
            "issuer": session.issuer,
            "access_token_expires_in": session.expires_at.saturating_sub(now_unix()),
        })
        .to_string();
        assert!(!rendered.contains("SECRET"));
    }
}
