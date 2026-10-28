use assert_cmd::Command;
use predicates::prelude::*;

fn bobbin() -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("bobbin"));
    command
        .env("BOBBIN_QUIPU_REMOTE", "")
        .env("QUIPU_SERVER", "http://127.0.0.1:0");
    command
}

#[test]
fn breadcrumb_cli_crud_round_trip() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_str().unwrap();

    bobbin()
        .args([
            "bc",
            "create",
            "auth-refresh",
            "token refresh flow",
            "Authentication refresh context",
            "--pin",
            "src/auth.rs,src/token.rs",
            "--tag",
            "auth,security",
            "--on",
            "refresh_token,token_expiry",
            "--path",
            root,
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Created breadcrumb 'auth-refresh'",
        ));

    bobbin()
        .args(["--json", "bc", "list", "--path", root])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"name\": \"auth-refresh\""));

    bobbin()
        .args(["bc", "recall", "auth-refresh", "--path", root])
        .assert()
        .success()
        .stdout(predicate::str::contains("Query: token refresh flow"))
        .stdout(predicate::str::contains("src/auth.rs"));

    bobbin()
        .args(["bc", "delete", "auth-refresh", "--path", root])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Deleted breadcrumb 'auth-refresh'",
        ));

    bobbin()
        .args(["bc", "recall", "auth-refresh", "--path", root])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Breadcrumb 'auth-refresh' not found",
        ));
}

#[test]
fn aliases_are_visible_and_share_handlers() {
    bobbin()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("mark"))
        .stdout(predicate::str::contains("recall"));

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_str().unwrap();
    bobbin()
        .args([
            "mark",
            "auth-refresh",
            "token refresh flow",
            "Authentication refresh context",
            "--path",
            root,
        ])
        .assert()
        .success();
    bobbin()
        .args(["recall", "auth-refresh", "--path", root])
        .assert()
        .success()
        .stdout(predicate::str::contains("token refresh flow"));
}

#[test]
fn corrupt_store_fails_without_erasing_it() {
    let temp = tempfile::tempdir().unwrap();
    let store_dir = temp.path().join(".bobbin");
    std::fs::create_dir_all(&store_dir).unwrap();
    let store = store_dir.join("breadcrumbs.json");
    std::fs::write(&store, "{broken").unwrap();

    bobbin()
        .args([
            "mark",
            "auth-refresh",
            "token refresh flow",
            "Authentication refresh context",
            "--path",
            temp.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("file left unchanged"));
    assert_eq!(std::fs::read_to_string(store).unwrap(), "{broken");
}

fn discovery_project() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    bobbin()
        .args(["init", temp.path().to_str().unwrap()])
        .assert()
        .success();
    bobbin()
        .args([
            "mark",
            "auth-refresh",
            "token refresh flow",
            "Authentication refresh context",
            "--pin",
            "src/auth.rs",
            "--path",
            temp.path().to_str().unwrap(),
        ])
        .assert()
        .success();
    temp
}

fn session_hook(
    root: &std::path::Path,
    command: &str,
    source: &str,
    budget: Option<&str>,
) -> String {
    let mut cmd = bobbin();
    cmd.env_remove("BOBBIN_SERVER")
        .current_dir(root)
        .args(["hook", command]);
    if let Some(budget) = budget {
        cmd.args(["--budget", budget]);
    }
    let output = cmd
        .write_stdin(
            serde_json::json!({"source": source, "cwd": root,
        "session_id": "breadcrumb-discovery-test"})
            .to_string(),
        )
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    if output.is_empty() {
        return String::new();
    }
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["hookSpecificOutput"]["hookEventName"], "SessionStart");
    json["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn startup_discovers_cli_created_breadcrumb_without_recalling_it() {
    let temp = discovery_project();
    let store = temp.path().join(".bobbin/breadcrumbs.json");
    let before = std::fs::read(&store).unwrap();
    let modified = std::fs::metadata(&store).unwrap().modified().unwrap();
    let context = session_hook(temp.path(), "prime-context", "startup", None);
    assert!(context.contains("Index Status"), "ordinary primer lost");
    assert!(context.contains("=== Breadcrumbs ===\n- auth-refresh: Authentication refresh context"));
    assert_eq!(
        before,
        std::fs::read(&store).unwrap(),
        "discovery must not recall or rewrite"
    );
    assert_eq!(
        modified,
        std::fs::metadata(store).unwrap().modified().unwrap()
    );
}

#[test]
fn empty_store_preserves_primer_and_small_budget_omits_discovery() {
    let temp = discovery_project();
    assert!(std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(temp.path())
        .status()
        .unwrap()
        .success());
    let compact = session_hook(temp.path(), "session-context", "compact", Some("3"));
    assert!(!compact.is_empty(), "ordinary working context lost");
    assert!(!compact.contains("=== Breadcrumbs ==="));
    assert!(compact.lines().count() <= 3);
    std::fs::remove_file(temp.path().join(".bobbin/breadcrumbs.json")).unwrap();
    let context = session_hook(temp.path(), "prime-context", "startup", None);
    assert!(context.contains("Index Status"));
    assert!(!context.contains("=== Breadcrumbs ==="));
}

#[test]
fn multiline_descriptions_and_many_entries_cannot_exceed_discovery_budget() {
    let temp = discovery_project();
    let store = temp.path().join(".bobbin/breadcrumbs.json");
    let mut data: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&store).unwrap()).unwrap();
    for index in 0..100 {
        let name = format!("entry-{index:03}");
        let mut entry = data["auth-refresh"].clone();
        entry["name"] = name.clone().into();
        entry["description"] = "first line\nsecond line\n".repeat(100).into();
        data[&name] = entry;
    }
    std::fs::write(store, serde_json::to_vec(&data).unwrap()).unwrap();
    let context = session_hook(temp.path(), "prime-context", "startup", None);
    let discovery = context.split("=== Breadcrumbs ===\n").nth(1).unwrap();
    assert_eq!(discovery.lines().count(), 29);
    assert!(discovery.lines().all(|line| line.chars().count() <= 485));
}

#[test]
fn compaction_discovers_breadcrumb_from_nested_working_directory_within_budget() {
    let temp = discovery_project();
    let nested = temp.path().join("src");
    std::fs::create_dir(&nested).unwrap();
    let context = session_hook(&nested, "session-context", "compact", Some("10"));
    assert!(context.contains("auth-refresh"));
    assert!(context.lines().count() <= 10, "over budget: {context}");
}

#[test]
fn corrupt_breadcrumb_store_preserves_startup_primer_and_store() {
    let temp = discovery_project();
    let store = temp.path().join(".bobbin/breadcrumbs.json");
    std::fs::write(&store, "{broken").unwrap();
    let context = session_hook(temp.path(), "prime-context", "startup", None);
    assert!(context.contains("Index Status"));
    assert!(!context.contains("=== Breadcrumbs ==="));
    assert_eq!(std::fs::read_to_string(store).unwrap(), "{broken");
}

#[test]
fn expired_breadcrumb_is_not_discovered_but_live_control_is() {
    let temp = discovery_project();
    let store = temp.path().join(".bobbin/breadcrumbs.json");
    let mut data: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&store).unwrap()).unwrap();
    let mut expired = data["auth-refresh"].clone();
    expired["name"] = "expired".into();
    expired["created_at"] = "2000-01-01T00:00:00Z".into();
    expired["ttl_days"] = 1.into();
    data["expired"] = expired;
    std::fs::write(&store, serde_json::to_vec(&data).unwrap()).unwrap();
    let context = session_hook(temp.path(), "prime-context", "startup", None);
    assert!(context.contains("auth-refresh"));
    assert!(!context.contains("expired"));
}
