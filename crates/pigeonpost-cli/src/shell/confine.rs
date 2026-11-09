//! What a session's process is, per mode (SDS §11.4, D-20).
//!
//! * `container`: a rootless-friendly container (podman, else docker) with **only** the workspace
//!   bind-mounted at `/workspace`, every capability dropped, `no-new-privileges`, a read-only
//!   root filesystem, a small `/tmp`, pid/memory/CPU limits, no network unless the owner chose
//!   `outbound`, and never the container runtime's socket. Named after the session, so teardown
//!   removes it even when the client process is already gone. `outbound` is the public internet
//!   only: an egress policy ([`EGRESS_POLICY`]) refuses private, link-local (cloud metadata),
//!   carrier-grade NAT and multicast destinations and the container's gateway, then drops every
//!   privilege before the session's program starts. Rootful Podman is refused like rootful Docker.
//! * `allowlist`: no shell at all. One exact command per request, executed directly (never through
//!   a shell) inside the same restricted container as `container` mode; trailing paths only when the
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

/// Set in an unconfined session's environment so teardown can find every process the session
/// started, including ones that left its process group with `setsid` (audit 2026-11-03 P3-5).
pub const SESSION_MARKER: &str = "PIGEONPOST_SSH_SESSION";

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

/// How a Docker daemon isolates container root from the host (from `docker info`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockerIsolation {
    /// Rootless Docker: the daemon itself runs as an unprivileged user.
    Rootless,
    /// `userns-remap`: container root maps to an unprivileged host range.
    UsernsRemap,
    /// Docker Desktop: containers run inside its Linux VM, not on this machine's kernel.
    DesktopVm,
    /// A rootful daemon without remapping: container root is host root after an escape.
    Rootful,
}

/// Classify `docker info --format '{{json .SecurityOptions}}|{{.OperatingSystem}}'` output.
pub fn classify_docker(info: &str) -> DockerIsolation {
    let (options, os) = info.split_once('|').unwrap_or((info, ""));
    if options.contains("name=rootless") {
        DockerIsolation::Rootless
    } else if options.contains("name=userns") {
        DockerIsolation::UsernsRemap
    } else if os.contains("Docker Desktop") {
        DockerIsolation::DesktopVm
    } else {
        DockerIsolation::Rootful
    }
}

/// Refuse rootful Podman: agentd running as root, or a Podman connection to a rootful service. A
/// container escape there is a root escape, and unlike Docker there is no reason to accept it —
/// rootless Podman is the recommended runtime (audit 2026-11-03 P2-14).
fn check_podman(rt: &Path) -> Result<(), String> {
    let out = std::process::Command::new(rt)
        .args(["info", "--format", "{{.Host.Security.Rootless}}"])
        .env_clear()
        .envs(runtime_env())
        .output()
        .map_err(|_| "cannot ask Podman how it isolates containers".to_string())?;
    if !out.status.success() {
        return Err("cannot ask Podman how it isolates containers".into());
    }
    podman_rootless(&String::from_utf8_lossy(&out.stdout))
}

/// Judge `podman info --format '{{.Host.Security.Rootless}}'` output: only `true` is accepted.
pub fn podman_rootless(info: &str) -> Result<(), String> {
    if info.trim() == "true" {
        Ok(())
    } else {
        Err(
            "refused: this Podman runs containers as root (rootful). Run agentd as an ordinary \
             user with rootless Podman"
                .into(),
        )
    }
}

/// Refuse a rootful Docker daemon unless the owner acknowledged it.
fn check_docker(shell: &Shell, rt: &Path) -> Result<(), String> {
    let out = std::process::Command::new(rt)
        .args([
            "info",
            "--format",
            "{{json .SecurityOptions}}|{{.OperatingSystem}}",
        ])
        .env_clear()
        .envs(runtime_env())
        .output()
        .map_err(|_| "cannot ask Docker how it isolates containers".to_string())?;
    if !out.status.success() {
        return Err("cannot ask Docker how it isolates containers (is the daemon running?)".into());
    }
    let isolation = classify_docker(&String::from_utf8_lossy(&out.stdout));
    if isolation == DockerIsolation::Rootful && !shell.docker_rootful_ok {
        return Err(
            "refused: this Docker daemon runs containers as host root (rootful, no user-namespace \
             remapping). Use rootless Podman (preferred), rootless Docker or userns-remap, or set \
             docker_rootful_acknowledged = true to accept that a container escape is a root escape"
                .into(),
        );
    }
    Ok(())
}

/// The container runtime to use: the configured one, else podman, else docker.
pub fn runtime(shell: &Shell) -> Result<(PathBuf, bool), String> {
    let pick = |n: &str| find_program(n).map(|p| (p, n == "podman"));
    match shell.runtime.as_deref() {
        Some(n) => pick(n).ok_or_else(|| format!("container runtime {n} is not installed")),
        None => pick("podman")
            .or_else(|| pick("docker"))
            .ok_or_else(|| "confined modes need podman or docker installed".to_string()),
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
            let argv = match_allowlist(shell, line)?
                .into_iter()
                .map(|arg| match Path::new(&arg).strip_prefix(&shell.workspace) {
                    Ok(relative) => Path::new("/workspace")
                        .join(relative)
                        .to_string_lossy()
                        .into_owned(),
                    Err(_) => arg,
                })
                .collect::<Vec<_>>();
            confined_launch(shell, sid, &term, false, &argv)
        }
        Mode::Container => {
            let mut argv = vec!["/bin/sh".to_string()];
            match command {
                Some(c) => {
                    argv.push("-c".into());
                    argv.push(c.to_string());
                }
                None => argv.push("-l".into()),
            }
            confined_launch(shell, sid, &term, pty, &argv)
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
                // Teardown finds descendants that left the process group (`setsid`) by this.
                (SESSION_MARKER.into(), sid.to_string()),
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

/// Both confined modes share the same runtime checks and isolation. Exact argv matching alone
/// is not a sandbox: otherwise an approved tool can execute workspace hooks on the host.
fn confined_launch(
    shell: &Shell,
    sid: &str,
    term: &str,
    pty: bool,
    argv: &[String],
) -> Result<Launch, String> {
    let (rt, podman) = runtime(shell)?;
    if podman {
        check_podman(&rt)?;
    } else {
        check_docker(shell, &rt)?;
    }
    Ok(confined_plan(shell, sid, term, pty, argv, rt, podman))
}

fn confined_plan(
    shell: &Shell,
    sid: &str,
    term: &str,
    pty: bool,
    argv: &[String],
    rt: PathBuf,
    podman: bool,
) -> Launch {
    let name = container_name(sid);
    let mut args = vec!["run".to_string(), "--rm".into(), "-i".into()];
    if pty {
        args.push("-t".into());
    }
    args.extend(container_args(shell, &name, term, podman));
    if shell.outbound {
        // The egress policy runs first, as the container's (unprivileged-on-the-host) root, then
        // becomes this machine's user with no capabilities and execs the program directly.
        args.extend([
            "--entrypoint".into(),
            "/bin/sh".into(),
            shell.image.clone(),
            "-c".into(),
            EGRESS_POLICY.into(),
            "pigeonpost-egress".into(),
            rustix::process::getuid().as_raw().to_string(),
            rustix::process::getgid().as_raw().to_string(),
        ]);
        args.extend_from_slice(argv);
    } else {
        // Override image ENTRYPOINT too: an allowlisted program is the actual executable, never a
        // shell string or an image-provided wrapper. No host program runs on a missing runtime/image.
        args.extend(["--entrypoint".into(), argv[0].clone(), shell.image.clone()]);
        args.extend_from_slice(&argv[1..]);
    }
    Launch {
        program: rt.clone(),
        args,
        env: runtime_env(),
        cwd: shell.workspace.clone(),
        pty,
        container: Some((rt, name)),
    }
}

/// The container runtime client's environment: PATH, HOME, and the variables that say where this
/// machine's runtime listens (rootless podman, a Docker context). Never passed into the container.
fn runtime_env() -> Vec<(String, String)> {
    let mut env = vec![
        ("PATH".to_string(), runtime_path()),
        (
            "HOME".into(),
            std::env::var("HOME").unwrap_or_else(|_| "/".into()),
        ),
        ("LANG".into(), "C.UTF-8".into()),
    ];
    for k in [
        "DOCKER_HOST",
        "DOCKER_CONTEXT",
        "DOCKER_CONFIG",
        "CONTAINER_HOST",
        "XDG_RUNTIME_DIR",
    ] {
        if let Ok(v) = std::env::var(k) {
            env.push((k.to_string(), v));
        }
    }
    env
}

fn runtime_path() -> String {
    std::env::var("PATH")
        .ok()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| format!("{SAFE_PATH}:/opt/homebrew/bin"))
}

/// The `network = "outbound"` egress policy (audit 2026-11-03 P2-14), run by `/bin/sh` inside the
/// container's own network namespace before anything else. Policy-routing rules come before every
/// route, so they also cover the container's own subnet and its gateway (the host). DNS to the
/// configured resolvers stays open (port 53 only). Every step must succeed or nothing runs: an
/// image without `ip` (iproute2) or `setpriv` (util-linux), or a kernel without rule port matching,
/// refuses outbound rather than run with open egress. `setpriv` then becomes this machine's user,
/// clears the bounding set and inheritable capabilities and sets no-new-privileges, so the session
/// can never undo the rules. Arguments: uid, gid, then the program and its arguments.
pub const EGRESS_POLICY: &str = r#"set -eu
uid=$1; gid=$2; shift 2
if ! command -v ip >/dev/null 2>&1 || ! command -v setpriv >/dev/null 2>&1; then
  echo 'pigeonpost: network = "outbound" needs ip (iproute2) and setpriv (util-linux) in the image to enforce its egress policy; refusing' >&2
  exit 126
fi
for ns in $(sed -n 's/^nameserver[[:space:]][[:space:]]*\([0-9][0-9.]*\)[[:space:]]*$/\1/p' /etc/resolv.conf); do
  ip rule add pref 50 to "$ns/32" ipproto udp dport 53 lookup main
  ip rule add pref 50 to "$ns/32" ipproto tcp dport 53 lookup main
done
for net in 0.0.0.0/8 10.0.0.0/8 100.64.0.0/10 169.254.0.0/16 172.16.0.0/12 192.0.0.0/24 192.168.0.0/16 198.18.0.0/15 224.0.0.0/4 240.0.0.0/4; do
  ip rule add pref 100 prohibit to "$net"
done
for gw in $(ip -4 route show default | sed -n 's/^default via \([0-9][0-9.]*\).*/\1/p'); do
  ip rule add pref 100 prohibit to "$gw/32"
done
if [ -e /proc/net/if_inet6 ]; then
  for net in fc00::/7 fe80::/10 ff00::/8 64:ff9b::/96 ::ffff:0:0/96; do
    ip -6 rule add pref 100 prohibit to "$net"
  done
fi
exec setpriv --reuid="$uid" --regid="$gid" --clear-groups --inh-caps=-all --bounding-set=-all --no-new-privs -- "$@"
"#;

/// The label every session container carries, with this machine user's uid, so agentd can find
/// containers a crash left behind ([`sweep_leftovers`]).
pub const OWNER_LABEL: &str = "dev.pigeonpost.shell.uid";

/// Remove session containers left behind by an earlier agentd run (a crash or a kill skips the
/// teardown). Called once at start, before any session exists; only this user's containers.
pub fn sweep_leftovers(rt: &Path) -> usize {
    let uid = rustix::process::getuid().as_raw();
    let out = std::process::Command::new(rt)
        .args([
            "ps",
            "-aq",
            "--filter",
            "label=dev.pigeonpost.shell=1",
            "--filter",
            &format!("label={OWNER_LABEL}={uid}"),
        ])
        .env_clear()
        .envs(runtime_env())
        .output();
    let Ok(out) = out else { return 0 };
    if !out.status.success() {
        return 0;
    }
    let ids: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .filter(|id| id.len() <= 64 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_string)
        .collect();
    if ids.is_empty() {
        return 0;
    }
    let mut rm = std::process::Command::new(rt);
    rm.args(["rm", "-f"])
        .args(&ids)
        .env_clear()
        .envs(runtime_env());
    match rm.output() {
        Ok(o) if o.status.success() => ids.len(),
        _ => 0,
    }
}

/// The isolation flags, shared by docker and podman.
pub fn container_args(shell: &Shell, name: &str, term: &str, podman: bool) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "--name".into(),
        name.into(),
        "--label".into(),
        "dev.pigeonpost.shell=1".into(),
        "--label".into(),
        format!("{OWNER_LABEL}={}", rustix::process::getuid().as_raw()),
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
    if shell.outbound {
        // The egress policy needs the container's root and NET_ADMIN *inside its own namespaces*
        // for a moment; it then drops to this machine's user with nothing (EGRESS_POLICY).
        for cap in ["NET_ADMIN", "SETUID", "SETGID", "SETPCAP"] {
            a.push("--cap-add".into());
            a.push(cap.into());
        }
        if podman {
            a.push("--userns".into());
            a.push("keep-id".into());
        }
        a.push("--user".into());
        a.push("0:0".into());
    } else if podman {
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
            image: crate::shell::config::DEFAULT_IMAGE.into(),
            docker_rootful_ok: false,
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
    fn an_allowed_command_never_falls_back_to_host_execution() {
        let t = tempfile::tempdir().unwrap();
        let mut s = shell(Mode::Allowlist, t.path());
        // A missing configured runtime must not select another runtime or the approved host tool.
        s.runtime = Some("pp-runtime-that-is-not-installed-6df9d5".into());
        let result = launch(
            &s,
            "ssh_abcdefghijklmnopqrstuvwxyz",
            "xterm",
            false,
            Some("ls -la"),
        );
        assert!(result.unwrap_err().contains("is not installed"));
    }

    #[test]
    fn environments_are_built_from_nothing() {
        let t = tempfile::tempdir().unwrap();
        let s = shell(Mode::Allowlist, t.path());
        std::env::set_var("PP_TEST_SECRET", "leak");
        let l = confined_plan(
            &s,
            "ssh_abcdefghijklmnopqrstuvwxyz",
            &term("xterm; rm -rf /"),
            false,
            &["/bin/ls".into(), "-la".into()],
            PathBuf::from("/usr/bin/docker"),
            false,
        );
        let keys: Vec<&str> = l.env.iter().map(|(k, _)| k.as_str()).collect();
        assert!(!keys.contains(&"PP_TEST_SECRET"));
        assert!(l.env.iter().all(|(_, v)| !v.contains("leak")));
        assert!(
            l.args.contains(&"TERM=xterm-256color".into()),
            "a hostile TERM is replaced"
        );
        assert_eq!(l.program, PathBuf::from("/usr/bin/docker"));
        assert!(l.container.is_some());
        assert!(l.args.ends_with(&[
            "--entrypoint".into(),
            "/bin/ls".into(),
            s.image.clone(),
            "-la".into()
        ]));
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

    #[test]
    fn outbound_runs_the_egress_policy_before_the_program() {
        let t = tempfile::tempdir().unwrap();
        let mut s = shell(Mode::Container, t.path());
        s.outbound = true;
        for podman in [false, true] {
            let joined = container_args(&s, "n", "xterm", podman).join(" ");
            assert!(
                joined.contains("--cap-drop ALL --security-opt no-new-privileges")
                    || joined.contains("--cap-drop ALL")
            );
            assert!(
                joined.contains("--cap-add NET_ADMIN") && joined.contains("--user 0:0"),
                "{joined}"
            );
            assert!(!joined.contains("SYS_ADMIN") && !joined.contains("--privileged"));
            assert_eq!(joined.contains("--userns keep-id"), podman);
        }
        let argv = vec!["/bin/sh".to_string(), "-l".to_string()];
        let l = confined_plan(
            &s,
            "ssh_x",
            "xterm",
            true,
            &argv,
            PathBuf::from("docker"),
            false,
        );
        let i = l.args.iter().position(|a| a == "--entrypoint").unwrap();
        assert_eq!(l.args[i + 1], "/bin/sh");
        assert_eq!(l.args[i + 2], s.image);
        assert_eq!(l.args[i + 3], "-c");
        assert_eq!(l.args[i + 4], EGRESS_POLICY);
        assert_eq!(
            &l.args[l.args.len() - 2..],
            &argv[..],
            "the program and its arguments come last, unchanged"
        );
        // Without outbound nothing changes: no capability is added and the program is the entrypoint.
        s.outbound = false;
        let l = confined_plan(
            &s,
            "ssh_x",
            "xterm",
            true,
            &argv,
            PathBuf::from("docker"),
            false,
        );
        assert!(!l
            .args
            .iter()
            .any(|a| a == "--cap-add" || a == EGRESS_POLICY));
    }

    /// The policy script itself, with stand-in `ip` and `setpriv` that record what they were asked.
    #[cfg(unix)]
    #[test]
    fn the_egress_policy_blocks_private_ranges_and_fails_closed() {
        use std::os::unix::fs::PermissionsExt;
        let bin = tempfile::tempdir().unwrap();
        let log = bin.path().join("log");
        let fake = |name: &str, body: &str| {
            let p = bin.path().join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        let run = |path: &str| {
            std::process::Command::new("/bin/sh")
                .args([
                    "-c",
                    EGRESS_POLICY,
                    "pigeonpost-egress",
                    "1000",
                    "1001",
                    "/bin/true",
                    "x",
                ])
                .env_clear()
                .env("PATH", path)
                .output()
                .unwrap()
        };
        // Missing tools: refused, nothing runs.
        let out = run(bin.path().to_str().unwrap());
        assert_eq!(out.status.code(), Some(126));
        assert!(String::from_utf8_lossy(&out.stderr).contains("refusing"));
        fake(
            "ip",
            &format!(
                "echo \"ip $*\" >> {l}\ncase \"$*\" in *'route show default'*) echo 'default via 172.17.0.1 dev eth0';; esac",
                l = log.display()
            ),
        );
        fake(
            "setpriv",
            &format!("echo \"setpriv $*\" >> {}", log.display()),
        );
        let out = run(&format!("{}:/usr/bin:/bin", bin.path().display()));
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let log = std::fs::read_to_string(&log).unwrap();
        for net in [
            "10.0.0.0/8",
            "172.16.0.0/12",
            "192.168.0.0/16",
            "169.254.0.0/16",
            "100.64.0.0/10",
            "172.17.0.1/32",
        ] {
            assert!(
                log.contains(&format!("ip rule add pref 100 prohibit to {net}")),
                "{net}\n{log}"
            );
        }
        let last = log.lines().last().unwrap();
        assert_eq!(
            last,
            "setpriv --reuid=1000 --regid=1001 --clear-groups --inh-caps=-all --bounding-set=-all --no-new-privs -- /bin/true x"
        );
        // A failing rule stops everything: the program never starts.
        fake("ip", "exit 1");
        let out = run(&format!("{}:/usr/bin:/bin", bin.path().display()));
        assert!(!out.status.success());
    }

    #[test]
    fn rootful_podman_is_refused() {
        assert!(podman_rootless("true\n").is_ok());
        for bad in ["false", "", "garbage", "True"] {
            assert!(podman_rootless(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn docker_isolation_is_read_from_docker_info() {
        assert_eq!(
            classify_docker(
                r#"["name=seccomp,profile=builtin","name=rootless","name=cgroupns"]|Ubuntu 24.04"#
            ),
            DockerIsolation::Rootless
        );
        assert_eq!(
            classify_docker(r#"["name=seccomp,profile=builtin","name=userns"]|Ubuntu 24.04"#),
            DockerIsolation::UsernsRemap
        );
        assert_eq!(
            classify_docker(
                r#"["name=seccomp,profile=unconfined","name=cgroupns"]|Docker Desktop"#
            ),
            DockerIsolation::DesktopVm
        );
        assert_eq!(
            classify_docker(
                r#"["name=apparmor","name=seccomp,profile=builtin","name=cgroupns"]|Ubuntu 24.04.1 LTS"#
            ),
            DockerIsolation::Rootful
        );
        assert_eq!(
            classify_docker(""),
            DockerIsolation::Rootful,
            "unknown is treated as rootful"
        );
    }

    #[test]
    fn the_default_image_is_pinned_by_digest() {
        let d = crate::shell::config::DEFAULT_IMAGE;
        let (_, digest) = d.split_once("@sha256:").expect("digest-pinned");
        assert_eq!(digest.len(), 64);
        assert!(digest.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(crate::shell::config::plain_token(d));
    }
}
