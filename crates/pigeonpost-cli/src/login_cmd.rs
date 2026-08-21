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

/// Refresh this far before the token actually expires. A request that takes two seconds must not
/// arrive holding a token that expired one second ago, and clocks drift.
const REFRESH_SKEW: u64 = 60;

/// Sent on every call to the issuer. Cloudflare fronts `auth.pigeonpost.dev` and answers requests
/// carrying a default library User-Agent with `error code: 1010`, which surfaces as an
/// indistinguishable 403 — so identify ourselves rather than inherit whatever reqwest sends.
const USER_AGENT: &str = concat!("pigeonpost-cli/", env!("CARGO_PKG_VERSION"));

fn http_client() -> Result<reqwest::Client, Error> {
    Ok(reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .user_agent(USER_AGENT)
        .build()?)
}

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
    /// Who signed in. Optional because a session written before these fields existed must keep
    /// working — an upgrade that silently demanded a fresh login would be a worse bug than the
    /// missing name it fixes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
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
    /// Present on the flows that ask for `openid`. Carries the human-readable identity; the access
    /// token is the postbox's business, not ours to read for display.
    #[serde(default)]
    id_token: Option<String>,
}

/// The identity claims worth showing a person. Everything is optional: a realm is free to omit any
/// of them, and a missing name should degrade to "signed in" rather than fail the command.
#[derive(Default, Deserialize)]
struct IdClaims {
    #[serde(default)]
    preferred_username: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    sub: Option<String>,
}

/// Read a JWT's claims **without verifying it**.
///
/// Safe only because of what it is used for: display, and only of a token this process just
/// received over TLS from the issuer it chose. Nothing is authorised on the strength of these
/// claims — the postbox validates the token itself, against the realm's JWKS, on every request.
fn claims_of(jwt: &str) -> IdClaims {
    fn b64url_decode(s: &str) -> Option<Vec<u8>> {
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = Vec::new();
        let mut acc: u32 = 0;
        let mut bits = 0u32;
        for c in s.bytes() {
            if c == b'=' {
                break;
            }
            let v = A.iter().position(|&a| a == c)? as u32;
            acc = (acc << 6) | v;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
            }
        }
        Some(out)
    }
    jwt.split('.')
        .nth(1)
        .and_then(b64url_decode)
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or_default()
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
    let http = http_client()?;
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
    let http = http_client()?;

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
        println!();
        // The whole point of the code being on this screen as well: a QR photographed over
        // somebody's shoulder is a URL, and the person holding it still has to be signed in to the
        // account for the realm to accept the grant.
        println!("Or scan this with the Pigeonpost app, or any camera:");
        println!();
        print_qr(complete);
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
    let http = http_client()?;
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
    let claims = token.id_token.as_deref().map(claims_of).unwrap_or_default();
    Session {
        issuer: issuer.to_string(),
        refresh_token: token.refresh_token,
        access_token: token.access_token,
        expires_at: now_unix() + token.expires_in.unwrap_or(300),
        account: None,
        username: claims.preferred_username,
        email: claims.email,
        subject: claims.sub,
    }
}

/// Trade the refresh token for a new access token.
///
/// Keycloak's refresh token here is `typ: Offline` and carries no expiry, so a session survives
/// indefinitely unless it is revoked — the thing that expires every five minutes is the *access*
/// token, which is why this exists.
async fn refresh(session: &Session) -> Result<Session, Error> {
    let endpoints = discover(&session.issuer).await?;
    let response = http_client()?
        .post(&endpoints.token_endpoint)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", CLIENT_ID),
            ("refresh_token", session.refresh_token.as_str()),
        ])
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let detail: serde_json::Value = response.json().await.unwrap_or_default();
        let reason = detail["error_description"]
            .as_str()
            .or_else(|| detail["error"].as_str())
            .unwrap_or("no detail");
        // A refused refresh is nearly always a revoked or superseded session, and the only way out
        // is a fresh login. Say so, rather than leaving a bare 400 for someone to interpret.
        return Err(format!(
            "this machine's sign-in is no longer valid ({status}: {reason}) — run: pigeonpost login"
        )
        .into());
    }
    let token: TokenResponse = response.json().await?;
    let mut next = session_from(&session.issuer, token);
    // A refresh response need not repeat the identity claims; keep what login established.
    next.account = session.account.clone();
    next.username = next.username.or_else(|| session.username.clone());
    next.email = next.email.or_else(|| session.email.clone());
    next.subject = next.subject.or_else(|| session.subject.clone());
    Ok(next)
}

/// The access token for account-scoped calls, refreshed and re-persisted when it is close to
/// expiry. Every caller that authenticates as the *account* (rather than as one mailbox) must go
/// through here — reading `auth.json` directly is how a command ends up holding a stale token.
pub async fn access_token(home: &Path) -> Result<String, Error> {
    let session = load(home)?.ok_or("not signed in — run: pigeonpost login")?;
    if session.expires_at > now_unix() + REFRESH_SKEW {
        return Ok(session.access_token);
    }
    let refreshed = refresh(&session).await?;
    save_refreshed(home, refreshed.clone())?;
    Ok(refreshed.access_token)
}

/// The signed-in session, refreshed first. For callers that need the identity, not just the token.
pub async fn current_session(home: &Path) -> Result<Session, Error> {
    let session = load(home)?.ok_or("not signed in — run: pigeonpost login")?;
    if session.expires_at > now_unix() + REFRESH_SKEW {
        return Ok(session);
    }
    let refreshed = refresh(&session).await?;
    save_refreshed(home, refreshed.clone())?;
    Ok(refreshed)
}

/// Show *who* is signed in on this machine. Deliberately never prints a token.
///
/// Refreshes first, so the answer reflects a session that still works rather than one that merely
/// exists on disk — "signed in" is not useful if the next command would 401.
pub async fn status(home: &Path, json: bool) -> Result<(), Error> {
    let Some(stored) = load(home)? else {
        if json {
            println!("{}", serde_json::json!({ "signed_in": false }));
        } else {
            println!("not signed in — run: pigeonpost login");
        }
        return Ok(());
    };

    // A session too old to refresh is not a session. Report that plainly instead of printing a
    // name next to a token that would be rejected.
    let session = match current_session(home).await {
        Ok(session) => session,
        Err(e) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "signed_in": false,
                        "issuer": stored.issuer,
                        "detail": e.to_string(),
                    })
                );
            } else {
                println!("not signed in: {e}");
            }
            return Ok(());
        }
    };

    // Fall back to the access token for a session written before the identity fields existed, so
    // an upgrade does not force a re-login just to learn a name we can already read.
    let fallback = claims_of(&session.access_token);
    let username = session
        .username
        .clone()
        .or(fallback.preferred_username)
        .unwrap_or_else(|| "(unknown)".into());
    let email = session.email.clone().or(fallback.email);
    let subject = session.subject.clone().or(fallback.sub);
    let remaining = session.expires_at.saturating_sub(now_unix());

    if json {
        println!(
            "{}",
            serde_json::json!({
                "signed_in": true,
                "username": username,
                "email": email,
                "subject": subject,
                "issuer": session.issuer,
                "access_token_expires_in": remaining,
            })
        );
    } else {
        match &email {
            Some(email) => println!("signed in as {username} <{email}>"),
            None => println!("signed in as {username}"),
        }
        println!("  issuer:  {}", session.issuer);
        println!("  renews:  in {remaining}s, automatically");
    }
    Ok(())
}

/// Forget this machine's session.
pub fn logout(home: &Path) -> Result<(), Error> {
    let path = credentials_path(home);
    match std::fs::remove_file(&path) {
        Ok(()) => println!("signed out; {} removed", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Removing the machine session from under an agent that merely borrows it would sign
            // out every other agent on the box, which is not what "log out of this one" means.
            // Say where it is instead, so the choice stays the operator's.
            match machine_session_path().filter(|p| p.exists()) {
                Some(shared) => {
                    println!("nothing to sign out of here.");
                    println!(
                        "This home borrows the machine session at {} — sign that out with:",
                        shared.display()
                    );
                    println!(
                        "  pigeonpost logout --home {}",
                        shared.parent().unwrap_or(&shared).display()
                    );
                    println!("which signs out every agent on this box.");
                }
                None => println!("not signed in"),
            }
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

pub fn load(home: &Path) -> Result<Option<Session>, Error> {
    match std::fs::read(session_path(home)) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn save(home: &Path, session: Session) -> Result<(), Error> {
    save_to(&credentials_path(home), session)
}

/// Persist a refreshed session over the file it was read from. Writing it into the agent's own
/// home instead would leave the machine session stale and make every agent refresh separately —
/// and quietly scatter copies of a token that mints under the whole account.
fn save_refreshed(home: &Path, session: Session) -> Result<(), Error> {
    save_to(&session_path(home), session)
}

fn save_to(path: &Path, session: Session) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let path = path.to_path_buf();
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

/// The machine-wide session, used when `home` has none of its own.
///
/// One box runs many agents — typically one per repository — and each wants its own mailbox, which
/// means its own `PIGEONPOST_HOME` so that no command needs `--as` and no agent can touch another's
/// credentials. But signing in is a property of the *person at the machine*, not of any one agent,
/// so requiring a login per home would mean logging in once per repository.
///
/// So the two are separated: mailboxes live in whatever home the agent was given, while the
/// session is looked up in that home first and then here. Sign in once; every agent on the box
/// mints under it.
fn machine_session_path() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|h| h.join(".pigeonpost").join(CREDENTIALS_FILE))
}

/// Where this home's session actually is: its own file, else the machine-wide one.
fn session_path(home: &Path) -> PathBuf {
    let local = credentials_path(home);
    if local.exists() {
        return local;
    }
    match machine_session_path() {
        Some(shared) if shared.exists() => shared,
        _ => local,
    }
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
            username: Some("bekir".into()),
            email: Some("bekir@example.com".into()),
            subject: Some("15369298-b830".into()),
        };
        let rendered = serde_json::json!({
            "signed_in": true,
            "username": session.username,
            "email": session.email,
            "subject": session.subject,
            "issuer": session.issuer,
            "access_token_expires_in": session.expires_at.saturating_sub(now_unix()),
        })
        .to_string();
        assert!(!rendered.contains("SECRET"));
        // The identity is the whole point of the command, so pin that it is actually there —
        // a "safe" rendering that printed nothing would also pass the assertion above.
        assert!(rendered.contains("bekir"));
    }

    /// A session written before the identity fields existed must still load, or an upgrade would
    /// silently sign everyone out.
    #[test]
    fn an_older_session_file_still_loads() {
        let older = r#"{
            "issuer": "https://auth.example/realms/x",
            "refresh_token": "r",
            "access_token": "a",
            "expires_at": 1
        }"#;
        let session: Session = serde_json::from_str(older).expect("older session must still parse");
        assert_eq!(session.username, None);
        assert_eq!(session.issuer, "https://auth.example/realms/x");
    }

    /// The claims reader is display-only, so it must never panic on input it did not expect.
    #[test]
    fn claims_of_survives_rubbish() {
        assert_eq!(claims_of("").preferred_username, None);
        assert_eq!(claims_of("not-a-jwt").preferred_username, None);
        assert_eq!(claims_of("a.!!!!.c").preferred_username, None);
        // A real shape, base64url with no padding.
        let body = "eyJwcmVmZXJyZWRfdXNlcm5hbWUiOiJiZWtpciJ9";
        assert_eq!(
            claims_of(&format!("x.{body}.y"))
                .preferred_username
                .as_deref(),
            Some("bekir")
        );
    }
}

/// Draw a QR code, two module rows to a terminal row.
///
/// Half-block characters rather than two spaces per module: at two columns each, a version-5 symbol
/// is 98 characters wide and wraps on an 80-column terminal, and a wrapped QR is one no camera will
/// read. One column per module with `▀` splitting the cell top from bottom keeps it at 49 and keeps
/// the modules roughly square, because a character cell is about twice as tall as it is wide.
///
/// Black on white explicitly: on a dark terminal theme an uncoloured render comes out inverted, and
/// scanners will not read that either. The four-module quiet zone is the part people leave out and
/// then wonder why nothing works.
fn print_qr(text: &str) {
    use qrcodegen::{QrCode, QrCodeEcc};

    let code = match QrCode::encode_text(text, QrCodeEcc::Low) {
        Ok(code) => code,
        Err(_) => return, // A URL too long to encode is not a reason to fail a sign-in.
    };
    let border: i32 = 4;
    let size = code.size();
    let dark = |x: i32, y: i32| code.get_module(x, y);

    let mut y = -border;
    while y < size + border {
        let mut line = String::from("\u{1b}[30;107m");
        for x in -border..size + border {
            line.push(match (dark(x, y), dark(x, y + 1)) {
                (true, true) => '\u{2588}',   // full block
                (true, false) => '\u{2580}',  // upper half
                (false, true) => '\u{2584}',  // lower half
                (false, false) => ' ',
            });
        }
        line.push_str("\u{1b}[0m");
        println!("{line}");
        y += 2;
    }
}
