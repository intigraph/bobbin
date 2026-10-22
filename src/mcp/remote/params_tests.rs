//! Proof for the request -> query-parameter mapping in `params.rs`.
//!
//! In its own file only to stay under the repo's size ratchet; it is the
//! proof for `params.rs` and should be read with it.

use super::params::*;
use crate::mcp::tools::*;

/// Every field of every proxied request must reach the server.
///
/// This is the test that matters. A query parameter the endpoint does not
/// recognise is ignored, not rejected, so a dropped filter widens the
/// result set and returns a confident, plausible, wrong answer. The
/// obvious implementation — routing these tools through the typed
/// `Client` helpers the CLI uses — drops `tag`, `exclude_tag` and
/// `bundle` from `search` for exactly that reason, because those helpers
/// model only the parameters the CLI exposes.
///
/// Each case below sets every optional field to a distinctive value and
/// asserts it arrives under the name the HTTP handler actually reads.

#[test]
fn search_carries_every_filter_including_the_tag_and_bundle_ones() {
    let p = search_params(&SearchRequest {
        query: "auth".into(),
        r#type: Some("function".into()),
        limit: Some(7),
        mode: Some("keyword".into()),
        repo: Some("quipu".into()),
        tag: Some("core,api".into()),
        exclude_tag: Some("test".into()),
        bundle: Some("context/pipeline".into()),
    });
    assert_eq!(p.get("q"), Some("auth"));
    assert_eq!(p.get("mode"), Some("keyword"));
    assert_eq!(p.get("limit"), Some("7"));
    assert_eq!(p.get("type"), Some("function"));
    assert_eq!(p.get("repo"), Some("quipu"));
    assert_eq!(p.get("tag"), Some("core,api"));
    assert_eq!(p.get("exclude_tag"), Some("test"));
    assert_eq!(p.get("bundle"), Some("context/pipeline"));
}

#[test]
fn search_defaults_match_the_local_tool() {
    let p = search_params(&SearchRequest {
        query: "auth".into(),
        r#type: None,
        limit: None,
        mode: None,
        repo: None,
        tag: None,
        exclude_tag: None,
        bundle: None,
    });
    assert_eq!(p.get("mode"), Some("hybrid"));
    assert_eq!(p.get("limit"), Some("10"));
    assert_eq!(p.keys(), vec!["q", "mode", "limit"]);
}

#[test]
fn grep_carries_every_field() {
    let p = grep_params(&GrepRequest {
        pattern: "fn main".into(),
        ignore_case: Some(true),
        regex: Some(true),
        r#type: Some("function".into()),
        limit: Some(3),
        repo: Some("bobbin".into()),
    });
    assert_eq!(p.get("pattern"), Some("fn main"));
    assert_eq!(p.get("ignore_case"), Some("true"));
    assert_eq!(p.get("regex"), Some("true"));
    assert_eq!(p.get("type"), Some("function"));
    assert_eq!(p.get("limit"), Some("3"));
    assert_eq!(p.get("repo"), Some("bobbin"));
}

#[test]
fn context_carries_every_field() {
    let p = context_params(&ContextRequest {
        query: "task".into(),
        budget: Some(900),
        depth: Some(2),
        max_coupled: Some(5),
        limit: Some(30),
        coupling_threshold: Some(0.25),
        repo: Some("quipu".into()),
        tag: Some("core".into()),
        exclude_tag: Some("generated".into()),
        bundle: Some("hook".into()),
    });
    assert_eq!(p.get("q"), Some("task"));
    assert_eq!(p.get("budget"), Some("900"));
    assert_eq!(p.get("depth"), Some("2"));
    assert_eq!(p.get("max_coupled"), Some("5"));
    assert_eq!(p.get("limit"), Some("30"));
    assert_eq!(p.get("coupling_threshold"), Some("0.25"));
    assert_eq!(p.get("repo"), Some("quipu"));
    assert_eq!(p.get("tag"), Some("core"));
    assert_eq!(p.get("exclude_tag"), Some("generated"));
    assert_eq!(p.get("bundle"), Some("hook"));
}

#[test]
fn read_chunk_carries_every_field() {
    let p = read_chunk_params(&ReadChunkRequest {
        repo: Some("alpha".into()),
        file: "src/main.rs".into(),
        start_line: 10,
        end_line: 42,
        context: Some(3),
    });
    assert_eq!(p.get("repo"), Some("alpha"));
    assert_eq!(p.get("file"), Some("src/main.rs"));
    assert_eq!(p.get("start_line"), Some("10"));
    assert_eq!(p.get("end_line"), Some("42"));
    assert_eq!(p.get("context"), Some("3"));
}

#[test]
fn related_and_refs_and_symbols_carry_every_field() {
    let p = related_params(&RelatedRequest {
        file: "src/a.rs".into(),
        limit: Some(4),
        threshold: Some(0.5),
        repo: Some("bobbin".into()),
    });
    assert_eq!(p.get("file"), Some("src/a.rs"));
    assert_eq!(p.get("limit"), Some("4"));
    assert_eq!(p.get("threshold"), Some("0.5"));
    assert_eq!(p.get("repo"), Some("bobbin"));

    let p = find_refs_params(&FindRefsRequest {
        symbol: "parse_config".into(),
        r#type: Some("function".into()),
        limit: Some(9),
        repo: Some("bobbin".into()),
    });
    assert_eq!(p.get("symbol"), Some("parse_config"));
    assert_eq!(p.get("type"), Some("function"));
    assert_eq!(p.get("limit"), Some("9"));
    assert_eq!(p.get("repo"), Some("bobbin"));

    let p = list_symbols_params(&ListSymbolsRequest {
        file: "src/a.rs".into(),
        repo: Some("bobbin".into()),
    });
    assert_eq!(p.get("file"), Some("src/a.rs"));
    assert_eq!(p.get("repo"), Some("bobbin"));
}

#[test]
fn deps_history_hotspots_impact_similar_carry_every_field() {
    let p = dependencies_params(&DependenciesRequest {
        file: "src/a.rs".into(),
        reverse: Some(true),
        both: Some(true),
    });
    assert_eq!(p.get("file"), Some("src/a.rs"));
    assert_eq!(p.get("reverse"), Some("true"));
    assert_eq!(p.get("both"), Some("true"));

    let p = file_history_params(&FileHistoryRequest {
        file: "src/a.rs".into(),
        limit: Some(5),
    });
    assert_eq!(p.get("file"), Some("src/a.rs"));
    assert_eq!(p.get("limit"), Some("5"));

    let p = hotspots_params(&HotspotsRequest {
        since: Some("6 months ago".into()),
        limit: Some(8),
        threshold: Some(0.3),
    });
    assert_eq!(p.get("since"), Some("6 months ago"));
    assert_eq!(p.get("limit"), Some("8"));
    assert_eq!(p.get("threshold"), Some("0.3"));

    let p = impact_params(&ImpactRequest {
        target: "src/a.rs:login".into(),
        depth: Some(2),
        mode: Some("coupling".into()),
        limit: Some(11),
        threshold: Some(0.2),
        repo: Some("bobbin".into()),
    });
    assert_eq!(p.get("target"), Some("src/a.rs:login"));
    assert_eq!(p.get("depth"), Some("2"));
    assert_eq!(p.get("mode"), Some("coupling"));
    assert_eq!(p.get("limit"), Some("11"));
    assert_eq!(p.get("threshold"), Some("0.2"));
    assert_eq!(p.get("repo"), Some("bobbin"));

    let p = similar_params(&SimilarRequest {
        target: Some("a.rs:foo".into()),
        scan: Some(true),
        threshold: Some(0.9),
        limit: Some(6),
        repo: Some("bobbin".into()),
        cross_repo: Some(true),
    });
    assert_eq!(p.get("target"), Some("a.rs:foo"));
    assert_eq!(p.get("scan"), Some("true"));
    assert_eq!(p.get("threshold"), Some("0.9"));
    assert_eq!(p.get("limit"), Some("6"));
    assert_eq!(p.get("repo"), Some("bobbin"));
    assert_eq!(p.get("cross_repo"), Some("true"));
}

#[test]
fn search_beads_carries_every_filter() {
    let p = search_beads_params(&SearchBeadsRequest {
        query: "cert expiry".into(),
        priority: Some(1),
        status: Some("open".into()),
        assignee: Some("aegis/crew/gennaro".into()),
        rig: Some("aegis".into()),
        issue_type: Some("bug".into()),
        label: Some("tech-debt".into()),
        limit: Some(12),
        enrich: Some(false),
        compact: Some(false),
    });
    assert_eq!(p.get("q"), Some("cert expiry"));
    assert_eq!(p.get("priority"), Some("1"));
    assert_eq!(p.get("status"), Some("open"));
    assert_eq!(p.get("assignee"), Some("aegis/crew/gennaro"));
    assert_eq!(p.get("rig"), Some("aegis"));
    assert_eq!(p.get("issue_type"), Some("bug"));
    assert_eq!(p.get("label"), Some("tech-debt"));
    assert_eq!(p.get("limit"), Some("12"));
    assert_eq!(p.get("enrich"), Some("false"));
    assert_eq!(p.get("compact"), Some("false"));
}

/// The tool parameter is `filter`; the endpoint reads `name_filter`.
/// Forwarding it under the tool's own name is accepted and ignored — the
/// search runs unfiltered and looks like it worked.
#[test]
fn archive_search_renames_filter_to_name_filter() {
    let p = archive_search_params(&ArchiveSearchRequest {
        query: "deploy failures".into(),
        source: Some("hla".into()),
        filter: Some("telegram".into()),
        after: Some("2026-01-01".into()),
        before: Some("2026-02-01".into()),
        limit: Some(4),
        mode: Some("semantic".into()),
    });
    assert_eq!(p.get("name_filter"), Some("telegram"));
    assert_eq!(p.get("filter"), None);
    assert_eq!(p.get("q"), Some("deploy failures"));
    assert_eq!(p.get("source"), Some("hla"));
    assert_eq!(p.get("after"), Some("2026-01-01"));
    assert_eq!(p.get("before"), Some("2026-02-01"));
    assert_eq!(p.get("limit"), Some("4"));
    assert_eq!(p.get("mode"), Some("semantic"));
}

#[test]
fn archive_recent_and_prime_carry_every_field() {
    let p = archive_recent_params(&ArchiveRecentRequest {
        after: Some("2026-01-01".into()),
        source: Some("pensieve".into()),
        limit: Some(5),
    });
    assert_eq!(p.get("after"), Some("2026-01-01"));
    assert_eq!(p.get("source"), Some("pensieve"));
    assert_eq!(p.get("limit"), Some("5"));

    let p = prime_params(&PrimeRequest {
        section: Some("architecture".into()),
        brief: Some(true),
    });
    assert_eq!(p.get("section"), Some("architecture"));
    assert_eq!(p.get("brief"), Some("true"));
}

#[test]
fn feedback_params_carry_every_field() {
    let p = feedback_list_params(&FeedbackListRequest {
        rating: Some("useful".into()),
        agent: Some("gennaro".into()),
        limit: Some(15),
    });
    assert_eq!(p.get("rating"), Some("useful"));
    assert_eq!(p.get("agent"), Some("gennaro"));
    assert_eq!(p.get("limit"), Some("15"));

    let p = feedback_lineage_list_params(&FeedbackLineageListRequest {
        feedback_id: Some(42),
        bead: Some("aegis-wbbycq".into()),
        commit_hash: Some("deadbeef".into()),
        limit: Some(2),
    });
    assert_eq!(p.get("feedback_id"), Some("42"));
    assert_eq!(p.get("bead"), Some("aegis-wbbycq"));
    assert_eq!(p.get("commit_hash"), Some("deadbeef"));
    assert_eq!(p.get("limit"), Some("2"));
}

#[test]
fn read_chunk_without_repo_remains_compatible() {
    let req: ReadChunkRequest = serde_json::from_value(serde_json::json!({
        "file": "repos/alpha/src/main.rs", "start_line": 1, "end_line": 5
    }))
    .unwrap();
    assert!(req.repo.is_none());
    assert_eq!(read_chunk_params(&req).get("repo"), None);
}
