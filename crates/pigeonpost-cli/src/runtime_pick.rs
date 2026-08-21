//! Which agent runtime should answer this mailbox — detected, then asked about.
//!
//! `onboard agent` used to stop one step short of the thing working: it created the mailbox and
//! installed the daemon, and never wrote a route. A mailbox with no route receives mail and answers
//! none of it, silently, which is the failure the three-command onboarding exists to avoid.
//!
//! It also could not have written one usefully. The runtime default is a hard-coded `claude`, with
//! no check that `claude` is installed — so the honest options were to guess, or to ask. This asks,
//! and only about runtimes it can actually see on the PATH the daemon will run with.
//!
//! Non-interactive is a first-class path, not a fallback: a piped stdin, `--yes`, or an explicit
//! `--runtime` all skip the questions. An installer that blocks on a prompt nobody is there to
//! answer is worse than one that picks nothing.

use std::io::{IsTerminal, Write};
use std::path::Path;

type Error = Box<dyn std::error::Error>;

/// A runtime this machine could actually run, with the words to describe it to somebody.
pub struct Detected {
    /// The spelling `agentd answer --runtime` accepts, once any agent slug is appended.
    pub family: &'static str,
    pub label: &'static str,
    pub note: &'static str,
}

/// The three families, in the order they are offered. Detection is by program name on the PATH the
/// installed service runs with — the same lookup `agentd status` uses, so what is offered here and
/// what the daemon can later spawn cannot disagree.
pub fn detect(service_path: impl Fn(&str) -> bool) -> Vec<Detected> {
    let all = [
        Detected {
            family: "claude",
            label: "Claude Code",
            note: "runs here, with this machine's Claude sign-in",
        },
        Detected {
            family: "codex",
            label: "Codex",
            note: "runs here, sandboxed to the tier you choose",
        },
        Detected {
            family: "mcoda",
            label: "mcoda",
            note: "picks one of mcoda's agents — local, or a managed remote one",
        },
    ];
    all.into_iter().filter(|d| service_path(d.family)).collect()
}

/// One mcoda agent, reduced to what a person choosing between hundreds needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Agent {
    pub slug: String,
    pub adapter: String,
    pub cloud: bool,
    pub healthy: bool,
    pub rating: Option<f64>,
    pub cost_per_million: Option<f64>,
    pub best_usage: Option<String>,
}

/// Parse `mcoda agent list --json`.
///
/// Tolerant on purpose: mcoda owns this shape and has changed it before. A row missing everything
/// but a slug is still offerable, and a field that is not the type expected is treated as absent
/// rather than failing the whole list — the alternative is that one odd row costs somebody the
/// ability to choose any agent at all.
pub fn parse_agents(json: &str) -> Vec<Agent> {
    let value: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let rows = value
        .as_array()
        .cloned()
        .or_else(|| value.get("agents").and_then(|a| a.as_array()).cloned())
        .or_else(|| value.get("data").and_then(|a| a.as_array()).cloned())
        .unwrap_or_default();

    rows.into_iter()
        .filter_map(|row| {
            let slug = row.get("slug")?.as_str()?.trim().to_string();
            if slug.is_empty() {
                return None;
            }
            // A slug with a separator in it cannot be written into a route: the runtime spelling is
            // `mcoda:<slug>`, so a colon or a space would parse as something else entirely.
            if slug.contains([':', '/', '\\', ' ']) {
                return None;
            }
            let cloud = slug.starts_with(MCODA_CLOUD_PREFIX);
            let text = |key: &str| row.get(key).and_then(|v| v.as_str()).map(str::to_string);
            let number = |key: &str| row.get(key).and_then(|v| v.as_f64());
            Some(Agent {
                adapter: text("adapter").unwrap_or_else(|| "?".into()),
                healthy: match row.get("health").and_then(|h| h.as_str()) {
                    // Unknown is not unhealthy. mcoda leaves this empty until something has run,
                    // which is the normal state for an agent nobody has used yet.
                    Some(h) => {
                        !h.eq_ignore_ascii_case("unhealthy") && !h.eq_ignore_ascii_case("limited")
                    }
                    None => true,
                },
                rating: number("rating"),
                cost_per_million: number("costPerMillion").or_else(|| number("cost_per_million")),
                best_usage: text("bestUsage").or_else(|| text("best_usage")),
                cloud,
                slug,
            })
        })
        .collect()
}

const MCODA_CLOUD_PREFIX: &str = "mswarm-cloud-";

/// The agents worth putting in front of somebody, best first.
///
/// There are hundreds — 415 managed remote ones on the machine this was written against — so the
/// list is a shortlist, not an inventory. Unhealthy agents are dropped because choosing one is
/// choosing a mailbox that fails on its first request; the rest sort by rating, then by price,
/// because those are the two axes anybody actually decides on.
pub fn shortlist(agents: &[Agent], cloud: bool, limit: usize) -> Vec<Agent> {
    let mut rows: Vec<Agent> = agents
        .iter()
        .filter(|a| a.cloud == cloud && a.healthy)
        .cloned()
        .collect();
    rows.sort_by(|a, b| {
        b.rating
            .unwrap_or(0.0)
            .partial_cmp(&a.rating.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.cost_per_million
                    .unwrap_or(0.0)
                    .partial_cmp(&b.cost_per_million.unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.slug.cmp(&b.slug))
    });
    rows.truncate(limit);
    rows
}

/// Ask, when there is somebody to ask.
///
/// Returns `None` whenever the answer would have to be invented: no terminal, or an empty list.
pub fn choose(prompt: &str, options: &[String], default: usize) -> Option<usize> {
    if options.is_empty() || !std::io::stdin().is_terminal() {
        return None;
    }
    println!();
    println!("{prompt}");
    for (i, option) in options.iter().enumerate() {
        let marker = if i == default { "*" } else { " " };
        println!(" {marker} {:>2}. {option}", i + 1);
    }
    print!("Choose [1-{}] (enter for {}): ", options.len(), default + 1);
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return None;
    }
    let line = line.trim();
    if line.is_empty() {
        return Some(default);
    }
    match line.parse::<usize>() {
        Ok(n) if n >= 1 && n <= options.len() => Some(n - 1),
        // A typo is not consent to the default: it is worth one more look rather than silently
        // configuring something nobody chose.
        _ => {
            println!("`{line}` is not one of those.");
            choose(prompt, options, default)
        }
    }
}

/// Run `mcoda agent list --json` and read it. Failure is empty rather than fatal: mcoda being
/// present on the PATH does not mean it is configured, and an unconfigured mcoda should cost the
/// person a menu, not the whole onboarding.
pub fn mcoda_agents(program: &Path) -> Vec<Agent> {
    let output = std::process::Command::new(program)
        .args(["agent", "list", "--json"])
        .output();
    match output {
        Ok(out) if out.status.success() => {
            parse_agents(&String::from_utf8_lossy(out.stdout.as_slice()))
        }
        _ => Vec::new(),
    }
}

/// How the shortlist reads on one line.
pub fn describe(agent: &Agent) -> String {
    let mut parts = vec![agent.slug.clone()];
    let mut detail: Vec<String> = Vec::new();
    if let Some(usage) = &agent.best_usage {
        detail.push(usage.clone());
    }
    if let Some(rating) = agent.rating {
        detail.push(format!("rated {rating:.1}"));
    }
    match agent.cost_per_million {
        Some(cost) if cost > 0.0 => detail.push(format!("${cost:.2}/M")),
        Some(_) => detail.push("free".into()),
        None => {}
    }
    detail.push(agent.adapter.clone());
    if !detail.is_empty() {
        parts.push(format!("({})", detail.join(", ")));
    }
    parts.join(" ")
}

/// Confirm a tier in words rather than in jargon. Defaults to the safe one.
pub fn choose_permission() -> crate::executor::Permission {
    use crate::executor::Permission;
    let options = vec![
        "Report only — read the project and answer questions. Changes nothing.".to_string(),
        "Work in this checkout — edit files, run the project's tests, commit locally.".to_string(),
        "Anything, including publishing — push, release, deploy.".to_string(),
    ];
    match choose("How much may an answer do?", &options, 0) {
        Some(1) => Permission::Workspace,
        Some(2) => Permission::Full,
        _ => Permission::ReadOnly,
    }
}

/// Say what could not be offered, and why, so "it did not ask me about X" has an answer.
pub fn report_missing(found: &[Detected]) -> Option<String> {
    let names: Vec<&str> = ["claude", "codex", "mcoda"]
        .into_iter()
        .filter(|n| !found.iter().any(|d| d.family == *n))
        .collect();
    if names.is_empty() {
        return None;
    }
    Some(format!(
        "not offered because they are not on this machine's PATH: {}",
        names.join(", ")
    ))
}

pub fn error_none_installed() -> Error {
    "no agent runtime is installed on this machine, so nothing can answer unattended yet.\n\
     Install one of `claude`, `codex` or `mcoda`, then run: pigeonpost agentd answer --verb report_status"
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(rows: &str) -> String {
        format!("[{rows}]")
    }

    #[test]
    fn detection_offers_only_what_is_on_the_path() {
        let found = detect(|p| p == "codex");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].family, "codex");

        assert!(detect(|_| false).is_empty());
        assert_eq!(detect(|_| true).len(), 3);
    }

    /// The order is the order somebody is asked in, so it is worth pinning.
    #[test]
    fn the_three_families_are_offered_in_a_fixed_order() {
        let all: Vec<&str> = detect(|_| true).into_iter().map(|d| d.family).collect();
        assert_eq!(all, vec!["claude", "codex", "mcoda"]);
    }

    #[test]
    fn report_missing_names_what_was_not_offered() {
        let found = detect(|p| p == "claude");
        let missing = report_missing(&found).unwrap();
        assert!(missing.contains("codex"));
        assert!(missing.contains("mcoda"));
        assert!(!missing.contains("claude"));
        assert!(report_missing(&detect(|_| true)).is_none());
    }

    #[test]
    fn cloud_agents_are_told_apart_by_their_slug() {
        let agents = parse_agents(&json(
            r#"{"slug":"claude-sonnet","adapter":"claude-cli"},
               {"slug":"mswarm-cloud-openrouter-x","adapter":"openai-api"}"#,
        ));
        assert_eq!(agents.len(), 2);
        assert!(!agents[0].cloud);
        assert!(agents[1].cloud);
    }

    /// A slug goes straight into `mcoda:<slug>`, so one carrying a separator would parse as
    /// something else — including, for a colon, a different runtime family.
    #[test]
    fn a_slug_that_could_not_be_written_into_a_route_is_dropped() {
        let agents = parse_agents(&json(
            r#"{"slug":"fine"},{"slug":"has:colon"},{"slug":"has space"},{"slug":"has/slash"},{"slug":""}"#,
        ));
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].slug, "fine");
    }

    #[test]
    fn a_row_missing_everything_but_a_slug_is_still_offerable() {
        let agents = parse_agents(&json(r#"{"slug":"bare"}"#));
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].adapter, "?");
        // Unknown health is not unhealthy: that is the normal state for an agent nobody has run.
        assert!(agents[0].healthy);
    }

    #[test]
    fn unhealthy_and_usage_limited_agents_are_not_offered() {
        let agents = parse_agents(&json(
            r#"{"slug":"ok","health":"healthy"},
               {"slug":"sick","health":"unhealthy"},
               {"slug":"spent","health":"limited"}"#,
        ));
        let offered = shortlist(&agents, false, 10);
        assert_eq!(offered.len(), 1);
        assert_eq!(offered[0].slug, "ok");
    }

    #[test]
    fn the_shortlist_is_best_first_and_bounded() {
        let agents = parse_agents(&json(
            r#"{"slug":"mid","rating":5.0},{"slug":"best","rating":9.0},{"slug":"worst","rating":1.0}"#,
        ));
        let rows = shortlist(&agents, false, 2);
        assert_eq!(rows.len(), 2, "the cap is what makes 400 agents choosable");
        assert_eq!(rows[0].slug, "best");
        assert_eq!(rows[1].slug, "mid");
    }

    /// Equal ratings break on price, because that is the other axis anybody decides on.
    #[test]
    fn equally_rated_agents_sort_cheapest_first() {
        let agents = parse_agents(&json(
            r#"{"slug":"dear","rating":5.0,"costPerMillion":40.0},
               {"slug":"cheap","rating":5.0,"costPerMillion":2.0}"#,
        ));
        assert_eq!(shortlist(&agents, false, 5)[0].slug, "cheap");
    }

    #[test]
    fn local_and_cloud_are_separate_lists() {
        let agents = parse_agents(&json(r#"{"slug":"here"},{"slug":"mswarm-cloud-there"}"#));
        assert_eq!(shortlist(&agents, false, 5)[0].slug, "here");
        assert_eq!(shortlist(&agents, true, 5)[0].slug, "mswarm-cloud-there");
    }

    #[test]
    fn a_row_reads_as_a_choice_rather_than_a_slug() {
        let agents = parse_agents(&json(
            r#"{"slug":"claude-sonnet","adapter":"claude-cli","rating":8.5,
                "costPerMillion":3.0,"bestUsage":"code_writer"}"#,
        ));
        let line = describe(&agents[0]);
        assert!(line.contains("claude-sonnet"));
        assert!(line.contains("code_writer"));
        assert!(line.contains("8.5"));
        assert!(line.contains("$3.00/M"));
    }

    #[test]
    fn a_free_agent_says_so_rather_than_showing_zero() {
        let agents = parse_agents(&json(r#"{"slug":"local","costPerMillion":0}"#));
        assert!(describe(&agents[0]).contains("free"));
    }

    /// mcoda has changed this shape before, and the whole menu should not be lost to it.
    #[test]
    fn an_unreadable_listing_is_empty_rather_than_fatal() {
        assert!(parse_agents("not json").is_empty());
        assert!(parse_agents("{}").is_empty());
        assert!(parse_agents("[]").is_empty());
        // Both wrappers mcoda has used, plus the bare array.
        assert_eq!(parse_agents(r#"{"agents":[{"slug":"a"}]}"#).len(), 1);
        assert_eq!(parse_agents(r#"{"data":[{"slug":"b"}]}"#).len(), 1);
    }
}
