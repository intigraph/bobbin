//! Ranking, budget and rendering of memory items for one prompt. Pure: no IO,
//! no network, so the hook path stays local.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::*;

const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "what", "when", "where", "which", "who",
    "why", "how", "are", "was", "were", "has", "have", "had", "not", "but", "you", "your", "our",
    "can", "will", "should", "would", "could", "about", "into", "onto", "than", "then", "them",
    "they", "their", "there", "here", "its", "it's", "does", "did", "done", "any", "all", "out",
    "use", "using", "get", "got", "let", "lets", "let's", "now", "just", "also", "some", "more",
    "make", "need", "want", "please", "thanks", "look", "check", "see",
];

fn tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 3)
        .map(str::to_lowercase)
        .filter(|t| !STOPWORDS.contains(&t.as_str()))
}

fn item_tokens(item: &MemoryItem) -> (HashSet<String>, HashSet<String>) {
    let key: HashSet<String> = tokens(&item.natural_key)
        .chain(tokens(&item.kind))
        .filter(|t| t != "aegis" && t != "mem" && t != "http" && t != "https")
        .collect();
    let mut all: HashSet<String> = tokens(&item.label).chain(tokens(&item.body)).collect();
    all.extend(key.iter().cloned());
    (all, key)
}

/// Rank mirror items against a prompt; at most `limit`, best first.
///
/// Lexical, IDF-weighted. An item qualifies with two distinct prompt terms in
/// its text, or one that is part of its natural key or kind (the key is built
/// from registered terms, so a hit there is a strong signal). Ties go to the
/// higher plane (declared first), then the newer record.
#[must_use]
pub fn rank<'a>(
    prompt: &str,
    mirror: &'a Mirror,
    limit: usize,
    now_rfc3339: &str,
) -> Vec<&'a MemoryItem> {
    let query: HashSet<String> = tokens(prompt).collect();
    if query.is_empty() || limit == 0 {
        return Vec::new();
    }
    let live: Vec<(&MemoryItem, HashSet<String>, HashSet<String>)> = mirror
        .items
        .values()
        .filter(|i| !i.is_expired(now_rfc3339))
        .map(|i| {
            let (all, key) = item_tokens(i);
            (i, all, key)
        })
        .collect();
    let n = live.len() as f64;
    let mut df: HashMap<&str, usize> = HashMap::new();
    for (_, all, _) in &live {
        for t in query.iter().filter(|t| all.contains(*t)) {
            *df.entry(t.as_str()).or_default() += 1;
        }
    }
    let mut scored: Vec<(f64, &MemoryItem)> = live
        .iter()
        .filter_map(|(item, all, key)| {
            let hits: Vec<&String> = query.iter().filter(|t| all.contains(*t)).collect();
            let key_hit = hits.iter().any(|t| key.contains(*t));
            if hits.len() < 2 && !key_hit {
                return None;
            }
            let score: f64 = hits
                .iter()
                .map(|t| {
                    let d = df.get(t.as_str()).copied().unwrap_or(1) as f64;
                    let idf = (1.0 + n / d).ln();
                    if key.contains(*t) {
                        idf * 1.5
                    } else {
                        idf
                    }
                })
                .sum();
            Some((score, *item))
        })
        .collect();
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.plane_rank().cmp(&b.1.plane_rank()))
            .then(b.1.recorded_at.cmp(&a.1.recorded_at))
            .then(a.1.iri.cmp(&b.1.iri))
    });
    scored.into_iter().take(limit).map(|(_, i)| i).collect()
}

// ---------------------------------------------------------------------------
// Budget and rendering
// ---------------------------------------------------------------------------

/// How many memory items fit in the knowledge share of the hook budget.
///
/// `budget_lines * knowledge_budget_pct / 100` lines (45 at the defaults of
/// 300 and 15%), one of them the header, one line per item, capped at
/// [`MAX_ITEMS`].
#[must_use]
pub fn item_cap(budget_lines: usize, knowledge_budget_pct: f32) -> usize {
    let pct = knowledge_budget_pct.clamp(0.0, 100.0);
    let lines = ((budget_lines as f32) * pct / 100.0).floor() as usize;
    lines.saturating_sub(1).min(MAX_ITEMS)
}

/// One cited item, as logged for feedback resolution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CitedItem {
    /// `m1`..`m5`.
    pub item: String,
    pub iri: String,
    pub kind: String,
    pub natural_key: String,
    /// The line the agent saw.
    pub line: String,
}

/// A rendered memory block.
#[derive(Debug, Default)]
pub struct Rendered {
    pub text: String,
    pub items: Vec<CitedItem>,
}

fn source_label(source_kind: &str) -> String {
    let s = local_name(source_kind);
    if s.is_empty() {
        "unknown".into()
    } else {
        s.to_string()
    }
}

/// Format `[mN] <Kind>: <label> - <body> (<sourceKind>, <date>) <iri>` lines
/// under the header, within [`MAX_TOTAL_CHARS`].
#[must_use]
pub fn render(items: &[&MemoryItem], stale_note: Option<&str>) -> Rendered {
    let mut out = Rendered::default();
    if items.is_empty() {
        return out;
    }
    let mut lines: Vec<String> = Vec::new();
    let mut total = 0usize;
    for item in items {
        let n = out.items.len() + 1;
        let date: String = item.recorded_at.chars().take(10).collect();
        let line = format!(
            "[m{n}] {}: {} ({}, {}) {}",
            if item.kind.is_empty() {
                "Memory"
            } else {
                &item.kind
            },
            item.text(),
            source_label(&item.source_kind),
            if date.is_empty() { "undated" } else { &date },
            item.iri
        );
        let len = line.chars().count();
        if total + len > MAX_TOTAL_CHARS {
            break;
        }
        total += len;
        out.items.push(CitedItem {
            item: format!("m{n}"),
            iri: item.iri.clone(),
            kind: item.kind.clone(),
            natural_key: item.natural_key.clone(),
            line: line.clone(),
        });
        lines.push(line);
    }
    if lines.is_empty() {
        return out;
    }
    let header = match stale_note {
        Some(note) => format!("{HEADER} - {note}"),
        None => HEADER.to_string(),
    };
    out.text = format!("{header}\n{}\n", lines.join("\n"));
    out
}

/// What the hook prints for memory on one prompt.
#[derive(Debug, PartialEq)]
pub enum Recall {
    /// Nothing to say: no endpoint, no match, or a cold mirror still loading.
    Silent,
    /// The one short outage line.
    Skipped(String),
    /// A memory block with cited items.
    Items { text: String, items: Vec<CitedItem> },
}

/// Decide the memory output for one prompt. Pure: no IO, no network.
#[must_use]
pub fn recall_for_prompt(
    query: &str,
    mirror: Option<&Mirror>,
    endpoint: &str,
    budget_lines: usize,
    knowledge_budget_pct: f32,
    now: u64,
) -> Recall {
    let cap = item_cap(budget_lines, knowledge_budget_pct);
    if cap == 0 {
        return Recall::Silent;
    }
    let mirror = mirror.filter(|m| m.endpoint == endpoint);
    let Some(mirror) = mirror else {
        return Recall::Silent;
    };
    let now_rfc = fmt_epoch(now);
    let hits = rank(query, mirror, cap, &now_rfc);
    let stale_note = match mirror.age_secs(now) {
        Some(age) if age > STALE_SECS => mirror
            .refreshed_at
            .map(|t| format!("memory index stale since {}", fmt_epoch(t))),
        _ => None,
    };
    let rendered = render(&hits, stale_note.as_deref());
    if !rendered.items.is_empty() {
        return Recall::Items {
            text: rendered.text,
            items: rendered.items,
        };
    }
    match (&mirror.last_error, mirror.refreshed_at) {
        (Some(_), None) => Recall::Skipped(
            "memory: recall skipped (quipu unreachable, no memory index yet)\n".into(),
        ),
        (Some(_), Some(t)) if now.saturating_sub(t) > STALE_SECS => Recall::Skipped(format!(
            "memory: recall skipped (quipu unreachable; index stale since {})\n",
            fmt_epoch(t)
        )),
        _ => Recall::Silent,
    }
}
