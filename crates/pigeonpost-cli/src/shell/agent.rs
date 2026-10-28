//! agentd's side of agent SSH (SDS §11.3–§11.5): an SSH server that listens on no port.
//!
//! For each mailbox whose `[mailbox.shell]` says `enabled = true` (and validates), agentd:
//!
//! 1. reports its local opt-in, mode and **host key** to the postbox with the mailbox's own
//!    credential (`PUT /v1/shell/agent`); the postbox pins the first key and holds any later one
//!    until a person approves the rotation;
//! 2. holds the mailbox's **control stream** (`GET /v1/shell/agent/events`), on which a
//!    `shell.request` arrives when an authorized person opens a session, and `shell.revoke` when
//!    the postbox ends one;
//! 3. for each request, dials the tunnel broker **outbound** with the agent leg's one-time token and
//!    serves exactly that one session over it, accepting only a certificate from the pinned
//!    Pigeonpost SSH CA that names this session, this agent, the key and the serial announced in
//!    the request — then runs the session inside the configured boundary ([`super::confine`]).
//!
//! Only an interactive session channel is served: no port, agent or X11 forwarding, no
//! subsystems (so no SFTP/SCP), no `env` requests. Sessions end on revocation, at the absolute
//! deadline, after the idle limit, when the control stream has been gone for a minute, or when the
//! configuration changes; teardown kills the whole process group and removes the container.
//!
//! Nothing typed or shown is recorded. The local log gets metadata only: who, which mailbox, when,
//! how long, in which mode.

use super::config::{Mode, Shell};
use super::confine::{self, Launch};
use crate::agentd_cmd::log_line;
use crate::postbox_cmd::Credential;
use russh::keys::{Certificate, HashAlg, PrivateKey};
use russh::server::{Auth, Handler, Msg, Session};
use russh::{Channel, ChannelId};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type Error = Box<dyn std::error::Error + Send + Sync>;
type Bytes = russh::keys::ssh_encoding::bytes::Bytes;

const HOST_KEY_FILE: &str = "shell-host-ed25519";
const CA_PIN_FILE: &str = "shell-ca.pub";
const SHELL_LOG: &str = "agentd-shell.jsonl";
/// With the control stream gone this long, live sessions end: revocations could not reach us.
const BLIND_SECS: u64 = 60;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---- live sessions -----------------------------------------------------------------------------

struct Live {
    mailbox: String,
    kill: tokio::sync::watch::Sender<Option<String>>,
}

fn live() -> &'static Mutex<HashMap<String, Live>> {
    static LIVE: OnceLock<Mutex<HashMap<String, Live>>> = OnceLock::new();
    LIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn kill_session(sid: &str, reason: &str) -> bool {
    match live().lock().expect("live lock").get(sid) {
        Some(l) => {
            l.kill.send_if_modified(|r| {
                if r.is_none() {
                    *r = Some(reason.to_string());
                    true
                } else {
                    false
                }
            });
            true
        }
        None => false,
    }
}

fn kill_mailbox(mailbox: &str, reason: &str) {
    let sids: Vec<String> = live()
        .lock()
        .expect("live lock")
        .iter()
        .filter(|(_, l)| l.mailbox == mailbox)
        .map(|(s, _)| s.clone())
        .collect();
    for sid in sids {
        kill_session(&sid, reason);
    }
}

fn live_for(mailbox: &str) -> usize {
    live()
        .lock()
        .expect("live lock")
        .values()
        .filter(|l| l.mailbox == mailbox)
        .count()
}

// ---- keys --------------------------------------------------------------------------------------

/// This machine's SSH host key, created on first use, owner-only.
pub fn host_key(machine_home: &Path) -> Result<PrivateKey, Error> {
    use pigeonpost_directory::private_store::{
        read_private_file_bounded, write_private_file_atomic, PrivateDirectory,
    };
    let path = machine_home.join(HOST_KEY_FILE);
    match read_private_file_bounded(&path, 16 * 1024) {
        Ok(bytes) => Ok(PrivateKey::from_openssh(&*bytes)
            .map_err(|_| format!("{} is not an OpenSSH private key", path.display()))?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            PrivateDirectory::open_or_create(machine_home)?;
            let key = super::new_ed25519();
            let pem = key
                .to_openssh(russh::keys::ssh_key::LineEnding::LF)
                .map_err(|e| format!("cannot encode the host key: {e}"))?;
            write_private_file_atomic(&path, pem.as_bytes(), 16 * 1024)?;
            Ok(key)
        }
        Err(e) => Err(e.into()),
    }
}

/// The production postbox's SSH CA, pinned in this build (Phase 9): a machine talking to
/// `postbox.pigeonpost.dev` trusts exactly this key, whatever the postbox sends, a pin file says or
/// `PIGEONPOST_SSH_CA` is set to. Fingerprint `SHA256:JYtla5q8tBzb4vFjJgeUNBiHblmEdsBvlKy2G+FbfGg`.
/// A CA rotation ships in an agentd release.
pub const RELEASE_CA: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMTPGs+6N5j3BGBcmG9CzYN4mCHrpY25/SI8x8If1lvh";
const RELEASE_POSTBOX_HOST: &str = "postbox.pigeonpost.dev";

fn is_release_postbox(base: &str) -> bool {
    reqwest::Url::parse(base)
        .ok()
        .is_some_and(|u| u.scheme() == "https" && u.host_str() == Some(RELEASE_POSTBOX_HOST))
}

/// The CA public key this machine trusts. For the production postbox: the key pinned in this
/// build. For any other postbox: a key the owner supplied out of band — `PIGEONPOST_SSH_CA`, or the
/// pin file — and never one learned from the postbox on first contact.
fn pin_ca(machine_home: &Path, base: &str, offered: &str) -> Result<russh::keys::PublicKey, Error> {
    let offered_key = russh::keys::PublicKey::from_openssh(offered.trim())
        .map_err(|_| "the postbox sent an invalid SSH CA key")?;
    if is_release_postbox(base) {
        let release = russh::keys::PublicKey::from_openssh(RELEASE_CA).expect("valid release CA");
        if release.key_data() != offered_key.key_data() {
            return Err(format!(
                "the postbox's SSH CA ({}) is not the one this agentd release pins ({}); refusing all \
                 sessions. A CA rotation ships in an agentd release.",
                super::fingerprint(&offered_key),
                super::fingerprint(&release)
            )
            .into());
        }
        return Ok(release);
    }
    let path = machine_home.join(CA_PIN_FILE);
    let override_key = std::env::var("PIGEONPOST_SSH_CA").ok();
    let pinned = match override_key {
        Some(k) => Some(k),
        None => std::fs::read_to_string(&path).ok(),
    };
    match pinned {
        Some(p) => {
            let pinned = russh::keys::PublicKey::from_openssh(p.trim())
                .map_err(|_| format!("{} is not an SSH public key", path.display()))?;
            if pinned.key_data() != offered_key.key_data() {
                return Err(format!(
                    "the postbox's SSH CA ({}) is not the one this machine pinned ({}); refusing all sessions. \
                     If the CA was rotated deliberately, delete {} and restart agentd.",
                    super::fingerprint(&offered_key),
                    super::fingerprint(&pinned),
                    path.display()
                )
                .into());
            }
            Ok(pinned)
        }
        None => Err(format!(
            "this postbox offers SSH CA {}; confirm it with the postbox's operator out of band, then \
             put that public key in {} (or PIGEONPOST_SSH_CA) and restart agentd. Sessions stay \
             refused until then.",
            super::fingerprint(&offered_key),
            path.display()
        )
        .into()),
    }
}

// ---- supervision -------------------------------------------------------------------------------

/// One configured shell: which mailbox, its credential, and its checked settings.
#[derive(Clone)]
pub struct Route {
    pub mailbox: String,
    pub credential: Credential,
    pub shell: Shell,
}

/// The shells `agentd.toml` enables, validated. Invalid ones are logged and skipped, never run in
/// some weaker form.
pub fn routes(home: &Path) -> Vec<Route> {
    let machine = crate::agentd_cmd::machine_home(home);
    let Ok(config) = crate::executor::load_routing(&machine) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for r in &config.mailbox {
        let Some(sc) = r.shell.as_ref().filter(|s| s.enabled) else {
            continue;
        };
        let shell = match sc.validate(&r.workspace, &machine) {
            Ok(s) => s,
            Err(e) => {
                log_line(&machine, &format!("ssh: {} not offered: {e}", r.address));
                continue;
            }
        };
        if shell.mode == Mode::Container {
            if let Err(e) = confine::runtime(&shell) {
                log_line(&machine, &format!("ssh: {} not offered: {e}", r.address));
                continue;
            }
        }
        match crate::postbox_cmd::credential_anywhere(&machine, &r.address) {
            Ok(credential) => out.push(Route {
                mailbox: r.address.clone(),
                credential,
                shell,
            }),
            Err(e) => log_line(&machine, &format!("ssh: {} not offered: {e}", r.address)),
        }
    }
    out
}

/// Start one supervisor per enabled shell. Called once by `agentd run`.
pub fn start(home: &Path) {
    let machine = crate::agentd_cmd::machine_home(home);
    let routes = routes(home);
    if routes.is_empty() {
        return;
    }
    // One host key for the machine, loaded (or made) once, before any supervisor needs it.
    let key = match host_key(&machine) {
        Ok(k) => k,
        Err(e) => {
            log_line(&machine, &format!("ssh: no host key, SSH not offered: {e}"));
            return;
        }
    };
    for route in routes {
        let (machine, key) = (machine.clone(), key.clone());
        tokio::spawn(async move { supervise(machine, route, key).await });
    }
}

async fn supervise(machine: PathBuf, route: Route, key: PrivateKey) {
    let mut backoff = Duration::from_secs(2);
    let blind_since: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    // Fail closed: without the control stream for BLIND_SECS, revocations cannot reach us.
    {
        let blind = blind_since.clone();
        let mailbox = route.mailbox.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let since = *blind.lock().expect("blind lock");
                if since.is_some_and(|t| t.elapsed() >= Duration::from_secs(BLIND_SECS)) {
                    kill_mailbox(&mailbox, "control_stream_lost");
                }
            }
        });
    }
    loop {
        match control_once(&machine, &route, &key, &blind_since).await {
            Ok(()) => backoff = Duration::from_secs(2),
            Err(e) => {
                log_line(&machine, &format!("ssh: {}: {e}", route.mailbox));
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
        }
        blind_since
            .lock()
            .expect("blind lock")
            .get_or_insert_with(Instant::now);
        tokio::time::sleep(backoff).await;
    }
}

fn http() -> Result<reqwest::Client, Error> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("pigeonpost-agentd/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .build()?)
}

/// Report the opt-in, then hold the control stream until it ends.
async fn control_once(
    machine: &Path,
    route: &Route,
    key: &PrivateKey,
    blind_since: &Arc<Mutex<Option<Instant>>>,
) -> Result<(), Error> {
    let base = route.credential.base_url.trim_end_matches('/').to_string();
    let token = route.credential.capability_token.clone();
    let host_line = key.public_key().to_openssh()?;
    let report = http()?
        .put(format!("{base}/v1/shell/agent"))
        .bearer_auth(&token)
        .timeout(Duration::from_secs(20))
        .json(&json!({ "enabled": true, "mode": route.shell.mode.as_str(),
                       "max_sessions": route.shell.max_sessions, "host_key": host_line,
                       "version": env!("CARGO_PKG_VERSION") }))
        .send()
        .await?;
    let status = report.status();
    let body: Value = report.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(format!(
            "the postbox refused the shell report: {status} {}",
            body["error"].as_str().unwrap_or("")
        )
        .into());
    }
    if body["status"] == "host_key_pending" {
        log_line(
            machine,
            &format!(
                "ssh: {}: this machine's host key {} is not the one the postbox pinned ({}); sessions stay refused until \
                 someone with authority over the agent approves it",
                route.mailbox,
                super::fingerprint(key.public_key()),
                body["host_key_fp"].as_str().unwrap_or("?")
            ),
        );
    }

    let resp = http()?
        .get(format!("{base}/v1/shell/agent/events"))
        .bearer_auth(&token)
        .header("accept", "text/event-stream")
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(format!("the postbox refused the control stream: {}", resp.status()).into());
    }
    let mut resp = resp;
    let mut buf = String::new();
    let mut ca: Option<russh::keys::PublicKey> = None;
    log_line(
        machine,
        &format!(
            "ssh: {} offered ({}), host key {}",
            route.mailbox,
            route.shell.mode.as_str(),
            super::fingerprint(key.public_key())
        ),
    );
    loop {
        let chunk = match tokio::time::timeout(Duration::from_secs(45), resp.chunk()).await {
            Ok(Ok(Some(c))) => c,
            Ok(Ok(None)) => return Ok(()),
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => return Err("control stream went silent".into()),
        };
        *blind_since.lock().expect("blind lock") = None;
        buf.push_str(&String::from_utf8_lossy(&chunk));
        if buf.len() > 1 << 20 {
            return Err("control stream frame too large".into());
        }
        while let Some(i) = buf.find("\n\n") {
            let frame = buf[..i].to_string();
            buf.drain(..i + 2);
            let kind = frame
                .lines()
                .find_map(|l| l.strip_prefix("event:"))
                .map(str::trim);
            let data = frame
                .lines()
                .find_map(|l| l.strip_prefix("data:"))
                .and_then(|d| serde_json::from_str::<Value>(d.trim()).ok());
            let (Some(kind), Some(data)) = (kind, data) else {
                continue;
            };
            match kind {
                "shell.hello" => {
                    ca = Some(pin_ca(
                        machine,
                        &base,
                        data["ca_public_key"].as_str().unwrap_or(""),
                    )?);
                }
                "shell.request" => {
                    let Some(ca) = ca.clone() else {
                        continue;
                    };
                    let (machine, route, key) = (machine.to_path_buf(), route.clone(), key.clone());
                    tokio::spawn(async move {
                        let sid = data["session_id"].as_str().unwrap_or("").to_string();
                        if let Err(e) = serve(&machine, &route, key, ca, data).await {
                            log_line(
                                &machine,
                                &format!("ssh: {} session {sid}: {e}", route.mailbox),
                            );
                        }
                    });
                }
                "shell.revoke" => {
                    let sid = data["session_id"].as_str().unwrap_or("");
                    let reason = data["reason"].as_str().unwrap_or("revoked");
                    kill_session(sid, &format!("revoked:{reason}"));
                }
                _ => {}
            }
        }
    }
}

// ---- one session -------------------------------------------------------------------------------

/// Everything a session is bound to, from the postbox's request and this machine's config.
#[derive(Clone)]
struct Spec {
    sid: String,
    agent: String,
    principal: String,
    client_fp: String,
    serial: String,
    ca: russh::keys::PublicKey,
    shell: Shell,
}

fn shell_log(machine: &Path, entry: Value) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(machine.join(SHELL_LOG))
    {
        let _ = writeln!(f, "{entry}");
    }
}

async fn serve(
    machine: &Path,
    route: &Route,
    host: PrivateKey,
    ca: russh::keys::PublicKey,
    req: Value,
) -> Result<(), Error> {
    let s = |k: &str| req[k].as_str().unwrap_or("").to_string();
    let sid = s("session_id");
    let url = s("broker_url");
    if !super::valid_session_id(&sid) {
        return Err("malformed session id".into());
    }
    if !super::broker_url_ok(&url, &super::tunnel_origin(), &sid) {
        return Err(format!("refused: broker URL {url:?} is not this machine's tunnel").into());
    }
    if req["agent"].as_str() != Some(route.credential.address.as_str()) {
        return Err("refused: request for another agent".into());
    }
    if req["mode"].as_str() != Some(route.shell.mode.as_str()) {
        return Err("refused: the request's mode is not this machine's".into());
    }
    if req["ca_fp"].as_str() != Some(super::fingerprint(&ca).as_str()) {
        return Err("refused: signed by a CA this machine does not trust".into());
    }
    if live_for(&route.mailbox) >= route.shell.max_sessions as usize {
        return Err("refused: max_sessions reached".into());
    }
    let ends_by = req["ends_by"].as_u64().unwrap_or(0);
    if ends_by <= now() {
        return Err("refused: already past its deadline".into());
    }
    let spec = Spec {
        sid: sid.clone(),
        agent: route.credential.address.clone(),
        principal: s("principal"),
        client_fp: s("client_key_fp"),
        serial: s("cert_serial"),
        ca,
        shell: route.shell.clone(),
    };
    let (kill_tx, mut kill_rx) = tokio::sync::watch::channel(None);
    live().lock().expect("live lock").insert(
        sid.clone(),
        Live {
            mailbox: route.mailbox.clone(),
            kill: kill_tx.clone(),
        },
    );
    struct Unlist(String);
    impl Drop for Unlist {
        fn drop(&mut self) {
            live().lock().expect("live lock").remove(&self.0);
        }
    }
    let _unlist = Unlist(sid.clone());
    let started = Instant::now();
    let who = req["requester"]["handle"]
        .as_str()
        .or(req["requester"]["account"].as_str())
        .unwrap_or("someone")
        .to_string();
    log_line(
        machine,
        &format!(
            "ssh: session {sid} on {} for {who} ({}, {})",
            route.mailbox,
            req["via"].as_str().unwrap_or("?"),
            route.shell.mode.as_str()
        ),
    );
    shell_log(
        machine,
        json!({ "at": now(), "event": "open", "session_id": sid, "mailbox": route.mailbox,
                "requester": who, "via": req["via"], "mode": route.shell.mode.as_str() }),
    );
    crate::agentd_cmd::notify_shell(
        "SSH session opened",
        &format!(
            "{who} opened a {} shell on {}",
            route.shell.mode.as_str(),
            route.mailbox
        ),
    );

    let (stream, mut broker_closed) = super::dial(&url, &s("leg_token")).await?;
    let activity = Arc::new(Mutex::new(Instant::now()));
    let proc_info: Arc<Mutex<ProcInfo>> = Arc::default();
    let idle = Duration::from_secs(
        req["idle_secs"]
            .as_u64()
            .unwrap_or(900)
            .min(route.shell.idle_secs),
    );
    let config = Arc::new(russh::server::Config {
        methods: russh::MethodSet::from(&[russh::MethodKind::PublicKey][..]),
        auth_rejection_time: Duration::from_secs(1),
        auth_rejection_time_initial: Some(Duration::from_millis(0)),
        max_auth_attempts: 3,
        keys: vec![host],
        inactivity_timeout: Some(idle),
        keepalive_interval: Some(Duration::from_secs(30)),
        ..Default::default()
    });
    let handler = Conn {
        spec: spec.clone(),
        authed: false,
        channel: None,
        pty: None,
        input: None,
        started: false,
        activity: activity.clone(),
        proc_info: proc_info.clone(),
        kill: kill_tx.clone(),
    };
    let running = russh::server::run_stream(config, stream, handler).await;
    let reason = match running {
        Err(e) => format!("ssh_error:{e:?}"),
        Ok(running) => {
            let deadline = tokio::time::sleep(Duration::from_secs(ends_by.saturating_sub(now())));
            tokio::pin!(deadline);
            let mut tick = tokio::time::interval(Duration::from_secs(5));
            let mut running = running;
            loop {
                tokio::select! {
                    r = &mut running => break match r { Ok(_) => "ended".to_string(), Err(e) => format!("ssh:{e:?}") },
                    _ = &mut deadline => break "max_duration".to_string(),
                    _ = kill_rx.changed() => break kill_rx.borrow().clone().unwrap_or_else(|| "killed".into()),
                    _ = broker_closed.changed() => {
                        let r = broker_closed.borrow().clone().unwrap_or_else(|| "tunnel_closed".into());
                        break format!("tunnel:{r}");
                    }
                    _ = tick.tick() => {
                        if activity.lock().expect("activity lock").elapsed() >= idle {
                            break "idle".to_string();
                        }
                    }
                }
            }
        }
    };
    teardown(&proc_info).await;
    let secs = started.elapsed().as_secs();
    log_line(
        machine,
        &format!(
            "ssh: session {sid} on {} ended after {secs}s ({reason})",
            route.mailbox
        ),
    );
    shell_log(
        machine,
        json!({ "at": now(), "event": "close", "session_id": sid, "mailbox": route.mailbox,
                "duration_secs": secs, "reason": reason }),
    );
    Ok(())
}

#[derive(Default)]
struct ProcInfo {
    pid: Option<u32>,
    container: Option<(PathBuf, String)>,
}

/// Kill the session's whole process group, then remove its container if it had one.
async fn teardown(info: &Arc<Mutex<ProcInfo>>) {
    let (pid, container) = {
        let mut i = info.lock().expect("proc lock");
        (i.pid.take(), i.container.take())
    };
    if let Some(pid) = pid.and_then(|p| rustix::process::Pid::from_raw(p as i32)) {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
    if let Some((rt, name)) = container {
        let _ = tokio::time::timeout(
            Duration::from_secs(20),
            tokio::process::Command::new(rt)
                .args(["rm", "-f", &name])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status(),
        )
        .await;
    }
}

enum Input {
    Pty(pty_process::OwnedWritePty),
    Pipe(tokio::process::ChildStdin),
}

struct Conn {
    spec: Spec,
    authed: bool,
    channel: Option<ChannelId>,
    pty: Option<(String, u16, u16)>,
    input: Option<Input>,
    started: bool,
    activity: Arc<Mutex<Instant>>,
    proc_info: Arc<Mutex<ProcInfo>>,
    kill: tokio::sync::watch::Sender<Option<String>>,
}

impl Conn {
    fn touch(&self) {
        *self.activity.lock().expect("activity lock") = Instant::now();
    }

    /// Every binding the certificate carries, checked against what the postbox announced for this
    /// session and against the CA this machine pinned.
    fn certificate_ok(&self, user: &str, cert: &Certificate) -> Result<(), &'static str> {
        if user != super::SSH_USER {
            return Err("wrong user");
        }
        let ca_fp = self.spec.ca.fingerprint(HashAlg::Sha256);
        cert.validate_at(now(), [&ca_fp])
            .map_err(|_| "not signed by the pinned CA, or expired")?;
        if cert.cert_type() != russh::keys::ssh_key::certificate::CertType::User {
            return Err("not a user certificate");
        }
        if cert.valid_principals() != [self.spec.principal.clone()]
            || self.spec.principal != format!("pp-ssh-{}", self.spec.sid)
        {
            return Err("certificate is for another session");
        }
        let key = russh::keys::PublicKey::from(cert.public_key().clone());
        if super::fingerprint(&key) != self.spec.client_fp {
            return Err("certificate key is not the session's key");
        }
        if format!("{:016x}", cert.serial()) != self.spec.serial {
            return Err("certificate serial does not match");
        }
        if !cert.critical_options().is_empty() {
            return Err("unexpected critical options");
        }
        let grant: Value = serde_json::from_str(cert.key_id()).map_err(|_| "unreadable grant")?;
        if grant["sid"].as_str() != Some(self.spec.sid.as_str())
            || grant["agent"].as_str() != Some(self.spec.agent.as_str())
            || grant["mode"].as_str() != Some(self.spec.shell.mode.as_str())
        {
            return Err("grant names another session, agent or mode");
        }
        Ok(())
    }

    async fn start(
        &mut self,
        channel: ChannelId,
        command: Option<String>,
        session: &mut Session,
    ) -> Result<(), russh::Error> {
        if self.started || self.channel != Some(channel) {
            session.channel_failure(channel)?;
            return Ok(());
        }
        let (term, cols, rows) = self
            .pty
            .clone()
            .unwrap_or_else(|| ("xterm-256color".into(), 80, 24));
        let want_pty = self.pty.is_some() && self.spec.shell.mode != Mode::Allowlist;
        let launch = match confine::launch(
            &self.spec.shell,
            &self.spec.sid,
            &term,
            want_pty,
            command.as_deref(),
        ) {
            Ok(l) => l,
            Err(message) => {
                session.channel_success(channel)?;
                let h = session.handle();
                let _ = h
                    .extended_data(channel, 1, Bytes::from(format!("{message}\r\n")))
                    .await;
                let _ = h.exit_status_request(channel, 126).await;
                let _ = h.eof(channel).await;
                let _ = h.close(channel).await;
                self.started = true;
                return Ok(());
            }
        };
        self.started = true;
        match spawn(&launch, cols, rows, self.proc_info.clone()) {
            Ok(spawned) => {
                session.channel_success(channel)?;
                self.input = Some(spawned.input);
                pump(
                    session.handle(),
                    channel,
                    spawned.outputs,
                    spawned.child,
                    self.activity.clone(),
                    self.kill.clone(),
                );
            }
            Err(e) => {
                session.channel_failure(channel)?;
                let _ = self.kill.send(Some(format!("spawn_failed:{e}")));
            }
        }
        Ok(())
    }
}

struct Spawned {
    input: Input,
    outputs: Vec<(u32, Box<dyn tokio::io::AsyncRead + Send + Unpin>)>,
    child: tokio::process::Child,
}

fn spawn(l: &Launch, cols: u16, rows: u16, info: Arc<Mutex<ProcInfo>>) -> Result<Spawned, String> {
    if l.pty {
        let (pty, pts) = pty_process::open().map_err(|e| e.to_string())?;
        pty.resize(pty_process::Size::new(rows, cols))
            .map_err(|e| e.to_string())?;
        // A pty child is its own session leader, so its process group is its pid.
        let child = pty_process::Command::new(&l.program)
            .args(&l.args)
            .env_clear()
            .envs(l.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .current_dir(&l.cwd)
            .kill_on_drop(true)
            .spawn(pts)
            .map_err(|e| e.to_string())?;
        {
            let mut i = info.lock().expect("proc lock");
            i.pid = child.id();
            i.container = l.container.clone();
        }
        let (rd, wr) = pty.into_split();
        Ok(Spawned {
            input: Input::Pty(wr),
            outputs: vec![(0, Box::new(rd))],
            child,
        })
    } else {
        let mut cmd = tokio::process::Command::new(&l.program);
        cmd.args(&l.args)
            .env_clear()
            .envs(l.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .current_dir(&l.cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .process_group(0);
        let mut child = cmd.spawn().map_err(|e| e.to_string())?;
        {
            let mut i = info.lock().expect("proc lock");
            i.pid = child.id();
            i.container = l.container.clone();
        }
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let stderr = child.stderr.take().ok_or("no stderr")?;
        Ok(Spawned {
            input: Input::Pipe(stdin),
            outputs: vec![(0, Box::new(stdout)), (1, Box::new(stderr))],
            child,
        })
    }
}

/// Copy the process's output to the channel, then report its exit status and close.
fn pump(
    h: russh::server::Handle,
    channel: ChannelId,
    outputs: Vec<(u32, Box<dyn tokio::io::AsyncRead + Send + Unpin>)>,
    mut child: tokio::process::Child,
    activity: Arc<Mutex<Instant>>,
    kill: tokio::sync::watch::Sender<Option<String>>,
) {
    tokio::spawn(async move {
        let mut readers = Vec::new();
        for (stream, mut rd) in outputs {
            let h = h.clone();
            let activity = activity.clone();
            readers.push(tokio::spawn(async move {
                let mut buf = vec![0u8; 16 * 1024];
                loop {
                    match rd.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            *activity.lock().expect("activity lock") = Instant::now();
                            let data = Bytes::copy_from_slice(&buf[..n]);
                            let sent = if stream == 0 {
                                h.data(channel, data).await
                            } else {
                                h.extended_data(channel, stream, data).await
                            };
                            if sent.is_err() {
                                break;
                            }
                        }
                    }
                }
            }));
        }
        let status = child
            .wait()
            .await
            .ok()
            .and_then(|s| s.code())
            .unwrap_or(255);
        for r in readers {
            let _ = tokio::time::timeout(Duration::from_secs(2), r).await;
        }
        let _ = h.exit_status_request(channel, status as u32).await;
        let _ = h.eof(channel).await;
        let _ = h.close(channel).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        kill.send_if_modified(|r| {
            if r.is_none() {
                *r = Some("exited".into());
                true
            } else {
                false
            }
        });
    });
}

impl Handler for Conn {
    type Error = russh::Error;

    async fn auth_publickey(
        &mut self,
        _user: &str,
        _key: russh::server::VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        Ok(Auth::reject())
    }

    async fn auth_openssh_certificate(
        &mut self,
        user: &str,
        cert: &Certificate,
    ) -> Result<Auth, Self::Error> {
        if self.authed {
            return Ok(Auth::reject());
        }
        match self.certificate_ok(user, cert) {
            Ok(()) => {
                self.authed = true;
                Ok(Auth::Accept)
            }
            Err(why) => {
                tracing::warn!(session = %self.spec.sid, why, "ssh certificate refused");
                Ok(Auth::reject())
            }
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        // One session channel per connection, and only after authentication.
        if self.authed && self.channel.is_none() {
            self.channel = Some(channel.id());
            reply.accept().await;
        }
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.channel != Some(channel) || self.started || self.spec.shell.mode == Mode::Allowlist
        {
            session.channel_failure(channel)?;
            return Ok(());
        }
        self.pty = Some((
            term.to_string(),
            col_width.clamp(20, 500) as u16,
            row_height.clamp(5, 300) as u16,
        ));
        session.channel_success(channel)?;
        Ok(())
    }

    async fn env_request(
        &mut self,
        channel: ChannelId,
        _name: &str,
        _value: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        // Environment comes from this machine only (LD_PRELOAD, PATH, BASH_ENV …).
        session.channel_failure(channel)?;
        Ok(())
    }

    async fn x11_request(
        &mut self,
        channel: ChannelId,
        _single: bool,
        _proto: &str,
        _cookie: &str,
        _screen: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)?;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        _name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        // No SFTP/SCP until they enforce the same boundary (SDS §11.5).
        session.channel_failure(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.touch();
        self.start(channel, None, session).await
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.touch();
        let Ok(command) = std::str::from_utf8(data) else {
            session.channel_failure(channel)?;
            return Ok(());
        };
        self.start(channel, Some(command.to_string()), session)
            .await
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.channel != Some(channel) {
            return Ok(());
        }
        self.touch();
        let ok = match self.input.as_mut() {
            Some(Input::Pty(w)) => w.write_all(data).await.is_ok(),
            Some(Input::Pipe(w)) => w.write_all(data).await.is_ok(),
            None => true,
        };
        if !ok {
            self.input = None;
        }
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.channel == Some(channel) {
            if let Some(Input::Pipe(_)) = self.input {
                // Closing stdin lets a command reading it finish.
                self.input = None;
            }
        }
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.channel == Some(channel) {
            if let Some(Input::Pty(w)) = &self.input {
                let _ = w.resize(pty_process::Size::new(
                    row_height.clamp(5, 300) as u16,
                    col_width.clamp(20, 500) as u16,
                ));
            }
        }
        Ok(())
    }

    async fn channel_close(
        &mut self,
        channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.channel == Some(channel) {
            let _ = self.kill.send(Some("client_closed".into()));
        }
        Ok(())
    }
}

/// `pigeonpost agentd shell`.
pub fn status(home: &Path, json_out: bool) -> Result<(), Box<dyn std::error::Error>> {
    let machine = crate::agentd_cmd::machine_home(home);
    let key = host_key(&machine).map_err(|e| e.to_string())?;
    let fp = super::fingerprint(key.public_key());
    let config = crate::executor::load_routing(&machine)?;
    let mut rows = Vec::new();
    for r in &config.mailbox {
        let Some(sc) = &r.shell else { continue };
        let checked = sc.validate(&r.workspace, &machine).and_then(|s| {
            if s.mode == Mode::Container {
                confine::runtime(&s).map(|_| s)
            } else {
                Ok(s)
            }
        });
        rows.push(json!({
            "mailbox": r.address, "enabled": sc.enabled, "mode": sc.mode,
            "workspace": r.workspace, "ok": checked.is_ok(), "problem": checked.err(),
        }));
    }
    let ca = std::fs::read_to_string(machine.join(CA_PIN_FILE)).ok();
    if json_out {
        println!(
            "{}",
            json!({ "host_key_fingerprint": fp, "ca_pinned": ca.as_deref().map(str::trim), "mailboxes": rows })
        );
        return Ok(());
    }
    println!("host key: {fp}");
    println!("  (compare with what `pigeonpost ssh` shows the first time it connects)");
    match ca {
        Some(c) => println!("SSH CA pinned: {}", c.trim()),
        None => println!("SSH CA: not pinned yet (pinned on the first connection to the postbox)"),
    }
    if rows.is_empty() {
        println!("no mailbox offers SSH — add a [mailbox.shell] table to agentd.toml (see docs/agent-ssh.md)");
    }
    for r in rows {
        println!(
            "{}: {} mode={} {}",
            r["mailbox"].as_str().unwrap_or("?"),
            if r["enabled"] == true {
                "enabled"
            } else {
                "disabled"
            },
            r["mode"].as_str().unwrap_or("?"),
            if r["ok"] == true {
                "ok".to_string()
            } else {
                format!("NOT OFFERED: {}", r["problem"].as_str().unwrap_or(""))
            }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_key_is_created_once_and_owner_only() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let a = host_key(&home).unwrap();
        let b = host_key(&home).unwrap();
        assert_eq!(a.public_key(), b.public_key());
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(home.join(HOST_KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
    }

    #[test]
    fn another_postboxs_ca_needs_an_out_of_band_pin_and_is_never_swapped() {
        let t = tempfile::tempdir().unwrap();
        let base = "https://postbox.example.test";
        let one = super::super::new_ed25519()
            .public_key()
            .to_openssh()
            .unwrap();
        let two = super::super::new_ed25519()
            .public_key()
            .to_openssh()
            .unwrap();
        // No trust on first use: nothing is pinned, so the offer is refused and nothing written.
        let e = pin_ca(t.path(), base, &one).unwrap_err().to_string();
        assert!(e.contains("confirm it with the postbox's operator"), "{e}");
        assert!(!t.path().join(CA_PIN_FILE).exists());
        // The owner pins it out of band.
        std::fs::write(t.path().join(CA_PIN_FILE), format!("{one}\n")).unwrap();
        assert!(pin_ca(t.path(), base, &one).is_ok());
        let e = pin_ca(t.path(), base, &two).unwrap_err().to_string();
        assert!(e.contains("not the one this machine pinned"), "{e}");
    }

    #[test]
    fn the_production_postbox_is_held_to_the_ca_in_this_build() {
        let t = tempfile::tempdir().unwrap();
        let release = russh::keys::PublicKey::from_openssh(RELEASE_CA).unwrap();
        assert_eq!(
            super::super::fingerprint(&release),
            "SHA256:JYtla5q8tBzb4vFjJgeUNBiHblmEdsBvlKy2G+FbfGg"
        );
        let base = "https://postbox.pigeonpost.dev";
        assert!(pin_ca(t.path(), base, RELEASE_CA).is_ok());
        let other = super::super::new_ed25519()
            .public_key()
            .to_openssh()
            .unwrap();
        // A pin file cannot widen it either.
        std::fs::write(t.path().join(CA_PIN_FILE), format!("{other}\n")).unwrap();
        let e = pin_ca(t.path(), base, &other).unwrap_err().to_string();
        assert!(e.contains("this agentd release pins"), "{e}");
        assert!(!is_release_postbox("http://postbox.pigeonpost.dev"));
        assert!(!is_release_postbox(
            "https://postbox.pigeonpost.dev.evil.test"
        ));
    }
}

/// The SSH server itself, over an in-memory stream: what a certificate must bind, and what a
/// session may and may not do once in.
#[cfg(test)]
mod ssh_tests {
    use super::*;
    use crate::shell::config::AllowedCommand;
    use russh::keys::ssh_key;

    fn key(seed: u8) -> PrivateKey {
        PrivateKey::from(ssh_key::private::Ed25519Keypair::from_seed(&[seed; 32]))
    }

    const SID: &str = "ssh_abcdefghijklmnopqrstuvwxyz";
    const AGENT: &str = "/k/agent";

    struct CertSpec<'a> {
        ca: &'a PrivateKey,
        client: &'a PrivateKey,
        principal: String,
        serial: u64,
        after: u64,
        before: u64,
        agent: &'a str,
        mode: &'a str,
    }

    fn good<'a>(ca: &'a PrivateKey, client: &'a PrivateKey, mode: &'a str) -> CertSpec<'a> {
        CertSpec {
            ca,
            client,
            principal: format!("pp-ssh-{SID}"),
            serial: 0x1234,
            after: now() - 60,
            before: now() + 300,
            agent: AGENT,
            mode,
        }
    }

    fn cert(c: CertSpec<'_>) -> Certificate {
        let mut b = ssh_key::certificate::Builder::new(
            vec![1u8; 32],
            c.client.public_key().key_data().clone(),
            c.after,
            c.before,
        )
        .unwrap();
        b.serial(c.serial).unwrap();
        b.cert_type(ssh_key::certificate::CertType::User).unwrap();
        b.valid_principal(c.principal).unwrap();
        b.key_id(json!({ "sid": SID, "agent": c.agent, "mode": c.mode }).to_string())
            .unwrap();
        b.extension("permit-pty", "").unwrap();
        b.sign(c.ca).unwrap()
    }

    fn shell(mode: Mode, ws: &Path) -> Shell {
        Shell {
            mode,
            workspace: std::fs::canonicalize(ws).unwrap(),
            image: "x".into(),
            docker_rootful_ok: false,
            runtime: None,
            outbound: false,
            read_only: false,
            max_sessions: 2,
            idle_secs: 900,
            commands: vec![
                AllowedCommand {
                    argv: vec!["/bin/echo".into(), "hello".into()],
                    paths: 0,
                },
                AllowedCommand {
                    argv: vec!["/bin/cat".into()],
                    paths: 1,
                },
            ],
        }
    }

    struct AnyHost;
    impl russh::client::Handler for AnyHost {
        type Error = russh::Error;
        async fn check_server_key(
            &mut self,
            _k: &russh::keys::PublicKeyOrCertificate,
        ) -> Result<bool, Self::Error> {
            Ok(true)
        }
    }

    struct Rig {
        client: russh::client::Handle<AnyHost>,
        proc_info: Arc<Mutex<ProcInfo>>,
        _ws: tempfile::TempDir,
    }

    async fn rig(mode: Mode, client: &PrivateKey, c: Certificate) -> (Rig, bool) {
        let ws = tempfile::tempdir().unwrap();
        let ca = key(1);
        let spec = Spec {
            sid: SID.into(),
            agent: AGENT.into(),
            principal: format!("pp-ssh-{SID}"),
            client_fp: crate::shell::fingerprint(client.public_key()),
            serial: format!("{:016x}", 0x1234),
            ca: ca.public_key().clone(),
            shell: shell(mode, ws.path()),
        };
        let proc_info: Arc<Mutex<ProcInfo>> = Arc::default();
        let conn = Conn {
            spec,
            authed: false,
            channel: None,
            pty: None,
            input: None,
            started: false,
            activity: Arc::new(Mutex::new(Instant::now())),
            proc_info: proc_info.clone(),
            kill: tokio::sync::watch::channel(None).0,
        };
        let (a, b) = tokio::io::duplex(1 << 16);
        let config = Arc::new(russh::server::Config {
            methods: russh::MethodSet::from(&[russh::MethodKind::PublicKey][..]),
            auth_rejection_time: Duration::from_millis(10),
            auth_rejection_time_initial: Some(Duration::from_millis(0)),
            keys: vec![key(2)],
            ..Default::default()
        });
        tokio::spawn(async move {
            if let Ok(r) = russh::server::run_stream(config, a, conn).await {
                let _ = r.await;
            }
        });
        let mut handle = russh::client::connect_stream(Arc::new(Default::default()), b, AnyHost)
            .await
            .unwrap();
        let ok = handle
            .authenticate_openssh_cert(crate::shell::SSH_USER, Arc::new(client.clone()), c)
            .await
            .unwrap()
            .success();
        (
            Rig {
                client: handle,
                proc_info,
                _ws: ws,
            },
            ok,
        )
    }

    async fn collect(
        ch: &mut russh::Channel<russh::client::Msg>,
    ) -> (String, String, Option<u32>, bool) {
        let (mut out, mut err, mut status, mut failed) = (Vec::new(), Vec::new(), None, false);
        loop {
            match tokio::time::timeout(Duration::from_secs(10), ch.wait())
                .await
                .expect("channel stalled")
            {
                Some(russh::ChannelMsg::Data { data }) => out.extend_from_slice(&data),
                Some(russh::ChannelMsg::ExtendedData { data, .. }) => err.extend_from_slice(&data),
                Some(russh::ChannelMsg::ExitStatus { exit_status }) => status = Some(exit_status),
                Some(russh::ChannelMsg::Failure) => failed = true,
                Some(russh::ChannelMsg::Close) | None => break,
                Some(_) => {}
            }
        }
        (
            String::from_utf8_lossy(&out).into_owned(),
            String::from_utf8_lossy(&err).into_owned(),
            status,
            failed,
        )
    }

    #[tokio::test]
    async fn a_certificate_must_bind_this_session_key_serial_agent_and_ca() {
        let (ca, client, other) = (key(1), key(3), key(4));
        let (_, ok) = rig(
            Mode::Allowlist,
            &client,
            cert(good(&ca, &client, "allowlist")),
        )
        .await;
        assert!(ok, "the session's own certificate");
        let rogue_ca = key(9);
        let cases: Vec<(&str, CertSpec)> = vec![
            (
                "expired",
                CertSpec {
                    before: now() - 5,
                    after: now() - 400,
                    ..good(&ca, &client, "allowlist")
                },
            ),
            (
                "not yet valid",
                CertSpec {
                    after: now() + 600,
                    before: now() + 900,
                    ..good(&ca, &client, "allowlist")
                },
            ),
            ("another CA", good(&rogue_ca, &client, "allowlist")),
            (
                "another session",
                CertSpec {
                    principal: "pp-ssh-ssh_bbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
                    ..good(&ca, &client, "allowlist")
                },
            ),
            (
                "a handle as principal",
                CertSpec {
                    principal: "/acme/agent1".into(),
                    ..good(&ca, &client, "allowlist")
                },
            ),
            (
                "another serial",
                CertSpec {
                    serial: 7,
                    ..good(&ca, &client, "allowlist")
                },
            ),
            (
                "another agent",
                CertSpec {
                    agent: "/k/other",
                    ..good(&ca, &client, "allowlist")
                },
            ),
            ("another mode", good(&ca, &client, "unconfined")),
        ];
        for (why, spec) in cases {
            let (_, ok) = rig(Mode::Allowlist, &client, cert(spec)).await;
            assert!(!ok, "{why}");
        }
        // A valid certificate for someone else's key is no use without that key.
        let (_, ok) = rig(
            Mode::Allowlist,
            &client,
            cert(good(&ca, &other, "allowlist")),
        )
        .await;
        assert!(!ok, "another key's certificate");
    }

    #[tokio::test]
    async fn one_session_channel_and_nothing_else() {
        let (ca, client) = (key(1), key(3));
        std::env::set_var("PP_AGENTD_SECRET", "must-not-leak");
        let (r, ok) = rig(
            Mode::Allowlist,
            &client,
            cert(good(&ca, &client, "allowlist")),
        )
        .await;
        assert!(ok);
        let mut ch = r.client.channel_open_session().await.unwrap();
        // Environment injection, X11, agent forwarding, subsystems (sftp): all refused.
        ch.set_env(true, "LD_PRELOAD", "/tmp/evil.so")
            .await
            .unwrap();
        assert!(
            matches!(ch.wait().await, Some(russh::ChannelMsg::Failure)),
            "env"
        );
        ch.request_subsystem(true, "sftp").await.unwrap();
        assert!(
            matches!(ch.wait().await, Some(russh::ChannelMsg::Failure)),
            "sftp"
        );
        ch.request_x11(true, false, "MIT-MAGIC-COOKIE-1", "00", 0)
            .await
            .unwrap();
        assert!(
            matches!(ch.wait().await, Some(russh::ChannelMsg::Failure)),
            "x11"
        );
        ch.agent_forward(true).await.unwrap();
        assert!(
            matches!(ch.wait().await, Some(russh::ChannelMsg::Failure)),
            "agent forwarding"
        );
        // No pty in allowlist mode.
        ch.request_pty(true, "xterm", 80, 24, 0, 0, &[])
            .await
            .unwrap();
        assert!(
            matches!(ch.wait().await, Some(russh::ChannelMsg::Failure)),
            "pty"
        );
        // A second channel, port forwarding of either kind: refused.
        assert!(r.client.channel_open_session().await.is_err());
        assert!(r
            .client
            .channel_open_direct_tcpip("127.0.0.1", 22, "127.0.0.1", 5000)
            .await
            .is_err());
        assert!(
            r.client.tcpip_forward("127.0.0.1", 0).await.is_err(),
            "remote forwarding"
        );
        // The allowed command runs, without the environment of agentd or of the request.
        ch.exec(true, "echo hello").await.unwrap();
        let (out, _, status, _) = collect(&mut ch).await;
        assert_eq!((out.trim(), status), ("hello", Some(0)));
    }

    #[tokio::test]
    async fn allowlist_refusals_over_ssh() {
        let (ca, client) = (key(1), key(3));
        for attempt in [
            "cat ../../etc/passwd",
            "echo hi; id",
            "echo $(id)",
            "sh -c id",
            "cat /etc/passwd",
        ] {
            let (r, ok) = rig(
                Mode::Allowlist,
                &client,
                cert(good(&ca, &client, "allowlist")),
            )
            .await;
            assert!(ok);
            let mut ch = r.client.channel_open_session().await.unwrap();
            ch.exec(true, attempt).await.unwrap();
            let (out, err, status, _) = collect(&mut ch).await;
            assert_eq!(status, Some(126), "{attempt}: {out} {err}");
            assert!(err.contains("refused"), "{attempt}: {err}");
            assert!(!out.contains("root:"), "{attempt}");
        }
        // A shell is refused with the list of what is allowed.
        let (r, _) = rig(
            Mode::Allowlist,
            &client,
            cert(good(&ca, &client, "allowlist")),
        )
        .await;
        let mut ch = r.client.channel_open_session().await.unwrap();
        ch.request_shell(true).await.unwrap();
        let (_, err, status, _) = collect(&mut ch).await;
        assert_eq!(status, Some(126));
        assert!(err.contains("/bin/echo"), "{err}");
    }

    #[tokio::test]
    async fn unconfined_sessions_get_a_clean_environment_and_teardown_kills_the_group() {
        let (ca, client) = (key(1), key(3));
        std::env::set_var("PP_AGENTD_SECRET", "must-not-leak");
        let (r, ok) = rig(
            Mode::Unconfined,
            &client,
            cert(good(&ca, &client, "unconfined")),
        )
        .await;
        assert!(ok);
        let mut ch = r.client.channel_open_session().await.unwrap();
        ch.set_env(true, "BASH_ENV", "/tmp/x").await.unwrap();
        assert!(matches!(ch.wait().await, Some(russh::ChannelMsg::Failure)));
        ch.exec(true, "env; pwd").await.unwrap();
        let (out, _, status, _) = collect(&mut ch).await;
        assert_eq!(status, Some(0));
        assert!(
            !out.contains("must-not-leak")
                && !out.contains("BASH_ENV")
                && !out.contains("LD_PRELOAD"),
            "{out}"
        );
        assert!(out.contains("PATH="), "{out}");
        // Teardown: a session with a child that would outlive its shell.
        let (r, _) = rig(
            Mode::Unconfined,
            &client,
            cert(good(&ca, &client, "unconfined")),
        )
        .await;
        let mut ch = r.client.channel_open_session().await.unwrap();
        ch.exec(true, "sleep 1000 & sleep 1000").await.unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let pid = r.proc_info.lock().unwrap().pid.expect("running");
        teardown(&r.proc_info).await;
        let (_, _, status, _) = collect(&mut ch).await;
        assert_ne!(status, Some(0));
        // Killed members linger as zombies until their reaper collects them; poll briefly.
        let group = rustix::process::Pid::from_raw(pid as i32).unwrap();
        let mut gone = false;
        for _ in 0..50 {
            if rustix::process::test_kill_process_group(group).is_err()
                || !group_has_live_member(pid)
            {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(gone, "the whole process group is gone");
    }

    /// Whether any process in group `pgid` is still running (zombies excluded), on Linux; elsewhere
    /// whether the group exists at all.
    fn group_has_live_member(pgid: u32) -> bool {
        let Ok(dir) = std::fs::read_dir("/proc") else {
            return true;
        };
        for e in dir.flatten() {
            let Ok(stat) = std::fs::read_to_string(e.path().join("stat")) else {
                continue;
            };
            // pid (comm) state ppid pgrp …
            let Some(rest) = stat.rsplit_once(')').map(|(_, r)| r.trim().to_string()) else {
                continue;
            };
            let f: Vec<&str> = rest.split_whitespace().collect();
            if f.len() > 2 && f[2] == pgid.to_string() && f[0] != "Z" {
                return true;
            }
        }
        false
    }
}
