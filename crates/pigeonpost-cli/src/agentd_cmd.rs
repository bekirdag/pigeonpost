//! `pigeonpost agentd` — the resident process that turns mail into a wake-up.
//!
//! Neither Claude Code nor Codex is a server. Both exist only while a session runs, so there is
//! nothing to push into when nobody is working, and no vendor mechanism to start a session from
//! outside. "Notify the agent" therefore has to mean: *something resident receives the push and
//! records it where an agent will see it*. This is that resident thing. Every alternative design
//! collapses back into polling.
//!
//! It holds one `GET /v1/events` stream per account and does nothing dangerous with what arrives:
//! a desktop notification, and an append to a per-mailbox spool. Executing requests is a later
//! phase deliberately — a daemon that both listens and acts is a much larger thing to get right,
//! and this half alone removes the five-minute loop.
//!
//! A sleeping laptop still cannot be woken. Mail waits, and the cursor means none of it is missed.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

type Error = Box<dyn std::error::Error>;

/// Where the resume cursor lives. One per home, because one stream covers the whole account.
const CURSOR_FILE: &str = "agentd-cursor";
const SPOOL_DIR: &str = "spool";
const LOG_FILE: &str = "agentd.log";

/// Reconnect backoff. Starts fast because the common disconnect is a laptop lid, not an outage.
const BACKOFF_MIN: Duration = Duration::from_secs(2);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

fn cursor_path(home: &Path) -> PathBuf {
    home.join(CURSOR_FILE)
}

fn read_cursor(home: &Path) -> Option<i64> {
    std::fs::read_to_string(cursor_path(home))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Persist the cursor *after* the event is spooled, never before. The two orders differ in what
/// they lose: writing first loses mail on a crash, writing after can only re-deliver something the
/// spool already has, which the spool's own dedupe absorbs.
fn write_cursor(home: &Path, cursor: i64) -> Result<(), Error> {
    let path = cursor_path(home);
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, cursor.to_string())?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// A mailbox address is not a filename: `/k/…` and `/bekir/agent1` both contain separators.
fn spool_file(home: &Path, mailbox: &str) -> PathBuf {
    let safe: String = mailbox
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    home.join(SPOOL_DIR).join(format!("{safe}.jsonl"))
}

fn log_line(home: &Path, line: &str) {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.join(LOG_FILE))
    {
        let _ = writeln!(f, "{stamp} {line}");
    }
    eprintln!("{line}");
}

/// Tell the person at the machine. Best-effort by design: a missing notifier must not stop the
/// spool being written, which is the delivery guarantee.
fn notify(summary: &str, body: &str) {
    #[cfg(target_os = "macos")]
    let attempt = std::process::Command::new("osascript")
        .arg("-e")
        .arg(format!(
            "display notification {} with title {}",
            applescript_quote(body),
            applescript_quote(summary)
        ))
        .status();
    #[cfg(target_os = "linux")]
    let attempt = std::process::Command::new("notify-send")
        .arg(summary)
        .arg(body)
        .status();
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let attempt: std::io::Result<std::process::ExitStatus> = {
        // No desktop notifier is wired up here yet. The spool is the delivery guarantee, so a
        // platform without notifications still gets its mail — it just gets it silently.
        let _ = (summary, body);
        Err(std::io::Error::other(
            "no desktop notifier on this platform",
        ))
    };
    let _ = attempt;
}

#[cfg(target_os = "macos")]
fn applescript_quote(value: &str) -> String {
    // AppleScript string literals escape backslash and quote, and nothing else.
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// One event off the wire.
#[derive(serde::Deserialize)]
struct MailEvent {
    event_id: i64,
    mailbox: String,
    message_id: String,
    sender: String,
}

/// Run in the foreground, holding the stream. This is what the service manager starts.
pub async fn run(home: &Path, once: bool) -> Result<(), Error> {
    std::fs::create_dir_all(home.join(SPOOL_DIR))?;
    log_line(home, "agentd starting");

    let mut backoff = BACKOFF_MIN;
    loop {
        match stream_once(home).await {
            Ok(()) => {
                // A clean end of stream is the server closing an idle connection; reconnect
                // promptly rather than treating it as a failure.
                backoff = BACKOFF_MIN;
            }
            Err(e) => {
                log_line(home, &format!("stream ended: {e}"));
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
        if once {
            return Ok(());
        }
        tokio::time::sleep(backoff).await;
    }
}

async fn stream_once(home: &Path) -> Result<(), Error> {
    // Fetched per connection, so a daemon running for weeks keeps working: the access token lives
    // five minutes, and reconnecting is exactly when a fresh one is free to obtain.
    let token = crate::login_cmd::access_token(home).await?;
    let base = std::env::var("PIGEONPOST_POSTBOX")
        .unwrap_or_else(|_| crate::postbox_cmd::DEFAULT_POSTBOX.to_string());
    let base = base.trim_end_matches('/').to_string();

    let mut url = format!("{base}/v1/events");
    if let Some(cursor) = read_cursor(home) {
        url.push_str(&format!("?last_event_id={cursor}"));
    }

    // No overall timeout: the point of this request is to stay open. The server's keep-alive is
    // what proves the connection is still alive.
    let http = reqwest::Client::builder()
        .user_agent(concat!("pigeonpost-agentd/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let response = http
        .get(&url)
        .bearer_auth(token)
        .header("accept", "text/event-stream")
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(format!(
            "the postbox refused the event stream: {}",
            response.status()
        )
        .into());
    }
    log_line(home, "listening");

    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        buffer.push_str(&String::from_utf8_lossy(&chunk?));
        // SSE frames are separated by a blank line; anything after the last one is a partial
        // frame still arriving and must stay in the buffer.
        while let Some(split) = buffer.find("\n\n") {
            let frame = buffer[..split].to_string();
            buffer.drain(..split + 2);
            if let Some(data) = frame
                .lines()
                .find_map(|line| line.strip_prefix("data:").map(str::trim))
            {
                if let Ok(event) = serde_json::from_str::<MailEvent>(data) {
                    handle(home, &event)?;
                }
            }
        }
    }
    Ok(())
}

fn handle(home: &Path, event: &MailEvent) -> Result<(), Error> {
    let path = spool_file(home, &event.mailbox);
    // Dedupe on event id: a reconnect that overlaps by one, or a cursor written after a crash,
    // must not show the same message twice to whoever reads the spool.
    if let Ok(existing) = std::fs::read_to_string(&path) {
        if existing
            .lines()
            .any(|line| line.contains(&format!("\"event_id\":{}", event.event_id)))
        {
            return Ok(());
        }
    }
    let record = serde_json::json!({
        "event_id": event.event_id,
        "mailbox": event.mailbox,
        "message_id": event.message_id,
        "sender": event.sender,
        "noticed_at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    });
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(file, "{record}")?;

    // Classify what this message *would* cause, and record it, without doing it. The rails are
    // live and observable before anything is spawned, so the audit log answers "would this have
    // run, and why" from real traffic rather than from a test fixture.
    match crate::executor::load_routing(home) {
        Ok(config) => {
            let decision = crate::executor::classify(
                &config,
                home,
                &event.mailbox,
                &serde_json::json!({ "message_id": event.message_id }),
            );
            let (outcome, detail) = match &decision {
                Ok(action) => ("would_execute", Some(action.verb.clone())),
                Err(refusal) => (refusal.as_str(), None),
            };
            let _ = crate::executor::audit(
                home,
                &event.mailbox,
                &event.message_id,
                outcome,
                detail.as_deref(),
            );
        }
        Err(e) => {
            let _ = crate::executor::audit(
                home,
                &event.mailbox,
                &event.message_id,
                "routing_unreadable",
                Some(&e.to_string()),
            );
        }
    }

    notify(
        "Pigeonpost",
        &format!("new mail for {} from {}", event.mailbox, event.sender),
    );
    log_line(
        home,
        &format!(
            "mail {} for {} from {}",
            &event.message_id[..12.min(event.message_id.len())],
            event.mailbox,
            event.sender
        ),
    );
    write_cursor(home, event.event_id)?;
    Ok(())
}

/// Show what the daemon has seen, without needing the service manager's own tooling.
pub fn status(home: &Path, json: bool) -> Result<(), Error> {
    let cursor = read_cursor(home);
    let spool = home.join(SPOOL_DIR);
    let mut waiting = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&spool) {
        for entry in entries.flatten() {
            let count = std::fs::read_to_string(entry.path())
                .map(|s| s.lines().count())
                .unwrap_or(0);
            if count > 0 {
                waiting.push((entry.file_name().to_string_lossy().to_string(), count));
            }
        }
    }
    if json {
        println!(
            "{}",
            serde_json::json!({
                "cursor": cursor,
                "service_unit": installed_unit().map(|p| p.display().to_string()),
                "spool": waiting.iter().map(|(f, c)| serde_json::json!({"file": f, "events": c})).collect::<Vec<_>>(),
            })
        );
        return Ok(());
    }
    match cursor {
        Some(c) => println!("last event seen: {c}"),
        None => println!("no events seen yet"),
    }
    if waiting.is_empty() {
        println!("spool is empty");
    } else {
        for (file, count) in waiting {
            println!("  {file}: {count} event(s)");
        }
    }
    let routing = crate::executor::load_routing(home).unwrap_or_default();
    if crate::executor::paused(home) {
        println!("unattended action: PAUSED");
    } else if routing.execute {
        println!(
            "unattended action: enabled for {} mailbox(es), at most {} at once",
            routing.mailbox.len(),
            routing.max_concurrent
        );
    } else {
        println!(
            "unattended action: off (rails only) — see {}",
            crate::executor::config_path(home).display()
        );
    }
    println!("audit: {}", crate::executor::audit_path(home).display());
    match installed_unit() {
        Some(unit) => println!("service: installed at {}", unit.display()),
        None => println!("service: not installed — run `pigeonpost agentd install`"),
    }
    println!("log: {}", home.join(LOG_FILE).display());
    Ok(())
}

/// The machine-wide home, where the one daemon per box keeps its spool.
///
/// An agent home is `<machine>/agents/<name>`, and the daemon does not run per agent — so an agent
/// reading its own home would find an empty spool and conclude, wrongly, that it had no mail.
pub(crate) fn machine_home_of(home: &Path) -> PathBuf {
    machine_home(home)
}

fn machine_home(home: &Path) -> PathBuf {
    let looks_like_agent_home = home
        .parent()
        .and_then(|p| p.file_name())
        .is_some_and(|n| n == "agents");
    if looks_like_agent_home {
        if let Some(machine) = home.parent().and_then(|p| p.parent()) {
            return machine.to_path_buf();
        }
    }
    home.to_path_buf()
}

/// The mailbox addresses this home owns, used to take only its own mail out of a shared spool.
/// `None` means "this home has no mailbox list", which is the machine home itself — and there the
/// honest answer is everything.
fn own_mailboxes(home: &Path) -> Option<Vec<String>> {
    let body = std::fs::read_to_string(home.join("postbox.json")).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&body).ok()?;
    let list: Vec<String> = parsed
        .get("identities")?
        .as_array()?
        .iter()
        .filter_map(|c| {
            c.get("address")
                .and_then(|a| a.as_str())
                .map(str::to_string)
        })
        .collect();
    (!list.is_empty()).then_some(list)
}

/// Print and clear the spool. Draining is what an agent session does at start-up, so this is the
/// command a hook calls rather than a human.
///
/// One machine runs one daemon but many agents, so the spool is shared. An agent scoped to its own
/// home takes only its own mailbox's mail and leaves the rest — otherwise the first session to
/// start would swallow every other agent's mail, and they would never learn it existed.
pub fn drain(home: &Path, keep: bool) -> Result<(), Error> {
    let machine = machine_home(home);
    let scoped_to_an_agent = machine != home;
    let mine = own_mailboxes(home);
    let spool = machine.join(SPOOL_DIR);

    // An agent home with no mailbox list is the dangerous case: falling back to "no filter" would
    // drain the whole box on behalf of an agent that owns none of it, and the fleet would never
    // learn the mail existed. That happens whenever hooks are installed for an agent before it has
    // onboarded, which is an easy order to get wrong.
    if scoped_to_an_agent && mine.is_none() {
        println!("no mailbox here yet — nothing of this agent's to drain.");
        println!("Take one first:  pigeonpost postbox onboard --handle /namespace/name");
        return Ok(());
    }

    let wanted: Option<Vec<PathBuf>> = mine
        .as_ref()
        .map(|addresses| addresses.iter().map(|a| spool_file(&machine, a)).collect());

    let mut any = false;
    if let Ok(entries) = std::fs::read_dir(&spool) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(wanted) = &wanted {
                if !wanted.contains(&path) {
                    continue;
                }
            }
            let body = std::fs::read_to_string(&path).unwrap_or_default();
            if body.trim().is_empty() {
                continue;
            }
            any = true;
            print!("{body}");
            if !keep {
                let _ = std::fs::write(&path, "");
            }
        }
    }
    if !any {
        println!("no new mail");
    }
    Ok(())
}

// ---- service installation ---------------------------------------------------------------------
//
// Explicit, never a package-install side effect. A background process that starts because someone
// ran `npm i` is a process nobody chose to run, and this one holds a credential for the account.

// launchd identifies a job by label; systemd identifies it by unit filename. So this is macOS-only
// rather than a shared constant, and saying that with a cfg keeps the other platforms from
// carrying a name they never use.
#[cfg(target_os = "macos")]
const SERVICE_LABEL: &str = "dev.pigeonpost.agentd";

/// A path a service manager or hook can still invoke tomorrow.
///
/// `current_exe()` is the obvious answer and the wrong one under the npm launcher: that unpacks the
/// native binary into `~/.cache/pigeonpost/run/exec-<pid>-<random>/` and deletes it when the process
/// exits. A unit or hook recorded from there points at a path that is gone by the time anything
/// runs it — which is exactly how a `Stop` hook ends up reporting "No such file or directory".
///
/// So an ephemeral path is detected and rejected in favour of the launcher's own stable entry
/// point on `PATH`, which npm manages and which keeps tracking upgrades. If neither is available
/// this errors rather than writing something that cannot work.
fn program() -> Result<PathBuf, Error> {
    let exe = std::env::current_exe()
        .map_err(|e| -> Error { format!("cannot resolve this binary's path: {e}").into() })?;

    if !is_ephemeral(&exe) {
        return Ok(exe);
    }

    // Running through the npm launcher, whose unpack directory is deleted on exit. Resolving the
    // `pigeonpost` on PATH is *not* the answer: that is the launcher's JavaScript entry point, and
    // a service manager runs it with a bare environment where `#!/usr/bin/env node` finds no node
    // — the daemon then dies with "env: node: No such file or directory". So copy the native
    // binary we are already running to a fixed location and record that. It is self-contained,
    // needs no interpreter, and re-running install after an upgrade refreshes it.
    let stable = service_binary_path()?;
    if let Some(parent) = stable.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Replace via a temporary file: overwriting a binary that a running daemon is executing can
    // fail or, worse, leave a half-written file where a service manager expects a program.
    let staging = stable.with_extension("new");
    std::fs::copy(&exe, &staging)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&staging, &stable)?;
    Ok(stable)
}

/// Where a copy of the native binary lives for service managers to start.
fn service_binary_path() -> Result<PathBuf, Error> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("pigeonpost")
        .join("pigeonpost"))
}

/// True for the launcher's per-execution unpack directory, which is removed on exit.
fn is_ephemeral(path: &Path) -> bool {
    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|name| name.starts_with("exec-"))
    }) && path.to_string_lossy().contains(".cache")
}

#[cfg(target_os = "macos")]
fn unit_path() -> Result<PathBuf, Error> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{SERVICE_LABEL}.plist")))
}

#[cfg(target_os = "linux")]
fn unit_path() -> Result<PathBuf, Error> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .ok_or("neither XDG_CONFIG_HOME nor HOME is set")?;
    Ok(base.join("systemd/user/pigeonpost-agentd.service"))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn unit_path() -> Result<PathBuf, Error> {
    Err("automatic installation is not implemented for this platform yet — run `pigeonpost agentd run` under your own supervisor".into())
}

/// Install the daemon with this machine's service manager and start it.
pub fn install(home: &Path) -> Result<(), Error> {
    // Refuse rather than install something that cannot work: without a session the daemon would
    // start, fail to authenticate, and retry forever while looking healthy to the service manager.
    if crate::login_cmd::load(home)?.is_none() {
        return Err(
            "not signed in — run `pigeonpost login` first, or the daemon will start and fail to authenticate"
                .into(),
        );
    }
    let program = program()?;
    let path = unit_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, unit_body(&program, home)?)?;
    load_service(&path)?;
    println!("installed {}", path.display());
    println!("  program: {}", program.display());
    println!("  home:    {}", home.display());
    println!();
    println!("It holds one event stream and writes new mail to the spool. Check it with:");
    println!("  pigeonpost agentd status");
    Ok(())
}

/// Stop the daemon and remove the unit.
pub fn uninstall(home: &Path) -> Result<(), Error> {
    let _ = home;
    let path = unit_path()?;
    unload_service(&path)?;
    match std::fs::remove_file(&path) {
        Ok(()) => println!("removed {}", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => println!("not installed"),
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn unit_body(program: &Path, home: &Path) -> Result<String, Error> {
    // KeepAlive rather than RunAtLoad alone: the daemon's whole job is to be there when mail
    // arrives, so launchd restarting it after a crash or a logout is the behaviour wanted.
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{program}</string>
    <string>--home</string>
    <string>{home}</string>
    <string>agentd</string>
    <string>run</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>{home}/agentd.out</string>
  <key>StandardErrorPath</key><string>{home}/agentd.err</string>
</dict>
</plist>
"#,
        label = SERVICE_LABEL,
        program = program.display(),
        home = home.display(),
    ))
}

#[cfg(target_os = "linux")]
fn unit_body(program: &Path, home: &Path) -> Result<String, Error> {
    Ok(format!(
        r#"[Unit]
Description=Pigeonpost agent daemon
After=network-online.target

[Service]
ExecStart={program} --home {home} agentd run
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
"#,
        program = program.display(),
        home = home.display(),
    ))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn unit_body(_program: &Path, _home: &Path) -> Result<String, Error> {
    Err("automatic installation is not implemented for this platform yet".into())
}

#[cfg(target_os = "macos")]
fn load_service(path: &Path) -> Result<(), Error> {
    // Unload first so `install` is idempotent: re-running it after an upgrade should replace the
    // running daemon rather than fail because one is already registered.
    // Silenced: when nothing is loaded this prints an I/O error that reads like a failure but is
    // just "there was nothing to unload".
    let _ = std::process::Command::new("launchctl")
        .args(["unload", &path.display().to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    let status = std::process::Command::new("launchctl")
        .args(["load", &path.display().to_string()])
        .status()?;
    if !status.success() {
        return Err("launchctl load failed".into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn unload_service(path: &Path) -> Result<(), Error> {
    let _ = std::process::Command::new("launchctl")
        .args(["unload", &path.display().to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    Ok(())
}

#[cfg(target_os = "linux")]
fn load_service(_path: &Path) -> Result<(), Error> {
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();
    let status = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", "pigeonpost-agentd.service"])
        .status()?;
    if !status.success() {
        return Err("systemctl --user enable failed; on a headless box you may need `loginctl enable-linger $USER`".into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn unload_service(_path: &Path) -> Result<(), Error> {
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "disable", "--now", "pigeonpost-agentd.service"])
        .status();
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn load_service(_path: &Path) -> Result<(), Error> {
    Err("automatic installation is not implemented for this platform yet".into())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn unload_service(_path: &Path) -> Result<(), Error> {
    Ok(())
}

/// Whether a unit is installed, for `status` to report. Being installed is not the same as
/// running, and saying so plainly beats implying either.
pub fn installed_unit() -> Option<PathBuf> {
    unit_path().ok().filter(|p| p.exists())
}

// ---- session hooks ----------------------------------------------------------------------------
//
// The daemon guarantees delivery; these only decide when a session *notices*. Without them mail
// sits in the spool until somebody runs `agentd drain` by hand, which is the polling habit again
// wearing a different name.

/// The hook block a Claude Code settings file needs. `SessionStart` surfaces whatever arrived
/// while nothing was running; `Stop` catches mail that landed mid-session, which is otherwise
/// invisible until the next launch.
///
/// The command carries `--home`, so a hook installed in one repository drains that repository's
/// mailbox and no other. That is the whole reason these belong in the project rather than in the
/// user's settings: one box runs many agents, and a user-scoped hook would make every session on
/// the machine drain whichever mailbox was configured last.
fn claude_hooks(program: &Path, home: &Path) -> serde_json::Value {
    let command = format!(
        "{} --home {} agentd drain",
        program.display(),
        home.display()
    );
    let entry = |event: &str| {
        serde_json::json!({
            "matcher": "",
            "hooks": [{ "type": "command", "command": command, "timeout": 10 }],
            "_comment": format!("pigeonpost: surface this repo's new mail on {event}")
        })
    };
    serde_json::json!({
        "SessionStart": [entry("session start")],
        "Stop": [entry("session end")],
    })
}

/// Where the hooks go. The repository by default, the user's settings only when asked.
fn settings_target(global: bool) -> Result<PathBuf, Error> {
    if global {
        let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
        return Ok(PathBuf::from(home).join(".claude").join("settings.json"));
    }
    Ok(std::env::current_dir()?
        .join(".claude")
        .join("settings.json"))
}

/// Print the hook and MCP configuration, or merge the hooks into a settings file.
///
/// Project scope is the default because a machine runs one agent per repository. Installing into
/// the user's settings makes every session on the box drain one mailbox — which is what happened
/// before this defaulted correctly, and it is silent: the other agents simply never see their mail.
pub fn hooks(home: &Path, install: bool, global: bool) -> Result<(), Error> {
    // Installing a hook for an agent that has not onboarded produces a hook that drains on behalf
    // of a mailbox that does not exist. Say so at the point of the mistake rather than letting it
    // be discovered later as missing mail.
    if install && machine_home(home) != home && own_mailboxes(home).is_none() {
        return Err(format!(
            "no mailbox in {} yet — `agentd hooks` only wires up an existing mailbox, it does not \
create one. Run `pigeonpost postbox onboard --handle /namespace/name` first (add --agent to keep \
it in this folder), then install the hooks.",
            home.display()
        )
        .into());
    }
    let program = program()?;
    let hooks = claude_hooks(&program, home);
    let token_hint = format!("pigeonpost --home {} postbox token", home.display());

    if !install {
        let target = settings_target(global)?;
        println!("Claude Code — add to {} under \"hooks\":", target.display());
        println!();
        println!("{}", serde_json::to_string_pretty(&hooks)?);
        println!();
        println!("Or merge it automatically:  pigeonpost agentd hooks --install");
        println!();
        println!(
            "MCP — put this in .mcp.json in the repository, so this project's sessions act as"
        );
        println!("this project's mailbox. A user-scoped registration would apply to every project");
        println!("on the machine, which is rarely what you want with one agent per repo:");
        println!();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "mcpServers": {
                    "pigeonpost": {
                        "url": "https://mcp.pigeonpost.dev/mcp",
                        "headers": { "Authorization": "Bearer <token>" }
                    }
                }
            }))?
        );
        println!();
        println!("Get the token with:  {token_hint}");
        return Ok(());
    }

    let path = settings_target(global)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Merge rather than write: this file holds the person's model choice, permissions and plugins,
    // and replacing it to add two hooks would be a poor trade.
    let mut root: serde_json::Value = match std::fs::read_to_string(&path) {
        Ok(body) if !body.trim().is_empty() => serde_json::from_str(&body).map_err(|e| {
            format!(
                "{} is not valid JSON ({e}); not touching it",
                path.display()
            )
        })?,
        _ => serde_json::json!({}),
    };
    let backup = path.with_extension("json.pigeonpost-bak");
    if path.exists() {
        std::fs::copy(&path, &backup)?;
    }

    let slot = root
        .as_object_mut()
        .ok_or("settings.json is not a JSON object")?
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    let slot = slot
        .as_object_mut()
        .ok_or("settings.json \"hooks\" is not an object")?;
    for (event, value) in hooks.as_object().expect("hook block is an object") {
        slot.insert(event.clone(), value.clone());
    }

    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&root)? + "\n")?;
    std::fs::rename(&tmp, &path)?;
    println!("merged SessionStart and Stop hooks into {}", path.display());
    if backup.exists() {
        println!("previous settings kept at {}", backup.display());
    }
    if !global {
        println!("Scoped to this repository, so it drains only this mailbox.");
    } else {
        println!(
            "Installed for every project on this machine — with one agent per repo, prefer the"
        );
        println!("default project scope instead.");
    }
    println!("Restart any open session for them to take effect.");
    Ok(())
}

// ---- kill switch ------------------------------------------------------------------------------

/// Stop acting on anything unattended, now, without unpicking grants or stopping delivery.
///
/// A file rather than a flag in the config: it can be created by anything, including a shell script
/// or another tool, and the executor checks it per message rather than per start-up. Mail keeps
/// arriving and keeps being spooled — pausing is about *acting*, not about going deaf.
pub fn pause(home: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(home)?;
    std::fs::write(crate::executor::pause_path(home), "paused\n")?;
    println!("unattended action paused.");
    println!("Mail still arrives and is still spooled; nothing will be acted on until:");
    println!("  pigeonpost agentd resume");
    Ok(())
}

pub fn resume(home: &Path) -> Result<(), Error> {
    match std::fs::remove_file(crate::executor::pause_path(home)) {
        Ok(()) => println!("unattended action resumed"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => println!("not paused"),
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The daemon runs once per machine, so an agent home must look upward for the spool rather
    /// than at its own empty one.
    #[test]
    fn an_agent_home_finds_the_machine_spool() {
        let root = PathBuf::from("/home/x/.pigeonpost");
        let agent = root.join("agents").join("docdex");
        assert_eq!(machine_home(&agent), root);
        // The machine home itself is already right.
        assert_eq!(machine_home(&root), root);
        // Something that merely looks similar is left alone.
        let other = PathBuf::from("/home/x/elsewhere/docdex");
        assert_eq!(machine_home(&other), other);
    }

    /// One box, many agents, one shared spool: taking another agent's mail would mean it never
    /// learns the message existed, which is worse than a delay.
    #[test]
    fn an_agent_drains_only_its_own_mailbox() {
        let machine = tempfile::tempdir().unwrap();
        let agent_home = machine.path().join("agents").join("docdex");
        std::fs::create_dir_all(&agent_home).unwrap();
        std::fs::create_dir_all(machine.path().join(SPOOL_DIR)).unwrap();

        std::fs::write(
            agent_home.join("postbox.json"),
            serde_json::json!({ "identities": [ { "address": "/k/mine" } ] }).to_string(),
        )
        .unwrap();

        let mine = spool_file(machine.path(), "/k/mine");
        let theirs = spool_file(machine.path(), "/k/theirs");
        std::fs::write(&mine, "{\"event_id\":1}\n").unwrap();
        std::fs::write(&theirs, "{\"event_id\":2}\n").unwrap();

        drain(&agent_home, false).unwrap();

        assert!(
            std::fs::read_to_string(&mine).unwrap().trim().is_empty(),
            "its own mail should be taken"
        );
        assert!(
            !std::fs::read_to_string(&theirs).unwrap().trim().is_empty(),
            "another agent's mail must be left where it is"
        );
    }

    /// The case that bites when hooks are installed before onboarding: an agent that owns no
    /// mailbox must drain nothing, not everything. Falling back to "no filter" empties the box on
    /// behalf of an agent with no claim to any of it.
    #[test]
    fn an_agent_with_no_mailbox_drains_nothing() {
        let machine = tempfile::tempdir().unwrap();
        let agent_home = machine.path().join("agents").join("newcomer");
        std::fs::create_dir_all(&agent_home).unwrap();
        std::fs::create_dir_all(machine.path().join(SPOOL_DIR)).unwrap();

        let someone_else = spool_file(machine.path(), "/k/theirs");
        std::fs::write(&someone_else, "{\"event_id\":9}\n").unwrap();

        drain(&agent_home, false).unwrap();

        assert!(
            !std::fs::read_to_string(&someone_else)
                .unwrap()
                .trim()
                .is_empty(),
            "an agent with no mailbox must not empty anybody else's"
        );
    }

    /// The machine home legitimately owns the whole spool, so it still drains all of it.
    #[test]
    fn the_machine_home_still_drains_everything() {
        let machine = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(machine.path().join(SPOOL_DIR)).unwrap();
        let one = spool_file(machine.path(), "/k/one");
        std::fs::write(&one, "{\"event_id\":1}\n").unwrap();

        drain(machine.path(), false).unwrap();
        assert!(std::fs::read_to_string(&one).unwrap().trim().is_empty());
    }

    /// A path under the launcher's per-run unpack directory is gone by the time a hook fires.
    #[test]
    fn the_launchers_temporary_path_is_never_recorded() {
        assert!(is_ephemeral(Path::new(
            "/Users/x/.cache/pigeonpost/run/exec-8280-EvpE4v/pigeonpost"
        )));
        assert!(!is_ephemeral(Path::new("/Users/x/.local/bin/pigeonpost")));
        assert!(!is_ephemeral(Path::new("/usr/local/bin/pigeonpost")));
    }
}
