use super::*;
use crate::breadcrumb::{Breadcrumb, BreadcrumbStore};

fn fixture(root: &Path) -> Breadcrumb {
    std::fs::create_dir_all(root.join(".bobbin")).unwrap();
    std::fs::write(root.join("auth.rs"), "fn refresh_token() {}\n").unwrap();
    let bc = Breadcrumb {
        name: "auth-refresh".into(),
        description: "Refresh flow".into(),
        query: "".into(),
        pinned_files: vec!["auth.rs".into()],
        tags: vec![],
        keywords: vec!["refresh_token".into()],
        created_by: "test".into(),
        created_at: Utc::now(),
        last_recalled: None,
        recall_count: 0,
        ttl_days: 0,
    };
    BreadcrumbStore::new(root)
        .save(&[(bc.name.clone(), bc.clone())].into())
        .unwrap();
    bc
}

#[tokio::test]
async fn literal_keywords_share_dedup_across_hook_inputs_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fixture(root);
    let config = Config::default();
    let store = root.join(".bobbin/breadcrumbs.json");
    let original = std::fs::read(&store).unwrap();
    let mut first = prepare(root, "explain REFRESH_TOKEN", "same", 30, &config, None).await;
    assert!(first.text.contains("auth-refresh"));
    assert!(first.text.contains("pinned: auth.rs"));
    assert!(first.text.lines().count() <= 9);
    // Preparation alone must not consume delivery.
    assert!(!prepare(root, "refresh_token", "same", 30, &config, None)
        .await
        .text
        .is_empty());
    first.commit();
    assert!(prepare(
        root,
        r#"{"command":"refresh_token"}"#,
        "same",
        30,
        &config,
        None
    )
    .await
    .text
    .is_empty());
    assert!(!prepare(root, "refresh_token", "new", 30, &config, None)
        .await
        .text
        .is_empty());
    assert_eq!(std::fs::read(store).unwrap(), original);
}

#[tokio::test]
async fn absent_corrupt_expired_empty_and_nonmatching_are_quiet() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let config = Config::default();
    assert!(prepare(root, "refresh_token", "s", 30, &config, None)
        .await
        .text
        .is_empty());
    let mut bc = fixture(root);
    assert!(matching(root, "unrelated").is_empty());
    assert!(prepare(root, "refresh_token", "s", 9, &config, None)
        .await
        .text
        .is_empty());
    bc.ttl_days = 1;
    bc.created_at = Utc::now() - chrono::Duration::days(2);
    BreadcrumbStore::new(root)
        .save(&[(bc.name.clone(), bc.clone())].into())
        .unwrap();
    assert!(matching(root, "refresh_token").is_empty());
    bc.ttl_days = 0;
    bc.keywords = vec!["".into(), " ".into()];
    BreadcrumbStore::new(root)
        .save(&[(bc.name.clone(), bc)].into())
        .unwrap();
    assert!(matching(root, "anything").is_empty());
    std::fs::write(root.join(".bobbin/breadcrumbs.json"), "broken").unwrap();
    assert!(prepare(root, "refresh_token", "s", 30, &config, None)
        .await
        .text
        .is_empty());
}

#[tokio::test]
async fn pins_cannot_escape_and_updated_content_rearms() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let config = Config::default();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), "secret").unwrap();
    let mut bc = fixture(root);
    std::os::unix::fs::symlink(outside.path(), root.join("link")).unwrap();
    bc.pinned_files.extend([
        "../secret".into(),
        "/etc/passwd".into(),
        "link/secret".into(),
    ]);
    BreadcrumbStore::new(root)
        .save(&[(bc.name.clone(), bc.clone())].into())
        .unwrap();
    let mut first = prepare(root, "refresh_token", "../bad", 30, &config, None).await;
    assert!(!first.text.contains("secret"));
    assert!(!first.text.contains("passwd"));
    first.commit();
    bc.description = "New flow".into();
    BreadcrumbStore::new(root)
        .save(&[(bc.name.clone(), bc)].into())
        .unwrap();
    assert!(prepare(root, "refresh_token", "../bad", 30, &config, None)
        .await
        .text
        .contains("New flow"));
    assert!(!root.join("bad").exists());
}

#[tokio::test]
async fn access_policy_applies_to_pins_and_unknown_repository_identity() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fixture(root);
    let mut config = Config::default();
    config.access.roles = vec![crate::config::RoleConfig {
        name: "default".into(),
        allow: vec![],
        deny: vec![],
        deny_paths: vec!["auth.rs".into()],
    }];
    assert!(
        !crate::access::RepoFilter::from_config(&config.access, "unlisted-role")
            .is_path_allowed("repos/project/auth.rs")
    );
    let allowed_metadata = prepare(root, "refresh_token", "s", 30, &config, None).await;
    assert!(allowed_metadata.text.contains("auth-refresh"));
    assert!(!allowed_metadata.text.contains("pinned:"));
    config.access.default_allow = false;
    assert!(prepare(root, "refresh_token", "s", 30, &config, None)
        .await
        .text
        .is_empty());
}

#[tokio::test]
async fn saved_query_uses_remote_search_role_budget_and_path_filter() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut bc = fixture(root);
    bc.query = "saved authentication query".into();
    BreadcrumbStore::new(root)
        .save(&[(bc.name.clone(), bc)].into())
        .unwrap();
    let mut config = Config::default();
    config.access.roles = vec![crate::config::RoleConfig {
        name: "default".into(),
        allow: vec![],
        deny: vec![],
        deny_paths: vec!["secrets.*".into()],
    }];
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) =
            tokio::time::timeout(std::time::Duration::from_secs(3), listener.accept())
                .await
                .unwrap()
                .unwrap();
        let mut input = [0u8; 8192];
        let n = stream.read(&mut input).await.unwrap();
        let request = String::from_utf8_lossy(&input[..n])
            .lines()
            .next()
            .unwrap()
            .to_owned();
        let body = serde_json::json!({
            "query":"saved authentication query", "budget":{"max_lines":5,"used_lines":2},
            "files":[
                {"path":"repos/project/related.rs","language":"rust","relevance":"direct","score":0.9,"chunks":[]},
                {"path":"repos/project/secrets.key","language":"text","relevance":"direct","score":0.9,"chunks":[]}
            ],
            "summary":{"total_files":2,"total_chunks":0,"direct_hits":2,"coupled_additions":0,"bridged_additions":0,"source_files":2,"doc_files":0}
        }).to_string();
        let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body);
        stream.write_all(response.as_bytes()).await.unwrap();
        request
    });
    let result = prepare(root, "refresh_token", "s", 30, &config, Some(&url)).await;
    assert!(
        result.text.contains("related: repos/project/related.rs"),
        "{}",
        result.text
    );
    assert!(!result.text.contains("secrets.key"));
    assert!(result.text.lines().count() <= 9);
    let request = server.await.unwrap();
    assert!(request.contains("q=saved+authentication+query"));
    assert!(request.contains("budget=5"));
    assert!(request.contains("role="));
}
