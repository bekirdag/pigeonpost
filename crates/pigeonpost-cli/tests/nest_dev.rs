//! `pigeonpost nest templates | init | run` end to end: the real binary, the real templates, the
//! mock broker. Runs offline. Language tests skip when node or python is not installed.

use std::process::{Command, Output};
use std::sync::OnceLock;
use tempfile::TempDir;

/// A home that is never the developer's: `nest` must not need one, and must not touch one.
fn home() -> &'static std::path::Path {
    static HOME: OnceLock<TempDir> = OnceLock::new();
    HOME.get_or_init(|| tempfile::tempdir().expect("home"))
        .path()
}

fn pp(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pigeonpost"))
        .args(args)
        .env("PIGEONPOST_HOME", home())
        .output()
        .expect("failed to run pigeonpost binary")
}

fn have(cmd: &str, arg: &str) -> bool {
    match Command::new(cmd).arg(arg).output() {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

fn json_ok(out: &Output) -> serde_json::Value {
    if !out.status.success() {
        panic!(
            "command failed with status {:?}\nstdout: {}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "failed to parse stdout as JSON: {}\nstdout: {}\nstderr: {}",
            e,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

/// stdout as JSON, whatever the exit status (a run whose jobs fail still prints its report).
fn json_of(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not JSON: {e}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn init(template: &str, lang: &str) -> TempDir {
    let tmp = tempfile::tempdir().expect("failed to create tempdir");
    let app_dir = tmp.path().join("app");
    let out = pp(&[
        "nest",
        "init",
        template,
        app_dir.to_str().unwrap(),
        "--lang",
        lang,
        "--publisher",
        "acme",
    ]);
    if !out.status.success() {
        panic!(
            "init failed for {} {} with status {:?}\nstdout: {}\nstderr: {}",
            template,
            lang,
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    tmp
}

#[test]
fn templates_are_listed() {
    let out = pp(&["--json", "nest", "templates"]);
    let json = json_ok(&out);
    assert!(json.is_array(), "expected JSON array");
    let arr = json.as_array().unwrap();
    assert_eq!(arr.len(), 3, "expected 3 templates");

    let names: Vec<&str> = arr.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["tool", "docs-qa", "approval-write"]);

    for t in arr {
        assert!(t["summary"].is_string());
        let langs = t["languages"].as_array().unwrap();
        assert_eq!(langs.len(), 2);
        assert_eq!(langs[0].as_str().unwrap(), "node");
        assert_eq!(langs[1].as_str().unwrap(), "python");
    }
}

#[test]
fn init_writes_a_named_app_for_each_language() {
    let python_name = if cfg!(windows) { "python" } else { "python3" };
    let langs = [("node", "node"), ("python", python_name)];

    for template in ["tool", "docs-qa", "approval-write"] {
        for (lang, _) in &langs {
            let tmp = init(template, lang);
            let app_dir = tmp.path().join("app");

            // Check common files exist
            assert!(
                app_dir.join("nest-release.toml").exists(),
                "missing nest-release.toml"
            );
            assert!(app_dir.join("operations").exists(), "missing operations/");
            assert!(
                app_dir.join("config.schema.json").exists(),
                "missing config.schema.json"
            );
            assert!(
                app_dir.join("jobs").join("example.json").exists(),
                "missing jobs/example.json"
            );
            assert!(app_dir.join("README.md").exists(), "missing README.md");

            if *lang == "node" {
                assert!(app_dir.join("app.mjs").exists(), "missing app.mjs");
                assert!(
                    app_dir.join("app.test.mjs").exists(),
                    "missing app.test.mjs"
                );
                assert!(
                    app_dir.join("Containerfile").exists(),
                    "missing Containerfile"
                );
                assert!(
                    app_dir.join("pigeonpost-nest.mjs").exists(),
                    "missing pigeonpost-nest.mjs"
                );
            } else {
                assert!(app_dir.join("app.py").exists(), "missing app.py");
                assert!(app_dir.join("test_app.py").exists(), "missing test_app.py");
                assert!(
                    app_dir.join("pigeonpost_nest.py").exists(),
                    "missing pigeonpost_nest.py"
                );
            }

            if template == "docs-qa" {
                assert!(
                    app_dir.join("docs").join("handbook.md").exists(),
                    "missing docs/handbook.md for docs-qa"
                );
            }

            // Check manifest lines
            let manifest = std::fs::read_to_string(app_dir.join("nest-release.toml")).unwrap();
            assert!(
                manifest.contains("name = \"app\""),
                "manifest missing name = \"app\""
            );
            assert!(
                manifest.contains("listing_address = \"/acme/app\""),
                "manifest missing listing_address"
            );
            if *lang == "python" {
                assert!(
                    manifest.contains("entrypoint = \"/usr/local/bin/python3\""),
                    "python manifest missing entrypoint"
                );
            }

            // Init into non-empty dir fails
            let out2 = pp(&[
                "nest",
                "init",
                template,
                app_dir.to_str().unwrap(),
                "--lang",
                lang,
                "--publisher",
                "acme",
            ]);
            assert!(
                !out2.status.success(),
                "init into non-empty dir should fail"
            );

            // Unknown template fails and stderr mentions docs-qa
            let out3 = pp(&[
                "nest",
                "init",
                "unknown-template",
                tmp.path().join("bad").to_str().unwrap(),
                "--lang",
                lang,
                "--publisher",
                "acme",
            ]);
            assert!(!out3.status.success(), "unknown template should fail");
            let stderr = String::from_utf8_lossy(&out3.stderr);
            assert!(
                stderr.contains("docs-qa"),
                "stderr should mention docs-qa: {}",
                stderr
            );
        }
    }
}

#[test]
fn the_tool_runs_without_a_broker() {
    let python_name = if cfg!(windows) { "python" } else { "python3" };
    let langs = [("node", "node"), ("python", python_name)];

    for (lang, interpreter) in &langs {
        if !have(interpreter, "--version") {
            eprintln!("skipping: {} not found", lang);
            continue;
        }

        let tmp = init("tool", lang);
        let app_dir = tmp.path().join("app");
        let out = pp(&["--json", "nest", "run", app_dir.to_str().unwrap()]);

        if !out.status.success() {
            panic!(
                "run failed for {} with status {:?}\nstdout: {}\nstderr: {}",
                lang,
                out.status,
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }

        let json = json_ok(&out);

        // broker.mock == false
        assert!(
            !json["broker"]["mock"].as_bool().unwrap(),
            "broker.mock should be false"
        );

        // final state succeeded
        let jobs = json["jobs"].as_array().unwrap();
        assert_eq!(jobs.len(), 1);
        let job = &jobs[0];
        assert_eq!(job["final"]["state"].as_str().unwrap(), "succeeded");

        // final.result.answer
        let answer = &job["final"]["result"]["answer"];
        assert_eq!(answer["rows"].as_i64().unwrap(), 3);

        let columns = answer["columns"].as_array().unwrap();
        assert_eq!(columns.len(), 3);
        assert_eq!(columns[0].as_str().unwrap(), "item");
        assert_eq!(columns[1].as_str().unwrap(), "qty");
        assert_eq!(columns[2].as_str().unwrap(), "price");

        let totals = &answer["totals"];
        assert_eq!(totals["qty"].as_i64().unwrap(), 26);
        assert_eq!(totals["price"].as_f64().unwrap(), 0.4);

        // model_used false
        assert!(!job["final"]["result"]["model_used"].as_bool().unwrap());
    }
}

#[test]
fn docs_qa_answers_through_the_mock_broker_and_falls_back_without_a_model() {
    let python_name = if cfg!(windows) { "python" } else { "python3" };
    let langs = [("node", "node"), ("python", python_name)];

    for (lang, interpreter) in &langs {
        if !have(interpreter, "--version") {
            eprintln!("skipping: {} not found", lang);
            continue;
        }

        // (a) run with reply
        let tmp1 = init("docs-qa", lang);
        let app_dir1 = tmp1.path().join("app");
        let out1 = pp(&[
            "--json",
            "nest",
            "run",
            app_dir1.to_str().unwrap(),
            "--reply",
            "Twenty-five days.",
        ]);

        if !out1.status.success() {
            panic!(
                "run with reply failed for {} with status {:?}\nstdout: {}\nstderr: {}",
                lang,
                out1.status,
                String::from_utf8_lossy(&out1.stdout),
                String::from_utf8_lossy(&out1.stderr)
            );
        }

        let json1 = json_of(&out1);
        let job1 = &json1["jobs"].as_array().unwrap()[0];

        assert_eq!(job1["final"]["state"].as_str().unwrap(), "succeeded");
        assert_eq!(
            job1["final"]["result"]["answer"].as_str().unwrap(),
            "Twenty-five days."
        );
        assert!(job1["final"]["result"]["model_used"].as_bool().unwrap());

        let sources = job1["final"]["result"]["sources"].as_array().unwrap();
        assert!(!sources.is_empty());
        assert_eq!(sources[0].as_str().unwrap(), "annual-leave");

        assert_eq!(json1["broker"]["requests"].as_i64().unwrap(), 1);
        assert!(json1["broker"]["input_tokens"]["used"].as_i64().unwrap() > 0);

        // Check events contain a model event
        let attempts = job1["attempts"].as_array().unwrap();
        let events = attempts[0]["events"].as_array().unwrap();
        let has_model_event = events
            .iter()
            .any(|e| e["type"].as_str().unwrap() == "model");
        assert!(has_model_event, "should have a model event");

        // (b) run with no-model
        let tmp2 = init("docs-qa", lang);
        let app_dir2 = tmp2.path().join("app");
        let out2 = pp(&[
            "--json",
            "nest",
            "run",
            app_dir2.to_str().unwrap(),
            "--no-model",
        ]);

        if !out2.status.success() {
            panic!(
                "run with no-model failed for {} with status {:?}\nstdout: {}\nstderr: {}",
                lang,
                out2.status,
                String::from_utf8_lossy(&out2.stdout),
                String::from_utf8_lossy(&out2.stderr)
            );
        }

        let json2 = json_ok(&out2);
        let job2 = &json2["jobs"].as_array().unwrap()[0];

        assert_eq!(job2["final"]["state"].as_str().unwrap(), "succeeded");
        assert!(!job2["final"]["result"]["model_used"].as_bool().unwrap());

        let answer_str = job2["final"]["result"]["answer"].as_str().unwrap();
        assert!(
            answer_str.contains("25 days"),
            "answer should contain '25 days': {}",
            answer_str
        );
    }
}

#[test]
fn an_approval_write_waits_for_approval_and_never_posts_twice() {
    let python_name = if cfg!(windows) { "python" } else { "python3" };
    let langs = [("node", "node"), ("python", python_name)];

    for (lang, interpreter) in &langs {
        if !have(interpreter, "--version") {
            eprintln!("skipping: {} not found", lang);
            continue;
        }

        // (a) run without approval
        let tmp1 = init("approval-write", lang);
        let app_dir1 = tmp1.path().join("app");
        let out1 = pp(&["--json", "nest", "run", app_dir1.to_str().unwrap()]);

        if out1.status.success() {
            panic!("run without approval should fail for {}", lang);
        }

        let json1 = json_of(&out1);
        let job1 = &json1["jobs"].as_array().unwrap()[0];

        assert_eq!(job1["final"]["state"].as_str().unwrap(), "failed");
        assert_eq!(
            job1["final"]["result"]["error_code"].as_str().unwrap(),
            "approval_required"
        );

        // Check events have an effect with state awaiting_approval
        let attempts = job1["attempts"].as_array().unwrap();
        let events = attempts[0]["events"].as_array().unwrap();
        let has_awaiting_effect = events.iter().any(|e| {
            e["type"].as_str().unwrap() == "effect"
                && e["state"].as_str().unwrap() == "awaiting_approval"
        });
        assert!(
            has_awaiting_effect,
            "should have an effect event with state awaiting_approval"
        );

        // (b) run with approve-all and redeliver
        let tmp2 = init("approval-write", lang);
        let app_dir2 = tmp2.path().join("app");
        let out2 = pp(&[
            "--json",
            "nest",
            "run",
            app_dir2.to_str().unwrap(),
            "--approve-all",
            "--redeliver",
        ]);

        if !out2.status.success() {
            panic!(
                "run with approve-all and redeliver failed for {} with status {:?}\nstdout: {}\nstderr: {}",
                lang,
                out2.status,
                String::from_utf8_lossy(&out2.stdout),
                String::from_utf8_lossy(&out2.stderr)
            );
        }

        let json2 = json_ok(&out2);
        let job2 = &json2["jobs"].as_array().unwrap()[0];

        assert_eq!(job2["final"]["state"].as_str().unwrap(), "succeeded");

        let attempts = job2["attempts"].as_array().unwrap();
        assert_eq!(attempts.len(), 3, "should have 3 attempts");

        // Attempt 2 (index 1) has effect event state done deduplicated false
        let events_2 = attempts[1]["events"].as_array().unwrap();
        let has_done_not_dedup = events_2.iter().any(|e| {
            e["type"].as_str().unwrap() == "effect"
                && e["state"].as_str().unwrap() == "done"
                && e.get("deduplicated").and_then(|v| v.as_bool()) != Some(true)
        });
        assert!(
            has_done_not_dedup,
            "attempt 2 should have effect event state done deduplicated false"
        );

        // Attempt 3 (index 2) has effect event state done deduplicated true
        let events_3 = attempts[2]["events"].as_array().unwrap();
        let has_done_dedup = events_3.iter().any(|e| {
            e["type"].as_str().unwrap() == "effect"
                && e["state"].as_str().unwrap() == "done"
                && e.get("deduplicated")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
        });
        assert!(
            has_done_dedup,
            "attempt 3 should have effect event state done deduplicated true"
        );

        // final.result.answer.posted == true and reference == "eff_local_1"
        let answer = &job2["final"]["result"]["answer"];
        assert!(answer["posted"].as_bool().unwrap());
        assert_eq!(answer["reference"].as_str().unwrap(), "eff_local_1");
    }
}

#[test]
fn jobs_are_checked_like_admission() {
    // Use node docs-qa, no interpreter needed since it fails before start
    let tmp = init("docs-qa", "node");
    let app_dir = tmp.path().join("app");

    // Bad job file 1: unknown field in payload
    let bad_job_1 =
        r#"{"operation":"docs.ask","operation_version":"1.0","payload":{"question":"hi","x":"y"}}"#;
    std::fs::write(app_dir.join("jobs").join("bad1.json"), bad_job_1).unwrap();

    let out1 = pp(&[
        "--json",
        "nest",
        "run",
        app_dir.to_str().unwrap(),
        "--job",
        app_dir.join("jobs").join("bad1.json").to_str().unwrap(),
    ]);

    if out1.status.success() {
        panic!("bad job 1 should fail");
    }
    let stderr1 = String::from_utf8_lossy(&out1.stderr);
    assert!(
        stderr1.contains("invalid_operation_payload"),
        "stderr should contain invalid_operation_payload: {}",
        stderr1
    );

    // Bad job file 2: operation not offered
    let bad_job_2 = r#"{"operation":"nonexistent.op","operation_version":"1.0","payload":{}}"#;
    std::fs::write(app_dir.join("jobs").join("bad2.json"), bad_job_2).unwrap();

    let out2 = pp(&[
        "--json",
        "nest",
        "run",
        app_dir.to_str().unwrap(),
        "--job",
        app_dir.join("jobs").join("bad2.json").to_str().unwrap(),
    ]);

    if out2.status.success() {
        panic!("bad job 2 should fail");
    }
    let stderr2 = String::from_utf8_lossy(&out2.stderr);
    assert!(
        stderr2.contains("offers no operation"),
        "stderr should contain offers no operation: {}",
        stderr2
    );
}

#[test]
fn template_unit_tests_pass() {
    let python_name = if cfg!(windows) { "python" } else { "python3" };
    let langs = [("node", "node"), ("python", python_name)];

    for template in ["tool", "docs-qa", "approval-write"] {
        for (lang, interpreter) in &langs {
            if !have(interpreter, "--version") {
                eprintln!("skipping: {} not found", lang);
                continue;
            }

            let tmp = init(template, lang);
            let app_dir = tmp.path().join("app");

            let test_out = if *lang == "node" {
                Command::new("node")
                    .args(["--test", "app.test.mjs"])
                    .current_dir(&app_dir)
                    .output()
                    .expect("failed to run node --test")
            } else {
                Command::new(interpreter)
                    .args(["-m", "unittest", "test_app"])
                    .current_dir(&app_dir)
                    .output()
                    .expect("failed to run python -m unittest")
            };

            if !test_out.status.success() {
                panic!(
                    "unit tests failed for {} {}: \nstdout: {}\nstderr: {}",
                    template,
                    lang,
                    String::from_utf8_lossy(&test_out.stdout),
                    String::from_utf8_lossy(&test_out.stderr)
                );
            }
        }
    }
}
