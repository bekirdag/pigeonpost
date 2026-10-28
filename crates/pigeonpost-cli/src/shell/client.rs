//! `pigeonpost ssh <agent>` — a terminal on an agent's workspace (SDS §11.3 "Client").
//!
//! 1. Ask the postbox for a session with this person's sign-in. Opening a shell needs a recent
//!    strong sign-in (SDS §3.4): when the postbox answers `step_up_required`, sign in again in the
//!    browser with the requested `acr_values` and `max_age=0`. That token is used for this request
//!    only and never saved.
//! 2. Make a fresh Ed25519 key in memory; the postbox certifies its public half for five minutes,
//!    for this session only. The private half never leaves this process.
//! 3. Check the agent's host key: the postbox's pinned fingerprint must equal the one this machine
//!    pinned for that agent the first time. A change is refused unless `--accept-host-key` names
//!    the new fingerprint (compare it with `pigeonpost agentd shell` on the agent machine).
//! 4. Reach the broker, present the client leg's one-time token, and speak SSH end to end over it:
//!    the server must present exactly the pinned host key, and is shown the certificate.

use super::Error;
use russh::client::{Config, Handler};
use russh::keys::{Certificate, PrivateKey, PublicKey};
use russh::ChannelMsg;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const PINS_FILE: &str = "ssh-known-agents.json";

pub struct Options<'a> {
    pub home: &'a Path,
    pub postbox: &'a str,
    pub agent: &'a str,
    /// Run one command instead of an interactive shell (required for `allowlist` agents).
    pub command: Option<String>,
    /// Accept a changed host key whose fingerprint is exactly this.
    pub accept_host_key: Option<String>,
}

struct Pinned {
    fingerprint: String,
}

impl Handler for Pinned {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        // A plain key, and exactly the pinned one. Host certificates are not part of this design.
        Ok(match key {
            russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } => {
                super::fingerprint(key) == self.fingerprint
            }
            _ => false,
        })
    }
}

fn http() -> Result<reqwest::Client, Error> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("pigeonpost-cli/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

async fn call(
    method: reqwest::Method,
    url: &str,
    token: &str,
    body: Option<Value>,
) -> Result<(u16, Value), Error> {
    let mut req = http()?.request(method, url).bearer_auth(token);
    if let Some(b) = body {
        req = req.json(&b);
    }
    let resp = req.send().await?;
    let status = resp.status().as_u16();
    Ok((status, resp.json().await.unwrap_or(Value::Null)))
}

fn refusal(v: &Value) -> String {
    let code = v["error"].as_str().unwrap_or("error");
    let msg = v["message"].as_str().unwrap_or("");
    let hint = match code {
        "delegation_required" => " — ask an agent admin for an `ssh` delegation on this agent",
        "ssh_not_allowed" => {
            " — an org owner or admin must allow SSH in the organization's settings"
        }
        "ssh_not_enabled" => " — an agent admin must enable SSH on this agent",
        "agent_offline" => " — the agent's machine is not running agentd, or it is not connected",
        "shell_not_offered" => {
            " — the agent's machine has not enabled [mailbox.shell] in agentd.toml"
        }
        "host_key_pending" => {
            " — the agent presented a new host key; someone with authority over it must approve it"
        }
        "feature_disabled" => " — agent SSH is not enabled for your account yet",
        "human_required" => {
            " — only a person's own sign-in can open a shell (not an API key or an agent)"
        }
        _ => "",
    };
    format!("{code}: {msg}{hint}")
}

fn load_pins(home: &Path) -> serde_json::Map<String, Value> {
    std::fs::read(home.join(PINS_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn save_pin(home: &Path, agent: &str, fp: &str) -> Result<(), Error> {
    let mut pins = load_pins(home);
    pins.insert(agent.to_string(), json!(fp));
    std::fs::create_dir_all(home)?;
    let tmp = home.join(format!("{PINS_FILE}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(&Value::Object(pins))?)?;
    std::fs::rename(&tmp, home.join(PINS_FILE))?;
    Ok(())
}

/// Decide whether to trust the host key the postbox reports for `agent`.
pub fn host_key_decision(
    pinned: Option<&str>,
    offered: &str,
    accept: Option<&str>,
) -> Result<bool, String> {
    match pinned {
        None => Ok(true),
        Some(p) if p == offered => Ok(false),
        Some(p) => {
            if accept == Some(offered) {
                Ok(true)
            } else {
                Err(format!(
                    "WARNING: the agent's host key changed.\n  pinned:  {p}\n  offered: {offered}\n\
                     This is expected only if the agent's machine was reinstalled and its new key approved.\n\
                     Check the fingerprint on the agent machine (`pigeonpost agentd shell`), then rerun with\n\
                     --accept-host-key {offered}"
                ))
            }
        }
    }
}

/// The terminal size, or 80×24 when there is no terminal.
fn term_size() -> (u32, u32) {
    #[cfg(unix)]
    {
        if let Ok(ws) = rustix::termios::tcgetwinsize(std::io::stdout()) {
            if ws.ws_col > 0 && ws.ws_row > 0 {
                return (u32::from(ws.ws_col), u32::from(ws.ws_row));
            }
        }
    }
    (80, 24)
}

fn interactive() -> bool {
    #[cfg(unix)]
    {
        rustix::termios::isatty(std::io::stdin()) && rustix::termios::isatty(std::io::stdout())
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Raw mode for the local terminal while the session runs; restored on drop.
#[cfg(unix)]
struct Raw(Option<rustix::termios::Termios>);

#[cfg(unix)]
impl Raw {
    fn enter() -> Raw {
        let stdin = std::io::stdin();
        match rustix::termios::tcgetattr(&stdin) {
            Ok(old) => {
                let mut raw = old.clone();
                raw.make_raw();
                let _ =
                    rustix::termios::tcsetattr(&stdin, rustix::termios::OptionalActions::Now, &raw);
                Raw(Some(old))
            }
            Err(_) => Raw(None),
        }
    }
}

#[cfg(unix)]
impl Drop for Raw {
    fn drop(&mut self) {
        if let Some(old) = self.0.take() {
            let _ = rustix::termios::tcsetattr(
                std::io::stdin(),
                rustix::termios::OptionalActions::Now,
                &old,
            );
        }
    }
}

/// The access token to ask with: the saved session's, or — when the postbox wants a step-up — a
/// fresh strong one from the browser.
async fn request_session(
    opts: &Options<'_>,
    base: &str,
    body: &Value,
) -> Result<(Value, String), Error> {
    let mut token = crate::login_cmd::access_token(opts.home)
        .await
        .map_err(|e| e.to_string())?;
    for attempt in 0..2 {
        let (s, v) = call(
            reqwest::Method::POST,
            &format!("{base}/v1/shell/sessions"),
            &token,
            Some(body.clone()),
        )
        .await?;
        match s {
            201 | 202 => return Ok((v, token)),
            401 if v["error"] == "step_up_required" && attempt == 0 => {
                let acr = v["acr_values"].as_str().unwrap_or("").to_string();
                eprintln!("Opening a shell needs a fresh two-step sign-in.");
                token = crate::login_cmd::step_up_token(opts.home, &acr)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            _ => return Err(format!("the postbox refused: {}", refusal(&v)).into()),
        }
    }
    Err("the postbox kept asking for a stronger sign-in".into())
}

pub async fn run(opts: Options<'_>) -> Result<i32, Error> {
    let base = opts.postbox.trim_end_matches('/').to_string();
    let key: PrivateKey = super::new_ed25519();
    let (cols, rows) = term_size();
    let body = json!({ "agent": opts.agent, "client_public_key": key.public_key().to_openssh()?,
                       "cols": cols.clamp(20, 500), "rows": rows.clamp(5, 300) });
    let (mut v, token) = request_session(&opts, &base, &body).await?;
    let sid = v["session_id"].as_str().unwrap_or("").to_string();
    if !super::valid_session_id(&sid) {
        return Err("the postbox returned no session".into());
    }
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    if v["state"] == "awaiting_approval" {
        eprintln!("Waiting for an agent admin to approve this session ({sid})…");
        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                _ = &mut ctrl_c => {
                    let _ = call(reqwest::Method::DELETE, &format!("{base}/v1/shell/sessions/{sid}"), &token, None).await;
                    return Err("cancelled".into());
                }
            }
            let (_, s) = call(
                reqwest::Method::GET,
                &format!("{base}/v1/shell/sessions/{sid}"),
                &token,
                None,
            )
            .await?;
            match s["session"]["state"].as_str() {
                Some("approved") => break,
                Some("awaiting_approval") => continue,
                _ => {
                    return Err(format!(
                        "the session was not approved ({})",
                        s["session"]["close_reason"].as_str().unwrap_or("closed")
                    )
                    .into())
                }
            }
        }
        let (s, c) = call(
            reqwest::Method::POST,
            &format!("{base}/v1/shell/sessions/{sid}/connect"),
            &token,
            Some(json!({})),
        )
        .await?;
        if s != 201 {
            return Err(format!("the postbox refused: {}", refusal(&c)).into());
        }
        v = c;
    }

    let agent = v["session"]["agent"]
        .as_str()
        .unwrap_or(opts.agent)
        .to_string();
    let offered = v["host_key_fp"].as_str().unwrap_or("").to_string();
    let host_key = PublicKey::from_openssh(v["host_key"].as_str().unwrap_or(""))
        .map_err(|_| "the postbox returned an invalid host key")?;
    if super::fingerprint(&host_key) != offered {
        return Err("the host key and its fingerprint disagree; refusing".into());
    }
    let pins = load_pins(opts.home);
    let pinned = pins.get(&agent).and_then(|p| p.as_str());
    match host_key_decision(pinned, &offered, opts.accept_host_key.as_deref()) {
        Ok(true) => {
            if pinned.is_none() {
                eprintln!("First connection to {agent}: pinning its host key {offered}");
                eprintln!(
                    "(the agent machine's owner can confirm it with `pigeonpost agentd shell`)"
                );
            }
            save_pin(opts.home, &agent, &offered)?;
        }
        Ok(false) => {}
        Err(msg) => {
            let _ = call(
                reqwest::Method::DELETE,
                &format!("{base}/v1/shell/sessions/{sid}"),
                &token,
                None,
            )
            .await;
            return Err(msg.into());
        }
    }
    let cert = Certificate::from_openssh(v["user_cert"].as_str().unwrap_or(""))
        .map_err(|_| "the postbox returned an invalid certificate")?;
    let url = v["broker_url"].as_str().unwrap_or("");
    if !super::broker_url_ok(url, &super::tunnel_origin(), &sid) {
        return Err(format!("refusing an unexpected tunnel address: {url}").into());
    }
    let leg = v["leg_token"].as_str().unwrap_or("").to_string();

    let code = session(url, &leg, &offered, key, cert, opts.command.clone()).await;
    // Tell the postbox it is over (it would learn from the broker too).
    let _ = call(
        reqwest::Method::DELETE,
        &format!("{base}/v1/shell/sessions/{sid}"),
        &token,
        None,
    )
    .await;
    code
}

async fn session(
    url: &str,
    leg: &str,
    host_fp: &str,
    key: PrivateKey,
    cert: Certificate,
    command: Option<String>,
) -> Result<i32, Error> {
    let (stream, mut closed) = super::dial(url, leg).await?;
    let config = Arc::new(Config {
        inactivity_timeout: None,
        keepalive_interval: Some(Duration::from_secs(30)),
        ..Default::default()
    });
    let mut handle = russh::client::connect_stream(
        config,
        stream,
        Pinned {
            fingerprint: host_fp.to_string(),
        },
    )
    .await
    .map_err(|e| match e {
        russh::Error::UnknownKey => {
            "the agent presented a host key that is not the pinned one; refusing".to_string()
        }
        other => format!("could not open the SSH session: {other}"),
    })?;
    let auth = handle
        .authenticate_openssh_cert(super::SSH_USER, Arc::new(key), cert)
        .await?;
    if !auth.success() {
        return Err("the agent refused this session's certificate".into());
    }
    let channel = handle.channel_open_session().await?;
    let tty = interactive();
    if tty {
        let (cols, rows) = term_size();
        let term = std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into());
        channel
            .request_pty(false, &term, cols, rows, 0, 0, &[])
            .await?;
    }
    match &command {
        Some(c) => channel.exec(true, c.as_bytes()).await?,
        None => channel.request_shell(true).await?,
    }
    #[cfg(unix)]
    let _raw = if tty { Some(Raw::enter()) } else { None };
    let (mut rd, wr) = channel.split();
    let wr = Arc::new(wr);
    let input = {
        let wr = wr.clone();
        tokio::spawn(async move {
            let mut stdin = tokio::io::stdin();
            let mut buf = vec![0u8; 8 * 1024];
            loop {
                match stdin.read(&mut buf).await {
                    Ok(0) | Err(_) => {
                        let _ = wr.eof().await;
                        break;
                    }
                    Ok(n) => {
                        if wr.data(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                }
            }
        })
    };
    #[cfg(unix)]
    let resize = if tty {
        let wr = wr.clone();
        Some(tokio::spawn(async move {
            let Ok(mut sig) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
            else {
                return;
            };
            while sig.recv().await.is_some() {
                let (c, r) = term_size();
                let _ = wr.window_change(c, r, 0, 0).await;
            }
        }))
    } else {
        None
    };
    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();
    let mut status: Option<u32> = None;
    loop {
        tokio::select! {
            m = rd.wait() => match m {
                Some(ChannelMsg::Data { data }) => {
                    stdout.write_all(&data).await?;
                    stdout.flush().await?;
                }
                Some(ChannelMsg::ExtendedData { data, .. }) => {
                    stderr.write_all(&data).await?;
                    stderr.flush().await?;
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => status = Some(exit_status),
                Some(ChannelMsg::Close) | None => break,
                Some(_) => {}
            },
            _ = closed.changed() => break,
        }
    }
    input.abort();
    #[cfg(unix)]
    if let Some(r) = resize {
        r.abort();
    }
    let reason = closed.borrow().clone();
    if status.is_none() {
        if let Some(r) = reason {
            eprintln!("\r\nsession ended: {r}");
        }
    }
    let _ = handle
        .disconnect(russh::Disconnect::ByApplication, "", "en")
        .await;
    Ok(status.map(|s| s as i32).unwrap_or(255))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_keys_are_pinned_and_changes_need_the_exact_new_fingerprint() {
        assert_eq!(host_key_decision(None, "SHA256:a", None), Ok(true));
        assert_eq!(
            host_key_decision(Some("SHA256:a"), "SHA256:a", None),
            Ok(false)
        );
        let e = host_key_decision(Some("SHA256:a"), "SHA256:b", None).unwrap_err();
        assert!(e.contains("host key changed") && e.contains("SHA256:b"));
        assert!(host_key_decision(Some("SHA256:a"), "SHA256:b", Some("SHA256:c")).is_err());
        assert_eq!(
            host_key_decision(Some("SHA256:a"), "SHA256:b", Some("SHA256:b")),
            Ok(true)
        );
    }

    #[test]
    fn pins_persist_per_agent() {
        let t = tempfile::tempdir().unwrap();
        save_pin(t.path(), "/acme/a", "SHA256:x").unwrap();
        save_pin(t.path(), "/acme/b", "SHA256:y").unwrap();
        let p = load_pins(t.path());
        assert_eq!(p["/acme/a"], "SHA256:x");
        assert_eq!(p["/acme/b"], "SHA256:y");
    }
}
