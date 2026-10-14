use std::io::{Read, Write};
use std::net::TcpListener;

use super::*;

#[tokio::test]
async fn bead_search_marks_retained_remote_metadata_without_changing_it() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buffer = [0; 4096];
        let n = stream.read(&mut buffer).unwrap();
        let request = String::from_utf8_lossy(&buffer[..n]).into_owned();
        let body = serde_json::json!({
            "query":"credential", "count":1,
            "results":[{"bead_id":"example-1", "assignee":"former", "status":"open", "extra":"retained"}]
        }).to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        request
    });
    let backend = RemoteBackend::new(format!("http://{address}"), "default".into());
    let request = SearchBeadsRequest {
        query: "credential".into(),
        priority: None,
        status: None,
        assignee: None,
        rig: None,
        issue_type: None,
        label: None,
        limit: Some(1),
        enrich: Some(true),
        compact: Some(true),
    };
    let response = backend.search_beads(&request).await.unwrap();
    let text = response.content[0].as_text().unwrap();
    let value: serde_json::Value = serde_json::from_str(&text.text).unwrap();
    assert_eq!(value["results"][0]["assignee"], "former");
    assert_eq!(value["results"][0]["status"], "open");
    assert_eq!(value["results"][0]["extra"], "retained");
    let p = &value["results"][0]["metadata_provenance"];
    assert_eq!(p["source"], "unreported");
    assert_eq!(p["as_of"], serde_json::Value::Null);
    assert_eq!(p["current_board_verified"], false);
    assert!(p["warning"].as_str().unwrap().contains("retired-store"));
    assert!(!value["warnings"].as_array().unwrap().is_empty());
    let request = server.join().unwrap();
    assert!(request.contains("enrich=true"));
    assert!(request.contains("compact=true"));
}
