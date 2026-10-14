//! Cheap SessionStart discovery of repository-local breadcrumbs.
//! This reads saved metadata only: no model, search, recall mutation or network.

use std::path::Path;

use chrono::Utc;

use crate::breadcrumb::BreadcrumbStore;

/// Reserve at most 30% of a compaction budget (and at most 30 lines) for
/// discovery. Missing or unreadable stores preserve the ordinary hook output.
pub(super) fn discovery(root: &Path, budget: usize) -> String {
    let limit = (budget / 10 * 3 + budget % 10 * 3 / 10).min(30);
    if limit < 2 {
        return String::new();
    }
    let store = match BreadcrumbStore::new(root).load() {
        Ok(store) => store,
        Err(error) => {
            eprintln!("bobbin: breadcrumb discovery unavailable: {error:#}");
            return String::new();
        }
    };
    let now = Utc::now();
    let entries: Vec<_> = store
        .values()
        .filter(|bc| {
            bc.ttl_days == 0
                || now.signed_duration_since(bc.last_recalled.unwrap_or(bc.created_at))
                    < chrono::Duration::days(i64::from(bc.ttl_days))
        })
        .take(limit - 1)
        .map(|bc| format!("- {}: {}\n", one_line(&bc.name), one_line(&bc.description)))
        .collect();
    if entries.is_empty() {
        String::new()
    } else {
        format!("=== Breadcrumbs ===\n{}", entries.concat())
    }
}

// A saved multiline description must not consume an unbounded hook budget.
fn one_line(text: &str) -> String {
    text.chars()
        .take(240)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
