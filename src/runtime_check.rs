//! Validate native model loading in isolation before committing it to this process.
//! Some system ONNX builds register their schemas twice when a session is created.
//! Those messages bypass ORT logging entirely (C++ stderr), so a logger cannot fix
//! them. A runtime producing them is rejected, never presented as healthy.
use anyhow::{bail, Context, Result};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

// Reap on every error path, including a failed poll or diagnostic-file read.
struct ProbeChild(Child);
impl Drop for ProbeChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub const PROBE: &str = "--internal-check-onnx-session";

/// Runs before CLI preparation: no downloads, GPU probe, indexing or live writes.
pub fn run_probe(model: &Path) -> Result<()> {
    let _session = ort::session::Session::builder()?
        .with_intra_threads(1)?
        .commit_from_file(model)?;
    Ok(())
}

fn diagnostic_failure(success: bool, stderr: &[u8]) -> Option<&'static str> {
    if String::from_utf8_lossy(stderr).contains("Schema error: Trying to register schema") {
        Some("ONNX runtime rejected: duplicate schema registration; use a compatible bundled runtime via ORT_DYLIB_PATH")
    } else if !success {
        Some("ONNX runtime could not load the model; check the model and ORT_DYLIB_PATH")
    } else {
        None
    }
}

pub fn validate(model: &Path) -> Result<()> {
    // Cache successes only. A failed runtime must never earn a usable receipt.
    static CHECKED: OnceLock<Mutex<Vec<(PathBuf, Option<std::ffi::OsString>)>>> = OnceLock::new();
    let key = (model.to_path_buf(), std::env::var_os("ORT_DYLIB_PATH"));
    let mut checked = CHECKED
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| anyhow::anyhow!("ONNX validation state unavailable"))?;
    if checked.contains(&key) {
        return Ok(());
    }
    let mut command = Command::new(std::env::current_exe()?);
    command.arg(PROBE).arg(model);
    check_command(command, Duration::from_secs(30), 1024 * 1024)?;
    checked.push(key);
    Ok(())
}

fn check_command(mut command: Command, deadline: Duration, limit: u64) -> Result<()> {
    let mut diagnostics = tempfile::tempfile().context("Cannot prepare ONNX validation")?;
    let mut child = ProbeChild(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(diagnostics.try_clone()?)
            .spawn()
            .context("Cannot start ONNX validation")?,
    );
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.0.try_wait()? {
            break status;
        }
        if start.elapsed() > deadline || diagnostics.metadata()?.len() > limit {
            let _ = child.0.kill();
            let _ = child.0.wait();
            bail!("ONNX validation exceeded its resource bound; semantic model unavailable");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    diagnostics.seek(SeekFrom::Start(0))?;
    let mut stderr = Vec::new();
    diagnostics.take(limit + 1).read_to_end(&mut stderr)?;
    if stderr.len() as u64 > limit {
        bail!("ONNX validation exceeded its diagnostic bound; semantic model unavailable");
    }
    if let Some(message) = diagnostic_failure(status.success(), &stderr) {
        bail!("{message}");
    }
    // Unknown native diagnostics remain visible as one notice, never as a false
    // claim that the runtime was silent. Debug diagnostics are explicitly opt-in.
    if !stderr.is_empty() {
        eprintln!("warning: ONNX runtime validation emitted diagnostics");
        tracing::debug!(diagnostics = %String::from_utf8_lossy(&stderr), "ONNX validation details");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(unix)]
    fn child_success_does_not_hide_native_schema_failure() {
        let mut cmd = std::process::Command::new("sh");
        cmd.args([
            "-c",
            "printf 'Schema error: Trying to register schema with name Abs\n' >&2",
        ]);
        let err = super::check_command(cmd, std::time::Duration::from_secs(2), 4096).unwrap_err();
        assert!(err.to_string().contains("duplicate schema"));
        assert!(!err.to_string().contains("name Abs"));
        let mut control = std::process::Command::new("sh");
        control.args(["-c", "exit 0"]);
        super::check_command(control, std::time::Duration::from_secs(2), 4096).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn child_failure_and_diagnostic_overflow_remain_errors() {
        for script in [
            "printf 'cannot read /private/model.onnx' >&2; exit 7",
            "printf '%0800d' 0 >&2",
        ] {
            let mut cmd = std::process::Command::new("sh");
            cmd.args(["-c", script]);
            let err = super::check_command(cmd, std::time::Duration::from_secs(2), 64).unwrap_err();
            assert!(!err.to_string().contains("/private"));
        }
    }

    #[test]
    #[cfg(unix)]
    fn timed_out_child_is_killed_and_reaped() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", "printf '%s' $$ > \"$1\"; exec sleep 10", "probe"])
            .arg(&pidfile);
        let start = std::time::Instant::now();
        assert!(super::check_command(cmd, std::time::Duration::from_secs(1), 4096).is_err());
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        let pid = std::fs::read_to_string(pidfile).unwrap();
        assert!(!std::process::Command::new("kill")
            .args(["-0", &pid])
            .output()
            .unwrap()
            .status
            .success());
    }
}
