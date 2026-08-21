//! `pigeonpost onboard agent` — the third of the three commands somebody actually types.
//!
//! ```text
//! npm i -g @bekirdag/pigeonpost
//! pigeonpost login
//! pigeonpost onboard agent
//! ```
//!
//! Everything this expands to could already be typed by hand:
//!
//! ```text
//! pigeonpost --agent <dir> postbox onboard --handle /<you>/<dir> --trust "/<you>/*" \
//!   --verb report_status --verb answer_question --verb run_tests --verb make_change \
//!   --verb git_push --verb deploy --git-repo auto
//! pigeonpost agentd install
//! ```
//!
//! Nobody types that. Every value in it is derivable from where the command was run and who is
//! signed in, so this derives them and says what it decided.

use std::path::Path;

use crate::{login_cmd, postbox_cmd, workspace_cmd};

type Error = Box<dyn std::error::Error>;

/// Namespaces that belong to a provider rather than to a person: a name in one is earned by proving
/// an identity, and the namespace itself is everybody's. `/github` is not somebody's fleet, so the
/// person is `/github/alex` and their agents live under that.
const PROVIDER_NAMESPACES: &[&str] = &["github", "google", "pp"];

pub async fn agent(
    base_url: &str,
    name: Option<&str>,
    dry_run: bool,
    json: bool,
) -> Result<(), Error> {
    let directory = std::env::current_dir()?;
    let derived = match name {
        Some(given) => sanitise(given),
        None => sanitise(
            directory
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default(),
        ),
    };
    let derived = derived
        .ok_or("could not turn this directory's name into an agent name — pass one with --name")?;

    let home = crate::agent_home(&derived)?;
    // The session is machine-wide, so an agent home that has never been used still finds it.
    let token = login_cmd::access_token(&home).await.map_err(|_| {
        "not signed in on this machine — run `pigeonpost login` first, then this again"
    })?;

    let namespace = account_namespace(base_url, &token).await?;
    let handle = namespace.as_ref().map(|ns| format!("{ns}/{derived}"));
    let trust = namespace.as_ref().map(|ns| format!("{ns}/*"));

    println!("Setting this directory up as an agent.");
    println!();
    println!("  directory   {}", directory.display());
    println!("  agent name  {derived}");
    match (&handle, &trust) {
        (Some(handle), Some(trust)) => {
            println!("  handle      {handle}");
            println!("  trusts      {trust}");
        }
        _ => {
            // Not a failure. An anonymous mailbox works exactly as well; it just cannot be
            // addressed by a name, and handle-based trust will never match it.
            println!("  handle      none — this account has no namespace of its own yet");
            println!("  trusts      nobody, until you name one");
        }
    }
    println!();

    if dry_run {
        println!("--dry-run: nothing was changed.");
        return Ok(());
    }

    let workspace = workspace_cmd::Workspace {
        git_repo: workspace_cmd::git_remote(&directory),
        local_path: Some(directory.display().to_string()),
        ..Default::default()
    };

    postbox_cmd::onboard(
        &home,
        base_url,
        handle.as_deref(),
        Some(&derived),
        None,
        false,
        None, // trust is applied below, once the postbox has said which verbs it will allow
        &[],
        workspace,
        json,
        false,
    )
    .await?;

    if let Some(trust) = &trust {
        // Ask the postbox what it will let anyone grant, and grant all of it. The alternative is
        // this command carrying its own copy of a list that has already grown twice.
        let verbs = postbox_cmd::grantable_verbs(&home, None)
            .await
            .unwrap_or_default();
        if verbs.is_empty() {
            println!("could not read this postbox's verb vocabulary; the fleet is trusted but no request will run unattended until you grant one");
        }
        postbox_cmd::set_contact(
            &home,
            None,
            trust,
            Some("my fleet"),
            Some("allow"),
            Some("auto"),
            Some(&verbs),
            json,
        )
        .await?;
    }

    println!();
    ensure_daemon(&home)?;
    Ok(())
}

/// Install the daemon if this machine has none, then say what it is doing.
///
/// The command ends with the thing working rather than with instructions for making it work, which
/// is the whole difference between three commands and three commands plus a documentation page.
fn ensure_daemon(home: &Path) -> Result<(), Error> {
    if crate::agentd_cmd::installed_unit().is_none() {
        println!("Starting the daemon that turns incoming mail into a wake-up…");
        // The daemon is one per machine and holds the machine's own home, not this agent's: it
        // spools for every mailbox on the box and each agent drains its own.
        crate::agentd_cmd::install(&crate::default_home())?;
    }
    println!();
    crate::agentd_cmd::status(home, false)
}

/// The account's own namespace, or `None` when it has no named mailbox to derive one from.
async fn account_namespace(base_url: &str, token: &str) -> Result<Option<String>, Error> {
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()?;
    let identities: serde_json::Value = http
        .get(format!("{base_url}/v1/identities"))
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let addresses: Vec<String> = identities["identities"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| r["address"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    for address in addresses {
        let who: serde_json::Value = http
            .get(format!("{base_url}/v1/whoami"))
            .query(&[("identity", address.as_str())])
            .bearer_auth(token)
            .send()
            .await?
            .json()
            .await
            .unwrap_or_default();
        if let Some(handle) = who["handle"].as_str() {
            return Ok(namespace_of(handle));
        }
    }
    Ok(None)
}

/// Which namespace an agent of this account's would live under.
///
/// `/bekir/main` → `/bekir`: the first segment is the account's own namespace and its agents are
/// its siblings. `/github/alex` → `/github/alex`: `/github` belongs to everybody, so the person is
/// the whole handle and their agents are its children. Both are one person; the difference is only
/// which segment says so.
fn namespace_of(handle: &str) -> Option<String> {
    let parts: Vec<&str> = handle.trim_start_matches('/').split('/').collect();
    match parts.as_slice() {
        [ns, _rest, ..] if PROVIDER_NAMESPACES.contains(ns) => {
            Some(format!("/{}/{}", parts[0], parts[1]))
        }
        [ns, ..] if !ns.is_empty() => Some(format!("/{ns}")),
        _ => None,
    }
}

/// A directory name as an agent name: lowercase, and only what `--agent` already accepts.
fn sanitise(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches(['-', '.']).to_string();
    (!cleaned.is_empty() && cleaned.len() <= 64).then_some(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_account_namespace_holds_its_agents_as_siblings() {
        assert_eq!(namespace_of("/bekir/main").as_deref(), Some("/bekir"));
        assert_eq!(namespace_of("/bekir/docdex").as_deref(), Some("/bekir"));
    }

    #[test]
    fn a_provider_handle_is_the_person_and_holds_its_agents_as_children() {
        assert_eq!(
            namespace_of("/github/alex").as_deref(),
            Some("/github/alex")
        );
        assert_eq!(
            namespace_of("/google/alex").as_deref(),
            Some("/google/alex")
        );
    }

    #[test]
    fn a_directory_name_becomes_an_agent_name() {
        assert_eq!(sanitise("superproject").as_deref(), Some("superproject"));
        assert_eq!(sanitise("My Project!").as_deref(), Some("my-project"));
        assert_eq!(sanitise("  .hidden  ").as_deref(), Some("hidden"));
        assert_eq!(sanitise("///").as_deref(), None);
    }
}
