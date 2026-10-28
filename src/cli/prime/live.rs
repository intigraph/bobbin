//! Bounded reads of an existing Lance table. Never use VectorStore::open or
//! get_stats here: those can migrate, create and compact the live index.
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use arrow::array::{Array, StringArray};
use futures::TryStreamExt;
use lancedb::query::{QueryRequest, Select};
use lancedb::table::{AnyQuery, BaseTable, NativeTable};
use serde_json::{json, Value};

pub(super) const DEADLINE: Duration = Duration::from_millis(550);
const ROW_CAP: usize = 2048;

pub(super) async fn summarize(root: &Path) -> Value {
    let path = crate::config::Config::lance_path(root).join("chunks.lance");
    bounded(&path, read(&path)).await
}

async fn bounded(path: &Path, future: impl std::future::Future<Output = Result<Value>>) -> Value {
    let started = Instant::now();
    let result = tokio::time::timeout(DEADLINE, future).await;
    let (status, summary) = match result {
        Ok(Ok(v)) => (
            if v["sample_truncated"] == true {
                "partial"
            } else {
                "ok"
            },
            v,
        ),
        Ok(Err(e)) => {
            let absent = e
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound);
            (
                if absent { "absent" } else { "unreadable" },
                json!({"error": e.to_string()}),
            )
        }
        Err(_) => ("partial", json!({"error": "summary deadline reached"})),
    };
    json!({"status": status, "elapsed_ms": started.elapsed().as_millis(), "summary": {
        "path": path, "index": summary,
        "fix": "Run `bobbin index` in the project, or pass the indexed project path.",
        "scope": "local index; freshness is indexed time, not a comparison with git HEAD"
    }})
}

async fn read(path: &Path) -> Result<Value> {
    // Resolving an existing filesystem path prevents a relative path from
    // being interpreted as a remote object-store URI. No directory creation.
    let path = tokio::fs::canonicalize(path).await?;
    let table = NativeTable::open(path.to_str().context("non-UTF8 index path")?).await?;
    let total = table.count_rows(None).await?;
    let request = AnyQuery::Query(QueryRequest {
        limit: Some(ROW_CAP),
        select: Select::Columns(vec![
            "repo".into(),
            "file_path".into(),
            "language".into(),
            "indexed_at".into(),
        ]),
        ..Default::default()
    });
    let mut stream = table.query(&request, Default::default()).await?;
    let mut files = HashSet::new();
    let mut languages = BTreeMap::<String, usize>::new();
    let mut last_indexed = None;
    let mut rows = 0;
    while let Some(batch) = stream.try_next().await? {
        let strings = |name| -> Result<&StringArray> {
            batch
                .column_by_name(name)
                .and_then(|a| a.as_any().downcast_ref())
                .context("index lacks expected string column")
        };
        let repos = strings("repo")?;
        let paths = strings("file_path")?;
        let langs = strings("language")?;
        let timestamps = strings("indexed_at")?;
        for i in 0..batch.num_rows().min(ROW_CAP - rows) {
            anyhow::ensure!(
                !repos.is_null(i)
                    && !paths.is_null(i)
                    && !langs.is_null(i)
                    && !timestamps.is_null(i),
                "null index field"
            );
            files.insert((repos.value(i).to_owned(), paths.value(i).to_owned()));
            *languages.entry(langs.value(i).to_owned()).or_default() += 1;
            let ts: i64 = timestamps
                .value(i)
                .parse()
                .context("invalid index timestamp")?;
            last_indexed = Some(last_indexed.map_or(ts, |old: i64| old.max(ts)));
        }
        rows += batch.num_rows().min(ROW_CAP - rows);
        if rows == ROW_CAP {
            break;
        }
    }
    let complete = rows == total;
    let age = last_indexed
        .and_then(|ts| chrono::Utc::now().timestamp().checked_sub(ts))
        .filter(|age| *age >= 0);
    Ok(json!({
        "total_chunks": total,
        "total_files": complete.then_some(files.len()),
        "last_indexed": if complete { last_indexed } else { None },
        "last_indexed_age_seconds": if complete { age } else { None },
        "sample_last_indexed": last_indexed,
        "sample_rows": rows, "sample_truncated": !complete,
        "sample_languages": languages.into_iter().take(20).collect::<BTreeMap<_, _>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{ArrayRef, RecordBatch, RecordBatchIterator};
    use arrow::datatypes::{DataType, Field, Schema};
    use std::sync::Arc;

    fn snapshot(root: &Path) -> Vec<(std::path::PathBuf, std::time::SystemTime, Vec<u8>)> {
        fn visit(
            root: &Path,
            p: &Path,
            out: &mut Vec<(std::path::PathBuf, std::time::SystemTime, Vec<u8>)>,
        ) {
            let meta = std::fs::symlink_metadata(p).unwrap();
            out.push((
                p.strip_prefix(root).unwrap().to_owned(),
                meta.modified().unwrap(),
                if meta.is_file() {
                    std::fs::read(p).unwrap()
                } else {
                    vec![]
                },
            ));
            if meta.is_dir() {
                for e in std::fs::read_dir(p).unwrap() {
                    visit(root, &e.unwrap().path(), out);
                }
            }
        }
        let mut out = vec![];
        visit(root, root, &mut out);
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    async fn fixture(root: &Path, rows: usize) {
        let path = crate::config::Config::lance_path(root);
        let conn = lancedb::connect(path.to_str().unwrap())
            .execute()
            .await
            .unwrap();
        let schema = Arc::new(Schema::new(
            ["repo", "file_path", "language", "indexed_at"]
                .map(|name| Field::new(name, DataType::Utf8, false))
                .to_vec(),
        ));
        let cols: Vec<ArrayRef> = ["fixture", "src/lib.rs", "rust", "100"]
            .map(|v| Arc::new(StringArray::from(vec![v; rows])) as ArrayRef)
            .into();
        let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();
        let reader = RecordBatchIterator::new(vec![Ok(batch)], schema);
        conn.create_table(
            "chunks",
            Box::new(reader) as Box<dyn arrow::array::RecordBatchReader + Send>,
        )
        .execute()
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn slow_read_is_partial_inside_one_second() {
        let started = Instant::now();
        let v = bounded(Path::new("fixture"), async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok(json!({}))
        })
        .await;
        assert_eq!(v["status"], "partial");
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(v["elapsed_ms"].as_u64().unwrap() < 1000);
    }

    #[tokio::test]
    async fn existing_table_counts_are_read_without_schema_migration() {
        let root = tempfile::tempdir().unwrap();
        fixture(root.path(), 2).await;
        let before = snapshot(root.path());
        // Deliberately lacks vectors and the production schema. The old open
        // would drop this table; the read-only reader projects only its needs.
        let v = read(&crate::config::Config::lance_path(root.path()).join("chunks.lance"))
            .await
            .unwrap();
        assert_eq!(v["total_chunks"], 2);
        assert_eq!(v["total_files"], 1);
        assert_eq!(v["last_indexed"], 100);
        assert_eq!(v["sample_truncated"], false);
        let again = read(&crate::config::Config::lance_path(root.path()).join("chunks.lance"))
            .await
            .unwrap();
        assert_eq!(again["total_chunks"], 2);
        assert_eq!(snapshot(root.path()), before);
    }

    #[tokio::test]
    async fn capped_scan_does_not_claim_global_freshness() {
        let root = tempfile::tempdir().unwrap();
        fixture(root.path(), ROW_CAP + 1).await;
        let v = read(&crate::config::Config::lance_path(root.path()).join("chunks.lance"))
            .await
            .unwrap();
        assert_eq!(v["total_chunks"], ROW_CAP + 1);
        assert_eq!(v["sample_rows"], ROW_CAP);
        assert_eq!(v["sample_truncated"], true);
        assert!(v["last_indexed"].is_null());
        assert!(v["total_files"].is_null());
    }
}
