//! Text and argument checks for the feedback MCP tools, kept out of
//! `server.rs` (file-size ratchet).

use crate::storage::feedback::{is_valid_item, FeedbackStats};

/// Normalize the optional cited item of a `feedback_submit` call, or the
/// plain-word error to return. `used` rates one cited memory item, so it
/// needs one.
pub(super) fn check_item(rating: &str, item: Option<&str>) -> Result<Option<String>, String> {
    let item = item.map(str::trim).filter(|s| !s.is_empty());
    if let Some(it) = item {
        if !is_valid_item(it) {
            return Err(format!(
                "item '{it}' is not a cited item id (expected m1..m5 from the [m<n>] lines)"
            ));
        }
    }
    if rating == "used" && item.is_none() {
        return Err("rating 'used' rates one cited memory item; pass item (m1..m5)".into());
    }
    Ok(item.map(String::from))
}

/// `inj-xxx` or `inj-xxx m2`.
pub(super) fn target(injection_id: &str, item: Option<&str>) -> String {
    match item {
        Some(it) => format!("{injection_id} {it}"),
        None => injection_id.to_string(),
    }
}

/// The aggregated `feedback_stats` text.
pub(super) fn stats_text(stats: &FeedbackStats) -> String {
    let rate = if stats.total_injections > 0 {
        format!(
            "{:.0}%",
            stats.total_feedback as f64 / stats.total_injections as f64 * 100.0
        )
    } else {
        "N/A".to_string()
    };
    let mut text = format!(
        "Injections: {}\nFeedback: {} ({} coverage)\n  useful: {}\n  noise: {}\n  harmful: {}\n  used: {}",
        stats.total_injections,
        stats.total_feedback,
        rate,
        stats.useful,
        stats.noise,
        stats.harmful,
        stats.used
    );
    if !stats.memory_entities.is_empty() {
        text.push_str(&format!(
            "\nMemory items: {} item ratings\n  {:<9} {:>4} {:>6} {:>5} {:>7}  entity",
            stats.item_feedback, "kind", "inj", "used", "noise", "harmful"
        ));
        for e in &stats.memory_entities {
            text.push_str(&format!(
                "\n  {:<9} {:>4} {:>6} {:>5} {:>7}  {}",
                e.kind, e.injected, e.used, e.noise, e.harmful, e.iri
            ));
        }
    }
    if stats.lineage_records > 0 || stats.actioned > 0 {
        text.push_str(&format!(
            "\nLineage: {} records\n  actioned: {}\n  unactioned: {}",
            stats.lineage_records, stats.actioned, stats.unactioned
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_item_rules() {
        assert_eq!(check_item("useful", None), Ok(None));
        assert_eq!(check_item("used", Some(" m2 ")), Ok(Some("m2".into())));
        assert!(check_item("used", None).unwrap_err().contains("'used'"));
        assert!(check_item("noise", Some("x9"))
            .unwrap_err()
            .contains("not a cited item"));
        assert_eq!(target("inj-1", Some("m1")), "inj-1 m1");
    }
}
