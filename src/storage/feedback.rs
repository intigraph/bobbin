//! Feedback storage using SQLite.
//!
//! Stores injection records (what bobbin injected) and agent feedback
//! (useful/noise/harmful ratings) for tuning search quality.

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Feedback store backed by SQLite.
pub struct FeedbackStore {
    conn: Connection,
}

/// Input for submitting feedback on an injection.
#[derive(Debug, Deserialize)]
pub struct FeedbackInput {
    pub injection_id: String,
    pub agent: String,
    #[serde(default)]
    pub rating: String,
    #[serde(default)]
    pub reason: String,
    /// Per-item citation inside the injection: `m1`..`m5` for the memory
    /// items bobbin tagged (aegis-gshu.2). Omit to rate the whole injection.
    #[serde(default)]
    pub item: Option<String>,
}

/// A stored feedback record.
#[derive(Debug, Serialize)]
pub struct FeedbackRecord {
    pub id: i64,
    pub injection_id: String,
    pub timestamp: String,
    pub agent: String,
    pub rating: String,
    pub reason: String,
    /// Cited item (`m1`..`m5`) when the rating is for one memory item.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// The entity IRI that item resolved to when the injection was logged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iri: Option<String>,
}

/// Usage of one cited memory entity across feedback (aegis-gshu.2).
#[derive(Debug, Serialize)]
pub struct EntityUsage {
    pub iri: String,
    pub kind: String,
    /// Times the entity was injected as a cited memory item.
    pub injected: u64,
    pub used: u64,
    pub useful: u64,
    pub noise: u64,
    pub harmful: u64,
    /// Last `used` rating, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used: Option<String>,
}

/// Query parameters for listing feedback.
#[derive(Debug, Default, Deserialize)]
pub struct FeedbackQuery {
    pub injection_id: Option<String>,
    pub rating: Option<String>,
    pub agent: Option<String>,
    pub limit: Option<usize>,
}

/// Aggregated feedback statistics.
#[derive(Debug, Serialize)]
pub struct FeedbackStats {
    pub total_injections: u64,
    pub total_feedback: u64,
    pub useful: u64,
    pub noise: u64,
    pub harmful: u64,
    /// Ratings that say a cited memory item was used (aegis-gshu.2).
    #[serde(default)]
    pub used: u64,
    /// Feedback records that name a cited item (`m1`..`m5`).
    #[serde(default)]
    pub item_feedback: u64,
    /// Most-injected cited memory entities with their ratings (top 20).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory_entities: Vec<EntityUsage>,
    /// Feedback records that have at least one lineage action.
    #[serde(default)]
    pub actioned: u64,
    /// Feedback records with no lineage action.
    #[serde(default)]
    pub unactioned: u64,
    /// Total lineage records.
    #[serde(default)]
    pub lineage_records: u64,
}

/// Feedback statistics for a single bundle or bead grouping.
#[derive(Debug, Serialize)]
pub struct GroupedStatsEntry {
    pub key: String,
    pub injections: u64,
    pub feedback: u64,
    pub useful: u64,
    pub noise: u64,
    pub harmful: u64,
}

/// Input for recording a lineage action that resolves feedback.
#[derive(Debug, Deserialize)]
pub struct LineageInput {
    /// Feedback record IDs this action resolves.
    pub feedback_ids: Vec<i64>,
    /// Type of action taken.
    pub action_type: String,
    /// Associated bead ID (e.g., "bo-a94q").
    #[serde(default)]
    pub bead: Option<String>,
    /// Git commit hash.
    #[serde(default)]
    pub commit_hash: Option<String>,
    /// Human-readable description of what was done.
    pub description: String,
    /// Agent that created this record.
    #[serde(default)]
    pub agent: Option<String>,
}

/// A stored lineage record.
#[derive(Debug, Serialize)]
pub struct LineageRecord {
    pub id: i64,
    pub timestamp: String,
    pub action_type: String,
    pub bead: Option<String>,
    pub commit_hash: Option<String>,
    pub description: String,
    pub agent: Option<String>,
    /// Feedback IDs linked to this action.
    pub feedback_ids: Vec<i64>,
}

/// Query parameters for listing lineage records.
#[derive(Debug, Default, Deserialize)]
pub struct LineageQuery {
    pub feedback_id: Option<i64>,
    pub bead: Option<String>,
    pub commit_hash: Option<String>,
    pub limit: Option<usize>,
}

/// Full injection detail with associated feedback.
#[derive(Debug, Serialize)]
pub struct InjectionDetail {
    pub injection_id: String,
    pub timestamp: String,
    pub session_id: Option<String>,
    pub agent: Option<String>,
    pub query: Option<String>,
    pub files: Vec<String>,
    pub total_chunks: i64,
    pub budget_lines: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub faction_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grounding_id: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub formatted_output: String,
    /// Cited memory items (`m1`..`m5`) and the entities they resolved to.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub memory_items: Vec<crate::memory_recall::CitedItem>,
    pub feedback: Vec<FeedbackRecord>,
}

/// `used` is per cited item: the agent acted on that memory item.
pub const VALID_RATINGS: &[&str] = &["useful", "noise", "harmful", "used"];

/// `m1`..`m99`: the per-injection item ids bobbin prints for memory items.
#[must_use]
pub fn is_valid_item(item: &str) -> bool {
    let Some(n) = item.strip_prefix('m') else {
        return false;
    };
    !n.is_empty() && n.len() <= 2 && !n.starts_with('0') && n.bytes().all(|b| b.is_ascii_digit())
}

impl FeedbackStore {
    /// Open or create a feedback store at the given path.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("Failed to open feedback database: {}", path.display()))?;

        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")
            .context("Failed to set pragmas")?;

        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS injections (
                injection_id TEXT PRIMARY KEY,
                timestamp TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                session_id TEXT,
                agent TEXT,
                query TEXT,
                files_json TEXT,
                chunks_json TEXT,
                total_chunks INTEGER,
                budget_lines INTEGER
            );

            CREATE TABLE IF NOT EXISTS feedback (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                injection_id TEXT NOT NULL,
                timestamp TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                agent TEXT NOT NULL,
                rating TEXT NOT NULL,
                reason TEXT DEFAULT ''
            );

            CREATE INDEX IF NOT EXISTS idx_feedback_injection ON feedback(injection_id);
            CREATE INDEX IF NOT EXISTS idx_feedback_rating ON feedback(rating);
            CREATE INDEX IF NOT EXISTS idx_feedback_agent ON feedback(agent);
        "#,
        )?;

        // Lineage tables for feedback-to-fix traceability
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS lineage (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                action_type TEXT NOT NULL,
                bead TEXT,
                commit_hash TEXT,
                description TEXT NOT NULL,
                agent TEXT
            );

            CREATE TABLE IF NOT EXISTS lineage_feedback (
                lineage_id INTEGER NOT NULL REFERENCES lineage(id),
                feedback_id INTEGER NOT NULL REFERENCES feedback(id),
                PRIMARY KEY (lineage_id, feedback_id)
            );

            CREATE INDEX IF NOT EXISTS idx_lineage_bead ON lineage(bead);
            CREATE INDEX IF NOT EXISTS idx_lineage_commit ON lineage(commit_hash);
        "#,
        )?;

        // Schema migrations: add columns if missing
        let migrations: &[(&str, &str)] = &[
            (
                "formatted_output",
                "ALTER TABLE injections ADD COLUMN formatted_output TEXT DEFAULT '';",
            ),
            (
                "bundle_name",
                "ALTER TABLE injections ADD COLUMN bundle_name TEXT;",
            ),
            ("bead_id", "ALTER TABLE injections ADD COLUMN bead_id TEXT;"),
            ("scope", "ALTER TABLE injections ADD COLUMN scope TEXT;"),
            (
                "faction_id",
                "ALTER TABLE injections ADD COLUMN faction_id TEXT;",
            ),
            (
                "grounding_id",
                "ALTER TABLE injections ADD COLUMN grounding_id TEXT;",
            ),
        ];
        for (col, ddl) in migrations {
            let has_col: bool = self.conn.query_row(
                &format!("SELECT COUNT(*) > 0 FROM pragma_table_info('injections') WHERE name = '{}'", col),
                [],
                |row| row.get(0),
            ).unwrap_or(false);
            if !has_col {
                self.conn.execute_batch(ddl)?;
            }
        }

        // Per-item citations (aegis-gshu.2): what each `m<n>` in an injection
        // resolved to, so a rating on (injection_id, item) lands on an entity.
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS injection_items (
                injection_id TEXT NOT NULL,
                item TEXT NOT NULL,
                iri TEXT NOT NULL,
                kind TEXT,
                natural_key TEXT,
                line TEXT,
                PRIMARY KEY (injection_id, item)
            );
            CREATE INDEX IF NOT EXISTS idx_injection_items_iri ON injection_items(iri);
        "#,
        )?;
        for (col, ddl) in [
            ("item", "ALTER TABLE feedback ADD COLUMN item TEXT;"),
            ("iri", "ALTER TABLE feedback ADD COLUMN iri TEXT;"),
        ] {
            let has_col: bool = self
                .conn
                .query_row(
                    "SELECT COUNT(*) > 0 FROM pragma_table_info('feedback') WHERE name = ?1",
                    [col],
                    |row| row.get(0),
                )
                .unwrap_or(false);
            if !has_col {
                self.conn.execute_batch(ddl)?;
            }
        }
        self.conn
            .execute_batch("CREATE INDEX IF NOT EXISTS idx_feedback_iri ON feedback(iri);")?;

        // Index for bundle/bead grouping queries
        self.conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_injections_bundle ON injections(bundle_name);
             CREATE INDEX IF NOT EXISTS idx_injections_bead ON injections(bead_id);",
        )?;

        Ok(())
    }

    /// Store an injection record (what bobbin injected, for later feedback reference).
    pub fn store_injection(
        &self,
        injection_id: &str,
        session_id: Option<&str>,
        agent: Option<&str>,
        query: &str,
        files: &[String],
        total_chunks: usize,
        budget_lines: usize,
    ) -> Result<()> {
        self.store_injection_with_output(
            injection_id,
            session_id,
            agent,
            query,
            files,
            total_chunks,
            budget_lines,
            None,
        )
    }

    /// Store an injection record with the formatted output text agents see.
    pub fn store_injection_with_output(
        &self,
        injection_id: &str,
        session_id: Option<&str>,
        agent: Option<&str>,
        query: &str,
        files: &[String],
        total_chunks: usize,
        budget_lines: usize,
        formatted_output: Option<&str>,
    ) -> Result<()> {
        self.store_scoped_injection_with_output(
            injection_id,
            session_id,
            agent,
            query,
            files,
            total_chunks,
            budget_lines,
            formatted_output,
            None,
            None,
            None,
        )
    }

    /// Store an injection plus its verified knowledge scope.
    #[allow(clippy::too_many_arguments)]
    pub fn store_scoped_injection_with_output(
        &self,
        injection_id: &str,
        session_id: Option<&str>,
        agent: Option<&str>,
        query: &str,
        files: &[String],
        total_chunks: usize,
        budget_lines: usize,
        formatted_output: Option<&str>,
        scope: Option<&str>,
        faction_id: Option<&str>,
        grounding_id: Option<&str>,
    ) -> Result<()> {
        if scope == Some("na") && (faction_id.is_none() || grounding_id.is_none()) {
            anyhow::bail!("NA injections require faction_id and grounding_id");
        }
        if scope != Some("na") && (faction_id.is_some() || grounding_id.is_some()) {
            anyhow::bail!("faction_id and grounding_id require scope=na");
        }
        let files_json = serde_json::to_string(files).unwrap_or_default();
        let now = chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.fZ")
            .to_string();
        self.conn.execute(
            "INSERT OR REPLACE INTO injections (injection_id, timestamp, session_id, agent, query, files_json, total_chunks, budget_lines, formatted_output, scope, faction_id, grounding_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![injection_id, now, session_id, agent, query, files_json, total_chunks as i64, budget_lines as i64, formatted_output.unwrap_or(""), scope, faction_id, grounding_id],
        )?;
        Ok(())
    }

    /// Store an injection record with full metadata including bundle/bead context.
    pub fn store_injection_full(
        &self,
        injection_id: &str,
        session_id: Option<&str>,
        agent: Option<&str>,
        query: &str,
        files: &[String],
        total_chunks: usize,
        budget_lines: usize,
        formatted_output: Option<&str>,
        bundle_name: Option<&str>,
        bead_id: Option<&str>,
    ) -> Result<()> {
        let files_json = serde_json::to_string(files).unwrap_or_default();
        let now = chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.fZ")
            .to_string();
        self.conn.execute(
            "INSERT OR REPLACE INTO injections (injection_id, timestamp, session_id, agent, query, files_json, total_chunks, budget_lines, formatted_output, bundle_name, bead_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![injection_id, now, session_id, agent, query, files_json, total_chunks as i64, budget_lines as i64, formatted_output.unwrap_or(""), bundle_name, bead_id],
        )?;
        Ok(())
    }

    /// Log the cited memory items of an injection (`m1`..`m5` -> entity IRI).
    pub fn store_injection_items(
        &self,
        injection_id: &str,
        items: &[crate::memory_recall::CitedItem],
    ) -> Result<()> {
        if injection_id.is_empty() {
            anyhow::bail!("injection_id is required");
        }
        let now = chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.fZ")
            .to_string();
        self.conn.execute(
            "INSERT OR IGNORE INTO injections (injection_id, timestamp) VALUES (?1, ?2)",
            rusqlite::params![injection_id, now],
        )?;
        for it in items {
            if !is_valid_item(&it.item) {
                anyhow::bail!("invalid item id '{}' (expected m1..m5)", it.item);
            }
            self.conn.execute(
                "INSERT OR REPLACE INTO injection_items (injection_id, item, iri, kind, natural_key, line) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![injection_id, it.item, it.iri, it.kind, it.natural_key, it.line],
            )?;
        }
        Ok(())
    }

    /// Cited items logged for an injection, in item order.
    pub fn injection_items(
        &self,
        injection_id: &str,
    ) -> Result<Vec<crate::memory_recall::CitedItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT item, iri, COALESCE(kind, ''), COALESCE(natural_key, ''), COALESCE(line, '') FROM injection_items WHERE injection_id = ?1 ORDER BY CAST(SUBSTR(item, 2) AS INTEGER)",
        )?;
        let rows = stmt.query_map([injection_id], |row| {
            Ok(crate::memory_recall::CitedItem {
                item: row.get(0)?,
                iri: row.get(1)?,
                kind: row.get(2)?,
                natural_key: row.get(3)?,
                line: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Store feedback for an injection.
    pub fn store_feedback(&self, input: &FeedbackInput) -> Result<i64> {
        if !VALID_RATINGS.contains(&input.rating.as_str()) {
            anyhow::bail!(
                "Invalid rating '{}'. Must be one of: {}",
                input.rating,
                VALID_RATINGS.join(", ")
            );
        }
        if input.injection_id.is_empty() {
            anyhow::bail!("injection_id is required");
        }
        let item = input
            .item
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let Some(item) = item {
            if !is_valid_item(item) {
                anyhow::bail!("Invalid item '{item}': expected a cited item id like m1..m5");
            }
        }
        if input.rating == "used" && item.is_none() {
            anyhow::bail!("Invalid rating 'used' without item: 'used' rates one cited memory item (item = m1..m5)");
        }

        // Explicitly set timestamp — older DB schemas may lack DEFAULT clauses.
        let now = chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.fZ")
            .to_string();

        // Ensure the injection_id exists in the injections table (auto-create if needed).
        // Include timestamp explicitly for older schemas where it's NOT NULL without DEFAULT.
        self.conn.execute(
            "INSERT OR IGNORE INTO injections (injection_id, timestamp) VALUES (?1, ?2)",
            rusqlite::params![input.injection_id, now],
        )?;

        // Resolve (injection_id, item) to the entity it cited. An item the
        // injection never logged is kept (the rating still counts) with no IRI.
        let iri: Option<String> = match item {
            Some(item) => self
                .conn
                .query_row(
                    "SELECT iri FROM injection_items WHERE injection_id = ?1 AND item = ?2",
                    rusqlite::params![input.injection_id, item],
                    |row| row.get(0),
                )
                .ok(),
            None => None,
        };

        self.conn.execute(
            "INSERT INTO feedback (injection_id, timestamp, agent, rating, reason, item, iri) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![input.injection_id, now, input.agent, input.rating, input.reason, item, iri],
        )?;

        Ok(self.conn.last_insert_rowid())
    }

    /// List feedback records with optional filters.
    pub fn list_feedback(&self, query: &FeedbackQuery) -> Result<Vec<FeedbackRecord>> {
        let limit = query.limit.unwrap_or(20).min(50);
        let mut sql = String::from(
            "SELECT id, injection_id, timestamp, agent, rating, reason, item, iri FROM feedback WHERE 1=1",
        );
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(ref inj) = query.injection_id {
            sql.push_str(&format!(" AND injection_id = ?{}", params.len() + 1));
            params.push(Box::new(inj.clone()));
        }
        if let Some(ref rating) = query.rating {
            sql.push_str(&format!(" AND rating = ?{}", params.len() + 1));
            params.push(Box::new(rating.clone()));
        }
        if let Some(ref agent) = query.agent {
            sql.push_str(&format!(" AND agent = ?{}", params.len() + 1));
            params.push(Box::new(agent.clone()));
        }

        sql.push_str(&format!(
            " ORDER BY timestamp DESC LIMIT ?{}",
            params.len() + 1
        ));
        params.push(Box::new(limit as i64));

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            Ok(FeedbackRecord {
                id: row.get(0)?,
                injection_id: row.get(1)?,
                timestamp: row.get(2)?,
                agent: row.get(3)?,
                rating: row.get(4)?,
                reason: row.get(5)?,
                item: row.get(6)?,
                iri: row.get(7)?,
            })
        })?;

        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    /// Get aggregated feedback statistics.
    pub fn stats(&self) -> Result<FeedbackStats> {
        // rusqlite 0.40 dropped FromSql for u64 (SQLite integers are i64);
        // COUNT(*) is non-negative, so the casts are lossless.
        let total_injections: u64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM injections", [], |row| {
                    row.get::<_, i64>(0)
                })? as u64;

        let total_feedback: u64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM feedback", [], |row| {
                    row.get::<_, i64>(0)
                })? as u64;

        let useful: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM feedback WHERE rating = 'useful'",
            [],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let noise: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM feedback WHERE rating = 'noise'",
            [],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let harmful: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM feedback WHERE rating = 'harmful'",
            [],
            |row| row.get::<_, i64>(0),
        )? as u64;

        // Lineage stats
        let lineage_records: u64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM lineage", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap_or(0) as u64;
        let actioned: u64 = self
            .conn
            .query_row(
                "SELECT COUNT(DISTINCT feedback_id) FROM lineage_feedback",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0) as u64;
        let unactioned = total_feedback.saturating_sub(actioned);

        let used: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM feedback WHERE rating = 'used'",
            [],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let item_feedback: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM feedback WHERE item IS NOT NULL",
            [],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let memory_entities = self.entity_usage(20)?;

        Ok(FeedbackStats {
            total_injections,
            total_feedback,
            useful,
            noise,
            harmful,
            used,
            item_feedback,
            memory_entities,
            actioned,
            unactioned,
            lineage_records,
        })
    }

    /// Per-entity usage of cited memory items, most injected first.
    pub fn entity_usage(&self, limit: usize) -> Result<Vec<EntityUsage>> {
        let mut stmt = self.conn.prepare(
            r#"SELECT ii.iri,
                      COALESCE(MAX(ii.kind), ''),
                      COUNT(DISTINCT ii.injection_id || '/' || ii.item),
                      COUNT(DISTINCT CASE WHEN f.rating = 'used' THEN f.id END),
                      COUNT(DISTINCT CASE WHEN f.rating = 'useful' THEN f.id END),
                      COUNT(DISTINCT CASE WHEN f.rating = 'noise' THEN f.id END),
                      COUNT(DISTINCT CASE WHEN f.rating = 'harmful' THEN f.id END),
                      MAX(CASE WHEN f.rating = 'used' THEN f.timestamp END)
               FROM injection_items ii
               LEFT JOIN feedback f
                 ON f.injection_id = ii.injection_id AND f.item = ii.item
               GROUP BY ii.iri
               ORDER BY 3 DESC, 4 DESC, ii.iri
               LIMIT ?1"#,
        )?;
        let rows = stmt.query_map([limit as i64], |row| {
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

    /// Get feedback statistics grouped by bundle name.
    pub fn stats_by_bundle(&self) -> Result<Vec<GroupedStatsEntry>> {
        self.stats_grouped("bundle_name")
    }

    /// Get feedback statistics grouped by bead ID.
    pub fn stats_by_bead(&self) -> Result<Vec<GroupedStatsEntry>> {
        self.stats_grouped("bead_id")
    }

    fn stats_grouped(&self, group_col: &str) -> Result<Vec<GroupedStatsEntry>> {
        let sql = format!(
            r#"SELECT
                COALESCE(i.{col}, '(none)') as group_key,
                COUNT(DISTINCT i.injection_id) as injections,
                COUNT(DISTINCT f.id) as feedback,
                COUNT(DISTINCT CASE WHEN f.rating = 'useful' THEN f.id END) as useful,
                COUNT(DISTINCT CASE WHEN f.rating = 'noise' THEN f.id END) as noise,
                COUNT(DISTINCT CASE WHEN f.rating = 'harmful' THEN f.id END) as harmful
            FROM injections i
            LEFT JOIN feedback f ON i.injection_id = f.injection_id
            GROUP BY group_key
            ORDER BY injections DESC"#,
            col = group_col,
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            // rusqlite 0.40 dropped FromSql for u64; the counts are
            // non-negative, so i64 -> u64 is lossless.
            Ok(GroupedStatsEntry {
                key: row.get(0)?,
                injections: row.get::<_, i64>(1)? as u64,
                feedback: row.get::<_, i64>(2)? as u64,
                useful: row.get::<_, i64>(3)? as u64,
                noise: row.get::<_, i64>(4)? as u64,
                harmful: row.get::<_, i64>(5)? as u64,
            })
        })?;
        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?);
        }
        Ok(entries)
    }

    /// Record a lineage action that resolves one or more feedback records.
    pub fn store_lineage(&self, input: &LineageInput) -> Result<i64> {
        const VALID_ACTIONS: &[&str] = &[
            "access_rule",
            "tag_effect",
            "config_change",
            "code_fix",
            "exclusion_rule",
        ];
        if !VALID_ACTIONS.contains(&input.action_type.as_str()) {
            anyhow::bail!(
                "Invalid action_type '{}'. Must be one of: {}",
                input.action_type,
                VALID_ACTIONS.join(", ")
            );
        }
        if input.feedback_ids.is_empty() {
            anyhow::bail!("feedback_ids must not be empty");
        }

        // Validate that all feedback IDs exist before creating the lineage record.
        // Without this, INSERT OR IGNORE silently drops rows that violate the FK
        // constraint, leading to lineage records with no linked feedback.
        let mut missing = Vec::new();
        for &fid in &input.feedback_ids {
            let exists: bool = self
                .conn
                .query_row(
                    "SELECT COUNT(*) > 0 FROM feedback WHERE id = ?1",
                    rusqlite::params![fid],
                    |row| row.get(0),
                )
                .unwrap_or(false);
            if !exists {
                missing.push(fid);
            }
        }
        if !missing.is_empty() {
            anyhow::bail!(
                "Feedback IDs not found: {:?}. Use bobbin_feedback_list to find valid IDs.",
                missing
            );
        }

        let now = chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.fZ")
            .to_string();

        self.conn.execute(
            "INSERT INTO lineage (timestamp, action_type, bead, commit_hash, description, agent) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![now, input.action_type, input.bead, input.commit_hash, input.description, input.agent],
        )?;
        let lineage_id = self.conn.last_insert_rowid();

        for &fid in &input.feedback_ids {
            self.conn.execute(
                "INSERT INTO lineage_feedback (lineage_id, feedback_id) VALUES (?1, ?2)",
                rusqlite::params![lineage_id, fid],
            )?;
        }

        Ok(lineage_id)
    }

    /// List lineage records with optional filters.
    pub fn list_lineage(&self, query: &LineageQuery) -> Result<Vec<LineageRecord>> {
        let limit = query.limit.unwrap_or(20).min(50);
        let mut sql = String::from("SELECT l.id, l.timestamp, l.action_type, l.bead, l.commit_hash, l.description, l.agent FROM lineage l");
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let mut where_added = false;

        if let Some(fid) = query.feedback_id {
            sql.push_str(
                " JOIN lineage_feedback lf ON l.id = lf.lineage_id WHERE lf.feedback_id = ?1",
            );
            params.push(Box::new(fid));
            where_added = true;
        }

        if let Some(ref bead) = query.bead {
            if where_added {
                sql.push_str(&format!(" AND l.bead = ?{}", params.len() + 1));
            } else {
                sql.push_str(&format!(" WHERE l.bead = ?{}", params.len() + 1));
                where_added = true;
            }
            params.push(Box::new(bead.clone()));
        }

        if let Some(ref commit) = query.commit_hash {
            if where_added {
                sql.push_str(&format!(" AND l.commit_hash = ?{}", params.len() + 1));
            } else {
                sql.push_str(&format!(" WHERE l.commit_hash = ?{}", params.len() + 1));
            }
            params.push(Box::new(commit.clone()));
        }

        sql.push_str(&format!(
            " ORDER BY l.timestamp DESC LIMIT ?{}",
            params.len() + 1
        ));
        params.push(Box::new(limit as i64));

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(param_refs.as_slice(), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?;

        let mut records = Vec::new();
        for row in rows {
            let (id, timestamp, action_type, bead, commit_hash, description, agent) = row?;
            // Fetch linked feedback IDs
            let feedback_ids = self.lineage_feedback_ids(id)?;
            records.push(LineageRecord {
                id,
                timestamp,
                action_type,
                bead,
                commit_hash,
                description,
                agent,
                feedback_ids,
            });
        }
        Ok(records)
    }

    /// Get feedback IDs linked to a lineage record.
    fn lineage_feedback_ids(&self, lineage_id: i64) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT feedback_id FROM lineage_feedback WHERE lineage_id = ?1 ORDER BY feedback_id",
        )?;
        let rows = stmt.query_map(rusqlite::params![lineage_id], |row| row.get(0))?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row?);
        }
        Ok(ids)
    }

    /// Get full injection detail by ID, including associated feedback.
    pub fn get_injection(&self, injection_id: &str) -> Result<Option<InjectionDetail>> {
        let mut stmt = self.conn.prepare(
            "SELECT injection_id, timestamp, session_id, agent, query, files_json, total_chunks, budget_lines, COALESCE(formatted_output, ''), scope, faction_id, grounding_id FROM injections WHERE injection_id = ?1",
        )?;

        let mut rows = stmt.query(rusqlite::params![injection_id])?;
        let row = match rows.next()? {
            Some(r) => r,
            None => return Ok(None),
        };

        let files_json: Option<String> = row.get(5)?;
        let files: Vec<String> = files_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default();
        let formatted_output: String = row.get(8)?;

        let feedback = self.list_feedback(&FeedbackQuery {
            injection_id: Some(injection_id.to_string()),
            limit: Some(50),
            ..Default::default()
        })?;

        Ok(Some(InjectionDetail {
            injection_id: row.get(0)?,
            timestamp: row.get(1)?,
            session_id: row.get(2)?,
            agent: row.get(3)?,
            query: row.get(4)?,
            files,
            total_chunks: row.get::<_, Option<i64>>(6)?.unwrap_or(0),
            budget_lines: row.get::<_, Option<i64>>(7)?.unwrap_or(0),
            formatted_output,
            scope: row.get(9)?,
            faction_id: row.get(10)?,
            grounding_id: row.get(11)?,
            memory_items: self.injection_items(injection_id).unwrap_or_default(),
            feedback,
        }))
    }
    /// Get injection IDs from a session that have no feedback yet.
    pub fn unrated_injections_for_session(&self, session_id: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT i.injection_id FROM injections i
             LEFT JOIN feedback f ON i.injection_id = f.injection_id
             WHERE i.session_id = ?1 AND f.id IS NULL
             ORDER BY i.timestamp DESC
             LIMIT 10",
        )?;
        let rows = stmt.query_map(rusqlite::params![session_id], |row| row.get::<_, String>(0))?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row?);
        }
        Ok(ids)
    }

    /// Compute per-file feedback scores based on cross-agent ratings.
    ///
    /// For each file that appeared in a rated injection, accumulates a score:
    /// - "useful" ratings add +1.0 (scaled by query keyword overlap)
    /// - "noise" ratings add -0.3
    /// - "harmful" ratings add -1.0
    ///
    /// Query overlap is measured by splitting both the current query and the
    /// injection query into lowercase keywords and computing Jaccard similarity.
    /// This ensures files rated useful for similar topics get boosted more than
    /// files rated useful for unrelated queries.
    ///
    /// Returns a map of file_path → aggregate score. Only files with non-zero
    /// scores are included. Scores are NOT normalized — callers should apply
    /// their own clamping/scaling.
    pub fn file_feedback_scores(
        &self,
        query: &str,
        min_overlap: f32,
    ) -> Result<std::collections::HashMap<String, f32>> {
        use std::collections::{HashMap, HashSet};

        let query_words: HashSet<String> = query
            .to_lowercase()
            .split_whitespace()
            .filter(|w| w.len() >= 3)
            .map(|w| w.to_string())
            .collect();

        if query_words.is_empty() {
            return Ok(HashMap::new());
        }

        // Join injections + feedback, get the query + files + rating
        let mut stmt = self.conn.prepare(
            "SELECT i.query, i.files_json, f.rating
             FROM feedback f
             JOIN injections i ON f.injection_id = i.injection_id
             WHERE i.files_json IS NOT NULL AND i.query IS NOT NULL
             ORDER BY f.timestamp DESC
             LIMIT 500",
        )?;

        let mut scores: HashMap<String, f32> = HashMap::new();

        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;

        for row in rows {
            let (inj_query, files_json, rating) = row?;

            // Compute keyword overlap (Jaccard similarity)
            let inj_words: HashSet<String> = inj_query
                .to_lowercase()
                .split_whitespace()
                .filter(|w| w.len() >= 3)
                .map(|w| w.to_string())
                .collect();
            if inj_words.is_empty() {
                continue;
            }
            let intersection = query_words.intersection(&inj_words).count() as f32;
            let union = query_words.union(&inj_words).count() as f32;
            let overlap = intersection / union;

            if overlap < min_overlap {
                continue;
            }

            let weight = match rating.as_str() {
                "useful" => 1.0 * overlap,
                "noise" => -0.3 * overlap,
                "harmful" => -1.0 * overlap,
                _ => continue,
            };

            // Parse files_json
            if let Ok(files) = serde_json::from_str::<Vec<String>>(&files_json) {
                for file in files {
                    *scores.entry(file).or_insert(0.0) += weight;
                }
            }
        }

        // Remove zero-score entries
        scores.retain(|_, v| *v != 0.0);
        Ok(scores)
    }

    /// Get feedback auto-tags (feedback:hot, feedback:cold) keyed by file path.
    /// Returns empty map if the feedback_tags table doesn't exist yet.
    pub fn get_feedback_tags(&self) -> Result<std::collections::HashMap<String, Vec<String>>> {
        let mut map = std::collections::HashMap::new();
        let has_table: bool = self.conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='feedback_tags'",
            [],
            |row| row.get(0),
        )?;
        if !has_table {
            return Ok(map);
        }
        let mut stmt = self
            .conn
            .prepare("SELECT file_path, tag FROM feedback_tags")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (path, tag) = row?;
            map.entry(path).or_insert_with(Vec::new).push(tag);
        }
        Ok(map)
    }
}

#[cfg(test)]
#[path = "feedback_tests.rs"]
mod tests;
