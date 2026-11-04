use super::*;
use serde_json::json;

fn item() -> MemoryItem {
    MemoryItem {
        iri: "urn:memory:version".into(),
        natural_key: "urn:memory:key".into(),
        kind: "Decision".into(),
        label: "fixture".into(),
        body: String::new(),
        source_kind: "crew:declared".into(),
        recorded_at: "2020-01-01T00:00:00Z".into(),
        review_after: Some("2099-01-01T00:00:00Z".into()),
        expires_at: None,
    }
}

#[test]
fn plan_never_postpones_due_dates_or_repairs_unknown_metadata() {
    let now = chrono::Utc::now();
    let mut record = item();
    assert!(plan(&record, now).unwrap().is_some());
    record.review_after = Some("2000-01-01T00:00:00Z".into());
    assert!(plan(&record, now).unwrap().is_none());
    record.review_after = None;
    assert!(plan(&record, now).unwrap().is_none());
    record.review_after = Some("invalid".into());
    assert!(plan(&record, now).is_err());
    record = item();
    record.expires_at = Some("2000-01-01T00:00:00Z".into());
    assert!(plan(&record, now).unwrap().is_none());
    record = item();
    record.source_kind = "unknown".into();
    assert!(plan(&record, now).is_err());
    record = item();
    record.iri = "urn:evil> INSERT DATA {}".into();
    assert!(plan(&record, now).is_err());
}

async fn protocol(
    receipt: serde_json::Value,
    readback: serde_json::Value,
    status: u16,
) -> (bool, usize) {
    use axum::{routing::post, Json, Router};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let queries = Arc::new(AtomicUsize::new(0));
    let count = queries.clone();
    let app = Router::new()
        .route(
            "/update",
            post(move || {
                let receipt = receipt.clone();
                async move {
                    (
                        axum::http::StatusCode::from_u16(status).unwrap(),
                        Json(receipt),
                    )
                }
            }),
        )
        .route(
            "/query",
            post(move || {
                let value = readback.clone();
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Json(value)
                }
            }),
        );
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", socket.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(socket, app).await.unwrap() });
    let mut changes = vec![plan(&item(), chrono::Utc::now()).unwrap().unwrap()];
    // Fixed metadata timestamp makes the fake readback deterministic.
    changes[0].review_after = "2026-01-01T00:00:00.000Z".into();
    let result = apply_changes_with(
        &endpoint,
        &mut changes,
        "fixture-only-not-a-credential",
        &reqwest::Client::new(),
    )
    .await;
    let good = result.is_ok() && changes[0].outcome == "applied_verified";
    server.abort();
    (good, queries.load(Ordering::SeqCst))
}

#[tokio::test]
async fn completion_requires_exact_graph_receipt_and_readback() {
    let graph = format!("{}crew/declared", crate::knowledge::quarantine::plane_ns());
    let receipt = json!({"asserted":1,"retracted":1,"graphs":[{"graph":graph,"tx":1,"asserted":1,"retracted":1}]});
    let readback =
        json!({"count":1,"rows":[{"review":"2026-01-01T00:00:00.000Z"}],"truncated":false});
    assert_eq!(
        protocol(receipt.clone(), readback.clone(), 200).await,
        (true, 1)
    );
    let typed = json!({"count":1,"rows":[{"review":{"datatype":"http://www.w3.org/2001/XMLSchema#dateTime","value":"2026-01-01T00:00:00.000Z"}}],"truncated":false});
    assert_eq!(
        protocol(receipt.clone(), typed.clone(), 200).await,
        (true, 1)
    );
    let mut wrong_type = typed;
    wrong_type["rows"][0]["review"]["datatype"] = json!("urn:wrong");
    assert_eq!(protocol(receipt.clone(), wrong_type, 200).await, (false, 1));
    let mut wrong = receipt.clone();
    wrong["graphs"][0]["graph"] = json!("urn:wrong");
    assert_eq!(protocol(wrong, readback.clone(), 200).await, (false, 0));
    let mut absent = receipt.clone();
    absent["asserted"] = json!(0);
    absent["retracted"] = json!(0);
    assert_eq!(protocol(absent, readback.clone(), 200).await, (false, 0));
    assert_eq!(
        protocol(
            receipt.clone(),
            json!({"count":0,"rows":[],"truncated":false}),
            200
        )
        .await,
        (false, 1)
    );
    assert_eq!(
        protocol(
            receipt.clone(),
            json!({"count":1,"rows":[{"review":"2099-01-01T00:00:00Z"}],"truncated":false}),
            200
        )
        .await,
        (false, 1)
    );
    assert_eq!(
        protocol(receipt.clone(), readback.clone(), 403).await,
        (false, 0)
    );
    assert_eq!(protocol(receipt, readback, 401).await, (false, 0));
}

#[tokio::test]
#[ignore = "requires QUIPU_BIN and QUIPU_SERVER_BIN; run just test-memory-review under the HOME guard"]
async fn real_quipu_conditional_update_preserves_other_graphs_and_refuses_replay() {
    use std::process::{Command, Stdio};
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("graph.db");
    let graph = format!("{}crew/declared", crate::knowledge::quarantine::plane_ns());
    let other = "urn:fixture:unrelated";
    let qp = std::env::var_os("QUIPU_BIN").expect("QUIPU_BIN required");
    for target in [&graph, other] {
        let output = Command::new(&qp)
            .args(["graph", "create", target, "--db"])
            .arg(&db)
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "graph fixture creation failed");
    }
    std::fs::create_dir_all(dir.path().join(".bobbin")).unwrap();
    std::fs::write(
        dir.path().join(".bobbin/config.toml"),
        "[quipu.server]\nauth_token = 'fixture-only-not-a-credential'\n",
    )
    .unwrap();
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = socket.local_addr().unwrap();
    drop(socket);
    let binary = std::env::var_os("QUIPU_SERVER_BIN").expect("QUIPU_SERVER_BIN required");
    let log = std::fs::File::create(dir.path().join("server.log")).unwrap();
    let _server = Child(
        Command::new(binary)
            .arg("--db")
            .arg(&db)
            .args(["--bind", &address.to_string()])
            .current_dir(dir.path())
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap(),
    );
    let endpoint = format!("http://{address}");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();
    let token = "fixture-only-not-a-credential";
    let mut ready = false;
    for _ in 0..100 {
        if client
            .get(format!("{endpoint}/health"))
            .send()
            .await
            .is_ok()
        {
            ready = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(ready, "fixture server did not start");
    let record = item();
    let ns = crate::iri::ONTOLOGY_NS;
    let seed = format!("INSERT DATA {{ GRAPH <{graph}> {{ <{}> <{ns}naturalKey> <{}>; <{ns}recordedAt> \"{}\"^^<http://www.w3.org/2001/XMLSchema#dateTime>; <{ns}reviewAfter> \"2099-01-01T00:00:00Z\"^^<http://www.w3.org/2001/XMLSchema#dateTime> }} GRAPH <{other}> {{ <urn:untouched> <urn:value> \"keep\" }} }}", record.iri, record.natural_key, record.recorded_at);
    let response = client
        .post(format!("{endpoint}/update"))
        .bearer_auth(token)
        .header("Content-Type", "application/sparql-update")
        .body(seed)
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "fixture seed refused: {}",
        response.status()
    );
    let mut changes = vec![plan(&record, chrono::Utc::now()).unwrap().unwrap()];
    if let Err(error) = apply_changes_with(&endpoint, &mut changes, token, &client).await {
        let query = format!("SELECT ?review WHERE {{ GRAPH <{graph}> {{ <{}> <{ns}reviewAfter> ?review }} }} LIMIT 2", record.iri);
        let body: serde_json::Value = client
            .post(format!("{endpoint}/query"))
            .bearer_auth(token)
            .json(&json!({"query":query}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        panic!("fixture readback failure: {error}; {body}");
    }
    assert_eq!(changes[0].outcome, "applied_verified");
    // An unchanged stale source version must not pass the guard twice.
    assert!(apply_changes_with(&endpoint, &mut changes, token, &client)
        .await
        .is_err());
    // A newer version in another plane must also defeat the update guard.
    let mut superseded = record.clone();
    superseded.iri = "urn:memory:superseded".into();
    let seed = format!("INSERT DATA {{ GRAPH <{graph}> {{ <{}> <{ns}naturalKey> <{}>; <{ns}recordedAt> \"{}\"^^<http://www.w3.org/2001/XMLSchema#dateTime>; <{ns}reviewAfter> \"2099-01-01T00:00:00Z\"^^<http://www.w3.org/2001/XMLSchema#dateTime> }} GRAPH <{other}> {{ <urn:memory:new> <{ns}supersedes> <{}> }} }}", superseded.iri, superseded.natural_key, superseded.recorded_at, superseded.iri);
    assert!(client
        .post(format!("{endpoint}/update"))
        .bearer_auth(token)
        .header("Content-Type", "application/sparql-update")
        .body(seed)
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
    let mut changes = vec![plan(&superseded, chrono::Utc::now()).unwrap().unwrap()];
    assert!(apply_changes_with(&endpoint, &mut changes, token, &client)
        .await
        .is_err());
    let check = format!(
        "SELECT ?review WHERE {{ GRAPH <{graph}> {{ <{}> <{ns}reviewAfter> ?review }} }} LIMIT 2",
        superseded.iri
    );
    let body: serde_json::Value = client
        .post(format!("{endpoint}/query"))
        .bearer_auth(token)
        .json(&json!({"query":check}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        review_value(&body["rows"][0]["review"]),
        Some("2099-01-01T00:00:00Z")
    );
    let body: serde_json::Value = client.post(format!("{endpoint}/query")).bearer_auth(token)
        .json(&json!({"query":format!("SELECT ?value WHERE {{ GRAPH <{other}> {{ <urn:untouched> <urn:value> ?value }} }}")}))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(body["rows"][0]["value"], "keep");
}
