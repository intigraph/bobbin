//! plate — the agent's current work item, published by the harness.
//!
//! Bobbin's context injection is per-prompt and stateless: it sees the prompt
//! text and nothing about what the agent is actually assigned to. yupana's
//! `docs/work-scoped-governance.md` §3 names both the gap and the cost — agents
//! rediscover the same context repeatedly, because retrieval starts from zero
//! every prompt. Keyed on the work item, retrieval becomes cumulative: the
//! second session on an item starts where the first finished.
//!
//! WE READ A FILE; WE DO NOT ASK ANYONE. The tracker owns `(agent → work item)`
//! and has more than one backend behind one interface, so resolving it here
//! would create a SECOND implementation that can disagree with the first.
//! Shelling out to the tracker's CLI is equally rejected: this runs on the
//! prompt-submit path, once per prompt, and a subprocess against a
//! network-backed tracker is not affordable there. So shantytown PUBLISHES
//! (`shantytown/shantytown/plate_publish.py`) and consumers READ:
//!
//! ```text
//! $SHANTY_ROOT/crew/$SHANTY_AGENT/plate.json
//! {"item": "abc-123", "at": <unix secs>, "session": "<id>|null"}
//! ```
//!
//! yupana reads the same file (`yupana/src/plate.rs`). That is the point of the
//! file existing rather than being a call: two tools, one published answer, no
//! second implementation. Bobbin deliberately does NOT ask yupana — the two own
//! different halves of an agent's context (bobbin: semantic code; yupana:
//! governance and work item) and nesting one tool's injection inside the
//! other's would inject the same context twice.
//!
//! ABSTAIN, NEVER GUESS. Missing, unreadable, malformed, empty or stale all
//! return `None`, and `None` means UNKNOWN — not "no work". A wrong work item
//! would silently bias every retrieval for a session toward the wrong subject,
//! which is worse than the stateless behaviour it replaces because it looks
//! like it is working.

use std::borrow::Cow;
use std::path::PathBuf;

/// A plate older than this is UNKNOWN. Overridable with `BOBBIN_PLATE_MAX_AGE_SECS`.
///
/// Matches yupana's default deliberately: two readers of one file disagreeing
/// about when it goes stale would attribute the same action to different items.
const DEFAULT_MAX_AGE_SECS: u64 = 4 * 60 * 60;

/// Where the harness publishes this agent's plate, from the environment.
///
/// `None` when either variable is absent rather than guessing a root — a guess
/// would read some OTHER deployment's plate, which is worse than reading none.
fn plate_path() -> Option<PathBuf> {
    let root = std::env::var("SHANTY_ROOT").ok()?;
    let agent = std::env::var("SHANTY_AGENT").ok()?;
    if root.is_empty() || agent.is_empty() {
        return None;
    }
    Some(
        PathBuf::from(root)
            .join("crew")
            .join(agent)
            .join("plate.json"),
    )
}

/// Parse a plate document, or `None` for anything we cannot trust.
///
/// Pure, so the staleness rule is testable without touching the clock or the
/// filesystem.
#[must_use]
pub fn parse(doc: &str, now: u64, max_age: u64) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(doc).ok()?;
    let item = value.get("item")?.as_str()?.trim().to_string();
    if item.is_empty() {
        return None;
    }
    // A plate written while an item was open keeps answering after that item
    // closes. The age check is a backstop against a file left behind by a dead
    // session, not a fix for that window — see yupana/src/plate.rs, which
    // documents the residual gap this shares.
    let at = value.get("at").and_then(serde_json::Value::as_u64)?;
    if now.saturating_sub(at) > max_age {
        return None;
    }
    Some(item)
}

/// The agent's current work item, or `None` (UNKNOWN).
#[must_use]
pub fn current() -> Option<String> {
    let path = plate_path()?;
    let doc = std::fs::read_to_string(path).ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let max_age = std::env::var("BOBBIN_PLATE_MAX_AGE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_MAX_AGE_SECS);
    parse(&doc, now, max_age)
}

/// Add the published work item as a retrieval prior without replacing the prompt.
/// Read optional group declarations from the same fresh anchored plate.
/// No tracker query, environment-provided group override, or guessed task scope.
pub fn current_groups(item: Option<&str>) -> Option<Vec<String>> {
    let item = item?;
    let doc = std::fs::read_to_string(plate_path()?).ok()?;
    if doc.len() > 65536 {
        return None;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let max_age = std::env::var("BOBBIN_PLATE_MAX_AGE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_AGE_SECS);
    parse_groups(&doc, item, now, max_age)
}

fn parse_groups(doc: &str, item: &str, now: u64, max_age: u64) -> Option<Vec<String>> {
    if parse(doc, now, max_age).as_deref() != Some(item) {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(doc).ok()?;
    if value.get("at")?.as_u64()? > now.saturating_add(60) {
        return None;
    }
    let groups = value.get("bobbin_groups")?.as_array()?;
    if groups.len() > 8 {
        return None;
    }
    groups
        .iter()
        .map(|v| {
            let s = v.as_str()?;
            if s.is_empty()
                || s.len() > 64
                || !s
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
            {
                return None;
            }
            Some(s.to_string())
        })
        .collect()
}

/// Add the published work item as a retrieval prior without replacing the prompt.
///
/// Shared by local and remote injection so the two execution modes cannot
/// disagree about how work-item scope biases retrieval. Unknown leaves the
/// original query borrowed and byte-identical.
#[must_use]
pub fn scope_query<'a>(query: &'a str, item: Option<&str>) -> Cow<'a, str> {
    match item {
        Some(item) => Cow::Owned(format!("{query} {item}")),
        None => Cow::Borrowed(query),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse, parse_groups, scope_query};

    #[test]
    fn groups_require_the_same_fresh_item_and_bounded_names() {
        let doc = r#"{"item":"work-1","at":1000,"bobbin_groups":["engine"]}"#;
        assert_eq!(
            parse_groups(doc, "work-1", 1001, 60),
            Some(vec!["engine".into()])
        );
        assert!(parse_groups(doc, "work-2", 1001, 60).is_none());
        assert!(parse_groups(doc, "work-1", 2000, 60).is_none());
        assert!(parse_groups(doc, "work-1", 0, 60).is_none());
        for groups in [
            r#"["bad,name"]"#,
            r#"["../other"]"#,
            r#""engine""#,
            r#"[null]"#,
        ] {
            let doc = format!(r#"{{"item":"work-1","at":1000,"bobbin_groups":{groups}}}"#);
            assert!(parse_groups(&doc, "work-1", 1001, 60).is_none());
        }
    }

    #[test]
    fn a_fresh_plate_yields_its_item() {
        let doc = r#"{"item":"aegis-1","at":1000,"session":"s"}"#;
        assert_eq!(parse(doc, 1_500, 3_600), Some("aegis-1".to_string()));
    }

    /// STALENESS IS THE SUBTLE ONE. A plate left by a dead session keeps
    /// answering, plausibly, which is the dangerous kind of wrong: it would
    /// bias every retrieval toward an item nobody is working on while looking
    /// exactly like it is working.
    #[test]
    fn a_stale_plate_is_UNKNOWN_not_a_best_guess() {
        let doc = r#"{"item":"aegis-1","at":1000,"session":"s"}"#;
        assert_eq!(parse(doc, 100_000, 3_600), None);
    }

    /// Every unreadable shape abstains. None of these is an error worth
    /// surfacing — they all mean the same thing, which is that we do not know.
    #[test]
    fn malformed_empty_and_undated_plates_all_abstain() {
        for doc in [
            "not json",
            "{}",
            r#"{"item":""}"#,
            r#"{"item":"   "}"#,
            r#"{"item":"aegis-1"}"#, // no `at`: cannot judge staleness, so cannot trust
        ] {
            assert_eq!(parse(doc, 1_500, 3_600), None, "should abstain on: {doc}");
        }
    }

    #[test]
    fn scope_is_an_additive_retrieval_prior() {
        assert_eq!(
            scope_query("how does dispatch work", Some("aegis-368cu.9")),
            "how does dispatch work aegis-368cu.9"
        );
    }

    #[test]
    fn unknown_scope_preserves_the_query_byte_for_byte() {
        let query = "  keep my exact whitespace  ";
        let scoped = scope_query(query, None);
        assert!(matches!(scoped, std::borrow::Cow::Borrowed(_)));
        assert_eq!(scoped, query);
    }
}
