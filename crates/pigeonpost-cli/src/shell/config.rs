//! `[mailbox.shell]` — the machine owner's local opt-in to SSH (SDS §11.4).
//!
//! Off unless this file says otherwise, and only this file can say so: nothing the postbox sends
//! turns it on, widens a mode or adds a command. Every setting is checked when agentd starts and
//! again for each session; anything this build cannot enforce is refused, never approximated.
//!
//! ```toml
//! [[mailbox]]
//! address = "/acme/agent1"
//! workspace = "/srv/app"
//! [mailbox.shell]
//! enabled = true
//! mode = "container"                       # container | allowlist | unconfined
//! image = "docker.io/library/debian:13"    # container
//! network = "none"                         # container: none | outbound
//! read_only = false                        # container: mount the workspace read-only
//! max_sessions = 2
//! ```

// Parsed on every platform (agentd.toml is shared); enforced only where agent SSH runs.
#![cfg_attr(not(unix), allow(dead_code))]

use std::path::{Path, PathBuf};

pub const DEFAULT_IMAGE: &str = "docker.io/library/debian:13";
const MAX_COMMANDS: usize = 64;
const MAX_PATH_ARGS: u8 = 8;

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShellConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Required, with no default: the boundary is the owner's explicit choice.
    pub mode: String,
    /// `container`: the image each session runs in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// `container`: `podman` or `docker`. Unset: podman if installed, else docker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// `container`: `none` (default) or `outbound`.
    #[serde(default = "default_network")]
    pub network: String,
    /// `container`: mount the workspace read-only.
    #[serde(default)]
    pub read_only: bool,
    #[serde(default = "default_max_sessions")]
    pub max_sessions: u8,
    /// Ends a session after this long without traffic (60–900; the broker's 15 minutes is the cap).
    #[serde(default = "default_idle")]
    pub idle_secs: u64,
    /// Session recording (SDS §11.4 `record`). Not implemented: `true` is refused.
    #[serde(default)]
    pub record: bool,
    /// `unconfined`: required, as a second explicit statement that a session is a full shell as
    /// this machine's user, with everything that user can reach.
    #[serde(default)]
    pub unconfined_acknowledged: bool,
    /// `allowlist`: the only commands a session may run.
    #[serde(default, rename = "command", skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<AllowedCommand>,
}

/// One allowed command: an exact argv, optionally followed by up to `paths` arguments that must be
/// relative paths resolving (symlinks included) inside the workspace.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct AllowedCommand {
    pub argv: Vec<String>,
    #[serde(default)]
    pub paths: u8,
}

fn default_network() -> String {
    "none".into()
}
fn default_max_sessions() -> u8 {
    2
}
fn default_idle() -> u64 {
    900
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Container,
    Allowlist,
    Unconfined,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Container => "container",
            Mode::Allowlist => "allowlist",
            Mode::Unconfined => "unconfined",
        }
    }
}

/// A checked configuration, with the workspace canonicalized.
#[derive(Debug, Clone)]
pub struct Shell {
    pub mode: Mode,
    pub workspace: PathBuf,
    pub image: String,
    pub runtime: Option<String>,
    pub outbound: bool,
    pub read_only: bool,
    pub max_sessions: u8,
    pub idle_secs: u64,
    pub commands: Vec<AllowedCommand>,
}

/// Characters an argument, an image name or a container path may contain. No whitespace, quotes,
/// globs, `$`, `;`, `|`, `&`, redirections, backslashes or control characters — nothing a shell
/// would interpret, even though this build never hands one to a shell.
pub fn plain_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 256
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/:=@%+,-".contains(&b))
}

impl ShellConfig {
    /// Check everything this build can check without starting a session. `home` is the
    /// Pigeonpost home holding this machine's credentials, which no session may reach.
    pub fn validate(&self, workspace: &Path, home: &Path) -> Result<Shell, String> {
        if !cfg!(unix) {
            return Err("agent SSH runs on Linux and macOS only".into());
        }
        let mode = match self.mode.as_str() {
            "container" => Mode::Container,
            "allowlist" => Mode::Allowlist,
            "unconfined" => Mode::Unconfined,
            "user" => {
                return Err(
                    "mode \"user\" (dedicated unix user + mount namespace) is not available in this build; \
                     use \"container\" or \"allowlist\""
                        .into(),
                )
            }
            other => return Err(format!("unknown shell mode {other:?} (container, allowlist, unconfined)")),
        };
        if self.record {
            return Err(
                "session recording is not supported by this build; set record = false".into(),
            );
        }
        if !(1..=8).contains(&self.max_sessions) {
            return Err("max_sessions is 1-8".into());
        }
        if !(60..=900).contains(&self.idle_secs) {
            return Err("idle_secs is 60-900".into());
        }
        let workspace = check_workspace(workspace, home)?;
        let mut shell = Shell {
            mode,
            workspace,
            image: self.image.clone().unwrap_or_else(|| DEFAULT_IMAGE.into()),
            runtime: self.runtime.clone(),
            outbound: false,
            read_only: self.read_only,
            max_sessions: self.max_sessions,
            idle_secs: self.idle_secs,
            commands: Vec::new(),
        };
        match mode {
            Mode::Container => {
                if !plain_token(&shell.image) || shell.image.starts_with('-') {
                    return Err("image must be a plain image reference".into());
                }
                shell.outbound = match self.network.as_str() {
                    "none" => false,
                    "outbound" => true,
                    _ => return Err("network is none or outbound".into()),
                };
                if let Some(rt) = &self.runtime {
                    if !matches!(rt.as_str(), "podman" | "docker") {
                        return Err("runtime is podman or docker".into());
                    }
                }
                // `--mount` is comma-separated: a path with a comma or `=` could add options.
                let ws = shell.workspace.to_string_lossy();
                if ws.contains(',') || ws.contains('=') || ws.contains('\n') {
                    return Err(
                        "the workspace path may not contain ',' or '=' in container mode".into(),
                    );
                }
            }
            Mode::Allowlist => {
                if self.commands.is_empty() || self.commands.len() > MAX_COMMANDS {
                    return Err(format!(
                        "allowlist mode needs 1-{MAX_COMMANDS} [[mailbox.shell.command]] entries"
                    ));
                }
                for c in &self.commands {
                    check_command(c)?;
                }
                shell.commands = self.commands.clone();
            }
            Mode::Unconfined => {
                if !self.unconfined_acknowledged {
                    return Err(
                        "mode \"unconfined\" is a full shell as this machine's user; set \
                         unconfined_acknowledged = true to confirm that is intended"
                            .into(),
                    );
                }
            }
        }
        if mode != Mode::Container && (self.image.is_some() || self.runtime.is_some()) {
            return Err("image and runtime apply to container mode only".into());
        }
        if mode != Mode::Allowlist && !self.commands.is_empty() {
            return Err("commands apply to allowlist mode only".into());
        }
        Ok(shell)
    }
}

fn check_command(c: &AllowedCommand) -> Result<(), String> {
    let Some(program) = c.argv.first() else {
        return Err("an allowed command needs an argv".into());
    };
    if !program.starts_with('/') {
        return Err(format!(
            "allowed command {program:?} must be an absolute path"
        ));
    }
    if c.argv.len() > 32 || c.argv.iter().any(|a| !plain_token(a)) {
        return Err(format!(
            "allowed command {:?} has an argument with characters that are not allowed",
            c.argv
        ));
    }
    let meta = std::fs::metadata(program)
        .map_err(|_| format!("allowed command {program} does not exist"))?;
    if !meta.is_file() {
        return Err(format!("allowed command {program} is not a file"));
    }
    if c.paths > MAX_PATH_ARGS {
        return Err(format!("paths is at most {MAX_PATH_ARGS}"));
    }
    // A shell or an interpreter turns any argument into arbitrary code.
    let base = Path::new(program)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let interpreters = [
        "sh", "bash", "zsh", "dash", "ksh", "fish", "csh", "tcsh", "env", "python", "python3",
        "perl", "ruby", "node", "php", "lua", "awk", "gawk", "busybox", "sudo", "su", "doas",
        "xargs", "find", "nohup", "nice", "timeout", "ssh", "script",
    ];
    if interpreters.contains(&base) {
        return Err(format!(
            "{program} runs other programs; it cannot be an allowed command (it would make the allowlist a full shell)"
        ));
    }
    Ok(())
}

/// The workspace must exist, be a directory, and not be (or contain) anything a session must not
/// reach: the filesystem root, the user's whole home, or the Pigeonpost home with this machine's
/// credentials.
pub fn check_workspace(workspace: &Path, home: &Path) -> Result<PathBuf, String> {
    let ws = std::fs::canonicalize(workspace)
        .map_err(|_| format!("workspace {} does not exist", workspace.display()))?;
    if !ws.is_dir() {
        return Err(format!("workspace {} is not a directory", ws.display()));
    }
    if ws.parent().is_none() {
        return Err("the workspace cannot be the filesystem root".into());
    }
    if let Some(user_home) = std::env::var_os("HOME").map(PathBuf::from) {
        if std::fs::canonicalize(&user_home).is_ok_and(|h| h == ws) {
            return Err("the workspace cannot be your whole home directory".into());
        }
    }
    if let Ok(home) = std::fs::canonicalize(home) {
        if home.starts_with(&ws) || ws.starts_with(&home) {
            return Err(format!(
                "the workspace may not contain, or be inside, the Pigeonpost home ({}) that holds this machine's credentials",
                home.display()
            ));
        }
    }
    Ok(ws)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn cfg(mode: &str) -> ShellConfig {
        ShellConfig {
            enabled: true,
            mode: mode.into(),
            image: None,
            runtime: None,
            network: "none".into(),
            read_only: false,
            max_sessions: 2,
            idle_secs: 900,
            record: false,
            unconfined_acknowledged: false,
            commands: vec![],
        }
    }

    fn dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let ws = t.path().join("ws");
        let home = t.path().join("pp-home");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        (t, ws, home)
    }

    #[test]
    fn mode_is_explicit_and_unsupported_modes_are_refused() {
        let (_t, ws, home) = dirs();
        assert!(cfg("container").validate(&ws, &home).is_ok());
        let e = cfg("user").validate(&ws, &home).unwrap_err();
        assert!(e.contains("not available"), "{e}");
        assert!(cfg("chroot").validate(&ws, &home).is_err());
        let toml_no_mode = "enabled = true\n";
        assert!(
            toml::from_str::<ShellConfig>(toml_no_mode).is_err(),
            "mode has no default"
        );
        assert!(toml::from_str::<ShellConfig>("mode = \"container\"\nsurprise = 1\n").is_err());
        let mut rec = cfg("container");
        rec.record = true;
        assert!(rec.validate(&ws, &home).is_err());
    }

    #[test]
    fn unconfined_needs_a_second_explicit_yes() {
        let (_t, ws, home) = dirs();
        assert!(cfg("unconfined").validate(&ws, &home).is_err());
        let mut c = cfg("unconfined");
        c.unconfined_acknowledged = true;
        assert_eq!(c.validate(&ws, &home).unwrap().mode, Mode::Unconfined);
    }

    #[test]
    fn workspaces_that_would_expose_credentials_or_everything_are_refused() {
        let (t, ws, home) = dirs();
        // The Pigeonpost home inside the workspace, or the workspace inside it.
        assert!(check_workspace(t.path(), &home).is_err());
        let inner = home.join("agents/x");
        std::fs::create_dir_all(&inner).unwrap();
        assert!(check_workspace(&inner, &home).is_err());
        assert!(check_workspace(Path::new("/"), &home).is_err());
        assert!(check_workspace(&t.path().join("missing"), &home).is_err());
        // A symlink is judged by where it points.
        let link = t.path().join("link");
        std::os::unix::fs::symlink(&home, &link).unwrap();
        assert!(check_workspace(&link, &home).is_err());
        assert!(check_workspace(&ws, &home).is_ok());
        // Container mount options cannot be smuggled in through the path.
        let odd = t.path().join("a,readonly=false");
        std::fs::create_dir_all(&odd).unwrap();
        assert!(cfg("container").validate(&odd, &home).is_err());
    }

    #[test]
    fn allowlists_hold_exact_plain_commands_and_no_interpreters() {
        let (_t, ws, home) = dirs();
        let mut c = cfg("allowlist");
        assert!(c.validate(&ws, &home).is_err(), "empty allowlist");
        c.commands = vec![AllowedCommand {
            argv: vec!["/bin/ls".into(), "-la".into()],
            paths: 1,
        }];
        assert!(c.validate(&ws, &home).is_ok());
        for (argv, why) in [
            (vec!["ls"], "relative program"),
            (vec!["/bin/sh", "-c", "id"], "a shell"),
            (vec!["/usr/bin/env"], "env"),
            (vec!["/bin/ls", "$(id)"], "metacharacters"),
            (vec!["/bin/ls", "a;b"], "metacharacters"),
            (vec!["/bin/ls", "a b"], "whitespace"),
            (vec!["/nonexistent/prog"], "missing"),
        ] {
            c.commands = vec![AllowedCommand {
                argv: argv.iter().map(|s| s.to_string()).collect(),
                paths: 0,
            }];
            assert!(c.validate(&ws, &home).is_err(), "{why}");
        }
    }
}
