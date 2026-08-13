//! `pigeonpost postbox new` — mint a hosted inbox on a postbox in one command.
//!
//! The bootstrap problem this solves: an agent with no credentials cannot call the postbox MCP
//! connector, because that connector is itself authenticated by the capability token a mailbox
//! hands out. Before this command the only way in was a human relaying an account API key between
//! boxes — a shared secret copied by hand onto every machine.
//!
//! So the client does what the onboarding web page does: fetch a proof-of-work challenge, solve it
//! locally (milliseconds), and POST it back. No account, no shared secret, no human. The server's
//! per-IP mint budget, not a secret, is what bounds abuse.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

type Error = Box<dyn std::error::Error>;

/// The hosted postbox agents share by default.
pub const DEFAULT_POSTBOX: &str = "https://postbox.pigeonpost.dev";

/// Where minted credentials land, under the agent's home.
const CREDENTIALS_FILE: &str = "postbox.json";

/// Refuse absurd difficulty rather than spinning forever if a server misreports it. 26 bits is the
/// postbox's own configured ceiling; ~10^8 hashes, seconds of CPU at worst.
const MAX_POW_BITS: u32 = 30;

const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
struct Challenge {
    challenge: String,
    bits: u32,
}

#[derive(Deserialize)]
struct Minted {
    address: String,
    capability_token: String,
}

/// One hosted mailbox this machine owns.
#[derive(Serialize, Deserialize, Clone)]
pub struct Credential {
    pub base_url: String,
    pub address: String,
    /// Full mailbox access. Treat as a password: never printed by any other command, never logged.
    pub capability_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub created_at: u64,
}

/// The credentials file: an append-only list, because one box legitimately runs many agents.
#[derive(Serialize, Deserialize, Default)]
struct Credentials {
    identities: Vec<Credential>,
}

/// Mint an inbox on `base_url` and record it. Prints the address plus paste-ready MCP config.
pub async fn new_inbox(
    home: &Path,
    base_url: &str,
    label: Option<&str>,
    json: bool,
) -> Result<(), Error> {
    let base = base_url.trim_end_matches('/').to_string();
    let http = reqwest::Client::builder().timeout(HTTP_TIMEOUT).build()?;

    let challenge: Challenge = http
        .get(format!("{base}/v1/pow/challenge"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if challenge.bits > MAX_POW_BITS {
        return Err(format!(
            "{base} asked for a {}-bit proof-of-work; refusing above {MAX_POW_BITS}",
            challenge.bits
        )
        .into());
    }

    // Hashing is CPU-bound: keep it off the async runtime's worker.
    let puzzle = challenge.challenge.clone();
    let bits = challenge.bits;
    let solution = tokio::task::spawn_blocking(move || solve(&puzzle, bits)).await?;

    let mut body = serde_json::json!({
        "pow_challenge": challenge.challenge,
        "pow_solution": solution,
    });
    if let Some(l) = label {
        body["label"] = serde_json::json!(l);
    }

    let resp = http
        .post(format!("{base}/v1/identities"))
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let detail: serde_json::Value = resp.json().await.unwrap_or_default();
        let message = detail["detail"]
            .as_str()
            .or_else(|| detail["error"].as_str())
            .unwrap_or("no detail")
            .to_string();
        return Err(format!("{base} refused the mint ({status}): {message}").into());
    }
    let minted: Minted = resp.json().await?;

    let credential = Credential {
        base_url: base.clone(),
        address: minted.address.clone(),
        capability_token: minted.capability_token.clone(),
        label: label.map(str::to_string),
        created_at: now_unix(),
    };
    let path = save(home, &credential)?;

    if json {
        // The token is deliberately absent: `--json` output is the form most likely to be piped
        // into a log or a transcript. It is on disk at `path` for whoever needs it.
        println!(
            "{}",
            serde_json::json!({
                "address": minted.address,
                "base_url": base,
                "label": label,
                "credentials": path.display().to_string(),
            })
        );
        return Ok(());
    }

    println!("inbox {} minted on {}", minted.address, base);
    println!("capability token saved to {}", path.display());
    println!();
    println!("Connect an agent to it:");
    println!();
    println!("  Claude Code:");
    println!(
        "    claude mcp add --transport http -s user pigeonpost {base}/mcp \\\n      --header \"Authorization: Bearer $(pigeonpost postbox token {})\"",
        minted.address
    );
    println!();
    println!("  Codex — in ~/.codex/config.toml:");
    println!("    [mcp_servers.pigeonpost]");
    println!("    url = \"{base}/mcp\"");
    println!(
        "    http_headers = {{ Authorization = \"Bearer <token from {}>\" }}",
        path.display()
    );
    println!();
    println!("The token is full access to this mailbox — treat it as a password.");
    println!("Restart the agent session afterwards; MCP servers only load at start-up.");
    Ok(())
}

/// Pick which of this home's mailboxes to act as. Refuses to guess when several are on file — the
/// wrong guess would apply a trust change to somebody else's inbox.
fn credential_for(home: &Path, address: Option<&str>) -> Result<Credential, Error> {
    let creds = load(home)?;
    let found = match address {
        Some(a) => creds.identities.iter().find(|c| c.address == a),
        None if creds.identities.len() == 1 => creds.identities.first(),
        None if creds.identities.is_empty() => {
            return Err("no hosted mailboxes on file — run: pigeonpost postbox new".into())
        }
        None => {
            return Err(format!(
                "{} mailboxes on file — name one with --as /k/…",
                creds.identities.len()
            )
            .into())
        }
    };
    found
        .cloned()
        .ok_or_else(|| "no such mailbox in this home".into())
}

/// Print the capability token for `address` (or the only one on file). Kept separate from `new` so
/// a token is never re-printed by accident — the caller has to ask for it by name.
pub fn print_token(home: &Path, address: Option<&str>) -> Result<(), Error> {
    let credential = credential_for(home, address)?;
    // Bare, no trailing prose: this is meant to be captured by `$(…)`.
    println!("{}", credential.capability_token);
    Ok(())
}

/// Add or amend a contact on the hosted inbox.
///
/// This is the *human* path. The postbox refuses to raise trust for a request that arrives over
/// MCP, so `--auto` only works from here — from a terminal, by someone holding the token.
pub async fn set_contact(
    home: &Path,
    as_address: Option<&str>,
    peer: &str,
    alias: Option<&str>,
    admission: Option<&str>,
    autonomy: Option<&str>,
    json: bool,
) -> Result<(), Error> {
    let credential = credential_for(home, as_address)?;
    let mut body = serde_json::json!({ "peer": peer });
    for (k, v) in [
        ("alias", alias),
        ("admission", admission),
        ("autonomy", autonomy),
    ] {
        if let Some(v) = v {
            body[k] = serde_json::json!(v);
        }
    }
    let value = request(
        &credential,
        reqwest::Method::PUT,
        "/v1/contacts",
        Some(body),
    )
    .await?;

    if json {
        println!("{value}");
        return Ok(());
    }
    println!(
        "{} → admission {}, autonomy {}",
        value["peer"].as_str().unwrap_or(peer),
        value["admission"].as_str().unwrap_or("?"),
        value["autonomy"].as_str().unwrap_or("?"),
    );
    if autonomy == Some("auto") {
        println!();
        println!("Note: `auto` means this agent may act on that sender's messages without asking");
        println!("you first. Their address is proven, but nothing checks that what they send is");
        println!("safe to obey — a compromised or confused peer inherits your agent's reach.");
    }
    Ok(())
}

/// Forget a contact; the peer reverts to whatever strangers get.
pub async fn forget_contact(
    home: &Path,
    as_address: Option<&str>,
    peer: &str,
    json: bool,
) -> Result<(), Error> {
    let credential = credential_for(home, as_address)?;
    let path = format!("/v1/contacts?peer={}", urlencode(peer));
    let value = request(&credential, reqwest::Method::DELETE, &path, None).await?;
    if json {
        println!("{value}");
    } else if value["removed"].as_bool().unwrap_or(false) {
        println!("forgot {peer}");
    } else {
        println!("{peer} was not a contact");
    }
    Ok(())
}

/// Show the hosted inbox's contacts and its stranger-defaults.
pub async fn show_contacts(home: &Path, as_address: Option<&str>, json: bool) -> Result<(), Error> {
    let credential = credential_for(home, as_address)?;
    let value = request(&credential, reqwest::Method::GET, "/v1/contacts", None).await?;
    if json {
        println!("{value}");
        return Ok(());
    }
    let policy = &value["policy"];
    println!(
        "{}: strangers {}, known senders {}",
        credential.address,
        if policy["accept_all"].as_bool().unwrap_or(true) {
            "accepted"
        } else {
            "refused"
        },
        if policy["auto_accept_known"].as_bool().unwrap_or(false) {
            "auto"
        } else {
            "reviewed"
        },
    );
    let contacts = value["contacts"].as_array().cloned().unwrap_or_default();
    if contacts.is_empty() {
        println!("no contacts yet");
        return Ok(());
    }
    for c in contacts {
        println!(
            "  {}  {}  {}/{}",
            c["peer"].as_str().unwrap_or("?"),
            c["alias"].as_str().unwrap_or("-"),
            c["admission"].as_str().unwrap_or("?"),
            c["autonomy"].as_str().unwrap_or("?"),
        );
    }
    Ok(())
}

/// Set the inbox's stranger-defaults.
pub async fn set_policy(
    home: &Path,
    as_address: Option<&str>,
    accept_all: Option<bool>,
    auto_accept_known: Option<bool>,
    json: bool,
) -> Result<(), Error> {
    if accept_all.is_none() && auto_accept_known.is_none() {
        return Err("nothing to set — pass --accept-all and/or --auto-accept-known".into());
    }
    let credential = credential_for(home, as_address)?;
    let mut body = serde_json::json!({});
    if let Some(v) = accept_all {
        body["accept_all"] = serde_json::json!(v);
    }
    if let Some(v) = auto_accept_known {
        body["auto_accept_known"] = serde_json::json!(v);
    }
    let value = request(&credential, reqwest::Method::PUT, "/v1/policy", Some(body)).await?;
    if json {
        println!("{value}");
        return Ok(());
    }
    println!(
        "accept_all={} auto_accept_known={}",
        value["accept_all"], value["auto_accept_known"]
    );
    if auto_accept_known == Some(true) {
        println!();
        println!("Note: every contact you have added may now drive this agent without asking you.");
        println!(
            "Review `pigeonpost postbox contacts` and remove anyone who should not have that."
        );
    }
    Ok(())
}

/// One authenticated call to the postbox, with the mailbox's capability token.
async fn request(
    credential: &Credential,
    method: reqwest::Method,
    path: &str,
    body: Option<serde_json::Value>,
) -> Result<serde_json::Value, Error> {
    let http = reqwest::Client::builder().timeout(HTTP_TIMEOUT).build()?;
    let mut req = http
        .request(method, format!("{}{path}", credential.base_url))
        .bearer_auth(&credential.capability_token);
    if let Some(b) = body {
        req = req.json(&b);
    }
    let resp = req.send().await?;
    let status = resp.status();
    let value: serde_json::Value = resp.json().await.unwrap_or_default();
    if !status.is_success() {
        let detail = value["detail"]
            .as_str()
            .or_else(|| value["error"].as_str())
            .unwrap_or("no detail");
        return Err(format!("{} refused ({status}): {detail}", credential.base_url).into());
    }
    Ok(value)
}

/// Percent-encode a query value. Addresses are `/k/` + base32, so only the slashes need it, but
/// encoding the whole conservative set keeps a hand-typed peer from splitting the query.
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

/// List the mailboxes minted from this home. Never prints tokens.
pub fn list(home: &Path, json: bool) -> Result<(), Error> {
    let creds = load(home)?;
    if json {
        let rows: Vec<_> = creds
            .identities
            .iter()
            .map(|c| {
                serde_json::json!({
                    "address": c.address,
                    "base_url": c.base_url,
                    "label": c.label,
                    "created_at": c.created_at,
                })
            })
            .collect();
        println!("{}", serde_json::json!({ "identities": rows }));
        return Ok(());
    }
    if creds.identities.is_empty() {
        println!("no hosted mailboxes yet — run: pigeonpost postbox new");
        return Ok(());
    }
    for c in &creds.identities {
        let label = c.label.as_deref().unwrap_or("-");
        println!("{}  {}  {}", c.address, label, c.base_url);
    }
    Ok(())
}

/// Hashcash: find a nonce whose `SHA-256(challenge ":" nonce)` starts with `bits` zero bits.
fn solve(challenge: &str, bits: u32) -> String {
    let prefix = format!("{challenge}:");
    let mut nonce: u64 = 0;
    loop {
        let mut h = Sha256::new();
        h.update(prefix.as_bytes());
        h.update(nonce.to_string().as_bytes());
        if leading_zero_bits(&h.finalize()) >= bits {
            return nonce.to_string();
        }
        nonce += 1;
    }
}

fn leading_zero_bits(bytes: &[u8]) -> u32 {
    let mut count = 0;
    for &b in bytes {
        if b == 0 {
            count += 8;
        } else {
            return count + b.leading_zeros();
        }
    }
    count
}

fn credentials_path(home: &Path) -> PathBuf {
    home.join(CREDENTIALS_FILE)
}

fn load(home: &Path) -> Result<Credentials, Error> {
    let path = credentials_path(home);
    match std::fs::read(&path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)
            .map_err(|e| format!("{} is not readable as credentials: {e}", path.display()))?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Credentials::default()),
        Err(e) => Err(format!("reading {}: {e}", path.display()).into()),
    }
}

/// Append a credential, owner-only. Written via a temp file + rename so a crash can't leave a
/// half-written file where the previous tokens used to be.
fn save(home: &Path, credential: &Credential) -> Result<PathBuf, Error> {
    std::fs::create_dir_all(home)?;
    let path = credentials_path(home);
    let mut creds = load(home)?;
    creds.identities.push(credential.clone());
    let body = serde_json::to_vec_pretty(&creds)?;

    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&tmp)?;
    restrict(&file)?;
    file.write_all(&body)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

#[cfg(unix)]
fn restrict(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict(_file: &std::fs::File) -> std::io::Result<()> {
    // Windows inherits the parent directory's ACL, which is the user profile — owner-only already.
    Ok(())
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solution_meets_the_stated_difficulty() {
        let challenge = "abc.123.12.deadbeef";
        let solution = solve(challenge, 12);
        let mut h = Sha256::new();
        h.update(format!("{challenge}:{solution}").as_bytes());
        assert!(leading_zero_bits(&h.finalize()) >= 12);
    }

    #[test]
    fn counts_leading_zero_bits() {
        assert_eq!(leading_zero_bits(&[0xff]), 0);
        assert_eq!(leading_zero_bits(&[0x0f]), 4);
        assert_eq!(leading_zero_bits(&[0x00, 0x80]), 8);
        assert_eq!(leading_zero_bits(&[0x00, 0x00]), 16);
    }

    #[test]
    fn credentials_round_trip_and_append() {
        let home = tempfile::tempdir().unwrap();
        let a = Credential {
            base_url: "https://postbox.example".into(),
            address: "/k/aaa".into(),
            capability_token: "cap_a".into(),
            label: Some("agent-A".into()),
            created_at: 1,
        };
        let b = Credential {
            address: "/k/bbb".into(),
            capability_token: "cap_b".into(),
            label: None,
            ..a.clone()
        };
        save(home.path(), &a).unwrap();
        let path = save(home.path(), &b).unwrap();

        let loaded = load(home.path()).unwrap();
        assert_eq!(loaded.identities.len(), 2, "minting must not clobber");
        assert_eq!(loaded.identities[0].address, "/k/aaa");
        assert_eq!(loaded.identities[1].capability_token, "cap_b");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "tokens must be owner-only");
        }
        let _ = path;
    }

    #[test]
    fn token_lookup_requires_a_name_when_ambiguous() {
        let home = tempfile::tempdir().unwrap();
        let a = Credential {
            base_url: "https://postbox.example".into(),
            address: "/k/aaa".into(),
            capability_token: "cap_a".into(),
            label: None,
            created_at: 1,
        };
        save(home.path(), &a).unwrap();
        assert!(print_token(home.path(), None).is_ok());

        save(
            home.path(),
            &Credential {
                address: "/k/bbb".into(),
                ..a.clone()
            },
        )
        .unwrap();
        assert!(print_token(home.path(), None).is_err());
        assert!(print_token(home.path(), Some("/k/bbb")).is_ok());
        assert!(print_token(home.path(), Some("/k/zzz")).is_err());
    }
}
