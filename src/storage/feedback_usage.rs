//! Immutable citations and local observation coverage in the existing store.

use super::*;
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};

/// A locally observed entity whose last recorded use is at least 90 days old.
#[derive(Debug, Serialize)]
pub struct IdleMemoryEntity {
    pub iri: String,
    pub observed_since: String,
    pub last_used: Option<String>,
}

impl FeedbackStore {
    /// Observe a complete, fresh mirror without backdating local coverage.
    pub fn observe_memory_entities<'a>(
        &self,
        endpoint: &str,
        iris: impl IntoIterator<Item = &'a str>,
    ) -> Result<()> {
        anyhow::ensure!(!endpoint.is_empty(), "observation endpoint is required");
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let now = chrono::Utc::now().to_rfc3339();
        for iri in iris {
            anyhow::ensure!(!iri.is_empty(), "observed entity IRI is required");
            tx.execute(
                "INSERT OR IGNORE INTO memory_observations (iri, endpoint, observed_since) VALUES (?1, ?2, ?3)",
                params![iri, endpoint, now],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Bounded candidates from the authoritative local store, not top-20 stats.
    pub fn idle_memory_entities(
        &self,
        endpoint: &str,
        limit: usize,
    ) -> Result<Vec<IdleMemoryEntity>> {
        self.idle_memory_entities_at(endpoint, limit, chrono::Utc::now())
    }

    fn idle_memory_entities_at(
        &self,
        endpoint: &str,
        limit: usize,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<IdleMemoryEntity>> {
        let cutoff = (now - chrono::Duration::days(90)).to_rfc3339();
        let mut stmt = self.conn.prepare(
            "SELECT o.iri, o.observed_since, MAX(f.timestamp)
             FROM memory_observations o
             LEFT JOIN feedback f ON f.iri = o.iri AND f.rating = 'used'
             WHERE o.endpoint = ?3 AND o.endpoint != ''
             GROUP BY o.iri
             HAVING julianday(o.observed_since) <= julianday(?1)
               AND (COUNT(f.id) = 0 OR
                    (COUNT(julianday(f.timestamp)) = COUNT(f.id)
                     AND MAX(julianday(f.timestamp)) <= julianday(?1)))
             ORDER BY o.iri LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![cutoff, limit.min(100) as i64, endpoint], |row| {
            Ok(IdleMemoryEntity {
                iri: row.get(0)?,
                observed_since: row.get(1)?,
                last_used: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Per-entity usage of cited memory items, most injected first.
    pub fn entity_usage(&self, limit: usize) -> Result<Vec<EntityUsage>> {
        let mut stmt = self.conn.prepare(
            r#"WITH entities AS (
                   SELECT iri, MAX(kind) AS kind FROM (
                       SELECT iri, kind FROM injection_items
                       UNION ALL SELECT iri, '' FROM feedback WHERE iri IS NOT NULL AND iri != ''
                   ) GROUP BY iri
               ), injections AS (
                   SELECT iri, COUNT(*) AS injected FROM injection_items GROUP BY iri
               ), ratings AS (
                   SELECT f.*, (SELECT MAX(p.timestamp) FROM feedback p
                       WHERE p.injection_id = f.injection_id AND p.item = f.item
                         AND p.agent = f.agent AND p.rating = 'used' AND p.iri = f.iri
                   ) AS last_recorded_use
                   FROM feedback f WHERE f.iri IS NOT NULL AND
                     (f.rating != 'used' OR f.id = (
                         SELECT MIN(p.id) FROM feedback p
                         WHERE p.injection_id = f.injection_id AND p.item = f.item
                           AND p.agent = f.agent AND p.rating = 'used' AND p.iri = f.iri
                     ))
               )
               SELECT e.iri, COALESCE(e.kind, ''), COALESCE(i.injected, 0),
                      COUNT(CASE WHEN f.rating = 'used' THEN 1 END),
                      COUNT(CASE WHEN f.rating = 'useful' THEN 1 END),
                      COUNT(CASE WHEN f.rating = 'noise' THEN 1 END),
                      COUNT(CASE WHEN f.rating = 'harmful' THEN 1 END),
                      MAX(CASE WHEN f.rating = 'used' THEN f.last_recorded_use END)
               FROM entities e LEFT JOIN injections i ON i.iri = e.iri
               LEFT JOIN ratings f ON f.iri = e.iri
               GROUP BY e.iri ORDER BY 3 DESC, 4 DESC, e.iri LIMIT ?1"#,
        )?;
        let rows = stmt.query_map([limit.min(100) as i64], |row| {
            Ok(EntityUsage {
                iri: row.get(0)?,
                kind: row.get(1)?,
                injected: row.get::<_, i64>(2)? as u64,
                used: row.get::<_, i64>(3)? as u64,
                useful: row.get::<_, i64>(4)? as u64,
                noise: row.get::<_, i64>(5)? as u64,
                harmful: row.get::<_, i64>(6)? as u64,
                last_used: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub(super) fn init_memory_usage(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS memory_observations (
                iri TEXT NOT NULL,
                endpoint TEXT NOT NULL DEFAULT '',
                observed_since TEXT NOT NULL,
                PRIMARY KEY (iri, endpoint)
            );
            CREATE INDEX IF NOT EXISTS idx_feedback_use_receipt ON feedback(injection_id, item, agent, rating, iri, id);
            CREATE TABLE IF NOT EXISTS memory_usage_migrations (
                name TEXT PRIMARY KEY NOT NULL
            );",
        )?;
        // Migration starts coverage now, never at a historical injection date.
        // Run the history scan once, not on every prompt's store open.
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        if tx.execute(
            "INSERT OR IGNORE INTO memory_usage_migrations (name) VALUES ('observations-v1')",
            [],
        )? == 1
        {
            tx.execute(
                "INSERT OR IGNORE INTO memory_observations (iri, observed_since)
                 SELECT DISTINCT iri, ?1 FROM injection_items WHERE iri != ''",
                [chrono::Utc::now().to_rfc3339()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(super) fn store_citations(
        &self,
        injection_id: &str,
        items: &[crate::memory_recall::CitedItem],
    ) -> Result<()> {
        anyhow::ensure!(!injection_id.is_empty(), "injection_id is required");
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute(
            "INSERT OR IGNORE INTO injections (injection_id, timestamp) VALUES (?1, ?2)",
            params![injection_id, now],
        )?;
        for it in items {
            anyhow::ensure!(is_valid_item(&it.item), "invalid item id '{}'", it.item);
            anyhow::ensure!(!it.iri.is_empty(), "citation entity IRI is required");
            let existing: Option<(String, String, String, String)> = tx
                .query_row(
                    "SELECT iri, COALESCE(kind, ''), COALESCE(natural_key, ''), COALESCE(line, '')
                 FROM injection_items WHERE injection_id = ?1 AND item = ?2",
                    params![injection_id, it.item],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            if let Some(existing) = existing {
                anyhow::ensure!(
                    existing
                        == (
                            it.iri.clone(),
                            it.kind.clone(),
                            it.natural_key.clone(),
                            it.line.clone()
                        ),
                    "citation {injection_id}/{} is immutable",
                    it.item
                );
                continue;
            }
            tx.execute(
                "INSERT INTO injection_items (injection_id, item, iri, kind, natural_key, line)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    injection_id,
                    it.item,
                    it.iri,
                    it.kind,
                    it.natural_key,
                    it.line
                ],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO memory_observations (iri, observed_since) VALUES (?1, ?2)",
                params![it.iri, now],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "feedback_usage_tests.rs"]
mod tests;
