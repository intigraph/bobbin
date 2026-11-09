//! The generation digest must survive fresh HashMap seeds in actual CLI processes.
use std::path::Path;
use std::process::{Command, Output};

fn config(reverse: bool, changed: bool) -> String {
    let mut text = String::from(
        "[embedding]\nbackend=\"openai-api\"\nmodel=\"fixture\"\ndimensions=4\n\
         [embedding.api]\nurl=\"http://127.0.0.1:1/v1/embeddings\"\n\
         [git]\ncoupling_enabled=false\ncommits_enabled=false\n[sources.repos]\n",
    );
    let mut keys: Vec<_> = (0..8).collect();
    if reverse {
        keys.reverse();
    }
    for i in &keys {
        text.push_str(&format!(
            "r{i}=\"https://example.invalid/{i}/{}/{{path}}\"\n",
            if changed { "changed" } else { "original" }
        ));
    }
    text.push_str("[sources.forge_overrides]\n");
    for i in &keys {
        text.push_str(&format!("\"f{i}.invalid\"=\"forgejo\"\n"));
    }
    text
}

fn invoke(root: &Path, source: &Path, flag: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bobbin"))
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("home/config"))
        .args(["index", flag, "fixture", "--repo", "fixture", "--source"])
        .arg(source)
        .args(["--maintenance", "never", "--skip-calibrate"])
        .arg(root)
        .output()
        .expect("launch isolated CLI process")
}

fn success(output: &Output) {
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn unchanged_maps_resume_across_processes_but_changed_identity_refuses() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    for dir in [".bobbin", "source", "other-source", "home/config"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    let path = root.join(".bobbin/config.toml");
    let source = root.join("source");
    std::fs::write(&path, config(false, false)).unwrap();
    // Empty source means the unused loopback API receives no embedding request.
    success(&invoke(root, &source, "--start-generation"));
    let first = invoke(root, &source, "--resume-generation");
    success(&first);
    let receipt: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(receipt["phase"], "Complete");
    for i in 0..8 {
        std::fs::write(&path, config(i % 2 == 0, false)).unwrap();
        let output = invoke(root, &source, "--resume-generation");
        success(&output);
        let resumed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(resumed, receipt, "resume must retain the original receipt");
    }
    std::fs::write(&path, config(false, true)).unwrap();
    let changed = invoke(root, &source, "--resume-generation");
    assert!(!changed.status.success());
    assert!(String::from_utf8_lossy(&changed.stderr).contains("generation identity mismatch"));
    std::fs::write(&path, config(false, false)).unwrap();
    let moved = invoke(root, &root.join("other-source"), "--resume-generation");
    assert!(!moved.status.success());
    assert!(String::from_utf8_lossy(&moved.stderr).contains("generation identity mismatch"));
    success(&invoke(root, &source, "--resume-generation"));
}
