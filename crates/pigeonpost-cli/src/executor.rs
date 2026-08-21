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

/// Every verb this daemon knows how to carry out.
///
/// Whether a given route may answer one is a second question, asked of [`Permission::admits`]:
/// being runnable is about this build, being admitted is about this machine. `read_file` is
/// deliberately absent — `full` supersedes it, and a path-confined reader is a different feature
/// with a different threat model.
pub const RUNNABLE_VERBS: &[&str] = &[
    "report_status",
    "answer_question",
    "run_tests",
    "make_change",
    "full_access",
    "git_push",
    "deploy",
];

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
const SPEND_FILE: &str = "agentd-spend.json";

/// One mailbox this machine will act for. Absence is a refusal: there is no default routing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
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
    /// How much an answer may do. Defaults to the tier that shipped.
    #[serde(default)]
    pub permission: Permission,
    /// Branches `git_push` and `deploy` may name, and the branches and tags an answer may create
    /// or move.
    ///
    /// Absent still means neither may touch anything: a deploy with no stated target is the one
    /// request this cannot bound, and defaulting that to "anything" would be a convenience nobody
    /// asked for.
    ///
    /// `"*"` means any of them, and is the setting most agents actually want. Ordinary work is
    /// branches and tags — a fix on a feature branch, a release tag, a push of both — and a route
    /// pinned to one branch name refuses all of it. That is not a safety boundary so much as a
    /// spelling of one: an agent that may rewrite `main` is not made safer by being unable to
    /// create `fix/thing`. What actually bounds it is the permission tier, the checkout it works
    /// in, and the refusal to force-push or rewrite history — none of which `*` touches.
    #[serde(default)]
    pub branches: Vec<String>,
    /// Ceiling on runs accepted from one sender in a day. Zero means no ceiling.
    ///
    /// A granted peer can otherwise trigger unbounded work: at `read-only` that costs a status
    /// report, at `full` it costs an implementation. `max_concurrent` bounds how many run at once,
    /// which is not the same question.
    #[serde(default = "default_daily_runs")]
    pub daily_runs_per_sender: u32,
}

fn default_daily_runs() -> u32 {
    50
}

fn default_runtime() -> String {
    "claude".into()
}
/// Ten minutes. A `report_status` worth sending goes and looks: queries a live API, reads logs,
/// counts what it found. Two minutes killed exactly those runs mid-work, which is worse than
/// slow — the peer gets a failure where an accurate answer was already half-assembled.
pub const DEFAULT_TIMEOUT_SECS: u64 = 600;

fn default_timeout_secs() -> u64 {
    DEFAULT_TIMEOUT_SECS
}

/// How much the runtime is allowed to do once it is running.
///
/// The verb says what was asked for; this says what the machine is willing to let any answer do.
/// They are separate on purpose: granting `run_tests` should not imply a runtime that may also
/// push, and raising the tier should not silently widen which verbs are answerable.
///
/// `ReadOnly` is the default and is what shipped, so an upgrade changes nothing for anyone. Every
/// step above it is a local edit on the machine that will do the work — nothing reachable from the
/// network can raise it.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Deserialize,
    serde::Serialize,
    clap::ValueEnum,
)]
#[serde(rename_all = "kebab-case")]
#[clap(rename_all = "kebab-case")]
pub enum Permission {
    /// Read and report. Tool calls that would need approval are refused by the runtime itself,
    /// because nobody is present to approve them.
    #[default]
    ReadOnly,
    /// Change files, run the project's own code, commit locally. Nothing leaves the machine.
    Workspace,
    /// Anything the machine's user can do, including pushing and deploying.
    Full,
}

impl Permission {
    pub fn as_str(&self) -> &'static str {
        match self {
            Permission::ReadOnly => "read-only",
            Permission::Workspace => "workspace",
            Permission::Full => "full",
        }
    }

    /// Whether this tier may answer `verb` at all.
    ///
    /// Read-mostly verbs run at every tier. Everything that executes the repository's own code
    /// needs `workspace`; everything that leaves the machine needs `full`.
    pub fn admits(&self, verb: &str) -> bool {
        match verb {
            "report_status" | "answer_question" => true,
            "run_tests" | "make_change" => {
                matches!(self, Permission::Workspace | Permission::Full)
            }
            // `full_access` is `full` only. It is the one verb whose whole meaning is "and publish
            // it", so admitting it at `workspace` would be admitting it and then refusing the half
            // that was asked for.
            "git_push" | "deploy" | "full_access" => matches!(self, Permission::Full),
            _ => false,
        }
    }
}

/// Which agent runtime a route hands its request to.
///
/// Kept as a string on disk and parsed here, so a config written for an older build still loads
/// and an unrecognised value is a refusal rather than a spawn-time surprise.
///
/// `Claude` is the default deliberately: unattended execution has to work for someone who
/// installed only the published npm package, without a private orchestrator in the picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Runtime {
    /// `claude -p`, driven directly. No dependency beyond the CLI itself.
    Claude,
    /// `codex exec`, driven directly. Non-interactive by design: it never prompts, and what it may
    /// do is decided by the sandbox mode this maps the permission tier onto.
    Codex,
    /// An mcoda agent by pinned slug, which owns adapter selection across the CLI family.
    Mcoda(String),
    /// An mcoda agent that is expected to be remote. Separate from `Mcoda` so that sending
    /// another agent's text off this machine cannot be reached by a routing default drifting.
    McodaCloud(String),
}

/// Slug prefix mcoda gives the managed remote agents it materialises.
const MCODA_CLOUD_PREFIX: &str = "mswarm-cloud-";

impl Runtime {
    /// Whether this runtime is expected to keep the request on this machine.
    ///
    /// The premise in this module's header — routing is local — is only true if the thing on the
    /// end of it is local too, so the caller checks this before handing over untrusted text.
    pub fn is_local(&self) -> bool {
        !matches!(self, Runtime::McodaCloud(_))
    }

    /// The pinned agent slug, where the runtime names one.
    pub fn slug(&self) -> Option<&str> {
        match self {
            Runtime::Claude | Runtime::Codex => None,
            Runtime::Mcoda(slug) | Runtime::McodaCloud(slug) => Some(slug),
        }
    }
}

impl std::str::FromStr for Runtime {
    type Err = Refusal;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        // A slug has to be pinned by whoever wrote the config. Resolving it through mcoda's own
        // `override → workspace_default → global_default` chain would let a file this project does
        // not own decide what runs, and — for a cloud default — where the text goes.
        let pinned = |rest: &str| -> Result<String, Refusal> {
            let slug = rest.trim();
            if slug.is_empty() {
                return Err(Refusal::RuntimeNotPinned);
            }
            if slug.contains(['/', '\\', ' ']) {
                return Err(Refusal::UnknownRuntime);
            }
            Ok(slug.to_string())
        };
        match text {
            "claude" => Ok(Runtime::Claude),
            "codex" => Ok(Runtime::Codex),
            // Named the family but not the agent. Whether mcoda is installed has nothing to do with
            // it — this is a spelling rule, and saying "unknown runtime" here sends people off to
            // check their installation for a problem that is one word long.
            "mcoda" | "mcoda-cloud" => Err(Refusal::RuntimeNotPinned),
            _ => match text.split_once(':') {
                Some(("mcoda", rest)) => {
                    let slug = pinned(rest)?;
                    // A cloud agent reached through the local spelling would be exactly the drift
                    // this split exists to prevent, so it is refused rather than upgraded.
                    if slug.starts_with(MCODA_CLOUD_PREFIX) {
                        return Err(Refusal::RuntimeNotLocal);
                    }
                    Ok(Runtime::Mcoda(slug))
                }
                Some(("mcoda-cloud", rest)) => Ok(Runtime::McodaCloud(pinned(rest)?)),
                _ => Err(Refusal::UnknownRuntime),
            },
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
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

/// Written out rather than derived. `#[derive(Default)]` would give `max_concurrent = 0`, because a
/// serde field default only applies to a *missing field* and never to `Default::default()` — and
/// `load_routing` returns exactly that when there is no config file. The two must agree, or a
/// machine with no `agentd.toml` reports a ceiling of zero and quietly runs one at a time.
impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            mailbox: Vec::new(),
            max_concurrent: default_concurrency(),
            execute: false,
        }
    }
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

/// Serialise routing back to TOML.
///
/// A separate shape from [`RoutingConfig`] for one mechanical reason: TOML requires scalars before
/// tables, and `mailbox` is an array of tables. Ordering the fields here keeps the emitted file
/// valid without dictating the order of the struct everything else reads.
#[derive(serde::Serialize)]
struct RoutingOut<'a> {
    execute: bool,
    max_concurrent: usize,
    mailbox: &'a [MailboxRoute],
}

pub fn write_routing(home: &Path, config: &RoutingConfig) -> Result<PathBuf, Error> {
    let body = toml::to_string_pretty(&RoutingOut {
        execute: config.execute,
        max_concurrent: config.max_concurrent,
        mailbox: &config.mailbox,
    })?;
    let path = config_path(home);
    let header = "# Written by `pigeonpost agentd answer`. Hand edits are kept, comments are not:\n\
                  # this file is parsed and rewritten, so anything explanatory belongs elsewhere.\n\
                  #\n\
                  # `execute = false` leaves the rails in place and runs nothing. `agentd pause` is\n\
                  # the same thing without editing anything, and is what to reach for in a hurry.\n\n";
    std::fs::write(&path, format!("{header}{body}"))?;
    Ok(path)
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
    /// The route's `runtime` is not a spelling this build understands.
    UnknownRuntime,
    /// A known runtime family, named without the agent it should run. Distinct from
    /// [`Refusal::UnknownRuntime`] because the fix is completely different, and one message for
    /// both reads as "that runtime is unsupported" when the runtime is fine.
    RuntimeNotPinned,
    /// The route names a runtime that would take the request off this machine, without saying so.
    RuntimeNotLocal,
    /// The verb was granted by the sender's postbox, but this machine's tier will not carry it out.
    PermissionTooLow,
    /// A push or deploy named a branch the route does not allow, or named none at all.
    BranchNotAllowed,
    /// This sender has had its day's runs from this mailbox.
    DailyLimitReached,
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
            Refusal::UnknownRuntime => "unknown_runtime",
            Refusal::RuntimeNotPinned => "runtime_not_pinned",
            Refusal::RuntimeNotLocal => "runtime_not_local",
            Refusal::PermissionTooLow => "permission_too_low",
            Refusal::BranchNotAllowed => "branch_not_allowed",
            Refusal::DailyLimitReached => "daily_limit_reached",
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
    /// `answer_question`'s question, already length-checked by [`validate_args`]. Untrusted in
    /// exactly the way `note` is; it is the request's content rather than its instruction.
    pub question: Option<String>,
    /// `make_change`'s task. Instruction text written by somebody else, and no schema bounds it —
    /// which is the whole reason the verb needs a permission tier and a route that names it.
    pub task: Option<String>,
    /// The branch or ref a `make_change`, `git_push` or `deploy` names, already checked against
    /// the route's allowlist.
    pub target: Option<String>,
    /// The route's `runtime`, already parsed, so the engine cannot be handed a spelling that was
    /// never validated.
    pub runtime: Runtime,
}

impl MailboxRoute {
    /// Whether `name` is a branch or ref this route may touch.
    ///
    /// `"*"` is any. Exact names otherwise — never a prefix or a pattern, because a route saying
    /// `release` should not quietly also mean `release-candidate-do-not-push`.
    pub fn allows_branch(&self, name: &str) -> bool {
        self.branches.iter().any(|b| b == "*" || b == name)
    }

    /// Whether this route is free to work across branches and tags as the task requires.
    pub fn any_branch(&self) -> bool {
        self.branches.iter().any(|b| b == "*")
    }

    /// Whether this route is the one for `mailbox`.
    ///
    /// A mailbox has two names and the two halves of this system use different ones: the event
    /// stream always carries the `/k/` address, while a config is written by a person and names the
    /// handle, because that is what the docs and its peers call it. Matching on one spelling means
    /// the other silently never routes — mail arrives, nothing runs, and the audit line says
    /// `no_route` as though the config were absent.
    pub fn is_for(&self, mailbox: &str, also_known_as: Option<&str>) -> bool {
        self.address == mailbox || Some(self.address.as_str()) == also_known_as
    }
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
    also_known_as: Option<&str>,
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
        .find(|m| m.is_for(mailbox, also_known_as))
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

    if !RUNNABLE_VERBS.contains(&verb.as_str()) {
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

    // What this machine is willing to let an answer do, which is a different question from what
    // the sender was granted. The server holds one key and this holds the other.
    if !route.permission.admits(&verb) {
        return Err(Refusal::PermissionTooLow);
    }

    // A push or deploy has to name something the route allows. An absent allowlist means nothing
    // is allowed — never "anything", because a deploy with no stated target is exactly the request
    // that cannot be bounded.
    let target = body
        .pointer("/args/branch")
        .or_else(|| body.pointer("/args/ref"))
        .and_then(|b| b.as_str())
        .map(str::to_string);
    if matches!(verb.as_str(), "git_push" | "deploy") {
        match &target {
            Some(t) if route.allows_branch(t) => {}
            _ => return Err(Refusal::BranchNotAllowed),
        }
    }

    // Both of these are the route's own correctness rather than the message's, so they are checked
    // last: a misconfigured route must not mask what the traffic itself would have been refused for.
    let runtime: Runtime = route.runtime.parse()?;
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
        // Read rather than re-validated: `validate_args` has already refused anything that is not
        // a string of a sane length, and refused the key entirely for verbs that do not take it.
        question: body
            .pointer("/args/question")
            .and_then(|q| q.as_str())
            .map(str::to_string),
        task: body
            .pointer("/args/task")
            .and_then(|t| t.as_str())
            .map(str::to_string),
        target,
        runtime,
    })
}

/// Per-verb argument schemas. "The verb is grantable" is not enough on its own: a granted verb
/// carrying attacker-chosen arguments is how a bounded request becomes an arbitrary one.
///
/// Both Phase 4 verbs are questions, so neither takes a path, a command, or anything else that
/// selects work. Anything unexpected is refused rather than ignored.
fn validate_args(verb: &str, args: Option<&serde_json::Value>) -> Result<(), Refusal> {
    let args = match args {
        None | Some(serde_json::Value::Null) => {
            // Only the verbs that carry no instruction can proceed with nothing at all.
            return match verb {
                "report_status" | "answer_question" | "run_tests" | "git_push" | "deploy" => Ok(()),
                "make_change" => Err(Refusal::BadArguments("make_change needs a 'task'")),
                "full_access" => Err(Refusal::BadArguments("full_access needs a 'task'")),
                _ => Err(Refusal::VerbNotInPhase),
            };
        }
        Some(serde_json::Value::Object(map)) => map,
        Some(_) => return Err(Refusal::BadArguments("args must be an object")),
    };

    // One place to say "this key must be a string of a sane length", so a new verb cannot quietly
    // accept an unbounded one.
    let text = |key: &str, max: usize| -> Result<Option<&str>, Refusal> {
        match args.get(key) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(v) => match v.as_str() {
                Some(t) if t.chars().count() <= max => Ok(Some(t)),
                Some(_) => Err(Refusal::BadArguments("an argument is too long")),
                None => Err(Refusal::BadArguments("an argument must be a string")),
            },
        }
    };
    let only = |allowed: &[&str]| -> Result<(), Refusal> {
        for key in args.keys() {
            if !allowed.contains(&key.as_str()) {
                return Err(Refusal::BadArguments("unexpected argument"));
            }
        }
        Ok(())
    };

    match verb {
        "report_status" => {
            if !args.is_empty() {
                return Err(Refusal::BadArguments("report_status takes no arguments"));
            }
        }
        "answer_question" => {
            only(&["question"])?;
            text("question", 4096)?;
        }
        "run_tests" => {
            only(&["target"])?;
            let target = text("target", 512)?;
            if let Some(t) = target {
                reject_traversal(t)?;
            }
        }
        // The task is instruction text. Length is the only property worth checking, and checking
        // more would be theatre: there is no shape that separates a safe instruction from an
        // unsafe one.
        "make_change" | "full_access" => {
            only(&["task", "branch"])?;
            match text("task", 8192)? {
                Some(t) if !t.trim().is_empty() => {}
                _ => {
                    return Err(Refusal::BadArguments(if verb == "make_change" {
                        "make_change needs a 'task'"
                    } else {
                        "full_access needs a 'task'"
                    }))
                }
            }
            if let Some(b) = text("branch", 256)? {
                reject_traversal(b)?;
            }
        }
        "git_push" | "deploy" => {
            only(&["branch", "ref"])?;
            for key in ["branch", "ref"] {
                if let Some(v) = text(key, 256)? {
                    reject_traversal(v)?;
                }
            }
        }
        _ => return Err(Refusal::VerbNotInPhase),
    }
    Ok(())
}

/// Refuse anything that reads like an attempt to leave the workspace or smuggle a second command.
///
/// Not a security boundary — the tiers are — but a name arriving from the network should look like
/// a name, and one that does not is worth refusing where it is cheap to say why.
fn reject_traversal(value: &str) -> Result<(), Refusal> {
    let bad = value.contains("..")
        || value.starts_with('/')
        || value.starts_with('-')
        || value.contains(['\0', '\n', ';', '&', '|', '`', '$']);
    if bad {
        return Err(Refusal::BadArguments("that argument is not a plain name"));
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

/// Count a run against this sender's day, and say whether it is within the route's ceiling.
///
/// A granted peer can otherwise trigger unbounded work, and the cost of a run is no longer a
/// status report — at `workspace` and `full` it is an implementation. `max_concurrent` bounds how
/// many happen at once, which is a different question from how many happen at all.
///
/// Counted before the work rather than after, so a flood is refused rather than merely recorded,
/// and keyed by day so it recovers on its own without anyone clearing state.
pub fn within_daily_limit(home: &Path, sender: &str, day: u64, limit: u32) -> bool {
    if limit == 0 {
        return true;
    }
    let path = home.join(SPEND_FILE);
    let mut ledger: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|b| serde_json::from_str(&b).ok())
        .unwrap_or_else(|| serde_json::json!({}));

    // One day's counts at a time: yesterday's are dropped rather than accumulated, so the file
    // cannot grow without bound on a busy mailbox.
    let today = day.to_string();
    if ledger.get("day").and_then(|d| d.as_str()) != Some(today.as_str()) {
        ledger = serde_json::json!({ "day": today, "senders": {} });
    }
    // Indexed, not pointed at: a JSON Pointer treats `/` as a separator and every sender key is
    // `/bekir/…`, so `/senders//bekir/noisy` resolves to nothing and the ceiling never fires.
    let used = ledger
        .get("senders")
        .and_then(|s| s.get(sender))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if used >= limit as u64 {
        return false;
    }
    if let Some(senders) = ledger.get_mut("senders").and_then(|s| s.as_object_mut()) {
        senders.insert(sender.to_string(), serde_json::json!(used + 1));
    }
    // Best effort: a ledger that cannot be written must not stop the work, or an unwritable home
    // would silently become a total refusal.
    let _ = std::fs::write(&path, ledger.to_string());
    true
}

/// Days since the epoch, which is all the resolution a daily ceiling needs.
pub fn today(now: u64) -> u64 {
    now / 86_400
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

    #[test]
    fn a_wildcard_branch_allows_any_target_and_an_empty_one_allows_none() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(&["git_push"], dir.path());
        cfg.mailbox[0].permission = Permission::Full;

        cfg.mailbox[0].branches = vec!["*".into()];
        assert!(cfg.mailbox[0].allows_branch("main"));
        assert!(cfg.mailbox[0].allows_branch("fix/anything"));
        assert!(cfg.mailbox[0].allows_branch("v1.2.3"));
        assert!(cfg.mailbox[0].any_branch());

        // Absent still means nothing: a deploy with no stated target is the request that cannot
        // be bounded, and it must not become "anything" by default.
        cfg.mailbox[0].branches = Vec::new();
        assert!(!cfg.mailbox[0].allows_branch("main"));
        assert!(!cfg.mailbox[0].any_branch());
    }

    /// An exact name is exact. `release` must not quietly also mean `release-candidate`.
    #[test]
    fn a_named_branch_is_not_a_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(&["git_push"], dir.path());
        cfg.mailbox[0].branches = vec!["release".into()];
        assert!(cfg.mailbox[0].allows_branch("release"));
        assert!(!cfg.mailbox[0].allows_branch("release-candidate"));
        assert!(!cfg.mailbox[0].allows_branch("rel"));
    }

    #[test]
    fn codex_is_a_runtime_this_build_understands() {
        assert_eq!("codex".parse::<Runtime>().unwrap(), Runtime::Codex);
        // Local, so untrusted text never leaves the machine on this route.
        assert!(Runtime::Codex.is_local());
        // No slug to pin: unlike mcoda, the family names the thing that runs.
        assert!(Runtime::Codex.slug().is_none());
    }

    fn route(verbs: &[&str], workspace: &Path) -> MailboxRoute {
        MailboxRoute {
            address: "/bekir/agent1".into(),
            workspace: workspace.to_path_buf(),
            runtime: "claude".into(),
            verbs: verbs.iter().map(|v| v.to_string()).collect(),
            timeout_secs: 60,
            permission: Permission::ReadOnly,
            branches: Vec::new(),
            daily_runs_per_sender: 50,
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

    fn envelope_with(verb: &str, args: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "v": 1, "verb": verb, "args": args, "note": "because" })
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
            None,
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
                None,
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
                None,
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
                None,
                &message("auto", "report_status", envelope("report_status"))
            ),
            Err(Refusal::NoRoute)
        );
    }

    /// The failure a live run found and no unit test had: the event stream carries the `/k/`
    /// address while the config names the handle, so exact matching refused real mail as `no_route`
    /// while looking correctly configured.
    #[test]
    fn a_route_written_by_handle_matches_the_address_on_the_wire() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(&["report_status"], dir.path());
        cfg.mailbox[0].address = "/bekir/agent1".into();

        let action = classify(
            &cfg,
            dir.path(),
            "/k/fd7qzt3zbkkgcmdgneph6kmr7w",
            Some("/bekir/agent1"),
            &message("auto", "report_status", envelope("report_status")),
        );
        assert!(action.is_ok(), "the handle in config names this mailbox");

        // And the other way round, for a config written with the address.
        cfg.mailbox[0].address = "/k/fd7qzt3zbkkgcmdgneph6kmr7w".into();
        assert!(classify(
            &cfg,
            dir.path(),
            "/k/fd7qzt3zbkkgcmdgneph6kmr7w",
            Some("/bekir/agent1"),
            &message("auto", "report_status", envelope("report_status")),
        )
        .is_ok());
    }

    /// Knowing a second name must not make every mailbox match.
    #[test]
    fn an_alias_does_not_widen_routing_to_other_mailboxes() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(&["report_status"], dir.path());
        assert_eq!(
            classify(
                &cfg,
                dir.path(),
                "/k/someone-else",
                Some("/bekir/someone-else"),
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
                None,
                &message("auto", "answer_question", envelope("answer_question"))
            ),
            Err(Refusal::VerbNotEnabledHere)
        );
    }

    /// The second key, and the reason a grant alone is not enough: the sender was granted this and
    /// the route names it, and it is still refused because this machine will not do that much.
    #[test]
    fn a_granted_verb_is_refused_by_a_tier_that_will_not_carry_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(&["run_tests"], dir.path());
        cfg.mailbox[0].verbs = vec!["run_tests".into()];
        assert_eq!(
            classify(
                &cfg,
                dir.path(),
                "/bekir/agent1",
                None,
                &message("auto", "run_tests", envelope("run_tests"))
            ),
            Err(Refusal::PermissionTooLow)
        );

        // Raised, it runs — the verb was never the problem.
        cfg.mailbox[0].permission = Permission::Workspace;
        assert!(classify(
            &cfg,
            dir.path(),
            "/bekir/agent1",
            None,
            &message("auto", "run_tests", envelope("run_tests"))
        )
        .is_ok());
    }

    /// Raising the tier must not widen which verbs are answerable: the two are separate keys and a
    /// machine willing to deploy has still only agreed to deploy when asked for a deploy.
    #[test]
    fn a_high_tier_does_not_admit_a_verb_the_route_never_named() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(&["report_status"], dir.path());
        cfg.mailbox[0].permission = Permission::Full;
        cfg.mailbox[0].branches = vec!["main".into()];
        assert_eq!(
            classify(
                &cfg,
                dir.path(),
                "/bekir/agent1",
                None,
                &message(
                    "auto",
                    "deploy",
                    envelope_with("deploy", serde_json::json!({"branch":"main"}))
                )
            ),
            Err(Refusal::VerbNotEnabledHere)
        );
    }

    /// A deploy with no stated target is the request this design cannot bound, so an absent
    /// allowlist means nothing is allowed rather than anything.
    #[test]
    fn a_push_or_deploy_must_name_a_branch_the_route_allows() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(&["deploy"], dir.path());
        cfg.mailbox[0].verbs = vec!["deploy".into()];
        cfg.mailbox[0].permission = Permission::Full;

        let asking =
            |args: serde_json::Value| message("auto", "deploy", envelope_with("deploy", args));

        // No allowlist at all.
        assert_eq!(
            classify(
                &cfg,
                dir.path(),
                "/bekir/agent1",
                None,
                &asking(serde_json::json!({"branch":"main"}))
            ),
            Err(Refusal::BranchNotAllowed)
        );

        cfg.mailbox[0].branches = vec!["main".into()];
        // A branch that is not on it.
        assert_eq!(
            classify(
                &cfg,
                dir.path(),
                "/bekir/agent1",
                None,
                &asking(serde_json::json!({"branch":"prod"}))
            ),
            Err(Refusal::BranchNotAllowed)
        );
        // None named at all.
        assert_eq!(
            classify(
                &cfg,
                dir.path(),
                "/bekir/agent1",
                None,
                &asking(serde_json::json!({}))
            ),
            Err(Refusal::BranchNotAllowed)
        );
        // And the one that is allowed.
        let action = classify(
            &cfg,
            dir.path(),
            "/bekir/agent1",
            None,
            &asking(serde_json::json!({"branch":"main"})),
        )
        .unwrap();
        assert_eq!(action.target.as_deref(), Some("main"));
    }

    #[test]
    fn make_change_needs_a_task_and_carries_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(&["make_change"], dir.path());
        cfg.mailbox[0].verbs = vec!["make_change".into()];
        cfg.mailbox[0].permission = Permission::Workspace;

        assert!(matches!(
            classify(
                &cfg,
                dir.path(),
                "/bekir/agent1",
                None,
                &message(
                    "auto",
                    "make_change",
                    envelope_with("make_change", serde_json::json!({}))
                )
            ),
            Err(Refusal::BadArguments(_))
        ));

        let action = classify(
            &cfg,
            dir.path(),
            "/bekir/agent1",
            None,
            &message(
                "auto",
                "make_change",
                envelope_with(
                    "make_change",
                    serde_json::json!({"task":"fix the pipeline"}),
                ),
            ),
        )
        .unwrap();
        assert_eq!(action.task.as_deref(), Some("fix the pipeline"));
    }

    /// A name arriving from the network should look like a name.
    #[test]
    fn an_argument_that_is_not_a_plain_name_is_refused() {
        for bad in [
            "../etc",
            "/etc/passwd",
            "main; rm -rf /",
            "main && curl x",
            "-force",
            "a`b`",
            "a$b",
        ] {
            assert!(
                reject_traversal(bad).is_err(),
                "{bad:?} should not pass as a branch"
            );
        }
        for good in ["main", "release/1.2", "feature_x"] {
            assert!(reject_traversal(good).is_ok(), "{good:?} is a name");
        }
    }

    /// The ceiling has to refuse *before* the work, or it merely records a flood.
    #[test]
    fn a_senders_day_is_counted_and_then_refused() {
        let dir = tempfile::tempdir().unwrap();
        for i in 1..=3 {
            assert!(
                within_daily_limit(dir.path(), "/bekir/noisy", 20_000, 3),
                "run {i} is within a ceiling of 3"
            );
        }
        assert!(!within_daily_limit(dir.path(), "/bekir/noisy", 20_000, 3));
        // Another sender is unaffected, and tomorrow recovers on its own.
        assert!(within_daily_limit(dir.path(), "/bekir/quiet", 20_000, 3));
        assert!(within_daily_limit(dir.path(), "/bekir/noisy", 20_001, 3));
        // Zero means no ceiling.
        for _ in 0..10 {
            assert!(within_daily_limit(
                dir.path(),
                "/bekir/unbounded",
                20_000,
                0
            ));
        }
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
                None,
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
                None,
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
                None,
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
                None,
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
                None,
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
            classify(&cfg, dir.path(), "/bekir/agent1", None, &msg),
            Err(Refusal::BadArguments(_))
        ));
    }

    /// The file-absent path and the field-absent path have to agree, or a machine with no config
    /// reports a ceiling nobody chose.
    #[test]
    fn a_missing_config_and_a_missing_field_give_the_same_ceiling() {
        let dir = tempfile::tempdir().unwrap();
        let absent = load_routing(dir.path()).unwrap();
        assert_eq!(absent.max_concurrent, default_concurrency());
        assert!(!absent.execute, "execution must stay off without a config");

        std::fs::write(config_path(dir.path()), "execute = true\n").unwrap();
        let partial = load_routing(dir.path()).unwrap();
        assert_eq!(partial.max_concurrent, absent.max_concurrent);
    }

    #[test]
    fn routing_survives_a_write_and_read_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(&["report_status"], dir.path());
        cfg.mailbox[0].runtime = "mcoda:claude-sonnet".into();
        write_routing(dir.path(), &cfg).unwrap();

        let back = load_routing(dir.path()).unwrap();
        assert!(back.execute);
        assert_eq!(back.max_concurrent, cfg.max_concurrent);
        assert_eq!(back.mailbox, cfg.mailbox);
    }

    #[test]
    fn the_default_runtime_parses_and_is_local() {
        assert_eq!(default_runtime().parse::<Runtime>(), Ok(Runtime::Claude));
        assert!(Runtime::Claude.is_local());
        assert_eq!(Runtime::Claude.slug(), None);
    }

    #[test]
    fn an_mcoda_runtime_carries_a_pinned_slug() {
        assert_eq!(
            "mcoda:claude-sonnet".parse::<Runtime>(),
            Ok(Runtime::Mcoda("claude-sonnet".into()))
        );
        assert_eq!(
            "mcoda:claude-sonnet".parse::<Runtime>().unwrap().slug(),
            Some("claude-sonnet")
        );
        assert!("mcoda:claude-sonnet".parse::<Runtime>().unwrap().is_local());
    }

    /// The whole point of the two spellings: reaching a managed remote agent has to be written
    /// down as such, so it cannot be arrived at by a default drifting underneath us.
    #[test]
    fn a_cloud_slug_under_the_local_spelling_is_refused() {
        assert_eq!(
            "mcoda:mswarm-cloud-some-remote".parse::<Runtime>(),
            Err(Refusal::RuntimeNotLocal)
        );
        let cloud = "mcoda-cloud:mswarm-cloud-some-remote"
            .parse::<Runtime>()
            .unwrap();
        assert_eq!(
            cloud,
            Runtime::McodaCloud("mswarm-cloud-some-remote".into())
        );
        assert!(!cloud.is_local());
    }

    #[test]
    fn an_unknown_runtime_is_refused_not_defaulted() {
        for text in [
            "",
            "gemini",
            "claude-cli",
            "mcoda:has a space",
            "mcoda:../escape",
        ] {
            assert_eq!(
                text.parse::<Runtime>(),
                Err(Refusal::UnknownRuntime),
                "{text:?} should not parse"
            );
        }
    }

    /// Naming the family without the agent is a different mistake from naming a family that does
    /// not exist, and telling someone their runtime is unknown when it is merely unpinned sends
    /// them to check whether mcoda is installed.
    #[test]
    fn a_runtime_family_named_without_its_agent_says_so() {
        for text in [
            "mcoda",
            "mcoda:",
            "mcoda:   ",
            "mcoda-cloud",
            "mcoda-cloud:",
        ] {
            assert_eq!(
                text.parse::<Runtime>(),
                Err(Refusal::RuntimeNotPinned),
                "{text:?} should ask for a slug, not report an unknown runtime"
            );
        }
    }

    #[test]
    fn a_route_with_an_unknown_runtime_refuses_rather_than_acting() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(&["report_status"], dir.path());
        cfg.mailbox[0].runtime = "gemini".into();
        assert_eq!(
            classify(
                &cfg,
                dir.path(),
                "/bekir/agent1",
                None,
                &message("auto", "report_status", envelope("report_status"))
            ),
            Err(Refusal::UnknownRuntime)
        );
    }

    /// A route's own misconfiguration is checked after the message's, so the audit log keeps saying
    /// what the traffic would have been refused for.
    #[test]
    fn a_held_message_still_reports_not_auto_on_a_misconfigured_route() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(&["report_status"], dir.path());
        cfg.mailbox[0].runtime = "gemini".into();
        assert_eq!(
            classify(
                &cfg,
                dir.path(),
                "/bekir/agent1",
                None,
                &message("review", "report_status", envelope("report_status"))
            ),
            Err(Refusal::NotAuto)
        );
    }

    #[test]
    fn an_actionable_message_carries_its_parsed_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(&["report_status"], dir.path());
        cfg.mailbox[0].runtime = "mcoda:claude-sonnet".into();
        let action = classify(
            &cfg,
            dir.path(),
            "/bekir/agent1",
            None,
            &message("auto", "report_status", envelope("report_status")),
        )
        .unwrap();
        assert_eq!(action.runtime, Runtime::Mcoda("claude-sonnet".into()));
    }
}
