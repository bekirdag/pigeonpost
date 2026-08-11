//! Process-level checks for exact-confirmation rotation and storage operator surfaces.

use std::path::Path;
use std::process::{Command, Output};

use pigeonpost_core::{Address, Identity};
use serde_json::Value;

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pigeonpost"))
        .arg("--home")
        .arg(home)
        .arg("--json")
        .args(args)
        .env_remove("PIGEONPOST_HOME")
        .env_remove("PIGEONPOST_RECOVERY_DIR")
        .output()
        .unwrap()
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "invalid JSON ({error}); stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn malformed_and_mismatched_confirmations_fail_before_home_creation() {
    let directory = tempfile::tempdir().unwrap();
    let rotate_home = directory.path().join("malformed-rotate");
    let rotate = run(&rotate_home, &["rotate", "--confirm", "not-an-address"]);
    assert!(!rotate.status.success());
    assert!(!rotate_home.exists());

    let delete_home = directory.path().join("mismatched-delete");
    let deletion = run(
        &delete_home,
        &[
            "storage",
            "delete-message",
            "message-id",
            "--confirm",
            "different-id",
        ],
    );
    assert!(!deletion.status.success());
    assert!(!delete_home.exists());

    let directory_home = directory.path().join("mismatched-directory-remove");
    let removal = run(
        &directory_home,
        &[
            "directory",
            "remove",
            "https://directory.example",
            "--confirm",
            "https://other.example",
        ],
    );
    assert!(!removal.status.success());
    assert!(!directory_home.exists());
}

#[test]
fn unrelated_rotation_source_never_changes_the_active_identity() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("agent");
    let created = run(&home, &["id"]);
    assert!(created.status.success());
    let active = json(&created)["address"].as_str().unwrap().to_string();
    let unrelated = Address::from_pubkey(&Identity::from_seed([0xD1; 32]).verifying_key());

    let rejected = run(&home, &["rotate", "--confirm", unrelated.as_str()]);
    assert!(!rejected.status.success());

    let reopened = run(&home, &["id"]);
    assert!(reopened.status.success());
    assert_eq!(json(&reopened)["address"], active);
    let agent = pigeonpost_client::Agent::open(&home).unwrap();
    assert_eq!(agent.address().as_str(), active);
    assert!(agent.state().own_rotations().unwrap().is_empty());
}

#[test]
fn storage_status_limits_and_empty_lists_are_machine_readable() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("agent");

    let status = run(&home, &["storage", "status"]);
    assert!(status.status.success());
    let status = json(&status);
    assert_eq!(status["updated"], false);
    assert!(status["limits"]["inbox_messages"].is_u64());
    assert_eq!(
        status["limits"]["inbox_tombstones"],
        pigeonpost_client::MAX_INBOX_TOMBSTONES
    );
    assert!(status["usage"]["inbox_tombstones"].is_u64());
    assert!(status["usage"]["outbox_payload_bytes"].is_u64());
    let rendered = status.to_string();
    assert!(!rendered.contains("wrap"));
    assert!(!rendered.contains("token"));

    let updated = run(
        &home,
        &[
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
        ],
    );
    assert!(updated.status.success());
    let updated = json(&updated);
    assert_eq!(updated["updated"], true);
    assert_eq!(updated["limits"]["inbox_messages"], 100);
    assert_eq!(updated["limits"]["outbox_payload_bytes"], 2000);

    for list in ["pending", "completed", "dead-letters"] {
        let output = run(&home, &["storage", list, "--limit", "7"]);
        assert!(output.status.success(), "{list}");
        let output = json(&output);
        assert_eq!(output["deliveries"], serde_json::json!([]));
        assert_eq!(output["returned"], 0);
        assert_eq!(output["limit"], 7);
    }
}

#[test]
fn exact_directory_removal_allows_an_explicit_new_signing_key() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("agent");
    let url = "https://directory.example";
    let agent = pigeonpost_client::Agent::open(&home).unwrap();
    assert!(agent.state().add_directory(url, &[0x11; 32], 1).unwrap());
    assert!(agent.state().add_directory(url, &[0x22; 32], 2).is_err());
    drop(agent);

    let removed = run(&home, &["directory", "remove", url, "--confirm", url]);
    assert!(
        removed.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&removed.stderr)
    );
    assert_eq!(
        json(&removed),
        serde_json::json!({ "url": url, "removed": true })
    );

    let agent = pigeonpost_client::Agent::open(&home).unwrap();
    assert!(agent.state().directories().unwrap().is_empty());
    assert!(agent.state().add_directory(url, &[0x22; 32], 3).unwrap());
}
