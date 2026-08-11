//! The `pigeonpost` binary.
//!
//! One binary for both halves of the product, as `docs/node.md` promises: the client verbs an
//! agent uses, and the `loft` verb that turns the host into a node. An operator who can Pigeonpost
//! a message is one command away from hosting.

mod directory_cmd;
mod handle_cmd;
mod install_cmd;
mod loft_cmd;
mod loft_key;
mod output;
mod registry_cmd;
mod runtime_config;
mod submit_cmd;
#[cfg(test)]
mod test_support;
mod trust_cmd;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use pigeonpost_client::{
    Agent, AgentOpenOptions, AttributionRequirement, Jurisdiction, OutboxRecordId, StorageLimits,
    StorageStatus, FINISHED_OUTBOX_PRUNE_CONFIRMATION, MAX_INBOX_BODY_BYTES_LIMIT,
    MAX_INBOX_MESSAGE_LIMIT, MAX_OUTBOX_PAYLOAD_BYTES_LIMIT, MAX_OUTBOX_ROW_LIMIT,
    PENDING_OUTBOX_DELETE_CONFIRMATION,
};
use pigeonpost_core::{Address, Destination};

use output::{print_inbox, print_message};

const MAX_DEAD_LETTER_OUTPUT: usize = 100;
const DEFAULT_STORAGE_LIST_RESULTS: usize = 50;
const MAX_STORAGE_LIST_RESULTS: usize = 1_000;

#[derive(Parser)]
#[command(
    name = "pigeonpost",
    version,
    about = "Asynchronous messaging for AI agents: permanent addresses and private inboxes.",
    long_about = None
)]
struct Cli {
    /// Where this agent's identity and state live.
    #[arg(long, env = "PIGEONPOST_HOME", global = true)]
    home: Option<PathBuf>,

    /// Existing owner-only directory holding the independently custodied successor key.
    #[arg(long, env = "PIGEONPOST_RECOVERY_DIR", global = true)]
    recovery_dir: Option<PathBuf>,

    /// Machine-readable output.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print this agent's address, creating an identity on first run.
    Id,

    /// Rotate to the precommitted successor, or resume that exact durable transition.
    Rotate {
        /// Exact currently active or journaled predecessor address being authorized.
        #[arg(long, value_parser = parse_address)]
        confirm: Address,
    },

    /// Send a message.
    Send {
        /// Recipient address, e.g. /k/6htgz65xb7yfs53dmhdanfmk7c
        to: String,
        /// Message body. Use `-` to read stdin.
        #[arg(long, short)]
        body: String,
        /// Call-local attribution agreement; avoids mutating the persistent sender default.
        #[arg(long, value_enum)]
        attribution_jurisdiction: Option<AttributionJurisdictionArg>,
        /// Stable 32-byte custody-authority id paired with `--attribution-jurisdiction`.
        #[arg(long)]
        attribution_authority: Option<String>,
    },

    /// Fetch waiting Pigeonpost messages from every loft, then list what is unread.
    Inbox {
        /// List everything, not just unread.
        #[arg(long)]
        all: bool,
        /// Skip fetching; show what is already stored.
        #[arg(long)]
        offline: bool,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },

    /// Show one message. Does not mark it read.
    Read {
        /// Message id, or an unambiguous prefix.
        id: String,
    },

    /// Mark a message read.
    Ack { id: String },

    /// Retry anything still sitting in the outbox.
    Flush,

    /// Inspect and explicitly manage bounded local message and delivery storage.
    Storage {
        #[command(subcommand)]
        action: StorageAction,
    },

    /// Pigeonpost messages held for review because their sender is unknown.
    Pending {
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },

    /// Allowlist a sender and release anything of theirs held pending.
    Allow { address: String },

    /// Block a sender.
    Block { address: String },

    /// Flag a message as spam, lowering its sender's local score.
    Spam { id: String },

    /// Demand proof-of-work from unsolicited senders.
    PowFloor { bits: u32 },

    /// Open or close the inbox to strangers.
    AcceptAll { value: bool },

    /// Manage capability tokens for an open inbox.
    Token {
        #[command(subcommand)]
        action: TokenAction,
    },

    /// Configure recipient attribution and sender jurisdiction behavior.
    Attribution {
        #[command(subcommand)]
        action: AttributionAction,
    },

    /// Import, inspect, or explicitly reset witnessed registry trust.
    RegistryTrust {
        #[command(subcommand)]
        action: RegistryTrustAction,
    },

    /// Serve this agent's Pigeonpost inbox as MCP tools over stdio.
    Mcp,

    /// Manage the lofts this agent uses.
    Loft {
        #[command(subcommand)]
        action: LoftAction,
    },

    /// Turn this box into a loft. Private by default; no flags needed.
    Install {
        #[arg(long, env = "PIGEONPOST_LOFT_DIR", default_value = ".")]
        dir: PathBuf,
        /// Public hostname. Without it the loft serves this host only and does not join the pool.
        #[arg(long)]
        domain: Option<String>,
        #[arg(long)]
        capacity_gb: Option<u64>,
        #[arg(long, default_value_t = 30)]
        retention_days: u64,
        #[arg(long)]
        bind: Option<String>,
        /// Write config and keys without touching the service manager.
        #[arg(long)]
        no_service: bool,
    },

    /// Claim, rotate, and resolve human-readable handles.
    Handle {
        #[command(subcommand)]
        action: HandleAction,
    },

    /// Run the handle registry (operators only).
    Registry {
        #[command(subcommand)]
        action: RegistryAction,
    },

    /// Run the pool directory and its prober (operators only).
    Directory {
        #[command(subcommand)]
        action: DirectoryAction,
    },
}

#[derive(Subcommand)]
enum StorageAction {
    /// Read exact local inbox/outbox limits and usage counters.
    Status,

    /// Atomically replace all four storage limits without deleting existing data.
    SetLimits {
        #[arg(long, value_parser = parse_inbox_message_limit)]
        inbox_messages: u64,
        #[arg(long, value_parser = parse_inbox_body_bytes_limit)]
        inbox_body_bytes: u64,
        #[arg(long, value_parser = parse_outbox_row_limit)]
        outbox_rows: u64,
        #[arg(long, value_parser = parse_outbox_payload_bytes_limit)]
        outbox_payload_bytes: u64,
    },

    /// List payload-free metadata for copies still owed to lofts.
    Pending {
        #[arg(long, default_value_t = DEFAULT_STORAGE_LIST_RESULTS, value_parser = parse_storage_list_limit)]
        limit: usize,
    },

    /// List payload-free metadata for copies already accepted by lofts.
    Completed {
        #[arg(long, default_value_t = DEFAULT_STORAGE_LIST_RESULTS, value_parser = parse_storage_list_limit)]
        limit: usize,
    },

    /// List payload-free metadata for terminal delivery copies.
    DeadLetters {
        #[arg(long, default_value_t = DEFAULT_STORAGE_LIST_RESULTS, value_parser = parse_storage_list_limit)]
        limit: usize,
    },

    /// Delete one completed-delivery metadata row.
    DeleteCompleted {
        #[arg(value_parser = parse_outbox_row_id)]
        row: OutboxRecordId,
    },

    /// Delete one terminal-delivery metadata row.
    DeleteDeadLetter {
        #[arg(value_parser = parse_outbox_row_id)]
        row: OutboxRecordId,
    },

    /// Permanently discard one undelivered copy.
    DeletePending {
        #[arg(value_parser = parse_outbox_row_id)]
        row: OutboxRecordId,
        #[arg(long, value_parser = parse_pending_delete_confirmation)]
        confirm: String,
    },

    /// Permanently delete one locally stored received message.
    DeleteMessage {
        #[arg(value_parser = parse_message_id)]
        id: String,
        /// Must exactly equal the message id.
        #[arg(long, value_parser = parse_message_id)]
        confirm: String,
    },

    /// Delete one bounded batch of completed or terminal delivery metadata.
    PruneFinished {
        /// Delete rows whose terminal timestamp is strictly before this Unix timestamp.
        #[arg(long, value_parser = parse_before_timestamp)]
        before: u64,
        #[arg(long, value_parser = parse_storage_list_limit)]
        limit: usize,
        #[arg(long, value_parser = parse_prune_finished_confirmation)]
        confirm: String,
    },
}

#[derive(Subcommand)]
enum DirectoryAction {
    /// Trust a directory using its out-of-band signing-key pin.
    Add {
        #[arg(value_parser = parse_canonical_directory_url)]
        url: String,
        /// 32-byte Ed25519 directory signing key, hex.
        #[arg(long)]
        key: String,
    },
    /// Remove one exact directory pin and cached snapshot so its slot or signing key can change.
    Remove {
        #[arg(value_parser = parse_canonical_directory_url)]
        url: String,
        /// Must exactly equal the canonical directory URL.
        #[arg(long, value_parser = parse_canonical_directory_url)]
        confirm: String,
    },
    /// Refresh every configured signed directory snapshot.
    Refresh,
    /// Select lofts from configured directories and publish this agent's record.
    Bootstrap,
    /// List configured directory pins.
    List,
    /// Serve directory.json, accept submissions, and probe the pool.
    Serve {
        #[arg(
            long,
            env = "PIGEONPOST_DIRECTORY_BIND",
            default_value = "127.0.0.1:7719"
        )]
        bind: String,
        #[arg(long, env = "PIGEONPOST_DIRECTORY_DIR", default_value = ".")]
        dir: PathBuf,
    },
}

#[derive(Subcommand)]
enum TokenAction {
    /// Mint a token and publish it to this agent's lofts.
    Mint { label: String },
    /// Revoke a token. Pigeonpost messages using it stop being accepted.
    Revoke { label: String },
    /// List live token labels.
    List,
}

#[derive(Subcommand)]
enum AttributionAction {
    /// Show the recipient gate and sender custody agreement.
    Status,
    /// Select the exact scope this recipient's lofts enforce, or disable it with `off`.
    Recipient {
        #[arg(value_enum)]
        jurisdiction: AttributionJurisdictionArg,
        /// Stable 32-byte custody-authority id as 64 lowercase hexadecimal characters.
        #[arg(long)]
        authority: Option<String>,
    },
    /// Agree to an exact attributed-sending scope, or restore privacy-first sending with `off`.
    Sender {
        #[arg(value_enum)]
        jurisdiction: AttributionJurisdictionArg,
        /// Stable 32-byte custody-authority id as 64 lowercase hexadecimal characters.
        #[arg(long)]
        authority: Option<String>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum AttributionJurisdictionArg {
    Off,
    Us,
    Eu,
    Tr,
    Test,
}

impl From<AttributionJurisdictionArg> for Option<Jurisdiction> {
    fn from(value: AttributionJurisdictionArg) -> Self {
        match value {
            AttributionJurisdictionArg::Off => None,
            AttributionJurisdictionArg::Us => Some(Jurisdiction::Us),
            AttributionJurisdictionArg::Eu => Some(Jurisdiction::Eu),
            AttributionJurisdictionArg::Tr => Some(Jurisdiction::Tr),
            AttributionJurisdictionArg::Test => Some(Jurisdiction::Test),
        }
    }
}

#[derive(Subcommand)]
enum RegistryTrustAction {
    /// Import a strict JSON trust bundle from PATH, or from stdin with `-`.
    Import {
        #[arg(long, value_name = "PATH")]
        file: PathBuf,
    },
    /// Show exact public trust anchors and the accepted witnessed checkpoint.
    Status,
    /// Delete trust and all registry-derived state. Requires the exact confirmation phrase.
    Reset {
        #[arg(long)]
        confirm: String,
    },
}

#[derive(Subcommand)]
enum HandleAction {
    /// Claim a handle, binding it to this agent's key.
    Claim {
        /// e.g. /github/superaidev
        handle: String,
        #[arg(long, env = "PIGEONPOST_REGISTRY_URL")]
        registry: String,
        /// Test registries only: claim without proving anything.
        #[arg(long)]
        mock_name: Option<String>,
        /// Do not launch or listen locally; paste the full provider callback URL at a hidden prompt.
        #[arg(long)]
        no_browser: bool,
    },
    /// Rebind an existing handle to this agent's current key, including after total local key loss.
    Rotate {
        /// e.g. /github/superaidev
        handle: String,
        #[arg(long, env = "PIGEONPOST_REGISTRY_URL")]
        registry: String,
        /// Test registries only: rotate without proving anything.
        #[arg(long)]
        mock_name: Option<String>,
        /// Do not launch or listen locally; paste the full provider callback URL at a hidden prompt.
        #[arg(long)]
        no_browser: bool,
    },
    /// Resolve through the previously imported witnessed registry trust root.
    Resolve {
        handle: String,
        #[arg(long, env = "PIGEONPOST_REGISTRY_URL")]
        registry: String,
    },
    /// Fetch a registry's signed tree head.
    Checkpoint {
        #[arg(long, env = "PIGEONPOST_REGISTRY_URL")]
        registry: String,
        /// Registry checkpoint public key, hex. Without it the signature is not checked.
        #[arg(long)]
        key: Option<String>,
    },
}

#[derive(Subcommand)]
enum RegistryAction {
    /// Serve the registry: handle registration, resolution, and the log dump.
    Serve {
        #[arg(
            long,
            env = "PIGEONPOST_REGISTRY_BIND",
            default_value = "127.0.0.1:7718"
        )]
        bind: String,
        #[arg(long, env = "PIGEONPOST_REGISTRY_DIR", default_value = ".")]
        dir: PathBuf,
        #[arg(
            long,
            env = "PIGEONPOST_REGISTRY_ORIGIN",
            default_value = "pigeonpost.dev/registry"
        )]
        origin: String,
        /// Last signed checkpoint from a legacy registry; authorizes its one-time migration.
        #[arg(long, env = "PIGEONPOST_LEGACY_CHECKPOINT")]
        legacy_checkpoint: Option<PathBuf>,
    },

    /// Publish or change a compliance public key through the local operator ceremony.
    ComplianceKey {
        #[command(subcommand)]
        action: RegistryComplianceKeyAction,
    },
}

#[derive(Args)]
struct RegistryComplianceOperatorArgs {
    #[arg(long, env = "PIGEONPOST_REGISTRY_DIR", default_value = ".")]
    dir: PathBuf,
    #[arg(
        long,
        env = "PIGEONPOST_REGISTRY_ORIGIN",
        default_value = "pigeonpost.dev/registry"
    )]
    origin: String,
    /// Exact canonical 47-byte compliance-key id, as 94 lowercase hexadecimal characters.
    #[arg(long)]
    key_id: String,
    /// Repeat the exact key id to acknowledge the append-only target.
    #[arg(long)]
    confirm_key_id: String,
    /// Independently stored copy of checkpoint.key; must be absolute and outside the registry dir.
    #[arg(long)]
    checkpoint_backup: PathBuf,
    /// Maximum time to wait for durable witness publication.
    #[arg(long, default_value_t = 120)]
    witness_timeout_seconds: u64,
    /// Confirm that the public registry process has been stopped for this local ceremony.
    #[arg(long)]
    confirm_offline: bool,
    /// Perform the append. Without this flag the command validates and prints a dry run.
    #[arg(long)]
    execute: bool,
}

#[derive(Subcommand)]
enum RegistryComplianceKeyAction {
    /// Append the first active publication for a newly provisioned custody public key.
    Publish {
        #[command(flatten)]
        operator: RegistryComplianceOperatorArgs,
        #[arg(long, value_enum)]
        purpose: CompliancePurposeArg,
        #[arg(long, value_enum)]
        jurisdiction: ComplianceJurisdictionArg,
        /// Stable 32-byte authority id, as 64 lowercase hexadecimal characters.
        #[arg(long)]
        authority: String,
        #[arg(long)]
        epoch_start_ms: u64,
        #[arg(long)]
        generation: u32,
        /// Raw 32-byte X25519 public key, as 64 lowercase hexadecimal characters.
        #[arg(long)]
        public_key: String,
        #[arg(long)]
        not_after_ms: u64,
    },
    /// Append an allowed retirement or revocation without redefining key material.
    Transition {
        #[command(flatten)]
        operator: RegistryComplianceOperatorArgs,
        #[arg(long, value_enum)]
        status: ComplianceTransitionArg,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum CompliancePurposeArg {
    Attribution,
    NetworkTrace,
    IdentityTrace,
}

impl From<CompliancePurposeArg> for pigeonpost_compliance_format::CompliancePurpose {
    fn from(value: CompliancePurposeArg) -> Self {
        match value {
            CompliancePurposeArg::Attribution => Self::Attribution,
            CompliancePurposeArg::NetworkTrace => Self::NetworkTrace,
            CompliancePurposeArg::IdentityTrace => Self::IdentityTrace,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum ComplianceJurisdictionArg {
    Us,
    Eu,
    Tr,
    Test,
}

impl From<ComplianceJurisdictionArg> for pigeonpost_compliance_format::Jurisdiction {
    fn from(value: ComplianceJurisdictionArg) -> Self {
        match value {
            ComplianceJurisdictionArg::Us => Self::Us,
            ComplianceJurisdictionArg::Eu => Self::Eu,
            ComplianceJurisdictionArg::Tr => Self::Tr,
            ComplianceJurisdictionArg::Test => Self::Test,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum ComplianceTransitionArg {
    Retired,
    Revoked,
}

impl From<ComplianceTransitionArg> for pigeonpost_registry::entry::ComplianceKeyStatus {
    fn from(value: ComplianceTransitionArg) -> Self {
        match value {
            ComplianceTransitionArg::Retired => Self::Retired,
            ComplianceTransitionArg::Revoked => Self::Revoked,
        }
    }
}

#[derive(Subcommand)]
enum LoftAction {
    /// Use a loft, and publish this agent's record to it.
    Add { url: String },
    /// Stop using a loft.
    Remove { url: String },
    /// List the lofts in use.
    List,
    /// Submit this host's loft to a directory, joining the pool.
    Submit {
        #[arg(long, env = "PIGEONPOST_DIRECTORY_URL")]
        directory: String,
        /// Public URL clients will reach this loft on.
        #[arg(long)]
        endpoint: String,
        #[arg(long, env = "PIGEONPOST_LOFT_DIR", default_value = ".")]
        dir: PathBuf,
        /// Optional Pigeonpost handle, offering accountability without gatekeeping admission.
        #[arg(long)]
        operator: Option<String>,
    },

    /// Stop new selection while continuing to serve reads through an absolute UTC deadline.
    Drain {
        #[arg(long, env = "PIGEONPOST_DIRECTORY_URL")]
        directory: String,
        /// Exact public URL previously submitted for this loft.
        #[arg(long)]
        endpoint: String,
        #[arg(long, env = "PIGEONPOST_LOFT_DIR", default_value = ".")]
        dir: PathBuf,
        /// Absolute UTC deadline in YYYY-MM-DDTHH:MM:SSZ form.
        #[arg(long)]
        after: String,
    },

    /// Run a loft on this machine.
    Serve {
        /// Override the bind address from loft.toml.
        #[arg(long, env = "PIGEONPOST_BIND")]
        bind: Option<String>,
        #[arg(long, env = "PIGEONPOST_LOFT_DIR", default_value = ".")]
        dir: PathBuf,
        /// Override the storage capacity from loft.toml.
        #[arg(long, env = "PIGEONPOST_CAPACITY_GB")]
        capacity_gb: Option<u64>,
        /// Override the retention period from loft.toml.
        #[arg(long, env = "PIGEONPOST_RETENTION_DAYS")]
        retention_days: Option<u64>,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(pigeonpost_log_filter())
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    runtime.block_on(run(cli))
}

/// Limit operator-selected verbosity to first-party targets. Global `trace` enables Axum's
/// connection and JSON-rejection diagnostics, both of which contain attacker-controlled network
/// identifiers or request values. `PIGEONPOST_LOG` is therefore a level, not an arbitrary target
/// directive string.
fn pigeonpost_log_filter() -> tracing_subscriber::EnvFilter {
    let configured = std::env::var("PIGEONPOST_LOG").ok();
    tracing_subscriber::EnvFilter::new(pigeonpost_log_directives(configured.as_deref()))
}

fn pigeonpost_log_directives(configured: Option<&str>) -> String {
    let level = match configured.map(str::trim).map(str::to_ascii_lowercase) {
        None => "warn",
        Some(level)
            if matches!(
                level.as_str(),
                "off" | "error" | "warn" | "info" | "debug" | "trace"
            ) =>
        {
            match level.as_str() {
                "off" => "off",
                "error" => "error",
                "warn" => "warn",
                "info" => "info",
                "debug" => "debug",
                "trace" => "trace",
                _ => unreachable!("matched closed log-level set"),
            }
        }
        Some(_) => "warn",
    };
    let mut directives = String::from("off");
    for target in [
        "pigeonpost",
        "pigeonpost_cli",
        "pigeonpost_client",
        "pigeonpost_core",
        "pigeonpost_loft",
        "pigeonpost_registry",
        "pigeonpost_directory",
        "pigeonpost_mcp",
        "pigeonpost_compliance",
        "pigeonpost_compliance_format",
        "pigeonpost_compliance_seal",
    ] {
        directives.push(',');
        directives.push_str(target);
        directives.push('=');
        directives.push_str(level);
    }
    directives
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    validate_preopen_confirmations(&cli.command)?;

    // `loft serve` is the one command that is not about an agent identity, so it runs before any
    // identity is created — a node operator need not be an agent.
    if let Command::Install {
        dir,
        domain,
        capacity_gb,
        retention_days,
        bind,
        no_service,
    } = &cli.command
    {
        return install_cmd::run(install_cmd::InstallOptions {
            dir: dir.clone(),
            domain: domain.clone(),
            capacity_gb: *capacity_gb,
            retention_days: *retention_days,
            bind: bind.clone(),
            no_service: *no_service,
        });
    }

    if let Command::Registry { action } = &cli.command {
        match action {
            RegistryAction::Serve {
                bind,
                dir,
                origin,
                legacy_checkpoint,
            } => {
                return registry_cmd::serve(bind, dir, origin, legacy_checkpoint.as_deref()).await;
            }
            RegistryAction::ComplianceKey { action } => match action {
                RegistryComplianceKeyAction::Publish {
                    operator,
                    purpose,
                    jurisdiction,
                    authority,
                    epoch_start_ms,
                    generation,
                    public_key,
                    not_after_ms,
                } => {
                    return registry_cmd::publish_compliance_key(
                        registry_cmd::ComplianceOperatorOptions {
                            dir: operator.dir.clone(),
                            origin: operator.origin.clone(),
                            key_id: operator.key_id.clone(),
                            confirm_key_id: operator.confirm_key_id.clone(),
                            checkpoint_backup: operator.checkpoint_backup.clone(),
                            witness_timeout_seconds: operator.witness_timeout_seconds,
                            confirm_offline: operator.confirm_offline,
                            execute: operator.execute,
                            json: cli.json,
                        },
                        (*purpose).into(),
                        (*jurisdiction).into(),
                        authority,
                        *epoch_start_ms,
                        *generation,
                        public_key,
                        *not_after_ms,
                    )
                    .await;
                }
                RegistryComplianceKeyAction::Transition { operator, status } => {
                    return registry_cmd::transition_compliance_key(
                        registry_cmd::ComplianceOperatorOptions {
                            dir: operator.dir.clone(),
                            origin: operator.origin.clone(),
                            key_id: operator.key_id.clone(),
                            confirm_key_id: operator.confirm_key_id.clone(),
                            checkpoint_backup: operator.checkpoint_backup.clone(),
                            witness_timeout_seconds: operator.witness_timeout_seconds,
                            confirm_offline: operator.confirm_offline,
                            execute: operator.execute,
                            json: cli.json,
                        },
                        (*status).into(),
                    )
                    .await;
                }
            },
        }
    }

    if let Command::Directory {
        action: DirectoryAction::Serve { bind, dir },
    } = &cli.command
    {
        return directory_cmd::serve(bind, dir).await;
    }

    let home = cli.home.clone().unwrap_or_else(default_home);

    // Read-only commands must not mint keys on someone's machine as a side effect of looking
    // something up, so they run before any identity exists.
    if let Command::Handle { action } = &cli.command {
        match action {
            HandleAction::Resolve { handle, registry } => {
                return handle_cmd::resolve(&home, registry, handle, cli.json).await
            }
            HandleAction::Checkpoint { registry, key } => {
                return handle_cmd::checkpoint(registry, key.as_deref()).await
            }
            HandleAction::Claim { .. } | HandleAction::Rotate { .. } => {} // needs the agent's key
        }
    }

    // Operator commands, like the read-only ones above: they act on a loft, not on an agent, and
    // must not mint keys in someone's home as a side effect.
    if let Command::Loft {
        action:
            LoftAction::Submit {
                directory,
                endpoint,
                dir,
                operator,
            },
    } = &cli.command
    {
        return submit_cmd::submit(dir, directory, endpoint, operator.clone(), cli.json).await;
    }

    if let Command::Loft {
        action:
            LoftAction::Drain {
                directory,
                endpoint,
                dir,
                after,
            },
    } = &cli.command
    {
        return submit_cmd::drain(dir, directory, endpoint, after, cli.json).await;
    }

    if let Command::Loft {
        action:
            LoftAction::Serve {
                bind,
                dir,
                capacity_gb,
                retention_days,
            },
    } = &cli.command
    {
        return loft_cmd::serve(loft_cmd::ServeOptions {
            dir: dir.clone(),
            bind: bind.clone(),
            capacity_gb: *capacity_gb,
            retention_days: *retention_days,
        })
        .await;
    }

    let agent = Agent::open_with_options(
        &home,
        AgentOpenOptions {
            recovery_dir: cli.recovery_dir.clone(),
        },
    )?;

    if agent.freshly_created {
        eprintln!("created a new identity in {}", home.display());
        if agent.successor_shares_a_disk() {
            eprintln!("{}", pigeonpost_client::keystore::SUCCESSOR_WARNING);
        }
    }

    match cli.command {
        Command::Id => {
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "address": agent.address().as_str(),
                        "home": home.display().to_string(),
                        "lofts": agent.lofts()?.iter().map(|(u, _)| u).collect::<Vec<_>>(),
                        "unread": agent.unread_count()?,
                    })
                );
            } else {
                println!("{}", agent.address());
            }
        }

        Command::Rotate { confirm } => {
            // Keep the common agent binding immutable. Identity custody is mutable only inside the
            // one operation which can promote the precommitted successor.
            let mut agent = agent;
            let report = agent.rotate_expected(&confirm).await?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "from": report.from.as_str(),
                        "to": report.to.as_str(),
                        "grace_until": report.grace_until,
                        "published": report.published,
                        "failed": report.failed,
                        "resumed": report.resumed,
                    })
                );
            } else {
                let action = if report.resumed { "resumed" } else { "rotated" };
                println!(
                    "{action} {} to {}; grace until {}; published to {}, failed at {} loft(s)",
                    report.from, report.to, report.grace_until, report.published, report.failed
                );
            }
        }

        Command::Send {
            to,
            body,
            attribution_jurisdiction,
            attribution_authority,
        } => {
            let to = Destination::parse(&to)?;
            let body = read_body(&body)?;
            let report = match attribution_jurisdiction {
                Some(jurisdiction) => {
                    let agreement = send_attribution_requirement_arg(
                        jurisdiction,
                        attribution_authority.as_deref(),
                    )?;
                    agent
                        .send_to_with_attribution_agreement(&to, &body, agreement)
                        .await?
                }
                None if attribution_authority.is_some() => {
                    return Err(
                        "--attribution-authority requires --attribution-jurisdiction".into(),
                    );
                }
                None => agent.send_to(&to, &body).await?,
            };

            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "id": report.message_id,
                        "delivered": report.delivered,
                        "queued": report.queued,
                        "terminal": report.terminal,
                        "deadline_exceeded": report.deadline_exceeded,
                    })
                );
            } else if report.delivered > 0 {
                println!(
                    "pigeonposted {} to {} loft(s)",
                    &report.message_id[..12],
                    report.delivered
                );
                if report.queued > 0 || report.terminal > 0 {
                    eprintln!(
                        "warning: {} copy/copies remain queued; {} require operator attention",
                        report.queued, report.terminal
                    );
                }
            } else if report.terminal > 0 {
                println!(
                    "{} was refused permanently by {} loft(s); inspect `pigeonpost flush --json`",
                    &report.message_id[..12],
                    report.terminal
                );
            } else {
                println!(
                    "queued {} — no loft accepted it yet; retry with `pigeonpost flush`",
                    &report.message_id[..12]
                );
            }
            if report.deadline_exceeded && !cli.json {
                eprintln!(
                    "warning: the send wake-up deadline elapsed; queued copies remain durable"
                );
            }
        }

        Command::Inbox {
            all,
            offline,
            limit,
        } => {
            if !offline {
                match agent.drain().await {
                    Ok(report) => {
                        if !report.lofts_failed.is_empty() && !cli.json {
                            eprintln!("warning: {} loft(s) unreachable", report.lofts_failed.len());
                        }
                        if report.deadline_exceeded && !cli.json {
                            eprintln!("warning: the inbox wake-up deadline elapsed");
                        }
                    }
                    Err(error) => eprintln!("warning: could not drain: {error}"),
                }
            }
            print_inbox(&agent.inbox(!all, limit)?, cli.json);
        }

        Command::Read { id } => print_message(&agent.read(&id)?, cli.json),

        Command::Ack { id } => {
            let message = agent.ack(&id)?;
            if cli.json {
                println!("{}", serde_json::json!({ "id": message.id, "read": true }));
            } else {
                println!("marked {} read", &message.id[..12.min(message.id.len())]);
            }
        }

        Command::Flush => {
            let report = agent.flush().await?;
            let dead_letters = agent.dead_letters(MAX_DEAD_LETTER_OUTPUT)?;
            let dead_letters_truncated = report.dead_letters > dead_letters.len() as u64;
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "attempted": report.attempted,
                        "delivered": report.delivered,
                        "retryable": report.retryable,
                        "terminalized": report.terminal,
                        "cancelled": report.cancelled,
                        "queued": report.queued,
                        "dead_letter_count": report.dead_letters,
                        "deadline_exceeded": report.deadline_exceeded,
                        "dead_letters_truncated": dead_letters_truncated,
                        "dead_letters": dead_letters.iter().map(|letter| serde_json::json!({
                            "row": letter.row.get().to_string(),
                            "id": letter.message_id,
                            "to": letter.to_addr,
                            "loft": letter.loft_url,
                            "attempts": letter.attempts,
                            "reason": letter.reason,
                            "terminal_at": letter.terminal_at,
                        })).collect::<Vec<_>>(),
                    })
                );
            } else {
                println!(
                    "delivered {}, {} still queued, {} require operator attention",
                    report.delivered, report.queued, report.dead_letters
                );
                for letter in dead_letters {
                    println!(
                        "terminal row {} message {} to {} via {} after {} attempt(s): {}",
                        letter.row.get(),
                        &letter.message_id[..12.min(letter.message_id.len())],
                        letter.to_addr,
                        letter.loft_url,
                        letter.attempts,
                        letter.reason
                    );
                }
                if dead_letters_truncated {
                    println!("additional terminal copies omitted from this bounded view");
                }
                if report.deadline_exceeded {
                    eprintln!("warning: the flush wake-up deadline elapsed");
                }
            }
        }

        Command::Storage { action } => match action {
            StorageAction::Status => {
                print_storage_status(&agent.storage_status()?, false, cli.json)
            }
            StorageAction::SetLimits {
                inbox_messages,
                inbox_body_bytes,
                outbox_rows,
                outbox_payload_bytes,
            } => {
                let status = agent.set_storage_limits(StorageLimits {
                    inbox_messages,
                    inbox_body_bytes,
                    outbox_rows,
                    outbox_payload_bytes,
                })?;
                print_storage_status(&status, true, cli.json);
            }
            StorageAction::Pending { limit } => {
                print_pending_deliveries(&agent.pending_deliveries(limit)?, limit, cli.json)
            }
            StorageAction::Completed { limit } => {
                print_completed_deliveries(&agent.completed_deliveries(limit)?, limit, cli.json)
            }
            StorageAction::DeadLetters { limit } => {
                print_dead_letters(&agent.dead_letters(limit)?, limit, cli.json)
            }
            StorageAction::DeleteCompleted { row } => print_row_deletion(
                "completed delivery",
                row,
                agent.delete_completed_delivery(row)?,
                cli.json,
            ),
            StorageAction::DeleteDeadLetter { row } => {
                print_row_deletion("dead letter", row, agent.delete_dead_letter(row)?, cli.json)
            }
            StorageAction::DeletePending { row, confirm } => print_row_deletion(
                "pending delivery",
                row,
                agent.delete_pending_outbox(row, &confirm)?,
                cli.json,
            ),
            StorageAction::DeleteMessage { id, .. } => {
                let body_erased = agent.delete_message(&id)?;
                if cli.json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "id": id,
                            "body_erased": body_erased,
                            "tombstone_retained": body_erased.then_some(true),
                        })
                    );
                } else if body_erased {
                    println!("message {id}: body erased; deletion tombstone retained");
                } else {
                    println!("message {id}: not found or already erased");
                }
            }
            StorageAction::PruneFinished {
                before,
                limit,
                confirm,
            } => {
                let pruned = agent.prune_finished_outbox(before, limit, &confirm)?;
                if cli.json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "before": before,
                            "limit": limit,
                            "pruned": pruned,
                        })
                    );
                } else {
                    println!("pruned {pruned} finished delivery metadata row(s)");
                }
            }
        },

        Command::Pending { limit } => print_inbox(&agent.pending(limit)?, cli.json),

        Command::Allow { address } => {
            let resolution = agent.resolve(&Address::parse(&address)?).await?;
            let released = agent.allow_sender(&resolution.pubkey, "allowed by operator")?;
            println!("allowed {address}; released {released} held message(s)");
        }

        Command::Block { address } => {
            let resolution = agent.resolve(&Address::parse(&address)?).await?;
            let score = agent.block_sender(&resolution.pubkey)?;
            println!("blocked {address} (score {score})");
        }

        Command::Spam { id } => {
            let score = agent.mark_spam(&id)?;
            println!("flagged; sender score is now {score}");
            if score <= pigeonpost_client::spam::DROP_THRESHOLD {
                println!("this sender's messages are now dropped on arrival");
            }
        }

        Command::PowFloor { bits } => {
            agent.set_pow_floor(bits).await?;
            println!("unsolicited Pigeonpost messages must now carry {bits} bits of work");
        }

        Command::AcceptAll { value } => {
            agent.set_accept_all(value)?;
            if value {
                println!("inbox open: messages from strangers go straight to the inbox");
            } else {
                println!("inbox closed: messages from strangers are held for review");
            }
        }

        Command::Token { action } => match action {
            TokenAction::Mint { label } => {
                agent.publish_token(&label).await?;
                let token = pigeonpost_core::Token::mint(&agent.token_secret()?, &label);
                println!("{}#t={}", agent.address(), token.to_hex());
                println!();
                println!("Publish that address. Revoke with: pigeonpost token revoke {label}");
            }
            TokenAction::Revoke { label } => {
                agent.revoke_token(&label).await?;
                println!("revoked {label}");
            }
            TokenAction::List => {
                let labels = agent.token_labels()?;
                if labels.is_empty() {
                    println!("no live tokens");
                } else {
                    for label in labels {
                        println!("{label}");
                    }
                }
            }
        },

        Command::Attribution { action } => {
            match action {
                AttributionAction::Status => {}
                AttributionAction::Recipient {
                    jurisdiction,
                    authority,
                } => {
                    let requirement =
                        attribution_requirement_arg(jurisdiction, authority.as_deref())?;
                    agent.set_attribution_requirement(requirement).await?;
                }
                AttributionAction::Sender {
                    jurisdiction,
                    authority,
                } => {
                    let requirement =
                        attribution_requirement_arg(jurisdiction, authority.as_deref())?;
                    agent.set_sender_attribution_requirement(requirement)?;
                }
            }
            print_attribution_status(&agent, cli.json)?;
        }

        Command::RegistryTrust { action } => match action {
            RegistryTrustAction::Import { file } => trust_cmd::import(&agent, &file, cli.json)?,
            RegistryTrustAction::Status => trust_cmd::status(&agent, cli.json)?,
            RegistryTrustAction::Reset { confirm } => trust_cmd::reset(&agent, &confirm, cli.json)?,
        },

        Command::Mcp => pigeonpost_mcp::serve_stdio(agent).await?,

        Command::Loft { action } => match action {
            LoftAction::Add { url } => {
                agent.add_loft(&url).await?;
                println!("using {url}");
            }
            LoftAction::Remove { url } => {
                if agent.remove_loft(&url).await? {
                    println!("draining {url} for the replacement grace window");
                } else {
                    println!("{url} was not in use");
                }
            }
            LoftAction::List => {
                let lofts = agent.lofts()?;
                if cli.json {
                    println!(
                        "{}",
                        serde_json::json!(lofts.iter().map(|(u, _)| u).collect::<Vec<_>>())
                    );
                } else if lofts.is_empty() {
                    println!("no lofts configured — add one with `pigeonpost loft add <url>`");
                } else {
                    for (url, _) in lofts {
                        println!("{url}");
                    }
                }
            }
            LoftAction::Submit { .. } | LoftAction::Drain { .. } | LoftAction::Serve { .. } => {
                unreachable!("handled before identity setup")
            }
        },

        Command::Handle { action } => match action {
            HandleAction::Claim {
                handle,
                registry,
                mock_name,
                no_browser,
            } => {
                handle_cmd::claim(
                    &agent,
                    &registry,
                    &handle,
                    handle_cmd::ClaimProof {
                        mock_name,
                        no_browser,
                    },
                    cli.json,
                )
                .await?
            }
            HandleAction::Rotate {
                handle,
                registry,
                mock_name,
                no_browser,
            } => {
                handle_cmd::rotate(
                    &agent,
                    &registry,
                    &handle,
                    handle_cmd::ClaimProof {
                        mock_name,
                        no_browser,
                    },
                    cli.json,
                )
                .await?
            }
            HandleAction::Resolve { .. } | HandleAction::Checkpoint { .. } => {
                unreachable!("handled before identity setup")
            }
        },

        Command::Directory { action } => match action {
            DirectoryAction::Add { url, key } => {
                let key = pigeonpost_directory::entry::parse_hex32(&key)
                    .ok_or("directory key must be exactly 32 hex bytes")?;
                let lofts = agent.add_directory(&url, key).await?;
                println!("trusted signed directory snapshot with {lofts} loft(s)");
            }
            DirectoryAction::Remove { url, .. } => {
                let removed = agent.remove_directory(&url)?;
                if cli.json {
                    println!("{}", serde_json::json!({ "url": url, "removed": removed }));
                } else if removed {
                    println!("removed trusted directory {url}");
                } else {
                    println!("trusted directory {url} was not configured");
                }
            }
            DirectoryAction::Refresh => {
                let refreshed = agent.refresh_directories().await?;
                println!("refreshed {refreshed} signed directory snapshot(s)");
            }
            DirectoryAction::Bootstrap => {
                let added = agent.bootstrap_lofts().await?;
                println!("selected {added} new loft(s)");
            }
            DirectoryAction::List => {
                for directory in agent.state().directories()? {
                    println!(
                        "{} {}",
                        directory.url,
                        pigeonpost_directory::entry::hex(&directory.signing_key)
                    );
                }
            }
            DirectoryAction::Serve { .. } => unreachable!("handled before identity setup"),
        },

        Command::Registry { .. } | Command::Install { .. } => {
            unreachable!("handled before identity setup")
        }
    }

    Ok(())
}

fn print_attribution_status(agent: &Agent, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let recipient = agent.attribution_requirement()?;
    let sender = agent.sender_attribution_requirement()?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "recipient_required": recipient.is_some(),
                "recipient_requirement": requirement_json(recipient),
                "sender_requirement": requirement_json(sender),
            })
        );
    } else {
        println!("recipient attribution: {}", requirement_text(recipient));
        println!("sender attribution: {}", requirement_text(sender));
    }
    Ok(())
}

fn attribution_requirement_arg(
    jurisdiction: AttributionJurisdictionArg,
    authority: Option<&str>,
) -> Result<Option<AttributionRequirement>, Box<dyn std::error::Error>> {
    attribution_requirement_arg_with_flag(jurisdiction, authority, "--authority")
}

fn send_attribution_requirement_arg(
    jurisdiction: AttributionJurisdictionArg,
    authority: Option<&str>,
) -> Result<Option<AttributionRequirement>, Box<dyn std::error::Error>> {
    attribution_requirement_arg_with_flag(jurisdiction, authority, "--attribution-authority")
}

fn attribution_requirement_arg_with_flag(
    jurisdiction: AttributionJurisdictionArg,
    authority: Option<&str>,
    authority_flag: &str,
) -> Result<Option<AttributionRequirement>, Box<dyn std::error::Error>> {
    let Some(jurisdiction) = Option::<Jurisdiction>::from(jurisdiction) else {
        if authority.is_some() {
            return Err(format!("{authority_flag} is invalid when attribution is off").into());
        }
        return Ok(None);
    };
    let authority = authority
        .ok_or_else(|| format!("{authority_flag} is required when attribution is enabled"))?;
    if !authority
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!("{authority_flag} must be 64 lowercase hexadecimal characters").into());
    }
    let authority = pigeonpost_directory::entry::parse_hex32(authority)
        .ok_or_else(|| format!("{authority_flag} must be 64 lowercase hexadecimal characters"))?;
    let requirement = AttributionRequirement::new(jurisdiction, authority);
    requirement.validate()?;
    Ok(Some(requirement))
}

fn requirement_json(requirement: Option<AttributionRequirement>) -> serde_json::Value {
    requirement.map_or(serde_json::Value::Null, |requirement| {
        serde_json::json!({
            "version": requirement.version,
            "jurisdiction": jurisdiction_name(requirement.jurisdiction),
            "authority": pigeonpost_directory::entry::hex(&requirement.authority),
        })
    })
}

fn requirement_text(requirement: Option<AttributionRequirement>) -> String {
    requirement.map_or_else(
        || "off".to_owned(),
        |requirement| {
            format!(
                "{} authority {}",
                jurisdiction_name(requirement.jurisdiction),
                pigeonpost_directory::entry::hex(&requirement.authority)
            )
        },
    )
}

fn jurisdiction_name(jurisdiction: Jurisdiction) -> &'static str {
    match jurisdiction {
        Jurisdiction::Us => "us",
        Jurisdiction::Eu => "eu",
        Jurisdiction::Tr => "tr",
        Jurisdiction::Test => "test",
    }
}

fn print_storage_status(status: &StorageStatus, updated: bool, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "updated": updated,
                "limits": {
                    "inbox_messages": status.limits.inbox_messages,
                    "inbox_tombstones": status.inbox_tombstone_limit,
                    "inbox_body_bytes": status.limits.inbox_body_bytes,
                    "outbox_rows": status.limits.outbox_rows,
                    "outbox_payload_bytes": status.limits.outbox_payload_bytes,
                },
                "usage": {
                    "inbox_messages": status.usage.inbox_messages,
                    "inbox_tombstones": status.usage.inbox_tombstones,
                    "inbox_body_bytes": status.usage.inbox_body_bytes,
                    "outbox_rows": status.usage.outbox_rows,
                    "outbox_payload_bytes": status.usage.outbox_payload_bytes,
                },
            })
        );
    } else {
        if updated {
            println!("storage limits updated");
        }
        println!(
            "inbox messages: {} / {}; deletion tombstones: {} / {}; inbox body bytes: {} / {}",
            status.usage.inbox_messages,
            status.limits.inbox_messages,
            status.usage.inbox_tombstones,
            status.inbox_tombstone_limit,
            status.usage.inbox_body_bytes,
            status.limits.inbox_body_bytes
        );
        println!(
            "outbox rows: {} / {}; outbox payload bytes: {} / {}",
            status.usage.outbox_rows,
            status.limits.outbox_rows,
            status.usage.outbox_payload_bytes,
            status.limits.outbox_payload_bytes
        );
    }
}

fn print_pending_deliveries(
    deliveries: &[pigeonpost_client::state::PendingDelivery],
    limit: usize,
    json: bool,
) {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "deliveries": deliveries.iter().map(|delivery| serde_json::json!({
                    "row": delivery.row.get().to_string(),
                    "message_id": delivery.message_id,
                    "to": delivery.to_addr,
                    "loft": delivery.loft_url,
                    "attempts": delivery.attempts,
                    "created_at": delivery.created_at,
                    "next_attempt_at": delivery.next_attempt_at,
                    "last_error": delivery.last_error,
                })).collect::<Vec<_>>(),
                "returned": deliveries.len(),
                "limit": limit,
            })
        );
    } else {
        for delivery in deliveries {
            println!(
                "pending row {} message {} to {} via {} after {} attempt(s); created {}, next attempt {}, last error {}",
                delivery.row.get(),
                delivery.message_id,
                delivery.to_addr,
                delivery.loft_url,
                delivery.attempts,
                delivery.created_at,
                delivery.next_attempt_at,
                delivery.last_error.as_deref().unwrap_or("none")
            );
        }
    }
}

fn print_completed_deliveries(
    deliveries: &[pigeonpost_client::CompletedDelivery],
    limit: usize,
    json: bool,
) {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "deliveries": deliveries.iter().map(|delivery| serde_json::json!({
                    "row": delivery.row.get().to_string(),
                    "message_id": delivery.message_id,
                    "to": delivery.to_addr,
                    "loft": delivery.loft_url,
                    "attempts": delivery.attempts,
                    "sent_at": delivery.sent_at,
                })).collect::<Vec<_>>(),
                "returned": deliveries.len(),
                "limit": limit,
            })
        );
    } else {
        for delivery in deliveries {
            println!(
                "completed row {} message {} to {} via {} after {} attempt(s) at {}",
                delivery.row.get(),
                delivery.message_id,
                delivery.to_addr,
                delivery.loft_url,
                delivery.attempts,
                delivery.sent_at
            );
        }
    }
}

fn print_dead_letters(deliveries: &[pigeonpost_client::DeadLetter], limit: usize, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "deliveries": deliveries.iter().map(|delivery| serde_json::json!({
                    "row": delivery.row.get().to_string(),
                    "message_id": delivery.message_id,
                    "to": delivery.to_addr,
                    "loft": delivery.loft_url,
                    "attempts": delivery.attempts,
                    "reason": delivery.reason,
                    "terminal_at": delivery.terminal_at,
                })).collect::<Vec<_>>(),
                "returned": deliveries.len(),
                "limit": limit,
            })
        );
    } else {
        for delivery in deliveries {
            println!(
                "dead-letter row {} message {} to {} via {} after {} attempt(s) at {}: {}",
                delivery.row.get(),
                delivery.message_id,
                delivery.to_addr,
                delivery.loft_url,
                delivery.attempts,
                delivery.terminal_at,
                delivery.reason
            );
        }
    }
}

fn print_row_deletion(kind: &str, row: OutboxRecordId, deleted: bool, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::json!({ "row": row.get().to_string(), "deleted": deleted })
        );
    } else {
        println!(
            "{kind} row {}: {}",
            row.get(),
            if deleted { "deleted" } else { "not found" }
        );
    }
}

/// `-` means stdin, so bodies can be piped without shell-quoting them.
fn read_body(arg: &str) -> std::io::Result<String> {
    if arg == "-" {
        read_body_from(std::io::stdin().lock())
    } else {
        validate_body_bytes(arg.as_bytes())
    }
}

fn read_body_from(reader: impl std::io::Read) -> std::io::Result<String> {
    use std::io::Read;

    let limit = pigeonpost_core::envelope::MAX_PLAINTEXT;
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    reader.take((limit + 1) as u64).read_to_end(&mut bytes)?;
    validate_body_bytes(&bytes)
}

fn validate_body_bytes(bytes: &[u8]) -> std::io::Result<String> {
    if bytes.len() > pigeonpost_core::envelope::MAX_PLAINTEXT {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "message exceeds the maximum plaintext size",
        ));
    }

    let body = std::str::from_utf8(bytes).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "message body must be valid UTF-8",
        )
    })?;
    Ok(body.trim_end().to_string())
}

fn default_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".pigeonpost")
}

fn parse_address(value: &str) -> Result<Address, String> {
    Address::parse(value).map_err(|_| "confirmation must be an exact /k/<address>".into())
}

fn parse_storage_list_limit(value: &str) -> Result<usize, String> {
    let limit = value
        .parse::<usize>()
        .map_err(|_| "limit must be a positive integer".to_string())?;
    if !(1..=MAX_STORAGE_LIST_RESULTS).contains(&limit) {
        return Err(format!(
            "limit must be between 1 and {MAX_STORAGE_LIST_RESULTS}"
        ));
    }
    Ok(limit)
}

fn parse_outbox_row_id(value: &str) -> Result<OutboxRecordId, String> {
    if value.starts_with('0') || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("row must be a canonical positive decimal SQLite row id".into());
    }
    let row = value
        .parse::<i64>()
        .map_err(|_| "row is outside the SQLite row-id range".to_string())?;
    OutboxRecordId::new(row)
        .map_err(|_| "row must be a canonical positive decimal SQLite row id".into())
}

fn parse_pending_delete_confirmation(value: &str) -> Result<String, String> {
    if value != PENDING_OUTBOX_DELETE_CONFIRMATION {
        return Err(format!(
            "confirmation must exactly equal {PENDING_OUTBOX_DELETE_CONFIRMATION}"
        ));
    }
    Ok(value.into())
}

fn parse_prune_finished_confirmation(value: &str) -> Result<String, String> {
    if value != FINISHED_OUTBOX_PRUNE_CONFIRMATION {
        return Err(format!(
            "confirmation must exactly equal {FINISHED_OUTBOX_PRUNE_CONFIRMATION}"
        ));
    }
    Ok(value.into())
}

fn parse_canonical_directory_url(value: &str) -> Result<String, String> {
    let canonical = pigeonpost_directory::canonical_directory_url(value)
        .map_err(|_| "directory URL is malformed".to_string())?;
    if value != canonical {
        return Err(
            "directory URL must use its canonical origin spelling without a trailing slash".into(),
        );
    }
    Ok(canonical)
}

fn parse_message_id(value: &str) -> Result<String, String> {
    if value.is_empty() || value.len() > 128 {
        return Err("message id is outside the allowed length".into());
    }
    Ok(value.into())
}

fn parse_bounded_storage_limit(value: &str, maximum: u64, name: &str) -> Result<u64, String> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| format!("{name} must be an integer"))?;
    if parsed == 0 || parsed > maximum {
        return Err(format!("{name} must be between 1 and {maximum}"));
    }
    Ok(parsed)
}

fn parse_inbox_message_limit(value: &str) -> Result<u64, String> {
    parse_bounded_storage_limit(value, MAX_INBOX_MESSAGE_LIMIT, "inbox-messages")
}

fn parse_inbox_body_bytes_limit(value: &str) -> Result<u64, String> {
    parse_bounded_storage_limit(value, MAX_INBOX_BODY_BYTES_LIMIT, "inbox-body-bytes")
}

fn parse_outbox_row_limit(value: &str) -> Result<u64, String> {
    parse_bounded_storage_limit(value, MAX_OUTBOX_ROW_LIMIT, "outbox-rows")
}

fn parse_outbox_payload_bytes_limit(value: &str) -> Result<u64, String> {
    parse_bounded_storage_limit(
        value,
        MAX_OUTBOX_PAYLOAD_BYTES_LIMIT,
        "outbox-payload-bytes",
    )
}

fn parse_before_timestamp(value: &str) -> Result<u64, String> {
    let before = value
        .parse::<u64>()
        .map_err(|_| "before must be a Unix timestamp".to_string())?;
    if before > i64::MAX as u64 {
        return Err("before is outside the SQLite timestamp range".into());
    }
    Ok(before)
}

fn validate_preopen_confirmations(command: &Command) -> Result<(), String> {
    match command {
        Command::Storage {
            action: StorageAction::DeleteMessage { id, confirm },
        } if id != confirm => {
            return Err("message deletion confirmation must exactly match the message id".into())
        }
        Command::Directory {
            action: DirectoryAction::Remove { url, confirm },
        } if url != confirm => {
            return Err(
                "directory removal confirmation must exactly match the canonical URL".into(),
            )
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod input_tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn stdin_body_is_bounded_before_allocation_can_grow_without_limit() {
        let input = vec![b'x'; pigeonpost_core::envelope::MAX_PLAINTEXT + 1];
        let error = read_body_from(input.as_slice()).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn exact_limit_and_trailing_newlines_are_supported() {
        let input = vec![b'x'; pigeonpost_core::envelope::MAX_PLAINTEXT];
        assert_eq!(read_body_from(input.as_slice()).unwrap().len(), input.len());
        assert_eq!(read_body_from(b"hello\n\n".as_slice()).unwrap(), "hello");
    }

    #[test]
    fn invalid_utf8_is_rejected() {
        let error = read_body_from([0xff].as_slice()).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn recovery_directory_is_a_global_argument_with_a_stable_environment_binding() {
        let parsed = Cli::try_parse_from([
            "pigeonpost",
            "--recovery-dir",
            "/private/pigeonpost-recovery",
            "id",
        ])
        .unwrap();
        assert_eq!(
            parsed.recovery_dir,
            Some(PathBuf::from("/private/pigeonpost-recovery"))
        );

        let command = Cli::command();
        let recovery = command
            .get_arguments()
            .find(|argument| argument.get_id() == "recovery_dir")
            .expect("global recovery-dir argument");
        assert_eq!(
            recovery.get_env(),
            Some(std::ffi::OsStr::new("PIGEONPOST_RECOVERY_DIR"))
        );
        assert_eq!(
            Cli::try_parse_from([
                "pigeonpost",
                "id",
                "--recovery-dir",
                "/private/pigeonpost-recovery",
            ])
            .unwrap()
            .recovery_dir,
            parsed.recovery_dir
        );
    }

    #[test]
    fn log_verbosity_is_scoped_to_first_party_targets() {
        let directives = pigeonpost_log_directives(Some("trace"));
        assert!(directives.starts_with("off,"));
        assert!(directives.contains("pigeonpost_loft=trace"));
        assert!(directives.contains("pigeonpost_registry=trace"));
        assert!(!directives.contains("axum"));
        assert!(!directives.contains("hyper"));
    }

    #[test]
    fn arbitrary_log_target_directives_cannot_reenable_request_diagnostics() {
        let directives = pigeonpost_log_directives(Some("trace,axum=trace"));
        assert!(directives.contains("pigeonpost=warn"));
        assert!(!directives.contains("axum"));
    }

    #[test]
    fn credential_sentinel_never_enters_the_cli_argv_contract() {
        let help = Cli::command().render_long_help().to_string();
        for forbidden in [
            "--github-code",
            "--github-code-verifier",
            "--github-state",
            "--google-token",
            "--google-nonce",
        ] {
            assert!(
                !help.contains(forbidden),
                "credential flag leaked into help: {forbidden}"
            );
        }

        let parsed = Cli::try_parse_from([
            "pigeonpost",
            "handle",
            "claim",
            "/github/alice",
            "--registry",
            "https://registry.example",
            "--github-code",
            "ARGV-CREDENTIAL-SENTINEL",
        ]);
        assert!(parsed.is_err(), "credential-bearing argv must be rejected");

        let rotated = Cli::try_parse_from([
            "pigeonpost",
            "handle",
            "rotate",
            "/github/alice",
            "--registry",
            "https://registry.example",
            "--no-browser",
        ])
        .unwrap();
        assert!(matches!(
            rotated.command,
            Command::Handle {
                action: HandleAction::Rotate {
                    no_browser: true,
                    ..
                }
            }
        ));
        assert!(Cli::try_parse_from([
            "pigeonpost",
            "handle",
            "rotate",
            "/github/alice",
            "--registry",
            "https://registry.example",
            "--google-token",
            "ARGV-CREDENTIAL-SENTINEL",
        ])
        .is_err());
    }

    #[test]
    fn rotate_requires_one_exact_key_address_confirmation() {
        let address =
            Address::from_pubkey(&pigeonpost_core::Identity::from_seed([7; 32]).verifying_key());
        let parsed =
            Cli::try_parse_from(["pigeonpost", "rotate", "--confirm", address.as_str()]).unwrap();
        assert!(matches!(
            parsed.command,
            Command::Rotate { confirm } if confirm == address
        ));
        assert!(Cli::try_parse_from(["pigeonpost", "rotate"]).is_err());
        assert!(
            Cli::try_parse_from(["pigeonpost", "rotate", "--confirm", "/github/alice",]).is_err()
        );
        assert!(
            Cli::try_parse_from(["pigeonpost", "rotate", "--confirm", "not-an-address",]).is_err()
        );
    }

    #[test]
    fn storage_commands_are_bounded_and_require_exact_confirmations() {
        let limits = Cli::try_parse_from([
            "pigeonpost",
            "storage",
            "set-limits",
            "--inbox-messages",
            "100",
            "--inbox-body-bytes",
            "1000",
            "--outbox-rows",
            "200",
            "--outbox-payload-bytes",
            "2000",
        ])
        .unwrap();
        assert!(matches!(
            limits.command,
            Command::Storage {
                action: StorageAction::SetLimits {
                    inbox_messages: 100,
                    inbox_body_bytes: 1000,
                    outbox_rows: 200,
                    outbox_payload_bytes: 2000,
                }
            }
        ));
        assert!(Cli::try_parse_from([
            "pigeonpost",
            "storage",
            "set-limits",
            "--inbox-messages",
            "100",
        ])
        .is_err());
        assert!(
            Cli::try_parse_from(["pigeonpost", "storage", "pending", "--limit", "1001",]).is_err()
        );

        let high = "9007199254740993";
        let deletion = Cli::try_parse_from([
            "pigeonpost",
            "storage",
            "delete-pending",
            high,
            "--confirm",
            PENDING_OUTBOX_DELETE_CONFIRMATION,
        ])
        .unwrap();
        let row = match deletion.command {
            Command::Storage {
                action: StorageAction::DeletePending { row, .. },
            } => row,
            _ => panic!("wrong command"),
        };
        assert_eq!(row.get().to_string(), high);
        assert_eq!(
            serde_json::json!({ "row": row.get().to_string() })["row"],
            high
        );
        assert!(Cli::try_parse_from([
            "pigeonpost",
            "storage",
            "delete-pending",
            "01",
            "--confirm",
            PENDING_OUTBOX_DELETE_CONFIRMATION,
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "pigeonpost",
            "storage",
            "delete-pending",
            "1",
            "--confirm",
            "delete it",
        ])
        .is_err());

        let mismatched = Cli::try_parse_from([
            "pigeonpost",
            "storage",
            "delete-message",
            "message-id",
            "--confirm",
            "other-id",
        ])
        .unwrap();
        assert!(validate_preopen_confirmations(&mismatched.command).is_err());
        let matched = Cli::try_parse_from([
            "pigeonpost",
            "storage",
            "delete-message",
            "message-id",
            "--confirm",
            "message-id",
        ])
        .unwrap();
        assert!(validate_preopen_confirmations(&matched.command).is_ok());

        assert!(Cli::try_parse_from([
            "pigeonpost",
            "storage",
            "prune-finished",
            "--before",
            "100",
            "--limit",
            "10",
            "--confirm",
            FINISHED_OUTBOX_PRUNE_CONFIRMATION,
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "pigeonpost",
            "storage",
            "prune-finished",
            "--before",
            "100",
            "--limit",
            "10",
            "--confirm",
            "prune",
        ])
        .is_err());
    }

    #[test]
    fn directory_removal_requires_canonical_url_and_exact_preopen_confirmation() {
        assert!(Cli::try_parse_from([
            "pigeonpost",
            "directory",
            "remove",
            "https://directory.example/",
            "--confirm",
            "https://directory.example",
        ])
        .is_err());

        let mismatch = Cli::try_parse_from([
            "pigeonpost",
            "directory",
            "remove",
            "https://directory.example",
            "--confirm",
            "https://other.example",
        ])
        .unwrap();
        assert!(validate_preopen_confirmations(&mismatch.command).is_err());

        let exact = Cli::try_parse_from([
            "pigeonpost",
            "directory",
            "remove",
            "https://directory.example",
            "--confirm",
            "https://directory.example",
        ])
        .unwrap();
        assert!(validate_preopen_confirmations(&exact.command).is_ok());
    }

    #[test]
    fn loft_drain_requires_an_explicit_absolute_deadline() {
        let parsed = Cli::try_parse_from([
            "pigeonpost",
            "loft",
            "drain",
            "--directory",
            "https://directory.example",
            "--endpoint",
            "https://loft.example",
            "--after",
            "2030-01-01T00:00:00Z",
        ])
        .unwrap();
        assert!(matches!(
            parsed.command,
            Command::Loft {
                action: LoftAction::Drain { .. }
            }
        ));

        assert!(Cli::try_parse_from([
            "pigeonpost",
            "loft",
            "drain",
            "--directory",
            "https://directory.example",
            "--endpoint",
            "https://loft.example",
        ])
        .is_err());
    }

    #[test]
    fn attribution_and_registry_trust_commands_are_closed_and_explicit() {
        let authority = "a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5";
        let sender = Cli::try_parse_from([
            "pigeonpost",
            "attribution",
            "sender",
            "eu",
            "--authority",
            authority,
        ])
        .expect("documented sender jurisdiction parses");
        assert!(matches!(
            sender.command,
            Command::Attribution {
                action: AttributionAction::Sender {
                    jurisdiction: AttributionJurisdictionArg::Eu,
                    authority: Some(_),
                }
            }
        ));

        let recipient = Cli::try_parse_from([
            "pigeonpost",
            "attribution",
            "recipient",
            "test",
            "--authority",
            authority,
        ])
        .expect("documented recipient policy parses");
        assert!(matches!(
            recipient.command,
            Command::Attribution {
                action: AttributionAction::Recipient {
                    jurisdiction: AttributionJurisdictionArg::Test,
                    authority: Some(_),
                }
            }
        ));

        let reset = Cli::try_parse_from([
            "pigeonpost",
            "registry-trust",
            "reset",
            "--confirm",
            pigeonpost_client::REGISTRY_TRUST_RESET_CONFIRMATION,
        ])
        .expect("explicit reset confirmation parses");
        assert!(matches!(
            reset.command,
            Command::RegistryTrust {
                action: RegistryTrustAction::Reset { .. }
            }
        ));
        assert!(Cli::try_parse_from(["pigeonpost", "registry-trust", "import"]).is_err());
        assert!(Cli::try_parse_from(["pigeonpost", "attribution", "sender", "unknown"]).is_err());
        assert!(attribution_requirement_arg(AttributionJurisdictionArg::Eu, None).is_err());
        assert!(
            attribution_requirement_arg(AttributionJurisdictionArg::Off, Some(authority)).is_err()
        );
        assert!(attribution_requirement_arg(
            AttributionJurisdictionArg::Eu,
            Some("0000000000000000000000000000000000000000000000000000000000000000")
        )
        .is_err());
        assert!(
            send_attribution_requirement_arg(AttributionJurisdictionArg::Eu, None)
                .unwrap_err()
                .to_string()
                .contains("--attribution-authority")
        );

        let call_local = Cli::try_parse_from([
            "pigeonpost",
            "send",
            "/k/j5pxq82nf4wt3h9m6rbdck0syv",
            "--body",
            "scope-pinned",
            "--attribution-jurisdiction",
            "eu",
            "--attribution-authority",
            authority,
        ])
        .expect("send accepts an exact call-local attribution agreement");
        assert!(matches!(
            call_local.command,
            Command::Send {
                attribution_jurisdiction: Some(AttributionJurisdictionArg::Eu),
                attribution_authority: Some(_),
                ..
            }
        ));
    }
}
