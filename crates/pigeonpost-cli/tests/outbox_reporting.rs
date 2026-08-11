use std::process::Command;

use pigeonpost_client::state::OutboxRoute;
use pigeonpost_client::Agent;
use pigeonpost_core::{envelope, Identity};

#[test]
fn flush_json_reports_bounded_terminal_state() {
    let temporary = tempfile::tempdir().unwrap();
    let home = temporary.path().join("agent");
    let agent = Agent::open(&home).unwrap();
    let sender = Identity::from_seed([0x61; 32]);
    let recipient = Identity::from_seed([0x62; 32]);
    let wrap = envelope::wrap(
        &sender,
        &recipient.verifying_key(),
        "response body must not appear",
        100,
    )
    .unwrap();
    agent
        .state()
        .queue(
            "terminal-message",
            "/k/recipient",
            OutboxRoute::new("https://loft.example", false),
            &wrap,
            None,
            100,
        )
        .unwrap();
    let row = agent.state().pending(1, 100).unwrap()[0].row;
    agent.state().mark_terminal(row, "http_403", 200).unwrap();
    drop(agent);

    let output = Command::new(env!("CARGO_BIN_EXE_pigeonpost"))
        .args(["--home", home.to_str().unwrap(), "--json", "flush"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["queued"], 0);
    assert_eq!(value["dead_letter_count"], 1);
    assert_eq!(value["dead_letters"][0]["reason"], "http_403");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("response body must not appear"));
}
