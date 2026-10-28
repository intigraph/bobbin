//! Compiled CLI contract checks. No embeddings, network or real HOME required.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::SystemTime;

fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bobbin"))
        .current_dir(root)
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env_remove("BOBBIN_SERVER")
        .args(["prime"])
        .args(args)
        .output()
        .unwrap()
}

fn snapshot(root: &Path) -> Vec<(PathBuf, SystemTime, Vec<u8>)> {
    fn visit(root: &Path, p: &Path, out: &mut Vec<(PathBuf, SystemTime, Vec<u8>)>) {
        let meta = std::fs::symlink_metadata(p).unwrap();
        out.push((
            p.strip_prefix(root).unwrap().to_owned(),
            meta.modified().unwrap(),
            if meta.is_file() {
                std::fs::read(p).unwrap()
            } else {
                vec![]
            },
        ));
        if meta.is_dir() {
            for e in std::fs::read_dir(p).unwrap() {
                visit(root, &e.unwrap().path(), out);
            }
        }
    }
    let mut out = vec![];
    visit(root, root, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn prime_contract_is_offline_read_only_and_bounded() {
    let root = tempfile::tempdir().unwrap();
    // A .bobbin directory makes an accidental post-command metric write visible.
    std::fs::create_dir(root.path().join(".bobbin")).unwrap();
    let before = snapshot(root.path());
    for offline in [false, true] {
        for brief in [false, true] {
            let mut args = vec!["--json"];
            if offline {
                args.push("--static");
            }
            if brief {
                args.push("--brief");
            }
            let out = run(root.path(), &args);
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(v["schema"], "stack-prime/1");
            assert_eq!(v["tool"], "bobbin");
            assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
            assert_eq!(v["primer"].get("text").is_none(), brief);
            assert!(v["primer"]["sections"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s == "Agent workflow"));
            assert_eq!(
                v["live"]["status"],
                if offline { "skipped" } else { "absent" }
            );
            assert_eq!(v["status"], if offline { "ok" } else { "degraded" });
            assert!(v["live"]["elapsed_ms"].as_u64().unwrap() < 1000);
            assert_eq!(snapshot(root.path()), before);
        }
        let args = if offline {
            vec!["--brief", "--static"]
        } else {
            vec!["--brief"]
        };
        let out = run(root.path(), &args);
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).unwrap();
        assert!((5..=10).contains(&text.lines().count()), "{text}");
        assert!(text.starts_with(concat!("bobbin ", env!("CARGO_PKG_VERSION"), ":")));
        assert!(text.ends_with("Run `bobbin prime` for the full primer.\n"));
    }
    assert_eq!(snapshot(root.path()), before);
}

#[test]
fn absent_project_and_garbage_index_are_not_created_or_repaired() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("does-not-exist");
    let out = run(root.path(), &[missing.to_str().unwrap(), "--json"]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["live"]["status"], "absent");
    assert!(!missing.exists());
    let table = root.path().join(".bobbin/vectors/chunks.lance");
    std::fs::create_dir_all(table.parent().unwrap()).unwrap();
    std::fs::write(&table, b"not a table").unwrap();
    let before = snapshot(root.path());
    let out = run(root.path(), &["--json"]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["live"]["status"], "unreadable");
    assert_eq!(snapshot(root.path()), before);
}

#[test]
fn section_aliases_and_invalid_arguments() {
    let root = tempfile::tempdir().unwrap();
    for section in ["MCP", "commands", "what bobbin does", "agent"] {
        let out = run(root.path(), &["--static", "--section", section]);
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).starts_with("## "));
    }
    for args in [
        &["--section", "nonesuch"][..],
        &["--unknown"][..],
        &["--section"][..],
    ] {
        assert_eq!(run(root.path(), args).status.code(), Some(2));
    }
}

#[test]
fn quiet_keeps_the_primer_without_a_live_appendix() {
    let root = tempfile::tempdir().unwrap();
    let out = run(root.path(), &["--quiet"]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("# Bobbin agent primer"));
    assert!(!text.contains("## Live state"));
}
