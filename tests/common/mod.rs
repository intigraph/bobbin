use assert_cmd::Command;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// A temporary project directory with sample source files for testing.
pub struct TestProject {
    pub dir: TempDir,
}

impl TestProject {
    /// Create a new temp directory with a git repo initialized.
    pub fn new() -> Self {
        let dir = TempDir::new().expect("failed to create temp dir");
        // Initialize a bare git repo so commands that check for git context work
        std::process::Command::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(dir.path())
            .output()
            .expect("failed to git init");
        // Configure git user for commits
        std::process::Command::new("git")
            .args(["config", "user.email", "test@bobbin.dev"])
            .current_dir(dir.path())
            .output()
            .expect("failed to configure git email");
        std::process::Command::new("git")
            .args(["config", "user.name", "Bobbin Test"])
            .current_dir(dir.path())
            .output()
            .expect("failed to configure git user");
        Self { dir }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Write a file relative to the project root, creating parent dirs as needed.
    pub fn write_file(&self, relative_path: &str, content: &str) {
        let full = self.dir.path().join(relative_path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("failed to create parent dirs");
        }
        std::fs::write(&full, content).expect("failed to write file");
    }

    /// Add all files and make an initial commit so git history exists.
    pub fn git_commit(&self, message: &str) {
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(self.dir.path())
            .output()
            .expect("failed to git add");
        std::process::Command::new("git")
            .args(["commit", "-m", message, "--allow-empty"])
            .current_dir(self.dir.path())
            .output()
            .expect("failed to git commit");
    }

    /// Write sample Rust source files for indexing tests.
    pub fn write_rust_fixtures(&self) {
        self.write_file(
            "src/lib.rs",
            r#"//! A sample library for testing bobbin indexing.

/// Adds two numbers together.
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

/// Subtracts b from a.
pub fn subtract(a: i32, b: i32) -> i32 {
    a - b
}

/// A simple calculator struct.
pub struct Calculator {
    pub value: f64,
}

impl Calculator {
    /// Create a new calculator starting at zero.
    pub fn new() -> Self {
        Calculator { value: 0.0 }
    }

    /// Add a value to the running total.
    pub fn add(&mut self, n: f64) {
        self.value += n;
    }

    /// Multiply the running total by a factor.
    pub fn multiply(&mut self, factor: f64) {
        self.value *= factor;
    }

    /// Reset the calculator to zero.
    pub fn reset(&mut self) {
        self.value = 0.0;
    }
}
"#,
        );

        self.write_file(
            "src/utils.rs",
            r#"/// Clamp a value to the range [min, max].
pub fn clamp(value: f64, min: f64, max: f64) -> f64 {
    if value < min {
        min
    } else if value > max {
        max
    } else {
        value
    }
}

/// Check if a string is a valid identifier.
pub fn is_valid_identifier(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap();
    (first.is_alphabetic() || first == '_')
        && chars.all(|c| c.is_alphanumeric() || c == '_')
}
"#,
        );
    }

    /// Write sample Python source files for multi-language tests.
    pub fn write_python_fixtures(&self) {
        self.write_file(
            "app.py",
            r#""""A sample Python application."""

def greet(name: str) -> str:
    """Return a greeting message."""
    return f"Hello, {name}!"

class UserService:
    """Manages user operations."""

    def __init__(self):
        self.users = {}

    def add_user(self, user_id: str, name: str):
        """Add a new user."""
        self.users[user_id] = name

    def get_user(self, user_id: str) -> str:
        """Retrieve a user by ID."""
        return self.users.get(user_id, "Unknown")
"#,
        );
    }

    /// Write a sample Markdown file for doc indexing tests.
    pub fn write_markdown_fixtures(&self) {
        self.write_file(
            "README.md",
            r#"# Sample Project

This is a sample project for testing bobbin indexing.

## Getting Started

Install the dependencies and run the tests.

## Architecture

The project uses a layered architecture with clear separation of concerns.

### Storage Layer

Handles persistence using SQLite and LanceDB.

### Search Layer

Provides semantic and keyword search capabilities.
"#,
        );
    }

    /// Initialize bobbin in this project directory.
    pub fn bobbin_init(&self) {
        std::process::Command::new(Self::bobbin_bin())
            .arg("init")
            .arg(self.path())
            .output()
            .expect("bobbin init failed");
    }

    /// Run `bobbin index` and return true if it succeeded.
    /// Returns false if the ONNX runtime is unavailable or indexing fails.
    pub fn bobbin_index(&self) -> bool {
        let output = std::process::Command::new(Self::bobbin_bin())
            .arg("index")
            .arg(self.path())
            .output()
            .expect("failed to run bobbin index");
        output.status.success()
    }

    /// Index, and FAIL LOUDLY if it did not work — never skip.
    ///
    /// Every caller used to write `if !project.bobbin_index() { return; }`. A
    /// returning `#[test]` has PASSED: Rust's harness reports it identically to
    /// one that asserted. So an environment where `bobbin index` cannot run
    /// turned every index-dependent test green, and the greenness was CAUSED by
    /// the failure — the worse the environment, the more certain the pass.
    ///
    /// Measured (aegis-pnm0uo): the whole 11-test `cli_index` suite completes in
    /// ~300 ms in CI against 15-21 s locally, i.e. CI has never executed these
    /// assertions.
    ///
    /// Two things are checked, and the second is the one that matters. An exit
    /// status says the process did not complain; it cannot distinguish "indexed
    /// everything" from "indexed nothing". The ARTIFACT can. So this asserts the
    /// index database exists and is non-empty, and reports the command's own
    /// output when it does not — turning a silent skip into a named cause.
    pub fn index_or_explain(&self) {
        let output = std::process::Command::new(Self::bobbin_bin())
            .arg("index")
            .arg(self.path())
            .output()
            .expect("failed to run bobbin index");

        assert!(
            output.status.success(),
            "`bobbin index` FAILED (status {:?}) — this test cannot run, and must \
             not report success.\n--- stdout ---\n{}\n--- stderr ---\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );

        // The artifact check. `test -s` for this suite (aegis-pnm0uo).
        let db = self.path().join(".bobbin").join("index.db");
        let size = std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0);
        assert!(
            size > 0,
            "`bobbin index` exited 0 but produced no index at {} ({} bytes). \
             An exit status cannot tell an empty index from a full one; this can.",
            db.display(),
            size,
        );
    }

    /// Return the path to the bobbin binary (built via cargo).
    pub fn bobbin_bin() -> PathBuf {
        // assert_cmd finds the binary automatically via cargo
        PathBuf::from(env!("CARGO_BIN_EXE_bobbin"))
    }

    /// Create an assert_cmd Command for bobbin with test isolation.
    /// Disable inherited search/Quipu routes unless a fixture overrides them.
    pub fn bobbin_cmd() -> Command {
        let mut cmd = Command::new(Self::bobbin_bin());
        cmd.env("BOBBIN_SERVER", "")
            .env("BOBBIN_QUIPU_REMOTE", "")
            .env("QUIPU_SERVER", "http://127.0.0.1:0");
        cmd
    }
}

/// Create an initialized project with fixtures (no indexing/embeddings needed).
pub fn init_project() -> TestProject {
    let project = TestProject::new();
    project.write_rust_fixtures();
    project.write_python_fixtures();
    project.write_markdown_fixtures();
    project.git_commit("initial");
    project.bobbin_init();
    project
}

/// Create an indexed project with fixtures. Returns `None` if the ONNX runtime
/// is unavailable and indexing cannot complete (tests should skip gracefully).
pub fn try_indexed_project() -> Option<TestProject> {
    let project = init_project();

    if !project.bobbin_index() {
        eprintln!(
            "SKIP: bobbin index failed (ONNX runtime likely unavailable). \
             Set ORT_DYLIB_PATH or install libonnxruntime.so to enable embedding tests."
        );
        return None;
    }

    Some(project)
}
