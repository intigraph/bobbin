//! Session-level progressive injection ledger, shared by hook lanes.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// A record of a chunk that was injected in a previous turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LedgerEntry {
    chunk_key: String,
    injection_id: String,
    turn: u64,
}

/// Session-level ledger tracking all chunks injected so far.
/// Stored as JSONL at `.bobbin/session/<cc_session_id>/ledger.jsonl`.
pub(super) struct SessionLedger {
    pub(super) entries: HashSet<String>, // chunk_keys for fast lookup
    pub(super) turn: u64,
    pub(super) path: Option<PathBuf>,
}

impl SessionLedger {
    /// Load ledger for a Claude Code session. Returns empty ledger if session_id
    /// is empty or file doesn't exist.
    pub(super) fn load(repo_root: &Path, cc_session_id: &str) -> Self {
        if cc_session_id.is_empty() {
            return Self {
                entries: HashSet::new(),
                turn: 0,
                path: None,
            };
        }
        let dir = repo_root
            .join(".bobbin")
            .join("session")
            .join(cc_session_id);
        let path = dir.join("ledger.jsonl");

        let mut entries = HashSet::new();
        let mut max_turn = 0u64;

        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                for line in content.lines() {
                    if let Ok(entry) = serde_json::from_str::<LedgerEntry>(line) {
                        if entry.turn > max_turn {
                            max_turn = entry.turn;
                        }
                        entries.insert(entry.chunk_key);
                    }
                }
            }
        }

        Self {
            entries,
            turn: max_turn,
            path: Some(path),
        }
    }

    /// Check if a chunk was already injected in a previous turn.
    pub(super) fn contains(&self, chunk_key: &str) -> bool {
        self.entries.contains(chunk_key)
    }

    /// Record newly injected chunks. Appends to the JSONL file.
    pub(super) fn record(&mut self, chunk_keys: &[String], injection_id: &str) {
        let new_turn = self.turn + 1;
        self.turn = new_turn;

        if let Some(path) = &self.path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            // Append new entries
            let mut lines = String::new();
            for key in chunk_keys {
                if let Ok(json) = serde_json::to_string(&LedgerEntry {
                    chunk_key: key.clone(),
                    injection_id: injection_id.to_string(),
                    turn: new_turn,
                }) {
                    lines.push_str(&json);
                    lines.push('\n');
                }
                self.entries.insert(key.clone());
            }
            if !lines.is_empty() {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                {
                    let _ = f.write_all(lines.as_bytes());
                }
            }
        } else {
            // In-memory only (no session_id)
            for key in chunk_keys {
                self.entries.insert(key.clone());
            }
        }
    }

    /// Clear the ledger (used on compaction reset).
    pub(super) fn clear(repo_root: &Path, cc_session_id: &str) {
        if cc_session_id.is_empty() {
            return;
        }
        let path = repo_root
            .join(".bobbin")
            .join("session")
            .join(cc_session_id)
            .join("ledger.jsonl");
        let _ = std::fs::remove_file(&path);
    }

    /// Number of unique chunks tracked.
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Get unique file paths from previously injected chunks.
    /// Chunk keys are formatted as "file_path:start_line:end_line".
    pub(super) fn injected_files(&self) -> Vec<String> {
        let mut files: HashSet<String> = HashSet::new();
        for key in &self.entries {
            // Find the last two colons to extract the file path
            // Keys are "path:start:end" — path itself may contain colons on Windows
            if let Some(last_colon) = key.rfind(':') {
                if let Some(second_colon) = key[..last_colon].rfind(':') {
                    let file = &key[..second_colon];
                    files.insert(file.to_string());
                }
            }
        }
        files.into_iter().collect()
    }
}

/// Build a chunk key from file path and chunk line range.
pub(super) fn chunk_key(file_path: &str, start_line: u32, end_line: u32) -> String {
    format!("{}:{}:{}", file_path, start_line, end_line)
}
