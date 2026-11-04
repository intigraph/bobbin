#!/usr/bin/env python3
"""Run the real feedback store from two revisions in isolated SQLite fixtures."""

import argparse
import json
import pathlib
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", required=True)
    parser.add_argument("--after", default="working")
    args = parser.parse_args()
    root = pathlib.Path(subprocess.check_output(["git", "rev-parse", "--show-toplevel"], text=True).strip())

    def source(revision, path):
        if revision == "working":
            return (root / path).read_text()
        return subprocess.check_output(["git", "show", f"{revision}:{path}"], cwd=root, text=True)

    with tempfile.TemporaryDirectory(prefix="memory-usage-demo-") as directory:
        for label, revision in [("before", args.before), ("after", args.after)]:
            project = pathlib.Path(directory) / label
            (project / "src").mkdir(parents=True)
            (project / "Cargo.toml").write_text('''[package]
name = "memory-usage-demo"
version = "0.0.0"
edition = "2021"
[dependencies]
anyhow = "1"
rusqlite = { version = "0.40", features = ["bundled"] }
chrono = { version = "0.4", features = ["serde"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
''')
            (project / "Cargo.lock").write_text(source(revision, "Cargo.lock"))
            store = source(revision, "src/storage/feedback.rs")
            (project / "src/feedback.rs").write_text(store)
            if 'mod usage;' in store:
                (project / "src/feedback_usage.rs").write_text(source(revision, "src/storage/feedback_usage.rs"))
            recall = source(revision, "src/memory_recall/recall.rs")
            start = recall.rfind("#[derive(", 0, recall.index("pub struct CitedItem"))
            end = recall.index("\n}", start) + 2
            cited_type = recall[start:end]
            (project / "src/main.rs").write_text('''#![allow(dead_code)]
mod memory_recall {
use serde::{Serialize, Deserialize};
''' + cited_type + '''
}
mod feedback;
use feedback::{FeedbackInput, FeedbackStore};
fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).unwrap();
    let store = FeedbackStore::open(std::path::Path::new(&path))?;
    let mut item = memory_recall::CitedItem {
        item: "m1".into(), iri: "urn:fixture:original".into(), kind: "Decision".into(),
        natural_key: "urn:fixture:key".into(), line: "fixture evidence".into(),
    };
    store.store_injection_items("inj-fixture", &[item.clone()])?;
    let input = FeedbackInput { injection_id: "inj-fixture".into(), agent: "fixture".into(),
        rating: "used".into(), reason: "acted on evidence".into(), item: Some("m1".into()) };
    let first = store.store_feedback(&input)?;
    let replay = store.store_feedback(&input)?;
    let used = store.entity_usage(10)?[0].used;
    item.iri = "urn:fixture:replacement".into();
    let retarget_rejected = store.store_injection_items("inj-fixture", &[item]).is_err();
    println!("{}", serde_json::json!({"same_receipt":first==replay,
        "used_after_replay":used,"retarget_rejected":retarget_rejected,
        "entities":store.entity_usage(10)?}));
    Ok(())
}
''')
            output = subprocess.check_output(["cargo", "run", "--offline", "--quiet", "--manifest-path", str(project / "Cargo.toml"), "--", str(project / "feedback.db")], text=True)
            result = json.loads(output)
            print(f"{label} ({revision}): {json.dumps(result, sort_keys=True)}", flush=True)
            expected = label == "after"
            assert result["same_receipt"] == expected
            assert result["retarget_rejected"] == expected
            assert result["used_after_replay"] == (1 if expected else 2)


if __name__ == "__main__":
    main()
