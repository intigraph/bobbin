//! Tests for the index command internals (sidecar of `index.rs`).

use super::*;
use crate::storage::MaintenanceOutcome;
use crate::types::ChunkType;
use tempfile::tempdir;

#[test]
fn maintenance_error_is_not_reported_as_success() {
    let err =
        MaintenanceReport::from_result(Err(anyhow::anyhow!("Failed to compact chunks table")))
            .expect_err("maintenance failures must fail the index command");

    assert_eq!(
        format!("{err:#}"),
        "Lance maintenance failed: Failed to compact chunks table"
    );
}

#[test]
fn maintenance_lock_contention_remains_an_explicit_outcome() {
    let report = MaintenanceReport::from_result(Ok(MaintenanceOutcome::SkippedLockHeld {
        waited: std::time::Duration::from_secs(120),
    }))
    .expect("lock contention is a modeled non-error outcome");

    assert_eq!(report.json_label(), "skipped_lock_held");
}

#[test]
fn test_compute_hash() {
    let hash1 = compute_hash("hello world");
    let hash2 = compute_hash("hello world");
    let hash3 = compute_hash("different content");

    assert_eq!(hash1, hash2);
    assert_ne!(hash1, hash3);
    assert_eq!(hash1.len(), 64);
}

#[test]
fn test_build_context_windows_enabled_language() {
    let file_content = "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10";
    let chunks = vec![Chunk {
        id: "c1".to_string(),
        file_path: "doc.md".to_string(),
        chunk_type: ChunkType::Section,
        name: Some("Section".to_string()),
        start_line: 4,
        end_line: 6,
        content: "line4\nline5\nline6".to_string(),
        language: "markdown".to_string(),
        tags: String::new(),
    }];
    let config = ContextualEmbeddingConfig {
        context_lines: 2,
        enabled_languages: vec!["markdown".to_string()],
    };

    let contexts = build_context_windows(&chunks, file_content, &config);
    assert_eq!(contexts.len(), 1);
    let ctx = contexts[0].as_ref().unwrap();
    // Should include lines 2-3 (prefix), 4-6 (content), 7-8 (suffix)
    assert!(ctx.contains("line2"));
    assert!(ctx.contains("line3"));
    assert!(ctx.contains("line4"));
    assert!(ctx.contains("line5"));
    assert!(ctx.contains("line6"));
    assert!(ctx.contains("line7"));
    assert!(ctx.contains("line8"));
    assert!(!ctx.contains("line1"));
    assert!(!ctx.contains("line9"));
}

#[test]
fn test_build_context_windows_disabled_language() {
    let file_content = "line1\nline2\nline3\nline4\nline5";
    let chunks = vec![Chunk {
        id: "c1".to_string(),
        file_path: "main.rs".to_string(),
        chunk_type: ChunkType::Function,
        name: Some("main".to_string()),
        start_line: 2,
        end_line: 4,
        content: "line2\nline3\nline4".to_string(),
        language: "rust".to_string(),
        tags: String::new(),
    }];
    let config = ContextualEmbeddingConfig {
        context_lines: 2,
        enabled_languages: vec!["markdown".to_string()],
    };

    let contexts = build_context_windows(&chunks, file_content, &config);
    assert_eq!(contexts.len(), 1);
    assert!(contexts[0].is_none()); // Rust not enabled
}

#[test]
fn test_build_context_windows_at_file_boundaries() {
    let file_content = "line1\nline2\nline3";
    let chunks = vec![Chunk {
        id: "c1".to_string(),
        file_path: "doc.md".to_string(),
        chunk_type: ChunkType::Section,
        name: Some("All".to_string()),
        start_line: 1,
        end_line: 3,
        content: "line1\nline2\nline3".to_string(),
        language: "markdown".to_string(),
        tags: String::new(),
    }];
    let config = ContextualEmbeddingConfig {
        context_lines: 5,
        enabled_languages: vec!["markdown".to_string()],
    };

    let contexts = build_context_windows(&chunks, file_content, &config);
    // No surrounding lines available, should return None
    assert!(contexts[0].is_none());
}

#[test]
fn test_build_context_windows_zero_lines() {
    let file_content = "line1\nline2\nline3";
    let chunks = vec![Chunk {
        id: "c1".to_string(),
        file_path: "doc.md".to_string(),
        chunk_type: ChunkType::Section,
        name: Some("S".to_string()),
        start_line: 2,
        end_line: 2,
        content: "line2".to_string(),
        language: "markdown".to_string(),
        tags: String::new(),
    }];
    let config = ContextualEmbeddingConfig {
        context_lines: 0,
        enabled_languages: vec!["markdown".to_string()],
    };

    let contexts = build_context_windows(&chunks, file_content, &config);
    assert!(contexts[0].is_none()); // context_lines=0 disables
}

#[test]
fn test_collect_files_respects_patterns() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    std::fs::write(root.join("main.rs"), "fn main() {}").unwrap();
    std::fs::write(root.join("lib.rs"), "pub fn lib() {}").unwrap();
    std::fs::write(root.join("test.txt"), "not code").unwrap();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/mod.rs"), "mod test;").unwrap();

    let config = Config::default();
    let files = collect_files(root, &config).unwrap();

    let rs_files: Vec<_> = files
        .iter()
        .filter(|p| p.extension().map(|e| e == "rs").unwrap_or(false))
        .collect();
    assert_eq!(rs_files.len(), 3);

    let txt_files: Vec<_> = files
        .iter()
        .filter(|p| p.extension().map(|e| e == "txt").unwrap_or(false))
        .collect();
    assert!(txt_files.is_empty());
}

#[test]
fn test_collect_files_multimodal_flag_gates_pdf() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    std::fs::write(root.join("main.rs"), "fn main() {}").unwrap();
    std::fs::write(root.join("runbook.pdf"), b"%PDF-1.4 dummy").unwrap();

    // Off by default: PDFs are not walked.
    let mut config = Config::default();
    assert!(!config.index.multimodal);
    let files = collect_files(root, &config).unwrap();
    assert!(!files
        .iter()
        .any(|p| p.extension().map(|e| e == "pdf").unwrap_or(false)));

    // Opt-in: the flag makes the walker pick up PDFs.
    config.index.multimodal = true;
    let files = collect_files(root, &config).unwrap();
    assert!(files
        .iter()
        .any(|p| p.file_name().map(|n| n == "runbook.pdf").unwrap_or(false)));
}

#[test]
fn test_collect_files_excludes_patterns() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    std::fs::write(root.join("main.rs"), "fn main() {}").unwrap();
    std::fs::create_dir_all(root.join("target/debug")).unwrap();
    std::fs::write(root.join("target/debug/lib.rs"), "// build artifact").unwrap();
    std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
    std::fs::write(root.join("node_modules/pkg/index.js"), "// npm").unwrap();

    let config = Config::default();
    let files = collect_files(root, &config).unwrap();

    assert!(files
        .iter()
        .any(|p| p.file_name().map(|n| n == "main.rs").unwrap_or(false)));

    assert!(!files
        .iter()
        .any(|p| p.to_string_lossy().contains("target/")));
    assert!(!files
        .iter()
        .any(|p| p.to_string_lossy().contains("node_modules/")));
}

#[test]
fn test_collect_files_documents_flag_gates_html() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    std::fs::write(root.join("main.rs"), "fn main() {}").unwrap();
    std::fs::write(root.join("guide.html"), "<h1>Guide</h1>").unwrap();
    std::fs::write(root.join("legacy.htm"), "<p>old page</p>").unwrap();

    // Off by default: HTML is not walked.
    let mut config = Config::default();
    assert!(!config.index.documents);
    let files = collect_files(root, &config).unwrap();
    assert!(!files
        .iter()
        .any(|p| crate::index::documents::is_document_file(p)));

    // Opt-in: the flag makes the walker pick up both extensions.
    config.index.documents = true;
    let files = collect_files(root, &config).unwrap();
    assert!(files
        .iter()
        .any(|p| p.file_name().map(|n| n == "guide.html").unwrap_or(false)));
    assert!(files
        .iter()
        .any(|p| p.file_name().map(|n| n == "legacy.htm").unwrap_or(false)));
}

#[test]
fn read_indexable_content_extracts_html_only_when_enabled() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("page.html");
    std::fs::write(&path, "<h1>Title</h1><p>Body.</p>").unwrap();

    // Flag off: raw file content (HTML files aren't walked then anyway,
    // but the routing must not extract behind the user's back).
    let config = Config::default();
    let raw = read_indexable_content(&path, &config).unwrap();
    assert!(raw.contains("<h1>"));

    // Flag on: converted markdown-ish text feeds the chunkers.
    let mut config = Config::default();
    config.index.documents = true;
    let text = read_indexable_content(&path, &config).unwrap();
    assert_eq!(text, "# Title\n\nBody.\n");
}

#[test]
fn include_flag_replaces_the_config_include_for_the_run_and_refuses_a_bad_glob() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("main.rs"), "fn main() {}").unwrap();
    std::fs::write(root.join("book.txt"), "chapter one").unwrap();
    std::fs::write(root.join("page.html"), "<p>hi</p>").unwrap();
    let names = |c: &Config| -> Vec<String> {
        let mut v: Vec<String> = collect_files(root, c)
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    };
    // Control: the default include takes the code file and not the text types.
    let base = Config::default();
    assert!(names(&base).contains(&"main.rs".to_string()));
    assert!(!names(&base).contains(&"book.txt".to_string()));

    // --include replaces the list: only what it names, and main.rs is no
    // longer matched (so an earlier-indexed main.rs would be pruned).
    let mut run = Config::default();
    args::apply_include(&mut run, &["**/*.txt".into(), "**/*.html".into()]).unwrap();
    assert_eq!(names(&run), ["book.txt", "page.html"]);

    // It is per run: a config the flag was not applied to is unchanged.
    assert_eq!(names(&base), names(&Config::default()));
    // No flag leaves the config include alone.
    let mut none = Config::default();
    args::apply_include(&mut none, &[]).unwrap();
    assert_eq!(none.index.include, Config::default().index.include);

    // A glob collect_files would silently drop is refused instead.
    let mut bad = Config::default();
    let err = args::apply_include(&mut bad, &["**/*.[txt".into()]).unwrap_err();
    assert!(err.to_string().contains("not a valid glob"), "{err}");
    assert_eq!(bad.index.include, Config::default().index.include);
}

#[test]
fn read_indexable_content_preserves_utf8_and_decodes_legacy_latin1() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("legacy.java");
    let config = Config::default();
    std::fs::write(&path, "// café 日本語\nclass Example {}\n").unwrap();
    assert_eq!(
        read_indexable_content(&path, &config).unwrap(),
        "// café 日本語\nclass Example {}\n"
    );
    std::fs::write(&path, b"// caf\xe9\nclass Example {}\n").unwrap();
    assert_eq!(
        read_indexable_content(&path, &config).unwrap(),
        "// café\nclass Example {}\n"
    );
    std::fs::remove_file(&path).unwrap();
    assert!(read_indexable_content(&path, &config).is_err());
}
