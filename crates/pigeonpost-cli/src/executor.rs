//! Deciding whether a message may be acted on unattended, and what that action is.
//!
//! This module is the rails, not the engine. Without them the feature is a remote code execution
//! service with a friendly name: the input is another agent's text, the trigger is automatic, and
//! the thing on the end holds tools. So every gate here is a refusal by default, and the checks are
//! ordered cheapest-and-most-decisive first.
//!
//! Three properties are worth stating because they are what make the rest defensible:
//!
//! * **The verb selects the action; the body never does.** A body is attacker-influenceable text.
//!   It supplies arguments that are validated against a schema, and a `note` that is carried as
//!   explicitly-labelled untrusted context — never as instruction.
//! * **Routing is local.** A mailbox that is not named in this machine's own config executes
//!   nothing, whatever the network says about it. Nothing remote can add a mailbox or point one at
//!   a different directory.
//! * **The server's verdict is obeyed, not recomputed.** The postbox decides `auto` vs `review`.
//!   Re-deriving it here would mean two implementations that can disagree, and the disagreement
//!   would be silent.

use std::path::{Path, PathBuf};

type Error = Box<dyn std::error::Error>;

/// Phase 4 admits only the two verbs that cannot change anything. `read_file` and `run_tests` reach
/// the filesystem and wait for Phase 5, where the sandboxing has to be real.
pub const PHASE4_VERBS: &[&str] = &["report_status", "answer_question"];

/// Marks a reply this executor generated. An auto-reply must never itself be auto-acted on, or two
/// agents answering each other run until something breaks — the failure most likely to happen by
/// accident rather than by malice.
///
/// Forging this field can only ever *reduce* automation, which is the safe direction for a flag
/// carried in attacker-influenceable text.
pub const AUTO_REPLY_MARKER: &str = "auto_reply";

const CONFIG_FILE: &str = "agentd.toml";
const PAUSE_FILE: &str = "agentd-paused";
const AUDIT_FILE: &str = "agentd-audit.jsonl";

/// One mailbox this machine will act for. Absence is a refusal: there is no default routing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct MailboxRoute {
    /// The mailbox, by handle or `/k/` address, exactly as the postbox reports it.
    pub address: String,
    /// Where the work happens. Every action runs with this as its working directory.
    pub workspace: PathBuf,
    /// Which agent runtime to hand the request to.
    #[serde(default = "default_runtime")]
    pub runtime: String,
    /// Verbs this mailbox may act on unattended. Opt-in per mailbox: a grant on the postbox says
    /// the *sender* may ask, and this says this machine is willing to answer.
    #[serde(default)]
    pub verbs: Vec<String>,
    /// Wall-clock ceiling for one action.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_runtime() -> String {
    "claude".into()
}
fn default_timeout_secs() -> u64 {
    120
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct RoutingConfig {
    #[serde(default)]
    pub mailbox: Vec<MailboxRoute>,
    /// Ceiling on actions running at once across every mailbox, so one flood cannot fork a
    /// hundred agent processes.
    #[serde(default = "default_concurrency")]
    pub max_concurrent: usize,
    /// Master switch. Absent or false means the rails exist but nothing is executed — which is the
    /// state this ships in, so enabling unattended execution is a deliberate edit.
    #[serde(default)]
    pub execute: bool,
}

fn default_concurrency() -> usize {
    2
}

pub fn config_path(home: &Path) -> PathBuf {
    home.join(CONFIG_FILE)
}

pub fn load_routing(home: &Path) -> Result<RoutingConfig, Error> {
    match std::fs::read_to_string(config_path(home)) {
        Ok(body) => Ok(toml::from_str(&body)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RoutingConfig::default()),
        Err(e) => Err(e.into()),
    }
}

/// Why a message will not be acted on. Every variant is a refusal; there is no "unknown" that
/// falls through to running something.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Unattended execution is switched off for this machine.
    ExecutionDisabled,
    /// `agentd pause` is in force.
    Paused,
    /// No route for this mailbox in this machine's config.
    NoRoute,
    /// The postbox held it for a human.
    NotAuto,
    /// The postbox stamped it `auto`, but this machine has not opted this mailbox into that verb.
    VerbNotEnabledHere,
    /// Beyond what this phase is willing to run at all.
    VerbNotInPhase,
    /// A reply this executor generated. Acting on it would close a loop.
    AutoReply,
    /// The arguments did not match the verb's schema.
    BadArguments(&'static str),
    /// The workspace named in config is not a directory that exists.
    WorkspaceMissing,
}

impl Refusal {
    pub fn as_str(&self) -> &'static str {
        match self {
            Refusal::ExecutionDisabled => "execution_disabled",
            Refusal::Paused => "paused",
            Refusal::NoRoute => "no_route",
            Refusal::NotAuto => "not_auto",
            Refusal::VerbNotEnabledHere => "verb_not_enabled_here",
            Refusal::VerbNotInPhase => "verb_not_in_phase",
            Refusal::AutoReply => "auto_reply",
            Refusal::BadArguments(_) => "bad_arguments",
            Refusal::WorkspaceMissing => "workspace_missing",
        }
    }
}

/// What a message would cause, if anything. Deliberately a pure function of already-fetched data:
/// it touches no network and spawns nothing, so it can be exercised exhaustively in tests.
#[derive(Debug, PartialEq, Eq)]
pub struct Action {
    pub route: MailboxRoute,
    pub verb: String,
    /// The sender's prose, carried through as untrusted context. Never interpreted here.
    pub note: Option<String>,
}

pub fn paused(home: &Path) -> bool {
    home.join(PAUSE_FILE).exists()
}

/// Decide whether one inbox message may be acted on.
///
/// `message` is the postbox's own JSON for that message. The order matters: the machine-wide
/// switches come first so a paused or disabled machine does no parsing at all, then routing, then
/// the server's verdict, then the body.
pub fn classify(
    config: &RoutingConfig,
    home: &Path,
    mailbox: &str,
    message: &serde_json::Value,
) -> Result<Action, Refusal> {
    if !config.execute {
        return Err(Refusal::ExecutionDisabled);
    }
    if paused(home) {
        return Err(Refusal::Paused);
    }

    let route = config
        .mailbox
        .iter()
        .find(|m| m.address == mailbox)
        .cloned()
        .ok_or(Refusal::NoRoute)?;

    // The postbox's verdict, obeyed rather than recomputed.
    if message.get("autonomy").and_then(|v| v.as_str()) != Some("auto") {
        return Err(Refusal::NotAuto);
    }
    let verb = message
        .get("verb")
        .and_then(|v| v.as_str())
        .ok_or(Refusal::NotAuto)?
        .to_string();

    if !PHASE4_VERBS.contains(&verb.as_str()) {
        return Err(Refusal::VerbNotInPhase);
    }
    if !route.verbs.iter().any(|v| v == &verb) {
        return Err(Refusal::VerbNotEnabledHere);
    }

    // The body is parsed only after every cheaper gate has passed.
    let body: serde_json::Value = message
        .get("untrusted_body")
        .or_else(|| message.get("body"))
        .and_then(|b| b.as_str())
        .and_then(|s| serde_json::from_str(s).ok())
        .ok_or(Refusal::BadArguments("body is not a request envelope"))?;

    if body.get(AUTO_REPLY_MARKER).and_then(|v| v.as_bool()) == Some(true) {
        return Err(Refusal::AutoReply);
    }

    validate_args(&verb, body.get("args"))?;

    if !route.workspace.is_dir() {
        return Err(Refusal::WorkspaceMissing);
    }

    Ok(Action {
        route,
        verb,
        note: body
            .get("note")
            .and_then(|n| n.as_str())
            .map(str::to_string),
    })
}

/// Per-verb argument schemas. "The verb is grantable" is not enough on its own: a granted verb
/// carrying attacker-chosen arguments is how a bounded request becomes an arbitrary one.
///
/// Both Phase 4 verbs are questions, so neither takes a path, a command, or anything else that
/// selects work. Anything unexpected is refused rather than ignored.
fn validate_args(verb: &str, args: Option<&serde_json::Value>) -> Result<(), Refusal> {
    let args = match args {
        None | Some(serde_json::Value::Null) => return Ok(()),
        Some(serde_json::Value::Object(map)) => map,
        Some(_) => return Err(Refusal::BadArguments("args must be an object")),
    };
    match verb {
        "report_status" => {
            if !args.is_empty() {
                return Err(Refusal::BadArguments("report_status takes no arguments"));
            }
        }
        "answer_question" => {
            for key in args.keys() {
                if key != "question" {
                    return Err(Refusal::BadArguments(
                        "answer_question takes only 'question'",
                    ));
                }
            }
            if let Some(q) = args.get("question") {
                match q.as_str() {
                    Some(text) if text.len() <= 4096 => {}
                    Some(_) => return Err(Refusal::BadArguments("question is too long")),
                    None => return Err(Refusal::BadArguments("question must be a string")),
                }
            }
        }
        _ => return Err(Refusal::VerbNotInPhase),
    }
    Ok(())
}

/// Append one line per decision, whether it ran or not. A refusal is as much worth recording as an
/// execution: "why did nothing happen" is the question this answers most often.
pub fn audit(
    home: &Path,
    mailbox: &str,
    message_id: &str,
    outcome: &str,
    detail: Option<&str>,
) -> Result<(), Error> {
    use std::io::Write;
    let record = serde_json::json!({
        "at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        "mailbox": mailbox,
        "message_id": message_id,
        "outcome": outcome,
        "detail": detail,
    });
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.join(AUDIT_FILE))?;
    writeln!(file, "{record}")?;
    Ok(())
}

pub fn audit_path(home: &Path) -> PathBuf {
    home.join(AUDIT_FILE)
}

pub fn pause_path(home: &Path) -> PathBuf {
    home.join(PAUSE_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(verbs: &[&str], workspace: &Path) -> MailboxRoute {
        MailboxRoute {
            address: "/bekir/agent1".into(),
            workspace: workspace.to_path_buf(),
            runtime: "claude".into(),
            verbs: verbs.iter().map(|v| v.to_string()).collect(),
            timeout_secs: 60,
        }
    }

    fn config(verbs: &[&str], workspace: &Path) -> RoutingConfig {
        RoutingConfig {
            mailbox: vec![route(verbs, workspace)],
            max_concurrent: 2,
            execute: true,
        }
    }

    fn message(autonomy: &str, verb: &str, body: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "autonomy": autonomy,
            "verb": verb,
            "untrusted_body": body.to_string(),
        })
    }

    fn envelope(verb: &str) -> serde_json::Value {
        serde_json::json!({ "v": 1, "verb": verb, "args": {}, "note": "because" })
    }

    #[test]
    fn a_granted_read_only_verb_is_actionable() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(&["report_status"], dir.path());
        let action = classify(
            &cfg,
            dir.path(),
            "/bekir/agent1",
            &message("auto", "report_status", envelope("report_status")),
        )
        .expect("actionable");
        assert_eq!(action.verb, "report_status");
        assert_eq!(action.note.as_deref(), Some("because"));
    }

    /// Ships inert: the rails exist, and turning them on is a deliberate edit.
    #[test]
    fn execution_is_off_until_someone_turns_it_on() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(&["report_status"], dir.path());
        cfg.execute = false;
        assert_eq!(
            classify(
                &cfg,
                dir.path(),
                "/bekir/agent1",
                &message("auto", "report_status", envelope("report_status"))
            ),
            Err(Refusal::ExecutionDisabled)
        );
    }

    /// The server decides; this obeys. A message it held is never second-guessed into running.
    #[test]
    fn a_message_held_for_review_is_never_acted_on() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(&["report_status"], dir.path());
        assert_eq!(
            classify(
                &cfg,
                dir.path(),
                "/bekir/agent1",
                &message("review", "report_status", envelope("report_status"))
            ),
            Err(Refusal::NotAuto)
        );
    }

    /// A mailbox this machine was never told about executes nothing, whatever the postbox says.
    #[test]
    fn an_unrouted_mailbox_does_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(&["report_status"], dir.path());
        assert_eq!(
            classify(
                &cfg,
                dir.path(),
                "/bekir/somebody-else",
                &message("auto", "report_status", envelope("report_status"))
            ),
            Err(Refusal::NoRoute)
        );
    }

    /// A grant on the postbox says the sender may ask. This says the machine is willing to answer.
    #[test]
    fn a_verb_granted_remotely_still_needs_local_opt_in() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(&["report_status"], dir.path());
        assert_eq!(
            classify(
                &cfg,
                dir.path(),
                "/bekir/agent1",
                &message("auto", "answer_question", envelope("answer_question"))
            ),
            Err(Refusal::VerbNotEnabledHere)
        );
    }

    /// Even a server mistake cannot get a filesystem verb run in this phase.
    #[test]
    fn a_verb_outside_this_phase_is_refused_even_if_stamped_auto() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(&["run_tests"], dir.path());
        cfg.mailbox[0].verbs.push("run_tests".into());
        assert_eq!(
            classify(
                &cfg,
                dir.path(),
                "/bekir/agent1",
                &message("auto", "run_tests", envelope("run_tests"))
            ),
            Err(Refusal::VerbNotInPhase)
        );
    }

    /// The failure most likely to happen by accident: two agents answering each other forever.
    #[test]
    fn an_auto_reply_is_never_auto_acted_on() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(&["report_status"], dir.path());
        let mut body = envelope("report_status");
        body["auto_reply"] = serde_json::json!(true);
        assert_eq!(
            classify(
                &cfg,
                dir.path(),
                "/bekir/agent1",
                &message("auto", "report_status", body)
            ),
            Err(Refusal::AutoReply)
        );
    }

    /// A granted verb carrying attacker-chosen arguments is how a bounded request becomes an
    /// arbitrary one, so unexpected arguments are refused rather than ignored.
    #[test]
    fn unexpected_arguments_are_refused_not_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(&["report_status"], dir.path());
        let mut body = envelope("report_status");
        body["args"] = serde_json::json!({ "path": "/etc/passwd" });
        assert!(matches!(
            classify(
                &cfg,
                dir.path(),
                "/bekir/agent1",
                &message("auto", "report_status", body)
            ),
            Err(Refusal::BadArguments(_))
        ));

        let cfg = config(&["answer_question"], dir.path());
        let mut body = envelope("answer_question");
        body["args"] = serde_json::json!({ "question": "ok", "command": "rm -rf /" });
        assert!(matches!(
            classify(
                &cfg,
                dir.path(),
                "/bekir/agent1",
                &message("auto", "answer_question", body)
            ),
            Err(Refusal::BadArguments(_))
        ));
    }

    #[test]
    fn the_pause_switch_stops_everything() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(&["report_status"], dir.path());
        std::fs::write(pause_path(dir.path()), "").unwrap();
        assert_eq!(
            classify(
                &cfg,
                dir.path(),
                "/bekir/agent1",
                &message("auto", "report_status", envelope("report_status"))
            ),
            Err(Refusal::Paused)
        );
    }

    /// A route pointing somewhere that no longer exists must refuse rather than run in whatever
    /// directory the process happens to be in.
    #[test]
    fn a_missing_workspace_refuses_rather_than_running_somewhere_else() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(&["report_status"], dir.path());
        cfg.mailbox[0].workspace = dir.path().join("gone");
        assert_eq!(
            classify(
                &cfg,
                dir.path(),
                "/bekir/agent1",
                &message("auto", "report_status", envelope("report_status"))
            ),
            Err(Refusal::WorkspaceMissing)
        );
    }

    #[test]
    fn a_body_that_is_not_an_envelope_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(&["report_status"], dir.path());
        let msg = serde_json::json!({
            "autonomy": "auto", "verb": "report_status",
            "untrusted_body": "just some prose",
        });
        assert!(matches!(
            classify(&cfg, dir.path(), "/bekir/agent1", &msg),
            Err(Refusal::BadArguments(_))
        ));
    }
}
