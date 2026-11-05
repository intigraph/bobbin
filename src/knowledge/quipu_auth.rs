//! Shared remote credential discovery and per-process write refusal circuit.
use anyhow::{bail, Result};
use std::{
    collections::HashSet,
    path::Path,
    sync::{Mutex, OnceLock},
};

const FIX: &str = "checked QUIPU_AUTH_TOKEN > QUIPU_AUTH_TOKEN_FILE > ~/.config/quipu/token; install the issued token at ~/.config/quipu/token; run tambo doctor";
static DISABLED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn key(endpoint: &str) -> String {
    match reqwest::Url::parse(endpoint) {
        Ok(mut url) => {
            url.set_query(None);
            url.set_fragment(None);
            url.to_string().trim_end_matches('/').to_owned()
        }
        Err(_) => endpoint.trim_end_matches('/').to_owned(),
    }
}

fn marker_path(endpoint: &str) -> Option<std::path::PathBuf> {
    let session = [
        "QUIPU_SESSION",
        "CODEX_SESSION_ID",
        "CODEX_THREAD_ID",
        "CLAUDE_CODE_SESSION_ID",
    ]
    .iter()
    .find_map(|name| std::env::var(name).ok().filter(|s| !s.is_empty()))?;
    let root = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".local/state"))
        })?;
    Some(marker_at(endpoint, &session, &root))
}

fn marker_at(endpoint: &str, session: &str, root: &Path) -> std::path::PathBuf {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("{}\0{session}", key(endpoint)).as_bytes());
    root.join("bobbin/quipu-auth")
        .join(format!("{}.disabled", hex::encode(digest)))
}

// create_new provides one diagnostic owner across separate hook/CLI processes.
fn record_marker(path: &Path, why: &str) -> std::io::Result<bool> {
    use std::io::Write;
    let parent = path.parent().expect("auth marker has parent");
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(why.as_bytes())?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

pub(crate) fn check(endpoint: &str) -> Result<()> {
    let disabled = DISABLED
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| anyhow::anyhow!("Quipu auth circuit unavailable; writes skipped"))?;
    if disabled.contains(&key(endpoint)) || marker_path(endpoint).is_some_and(|path| path.exists())
    {
        bail!("Quipu writes disabled for this session; run tambo doctor and start a new session after repair");
    }
    Ok(())
}

fn disable(endpoint: &str, why: &str) -> anyhow::Error {
    if let Ok(mut disabled) = DISABLED.get_or_init(Default::default).lock() {
        if disabled.insert(key(endpoint)) {
            let (first, scope) = match marker_path(endpoint) {
                Some(path) => match record_marker(&path, why) {
                    Ok(first) => (first, "harness session"),
                    Err(_) => (true, "process only (session latch unavailable)"),
                },
                None => (true, "process only (no harness session ID)"),
            };
            if first {
                eprintln!("Quipu credential {why}; {FIX}; writes disabled for this {scope}");
            }
        }
    }
    anyhow::anyhow!("Quipu writes disabled: credential {why}; run tambo doctor")
}

pub(crate) fn rejected(endpoint: &str) -> anyhow::Error {
    disable(endpoint, "rejected (HTTP 401)")
}

pub(crate) fn require(endpoint: &str) -> Result<String> {
    check(endpoint)?;
    match resolve(
        std::env::var("QUIPU_AUTH_TOKEN").ok().as_deref(),
        std::env::var_os("QUIPU_AUTH_TOKEN_FILE")
            .as_deref()
            .map(Path::new),
        directories::BaseDirs::new()
            .as_ref()
            .map(|dirs| dirs.home_dir()),
    ) {
        Ok(Some(token)) => Ok(token),
        Ok(None) => Err(disable(endpoint, "missing")),
        Err(_) => Err(disable(endpoint, "unreadable")),
    }
}

fn resolve(
    inline: Option<&str>,
    explicit: Option<&Path>,
    home: Option<&Path>,
) -> Result<Option<String>> {
    if let Some(value) = inline.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(Some(value.to_owned()));
    }
    let default = home.map(|home| home.join(".config/quipu/token"));
    let Some(path) = explicit
        .filter(|p| !p.as_os_str().is_empty())
        .or(default.as_deref())
    else {
        return Ok(None);
    };
    match std::fs::read_to_string(path) {
        Ok(value) => Ok((!value.trim().is_empty()).then(|| value.trim().to_owned())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => bail!("Quipu credential file unreadable; {FIX}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_and_explicit_precedence_never_fall_back_to_legacy() {
        let home = tempfile::tempdir().unwrap();
        let canonical = home.path().join(".config/quipu/token");
        let legacy = home.path().join(".config/aegis/quipu_token");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(legacy, "legacy").unwrap();
        assert_eq!(resolve(None, None, Some(home.path())).unwrap(), None);
        std::fs::create_dir_all(canonical.parent().unwrap()).unwrap();
        std::fs::write(canonical, "canonical\n").unwrap();
        assert_eq!(
            resolve(None, None, Some(home.path())).unwrap().as_deref(),
            Some("canonical")
        );
        let explicit = home.path().join("override");
        assert_eq!(
            resolve(None, Some(&explicit), Some(home.path())).unwrap(),
            None
        );
        std::fs::write(&explicit, "override\n").unwrap();
        assert_eq!(
            resolve(Some("inline"), Some(&explicit), Some(home.path()))
                .unwrap()
                .as_deref(),
            Some("inline")
        );
        assert_eq!(
            resolve(Some(""), Some(&explicit), Some(home.path()))
                .unwrap()
                .as_deref(),
            Some("override")
        );
        std::fs::write(&explicit, "\n").unwrap();
        assert_eq!(
            resolve(None, Some(&explicit), Some(home.path())).unwrap(),
            None
        );
        std::fs::write(&explicit, [0xff]).unwrap();
        assert!(resolve(None, Some(&explicit), Some(home.path())).is_err());
    }
    #[test]
    fn session_marker_is_seen_in_fresh_processes_and_other_sessions_are_independent() {
        let endpoint = std::env::var("BOBBIN_AUTH_TEST_ENDPOINT")
            .unwrap_or_else(|_| "http://circuit.invalid".into());
        if let Ok(expected) = std::env::var("BOBBIN_AUTH_TEST_CHILD") {
            assert_eq!(check(&endpoint).is_err(), expected == "disabled");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let run = |session: &str, endpoint: &str, expected: &str| {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["knowledge::quipu_auth::tests::session_marker_is_seen_in_fresh_processes_and_other_sessions_are_independent", "--exact"])
                .env("XDG_STATE_HOME", root.path())
                .env("QUIPU_SESSION", session)
                .env("BOBBIN_AUTH_TEST_ENDPOINT", endpoint)
                .env("BOBBIN_AUTH_TEST_CHILD", expected)
                .output().unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stdout)
            );
        };
        run("session-one", &endpoint, "enabled");
        let path = marker_at(&endpoint, "session-one", root.path());
        assert!(record_marker(&path, "rejected").unwrap());
        assert!(!record_marker(&path, "rejected").unwrap());
        run("session-one", &endpoint, "disabled");
        run("session-two", &endpoint, "enabled");
        run("session-one", "http://other-circuit.invalid", "enabled");
        run(
            "session-one",
            "http://circuit.invalid/another-service",
            "enabled",
        );
    }

    #[test]
    fn rejection_disables_other_paths_on_same_server_but_not_other_servers() {
        let endpoint = "http://auth-circuit.test:23456/knot/stage"; // gitleaks:allow synthetic URL, no credential
        check(endpoint).unwrap();
        assert!(rejected(endpoint).to_string().contains("rejected"));
        assert!(check(endpoint).is_err());
        assert!(check("http://auth-circuit-control.test:23456/import").is_ok());
    }
}
