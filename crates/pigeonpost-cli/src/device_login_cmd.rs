//! `pigeonpost login --device --handle /acme/agent1` and `pigeonpost login --enroll-stdin` —
//! bringing a machine up as one pre-assigned organization agent (SDS §10, `docs/orgs.md` §8).
//!
//! These are **mailbox** enrollments, not account sign-ins. The machine ends up holding exactly one
//! mailbox credential for the agent it was approved as, and nothing that reaches the approving
//! person's account:
//!
//! * the device makes its own Ed25519 key and a private claim secret; the server only ever learns
//!   the public key and a hash of the secret, and redeeming needs both proofs;
//! * the person approves in their own browser or phone, so their sign-in never touches this box;
//! * an account session this machine may already hold is revoked at the realm and deleted, so the
//!   box is downgraded to the one mailbox it was enrolled for.
//!
//! The returned credential is a mailbox capability token, stored through the same owner-only
//! custody as every other mailbox token (`postbox.json`); the device key sits beside it in
//! `device-keys.json`, also owner-only.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ed25519_dalek::{Signer, SigningKey};
use pigeonpost_directory::private_store::{
    read_private_file_bounded, write_private_file_atomic, PrivateDirectory,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::{login_cmd, postbox_cmd};

type Error = Box<dyn std::error::Error>;

const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const DEVICE_KEYS_FILE: &str = "device-keys.json";
const MAX_DEVICE_KEYS_BYTES: u64 = 1024 * 1024;
/// Domain separation for the two proofs, exactly as `docs/orgs.md` §8 spells them.
const PAIRING_CONTEXT: &[u8] = b"pigeonpost-pairing-claim-v1\0";
const ENROLL_CONTEXT: &[u8] = b"pigeonpost-enroll-v1\0";
/// A server cannot make us poll faster than this, whatever `interval` it sends.
const MIN_INTERVAL: Duration = Duration::from_secs(1);
const USER_AGENT: &str = concat!("pigeonpost-cli/", env!("CARGO_PKG_VERSION"));

fn http() -> Result<reqwest::Client, Error> {
    Ok(reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .user_agent(USER_AGENT)
        .build()?)
}

/// Standard base64 with padding, the encoding the postbox's JSON uses for keys.
pub(crate) fn b64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn random32() -> [u8; 32] {
    use rand_core::RngCore;
    let mut b = [0u8; 32];
    rand_core::OsRng.fill_bytes(&mut b);
    b
}

/// What a successful claim or redemption returns.
#[derive(Deserialize, Debug)]
pub(crate) struct Issued {
    pub credential: String,
    pub address: String,
    #[serde(default)]
    pub handle: Option<String>,
    #[serde(default)]
    pub expires_at: Option<u64>,
    #[serde(default)]
    pub device_id: Option<String>,
}

#[derive(Deserialize, Debug)]
pub(crate) struct Pairing {
    pub pairing_id: String,
    pub user_code: String,
    #[serde(default)]
    pub verification_uri: Option<String>,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    #[serde(default)]
    pub confirmation: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub interval: Option<u64>,
}

/// One device key, kept beside the mailbox token it was enrolled with.
#[derive(Serialize, Deserialize, Clone)]
struct DeviceKey {
    address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    device_id: Option<String>,
    public_key: String,
    /// The Ed25519 seed. Owner-only file; never printed.
    secret_key: String,
    created_at: u64,
}

impl Drop for DeviceKey {
    fn drop(&mut self) {
        self.secret_key.zeroize();
    }
}

#[derive(Serialize, Deserialize, Default)]
struct DeviceKeys {
    devices: Vec<DeviceKey>,
}

/// Options shared by both enrollment paths.
pub struct Options<'a> {
    pub home: &'a Path,
    pub postbox: &'a str,
    pub label: Option<&'a str>,
    pub workspace: Option<PathBuf>,
    pub onboard: bool,
    pub json: bool,
}

/// The pairing flow: register, show the code, wait for a person to approve, claim.
pub async fn login_device_handle(opts: &Options<'_>, handle: &str) -> Result<(), Error> {
    let base = opts.postbox.trim_end_matches('/');
    let handle = handle.trim();
    if !handle.starts_with('/') || handle.split('/').filter(|s| !s.is_empty()).count() < 2 {
        return Err("--handle must be an agent address like /acme/agent1".into());
    }
    // Fail before issuing an enrollment or pairing if this home cannot retain
    // credentials privately. Keep the guarded directory alive for the flow.
    let _private_home = prepare_private_home(opts.home)?;
    let mut seed = zeroize::Zeroizing::new(random32());
    let key = SigningKey::from_bytes(&seed);
    let mut secret = zeroize::Zeroizing::new(random32());
    let challenge = Sha256::digest(&secret[..]);

    let client = http()?;
    let mut body = serde_json::json!({
        "handle": handle,
        "device_public_key": b64(key.verifying_key().as_bytes()),
        "claim_challenge": b64(&challenge),
    });
    if let Some(label) = opts.label {
        body["label"] = serde_json::json!(label);
    }
    let response = client
        .post(format!("{base}/v1/device-pairings"))
        .json(&body)
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let body: serde_json::Value = response.json().await.unwrap_or_default();
        return Err(describe("could not start the device login", status, &body).into());
    }
    let pairing: Pairing = response.json().await?;

    if !opts.json {
        println!("Approve this machine as {handle}.");
        println!();
        if let Some(uri) = &pairing.verification_uri_complete {
            println!("Scan this with your phone, or open the link on any signed-in device:");
            println!();
            login_cmd::print_qr(uri);
            println!();
            println!("  {uri}");
        } else if let Some(uri) = &pairing.verification_uri {
            println!("Open {uri} on any signed-in device.");
        }
        println!();
        println!("  code          {}", pairing.user_code);
        if let Some(c) = &pairing.confirmation {
            println!("  confirmation  {c}");
            println!();
            println!("Approve only if the page shows the same confirmation number.");
        }
        println!();
        println!("waiting for approval…");
    } else {
        println!(
            "{}",
            serde_json::json!({
                "pairing_id": pairing.pairing_id, "user_code": pairing.user_code,
                "verification_uri_complete": pairing.verification_uri_complete,
                "confirmation": pairing.confirmation, "state": "waiting",
            })
        );
    }

    let mut message = PAIRING_CONTEXT.to_vec();
    message.extend_from_slice(pairing.pairing_id.as_bytes());
    let signature = key.sign(&message);
    let claim = serde_json::json!({
        "pairing_id": pairing.pairing_id,
        "claim_secret": b64(&secret[..]),
        "signature": b64(&signature.to_bytes()),
    });
    secret.zeroize();

    let mut interval = Duration::from_secs(pairing.interval.unwrap_or(5)).max(MIN_INTERVAL);
    let deadline = Instant::now() + Duration::from_secs(pairing.expires_in.unwrap_or(600));
    let issued = loop {
        if Instant::now() >= deadline {
            return Err("the device login expired before anybody approved it".into());
        }
        tokio::time::sleep(interval).await;
        let response = client
            .post(format!("{base}/v1/device-pairings/claim"))
            .json(&claim)
            .send()
            .await?;
        let status = response.status();
        if status.is_success() {
            break response.json::<Issued>().await?;
        }
        let body: serde_json::Value = response.json().await.unwrap_or_default();
        match (status.as_u16(), body["error"].as_str().unwrap_or("")) {
            (_, "authorization_pending") | (428, _) => {}
            (_, "slow_down") | (429, _) => interval += Duration::from_secs(5),
            (_, "pairing_expired") | (410, _) => {
                return Err("the device login expired before anybody approved it".into())
            }
            (_, "access_denied") => return Err("the device login was declined".into()),
            _ => return Err(describe("the device login failed", status, &body).into()),
        }
    };
    let result = finish(opts, issued, &seed).await;
    seed.zeroize();
    result
}

/// Redeem a one-time enrollment code.
pub async fn login_enroll(opts: &Options<'_>, code: &str) -> Result<(), Error> {
    let base = opts.postbox.trim_end_matches('/');
    let code = code.trim();
    if code.is_empty() {
        return Err("no enrollment code given".into());
    }
    let _private_home = prepare_private_home(opts.home)?;
    let mut seed = zeroize::Zeroizing::new(random32());
    let key = SigningKey::from_bytes(&seed);
    let mut message = ENROLL_CONTEXT.to_vec();
    message.extend_from_slice(code.as_bytes());
    let signature = key.sign(&message);
    let mut body = serde_json::json!({
        "code": code,
        "device_public_key": b64(key.verifying_key().as_bytes()),
        "signature": b64(&signature.to_bytes()),
    });
    if let Some(label) = opts.label {
        body["label"] = serde_json::json!(label);
    }
    let response = http()?
        .post(format!("{base}/v1/enrollments/redeem"))
        .json(&body)
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let body: serde_json::Value = response.json().await.unwrap_or_default();
        seed.zeroize();
        if body["error"] == "enrollment_invalid" {
            return Err(
                "that enrollment code is not valid (wrong, already used, expired or revoked)"
                    .into(),
            );
        }
        return Err(describe("enrollment failed", status, &body).into());
    }
    let issued: Issued = response.json().await?;
    let result = finish(opts, issued, &seed).await;
    seed.zeroize();
    result
}

/// Store the credential and device key, downgrade the machine, optionally onboard.
async fn finish(opts: &Options<'_>, issued: Issued, seed: &[u8; 32]) -> Result<(), Error> {
    if !issued.credential.starts_with("cap_") {
        return Err("the postbox returned something that is not a mailbox credential".into());
    }
    let credential = postbox_cmd::Credential {
        base_url: opts.postbox.trim_end_matches('/').to_string(),
        address: issued.address.clone(),
        capability_token: issued.credential.clone(),
        label: opts.label.map(str::to_string),
        handle: issued.handle.clone(),
        created_at: now_unix(),
    };
    let key = SigningKey::from_bytes(seed);
    save_device_key(
        opts.home,
        DeviceKey {
            address: issued.address.clone(),
            device_id: issued.device_id.clone(),
            public_key: b64(key.verifying_key().as_bytes()),
            secret_key: b64(seed),
            created_at: now_unix(),
        },
    )?;
    let path = postbox_cmd::record_credential(opts.home, &credential)?;

    let downgraded = login_cmd::revoke_and_forget(opts.home).await;

    let name = issued
        .handle
        .clone()
        .unwrap_or_else(|| issued.address.clone());
    if opts.json {
        println!(
            "{}",
            serde_json::json!({
                "address": issued.address, "handle": issued.handle, "device_id": issued.device_id,
                "expires_at": issued.expires_at, "credentials": path.display().to_string(),
                "account_session": downgraded.as_json(),
            })
        );
    } else {
        println!("this machine is now {name}");
        println!("  mailbox credential saved to {}", path.display());
        match &downgraded {
            login_cmd::Downgrade::NoSession => {}
            login_cmd::Downgrade::Revoked(p) => println!(
                "  the account session on this machine was revoked and {} removed",
                p.display()
            ),
            login_cmd::Downgrade::RemovedOnly(p, why) => println!(
                "  {} removed; revoking it at the realm failed ({why}) — sign out of other sessions from your account page",
                p.display()
            ),
        }
    }

    if opts.onboard {
        let dir = match &opts.workspace {
            Some(d) => d.clone(),
            None => std::env::current_dir()?,
        };
        let workspace = crate::workspace_cmd::Workspace {
            git_repo: crate::workspace_cmd::git_remote(&dir),
            local_path: Some(dir.display().to_string()),
            ..Default::default()
        };
        postbox_cmd::set_workspace(opts.home, Some(&issued.address), workspace, opts.json).await?;
        if !opts.json {
            println!();
            println!("Next, choose what answers this agent's requests and start the daemon:");
            println!("  pigeonpost agentd answer --runtime <claude|codex|…>");
            println!("  pigeonpost agentd install");
            println!("Nothing here grants SSH or new automatic actions; those stay with your org's admins.");
        }
    }
    Ok(())
}

fn describe(context: &str, status: reqwest::StatusCode, body: &serde_json::Value) -> String {
    let code = body["error"].as_str().unwrap_or("error");
    match body["detail"].as_str() {
        Some(detail) => format!("{context}: {code} ({status}) — {detail}"),
        None => format!("{context}: {code} ({status})"),
    }
}

fn device_keys_path(home: &Path) -> PathBuf {
    home.join(DEVICE_KEYS_FILE)
}

fn load_device_keys(home: &Path) -> Result<DeviceKeys, Error> {
    let path = device_keys_path(home);
    match read_private_file_bounded(&path, MAX_DEVICE_KEYS_BYTES) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|_| "device-keys.json is invalid; no device credentials were changed".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DeviceKeys::default()),
        Err(e) => Err(e.into()),
    }
}

fn prepare_private_home(home: &Path) -> Result<PrivateDirectory, Error> {
    let directory = PrivateDirectory::open_or_create(home)
        .map_err(|e| format!("device credentials need a private PIGEONPOST_HOME directory: {e}"))?;
    // Check existing state before consuming a one-time enrollment code. The
    // bounded custody read refuses links and unsafe file permissions on each OS.
    load_device_keys(home)?;
    postbox_cmd::check_enrollment_credentials(home)?;
    Ok(directory)
}

fn save_device_key(home: &Path, key: DeviceKey) -> Result<(), Error> {
    let path = device_keys_path(home);
    let mut all = load_device_keys(home)?;
    all.devices.retain(|d| d.address != key.address);
    all.devices.push(key);
    let bytes = zeroize::Zeroizing::new(serde_json::to_vec_pretty(&all)?);
    write_private_file_atomic(&path, &bytes, MAX_DEVICE_KEYS_BYTES)?;
    Ok(())
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    use std::sync::{Arc, Mutex};

    fn unb64(s: &str) -> Vec<u8> {
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = Vec::new();
        let (mut acc, mut bits) = (0u32, 0u32);
        for c in s.bytes().filter(|c| *c != b'=') {
            acc = (acc << 6) | A.iter().position(|a| *a == c).unwrap() as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
            }
        }
        out
    }

    #[test]
    fn base64_matches_rfc_4648_vectors() {
        assert_eq!(b64(b""), "");
        assert_eq!(b64(b"f"), "Zg==");
        assert_eq!(b64(b"fo"), "Zm8=");
        assert_eq!(b64(b"foo"), "Zm9v");
        assert_eq!(b64(b"foobar"), "Zm9vYmFy");
        assert_eq!(unb64("Zm9vYmFy"), b"foobar");
    }

    /// What the mock postbox should answer to claims.
    #[derive(Clone, Copy, PartialEq)]
    enum Outcome {
        Approve,
        Deny,
        Expire,
    }

    #[derive(Default)]
    struct Seen {
        pubkey: Option<Vec<u8>>,
        challenge: Option<Vec<u8>>,
        claims: usize,
        revoked: Vec<String>,
        enroll_ok: bool,
    }

    struct Mock {
        outcome: Outcome,
        seen: Mutex<Seen>,
        base: Mutex<String>,
    }

    async fn pairings(
        State(m): State<Arc<Mock>>,
        Json(b): Json<serde_json::Value>,
    ) -> impl IntoResponse {
        assert_eq!(b["handle"], "/acme/agent1");
        assert_eq!(b["label"], "Build box");
        let mut s = m.seen.lock().unwrap();
        s.pubkey = Some(unb64(b["device_public_key"].as_str().unwrap()));
        s.challenge = Some(unb64(b["claim_challenge"].as_str().unwrap()));
        assert_eq!(s.pubkey.as_ref().unwrap().len(), 32);
        assert_eq!(s.challenge.as_ref().unwrap().len(), 32);
        let base = m.base.lock().unwrap().clone();
        (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "pairing_id": "pair_test1", "user_code": "ABCD-EFGH",
                "verification_uri": format!("{base}/pair.html"),
                "verification_uri_complete": format!("{base}/pair.html#ABCD-EFGH"),
                "confirmation": "123 456", "expires_in": 30, "interval": 0,
            })),
        )
    }

    async fn claim(
        State(m): State<Arc<Mock>>,
        Json(b): Json<serde_json::Value>,
    ) -> axum::response::Response {
        let mut s = m.seen.lock().unwrap();
        s.claims += 1;
        // Both proofs, exactly as the contract spells them.
        let secret = unb64(b["claim_secret"].as_str().unwrap());
        assert_eq!(
            Sha256::digest(&secret).to_vec(),
            s.challenge.clone().unwrap()
        );
        let key = VerifyingKey::from_bytes(&s.pubkey.clone().unwrap().try_into().unwrap()).unwrap();
        let sig = Signature::from_slice(&unb64(b["signature"].as_str().unwrap())).unwrap();
        let mut msg = b"pigeonpost-pairing-claim-v1\0".to_vec();
        msg.extend_from_slice(b["pairing_id"].as_str().unwrap().as_bytes());
        key.verify(&msg, &sig).expect("claim signature");
        if s.claims < 3 {
            return (
                StatusCode::from_u16(428).unwrap(),
                Json(serde_json::json!({"error": "authorization_pending"})),
            )
                .into_response();
        }
        match m.outcome {
            Outcome::Deny => (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "access_denied"})),
            )
                .into_response(),
            Outcome::Expire => (
                StatusCode::GONE,
                Json(serde_json::json!({"error": "pairing_expired"})),
            )
                .into_response(),
            Outcome::Approve => Json(serde_json::json!({
                "credential": "cap_0123456789abcdef", "address": "/k/agentaddr",
                "handle": "/acme/agent1", "expires_at": 1, "device_id": "dev_1",
            }))
            .into_response(),
        }
    }

    async fn redeem(
        State(m): State<Arc<Mock>>,
        Json(b): Json<serde_json::Value>,
    ) -> axum::response::Response {
        let key = VerifyingKey::from_bytes(
            &unb64(b["device_public_key"].as_str().unwrap())
                .try_into()
                .unwrap(),
        )
        .unwrap();
        let sig = Signature::from_slice(&unb64(b["signature"].as_str().unwrap())).unwrap();
        let mut msg = b"pigeonpost-enroll-v1\0".to_vec();
        msg.extend_from_slice(b["code"].as_str().unwrap().as_bytes());
        key.verify(&msg, &sig).expect("enroll signature");
        if b["code"] != "enr_good" {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "enrollment_invalid"})),
            )
                .into_response();
        }
        m.seen.lock().unwrap().enroll_ok = true;
        Json(serde_json::json!({
            "credential": "cap_enrolled", "address": "/k/enrolled", "handle": "/acme/agent2",
            "expires_at": 2, "device_id": "dev_2",
        }))
        .into_response()
    }

    async fn discovery(State(m): State<Arc<Mock>>) -> impl IntoResponse {
        let base = m.base.lock().unwrap().clone();
        Json(serde_json::json!({
            "authorization_endpoint": format!("{base}/auth"),
            "token_endpoint": format!("{base}/token"),
            "revocation_endpoint": format!("{base}/revoke"),
        }))
    }

    async fn revoke(
        State(m): State<Arc<Mock>>,
        axum::extract::Form(f): axum::extract::Form<std::collections::HashMap<String, String>>,
    ) -> StatusCode {
        assert_eq!(
            f.get("token_type_hint").map(String::as_str),
            Some("refresh_token")
        );
        assert_eq!(
            f.get("client_id").map(String::as_str),
            Some("pigeonpost-cli")
        );
        m.seen
            .lock()
            .unwrap()
            .revoked
            .push(f.get("token").cloned().unwrap_or_default());
        StatusCode::OK
    }

    async fn serve(outcome: Outcome) -> (Arc<Mock>, String) {
        let mock = Arc::new(Mock {
            outcome,
            seen: Mutex::new(Seen::default()),
            base: Mutex::new(String::new()),
        });
        let app = Router::new()
            .route("/v1/device-pairings", post(pairings))
            .route("/v1/device-pairings/claim", post(claim))
            .route("/v1/enrollments/redeem", post(redeem))
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/revoke", post(revoke))
            .with_state(mock.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        *mock.base.lock().unwrap() = base.clone();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (mock, base)
    }

    fn write_session(home: &Path, issuer: &str) {
        let _directory = PrivateDirectory::open_or_create(home).unwrap();
        write_private_file_atomic(
            &home.join("auth.json"),
            &serde_json::to_vec(&serde_json::json!({
                "issuer": issuer, "refresh_token": "rt_secret", "access_token": "at",
                "expires_at": 0,
            }))
            .unwrap(),
            MAX_DEVICE_KEYS_BYTES,
        )
        .unwrap();
    }

    fn opts<'a>(home: &'a Path, base: &'a str) -> Options<'a> {
        Options {
            home,
            postbox: base,
            label: Some("Build box"),
            workspace: None,
            onboard: false,
            json: true,
        }
    }

    #[tokio::test]
    async fn device_flow_waits_claims_stores_and_downgrades() {
        let (mock, base) = serve(Outcome::Approve).await;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("agent");
        write_session(&home, &base);
        login_device_handle(&opts(&home, &base), "/acme/agent1")
            .await
            .unwrap();
        let seen = mock.seen.lock().unwrap();
        assert_eq!(seen.claims, 3, "polled through two pending answers");
        assert_eq!(seen.revoked, vec!["rt_secret".to_string()]);
        drop(seen);
        assert!(!home.join("auth.json").exists(), "account session deleted");
        let creds: serde_json::Value =
            serde_json::from_slice(&std::fs::read(home.join("postbox.json")).unwrap()).unwrap();
        assert_eq!(
            creds["identities"][0]["capability_token"],
            "cap_0123456789abcdef"
        );
        assert_eq!(creds["identities"][0]["handle"], "/acme/agent1");
        let keys: serde_json::Value =
            serde_json::from_slice(&std::fs::read(home.join("device-keys.json")).unwrap()).unwrap();
        assert_eq!(keys["devices"][0]["address"], "/k/agentaddr");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for f in ["postbox.json", "device-keys.json"] {
                let mode = std::fs::metadata(home.join(f))
                    .unwrap()
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o077, 0, "{f} is owner-only");
            }
        }
    }

    #[tokio::test]
    async fn denied_and_expired_pairings_store_nothing() {
        for (outcome, want) in [(Outcome::Deny, "declined"), (Outcome::Expire, "expired")] {
            let (_mock, base) = serve(outcome).await;
            let dir = tempfile::tempdir().unwrap();
            let home = dir.path().join("agent");
            write_session(&home, &base);
            let err = login_device_handle(&opts(&home, &base), "/acme/agent1")
                .await
                .unwrap_err()
                .to_string();
            assert!(err.contains(want), "{err}");
            assert!(!home.join("postbox.json").exists());
            assert!(home.join("auth.json").exists(), "no downgrade on failure");
        }
    }

    #[tokio::test]
    async fn enrollment_redeems_and_refuses_bad_codes() {
        let (mock, base) = serve(Outcome::Approve).await;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("agent");
        let err = login_enroll(&opts(&home, &base), "enr_bad")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("not valid"), "{err}");
        assert!(!home.join("postbox.json").exists());

        write_session(&home, &base);
        login_enroll(&opts(&home, &base), " enr_good\n")
            .await
            .unwrap();
        assert!(mock.seen.lock().unwrap().enroll_ok);
        assert!(!home.join("auth.json").exists());
        let creds: serde_json::Value =
            serde_json::from_slice(&std::fs::read(home.join("postbox.json")).unwrap()).unwrap();
        assert_eq!(creds["identities"][0]["capability_token"], "cap_enrolled");
    }

    #[tokio::test]
    async fn corrupt_credentials_are_refused_before_consuming_enrollment() {
        let (mock, base) = serve(Outcome::Approve).await;
        let dir = tempfile::tempdir().unwrap();
        let damaged = b"{\"secret\":\"private-marker\",invalid}";
        for name in ["device-keys.json", "postbox.json"] {
            let home = dir.path().join(name);
            let _directory = PrivateDirectory::open_or_create(&home).unwrap();
            write_private_file_atomic(&home.join(name), damaged, MAX_DEVICE_KEYS_BYTES).unwrap();
            let error = login_enroll(&opts(&home, &base), "enr_good")
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains(&format!("{name} is invalid")), "{error}");
            assert!(!error.contains("private-marker"));
            assert_eq!(std::fs::read(home.join(name)).unwrap(), damaged);
            assert!(!mock.seen.lock().unwrap().enroll_ok);
        }
    }

    #[tokio::test]
    async fn oversized_keys_are_refused_before_consuming_enrollment() {
        let (mock, base) = serve(Outcome::Approve).await;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("agent");
        let _directory = PrivateDirectory::open_or_create(&home).unwrap();
        let oversized = vec![b' '; MAX_DEVICE_KEYS_BYTES as usize + 1];
        write_private_file_atomic(
            &home.join(DEVICE_KEYS_FILE),
            &oversized,
            MAX_DEVICE_KEYS_BYTES + 1,
        )
        .unwrap();
        assert!(login_enroll(&opts(&home, &base), "enr_good").await.is_err());
        assert!(!mock.seen.lock().unwrap().enroll_ok);
        assert!(!home.join("postbox.json").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn linked_credentials_and_nonprivate_homes_are_refused_before_enrollment() {
        use std::os::unix::{fs::symlink, fs::PermissionsExt};

        let (mock, base) = serve(Outcome::Approve).await;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("unrelated");
        std::fs::write(&target, b"unchanged").unwrap();
        for name in ["device-keys.json", "postbox.json"] {
            let home = dir.path().join(name);
            let _directory = PrivateDirectory::open_or_create(&home).unwrap();
            symlink(&target, home.join(name)).unwrap();
            assert!(login_enroll(&opts(&home, &base), "enr_good").await.is_err());
            assert_eq!(std::fs::read(&target).unwrap(), b"unchanged");
            assert!(!mock.seen.lock().unwrap().enroll_ok);
        }
        let home = dir.path().join("shared");
        let _directory = PrivateDirectory::open_or_create(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o755)).unwrap();
        let error = login_enroll(&opts(&home, &base), "enr_good")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("private PIGEONPOST_HOME"));
        assert!(!mock.seen.lock().unwrap().enroll_ok);
        assert_eq!(
            std::fs::metadata(&home).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn predictable_temporary_links_cannot_redirect_credential_writes() {
        use std::os::unix::fs::symlink;

        let (_mock, base) = serve(Outcome::Approve).await;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("agent");
        let _directory = PrivateDirectory::open_or_create(&home).unwrap();
        write_session(&home, &base);
        let target = dir.path().join("unrelated");
        std::fs::write(&target, b"unchanged").unwrap();
        for name in ["device-keys.json.tmp", "postbox.json.tmp"] {
            symlink(&target, home.join(name)).unwrap();
        }
        login_enroll(&opts(&home, &base), "enr_good").await.unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"unchanged");
        assert_eq!(load_device_keys(&home).unwrap().devices.len(), 1);
        assert!(
            read_private_file_bounded(&home.join("postbox.json"), MAX_DEVICE_KEYS_BYTES).is_ok()
        );
        for name in ["device-keys.json.tmp", "postbox.json.tmp"] {
            assert!(std::fs::symlink_metadata(home.join(name))
                .unwrap()
                .file_type()
                .is_symlink());
        }
    }

    #[test]
    fn handle_shape_is_checked_before_any_request() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let err = rt
            .block_on(login_device_handle(
                &opts(dir.path(), "http://127.0.0.1:9"),
                "acme",
            ))
            .unwrap_err()
            .to_string();
        assert!(err.contains("/acme/agent1"));
    }

    /// Against a real postbox: `PIGEONPOST_DEVICE_LOGIN_TEST_POSTBOX=http://127.0.0.1:8990
    /// PIGEONPOST_DEVICE_LOGIN_TEST_ENROLL=enr_… cargo test -p pigeonpost-cli live_enrollment --
    /// --ignored`.
    #[tokio::test]
    #[ignore]
    async fn live_enrollment() {
        let base = std::env::var("PIGEONPOST_DEVICE_LOGIN_TEST_POSTBOX").expect("postbox url");
        let code = std::env::var("PIGEONPOST_DEVICE_LOGIN_TEST_ENROLL").expect("enrollment code");
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("agent");
        login_enroll(&opts(&home, &base), &code).await.unwrap();
        let creds: serde_json::Value =
            serde_json::from_slice(&std::fs::read(home.join("postbox.json")).unwrap()).unwrap();
        let token = creds["identities"][0]["capability_token"].as_str().unwrap();
        let who = reqwest::Client::new()
            .get(format!("{base}/v1/whoami"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        assert!(
            who.status().is_success(),
            "the credential works on /v1/whoami"
        );
    }
}
