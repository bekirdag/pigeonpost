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
//! pigeonpost agentd answer --runtime <something> --verb … --permission <tier>
//! pigeonpost agentd install
//! ```
//!
//! Nobody types that. Every value in it is derivable from where the command was run and who is
//! signed in, so this derives them and says what it decided.
//!
//! Every value except three. Which model reads a repository unattended, how much it may do, and
//! whether a second model should read the work before the reply is sent, are not derivable from
//! anything — so they are the only things this asks about, and it asks only about runtimes it can
//! see on the PATH the daemon will run with. `--runtime` skips the questions entirely, which is
//! how a script runs this; so does a stdin that is not a terminal, and both leave a single-agent
//! mailbox.
//!
//! The middle line above is the one that used to be missing. Without it the mailbox is created,
//! the daemon is installed, and nothing answers — which reads as the daemon being broken rather
//! than as a step that was never taken.

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
    runtime: Option<String>,
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
    // The step this command used to leave out. Without a route the mailbox receives mail and
    // answers none of it, which looks exactly like the daemon being broken.
    route_this_mailbox(&home, runtime.as_deref())?;
    ensure_daemon(&home)?;
    Ok(())
}

/// Pick a runtime and write the route, or say why it could not.
///
/// Never fatal. The mailbox is already made by the time this runs, and it is a working mailbox
/// whether or not anything answers automatically — failing here would undo nothing and would
/// report a finished setup as broken.
fn route_this_mailbox(home: &Path, requested: Option<&str>) -> Result<(), Error> {
    use crate::runtime_pick;

    let machine = crate::agentd_cmd::machine_home_of(home);

    // Given explicitly, so nothing is asked. This is the path a script takes.
    if let Some(runtime) = requested {
        return write_route(
            &machine,
            runtime,
            crate::executor::Permission::ReadOnly,
            &[],
        );
    }

    let found = runtime_pick::detect(|program| {
        crate::agentd_cmd::installed_service_path(program).is_some()
    });
    if found.is_empty() {
        println!("{}", runtime_pick::error_none_installed());
        return Ok(());
    }

    let labels: Vec<String> = found
        .iter()
        .map(|d| format!("{:<12} {}", d.label, d.note))
        .collect();
    let Some(picked) =
        runtime_pick::choose("What should answer requests to this mailbox?", &labels, 0)
    else {
        // No terminal. Say what would have been asked rather than choosing on somebody's behalf:
        // which model reads a repository unattended is not a decision to make by default.
        println!(
            "not routed for unattended answering — this is not an interactive terminal.\n\
             When you are ready: pigeonpost agentd answer --runtime {} --verb report_status",
            found[0].family
        );
        return Ok(());
    };
    if let Some(missing) = runtime_pick::report_missing(&found) {
        println!("({missing})");
    }

    let runtime = match found[picked].family {
        "mcoda" => match pick_mcoda_agent()? {
            Some(spelling) => spelling,
            None => return Ok(()),
        },
        family => family.to_string(),
    };
    let permission = runtime_pick::choose_permission();
    let reviewers = choose_reviewers(&found, found[picked].family)?;
    write_route(&machine, &runtime, permission, &reviewers)
}

/// Ask whether a second agent should read the work, when this machine has a second agent to offer.
///
/// The third and last question. It is asked only when more than one runtime was detected — one
/// runtime has nothing to review with — and `choose` itself returns `None` when there is nobody to
/// ask, so every non-interactive path still produces a single-agent mailbox exactly as before.
fn choose_reviewers(
    found: &[crate::runtime_pick::Detected],
    main: &str,
) -> Result<Vec<String>, Error> {
    use crate::runtime_pick;

    let others: Vec<&str> = found
        .iter()
        .map(|d| d.family)
        .filter(|family| *family != main)
        .collect();
    if others.is_empty() {
        return Ok(Vec::new());
    }

    // Built alongside the words, so the option somebody reads and the runtimes they get cannot
    // drift apart.
    let mut options = vec!["No — one agent answers. Fastest and cheapest.".to_string()];
    let mut picks: Vec<Vec<&str>> = vec![Vec::new()];
    for other in &others {
        options.push(format!(
            "{other} reviews {main}'s work, then {main} reworks it. Roughly 3× the cost."
        ));
        picks.push(vec![*other]);
    }
    if others.len() > 1 {
        options.push(format!(
            "{} both review. Roughly {}× the cost.",
            others.join(" and "),
            2 + others.len()
        ));
        picks.push(others.clone());
    }

    let Some(chosen) = runtime_pick::choose(
        "Have another agent review the work before it is sent?",
        &options,
        0,
    ) else {
        return Ok(Vec::new());
    };

    let mut spellings = Vec::new();
    for family in &picks[chosen] {
        match *family {
            // The same sub-menu the main runtime gets: a route can only name a pinned slug, never
            // the family, so there is nothing to write until one is chosen.
            "mcoda" => {
                if let Some(spelling) = pick_mcoda_agent()? {
                    spellings.push(spelling);
                }
            }
            family => spellings.push(family.to_string()),
        }
    }
    Ok(spellings)
}

/// Local or managed-remote first, then which agent — because the two lists have nothing in common
/// and one of them is four hundred rows long.
fn pick_mcoda_agent() -> Result<Option<String>, Error> {
    use crate::runtime_pick;

    let Some(program) = crate::agentd_cmd::installed_service_path("mcoda") else {
        println!("mcoda went missing from the PATH between being offered and being chosen.");
        return Ok(None);
    };
    let agents = runtime_pick::mcoda_agents(&program);
    if agents.is_empty() {
        println!(
            "mcoda is installed but listed no agents. Check `mcoda agent list`, then: \n  \
             pigeonpost agentd answer --runtime mcoda:<slug> --verb report_status"
        );
        return Ok(None);
    }

    let where_it_runs = vec![
        "On this machine — the text never leaves it.".to_string(),
        "A managed remote agent — the request is sent to somebody else's machine to run."
            .to_string(),
    ];
    let Some(choice) = runtime_pick::choose("Where should it run?", &where_it_runs, 0) else {
        return Ok(None);
    };
    let cloud = choice == 1;

    // A shortlist, not an inventory: hundreds exist and nobody reads past the first screen.
    let rows = runtime_pick::shortlist(&agents, cloud, 15);
    if rows.is_empty() {
        println!(
            "mcoda has no healthy {} agents configured.",
            if cloud { "remote" } else { "local" }
        );
        return Ok(None);
    }
    let labels: Vec<String> = rows.iter().map(runtime_pick::describe).collect();
    let Some(picked) = runtime_pick::choose("Which agent?", &labels, 0) else {
        return Ok(None);
    };
    // The two spellings are deliberately different so a remote agent can never be reached by a
    // local-looking config line drifting.
    let prefix = if cloud { "mcoda-cloud" } else { "mcoda" };
    Ok(Some(format!("{prefix}:{}", rows[picked].slug)))
}

fn write_route(
    machine: &Path,
    runtime: &str,
    permission: crate::executor::Permission,
    reviewers: &[String],
) -> Result<(), Error> {
    // Every verb the postbox will grant, so the route matches the trust that was just set up.
    // The permission tier is the second key and is chosen separately — a verb being answerable
    // here still does not mean this machine will do it.
    let verbs: Vec<String> = crate::executor::RUNNABLE_VERBS
        .iter()
        .filter(|v| permission.admits(v))
        .map(|v| v.to_string())
        .collect();
    // `*` rather than a branch name. A route pinned to one branch cannot do ordinary git work —
    // no feature branch, no tag — and pinning is not what bounds an agent anyway: the tier is.
    // Whoever wants a narrower one can say so with `agentd answer --branch <name>`.
    let branches = if permission == crate::executor::Permission::Full {
        vec!["*".to_string()]
    } else {
        Vec::new()
    };
    crate::agentd_cmd::answer(
        machine,
        &crate::agentd_cmd::Answer {
            verbs: &verbs,
            runtime,
            timeout_secs: None,
            permission,
            branches: &branches,
            daily_runs: None,
            reviewers,
            // Every panel default, because onboarding asks one question about this and writing
            // four answers from it would be inventing three of them.
            panel_rounds: None,
            panel_verbs: &[],
            panel_permission: None,
            panel_on_failure: None,
            install: true,
            off: false,
        },
    )
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
        // An address is a person on its own, so agents hang directly beneath it.
        [address, ..] if address.contains('@') => Some(format!("/{address}")),
        [ns, _name, ..] if PROVIDER_NAMESPACES.contains(ns) => {
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
    fn an_address_is_the_person_and_holds_its_agents_directly() {
        assert_eq!(
            namespace_of("/alex@gmail.com").as_deref(),
            Some("/alex@gmail.com")
        );
        assert_eq!(
            namespace_of("/alex@gmail.com/agent2").as_deref(),
            Some("/alex@gmail.com")
        );
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
