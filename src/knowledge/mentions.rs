//! Chunk→entity reconcile mapping (roadmap W2.P5, bobbin-c79).
//!
//! Adopts quipu's `src/reconcile/` pattern: the index lane writes the WEAK
//! literal form — `bobbin:mentions "SymbolName"` on `bobbin:Chunk` facts,
//! derived strictly from what the parser already observed (the chunk's own
//! symbol name plus the symbol names its edges target; never model
//! inference) — and an idempotent second pass resolves those literals
//! against the live entity graph into typed `Ref` edges, classifying each
//! mention `Resolved` | `Dangling` | `Ambiguous` honestly.
//!
//! One deliberate divergence from quipu's reconcile: the literal is NOT
//! retracted on resolution. The chunk-snapshot lane (`replace_snapshot`,
//! W2.P4) owns the literals and would re-assert them on the next reindex
//! anyway; leaving them in place keeps the pass a true no-op on re-run
//! (identical report, zero writes) instead of oscillating retract/assert
//! with every snapshot replace. The resolved `Ref` edge lives on the SAME
//! `bobbin:mentions` predicate — quipu's weak-Str/strong-Ref idiom — under
//! its own transaction source (`mention-reconcile`), outside the snapshot.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

use crate::iri::{CODE_BASE, ONTOLOGY_NS};

/// Full IRI of the `bobbin:mentions` predicate.
pub(crate) fn mentions_iri() -> String {
    format!("{ONTOLOGY_NS}mentions")
}

/// Full IRI of the `bobbin:name` predicate the live entity graph uses.
fn name_iri() -> String {
    format!("{ONTOLOGY_NS}name")
}

// ── Weak-literal emission (index/snapshot time) ───────────────

// Pure name projection is also used by the disposable document batch lab.
mod names;
pub(crate) use names::{chunk_mention_names, edge_mention_map};

// ── Reconcile pass (second pass, idempotent) ──────────────────

/// Outcome for a single mention literal.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "status")]
pub enum MentionResolution {
    /// Exactly one entity match — the `Ref` edge exists (written now or on
    /// a prior run).
    Resolved {
        chunk_iri: String,
        name: String,
        entity_iri: String,
    },
    /// No match — the entity graph does not (yet) know this symbol. The
    /// literal stays; a later run resolves it once the entity appears.
    Dangling { chunk_iri: String, name: String },
    /// Multiple matches and no same-module tiebreak — left unresolved,
    /// never coin-flipped.
    Ambiguous {
        chunk_iri: String,
        name: String,
        candidates: Vec<String>,
    },
}

/// Honest summary of one reconcile pass.
///
/// Re-running against an unchanged store yields identical counts and
/// details with `edges_written == 0` — the no-op proof.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MentionReport {
    pub resolved: usize,
    pub dangling: usize,
    pub ambiguous: usize,
    /// `Ref` edges written THIS run (0 on an idempotent re-run).
    pub edges_written: usize,
    pub details: Vec<MentionResolution>,
}

/// Resolve `bobbin:mentions` string literals against the live entity graph.
///
/// Match rule, in order:
/// 1. exact `bobbin:name` match among `CODE_BASE` entities;
/// 2. on multiple matches, exactly one candidate in the mentioning chunk's
///    own module (same `{repo}/{path}` IRI prefix) wins — quipu's
///    module-hint idiom, deterministic, not a coin flip;
/// 3. otherwise `Ambiguous`, reported and left alone.
///
/// Idempotent: a mention whose `Ref` edge already exists (and whose target
/// still carries the mentioned name) is reported `Resolved` without any
/// write; the store's own duplicate-assertion guard backstops this.
pub fn reconcile_mentions(store: &mut quipu::Store, timestamp: &str) -> Result<MentionReport> {
    let mut report = MentionReport {
        resolved: 0,
        dangling: 0,
        ambiguous: 0,
        edges_written: 0,
        details: Vec::new(),
    };

    let Some(mentions_id) = store
        .lookup(&mentions_iri())
        .map_err(|e| anyhow::anyhow!("mentions predicate lookup failed: {e}"))?
    else {
        // No mention literal was ever written — nothing to reconcile.
        return Ok(report);
    };
    let name_id = store
        .lookup(&name_iri())
        .map_err(|e| anyhow::anyhow!("name predicate lookup failed: {e}"))?;

    let facts = store
        .current_facts()
        .map_err(|e| anyhow::anyhow!("reading current facts failed: {e}"))?;

    // Name → [(entity id, IRI)] over the live CODE_BASE entity graph, and
    // the reverse (entity id → names) for prior-resolution checks. A store
    // with no `bobbin:name` facts yields empty indexes — every mention
    // then dangles, which is the honest answer, not an error.
    let mut name_index: HashMap<&str, Vec<(i64, String)>> = HashMap::new();
    let mut entity_names: HashMap<i64, Vec<&str>> = HashMap::new();
    if let Some(name_id) = name_id {
        for fact in &facts {
            if fact.attribute != name_id {
                continue;
            }
            if let quipu::types::Value::Str(ref name) = fact.value {
                let iri = store
                    .resolve(fact.entity)
                    .map_err(|e| anyhow::anyhow!("IRI resolve failed: {e}"))?;
                if iri.starts_with(CODE_BASE) {
                    name_index
                        .entry(name.as_str())
                        .or_default()
                        .push((fact.entity, iri));
                    entity_names.entry(fact.entity).or_default().push(name);
                }
            }
        }
    }

    // Existing Ref edges on the mentions predicate: chunk entity → targets.
    let mut existing_refs: HashMap<i64, Vec<i64>> = HashMap::new();
    for fact in &facts {
        if fact.attribute != mentions_id {
            continue;
        }
        if let quipu::types::Value::Ref(target) = fact.value {
            existing_refs.entry(fact.entity).or_default().push(target);
        }
    }

    let mut datums: Vec<quipu::store::Datum> = Vec::new();

    for fact in &facts {
        if fact.attribute != mentions_id {
            continue;
        }
        let quipu::types::Value::Str(ref name) = fact.value else {
            continue;
        };
        let chunk_iri = store
            .resolve(fact.entity)
            .map_err(|e| anyhow::anyhow!("IRI resolve failed: {e}"))?;

        // Prior resolution: a Ref edge to an entity that still carries this
        // name means this mention was resolved on an earlier run. Keep that
        // classification — do not recompute it away.
        let prior = existing_refs.get(&fact.entity).and_then(|targets| {
            targets.iter().find(|t| {
                entity_names
                    .get(t)
                    .is_some_and(|names| names.contains(&name.as_str()))
            })
        });
        if let Some(&target) = prior {
            let entity_iri = store
                .resolve(target)
                .map_err(|e| anyhow::anyhow!("IRI resolve failed: {e}"))?;
            report.resolved += 1;
            report.details.push(MentionResolution::Resolved {
                chunk_iri,
                name: name.clone(),
                entity_iri,
            });
            continue;
        }

        let matches: &[(i64, String)] = name_index.get(name.as_str()).map_or(&[], |v| v.as_slice());

        // Same-module narrowing for multi-match. The chunk may be on its own
        // `chunk/` lane or, when dual-typed, on the `code/` lane under the
        // symbol identity it merged into (aegis-6noan) — `code_module_prefix_of`
        // accepts both and yields the `{module}::` its file's symbols share.
        let chosen = match matches {
            [] => None,
            [only] => Some(only),
            many => {
                let module_prefix = crate::iri::code_module_prefix_of(&chunk_iri);
                let scoped: Vec<&(i64, String)> = module_prefix
                    .as_deref()
                    .map(|prefix| {
                        many.iter()
                            .filter(|(_, iri)| iri.starts_with(prefix))
                            .collect()
                    })
                    .unwrap_or_default();
                match scoped.as_slice() {
                    [only] => Some(*only),
                    _ => None,
                }
            }
        };

        match chosen {
            Some((target_id, target_iri)) => {
                datums.push(quipu::store::Datum {
                    entity: fact.entity,
                    attribute: mentions_id,
                    value: quipu::types::Value::Ref(*target_id),
                    valid_from: timestamp.to_string(),
                    valid_to: None,
                    op: quipu::types::Op::Assert,
                });
                report.resolved += 1;
                report.edges_written += 1;
                report.details.push(MentionResolution::Resolved {
                    chunk_iri,
                    name: name.clone(),
                    entity_iri: target_iri.clone(),
                });
            }
            None if matches.is_empty() => {
                report.dangling += 1;
                report.details.push(MentionResolution::Dangling {
                    chunk_iri,
                    name: name.clone(),
                });
            }
            None => {
                report.ambiguous += 1;
                report.details.push(MentionResolution::Ambiguous {
                    chunk_iri,
                    name: name.clone(),
                    candidates: matches.iter().map(|(_, iri)| iri.clone()).collect(),
                });
            }
        }
    }

    if !datums.is_empty() {
        store
            .transact(
                &datums,
                timestamp,
                Some("bobbin"),
                Some("mention-reconcile"),
            )
            .map_err(|e| anyhow::anyhow!("writing resolved mention edges failed: {e}"))?;
    }

    Ok(report)
}

/// Open the repo's quipu store (same path resolution as the chunk push)
/// and run the reconcile pass against it.
pub fn reconcile_mentions_at(repo_root: &Path) -> Result<MentionReport> {
    let quipu_config = quipu::QuipuConfig::load(repo_root);
    let db_path = if quipu_config.store_path.is_relative() {
        repo_root.join(&quipu_config.store_path)
    } else {
        quipu_config.store_path.clone()
    };
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create quipu store directory")?;
    }
    let mut store = quipu::Store::open(db_path.to_string_lossy().as_ref())
        .map_err(|e| anyhow::anyhow!("Failed to open quipu store: {e}"))?;
    reconcile_mentions(&mut store, &chrono::Utc::now().to_rfc3339())
}

#[cfg(test)]
#[path = "mentions_tests.rs"]
mod tests;
