use super::{optional_spotlight, SearchParams};
use axum::{extract::Query, http::Uri, routing::post, Json, Router};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[test]
fn spotlight_query_option_is_explicit_and_defaults_to_enabled() {
    for (query, enabled) in [
        ("q=test", true),
        ("q=test&spotlight=true", true),
        ("q=test&spotlight=false", false),
    ] {
        let uri: Uri = format!("/search?{query}").parse().unwrap();
        let params = Query::<SearchParams>::try_from_uri(&uri).unwrap().0;
        assert_eq!(params.spotlight.unwrap_or(true), enabled);
        assert_eq!(params.q, "test");
    }
    let invalid: Uri = "/search?q=test&spotlight=maybe".parse().unwrap();
    assert!(Query::<SearchParams>::try_from_uri(&invalid).is_err());
}

#[tokio::test]
async fn opted_out_probe_makes_no_spotlight_request_but_default_calls_are_attributed() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    let app = Router::new().route(
        "/spotlight",
        post(move |headers: axum::http::HeaderMap| {
            let observed = Arc::clone(&observed);
            async move {
                assert_eq!(headers.get("x-quipu-client").unwrap(), "bobbin-search");
                observed.fetch_add(1, Ordering::SeqCst);
                Json(serde_json::json!({"annotations":[{
                    "surface":"test","iri":"urn:test","type":"Fixture","confidence":0.9
                }]}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let parse = |uri: &str| {
        Query::<SearchParams>::try_from_uri(&uri.parse().unwrap())
            .unwrap()
            .0
    };
    let disabled = parse("/search?q=test&spotlight=false");
    let default = parse("/search?q=test");
    assert!(optional_spotlight(Some(&url), &disabled).await.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(optional_spotlight(None, &default).await.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let hits = optional_spotlight(Some(&url), &default).await;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].iri, "urn:test");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    server.abort();
}
