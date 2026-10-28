//! Tests for per-prompt memory recall (aegis-gshu.2).
//!
//! The pure tests run everywhere. The `knowledge` tests load the fixture
//! entities and the stub `mem_index_since` stored query into a local,
//! in-memory quipu store and drive the real refresh path over HTTP against it.
//! Nothing here touches a production quipu.

use super::*;

const NOW: &str = "2026-10-28T12:00:00Z";
const NOW_EPOCH: u64 = 1_793_188_800; // 2026-10-28T12:00:00Z

fn item(iri: &str, kind: &str, label: &str, body: &str, source: &str, at: &str) -> MemoryItem {
    MemoryItem {
        iri: iri.into(),
        natural_key: iri
            .rsplit_once('/')
            .map(|(k, _)| k.to_string())
            .unwrap_or_default(),
        kind: kind.into(),
        label: label.into(),
        body: body.into(),
        source_kind: source.into(),
        recorded_at: at.into(),
        review_after: None,
        expires_at: None,
    }
}

fn mirror_of(items: Vec<MemoryItem>) -> Mirror {
    Mirror {
        endpoint: "http://quipu.test".into(),
        items: items.into_iter().map(|i| (i.iri.clone(), i)).collect(),
        refreshed_at: Some(NOW_EPOCH - 60),
        attempted_at: Some(NOW_EPOCH - 60),
        ..Mirror::default()
    }
}

#[test]
fn now_epoch_constant_matches_now() {
    assert_eq!(fmt_epoch(NOW_EPOCH), NOW);
}

#[test]
fn item_cap_follows_knowledge_budget_pct() {
    // 300 lines * 15% = 45 lines: header + 5 items fits; cap is 5.
    assert_eq!(item_cap(300, 15.0), 5);
    // 20 lines * 15% = 3 lines: header + 2 items.
    assert_eq!(item_cap(20, 15.0), 2);
    // No knowledge share, no memory.
    assert_eq!(item_cap(300, 0.0), 0);
    assert_eq!(item_cap(5, 15.0), 0);
}

#[test]
fn decode_page_reads_quipu_cells_and_statuses() {
    let body = serde_json::json!({
        "rows": [
            {"iri": "urn:a", "naturalKey": "urn:k/a", "kind": "http://aegis.gastown.local/ontology/Decision",
             "label": "A", "body": {"value": "chose x", "lang": "en"}, "sourceKind": "crew:declared",
             "recordedAt": {"value": "2026-10-01T00:00:00Z", "datatype": "xsd:dateTime"},
             "status": "current", "cursor": "2026-10-01T00:00:00Z"},
            {"iri": "urn:a", "kind": "http://aegis.gastown.local/ontology/Record", "status": "current",
             "cursor": "2026-10-01T00:00:00Z"},
            {"iri": "urn:b", "status": "superseded", "cursor": "2026-10-02T00:00:00Z"}
        ]
    });
    let page = decode_page(&body).unwrap();
    assert!(!page.overflow);
    assert_eq!(page.last_cursor.as_deref(), Some("2026-10-02T00:00:00Z"));
    assert_eq!(
        page.changes.len(),
        2,
        "duplicate type rows collapse per IRI"
    );
    match &page.changes[0] {
        Change::Upsert(i) => {
            assert_eq!(i.kind, "Decision");
            assert_eq!(i.body, "chose x");
            assert_eq!(i.recorded_at, "2026-10-01T00:00:00Z");
        }
        other => panic!("expected upsert, got {other:?}"),
    }
    assert_eq!(page.changes[1], Change::Remove("urn:b".into()));
}

#[test]
fn decode_page_flags_overflow_sentinel_and_drops_it() {
    let rows: Vec<serde_json::Value> = (0..=PAGE_LIMIT)
        .map(|n| serde_json::json!({"iri": format!("urn:i{n:03}"), "label": "x", "cursor": format!("{n:03}")}))
        .collect();
    let page = decode_page(&serde_json::json!({ "rows": rows })).unwrap();
    assert!(page.overflow);
    assert_eq!(page.changes.len(), PAGE_LIMIT);
    assert_eq!(page.last_cursor.as_deref(), Some("049"));
}

#[test]
fn apply_removes_superseded_and_expired() {
    let mut m = mirror_of(vec![item(
        "urn:k/old",
        "Decision",
        "x",
        "y",
        "crew:declared",
        "2026-10-01",
    )]);
    let mut expired = item(
        "urn:k/exp",
        "Directive",
        "x",
        "y",
        "crew:declared",
        "2026-10-01",
    );
    expired.expires_at = Some("2026-10-27T00:00:00Z".into());
    let (up, rm) = apply(
        &mut m,
        vec![Change::Remove("urn:k/old".into()), Change::Upsert(expired)],
        NOW,
    );
    assert_eq!((up, rm), (0, 1));
    assert!(m.items.is_empty());
}

#[test]
fn rank_matches_on_terms_and_prefers_declared() {
    let m = mirror_of(vec![
        item(
            "urn:mem/decision/svc/rate-limit/1",
            "Decision",
            "Service rate limit",
            "chose 250 per minute",
            "crew:declared",
            "2026-10-20",
        ),
        item(
            "urn:mem/lesson/svc/rate-limit/1",
            "Lesson",
            "Service rate limit",
            "bursts trip the limit",
            "crew:inferred",
            "2026-10-21",
        ),
        item(
            "urn:mem/reference/unrelated/x/1",
            "Reference",
            "Grafana API",
            "dashboards",
            "crew:observed",
            "2026-10-21",
        ),
    ]);
    let hits = rank("what rate limit does the service use?", &m, 5, NOW);
    let kinds: Vec<&str> = hits.iter().map(|i| i.kind.as_str()).collect();
    assert_eq!(
        kinds,
        vec!["Decision", "Lesson"],
        "declared first, unrelated dropped"
    );
    assert!(rank("hello there", &m, 5, NOW).is_empty());
}

#[test]
fn render_tags_items_and_holds_char_caps() {
    let long = "word ".repeat(400);
    let items: Vec<MemoryItem> = (0..8)
        .map(|n| {
            item(
                &format!("urn:mem/finding/x/{n}"),
                "Finding",
                &format!("Finding {n}"),
                &long,
                "crew:observed",
                "2026-10-08T00:00:00Z",
            )
        })
        .collect();
    let refs: Vec<&MemoryItem> = items.iter().collect();
    let r = render(&refs, None);
    assert!(r.text.starts_with(HEADER));
    assert!(!r.items.is_empty());
    let item_chars: usize = r.items.iter().map(|c| c.line.chars().count()).sum();
    assert!(
        item_chars <= MAX_TOTAL_CHARS,
        "{item_chars} > {MAX_TOTAL_CHARS}"
    );
    for (n, c) in r.items.iter().enumerate() {
        assert_eq!(c.item, format!("m{}", n + 1));
        assert!(c
            .line
            .starts_with(&format!("[m{}] Finding: Finding {n} - ", n + 1)));
        assert!(c
            .line
            .ends_with(&format!("(observed, 2026-10-08) urn:mem/finding/x/{n}")));
        assert!(!c.line.contains('\n'));
    }
    assert!(items[0].text().chars().count() <= MAX_ITEM_CHARS);
}

#[test]
fn caps_hold_on_a_200_item_mirror() {
    let items: Vec<MemoryItem> = (0..200)
        .map(|n| {
            item(
                &format!("urn:mem/decision/deploy/window-{n}/1"),
                "Decision",
                &format!("Deploy window rule {n}"),
                &"deploy window details ".repeat(50),
                "crew:declared",
                "2026-10-02",
            )
        })
        .collect();
    let m = mirror_of(items);
    match recall_for_prompt(
        "what is the deploy window rule",
        Some(&m),
        "http://quipu.test",
        300,
        15.0,
        NOW_EPOCH,
    ) {
        Recall::Items { text, items } => {
            assert!(items.len() <= MAX_ITEMS);
            assert!(!items.is_empty());
            let chars: usize = items.iter().map(|c| c.line.chars().count()).sum();
            assert!(chars <= MAX_TOTAL_CHARS);
            assert!(text.lines().count() <= 1 + MAX_ITEMS);
        }
        other => panic!("expected items, got {other:?}"),
    }
}

#[test]
fn outage_says_skipped_in_one_line_and_stale_header_when_serving() {
    // Cold mirror and quipu down: one line.
    let cold = Mirror {
        endpoint: "http://quipu.test".into(),
        attempted_at: Some(NOW_EPOCH),
        last_error: Some("quipu /ask unreachable".into()),
        ..Mirror::default()
    };
    match recall_for_prompt(
        "what is the deploy window rule",
        Some(&cold),
        "http://quipu.test",
        300,
        15.0,
        NOW_EPOCH,
    ) {
        Recall::Skipped(line) => {
            assert_eq!(line.lines().count(), 1);
            assert!(line.contains("recall skipped"));
        }
        other => panic!("expected skipped, got {other:?}"),
    }

    // A two-hour-old mirror still serves, and says it is stale.
    let mut m = mirror_of(vec![item(
        "urn:mem/directive/crew/deploy-window/1",
        "Directive",
        "Deploy window",
        "02:00-09:00 ET",
        "crew:declared",
        "2026-10-02",
    )]);
    m.refreshed_at = Some(NOW_EPOCH - 7200);
    m.last_error = Some("timeout".into());
    match recall_for_prompt(
        "when is the deploy window",
        Some(&m),
        "http://quipu.test",
        300,
        15.0,
        NOW_EPOCH,
    ) {
        Recall::Items { text, .. } => {
            assert!(text
                .lines()
                .next()
                .unwrap()
                .contains("memory index stale since 2026-10-28T10:00:00Z"));
        }
        other => panic!("expected items, got {other:?}"),
    }
    // Same stale mirror, nothing matches: the skip line.
    assert!(matches!(
        recall_for_prompt(
            "unrelated question about parsers",
            Some(&m),
            "http://quipu.test",
            300,
            15.0,
            NOW_EPOCH
        ),
        Recall::Skipped(_)
    ));
    // No mirror for this endpoint: silent (the background refresh fills it).
    assert_eq!(
        recall_for_prompt(
            "when is the deploy window",
            Some(&m),
            "http://other",
            300,
            15.0,
            NOW_EPOCH
        ),
        Recall::Silent
    );
}

#[test]
fn mirror_round_trips_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sub/memory-mirror.json");
    let m = mirror_of(vec![item(
        "urn:k/1",
        "Decision",
        "a",
        "b",
        "crew:declared",
        "2026-10-01",
    )]);
    save_mirror(&path, &m).unwrap();
    let back = load_mirror(&path).unwrap();
    assert_eq!(back.items, m.items);
    let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
        .collect();
    assert!(leftovers.is_empty());
}

#[tokio::test]
async fn refresh_records_unreachable_quipu() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory-mirror.json");
    // Port 9 (discard) on localhost: nothing listens, connection refused.
    let err = refresh("http://127.0.0.1:9", &path).await.unwrap_err();
    assert!(!format!("{err:#}").is_empty());
    let m = load_mirror(&path).unwrap();
    assert!(m.last_error.is_some());
    assert!(m.refreshed_at.is_none());
}

#[test]
fn rewind_cursor_steps_back_one_second() {
    assert_eq!(
        rewind_cursor("2026-10-20T09:00:00Z|urn:mem/x"),
        "2026-10-20T08:59:59Z|"
    );
    assert_eq!(rewind_cursor(""), "");
    assert_eq!(rewind_cursor("opaque"), "opaque");
}

#[test]
fn reapplying_the_same_row_is_not_a_change() {
    let i = item(
        "urn:k/1",
        "Decision",
        "a",
        "b",
        "crew:declared",
        "2026-10-01",
    );
    let mut m = mirror_of(vec![i.clone()]);
    assert_eq!(apply(&mut m, vec![Change::Upsert(i)], NOW), (0, 0));
}

#[test]
fn refresh_lock_is_exclusive_and_released_on_drop() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory-mirror.json");
    let held = RefreshLock::acquire(&path).expect("first lock");
    assert!(refresh_running(&path));
    assert!(
        RefreshLock::acquire(&path).is_none(),
        "second refresh must not start"
    );
    drop(held);
    assert!(!refresh_running(&path));
    assert!(RefreshLock::acquire(&path).is_some());
}

#[test]
fn stale_refresh_lock_is_taken_over() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory-mirror.json");
    // A crashed holder left its lock behind.
    std::fs::write(lock_path(&path), "4242 0").unwrap();
    assert!(
        RefreshLock::acquire(&path).is_none(),
        "a fresh lock is respected"
    );
    let lock = RefreshLock::acquire_with(&path, 0).expect("stale lock is taken over");
    drop(lock);
    assert!(!lock_path(&path).exists());
}

#[tokio::test]
async fn refresh_while_another_runs_does_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory-mirror.json");
    let _held = RefreshLock::acquire(&path).unwrap();
    let err = refresh("http://127.0.0.1:9", &path).await.unwrap_err();
    assert!(err.downcast_ref::<RefreshBusy>().is_some(), "{err:#}");
    // No attempt recorded, no error written: the running refresh owns the mirror.
    assert!(load_mirror(&path).is_none());
}

// ---------------------------------------------------------------------------
// Local quipu store with fixture entities and the stub stored query
// ---------------------------------------------------------------------------

#[cfg(feature = "knowledge")]
mod local_quipu {
    use super::*;
    use std::sync::{Arc, Mutex};

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/agent-memory");

    fn stub_query() -> quipu::store::queries::StoredQuery {
        let raw = std::fs::read_to_string(format!("{FIXTURES}/mem_index_since.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        quipu::store::queries::StoredQuery {
            name: v["name"].as_str().unwrap().into(),
            description: v["description"].as_str().unwrap().into(),
            template: v["template"].as_str().unwrap().into(),
            dataset: None,
            params: v["params"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| quipu::store::queries::StoredParam {
                    name: p["name"].as_str().unwrap().into(),
                    kind: p["kind"].as_str().unwrap().into(),
                    required: p["required"].as_bool().unwrap(),
                    default: p["default"].as_str().map(String::from),
                    description: p["description"].as_str().unwrap().into(),
                })
                .collect(),
        }
    }

    fn knot(store: &mut quipu::Store, turtle: &str, ts: &str) {
        quipu::tool_knot(
            store,
            &serde_json::json!({"turtle": turtle, "timestamp": ts, "actor": "test", "source": "test"}),
        )
        .unwrap();
    }

    fn fixture_store() -> quipu::Store {
        let mut store = quipu::Store::open_in_memory().unwrap();
        let ttl = std::fs::read_to_string(format!("{FIXTURES}/entities.ttl")).unwrap();
        knot(&mut store, &ttl, "2026-10-27T00:00:00Z");
        store
            .query_load(&stub_query(), "2026-10-27T00:00:00Z")
            .unwrap();
        store
    }

    /// Serve `/ask` from a local store, the way quipu.svc does.
    async fn serve(store: Arc<Mutex<quipu::Store>>) -> String {
        use axum::{routing::post, Json, Router};
        let app = Router::new().route(
            "/ask",
            post(move |Json(body): Json<serde_json::Value>| {
                let store = store.clone();
                async move {
                    let guard = store.lock().unwrap();
                    match quipu::mcp::named_query::tool_ask(&guard, &body) {
                        Ok(v) => (axum::http::StatusCode::OK, Json(v)),
                        Err(e) => (
                            axum::http::StatusCode::BAD_REQUEST,
                            Json(serde_json::json!({"error": e.to_string()})),
                        ),
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[test]
    fn stub_query_returns_current_and_superseded_rows() {
        let store = fixture_store();
        let out = quipu::mcp::named_query::tool_ask(
            &store,
            &serde_json::json!({"name": INDEX_QUERY, "params": {"since": ""}}),
        )
        .unwrap();
        let page = decode_page(&out).unwrap();
        let removes = page
            .changes
            .iter()
            .filter(|c| matches!(c, Change::Remove(_)))
            .count();
        let upserts = page
            .changes
            .iter()
            .filter(|c| matches!(c, Change::Upsert(_)))
            .count();
        assert_eq!(removes, 1, "the superseded Decision comes back as a delete");
        assert_eq!(upserts, 9, "one current record per kind");
        assert!(!page.overflow);
    }

    #[tokio::test]
    async fn every_kind_is_recalled_and_superseded_never_appears() {
        let store = Arc::new(Mutex::new(fixture_store()));
        let endpoint = serve(store.clone()).await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory-mirror.json");

        let report = refresh(&endpoint, &path).await.unwrap();
        assert_eq!(report.items, 9);
        let mirror = load_mirror(&path).unwrap();
        assert!(mirror.last_error.is_none());

        let prompts: Vec<serde_json::Value> = serde_json::from_str(
            &std::fs::read_to_string(format!("{FIXTURES}/recall-prompts.json")).unwrap(),
        )
        .unwrap();
        for p in &prompts {
            let prompt = p["prompt"].as_str().unwrap();
            let key = p["expect_key"].as_str().unwrap();
            let now = mirror.refreshed_at.unwrap();
            match recall_for_prompt(prompt, Some(&mirror), &endpoint, 300, 15.0, now) {
                Recall::Items { text, items } => {
                    assert!(
                        items.iter().any(|c| c.natural_key.ends_with(key)),
                        "{prompt:?} should recall {key}; got:\n{text}"
                    );
                    if let Some(body) = p["expect_body"].as_str() {
                        assert!(text.contains(body), "{text}");
                    }
                    if let Some(never) = p["never_body"].as_str() {
                        assert!(!text.contains(never), "superseded text leaked:\n{text}");
                    }
                }
                other => panic!("{prompt:?}: expected items, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn incremental_refresh_drops_a_record_once_superseded() {
        let store = Arc::new(Mutex::new(fixture_store()));
        let endpoint = serve(store.clone()).await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory-mirror.json");
        refresh(&endpoint, &path).await.unwrap();

        let old =
            "http://aegis.gastown.local/ontology/mem/directive/crew/deploy-window/20261002T000000Z";
        let new =
            "http://aegis.gastown.local/ontology/mem/directive/crew/deploy-window/20261027T000000Z";
        assert!(load_mirror(&path).unwrap().items.contains_key(old));

        let ttl = format!(
            "@prefix aegis: <http://aegis.gastown.local/ontology/> .\n\
             @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
             <{new}> a aegis:Directive ;\n\
               aegis:naturalKey <http://aegis.gastown.local/ontology/mem/directive/crew/deploy-window> ;\n\
               rdfs:label \"Deploy window\" ;\n\
               rdfs:comment \"Run disruptive deploys between 01:00 and 06:00 ET\" ;\n\
               aegis:sourceKind \"crew:declared\" ;\n\
               aegis:recordedAt \"2026-10-27T00:00:00Z\" ;\n\
               aegis:indexCursor \"2026-10-27T00:00:00Z|{new}\" ;\n\
               aegis:supersedes <{old}> .\n\
             <{old}> aegis:supersededBy <{new}> ;\n\
               aegis:supersededCursor \"2026-10-27T00:00:00Z|{old}\" .\n"
        );
        knot(&mut store.lock().unwrap(), &ttl, "2026-10-27T01:00:00Z");

        let report = refresh(&endpoint, &path).await.unwrap();
        assert_eq!(report.removed, 1);
        let mirror = load_mirror(&path).unwrap();
        assert!(!mirror.items.contains_key(old));
        assert!(mirror.items.contains_key(new));
        match recall_for_prompt(
            "when is the deploy window for disruptive changes",
            Some(&mirror),
            &endpoint,
            300,
            15.0,
            mirror.refreshed_at.unwrap(),
        ) {
            Recall::Items { text, .. } => {
                assert!(text.contains("01:00 and 06:00"), "{text}");
                assert!(!text.contains("02:00 and 09:00"), "{text}");
            }
            other => panic!("expected items, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn supersession_in_the_cursor_second_is_not_missed() {
        // A supersession stamped in the cursor's own second whose feed cursor
        // does not sort after the stored one: only the one-second re-read
        // sees it.
        let store = Arc::new(Mutex::new(fixture_store()));
        let endpoint = serve(store.clone()).await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory-mirror.json");
        refresh(&endpoint, &path).await.unwrap();
        let cursor = load_mirror(&path).unwrap().cursor;
        let second = cursor.split('|').next().unwrap().to_string();

        // The record holding the cursor itself: its supersession row has a
        // cursor equal to the stored one, which a strict `> since` skips.
        let old = cursor.split('|').nth(1).unwrap().to_string();
        let ttl = format!(
            "@prefix aegis: <http://aegis.gastown.local/ontology/> .\n\
             <{old}> aegis:supersededBy <urn:aaa:newer> ;\n\
               aegis:supersededCursor \"{second}|{old}\" .\n\
             <urn:aaa:newer> aegis:recordedAt \"{second}\" .\n"
        );
        assert!(
            format!("{second}|{old}") <= cursor,
            "fixture must not sort after the cursor"
        );
        knot(&mut store.lock().unwrap(), &ttl, "2026-10-27T03:00:00Z");

        let report = refresh(&endpoint, &path).await.unwrap();
        assert_eq!(report.removed, 1, "{report:?}");
        assert!(!load_mirror(&path).unwrap().items.contains_key(&old));
    }

    #[tokio::test]
    async fn same_timestamp_rows_across_a_page_boundary_are_not_dropped() {
        let mut store = fixture_store();
        // 120 records changed in the same second: more than two pages sharing
        // one timestamp. The '<recordedAt>|<iri>' cursor keeps every row; a
        // timestamp-only cursor would lose all but the first page of them.
        let mut ttl = String::from(
            "@prefix aegis: <http://aegis.gastown.local/ontology/> .\n\
             @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n",
        );
        for n in 0..120 {
            ttl.push_str(&format!(
                "<http://aegis.gastown.local/ontology/mem/lesson/bulk/t{n:03}/20261015T000000Z> a aegis:Lesson ;\n\
                   aegis:naturalKey <http://aegis.gastown.local/ontology/mem/lesson/bulk/t{n:03}> ;\n\
                   rdfs:label \"Bulk lesson {n}\" ;\n\
                   aegis:sourceKind \"crew:inferred\" ;\n\
                   aegis:recordedAt \"2026-10-15T00:00:00Z\" ;\n\
                   aegis:indexCursor \"2026-10-15T00:00:00Z|http://aegis.gastown.local/ontology/mem/lesson/bulk/t{n:03}/20261015T000000Z\" .\n"
            ));
        }
        knot(&mut store, &ttl, "2026-10-27T02:00:00Z");
        let endpoint = serve(Arc::new(Mutex::new(store))).await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory-mirror.json");

        let report = refresh(&endpoint, &path).await.unwrap();
        assert!(report.pages >= 3, "{report:?}");
        assert_eq!(report.items, 9 + 120, "{report:?}");
    }
}
