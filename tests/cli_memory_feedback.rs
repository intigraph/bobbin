//! Real CLI maintenance with local fixtures; preview must not contact Quipu.
#![cfg(feature = "knowledge")]

use assert_cmd::Command;
use serde_json::{json, Value};
use std::path::Path;

fn command(root: &Path, mirror: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bobbin"));
    command
        .current_dir(root)
        .env("BOBBIN_SERVER", "")
        .env("BOBBIN_QUIPU_REMOTE", "")
        .env("QUIPU_SERVER", "http://127.0.0.1:0")
        .env("BOBBIN_MEMORY_MIRROR", mirror);
    command
}

fn mirror(endpoint: &str, refreshed: u64) -> Value {
    let mut items = serde_json::Map::new();
    for name in ["unused", "used"] {
        let iri = format!("urn:fixture:{name}");
        items.insert(
            iri.clone(),
            json!({"iri":iri,"natural_key":format!("urn:key:{name}"),
            "kind":"Decision","label":name,"source_kind":"crew:declared",
            "recorded_at":"2020-01-01T00:00:00Z","review_after":"2099-01-01T00:00:00Z"}),
        );
    }
    json!({"endpoint":endpoint,"items":items,"refreshed_at":refreshed})
}

#[test]
fn preview_observes_never_injected_records_and_excludes_recent_use() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".bobbin")).unwrap();
    std::fs::write(dir.path().join(".bobbin/config.toml"), "").unwrap();
    let path = dir.path().join("mirror.json");
    let endpoint = "https://graph.example";
    std::fs::write(
        &path,
        mirror(endpoint, chrono::Utc::now().timestamp() as u64).to_string(),
    )
    .unwrap();
    let invoke = || {
        let result = command(dir.path(), &path)
            .args(["feedback", "review-due", "--endpoint", endpoint])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice::<Value>(&result).unwrap()
    };
    let first = invoke();
    assert_eq!(first["observed"], 2);
    assert_eq!(first["considered"], 0);
    let db = rusqlite::Connection::open(dir.path().join(".bobbin/feedback.db")).unwrap();
    db.execute(
        "UPDATE memory_observations SET observed_since = '2020-01-01T00:00:00Z'",
        [],
    )
    .unwrap();
    db.execute_batch("INSERT INTO injections (injection_id, timestamp) VALUES ('inj', '2020-01-01T00:00:00Z');
        INSERT INTO injection_items (injection_id, item, iri) VALUES ('inj', 'm1', 'urn:fixture:used');").unwrap();
    command(dir.path(), &path)
        .args([
            "feedback",
            "submit",
            "--injection",
            "inj",
            "--item",
            "m1",
            "--rating",
            "used",
            "--agent",
            "fixture",
        ])
        .assert()
        .success();
    let preview = invoke();
    assert_eq!(preview["mode"], "preview");
    assert_eq!(preview["changes"].as_array().unwrap().len(), 1);
    assert_eq!(preview["changes"][0]["iri"], "urn:fixture:unused");
    assert_eq!(preview["changes"][0]["outcome"], "preview");
    assert_eq!(
        preview["changes"][0]["previous_review_after"],
        "2099-01-01T00:00:00Z"
    );
}

#[test]
fn cli_refuses_stale_mismatched_and_remote_authority() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".bobbin")).unwrap();
    std::fs::write(dir.path().join(".bobbin/config.toml"), "").unwrap();
    let path = dir.path().join("mirror.json");
    let endpoint = "https://graph.example";
    std::fs::write(&path, mirror(endpoint, 0).to_string()).unwrap();
    command(dir.path(), &path)
        .args(["feedback", "review-due", "--endpoint", endpoint])
        .assert()
        .failure()
        .stderr(predicates::str::contains("stale"));
    command(dir.path(), &path)
        .args([
            "feedback",
            "review-due",
            "--endpoint",
            "https://other.example",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("endpoint differs"));
    command(dir.path(), &path)
        .args([
            "--server",
            "http://127.0.0.1:9",
            "feedback",
            "review-due",
            "--endpoint",
            endpoint,
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("authoritative local"));
}
