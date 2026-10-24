//! Edits and replies against a scripted runtime and an in-memory postbox.

use super::*;
use crate::executor::{MailboxRoute, Permission};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// `(kind, status, message_ids, msg_refs)`.
type Receipt = (String, Option<String>, Vec<String>, Vec<String>);

#[derive(Default)]
struct Record {
    /// `(to, body, reply_to)`.
    sent: Vec<(String, String, Option<String>)>,
    acks: Vec<String>,
    /// `(kind, status, message_ids, msg_refs)`.
    receipts: Vec<Receipt>,
    prompts: Vec<String>,
}

struct Fake {
    messages: Mutex<HashMap<String, Value>>,
    record: Mutex<Record>,
    calls: AtomicUsize,
    /// Runs (by call index) that wait for `gate` before answering.
    blocking: Mutex<Vec<usize>>,
    gate: tokio::sync::Semaphore,
    started: tokio::sync::mpsc::UnboundedSender<usize>,
    slots: tokio::sync::Semaphore,
}

impl Fake {
    fn new(slots: usize) -> (Arc<Self>, tokio::sync::mpsc::UnboundedReceiver<usize>) {
        let (started, rx) = tokio::sync::mpsc::unbounded_channel();
        (
            Arc::new(Fake {
                messages: Mutex::new(HashMap::new()),
                record: Mutex::new(Record::default()),
                calls: AtomicUsize::new(0),
                blocking: Mutex::new(Vec::new()),
                gate: tokio::sync::Semaphore::new(0),
                started,
                slots: tokio::sync::Semaphore::new(slots),
            }),
            rx,
        )
    }

    fn put(&self, message: Value) {
        let id = message["message_id"].as_str().unwrap().to_string();
        self.messages.lock().unwrap().insert(id, message);
    }

    fn block(&self, call: usize) {
        self.blocking.lock().unwrap().push(call);
    }

    fn prompts(&self) -> Vec<String> {
        self.record.lock().unwrap().prompts.clone()
    }

    fn sent(&self) -> Vec<(String, String, Option<String>)> {
        self.record.lock().unwrap().sent.clone()
    }
}

impl Ports for Arc<Fake> {
    async fn fetch(
        &self,
        _mailbox: String,
        message_id: String,
    ) -> Result<Option<Value>, (&'static str, String)> {
        Ok(self.messages.lock().unwrap().get(&message_id).cloned())
    }

    async fn send(
        &self,
        _mailbox: String,
        to: String,
        body: String,
        _thread: Option<String>,
        extras: SendExtras,
    ) -> Result<(), String> {
        self.record
            .lock()
            .unwrap()
            .sent
            .push((to, body, extras.reply_to));
        Ok(())
    }

    async fn ack(&self, _mailbox: String, message_id: String) -> Result<(), String> {
        self.record.lock().unwrap().acks.push(message_id);
        Ok(())
    }

    async fn receipt(
        &self,
        _mailbox: String,
        message_ids: Vec<String>,
        msg_refs: Vec<String>,
        kind: &'static str,
        status: Option<&'static str>,
    ) {
        self.record.lock().unwrap().receipts.push((
            kind.to_string(),
            status.map(str::to_string),
            message_ids,
            msg_refs,
        ));
    }

    async fn run(
        &self,
        _home: PathBuf,
        action: Action,
        sender: String,
        _message_id: String,
    ) -> Result<Run, RunFailure> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        self.record
            .lock()
            .unwrap()
            .prompts
            .push(crate::runner::prompt(&action, &sender));
        let _ = self.started.send(call);
        let blocked = self.blocking.lock().unwrap().contains(&call);
        if blocked {
            self.gate.acquire().await.unwrap().forget();
        }
        Ok(Run {
            text: format!("answer {call}"),
            truncated: false,
            adapter: None,
            panel: None,
        })
    }

    async fn work_summary(&self, _workspace: PathBuf) -> String {
        "M src/lib.rs (FAKE SUMMARY)".into()
    }

    fn slots(&self, _max: usize) -> &tokio::sync::Semaphore {
        &self.slots
    }
}

fn config(mailbox: &str, workspace: &Path, verbs: &[&str], report_status: bool) -> RoutingConfig {
    RoutingConfig {
        mailbox: vec![MailboxRoute {
            address: mailbox.into(),
            workspace: workspace.to_path_buf(),
            runtime: "claude".into(),
            verbs: verbs.iter().map(|v| v.to_string()).collect(),
            timeout_secs: 60,
            permission: Permission::Full,
            branches: vec!["main".into()],
            daily_runs_per_sender: 50,
            panel: None,
        }],
        max_concurrent: 2,
        execute: true,
        report_status,
    }
}

fn message(
    id: &str,
    msg_ref: &str,
    verb: &str,
    args: Value,
    autonomy: &str,
    edit_of: Option<&str>,
) -> Value {
    let note = args["task"].as_str().unwrap_or("").to_string();
    serde_json::json!({
        "message_id": id,
        "msg_ref": msg_ref,
        "direction": "in",
        "from": "/k/sender",
        "sender_handle": "/bekir/main",
        "thread_id": "thread-1",
        "autonomy": autonomy,
        "verb": verb,
        "edit_of": edit_of,
        "body": serde_json::json!({"v": 1, "verb": verb, "args": args, "note": note}).to_string(),
    })
}

fn event(mailbox: &str, event_id: i64, message_id: &str) -> MailEvent {
    MailEvent {
        event_id,
        mailbox: mailbox.into(),
        message_id: message_id.into(),
        sender: "/k/sender".into(),
    }
}

fn audit_outcomes(home: &Path) -> Vec<String> {
    std::fs::read_to_string(crate::executor::audit_path(home))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| v["outcome"].as_str().map(str::to_string))
        .collect()
}

async fn wait_for_state(mailbox: &str, msg_ref: &str, want: &str) {
    for _ in 0..400 {
        if state_of(mailbox, msg_ref) == Some(want) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!(
        "{mailbox} {msg_ref} never reached {want} (is {:?})",
        state_of(mailbox, msg_ref)
    );
}

fn spawn_act(
    fake: &Arc<Fake>,
    home: &Path,
    config: &RoutingConfig,
    event: MailEvent,
) -> tokio::task::JoinHandle<()> {
    let fake = fake.clone();
    let home = home.to_path_buf();
    let config = config.clone();
    tokio::spawn(async move { act_with(&fake, &home, &config, &event).await })
}

const ROOT: &str = "mref_aaaaaaaaaaaaaaaaaaaaaaaaaa";

/// Edited while waiting for a slot: one run, on the edited text, and both versions acknowledged.
#[tokio::test]
async fn an_edit_to_a_queued_request_replaces_it() {
    let home = tempfile::tempdir().unwrap();
    let mailbox = "/bekir/edit-queued";
    let config = config(mailbox, home.path(), &["make_change"], true);
    let (fake, _started) = Fake::new(1);
    fake.put(message(
        "orig",
        ROOT,
        "make_change",
        serde_json::json!({"task": "ORIGINAL TASK"}),
        "auto",
        None,
    ));
    fake.put(message(
        "edit",
        "mref_bbbbbbbbbbbbbbbbbbbbbbbbbb",
        "make_change",
        serde_json::json!({"task": "EDITED TASK"}),
        "auto",
        Some(ROOT),
    ));

    let hold = fake.slots.try_acquire().unwrap();
    let original = spawn_act(&fake, home.path(), &config, event(mailbox, 1, "orig"));
    wait_for_state(mailbox, ROOT, "queued").await;
    act_with(&fake, home.path(), &config, &event(mailbox, 2, "edit")).await;
    drop(hold);
    original.await.unwrap();

    let prompts = fake.prompts();
    assert_eq!(prompts.len(), 1, "a replaced request runs once");
    assert!(prompts[0].contains("EDITED TASK"));
    assert!(!prompts[0].contains("ORIGINAL TASK"));
    let acks = fake.record.lock().unwrap().acks.clone();
    assert!(acks.contains(&"orig".to_string()) && acks.contains(&"edit".to_string()));
    let outcomes = audit_outcomes(home.path());
    assert!(outcomes.contains(&"edit_replaced_queued".to_string()));
    assert!(outcomes.contains(&"executed".to_string()));
    assert_eq!(state_of(mailbox, ROOT), Some("done"));
}

/// Edited mid-run: the run is stopped and restarted on the edit, told what was already done.
#[tokio::test]
async fn an_edit_to_a_running_request_restarts_it_with_a_summary() {
    let home = tempfile::tempdir().unwrap();
    let mailbox = "/bekir/edit-running";
    let config = config(mailbox, home.path(), &["make_change"], true);
    let (fake, mut started) = Fake::new(2);
    fake.block(0);
    fake.put(message(
        "orig",
        ROOT,
        "make_change",
        serde_json::json!({"task": "ORIGINAL TASK"}),
        "auto",
        None,
    ));
    fake.put(message(
        "edit",
        "mref_cccccccccccccccccccccccccc",
        "make_change",
        serde_json::json!({"task": "EDITED TASK"}),
        "auto",
        Some(ROOT),
    ));

    let original = spawn_act(&fake, home.path(), &config, event(mailbox, 1, "orig"));
    assert_eq!(started.recv().await, Some(0));
    act_with(&fake, home.path(), &config, &event(mailbox, 2, "edit")).await;
    original.await.unwrap();

    let prompts = fake.prompts();
    assert_eq!(prompts.len(), 2);
    assert!(prompts[1].contains("edited by the sender while you were working on it"));
    assert!(prompts[1].contains("FAKE SUMMARY"));
    assert!(prompts[1].contains("EDITED TASK"));
    let sent = fake.sent();
    assert_eq!(sent.len(), 1, "one answer, for the edited request");
    assert!(sent[0].1.contains("answer 1"));
    assert_eq!(
        sent[0].2.as_deref(),
        Some(ROOT),
        "the answer replies to the request"
    );
    assert!(audit_outcomes(home.path()).contains(&"edit_restart".to_string()));
    // The gear keeps turning across the restart.
    let working = fake
        .record
        .lock()
        .unwrap()
        .receipts
        .iter()
        .filter(|r| r.1.as_deref() == Some("working"))
        .count();
    assert_eq!(working, 2);
}

/// A push is never stopped half-way: the edit waits and becomes a follow-up run.
#[tokio::test]
async fn an_edit_to_an_uninterruptible_run_follows_it() {
    let home = tempfile::tempdir().unwrap();
    let mailbox = "/bekir/edit-push";
    let config = config(mailbox, home.path(), &["git_push"], true);
    let (fake, mut started) = Fake::new(2);
    fake.block(0);
    fake.put(message(
        "orig",
        ROOT,
        "git_push",
        serde_json::json!({"branch": "main"}),
        "auto",
        None,
    ));
    fake.put(message(
        "edit",
        "mref_dddddddddddddddddddddddddd",
        "git_push",
        serde_json::json!({"ref": "main"}),
        "auto",
        Some(ROOT),
    ));

    let original = spawn_act(&fake, home.path(), &config, event(mailbox, 1, "orig"));
    assert_eq!(started.recv().await, Some(0));
    act_with(&fake, home.path(), &config, &event(mailbox, 2, "edit")).await;
    assert!(audit_outcomes(home.path()).contains(&"edit_amendment_queued".to_string()));
    assert_eq!(fake.prompts().len(), 1, "nothing restarts a push");
    fake.gate.add_permits(1);
    original.await.unwrap();

    let prompts = fake.prompts();
    assert_eq!(prompts.len(), 2);
    assert!(prompts[1].contains("could not be stopped safely"));
    assert!(prompts[1].contains("answer 0"));
    assert_eq!(fake.sent().len(), 2, "the push is answered, then the edit");
    assert!(audit_outcomes(home.path()).contains(&"edit_followup".to_string()));
}

/// Edited after the answer went: a follow-up run told to reconcile.
#[tokio::test]
async fn an_edit_after_completion_starts_a_reconciling_followup() {
    let home = tempfile::tempdir().unwrap();
    let mailbox = "/bekir/edit-done";
    let config = config(mailbox, home.path(), &["make_change"], true);
    let (fake, _started) = Fake::new(2);
    fake.put(message(
        "orig",
        ROOT,
        "make_change",
        serde_json::json!({"task": "ORIGINAL TASK"}),
        "auto",
        None,
    ));
    fake.put(message(
        "edit",
        "mref_eeeeeeeeeeeeeeeeeeeeeeeeee",
        "make_change",
        serde_json::json!({"task": "EDITED TASK"}),
        "auto",
        Some(ROOT),
    ));

    act_with(&fake, home.path(), &config, &event(mailbox, 1, "orig")).await;
    assert_eq!(state_of(mailbox, ROOT), Some("done"));
    act_with(&fake, home.path(), &config, &event(mailbox, 2, "edit")).await;

    let prompts = fake.prompts();
    assert_eq!(prompts.len(), 2);
    assert!(prompts[1].contains("The user edited their request after you finished"));
    assert!(prompts[1].contains("reconcile your result with the edited"));
    assert!(
        prompts[1].contains("ORIGINAL TASK"),
        "the original is quoted"
    );
    assert!(
        prompts[1].contains("answer 0"),
        "and so is the earlier answer"
    );
    assert!(prompts[1].contains("EDITED TASK"));
    assert!(audit_outcomes(home.path()).contains(&"edit_followup".to_string()));
}

/// Held for a human: the edit takes the original's place in the session spool.
#[tokio::test]
async fn an_edit_to_a_held_request_replaces_it_in_the_spool() {
    let home = tempfile::tempdir().unwrap();
    let mailbox = "/bekir/edit-held";
    let config = config(mailbox, home.path(), &["make_change"], true);
    let (fake, _started) = Fake::new(2);
    fake.put(message(
        "orig",
        ROOT,
        "make_change",
        serde_json::json!({"task": "ORIGINAL TASK"}),
        "review",
        None,
    ));
    fake.put(message(
        "edit",
        "mref_ffffffffffffffffffffffffff",
        "make_change",
        serde_json::json!({"task": "EDITED TASK"}),
        "review",
        Some(ROOT),
    ));
    let spool =
        crate::agentd_cmd::spool_file(&crate::agentd_cmd::machine_home(home.path()), mailbox);
    std::fs::create_dir_all(spool.parent().unwrap()).unwrap();
    std::fs::write(
        &spool,
        "{\"event_id\":1,\"message_id\":\"orig\",\"claimed\":true}\n\
         {\"event_id\":2,\"message_id\":\"edit\",\"claimed\":true}\n",
    )
    .unwrap();

    act_with(&fake, home.path(), &config, &event(mailbox, 1, "orig")).await;
    assert_eq!(state_of(mailbox, ROOT), Some("given_back"));
    act_with(&fake, home.path(), &config, &event(mailbox, 2, "edit")).await;

    let lines = spool_lines(home.path(), mailbox);
    assert_eq!(lines.len(), 1, "the original's line is gone");
    assert_eq!(lines[0]["message_id"], "edit");
    assert_eq!(
        lines[0]["claimed"], false,
        "and the edit is the session's to read"
    );
    assert!(audit_outcomes(home.path()).contains(&"edit_replaced_held".to_string()));
    assert!(fake.prompts().is_empty(), "nothing held ever runs");
}

/// An edit is classified like any request: one asking for a verb this route will not run is
/// refused, and the run it tried to change carries on untouched.
#[tokio::test]
async fn an_edit_that_the_rails_refuse_changes_nothing_mid_run() {
    let home = tempfile::tempdir().unwrap();
    let mailbox = "/bekir/edit-refused";
    let config = config(mailbox, home.path(), &["make_change"], true);
    let (fake, mut started) = Fake::new(2);
    fake.block(0);
    fake.put(message(
        "orig",
        ROOT,
        "make_change",
        serde_json::json!({"task": "ORIGINAL TASK"}),
        "auto",
        None,
    ));
    fake.put(message(
        "edit",
        "mref_gggggggggggggggggggggggggg",
        "deploy",
        serde_json::json!({"branch": "main"}),
        "auto",
        Some(ROOT),
    ));

    let original = spawn_act(&fake, home.path(), &config, event(mailbox, 1, "orig"));
    assert_eq!(started.recv().await, Some(0));
    act_with(&fake, home.path(), &config, &event(mailbox, 2, "edit")).await;
    fake.gate.add_permits(1);
    original.await.unwrap();

    assert_eq!(fake.prompts().len(), 1, "the refused edit started nothing");
    assert!(fake.prompts()[0].contains("ORIGINAL TASK"));
    let outcomes = audit_outcomes(home.path());
    assert!(outcomes.contains(&"edit_refused".to_string()));
    assert!(outcomes.contains(&"executed".to_string()));
    let sent = fake.sent();
    assert_eq!(
        sent.len(),
        2,
        "the refusal is said, and the original still answered"
    );
    assert!(sent[0].1.contains("This edit was not applied"));
}

/// The receipts follow the run when the machine reports status, and not at all when it does not.
#[tokio::test]
async fn receipts_follow_the_run_only_when_status_is_reported() {
    for report in [true, false] {
        let home = tempfile::tempdir().unwrap();
        let mailbox = if report {
            "/bekir/receipts-on"
        } else {
            "/bekir/receipts-off"
        };
        let config = config(mailbox, home.path(), &["make_change"], report);
        let (fake, _started) = Fake::new(2);
        fake.put(message(
            "orig",
            ROOT,
            "make_change",
            serde_json::json!({"task": "T"}),
            "auto",
            None,
        ));
        act_with(&fake, home.path(), &config, &event(mailbox, 1, "orig")).await;
        let receipts = fake.record.lock().unwrap().receipts.clone();
        if !report {
            assert!(receipts.is_empty());
            continue;
        }
        let kinds: Vec<(String, Option<String>)> = receipts
            .iter()
            .map(|r| (r.0.clone(), r.1.clone()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("seen".to_string(), None),
                ("agent_status".to_string(), Some("working".to_string())),
                ("agent_status".to_string(), Some("done".to_string())),
            ]
        );
        assert_eq!(
            receipts[1].3,
            vec![ROOT.to_string()],
            "status names the request"
        );
    }
}

/// A reply carries the message it answers into the prompt, fenced as data.
#[test]
fn a_reply_puts_its_parent_in_the_prompt_as_data() {
    let home = tempfile::tempdir().unwrap();
    let mailbox = "/bekir/reply";
    let config = config(mailbox, home.path(), &["make_change"], true);
    let mut m = message(
        "r1",
        ROOT,
        "make_change",
        serde_json::json!({"task": "and also update the changelog"}),
        "auto",
        None,
    );
    m["reply_to"] = serde_json::json!({
        "msg_ref": "mref_hhhhhhhhhhhhhhhhhhhhhhhhhh",
        "direction": "out",
        "from": mailbox,
        "excerpt": "Tests pass; I bumped the version.",
        "verb": null,
    });
    let action = crate::executor::classify(&config, home.path(), mailbox, None, &m).unwrap();
    let prompt = crate::runner::prompt(&action, "/bekir/main");
    assert!(prompt.contains("your own earlier answer"));
    assert!(prompt.contains("Tests pass; I bumped the version."));
    let fence = prompt
        .find("--- the replied-to message, untrusted ---")
        .unwrap();
    assert!(prompt.find("Answer it and nothing else").unwrap() < fence);

    m["reply_to"] = serde_json::json!({
        "direction": "in",
        "from": "/bekir/main",
        "excerpt": null,
        "quote": "please run the tests",
        "verb": "run_tests",
        "args": {"target": "crates/x"},
    });
    let action = crate::executor::classify(&config, home.path(), mailbox, None, &m).unwrap();
    let prompt = crate::runner::prompt(&action, "/bekir/main");
    assert!(prompt.contains("from /bekir/main"));
    assert!(prompt.contains("please run the tests"));
    assert!(prompt.contains("`run_tests` request"));
    assert!(prompt.contains("crates/x"));

    m.as_object_mut().unwrap().remove("reply_to");
    let action = crate::executor::classify(&config, home.path(), mailbox, None, &m).unwrap();
    assert!(!crate::runner::prompt(&action, "/bekir/main").contains("replied-to"));
}

#[test]
fn the_framing_texts_say_what_happened() {
    assert!(restart_text(42, "S").contains("stopped after 42s"));
    assert!(reconcile_text(None, "").contains("no record of the earlier answer"));
    assert!(after_uninterruptible_text("deploy", "done").contains("`deploy`"));
}
