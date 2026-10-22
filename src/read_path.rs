//! Resolve read requests in single-repository and multi-repository workspaces.
use std::io;
use std::path::{Component, Path, PathBuf};

fn failure(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, message)
}

fn is_file(path: &Path) -> io::Result<bool> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

/// Resolve only existing files. Never select an arbitrary repository when a
/// repository-relative path has multiple matches.
pub(crate) fn resolve_read_path(
    root: &Path,
    file: &str,
    repo: Option<&str>,
    repo_prefix: Option<&str>,
) -> io::Result<PathBuf> {
    let repos = repo_prefix
        .map(|prefix| root.join(prefix))
        .unwrap_or_else(|| root.join("repos"));
    if let Some(repo) = repo {
        let mut parts = Path::new(repo).components();
        if !matches!(parts.next(), Some(Component::Normal(_))) || parts.next().is_some() {
            return Err(failure("Invalid repository name; use one repository name and a repository-relative file path".into()));
        }
        let nested_repo = repos.join(repo);
        let nested_exists = match std::fs::metadata(&nested_repo) {
            Ok(metadata) => metadata.is_dir(),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                false
            }
            Err(error) => return Err(error),
        };
        // A workspace can itself share a repository's name. Prefer its
        // configured child repository before the single-repository fallback.
        let repo_root = if nested_exists {
            nested_repo
        } else if root.file_name().and_then(|n| n.to_str()) == Some(repo) {
            root.to_path_buf()
        } else {
            nested_repo
        };
        if !repo_root.is_dir() {
            return Err(failure(format!(
                "Unknown repository: {repo}; use the repository name from the search result"
            )));
        }
        let relative = Path::new(file);
        if relative.is_absolute()
            || relative.components().any(|c| {
                matches!(
                    c,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(failure(
                "With repo set, file must be repository-relative without parent traversal".into(),
            ));
        }
        let candidate = repo_root.join(relative);
        if is_file(&candidate)? {
            // An explicit repository may not escape through a symlink.
            let canonical = candidate.canonicalize()?;
            if canonical.starts_with(repo_root.canonicalize()?) {
                return Ok(candidate);
            }
            return Err(failure(
                "File resolves outside the requested repository".into(),
            ));
        }
        return Err(failure(format!("File not found in repository {repo}: {file}; use the file path from a current search result")));
    }
    // Preserve existing workspace-relative (including repos/<repo>/...) and
    // absolute path behavior before attempting repository-relative resolution.
    let direct = root.join(file);
    if is_file(&direct)? {
        return Ok(direct);
    }
    if Path::new(file).is_absolute()
        || Path::new(file)
            .components()
            .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(failure(format!(
            "File not found: {file}; use a current search result and its repository"
        )));
    }
    let mut matches = Vec::new();
    match std::fs::read_dir(&repos) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                if !entry.file_type()?.is_dir() {
                    continue;
                }
                let candidate = entry.path().join(file);
                if is_file(&candidate)? {
                    let canonical = candidate.canonicalize()?;
                    if canonical.starts_with(entry.path().canonicalize()?) {
                        matches.push((entry.file_name().to_string_lossy().into_owned(), candidate));
                    }
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    matches.sort_by(|a, b| a.0.cmp(&b.0));
    match matches.len() {
        1 => Ok(matches.remove(0).1),
        0 => Err(failure(format!("File not found: {file}; pass repo with the repository-relative path from a current search result, or use repos/<repo>/<file>"))),
        _ => Err(failure(format!("Ambiguous file: {file}; pass repo explicitly (matches: {})", matches.iter().map(|(repo, _)| repo.as_str()).collect::<Vec<_>>().join(", ")))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "read-path-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn file(&self, path: &str) -> PathBuf {
            let p = self.0.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "requested content\n").unwrap();
            p
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    #[test]
    fn search_relative_path_reads_unique_content() {
        let f = Fixture::new();
        f.file("repos/alpha/src/example.rs");
        let p = resolve_read_path(&f.0, "src/example.rs", None, None).unwrap();
        assert_eq!(std::fs::read_to_string(p).unwrap(), "requested content\n");
    }
    #[test]
    fn ambiguity_refuses_and_explicit_repo_selects() {
        let f = Fixture::new();
        f.file("repos/zeta/src/example.rs");
        let p = f.file("repos/alpha/src/example.rs");
        let error = resolve_read_path(&f.0, "src/example.rs", None, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("pass repo explicitly (matches: alpha, zeta)"));
        assert_eq!(
            resolve_read_path(&f.0, "src/example.rs", Some("alpha"), None).unwrap(),
            p
        );
    }
    #[test]
    fn explicit_repo_prefers_nested_repository_over_workspace_name() {
        let f = Fixture::new();
        let workspace_file = f.file("alpha/src/example.rs");
        std::fs::write(
            workspace_file,
            "workspace file, not the requested repository",
        )
        .unwrap();
        let wanted = f.file("alpha/repos/alpha/src/example.rs");
        let workspace = f.0.join("alpha");
        let actual = resolve_read_path(&workspace, "src/example.rs", Some("alpha"), None).unwrap();
        assert_eq!(actual, wanted);
    }

    #[test]
    fn explicit_single_repository_root_is_preserved() {
        let f = Fixture::new();
        let wanted = f.file("solo/src/example.rs");
        let actual =
            resolve_read_path(&f.0.join("solo"), "src/example.rs", Some("solo"), None).unwrap();
        assert_eq!(actual, wanted);
    }

    #[test]
    fn workspace_paths_remain_readable() {
        let f = Fixture::new();
        let p = f.file("repos/alpha/src/example.rs");
        assert_eq!(
            resolve_read_path(&f.0, "repos/alpha/src/example.rs", None, None).unwrap(),
            p
        );
        let p = f.file("src/local.rs");
        assert_eq!(
            resolve_read_path(&f.0, "src/local.rs", None, None).unwrap(),
            p
        );
    }
    #[test]
    fn missing_and_unknown_repo_have_guidance() {
        let f = Fixture::new();
        f.file("repos/alpha/src/example.rs");
        assert!(
            resolve_read_path(&f.0, "src/missing.rs", Some("alpha"), None)
                .unwrap_err()
                .to_string()
                .contains("current search result")
        );
        assert!(
            resolve_read_path(&f.0, "src/example.rs", Some("missing"), None)
                .unwrap_err()
                .to_string()
                .contains("Unknown repository")
        );
        assert!(resolve_read_path(&f.0, "missing", None, None)
            .unwrap_err()
            .to_string()
            .contains("pass repo"));
    }
    #[test]
    fn configured_prefix_is_respected() {
        let f = Fixture::new();
        let p = f.file("sources/alpha/src/example.rs");
        let prefix = f.0.join("sources");
        assert_eq!(
            resolve_read_path(&f.0, "src/example.rs", Some("alpha"), prefix.to_str()).unwrap(),
            p
        );
        assert_eq!(
            resolve_read_path(&f.0, "src/example.rs", None, prefix.to_str()).unwrap(),
            p
        );
    }
    #[test]
    fn explicit_repository_rejects_traversal() {
        let f = Fixture::new();
        f.file("repos/alpha/src/example.rs");
        f.file("secret");
        assert!(resolve_read_path(&f.0, "../../secret", Some("alpha"), None).is_err());
        assert!(resolve_read_path(&f.0, "src/example.rs", Some("../alpha"), None).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn explicit_repository_rejects_symlink_escape() {
        let f = Fixture::new();
        let secret = f.file("secret");
        f.file("repos/alpha/src/example.rs");
        std::os::unix::fs::symlink(secret, f.0.join("repos/alpha/link")).unwrap();
        assert!(resolve_read_path(&f.0, "link", Some("alpha"), None).is_err());
    }
}
