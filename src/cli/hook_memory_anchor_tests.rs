//! All HTTP fixtures bind loopback and use an explicit endpoint and no token.
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-28T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}
fn row(kind: &str, name: &str) -> Value {
    json!({"iri":format!("urn:memory:{name}"), "kind":kind, "label":name, "body":"next step",
        "sourceKind":"declared", "recordedAt":"2026-10-28T11:00:00Z"})
}
fn rows(kind: &str, count: usize) -> Value {
    json!({"rows":(0..count).map(|i|row(kind,&format!("pin-{i:02}"))).collect::<Vec<_>>()})
}

#[test]
fn identity_never_uses_an_empty_or_broad_agent_filter() {
    for value in [
        "",
        "   ",
        "agent with spaces",
        "agent' OR true",
        "../agent",
        "other:agent",
    ] {
        assert!(agent_iri(value).is_none(), "{value}");
    }
    assert_eq!(agent_iri("fixture"), Some(format!("{NAMESPACE}fixture")));
    assert_eq!(agent_iri("aegis:fixture"), agent_iri("fixture"));
    assert_eq!(
        agent_iri("urn:agent:fixture"),
        Some("urn:agent:fixture".into())
    );
}

#[test]
fn requests_use_producer_queries_and_narrow_fresh_handoff() {
    let handoff = request("mem_handoff", "urn:agent:fixture", "urn:plane:", now());
    assert_eq!(handoff["params"]["agent"], "urn:agent:fixture");
    assert_eq!(handoff["params"]["fresh_since"], "2026-10-26T12:00:00Z");
    assert_eq!(handoff["params"]["declared"], "urn:plane:crew/declared");
    let directives = request(
        "mem_directives_for",
        "urn:agent:fixture",
        "urn:plane:",
        now(),
    );
    assert_eq!(directives["params"]["appliesTo"], "urn:agent:fixture");
    assert!(directives["params"].get("agent").is_none());
}

#[test]
fn anchor_is_at_most_fifteen_lines_with_current_handoff_first() {
    let result = render(
        decode(&rows("handoff", 1), "handoff", now()),
        decode(&rows("directive", 51), "directive", now()),
        300,
    );
    assert_eq!(result.lines().count(), 15);
    assert!(result.lines().nth(1).unwrap().starts_with("- Handoff:"));
    assert!(result.lines().last().unwrap().contains("omitted"));
    for budget in 0..18 {
        let result = render(
            decode(&rows("handoff", 1), "handoff", now()),
            decode(&rows("directive", 30), "directive", now()),
            budget,
        );
        assert!(result.lines().count() <= budget.min(15));
        if budget >= 2 {
            assert!(result.contains("- Handoff:"));
        }
    }
}

#[test]
fn directives_are_titles_and_handoff_keeps_next_steps_and_citations() {
    let result = render(
        decode(&rows("handoff", 1), "handoff", now()),
        decode(&rows("directive", 1), "directive", now()),
        15,
    );
    let handoff = result.lines().find(|l| l.starts_with("- Handoff")).unwrap();
    let directive = result
        .lines()
        .find(|l| l.starts_with("- Directive"))
        .unwrap();
    assert!(handoff.contains("next step"));
    assert!(!directive.contains("next step"));
    assert!(directive.contains("declared, 2026-10-28"));
    assert!(directive.contains("<urn:memory:pin-00>"));
}

#[test]
fn superseded_expired_and_old_handoffs_do_not_survive_rebuild() {
    let mut old = row("handoff", "old");
    old["recordedAt"] = json!("2026-10-20T12:00:00Z");
    let mut superseded = row("handoff", "superseded");
    superseded["supersededBy"] = json!("urn:new");
    let mut expired = row("handoff", "expired");
    expired["expiresAt"] = json!("2026-10-28T11:00:00Z");
    let value = json!({"rows":[old,superseded,expired,row("handoff","current")]});
    let result = decode(&value, "handoff", now()).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].label, "current");
    assert!(render(Ok(vec![]), Ok(vec![]), 15).is_empty());
}

#[test]
fn unknown_query_shapes_are_not_a_healthy_empty_anchor() {
    for bad in [
        json!({}),
        json!({"rows":null}),
        json!({"rows":[],"truncated":true}),
        json!({"error":"not installed","rows":[]}),
        rows("directive", 52),
    ] {
        assert!(decode(&bad, "directive", now()).is_err());
    }
    let result = render(Err(anyhow::anyhow!("offline")), Ok(vec![]), 15);
    assert!(result.contains("Handoff unavailable"));
    assert!(!result.contains("offline"));
}

#[test]
fn multiline_titles_and_bodies_cannot_escape_line_or_character_budget() {
    let mut value = row("handoff", "multiline");
    value["label"] = json!("one\ntwo\rthree\t".repeat(100));
    value["body"] = json!("next\nstep".repeat(300));
    let rendered = render(
        decode(&json!({"rows":[value]}), "handoff", now()),
        Ok(vec![]),
        15,
    );
    assert_eq!(rendered.lines().count(), 2);
    assert!(rendered.lines().all(|s| s.chars().count() <= 600));
    assert!(rendered.ends_with("<urn:memory:multiline>"));
}

#[test]
fn pins_reserve_space_without_losing_existing_git_or_breadcrumb_sections() {
    assert_eq!(
        combine(&["pins\nline", "git\nfiles", "breadcrumbs\n"], 5),
        "pins\nline\ngit\nfiles\nbreadcrumbs"
    );
    assert!(combine(&["pins", "git"], 0).is_empty());
    let first = render(
        decode(&rows("handoff", 1), "handoff", now()),
        Ok(vec![]),
        15,
    );
    let rebuilt = render(Ok(vec![]), Ok(vec![]), 15);
    assert!(!first.is_empty());
    assert!(rebuilt.is_empty(), "no persisted previous anchor is reused");
}

async fn local_server(
    handoff: Value,
    directives: Value,
) -> (String, tokio::task::JoinHandle<Vec<Value>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let (head_end, content_length) = loop {
                let mut buf = [0; 2048];
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buf[..n]);
                assert!(bytes.len() <= 8192);
                if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&bytes[..end]);
                    assert!(header.starts_with("POST /ask "));
                    assert!(!header.to_ascii_lowercase().contains("authorization:"));
                    let length = header
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|s| s.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    break (end + 4, length);
                }
            };
            while bytes.len() < head_end + content_length {
                let mut buf = [0; 2048];
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buf[..n]);
            }
            let body: Value =
                serde_json::from_slice(&bytes[head_end..head_end + content_length]).unwrap();
            let result = if body["name"] == "mem_handoff" {
                &handoff
            } else {
                assert_eq!(body["name"], "mem_directives_for");
                &directives
            };
            let reply = result.to_string();
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                        reply.len()
                    )
                    .as_bytes(),
                )
                .await
                .ok(); // A bounded client may close an oversized response.
            requests.push(body);
        }
        requests
    });
    (endpoint, task)
}

#[tokio::test]
async fn compact_reads_both_scoped_queries_from_loopback_only() {
    let (endpoint, server) = local_server(rows("handoff", 1), rows("directive", 2)).await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(TIMEOUT)
        .build()
        .unwrap();
    let text = fetch_with_client(
        &client,
        &endpoint,
        "urn:agent:fixture",
        "urn:plane:",
        now(),
        15,
        None,
    )
    .await;
    assert!(text.contains("- Handoff:"));
    assert_eq!(text.matches("- Directive:").count(), 2);
    let requests = tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(requests.len(), 2);
    for body in requests {
        assert_eq!(body["params"]["now"], "2026-10-28T12:00:00Z");
    }
}

#[tokio::test]
async fn missing_query_keeps_other_pins_and_reports_the_unknown() {
    let (endpoint, server) =
        local_server(rows("handoff", 1), json!({"error":"query missing"})).await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(TIMEOUT)
        .build()
        .unwrap();
    let text = fetch_with_client(
        &client,
        &endpoint,
        "urn:agent:fixture",
        "urn:plane:",
        now(),
        15,
        None,
    )
    .await;
    assert!(text.contains("- Handoff:"));
    assert!(text.contains("Directives unavailable"));
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn oversized_query_response_does_not_replace_other_current_pins() {
    let mut huge = row("handoff", "oversized");
    huge["body"] = json!("x".repeat(MAX_BODY + 1));
    let (endpoint, server) = local_server(json!({"rows":[huge]}), rows("directive", 1)).await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(TIMEOUT)
        .build()
        .unwrap();
    let text = fetch_with_client(
        &client,
        &endpoint,
        "urn:agent:fixture",
        "urn:plane:",
        now(),
        15,
        None,
    )
    .await;
    assert!(text.contains("Handoff unavailable"));
    assert!(text.contains("- Directive:"));
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn stalled_pin_queries_are_bounded_without_stale_fallback() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (_first, _) = listener.accept().await.unwrap();
        let (_second, _) = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let start = std::time::Instant::now();
    let text = fetch_with_client(
        &client,
        &endpoint,
        "urn:agent:fixture",
        "urn:plane:",
        now(),
        15,
        None,
    )
    .await;
    server.abort();
    let _ = server.await;
    assert!(start.elapsed() < Duration::from_secs(3));
    assert!(text.contains("Handoff unavailable"));
    assert!(text.contains("Directives unavailable"));
    assert!(!text.contains("- Handoff:"));
}
