use super::*;

#[test]
fn the_bundle_is_valid_owned_by_bobbin_and_versioned_by_the_build() {
    let b = bundle();
    assert_eq!(b["schema"], "st.hook-bundle/1");
    assert_eq!(b["name"], "bobbin");
    assert_eq!(b["owner"], "bobbin");
    assert_eq!(b["version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn a_hook_without_harnesses_applies_to_both() {
    let b = bundle();
    let claude = hooks_for(&b, Harness::Claude);
    let codex = hooks_for(&b, Harness::Codex);
    assert!(claude.len() > codex.len(), "claude-only hooks exist");
    for hook in &codex {
        assert!(claude.contains(hook), "{hook:?} is codex-only");
    }
    assert!(codex.iter().any(|h| h.0 == "UserPromptSubmit"));
}

#[test]
fn merge_is_idempotent_and_keeps_foreign_hooks() {
    let b = bundle();
    let mut cfg = json!({"hooks": {"UserPromptSubmit": [
        {"hooks": [{"type": "command", "command": "other-tool hook"}]}
    ]}, "model": "x"});
    let want = hooks_for(&b, Harness::Claude).len();
    assert_eq!(merge(&mut cfg, &b, Harness::Claude), want);
    assert_eq!(merge(&mut cfg, &b, Harness::Claude), 0);
    assert_eq!(present(&cfg, &b, Harness::Claude), (want, want));
    assert_eq!(cfg["model"], "x");
    let ups = &cfg["hooks"]["UserPromptSubmit"][0]["hooks"];
    assert!(ups
        .as_array()
        .unwrap()
        .iter()
        .any(|h| h["command"] == "other-tool hook"));
}

#[test]
fn remove_takes_only_bobbins_hooks() {
    let b = bundle();
    let mut cfg = json!({"hooks": {"UserPromptSubmit": [
        {"hooks": [{"type": "command", "command": "other-tool hook"}]}
    ]}});
    merge(&mut cfg, &b, Harness::Claude);
    let n = hooks_for(&b, Harness::Claude).len();
    assert_eq!(remove(&mut cfg, &b, Harness::Claude), n);
    assert_eq!(present(&cfg, &b, Harness::Claude).0, 0);
    assert_eq!(
        cfg["hooks"]["UserPromptSubmit"][0]["hooks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(cfg["hooks"].get("PostToolUseFailure").is_none());
}

#[test]
fn codex_config_round_trips_through_toml() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "model = \"gpt\"\n[mcp_servers.x]\ncommand = \"x\"\n").unwrap();
    let b = bundle();
    let mut cfg = read_config(&path, Harness::Codex).unwrap();
    assert!(merge(&mut cfg, &b, Harness::Codex) > 0);
    write_config(&path, Harness::Codex, &cfg).unwrap();
    let back = read_config(&path, Harness::Codex).unwrap();
    let n = hooks_for(&b, Harness::Codex).len();
    assert_eq!(present(&back, &b, Harness::Codex), (n, n));
    assert_eq!(back["model"], "gpt");
    assert_eq!(back["mcp_servers"]["x"]["command"], "x");
    assert!(dir.path().join("config.toml.bak-bobbin").exists());
}

/// The UserPromptSubmit evidence must find metrics under the shanty root the
/// deployment names, not only the `~/gt/*/crew/*` layout (aegis-9uq.3: on a
/// Mac the live file is `~/workspace/.shanty/.bobbin/metrics.jsonl`).
#[test]
fn prompt_evidence_resolves_metrics_from_the_shanty_root() {
    let b = bundle();
    let cmd = b["hooks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["event"] == "UserPromptSubmit")
        .and_then(|h| h["evidence"]["command"].as_str())
        .expect("UserPromptSubmit evidence")
        .to_string();
    if std::process::Command::new("python3")
        .arg("-V")
        .output()
        .is_err()
    {
        return; // evidence is a python3 one-liner; nothing to exercise here
    }
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let root = home.join("workspace/.shanty");
    std::fs::create_dir_all(root.join(".bobbin")).unwrap();
    std::fs::write(
        root.join(".bobbin/metrics.jsonl"),
        "{\"event_type\":\"hook_injection\",\"timestamp\":\"2026-11-27T10:00:00Z\"}\n",
    )
    .unwrap();
    let cfg = tmp.path().join("cfg");
    std::fs::create_dir_all(cfg.join("shantytown")).unwrap();
    std::fs::write(cfg.join("shantytown/root"), format!("{}\n", root.display())).unwrap();
    let run = |cfg_home: &std::path::Path, shanty_root: Option<&std::path::Path>| {
        let mut c = std::process::Command::new("sh");
        c.arg("-c")
            .arg(&cmd)
            .current_dir(tmp.path())
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", cfg_home)
            .env_remove("SHANTY_ROOT");
        if let Some(r) = shanty_root {
            c.env("SHANTY_ROOT", r);
        }
        c.output().unwrap()
    };
    let missing = tmp.path().join("no-config");
    for out in [run(&cfg, None), run(&missing, Some(&root))] {
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "1795773600");
    }
    assert!(!run(&missing, None).status.success());
}
