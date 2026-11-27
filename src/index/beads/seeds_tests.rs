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

fn row(id: &str, title: &str, status: &str) -> Value {
    json!({
        "id": id,
        "title": title,
        "n_title": 1,
        "status": status,
        "n_status": 1,
        "priority": 1,
        "type": "task",
        "assignee": "https://seeds.local/principal/sattler",
        "owner": "https://seeds.local/principal/ian",
        "created": {"value": "2026-11-21T22:44:11Z", "datatype": "http://www.w3.org/2001/XMLSchema#dateTime"},
    })
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
fn fold_rows_and_keywords_strip_principals_and_merge_labels() {
    let rows = vec![
        row("aegis-1", "one", "open"),
        row("aegis-2", "two", "in_progress"),
    ];
    let mut items = fold_rows(&rows).unwrap();
    let kw = vec![
        json!({"id": "aegis-1", "keyword": "pitch"}),
        json!({"id": "aegis-1", "keyword": "bobbin"}),
        json!({"id": "aegis-1", "keyword": "pitch"}),
        json!({"id": "other", "keyword": "x"}),
    ];
    attach_keywords(&mut items, &kw, 10).unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].labels, vec!["pitch", "bobbin"]);
    assert_eq!(items[0].assignee.as_deref(), Some("sattler"));
    assert_eq!(items[0].owner, "ian");
    assert_eq!(items[0].priority, 1);
    assert_eq!(items[0].created_at.as_deref(), Some("2026-11-21T22:44:11Z"));
    assert!(items[1].labels.is_empty());
}

#[test]
fn multi_valued_scalar_facts_are_refused_not_sampled() {
    let mut r = row("aegis-1", "one", "open");
    r["n_title"] = json!(2);
    let err = fold_rows(&[r]).unwrap_err().to_string();
    assert!(err.contains("2 title values"), "{err}");
    let mut r = row("aegis-1", "one", "open");
    r["n_status"] = json!({"value": "3", "datatype": "xsd:integer"});
    assert!(fold_rows(&[r]).is_err());
}

#[test]
fn keyword_reply_past_cap_is_an_error() {
    let mut items = fold_rows(&[row("aegis-1", "one", "open")]).unwrap();
    let kw: Vec<Value> = (0..3)
        .map(|i| json!({"id": "aegis-1", "keyword": format!("k{i}")}))
        .collect();
    assert!(attach_keywords(&mut items, &kw, 2).is_err());
    assert!(attach_keywords(&mut items, &kw, 3).is_ok());
}

#[test]
fn unparseable_priority_and_missing_type_fall_back() {
    let mut r = row("aegis-1", "one", "open");
    r["priority"] = json!("high");
    r.as_object_mut().unwrap().remove("type");
    let items = fold_rows(&[r]).unwrap();
    assert_eq!(items[0].priority, 2);
    assert_eq!(items[0].issue_type, "task");
}

#[test]
fn inbox_pointers_and_closed_items_never_become_chunks() {
    let items = fold_rows(&[
        row("aegis-1", "real work mentioning inbox", "open"),
        row("aegis-2", "  Inbox: mail pointer", "open"),
        row("aegis-3", "done", "closed"),
    ])
    .unwrap();
    let chunks = items_to_chunks(&config(), "beads_aegis", items);
    let names: Vec<_> = chunks.iter().filter_map(|c| c.name.clone()).collect();
    assert_eq!(chunks.len(), 1, "{names:?}");
    assert!(chunks[0].file_path.contains("aegis-1"));
}

#[test]
fn queries_escape_ids_page_by_keyset_and_bound_rows() {
    let p = id_page_query(Some("aegis-\"x"), 50);
    assert!(p.contains(r#"FILTER(STR(?id) > "aegis-\"x")"#), "{p}");
    assert!(p.ends_with("ORDER BY ?id LIMIT 50"), "{p}");
    assert!(!id_page_query(None, 5).contains("FILTER"));
    let q = items_query(&["a\\b".to_string(), "c".to_string()]);
    assert!(q.contains(r#"VALUES ?id { "a\\b" "c" }"#), "{q}");
    assert!(q.ends_with("GROUP BY ?id ORDER BY ?id LIMIT 2"), "{q}");
    assert!(
        !q.contains("keywords"),
        "keywords must not multiply item rows: {q}"
    );
    let k = keywords_query(&["a\"b".to_string()], 64);
    assert!(k.contains(r#"VALUES ?id { "a\"b" }"#), "{k}");
    assert!(k.ends_with("LIMIT 65"), "{k}");
}

#[test]
fn truncated_or_rowless_replies_are_errors_not_empty_boards() {
    assert!(parse_query_reply(r#"{"rows":[],"truncated":true}"#, "g").is_err());
    assert!(parse_query_reply(r#"{"error":"x"}"#, "g").is_err());
    // No `truncated` field is unproven completeness, not completeness.
    assert!(parse_query_reply(r#"{"rows":[]}"#, "g").is_err());
    assert!(parse_query_reply(r#"{"rows":[],"truncated":null}"#, "g").is_err());
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
    // next_tx is the reply's cursor, used verbatim as the next (exclusive)
    // `since`; it is never derived from record txs.
    let p = parse_changes(r#"{"records":[],"next_tx":7}"#).unwrap();
    assert!(p.item_ids.is_empty());
    assert_eq!((p.next_tx, p.watermark_tx), (7, 7));
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
