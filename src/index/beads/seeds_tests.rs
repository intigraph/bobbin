use super::*;
use crate::config::{BeadsConfig, BeadsSource};
use serde_json::json;

fn config() -> BeadsConfig {
    BeadsConfig {
        enabled: true,
        source: BeadsSource::Seeds,
        seeds: crate::config::SeedsBoard {
            url: "http://quipu.example".into(),
            ..Default::default()
        },
        databases: vec!["beads_aegis".into()],
        max_age_days: 0,
        ..BeadsConfig::default()
    }
}

fn row(id: &str, title: &str, status: &str, keyword: Option<&str>) -> Value {
    let mut r = json!({
        "id": id,
        "title": title,
        "status": status,
        "priority": 1,
        "type": "task",
        "assignee": "https://seeds.local/principal/sattler",
        "owner": "https://seeds.local/principal/ian",
        "created": {"value": "2026-11-21T22:44:11Z", "datatype": "http://www.w3.org/2001/XMLSchema#dateTime"},
    });
    if let Some(k) = keyword {
        r["keyword"] = json!(k);
    }
    r
}

#[test]
fn graph_defaults_to_seeds_project_convention_and_honours_override() {
    let mut c = config();
    assert_eq!(
        graph_for(&c, "beads_aegis"),
        "https://seeds.local/project/aegis"
    );
    c.seeds
        .graphs
        .insert("beads_aegis".into(), "https://g.example/board".into());
    assert_eq!(graph_for(&c, "beads_aegis"), "https://g.example/board");
}

#[test]
fn fold_rows_merges_keywords_and_strips_principal_iris() {
    let rows = vec![
        row("aegis-1", "one", "open", Some("pitch")),
        row("aegis-1", "one", "open", Some("bobbin")),
        row("aegis-2", "two", "in_progress", None),
    ];
    let (items, last) = fold_rows(&rows);
    assert_eq!(last.as_deref(), Some("aegis-2"));
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].labels, vec!["pitch", "bobbin"]);
    assert_eq!(items[0].assignee.as_deref(), Some("sattler"));
    assert_eq!(items[0].owner, "ian");
    assert_eq!(items[0].priority, 1);
    assert_eq!(items[0].created_at.as_deref(), Some("2026-11-21T22:44:11Z"));
    assert!(items[1].labels.is_empty());
}

#[test]
fn inbox_pointers_and_closed_items_never_become_chunks() {
    let (items, _) = fold_rows(&[
        row("aegis-1", "real work mentioning inbox", "open", None),
        row("aegis-2", "  Inbox: mail pointer", "open", None),
        row("aegis-3", "done", "closed", None),
    ]);
    let chunks = items_to_chunks(&config(), "beads_aegis", items);
    let names: Vec<_> = chunks.iter().filter_map(|c| c.name.clone()).collect();
    assert_eq!(chunks.len(), 1, "{names:?}");
    assert!(chunks[0].file_path.contains("aegis-1"));
}

#[test]
fn items_query_escapes_ids_and_pages_by_keyset() {
    let q = items_query(Some("aegis-\"x"), Some(&["a\\b".to_string()]), 50);
    assert!(q.contains(r#"FILTER(STR(?id) > "aegis-\"x")"#), "{q}");
    assert!(q.contains(r#"VALUES ?id { "a\\b" }"#), "{q}");
    assert!(q.ends_with("ORDER BY ?id LIMIT 50"));
}

#[test]
fn truncated_or_rowless_replies_are_errors_not_empty_boards() {
    assert!(parse_query_reply(r#"{"rows":[],"truncated":true}"#, "g").is_err());
    assert!(parse_query_reply(r#"{"error":"x"}"#, "g").is_err());
    assert_eq!(
        parse_query_reply(r#"{"rows":[],"truncated":false}"#, "g")
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn change_page_reduces_to_item_ids_and_cursor() {
    let page = parse_changes(
        &json!({
            "records": [
                {"tx": 10, "entity": "https://seeds.local/item/aegis-1", "attribute": "x", "op": "assert"},
                {"tx": 10, "entity": "https://seeds.local/item/aegis-1", "attribute": "y", "op": "retract"},
                {"tx": 11, "entity": "https://seeds.local/item/aegis-2/comment/3", "attribute": "z", "op": "assert"},
                {"tx": 11, "entity": "https://seeds.local/principal/ian", "attribute": "z", "op": "assert"},
            ],
            "since": 9, "next_tx": 11, "watermark_tx": 40,
        })
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        page.item_ids.into_iter().collect::<Vec<_>>(),
        vec!["aegis-1", "aegis-2"]
    );
    assert_eq!(page.next_tx, 11);
    assert_eq!(page.watermark_tx, 40);
}

#[test]
fn change_page_without_cursor_is_an_error() {
    assert!(parse_changes(r#"{"records":[]}"#).is_err());
    assert!(parse_changes(r#"{"next_tx":3}"#).is_err());
}

#[test]
fn empty_seeds_url_is_refused() {
    let mut c = config();
    c.seeds.url = " ".into();
    assert!(base_url(&c).is_err());
}
