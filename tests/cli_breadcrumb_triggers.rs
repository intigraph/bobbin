mod common;
use common::TestProject;
use serde_json::{json, Value};

fn fixture() -> TestProject {
    let p = TestProject::new();
    let home = p.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    TestProject::bobbin_cmd()
        .env("HOME", &home)
        .env("BOBBIN_GPU", "0")
        .args(["init", p.path().to_str().unwrap()])
        .assert()
        .success();
    p.write_file("auth.rs", "fn refresh_token() {}\n");
    TestProject::bobbin_cmd()
        .env("HOME", &home)
        .env("BOBBIN_GPU", "0")
        .args([
            "mark",
            "auth-refresh",
            "authentication refresh",
            "Refresh flow",
            "--pin",
            "auth.rs",
            "--on",
            "refresh_token",
            "--path",
            p.path().to_str().unwrap(),
        ])
        .assert()
        .success();
    p
}
fn hook(p: &TestProject, name: &str, mut input: Value, extra: &[&str]) -> String {
    input["cwd"] = json!(p.path());
    let mut cmd = TestProject::bobbin_cmd();
    cmd.env("HOME", p.path().join("home"))
        .env("BOBBIN_GPU", "0")
        .current_dir(p.path())
        .args(["hook", name])
        .args(extra)
        .write_stdin(input.to_string());
    String::from_utf8(cmd.assert().success().get_output().stdout.clone()).unwrap()
}
#[test]
fn actual_prompt_and_tool_hooks_share_dedup_and_preserve_startup() {
    let p = fixture();
    let store = p.path().join(".bobbin/breadcrumbs.json");
    let before = std::fs::read(&store).unwrap();
    let prompt = hook(
        &p,
        "inject-context",
        json!({"prompt":"Explain REFRESH_TOKEN implementation","session_id":"same"}),
        &["--budget", "20"],
    );
    assert!(prompt.contains("Breadcrumb: auth-refresh"), "{prompt}");
    assert!(prompt.contains("pinned: auth.rs"));
    assert!(prompt.lines().count() <= 6);
    let duplicate = hook(
        &p,
        "post-tool-use",
        json!({"tool_name":"UnknownControl","tool_input":{"message":"refresh_token"},"session_id":"same"}),
        &[],
    );
    assert!(!duplicate.contains("auth-refresh"), "{duplicate}");
    let tool = hook(
        &p,
        "post-tool-use",
        json!({"tool_name":"UnknownControl","tool_input":{"message":"ReFrEsH_ToKeN"},"session_id":"new"}),
        &["--budget", "20"],
    );
    let tool: Value =
        serde_json::from_str(&tool).expect("one valid tool response, not multiple JSON documents");
    let context = tool["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(context.contains("Breadcrumb: auth-refresh"), "{context}");
    assert!(context.lines().count() <= 6);
    let startup = hook(&p, "prime-context", json!({"session_id":"new"}), &[]);
    let startup: Value = serde_json::from_str(&startup).unwrap();
    assert!(startup["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("=== Breadcrumbs ==="));
    assert_eq!(std::fs::read(store).unwrap(), before);
}
#[test]
fn tool_result_is_not_a_keyword_source_and_corruption_stays_read_only() {
    let p = fixture();
    let result_only = hook(
        &p,
        "post-tool-use",
        json!({"tool_name":"UnknownControl","tool_input":{"message":"unrelated"},"tool_response":"refresh_token","session_id":"s"}),
        &[],
    );
    assert!(result_only.is_empty(), "{result_only}");
    let store = p.path().join(".bobbin/breadcrumbs.json");
    std::fs::write(&store, "not json").unwrap();
    let broken = hook(
        &p,
        "inject-context",
        json!({"prompt":"Explain refresh_token implementation","session_id":"s"}),
        &[],
    );
    assert!(broken.is_empty(), "{broken}");
    assert_eq!(std::fs::read_to_string(store).unwrap(), "not json");
}

#[cfg(unix)]
#[test]
fn special_and_symlink_stores_never_block_or_emit_breadcrumbs() {
    use std::os::unix::fs::{symlink, FileTypeExt};
    let p = fixture();
    let store = p.path().join(".bobbin/breadcrumbs.json");
    let saved = p.path().join("saved.json");
    std::fs::copy(&store, &saved).unwrap();
    let before = std::fs::read(&saved).unwrap();
    std::fs::remove_file(&store).unwrap();
    let name = std::ffi::CString::new(store.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: name is a live, NUL-terminated path to this private fixture.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    for kind in ["fifo", "symlink"] {
        for (hook_name, input) in [
            (
                "inject-context",
                json!({"prompt":"Explain refresh_token implementation"}),
            ),
            (
                "post-tool-use",
                json!({"tool_name":"UnknownControl","tool_input":{"message":"refresh_token"}}),
            ),
        ] {
            let mut input = input;
            input["cwd"] = json!(p.path());
            input["session_id"] = json!(format!("{kind}-{hook_name}"));
            let output = TestProject::bobbin_cmd()
                .env("HOME", p.path().join("home"))
                .env("BOBBIN_GPU", "0")
                .current_dir(p.path())
                .args(["hook", hook_name])
                .timeout(std::time::Duration::from_secs(3))
                .write_stdin(input.to_string())
                .assert()
                .success()
                .get_output()
                .stdout
                .clone();
            assert!(!String::from_utf8_lossy(&output).contains("Breadcrumb:"));
        }
        if kind == "fifo" {
            assert!(std::fs::symlink_metadata(&store)
                .unwrap()
                .file_type()
                .is_fifo());
            std::fs::remove_file(&store).unwrap();
            symlink(&saved, &store).unwrap();
        }
    }
    assert!(std::fs::symlink_metadata(&store)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(std::fs::read(saved).unwrap(), before);
}

#[test]
fn actual_post_tool_hook_uses_the_selected_remote_for_saved_queries() {
    use std::io::{Read, Write};
    let p = fixture();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "hook did not execute saved query on selected server"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => panic!("mock accept failed: {e}"),
            }
        };
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let mut buf = [0; 8192];
        let n = stream.read(&mut buf).unwrap();
        let request = String::from_utf8_lossy(&buf[..n])
            .lines()
            .next()
            .unwrap()
            .to_owned();
        let body=json!({"query":"authentication refresh","budget":{"max_lines":5,"used_lines":1},"files":[{"path":"repos/project/trigger-result.rs","language":"rust","relevance":"direct","score":0.9,"chunks":[]}],"summary":{"total_files":1,"total_chunks":0,"direct_hits":1,"coupled_additions":0,"bridged_additions":0,"source_files":1,"doc_files":0}}).to_string();
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
        request
    });
    let tool = hook(
        &p,
        "post-tool-use",
        json!({"tool_name":"UnknownControl","tool_input":{"message":"refresh_token"},"session_id":"remote"}),
        &["--server", &url, "--budget", "30"],
    );
    let tool: Value = serde_json::from_str(&tool).unwrap();
    assert!(tool["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("related: repos/project/trigger-result.rs"));
    let request = server.join().unwrap();
    assert!(request.contains("q=authentication+refresh"));
    assert!(request.contains("role="));
}
