//! What a session's process is, per mode (SDS §11.4, D-20).
//!
//! * `container`: a rootless-friendly container (podman, else docker) with **only** the workspace
//!   bind-mounted at `/workspace`, every capability dropped, `no-new-privileges`, a read-only
//!   root filesystem, a small `/tmp`, pid/memory/CPU limits, no network unless the owner chose
//!   `outbound`, and never the container runtime's socket. Named after the session, so teardown
//!   removes it even when the client process is already gone.
//! * `allowlist`: no shell at all. One exact command per request, executed directly (never through
//!   a shell) with a fixed environment, in the workspace; trailing path arguments only when the
//!   entry allows them, and only paths that resolve — symlinks included — inside the workspace.
//! * `unconfined`: the machine user's own login shell in the workspace. Explicitly acknowledged in
//!   the config; it is the current `full` tier's risk, said out loud.
//!
//! In every mode the environment is built from nothing: SSH `env` requests are refused, and no
//! variable from agentd's own environment reaches a session except `PATH` (and, unconfined, the
//! user's `HOME`/`USER`/`SHELL`).

use super::config::{plain_token, Mode, Shell};
use std::path::{Path, PathBuf};

/// How to start one session's process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
    /// Allocate a terminal (`pty-req` was granted and the mode allows it).
    pub pty: bool,
    /// `(runtime, name)` of the container to remove on teardown.
    pub container: Option<(PathBuf, String)>,
}

const SAFE_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// `TERM` from the client, or `xterm-256color` if it is anything but a plain terminal name.
pub fn term(raw: &str) -> String {
    if !raw.is_empty()
        && raw.len() <= 32
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.+".contains(&b))
    {
        raw.to_string()
    } else {
        "xterm-256color".into()
    }
}

/// The container name for a session (also its teardown handle).
pub fn container_name(sid: &str) -> String {
    format!("pp-ssh-{}", sid.trim_start_matches("ssh_"))
}

/// Find a program on a fixed set of directories plus `PATH`.
pub fn find_program(name: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for d in ["/usr/local/bin", "/usr/bin", "/bin", "/opt/homebrew/bin"] {
        dirs.push(PathBuf::from(d));
    }
    dirs.into_iter().map(|d| d.join(name)).find(|p| p.is_file())
}

/// The container runtime to use: the configured one, else podman, else docker.
pub fn runtime(shell: &Shell) -> Result<(PathBuf, bool), String> {
    let pick = |n: &str| find_program(n).map(|p| (p, n == "podman"));
    match shell.runtime.as_deref() {
        Some(n) => pick(n).ok_or_else(|| format!("container runtime {n} is not installed")),
        None => pick("podman")
            .or_else(|| pick("docker"))
            .ok_or_else(|| "container mode needs podman or docker installed".to_string()),
    }
}

/// What to start for a shell (`command == None`) or an exec request.
pub fn launch(
    shell: &Shell,
    sid: &str,
    term_name: &str,
    pty: bool,
    command: Option<&str>,
) -> Result<Launch, String> {
    let term = term(term_name);
    match shell.mode {
        Mode::Allowlist => {
            let Some(line) = command else {
                return Err(allowlist_help(shell));
            };
            let argv = match_allowlist(shell, line)?;
            Ok(Launch {
                program: PathBuf::from(&argv[0]),
                args: argv[1..].to_vec(),
                env: vec![
                    ("PATH".into(), SAFE_PATH.into()),
                    ("HOME".into(), shell.workspace.to_string_lossy().into()),
                    ("LANG".into(), "C.UTF-8".into()),
                    ("TERM".into(), term),
                ],
                cwd: shell.workspace.clone(),
                // Commands, not a terminal: no pty, so nothing can be driven interactively.
                pty: false,
                container: None,
            })
        }
        Mode::Container => {
            let (rt, podman) = runtime(shell)?;
            let name = container_name(sid);
            let mut args = vec!["run".to_string(), "--rm".into(), "-i".into()];
            if pty {
                args.push("-t".into());
            }
            args.extend(container_args(shell, &name, &term, podman));
            args.push(shell.image.clone());
            args.push("/bin/sh".into());
            match command {
                Some(c) => {
                    args.push("-c".into());
                    args.push(c.to_string());
                }
                None => args.push("-l".into()),
            }
            Ok(Launch {
                program: rt.clone(),
                args,
                // The runtime client itself needs very little; nothing of the person's.
                env: vec![
                    ("PATH".into(), runtime_path()),
                    (
                        "HOME".into(),
                        std::env::var("HOME").unwrap_or_else(|_| "/".into()),
                    ),
                    ("LANG".into(), "C.UTF-8".into()),
                ],
                cwd: shell.workspace.clone(),
                pty,
                container: Some((rt, name)),
            })
        }
        Mode::Unconfined => {
            let sh = std::env::var("SHELL")
                .ok()
                .filter(|s| s.starts_with('/') && Path::new(s).is_file())
                .unwrap_or_else(|| "/bin/sh".into());
            let mut args = Vec::new();
            match command {
                Some(c) => {
                    args.push("-c".to_string());
                    args.push(c.to_string());
                }
                None => args.push("-l".into()),
            }
            let mut env = vec![
                ("PATH".to_string(), runtime_path()),
                ("TERM".into(), term),
                ("LANG".into(), "C.UTF-8".into()),
                ("SHELL".into(), sh.clone()),
            ];
            for k in ["HOME", "USER", "LOGNAME"] {
                if let Ok(v) = std::env::var(k) {
                    env.push((k.into(), v));
                }
            }
            Ok(Launch {
                program: PathBuf::from(sh),
                args,
                env,
                cwd: shell.workspace.clone(),
                pty,
                container: None,
            })
        }
    }
}

fn runtime_path() -> String {
    std::env::var("PATH")
        .ok()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| format!("{SAFE_PATH}:/opt/homebrew/bin"))
}

/// The isolation flags, shared by docker and podman.
pub fn container_args(shell: &Shell, name: &str, term: &str, podman: bool) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "--name".into(),
        name.into(),
        "--label".into(),
        "dev.pigeonpost.shell=1".into(),
        "--hostname".into(),
        "pigeonpost-shell".into(),
        "--init".into(),
        "--network".into(),
        if shell.outbound { "bridge" } else { "none" }.into(),
        "--cap-drop".into(),
        "ALL".into(),
        "--security-opt".into(),
        "no-new-privileges".into(),
        "--pids-limit".into(),
        "256".into(),
        "--memory".into(),
        "1g".into(),
        "--cpus".into(),
        "1".into(),
        "--read-only".into(),
        "--tmpfs".into(),
        "/tmp:rw,nosuid,nodev,size=64m".into(),
        "--mount".into(),
        format!(
            "type=bind,source={},target=/workspace{}",
            shell.workspace.display(),
            if shell.read_only { ",readonly" } else { "" }
        ),
        "--workdir".into(),
        "/workspace".into(),
        "--env".into(),
        "HOME=/workspace".into(),
        "--env".into(),
        format!("TERM={term}"),
        "--env".into(),
        "LANG=C.UTF-8".into(),
        "--pull".into(),
        "missing".into(),
    ];
    if podman {
        // Rootless podman: the person's files stay owned by this machine's user.
        a.push("--userns".into());
        a.push("keep-id".into());
    } else {
        // Docker: never root inside; this machine's user owns what the session writes.
        a.push("--user".into());
        a.push(format!(
            "{}:{}",
            rustix::process::getuid().as_raw(),
            rustix::process::getgid().as_raw()
        ));
    }
    a
}

fn allowlist_help(shell: &Shell) -> String {
    let mut s = String::from(
        "This agent allows only these commands (run one with: pigeonpost ssh <agent> -- <command>):\r\n",
    );
    for c in &shell.commands {
        s.push_str("  ");
        s.push_str(&c.argv.join(" "));
        if c.paths > 0 {
            s.push_str(&format!(" [up to {} workspace path(s)]", c.paths));
        }
        s.push_str("\r\n");
    }
    s
}

/// Match one exec request against the allowlist. Returns the argv to execute directly.
pub fn match_allowlist(shell: &Shell, line: &str) -> Result<Vec<String>, String> {
    let refuse = |why: &str| Err(format!("refused: {why}\r\n{}", allowlist_help(shell)));
    if line.len() > 1024 || line.chars().any(|c| c.is_control()) {
        return refuse("the command contains control characters or is too long");
    }
    let tokens: Vec<&str> = line.split(' ').filter(|t| !t.is_empty()).collect();
    if tokens.is_empty() {
        return refuse("empty command");
    }
    if let Some(bad) = tokens.iter().find(|t| !plain_token(t)) {
        return refuse(&format!(
            "{bad:?} contains characters that are not allowed (no shell syntax is interpreted here)"
        ));
    }
    for entry in &shell.commands {
        let n = entry.argv.len();
        // The program may be named by its full path or by its file name.
        let program_ok = tokens[0] == entry.argv[0]
            || Path::new(&entry.argv[0])
                .file_name()
                .and_then(|f| f.to_str())
                == Some(tokens[0]);
        if tokens.len() < n
            || !program_ok
            || tokens[1..n] != entry.argv[1..]
            || tokens.len() - n > entry.paths as usize
        {
            continue;
        }
        let mut argv = entry.argv.clone();
        for raw in &tokens[n..] {
            argv.push(workspace_path(&shell.workspace, raw)?);
        }
        return Ok(argv);
    }
    refuse("not an allowed command")
}

/// A trailing argument must name something inside the workspace, after resolving symlinks.
pub fn workspace_path(workspace: &Path, raw: &str) -> Result<String, String> {
    let refuse = |why: &str| Err(format!("refused: {raw:?} {why}"));
    if raw.starts_with('-') {
        return refuse("looks like an option");
    }
    if raw.starts_with('/') {
        return refuse("is absolute; name paths relative to the workspace");
    }
    if Path::new(raw)
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return refuse("leaves the workspace");
    }
    let resolved = std::fs::canonicalize(workspace.join(raw))
        .map_err(|_| format!("refused: {raw:?} does not exist in the workspace"))?;
    if !resolved.starts_with(workspace) {
        return refuse("resolves outside the workspace (through a symlink)");
    }
    Ok(resolved.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::config::AllowedCommand;

    fn shell(mode: Mode, ws: &Path) -> Shell {
        Shell {
            mode,
            workspace: std::fs::canonicalize(ws).unwrap(),
            image: "docker.io/library/debian:13".into(),
            runtime: None,
            outbound: false,
            read_only: false,
            max_sessions: 2,
            idle_secs: 900,
            commands: vec![
                AllowedCommand {
                    argv: vec!["/bin/cat".into()],
                    paths: 2,
                },
                AllowedCommand {
                    argv: vec!["/bin/ls".into(), "-la".into()],
                    paths: 0,
                },
            ],
        }
    }

    #[test]
    fn allowlist_matches_exact_commands_and_nothing_a_shell_would_interpret() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("notes.txt"), "x").unwrap();
        let s = shell(Mode::Allowlist, t.path());
        let ws = s.workspace.clone();
        assert_eq!(match_allowlist(&s, "ls -la").unwrap(), ["/bin/ls", "-la"]);
        assert_eq!(
            match_allowlist(&s, "/bin/ls -la").unwrap(),
            ["/bin/ls", "-la"]
        );
        assert_eq!(
            match_allowlist(&s, "cat notes.txt").unwrap(),
            [
                "/bin/cat".to_string(),
                ws.join("notes.txt").to_string_lossy().into_owned()
            ]
        );
        for attempt in [
            "ls -la; id",
            "ls -la && id",
            "ls -la | sh",
            "ls -la `id`",
            "ls -la $(id)",
            "ls -la > /tmp/x",
            "cat notes.txt\nid",
            "ls -la\tid",
            "ls -l",
            "ls -la extra",
            "ls",
            "id",
            "/usr/bin/id",
            "sh -c id",
            "cat 'notes.txt'",
            "cat \"notes.txt\"",
            "cat notes.*",
            "cat ~/x",
            "LD_PRELOAD=/tmp/x.so ls -la",
        ] {
            assert!(match_allowlist(&s, attempt).is_err(), "{attempt:?}");
        }
    }

    #[test]
    fn path_arguments_cannot_leave_the_workspace() {
        let t = tempfile::tempdir().unwrap();
        let ws = t.path().join("ws");
        std::fs::create_dir_all(ws.join("sub")).unwrap();
        std::fs::write(ws.join("sub/ok"), "x").unwrap();
        std::fs::write(t.path().join("secret"), "s").unwrap();
        std::os::unix::fs::symlink(t.path().join("secret"), ws.join("escape")).unwrap();
        std::os::unix::fs::symlink(t.path(), ws.join("updir")).unwrap();
        std::os::unix::fs::symlink(ws.join("sub/ok"), ws.join("inside")).unwrap();
        let s = shell(Mode::Allowlist, &ws);
        assert!(match_allowlist(&s, "cat sub/ok").is_ok());
        assert!(
            match_allowlist(&s, "cat inside").is_ok(),
            "a symlink that stays inside"
        );
        for attempt in [
            "cat ../secret",
            "cat sub/../../secret",
            "cat escape",
            "cat updir/secret",
            "cat /etc/passwd",
            "cat -n",
            "cat --help",
            "cat missing",
            "cat sub/ok sub/ok sub/ok",
        ] {
            assert!(match_allowlist(&s, attempt).is_err(), "{attempt:?}");
        }
    }

    #[test]
    fn environments_are_built_from_nothing() {
        let t = tempfile::tempdir().unwrap();
        let s = shell(Mode::Allowlist, t.path());
        std::env::set_var("PP_TEST_SECRET", "leak");
        let l = launch(
            &s,
            "ssh_abcdefghijklmnopqrstuvwxyz",
            "xterm; rm -rf /",
            false,
            Some("ls -la"),
        )
        .unwrap();
        let keys: Vec<&str> = l.env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["PATH", "HOME", "LANG", "TERM"]);
        assert!(l.env.iter().all(|(_, v)| !v.contains("leak")));
        assert_eq!(l.env[3].1, "xterm-256color", "a hostile TERM is replaced");
        assert!(!l.pty);
        assert_eq!(l.cwd, s.workspace);
        // A shell request in allowlist mode is a refusal that lists what is allowed.
        let e = launch(&s, "ssh_abcdefghijklmnopqrstuvwxyz", "xterm", true, None).unwrap_err();
        assert!(e.contains("/bin/ls -la"), "{e}");
        assert_eq!(term("screen-256color"), "screen-256color");
    }

    #[test]
    fn containers_mount_only_the_workspace_and_drop_everything() {
        let t = tempfile::tempdir().unwrap();
        let mut s = shell(Mode::Container, t.path());
        let a = container_args(&s, "pp-ssh-x", "xterm", false);
        let joined = a.join(" ");
        assert_eq!(
            a.iter()
                .filter(|x| *x == "--mount" || *x == "-v" || *x == "--volume")
                .count(),
            1
        );
        assert!(joined.contains(&format!(
            "type=bind,source={},target=/workspace",
            s.workspace.display()
        )));
        for must in [
            "--cap-drop ALL",
            "--security-opt no-new-privileges",
            "--network none",
            "--read-only",
            "--pids-limit 256",
            "--init",
        ] {
            assert!(joined.contains(must), "{must}");
        }
        for never in [
            "--privileged",
            "docker.sock",
            "podman.sock",
            "--pid host",
            "--network host",
            "--cap-add",
            "--device",
        ] {
            assert!(!joined.contains(never), "{never}");
        }
        assert!(joined.contains("--user "), "never root in docker");
        assert!(container_args(&s, "n", "xterm", true)
            .join(" ")
            .contains("--userns keep-id"));
        s.outbound = true;
        s.read_only = true;
        let joined = container_args(&s, "n", "xterm", false).join(" ");
        assert!(
            joined.contains("--network bridge") && joined.contains("target=/workspace,readonly")
        );
        assert_eq!(
            container_name("ssh_abcdefghijklmnopqrstuvwxyz"),
            "pp-ssh-abcdefghijklmnopqrstuvwxyz"
        );
    }
}
