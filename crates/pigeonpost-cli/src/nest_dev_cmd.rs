//! `pigeonpost nest …` for Nest publishers: start an app from a template and run it locally.
//!
//! ```text
//! pigeonpost nest templates
//! pigeonpost nest init <template> <dir> [--lang node|python] [--name <name>] [--publisher <p>]
//! pigeonpost nest run <dir> [--job job.json]… [--config config.json] [--approve-all] [-- cmd…]
//! ```
//!
//! `run` is the local test environment: the app runs as a plain process (no sandbox) against the
//! same file contract the marketplace dispatcher uses (`jobs/in`, `jobs/out`, `run/broker-token`,
//! `NEST_BROKER_URL`), with a mock broker on 127.0.0.1 standing in for the model broker. Nothing
//! here needs an identity, a home directory or the network, so it runs before any of them.
//!
//! Checking a release for submission (`pigeonpost nest validate`) and the publisher commands
//! (`submit`, `status`) belong to the publisher programme and are not part of this module.

mod manifest;
mod mock_broker;

use clap::{Args, Subcommand};
use manifest::{JobFile, Manifest};
use mock_broker::Mock;
use pigeonpost_nest_kit::{Lang, TEMPLATES};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};

type Error = Box<dyn std::error::Error>;

/// Largest result file read back, as the dispatcher reads it.
const MAX_RESULT_BYTES: u64 = 512 * 1024;
/// Deliveries of one job before the emulator stops approving and redelivering.
const MAX_ATTEMPTS: usize = 4;

#[derive(Args)]
pub struct NestArgs {
    #[command(subcommand)]
    pub action: NestAction,
}

#[derive(Subcommand)]
pub enum NestAction {
    /// List the starter templates.
    Templates,
    /// Create a Nest app from a starter template.
    Init {
        /// Template name (see `pigeonpost nest templates`).
        template: String,
        /// New directory for the app; must not exist or be empty.
        dir: PathBuf,
        /// node or python.
        #[arg(long, default_value = "node")]
        lang: String,
        /// The release name (1-64 of a-z 0-9 -). Defaults to the directory's name.
        #[arg(long)]
        name: Option<String>,
        /// Your publisher name, the first part of the listing address.
        #[arg(long)]
        publisher: Option<String>,
    },
    /// Run a Nest app locally against a mock broker and print what it does.
    Run(RunArgs),
    /// Check a package directory (`nest-release.toml`, `Containerfile`) against the marketplace's
    /// package rules, the same ones the postbox applies. Exits non-zero on any finding.
    Validate {
        /// The package directory.
        dir: PathBuf,
        /// Also check an example installation configuration (a JSON file) against the package's
        /// `config_schema`.
        #[arg(long)]
        config: Option<PathBuf>,
    },
}

#[derive(Args)]
pub struct RunArgs {
    /// The app's directory (holding nest-release.toml).
    pub dir: PathBuf,
    /// A job file ({operation, operation_version, payload}); repeatable. Default: jobs/example.json.
    #[arg(long = "job")]
    pub jobs: Vec<PathBuf>,
    /// The installation's configuration (JSON), checked against the manifest's `config_schema`.
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Approve the effect with this idempotency key when the app asks for it; repeatable.
    #[arg(long = "approve")]
    pub approve: Vec<String>,
    /// Approve every effect the app asks for.
    #[arg(long)]
    pub approve_all: bool,
    /// Deliver each job a second time after it finishes, as the platform may.
    #[arg(long)]
    pub redeliver: bool,
    /// The mock model's reply (repeat for successive requests). Default: a deterministic echo.
    #[arg(long = "reply")]
    pub replies: Vec<String>,
    /// Answer model requests as a Nest with no AI connected.
    #[arg(long)]
    pub no_model: bool,
    /// Seconds to wait for each result.
    #[arg(long, default_value_t = 30)]
    pub timeout: u64,
    /// Keep the emulated Nest volume (jobs, results, token) and print where it is.
    #[arg(long)]
    pub keep: bool,
    /// How to start the app. Default: `node app.mjs` or `python3 app.py`, whichever exists.
    #[arg(last = true)]
    pub command: Vec<String>,
}

pub async fn run(args: &NestArgs, json: bool) -> Result<(), Error> {
    match &args.action {
        NestAction::Templates => {
            templates(json);
            Ok(())
        }
        NestAction::Init {
            template,
            dir,
            lang,
            name,
            publisher,
        } => init(
            template,
            dir,
            lang,
            name.as_deref(),
            publisher.as_deref(),
            json,
        ),
        NestAction::Run(r) => run_app(r, json).await,
        NestAction::Validate { dir, config } => {
            crate::nest_cmd::validate(dir, config.as_deref(), json)
        }
    }
}

fn templates(json: bool) {
    if json {
        let list: Vec<Value> = TEMPLATES
            .iter()
            .map(|t| json!({ "name": t.name, "summary": t.summary, "languages": ["node", "python"] }))
            .collect();
        println!("{}", Value::Array(list));
        return;
    }
    for t in TEMPLATES {
        println!("{:<16} {}", t.name, t.summary);
    }
}

/// A release name from a directory name: lower-case, other characters as `-`.
pub fn name_from_dir(dir: &Path) -> String {
    let raw = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut out = String::new();
    for c in raw.chars() {
        let c = if c.is_ascii_lowercase() || c.is_ascii_digit() {
            c
        } else {
            '-'
        };
        if !(c == '-' && out.ends_with('-')) {
            out.push(c);
        }
    }
    out.trim_matches('-').chars().take(64).collect()
}

fn init(
    template: &str,
    dir: &Path,
    lang: &str,
    name: Option<&str>,
    publisher: Option<&str>,
    json: bool,
) -> Result<(), Error> {
    let Some(t) = pigeonpost_nest_kit::template(template) else {
        let names: Vec<&str> = TEMPLATES.iter().map(|t| t.name).collect();
        return Err(format!("no template {template:?}; one of {}", names.join(", ")).into());
    };
    let lang = Lang::parse(lang).ok_or("--lang: node or python")?;
    let name = name
        .map(str::to_string)
        .unwrap_or_else(|| name_from_dir(dir));
    if !pigeonpost_nest_kit::valid_name(&name) {
        return Err(format!("release name {name:?}: 1-64 of a-z 0-9 - (use --name)").into());
    }
    let publisher = publisher.unwrap_or(pigeonpost_nest_kit::PLACEHOLDER_PUBLISHER);
    if !pigeonpost_nest_kit::valid_name(publisher) {
        return Err("--publisher: 1-64 of a-z 0-9 -".into());
    }
    if dir.exists() && std::fs::read_dir(dir)?.next().is_some() {
        return Err(format!("{} exists and is not empty", dir.display()).into());
    }
    let files = pigeonpost_nest_kit::render(t, lang, &name, publisher);
    for (rel, contents) in &files {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, contents)?;
    }
    if json {
        println!(
            "{}",
            json!({
                "dir": dir.display().to_string(),
                "template": t.name,
                "lang": lang.name(),
                "name": name,
                "files": files.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(),
            })
        );
        return Ok(());
    }
    let test = match lang {
        Lang::Node => "node --test app.test.mjs",
        Lang::Python => "python3 -m unittest test_app",
    };
    println!(
        "Created {} from the {} template ({}), release name {name}.",
        dir.display(),
        t.name,
        lang.name()
    );
    println!("Next:");
    println!("  cd {}", dir.display());
    println!("  {test}");
    println!("  pigeonpost nest run .");
    Ok(())
}

// ---- run ----------------------------------------------------------------------------------------

struct PreparedJob {
    job_id: String,
    operation: String,
    operation_version: String,
    body: Value,
}

fn prepare_jobs(dir: &Path, m: &Manifest, files: &[PathBuf]) -> Result<Vec<PreparedJob>, Error> {
    let files: Vec<PathBuf> = if files.is_empty() {
        let example = dir.join("jobs").join("example.json");
        if !example.is_file() {
            return Err("no --job given and no jobs/example.json in the app".into());
        }
        vec![example]
    } else {
        files.to_vec()
    };
    let mut out = Vec::new();
    for (i, path) in files.iter().enumerate() {
        let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if meta.len() > manifest::MAX_JOB_BYTES {
            return Err(format!("{}: larger than a job may be", path.display()).into());
        }
        let bytes = std::fs::read(path)?;
        let job: JobFile =
            serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        let op = m
            .operation(&job.operation, &job.operation_version)
            .ok_or_else(|| {
                format!(
                    "{}: the manifest offers no operation {}@{}",
                    path.display(),
                    job.operation,
                    job.operation_version
                )
            })?;
        manifest::check_payload(op, &job.payload)
            .map_err(|e| format!("{}: invalid_operation_payload: {e}", path.display()))?;
        let job_id = match job.job_id {
            Some(id) if manifest::valid_job_id(&id) => id,
            Some(id) => {
                return Err(
                    format!("{}: job_id {id:?}: job_ and 26 of a-z 2-7", path.display()).into(),
                )
            }
            None => manifest::derived_job_id(&bytes, i),
        };
        out.push(PreparedJob {
            body: json!({
                "job_id": job_id,
                "operation": job.operation,
                "operation_version": job.operation_version,
                "payload": Value::Object(job.payload),
            }),
            job_id,
            operation: job.operation,
            operation_version: job.operation_version,
        });
    }
    Ok(out)
}

/// The installation's configuration: the given file (or `{}`) checked against the manifest's
/// `config_schema` with its defaults filled in. `None` when the app has no schema and none was
/// given.
fn load_config(m: &Manifest, file: Option<&Path>) -> Result<Option<manifest::Config>, Error> {
    let given = match file {
        Some(p) => Some(
            serde_json::from_slice::<Value>(
                &std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?,
            )
            .map_err(|e| format!("{}: {e}", p.display()))?,
        ),
        None => None,
    };
    if m.config_schema.is_none() && given.is_none() {
        return Ok(None);
    }
    let config = manifest::apply_config(&m.schema(), given.unwrap_or_else(|| json!({})))
        .map_err(|e| format!("configuration: {e}"))?;
    Ok(Some(config))
}

fn default_command(dir: &Path) -> Result<Vec<String>, Error> {
    let node = std::env::var("PIGEONPOST_NEST_NODE").unwrap_or_else(|_| "node".into());
    let python = std::env::var("PIGEONPOST_NEST_PYTHON").unwrap_or_else(|_| {
        if cfg!(windows) {
            "python".into()
        } else {
            "python3".into()
        }
    });
    if dir.join(Lang::Node.main_file()).is_file() {
        Ok(vec![node, Lang::Node.main_file().into()])
    } else if dir.join(Lang::Python.main_file()).is_file() {
        Ok(vec![python, Lang::Python.main_file().into()])
    } else {
        Err("no app.mjs or app.py: say how to start the app, e.g. `pigeonpost nest run . -- ./my-app`".into())
    }
}

fn random_hex(n: usize) -> String {
    use rand_core::RngCore;
    let mut bytes = vec![0u8; n];
    rand_core::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The buyer's view of a result, exactly what the dispatcher keeps (`dispatch::Dispatcher::poll`),
/// with warnings for anything it would drop or treat as a failure.
pub fn buyer_view(job_id: &str, raw: &Value) -> (Value, Vec<String>) {
    let mut warnings = Vec::new();
    if raw["job_id"] != job_id {
        warnings
            .push("the result's job_id is not the job's: the platform counts it as failed".into());
    }
    if !matches!(raw["status"].as_str(), Some("succeeded" | "failed")) {
        warnings.push("status must be \"succeeded\" or \"failed\"".into());
    }
    let ok = raw["status"] == "succeeded" && raw["job_id"] == job_id;
    if ok {
        if let Some(obj) = raw.as_object() {
            for k in obj.keys() {
                if !matches!(
                    k.as_str(),
                    "job_id" | "status" | "answer" | "sources" | "model_used"
                ) {
                    warnings.push(format!("{k}: not kept by the platform; put it in answer"));
                }
            }
        }
        if !raw["sources"].is_array() {
            warnings.push("sources should be an array".into());
        }
        if !raw["model_used"].is_boolean() {
            warnings.push("model_used should be true or false".into());
        }
        (
            json!({
                "state": "succeeded",
                "result": {
                    "answer": raw["answer"],
                    "sources": raw["sources"],
                    "model_used": raw["model_used"],
                },
            }),
            warnings,
        )
    } else {
        let code = raw["error_code"].as_str().unwrap_or("worker_failed");
        (
            json!({ "state": "failed", "result": { "error_code": code } }),
            warnings,
        )
    }
}

fn read_result(path: &Path) -> Option<Value> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() {
        return Some(Value::Null);
    }
    if meta.len() > MAX_RESULT_BYTES {
        return Some(Value::Null);
    }
    let text = std::fs::read_to_string(path).ok()?;
    Some(serde_json::from_str(&text).unwrap_or(Value::Null))
}

fn print_event(e: &Value) {
    match e["type"].as_str() {
        Some("progress") => println!(
            "  progress  {:>3}%  {}",
            (e["fraction"].as_f64().unwrap_or(0.0) * 100.0).round(),
            e["message"].as_str().unwrap_or("")
        ),
        Some("secret") => println!(
            "  secret    {} (value not shown)",
            e["name"].as_str().unwrap_or("")
        ),
        Some("model") => println!(
            "  model     {} input / {} output tokens (max {})",
            e["input_tokens"], e["output_tokens"], e["max_tokens"]
        ),
        Some("effect") => {
            let state = match (e["state"].as_str(), e["deduplicated"] == true) {
                (Some("done"), true) => format!(
                    "already done ({}), not performed again",
                    e["reference"].as_str().unwrap_or("")
                ),
                (Some("done"), false) => {
                    format!("performed ({})", e["reference"].as_str().unwrap_or(""))
                }
                (Some("awaiting_approval"), _) => "awaiting approval".into(),
                (Some("refused"), _) => "refused: the manifest declares no external writes".into(),
                (s, _) => s.unwrap_or("?").into(),
            };
            println!(
                "  effect    {} {}  {}",
                e["kind"].as_str().unwrap_or(""),
                e["key"].as_str().unwrap_or(""),
                state
            );
            if let Some(summary) = e["summary"].as_str().filter(|s| !s.is_empty()) {
                if e["state"] == "awaiting_approval" {
                    println!("            {summary}");
                }
            }
        }
        _ => println!("  event     {e}"),
    }
}

fn print_view(view: &Value, warnings: &[String]) {
    for w in warnings {
        println!("  warning   {w}");
    }
    let r = &view["result"];
    if view["state"] == "succeeded" {
        println!("  result    succeeded");
        let pretty = serde_json::to_string_pretty(&r["answer"]).unwrap_or_default();
        for (i, line) in pretty.lines().enumerate() {
            println!(
                "{}{line}",
                if i == 0 {
                    "  answer    "
                } else {
                    "            "
                }
            );
        }
        println!("  sources   {}", r["sources"]);
        println!("  model_used {}", r["model_used"]);
    } else {
        println!(
            "  result    failed: {}",
            r["error_code"].as_str().unwrap_or("worker_failed")
        );
    }
}

fn forward<R>(reader: R)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            eprintln!("  app | {line}");
        }
    });
}

async fn run_app(args: &RunArgs, json_out: bool) -> Result<(), Error> {
    let dir = args.dir.as_path();
    let m = Manifest::load(dir)?;
    let jobs = prepare_jobs(dir, &m, &args.jobs)?;
    let config = load_config(&m, args.config.as_deref())?;
    let command = if args.command.is_empty() {
        default_command(dir)?
    } else {
        args.command.clone()
    };

    // The emulated Nest volume, as the dispatcher lays it out.
    let root = std::env::temp_dir().join(format!("pigeonpost-nest-run-{}", random_hex(6)));
    for sub in ["jobs/in", "jobs/out", "run"] {
        std::fs::create_dir_all(root.join(sub))?;
    }
    let token = format!("nbt_{}", random_hex(32));
    std::fs::write(root.join("run").join("broker-token"), &token)?;

    let mock = Mock::new(mock_broker::Config {
        token,
        replies: args.replies.clone(),
        no_model: args.no_model,
        external_writes: m.external_writes,
        approve_all: args.approve_all,
        approve: args.approve.iter().cloned().collect::<BTreeSet<_>>(),
        secrets: config
            .as_ref()
            .map(|c| c.secrets.clone())
            .unwrap_or_default(),
    });
    // The broker exists for the app only if its manifest says it may reach it.
    let broker_url = if m.uses_broker() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}", listener.local_addr()?);
        let app = mock_broker::router(mock.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Some(url)
    } else {
        None
    };

    let mut cmd = tokio::process::Command::new(&command[0]);
    cmd.args(&command[1..])
        .current_dir(dir)
        .env_clear()
        .env("NEST_ROOT", &root)
        .env("NEST_ID", "nst_local")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // The sandbox has no inherited environment; keep only what starting an interpreter needs.
    for key in [
        "PATH",
        "SYSTEMROOT",
        "WINDIR",
        "TEMP",
        "TMP",
        "TMPDIR",
        "PATHEXT",
    ] {
        if let Some(v) = std::env::var_os(key) {
            cmd.env(key, v);
        }
    }
    if let Some(url) = &broker_url {
        cmd.env("NEST_BROKER_URL", url);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start {:?}: {e}", command.join(" ")))?;
    if let Some(out) = child.stdout.take() {
        forward(out);
    }
    if let Some(err) = child.stderr.take() {
        forward(err);
    }

    if !json_out {
        println!(
            "{} {} | {} | broker: {}",
            m.name,
            m.version,
            command.join(" "),
            match &broker_url {
                Some(u) => format!("mock at {u}"),
                None => "none (the manifest declares no broker egress)".into(),
            }
        );
    }

    let outcome = drive(
        &jobs,
        args,
        config.as_ref().map(|c| &c.plain),
        &root,
        &mock,
        &mut child,
        json_out,
    )
    .await;
    let _ = child.start_kill();
    let _ = child.wait().await;

    let (reports, failures) = match outcome {
        Ok(v) => v,
        Err(e) => {
            if !args.keep {
                let _ = std::fs::remove_dir_all(&root);
            }
            return Err(e);
        }
    };
    let (requests, input_used, output_used) = {
        let m = mock.lock().unwrap_or_else(|e| e.into_inner());
        (m.requests, m.input_used, m.output_used)
    };
    if json_out {
        let mut out = json!({
            "app": { "name": m.name, "version": m.version },
            "command": command,
            "broker": {
                "mock": broker_url.is_some(),
                "requests": requests,
                "input_tokens": { "used": input_used, "limit": mock_broker::INPUT_ALLOWANCE },
                "output_tokens": { "used": output_used, "limit": mock_broker::OUTPUT_ALLOWANCE },
            },
            "jobs": reports,
        });
        if args.keep {
            out["root"] = json!(root.display().to_string());
        }
        println!("{out}");
    } else {
        if broker_url.is_some() {
            println!(
                "broker: {requests} model request(s); {input_used}/{} input and {output_used}/{} output tokens of the monthly allowance",
                mock_broker::INPUT_ALLOWANCE,
                mock_broker::OUTPUT_ALLOWANCE
            );
        }
        if args.keep {
            println!("volume kept at {}", root.display());
        }
    }
    if !args.keep {
        let _ = std::fs::remove_dir_all(&root);
    }
    if failures > 0 {
        return Err(format!("{failures} of {} job(s) did not succeed", jobs.len()).into());
    }
    Ok(())
}

/// Deliver one job and wait for its result file.
async fn deliver(
    job: &PreparedJob,
    config: Option<&Value>,
    root: &Path,
    timeout: Duration,
    child: &mut tokio::process::Child,
) -> Result<Value, Error> {
    let mut body = job.body.clone();
    if let Some(c) = config {
        body["config"] = c.clone();
    }
    let inbox = root.join("jobs").join("in");
    let tmp = inbox.join(format!("{}.tmp", job.job_id));
    std::fs::write(&tmp, body.to_string())?;
    std::fs::rename(&tmp, inbox.join(format!("{}.json", job.job_id)))?;
    let out = root
        .join("jobs")
        .join("out")
        .join(format!("{}.json", job.job_id));
    let started = Instant::now();
    loop {
        if let Some(v) = read_result(&out) {
            let _ = std::fs::remove_file(&out);
            return Ok(v);
        }
        if let Some(status) = child.try_wait()? {
            return Err(
                format!("the app exited ({status}) before answering {}", job.job_id).into(),
            );
        }
        if started.elapsed() > timeout {
            return Err(format!(
                "no result for {} after {}s (is the app polling {}?)",
                job.job_id,
                timeout.as_secs(),
                inbox.display()
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn drive(
    jobs: &[PreparedJob],
    args: &RunArgs,
    config: Option<&Value>,
    root: &Path,
    mock: &mock_broker::Shared,
    child: &mut tokio::process::Child,
    json_out: bool,
) -> Result<(Vec<Value>, usize), Error> {
    let timeout = Duration::from_secs(args.timeout.max(1));
    let mut reports = Vec::new();
    let mut failures = 0;
    for job in jobs {
        if !json_out {
            println!(
                "-- job {} {}@{}",
                job.job_id, job.operation, job.operation_version
            );
        }
        let mut attempts = Vec::new();
        let mut last_view = Value::Null;
        let planned = if args.redeliver { 2 } else { 1 };
        let mut deliveries_done = 0;
        while attempts.len() < MAX_ATTEMPTS {
            let raw = deliver(job, config, root, timeout, child).await?;
            let events = mock.lock().unwrap_or_else(|e| e.into_inner()).drain();
            let (view, warnings) = buyer_view(&job.job_id, &raw);
            if !json_out {
                if !attempts.is_empty() {
                    println!("  delivery  again (same job id)");
                }
                for e in &events {
                    print_event(e);
                }
                print_view(&view, &warnings);
            }
            let approval_needed = view["result"]["error_code"] == "approval_required";
            attempts.push(json!({
                "events": events,
                "result": raw,
                "buyer_view": view,
                "warnings": warnings,
            }));
            last_view = view;
            if approval_needed {
                let mut m = mock.lock().unwrap_or_else(|e| e.into_inner());
                let approved = m.approve_waiting(&job.job_id);
                if !approved.is_empty() {
                    drop(m);
                    if !json_out {
                        println!("  approve   {} (as the buyer)", approved.join(", "));
                    }
                    continue;
                }
                let waiting = m.waiting(&job.job_id);
                if !json_out && !waiting.is_empty() {
                    println!(
                        "  hint      run again with --approve-all or --approve {}",
                        waiting.join(" --approve ")
                    );
                }
            }
            deliveries_done += 1;
            if deliveries_done >= planned {
                break;
            }
        }
        if last_view["state"] != "succeeded" {
            failures += 1;
        }
        reports.push(json!({
            "job_id": job.job_id,
            "operation": job.operation,
            "operation_version": job.operation_version,
            "attempts": attempts,
            "final": last_view,
        }));
    }
    Ok((reports, failures))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_come_from_directories() {
        assert_eq!(name_from_dir(Path::new("/x/My App_2")), "my-app-2");
        assert_eq!(name_from_dir(Path::new("docs-qa")), "docs-qa");
        assert_eq!(name_from_dir(Path::new("--")), "");
    }

    #[test]
    fn the_buyer_sees_what_the_dispatcher_keeps() {
        let id = "job_aaaaaaaaaaaaaaaaaaaaaaaaaa";
        let (v, w) = buyer_view(
            id,
            &json!({"job_id": id, "status": "succeeded", "answer": {"x": 1}, "sources": [], "model_used": false, "debug": 1}),
        );
        assert_eq!(v["state"], "succeeded");
        assert_eq!(v["result"]["answer"], json!({"x": 1}));
        assert!(v["result"].get("debug").is_none());
        assert_eq!(w.len(), 1);
        let (v, _) = buyer_view(id, &json!({"job_id": "job_other", "status": "succeeded"}));
        assert_eq!(v["state"], "failed");
        assert_eq!(v["result"]["error_code"], "worker_failed");
        let (v, w) = buyer_view(
            id,
            &json!({"job_id": id, "status": "failed", "error_code": "approval_required"}),
        );
        assert_eq!(v["result"]["error_code"], "approval_required");
        assert!(w.is_empty());
    }
}
